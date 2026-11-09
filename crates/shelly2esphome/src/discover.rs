//! Finds Shelly devices in IPv4 networks and tells which ones can be migrated.
//!
//! Every address of the given networks is asked for `/shelly` (and, if that
//! does not exist, for `MG2X.Status`). Only reading requests are sent.

use std::{
    net::{IpAddr, Ipv4Addr},
    str::FromStr,
    time::Duration,
};

use futures_util::{StreamExt, stream};
use serde::Deserialize;

use crate::{
    device::{MigrationState, MigrationStatus},
    model::Model,
};

/// Largest network that is scanned (a /16).
const MIN_PREFIX: u8 = 16;

/// An IPv4 network in CIDR notation, e.g. `10.10.5.0/24`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Network {
    address: Ipv4Addr,
    prefix: u8,
}

/// Reasons why a network cannot be scanned.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NetworkError {
    /// The text is not `a.b.c.d/n` or `a.b.c.d`.
    #[error("{0:?} is not an IPv4 network like 10.10.5.0/24")]
    Invalid(String),
    /// The network has more addresses than a /16.
    #[error("{0:?} is too large; scan at most a /{MIN_PREFIX} at once")]
    TooLarge(String),
}

impl FromStr for Network {
    type Err = NetworkError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || NetworkError::Invalid(text.to_owned());
        let (address, prefix) = text.split_once('/').unwrap_or((text, "32"));
        let address: Ipv4Addr = address.parse().map_err(|_| invalid())?;
        let prefix: u8 = prefix.parse().map_err(|_| invalid())?;
        if prefix > 32 {
            return Err(invalid());
        }
        if prefix < MIN_PREFIX {
            return Err(NetworkError::TooLarge(text.to_owned()));
        }
        Ok(Self { address, prefix })
    }
}

impl std::fmt::Display for Network {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", Ipv4Addr::from(self.first()), self.prefix)
    }
}

impl Network {
    /// The /24 network around `address`.
    #[must_use]
    pub fn around(address: Ipv4Addr) -> Self {
        Self { address, prefix: 24 }
    }

    fn mask(self) -> u32 {
        u32::MAX.checked_shl(32 - u32::from(self.prefix)).unwrap_or(0)
    }

    fn first(self) -> u32 {
        u32::from(self.address) & self.mask()
    }

    /// Host addresses of the network (without network and broadcast address
    /// for prefixes up to /30).
    pub fn hosts(self) -> impl Iterator<Item = Ipv4Addr> {
        let first = self.first();
        let last = first | !self.mask();
        let (start, end) = if self.prefix >= 31 { (first, last) } else { (first + 1, last - 1) };
        (start..=end).map(Ipv4Addr::from)
    }
}

/// What answers on an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// Gen1 stock firmware.
    Stock {
        /// Device type, e.g. `SHSW-PM`.
        device_type: String,
        /// Firmware build identifier.
        firmware: String,
        /// MAC address.
        mac: String,
        /// Whether web authentication is enabled.
        auth: bool,
        /// Supported model, `None` if this tool cannot migrate it.
        model: Option<Model>,
    },
    /// The migration firmware of this project.
    Migration(MigrationStatus),
    /// A Shelly of generation 2 or later.
    Newer {
        /// Generation.
        generation: u32,
        /// Model code, e.g. `SNSW-001P16EU`.
        model: String,
        /// Firmware version.
        firmware: String,
    },
}

impl Found {
    /// Whether `flash` can work on this device.
    #[must_use]
    pub fn can_flash(&self) -> bool {
        match self {
            Self::Stock { model, .. } => model.is_some(),
            Self::Migration(status) => {
                !matches!(status.state, MigrationState::Downloading | MigrationState::Installing)
            }
            Self::Newer { .. } => false,
        }
    }

