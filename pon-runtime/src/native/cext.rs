//! Native CPython C-extension parity for compression, crypto, and UUID modules.
//!
//! These modules cover independently tractable stdlib accelerators with real
//! Rust/native behavior.  Large subsystem modules such as `_ssl`, `_sqlite3`,
//! `_ctypes`, `_dbm`, and `_decimal` intentionally remain outside this file:
//! they require TLS/OpenSSL, DB-API/sqlite, libffi, ndbm, or libmpdec surfaces.

use core::{ffi::c_int, ptr};
use std::{
	io::{Cursor, Read, Write},
	sync::LazyLock,
	time::{SystemTime, UNIX_EPOCH},
};

use ::blake2::Digest as _;
use ::num_traits::ToPrimitive as _;

use super::{
	builtins_mod::{VARIADIC_ARITY, alloc_tuple},
	install_module,
};
use crate::{
	abi,
	intern::intern,
	object::{PyFunction, PyObject, PyObjectHeader, PyType},
	thread_state::{pon_err_clear, pon_err_message},
	types::{
		bytearray_ as bytearray_type, bytes_ as bytes_type, exc::ExceptionKind,
		memoryview as memoryview_type, type_,
	},
};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

fn raise(kind: ExceptionKind, message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(kind, message)
}

fn type_error(message: &str) -> *mut PyObject {
	raise(ExceptionKind::TypeError, message)
}

fn value_error(message: &str) -> *mut PyObject {
	raise(ExceptionKind::ValueError, message)
}

fn overflow_error(message: &str) -> *mut PyObject {
	raise(ExceptionKind::OverflowError, message)
}

fn not_implemented(message: &str) -> *mut PyObject {
	raise(ExceptionKind::NotImplementedError, message)
}

fn py_str(text: &str) -> *mut PyObject {
	// SAFETY: Runtime copies the UTF-8 bytes; NULL carries the pending error.
	unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn py_bytes(bytes: &[u8]) -> *mut PyObject {
	// SAFETY: Runtime copies the byte slice; NULL carries the pending error.
	unsafe { abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) }
}

fn py_int(value: i64) -> *mut PyObject {
	// SAFETY: Runtime integer allocator follows the NULL-sentinel contract.
	unsafe { abi::pon_const_int(value) }
}

fn py_bool(value: bool) -> *mut PyObject {
	// SAFETY: Bool constructor returns the shared singleton.
	unsafe { abi::pon_const_bool(c_int::from(value)) }
}

fn none() -> *mut PyObject {
	// SAFETY: Returns the process singleton.
	unsafe { abi::pon_none() }
}

fn is_none(object: *mut PyObject) -> bool {
	crate::tag::untag_arg(object) == none()
}

unsafe fn argv_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
	if argc == 0 {
		Some(&[])
	} else if argv.is_null() {
		None
	} else {
		Some(unsafe { core::slice::from_raw_parts(argv, argc) })
	}
}

fn args_or_type_error<'a>(
	argv: *mut *mut PyObject,
	argc: usize,
	function: &str,
) -> Result<&'a [*mut PyObject], *mut PyObject> {
	unsafe { argv_slice(argv, argc) }
		.ok_or_else(|| type_error(&format!("{function}() received a null argument vector")))
}

fn type_name(object: *mut PyObject) -> &'static str {
	unsafe { crate::types::dict::type_name(object) }.unwrap_or("object")
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
	if memoryview_type::is_memoryview_type(ty) {
		let view = unsafe { &*object.cast::<memoryview_type::PyMemoryView>() };
		if view.released {
			return None;
		}
		return Some(unsafe { view.as_slice() });
	}
	None
}

fn bytes_arg(object: *mut PyObject, name: &str) -> Result<Vec<u8>, *mut PyObject> {
	bytes_like(object).map(<[u8]>::to_vec).ok_or_else(|| {
		type_error(&format!(
			"a bytes-like object is required for {name}, not '{}'",
			type_name(crate::tag::untag_arg(object))
		))
	})
}

fn str_arg(object: *mut PyObject, name: &str) -> Result<String, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	unsafe { type_::unicode_text(object) }
		.map(str::to_owned)
		.ok_or_else(|| type_error(&format!("{name} must be str, not '{}'", type_name(object))))
}

fn int_arg(object: *mut PyObject, name: &str) -> Result<i64, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	let Some(value) = (unsafe { crate::types::int::to_bigint_including_bool(object) }) else {
		return Err(type_error(&format!("{name} must be an integer")));
	};
	value
		.to_i64()
		.ok_or_else(|| overflow_error(&format!("{name} is too large")))
}

fn optional_int_arg(
	args: &[*mut PyObject],
	index: usize,
	default: i64,
	name: &str,
) -> Result<i64, *mut PyObject> {
	match args.get(index).copied() {
		Some(object) if !object.is_null() && !is_none(object) => int_arg(object, name),
		_ => Ok(default),
	}
}

fn function_attr(
	attr: &str,
	function_name: &str,
	entry: BuiltinFn,
) -> Result<(u32, *mut PyObject), String> {
	// SAFETY: Entry points are live Rust functions using Pon's variadic ABI.
	let function =
		unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(function_name)) };
	(!function.is_null())
		.then_some((intern(attr), function))
		.ok_or_else(|| format!("failed to allocate native function {function_name}"))
}

fn bound_method(receiver: *mut PyObject, name: &str, entry: BuiltinFn) -> *mut PyObject {
	let function =
		unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	if function.is_null() {
		return ptr::null_mut();
	}
	match crate::types::method::new_bound_method(function, receiver) {
		Ok(method) => method.cast::<PyObject>(),
		Err(message) => type_error(&message),
	}
}

fn module_name_attr(name: &str) -> Result<(u32, *mut PyObject), String> {
	let object = py_str(name);
	(!object.is_null())
		.then_some((intern("__name__"), object))
		.ok_or_else(|| format!("failed to allocate {name}.__name__"))
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
	let object = py_int(value);
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate integer attribute {name}"))
}

fn bool_attr(name: &str, value: bool) -> Result<(u32, *mut PyObject), String> {
	let object = py_bool(value);
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate boolean attribute {name}"))
}

fn str_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
	let object = py_str(value);
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate string attribute {name}"))
}

fn exception_class(module: &str, name: &str, base: &str) -> Result<*mut PyObject, String> {
	let base_class = unsafe { abi::pon_load_global(intern(base), ptr::null_mut()) };
	if base_class.is_null() {
		pon_err_clear();
		return Err(format!("builtin class '{base}' is not registered"));
	}
	let namespace = type_::new_namespace();
	if namespace.is_null() {
		return Err(format!("failed to allocate {module}.{name} namespace"));
	}
	let module_object = py_str(module);
	if module_object.is_null() {
		return Err(format!("failed to allocate {module}.{name}.__module__"));
	}
	unsafe { (*namespace).set(intern("__module__"), module_object) };
	let class = unsafe { type_::build_class_from_namespace(name, &[base_class], namespace, &[]) };
	if class.is_null() {
		let detail = pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
		pon_err_clear();
		return Err(format!("failed to create {module}.{name}: {detail}"));
	}
	unsafe {
		if (*class).ob_type.is_null() {
			(*class).ob_type = abi::runtime_type_type().cast_const();
		}
	}
	Ok(class)
}

fn raise_class(class_slot: &LazyLock<usize>, fallback: ExceptionKind, text: &str) -> *mut PyObject {
	let class = *LazyLock::force(class_slot);
	if class == 0 {
		return raise(fallback, text);
	}
	let message = py_str(text);
	if message.is_null() {
		return ptr::null_mut();
	}
	let mut argv = [message];
	let instance = unsafe { abi::pon_call(class as *mut PyObject, argv.as_mut_ptr(), argv.len()) };
	if instance.is_null() {
		return ptr::null_mut();
	}
	unsafe { abi::exc::pon_raise(instance, ptr::null_mut()) }
}

// ---------------------------------------------------------------------------
// `_hashlib` and `_hmac`

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DigestKind {
	Md5,
	Sha1,
	Sha224,
	Sha256,
	Sha384,
	Sha512,
	Sha3_224,
	Sha3_256,
	Sha3_384,
	Sha3_512,
	Shake128,
	Shake256,
	Blake2b,
	Blake2s,
}

impl DigestKind {
	const ALL: [Self; 14] = [
		Self::Md5,
		Self::Sha1,
		Self::Sha224,
		Self::Sha256,
		Self::Sha384,
		Self::Sha512,
		Self::Sha3_224,
		Self::Sha3_256,
		Self::Sha3_384,
		Self::Sha3_512,
		Self::Shake128,
		Self::Shake256,
		Self::Blake2b,
		Self::Blake2s,
	];
	const HMAC: [Self; 12] = [
		Self::Md5,
		Self::Sha1,
		Self::Sha224,
		Self::Sha256,
		Self::Sha384,
		Self::Sha512,
		Self::Sha3_224,
		Self::Sha3_256,
		Self::Sha3_384,
		Self::Sha3_512,
		Self::Blake2b,
		Self::Blake2s,
	];

	const fn name(self) -> &'static str {
		match self {
			Self::Md5 => "md5",
			Self::Sha1 => "sha1",
			Self::Sha224 => "sha224",
			Self::Sha256 => "sha256",
			Self::Sha384 => "sha384",
			Self::Sha512 => "sha512",
			Self::Sha3_224 => "sha3_224",
			Self::Sha3_256 => "sha3_256",
			Self::Sha3_384 => "sha3_384",
			Self::Sha3_512 => "sha3_512",
			Self::Shake128 => "shake_128",
			Self::Shake256 => "shake_256",
			Self::Blake2b => "blake2b",
			Self::Blake2s => "blake2s",
		}
	}

	const fn openssl_name(self) -> &'static str {
		match self {
			Self::Md5 => "openssl_md5",
			Self::Sha1 => "openssl_sha1",
			Self::Sha224 => "openssl_sha224",
			Self::Sha256 => "openssl_sha256",
			Self::Sha384 => "openssl_sha384",
			Self::Sha512 => "openssl_sha512",
			Self::Sha3_224 => "openssl_sha3_224",
			Self::Sha3_256 => "openssl_sha3_256",
			Self::Sha3_384 => "openssl_sha3_384",
			Self::Sha3_512 => "openssl_sha3_512",
			Self::Shake128 => "openssl_shake_128",
			Self::Shake256 => "openssl_shake_256",
			Self::Blake2b => "openssl_blake2b",
			Self::Blake2s => "openssl_blake2s",
		}
	}

	const fn digest_size(self) -> usize {
		match self {
			Self::Md5 => 16,
			Self::Sha1 => 20,
			Self::Sha224 | Self::Sha3_224 => 28,
			Self::Sha256 | Self::Sha3_256 | Self::Blake2s => 32,
			Self::Sha384 | Self::Sha3_384 => 48,
			Self::Sha512 | Self::Sha3_512 | Self::Blake2b => 64,
			Self::Shake128 | Self::Shake256 => 0,
		}
	}

	const fn block_size(self) -> usize {
		match self {
			Self::Md5 | Self::Sha1 | Self::Sha224 | Self::Sha256 | Self::Blake2s => 64,
			Self::Sha384 | Self::Sha512 | Self::Blake2b => 128,
			Self::Sha3_224 => 144,
			Self::Sha3_256 | Self::Shake256 => 136,
			Self::Sha3_384 => 104,
			Self::Sha3_512 => 72,
			Self::Shake128 => 168,
		}
	}

	const fn is_xof(self) -> bool {
		matches!(self, Self::Shake128 | Self::Shake256)
	}

	fn from_name(name: &str) -> Option<Self> {
		match name.to_ascii_lowercase().replace('-', "_").as_str() {
			"md5" => Some(Self::Md5),
			"sha1" | "sha" => Some(Self::Sha1),
			"sha224" => Some(Self::Sha224),
			"sha256" => Some(Self::Sha256),
			"sha384" => Some(Self::Sha384),
			"sha512" => Some(Self::Sha512),
			"sha3_224" => Some(Self::Sha3_224),
			"sha3_256" => Some(Self::Sha3_256),
			"sha3_384" => Some(Self::Sha3_384),
			"sha3_512" => Some(Self::Sha3_512),
			"shake_128" => Some(Self::Shake128),
			"shake_256" => Some(Self::Shake256),
			"blake2b" => Some(Self::Blake2b),
			"blake2s" => Some(Self::Blake2s),
			_ => None,
		}
	}
}

