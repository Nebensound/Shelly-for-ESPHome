//! The device report link and the issue forms agree on every field.
#![allow(missing_docs, clippy::unwrap_used)]

use std::time::Duration;

use shelly2esphome::report::{DeviceFacts, Report};

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
