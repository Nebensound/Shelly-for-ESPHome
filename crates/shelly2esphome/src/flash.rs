//! The complete flash procedure: stock firmware → migration firmware → ESPHome.
//!
//! Every step before `MG2X.Flash` leaves the device recoverable without cable:
//! the stock firmware verifies the OTA package itself, and the migration
//! firmware only overwrites the bootloader after it has verified the image.

use std::{
    net::{IpAddr, SocketAddr, UdpSocket},
    time::Duration,
};

use tokio::time::{Instant, sleep, timeout};

use crate::{
    device::{DeviceClient, DeviceError, Firmware, MigrationState, MigrationStatus},
    embedded::Packages,
    image::FirmwareImage,
    migration::{MigrationPackage, PackageError},
    model::Model,
    server::{FileServer, IMAGE_PATH, PACKAGE_PATH},
};

/// Unanswered status requests after which a device that received the whole
/// image is considered to be restarting into the new firmware.
const MISSES_BEFORE_RESTART: u32 = 3;

/// Time limits of the individual phases.
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    /// Until the stock firmware has downloaded the migration package.
    pub package_fetch: Duration,
    /// Until the migration firmware answers after the stock OTA.
    pub migration_boot: Duration,
    /// Until the image is downloaded, verified and installed.
    pub flash: Duration,
    /// Until the ESPHome API port answers after installation.
    pub esphome_boot: Duration,
    /// Pause between status requests.
    pub poll_interval: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            package_fetch: Duration::from_secs(120),
            migration_boot: Duration::from_secs(240),
            flash: Duration::from_secs(300),
            esphome_boot: Duration::from_secs(180),
            poll_interval: Duration::from_secs(1),
        }
    }
}

/// Options of a flash run.
#[derive(Debug, Clone)]
pub struct FlashOptions {
    /// Weakest accepted Wi-Fi signal of the device in dBm.
    pub min_rssi_dbm: i32,
    /// Flash even if the signal is weaker than `min_rssi_dbm`.
    pub allow_weak_signal: bool,
    /// Address of this computer as seen from the device; detected if `None`.
    pub serve_ip: Option<IpAddr>,
    /// Port of the local file server; 0 picks a free one.
    pub serve_port: u16,
    /// Port of the ESPHome native API used to detect the new firmware.
    pub esphome_port: u16,
    /// HTTP port of the migration firmware (fixed to 80 in `mos.yml`).
    pub migration_port: u16,
    /// Phase time limits.
    pub timeouts: Timeouts,
    /// Migration packages used when no package is given explicitly.
    pub embedded_packages: Packages,
}

impl Default for FlashOptions {
    fn default() -> Self {
        Self {
            min_rssi_dbm: -75,
            allow_weak_signal: false,
            serve_ip: None,
            serve_port: 0,
            esphome_port: 6053,
            migration_port: 80,
            timeouts: Timeouts::default(),
            embedded_packages: Packages::BUILT_IN,
        }
    }
}

/// Receives progress information; implemented by the console UI and by tests.
pub trait Reporter {
    /// A step started or finished.
    fn info(&mut self, message: &str);
    /// Bytes of the image handed to the device so far.
    fn progress(&mut self, sent_bytes: u64, total_bytes: u64);
    /// Asks before the first change to the device; `false` aborts.
    fn confirm(&mut self, summary: &str) -> bool;
}

/// Result of a successful run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashOutcome {
    /// Address where the ESPHome API answered.
    pub esphome_address: SocketAddr,
}

