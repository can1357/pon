//! `--suite cpython-full`: runs the vendored CPython 3.14.0 test suite under
//! both `python3.14` (oracle) and pon, diffing per-test outcome vectors.
//!
//! Contract: `plans/pon-pin-J07-cpython-full-runner.md` (frozen) — discovery
//! (§4), driver + subprocess isolation (§5), classification (§6), scoreboard
//! shape (§8), determinism (§9), exit codes (§10).

use std::{
	collections::{BTreeMap, BTreeSet},
	env, fs,
	io::Read,
	path::{Path, PathBuf},
	process::{Command, Stdio},
	sync::{
		Arc, Mutex,
		atomic::{AtomicUsize, Ordering},
	},
	thread,
	time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

use crate::{
	ledger::{self, Ledger},
	scoreboard::{Scoreboard, Status},
	suite,
};

/// Default per-test-file wall-clock timeout in seconds (pin §2), applied
/// independently to the oracle subprocess and the pon subprocess of each unit.
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Every record `detail` is truncated to this many bytes on a UTF-8 boundary
/// with `…` appended when cut (pin §6.2).
const MAX_DETAIL_BYTES: usize = 2000;

/// Pinned driver source (§5.1). The only substitution is `@MODULE@` → the
/// unit's dotted name; the same bytes run on both sides.
const DRIVER_TEMPLATE: &str = r#"# pon-conformance cpython-full driver (generated; do not edit).
import sys
import unittest

MODULE = "@MODULE@"


class _Result(unittest.TestResult):
    def __init__(self):
        super().__init__()
        self.outcomes = {}

    def addSuccess(self, test):
        super().addSuccess(test)
        self.outcomes[test.id()] = "ok"

    def addFailure(self, test, err):
        super().addFailure(test, err)
        self.outcomes[test.id()] = "FAIL"

    def addError(self, test, err):
        super().addError(test, err)
        self.outcomes[test.id()] = "ERROR"

    def addSkip(self, test, reason):
        super().addSkip(test, reason)
        self.outcomes[test.id()] = "skip"

    def addExpectedFailure(self, test, err):
        super().addExpectedFailure(test, err)
        self.outcomes[test.id()] = "xfail"

    def addUnexpectedSuccess(self, test):
        super().addUnexpectedSuccess(test)
        self.outcomes[test.id()] = "xpass"

    def addSubTest(self, test, subtest, err):
        super().addSubTest(test, subtest, err)
        if err is not None:
            if issubclass(err[0], test.failureException):
                self.outcomes[test.id()] = "FAIL"
            else:
                self.outcomes[test.id()] = "ERROR"


def main():
    module = __import__(MODULE, fromlist=["*"])
    suite = unittest.defaultTestLoader.loadTestsFromModule(module)
    result = _Result()
    suite.run(result)
    sys.stdout.flush()
    sys.stdout.write("\nPONTEST BEGIN %s\n" % MODULE)
    for test_id in sorted(result.outcomes):
        sys.stdout.write("PONTEST %s %s\n" % (result.outcomes[test_id], test_id))
    sys.stdout.write("PONTEST END %d\n" % len(result.outcomes))
    sys.stdout.flush()


main()
"#;

const OUTCOME_STRINGS: &[&str] = &["ok", "FAIL", "ERROR", "skip", "xfail", "xpass"];

/// Sorted per-test outcome vector: test id → outcome string (§5.4).
type Outcomes = BTreeMap<String, String>;

/// Options for one `--suite cpython-full` invocation (pin §3).
pub struct FullSuiteOptions {
	/// Raw selectors from `--modules` (§4.3); empty = every discovered unit.
	pub modules: Vec<String>,
	/// Per-subprocess wall-clock timeout (§2).
	pub timeout: Duration,
	/// `--shard <i>/<N>` (§4.4).
	pub shard:   Option<(u32, u32)>,
	/// Intra-process worker threads; must not affect output (§9).
	pub jobs:    usize,
}

/// One discovered test unit (§4.2).
#[derive(Clone, Debug, Eq, PartialEq)]
struct Unit {
	/// Dotted name: `test.test_json.test_decode`.
	dotted: String,
	/// Match key: dotted name minus the leading `test.`.
	key:    String,
	/// Final path component minus `.py`.
	stem:   String,
}

struct ExecContext<'a> {
	lib_dir:          PathBuf,
	scratch_base:     PathBuf,
	scratch_base_str: String,
	pon_binary:       Option<PathBuf>,
	python_available: bool,
	timeout:          Duration,
	ledger:           &'a Ledger,
}

