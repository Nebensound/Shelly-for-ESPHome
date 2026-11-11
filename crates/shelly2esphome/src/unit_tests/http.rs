//! Unit tests of `crate::http` (not counted as production code for coverage).

use super::*;

#[test]
fn builds_clients_after_installing_the_provider() {
    // A second call finds the provider installed and still works.
    assert!(builder().build().is_ok());
    assert!(builder().build().is_ok());
    assert!(rustls::crypto::CryptoProvider::get_default().is_some());
}
