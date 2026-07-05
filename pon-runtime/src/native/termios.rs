//! Native `termios` module backed by POSIX terminal APIs.
//!
//! The public functions call the host libc `tc*`/`ioctl(TIOCGWINSZ)` entry
//! points and use CPython's list layout for `tcgetattr`: six integer fields
//! followed by a control-character list.

use std::{ptr, sync::LazyLock};

use num_traits::ToPrimitive;

use super::install_module;
use crate::{
	abi::{self, pon_const_int, pon_const_str, pon_make_function},
	intern::intern,
	object::{PyObject, PyType},
	types::{
		bytearray_ as bytearray_type, bytes_ as bytes_type,
		exc::{ExceptionKind, PyBaseException},
	},
};

const VARIADIC_ARITY: usize = crate::native::builtins_mod::VARIADIC_ARITY;

#[cfg(target_os = "macos")]
const CONSTANTS: &[(&str, i64)] = &[
	("ALTWERASE", 512i64),
	("B0", 0i64),
	("B110", 110i64),
	("B115200", 115200i64),
	("B1200", 1200i64),
	("B134", 134i64),
	("B14400", 14400i64),
	("B150", 150i64),
	("B1800", 1800i64),
	("B19200", 19200i64),
	("B200", 200i64),
	("B230400", 230400i64),
	("B2400", 2400i64),
	("B28800", 28800i64),
	("B300", 300i64),
	("B38400", 38400i64),
	("B4800", 4800i64),
	("B50", 50i64),
	("B57600", 57600i64),
	("B600", 600i64),
	("B7200", 7200i64),
	("B75", 75i64),
	("B76800", 76800i64),
	("B9600", 9600i64),
	("BRKINT", 2i64),
	("BS0", 0i64),
	("BS1", 32768i64),
	("BSDLY", 32768i64),
	("CCAR_OFLOW", 1048576i64),
	("CCTS_OFLOW", 65536i64),
	("CDSR_OFLOW", 524288i64),
	("CDSUSP", 25i64),
	("CDTR_IFLOW", 262144i64),
	("CEOF", 4i64),
	("CEOL", 255i64),
	("CEOT", 4i64),
	("CERASE", 127i64),
	("CFLUSH", 15i64),
	("CIGNORE", 1i64),
	("CINTR", 3i64),
	("CKILL", 21i64),
	("CLNEXT", 22i64),
	("CLOCAL", 32768i64),
	("CQUIT", 28i64),
	("CR0", 0i64),
	("CR1", 4096i64),
	("CR2", 8192i64),
	("CR3", 12288i64),
	("CRDLY", 12288i64),
	("CREAD", 2048i64),
	("CRPRNT", 18i64),
	("CRTSCTS", 196608i64),
	("CRTS_IFLOW", 131072i64),
	("CS5", 0i64),
	("CS6", 256i64),
	("CS7", 512i64),
	("CS8", 768i64),
	("CSIZE", 768i64),
	("CSTART", 17i64),
	("CSTOP", 19i64),
	("CSTOPB", 1024i64),
	("CSUSP", 26i64),
	("CWERASE", 23i64),
	("ECHO", 8i64),
	("ECHOCTL", 64i64),
	("ECHOE", 2i64),
	("ECHOK", 4i64),
	("ECHOKE", 1i64),
	("ECHONL", 16i64),
	("ECHOPRT", 32i64),
	("EXTA", 19200i64),
	("EXTB", 38400i64),
	("EXTPROC", 2048i64),
	("FF0", 0i64),
	("FF1", 16384i64),
	("FFDLY", 16384i64),
	("FIOASYNC", 2147772029i64),
	("FIOCLEX", 536897025i64),
	("FIONBIO", 2147772030i64),
	("FIONCLEX", 536897026i64),
	("FIONREAD", 1074030207i64),
	("FLUSHO", 8388608i64),
	("HUPCL", 16384i64),
	("ICANON", 256i64),
	("ICRNL", 256i64),
	("IEXTEN", 1024i64),
	("IGNBRK", 1i64),
	("IGNCR", 128i64),
	("IGNPAR", 4i64),
	("IMAXBEL", 8192i64),
	("INLCR", 64i64),
	("INPCK", 16i64),
	("ISIG", 128i64),
	("ISTRIP", 32i64),
	("IUTF8", 16384i64),
	("IXANY", 2048i64),
	("IXOFF", 1024i64),
	("IXON", 512i64),
	("MDMBUF", 1048576i64),
	("NCCS", 20i64),
	("NL0", 0i64),
	("NL1", 256i64),
	("NL2", 512i64),
	("NL3", 768i64),
	("NLDLY", 768i64),
	("NOFLSH", 2147483648i64),
	("NOKERNINFO", 33554432i64),
	("OCRNL", 16i64),
	("OFDEL", 131072i64),
	("OFILL", 128i64),
	("ONLCR", 2i64),
	("ONLRET", 64i64),
	("ONOCR", 32i64),
	("ONOEOT", 8i64),
	("OPOST", 1i64),
	("OXTABS", 4i64),
	("PARENB", 4096i64),
	("PARMRK", 8i64),
	("PARODD", 8192i64),
	("PENDIN", 536870912i64),
	("TAB0", 0i64),
	("TAB1", 1024i64),
	("TAB2", 2048i64),
	("TAB3", 4i64),
	("TABDLY", 3076i64),
	("TCIFLUSH", 1i64),
	("TCIOFF", 3i64),
	("TCIOFLUSH", 3i64),
	("TCION", 4i64),
	("TCOFLUSH", 2i64),
	("TCOOFF", 1i64),
	("TCOON", 2i64),
	("TCSADRAIN", 1i64),
	("TCSAFLUSH", 2i64),
	("TCSANOW", 0i64),
	("TCSASOFT", 16i64),
	("TIOCCONS", 2147775586i64),
	("TIOCEXCL", 536900621i64),
	("TIOCGETD", 1074033690i64),
	("TIOCGPGRP", 1074033783i64),
	("TIOCGSIZE", 1074295912i64),
	("TIOCGWINSZ", 1074295912i64),
	("TIOCMBIC", 2147775595i64),
	("TIOCMBIS", 2147775596i64),
	("TIOCMGET", 1074033770i64),
	("TIOCMSET", 2147775597i64),
	("TIOCM_CAR", 64i64),
	("TIOCM_CD", 64i64),
	("TIOCM_CTS", 32i64),
	("TIOCM_DSR", 256i64),
	("TIOCM_DTR", 2i64),
	("TIOCM_LE", 1i64),
	("TIOCM_RI", 128i64),
	("TIOCM_RNG", 128i64),
	("TIOCM_RTS", 4i64),
	("TIOCM_SR", 16i64),
	("TIOCM_ST", 8i64),
	("TIOCNOTTY", 536900721i64),
	("TIOCNXCL", 536900622i64),
	("TIOCOUTQ", 1074033779i64),
	("TIOCPKT", 2147775600i64),
	("TIOCPKT_DATA", 0i64),
	("TIOCPKT_DOSTOP", 32i64),
	("TIOCPKT_FLUSHREAD", 1i64),
	("TIOCPKT_FLUSHWRITE", 2i64),
	("TIOCPKT_NOSTOP", 16i64),
	("TIOCPKT_START", 8i64),
	("TIOCPKT_STOP", 4i64),
	("TIOCSCTTY", 536900705i64),
	("TIOCSETD", 2147775515i64),
	("TIOCSPGRP", 2147775606i64),
	("TIOCSSIZE", 2148037735i64),
	("TIOCSTI", 2147578994i64),
	("TIOCSWINSZ", 2148037735i64),
	("TOSTOP", 4194304i64),
	("VDISCARD", 15i64),
	("VDSUSP", 11i64),
	("VEOF", 0i64),
	("VEOL", 1i64),
	("VEOL2", 2i64),
	("VERASE", 3i64),
	("VINTR", 8i64),
	("VKILL", 5i64),
	("VLNEXT", 14i64),
	("VMIN", 16i64),
	("VQUIT", 9i64),
	("VREPRINT", 6i64),
	("VSTART", 12i64),
	("VSTATUS", 18i64),
	("VSTOP", 13i64),
	("VSUSP", 10i64),
	("VT0", 0i64),
	("VT1", 65536i64),
	("VTDLY", 65536i64),
	("VTIME", 17i64),
	("VWERASE", 4i64),
	("_POSIX_VDISABLE", 255i64),
];

