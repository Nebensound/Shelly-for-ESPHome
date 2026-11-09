//! Protocol version shared by the CLI, `build.rs` and the migration firmware.
//!
//! This file is also included by `build.rs`, so it must stay free of crate
//! imports. `MIGRATION_PROTOCOL` changes together with `MG2X_PROTOCOL_VERSION`
//! and the marker in `migration_firmware/src/main.c`.

/// Protocol version of the migration firmware this tool speaks.
pub const MIGRATION_PROTOCOL: u32 = 2;

/// Prefix of the marker the migration firmware embeds, followed by the
/// protocol number (`shelly2esphome-mg2x-protocol=2`).
pub const MARKER_PREFIX: &[u8] = b"shelly2esphome-mg2x-protocol=";

/// Protocol number following the first marker in `content`, if there is one.
/// A marker without digits yields `Some(0)`.
#[must_use]
pub fn find_marker(content: &[u8]) -> Option<u32> {
    let position =
        content.windows(MARKER_PREFIX.len()).position(|window| window == MARKER_PREFIX)?;
    let digits: String = content[position + MARKER_PREFIX.len()..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .map(|&byte| char::from(byte))
        .collect();
    Some(digits.parse().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_protocol_number() {
        assert_eq!(find_marker(b"\0\0shelly2esphome-mg2x-protocol=2\0rest"), Some(2));
        assert_eq!(find_marker(b"shelly2esphome-mg2x-protocol=13"), Some(13));
        assert_eq!(find_marker(b"shelly2esphome-mg2x-protocol=x"), Some(0));
        assert_eq!(find_marker(b"no marker"), None);
    }
}
