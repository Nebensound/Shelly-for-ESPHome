//! Validation of the user-built ESPHome firmware image.
//!
//! The migration firmware writes the image unchanged from address `0x0`, so it
//! has to be a complete, uncompressed ESP8266 image (bootloader included), as
//! produced by ESPHome as `firmware.bin` for ESP8266 targets.

use sha2::{Digest, Sha256};

/// First byte of every ESP8266 application image.
const IMAGE_MAGIC: u8 = 0xE9;
/// gzip magic; ESPHome can emit compressed OTA files, which cannot be used here.
const GZIP_MAGIC: [u8; 2] = [0x1F, 0x8B];
/// Smallest image the migration firmware accepts with margin (it needs at least
/// the region up to the boot configuration plus two sectors, i.e. 36 KiB).
pub const MIN_IMAGE_BYTES: usize = 64 * 1024;
/// Upper bound before the device reports its real limit (staging slot size).
pub const MAX_IMAGE_BYTES: usize = 1024 * 1024;
/// The ESP8266 bootloader/eboot entry point lies in instruction RAM.
const ESP8266_IRAM: std::ops::Range<u32> = 0x4010_0000..0x4011_0000;
const MAX_SEGMENTS: u8 = 16;

/// Reasons why a file is not a usable ESP8266 image.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ImageError {
    /// The file is gzip compressed.
    #[error("die Datei ist gzip-komprimiert; benötigt wird die unkomprimierte firmware.bin")]
    Compressed,
    /// The file is smaller than any plausible ESPHome image.
    #[error("die Datei ist mit {actual} Bytes zu klein (mindestens {MIN_IMAGE_BYTES} Bytes)")]
    TooSmall {
        /// Size of the file in bytes.
        actual: usize,
    },
    /// The file is larger than the staging slot can ever hold.
    #[error("die Datei ist mit {actual} Bytes zu groß (höchstens {MAX_IMAGE_BYTES} Bytes)")]
    TooLarge {
        /// Size of the file in bytes.
        actual: usize,
    },
    /// The first byte is not the ESP image magic.
    #[error("keine ESP-Firmware (erstes Byte 0x{0:02X} statt 0xE9)")]
    BadMagic(u8),
    /// The header announces an implausible number of segments.
    #[error("ungültiger Image-Header ({0} Segmente)")]
    BadSegmentCount(u8),
    /// The entry point is outside the ESP8266 instruction RAM (e.g. ESP32 image).
    #[error("kein ESP8266-Image (Einsprungadresse 0x{0:08X}); ist es für einen ESP32 gebaut?")]
    NotEsp8266(u32),
}

/// A validated firmware image together with its digest.
#[derive(Debug, Clone)]
pub struct FirmwareImage {
    bytes: bytes::Bytes,
    sha256_hex: String,
}

impl FirmwareImage {
    /// Validates `bytes` as an ESP8266 image that the migration firmware can install.
    ///
    /// # Errors
    /// Returns an [`ImageError`] describing the first failed check.
    pub fn parse(bytes: impl Into<bytes::Bytes>) -> Result<Self, ImageError> {
        let bytes = bytes.into();
        if bytes.starts_with(&GZIP_MAGIC) {
            return Err(ImageError::Compressed);
        }
        if bytes.len() < MIN_IMAGE_BYTES {
            return Err(ImageError::TooSmall { actual: bytes.len() });
        }
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(ImageError::TooLarge { actual: bytes.len() });
        }
        if bytes[0] != IMAGE_MAGIC {
            return Err(ImageError::BadMagic(bytes[0]));
        }
        let segments = bytes[1];
        if segments == 0 || segments > MAX_SEGMENTS {
            return Err(ImageError::BadSegmentCount(segments));
        }
        let entry = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        if !ESP8266_IRAM.contains(&entry) {
            return Err(ImageError::NotEsp8266(entry));
        }
        let sha256_hex = hex::encode(Sha256::digest(&bytes));
        Ok(Self { bytes, sha256_hex })
    }

    /// Raw image bytes.
    #[must_use]
    pub fn bytes(&self) -> &bytes::Bytes {
        &self.bytes
    }

    /// Image size in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Always `false`: a validated image is never empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Lowercase hex SHA-256 of the image.
    #[must_use]
    pub fn sha256_hex(&self) -> &str {
        &self.sha256_hex
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A minimal image that passes all header checks.
    pub(crate) fn valid_image(len: usize) -> Vec<u8> {
        let mut bytes = vec![0xA5_u8; len];
        bytes[0] = IMAGE_MAGIC;
        bytes[1] = 1;
        bytes[4..8].copy_from_slice(&0x4010_f000_u32.to_le_bytes());
        bytes
    }

    #[test]
    fn accepts_a_plausible_esp8266_image() {
        let image = FirmwareImage::parse(valid_image(MIN_IMAGE_BYTES)).expect("valid image");
        assert_eq!(image.len(), MIN_IMAGE_BYTES);
        assert!(!image.is_empty());
        assert_eq!(image.bytes().len(), MIN_IMAGE_BYTES);
        assert_eq!(image.sha256_hex().len(), 64);
        assert_eq!(image.sha256_hex(), hex::encode(Sha256::digest(valid_image(MIN_IMAGE_BYTES))));
    }

    #[test]
    fn rejects_invalid_files() {
        let mut gz = valid_image(MIN_IMAGE_BYTES);
        gz[..2].copy_from_slice(&GZIP_MAGIC);
        assert_eq!(FirmwareImage::parse(gz).err(), Some(ImageError::Compressed));

        assert_eq!(
            FirmwareImage::parse(valid_image(MIN_IMAGE_BYTES - 1)).err(),
            Some(ImageError::TooSmall { actual: MIN_IMAGE_BYTES - 1 })
        );
        assert_eq!(
            FirmwareImage::parse(valid_image(MAX_IMAGE_BYTES + 1)).err(),
            Some(ImageError::TooLarge { actual: MAX_IMAGE_BYTES + 1 })
        );

        let mut magic = valid_image(MIN_IMAGE_BYTES);
        magic[0] = 0x00;
        assert_eq!(FirmwareImage::parse(magic).err(), Some(ImageError::BadMagic(0)));

        for count in [0_u8, 17] {
            let mut segments = valid_image(MIN_IMAGE_BYTES);
            segments[1] = count;
            assert_eq!(
                FirmwareImage::parse(segments).err(),
                Some(ImageError::BadSegmentCount(count))
            );
        }

        let mut esp32 = valid_image(MIN_IMAGE_BYTES);
        esp32[4..8].copy_from_slice(&0x4008_0400_u32.to_le_bytes());
        assert_eq!(FirmwareImage::parse(esp32).err(), Some(ImageError::NotEsp8266(0x4008_0400)));
    }

    #[test]
    fn error_messages_are_german_and_specific() {
        assert!(ImageError::Compressed.to_string().contains("gzip"));
        assert!(ImageError::TooSmall { actual: 1 }.to_string().contains("zu klein"));
        assert!(ImageError::TooLarge { actual: 1 }.to_string().contains("zu groß"));
        assert!(ImageError::BadMagic(0x12).to_string().contains("0x12"));
        assert!(ImageError::BadSegmentCount(0).to_string().contains("0 Segmente"));
        assert!(ImageError::NotEsp8266(0x4008_0400).to_string().contains("ESP32"));
    }
}
