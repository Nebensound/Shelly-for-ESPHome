//! ESP-IDF partition tables: 32-byte entries, an optional MD5 entry over the
//! entries before it, and an end marker of `0xFF` bytes.

use md5::{Digest, Md5};

const ENTRY_LEN: usize = 32;
/// Largest table ESP-IDF reads.
pub const MAX_TABLE_LEN: usize = 0xC00;

/// Partitions whose position and size the stock installer, the bootloader and
/// ESPHome must agree on. A difference in any other partition only warns.
pub const CRITICAL: [&str; 6] = ["otadata", "nvs", "app_0", "app_1", "fs_0", "fs_1"];

/// One entry of a partition table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    /// Label, e.g. `app_0`.
    pub label: String,
    /// Type (0 = app, 1 = data).
    pub kind: u8,
    /// Subtype, e.g. 0x10 for `ota_0`.
    pub subtype: u8,
    /// Flash offset in bytes.
    pub offset: usize,
    /// Size in bytes.
    pub size: usize,
    /// Flags (bit 0: encrypted, bit 1: read-only).
    pub flags: u32,
}

impl std::fmt::Display for Partition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let flags =
            if self.flags == 0 { String::new() } else { format!(", flags {:#x}", self.flags) };
        write!(
            formatter,
            "type {}/{:#04x} at {:#x}, {:#x} bytes{flags}",
            self.kind, self.subtype, self.offset, self.size
        )
    }
}

/// A parsed partition table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionTable {
    entries: Vec<Partition>,
}

/// Reasons why bytes are no usable partition table.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PartitionError {
    /// An entry starts with neither the entry, MD5 nor end magic.
    #[error("no valid partition table (entry {0} has no valid magic)")]
    BadMagic(usize),
    /// The table has no entries.
    #[error("the partition table is empty")]
    Empty,
    /// The table runs past its maximum size or the end of the data.
    #[error("the partition table has no end marker")]
    Unterminated,
    /// Two entries share a label; ESP-IDF's table generator refuses that,
    /// and a comparison by label would miss the second one.
    #[error("the partition table names {0} twice")]
    DuplicateLabel(String),
    /// The MD5 entry does not match the entries.
    #[error("the MD5 checksum of the partition table does not match (damaged table)")]
    Checksum,
}

/// How two partition tables differ.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableDifferences {
    /// Differences in a [`CRITICAL`] partition.
    pub critical: Vec<String>,
    /// Differences in any other partition.
    pub other: Vec<String>,
}

impl PartitionTable {
    /// Parses the table at the start of `bytes`.
    ///
    /// # Errors
    /// Returns a [`PartitionError`] if the table is malformed or damaged.
    pub fn parse(bytes: &[u8]) -> Result<Self, PartitionError> {
        let mut entries = Vec::new();
        let limit = bytes.get(..MAX_TABLE_LEN).unwrap_or(bytes);
        for (index, entry) in limit.as_chunks::<ENTRY_LEN>().0.iter().enumerate() {
            match [entry[0], entry[1]] {
                [0xAA, 0x50] => entries.push(Partition {
                    label: String::from_utf8_lossy(&entry[12..28])
                        .trim_end_matches('\0')
                        .to_owned(),
                    kind: entry[2],
                    subtype: entry[3],
                    offset: le_usize([entry[4], entry[5], entry[6], entry[7]]),
                    size: le_usize([entry[8], entry[9], entry[10], entry[11]]),
                    flags: u32::from_le_bytes([entry[28], entry[29], entry[30], entry[31]]),
                }),
                [0xEB, 0xEB] => {
                    let covered = limit.get(..index * ENTRY_LEN).unwrap_or_default();
                    if Md5::digest(covered).as_slice() != &entry[16..] {
                        return Err(PartitionError::Checksum);
                    }
                    return Self::finish(entries);
                }
                [0xFF, 0xFF] => return Self::finish(entries),
                _ => return Err(PartitionError::BadMagic(index)),
            }
        }
        Err(PartitionError::Unterminated)
    }

    fn finish(entries: Vec<Partition>) -> Result<Self, PartitionError> {
        let mut labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
        labels.sort_unstable();
        if let Some(pair) = labels.windows(2).find(|pair| pair[0] == pair[1]) {
            return Err(PartitionError::DuplicateLabel(pair[0].to_owned()));
        }
        if entries.is_empty() { Err(PartitionError::Empty) } else { Ok(Self { entries }) }
    }

    /// All entries in table order.
    #[must_use]
    pub fn entries(&self) -> &[Partition] {
        &self.entries
    }

    /// The partition with `label`.
    #[must_use]
    pub fn find(&self, label: &str) -> Option<&Partition> {
        self.entries.iter().find(|partition| partition.label == label)
    }

    /// How `self` differs from `reference`.
    #[must_use]
    pub fn differences(&self, reference: &Self) -> TableDifferences {
        let mut labels: Vec<&str> = reference
            .entries
            .iter()
            .chain(&self.entries)
            .map(|partition| partition.label.as_str())
            .collect();
        labels.sort_unstable();
        labels.dedup();
        let mut differences = TableDifferences::default();
        for label in labels {
            let line = match (self.find(label), reference.find(label)) {
                (Some(own), Some(theirs)) if own == theirs => continue,
                (Some(own), Some(theirs)) => format!("{label}: {own}, the device has {theirs}"),
                (Some(own), None) => format!("{label}: {own}, missing on the device"),
                (None, _) => format!("{label}: missing, the device has it"),
            };
            if CRITICAL.contains(&label) {
                differences.critical.push(line);
            } else {
                differences.other.push(line);
            }
        }
        differences
    }
}

fn le_usize(bytes: [u8; 4]) -> usize {
    // u32 always fits into usize on the 32- and 64-bit targets this tool runs on.
    usize::try_from(u32::from_le_bytes(bytes)).unwrap_or(usize::MAX)
}

#[cfg(test)]
#[path = "../unit_tests/esp32/partition.rs"]
mod tests;
