//! Unit tests of `crate::cli` (not counted as production code for coverage).

use super::*;

#[test]
fn default_network_is_the_ipv4_slash_24() {
    let network = default_network(|_| Ok(IpAddr::from([192, 168, 1, 20]))).expect("ipv4");
    assert_eq!(network.to_string(), "192.168.1.0/24");
    let error = default_network(|_| Ok(IpAddr::from([0_u16; 8]))).expect_err("ipv6");
    assert!(error.to_string().contains("pass one like"));
    assert!(default_network(|_| Err(std::io::Error::other("no route"))).is_err());
}

#[test]
fn table_aligns_columns() {
    let rows = [["10.0.0.1".into(), "Shelly1PM (SHSW-PM)".into(), "1.14".into(), "ok".into()]];
    let lines = table(&rows);
    assert_eq!(lines[0], "ADDRESS   DEVICE               FIRMWARE  STATE");
    assert_eq!(lines[1], "10.0.0.1  Shelly1PM (SHSW-PM)  1.14      ok");
}
