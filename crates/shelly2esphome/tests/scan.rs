//! `discover::scan` against simulated devices on 127.0.0.1 (one port each, so
//! the test also runs on macOS, which only has 127.0.0.1).
#![allow(missing_docs, clippy::unwrap_used)]

use std::net::{IpAddr, Ipv4Addr};

use axum::{Json, Router, http::StatusCode, routing::get};
use shelly2esphome::discover::{Found, Network, ScanOptions, scan};

async fn serve(router: Router) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    port
}

async fn scan_port(port: u16) -> Vec<(IpAddr, Found)> {
    // The same address twice: it must be asked only once.
    let networks: Vec<Network> =
        vec!["127.0.0.1".parse().unwrap(), "127.0.0.1/32".parse().unwrap()];
    scan(&networks, &ScanOptions { port, ..ScanOptions::default() }).await
}

#[tokio::test]
async fn finds_stock_gen1_devices() {
    let port = serve(Router::new().route(
        "/shelly",
        get(|| async {
            Json(serde_json::json!({
                "type": "SHSW-PM", "mac": "AABBCCDDEEFF", "auth": false, "fw": "1.14.0"
            }))
        }),
    ))
    .await;
    let found = scan_port(port).await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert!(matches!(found[0].1, Found::Stock { model: Some(_), .. }));
}

#[tokio::test]
async fn finds_newer_generations() {
    let port = serve(Router::new().route(
        "/shelly",
        get(|| async {
            Json(serde_json::json!({
                "id": "shellyplus1pm-x", "mac": "AABBCCDDEE00", "model": "SNSW-001P16EU",
                "gen": 2, "ver": "1.4.4", "app": "Plus1PM"
            }))
        }),
    ))
    .await;
    let found = scan_port(port).await;
    assert!(matches!(found[0].1, Found::Newer { generation: 2, .. }));
}

#[tokio::test]
async fn finds_a_waiting_migration_firmware() {
    let port =
        serve(Router::new().route("/shelly", get(|| async { StatusCode::NOT_FOUND })).route(
            "/rpc/MG2X.Status",
            get(|| async {
                Json(serde_json::json!({
                    "protocol": 2, "state": "idle", "received": 0, "size": 0,
                    "max_size": 991232, "error": ""
                }))
            }),
        ))
        .await;
    let found = scan_port(port).await;
    assert!(matches!(found[0].1, Found::Migration(_)));
}

#[tokio::test]
async fn ignores_everything_else() {
    // A web server with neither /shelly nor MG2X.Status.
    assert!(scan_port(serve(Router::new()).await).await.is_empty());
    // A /shelly that is not a Shelly answer.
    let odd = Router::new().route("/shelly", get(|| async { Json(serde_json::json!({"x": 1})) }));
    assert!(scan_port(serve(odd).await).await.is_empty());
    // A /shelly that is not JSON at all.
    let text = Router::new().route("/shelly", get(|| async { "hello" }));
    assert!(scan_port(serve(text).await).await.is_empty());
    // An MG2X.Status that is not a status.
    let bad_status = Router::new()
        .route("/rpc/MG2X.Status", get(|| async { Json(serde_json::json!({"protocol": "x"})) }));
    assert!(scan_port(serve(bad_status).await).await.is_empty());
    // Nothing listening at all: take a free port and release it again.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed = listener.local_addr().unwrap().port();
    drop(listener);
    assert!(scan_port(closed).await.is_empty());
}

#[tokio::test]
async fn ignores_a_device_that_vanishes_between_requests() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // Answers /shelly with 404 once, then nothing listens any more.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await;
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
    });
    assert!(scan_port(port).await.is_empty());
}
