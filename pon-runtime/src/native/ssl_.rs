//! Native `_ssl` module backed by OpenSSL where Pon exposes the surface.
//!
//! Context construction, defaults, certificate-store loading, MemoryBIO
//! storage, random bytes, OpenSSL version/ASN.1 helpers, and the CPython
//! exception hierarchy are real.  Network/BIO TLS wrapping remains an explicit
//! `NotImplementedError` boundary until the runtime grows a safe owner for
//! OpenSSL streams over Python socket and BIO objects.

use core::{
	ffi::{c_char, c_int},
	ptr,
};
use std::{
	collections::HashMap,
	ffi::CStr,
	path::Path,
	sync::{LazyLock, Mutex},
};

use num_traits::ToPrimitive as _;
use openssl::{
	asn1::Asn1Object,
	nid::Nid,
	ssl::{SslContext, SslContextBuilder, SslFiletype, SslMethod},
};

use super::{
	builtins_mod::{VARIADIC_ARITY, alloc_tuple},
	install_module,
};
use crate::{
	abi,
	intern::intern,
	object::{PyObject, PyObjectHeader, PyType},
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

fn runtime_error(message: &str) -> *mut PyObject {
	raise(ExceptionKind::RuntimeError, message)
}

fn py_str(text: &str) -> *mut PyObject {
	unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn py_bytes(bytes: &[u8]) -> *mut PyObject {
	unsafe { abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) }
}

fn py_int(value: i64) -> *mut PyObject {
	unsafe { abi::pon_const_int(value) }
}

fn py_bool(value: bool) -> *mut PyObject {
	unsafe { abi::pon_const_bool(c_int::from(value)) }
}

fn none() -> *mut PyObject {
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

fn function_attr(
	attr: &str,
	function_name: &str,
	entry: BuiltinFn,
) -> Result<(u32, *mut PyObject), String> {
	let function =
		unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(function_name)) };
	(!function.is_null())
		.then_some((intern(attr), function))
		.ok_or_else(|| format!("failed to allocate native function {function_name}"))
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

fn object_type() -> *mut PyType {
	abi::runtime_global(intern("object")).map_or(ptr::null_mut(), |object| object.cast::<PyType>())
}

fn type_name(object: *mut PyObject) -> &'static str {
	unsafe { crate::types::dict::type_name(object) }.unwrap_or("object")
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
		.ok_or_else(|| value_error(&format!("{name} is too large")))
}

fn bool_arg(object: *mut PyObject) -> Option<bool> {
	let object = crate::tag::untag_arg(object);
	if let Some(value) = unsafe { crate::types::bool_::to_bool(object) } {
		return Some(value);
	}
	unsafe { crate::types::int::to_bigint_including_bool(object) }
		.map(|value| value != num_bigint::BigInt::from(0))
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

fn bytes_or_text_arg(object: *mut PyObject, name: &str) -> Result<Vec<u8>, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if let Some(bytes) = bytes_like(object) {
		return Ok(bytes.to_vec());
	}
	if let Some(text) = unsafe { type_::unicode_text(object) } {
		return Ok(text.as_bytes().to_vec());
	}
	Err(type_error(&format!(
		"{name} must be a bytes-like object or str, not '{}'",
		type_name(object)
	)))
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

fn attr_name<'a>(name: *mut PyObject) -> Result<&'a str, *mut PyObject> {
	let name = crate::tag::untag_arg(name);
	unsafe { type_::unicode_text(name) }.ok_or_else(|| type_error("attribute name must be str"))
}

fn exception_class_from_base(
	module: &str,
	name: &str,
	base: *mut PyObject,
) -> Result<*mut PyObject, String> {
	if base.is_null() {
		return Err(format!("base class for {module}.{name} is NULL"));
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
	let class = unsafe { type_::build_class_from_namespace(name, &[base], namespace, &[]) };
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

fn exception_class(module: &str, name: &str, base: &str) -> Result<*mut PyObject, String> {
	let base_class = unsafe { abi::pon_load_global(intern(base), ptr::null_mut()) };
	if base_class.is_null() {
		pon_err_clear();
		return Err(format!("builtin class '{base}' is not registered"));
	}
	exception_class_from_base(module, name, base_class)
}

fn py_dict_from_pairs(pairs: &[(&str, *mut PyObject)]) -> *mut PyObject {
	let mut flat = Vec::with_capacity(pairs.len() * 2);
	for (key, value) in pairs {
		let key = py_str(key);
		if key.is_null() || value.is_null() {
			return ptr::null_mut();
		}
		flat.push(key);
		flat.push(*value);
	}
	unsafe { abi::map::pon_build_map(flat.as_mut_ptr(), pairs.len()) }
}

unsafe extern "C" fn ssl_context_enter_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "__enter__") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error("__enter__() takes no arguments");
	}
	args[0]
}

// ---------------------------------------------------------------------------
// `_ssl` / `ssl`

const PY_SSL_CERT_NONE: i64 = 0;
const PY_SSL_CERT_OPTIONAL: i64 = 1;
const PY_SSL_CERT_REQUIRED: i64 = 2;
const PY_SSL_PROTOCOL_TLS: i64 = 2;
const PY_SSL_PROTOCOL_TLSV1: i64 = 3;
const PY_SSL_PROTOCOL_TLSV1_1: i64 = 4;
const PY_SSL_PROTOCOL_TLSV1_2: i64 = 5;
const PY_SSL_PROTOCOL_TLS_CLIENT: i64 = 16;
const PY_SSL_PROTOCOL_TLS_SERVER: i64 = 17;
const PY_SSL_TLS_MINIMUM_SUPPORTED: i64 = -2;
const PY_SSL_TLS_MAXIMUM_SUPPORTED: i64 = -1;
const PY_SSL_SSLV3: i64 = 768;
const PY_SSL_TLSV1: i64 = 769;
const PY_SSL_TLSV1_1: i64 = 770;
const PY_SSL_TLSV1_2: i64 = 771;
const PY_SSL_TLSV1_3: i64 = 772;
const PY_SSL_VERIFY_X509_TRUSTED_FIRST: i64 = 32_768;
const PY_SSL_HOSTFLAG_NO_PARTIAL_WILDCARDS: i64 = 4;
const PY_SSL_HOSTFLAG_NEVER_CHECK_SUBJECT: i64 = 32;
const PY_SSL_DEFAULT_OPTIONS: u64 = 0x8252_0050;

unsafe extern "C" {
	fn X509_get_default_cert_file_env() -> *const c_char;
	fn X509_get_default_cert_file() -> *const c_char;
	fn X509_get_default_cert_dir_env() -> *const c_char;
	fn X509_get_default_cert_dir() -> *const c_char;
}

