#![doc = "Command-line entry point for the pon binary."]

use std::io::{self, Write};
use std::process::ExitCode;

use pon::run::{SystemExitRequested, UncaughtExceptionReported};

fn main() -> ExitCode {
    match pon::cli::run_from_env() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = io::stdout().flush();
            // `sys.exit(code)` surfaces as `SystemExitRequested`: exit with its
            // status (any message was already printed) rather than the generic
            // uncaught-error report.
            if let Some(SystemExitRequested(code)) = error.downcast_ref::<SystemExitRequested>() {
                return ExitCode::from(*code as u8);
            }
            if error.downcast_ref::<UncaughtExceptionReported>().is_some() {
                return ExitCode::FAILURE;
            }
            eprintln!("pon: {error:#}");
            ExitCode::FAILURE
        }
    }
}
