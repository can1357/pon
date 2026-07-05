//! Native `fcntl` module seed backed by host `flock(2)`.
//!
//! meson's `mesonbuild.utils.posix.DirectoryLock` guards build directories
//! with `fcntl.flock(lockfile, LOCK_EX | LOCK_NB)`, so Pon exposes the lock
//! constants, the portable `F_*` command constants, and a real `flock` with
//! CPython's fd coercion (an int, or any object with a `fileno()` method).
//! `fcntl`, `ioctl`, and `lockf` now forward common integer and bytes-buffer
//! calls to libc as CPython does.

use super::install_module;
use crate::{
	abi::{pon_call, pon_const_int, pon_const_str, pon_make_function},
	intern::intern,
	object::PyObject,
	types::{bytearray_ as bytearray_type, bytes_ as bytes_type, exc::ExceptionKind},
};

const VARIADIC_ARITY: usize = crate::native::builtins_mod::VARIADIC_ARITY;

/// Constants shared by macOS and Linux, sorted by name.  The lock flags are
/// the surface meson reads at import; the `F_*` rows cover the common
/// `fcntl(2)` command vocabulary CPython always exposes.
const CONSTANTS: &[(&str, i64)] = &[
	("FD_CLOEXEC", libc::FD_CLOEXEC as i64),
	("F_DUPFD", libc::F_DUPFD as i64),
	("F_DUPFD_CLOEXEC", libc::F_DUPFD_CLOEXEC as i64),
	("F_GETFD", libc::F_GETFD as i64),
	("F_GETFL", libc::F_GETFL as i64),
	("F_GETLK", libc::F_GETLK as i64),
	("F_GETOWN", libc::F_GETOWN as i64),
	("F_RDLCK", libc::F_RDLCK as i64),
	("F_SETFD", libc::F_SETFD as i64),
	("F_SETFL", libc::F_SETFL as i64),
	("F_SETLK", libc::F_SETLK as i64),
	("F_SETLKW", libc::F_SETLKW as i64),
	("F_SETOWN", libc::F_SETOWN as i64),
	("F_UNLCK", libc::F_UNLCK as i64),
	("F_WRLCK", libc::F_WRLCK as i64),
	("LOCK_EX", libc::LOCK_EX as i64),
	("LOCK_NB", libc::LOCK_NB as i64),
	("LOCK_SH", libc::LOCK_SH as i64),
	("LOCK_UN", libc::LOCK_UN as i64),
];

#[cfg(target_os = "macos")]
const OS_CONSTANTS: &[(&str, i64)] = &[
	("FASYNC", 64),
	("F_FULLFSYNC", libc::F_FULLFSYNC as i64),
	("F_GETLEASE", 107),
	("F_GETNOSIGPIPE", 74),
	("F_GETPATH", libc::F_GETPATH as i64),
	("F_NOCACHE", libc::F_NOCACHE as i64),
	("F_OFD_GETLK", 92),
	("F_OFD_SETLK", 90),
	("F_OFD_SETLKW", 91),
	("F_RDAHEAD", 45),
	("F_SETLEASE", 106),
	("F_SETNOSIGPIPE", 73),
];

#[cfg(target_os = "linux")]
const OS_CONSTANTS: &[(&str, i64)] = &[
	("F_OFD_GETLK", libc::F_OFD_GETLK as i64),
	("F_OFD_SETLK", libc::F_OFD_SETLK as i64),
	("F_OFD_SETLKW", libc::F_OFD_SETLKW as i64),
];

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let name = "fcntl";
	// SAFETY: Runtime allocation helper; NULL is checked below.
	let name_obj = unsafe { pon_const_str(name.as_ptr(), name.len()) };
	if name_obj.is_null() {
		return Err("failed to allocate fcntl.__name__".to_owned());
	}
	let mut attrs = vec![(intern("__name__"), name_obj)];
	for &(const_name, value) in CONSTANTS.iter().chain(OS_CONSTANTS) {
		// SAFETY: Runtime allocation helper; NULL is checked below.
		let object = unsafe { pon_const_int(value) };
		if object.is_null() {
			return Err(format!("failed to allocate fcntl.{const_name}"));
		}
		attrs.push((intern(const_name), object));
	}
	attrs.push(function_attr("flock", fcntl_flock)?);
	attrs.push(function_attr("fcntl", fcntl_fcntl)?);
	attrs.push(function_attr("ioctl", fcntl_ioctl)?);
	attrs.push(function_attr("lockf", fcntl_lockf)?);
	install_module(name, attrs)
}

