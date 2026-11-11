//! Unit tests of `crate::rpc` (not counted as production code for coverage).

use super::*;

#[test]
fn generations_start_at_two() {
    assert!(Generation::try_from(1).expect_err("generation 1").contains("generation 1"));
    assert_eq!(Generation::try_from(3), Ok(Generation(3)));
    assert_eq!(Generation(3).to_string(), "Gen3");
}

#[test]
fn reads_the_signal_only_while_connected() {
    let signal = |json: &str| serde_json::from_str::<WifiStatusWire>(json).expect(json).signal();
    assert_eq!(signal(r#"{"status":"got ip","rssi":-63}"#), WifiSignal::Dbm(-63));
    assert_eq!(signal(r#"{"status":"connected","rssi":-70}"#), WifiSignal::Dbm(-70));
    // A Pro device on Ethernet: rssi reads 0 or is missing.
    assert_eq!(signal(r#"{"status":"disconnected","rssi":0}"#), WifiSignal::NotConnected);
    assert_eq!(signal(r#"{"status":"disconnected"}"#), WifiSignal::NotConnected);
    assert_eq!(signal(r#"{"status":"got ip"}"#), WifiSignal::NotConnected);
}

#[test]
fn recognises_beta_firmware() {
    assert!(FirmwareVersion("2.2.0-beta2".into()).is_beta());
    assert!(!FirmwareVersion("2.0.3".into()).is_beta());
    assert_eq!(FirmwareVersion("1.7.6".into()).to_string(), "1.7.6");
}

#[test]
fn reads_the_answer_of_a_gen3_device() {
    // Recorded from a Shelly 1 Gen3 on 2.0.3.
    let info: DeviceInfo = serde_json::from_str(
        r#"{"name":"Licht","id":"shelly1g3-dcda0cb2e7f4","mac":"DCDA0CB2E7F4","slot":0,
            "model":"S3SW-001X16EU","gen":3,"fw_id":"20261026-122250/2.0.3-g4601943",
            "ver":"2.0.3","app":"S1G3","auth_en":false,"auth_domain":null}"#,
    )
    .expect("gen3");
    assert_eq!(info.generation, Generation(3));
    assert_eq!(info.slot, Some(0));
    assert_eq!(info.app.as_str(), "S1G3");
    assert_eq!(info.model.to_string(), "S3SW-001X16EU");
    assert_eq!(info.firmware.as_str(), "2.0.3");
    assert!(!info.auth);

    let minimal: DeviceInfo = serde_json::from_str(
        r#"{"id":"x","mac":"AA","model":"SNSW-001P16EU","gen":2,"ver":"1.4.4","app":"Plus1PM"}"#,
    )
    .expect("without slot and auth");
    assert_eq!(minimal.slot, None);
    assert!(!minimal.auth);

    assert!(serde_json::from_str::<DeviceInfo>(r#"{"type":"SHSW-PM","gen":1}"#).is_err());
}
