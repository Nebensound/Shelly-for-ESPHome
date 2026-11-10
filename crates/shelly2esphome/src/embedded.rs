//! Migration packages compiled into the binary by `build.rs`.
//!
//! Release builds embed the packages of all models, so `flash` finds the
//! right one by itself. `build.rs` lists where they come from; a build
//! without any packages needs `--migration`.

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
#[path = "unit_tests/embedded.rs"]
mod tests;