#[repr(C)]
struct PyOpenSslHash {
	ob_base: PyObjectHeader,
	kind:    DigestKind,
	data:    Vec<u8>,
}

#[repr(C)]
struct PyHmac {
	ob_base: PyObjectHeader,
	kind:    DigestKind,
	key:     Vec<u8>,
	data:    Vec<u8>,
}

fn hashlib_hash_type_slot(name: &'static str) -> usize {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		name,
		core::mem::size_of::<PyOpenSslHash>(),
	);
	ty.tp_getattro = Some(hash_getattro);
	Box::into_raw(Box::new(ty)) as usize
}

static HASH_TYPE: LazyLock<usize> = LazyLock::new(|| hashlib_hash_type_slot("_hashlib.HASH"));
static HASHXOF_TYPE: LazyLock<usize> = LazyLock::new(|| hashlib_hash_type_slot("_hashlib.HASHXOF"));
static HMAC_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"_hashlib.HMAC",
		core::mem::size_of::<PyHmac>(),
	);
	ty.tp_getattro = Some(hmac_getattro);
	Box::into_raw(Box::new(ty)) as usize
});
static HMAC_HACL_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"_hmac.HMAC",
		core::mem::size_of::<PyHmac>(),
	);
	ty.tp_getattro = Some(hmac_getattro);
	Box::into_raw(Box::new(ty)) as usize
});
static UNSUPPORTED_DIGESTMOD_ERROR: LazyLock<usize> = LazyLock::new(|| {
	exception_class("_hashlib", "UnsupportedDigestmodError", "ValueError")
		.map_or(0, |class| class as usize)
});
static UNKNOWN_HASH_ERROR: LazyLock<usize> = LazyLock::new(|| {
	exception_class("_hmac", "UnknownHashError", "ValueError").map_or(0, |class| class as usize)
});

fn digest_bytes(kind: DigestKind, data: &[u8]) -> Vec<u8> {
	match kind {
		DigestKind::Md5 => ::md5::Md5::digest(data).to_vec(),
		DigestKind::Sha1 => ::sha1::Sha1::digest(data).to_vec(),
		DigestKind::Sha224 => ::sha2::Sha224::digest(data).to_vec(),
		DigestKind::Sha256 => ::sha2::Sha256::digest(data).to_vec(),
		DigestKind::Sha384 => ::sha2::Sha384::digest(data).to_vec(),
		DigestKind::Sha512 => ::sha2::Sha512::digest(data).to_vec(),
		DigestKind::Sha3_224 => ::sha3::Sha3_224::digest(data).to_vec(),
		DigestKind::Sha3_256 => ::sha3::Sha3_256::digest(data).to_vec(),
		DigestKind::Sha3_384 => ::sha3::Sha3_384::digest(data).to_vec(),
		DigestKind::Sha3_512 => ::sha3::Sha3_512::digest(data).to_vec(),
		DigestKind::Blake2b => ::blake2::Blake2b512::digest(data).to_vec(),
		DigestKind::Blake2s => ::blake2::Blake2s256::digest(data).to_vec(),
		DigestKind::Shake128 | DigestKind::Shake256 => Vec::new(),
	}
}

fn shake_digest_bytes(kind: DigestKind, data: &[u8], length: usize) -> Vec<u8> {
	use ::sha3::digest::{ExtendableOutput as _, Update as _};
	let mut out = vec![0_u8; length];
	match kind {
		DigestKind::Shake128 => {
			let mut hasher = ::sha3::Shake128::default();
			hasher.update(data);
			let mut reader = hasher.finalize_xof();
			let _ = ::sha3::digest::XofReader::read(&mut reader, &mut out);
		},
		DigestKind::Shake256 => {
			let mut hasher = ::sha3::Shake256::default();
			hasher.update(data);
			let mut reader = hasher.finalize_xof();
			let _ = ::sha3::digest::XofReader::read(&mut reader, &mut out);
		},
		_ => {},
	}
	out
}

fn hex_digest(bytes: &[u8]) -> String {
	const HEX: &[u8; 16] = b"0123456789abcdef";
	let mut out = String::with_capacity(bytes.len() * 2);
	for byte in bytes {
		out.push(HEX[(byte >> 4) as usize] as char);
		out.push(HEX[(byte & 0x0f) as usize] as char);
	}
	out
}

fn hmac_digest_bytes(kind: DigestKind, key: &[u8], data: &[u8]) -> Vec<u8> {
	let block = kind.block_size();
	let mut key_block = if key.len() > block {
		digest_bytes(kind, key)
	} else {
		key.to_vec()
	};
	key_block.resize(block, 0);
	let mut inner = Vec::with_capacity(block + data.len());
	inner.extend(key_block.iter().map(|byte| byte ^ 0x36));
	inner.extend_from_slice(data);
	let inner_digest = digest_bytes(kind, &inner);
	let mut outer = Vec::with_capacity(block + inner_digest.len());
	outer.extend(key_block.iter().map(|byte| byte ^ 0x5c));
	outer.extend_from_slice(&inner_digest);
	digest_bytes(kind, &outer)
}

fn alloc_hash(kind: DigestKind, data: Vec<u8>) -> *mut PyObject {
	let ty = if kind.is_xof() {
		*HASHXOF_TYPE
	} else {
		*HASH_TYPE
	} as *mut PyType;
	Box::into_raw(Box::new(PyOpenSslHash { ob_base: PyObjectHeader::new(ty), kind, data }))
		.cast::<PyObject>()
}

fn alloc_hmac(kind: DigestKind, key: Vec<u8>, data: Vec<u8>, hacl: bool) -> *mut PyObject {
	let ty = if hacl { *HMAC_HACL_TYPE } else { *HMAC_TYPE } as *mut PyType;
	Box::into_raw(Box::new(PyHmac { ob_base: PyObjectHeader::new(ty), kind, key, data }))
		.cast::<PyObject>()
}

unsafe fn hash_receiver<'a>(object: *mut PyObject) -> Option<&'a mut PyOpenSslHash> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() {
		return None;
	}
	let ty = unsafe { (*object).ob_type };
	if ty == (*HASH_TYPE as *mut PyType).cast_const()
		|| ty == (*HASHXOF_TYPE as *mut PyType).cast_const()
	{
		Some(unsafe { &mut *object.cast::<PyOpenSslHash>() })
	} else {
		None
	}
}

unsafe fn hmac_receiver<'a>(object: *mut PyObject) -> Option<&'a mut PyHmac> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() {
		return None;
	}
	let ty = unsafe { (*object).ob_type };
	if ty == (*HMAC_TYPE as *mut PyType).cast_const()
		|| ty == (*HMAC_HACL_TYPE as *mut PyType).cast_const()
	{
		Some(unsafe { &mut *object.cast::<PyHmac>() })
	} else {
		None
	}
}

fn parse_hash_new_args(
	args: &[*mut PyObject],
	function_name: &str,
) -> Result<(DigestKind, Vec<u8>), *mut PyObject> {
	if args.is_empty() {
		return Err(type_error(&format!("{function_name}() missing required argument 'name'")));
	}
	if args.len() > 3 {
		return Err(type_error(&format!(
			"{function_name}() expected at most 3 arguments, got {}",
			args.len()
		)));
	}
	let name = str_arg(args[0], "name")?;
	let Some(kind) = DigestKind::from_name(&name) else {
		return Err(value_error(&format!("unsupported hash type {name}")));
	};
	let data = match args.get(1).copied() {
		Some(object) if !object.is_null() && !is_none(object) => bytes_arg(object, "data")?,
		_ => Vec::new(),
	};
	Ok((kind, data))
}

fn parse_constructor_data(
	args: &[*mut PyObject],
	kind: DigestKind,
) -> Result<Vec<u8>, *mut PyObject> {
	if args.len() > 2 {
		return Err(type_error(&format!(
			"{}() expected at most 2 arguments, got {}",
			kind.openssl_name(),
			args.len()
		)));
	}
	match args.first().copied() {
		Some(object) if !object.is_null() && !is_none(object) => bytes_arg(object, "data"),
		_ => Ok(Vec::new()),
	}
}

unsafe extern "C" fn hashlib_new_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "_hashlib.new") {
		Ok(args) => args,
		Err(error) => return error,
	};
	match parse_hash_new_args(args, "_hashlib.new") {
		Ok((kind, data)) => alloc_hash(kind, data),
		Err(error) => error,
	}
}

macro_rules! hash_constructor {
	($entry:ident, $kind:expr) => {
		unsafe extern "C" fn $entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
			let args = match args_or_type_error(argv, argc, stringify!($entry)) {
				Ok(args) => args,
				Err(error) => return error,
			};
			match parse_constructor_data(args, $kind) {
				Ok(data) => alloc_hash($kind, data),
				Err(error) => error,
			}
		}
	};
}

hash_constructor!(openssl_md5_entry, DigestKind::Md5);
hash_constructor!(openssl_sha1_entry, DigestKind::Sha1);
hash_constructor!(openssl_sha224_entry, DigestKind::Sha224);
hash_constructor!(openssl_sha256_entry, DigestKind::Sha256);
hash_constructor!(openssl_sha384_entry, DigestKind::Sha384);
hash_constructor!(openssl_sha512_entry, DigestKind::Sha512);
hash_constructor!(openssl_sha3_224_entry, DigestKind::Sha3_224);
hash_constructor!(openssl_sha3_256_entry, DigestKind::Sha3_256);
hash_constructor!(openssl_sha3_384_entry, DigestKind::Sha3_384);
hash_constructor!(openssl_sha3_512_entry, DigestKind::Sha3_512);
hash_constructor!(openssl_shake_128_entry, DigestKind::Shake128);
hash_constructor!(openssl_shake_256_entry, DigestKind::Shake256);

macro_rules! hash_method {
	($name:ident, $method:literal, | $this:ident, $args:ident | $body:block) => {
		unsafe extern "C" fn $name(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
			let raw = match args_or_type_error(argv, argc, $method) {
				Ok(raw) => raw,
				Err(error) => return error,
			};
			if raw.is_empty() {
				return type_error(concat!($method, "() missing receiver"));
			}
			let Some($this) = (unsafe { hash_receiver(raw[0]) }) else {
				return type_error(concat!($method, "() receiver must be a hash object"));
			};
			let $args = &raw[1..];
			$body
		}
	};
}