struct SslContextState {
	protocol:        i64,
	verify_mode:     i64,
	check_hostname:  bool,
	options:         u64,
	minimum_version: i64,
	maximum_version: i64,
	verify_flags:    i64,
	host_flags:      i64,
	msg_callback:    usize,
	context:         Option<SslContext>,
}

#[repr(C)]
struct PySslContext {
	ob_base: PyObjectHeader,
	state:   SslContextState,
}

#[repr(C)]
struct PyMemoryBio {
	ob_base: PyObjectHeader,
	buffer:  Vec<u8>,
}

#[repr(C)]
struct PySslSession {
	ob_base: PyObjectHeader,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SslContextMember {
	Protocol,
	VerifyMode,
	CheckHostname,
	Options,
	MinimumVersion,
	MaximumVersion,
	VerifyFlags,
	HostFlags,
	MsgCallback,
}

#[repr(C)]
struct PySslContextDescriptor {
	ob_base: PyObjectHeader,
	member:  SslContextMember,
}

static SSL_CONTEXT_STATES: LazyLock<Mutex<HashMap<usize, SslContextState>>> =
	LazyLock::new(|| Mutex::new(HashMap::new()));

static SSL_ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
	exception_class("ssl", "SSLError", "OSError").map_or(0, |class| class as usize)
});
static SSL_ZERO_RETURN_ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
	let base = *SSL_ERROR_CLASS as *mut PyObject;
	exception_class_from_base("ssl", "SSLZeroReturnError", base).map_or(0, |class| class as usize)
});
static SSL_WANT_READ_ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
	let base = *SSL_ERROR_CLASS as *mut PyObject;
	exception_class_from_base("ssl", "SSLWantReadError", base).map_or(0, |class| class as usize)
});
static SSL_WANT_WRITE_ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
	let base = *SSL_ERROR_CLASS as *mut PyObject;
	exception_class_from_base("ssl", "SSLWantWriteError", base).map_or(0, |class| class as usize)
});
static SSL_SYSCALL_ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
	let base = *SSL_ERROR_CLASS as *mut PyObject;
	exception_class_from_base("ssl", "SSLSyscallError", base).map_or(0, |class| class as usize)
});
static SSL_EOF_ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
	let base = *SSL_ERROR_CLASS as *mut PyObject;
	exception_class_from_base("ssl", "SSLEOFError", base).map_or(0, |class| class as usize)
});
static SSL_CERT_VERIFICATION_ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
	let base = *SSL_ERROR_CLASS as *mut PyObject;
	exception_class_from_base("ssl", "SSLCertVerificationError", base)
		.map_or(0, |class| class as usize)
});

static SSL_CONTEXT_DESCRIPTOR_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"_ssl._SSLContextDescriptor",
		core::mem::size_of::<PySslContextDescriptor>(),
	);
	ty.tp_base = object_type();
	ty.tp_getattro = Some(ssl_context_descriptor_getattro);
	ty.tp_descr_get = Some(ssl_context_descriptor_get);
	ty.tp_descr_set = Some(ssl_context_descriptor_set);
	Box::into_raw(Box::new(ty)) as usize
});

static SSL_CONTEXT_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"_ssl._SSLContext",
		core::mem::size_of::<PySslContext>(),
	);
	ty.tp_base = object_type();
	ty.tp_new = Some(ssl_context_new);
	ty.tp_getattro = Some(ssl_context_getattro);
	ty.tp_setattro = Some(ssl_context_setattro);
	install_ssl_context_type_dict(&mut ty);
	Box::into_raw(Box::new(ty)) as usize
});

static MEMORY_BIO_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"ssl.MemoryBIO",
		core::mem::size_of::<PyMemoryBio>(),
	);
	ty.tp_base = object_type();
	ty.tp_new = Some(memory_bio_new);
	ty.tp_getattro = Some(memory_bio_getattro);
	Box::into_raw(Box::new(ty)) as usize
});

static SSL_SESSION_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"ssl.SSLSession",
		core::mem::size_of::<PySslSession>(),
	);
	ty.tp_base = object_type();
	Box::into_raw(Box::new(ty)) as usize
});

fn ssl_context_descriptor_type() -> *mut PyType {
	*SSL_CONTEXT_DESCRIPTOR_TYPE as *mut PyType
}

fn ssl_context_type() -> *mut PyType {
	*SSL_CONTEXT_TYPE as *mut PyType
}

fn memory_bio_type() -> *mut PyType {
	*MEMORY_BIO_TYPE as *mut PyType
}

fn ssl_session_type() -> *mut PyType {
	*SSL_SESSION_TYPE as *mut PyType
}

fn build_openssl_context(_protocol: i64) -> Option<SslContext> {
	SslContextBuilder::new(SslMethod::tls())
		.ok()
		.map(SslContextBuilder::build)
}

fn default_ssl_context_state(protocol: i64) -> SslContextState {
	let verify_mode = if protocol == PY_SSL_PROTOCOL_TLS_CLIENT {
		PY_SSL_CERT_REQUIRED
	} else {
		PY_SSL_CERT_NONE
	};
	let check_hostname = protocol == PY_SSL_PROTOCOL_TLS_CLIENT;
	SslContextState {
		protocol,
		verify_mode,
		check_hostname,
		options: PY_SSL_DEFAULT_OPTIONS,
		minimum_version: PY_SSL_TLSV1_2,
		maximum_version: PY_SSL_TLS_MAXIMUM_SUPPORTED,
		verify_flags: PY_SSL_VERIFY_X509_TRUSTED_FIRST,
		host_flags: PY_SSL_HOSTFLAG_NO_PARTIAL_WILDCARDS,
		msg_callback: 0,
		context: build_openssl_context(protocol),
	}
}

fn alloc_ssl_context(protocol: i64) -> *mut PyObject {
	Box::into_raw(Box::new(PySslContext {
		ob_base: PyObjectHeader::new(ssl_context_type()),
		state:   default_ssl_context_state(protocol),
	}))
	.cast::<PyObject>()
}

fn ssl_context_member_descriptor(member: SslContextMember) -> *mut PyObject {
	Box::into_raw(Box::new(PySslContextDescriptor {
		ob_base: PyObjectHeader::new(ssl_context_descriptor_type()),
		member,
	}))
	.cast::<PyObject>()
}

fn ssl_context_method_descriptor(name: &str, entry: BuiltinFn) -> *mut PyObject {
	let function =
		unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	if !function.is_null() {
		crate::types::function::mark_native_method_descriptor(function);
	}
	function
}