#[cfg(not(target_os = "macos"))]
const CONSTANTS: &[(&str, i64)] = &[
	("B0", libc::B0 as i64),
	("B9600", libc::B9600 as i64),
	("BRKINT", libc::BRKINT as i64),
	("CS8", libc::CS8 as i64),
	("CSIZE", libc::CSIZE as i64),
	("ECHO", libc::ECHO as i64),
	("ICANON", libc::ICANON as i64),
	("ICRNL", libc::ICRNL as i64),
	("IEXTEN", libc::IEXTEN as i64),
	("IGNBRK", libc::IGNBRK as i64),
	("IGNCR", libc::IGNCR as i64),
	("IGNPAR", libc::IGNPAR as i64),
	("INLCR", libc::INLCR as i64),
	("INPCK", libc::INPCK as i64),
	("ISIG", libc::ISIG as i64),
	("ISTRIP", libc::ISTRIP as i64),
	("IXON", libc::IXON as i64),
	("NCCS", libc::NCCS as i64),
	("NOFLSH", libc::NOFLSH as i64),
	("OPOST", libc::OPOST as i64),
	("PARENB", libc::PARENB as i64),
	("PARMRK", libc::PARMRK as i64),
	("TCSAFLUSH", libc::TCSAFLUSH as i64),
	("TCSANOW", libc::TCSANOW as i64),
	("VMIN", libc::VMIN as i64),
	("VTIME", libc::VTIME as i64),
];