hash_method!(hash_update_entry, "update", |this, args| {
	if args.len() != 1 {
		return type_error(&format!("update() expected 1 argument, got {}", args.len()));
	}
	let data = match bytes_arg(args[0], "data") {
		Ok(data) => data,
		Err(error) => return error,
	};
	this.data.extend_from_slice(&data);
	none()
});

hash_method!(hash_digest_entry, "digest", |this, args| {
	let digest = if this.kind.is_xof() {
		if args.len() != 1 {
			return type_error("digest() missing required length for SHAKE hash");
		}
		let length = match int_arg(args[0], "length") {
			Ok(value) if value >= 0 => value as usize,
			Ok(_) => return value_error("length must be non-negative"),
			Err(error) => return error,
		};
		shake_digest_bytes(this.kind, &this.data, length)
	} else {
		if !args.is_empty() {
			return type_error("digest() takes no arguments");
		}
		digest_bytes(this.kind, &this.data)
	};
	py_bytes(&digest)
});

hash_method!(hash_hexdigest_entry, "hexdigest", |this, args| {
	let digest = if this.kind.is_xof() {
		if args.len() != 1 {
			return type_error("hexdigest() missing required length for SHAKE hash");
		}
		let length = match int_arg(args[0], "length") {
			Ok(value) if value >= 0 => value as usize,
			Ok(_) => return value_error("length must be non-negative"),
			Err(error) => return error,
		};
		shake_digest_bytes(this.kind, &this.data, length)
	} else {
		if !args.is_empty() {
			return type_error("hexdigest() takes no arguments");
		}
		digest_bytes(this.kind, &this.data)
	};
	py_str(&hex_digest(&digest))
});

hash_method!(hash_copy_entry, "copy", |this, args| {
	if !args.is_empty() {
		return type_error("copy() takes no arguments");
	}
	alloc_hash(this.kind, this.data.clone())
});

unsafe extern "C" fn hash_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { type_::unicode_text(name) }) else {
		return type_error("attribute name must be str");
	};
	let Some(this) = (unsafe { hash_receiver(object) }) else {
		return type_error("hash getattro on non-hash receiver");
	};
	match name_text {
		"name" => return py_str(this.kind.name()),
		"digest_size" => return py_int(this.kind.digest_size() as i64),
		"block_size" => return py_int(this.kind.block_size() as i64),
		"update" => bound_method(object, "update", hash_update_entry),
		"digest" => bound_method(object, "digest", hash_digest_entry),
		"hexdigest" => bound_method(object, "hexdigest", hash_hexdigest_entry),
		"copy" => bound_method(object, "copy", hash_copy_entry),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

fn digest_kind_arg(
	object: *mut PyObject,
	slot_name: &str,
	error_class: &LazyLock<usize>,
) -> Result<DigestKind, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if let Some(name) = unsafe { type_::unicode_text(object) } {
		return DigestKind::from_name(name)
			.filter(|kind| DigestKind::HMAC.contains(kind))
			.ok_or_else(|| {
				raise_class(
					error_class,
					ExceptionKind::ValueError,
					&format!("unsupported hash type {name}"),
				)
			});
	}
	if unsafe { crate::types::int::type_name_is(object, "function") } {
		let name_id = unsafe { (*object.cast::<PyFunction>()).name_interned };
		if let Some(name) = crate::intern::resolve(name_id) {
			let normalized = name.strip_prefix("openssl_").unwrap_or(&name);
			if let Some(kind) =
				DigestKind::from_name(normalized).filter(|kind| DigestKind::HMAC.contains(kind))
			{
				return Ok(kind);
			}
		}
		return Err(raise_class(
			error_class,
			ExceptionKind::ValueError,
			&format!("Unsupported digestmod {}", crate::native::builtins_mod::repr_text(object)),
		));
	}
	Err(type_error(&format!(
		"{slot_name} must be str or a builtin hash constructor, not '{}'",
		type_name(object)
	)))
}

fn parse_hmac_args(
	args: &[*mut PyObject],
	function_name: &str,
	error_class: &LazyLock<usize>,
) -> Result<(Vec<u8>, Vec<u8>, DigestKind), *mut PyObject> {
	if args.is_empty() {
		return Err(type_error(&format!("{function_name}() missing required argument 'key'")));
	}
	if args.len() > 3 {
		return Err(type_error(&format!(
			"{function_name}() expected at most 3 arguments, got {}",
			args.len()
		)));
	}
	let key = bytes_arg(args[0], "key")?;
	let msg = match args.get(1).copied() {
		Some(object) if !object.is_null() && !is_none(object) => bytes_arg(object, "msg")?,
		_ => Vec::new(),
	};
	let kind = match args.get(2).copied() {
		Some(object) if !object.is_null() && !is_none(object) => {
			digest_kind_arg(object, "digestmod", error_class)?
		},
		_ => return Err(type_error("Missing required argument 'digestmod'.")),
	};
	Ok((key, msg, kind))
}

unsafe extern "C" fn hashlib_hmac_new_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "hmac_new") {
		Ok(args) => args,
		Err(error) => return error,
	};
	match parse_hmac_args(args, "hmac_new", &UNSUPPORTED_DIGESTMOD_ERROR) {
		Ok((key, msg, kind)) => alloc_hmac(kind, key, msg, false),
		Err(error) => error,
	}
}

unsafe extern "C" fn hmac_new_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "_hmac.new") {
		Ok(args) => args,
		Err(error) => return error,
	};
	match parse_hmac_args(args, "_hmac.new", &UNKNOWN_HASH_ERROR) {
		Ok((key, msg, kind)) => alloc_hmac(kind, key, msg, true),
		Err(error) => error,
	}
}

macro_rules! hmac_method {
	($name:ident, $method:literal, | $this:ident, $args:ident | $body:block) => {
		unsafe extern "C" fn $name(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
			let raw = match args_or_type_error(argv, argc, $method) {
				Ok(raw) => raw,
				Err(error) => return error,
			};
			if raw.is_empty() {
				return type_error(concat!($method, "() missing receiver"));
			}
			let Some($this) = (unsafe { hmac_receiver(raw[0]) }) else {
				return type_error(concat!($method, "() receiver must be an HMAC object"));
			};
			let $args = &raw[1..];
			$body
		}
	};
}

hmac_method!(hmac_update_entry, "update", |this, args| {
	if args.len() != 1 {
		return type_error(&format!("update() expected 1 argument, got {}", args.len()));
	}
	let data = match bytes_arg(args[0], "msg") {
		Ok(data) => data,
		Err(error) => return error,
	};
	this.data.extend_from_slice(&data);
	none()
});

hmac_method!(hmac_digest_method_entry, "digest", |this, args| {
	if !args.is_empty() {
		return type_error("digest() takes no arguments");
	}
	py_bytes(&hmac_digest_bytes(this.kind, &this.key, &this.data))
});

hmac_method!(hmac_hexdigest_entry, "hexdigest", |this, args| {
	if !args.is_empty() {
		return type_error("hexdigest() takes no arguments");
	}
	py_str(&hex_digest(&hmac_digest_bytes(this.kind, &this.key, &this.data)))
});

hmac_method!(hmac_copy_entry, "copy", |this, args| {
	if !args.is_empty() {
		return type_error("copy() takes no arguments");
	}
	let is_hacl = this.ob_base.ob_type == (*HMAC_HACL_TYPE as *mut PyType).cast_const();
	alloc_hmac(this.kind, this.key.clone(), this.data.clone(), is_hacl)
});

unsafe extern "C" fn hmac_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { type_::unicode_text(name) }) else {
		return type_error("attribute name must be str");
	};
	let Some(this) = (unsafe { hmac_receiver(object) }) else {
		return type_error("HMAC getattro on non-HMAC receiver");
	};
	match name_text {
		"name" => return py_str(&format!("hmac-{}", this.kind.name())),
		"digest_size" => return py_int(this.kind.digest_size() as i64),
		"block_size" => return py_int(this.kind.block_size() as i64),
		"update" => bound_method(object, "update", hmac_update_entry),
		"digest" => bound_method(object, "digest", hmac_digest_method_entry),
		"hexdigest" => bound_method(object, "hexdigest", hmac_hexdigest_entry),
		"copy" => bound_method(object, "copy", hmac_copy_entry),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

unsafe extern "C" fn hmac_digest_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "hmac_digest") {
		Ok(args) => args,
		Err(error) => return error,
	};
	match parse_hmac_args(args, "hmac_digest", &UNSUPPORTED_DIGESTMOD_ERROR) {
		Ok((key, msg, kind)) => py_bytes(&hmac_digest_bytes(kind, &key, &msg)),
		Err(error) => error,
	}
}

unsafe extern "C" fn hmac_compute_digest_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "_hmac.compute_digest") {
		Ok(args) => args,
		Err(error) => return error,
	};
	match parse_hmac_args(args, "_hmac.compute_digest", &UNKNOWN_HASH_ERROR) {
		Ok((key, msg, kind)) => py_bytes(&hmac_digest_bytes(kind, &key, &msg)),
		Err(error) => error,
	}
}

macro_rules! fixed_hmac_compute {
	($entry:ident, $kind:expr) => {
		unsafe extern "C" fn $entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
			let args = match args_or_type_error(argv, argc, stringify!($entry)) {
				Ok(args) => args,
				Err(error) => return error,
			};
			if args.len() != 2 {
				return type_error(&format!(
					"{}() expected 2 arguments, got {}",
					stringify!($entry),
					args.len()
				));
			}
			let key = match bytes_arg(args[0], "key") {
				Ok(value) => value,
				Err(error) => return error,
			};
			let msg = match bytes_arg(args[1], "msg") {
				Ok(value) => value,
				Err(error) => return error,
			};
			py_bytes(&hmac_digest_bytes($kind, &key, &msg))
		}
	};
}

fixed_hmac_compute!(compute_md5_entry, DigestKind::Md5);
fixed_hmac_compute!(compute_sha1_entry, DigestKind::Sha1);
fixed_hmac_compute!(compute_sha224_entry, DigestKind::Sha224);
fixed_hmac_compute!(compute_sha256_entry, DigestKind::Sha256);
fixed_hmac_compute!(compute_sha384_entry, DigestKind::Sha384);
fixed_hmac_compute!(compute_sha512_entry, DigestKind::Sha512);
fixed_hmac_compute!(compute_sha3_224_entry, DigestKind::Sha3_224);
fixed_hmac_compute!(compute_sha3_256_entry, DigestKind::Sha3_256);
fixed_hmac_compute!(compute_sha3_384_entry, DigestKind::Sha3_384);
fixed_hmac_compute!(compute_sha3_512_entry, DigestKind::Sha3_512);
fixed_hmac_compute!(compute_blake2b_32_entry, DigestKind::Blake2b);
fixed_hmac_compute!(compute_blake2s_32_entry, DigestKind::Blake2s);

unsafe extern "C" fn compare_digest_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "compare_digest") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 2 {
		return type_error(&format!("compare_digest() expected 2 arguments, got {}", args.len()));
	}
	let left = crate::tag::untag_arg(args[0]);
	let right = crate::tag::untag_arg(args[1]);
	let (Some(left_bytes), Some(right_bytes)) = (bytes_like(left), bytes_like(right)) else {
		let (Some(left_text), Some(right_text)) =
			(unsafe { type_::unicode_text(left) }, unsafe { type_::unicode_text(right) })
		else {
			return type_error("unsupported operand types for compare_digest");
		};
		return py_bool(constant_time_eq(left_text.as_bytes(), right_text.as_bytes()));
	};
	py_bool(constant_time_eq(left_bytes, right_bytes))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
	let mut diff = left.len() ^ right.len();
	let max = left.len().max(right.len());
	for index in 0..max {
		let a = left.get(index).copied().unwrap_or(0);
		let b = right.get(index).copied().unwrap_or(0);
		diff |= usize::from(a ^ b);
	}
	diff == 0
}

