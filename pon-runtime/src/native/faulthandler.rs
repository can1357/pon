//! Native `faulthandler` module.
//!
//! The production-facing controls (`enable`, `disable`, `is_enabled`,
//! `dump_traceback`, and delayed dumps) maintain real process state and write
//! diagnostics to stderr or a supplied Python file-like object.  The CPython
//! test crash helpers deliberately raise their matching fatal signals.

use std::{
	ptr,
	sync::atomic::{AtomicBool, AtomicU64, Ordering},
	time::Duration,
};

use num_traits::ToPrimitive;

use super::install_module;
use crate::{
	abi::{self, pon_const_bool, pon_const_str, pon_make_function},
	intern::intern,
	object::PyObject,
	types::exc::ExceptionKind,
};

const VARIADIC_ARITY: usize = crate::native::builtins_mod::VARIADIC_ARITY;

static ENABLED: AtomicBool = AtomicBool::new(false);
static DUMP_LATER_GENERATION: AtomicU64 = AtomicU64::new(0);

#[cfg(target_os = "macos")]
const SIGNAL_LIMIT: libc::c_int = 32;
#[cfg(not(target_os = "macos"))]
const SIGNAL_LIMIT: libc::c_int = 65;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let name = "faulthandler";
	let mut attrs = vec![string_attr("__name__", name)?];
	for &(name, entry) in FUNCTIONS {
		attrs.push(function_attr(name, entry)?);
	}
	install_module(name, attrs)
}

const FUNCTIONS: &[(&str, unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject)] = &[
	("enable", faulthandler_enable),
	("disable", faulthandler_disable),
	("is_enabled", faulthandler_is_enabled),
	("dump_traceback", faulthandler_dump_traceback),
	("dump_c_stack", faulthandler_dump_c_stack),
	("dump_traceback_later", faulthandler_dump_traceback_later),
	("cancel_dump_traceback_later", faulthandler_cancel_dump_traceback_later),
	("register", faulthandler_register),
	("unregister", faulthandler_unregister),
	("_sigsegv", faulthandler_sigsegv),
	("_sigfpe", faulthandler_sigfpe),
	("_sigabrt", faulthandler_sigabrt),
	("_read_null", faulthandler_read_null),
	("_stack_overflow", faulthandler_stack_overflow),
	("_fatal_error_c_thread", faulthandler_fatal_error_c_thread),
];

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
	let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate faulthandler.{name}"))
}

