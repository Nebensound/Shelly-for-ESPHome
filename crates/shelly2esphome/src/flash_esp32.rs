//! `flash` for Shelly Gen2 and later (ESP32, ESP32-C3). There is no
//! migration firmware: the stock updater installs the ESPHome package
//! (`esphome_package`), which carries ESPHome's application and bootloader.
//!
//! Every refusal comes before the confirmation, and the backup of the stock
//! settings right after it. The only change before the update is the
//! destination of the device log, which the slot check needs; it is put
//! back before any refusal.

use std::net::SocketAddr;

use bytes::Bytes;
use tokio::time::{Instant, sleep};

use crate::{
    backup,
    device::Credentials,
    esphome_package::EsphomePackage,
    flash::{
        FlashError, FlashOptions, FlashOutcome, Reporter, Timeouts, local_ip_towards, resolve,
        wait_for_esphome,
    },
    profile::BOOT_MIN_VERSION,
    report::{DeviceFacts, PackageSource, ReportedModel},
    rpc::{DeviceInfo, WifiSignal, WifiStatusWire},
    rpc_client::{RpcClient, RpcError},
    server::{FileServer, PACKAGE_PATH},
    stock_package::StockPackage,
    udp_log::{LogEvent, UdpLog},
};

/// What `flash` needs for a Gen2+ device besides the options.
#[derive(Debug)]
pub struct Esp32Flash<'a> {
    /// What the device reported about itself (`/shelly`).
    pub info: &'a DeviceInfo,
    /// Login if web authentication is enabled.
    pub credentials: Option<Credentials>,
    /// ESPHome's `firmware.factory.bin`.
    pub factory: Vec<u8>,
    /// The official package of the device's app.
    pub stock: &'a StockPackage,
    /// Where the official package came from.
    pub source: PackageSource,
}

fn summary(info: &DeviceInfo, signal: WifiSignal, package: &EsphomePackage) -> String {
    let wifi = match signal {
        WifiSignal::Dbm(rssi) => format!("Wi-Fi {rssi} dBm"),
        _ => "no Wi-Fi (Ethernet)".to_owned(),
    };
    format!(
        "Device: {} ({}, {}), stock firmware {}, {wifi}\n\
         ESPHome package: {} bytes with the {} application ({} bytes) and ESPHome's bootloader \
         (min_version {BOOT_MIN_VERSION})\n\
         Steps: the stock updater installs ESPHome and its bootloader. Going back to the Shelly \
         firmware afterwards is only possible over a cable.",
        info.app,
        info.model,
        info.generation,
        info.firmware,
        package.bytes().len(),
        package.factory().app().chip,
        package.factory().app().len,
    )
}

/// The slot the core dump line names: `Some(Some(N))` for `app_N`,
/// `Some(None)` for a partition that is no slot, `None` without that line.
async fn probe_slot(log: &mut UdpLog, timeouts: &Timeouts) -> Option<Option<u8>> {
    let deadline = Instant::now() + timeouts.slot_probe;
    while let Some(event) = log.next_event(deadline).await {
        if let LogEvent::CoreDumpTarget(target) = event {
            return Some(target);
        }
    }
    None
}

/// Checks the slot the installer will write to; asks if it is unknown.
async fn check_slot(
    log: &mut UdpLog,
    options: &FlashOptions,
    reporter: &mut dyn Reporter,
) -> Result<(), FlashError> {
    let reason = match probe_slot(log, &options.timeouts).await {
        Some(Some(0)) => {
            reporter
                .info("The stock installer writes to slot 0, which ESPHome's bootloader starts");
            return Ok(());
        }
        Some(Some(slot)) => return Err(FlashError::TargetSlot(slot)),
        Some(None) => "this firmware does not name it in its log".to_owned(),
        None if log.datagrams() == 0 => format!(
            "the device log did not arrive on UDP port {} (a firewall on this computer?)",
            options.log_port
        ),
        None => "the device log did not name it in time".to_owned(),
    };
    let question = format!(
        "The slot the stock installer will write to is unknown: {reason}. If it is slot 1, the \
         stock firmware starts again after the update; one official update and another flash \
         fix that."
    );
    if reporter.confirm(&question) { Ok(()) } else { Err(FlashError::Aborted) }
}

/// `Shelly.Update` refused, in words for the user.
fn refusal(error: RpcError) -> FlashError {
    match error {
        RpcError::Refused { message, .. } if message.to_lowercase().contains("not committed") => {
            FlashError::UpdateRefused(format!(
                "{message}. Right after a start the stock firmware accepts an update only once it \
                 has confirmed itself; run flash again in two minutes. Nothing was changed."
            ))
        }
        RpcError::Refused { message, .. } => {
            FlashError::UpdateRefused(format!("{message}. Nothing was changed."))
        }
        other => other.into(),
    }
}

