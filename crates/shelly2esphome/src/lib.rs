//! Flash a self-built ESPHome firmware onto Shelly Gen1 devices over the air.
//!
//! The stock firmware first installs the migration firmware of this repository
//! (`migration_firmware/`); the migration firmware then downloads the ESPHome
//! image from this computer, verifies size and SHA-256, and only then replaces
//! the bootloader and the application.

pub mod device;
pub mod flash;
pub mod image;
pub mod migration;
pub mod model;
pub mod server;
