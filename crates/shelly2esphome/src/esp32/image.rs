//! ESP32 images (bootloader and application) in the layout esptool writes:
//! a 24-byte header, segments, a checksum byte that ends a 16-byte block, and
//! an optional SHA-256 over everything before it.

use sha2::{Digest, Sha256};

use super::ChipId;

/// First byte of every ESP32 image.
pub const MAGIC: u8 = 0xE9;
const HEADER_LEN: usize = 24;
const SEGMENT_HEADER_LEN: usize = 8;
const MAX_SEGMENTS: u8 = 16;
const CHECKSUM_SEED: u8 = 0xEF;
const SHA256_LEN: usize = 32;
/// `esp_app_desc_t.magic_word`, right after the first segment header of an
/// application; a bootloader has none.
const APP_DESC_MAGIC: [u8; 4] = 0xABCD_5432_u32.to_le_bytes();
const APP_DESC_OFFSET: usize = HEADER_LEN + SEGMENT_HEADER_LEN;

/// Flash settings from the image header. The bootloader's must match the
/// flash chip of the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashSettings {
    /// SPI mode (QIO, QOUT, DIO, DOUT).
    pub mode: u8,
    /// Flash size (high nibble) and frequency (low nibble).
    pub size_and_frequency: u8,
}

/// A parsed and verified ESP32 image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Esp32Image {
    /// Chip the image was built for.
    pub chip: ChipId,
    /// Flash settings.
    pub flash: FlashSettings,
    /// Minimum chip revision (`major * 100 + minor`).
    pub min_chip_revision: u16,
    /// Length of the image in bytes, checksum and hash included.
    pub len: usize,
    /// Whether it is an application (it carries an app descriptor).
    pub is_app: bool,
}

/// Reasons why bytes are no usable ESP32 image.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ImageError {
    /// The first byte is not the image magic.
    #[error("not an ESP32 image (first byte {0:#04x}, expected 0xe9)")]
    NotAnImage(u8),
    /// The bytes end inside the image.
    #[error("the image is truncated")]
    Truncated,
    /// The segment count is zero or larger than esptool allows.
    #[error("implausible segment count {0}")]
    Segments(u8),
    /// The checksum over the segment data does not match.
    #[error("the image checksum does not match (damaged image)")]
    Checksum,
    /// The appended SHA-256 does not match.
    #[error("the SHA-256 appended to the image does not match (damaged image)")]
    Hash,
}

fn le_u32(bytes: &[u8], at: usize) -> Option<usize> {
    let word = bytes.get(at..).and_then(<[u8]>::first_chunk::<4>)?;
    // A u32 fits into usize on every target this tool runs on.
    Some(usize::try_from(u32::from_le_bytes(*word)).unwrap_or(usize::MAX))
}

impl Esp32Image {
    /// Parses and verifies the image at the start of `bytes`; anything after
    /// it is ignored.
    ///
    /// # Errors
    /// Returns an [`ImageError`] if the image is malformed or damaged.
    pub fn parse(bytes: &[u8]) -> Result<Self, ImageError> {
        let header: [u8; HEADER_LEN] = bytes
            .get(..HEADER_LEN)
            .and_then(|header| header.try_into().ok())
            .ok_or(ImageError::Truncated)?;
        if header[0] != MAGIC {
            return Err(ImageError::NotAnImage(header[0]));
        }
        let segments = header[1];
        if segments == 0 || segments > MAX_SEGMENTS {
            return Err(ImageError::Segments(segments));
        }
        let mut offset = HEADER_LEN;
        let mut checksum = CHECKSUM_SEED;
        for _ in 0..segments {
            let data = le_u32(bytes, offset + 4)
                .and_then(|len| {
                    bytes.get(offset + SEGMENT_HEADER_LEN..).and_then(|rest| rest.get(..len))
                })
                .ok_or(ImageError::Truncated)?;
            checksum = data.iter().fold(checksum, |sum, byte| sum ^ byte);
            offset += SEGMENT_HEADER_LEN + data.len();
        }
        // Zero padding up to the last byte of a 16-byte block, which holds
        // the checksum.
        let checksum_at = offset | 15;
        if *bytes.get(checksum_at).ok_or(ImageError::Truncated)? != checksum {
            return Err(ImageError::Checksum);
        }
        let mut len = checksum_at + 1;
        // ESP-IDF treats any non-zero value as "digest appended".
        if header[23] != 0 {
            let appended = bytes.get(len..len + SHA256_LEN).ok_or(ImageError::Truncated)?;
            if Sha256::digest(bytes.get(..len).unwrap_or_default()).as_slice() != appended {
                return Err(ImageError::Hash);
            }
            len += SHA256_LEN;
        }
        Ok(Self {
            chip: ChipId(u16::from_le_bytes([header[12], header[13]])),
            flash: FlashSettings { mode: header[2], size_and_frequency: header[3] },
            min_chip_revision: u16::from_le_bytes([header[15], header[16]]),
            len,
            is_app: bytes.get(APP_DESC_OFFSET..APP_DESC_OFFSET + 4) == Some(&APP_DESC_MAGIC[..]),
        })
    }
}

#[cfg(test)]
#[path = "../unit_tests/esp32/image.rs"]
mod tests;
