//! Native `mmap` module backed by POSIX `mmap(2)`.
//!
//! The `mmap()` constructor creates a real virtual-memory mapping.  The object
//! exposes the common CPython methods used by stdlib and packaging code:
//! read/write, seek/tell, flush, size, close, indexing, and context-manager
//! close semantics.

use std::{ptr, sync::LazyLock};

use num_traits::ToPrimitive;

use super::install_module;
use crate::{
	abi::{self, pon_const_bool, pon_const_int, pon_const_str, pon_make_function},
	intern::intern,
	object::{PyObject, PyObjectHeader, PySequenceMethods, PyType},
	types::{
		bytearray_ as bytearray_type, bytes_ as bytes_type, exc::ExceptionKind, type_::unicode_text,
	},
};

const VARIADIC_ARITY: usize = crate::native::builtins_mod::VARIADIC_ARITY;
const ACCESS_DEFAULT: i64 = 0;
const ACCESS_READ: i64 = 1;
const ACCESS_WRITE: i64 = 2;
const ACCESS_COPY: i64 = 3;

#[cfg(target_os = "macos")]
const CONSTANTS: &[(&str, i64)] = &[
	("ACCESS_COPY", 3),
	("ACCESS_DEFAULT", 0),
	("ACCESS_READ", 1),
	("ACCESS_WRITE", 2),
	("ALLOCATIONGRANULARITY", 16384),
	("MADV_DONTNEED", 4),
	("MADV_FREE", 5),
	("MADV_FREE_REUSABLE", 7),
	("MADV_FREE_REUSE", 8),
	("MADV_NORMAL", 0),
	("MADV_RANDOM", 1),
	("MADV_SEQUENTIAL", 2),
	("MADV_WILLNEED", 3),
	("MAP_32BIT", 32768),
	("MAP_ANON", 4096),
	("MAP_ANONYMOUS", 4096),
	("MAP_HASSEMAPHORE", 512),
	("MAP_JIT", 2048),
	("MAP_NOCACHE", 1024),
	("MAP_NOEXTEND", 256),
	("MAP_NORESERVE", 64),
	("MAP_PRIVATE", 2),
	("MAP_RESILIENT_CODESIGN", 8192),
	("MAP_RESILIENT_MEDIA", 16384),
	("MAP_SHARED", 1),
	("MAP_TPRO", 524288),
	("MAP_TRANSLATED_ALLOW_EXECUTE", 131072),
	("MAP_UNIX03", 262144),
	("PAGESIZE", 16384),
	("PROT_EXEC", 4),
	("PROT_READ", 1),
	("PROT_WRITE", 2),
];

#[cfg(not(target_os = "macos"))]
const CONSTANTS: &[(&str, i64)] = &[
	("ACCESS_COPY", ACCESS_COPY),
	("ACCESS_DEFAULT", ACCESS_DEFAULT),
	("ACCESS_READ", ACCESS_READ),
	("ACCESS_WRITE", ACCESS_WRITE),
	("ALLOCATIONGRANULARITY", libc::PAGESIZE as i64),
	("MAP_ANON", libc::MAP_ANON as i64),
	("MAP_ANONYMOUS", libc::MAP_ANONYMOUS as i64),
	("MAP_PRIVATE", libc::MAP_PRIVATE as i64),
	("MAP_SHARED", libc::MAP_SHARED as i64),
	("PAGESIZE", libc::PAGESIZE as i64),
	("PROT_EXEC", libc::PROT_EXEC as i64),
	("PROT_READ", libc::PROT_READ as i64),
	("PROT_WRITE", libc::PROT_WRITE as i64),
];

#[repr(C)]
struct PyMmap {
	ob_base:  PyObjectHeader,
	data:     *mut u8,
	len:      usize,
	pos:      usize,
	fd:       libc::c_int,
	writable: bool,
	closed:   bool,
}

static MMAP_SEQUENCE: LazyLock<PySequenceMethods> = LazyLock::new(|| PySequenceMethods {
	sq_length: Some(mmap_len_slot),
	sq_item: Some(mmap_item_slot),
	sq_ass_item: Some(mmap_ass_item_slot),
	..PySequenceMethods::EMPTY
});

static MMAP_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"mmap.mmap",
		std::mem::size_of::<PyMmap>(),
	);
	ty.tp_as_sequence = &*MMAP_SEQUENCE as *const PySequenceMethods as *mut PySequenceMethods;
	ty.tp_getattro = Some(mmap_getattro);
	ty.tp_repr = Some(mmap_repr);
	Box::into_raw(Box::new(ty)) as usize
});

