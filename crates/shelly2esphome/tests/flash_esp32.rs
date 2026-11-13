//! `flash` of a Gen2/Gen3 device against the simulated stock firmware.
#![allow(clippy::unwrap_used)]

mod common;

use std::time::Duration;

use common::{
    Recorder,
    gen2::{self, Gen2, PASSWORD},
    scratch_dir,
};
use shelly2esphome::{
    device::{DeviceClient, Firmware},
    esp32::synthetic::{Fixture, LAYOUT, table},
    esphome_package::EsphomePackage,
    flash::{FlashError, FlashOptions, FlashOutcome, Timeouts},
    flash_esp32::{self, Esp32Flash},
    report::PackageSource,
    stock_package::StockPackage,
};

/// What a run did.
struct Run {
    result: Result<FlashOutcome, FlashError>,
    recorder: Recorder,
    device: gen2::Shared,
    backup: std::path::PathBuf,
}

impl Run {
    fn requests(&self) -> Vec<String> {
        self.device.lock().unwrap().requests.clone()
    }

    fn error(&self) -> String {
        self.result.as_ref().map(|_| ()).expect_err("must fail").to_string()
    }

    /// Whether the device was changed beyond the temporary log setting.
    fn updated(&self) -> bool {
        self.requests().iter().any(|request| request == "Shelly.Update")
    }

    fn log_changed(&self) -> bool {
        self.requests().iter().any(|request| request.starts_with("log -> "))
    }
}

fn timeouts() -> Timeouts {
    Timeouts {
        package_fetch: Duration::from_secs(5),
        migration_boot: Duration::from_secs(5),
        flash: Duration::from_secs(5),
        esphome_boot: Duration::from_secs(5),
        poll_interval: Duration::from_millis(20),
        slot_probe: Duration::from_secs(2),
        settle: Duration::ZERO,
        package_download: Duration::from_secs(5),
        update_result: Duration::from_secs(2),
    }
}

struct Setup {
    fixture: Fixture,
    credentials: Option<&'static str>,
    options: FlashOptions,
    recorder: Recorder,
}

fn setup() -> Setup {
    Setup {
        fixture: Fixture::default(),
        credentials: None,
        options: FlashOptions {
            serve_ip: Some("127.0.0.1".parse().unwrap()),
            log_port: gen2::log_port(),
            timeouts: timeouts(),
            backup_dir: Some(scratch_dir()),
            ..FlashOptions::default()
        },
        recorder: Recorder::default(),
    }
}

async fn run_with(device: Gen2, change: impl FnOnce(&mut Setup)) -> Run {
    let mut setup = setup();
    change(&mut setup);
    let (host, shared) = gen2::start(device).await;
    setup.options.esphome_port = shared.lock().unwrap().esphome_port;
    let Ok(Firmware::Newer { info, .. }) = DeviceClient::new(&host, None).identify().await else {
        panic!("not a Gen2+ device");
    };
    let stock = StockPackage::parse(&setup.fixture.package()).unwrap();
    let flash = Esp32Flash {
        info: &info,
        credentials: setup.credentials.map(|login| login.parse().unwrap()),
        factory: setup.fixture.factory(),
        stock: &stock,
        source: PackageSource::Official,
    };
    let mut recorder = setup.recorder;
    let result = flash_esp32::run(&host, flash, &setup.options, &mut recorder).await;
    let backup = setup.options.backup_dir.unwrap_or_default();
    Run { result, recorder, device: shared, backup }
}

async fn run(device: Gen2) -> Run {
    run_with(device, |_| {}).await
}

