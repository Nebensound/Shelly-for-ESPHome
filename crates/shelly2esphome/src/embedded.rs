//! Migration packages compiled into the binary by `build.rs`.
//!
//! CI release builds embed the packages of all models, so `flash` finds the
//! right one by itself. Builds without `SHELLY2ESPHOME_PACKAGES_DIR` contain
//! none and need `--migration`.

use std::fmt;

use crate::model::Model;

/// File name (`mg2esphome-<Model>.zip`) and content of an embedded package.
pub type EmbeddedPackage = (&'static str, &'static [u8]);

include!(concat!(env!("OUT_DIR"), "/embedded_packages.rs"));

/// A set of embedded migration packages.
#[derive(Clone, Copy)]
pub struct Packages(pub &'static [EmbeddedPackage]);

impl Packages {
    /// The packages compiled into this binary.
    pub const BUILT_IN: Self = Self(PACKAGES);

    /// The package for `model`, if there is one.
    #[must_use]
    pub fn find(self, model: Model) -> Option<&'static [u8]> {
        let name = model.migration_asset_name();
        self.0.iter().find(|(file, _)| *file == name).map(|(_, bytes)| *bytes)
    }
}

impl fmt::Debug for Packages {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Only the names: the contents are several megabytes.
        formatter.debug_list().entries(self.0.iter().map(|(name, _)| name)).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static PACKAGES_FOR_TEST: Packages = Packages(&[
        ("mg2esphome-Shelly1PM.zip", b"1pm"),
        ("mg2esphome-ShellyDimmer2.zip", b"dimmer"),
    ]);

    #[test]
    fn finds_the_package_of_the_model() {
        assert_eq!(PACKAGES_FOR_TEST.find(Model::ShellyDimmer2), Some(&b"dimmer"[..]));
        assert_eq!(PACKAGES_FOR_TEST.find(Model::Shelly1Pm), Some(&b"1pm"[..]));
        assert_eq!(PACKAGES_FOR_TEST.find(Model::Shelly25), None);
    }

    #[test]
    fn debug_lists_only_the_names() {
        assert_eq!(
            format!("{PACKAGES_FOR_TEST:?}"),
            r#"["mg2esphome-Shelly1PM.zip", "mg2esphome-ShellyDimmer2.zip"]"#
        );
    }
}