/// Runs the full suite per the frozen pin and returns the scoreboard
/// (records sorted by dotted name; one per unit in the selected shard).
pub fn run_full_suite(root: &Path, opts: &FullSuiteOptions) -> Result<Scoreboard> {
	if !cfg!(unix) {
		bail!("cpython-full requires a Unix host");
	}

	let ledger = ledger::load_ledger(root)?;
	let lib_dir = root.join(suite::CPYTHON_VENDOR_DIR).join("Lib");
	let test_root = lib_dir.join("test");
	if !test_root.is_dir() {
		bail!(
			"cpython-full discovery root {} is missing; vendor the CPython v3.14.0 Lib/test tree",
			test_root.display()
		);
	}

	let units = discover_units(&test_root)?;
	if opts.shard.is_none() && opts.modules.is_empty() {
		check_ledger_staleness(&ledger, &units)?;
	}

	let mut selected = select_units(&units, &opts.modules)?;
	if let Some((index, count)) = opts.shard {
		selected = shard_units(selected, index, count);
	}

	let python_available = suite::python314_available();
	let needs_pon = python_available
		&& selected
			.iter()
			.any(|unit| ledger.exclusion_for(&unit.key, &unit.stem).is_none());
	let pon_binary = if needs_pon {
		Some(suite::ensure_pon(root)?)
	} else {
		None
	};

	let scratch_base = env::temp_dir().join(format!("pon-conf-full-{}", std::process::id()));
	fs::create_dir_all(&scratch_base).with_context(|| {
		format!("failed to create scratch directory `{}`", scratch_base.display())
	})?;
	let ctx = ExecContext {
		scratch_base_str: scratch_base.to_string_lossy().into_owned(),
		lib_dir,
		scratch_base,
		pon_binary,
		python_available,
		timeout: opts.timeout,
		ledger: &ledger,
	};

	let jobs = opts.jobs.clamp(1, selected.len().max(1));
	let next = AtomicUsize::new(0);
	let slots: Vec<Mutex<Option<Result<(Status, Option<String>)>>>> =
		selected.iter().map(|_| Mutex::new(None)).collect();

	thread::scope(|scope| {
		for _ in 0..jobs {
			scope.spawn(|| {
				loop {
					let index = next.fetch_add(1, Ordering::Relaxed);
					let Some(unit) = selected.get(index) else {
						break;
					};
					let outcome = run_unit(&ctx, unit);
					*slots[index]
						.lock()
						.unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome);
				}
			});
		}
	});

	let _ = fs::remove_dir_all(&ctx.scratch_base);

	let mut scoreboard = Scoreboard::new("cpython-full", Some(suite::cpython_revision(root)));
	for (unit, slot) in selected.iter().zip(slots) {
		let outcome = slot
			.into_inner()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.with_context(|| format!("worker recorded no result for `{}`", unit.dotted))?;
		let (status, detail) =
			outcome.with_context(|| format!("failed to execute unit `{}`", unit.dotted))?;
		scoreboard.push(unit.dotted.clone(), status, detail);
	}
	Ok(scoreboard)
}

// ---------------------------------------------------------------------------
// Discovery and selection (pin §4)
// ---------------------------------------------------------------------------

fn discover_units(test_root: &Path) -> Result<Vec<Unit>> {
	let mut units = Vec::new();
	let mut prefix = vec!["test".to_owned()];
	walk_test_dir(test_root, &mut prefix, &mut units)?;
	units.sort_by(|a, b| a.dotted.cmp(&b.dotted));
	Ok(units)
}

fn walk_test_dir(dir: &Path, prefix: &mut Vec<String>, units: &mut Vec<Unit>) -> Result<()> {
	let entries =
		fs::read_dir(dir).with_context(|| format!("failed to read `{}`", dir.display()))?;
	for entry in entries {
		let entry = entry.with_context(|| format!("failed to list `{}`", dir.display()))?;
		let file_type = entry
			.file_type()
			.with_context(|| format!("failed to stat `{}`", entry.path().display()))?;
		if file_type.is_symlink() {
			continue; // Pinned: symlinks are not followed (§4.1).
		}
		let name = entry.file_name();
		let Some(name) = name.to_str() else { continue };
		if file_type.is_dir() {
			if name == "__pycache__" {
				continue;
			}
			prefix.push(name.to_owned());
			walk_test_dir(&entry.path(), prefix, units)?;
			prefix.pop();
		} else if file_type.is_file() && name.starts_with("test_") && name.ends_with(".py") {
			let stem = name[..name.len() - ".py".len()].to_owned();
			let mut dotted = prefix.join(".");
			dotted.push('.');
			dotted.push_str(&stem);
			let key = dotted["test.".len()..].to_owned();
			units.push(Unit { dotted, key, stem });
		}
	}
	Ok(())
}

