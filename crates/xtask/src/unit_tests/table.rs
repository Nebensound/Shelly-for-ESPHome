//! Unit tests of `crate::table` (not counted as production code for coverage).

use super::*;
use crate::intake::tests::{coded_with, scratch};

/// A valid report of `issue` with the given fields changed.
fn report_with(issue: u64, changes: &[(&str, &str)]) -> Submission {
    let mut fields = coded_with("", "");
    fields.pop();
    for (id, value) in changes {
        for field in &mut fields {
            if field.0 == *id {
                field.1 = (*value).to_owned();
            }
        }
    }
    let code = shelly2esphome::report::Report::code(&fields);
    Submission { issue, fields, code }
}

#[test]
fn takes_the_version_out_of_a_stock_build() {
    for (build, version) in [
        ("20230913-114008/v1.14.0-gcb84623", Some("v1.14.0")),
        ("20210115-102730/v1.9.4@e2732e05", Some("v1.9.4")),
        ("20230309-114628/v1.13.0-rc1-g1234567", Some("v1.13.0-rc1")),
        ("20221027-091427/v1.12.1-beta2", Some("v1.12.1-beta2")),
        ("unknown", None),
    ] {
        assert_eq!(stock_version(build), version, "{build}");
    }
}

#[test]
fn renders_one_row_per_model() {
    assert_eq!(render(&[]), "No device reports yet.\n");
    let reports = [
        report_with(12, &[]),
        report_with(13, &[("stock-firmware", "20210115-102730/v1.9.4@e2732e05")]),
        report_with(14, &[("stock-firmware", "20230913-114008/v1.14.0-gaaaaaaa")]),
        report_with(15, &[("stock-firmware", "20230309-114628/v1.14.0-rc1-g1234567")]),
        report_with(16, &[("stock-firmware", "20230309-114628/v1.10.0")]),
        report_with(17, &[("model", "SHDM-2"), ("stock-firmware", "unknown")]),
        report_with(18, &[("model", "SHSW-PM")]),
        report_with(
            19,
            &[
                ("model", "S1G3"),
                ("generation", "Gen3"),
                ("stock-firmware", "20261109-150519/2.2.0-g7968dca"),
            ],
        ),
    ];
    assert_eq!(
        render(&reports),
        "| Model | Stock firmware |\n\
         |---|---|\n\
         | S1G3 (Gen3) | 2.2.0 |\n\
         | ShellyDimmer2 (`SHDM-2`) | unknown |\n\
         | Shelly25 (`SHSW-25`) | v1.14.0, v1.14.0-rc1, v1.10.0, v1.9.4 |\n\
         | Shelly1PM (`SHSW-PM`) | v1.14.0 |\n"
    );
}

#[test]
fn writes_the_table_between_the_markers() {
    let root = scratch("table");
    let readme = root.join("README.md");
    let text = format!("# Title\n\n{START}\nold table\n{END}\n\nMore text.\n");
    std::fs::write(&readme, &text).expect("README");
    assert!(update_readme(&root).expect("updated"));
    let updated = std::fs::read_to_string(&readme).expect("README");
    assert_eq!(
        updated,
        format!("# Title\n\n{START}\nNo device reports yet.\n{END}\n\nMore text.\n")
    );
    assert!(!update_readme(&root).expect("unchanged"));

    let dir = root.join(intake::REPORT_DIR);
    std::fs::create_dir_all(&dir).expect("directory");
    std::fs::write(dir.join("12.json"), report_with(12, &[]).file_text()).expect("report");
    assert!(update_readme(&root).expect("updated"));
    let updated = std::fs::read_to_string(&readme).expect("README");
    assert!(updated.contains("| Shelly25 (`SHSW-25`) | v1.14.0 |\n<!-- device-table:end -->"));

    // The markers stay where they are; nothing else changes.
    assert!(updated.starts_with("# Title\n\n<!-- device-table:start -->\n| Model |"));
    assert!(updated.ends_with("<!-- device-table:end -->\n\nMore text.\n"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn names_what_keeps_the_table_from_being_written() {
    let root = scratch("table-errors");
    let readme = root.join("README.md");
    let error = |root: &Path| format!("{:#}", update_readme(root).expect_err("fails"));
    assert!(error(&root).starts_with("reading "));
    std::fs::write(&readme, "no markers").expect("README");
    assert_eq!(error(&root), "README.md has no start marker");
    std::fs::write(&readme, format!("{START}\nno end")).expect("README");
    assert_eq!(error(&root), "README.md has no end marker");

    // A report file that fails its check stops the table.
    let dir = root.join(intake::REPORT_DIR);
    std::fs::create_dir_all(&dir).expect("directory");
    std::fs::write(dir.join("12.json"), "[]").expect("report");
    assert!(error(&root).ends_with("12.json: not a JSON object"));
    std::fs::remove_file(dir.join("12.json")).expect("removed");

    // A README the build may not write.
    std::fs::write(&readme, format!("{START}\nold\n{END}\n")).expect("README");
    let mut permissions = std::fs::metadata(&readme).expect("metadata").permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&readme, permissions.clone()).expect("read-only");
    assert!(error(&root).starts_with("writing "));
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(&readme, permissions).expect("writable");
    let _ = std::fs::remove_dir_all(root);
}
