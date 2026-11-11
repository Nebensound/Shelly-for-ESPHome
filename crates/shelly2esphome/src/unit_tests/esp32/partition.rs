//! Unit tests of `crate::esp32::partition` (not counted as production code for coverage).

use super::*;
use crate::esp32::synthetic::{LAYOUT, table, table_with_flags};

#[test]
fn reads_a_table_with_and_without_checksum() {
    let parsed = PartitionTable::parse(&table(&LAYOUT)).expect("table");
    assert_eq!(parsed.entries().len(), LAYOUT.len());
    let app = parsed.find("app_0").expect("app_0");
    assert_eq!((app.kind, app.subtype, app.offset, app.size), (0, 0x10, 0x1_0000, 0x1_0000));
    assert_eq!(app.to_string(), "type 0/0x10 at 0x10000, 0x10000 bytes");
    assert_eq!(app.flags, 0);
    assert!(parsed.find("missing").is_none());

    // ESP-IDF can build tables without the MD5 entry.
    let mut without = table(&LAYOUT[..2]);
    without.drain(64..96);
    assert_eq!(PartitionTable::parse(&without).expect("without md5").entries().len(), 2);
}

#[test]
fn refuses_malformed_and_damaged_tables() {
    let parse = |bytes: &[u8]| PartitionTable::parse(bytes).expect_err("must fail");
    assert_eq!(parse(&[0x00; 64]), PartitionError::BadMagic(0));
    assert_eq!(parse(&[0xFF; 32]), PartitionError::Empty);
    // No end marker within the data.
    let good = table(&LAYOUT[..1]);
    assert_eq!(parse(&good[..32]), PartitionError::Unterminated);
    // No end marker within the largest table ESP-IDF reads.
    let entry = &good[..32];
    let endless: Vec<u8> = entry.iter().copied().cycle().take(MAX_TABLE_LEN + 64).collect();
    assert_eq!(parse(&endless), PartitionError::Unterminated);
    let mut damaged = good.clone();
    damaged[20] ^= 0x01;
    assert_eq!(parse(&damaged), PartitionError::Checksum);
    // A second app_0 at another offset, with a valid MD5.
    let mut doubled = LAYOUT.to_vec();
    doubled.push(("app_0", 0, 0x12, 0x5_0000, 0x1_0000));
    assert_eq!(parse(&table(&doubled)), PartitionError::DuplicateLabel("app_0".into()));
}

#[test]
fn compares_flags_too() {
    let reference = PartitionTable::parse(&table(&LAYOUT)).expect("reference");
    let encrypted =
        PartitionTable::parse(&table_with_flags(&LAYOUT, &[("nvs", 1)])).expect("flags");
    assert_eq!(encrypted.find("nvs").map(|nvs| nvs.flags), Some(1));
    assert_eq!(
        encrypted.differences(&reference).critical,
        ["nvs: type 1/0x02 at 0xb000, 0x5000 bytes, flags 0x1, the device has type 1/0x02 at \
          0xb000, 0x5000 bytes"]
    );
}

#[test]
fn tells_critical_from_other_differences() {
    let reference = PartitionTable::parse(&table(&LAYOUT)).expect("reference");
    assert_eq!(reference.differences(&reference), TableDifferences::default());

    let mut changed = LAYOUT.to_vec();
    changed[2].4 = 0x2_0000; // app_0 larger
    changed.retain(|entry| entry.0 != "fs_1" && entry.0 != "shelly");
    changed.push(("coredump", 1, 0x03, 0x5_0000, 0x1000));
    let own = PartitionTable::parse(&table(&changed)).expect("changed");
    let differences = own.differences(&reference);
    assert_eq!(
        differences.critical,
        [
            "app_0: type 0/0x10 at 0x10000, 0x20000 bytes, the device has type 0/0x10 at \
             0x10000, 0x10000 bytes",
            "fs_1: missing, the device has it",
        ]
    );
    assert_eq!(
        differences.other,
        [
            "coredump: type 1/0x03 at 0x50000, 0x1000 bytes, missing on the device",
            "shelly: missing, the device has it",
        ]
    );
}
