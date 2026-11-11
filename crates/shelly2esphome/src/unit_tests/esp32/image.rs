//! Unit tests of `crate::esp32::image` (not counted as production code for coverage).

use super::*;
use crate::esp32::synthetic::{ImageSpec, image};

#[test]
fn reads_bootloaders_and_applications() {
    let bootloader = image(ImageSpec::default());
    let parsed = Esp32Image::parse(&bootloader).expect("bootloader");
    assert_eq!(parsed.chip, ChipId(5));
    assert_eq!(parsed.flash, FlashSettings { mode: 2, size_and_frequency: 0x2F });
    assert_eq!(parsed.min_chip_revision, 3);
    assert_eq!(parsed.len, bootloader.len());
    assert!(!parsed.is_app);

    // Trailing bytes (the rest of a factory image) are not part of the image.
    let mut app = image(ImageSpec { app: true, payload: 100, ..ImageSpec::default() });
    let len = app.len();
    app.extend_from_slice(&[0xFF; 64]);
    let parsed = Esp32Image::parse(&app).expect("app");
    assert!(parsed.is_app);
    assert_eq!(parsed.len, len);

    // An image without appended hash ends after the checksum.
    let mut plain = image(ImageSpec::default());
    plain.truncate(plain.len() - 32);
    plain[23] = 0;
    assert_eq!(Esp32Image::parse(&plain).expect("without hash").len, plain.len());

    // Any non-zero flag means a digest is appended; it is checked and counted.
    let mut flagged = image(ImageSpec::default());
    flagged[23] = 2;
    let len = flagged.len();
    let digest = Sha256::digest(&flagged[..len - 32]);
    flagged[len - 32..].copy_from_slice(&digest);
    assert_eq!(Esp32Image::parse(&flagged).expect("flag 2").len, len);
    flagged[len - 1] ^= 0x01;
    assert_eq!(Esp32Image::parse(&flagged), Err(ImageError::Hash));
}

#[test]
fn refuses_malformed_and_damaged_images() {
    let good = image(ImageSpec { app: true, ..ImageSpec::default() });
    let parse = |bytes: &[u8]| Esp32Image::parse(bytes).expect_err("must fail");

    assert_eq!(parse(&good[..10]), ImageError::Truncated);
    let mut wrong_magic = good.clone();
    wrong_magic[0] = 0xE8;
    assert_eq!(parse(&wrong_magic), ImageError::NotAnImage(0xE8));
    for count in [0, 17] {
        let mut segments = good.clone();
        segments[1] = count;
        assert_eq!(parse(&segments), ImageError::Segments(count));
    }
    // Ends in the segment header, in the segment data, before the checksum
    // and inside the appended hash.
    for cut in [30, 40, good.len() - 33, good.len() - 1] {
        assert_eq!(parse(&good[..cut]), ImageError::Truncated, "cut at {cut}");
    }
    let mut damaged = good.clone();
    damaged[40] ^= 0x01;
    assert_eq!(parse(&damaged), ImageError::Checksum);
    let mut hash = good.clone();
    let last = hash.len() - 1;
    hash[last] ^= 0x01;
    assert_eq!(parse(&hash), ImageError::Hash);
    assert!(ImageError::NotAnImage(0).to_string().contains("not an ESP32 image"));
}

#[test]
fn names_known_chips() {
    let names: Vec<String> = [0, 5, 13, 9].map(|id| ChipId(id).to_string()).to_vec();
    assert_eq!(names, ["ESP32", "ESP32-C3", "ESP32-C6", "chip ID 0x0009"]);
}
