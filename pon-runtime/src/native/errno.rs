//! Native `errno` module (WS-IMPORT: `posixpath` -> `os.path` -> `unittest`).
//!
//! CPython's `errno` is a C extension exposing the host's `<errno.h>`
//! constants plus the `errorcode` value->name dict.  pon serves the POSIX
//! set shared by macOS and Linux through the `libc` crate, so values always
//! match the host CPython.  Aliased values (`EWOULDBLOCK`/`EAGAIN`,
//! `ENOTSUP`/`EOPNOTSUPP` on Linux) keep CPython's `errorcode` winner by
//! inserting the loser first.

use super::install_module;
use crate::{intern::intern, object::PyObject};

/// POSIX errno constants available on every supported host, in `errorcode`
/// insertion order (later duplicates of a value overwrite earlier names).
const CONSTANTS: &[(&str, i32)] = &[
	("EPERM", libc::EPERM),
	("ENOENT", libc::ENOENT),
	("ESRCH", libc::ESRCH),
	("EINTR", libc::EINTR),
	("EIO", libc::EIO),
	("ENXIO", libc::ENXIO),
	("E2BIG", libc::E2BIG),
	("ENOEXEC", libc::ENOEXEC),
	("EBADF", libc::EBADF),
	("ECHILD", libc::ECHILD),
	("EWOULDBLOCK", libc::EWOULDBLOCK),
	("EAGAIN", libc::EAGAIN),
	("ENOMEM", libc::ENOMEM),
	("EACCES", libc::EACCES),
	("EFAULT", libc::EFAULT),
	("ENOTBLK", libc::ENOTBLK),
	("EBUSY", libc::EBUSY),
	("EEXIST", libc::EEXIST),
	("EXDEV", libc::EXDEV),
	("ENODEV", libc::ENODEV),
	("ENOTDIR", libc::ENOTDIR),
	("EISDIR", libc::EISDIR),
	("EINVAL", libc::EINVAL),
	("ENFILE", libc::ENFILE),
	("EMFILE", libc::EMFILE),
	("ENOTTY", libc::ENOTTY),
	("ETXTBSY", libc::ETXTBSY),
	("EFBIG", libc::EFBIG),
	("ENOSPC", libc::ENOSPC),
	("ESPIPE", libc::ESPIPE),
	("EROFS", libc::EROFS),
	("EMLINK", libc::EMLINK),
	("EPIPE", libc::EPIPE),
	("EDOM", libc::EDOM),
	("ERANGE", libc::ERANGE),
	("EDEADLK", libc::EDEADLK),
	("ENAMETOOLONG", libc::ENAMETOOLONG),
	("ENOLCK", libc::ENOLCK),
	("ENOSYS", libc::ENOSYS),
	("ENOTEMPTY", libc::ENOTEMPTY),
	("ELOOP", libc::ELOOP),
	("ENOMSG", libc::ENOMSG),
	("EIDRM", libc::EIDRM),
	("ENOSTR", libc::ENOSTR),
	("ENODATA", libc::ENODATA),
	("ETIME", libc::ETIME),
	("ENOSR", libc::ENOSR),
	("EREMOTE", libc::EREMOTE),
	("ENOLINK", libc::ENOLINK),
	("EPROTO", libc::EPROTO),
	("EMULTIHOP", libc::EMULTIHOP),
	("EBADMSG", libc::EBADMSG),
	("EOVERFLOW", libc::EOVERFLOW),
	("EILSEQ", libc::EILSEQ),
	("EUSERS", libc::EUSERS),
	("ENOTSOCK", libc::ENOTSOCK),
	("EDESTADDRREQ", libc::EDESTADDRREQ),
	("EMSGSIZE", libc::EMSGSIZE),
	("EPROTOTYPE", libc::EPROTOTYPE),
	("ENOPROTOOPT", libc::ENOPROTOOPT),
	("EPROTONOSUPPORT", libc::EPROTONOSUPPORT),
	("ESOCKTNOSUPPORT", libc::ESOCKTNOSUPPORT),
	("ENOTSUP", libc::ENOTSUP),
	("EOPNOTSUPP", libc::EOPNOTSUPP),
	("EPFNOSUPPORT", libc::EPFNOSUPPORT),
	("EAFNOSUPPORT", libc::EAFNOSUPPORT),
	("EADDRINUSE", libc::EADDRINUSE),
	("EADDRNOTAVAIL", libc::EADDRNOTAVAIL),
	("ENETDOWN", libc::ENETDOWN),
	("ENETUNREACH", libc::ENETUNREACH),
	("ENETRESET", libc::ENETRESET),
	("ECONNABORTED", libc::ECONNABORTED),
	("ECONNRESET", libc::ECONNRESET),
	("ENOBUFS", libc::ENOBUFS),
	("EISCONN", libc::EISCONN),
	("ENOTCONN", libc::ENOTCONN),
	("ESHUTDOWN", libc::ESHUTDOWN),
	("ETOOMANYREFS", libc::ETOOMANYREFS),
	("ETIMEDOUT", libc::ETIMEDOUT),
	("ECONNREFUSED", libc::ECONNREFUSED),
	("EHOSTDOWN", libc::EHOSTDOWN),
	("EHOSTUNREACH", libc::EHOSTUNREACH),
	("EALREADY", libc::EALREADY),
	("EINPROGRESS", libc::EINPROGRESS),
	("ESTALE", libc::ESTALE),
	("EDQUOT", libc::EDQUOT),
	("ECANCELED", libc::ECANCELED),
	("EOWNERDEAD", libc::EOWNERDEAD),
	("ENOTRECOVERABLE", libc::ENOTRECOVERABLE),
];