fn mmap_type() -> *mut PyType {
	*MMAP_TYPE as *mut PyType
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let name = "mmap";
	let mut attrs = vec![string_attr("__name__", name)?];
	attrs.push((intern("error"), builtin_os_error()?));
	attrs.push((intern("mmap"), unsafe {
		pon_make_function(mmap_constructor as *const u8, VARIADIC_ARITY, intern("mmap"))
	}));
	for &(const_name, value) in CONSTANTS {
		attrs.push(int_attr(const_name, value)?);
	}
	install_module(name, attrs)
}

fn builtin_os_error() -> Result<*mut PyObject, String> {
	crate::import::module_attr(intern("builtins"), intern("OSError"))
		.ok_or_else(|| "failed to resolve builtins.OSError".to_owned())
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
	let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate mmap.{name}"))
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
	let object = unsafe { pon_const_int(value) };
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate mmap.{name}"))
}

unsafe extern "C" fn mmap_constructor(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if (2..=6).contains(&args.len()) => args,
		_ => return raise_type_error(&format!("mmap() expected 2 to 6 arguments, got {argc}")),
	};
	let fd = match c_int_arg(args[0], "fileno") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let mut length = match usize_arg(args[1], "length") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let mut flags = args.get(2).map_or(libc::MAP_SHARED, |&object| {
		int_arg(object, "flags").unwrap_or(libc::MAP_SHARED as i64) as libc::c_int
	});
	let mut prot = args
		.get(3)
		.map_or((libc::PROT_READ | libc::PROT_WRITE) as i64, |&object| {
			int_arg(object, "prot").unwrap_or((libc::PROT_READ | libc::PROT_WRITE) as i64)
		}) as libc::c_int;
	let access = match args.get(4).copied() {
		Some(object) => match int_arg(object, "access") {
			Ok(value) => value,
			Err(error) => return error,
		},
		None => ACCESS_DEFAULT,
	};
	let offset = match args.get(5).copied() {
		Some(object) => match off_t_arg(object, "offset") {
			Ok(value) => value,
			Err(error) => return error,
		},
		None => 0,
	};
	let writable = match access {
		ACCESS_DEFAULT => prot & libc::PROT_WRITE != 0,
		ACCESS_READ => {
			prot = libc::PROT_READ;
			flags = libc::MAP_SHARED;
			false
		},
		ACCESS_WRITE => {
			prot = libc::PROT_READ | libc::PROT_WRITE;
			flags = libc::MAP_SHARED;
			true
		},
		ACCESS_COPY => {
			prot = libc::PROT_READ | libc::PROT_WRITE;
			flags = libc::MAP_PRIVATE;
			true
		},
		_ => return raise_value_error("mmap invalid access parameter"),
	};
	if fd == -1 {
		flags |= libc::MAP_ANON;
		if length == 0 {
			return raise_value_error("cannot mmap an empty file");
		}
	} else if length == 0 {
		let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
		if unsafe { libc::fstat(fd, st.as_mut_ptr()) } != 0 {
			return raise_errno();
		}
		let st = unsafe { st.assume_init() };
		if st.st_size < offset {
			return raise_value_error("mmap offset is greater than file size");
		}
		length = usize::try_from(st.st_size - offset).unwrap_or(0);
	}
	let ptr = unsafe { libc::mmap(ptr::null_mut(), length, prot, flags, fd, offset) };
	if ptr == libc::MAP_FAILED {
		return raise_errno();
	}
	Box::into_raw(Box::new(PyMmap {
		ob_base: PyObjectHeader::new(mmap_type()),
		data: ptr.cast::<u8>(),
		len: length,
		pos: 0,
		fd,
		writable,
		closed: false,
	}))
	.cast::<PyObject>()
}

