//! Unit tests of `crate::shelly_tls::cert` (not counted as production code for coverage).

use super::*;
use crate::shelly_tls::{
    ROOT,
    synthetic::{CertSpec, TestCa, certificate, name, tbs_fields, tlv},
};

const LEAF: &[u8] = include_bytes!("../fixtures/updates.shelly.cloud.der");

#[test]
fn reads_shellys_root_and_server_certificate() {
    let root = Certificate::parse(ROOT).expect("root");
    assert_eq!(root.issuer, root.subject);
    assert!(root.common_names.is_empty());
    assert_eq!((root.not_before, root.not_after), (1_596_542_621, 1_911_902_621));
    // rsaEncryption with NULL parameters.
    assert_eq!(root.key_algorithm, [6, 9, 42, 134, 72, 134, 247, 13, 1, 1, 1, 5, 0]);

    let leaf = Certificate::parse(LEAF).expect("leaf");
    assert_eq!(leaf.issuer, root.subject);
    assert_eq!(leaf.common_names, ["updates.shelly.cloud"]);
    // sha256WithRSAEncryption.
    assert_eq!(leaf.signature_algorithm, [6, 9, 42, 134, 72, 134, 247, 13, 1, 1, 11, 5, 0]);
    // The signed bytes follow the outer header unchanged.
    assert!(LEAF[4..].starts_with(leaf.tbs));
}

#[test]
fn reads_version_3_and_generalized_time() {
    let ca = TestCa::new(1, "Test CA");
    let spec = CertSpec {
        v3: true,
        not_after: "20500101000000Z".into(),
        ..CertSpec::server("updates.shelly.cloud")
    };
    let der = ca.issue(&spec);
    let cert = Certificate::parse(&der).expect("v3");
    assert_eq!(cert.not_after, 2_524_608_000);
    assert_eq!(cert.common_names, ["updates.shelly.cloud"]);
    assert_eq!(cert.issuer, &ca.name[2..]);
}

#[test]
fn reads_der_lengths_and_refuses_bad_ones() {
    let long = vec![0xAB; 300];
    let mut der = [tlv(0x04, &long), tlv(0x04, &[1; 200]), vec![0x05, 0x00]].concat();
    der.push(0xFF);
    let mut reader = Der(&der);
    assert_eq!(reader.next().map(|(tag, contents, _)| (tag, contents.len())), Some((4, 300)));
    assert_eq!(
        reader.next().map(|(_, contents, whole)| (contents.len(), whole.len())),
        Some((200, 203))
    );
    assert_eq!(reader.next().map(|(tag, contents, _)| (tag, contents)), Some((5, &[][..])));
    assert!(reader.end().is_none());
    // Only the tag left.
    assert!(reader.next().is_none());

    for bad in [
        &[][..],
        &[0x30, 0x80, 0x00, 0x00],       // indefinite length
        &[0x30, 0x83, 0x00, 0x00, 0x01], // three length bytes
        &[0x30, 0x82, 0x01],             // length bytes cut off
        &[0x30, 0x03, 0x01, 0x02],       // contents cut off
    ] {
        assert!(Der(bad).next().is_none(), "{bad:02x?}");
    }
    assert!(Der(&[0x02, 0x01, 0x00]).expect(0x30).is_none());
    assert_eq!(Der(&[0x02, 0x01, 0x00]).skip(0x30), Some(()));
    assert!(Der(&[0x30, 0x05]).skip(0x30).is_none());
    assert!(bit_string(&[1, 0xFF]).is_none());
    assert!(bit_string(&[]).is_none());
}

fn time_of(tag: u8, text: &[u8]) -> Option<u64> {
    time(&mut Der(&tlv(tag, text)))
}

#[test]
fn reads_both_time_formats() {
    assert_eq!(time_of(UTC_TIME, b"200101000000Z"), Some(1_577_836_800));
    assert_eq!(time_of(UTC_TIME, b"991231235959Z"), Some(946_684_799));
    assert_eq!(time_of(UTC_TIME, b"700301000000Z"), Some(5_097_600));
    assert_eq!(time_of(GENERALIZED_TIME, b"20240229120000Z"), Some(1_709_208_000));
    for (tag, bad) in [
        (UTC_TIME, &b"Z"[..]),                  // too short for a year
        (UTC_TIME, b"2001010000Z"),             // too short
        (UTC_TIME, b"200101000000"),            // no Z
        (UTC_TIME, b"2001010000AAZ"),           // not digits
        (UTC_TIME, b"201301000000Z"),           // month 13
        (UTC_TIME, b"200100000000Z"),           // day 0
        (UTC_TIME, b"200101240000Z"),           // hour 24
        (UTC_TIME, b"200101006000Z"),           // minute 60
        (UTC_TIME, b"200101000060Z"),           // second 60
        (UTC_TIME, b"\xFF\xFF0101000000Z"),     // not text
        (GENERALIZED_TIME, b"19691231235959Z"), // before 1970
        (INTEGER, b"200101000000Z"),            // no time
    ] {
        assert_eq!(time_of(tag, bad), None, "{tag:#x} {}", String::from_utf8_lossy(bad));
    }
    assert_eq!(time(&mut Der(&[])), None);
}