unsafe extern "C" fn pbkdf2_hmac_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "pbkdf2_hmac") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() < 4 || args.len() > 5 {
		return type_error(&format!("pbkdf2_hmac() expected 4 or 5 arguments, got {}", args.len()));
	}
	let digest_name = match str_arg(args[0], "hash_name") {
		Ok(name) => name,
		Err(error) => return error,
	};
	let Some(kind) =
		DigestKind::from_name(&digest_name).filter(|kind| DigestKind::HMAC.contains(kind))
	else {
		return value_error(&format!("unsupported hash type {digest_name}"));
	};
	let password = match bytes_arg(args[1], "password") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let salt = match bytes_arg(args[2], "salt") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let iterations = match int_arg(args[3], "iterations") {
		Ok(value) if value > 0 => value as usize,
		Ok(_) => return value_error("iteration value must be greater than 0"),
		Err(error) => return error,
	};
	let dklen = match args.get(4).copied() {
		Some(object) if !object.is_null() && !is_none(object) => match int_arg(object, "dklen") {
			Ok(value) if value > 0 => value as usize,
			Ok(_) => return value_error("key length must be greater than 0"),
			Err(error) => return error,
		},
		_ => kind.digest_size(),
	};
	py_bytes(&pbkdf2_hmac(kind, &password, &salt, iterations, dklen))
}

fn pbkdf2_hmac(
	kind: DigestKind,
	password: &[u8],
	salt: &[u8],
	iterations: usize,
	dklen: usize,
) -> Vec<u8> {
	let hlen = kind.digest_size();
	let blocks = dklen.div_ceil(hlen);
	let mut out = Vec::with_capacity(blocks * hlen);
	for block_index in 1..=blocks {
		let mut salt_block = Vec::with_capacity(salt.len() + 4);
		salt_block.extend_from_slice(salt);
		salt_block.extend_from_slice(&(block_index as u32).to_be_bytes());
		let mut u = hmac_digest_bytes(kind, password, &salt_block);
		let mut t = u.clone();
		for _ in 1..iterations {
			u = hmac_digest_bytes(kind, password, &u);
			for (acc, byte) in t.iter_mut().zip(&u) {
				*acc ^= *byte;
			}
		}
		out.extend_from_slice(&t);
	}
	out.truncate(dklen);
	out
}

unsafe extern "C" fn scrypt_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "scrypt") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() < 6 || args.len() > 7 {
		return type_error(&format!(
			"scrypt() expected password, salt, n, r, p, maxmem, dklen; got {} arguments",
			args.len()
		));
	}
	let password = match bytes_arg(args[0], "password") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let salt = match bytes_arg(args[1], "salt") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let n = match int_arg(args[2], "n") {
		Ok(value) if value > 1 && (value as u64).is_power_of_two() => value as u64,
		Ok(_) => return value_error("n must be a power of 2 greater than 1"),
		Err(error) => return error,
	};
	let r = match int_arg(args[3], "r") {
		Ok(value) if value > 0 => value as u32,
		Ok(_) => return value_error("r must be positive"),
		Err(error) => return error,
	};
	let p = match int_arg(args[4], "p") {
		Ok(value) if value > 0 => value as u32,
		Ok(_) => return value_error("p must be positive"),
		Err(error) => return error,
	};
	let _maxmem = match optional_int_arg(args, 5, 0, "maxmem") {
		Ok(value) if value >= 0 => value,
		Ok(_) => return value_error("maxmem must be positive or zero"),
		Err(error) => return error,
	};
	let dklen = match optional_int_arg(args, 6, 64, "dklen") {
		Ok(value) if value > 0 => value as usize,
		Ok(_) => return value_error("dklen must be positive"),
		Err(error) => return error,
	};
	let log_n = n.trailing_zeros() as u8;
	let params = match ::scrypt::Params::new(log_n, r, p, dklen) {
		Ok(params) => params,
		Err(error) => return value_error(&format!("Invalid parameter combination: {error}")),
	};
	let mut out = vec![0_u8; dklen];
	match ::scrypt::scrypt(&password, &salt, &params, &mut out) {
		Ok(()) => py_bytes(&out),
		Err(error) => value_error(&format!("scrypt failed: {error}")),
	}
}

unsafe extern "C" fn get_fips_mode_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	py_int(0)
}

fn hash_constructor_entry(kind: DigestKind) -> BuiltinFn {
	match kind {
		DigestKind::Md5 => openssl_md5_entry,
		DigestKind::Sha1 => openssl_sha1_entry,
		DigestKind::Sha224 => openssl_sha224_entry,
		DigestKind::Sha256 => openssl_sha256_entry,
		DigestKind::Sha384 => openssl_sha384_entry,
		DigestKind::Sha512 => openssl_sha512_entry,
		DigestKind::Sha3_224 => openssl_sha3_224_entry,
		DigestKind::Sha3_256 => openssl_sha3_256_entry,
		DigestKind::Sha3_384 => openssl_sha3_384_entry,
		DigestKind::Sha3_512 => openssl_sha3_512_entry,
		DigestKind::Shake128 => openssl_shake_128_entry,
		DigestKind::Shake256 => openssl_shake_256_entry,
		DigestKind::Blake2b | DigestKind::Blake2s => openssl_sha256_entry,
	}
}

pub(super) fn make_hashlib_module() -> Result<*mut PyObject, String> {
	let unsupported = *UNSUPPORTED_DIGESTMOD_ERROR;
	if unsupported == 0 {
		return Err("failed to create _hashlib.UnsupportedDigestmodError".to_owned());
	}
	let mut attrs = vec![module_name_attr("_hashlib")?];
	attrs.push((intern("HASH"), (*HASH_TYPE as *mut PyType).cast::<PyObject>()));
	attrs.push((intern("HASHXOF"), (*HASHXOF_TYPE as *mut PyType).cast::<PyObject>()));
	attrs.push((intern("HMAC"), (*HMAC_TYPE as *mut PyType).cast::<PyObject>()));
	attrs.push((intern("UnsupportedDigestmodError"), unsupported as *mut PyObject));
	attrs.push(int_attr("_GIL_MINSIZE", 2048)?);
	attrs.push(function_attr("new", "openssl_new", hashlib_new_entry)?);
	attrs.push(function_attr("compare_digest", "compare_digest", compare_digest_entry)?);
	attrs.push(function_attr("pbkdf2_hmac", "pbkdf2_hmac", pbkdf2_hmac_entry)?);
	attrs.push(function_attr("scrypt", "scrypt", scrypt_entry)?);
	attrs.push(function_attr("hmac_new", "hmac_new", hashlib_hmac_new_entry)?);
	attrs.push(function_attr("hmac_digest", "hmac_digest", hmac_digest_entry)?);
	attrs.push(function_attr("get_fips_mode", "get_fips_mode", get_fips_mode_entry)?);
	let mut constructor_pairs = Vec::new();
	for kind in DigestKind::ALL {
		if matches!(kind, DigestKind::Blake2b | DigestKind::Blake2s) {
			continue;
		}
		let attr =
			function_attr(kind.openssl_name(), kind.openssl_name(), hash_constructor_entry(kind))?;
		let digest_name = py_str(kind.name());
		if digest_name.is_null() {
			return Err("failed to allocate _hashlib._constructors entry".to_owned());
		}
		constructor_pairs.push(attr.1);
		constructor_pairs.push(digest_name);
		attrs.push(attr);
	}
	let constructors = unsafe {
		abi::map::pon_build_map(constructor_pairs.as_mut_ptr(), constructor_pairs.len() / 2)
	};
	if constructors.is_null() {
		return Err("failed to allocate _hashlib._constructors".to_owned());
	}
	attrs.push((intern("_constructors"), constructors));
	let mut names = Vec::new();
	for kind in DigestKind::ALL {
		let name = py_str(kind.name());
		if name.is_null() {
			return Err("failed to allocate _hashlib.openssl_md_meth_names entry".to_owned());
		}
		names.push(name);
	}
	let set = unsafe { abi::map::pon_build_set(names.as_mut_ptr(), names.len()) };
	if set.is_null() {
		return Err("failed to allocate _hashlib.openssl_md_meth_names".to_owned());
	}
	attrs.push((intern("openssl_md_meth_names"), set));
	install_module("_hashlib", attrs)
}

pub(super) fn make_hmac_module() -> Result<*mut PyObject, String> {
	let unknown = *UNKNOWN_HASH_ERROR;
	if unknown == 0 {
		return Err("failed to create _hmac.UnknownHashError".to_owned());
	}
	let mut attrs = vec![module_name_attr("_hmac")?];
	attrs.push((intern("HMAC"), (*HMAC_HACL_TYPE as *mut PyType).cast::<PyObject>()));
	attrs.push((intern("UnknownHashError"), unknown as *mut PyObject));
	attrs.push(int_attr("_GIL_MINSIZE", 2048)?);
	attrs.push(function_attr("new", "_hmac_new", hmac_new_entry)?);
	attrs.push(function_attr("compute_digest", "_hmac_compute_digest", hmac_compute_digest_entry)?);
	attrs.push(function_attr("compute_md5", "compute_md5", compute_md5_entry)?);
	attrs.push(function_attr("compute_sha1", "compute_sha1", compute_sha1_entry)?);
	attrs.push(function_attr("compute_sha224", "compute_sha224", compute_sha224_entry)?);
	attrs.push(function_attr("compute_sha256", "compute_sha256", compute_sha256_entry)?);
	attrs.push(function_attr("compute_sha384", "compute_sha384", compute_sha384_entry)?);
	attrs.push(function_attr("compute_sha512", "compute_sha512", compute_sha512_entry)?);
	attrs.push(function_attr("compute_sha3_224", "compute_sha3_224", compute_sha3_224_entry)?);
	attrs.push(function_attr("compute_sha3_256", "compute_sha3_256", compute_sha3_256_entry)?);
	attrs.push(function_attr("compute_sha3_384", "compute_sha3_384", compute_sha3_384_entry)?);
	attrs.push(function_attr("compute_sha3_512", "compute_sha3_512", compute_sha3_512_entry)?);
	attrs.push(function_attr("compute_blake2b_32", "compute_blake2b_32", compute_blake2b_32_entry)?);
	attrs.push(function_attr("compute_blake2s_32", "compute_blake2s_32", compute_blake2s_32_entry)?);
	install_module("_hmac", attrs)
}

// ---------------------------------------------------------------------------
// `_bz2`

#[repr(C)]
struct PyBz2Compressor {
	ob_base: PyObjectHeader,
	level:   u32,
	input:   Vec<u8>,
	flushed: bool,
}

#[repr(C)]
struct PyBz2Decompressor {
	ob_base:     PyObjectHeader,
	input:       Vec<u8>,
	output:      Vec<u8>,
	offset:      usize,
	eof:         bool,
	needs_input: bool,
	unused_data: Vec<u8>,
}