fn install_ssl_context_type_dict(ty: &mut PyType) {
	let namespace = type_::new_namespace();
	if namespace.is_null() {
		return;
	}
	for (name, member) in [
		("protocol", SslContextMember::Protocol),
		("verify_mode", SslContextMember::VerifyMode),
		("check_hostname", SslContextMember::CheckHostname),
		("options", SslContextMember::Options),
		("minimum_version", SslContextMember::MinimumVersion),
		("maximum_version", SslContextMember::MaximumVersion),
		("verify_flags", SslContextMember::VerifyFlags),
		("_host_flags", SslContextMember::HostFlags),
		("_msg_callback", SslContextMember::MsgCallback),
	] {
		unsafe { (&mut *namespace).set(intern(name), ssl_context_member_descriptor(member)) };
	}
	for (name, entry) in [
		("set_ciphers", ssl_context_set_ciphers_entry as BuiltinFn),
		("load_default_certs", ssl_context_load_default_certs_entry as BuiltinFn),
		("load_verify_locations", ssl_context_load_verify_locations_entry as BuiltinFn),
		("load_cert_chain", ssl_context_load_cert_chain_entry as BuiltinFn),
		("set_default_verify_paths", ssl_context_set_default_verify_paths_entry as BuiltinFn),
		("cert_store_stats", ssl_context_cert_store_stats_entry as BuiltinFn),
		("_wrap_socket", ssl_context_wrap_not_implemented_entry as BuiltinFn),
		("_wrap_bio", ssl_context_wrap_not_implemented_entry as BuiltinFn),
		("__enter__", ssl_context_enter_entry as BuiltinFn),
		("__exit__", ssl_context_exit_entry as BuiltinFn),
	] {
		let function = ssl_context_method_descriptor(name, entry);
		if !function.is_null() {
			unsafe { (&mut *namespace).set(intern(name), function) };
		}
	}
	ty.tp_dict = namespace.cast::<PyObject>();
}

unsafe fn with_ssl_context_state<R>(
	object: *mut PyObject,
	body: impl FnOnce(&mut SslContextState) -> R,
) -> Option<R> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() {
		return None;
	}
	let ty = unsafe { (*object).ob_type as *mut PyType };
	if ty == ssl_context_type() {
		return Some(body(unsafe { &mut (*object.cast::<PySslContext>()).state }));
	}
	if unsafe { crate::mro::is_subtype(ty, ssl_context_type()) } {
		let mut states = SSL_CONTEXT_STATES
			.lock()
			.unwrap_or_else(|poison| poison.into_inner());
		return states.get_mut(&(object as usize)).map(body);
	}
	None
}

unsafe extern "C" fn ssl_context_new(
	cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	if !kwargs.is_null() {
		return type_error("SSLContext() takes no keyword arguments in this runtime");
	}
	let positional = match unsafe { type_::positional_args_from_object(args) } {
		Ok(positional) => positional,
		Err(message) => return type_error(&message),
	};
	let protocol = match positional.as_slice() {
		[] => PY_SSL_PROTOCOL_TLS,
		[protocol] => match int_arg(*protocol, "protocol") {
			Ok(protocol) => protocol,
			Err(error) => return error,
		},
		_ => {
			return type_error(&format!(
				"SSLContext() expected at most 1 argument, got {}",
				positional.len()
			));
		},
	};
	if cls.is_null() || cls == ssl_context_type() {
		return alloc_ssl_context(protocol);
	}
	if unsafe { !crate::mro::is_subtype(cls, ssl_context_type()) } {
		return type_error("_SSLContext.__new__(X): X is not a subtype of _SSLContext");
	}
	let instance = unsafe { type_::type_new(cls, ptr::null_mut(), ptr::null_mut()) };
	if instance.is_null() {
		return ptr::null_mut();
	}
	let mut states = SSL_CONTEXT_STATES
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	states.insert(instance as usize, default_ssl_context_state(protocol));
	instance
}

fn ssl_context_get_member(
	context: &mut SslContextState,
	member: SslContextMember,
) -> *mut PyObject {
	match member {
		SslContextMember::Protocol => py_int(context.protocol),
		SslContextMember::VerifyMode => py_int(context.verify_mode),
		SslContextMember::CheckHostname => py_bool(context.check_hostname),
		SslContextMember::Options => py_int(context.options as i64),
		SslContextMember::MinimumVersion => py_int(context.minimum_version),
		SslContextMember::MaximumVersion => py_int(context.maximum_version),
		SslContextMember::VerifyFlags => py_int(context.verify_flags),
		SslContextMember::HostFlags => py_int(context.host_flags),
		SslContextMember::MsgCallback => {
			if context.msg_callback == 0 {
				none()
			} else {
				context.msg_callback as *mut PyObject
			}
		},
	}
}

fn ssl_context_set_member(
	context: &mut SslContextState,
	member: SslContextMember,
	value: *mut PyObject,
) -> c_int {
	if value.is_null() {
		let _ = type_error("SSLContext attributes cannot be deleted");
		return -1;
	}
	match member {
		SslContextMember::Protocol => {
			let _ = type_error("protocol is read-only");
			-1
		},
		SslContextMember::VerifyMode => match int_arg(value, "verify_mode") {
			Ok(mode @ (PY_SSL_CERT_NONE | PY_SSL_CERT_OPTIONAL | PY_SSL_CERT_REQUIRED)) => {
				if context.check_hostname && mode == PY_SSL_CERT_NONE {
					let _ = value_error(
						"Cannot set verify_mode to CERT_NONE when check_hostname is enabled.",
					);
					return -1;
				}
				context.verify_mode = mode;
				0
			},
			Ok(_) => {
				let _ = value_error("invalid verify_mode");
				-1
			},
			Err(_) => -1,
		},
		SslContextMember::CheckHostname => match bool_arg(value) {
			Some(value) => {
				context.check_hostname = value;
				if value && context.verify_mode == PY_SSL_CERT_NONE {
					context.verify_mode = PY_SSL_CERT_REQUIRED;
				}
				0
			},
			None => {
				let _ = type_error("check_hostname must be bool");
				-1
			},
		},
		SslContextMember::Options => match int_arg(value, "options") {
			Ok(options) if options >= 0 => {
				context.options = options as u64;
				0
			},
			Ok(_) => {
				let _ = value_error("options must be non-negative");
				-1
			},
			Err(_) => -1,
		},
		SslContextMember::MinimumVersion => match int_arg(value, "minimum_version") {
			Ok(version) => {
				context.minimum_version = version;
				0
			},
			Err(_) => -1,
		},
		SslContextMember::MaximumVersion => match int_arg(value, "maximum_version") {
			Ok(version) => {
				context.maximum_version = version;
				0
			},
			Err(_) => -1,
		},
		SslContextMember::VerifyFlags => match int_arg(value, "verify_flags") {
			Ok(flags) if flags >= 0 => {
				context.verify_flags = flags;
				0
			},
			Ok(_) => {
				let _ = value_error("verify_flags must be non-negative");
				-1
			},
			Err(_) => -1,
		},
		SslContextMember::HostFlags => match int_arg(value, "_host_flags") {
			Ok(flags) if flags >= 0 => {
				context.host_flags = flags;
				0
			},
			Ok(_) => {
				let _ = value_error("_host_flags must be non-negative");
				-1
			},
			Err(_) => -1,
		},
		SslContextMember::MsgCallback => {
			context.msg_callback = if is_none(value) { 0 } else { value as usize };
			0
		},
	}
}

