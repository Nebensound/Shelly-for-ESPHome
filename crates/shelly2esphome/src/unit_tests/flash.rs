//! Unit tests of `crate::flash` (not counted as production code for coverage).

use super::*;

#[tokio::test]
async fn resolves_all_host_forms() {
    assert_eq!(resolve("10.10.5.78").await.expect("ip").to_string(), "10.10.5.78:80");
    assert_eq!(resolve("10.10.5.78:8080").await.expect("ip:port").port(), 8080);
    assert_eq!(resolve("localhost").await.expect("name").port(), 80);
    assert_eq!(resolve("localhost:81").await.expect("name:port").port(), 81);
    assert!(resolve("does-not-exist.invalid").await.is_err());
}

#[test]
fn local_ip_for_loopback_is_loopback() {
    let ip = local_ip_towards(IpAddr::from([127, 0, 0, 1])).expect("route");
    assert!(ip.is_loopback());
}

#[test]
fn default_options_are_conservative() {
    let options = FlashOptions::default();
    assert_eq!(options.min_rssi_dbm, -75);
    assert!(!options.allow_weak_signal);
    assert_eq!(options.esphome_port, 6053);
}

#[test]
fn uses_an_ipv6_socket_for_ipv6_targets() {
    // Hosts without IPv6 refuse the socket; otherwise the answer is IPv6.
    if let Ok(ip) = local_ip_towards(IpAddr::from([0_u16, 0, 0, 0, 0, 0, 0, 1])) {
        assert!(ip.is_ipv6());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn reports_when_no_local_address_can_reach_the_target() {
    // Linux refuses to connect a UDP socket to the broadcast address.
    assert!(local_ip_towards(IpAddr::from([255, 255, 255, 255])).is_err());
}
