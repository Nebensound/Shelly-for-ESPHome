//! The built binary itself: `main.rs` wires the CLI to the process.
#![allow(missing_docs, clippy::unwrap_used)]

use std::process::Command;

#[test]
fn the_binary_prints_its_version() {
    let output =
        Command::new(env!("CARGO_BIN_EXE_shelly2esphome")).arg("--version").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.trim(), format!("shelly2esphome {}", env!("CARGO_PKG_VERSION")));
}

#[test]
fn the_binary_reports_errors_with_exit_code_1() {
    let output = Command::new(env!("CARGO_BIN_EXE_shelly2esphome"))
        .args(["check", "/does/not/exist.bin"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8(output.stderr).unwrap().contains("Error: cannot read"));
}