#[tokio::test]
async fn installs_esphome_through_the_stock_updater() {
    // A line of an earlier update before the slot line changes nothing.
    let run = run(Gen2 { stale_line: true, ..gen2::gen2() }).await;
    let outcome = run.result.as_ref().expect("flashed");
    let port = run.device.lock().unwrap().esphome_port;
    assert_eq!(outcome.esphome_address.port(), port);

    // The device got exactly the package `check` builds.
    let fixture = Fixture::default();
    let stock = StockPackage::parse(&fixture.package()).unwrap();
    let (expected, _) = EsphomePackage::build(&stock, fixture.factory()).unwrap();
    assert_eq!(run.device.lock().unwrap().downloaded, expected.bytes());

    // Read first, then the log setting, then the update.
    let requests = run.requests();
    let first_change = requests.iter().position(|request| request == "Sys.SetConfig").unwrap();
    assert!(
        requests[..first_change].iter().all(|request| {
            request.contains(".Get") || request.ends_with(".List") || request == "Sys.GetConfig"
        }),
        "{requests:?}"
    );
    assert_eq!(requests.last().map(String::as_str), Some("Shelly.Update"));
    assert!(requests.contains(&"Script.GetCode".to_owned()));

    let summary = &run.recorder.summaries[0];
    assert!(summary.starts_with(
        "Device: S1G3 (S3SW-001X16EU, Gen3), stock firmware 2.0.3, Wi-Fi -60 dBm\nESPHome \
         package: "
    ));
    assert!(summary.contains("ESPHome's bootloader (min_version 1.0.9)"));
    let messages = run.recorder.messages.join("\n");
    for expected in [
        "Stock settings saved to ",
        "The stock installer writes to slot 0, which ESPHome's bootloader starts",
        "Handing the ESPHome package to the stock updater …",
        "Package downloaded; the stock installer installs ESPHome",
        "The stock installer writes ESPHome's bootloader",
        "The stock installer reports success; the device restarts",
        "Waiting for ESPHome …",
    ] {
        assert!(messages.contains(expected), "{expected}\n{messages}");
    }
    let total = expected.bytes().len() as u64;
    assert_eq!(run.recorder.last_progress, Some((total, total)));

    // The backup holds the configuration, the scripts with their code and
    // every key-value entry across pages.
    let directory = std::fs::read_dir(&run.backup).unwrap().next().unwrap().unwrap().path();
    let read = |name: &str| std::fs::read_to_string(directory.join(name)).unwrap();
    assert!(read("shelly-getconfig.json").contains("lamp"));
    assert!(read("script-1.json").contains("Switch.Toggle"));
    assert!(read("kvs-getmany.json").contains("\"c\""));
    assert!(
        directory
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("shelly-backup-S3SW-001X16EU-AABBCCDDEEFF-")
    );
}

#[tokio::test]
async fn logs_in_with_digest_and_works_on_ethernet() {
    let device = Gen2 { auth: true, rssi: None, without_scripts: true, ..gen2::gen2() };
    let run = run_with(device, |setup| {
        setup.credentials = Some("admin:secret");
        let mut layout = LAYOUT.to_vec();
        layout.retain(|entry| entry.0 != "shelly");
        setup.fixture.table = table(&layout);
    })
    .await;
    assert_eq!(PASSWORD, "secret");
    run.result.as_ref().expect("flashed");
    assert!(run.recorder.summaries[0].contains("no Wi-Fi (Ethernet)"));
    assert!(run.recorder.messages[0].starts_with("Note: the partition table differs in shelly"));
    let directory = std::fs::read_dir(&run.backup).unwrap().next().unwrap().unwrap().path();
    assert!(!directory.join("script-list.json").exists());
    assert!(!directory.join("kvs-getmany.json").exists());
}