/// Applies `--modules` selectors (§4.3): exact dotted name, match-key form
/// (auto-prefixed `test.`), or glob over match keys. A selector matching zero
/// units is a hard error. Output preserves canonical order.
fn select_units(units: &[Unit], selectors: &[String]) -> Result<Vec<Unit>> {
	if selectors.is_empty() {
		return Ok(units.to_vec());
	}
	let mut chosen = BTreeSet::new();
	for selector in selectors {
		let is_glob = selector.contains('*') || selector.contains('?');
		let mut matched_any = false;
		for (index, unit) in units.iter().enumerate() {
			let matched = if is_glob {
				ledger::glob_match(selector, &unit.key)
			} else {
				unit.dotted == *selector || unit.key == *selector
			};
			if matched {
				chosen.insert(index);
				matched_any = true;
			}
		}
		if !matched_any {
			bail!("--modules selector {selector} matches no vendored test module");
		}
	}
	Ok(chosen
		.into_iter()
		.map(|index| units[index].clone())
		.collect())
}

/// Unit at index `k` belongs to shard `i` iff `k % N == i` (§4.4).
fn shard_units(units: Vec<Unit>, index: u32, count: u32) -> Vec<Unit> {
	units
		.into_iter()
		.enumerate()
		.filter(|(k, _)| k % count as usize == index as usize)
		.map(|(_, unit)| unit)
		.collect()
}

/// In an unsharded, unfiltered run every ledger pattern must match at least
/// one discovered unit (pin §7.1 staleness rule).
fn check_ledger_staleness(ledger: &Ledger, units: &[Unit]) -> Result<()> {
	let patterns = ledger
		.exclusions
		.iter()
		.map(|entry| ("exclusions.toml", &entry.pattern))
		.chain(
			ledger
				.divergences
				.iter()
				.map(|entry| ("divergence-ledger.toml", &entry.pattern)),
		);
	for (file, pattern) in patterns {
		if !units
			.iter()
			.any(|unit| ledger::pattern_matches_unit(pattern, &unit.key, &unit.stem))
		{
			bail!("stale ledger entry: pattern `{pattern}` in {file} matches no discovered unit");
		}
	}
	Ok(())
}

// ---------------------------------------------------------------------------
// Per-unit execution (pin §5, §6)
// ---------------------------------------------------------------------------

fn run_unit(ctx: &ExecContext<'_>, unit: &Unit) -> Result<(Status, Option<String>)> {
	// Row 1: excluded units are not executed at all.
	if let Some(entry) = ctx.ledger.exclusion_for(&unit.key, &unit.stem) {
		return Ok((
			Status::Excluded,
			Some(format!("excluded by {} ({})", entry.pattern, entry.reason)),
		));
	}
	// Row 2: no oracle interpreter.
	if !ctx.python_available {
		return Ok((
			Status::Unsupported,
			Some("python3.14 reference interpreter is not available".to_owned()),
		));
	}

	let unit_scratch = ctx.scratch_base.join(&unit.dotted);
	let outcome = execute_unit(ctx, unit, &unit_scratch);
	let _ = fs::remove_dir_all(&unit_scratch);
	let (status, detail) = outcome?;
	Ok((status, detail.map(|detail| truncate_detail(&sanitize_detail(ctx, &detail)))))
}

fn execute_unit(
	ctx: &ExecContext<'_>,
	unit: &Unit,
	unit_scratch: &Path,
) -> Result<(Status, Option<String>)> {
	let oracle_dir = unit_scratch.join("oracle");
	let pon_dir = unit_scratch.join("pon");
	if unit_scratch.exists() {
		let _ = fs::remove_dir_all(unit_scratch);
	}
	for dir in [&oracle_dir, &pon_dir] {
		fs::create_dir_all(dir)
			.with_context(|| format!("failed to create scratch dir `{}`", dir.display()))?;
	}

	let driver = DRIVER_TEMPLATE.replace("@MODULE@", &unit.dotted);
	for dir in [&oracle_dir, &pon_dir] {
		let path = dir.join("driver.py");
		fs::write(&path, &driver).with_context(|| format!("failed to write `{}`", path.display()))?;
	}

	let timeout_secs = ctx.timeout.as_secs();

	// Oracle side runs first; a malformed oracle skips the pon subprocess (§5.2).
	let mut oracle_cmd = Command::new("python3.14");
	oracle_cmd.arg(oracle_dir.join("driver.py"));
	configure_side(&mut oracle_cmd, ctx, &oracle_dir, "PYTHONPATH")?;
	let oracle = match run_with_timeout(&mut oracle_cmd, ctx.timeout) {
		Err(error) => return Ok((Status::Unsupported, Some(format!("oracle: {error}")))),
		Ok(RunOutcome::TimedOut) => {
			return Ok((
				Status::Unsupported,
				Some(format!("oracle: timed out after {timeout_secs}s")),
			));
		},
		Ok(RunOutcome::Completed(side)) => side,
	};
	let (oracle_vector, oracle_exit) = match parse_side(&oracle, &unit.dotted) {
		Err(why) => {
			return Ok((Status::Unsupported, Some(format!("oracle: outcome block malformed: {why}"))));
		},
		Ok(parsed) => parsed,
	};

	// pon side.
	let Some(pon_binary) = &ctx.pon_binary else {
		bail!("pon binary was not built despite a runnable unit");
	};
	let mut pon_cmd = Command::new(pon_binary);
	pon_cmd.arg("run").arg(pon_dir.join("driver.py"));
	configure_side(&mut pon_cmd, ctx, &pon_dir, "PONPATH")?;
	pon_cmd.env("PON_STDLIB_PATH", &ctx.lib_dir);
	let pon = match run_with_timeout(&mut pon_cmd, ctx.timeout) {
		Err(error) => return Ok((Status::Fail, Some(format!("failed to run pon: {error}")))),
		Ok(RunOutcome::TimedOut) => {
			return Ok((Status::Fail, Some(format!("pon timed out after {timeout_secs}s"))));
		},
		Ok(RunOutcome::Completed(side)) => side,
	};
	let (pon_vector, pon_exit) = match parse_side(&pon, &unit.dotted) {
		Err(why) => {
			// Rows 6/7: malformed pon block is `unsupported` only when the run
			// itself self-reports an unsupported construct.
			if suite::is_unsupported_pon_output(pon.exit.unwrap_or(1), &pon.stderr) {
				return Ok((Status::Unsupported, Some(first_unsupported_line(&pon.stderr))));
			}
			return Ok((Status::Fail, Some(format!("pon outcome block malformed: {why}"))));
		},
		Ok(parsed) => parsed,
	};

	Ok(classify_vectors(&oracle_vector, oracle_exit, &pon_vector, pon_exit, unit, ctx.ledger))
}

