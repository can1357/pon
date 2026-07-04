#![doc = "Thin command-line entry point for Pon."]

use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    match pon_cli::run_from_env() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = io::stdout().flush();
            // `sys.exit(code)` surfaces as a `SystemExitRequested`: exit with
            // its status (any message was already printed) rather than the
            // generic uncaught-error report.
            if let Some(pon_cli::SystemExitRequested(code)) = error.downcast_ref::<pon_cli::SystemExitRequested>() {
                return ExitCode::from(*code as u8);
            }
            eprintln!("pon: {error:#}");
            ExitCode::FAILURE
        }
    }
}
