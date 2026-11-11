//! Unit tests of `crate::run` (not counted as production code for coverage).

use std::path::Path;

use super::*;
use crate::intake::tests::{body, scratch, trusted};

fn run_with(args: &[&str]) -> (anyhow::Result<()>, String) {
    let mut out = Vec::new();
    let args: Vec<OsString> = ["cargo xtask"].iter().chain(args).map(OsString::from).collect();
    let result = run(&args, &mut out);
    (result, String::from_utf8(out).expect("UTF-8"))
}

fn event(root: &Path, issue: &Value) -> String {
    let path = root.join("event.json");
    std::fs::write(&path, serde_json::json!({ "action": "opened", "issue": issue }).to_string())
        .expect("event");
    path.display().to_string()
}

#[test]
fn prints_the_decision_about_an_issue() {
    let root = scratch("run-intake");
    let path = event(&root, &serde_json::json!({ "number": 12, "body": body() }));
    let released: Vec<String> = trusted()
        .released
        .iter()
        .flat_map(|version| ["--released".to_owned(), version.clone()])
        .collect();
    let mut args = vec!["report-intake", "--event", &path, "--account-age-days", "400"];
    args.extend(["--recent-reports", "1", "--root", root.to_str().expect("UTF-8")]);
    args.extend(released.iter().map(String::as_str));
    let (result, out) = run_with(&args);
    result.expect("decided");
    assert_eq!(
        out,
        "{\"verdict\":\"accept\",\"reasons\":[],\"file\":\"docs/device-reports/12.json\"}\n"
    );

    // Without released versions the same report needs a review.
    let (result, out) = run_with(&args[..args.len() - released.len()]);
    result.expect("decided");
    assert!(out.starts_with("{\"verdict\":\"review\",\"reasons\":[\"shelly2esphome "), "{out}");

    let (result, out) = run_with(&["check-reports", "--root", root.to_str().expect("UTF-8")]);
    result.expect("valid");
    assert_eq!(out, "1 device reports are valid\n");

    // A form whose fields were all left empty has no body.
    let path = event(&root, &serde_json::json!({ "number": 13, "body": null }));
    let (result, out) = run_with(&[
        "report-intake",
        "--event",
        &path,
        "--account-age-days",
        "1",
        "--recent-reports",
        "1",
    ]);
    result.expect("decided");
    assert!(
        out.starts_with("{\"verdict\":\"reject\",\"reasons\":[\"The field `result` is missing.\""),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn names_what_went_wrong() {
    let root = scratch("run-errors");
    let missing = root.join("missing.json").display().to_string();
    let no_issue = event(&root, &Value::Null);
    let not_json = root.join("not.json");
    std::fs::write(&not_json, "{").expect("file");
    let not_json = not_json.display().to_string();
    let a_file = event(&root, &Value::Null);
    let intake = |event: &str, root: &str| {
        run_with(&[
            "report-intake",
            "--event",
            event,
            "--account-age-days",
            "1",
            "--recent-reports",
            "1",
            "--root",
            root,
        ])
    };
    let root_text = root.to_str().expect("UTF-8");
    for ((result, out), error) in [
        (intake(&missing, root_text), "reading "),
        (intake(&not_json, root_text), "the event is no JSON"),
        (intake(&no_issue, root_text), "the event names no issue"),
        (run_with(&["check-reports", "--root", &a_file]), "reading "),
        (run_with(&["unknown-task"]), "error: unrecognized subcommand"),
    ] {
        let error_text = format!("{:#}", result.expect_err(error));
        assert!(error_text.starts_with(error), "{error_text}");
        assert_eq!(out, "");
    }
    // The event file as the root: the report file cannot be written.
    let path = event(&root, &serde_json::json!({ "number": 12, "body": body() }));
    let error = intake(&path, &path).0.expect_err("not a directory");
    assert!(format!("{error:#}").starts_with("writing "), "{error:#}");
    let _ = std::fs::remove_dir_all(root);
}

/// A writer that refuses everything.
struct Closed;

impl Write for Closed {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("closed"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn reports_a_failed_write() {
    let root = scratch("run-closed");
    let args = ["cargo xtask", "check-reports", "--root", root.to_str().expect("UTF-8")];
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let error = run(&args, &mut Closed).expect_err("closed");
    assert_eq!(format!("{error:#}"), "closed");
    let _ = std::fs::remove_dir_all(root);
}