/// Scrubbed child environment (§5.2): exactly PATH/HOME (inherited), TMPDIR,
/// TZ, PYTHONHASHSEED, PYTHONDONTWRITEBYTECODE, PYTHONIOENCODING, and the
/// side's import-path variable. Nothing else leaks in.
fn configure_side(
	cmd: &mut Command,
	ctx: &ExecContext<'_>,
	side_dir: &Path,
	path_var: &str,
) -> Result<()> {
	let import_path = env::join_paths([side_dir.as_os_str(), ctx.lib_dir.as_os_str()])
		.context("scratch or vendored Lib path contains a path separator")?;
	cmd.current_dir(side_dir)
		.env_clear()
		.env("TMPDIR", side_dir)
		.env("TZ", "UTC")
		.env("PYTHONHASHSEED", "0")
		.env("PYTHONDONTWRITEBYTECODE", "1")
		.env("PYTHONIOENCODING", "utf-8")
		.env(path_var, import_path);
	if let Some(path) = env::var_os("PATH") {
		cmd.env("PATH", path);
	}
	if let Some(home) = env::var_os("HOME") {
		cmd.env("HOME", home);
	}
	apply_unix_isolation(cmd, ctx.timeout);
	Ok(())
}

// ---------------------------------------------------------------------------
// Subprocess supervision (pin §5.2). The aot suites reuse this machinery for
// their own wall-clock caps (`aot.rs`); the pin governs only `cpython-full`.
// ---------------------------------------------------------------------------

pub(crate) struct SideResult {
	/// `None` when the process was killed by a signal.
	pub(crate) exit:   Option<i32>,
	pub(crate) stdout: Vec<u8>,
	pub(crate) stderr: Vec<u8>,
}

pub(crate) enum RunOutcome {
	Completed(SideResult),
	TimedOut,
}

pub(crate) fn run_with_timeout(
	cmd: &mut Command,
	timeout: Duration,
) -> std::io::Result<RunOutcome> {
	cmd.stdin(Stdio::null())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped());
	let mut child = cmd.spawn()?;
	let deadline = Instant::now() + timeout;

	let stdout_buf = Arc::new(Mutex::new(Vec::new()));
	let stderr_buf = Arc::new(Mutex::new(Vec::new()));
	let stdout_reader = spawn_reader(child.stdout.take(), Arc::clone(&stdout_buf));
	let stderr_reader = spawn_reader(child.stderr.take(), Arc::clone(&stderr_buf));

	let status = loop {
		if let Some(status) = child.try_wait()? {
			break status;
		}
		if Instant::now() >= deadline {
			// SIGKILL the whole group, then the direct child in case it left
			// the group via setsid, then reap.
			kill_process_group(child.id());
			let _ = child.kill();
			let _ = child.wait();
			// Reader threads are detached; the group kill EOFs their pipes.
			drop(stdout_reader);
			drop(stderr_reader);
			return Ok(RunOutcome::TimedOut);
		}
		thread::sleep(Duration::from_millis(5));
	};

	// The child exited. Anything still alive in its process group is a leak
	// that would hold the pipes open; kill it so the readers see EOF, then
	// drain them (pipe contents written before the kill are preserved).
	kill_process_group(child.id());
	let stdout = finish_reader(stdout_reader, &stdout_buf);
	let stderr = finish_reader(stderr_reader, &stderr_buf);
	Ok(RunOutcome::Completed(SideResult { exit: status.code(), stdout, stderr }))
}

