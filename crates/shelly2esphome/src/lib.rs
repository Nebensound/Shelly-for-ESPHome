//! Flash a self-built ESPHome firmware onto Shelly devices over the air.
//!
//! Gen1: the stock firmware first installs the migration firmware of this
//! repository (`migration_firmware/`); the migration firmware then downloads
//! the ESPHome image from this computer, verifies size and SHA-256, and only
//! then replaces the bootloader and the application. Gen2 and later: the
//! stock updater installs a package with ESPHome's application and
//! bootloader (`esphome_package`, `flash_esp32`).

pub mod backup;
pub mod cli;
pub mod device;
pub mod discover;
pub mod embedded;
pub mod esp32;
pub mod esphome_package;
pub mod fetch;
pub mod flash;
pub mod flash_esp32;
pub mod http;
pub mod image;
pub mod migration;
pub mod model;
pub mod profile;
pub mod protocol;
pub mod report;
pub mod rpc;
pub mod rpc_client;
pub mod server;
pub mod shelly_tls;
pub mod stock_package;
pub mod udp_log;
