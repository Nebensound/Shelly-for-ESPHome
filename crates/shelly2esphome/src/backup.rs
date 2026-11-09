//! Backup of the stock firmware's settings before they are lost.
//!
//! Gen1 settings are not kept by the migration firmware or by ESPHome, so the
//! JSON answers of the stock firmware are saved to disk: device info, status,
//! settings, actions and the settings of every relay, light and roller.

use std::{
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::device::{DeviceClient, DeviceError, StockInfo};

/// Channel lists in `/settings` and the endpoint of one channel.
const CHANNELS: [(&str, &str); 3] =
    [("relays", "/settings/relay"), ("lights", "/settings/light"), ("rollers", "/settings/roller")];

/// The collected answers, one JSON document per endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct Backup {
    /// Device info (`/shelly`).
    pub info: StockInfo,
    /// File name (without directory) and content of every saved document.
    pub files: Vec<(String, serde_json::Value)>,
}

/// Reasons why a backup could not be made.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// Reading from the device failed.
    #[error(transparent)]
    Device(#[from] DeviceError),
    /// The device does not answer `/shelly` or `/settings` like the Gen1 stock firmware.
    #[error("the device does not run the Shelly Gen1 stock firmware; there is nothing to back up")]
    NotStock,
    /// Writing the files failed.
    #[error("could not write the backup to {0}: {1}")]
    Write(PathBuf, #[source] io::Error),
}

/// File name for the answer of `path` (`/settings/relay/0` → `settings-relay-0.json`).
fn file_name(path: &str) -> String {
    format!("{}.json", path.trim_start_matches('/').replace('/', "-"))
}

/// Reads all settings of a device running the stock firmware.
///
/// # Errors
/// Fails if the device is unreachable, needs a login or does not run the
/// Gen1 stock firmware.
pub async fn collect(client: &DeviceClient) -> Result<Backup, BackupError> {
    let shelly = client.get_json("/shelly", "/shelly").await?.ok_or(BackupError::NotStock)?;
    let info: StockInfo =
        serde_json::from_value(shelly.clone()).map_err(|_| BackupError::NotStock)?;
    let settings = client.get_json("/settings", "/settings").await?.ok_or(BackupError::NotStock)?;

    let mut files = vec![(file_name("/shelly"), shelly)];
    if let Some(status) = client.get_json("/status", "/status").await? {
        files.push((file_name("/status"), status));
    }
    for (key, endpoint) in CHANNELS {
        let count = settings.get(key).and_then(serde_json::Value::as_array).map_or(0, Vec::len);
        for index in 0..count {
            let path = format!("{endpoint}/{index}");
            if let Some(channel) = client.get_json(endpoint, &path).await? {
                files.push((file_name(&path), channel));
            }
        }
    }
    // Older firmware versions have no /settings/actions.
    if let Some(actions) = client.get_json("/settings/actions", "/settings/actions").await? {
        files.push((file_name("/settings/actions"), actions));
    }
    files.push((file_name("/settings"), settings));
    Ok(Backup { info, files })
}

/// `YYYYMMDDTHHMMSSZ` for `unix_seconds` (UTC).
#[must_use]
pub fn utc_timestamp(unix_seconds: u64) -> String {
    let days = unix_seconds / 86_400;
    let seconds_of_day = unix_seconds % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        seconds_of_day / 3_600,
        seconds_of_day / 60 % 60,
        seconds_of_day % 60
    )
}

impl Backup {
    /// Writes the backup into a new directory below `parent` and returns it.
    ///
    /// The directory is named `shelly-backup-<type>-<mac>-<UTC time>`.
    ///
    /// # Errors
    /// Fails if the directory or a file cannot be written.
    pub fn write_to(&self, parent: &Path) -> Result<PathBuf, BackupError> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
        let directory = parent.join(format!(
            "shelly-backup-{}-{}-{}",
            self.info.device_type,
            self.info.mac,
            utc_timestamp(now)
        ));
        let failed = |error| BackupError::Write(directory.clone(), error);
        std::fs::create_dir_all(&directory).map_err(failed)?;
        for (name, content) in &self.files {
            // Serializing a `Value` cannot fail.
            let text = serde_json::to_string_pretty(content).unwrap_or_default();
            std::fs::write(directory.join(name), text + "\n").map_err(failed)?;
        }
        Ok(directory)
    }
}

#[cfg(test)]
mod tests {
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
}
