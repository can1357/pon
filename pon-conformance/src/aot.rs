use std::env;
use std::collections::BTreeMap;
use std::fs;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use crate::ratchet;
use crate::scoreboard::{Scoreboard, Status};
use crate::suite::{self, RunResult};

const AOT_SUBSET_SUITE_NAME: &str = "cpython-aot-subset";
pub const AOT_PARITY_SUITE_NAME: &str = "aot-parity";
pub const AOT_PARITY_RESULTS_FILE: &str = "aot-parity.json";
pub const AOT_PARITY_FLOOR_FILE: &str = "aot-parity-floor.json";

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AotParityStatus {
    Pass,
    Fail,
    Refused,
    Error,
}

impl AotParityStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "aot-pass",
            Self::Fail => "aot-fail",
            Self::Refused => "aot-refused",
            Self::Error => "aot-error",
        }
    }

    fn scoreboard_status(self) -> Status {
        match self {
            Self::Pass => Status::Pass,
            Self::Fail | Self::Error => Status::Fail,
            Self::Refused => Status::Unsupported,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AotParityRecord {
    module: String,
    status: AotParityStatus,
    detail: Option<String>,
}

impl AotParityRecord {
    fn new(module: String, status: AotParityStatus, detail: Option<String>) -> Self {
        Self { module, status, detail }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AotParityReport {
    scoreboard: Scoreboard,
    records: Vec<AotParityRecord>,
}

impl AotParityReport {
    fn new(cpython_tag: String) -> Self {
        Self {
            scoreboard: Scoreboard::new(AOT_PARITY_SUITE_NAME, Some(cpython_tag)),
            records: Vec::new(),
        }
    }

    pub fn scoreboard(&self) -> &Scoreboard {
        &self.scoreboard
    }

    fn push(&mut self, record: AotParityRecord) {
        self.scoreboard
            .push(record.module.clone(), record.status.scoreboard_status(), record.detail.clone());
        self.records.push(record);
    }

    fn status_count(&self, status: AotParityStatus) -> usize {
        self.records.iter().filter(|record| record.status == status).count()
    }

    fn refusal_buckets(&self) -> Vec<(String, Vec<String>)> {
        let mut buckets = BTreeMap::<String, Vec<String>>::new();
        for record in &self.records {
            if record.status == AotParityStatus::Refused {
                let message = record.detail.as_deref().unwrap_or("AoT compiler refused").to_owned();
                buckets.entry(message).or_default().push(record.module.clone());
            }
        }

        let mut buckets = buckets.into_iter().collect::<Vec<_>>();
        for (_, modules) in &mut buckets {
            modules.sort();
        }
        buckets.sort_by(|(left_message, left_modules), (right_message, right_modules)| {
            right_modules
                .len()
                .cmp(&left_modules.len())
                .then_with(|| left_message.cmp(right_message))
        });
        buckets
    }

    pub fn to_json(&self) -> String {
        let mut json = String::new();
        json.push_str("{\n");
        write!(json, "  \"suite\": \"{}\"", escape_json(&self.scoreboard.suite)).expect("write to String cannot fail");
        if let Some(tag) = &self.scoreboard.cpython_tag {
            write!(json, ",\n  \"cpython_tag\": \"{}\"", escape_json(tag)).expect("write to String cannot fail");
        }
        json.push_str(",\n  \"results_file\": \"");
        json.push_str(AOT_PARITY_RESULTS_FILE);
        json.push_str("\",\n  \"summary\": {\n");
        write!(
            json,
            "    \"aot-pass\": {},\n    \"aot-fail\": {},\n    \"aot-refused\": {},\n    \"aot-error\": {}\n",
            self.status_count(AotParityStatus::Pass),
            self.status_count(AotParityStatus::Fail),
            self.status_count(AotParityStatus::Refused),
            self.status_count(AotParityStatus::Error),
        )
        .expect("write to String cannot fail");
        json.push_str("  },\n  \"refusal_buckets\": [");
        let refusal_buckets = self.refusal_buckets();
        if !refusal_buckets.is_empty() {
            json.push('\n');
        }
        for (index, (message, modules)) in refusal_buckets.iter().enumerate() {
            let comma = if index + 1 == refusal_buckets.len() { "" } else { "," };
            write!(
                json,
                "    {{ \"message\": \"{}\", \"count\": {}, \"modules\": [",
                escape_json(message),
                modules.len(),
            )
            .expect("write to String cannot fail");
            for (module_index, module) in modules.iter().enumerate() {
                let module_comma = if module_index + 1 == modules.len() { "" } else { ", " };
                write!(json, "\"{}\"{module_comma}", escape_json(module)).expect("write to String cannot fail");
            }
            write!(json, "] }}{comma}\n").expect("write to String cannot fail");
        }
        json.push_str("  ],\n  \"records\": [");
        if !self.records.is_empty() {
            json.push('\n');
        }
        for (index, record) in self.records.iter().enumerate() {
            let comma = if index + 1 == self.records.len() { "" } else { "," };
            write!(
                json,
                "    {{ \"module\": \"{}\", \"status\": \"{}\"",
                escape_json(&record.module),
                record.status.as_str(),
            )
            .expect("write to String cannot fail");
            if let Some(detail) = &record.detail {
                write!(json, ", \"detail\": \"{}\"", escape_json(detail)).expect("write to String cannot fail");
            }
            write!(json, " }}{comma}\n").expect("write to String cannot fail");
        }
        json.push_str("  ]\n}\n");
        json
    }
}

pub fn run_aot_suite(root: &Path, requested_modules: &[PathBuf]) -> Result<Scoreboard> {
    let modules = aot_modules(root, requested_modules);
    let mut scoreboard = Scoreboard::new(AOT_SUBSET_SUITE_NAME, Some(suite::cpython_revision(root)));

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

pub fn run_aot_parity_suite(root: &Path, requested_modules: &[PathBuf]) -> Result<AotParityReport> {
    let modules = aot_parity_modules(root, requested_modules)?;
    let mut report = AotParityReport::new(suite::cpython_revision(root));

    if modules.is_empty() {
        return Ok(report);
    }

    let pon_binary = suite::ensure_pon_cli(root)?;
    let run_dir = aot_run_dir(root)?;
    let python_available = suite::python314_available();

    for (index, (label, script)) in modules.into_iter().enumerate() {
        let record = classify_aot_parity_module(
            root,
            &pon_binary,
            &run_dir,
            index,
            label,
            script,
            python_available,
        );
        report.push(record);
    }

    Ok(report)
}

pub fn write_aot_parity_results(root: &Path, report: &AotParityReport) -> Result<()> {
    let path = root.join(AOT_PARITY_RESULTS_FILE);
    fs::write(&path, report.to_json()).with_context(|| format!("failed to write `{}`", path.display()))
}

fn aot_parity_modules(root: &Path, requested_modules: &[PathBuf]) -> Result<Vec<(String, Option<PathBuf>)>> {
    if requested_modules.is_empty() {
        return Ok(suite::cpython_manifest_modules(root)?
            .into_iter()
            .map(|module| resolve_aot_module(root, &module))
            .collect());
    }
    Ok(requested_modules.iter().map(|module| resolve_aot_module(root, module)).collect())
}

fn classify_aot_parity_module(
    root: &Path,
    pon_binary: &Path,
    run_dir: &Path,
    index: usize,
    label: String,
    script: Option<PathBuf>,
    python_available: bool,
) -> AotParityRecord {
    let Some(script) = script else {
        return AotParityRecord::new(label, AotParityStatus::Error, Some("manifest entry is not present".to_owned()));
    };

    if !python_available {
        return AotParityRecord::new(
            label,
            AotParityStatus::Error,
            Some("python3.14 reference interpreter is not available".to_owned()),
        );
    }

    let reference = match suite::run_python314(root, &script) {
        Ok(reference) => reference,
        Err(error) => {
            return AotParityRecord::new(label, AotParityStatus::Error, Some(format!("failed to run python3.14: {error:#}")));
        }
    };

    let exe = run_dir.join(format!("{}-{}{}", index, safe_exe_stem(&label), env::consts::EXE_SUFFIX));
    let build = match run_build(root, pon_binary, &script, &exe) {
        Ok(build) => build,
        Err(error) => {
            return AotParityRecord::new(label, AotParityStatus::Error, Some(format!("failed to spawn pon build: {error:#}")));
        }
    };
    if build.exit != 0 {
        if suite::is_unsupported_pon_output(build.exit, &build.stderr) {
            return AotParityRecord::new(label, AotParityStatus::Refused, Some(refusal_message(&build)));
        }
        return AotParityRecord::new(label, AotParityStatus::Error, Some(build_failure_report(&exe, &build)));
    }
    if !exe.is_file() {
        return AotParityRecord::new(
            label,
            AotParityStatus::Error,
            Some(format!("pon build succeeded but `{}` was not created", exe.display())),
        );
    }

    let aot = match run_executable_without_pon(root, &exe) {
        Ok(aot) => aot,
        Err(error) => {
            return AotParityRecord::new(
                label,
                AotParityStatus::Error,
                Some(format!("failed to run AoT executable without pon on PATH: {error:#}")),
            );
        }
    };

    if is_runtime_panic(&aot) {
        return AotParityRecord::new(
            label.clone(),
            AotParityStatus::Error,
            Some(diff_report(&label, "AoT executable", &aot, "python3.14", &reference)),
        );
    }

    if aot == reference {
        AotParityRecord::new(label, AotParityStatus::Pass, None)
    } else {
        AotParityRecord::new(
            label.clone(),
            AotParityStatus::Fail,
            Some(diff_report(&label, "AoT executable", &aot, "python3.14", &reference)),
        )
    }
}

fn refusal_message(build: &RunResult) -> String {
    String::from_utf8_lossy(&build.stderr)
        .lines()
        .find(|line| line.to_ascii_lowercase().contains("unsupported"))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| "AoT compiler refused".to_owned())
}

fn is_runtime_panic(result: &RunResult) -> bool {
    let stderr = String::from_utf8_lossy(&result.stderr).to_ascii_lowercase();
    stderr.contains("panicked at") || (stderr.contains("thread '") && stderr.contains("panicked"))
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

fn escape_json(value: &str) -> String {
    let mut escaped = String::new();
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                write!(escaped, "\\u{:04x}", character as u32).expect("write to String cannot fail");
            }
            character => escaped.push(character),
        }
    }
    escaped
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