static TERMIOS_ERROR_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let base = crate::import::module_attr(intern("builtins"), intern("Exception"))
		.map_or(ptr::null_mut(), |object| object.cast::<PyType>());
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"termios.error",
		std::mem::size_of::<PyBaseException>(),
	);
	ty.tp_base = base;
	ty.tp_getattro = Some(crate::types::exc::exception_getattro);
	ty.tp_setattro = Some(crate::types::exc::exception_setattro);
	Box::into_raw(Box::new(ty)) as usize
});

fn termios_error_type() -> *mut PyType {
	*TERMIOS_ERROR_TYPE as *mut PyType
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let name = "termios";
	let mut attrs = vec![string_attr("__name__", name)?];
	attrs.push((intern("error"), termios_error_type().cast::<PyObject>()));
	for &(const_name, value) in CONSTANTS {
		attrs.push(int_attr(const_name, value)?);
	}
	attrs.push(function_attr("tcgetattr", termios_tcgetattr)?);
	attrs.push(function_attr("tcsetattr", termios_tcsetattr)?);
	attrs.push(function_attr("tcdrain", termios_tcdrain)?);
	attrs.push(function_attr("tcflow", termios_tcflow)?);
	attrs.push(function_attr("tcflush", termios_tcflush)?);
	attrs.push(function_attr("tcsendbreak", termios_tcsendbreak)?);
	attrs.push(function_attr("tcgetwinsize", termios_tcgetwinsize)?);
	attrs.push(function_attr("tcsetwinsize", termios_tcsetwinsize)?);
	install_module(name, attrs)
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
	let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate termios.{name}"))
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
	let object = unsafe { pon_const_int(value) };
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate termios.{name}"))
}

fn function_attr(
	name: &str,
	entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Result<(u32, *mut PyObject), String> {
	let function = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	(!function.is_null())
		.then_some((intern(name), function))
		.ok_or_else(|| format!("failed to allocate termios.{name}"))
}

unsafe extern "C" fn termios_tcgetattr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if args.len() == 1 => args,
		_ => return raise_type_error(&format!("tcgetattr expected 1 argument, got {argc}")),
	};
	let fd = match fd_arg(args[0]) {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let mut term = std::mem::MaybeUninit::<libc::termios>::uninit();
	if unsafe { libc::tcgetattr(fd, term.as_mut_ptr()) } != 0 {
		return raise_errno();
	}
	let term = unsafe { term.assume_init() };
	termios_list(&term)
}

