//! Unit tests of `crate::flash` (not counted as production code for coverage).

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
        (FlashError::PackageNotFetched("10.0.0.2:8080".into()), "package-not-fetched"),
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
