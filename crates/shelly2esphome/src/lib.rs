//! Flash a self-built ESPHome firmware onto Shelly Gen1 devices over the air.
//!
//! The stock firmware first installs the migration firmware of this repository
//! (`migration_firmware/`); the migration firmware then downloads the ESPHome
//! image from this computer, verifies size and SHA-256, and only then replaces
//! the bootloader and the application.

pub mod backup;
pub mod cli;
pub mod device;
pub mod discover;
pub mod embedded;
pub mod esp32;
pub mod fetch;
pub mod flash;
pub mod http;
pub mod image;
pub mod migration;
pub mod model;
pub mod protocol;
pub mod report;
pub mod rpc;
pub mod server;
pub mod shelly_tls;
pub mod stock_package;
