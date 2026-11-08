//! HTTP access to a Shelly Gen1 device, both in stock and in migration firmware.

use std::time::Duration;

use serde::Deserialize;

/// Protocol version of the `MG2X.*` RPC methods this tool speaks.
pub const MIGRATION_PROTOCOL: u32 = 1;
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
            _ => Err("erwartet benutzer:passwort".to_owned()),
        }
    }
}

/// Errors while talking to the device.
#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    /// The device could not be reached or answered with an unreadable body.
    #[error("Gerät nicht erreichbar oder Antwort unlesbar: {0}")]
    Http(#[from] reqwest::Error),
    /// The device requires a login that was not given or is wrong.
    #[error("das Gerät verlangt eine Anmeldung (--auth benutzer:passwort)")]
    AuthRequired,
    /// An endpoint answered with an unexpected HTTP status.
    #[error("{endpoint} antwortete mit HTTP {status}")]
    Status {
        /// Path of the endpoint.
        endpoint: &'static str,
        /// HTTP status code.
        status: u16,
    },
    /// The migration firmware refused the request.
    #[error("die Migrationsfirmware lehnt ab: {0}")]
    Rejected(String),
    /// Neither the stock nor the migration firmware answered.
    #[error("unbekannte Firmware: weder Shelly-Originalfirmware noch Migrationsfirmware")]
    UnknownFirmware,
    /// The migration firmware speaks another protocol version.
    #[error(
        "die Migrationsfirmware spricht Protokoll {0}, dieses Werkzeug Protokoll {MIGRATION_PROTOCOL}"
    )]
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
    ///
    /// # Errors
    /// Fails if the HTTP client cannot be constructed.
    pub fn new(host: &str, credentials: Option<Credentials>) -> Result<Self, DeviceError> {
        let http = reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build()?;
        Ok(Self { base_url: format!("http://{host}"), http, credentials })
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

    /// Determines the running firmware.
    ///
    /// # Errors
    /// Fails if the device is unreachable, needs a login, or runs unknown firmware.
    pub async fn identify(&self) -> Result<Firmware, DeviceError> {
        let response = self.get("/shelly").send().await?;
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
            .map_err(|error| DeviceError::Rejected(format!("ungültige Geräteadresse: {error}")))?;
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
            Err(error) => Err(DeviceError::Rejected(format!("unerwartete Antwort: {error}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_parse_and_hide_the_password() {
        let credentials: Credentials = "admin:geheim:mit:doppelpunkt".parse().expect("valid");
        assert_eq!(credentials.user, "admin");
        assert_eq!(credentials.password, "geheim:mit:doppelpunkt");
        assert!(!format!("{credentials:?}").contains("geheim"));
        assert!("ohne-doppelpunkt".parse::<Credentials>().is_err());
        assert!(":nur-passwort".parse::<Credentials>().is_err());
    }

    #[test]
    fn migration_status_parses_the_firmware_answer() {
        let status: MigrationStatus = serde_json::from_str(
            r#"{"protocol":1,"state":"failed","received":5,"size":10,"max_size":20,"error":"hash_mismatch"}"#,
        )
        .expect("valid");
        assert_eq!(status.state, MigrationState::Failed);
        assert_eq!(status.error, "hash_mismatch");
    }

    #[test]
    fn error_messages_are_german() {
        assert!(DeviceError::AuthRequired.to_string().contains("Anmeldung"));
        assert!(DeviceError::UnknownFirmware.to_string().contains("unbekannte Firmware"));
        assert!(DeviceError::UnsupportedProtocol(2).to_string().contains("Protokoll 2"));
        assert!(DeviceError::Rejected("busy".into()).to_string().contains("busy"));
        assert!(
            DeviceError::Status { endpoint: "/ota", status: 500 }.to_string().contains("HTTP 500")
        );
    }
}
