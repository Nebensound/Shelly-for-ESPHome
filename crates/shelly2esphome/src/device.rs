//! HTTP access to a Shelly Gen1 device, both in stock and in migration firmware.

use std::time::Duration;

use serde::Deserialize;

pub use crate::protocol::MIGRATION_PROTOCOL;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Login for devices with enabled web authentication, given as `user:password`.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    user: String,
    password: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials").field("user", &self.user).finish_non_exhaustive()
    }
}

impl std::str::FromStr for Credentials {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.split_once(':') {
            Some((user, password)) if !user.is_empty() => {
                Ok(Self { user: user.to_owned(), password: password.to_owned() })
            }
            _ => Err("expected user:password".to_owned()),
        }
    }
}

/// Errors while talking to the device.
#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    /// The device could not be reached or answered with an unreadable body.
    #[error("device unreachable or unreadable answer: {0}")]
    Http(#[from] reqwest::Error),
    /// The device requires a login that was not given or is wrong.
    #[error("the device requires a login (--auth user:password)")]
    AuthRequired,
    /// An endpoint answered with an unexpected HTTP status.
    #[error("{endpoint} answered with HTTP {status}")]
    Status {
        /// Path of the endpoint.
        endpoint: &'static str,
        /// HTTP status code.
        status: u16,
    },
    /// The migration firmware refused the request.
    #[error("the migration firmware refused: {0}")]
    Rejected(String),
    /// Neither the stock nor the migration firmware answered.
    #[error("unknown firmware: neither Shelly stock firmware nor migration firmware")]
    UnknownFirmware,
    /// The migration firmware speaks another protocol version.
    #[error("the migration firmware speaks protocol {0}, this tool protocol {MIGRATION_PROTOCOL}")]
    UnsupportedProtocol(u32),
}

/// Answer of the stock firmware's `/shelly` endpoint.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct StockInfo {
    /// Device type, e.g. `SHDM-2`.
    #[serde(rename = "type")]
    pub device_type: String,
    /// MAC address without separators.
    pub mac: String,
    /// Whether web authentication is enabled.
    pub auth: bool,
    /// Firmware build identifier.
    pub fw: String,
}

#[derive(Debug, Deserialize)]
struct StockStatusWire {
    wifi_sta: WifiStaWire,
}

#[derive(Debug, Deserialize)]
struct WifiStaWire {
    rssi: i32,
}

/// State of the flash job inside the migration firmware.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MigrationState {
    /// Waiting for `MG2X.Flash`.
    Idle,
    /// Downloading and verifying the image.
    Downloading,
    /// Image verified, moving it into place; the device restarts afterwards.
    Installing,
    /// The last job failed; the device accepts a new one.
    Failed,
    /// Moving the migration firmware into slot 1 failed; it retries after a restart.
    RelocationFailed,
}

/// Answer of `MG2X.Status`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MigrationStatus {
    /// Protocol version.
    pub protocol: u32,
    /// Current job state.
    pub state: MigrationState,
    /// Bytes received by the device for the current job.
    pub received: u64,
    /// Announced size of the current job.
    pub size: u64,
    /// Largest image the staging slot can hold.
    pub max_size: u64,
    /// Machine-readable reason of the last failure, empty otherwise.
    pub error: String,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct FlashStarted {
    started: bool,
}

/// Which firmware currently runs on the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Firmware {
    /// Shelly stock firmware.
    Stock {
        /// `/shelly` answer.
        info: StockInfo,
        /// Wi-Fi signal strength in dBm.
        rssi_dbm: i32,
    },
    /// The migration firmware of this project.
    Migration(MigrationStatus),
}

/// Client for one device.
#[derive(Debug, Clone)]
pub struct DeviceClient {
    base_url: String,
    http: reqwest::Client,
    credentials: Option<Credentials>,
}

impl DeviceClient {
    /// Creates a client for `host` (`10.10.5.78`, `name.local` or `host:port`).
    #[must_use]
    pub fn new(host: &str, credentials: Option<Credentials>) -> Self {
        // Without TLS backends, building the client cannot fail.
        let http = reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build().unwrap_or_default();
        Self { base_url: format!("http://{host}"), http, credentials }
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.get_url(format!("{}{path}", self.base_url))
    }

    fn get_url(&self, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
        let request = self.http.get(url);
        match &self.credentials {
            Some(credentials) => request.basic_auth(&credentials.user, Some(&credentials.password)),
            None => request,
        }
    }

