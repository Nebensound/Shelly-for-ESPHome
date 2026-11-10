//! Unit tests of `crate::embedded` (not counted as production code for coverage).

use super::*;

static PACKAGES_FOR_TEST: Packages =
    Packages(&[("mg2esphome-Shelly1PM.zip", b"1pm"), ("mg2esphome-ShellyDimmer2.zip", b"dimmer")]);

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
