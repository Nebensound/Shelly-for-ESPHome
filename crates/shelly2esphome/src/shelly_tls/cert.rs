//! The fields of an X.509 certificate that the Shelly verifier needs, read
//! straight from the DER encoding.
//!
//! Shelly's server certificates are X.509 version 1: no extensions, the host
//! only in the subject's common name. General-purpose verifiers (webpki,
//! the platform verifiers) refuse those, so this reader accepts version 1 and
//! keeps the signed bytes exactly as received. It checks structure only;
//! signatures, validity and names are checked by the caller. Extensions are
//! not interpreted; the caller refuses certificates that have any.

use std::ops::Range;

const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const INTEGER: u8 = 0x02;
const BIT_STRING: u8 = 0x03;
const OID: u8 = 0x06;
const UTC_TIME: u8 = 0x17;
const GENERALIZED_TIME: u8 = 0x18;
const VERSION: u8 = 0xA0;
const ISSUER_UNIQUE_ID: u8 = 0x81;
const SUBJECT_UNIQUE_ID: u8 = 0x82;
const EXTENSIONS: u8 = 0xA3;
/// OID 2.5.4.3 (`id-at-commonName`), DER contents.
const COMMON_NAME: [u8; 3] = [0x55, 0x04, 0x03];

/// A DER reader over a sequence of TLVs.
struct Der<'a>(&'a [u8]);

impl<'a> Der<'a> {
    /// The next TLV as tag, contents and the whole encoding. Lengths up to
    /// two bytes (64 KiB) are accepted, which covers every certificate.
    fn next(&mut self) -> Option<(u8, &'a [u8], &'a [u8])> {
        let all = self.0;
        let (&tag, rest) = all.split_first()?;
        let (&first, rest) = rest.split_first()?;
        let (len, rest) = if first < 0x80 {
            (usize::from(first), rest)
        } else {
            let count = usize::from(first & 0x7F);
            let (bytes, rest) =
                rest.split_at_checked(count).filter(|_| (1..=2).contains(&count))?;
            (bytes.iter().fold(0, |len, byte| len << 8 | usize::from(*byte)), rest)
        };
        let (contents, rest) = rest.split_at_checked(len)?;
        self.0 = rest;
        Some((tag, contents, &all[..all.len() - rest.len()]))
    }

    /// The contents of the next TLV, which must have `tag`.
    fn expect(&mut self, tag: u8) -> Option<&'a [u8]> {
        self.next().filter(|(found, _, _)| *found == tag).map(|(_, contents, _)| contents)
    }

    /// Skips the next TLV if it has `tag`.
    fn skip(&mut self, tag: u8) -> Option<()> {
        if self.0.first() == Some(&tag) { self.expect(tag).map(drop) } else { Some(()) }
    }

    /// `Some` if every byte was read.
    fn end(&self) -> Option<()> {
        self.0.is_empty().then_some(())
    }
}

/// The contents of a BIT STRING without unused bits.
fn bit_string(contents: &[u8]) -> Option<&[u8]> {
    contents.split_first().filter(|(unused, _)| **unused == 0).map(|(_, bits)| bits)
}