fn function_attr(
	name: &str,
	entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Result<(u32, *mut PyObject), String> {
	let object = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate fcntl.{name}"))
}

/// `fcntl.flock(fd, operation)`: host `flock(2)` with an EINTR retry,
/// raising the PEP 3151 OSError subclass on failure (`LOCK_NB` conflicts
/// surface as `BlockingIOError` through the shared errno mapping).
unsafe extern "C" fn fcntl_flock(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 2 || argv.is_null() {
		return raise_type_error(&format!("flock expected 2 arguments, got {argc}"));
	}
	// SAFETY: The caller passed two live argument slots.
	let args = unsafe { std::slice::from_raw_parts(argv, argc) };
	let fd = match fd_arg(args[0]) {
		Ok(fd) => fd,
		Err(raised) => return raised,
	};
	let operation = match int_arg(args[1], "operation") {
		Ok(value) => value as libc::c_int,
		Err(raised) => return raised,
	};
	loop {
		// SAFETY: Plain syscall; the kernel validates the descriptor.
		if unsafe { libc::flock(fd, operation) } == 0 {
			return unsafe { crate::abi::pon_none() };
		}
		let errno = std::io::Error::last_os_error()
			.raw_os_error()
			.unwrap_or(libc::EIO);
		if errno != libc::EINTR {
			return super::os::raise_errno(errno, None);
		}
	}
}

unsafe extern "C" fn fcntl_fcntl(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if (2..=3).contains(&args.len()) => args,
		_ => return raise_type_error(&format!("fcntl expected 2 or 3 arguments, got {argc}")),
	};
	let fd = match fd_arg(args[0]) {
		Ok(fd) => fd,
		Err(raised) => return raised,
	};
	let cmd = match c_int_arg(args[1], "cmd") {
		Ok(value) => value,
		Err(raised) => return raised,
	};
	let Some(&arg) = args.get(2) else {
		let result = unsafe { libc::fcntl(fd, cmd, 0) };
		return fcntl_result(result);
	};
	if let Some(bytes) = bytes_like(arg) {
		let mut buffer = bytes.to_vec();
		let result = unsafe { libc::fcntl(fd, cmd, buffer.as_mut_ptr()) };
		if result < 0 {
			return super::os::raise_errno(last_errno(), None);
		}
		return unsafe { crate::abi::str_::pon_const_bytes(buffer.as_ptr(), buffer.len()) };
	}
	let value = match c_int_arg(arg, "arg") {
		Ok(value) => value,
		Err(raised) => return raised,
	};
	let result = unsafe { libc::fcntl(fd, cmd, value) };
	fcntl_result(result)
}