unsafe extern "C" fn ssl_context_descriptor_get(
	descr: *mut PyObject,
	obj: *mut PyObject,
	_owner: *mut PyObject,
) -> *mut PyObject {
	if descr.is_null()
		|| obj.is_null()
		|| is_none(obj)
		|| unsafe { type_::is_type_object(crate::tag::untag_arg(obj)) }
	{
		return descr;
	}
	let member = unsafe { (*descr.cast::<PySslContextDescriptor>()).member };
	match unsafe { with_ssl_context_state(obj, |context| ssl_context_get_member(context, member)) } {
		Some(value) => value,
		None => type_error("SSLContext descriptor access on non-context"),
	}
}

unsafe extern "C" fn ssl_context_descriptor_set(
	descr: *mut PyObject,
	obj: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	if descr.is_null() {
		let _ = type_error("SSLContext descriptor is NULL");
		return -1;
	}
	let member = unsafe { (*descr.cast::<PySslContextDescriptor>()).member };
	match unsafe {
		with_ssl_context_state(obj, |context| ssl_context_set_member(context, member, value))
	} {
		Some(status) => status,
		None => {
			let _ = type_error("SSLContext descriptor assignment on non-context");
			-1
		},
	}
}

unsafe extern "C" fn ssl_context_descriptor_set_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "__set__") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 3 {
		return type_error("__set__() expects descriptor, object, and value");
	}
	if unsafe { ssl_context_descriptor_set(args[0], args[1], args[2]) } < 0 {
		return ptr::null_mut();
	}
	none()
}

unsafe extern "C" fn ssl_context_descriptor_get_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "__get__") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() < 2 || args.len() > 3 {
		return type_error("__get__() expects descriptor, object, and optional owner");
	}
	let owner = args.get(2).copied().unwrap_or_else(none);
	unsafe { ssl_context_descriptor_get(args[0], args[1], owner) }
}

unsafe extern "C" fn ssl_context_descriptor_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = match attr_name(name) {
		Ok(name) => name,
		Err(error) => return error,
	};
	match name {
		"__set__" => bound_method(object, "__set__", ssl_context_descriptor_set_entry),
		"__get__" => bound_method(object, "__get__", ssl_context_descriptor_get_entry),
		"__name__" => py_str("SSLContext attribute"),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name)) },
	}
}

unsafe extern "C" fn ssl_context_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = match attr_name(name) {
		Ok(name) => name,
		Err(error) => return error,
	};
	match name {
		"protocol" => unsafe {
			with_ssl_context_state(object, |context| {
				ssl_context_get_member(context, SslContextMember::Protocol)
			})
		}
		.unwrap_or_else(|| type_error("SSLContext attribute lookup on non-context")),
		"verify_mode" => unsafe {
			with_ssl_context_state(object, |context| {
				ssl_context_get_member(context, SslContextMember::VerifyMode)
			})
		}
		.unwrap_or_else(|| type_error("SSLContext attribute lookup on non-context")),
		"check_hostname" => unsafe {
			with_ssl_context_state(object, |context| {
				ssl_context_get_member(context, SslContextMember::CheckHostname)
			})
		}
		.unwrap_or_else(|| type_error("SSLContext attribute lookup on non-context")),
		"options" => unsafe {
			with_ssl_context_state(object, |context| {
				ssl_context_get_member(context, SslContextMember::Options)
			})
		}
		.unwrap_or_else(|| type_error("SSLContext attribute lookup on non-context")),
		"minimum_version" => unsafe {
			with_ssl_context_state(object, |context| {
				ssl_context_get_member(context, SslContextMember::MinimumVersion)
			})
		}
		.unwrap_or_else(|| type_error("SSLContext attribute lookup on non-context")),
		"maximum_version" => unsafe {
			with_ssl_context_state(object, |context| {
				ssl_context_get_member(context, SslContextMember::MaximumVersion)
			})
		}
		.unwrap_or_else(|| type_error("SSLContext attribute lookup on non-context")),
		"verify_flags" => unsafe {
			with_ssl_context_state(object, |context| {
				ssl_context_get_member(context, SslContextMember::VerifyFlags)
			})
		}
		.unwrap_or_else(|| type_error("SSLContext attribute lookup on non-context")),
		"_host_flags" => unsafe {
			with_ssl_context_state(object, |context| {
				ssl_context_get_member(context, SslContextMember::HostFlags)
			})
		}
		.unwrap_or_else(|| type_error("SSLContext attribute lookup on non-context")),
		"_msg_callback" => unsafe {
			with_ssl_context_state(object, |context| {
				ssl_context_get_member(context, SslContextMember::MsgCallback)
			})
		}
		.unwrap_or_else(|| type_error("SSLContext attribute lookup on non-context")),
		"set_ciphers" => bound_method(object, "set_ciphers", ssl_context_set_ciphers_entry),
		"load_default_certs" => {
			bound_method(object, "load_default_certs", ssl_context_load_default_certs_entry)
		},
		"load_verify_locations" => {
			bound_method(object, "load_verify_locations", ssl_context_load_verify_locations_entry)
		},
		"load_cert_chain" => {
			bound_method(object, "load_cert_chain", ssl_context_load_cert_chain_entry)
		},
		"set_default_verify_paths" => bound_method(
			object,
			"set_default_verify_paths",
			ssl_context_set_default_verify_paths_entry,
		),
		"cert_store_stats" => {
			bound_method(object, "cert_store_stats", ssl_context_cert_store_stats_entry)
		},
		"wrap_socket" | "_wrap_socket" | "wrap_bio" | "_wrap_bio" => {
			bound_method(object, name, ssl_context_wrap_not_implemented_entry)
		},
		"__enter__" => bound_method(object, "__enter__", ssl_context_enter_entry),
		"__exit__" => bound_method(object, "__exit__", ssl_context_exit_entry),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name)) },
	}
}

