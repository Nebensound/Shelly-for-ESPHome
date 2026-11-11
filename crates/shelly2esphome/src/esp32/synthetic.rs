//! Synthetic ESP32 images, partition tables, factory images and official
//! packages with the structure of the real ones. Public for the integration
//! tests; not part of the CLI.

use std::io::Write;

use md5::Md5;
use sha2::{Digest, Sha256};

/// Header fields of a synthetic image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageSpec {
    /// Chip ID (5 = ESP32-C3).
    pub chip: u16,
    /// SPI mode.
    pub mode: u8,
    /// Flash size and frequency.
    pub size_and_frequency: u8,
    /// Minimum chip revision.
    pub min_chip_revision: u16,
    /// Whether to add an app descriptor.
    pub app: bool,
    /// Number of payload bytes after the descriptor.
    pub payload: usize,
    /// Payload byte, to tell images apart.
    pub fill: u8,
}

impl Default for ImageSpec {
    fn default() -> Self {
        Self {
            chip: 5,
            mode: 2,
            size_and_frequency: 0x2F,
            min_chip_revision: 3,
            app: false,
            payload: 64,
            fill: 0x11,
        }
    }
}

/// An image with one segment, checksum and appended SHA-256.
#[must_use]
pub fn image(spec: ImageSpec) -> Vec<u8> {
    let mut data = Vec::new();
    if spec.app {
        data.extend_from_slice(&0xABCD_5432_u32.to_le_bytes());
    }
    data.resize(data.len() + spec.payload, spec.fill);
    let mut out = vec![0xE9, 1, spec.mode, spec.size_and_frequency];
    out.extend_from_slice(&0x4038_0000_u32.to_le_bytes());
    out.extend_from_slice(&[0xEE, 0, 0, 0]);
    out.extend_from_slice(&spec.chip.to_le_bytes());
    out.push(0);
    out.extend_from_slice(&spec.min_chip_revision.to_le_bytes());
    out.extend_from_slice(&[0xFF, 0xFF, 0, 0, 0, 0, 1]);
    out.extend_from_slice(&0x3FC8_0000_u32.to_le_bytes());
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&data);
    let checksum = data.iter().fold(0xEF_u8, |sum, byte| sum ^ byte);
    out.resize(out.len() | 15, 0);
    out.push(checksum);
    let digest = Sha256::digest(&out);
    out.extend_from_slice(&digest);
    out
}

