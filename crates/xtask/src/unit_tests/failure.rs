//! Unit tests of `crate::failure` (not counted as production code for coverage).

use shelly2esphome::report::Report;

use super::*;
use crate::intake::tests::{body, body_of, report};

/// The body of a failure report of `model` with `result`, as the tool fills
/// it in.
pub fn failure_body(model: &str, result: &str) -> String {
    let mut fields = report().fields();
    for field in &mut fields {
        match field.0 {
            "model" => field.1 = model.to_owned(),
            "result" => field.1 = result.to_owned(),
            _ => {}
        }
    }
    let code = Report::code(&fields);
    fields.push(("report-code", code));
    body_of(&fields)
}

fn open(number: u64, body: Option<String>) -> OpenIssue {
    OpenIssue { number, body }
}

const KIND: &str = "device-failed:hash_mismatch";

#[test]
fn finds_the_oldest_open_report_of_the_same_failure() {
    let same = failure_body("SHSW-25", KIND);
    let issues = [
        open(5, Some(same.clone())),
        open(3, Some(same.clone())),
        open(2, Some(failure_body("SHSW-PM", KIND))),
        open(4, Some(failure_body("SHSW-25", "flash-timeout"))),
        open(1, Some(same.replace("SHSW-25", "SHSW-21"))),
        open(6, None),
        open(9, Some(same.clone())),
    ];
    let duplicate = find_original(8, &same, &issues).expect("duplicate");
    assert_eq!(duplicate.original, 3);
    assert!(duplicate.fields.starts_with("| Field | Value |\n|---|---|\n| result | device-failed:hash_mismatch |\n| model | SHSW-25 |\n"));
    assert_eq!(duplicate.fields.lines().count(), 13);

    // The oldest report is no duplicate, and one without an older twin has none.
    assert_eq!(find_original(3, &same, &issues), None);
    assert_eq!(find_original(8, &failure_body("SHDM-2", KIND), &issues), None);
}

#[test]
fn takes_only_valid_failure_reports() {
    let same = failure_body("SHSW-25", KIND);
    let issues = [open(3, Some(same.clone()))];
    // A success report, an edited field and no form at all have no original.
    assert_eq!(find_original(8, &body(), &issues), None);
    assert_eq!(find_original(8, &same.replace("-67 dBm", "-40 dBm"), &issues), None);
    assert_eq!(find_original(8, "free text", &issues), None);
}

#[test]
fn deserializes_the_issue_list_of_gh() {
    let issues: Vec<OpenIssue> =
        serde_json::from_str(r#"[{"number":3,"body":"text"},{"number":4,"body":null}]"#)
            .expect("issue list");
    assert_eq!(issues, [open(3, Some("text".into())), open(4, None)]);
}
