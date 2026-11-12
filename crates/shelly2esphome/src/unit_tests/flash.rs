//! Unit tests of `crate::flash` (not counted as production code for coverage).

use bytes::Bytes;

use super::*;

#[tokio::test]
async fn resolves_all_host_forms() {
    assert_eq!(resolve("10.10.5.78").await.expect("ip").to_string(), "10.10.5.78:80");
    assert_eq!(resolve("10.10.5.78:8080").await.expect("ip:port").port(), 8080);
    assert_eq!(resolve("localhost").await.expect("name").port(), 80);
    assert_eq!(resolve("localhost:81").await.expect("name:port").port(), 81);
    assert!(resolve("does-not-exist.invalid").await.is_err());
}

#[test]
fn local_ip_for_loopback_is_loopback() {
    let ip = local_ip_towards(IpAddr::from([127, 0, 0, 1])).expect("route");
    assert!(ip.is_loopback());
}

#[test]
fn default_options_are_conservative() {
    let options = FlashOptions::default();
    assert_eq!(options.min_rssi_dbm, -75);
    assert!(!options.allow_weak_signal);
    assert_eq!(options.esphome_port, 6053);
}

#[test]
fn uses_an_ipv6_socket_for_ipv6_targets() {
    // Hosts without IPv6 refuse the socket; otherwise the answer is IPv6.
    if let Ok(ip) = local_ip_towards(IpAddr::from([0_u16, 0, 0, 0, 0, 0, 0, 1])) {
        assert!(ip.is_ipv6());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn reports_when_no_local_address_can_reach_the_target() {
    // Linux refuses to connect a UDP socket to the broadcast address.
    assert!(local_ip_towards(IpAddr::from([255, 255, 255, 255])).is_err());
}

#[tokio::test]
async fn names_every_failure_for_device_reports() {
    let http = crate::http::builder()
        .build()
        .expect("client")
        .get("no url")
        .send()
        .await
        .expect_err("no url");
    let rpc_http = crate::http::builder()
        .build()
        .expect("client")
        .get("no url")
        .send()
        .await
        .expect_err("no url");
    let model: ModelId = serde_json::from_str("\"S3SW-001X16EU\"").expect("model");
    let generation = Generation::try_from(3).expect("generation");
    let refusals = [
        FlashError::Device(DeviceError::AuthRequired),
        // The login is missing only for the settings the backup reads.
        FlashError::Backup(BackupError::Device(DeviceError::AuthRequired)),
        // A package for another model, refused before the confirmation.
        FlashError::Package(PackageError::WrongModel {
            expected: "ShellyDimmer2",
            found: "Shelly1".into(),
        }),
        FlashError::UnsupportedModel("SHXX-1".into()),
        FlashError::UnsupportedGeneration { model, generation },
        FlashError::WeakSignal { rssi_dbm: -90, min_rssi_dbm: -75 },
        FlashError::PackageRequired("mg2esphome-Shelly1.zip".into()),
        FlashError::Aborted,
        FlashError::Busy(MigrationState::Downloading),
        FlashError::ImageTooLarge { size: 2, max_size: 1 },
        FlashError::Rpc(RpcError::AuthRequired),
        FlashError::Rpc(RpcError::AuthFailed),
        FlashError::Backup(BackupError::Rpc(RpcError::AuthRequired)),
        FlashError::Backup(BackupError::Rpc(RpcError::AuthFailed)),
        FlashError::PackageMismatch { device: "S1G3".into(), package: "S1PMG3".into() },
        FlashError::VersionMismatch { device: "2.1.1".into(), package: "2.2.0".into() },
        FlashError::Build(BuildError::NoOtadata),
        FlashError::TargetSlot(1),
    ];
    for refusal in refusals {
        assert_eq!(refusal.report_kind(), None, "{refusal}");
    }
    let failures = [
        (FlashError::Device(DeviceError::Http(http)), "device-unreachable"),
        (
            FlashError::Device(DeviceError::Status { endpoint: "/x", status: 500 }),
            "device-http-status",
        ),
        (FlashError::Device(DeviceError::Rejected("no".into())), "migration-rejected"),
        (FlashError::Device(DeviceError::UnknownFirmware), "unknown-firmware"),
        (FlashError::Device(DeviceError::UnsupportedProtocol(9)), "protocol-mismatch"),
        (FlashError::Package(PackageError::MissingManifest), "package-invalid"),
        (FlashError::Backup(BackupError::NotStock), "backup-failed"),
        (FlashError::Io(std::io::Error::other("no route")), "local-network"),
        (FlashError::PackageNotFetched(FetchProblem::OtherAddress(1)), "package-not-fetched"),
        (FlashError::MigrationNotReachable, "migration-not-reachable"),
        (FlashError::RelocationFailed("flash".into()), "relocation-failed"),
        (FlashError::DeviceFailed("hash_mismatch".into()), "device-failed:hash_mismatch"),
        (FlashError::DeviceFailed("http_404".into()), "device-failed:http_404"),
        // Anything the firmware does not set becomes "other": it could be a
        // MAC, a name or an address.
        (FlashError::DeviceFailed("aabbccddeeff".into()), "device-failed:other"),
        (FlashError::DeviceFailed("my-kitchen-light".into()), "device-failed:other"),
        (FlashError::DeviceFailed("bad 10.0.0.2".into()), "device-failed:other"),
        (FlashError::DeviceFailed("http_4040".into()), "device-failed:other"),
        (FlashError::DeviceFailed("http_abc".into()), "device-failed:other"),
        (FlashError::DeviceFailed(String::new()), "device-failed:other"),
        (FlashError::StagingSlotTooSmall { size: 2, max_size: 1 }, "staging-slot-too-small"),
        (FlashError::InstallFatal, "install-fatal"),
        (FlashError::FlashTimeout, "flash-timeout"),
        (FlashError::EsphomeNotSeen("10.0.0.2:6053".parse().expect("addr")), "esphome-not-seen"),
        (FlashError::Rpc(RpcError::Http(rpc_http)), "device-unreachable"),
        (
            FlashError::Rpc(RpcError::Status { method: "X".into(), status: 500 }),
            "device-http-status",
        ),
        (FlashError::Rpc(RpcError::Unreadable("X".into())), "device-http-status"),
        (
            FlashError::Rpc(RpcError::Refused {
                method: "X".into(),
                code: -1,
                message: "no".into(),
            }),
            "device-refused",
        ),
        (FlashError::Rpc(RpcError::AuthUnsupported), "device-refused"),
        (FlashError::Log(LogError::Rpc(RpcError::AuthFailed)), "log-unavailable"),
        (FlashError::UpdateRefused("no".into()), "update-refused"),
        (FlashError::WrongSlotWritten(1), "wrong-slot"),
        (
            FlashError::BootloaderKept {
                installed: "1.0.3".parse().expect("version"),
                minimum: "1.0.3".parse().expect("version"),
            },
            "bootloader-kept",
        ),
    ];
    for (failure, kind) in &failures {
        assert_eq!(failure.report_kind().as_deref(), Some(*kind), "{failure}");
        assert!(is_report_kind(kind), "{kind}");
    }
    // Every listed kind is one a failure names.
    for kind in REPORT_KINDS {
        assert!(failures.iter().any(|(_, named)| *named == kind), "{kind}");
    }
}

#[test]
fn knows_only_the_kinds_it_names() {
    for text in [
        "success",
        "foo-bar",
        "Device-unreachable",
        "device-failed:",
        "device-failed:anything",
        "device-failed:http_40",
        "device-failed:hash_mismatch ",
    ] {
        assert!(!is_report_kind(text), "{text}");
    }
}

async fn package_server(allowed_peer: IpAddr, package: Bytes) -> FileServer {
    let bind = SocketAddr::from(([127, 0, 0, 1], 0));
    FileServer::start(bind, package, Bytes::new(), Some(allowed_peer)).await.expect("server")
}

#[tokio::test]
async fn tells_why_the_package_was_not_fetched() {
    let device = IpAddr::from([127, 0, 0, 1]);
    let idle = package_server(device, Bytes::from_static(b"zip")).await;
    let start = FetchStart::of(&idle);
    assert!(!start.requested(&idle));
    assert_eq!(start.problem(&idle, 3), FetchProblem::NoRequest(idle.address()));

    // A router that translates addresses: the request comes from elsewhere.
    let translated = package_server(IpAddr::from([10, 0, 0, 2]), Bytes::new()).await;
    let start = FetchStart::of(&translated);
    let refused = reqwest::get(translated.url(PACKAGE_PATH)).await.expect("request");
    assert_eq!(refused.status(), reqwest::StatusCode::FORBIDDEN);
    assert_eq!(start.problem(&translated, 3), FetchProblem::OtherAddress(1));
    // A new attempt only counts what happened since it started.
    assert_eq!(
        FetchStart::of(&translated).problem(&translated, 3),
        FetchProblem::NoRequest(translated.address())
    );

    // The device asks, then gives up after the headers.
    let big = Bytes::from(vec![0_u8; 8 << 20]);
    let stopped = package_server(device, big.clone()).await;
    let start = FetchStart::of(&stopped);
    drop(reqwest::get(stopped.url(PACKAGE_PATH)).await.expect("request"));
    assert!(start.requested(&stopped));
    assert!(matches!(
        start.problem(&stopped, big.len() as u64),
        FetchProblem::Stopped { sent, total } if sent < total && total == 8 << 20
    ));
}

#[test]
fn names_the_connection_the_device_needs() {
    let address = SocketAddr::from(([10, 10, 10, 96], 65187));
    let no_request = FlashError::PackageNotFetched(FetchProblem::NoRequest(address)).to_string();
    assert!(no_request.starts_with("the device did not fetch the package: no request reached"));
    assert!(no_request.contains("open a TCP connection to 10.10.10.96:65187"));
    assert!(no_request.ends_with("Nothing was changed."));
    assert!(
        FetchProblem::OtherAddress(2).to_string().starts_with("2 request(s) came from another")
    );
    assert!(FetchProblem::OtherAddress(2).to_string().contains("(NAT)"));
    assert_eq!(
        FetchProblem::Stopped { sent: 10, total: 20 }.to_string(),
        "it stopped the download after 10 of 20 bytes. Check its Wi-Fi signal"
    );
}

/// Answers retries from a list and records the problems.
#[derive(Default)]
struct Retries {
    answers: Vec<bool>,
    asked: Vec<String>,
}

impl Reporter for Retries {
    fn info(&mut self, _message: &str) {}
    fn progress(&mut self, _sent_bytes: u64, _total_bytes: u64) {}
    fn confirm(&mut self, _summary: &str) -> bool {
        true
    }
    fn retry(&mut self, problem: &str) -> bool {
        self.asked.push(problem.to_owned());
        self.answers.get(self.asked.len() - 1).copied().unwrap_or(false)
    }
}

#[test]
fn hands_the_package_over_again_only_if_asked() {
    let mut reporter = Retries { answers: vec![true], ..Retries::default() };
    assert!(retry(FetchProblem::OtherAddress(1), &mut reporter).is_ok());
    let declined = retry(FetchProblem::OtherAddress(1), &mut reporter).expect_err("declined");
    assert!(matches!(declined, FlashError::PackageNotFetched(FetchProblem::OtherAddress(1))));
    assert_eq!(reporter.asked.len(), 2);
    assert!(reporter.asked[0].starts_with("the device did not fetch the package: 1 request(s)"));
}
