//! Grammar-bounded differential fuzzing for `pon-conformance --suite fuzz`.
//!
//! The generator is deterministic per `(seed, case-index)`, emits bounded
//! import-free Python programs, and runs every case under both `python3.14` and
//! `pon-cli run` using the same subprocess hygiene as the CPython-full runner:
//! scrubbed environments, per-side scratch directories, Unix process groups,
//! resource limits, and wall-clock timeouts.

use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::suite;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const RUNNER_DIR: &str = "pon-conformance-fuzz";
const CASE_FILE: &str = "case.py";
const MAX_FEATURE_CHUNKS: usize = 6;
const TIMEOUT_EXIT: i32 = 124;

const FEATURES: &[Feature] = &[
    Feature::Arithmetic,
    Feature::Strings,
    Feature::ControlFlow,
    Feature::Closures,
    Feature::Descriptors,
    Feature::Generators,
    Feature::TryExceptFinally,
    Feature::Match,
    Feature::Del,
    Feature::Comprehensions,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FuzzOptions {
    pub seed: u64,
    pub count: usize,
    pub jobs: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedRepro {
    pub index: usize,
    pub original_path: PathBuf,
    pub minimized_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FuzzSummary {
    pub generated: usize,
    pub ran: usize,
    pub divergences: Vec<SavedRepro>,
}

impl FuzzSummary {
    pub fn summary_line(&self, root: &Path) -> String {
        let repros = if self.divergences.is_empty() {
            "<none>".to_owned()
        } else {
            self.divergences
                .iter()
                .map(|repro| display_path(root, &repro.minimized_path))
                .collect::<Vec<_>>()
                .join(",")
        };
        format!(
            "fuzz summary: generated={} ran={} diverged={} repros={repros}",
            self.generated,
            self.ran,
            self.divergences.len(),
        )
    }
}

#[derive(Clone, Debug)]
struct GeneratedCase {
    index: usize,
    source: String,
}

#[derive(Clone, Debug)]
struct ExecContext {
    lib_dir: PathBuf,
    pon_binary: PathBuf,
    temp_dir: PathBuf,
    repro_dir: PathBuf,
    timeout: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Feature {
    Arithmetic,
    Strings,
    ControlFlow,
    Closures,
    Descriptors,
    Generators,
    TryExceptFinally,
    Match,
    Del,
    Comprehensions,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Observation {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stderr_class: String,
    exit: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Comparison {
    oracle: Observation,
    pon: Observation,
}

impl Comparison {
    fn diverged(&self) -> bool {
        self.oracle.stdout != self.pon.stdout
            || self.oracle.stderr_class != self.pon.stderr_class
            || self.oracle.exit != self.pon.exit
    }
}

#[derive(Clone, Debug)]
struct ProcessOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit: i32,
    timed_out: bool,
}

/// Runs `count` generated programs and returns a deterministic summary. Any
/// divergences are minimized and written under ignored `target/` scratch space.
pub fn run_fuzz_suite(root: &Path, opts: &FuzzOptions) -> Result<FuzzSummary> {
    if opts.count == 0 {
        anyhow::bail!("`--count` must be at least 1");
    }
    if !suite::python314_available() {
        anyhow::bail!("python3.14 reference interpreter is not available");
    }

    let pon_binary = suite::ensure_pon_cli(root)?;
    let target_dir = suite::target_dir(root)?;
    let run_dir = target_dir
        .join(RUNNER_DIR)
        .join(format!("seed-{}-count-{}", opts.seed, opts.count));
    let temp_dir = run_dir.join("tmp");
    let repro_dir = run_dir.join("repros");
    if run_dir.exists() {
        fs::remove_dir_all(&run_dir)
            .with_context(|| format!("failed to clear fuzz scratch `{}`", run_dir.display()))?;
    }
    fs::create_dir_all(&temp_dir).with_context(|| format!("failed to create `{}`", temp_dir.display()))?;
    fs::create_dir_all(&repro_dir).with_context(|| format!("failed to create `{}`", repro_dir.display()))?;

    let cases = (0..opts.count)
        .map(|index| GeneratedCase {
            index,
            source: generate_program(opts.seed, index),
        })
        .collect::<Vec<_>>();
    let cases = Arc::new(cases);
    let slots = Arc::new(
        (0..opts.count)
            .map(|_| Mutex::new(None))
            .collect::<Vec<Mutex<Option<Result<Option<SavedRepro>>>>>>(),
    );
    let ctx = Arc::new(ExecContext {
        lib_dir: root.join(suite::CPYTHON_VENDOR_DIR).join("Lib"),
        pon_binary,
        temp_dir,
        repro_dir,
        timeout: DEFAULT_TIMEOUT,
    });

    let jobs = opts.jobs.clamp(1, opts.count.max(1));
    let next = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::with_capacity(jobs);
    for _ in 0..jobs {
        let cases = Arc::clone(&cases);
        let slots = Arc::clone(&slots);
        let ctx = Arc::clone(&ctx);
        let next = Arc::clone(&next);
        workers.push(thread::spawn(move || {
            loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(case) = cases.get(index) else { break };
                let result = run_case(&ctx, case);
                *slots[index]
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
            }
        }));
    }

    for worker in workers {
        worker.join().map_err(|_| anyhow::anyhow!("fuzz worker panicked"))?;
    }

    let mut divergences = Vec::new();
    for (index, slot) in slots.iter().enumerate() {
        let result = slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .with_context(|| format!("fuzz worker recorded no result for case {index}"))?;
        if let Some(repro) = result.with_context(|| format!("failed to run fuzz case {index}"))? {
            divergences.push(repro);
        }
    }

    Ok(FuzzSummary {
        generated: opts.count,
        ran: opts.count,
        divergences,
    })
}

fn run_case(ctx: &ExecContext, case: &GeneratedCase) -> Result<Option<SavedRepro>> {
    let scratch = ctx.temp_dir.join(format!("case-{:04}", case.index));
    let comparison = observe_program(ctx, &case.source, &scratch)?;
    if !comparison.diverged() {
        return Ok(None);
    }

    let minimized = minimize_divergence(ctx, case.index, &case.source)?;
    let final_scratch = ctx.temp_dir.join(format!("case-{:04}-final", case.index));
    let minimized_comparison = observe_program(ctx, &minimized, &final_scratch)?;
    save_repro(ctx, case.index, &case.source, &minimized, &minimized_comparison).map(Some)
}

fn observe_program(ctx: &ExecContext, source: &str, scratch: &Path) -> Result<Comparison> {
    if scratch.exists() {
        let _ = fs::remove_dir_all(scratch);
    }
    let oracle_dir = scratch.join("oracle");
    let pon_dir = scratch.join("pon");
    fs::create_dir_all(&oracle_dir).with_context(|| format!("failed to create `{}`", oracle_dir.display()))?;
    fs::create_dir_all(&pon_dir).with_context(|| format!("failed to create `{}`", pon_dir.display()))?;

    let oracle_script = oracle_dir.join(CASE_FILE);
    let pon_script = pon_dir.join(CASE_FILE);
    fs::write(&oracle_script, source).with_context(|| format!("failed to write `{}`", oracle_script.display()))?;
    fs::write(&pon_script, source).with_context(|| format!("failed to write `{}`", pon_script.display()))?;

    let mut oracle_cmd = Command::new("python3.14");
    oracle_cmd.arg(&oracle_script);
    configure_side(&mut oracle_cmd, ctx, &oracle_dir, "PYTHONPATH")?;
    let oracle = run_with_timeout(&mut oracle_cmd, ctx.timeout).context("failed to run python3.14 fuzz case")?;

    let mut pon_cmd = Command::new(&ctx.pon_binary);
    pon_cmd.arg("run").arg(&pon_script);
    configure_side(&mut pon_cmd, ctx, &pon_dir, "PONPATH")?;
    pon_cmd.env("PON_STDLIB_PATH", &ctx.lib_dir);
    let pon = run_with_timeout(&mut pon_cmd, ctx.timeout).context("failed to run pon fuzz case")?;

    let _ = fs::remove_dir_all(scratch);
    Ok(Comparison {
        oracle: Observation::from_process(oracle),
        pon: Observation::from_process(pon),
    })
}

fn configure_side(cmd: &mut Command, ctx: &ExecContext, side_dir: &Path, path_var: &str) -> Result<()> {
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

impl Observation {
    fn from_process(output: ProcessOutput) -> Self {
        let stderr_class = if output.timed_out {
            "timeout".to_owned()
        } else {
            stderr_class(&output.stderr)
        };
        Self {
            stdout: output.stdout,
            stderr: output.stderr,
            stderr_class,
            exit: output.exit,
        }
    }
}

fn save_repro(
    ctx: &ExecContext,
    index: usize,
    original: &str,
    minimized: &str,
    comparison: &Comparison,
) -> Result<SavedRepro> {
    let stem = format!("case-{index:04}");
    let original_path = ctx.repro_dir.join(format!("{stem}.py"));
    let minimized_path = ctx.repro_dir.join(format!("{stem}.min.py"));
    fs::write(&original_path, original).with_context(|| format!("failed to write `{}`", original_path.display()))?;
    fs::write(&minimized_path, minimized).with_context(|| format!("failed to write `{}`", minimized_path.display()))?;
    fs::write(ctx.repro_dir.join(format!("{stem}.oracle.stdout")), &comparison.oracle.stdout)?;
    fs::write(ctx.repro_dir.join(format!("{stem}.oracle.stderr")), &comparison.oracle.stderr)?;
    fs::write(ctx.repro_dir.join(format!("{stem}.pon.stdout")), &comparison.pon.stdout)?;
    fs::write(ctx.repro_dir.join(format!("{stem}.pon.stderr")), &comparison.pon.stderr)?;
    fs::write(
        ctx.repro_dir.join(format!("{stem}.summary.txt")),
        mismatch_summary(comparison),
    )?;
    Ok(SavedRepro {
        index,
        original_path,
        minimized_path,
    })
}

fn mismatch_summary(comparison: &Comparison) -> String {
    format!(
        "oracle_exit={}\noracle_stderr_class={}\npon_exit={}\npon_stderr_class={}\nstdout_equal={}\n",
        comparison.oracle.exit,
        comparison.oracle.stderr_class,
        comparison.pon.exit,
        comparison.pon.stderr_class,
        comparison.oracle.stdout == comparison.pon.stdout,
    )
}

fn minimize_divergence(ctx: &ExecContext, index: usize, source: &str) -> Result<String> {
    let mut attempt = 0usize;
    minimize_source(source, |candidate| {
        attempt += 1;
        let scratch = ctx.temp_dir.join(format!("case-{index:04}-shrink-{attempt:03}"));
        observe_program(ctx, candidate, &scratch).map(|comparison| comparison.diverged())
    })
}

fn minimize_source<F>(source: &str, mut still_diverges: F) -> Result<String>
where
    F: FnMut(&str) -> Result<bool>,
{
    let mut units = split_statement_units(source);
    if units.len() <= 1 {
        return Ok(source.to_owned());
    }

    let mut index = 0usize;
    while index < units.len() && units.len() > 1 {
        let mut candidate_units = units.clone();
        candidate_units.remove(index);
        let candidate = join_statement_units(&candidate_units);
        if still_diverges(&candidate)? {
            units = candidate_units;
            index = 0;
        } else {
            index += 1;
        }
    }

    Ok(join_statement_units(&units))
}

fn split_statement_units(source: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    let mut in_marker = false;
    let mut saw_marker = false;

    for line in source.lines() {
        if line.starts_with("# PONFUZZ BEGIN ") {
            if !current.is_empty() && !in_marker {
                chunks.push(current.join("\n"));
                current.clear();
            }
            saw_marker = true;
            in_marker = true;
            current.push(line.to_owned());
        } else if in_marker {
            current.push(line.to_owned());
            if line.starts_with("# PONFUZZ END ") {
                chunks.push(current.join("\n"));
                current.clear();
                in_marker = false;
            }
        } else if !line.trim().is_empty() {
            current.push(line.to_owned());
        }
    }

    if !current.is_empty() {
        chunks.push(current.join("\n"));
    }
    if saw_marker {
        return chunks;
    }
    split_physical_top_level(source)
}

fn split_physical_top_level(source: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    for line in source.lines() {
        let starts_top_level = !line.trim().is_empty() && !line.starts_with([' ', '\t']);
        if starts_top_level && !current.is_empty() {
            chunks.push(current.join("\n"));
            current.clear();
        }
        if !line.trim().is_empty() || !current.is_empty() {
            current.push(line.to_owned());
        }
    }
    if !current.is_empty() {
        chunks.push(current.join("\n"));
    }
    chunks
}

fn join_statement_units(units: &[String]) -> String {
    let mut source = units.join("\n\n");
    source.push('\n');
    source
}

fn generate_program(seed: u64, index: usize) -> String {
    let mut rng = SplitMix64::new(seed ^ ((index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)));
    let mut selected = Vec::with_capacity(MAX_FEATURE_CHUNKS);
    add_feature(&mut selected, Feature::Arithmetic);
    add_feature(&mut selected, Feature::ControlFlow);
    add_feature(&mut selected, FEATURES[index % FEATURES.len()]);
    while selected.len() < MAX_FEATURE_CHUNKS {
        add_feature(&mut selected, FEATURES[rng.range(FEATURES.len())]);
    }

    let mut source = format!("# pon-conformance fuzz generated seed={seed} case={index}\n");
    for feature in selected {
        source.push('\n');
        source.push_str(&emit_feature(feature, &mut rng));
    }
    source
}

fn add_feature(selected: &mut Vec<Feature>, feature: Feature) {
    if !selected.contains(&feature) {
        selected.push(feature);
    }
}

fn emit_feature(feature: Feature, rng: &mut SplitMix64) -> String {
    match feature {
        Feature::Arithmetic => emit_arithmetic(rng),
        Feature::Strings => emit_strings(rng),
        Feature::ControlFlow => emit_control_flow(rng),
        Feature::Closures => emit_closures(rng),
        Feature::Descriptors => emit_descriptors(rng),
        Feature::Generators => emit_generators(rng),
        Feature::TryExceptFinally => emit_try_except_finally(rng),
        Feature::Match => emit_match(rng),
        Feature::Del => emit_del(rng),
        Feature::Comprehensions => emit_comprehensions(rng),
    }
}

fn emit_arithmetic(rng: &mut SplitMix64) -> String {
    let small = rng.range(97) as i64 - 31;
    let shift = 70 + rng.range(57);
    let tail = 10_000 + rng.range(90_000);
    let div = 3 + rng.range(19);
    format!(
        "# PONFUZZ BEGIN arithmetic\nfa = {small}\nfb = (1 << {shift}) + {tail}\nfc = ((fb // {div}) % 1000003, (fb + fa) % 97, (-fb) % 89)\nprint('arith', fa, fc)\n# PONFUZZ END arithmetic\n"
    )
}

fn emit_strings(rng: &mut SplitMix64) -> String {
    let repeat = 2 + rng.range(5);
    let value = rng.range(10_000);
    format!(
        "# PONFUZZ BEGIN strings\nfs = ('abC' * {repeat}) + '-' + str({value})\nft = fs[::-1]\nprint('str', len(fs), fs[:4], ft[:5], fs.replace('a', 'x').count('x'))\n# PONFUZZ END strings\n"
    )
}

fn emit_control_flow(rng: &mut SplitMix64) -> String {
    let for_bound = 3 + rng.range(7);
    let while_bound = 2 + rng.range(5);
    format!(
        "# PONFUZZ BEGIN control\nct = 0\nfor ci in range({for_bound}):\n    if ci % 2:\n        ct += ci * ci\n    else:\n        ct -= ci\ncj = 0\nwhile cj < {while_bound}:\n    ct += cj + {for_bound}\n    cj += 1\nprint('ctrl', ct)\n# PONFUZZ END control\n"
    )
}

fn emit_closures(rng: &mut SplitMix64) -> String {
    let base = rng.range(17) as i64 - 4;
    format!(
        "# PONFUZZ BEGIN closures\ndef fuzz_outer(base):\n    acc = base\n    def fuzz_inner(x):\n        nonlocal acc\n        acc += x\n        return acc * 2\n    return fuzz_inner\nff = fuzz_outer({base})\nprint('closure', ff(1), ff(2), ff(-1))\n# PONFUZZ END closures\n"
    )
}

fn emit_descriptors(rng: &mut SplitMix64) -> String {
    let bias = 1 + rng.range(9);
    let start = rng.range(23) as i64 - 5;
    format!(
        "# PONFUZZ BEGIN descriptors\nclass FuzzDesc:\n    def __init__(self, bias):\n        self.bias = bias\n    def __get__(self, obj, typ=None):\n        if obj is None:\n            return self\n        return obj.value + self.bias\n    def __set__(self, obj, value):\n        obj.value = value - self.bias\nclass FuzzBox:\n    item = FuzzDesc({bias})\n    def __init__(self, value):\n        self.value = value\n    def bump(self):\n        self.item = self.item + 3\n        return self.item\nfbox = FuzzBox({start})\nprint('desc', fbox.item, fbox.bump(), fbox.value)\n# PONFUZZ END descriptors\n"
    )
}

fn emit_generators(rng: &mut SplitMix64) -> String {
    let seed = rng.range(13) as i64 - 3;
    let sent = 1 + rng.range(7);
    format!(
        "# PONFUZZ BEGIN generators\ndef fuzz_gen(seed):\n    total = seed\n    try:\n        received = yield total\n        total += received\n        try:\n            yield total\n        except ValueError as exc:\n            total += len(str(exc))\n            yield total\n    finally:\n        total += 100\nfg = fuzz_gen({seed})\nfg_a = next(fg)\nfg_b = fg.send({sent})\nfg_c = fg.throw(ValueError('xy'))\ntry:\n    next(fg)\nexcept StopIteration:\n    fg_done = 'stop'\nfg2 = fuzz_gen(1)\nnext(fg2)\nfg2.close()\nprint('gen', fg_a, fg_b, fg_c, fg_done)\n# PONFUZZ END generators\n"
    )
}

fn emit_try_except_finally(rng: &mut SplitMix64) -> String {
    let mode = rng.range(3);
    format!(
        "# PONFUZZ BEGIN try\ntry:\n    if {mode} == 0:\n        raise KeyError('k')\n    if {mode} == 1:\n        tv = 10 // 0\n    else:\n        tv = 10 // 2\nexcept KeyError as exc:\n    tout = ('key', len(exc.args))\nexcept ZeroDivisionError:\n    tout = ('zero', 0)\nelse:\n    tout = ('else', tv)\nfinally:\n    tout = tout + ('fin',)\nprint('try', tout)\n# PONFUZZ END try\n"
    )
}

fn emit_match(rng: &mut SplitMix64) -> String {
    let tag = rng.range(3);
    let item = rng.range(20);
    format!(
        "# PONFUZZ BEGIN match\nmsubj = {{'tag': {tag}, 'items': [{item}, {item} + 1, {item} + 2]}}\nmatch msubj:\n    case {{'tag': 0, 'items': [mx, my, *_]}} if my == mx + 1:\n        mout = mx + 10\n    case {{'tag': 1, 'items': [mx, *_]}}:\n        mout = mx + 20\n    case {{'items': [mx, my, mz]}}:\n        mout = mx + my + mz\nprint('match', mout)\n# PONFUZZ END match\n"
    )
}

fn emit_del(rng: &mut SplitMix64) -> String {
    let base = rng.range(30) as i64 - 10;
    format!(
        "# PONFUZZ BEGIN del\nclass FuzzDelBox:\n    pass\nfd_box = FuzzDelBox()\nfd_box.value = {base}\nfd_items = [{base}, {base} + 1, {base} + 2]\nfd_dict = {{'left': {base}, 'right': {base} + 1}}\ndel fd_items[1]\ndel fd_dict['left']\ndel fd_box.value\nfd_shadow = {base} + 5\ndel fd_shadow\ntry:\n    fd_seen = fd_box.value\nexcept AttributeError:\n    fd_seen = 'gone'\nprint('del', fd_items, sorted(fd_dict.items()), fd_seen)\n# PONFUZZ END del\n"
    )
}

fn emit_comprehensions(rng: &mut SplitMix64) -> String {
    let bound = 3 + rng.range(6);
    let bias = rng.range(9) as i64 - 2;
    format!(
        "# PONFUZZ BEGIN comprehensions\nclist = [ci * ci + ({bias}) for ci in range({bound}) if ci % 2 == {bound} % 2]\ncdict = {{str(ci): ci + ({bias}) for ci in range(3)}}\ncset = {{ci % 4 for ci in range({bound} + 2)}}\ncgen = (ci + ({bias}) for ci in range(4))\nprint('comp', clist, sorted(cdict.items()), sorted(cset), list(cgen))\n# PONFUZZ END comprehensions\n"
    )
}

#[derive(Clone, Debug)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    fn range(&mut self, upper: usize) -> usize {
        debug_assert!(upper > 0);
        (self.next() % upper as u64) as usize
    }
}

fn stderr_class(stderr: &[u8]) -> String {
    if stderr.is_empty() {
        return "none".to_owned();
    }
    let text = String::from_utf8_lossy(stderr);
    for line in text.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.to_ascii_lowercase().contains("unsupported") {
            return "unsupported".to_owned();
        }
        if let Some((head, _)) = trimmed.split_once(':') {
            let class = head.trim();
            if is_exception_class(class) {
                return class.to_owned();
            }
        }
        return trimmed
            .split_whitespace()
            .next()
            .unwrap_or("stderr")
            .trim_end_matches(':')
            .to_owned();
    }
    "stderr".to_owned()
}

fn is_exception_class(value: &str) -> bool {
    let Some(first) = value.chars().next() else {
        return false;
    };
    first.is_ascii_uppercase()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.')
}

fn display_path(root: &Path, path: &Path) -> String {
    let display = path.strip_prefix(root).unwrap_or(path);
    suite::normalize_path(display)
}

fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> std::io::Result<ProcessOutput> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let deadline = Instant::now() + timeout;
    let stdout_buf = Arc::new(Mutex::new(Vec::new()));
    let stderr_buf = Arc::new(Mutex::new(Vec::new()));
    let stdout_reader = spawn_reader(child.stdout.take(), Arc::clone(&stdout_buf));
    let stderr_reader = spawn_reader(child.stderr.take(), Arc::clone(&stderr_buf));

    loop {
        if let Some(status) = child.try_wait()? {
            let stdout = finish_reader(stdout_reader, &stdout_buf);
            let stderr = finish_reader(stderr_reader, &stderr_buf);
            return Ok(ProcessOutput {
                stdout,
                stderr,
                exit: status.code().unwrap_or(1),
                timed_out: false,
            });
        }
        if Instant::now() >= deadline {
            kill_process_group(child.id());
            let _ = child.kill();
            let _ = child.wait();
            let stdout = finish_reader(stdout_reader, &stdout_buf);
            let stderr = finish_reader(stderr_reader, &stderr_buf);
            return Ok(ProcessOutput {
                stdout,
                stderr,
                exit: TIMEOUT_EXIT,
                timed_out: true,
            });
        }
        thread::sleep(Duration::from_millis(5));
    }
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
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        })
    })
}