/// Why a flash run stopped.
#[derive(Debug, thiserror::Error)]
pub enum FlashError {
    /// Communication with the device failed.
    #[error(transparent)]
    Device(#[from] DeviceError),
    /// The migration package is unusable.
    #[error(transparent)]
    Package(#[from] PackageError),
    /// The local file server could not be started or the host not resolved.
    #[error("Netzwerkfehler auf diesem Rechner: {0}")]
    Io(#[from] std::io::Error),
    /// The device type is not supported by the migration firmware.
    #[error("Gerätetyp {0} wird nicht unterstützt")]
    UnsupportedModel(String),
    /// The Wi-Fi signal is too weak for a safe transfer.
    #[error(
        "WLAN-Signal zu schwach: {rssi_dbm} dBm (Minimum {min_rssi_dbm} dBm). Erst den Empfang \
         verbessern oder bewusst --allow-weak-signal setzen. Nichts verändert."
    )]
    WeakSignal {
        /// Measured signal.
        rssi_dbm: i32,
        /// Configured minimum.
        min_rssi_dbm: i32,
    },
    /// The device runs stock firmware but no migration package was given.
    #[error(
        "das Gerät läuft mit der Originalfirmware und dieses Programm enthält kein \
         Migrationspaket dafür: bitte --migration {0} angeben oder das macOS-Binary aus der CI \
         verwenden"
    )]
    PackageRequired(String),
    /// The user declined the confirmation.
    #[error("abgebrochen, nichts verändert")]
    Aborted,
    /// The stock firmware did not download the package.
    #[error(
        "das Gerät hat das Migrationspaket nicht abgeholt. Erreicht es diesen Rechner \
         ({0})? Firewall/VLAN prüfen oder --serve-ip setzen. Nichts verändert."
    )]
    PackageNotFetched(String),
    /// The migration firmware did not show up after the stock OTA.
    #[error(
        "die Migrationsfirmware meldet sich nicht. Gerät neu starten und den Befehl erneut \
         ausführen; die IP-Adresse kann sich geändert haben."
    )]
    MigrationNotReachable,
    /// The migration firmware could not move itself into slot 1 yet.
    #[error(
        "die Migrationsfirmware konnte sich noch nicht in Slot 1 verschieben ({0}); sie versucht \
         es nach einem Neustart automatisch erneut. In einer Minute nochmals ausführen."
    )]
    RelocationFailed(String),
    /// The migration firmware is already working on a job.
    #[error("die Migrationsfirmware ist beschäftigt ({0:?}); kurz warten und erneut ausführen")]
    Busy(MigrationState),
    /// The image does not fit into the staging slot.
    #[error(
        "die Firmware ist mit {size} Bytes zu groß für dieses Gerät (höchstens {max_size} Bytes)"
    )]
    ImageTooLarge {
        /// Image size.
        size: u64,
        /// Slot size reported by the device.
        max_size: u64,
    },
    /// The migration firmware reported a failed job.
    #[error(
        "das Gerät meldet den Fehler „{0}“. Es bleibt in der Migrationsfirmware und nimmt einen \
         neuen Versuch an: Befehl erneut ausführen."
    )]
    DeviceFailed(String),
    /// Writing the new boot sectors and restoring the original ones both failed.
    #[error(
        "das Gerät meldet „install_fatal“: die Bootsektoren konnten weder geschrieben noch \
         wiederhergestellt werden. Gerät NICHT vom Strom trennen und nicht neu starten – die \
         Migrationsfirmware läuft weiter und nimmt einen neuen Versuch an: Befehl sofort erneut \
         ausführen. Schlägt auch das fehl oder verliert das Gerät den Strom, ist eine \
         Wiederherstellung über die serielle Schnittstelle nötig."
    )]
    InstallFatal,
    /// The job did not finish in time.
    #[error("Zeitüberschreitung beim Flashen; Status mit `probe` prüfen und erneut ausführen")]
    FlashTimeout,
    /// ESPHome did not answer after installation.
    #[error(
        "ESPHome antwortet nicht auf {0}. Gerät im ESPHome-Dashboard oder im Router suchen \
         (neue IP-Adresse?)."
    )]
    EsphomeNotSeen(SocketAddr),
}

/// Resolves `host` (`ip`, `ip:port`, `name` or `name:port`) to a socket address.
///
/// # Errors
/// Fails if the name cannot be resolved.
pub async fn resolve(host: &str) -> std::io::Result<SocketAddr> {
    if let Ok(address) = host.parse::<SocketAddr>() {
        return Ok(address);
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, 80));
    }
    let query = if host.contains(':') { host.to_owned() } else { format!("{host}:80") };
    tokio::net::lookup_host(query)
        .await?
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, host.to_owned()))
}

/// Local address used to reach `target` (no packet is sent).
///
/// # Errors
/// Fails if there is no route to `target`.
pub fn local_ip_towards(target: IpAddr) -> std::io::Result<IpAddr> {
    let socket = UdpSocket::bind(SocketAddr::new(
        if target.is_ipv4() { IpAddr::from([0, 0, 0, 0]) } else { IpAddr::from([0_u16; 8]) },
        0,
    ))?;
    socket.connect(SocketAddr::new(target, 80))?;
    Ok(socket.local_addr()?.ip())
}

/// Error reported by the migration firmware when the boot sectors are left broken.
pub const INSTALL_FATAL: &str = "install_fatal";