fn spawn_reader<R: Read + Send + 'static>(
	source: Option<R>,
	buffer: Arc<Mutex<Vec<u8>>>,
) -> Option<thread::JoinHandle<()>> {
	source.map(|mut source| {
		thread::spawn(move || {
			let mut chunk = [0u8; 8192];
			loop {
				match source.read(&mut chunk) {
					Ok(0) => break,
					Ok(read) => buffer
						.lock()
						.unwrap_or_else(std::sync::PoisonError::into_inner)
						.extend_from_slice(&chunk[..read]),
					Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {},
					Err(_) => break,
				}
			}
		})
	})
}

/// Waits briefly for a reader to hit EOF, then snapshots its buffer. A reader
/// still blocked after the grace window (a pipe FD escaped the killed process
/// group) is detached; the bytes collected so far are used.
fn finish_reader(handle: Option<thread::JoinHandle<()>>, buffer: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
	if let Some(handle) = handle {
		let grace = Instant::now() + Duration::from_secs(5);
		while !handle.is_finished() && Instant::now() < grace {
			thread::sleep(Duration::from_millis(2));
		}
		if handle.is_finished() {
			let _ = handle.join();
		}
	}
	std::mem::take(
		&mut *buffer
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner),
	)
}

#[cfg(unix)]
pub(crate) fn apply_unix_isolation(cmd: &mut Command, timeout: Duration) {
	use std::os::unix::process::CommandExt;

	// Resource numbers are identical on Linux and macOS (the pinned CI hosts).
	const RLIMIT_CPU: i32 = 0;
	const RLIMIT_FSIZE: i32 = 1;
	const RLIMIT_CORE: i32 = 4;
	const GIB: u64 = 1 << 30;

	#[repr(C)]
	struct RLimit {
		rlim_cur: u64,
		rlim_max: u64,
	}

	unsafe extern "C" {
		fn setrlimit(resource: i32, rlim: *const RLimit) -> i32;
	}

	fn set_rlimit(resource: i32, soft: u64, hard: u64) -> std::io::Result<()> {
		let limit = RLimit { rlim_cur: soft, rlim_max: hard };
		// SAFETY: `setrlimit` only reads the pointed-to struct, which lives
		// across the call; it is async-signal-safe (legal between fork/exec).
		if unsafe { setrlimit(resource, &limit) } == 0 {
			Ok(())
		} else {
			Err(std::io::Error::last_os_error())
		}
	}

	cmd.process_group(0);
	let secs = timeout.as_secs();
	let soft_cpu = secs + secs.div_ceil(2); // ceil(1.5 × timeout)
	let hard_cpu = secs.saturating_mul(2);
	// SAFETY: the closure runs between fork and exec and calls only
	// async-signal-safe `setrlimit`; no allocation, locks, or FS access.
	unsafe {
		cmd.pre_exec(move || {
			set_rlimit(RLIMIT_CORE, 0, 0)?;
			set_rlimit(RLIMIT_CPU, soft_cpu, hard_cpu)?;
			set_rlimit(RLIMIT_FSIZE, GIB, GIB)?;
			Ok(())
		});
	}
}

#[cfg(not(unix))]
pub(crate) fn apply_unix_isolation(_cmd: &mut Command, _timeout: Duration) {}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
	unsafe extern "C" {
		fn kill(pid: i32, sig: i32) -> i32;
	}
	const SIGKILL: i32 = 9;
	// SAFETY: plain syscall wrapper; a negative pid addresses the process
	// group. Failure (group already gone) is ignored by design.
	unsafe {
		kill(-(pid as i32), SIGKILL);
	}
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {}

// ---------------------------------------------------------------------------
// Outcome-block parsing (pin §5.4)
// ---------------------------------------------------------------------------

fn parse_side(side: &SideResult, dotted: &str) -> std::result::Result<(Outcomes, i32), String> {
	let Some(exit) = side.exit else {
		return Err("process was killed by a signal".to_owned());
	};
	let vector = parse_outcome_block(&side.stdout, dotted)?;
	Ok((vector, exit))
}

/// Scans lossy-UTF-8 stdout for the *last* `PONTEST BEGIN <dotted>` line and
/// parses the outcome block after it. Returns the outcome vector (re-sorted
/// by test id via the map; last-wins on duplicates) or a malformed-why string.
fn parse_outcome_block(stdout: &[u8], dotted: &str) -> std::result::Result<Outcomes, String> {
	let text = String::from_utf8_lossy(stdout);
	let lines = text.lines().collect::<Vec<_>>();
	let begin_marker = format!("PONTEST BEGIN {dotted}");
	let Some(begin) = lines.iter().rposition(|line| *line == begin_marker) else {
		return Err(format!("missing `{begin_marker}` line"));
	};

	let mut outcomes = Outcomes::new();
	let mut collected = 0usize;
	for line in &lines[begin + 1..] {
		if let Some(count_text) = line.strip_prefix("PONTEST END ") {
			let Ok(count) = count_text.parse::<usize>() else {
				return Err("invalid PONTEST END count".to_owned());
			};
			if count != collected {
				return Err(format!("PONTEST END declares {count} outcome line(s), found {collected}"));
			}
			return Ok(outcomes);
		}
		let Some(rest) = line.strip_prefix("PONTEST ") else {
			return Err("stray line inside outcome block".to_owned());
		};
		let Some((outcome, test_id)) = rest.split_once(' ') else {
			return Err("malformed outcome line".to_owned());
		};
		if !OUTCOME_STRINGS.contains(&outcome) || test_id.is_empty() {
			return Err("malformed outcome line".to_owned());
		}
		outcomes.insert(test_id.to_owned(), outcome.to_owned());
		collected += 1;
	}
	Err("missing PONTEST END line".to_owned())
}

