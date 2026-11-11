//! `cargo xtask`: see the library.

use std::process::ExitCode;

fn main() -> ExitCode {
    match xtask::run(&std::env::args_os().collect::<Vec<_>>(), &mut std::io::stdout()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
