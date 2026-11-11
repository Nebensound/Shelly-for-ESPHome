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

impl Generation {
    /// The chip family of this generation, `None` for generations this tool
    /// does not know yet.
    #[must_use]
    pub fn chip_family(self) -> Option<ChipFamily> {
        match self.0 {
            2 => Some(ChipFamily::Esp32),
            3 => Some(ChipFamily::Esp32C3),
            4 => Some(ChipFamily::Esp32C6),
            _ => None,
        }
    }
}

impl std::fmt::Display for Generation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Gen{}", self.0)
    }
}

/// Espressif chip family of a Shelly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipFamily {
    /// Classic ESP32 (Gen2); its bootloader starts at 0x1000.
    Esp32,
    /// ESP32-C3 (Gen3); its bootloader starts at 0x0.
    Esp32C3,
    /// ESP32-C6 (Gen4).
    Esp32C6,
}

impl std::fmt::Display for ChipFamily {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Esp32 => "ESP32",
            Self::Esp32C3 => "ESP32-C3",
            Self::Esp32C6 => "ESP32-C6",
        })
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

#[derive(Debug, Deserialize)]
pub(crate) struct WifiStatusWire {
    pub(crate) rssi: i32,
}

#[cfg(test)]
#[path = "unit_tests/rpc.rs"]
mod tests;
