//! The command line interface (`cli::run`) end to end: arguments in, text out.
#![allow(missing_docs, clippy::unwrap_used)]

mod common;

use std::{
    io::{BufReader, Cursor, Read},
    net::IpAddr,
    path::Path,
    process::ExitCode,
    time::Duration,
};

use common::*;
use shelly2esphome::{
    cli::{self, Settings, Terminal},
    discover::ScanOptions,
    embedded::Packages,
    model::Model,
};

struct Output {
    code: ExitCode,
    out: String,
    err: String,
}

async fn run_with_input(
    args: &[&str],
    settings: &Settings,
    input: &mut dyn std::io::BufRead,
) -> Output {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut terminal = Terminal { out: &mut out, err: &mut err, input };
    let mut full = vec!["shelly2esphome"];
    full.extend_from_slice(args);
    let code = cli::run(full, settings, &mut terminal).await;
    Output { code, out: String::from_utf8(out).unwrap(), err: String::from_utf8(err).unwrap() }
}

async fn run(args: &[&str], settings: &Settings) -> Output {
    run_with_input(args, settings, &mut Cursor::new(Vec::new())).await
}

fn write_temp(name: &str, bytes: &[u8]) -> String {
    let directory = scratch_dir();
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(name);
    std::fs::write(&path, bytes).unwrap();
    path.to_str().unwrap().to_owned()
}

fn firmware_file() -> String {
    write_temp("firmware.bin", image().bytes())
}

fn package_file() -> String {
    write_temp("mg2esphome-ShellyDimmer2.zip", package(Model::ShellyDimmer2).bytes())
}

#[tokio::test]
async fn shows_help_version_and_usage_errors() {
    let settings = Settings::default();
    let help = run(&["--help"], &settings).await;
    assert_eq!(help.code, ExitCode::SUCCESS);
    assert!(help.out.contains("Usage: shelly2esphome"));
    assert!(help.out.contains("list"));
    let version = run(&["--version"], &settings).await;
    assert_eq!(version.code, ExitCode::SUCCESS);
    assert!(version.out.contains(env!("CARGO_PKG_VERSION")));
    assert!(help.out.contains("version  Print the version of this program"));
    let subcommand = run(&["version"], &settings).await;
    assert_eq!(subcommand.code, ExitCode::SUCCESS);
    assert_eq!(subcommand.out, version.out);
    assert!(subcommand.err.is_empty());
    let wrong = run(&["frobnicate"], &settings).await;
    assert_eq!(wrong.code, ExitCode::from(2));
    assert!(wrong.err.contains("unrecognized subcommand"));
}

#[tokio::test]
async fn checks_firmware_files() {
    let settings = Settings::default();
    let file = firmware_file();
    let ok = run(&["check", &file], &settings).await;
    assert_eq!(ok.code, ExitCode::SUCCESS);
    assert!(ok.out.contains(image().sha256_hex()));
    assert!(ok.out.contains("Usable for shelly2esphome flash."));

    let missing = run(&["check", "/does/not/exist.bin"], &settings).await;
    assert_eq!(missing.code, ExitCode::FAILURE);
    assert!(missing.err.contains("Error: cannot read /does/not/exist.bin"));

    let junk = write_temp("junk.bin", &[0_u8; 100]);
    let unusable = run(&["check", &junk], &settings).await;
    assert!(unusable.err.contains("is not usable"));
    assert!(unusable.err.contains("too small"));
}

const STOCK_DIMMER: &str = r#"{"type":"SHDM-2","mac":"AABBCCDDEEFF","auth":false,"fw":"1.14.0"}"#;
const SIGNAL: &str = r#"{"wifi_sta":{"rssi":-64}}"#;

