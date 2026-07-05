use std::{
	fs, io,
	path::{Path, PathBuf},
	process::Command,
	sync::atomic::{AtomicU64, Ordering},
};

const PHASE_A_HELLO: &str = r#"
def add(a, b):
    return a + b

print("hello, world")
print(add(2, 3))
"#;

#[test]
fn run_executes_hello_fixture() {
	let fixture_dir = TempDir::new("pon-run-hello");
	let fixture_path = fixture_dir.path().join("hello.py");
	fs::write(&fixture_path, PHASE_A_HELLO).expect("write hello.py fixture");

	let output = Command::new(env!("CARGO_BIN_EXE_pon"))
		.arg("run")
		.arg(&fixture_path)
		.output()
		.expect("run pon binary");

	assert!(
		output.status.success(),
		"pon run should exit successfully; status={:?}, stderr={}",
		output.status.code(),
		String::from_utf8_lossy(&output.stderr)
	);
	assert_eq!(output.stdout.as_slice(), b"hello, world\n5\n");
	assert_eq!(output.stderr.as_slice(), b"");
}

/// pon's build-time config module (`_sysconfigdata__<platform>_`, the
/// generated-at-build-time module CPython's `sysconfig._init_posix` imports)
/// is served by the curated native registry.  This pins the CT test.support
/// ladder step: `sysconfig.get_config_var('CFLAGS')` must return a str
/// instead of raising ModuleNotFoundError out of `get_config_vars()`.
#[test]
fn run_serves_sysconfig_build_time_config() {
	let fixture_dir = TempDir::new("pon-run-sysconfig");
	let fixture_path = fixture_dir.path().join("sysconfig_probe.py");
	fs::write(
		&fixture_path,
		"import sysconfig\ncflags = \
		 sysconfig.get_config_var('CFLAGS')\nprint(type(cflags).__name__)\nprint(repr(cflags))\\
		 nprint(repr(sysconfig.get_config_var('Py_GIL_DISABLED')))\nprint(repr(sysconfig.get_config_var('\
		 no_such_var_probe')))\n",
	)
	.expect("write sysconfig_probe.py fixture");

	let stdlib =
		Path::new(env!("CARGO_MANIFEST_DIR")).join("../pon-conformance/vendor/cpython-3.14/Lib");
	let output = Command::new(env!("CARGO_BIN_EXE_pon"))
		.arg("run")
		.arg(&fixture_path)
		.env("PON_STDLIB_PATH", &stdlib)
		.env("PONPATH", &stdlib)
		.output()
		.expect("run pon binary");

	assert!(
		output.status.success(),
		"sysconfig probe should exit successfully; status={:?}, stderr={}",
		output.status.code(),
		String::from_utf8_lossy(&output.stderr)
	);
	let stdout = String::from_utf8_lossy(&output.stdout);
	let lines = stdout.lines().collect::<Vec<_>>();
	assert_eq!(lines.len(), 4, "unexpected sysconfig probe output: {stdout}");
	assert_eq!(lines[0], "str");
	assert!(
		lines[1].starts_with('\'') && lines[1].ends_with('\''),
		"CFLAGS repr should be a Python string repr: {stdout}"
	);
	assert_eq!(lines[2], "0");
	assert_eq!(lines[3], "None");
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
