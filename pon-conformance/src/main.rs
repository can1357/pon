#![doc = "Differential conformance runner and Phase-B/Phase-C ratchet gates."]

mod aot;
mod ratchet;
mod scoreboard;
mod suite;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::scoreboard::Status;
use crate::suite::SuiteName;

#[derive(Clone, Debug, Eq, PartialEq)]
struct Cli {
    suite: SuiteName,
    mode: Mode,
    check_floor: bool,
    update_floor: bool,
    modules: Vec<PathBuf>,
    bench: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Jit,
    Aot,
}

impl Mode {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "jit" => Ok(Self::Jit),
            "aot" => Ok(Self::Aot),
            _ => bail!("unsupported mode `{value}`"),
        }
    }
}

fn main() -> ExitCode {
    match run_cli() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("pon-conformance: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_cli() -> Result<()> {
    let cli = parse_cli(env::args().skip(1))?;
    let root = suite::workspace_root();

    if cli.bench {
        if cli.mode != Mode::Jit {
            bail!("`--bench` uses the JIT tier-up path; omit `--mode` or pass `--mode jit`")
        }
        if cli.suite != SuiteName::Slice {
            bail!("`--bench` is independent of `--suite`; omit `--suite`")
        }
        if cli.check_floor || cli.update_floor {
            bail!("floor checks are not defined for `--bench`")
        }

        run_bench_gate(&root, &cli.modules)?;
        return Ok(());
    }

    match (cli.mode, cli.suite) {
        (Mode::Jit, SuiteName::Slice) => {
            if cli.check_floor || cli.update_floor {
                bail!("floor checks are only defined for `--suite cpython`")
            }

            let scoreboard = suite::run_slice_suite(&root, &cli.modules)?;
            println!("{}", scoreboard.to_json());
            if scoreboard.has_status(Status::Fail) {
                bail!("one or more Phase-A slice scripts failed")
            }
        }
        (Mode::Jit, SuiteName::Cpython) => {
            let floor = ratchet::Floor::read_or_default(&root)?;
            let scoreboard = suite::run_cpython_suite(&root, &cli.modules)?;
            println!("{}", scoreboard.to_json());

            if cli.check_floor {
                let report = ratchet::check_floor(&floor, &scoreboard);
                if !report.is_ok() {
                    bail!("{}", report.message())
                }
            }

            if cli.update_floor {
                let cpython_tag = scoreboard.cpython_tag.as_deref().unwrap_or(&floor.cpython_tag);
                ratchet::write_floor_from_scoreboard(&root, cpython_tag, &scoreboard)?;
            }

            if scoreboard.has_status(Status::SemanticsDivergent) {
                bail!("one or more CPython modules produced an unclassified semantic divergence")
            }
        }
        (Mode::Aot, SuiteName::CpythonAotSubset) => {
            if cli.update_floor {
                bail!("AoT floor updates are ratcheted in `pon-conformance/src/aot.rs`")
            }

            let scoreboard = aot::run_aot_suite(&root, &cli.modules)?;
            println!("{}", scoreboard.to_json());

            if cli.check_floor {
                let report = aot::check_floor(&scoreboard);
                if !report.is_ok() {
                    bail!("{}", report.message())
                }
            }

            if scoreboard.has_status(Status::Fail) || scoreboard.has_status(Status::SemanticsDivergent) {
                bail!("one or more AoT subset scripts failed differential verification")
            }
        }
        (Mode::Jit, SuiteName::FtStress) => {
            if cli.check_floor || cli.update_floor {
                bail!("floor checks are only defined for `--suite cpython`")
            }

            let scoreboard = suite::run_ft_stress_suite(&root, &cli.modules)?;
            println!("{}", scoreboard.to_json());
            if scoreboard.has_status(Status::Fail) {
                bail!("one or more FT stress scripts failed")
            }
        }
        (Mode::Jit, SuiteName::CpythonAotSubset) => {
            bail!("`--suite cpython-aot-subset` requires `--mode aot`")
        }
        (Mode::Aot, SuiteName::Slice | SuiteName::Cpython | SuiteName::FtStress) => {
            bail!("`--mode aot` requires `--suite cpython-aot-subset`")
        }
    }

    Ok(())
}

fn parse_cli(args: impl IntoIterator<Item = String>) -> Result<Cli> {
    let mut suite = SuiteName::Slice;
    let mut mode = Mode::Jit;
    let mut check_floor = false;
    let mut update_floor = false;
    let mut modules = Vec::new();
    let mut bench = false;
    let mut args = args.into_iter().peekable();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--suite" => {
                let value = args.next().ok_or_else(usage)?;
                suite = SuiteName::parse(&value)?;
            }
            "--mode" => {
                let value = args.next().ok_or_else(usage)?;
                mode = Mode::parse(&value)?;
            }
            "--check-floor" => check_floor = true,
            "--update-floor" => update_floor = true,
            "--bench" => bench = true,
            "--modules" => {
                let before = modules.len();
                while let Some(next) = args.peek() {
                    if next.starts_with("--") {
                        break;
                    }
                    modules.push(PathBuf::from(args.next().expect("peeked argument exists")));
                }
                if modules.len() == before {
                    bail!("`--modules` requires at least one path")
                }
            }
            _ => return Err(usage()),
        }
    }

    Ok(Cli {
        suite,
        mode,
        check_floor,
        update_floor,
        bench,
        modules,
    })
}