unsafe extern "C" fn fcntl_ioctl(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if (2..=4).contains(&args.len()) => args,
		_ => return raise_type_error(&format!("ioctl expected 2 to 4 arguments, got {argc}")),
	};
	let fd = match fd_arg(args[0]) {
		Ok(fd) => fd,
		Err(raised) => return raised,
	};
	let request_int = match int_arg(args[1], "request") {
		Ok(value) => value,
		Err(raised) => return raised,
	};
	let request = match libc::c_ulong::try_from(request_int) {
		Ok(value) => value,
		Err(_) => return raise_value_error("request is out of range"),
	};
	let Some(&arg) = args.get(2) else {
		let result = unsafe { libc::ioctl(fd, request) };
		return fcntl_result(result);
	};
	let mutate = match args.get(3).copied() {
		Some(flag) => match unsafe { crate::abi::pon_is_true(flag) } {
			0 => false,
			1 => true,
			_ => return std::ptr::null_mut(),
		},
		None => true,
	};
	let raw = crate::tag::untag_arg(arg);
	if !raw.is_null() && !crate::tag::is_small_int(raw) {
		let ty = unsafe { (*raw).ob_type };
		if bytearray_type::is_bytearray_type(ty) {
			let bytearray = unsafe { &mut *raw.cast::<bytearray_type::PyByteArray>() };
			let result = unsafe { libc::ioctl(fd, request, bytearray.as_mut_slice().as_mut_ptr()) };
			if result < 0 {
				return super::os::raise_errno(last_errno(), None);
			}
			return if mutate {
				unsafe { pon_const_int(i64::from(result)) }
			} else {
				unsafe {
					crate::abi::str_::pon_const_bytes(
						bytearray.as_slice().as_ptr(),
						bytearray.as_slice().len(),
					)
				}
			};
		}
	}
	if let Some(bytes) = bytes_like(arg) {
		let mut buffer = bytes.to_vec();
		let result = unsafe { libc::ioctl(fd, request, buffer.as_mut_ptr()) };
		if result < 0 {
			return super::os::raise_errno(last_errno(), None);
		}
		return unsafe { crate::abi::str_::pon_const_bytes(buffer.as_ptr(), buffer.len()) };
	}
	let value = match c_int_arg(arg, "arg") {
		Ok(value) => value,
		Err(raised) => return raised,
	};
	let result = unsafe { libc::ioctl(fd, request, value) };
	fcntl_result(result)
}

unsafe extern "C" fn fcntl_lockf(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if (2..=5).contains(&args.len()) => args,
		_ => return raise_type_error(&format!("lockf expected 2 to 5 arguments, got {argc}")),
	};
	let fd = match fd_arg(args[0]) {
		Ok(fd) => fd,
		Err(raised) => return raised,
	};
	let operation = match c_int_arg(args[1], "operation") {
		Ok(value) => value,
		Err(raised) => return raised,
	};
	let len = match args.get(2).copied() {
		Some(object) => match off_t_arg(object, "len") {
			Ok(value) => value,
			Err(raised) => return raised,
		},
		None => 0,
	};
	let start = match args.get(3).copied() {
		Some(object) => match off_t_arg(object, "start") {
			Ok(value) => value,
			Err(raised) => return raised,
		},
		None => 0,
	};
	let whence = match args.get(4).copied() {
		Some(object) => match c_int_arg(object, "whence") {
			Ok(value) => value,
			Err(raised) => return raised,
		},
		None => libc::SEEK_SET,
	};
	let lock_type = if operation & libc::LOCK_UN != 0 {
		libc::F_UNLCK
	} else if operation & libc::LOCK_SH != 0 {
		libc::F_RDLCK
	} else {
		libc::F_WRLCK
	};
	let cmd = if operation & libc::LOCK_NB != 0 {
		libc::F_SETLK
	} else {
		libc::F_SETLKW
	};
	let mut lock = libc::flock {
		l_start:  start,
		l_len:    len,
		l_pid:    0,
		l_type:   lock_type as libc::c_short,
		l_whence: whence as libc::c_short,
	};
	loop {
		let result = unsafe { libc::fcntl(fd, cmd, &mut lock) };
		if result == 0 {
			return unsafe { crate::abi::pon_none() };
		}
		let errno = last_errno();
		if errno != libc::EINTR {
			return super::os::raise_errno(errno, None);
		}
	}
}

