//!Differential conformance runner and Phase-B/Phase-C ratchet gates.

mod aot;
mod full;
mod fuzz;
mod ledger;
mod ratchet;
mod scoreboard;
mod suite;

use std::{
	env, fs,
	path::{Path, PathBuf},
	process::{Command, ExitCode},
	time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

use crate::{scoreboard::Status, suite::SuiteName};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Cli {
	suite:        SuiteName,
	mode:         Mode,
	check_floor:  bool,
	update_floor: bool,
	diff_floor:   bool,
	modules:      Vec<String>,
	timeout:      Option<u64>,
	shard:        Option<(u32, u32)>,
	jobs:         Option<usize>,
	seed:         Option<u64>,
	count:        Option<usize>,
	bench:        bool,
	bench_python: bool,
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
		},
	}
}

fn run_cli() -> Result<()> {
	let cli = parse_cli(env::args().skip(1))?;
	let root = suite::workspace_root();

	// Pinned validation (J0.7 §2): the full-suite flags are CLI errors with
	// most other suites; floor ops on partial full/parity runs are CLI errors.
	if matches!(cli.suite, SuiteName::CpythonFull | SuiteName::AotParity)
		&& (cli.check_floor || cli.update_floor || cli.diff_floor)
		&& (cli.shard.is_some() || !cli.modules.is_empty())
	{
		bail!("floor operations require an unsharded, unfiltered run")
	}
	if cli.suite != SuiteName::CpythonFull {
		if cli.timeout.is_some() {
			bail!("`--timeout` is only defined for `--suite cpython-full`")
		}
		if cli.shard.is_some() {
			bail!("`--shard` is only defined for `--suite cpython-full`")
		}
	}
	if !matches!(cli.suite, SuiteName::CpythonFull | SuiteName::Fuzz) && cli.jobs.is_some() {
		bail!("`--jobs` is only defined for `--suite cpython-full` or `--suite fuzz`")
	}
	if cli.seed.is_some() && cli.suite != SuiteName::Fuzz {
		bail!("`--seed` is only defined for `--suite fuzz`")
	}
	if cli.count.is_some() && cli.suite != SuiteName::Fuzz {
		bail!("`--count` is only defined for `--suite fuzz`")
	}
	if cli.suite == SuiteName::Fuzz {
		if cli.check_floor || cli.update_floor || cli.diff_floor {
			bail!("floor operations are not defined for `--suite fuzz`")
		}
		if !cli.modules.is_empty() {
			bail!("`--modules` is not defined for `--suite fuzz`")
		}
	}
	if cli.diff_floor && !matches!(cli.suite, SuiteName::CpythonFull | SuiteName::AotParity) {
		bail!("`--diff-floor` is only defined for `--suite cpython-full` or `--suite aot-parity`")
	}

	// Path-taking suites convert selectors to workspace/vendored paths verbatim.
	let module_paths = cli.modules.iter().map(PathBuf::from).collect::<Vec<_>>();

	if cli.bench && cli.bench_python {
		bail!("`--bench` and `--bench-python` are mutually exclusive")
	}
	if cli.bench || cli.bench_python {
		if cli.mode != Mode::Jit {
			bail!("benchmark modes use the JIT tier-up path; omit `--mode` or pass `--mode jit`")
		}
		if cli.suite != SuiteName::Slice {
			bail!("benchmark modes are independent of `--suite`; omit `--suite`")
		}
		if cli.check_floor || cli.update_floor {
			bail!("floor checks are not defined for benchmark modes")
		}

		if cli.bench_python {
			run_python_bench_gate(&root, &module_paths)?;
		} else {
			run_bench_gate(&root, &module_paths)?;
		}
		return Ok(());
	}

	match (cli.mode, cli.suite) {
		(Mode::Jit, SuiteName::Slice) => {
			if cli.check_floor || cli.update_floor {
				bail!("floor checks are only defined for `--suite cpython`")
			}

			let scoreboard = suite::run_slice_suite(&root, &module_paths)?;
			println!("{}", scoreboard.to_json());
			if scoreboard.has_status(Status::Fail) {
				bail!("one or more Phase-A slice scripts failed")
			}
		},
		(Mode::Jit, SuiteName::Cpython) => {
			let floor = ratchet::Floor::read_or_default(&root)?;
			let scoreboard = suite::run_cpython_suite(&root, &module_paths)?;
			println!("{}", scoreboard.to_json());

			if cli.check_floor {
				let report = ratchet::check_floor(&floor, &scoreboard);
				if !report.is_ok() {
					bail!("{}", report.message())
				}
			}

			if cli.update_floor {
				let cpython_tag = scoreboard
					.cpython_tag
					.as_deref()
					.unwrap_or(&floor.cpython_tag);
				ratchet::write_floor_from_scoreboard(&root, cpython_tag, &scoreboard)?;
			}

			if scoreboard.has_status(Status::SemanticsDivergent) {
				bail!("one or more CPython modules produced an unclassified semantic divergence")
			}
		},
		(Mode::Aot, SuiteName::CpythonAotSubset) => {
			if cli.update_floor {
				bail!("AoT floor updates are ratcheted in `pon-conformance/src/aot.rs`")
			}

			let scoreboard = aot::run_aot_suite(&root, &module_paths)?;
			println!("{}", scoreboard.to_json());

			if cli.check_floor {
				let report = aot::check_floor(&scoreboard);
				if !report.is_ok() {
					bail!("{}", report.message())
				}
			}

			if scoreboard.has_status(Status::Fail) || scoreboard.has_status(Status::SemanticsDivergent)
			{
				bail!("one or more AoT subset scripts failed differential verification")
			}
		},
		(_, SuiteName::AotParity) => {
			let floor = ratchet::Floor::read_or_default_at(&root, aot::AOT_PARITY_FLOOR_FILE)?;
			let report = aot::run_aot_parity_suite(&root, &module_paths)?;
			aot::write_aot_parity_results(&root, &report)?;
			println!("{}", report.to_json());

			if cli.diff_floor {
				eprint!("{}", ratchet::diff_floor(&floor, report.scoreboard()));
			}

			if cli.check_floor {
				let floor_check = ratchet::check_floor(&floor, report.scoreboard());
				if !floor_check.is_ok() {
					bail!("{}", floor_check.message())
				}
			}

			if cli.update_floor {
				let cpython_tag = report
					.scoreboard()
					.cpython_tag
					.as_deref()
					.unwrap_or(&floor.cpython_tag);
				ratchet::write_floor_from_scoreboard_at(
					&root,
					aot::AOT_PARITY_FLOOR_FILE,
					cpython_tag,
					report.scoreboard(),
				)?;
			}
			// Meter contract: aot-fail/aot-error records are bugs to report, not
			// process-failing conditions. Floor checks are the gate.
		},
		(Mode::Jit, SuiteName::FtStress) => {
			if cli.check_floor || cli.update_floor {
				bail!("floor checks are only defined for `--suite cpython`")
			}

			let scoreboard = suite::run_ft_stress_suite(&root, &module_paths)?;
			println!("{}", scoreboard.to_json());
			if scoreboard.has_status(Status::Fail) {
				bail!("one or more FT stress scripts failed")
			}
		},
		(Mode::Jit, SuiteName::CpythonFull) => {
			let opts = full::FullSuiteOptions {
				modules: cli.modules.clone(),
				timeout: Duration::from_secs(cli.timeout.unwrap_or(full::DEFAULT_TIMEOUT_SECS)),
				shard:   cli.shard,
				jobs:    cli.jobs.unwrap_or_else(default_jobs),
			};
			let floor = ratchet::Floor::read_or_default_at(&root, ratchet::FULL_FLOOR_FILE)?;
			let scoreboard = full::run_full_suite(&root, &opts)?;
			println!("{}", scoreboard.to_json());

			if cli.diff_floor {
				eprint!("{}", ratchet::diff_floor(&floor, &scoreboard));
			}

			if cli.check_floor {
				let report = ratchet::check_floor(&floor, &scoreboard);
				if !report.is_ok() {
					bail!("{}", report.message())
				}
			}

			if cli.update_floor {
				let cpython_tag = scoreboard
					.cpython_tag
					.as_deref()
					.unwrap_or(&floor.cpython_tag);
				ratchet::write_floor_from_scoreboard_at(
					&root,
					ratchet::FULL_FLOOR_FILE,
					cpython_tag,
					&scoreboard,
				)?;
			}
			// Pinned exit contract (§10): fail/unsupported records do NOT
			// affect the exit code for this suite.
		},
		(Mode::Jit, SuiteName::Fuzz) => {
			let opts = fuzz::FuzzOptions {
				seed:  cli.seed.unwrap_or(0),
				count: cli.count.unwrap_or(100),
				jobs:  cli.jobs.unwrap_or_else(default_jobs),
			};
			let summary = fuzz::run_fuzz_suite(&root, &opts)?;
			println!("{}", summary.summary_line(&root));
		},
		(Mode::Jit, SuiteName::CpythonAotSubset) => {
			bail!("`--suite cpython-aot-subset` requires `--mode aot`")
		},
		(Mode::Aot, SuiteName::CpythonFull) => {
			bail!("`--suite cpython-full` requires `--mode jit`")
		},
		(Mode::Aot, SuiteName::Fuzz) => {
			bail!("`--suite fuzz` requires `--mode jit`")
		},
		(Mode::Aot, SuiteName::Slice | SuiteName::Cpython | SuiteName::FtStress) => {
			bail!("`--mode aot` requires `--suite cpython-aot-subset` or `--suite aot-parity`")
		},
	}

	Ok(())
}

