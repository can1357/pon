use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use crate::ratchet;
use crate::scoreboard::{Scoreboard, Status};
use crate::suite::{self, RunResult};

const SUITE_NAME: &str = "cpython-aot-subset";

/// Static scripts that can be built as closed AoT units: no eval/exec, no dynamic
/// imports, and no extension-module dependency. Increase `AOT_MIN_PASS_COUNT`
/// and populate `AOT_FLOOR_MODULES` when the orchestrated exit gate establishes
/// a new green floor.
const AOT_SUBSET: &[&str] = &[
    "pon-conformance/corpus/hello.py",
    "pon-conformance/corpus/arithmetic.py",
    "pon-conformance/corpus/import_util.py",
    "pon-conformance/corpus/from_util_import_name.py",
    "corpus/match.py",
    "corpus/dict_list_str_methods.py",
    "corpus/exceptions.py",
    "corpus/generators.py",
    "corpus/classes.py",
    "corpus/tstrings.py",
];

const AOT_MIN_PASS_COUNT: usize = 0;
const AOT_FLOOR_MODULES: &[&str] = &[];

pub fn run_aot_suite(root: &Path, requested_modules: &[PathBuf]) -> Result<Scoreboard> {
    let modules = aot_modules(root, requested_modules);
    let mut scoreboard = Scoreboard::new(SUITE_NAME, Some(suite::cpython_revision(root)));

    if modules.is_empty() {
        return Ok(scoreboard);
    }

    if !suite::python314_available() {
        for (label, _) in modules {
            scoreboard.push(
                label,
                Status::Unsupported,
                Some("python3.14 reference interpreter is not available".to_owned()),
            );
        }
        return Ok(scoreboard);
    }

    let pon_binary = suite::ensure_pon_cli(root)?;
    let run_dir = aot_run_dir(root)?;

    for (index, (label, script)) in modules.into_iter().enumerate() {
        let Some(script) = script else {
            scoreboard.push(label, Status::Unsupported, Some("AoT subset script is not present".to_owned()));
            continue;
        };

        let reference = match suite::run_python314(root, &script) {
            Ok(reference) => reference,
            Err(error) => {
                scoreboard.push(label, Status::Fail, Some(format!("failed to run python3.14: {error:#}")));
                continue;
            }
        };
        let jit = match suite::run_pon(root, &pon_binary, &script) {
            Ok(jit) => jit,
            Err(error) => {
                scoreboard.push(label, Status::Fail, Some(format!("failed to run pon run: {error:#}")));
                continue;
            }
        };

        let exe = run_dir.join(format!("{}-{}{}", index, safe_exe_stem(&label), env::consts::EXE_SUFFIX));
        let build = run_build(root, &pon_binary, &script, &exe)
            .with_context(|| format!("failed to spawn pon build for `{}`", script.display()))?;
        if build.exit != 0 {
            scoreboard.push(label, Status::Fail, Some(build_failure_report(&exe, &build)));
            continue;
        }
        if !exe.is_file() {
            scoreboard.push(
                label,
                Status::Fail,
                Some(format!("pon build succeeded but `{}` was not created", exe.display())),
            );
            continue;
        }

        let aot = match run_executable_without_pon(root, &exe) {
            Ok(aot) => aot,
            Err(error) => {
                scoreboard.push(label, Status::Fail, Some(format!("failed to run AoT executable without pon on PATH: {error:#}")));
                continue;
            }
        };

        let mut reports = Vec::new();
        if jit != reference {
            reports.push(diff_report(&label, "pon run", &jit, "python3.14", &reference));
        }
        if aot != reference {
            reports.push(diff_report(&label, "AoT executable", &aot, "python3.14", &reference));
        }
        if aot != jit {
            reports.push(diff_report(&label, "AoT executable", &aot, "pon run", &jit));
        }

        if reports.is_empty() {
            scoreboard.push(label, Status::Pass, None);
        } else {
            scoreboard.push(label, Status::Fail, Some(reports.join("\n")));
        }
    }

    Ok(scoreboard)
}

pub fn check_floor(scoreboard: &Scoreboard) -> ratchet::FloorCheck {
    let floor = ratchet::Floor {
        cpython_tag: suite::CPYTHON_TAG.to_owned(),
        passing_modules: AOT_FLOOR_MODULES.iter().map(|module| (*module).to_owned()).collect(),
        min_pass_count: AOT_MIN_PASS_COUNT,
    };
    ratchet::check_floor(&floor, scoreboard)
}

fn aot_modules(root: &Path, requested_modules: &[PathBuf]) -> Vec<(String, Option<PathBuf>)> {
    if requested_modules.is_empty() {
        return AOT_SUBSET.iter().map(|module| resolve_aot_module(root, Path::new(module))).collect();
    }
    requested_modules.iter().map(|module| resolve_aot_module(root, module)).collect()
}

