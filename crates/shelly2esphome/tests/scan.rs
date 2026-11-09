//! `discover::scan` against simulated devices on loopback addresses.
#![allow(missing_docs, clippy::unwrap_used)]

use std::net::{Ipv4Addr, SocketAddr};

use axum::{Json, Router, http::StatusCode, routing::get};
use shelly2esphome::discover::{Found, Network, ScanOptions, scan};

async fn serve(ip: Ipv4Addr, port: u16, router: Router) -> u16 {
    let listener = tokio::net::TcpListener::bind(SocketAddr::from((ip, port))).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    port
}

#[tokio::test]
async fn finds_and_classifies_shellys() {
    let stock = Router::new().route(
        "/shelly",
        get(|| async {
            Json(serde_json::json!({
                "type": "SHSW-PM", "mac": "AABBCCDDEEFF", "auth": false, "fw": "1.14.0"
            }))
        }),
    );
    let port = serve(Ipv4Addr::new(127, 0, 0, 2), 0, stock).await;
    let newer = Router::new().route(
        "/shelly",
        get(|| async {
            Json(serde_json::json!({
                "id": "shellyplus1pm-x", "mac": "AABBCCDDEE00", "model": "SNSW-001P16EU",
                "gen": 2, "ver": "1.4.4", "app": "Plus1PM"
            }))
        }),
    );
    serve(Ipv4Addr::new(127, 0, 0, 3), port, newer).await;
    let migration = Router::new().route("/shelly", get(|| async { StatusCode::NOT_FOUND })).route(
        "/rpc/MG2X.Status",
        get(|| async {
            Json(serde_json::json!({
                "protocol": 2, "state": "idle", "received": 0, "size": 0,
                "max_size": 991232, "error": ""
            }))
        }),
    );
    serve(Ipv4Addr::new(127, 0, 0, 4), port, migration).await;
    // Some other web server: neither /shelly nor MG2X.Status.
    serve(Ipv4Addr::new(127, 0, 0, 5), port, Router::new()).await;
    // A web server whose /shelly is no Shelly answer.
    let other = Router::new().route("/shelly", get(|| async { Json(serde_json::json!({"x": 1})) }));
    serve(Ipv4Addr::new(127, 0, 0, 6), port, other).await;

    let networks: Vec<Network> =
        vec!["127.0.0.0/29".parse().unwrap(), "127.0.0.2".parse().unwrap()];
    let options = ScanOptions { port, ..ScanOptions::default() };
    let found = scan(&networks, &options).await.unwrap();
    let addresses: Vec<String> = found.iter().map(|(ip, _)| ip.to_string()).collect();
    assert_eq!(addresses, ["127.0.0.2", "127.0.0.3", "127.0.0.4"]);
    assert!(matches!(found[0].1, Found::Stock { model: Some(_), .. }));
    assert!(matches!(found[1].1, Found::Newer { generation: 2, .. }));
    assert!(matches!(found[2].1, Found::Migration(_)));
}
