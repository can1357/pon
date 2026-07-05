//! Native `_testsinglephase` shim (owner-sanctioned, ledger-triage wave 6).
//!
//! CPython's `_testsinglephase` is a C test extension
//! (`Modules/_testsinglephase.c`) exercising single-phase extension-module
//! initialization.  pon loads no C extensions, but the module's *absence*
//! kills meaningful pure-Python coverage: `test.test_importlib.util:18` runs
//! `import_helper.import_module("_testsinglephase")` at module scope, so
//! every unit importing that shared helper dies with `SkipTest: No module
//! named '_testsinglephase'` before reaching its actual subject — the
//! `test_importlib.import_.*`/`source.*`/`builtin.*` import-machinery units
//! plus `test_api`/`test_locks`/`test_spec`/`test_util` and the
//! `test_pyclbr` chain (23 gray units, `local://exclusion-proposal.md`
//! §Gray).
//!
//! Serving the module as a pon builtin is exactly the static-build path
//! upstream already supports: `util.py:38-63` probes `sys.path` for a
//! `_testsinglephase` *file* and leaves `EXTENSIONS.file_path = None` when
//! none exists, and the `extension.*` loader tests skip themselves when the
//! name appears in `sys.builtin_module_names` (`extension/test_loader.py:24`,
//! `extension/test_finder.py:17`) — which it does here via the
//! [`super::NATIVE_MODULES`] row.
//!
//! Surface policy (minimal honest, J0.4):
//!
//! * The 23 gray units consume only *existence*; nothing else is required.
//! * `int_const`/`str_const` are the module's definition-time constants,
//!   verbatim from `Modules/_testsinglephase.c` (`1969`, `"something
//!   different"`).
//! * `initialized()`/`initialized_count()` answer with pon's real module
//!   lifecycle: the wall-clock time the factory first ran and the number of
//!   factory runs.  pon creates a native module once per process and caches it,
//!   so the count is `1` — honestly reflecting that pon has no extension
//!   re-initialization machinery (CPython's counter exists to observe repeated
//!   single-phase init).
//! * Everything else (`sum`, `look_up_self`, `state_initialized`, `error`, the
//!   `_with_reinit`/`_with_state` variants) is deliberately absent: those names
//!   only matter to the excluded `c-abi-boundary` families, and an un-shimmed
//!   access must fail loudly as `AttributeError` naming this module rather than
//!   return a fabricated value.

use std::{
	sync::{
		LazyLock,
		atomic::{AtomicU64, Ordering},
	},
	time::{SystemTime, UNIX_EPOCH},
};

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{abi, intern::intern, object::PyObject, types::exc::ExceptionKind};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

/// Wall-clock seconds at first module initialization; forced by
/// [`make_module`], so the value pins the moment the module factory ran.
static INIT_TIME: LazyLock<f64> = LazyLock::new(now_seconds);

/// Number of times the module factory ran in this process (pon caches the
/// module after the first import, so this stays `1` outside of factory
/// re-entry, which pon does not perform).
static INIT_COUNT: AtomicU64 = AtomicU64::new(0);

fn now_seconds() -> f64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|elapsed| elapsed.as_secs_f64())
		.unwrap_or(0.0)
}

/// Zero-argument entry points sharing the arity contract.
macro_rules! noargs_guard {
	($name:literal, $argc:expr) => {
		if $argc != 0 {
			return abi::exc::raise_kind_error_text(
				ExceptionKind::TypeError,
				&format!(concat!("_testsinglephase.", $name, " expected 0 arguments, got {}"), $argc),
			);
		}
	};
}

/// `initialized()`: wall-clock time (seconds since the epoch) recorded when
/// the module factory ran.  CPython returns its module-state init timestamp;
/// pon's analogue is the factory-run instant.
unsafe extern "C" fn initialized_entry(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	noargs_guard!("initialized", argc);
	// SAFETY: Float constant allocator; NULL propagates with the error set.
	unsafe { abi::number::pon_const_float(*INIT_TIME) }
}

/// `initialized_count()`: how many times this module initialized.  pon
/// native modules initialize exactly once per process (the import cache
/// serves every later import), so the honest answer is the factory-run
/// count — `1`.
unsafe extern "C" fn initialized_count_entry(
	_argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	noargs_guard!("initialized_count", argc);
	let count = i64::try_from(INIT_COUNT.load(Ordering::Relaxed)).unwrap_or(i64::MAX);
	// SAFETY: Int constant allocator; NULL propagates with the error set.
	unsafe { abi::pon_const_int(count) }
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	INIT_COUNT.fetch_add(1, Ordering::Relaxed);
	// Pin the init timestamp to the first factory run.
	let _ = *INIT_TIME;

	let name = "_testsinglephase";
	// SAFETY: Runtime allocation helper; NULL is checked below.
	let name_obj = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
	if name_obj.is_null() {
		return Err("failed to allocate _testsinglephase.__name__".to_owned());
	}
	let doc = "pon-native shim for CPython's _testsinglephase C test extension: presents the \
	           minimal honest surface (existence, definition-time constants, real init lifecycle) \
	           so the pure-Python importlib test family imports; see native/testsinglephase.rs.";
	// SAFETY: Runtime allocation helper; NULL is checked below.
	let doc_obj = unsafe { abi::pon_const_str(doc.as_ptr(), doc.len()) };
	if doc_obj.is_null() {
		return Err("failed to allocate _testsinglephase.__doc__".to_owned());
	}
	let mut attrs: Vec<(u32, *mut PyObject)> =
		vec![(intern("__name__"), name_obj), (intern("__doc__"), doc_obj)];

	// Definition-time constants, verbatim from `Modules/_testsinglephase.c`.
	// SAFETY: Int constant allocator; NULL is checked below.
	let int_const = unsafe { abi::pon_const_int(1969) };
	if int_const.is_null() {
		return Err("failed to allocate _testsinglephase.int_const".to_owned());
	}
	attrs.push((intern("int_const"), int_const));
	let str_const = "something different";
	// SAFETY: String allocation helper; NULL is checked below.
	let str_const_obj = unsafe { abi::pon_const_str(str_const.as_ptr(), str_const.len()) };
	if str_const_obj.is_null() {
		return Err("failed to allocate _testsinglephase.str_const".to_owned());
	}
	attrs.push((intern("str_const"), str_const_obj));

	for (fn_name, entry) in [
		("initialized", initialized_entry as BuiltinFn),
		("initialized_count", initialized_count_entry),
	] {
		// SAFETY: `entry` is a live builtin entry point with the runtime
		// calling convention.
		let function =
			unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(fn_name)) };
		if function.is_null() {
			return Err(format!("failed to allocate _testsinglephase.{fn_name}"));
		}
		attrs.push((intern(fn_name), function));
	}
	install_module(name, attrs)
}
