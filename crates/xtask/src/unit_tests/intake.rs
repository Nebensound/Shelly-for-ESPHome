//! Unit tests of `crate::intake` (not counted as production code for coverage).

use std::path::Path;

use shelly2esphome::report::PackageSource;

use super::*;

/// A report as the tool fills it after a successful run.
pub fn report() -> Report {
    Report {
        result: SUCCESS.into(),
        facts: DeviceFacts {
            model: Some(Model::Shelly25),
            stock_firmware: Some("20230913-114008/v1.14.0-gcb84623".into()),
            rssi_dbm: Some(-67),
            login: Some(true),
            backup: Some(true),
            package: Some(PackageSource::Embedded),
        },
        duration: Duration::from_secs(94),
    }
}

/// The coded fields of `report()` with `id` set to `value` and a code over
/// them, so only the changed value can be wrong.
pub fn coded_with(id: &str, value: &str) -> Vec<(&'static str, String)> {
    let mut fields = report().fields();
    for field in &mut fields {
        if field.0 == id {
            field.1 = value.to_owned();
        }
    }
    let code = Report::code(&fields);
    fields.push((CODE_ID, code));
    fields
}

/// An issue body as GitHub writes it for the form.
pub fn body_of(values: &[(&str, String)]) -> String {
    let fields = form_fields();
    values
        .iter()
        .map(|(id, value)| {
            let label = fields.iter().find(|(_, known)| known == id).map(|(label, _)| *label);
            format!("### {}\n\n{value}\n\n", label.expect("a field of the form"))
        })
        .collect()
}

/// The body of `report()` with the optional fields.
pub fn body() -> String {
    let mut values = coded_with("", "");
    values.push(("esphome", NO_RESPONSE.into()));
    values.push(("notes", "Works fine.\n\nThanks!".into()));
    body_of(&values)
}

fn tool() -> String {
    report().fields().into_iter().find(|(id, _)| *id == "tool").expect("tool").1
}

/// An author whose report is merged without a review.
pub fn trusted() -> Author {
    Author { account_age_days: 400, recent_reports: 1, released: vec!["0.1.0".into(), tool()] }
}

/// An empty directory for one test.
pub fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xtask-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn submission(values: &[(&str, String)]) -> Submission {
    Submission::from_values(12, values).expect("complete")
}

#[test]
fn knows_every_field_of_the_form() {
    let ids: Vec<&str> = form_fields().into_iter().map(|(_, id)| id).collect();
    let mut expected = field_ids();
    expected.extend([CODE_ID, "esphome", "notes"]);
    assert_eq!(ids, expected);
}

#[test]
fn reads_the_answers_of_an_issue_body() {
    let body = format!("Text before the form\n### Unknown heading\n\nignored\n{}", body());
    let submission = Submission::from_issue(12, &body).expect("complete");
    let fields = report().fields();
    let code = Report::code(&fields);
    assert_eq!(submission, Submission { issue: 12, fields, code });
    assert!(answers("### Notes").contains(&("notes", String::new())));
}

#[test]
fn names_every_missing_field() {
    let mut values = coded_with("", "");
    values.retain(|(id, _)| *id != "model" && *id != CODE_ID);
    for (id, value) in &mut values {
        match *id {
            "signal" => *value = String::new(),
            "backup" => *value = NO_RESPONSE.into(),
            _ => {}
        }
    }
    let problems = Submission::from_issue(12, &body_of(&values)).expect_err("incomplete");
    assert_eq!(
        problems,
        [
            "The field `model` is missing.",
            "The field `signal` is missing.",
            "The field `backup` is missing.",
            "The field `report-code` is missing."
        ]
    );
}

#[test]
fn accepts_what_the_tool_writes() {
    let fine = submission(&coded_with("", ""));
    assert_eq!(fine.problems(), Vec::<String>::new());
    for (id, value) in [
        ("stock-firmware", "unknown"),
        ("signal", "unknown"),
        ("login", "no"),
        ("backup", "unknown"),
        ("package", "file"),
        ("package", "unknown"),
        ("platform", "windows-aarch64"),
        ("platform", "freebsd-riscv64"),
    ] {
        assert_eq!(submission(&coded_with(id, value)).problems(), Vec::<String>::new(), "{id}");
    }
}

#[test]
fn refuses_a_changed_field() {
    let mut values = coded_with("", "");
    values[4].1 = "-40 dBm".into();
    let problems = submission(&values).problems();
    assert_eq!(problems.len(), 1);
    assert!(problems[0].starts_with("The report code does not match the fields."));
}

