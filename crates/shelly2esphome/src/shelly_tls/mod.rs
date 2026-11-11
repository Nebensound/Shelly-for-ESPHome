//! HTTPS to Shelly's update servers.
//!
//! `updates.shelly.cloud` and `fwcdn.shelly.cloud` present certificates of
//! Shelly's own certificate authority (`O=Allterco`), which no trust store
//! contains. The certificates are X.509 version 1 with the host only in the
//! common name, which general-purpose verifiers refuse even with the right
//! root. This client therefore trusts exactly one root, built into the
//! program, and checks:
//!
//! - the server certificate is signed directly by that root (certificates
//!   the server sends along are ignored) and has no extensions,
//! - the root and the server certificate are valid now,
//! - the server certificate's common name is the requested host,
//! - the handshake is signed with the server certificate's key.
//!
//! The root is the certificate that Shelly's official firmware packages carry
//! in their file system so that the devices can verify these servers
//! (`fs.img`; older packages as `shelly_cloud.pem`). Its SHA-256 is pinned in
//! [`ROOT_SHA256`] and checked by a test, and it verifies the live
//! certificates of both servers.
//!
//! The client refuses plain HTTP and redirects.

use std::{fmt, sync::Arc};

use rustls::{
    ClientConfig, DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{WebPkiSupportedAlgorithms, ring},
    pki_types::{CertificateDer, ServerName, SignatureVerificationAlgorithm, UnixTime},
};

use self::cert::Certificate;

mod cert;
#[doc(hidden)]
pub mod synthetic;

/// Shelly's root certificate (DER): `O=Allterco`, self-signed, RSA 2048,
/// valid from 2020-08-04 to 2030-08-02.
pub const ROOT: &[u8] = include_bytes!("shelly-root-ca.der");

/// SHA-256 of [`ROOT`], lowercase hex.
pub const ROOT_SHA256: &str = "5a7ee2faf82bd43beef6581f48bbd4a134456488a7904eec52a15c572f53093e";

/// The schemes TLS 1.3 allows for handshake signatures (RFC 8446 4.2.3);
/// RSA PKCS#1 and SHA-1 are TLS 1.2 only.
const TLS13_SCHEMES: [SignatureScheme; 8] = [
    SignatureScheme::ECDSA_NISTP256_SHA256,
    SignatureScheme::ECDSA_NISTP384_SHA384,
    SignatureScheme::ECDSA_NISTP521_SHA512,
    SignatureScheme::RSA_PSS_SHA256,
    SignatureScheme::RSA_PSS_SHA384,
    SignatureScheme::RSA_PSS_SHA512,
    SignatureScheme::ED25519,
    SignatureScheme::ED448,
];

/// Error of building the client.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A point in time, in seconds since 1970-01-01 UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(pub u64);

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // H. Hinnant's `civil_from_days`.
        let days = self.0 / 86_400 + 719_468;
        let era = days / 146_097;
        let day_of_era = days % 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
        let year = year_of_era + era * 400 + u64::from(month <= 2);
        write!(f, "{year:04}-{month:02}-{day:02}")
    }
}

/// Why a server is not trusted or the client cannot be built.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// The root certificate given to [`client_with_root`] is malformed.
    #[error("the root certificate is malformed")]
    BadRoot,
    /// The server's certificate is malformed.
    #[error("the server certificate is malformed")]
    Malformed,
    /// The built-in root is not valid at the current time.
    #[error(
        "the Shelly root certificate in this program is valid from {from} to {until}, not now; \
         check the system clock or update shelly2esphome"
    )]
    RootNotValid {
        /// Start of its validity.
        from: Timestamp,
        /// End of its validity.
        until: Timestamp,
    },
    /// The server's certificate is not issued by the root.
    #[error("the server certificate is not issued by Shelly's root certificate")]
    UnknownIssuer,
    /// The server's certificate has extensions. Shelly's have none, and
    /// extensions can restrict a certificate (for example to client
    /// authentication), which this tool does not interpret.
    #[error("the server certificate has extensions, which Shelly's certificates do not have")]
    Extensions,
    /// No supported algorithm matches the key and signature.
    #[error("the signature algorithm of the server is not supported")]
    UnsupportedAlgorithm,
    /// A signature does not match.
    #[error("a signature of the server does not match (forged or damaged certificate)")]
    BadSignature,
    /// The server's certificate is not valid at the current time.
    #[error(
        "the server certificate is valid from {from} to {until}, not now; check the system clock"
    )]
    NotValidNow {
        /// Start of its validity.
        from: Timestamp,
        /// End of its validity.
        until: Timestamp,
    },
    /// The server's certificate is for another host.
    #[error("the server certificate is for {}, not for {host}", names.join(", "))]
    WrongHost {
        /// The requested host.
        host: String,
        /// The certificate's common names.
        names: Vec<String>,
    },
    /// The TLS configuration or the HTTP client cannot be built.
    #[error("the HTTPS client cannot be built: {0}")]
    Client(BoxError),
}

/// rustls shows `CertificateError::Other` only in its debug form; the
/// top-level `Other` shows the message.
fn rustls_error(error: TlsError) -> rustls::Error {
    rustls::Error::Other(rustls::OtherError(Arc::new(error)))
}

/// The parts of the root the checks use.
#[derive(Debug)]
struct Root {
    subject: Vec<u8>,
    key_algorithm: Vec<u8>,
    public_key: Vec<u8>,
    not_before: u64,
    not_after: u64,
}