/// Seconds since 1970 of a UTCTime or GeneralizedTime in the `…Z` form
/// RFC 5280 prescribes.
fn time(der: &mut Der<'_>) -> Option<u64> {
    let (tag, contents, _) = der.next()?;
    let text = std::str::from_utf8(contents).ok()?;
    let full = match tag {
        // Two-digit years: 50–99 are 19xx, 00–49 are 20xx (RFC 5280 4.1.2.5.1).
        UTC_TIME => format!("{}{text}", if text.get(..2)? >= "50" { "19" } else { "20" }),
        GENERALIZED_TIME => text.to_owned(),
        _ => return None,
    };
    let digits = full
        .strip_suffix('Z')
        .filter(|digits| digits.len() == 14 && digits.bytes().all(|byte| byte.is_ascii_digit()))?;
    // Only digits remain, so parsing cannot fail.
    let field = |range: Range<usize>| digits[range].parse::<i64>().unwrap_or_default();
    let (year, month, day) = (field(0..4), field(4..6), field(6..8));
    let (hour, minute, second) = (field(8..10), field(10..12), field(12..14));
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = [31, 28 + i64::from(leap), 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    // An impossible date (February 31st) is malformed, not March 3rd.
    let plausible = usize::try_from(month - 1)
        .ok()
        .and_then(|index| month_days.get(index))
        .is_some_and(|last| (1..=*last).contains(&day))
        && hour < 24
        && minute < 60
        && second < 60;
    // Days since 1970-01-01 of the proleptic Gregorian date (H. Hinnant's
    // `days_from_civil`).
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let year_of_era = shifted - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    plausible.then(|| days * 86_400 + hour * 3600 + minute * 60 + second).and_then(|seconds| {
        // Times before 1970 do not occur in server certificates.
        u64::try_from(seconds).ok()
    })
}

/// The common names in a Name (the contents of its SEQUENCE).
fn common_names(name: &[u8]) -> Option<Vec<String>> {
    let mut names = Vec::new();
    let mut rdns = Der(name);
    while rdns.end().is_none() {
        let mut set = Der(rdns.expect(SET)?);
        while set.end().is_none() {
            let mut attribute = Der(set.expect(SEQUENCE)?);
            let oid = attribute.expect(OID)?;
            let (_, value, _) = attribute.next()?;
            if oid == COMMON_NAME {
                names.push(String::from_utf8_lossy(value).into_owned());
            }
        }
    }
    Some(names)
}

/// A structurally valid certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Certificate<'a> {
    /// The signed part (`tbsCertificate`), exactly as encoded.
    pub tbs: &'a [u8],
    /// `signatureAlgorithm` (contents of the AlgorithmIdentifier).
    pub signature_algorithm: &'a [u8],
    /// The signature over [`Self::tbs`].
    pub signature: &'a [u8],
    /// The issuer Name (contents).
    pub issuer: &'a [u8],
    /// The subject Name (contents).
    pub subject: &'a [u8],
    /// Start of the validity in seconds since 1970.
    pub not_before: u64,
    /// End of the validity in seconds since 1970.
    pub not_after: u64,
    /// Algorithm of the public key (contents of the AlgorithmIdentifier).
    pub key_algorithm: &'a [u8],
    /// The public key (contents of the BIT STRING).
    pub public_key: &'a [u8],
    /// The subject's common names.
    pub common_names: Vec<String>,
    /// Whether the certificate has an extensions field.
    pub has_extensions: bool,
}

impl<'a> Certificate<'a> {
    /// Reads a DER certificate; `None` if it is malformed.
    pub(crate) fn parse(der: &'a [u8]) -> Option<Self> {
        let mut outer = Der(der);
        let mut certificate = Der(outer.expect(SEQUENCE)?);
        outer.end()?;
        let (_, tbs_contents, tbs) = certificate.next().filter(|(tag, _, _)| *tag == SEQUENCE)?;
        let signature_algorithm = certificate.expect(SEQUENCE)?;
        let signature = certificate.expect(BIT_STRING).and_then(bit_string)?;
        certificate.end()?;

        let mut fields = Der(tbs_contents);
        fields.skip(VERSION)?;
        fields.expect(INTEGER)?;
        // RFC 5280 4.1.1.2: both algorithm fields must be the same.
        fields.expect(SEQUENCE).filter(|inner| *inner == signature_algorithm)?;
        let issuer = fields.expect(SEQUENCE)?;
        let mut validity = Der(fields.expect(SEQUENCE)?);
        let not_before = time(&mut validity)?;
        let not_after = time(&mut validity)?;
        validity.end()?;
        let subject = fields.expect(SEQUENCE)?;
        let mut key_info = Der(fields.expect(SEQUENCE)?);
        let key_algorithm = key_info.expect(SEQUENCE)?;
        let public_key = key_info.expect(BIT_STRING).and_then(bit_string)?;
        key_info.end()?;
        fields.skip(ISSUER_UNIQUE_ID)?;
        fields.skip(SUBJECT_UNIQUE_ID)?;
        let has_extensions = fields.0.first() == Some(&EXTENSIONS);
        fields.skip(EXTENSIONS)?;
        fields.end()?;

        Some(Self {
            tbs,
            signature_algorithm,
            signature,
            issuer,
            subject,
            not_before,
            not_after,
            key_algorithm,
            public_key,
            common_names: common_names(subject)?,
            has_extensions,
        })
    }
}

#[cfg(test)]
#[path = "../unit_tests/shelly_tls/cert.rs"]
mod tests;
