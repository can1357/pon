//! Native `gc` module seed for deterministic conformance collection.

use std::{
	ptr,
	sync::{
		Mutex,
		atomic::{AtomicBool, AtomicI64, Ordering},
	},
};

use num_traits::ToPrimitive;

use super::{
	builtins_mod::{VARIADIC_ARITY, alloc_list},
	install_module,
};
use crate::{intern::intern, object::PyObject, types::exc::ExceptionKind};

/// Automatic-collection enabled flag (CPython `gc.enable`/`gc.disable`/
/// `gc.isenabled`).  pon collects only on explicit `gc.collect()`, so the flag
/// is pure state that stdlib callers (`timeit`, Cython inline, atexit hooks)
/// read and toggle; it never gates the manual collector.
static GC_ENABLED: AtomicBool = AtomicBool::new(true);
static DEBUG_FLAGS: AtomicI64 = AtomicI64::new(0);
static THRESHOLDS: Mutex<(i64, i64, i64)> = Mutex::new((700, 10, 10));
static GARBAGE_LIST: Mutex<usize> = Mutex::new(0);
static CALLBACKS_LIST: Mutex<usize> = Mutex::new(0);

const DEBUG_STATS: i64 = 1;
const DEBUG_COLLECTABLE: i64 = 2;
const DEBUG_UNCOLLECTABLE: i64 = 4;
const DEBUG_SAVEALL: i64 = 32;
const DEBUG_LEAK: i64 = DEBUG_COLLECTABLE | DEBUG_UNCOLLECTABLE | DEBUG_SAVEALL;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let mut attrs = vec![
		(intern("__name__"), unsafe { crate::abi::pon_const_str(b"gc".as_ptr(), 2) }),
		int_attr("DEBUG_STATS", DEBUG_STATS)?,
		int_attr("DEBUG_COLLECTABLE", DEBUG_COLLECTABLE)?,
		int_attr("DEBUG_UNCOLLECTABLE", DEBUG_UNCOLLECTABLE)?,
		int_attr("DEBUG_SAVEALL", DEBUG_SAVEALL)?,
		int_attr("DEBUG_LEAK", DEBUG_LEAK)?,
		(intern("garbage"), list_singleton(&GARBAGE_LIST)?),
		(intern("callbacks"), list_singleton(&CALLBACKS_LIST)?),
	];
	for (name, entry) in [
		("collect", gc_collect as BuiltinFn),
		("enable", gc_enable),
		("disable", gc_disable),
		("isenabled", gc_isenabled),
		("set_debug", gc_set_debug),
		("get_debug", gc_get_debug),
		("set_threshold", gc_set_threshold),
		("get_threshold", gc_get_threshold),
		("get_count", gc_get_count),
		("get_stats", gc_get_stats),
		("freeze", gc_freeze),
		("unfreeze", gc_unfreeze),
		("get_freeze_count", gc_get_freeze_count),
		("get_objects", gc_get_objects),
		("get_referents", gc_get_referents),
		("get_referrers", gc_get_referrers),
		("is_tracked", gc_is_tracked),
		("is_finalized", gc_is_finalized),
	] {
		attrs.push((intern(name), function_attr(name, entry)?));
	}
	install_module("gc", attrs)
}

fn function_attr(name: &str, entry: BuiltinFn) -> Result<*mut PyObject, String> {
	let function =
		unsafe { crate::abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	if function.is_null() {
		return Err(format!("failed to allocate gc.{name}"));
	}
	Ok(function)
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
	let object = unsafe { crate::abi::pon_const_int(value) };
	if object.is_null() {
		return Err(format!("failed to allocate gc.{name}"));
	}
	Ok((intern(name), object))
}

fn list_singleton(slot: &Mutex<usize>) -> Result<*mut PyObject, String> {
	let mut slot = slot.lock().unwrap_or_else(|poison| poison.into_inner());
	if *slot == 0 {
		let list = alloc_list(Vec::new());
		if list.is_null() {
			return Err("failed to allocate gc list".to_owned());
		}
		*slot = list as usize;
	}
	Ok(*slot as *mut PyObject)
}

fn none() -> *mut PyObject {
	unsafe { crate::abi::pon_none() }
}

fn int_value(object: *mut PyObject) -> Option<i64> {
	unsafe { crate::types::int::to_bigint_including_bool(object) }.and_then(|value| value.to_i64())
}

fn tuple3(a: i64, b: i64, c: i64) -> *mut PyObject {
	let mut items =
		[unsafe { crate::abi::pon_const_int(a) }, unsafe { crate::abi::pon_const_int(b) }, unsafe {
			crate::abi::pon_const_int(c)
		}];
	if items.iter().any(|item| item.is_null()) {
		return ptr::null_mut();
	}
	unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

fn stats_dict() -> *mut PyObject {
	let mut items = [
		unsafe { crate::abi::pon_const_str(b"collections".as_ptr(), 11) },
		unsafe { crate::abi::pon_const_int(0) },
		unsafe { crate::abi::pon_const_str(b"collected".as_ptr(), 9) },
		unsafe { crate::abi::pon_const_int(0) },
		unsafe { crate::abi::pon_const_str(b"uncollectable".as_ptr(), 13) },
		unsafe { crate::abi::pon_const_int(0) },
	];
	if items.iter().any(|item| item.is_null()) {
		return ptr::null_mut();
	}
	unsafe { crate::abi::map::pon_build_map(items.as_mut_ptr(), 3) }
}

fn unsupported(name: &str) -> *mut PyObject {
	crate::abi::exc::raise_kind_error_text(
		ExceptionKind::NotImplementedError,
		&format!(
			"gc.{name} is not supported: pon's collector does not expose CPython object-tracking \
			 introspection"
		),
	)
}

unsafe extern "C" fn gc_enable(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("gc.enable expected no arguments");
	}
	GC_ENABLED.store(true, Ordering::Relaxed);
	none()
}

unsafe extern "C" fn gc_disable(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("gc.disable expected no arguments");
	}
	GC_ENABLED.store(false, Ordering::Relaxed);
	none()
}