/// Finds the algorithm for the key (and, for certificates, the signature
/// algorithm) and checks the signature.
fn verify<'a>(
    mut candidates: impl Iterator<Item = &'a &'static dyn SignatureVerificationAlgorithm>,
    key_algorithm: &[u8],
    signature_algorithm: Option<&[u8]>,
    public_key: &[u8],
    message: &[u8],
    signature: &[u8],
) -> Result<(), TlsError> {
    let algorithm = candidates
        .find(|algorithm| {
            *algorithm.public_key_alg_id() == *key_algorithm
                && signature_algorithm.is_none_or(|wanted| *algorithm.signature_alg_id() == *wanted)
        })
        .ok_or(TlsError::UnsupportedAlgorithm)?;
    algorithm.verify_signature(public_key, message, signature).or(Err(TlsError::BadSignature))
}

/// Trusts server certificates signed by one root.
#[derive(Debug)]
struct Verifier {
    root: Root,
    algorithms: WebPkiSupportedAlgorithms,
}

impl Verifier {
    fn new(root: &[u8]) -> Result<Self, TlsError> {
        let root = Certificate::parse(root).ok_or(TlsError::BadRoot)?;
        Ok(Self {
            root: Root {
                subject: root.subject.to_vec(),
                key_algorithm: root.key_algorithm.to_vec(),
                public_key: root.public_key.to_vec(),
                not_before: root.not_before,
                not_after: root.not_after,
            },
            algorithms: ring::default_provider().signature_verification_algorithms,
        })
    }

    /// The checks of a server certificate, in the order of their messages'
    /// usefulness: a stale root or clock first, a wrong host last.
    fn check(&self, end_entity: &[u8], host: &str, now: u64) -> Result<(), TlsError> {
        let root = &self.root;
        if !(root.not_before..=root.not_after).contains(&now) {
            return Err(TlsError::RootNotValid {
                from: Timestamp(root.not_before),
                until: Timestamp(root.not_after),
            });
        }
        let cert = Certificate::parse(end_entity).ok_or(TlsError::Malformed)?;
        if cert.issuer != root.subject {
            return Err(TlsError::UnknownIssuer);
        }
        verify(
            self.algorithms.all.iter(),
            &root.key_algorithm,
            Some(cert.signature_algorithm),
            &root.public_key,
            cert.tbs,
            cert.signature,
        )?;
        if cert.has_extensions {
            return Err(TlsError::Extensions);
        }
        if !(cert.not_before..=cert.not_after).contains(&now) {
            return Err(TlsError::NotValidNow {
                from: Timestamp(cert.not_before),
                until: Timestamp(cert.not_after),
            });
        }
        if !cert.common_names.iter().any(|name| name.eq_ignore_ascii_case(host)) {
            return Err(TlsError::WrongHost { host: host.to_owned(), names: cert.common_names });
        }
        Ok(())
    }

    /// Checks a handshake signature with the key of the (already verified)
    /// server certificate.
    fn handshake(
        &self,
        message: &[u8],
        cert: &[u8],
        scheme: SignatureScheme,
        signature: &[u8],
        tls13: bool,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // rustls verifies the certificate before the handshake signature, so
        // it parses here.
        Certificate::parse(cert)
            .ok_or(TlsError::Malformed)
            .and_then(|cert| {
                let candidates = self
                    .algorithms
                    .mapping
                    .iter()
                    .filter(|(offered, _)| {
                        *offered == scheme && (!tls13 || TLS13_SCHEMES.contains(offered))
                    })
                    // TLS 1.3 binds an ECDSA scheme to one curve: only the
                    // first algorithm; the cross-curve ones are TLS 1.2 only.
                    .flat_map(|(_, algorithms)| {
                        algorithms.iter().take(if tls13 { 1 } else { algorithms.len() })
                    });
                verify(candidates, cert.key_algorithm, None, cert.public_key, message, signature)
            })
            .map(|()| HandshakeSignatureValid::assertion())
            .map_err(rustls_error)
    }
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.check(end_entity, &server_name.to_str(), now.as_secs())
            .map(|()| ServerCertVerified::assertion())
            .map_err(rustls_error)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.handshake(message, cert, dss.scheme, dss.signature(), false)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.handshake(message, cert, dss.scheme, dss.signature(), true)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// An HTTPS client from `builder` that trusts only Shelly's servers.
///
/// # Errors
/// Returns a [`TlsError`] if the client cannot be built.
pub fn client(builder: reqwest::ClientBuilder) -> Result<reqwest::Client, TlsError> {
    client_with_root(ROOT, builder)
}

/// Like [`client`] with another root certificate (DER), for tests.
///
/// # Errors
/// Returns [`TlsError::BadRoot`] if `root` is malformed and
/// [`TlsError::Client`] if the client cannot be built.
pub fn client_with_root(
    root: &[u8],
    builder: reqwest::ClientBuilder,
) -> Result<reqwest::Client, TlsError> {
    let verifier = Arc::new(Verifier::new(root)?);
    ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map(|config| {
            config.dangerous().with_custom_certificate_verifier(verifier).with_no_client_auth()
        })
        .map_err(BoxError::from)
        .and_then(|config| {
            builder
                .use_preconfigured_tls(config)
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(BoxError::from)
        })
        .map_err(TlsError::Client)
}

#[cfg(test)]
#[path = "../unit_tests/shelly_tls/mod.rs"]
mod tests;
