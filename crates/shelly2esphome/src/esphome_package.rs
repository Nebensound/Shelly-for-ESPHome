//! The package that installs ESPHome on a Gen2/Gen3 device through its stock
//! updater: the official package of the device with ESPHome's application
//! and bootloader and an erased otadata.
//!
//! The stock bootloader keeps its own boot state and starts the stock app
//! again after three starts, so ESPHome only stays with its own bootloader.
//! The installer writes a bootloader only if the package's `min_version` is
//! higher than the installed one, so it is raised to
//! [`BOOT_MIN_VERSION`]. The partition table and every other part
//! stay as shipped, and so does the order of the parts in the manifest.
//!
//! The package contains the user's Wi-Fi credentials (they are in the
//! ESPHome application). It is only built in memory; nothing here writes a
//! file.

use std::io::Write as _;

use sha2::{Digest, Sha256};

use crate::{
    esp32::factory::{FactoryError, FactoryImage},
    profile::{BOOT_MIN_VERSION, BootloaderVersion, Profile, ProfileError},
    stock_package::{StockPackage, StockPackageError},
};

const OTADATA: &str = "otadata";

/// Reasons why no ESPHome package is built.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// The official package has no profile or another layout.
    #[error(transparent)]
    Profile(#[from] ProfileError),
    /// The factory image does not suit the device.
    #[error(transparent)]
    Factory(#[from] FactoryError),
    /// The official package carries no otadata file to replace.
    #[error("the official package has no otadata part with a file")]
    NoOtadata,
    /// The official bootloader's `min_version` is missing, unreadable or not
    /// below [`BOOT_MIN_VERSION`], so the installer might keep Shelly's
    /// bootloader.
    #[error(
        "the bootloader of the official package has min_version {}, which is not below \
         {BOOT_MIN_VERSION}; the installer might not write ESPHome's bootloader",
        .0.as_deref().unwrap_or("none")
    )]
    BootVersion(Option<String>),
    /// The ZIP cannot be written.
    #[error("the package cannot be written: {0}")]
    Zip(#[from] zip::result::ZipError),
    /// The built package does not pass the checks of an official one.
    #[error("the built package is unusable: {0}")]
    Verify(StockPackageError),
}

/// An ESPHome package, verified like an official one.
#[derive(Debug, Clone)]
pub struct EsphomePackage {
    bytes: Vec<u8>,
    profile: &'static Profile,
    factory: FactoryImage,
    official_boot_version: BootloaderVersion,
}

/// `part`'s `size` and `cs_sha256` for `data`.
fn describe(manifest: &mut serde_json::Value, part: &str, data: &[u8]) {
    // `StockPackage::parse` read every part as an object, so indexing inserts
    // the fields instead of panicking.
    manifest["parts"][part]["size"] = data.len().into();
    manifest["parts"][part]["cs_sha256"] = hex::encode(Sha256::digest(data)).into();
}

impl EsphomePackage {
    /// Builds the package from the official package `stock` and the ESPHome
    /// `factory` image (`firmware.factory.bin`), after the profile check and
    /// every check of [`FactoryImage::check`]. Returns the package and the
    /// warnings of the factory check.
    ///
    /// # Errors
    /// A [`BuildError`] naming the first problem.
    pub fn build(
        stock: &StockPackage,
        factory: Vec<u8>,
    ) -> Result<(Self, Vec<String>), BuildError> {
        let profile = Profile::of(stock)?;
        let (factory, warnings) = FactoryImage::check(factory, stock)?;
        let official = stock.part("boot").and_then(|part| part.min_version.clone());
        let official_boot_version = official
            .as_deref()
            .and_then(|version| version.parse::<BootloaderVersion>().ok())
            .filter(|version| *version < BOOT_MIN_VERSION)
            .ok_or(BuildError::BootVersion(official))?;
        if stock.part(OTADATA).and_then(|part| part.src.as_ref()).is_none() {
            return Err(BuildError::NoOtadata);
        }
        // The installer writes otadata only as far as the file reaches, so
        // the erased one covers the whole partition. The factory check found
        // the partition in the package's table.
        let otadata_size = stock.partition_table().find(OTADATA).map_or(0, |otadata| otadata.size);
        let erased = vec![0xFF; otadata_size];

        let mut manifest = stock.raw_manifest().clone();
        describe(&mut manifest, "app", factory.app_bytes());
        describe(&mut manifest, "boot", factory.bootloader_bytes());
        describe(&mut manifest, OTADATA, &erased);
        manifest["parts"]["boot"]["min_version"] = BOOT_MIN_VERSION.to_string().into();

        let manifest_text = manifest.to_string();
        let mut entries = vec![("manifest.json", manifest_text.as_bytes())];
        for (name, part) in manifest["parts"].as_object().into_iter().flatten() {
            let Some(src) = part["src"].as_str() else { continue };
            let data = match name.as_str() {
                "app" => factory.app_bytes(),
                "boot" => factory.bootloader_bytes(),
                OTADATA => &erased,
                _ => stock.file(name).unwrap_or_default(),
            };
            entries.push((src, data));
        }
        // Uncompressed, like the official packages. Writing to memory only
        // fails for a file name used twice.
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let bytes = entries
            .iter()
            .try_for_each(|(name, data)| {
                writer.start_file(*name, options)?;
                writer.write_all(data).map_err(zip::result::ZipError::Io)
            })
            .and_then(|()| writer.finish())?
            .into_inner();
        StockPackage::parse(&bytes).map_err(BuildError::Verify)?;
        let package = Self { bytes, profile, factory, official_boot_version };
        Ok((package, warnings))
    }

    /// The ZIP.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The profile the official package matched.
    #[must_use]
    pub fn profile(&self) -> &'static Profile {
        self.profile
    }

    /// The checked factory image the package was built from.
    #[must_use]
    pub fn factory(&self) -> &FactoryImage {
        &self.factory
    }

    /// The bootloader `min_version` of the official package, raised to
    /// [`BOOT_MIN_VERSION`] in this one.
    #[must_use]
    pub fn official_boot_version(&self) -> BootloaderVersion {
        self.official_boot_version
    }
}

#[cfg(test)]
#[path = "unit_tests/esphome_package.rs"]
mod tests;