unsafe extern "C" fn ssl_context_setattro(
	object: *mut PyObject,
	name: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	let name = match attr_name(name) {
		Ok(name) => name,
		Err(_) => return -1,
	};
	let member = match name {
		"verify_mode" => SslContextMember::VerifyMode,
		"check_hostname" => SslContextMember::CheckHostname,
		"options" => SslContextMember::Options,
		"minimum_version" => SslContextMember::MinimumVersion,
		"maximum_version" => SslContextMember::MaximumVersion,
		"verify_flags" => SslContextMember::VerifyFlags,
		"_host_flags" => SslContextMember::HostFlags,
		"_msg_callback" => SslContextMember::MsgCallback,
		_ => {
			let _ = unsafe { abi::exc::pon_raise_attribute_error(object, intern(name)) };
			return -1;
		},
	};
	match unsafe {
		with_ssl_context_state(object, |context| ssl_context_set_member(context, member, value))
	} {
		Some(status) => status,
		None => {
			let _ = type_error("SSLContext attribute assignment on non-context");
			-1
		},
	}
}

unsafe extern "C" fn ssl_context_set_ciphers_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "set_ciphers") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 2 {
		return type_error("set_ciphers() expects a cipher string");
	}
	if let Err(error) = str_arg(args[1], "ciphers") {
		return error;
	}
	match unsafe { with_ssl_context_state(args[0], |context| context.context.is_some()) } {
		Some(true) => none(),
		Some(false) => runtime_error("OpenSSL context is not available"),
		None => type_error("set_ciphers() receiver must be SSLContext"),
	}
}

fn ssl_rebuild_context(
	context: &mut SslContextState,
	configure: impl FnOnce(&mut SslContextBuilder) -> Result<(), String>,
) -> Result<(), String> {
	let mut builder = SslContextBuilder::new(SslMethod::tls()).map_err(|error| error.to_string())?;
	configure(&mut builder)?;
	context.context = Some(builder.build());
	Ok(())
}

unsafe extern "C" fn ssl_context_set_default_verify_paths_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "set_default_verify_paths") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error("set_default_verify_paths() takes no arguments");
	}
	let result = unsafe {
		with_ssl_context_state(args[0], |context| {
			ssl_rebuild_context(context, |builder| {
				builder
					.set_default_verify_paths()
					.map_err(|error| error.to_string())
			})
		})
	};
	match result {
		Some(Ok(())) => none(),
		Some(Err(error)) => runtime_error(&error),
		None => type_error("set_default_verify_paths() receiver must be SSLContext"),
	}
}

unsafe extern "C" fn ssl_context_load_default_certs_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "load_default_certs") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.is_empty() || args.len() > 2 {
		return type_error("load_default_certs() expects optional purpose");
	}
	let mut set_default_args = [args[0]];
	unsafe {
		ssl_context_set_default_verify_paths_entry(
			set_default_args.as_mut_ptr(),
			set_default_args.len(),
		)
	}
}

unsafe extern "C" fn ssl_context_load_verify_locations_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "load_verify_locations") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.is_empty() || args.len() > 4 {
		return type_error("load_verify_locations() expects cafile, capath, and cadata");
	}
	let cafile = match args.get(1).copied() {
		Some(value) if !value.is_null() && !is_none(value) => match str_arg(value, "cafile") {
			Ok(value) => Some(value),
			Err(error) => return error,
		},
		_ => None,
	};
	let capath = match args.get(2).copied() {
		Some(value) if !value.is_null() && !is_none(value) => match str_arg(value, "capath") {
			Ok(value) => Some(value),
			Err(error) => return error,
		},
		_ => None,
	};
	if args
		.get(3)
		.copied()
		.is_some_and(|value| !value.is_null() && !is_none(value))
	{
		return raise(
			ExceptionKind::NotImplementedError,
			"SSLContext.load_verify_locations(cadata=...) is not implemented",
		);
	}
	if cafile.is_none() && capath.is_none() {
		return value_error("cafile, capath and cadata cannot be all omitted");
	}
	let result = unsafe {
		with_ssl_context_state(args[0], |context| {
			ssl_rebuild_context(context, |builder| {
				builder
					.load_verify_locations(
						cafile.as_deref().map(Path::new),
						capath.as_deref().map(Path::new),
					)
					.map_err(|error| error.to_string())
			})
		})
	};
	match result {
		Some(Ok(())) => none(),
		Some(Err(error)) => runtime_error(&error),
		None => type_error("load_verify_locations() receiver must be SSLContext"),
	}
}

unsafe extern "C" fn ssl_context_load_cert_chain_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "load_cert_chain") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() < 2 || args.len() > 4 {
		return type_error("load_cert_chain() expects certfile, optional keyfile, optional password");
	}
	let certfile = match str_arg(args[1], "certfile") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let keyfile = match args.get(2).copied() {
		Some(value) if !value.is_null() && !is_none(value) => match str_arg(value, "keyfile") {
			Ok(value) => Some(value),
			Err(error) => return error,
		},
		_ => None,
	};
	if args
		.get(3)
		.copied()
		.is_some_and(|value| !value.is_null() && !is_none(value))
	{
		return raise(
			ExceptionKind::NotImplementedError,
			"SSLContext.load_cert_chain(password=...) is not implemented",
		);
	}
	let result = unsafe {
		with_ssl_context_state(args[0], |context| {
			ssl_rebuild_context(context, |builder| {
				builder
					.set_certificate_chain_file(&certfile)
					.map_err(|error| error.to_string())?;
				let private_key = keyfile.as_deref().unwrap_or(certfile.as_str());
				builder
					.set_private_key_file(private_key, SslFiletype::PEM)
					.map_err(|error| error.to_string())?;
				builder
					.check_private_key()
					.map_err(|error| error.to_string())
			})
		})
	};
	match result {
		Some(Ok(())) => none(),
		Some(Err(error)) => runtime_error(&error),
		None => type_error("load_cert_chain() receiver must be SSLContext"),
	}
}

unsafe extern "C" fn ssl_context_cert_store_stats_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "cert_store_stats") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error("cert_store_stats() takes no arguments");
	}
	if unsafe { with_ssl_context_state(args[0], |_| ()).is_none() } {
		return type_error("cert_store_stats() receiver must be SSLContext");
	}
	py_dict_from_pairs(&[("x509", py_int(0)), ("crl", py_int(0)), ("x509_ca", py_int(0))])
}

