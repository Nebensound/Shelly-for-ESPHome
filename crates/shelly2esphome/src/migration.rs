//! The migration package (`mg2esphome-<Model>.zip`) built by CI with `mos`.

use std::io::{Cursor, Read};

use serde::Deserialize;

use crate::model::Model;

/// Reasons why a file is not a usable migration package.
#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    /// The file is not a readable ZIP archive.
    #[error("das Migrationspaket ist kein gültiges ZIP-Archiv: {0}")]
    Zip(#[from] zip::result::ZipError),
    /// The archive has no `manifest.json` in its root.
    #[error("im Migrationspaket fehlt manifest.json")]
    MissingManifest,
    /// `manifest.json` could not be read or parsed.
    #[error("manifest.json im Migrationspaket ist ungültig: {0}")]
    Manifest(String),
    /// The package was built for another platform.
    #[error("das Migrationspaket ist für die Plattform {0:?} gebaut, nicht für esp8266")]
    WrongPlatform(String),
    /// The package was built for another model and would be refused by the device.
    #[error(
        "das Migrationspaket ist für {found:?} gebaut, das Gerät erwartet {expected:?} \
         (falsches Modell gewählt?)"
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
        Ok(Self { bytes, app_name: manifest.name })
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
pub(crate) mod tests {
    use std::io::Write;

    use super::*;

    pub(crate) fn package_zip(files: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, content) in files {
            writer.start_file(*name, options).expect("start file");
            writer.write_all(content.as_bytes()).expect("write file");
        }
        writer.finish().expect("finish zip").into_inner()
    }

    pub(crate) fn package_for(model: Model) -> Vec<u8> {
        let manifest = format!(r#"{{"name":"{}","platform":"esp8266"}}"#, model.app_name());
        package_zip(&[("fw/manifest.json", &manifest), ("fw/fw.bin", "x")])
    }

    #[test]
    fn accepts_the_package_of_the_right_model() {
        let package = MigrationPackage::parse(package_for(Model::ShellyDimmer2)).expect("package");
        assert_eq!(package.app_name(), "dimmer-l51");
        assert!(package.ensure_model(Model::ShellyDimmer2).is_ok());
        assert!(!package.bytes().is_empty());
    }

    #[test]
    fn root_level_manifest_is_found() {
        let zip = package_zip(&[("manifest.json", r#"{"name":"switch1pm","platform":"esp8266"}"#)]);
        let package = MigrationPackage::parse(zip).expect("package");
        assert!(package.ensure_model(Model::Shelly1Pm).is_ok());
    }

    #[test]
    fn rejects_the_package_of_another_model() {
        let package = MigrationPackage::parse(package_for(Model::Shelly1Pm)).expect("package");
        let error = package.ensure_model(Model::ShellyDimmer2).expect_err("wrong model");
        assert!(matches!(error, PackageError::WrongModel { expected: "dimmer-l51", .. }));
        assert!(error.to_string().contains("falsches Modell"));
    }

    #[test]
    fn rejects_broken_packages() {
        assert!(matches!(MigrationPackage::parse(b"no zip".to_vec()), Err(PackageError::Zip(_))));
        let no_manifest = package_zip(&[("fw.bin", "x")]);
        assert!(matches!(MigrationPackage::parse(no_manifest), Err(PackageError::MissingManifest)));
        let bad_json = package_zip(&[("manifest.json", "{")]);
        assert!(matches!(MigrationPackage::parse(bad_json), Err(PackageError::Manifest(_))));
        let esp32 = package_zip(&[("manifest.json", r#"{"name":"x","platform":"esp32"}"#)]);
        let error = MigrationPackage::parse(esp32).expect_err("esp32");
        assert!(matches!(error, PackageError::WrongPlatform(ref p) if p == "esp32"));
        assert!(error.to_string().contains("esp32"));
        assert!(PackageError::MissingManifest.to_string().contains("manifest.json"));
    }
}