/// A device, a change to the setup and the message of the refusal.
type Refusal = (Gen2, fn(&mut Setup), &'static str);

#[tokio::test]
async fn refuses_before_any_change() {
    let cases: [Refusal; 7] = [
        (
            Gen2 { firmware: "2.1.1", ..gen2::gen2() },
            |_| {},
            "the device runs firmware 2.1.1, the ESPHome package is built on the official 2.0.3. \
             Update the device to 2.0.3 first",
        ),
        (
            Gen2 { app: "S1PMG3", ..gen2::gen2() },
            |_| {},
            "the official package is for S1G3, the device runs S1PMG3. Nothing was changed.",
        ),
        (Gen2 { rssi: Some(-90), ..gen2::gen2() }, |_| {}, "Wi-Fi signal too weak: -90 dBm"),
        (Gen2 { auth: true, ..gen2::gen2() }, |_| {}, "the device requires a login"),
        (
            Gen2 { auth: true, ..gen2::gen2() },
            |setup| setup.credentials = Some("admin:wrong"),
            "the device refused the login",
        ),
        (gen2::gen2(), |setup| setup.fixture.bootloader[0] = 0, "no valid bootloader at 0x0"),
        (gen2::gen2(), |setup| setup.recorder.decline = true, "aborted, nothing was changed"),
    ];
    for (device, change, message) in cases {
        let run = run_with(device, change).await;
        assert!(run.error().contains(message), "{}", run.error());
        assert!(!run.log_changed() && !run.updated(), "{:?}", run.requests());
    }
    let broken = run(Gen2 { broken_wifi: true, ..gen2::gen2() }).await;
    assert_eq!(broken.error(), "Wifi.GetStatus gave an answer that is no RPC reply");
}

#[tokio::test]
async fn refuses_slot_1_and_puts_the_log_back() {
    let run = run(Gen2 { core_dump: Some("app_1 (0x200000)"), ..gen2::gen2() }).await;
    assert!(matches!(run.result, Err(FlashError::TargetSlot(1))));
    assert!(run.error().contains("Run one official update first"));
    assert!(!run.updated());
    assert_eq!(run.requests().last().map(String::as_str), Some("log -> null"));
}

#[tokio::test]
async fn asks_when_the_slot_is_unknown() {
    // The firmware names no slot; the user stops.
    let declined =
        run_with(Gen2 { core_dump: Some("scratch (0x720000)"), ..gen2::gen2() }, |setup| {
            setup.recorder.decline_from = Some(1);
        })
        .await;
    assert!(matches!(declined.result, Err(FlashError::Aborted)));
    assert!(declined.recorder.summaries[1].contains("this firmware does not name it in its log"));
    assert_eq!(declined.requests().last().map(String::as_str), Some("log -> null"));

    // The log arrives but names nothing in time.
    let silent = run_with(Gen2 { core_dump: None, ..gen2::gen2() }, |setup| {
        setup.recorder.decline_from = Some(1);
    })
    .await;
    assert!(silent.recorder.summaries[1].contains("the device log did not name it in time"));

    // No log at all: the user goes on, and ESPHome decides.
    let blind = run(Gen2 { send_log: false, ..gen2::gen2() }).await;
    blind.result.as_ref().expect("flashed");
    assert!(blind.recorder.summaries[1].contains("did not arrive on UDP port"));
    assert!(blind.recorder.summaries[1].contains("(a firewall on this computer?)"));
}

#[tokio::test]
async fn stops_on_what_the_installer_logs() {
    let kept = run(Gen2 { bootloader_update: false, ..gen2::gen2() }).await;
    assert_eq!(
        kept.result.as_ref().err().and_then(FlashError::report_kind).as_deref(),
        Some("bootloader-kept")
    );
    assert!(
        kept.error().contains("kept Shelly's bootloader (installed 1.0.2, package minimum 1.0.9)")
    );

    let wrong = run(Gen2 { will_write: 1, ..gen2::gen2() }).await;
    assert!(matches!(wrong.result, Err(FlashError::WrongSlotWritten(1))));
}

#[tokio::test]
async fn explains_a_refused_update_and_a_missing_download() {
    let refused = run(Gen2 { refuse_update: Some("Not committed"), ..gen2::gen2() }).await;
    assert!(refused.error().contains("Not committed. Right after a start"));
    assert_eq!(refused.requests().last().map(String::as_str), Some("log -> null"));

    let idle = run_with(Gen2 { download: false, ..gen2::gen2() }, |setup| {
        setup.options.timeouts.package_download = Duration::from_millis(300);
    })
    .await;
    assert!(matches!(idle.result, Err(FlashError::PackageNotFetched(_))));

    // The log setting cannot be put back: the run says so.
    let stuck = run(Gen2 { refuse_update: Some("busy"), fail_restore: true, ..gen2::gen2() }).await;
    assert!(stuck.recorder.messages.iter().any(|message| {
        message.starts_with("Note: the device log setting was not put back (Sys.SetConfig failed")
    }));
}

#[tokio::test]
async fn waits_for_a_fresh_device_and_for_esphome() {
    let fresh = run_with(Gen2 { uptime: 0, report_success: false, ..gen2::gen2() }, |setup| {
        setup.options.timeouts.settle = Duration::from_secs(1);
    })
    .await;
    fresh.result.as_ref().expect("flashed");
    assert!(fresh.recorder.messages.iter().any(|message| message.ends_with("waiting 1 s")));

    let note = "whether it wrote ESPHome's bootloader is unknown";
    assert!(!fresh.recorder.messages.iter().any(|message| message.contains(note)));

    // The bootloader decision got lost: ESPHome may answer only until a
    // few restarts, so the run says that it is unconfirmed.
    let unconfirmed = run(Gen2 { boot_line: false, ..gen2::gen2() }).await;
    unconfirmed.result.as_ref().expect("flashed");
    assert!(unconfirmed.recorder.messages.iter().any(|message| message.contains(note)));

    // ESPHome does not come and the stock firmware still answers: the log
    // setting is put back, and the run says why.
    let silent = run_with(Gen2 { open_esphome_port: false, ..gen2::gen2() }, |setup| {
        setup.options.timeouts.esphome_boot = Duration::from_millis(300);
    })
    .await;
    assert!(matches!(silent.result, Err(FlashError::EsphomeNotSeen(_))));
    assert_eq!(silent.requests().last().map(String::as_str), Some("log -> null"));
    let still_stock = "The Shelly firmware still answers, so ESPHome is not running";
    assert!(silent.recorder.messages.iter().any(|message| message.starts_with(still_stock)));

    // Nothing answers the log setting either: no claim about the firmware.
    let gone =
        run_with(Gen2 { open_esphome_port: false, fail_restore: true, ..gen2::gen2() }, |setup| {
            setup.options.timeouts.esphome_boot = Duration::from_millis(300)
        })
        .await;
    assert!(matches!(gone.result, Err(FlashError::EsphomeNotSeen(_))));
    assert!(!gone.recorder.messages.iter().any(|message| message.starts_with(still_stock)));
}

#[tokio::test]
async fn reports_what_this_computer_cannot_do() {
    let taken = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let log = run_with(gen2::gen2(), |setup| setup.options.log_port = port).await;
    assert!(log.error().starts_with("cannot listen for the device log on UDP 127.0.0.1:"));

    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = busy.local_addr().unwrap().port();
    let server = run_with(gen2::gen2(), |setup| setup.options.serve_port = port).await;
    assert!(server.error().starts_with("network error on this computer"));
    assert!(!server.updated());
    assert_eq!(server.requests().last().map(String::as_str), Some("log -> null"));
}

#[tokio::test]
async fn stops_when_the_device_fails_a_call() {
    for method in [
        "Sys.GetStatus",
        "Shelly.GetDeviceInfo",
        "Shelly.GetConfig",
        "Schedule.List",
        "Script.GetCode",
        "KVS.GetMany",
        "Sys.GetConfig",
    ] {
        let run = run(Gen2 { fail_method: Some(method), ..gen2::gen2() }).await;
        assert!(
            run.error().contains(&format!("{method} answered with HTTP 500")),
            "{}",
            run.error()
        );
        assert!(!run.log_changed(), "{method}");
    }
}

#[tokio::test]
async fn backs_up_only_when_asked_and_stops_when_it_cannot() {
    let skipped = run_with(gen2::gen2(), |setup| setup.options.backup_dir = None).await;
    skipped.result.as_ref().expect("flashed");
    assert!(!skipped.recorder.messages.iter().any(|message| message.starts_with("Stock settings")));

    let file = scratch_dir();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "not a directory").unwrap();
    let blocked =
        run_with(gen2::gen2(), |setup| setup.options.backup_dir = Some(file.join("x"))).await;
    assert!(blocked.error().starts_with("could not write the backup"), "{}", blocked.error());
    assert!(!blocked.log_changed());

    // A script cut inside a character would be saved garbled: stop instead.
    let garbled = run(Gen2 { garbled_script: true, ..gen2::gen2() }).await;
    assert!(garbled.error().starts_with("the code of script 1 did not arrive intact"));
    assert!(!garbled.log_changed());
}

#[tokio::test]
async fn stops_when_the_host_cannot_be_resolved() {
    let (host, _) = gen2::start(gen2::gen2()).await;
    let Ok(Firmware::Newer { info, .. }) = DeviceClient::new(&host, None).identify().await else {
        panic!("not a Gen2+ device");
    };
    let fixture = Fixture::default();
    let stock = StockPackage::parse(&fixture.package()).unwrap();
    for (target, serve_ip) in
        [("256.0.0.1", setup().options.serve_ip), ("255.255.255.255:80", None)]
    {
        // Linux refuses a route to the broadcast address; elsewhere the run
        // goes on and fails to connect.
        if serve_ip.is_none() && !cfg!(target_os = "linux") {
            continue;
        }
        let flash = Esp32Flash {
            info: &info,
            credentials: None,
            factory: fixture.factory(),
            stock: &stock,
            source: PackageSource::File,
        };
        let options = FlashOptions { serve_ip, ..setup().options };
        let error = flash_esp32::run(target, flash, &options, &mut Recorder::default()).await;
        assert!(matches!(error, Err(FlashError::Io(_))), "{target}");
    }
}