/// CPython `PyObject_AsFileDescriptor`: an int is the descriptor itself;
/// otherwise `fileno()` is called and must return an int.
fn fd_arg(object: *mut PyObject) -> Result<libc::c_int, *mut PyObject> {
	if let Ok(fd) = int_arg(object, "fd") {
		return c_int_range(fd);
	}
	crate::thread_state::pon_err_clear();
	let raw = crate::tag::untag_arg(object);
	if raw.is_null() {
		return Err(std::ptr::null_mut());
	}
	// SAFETY: Attribute dispatch follows the NULL-sentinel error contract.
	let method = unsafe { crate::abstract_op::get_attr(raw, intern("fileno")) };
	if method.is_null() {
		crate::thread_state::pon_err_clear();
		return Err(raise_type_error("argument must be an int, or have a fileno() method."));
	}
	// SAFETY: `pon_call` self-normalizes callee and dispatches by target kind.
	let result = unsafe { pon_call(method, std::ptr::null_mut(), 0) };
	if result.is_null() {
		return Err(std::ptr::null_mut());
	}
	match int_arg(result, "fd") {
		Ok(fd) => c_int_range(fd),
		Err(_) => {
			crate::thread_state::pon_err_clear();
			Err(raise_type_error("fileno() returned a non-integer"))
		},
	}
}

fn c_int_range(fd: i64) -> Result<libc::c_int, *mut PyObject> {
	if fd < i64::from(i32::MIN) || fd > i64::from(i32::MAX) {
		return Err(raise_value_error("file descriptor is out of range"));
	}
	Ok(fd as libc::c_int)
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

fn c_int_arg(object: *mut PyObject, what: &str) -> Result<libc::c_int, *mut PyObject> {
	let value = int_arg(object, what)?;
	libc::c_int::try_from(value).map_err(|_| raise_value_error(&format!("{what} is out of range")))
}

fn off_t_arg(object: *mut PyObject, what: &str) -> Result<libc::off_t, *mut PyObject> {
	let value = int_arg(object, what)?;
	libc::off_t::try_from(value).map_err(|_| raise_value_error(&format!("{what} is out of range")))
}

fn fcntl_result(result: libc::c_int) -> *mut PyObject {
	if result < 0 {
		super::os::raise_errno(last_errno(), None)
	} else {
		unsafe { pon_const_int(i64::from(result)) }
	}
}

fn bytes_like<'a>(object: *mut PyObject) -> Option<&'a [u8]> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || crate::tag::is_small_int(object) {
		return None;
	}
	let ty = unsafe { (*object).ob_type };
	if bytes_type::is_bytes_type(ty) {
		return Some(unsafe { (*object.cast::<bytes_type::PyBytes>()).as_slice() });
	}
	if bytearray_type::is_bytearray_type(ty) {
		return Some(unsafe { (*object.cast::<bytearray_type::PyByteArray>()).as_slice() });
	}
	None
}

fn last_errno() -> i32 {
	std::io::Error::last_os_error()
		.raw_os_error()
		.unwrap_or(libc::EIO)
}

fn int_arg(object: *mut PyObject, what: &str) -> Result<i64, *mut PyObject> {
	if crate::tag::is_small_int(object) {
		return Ok(crate::tag::untag_small_int(object));
	}
	let raw = crate::tag::untag_arg(object);
	if raw.is_null() {
		return Err(std::ptr::null_mut());
	}
	// SAFETY: Heap pointer with a live header after the tag checks.
	match unsafe { crate::types::int::to_bigint_including_bool(raw) } {
		Some(value) => num_traits::ToPrimitive::to_i64(&value)
			.ok_or_else(|| raise_value_error(&format!("{what} is too large"))),
		None => Err(raise_type_error(&format!("{what} must be an integer"))),
	}
}

fn raise_type_error(message: &str) -> *mut PyObject {
	crate::abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

fn raise_value_error(message: &str) -> *mut PyObject {
	crate::abi::exc::raise_kind_error_text(ExceptionKind::ValueError, message)
}
