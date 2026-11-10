//! The migration package (`mg2esphome-<Model>.zip`) built by CI with `mos`.

use std::borrow::Cow;
use std::io::{Cursor, Read};

use serde::Deserialize;

use crate::{
    model::Model,
    protocol::{self, MIGRATION_PROTOCOL},
};

/// Reasons why a file is not a usable migration package.
#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    /// The file is not a readable ZIP archive.
    #[error("the migration package is not a valid ZIP archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    /// The archive has no `manifest.json` in its root.
    #[error("the migration package has no manifest.json")]
    MissingManifest,
    /// `manifest.json` could not be read or parsed.
    #[error("manifest.json in the migration package is invalid: {0}")]
    Manifest(String),
    /// A binary inside the archive could not be read (e.g. corrupted data).
    #[error("a file in the migration package is corrupted: {0}")]
    ReadEntry(String),
    /// The package was built for another platform.
    #[error("the migration package is built for platform {0:?}, not esp8266")]
    WrongPlatform(String),
    /// The archive is a Shelly ZIP, but not this project's migration firmware
    /// (e.g. a stock firmware, mg2tasmota or an older auto-downloading build).
    #[error(
        "the ZIP is not a shelly2esphome migration package (stock firmware, mg2tasmota or an \
         older version?); use the current mg2esphome-<Model>.zip"
    )]
    NotAMigrationPackage,
    /// The migration firmware speaks another protocol version than this tool.
    #[error(
        "the migration package speaks protocol {0}, this tool protocol {MIGRATION_PROTOCOL}; use \
         package and tool from the same version"
    )]
    ProtocolMismatch(u32),
    /// The package was built for another model and would be refused by the device.
    #[error(
        "the migration package is built for {found:?}, the device expects {expected:?} (wrong \
         model?)"
    )]
    WrongModel {
        /// Application name the device expects.
        expected: &'static str,
        /// Application name found in the manifest.
        found: String,
    },
}

#[derive(Debug, Deserialize)]
struct Manifest {
    name: String,
    platform: String,
}

/// The decoded name of an archive entry.
///
/// `zip` decodes a name as UTF-8 and otherwise as CP437, which maps every
/// byte; its error only covers ASCII-only names that are not UTF-8, and there
/// are none. The error path cannot occur, so it gets no branch.
fn entry_name(name: zip::result::ZipResult<Cow<'_, str>>) -> Cow<'_, str> {
    name.unwrap_or_default()
}

/// Searches the binaries of the archive for the protocol marker and returns
/// the protocol number that follows it.
fn find_protocol_marker(
    archive: &mut zip::ZipArchive<Cursor<bytes::Bytes>>,
) -> Result<Option<u32>, PackageError> {
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if !entry_name(entry.name()).ends_with(".bin") {
            continue;
        }
        let mut content = Vec::new();
        entry
            .read_to_end(&mut content)
            .map_err(|error| PackageError::ReadEntry(error.to_string()))?;
        if let Some(protocol) = protocol::find_marker(&content) {
            return Ok(Some(protocol));
        }
    }
    Ok(None)
}

/// A migration package whose manifest has been inspected.
#[derive(Debug, Clone)]
pub struct MigrationPackage {
    bytes: bytes::Bytes,
    app_name: String,
}

impl MigrationPackage {
    /// Reads the manifest of a Mongoose OS firmware ZIP.
    ///
    /// # Errors
    /// Returns a [`PackageError`] if the archive or its manifest is unusable.
    pub fn parse(bytes: impl Into<bytes::Bytes>) -> Result<Self, PackageError> {
        let bytes = bytes.into();
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes.clone()))?;
        let manifest_index = (0..archive.len())
            .find(|&index| {
                archive
                    .name_for_index(index)
                    .map(entry_name)
                    .is_some_and(|name| name == "manifest.json" || name.ends_with("/manifest.json"))
            })
            .ok_or(PackageError::MissingManifest)?;
        let mut text = String::new();
        archive
            .by_index(manifest_index)?
            .read_to_string(&mut text)
            .map_err(|error| PackageError::Manifest(error.to_string()))?;
        let manifest: Manifest = serde_json::from_str(&text)
            .map_err(|error| PackageError::Manifest(error.to_string()))?;
        if manifest.platform != "esp8266" {
            return Err(PackageError::WrongPlatform(manifest.platform));
        }
        match find_protocol_marker(&mut archive)? {
            Some(MIGRATION_PROTOCOL) => Ok(Self { bytes, app_name: manifest.name }),
            Some(protocol) => Err(PackageError::ProtocolMismatch(protocol)),
            None => Err(PackageError::NotAMigrationPackage),
        }
    }

    /// Ensures the package is the one built for `model`.
    ///
    /// # Errors
    /// Returns [`PackageError::WrongModel`] if the application names differ.
    pub fn ensure_model(&self, model: Model) -> Result<(), PackageError> {
        if self.app_name == model.app_name() {
            Ok(())
        } else {
            Err(PackageError::WrongModel {
                expected: model.app_name(),
                found: self.app_name.clone(),
            })
        }
    }

    /// Raw ZIP bytes as served to the stock firmware.
    #[must_use]
    pub fn bytes(&self) -> &bytes::Bytes {
        &self.bytes
    }

    /// Application name from the manifest.
    #[must_use]
    pub fn app_name(&self) -> &str {
        &self.app_name
    }
}

#[cfg(test)]
#[path = "unit_tests/migration.rs"]
pub(crate) mod tests;