unsafe extern "C" fn mmap_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let Some(name_text) = (unsafe { unicode_text(crate::tag::untag_arg(name)) }) else {
		return raise_type_error("attribute name must be str");
	};
	match name_text {
		"closed" => unsafe { pon_const_bool(i32::from((*object.cast::<PyMmap>()).closed)) },
		"close" => bound_method(object, name_text, mmap_close_method),
		"read" => bound_method(object, name_text, mmap_read_method),
		"readline" => bound_method(object, name_text, mmap_readline_method),
		"write" => bound_method(object, name_text, mmap_write_method),
		"seek" => bound_method(object, name_text, mmap_seek_method),
		"tell" => bound_method(object, name_text, mmap_tell_method),
		"flush" => bound_method(object, name_text, mmap_flush_method),
		"size" => bound_method(object, name_text, mmap_size_method),
		"find" => bound_method(object, name_text, mmap_find_method),
		"rfind" => bound_method(object, name_text, mmap_rfind_method),
		"__enter__" => bound_method(object, name_text, mmap_enter_method),
		"__exit__" => bound_method(object, name_text, mmap_exit_method),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

fn bound_method(
	receiver: *mut PyObject,
	name: &str,
	entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> *mut PyObject {
	let function = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	if function.is_null() {
		return ptr::null_mut();
	}
	match crate::types::method::new_bound_method(function, receiver) {
		Ok(method) => method.cast::<PyObject>(),
		Err(message) => abi::exc::raise_kind_error_text(ExceptionKind::RuntimeError, &message),
	}
}

unsafe extern "C" fn mmap_repr(object: *mut PyObject) -> *mut PyObject {
	let mmap = unsafe { &*object.cast::<PyMmap>() };
	let text = if mmap.closed {
		"<mmap.mmap closed=true>".to_owned()
	} else {
		format!("<mmap.mmap length={} pos={}>", mmap.len, mmap.pos)
	};
	unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

unsafe extern "C" fn mmap_len_slot(object: *mut PyObject) -> isize {
	let mmap = unsafe { &*object.cast::<PyMmap>() };
	if mmap.closed {
		-1
	} else {
		isize::try_from(mmap.len).unwrap_or(isize::MAX)
	}
}

unsafe extern "C" fn mmap_item_slot(object: *mut PyObject, index: isize) -> *mut PyObject {
	let mmap = match unsafe { mmap_ref(object) } {
		Ok(value) => value,
		Err(error) => return error,
	};
	let index = normalize_index(index, mmap.len).unwrap_or(usize::MAX);
	if index >= mmap.len {
		return raise_index_error("mmap index out of range");
	}
	unsafe { pon_const_int(i64::from(*mmap.data.add(index))) }
}

unsafe extern "C" fn mmap_ass_item_slot(
	object: *mut PyObject,
	index: isize,
	value: *mut PyObject,
) -> libc::c_int {
	let mmap = match unsafe { mmap_mut(object) } {
		Ok(value) => value,
		Err(_) => return -1,
	};
	if !mmap.writable {
		let _ = raise_type_error("mmap can't modify a readonly memory map.");
		return -1;
	}
	let index = normalize_index(index, mmap.len).unwrap_or(usize::MAX);
	if index >= mmap.len {
		let _ = raise_index_error("mmap index out of range");
		return -1;
	}
	let byte = match byte_value(value) {
		Ok(value) => value,
		Err(_) => return -1,
	};
	unsafe { *mmap.data.add(index) = byte };
	0
}

unsafe extern "C" fn mmap_close_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let mmap = match unsafe { receiver_mut(argv, argc, "close", 0) } {
		Ok((mmap, _)) => mmap,
		Err(error) => return error,
	};
	close_mmap(mmap);
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn mmap_enter_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (mmap, _) = match unsafe { receiver_mut(argv, argc, "__enter__", 0) } {
		Ok(value) => value,
		Err(error) => return error,
	};
	mmap as *mut PyMmap as *mut PyObject
}

unsafe extern "C" fn mmap_exit_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let mmap = match unsafe { receiver_mut(argv, argc, "__exit__", 3) } {
		Ok((mmap, _)) => mmap,
		Err(error) => return error,
	};
	close_mmap(mmap);
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn mmap_read_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (mmap, args) = match unsafe { receiver_mut(argv, argc, "read", 1) } {
		Ok(value) => value,
		Err(error) => return error,
	};
	let wanted = match args.first().copied() {
		Some(object) if !is_none(object) => match int_arg(object, "n") {
			Ok(value) if value >= 0 => usize::try_from(value).unwrap_or(usize::MAX),
			Ok(_) => mmap.len.saturating_sub(mmap.pos),
			Err(error) => return error,
		},
		_ => mmap.len.saturating_sub(mmap.pos),
	};
	let n = wanted.min(mmap.len.saturating_sub(mmap.pos));
	let bytes = unsafe { std::slice::from_raw_parts(mmap.data.add(mmap.pos), n) };
	mmap.pos += n;
	unsafe { abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) }
}

