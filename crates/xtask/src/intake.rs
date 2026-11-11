//! Device report intake: an issue from the form `device-report.yml` becomes
//! a report file in `docs/device-reports/` that the README's device table is
//! built from.
//!
//! The tool fills in every required field and codes them (`Report::code`), so
//! a report is checked against the same rules the tool applies when it fills
//! the form. A report that breaks one of them is refused. One that passes but
//! is not clearly genuine (an unreleased tool, a new account, many reports at
//! once, an own migration package, odd values) waits for a review; the rest
//! is merged without one.

use std::{
    ops::RangeInclusive,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::failure::{self, Duplicate, OpenIssue};
use anyhow::{Context, bail};
use serde::Serialize;
use serde_json::Value;
use shelly2esphome::{
    model::Model,
    report::{self, DeviceFacts, Report, SUCCESS},
};

/// The issue form of successful runs; its labels name the fields in an
/// issue body.
const FORM: &str = include_str!("../../../.github/ISSUE_TEMPLATE/device-report.yml");
/// Where report files live, relative to the repository root.
pub const REPORT_DIR: &str = "docs/device-reports";
/// The ID of the report code field.
const CODE_ID: &str = "report-code";
/// What GitHub writes for an optional field left empty.
const NO_RESPONSE: &str = "_No response_";

/// A younger account needs a review.
const MIN_ACCOUNT_AGE_DAYS: u32 = 30;
/// More reports by one author within a day need a review.
const MAX_RECENT_REPORTS: u32 = 3;
/// A signal outside this range is odd for a run that succeeded.
const PLAUSIBLE_SIGNAL_DBM: RangeInclusive<i32> = -100..=-10;
/// A run is never faster than the two uploads and reboots, and one that took
/// longer than half an hour is odd.
const PLAUSIBLE_DURATION_S: RangeInclusive<u32> = 20..=1800;

/// The IDs of the coded fields, in the order the tool codes them.
fn field_ids() -> Vec<&'static str> {
    let report =
        Report { result: SUCCESS.into(), facts: DeviceFacts::default(), duration: Duration::ZERO };
    report.fields().into_iter().map(|(id, _)| id).collect()
}

/// Label and ID of every field of the form.
fn form_fields() -> Vec<(&'static str, &'static str)> {
    let mut fields = Vec::new();
    let mut id = None;
    for line in FORM.lines() {
        if let Some(found) = line.strip_prefix("    id: ") {
            id = Some(found);
        } else if let Some(label) = line.strip_prefix("      label: ") {
            fields.extend(id.take().map(|id| (label, id)));
        }
    }
    fields
}

/// The answers of an issue form body by field ID. GitHub writes every field as
/// `### <label>`, a blank line and the value.
pub(crate) fn answers(body: &str) -> Vec<(&'static str, String)> {
    let fields = form_fields();
    body.split("### ")
        .skip(1)
        .filter_map(|section| {
            let (label, value) = section.split_once('\n').unwrap_or((section, ""));
            let id = fields.iter().find(|(known, _)| *known == label.trim())?.1;
            Some((id, value.trim().to_owned()))
        })
        .collect()
}

/// A device report: the coded fields in order and the report code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    /// The issue it was submitted with.
    pub issue: u64,
    /// ID and value of every coded field, in the order the tool codes them.
    pub fields: Vec<(&'static str, String)>,
    /// The report code the tool printed.
    pub code: String,
}

impl Submission {
    /// Takes the coded fields and the code from `values` (field ID, value).
    /// Every missing field is named.
    ///
    /// # Errors
    ///
    /// The problems found, one sentence each.
    pub fn from_values(issue: u64, values: &[(&str, String)]) -> Result<Self, Vec<String>> {
        let value = |id: &str| {
            values
                .iter()
                .find(|(known, value)| *known == id && !value.is_empty() && value != NO_RESPONSE)
                .map(|(_, value)| value.clone())
        };
        let mut problems = Vec::new();
        let mut fields = Vec::new();
        for id in field_ids() {
            match value(id) {
                Some(found) => fields.push((id, found)),
                None => problems.push(format!("The field `{id}` is missing.")),
            }
        }
        let code = value(CODE_ID).unwrap_or_default();
        if code.is_empty() {
            problems.push(format!("The field `{CODE_ID}` is missing."));
        }
        if problems.is_empty() { Ok(Self { issue, fields, code }) } else { Err(problems) }
    }

