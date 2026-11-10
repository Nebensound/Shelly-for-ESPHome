//! Unit tests of `crate::image` (not counted as production code for coverage).

use super::*;

#[test]
fn crc_matches_the_elf2bin_reference() {
    // Reference value from the Python implementation in esp8266/Arduino tools/elf2bin.py.
    assert_eq!(crc_eboot(b"123456789", &(0..0)), 0x0376_E6E7);
}

#[test]
fn accepts_a_complete_image() {
    let bytes = synthetic::image(MIN_IMAGE_BYTES + 12_345);
    let image = FirmwareImage::parse(bytes.clone()).expect("valid image");
    assert_eq!(image.len(), bytes.len());
    assert!(!image.is_empty());
    assert_eq!(image.bytes().len(), bytes.len());
    assert_eq!(image.sha256_hex(), hex::encode(Sha256::digest(&bytes)));
}

#[test]
fn rejects_truncated_and_corrupted_images() {
    let good = synthetic::image(MIN_IMAGE_BYTES + 12_345);

    let truncated = good[..good.len() - 100].to_vec();
    assert_eq!(
        FirmwareImage::parse(truncated).err(),
        Some(ImageError::Truncated { part: Part::Application })
    );

    let mut flipped = good.clone();
    flipped[APP_OFFSET + 5000] ^= 0x01;
    assert_eq!(
        FirmwareImage::parse(flipped).err(),
        Some(ImageError::BadChecksum { part: Part::Application })
    );

    let mut boot_flipped = good.clone();
    boot_flipped[100] ^= 0x01;
    assert_eq!(
        FirmwareImage::parse(boot_flipped).err(),
        Some(ImageError::BadChecksum { part: Part::Bootloader })
    );

    let mut trailing = good.clone();
    trailing.extend_from_slice(&[0; 32]);
    assert_eq!(FirmwareImage::parse(trailing).err(), Some(ImageError::TrailingData(32)));

    let mut crc = good.clone();
    crc[CRC_VAL_OFFSET] ^= 0x01;
    assert_eq!(FirmwareImage::parse(crc).err(), Some(ImageError::BadCrc));

    let mut length = good.clone();
    length[CRC_LEN_OFFSET] ^= 0x01;
    assert_eq!(FirmwareImage::parse(length).err(), Some(ImageError::BadCrc));
}

#[test]
fn rejects_files_that_are_no_esp8266_images() {
    let good = synthetic::image(MIN_IMAGE_BYTES);

    let mut gz = good.clone();
    gz[..2].copy_from_slice(&GZIP_MAGIC);
    assert_eq!(FirmwareImage::parse(gz).err(), Some(ImageError::Compressed));
    assert_eq!(
        FirmwareImage::parse(good[..MIN_IMAGE_BYTES - 1].to_vec()).err(),
        Some(ImageError::TooSmall { actual: MIN_IMAGE_BYTES - 1 })
    );
    assert_eq!(
        FirmwareImage::parse(vec![0; MAX_IMAGE_BYTES + 1]).err(),
        Some(ImageError::TooLarge { actual: MAX_IMAGE_BYTES + 1 })
    );

    let mut magic = good.clone();
    magic[0] = 0;
    assert_eq!(
        FirmwareImage::parse(magic).err(),
        Some(ImageError::BadMagic { part: Part::Bootloader, found: 0 })
    );
    let mut app_magic = good.clone();
    app_magic[APP_OFFSET] = 0x12;
    assert_eq!(
        FirmwareImage::parse(app_magic).err(),
        Some(ImageError::BadMagic { part: Part::Application, found: 0x12 })
    );
    for count in [0_u8, 17] {
        let mut segments = good.clone();
        segments[1] = count;
        assert_eq!(
            FirmwareImage::parse(segments).err(),
            Some(ImageError::BadSegmentCount { part: Part::Bootloader, count })
        );
    }

    // A bootloader segment that reaches into the application sector.
    let mut long_boot = good.clone();
    long_boot[12..16].copy_from_slice(&5000_u32.to_le_bytes());
    assert_eq!(
        FirmwareImage::parse(long_boot).err(),
        Some(ImageError::Truncated { part: Part::Bootloader })
    );

    let mut esp32 = good.clone();
    esp32[4..8].copy_from_slice(&0x4008_0400_u32.to_le_bytes());
    assert_eq!(FirmwareImage::parse(esp32).err(), Some(ImageError::NotEsp8266(0x4008_0400)));
}

#[test]
fn error_messages_are_specific() {
    assert!(ImageError::Compressed.to_string().contains("gzip"));
    assert!(ImageError::TooSmall { actual: 1 }.to_string().contains("too small"));
    assert!(ImageError::TooLarge { actual: 1 }.to_string().contains("too large"));
    assert!(
        ImageError::BadMagic { part: Part::Bootloader, found: 0x12 }
            .to_string()
            .contains("bootloader: not an ESP firmware (byte 0x12")
    );
    assert!(
        ImageError::BadSegmentCount { part: Part::Application, count: 0 }
            .to_string()
            .contains("application")
    );
    assert!(ImageError::NotEsp8266(0x4008_0400).to_string().contains("ESP32"));
    assert!(ImageError::Truncated { part: Part::Application }.to_string().contains("truncated"));
    assert!(ImageError::BadChecksum { part: Part::Bootloader }.to_string().contains("corrupted"));
    assert!(ImageError::TrailingData(3).to_string().contains("3 more bytes"));
    assert!(ImageError::BadCrc.to_string().contains("CRC"));
}

#[test]
fn rejects_a_bootloader_whose_checksum_lies_beyond_its_area() {
    let mut bytes = synthetic::image(MIN_IMAGE_BYTES);
    // One segment that fills the bootloader area completely, so the checksum
    // byte would have to follow at 0x1000 and beyond.
    let data_len = APP_OFFSET - 2 * HEADER_BYTES;
    let mut bootloader = vec![IMAGE_MAGIC, 1, 3, 0x20];
    bootloader.extend_from_slice(&0x4010_f480_u32.to_le_bytes());
    bootloader.extend_from_slice(&0x4010_f000_u32.to_le_bytes());
    bootloader.extend_from_slice(&u32::try_from(data_len).expect("fits").to_le_bytes());
    bootloader.resize(APP_OFFSET, 0x11);
    bytes[..APP_OFFSET].copy_from_slice(&bootloader);
    assert_eq!(
        FirmwareImage::parse(bytes).err(),
        Some(ImageError::Truncated { part: Part::Bootloader })
    );
}

#[test]
fn rejects_segment_headers_beyond_the_end() {
    let mut bytes = synthetic::image(MIN_IMAGE_BYTES);
    // Announce more application segments than the file contains.
    bytes[APP_OFFSET + 1] = MAX_SEGMENTS;
    assert_eq!(
        FirmwareImage::parse(bytes).err(),
        Some(ImageError::Truncated { part: Part::Application })
    );
}