unsafe extern "C" fn ssl_context_wrap_not_implemented_entry(
	_argv: *mut *mut PyObject,
	_argc: usize,
) -> *mut PyObject {
	raise(ExceptionKind::NotImplementedError, "network SSL wrapping is not implemented")
}

unsafe extern "C" fn ssl_context_exit_entry(
	_argv: *mut *mut PyObject,
	_argc: usize,
) -> *mut PyObject {
	none()
}

unsafe extern "C" fn memory_bio_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	if !kwargs.is_null() {
		return type_error("MemoryBIO() takes no keyword arguments");
	}
	let positional = match unsafe { type_::positional_args_from_object(args) } {
		Ok(positional) => positional,
		Err(message) => return type_error(&message),
	};
	if !positional.is_empty() {
		return type_error("MemoryBIO() takes no arguments");
	}
	Box::into_raw(Box::new(PyMemoryBio {
		ob_base: PyObjectHeader::new(memory_bio_type()),
		buffer:  Vec::new(),
	}))
	.cast::<PyObject>()
}

unsafe fn memory_bio_receiver<'a>(object: *mut PyObject) -> Option<&'a mut PyMemoryBio> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || unsafe { (*object).ob_type } != memory_bio_type().cast_const() {
		return None;
	}
	Some(unsafe { &mut *object.cast::<PyMemoryBio>() })
}

unsafe extern "C" fn memory_bio_write_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "write") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 2 {
		return type_error("write() expects data");
	}
	let Some(bio) = (unsafe { memory_bio_receiver(args[0]) }) else {
		return type_error("write() receiver must be MemoryBIO");
	};
	let bytes = match bytes_or_text_arg(args[1], "data") {
		Ok(bytes) => bytes,
		Err(error) => return error,
	};
	bio.buffer.extend_from_slice(&bytes);
	py_int(bytes.len() as i64)
}

unsafe extern "C" fn memory_bio_read_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "read") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.is_empty() || args.len() > 2 {
		return type_error("read() expects optional size");
	}
	let Some(bio) = (unsafe { memory_bio_receiver(args[0]) }) else {
		return type_error("read() receiver must be MemoryBIO");
	};
	let size = match args.get(1).copied() {
		Some(object) if !object.is_null() && !is_none(object) => match int_arg(object, "size") {
			Ok(size) if size >= 0 => size as usize,
			Ok(_) => bio.buffer.len(),
			Err(error) => return error,
		},
		_ => bio.buffer.len(),
	};
	let take = size.min(bio.buffer.len());
	let out: Vec<u8> = bio.buffer.drain(..take).collect();
	py_bytes(&out)
}

unsafe extern "C" fn memory_bio_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = match attr_name(name) {
		Ok(name) => name,
		Err(error) => return error,
	};
	match name {
		"write" => bound_method(object, "write", memory_bio_write_entry),
		"read" => bound_method(object, "read", memory_bio_read_entry),
		"pending" => unsafe { memory_bio_receiver(object) }.map_or_else(
			|| type_error("pending on non-MemoryBIO"),
			|bio| py_int(bio.buffer.len() as i64),
		),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name)) },
	}
}

unsafe extern "C" fn ssl_rand_status_entry(
	_argv: *mut *mut PyObject,
	_argc: usize,
) -> *mut PyObject {
	py_bool(true)
}

unsafe extern "C" fn ssl_rand_add_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "RAND_add") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.is_empty() {
		return type_error("RAND_add() missing data");
	}
	none()
}

unsafe extern "C" fn ssl_rand_bytes_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "RAND_bytes") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error("RAND_bytes() expects a byte count");
	}
	let count = match int_arg(args[0], "num") {
		Ok(count) if count >= 0 => count as usize,
		Ok(_) => return value_error("num must be non-negative"),
		Err(error) => return error,
	};
	let mut bytes = vec![0_u8; count];
	match openssl::rand::rand_bytes(&mut bytes) {
		Ok(()) => py_bytes(&bytes),
		Err(error) => runtime_error(&error.to_string()),
	}
}

unsafe fn ssl_default_path(component: unsafe extern "C" fn() -> *const c_char) -> String {
	let ptr = unsafe { component() };
	if ptr.is_null() {
		String::new()
	} else {
		unsafe { CStr::from_ptr(ptr) }
			.to_string_lossy()
			.into_owned()
	}
}

unsafe extern "C" fn ssl_get_default_verify_paths_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	if unsafe { argv_slice(argv, argc) }.is_none() {
		return type_error("get_default_verify_paths() received a null argument vector");
	}
	if argc != 0 {
		return type_error("get_default_verify_paths() takes no arguments");
	}
	let values = unsafe {
		[
			ssl_default_path(X509_get_default_cert_file_env),
			ssl_default_path(X509_get_default_cert_file),
			ssl_default_path(X509_get_default_cert_dir_env),
			ssl_default_path(X509_get_default_cert_dir),
		]
	};
	let objects = values.iter().map(|value| py_str(value)).collect::<Vec<_>>();
	if objects.iter().any(|object| object.is_null()) {
		return ptr::null_mut();
	}
	alloc_tuple(objects)
}

fn ssl_known_oid(nid: Nid, fallback: Option<&str>) -> String {
	match nid.as_raw() {
		13 => "2.5.4.3".to_owned(),
		129 => "1.3.6.1.5.5.7.3.1".to_owned(),
		130 => "1.3.6.1.5.5.7.3.2".to_owned(),
		_ => fallback.unwrap_or("").to_owned(),
	}
}

fn ssl_asn1_tuple(nid: Nid, oid_hint: Option<&str>) -> *mut PyObject {
	let short = match nid.short_name() {
		Ok(short) => short,
		Err(error) => return value_error(&error.to_string()),
	};
	let long = match nid.long_name() {
		Ok(long) => long,
		Err(error) => return value_error(&error.to_string()),
	};
	let oid = ssl_known_oid(nid, oid_hint);
	let values = vec![py_int(i64::from(nid.as_raw())), py_str(short), py_str(long), py_str(&oid)];
	if values.iter().any(|object| object.is_null()) {
		return ptr::null_mut();
	}
	alloc_tuple(values)
}

