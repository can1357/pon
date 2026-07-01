#![doc = "Thin command-line entry point for Pon."]

use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    match pon_cli::run_from_env() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = io::stdout().flush();
            eprintln!("pon: {error:#}");
            ExitCode::FAILURE
        }
    }
}