    /// Reads a JSON endpoint of the stock firmware; `None` if it does not exist (404).
    ///
    /// # Errors
    /// Fails if the device is unreachable, needs a login, or answers with another status.
    pub async fn get_json(
        &self,
        endpoint: &'static str,
        path: &str,
    ) -> Result<Option<serde_json::Value>, DeviceError> {
        let response = self.get(path).send().await?;
        match response.status().as_u16() {
            200 => Ok(Some(response.json().await?)),
            404 => Ok(None),
            401 => Err(DeviceError::AuthRequired),
            status => Err(DeviceError::Status { endpoint, status }),
        }
    }

    /// Determines the running firmware.
    ///
    /// # Errors
    /// Fails if the device is unreachable, needs a login, or runs unknown firmware.
    pub async fn identify(&self) -> Result<Firmware, DeviceError> {
        let response = self.get("/shelly").send().await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(DeviceError::AuthRequired);
        }
        if response.status().is_success() {
            if let Ok(info) = response.json::<StockInfo>().await {
                let rssi_dbm = self.stock_rssi().await?;
                return Ok(Firmware::Stock { info, rssi_dbm });
            }
        }
        match self.migration_status().await? {
            Some(status) => Ok(Firmware::Migration(status)),
            None => Err(DeviceError::UnknownFirmware),
        }
    }

    async fn stock_rssi(&self) -> Result<i32, DeviceError> {
        let response = self.get("/status").send().await?;
        match response.status().as_u16() {
            200 => Ok(response.json::<StockStatusWire>().await?.wifi_sta.rssi),
            401 => Err(DeviceError::AuthRequired),
            status => Err(DeviceError::Status { endpoint: "/status", status }),
        }
    }

    /// Asks the stock firmware to install the OTA package at `package_url`.
    ///
    /// # Errors
    /// Fails if the device refuses the request.
    pub async fn start_stock_ota(&self, package_url: &str) -> Result<(), DeviceError> {
        let mut url = reqwest::Url::parse(&format!("{}/ota", self.base_url))
            .map_err(|error| DeviceError::Rejected(format!("invalid device address: {error}")))?;
        url.query_pairs_mut().append_pair("url", package_url);
        let response = self.get_url(url).send().await?;
        match response.status().as_u16() {
            200 => Ok(()),
            401 => Err(DeviceError::AuthRequired),
            status => Err(DeviceError::Status { endpoint: "/ota", status }),
        }
    }

    /// Reads `MG2X.Status`; `None` if the method does not exist.
    ///
    /// # Errors
    /// Fails if the device is unreachable or speaks another protocol.
    pub async fn migration_status(&self) -> Result<Option<MigrationStatus>, DeviceError> {
        let response = self.http.get(format!("{}/rpc/MG2X.Status", self.base_url)).send().await?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let Ok(status) = response.json::<MigrationStatus>().await else {
            return Ok(None);
        };
        if status.protocol != MIGRATION_PROTOCOL {
            return Err(DeviceError::UnsupportedProtocol(status.protocol));
        }
        Ok(Some(status))
    }

    /// Hands the flash job to the migration firmware.
    ///
    /// # Errors
    /// Fails if the device rejects the job (busy, size, hash format, ...).
    pub async fn start_migration(
        &self,
        image_url: &str,
        size_bytes: usize,
        sha256_hex: &str,
    ) -> Result<(), DeviceError> {
        let body =
            serde_json::json!({ "url": image_url, "size": size_bytes, "sha256": sha256_hex });
        let response =
            self.http.post(format!("{}/rpc/MG2X.Flash", self.base_url)).json(&body).send().await?;
        let status = response.status();
        let text = response.text().await?;
        if let Ok(error) = serde_json::from_str::<RpcError>(&text) {
            return Err(DeviceError::Rejected(error.message));
        }
        if !status.is_success() {
            return Err(DeviceError::Status {
                endpoint: "/rpc/MG2X.Flash",
                status: status.as_u16(),
            });
        }
        match serde_json::from_str::<FlashStarted>(&text) {
            Ok(FlashStarted { started: true }) => Ok(()),
            Ok(FlashStarted { started: false }) => {
                Err(DeviceError::Rejected("connect_failed".into()))
            }
            Err(error) => Err(DeviceError::Rejected(format!("unexpected answer: {error}"))),
        }
    }
}

#[cfg(test)]
#[path = "unit_tests/device.rs"]
mod tests;