/// Hands the package to the stock updater and follows the installation.
async fn update(
    rpc: &RpcClient,
    log: &mut UdpLog,
    server: &FileServer,
    total: u64,
    timeouts: &Timeouts,
    reporter: &mut dyn Reporter,
) -> Result<(), FlashError> {
    rpc.call("Shelly.Update", serde_json::json!({ "url": server.url(PACKAGE_PATH) }))
        .await
        .map_err(refusal)?;
    let download_deadline = Instant::now() + timeouts.package_download;
    let mut result_deadline = None;
    loop {
        reporter.progress(server.package_stats().sent_bytes(), total);
        if result_deadline.is_none() && server.package_stats().completed() > 0 {
            reporter.info(
                "Package downloaded; the stock installer installs ESPHome – do not cut the power \
                 now",
            );
            result_deadline = Some(Instant::now() + timeouts.update_result);
        }
        let limit = result_deadline.unwrap_or(download_deadline);
        match log.next_event(limit.min(Instant::now() + timeouts.poll_interval)).await {
            Some(LogEvent::WillWrite(0)) => reporter.info("The stock installer writes to slot 0"),
            Some(LogEvent::WillWrite(slot)) => return Err(FlashError::WrongSlotWritten(slot)),
            Some(LogEvent::Bootloader { update: true, .. }) => {
                reporter.info("The stock installer writes ESPHome's bootloader");
            }
            Some(LogEvent::Bootloader { installed, minimum, update: false }) => {
                return Err(FlashError::BootloaderKept { installed, minimum });
            }
            Some(LogEvent::Succeeded) => {
                reporter.info("The stock installer reports success; the device restarts");
                return Ok(());
            }
            Some(LogEvent::CoreDumpTarget(_)) | None => {}
        }
        if Instant::now() >= limit {
            // Downloaded but not confirmed: whether ESPHome answers decides.
            return match result_deadline {
                Some(_) => Ok(()),
                None => Err(FlashError::PackageNotFetched(server.address().to_string())),
            };
        }
    }
}

/// Runs the whole procedure against the Gen2+ device at `host`.
///
/// # Errors
/// Returns a [`FlashError`] describing where and why the run stopped and
/// whether the device was changed.
pub async fn run(
    host: &str,
    flash: Esp32Flash<'_>,
    options: &FlashOptions,
    reporter: &mut dyn Reporter,
) -> Result<FlashOutcome, FlashError> {
    let device_address = resolve(host).await?;
    let serve_ip = match options.serve_ip {
        Some(ip) => ip,
        None => local_ip_towards(device_address.ip())?,
    };
    let Esp32Flash { info, credentials, factory, stock, source } = flash;
    let mut facts = DeviceFacts {
        model: Some(ReportedModel::Newer { app: info.app.clone(), generation: info.generation }),
        stock_firmware: info.build.clone(),
        rssi_dbm: None,
        login: Some(info.auth),
        backup: None,
        package: Some(source),
    };
    reporter.learned(&facts);
    if stock.name() != &info.app {
        return Err(FlashError::PackageMismatch {
            device: info.app.to_string(),
            package: stock.name().to_string(),
        });
    }
    if stock.version() != &info.firmware {
        return Err(FlashError::VersionMismatch {
            device: info.firmware.to_string(),
            package: stock.version().to_string(),
        });
    }
    let (package, notes) = EsphomePackage::build(stock, factory)?;
    for note in notes {
        reporter.info(&format!("Note: the partition table differs in {note}"));
    }
    let rpc = RpcClient::new(host, credentials);
    let wifi = rpc.call("Wifi.GetStatus", serde_json::json!({})).await?;
    let signal = serde_json::from_value::<WifiStatusWire>(wifi)
        .map_err(|_| RpcError::Unreadable("Wifi.GetStatus".to_owned()))?
        .signal();
    if let WifiSignal::Dbm(rssi_dbm) = signal {
        facts.rssi_dbm = Some(rssi_dbm);
        reporter.learned(&facts);
        if rssi_dbm < options.min_rssi_dbm && !options.allow_weak_signal {
            return Err(FlashError::WeakSignal { rssi_dbm, min_rssi_dbm: options.min_rssi_dbm });
        }
    }
    let status = rpc.call("Sys.GetStatus", serde_json::json!({})).await?;
    let uptime = std::time::Duration::from_secs(status["uptime"].as_u64().unwrap_or_default());
    if !reporter.confirm(&summary(info, signal, &package)) {
        return Err(FlashError::Aborted);
    }
    if let Some(parent) = &options.backup_dir {
        let directory = backup::collect_rpc(&rpc, info).await?.write_to(parent)?;
        reporter.info(&format!("Stock settings saved to {}", directory.display()));
    }
    facts.backup = Some(options.backup_dir.is_some());
    reporter.learned(&facts);

    let total = package.bytes().len() as u64;
    let local = SocketAddr::new(serve_ip, options.log_port);
    let mut log = UdpLog::start(&rpc, device_address.ip(), local).await?;
    let mut result = check_slot(&mut log, options, reporter).await;
    if result.is_ok() {
        if let Some(rest) =
            options.timeouts.settle.checked_sub(uptime).filter(|rest| !rest.is_zero())
        {
            reporter.info(&format!(
                "The stock firmware accepts an update only after it has run for a while; waiting \
                 {} s",
                rest.as_secs()
            ));
            sleep(rest).await;
        }
        let bytes = Bytes::copy_from_slice(package.bytes());
        let bind = SocketAddr::new(serve_ip, options.serve_port);
        result = match FileServer::start(bind, bytes, Bytes::new(), Some(device_address.ip())).await
        {
            Ok(server) => {
                reporter.info("Handing the ESPHome package to the stock updater …");
                update(&rpc, &mut log, &server, total, &options.timeouts, reporter).await
            }
            Err(error) => Err(error.into()),
        };
    }
    match result {
        // The device now restarts into ESPHome; the log setting went with
        // the stock firmware.
        Ok(()) => drop(log),
        Err(error) => {
            if let Err(restore) = log.restore(&rpc).await {
                reporter
                    .info(&format!("Note: the device log setting was not put back ({restore})"));
            }
            return Err(error);
        }
    }
    reporter.progress(total, total);
    reporter.info("Waiting for ESPHome …");
    let esphome_address = SocketAddr::new(device_address.ip(), options.esphome_port);
    wait_for_esphome(esphome_address, &options.timeouts).await?;
    Ok(FlashOutcome { esphome_address })
}

#[cfg(test)]
#[path = "unit_tests/flash_esp32.rs"]
mod tests;
