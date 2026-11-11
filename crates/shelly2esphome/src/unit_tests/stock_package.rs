//! Unit tests of `crate::stock_package` (not counted as production code for coverage).

use super::*;
use crate::esp32::synthetic::{BOOT_ADDR, Fixture, TABLE_ADDR, zip_package};

#[test]
fn reads_an_official_package() {
    let fixture = Fixture::default();
    let package = StockPackage::parse(&fixture.package()).expect("package");
    assert_eq!(package.name().as_str(), "S1G3");
    assert_eq!(package.version().as_str(), "2.0.3");
    assert_eq!(package.platform(), Some("esp32c3"));
    assert_eq!(package.addresses(), (BOOT_ADDR, TABLE_ADDR));
    assert_eq!(package.part("boot").and_then(|part| part.min_version.as_deref()), Some("1.0.2"));
    assert_eq!(package.file("app"), Some(fixture.stock_app.as_slice()));
    // Filled parts have no file.
    assert_eq!(package.file("otadata"), None);
    assert_eq!(package.file("missing"), None);
    assert!(package.app().is_app);
    assert!(!package.bootloader().is_app);
    assert!(package.partition_table().find("app_1").is_some());
    // Unknown fields stay in the raw manifest for rebuilding the package.
    assert_eq!(package.raw_manifest()["build_id"], "20261026");
}

fn refuse(manifest: &serde_json::Value, files: &[(&str, Vec<u8>)]) -> String {
    StockPackage::parse(&zip_package(manifest, files)).expect_err("must fail").to_string()
}

#[test]
fn refuses_unusable_packages() {
    let fixture = Fixture::default();
    let files = fixture.files();
    assert!(
        StockPackage::parse(&vec![0; MAX_PACKAGE_BYTES + 1])
            .expect_err("too large")
            .to_string()
            .contains("larger than")
    );
    assert!(
        StockPackage::parse(b"no zip").expect_err("no zip").to_string().contains("no readable ZIP")
    );
    assert!(refuse(&serde_json::json!({}), &[]).contains("manifest.json is unusable"));
    // manifest.json missing, and not JSON at all.
    let empty = zip_package(&serde_json::Value::Null, &[]);
    assert!(StockPackage::parse(&empty).expect_err("null").to_string().contains("unusable"));
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer.start_file("other.txt", zip::write::SimpleFileOptions::default()).expect("start");
    let no_manifest = writer.finish().expect("finish").into_inner();
    assert!(
        StockPackage::parse(&no_manifest)
            .expect_err("no manifest")
            .to_string()
            .contains("manifest.json in the package cannot be read")
    );
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer.start_file("manifest.json", zip::write::SimpleFileOptions::default()).expect("start");
    std::io::Write::write_all(&mut writer, b"{").expect("write");
    let broken_json = writer.finish().expect("finish").into_inner();
    assert!(StockPackage::parse(&broken_json).expect_err("json").to_string().contains("unusable"));

    // A stored file whose bytes no longer match its CRC.
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file("manifest.json", stored).expect("start");
    std::io::Write::write_all(&mut writer, fixture.manifest().to_string().as_bytes())
        .expect("write");
    for (name, data) in &files {
        writer.start_file(*name, stored).expect("start");
        std::io::Write::write_all(&mut writer, data).expect("write");
    }
    let mut corrupt = writer.finish().expect("finish").into_inner();
    let at = corrupt.windows(64).position(|window| window == &files[0].1[..64]).expect("data");
    corrupt[at + 40] ^= 0x01;
    assert!(
        StockPackage::parse(&corrupt)
            .expect_err("crc")
            .to_string()
            .contains("bootloader.bin in the package cannot be read")
    );

    // A part's file missing, wrong size, wrong hash.
    assert!(refuse(&fixture.manifest(), &files[1..]).contains("bootloader.bin in the package"));
    let mut size = fixture.manifest();
    size["parts"]["fs"]["size"] = 1.into();
    assert!(refuse(&size, &files).contains("part fs does not match the manifest (size)"));
    let mut hash = fixture.manifest();
    hash["parts"]["fs"]["cs_sha256"] = "00".into();
    assert!(refuse(&hash, &files).contains("part fs does not match the manifest (sha256)"));

    // Needed parts missing or without location.
    for (part, field) in [("boot", "addr"), ("pt", "addr"), ("app", "ptn")] {
        let mut manifest = fixture.manifest();
        manifest["parts"][part].as_object_mut().expect("part").remove(field);
        assert!(refuse(&manifest, &files).contains(&format!("no usable {part} part")), "{part}");
    }

    // Parts whose content is no image or table.
    let broken = |name: &'static str| {
        let mut changed = Fixture::default();
        match name {
            "boot" => changed.stock_bootloader[0] = 0,
            "pt" => changed.stock_table[0] = 0,
            _ => changed.stock_app[0] = 0,
        }
        StockPackage::parse(&changed.package()).expect_err(name).to_string()
    };
    assert!(broken("boot").contains("the bootloader of the package: not an ESP32 image"));
    assert!(broken("pt").contains("the partition table of the package"));
    assert!(broken("app").contains("the app of the package: not an ESP32 image"));
}
