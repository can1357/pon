use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    match pon_pm::cli::run_from_env() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = io::stdout().flush();
            eprintln!("pon-pm: {error}");
            ExitCode::FAILURE
        }
    }
}
