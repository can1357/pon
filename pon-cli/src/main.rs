#![doc = "Phase-A command-line entry point for pon."]

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use pon_ir::lower_source;
use pon_jit::JitEngine;

fn main() -> ExitCode {
    match run_cli() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = io::stdout().flush();
            eprintln!("pon: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_cli() -> Result<()> {
    let mut args = env::args();
    let program = args.next().unwrap_or_else(|| "pon".to_owned());
    match args.next().as_deref() {
        Some("run") => {
            let file = args.next().context("missing file for `pon run <file>`")?;
            if let Some(extra) = args.next() {
                bail!("unexpected argument `{extra}` for `pon run <file>`");
            }
            run_file(Path::new(&file))
        }
        Some("build") => bail!("`pon build` is unsupported in Phase A"),
        Some("repl") => bail!("`pon repl` is unsupported in Phase A"),
        Some(command) => bail!("unknown subcommand `{command}`\nusage: {program} run <file>"),
        None => bail!("usage: {program} run <file>"),
    }
}

fn run_file(path: &Path) -> Result<()> {
    let source = fs::read_to_string(path).with_context(|| format!("failed to read UTF-8 source `{}`", path.display()))?;
    let module = lower_source(&source).context("failed to parse/lower source")?;
    let mut engine = JitEngine::new();
    engine.run(&module).context("JIT execution failed")?;
    io::stdout().flush().context("failed to flush stdout")
}