// ---------------------------------------------------------------------------
// Classification (pin §6)
// ---------------------------------------------------------------------------

const ABSENT: &str = "absent";

/// Decision-table rows 8–10 given both parsed vectors.
fn classify_vectors(
	oracle: &Outcomes,
	oracle_exit: i32,
	pon: &Outcomes,
	pon_exit: i32,
	unit: &Unit,
	ledger: &Ledger,
) -> (Status, Option<String>) {
	// Differing ids: distinct outcome, missing on pon, or extra on pon —
	// each as (id, oracle outcome, pon outcome) with `absent` markers.
	let mut differing: Vec<(&str, &str, &str)> = Vec::new();
	for (id, oracle_outcome) in oracle {
		match pon.get(id) {
			Some(pon_outcome) if pon_outcome == oracle_outcome => {},
			Some(pon_outcome) => differing.push((id, oracle_outcome, pon_outcome)),
			None => differing.push((id, oracle_outcome, ABSENT)),
		}
	}
	for (id, pon_outcome) in pon {
		if !oracle.contains_key(id) {
			differing.push((id, ABSENT, pon_outcome));
		}
	}
	differing.sort_unstable();

	let vectors_equal = differing.is_empty();
	// Row 8.
	if vectors_equal && oracle_exit == pon_exit {
		return (Status::Pass, None);
	}

	// Row 9: matched + covered divergence entry.
	let matched_divergence = ledger.divergence_for(&unit.key, &unit.stem);
	if let Some(entry) = matched_divergence {
		let covered = if entry.test_ids.is_empty() {
			true // Empty test_ids covers every differing id and exit-only mismatches (§6.3).
		} else if vectors_equal {
			false // Exit-code-only mismatch needs an empty-test_ids entry (§6.3).
		} else {
			differing
				.iter()
				.all(|(id, ..)| entry.test_ids.iter().any(|t| t == id))
		};
		if covered {
			return (
				Status::SemanticsDivergent,
				Some(format!(
					"ledgered by {} ({}): {} id(s) diverge",
					entry.pattern,
					entry.reason,
					differing.len()
				)),
			);
		}
	}

	// Row 10.
	if vectors_equal {
		return (Status::Fail, Some(format!("exit mismatch: oracle={oracle_exit} pon={pon_exit}")));
	}
	let differ_count = differing
		.iter()
		.filter(|(_, o, p)| *o != ABSENT && *p != ABSENT)
		.count();
	let missing_count = differing.iter().filter(|(_, _, p)| *p == ABSENT).count();
	let extra_count = differing.iter().filter(|(_, o, _)| *o == ABSENT).count();
	// With an uncovered divergence entry, name the first uncovered id (§6.3).
	let first = matched_divergence
		.filter(|entry| !entry.test_ids.is_empty())
		.and_then(|entry| {
			differing
				.iter()
				.find(|(id, ..)| !entry.test_ids.iter().any(|t| t == id))
		})
		.or_else(|| differing.first())
		.expect("differing is non-empty here");
	(
		Status::Fail,
		Some(format!(
			"outcome mismatch: {differ_count} id(s) differ, {missing_count} missing on pon, \
			 {extra_count} extra on pon; first: {} oracle={} pon={}",
			first.0, first.1, first.2
		)),
	)
}

// ---------------------------------------------------------------------------
// Detail hygiene (pin §6.2, §9)
// ---------------------------------------------------------------------------

/// Scratch paths embed the runner PID; scrub them so details stay
/// deterministic across runs (§9 obligation 4).
fn sanitize_detail(ctx: &ExecContext<'_>, detail: &str) -> String {
	detail.replace(&ctx.scratch_base_str, "<scratch>")
}

fn truncate_detail(detail: &str) -> String {
	if detail.len() <= MAX_DETAIL_BYTES {
		return detail.to_owned();
	}
	let mut end = MAX_DETAIL_BYTES;
	while !detail.is_char_boundary(end) {
		end -= 1;
	}
	format!("{}…", &detail[..end])
}