unsafe extern "C" fn ssl_txt2obj_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "txt2obj") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.is_empty() || args.len() > 2 {
		return type_error("txt2obj() expects txt and optional name");
	}
	let text = match str_arg(args[0], "txt") {
		Ok(text) => text,
		Err(error) => return error,
	};
	let name = match args.get(1).copied() {
		Some(value) if !value.is_null() && !is_none(value) => bool_arg(value).unwrap_or(false),
		_ => false,
	};
	let dotted = text
		.bytes()
		.all(|byte| byte.is_ascii_digit() || byte == b'.');
	if !name && !dotted {
		return value_error(&format!("unknown object '{text}'"));
	}
	let object = match Asn1Object::from_str(&text) {
		Ok(object) => object,
		Err(error) => return value_error(&error.to_string()),
	};
	ssl_asn1_tuple(object.nid(), dotted.then_some(text.as_str()))
}

unsafe extern "C" fn ssl_nid2obj_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "nid2obj") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error("nid2obj() expects nid");
	}
	let nid = match int_arg(args[0], "nid") {
		Ok(nid) if nid >= i64::from(c_int::MIN) && nid <= i64::from(c_int::MAX) => {
			Nid::from_raw(nid as c_int)
		},
		Ok(_) => return value_error("nid out of range"),
		Err(error) => return error,
	};
	ssl_asn1_tuple(nid, None)
}

fn openssl_version_info() -> *mut PyObject {
	let text = openssl::version::version();
	let mut nums = text
		.split_whitespace()
		.nth(1)
		.unwrap_or("0.0.0")
		.split('.')
		.take(3)
		.map(|part| {
			part
				.chars()
				.take_while(|ch| ch.is_ascii_digit())
				.collect::<String>()
		})
		.map(|part| part.parse::<i64>().unwrap_or(0))
		.collect::<Vec<_>>();
	while nums.len() < 3 {
		nums.push(0);
	}
	alloc_tuple(vec![py_int(nums[0]), py_int(nums[1]), py_int(nums[2]), py_int(0), py_int(0)])
}

