//! The binary reports success and failure through its exit code.
#![allow(missing_docs, clippy::unwrap_used)]

use std::process::Command;

#[test]
fn exits_with_the_result_of_the_task() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["check-reports", "--root", root])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8(output.stdout).unwrap().ends_with(" device reports are valid\n"));

    let output = Command::new(env!("CARGO_BIN_EXE_xtask")).arg("unknown-task").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stderr).unwrap().starts_with("error: unrecognized subcommand")
    );
}
