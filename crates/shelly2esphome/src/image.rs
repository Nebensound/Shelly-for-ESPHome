//! Validation of the user-built ESPHome firmware image.
//!
//! The migration firmware writes the image unchanged from address `0x0`, so it
//! has to be a complete, uncompressed ESP8266 image as produced by ESPHome
//! (Arduino core, `elf2bin`) as the single-image `firmware.bin`:
//!
//! ```text
//! 0x0000  eboot bootloader image   (header, segments, XOR checksum)
//! 0x1000  application image        (header, segments, XOR checksum)
//! 0x1010  embedded image length    (u32, part of the first app segment)
//! 0x1014  embedded CRC-32 of the whole file (eboot algorithm)
//! ```
//!
//! Every segment must lie within the file, both XOR checksums must match, the
//! application image must end exactly at the end of the file, and the embedded
//! length and CRC must match. A truncated or corrupted file therefore fails
//! here, before anything is sent to a device.

use sha2::{Digest, Sha256};

/// First byte of every ESP8266 image.
const IMAGE_MAGIC: u8 = 0xE9;
/// gzip magic; ESPHome can emit compressed OTA files, which cannot be used here.
const GZIP_MAGIC: [u8; 2] = [0x1F, 0x8B];
/// Smallest image accepted (eboot plus a plausible application).
pub const MIN_IMAGE_BYTES: usize = 64 * 1024;
/// Upper bound independent of the device; the model limit is checked separately.
pub const MAX_IMAGE_BYTES: usize = 1024 * 1024;
/// The application image starts after the 4 KiB eboot sector.
const APP_OFFSET: usize = 0x1000;
/// Offsets of the length and CRC words written by `elf2bin`.
const CRC_LEN_OFFSET: usize = APP_OFFSET + 16;
const CRC_VAL_OFFSET: usize = APP_OFFSET + 20;
/// The eboot entry point lies in ESP8266 instruction RAM.
const ESP8266_IRAM: std::ops::Range<u32> = 0x4010_0000..0x4011_0000;
const MAX_SEGMENTS: u8 = 16;
const HEADER_BYTES: usize = 8;
const XOR_SEED: u8 = 0xEF;

/// Which of the two images inside the file a finding refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// The eboot bootloader at `0x0`.
    Bootloader,
    /// The application at `0x1000`.
    Application,
}

impl std::fmt::Display for Part {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Bootloader => "bootloader",
            Self::Application => "application",
        })
    }
}

/// Reasons why a file is not a usable ESP8266 image.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ImageError {
    /// The file is gzip compressed.
    #[error("the file is gzip-compressed; the uncompressed firmware.bin is required")]
    Compressed,
    /// The file is smaller than any plausible ESPHome image.
    #[error("the file is too small with {actual} bytes (at least {MIN_IMAGE_BYTES} bytes)")]
    TooSmall {
        /// Size of the file in bytes.
        actual: usize,
    },
    /// The file is larger than any supported staging slot.
    #[error("the file is too large with {actual} bytes (at most {MAX_IMAGE_BYTES} bytes)")]
    TooLarge {
        /// Size of the file in bytes.
        actual: usize,
    },
    /// An image header does not start with the ESP magic byte.
    #[error("{part}: not an ESP firmware (byte 0x{found:02X} instead of 0xE9)")]
    BadMagic {
        /// Affected image.
        part: Part,
        /// Byte found instead of the magic.
        found: u8,
    },
    /// An image header announces an implausible number of segments.
    #[error("{part}: invalid image header ({count} segments)")]
    BadSegmentCount {
        /// Affected image.
        part: Part,
        /// Announced number of segments.
        count: u8,
    },
    /// The bootloader entry point is outside the ESP8266 instruction RAM.
    #[error("not an ESP8266 image (entry point 0x{0:08X}); was it built for an ESP32?")]
    NotEsp8266(u32),
    /// A segment or the checksum lies beyond the end of the file or image.
    #[error("{part}: a segment extends past the end – truncated file?")]
    Truncated {
        /// Affected image.
        part: Part,
    },
    /// The XOR checksum of an image does not match.
    #[error("{part}: checksum mismatch – corrupted file?")]
    BadChecksum {
        /// Affected image.
        part: Part,
    },
    /// The file continues after the application image.
    #[error("{0} more bytes follow the application – not a single ESPHome firmware.bin?")]
    TrailingData(usize),
    /// The embedded length or CRC-32 does not match the file.
    #[error("embedded length/CRC does not match the file – truncated or modified?")]
    BadCrc,
}

/// A validated firmware image together with its digest.
#[derive(Debug, Clone)]
pub struct FirmwareImage {
    bytes: bytes::Bytes,
    sha256_hex: String,
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes
        .get(offset..offset + 4)
        .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
}

