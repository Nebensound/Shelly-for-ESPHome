//! Unit tests of `crate::device` (not counted as production code for coverage).

use super::*;

#[test]
fn credentials_parse_and_hide_the_password() {
    let credentials: Credentials = "admin:secret:with:colon".parse().expect("valid");
    assert_eq!(credentials.user, "admin");
    assert_eq!(credentials.password, "secret:with:colon");
    assert!(!format!("{credentials:?}").contains("secret"));
    assert!("no-colon".parse::<Credentials>().is_err());
    assert!(":password-only".parse::<Credentials>().is_err());
}

#[test]
fn migration_status_parses_the_firmware_answer() {
    let status: MigrationStatus = serde_json::from_str(
        r#"{"protocol":2,"state":"failed","received":5,"size":10,"max_size":20,"error":"hash_mismatch"}"#,
    )
    .expect("valid");
    assert_eq!(status.state, MigrationState::Failed);
    assert_eq!(status.error, "hash_mismatch");
}

#[test]
fn error_messages_are_readable() {
    assert!(DeviceError::AuthRequired.to_string().contains("login"));
    assert!(DeviceError::UnknownFirmware.to_string().contains("unknown firmware"));
    assert!(DeviceError::UnsupportedProtocol(2).to_string().contains("protocol 2"));
    assert!(DeviceError::Rejected("busy".into()).to_string().contains("busy"));
    assert!(DeviceError::Status { endpoint: "/ota", status: 500 }.to_string().contains("HTTP 500"));
}