fn resolve_aot_module(root: &Path, module: &Path) -> (String, Option<PathBuf>) {
    if module.is_absolute() {
        return (suite::display_path(root, module), module.is_file().then(|| module.to_path_buf()));
    }

    let workspace_candidate = root.join(module);
    if workspace_candidate.is_file() {
        return (suite::normalize_path(module), Some(workspace_candidate));
    }

    if module.extension().is_none() {
        let cpython_test = PathBuf::from("Lib").join("test").join(format!("{}.py", module.to_string_lossy()));
        return suite::resolve_cpython_module(root, &cpython_test);
    }

    suite::resolve_cpython_module(root, module)
}

fn aot_run_dir(root: &Path) -> Result<PathBuf> {
    let dir = root
        .join("target")
        .join("pon-conformance-aot")
        .join(std::process::id().to_string());
    if dir.exists() {
        fs::remove_dir_all(&dir).with_context(|| format!("failed to clear `{}`", dir.display()))?;
    }
    fs::create_dir_all(&dir).with_context(|| format!("failed to create `{}`", dir.display()))?;
    Ok(dir)
}

fn run_build(root: &Path, pon_binary: &Path, script: &Path, exe: &Path) -> Result<RunResult> {
    suite::run_command(
        Command::new(pon_binary)
            .arg("build")
            .arg(script)
            .arg("-o")
            .arg(exe)
            .current_dir(root),
    )
}

fn run_executable_without_pon(root: &Path, exe: &Path) -> Result<RunResult> {
    let mut command = Command::new(exe);
    command.env_clear().env("PATH", "/usr/bin:/bin").current_dir(root);
    if let Some(home) = env::var_os("HOME") {
        command.env("HOME", home);
    }
    suite::run_command(&mut command)
}

fn build_failure_report(exe: &Path, build: &RunResult) -> String {
    format!(
        "pon build failed for output `{}`: exit={} stdout={:?} stderr={:?}",
        exe.display(),
        build.exit,
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr),
    )
}

fn diff_report(path: &str, actual_label: &str, actual: &RunResult, expected_label: &str, expected: &RunResult) -> String {
    format!(
        "FAIL {path}\n  {actual_label}: exit={} stdout={:?} stderr={:?}\n  {expected_label}: exit={} stdout={:?} stderr={:?}",
        actual.exit,
        String::from_utf8_lossy(&actual.stdout),
        String::from_utf8_lossy(&actual.stderr),
        expected.exit,
        String::from_utf8_lossy(&expected.stdout),
        String::from_utf8_lossy(&expected.stderr),
    )
}

fn safe_exe_stem(label: &str) -> String {
    let stem = label
        .chars()
        .map(|character| if character.is_ascii_alphanumeric() { character } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    if stem.is_empty() { "script".to_owned() } else { stem }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    const NO_CLASS_HELLO: &str = r#"
def add(a, b):
    return a + b

print("hello, world")
print(add(2, 3))
"#;

    #[test]
    fn no_class_module_executes_aot_body_not_build_class() {
        let root = suite::workspace_root();
        let fixture_dir = TempDir::new("pon-conformance-aot-no-class");
        let script = fixture_dir.path().join("hello_no_class.py");
        std::fs::write(&script, NO_CLASS_HELLO).expect("write no-class AoT fixture");

        let exe = fixture_dir.path().join(format!(
            "hello_no_class{}",
            std::env::consts::EXE_SUFFIX
        ));
        let pon_binary = suite::ensure_pon_cli(&root).expect("build pon-cli for AoT regression");

        let build = run_build(&root, &pon_binary, &script, &exe).expect("spawn pon build");
        assert_eq!(
            build.exit,
            0,
            "no-class AoT build failed: stdout={} stderr={}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        assert!(exe.is_file(), "pon build did not create {}", exe.display());

        let aot = run_executable_without_pon(&root, &exe).expect("run no-class AoT executable");
        let stderr = String::from_utf8_lossy(&aot.stderr);
        assert!(
            !stderr.contains("__build_class__"),
            "no-class AoT module main must not invoke __build_class__; stderr={stderr}"
        );
        assert_eq!(
            aot.exit, 0,
            "no-class AoT executable failed: stdout={} stderr={stderr}",
            String::from_utf8_lossy(&aot.stdout)
        );
        assert_eq!(aot.stdout.as_slice(), b"hello, world\n5\n");
        assert_eq!(aot.stderr.as_slice(), b"");
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

            for _ in 0..1000 {
                let suffix = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!("{prefix}-{}-{suffix}", std::process::id()));

                match std::fs::create_dir(&path) {
                    Ok(()) => return Self { path },
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(err) => panic!("create test fixture directory {path:?}: {err}"),
                }
            }

            panic!("could not create a unique temporary directory for {prefix}");
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