static BZ2_COMPRESSOR_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"_bz2.BZ2Compressor",
		core::mem::size_of::<PyBz2Compressor>(),
	);
	ty.tp_base = abi::runtime_global(intern("object"))
		.map_or(ptr::null_mut(), |object| object.cast::<PyType>());
	ty.tp_new = Some(bz2_compressor_new);
	ty.tp_getattro = Some(bz2_compressor_getattro);
	Box::into_raw(Box::new(ty)) as usize
});

static BZ2_DECOMPRESSOR_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"_bz2.BZ2Decompressor",
		core::mem::size_of::<PyBz2Decompressor>(),
	);
	ty.tp_base = abi::runtime_global(intern("object"))
		.map_or(ptr::null_mut(), |object| object.cast::<PyType>());
	ty.tp_new = Some(bz2_decompressor_new);
	ty.tp_getattro = Some(bz2_decompressor_getattro);
	Box::into_raw(Box::new(ty)) as usize
});

unsafe extern "C" fn bz2_compressor_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	let positional = match unsafe { type_::positional_args_from_object(args) } {
		Ok(values) => values,
		Err(message) => return type_error(&message),
	};
	if positional.len() > 1 {
		return type_error(&format!(
			"BZ2Compressor() expected at most 1 argument, got {}",
			positional.len()
		));
	}
	let mut level = match positional.first().copied() {
		Some(object) => match int_arg(object, "compresslevel") {
			Ok(value) => value,
			Err(error) => return error,
		},
		None => 9,
	};
	if !kwargs.is_null() {
		let entries = match unsafe { crate::types::dict::dict_entries_snapshot(kwargs) } {
			Ok(entries) => entries,
			Err(message) => return type_error(&message),
		};
		for entry in entries {
			let Some(key) = (unsafe { type_::unicode_text(crate::tag::untag_arg(entry.key)) }) else {
				return type_error("BZ2Compressor() keywords must be strings");
			};
			match key {
				"compresslevel" => match int_arg(entry.value, "compresslevel") {
					Ok(value) => level = value,
					Err(error) => return error,
				},
				other => {
					return type_error(&format!(
						"BZ2Compressor() got an unexpected keyword argument '{other}'"
					));
				},
			}
		}
	}
	if !(1..=9).contains(&level) {
		return value_error("compresslevel must be between 1 and 9");
	}
	Box::into_raw(Box::new(PyBz2Compressor {
		ob_base: PyObjectHeader::new(*BZ2_COMPRESSOR_TYPE as *mut PyType),
		level:   level as u32,
		input:   Vec::new(),
		flushed: false,
	}))
	.cast::<PyObject>()
}

unsafe extern "C" fn bz2_decompressor_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	let positional = match unsafe { type_::positional_args_from_object(args) } {
		Ok(values) => values,
		Err(message) => return type_error(&message),
	};
	if !positional.is_empty() || !kwargs.is_null() {
		return type_error("BZ2Decompressor() takes no arguments");
	}
	Box::into_raw(Box::new(PyBz2Decompressor {
		ob_base:     PyObjectHeader::new(*BZ2_DECOMPRESSOR_TYPE as *mut PyType),
		input:       Vec::new(),
		output:      Vec::new(),
		offset:      0,
		eof:         false,
		needs_input: true,
		unused_data: Vec::new(),
	}))
	.cast::<PyObject>()
}

unsafe fn bz2_compressor_receiver<'a>(object: *mut PyObject) -> Option<&'a mut PyBz2Compressor> {
	let object = crate::tag::untag_arg(object);
	(!object.is_null()
		&& unsafe { (*object).ob_type == (*BZ2_COMPRESSOR_TYPE as *mut PyType).cast_const() })
	.then(|| unsafe { &mut *object.cast::<PyBz2Compressor>() })
}

unsafe fn bz2_decompressor_receiver<'a>(
	object: *mut PyObject,
) -> Option<&'a mut PyBz2Decompressor> {
	let object = crate::tag::untag_arg(object);
	(!object.is_null()
		&& unsafe { (*object).ob_type == (*BZ2_DECOMPRESSOR_TYPE as *mut PyType).cast_const() })
	.then(|| unsafe { &mut *object.cast::<PyBz2Decompressor>() })
}

unsafe extern "C" fn bz2_compress_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let raw = match args_or_type_error(argv, argc, "compress") {
		Ok(raw) => raw,
		Err(error) => return error,
	};
	if raw.len() != 2 {
		return type_error(&format!(
			"compress() expected 1 argument, got {}",
			raw.len().saturating_sub(1)
		));
	}
	let Some(this) = (unsafe { bz2_compressor_receiver(raw[0]) }) else {
		return type_error("compress() receiver must be BZ2Compressor");
	};
	if this.flushed {
		return value_error("Compressor has been flushed");
	}
	let data = match bytes_arg(raw[1], "data") {
		Ok(data) => data,
		Err(error) => return error,
	};
	this.input.extend_from_slice(&data);
	py_bytes(&[])
}

unsafe extern "C" fn bz2_flush_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let raw = match args_or_type_error(argv, argc, "flush") {
		Ok(raw) => raw,
		Err(error) => return error,
	};
	if raw.len() != 1 {
		return type_error(&format!(
			"flush() expected no arguments, got {}",
			raw.len().saturating_sub(1)
		));
	}
	let Some(this) = (unsafe { bz2_compressor_receiver(raw[0]) }) else {
		return type_error("flush() receiver must be BZ2Compressor");
	};
	if this.flushed {
		return value_error("Repeated call to flush()");
	}
	this.flushed = true;
	let mut encoder =
		::bzip2::write::BzEncoder::new(Vec::new(), ::bzip2::Compression::new(this.level));
	if let Err(error) = encoder.write_all(&this.input) {
		return raise(ExceptionKind::OSError, &format!("bzip2 compress failed: {error}"));
	}
	match encoder.finish() {
		Ok(bytes) => py_bytes(&bytes),
		Err(error) => raise(ExceptionKind::OSError, &format!("bzip2 compress failed: {error}")),
	}
}

fn bz2_decode_first_stream(input: &[u8]) -> Result<(Vec<u8>, usize), String> {
	if input.len() >= 3 && &input[..3] != b"BZh" {
		return Err("Invalid data stream".to_owned());
	}
	let mut cursor = Cursor::new(input);
	let mut decoder = ::bzip2::read::BzDecoder::new(&mut cursor);
	let mut out = Vec::new();
	decoder
		.read_to_end(&mut out)
		.map_err(|error| error.to_string())?;
	Ok((out, cursor.position() as usize))
}

fn take_decompressed(output: &[u8], offset: &mut usize, max_length: i64) -> Vec<u8> {
	let remaining = output.len().saturating_sub(*offset);
	let take = if max_length < 0 {
		remaining
	} else {
		remaining.min(max_length as usize)
	};
	let chunk = output[*offset..*offset + take].to_vec();
	*offset += take;
	chunk
}

unsafe extern "C" fn bz2_decompress_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let raw = match args_or_type_error(argv, argc, "decompress") {
		Ok(raw) => raw,
		Err(error) => return error,
	};
	if raw.len() < 2 || raw.len() > 3 {
		return type_error(&format!(
			"decompress() expected 1 or 2 arguments, got {}",
			raw.len().saturating_sub(1)
		));
	}
	let Some(this) = (unsafe { bz2_decompressor_receiver(raw[0]) }) else {
		return type_error("decompress() receiver must be BZ2Decompressor");
	};
	let data = match bytes_arg(raw[1], "data") {
		Ok(data) => data,
		Err(error) => return error,
	};
	let max_length = match raw.get(2).copied() {
		Some(object) => match int_arg(object, "max_length") {
			Ok(value) => value,
			Err(error) => return error,
		},
		None => -1,
	};
	if max_length < -1 {
		return value_error("max_length must be non-negative");
	}
	if this.eof {
		if !data.is_empty() {
			this.unused_data.extend_from_slice(&data);
		}
		return py_bytes(&[]);
	}
	this.input.extend_from_slice(&data);
	match bz2_decode_first_stream(&this.input) {
		Ok((out, consumed)) => {
			this.output = out;
			this.eof = true;
			this.unused_data = this.input.get(consumed..).unwrap_or(&[]).to_vec();
			let chunk = take_decompressed(&this.output, &mut this.offset, max_length);
			this.needs_input = this.offset >= this.output.len();
			py_bytes(&chunk)
		},
		Err(_message) if this.input.len() < 4 => {
			this.needs_input = true;
			py_bytes(&[])
		},
		Err(message) => raise(ExceptionKind::OSError, &message),
	}
}

unsafe extern "C" fn bz2_compressor_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { type_::unicode_text(name) }) else {
		return type_error("attribute name must be str");
	};
	match name_text {
		"compress" => bound_method(object, "compress", bz2_compress_method),
		"flush" => bound_method(object, "flush", bz2_flush_method),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

unsafe extern "C" fn bz2_decompressor_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { type_::unicode_text(name) }) else {
		return type_error("attribute name must be str");
	};
	let Some(this) = (unsafe { bz2_decompressor_receiver(object) }) else {
		return type_error("attribute receiver must be BZ2Decompressor");
	};
	match name_text {
		"eof" => py_bool(this.eof),
		"needs_input" => py_bool(this.needs_input),
		"unused_data" => py_bytes(&this.unused_data),
		"decompress" => bound_method(object, "decompress", bz2_decompress_method),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

pub(super) fn make_bz2_module() -> Result<*mut PyObject, String> {
	install_module("_bz2", vec![
		module_name_attr("_bz2")?,
		(intern("BZ2Compressor"), (*BZ2_COMPRESSOR_TYPE as *mut PyType).cast::<PyObject>()),
		(intern("BZ2Decompressor"), (*BZ2_DECOMPRESSOR_TYPE as *mut PyType).cast::<PyObject>()),
	])
}

// ---------------------------------------------------------------------------
// `_lzma`

#[repr(C)]
struct PyLzmaCompressor {
	ob_base: PyObjectHeader,
	input:   Vec<u8>,
	preset:  u32,
	flushed: bool,
}

#[repr(C)]
struct PyLzmaDecompressor {
	ob_base:     PyObjectHeader,
	input:       Vec<u8>,
	output:      Vec<u8>,
	offset:      usize,
	eof:         bool,
	needs_input: bool,
	unused_data: Vec<u8>,
	check:       i64,
}

static LZMA_ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
	exception_class("_lzma", "LZMAError", "Exception").map_or(0, |class| class as usize)
});

static LZMA_COMPRESSOR_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"_lzma.LZMACompressor",
		core::mem::size_of::<PyLzmaCompressor>(),
	);
	ty.tp_base = abi::runtime_global(intern("object"))
		.map_or(ptr::null_mut(), |object| object.cast::<PyType>());
	ty.tp_new = Some(lzma_compressor_new);
	ty.tp_getattro = Some(lzma_compressor_getattro);
	Box::into_raw(Box::new(ty)) as usize
});

static LZMA_DECOMPRESSOR_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"_lzma.LZMADecompressor",
		core::mem::size_of::<PyLzmaDecompressor>(),
	);
	ty.tp_base = abi::runtime_global(intern("object"))
		.map_or(ptr::null_mut(), |object| object.cast::<PyType>());
	ty.tp_new = Some(lzma_decompressor_new);
	ty.tp_getattro = Some(lzma_decompressor_getattro);
	Box::into_raw(Box::new(ty)) as usize
});