fn usage() -> anyhow::Error {
    anyhow::anyhow!(
        "usage: pon-conformance [--bench] [--mode jit|aot] [--suite slice|cpython|cpython-aot-subset|ft-stress] [--check-floor] [--update-floor] [--modules <paths...>]"
    )
}

const BENCH_KERNELS: &[&str] = &["int_loop.py", "fib.py", "nbody.py"];
const BENCH_WARMUP_REPS: usize = 1;
const BENCH_TIMED_REPS: usize = 5;
const INT_LOOP_MIN_SPEEDUP: f64 = 5.0;
const TIER0_ONLY_ENV: &str = "PON_TIER0_ONLY";

#[derive(Clone, Debug)]
struct BenchMeasurement {
    output: suite::RunResult,
    best: Duration,
}

fn run_bench_gate(root: &Path, requested_modules: &[PathBuf]) -> Result<()> {
    let scripts = bench_scripts(root, requested_modules)?;
    let runner_binary = ensure_bench_runner(root)?;
    let mut failures = Vec::new();

    println!(
        "Phase D bench gate: {} kernel(s), tier-0-only via {TIER0_ONLY_ENV}=1, tier-up enabled by default",
        scripts.len()
    );

    for script in scripts {
        let label = suite::display_path(root, &script);
        let tier0 = measure_bench_variant(root, &runner_binary, &script, true)
            .with_context(|| format!("failed to measure tier-0-only benchmark `{label}`"))?;
        let tier1 = measure_bench_variant(root, &runner_binary, &script, false)
            .with_context(|| format!("failed to measure tier-up benchmark `{label}`"))?;

        if tier0.output != tier1.output {
            failures.push(format!(
                "`{label}` changed observable output between tier-0-only and tier-up execution\n{}",
                bench_mismatch_report(&tier0.output, &tier1.output)
            ));
            continue;
        }

        let speedup = speedup(tier0.best, tier1.best);
        println!(
            "bench {label}: tier0={:.6}s tier1={:.6}s speedup={speedup:.2}x",
            tier0.best.as_secs_f64(),
            tier1.best.as_secs_f64()
        );

        if script.file_stem().and_then(|stem| stem.to_str()) == Some("int_loop") && speedup < INT_LOOP_MIN_SPEEDUP {
            failures.push(format!(
                "`{label}` speedup {speedup:.2}x is below the Phase D floor {INT_LOOP_MIN_SPEEDUP:.2}x"
            ));
        }
    }

    if !failures.is_empty() {
        bail!("Phase D bench gate failed:\n{}", failures.join("\n\n"))
    }

    println!("Phase D bench gate PASS");
    Ok(())
}

fn bench_scripts(root: &Path, requested_modules: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let bench_dir = root.join("pon-conformance").join("benches");
    if requested_modules.is_empty() {
        BENCH_KERNELS
            .iter()
            .map(|name| {
                let path = bench_dir.join(name);
                if path.is_file() {
                    Ok(path)
                } else {
                    bail!("Phase D benchmark kernel `{}` is missing", suite::normalize_path(&path))
                }
            })
            .collect()
    } else {
        requested_modules
            .iter()
            .map(|module| resolve_bench_path(root, &bench_dir, module))
            .map(|path| {
                if path.is_file() {
                    Ok(path)
                } else {
                    bail!("Phase D benchmark kernel `{}` is missing", suite::normalize_path(&path))
                }
            })
            .collect()
    }
}

fn resolve_bench_path(root: &Path, bench_dir: &Path, module: &Path) -> PathBuf {
    if module.is_absolute() {
        return module.to_path_buf();
    }

    let workspace_path = root.join(module);
    if workspace_path.is_file() {
        workspace_path
    } else {
        bench_dir.join(module)
    }
}