unsafe extern "C" fn termios_tcsetattr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if args.len() == 3 => args,
		_ => return raise_type_error(&format!("tcsetattr expected 3 arguments, got {argc}")),
	};
	let fd = match fd_arg(args[0]) {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let when = match c_int_arg(args[1], "when") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let term = match termios_from_object(args[2]) {
		Ok(term) => term,
		Err(error) => return error,
	};
	if unsafe { libc::tcsetattr(fd, when, &term) } != 0 {
		return raise_errno();
	}
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn termios_tcdrain(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unary_fd_call(argv, argc, "tcdrain", libc::tcdrain)
}

unsafe extern "C" fn termios_tcflow(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if args.len() == 2 => args,
		_ => return raise_type_error(&format!("tcflow expected 2 arguments, got {argc}")),
	};
	let fd = match fd_arg(args[0]) {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let action = match c_int_arg(args[1], "action") {
		Ok(value) => value,
		Err(error) => return error,
	};
	if unsafe { libc::tcflow(fd, action) } != 0 {
		return raise_errno();
	}
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn termios_tcflush(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if args.len() == 2 => args,
		_ => return raise_type_error(&format!("tcflush expected 2 arguments, got {argc}")),
	};
	let fd = match fd_arg(args[0]) {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let queue = match c_int_arg(args[1], "queue") {
		Ok(value) => value,
		Err(error) => return error,
	};
	if unsafe { libc::tcflush(fd, queue) } != 0 {
		return raise_errno();
	}
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn termios_tcsendbreak(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if args.len() == 2 => args,
		_ => return raise_type_error(&format!("tcsendbreak expected 2 arguments, got {argc}")),
	};
	let fd = match fd_arg(args[0]) {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let duration = match c_int_arg(args[1], "duration") {
		Ok(value) => value,
		Err(error) => return error,
	};
	if unsafe { libc::tcsendbreak(fd, duration) } != 0 {
		return raise_errno();
	}
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn termios_tcgetwinsize(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if args.len() == 1 => args,
		_ => return raise_type_error(&format!("tcgetwinsize expected 1 argument, got {argc}")),
	};
	let fd = match fd_arg(args[0]) {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let mut winsize = std::mem::MaybeUninit::<libc::winsize>::uninit();
	if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, winsize.as_mut_ptr()) } != 0 {
		return raise_errno();
	}
	let winsize = unsafe { winsize.assume_init() };
	let mut values = [unsafe { pon_const_int(i64::from(winsize.ws_row)) }, unsafe {
		pon_const_int(i64::from(winsize.ws_col))
	}];
	unsafe { abi::seq::pon_build_tuple(values.as_mut_ptr(), values.len()) }
}

unsafe extern "C" fn termios_tcsetwinsize(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if args.len() == 2 => args,
		_ => return raise_type_error(&format!("tcsetwinsize expected 2 arguments, got {argc}")),
	};
	let fd = match fd_arg(args[0]) {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	let items = match sequence_items(args[1], "winsize") {
		Ok(items) if items.len() == 2 => items,
		Ok(_) => return raise_type_error("tcsetwinsize, arg 2: must be a two-item sequence"),
		Err(error) => return error,
	};
	let rows = match u16_arg(items[0], "rows") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let cols = match u16_arg(items[1], "columns") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let winsize = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
	if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &winsize) } != 0 {
		return raise_errno();
	}
	unsafe { abi::pon_none() }
}

fn unary_fd_call(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
	call: unsafe extern "C" fn(libc::c_int) -> libc::c_int,
) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if args.len() == 1 => args,
		_ => return raise_type_error(&format!("{name} expected 1 argument, got {argc}")),
	};
	let fd = match fd_arg(args[0]) {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	if unsafe { call(fd) } != 0 {
		return raise_errno();
	}
	unsafe { abi::pon_none() }
}

fn termios_list(term: &libc::termios) -> *mut PyObject {
	let mut cc_items = Vec::with_capacity(term.c_cc.len());
	for &byte in &term.c_cc {
		let one = [byte];
		let object = unsafe { abi::str_::pon_const_bytes(one.as_ptr(), one.len()) };
		if object.is_null() {
			return ptr::null_mut();
		}
		cc_items.push(object);
	}
	let cc = unsafe { abi::seq::pon_build_list(cc_items.as_mut_ptr(), cc_items.len()) };
	if cc.is_null() {
		return ptr::null_mut();
	}
	let mut values = [
		unsafe { pon_const_int(term.c_iflag as i64) },
		unsafe { pon_const_int(term.c_oflag as i64) },
		unsafe { pon_const_int(term.c_cflag as i64) },
		unsafe { pon_const_int(term.c_lflag as i64) },
		unsafe { pon_const_int(libc::cfgetispeed(term) as i64) },
		unsafe { pon_const_int(libc::cfgetospeed(term) as i64) },
		cc,
	];
	unsafe { abi::seq::pon_build_list(values.as_mut_ptr(), values.len()) }
}

