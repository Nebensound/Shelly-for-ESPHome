//! `DeviceClient` against scripted and broken devices.
#![allow(missing_docs, clippy::unwrap_used)]

mod common;

use common::{closed_port, raw, scripted};
use shelly2esphome::device::{Credentials, DeviceClient, DeviceError, Firmware};

const STOCK: &str = r#"{"type":"SHSW-PM","mac":"AABBCCDDEEFF","auth":true,"fw":"1.14.0"}"#;
const STATUS: &str =
    r#"{"protocol":2,"state":"idle","received":0,"size":0,"max_size":1,"error":""}"#;

fn client(host: &str) -> DeviceClient {
    DeviceClient::new(host, None)
}

#[tokio::test]
async fn sends_the_login_when_given() {
    let (host, seen) = scripted(&[("/settings", 200, "{}"), ("/ota", 200, "{}")]).await;
    let credentials: Credentials = "admin:secret".parse().unwrap();
    let client = DeviceClient::new(&host, Some(credentials));
    client.get_json("/settings", "/settings").await.unwrap();
    client.start_stock_ota("http://127.0.0.1/package.zip").await.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    // base64("admin:secret")
    assert!(seen.iter().all(|auth| auth.as_deref() == Some("Basic YWRtaW46c2VjcmV0")));
}

#[tokio::test]
async fn get_json_maps_every_answer() {
    let (host, _) = scripted(&[
        ("/ok", 200, r#"{"a":1}"#),
        ("/garbage", 200, "not json"),
        ("/login", 401, ""),
        ("/broken", 500, ""),
    ])
    .await;
    let client = client(&host);
    assert_eq!(client.get_json("/ok", "/ok").await.unwrap(), Some(serde_json::json!({"a": 1})));
    assert_eq!(client.get_json("/missing", "/missing").await.unwrap(), None);
    assert!(matches!(client.get_json("/garbage", "/garbage").await, Err(DeviceError::Http(_))));
    assert!(matches!(client.get_json("/login", "/login").await, Err(DeviceError::AuthRequired)));
    let error = client.get_json("/broken", "/broken").await.unwrap_err();
    assert!(matches!(error, DeviceError::Status { endpoint: "/broken", status: 500 }));
    let gone = DeviceClient::new(&closed_port(), None);
    assert!(matches!(gone.get_json("/x", "/x").await, Err(DeviceError::Http(_))));
}

#[tokio::test]
async fn identify_reports_unreachable_and_unknown_devices() {
    assert!(matches!(client(&closed_port()).identify().await, Err(DeviceError::Http(_))));

    // /shelly answers something else, MG2X.Status is no status either.
    let (host, _) =
        scripted(&[("/shelly", 200, r#"{"gen":2}"#), ("/rpc/MG2X.Status", 200, "garbage")]).await;
    assert!(matches!(client(&host).identify().await, Err(DeviceError::UnknownFirmware)));

    // /shelly says 404 and the device is gone before MG2X.Status.
    let host =
        raw(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", 1).await;
    assert!(matches!(client(&host).identify().await, Err(DeviceError::Http(_))));
}

#[tokio::test]
async fn identify_reads_the_signal_of_the_stock_firmware() {
    let (host, _) = scripted(&[("/shelly", 200, STOCK), ("/status", 200, "garbage")]).await;
    assert!(matches!(client(&host).identify().await, Err(DeviceError::Http(_))));

    let (host, _) = scripted(&[("/shelly", 200, STOCK), ("/status", 503, "")]).await;
    let error = client(&host).identify().await.unwrap_err();
    assert!(matches!(error, DeviceError::Status { endpoint: "/status", status: 503 }));

    // The device answers /shelly and is gone before /status.
    let response: &'static [u8] = Box::leak(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{STOCK}",
            STOCK.len()
        )
        .into_bytes()
        .into_boxed_slice(),
    );
    let host = raw(response, 1).await;
    assert!(matches!(client(&host).identify().await, Err(DeviceError::Http(_))));

    let (host, _) =
        scripted(&[("/shelly", 200, STOCK), ("/status", 200, r#"{"wifi_sta":{"rssi":-61}}"#)])
            .await;
    let Firmware::Stock { info, rssi_dbm } = client(&host).identify().await.unwrap() else {
        panic!("stock firmware expected");
    };
    assert!(info.auth);
    assert_eq!(rssi_dbm, -61);
}

#[tokio::test]
async fn stock_ota_maps_every_answer() {
    let invalid = client("bad host");
    let error = invalid.start_stock_ota("http://x/p.zip").await.unwrap_err();
    assert!(error.to_string().contains("invalid device address"));

    assert!(matches!(
        client(&closed_port()).start_stock_ota("http://x/p.zip").await,
        Err(DeviceError::Http(_))
    ));
    let (host, _) = scripted(&[("/ota", 401, "")]).await;
    assert!(matches!(
        client(&host).start_stock_ota("http://x/p.zip").await,
        Err(DeviceError::AuthRequired)
    ));
    let (host, _) = scripted(&[("/ota", 500, "")]).await;
    let error = client(&host).start_stock_ota("http://x/p.zip").await.unwrap_err();
    assert!(matches!(error, DeviceError::Status { endpoint: "/ota", status: 500 }));
}

#[tokio::test]
async fn migration_status_checks_the_protocol() {
    assert!(matches!(client(&closed_port()).migration_status().await, Err(DeviceError::Http(_))));
    let (host, _) = scripted(&[]).await;
    assert_eq!(client(&host).migration_status().await.unwrap(), None);
    let (host, _) = scripted(&[(
        "/rpc/MG2X.Status",
        200,
        r#"{"protocol":1,"state":"idle","received":0,"size":0,"max_size":1,"error":""}"#,
    )])
    .await;
    let error = client(&host).migration_status().await.unwrap_err();
    assert!(matches!(error, DeviceError::UnsupportedProtocol(1)));
    let (host, _) = scripted(&[("/rpc/MG2X.Status", 200, STATUS)]).await;
    assert!(client(&host).migration_status().await.unwrap().is_some());
}

#[tokio::test]
async fn start_migration_maps_every_answer() {
    let start = |host: String| async move {
        client(&host).start_migration("http://127.0.0.1/firmware.bin", 4096, "00").await
    };
    assert!(matches!(start(closed_port()).await, Err(DeviceError::Http(_))));

    // The body ends before its announced length.
    let host =
        raw(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{\"st", 1).await;
    assert!(matches!(start(host).await, Err(DeviceError::Http(_))));

    let (host, _) = scripted(&[("/rpc/MG2X.Flash", 400, r#"{"code":400,"message":"busy"}"#)]).await;
    assert!(matches!(start(host).await, Err(DeviceError::Rejected(ref m)) if m == "busy"));

    let (host, _) = scripted(&[("/rpc/MG2X.Flash", 502, "bad gateway")]).await;
    let error = start(host).await.unwrap_err();
    assert!(matches!(error, DeviceError::Status { endpoint: "/rpc/MG2X.Flash", status: 502 }));

    let (host, _) = scripted(&[("/rpc/MG2X.Flash", 200, r#"{"started":false}"#)]).await;
    assert!(
        matches!(start(host).await, Err(DeviceError::Rejected(ref m)) if m == "connect_failed")
    );

    let (host, _) = scripted(&[("/rpc/MG2X.Flash", 200, "{}")]).await;
    let error = start(host).await.unwrap_err();
    assert!(error.to_string().contains("unexpected answer"));

    let (host, _) = scripted(&[("/rpc/MG2X.Flash", 200, r#"{"started":true}"#)]).await;
    start(host).await.unwrap();
}
