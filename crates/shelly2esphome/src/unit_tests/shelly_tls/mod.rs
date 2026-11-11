//! Unit tests of `crate::shelly_tls` (not counted as production code for coverage).

use std::{error::Error as _, net::SocketAddr, time::Duration};

use ::ring::{
    rand::SystemRandom,
    signature::{ECDSA_P384_SHA384_ASN1_SIGNING, EcdsaKeyPair, KeyPair},
};
use rustls::{
    ServerConfig,
    server::{ClientHello, ResolvesServerCert},
    sign::{CertifiedKey, SigningKey},
    version::{TLS12, TLS13},
};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::*;
use crate::shelly_tls::synthetic::{
    CertSpec, TestCa, certificate, private_key, public_key_info, tbs_fields, tlv,
};

const LEAF: &[u8] = include_bytes!("../fixtures/updates.shelly.cloud.der");
const HOST: &str = "updates.shelly.cloud";
/// 2026-11-11, when the fixture was captured.
const NOW: u64 = 1_794_397_570;

#[test]
fn pins_shellys_root() {
    assert_eq!(hex::encode(Sha256::digest(ROOT)), ROOT_SHA256);
    let root = cert::Certificate::parse(ROOT).expect("root");
    assert_eq!(cert::Certificate::parse(ROOT).map(|cert| cert.issuer), Some(root.subject));
    assert!(String::from_utf8_lossy(root.subject).contains("Allterco"));
    let validity = (Timestamp(root.not_before).to_string(), Timestamp(root.not_after).to_string());
    assert_eq!(validity, ("2020-08-04".to_owned(), "2030-08-02".to_owned()));
}

#[test]
fn trusts_shellys_live_certificate_only_for_its_host_and_time() {
    let verifier = Verifier::new(ROOT).expect("root");
    assert!(verifier.check(LEAF, HOST, NOW).is_ok());
    assert!(verifier.check(LEAF, "UPDATES.shelly.cloud", NOW).is_ok());
    assert!(matches!(
        verifier.check(LEAF, "fwcdn.shelly.cloud", NOW),
        Err(TlsError::WrongHost { host, names }) if host == "fwcdn.shelly.cloud" && names == [HOST]
    ));
    // After the root expired, before the server certificate started.
    let expired = verifier.check(LEAF, HOST, 1_911_902_622).expect_err("root expired");
    assert_eq!(
        expired.to_string(),
        "the Shelly root certificate in this program is valid from 2020-08-04 to 2030-08-02, not \
         now; check the system clock or update shelly2esphome"
    );
    let early = verifier.check(LEAF, HOST, 1_609_459_200).expect_err("not yet");
    assert_eq!(
        early.to_string(),
        "the server certificate is valid from 2021-06-22 to 2031-06-20, not now; check the \
         system clock"
    );
    // One flipped bit in the signed part.
    let mut forged = LEAF.to_vec();
    forged[200] ^= 0x01;
    assert!(matches!(verifier.check(&forged, HOST, NOW), Err(TlsError::BadSignature)));
}

#[test]
fn refuses_other_issuers_keys_and_algorithms() {
    let ca = TestCa::new(1, "Test CA");
    let verifier = Verifier::new(&ca.root()).expect("root");
    let leaf = ca.issue(&CertSpec::server(HOST));
    assert!(verifier.check(&leaf, HOST, NOW).is_ok());
    assert!(matches!(verifier.check(b"junk", HOST, NOW), Err(TlsError::Malformed)));
    // Version 3 with an extensions field, signed by the root.
    let v3 = ca.issue(&CertSpec { v3: true, ..CertSpec::server(HOST) });
    assert!(matches!(verifier.check(&v3, HOST, NOW), Err(TlsError::Extensions)));
    let stranger = TestCa::new(1, "Other CA").issue(&CertSpec::server(HOST));
    assert!(matches!(verifier.check(&stranger, HOST, NOW), Err(TlsError::UnknownIssuer)));
    // Same name, other key.
    let impostor = TestCa::new(2, "Test CA").issue(&CertSpec::server(HOST));
    assert!(matches!(verifier.check(&impostor, HOST, NOW), Err(TlsError::BadSignature)));
    // A root whose key algorithm no provider knows.
    let mut fields =
        tbs_fields(&ca.name, &CertSpec { subject: ca.name.clone(), ..CertSpec::server("") });
    fields[5] = tlv(0x30, &[tlv(0x30, &[0x06, 0x02, 0x2A, 0x03]), tlv(0x03, &[0, 1])].concat());
    let odd = Verifier::new(&certificate(&fields, 1)).expect("odd root");
    assert!(matches!(odd.check(&leaf, HOST, NOW), Err(TlsError::UnsupportedAlgorithm)));
    assert!(matches!(Verifier::new(b"junk"), Err(TlsError::BadRoot)));
}