unsafe extern "C" fn mmap_readline_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (mmap, _) = match unsafe { receiver_mut(argv, argc, "readline", 0) } {
		Ok(value) => value,
		Err(error) => return error,
	};
	let rest = unsafe {
		std::slice::from_raw_parts(mmap.data.add(mmap.pos), mmap.len.saturating_sub(mmap.pos))
	};
	let n = rest
		.iter()
		.position(|&byte| byte == b'\n')
		.map_or(rest.len(), |pos| pos + 1);
	let bytes = &rest[..n];
	mmap.pos += n;
	unsafe { abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) }
}

unsafe extern "C" fn mmap_write_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (mmap, args) = match unsafe { receiver_mut(argv, argc, "write", 1) } {
		Ok(value) => value,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return raise_type_error("write() argument required");
	}
	if !mmap.writable {
		return raise_type_error("mmap can't modify a readonly memory map.");
	}
	let data = match bytes_like(args[0]) {
		Some(data) => data,
		None => return raise_type_error("a bytes-like object is required"),
	};
	if data.len() > mmap.len.saturating_sub(mmap.pos) {
		return raise_value_error("data out of range");
	}
	unsafe { ptr::copy_nonoverlapping(data.as_ptr(), mmap.data.add(mmap.pos), data.len()) };
	mmap.pos += data.len();
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn mmap_seek_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (mmap, args) = match unsafe { receiver_mut(argv, argc, "seek", 2) } {
		Ok(value) => value,
		Err(error) => return error,
	};
	if args.is_empty() {
		return raise_type_error("seek() missing required argument 'pos'");
	}
	let pos = match int_arg(args[0], "pos") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let whence = match args.get(1).copied() {
		Some(object) => match int_arg(object, "whence") {
			Ok(value) => value,
			Err(error) => return error,
		},
		None => 0,
	};
	let base = match whence {
		0 => 0_i64,
		1 => mmap.pos as i64,
		2 => mmap.len as i64,
		_ => return raise_value_error("unknown seek type"),
	};
	let new_pos = base.saturating_add(pos);
	if new_pos < 0 || usize::try_from(new_pos).map_or(true, |value| value > mmap.len) {
		return raise_value_error("seek out of range");
	}
	mmap.pos = new_pos as usize;
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn mmap_tell_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (mmap, _) = match unsafe { receiver_mut(argv, argc, "tell", 0) } {
		Ok(value) => value,
		Err(error) => return error,
	};
	unsafe { pon_const_int(mmap.pos as i64) }
}

unsafe extern "C" fn mmap_flush_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (mmap, args) = match unsafe { receiver_mut(argv, argc, "flush", 2) } {
		Ok(value) => value,
		Err(error) => return error,
	};
	let offset = match args.first().copied() {
		Some(object) => match usize_arg(object, "offset") {
			Ok(value) => value,
			Err(error) => return error,
		},
		None => 0,
	};
	let size = match args.get(1).copied() {
		Some(object) => match usize_arg(object, "size") {
			Ok(value) => value,
			Err(error) => return error,
		},
		None => mmap.len.saturating_sub(offset),
	};
	if offset > mmap.len || size > mmap.len.saturating_sub(offset) {
		return raise_value_error("flush values out of range");
	}
	if unsafe { libc::msync(mmap.data.add(offset).cast(), size, libc::MS_SYNC) } != 0 {
		return raise_errno();
	}
	unsafe { pon_const_int(0) }
}

unsafe extern "C" fn mmap_size_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (mmap, _) = match unsafe { receiver_mut(argv, argc, "size", 0) } {
		Ok(value) => value,
		Err(error) => return error,
	};
	if mmap.fd < 0 {
		return unsafe { pon_const_int(mmap.len as i64) };
	}
	let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
	if unsafe { libc::fstat(mmap.fd, st.as_mut_ptr()) } != 0 {
		return raise_errno();
	}
	let st = unsafe { st.assume_init() };
	unsafe { pon_const_int(st.st_size as i64) }
}

unsafe extern "C" fn mmap_find_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	find_common(argv, argc, false)
}

unsafe extern "C" fn mmap_rfind_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	find_common(argv, argc, true)
}