fn raise_lzma(text: &str) -> *mut PyObject {
	raise_class(&LZMA_ERROR_CLASS, ExceptionKind::ValueError, text)
}

unsafe extern "C" fn lzma_compressor_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	let positional = match unsafe { type_::positional_args_from_object(args) } {
		Ok(values) => values,
		Err(message) => return type_error(&message),
	};
	let mut slots = [ptr::null_mut(); 4];
	for (idx, value) in positional.iter().take(4).enumerate() {
		slots[idx] = *value;
	}
	if positional.len() > 4 {
		return type_error(&format!(
			"LZMACompressor() expected at most 4 arguments, got {}",
			positional.len()
		));
	}
	if !kwargs.is_null() {
		let entries = match unsafe { crate::types::dict::dict_entries_snapshot(kwargs) } {
			Ok(entries) => entries,
			Err(message) => return type_error(&message),
		};
		for entry in entries {
			let Some(key) = (unsafe { type_::unicode_text(crate::tag::untag_arg(entry.key)) }) else {
				return type_error("LZMACompressor() keywords must be strings");
			};
			let index = match key {
				"format" => 0,
				"check" => 1,
				"preset" => 2,
				"filters" => 3,
				other => {
					return type_error(&format!(
						"LZMACompressor() got an unexpected keyword argument '{other}'"
					));
				},
			};
			slots[index] = entry.value;
		}
	}
	let format = if slots[0].is_null() || is_none(slots[0]) {
		1
	} else {
		match int_arg(slots[0], "format") {
			Ok(v) => v,
			Err(e) => return e,
		}
	};
	if format != 1 {
		return not_implemented("pon _lzma currently supports FORMAT_XZ compression only");
	}
	if !slots[3].is_null() && !is_none(slots[3]) {
		return not_implemented("custom LZMA filter chains are not implemented in pon");
	}
	let preset = if slots[2].is_null() || is_none(slots[2]) {
		6
	} else {
		match int_arg(slots[2], "preset") {
			Ok(v) => v,
			Err(e) => return e,
		}
	};
	let level = (preset & 0x0f).clamp(0, 9) as u32;
	Box::into_raw(Box::new(PyLzmaCompressor {
		ob_base: PyObjectHeader::new(*LZMA_COMPRESSOR_TYPE as *mut PyType),
		input:   Vec::new(),
		preset:  level,
		flushed: false,
	}))
	.cast::<PyObject>()
}

unsafe extern "C" fn lzma_decompressor_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	let positional = match unsafe { type_::positional_args_from_object(args) } {
		Ok(values) => values,
		Err(message) => return type_error(&message),
	};
	let mut slots = [ptr::null_mut(); 3];
	for (idx, value) in positional.iter().take(3).enumerate() {
		slots[idx] = *value;
	}
	if positional.len() > 3 {
		return type_error(&format!(
			"LZMADecompressor() expected at most 3 arguments, got {}",
			positional.len()
		));
	}
	if !kwargs.is_null() {
		let entries = match unsafe { crate::types::dict::dict_entries_snapshot(kwargs) } {
			Ok(entries) => entries,
			Err(message) => return type_error(&message),
		};
		for entry in entries {
			let Some(key) = (unsafe { type_::unicode_text(crate::tag::untag_arg(entry.key)) }) else {
				return type_error("LZMADecompressor() keywords must be strings");
			};
			let index = match key {
				"format" => 0,
				"memlimit" => 1,
				"filters" => 2,
				other => {
					return type_error(&format!(
						"LZMADecompressor() got an unexpected keyword argument '{other}'"
					));
				},
			};
			slots[index] = entry.value;
		}
	}
	let format = if slots[0].is_null() || is_none(slots[0]) {
		0
	} else {
		match int_arg(slots[0], "format") {
			Ok(v) => v,
			Err(e) => return e,
		}
	};
	if format != 0 && format != 1 {
		return not_implemented(
			"pon _lzma currently supports FORMAT_AUTO/FORMAT_XZ decompression only",
		);
	}
	if !slots[2].is_null() && !is_none(slots[2]) {
		return not_implemented("custom LZMA filter chains are not implemented in pon");
	}
	Box::into_raw(Box::new(PyLzmaDecompressor {
		ob_base:     PyObjectHeader::new(*LZMA_DECOMPRESSOR_TYPE as *mut PyType),
		input:       Vec::new(),
		output:      Vec::new(),
		offset:      0,
		eof:         false,
		needs_input: true,
		unused_data: Vec::new(),
		check:       4,
	}))
	.cast::<PyObject>()
}

unsafe fn lzma_compressor_receiver<'a>(object: *mut PyObject) -> Option<&'a mut PyLzmaCompressor> {
	let object = crate::tag::untag_arg(object);
	(!object.is_null()
		&& unsafe { (*object).ob_type == (*LZMA_COMPRESSOR_TYPE as *mut PyType).cast_const() })
	.then(|| unsafe { &mut *object.cast::<PyLzmaCompressor>() })
}

unsafe fn lzma_decompressor_receiver<'a>(
	object: *mut PyObject,
) -> Option<&'a mut PyLzmaDecompressor> {
	let object = crate::tag::untag_arg(object);
	(!object.is_null()
		&& unsafe { (*object).ob_type == (*LZMA_DECOMPRESSOR_TYPE as *mut PyType).cast_const() })
	.then(|| unsafe { &mut *object.cast::<PyLzmaDecompressor>() })
}

unsafe extern "C" fn lzma_compress_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let raw = match args_or_type_error(argv, argc, "compress") {
		Ok(raw) => raw,
		Err(error) => return error,
	};
	if raw.len() != 2 {
		return type_error(&format!(
			"compress() expected 1 argument, got {}",
			raw.len().saturating_sub(1)
		));
	}
	let Some(this) = (unsafe { lzma_compressor_receiver(raw[0]) }) else {
		return type_error("compress() receiver must be LZMACompressor");
	};
	if this.flushed {
		return value_error("Compressor has been flushed");
	}
	let data = match bytes_arg(raw[1], "data") {
		Ok(data) => data,
		Err(error) => return error,
	};
	this.input.extend_from_slice(&data);
	py_bytes(&[])
}

unsafe extern "C" fn lzma_flush_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let raw = match args_or_type_error(argv, argc, "flush") {
		Ok(raw) => raw,
		Err(error) => return error,
	};
	if raw.len() != 1 {
		return type_error(&format!(
			"flush() expected no arguments, got {}",
			raw.len().saturating_sub(1)
		));
	}
	let Some(this) = (unsafe { lzma_compressor_receiver(raw[0]) }) else {
		return type_error("flush() receiver must be LZMACompressor");
	};
	if this.flushed {
		return value_error("Repeated call to flush()");
	}
	this.flushed = true;
	let mut encoder = ::xz2::write::XzEncoder::new(Vec::new(), this.preset);
	if let Err(error) = encoder.write_all(&this.input) {
		return raise_lzma(&format!("LZMA compression failed: {error}"));
	}
	match encoder.finish() {
		Ok(bytes) => py_bytes(&bytes),
		Err(error) => raise_lzma(&format!("LZMA compression failed: {error}")),
	}
}

fn lzma_decode_xz(input: &[u8]) -> Result<(Vec<u8>, usize), String> {
	if input.len() >= 6 && &input[..6] != b"\xfd7zXZ\0" {
		return Err("Input format not supported by decoder".to_owned());
	}
	let mut cursor = Cursor::new(input);
	let mut decoder = ::xz2::read::XzDecoder::new(&mut cursor);
	let mut out = Vec::new();
	decoder
		.read_to_end(&mut out)
		.map_err(|error| error.to_string())?;
	Ok((out, cursor.position() as usize))
}

unsafe extern "C" fn lzma_decompress_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let raw = match args_or_type_error(argv, argc, "decompress") {
		Ok(raw) => raw,
		Err(error) => return error,
	};
	if raw.len() < 2 || raw.len() > 3 {
		return type_error(&format!(
			"decompress() expected 1 or 2 arguments, got {}",
			raw.len().saturating_sub(1)
		));
	}
	let Some(this) = (unsafe { lzma_decompressor_receiver(raw[0]) }) else {
		return type_error("decompress() receiver must be LZMADecompressor");
	};
	let data = match bytes_arg(raw[1], "data") {
		Ok(data) => data,
		Err(error) => return error,
	};
	let max_length = match raw.get(2).copied() {
		Some(object) => match int_arg(object, "max_length") {
			Ok(value) => value,
			Err(error) => return error,
		},
		None => -1,
	};
	if max_length < -1 {
		return value_error("max_length must be non-negative");
	}
	if this.eof {
		if !data.is_empty() {
			this.unused_data.extend_from_slice(&data);
		}
		return py_bytes(&[]);
	}
	this.input.extend_from_slice(&data);
	match lzma_decode_xz(&this.input) {
		Ok((out, consumed)) => {
			this.output = out;
			this.eof = true;
			this.unused_data = this.input.get(consumed..).unwrap_or(&[]).to_vec();
			let chunk = take_decompressed(&this.output, &mut this.offset, max_length);
			this.needs_input = this.offset >= this.output.len();
			py_bytes(&chunk)
		},
		Err(_) if this.input.len() < 6 => {
			this.needs_input = true;
			py_bytes(&[])
		},
		Err(message) => raise_lzma(&message),
	}
}

unsafe extern "C" fn lzma_compressor_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { type_::unicode_text(name) }) else {
		return type_error("attribute name must be str");
	};
	match name_text {
		"compress" => bound_method(object, "compress", lzma_compress_method),
		"flush" => bound_method(object, "flush", lzma_flush_method),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

unsafe extern "C" fn lzma_decompressor_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { type_::unicode_text(name) }) else {
		return type_error("attribute name must be str");
	};
	let Some(this) = (unsafe { lzma_decompressor_receiver(object) }) else {
		return type_error("attribute receiver must be LZMADecompressor");
	};
	match name_text {
		"eof" => py_bool(this.eof),
		"needs_input" => py_bool(this.needs_input),
		"unused_data" => py_bytes(&this.unused_data),
		"check" => py_int(this.check),
		"decompress" => bound_method(object, "decompress", lzma_decompress_method),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

unsafe extern "C" fn lzma_is_check_supported(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "is_check_supported") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error(&format!("is_check_supported() expected 1 argument, got {}", args.len()));
	}
	match int_arg(args[0], "check") {
		Ok(0 | 1 | 4) => py_bool(true),
		Ok(10) => py_bool(false),
		Ok(_) => py_bool(false),
		Err(error) => error,
	}
}

unsafe extern "C" fn lzma_filter_props(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	not_implemented(
		"LZMA filter property encoding/decoding requires liblzma filter-chain APIs not yet exposed \
		 by pon",
	)
}