#[test]
fn collects_common_names() {
    let pair = |oid: u8, value: &str| {
        tlv(0x30, &[tlv(0x06, &[0x55, 0x04, oid]), tlv(0x13, value.as_bytes())].concat())
    };
    // Two RDNs, one with two attributes; only common names count.
    let name =
        [tlv(0x31, &[pair(3, "a"), pair(10, "org")].concat()), tlv(0x31, &pair(3, "b"))].concat();
    assert_eq!(common_names(&name), Some(vec!["a".to_owned(), "b".to_owned()]));
    assert_eq!(common_names(&[]), Some(vec![]));
    for bad in [
        tlv(0x30, &pair(3, "a")),                               // RDN is no SET
        tlv(0x31, &tlv(0x31, &[])),                             // attribute is no SEQUENCE
        tlv(0x31, &tlv(0x30, &tlv(0x13, b"a"))),                // no OID
        tlv(0x31, &tlv(0x30, &tlv(0x06, &[0x55, 0x04, 0x03]))), // no value
    ] {
        assert_eq!(common_names(&bad), None, "{bad:02x?}");
    }
}

#[test]
fn refuses_malformed_certificates() {
    let ca = TestCa::new(1, "Test CA");
    let v3 = CertSpec { v3: true, ..CertSpec::server("host") };
    let good = tbs_fields(&ca.name, &v3);
    let alg = good[2].clone();
    let with = |index: usize, field: Vec<u8>| {
        let mut fields = good.clone();
        fields[index] = field;
        certificate(&fields, 1)
    };
    let validity = |times: &[Vec<u8>]| tlv(0x30, &times.concat());
    let time = |text: &str| tlv(UTC_TIME, text.as_bytes());
    let key_info = |parts: &[Vec<u8>]| tlv(0x30, &parts.concat());
    let key_alg = tlv(0x30, &[0x06, 0x03, 0x2B, 0x65, 0x70]);
    let mut appended = good.clone();
    appended.push(tlv(0x05, &[]));
    let tbs = tlv(0x30, &good.concat());
    let signature = tlv(BIT_STRING, &[0, 1, 2]);

    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("trailing bytes", [certificate(&good, 1), vec![0]].concat()),
        ("no SEQUENCE", tlv(0x31, &[])),
        (
            "tbs no SEQUENCE",
            tlv(0x30, &[tlv(0x31, &good.concat()), alg.clone(), signature.clone()].concat()),
        ),
        ("no algorithm", tlv(0x30, &tbs)),
        ("no signature", tlv(0x30, &[tbs.clone(), alg.clone()].concat())),
        ("unused bits", tlv(0x30, &[tbs.clone(), alg.clone(), tlv(BIT_STRING, &[1, 0])].concat())),
        (
            "extra element",
            tlv(0x30, &[tbs.clone(), alg.clone(), signature.clone(), vec![5, 0]].concat()),
        ),
        ("bad version length", with(0, vec![VERSION, 0x80])),
        ("no serial", with(1, tlv(0x04, &[1]))),
        ("other inner algorithm", with(2, tlv(0x30, &[0x06, 0x01, 0x01]))),
        ("no issuer", with(3, tlv(0x31, &[]))),
        ("no validity", with(4, tlv(0x31, &[]))),
        ("bad start", with(4, validity(&[time("2001"), time("491231235959Z")]))),
        ("bad end", with(4, validity(&[time("200101000000Z"), time("49")]))),
        (
            "three times",
            with(
                4,
                validity(&[time("200101000000Z"), time("491231235959Z"), time("491231235959Z")]),
            ),
        ),
        ("no subject", with(5, tlv(0x31, &[]))),
        ("bad subject", with(5, tlv(0x30, &tlv(0x30, &[])))),
        ("no key info", with(6, tlv(0x31, &[]))),
        ("no key algorithm", with(6, key_info(&[tlv(BIT_STRING, &[0, 1])]))),
        ("no key", with(6, key_info(std::slice::from_ref(&key_alg)))),
        (
            "extra key part",
            with(6, key_info(&[key_alg.clone(), tlv(BIT_STRING, &[0, 1]), vec![5, 0]])),
        ),
        ("cut issuer id", with(7, vec![ISSUER_UNIQUE_ID, 0x05])),
        ("cut subject id", with(7, vec![SUBJECT_UNIQUE_ID, 0x05])),
        ("cut extensions", with(7, vec![EXTENSIONS, 0x05])),
        ("extra field", certificate(&appended, 1)),
    ];
    assert!(Certificate::parse(&certificate(&good, 1)).is_some());
    for (case, der) in cases {
        assert_eq!(Certificate::parse(&der), None, "{case}");
    }
    // A name with a common name attribute is read as such.
    assert_eq!(
        Certificate::parse(&with(5, name(3, "x"))).map(|cert| cert.common_names),
        Some(vec!["x".to_owned()])
    );
}
