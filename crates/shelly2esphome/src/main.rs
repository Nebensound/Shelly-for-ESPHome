//! Command line interface of `shelly2esphome`.

use std::{
    io::{BufRead, Write},
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::Context;
use clap::{Parser, Subcommand};
use shelly2esphome::{
    backup,
    device::{Credentials, DeviceClient, Firmware},
    discover::{self, Network, ScanOptions},
    embedded::Packages,
    flash::{self, FlashOptions, Reporter, local_ip_towards},
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
    /// List the Shellys in one or more networks and whether they can be flashed.
    #[command(alias = "detect")]
    List {
        /// Networks to scan, e.g. 10.10.5.0/24 (default: the /24 of this computer).
        networks: Vec<Network>,
    },
    /// Save the stock firmware's settings as JSON files (read-only).
    Backup {
        /// IP address or host name of the device (optionally with :port).
        host: String,
        /// Login if the web authentication of the stock firmware is enabled.
        #[arg(long, value_name = "USER:PASSWORD")]
        auth: Option<Credentials>,
        /// Directory in which the backup directory is created.
        #[arg(long, short, default_value = ".")]
        output: PathBuf,
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
        /// Directory for the automatic backup of the stock settings.
        #[arg(long, default_value = ".")]
        backup_dir: PathBuf,
        /// Skip the automatic backup of the stock settings.
        #[arg(long)]
        no_backup: bool,
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
            eprint!("\r  transferred: {percent:>3} % ({sent_bytes} of {total_bytes} bytes)");
            let _ = std::io::stderr().flush();
        }
    }

    fn confirm(&mut self, summary: &str) -> bool {
        eprintln!("\n{summary}\n");
        if self.assume_yes {
            return true;
        }
        eprint!("Continue? Type \"yes\" to confirm: ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        let read = tokio::task::block_in_place(|| std::io::stdin().lock().read_line(&mut answer));
        read.is_ok() && answer.trim().eq_ignore_ascii_case("yes")
    }
}

fn read_image(path: &PathBuf) -> anyhow::Result<FirmwareImage> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    FirmwareImage::parse(bytes).with_context(|| format!("{} is not usable", path.display()))
}

async fn probe(host: &str, auth: Option<Credentials>) -> anyhow::Result<()> {
    let client = DeviceClient::new(host, auth)?;
    match client.identify().await? {
        Firmware::Stock { info, rssi_dbm } => {
            println!("Stock firmware: {} ({}), MAC {}", info.device_type, info.fw, info.mac);
            println!("Wi-Fi signal: {rssi_dbm} dBm");
            match Model::from_device_type(&info.device_type) {
                Some(model) if Packages::BUILT_IN.find(model).is_some() => {
                    println!("Supported; the migration package is built into this program.");
                }
                Some(model) => println!(
                    "Supported; the migration package is not built in: pass flash --migration {}.",
                    model.migration_asset_name()
                ),
                None => println!("This device type is not supported."),
            }
            if info.auth {
                println!("Login enabled: pass --auth user:password.");
            }
        }
        Firmware::Migration(status) => {
            println!(
                "Migration firmware (protocol {}): state {:?}, {} of {} bytes, max. {} bytes",
                status.protocol, status.state, status.received, status.size, status.max_size
            );
            if !status.error.is_empty() {
                println!("Last error: {}", status.error);
            }
        }
    }
    Ok(())
}

async fn list(mut networks: Vec<Network>) -> anyhow::Result<()> {
    if networks.is_empty() {
        let IpAddr::V4(own) = local_ip_towards(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)))? else {
            anyhow::bail!("no IPv4 address on this computer; pass a network like 10.10.5.0/24");
        };
        networks.push(Network::around(own));
    }
    let names: Vec<String> = networks.iter().map(ToString::to_string).collect();
    eprintln!("Scanning {} …", names.join(", "));
    let found = discover::scan(&networks, &ScanOptions::default()).await?;
    if found.is_empty() {
        println!("No Shelly found. Devices in other VLANs need their network as argument.");
        return Ok(());
    }
    let rows: Vec<[String; 4]> = found
        .iter()
        .map(|(ip, device)| [ip.to_string(), device.device(), device.firmware(), device.state()])
        .collect();
    let header = ["ADDRESS", "DEVICE", "FIRMWARE", "STATE"].map(str::to_owned);
    let mut widths = header.each_ref().map(String::len);
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    for row in std::iter::once(&header).chain(&rows) {
        let [address, device, firmware, state] = row;
        println!(
            "{address:<a$}  {device:<d$}  {firmware:<f$}  {state}",
            a = widths[0],
            d = widths[1],
            f = widths[2]
        );
    }
    let flashable = found.iter().filter(|(_, device)| device.can_flash()).count();
    println!("\n{} Shelly(s) found, {flashable} can be flashed.", found.len());
    Ok(())
}

async fn backup_command(
    host: &str,
    auth: Option<Credentials>,
    output: &Path,
) -> anyhow::Result<()> {
    let client = DeviceClient::new(host, auth)?;
    let saved = backup::collect(&client).await?;
    let directory = saved.write_to(output)?;
    println!(
        "Saved {} files of {} ({}) to {}",
        saved.files.len(),
        saved.info.device_type,
        saved.info.mac,
        directory.display()
    );
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
    backup_dir: Option<PathBuf>,
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
        backup_dir,
        ..FlashOptions::default()
    };
    let mut console = Console { assume_yes: yes, last_percent: None };
    let outcome =
        flash::run(host, &client, &image, package.as_ref(), &options, &mut console).await?;
    console.info(&format!(
        "Done: ESPHome answers on {}. The device now shows up in the ESPHome dashboard.",
        outcome.esphome_address
    ));
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Probe { host, auth } => probe(&host, auth).await,
        Command::List { networks } => list(networks).await,
        Command::Backup { host, auth, output } => backup_command(&host, auth, &output).await,
        Command::Check { firmware } => read_image(&firmware).map(|image| {
            println!(
                "{}: {} bytes, SHA-256 {}",
                firmware.display(),
                image.len(),
                image.sha256_hex()
            );
            println!("Usable for shelly2esphome flash.");
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
            backup_dir,
            no_backup,
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
                (!no_backup).then_some(backup_dir),
                yes,
            )
            .await
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("\nError: {error:#}");
            ExitCode::FAILURE
        }
    }
}