#[cfg(target_os = "macos")]
const OS_CONSTANTS: &[(&str, i32)] = &[
	("EAUTH", 80),
	("EBADARCH", 86),
	("EBADEXEC", 85),
	("EBADMACHO", 88),
	("EBADRPC", 72),
	("EDEVERR", 83),
	("EFTYPE", 79),
	("ENEEDAUTH", 81),
	("ENOATTR", 93),
	("ENOPOLICY", 103),
	("ENOTCAPABLE", 107),
	("EPROCLIM", 67),
	("EPROCUNAVAIL", 76),
	("EPROGMISMATCH", 75),
	("EPROGUNAVAIL", 74),
	("EPWROFF", 82),
	("EQFULL", 106),
	("ERPCMISMATCH", 73),
	("ESHLIBVERS", 87),
];

#[cfg(not(target_os = "macos"))]
const OS_CONSTANTS: &[(&str, i32)] = &[];

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let name = "errno";
	// SAFETY: runtime allocation helper; NULL is checked below.
	let name_obj = unsafe { crate::abi::pon_const_str(name.as_ptr(), name.len()) };
	if name_obj.is_null() {
		return Err("failed to allocate errno.__name__".to_owned());
	}
	let mut attrs = vec![(intern("__name__"), name_obj)];
	let mut pairs: Vec<*mut PyObject> =
		Vec::with_capacity((CONSTANTS.len() + OS_CONSTANTS.len()) * 2);
	for &(const_name, value) in CONSTANTS.iter().chain(OS_CONSTANTS) {
		// SAFETY: integer boxing helper; NULL is checked below.
		let boxed = unsafe { crate::abi::pon_const_int(i64::from(value)) };
		if boxed.is_null() {
			return Err(format!("failed to allocate errno.{const_name}"));
		}
		attrs.push((intern(const_name), boxed));
		// SAFETY: string allocation helper; NULL is checked below.
		let name_str = unsafe { crate::abi::pon_const_str(const_name.as_ptr(), const_name.len()) };
		if name_str.is_null() {
			return Err(format!("failed to allocate errno errorcode name {const_name}"));
		}
		pairs.push(boxed);
		pairs.push(name_str);
	}
	// SAFETY: `pairs` holds `CONSTANTS.len() + OS_CONSTANTS.len()` live key/value
	// pairs.
	let errorcode = unsafe {
		crate::abi::map::pon_build_map(pairs.as_mut_ptr(), CONSTANTS.len() + OS_CONSTANTS.len())
	};
	if errorcode.is_null() {
		return Err("failed to allocate errno.errorcode".to_owned());
	}
	attrs.push((intern("errorcode"), errorcode));
	install_module(name, attrs)
}
