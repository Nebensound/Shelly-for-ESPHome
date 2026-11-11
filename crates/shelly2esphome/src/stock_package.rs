//! The official OTA package of a Shelly Gen2+ stock firmware.
//!
//! A ZIP with `manifest.json` and one file per part. Each part names its file
//! (`src`), size and SHA-256 (`cs_sha256`) and either a flash address
//! (`addr`) or a partition (`ptn`). The parts used here are `boot` (the
//! bootloader, with the `min_version` the installer compares), `pt` (the
//! partition table) and `app`. Fields this tool does not know are kept.

use std::{collections::BTreeMap, io::Read};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    esp32::{
        image::{Esp32Image, ImageError},
        partition::{PartitionError, PartitionTable},
    },
    rpc::{AppName, FirmwareVersion},
};

/// Largest package this tool reads, packed and unpacked (the largest Gen2+
/// flash is 16 MB).
pub const MAX_PACKAGE_BYTES: usize = 16 * 1024 * 1024;

/// One part as the manifest describes it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Part {
    /// File in the ZIP; `None` for parts that are only filled.
    pub src: Option<String>,
    /// Flash address.
    pub addr: Option<usize>,
    /// Target partition.
    pub ptn: Option<String>,
    /// Size of the file in bytes.
    pub size: Option<usize>,
    /// SHA-256 of the file, lowercase hex.
    pub cs_sha256: Option<String>,
    /// For the bootloader: written only if newer than the installed one.
    pub min_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    name: AppName,
    version: FirmwareVersion,
    platform: Option<String>,
    parts: BTreeMap<String, Part>,
}