    /// Device column of the listing.
    #[must_use]
    pub fn device(&self) -> String {
        match self {
            Self::Stock { device_type, model: Some(model), .. } => {
                format!("{} ({device_type})", model.build_name())
            }
            Self::Stock { device_type, model: None, .. } => device_type.clone(),
            Self::Migration(_) => "migration firmware".to_owned(),
            Self::Newer { generation, model, .. } => format!("{model} (Gen{generation})"),
        }
    }

    /// Firmware column of the listing.
    #[must_use]
    pub fn firmware(&self) -> String {
        match self {
            Self::Stock { firmware, .. } | Self::Newer { firmware, .. } => firmware.clone(),
            Self::Migration(status) => format!("protocol {}", status.protocol),
        }
    }

    /// State column of the listing.
    #[must_use]
    pub fn state(&self) -> String {
        match self {
            Self::Stock { model: Some(_), auth: true, .. } => {
                "can be flashed (login required: --auth)".to_owned()
            }
            Self::Stock { model: Some(_), auth: false, .. } => "can be flashed".to_owned(),
            Self::Stock { model: None, .. } => "Gen1 model not supported".to_owned(),
            Self::Migration(status) if self.can_flash() => {
                format!("waiting ({:?}): run flash to continue", status.state)
            }
            Self::Migration(status) => format!("busy ({:?})", status.state),
            Self::Newer { .. } => "not supported (only Gen1)".to_owned(),
        }
    }
}

/// Settings of a scan.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// HTTP port that is asked.
    pub port: u16,
    /// Time limit per request.
    pub timeout: Duration,
    /// Number of addresses asked at the same time.
    pub concurrency: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self { port: 80, timeout: Duration::from_millis(1500), concurrency: 64 }
    }
}

#[derive(Debug, Deserialize)]
struct ShellyWire {
    #[serde(rename = "type")]
    device_type: Option<String>,
    mac: Option<String>,
    auth: Option<bool>,
    fw: Option<String>,
    // Gen2+.
    #[serde(rename = "gen")]
    generation: Option<u32>,
    model: Option<String>,
    ver: Option<String>,
}

fn classify(wire: ShellyWire) -> Option<Found> {
    match wire {
        ShellyWire { generation: Some(generation), model: Some(model), ver, .. }
            if generation >= 2 =>
        {
            Some(Found::Newer { generation, model, firmware: ver.unwrap_or_default() })
        }
        ShellyWire {
            device_type: Some(device_type),
            mac: Some(mac),
            fw: Some(firmware),
            auth,
            ..
        } => {
            let model = Model::from_device_type(&device_type);
            Some(Found::Stock { device_type, firmware, mac, auth: auth.unwrap_or(false), model })
        }
        _ => None,
    }
}

async fn probe(http: &reqwest::Client, ip: Ipv4Addr, port: u16) -> Option<Found> {
    let base = format!("http://{ip}:{port}");
    let response = http.get(format!("{base}/shelly")).send().await.ok()?;
    if response.status().is_success() {
        return classify(response.json().await.ok()?);
    }
    // The migration firmware serves no /shelly.
    let response = http.get(format!("{base}/rpc/MG2X.Status")).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json::<MigrationStatus>().await.ok().map(Found::Migration)
}

