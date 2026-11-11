//! Formats of the ESP32 family (Shelly Gen2 and later): images, partition
//! tables and the merged factory image ESPHome builds.
//!
//! Everything here works on bytes only; nothing talks to a device.

pub mod factory;
pub mod image;
pub mod partition;
#[doc(hidden)]
pub mod synthetic;

/// Chip ID from an ESP32 image header (`esp_chip_id_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChipId(pub u16);

impl std::fmt::Display for ChipId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            0x0000 => formatter.write_str("ESP32"),
            0x0005 => formatter.write_str("ESP32-C3"),
            0x000D => formatter.write_str("ESP32-C6"),
            other => write!(formatter, "chip ID {other:#06x}"),
        }
    }
}
