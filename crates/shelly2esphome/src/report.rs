//! Device reports: after `flash`, a link to a GitHub issue form that the
//! tool has filled in with what the run learned. Nothing is sent; the user
//! opens the link and decides.
//!
//! A report carries only what helps to judge a model and the tool: the
//! result, the device type, the stock firmware it came from, the Wi-Fi
//! signal, whether a login and a backup were involved, the package source,
//! the tool version, the computer's OS and architecture and the duration. It
//! never carries addresses, MAC, device ID or name, network names,
//! credentials or free error texts (they can contain addresses).
//!
//! The field IDs and the result values are the contract with the issue
//! forms (`.github/ISSUE_TEMPLATE/device-*.yml`) and the workflow that turns
//! reports into documentation; they do not change.

use std::{fmt::Write as _, time::Duration};

use sha2::{Digest, Sha256};

use crate::model::Model;

/// Where the issue forms live.
pub const REPOSITORY: &str = "https://github.com/Nebensound/Shelly-for-ESPHome";
/// The result of a successful run.
pub const SUCCESS: &str = "success";
/// Mixed into the report code. The code is no signature (anyone can read
/// this); it only shows that the fields were not edited by hand.
const CODE_SALT: &str = "shelly2esphome device report v1";

/// Where the migration package came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageSource {
    /// Built into this program.
    Embedded,
    /// Given with `--migration`.
    File,
}

/// What a `flash` run learned about the device, as far as it got. Nothing
/// in here identifies the device, its network or its owner.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceFacts {
    /// The supported model, once the stock firmware named it.
    pub model: Option<Model>,
    /// Stock firmware build, e.g. `20230913-114008/v1.14.0-gcb84623`.
    pub stock_firmware: Option<String>,
    /// Wi-Fi signal in dBm.
    pub rssi_dbm: Option<i32>,
    /// Whether the stock firmware had web authentication enabled.
    pub login: Option<bool>,
    /// Whether the stock settings were backed up.
    pub backup: Option<bool>,
    /// Where the migration package came from.
    pub package: Option<PackageSource>,
}

/// A device report of one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// [`SUCCESS`] or the kind of failure (`FlashError::report_kind`).
    pub result: String,
    /// What the run learned.
    pub facts: DeviceFacts,
    /// How long the run took.
    pub duration: Duration,
}

/// One to four decimal digits.
fn is_number(text: &str) -> bool {
    (1..=4).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// Lowercase hex digits, as many as `lengths` allows.
fn is_hex(text: &str, lengths: std::ops::RangeInclusive<usize>) -> bool {
    lengths.contains(&text.len())
        && text.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The version of a stock build: `v<major>.<minor>.<patch>`, optionally
/// `-rc<n>` or `-beta<n>`, then optionally the commit as `-g<7–12 hex>` or
/// `@<8 hex>`, e.g. `v1.14.0-gcb84623` or `v1.9.4@e2732e05`.
fn is_stock_version(version: &str) -> bool {
    let (release, commit) = if let Some((release, hash)) = version.split_once('@') {
        (release, is_hex(hash, 8..=8))
    } else if let Some((release, _)) =
        version.rsplit_once("-g").filter(|(_, hash)| is_hex(hash, 7..=12))
    {
        (release, true)
    } else {
        (version, true)
    };
    let mut parts = release.strip_prefix('v').unwrap_or_default().splitn(2, '-');
    let numbers = parts.next().unwrap_or_default();
    let pre_release = parts.next();
    commit
        && numbers.split('.').count() == 3
        && numbers.split('.').all(is_number)
        && pre_release.is_none_or(|pre| {
            pre.strip_prefix("rc").or_else(|| pre.strip_prefix("beta")).is_some_and(is_number)
        })
}

/// Whether `text` is a Gen1 stock firmware build, `YYYYMMDD-HHMMSS/<version>`
/// (see [`is_stock_version`]). The device reports it, so anything else could
/// be free text (a name, an address) and is not put into a report. The
/// report workflow applies the same rule to submitted reports.
#[must_use]
pub fn is_stock_build(text: &str) -> bool {
    text.split_once('/').is_some_and(|(date, version)| {
        date.len() == 15
            && date
                .bytes()
                .enumerate()
                .all(|(index, byte)| if index == 8 { byte == b'-' } else { byte.is_ascii_digit() })
            && is_stock_version(version)
    })
}

fn yes_no(value: Option<bool>) -> String {
    value.map_or("unknown", |value| if value { "yes" } else { "no" }).to_owned()
}

/// Percent-encodes everything but the unreserved characters (RFC 3986).
fn encode(text: &str) -> String {
    text.bytes().fold(String::new(), |mut out, byte| {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
        out
    })
}

impl Report {
    /// The prefilled fields in form order: ID and value.
    #[must_use]
    pub fn fields(&self) -> Vec<(&'static str, String)> {
        let facts = &self.facts;
        let package = facts.package.map_or("unknown", |package| match package {
            PackageSource::Embedded => "embedded",
            PackageSource::File => "file",
        });
        vec![
            ("result", self.result.clone()),
            ("model", facts.model.map_or("unknown", Model::device_type).to_owned()),
            ("generation", "Gen1".to_owned()),
            (
                "stock-firmware",
                facts
                    .stock_firmware
                    .as_deref()
                    .filter(|build| is_stock_build(build))
                    .unwrap_or("unknown")
                    .to_owned(),
            ),
            ("signal", facts.rssi_dbm.map_or("unknown".into(), |rssi| format!("{rssi} dBm"))),
            ("login", yes_no(facts.login)),
            ("backup", yes_no(facts.backup)),
            ("package", package.to_owned()),
            ("tool", env!("CARGO_PKG_VERSION").to_owned()),
            ("platform", format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)),
            ("duration", format!("{} s", self.duration.as_secs())),
        ]
    }

    /// The report code over `fields`: the first 16 hex digits of the SHA-256
    /// of the salt and one `id=value` line per field.
    #[must_use]
    pub fn code(fields: &[(&str, String)]) -> String {
        let text = fields.iter().fold(format!("{CODE_SALT}\n"), |mut text, (id, value)| {
            let _ = writeln!(text, "{id}={value}");
            text
        });
        hex::encode(Sha256::digest(text.as_bytes()))[..16].to_owned()
    }

    /// The link to the prefilled issue form: `device-report.yml` for a
    /// success, `device-failure.yml` (a bug) otherwise.
    #[must_use]
    pub fn url(&self) -> String {
        let fields = self.fields();
        let model = self.facts.model.map_or("unknown device", Model::device_type);
        let (template, title) = if self.result == SUCCESS {
            ("device-report.yml", format!("Device report: {model} works"))
        } else {
            ("device-failure.yml", format!("Flash failed: {model} ({})", self.result))
        };
        let code = Self::code(&fields);
        let query = [("template", template.to_owned()), ("title", title)]
            .into_iter()
            .chain(fields)
            .chain([("report-code", code)])
            .map(|(id, value)| format!("{id}={}", encode(&value)))
            .collect::<Vec<_>>()
            .join("&");
        format!("{REPOSITORY}/issues/new?{query}")
    }
}

#[cfg(test)]
#[path = "unit_tests/report.rs"]
mod tests;