#[test]
fn reports_refusals_to_rustls() {
    let verifier = Verifier::new(ROOT).expect("root");
    let name = ServerName::try_from("fwcdn.shelly.cloud").expect("name");
    let now = UnixTime::since_unix_epoch(Duration::from_secs(NOW));
    let error = verifier
        .verify_server_cert(&CertificateDer::from(LEAF), &[], &name, &[], now)
        .expect_err("wrong host");
    assert_eq!(
        error.to_string(),
        "other error: the server certificate is for updates.shelly.cloud, not for \
         fwcdn.shelly.cloud"
    );
    let host = ServerName::try_from(HOST).expect("name");
    assert!(verifier.verify_server_cert(&CertificateDer::from(LEAF), &[], &host, &[], now).is_ok());
    let schemes = verifier.supported_verify_schemes();
    assert!(schemes.contains(&SignatureScheme::ED25519));
    assert!(schemes.contains(&SignatureScheme::RSA_PKCS1_SHA256));
}

#[test]
fn names_every_refusal() {
    let messages = [
        TlsError::BadRoot.to_string(),
        TlsError::Malformed.to_string(),
        TlsError::UnknownIssuer.to_string(),
        TlsError::UnsupportedAlgorithm.to_string(),
        TlsError::BadSignature.to_string(),
        TlsError::Extensions.to_string(),
        TlsError::Client("no TLS".into()).to_string(),
    ];
    assert_eq!(
        messages,
        [
            "the root certificate is malformed",
            "the server certificate is malformed",
            "the server certificate is not issued by Shelly's root certificate",
            "the signature algorithm of the server is not supported",
            "a signature of the server does not match (forged or damaged certificate)",
            "the server certificate has extensions, which Shelly's certificates do not have",
            "the HTTPS client cannot be built: no TLS",
        ]
    );
    assert_eq!(Timestamp(0).to_string(), "1970-01-01");
    assert_eq!(Timestamp(951_782_400).to_string(), "2000-02-29");
}

/// Always presents the same certificate and key.
#[derive(Debug)]
struct Fixed(Arc<CertifiedKey>);

impl ResolvesServerCert for Fixed {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.clone())
    }
}

/// A TLS server on localhost that presents `cert`, signs with the key of
/// seed byte `key` and answers every request with `response`.
async fn server(
    cert: Vec<u8>,
    key: u8,
    version: &'static rustls::SupportedProtocolVersion,
    response: &'static str,
) -> SocketAddr {
    let signing = ring::default_provider()
        .key_provider
        .load_private_key(rustls::pki_types::PrivateKeyDer::Pkcs8(private_key(key).into()))
        .expect("key");
    server_signing_with(cert, signing, version, response).await
}

/// Like [`server`], signing the handshake with `signing`.
async fn server_signing_with(
    cert: Vec<u8>,
    signing: Arc<dyn SigningKey>,
    version: &'static rustls::SupportedProtocolVersion,
    response: &'static str,
) -> SocketAddr {
    let provider = Arc::new(ring::default_provider());
    // CertifiedKey::new does not compare certificate and key, so a test can
    // present a certificate with a key it does not hold.
    let certified = Arc::new(CertifiedKey::new(vec![CertificateDer::from(cert)], signing));
    let config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[version])
        .expect("versions")
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(Fixed(certified)));
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else { return };
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match tls.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => request.extend_from_slice(&buffer[..read]),
                    }
                }
                let _ = tls.write_all(response.as_bytes()).await;
                let _ = tls.shutdown().await;
            });
        }
    });
    addr
}

const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

fn client_for(ca: &TestCa, addr: SocketAddr) -> reqwest::Client {
    // Without proxies: a proxy from the environment would bypass `resolve`.
    let builder = reqwest::Client::builder().no_proxy().resolve(HOST, addr);
    client_with_root(&ca.root(), builder).expect("client")
}

/// The whole error chain of a failed request, which names the TLS refusal.
fn chain(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text = format!("{text}: {cause}");
        source = cause.source();
    }
    text
}

#[tokio::test]
async fn connects_over_tls_1_2_and_1_3() {
    let ca = TestCa::new(1, "Test CA");
    for version in [&TLS12, &TLS13] {
        let addr = server(ca.issue(&CertSpec::server(HOST)), 0x51, version, OK).await;
        let url = format!("https://{HOST}:{}/update/S1G3", addr.port());
        let response = client_for(&ca, addr).get(&url).send().await.expect("get");
        assert_eq!(response.text().await.expect("body"), "ok");
    }
}

