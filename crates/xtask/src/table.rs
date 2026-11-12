//! The device table in the README: one row per model with a successful
//! report and the stock firmware versions it was flashed from. It is built
//! from the report files and sits between two markers, so nothing else in
//! the README changes with it.

use std::{collections::BTreeMap, fmt::Write as _, path::Path};

use anyhow::Context;
use shelly2esphome::model::Model;

use crate::intake::{self, Submission};

/// The line before the table.
pub const START: &str = "<!-- device-table:start -->";
/// The line after the table.
pub const END: &str = "<!-- device-table:end -->";

/// The version of a stock build without its date and commit:
/// `20230913-114008/v1.14.0-gcb84623` becomes `v1.14.0`. `None` for
/// `unknown`. The intake allows only the tool's build format, so the commit
/// is the only `-g` or `@` part.
fn stock_version(build: &str) -> Option<&str> {
    let (_, version) = build.split_once('/')?;
    let version = version.split('@').next().unwrap_or(version);
    Some(version.rsplit_once("-g").map_or(version, |(release, _)| release))
}

/// Sorts versions oldest first: by their numbers, a pre-release before its
/// release.
fn version_key(version: &str) -> (Vec<u32>, bool, String) {
    let version = version.trim_start_matches('v');
    let (release, pre) = version.split_once('-').unwrap_or((version, ""));
    let numbers = release.split('.').map(|part| part.parse().unwrap_or_default()).collect();
    (numbers, pre.is_empty(), pre.to_owned())
}

/// The table of `reports`: one row per model, its stock firmware versions
/// newest first. A Gen1 model shows its build name and device type, a later
/// one its app name and generation.
#[must_use]
pub fn render(reports: &[Submission]) -> String {
    let mut models: BTreeMap<&str, (&str, Vec<&str>)> = BTreeMap::new();
    for report in reports {
        let (_, versions) =
            models.entry(report.get("model")).or_insert((report.get("generation"), Vec::new()));
        versions.extend(stock_version(report.get("stock-firmware")));
    }
    if models.is_empty() {
        return "No device reports yet.\n".to_owned();
    }
    let mut table = String::from("| Model | Stock firmware |\n|---|---|\n");
    for (model, (generation, mut versions)) in models {
        versions.sort_by_key(|version| version_key(version));
        versions.dedup();
        versions.reverse();
        let name = match Model::from_device_type(model) {
            Some(gen1) => format!("{} (`{model}`)", gen1.build_name()),
            None => format!("{model} ({generation})"),
        };
        let firmware = if versions.is_empty() { "unknown".to_owned() } else { versions.join(", ") };
        let _ = writeln!(table, "| {name} | {firmware} |");
    }
    table
}

/// Writes the table of the report files below `root` into its `README.md`
/// between [`START`] and [`END`]. Returns whether the README changed.
///
/// # Errors
///
/// An invalid report file, a README without the markers, and read or write
/// errors.
pub fn update_readme(root: &Path) -> anyhow::Result<bool> {
    let reports = intake::load_reports(root)?;
    let path = root.join("README.md");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let (before, rest) = text.split_once(START).context("README.md has no start marker")?;
    let (_, after) = rest.split_once(END).context("README.md has no end marker")?;
    let updated = format!("{before}{START}\n{}{END}{after}", render(&reports));
    if updated == text {
        return Ok(false);
    }
    std::fs::write(&path, updated).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
#[path = "unit_tests/table.rs"]
mod tests;
