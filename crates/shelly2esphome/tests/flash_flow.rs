//! End-to-end tests of the flash procedure against a simulated Shelly device.
#![allow(missing_docs, clippy::unwrap_used)]

use std::{
    io::{Cursor, Write},
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use shelly2esphome::{
    device::{DeviceClient, DeviceError, MigrationState},
    flash::{self, FlashError, FlashOptions, Reporter, Timeouts},
    image::{FirmwareImage, MIN_IMAGE_BYTES, synthetic},
    migration::{MigrationPackage, PackageError},
    model::Model,
};
use tokio::net::TcpListener;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Stock,
    Migration,
    Esphome,
}

#[derive(Debug)]
struct Fake {
    mode: Mode,
    device_type: &'static str,
    rssi: i32,
    auth_required: bool,
    ignore_ota: bool,
    stays_stock_after_ota: bool,
    state: MigrationState,
    error: String,
    max_size: u64,
    received: u64,
    size: u64,
    fail_with: Option<&'static str>,
    reject_flash: Option<&'static str>,
    open_esphome_port: bool,
    esphome_port: u16,
    http_port: u16,
    ota_requests: u32,
    package: Vec<u8>,
    image: Vec<u8>,
}

type Shared = Arc<Mutex<Fake>>;

fn fake(mode: Mode) -> Fake {
    Fake {
        mode,
        device_type: "SHDM-2",
        rssi: -60,
        auth_required: false,
        ignore_ota: false,
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

async fn shelly(State(shared): State<Shared>) -> Response {
    let fake = shared.lock().unwrap();
    if fake.mode != Mode::Stock {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(serde_json::json!({
        "type": fake.device_type, "mac": "E8DB84D41F49", "auth": fake.auth_required,
        "fw": "20230913-114008/v1.14.0-gcb84623"
    }))
    .into_response()
}

async fn status(State(shared): State<Shared>) -> Response {
    let fake = shared.lock().unwrap();
    if fake.auth_required {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(serde_json::json!({ "wifi_sta": { "rssi": fake.rssi } })).into_response()
}

#[derive(Deserialize)]
struct OtaQuery {
    url: String,
}

async fn ota(State(shared): State<Shared>, Query(query): Query<OtaQuery>) -> Response {
    {
        let mut fake = shared.lock().unwrap();
        fake.ota_requests += 1;
        if fake.ignore_ota {
            return Json(serde_json::json!({ "status": "updating" })).into_response();
        }
    }
    tokio::spawn(async move {
        let package = reqwest::get(&query.url).await.unwrap().bytes().await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await; // reboots
        let mut fake = shared.lock().unwrap();
        fake.package = package.to_vec();
        if !fake.stays_stock_after_ota {
            fake.mode = Mode::Migration;
        }
    });
    Json(serde_json::json!({ "status": "updating" })).into_response()
}

async fn mg2x_status(State(shared): State<Shared>) -> Response {
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
struct FlashRequest {
    url: String,
    size: u64,
    sha256: String,
}

async fn mg2x_flash(State(shared): State<Shared>, Json(request): Json<FlashRequest>) -> Response {
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
        let image = reqwest::get(&request.url).await.unwrap().bytes().await.unwrap();
        let digest = hex::encode(Sha256::digest(&image));
        let (fail_with, open_port, port) = {
            let mut fake = shared.lock().unwrap();
            fake.received = image.len() as u64;
            fake.image = image.to_vec();
            (fake.fail_with, fake.open_esphome_port, fake.esphome_port)
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
        shared.lock().unwrap().state = MigrationState::Installing;
        tokio::time::sleep(Duration::from_millis(150)).await;
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

async fn start_fake(mut fake: Fake) -> (String, Shared) {
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
        .route("/ota", get(ota))
        .route("/rpc/MG2X.Status", get(mg2x_status))
        .route("/rpc/MG2X.Flash", post(mg2x_flash))
        .with_state(Arc::clone(&shared));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (address.to_string(), shared)
}

#[derive(Default)]
struct Recorder {
    messages: Vec<String>,
    summaries: Vec<String>,
    last_progress: Option<(u64, u64)>,
    decline: bool,
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
        !self.decline
    }
}

fn image() -> FirmwareImage {
    FirmwareImage::parse(synthetic::image(MIN_IMAGE_BYTES + 12_345)).unwrap()
}

fn package(model: Model) -> MigrationPackage {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer.start_file("fw/manifest.json", zip::write::SimpleFileOptions::default()).unwrap();
    write!(writer, r#"{{"name":"{}","platform":"esp8266"}}"#, model.app_name()).unwrap();
    writer.start_file("fw/app.bin", zip::write::SimpleFileOptions::default()).unwrap();
    writer.write_all(b"\0shelly2esphome-mg2x-protocol=2\0").unwrap();
    MigrationPackage::parse(writer.finish().unwrap().into_inner()).unwrap()
}

fn options(shared: &Shared) -> FlashOptions {
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
        },
        ..FlashOptions::default()
    }
}

async fn run(
    host: &str,
    shared: &Shared,
    package: Option<&MigrationPackage>,
    recorder: &mut Recorder,
) -> Result<flash::FlashOutcome, FlashError> {
    let client = DeviceClient::new(host, None).unwrap();
    flash::run(host, &client, &image(), package, &options(shared), recorder).await
}

#[tokio::test(flavor = "multi_thread")]
async fn flashes_from_stock_firmware_end_to_end() {
    let (host, shared) = start_fake(fake(Mode::Stock)).await;
    let package = package(Model::ShellyDimmer2);
    let mut recorder = Recorder::default();
    let outcome = run(&host, &shared, Some(&package), &mut recorder).await.unwrap();

    let fake = shared.lock().unwrap();
    assert_eq!(fake.mode, Mode::Esphome);
    assert_eq!(fake.package, package.bytes().to_vec());
    assert_eq!(fake.image, image().bytes().to_vec());
    assert_eq!(outcome.esphome_address.port(), fake.esphome_port);
    assert!(recorder.summaries[0].contains("ShellyDimmer2 (SHDM-2)"));
    assert!(recorder.summaries[0].contains(image().sha256_hex()));
    let total = image().len() as u64;
    assert_eq!(recorder.last_progress, Some((total, total)));
    assert!(recorder.messages.iter().any(|m| m.contains("Migrationsfirmware läuft")));
    // The power warning is shown before the job is handed to the device.
    let warning = recorder.messages.iter().position(|m| m.contains("Strom nicht trennen"));
    let running = recorder.messages.iter().position(|m| m.contains("Migrationsfirmware läuft"));
    assert!(warning.is_some() && warning > running);
}

#[tokio::test(flavor = "multi_thread")]
async fn resumes_on_a_waiting_migration_firmware_without_package() {
    let mut device = fake(Mode::Migration);
    device.state = MigrationState::Failed;
    device.error = "timeout".to_owned();
    let (host, shared) = start_fake(device).await;
    let mut recorder = Recorder::default();
    run(&host, &shared, None, &mut recorder).await.unwrap();
    assert_eq!(shared.lock().unwrap().ota_requests, 0);
    assert!(recorder.summaries[0].contains("bereits mit der Migrationsfirmware"));
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_a_verification_failure_of_the_device() {
    let mut device = fake(Mode::Migration);
    device.fail_with = Some("hash_mismatch");
    let (host, shared) = start_fake(device).await;
    let error = run(&host, &shared, None, &mut Recorder::default()).await.unwrap_err();
    assert!(matches!(error, FlashError::DeviceFailed(ref reason) if reason == "hash_mismatch"));
    assert!(error.to_string().contains("erneut ausführen"));
    assert_eq!(shared.lock().unwrap().mode, Mode::Migration);
}

#[tokio::test(flavor = "multi_thread")]
async fn warns_against_power_loss_when_restoring_the_boot_sectors_failed() {
    let mut device = fake(Mode::Migration);
    device.fail_with = Some("install_fatal");
    let (host, shared) = start_fake(device).await;
    let error = run(&host, &shared, None, &mut Recorder::default()).await.unwrap_err();
    assert!(matches!(error, FlashError::InstallFatal));
    let message = error.to_string();
    assert!(message.contains("NICHT vom Strom trennen"));
    assert!(message.contains("serielle Schnittstelle"));
    assert_eq!(shared.lock().unwrap().mode, Mode::Migration);
}

#[tokio::test(flavor = "multi_thread")]
async fn warns_on_resume_after_a_fatal_install() {
    let mut device = fake(Mode::Migration);
    device.state = MigrationState::Failed;
    device.error = "install_fatal".to_owned();
    let (host, shared) = start_fake(device).await;
    let mut recorder = Recorder::default();
    run(&host, &shared, None, &mut recorder).await.unwrap();
    assert!(recorder.summaries[0].contains("Strom nicht trennen"));
}

#[tokio::test(flavor = "multi_thread")]
async fn stops_before_any_change_on_weak_signal() {
    let mut device = fake(Mode::Stock);
    device.rssi = -83;
    let (host, shared) = start_fake(device).await;
    let error = run(&host, &shared, Some(&package(Model::ShellyDimmer2)), &mut Recorder::default())
        .await
        .unwrap_err();
    assert!(matches!(error, FlashError::WeakSignal { rssi_dbm: -83, min_rssi_dbm: -75 }));
    assert_eq!(shared.lock().unwrap().ota_requests, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn weak_signal_can_be_overridden_deliberately() {
    let mut device = fake(Mode::Stock);
    device.rssi = -83;
    let (host, shared) = start_fake(device).await;
    let client = DeviceClient::new(&host, None).unwrap();
    let options = FlashOptions { allow_weak_signal: true, ..options(&shared) };
    let package = package(Model::ShellyDimmer2);
    flash::run(&host, &client, &image(), Some(&package), &options, &mut Recorder::default())
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_a_package_for_another_model() {
    let (host, shared) = start_fake(fake(Mode::Stock)).await;
    let error = run(&host, &shared, Some(&package(Model::Shelly1Pm)), &mut Recorder::default())
        .await
        .unwrap_err();
    assert!(matches!(error, FlashError::Package(PackageError::WrongModel { .. })));
    assert_eq!(shared.lock().unwrap().ota_requests, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn needs_a_package_on_stock_firmware() {
    let (host, shared) = start_fake(fake(Mode::Stock)).await;
    let error = run(&host, &shared, None, &mut Recorder::default()).await.unwrap_err();
    assert!(
        matches!(error, FlashError::PackageRequired(ref name) if name == "mg2esphome-ShellyDimmer2.zip")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_unsupported_models() {
    let mut device = fake(Mode::Stock);
    device.device_type = "SHHT-1";
    let (host, shared) = start_fake(device).await;
    let error = run(&host, &shared, None, &mut Recorder::default()).await.unwrap_err();
    assert!(matches!(error, FlashError::UnsupportedModel(ref ty) if ty == "SHHT-1"));
}

#[tokio::test(flavor = "multi_thread")]
async fn aborts_when_the_user_declines() {
    for mode in [Mode::Stock, Mode::Migration] {
        let (host, shared) = start_fake(fake(mode)).await;
        let mut recorder = Recorder { decline: true, ..Recorder::default() };
        let error = run(&host, &shared, Some(&package(Model::ShellyDimmer2)), &mut recorder)
            .await
            .unwrap_err();
        assert!(matches!(error, FlashError::Aborted));
        assert_eq!(shared.lock().unwrap().ota_requests, 0);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_when_the_package_is_never_fetched() {
    let mut device = fake(Mode::Stock);
    device.ignore_ota = true;
    let (host, shared) = start_fake(device).await;
    let client = DeviceClient::new(&host, None).unwrap();
    let mut options = options(&shared);
    options.timeouts.package_fetch = Duration::from_millis(200);
    let package = package(Model::ShellyDimmer2);
    let error =
        flash::run(&host, &client, &image(), Some(&package), &options, &mut Recorder::default())
            .await
            .unwrap_err();
    assert!(matches!(error, FlashError::PackageNotFetched(_)));
    assert!(error.to_string().contains("Nichts verändert"));
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_when_the_migration_firmware_does_not_boot() {
    // The package is fetched, but the device never comes up in the migration firmware.
    let mut device = fake(Mode::Stock);
    device.stays_stock_after_ota = true;
    let (host, shared) = start_fake(device).await;
    let client = DeviceClient::new(&host, None).unwrap();
    let mut options = options(&shared);
    options.timeouts.migration_boot = Duration::from_millis(300);
    let package = package(Model::ShellyDimmer2);
    let error =
        flash::run(&host, &client, &image(), Some(&package), &options, &mut Recorder::default())
            .await
            .unwrap_err();
    assert!(matches!(error, FlashError::MigrationNotReachable));
    assert_eq!(shared.lock().unwrap().package, package.bytes().to_vec());
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_an_image_larger_than_the_staging_slot() {
    let mut device = fake(Mode::Migration);
    device.max_size = 1000;
    let (host, shared) = start_fake(device).await;
    let error = run(&host, &shared, None, &mut Recorder::default()).await.unwrap_err();
    assert!(matches!(error, FlashError::ImageTooLarge { max_size: 1000, .. }));
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_to_interrupt_a_running_job() {
    let mut device = fake(Mode::Migration);
    device.state = MigrationState::Installing;
    let (host, shared) = start_fake(device).await;
    let error = run(&host, &shared, None, &mut Recorder::default()).await.unwrap_err();
    assert!(matches!(error, FlashError::Busy(MigrationState::Installing)));
}

#[tokio::test(flavor = "multi_thread")]
async fn passes_on_a_rejection_of_the_flash_job() {
    let mut device = fake(Mode::Migration);
    device.reject_flash = Some("size_too_large");
    let (host, shared) = start_fake(device).await;
    let error = run(&host, &shared, None, &mut Recorder::default()).await.unwrap_err();
    assert!(
        matches!(error, FlashError::Device(DeviceError::Rejected(ref m)) if m == "size_too_large")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_when_esphome_does_not_answer() {
    let mut device = fake(Mode::Migration);
    device.open_esphome_port = false;
    let (host, shared) = start_fake(device).await;
    let client = DeviceClient::new(&host, None).unwrap();
    let mut options = options(&shared);
    options.timeouts.esphome_boot = Duration::from_millis(300);
    let error = flash::run(&host, &client, &image(), None, &options, &mut Recorder::default())
        .await
        .unwrap_err();
    assert!(matches!(error, FlashError::EsphomeNotSeen(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn identifies_unknown_firmware_and_missing_login() {
    let (host, _shared) = start_fake(fake(Mode::Esphome)).await;
    let client = DeviceClient::new(&host, None).unwrap();
    assert!(matches!(client.identify().await, Err(DeviceError::UnknownFirmware)));

    let mut device = fake(Mode::Stock);
    device.auth_required = true;
    let (host, _shared) = start_fake(device).await;
    let client = DeviceClient::new(&host, None).unwrap();
    assert!(matches!(client.identify().await, Err(DeviceError::AuthRequired)));
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_an_oversized_image_before_touching_stock_firmware() {
    let (host, shared) = start_fake(fake(Mode::Stock)).await;
    let big = FirmwareImage::parse(synthetic::image(Model::ShellyDimmer2.max_image_bytes() + 4096))
        .unwrap();
    let client = DeviceClient::new(&host, None).unwrap();
    let package = package(Model::ShellyDimmer2);
    let error = flash::run(
        &host,
        &client,
        &big,
        Some(&package),
        &options(&shared),
        &mut Recorder::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, FlashError::ImageTooLarge { max_size: 991_232, .. }));
    assert_eq!(shared.lock().unwrap().ota_requests, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_a_failed_relocation_on_resume() {
    let mut device = fake(Mode::Migration);
    device.state = MigrationState::RelocationFailed;
    device.error = "relocation_copy".to_owned();
    let (host, shared) = start_fake(device).await;
    let error = run(&host, &shared, None, &mut Recorder::default()).await.unwrap_err();
    assert!(matches!(error, FlashError::RelocationFailed(ref e) if e == "relocation_copy"));
    assert!(error.to_string().contains("Slot 1"));
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_a_failed_relocation_after_the_stock_update() {
    let mut device = fake(Mode::Stock);
    device.state = MigrationState::RelocationFailed;
    device.error = "relocation_copy".to_owned();
    let (host, shared) = start_fake(device).await;
    let client = DeviceClient::new(&host, None).unwrap();
    let mut options = options(&shared);
    options.timeouts.migration_boot = Duration::from_millis(400);
    let package = package(Model::ShellyDimmer2);
    let error =
        flash::run(&host, &client, &image(), Some(&package), &options, &mut Recorder::default())
            .await
            .unwrap_err();
    assert!(matches!(error, FlashError::RelocationFailed(_)));
}
