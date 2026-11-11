//! Synthetic certificates with the structure of Shelly's: a self-signed
//! version 1 root and version 1 server certificates that name the host only
//! in the common name. Ed25519 keys from fixed seeds keep them reproducible.
//! Public for the integration tests; not part of the CLI.

use ring::signature::{Ed25519KeyPair, KeyPair};

/// `AlgorithmIdentifier` of Ed25519 (OID 1.3.101.112, no parameters).
const ED25519: [u8; 7] = [0x30, 0x05, 0x06, 0x03, 0x2B, 0x65, 0x70];

/// One DER TLV.
#[must_use]
pub fn tlv(tag: u8, contents: &[u8]) -> Vec<u8> {
    let [high, low] = u16::try_from(contents.len()).unwrap_or(u16::MAX).to_be_bytes();
    let mut out = match contents.len() {
        0..0x80 => vec![tag, low],
        0x80..0x100 => vec![tag, 0x81, low],
        _ => vec![tag, 0x82, high, low],
    };
    out.extend_from_slice(contents);
    out
}

/// A Name with one attribute; `attribute` is the last byte of the OID
/// 2.5.4.x (3 = common name, 10 = organization).
#[must_use]
pub fn name(attribute: u8, value: &str) -> Vec<u8> {
    let pair = [tlv(0x06, &[0x55, 0x04, attribute]), tlv(0x0C, value.as_bytes())].concat();
    tlv(0x30, &tlv(0x31, &tlv(0x30, &pair)))
}

/// UTCTime for 13 characters, GeneralizedTime otherwise.
fn time(text: &str) -> Vec<u8> {
    tlv(if text.len() == 13 { 0x17 } else { 0x18 }, text.as_bytes())
}

/// The subjectPublicKeyInfo of the key with seed byte `key`.
#[must_use]
pub fn public_key_info(key: u8) -> Vec<u8> {
    let public = Ed25519KeyPair::from_seed_unchecked(&[key; 32])
        .map(|pair| pair.public_key().as_ref().to_vec())
        .unwrap_or_default();
    tlv(0x30, &[&ED25519[..], &tlv(0x03, &[&[0][..], &public].concat())].concat())
}

/// The PKCS#8 encoding of the key with seed byte `key`, for a TLS server.
#[must_use]
pub fn private_key(key: u8) -> Vec<u8> {
    let prefix = [
        0x30, 0x2E, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2B, 0x65, 0x70, 0x04, 0x22, 0x04,
        0x20,
    ];
    [&prefix[..], &[key; 32]].concat()
}

/// What a certificate states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertSpec {
    /// Subject Name (whole TLV).
    pub subject: Vec<u8>,
    /// Start of the validity (`YYMMDDHHMMSSZ` or `YYYYMMDDHHMMSSZ`).
    pub not_before: String,
    /// End of the validity.
    pub not_after: String,
    /// Seed byte of the certified key.
    pub key: u8,
    /// Version 3 with an (empty) extensions field instead of version 1.
    pub v3: bool,
}

impl CertSpec {
    /// A server certificate for `host`, valid from 2020 to 2049.
    #[must_use]
    pub fn server(host: &str) -> Self {
        Self {
            subject: name(3, host),
            not_before: "200101000000Z".into(),
            not_after: "491231235959Z".into(),
            key: 0x51,
            v3: false,
        }
    }
}

/// The `tbsCertificate` fields of `spec` issued by `issuer`, in order, so
/// tests can replace single fields.
#[must_use]
pub fn tbs_fields(issuer: &[u8], spec: &CertSpec) -> Vec<Vec<u8>> {
    let mut fields = Vec::new();
    if spec.v3 {
        fields.push(tlv(0xA0, &tlv(0x02, &[2])));
    }
    fields.extend([
        tlv(0x02, &[0x01, 0x23]),
        ED25519.to_vec(),
        issuer.to_vec(),
        tlv(0x30, &[time(&spec.not_before), time(&spec.not_after)].concat()),
        spec.subject.clone(),
        public_key_info(spec.key),
    ]);
    if spec.v3 {
        fields.push(tlv(0xA3, &tlv(0x30, &[])));
    }
    fields
}

/// A certificate with these `tbsCertificate` fields, signed with the key
/// with seed byte `signer`.
#[must_use]
pub fn certificate(fields: &[Vec<u8>], signer: u8) -> Vec<u8> {
    let tbs = tlv(0x30, &fields.concat());
    let signature = Ed25519KeyPair::from_seed_unchecked(&[signer; 32])
        .map(|pair| pair.sign(&tbs).as_ref().to_vec())
        .unwrap_or_default();
    tlv(0x30, &[tbs, ED25519.to_vec(), tlv(0x03, &[&[0][..], &signature].concat())].concat())
}

/// A certificate authority like Shelly's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestCa {
    /// Seed byte of the CA key.
    pub key: u8,
    /// The CA's Name (whole TLV).
    pub name: Vec<u8>,
}

impl TestCa {
    /// A CA named `O=<organization>` with the key of seed byte `key`.
    #[must_use]
    pub fn new(key: u8, organization: &str) -> Self {
        Self { key, name: name(10, organization) }
    }

    /// The self-signed version 1 root, valid from 2020 to 2049.
    #[must_use]
    pub fn root(&self) -> Vec<u8> {
        let spec = CertSpec { subject: self.name.clone(), key: self.key, ..CertSpec::server("") };
        certificate(&tbs_fields(&self.name, &spec), self.key)
    }

    /// A certificate as `spec` states, signed by this CA.
    #[must_use]
    pub fn issue(&self, spec: &CertSpec) -> Vec<u8> {
        certificate(&tbs_fields(&self.name, spec), self.key)
    }
}