#[test]
fn refuses_values_the_tool_does_not_write() {
    for (id, value) in [
        ("result", "device-failed:hash_mismatch"),
        ("model", "SHSW-99"),
        ("generation", "Gen2"),
        ("stock-firmware", "my-kitchen-light/v1.14.0"),
        ("signal", "-67"),
        ("signal", "strong dBm"),
        ("login", "maybe"),
        ("backup", "sure"),
        ("package", "zip"),
        ("tool", "0.8"),
        ("tool", "0..8"),
        ("tool", "0.8.x"),
        ("platform", "linux"),
        ("platform", "linux-"),
        ("platform", "Linux-x86_64"),
        ("platform", "my_kitchen-private_wifi"),
        ("platform", "linux-wifi"),
        ("duration", "94"),
        ("duration", "long s"),
    ] {
        let problems = submission(&coded_with(id, value)).problems();
        assert_eq!(
            problems,
            [format!("The field `{id}` holds a value the tool does not write.")],
            "{id}={value}"
        );
    }
}

#[test]
fn doubts_what_is_not_clearly_genuine() {
    let fine = submission(&coded_with("", ""));
    assert_eq!(fine.doubts(&trusted()), Vec::<String>::new());
    assert_eq!(
        submission(&coded_with("signal", "unknown")).doubts(&trusted()),
        Vec::<String>::new()
    );
    for (values, author, doubt) in [
        (
            coded_with("", ""),
            Author { released: vec!["0.1.0".into()], ..trusted() },
            "is no released version.",
        ),
        (coded_with("", ""), Author { account_age_days: 29, ..trusted() }, "younger than 30 days."),
        (coded_with("", ""), Author { recent_reports: 4, ..trusted() }, "more than 3 reports"),
        (coded_with("package", "file"), trusted(), "migration package"),
        (coded_with("signal", "-101 dBm"), trusted(), "signal is unusual."),
        (coded_with("signal", "-9 dBm"), trusted(), "signal is unusual."),
        (coded_with("duration", "19 s"), trusted(), "duration is unusual."),
        (coded_with("duration", "1801 s"), trusted(), "duration is unusual."),
    ] {
        let doubts = submission(&values).doubts(&author);
        assert_eq!(doubts.len(), 1, "{doubt}: {doubts:?}");
        assert!(doubts[0].contains(doubt), "{doubts:?}");
    }
}

#[test]
fn writes_one_field_per_line() {
    let mut values = coded_with("", "");
    values[0].1 = "say \"hi\"".into();
    let text = submission(&values).file_text();
    assert!(text.starts_with(
        "{\n  \"issue\": 12,\n  \"result\": \"say \\\"hi\\\"\",\n  \"model\": \"SHSW-25\",\n"
    ));
    assert!(text.ends_with(&format!(",\n  \"report-code\": \"{}\"\n}}\n", values[11].1)));
    assert_eq!(text.lines().count(), 15);
    assert_eq!(Submission::from_file(&text).expect("round trip"), submission(&values));
    assert_eq!(submission(&values).path(), Path::new("docs/device-reports/12.json"));
}

