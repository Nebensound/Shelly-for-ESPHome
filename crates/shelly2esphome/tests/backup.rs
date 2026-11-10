//! `backup::collect` against scripted devices: every request can fail.
#![allow(missing_docs, clippy::unwrap_used)]

mod common;

use common::scripted;
use shelly2esphome::{
    backup::{self, BackupError},
    device::{DeviceClient, DeviceError},
};

const STOCK: &str = r#"{"type":"SHSW-25","mac":"AABBCCDDEEFF","auth":false,"fw":"1.14.0"}"#;
const SETTINGS: &str = r#"{"relays":[{},{}],"rollers":[{}]}"#;

async fn collect(
    routes: &[(&'static str, u16, &'static str)],
) -> Result<backup::Backup, BackupError> {
    let (host, _) = scripted(routes).await;
    backup::collect(&DeviceClient::new(&host, None)).await
}

#[tokio::test]
async fn saves_every_channel_and_skips_missing_endpoints() {
    let saved = collect(&[
        ("/shelly", 200, STOCK),
        ("/settings", 200, SETTINGS),
        ("/settings/relay/0", 200, "{}"),
        // relay 1 and /status are missing (404) and skipped.
        ("/settings/roller/0", 200, "{}"),
        ("/settings/actions", 200, "{}"),
    ])
    .await
    .unwrap();
    let names: Vec<&str> = saved.files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "shelly.json",
            "settings-relay-0.json",
            "settings-roller-0.json",
            "settings-actions.json",
            "settings.json"
        ]
    );
}

#[tokio::test]
async fn reports_each_failing_request() {
    let error = collect(&[("/shelly", 401, "")]).await.unwrap_err();
    assert!(matches!(error, BackupError::Device(DeviceError::AuthRequired)));

    // A Gen2 answer is no stock firmware.
    let error = collect(&[("/shelly", 200, r#"{"gen":2}"#)]).await.unwrap_err();
    assert!(matches!(error, BackupError::NotStock));

    let base = [("/shelly", 200, STOCK), ("/settings", 200, SETTINGS)];
    for failing in ["/status", "/settings/relay/0", "/settings/actions"] {
        let mut routes = base.to_vec();
        routes.push((failing, 500, ""));
        let error = collect(&routes).await.unwrap_err();
        assert!(error.to_string().contains("answered with HTTP 500"), "{failing}: {error}");
    }
}