    /// Reads the report from the body of an issue opened with the form.
    ///
    /// # Errors
    ///
    /// The missing fields.
    pub fn from_issue(issue: u64, body: &str) -> Result<Self, Vec<String>> {
        Self::from_values(issue, &answers(body))
    }

    /// Reads a report file.
    ///
    /// # Errors
    ///
    /// When the file is no JSON object with exactly the issue, the coded
    /// fields and the code, all of them strings except the issue number, or
    /// when it is not written exactly as [`Self::file_text`] writes it. The
    /// last rule refuses what the parsed value does not show: a repeated
    /// key (the parser keeps only the last one), other spacing or order.
    pub fn from_file(text: &str) -> anyhow::Result<Self> {
        let value: Value = serde_json::from_str(text).context("not JSON")?;
        let object = value.as_object().context("not a JSON object")?;
        let issue = object.get("issue").and_then(Value::as_u64).context("no issue number")?;
        let mut values = Vec::new();
        for (id, value) in object.iter().filter(|(id, _)| *id != "issue") {
            let text = value.as_str().with_context(|| format!("`{id}` is no string"))?;
            values.push((id.as_str(), text.to_owned()));
        }
        let submission = Self::from_values(issue, &values)
            .map_err(|problems| anyhow::anyhow!(problems.join(" ")))?;
        if values.len() != submission.fields.len() + 1 {
            bail!("unknown fields besides {}", field_ids().join(", "));
        }
        if submission.file_text() != text {
            bail!("not written as the intake writes it (one field per line, in order, no repeats)");
        }
        Ok(submission)
    }

    /// The value of the coded field `id`; empty for an unknown ID.
    #[must_use]
    pub fn get(&self, id: &str) -> &str {
        self.fields.iter().find(|(known, _)| *known == id).map_or("", |(_, value)| value)
    }

    /// The problems that refuse the report of a successful run: a code that
    /// does not match the fields, or a value the tool would never write.
    #[must_use]
    pub fn problems(&self) -> Vec<String> {
        self.problems_with(|value| value == SUCCESS)
    }

    /// The problems of a report of a failed run, see [`Self::problems`]; its
    /// result must be a failure kind (`FlashError::report_kind`).
    #[must_use]
    pub fn failure_problems(&self) -> Vec<String> {
        self.problems_with(is_failure_kind)
    }

    fn problems_with(&self, result: Check) -> Vec<String> {
        let mut problems = Vec::new();
        if Report::code(&self.fields) != self.code {
            problems.push(
                "The report code does not match the fields. Please submit the link the tool \
                 printed without changing the prefilled fields."
                    .to_owned(),
            );
        }
        let checks: [(&str, Check); 11] = [
            ("result", result),
            ("model", |value| Model::from_device_type(value).is_some()),
            ("generation", |value| value == "Gen1"),
            ("stock-firmware", |value| value == "unknown" || report::is_stock_build(value)),
            ("signal", |value| value == "unknown" || dbm(value).is_some()),
            ("login", is_yes_no),
            ("backup", is_yes_no),
            ("package", |value| ["embedded", "file", "unknown"].contains(&value)),
            ("tool", is_version),
            ("platform", is_platform),
            ("duration", |value| seconds(value).is_some()),
        ];
        for (id, check) in checks {
            if !check(self.get(id)) {
                problems.push(format!("The field `{id}` holds a value the tool does not write."));
            }
        }
        problems
    }

