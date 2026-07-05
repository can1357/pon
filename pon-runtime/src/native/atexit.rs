//! Native `atexit` seed: `logging` registers its shutdown hook at import
//! (`atexit.register(shutdown)`), and unittest machinery unregisters.
//!
//! Registered callbacks are held as GC roots through [`gc_held_roots`] and
//! run in LIFO order once the `__main__` module finishes — both process
//! drivers (JIT `run_file_inner`, AoT `pon_aot_entry`) call
//! [`run_exit_callbacks`] before `__main__` teardown, mirroring CPython's
//! finalization order (`Py_FinalizeEx` runs `atexit` callbacks before module
//! destruction).

use std::{ptr, sync::Mutex};

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{
	abi::{pon_call, pon_make_function},
	intern::intern,
	object::PyObject,
	thread_state::pon_err_set,
};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let name = "atexit";
	let mut attrs =
		vec![(intern("__name__"), unsafe { crate::abi::pon_const_str(name.as_ptr(), name.len()) })];
	let functions: [(&str, BuiltinFn); 4] = [
		("register", atexit_register),
		("unregister", atexit_unregister),
		("_clear", atexit_clear),
		("_ncallbacks", atexit_ncallbacks),
	];
	for (function_name, entry) in functions {
		// SAFETY: `entry` is a live builtin entry with the runtime calling convention.
		let function =
			unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(function_name)) };
		if function.is_null() {
			return Err(format!("failed to allocate atexit.{function_name}"));
		}
		attrs.push((intern(function_name), function));
	}
	install_module(name, attrs)
}

/// One registration: the callback and its captured positional arguments
/// (held as GC roots until [`run_exit_callbacks`] consumes them).
struct ExitCallback {
	callable: usize,
	args:     Vec<usize>,
}

static CALLBACKS: Mutex<Vec<ExitCallback>> = Mutex::new(Vec::new());

/// Runs registered callbacks in LIFO order and empties the table: CPython's
/// finalization contract (`atexit_callfuncs`).  Called by the process
/// drivers once `__main__` has finished (normally or with an uncaught
/// exception).  A raising callback is reported to stderr and does not stop
/// later callbacks or change the exit status (CPython 3.14 routes it through
/// the unraisable hook); callbacks registered while the snapshot runs are
/// not invoked, matching observed CPython 3.14 behavior.
pub fn run_exit_callbacks() {
	let callbacks = std::mem::take(
		&mut *CALLBACKS
			.lock()
			.unwrap_or_else(|poison| poison.into_inner()),
	);
	for callback in callbacks.into_iter().rev() {
		let callable = callback.callable as *mut PyObject;
		let mut args = callback
			.args
			.iter()
			.map(|&arg| arg as *mut PyObject)
			.collect::<Vec<_>>();
		let argv = if args.is_empty() {
			ptr::null_mut()
		} else {
			args.as_mut_ptr()
		};
		// SAFETY: The callable and its arguments were captured from live
		// argument slots at registration time and held as GC roots since.
		let result = unsafe { pon_call(callable, argv, args.len()) };
		if result.is_null() {
			report_ignored_exception();
		}
	}
}

/// Reports a failed exit callback to stderr in pon's compact uncaught shape
/// (`Type: message`, see `sys.excepthook`) and clears the pending error so
/// the remaining callbacks still run.
fn report_ignored_exception() {
	use std::io::Write;

	let detail = crate::abi::exc::pending_exception_object().map(|value| {
		// SAFETY: `pending_exception_object` returns a live heap exception.
		let type_name = unsafe {
			let ty = (*value).ob_type;
			if ty.is_null() {
				"<unknown>"
			} else {
				(*ty).name()
			}
		};
		let message = super::builtins_mod::str_text(value);
		if message.is_empty() {
			type_name.to_owned()
		} else {
			format!("{type_name}: {message}")
		}
	});
	crate::thread_state::pon_err_clear();
	let mut stderr = std::io::stderr().lock();
	let _ = match detail {
		Some(detail) => writeln!(stderr, "Exception ignored in atexit callback: {detail}"),
		None => writeln!(stderr, "Exception ignored in atexit callback"),
	};
	let _ = stderr.flush();
}

/// GC roots held by the registration table.  Consumed by
/// `crate::abi::collect` under the runtime lock; must not re-enter the
/// runtime.
pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
	let callbacks = CALLBACKS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	let mut roots = Vec::new();
	for callback in callbacks.iter() {
		let callable = callback.callable as *mut PyObject;
		if !callable.is_null() && crate::tag::is_heap(callable) {
			roots.push(callable);
		}
		for &arg in &callback.args {
			let arg = arg as *mut PyObject;
			if !arg.is_null() && crate::tag::is_heap(arg) {
				roots.push(arg);
			}
		}
	}
	roots
}

/// `atexit.register(func, /, *args, **kwargs)`: records and returns `func`.
unsafe extern "C" fn atexit_register(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc == 0 || argv.is_null() {
		pon_err_set("register() requires a callable");
		return ptr::null_mut();
	}
	// SAFETY: The call helper supplies `argv` with `argc` entries.
	let args = unsafe { core::slice::from_raw_parts(argv.cast_const(), argc) };
	let callable = args[0];
	CALLBACKS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.push(ExitCallback {
			callable: callable as usize,
			args:     args[1..].iter().map(|&arg| arg as usize).collect(),
		});
	callable
}

/// `atexit.unregister(func)`: drops every registration of `func`.
unsafe extern "C" fn atexit_unregister(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 1 || argv.is_null() {
		pon_err_set(format!("unregister() takes exactly 1 argument ({argc} given)"));
		return ptr::null_mut();
	}
	// SAFETY: The call helper supplies `argv` with at least one entry.
	let callable = unsafe { *argv } as usize;
	CALLBACKS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.retain(|callback| callback.callable != callable);
	// SAFETY: Singleton accessor.
	unsafe { crate::abi::pon_none() }
}

/// `atexit._clear()`: empties the registration table.
unsafe extern "C" fn atexit_clear(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		pon_err_set(format!("_clear() takes no arguments ({argc} given)"));
		return ptr::null_mut();
	}
	CALLBACKS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.clear();
	// SAFETY: Singleton accessor.
	unsafe { crate::abi::pon_none() }
}

/// `atexit._ncallbacks()`: number of live registrations.
unsafe extern "C" fn atexit_ncallbacks(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		pon_err_set(format!("_ncallbacks() takes no arguments ({argc} given)"));
		return ptr::null_mut();
	}
	let count = CALLBACKS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.len();
	// SAFETY: Allocation helper.
	unsafe { crate::abi::pon_const_int(count as i64) }
}
