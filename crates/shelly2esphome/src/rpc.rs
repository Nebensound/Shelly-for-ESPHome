//! What a Shelly of generation 2 or later reports about itself.
//!
//! These devices answer `/shelly` (the same data as the RPC method
//! `Shelly.GetDeviceInfo`) without a login, also when web authentication is
//! enabled. Only reading requests are sent; migrating them is not supported
//! yet.

use serde::Deserialize;

/// Generation of a Shelly that speaks the RPC protocol (2 or later).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "u8")]
pub struct Generation(u8);

impl TryFrom<u8> for Generation {
    type Error = String;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value >= 2 {
            Ok(Self(value))
        } else {
            Err(format!("generation {value} does not speak the RPC protocol"))
        }
    }
}

impl std::fmt::Display for Generation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Gen{}", self.0)
    }
}

/// Stock firmware version, e.g. `2.0.3` or `2.2.0-beta2`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct FirmwareVersion(String);

impl FirmwareVersion {
    /// Whether this is a beta release. Betas can change the flash layout
    /// (2.2.0-beta2 does for many Gen3 models), so a migration must not assume
    /// the layout of the stable release.
    #[must_use]
    pub fn is_beta(&self) -> bool {
        self.0.contains("-beta")
    }
}

/// Model ID of a Gen2+ Shelly, e.g. `S3SW-001X16EU`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct ModelId(String);

/// Application name of a Gen2+ stock firmware, e.g. `S1G3`; it names the
/// official update package of the model.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct AppName(String);

macro_rules! text_type {
    ($($name:ident),*) => {$(
        impl $name {
            /// The text as reported by the device.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    )*};
}

text_type!(ModelId, AppName, FirmwareVersion);

/// Answer of `/shelly` from a Shelly of generation 2 or later.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DeviceInfo {
    /// Device ID, e.g. `shelly1g3-dcda0cb2e7f4`.
    pub id: String,
    /// MAC address without separators.
    pub mac: String,
    /// Model ID.
    pub model: ModelId,
    /// Generation.
    #[serde(rename = "gen")]
    pub generation: Generation,
    /// Stock firmware version.
    #[serde(rename = "ver")]
    pub firmware: FirmwareVersion,
    /// Application name of the stock firmware.
    pub app: AppName,
    /// Whether web authentication is enabled.
    #[serde(rename = "auth_en", default)]
    pub auth: bool,
    /// App slot the stock firmware runs from; the stock updater writes to the
    /// other one.
    #[serde(default)]
    pub slot: Option<u8>,
}

/// Wi-Fi signal of a Gen2+ device as far as it can be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiSignal {
    /// Connected; signal strength in dBm.
    Dbm(i32),
    /// Not connected over Wi-Fi, e.g. a Pro device on Ethernet.
    NotConnected,
    /// The device requires a login to report it.
    LoginRequired,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WifiStatusWire {
    status: String,
    #[serde(default)]
    rssi: Option<i32>,
}

impl WifiStatusWire {
    /// The signal; `rssi` only means something while connected (it reads 0
    /// otherwise, or is missing).
    pub(crate) fn signal(&self) -> WifiSignal {
        match (self.status.as_str(), self.rssi) {
            ("connected" | "got ip", Some(rssi)) => WifiSignal::Dbm(rssi),
            _ => WifiSignal::NotConnected,
        }
    }
}

#[cfg(test)]
#[path = "unit_tests/rpc.rs"]
mod tests;
