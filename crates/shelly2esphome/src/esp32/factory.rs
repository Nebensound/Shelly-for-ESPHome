//! The merged factory image ESPHome builds (`firmware.factory.bin`), checked
//! against the official package of the device.
//!
//! The image starts at flash address 0. It must be built with the device's
//! stock partition table: the bootloader and the table sit at the addresses
//! the official package uses, and the application in `app_0`, the slot that
//! ESPHome's bootloader starts after the stock installer.

use super::{
    image::{Esp32Image, ImageError},
    partition::{PartitionError, PartitionTable},
};
use crate::stock_package::StockPackage;

/// Partition that holds the application in the factory image.
pub const APP_PARTITION: &str = "app_0";
const OTADATA_PARTITION: &str = "otadata";

/// Reasons why a factory image cannot be used for the device.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FactoryError {
    /// No valid bootloader at the package's bootloader address.
    #[error("no valid bootloader at {address:#x}: {source}")]
    Bootloader {
        /// Flash address.
        address: usize,
        /// Cause.
        source: ImageError,
    },
    /// An application sits where the bootloader belongs: not a factory image.
    #[error(
        "{address:#x} holds an application, not a bootloader: pass ESPHome's \
         firmware.factory.bin, not the OTA image"
    )]
    NotABootloader {
        /// Flash address.
        address: usize,
    },
    /// No valid partition table at the package's table address.
    #[error(
        "no valid partition table at {address:#x} ({source}); build ESPHome with the device's \
         stock partition table and table offset"
    )]
    PartitionTable {
        /// Flash address.
        address: usize,
        /// Cause.
        source: PartitionError,
    },
    /// The partition table lacks a partition this tool needs.
    #[error("the partition table has no {0} partition")]
    MissingPartition(&'static str),
    /// The otadata area holds data; the stock installer's boot state must not
    /// meet a leftover one.
    #[error("the otadata partition of the factory image is not erased")]
    OtadataNotErased,
    /// No valid application in `app_0`.
    #[error("no valid application in {APP_PARTITION}: {0}")]
    App(ImageError),
    /// The image in `app_0` is a bootloader, not an application.
    #[error("the image in {APP_PARTITION} is no application")]
    NotAnApp,
    /// The application does not fit `app_0`.
    #[error("the application has {size} bytes, {APP_PARTITION} holds {capacity}")]
    AppTooLarge {
        /// Application size.
        size: usize,
        /// Partition size.
        capacity: usize,
    },
    /// A critical partition differs from the device's.
    #[error("the partition table differs from the device's: {}", .0.join("; "))]
    TableMismatch(Vec<String>),
    /// The bootloader header does not suit the device.
    #[error("the bootloader {0} differs from the device's stock bootloader")]
    BootloaderMismatch(&'static str),
    /// The firmware was built for another chip.
    #[error("the firmware is built for {built}, the device has {device}")]
    ChipMismatch {
        /// Chip of the factory image.
        built: String,
        /// Chip of the official package.
        device: String,
    },
}

/// A factory image whose parts are located and verified.
#[derive(Debug, Clone)]
pub struct FactoryImage {
    bytes: Vec<u8>,
    bootloader_address: usize,
    bootloader: Esp32Image,
    table: PartitionTable,
    app_offset: usize,
    app: Esp32Image,
}

impl FactoryImage {
    /// Locates and verifies bootloader, partition table, otadata and
    /// application at the addresses of `package`, and checks that the image
    /// suits the device the package is for.
    ///
    /// Returns the image and warnings about differences that do not stop a
    /// migration (partitions outside [`super::partition::CRITICAL`]).
    ///
    /// # Errors
    /// Returns a [`FactoryError`] naming the first problem.
    pub fn check(
        bytes: Vec<u8>,
        package: &StockPackage,
    ) -> Result<(Self, Vec<String>), FactoryError> {
        let (bootloader_address, table_address) = package.addresses();
        let at = |address: usize| bytes.get(address..).unwrap_or_default();
        let bootloader = Esp32Image::parse(at(bootloader_address))
            .map_err(|source| FactoryError::Bootloader { address: bootloader_address, source })?;
        if bootloader.is_app {
            return Err(FactoryError::NotABootloader { address: bootloader_address });
        }
        let table = PartitionTable::parse(at(table_address))
            .map_err(|source| FactoryError::PartitionTable { address: table_address, source })?;
        let otadata = table
            .find(OTADATA_PARTITION)
            .ok_or(FactoryError::MissingPartition(OTADATA_PARTITION))?;
        let otadata_bytes = at(otadata.offset).iter().take(otadata.size);
        if otadata_bytes.into_iter().any(|byte| *byte != 0xFF) {
            return Err(FactoryError::OtadataNotErased);
        }
        let slot =
            table.find(APP_PARTITION).ok_or(FactoryError::MissingPartition(APP_PARTITION))?;
        let app = Esp32Image::parse(at(slot.offset)).map_err(FactoryError::App)?;
        if !app.is_app {
            return Err(FactoryError::NotAnApp);
        }
        if app.len > slot.size {
            return Err(FactoryError::AppTooLarge { size: app.len, capacity: slot.size });
        }
        let differences = table.differences(package.partition_table());
        if !differences.critical.is_empty() {
            return Err(FactoryError::TableMismatch(differences.critical));
        }
        let stock = package.bootloader();
        for (field, own, theirs) in [
            ("chip", u32::from(bootloader.chip.0), u32::from(stock.chip.0)),
            ("flash mode", bootloader.flash.mode.into(), stock.flash.mode.into()),
            (
                "flash size and frequency",
                bootloader.flash.size_and_frequency.into(),
                stock.flash.size_and_frequency.into(),
            ),
            (
                "minimum chip revision",
                bootloader.min_chip_revision.into(),
                stock.min_chip_revision.into(),
            ),
        ] {
            if own != theirs {
                return Err(FactoryError::BootloaderMismatch(field));
            }
        }
        if app.chip != package.app().chip {
            return Err(FactoryError::ChipMismatch {
                built: app.chip.to_string(),
                device: package.app().chip.to_string(),
            });
        }
        let app_offset = slot.offset;
        let image = Self { bytes, bootloader_address, bootloader, table, app_offset, app };
        Ok((image, differences.other))
    }

    /// The bootloader bytes.
    #[must_use]
    pub fn bootloader_bytes(&self) -> &[u8] {
        self.bytes
            .get(self.bootloader_address..self.bootloader_address + self.bootloader.len)
            .unwrap_or_default()
    }

    /// The application bytes.
    #[must_use]
    pub fn app_bytes(&self) -> &[u8] {
        self.bytes.get(self.app_offset..self.app_offset + self.app.len).unwrap_or_default()
    }

    /// The parsed application.
    #[must_use]
    pub fn app(&self) -> &Esp32Image {
        &self.app
    }

    /// The partition table of the factory image.
    #[must_use]
    pub fn partition_table(&self) -> &PartitionTable {
        &self.table
    }
}

#[cfg(test)]
#[path = "../unit_tests/esp32/factory.rs"]
mod tests;
