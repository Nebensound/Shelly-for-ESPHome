//! Unit tests of `crate::profile` (not counted as production code for coverage).

use super::*;
use crate::esp32::synthetic::{Fixture, ImageSpec, LAYOUT, image, table, zip_package};

fn package(manifest: &serde_json::Value, fixture: &Fixture) -> StockPackage {
    StockPackage::parse(&zip_package(manifest, &fixture.files())).expect("package")
}

#[test]
fn reads_and_orders_bootloader_versions() {
    let version: BootloaderVersion = "1.0.2".parse().expect("version");
    assert_eq!(version.to_string(), "1.0.2");
    assert!(version < BOOT_MIN_VERSION);
    assert!("1.0.10".parse::<BootloaderVersion>().expect("version") > BOOT_MIN_VERSION);
    assert!("1.1.0".parse::<BootloaderVersion>().expect("version") > BOOT_MIN_VERSION);
    assert_eq!(BOOT_MIN_VERSION.to_string(), "1.0.9");
    for text in ["1.0", "1.0.9.1", "1.0.x", "1.0.256", "", "v1.0.2"] {
        assert_eq!(
            text.parse::<BootloaderVersion>(),
            Err(format!("{text:?} is no bootloader version"))
        );
    }
}

#[test]
fn names_every_app_once() {
    let mut apps: Vec<&str> = PROFILES.iter().map(Profile::app).collect();
    apps.sort_unstable();
    apps.dedup();
    assert_eq!(apps.len(), PROFILES.len());
}

#[test]
fn finds_the_profile_of_a_package_with_the_known_layout() {
    let fixture = Fixture::default();
    let profile = Profile::of(&package(&fixture.manifest(), &fixture)).expect("profile");
    assert_eq!(profile.app(), "S1G3");
}

#[test]
fn refuses_an_app_without_profile() {
    let fixture = Fixture::default();
    let mut manifest = fixture.manifest();
    manifest["name"] = "S1G4".into();
    assert_eq!(
        Profile::of(&package(&manifest, &fixture)).expect_err("unknown").to_string(),
        "S1G4 has no profile: its flash layout and bootloader have not been checked for a \
         migration yet"
    );
}

#[test]
fn refuses_another_layout() {
    // The layout before 2.2.0: smaller app slots; and the table elsewhere.
    let mut layout = LAYOUT.to_vec();
    layout[2].4 = 0x2B_0000;
    let esp32 = image(ImageSpec { chip: 0, app: true, payload: 4096, ..ImageSpec::default() });
    let fixture = Fixture { stock_table: table(&layout), stock_app: esp32, ..Fixture::default() };
    let mut manifest = fixture.manifest();
    manifest["version"] = "2.1.1".into();
    manifest["parts"]["pt"]["addr"] = 0x1_0000.into();
    manifest["parts"]["app"]["ptn"] = "app_1".into();
    let error = Profile::of(&package(&manifest, &fixture)).expect_err("layout");
    assert_eq!(
        error.to_string(),
        "the official package S1G3 2.1.1 has another flash layout than S1G3 2.2.0, the one this \
         tool knows (chip ESP32, expected ESP32-C3; partition table at 0x10000, expected 0xf000; \
         app_0 of 0x2b0000 bytes, expected 0x300000; the application goes to app_1, expected \
         app_0); Shelly changes the layout between releases, and a new one needs a new profile"
    );

    let without_slot = LAYOUT.iter().filter(|entry| entry.0 != "app_0").copied();
    let fixture = Fixture { stock_table: table(&without_slot.collect::<Vec<_>>()), ..fixture };
    let error = Profile::of(&package(&fixture.manifest(), &fixture)).expect_err("no slot");
    assert!(error.to_string().contains("(chip ESP32, expected ESP32-C3; no app_0)"), "{error}");
}