fn parse_cli(args: impl IntoIterator<Item = String>) -> Result<Cli> {
	let mut suite = SuiteName::Slice;
	let mut mode = Mode::Jit;
	let mut check_floor = false;
	let mut update_floor = false;
	let mut diff_floor = false;
	let mut modules = Vec::new();
	let mut timeout = None;
	let mut shard = None;
	let mut jobs = None;
	let mut seed = None;
	let mut count = None;
	let mut bench = false;
	let mut bench_python = false;
	let mut args = args.into_iter().peekable();

	while let Some(arg) = args.next() {
		match arg.as_str() {
			"--suite" => {
				let value = args.next().ok_or_else(usage)?;
				suite = SuiteName::parse(&value)?;
			},
			"--mode" => {
				let value = args.next().ok_or_else(usage)?;
				mode = Mode::parse(&value)?;
			},
			"--check-floor" => check_floor = true,
			"--update-floor" => update_floor = true,
			"--diff-floor" => diff_floor = true,
			"--bench" => bench = true,
			"--bench-python" => bench_python = true,
			"--timeout" => {
				let value = args.next().ok_or_else(usage)?;
				let secs = value
					.parse::<u64>()
					.with_context(|| format!("invalid `--timeout` value `{value}`"))?;
				if secs == 0 {
					bail!("`--timeout` must be at least 1 second")
				}
				timeout = Some(secs);
			},
			"--shard" => {
				let value = args.next().ok_or_else(usage)?;
				shard = Some(parse_shard(&value)?);
			},
			"--jobs" => {
				let value = args.next().ok_or_else(usage)?;
				let count = value
					.parse::<usize>()
					.with_context(|| format!("invalid `--jobs` value `{value}`"))?;
				if count == 0 {
					bail!("`--jobs` must be at least 1")
				}
				jobs = Some(count);
			},
			"--seed" => {
				let value = args.next().ok_or_else(usage)?;
				seed = Some(
					value
						.parse::<u64>()
						.with_context(|| format!("invalid `--seed` value `{value}`"))?,
				);
			},
			"--count" => {
				let value = args.next().ok_or_else(usage)?;
				let parsed = value
					.parse::<usize>()
					.with_context(|| format!("invalid `--count` value `{value}`"))?;
				if parsed == 0 {
					bail!("`--count` must be at least 1")
				}
				count = Some(parsed);
			},
			"--modules" => {
				let before = modules.len();
				while let Some(next) = args.peek() {
					if next.starts_with("--") {
						break;
					}
					modules.push(args.next().expect("peeked argument exists"));
				}
				if modules.len() == before {
					bail!("`--modules` requires at least one path")
				}
			},
			_ => return Err(usage()),
		}
	}

	Ok(Cli {
		suite,
		mode,
		check_floor,
		update_floor,
		diff_floor,
		bench,
		bench_python,
		modules,
		timeout,
		shard,
		jobs,
		seed,
		count,
	})
}