#[tokio::test]
async fn refuses_wrong_hosts_keys_and_plain_http() {
    let ca = TestCa::new(1, "Test CA");
    let get = |addr: SocketAddr| {
        let client = client_for(&ca, addr);
        async move { client.get(format!("https://{HOST}:{}/", addr.port())).send().await }
    };

    let other_host = server(ca.issue(&CertSpec::server("fwcdn.shelly.cloud")), 0x51, &TLS13, OK);
    let error = get(other_host.await).await.expect_err("wrong host");
    assert!(chain(&error).contains("is for fwcdn.shelly.cloud, not for updates.shelly.cloud"));

    // The certificate is genuine, but the server does not hold its key.
    for version in [&TLS12, &TLS13] {
        let addr = server(ca.issue(&CertSpec::server(HOST)), 0x52, version, OK).await;
        let error = get(addr).await.expect_err("foreign key");
        assert!(chain(&error).contains("a signature of the server does not match"), "{error:?}");
    }

    // A certificate whose key algorithm does not fit the handshake signature.
    let mut fields = tbs_fields(&ca.name, &CertSpec::server(HOST));
    // The Ed25519 key's BIT STRING follows its 2-byte header and 7-byte algorithm.
    let key = public_key_info(0x51)[9..].to_vec();
    fields[5] = tlv(0x30, &[tlv(0x30, &[0x06, 0x02, 0x2A, 0x03]), key].concat());
    let addr = server(certificate(&fields, 1), 0x51, &TLS13, OK).await;
    let error = get(addr).await.expect_err("odd key");
    assert!(
        chain(&error).contains("signature algorithm of the server is not supported"),
        "{error:?}"
    );

    let addr = server(ca.issue(&CertSpec::server(HOST)), 0x51, &TLS13, OK).await;
    let plain = client_for(&ca, addr).get(format!("http://{HOST}:{}/", addr.port())).send().await;
    assert!(plain.is_err());
}

#[tokio::test]
async fn does_not_follow_redirects() {
    let ca = TestCa::new(1, "Test CA");
    let moved = "HTTP/1.1 302 Found\r\nLocation: https://example.com/\r\nContent-Length: 0\r\n\
                 Connection: close\r\n\r\n";
    let addr = server(ca.issue(&CertSpec::server(HOST)), 0x51, &TLS13, moved).await;
    let response = client_for(&ca, addr)
        .get(format!("https://{HOST}:{}/", addr.port()))
        .send()
        .await
        .expect("get");
    assert_eq!(response.status(), 302);
}

#[test]
fn builds_the_client_with_the_built_in_root() {
    assert!(client(reqwest::Client::builder()).is_ok());
    assert!(matches!(
        client_with_root(b"junk", reqwest::Client::builder()),
        Err(TlsError::BadRoot)
    ));
}

#[test]
fn binds_tls_1_3_ecdsa_schemes_to_their_curve() {
    let random = SystemRandom::new();
    let pkcs8 =
        EcdsaKeyPair::generate_pkcs8(&ECDSA_P384_SHA384_ASN1_SIGNING, &random).expect("key");
    let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P384_SHA384_ASN1_SIGNING, pkcs8.as_ref(), &random)
        .expect("key");
    // id-ecPublicKey with secp384r1.
    let algorithm = tlv(
        0x30,
        &[
            &[0x06, 0x07, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01][..],
            &[0x06, 0x05, 0x2B, 0x81, 0x04, 0x00, 0x22],
        ]
        .concat(),
    );
    let key = tlv(0x03, &[&[0][..], pair.public_key().as_ref()].concat());
    let ca = TestCa::new(1, "Test CA");
    let mut fields = tbs_fields(&ca.name, &CertSpec::server(HOST));
    fields[5] = tlv(0x30, &[algorithm, key].concat());
    let cert = certificate(&fields, 1);
    let verifier = Verifier::new(&ca.root()).expect("root");
    let scheme = SignatureScheme::ECDSA_NISTP256_SHA256;
    // A P-384 key under the P-256 scheme: TLS 1.2 tries the cross-curve
    // algorithm (and rejects the junk signature), TLS 1.3 has none for it.
    let tls12 = verifier.handshake(b"hello", &cert, scheme, b"junk", false).expect_err("junk");
    assert!(tls12.to_string().contains("a signature of the server does not match"), "{tls12}");
    let tls13 = verifier.handshake(b"hello", &cert, scheme, b"junk", true).expect_err("curve");
    assert!(tls13.to_string().contains("algorithm of the server is not supported"), "{tls13}");
}