fn termios_from_object(object: *mut PyObject) -> Result<libc::termios, *mut PyObject> {
	let items = sequence_items(object, "attributes")?;
	if items.len() < 7 {
		return Err(raise_type_error("tcsetattr, arg 3: must be 7 element list"));
	}
	let mut term = unsafe { std::mem::zeroed::<libc::termios>() };
	term.c_iflag = int_arg(items[0], "iflag")? as libc::tcflag_t;
	term.c_oflag = int_arg(items[1], "oflag")? as libc::tcflag_t;
	term.c_cflag = int_arg(items[2], "cflag")? as libc::tcflag_t;
	term.c_lflag = int_arg(items[3], "lflag")? as libc::tcflag_t;
	let ispeed = int_arg(items[4], "ispeed")? as libc::speed_t;
	let ospeed = int_arg(items[5], "ospeed")? as libc::speed_t;
	let cc_items = sequence_items(items[6], "cc")?;
	if cc_items.len() != term.c_cc.len() {
		return Err(raise_type_error("tcsetattr: attributes[6] must have NCCS elements"));
	}
	for (slot, item) in term.c_cc.iter_mut().zip(cc_items) {
		*slot = cc_byte(item)?;
	}
	if unsafe { libc::cfsetispeed(&mut term, ispeed) } != 0
		|| unsafe { libc::cfsetospeed(&mut term, ospeed) } != 0
	{
		return Err(raise_errno());
	}
	Ok(term)
}

fn cc_byte(object: *mut PyObject) -> Result<libc::cc_t, *mut PyObject> {
	if let Some(bytes) = bytes_like(object) {
		if bytes.len() != 1 {
			return Err(raise_type_error(
				"tcsetattr: elements of attributes must be characters or integers",
			));
		}
		return Ok(bytes[0] as libc::cc_t);
	}
	let value = int_arg(object, "control character")?;
	u8::try_from(value)
		.map(|value| value as libc::cc_t)
		.map_err(|_| raise_value_error("byte must be in range(0, 256)"))
}

fn sequence_items(object: *mut PyObject, what: &str) -> Result<Vec<*mut PyObject>, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() {
		return Err(ptr::null_mut());
	}
	match unsafe { crate::types::dict::type_name(object) } {
		Some("list") => {
			let list = unsafe { &*object.cast::<crate::types::list::PyList>() };
			Ok(unsafe { list.as_slice() }.to_vec())
		},
		Some("tuple") => {
			let tuple = unsafe { &*object.cast::<crate::types::tuple::PyTuple>() };
			Ok(unsafe { tuple.as_slice() }.to_vec())
		},
		_ => Err(raise_type_error(&format!("{what} must be a sequence"))),
	}
}

fn bytes_like<'a>(object: *mut PyObject) -> Option<&'a [u8]> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() {
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

unsafe fn arg_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
	if argc == 0 {
		Some(&[])
	} else if argv.is_null() {
		None
	} else {
		Some(unsafe { std::slice::from_raw_parts(argv, argc) })
	}
}

fn fd_arg(object: *mut PyObject) -> Result<libc::c_int, *mut PyObject> {
	let fd = int_arg(object, "fd")?;
	libc::c_int::try_from(fd).map_err(|_| raise_value_error("file descriptor is out of range"))
}

fn c_int_arg(object: *mut PyObject, what: &str) -> Result<libc::c_int, *mut PyObject> {
	let value = int_arg(object, what)?;
	libc::c_int::try_from(value).map_err(|_| raise_value_error(&format!("{what} is out of range")))
}

fn u16_arg(object: *mut PyObject, what: &str) -> Result<u16, *mut PyObject> {
	let value = int_arg(object, what)?;
	u16::try_from(value).map_err(|_| raise_value_error(&format!("{what} is out of range")))
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

fn raise_errno() -> *mut PyObject {
	let errno = std::io::Error::last_os_error()
		.raw_os_error()
		.unwrap_or(libc::EIO);
	let message = std::io::Error::from_raw_os_error(errno).to_string();
	raise_termios_error(&format!("({errno}, '{message}')"))
}

fn raise_termios_error(message: &str) -> *mut PyObject {
	let message_obj = unsafe { pon_const_str(message.as_ptr(), message.len()) };
	if message_obj.is_null() {
		return ptr::null_mut();
	}
	let mut args = [message_obj];
	let exception = unsafe {
		abi::pon_call(termios_error_type().cast::<PyObject>(), args.as_mut_ptr(), args.len())
	};
	if exception.is_null() {
		return ptr::null_mut();
	}
	unsafe { abi::pon_raise(exception, ptr::null_mut()) }
}
