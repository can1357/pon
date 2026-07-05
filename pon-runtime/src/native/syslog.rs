//! Native `syslog` module backed by libc syslog(3).
//!
//! The module keeps the openlog ident string alive across calls, mirrors the
//! Darwin constants exposed by CPython, and forwards messages to the host
//! syslog facility.

use std::{ffi::CString, ptr, sync::Mutex};

use num_traits::ToPrimitive;

use super::install_module;
use crate::{
	abi::{self, pon_const_int, pon_const_str, pon_make_function},
	intern::intern,
	object::PyObject,
	types::{exc::ExceptionKind, type_::unicode_text},
};

const VARIADIC_ARITY: usize = crate::native::builtins_mod::VARIADIC_ARITY;

#[cfg(target_os = "macos")]
const CONSTANTS: &[(&str, i64)] = &[
	("LOG_ALERT", 1),
	("LOG_AUTH", 32),
	("LOG_AUTHPRIV", 80),
	("LOG_CONS", 2),
	("LOG_CRIT", 2),
	("LOG_CRON", 72),
	("LOG_DAEMON", 24),
	("LOG_DEBUG", 7),
	("LOG_EMERG", 0),
	("LOG_ERR", 3),
	("LOG_FTP", 88),
	("LOG_INFO", 6),
	("LOG_INSTALL", 112),
	("LOG_KERN", 0),
	("LOG_LAUNCHD", 192),
	("LOG_LOCAL0", 128),
	("LOG_LOCAL1", 136),
	("LOG_LOCAL2", 144),
	("LOG_LOCAL3", 152),
	("LOG_LOCAL4", 160),
	("LOG_LOCAL5", 168),
	("LOG_LOCAL6", 176),
	("LOG_LOCAL7", 184),
	("LOG_LPR", 48),
	("LOG_MAIL", 16),
	("LOG_NDELAY", 8),
	("LOG_NETINFO", 96),
	("LOG_NEWS", 56),
	("LOG_NOTICE", 5),
	("LOG_NOWAIT", 16),
	("LOG_ODELAY", 4),
	("LOG_PERROR", 32),
	("LOG_PID", 1),
	("LOG_RAS", 120),
	("LOG_REMOTEAUTH", 104),
	("LOG_SYSLOG", 40),
	("LOG_USER", 8),
	("LOG_UUCP", 64),
	("LOG_WARNING", 4),
];

#[cfg(not(target_os = "macos"))]
const CONSTANTS: &[(&str, i64)] = &[
	("LOG_ALERT", libc::LOG_ALERT as i64),
	("LOG_AUTH", libc::LOG_AUTH as i64),
	("LOG_CONS", libc::LOG_CONS as i64),
	("LOG_CRIT", libc::LOG_CRIT as i64),
	("LOG_CRON", libc::LOG_CRON as i64),
	("LOG_DAEMON", libc::LOG_DAEMON as i64),
	("LOG_DEBUG", libc::LOG_DEBUG as i64),
	("LOG_EMERG", libc::LOG_EMERG as i64),
	("LOG_ERR", libc::LOG_ERR as i64),
	("LOG_INFO", libc::LOG_INFO as i64),
	("LOG_KERN", libc::LOG_KERN as i64),
	("LOG_LOCAL0", libc::LOG_LOCAL0 as i64),
	("LOG_LOCAL1", libc::LOG_LOCAL1 as i64),
	("LOG_LOCAL2", libc::LOG_LOCAL2 as i64),
	("LOG_LOCAL3", libc::LOG_LOCAL3 as i64),
	("LOG_LOCAL4", libc::LOG_LOCAL4 as i64),
	("LOG_LOCAL5", libc::LOG_LOCAL5 as i64),
	("LOG_LOCAL6", libc::LOG_LOCAL6 as i64),
	("LOG_LOCAL7", libc::LOG_LOCAL7 as i64),
	("LOG_LPR", libc::LOG_LPR as i64),
	("LOG_MAIL", libc::LOG_MAIL as i64),
	("LOG_NDELAY", libc::LOG_NDELAY as i64),
	("LOG_NEWS", libc::LOG_NEWS as i64),
	("LOG_NOTICE", libc::LOG_NOTICE as i64),
	("LOG_NOWAIT", libc::LOG_NOWAIT as i64),
	("LOG_ODELAY", libc::LOG_ODELAY as i64),
	("LOG_PID", libc::LOG_PID as i64),
	("LOG_SYSLOG", libc::LOG_SYSLOG as i64),
	("LOG_USER", libc::LOG_USER as i64),
	("LOG_UUCP", libc::LOG_UUCP as i64),
	("LOG_WARNING", libc::LOG_WARNING as i64),
];