    /// Why the report needs a review although it passes [`Self::problems`].
    #[must_use]
    pub fn doubts(&self, author: &Author) -> Vec<String> {
        let mut doubts = Vec::new();
        let tool = self.get("tool");
        if !author.released.iter().any(|version| version == tool) {
            doubts.push(format!("shelly2esphome {tool} is no released version."));
        }
        if author.account_age_days < MIN_ACCOUNT_AGE_DAYS {
            doubts.push(format!("The account is younger than {MIN_ACCOUNT_AGE_DAYS} days."));
        }
        if author.recent_reports > MAX_RECENT_REPORTS {
            doubts.push(format!(
                "The author opened more than {MAX_RECENT_REPORTS} reports within a day."
            ));
        }
        if self.get("package") != "embedded" {
            doubts
                .push("The run did not use the migration package built into the tool.".to_owned());
        }
        if dbm(self.get("signal")).is_some_and(|signal| !PLAUSIBLE_SIGNAL_DBM.contains(&signal)) {
            doubts.push("The Wi-Fi signal is unusual.".to_owned());
        }
        if seconds(self.get("duration"))
            .is_some_and(|duration| !PLAUSIBLE_DURATION_S.contains(&duration))
        {
            doubts.push("The duration is unusual.".to_owned());
        }
        doubts
    }

    /// The path of the report file below the repository root.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        Path::new(REPORT_DIR).join(format!("{}.json", self.issue))
    }

    /// The report file: the issue number, the coded fields in order and the
    /// code, one per line, so a diff shows a changed value.
    #[must_use]
    pub fn file_text(&self) -> String {
        let code = (CODE_ID, self.code.clone());
        let lines: Vec<String> = self
            .fields
            .iter()
            .chain([&code])
            .map(|(id, value)| format!("  \"{id}\": {}", Value::from(value.as_str())))
            .collect();
        format!("{{\n  \"issue\": {},\n{}\n}}\n", self.issue, lines.join(",\n"))
    }
}

/// A rule for the value of one field.
type Check = fn(&str) -> bool;

/// A failure kind as `FlashError::report_kind` writes it: lowercase words
/// joined by `-`, optionally `:` and the device's reason (`device-failed:
/// hash_mismatch`, `device-failed:http_404`).
fn is_failure_kind(value: &str) -> bool {
    let (kind, reason) = value.split_once(':').unwrap_or((value, "x"));
    value != SUCCESS
        && kind
            .split('-')
            .all(|word| !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_lowercase()))
        && !reason.is_empty()
        && reason
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn is_yes_no(value: &str) -> bool {
    ["yes", "no", "unknown"].contains(&value)
}

/// `<major>.<minor>.<patch>`, digits only.
fn is_version(value: &str) -> bool {
    value.split('.').count() == 3
        && value
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Operating systems as Rust names them (`std::env::consts::OS`) that can
/// run the tool. A closed list: the field must not carry any other text.
const OPERATING_SYSTEMS: [&str; 10] = [
    "linux",
    "macos",
    "windows",
    "freebsd",
    "netbsd",
    "openbsd",
    "dragonfly",
    "illumos",
    "solaris",
    "android",
];
/// Architectures as Rust names them (`std::env::consts::ARCH`).
const ARCHITECTURES: [&str; 9] =
    ["x86_64", "aarch64", "x86", "arm", "riscv64", "powerpc64", "s390x", "loongarch64", "mips64"];

/// `<os>-<arch>` as the tool writes it, e.g. `linux-x86_64`.
fn is_platform(value: &str) -> bool {
    value
        .split_once('-')
        .is_some_and(|(os, arch)| OPERATING_SYSTEMS.contains(&os) && ARCHITECTURES.contains(&arch))
}

/// `-67 dBm`.
fn dbm(value: &str) -> Option<i32> {
    value.strip_suffix(" dBm")?.parse().ok()
}

/// `94 s`.
fn seconds(value: &str) -> Option<u32> {
    value.strip_suffix(" s")?.parse().ok()
}

/// What the workflow found out about the author and the releases.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Author {
    /// Days since the account was created.
    pub account_age_days: u32,
    /// Device reports the author opened within the last day, this one included.
    pub recent_reports: u32,
    /// The released tool versions.
    pub released: Vec<String>,
}