pub(super) fn make_lzma_module() -> Result<*mut PyObject, String> {
	let error_class = *LZMA_ERROR_CLASS;
	if error_class == 0 {
		return Err("failed to create _lzma.LZMAError".to_owned());
	}
	let mut attrs = vec![module_name_attr("_lzma")?];
	attrs
		.push((intern("LZMACompressor"), (*LZMA_COMPRESSOR_TYPE as *mut PyType).cast::<PyObject>()));
	attrs.push((
		intern("LZMADecompressor"),
		(*LZMA_DECOMPRESSOR_TYPE as *mut PyType).cast::<PyObject>(),
	));
	attrs.push((intern("LZMAError"), error_class as *mut PyObject));
	for (name, value) in [
		("FORMAT_AUTO", 0),
		("FORMAT_XZ", 1),
		("FORMAT_ALONE", 2),
		("FORMAT_RAW", 3),
		("CHECK_NONE", 0),
		("CHECK_CRC32", 1),
		("CHECK_CRC64", 4),
		("CHECK_SHA256", 10),
		("CHECK_ID_MAX", 15),
		("CHECK_UNKNOWN", 16),
		("FILTER_LZMA1", 4_611_686_018_427_387_905_i64),
		("FILTER_LZMA2", 33),
		("FILTER_DELTA", 3),
		("FILTER_X86", 4),
		("FILTER_POWERPC", 5),
		("FILTER_IA64", 6),
		("FILTER_ARM", 7),
		("FILTER_ARMTHUMB", 8),
		("FILTER_SPARC", 9),
		("MF_HC3", 3),
		("MF_HC4", 4),
		("MF_BT2", 18),
		("MF_BT3", 19),
		("MF_BT4", 20),
		("MODE_FAST", 1),
		("MODE_NORMAL", 2),
		("PRESET_DEFAULT", 6),
		("PRESET_EXTREME", 2_147_483_648_i64),
	] {
		attrs.push(int_attr(name, value)?);
	}
	attrs.push(function_attr("is_check_supported", "is_check_supported", lzma_is_check_supported)?);
	attrs.push(function_attr(
		"_encode_filter_properties",
		"_encode_filter_properties",
		lzma_filter_props,
	)?);
	attrs.push(function_attr(
		"_decode_filter_properties",
		"_decode_filter_properties",
		lzma_filter_props,
	)?);
	install_module("_lzma", attrs)
}

// ---------------------------------------------------------------------------
// `_zstd`

#[repr(C)]
struct PyZstdCompressor {
	ob_base:   PyObjectHeader,
	input:     Vec<u8>,
	level:     i32,
	last_mode: i64,
}
#[repr(C)]
struct PyZstdDecompressor {
	ob_base:     PyObjectHeader,
	input:       Vec<u8>,
	output:      Vec<u8>,
	offset:      usize,
	eof:         bool,
	needs_input: bool,
	unused_data: Vec<u8>,
}
#[repr(C)]
struct PyZstdDict {
	ob_base: PyObjectHeader,
	content: Vec<u8>,
}

static ZSTD_ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
	exception_class("_zstd", "ZstdError", "Exception").map_or(0, |class| class as usize)
});

static ZSTD_COMPRESSOR_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"_zstd.ZstdCompressor",
		core::mem::size_of::<PyZstdCompressor>(),
	);
	ty.tp_base = abi::runtime_global(intern("object"))
		.map_or(ptr::null_mut(), |object| object.cast::<PyType>());
	ty.tp_new = Some(zstd_compressor_new);
	ty.tp_getattro = Some(zstd_compressor_getattro);
	let namespace = type_::new_namespace();
	if !namespace.is_null() {
		unsafe {
			(*namespace).set(intern("CONTINUE"), py_int(0));
			(*namespace).set(intern("FLUSH_BLOCK"), py_int(1));
			(*namespace).set(intern("FLUSH_FRAME"), py_int(2));
		}
		ty.tp_dict = namespace.cast::<PyObject>();
	}
	Box::into_raw(Box::new(ty)) as usize
});
static ZSTD_DECOMPRESSOR_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"_zstd.ZstdDecompressor",
		core::mem::size_of::<PyZstdDecompressor>(),
	);
	ty.tp_base = abi::runtime_global(intern("object"))
		.map_or(ptr::null_mut(), |object| object.cast::<PyType>());
	ty.tp_new = Some(zstd_decompressor_new);
	ty.tp_getattro = Some(zstd_decompressor_getattro);
	Box::into_raw(Box::new(ty)) as usize
});
static ZSTD_DICT_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"_zstd.ZstdDict",
		core::mem::size_of::<PyZstdDict>(),
	);
	ty.tp_base = abi::runtime_global(intern("object"))
		.map_or(ptr::null_mut(), |object| object.cast::<PyType>());
	ty.tp_new = Some(zstd_dict_new);
	ty.tp_getattro = Some(zstd_dict_getattro);
	Box::into_raw(Box::new(ty)) as usize
});

fn raise_zstd(text: &str) -> *mut PyObject {
	raise_class(&ZSTD_ERROR_CLASS, ExceptionKind::ValueError, text)
}

unsafe extern "C" fn zstd_compressor_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	let positional = match unsafe { type_::positional_args_from_object(args) } {
		Ok(v) => v,
		Err(m) => return type_error(&m),
	};
	let mut level = 3_i64;
	if let Some(first) = positional.first().copied() {
		if !is_none(first) {
			match int_arg(first, "level") {
				Ok(v) => level = v,
				Err(e) => return e,
			}
		}
	}
	if positional.len() > 4 {
		return type_error(&format!(
			"ZstdCompressor() expected at most 4 arguments, got {}",
			positional.len()
		));
	}
	if !kwargs.is_null() {
		let entries = match unsafe { crate::types::dict::dict_entries_snapshot(kwargs) } {
			Ok(e) => e,
			Err(m) => return type_error(&m),
		};
		for entry in entries {
			let Some(key) = (unsafe { type_::unicode_text(crate::tag::untag_arg(entry.key)) }) else {
				return type_error("ZstdCompressor() keywords must be strings");
			};
			match key {
				"level" => {
					if !is_none(entry.value) {
						match int_arg(entry.value, "level") {
							Ok(v) => level = v,
							Err(e) => return e,
						}
					}
				},
				"options" | "zstd_dict" => {},
				other => {
					return type_error(&format!(
						"ZstdCompressor() got an unexpected keyword argument '{other}'"
					));
				},
			}
		}
	}
	Box::into_raw(Box::new(PyZstdCompressor {
		ob_base:   PyObjectHeader::new(*ZSTD_COMPRESSOR_TYPE as *mut PyType),
		input:     Vec::new(),
		level:     level as i32,
		last_mode: -1,
	}))
	.cast::<PyObject>()
}

unsafe extern "C" fn zstd_decompressor_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	let positional = match unsafe { type_::positional_args_from_object(args) } {
		Ok(v) => v,
		Err(m) => return type_error(&m),
	};
	if positional.len() > 2 {
		return type_error(&format!(
			"ZstdDecompressor() expected at most 2 arguments, got {}",
			positional.len()
		));
	}
	if !kwargs.is_null() {
		let entries = match unsafe { crate::types::dict::dict_entries_snapshot(kwargs) } {
			Ok(e) => e,
			Err(m) => return type_error(&m),
		};
		for entry in entries {
			let Some(key) = (unsafe { type_::unicode_text(crate::tag::untag_arg(entry.key)) }) else {
				return type_error("ZstdDecompressor() keywords must be strings");
			};
			match key {
				"options" | "zstd_dict" => {},
				other => {
					return type_error(&format!(
						"ZstdDecompressor() got an unexpected keyword argument '{other}'"
					));
				},
			}
		}
	}
	Box::into_raw(Box::new(PyZstdDecompressor {
		ob_base:     PyObjectHeader::new(*ZSTD_DECOMPRESSOR_TYPE as *mut PyType),
		input:       Vec::new(),
		output:      Vec::new(),
		offset:      0,
		eof:         false,
		needs_input: true,
		unused_data: Vec::new(),
	}))
	.cast::<PyObject>()
}

unsafe extern "C" fn zstd_dict_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	let positional = match unsafe { type_::positional_args_from_object(args) } {
		Ok(v) => v,
		Err(m) => return type_error(&m),
	};
	if positional.len() != 1 || !kwargs.is_null() {
		return type_error("ZstdDict() expects exactly one bytes-like argument");
	}
	let content = match bytes_arg(positional[0], "dict_content") {
		Ok(v) => v,
		Err(e) => return e,
	};
	Box::into_raw(Box::new(PyZstdDict {
		ob_base: PyObjectHeader::new(*ZSTD_DICT_TYPE as *mut PyType),
		content,
	}))
	.cast::<PyObject>()
}

unsafe fn zstd_compressor_receiver<'a>(object: *mut PyObject) -> Option<&'a mut PyZstdCompressor> {
	let object = crate::tag::untag_arg(object);
	(!object.is_null()
		&& unsafe { (*object).ob_type == (*ZSTD_COMPRESSOR_TYPE as *mut PyType).cast_const() })
	.then(|| unsafe { &mut *object.cast::<PyZstdCompressor>() })
}
unsafe fn zstd_decompressor_receiver<'a>(
	object: *mut PyObject,
) -> Option<&'a mut PyZstdDecompressor> {
	let object = crate::tag::untag_arg(object);
	(!object.is_null()
		&& unsafe { (*object).ob_type == (*ZSTD_DECOMPRESSOR_TYPE as *mut PyType).cast_const() })
	.then(|| unsafe { &mut *object.cast::<PyZstdDecompressor>() })
}
unsafe fn zstd_dict_receiver<'a>(object: *mut PyObject) -> Option<&'a mut PyZstdDict> {
	let object = crate::tag::untag_arg(object);
	(!object.is_null()
		&& unsafe { (*object).ob_type == (*ZSTD_DICT_TYPE as *mut PyType).cast_const() })
	.then(|| unsafe { &mut *object.cast::<PyZstdDict>() })
}

unsafe extern "C" fn zstd_compress_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let raw = match args_or_type_error(argv, argc, "compress") {
		Ok(r) => r,
		Err(e) => return e,
	};
	if raw.len() < 2 || raw.len() > 3 {
		return type_error(&format!(
			"compress() expected 1 or 2 arguments, got {}",
			raw.len().saturating_sub(1)
		));
	}
	let Some(this) = (unsafe { zstd_compressor_receiver(raw[0]) }) else {
		return type_error("compress() receiver must be ZstdCompressor");
	};
	let data = match bytes_arg(raw[1], "data") {
		Ok(v) => v,
		Err(e) => return e,
	};
	let mode = match raw.get(2).copied() {
		Some(o) if !is_none(o) => match int_arg(o, "mode") {
			Ok(v) => v,
			Err(e) => return e,
		},
		_ => 0,
	};
	this.input.extend_from_slice(&data);
	this.last_mode = mode;
	if mode == 2 {
		match ::zstd::stream::encode_all(Cursor::new(&this.input), this.level) {
			Ok(bytes) => {
				this.input.clear();
				py_bytes(&bytes)
			},
			Err(error) => raise_zstd(&format!("zstd compress failed: {error}")),
		}
	} else {
		py_bytes(&[])
	}
}

unsafe extern "C" fn zstd_flush_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let raw = match args_or_type_error(argv, argc, "flush") {
		Ok(r) => r,
		Err(e) => return e,
	};
	if raw.len() > 2 {
		return type_error(&format!(
			"flush() expected at most 1 argument, got {}",
			raw.len().saturating_sub(1)
		));
	}
	let Some(this) = (unsafe { zstd_compressor_receiver(raw[0]) }) else {
		return type_error("flush() receiver must be ZstdCompressor");
	};
	let mode = match raw.get(1).copied() {
		Some(o) if !is_none(o) => match int_arg(o, "mode") {
			Ok(v) => v,
			Err(e) => return e,
		},
		_ => 1,
	};
	this.last_mode = mode;
	if mode == 2 || mode == 1 {
		match ::zstd::stream::encode_all(Cursor::new(&this.input), this.level) {
			Ok(bytes) => {
				this.input.clear();
				py_bytes(&bytes)
			},
			Err(error) => raise_zstd(&format!("zstd flush failed: {error}")),
		}
	} else {
		value_error("Invalid mode argument")
	}
}