fn find_common(argv: *mut *mut PyObject, argc: usize, reverse: bool) -> *mut PyObject {
	let (mmap, args) =
		match unsafe { receiver_mut(argv, argc, if reverse { "rfind" } else { "find" }, 3) } {
			Ok(value) => value,
			Err(error) => return error,
		};
	if args.is_empty() {
		return raise_type_error("find() missing required argument 'sub'");
	}
	let needle = match bytes_like(args[0]) {
		Some(data) => data,
		None => return raise_type_error("a bytes-like object is required"),
	};
	let start = match args.get(1).copied() {
		Some(object) => match usize_arg(object, "start") {
			Ok(value) => value.min(mmap.len),
			Err(error) => return error,
		},
		None => mmap.pos,
	};
	let end = match args.get(2).copied() {
		Some(object) => match usize_arg(object, "end") {
			Ok(value) => value.min(mmap.len),
			Err(error) => return error,
		},
		None => mmap.len,
	};
	let haystack = unsafe { std::slice::from_raw_parts(mmap.data, mmap.len) };
	let range = if start <= end {
		&haystack[start..end]
	} else {
		&[]
	};
	let found = if needle.is_empty() {
		Some(if reverse { end } else { start })
	} else if reverse {
		range
			.windows(needle.len())
			.rposition(|window| window == needle)
			.map(|pos| start + pos)
	} else {
		range
			.windows(needle.len())
			.position(|window| window == needle)
			.map(|pos| start + pos)
	};
	unsafe { pon_const_int(found.map_or(-1, |pos| pos as i64)) }
}

unsafe fn receiver_mut<'a>(
	argv: *mut *mut PyObject,
	argc: usize,
	function: &str,
	max_extra: usize,
) -> Result<(&'a mut PyMmap, &'a [*mut PyObject]), *mut PyObject> {
	let args = unsafe { arg_slice(argv, argc) }
		.ok_or_else(|| raise_type_error("invalid argument vector"))?;
	if args.is_empty() || args.len() > max_extra + 1 {
		return Err(raise_type_error(&format!("{function} expected at most {max_extra} arguments")));
	}
	let mmap = unsafe { mmap_mut(args[0]) }?;
	Ok((mmap, &args[1..]))
}

unsafe fn mmap_ref<'a>(object: *mut PyObject) -> Result<&'a PyMmap, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || unsafe { (*object).ob_type } != mmap_type().cast_const() {
		return Err(raise_type_error("mmap method called with invalid receiver"));
	}
	let mmap = unsafe { &*object.cast::<PyMmap>() };
	if mmap.closed {
		return Err(raise_value_error("mmap closed or invalid"));
	}
	Ok(mmap)
}

unsafe fn mmap_mut<'a>(object: *mut PyObject) -> Result<&'a mut PyMmap, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || unsafe { (*object).ob_type } != mmap_type().cast_const() {
		return Err(raise_type_error("mmap method called with invalid receiver"));
	}
	let mmap = unsafe { &mut *object.cast::<PyMmap>() };
	if mmap.closed {
		return Err(raise_value_error("mmap closed or invalid"));
	}
	Ok(mmap)
}

fn close_mmap(mmap: &mut PyMmap) {
	if !mmap.closed {
		unsafe { libc::munmap(mmap.data.cast(), mmap.len) };
		mmap.closed = true;
		mmap.data = ptr::null_mut();
		mmap.len = 0;
		mmap.pos = 0;
	}
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

fn byte_value(object: *mut PyObject) -> Result<u8, *mut PyObject> {
	let value = int_arg(object, "value")?;
	u8::try_from(value).map_err(|_| {
		raise_value_error(
			"mmap assignment must be single-character bytes or an integer in range(256)",
		)
	})
}

fn normalize_index(index: isize, len: usize) -> Option<usize> {
	if index < 0 {
		len.checked_sub(index.unsigned_abs())
	} else {
		usize::try_from(index).ok()
	}
}

fn is_none(object: *mut PyObject) -> bool {
	unsafe { crate::types::dict::type_name(crate::tag::untag_arg(object)) == Some("NoneType") }
}

fn c_int_arg(object: *mut PyObject, what: &str) -> Result<libc::c_int, *mut PyObject> {
	let value = int_arg(object, what)?;
	libc::c_int::try_from(value).map_err(|_| raise_value_error(&format!("{what} is out of range")))
}

fn off_t_arg(object: *mut PyObject, what: &str) -> Result<libc::off_t, *mut PyObject> {
	let value = int_arg(object, what)?;
	libc::off_t::try_from(value).map_err(|_| raise_value_error(&format!("{what} is out of range")))
}

fn usize_arg(object: *mut PyObject, what: &str) -> Result<usize, *mut PyObject> {
	let value = int_arg(object, what)?;
	usize::try_from(value).map_err(|_| raise_value_error(&format!("{what} is out of range")))
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

fn raise_errno() -> *mut PyObject {
	let errno = std::io::Error::last_os_error()
		.raw_os_error()
		.unwrap_or(libc::EIO);
	super::os::raise_errno(errno, None)
}

fn raise_type_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

fn raise_value_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::ValueError, message)
}

fn raise_index_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::IndexError, message)
}