fn device_failure(error: String) -> FlashError {
    if error == INSTALL_FATAL { FlashError::InstallFatal } else { FlashError::DeviceFailed(error) }
}

fn stock_summary(model: Model, firmware: &str, rssi_dbm: i32, image: &FirmwareImage) -> String {
    format!(
        "Gerät: {} ({}), Originalfirmware {firmware}, WLAN {rssi_dbm} dBm\n\
         Neue Firmware: {} Bytes, SHA-256 {}\n\
         Ablauf: Migrationsfirmware installieren, dann ESPHome. Ein Zurück zur Shelly-Firmware \
         ist danach nur noch per Kabel sicher möglich.",
        model.build_name(),
        model.device_type(),
        image.len(),
        image.sha256_hex()
    )
}

fn resume_summary(image: &FirmwareImage, last_error: &str) -> String {
    let warning = if last_error == INSTALL_FATAL {
        "\nACHTUNG: Der letzte Versuch hat die Bootsektoren beschädigt hinterlassen. Bis zum \
         erfolgreichen Abschluss Strom nicht trennen und nicht neu starten."
    } else {
        ""
    };
    format!(
        "Das Gerät läuft bereits mit der Migrationsfirmware.\n\
         Neue Firmware: {} Bytes, SHA-256 {}{warning}",
        image.len(),
        image.sha256_hex()
    )
}

async fn wait_for_package(
    server: &FileServer,
    timeouts: &Timeouts,
    serve_address: SocketAddr,
) -> Result<(), FlashError> {
    let deadline = Instant::now() + timeouts.package_fetch;
    while server.package_stats().completed() == 0 {
        if Instant::now() >= deadline {
            return Err(FlashError::PackageNotFetched(serve_address.to_string()));
        }
        sleep(timeouts.poll_interval).await;
    }
    Ok(())
}

async fn wait_for_migration_firmware(
    client: &DeviceClient,
    timeouts: &Timeouts,
) -> Result<MigrationStatus, FlashError> {
    let deadline = Instant::now() + timeouts.migration_boot;
    let mut relocation_error = None;
    loop {
        // The device restarts twice; unreachable phases are expected here.
        if let Ok(Some(status)) = client.migration_status().await {
            match status.state {
                MigrationState::Idle | MigrationState::Failed => return Ok(status),
                MigrationState::RelocationFailed => relocation_error = Some(status.error),
                MigrationState::Downloading | MigrationState::Installing => {}
            }
        }
        if Instant::now() >= deadline {
            return Err(relocation_error
                .map_or(FlashError::MigrationNotReachable, FlashError::RelocationFailed));
        }
        sleep(timeouts.poll_interval).await;
    }
}

async fn monitor_flash(
    client: &DeviceClient,
    server: &FileServer,
    image_len: u64,
    timeouts: &Timeouts,
    reporter: &mut dyn Reporter,
) -> Result<(), FlashError> {
    let deadline = Instant::now() + timeouts.flash;
    let mut installing = false;
    let mut consecutive_misses = 0_u32;
    loop {
        reporter.progress(server.image_stats().sent_bytes(), image_len);
        let image_sent = server.image_stats().completed() > 0;
        match client.migration_status().await {
            Ok(Some(status)) => {
                consecutive_misses = 0;
                match status.state {
                    MigrationState::Failed | MigrationState::RelocationFailed => {
                        return Err(device_failure(status.error));
                    }
                    MigrationState::Installing if !installing => {
                        installing = true;
                        reporter
                            .info("Firmware geprüft, wird installiert – Strom jetzt nicht trennen");
                    }
                    MigrationState::Installing
                    | MigrationState::Downloading
                    | MigrationState::Idle => {}
                }
            }
            Ok(None) | Err(_) => {
                consecutive_misses += 1;
                // Gone for good after a complete transfer: it restarts into ESPHome.
                if (installing || image_sent) && consecutive_misses >= MISSES_BEFORE_RESTART {
                    return Ok(());
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(FlashError::FlashTimeout);
        }
        sleep(timeouts.poll_interval).await;
    }
}

async fn wait_for_esphome(address: SocketAddr, timeouts: &Timeouts) -> Result<(), FlashError> {
    let deadline = Instant::now() + timeouts.esphome_boot;
    loop {
        let attempt =
            timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(address)).await;
        if matches!(attempt, Ok(Ok(_))) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(FlashError::EsphomeNotSeen(address));
        }
        sleep(timeouts.poll_interval).await;
    }
}

