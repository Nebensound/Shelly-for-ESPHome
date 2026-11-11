//! Unit tests of `crate::esphome_package` (not counted as production code for coverage).

use std::io::Read as _;

use super::*;
use crate::{
    esp32::synthetic::{Fixture, ImageSpec, LAYOUT, OTADATA, image, table, zip_package},
    stock_package::MAX_PACKAGE_BYTES,
};

fn stock(manifest: &serde_json::Value, files: &[(&str, Vec<u8>)]) -> StockPackage {
    StockPackage::parse(&zip_package(manifest, files)).expect("official package")
}

fn build(fixture: &Fixture) -> Result<(EsphomePackage, Vec<String>), BuildError> {
    EsphomePackage::build(&stock(&fixture.manifest(), &fixture.files()), fixture.factory())
}

fn refusal(manifest: &serde_json::Value, fixture: &Fixture) -> String {
    let stock = stock(manifest, &fixture.files());
    EsphomePackage::build(&stock, fixture.factory()).expect_err("must fail").to_string()
}

#[test]
fn replaces_app_bootloader_and_otadata_and_keeps_the_rest() {
    let fixture = Fixture::default();
    let official = stock(&fixture.manifest(), &fixture.files());
    let (package, warnings) = build(&fixture).expect("package");
    assert!(warnings.is_empty());
    assert_eq!(package.profile().app(), "S1G3");
    assert_eq!(package.official_boot_version().to_string(), "1.0.2");
    assert_eq!(package.factory().app_bytes(), fixture.app.as_slice());

    // The built package passes the checks of an official one: every size and
    // SHA-256 in the manifest matches its file.
    let built = StockPackage::parse(package.bytes()).expect("built package");
    assert_eq!(built.file("app"), Some(fixture.app.as_slice()));
    assert_eq!(built.file("boot"), Some(fixture.bootloader.as_slice()));
    assert_eq!(built.file("otadata"), Some(vec![0xFF; OTADATA.1].as_slice()));
    for part in ["pt", "fs"] {
        assert_eq!(built.file(part), official.file(part), "{part}");
    }
    assert_eq!(built.part("boot").and_then(|part| part.min_version.as_deref()), Some("1.0.9"));
    // The factory image still suits the package it went into.
    FactoryImage::check(fixture.factory(), &built).expect("offline validation");

    // Everything else in the manifest is as shipped, in the same order.
    let mut expected = official.raw_manifest().clone();
    for part in ["app", "boot", "otadata"] {
        expected["parts"][part]["size"] = built.raw_manifest()["parts"][part]["size"].clone();
        expected["parts"][part]["cs_sha256"] =
            built.raw_manifest()["parts"][part]["cs_sha256"].clone();
    }
    expected["parts"]["boot"]["min_version"] = "1.0.9".into();
    assert_eq!(built.raw_manifest(), &expected);
    let order = |manifest: &serde_json::Value| -> Vec<String> {
        manifest["parts"].as_object().expect("parts").keys().cloned().collect()
    };
    assert_eq!(order(built.raw_manifest()), ["boot", "pt", "otadata", "nvs", "app", "fs"]);

    // Uncompressed, the manifest first, then the files in the manifest's order.
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(package.bytes())).expect("readable ZIP");
    let names: Vec<String> = (0..archive.len())
        .map(|index| archive.by_index(index).expect("entry").name().expect("name").into_owned())
        .collect();
    assert_eq!(
        names,
        [
            "manifest.json",
            "bootloader.bin",
            "partitions.bin",
            "boot_state.bin",
            "S1G3.bin",
            "fs.bin"
        ]
    );
    let mut manifest = String::new();
    let file = archive.by_name("manifest.json").expect("manifest");
    assert_eq!(file.compression(), zip::CompressionMethod::Stored);
    file.take(1 << 20).read_to_string(&mut manifest).expect("read");
    assert!(manifest.starts_with(r#"{"name":"S1G3","version":"2.0.3","platform":"esp32c3","#));
}

#[test]
fn passes_on_the_warnings_of_the_factory_check() {
    let mut layout = LAYOUT.to_vec();
    layout.retain(|entry| entry.0 != "shelly");
    let fixture = Fixture { table: table(&layout), ..Fixture::default() };
    let (_, warnings) = build(&fixture).expect("package");
    assert_eq!(warnings, ["shelly: missing, the device has it"]);
}

#[test]
fn refuses_a_package_without_profile_or_a_factory_image_that_does_not_suit() {
    let fixture = Fixture::default();
    let mut manifest = fixture.manifest();
    manifest["name"] = "S1G4".into();
    assert!(refusal(&manifest, &fixture).starts_with("S1G4 has no profile"));

    let mut damaged = Fixture::default();
    damaged.bootloader[0] = 0;
    assert!(matches!(build(&damaged), Err(BuildError::Factory(FactoryError::Bootloader { .. }))));
}

#[test]
fn refuses_a_bootloader_version_it_cannot_raise() {
    let fixture = Fixture::default();
    for (version, shown) in [
        (serde_json::Value::Null, "none"),
        ("1.0.9".into(), "1.0.9"),
        ("1.1.0".into(), "1.1.0"),
        ("one".into(), "one"),
    ] {
        let mut manifest = fixture.manifest();
        if version.is_null() {
            manifest["parts"]["boot"].as_object_mut().expect("boot").remove("min_version");
        } else {
            manifest["parts"]["boot"]["min_version"] = version;
        }
        assert_eq!(
            refusal(&manifest, &fixture),
            format!(
                "the bootloader of the official package has min_version {shown}, which is not \
                 below 1.0.9; the installer might not write ESPHome's bootloader"
            )
        );
    }
}

#[test]
fn refuses_a_package_without_an_otadata_file() {
    let fixture = Fixture::default();
    let mut manifest = fixture.manifest();
    manifest["parts"]["otadata"] =
        serde_json::json!({ "type": "otadata", "ptn": "otadata", "fill": 255, "size": OTADATA.1 });
    assert_eq!(
        refusal(&manifest, &fixture),
        "the official package has no otadata part with a file"
    );
}

#[test]
fn reports_a_package_it_cannot_write() {
    // Two parts with one file: the ZIP cannot hold the name twice.
    let fixture = Fixture::default();
    let mut manifest = fixture.manifest();
    manifest["parts"]["fs"] = manifest["parts"]["app"].clone();
    manifest["parts"]["fs"]["ptn"] = "fs_0".into();
    assert!(refusal(&manifest, &fixture).starts_with("the package cannot be written:"));
}

#[test]
fn refuses_a_package_larger_than_an_official_one_may_be() {
    // An official package just below the limit, and an application 60 kB
    // larger than the stock one.
    let fixture = Fixture {
        app: image(ImageSpec { app: true, payload: 64 * 1024, fill: 0x44, ..ImageSpec::default() }),
        ..Fixture::default()
    };
    let mut files = fixture.files();
    let others: usize =
        files.iter().filter(|file| file.0 != "fs.bin").map(|file| file.1.len()).sum();
    let fs = vec![0; MAX_PACKAGE_BYTES - others - 4096];
    let mut manifest = fixture.manifest();
    manifest["parts"]["fs"]["size"] = fs.len().into();
    manifest["parts"]["fs"]["cs_sha256"] = hex::encode(Sha256::digest(&fs)).into();
    for file in &mut files {
        if file.0 == "fs.bin" {
            file.1.clone_from(&fs);
        }
    }
    let error =
        EsphomePackage::build(&stock(&manifest, &files), fixture.factory()).expect_err("too large");
    assert!(matches!(error, BuildError::Verify(StockPackageError::TooLarge)));
    assert!(error.to_string().starts_with("the built package is unusable: the package is larger"));
}
