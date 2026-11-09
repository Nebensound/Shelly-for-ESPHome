//! Command line interface of `shelly2esphome`.

use std::{
    io::{BufRead, Write},
    net::IpAddr,
    path::PathBuf,
    process::ExitCode,
};

use anyhow::Context;
use clap::{Parser, Subcommand};
use shelly2esphome::{
    device::{Credentials, DeviceClient, Firmware},
    embedded::Packages,
    flash::{self, FlashOptions, Reporter},
    image::FirmwareImage,
    migration::MigrationPackage,
    model::Model,
};

/// Flash a self-built ESPHome firmware onto Shelly Gen1 devices over the air.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show which firmware the device runs and whether it can be migrated.
    Probe {
        /// IP address or host name of the device (optionally with :port).
        host: String,
        /// Login if the web authentication of the stock firmware is enabled.
        #[arg(long, value_name = "USER:PASSWORD")]
        auth: Option<Credentials>,
    },
    /// Check a firmware file without touching any device.
    Check {
        /// ESPHome `firmware.bin` (uncompressed, ESP8266).
        firmware: PathBuf,
    },
    /// Replace the stock firmware (or a waiting migration firmware) with ESPHome.
    Flash {
        /// IP address or host name of the device (optionally with :port).
        host: String,
        /// ESPHome `firmware.bin` built for this device, Wi-Fi settings included.
        #[arg(long, short)]
        firmware: PathBuf,
        /// Migration package `mg2esphome-<Model>.zip`; only needed from stock firmware if this
        /// build does not contain it (CI release binaries contain all models).
        #[arg(long, short)]
        migration: Option<PathBuf>,
        /// Login if the web authentication of the stock firmware is enabled.
        #[arg(long, value_name = "USER:PASSWORD")]
        auth: Option<Credentials>,
        /// Weakest accepted Wi-Fi signal of the device in dBm.
        #[arg(long, default_value_t = -75, allow_hyphen_values = true)]
        min_rssi: i32,
        /// Flash even if the device's Wi-Fi signal is weaker than --min-rssi.
        #[arg(long)]
        allow_weak_signal: bool,
        /// Address of this computer as reachable from the device (detected otherwise).
        #[arg(long)]
        serve_ip: Option<IpAddr>,
        /// Port of the local file server (0 = any free port).
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Do not ask for confirmation.
        #[arg(long)]
        yes: bool,
    },
}

struct Console {
    assume_yes: bool,
    last_percent: Option<u64>,
}

impl Reporter for Console {
    fn info(&mut self, message: &str) {
        if self.last_percent.take().is_some() {
            eprintln!();
        }
        eprintln!("• {message}");
    }

    fn progress(&mut self, sent_bytes: u64, total_bytes: u64) {
        let percent = sent_bytes.saturating_mul(100).checked_div(total_bytes).unwrap_or(0);
        if self.last_percent != Some(percent) {
            self.last_percent = Some(percent);
            eprint!("\r  übertragen: {percent:>3} % ({sent_bytes} von {total_bytes} Bytes)");
            let _ = std::io::stderr().flush();
        }
    }

    fn confirm(&mut self, summary: &str) -> bool {
        eprintln!("\n{summary}\n");
        if self.assume_yes {
            return true;
        }
        eprint!("Fortfahren? Zum Bestätigen „ja“ eingeben: ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        let read = tokio::task::block_in_place(|| std::io::stdin().lock().read_line(&mut answer));
        read.is_ok() && answer.trim().eq_ignore_ascii_case("ja")
    }
}

fn read_image(path: &PathBuf) -> anyhow::Result<FirmwareImage> {
    let bytes = std::fs::read(path).with_context(|| format!("{} nicht lesbar", path.display()))?;
    FirmwareImage::parse(bytes).with_context(|| format!("{} ist nicht verwendbar", path.display()))
}

async fn probe(host: &str, auth: Option<Credentials>) -> anyhow::Result<()> {
    let client = DeviceClient::new(host, auth)?;
    match client.identify().await? {
        Firmware::Stock { info, rssi_dbm } => {
            println!("Originalfirmware: {} ({}), MAC {}", info.device_type, info.fw, info.mac);
            println!("WLAN-Signal: {rssi_dbm} dBm");
            match Model::from_device_type(&info.device_type) {
                Some(model) if Packages::BUILT_IN.find(model).is_some() => {
                    println!("Unterstützt; Migrationspaket ist in diesem Programm enthalten.");
                }
                Some(model) => println!(
                    "Unterstützt; Migrationspaket nicht enthalten: für flash --migration {} angeben.",
                    model.migration_asset_name()
                ),
                None => println!("Dieser Gerätetyp wird nicht unterstützt."),
            }
            if info.auth {
                println!("Anmeldung aktiv: für flash --auth benutzer:passwort angeben.");
            }
        }
        Firmware::Migration(status) => {
            println!(
                "Migrationsfirmware (Protokoll {}): Zustand {:?}, {} von {} Bytes, max. {} Bytes",
                status.protocol, status.state, status.received, status.size, status.max_size
            );
            if !status.error.is_empty() {
                println!("Letzter Fehler: {}", status.error);
            }
        }
    }
    Ok(())
}

#[expect(clippy::too_many_arguments, reason = "mirrors the flash subcommand's options")]
async fn flash_command(
    host: &str,
    firmware: &PathBuf,
    migration: Option<&PathBuf>,
    auth: Option<Credentials>,
    min_rssi: i32,
    allow_weak_signal: bool,
    serve_ip: Option<IpAddr>,
    port: u16,
    yes: bool,
) -> anyhow::Result<()> {
    let image = read_image(firmware)?;
    let package = match migration {
        Some(path) => {
            let bytes =
                std::fs::read(path).with_context(|| format!("{} nicht lesbar", path.display()))?;
            Some(MigrationPackage::parse(bytes)?)
        }
        None => None,
    };
    let client = DeviceClient::new(host, auth)?;
    let options = FlashOptions {
        min_rssi_dbm: min_rssi,
        allow_weak_signal,
        serve_ip,
        serve_port: port,
        ..FlashOptions::default()
    };
    let mut console = Console { assume_yes: yes, last_percent: None };
    let outcome =
        flash::run(host, &client, &image, package.as_ref(), &options, &mut console).await?;
    console.info(&format!(
        "Fertig: ESPHome antwortet auf {}. Das Gerät erscheint jetzt im ESPHome-Dashboard.",
        outcome.esphome_address
    ));
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Probe { host, auth } => probe(&host, auth).await,
        Command::Check { firmware } => read_image(&firmware).map(|image| {
            println!(
                "{}: {} Bytes, SHA-256 {}",
                firmware.display(),
                image.len(),
                image.sha256_hex()
            );
            println!("Verwendbar für shelly2esphome flash.");
        }),
        Command::Flash {
            host,
            firmware,
            migration,
            auth,
            min_rssi,
            allow_weak_signal,
            serve_ip,
            port,
            yes,
        } => {
            flash_command(
                &host,
                &firmware,
                migration.as_ref(),
                auth,
                min_rssi,
                allow_weak_signal,
                serve_ip,
                port,
                yes,
            )
            .await
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("\nFehler: {error:#}");
            ExitCode::FAILURE
        }
    }
}
