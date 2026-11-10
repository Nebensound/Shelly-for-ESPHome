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

fn sample(device_type: &str, mac: &str) -> Backup {
    Backup {
        info: StockInfo {
            device_type: device_type.into(),
            mac: mac.into(),
            auth: false,
            fw: "1.14.0".into(),
        },
        files: vec![
            ("shelly.json".into(), serde_json::json!({"type": "SHSW-PM"})),
            ("settings.json".into(), serde_json::json!({"relays": [{}], "wifi_ap": {"key": "x"}})),
        ],
    }
}

/// A fresh, empty scratch directory for one test.
fn scratch(test: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("s2e-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    directory
}

fn name_of(directory: &Path) -> &str {
    directory.file_name().and_then(|n| n.to_str()).expect("name")
}

#[test]
fn directory_names_only_contain_safe_characters() {
    assert_eq!(safe_component("SHSW-PM_2"), "SHSW-PM_2");
    assert_eq!(safe_component("../../etc"), "______etc");
    assert_eq!(safe_component("a/b\\c d"), "a_b_c_d");
    assert_eq!(safe_component(""), "unknown");
}

#[test]
fn writes_one_file_per_document() {
    let parent = scratch("backup").join("missing-parent");
    let directory = sample("SHSW-PM", "AABBCCDDEEFF").write_to(&parent).expect("write");
    assert!(name_of(&directory).starts_with("shelly-backup-SHSW-PM-AABBCCDDEEFF-"));
    assert_eq!(directory.parent(), Some(parent.as_path()));
    let settings = std::fs::read_to_string(directory.join("settings.json")).expect("read");
    assert!(settings.contains("\"relays\""));
    assert!(settings.ends_with('\n'));
    std::fs::remove_dir_all(parent.parent().expect("scratch")).expect("clean up");
}

#[test]
fn device_data_cannot_leave_the_backup_directory() {
    let parent = scratch("traversal");
    let directory = sample("../../evil", "/tmp/x").write_to_at(&parent, 0).expect("write");
    assert_eq!(directory.parent(), Some(parent.as_path()));
    assert_eq!(name_of(&directory), "shelly-backup-______evil-_tmp_x-19700101T000000Z");
    std::fs::remove_dir_all(&parent).expect("clean up");
}

#[test]
fn backups_in_the_same_second_get_their_own_directory() {
    let parent = scratch("unique");
    let backup = sample("SHSW-PM", "AABBCCDDEEFF");
    let first = backup.write_to_at(&parent, 0).expect("first");
    let second = backup.write_to_at(&parent, 0).expect("second");
    let third = backup.write_to_at(&parent, 0).expect("third");
    assert_eq!(name_of(&first), "shelly-backup-SHSW-PM-AABBCCDDEEFF-19700101T000000Z");
    assert_eq!(name_of(&second), "shelly-backup-SHSW-PM-AABBCCDDEEFF-19700101T000000Z-2");
    assert_eq!(name_of(&third), "shelly-backup-SHSW-PM-AABBCCDDEEFF-19700101T000000Z-3");
    for directory in [&first, &second, &third] {
        assert!(directory.join("settings.json").is_file());
    }
    std::fs::remove_dir_all(&parent).expect("clean up");
}

#[test]
fn reports_a_parent_that_is_not_a_directory() {
    let blocker = scratch("blocker");
    std::fs::write(&blocker, "x").expect("blocker");
    let error = sample("SHSW-PM", "AABBCCDDEEFF").write_to(&blocker).expect_err("not a directory");
    assert!(matches!(error, BackupError::Write(ref path, _) if path == &blocker));
    assert!(error.to_string().starts_with("could not write the backup"));
    std::fs::remove_file(&blocker).expect("clean up");
}

#[test]
fn reports_a_directory_that_cannot_be_created() {
    let parent = scratch("create");
    std::fs::create_dir_all(&parent).expect("parent");
    // The parent of `missing/name` does not exist and is not created here.
    let error = create_unique_directory(&parent.join("missing"), "name").expect_err("no parent");
    assert!(
        matches!(error, BackupError::Write(ref path, _) if path == &parent.join("missing/name"))
    );
    std::fs::remove_dir_all(&parent).expect("clean up");
}

#[test]
fn never_overwrites_an_existing_file() {
    let directory = scratch("write");
    // A directory where the file should go.
    std::fs::create_dir_all(directory.join("shelly.json")).expect("blocker");
    let error = sample("SHSW-PM", "AABBCCDDEEFF").write_into(&directory).expect_err("exists");
    assert!(matches!(error, BackupError::Write(ref path, _) if path == &directory));
    std::fs::remove_dir_all(&directory).expect("clean up");
}

#[cfg(unix)]
#[test]
fn only_the_owner_can_read_the_backup() {
    use std::os::unix::fs::PermissionsExt;
    let parent = scratch("private");
    let directory = sample("SHSW-PM", "AABBCCDDEEFF").write_to(&parent).expect("write");
    let mode =
        |path: &Path| std::fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode(&directory), 0o700);
    assert_eq!(mode(&directory.join("settings.json")), 0o600);
    std::fs::remove_dir_all(&parent).expect("clean up");
}
