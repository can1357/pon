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
		r#"import sysconfig
cflags = sysconfig.get_config_var('CFLAGS')
print(type(cflags).__name__)
print(repr(cflags))
print(repr(sysconfig.get_config_var('Py_GIL_DISABLED')))
print(repr(sysconfig.get_config_var('no_such_var_probe')))
"#,
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
	assert_eq!(lines[2], "1", "pon reports Py_GIL_DISABLED=1 (no-GIL runtime)");
	assert_eq!(lines[3], "None");
}

#[test]
fn run_exposes_pon_debug_module() {
	let fixture_dir = TempDir::new("pon-run-debug-module");
	let fixture_path = fixture_dir.path().join("debug_module.py");
	fs::write(
		&fixture_path,
		r#"import pon

def add(a, b):
    total = a + b
    return total

class Accumulator:
    def method(self, value):
        total = value + 1
        return total

def shaped(a, b=2, *args, c=3, **kw):
    return a + b + c + len(args) + len(kw)

def known_tier(value):
    return (
        type(value) is str
        and (
            value == "tier0"
            or value == "queued"
            or value == "tier1"
            or value == "deferred"
            or value == "disabled"
        )
    )

def non_negative_int(value):
    return type(value) is int and value >= 0

def optional_non_negative_int(value):
    return value is None or non_negative_int(value)

def known_tier_state(value):
    return type(value) is int and 0 <= value <= 4

def core_state_ok(state, expected_name, expected_positional_arity):
    return (
        type(state) is dict
        and state["name"] == expected_name
        and state["arity"] == expected_positional_arity
        and known_tier(state["tier"])
        and known_tier_state(state["tier_state"])
        and non_negative_int(state["hotness"])
        and non_negative_int(state["loop_hotness"])
        and non_negative_int(state["deopt_count"])
        and non_negative_int(state["tier_epoch"])
        and type(state["osr_installed"]) is bool
        and optional_non_negative_int(state["osr_loop_header"])
        and type(state["has_metadata"]) is bool
    )

def metadata_ok(
    state,
    expected_positional_arity,
    expected_keyword_only,
    expected_has_varargs,
    expected_has_varkw,
    expected_default_count,
    expected_kwdefault_count,
    expected_closure_count,
):
    return (
        state["has_metadata"] is True
        and non_negative_int(state["n_locals"])
        and state["n_locals"] >= expected_positional_arity + expected_keyword_only
        and non_negative_int(state["flags"])
        and state["positional_arity"] == expected_positional_arity
        and state["positional_only"] == 0
        and state["positional_or_keyword"] == expected_positional_arity
        and state["keyword_only"] == expected_keyword_only
        and state["has_varargs"] is expected_has_varargs
        and state["has_varkw"] is expected_has_varkw
        and state["default_count"] == expected_default_count
        and state["kwdefault_count"] == expected_kwdefault_count
        and state["closure_count"] == expected_closure_count
    )

ir = pon.ir(add)
clif = pon.clif(add)
asm = pon.asm(add)
print("IR_OK", "add(a, b)" in ir and "block0:" in ir and "BinaryOp" in ir and "return v" in ir)
print("CLIF_OK", "function u0:" in clif)
print("ASM_OK", len(asm) > 0 and "block0:" in asm)

add_tier = pon.tier(add)
add_state = pon.state(add)
print("TIER_OK", known_tier(add_tier) and add_tier == add_state["tier"])
print("STATE_OK", core_state_ok(add_state, "add", 2) and metadata_ok(add_state, 2, 0, False, False, 0, 0, 0))

bound_method = Accumulator().method
method_ir = pon.ir(bound_method)
print("METHOD_IR_OK", "method(self, value)" in method_ir and "BinaryOp" in method_ir)
method_tier = pon.tier(bound_method)
method_state = pon.state(bound_method)
print("METHOD_TIER_OK", known_tier(method_tier) and method_tier == method_state["tier"])
print("METHOD_STATE_OK", core_state_ok(method_state, "method", 2) and metadata_ok(method_state, 2, 0, False, False, 0, 0, 0))

shaped_tier = pon.tier(shaped)
shaped_state = pon.state(shaped)
print("SHAPED_TIER_OK", known_tier(shaped_tier) and shaped_tier == shaped_state["tier"])
print("SHAPED_STATE_OK", core_state_ok(shaped_state, "shaped", 2) and metadata_ok(shaped_state, 2, 1, True, True, 1, 1, 0))

try:
    pon.ir(1)
except TypeError as exc:
    print("TYPE_ERROR_OK", "expected a Python function, got int" in str(exc))
else:
    print("TYPE_ERROR_OK", False)

try:
    pon.asm(len)
except ValueError as exc:
    print("VALUE_ERROR_OK", "was not compiled by the pon JIT" in str(exc))
else:
    print("VALUE_ERROR_OK", False)
"#,
	)
	.expect("write debug_module.py fixture");

	let output = Command::new(env!("CARGO_BIN_EXE_pon"))
		.arg("run")
		.arg(&fixture_path)
		.output()
		.expect("run pon binary");

	assert!(
		output.status.success(),
		"debug module probe should exit successfully; status={:?}, stderr={}",
		output.status.code(),
		String::from_utf8_lossy(&output.stderr)
	);
	assert_eq!(output.stderr.as_slice(), b"");
	let stdout = String::from_utf8_lossy(&output.stdout);
	let lines = stdout.lines().collect::<Vec<_>>();
	assert_eq!(
		lines.as_slice(),
		&[
			"IR_OK True",
			"CLIF_OK True",
			"ASM_OK True",
			"TIER_OK True",
			"STATE_OK True",
			"METHOD_IR_OK True",
			"METHOD_TIER_OK True",
			"METHOD_STATE_OK True",
			"SHAPED_TIER_OK True",
			"SHAPED_STATE_OK True",
			"TYPE_ERROR_OK True",
			"VALUE_ERROR_OK True",
		],
		"unexpected debug module output: {stdout}"
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
