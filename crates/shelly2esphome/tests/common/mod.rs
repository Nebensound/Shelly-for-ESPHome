//! Simulated Shelly device and helpers shared by the integration tests.
#![allow(dead_code, missing_docs, clippy::unwrap_used)]

pub mod cloud;
pub mod gen2;

use std::{
    io::{Cursor, Write},
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use shelly2esphome::{
    device::MigrationState,
    embedded::Packages,
    flash::{FlashOptions, Reporter, Timeouts},
    image::{FirmwareImage, MIN_IMAGE_BYTES, synthetic},
    migration::MigrationPackage,
    model::Model,
};
use tokio::net::TcpListener;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Stock,
    Migration,
    Esphome,
}

#[derive(Debug)]
pub struct Fake {
    pub mode: Mode,
    pub device_type: &'static str,
    pub rssi: i32,
    pub auth_required: bool,
    pub shelly_requires_auth: bool,
    pub has_actions: bool,
    pub settings_status: u16,
    pub ignore_ota: bool,
    pub ota_status: u16,
    pub skip_installing: bool,
    pub hang_after_download: bool,
    pub stays_stock_after_ota: bool,
    pub state: MigrationState,
    pub error: String,
    pub max_size: u64,
    pub received: u64,
    pub size: u64,
    pub fail_with: Option<&'static str>,
    pub reject_flash: Option<&'static str>,
    pub open_esphome_port: bool,
    pub esphome_port: u16,
    pub http_port: u16,
    pub ota_requests: u32,
    pub package: Vec<u8>,
    pub image: Vec<u8>,
}

pub type Shared = Arc<Mutex<Fake>>;

/// A GET with a client from `shelly2esphome::http`, which installs the
/// crypto provider.
async fn http_get(url: &str) -> reqwest::Result<reqwest::Response> {
    shelly2esphome::http::builder().build()?.get(url).send().await
}

pub fn fake(mode: Mode) -> Fake {
    Fake {
        mode,
        device_type: "SHDM-2",
        rssi: -60,
        auth_required: false,
        shelly_requires_auth: false,
        has_actions: true,
        settings_status: 200,
        ignore_ota: false,
        ota_status: 200,
        skip_installing: false,
        hang_after_download: false,
        stays_stock_after_ota: false,
        state: MigrationState::Idle,
        error: String::new(),
        max_size: 1_000_000,
        received: 0,
        size: 0,
        fail_with: None,
        reject_flash: None,
        open_esphome_port: true,
        esphome_port: 0,
        http_port: 0,
        ota_requests: 0,
        package: Vec::new(),
        image: Vec::new(),
    }
}

