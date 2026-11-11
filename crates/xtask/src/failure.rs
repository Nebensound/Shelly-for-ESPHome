//! Failure reports: issues from the form `device-failure.yml`. One that names
//! the same model and failure kind as an older open report is a duplicate:
//! the workflow comments its fields on the older one and closes it, so each
//! failure is discussed in one place.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::intake::Submission;

/// An open issue as `gh issue list --json number,body` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OpenIssue {
    /// The issue number.
    pub number: u64,
    /// The body; `null` for an issue without one.
    pub body: Option<String>,
}

/// A duplicate failure report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Duplicate {
    /// The oldest open report of the same model and failure kind.
    pub original: u64,
    /// The duplicate's fields as a Markdown table, for a comment on the
    /// original. Every value passed the checks of a report, so it carries
    /// no free text.
    pub fields: String,
}

/// A failure report that passes the checks: its code matches its fields and
/// every value is one the tool writes.
fn valid(issue: u64, body: &str) -> Option<Submission> {
    let report = Submission::from_issue(issue, body).ok()?;
    report.failure_problems().is_empty().then_some(report)
}

/// The original of the failure report `issue` with `body` among the `open`
/// issues: the oldest valid report of the same model and failure kind that
/// was opened before it. `None` if `issue` is no valid failure report or has
/// no original.
#[must_use]
pub fn find_original(issue: u64, body: &str, open: &[OpenIssue]) -> Option<Duplicate> {
    let report = valid(issue, body)?;
    let same = |other: &Submission| {
        other.get("model") == report.get("model") && other.get("result") == report.get("result")
    };
    let original = open
        .iter()
        .filter(|candidate| candidate.number < issue)
        .filter(|candidate| {
            valid(candidate.number, candidate.body.as_deref().unwrap_or_default())
                .is_some_and(|other| same(&other))
        })
        .map(|candidate| candidate.number)
        .min()?;
    let mut fields = String::from("| Field | Value |\n|---|---|\n");
    for (id, value) in &report.fields {
        let _ = writeln!(fields, "| {id} | {value} |");
    }
    Some(Duplicate { original, fields })
}

#[cfg(test)]
#[path = "unit_tests/failure.rs"]
pub(crate) mod tests;
