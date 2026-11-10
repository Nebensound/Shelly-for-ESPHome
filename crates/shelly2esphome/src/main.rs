//! Command line entry point of `shelly2esphome`; the interface lives in
//! [`shelly2esphome::cli`].

use std::process::ExitCode;

use shelly2esphome::cli::{self, Settings, Terminal};

#[tokio::main]
async fn main() -> ExitCode {
    let stdin = std::io::stdin();
    let mut terminal = Terminal {
        out: &mut std::io::stdout(),
        err: &mut std::io::stderr(),
        input: &mut stdin.lock(),
    };
    cli::run(std::env::args_os(), &Settings::default(), &mut terminal).await
}