pub async fn shelly(State(shared): State<Shared>) -> Response {
    let fake = shared.lock().unwrap();
    if fake.mode != Mode::Stock {
        return StatusCode::NOT_FOUND.into_response();
    }
    if fake.shelly_requires_auth {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(serde_json::json!({
        "type": fake.device_type, "mac": "E8DB84D41F49", "auth": fake.auth_required,
        "fw": "20230913-114008/v1.14.0-gcb84623"
    }))
    .into_response()
}

pub async fn settings(State(shared): State<Shared>) -> Response {
    let fake = shared.lock().unwrap();
    if fake.mode != Mode::Stock {
        return StatusCode::NOT_FOUND.into_response();
    }
    if fake.auth_required {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if fake.settings_status != 200 {
        return StatusCode::from_u16(fake.settings_status).unwrap().into_response();
    }
    let channels = if fake.device_type.starts_with("SHDM") { "lights" } else { "relays" };
    Json(serde_json::json!({ "name": "test", channels: [{ "name": "ch0" }] })).into_response()
}

pub async fn channel_settings(
    State(shared): State<Shared>,
    Path((kind, index)): Path<(String, u32)>,
) -> Response {
    let fake = shared.lock().unwrap();
    if fake.mode != Mode::Stock {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(serde_json::json!({ "kind": kind, "index": index })).into_response()
}

pub async fn actions(State(shared): State<Shared>) -> Response {
    let fake = shared.lock().unwrap();
    if fake.mode != Mode::Stock || !fake.has_actions {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(serde_json::json!({ "actions": {} })).into_response()
}

pub async fn status(State(shared): State<Shared>) -> Response {
    let fake = shared.lock().unwrap();
    if fake.auth_required {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(serde_json::json!({ "wifi_sta": { "rssi": fake.rssi } })).into_response()
}

#[derive(Deserialize)]
pub struct OtaQuery {
    url: String,
}

pub async fn ota(State(shared): State<Shared>, Query(query): Query<OtaQuery>) -> Response {
    {
        let mut fake = shared.lock().unwrap();
        fake.ota_requests += 1;
        if fake.ota_status != 200 {
            return StatusCode::from_u16(fake.ota_status).unwrap().into_response();
        }
        if fake.ignore_ota {
            return Json(serde_json::json!({ "status": "updating" })).into_response();
        }
    }
    tokio::spawn(async move {
        let package = http_get(&query.url).await.unwrap().bytes().await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await; // reboots
        let mut fake = shared.lock().unwrap();
        fake.package = package.to_vec();
        if !fake.stays_stock_after_ota {
            fake.mode = Mode::Migration;
        }
    });
    Json(serde_json::json!({ "status": "updating" })).into_response()
}

pub async fn mg2x_status(State(shared): State<Shared>) -> Response {
    let fake = shared.lock().unwrap();
    if fake.mode != Mode::Migration {
        return StatusCode::NOT_FOUND.into_response();
    }
    let state = match fake.state {
        MigrationState::Idle => "idle",
        MigrationState::Downloading => "downloading",
        MigrationState::Installing => "installing",
        MigrationState::Failed => "failed",
        MigrationState::RelocationFailed => "relocation_failed",
    };
    Json(serde_json::json!({
        "protocol": 2, "state": state, "received": fake.received, "size": fake.size,
        "max_size": fake.max_size, "error": fake.error
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct FlashRequest {
    url: String,
    size: u64,
    sha256: String,
}

pub async fn mg2x_flash(
    State(shared): State<Shared>,
    Json(request): Json<FlashRequest>,
) -> Response {
    {
        let mut fake = shared.lock().unwrap();
        if fake.mode != Mode::Migration {
            return StatusCode::NOT_FOUND.into_response();
        }
        if let Some(message) = fake.reject_flash {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"code": 400, "message": message})),
            )
                .into_response();
        }
        fake.state = MigrationState::Downloading;
        fake.size = request.size;
    }
    tokio::spawn(async move {
        let image = http_get(&request.url).await.unwrap().bytes().await.unwrap();
        let digest = hex::encode(Sha256::digest(&image));
        let (fail_with, open_port, port, skip_installing) = {
            let mut fake = shared.lock().unwrap();
            fake.received = image.len() as u64;
            fake.image = image.to_vec();
            if fake.hang_after_download {
                return; // stays in "downloading" forever
            }
            (fake.fail_with, fake.open_esphome_port, fake.esphome_port, fake.skip_installing)
        };
        let failure =
            fail_with.or(if digest == request.sha256 && image.len() as u64 == request.size {
                None
            } else {
                Some("hash_mismatch")
            });
        if let Some(reason) = failure {
            let mut fake = shared.lock().unwrap();
            fake.state = MigrationState::Failed;
            fake.error = reason.to_owned();
            return;
        }
        if !skip_installing {
            shared.lock().unwrap().state = MigrationState::Installing;
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        shared.lock().unwrap().mode = Mode::Esphome;
        if open_port {
            let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
            tokio::spawn(async move {
                loop {
                    let _ = listener.accept().await;
                }
            });
        }
    });
    Json(serde_json::json!({ "started": true })).into_response()
}

pub async fn start_fake(mut fake: Fake) -> (String, Shared) {
    // Reserve a port for the ESPHome API; it is opened only after installation.
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    fake.esphome_port = reserved.local_addr().unwrap().port();
    drop(reserved);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    fake.http_port = address.port();
    let shared = Arc::new(Mutex::new(fake));
    let app = Router::new()
        .route("/shelly", get(shelly))
        .route("/status", get(status))
        .route("/settings", get(settings))
        .route("/settings/actions", get(actions))
        .route("/settings/{kind}/{index}", get(channel_settings))
        .route("/ota", get(ota))
        .route("/rpc/MG2X.Status", get(mg2x_status))
        .route("/rpc/MG2X.Flash", post(mg2x_flash))
        .with_state(Arc::clone(&shared));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (address.to_string(), shared)
}

#[derive(Default)]
pub struct Recorder {
    pub messages: Vec<String>,
    pub summaries: Vec<String>,
    pub last_progress: Option<(u64, u64)>,
    pub decline: bool,
    /// Declines from this confirmation on (0 is the first).
    pub decline_from: Option<usize>,
}

impl Reporter for Recorder {
    fn info(&mut self, message: &str) {
        self.messages.push(message.to_owned());
    }
    fn progress(&mut self, sent_bytes: u64, total_bytes: u64) {
        self.last_progress = Some((sent_bytes, total_bytes));
    }
    fn confirm(&mut self, summary: &str) -> bool {
        self.summaries.push(summary.to_owned());
        !self.decline && self.decline_from.is_none_or(|first| self.summaries.len() <= first)
    }
}

pub fn image() -> FirmwareImage {
    FirmwareImage::parse(synthetic::image(MIN_IMAGE_BYTES + 12_345)).unwrap()
}

pub fn package(model: Model) -> MigrationPackage {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer.start_file("fw/manifest.json", zip::write::SimpleFileOptions::default()).unwrap();
    write!(writer, r#"{{"name":"{}","platform":"esp8266"}}"#, model.app_name()).unwrap();
    writer.start_file("fw/app.bin", zip::write::SimpleFileOptions::default()).unwrap();
    writer.write_all(b"\0shelly2esphome-mg2x-protocol=2\0").unwrap();
    MigrationPackage::parse(writer.finish().unwrap().into_inner()).unwrap()
}

/// A fresh, empty directory below the system's temporary directory.
pub fn scratch_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let directory = std::env::temp_dir().join(format!(
        "s2e-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&directory);
    directory
}

pub fn options(shared: &Shared) -> FlashOptions {
    let fake = shared.lock().unwrap();
    FlashOptions {
        serve_ip: Some("127.0.0.1".parse().unwrap()),
        esphome_port: fake.esphome_port,
        migration_port: fake.http_port,
        timeouts: Timeouts {
            package_fetch: Duration::from_secs(5),
            migration_boot: Duration::from_secs(5),
            flash: Duration::from_secs(10),
            esphome_boot: Duration::from_secs(5),
            poll_interval: Duration::from_millis(20),
            slot_probe: Duration::from_secs(2),
            settle: Duration::ZERO,
            package_download: Duration::from_secs(5),
            update_result: Duration::from_secs(2),
        },
        // Independent of how the test binary was built.
        embedded_packages: Packages(&[]),
        backup_dir: Some(scratch_dir()),
        ..FlashOptions::default()
    }
}

/// A device that answers the listed `(path, status, body)` for any method and
/// 404 otherwise. Returns its `host:port` and the `Authorization` headers seen.
pub async fn scripted(
    routes: &[(&'static str, u16, &'static str)],
) -> (String, Arc<Mutex<Vec<Option<String>>>>) {
    use axum::{extract::Request, http::header};
    let routes: Arc<Vec<(&'static str, u16, &'static str)>> = Arc::new(routes.to_vec());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_by_handler = Arc::clone(&seen);
    let app = Router::new().fallback(move |request: Request| {
        let routes = Arc::clone(&routes);
        let seen = Arc::clone(&seen_by_handler);
        async move {
            let auth = request
                .headers()
                .get(header::AUTHORIZATION)
                .map(|value| value.to_str().unwrap().to_owned());
            seen.lock().unwrap().push(auth);
            match routes.iter().find(|(path, _, _)| *path == request.uri().path()) {
                Some((_, status, body)) => (
                    StatusCode::from_u16(*status).unwrap(),
                    [(header::CONTENT_TYPE, "application/json")],
                    *body,
                )
                    .into_response(),
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (address.to_string(), seen)
}

/// Answers the first `connections` connections with the raw bytes `response`
/// and closes each; afterwards nothing listens on the port any more.
pub async fn raw(response: &'static [u8], connections: usize) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for _ in 0..connections {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).await;
            let _ = stream.write_all(response).await;
            let _ = stream.shutdown().await;
        }
    });
    address.to_string()
}

/// A `host:port` on which nothing listens.
/// An address nobody listens on. The port stays bound, without listening,
/// for the whole test process: a port that was only probed and released can
/// be taken by a test running in parallel, and then answers.
pub fn closed_port() -> String {
    static RESERVED: std::sync::OnceLock<(tokio::net::TcpSocket, String)> =
        std::sync::OnceLock::new();
    let (_, address) = RESERVED.get_or_init(|| {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = socket.local_addr().unwrap().to_string();
        (socket, address)
    });
    address.clone()
}