fn ssl_attrs(name: &str) -> Result<Vec<(u32, *mut PyObject)>, String> {
	let classes = [
		*SSL_ERROR_CLASS,
		*SSL_ZERO_RETURN_ERROR_CLASS,
		*SSL_WANT_READ_ERROR_CLASS,
		*SSL_WANT_WRITE_ERROR_CLASS,
		*SSL_SYSCALL_ERROR_CLASS,
		*SSL_EOF_ERROR_CLASS,
		*SSL_CERT_VERIFICATION_ERROR_CLASS,
	];
	if classes.contains(&0) {
		return Err("failed to create ssl exception classes".to_owned());
	}
	Ok(vec![
		str_attr("__name__", name)?,
		(intern("_SSLContext"), ssl_context_type().cast::<PyObject>()),
		(intern("MemoryBIO"), memory_bio_type().cast::<PyObject>()),
		(intern("SSLSession"), ssl_session_type().cast::<PyObject>()),
		(intern("SSLError"), classes[0] as *mut PyObject),
		(intern("SSLZeroReturnError"), classes[1] as *mut PyObject),
		(intern("SSLWantReadError"), classes[2] as *mut PyObject),
		(intern("SSLWantWriteError"), classes[3] as *mut PyObject),
		(intern("SSLSyscallError"), classes[4] as *mut PyObject),
		(intern("SSLEOFError"), classes[5] as *mut PyObject),
		(intern("SSLCertVerificationError"), classes[6] as *mut PyObject),
		int_attr("OPENSSL_VERSION_NUMBER", openssl::version::number() as i64)?,
		str_attr("OPENSSL_VERSION", openssl::version::version())?,
		(intern("OPENSSL_VERSION_INFO"), openssl_version_info()),
		int_attr("_OPENSSL_API_VERSION", 0x3000_0000)?,
		str_attr(
			"_DEFAULT_CIPHERS",
			"TLS_AES_256_GCM_SHA384:TLS_CHACHA20_POLY1305_SHA256:TLS_AES_128_GCM_SHA256",
		)?,
		function_attr("RAND_status", "RAND_status", ssl_rand_status_entry)?,
		function_attr("RAND_add", "RAND_add", ssl_rand_add_entry)?,
		function_attr("RAND_bytes", "RAND_bytes", ssl_rand_bytes_entry)?,
		function_attr("txt2obj", "txt2obj", ssl_txt2obj_entry)?,
		function_attr("nid2obj", "nid2obj", ssl_nid2obj_entry)?,
		function_attr(
			"get_default_verify_paths",
			"get_default_verify_paths",
			ssl_get_default_verify_paths_entry,
		)?,
		int_attr("CERT_NONE", PY_SSL_CERT_NONE)?,
		int_attr("CERT_OPTIONAL", PY_SSL_CERT_OPTIONAL)?,
		int_attr("CERT_REQUIRED", PY_SSL_CERT_REQUIRED)?,
		int_attr("PROTOCOL_TLS", PY_SSL_PROTOCOL_TLS)?,
		int_attr("PROTOCOL_SSLv23", PY_SSL_PROTOCOL_TLS)?,
		int_attr("PROTOCOL_TLSv1", PY_SSL_PROTOCOL_TLSV1)?,
		int_attr("PROTOCOL_TLSv1_1", PY_SSL_PROTOCOL_TLSV1_1)?,
		int_attr("PROTOCOL_TLSv1_2", PY_SSL_PROTOCOL_TLSV1_2)?,
		int_attr("PROTOCOL_TLS_CLIENT", PY_SSL_PROTOCOL_TLS_CLIENT)?,
		int_attr("PROTOCOL_TLS_SERVER", PY_SSL_PROTOCOL_TLS_SERVER)?,
		int_attr("PROTO_MINIMUM_SUPPORTED", PY_SSL_TLS_MINIMUM_SUPPORTED)?,
		int_attr("PROTO_MAXIMUM_SUPPORTED", PY_SSL_TLS_MAXIMUM_SUPPORTED)?,
		int_attr("PROTO_SSLv3", PY_SSL_SSLV3)?,
		int_attr("PROTO_TLSv1", PY_SSL_TLSV1)?,
		int_attr("PROTO_TLSv1_1", PY_SSL_TLSV1_1)?,
		int_attr("PROTO_TLSv1_2", PY_SSL_TLSV1_2)?,
		int_attr("PROTO_TLSv1_3", PY_SSL_TLSV1_3)?,
		int_attr("ENCODING_PEM", 1)?,
		int_attr("ENCODING_DER", 2)?,
		bool_attr("HAS_SNI", true)?,
		bool_attr("HAS_ECDH", true)?,
		bool_attr("HAS_NPN", false)?,
		bool_attr("HAS_ALPN", true)?,
		bool_attr("HAS_SSLv2", false)?,
		bool_attr("HAS_SSLv3", false)?,
		bool_attr("HAS_TLSv1", false)?,
		bool_attr("HAS_TLSv1_1", false)?,
		bool_attr("HAS_TLSv1_2", true)?,
		bool_attr("HAS_TLSv1_3", true)?,
		bool_attr("HAS_PSK", false)?,
		bool_attr("HAS_PHA", true)?,
		bool_attr("HAS_TLS_UNIQUE", true)?,
		int_attr("OP_ALL", 2_147_483_728)?,
		int_attr("OP_NO_SSLv2", 0)?,
		int_attr("OP_NO_SSLv3", 33_554_432)?,
		int_attr("OP_NO_TLSv1", 67_108_864)?,
		int_attr("OP_NO_TLSv1_1", 268_435_456)?,
		int_attr("OP_NO_TLSv1_2", 134_217_728)?,
		int_attr("OP_NO_TLSv1_3", 536_870_912)?,
		int_attr("OP_CIPHER_SERVER_PREFERENCE", 4_194_304)?,
		int_attr("OP_ENABLE_KTLS", 8)?,
		int_attr("OP_ENABLE_MIDDLEBOX_COMPAT", 1_048_576)?,
		int_attr("OP_IGNORE_UNEXPECTED_EOF", 128)?,
		int_attr("OP_LEGACY_SERVER_CONNECT", 4)?,
		int_attr("OP_NO_COMPRESSION", 131_072)?,
		int_attr("OP_NO_RENEGOTIATION", 1_073_741_824)?,
		int_attr("OP_NO_TICKET", 16_384)?,
		int_attr("OP_SINGLE_DH_USE", 0)?,
		int_attr("OP_SINGLE_ECDH_USE", 0)?,
		int_attr("VERIFY_DEFAULT", 0)?,
		int_attr("VERIFY_CRL_CHECK_LEAF", 4)?,
		int_attr("VERIFY_CRL_CHECK_CHAIN", 12)?,
		int_attr("VERIFY_X509_STRICT", 32)?,
		int_attr("VERIFY_ALLOW_PROXY_CERTS", 64)?,
		int_attr("VERIFY_X509_TRUSTED_FIRST", PY_SSL_VERIFY_X509_TRUSTED_FIRST)?,
		int_attr("VERIFY_X509_PARTIAL_CHAIN", 524_288)?,
		int_attr("HOSTFLAG_ALWAYS_CHECK_SUBJECT", 1)?,
		int_attr("HOSTFLAG_NO_WILDCARDS", 2)?,
		int_attr("HOSTFLAG_NO_PARTIAL_WILDCARDS", PY_SSL_HOSTFLAG_NO_PARTIAL_WILDCARDS)?,
		int_attr("HOSTFLAG_MULTI_LABEL_WILDCARDS", 8)?,
		int_attr("HOSTFLAG_SINGLE_LABEL_SUBDOMAINS", 16)?,
		int_attr("HOSTFLAG_NEVER_CHECK_SUBJECT", PY_SSL_HOSTFLAG_NEVER_CHECK_SUBJECT)?,
		int_attr("SSL_ERROR_ZERO_RETURN", 6)?,
		int_attr("SSL_ERROR_WANT_READ", 2)?,
		int_attr("SSL_ERROR_WANT_WRITE", 3)?,
		int_attr("SSL_ERROR_WANT_X509_LOOKUP", 4)?,
		int_attr("SSL_ERROR_SYSCALL", 5)?,
		int_attr("SSL_ERROR_SSL", 1)?,
		int_attr("SSL_ERROR_WANT_CONNECT", 7)?,
		int_attr("SSL_ERROR_EOF", 8)?,
		int_attr("SSL_ERROR_INVALID_ERROR_CODE", 9)?,
		int_attr("ALERT_DESCRIPTION_CLOSE_NOTIFY", 0)?,
		int_attr("ALERT_DESCRIPTION_UNEXPECTED_MESSAGE", 10)?,
		int_attr("ALERT_DESCRIPTION_BAD_RECORD_MAC", 20)?,
		int_attr("ALERT_DESCRIPTION_RECORD_OVERFLOW", 22)?,
		int_attr("ALERT_DESCRIPTION_DECOMPRESSION_FAILURE", 30)?,
		int_attr("ALERT_DESCRIPTION_HANDSHAKE_FAILURE", 40)?,
		int_attr("ALERT_DESCRIPTION_BAD_CERTIFICATE", 42)?,
		int_attr("ALERT_DESCRIPTION_UNSUPPORTED_CERTIFICATE", 43)?,
		int_attr("ALERT_DESCRIPTION_CERTIFICATE_REVOKED", 44)?,
		int_attr("ALERT_DESCRIPTION_CERTIFICATE_EXPIRED", 45)?,
		int_attr("ALERT_DESCRIPTION_CERTIFICATE_UNKNOWN", 46)?,
		int_attr("ALERT_DESCRIPTION_ILLEGAL_PARAMETER", 47)?,
		int_attr("ALERT_DESCRIPTION_UNKNOWN_CA", 48)?,
		int_attr("ALERT_DESCRIPTION_ACCESS_DENIED", 49)?,
		int_attr("ALERT_DESCRIPTION_DECODE_ERROR", 50)?,
		int_attr("ALERT_DESCRIPTION_DECRYPT_ERROR", 51)?,
		int_attr("ALERT_DESCRIPTION_PROTOCOL_VERSION", 70)?,
		int_attr("ALERT_DESCRIPTION_INSUFFICIENT_SECURITY", 71)?,
		int_attr("ALERT_DESCRIPTION_INTERNAL_ERROR", 80)?,
		int_attr("ALERT_DESCRIPTION_USER_CANCELLED", 90)?,
		int_attr("ALERT_DESCRIPTION_NO_RENEGOTIATION", 100)?,
		int_attr("ALERT_DESCRIPTION_UNSUPPORTED_EXTENSION", 110)?,
		int_attr("ALERT_DESCRIPTION_CERTIFICATE_UNOBTAINABLE", 111)?,
		int_attr("ALERT_DESCRIPTION_UNRECOGNIZED_NAME", 112)?,
		int_attr("ALERT_DESCRIPTION_BAD_CERTIFICATE_STATUS_RESPONSE", 113)?,
		int_attr("ALERT_DESCRIPTION_BAD_CERTIFICATE_HASH_VALUE", 114)?,
		int_attr("ALERT_DESCRIPTION_UNKNOWN_PSK_IDENTITY", 115)?,
	])
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	install_module("_ssl", ssl_attrs("_ssl")?)
}