/// Runs the whole procedure against the device at `host`.
///
/// # Errors
/// Returns a [`FlashError`] describing where and why the run stopped and
/// whether the device was changed.
pub async fn run(
    host: &str,
    client: &DeviceClient,
    image: &FirmwareImage,
    package: Option<&MigrationPackage>,
    options: &FlashOptions,
    reporter: &mut dyn Reporter,
) -> Result<FlashOutcome, FlashError> {
    let device_address = resolve(host).await?;
    let serve_ip = match options.serve_ip {
        Some(ip) => ip,
        None => local_ip_towards(device_address.ip())?,
    };
    let firmware = client.identify().await?;
    let embedded_package;

    let (stock, migration_status) = match firmware {
        Firmware::Stock { info, rssi_dbm } => {
            let model = Model::from_device_type(&info.device_type)
                .ok_or_else(|| FlashError::UnsupportedModel(info.device_type.clone()))?;
            if rssi_dbm < options.min_rssi_dbm && !options.allow_weak_signal {
                return Err(FlashError::WeakSignal {
                    rssi_dbm,
                    min_rssi_dbm: options.min_rssi_dbm,
                });
            }
            let package = match package {
                Some(package) => package,
                None => {
                    let bytes = options
                        .embedded_packages
                        .find(model)
                        .ok_or_else(|| FlashError::PackageRequired(model.migration_asset_name()))?;
                    embedded_package = MigrationPackage::parse(bytes::Bytes::from_static(bytes))?;
                    &embedded_package
                }
            };
            package.ensure_model(model)?;
            if image.len() > model.max_image_bytes() {
                return Err(FlashError::ImageTooLarge {
                    size: image.len() as u64,
                    max_size: model.max_image_bytes() as u64,
                });
            }
            if !reporter.confirm(&stock_summary(model, &info.fw, rssi_dbm, image)) {
                return Err(FlashError::Aborted);
            }
            (Some(package), None)
        }
        Firmware::Migration(status) => {
            if matches!(status.state, MigrationState::Downloading | MigrationState::Installing) {
                return Err(FlashError::Busy(status.state));
            }
            if status.state == MigrationState::RelocationFailed {
                return Err(FlashError::RelocationFailed(status.error));
            }
            if !reporter.confirm(&resume_summary(image, &status.error)) {
                return Err(FlashError::Aborted);
            }
            (None, Some(status))
        }
    };

    let package_bytes = stock.map(|package| package.bytes().clone()).unwrap_or_default();
    let server = FileServer::start(
        SocketAddr::new(serve_ip, options.serve_port),
        package_bytes,
        image.bytes().clone(),
        Some(device_address.ip()),
    )
    .await?;

    let (migration_client, status) = match migration_status {
        Some(status) => (client.clone(), status),
        None => {
            reporter.info("Migrationsfirmware wird an das Gerät übergeben …");
            client.start_stock_ota(&server.url(PACKAGE_PATH)).await?;
            wait_for_package(&server, &options.timeouts, server.address()).await?;
            reporter.info("Migrationspaket abgeholt, Gerät startet neu …");
            // The migration firmware always listens on its own port.
            let migration_host =
                SocketAddr::new(device_address.ip(), options.migration_port).to_string();
            let migration_client = DeviceClient::new(&migration_host, None)?;
            let status = wait_for_migration_firmware(&migration_client, &options.timeouts).await?;
            reporter.info("Migrationsfirmware läuft");
            (migration_client, status)
        }
    };

    let image_len = image.len() as u64;
    if image_len > status.max_size {
        return Err(FlashError::ImageTooLarge { size: image_len, max_size: status.max_size });
    }

    reporter.info(
        "ESPHome-Firmware wird übertragen und geprüft; danach installiert das Gerät sofort. \
         Ab jetzt bis zur Meldung „Fertig“ den Strom nicht trennen.",
    );
    migration_client
        .start_migration(&server.url(IMAGE_PATH), image.len(), image.sha256_hex())
        .await?;
    monitor_flash(&migration_client, &server, image_len, &options.timeouts, reporter).await?;
    reporter.progress(image_len, image_len);

    reporter.info("Warte auf ESPHome …");
    let esphome_address = SocketAddr::new(device_address.ip(), options.esphome_port);
    wait_for_esphome(esphome_address, &options.timeouts).await?;
    Ok(FlashOutcome { esphome_address })
}

#[cfg(test)]
mod tests {
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
}
