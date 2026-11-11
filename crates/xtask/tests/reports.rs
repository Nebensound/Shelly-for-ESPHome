//! The report files in the repository pass the checks a submitted report
//! passes; CI runs this for every change, so a broken file cannot be merged.
#![allow(missing_docs)]

use std::path::Path;

#[test]
fn every_device_report_in_the_repository_is_valid() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if let Err(error) = xtask::intake::check_reports(&root) {
        panic!("{error:#}");
    }
}