/// A partition: label, type, subtype, offset, size.
pub type Entry = (&'static str, u8, u8, u32, u32);

/// A Shelly-like layout with the bootloader at 0x0 and the table at 0x8000.
pub const LAYOUT: [Entry; 7] = [
    ("otadata", 1, 0x00, 0x9000, 0x2000),
    ("nvs", 1, 0x02, 0xB000, 0x5000),
    ("app_0", 0, 0x10, 0x1_0000, 0x1_0000),
    ("app_1", 0, 0x11, 0x2_0000, 0x1_0000),
    ("fs_0", 1, 0x82, 0x3_0000, 0x8000),
    ("fs_1", 1, 0x82, 0x3_8000, 0x8000),
    ("shelly", 1, 0x99, 0x4_0000, 0x1000),
];
/// Bootloader address of [`LAYOUT`].
pub const BOOT_ADDR: usize = 0;
/// Partition table address of [`LAYOUT`].
pub const TABLE_ADDR: usize = 0x8000;

/// A partition table with MD5 entry and end marker.
#[must_use]
pub fn table(entries: &[Entry]) -> Vec<u8> {
    table_with_flags(entries, &[])
}

/// Like [`table`], with `flags` for the named partitions.
#[must_use]
pub fn table_with_flags(entries: &[Entry], flags: &[(&str, u32)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (label, kind, subtype, offset, size) in entries {
        out.extend_from_slice(&[0xAA, 0x50, *kind, *subtype]);
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        let mut name = [0_u8; 16];
        for (slot, byte) in name.iter_mut().zip(label.bytes()) {
            *slot = byte;
        }
        out.extend_from_slice(&name);
        let flag = flags.iter().find(|(name, _)| name == label).map_or(0, |(_, flag)| *flag);
        out.extend_from_slice(&flag.to_le_bytes());
    }
    let digest = Md5::digest(&out);
    out.extend_from_slice(&[0xEB, 0xEB]);
    out.extend_from_slice(&[0xFF; 14]);
    out.extend_from_slice(&digest);
    out.extend_from_slice(&[0xFF; 32]);
    out
}

/// The pieces of a matching official package and factory image.
#[derive(Debug, Clone)]
pub struct Fixture {
    /// Stock bootloader in the package.
    pub stock_bootloader: Vec<u8>,
    /// Stock application in the package.
    pub stock_app: Vec<u8>,
    /// Partition table of the package.
    pub stock_table: Vec<u8>,
    /// ESPHome bootloader in the factory image.
    pub bootloader: Vec<u8>,
    /// Partition table in the factory image.
    pub table: Vec<u8>,
    /// ESPHome application in the factory image.
    pub app: Vec<u8>,
}

impl Default for Fixture {
    fn default() -> Self {
        let app = ImageSpec { app: true, payload: 4096, ..ImageSpec::default() };
        Self {
            stock_bootloader: image(ImageSpec::default()),
            stock_app: image(ImageSpec { fill: 0x22, ..app }),
            stock_table: table(&LAYOUT),
            bootloader: image(ImageSpec { fill: 0x33, ..ImageSpec::default() }),
            table: table(&LAYOUT),
            app: image(ImageSpec { fill: 0x44, ..app }),
        }
    }
}

fn part(src: &str, data: &[u8], location: (&str, serde_json::Value)) -> serde_json::Value {
    let mut part = serde_json::json!({
        "type": "x", "src": src, "size": data.len(), "cs_sha256": hex::encode(Sha256::digest(data))
    });
    part[location.0] = location.1;
    part
}

impl Fixture {
    /// The manifest of [`Fixture::package`].
    #[must_use]
    pub fn manifest(&self) -> serde_json::Value {
        let mut boot = part("bootloader.bin", &self.stock_bootloader, ("addr", BOOT_ADDR.into()));
        boot["min_version"] = "1.0.2".into();
        serde_json::json!({
            "name": "S1G3", "version": "2.0.3", "platform": "esp32c3", "build_id": "20261026",
            "parts": {
                "boot": boot,
                "pt": part("partitions.bin", &self.stock_table, ("addr", TABLE_ADDR.into())),
                "otadata": { "type": "otadata", "ptn": "otadata", "fill": 255, "size": 0x2000 },
                "app": part("S1G3.bin", &self.stock_app, ("ptn", "app_0".into())),
                "fs": part("fs.bin", &[0xA5; 256], ("ptn", "fs_0".into())),
            }
        })
    }

    /// The files of [`Fixture::package`].
    #[must_use]
    pub fn files(&self) -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("bootloader.bin", self.stock_bootloader.clone()),
            ("partitions.bin", self.stock_table.clone()),
            ("S1G3.bin", self.stock_app.clone()),
            ("fs.bin", vec![0xA5; 256]),
        ]
    }

    /// The official package.
    #[must_use]
    pub fn package(&self) -> Vec<u8> {
        zip_package(&self.manifest(), &self.files())
    }

    /// The factory image: everything from flash address 0 to the end of the
    /// application, gaps filled with 0xFF.
    #[must_use]
    pub fn factory(&self) -> Vec<u8> {
        let app_offset = 0x1_0000;
        let mut out = vec![0xFF; app_offset + self.app.len()];
        for (offset, data) in
            [(BOOT_ADDR, &self.bootloader), (TABLE_ADDR, &self.table), (app_offset, &self.app)]
        {
            for (slot, byte) in out.iter_mut().skip(offset).zip(data.iter()) {
                *slot = *byte;
            }
        }
        out
    }
}

/// A ZIP with `manifest.json` and `files`.
#[must_use]
pub fn zip_package(manifest: &serde_json::Value, files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    let manifest = manifest.to_string();
    for (name, data) in std::iter::once(("manifest.json", manifest.as_bytes()))
        .chain(files.iter().map(|(name, data)| (*name, data.as_slice())))
    {
        let _ = writer.start_file(name, options);
        let _ = writer.write_all(data);
    }
    writer.finish().map(std::io::Cursor::into_inner).unwrap_or_default()
}