#[test]
fn takes_a_report_in() {
    let root = scratch("intake");
    let decision = intake(12, &body(), Form::Success, &trusted(), &[], &root).expect("written");
    assert_eq!(
        decision,
        Decision {
            verdict: Verdict::Accept,
            reasons: vec![],
            file: Some("docs/device-reports/12.json".into()),
            duplicate: None,
        }
    );
    let written = std::fs::read_to_string(root.join("docs/device-reports/12.json")).expect("file");
    assert_eq!(written, submission(&coded_with("", "")).file_text());

    let review = intake(
        13,
        &body(),
        Form::Success,
        &Author { account_age_days: 1, ..trusted() },
        &[],
        &root,
    )
    .expect("written");
    assert_eq!(review.verdict, Verdict::Review);
    assert_eq!(review.reasons.len(), 1);
    assert_eq!(check_reports(&root).expect("valid"), 2);

    let mut edited = coded_with("", "");
    edited[4].1 = "-40 dBm".into();
    let refused =
        intake(14, &body_of(&edited), Form::Success, &trusted(), &[], &root).expect("decided");
    assert_eq!((refused.verdict, refused.file), (Verdict::Reject, None));
    let incomplete = intake(15, "", Form::Success, &trusted(), &[], &root).expect("decided");
    assert_eq!((incomplete.verdict, incomplete.reasons.len()), (Verdict::Reject, 12));
    assert!(!root.join("docs/device-reports/14.json").exists());

    // A report from the failure form is no candidate for the table.
    let failed = coded_with("result", "device-failed:hash_mismatch");
    let failure = intake(17, &body_of(&failed), Form::Failure, &trusted(), &[], &root);
    assert_eq!(
        failure.expect("decided"),
        Decision { verdict: Verdict::Failure, reasons: vec![], file: None, duplicate: None }
    );
    // In the success form, any other result is refused with a reason, even a
    // failure kind or a typo.
    for result in ["device-failed:hash_mismatch", "Success", "succes", "", NO_RESPONSE] {
        let changed = coded_with("result", result);
        let refused = intake(18, &body_of(&changed), Form::Success, &trusted(), &[], &root);
        assert_eq!(refused.expect("decided").verdict, Verdict::Reject, "{result:?}");
    }
    assert!(!root.join("docs/device-reports/17.json").exists());

    let file = root.join("a-file");
    std::fs::write(&file, "").expect("file");
    let error =
        intake(16, &body(), Form::Success, &trusted(), &[], &file).expect_err("not a directory");
    assert!(format!("{error:#}").starts_with("writing "), "{error:#}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn reads_only_complete_report_files() {
    let text = submission(&coded_with("", "")).file_text();
    for (broken, error) in [
        ("not json".to_owned(), "not JSON"),
        ("[]".to_owned(), "not a JSON object"),
        (text.replace("\"issue\": 12", "\"issue\": \"12\""), "no issue number"),
        (text.replace("\"Gen1\"", "1"), "`generation` is no string"),
        (text.replace("  \"model\": \"SHSW-25\",\n", ""), "The field `model` is missing."),
        (
            text.replace("{\n", "{\n  \"name\": \"kitchen\",\n"),
            "unknown fields besides result, model",
        ),
        // The parser keeps the last of repeated keys; the first must not hide.
        (
            text.replace("{\n", "{\n  \"model\": \"my kitchen\",\n"),
            "not written as the intake writes it",
        ),
        (text.replace("\n  ", "\n    "), "not written as the intake writes it"),
    ] {
        let found = Submission::from_file(&broken).expect_err(error);
        assert!(format!("{found:#}").starts_with(error), "{found:#}");
    }
}

#[test]
fn checks_every_report_file() {
    let root = scratch("check");
    assert_eq!(check_reports(&root).expect("no directory yet"), 0);
    let dir = root.join(REPORT_DIR);
    std::fs::create_dir_all(&dir).expect("directory");
    let fine = submission(&coded_with("", ""));
    std::fs::write(dir.join("12.json"), fine.file_text()).expect("file");
    assert_eq!(check_reports(&root).expect("valid"), 1);

    std::fs::write(dir.join("13.json"), fine.file_text()).expect("file");
    let mut edited = coded_with("", "");
    edited[4].1 = "-40 dBm".into();
    std::fs::write(dir.join("14.json"), submission(&edited).file_text().replace("12", "14"))
        .expect("file");
    std::fs::write(dir.join("15.json"), "[]").expect("file");
    std::fs::create_dir(dir.join("16.json")).expect("directory");
    let error = format!("{:#}", check_reports(&root).expect_err("invalid"));
    let lines: Vec<&str> = error.lines().collect();
    assert_eq!(lines.len(), 4, "{error}");
    assert!(lines[0].ends_with("13.json: the file is not named after its issue 12"), "{error}");
    assert!(lines[1].contains("14.json: The report code does not match"), "{error}");
    assert!(lines[2].ends_with("15.json: not a JSON object"), "{error}");
    assert!(lines[3].contains("16.json: "), "{error}");

    let _ = std::fs::remove_dir_all(&dir);
    std::fs::write(&dir, "").expect("a file instead of the directory");
    let error = check_reports(&root).expect_err("unreadable");
    assert!(format!("{error:#}").starts_with("reading "), "{error:#}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn checks_failure_reports_with_the_kinds_of_the_tool() {
    for kind in ["foo-bar", "device-failed:anything", "Device-unreachable"] {
        let refused = submission(&coded_with("result", kind));
        assert_eq!(refused.failure_problems().len(), 1, "{kind}");
    }
    let failed = submission(&coded_with("result", "device-failed:hash_mismatch"));
    assert_eq!(failed.failure_problems(), Vec::<String>::new());
    assert_eq!(failed.problems().len(), 1);
    let success = submission(&coded_with("", ""));
    assert_eq!(success.failure_problems().len(), 1);
}