fn ensure_bench_runner(root: &Path) -> Result<PathBuf> {
    let runner_dir = suite::target_dir(root)?.join("pon-conformance-bench-runner");
    let source_dir = runner_dir.join("src");
    fs::create_dir_all(&source_dir)
        .with_context(|| format!("failed to create benchmark runner directory `{}`", source_dir.display()))?;

    let manifest_path = runner_dir.join("Cargo.toml");
    let source_path = source_dir.join("main.rs");
    write_if_changed(&manifest_path, &bench_runner_manifest(root))?;
    write_if_changed(&source_path, bench_runner_source())?;

    let output = Command::new("cargo")
        .arg("build")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(&manifest_path)
        .arg("--target-dir")
        .arg(runner_dir.join("target"))
        .current_dir(root)
        .output()
        .context("failed to spawn cargo build for Phase D benchmark runner")?;
    if !output.status.success() {
        bail!(
            "failed to build Phase D benchmark runner\nstdout={:?}\nstderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let binary = runner_dir
        .join("target")
        .join("debug")
        .join(format!("pon-bench-runner{}", std::env::consts::EXE_SUFFIX));
    if !binary.is_file() {
        bail!("benchmark runner build succeeded but `{}` was not created", binary.display());
    }
    Ok(binary)
}

fn write_if_changed(path: &Path, contents: &str) -> Result<()> {
    if fs::read_to_string(path).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    fs::write(path, contents).with_context(|| format!("failed to write `{}`", path.display()))
}

fn bench_runner_manifest(root: &Path) -> String {
    format!(
        r#"[package]
name = "pon-bench-runner"
version = "0.1.0"
edition = "2024"

[workspace]

[dependencies]
anyhow = "1"
pon-ir = {{ path = "{}" }}
pon-jit = {{ path = "{}" }}
pon-runtime = {{ path = "{}" }}
"#,
        toml_path(&root.join("pon-ir")),
        toml_path(&root.join("pon-jit")),
        toml_path(&root.join("pon-runtime"))
    )
}

fn toml_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"")
}

fn bench_runner_source() -> &'static str {
    r#"use std::env;
use std::fs;
use std::io::{self, Write};
use std::process::ExitCode;
use std::ptr;

use anyhow::{Context, Result, bail};
use pon_ir::lower_source;
use pon_jit::JitEngine;
use pon_runtime::abi::pon_tierup_set_hook;

const TIER0_ONLY_ENV: &str = "PON_TIER0_ONLY";

fn main() -> ExitCode {
    match run_cli() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("pon-bench-runner: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_cli() -> Result<()> {
    let mut args = env::args();
    let program = args.next().unwrap_or_else(|| "pon-bench-runner".to_owned());
    let file = args.next().with_context(|| format!("usage: {program} <file>"))?;
    if let Some(extra) = args.next() {
        bail!("unexpected argument `{extra}`\nusage: {program} <file>");
    }

    let source = fs::read_to_string(&file).with_context(|| format!("failed to read UTF-8 source `{}`", file))?;
    let module = lower_source(&source).context("failed to parse/lower source")?;
    let mut engine = JitEngine::new();
    if env::var_os(TIER0_ONLY_ENV).is_some() {
        unsafe { pon_tierup_set_hook(ptr::null_mut()) };
    }
    engine.run(&module).context("JIT execution failed")?;
    io::stdout().flush().context("failed to flush stdout")
}
"#
}

fn measure_bench_variant(root: &Path, runner_binary: &Path, script: &Path, tier0_only: bool) -> Result<BenchMeasurement> {
    for _ in 0..BENCH_WARMUP_REPS {
        let output = run_pon_bench(root, runner_binary, script, tier0_only)?;
        ensure_bench_success(script, tier0_only, &output)?;
    }

    let mut best = None;
    let mut observed = None;
    for _ in 0..BENCH_TIMED_REPS {
        let start = Instant::now();
        let output = run_pon_bench(root, runner_binary, script, tier0_only)?;
        let elapsed = start.elapsed();
        ensure_bench_success(script, tier0_only, &output)?;

        if best.is_none_or(|current| elapsed < current) {
            best = Some(elapsed);
            observed = Some(output);
        }
    }

    Ok(BenchMeasurement {
        output: observed.context("benchmark did not record a timed run")?,
        best: best.context("benchmark did not record a duration")?,
    })
}

fn run_pon_bench(root: &Path, runner_binary: &Path, script: &Path, tier0_only: bool) -> Result<suite::RunResult> {
    let mut command = Command::new(runner_binary);
    command.arg(script).current_dir(root);
    if tier0_only {
        command.env(TIER0_ONLY_ENV, "1");
    } else {
        command.env_remove(TIER0_ONLY_ENV);
    }
    suite::run_command(&mut command)
}

fn ensure_bench_success(script: &Path, tier0_only: bool, output: &suite::RunResult) -> Result<()> {
    if output.exit == 0 {
        return Ok(());
    }

    let variant = if tier0_only { "tier-0-only" } else { "tier-up" };
    bail!(
        "{variant} benchmark `{}` exited with {}\nstdout={:?}\nstderr={:?}",
        suite::normalize_path(script),
        output.exit,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn speedup(tier0: Duration, tier1: Duration) -> f64 {
    let tier1_secs = tier1.as_secs_f64();
    if tier1_secs == 0.0 {
        f64::INFINITY
    } else {
        tier0.as_secs_f64() / tier1_secs
    }
}

fn bench_mismatch_report(tier0: &suite::RunResult, tier1: &suite::RunResult) -> String {
    format!(
        "tier0: exit={} stdout={:?} stderr={:?}\ntier1: exit={} stdout={:?} stderr={:?}",
        tier0.exit,
        String::from_utf8_lossy(&tier0.stdout),
        String::from_utf8_lossy(&tier0.stderr),
        tier1.exit,
        String::from_utf8_lossy(&tier1.stdout),
        String::from_utf8_lossy(&tier1.stderr)
    )
}