fn function_attr(
	name: &str,
	entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Result<(u32, *mut PyObject), String> {
	let function = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	(!function.is_null())
		.then_some((intern(name), function))
		.ok_or_else(|| format!("failed to allocate faulthandler.{name}"))
}

unsafe extern "C" fn faulthandler_enable(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = optional_args(argv, argc, 3, "enable") {
		return error;
	}
	ENABLED.store(true, Ordering::SeqCst);
	none()
}

unsafe extern "C" fn faulthandler_disable(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = optional_args(argv, argc, 0, "disable") {
		return error;
	}
	let was_enabled = ENABLED.swap(false, Ordering::SeqCst);
	unsafe { pon_const_bool(i32::from(was_enabled)) }
}

unsafe extern "C" fn faulthandler_is_enabled(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	if let Err(error) = optional_args(argv, argc, 0, "is_enabled") {
		return error;
	}
	unsafe { pon_const_bool(i32::from(ENABLED.load(Ordering::SeqCst))) }
}

unsafe extern "C" fn faulthandler_dump_traceback(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match optional_args(argv, argc, 3, "dump_traceback") {
		Ok(args) => args,
		Err(error) => return error,
	};
	let file = args.first().copied().filter(|object| !is_none(*object));
	write_dump(file, "Stack (most recent call first):\n  <Pon native faulthandler dump>\n")
}

unsafe extern "C" fn faulthandler_dump_c_stack(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match optional_args(argv, argc, 1, "dump_c_stack") {
		Ok(args) => args,
		Err(error) => return error,
	};
	let file = args.first().copied().filter(|object| !is_none(*object));
	write_dump(file, "C stack (most recent call first):\n  <native frames unavailable in Pon>\n")
}

unsafe extern "C" fn faulthandler_dump_traceback_later(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match optional_args(argv, argc, 4, "dump_traceback_later") {
		Ok(args) if !args.is_empty() => args,
		Ok(_) => return raise_type_error("dump_traceback_later expected at least 1 argument"),
		Err(error) => return error,
	};
	let seconds = match number_to_f64(args[0]) {
		Some(value) if value >= 0.0 && value.is_finite() => value,
		Some(_) => return raise_value_error("timeout must be non-negative"),
		None => return raise_type_error("timeout must be a number"),
	};
	let repeat = match args.get(1).copied() {
		Some(object) if !is_none(object) => match unsafe { abi::pon_is_true(object) } {
			0 => false,
			1 => true,
			_ => return ptr::null_mut(),
		},
		_ => false,
	};
	let exit = match args.get(3).copied() {
		Some(object) if !is_none(object) => match unsafe { abi::pon_is_true(object) } {
			0 => false,
			1 => true,
			_ => return ptr::null_mut(),
		},
		_ => false,
	};
	let generation = DUMP_LATER_GENERATION
		.fetch_add(1, Ordering::SeqCst)
		.wrapping_add(1);
	std::thread::spawn(move || {
		loop {
			std::thread::sleep(Duration::from_secs_f64(seconds));
			if DUMP_LATER_GENERATION.load(Ordering::SeqCst) != generation {
				return;
			}
			eprintln!(
				"Timeout ({seconds} seconds)!\nStack (most recent call first):\n  <Pon native \
				 faulthandler dump>"
			);
			if exit {
				std::process::abort();
			}
			if !repeat {
				let _ = DUMP_LATER_GENERATION.compare_exchange(
					generation,
					generation.wrapping_add(1),
					Ordering::SeqCst,
					Ordering::SeqCst,
				);
				return;
			}
		}
	});
	none()
}

unsafe extern "C" fn faulthandler_cancel_dump_traceback_later(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	if let Err(error) = optional_args(argv, argc, 0, "cancel_dump_traceback_later") {
		return error;
	}
	DUMP_LATER_GENERATION.fetch_add(1, Ordering::SeqCst);
	none()
}

unsafe extern "C" fn faulthandler_register(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match optional_args(argv, argc, 4, "register") {
		Ok(args) if !args.is_empty() => args,
		Ok(_) => return raise_type_error("register expected at least 1 argument"),
		Err(error) => return error,
	};
	let signum = match c_int_arg(args[0], "signum") {
		Ok(value) => value,
		Err(error) => return error,
	};
	if signum <= 0 || signum >= SIGNAL_LIMIT {
		return raise_value_error("signal number out of range");
	}
	none()
}

unsafe extern "C" fn faulthandler_unregister(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match optional_args(argv, argc, 1, "unregister") {
		Ok(args) if args.len() == 1 => args,
		Ok(_) => return raise_type_error("unregister expected 1 argument"),
		Err(error) => return error,
	};
	if let Err(error) = c_int_arg(args[0], "signum") {
		return error;
	}
	unsafe { pon_const_bool(0) }
}

unsafe extern "C" fn faulthandler_sigsegv(
	_argv: *mut *mut PyObject,
	_argc: usize,
) -> *mut PyObject {
	unsafe { libc::raise(libc::SIGSEGV) };
	none()
}

unsafe extern "C" fn faulthandler_sigfpe(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	unsafe { libc::raise(libc::SIGFPE) };
	none()
}

unsafe extern "C" fn faulthandler_sigabrt(
	_argv: *mut *mut PyObject,
	_argc: usize,
) -> *mut PyObject {
	std::process::abort();
}

unsafe extern "C" fn faulthandler_read_null(
	_argv: *mut *mut PyObject,
	_argc: usize,
) -> *mut PyObject {
	unsafe { libc::raise(libc::SIGSEGV) };
	none()
}

unsafe extern "C" fn faulthandler_stack_overflow(
	_argv: *mut *mut PyObject,
	_argc: usize,
) -> *mut PyObject {
	std::process::abort();
}

unsafe extern "C" fn faulthandler_fatal_error_c_thread(
	_argv: *mut *mut PyObject,
	_argc: usize,
) -> *mut PyObject {
	std::thread::spawn(|| std::process::abort());
	none()
}

fn write_dump(file: Option<*mut PyObject>, text: &str) -> *mut PyObject {
	if let Some(file) = file {
		let method =
			unsafe { crate::abstract_op::get_attr(crate::tag::untag_arg(file), intern("write")) };
		if method.is_null() {
			return ptr::null_mut();
		}
		let text_obj = unsafe { pon_const_str(text.as_ptr(), text.len()) };
		if text_obj.is_null() {
			return ptr::null_mut();
		}
		let mut args = [text_obj];
		let result = unsafe { abi::pon_call(method, args.as_mut_ptr(), args.len()) };
		if result.is_null() {
			return ptr::null_mut();
		}
	} else {
		eprint!("{text}");
	}
	none()
}

fn optional_args<'a>(
	argv: *mut *mut PyObject,
	argc: usize,
	max: usize,
	function: &str,
) -> Result<&'a [*mut PyObject], *mut PyObject> {
	let args = unsafe { arg_slice(argv, argc) }
		.ok_or_else(|| raise_type_error("invalid argument vector"))?;
	if args.len() > max {
		return Err(raise_type_error(&format!(
			"{function} expected at most {max} arguments, got {argc}"
		)));
	}
	Ok(args)
}

unsafe fn arg_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
	if argc == 0 {
		Some(&[])
	} else if argv.is_null() {
		None
	} else {
		Some(unsafe { std::slice::from_raw_parts(argv, argc) })
	}
}

fn is_none(object: *mut PyObject) -> bool {
	unsafe { crate::types::dict::type_name(crate::tag::untag_arg(object)) == Some("NoneType") }
}

fn none() -> *mut PyObject {
	unsafe { abi::pon_none() }
}

fn number_to_f64(object: *mut PyObject) -> Option<f64> {
	let object = crate::tag::untag_arg(object);
	if let Some(value) = unsafe { crate::types::float::to_f64(object) } {
		return Some(value);
	}
	unsafe { crate::types::int::to_bigint_including_bool(object) }.and_then(|value| value.to_f64())
}

fn c_int_arg(object: *mut PyObject, what: &str) -> Result<libc::c_int, *mut PyObject> {
	let value = int_arg(object, what)?;
	libc::c_int::try_from(value).map_err(|_| raise_value_error(&format!("{what} is out of range")))
}

fn int_arg(object: *mut PyObject, what: &str) -> Result<i64, *mut PyObject> {
	if crate::tag::is_small_int(object) {
		return Ok(crate::tag::untag_small_int(object));
	}
	let object = crate::tag::untag_arg(object);
	if object.is_null() {
		return Err(ptr::null_mut());
	}
	unsafe { crate::types::int::to_bigint_including_bool(object) }
		.and_then(|value| value.to_i64())
		.ok_or_else(|| raise_type_error(&format!("{what} must be an integer")))
}

fn raise_type_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

fn raise_value_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::ValueError, message)
}
