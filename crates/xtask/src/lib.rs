//! Repository tasks for CI, run as `cargo xtask <task>`. Not part of the
//! published CLI; it shares the CLI's report rules (`shelly2esphome::report`)
//! so the tool and the workflows cannot drift apart.

pub mod intake;

use std::{ffi::OsString, io::Write, path::PathBuf};

use anyhow::Context;
use clap::{Parser, Subcommand};
use serde_json::Value;

use crate::intake::Author;

/// Repository tasks for CI.
#[derive(Debug, Parser)]
#[command(name = "cargo xtask")]
struct Cli {
    #[command(subcommand)]
    task: Task,
}

#[derive(Debug, Subcommand)]
enum Task {
    /// Check a device report issue, write its report file unless it is
    /// refused and print the decision as JSON.
    ReportIntake {
        /// The `issues` event that GitHub Actions passes in
        /// `GITHUB_EVENT_PATH`.
        #[arg(long)]
        event: PathBuf,
        /// Days since the author's account was created.
        #[arg(long)]
        account_age_days: u32,
        /// Device reports the author opened within the last day, this one
        /// included.
        #[arg(long)]
        recent_reports: u32,
        /// A released version of the tool, e.g. `0.8.0`; repeated.
        #[arg(long = "released")]
        released: Vec<String>,
        /// The repository root.
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Check every report file in `docs/device-reports/`.
    CheckReports {
        /// The repository root.
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
}

/// Runs the task that `args` (with the program name first) names and writes
/// its result to `out`.
///
/// # Errors
///
/// Invalid arguments (including `--help`, which clap reports as an error
/// with the help text), an unreadable event and the errors of the task.
pub fn run(args: &[OsString], out: &mut dyn Write) -> anyhow::Result<()> {
    let output = match Cli::try_parse_from(args)?.task {
        Task::ReportIntake { event, account_age_days, recent_reports, released, root } => {
            let text = std::fs::read_to_string(&event)
                .with_context(|| format!("reading {}", event.display()))?;
            let event: Value = serde_json::from_str(&text).context("the event is no JSON")?;
            let issue = event.pointer("/issue/number").and_then(Value::as_u64);
            let issue = issue.context("the event names no issue")?;
            // A form whose fields were all left empty has no body.
            let body = event.pointer("/issue/body").and_then(Value::as_str).unwrap_or_default();
            let author = Author { account_age_days, recent_reports, released };
            let decision = intake::intake(issue, body, &author, &root)?;
            // Strings, a path and a unit enum always serialize.
            serde_json::to_string(&decision).unwrap_or_default()
        }
        Task::CheckReports { root } => {
            format!("{} device reports are valid", intake::check_reports(&root)?)
        }
    };
    out.write_all(format!("{output}\n").as_bytes())?;
    Ok(())
}

#[cfg(test)]
#[path = "unit_tests/lib.rs"]
mod tests;