/// Walks one ESP8266 image starting at `start`, verifies its segments and XOR
/// checksum and returns the offset just behind its checksum byte. `zeroed`
/// lists byte ranges that count as zero (patched after the checksum was made).
fn walk_image(
    bytes: &[u8],
    start: usize,
    end_limit: usize,
    part: Part,
    zeroed: &std::ops::Range<usize>,
) -> Result<usize, ImageError> {
    // Missing bytes read as 0, which fails the magic or segment count check.
    let magic = bytes.get(start).copied().unwrap_or(0);
    if magic != IMAGE_MAGIC {
        return Err(ImageError::BadMagic { part, found: magic });
    }
    let count = bytes.get(start + 1).copied().unwrap_or(0);
    if count == 0 || count > MAX_SEGMENTS {
        return Err(ImageError::BadSegmentCount { part, count });
    }
    let mut checksum = XOR_SEED;
    let mut position = start + HEADER_BYTES;
    for _ in 0..count {
        // A missing segment header reads as an endless segment.
        let length = read_u32(bytes, position + 4).unwrap_or(u32::MAX);
        let data_start = position + HEADER_BYTES;
        let data_end = data_start.saturating_add(length as usize);
        if data_end > end_limit {
            return Err(ImageError::Truncated { part });
        }
        for (offset, byte) in bytes[data_start..data_end].iter().enumerate() {
            if !zeroed.contains(&(data_start + offset)) {
                checksum ^= byte;
            }
        }
        position = data_end;
    }
    // Zero padding up to a 16-byte boundary, the checksum is the last byte.
    let checksum_offset = start + (position - start + 1).div_ceil(16) * 16 - 1;
    let stored = *bytes
        .get(checksum_offset)
        .filter(|_| checksum_offset < end_limit)
        .ok_or(ImageError::Truncated { part })?;
    if stored != checksum {
        return Err(ImageError::BadChecksum { part });
    }
    Ok(checksum_offset + 1)
}

/// CRC-32 as computed by the ESP8266 Arduino `elf2bin`/eboot (MSB first,
/// polynomial 0x04C11DB7, no final XOR).
fn crc_eboot(bytes: &[u8], zeroed: &std::ops::Range<usize>) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for (index, &byte) in bytes.iter().enumerate() {
        let byte = if zeroed.contains(&index) { 0 } else { byte };
        for bit in (0..8).rev() {
            let top = crc & 0x8000_0000 != 0;
            let input = (byte >> bit) & 1 == 1;
            crc <<= 1;
            if top != input {
                crc ^= 0x04C1_1DB7;
            }
        }
    }
    crc
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
        let nothing_zeroed = 0..0;
        walk_image(&bytes, 0, APP_OFFSET, Part::Bootloader, &nothing_zeroed)?;
        let entry = read_u32(&bytes, 4).unwrap_or_default();
        if !ESP8266_IRAM.contains(&entry) {
            return Err(ImageError::NotEsp8266(entry));
        }
        // The length/CRC words were written after the XOR checksum was computed.
        let crc_words = CRC_LEN_OFFSET..CRC_VAL_OFFSET + 4;
        let app_end = walk_image(&bytes, APP_OFFSET, bytes.len(), Part::Application, &crc_words)?;
        if app_end != bytes.len() {
            return Err(ImageError::TrailingData(bytes.len() - app_end));
        }
        let embedded_len = read_u32(&bytes, CRC_LEN_OFFSET).unwrap_or_default();
        let embedded_crc = read_u32(&bytes, CRC_VAL_OFFSET).unwrap_or_default();
        if embedded_len as usize != bytes.len() || crc_eboot(&bytes, &crc_words) != embedded_crc {
            return Err(ImageError::BadCrc);
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

/// Builds synthetic images with the exact structure of an ESPHome ESP8266
/// `firmware.bin`. Public for the integration tests; not part of the CLI.
#[doc(hidden)]
pub mod synthetic {
    use super::{APP_OFFSET, CRC_LEN_OFFSET, CRC_VAL_OFFSET, XOR_SEED, crc_eboot};

    fn push_image(out: &mut Vec<u8>, entry: u32, segments: &[(u32, Vec<u8>)]) {
        let start = out.len();
        let count = u8::try_from(segments.len()).unwrap_or(u8::MAX);
        out.extend_from_slice(&[0xE9, count, 3, 0x20]);
        out.extend_from_slice(&entry.to_le_bytes());
        let mut checksum = XOR_SEED;
        for (address, data) in segments {
            out.extend_from_slice(&address.to_le_bytes());
            out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_le_bytes());
            out.extend_from_slice(data);
            checksum = data.iter().fold(checksum, |acc, byte| acc ^ byte);
        }
        while (out.len() - start + 1) % 16 != 0 {
            out.push(0);
        }
        out.push(checksum);
    }

    /// A valid image of `len` bytes rounded down to a multiple of 16
    /// (`len` ≥ 64 KiB).
    #[must_use]
    pub fn image(len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        push_image(
            &mut out,
            0x4010_f480,
            &[(0x4010_f000, vec![0x11; 3424]), (0x3fff_20b8, vec![0x22; 40])],
        );
        out.resize(APP_OFFSET, 0xFF);
        // Application: header (8) + three segment headers (24) + small
        // segments (36) + checksum byte (1) + flash-mapped segment.
        let app_bytes = (len - APP_OFFSET) / 16 * 16;
        let irom_len = app_bytes - (8 + 24 + 36 + 1);
        // The first 8 bytes hold the length/CRC words; they are zero while the
        // XOR checksum is computed, exactly like elf2bin does it.
        let irom: Vec<u8> = (0..irom_len)
            .map(|i| if i < 8 { 0 } else { u8::try_from(i % 251).unwrap_or(0) })
            .collect();
        push_image(
            &mut out,
            0x4010_00c0,
            &[(0x4020_1010, irom), (0x4010_0000, vec![0x33; 20]), (0x3ffe_8000, vec![0x44; 16])],
        );
        let total = u32::try_from(out.len()).unwrap_or(u32::MAX);
        let crc = crc_eboot(&out, &(0..0));
        out[CRC_LEN_OFFSET..CRC_LEN_OFFSET + 4].copy_from_slice(&total.to_le_bytes());
        out[CRC_VAL_OFFSET..CRC_VAL_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
        out
    }
}

#[cfg(test)]
#[path = "unit_tests/image.rs"]
mod tests;
