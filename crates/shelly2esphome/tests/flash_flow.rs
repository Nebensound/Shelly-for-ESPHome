//! End-to-end tests of the flash procedure against a simulated Shelly device.
#![allow(missing_docs, clippy::unwrap_used)]

mod common;

use std::{path::PathBuf, time::Duration};

use common::*;
use shelly2esphome::{
    backup::{self, BackupError},
    device::{DeviceClient, DeviceError, MigrationState},
    embedded::Packages,
    flash::{self, FlashError, FlashOptions},
    image::{FirmwareImage, synthetic},
    migration::{MigrationPackage, PackageError},
    model::Model,
};

async fn run(
    host: &str,
    shared: &Shared,
    package: Option<&MigrationPackage>,
    recorder: &mut Recorder,
) -> Result<flash::FlashOutcome, FlashError> {
    let client = DeviceClient::new(host, None);
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
    assert!(recorder.messages.iter().any(|m| m.contains("Migration firmware is running")));
    // The power warning is shown before the job is handed to the device.
    let warning = recorder.messages.iter().position(|m| m.contains("Do not cut the power"));
    let running =
        recorder.messages.iter().position(|m| m.contains("Migration firmware is running"));
    assert!(warning.is_some() && warning > running);
    // The stock settings were saved before the first change.
    let saved = recorder
        .messages
        .iter()
        .position(|m| m.starts_with("Stock settings saved to "))
        .expect("backup message");
    let handed = recorder.messages.iter().position(|m| m.contains("Handing the migration"));
    assert!(Some(saved) < handed);
    let directory = PathBuf::from(&recorder.messages[saved]["Stock settings saved to ".len()..]);
    let mut files: Vec<String> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            "settings-actions.json",
            "settings-light-0.json",
            "settings.json",
            "shelly.json",
            "status.json"
        ]
    );
    std::fs::remove_dir_all(directory.parent().unwrap()).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn stops_before_any_change_when_the_backup_fails() {
    let mut device = fake(Mode::Stock);
    device.settings_status = 500;
    let (host, shared) = start_fake(device).await;
    let package = package(Model::ShellyDimmer2);
    let error = run(&host, &shared, Some(&package), &mut Recorder::default()).await.unwrap_err();
    assert!(matches!(error, FlashError::Backup(_)));
    assert!(error.to_string().contains("Nothing was changed"));
    assert_eq!(shared.lock().unwrap().ota_requests, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn backs_up_a_relay_device_without_actions() {
    let mut device = fake(Mode::Stock);
    device.device_type = "SHSW-PM";
    device.has_actions = false;
    let (host, _shared) = start_fake(device).await;
    let client = DeviceClient::new(&host, None);
    let saved = backup::collect(&client).await.unwrap();
    assert_eq!(saved.device, "SHSW-PM");
    let names: Vec<&str> = saved.files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["shelly.json", "status.json", "settings-relay-0.json", "settings.json"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn backup_needs_the_stock_firmware_and_the_login() {
    let (host, _shared) = start_fake(fake(Mode::Migration)).await;
    let client = DeviceClient::new(&host, None);
    let error = backup::collect(&client).await.unwrap_err();
    assert!(matches!(error, BackupError::NotStock));
    assert!(error.to_string().contains("nothing to back up"));

    let mut device = fake(Mode::Stock);
    device.auth_required = true;
    let (host, _shared) = start_fake(device).await;
    let client = DeviceClient::new(&host, None);
    let error = backup::collect(&client).await.unwrap_err();
    assert!(matches!(error, BackupError::Device(DeviceError::AuthRequired)));

    let mut device = fake(Mode::Stock);
    device.settings_status = 404;
    let (host, _shared) = start_fake(device).await;
    let client = DeviceClient::new(&host, None);
    assert!(matches!(backup::collect(&client).await, Err(BackupError::NotStock)));

    let mut device = fake(Mode::Stock);
    device.settings_status = 500;
    let (host, _shared) = start_fake(device).await;
    let client = DeviceClient::new(&host, None);
    let error = backup::collect(&client).await.unwrap_err();
    assert!(error.to_string().contains("/settings answered with HTTP 500"));
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
    assert!(recorder.summaries[0].contains("already runs the migration firmware"));
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_a_verification_failure_of_the_device() {
    let mut device = fake(Mode::Migration);
    device.fail_with = Some("hash_mismatch");
    let (host, shared) = start_fake(device).await;
    let error = run(&host, &shared, None, &mut Recorder::default()).await.unwrap_err();
    assert!(matches!(error, FlashError::DeviceFailed(ref reason) if reason == "hash_mismatch"));
    assert!(error.to_string().contains("run the command again"));
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
    assert!(message.contains("Do NOT cut the power"));
    assert!(message.contains("serial interface"));
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
    assert!(recorder.summaries[0].contains("Do not cut the power"));
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
    let client = DeviceClient::new(&host, None);
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
async fn picks_the_embedded_package_of_the_detected_model() {
    let (host, shared) = start_fake(fake(Mode::Stock)).await;
    let wrong = package(Model::Shelly1Pm).bytes().to_vec().leak();
    let right = package(Model::ShellyDimmer2).bytes().to_vec().leak();
    let packages: &'static [_] =
        vec![("mg2esphome-Shelly1PM.zip", &*wrong), ("mg2esphome-ShellyDimmer2.zip", &*right)]
            .leak();
    let options = FlashOptions { embedded_packages: Packages(packages), ..options(&shared) };
    let client = DeviceClient::new(&host, None);
    flash::run(&host, &client, &image(), None, &options, &mut Recorder::default()).await.unwrap();
    let fake = shared.lock().unwrap();
    assert_eq!(fake.package, right.to_vec());
    assert_eq!(fake.mode, Mode::Esphome);
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
    let client = DeviceClient::new(&host, None);
    let mut options = options(&shared);
    options.timeouts.package_fetch = Duration::from_millis(200);
    let package = package(Model::ShellyDimmer2);
    let error =
        flash::run(&host, &client, &image(), Some(&package), &options, &mut Recorder::default())
            .await
            .unwrap_err();
    assert!(matches!(error, FlashError::PackageNotFetched(_)));
    assert!(error.to_string().contains("Nothing was changed"));
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_when_the_migration_firmware_does_not_boot() {
    // The package is fetched, but the device never comes up in the migration firmware.
    let mut device = fake(Mode::Stock);
    device.stays_stock_after_ota = true;
    let (host, shared) = start_fake(device).await;
    let client = DeviceClient::new(&host, None);
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
    assert!(matches!(error, FlashError::StagingSlotTooSmall { max_size: 1000, .. }));

    // The same after the stock firmware installed the migration firmware:
    // the device has changed, so this is no refusal before any change.
    let mut device = fake(Mode::Stock);
    device.max_size = 1000;
    let (host, shared) = start_fake(device).await;
    let package = package(Model::ShellyDimmer2);
    let error = run(&host, &shared, Some(&package), &mut Recorder::default()).await.unwrap_err();
    assert!(matches!(error, FlashError::StagingSlotTooSmall { max_size: 1000, .. }));
    assert_eq!(shared.lock().unwrap().ota_requests, 1);
    assert!(error.report_kind().is_some());
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
    let client = DeviceClient::new(&host, None);
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
    let client = DeviceClient::new(&host, None);
    assert!(matches!(client.identify().await, Err(DeviceError::UnknownFirmware)));

    let mut device = fake(Mode::Stock);
    device.auth_required = true;
    let (host, _shared) = start_fake(device).await;
    let client = DeviceClient::new(&host, None);
    assert!(matches!(client.identify().await, Err(DeviceError::AuthRequired)));

    // Firmware variants that protect /shelly as well.
    let mut device = fake(Mode::Stock);
    device.shelly_requires_auth = true;
    let (host, _shared) = start_fake(device).await;
    let client = DeviceClient::new(&host, None);
    assert!(matches!(client.identify().await, Err(DeviceError::AuthRequired)));
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_an_oversized_image_before_touching_stock_firmware() {
    let (host, shared) = start_fake(fake(Mode::Stock)).await;
    let big = FirmwareImage::parse(synthetic::image(Model::ShellyDimmer2.max_image_bytes() + 4096))
        .unwrap();
    let client = DeviceClient::new(&host, None);
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
    assert!(error.to_string().contains("slot 1"));
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_a_failed_relocation_after_the_stock_update() {
    let mut device = fake(Mode::Stock);
    device.state = MigrationState::RelocationFailed;
    device.error = "relocation_copy".to_owned();
    let (host, shared) = start_fake(device).await;
    let client = DeviceClient::new(&host, None);
    let mut options = options(&shared);
    options.timeouts.migration_boot = Duration::from_millis(400);
    let package = package(Model::ShellyDimmer2);
    let error =
        flash::run(&host, &client, &image(), Some(&package), &options, &mut Recorder::default())
            .await
            .unwrap_err();
    assert!(matches!(error, FlashError::RelocationFailed(_)));
}

async fn run_with(
    host: &str,
    package: Option<&MigrationPackage>,
    options: &FlashOptions,
) -> Result<flash::FlashOutcome, FlashError> {
    let client = DeviceClient::new(host, None);
    flash::run(host, &client, &image(), package, options, &mut Recorder::default()).await
}

#[tokio::test(flavor = "multi_thread")]
async fn stops_when_the_host_cannot_be_found_or_reached() {
    let (_, shared) = start_fake(fake(Mode::Stock)).await;
    let options = options(&shared);
    let error = run_with("does-not-exist.invalid", None, &options).await.unwrap_err();
    assert!(matches!(error, FlashError::Io(_)));
    let error = run_with(&closed_port(), None, &options).await.unwrap_err();
    assert!(matches!(error, FlashError::Device(DeviceError::Http(_))));
}

#[tokio::test(flavor = "multi_thread")]
async fn detects_the_own_address_when_no_serve_ip_is_given() {
    let (host, shared) = start_fake(fake(Mode::Migration)).await;
    let options = FlashOptions { serve_ip: None, ..options(&shared) };
    run_with(&host, None, &options).await.unwrap();
    assert_eq!(shared.lock().unwrap().mode, Mode::Esphome);
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn stops_when_there_is_no_route_to_the_device() {
    // Linux refuses to connect a UDP socket to the broadcast address.
    let (_, shared) = start_fake(fake(Mode::Migration)).await;
    let options = FlashOptions { serve_ip: None, ..options(&shared) };
    let error = run_with("255.255.255.255:80", None, &options).await.unwrap_err();
    assert!(matches!(error, FlashError::Io(_)), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_a_broken_embedded_package() {
    let (host, shared) = start_fake(fake(Mode::Stock)).await;
    let options = FlashOptions {
        embedded_packages: Packages(&[("mg2esphome-ShellyDimmer2.zip", b"not a zip")]),
        ..options(&shared)
    };
    let error = run_with(&host, None, &options).await.unwrap_err();
    assert!(matches!(error, FlashError::Package(PackageError::Zip(_))));
    assert_eq!(shared.lock().unwrap().ota_requests, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn stops_when_the_backup_cannot_be_written() {
    let (host, shared) = start_fake(fake(Mode::Stock)).await;
    let blocker = scratch_dir();
    std::fs::write(&blocker, "a file, not a directory").unwrap();
    let options = FlashOptions { backup_dir: Some(blocker.clone()), ..options(&shared) };
    let error = run_with(&host, Some(&package(Model::ShellyDimmer2)), &options).await.unwrap_err();
    assert!(matches!(error, FlashError::Backup(BackupError::Write(..))));
    assert_eq!(shared.lock().unwrap().ota_requests, 0);
    std::fs::remove_file(blocker).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn flashes_without_backup_when_asked() {
    let (host, shared) = start_fake(fake(Mode::Stock)).await;
    let options = FlashOptions { backup_dir: None, ..options(&shared) };
    run_with(&host, Some(&package(Model::ShellyDimmer2)), &options).await.unwrap();
    assert_eq!(shared.lock().unwrap().mode, Mode::Esphome);
}

#[tokio::test(flavor = "multi_thread")]
async fn stops_when_the_file_server_cannot_start() {
    let (host, shared) = start_fake(fake(Mode::Migration)).await;
    // TEST-NET-1: no local interface has this address.
    let options = FlashOptions { serve_ip: Some("192.0.2.1".parse().unwrap()), ..options(&shared) };
    let error = run_with(&host, None, &options).await.unwrap_err();
    assert!(matches!(error, FlashError::Io(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn stops_when_the_stock_firmware_refuses_the_update() {
    let mut device = fake(Mode::Stock);
    device.ota_status = 500;
    let (host, shared) = start_fake(device).await;
    let error = run(&host, &shared, Some(&package(Model::ShellyDimmer2)), &mut Recorder::default())
        .await
        .unwrap_err();
    assert!(matches!(error, FlashError::Device(DeviceError::Status { endpoint: "/ota", .. })));
}

#[tokio::test(flavor = "multi_thread")]
async fn accepts_a_device_that_restarts_without_reporting_installing() {
    let mut device = fake(Mode::Migration);
    device.skip_installing = true;
    let (host, shared) = start_fake(device).await;
    run(&host, &shared, None, &mut Recorder::default()).await.unwrap();
    assert_eq!(shared.lock().unwrap().mode, Mode::Esphome);
}

#[tokio::test(flavor = "multi_thread")]
async fn gives_up_when_the_device_never_finishes() {
    let mut device = fake(Mode::Migration);
    device.hang_after_download = true;
    let (host, shared) = start_fake(device).await;
    let mut options = options(&shared);
    options.timeouts.flash = Duration::from_secs(1);
    let error = run_with(&host, None, &options).await.unwrap_err();
    assert!(matches!(error, FlashError::FlashTimeout));
}

#[tokio::test(flavor = "multi_thread")]
async fn gives_up_when_the_migration_firmware_never_gets_ready() {
    let mut device = fake(Mode::Stock);
    // After the stock OTA the migration firmware keeps reporting a running job.
    device.state = MigrationState::Downloading;
    let (host, shared) = start_fake(device).await;
    let mut options = options(&shared);
    options.timeouts.migration_boot = Duration::from_secs(1);
    let error = run_with(&host, Some(&package(Model::ShellyDimmer2)), &options).await.unwrap_err();
    assert!(matches!(error, FlashError::MigrationNotReachable));
}

#[tokio::test(flavor = "multi_thread")]
async fn leaves_later_generations_to_their_own_procedure() {
    let (_, shared) = start_fake(fake(Mode::Stock)).await;
    let (host, device) = common::gen2::start(common::gen2::gen2()).await;
    let error = run_with(&host, None, &options(&shared)).await.unwrap_err();
    assert!(matches!(error, FlashError::UnsupportedGeneration { .. }), "{error}");
    assert!(device.lock().unwrap().requests.is_empty());
}
