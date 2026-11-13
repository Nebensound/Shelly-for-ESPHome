//! A simulated Shelly Gen3 with the stock firmware: `/shelly`, the RPC with
//! digest authentication, the debug log over UDP and the stock updater.
#![allow(dead_code, clippy::unwrap_used)]

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;

pub const PASSWORD: &str = "secret";
const REALM: &str = "shelly1g3-test";
const NONCE: &str = "1700000000";

/// How the simulated device behaves.
#[derive(Debug)]
pub struct Gen2 {
    pub app: &'static str,
    pub firmware: &'static str,
    pub auth: bool,
    /// `None`: on Ethernet.
    pub rssi: Option<i32>,
    pub uptime: u64,
    /// What the log names right after it is switched on; `None` names nothing.
    pub core_dump: Option<&'static str>,
    /// Whether the log reaches this computer at all.
    pub send_log: bool,
    pub refuse_update: Option<&'static str>,
    /// Whether the updater downloads the package.
    pub download: bool,
    pub will_write: u8,
    pub bootloader_update: bool,
    /// Whether the installer's bootloader decision reaches the log.
    pub boot_line: bool,
    /// Hands out script code with a character cut in half.
    pub garbled_script: bool,
    pub report_success: bool,
    pub open_esphome_port: bool,
    pub without_scripts: bool,
    /// Answers `Wifi.GetStatus` with something that is no status.
    pub broken_wifi: bool,
    /// Refuses to put the log setting back.
    pub fail_restore: bool,
    /// Answers this method with HTTP 500.
    pub fail_method: Option<&'static str>,
    /// Logs a line of an earlier update before the core dump target.
    pub stale_line: bool,
    pub log_addr: serde_json::Value,
    pub esphome_port: u16,
    /// Every RPC method and every change of the log setting, in order.
    pub requests: Vec<String>,
    pub downloaded: Vec<u8>,
}

pub type Shared = Arc<Mutex<Gen2>>;

pub fn gen2() -> Gen2 {
    Gen2 {
        app: "S1G3",
        firmware: "2.0.3",
        auth: false,
        rssi: Some(-60),
        uptime: 3600,
        core_dump: Some("app_0 (0x20000)"),
        send_log: true,
        refuse_update: None,
        download: true,
        will_write: 0,
        bootloader_update: true,
        boot_line: true,
        garbled_script: false,
        report_success: true,
        open_esphome_port: true,
        without_scripts: false,
        broken_wifi: false,
        fail_restore: false,
        fail_method: None,
        stale_line: false,
        log_addr: serde_json::Value::Null,
        esphome_port: 0,
        requests: Vec::new(),
        downloaded: Vec::new(),
    }
}

async fn shelly(State(shared): State<Shared>) -> Response {
    let device = shared.lock().unwrap();
    Json(serde_json::json!({
        "id": "shelly1g3-aabbccddeeff", "mac": "AABBCCDDEEFF", "model": "S3SW-001X16EU",
        "gen": 3, "fw_id": format!("20261026-125850/{}-gb8c68b1", device.firmware),
        "ver": device.firmware, "app": device.app, "auth_en": device.auth, "slot": 1
    }))
    .into_response()
}

