//! The device report link and the issue forms agree on every field.
#![allow(missing_docs, clippy::unwrap_used)]

use std::time::Duration;

use shelly2esphome::{
    flash::DEVICE_ERROR_REASONS,
    report::{DeviceFacts, Report},
};

/// The IDs of the required fields of an issue form, in order.
fn required_ids(form: &str) -> Vec<String> {
    let path = format!("{}/../../.github/ISSUE_TEMPLATE/{form}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(path).unwrap();
    let mut ids = Vec::new();
    let mut current = None;
    for line in text.lines() {
        if let Some(id) = line.strip_prefix("    id: ") {
            current = Some(id.to_owned());
        }
        if line == "      required: true" {
            ids.push(current.take().unwrap());
        }
    }
    ids
}

#[test]
fn the_tool_fills_every_required_field_of_both_forms() {
    let report = Report {
        result: "success".into(),
        facts: DeviceFacts::default(),
        duration: Duration::ZERO,
    };
    let mut filled: Vec<String> =
        report.fields().into_iter().map(|(id, _)| id.to_owned()).collect();
    filled.push("report-code".into());
    for form in ["device-report.yml", "device-failure.yml"] {
        assert_eq!(required_ids(form), filled, "{form}");
    }
}

/// Every literal reason the migration firmware sets is one a report may
/// carry: the returns of the `*_result_str` functions and the `fail()` and
/// relocation reasons in `main.c`.
#[test]
fn knows_every_error_reason_of_the_migration_firmware() {
    let sources = format!("{}/../../migration_firmware/src", env!("CARGO_MANIFEST_DIR"));
    let mut reasons = Vec::new();
    for entry in std::fs::read_dir(sources).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|extension| extension != "c") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let in_result_str = |line: &str| line.trim_start().starts_with("return \"");
        let mut in_function = false;
        for line in text.lines() {
            if line.contains("_result_str(") && line.contains('{') {
                in_function = true;
            } else if line.starts_with('}') {
                in_function = false;
            }
            let literal = |prefix: &str| {
                line.split_once(prefix)
                    .and_then(|(_, rest)| rest.split_once('"'))
                    .map(|(reason, _)| reason.to_owned())
            };
            if in_function && in_result_str(line) {
                reasons.extend(literal("return \""));
            }
            reasons.extend(literal("fail(\""));
            if line.contains("s_error") {
                reasons.extend(literal("\"%s\", \""));
            }
        }
    }
    reasons.retain(|reason| reason != "ok");
    reasons.sort();
    reasons.dedup();
    assert!(reasons.len() >= 15, "{reasons:?}");
    for reason in &reasons {
        assert!(DEVICE_ERROR_REASONS.contains(&reason.as_str()), "{reason} is missing");
    }
}
