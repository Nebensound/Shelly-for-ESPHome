//! Unit tests of `crate::discover` (not counted as production code for coverage).

use super::*;

#[test]
fn parses_networks() {
    let network: Network = "10.10.5.77/24".parse().expect("valid");
    assert_eq!(network.to_string(), "10.10.5.0/24");
    assert_eq!(network.hosts().count(), 254);
    assert_eq!(network.hosts().next(), Some(Ipv4Addr::new(10, 10, 5, 1)));
    assert_eq!(network.hosts().last(), Some(Ipv4Addr::new(10, 10, 5, 254)));
    let single: Network = "10.10.5.79".parse().expect("single address");
    assert_eq!(single.hosts().collect::<Vec<_>>(), vec![Ipv4Addr::new(10, 10, 5, 79)]);
    assert_eq!("10.0.0.0/31".parse::<Network>().expect("/31").hosts().count(), 2);
    assert_eq!("10.0.0.0/16".parse::<Network>().expect("/16").hosts().count(), 65_534);
    assert_eq!(Network::around(Ipv4Addr::new(192, 168, 1, 20)).to_string(), "192.168.1.0/24");
    assert!(matches!("10.0.0.0/8".parse::<Network>(), Err(NetworkError::TooLarge(_))));
    for invalid in ["", "10.0.0/24", "10.0.0.0/33", "10.0.0.0/x", "::1/128"] {
        let error = invalid.parse::<Network>().expect_err(invalid);
        assert!(error.to_string().contains("is not an IPv4 network"), "{invalid}");
    }
    assert!(NetworkError::TooLarge("x".into()).to_string().contains("/16"));
}

fn stock(device_type: &str, auth: bool) -> Found {
    classify(ShellyWire {
        device_type: Some(device_type.into()),
        mac: Some("AABBCCDDEEFF".into()),
        auth: Some(auth),
        fw: Some("1.14.0".into()),
        generation: None,
        model: None,
        ver: None,
    })
    .expect("stock")
}

fn migration(state: MigrationState) -> Found {
    Found::Migration(MigrationStatus {
        protocol: 2,
        state,
        received: 0,
        size: 0,
        max_size: 1,
        error: String::new(),
    })
}

#[test]
fn classifies_and_describes_devices() {
    let supported = stock("SHSW-PM", false);
    assert!(supported.can_flash());
    assert_eq!(supported.device(), "Shelly1PM (SHSW-PM)");
    assert_eq!(supported.firmware(), "1.14.0");
    assert_eq!(supported.state(), "can be flashed");
    assert!(stock("SHSW-PM", true).state().contains("--auth"));

    let unsupported = stock("SHHT-1", false);
    assert!(!unsupported.can_flash());
    assert_eq!(unsupported.device(), "SHHT-1");
    assert!(unsupported.state().contains("not supported"));

    let newer = classify(ShellyWire {
        device_type: None,
        mac: Some("AABBCCDDEEFF".into()),
        auth: None,
        fw: None,
        generation: Some(2),
        model: Some("SNSW-001P16EU".into()),
        ver: Some("1.4.4".into()),
    })
    .expect("gen2");
    assert!(!newer.can_flash());
    assert_eq!(newer.device(), "SNSW-001P16EU (Gen2)");
    assert_eq!(newer.firmware(), "1.4.4");
    assert!(newer.state().contains("only Gen1"));

    let waiting = migration(MigrationState::Failed);
    assert!(waiting.can_flash());
    assert_eq!(waiting.device(), "migration firmware");
    assert_eq!(waiting.firmware(), "protocol 2");
    assert!(waiting.state().contains("run flash"));
    let busy = migration(MigrationState::Installing);
    assert!(!busy.can_flash());
    assert!(busy.state().starts_with("busy"));

    let nothing = ShellyWire {
        device_type: None,
        mac: None,
        auth: None,
        fw: None,
        generation: None,
        model: None,
        ver: None,
    };
    assert_eq!(classify(nothing), None);
}