/// `GET /rpc/Wifi.GetStatus`, which `identify` reads without a login.
async fn wifi(State(shared): State<Shared>) -> Response {
    let device = shared.lock().unwrap();
    if device.auth {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(serde_json::json!({ "status": "got ip", "rssi": device.rssi.unwrap_or(-60) }))
        .into_response()
}

fn sha256_hex(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Whether `headers` carry a valid digest login for `admin`.
fn logged_in(headers: &HeaderMap) -> bool {
    let Some(header) = headers.get("authorization").and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let field = |name: &str| {
        let start = header.find(&format!("{name}="))? + name.len() + 1;
        let rest = header[start..].trim_start_matches('"');
        Some(rest.split(['"', ',']).next().unwrap_or_default().to_owned())
    };
    let (Some(user), Some(uri), Some(count), Some(cnonce), Some(response)) =
        (field("username"), field("uri"), field("nc"), field("cnonce"), field("response"))
    else {
        return false;
    };
    let secret = sha256_hex(&format!("admin:{REALM}:{PASSWORD}"));
    let request = sha256_hex(&format!("POST:{uri}"));
    let expected = sha256_hex(&format!("{secret}:{NONCE}:{count}:{cnonce}:auth:{request}"));
    user == "admin" && response == expected
}

fn send_log(addr: &str, lines: &[String]) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    for line in lines {
        let _ = socket.send_to(line.as_bytes(), addr);
    }
}

fn reply(result: serde_json::Value) -> Response {
    Json(serde_json::json!({ "id": 1, "result": result })).into_response()
}

fn refused(code: i64, message: &str) -> Response {
    Json(serde_json::json!({ "id": 1, "error": { "code": code, "message": message } }))
        .into_response()
}

/// The updater: downloads the package, logs its decisions and starts ESPHome.
async fn install(shared: Shared, url: String) {
    let (download, addr, lines, open, port) = {
        let device = shared.lock().unwrap();
        let mut lines = vec![format!("shos_ota Will write to slot {}", device.will_write)];
        if device.boot_line {
            lines.push(format!(
                "shos_ota Boot: cur 010002ff, new 00000000, min 010009ff, update? {}",
                u8::from(device.bootloader_update)
            ));
        }
        if device.report_success {
            lines.push("shos_ota Update succeeded".to_owned());
        }
        (
            device.download,
            device.log_addr.as_str().filter(|_| device.send_log).map(str::to_owned),
            lines,
            device.open_esphome_port,
            device.esphome_port,
        )
    };
    if !download {
        return;
    }
    let client = shelly2esphome::http::builder().build().unwrap();
    let body = client.get(&url).send().await.unwrap().bytes().await.unwrap();
    shared.lock().unwrap().downloaded = body.to_vec();
    if let Some(addr) = addr {
        send_log(&addr, &lines);
    }
    if open {
        let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        tokio::spawn(async move {
            loop {
                let _ = listener.accept().await;
            }
        });
    }
}

async fn rpc(State(shared): State<Shared>, headers: HeaderMap, body: String) -> Response {
    let request: serde_json::Value = serde_json::from_str(&body).unwrap();
    let method = request["method"].as_str().unwrap_or_default().to_owned();
    let params = request["params"].clone();
    let mut device = shared.lock().unwrap();
    if device.auth && !logged_in(&headers) {
        let challenge =
            format!("Digest qop=\"auth\", realm=\"{REALM}\", nonce=\"{NONCE}\", algorithm=SHA-256");
        return (StatusCode::UNAUTHORIZED, [(axum::http::header::WWW_AUTHENTICATE, challenge)])
            .into_response();
    }
    device.requests.push(method.clone());
    if device.fail_method == Some(method.as_str()) {
        return (StatusCode::INTERNAL_SERVER_ERROR, "broken").into_response();
    }
    match method.as_str() {
        "Wifi.GetStatus" if device.broken_wifi => reply(serde_json::json!(42)),
        "Wifi.GetStatus" => reply(match device.rssi {
            Some(rssi) => serde_json::json!({ "status": "got ip", "rssi": rssi }),
            None => serde_json::json!({ "status": "disconnected" }),
        }),
        "Sys.GetStatus" => reply(serde_json::json!({ "uptime": device.uptime })),
        "Shelly.GetDeviceInfo" => reply(serde_json::json!({ "app": device.app })),
        "Shelly.GetConfig" => reply(serde_json::json!({ "switch:0": { "name": "lamp" } })),
        "Schedule.List" => reply(serde_json::json!({ "jobs": [] })),
        "Webhook.List" => reply(serde_json::json!({ "hooks": [] })),
        "Script.List" | "KVS.GetMany" if device.without_scripts => refused(-114, "No such method"),
        "Script.List" => reply(serde_json::json!({ "scripts": [{ "id": 1, "name": "blink" }] })),
        "Script.GetCode" if device.garbled_script => {
            reply(serde_json::json!({ "data": "// gr\u{FFFD}n", "left": 0 }))
        }
        "Script.GetCode" => {
            let code = "let on = true;\nShelly.call('Switch.Toggle', {id: 0});\n";
            let offset = usize::try_from(params["offset"].as_u64().unwrap()).unwrap();
            let end = code.len().min(offset + 16);
            reply(serde_json::json!({ "data": &code[offset..end], "left": code.len() - end }))
        }
        "KVS.GetMany" => {
            let all = ["a", "b", "c"];
            let offset = usize::try_from(params["offset"].as_u64().unwrap()).unwrap();
            let items: Vec<_> = all
                .iter()
                .skip(offset)
                .take(2)
                .map(|key| serde_json::json!({ "key": key, "value": "1" }))
                .collect();
            reply(serde_json::json!({ "items": items, "offset": offset, "total": all.len() }))
        }
        "Sys.GetConfig" => {
            reply(serde_json::json!({ "debug": { "udp": { "addr": device.log_addr } } }))
        }
        "Sys.SetConfig" => {
            let addr = params["config"]["debug"]["udp"]["addr"].clone();
            let switching_on = addr.as_str().is_some();
            if device.fail_restore && !switching_on && device.log_addr.is_string() {
                return refused(-103, "busy");
            }
            device.requests.push(format!("log -> {addr}"));
            device.log_addr = addr.clone();
            if let (Some(addr), true) = (addr.as_str(), device.send_log) {
                let mut lines = vec!["shelly_debug.cpp:236 Streaming logs to 127.0.0.1".to_owned()];
                if device.stale_line {
                    lines.push("shos_ota Update succeeded".to_owned());
                }
                if let Some(target) = device.core_dump {
                    lines.push(format!("shos_ota_esp32_back:949 Storing core dumps to {target}"));
                }
                send_log(addr, &lines);
            }
            reply(serde_json::json!({ "restart_required": false }))
        }
        "Shelly.Update" => {
            if let Some(message) = device.refuse_update {
                return refused(-109, message);
            }
            let url = params["url"].as_str().unwrap_or_default().to_owned();
            tokio::spawn(install(Arc::clone(&shared), url));
            reply(serde_json::Value::Null)
        }
        _ => refused(-114, "No such method"),
    }
}

/// Starts the device; returns its `host:port` and its state.
pub async fn start(mut device: Gen2) -> (String, Shared) {
    // A port for the ESPHome API; it is opened only after the update.
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    device.esphome_port = reserved.local_addr().unwrap().port();
    drop(reserved);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    let shared = Arc::new(Mutex::new(device));
    let app = Router::new()
        .route("/shelly", get(shelly))
        .route("/rpc", post(rpc))
        .route("/rpc/Wifi.GetStatus", get(wifi))
        .with_state(Arc::clone(&shared));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (address.to_string(), shared)
}

/// A free UDP port for the device log.
pub fn log_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}
