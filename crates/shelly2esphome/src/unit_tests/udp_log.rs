//! Unit tests of `crate::udp_log` (not counted as production code for coverage).

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{Router, extract::State, response::IntoResponse, routing::post};

use super::*;

fn version(text: &str) -> BootloaderVersion {
    text.parse().expect("version")
}

#[test]
fn reads_the_lines_flash_needs() {
    assert_eq!(
        parse("shos_ota_esp32_back:949 Storing core dumps to app_1 (0x200000)"),
        Some(LogEvent::CoreDumpTarget(Some(1)))
    );
    assert_eq!(
        parse("Storing core dumps to scratch (0x720000)"),
        Some(LogEvent::CoreDumpTarget(None))
    );
    assert_eq!(parse("Storing core dumps to app_x"), Some(LogEvent::CoreDumpTarget(None)));
    assert_eq!(parse("shelly_ota.c:12 Will write to slot 0"), Some(LogEvent::WillWrite(0)));
    assert_eq!(parse("Will write to slot x"), None);
    assert_eq!(
        parse("Boot: cur 010003ff, new 00000000, min 010009ff, update? 1"),
        Some(LogEvent::Bootloader {
            installed: version("1.0.3"),
            minimum: version("1.0.9"),
            update: true
        })
    );
    assert_eq!(
        parse("x Boot: cur 010003ff, new 00000000, min 010003ff, update? 0 y"),
        Some(LogEvent::Bootloader {
            installed: version("1.0.3"),
            minimum: version("1.0.3"),
            update: false
        })
    );
    for broken in [
        "Boot: cur 0100zzff, new 00000000, min 010009ff, update? 1",
        "Boot: cur 010003ff, new 00000000, min 0100, update? 1",
        "Boot: cur 010003ff, new 00000000, update? 1",
        "Boot: cur 010003ff, new 00000000, min 010009ff, update? x",
        "Boot: cur 0103",
    ] {
        assert_eq!(parse(broken), None, "{broken}");
    }
    assert_eq!(parse("shelly_ota: Update succeeded"), Some(LogEvent::Succeeded));
    assert_eq!(parse("New min heap free: 1234"), None);
}

#[derive(Debug, Default)]
struct Device {
    addr: serde_json::Value,
    calls: Vec<String>,
    refuse_set: bool,
    refuse_get: bool,
}

async fn rpc(State(device): State<Arc<Mutex<Device>>>, body: String) -> axum::response::Response {
    let request: serde_json::Value = serde_json::from_str(&body).expect("JSON");
    let mut device = device.lock().expect("lock");
    let method = request["method"].as_str().unwrap_or_default().to_owned();
    match method.as_str() {
        "Sys.GetConfig" if device.refuse_get => {
            r#"{"id":1,"error":{"code":-103,"message":"locked"}}"#.into_response()
        }
        "Sys.GetConfig" => {
            device.calls.push(method);
            serde_json::json!({ "id": 1, "result": { "debug": { "udp": { "addr": device.addr } } } })
                .to_string()
                .into_response()
        }
        _ if device.refuse_set => {
            r#"{"id":1,"error":{"code":-103,"message":"no"}}"#.into_response()
        }
        _ => {
            let addr = request["params"]["config"]["debug"]["udp"]["addr"].clone();
            device.calls.push(format!("{method} {addr}"));
            device.addr = addr;
            r#"{"id":1,"result":{"restart_required":false}}"#.into_response()
        }
    }
}

async fn start(device: Device) -> (RpcClient, Arc<Mutex<Device>>) {
    let shared = Arc::new(Mutex::new(device));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let app = Router::new().route("/rpc", post(rpc)).with_state(Arc::clone(&shared));
    tokio::spawn(async move { axum::serve(listener, app).await });
    (RpcClient::new(&address.to_string(), None), shared)
}

fn local() -> SocketAddr {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("free port");
    socket.local_addr().expect("address")
}

const DEVICE: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

#[tokio::test]
async fn points_the_log_here_and_puts_it_back() {
    let (rpc, device) = start(Device { addr: "10.0.0.5:514".into(), ..Device::default() }).await;
    let local = local();
    let mut log = UdpLog::start(&rpc, DEVICE, local).await.expect("log");
    let sender = std::net::UdpSocket::bind("127.0.0.1:0").expect("sender");
    sender
        .send_to(b"noise\nshos_ota Storing core dumps to app_0 (0x10000)\nBoot: cur 1", local)
        .expect("send");
    sender.send_to(b"Update succeeded", local).expect("send");
    let deadline = Instant::now() + Duration::from_secs(5);
    assert_eq!(log.next_event(deadline).await, Some(LogEvent::CoreDumpTarget(Some(0))));
    assert_eq!(log.next_event(deadline).await, Some(LogEvent::Succeeded));
    assert_eq!(log.datagrams(), 2);
    assert_eq!(log.next_event(Instant::now() + Duration::from_millis(50)).await, None);
    log.restore(&rpc).await.expect("restored");
    assert_eq!(
        device.lock().expect("lock").calls,
        ["Sys.GetConfig", &format!("Sys.SetConfig \"{local}\""), "Sys.SetConfig \"10.0.0.5:514\""]
    );
}

#[tokio::test]
async fn switches_an_old_destination_off_first_and_ignores_other_senders() {
    let local = local();
    let (rpc, device) = start(Device { addr: local.to_string().into(), ..Device::default() }).await;
    // Datagrams from anyone but the device are not the device's log.
    let other = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 1));
    let mut log = UdpLog::start(&rpc, other, local).await.expect("log");
    let sender = std::net::UdpSocket::bind("127.0.0.1:0").expect("sender");
    sender.send_to(b"Update succeeded", local).expect("send");
    assert_eq!(log.next_event(Instant::now() + Duration::from_millis(300)).await, None);
    assert_eq!(log.datagrams(), 0);
    drop(log);
    assert_eq!(
        device.lock().expect("lock").calls,
        ["Sys.GetConfig", "Sys.SetConfig null", &format!("Sys.SetConfig \"{local}\"")]
    );
}

#[tokio::test]
async fn reports_what_stops_the_log() {
    let (rpc, _) = start(Device { refuse_set: true, ..Device::default() }).await;
    let error = UdpLog::start(&rpc, DEVICE, local()).await.expect_err("refused");
    assert_eq!(
        error.to_string(),
        "cannot switch on the device log: Sys.SetConfig failed: no (code -103)"
    );
    let (rpc, _) = start(Device { refuse_get: true, ..Device::default() }).await;
    let error = UdpLog::start(&rpc, DEVICE, local()).await.expect_err("locked");
    assert!(error.to_string().contains("Sys.GetConfig failed: locked"));
    // The old destination cannot be switched off.
    let ours = local();
    let (rpc, _) =
        start(Device { addr: ours.to_string().into(), refuse_set: true, ..Device::default() })
            .await;
    let error = UdpLog::start(&rpc, DEVICE, ours).await.expect_err("refused");
    assert!(error.to_string().contains("Sys.SetConfig failed: no"));

    let taken = std::net::UdpSocket::bind("127.0.0.1:0").expect("taken");
    let address = taken.local_addr().expect("address");
    let error = UdpLog::start(&rpc, DEVICE, address).await.expect_err("port taken");
    assert!(
        error
            .to_string()
            .starts_with(&format!("cannot listen for the device log on UDP {address}: ")),
        "{error}"
    );
    assert!(error.to_string().ends_with("(try --log-port)"));
}