fn finish_reader(handle: Option<thread::JoinHandle<()>>, buffer: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    if let Some(handle) = handle {
        let grace = Instant::now() + Duration::from_millis(50);
        while !handle.is_finished() && Instant::now() < grace {
            thread::sleep(Duration::from_millis(2));
        }
        if handle.is_finished() {
            let _ = handle.join();
        }
    }
    std::mem::take(&mut *buffer.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
}

#[cfg(unix)]
fn apply_unix_isolation(cmd: &mut Command, timeout: Duration) {
    use std::os::unix::process::CommandExt;

    const RLIMIT_CPU: i32 = 0;
    const RLIMIT_FSIZE: i32 = 1;
    const RLIMIT_CORE: i32 = 4;
    const GIB: u64 = 1024 * 1024 * 1024;

    #[repr(C)]
    struct RLimit {
        rlim_cur: u64,
        rlim_max: u64,
    }

    unsafe extern "C" {
        fn setrlimit(resource: i32, limit: *const RLimit) -> i32;
    }

    fn set_rlimit(resource: i32, soft: u64, hard: u64) -> std::io::Result<()> {
        let limit = RLimit {
            rlim_cur: soft,
            rlim_max: hard,
        };
        // SAFETY: `setrlimit` only reads the pointed-to plain-old-data struct,
        // which lives for the duration of the call.
        if unsafe { setrlimit(resource, &limit) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    cmd.process_group(0);
    let secs = timeout.as_secs().max(1);
    let soft_cpu = secs + secs.div_ceil(2);
    let hard_cpu = secs.saturating_mul(2).max(soft_cpu);
    // SAFETY: the closure runs after fork and before exec. It performs only
    // async-signal-safe libc calls through `setrlimit`.
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
fn apply_unix_isolation(_cmd: &mut Command, _timeout: Duration) {}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    const SIGKILL: i32 = 9;
    // SAFETY: sending SIGKILL to the child's process group is the documented
    // Unix timeout cleanup path. Errors are intentionally ignored here.
    let _ = unsafe { kill(-(pid as i32), SIGKILL) };
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generator_is_deterministic_per_seed_and_case() {
        let first = generate_program(0xA11C_E5EED, 7);
        let repeated = generate_program(0xA11C_E5EED, 7);
        let different_case = generate_program(0xA11C_E5EED, 8);
        let different_seed = generate_program(0xA11C_E5EEC, 7);

        assert_eq!(first, repeated);
        assert_ne!(first, different_case);
        assert_ne!(first, different_seed);
    }

    #[test]
    fn generated_programs_include_marked_printing_feature_chunks() {
        let source = generate_program(0x5150_F00D, 3);
        let begin_markers = source.matches("# PONFUZZ BEGIN ").count();
        let end_markers = source.matches("# PONFUZZ END ").count();

        assert!(begin_markers >= 2, "generated source should carry multiple marked feature chunks:\n{source}");
        assert_eq!(begin_markers, end_markers, "feature chunk markers should be paired:\n{source}");
        assert!(source.contains("print("), "generated source should emit observable output:\n{source}");
    }

    #[test]
    fn minimizer_greedily_rechecks_earlier_chunks_and_preserves_multiline_marker() {
        let source = "\
padding = 'temporary'

blocker = 'guard'

# PONFUZZ BEGIN critical
def trigger():
    return 'diverged'
print('critical', trigger())
# PONFUZZ END critical

noise = 'always removable'
";

        let minimized = minimize_source(source, |candidate| {
            Ok(candidate.contains("trigger()")
                && (!candidate.contains("blocker") || candidate.contains("padding")))
        })
        .expect("predicate is infallible");

        assert_eq!(
            minimized,
            "\
# PONFUZZ BEGIN critical
def trigger():
    return 'diverged'
print('critical', trigger())
# PONFUZZ END critical
"
        );
    }

    #[test]
    fn split_join_keeps_marked_multiline_units_minimizable() {
        let source = "\
# PONFUZZ BEGIN class
class Box:
    def value(self):
        return 41
print('box', Box().value())
# PONFUZZ END class

# PONFUZZ BEGIN try
try:
    result = 10 // 2
except ZeroDivisionError:
    result = -1
print('try', result)
# PONFUZZ END try
";
        let units = split_statement_units(source);

        assert_eq!(units.len(), 2);
        assert!(units[0].contains("class Box:\n    def value(self):\n        return 41"));
        assert!(units[1].contains("try:\n    result = 10 // 2\nexcept ZeroDivisionError:"));

        let joined = join_statement_units(&units);
        assert_eq!(split_statement_units(&joined), units);

        let minimized = minimize_source(&joined, |candidate| Ok(candidate.contains("print('try', result)")))
            .expect("predicate is infallible");

        assert_eq!(
            minimized,
            "\
# PONFUZZ BEGIN try
try:
    result = 10 // 2
except ZeroDivisionError:
    result = -1
print('try', result)
# PONFUZZ END try
"
        );
    }
}