#[tokio::test]
async fn probes_stock_devices() {
    let (host, _) = scripted(&[("/shelly", 200, STOCK_DIMMER), ("/status", 200, SIGNAL)]).await;
    let mut settings = Settings::default();
    settings.flash.embedded_packages = Packages(&[]);
    let probe = run(&["probe", &host], &settings).await;
    assert_eq!(probe.code, ExitCode::SUCCESS);
    assert!(probe.out.contains("Stock firmware: SHDM-2 (1.14.0), MAC AABBCCDDEEFF"));
    assert!(probe.out.contains("Wi-Fi signal: -64 dBm"));
    assert!(probe.out.contains("pass flash --migration mg2esphome-ShellyDimmer2.zip"));

    settings.flash.embedded_packages = Packages(&[("mg2esphome-ShellyDimmer2.zip", b"zip")]);
    let built_in = run(&["probe", &host], &settings).await;
    assert!(built_in.out.contains("built into this program"));

    let (host, _) = scripted(&[
        ("/shelly", 200, r#"{"type":"SHHT-1","mac":"AABBCCDDEEFF","auth":true,"fw":"1.0"}"#),
        ("/status", 200, SIGNAL),
    ])
    .await;
    let other = run(&["probe", &host, "--auth", "admin:secret"], &settings).await;
    assert!(other.out.contains("This device type is not supported."));
    assert!(other.out.contains("Login enabled"));

    let gone = run(&["probe", &closed_port()], &settings).await;
    assert_eq!(gone.code, ExitCode::FAILURE);
    assert!(gone.err.contains("Error: device unreachable"));
}

#[tokio::test]
async fn probes_the_migration_firmware() {
    let settings = Settings::default();
    let (host, _) = scripted(&[(
        "/rpc/MG2X.Status",
        200,
        r#"{"protocol":2,"state":"failed","received":5,"size":9,"max_size":99,"error":"timeout"}"#,
    )])
    .await;
    let failed = run(&["probe", &host], &settings).await;
    assert!(failed.out.contains("Migration firmware (protocol 2): state Failed, 5 of 9 bytes"));
    assert!(failed.out.contains("Last error: timeout"));

    let (host, _) = scripted(&[(
        "/rpc/MG2X.Status",
        200,
        r#"{"protocol":2,"state":"idle","received":0,"size":0,"max_size":99,"error":""}"#,
    )])
    .await;
    let idle = run(&["probe", &host], &settings).await;
    assert!(idle.out.contains("state Idle"));
    assert!(!idle.out.contains("Last error"));
}

#[tokio::test]
async fn probes_later_generations_and_refuses_to_flash_them() {
    let settings = Settings::default();
    let gen3 = r#"{"id":"shellyplugmg3-x","mac":"08927251CA24","slot":1,"model":"S3PL-30110EU",
        "gen":3,"ver":"2.2.0-beta2","app":"PlugMG3","auth_en":false}"#;
    let (host, _) = scripted(&[
        ("/shelly", 200, gen3),
        ("/rpc/Wifi.GetStatus", 200, r#"{"status":"got ip","rssi":-74}"#),
    ])
    .await;
    let probe = run(&["probe", &host], &settings).await;
    assert_eq!(probe.code, ExitCode::SUCCESS);
    assert!(probe.out.contains(
        "Stock firmware: Shelly Gen3, model S3PL-30110EU, app PlugMG3, firmware 2.2.0-beta2, \
         MAC 08927251CA24"
    ));
    assert!(probe.out.contains("Runs from app slot 1."));
    assert!(probe.out.contains("Wi-Fi signal: -74 dBm"));
    assert!(probe.out.contains("Beta firmware"));
    assert!(probe.out.contains("Not supported yet: only Gen1 devices can be flashed so far."));

    // A stable Gen2 with login and without slot information.
    let gen2 = r#"{"id":"shellyplus1-x","mac":"AA","model":"SNSW-001X16EU","gen":2,
        "ver":"1.7.6","app":"Plus1","auth_en":true}"#;
    let (host, _) = scripted(&[("/shelly", 200, gen2), ("/rpc/Wifi.GetStatus", 401, "")]).await;
    let probe = run(&["probe", &host], &settings).await;
    assert!(probe.out.contains("Shelly Gen2, model SNSW-001X16EU"));
    assert!(probe.out.contains("Wi-Fi signal: unknown (login enabled)"));
    assert!(!probe.out.contains("app slot"));
    assert!(!probe.out.contains("Beta firmware"));

    // A Pro device on Ethernet with Wi-Fi off.
    let pro =
        r#"{"id":"x","mac":"AA","model":"SPSW-202XE16EU","gen":2,"ver":"2.0.3","app":"Pro2"}"#;
    let (host, _) = scripted(&[
        ("/shelly", 200, pro),
        ("/rpc/Wifi.GetStatus", 200, r#"{"status":"disconnected","rssi":0}"#),
    ])
    .await;
    let probe = run(&["probe", &host], &settings).await;
    assert!(probe.out.contains("Wi-Fi: not connected"));
    assert!(!probe.out.contains("0 dBm"));

    // flash refuses after reading only.
    let (host, seen) = scripted(&[
        ("/shelly", 200, gen3),
        ("/rpc/Wifi.GetStatus", 200, r#"{"status":"got ip","rssi":-74}"#),
    ])
    .await;
    let flash = run(&["flash", &host, "--firmware", &firmware_file(), "--yes"], &settings).await;
    assert_eq!(flash.code, ExitCode::FAILURE);
    assert!(flash.err.contains(
        "S3PL-30110EU is a Shelly Gen3; only Gen1 devices can be flashed so far. Nothing was \
         changed."
    ));
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn checks_a_factory_image_against_the_official_package() {
    use shelly2esphome::esp32::synthetic::{Fixture, LAYOUT, table};
    let settings = Settings::default();
    let fixture = Fixture::default();
    let package = write_temp("S1G3.zip", &fixture.package());
    let factory = write_temp("firmware.factory.bin", &fixture.factory());

    let ok = run(&["check", &factory, "--package", &package], &settings).await;
    assert_eq!(ok.code, ExitCode::SUCCESS, "{}", ok.err);
    assert!(ok.out.contains(
        "ESP32-C3 factory image for S1G3 2.0.3; bootloader, partition table and application \
         match the official package."
    ));
    assert!(
        ok.out.contains(&format!("Application: {} of 65536 bytes in app_0.", fixture.app.len()))
    );
    assert!(ok.out.contains("Gen2/Gen3 devices cannot be flashed yet."));
    assert!(!ok.out.contains("Note:"));

    let other = Fixture { table: table(&LAYOUT[..6]), ..Fixture::default() };
    let factory_other = write_temp("other.factory.bin", &other.factory());
    let noted = run(&["check", &factory_other, "--package", &package], &settings).await;
    assert_eq!(noted.code, ExitCode::SUCCESS, "{}", noted.err);
    assert!(noted.out.contains("Note: the partition table differs in shelly: missing"));

    let missing = run(&["check", &factory, "--package", "/nonexistent.zip"], &settings).await;
    assert!(missing.err.contains("cannot read /nonexistent.zip"));
    let not_a_package = run(&["check", &factory, "--package", &factory], &settings).await;
    assert!(not_a_package.err.contains("is no usable official package"));
    let gone = run(&["check", "/nonexistent.bin", "--package", &package], &settings).await;
    assert!(gone.err.contains("cannot read /nonexistent.bin"));
    let mut damaged = Fixture::default();
    damaged.bootloader[0] = 0;
    let broken = write_temp("broken.factory.bin", &damaged.factory());
    let unsuitable = run(&["check", &broken, "--package", &package], &settings).await;
    assert_eq!(unsuitable.code, ExitCode::FAILURE);
    assert!(unsuitable.err.contains("does not suit S1G3: no valid bootloader at 0x0"));
}

fn scan_settings(port: u16) -> Settings {
    Settings {
        scan: ScanOptions { port, timeout: Duration::from_millis(300), ..ScanOptions::default() },
        ..Settings::default()
    }
}

#[tokio::test]
async fn lists_shellys() {
    let (host, _) = scripted(&[("/shelly", 200, STOCK_DIMMER)]).await;
    let port: u16 = host.rsplit(':').next().unwrap().parse().unwrap();
    let list = run(&["list", "127.0.0.1/32"], &scan_settings(port)).await;
    assert_eq!(list.code, ExitCode::SUCCESS);
    assert!(list.err.contains("Scanning 127.0.0.1/32"));
    assert!(list.out.starts_with("ADDRESS"));
    assert!(list.out.contains("127.0.0.1  ShellyDimmer2 (SHDM-2)"));
    assert!(list.out.contains("1 Shelly(s) found, 1 can be flashed."));

    let closed: u16 = closed_port().rsplit(':').next().unwrap().parse().unwrap();
    let empty = run(&["detect", "127.0.0.1"], &scan_settings(closed)).await;
    assert!(empty.out.contains("No Shelly found"));

    let bad = run(&["list", "10.0.0.0/8"], &scan_settings(closed)).await;
    assert_eq!(bad.code, ExitCode::from(2));
    assert!(bad.err.contains("too large"));
}

#[tokio::test]
async fn lists_the_own_network_by_default() {
    let closed: u16 = closed_port().rsplit(':').next().unwrap().parse().unwrap();
    let mut settings = scan_settings(closed);
    // Pretend this computer is 127.0.0.77: the default is then 127.0.0.0/24.
    settings.local_ip = |_| Ok(IpAddr::from([127, 0, 0, 77]));
    let list = run(&["list"], &settings).await;
    assert!(list.err.contains("Scanning 127.0.0.0/24"), "{}", list.err);

    settings.local_ip = |_| Err(std::io::Error::other("no route"));
    let failed = run(&["list"], &settings).await;
    assert_eq!(failed.code, ExitCode::FAILURE);
    assert!(failed.err.contains("pass one like 10.10.5.0/24"));
}

const SETTINGS_DIMMER: &str = r#"{"lights":[{}]}"#;

#[tokio::test]
async fn backs_up_devices() {
    let (host, _) = scripted(&[
        ("/shelly", 200, STOCK_DIMMER),
        ("/settings", 200, SETTINGS_DIMMER),
        ("/settings/light/0", 200, "{}"),
    ])
    .await;
    let output = scratch_dir();
    let saved = run(&["backup", &host, "-o", output.to_str().unwrap()], &Settings::default()).await;
    assert_eq!(saved.code, ExitCode::SUCCESS, "{}", saved.err);
    assert!(saved.out.starts_with("Saved 3 files of SHDM-2 (AABBCCDDEEFF) to "));
    assert_eq!(std::fs::read_dir(&output).unwrap().count(), 1);
    std::fs::remove_dir_all(&output).unwrap();

    let failed = run(&["backup", &closed_port()], &Settings::default()).await;
    assert_eq!(failed.code, ExitCode::FAILURE);

    // The output directory is a file.
    let blocker = write_temp("not-a-directory", b"x");
    let unwritable = run(&["backup", &host, "-o", &blocker], &Settings::default()).await;
    assert_eq!(unwritable.code, ExitCode::FAILURE);
    assert!(unwritable.err.contains("could not write the backup"));
}

async fn flash_settings(mode: Mode) -> (String, Shared, Settings) {
    let (host, shared) = start_fake(fake(mode)).await;
    let settings = Settings { flash: options(&shared), ..Settings::default() };
    (host, shared, settings)
}

#[tokio::test(flavor = "multi_thread")]
async fn flashes_with_backup_and_confirmation_flag() {
    let (host, shared, settings) = flash_settings(Mode::Stock).await;
    let backups = scratch_dir();
    let (firmware, package) = (firmware_file(), package_file());
    let args = [
        "flash",
        &host,
        "-f",
        &firmware,
        "-m",
        &package,
        "--backup-dir",
        backups.to_str().unwrap(),
        "--serve-ip",
        "127.0.0.1",
        "--yes",
    ];
    let flashed = run(&args, &settings).await;
    assert_eq!(flashed.code, ExitCode::SUCCESS, "{}", flashed.err);
    assert!(flashed.err.contains("Device: ShellyDimmer2 (SHDM-2)"));
    assert!(flashed.err.contains("• Stock settings saved to"));
    assert!(flashed.err.contains("transferred: 100 %"));
    assert!(flashed.err.contains("• Done: ESPHome answers on"));
    assert!(!flashed.err.contains("Continue?"));
    // The report link carries what the run learned, and no address.
    assert!(flashed.out.starts_with("Help others with the same device: report that it works."));
    let link = flashed.out.lines().last().unwrap();
    assert!(link.starts_with(
        "https://github.com/Nebensound/Shelly-for-ESPHome/issues/new?template=device-report.yml&"
    ));
    for field in
        ["&result=success&", "&model=SHDM-2&", "&login=no&", "&backup=yes&", "&package=file&"]
    {
        assert!(link.contains(field), "{field} in {link}");
    }
    assert!(!link.contains("127.0.0.1"), "{link}");
    assert_eq!(shared.lock().unwrap().mode, Mode::Esphome);
    assert_eq!(std::fs::read_dir(&backups).unwrap().count(), 1);
    std::fs::remove_dir_all(&backups).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn links_a_failure_report_after_the_error() {
    let (host, shared, settings) = flash_settings(Mode::Stock).await;
    shared.lock().unwrap().fail_with = Some("hash_mismatch");
    let args =
        ["flash", &host, "-f", &firmware_file(), "-m", &package_file(), "--no-backup", "--yes"];
    let failed = run(&args, &settings).await;
    assert_eq!(failed.code, ExitCode::FAILURE);
    let (error, report) = failed.err.split_once("\n\nPlease report this failure").unwrap();
    assert!(error.contains("Error: the device reports the error \"hash_mismatch\""), "{error}");
    let link = report.lines().last().unwrap();
    assert!(link.contains("?template=device-failure.yml&"), "{link}");
    assert!(link.contains("&result=device-failed%3Ahash_mismatch&model=SHDM-2&"), "{link}");
    assert!(link.contains("&backup=no&"), "{link}");
    assert!(failed.out.is_empty());

    // A run resumed in the migration firmware does not know the model and
    // links nothing, neither after a success.
    let (host, shared, settings) = flash_settings(Mode::Migration).await;
    let resumed = run(&["flash", &host, "-f", &firmware_file(), "--yes"], &settings).await;
    assert_eq!(resumed.code, ExitCode::SUCCESS, "{}", resumed.err);
    assert_eq!(shared.lock().unwrap().mode, Mode::Esphome);
    assert!(resumed.out.is_empty(), "{}", resumed.out);

    // A refusal before any change links nothing.
    let (host, _, settings) = flash_settings(Mode::Stock).await;
    let weak = run(&["flash", &host, "-f", &firmware_file(), "--min-rssi", "-50"], &settings).await;
    assert!(weak.err.contains("Wi-Fi signal too weak"));
    assert!(!weak.err.contains("Please report"));
}

#[tokio::test(flavor = "multi_thread")]
async fn asks_before_flashing() {
    let (firmware, package) = (firmware_file(), package_file());

    let (host, shared, settings) = flash_settings(Mode::Stock).await;
    let args = ["flash", &host, "-f", &firmware, "-m", &package, "--no-backup"];
    let confirmed = run_with_input(&args, &settings, &mut Cursor::new(b"yes\n".to_vec())).await;
    assert_eq!(confirmed.code, ExitCode::SUCCESS, "{}", confirmed.err);
    assert!(confirmed.err.contains("Continue? Type \"yes\" to confirm:"));
    assert_eq!(shared.lock().unwrap().mode, Mode::Esphome);

    let (host, shared, settings) = flash_settings(Mode::Stock).await;
    let args = ["flash", &host, "-f", &firmware, "-m", &package, "--no-backup"];
    let declined = run_with_input(&args, &settings, &mut Cursor::new(b"no\n".to_vec())).await;
    assert_eq!(declined.code, ExitCode::FAILURE);
    assert!(declined.err.contains("Error: aborted, nothing was changed"));
    assert_eq!(shared.lock().unwrap().ota_requests, 0);

    // A broken terminal counts as "no".
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("broken terminal"))
        }
    }
    let (host, _, settings) = flash_settings(Mode::Migration).await;
    let args = ["flash", &host, "-f", &firmware];
    let broken = run_with_input(&args, &settings, &mut BufReader::new(Broken)).await;
    assert!(broken.err.contains("aborted"));
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_unusable_flash_inputs() {
    let (host, _, settings) = flash_settings(Mode::Stock).await;
    let firmware = firmware_file();
    let missing_package =
        run(&["flash", &host, "-f", &firmware, "-m", "/no/such.zip"], &settings).await;
    assert!(missing_package.err.contains("cannot read /no/such.zip"));

    let junk = write_temp("junk.zip", b"no zip");
    let broken_package = run(&["flash", &host, "-f", &firmware, "-m", &junk], &settings).await;
    assert!(broken_package.err.contains("not a valid ZIP archive"));

    let missing_firmware = run(&["flash", &host, "-f", "/no/firmware.bin"], &settings).await;
    assert_eq!(missing_firmware.code, ExitCode::FAILURE);
    assert!(missing_firmware.err.contains("cannot read /no/firmware.bin"));

    // Weak signal: the minimum comes from the command line.
    let weak =
        run(&["flash", &host, "-f", &firmware, "--min-rssi", "-50", "--yes"], &settings).await;
    assert!(weak.err.contains("Wi-Fi signal too weak"));
    assert!(Path::new(&firmware).exists());
}
