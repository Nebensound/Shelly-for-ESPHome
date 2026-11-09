//! Unit tests of `crate::backup` (not counted as production code for coverage).

use super::*;

#[test]
fn file_names_follow_the_endpoint() {
    assert_eq!(file_name("/shelly"), "shelly.json");
    assert_eq!(file_name("/settings/relay/0"), "settings-relay-0.json");
}

#[test]
fn timestamps_are_utc() {
    assert_eq!(utc_timestamp(0), "19700101T000000Z");
    assert_eq!(utc_timestamp(951_782_400), "20000229T000000Z");
    assert_eq!(utc_timestamp(1_762_667_461), "20251109T055101Z");
    assert_eq!(utc_timestamp(4_107_542_399), "21000228T235959Z");
}

#[test]
fn writes_one_file_per_document() {
    let backup = Backup {
        info: StockInfo {
            device_type: "SHSW-PM".into(),
            mac: "AABBCCDDEEFF".into(),
            auth: false,
            fw: "1.14.0".into(),
        },
        files: vec![
            ("shelly.json".into(), serde_json::json!({"type": "SHSW-PM"})),
            ("settings.json".into(), serde_json::json!({"relays": [{}]})),
        ],
    };
    let parent = std::env::temp_dir().join(format!("s2e-backup-{}", std::process::id()));
    let directory = backup.write_to(&parent).expect("write");
    let name = directory.file_name().and_then(|n| n.to_str()).expect("name").to_owned();
    assert!(name.starts_with("shelly-backup-SHSW-PM-AABBCCDDEEFF-"));
    let settings = std::fs::read_to_string(directory.join("settings.json")).expect("read");
    assert!(settings.contains("\"relays\""));
    assert!(settings.ends_with('\n'));
    std::fs::remove_dir_all(&parent).expect("clean up");

    // A file where the directory should be makes writing fail.
    let blocker = std::env::temp_dir().join(format!("s2e-blocker-{}", std::process::id()));
    std::fs::write(&blocker, "x").expect("blocker");
    let error = backup.write_to(&blocker).expect_err("not a directory");
    assert!(error.to_string().starts_with("could not write the backup"));
    std::fs::remove_file(&blocker).expect("clean up");
}

#[test]
fn reports_a_file_that_cannot_be_written() {
    let backup = Backup {
        info: StockInfo {
            device_type: "SHSW-PM".into(),
            mac: "AABBCCDDEEFF".into(),
            auth: false,
            fw: "1.14.0".into(),
        },
        files: vec![("shelly.json".into(), serde_json::json!({}))],
    };
    let directory = std::env::temp_dir().join(format!("s2e-write-{}", std::process::id()));
    // A directory where the file should go.
    std::fs::create_dir_all(directory.join("shelly.json")).expect("blocker");
    let error = backup.write_into(&directory).expect_err("cannot write");
    assert!(matches!(error, BackupError::Write(ref path, _) if path == &directory));
    std::fs::remove_dir_all(&directory).expect("clean up");
}
