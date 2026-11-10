//! Unit tests of `crate::migration` (not counted as production code for coverage).

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
    package_zip(&[
        ("fw/manifest.json", &manifest),
        ("fw/fs.bin", "no marker here"),
        ("fw/app.bin", "\0\0shelly2esphome-mg2x-protocol=2\0rest"),
    ])
}

#[test]
fn accepts_the_package_of_the_right_model() {
    let package = MigrationPackage::parse(package_for(Model::ShellyDimmer2)).expect("package");
    assert_eq!(package.app_name(), "dimmer-l51");
    assert!(package.ensure_model(Model::ShellyDimmer2).is_ok());
    assert!(!package.bytes().is_empty());
}

#[test]
fn rejects_stock_and_outdated_packages() {
    let manifest = r#"{"name":"dimmer-l51","platform":"esp8266"}"#;
    let stock = package_zip(&[("manifest.json", manifest), ("dimmer-l51.bin", "stock")]);
    let error = MigrationPackage::parse(stock).expect_err("stock");
    assert!(matches!(error, PackageError::NotAMigrationPackage));
    assert!(error.to_string().contains("not a shelly2esphome migration package"));

    let old =
        package_zip(&[("manifest.json", manifest), ("fw.bin", "shelly2esphome-mg2x-protocol=1")]);
    let error = MigrationPackage::parse(old).expect_err("old");
    assert!(matches!(error, PackageError::ProtocolMismatch(1)));
    assert!(error.to_string().contains("protocol 1"));

    let garbled =
        package_zip(&[("manifest.json", manifest), ("fw.bin", "shelly2esphome-mg2x-protocol=")]);
    assert!(matches!(MigrationPackage::parse(garbled), Err(PackageError::ProtocolMismatch(0))));
}

#[test]
fn root_level_manifest_is_found() {
    let zip = package_zip(&[
        ("manifest.json", r#"{"name":"switch1pm","platform":"esp8266"}"#),
        ("fw.bin", "shelly2esphome-mg2x-protocol=2"),
    ]);
    let package = MigrationPackage::parse(zip).expect("package");
    assert!(package.ensure_model(Model::Shelly1Pm).is_ok());
}

#[test]
fn rejects_the_package_of_another_model() {
    let package = MigrationPackage::parse(package_for(Model::Shelly1Pm)).expect("package");
    let error = package.ensure_model(Model::ShellyDimmer2).expect_err("wrong model");
    assert!(matches!(error, PackageError::WrongModel { expected: "dimmer-l51", .. }));
    assert!(error.to_string().contains("wrong"));
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

#[test]
fn reports_a_corrupted_binary_as_such() {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file("manifest.json", stored).expect("start manifest");
    writer.write_all(br#"{"name":"switch1pm","platform":"esp8266"}"#).expect("manifest");
    writer.start_file("fw.bin", stored).expect("start bin");
    writer.write_all(b"shelly2esphome-mg2x-protocol=2 PAYLOAD").expect("bin");
    let mut zip = writer.finish().expect("finish").into_inner();
    // Flip a data byte: the entry's CRC-32 no longer matches.
    let at = zip.windows(7).position(|w| w == b"PAYLOAD").expect("payload");
    zip[at] ^= 0x01;
    let error = MigrationPackage::parse(zip).expect_err("corrupted");
    assert!(matches!(error, PackageError::ReadEntry(_)));
    assert!(error.to_string().contains("corrupted"));
}

/// Flips the signature of the local header that precedes `name`'s data.
fn corrupt_local_header(zip: &mut [u8], name: &str) {
    let needle = name.as_bytes();
    let at = zip.windows(needle.len()).position(|w| w == needle).expect("name in local header");
    // Local file header: 30 fixed bytes, then the name.
    zip[at - 30] ^= 0xFF;
}

#[test]
fn reports_unreadable_entries() {
    let manifest = r#"{"name":"switch1pm","platform":"esp8266"}"#;
    let mut zip =
        package_zip(&[("manifest.json", manifest), ("fw.bin", "shelly2esphome-mg2x-protocol=2")]);
    corrupt_local_header(&mut zip, "fw.bin");
    assert!(matches!(MigrationPackage::parse(zip), Err(PackageError::Zip(_))));

    let mut zip =
        package_zip(&[("manifest.json", manifest), ("fw.bin", "shelly2esphome-mg2x-protocol=2")]);
    corrupt_local_header(&mut zip, "manifest.json");
    assert!(matches!(MigrationPackage::parse(zip), Err(PackageError::Zip(_))));
}

#[test]
fn reports_a_manifest_that_is_no_text() {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer.start_file("manifest.json", zip::write::SimpleFileOptions::default()).expect("start");
    writer.write_all(&[0xFF, 0xFE, 0x00]).expect("write");
    let zip = writer.finish().expect("finish").into_inner();
    assert!(matches!(MigrationPackage::parse(zip), Err(PackageError::Manifest(_))));
}