/// Asks every host address of `networks` and returns the Shellys found,
/// sorted by address.
///
/// # Errors
/// Fails if the HTTP client cannot be constructed.
pub async fn scan(
    networks: &[Network],
    options: &ScanOptions,
) -> Result<Vec<(IpAddr, Found)>, reqwest::Error> {
    let http = reqwest::Client::builder().timeout(options.timeout).build()?;
    let mut addresses: Vec<Ipv4Addr> =
        networks.iter().flat_map(|network| network.hosts()).collect();
    addresses.sort();
    addresses.dedup();
    let mut found: Vec<(IpAddr, Found)> = stream::iter(addresses)
        .map(|ip| {
            let http = &http;
            async move { probe(http, ip, options.port).await.map(|found| (IpAddr::V4(ip), found)) }
        })
        .buffer_unordered(options.concurrency.max(1))
        .filter_map(|result| async move { result })
        .collect()
        .await;
    found.sort_by_key(|(ip, _)| *ip);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_networks() {
        let network: Network = "10.10.5.77/24".parse().expect("valid");
        assert_eq!(network.to_string(), "10.10.5.0/24");
        assert_eq!(network.hosts().count(), 254);
        assert_eq!(network.hosts().next(), Some(Ipv4Addr::new(10, 10, 5, 1)));
        assert_eq!(network.hosts().last(), Some(Ipv4Addr::new(10, 10, 5, 254)));
        let single: Network = "10.10.5.79".parse().expect("single address");
        assert_eq!(single.hosts().collect::<Vec<_>>(), vec![Ipv4Addr::new(10, 10, 5, 79)]);
        assert_eq!("10.0.0.0/31".parse::<Network>().expect("/31").hosts().count(), 2);
        assert_eq!("10.0.0.0/16".parse::<Network>().expect("/16").hosts().count(), 65_534);
        assert_eq!(Network::around(Ipv4Addr::new(192, 168, 1, 20)).to_string(), "192.168.1.0/24");
        assert!(matches!("10.0.0.0/8".parse::<Network>(), Err(NetworkError::TooLarge(_))));
        for invalid in ["", "10.0.0/24", "10.0.0.0/33", "10.0.0.0/x", "::1/128"] {
            let error = invalid.parse::<Network>().expect_err(invalid);
            assert!(error.to_string().contains("is not an IPv4 network"), "{invalid}");
        }
        assert!(NetworkError::TooLarge("x".into()).to_string().contains("/16"));
    }

    fn stock(device_type: &str, auth: bool) -> Found {
        classify(ShellyWire {
            device_type: Some(device_type.into()),
            mac: Some("AABBCCDDEEFF".into()),
            auth: Some(auth),
            fw: Some("1.14.0".into()),
            generation: None,
            model: None,
            ver: None,
        })
        .expect("stock")
    }

    fn migration(state: MigrationState) -> Found {
        Found::Migration(MigrationStatus {
            protocol: 2,
            state,
            received: 0,
            size: 0,
            max_size: 1,
            error: String::new(),
        })
    }

    #[test]
    fn classifies_and_describes_devices() {
        let supported = stock("SHSW-PM", false);
        assert!(supported.can_flash());
        assert_eq!(supported.device(), "Shelly1PM (SHSW-PM)");
        assert_eq!(supported.firmware(), "1.14.0");
        assert_eq!(supported.state(), "can be flashed");
        assert!(stock("SHSW-PM", true).state().contains("--auth"));

        let unsupported = stock("SHHT-1", false);
        assert!(!unsupported.can_flash());
        assert_eq!(unsupported.device(), "SHHT-1");
        assert!(unsupported.state().contains("not supported"));

        let newer = classify(ShellyWire {
            device_type: None,
            mac: Some("AABBCCDDEEFF".into()),
            auth: None,
            fw: None,
            generation: Some(2),
            model: Some("SNSW-001P16EU".into()),
            ver: Some("1.4.4".into()),
        })
        .expect("gen2");
        assert!(!newer.can_flash());
        assert_eq!(newer.device(), "SNSW-001P16EU (Gen2)");
        assert_eq!(newer.firmware(), "1.4.4");
        assert!(newer.state().contains("only Gen1"));

        let waiting = migration(MigrationState::Failed);
        assert!(waiting.can_flash());
        assert_eq!(waiting.device(), "migration firmware");
        assert_eq!(waiting.firmware(), "protocol 2");
        assert!(waiting.state().contains("run flash"));
        let busy = migration(MigrationState::Installing);
        assert!(!busy.can_flash());
        assert!(busy.state().starts_with("busy"));

        let nothing = ShellyWire {
            device_type: None,
            mac: None,
            auth: None,
            fw: None,
            generation: None,
            model: None,
            ver: None,
        };
        assert_eq!(classify(nothing), None);
    }
}
