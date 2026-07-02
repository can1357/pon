use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::scoreboard::{Scoreboard, Status};

pub const CPYTHON_TAG: &str = "v3.14.0";
pub(crate) const CPYTHON_VENDOR_DIR: &str = "pon-conformance/vendor/cpython-3.14";
const CPYTHON_MANIFEST: &str = "pon-conformance/corpus/MANIFEST";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SuiteName {
    Slice,
    Cpython,
    CpythonAotSubset,
    AotParity,
    CpythonFull,
    FtStress,
    Fuzz,
}

impl SuiteName {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "slice" => Ok(Self::Slice),
            "cpython" => Ok(Self::Cpython),
            "cpython-aot-subset" => Ok(Self::CpythonAotSubset),
            "aot-parity" => Ok(Self::AotParity),
            "cpython-full" => Ok(Self::CpythonFull),
            "ft-stress" => Ok(Self::FtStress),
            "fuzz" => Ok(Self::Fuzz),
            _ => bail!("unsupported suite `{value}`"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RunResult {
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) exit: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReferenceMode {
    Python314,
    BuiltInGoldens,
}

pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn run_slice_suite(root: &Path, requested_modules: &[PathBuf]) -> Result<Scoreboard> {
    let scripts = if requested_modules.is_empty() {
        slice_scripts(root)?
    } else {
        requested_modules
            .iter()
            .map(|path| resolve_workspace_path(root, path))
            .collect::<Vec<_>>()
    };
    let mode = if python314_available() {
        ReferenceMode::Python314
    } else {
        ReferenceMode::BuiltInGoldens
    };

    match mode {
        ReferenceMode::Python314 => println!("reference: python3.14"),
        ReferenceMode::BuiltInGoldens => println!("reference: built-in Phase-A goldens"),
    }

    let pon_binary = ensure_pon_cli(root)?;
    let mut scoreboard = Scoreboard::new("slice", None);

    for script in scripts {
        let pon = run_pon(root, &pon_binary, &script).with_context(|| format!("failed to run pon for `{}`", script.display()))?;
        let reference = match mode {
            ReferenceMode::Python314 => {
                run_python314(root, &script).with_context(|| format!("failed to run python3.14 for `{}`", script.display()))?
            }
            ReferenceMode::BuiltInGoldens => built_in_golden(&script)?,
        };

        let rel = display_path(root, &script);
        if pon == reference {
            scoreboard.push(rel, Status::Pass, None);
        } else {
            let report = mismatch_report(&rel, &pon, &reference);
            eprintln!("{report}");
            scoreboard.push(rel, Status::Fail, Some(report));
        }
    }

    Ok(scoreboard)
}

pub fn run_cpython_suite(root: &Path, requested_modules: &[PathBuf]) -> Result<Scoreboard> {
    let mut scoreboard = Scoreboard::new("cpython", Some(cpython_revision(root)));
    let modules = if requested_modules.is_empty() {
        cpython_manifest_modules(root)?
            .into_iter()
            .map(|module| {
                let (label, script) = resolve_cpython_module(root, &module);
                (label, script, Status::Fail, "manifest entry is not present".to_owned())
            })
            .collect::<Vec<_>>()
    } else {
        requested_modules
            .iter()
            .map(|module| {
                let (label, script) = resolve_cpython_module(root, module);
                (label, script, Status::Unsupported, "vendored CPython module is not present".to_owned())
            })
            .collect::<Vec<_>>()
    };

    if modules.is_empty() {
        return Ok(scoreboard);
    }

    if !python314_available() {
        for (label, _, _, _) in modules {
            scoreboard.push(label, Status::Unsupported, Some("python3.14 reference interpreter is not available".to_owned()));
        }
        return Ok(scoreboard);
    }

    let pon_binary = ensure_pon_cli(root)?;
    for (label, script, missing_status, missing_detail) in modules {
        let Some(script) = script else {
            scoreboard.push(label, missing_status, Some(missing_detail));
            continue;
        };

        let reference = match run_python314(root, &script) {
            Ok(reference) => reference,
            Err(error) => {
                scoreboard.push(label, Status::Fail, Some(format!("failed to run python3.14: {error:#}")));
                continue;
            }
        };
        let pon = match run_pon(root, &pon_binary, &script) {
            Ok(pon) => pon,
            Err(error) => {
                scoreboard.push(label, Status::Fail, Some(format!("failed to run pon: {error:#}")));
                continue;
            }
        };

        if pon == reference {
            scoreboard.push(label, Status::Pass, None);
        } else if is_unsupported_pon_result(&pon) {
            scoreboard.push(label.clone(), Status::Unsupported, Some(mismatch_report(&label, &pon, &reference)));
        } else {
            scoreboard.push(label.clone(), Status::Fail, Some(mismatch_report(&label, &pon, &reference)));
        }
    }

    Ok(scoreboard)
}

pub fn run_ft_stress_suite(root: &Path, requested_modules: &[PathBuf]) -> Result<Scoreboard> {
    let scripts = if requested_modules.is_empty() {
        ft_stress_scripts(root)?
    } else {
        requested_modules
            .iter()
            .map(|path| resolve_workspace_path(root, path))
            .collect::<Vec<_>>()
    };
    let mode = if python314_available() {
        ReferenceMode::Python314
    } else {
        ReferenceMode::BuiltInGoldens
    };

    match mode {
        ReferenceMode::Python314 => println!("reference: python3.14"),
        ReferenceMode::BuiltInGoldens => println!("reference: built-in FT stress goldens"),
    }

    let pon_binary = ensure_pon_cli_free_threading(root)?;
    let mut scoreboard = Scoreboard::new("ft-stress", None);

    for script in scripts {
        let pon = run_pon(root, &pon_binary, &script).with_context(|| format!("failed to run pon for `{}`", script.display()))?;
        let reference = match mode {
            ReferenceMode::Python314 => {
                run_python314(root, &script).with_context(|| format!("failed to run python3.14 for `{}`", script.display()))?
            }
            ReferenceMode::BuiltInGoldens => ft_stress_golden(&script)?,
        };

        let rel = display_path(root, &script);
        if pon == reference {
            scoreboard.push(rel, Status::Pass, None);
        } else {
            let report = mismatch_report(&rel, &pon, &reference);
            eprintln!("{report}");
            scoreboard.push(rel, Status::Fail, Some(report));
        }
    }

    Ok(scoreboard)
}


pub(crate) fn cpython_manifest_modules(root: &Path) -> Result<Vec<PathBuf>> {
    let manifest = root.join(CPYTHON_MANIFEST);
    let text = fs::read_to_string(&manifest).with_context(|| format!("failed to read `{}`", manifest.display()))?;
    let mut modules = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let entry = line.split_once('#').map_or(line, |(entry, _)| entry).trim();
        if entry.is_empty() {
            continue;
        }
        let path = PathBuf::from(entry);
        if path.is_absolute() {
            bail!("{}:{} manifest entries must be relative paths", manifest.display(), index + 1);
        }
        modules.push(path);
    }
    Ok(modules)
}

fn slice_scripts(root: &Path) -> Result<Vec<PathBuf>> {
    let corpus = root.join("pon-conformance").join("corpus");
    let mut scripts = Vec::new();

    let corpus_hello = corpus.join("hello.py");
    if corpus_hello.is_file() {
        scripts.push(corpus_hello);
    } else {
        let root_hello = root.join("hello.py");
        if root_hello.is_file() {
            scripts.push(root_hello);
        }
    }

    if corpus.is_dir() {
        let mut arithmetic = fs::read_dir(&corpus)
            .with_context(|| format!("failed to read corpus directory `{}`", corpus.display()))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .with_context(|| format!("failed to list corpus directory `{}`", corpus.display()))?;
        arithmetic.retain(|path| is_arithmetic_script(path));
        arithmetic.sort();
        scripts.extend(arithmetic);
    }

    scripts.dedup();
    if scripts.is_empty() {
        bail!("no Phase-A slice corpus found")
    }
    Ok(scripts)
}

fn ft_stress_scripts(root: &Path) -> Result<Vec<PathBuf>> {
    let script = root.join("tests").join("ft").join("shared_counter.py");
    if script.is_file() {
        Ok(vec![script])
    } else {
        bail!("no FT stress fixtures found")
    }
}


fn is_arithmetic_script(path: &Path) -> bool {
    path.extension() == Some(OsStr::new("py"))
        && path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with("arithmetic"))
}