static IDENT: Mutex<Option<CString>> = Mutex::new(None);

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let name = "syslog";
	let mut attrs = vec![string_attr("__name__", name)?];
	for &(const_name, value) in CONSTANTS {
		attrs.push(int_attr(const_name, value)?);
	}
	attrs.push(function_attr("openlog", syslog_openlog)?);
	attrs.push(function_attr("closelog", syslog_closelog)?);
	attrs.push(function_attr("syslog", syslog_syslog)?);
	attrs.push(function_attr("setlogmask", syslog_setlogmask)?);
	attrs.push(function_attr("LOG_MASK", syslog_log_mask)?);
	attrs.push(function_attr("LOG_UPTO", syslog_log_upto)?);
	install_module(name, attrs)
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
	let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate syslog.{name}"))
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
	let object = unsafe { pon_const_int(value) };
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate syslog.{name}"))
}

fn function_attr(
	name: &str,
	entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Result<(u32, *mut PyObject), String> {
	let function = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	(!function.is_null())
		.then_some((intern(name), function))
		.ok_or_else(|| format!("failed to allocate syslog.{name}"))
}

unsafe extern "C" fn syslog_openlog(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if args.len() <= 3 => args,
		_ => return raise_type_error(&format!("openlog expected at most 3 arguments, got {argc}")),
	};
	let ident = if let Some(&object) = args.first() {
		Some(match string_arg(object, "ident") {
			Ok(value) => value,
			Err(error) => return error,
		})
	} else {
		None
	};
	let option = match args.get(1).copied() {
		Some(object) => match c_int_arg(object, "logoption") {
			Ok(value) => value,
			Err(error) => return error,
		},
		None => 0,
	};
	let facility = match args.get(2).copied() {
		Some(object) => match c_int_arg(object, "facility") {
			Ok(value) => value,
			Err(error) => return error,
		},
		None => libc::LOG_USER,
	};
	let mut guard = IDENT.lock().unwrap_or_else(|poison| poison.into_inner());
	*guard = match ident {
		Some(text) => match CString::new(text) {
			Ok(value) => Some(value),
			Err(_) => return raise_value_error("embedded null character"),
		},
		None => None,
	};
	let ptr = guard.as_ref().map_or(ptr::null(), |ident| ident.as_ptr());
	unsafe { libc::openlog(ptr, option, facility) };
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn syslog_closelog(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 || !argv.is_null() {
		return raise_type_error(&format!("closelog() takes no arguments ({argc} given)"));
	}
	unsafe { libc::closelog() };
	*IDENT.lock().unwrap_or_else(|poison| poison.into_inner()) = None;
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn syslog_syslog(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if args.len() == 1 || args.len() == 2 => args,
		_ => return raise_type_error(&format!("syslog expected 1 or 2 arguments, got {argc}")),
	};
	let (priority, message_obj) = if args.len() == 1 {
		(libc::LOG_INFO, args[0])
	} else {
		let priority = match c_int_arg(args[0], "priority") {
			Ok(value) => value,
			Err(error) => return error,
		};
		(priority, args[1])
	};
	let message = match string_arg(message_obj, "message") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let c_message = match CString::new(message) {
		Ok(value) => value,
		Err(_) => return raise_value_error("embedded null character"),
	};
	unsafe { libc::syslog(priority, b"%s\0".as_ptr().cast::<libc::c_char>(), c_message.as_ptr()) };
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn syslog_setlogmask(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if args.len() == 1 => args,
		_ => return raise_type_error(&format!("setlogmask expected 1 argument, got {argc}")),
	};
	let mask = match c_int_arg(args[0], "mask") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let old = unsafe { libc::setlogmask(mask) };
	unsafe { pon_const_int(i64::from(old)) }
}

unsafe extern "C" fn syslog_log_mask(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if args.len() == 1 => args,
		_ => return raise_type_error(&format!("LOG_MASK expected 1 argument, got {argc}")),
	};
	let priority = match c_int_arg(args[0], "priority") {
		Ok(value) => value,
		Err(error) => return error,
	};
	unsafe { pon_const_int(i64::from(1_i32.wrapping_shl(priority as u32))) }
}

unsafe extern "C" fn syslog_log_upto(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if args.len() == 1 => args,
		_ => return raise_type_error(&format!("LOG_UPTO expected 1 argument, got {argc}")),
	};
	let priority = match c_int_arg(args[0], "priority") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let mask = if priority >= 31 {
		-1
	} else {
		(1_i32 << (priority + 1)) - 1
	};
	unsafe { pon_const_int(i64::from(mask)) }
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

fn string_arg(object: *mut PyObject, what: &str) -> Result<String, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	let Some(text) = (unsafe { unicode_text(object) }) else {
		return Err(raise_type_error(&format!("{what} must be str")));
	};
	Ok(text.to_owned())
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