/// Reasons why a file is no usable official package.
#[derive(Debug, thiserror::Error)]
pub enum StockPackageError {
    /// The file is larger than any official package.
    #[error("the package is larger than {MAX_PACKAGE_BYTES} bytes (packed or unpacked)")]
    TooLarge,
    /// The file is no readable ZIP.
    #[error("the package is no readable ZIP: {0}")]
    Zip(#[from] zip::result::ZipError),
    /// A file in the ZIP cannot be read.
    #[error("{name} in the package cannot be read: {reason}")]
    Read {
        /// File name.
        name: String,
        /// Cause.
        reason: String,
    },
    /// `manifest.json` is not the expected JSON.
    #[error("manifest.json is unusable: {0}")]
    Manifest(String),
    /// A part this tool needs is missing or incomplete.
    #[error("the manifest has no usable {0} part (src and addr or ptn)")]
    MissingPart(&'static str),
    /// A part's file does not match the manifest.
    #[error("part {part} does not match the manifest ({what}); the package is damaged")]
    PartMismatch {
        /// Part name.
        part: String,
        /// `size` or `sha256`.
        what: &'static str,
    },
    /// The bootloader or app of the package is no valid image.
    #[error("the {part} of the package: {source}")]
    Image {
        /// `bootloader` or `app`.
        part: &'static str,
        /// Cause.
        source: ImageError,
    },
    /// A part holds the wrong kind of image (an application as bootloader
    /// or the other way round).
    #[error("the {0} part of the package holds the wrong kind of image")]
    WrongKind(&'static str),
    /// The partition table of the package is unusable.
    #[error("the partition table of the package: {0}")]
    PartitionTable(#[from] PartitionError),
}

/// A verified official package.
#[derive(Debug, Clone)]
pub struct StockPackage {
    name: AppName,
    version: FirmwareVersion,
    platform: Option<String>,
    parts: BTreeMap<String, Part>,
    files: BTreeMap<String, Vec<u8>>,
    raw_manifest: serde_json::Value,
    bootloader: Esp32Image,
    table: PartitionTable,
    app: Esp32Image,
}

/// Reads `name`, unpacking at most `budget` bytes; every file read counts
/// against the same budget, so a highly compressed package cannot exhaust
/// memory.
fn read_file(
    archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    name: &str,
    budget: &mut usize,
) -> Result<Vec<u8>, StockPackageError> {
    let error = |reason: String| StockPackageError::Read { name: name.to_owned(), reason };
    let file = archive.by_name(name).map_err(|reason| error(reason.to_string()))?;
    let mut data = Vec::new();
    let limit = u64::try_from(*budget).unwrap_or(u64::MAX).saturating_add(1);
    file.take(limit).read_to_end(&mut data).map_err(|reason| error(reason.to_string()))?;
    *budget = budget.checked_sub(data.len()).ok_or(StockPackageError::TooLarge)?;
    Ok(data)
}

impl StockPackage {
    /// Reads and verifies an official package: every part's size and
    /// SHA-256, the bootloader and app images and the partition table.
    ///
    /// # Errors
    /// Returns a [`StockPackageError`] naming the first problem.
    pub fn parse(bytes: &[u8]) -> Result<Self, StockPackageError> {
        if bytes.len() > MAX_PACKAGE_BYTES {
            return Err(StockPackageError::TooLarge);
        }
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        let mut budget = MAX_PACKAGE_BYTES;
        let manifest_bytes = read_file(&mut archive, "manifest.json", &mut budget)?;
        let raw_manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)
            .map_err(|error| StockPackageError::Manifest(error.to_string()))?;
        let manifest: Manifest = serde_json::from_value(raw_manifest.clone())
            .map_err(|error| StockPackageError::Manifest(error.to_string()))?;
        let mut files = BTreeMap::new();
        for (name, part) in &manifest.parts {
            let Some(src) = &part.src else { continue };
            let data = read_file(&mut archive, src, &mut budget)?;
            let mismatch = |what| StockPackageError::PartMismatch { part: name.clone(), what };
            if part.size != Some(data.len()) {
                return Err(mismatch("size"));
            }
            if part.cs_sha256.as_deref() != Some(hex::encode(Sha256::digest(&data)).as_str()) {
                return Err(mismatch("sha256"));
            }
            files.insert(src.clone(), data);
        }
        let file_of = |part: &'static str, located: fn(&Part) -> bool| {
            manifest
                .parts
                .get(part)
                .filter(|entry| located(entry))
                .and_then(|entry| entry.src.as_ref())
                .and_then(|src| files.get(src))
                .ok_or(StockPackageError::MissingPart(part))
        };
        let bootloader = Esp32Image::parse(file_of("boot", |part| part.addr.is_some())?)
            .map_err(|source| StockPackageError::Image { part: "bootloader", source })?;
        if bootloader.is_app {
            return Err(StockPackageError::WrongKind("boot"));
        }
        let table = PartitionTable::parse(file_of("pt", |part| part.addr.is_some())?)?;
        let app = Esp32Image::parse(file_of("app", |part| part.ptn.is_some())?)
            .map_err(|source| StockPackageError::Image { part: "app", source })?;
        if !app.is_app {
            return Err(StockPackageError::WrongKind("app"));
        }
        Ok(Self {
            name: manifest.name,
            version: manifest.version,
            platform: manifest.platform,
            parts: manifest.parts,
            files,
            raw_manifest,
            bootloader,
            table,
            app,
        })
    }

    /// Application name the package is for, e.g. `S1G3`.
    #[must_use]
    pub fn name(&self) -> &AppName {
        &self.name
    }

    /// Stock firmware version of the package.
    #[must_use]
    pub fn version(&self) -> &FirmwareVersion {
        &self.version
    }

    /// Platform from the manifest, e.g. `esp32c3`.
    #[must_use]
    pub fn platform(&self) -> Option<&str> {
        self.platform.as_deref()
    }

    /// The manifest entry of `part`.
    #[must_use]
    pub fn part(&self, part: &str) -> Option<&Part> {
        self.parts.get(part)
    }

    /// The file of `part`, if it has one.
    #[must_use]
    pub fn file(&self, part: &str) -> Option<&[u8]> {
        self.part(part)?.src.as_ref().and_then(|src| self.files.get(src)).map(Vec::as_slice)
    }

    /// The manifest as read, unknown fields included.
    #[must_use]
    pub fn raw_manifest(&self) -> &serde_json::Value {
        &self.raw_manifest
    }

    /// The package's bootloader.
    #[must_use]
    pub fn bootloader(&self) -> &Esp32Image {
        &self.bootloader
    }

    /// Flash address of the bootloader and of the partition table.
    #[must_use]
    pub fn addresses(&self) -> (usize, usize) {
        let address = |part| self.part(part).and_then(|part| part.addr).unwrap_or_default();
        (address("boot"), address("pt"))
    }

    /// The package's partition table, which is the device's after the update.
    #[must_use]
    pub fn partition_table(&self) -> &PartitionTable {
        &self.table
    }

    /// The package's application.
    #[must_use]
    pub fn app(&self) -> &Esp32Image {
        &self.app
    }
}

#[cfg(test)]
#[path = "unit_tests/stock_package.rs"]
mod tests;
