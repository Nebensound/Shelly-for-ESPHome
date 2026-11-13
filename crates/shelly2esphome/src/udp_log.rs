//! The debug log of a Gen2+ stock firmware, which the device sends as UDP
//! datagrams to this computer once `Sys.SetConfig` points `debug.udp.addr`
//! here. `flash` reads the slot the installer will write to and the
//! installer's decisions from it; the previous setting is put back.
//!
//! The lines this module understands were seen on real devices by
//! Free-Shelly OTA:
//!
//! - `Storing core dumps to app_1 (0x200000)`, right after the log is
//!   switched on: the slot the installer will write to. Some firmware names
//!   another partition (`scratch`) and so reveals no slot.
//! - `Will write to slot 1`, during the update.
//! - `Boot: cur 010003ff, new 00000000, min 010009ff, update? 1`: the
//!   installed bootloader, the package's `min_version` and whether the
//!   installer writes the package's bootloader.
//! - `Update succeeded`.

use std::{
    net::{IpAddr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use tokio::{net::UdpSocket, sync::mpsc, task::JoinHandle, time::Instant};

use crate::{
    profile::BootloaderVersion,
    rpc_client::{RpcClient, RpcError},
};

/// Largest datagram read; the device sends one log record per datagram.
const DATAGRAM_BYTES: usize = 4096;

/// What a log line says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogEvent {
    /// The partition the installer would store core dumps in, which is the
    /// slot it will write to: `Some(N)` for `app_N`, `None` for a partition
    /// that is no app slot.
    CoreDumpTarget(Option<u8>),
    /// The slot the installer writes to.
    WillWrite(u8),
    /// The installer's bootloader decision.
    Bootloader {
        /// The bootloader it considers installed.
        installed: BootloaderVersion,
        /// The package's `min_version`.
        minimum: BootloaderVersion,
        /// Whether it writes the package's bootloader.
        update: bool,
    },
    /// The installer finished.
    Succeeded,
}

/// The digit after `prefix` in `line`.
fn digit_after(line: &str, prefix: &str) -> Option<u8> {
    let (_, rest) = line.split_once(prefix)?;
    rest.chars().next().and_then(|digit| digit.to_digit(10)).and_then(|digit| digit.try_into().ok())
}

/// The event of one log line, if it is one this module knows.
#[must_use]
pub fn parse(line: &str) -> Option<LogEvent> {
    if let Some((_, rest)) = line.split_once("Storing core dumps to ") {
        let target = rest.split(|c: char| c.is_whitespace() || c == '(').next().unwrap_or_default();
        let slot = target.strip_prefix("app_").and_then(|digit| digit.parse().ok());
        return Some(LogEvent::CoreDumpTarget(slot));
    }
    if let Some(slot) = digit_after(line, "Will write to slot ") {
        return Some(LogEvent::WillWrite(slot));
    }
    if let Some((_, rest)) = line.split_once("Boot: cur ") {
        let word = |prefix: &str| {
            rest.split_once(prefix)
                .and_then(|(_, after)| after.get(..8))
                .and_then(BootloaderVersion::from_log_word)
        };
        let installed = rest.get(..8).and_then(BootloaderVersion::from_log_word)?;
        let minimum = word(", min ")?;
        let update = digit_after(rest, "update? ")?;
        return Some(LogEvent::Bootloader { installed, minimum, update: update == 1 });
    }
    line.contains("Update succeeded").then_some(LogEvent::Succeeded)
}

/// Reasons why the log cannot be read.
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    /// The UDP port cannot be opened.
    #[error("cannot listen for the device log on UDP {address}: {source} (try --log-port)")]
    Bind {
        /// Local address.
        address: SocketAddr,
        /// Cause.
        source: std::io::Error,
    },
    /// The device refused the log setting.
    #[error("cannot switch on the device log: {0}")]
    Rpc(#[from] RpcError),
}

/// The device log while it is switched on.
#[derive(Debug)]
pub struct UdpLog {
    previous: serde_json::Value,
    events: mpsc::UnboundedReceiver<LogEvent>,
    datagrams: Arc<AtomicU64>,
    reader: JoinHandle<()>,
}

fn log_setting(address: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "config": { "debug": { "udp": { "addr": address } } } })
}

impl UdpLog {
    /// Listens on `local` for datagrams from `device` and points the device's
    /// log there. A destination that is already this one is switched off
    /// first, so the device logs its core dump target again.
    ///
    /// # Errors
    /// A [`LogError`] if the port cannot be opened or the device refuses.
    pub async fn start(
        rpc: &RpcClient,
        device: IpAddr,
        local: SocketAddr,
    ) -> Result<Self, LogError> {
        let socket = UdpSocket::bind(local)
            .await
            .map_err(|source| LogError::Bind { address: local, source })?;
        let config = rpc.call("Sys.GetConfig", serde_json::json!({})).await?;
        let previous = config["debug"]["udp"]["addr"].clone();
        let ours = local.to_string();
        if previous.as_str() == Some(ours.as_str()) {
            rpc.call("Sys.SetConfig", log_setting(serde_json::Value::Null)).await?;
        }
        let (sender, events) = mpsc::unbounded_channel();
        let datagrams = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&datagrams);
        let reader = tokio::spawn(async move {
            let mut buffer = vec![0; DATAGRAM_BYTES];
            // A bound UDP socket that only receives does not fail in
            // practice; a failure reads as a datagram from nowhere, and the
            // task ends when the log is stopped.
            let nowhere = (0, SocketAddr::from(([0, 0, 0, 0], 0)));
            loop {
                let (length, peer) = socket.recv_from(&mut buffer).await.unwrap_or(nowhere);
                if peer.ip() != device {
                    continue;
                }
                counter.fetch_add(1, Ordering::SeqCst);
                let text = String::from_utf8_lossy(buffer.get(..length).unwrap_or_default());
                for event in text.lines().filter_map(parse) {
                    let _ = sender.send(event);
                }
            }
        });
        let log = Self { previous, events, datagrams, reader };
        rpc.call("Sys.SetConfig", log_setting(ours.into())).await?;
        Ok(log)
    }

    /// The next event before `deadline`; `None` once it has passed.
    pub async fn next_event(&mut self, deadline: Instant) -> Option<LogEvent> {
        tokio::time::timeout_at(deadline, self.events.recv()).await.ok().flatten()
    }

    /// Datagrams received from the device so far; none means the log does
    /// not reach this computer (a firewall, another network).
    #[must_use]
    pub fn datagrams(&self) -> u64 {
        self.datagrams.load(Ordering::SeqCst)
    }

    /// Stops listening and puts the previous log setting back.
    ///
    /// # Errors
    /// The [`RpcError`] of the device, e.g. while it restarts after an
    /// update.
    pub async fn restore(mut self, rpc: &RpcClient) -> Result<(), RpcError> {
        self.reader.abort();
        let previous = std::mem::take(&mut self.previous);
        rpc.call("Sys.SetConfig", log_setting(previous)).await.map(drop)
    }
}

impl Drop for UdpLog {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

#[cfg(test)]
#[path = "unit_tests/udp_log.rs"]
mod tests;