fn first_unsupported_line(stderr: &[u8]) -> String {
	String::from_utf8_lossy(stderr)
		.lines()
		.find(|line| line.to_ascii_lowercase().contains("unsupported"))
		.map_or_else(|| "unsupported".to_owned(), |line| line.trim().to_owned())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ledger::{DivergenceEntry, DivergenceReason};

	fn unit(dotted: &str) -> Unit {
		let key = dotted
			.strip_prefix("test.")
			.expect("dotted names start with test.")
			.to_owned();
		let stem = dotted.rsplit('.').next().expect("non-empty").to_owned();
		Unit { dotted: dotted.to_owned(), key, stem }
	}

	fn outcomes(pairs: &[(&str, &str)]) -> Outcomes {
		pairs
			.iter()
			.map(|(id, outcome)| ((*id).to_owned(), (*outcome).to_owned()))
			.collect()
	}

	fn divergence(pattern: &str, test_ids: &[&str]) -> Ledger {
		Ledger {
			divergences: vec![DivergenceEntry {
				pattern:     pattern.to_owned(),
				reason:      DivergenceReason::DelTiming,
				cpython:     "c".to_owned(),
				pon:         "p".to_owned(),
				test_ids:    test_ids.iter().map(|id| (*id).to_owned()).collect(),
				approved_by: "test".to_owned(),
				note:        String::new(),
			}],
			exclusions:  Vec::new(),
		}
	}

	#[test]
	fn discovery_finds_vendored_units_sorted_and_named() {
		let root = crate::suite::workspace_root();
		let test_root = root
			.join(crate::suite::CPYTHON_VENDOR_DIR)
			.join("Lib")
			.join("test");
		if !test_root.is_dir() {
			eprintln!("skipping CPython-full discovery test; `{}` is not present", test_root.display());
			return;
		}
		let units = discover_units(&test_root).expect("vendored tree discoverable");

		assert!(units.len() > 700, "expected the full vendored suite, found {}", units.len());
		assert!(units.windows(2).all(|pair| pair[0].dotted < pair[1].dotted), "sorted, no dups");
		assert!(units.iter().all(|unit| unit.dotted.starts_with("test.")));

		let grammar = units
			.iter()
			.find(|unit| unit.dotted == "test.test_grammar")
			.expect("top-level unit");
		assert_eq!(grammar.key, "test_grammar");
		assert_eq!(grammar.stem, "test_grammar");

		let decode = units
			.iter()
			.find(|unit| unit.dotted == "test.test_json.test_decode")
			.expect("subpackage unit");
		assert_eq!(decode.key, "test_json.test_decode");
		assert_eq!(decode.stem, "test_decode");
	}

	#[test]
	fn selectors_accept_pinned_forms_and_reject_typos() {
		let units = vec![
			unit("test.test_abc"),
			unit("test.test_json.test_decode"),
			unit("test.test_json.test_scanner"),
		];

		let all = select_units(&units, &[]).expect("empty = all");
		assert_eq!(all.len(), 3);

		let dotted =
			select_units(&units, &["test.test_json.test_decode".to_owned()]).expect("dotted form");
		assert_eq!(dotted.len(), 1);

		let keyed = select_units(&units, &["test_json.test_decode".to_owned()]).expect("key form");
		assert_eq!(keyed.len(), 1);
		assert_eq!(keyed[0].dotted, "test.test_json.test_decode");

		let globbed = select_units(&units, &["test_json*".to_owned()]).expect("glob form");
		assert_eq!(globbed.len(), 2);

		let union =
			select_units(&units, &["test_abc".to_owned(), "test_json*".to_owned()]).expect("union");
		assert_eq!(union.len(), 3);
		assert!(union.windows(2).all(|pair| pair[0].dotted < pair[1].dotted));

		let error = select_units(&units, &["test_nope".to_owned()]).expect_err("typo is loud");
		assert!(
			error
				.to_string()
				.contains("matches no vendored test module")
		);
	}

	#[test]
	fn shards_partition_the_unit_list() {
		let units = (0..10)
			.map(|i| unit(&format!("test.test_{i:02}")))
			.collect::<Vec<_>>();
		let mut recombined = Vec::new();
		for shard in 0..3u32 {
			recombined.extend(shard_units(units.clone(), shard, 3));
		}
		recombined.sort_by(|a, b| a.dotted.cmp(&b.dotted));
		assert_eq!(recombined, units);
		assert_eq!(shard_units(units.clone(), 0, 3).len(), 4);
		assert_eq!(shard_units(units, 2, 3).len(), 3);
	}

	#[test]
	fn outcome_block_parser_takes_last_block_and_validates() {
		let good = b"noise\nPONTEST BEGIN test.test_x\nPONTEST ok test.test_x.T.a\nPONTEST FAIL test.test_x.T.b\nPONTEST END 2\ntrailing";
		let vector = parse_outcome_block(good, "test.test_x").expect("well-formed");
		assert_eq!(vector.len(), 2);
		assert_eq!(vector["test.test_x.T.a"], "ok");
		assert_eq!(vector["test.test_x.T.b"], "FAIL");

		// A test that prints a fake block: only the LAST block is parsed.
		let doubled = b"PONTEST BEGIN test.test_x\nPONTEST ok fake\nPONTEST END 1\nPONTEST BEGIN test.test_x\nPONTEST END 0\n";
		let vector = parse_outcome_block(doubled, "test.test_x").expect("last block wins");
		assert!(vector.is_empty());

		assert!(parse_outcome_block(b"", "test.test_x").is_err());
		assert!(
			parse_outcome_block(b"PONTEST BEGIN test.test_x\nPONTEST ok a\n", "test.test_x").is_err()
		);
		assert!(
			parse_outcome_block(
				b"PONTEST BEGIN test.test_x\nPONTEST ok a\nPONTEST END 2\n",
				"test.test_x"
			)
			.is_err()
		);
		assert!(
			parse_outcome_block(b"PONTEST BEGIN test.test_x\ngarbage\nPONTEST END 0\n", "test.test_x")
				.is_err()
		);
		assert!(
			parse_outcome_block(
				b"PONTEST BEGIN test.test_x\nPONTEST bogus a\nPONTEST END 1\n",
				"test.test_x"
			)
			.is_err()
		);
		// Mismatched module name in BEGIN.
		assert!(
			parse_outcome_block(b"PONTEST BEGIN test.test_y\nPONTEST END 0\n", "test.test_x").is_err()
		);
	}

	#[test]
	fn classification_follows_the_decision_table() {
		let empty = Ledger::default();
		let target = unit("test.test_x");
		let base = outcomes(&[("test.test_x.T.a", "ok"), ("test.test_x.T.b", "skip")]);

		// Row 8: equal vectors, equal exits.
		assert_eq!(
			classify_vectors(&base, 0, &base.clone(), 0, &target, &empty),
			(Status::Pass, None)
		);

		// Row 10: exit mismatch with equal vectors.
		let (status, detail) = classify_vectors(&base, 0, &base.clone(), 1, &target, &empty);
		assert_eq!(status, Status::Fail);
		assert_eq!(detail.as_deref(), Some("exit mismatch: oracle=0 pon=1"));

		// Row 10: outcome mismatch without a ledger entry.
		let pon = outcomes(&[("test.test_x.T.a", "ERROR"), ("test.test_x.T.c", "ok")]);
		let (status, detail) = classify_vectors(&base, 0, &pon, 0, &target, &empty);
		assert_eq!(status, Status::Fail);
		assert_eq!(
			detail.as_deref(),
			Some(
				"outcome mismatch: 1 id(s) differ, 1 missing on pon, 1 extra on pon; first: \
				 test.test_x.T.a oracle=ok pon=ERROR"
			)
		);

		// Row 9: covered by an empty-test_ids entry (also covers exit-only mismatch).
		let whole_file = divergence("test_x", &[]);
		let (status, detail) = classify_vectors(&base, 0, &pon, 0, &target, &whole_file);
		assert_eq!(status, Status::SemanticsDivergent);
		assert_eq!(detail.as_deref(), Some("ledgered by test_x (del-timing): 3 id(s) diverge"));
		let (status, _) = classify_vectors(&base, 0, &base.clone(), 1, &target, &whole_file);
		assert_eq!(status, Status::SemanticsDivergent);

		// Row 9: covered by listed ids.
		let listed = divergence("test_x", &["test.test_x.T.a", "test.test_x.T.b", "test.test_x.T.c"]);
		let (status, _) = classify_vectors(&base, 0, &pon, 0, &target, &listed);
		assert_eq!(status, Status::SemanticsDivergent);

		// Row 10 via §6.3: an uncovered differing id defeats the entry and is named
		// first.
		let partial = divergence("test_x", &["test.test_x.T.a"]);
		let (status, detail) = classify_vectors(&base, 0, &pon, 0, &target, &partial);
		assert_eq!(status, Status::Fail);
		let detail = detail.expect("detail");
		assert!(detail.contains("first: test.test_x.T.b oracle=skip pon=absent"), "got: {detail}");

		// Exit-only mismatch is NOT covered by a listed-ids entry.
		let (status, detail) = classify_vectors(&base, 0, &base.clone(), 1, &target, &partial);
		assert_eq!(status, Status::Fail);
		assert_eq!(detail.as_deref(), Some("exit mismatch: oracle=0 pon=1"));
	}

	#[test]
	fn detail_truncation_respects_utf8_boundaries() {
		let short = "x".repeat(MAX_DETAIL_BYTES);
		assert_eq!(truncate_detail(&short), short);

		let long = "é".repeat(MAX_DETAIL_BYTES); // 2 bytes each
		let truncated = truncate_detail(&long);
		assert!(truncated.ends_with('…'));
		assert!(truncated.len() <= MAX_DETAIL_BYTES + '…'.len_utf8());
		assert!(truncated.is_char_boundary(truncated.len() - '…'.len_utf8()));
	}
}
