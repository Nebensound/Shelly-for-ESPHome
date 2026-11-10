//! Command line interface: argument parsing and console output.
//!
//! Everything the binary does lives here, so tests can drive it with their
//! own arguments, writers and device settings. `main.rs` only connects it to
//! the process's arguments, stdout, stderr and stdin.

use std::{
    ffi::OsString,
    io::{BufRead, Write},
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::Context;
use clap::{Parser, Subcommand};

use crate::{
    backup,
    device::{Credentials, DeviceClient, Firmware},
    discover::{self, Network, ScanOptions},
    flash::{self, FlashOptions, Reporter, local_ip_towards},
    image::FirmwareImage,
    migration::MigrationPackage,
    model::Model,
};

/// Flash a self-built ESPHome firmware onto Shelly Gen1 devices over the air.
#[derive(Debug, Parser)]
#[command(name = "shelly2esphome", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List the Shellys in one or more networks and whether they can be flashed.
    #[command(alias = "detect")]
    List {
        /// Networks to scan, e.g. 10.10.5.0/24 (default: the /24 of this computer).
        networks: Vec<Network>,
    },
    /// Show which firmware the device runs and whether it can be migrated.
    Probe {
        /// IP address or host name of the device (optionally with :port).
        host: String,
        /// Login if the web authentication of the stock firmware is enabled.
        #[arg(long, value_name = "USER:PASSWORD")]
        auth: Option<Credentials>,
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
        /// Migration package `mg2esphome-<Model>.zip`; only needed if this build
        /// does not contain the package of the model (see `probe`).
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

/// Defaults for what the command line does not set; tests replace them.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Base options of `flash`; the command line overrides some of them.
    pub flash: FlashOptions,
    /// Options of `list`.
    pub scan: ScanOptions,
    /// Finds this computer's address towards a target (default network of `list`).
    pub local_ip: fn(IpAddr) -> std::io::Result<IpAddr>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            flash: FlashOptions::default(),
            scan: ScanOptions::default(),
            local_ip: local_ip_towards,
        }
    }
}

/// Where the commands write to and read the confirmation from.
pub struct Terminal<'a> {
    /// Results (stdout).
    pub out: &'a mut dyn Write,
    /// Progress, prompts and errors (stderr).
    pub err: &'a mut dyn Write,
    /// Answers to prompts (stdin).
    pub input: &'a mut dyn BufRead,
}

struct Console<'t, 'a> {
    terminal: &'t mut Terminal<'a>,
    assume_yes: bool,
    /// Last percentage shown; kept across messages so it is not repeated.
    last_percent: Option<u64>,
    /// A progress line is open (written without a line break).
    progress_open: bool,
}

impl Reporter for Console<'_, '_> {
    fn info(&mut self, message: &str) {
        if std::mem::take(&mut self.progress_open) {
            let _ = writeln!(self.terminal.err);
        }
        let _ = writeln!(self.terminal.err, "• {message}");
    }

    fn progress(&mut self, sent_bytes: u64, total_bytes: u64) {
        let percent = sent_bytes.saturating_mul(100).checked_div(total_bytes).unwrap_or(0);
        if self.last_percent != Some(percent) {
            self.last_percent = Some(percent);
            self.progress_open = true;
            let _ = write!(
                self.terminal.err,
                "\r  transferred: {percent:>3} % ({sent_bytes} of {total_bytes} bytes)"
            );
            let _ = self.terminal.err.flush();
        }
    }

    fn confirm(&mut self, summary: &str) -> bool {
        let _ = writeln!(self.terminal.err, "\n{summary}\n");
        if self.assume_yes {
            return true;
        }
        let _ = write!(self.terminal.err, "Continue? Type \"yes\" to confirm: ");
        let _ = self.terminal.err.flush();
        let mut answer = String::new();
        let input = &mut *self.terminal.input;
        let read = tokio::task::block_in_place(|| input.read_line(&mut answer));
        read.is_ok() && answer.trim().eq_ignore_ascii_case("yes")
    }
}

fn read_image(path: &Path) -> anyhow::Result<FirmwareImage> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    FirmwareImage::parse(bytes).with_context(|| format!("{} is not usable", path.display()))
}

/// The /24 of this computer's IPv4 address, the default network of `list`.
fn default_network(local_ip: fn(IpAddr) -> std::io::Result<IpAddr>) -> anyhow::Result<Network> {
    match local_ip(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))) {
        Ok(IpAddr::V4(own)) => Ok(Network::around(own)),
        _ => anyhow::bail!(
            "could not determine this computer's IPv4 network; pass one like 10.10.5.0/24"
        ),
    }
}

fn table(rows: &[[String; 4]]) -> Vec<String> {
    let header = ["ADDRESS", "DEVICE", "FIRMWARE", "STATE"].map(str::to_owned);
    let mut widths = header.each_ref().map(String::len);
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    std::iter::once(&header)
        .chain(rows)
        .map(|[address, device, firmware, state]| {
            format!(
                "{address:<a$}  {device:<d$}  {firmware:<f$}  {state}",
                a = widths[0],
                d = widths[1],
                f = widths[2]
            )
        })
        .collect()
}