unsafe extern "C" fn gc_isenabled(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("gc.isenabled expected no arguments");
	}
	unsafe { crate::abi::number::pon_const_bool(i32::from(GC_ENABLED.load(Ordering::Relaxed))) }
}

unsafe extern "C" fn gc_set_debug(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 1 || argv.is_null() {
		return crate::abi::return_null_with_error("gc.set_debug expected one argument");
	}
	let Some(flags) = int_value(unsafe { *argv }) else {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			"an integer is required",
		);
	};
	DEBUG_FLAGS.store(flags, Ordering::Relaxed);
	none()
}

unsafe extern "C" fn gc_get_debug(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("gc.get_debug expected no arguments");
	}
	unsafe { crate::abi::pon_const_int(DEBUG_FLAGS.load(Ordering::Relaxed)) }
}

unsafe extern "C" fn gc_set_threshold(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc == 0 || argc > 3 || argv.is_null() {
		return crate::abi::return_null_with_error(
			"gc.set_threshold expected one to three arguments",
		);
	}
	let mut thresholds = THRESHOLDS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	let mut values = [thresholds.0, thresholds.1, thresholds.2];
	for index in 0..argc {
		let Some(value) = int_value(unsafe { *argv.add(index) }) else {
			return crate::abi::exc::raise_kind_error_text(
				ExceptionKind::TypeError,
				"an integer is required",
			);
		};
		values[index] = value;
	}
	*thresholds = (values[0], values[1], values[2]);
	none()
}

unsafe extern "C" fn gc_get_threshold(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("gc.get_threshold expected no arguments");
	}
	let thresholds = THRESHOLDS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	tuple3(thresholds.0, thresholds.1, thresholds.2)
}

unsafe extern "C" fn gc_get_count(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("gc.get_count expected no arguments");
	}
	tuple3(0, 0, 0)
}

unsafe extern "C" fn gc_get_stats(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("gc.get_stats expected no arguments");
	}
	let mut stats = vec![stats_dict(), stats_dict(), stats_dict()];
	if stats.iter().any(|item| item.is_null()) {
		return ptr::null_mut();
	}
	unsafe { crate::abi::seq::pon_build_list(stats.as_mut_ptr(), stats.len()) }
}

unsafe extern "C" fn gc_collect(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc > 1 {
		return crate::abi::return_null_with_error("gc.collect expected at most one argument");
	}
	// Scrub before any collection frame is pushed: `abi::collect` re-scrubs,
	// but its own wrapper frame would otherwise sit in unscrubbed territory
	// still holding ghosts of the previous deep call chain (see
	// `abi::scrub_dead_stack_below`).  Scrubbing from the native entry point
	// pushes the ghost boundary up to this frame.
	crate::abi::scrub_dead_stack_below();
	match crate::abi::collect() {
		Ok(()) => unsafe { crate::abi::pon_const_int(0) },
		Err(message) => crate::abi::return_null_with_error(message),
	}
}

unsafe extern "C" fn gc_get_freeze_count(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return crate::abi::return_null_with_error("gc.get_freeze_count expected no arguments");
	}
	unsafe { crate::abi::pon_const_int(0) }
}

unsafe extern "C" fn gc_freeze(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	unsupported("freeze")
}

unsafe extern "C" fn gc_unfreeze(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	unsupported("unfreeze")
}

unsafe extern "C" fn gc_get_objects(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	unsupported("get_objects")
}

unsafe extern "C" fn gc_get_referents(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	unsupported("get_referents")
}

unsafe extern "C" fn gc_get_referrers(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	unsupported("get_referrers")
}

unsafe extern "C" fn gc_is_tracked(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	unsupported("is_tracked")
}

unsafe extern "C" fn gc_is_finalized(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	unsupported("is_finalized")
}
