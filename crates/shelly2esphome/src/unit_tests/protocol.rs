//! Unit tests of `crate::protocol` (not counted as production code for coverage).

use super::*;

#[test]
fn finds_the_protocol_number() {
    assert_eq!(find_marker(b"\0\0shelly2esphome-mg2x-protocol=2\0rest"), Some(2));
    assert_eq!(find_marker(b"shelly2esphome-mg2x-protocol=13"), Some(13));
    assert_eq!(find_marker(b"shelly2esphome-mg2x-protocol=x"), Some(0));
    assert_eq!(find_marker(b"no marker"), None);
}