pub(crate) fn python314_available() -> bool {
    Command::new("python3.14")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

pub(crate) fn ensure_pon_cli(root: &Path) -> Result<PathBuf> {
    let output = Command::new("cargo")
        .arg("build")
        .arg("--quiet")
        .arg("-p")
        .arg("pon-cli")
        .current_dir(root)
        .output()
        .context("failed to spawn cargo build for pon-cli")?;
    if !output.status.success() {
        bail!(
            "failed to build pon-cli\nstdout={:?}\nstderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    let binary = target_dir(root)?
        .join("debug")
        .join(format!("pon-cli{}", std::env::consts::EXE_SUFFIX));
    if !binary.is_file() {
        bail!("cargo build succeeded but `{}` was not created", binary.display());
    }
    Ok(binary)
}

fn ensure_pon_cli_free_threading(root: &Path) -> Result<PathBuf> {
    let output = Command::new("cargo")
        .arg("build")
        .arg("--quiet")
        .arg("-p")
        .arg("pon-cli")
        .arg("--features")
        .arg("free-threading")
        .current_dir(root)
        .output()
        .context("failed to spawn cargo build for pon-cli with free-threading")?;
    if !output.status.success() {
        bail!(
            "failed to build pon-cli with free-threading\nstdout={:?}\nstderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    let binary = target_dir(root)?
        .join("debug")
        .join(format!("pon-cli{}", std::env::consts::EXE_SUFFIX));
    if !binary.is_file() {
        bail!("cargo build succeeded but `{}` was not created", binary.display());
    }
    Ok(binary)
}


pub(crate) fn target_dir(root: &Path) -> Result<PathBuf> {
    let output = Command::new("cargo")
        .arg("metadata")
        .arg("--no-deps")
        .arg("--format-version")
        .arg("1")
        .current_dir(root)
        .output()
        .context("failed to query cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed\nstdout={:?}\nstderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    let metadata = String::from_utf8(output.stdout).context("cargo metadata was not UTF-8")?;
    let key = "\"target_directory\":\"";
    let start = metadata
        .find(key)
        .map(|index| index + key.len())
        .context("cargo metadata did not contain target_directory")?;
    let rest = &metadata[start..];
    let end = rest
        .find('"')
        .context("cargo metadata target_directory was unterminated")?;
    Ok(PathBuf::from(&rest[..end]))
}

/// Determinism pins shared by both corpus sides (the `cpython-full`/`fuzz`
/// runners scrub the whole environment per §5.2; the corpus suite only pins
/// the two variables differential output depends on): `TZ=UTC` so
/// `time.localtime` agrees between pon's pinned-UTC clock and the reference
/// interpreter on any host, and `PYTHONHASHSEED=0` so reference hash order
/// is reproducible.
fn pin_determinism(command: &mut Command) -> &mut Command {
    command.env("TZ", "UTC").env("PYTHONHASHSEED", "0")
}

pub(crate) fn run_pon(root: &Path, pon_binary: &Path, script: &Path) -> Result<RunResult> {
    run_command(pin_determinism(
        Command::new(pon_binary)
            .arg("run")
            .arg(script)
            .current_dir(root),
    ))
}

pub(crate) fn run_python314(root: &Path, script: &Path) -> Result<RunResult> {
    run_command(pin_determinism(Command::new("python3.14").arg(script).current_dir(root)))
}

pub(crate) fn run_command(command: &mut Command) -> Result<RunResult> {
    let output = command.output().context("failed to spawn process")?;
    Ok(RunResult {
        stdout: output.stdout,
        stderr: output.stderr,
        exit: output.status.code().unwrap_or(1),
    })
}

fn built_in_golden(script: &Path) -> Result<RunResult> {
    let name = script.file_name().and_then(OsStr::to_str).unwrap_or_default();
    let stdout = if name == "hello.py" {
        b"hello, world\n5\n".to_vec()
    } else if name.starts_with("arithmetic") {
        b"5\n".to_vec()
    } else {
        bail!("no built-in Phase-A golden for `{}`", script.display())
    };
    Ok(RunResult {
        stdout,
        stderr: Vec::new(),
        exit: 0,
    })
}

fn ft_stress_golden(script: &Path) -> Result<RunResult> {
    let name = script.file_name().and_then(OsStr::to_str).unwrap_or_default();
    let stdout = if name == "shared_counter.py" {
        b"shared_counter ok 100\n".to_vec()
    } else {
        bail!("no built-in FT stress golden for `{}`", script.display())
    };
    Ok(RunResult {
        stdout,
        stderr: Vec::new(),
        exit: 0,
    })
}

fn resolve_workspace_path(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

pub(crate) fn cpython_revision(root: &Path) -> String {
    let revision_path = root.join(CPYTHON_VENDOR_DIR).join("REVISION");
    fs::read_to_string(revision_path)
        .map(|revision| revision.trim().to_owned())
        .ok()
        .filter(|revision| !revision.is_empty())
        .unwrap_or_else(|| CPYTHON_TAG.to_owned())
}

pub(crate) fn resolve_cpython_module(root: &Path, module: &Path) -> (String, Option<PathBuf>) {
    if module.is_absolute() {
        let label = display_path(root, module);
        return (label, module.is_file().then(|| module.to_path_buf()));
    }

    let workspace_candidate = root.join(module);
    if workspace_candidate.is_file() {
        return (normalize_path(module), Some(workspace_candidate));
    }

    if module.starts_with("corpus") {
        let conformance_candidate = root.join("pon-conformance").join(module);
        if conformance_candidate.is_file() {
            return (normalize_path(module), Some(conformance_candidate));
        }
    }

    let vendor_candidate = root.join(CPYTHON_VENDOR_DIR).join(module);
    (normalize_path(module), vendor_candidate.is_file().then_some(vendor_candidate))
}

pub(crate) fn display_path(root: &Path, script: &Path) -> String {
    script
        .strip_prefix(root)
        .unwrap_or(script)
        .to_string_lossy()
        .replace('\\', "/")
}

pub(crate) fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn is_unsupported_pon_result(result: &RunResult) -> bool {
    is_unsupported_pon_output(result.exit, &result.stderr)
}

/// The pin's `is_unsupported_pon_result` predicate on raw exit/stderr: a non-zero
/// exit whose stderr mentions "unsupported" (ASCII case-insensitively).
pub(crate) fn is_unsupported_pon_output(exit: i32, stderr: &[u8]) -> bool {
    if exit == 0 {
        return false;
    }
    String::from_utf8_lossy(stderr)
        .to_ascii_lowercase()
        .contains("unsupported")
}

fn mismatch_report(path: &str, pon: &RunResult, reference: &RunResult) -> String {
    format!(
        "FAIL {path}\n  pon: exit={} stdout={:?} stderr={:?}\n  ref: exit={} stdout={:?} stderr={:?}",
        pon.exit,
        String::from_utf8_lossy(&pon.stdout),
        String::from_utf8_lossy(&pon.stderr),
        reference.exit,
        String::from_utf8_lossy(&reference.stdout),
        String::from_utf8_lossy(&reference.stderr),
    )
}