/// Parses the pinned `--shard <i>/<N>` grammar: two integers joined by `/`,
/// `0 <= i < N`, `N >= 1`.
fn parse_shard(value: &str) -> Result<(u32, u32)> {
	let Some((index, count)) = value.split_once('/') else {
		bail!("invalid `--shard` value `{value}` (expected `<i>/<N>`)")
	};
	let index = index
		.parse::<u32>()
		.with_context(|| format!("invalid `--shard` index in `{value}`"))?;
	let count = count
		.parse::<u32>()
		.with_context(|| format!("invalid `--shard` count in `{value}`"))?;
	if count == 0 {
		bail!("`--shard` count must be at least 1")
	}
	if index >= count {
		bail!("`--shard` index {index} is out of range for {count} shard(s)")
	}
	Ok((index, count))
}

/// Default `--jobs`: `std::thread::available_parallelism()` (pin §2).
fn default_jobs() -> usize {
	std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

fn usage() -> anyhow::Error {
	anyhow::anyhow!(
		"usage: pon-conformance [--bench|--bench-python] [--mode jit|aot] [--suite \
		 slice|cpython|cpython-aot-subset|aot-parity|cpython-full|ft-stress|fuzz] [--check-floor] \
		 [--update-floor] [--diff-floor] [--modules <selectors...>] [--timeout <secs>] [--shard \
		 <i>/<N>] [--jobs <J>] [--seed N] [--count K]"
	)
}

const BENCH_KERNELS: &[&str] =
	&["int_loop.py", "fib.py", "nbody.py", "comprehension.py", "generator.py"];
const BENCH_SPEEDUP_RATCHETS: &[(&str, f64)] = &[
	("int_loop.py", 5.0),
	("fib.py", 1.0),
	("nbody.py", 1.0),
	("comprehension.py", 1.0),
	("generator.py", 1.0),
];
const BENCH_SPEEDUP_NOISE_ALLOWANCE: f64 = 0.05;
const BENCH_WARMUP_REPS: usize = 1;
const BENCH_TIMED_REPS: usize = 5;
const TIER0_ONLY_ENV: &str = "PON_TIER0_ONLY";

#[derive(Clone, Debug)]
struct BenchMeasurement {
	output: suite::RunResult,
	best:   Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BenchRunnerProfile {
	Debug,
	Release,
}

impl BenchRunnerProfile {
	fn target_dir(self) -> &'static str {
		match self {
			Self::Debug => "debug",
			Self::Release => "release",
		}
	}
}

fn run_bench_gate(root: &Path, requested_modules: &[PathBuf]) -> Result<()> {
	let scripts = bench_scripts(root, requested_modules)?;
	let runner_binary = ensure_bench_runner(root, BenchRunnerProfile::Debug)?;
	let mut failures = Vec::new();

	println!(
		"Phase D bench gate: {} kernel(s), tier-0-only via {TIER0_ONLY_ENV}=1, tier-up enabled by \
		 default",
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

		let floor = bench_speedup_floor(&script);
		if speedup + BENCH_SPEEDUP_NOISE_ALLOWANCE < floor {
			failures
				.push(format!("`{label}` speedup {speedup:.2}x is below the bench floor {floor:.2}x"));
		}
	}

	if !failures.is_empty() {
		bail!("Phase D bench gate failed:\n{}", failures.join("\n\n"))
	}

	println!("Phase D bench gate PASS");
	Ok(())
}

fn run_python_bench_gate(root: &Path, requested_modules: &[PathBuf]) -> Result<()> {
	let scripts = bench_scripts(root, requested_modules)?;
	if !suite::python314_available() {
		bail!("python3.14 reference interpreter is not available")
	}
	let runner_binary = ensure_bench_runner(root, BenchRunnerProfile::Release)?;
	let mut failures = Vec::new();

	println!("Python comparison bench: {} kernel(s), pon tier-up vs python3.14", scripts.len());

	for script in scripts {
		let label = suite::display_path(root, &script);
		let pon = measure_bench_variant(root, &runner_binary, &script, false)
			.with_context(|| format!("failed to measure pon benchmark `{label}`"))?;
		let python = measure_python_bench(root, &script)
			.with_context(|| format!("failed to measure python3.14 benchmark `{label}`"))?;

		if pon.output != python.output {
			failures.push(format!(
				"`{label}` changed observable output between pon and python3.14 execution\n{}",
				python_bench_mismatch_report(&pon.output, &python.output)
			));
			continue;
		}

		let speedup = speedup(python.best, pon.best);
		println!(
			"bench {label}: python3.14={:.6}s pon={:.6}s pon_vs_python={speedup:.2}x",
			python.best.as_secs_f64(),
			pon.best.as_secs_f64()
		);
	}

	if !failures.is_empty() {
		bail!("Python comparison bench failed:\n{}", failures.join("\n\n"))
	}

	println!("Python comparison bench PASS");
	Ok(())
}

fn bench_speedup_floor(script: &Path) -> f64 {
	let Some(file_name) = script.file_name().and_then(|name| name.to_str()) else {
		return 1.0;
	};
	BENCH_SPEEDUP_RATCHETS
		.iter()
		.find_map(|(name, floor)| (*name == file_name).then_some(*floor))
		.unwrap_or(1.0)
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

fn ensure_bench_runner(root: &Path, profile: BenchRunnerProfile) -> Result<PathBuf> {
	let runner_dir = suite::target_dir(root)?.join("pon-conformance-bench-runner");
	let source_dir = runner_dir.join("src");
	fs::create_dir_all(&source_dir).with_context(|| {
		format!("failed to create benchmark runner directory `{}`", source_dir.display())
	})?;

	let manifest_path = runner_dir.join("Cargo.toml");
	let source_path = source_dir.join("main.rs");
	write_if_changed(&manifest_path, &bench_runner_manifest(root))?;
	write_if_changed(&source_path, bench_runner_source())?;

	let mut command = Command::new("cargo");
	command.arg("build").arg("--quiet");
	if profile == BenchRunnerProfile::Release {
		command.arg("--release");
	}
	let output = command
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
		.join(profile.target_dir())
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
	path
		.to_string_lossy()
		.replace('\\', "\\\\")
		.replace('"', "\\\"")
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

fn measure_bench_variant(
	root: &Path,
	runner_binary: &Path,
	script: &Path,
	tier0_only: bool,
) -> Result<BenchMeasurement> {
	let variant = if tier0_only { "tier-0-only" } else { "tier-up" };
	measure_bench(script, variant, || run_pon_bench(root, runner_binary, script, tier0_only))
}

fn measure_python_bench(root: &Path, script: &Path) -> Result<BenchMeasurement> {
	measure_bench(script, "python3.14", || suite::run_python314(root, script))
}

fn measure_bench(
	script: &Path,
	variant: &str,
	mut run_once: impl FnMut() -> Result<suite::RunResult>,
) -> Result<BenchMeasurement> {
	for _ in 0..BENCH_WARMUP_REPS {
		let output = run_once()?;
		ensure_bench_success(script, variant, &output)?;
	}

	let mut best = None;
	let mut observed = None;
	for _ in 0..BENCH_TIMED_REPS {
		let start = Instant::now();
		let output = run_once()?;
		let elapsed = start.elapsed();
		ensure_bench_success(script, variant, &output)?;

		if best.is_none_or(|current| elapsed < current) {
			best = Some(elapsed);
			observed = Some(output);
		}
	}

	Ok(BenchMeasurement {
		output: observed.context("benchmark did not record a timed run")?,
		best:   best.context("benchmark did not record a duration")?,
	})
}

fn run_pon_bench(
	root: &Path,
	runner_binary: &Path,
	script: &Path,
	tier0_only: bool,
) -> Result<suite::RunResult> {
	let mut command = Command::new(runner_binary);
	command.arg(script).current_dir(root);
	if tier0_only {
		command.env(TIER0_ONLY_ENV, "1");
	} else {
		command.env_remove(TIER0_ONLY_ENV);
	}
	suite::run_command(&mut command)
}

fn ensure_bench_success(script: &Path, variant: &str, output: &suite::RunResult) -> Result<()> {
	if output.exit == 0 {
		return Ok(());
	}

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

fn python_bench_mismatch_report(pon: &suite::RunResult, python: &suite::RunResult) -> String {
	format!(
		"pon: exit={} stdout={:?} stderr={:?}\npython3.14: exit={} stdout={:?} stderr={:?}",
		pon.exit,
		String::from_utf8_lossy(&pon.stdout),
		String::from_utf8_lossy(&pon.stderr),
		python.exit,
		String::from_utf8_lossy(&python.stdout),
		String::from_utf8_lossy(&python.stderr)
	)
}
