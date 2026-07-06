use std::{
	fs,
	io::{self, Write},
	path::{Path, PathBuf},
	process::{Command, Output, Stdio},
	sync::atomic::{AtomicU64, Ordering},
};

fn pon_command() -> Command {
	Command::new(env!("CARGO_BIN_EXE_pon"))
}

fn run_pon(args: &[&str]) -> Output {
	pon_command().args(args).output().expect("run pon binary")
}

fn run_pon_with_stdin(args: &[&str], stdin: &str) -> Output {
	let mut child = pon_command()
		.args(args)
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.expect("spawn pon binary");

	child
		.stdin
		.as_mut()
		.expect("open pon stdin")
		.write_all(stdin.as_bytes())
		.expect("write pon stdin");

	child.wait_with_output().expect("wait for pon binary")
}

fn stdout(output: &Output) -> String {
	String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
	String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn no_args_prints_root_help_with_runtime_and_project_commands() {
	let output = run_pon(&[]);

	assert!(
		output.status.success(),
		"no-arg help should exit successfully; status={:?}, stdout={}, stderr={}",
		output.status.code(),
		stdout(&output),
		stderr(&output)
	);
	let stdout = stdout(&output);
	assert!(stdout.contains("Usage: pon"), "stdout should contain root usage; stdout={stdout}");
	assert!(stdout.contains("install"), "stdout should list package install; stdout={stdout}");
	assert!(stdout.contains("repl"), "stdout should list the REPL command; stdout={stdout}");
	assert!(stdout.contains("build"), "stdout should list the AoT build command; stdout={stdout}");
	assert_eq!(output.stderr.as_slice(), b"");
}

#[test]
fn version_prints_package_version() {
	let output = run_pon(&["--version"]);

	assert!(
		output.status.success(),
		"--version should exit successfully; status={:?}, stdout={}, stderr={}",
		output.status.code(),
		stdout(&output),
		stderr(&output)
	);
	assert_eq!(stdout(&output).trim(), "pon 0.1.0");
	assert_eq!(output.stderr.as_slice(), b"");
}

#[test]
fn inline_code_receives_cpython_style_argv() {
	let output = run_pon(&["-c", "import sys; print(sys.argv)", "a", "b"]);

	assert!(
		output.status.success(),
		"-c argv probe should exit successfully; status={:?}, stdout={}, stderr={}",
		output.status.code(),
		stdout(&output),
		stderr(&output)
	);
	assert_eq!(stdout(&output).trim(), "['-c', 'a', 'b']");
	assert_eq!(output.stderr.as_slice(), b"");
}

#[test]
fn inline_code_executes_expression_statement_program() {
	let output = run_pon(&["-c", "print(40 + 2)"]);

	assert!(
		output.status.success(),
		"-c arithmetic program should exit successfully; status={:?}, stdout={}, stderr={}",
		output.status.code(),
		stdout(&output),
		stderr(&output)
	);
	assert_eq!(stdout(&output).trim(), "42");
	assert_eq!(output.stderr.as_slice(), b"");
}

#[test]
fn inline_code_zero_arg_super_tracks_lexical_owner() {
	let output = run_pon(&[
		"-c",
		"from enum import Enum\nfrom collections import namedtuple\nclass ASN(namedtuple(\"ASN\", \"nid shortname longname oid\")):\n    __slots__ = ()\n    def __new__(cls, oid):\n        return super().__new__(cls, 1, 'short', 'long', oid)\nclass Purpose(ASN, Enum):\n    SERVER_AUTH = '1.2.3'\nprint(Purpose.SERVER_AUTH.oid)",
	]);

	assert!(
		output.status.success(),
		"zero-arg super enum/namedtuple probe should succeed; status={:?}, stdout={}, stderr={}",
		output.status.code(),
		stdout(&output),
		stderr(&output)
	);
	assert_eq!(stdout(&output).trim(), "1.2.3");
	assert_eq!(output.stderr.as_slice(), b"");
}

#[test]
fn inline_code_does_not_materialize_a_file_attribute() {
	let output = run_pon(&["-c", "print(__file__)"]);

	assert!(
		!output.status.success(),
		"printing __file__ in inline code should fail; status={:?}, stdout={}, stderr={}",
		output.status.code(),
		stdout(&output),
		stderr(&output)
	);
	let stderr = stderr(&output);
	assert!(stderr.contains("__file__"), "stderr should name missing __file__; stderr={stderr}");
	let stdout = stdout(&output);
	assert!(
		!stdout.contains(".py"),
		"inline execution should not expose a synthetic .py path on stdout; stdout={stdout}"
	);
}

#[test]
fn inline_imports_resolve_from_cwd_before_any_other_path() {
	let fixture_dir = TempDir::new("pon-surface-inline-import");
	fs::write(fixture_dir.path().join("helper.py"), "MARKER = 'inline-import-from-cwd'\n")
		.expect("write helper.py fixture");

	let output = pon_command()
		.arg("-c")
		.arg("import helper; print(helper.MARKER)")
		.current_dir(fixture_dir.path())
		.output()
		.expect("run pon binary");

	assert!(
		output.status.success(),
		"-c import should resolve helper.py from cwd; status={:?}, stdout={}, stderr={}",
		output.status.code(),
		stdout(&output),
		stderr(&output)
	);
	assert_eq!(stdout(&output).trim(), "inline-import-from-cwd");
	assert_eq!(output.stderr.as_slice(), b"");
}

#[test]
fn stdin_program_executes_and_preserves_dash_argv_zero() {
	let output = run_pon_with_stdin(&["-"], "print(6 * 7)\n");

	assert!(
		output.status.success(),
		"stdin arithmetic program should exit successfully; status={:?}, stdout={}, stderr={}",
		output.status.code(),
		stdout(&output),
		stderr(&output)
	);
	assert_eq!(stdout(&output).trim(), "42");
	assert_eq!(output.stderr.as_slice(), b"");

	let output = run_pon_with_stdin(&["-", "extra"], "import sys; print(sys.argv[0])\n");

	assert!(
		output.status.success(),
		"stdin argv probe should exit successfully; status={:?}, stdout={}, stderr={}",
		output.status.code(),
		stdout(&output),
		stderr(&output)
	);
	assert_eq!(stdout(&output).trim(), "-");
	assert_eq!(output.stderr.as_slice(), b"");
}

#[test]
fn existing_script_path_dispatches_without_run_subcommand_and_allows_args() {
	let fixture_dir = TempDir::new("pon-surface-script-dispatch");
	let script = fixture_dir.path().join("t.py");
	fs::write(&script, "print('script-dispatch-marker')\n").expect("write t.py fixture");

	let output = pon_command()
		.arg(&script)
		.arg("extra")
		.output()
		.expect("run pon binary");

	assert!(
		output.status.success(),
		"script path dispatch should run successfully; status={:?}, stdout={}, stderr={}",
		output.status.code(),
		stdout(&output),
		stderr(&output)
	);
	assert_eq!(stdout(&output).trim(), "script-dispatch-marker");
	assert_eq!(output.stderr.as_slice(), b"");
}

#[test]
fn run_file_in_markerless_directory_does_not_create_project_environment() {
	let fixture_dir = TempDir::new("pon-surface-markerless-run");
	fs::write(fixture_dir.path().join("app.py"), "print('markerless-run-ok')\n")
		.expect("write app.py fixture");

	let output = pon_command()
		.arg("run")
		.arg("app.py")
		.current_dir(fixture_dir.path())
		.output()
		.expect("run pon binary");

	assert!(
		output.status.success(),
		"pon run app.py in markerless cwd should run successfully; status={:?}, stdout={}, stderr={}",
		output.status.code(),
		stdout(&output),
		stderr(&output)
	);
	assert_eq!(stdout(&output).trim(), "markerless-run-ok");
	assert_eq!(output.stderr.as_slice(), b"");
	assert!(
		!fixture_dir.path().join(".pon").exists(),
		"markerless pon run should not create .pon/ in {:?}",
		fixture_dir.path()
	);
}

#[test]
fn repl_piped_input_preserves_state_between_entries() {
	let output = run_pon_with_stdin(&["repl"], "1 + 1\n_\nif True:\n    y = 40\ny + _\n");

	assert!(
		output.status.success(),
		"piped repl should exit successfully; status={:?}, stdout={}, stderr={}",
		output.status.code(),
		stdout(&output),
		stderr(&output)
	);
	let stdout = stdout(&output);
	assert!(
		stdout.contains("\n2\n2\n42\n") || stdout.ends_with("\n2\n2\n42\n"),
		"repl should bind `_` and continue multiline input until parse-complete; stdout={stdout}"
	);
	assert_eq!(output.stderr.as_slice(), b"");
}


#[test]
fn package_manager_no_index_installs_from_find_links() {
	let fixture_dir = TempDir::new("pon-surface-pkg-no-index");
	let manifest = fixture_dir.path().join("pyproject.toml");
	let init = pon_command()
		.arg("init")
		.arg("--manifest")
		.arg(&manifest)
		.output()
		.expect("pon init");
	assert!(
		init.status.success(),
		"pon init should succeed; status={:?}, stdout={}, stderr={}",
		init.status.code(),
		stdout(&init),
		stderr(&init)
	);
	let wheelhouse = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("fixtures")
		.join("wheels");

	let install = pon_command()
		.arg("install")
		.arg("idna")
		.arg("--manifest")
		.arg(&manifest)
		.arg("--no-index")
		.arg("--find-links")
		.arg(&wheelhouse)
		.output()
		.expect("pon install");
	assert!(
		install.status.success(),
		"pon install should resolve from find-links without index access; status={:?}, stdout={}, stderr={}",
		install.status.code(),
		stdout(&install),
		stderr(&install)
	);

	let run = pon_command()
		.arg("run")
		.arg("-c")
		.arg("import idna; print(idna.__version__)")
		.current_dir(fixture_dir.path())
		.output()
		.expect("pon run");
	assert!(
		run.status.success(),
		"pon run should import package installed from find-links; status={:?}, stdout={}, stderr={}",
		run.status.code(),
		stdout(&run),
		stderr(&run)
	);
	assert_eq!(stdout(&run).trim(), "3.10");
	assert_eq!(run.stderr.as_slice(), b"");
}
#[test]
fn package_manager_catalog_e2e() {
	let fixture_dir = TempDir::new("pon-surface-pkg-e2e");
	let manifest = fixture_dir.path().join("pyproject.toml");
	let init = pon_command()
		.arg("init")
		.arg("--manifest")
		.arg(&manifest)
		.output()
		.expect("pon init");
	assert!(
		init.status.success(),
		"pon init should succeed; status={:?}, stdout={}, stderr={}",
		init.status.code(),
		stdout(&init),
		stderr(&init)
	);

	let add = pon_command()
		.arg("add")
		.arg("idna")
		.arg("--manifest")
		.arg(&manifest)
		.arg("--index-url")
		.arg("catalog:")
		.env("PON_TEST_ALLOW_CATALOG", "1")
		.output()
		.expect("pon add");
	assert!(
		add.status.success(),
		"pon add should install from catalog fixture; status={:?}, stdout={}, stderr={}",
		add.status.code(),
		stdout(&add),
		stderr(&add)
	);

	let run = pon_command()
		.arg("run")
		.arg("-c")
		.arg("import idna; print(idna.__version__)")
		.current_dir(fixture_dir.path())
		.output()
		.expect("pon run");
	assert!(
		run.status.success(),
		"pon run should import installed package; status={:?}, stdout={}, stderr={}",
		run.status.code(),
		stdout(&run),
		stderr(&run)
	);
	assert_eq!(stdout(&run).trim(), "3.10");
	assert_eq!(run.stderr.as_slice(), b"");
}

#[test]
fn repl_sys_exit_controls_process_status() {
	let output = run_pon_with_stdin(&["repl"], "import sys\nsys.exit(7)\n");

	assert_eq!(
		output.status.code(),
		Some(7),
		"sys.exit(7) at the repl should become process exit 7; stdout={}, stderr={}",
		stdout(&output),
		stderr(&output)
	);
}

#[test]
fn unknown_subcommand_exits_two_and_reports_command_usage() {
	let output = run_pon(&["frobnicate"]);

	assert_eq!(
		output.status.code(),
		Some(2),
		"unknown subcommand should use clap usage-error exit 2; stdout={}, stderr={}",
		stdout(&output),
		stderr(&output)
	);
	assert_eq!(output.stdout.as_slice(), b"");
	let stderr = stderr(&output);
	assert!(stderr.contains("frobnicate"), "stderr should name the bad subcommand; stderr={stderr}");
	assert!(stderr.contains("Usage: pon"), "stderr should include command usage; stderr={stderr}");
	assert!(
		stderr.contains("<COMMAND>") || stderr.contains("run") || stderr.contains("install"),
		"stderr should mention the available command surface; stderr={stderr}"
	);
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

			match fs::create_dir(&path) {
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
		let _ = fs::remove_dir_all(&self.path);
	}
}
