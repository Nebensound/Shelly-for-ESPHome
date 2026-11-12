//! Backup of the stock firmware's settings before they are lost.
//!
//! Neither the migration firmware nor ESPHome keeps the stock settings, so
//! the JSON answers of the stock firmware are saved to disk. Gen1: device
//! info, status, settings, actions and the settings of every relay, light
//! and roller. Gen2 and later: device info, the configuration of every
//! component, schedules, webhooks, scripts with their code and the key-value
//! store.

use std::{
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    device::{DeviceClient, DeviceError, StockInfo},
    rpc::DeviceInfo,
    rpc_client::{RpcClient, RpcError},
};

/// Channel lists in `/settings` and the endpoint of one channel.
const CHANNELS: [(&str, &str); 3] =
    [("relays", "/settings/relay"), ("lights", "/settings/light"), ("rollers", "/settings/roller")];

/// The collected answers, one JSON document per endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct Backup {
    /// Device type (Gen1) or model ID (Gen2 and later), for the directory
    /// name.
    pub device: String,
    /// MAC address, for the directory name.
    pub mac: String,
    /// File name (without directory) and content of every saved document.
    pub files: Vec<(String, serde_json::Value)>,
}

/// Reasons why a backup could not be made.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// Reading from the device failed.
    #[error(transparent)]
    Device(#[from] DeviceError),
    /// Reading from a Gen2+ device failed.
    #[error("reading the settings failed: {0}")]
    Rpc(#[from] RpcError),
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
    Ok(Backup { device: info.device_type, mac: info.mac, files })
}

/// The RPC methods whose answer is saved as it is, and the file it goes to.
/// A device without the component refuses the call; the file is left out.
const OPTIONAL: [(&str, &str); 3] = [
    ("Schedule.List", "schedule-list.json"),
    ("Webhook.List", "webhook-list.json"),
    ("Script.List", "script-list.json"),
];

/// The answer, or `None` if the device refused the call (it lacks the
/// component).
fn optional(
    answer: Result<serde_json::Value, RpcError>,
) -> Result<Option<serde_json::Value>, RpcError> {
    match answer {
        Ok(value) => Ok(Some(value)),
        Err(RpcError::Refused { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The code of script `id`, which the device hands out in pieces.
async fn script_code(rpc: &RpcClient, id: &serde_json::Value) -> Result<String, RpcError> {
    let mut code = String::new();
    loop {
        let piece = rpc
            .call("Script.GetCode", serde_json::json!({ "id": id, "offset": code.len() }))
            .await?;
        let data = piece["data"].as_str().unwrap_or_default();
        code.push_str(data);
        if data.is_empty() || piece["left"].as_u64().unwrap_or_default() == 0 {
            return Ok(code);
        }
    }
}

/// Every entry of the key-value store, which the device hands out in pages.
async fn key_values(rpc: &RpcClient) -> Result<Option<serde_json::Value>, RpcError> {
    let mut items = Vec::new();
    loop {
        let params = serde_json::json!({ "offset": items.len() });
        let Some(page) = optional(rpc.call("KVS.GetMany", params).await)? else {
            return Ok(None);
        };
        let entries = page["items"].as_array().cloned().unwrap_or_default();
        let total = page["total"].as_u64().unwrap_or_default();
        let done = entries.is_empty();
        items.extend(entries);
        if done || items.len() as u64 >= total {
            return Ok(Some(serde_json::json!({ "items": items })));
        }
    }
}

/// Collects the settings of the Gen2+ device `info` describes over RPC.
///
/// # Errors
/// Fails if the device cannot be read; a component it lacks is left out.
pub async fn collect_rpc(rpc: &RpcClient, info: &DeviceInfo) -> Result<Backup, BackupError> {
    let call = |method| rpc.call(method, serde_json::json!({}));
    let mut files = vec![
        ("shelly-getdeviceinfo.json".to_owned(), call("Shelly.GetDeviceInfo").await?),
        ("shelly-getconfig.json".to_owned(), call("Shelly.GetConfig").await?),
    ];
    for (method, file) in OPTIONAL {
        if let Some(answer) = optional(call(method).await)? {
            files.push((file.to_owned(), answer));
        }
    }
    let scripts = files
        .iter()
        .find(|(file, _)| file == "script-list.json")
        .and_then(|(_, list)| list["scripts"].as_array().cloned())
        .unwrap_or_default();
    for script in scripts {
        let code = script_code(rpc, &script["id"]).await?;
        let file = format!("script-{}.json", script["id"]);
        files.push((file, serde_json::json!({ "script": script, "code": code })));
    }
    if let Some(store) = key_values(rpc).await? {
        files.push(("kvs-getmany.json".to_owned(), store));
    }
    Ok(Backup { device: info.model.to_string(), mac: info.mac.clone(), files })
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

/// `text` as a safe part of a directory name.
///
/// The device type and MAC address come from the device, so everything except
/// ASCII letters, digits, `-` and `_` is replaced; this keeps path separators
/// and `..` out of the backup directory's name.
fn safe_component(text: &str) -> String {
    let safe: String = text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if safe.is_empty() { "unknown".to_owned() } else { safe }
}

impl Backup {
    /// Writes the backup into a new directory below `parent` and returns it.
    ///
    /// The directory is named `shelly-backup-<type>-<mac>-<UTC time>`; if that
    /// already exists, `-2`, `-3`, … is appended. An existing backup is never
    /// written into or overwritten.
    ///
    /// # Errors
    /// Fails if the directory or a file cannot be written.
    pub fn write_to(&self, parent: &Path) -> Result<PathBuf, BackupError> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
        self.write_to_at(parent, now)
    }

    /// [`Backup::write_to`] with the time given in seconds since the Unix epoch.
    fn write_to_at(&self, parent: &Path, unix_seconds: u64) -> Result<PathBuf, BackupError> {
        let name = format!(
            "shelly-backup-{}-{}-{}",
            safe_component(&self.device),
            safe_component(&self.mac),
            utc_timestamp(unix_seconds)
        );
        std::fs::create_dir_all(parent)
            .map_err(|error| BackupError::Write(parent.to_path_buf(), error))
            .and_then(|()| create_unique_directory(parent, &name))
            .and_then(|directory| self.write_into(&directory).map(|()| directory))
    }

    /// Writes every document as a new file into the empty `directory`.
    ///
    /// The settings can contain the device's access point password, so the
    /// files are only accessible to the owner (on Unix).
    fn write_into(&self, directory: &Path) -> Result<(), BackupError> {
        let failed = |error| BackupError::Write(directory.to_path_buf(), error);
        for (name, content) in &self.files {
            // Serializing a `Value` cannot fail.
            let text = serde_json::to_string_pretty(content).unwrap_or_default() + "\n";
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            options
                .open(directory.join(name))
                .and_then(|mut file| io::Write::write_all(&mut file, text.as_bytes()))
                .map_err(failed)?;
        }
        Ok(())
    }
}

/// A builder for directories only the owner can access. Windows has no mode
/// bits; its directories keep the default permissions of their parent.
fn private_dir_builder() -> std::fs::DirBuilder {
    #[cfg(unix)]
    {
        let mut builder = std::fs::DirBuilder::new();
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder
    }
    #[cfg(not(unix))]
    {
        std::fs::DirBuilder::new()
    }
}

/// Creates a new directory `parent/name` (or `name-2`, `name-3`, …) that did
/// not exist before and is only accessible to the owner (on Unix).
fn create_unique_directory(parent: &Path, name: &str) -> Result<PathBuf, BackupError> {
    let builder = private_dir_builder();
    let mut attempt = 1_u32;
    loop {
        let directory = match attempt {
            1 => parent.join(name),
            _ => parent.join(format!("{name}-{attempt}")),
        };
        match builder.create(&directory) {
            Ok(()) => return Ok(directory),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => attempt += 1,
            Err(error) => return Err(BackupError::Write(directory, error)),
        }
    }
}

#[cfg(test)]
#[path = "unit_tests/backup.rs"]
mod tests;
