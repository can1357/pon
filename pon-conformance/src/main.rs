#![doc = "Phase-A differential conformance runner."]

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

#[derive(Clone, Debug, Eq, PartialEq)]
struct RunResult {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReferenceMode {
    Python314,
    BuiltInGoldens,
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
    let args = env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [] => run_slice_suite(),
        [flag, suite] if flag.as_str() == "--suite" && suite.as_str() == "slice" => run_slice_suite(),
        [flag, suite] if flag.as_str() == "--suite" => bail!("unsupported suite `{suite}`"),
        _ => bail!("usage: pon-conformance --suite slice"),
    }
}

fn run_slice_suite() -> Result<()> {
    let root = workspace_root();
    let scripts = slice_scripts(&root)?;
    let mode = if python314_available() {
        ReferenceMode::Python314
    } else {
        ReferenceMode::BuiltInGoldens
    };

    match mode {
        ReferenceMode::Python314 => println!("reference: python3.14"),
        ReferenceMode::BuiltInGoldens => println!("reference: built-in Phase-A goldens"),
    }

    let pon_binary = ensure_pon_cli(&root)?;

    let mut any_fail = false;
    let mut scoreboard = Vec::with_capacity(scripts.len());

    for script in scripts {
        let pon = run_pon(&root, &pon_binary, &script).with_context(|| format!("failed to run pon for `{}`", script.display()))?;
        let reference = match mode {
            ReferenceMode::Python314 => {
                run_python314(&root, &script).with_context(|| format!("failed to run python3.14 for `{}`", script.display()))?
            }
            ReferenceMode::BuiltInGoldens => built_in_golden(&script)?,
        };

        let rel = display_path(&root, &script);
        if pon == reference {
            scoreboard.push((rel, "pass"));
        } else {
            any_fail = true;
            eprintln!("{}", mismatch_report(&rel, &pon, &reference));
            scoreboard.push((rel, "fail"));
        }
    }

    print_scoreboard(&scoreboard);
    if any_fail {
        bail!("one or more Phase-A slice scripts failed")
    }
    Ok(())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
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

fn is_arithmetic_script(path: &Path) -> bool {
    path.extension() == Some(OsStr::new("py"))
        && path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with("arithmetic"))
}

fn python314_available() -> bool {
    Command::new("python3.14")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn ensure_pon_cli(root: &Path) -> Result<PathBuf> {
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
        .join(format!("pon-cli{}", env::consts::EXE_SUFFIX));
    if !binary.is_file() {
        bail!("cargo build succeeded but `{}` was not created", binary.display());
    }
    Ok(binary)
}

fn target_dir(root: &Path) -> Result<PathBuf> {
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

fn run_pon(root: &Path, pon_binary: &Path, script: &Path) -> Result<RunResult> {
    run_command(
        Command::new(pon_binary)
            .arg("run")
            .arg(script)
            .current_dir(root),
    )
}

fn run_python314(root: &Path, script: &Path) -> Result<RunResult> {
    run_command(Command::new("python3.14").arg(script).current_dir(root))
}

fn run_command(command: &mut Command) -> Result<RunResult> {
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

fn display_path(root: &Path, script: &Path) -> String {
    script
        .strip_prefix(root)
        .unwrap_or(script)
        .to_string_lossy()
        .into_owned()
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

fn print_scoreboard(entries: &[(String, &str)]) {
    println!("{{");
    for (index, (path, status)) in entries.iter().enumerate() {
        let comma = if index + 1 == entries.len() { "" } else { "," };
        println!("  \"{path}\": \"{status}\"{comma}");
    }
    println!("}}");
}