async fn list(
    mut networks: Vec<Network>,
    settings: &Settings,
    terminal: &mut Terminal<'_>,
) -> anyhow::Result<()> {
    if networks.is_empty() {
        networks.push(default_network(settings.local_ip)?);
    }
    let names: Vec<String> = networks.iter().map(ToString::to_string).collect();
    let _ = writeln!(terminal.err, "Scanning {} …", names.join(", "));
    let found = discover::scan(&networks, &settings.scan).await;
    if found.is_empty() {
        let _ = writeln!(
            terminal.out,
            "No Shelly found. Devices in other VLANs need their network as argument."
        );
        return Ok(());
    }
    let rows: Vec<[String; 4]> = found
        .iter()
        .map(|(ip, device)| [ip.to_string(), device.device(), device.firmware(), device.state()])
        .collect();
    for line in table(&rows) {
        let _ = writeln!(terminal.out, "{line}");
    }
    let flashable = found.iter().filter(|(_, device)| device.can_flash()).count();
    let _ =
        writeln!(terminal.out, "\n{} Shelly(s) found, {flashable} can be flashed.", found.len());
    Ok(())
}

async fn probe(
    host: &str,
    auth: Option<Credentials>,
    settings: &Settings,
    terminal: &mut Terminal<'_>,
) -> anyhow::Result<()> {
    let client = DeviceClient::new(host, auth);
    let out = &mut *terminal.out;
    match client.identify().await? {
        Firmware::Stock { info, rssi_dbm } => {
            let _ = writeln!(
                out,
                "Stock firmware: {} ({}), MAC {}",
                info.device_type, info.fw, info.mac
            );
            let _ = writeln!(out, "Wi-Fi signal: {rssi_dbm} dBm");
            let _ = match Model::from_device_type(&info.device_type) {
                Some(model) if settings.flash.embedded_packages.find(model).is_some() => {
                    writeln!(out, "Supported; the migration package is built into this program.")
                }
                Some(model) => writeln!(
                    out,
                    "Supported; the migration package is not built in: pass flash --migration {}.",
                    model.migration_asset_name()
                ),
                None => writeln!(out, "This device type is not supported."),
            };
            if info.auth {
                let _ = writeln!(out, "Login enabled: pass --auth user:password.");
            }
        }
        Firmware::Migration(status) => {
            let _ = writeln!(
                out,
                "Migration firmware (protocol {}): state {:?}, {} of {} bytes, max. {} bytes",
                status.protocol, status.state, status.received, status.size, status.max_size
            );
            if !status.error.is_empty() {
                let _ = writeln!(out, "Last error: {}", status.error);
            }
        }
    }
    Ok(())
}

async fn backup_command(
    host: &str,
    auth: Option<Credentials>,
    output: &Path,
    terminal: &mut Terminal<'_>,
) -> anyhow::Result<()> {
    let client = DeviceClient::new(host, auth);
    let saved = backup::collect(&client).await?;
    let directory = saved.write_to(output)?;
    let _ = writeln!(
        terminal.out,
        "Saved {} files of {} ({}) to {}",
        saved.files.len(),
        saved.info.device_type,
        saved.info.mac,
        directory.display()
    );
    Ok(())
}

fn check(firmware: &Path, terminal: &mut Terminal<'_>) -> anyhow::Result<()> {
    let image = read_image(firmware)?;
    let _ = writeln!(
        terminal.out,
        "{}: {} bytes, SHA-256 {}\nUsable for shelly2esphome flash.",
        firmware.display(),
        image.len(),
        image.sha256_hex()
    );
    Ok(())
}

async fn execute(
    command: Command,
    settings: &Settings,
    terminal: &mut Terminal<'_>,
) -> anyhow::Result<()> {
    match command {
        Command::List { networks } => list(networks, settings, terminal).await,
        Command::Probe { host, auth } => probe(&host, auth, settings, terminal).await,
        Command::Backup { host, auth, output } => {
            backup_command(&host, auth, &output, terminal).await
        }
        Command::Check { firmware } => check(&firmware, terminal),
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
            let image = read_image(&firmware)?;
            let package = match migration {
                Some(path) => {
                    let bytes = std::fs::read(&path)
                        .with_context(|| format!("cannot read {}", path.display()))?;
                    Some(MigrationPackage::parse(bytes)?)
                }
                None => None,
            };
            let client = DeviceClient::new(&host, auth);
            let options = FlashOptions {
                min_rssi_dbm: min_rssi,
                allow_weak_signal,
                serve_ip: serve_ip.or(settings.flash.serve_ip),
                serve_port: port,
                backup_dir: (!no_backup).then_some(backup_dir),
                ..settings.flash.clone()
            };
            let mut console =
                Console { terminal, assume_yes: yes, last_percent: None, progress_open: false };
            let outcome =
                flash::run(&host, &client, &image, package.as_ref(), &options, &mut console)
                    .await?;
            console.info(&format!(
                "Done: ESPHome answers on {}. The device now shows up in the ESPHome dashboard.",
                outcome.esphome_address
            ));
            Ok(())
        }
    }
}

/// Parses `args` (including the program name), runs the command and returns
/// the process exit code. Errors are written to `terminal.err`.
pub async fn run<I, T>(args: I, settings: &Settings, terminal: &mut Terminal<'_>) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            // --help and --version are "errors" that go to stdout with code 0.
            let target = if error.use_stderr() { &mut *terminal.err } else { &mut *terminal.out };
            let _ = write!(target, "{}", error.render());
            return ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(2));
        }
    };
    match execute(cli.command, settings, terminal).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(terminal.err, "\nError: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[path = "unit_tests/cli.rs"]
mod tests;