unsafe extern "C" fn zstd_set_pledged_size(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let raw = match args_or_type_error(argv, argc, "set_pledged_input_size") {
		Ok(r) => r,
		Err(e) => return e,
	};
	if raw.is_empty() {
		return type_error("set_pledged_input_size() missing receiver");
	}
	none()
}

unsafe extern "C" fn zstd_decompress_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let raw = match args_or_type_error(argv, argc, "decompress") {
		Ok(r) => r,
		Err(e) => return e,
	};
	if raw.len() < 2 || raw.len() > 3 {
		return type_error(&format!(
			"decompress() expected 1 or 2 arguments, got {}",
			raw.len().saturating_sub(1)
		));
	}
	let Some(this) = (unsafe { zstd_decompressor_receiver(raw[0]) }) else {
		return type_error("decompress() receiver must be ZstdDecompressor");
	};
	let data = match bytes_arg(raw[1], "data") {
		Ok(v) => v,
		Err(e) => return e,
	};
	let max_length = match raw.get(2).copied() {
		Some(o) if !is_none(o) => match int_arg(o, "max_length") {
			Ok(v) => v,
			Err(e) => return e,
		},
		_ => -1,
	};
	if max_length < -1 {
		return value_error("max_length must be non-negative");
	}
	if this.eof {
		if !data.is_empty() {
			this.unused_data.extend_from_slice(&data);
		}
		return py_bytes(&[]);
	}
	this.input.extend_from_slice(&data);
	match ::zstd::stream::decode_all(Cursor::new(&this.input)) {
		Ok(out) => {
			this.output = out;
			this.eof = true;
			this.unused_data.clear();
			let chunk = take_decompressed(&this.output, &mut this.offset, max_length);
			this.needs_input = this.offset >= this.output.len();
			py_bytes(&chunk)
		},
		Err(_error) if this.input.len() < 8 => {
			this.needs_input = true;
			py_bytes(&[])
		},
		Err(error) => raise_zstd(&format!("zstd decompress failed: {error}")),
	}
}

unsafe extern "C" fn zstd_compressor_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { type_::unicode_text(name) }) else {
		return type_error("attribute name must be str");
	};
	let Some(this) = (unsafe { zstd_compressor_receiver(object) }) else {
		return type_error("attribute receiver must be ZstdCompressor");
	};
	match name_text {
		"last_mode" => py_int(this.last_mode),
		"compress" => bound_method(object, "compress", zstd_compress_method),
		"flush" => bound_method(object, "flush", zstd_flush_method),
		"set_pledged_input_size" => {
			bound_method(object, "set_pledged_input_size", zstd_set_pledged_size)
		},
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}
unsafe extern "C" fn zstd_decompressor_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { type_::unicode_text(name) }) else {
		return type_error("attribute name must be str");
	};
	let Some(this) = (unsafe { zstd_decompressor_receiver(object) }) else {
		return type_error("attribute receiver must be ZstdDecompressor");
	};
	match name_text {
		"eof" => py_bool(this.eof),
		"needs_input" => py_bool(this.needs_input),
		"unused_data" => py_bytes(&this.unused_data),
		"decompress" => bound_method(object, "decompress", zstd_decompress_method),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}
unsafe extern "C" fn zstd_dict_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { type_::unicode_text(name) }) else {
		return type_error("attribute name must be str");
	};
	let Some(this) = (unsafe { zstd_dict_receiver(object) }) else {
		return type_error("attribute receiver must be ZstdDict");
	};
	match name_text {
		"dict_content" => py_bytes(&this.content),
		"is_raw" => py_bool(false),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

unsafe extern "C" fn zstd_get_frame_size(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "get_frame_size") {
		Ok(a) => a,
		Err(e) => return e,
	};
	if args.len() != 1 {
		return type_error(&format!("get_frame_size() expected 1 argument, got {}", args.len()));
	}
	let data = match bytes_arg(args[0], "frame_buffer") {
		Ok(v) => v,
		Err(e) => return e,
	};
	match ::zstd::stream::decode_all(Cursor::new(&data)) {
		Ok(out) => py_int(out.len() as i64),
		Err(error) => raise_zstd(&format!("zstd frame decode failed: {error}")),
	}
}
unsafe extern "C" fn zstd_get_frame_info(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let size = unsafe { zstd_get_frame_size(argv, argc) };
	if size.is_null() {
		return size;
	}
	alloc_tuple(vec![size, py_int(0)])
}
unsafe extern "C" fn zstd_get_param_bounds(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "get_param_bounds") {
		Ok(a) => a,
		Err(e) => return e,
	};
	if args.is_empty() {
		return type_error("get_param_bounds() missing parameter");
	}
	let param = match int_arg(args[0], "parameter") {
		Ok(v) => v,
		Err(e) => return e,
	};
	let (lo, hi) = if param == 100 {
		(-131_072, 22)
	} else {
		(0, 2_147_483_647)
	};
	alloc_tuple(vec![py_int(lo), py_int(hi)])
}
unsafe extern "C" fn zstd_train_dict(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	not_implemented(
		"zstd dictionary training requires zstd experimental dictionary APIs not yet exposed by pon",
	)
}
unsafe extern "C" fn zstd_finalize_dict(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	not_implemented(
		"zstd dictionary finalization requires zstd experimental dictionary APIs not yet exposed by \
		 pon",
	)
}
unsafe extern "C" fn zstd_set_parameter_types(
	_argv: *mut *mut PyObject,
	_argc: usize,
) -> *mut PyObject {
	none()
}

pub(super) fn make_zstd_module() -> Result<*mut PyObject, String> {
	let error_class = *ZSTD_ERROR_CLASS;
	if error_class == 0 {
		return Err("failed to create _zstd.ZstdError".to_owned());
	}
	let mut attrs = vec![module_name_attr("_zstd")?];
	attrs
		.push((intern("ZstdCompressor"), (*ZSTD_COMPRESSOR_TYPE as *mut PyType).cast::<PyObject>()));
	attrs.push((
		intern("ZstdDecompressor"),
		(*ZSTD_DECOMPRESSOR_TYPE as *mut PyType).cast::<PyObject>(),
	));
	attrs.push((intern("ZstdDict"), (*ZSTD_DICT_TYPE as *mut PyType).cast::<PyObject>()));
	attrs.push((intern("ZstdError"), error_class as *mut PyObject));
	attrs.push(int_attr("zstd_version_number", ::zstd::zstd_safe::version_number() as i64)?);
	attrs.push(int_attr("ZSTD_CLEVEL_DEFAULT", ::zstd::DEFAULT_COMPRESSION_LEVEL as i64)?);
	attrs.push(int_attr("ZSTD_DStreamOutSize", 131_072)?);
	for (name, value) in [
		("ZSTD_c_compressionLevel", 100),
		("ZSTD_c_windowLog", 101),
		("ZSTD_c_hashLog", 102),
		("ZSTD_c_chainLog", 103),
		("ZSTD_c_searchLog", 104),
		("ZSTD_c_minMatch", 105),
		("ZSTD_c_targetLength", 106),
		("ZSTD_c_strategy", 107),
		("ZSTD_c_enableLongDistanceMatching", 160),
		("ZSTD_c_ldmHashLog", 161),
		("ZSTD_c_ldmMinMatch", 162),
		("ZSTD_c_ldmBucketSizeLog", 163),
		("ZSTD_c_ldmHashRateLog", 164),
		("ZSTD_c_contentSizeFlag", 200),
		("ZSTD_c_checksumFlag", 201),
		("ZSTD_c_dictIDFlag", 202),
		("ZSTD_c_nbWorkers", 400),
		("ZSTD_c_jobSize", 401),
		("ZSTD_c_overlapLog", 402),
		("ZSTD_d_windowLogMax", 100),
		("ZSTD_fast", 1),
		("ZSTD_dfast", 2),
		("ZSTD_greedy", 3),
		("ZSTD_lazy", 4),
		("ZSTD_lazy2", 5),
		("ZSTD_btlazy2", 6),
		("ZSTD_btopt", 7),
		("ZSTD_btultra", 8),
		("ZSTD_btultra2", 9),
	] {
		attrs.push(int_attr(name, value)?);
	}
	attrs.push(str_attr("zstd_version", ::zstd::zstd_safe::version_string())?);
	attrs.push(function_attr("get_frame_size", "get_frame_size", zstd_get_frame_size)?);
	attrs.push(function_attr("get_frame_info", "get_frame_info", zstd_get_frame_info)?);
	attrs.push(function_attr("get_param_bounds", "get_param_bounds", zstd_get_param_bounds)?);
	attrs.push(function_attr("train_dict", "train_dict", zstd_train_dict)?);
	attrs.push(function_attr("finalize_dict", "finalize_dict", zstd_finalize_dict)?);
	attrs.push(function_attr(
		"set_parameter_types",
		"set_parameter_types",
		zstd_set_parameter_types,
	)?);
	install_module("_zstd", attrs)
}

// ---------------------------------------------------------------------------
// `_uuid`

unsafe extern "C" fn uuid_generate_time_safe(
	_argv: *mut *mut PyObject,
	_argc: usize,
) -> *mut PyObject {
	let uuid = uuid_v1_bytes();
	alloc_tuple(vec![py_bytes(&uuid), py_int(-1)])
}

fn fill_random(bytes: &mut [u8]) {
	if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
		if file.read_exact(bytes).is_ok() {
			return;
		}
	}
	let nanos = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_or(0, |duration| duration.as_nanos() as u64);
	let mut x = nanos ^ (bytes.as_ptr() as usize as u64);
	for byte in bytes {
		x ^= x << 13;
		x ^= x >> 7;
		x ^= x << 17;
		*byte = x as u8;
	}
}

fn uuid_v1_bytes() -> [u8; 16] {
	let unix_100ns = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_or(0, |duration| duration.as_nanos() / 100);
	let timestamp = unix_100ns + 0x01b2_1dd2_1381_4000_u128;
	let mut random = [0_u8; 8];
	fill_random(&mut random);
	let clock_seq = (u16::from_be_bytes([random[0], random[1]]) & 0x3fff) | 0x8000;
	let mut node = [0_u8; 6];
	node.copy_from_slice(&random[2..8]);
	node[0] |= 0x01;
	let time_low = (timestamp & 0xffff_ffff) as u32;
	let time_mid = ((timestamp >> 32) & 0xffff) as u16;
	let time_hi = (((timestamp >> 48) & 0x0fff) as u16) | 0x1000;
	let mut out = [0_u8; 16];
	out[0..4].copy_from_slice(&time_low.to_be_bytes());
	out[4..6].copy_from_slice(&time_mid.to_be_bytes());
	out[6..8].copy_from_slice(&time_hi.to_be_bytes());
	out[8..10].copy_from_slice(&clock_seq.to_be_bytes());
	out[10..16].copy_from_slice(&node);
	out
}

pub(super) fn make_uuid_module() -> Result<*mut PyObject, String> {
	install_module("_uuid", vec![
		module_name_attr("_uuid")?,
		bool_attr("has_stable_extractable_node", false)?,
		bool_attr("has_uuid_generate_time_safe", true)?,
		function_attr("generate_time_safe", "generate_time_safe", uuid_generate_time_safe)?,
	])
}