/// What happens to a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Merged without a review.
    Accept,
    /// Waits for a review.
    Review,
    /// Not added; the author can fix the issue.
    Reject,
    /// A report of a failed run (form `device-failure.yml`, which carries
    /// the same label): it stays open as a bug and is not added.
    Failure,
}

/// The decision about one report issue, as the workflow reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Decision {
    /// What happens to the report.
    pub verdict: Verdict,
    /// Why, one sentence each; empty for [`Verdict::Accept`].
    pub reasons: Vec<String>,
    /// The report file below the repository root, unless refused.
    pub file: Option<PathBuf>,
    /// For a [`Verdict::Failure`]: the older report it duplicates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate: Option<Duplicate>,
}

/// Decides about the issue `issue` with `body` and writes its report file
/// below `root` unless it is refused. A report of a failed run (any result
/// but `success`) is no candidate for the device table; for it, the decision
/// names an older `open` report of the same failure it duplicates.
///
/// # Errors
///
/// When the report file cannot be written.
pub fn intake(
    issue: u64,
    body: &str,
    author: &Author,
    open: &[OpenIssue],
    root: &Path,
) -> anyhow::Result<Decision> {
    let failed = answers(body).iter().any(|(id, value)| {
        *id == "result" && !value.is_empty() && value != NO_RESPONSE && value != SUCCESS
    });
    if failed {
        let duplicate = failure::find_original(issue, body, open);
        return Ok(Decision {
            verdict: Verdict::Failure,
            reasons: Vec::new(),
            file: None,
            duplicate,
        });
    }
    let submission = match Submission::from_issue(issue, body) {
        Ok(submission) => submission,
        Err(reasons) => {
            return Ok(Decision { verdict: Verdict::Reject, reasons, file: None, duplicate: None });
        }
    };
    let problems = submission.problems();
    if !problems.is_empty() {
        return Ok(Decision {
            verdict: Verdict::Reject,
            reasons: problems,
            file: None,
            duplicate: None,
        });
    }
    let path = submission.path();
    let target = root.join(&path);
    std::fs::create_dir_all(root.join(REPORT_DIR))
        .and_then(|()| std::fs::write(&target, submission.file_text()))
        .with_context(|| format!("writing {}", target.display()))?;
    let reasons = submission.doubts(author);
    let verdict = if reasons.is_empty() { Verdict::Accept } else { Verdict::Review };
    Ok(Decision { verdict, reasons, file: Some(path), duplicate: None })
}

/// Checks every report file below `root`: its name is its issue number and
/// it passes the checks a submitted report passes. Returns how many there are.
///
/// # Errors
///
/// Every file that fails, with the reason.
pub fn check_reports(root: &Path) -> anyhow::Result<usize> {
    let dir = root.join(REPORT_DIR);
    let listed = std::fs::read_dir(&dir).and_then(|entries| {
        entries.map(|entry| entry.map(|entry| entry.path())).collect::<Result<Vec<_>, _>>()
    });
    let mut paths = match listed {
        Ok(paths) => paths,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).with_context(|| format!("reading {}", dir.display())),
    };
    paths.sort();
    let mut failures = Vec::new();
    for path in &paths {
        let checked = std::fs::read_to_string(path).map_err(anyhow::Error::from).and_then(|text| {
            let submission = Submission::from_file(&text)?;
            let problems = submission.problems();
            if !problems.is_empty() {
                bail!(problems.join(" "));
            }
            if path.file_name() != submission.path().file_name() {
                bail!("the file is not named after its issue {}", submission.issue);
            }
            Ok(())
        });
        if let Err(error) = checked {
            failures.push(format!("{}: {error:#}", path.display()));
        }
    }
    if failures.is_empty() { Ok(paths.len()) } else { bail!(failures.join("\n")) }
}

#[cfg(test)]
#[path = "unit_tests/intake.rs"]
pub(crate) mod tests;
