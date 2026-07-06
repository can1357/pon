//! Native subsystem-backed stdlib modules for large C-extension surfaces.
//!
//! This file deliberately implements only behavior backed by real platform
//! facilities: SQLite through `rusqlite`, TLS context/random/version data
//! through OpenSSL, dynamic-library loading through `dlopen`/`dlsym`, and dbm
//! through the platform ndbm API.  Surfaces that would require full CPython
//! subsystem parity are left absent rather than faked.

use core::{
	ffi::{c_char, c_int, c_void},
	ptr,
};
use std::{
	ffi::{CStr, CString},
	sync::{
		LazyLock,
		atomic::{AtomicBool, Ordering},
	},
};

use num_traits::ToPrimitive as _;
use rusqlite::{
	Connection as RusqliteConnection, OpenFlags, ffi, params_from_iter,
	types::{Value as SqlValueRaw, ValueRef},
};

use super::{
	builtins_mod::{VARIADIC_ARITY, alloc_list, alloc_tuple},
	install_module,
};
#[cfg(target_os = "macos")]
use crate::object::PyMappingMethods;
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
#[cfg(target_os = "macos")]
unsafe extern "C" {
	fn _dyld_shared_cache_contains_path(path: *const c_char) -> bool;
}

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

#[cfg(target_os = "macos")]
fn key_error(message: &str) -> *mut PyObject {
	raise(ExceptionKind::KeyError, message)
}

fn os_error(message: &str) -> *mut PyObject {
	raise(ExceptionKind::OSError, message)
}

fn py_str(text: &str) -> *mut PyObject {
	// SAFETY: Runtime copies `text` and reports allocation errors via NULL.
	unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn py_bytes(bytes: &[u8]) -> *mut PyObject {
	// SAFETY: Runtime copies `bytes` and reports allocation errors via NULL.
	unsafe { abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) }
}

fn py_int(value: i64) -> *mut PyObject {
	// SAFETY: Runtime integer allocator follows the NULL-sentinel contract.
	unsafe { abi::pon_const_int(value) }
}

fn py_float(value: f64) -> *mut PyObject {
	// SAFETY: Runtime float allocator follows the NULL-sentinel contract.
	unsafe { abi::number::pon_const_float(value) }
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

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
	let object = py_int(value);
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate integer attribute {name}"))
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

#[cfg(target_os = "macos")]
fn optional_str_arg(
	args: &[*mut PyObject],
	index: usize,
	default: &str,
	name: &str,
) -> Result<String, *mut PyObject> {
	match args.get(index).copied() {
		Some(object) if !object.is_null() && !is_none(object) => str_arg(object, name),
		_ => Ok(default.to_owned()),
	}
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

fn sequence_items(object: *mut PyObject, name: &str) -> Result<Vec<*mut PyObject>, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() {
		return Err(type_error(&format!("{name} must be a sequence")));
	}
	match unsafe { crate::types::dict::type_name(object) } {
		Some("list") => {
			Ok(unsafe { (*object.cast::<crate::types::list::PyList>()).as_slice() }.to_vec())
		},
		Some("tuple") => {
			Ok(unsafe { (*object.cast::<crate::types::tuple::PyTuple>()).as_slice() }.to_vec())
		},
		_ => Err(type_error(&format!("{name} must be a sequence, not '{}'", type_name(object)))),
	}
}

fn optional_attr(object: *mut PyObject, name: &str) -> Option<*mut PyObject> {
	let value = unsafe { abi::pon_get_attr(object, intern(name), ptr::null_mut()) };
	if value.is_null() {
		pon_err_clear();
		None
	} else {
		Some(value)
	}
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

unsafe fn module_attr_object(module_name: &str, attr_name: &str) -> Option<*mut PyObject> {
	let module = crate::import::cached_module(intern(module_name))?;
	let module = module.cast::<crate::import::PyModuleObject>();
	unsafe { (*module).attrs.get(&intern(attr_name)).copied() }
}

fn sqlite_registry_dict(attr_name: &str) -> Result<*mut PyObject, *mut PyObject> {
	let Some(dict) = (unsafe { module_attr_object("_sqlite3", attr_name) }) else {
		return Err(runtime_error(&format!("_sqlite3.{attr_name} registry is missing")));
	};
	if unsafe { !crate::types::dict::is_dict(dict) } {
		return Err(runtime_error(&format!("_sqlite3.{attr_name} registry is not a dict")));
	}
	Ok(dict)
}

fn sqlite_exact_type_object(object: *mut PyObject) -> Option<*mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() {
		None
	} else {
		Some(unsafe { (*object).ob_type as *mut PyObject })
	}
}

fn sqlite_adapter_key(
	type_object: *mut PyObject,
	protocol: *mut PyObject,
) -> Result<*mut PyObject, *mut PyObject> {
	let key = alloc_tuple(vec![type_object, protocol]);
	if key.is_null() {
		Err(ptr::null_mut())
	} else {
		Ok(key)
	}
}

fn sqlite_adapter_for_type(
	type_object: *mut PyObject,
	protocol: *mut PyObject,
) -> Result<Option<*mut PyObject>, *mut PyObject> {
	let Some(dict) = (unsafe { module_attr_object("_sqlite3", "adapters") }) else {
		return Ok(None);
	};
	if unsafe { !crate::types::dict::is_dict(dict) } {
		return Ok(None);
	}
	let key = sqlite_adapter_key(type_object, protocol)?;
	match unsafe { crate::types::dict::dict_get(dict, key) } {
		Ok(found) => Ok(found),
		Err(message) => Err(runtime_error(&message)),
	}
}

fn sqlite_call_unary(
	callable: *mut PyObject,
	arg: *mut PyObject,
) -> Result<*mut PyObject, *mut PyObject> {
	let mut argv = [arg];
	let result = unsafe { abi::pon_call(callable, argv.as_mut_ptr(), argv.len()) };
	if result.is_null() {
		Err(ptr::null_mut())
	} else {
		Ok(result)
	}
}

fn sqlite_conform(
	object: *mut PyObject,
	protocol: *mut PyObject,
) -> Result<Option<*mut PyObject>, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() {
		return Ok(None);
	}
	let conform = unsafe { abi::pon_get_attr(object, intern("__conform__"), ptr::null_mut()) };
	if conform.is_null() {
		pon_err_clear();
		return Ok(None);
	}
	let adapted = sqlite_call_unary(conform, protocol)?;
	if is_none(adapted) {
		Ok(None)
	} else {
		Ok(Some(adapted))
	}
}

fn sqlite_adapt_for_binding(object: *mut PyObject) -> Result<Option<*mut PyObject>, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() {
		return Ok(None);
	}
	let protocol = sqlite_prepare_protocol_type().cast::<PyObject>();
	if let Some(type_object) = sqlite_exact_type_object(object) {
		if let Some(adapter) = sqlite_adapter_for_type(type_object, protocol)? {
			return sqlite_call_unary(adapter, object).map(Some);
		}
	}
	sqlite_conform(object, protocol)
}

// ---------------------------------------------------------------------------
// `_sqlite3` / `sqlite3`

#[derive(Clone, Debug)]
enum SqlValue {
	Null,
	Integer(i64),
	Real(f64),
	Text(String),
	Blob(Vec<u8>),
}

#[repr(C)]
struct PySqliteConnection {
	ob_base: PyObjectHeader,
	conn:    Option<RusqliteConnection>,
}

#[repr(C)]
struct PySqliteCursor {
	ob_base:    PyObjectHeader,
	connection: *mut PySqliteConnection,
	rows:       Vec<Vec<SqlValue>>,
	index:      usize,
	rowcount:   isize,
}

#[repr(C)]
struct PySqlitePrepareProtocol {
	ob_base: PyObjectHeader,
}

static SQLITE_ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
	exception_class("sqlite3", "Error", "Exception").map_or(0, |class| class as usize)
});
static SQLITE_WARNING_CLASS: LazyLock<usize> = LazyLock::new(|| {
	exception_class("sqlite3", "Warning", "Exception").map_or(0, |class| class as usize)
});
static SQLITE_DATABASE_ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
	let base = *SQLITE_ERROR_CLASS as *mut PyObject;
	exception_class_from_base("sqlite3", "DatabaseError", base).map_or(0, |class| class as usize)
});
static SQLITE_OPERATIONAL_ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
	let base = *SQLITE_DATABASE_ERROR_CLASS as *mut PyObject;
	exception_class_from_base("sqlite3", "OperationalError", base).map_or(0, |class| class as usize)
});
static SQLITE_INTEGRITY_ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
	let base = *SQLITE_DATABASE_ERROR_CLASS as *mut PyObject;
	exception_class_from_base("sqlite3", "IntegrityError", base).map_or(0, |class| class as usize)
});
static SQLITE_PROGRAMMING_ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
	let base = *SQLITE_DATABASE_ERROR_CLASS as *mut PyObject;
	exception_class_from_base("sqlite3", "ProgrammingError", base).map_or(0, |class| class as usize)
});
static SQLITE_INTERFACE_ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
	let base = *SQLITE_ERROR_CLASS as *mut PyObject;
	exception_class_from_base("sqlite3", "InterfaceError", base).map_or(0, |class| class as usize)
});
static SQLITE_DATA_ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
	let base = *SQLITE_DATABASE_ERROR_CLASS as *mut PyObject;
	exception_class_from_base("sqlite3", "DataError", base).map_or(0, |class| class as usize)
});
static SQLITE_INTERNAL_ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
	let base = *SQLITE_DATABASE_ERROR_CLASS as *mut PyObject;
	exception_class_from_base("sqlite3", "InternalError", base).map_or(0, |class| class as usize)
});
static SQLITE_NOT_SUPPORTED_ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
	let base = *SQLITE_DATABASE_ERROR_CLASS as *mut PyObject;
	exception_class_from_base("sqlite3", "NotSupportedError", base).map_or(0, |class| class as usize)
});

fn sqlite_error(message: &str) -> *mut PyObject {
	raise_class(&SQLITE_OPERATIONAL_ERROR_CLASS, ExceptionKind::RuntimeError, message)
}

static SQLITE_CONNECTION_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"sqlite3.Connection",
		core::mem::size_of::<PySqliteConnection>(),
	);
	ty.tp_base = object_type();
	ty.tp_getattro = Some(sqlite_connection_getattro);
	Box::into_raw(Box::new(ty)) as usize
});

static SQLITE_CURSOR_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"sqlite3.Cursor",
		core::mem::size_of::<PySqliteCursor>(),
	);
	ty.tp_base = object_type();
	ty.tp_getattro = Some(sqlite_cursor_getattro);
	ty.tp_iter = Some(sqlite_cursor_iter);
	ty.tp_iternext = Some(sqlite_cursor_iternext);
	Box::into_raw(Box::new(ty)) as usize
});

static SQLITE_PREPARE_PROTOCOL_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"sqlite3.PrepareProtocol",
		core::mem::size_of::<PySqlitePrepareProtocol>(),
	);
	ty.tp_base = object_type();
	ty.tp_new = Some(sqlite_prepare_protocol_new);
	Box::into_raw(Box::new(ty)) as usize
});

static SQLITE_CALLBACK_TRACEBACKS: AtomicBool = AtomicBool::new(false);

fn sqlite_connection_type() -> *mut PyType {
	*SQLITE_CONNECTION_TYPE as *mut PyType
}

fn sqlite_cursor_type() -> *mut PyType {
	*SQLITE_CURSOR_TYPE as *mut PyType
}

fn sqlite_prepare_protocol_type() -> *mut PyType {
	*SQLITE_PREPARE_PROTOCOL_TYPE as *mut PyType
}

fn alloc_sqlite_connection(conn: RusqliteConnection) -> *mut PyObject {
	Box::into_raw(Box::new(PySqliteConnection {
		ob_base: PyObjectHeader::new(sqlite_connection_type()),
		conn:    Some(conn),
	}))
	.cast::<PyObject>()
}

fn alloc_sqlite_cursor(
	connection: *mut PySqliteConnection,
	rows: Vec<Vec<SqlValue>>,
	rowcount: isize,
) -> *mut PyObject {
	Box::into_raw(Box::new(PySqliteCursor {
		ob_base: PyObjectHeader::new(sqlite_cursor_type()),
		connection,
		rows,
		index: 0,
		rowcount,
	}))
	.cast::<PyObject>()
}

unsafe fn sqlite_connection_receiver<'a>(
	object: *mut PyObject,
) -> Option<&'a mut PySqliteConnection> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || unsafe { (*object).ob_type } != sqlite_connection_type().cast_const() {
		return None;
	}
	Some(unsafe { &mut *object.cast::<PySqliteConnection>() })
}

unsafe fn sqlite_cursor_receiver<'a>(object: *mut PyObject) -> Option<&'a mut PySqliteCursor> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || unsafe { (*object).ob_type } != sqlite_cursor_type().cast_const() {
		return None;
	}
	Some(unsafe { &mut *object.cast::<PySqliteCursor>() })
}

fn sqlite_py_to_value_direct(object: *mut PyObject) -> Result<SqlValueRaw, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || is_none(object) {
		return Ok(SqlValueRaw::Null);
	}
	if let Some(value) = unsafe { crate::types::bool_::to_bool(object) } {
		return Ok(SqlValueRaw::Integer(i64::from(value)));
	}
	if let Some(value) =
		unsafe { crate::types::int::to_bigint(object) }.and_then(|value| value.to_i64())
	{
		return Ok(SqlValueRaw::Integer(value));
	}
	if let Some(value) = unsafe { crate::types::float::to_f64(object) } {
		return Ok(SqlValueRaw::Real(value));
	}
	if let Some(text) = unsafe { type_::unicode_text(object) } {
		return Ok(SqlValueRaw::Text(text.to_owned()));
	}
	if let Some(bytes) = bytes_like(object) {
		return Ok(SqlValueRaw::Blob(bytes.to_vec()));
	}
	Err(type_error(&format!("SQLite parameter type '{}' is not supported", type_name(object))))
}

fn sqlite_py_to_value(object: *mut PyObject) -> Result<SqlValueRaw, *mut PyObject> {
	if let Some(adapted) = sqlite_adapt_for_binding(object)? {
		return sqlite_py_to_value_direct(adapted);
	}
	sqlite_py_to_value_direct(object)
}

fn sqlite_params_from_object(
	object: Option<*mut PyObject>,
) -> Result<Vec<SqlValueRaw>, *mut PyObject> {
	let Some(object) = object else {
		return Ok(Vec::new());
	};
	let object = crate::tag::untag_arg(object);
	if object.is_null() || is_none(object) {
		return Ok(Vec::new());
	}
	if let Some(items) = unsafe { abi::seq::tuple_storage_slice(object) } {
		return items.iter().copied().map(sqlite_py_to_value).collect();
	}
	if unsafe { crate::types::int::type_name_is(object, "list") } {
		let items = unsafe { (&*object.cast::<crate::types::list::PyList>()).as_slice() };
		return items.iter().copied().map(sqlite_py_to_value).collect();
	}
	Err(type_error("parameters must be a sequence"))
}

fn sqlite_value_from_ref(value: ValueRef<'_>) -> SqlValue {
	match value {
		ValueRef::Null => SqlValue::Null,
		ValueRef::Integer(value) => SqlValue::Integer(value),
		ValueRef::Real(value) => SqlValue::Real(value),
		ValueRef::Text(value) => SqlValue::Text(String::from_utf8_lossy(value).into_owned()),
		ValueRef::Blob(value) => SqlValue::Blob(value.to_vec()),
	}
}

fn sqlite_value_to_py(value: &SqlValue) -> *mut PyObject {
	match value {
		SqlValue::Null => none(),
		SqlValue::Integer(value) => py_int(*value),
		SqlValue::Real(value) => py_float(*value),
		SqlValue::Text(value) => py_str(value),
		SqlValue::Blob(value) => py_bytes(value),
	}
}

fn sqlite_row_to_tuple(row: &[SqlValue]) -> *mut PyObject {
	let mut values = Vec::with_capacity(row.len());
	for value in row {
		let object = sqlite_value_to_py(value);
		if object.is_null() {
			return ptr::null_mut();
		}
		values.push(object);
	}
	alloc_tuple(values)
}

fn sqlite_execute_core(
	conn: &mut RusqliteConnection,
	sql: &str,
	params: Vec<SqlValueRaw>,
) -> Result<(Vec<Vec<SqlValue>>, isize), String> {
	let mut stmt = conn.prepare(sql).map_err(|error| error.to_string())?;
	let column_count = stmt.column_count();
	if column_count == 0 {
		let changed = stmt
			.execute(params_from_iter(params.iter()))
			.map_err(|error| error.to_string())?;
		return Ok((Vec::new(), changed as isize));
	}
	let mut rows = stmt
		.query(params_from_iter(params.iter()))
		.map_err(|error| error.to_string())?;
	let mut out = Vec::new();
	while let Some(row) = rows.next().map_err(|error| error.to_string())? {
		let mut values = Vec::with_capacity(column_count);
		for index in 0..column_count {
			values.push(sqlite_value_from_ref(row.get_ref(index).map_err(|error| error.to_string())?));
		}
		out.push(values);
	}
	Ok((out, -1))
}
unsafe extern "C" fn sqlite_prepare_protocol_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	if !kwargs.is_null() {
		return type_error("PrepareProtocol() takes no keyword arguments");
	}
	let positional = match unsafe { type_::positional_args_from_object(args) } {
		Ok(positional) => positional,
		Err(message) => return type_error(&message),
	};
	if !positional.is_empty() {
		return type_error(&format!(
			"PrepareProtocol() takes no arguments, got {}",
			positional.len()
		));
	}
	Box::into_raw(Box::new(PySqlitePrepareProtocol {
		ob_base: PyObjectHeader::new(sqlite_prepare_protocol_type()),
	}))
	.cast::<PyObject>()
}

unsafe extern "C" fn sqlite_adapt_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "adapt") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.is_empty() || args.len() > 3 {
		return type_error(&format!("adapt() expected 1 to 3 arguments, got {}", args.len()));
	}
	let object = crate::tag::untag_arg(args[0]);
	let protocol = args
		.get(1)
		.copied()
		.map(crate::tag::untag_arg)
		.unwrap_or_else(|| sqlite_prepare_protocol_type().cast::<PyObject>());
	if let Some(type_object) = sqlite_exact_type_object(object) {
		if let Some(adapter) = match sqlite_adapter_for_type(type_object, protocol) {
			Ok(adapter) => adapter,
			Err(error) => return error,
		} {
			return match sqlite_call_unary(adapter, object) {
				Ok(adapted) => adapted,
				Err(error) => error,
			};
		}
	}
	match sqlite_conform(object, protocol) {
		Ok(Some(adapted)) => return adapted,
		Ok(None) => {},
		Err(error) => return error,
	}
	if let Some(alternate) = args.get(2).copied() {
		return alternate;
	}
	raise_class(&SQLITE_PROGRAMMING_ERROR_CLASS, ExceptionKind::RuntimeError, "can't adapt")
}

unsafe extern "C" fn sqlite_register_adapter_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "register_adapter") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 2 {
		return type_error(&format!("register_adapter() expected 2 arguments, got {}", args.len()));
	}
	let type_object = crate::tag::untag_arg(args[0]);
	if type_object.is_null() {
		return type_error("register_adapter() type argument is NULL");
	}
	let protocol = sqlite_prepare_protocol_type().cast::<PyObject>();
	let key = match sqlite_adapter_key(type_object, protocol) {
		Ok(key) => key,
		Err(error) => return error,
	};
	let dict = match sqlite_registry_dict("adapters") {
		Ok(dict) => dict,
		Err(error) => return error,
	};
	match unsafe { crate::types::dict::dict_insert(dict, key, args[1]) } {
		Ok(()) => none(),
		Err(message) => runtime_error(&message),
	}
}

unsafe extern "C" fn sqlite_register_converter_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "register_converter") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 2 {
		return type_error(&format!("register_converter() expected 2 arguments, got {}", args.len()));
	}
	let dict = match sqlite_registry_dict("converters") {
		Ok(dict) => dict,
		Err(error) => return error,
	};
	let name = match str_arg(args[0], "typename") {
		Ok(name) => name,
		Err(error) => return error,
	};
	let key = py_str(&name.to_ascii_uppercase());
	if key.is_null() {
		return ptr::null_mut();
	}
	match unsafe { crate::types::dict::dict_insert(dict, key, args[1]) } {
		Ok(()) => none(),
		Err(message) => runtime_error(&message),
	}
}

unsafe extern "C" fn sqlite_complete_statement_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "complete_statement") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error(&format!("complete_statement() expected 1 argument, got {}", args.len()));
	}
	let sql = match str_arg(args[0], "statement") {
		Ok(sql) => sql,
		Err(error) => return error,
	};
	let c_sql = match CString::new(sql) {
		Ok(sql) => sql,
		Err(_) => return value_error("embedded null character"),
	};
	py_bool(unsafe { ffi::sqlite3_complete(c_sql.as_ptr()) != 0 })
}

unsafe extern "C" fn sqlite_enable_callback_tracebacks_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "enable_callback_tracebacks") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error(&format!(
			"enable_callback_tracebacks() expected 1 argument, got {}",
			args.len()
		));
	}
	let truth = unsafe { abi::pon_is_true(args[0]) };
	if truth < 0 {
		return ptr::null_mut();
	}
	SQLITE_CALLBACK_TRACEBACKS.store(truth != 0, Ordering::Relaxed);
	none()
}

unsafe extern "C" fn sqlite_connect_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "connect") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.is_empty() || args.len() > 9 {
		return type_error(&format!(
			"connect() expected database and optional arguments, got {}",
			args.len()
		));
	}
	let database = match str_arg(args[0], "database") {
		Ok(database) => database,
		Err(error) => return error,
	};
	let uri = args
		.get(7)
		.copied()
		.and_then(bool_arg)
		.unwrap_or_else(|| database.starts_with("file:"));
	let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE;
	if uri {
		flags |= OpenFlags::SQLITE_OPEN_URI;
	}
	match RusqliteConnection::open_with_flags(&database, flags) {
		Ok(conn) => alloc_sqlite_connection(conn),
		Err(error) => sqlite_error(&error.to_string()),
	}
}

unsafe extern "C" fn sqlite_connection_cursor_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "cursor") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error(&format!(
			"cursor() expected no arguments, got {}",
			args.len().saturating_sub(1)
		));
	}
	let Some(connection) = (unsafe { sqlite_connection_receiver(args[0]) }) else {
		return type_error("cursor() receiver must be a sqlite3.Connection");
	};
	if connection.conn.is_none() {
		return sqlite_error("Cannot operate on a closed database.");
	}
	alloc_sqlite_cursor(connection, Vec::new(), -1)
}

unsafe extern "C" fn sqlite_connection_execute_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "execute") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() < 2 || args.len() > 3 {
		return type_error(&format!(
			"execute() expected sql and optional parameters, got {}",
			args.len().saturating_sub(1)
		));
	}
	let Some(connection) = (unsafe { sqlite_connection_receiver(args[0]) }) else {
		return type_error("execute() receiver must be a sqlite3.Connection");
	};
	let Some(conn) = connection.conn.as_mut() else {
		return sqlite_error("Cannot operate on a closed database.");
	};
	let sql = match str_arg(args[1], "sql") {
		Ok(sql) => sql,
		Err(error) => return error,
	};
	let params = match sqlite_params_from_object(args.get(2).copied()) {
		Ok(params) => params,
		Err(error) => return error,
	};
	match sqlite_execute_core(conn, &sql, params) {
		Ok((rows, rowcount)) => alloc_sqlite_cursor(connection, rows, rowcount),
		Err(error) => sqlite_error(&error),
	}
}

unsafe extern "C" fn sqlite_connection_close_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "close") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error("close() takes no arguments");
	}
	let Some(connection) = (unsafe { sqlite_connection_receiver(args[0]) }) else {
		return type_error("close() receiver must be a sqlite3.Connection");
	};
	connection.conn.take();
	none()
}

unsafe extern "C" fn sqlite_connection_commit_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "commit") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error("commit() takes no arguments");
	}
	let Some(connection) = (unsafe { sqlite_connection_receiver(args[0]) }) else {
		return type_error("commit() receiver must be a sqlite3.Connection");
	};
	let Some(conn) = connection.conn.as_mut() else {
		return sqlite_error("Cannot operate on a closed database.");
	};
	match conn.execute_batch("COMMIT") {
		Ok(()) => none(),
		Err(_) => none(),
	}
}

unsafe extern "C" fn sqlite_connection_rollback_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "rollback") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error("rollback() takes no arguments");
	}
	let Some(connection) = (unsafe { sqlite_connection_receiver(args[0]) }) else {
		return type_error("rollback() receiver must be a sqlite3.Connection");
	};
	let Some(conn) = connection.conn.as_mut() else {
		return sqlite_error("Cannot operate on a closed database.");
	};
	match conn.execute_batch("ROLLBACK") {
		Ok(()) => none(),
		Err(_) => none(),
	}
}

unsafe extern "C" fn sqlite_context_enter_entry(
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

unsafe extern "C" fn sqlite_connection_exit_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "__exit__") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.is_empty() {
		return type_error("__exit__() missing receiver");
	}
	let mut close_args = [args[0]];
	unsafe { sqlite_connection_close_entry(close_args.as_mut_ptr(), close_args.len()) }
}

unsafe extern "C" fn sqlite_cursor_execute_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "execute") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() < 2 || args.len() > 3 {
		return type_error(&format!(
			"execute() expected sql and optional parameters, got {}",
			args.len().saturating_sub(1)
		));
	}
	let Some(cursor) = (unsafe { sqlite_cursor_receiver(args[0]) }) else {
		return type_error("execute() receiver must be a sqlite3.Cursor");
	};
	if cursor.connection.is_null() {
		return sqlite_error("Cannot operate on a cursor without connection.");
	}
	let connection = unsafe { &mut *cursor.connection };
	let Some(conn) = connection.conn.as_mut() else {
		return sqlite_error("Cannot operate on a closed database.");
	};
	let sql = match str_arg(args[1], "sql") {
		Ok(sql) => sql,
		Err(error) => return error,
	};
	let params = match sqlite_params_from_object(args.get(2).copied()) {
		Ok(params) => params,
		Err(error) => return error,
	};
	match sqlite_execute_core(conn, &sql, params) {
		Ok((rows, rowcount)) => {
			cursor.rows = rows;
			cursor.index = 0;
			cursor.rowcount = rowcount;
			args[0]
		},
		Err(error) => sqlite_error(&error),
	}
}

unsafe extern "C" fn sqlite_cursor_fetchone_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "fetchone") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error("fetchone() takes no arguments");
	}
	let Some(cursor) = (unsafe { sqlite_cursor_receiver(args[0]) }) else {
		return type_error("fetchone() receiver must be a sqlite3.Cursor");
	};
	if cursor.index >= cursor.rows.len() {
		return none();
	}
	let row = sqlite_row_to_tuple(&cursor.rows[cursor.index]);
	cursor.index += 1;
	row
}

unsafe extern "C" fn sqlite_cursor_fetchall_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "fetchall") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error("fetchall() takes no arguments");
	}
	let Some(cursor) = (unsafe { sqlite_cursor_receiver(args[0]) }) else {
		return type_error("fetchall() receiver must be a sqlite3.Cursor");
	};
	let mut out = Vec::new();
	while cursor.index < cursor.rows.len() {
		let row = sqlite_row_to_tuple(&cursor.rows[cursor.index]);
		if row.is_null() {
			return ptr::null_mut();
		}
		out.push(row);
		cursor.index += 1;
	}
	alloc_list(out)
}

unsafe extern "C" fn sqlite_cursor_close_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "close") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error("close() takes no arguments");
	}
	let Some(cursor) = (unsafe { sqlite_cursor_receiver(args[0]) }) else {
		return type_error("close() receiver must be a sqlite3.Cursor");
	};
	cursor.rows.clear();
	cursor.index = 0;
	none()
}

unsafe extern "C" fn sqlite_cursor_exit_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "__exit__") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.is_empty() {
		return type_error("__exit__() missing receiver");
	}
	let mut close_args = [args[0]];
	unsafe { sqlite_cursor_close_entry(close_args.as_mut_ptr(), close_args.len()) }
}

unsafe extern "C" fn sqlite_connection_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = match attr_name(name) {
		Ok(name) => name,
		Err(error) => return error,
	};
	match name {
		"cursor" => bound_method(object, "cursor", sqlite_connection_cursor_entry),
		"execute" => bound_method(object, "execute", sqlite_connection_execute_entry),
		"close" => bound_method(object, "close", sqlite_connection_close_entry),
		"commit" => bound_method(object, "commit", sqlite_connection_commit_entry),
		"rollback" => bound_method(object, "rollback", sqlite_connection_rollback_entry),
		"__enter__" => bound_method(object, "__enter__", sqlite_context_enter_entry),
		"__exit__" => bound_method(object, "__exit__", sqlite_connection_exit_entry),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name)) },
	}
}

unsafe extern "C" fn sqlite_cursor_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = match attr_name(name) {
		Ok(name) => name,
		Err(error) => return error,
	};
	let Some(cursor) = (unsafe { sqlite_cursor_receiver(object) }) else {
		return type_error("sqlite cursor attribute lookup on non-cursor");
	};
	match name {
		"execute" => bound_method(object, "execute", sqlite_cursor_execute_entry),
		"fetchone" => bound_method(object, "fetchone", sqlite_cursor_fetchone_entry),
		"fetchall" => bound_method(object, "fetchall", sqlite_cursor_fetchall_entry),
		"close" => bound_method(object, "close", sqlite_cursor_close_entry),
		"__enter__" => bound_method(object, "__enter__", sqlite_context_enter_entry),
		"__exit__" => bound_method(object, "__exit__", sqlite_cursor_exit_entry),
		"rowcount" => py_int(cursor.rowcount as i64),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name)) },
	}
}

unsafe extern "C" fn sqlite_cursor_iter(object: *mut PyObject) -> *mut PyObject {
	object
}

unsafe extern "C" fn sqlite_cursor_iternext(object: *mut PyObject) -> *mut PyObject {
	let Some(cursor) = (unsafe { sqlite_cursor_receiver(object) }) else {
		return type_error("sqlite cursor iteration on non-cursor");
	};
	if cursor.index >= cursor.rows.len() {
		return unsafe { abi::exc::pon_raise_stop_iteration(ptr::null_mut()) };
	}
	let row = sqlite_row_to_tuple(&cursor.rows[cursor.index]);
	cursor.index += 1;
	row
}

fn sqlite_attrs(name: &str) -> Result<Vec<(u32, *mut PyObject)>, String> {
	let error = *SQLITE_ERROR_CLASS;
	let warning = *SQLITE_WARNING_CLASS;
	let database_error = *SQLITE_DATABASE_ERROR_CLASS;
	let operational_error = *SQLITE_OPERATIONAL_ERROR_CLASS;
	let integrity_error = *SQLITE_INTEGRITY_ERROR_CLASS;
	let programming_error = *SQLITE_PROGRAMMING_ERROR_CLASS;
	let interface_error = *SQLITE_INTERFACE_ERROR_CLASS;
	let data_error = *SQLITE_DATA_ERROR_CLASS;
	let internal_error = *SQLITE_INTERNAL_ERROR_CLASS;
	let not_supported_error = *SQLITE_NOT_SUPPORTED_ERROR_CLASS;
	if [
		error,
		warning,
		database_error,
		operational_error,
		integrity_error,
		programming_error,
		interface_error,
		data_error,
		internal_error,
		not_supported_error,
	]
	.contains(&0)
	{
		return Err("failed to create sqlite3 exception classes".to_owned());
	}
	let tuple_type = unsafe { abi::pon_load_global(intern("tuple"), ptr::null_mut()) };
	if tuple_type.is_null() {
		pon_err_clear();
	}
	let adapters = unsafe { abi::map::pon_build_map(ptr::null_mut(), 0) };
	if adapters.is_null() {
		return Err("failed to allocate _sqlite3.adapters".to_owned());
	}
	let converters = unsafe { abi::map::pon_build_map(ptr::null_mut(), 0) };
	if converters.is_null() {
		return Err("failed to allocate _sqlite3.converters".to_owned());
	}
	let mut attrs = vec![
		str_attr("__name__", name)?,
		function_attr("connect", "connect", sqlite_connect_entry)?,
		function_attr("adapt", "adapt", sqlite_adapt_entry)?,
		function_attr("complete_statement", "complete_statement", sqlite_complete_statement_entry)?,
		function_attr(
			"enable_callback_tracebacks",
			"enable_callback_tracebacks",
			sqlite_enable_callback_tracebacks_entry,
		)?,
		function_attr("register_adapter", "register_adapter", sqlite_register_adapter_entry)?,
		function_attr("register_converter", "register_converter", sqlite_register_converter_entry)?,
		(intern("adapters"), adapters),
		(intern("converters"), converters),
		(intern("Connection"), sqlite_connection_type().cast::<PyObject>()),
		(intern("Cursor"), sqlite_cursor_type().cast::<PyObject>()),
		(
			intern("Row"),
			if tuple_type.is_null() {
				sqlite_cursor_type().cast::<PyObject>()
			} else {
				tuple_type
			},
		),
		(intern("PrepareProtocol"), sqlite_prepare_protocol_type().cast::<PyObject>()),
		(intern("Error"), error as *mut PyObject),
		(intern("Warning"), warning as *mut PyObject),
		(intern("DatabaseError"), database_error as *mut PyObject),
		(intern("OperationalError"), operational_error as *mut PyObject),
		(intern("IntegrityError"), integrity_error as *mut PyObject),
		(intern("ProgrammingError"), programming_error as *mut PyObject),
		(intern("InterfaceError"), interface_error as *mut PyObject),
		(intern("DataError"), data_error as *mut PyObject),
		(intern("InternalError"), internal_error as *mut PyObject),
		(intern("NotSupportedError"), not_supported_error as *mut PyObject),
		str_attr("sqlite_version", rusqlite::version())?,
		int_attr("threadsafety", 3)?,
		int_attr("PARSE_DECLTYPES", 1)?,
		int_attr("PARSE_COLNAMES", 2)?,
		int_attr("LEGACY_TRANSACTION_CONTROL", -1)?,
		int_attr("SQLITE_OK", i64::from(ffi::SQLITE_OK))?,
		int_attr("SQLITE_ERROR", i64::from(ffi::SQLITE_ERROR))?,
		int_attr("SQLITE_CONSTRAINT", i64::from(ffi::SQLITE_CONSTRAINT))?,
	];
	macro_rules! push_sqlite_constants {
        ($($name:ident),* $(,)?) => {
            $(attrs.push(int_attr(stringify!($name), i64::from(ffi::$name))?);)*
        };
    }
	push_sqlite_constants!(
		SQLITE_ABORT,
		SQLITE_ABORT_ROLLBACK,
		SQLITE_ALTER_TABLE,
		SQLITE_ANALYZE,
		SQLITE_ATTACH,
		SQLITE_AUTH,
		SQLITE_AUTH_USER,
		SQLITE_BUSY,
		SQLITE_BUSY_RECOVERY,
		SQLITE_BUSY_SNAPSHOT,
		SQLITE_BUSY_TIMEOUT,
		SQLITE_CANTOPEN,
		SQLITE_CANTOPEN_CONVPATH,
		SQLITE_CANTOPEN_DIRTYWAL,
		SQLITE_CANTOPEN_FULLPATH,
		SQLITE_CANTOPEN_ISDIR,
		SQLITE_CANTOPEN_NOTEMPDIR,
		SQLITE_CANTOPEN_SYMLINK,
		SQLITE_CONSTRAINT_CHECK,
		SQLITE_CONSTRAINT_COMMITHOOK,
		SQLITE_CONSTRAINT_FOREIGNKEY,
		SQLITE_CONSTRAINT_FUNCTION,
		SQLITE_CONSTRAINT_NOTNULL,
		SQLITE_CONSTRAINT_PINNED,
		SQLITE_CONSTRAINT_PRIMARYKEY,
		SQLITE_CONSTRAINT_ROWID,
		SQLITE_CONSTRAINT_TRIGGER,
		SQLITE_CONSTRAINT_UNIQUE,
		SQLITE_CONSTRAINT_VTAB,
		SQLITE_CORRUPT,
		SQLITE_CORRUPT_INDEX,
		SQLITE_CORRUPT_SEQUENCE,
		SQLITE_CORRUPT_VTAB,
		SQLITE_CREATE_INDEX,
		SQLITE_CREATE_TABLE,
		SQLITE_CREATE_TEMP_INDEX,
		SQLITE_CREATE_TEMP_TABLE,
		SQLITE_CREATE_TEMP_TRIGGER,
		SQLITE_CREATE_TEMP_VIEW,
		SQLITE_CREATE_TRIGGER,
		SQLITE_CREATE_VIEW,
		SQLITE_CREATE_VTABLE,
		SQLITE_DBCONFIG_DEFENSIVE,
		SQLITE_DBCONFIG_DQS_DDL,
		SQLITE_DBCONFIG_DQS_DML,
		SQLITE_DBCONFIG_ENABLE_FKEY,
		SQLITE_DBCONFIG_ENABLE_FTS3_TOKENIZER,
		SQLITE_DBCONFIG_ENABLE_LOAD_EXTENSION,
		SQLITE_DBCONFIG_ENABLE_QPSG,
		SQLITE_DBCONFIG_ENABLE_TRIGGER,
		SQLITE_DBCONFIG_ENABLE_VIEW,
		SQLITE_DBCONFIG_LEGACY_ALTER_TABLE,
		SQLITE_DBCONFIG_LEGACY_FILE_FORMAT,
		SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
		SQLITE_DBCONFIG_RESET_DATABASE,
		SQLITE_DBCONFIG_TRIGGER_EQP,
		SQLITE_DBCONFIG_TRUSTED_SCHEMA,
		SQLITE_DBCONFIG_WRITABLE_SCHEMA,
		SQLITE_DELETE,
		SQLITE_DENY,
		SQLITE_DETACH,
		SQLITE_DONE,
		SQLITE_DROP_INDEX,
		SQLITE_DROP_TABLE,
		SQLITE_DROP_TEMP_INDEX,
		SQLITE_DROP_TEMP_TABLE,
		SQLITE_DROP_TEMP_TRIGGER,
		SQLITE_DROP_TEMP_VIEW,
		SQLITE_DROP_TRIGGER,
		SQLITE_DROP_VIEW,
		SQLITE_DROP_VTABLE,
		SQLITE_EMPTY,
		SQLITE_ERROR_MISSING_COLLSEQ,
		SQLITE_ERROR_RETRY,
		SQLITE_ERROR_SNAPSHOT,
		SQLITE_FORMAT,
		SQLITE_FULL,
		SQLITE_FUNCTION,
		SQLITE_IGNORE,
		SQLITE_INSERT,
		SQLITE_INTERNAL,
		SQLITE_INTERRUPT,
		SQLITE_IOERR,
		SQLITE_IOERR_ACCESS,
		SQLITE_IOERR_AUTH,
		SQLITE_IOERR_BEGIN_ATOMIC,
		SQLITE_IOERR_BLOCKED,
		SQLITE_IOERR_CHECKRESERVEDLOCK,
		SQLITE_IOERR_CLOSE,
		SQLITE_IOERR_COMMIT_ATOMIC,
		SQLITE_IOERR_CONVPATH,
		SQLITE_IOERR_CORRUPTFS,
		SQLITE_IOERR_DATA,
		SQLITE_IOERR_DELETE,
		SQLITE_IOERR_DELETE_NOENT,
		SQLITE_IOERR_DIR_CLOSE,
		SQLITE_IOERR_DIR_FSYNC,
		SQLITE_IOERR_FSTAT,
		SQLITE_IOERR_FSYNC,
		SQLITE_IOERR_GETTEMPPATH,
		SQLITE_IOERR_LOCK,
		SQLITE_IOERR_MMAP,
		SQLITE_IOERR_NOMEM,
		SQLITE_IOERR_RDLOCK,
		SQLITE_IOERR_READ,
		SQLITE_IOERR_ROLLBACK_ATOMIC,
		SQLITE_IOERR_SEEK,
		SQLITE_IOERR_SHMLOCK,
		SQLITE_IOERR_SHMMAP,
		SQLITE_IOERR_SHMOPEN,
		SQLITE_IOERR_SHMSIZE,
		SQLITE_IOERR_SHORT_READ,
		SQLITE_IOERR_TRUNCATE,
		SQLITE_IOERR_UNLOCK,
		SQLITE_IOERR_VNODE,
		SQLITE_IOERR_WRITE,
		SQLITE_LIMIT_ATTACHED,
		SQLITE_LIMIT_COLUMN,
		SQLITE_LIMIT_COMPOUND_SELECT,
		SQLITE_LIMIT_EXPR_DEPTH,
		SQLITE_LIMIT_FUNCTION_ARG,
		SQLITE_LIMIT_LENGTH,
		SQLITE_LIMIT_LIKE_PATTERN_LENGTH,
		SQLITE_LIMIT_SQL_LENGTH,
		SQLITE_LIMIT_TRIGGER_DEPTH,
		SQLITE_LIMIT_VARIABLE_NUMBER,
		SQLITE_LIMIT_VDBE_OP,
		SQLITE_LIMIT_WORKER_THREADS,
		SQLITE_LOCKED,
		SQLITE_LOCKED_SHAREDCACHE,
		SQLITE_LOCKED_VTAB,
		SQLITE_MISMATCH,
		SQLITE_MISUSE,
		SQLITE_NOLFS,
		SQLITE_NOMEM,
		SQLITE_NOTADB,
		SQLITE_NOTFOUND,
		SQLITE_NOTICE,
		SQLITE_NOTICE_RECOVER_ROLLBACK,
		SQLITE_NOTICE_RECOVER_WAL,
		SQLITE_OK_LOAD_PERMANENTLY,
		SQLITE_OK_SYMLINK,
		SQLITE_PERM,
		SQLITE_PRAGMA,
		SQLITE_PROTOCOL,
		SQLITE_RANGE,
		SQLITE_READ,
		SQLITE_READONLY,
		SQLITE_READONLY_CANTINIT,
		SQLITE_READONLY_CANTLOCK,
		SQLITE_READONLY_DBMOVED,
		SQLITE_READONLY_DIRECTORY,
		SQLITE_READONLY_RECOVERY,
		SQLITE_READONLY_ROLLBACK,
		SQLITE_RECURSIVE,
		SQLITE_REINDEX,
		SQLITE_ROW,
		SQLITE_SAVEPOINT,
		SQLITE_SCHEMA,
		SQLITE_SELECT,
		SQLITE_TOOBIG,
		SQLITE_TRANSACTION,
		SQLITE_UPDATE,
		SQLITE_WARNING,
		SQLITE_WARNING_AUTOINDEX,
	);
	Ok(attrs)
}

pub(super) fn make_sqlite3_underscore_module() -> Result<*mut PyObject, String> {
	install_module("_sqlite3", sqlite_attrs("_sqlite3")?)
}

// ---------------------------------------------------------------------------
// `_dbm` / `dbm` — Darwin-only: ndbm ships in libSystem there, while glibc
// hosts need an external gdbm compat library that CI runners lack.

#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
#[repr(C)]
struct Datum {
	dptr:  *mut c_char,
	dsize: c_int,
}

#[cfg(target_os = "macos")]
enum RawDbm {}

#[cfg(target_os = "macos")]
unsafe extern "C" {
	fn dbm_open(file: *const c_char, flags: c_int, mode: libc::mode_t) -> *mut RawDbm;
	fn dbm_close(db: *mut RawDbm);
	fn dbm_fetch(db: *mut RawDbm, key: Datum) -> Datum;
	fn dbm_store(db: *mut RawDbm, key: Datum, content: Datum, flags: c_int) -> c_int;
	fn dbm_delete(db: *mut RawDbm, key: Datum) -> c_int;
	fn dbm_firstkey(db: *mut RawDbm) -> Datum;
	fn dbm_nextkey(db: *mut RawDbm) -> Datum;
	fn dbm_error(db: *mut RawDbm) -> c_int;
	fn dbm_clearerr(db: *mut RawDbm);
}

#[cfg(target_os = "macos")]
const DBM_REPLACE: c_int = 1;

#[cfg(target_os = "macos")]
#[repr(C)]
struct PyDbm {
	ob_base: PyObjectHeader,
	db:      *mut RawDbm,
}

#[cfg(target_os = "macos")]
static DBM_ERROR_CLASS: LazyLock<usize> =
	LazyLock::new(|| exception_class("dbm", "error", "OSError").map_or(0, |class| class as usize));

#[cfg(target_os = "macos")]
static DBM_MAPPING: PyMappingMethods = PyMappingMethods {
	mp_length:        Some(dbm_len_slot),
	mp_subscript:     Some(dbm_subscript_slot),
	mp_ass_subscript: Some(dbm_ass_subscript_slot),
};

#[cfg(target_os = "macos")]
static DBM_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty =
		PyType::new(abi::runtime_type_type().cast_const(), "dbm.dbm", core::mem::size_of::<PyDbm>());
	ty.tp_base = object_type();
	ty.tp_getattro = Some(dbm_getattro);
	ty.tp_as_mapping = ptr::addr_of!(DBM_MAPPING).cast_mut();
	Box::into_raw(Box::new(ty)) as usize
});

#[cfg(target_os = "macos")]
fn dbm_type() -> *mut PyType {
	*DBM_TYPE as *mut PyType
}

#[cfg(target_os = "macos")]
fn raise_dbm_error(message: &str) -> *mut PyObject {
	raise_class(&DBM_ERROR_CLASS, ExceptionKind::OSError, message)
}

#[cfg(target_os = "macos")]
unsafe fn dbm_receiver<'a>(object: *mut PyObject) -> Option<&'a mut PyDbm> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || unsafe { (*object).ob_type } != dbm_type().cast_const() {
		return None;
	}
	Some(unsafe { &mut *object.cast::<PyDbm>() })
}

#[cfg(target_os = "macos")]
fn dbm_key_or_value(object: *mut PyObject, name: &str) -> Result<Vec<u8>, *mut PyObject> {
	bytes_or_text_arg(object, name)
}

#[cfg(target_os = "macos")]
fn datum_from_slice(bytes: &[u8]) -> Datum {
	Datum {
		dptr:  bytes.as_ptr().cast::<c_char>().cast_mut(),
		dsize: bytes.len().try_into().unwrap_or(c_int::MAX),
	}
}

#[cfg(target_os = "macos")]
unsafe fn datum_to_vec(datum: Datum) -> Option<Vec<u8>> {
	if datum.dptr.is_null() || datum.dsize < 0 {
		return None;
	}
	let len = usize::try_from(datum.dsize).ok()?;
	Some(unsafe { core::slice::from_raw_parts(datum.dptr.cast::<u8>(), len) }.to_vec())
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn dbm_open_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "open") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.is_empty() || args.len() > 3 {
		return type_error(&format!(
			"open() expected file, flag='r', mode=0o666; got {} arguments",
			args.len()
		));
	}
	let filename = match bytes_or_text_arg(args[0], "file") {
		Ok(filename) => filename,
		Err(error) => return error,
	};
	let flag = match optional_str_arg(args, 1, "r", "flag") {
		Ok(flag) => flag,
		Err(error) => return error,
	};
	let mode = match optional_int_arg(args, 2, 0o666, "mode") {
		Ok(mode) => mode as libc::mode_t,
		Err(error) => return error,
	};
	let flags = match flag.as_str() {
		"r" => libc::O_RDONLY,
		"w" => libc::O_RDWR,
		"c" => libc::O_RDWR | libc::O_CREAT,
		"n" => libc::O_RDWR | libc::O_CREAT | libc::O_TRUNC,
		_ => return value_error("Flag must be one of 'r', 'w', 'c', or 'n'"),
	};
	let filename = match CString::new(filename) {
		Ok(filename) => filename,
		Err(_) => return value_error("embedded null byte in dbm filename"),
	};
	let db = unsafe { dbm_open(filename.as_ptr(), flags, mode) };
	if db.is_null() {
		return raise_dbm_error("dbm.open failed");
	}
	Box::into_raw(Box::new(PyDbm { ob_base: PyObjectHeader::new(dbm_type()), db }))
		.cast::<PyObject>()
}

#[cfg(target_os = "macos")]
unsafe fn dbm_live<'a>(object: *mut PyObject) -> Result<&'a mut PyDbm, *mut PyObject> {
	let Some(dbm) = (unsafe { dbm_receiver(object) }) else {
		return Err(type_error("dbm operation on non-dbm object"));
	};
	if dbm.db.is_null() {
		return Err(raise_dbm_error("DBM object has already been closed"));
	}
	Ok(dbm)
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn dbm_subscript_slot(
	object: *mut PyObject,
	key: *mut PyObject,
) -> *mut PyObject {
	let dbm = match unsafe { dbm_live(object) } {
		Ok(dbm) => dbm,
		Err(error) => return error,
	};
	let key = match dbm_key_or_value(key, "key") {
		Ok(key) => key,
		Err(error) => return error,
	};
	let datum = unsafe { dbm_fetch(dbm.db, datum_from_slice(&key)) };
	let Some(value) = (unsafe { datum_to_vec(datum) }) else {
		return key_error("dbm key not found");
	};
	py_bytes(&value)
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn dbm_ass_subscript_slot(
	object: *mut PyObject,
	key: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	let dbm = match unsafe { dbm_live(object) } {
		Ok(dbm) => dbm,
		Err(_) => return -1,
	};
	let key = match dbm_key_or_value(key, "key") {
		Ok(key) => key,
		Err(_) => return -1,
	};
	if value.is_null() {
		let rc = unsafe { dbm_delete(dbm.db, datum_from_slice(&key)) };
		if rc != 0 {
			let _ = key_error("dbm key not found");
			return -1;
		}
		return 0;
	}
	let value = match dbm_key_or_value(value, "value") {
		Ok(value) => value,
		Err(_) => return -1,
	};
	let rc =
		unsafe { dbm_store(dbm.db, datum_from_slice(&key), datum_from_slice(&value), DBM_REPLACE) };
	if rc != 0 {
		let _ = raise_dbm_error("dbm store failed");
		return -1;
	}
	0
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn dbm_len_slot(object: *mut PyObject) -> isize {
	let dbm = match unsafe { dbm_live(object) } {
		Ok(dbm) => dbm,
		Err(_) => return -1,
	};
	let mut count = 0isize;
	let mut key = unsafe { dbm_firstkey(dbm.db) };
	while unsafe { datum_to_vec(key) }.is_some() {
		count = count.saturating_add(1);
		key = unsafe { dbm_nextkey(dbm.db) };
	}
	count
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn dbm_close_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "close") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error("close() takes no arguments");
	}
	let Some(dbm) = (unsafe { dbm_receiver(args[0]) }) else {
		return type_error("close() receiver must be a dbm object");
	};
	if !dbm.db.is_null() {
		unsafe { dbm_close(dbm.db) };
		dbm.db = ptr::null_mut();
	}
	none()
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn dbm_keys_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "keys") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error("keys() takes no arguments");
	}
	let dbm = match unsafe { dbm_live(args[0]) } {
		Ok(dbm) => dbm,
		Err(error) => return error,
	};
	unsafe { dbm_clearerr(dbm.db) };
	let mut out = Vec::new();
	let mut key = unsafe { dbm_firstkey(dbm.db) };
	while let Some(bytes) = unsafe { datum_to_vec(key) } {
		let object = py_bytes(&bytes);
		if object.is_null() {
			return ptr::null_mut();
		}
		out.push(object);
		key = unsafe { dbm_nextkey(dbm.db) };
	}
	if unsafe { dbm_error(dbm.db) } != 0 {
		return raise_dbm_error("dbm key iteration failed");
	}
	alloc_list(out)
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn dbm_exit_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "__exit__") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.is_empty() {
		return type_error("__exit__() missing receiver");
	}
	let mut close_args = [args[0]];
	unsafe { dbm_close_entry(close_args.as_mut_ptr(), close_args.len()) }
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn dbm_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let name = match attr_name(name) {
		Ok(name) => name,
		Err(error) => return error,
	};
	match name {
		"close" => bound_method(object, "close", dbm_close_entry),
		"keys" => bound_method(object, "keys", dbm_keys_entry),
		"__enter__" => bound_method(object, "__enter__", sqlite_context_enter_entry),
		"__exit__" => bound_method(object, "__exit__", dbm_exit_entry),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name)) },
	}
}

#[cfg(target_os = "macos")]
fn dbm_attrs(name: &str) -> Result<Vec<(u32, *mut PyObject)>, String> {
	let error = *DBM_ERROR_CLASS;
	if error == 0 {
		return Err("failed to create dbm.error".to_owned());
	}
	Ok(vec![
		str_attr("__name__", name)?,
		function_attr("open", "open", dbm_open_entry)?,
		(intern("error"), error as *mut PyObject),
		str_attr("library", "ndbm")?,
		(intern("_Database"), dbm_type().cast::<PyObject>()),
	])
}

#[cfg(target_os = "macos")]
pub(super) fn make_dbm_underscore_module() -> Result<*mut PyObject, String> {
	install_module("_dbm", dbm_attrs("_dbm")?)
}

// ---------------------------------------------------------------------------
// `_ctypes` / `ctypes`

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CKind {
	Int,
	UInt,
	Long,
	ULong,
	LongLong,
	ULongLong,
	Short,
	UShort,
	Byte,
	UByte,
	Bool,
	SizeT,
	VoidP,
	Char,
	CharP,
	WChar,
	WCharP,
	PyObject,
	Float,
	Double,
	LongDouble,
	FloatComplex,
	DoubleComplex,
	LongDoubleComplex,
}

impl CKind {
	const fn size(self) -> usize {
		match self {
			Self::Byte | Self::UByte | Self::Bool | Self::Char => 1,
			Self::Short | Self::UShort => 2,
			Self::Int | Self::UInt | Self::Float => 4,
			Self::Long | Self::ULong => core::mem::size_of::<libc::c_long>(),
			Self::LongLong | Self::ULongLong | Self::Double | Self::LongDouble => 8,
			Self::FloatComplex => 8,
			Self::DoubleComplex | Self::LongDoubleComplex => 16,
			Self::WChar => core::mem::size_of::<libc::wchar_t>(),
			Self::SizeT | Self::VoidP | Self::CharP | Self::WCharP | Self::PyObject => {
				core::mem::size_of::<usize>()
			},
		}
	}

	const fn signed(self) -> bool {
		matches!(self, Self::Int | Self::Long | Self::LongLong | Self::Short | Self::Byte)
	}

	const fn public_name(self) -> &'static str {
		match self {
			Self::Int => "c_int",
			Self::UInt => "c_uint",
			Self::Long => "c_long",
			Self::ULong => "c_ulong",
			Self::LongLong => "c_longlong",
			Self::ULongLong => "c_ulonglong",
			Self::Short => "c_short",
			Self::UShort => "c_ushort",
			Self::Byte => "c_byte",
			Self::UByte => "c_ubyte",
			Self::Bool => "c_bool",
			Self::SizeT => "c_size_t",
			Self::VoidP => "c_void_p",
			Self::Char => "c_char",
			Self::CharP => "c_char_p",
			Self::WChar => "c_wchar",
			Self::WCharP => "c_wchar_p",
			Self::PyObject => "py_object",
			Self::Float => "c_float",
			Self::Double => "c_double",
			Self::LongDouble => "c_longdouble",
			Self::FloatComplex => "c_float_complex",
			Self::DoubleComplex => "c_double_complex",
			Self::LongDoubleComplex => "c_longdouble_complex",
		}
	}

	const fn typecode(self) -> &'static str {
		match self {
			Self::Int => "i",
			Self::UInt => "I",
			Self::Long => "l",
			Self::ULong => "L",
			Self::LongLong => "q",
			Self::ULongLong => "Q",
			Self::Short => "h",
			Self::UShort => "H",
			Self::Byte => "b",
			Self::UByte => "B",
			Self::Bool => "?",
			Self::SizeT => "P",
			Self::VoidP => "P",
			Self::Char => "c",
			Self::CharP => "z",
			Self::WChar => "u",
			Self::WCharP => "Z",
			Self::PyObject => "O",
			Self::Float => "f",
			Self::Double => "d",
			Self::LongDouble => "g",
			Self::FloatComplex => "F",
			Self::DoubleComplex => "D",
			Self::LongDoubleComplex => "G",
		}
	}
}

#[repr(C)]
struct PyCData {
	ob_base: PyObjectHeader,
	kind:    CKind,
	value:   i128,
	bytes:   Option<Vec<u8>>,
}

#[repr(C)]
struct PyPointer {
	ob_base: PyObjectHeader,
	target:  *mut PyObject,
	address: usize,
}

#[repr(C)]
struct PyCDll {
	ob_base: PyObjectHeader,
	name:    Option<String>,
	handle:  *mut c_void,
}

#[repr(C)]
struct PyCFunc {
	ob_base:        PyObjectHeader,
	name:           String,
	address:        *mut c_void,
	restype:        Option<CKind>,
	restype_object: *mut PyObject,
	argtypes:       *mut PyObject,
}

fn namespace_for_ctypes_type(kind: Option<CKind>, include_from_param: bool) -> *mut PyObject {
	let namespace = type_::new_namespace();
	if namespace.is_null() {
		return ptr::null_mut();
	}
	unsafe {
		if let Some(kind) = kind {
			(*namespace).set(intern("_type_"), py_str(kind.typecode()));
		}
		if include_from_param {
			let function = abi::pon_make_function(
				ctypes_from_param_entry as *const u8,
				VARIADIC_ARITY,
				intern("from_param"),
			);
			if !function.is_null() {
				(*namespace).set(intern("from_param"), function);
			}
		}
	}
	namespace.cast::<PyObject>()
}

fn finish_ctypes_type(mut ty: PyType, namespace: *mut PyObject) -> usize {
	if !namespace.is_null() {
		ty.tp_dict = namespace;
	}
	let raw = Box::into_raw(Box::new(ty));
	if !namespace.is_null() {
		crate::sync::register_namespaced_type(raw);
		crate::sync::type_modified(raw);
	}
	raw as usize
}

static SIMPLE_CDATA_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"ctypes._SimpleCData",
		core::mem::size_of::<PyCData>(),
	);
	ty.tp_base = object_type();
	ty.tp_new = Some(cdata_new);
	ty.tp_getattro = Some(cdata_getattro);
	ty.tp_setattro = Some(cdata_setattro);
	ty.tp_repr = Some(cdata_repr);
	finish_ctypes_type(ty, namespace_for_ctypes_type(None, true))
});

fn simple_cdata_type() -> *mut PyType {
	*SIMPLE_CDATA_TYPE as *mut PyType
}

fn make_c_type(kind: CKind) -> usize {
	let typename = match kind {
		CKind::Int => "ctypes.c_int",
		CKind::UInt => "ctypes.c_uint",
		CKind::Long => "ctypes.c_long",
		CKind::ULong => "ctypes.c_ulong",
		CKind::LongLong => "ctypes.c_longlong",
		CKind::ULongLong => "ctypes.c_ulonglong",
		CKind::Short => "ctypes.c_short",
		CKind::UShort => "ctypes.c_ushort",
		CKind::Byte => "ctypes.c_byte",
		CKind::UByte => "ctypes.c_ubyte",
		CKind::Bool => "ctypes.c_bool",
		CKind::SizeT => "ctypes.c_size_t",
		CKind::VoidP => "ctypes.c_void_p",
		CKind::Char => "ctypes.c_char",
		CKind::CharP => "ctypes.c_char_p",
		CKind::WChar => "ctypes.c_wchar",
		CKind::WCharP => "ctypes.c_wchar_p",
		CKind::PyObject => "ctypes.py_object",
		CKind::Float => "ctypes.c_float",
		CKind::Double => "ctypes.c_double",
		CKind::LongDouble => "ctypes.c_longdouble",
		CKind::FloatComplex => "ctypes.c_float_complex",
		CKind::DoubleComplex => "ctypes.c_double_complex",
		CKind::LongDoubleComplex => "ctypes.c_longdouble_complex",
	};
	let mut ty =
		PyType::new(abi::runtime_type_type().cast_const(), typename, core::mem::size_of::<PyCData>());
	ty.tp_base = simple_cdata_type();
	ty.tp_new = Some(cdata_new);
	ty.tp_getattro = Some(cdata_getattro);
	ty.tp_setattro = Some(cdata_setattro);
	ty.tp_repr = Some(cdata_repr);
	finish_ctypes_type(ty, namespace_for_ctypes_type(Some(kind), true))
}

static C_INT_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::Int));
static C_UINT_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::UInt));
static C_LONG_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::Long));
static C_ULONG_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::ULong));
static C_LONGLONG_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::LongLong));
static C_ULONGLONG_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::ULongLong));
static C_SHORT_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::Short));
static C_USHORT_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::UShort));
static C_BYTE_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::Byte));
static C_UBYTE_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::UByte));
static C_BOOL_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::Bool));
static C_SIZET_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::SizeT));
static C_VOIDP_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::VoidP));
static C_CHAR_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::Char));
static C_CHARP_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::CharP));
static C_WCHAR_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::WChar));
static C_WCHARP_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::WCharP));
static C_PYOBJECT_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::PyObject));
static C_FLOAT_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::Float));
static C_DOUBLE_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::Double));
static C_LONGDOUBLE_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::LongDouble));
static C_FLOAT_COMPLEX_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::FloatComplex));
static C_DOUBLE_COMPLEX_TYPE: LazyLock<usize> = LazyLock::new(|| make_c_type(CKind::DoubleComplex));
static C_LONGDOUBLE_COMPLEX_TYPE: LazyLock<usize> =
	LazyLock::new(|| make_c_type(CKind::LongDoubleComplex));

fn make_plain_ctypes_type(name: &'static str, size: usize) -> usize {
	let mut ty = PyType::new(abi::runtime_type_type().cast_const(), name, size);
	ty.tp_base = object_type();
	finish_ctypes_type(ty, ptr::null_mut())
}

static ARRAY_TYPE: LazyLock<usize> =
	LazyLock::new(|| make_plain_ctypes_type("ctypes.Array", core::mem::size_of::<PyObjectHeader>()));
static STRUCTURE_TYPE: LazyLock<usize> = LazyLock::new(|| {
	make_plain_ctypes_type("ctypes.Structure", core::mem::size_of::<PyObjectHeader>())
});
static UNION_TYPE: LazyLock<usize> =
	LazyLock::new(|| make_plain_ctypes_type("ctypes.Union", core::mem::size_of::<PyObjectHeader>()));
static CFIELD_TYPE: LazyLock<usize> = LazyLock::new(|| {
	make_plain_ctypes_type("ctypes.CField", core::mem::size_of::<PyObjectHeader>())
});

static POINTER_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"ctypes._Pointer",
		core::mem::size_of::<PyPointer>(),
	);
	ty.tp_base = object_type();
	ty.tp_new = Some(pointer_new);
	ty.tp_getattro = Some(pointer_getattro);
	finish_ctypes_type(ty, ptr::null_mut())
});

static CDLL_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"ctypes.CDLL",
		core::mem::size_of::<PyCDll>(),
	);
	ty.tp_base = object_type();
	ty.tp_new = Some(cdll_new);
	ty.tp_getattro = Some(cdll_getattro);
	finish_ctypes_type(ty, ptr::null_mut())
});

static CFUNC_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"ctypes._CFuncPtr",
		core::mem::size_of::<PyCFunc>(),
	);
	ty.tp_base = object_type();
	ty.tp_new = Some(cfunc_new);
	ty.tp_call = Some(cfunc_call);
	ty.tp_getattro = Some(cfunc_getattro);
	ty.tp_setattro = Some(cfunc_setattro);
	finish_ctypes_type(ty, ptr::null_mut())
});

fn pointer_type() -> *mut PyType {
	*POINTER_TYPE as *mut PyType
}

fn cdll_type() -> *mut PyType {
	*CDLL_TYPE as *mut PyType
}

fn cfunc_type() -> *mut PyType {
	*CFUNC_TYPE as *mut PyType
}

fn array_type() -> *mut PyType {
	*ARRAY_TYPE as *mut PyType
}

fn structure_type() -> *mut PyType {
	*STRUCTURE_TYPE as *mut PyType
}

fn union_type() -> *mut PyType {
	*UNION_TYPE as *mut PyType
}

fn cfield_type() -> *mut PyType {
	*CFIELD_TYPE as *mut PyType
}

fn c_type_for_kind(kind: CKind) -> *mut PyType {
	match kind {
		CKind::Int => *C_INT_TYPE as *mut PyType,
		CKind::UInt => *C_UINT_TYPE as *mut PyType,
		CKind::Long => *C_LONG_TYPE as *mut PyType,
		CKind::ULong => *C_ULONG_TYPE as *mut PyType,
		CKind::LongLong => *C_LONGLONG_TYPE as *mut PyType,
		CKind::ULongLong => *C_ULONGLONG_TYPE as *mut PyType,
		CKind::Short => *C_SHORT_TYPE as *mut PyType,
		CKind::UShort => *C_USHORT_TYPE as *mut PyType,
		CKind::Byte => *C_BYTE_TYPE as *mut PyType,
		CKind::UByte => *C_UBYTE_TYPE as *mut PyType,
		CKind::Bool => *C_BOOL_TYPE as *mut PyType,
		CKind::SizeT => *C_SIZET_TYPE as *mut PyType,
		CKind::VoidP => *C_VOIDP_TYPE as *mut PyType,
		CKind::Char => *C_CHAR_TYPE as *mut PyType,
		CKind::CharP => *C_CHARP_TYPE as *mut PyType,
		CKind::WChar => *C_WCHAR_TYPE as *mut PyType,
		CKind::WCharP => *C_WCHARP_TYPE as *mut PyType,
		CKind::PyObject => *C_PYOBJECT_TYPE as *mut PyType,
		CKind::Float => *C_FLOAT_TYPE as *mut PyType,
		CKind::Double => *C_DOUBLE_TYPE as *mut PyType,
		CKind::LongDouble => *C_LONGDOUBLE_TYPE as *mut PyType,
		CKind::FloatComplex => *C_FLOAT_COMPLEX_TYPE as *mut PyType,
		CKind::DoubleComplex => *C_DOUBLE_COMPLEX_TYPE as *mut PyType,
		CKind::LongDoubleComplex => *C_LONGDOUBLE_COMPLEX_TYPE as *mut PyType,
	}
}

fn c_kind_from_typecode(typecode: &str) -> Option<CKind> {
	match typecode {
		"i" => Some(CKind::Int),
		"I" => Some(CKind::UInt),
		"l" => Some(CKind::Long),
		"L" => Some(CKind::ULong),
		"q" => Some(CKind::LongLong),
		"Q" => Some(CKind::ULongLong),
		"h" => Some(CKind::Short),
		"H" => Some(CKind::UShort),
		"b" => Some(CKind::Byte),
		"B" => Some(CKind::UByte),
		"?" => Some(CKind::Bool),
		"P" => Some(CKind::VoidP),
		"c" => Some(CKind::Char),
		"z" => Some(CKind::CharP),
		"u" => Some(CKind::WChar),
		"Z" => Some(CKind::WCharP),
		"O" => Some(CKind::PyObject),
		"f" => Some(CKind::Float),
		"d" => Some(CKind::Double),
		"g" => Some(CKind::LongDouble),
		"F" => Some(CKind::FloatComplex),
		"D" => Some(CKind::DoubleComplex),
		"G" => Some(CKind::LongDoubleComplex),
		_ => None,
	}
}

fn c_kind_from_type(ty: *const PyType) -> Option<CKind> {
	if ty.is_null() {
		return None;
	}
	let ty_addr = ty as usize;
	let exact = [
		(*C_INT_TYPE, CKind::Int),
		(*C_UINT_TYPE, CKind::UInt),
		(*C_LONG_TYPE, CKind::Long),
		(*C_ULONG_TYPE, CKind::ULong),
		(*C_LONGLONG_TYPE, CKind::LongLong),
		(*C_ULONGLONG_TYPE, CKind::ULongLong),
		(*C_SHORT_TYPE, CKind::Short),
		(*C_USHORT_TYPE, CKind::UShort),
		(*C_BYTE_TYPE, CKind::Byte),
		(*C_UBYTE_TYPE, CKind::UByte),
		(*C_BOOL_TYPE, CKind::Bool),
		(*C_SIZET_TYPE, CKind::SizeT),
		(*C_VOIDP_TYPE, CKind::VoidP),
		(*C_CHAR_TYPE, CKind::Char),
		(*C_CHARP_TYPE, CKind::CharP),
		(*C_WCHAR_TYPE, CKind::WChar),
		(*C_WCHARP_TYPE, CKind::WCharP),
		(*C_PYOBJECT_TYPE, CKind::PyObject),
		(*C_FLOAT_TYPE, CKind::Float),
		(*C_DOUBLE_TYPE, CKind::Double),
		(*C_LONGDOUBLE_TYPE, CKind::LongDouble),
		(*C_FLOAT_COMPLEX_TYPE, CKind::FloatComplex),
		(*C_DOUBLE_COMPLEX_TYPE, CKind::DoubleComplex),
		(*C_LONGDOUBLE_COMPLEX_TYPE, CKind::LongDoubleComplex),
	]
	.into_iter()
	.find_map(|(slot, kind)| (ty_addr == slot).then_some(kind));
	if exact.is_some() {
		return exact;
	}
	let type_object = ty.cast_mut().cast::<PyObject>();
	let typecode = optional_attr(type_object, "_type_")?;
	let text = unsafe { type_::unicode_text(crate::tag::untag_arg(typecode)) }?;
	c_kind_from_typecode(text)
}

fn c_kind_from_type_object(object: *mut PyObject) -> Option<CKind> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || unsafe { (*object).ob_type } != abi::runtime_type_type().cast_const() {
		return None;
	}
	c_kind_from_type(object.cast::<PyType>().cast_const())
}

unsafe fn type_has_base(mut ty: *const PyType, base: *const PyType) -> bool {
	while !ty.is_null() {
		if ty == base {
			return true;
		}
		ty = unsafe { (*ty).tp_base.cast_const() };
	}
	false
}

unsafe fn cdata_receiver<'a>(object: *mut PyObject) -> Option<&'a mut PyCData> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || c_kind_from_type(unsafe { (*object).ob_type }).is_none() {
		return None;
	}
	Some(unsafe { &mut *object.cast::<PyCData>() })
}

unsafe fn pointer_receiver<'a>(object: *mut PyObject) -> Option<&'a mut PyPointer> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || !unsafe { type_has_base((*object).ob_type, pointer_type().cast_const()) }
	{
		return None;
	}
	Some(unsafe { &mut *object.cast::<PyPointer>() })
}

fn number_to_f64_arg(object: *mut PyObject, name: &str) -> Result<f64, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if let Some(value) = unsafe { crate::types::float::to_f64(object) } {
		return Ok(value);
	}
	unsafe { crate::types::int::to_bigint_including_bool(object) }
		.and_then(|value| value.to_f64())
		.ok_or_else(|| type_error(&format!("{name} must be a real number")))
}

fn one_char_codepoint(object: *mut PyObject, name: &str) -> Result<u32, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if let Some(text) = unsafe { type_::unicode_text(object) } {
		let mut chars = text.chars();
		let Some(ch) = chars.next() else {
			return Err(value_error(&format!("{name} must not be empty")));
		};
		if chars.next().is_some() {
			return Err(value_error(&format!("{name} must be a single character")));
		}
		return Ok(u32::from(ch));
	}
	u32::try_from(int_arg(object, name)?)
		.map_err(|_| value_error(&format!("{name} is out of range")))
}

fn cdata_value_from_py(
	kind: CKind,
	object: *mut PyObject,
) -> Result<(i128, Option<Vec<u8>>), *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || is_none(object) {
		return Ok((0, None));
	}
	match kind {
		CKind::Char => {
			let bytes = bytes_or_text_arg(object, "value")?;
			if bytes.len() != 1 {
				return Err(value_error("c_char value must be a single byte"));
			}
			Ok((i128::from(bytes[0]), Some(bytes)))
		},
		CKind::CharP => {
			let mut bytes = bytes_or_text_arg(object, "value")?;
			bytes.push(0);
			Ok((bytes.as_ptr() as usize as i128, Some(bytes)))
		},
		CKind::WChar => Ok((i128::from(one_char_codepoint(object, "value")?), None)),
		CKind::WCharP => {
			let mut bytes = str_arg(object, "value")?.into_bytes();
			bytes.push(0);
			Ok((bytes.as_ptr() as usize as i128, Some(bytes)))
		},
		CKind::VoidP => {
			if let Some(ptr) = unsafe { pointer_receiver(object) } {
				return Ok((ptr.address as i128, None));
			}
			Ok((int_arg(object, "value")? as i128, None))
		},
		CKind::PyObject => Ok((object as usize as i128, None)),
		CKind::Bool => Ok((i128::from(bool_arg(object).unwrap_or(false)), None)),
		CKind::Float => {
			let value = number_to_f64_arg(object, "value")? as f32;
			Ok((i128::from(value.to_bits()), None))
		},
		CKind::Double | CKind::LongDouble => {
			Ok((number_to_f64_arg(object, "value")?.to_bits() as i128, None))
		},
		CKind::FloatComplex | CKind::DoubleComplex | CKind::LongDoubleComplex => {
			Err(type_error("complex ctypes scalars are not implemented"))
		},
		_ => Ok((int_arg(object, "value")? as i128, None)),
	}
}

fn cdata_new_for_type(kind: CKind, ty: *mut PyType, value: Option<*mut PyObject>) -> *mut PyObject {
	let (value, bytes) = match value {
		Some(value) => match cdata_value_from_py(kind, value) {
			Ok(parsed) => parsed,
			Err(error) => return error,
		},
		None => (0, None),
	};
	Box::into_raw(Box::new(PyCData { ob_base: PyObjectHeader::new(ty), kind, value, bytes }))
		.cast::<PyObject>()
}

unsafe extern "C" fn cdata_new(
	cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	if !kwargs.is_null() {
		return type_error("ctypes scalar constructors take no keyword arguments");
	}
	let Some(kind) = c_kind_from_type(cls.cast_const()) else {
		return type_error("unknown ctypes scalar type");
	};
	let positional = match unsafe { type_::positional_args_from_object(args) } {
		Ok(positional) => positional,
		Err(message) => return type_error(&message),
	};
	if positional.len() > 1 {
		return type_error(&format!(
			"{}() expected at most 1 argument, got {}",
			kind.public_name(),
			positional.len()
		));
	}
	cdata_new_for_type(kind, cls, positional.first().copied())
}

unsafe extern "C" fn ctypes_from_param_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "from_param") {
		Ok(args) => args,
		Err(error) => return error,
	};
	match args {
		[value] => *value,
		[_cls, value] => *value,
		_ => type_error(&format!(
			"from_param() expected 1 argument, got {}",
			args.len().saturating_sub(1)
		)),
	}
}

fn cdata_float_value(cdata: &PyCData) -> f64 {
	match cdata.kind {
		CKind::Float => f32::from_bits(cdata.value as u32) as f64,
		CKind::Double | CKind::LongDouble => f64::from_bits(cdata.value as u64),
		_ => cdata.value as f64,
	}
}

unsafe extern "C" fn cdata_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let name = match attr_name(name) {
		Ok(name) => name,
		Err(error) => return error,
	};
	let Some(cdata) = (unsafe { cdata_receiver(object) }) else {
		return type_error("ctypes scalar attribute lookup on non-CData");
	};
	match name {
		"value" => match cdata.kind {
			CKind::Char => match cdata.bytes.as_ref() {
				Some(bytes) => py_bytes(bytes),
				None => py_bytes(&[cdata.value as u8]),
			},
			CKind::CharP => match cdata.bytes.as_ref() {
				Some(bytes) if !bytes.is_empty() => py_bytes(&bytes[..bytes.len().saturating_sub(1)]),
				_ => none(),
			},
			CKind::WChar => {
				char::from_u32(cdata.value as u32).map_or_else(none, |ch| py_str(&ch.to_string()))
			},
			CKind::WCharP => match cdata.bytes.as_ref() {
				Some(bytes) if !bytes.is_empty() => {
					let text = String::from_utf8_lossy(&bytes[..bytes.len().saturating_sub(1)]);
					py_str(&text)
				},
				_ => none(),
			},
			CKind::VoidP => {
				if cdata.value == 0 {
					none()
				} else {
					py_int(cdata.value as i64)
				}
			},
			CKind::PyObject => {
				if cdata.value == 0 {
					none()
				} else {
					cdata.value as usize as *mut PyObject
				}
			},
			CKind::Bool => py_bool(cdata.value != 0),
			CKind::Float | CKind::Double | CKind::LongDouble => py_float(cdata_float_value(cdata)),
			_ => py_int(cdata.value as i64),
		},
		"_objects" => none(),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name)) },
	}
}

unsafe extern "C" fn cdata_setattro(
	object: *mut PyObject,
	name: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	let name = match attr_name(name) {
		Ok(name) => name,
		Err(_) => return -1,
	};
	if value.is_null() {
		let _ = type_error("ctypes scalar attributes cannot be deleted");
		return -1;
	}
	let Some(cdata) = (unsafe { cdata_receiver(object) }) else {
		let _ = type_error("ctypes scalar attribute assignment on non-CData");
		return -1;
	};
	match name {
		"value" => match cdata_value_from_py(cdata.kind, value) {
			Ok((new_value, bytes)) => {
				cdata.value = new_value;
				cdata.bytes = bytes;
				0
			},
			Err(_) => -1,
		},
		_ => {
			let _ = unsafe { abi::exc::pon_raise_attribute_error(object, intern(name)) };
			-1
		},
	}
}

unsafe extern "C" fn cdata_repr(object: *mut PyObject) -> *mut PyObject {
	let Some(cdata) = (unsafe { cdata_receiver(object) }) else {
		return type_error("repr on non-CData");
	};
	py_str(&format!("{}({})", cdata.kind.public_name(), match cdata.kind {
		CKind::Float | CKind::Double | CKind::LongDouble => cdata_float_value(cdata).to_string(),
		_ => cdata.value.to_string(),
	}))
}

fn pointer_to_object(target: *mut PyObject) -> *mut PyObject {
	let address = unsafe { cdata_receiver(target) }
		.map(|cdata| cdata as *mut PyCData as usize)
		.unwrap_or(target as usize);
	Box::into_raw(Box::new(PyPointer {
		ob_base: PyObjectHeader::new(pointer_type()),
		target,
		address,
	}))
	.cast::<PyObject>()
}

unsafe extern "C" fn pointer_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	if !kwargs.is_null() {
		return type_error("pointer constructors take no keyword arguments");
	}
	let positional = match unsafe { type_::positional_args_from_object(args) } {
		Ok(positional) => positional,
		Err(message) => return type_error(&message),
	};
	match positional.as_slice() {
		[] => Box::into_raw(Box::new(PyPointer {
			ob_base: PyObjectHeader::new(pointer_type()),
			target:  ptr::null_mut(),
			address: 0,
		}))
		.cast::<PyObject>(),
		[target] => pointer_to_object(*target),
		_ => type_error(&format!("_Pointer() expected at most 1 argument, got {}", positional.len())),
	}
}

unsafe extern "C" fn pointer_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let name = match attr_name(name) {
		Ok(name) => name,
		Err(error) => return error,
	};
	let Some(pointer) = (unsafe { pointer_receiver(object) }) else {
		return type_error("pointer attribute lookup on non-pointer");
	};
	match name {
		"contents" => {
			if pointer.target.is_null() {
				value_error("NULL pointer access")
			} else {
				pointer.target
			}
		},
		"value" => {
			if pointer.address == 0 {
				none()
			} else {
				py_int(pointer.address as i64)
			}
		},
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name)) },
	}
}

unsafe extern "C" fn ctypes_pointer_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "pointer") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error(&format!("pointer() expected 1 argument, got {}", args.len()));
	}
	if unsafe { cdata_receiver(args[0]) }.is_none() {
		return type_error("_type_ must have storage info");
	}
	pointer_to_object(args[0])
}

unsafe extern "C" fn ctypes_byref_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { ctypes_pointer_entry(argv, argc) }
}

unsafe extern "C" fn ctypes_pointer_type_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "POINTER") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error(&format!("POINTER() expected 1 argument, got {}", args.len()));
	}
	pointer_type().cast::<PyObject>()
}

unsafe extern "C" fn ctypes_sizeof_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "sizeof") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error(&format!("sizeof() expected 1 argument, got {}", args.len()));
	}
	let object = crate::tag::untag_arg(args[0]);
	if let Some(kind) = c_kind_from_type_object(object) {
		return py_int(kind.size() as i64);
	}
	if let Some(cdata) = unsafe { cdata_receiver(object) } {
		return py_int(cdata.kind.size() as i64);
	}
	if unsafe { pointer_receiver(object) }.is_some() || object == pointer_type().cast::<PyObject>() {
		return py_int(core::mem::size_of::<usize>() as i64);
	}
	type_error("this type has no size")
}

unsafe extern "C" fn ctypes_alignment_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	unsafe { ctypes_sizeof_entry(argv, argc) }
}

unsafe extern "C" fn ctypes_addressof_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "addressof") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error(&format!("addressof() expected 1 argument, got {}", args.len()));
	}
	let object = crate::tag::untag_arg(args[0]);
	if unsafe { cdata_receiver(object) }.is_some() || unsafe { pointer_receiver(object) }.is_some() {
		return py_int(object as usize as i64);
	}
	type_error("invalid type")
}

unsafe extern "C" fn ctypes_get_errno_entry(
	_argv: *mut *mut PyObject,
	_argc: usize,
) -> *mut PyObject {
	py_int(0)
}

unsafe extern "C" fn ctypes_set_errno_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "set_errno") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.len() != 1 {
		return type_error(&format!("set_errno() expected 1 argument, got {}", args.len()));
	}
	let value = match int_arg(args[0], "errno") {
		Ok(value) => value,
		Err(error) => return error,
	};
	py_int(value)
}

fn cdll_name_arg(object: *mut PyObject) -> Result<Option<String>, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || is_none(object) {
		return Ok(None);
	}
	str_arg(object, "name").map(Some)
}

unsafe extern "C" fn cdll_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	if !kwargs.is_null() {
		return type_error("CDLL() keyword arguments are not supported in this runtime");
	}
	let positional = match unsafe { type_::positional_args_from_object(args) } {
		Ok(positional) => positional,
		Err(message) => return type_error(&message),
	};
	if positional.len() > 3 {
		return type_error(&format!("CDLL() expected at most 3 arguments, got {}", positional.len()));
	}
	let name = match positional.first().copied() {
		Some(name) => match cdll_name_arg(name) {
			Ok(name) => name,
			Err(error) => return error,
		},
		None => None,
	};
	let mode = match optional_int_arg(&positional, 1, libc::RTLD_NOW as i64, "mode") {
		Ok(mode) => mode as c_int,
		Err(error) => return error,
	};
	let handle = if let Some(handle) = positional
		.get(2)
		.copied()
		.filter(|object| !is_none(*object))
	{
		match int_arg(handle, "handle") {
			Ok(handle) => handle as usize as *mut c_void,
			Err(error) => return error,
		}
	} else {
		let c_name = match name.as_ref() {
			Some(name) => match CString::new(name.as_bytes()) {
				Ok(name) => Some(name),
				Err(_) => return value_error("embedded null byte in library name"),
			},
			None => None,
		};
		let raw_name = c_name.as_ref().map_or(ptr::null(), |name| name.as_ptr());
		unsafe { libc::dlopen(raw_name, mode) }
	};
	if handle.is_null() {
		let detail = unsafe {
			let error = libc::dlerror();
			if error.is_null() {
				"dlopen failed".to_owned()
			} else {
				CStr::from_ptr(error).to_string_lossy().into_owned()
			}
		};
		return os_error(&detail);
	}
	Box::into_raw(Box::new(PyCDll { ob_base: PyObjectHeader::new(cdll_type()), name, handle }))
		.cast::<PyObject>()
}

unsafe fn cdll_receiver<'a>(object: *mut PyObject) -> Option<&'a mut PyCDll> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || unsafe { (*object).ob_type } != cdll_type().cast_const() {
		return None;
	}
	Some(unsafe { &mut *object.cast::<PyCDll>() })
}

unsafe extern "C" fn cdll_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let name_text = match attr_name(name) {
		Ok(name) => name,
		Err(error) => return error,
	};
	let Some(cdll) = (unsafe { cdll_receiver(object) }) else {
		return type_error("CDLL attribute lookup on non-CDLL");
	};
	match name_text {
		"_name" => cdll.name.as_deref().map_or_else(none, py_str),
		"_handle" => py_int(cdll.handle as usize as i64),
		_ => {
			let symbol = match CString::new(name_text) {
				Ok(symbol) => symbol,
				Err(_) => return value_error("embedded null byte in symbol name"),
			};
			let address = unsafe { libc::dlsym(cdll.handle, symbol.as_ptr()) };
			if address.is_null() {
				return unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) };
			}
			Box::into_raw(Box::new(PyCFunc {
				ob_base: PyObjectHeader::new(cfunc_type()),
				name: name_text.to_owned(),
				address,
				restype: Some(CKind::Int),
				restype_object: c_type_for_kind(CKind::Int).cast::<PyObject>(),
				argtypes: none(),
			}))
			.cast::<PyObject>()
		},
	}
}

fn cfunc_restype_from_object(
	object: *mut PyObject,
) -> Result<(Option<CKind>, *mut PyObject), *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || is_none(object) {
		return Ok((None, none()));
	}
	let Some(kind) = c_kind_from_type_object(object) else {
		return Err(type_error("restype must be a ctypes type or None"));
	};
	Ok((Some(kind), object))
}

fn cfunc_defaults_from_class(
	cls: *mut PyType,
) -> Result<(Option<CKind>, *mut PyObject, *mut PyObject), *mut PyObject> {
	let cls_object = cls.cast::<PyObject>();
	let restype_object = optional_attr(cls_object, "_restype_")
		.unwrap_or_else(|| c_type_for_kind(CKind::Int).cast::<PyObject>());
	let (restype, restype_object) = cfunc_restype_from_object(restype_object)?;
	let argtypes = optional_attr(cls_object, "_argtypes_").unwrap_or_else(none);
	Ok((restype, restype_object, argtypes))
}

fn cfunc_from_address(
	cls: *mut PyType,
	name: String,
	address: *mut c_void,
	restype: Option<CKind>,
	restype_object: *mut PyObject,
	argtypes: *mut PyObject,
) -> *mut PyObject {
	Box::into_raw(Box::new(PyCFunc {
		ob_base: PyObjectHeader::new(cls),
		name,
		address,
		restype,
		restype_object,
		argtypes,
	}))
	.cast::<PyObject>()
}

fn handle_from_library_object(object: *mut PyObject) -> Result<*mut c_void, *mut PyObject> {
	if let Some(cdll) = unsafe { cdll_receiver(object) } {
		return Ok(cdll.handle);
	}
	let Some(handle) = optional_attr(object, "_handle") else {
		return Err(type_error("library object has no _handle"));
	};
	let handle = int_arg(handle, "_handle")?;
	Ok(handle as usize as *mut c_void)
}

unsafe extern "C" fn cfunc_new(
	cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	if !kwargs.is_null() {
		return type_error("CFuncPtr constructors take no keyword arguments");
	}
	let positional = match unsafe { type_::positional_args_from_object(args) } {
		Ok(positional) => positional,
		Err(message) => return type_error(&message),
	};
	if positional.len() != 1 {
		return type_error(&format!(
			"CFuncPtr() expected exactly 1 argument, got {}",
			positional.len()
		));
	}
	let (restype, restype_object, argtypes) = match cfunc_defaults_from_class(cls) {
		Ok(defaults) => defaults,
		Err(error) => return error,
	};
	let target = crate::tag::untag_arg(positional[0]);
	if let Ok(address) = int_arg(target, "address") {
		return cfunc_from_address(
			cls,
			format!("0x{:x}", address as usize),
			address as usize as *mut c_void,
			restype,
			restype_object,
			argtypes,
		);
	}
	let items = match sequence_items(target, "function specifier") {
		Ok(items) => items,
		Err(error) => return error,
	};
	let [symbol, library] = items.as_slice() else {
		return type_error("function specifier must be a (name, library) pair");
	};
	let symbol_text = match str_arg(*symbol, "function name") {
		Ok(symbol) => symbol,
		Err(error) => return error,
	};
	let handle = match handle_from_library_object(*library) {
		Ok(handle) => handle,
		Err(error) => return error,
	};
	let symbol = match CString::new(symbol_text.as_bytes()) {
		Ok(symbol) => symbol,
		Err(_) => return value_error("embedded null byte in symbol name"),
	};
	let address = unsafe { libc::dlsym(handle, symbol.as_ptr()) };
	if address.is_null() {
		return os_error("dlsym failed");
	}
	cfunc_from_address(cls, symbol_text, address, restype, restype_object, argtypes)
}

unsafe fn cfunc_receiver<'a>(object: *mut PyObject) -> Option<&'a mut PyCFunc> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || !unsafe { type_has_base((*object).ob_type, cfunc_type().cast_const()) } {
		return None;
	}
	Some(unsafe { &mut *object.cast::<PyCFunc>() })
}

fn cdata_word(cdata: &PyCData) -> usize {
	match cdata.kind {
		CKind::CharP | CKind::WCharP => cdata
			.bytes
			.as_ref()
			.map_or(0, |bytes| bytes.as_ptr() as usize),
		CKind::PyObject => cdata.value as usize,
		_ => cdata.value as usize,
	}
}

fn c_arg_word(object: *mut PyObject, keepalive: &mut Vec<Vec<u8>>) -> Result<usize, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || is_none(object) {
		return Ok(0);
	}
	if let Some(cdata) = unsafe { cdata_receiver(object) } {
		return Ok(cdata_word(cdata));
	}
	if let Some(pointer) = unsafe { pointer_receiver(object) } {
		return Ok(pointer.address);
	}
	if let Some(value) = unsafe { crate::types::int::to_bigint_including_bool(object) }
		.and_then(|value| value.to_i64())
	{
		return Ok(value as usize);
	}
	if let Some(bytes) = bytes_like(object) {
		let mut owned = bytes.to_vec();
		owned.push(0);
		let ptr = owned.as_ptr() as usize;
		keepalive.push(owned);
		return Ok(ptr);
	}
	if let Some(text) = unsafe { type_::unicode_text(object) } {
		let mut owned = text.as_bytes().to_vec();
		owned.push(0);
		let ptr = owned.as_ptr() as usize;
		keepalive.push(owned);
		return Ok(ptr);
	}
	Err(type_error(&format!("cannot convert '{}' object to a C argument", type_name(object))))
}

unsafe fn call_c_address(address: *mut c_void, words: &[usize]) -> usize {
	match words {
		[] => unsafe {
			core::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> usize>(address)()
		},
		[a] => unsafe {
			core::mem::transmute::<*mut c_void, unsafe extern "C" fn(usize) -> usize>(address)(*a)
		},
		[a, b] => unsafe {
			core::mem::transmute::<*mut c_void, unsafe extern "C" fn(usize, usize) -> usize>(address)(
				*a, *b,
			)
		},
		[a, b, c] => unsafe {
			core::mem::transmute::<*mut c_void, unsafe extern "C" fn(usize, usize, usize) -> usize>(
				address,
			)(*a, *b, *c)
		},
		[a, b, c, d] => unsafe {
			core::mem::transmute::<
				*mut c_void,
				unsafe extern "C" fn(usize, usize, usize, usize) -> usize,
			>(address)(*a, *b, *c, *d)
		},
		[a, b, c, d, e] => unsafe {
			core::mem::transmute::<
				*mut c_void,
				unsafe extern "C" fn(usize, usize, usize, usize, usize) -> usize,
			>(address)(*a, *b, *c, *d, *e)
		},
		[a, b, c, d, e, f] => unsafe {
			core::mem::transmute::<
				*mut c_void,
				unsafe extern "C" fn(usize, usize, usize, usize, usize, usize) -> usize,
			>(address)(*a, *b, *c, *d, *e, *f)
		},
		_ => 0,
	}
}

fn c_result_to_py(kind: Option<CKind>, result: usize) -> *mut PyObject {
	match kind {
		None => none(),
		Some(CKind::Bool) => py_bool(result != 0),
		Some(CKind::CharP) => {
			if result == 0 {
				none()
			} else {
				let bytes = unsafe { CStr::from_ptr(result as *const c_char).to_bytes() };
				py_bytes(bytes)
			}
		},
		Some(CKind::VoidP) => {
			if result == 0 {
				none()
			} else {
				py_int(result as i64)
			}
		},
		Some(CKind::PyObject) => {
			if result == 0 {
				none()
			} else {
				result as *mut PyObject
			}
		},
		Some(kind) if kind.signed() => py_int(result as isize as i64),
		Some(_) => py_int(result as i64),
	}
}

fn argtype_kind(argtypes: *mut PyObject, index: usize) -> Option<CKind> {
	if argtypes.is_null() || is_none(argtypes) {
		return None;
	}
	let items = sequence_items(argtypes, "argtypes").ok()?;
	items.get(index).copied().and_then(c_kind_from_type_object)
}

fn c_arg_word_for_kind(
	object: *mut PyObject,
	expected: Option<CKind>,
	keepalive: &mut Vec<Vec<u8>>,
) -> Result<usize, *mut PyObject> {
	if expected == Some(CKind::PyObject) {
		return Ok(crate::tag::untag_arg(object) as usize);
	}
	c_arg_word(object, keepalive)
}

unsafe extern "C" fn cfunc_call(
	callee: *mut PyObject,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	if !kwargs.is_null() {
		return type_error("foreign function calls do not accept keyword arguments");
	}
	let Some(function) = (unsafe { cfunc_receiver(callee) }) else {
		return type_error("C function call on non-CFuncPtr");
	};
	let positional = match unsafe { type_::positional_args_from_object(args) } {
		Ok(positional) => positional,
		Err(message) => return type_error(&message),
	};
	if positional.len() > 6 {
		return type_error("foreign function calls support at most 6 integer/pointer arguments");
	}
	let mut keepalive = Vec::new();
	let mut words = Vec::with_capacity(positional.len());
	for (index, arg) in positional.into_iter().enumerate() {
		let expected = argtype_kind(function.argtypes, index);
		match c_arg_word_for_kind(arg, expected, &mut keepalive) {
			Ok(word) => words.push(word),
			Err(error) => return error,
		}
	}
	let result = unsafe { call_c_address(function.address, &words) };
	c_result_to_py(function.restype, result)
}

unsafe extern "C" fn cfunc_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let name = match attr_name(name) {
		Ok(name) => name,
		Err(error) => return error,
	};
	let Some(function) = (unsafe { cfunc_receiver(object) }) else {
		return type_error("C function attribute lookup on non-CFuncPtr");
	};
	match name {
		"__name__" => py_str(&function.name),
		"restype" => function.restype_object,
		"argtypes" => function.argtypes,
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name)) },
	}
}

unsafe extern "C" fn cfunc_setattro(
	object: *mut PyObject,
	name: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	let name = match attr_name(name) {
		Ok(name) => name,
		Err(_) => return -1,
	};
	let Some(function) = (unsafe { cfunc_receiver(object) }) else {
		let _ = type_error("C function attribute assignment on non-CFuncPtr");
		return -1;
	};
	match name {
		"restype" => {
			if value.is_null() || is_none(value) {
				function.restype = None;
				function.restype_object = none();
				return 0;
			}
			let Some(kind) = c_kind_from_type_object(value) else {
				let _ = type_error("restype must be a ctypes type or None");
				return -1;
			};
			function.restype = Some(kind);
			function.restype_object = value;
			0
		},
		"argtypes" => {
			function.argtypes = if value.is_null() { none() } else { value };
			0
		},
		_ => {
			let _ = unsafe { abi::exc::pon_raise_attribute_error(object, intern(name)) };
			-1
		},
	}
}

unsafe extern "C" fn ctypes_dlopen_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "dlopen") {
		Ok(args) => args,
		Err(error) => return error,
	};
	if args.is_empty() || args.len() > 2 {
		return type_error(&format!("dlopen() expected name and optional mode, got {}", args.len()));
	}
	let name = match cdll_name_arg(args[0]) {
		Ok(name) => name,
		Err(error) => return error,
	};
	let mode = match optional_int_arg(args, 1, libc::RTLD_NOW as i64, "mode") {
		Ok(mode) => mode as c_int,
		Err(error) => return error,
	};
	let c_name = match name.as_ref() {
		Some(name) => match CString::new(name.as_bytes()) {
			Ok(name) => Some(name),
			Err(_) => return value_error("embedded null byte in library name"),
		},
		None => None,
	};
	let raw_name = c_name.as_ref().map_or(ptr::null(), |name| name.as_ptr());
	let handle = unsafe { libc::dlopen(raw_name, mode) };
	if handle.is_null() {
		return os_error("dlopen failed");
	}
	py_int(handle as usize as i64)
}

unsafe extern "C" fn ctypes_load_library_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	unsafe { ctypes_dlopen_entry(argv, argc) }
}

unsafe extern "C" fn ctypes_dlsym_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "dlsym") {
		Ok([handle, name]) => [*handle, *name],
		Ok(args) => return type_error(&format!("dlsym() expected 2 arguments, got {}", args.len())),
		Err(error) => return error,
	};
	let handle = match int_arg(args[0], "handle") {
		Ok(handle) => handle as usize as *mut c_void,
		Err(error) => return error,
	};
	let name = match CString::new(match bytes_or_text_arg(args[1], "name") {
		Ok(name) => name,
		Err(error) => return error,
	}) {
		Ok(name) => name,
		Err(_) => return value_error("embedded null byte in symbol name"),
	};
	let address = unsafe { libc::dlsym(handle, name.as_ptr()) };
	if address.is_null() {
		return os_error("dlsym failed");
	}
	py_int(address as usize as i64)
}

unsafe extern "C" fn ctypes_dlclose_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "dlclose") {
		Ok([handle]) => [*handle],
		Ok(args) => return type_error(&format!("dlclose() expected 1 argument, got {}", args.len())),
		Err(error) => return error,
	};
	let handle = match int_arg(args[0], "handle") {
		Ok(handle) => handle as usize as *mut c_void,
		Err(error) => return error,
	};
	if unsafe { libc::dlclose(handle) } != 0 {
		os_error("dlclose failed")
	} else {
		none()
	}
}

unsafe extern "C" fn ctypes_dyld_shared_cache_contains_path_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "_dyld_shared_cache_contains_path") {
		Ok([path]) => [*path],
		Ok(args) => {
			return type_error(&format!(
				"_dyld_shared_cache_contains_path() expected 1 argument, got {}",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let path = match str_arg(args[0], "path") {
		Ok(path) => path,
		Err(error) => return error,
	};
	#[cfg(target_os = "macos")]
	{
		let c_path = match CString::new(path) {
			Ok(path) => path,
			Err(_) => return value_error("embedded null character"),
		};
		return py_bool(unsafe { _dyld_shared_cache_contains_path(c_path.as_ptr()) });
	}
	#[cfg(not(target_os = "macos"))]
	{
		let _ = path;
		raise(
			ExceptionKind::NotImplementedError,
			"_dyld_shared_cache_contains_path is only available on Darwin",
		)
	}
}

unsafe extern "C" fn ctypes_resize_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "resize") {
		Ok([object, size]) => [*object, *size],
		Ok(args) => return type_error(&format!("resize() expected 2 arguments, got {}", args.len())),
		Err(error) => return error,
	};
	let size = match int_arg(args[1], "size") {
		Ok(size) if size >= 0 => size as usize,
		Ok(_) => return value_error("minimum size is negative"),
		Err(error) => return error,
	};
	let Some(cdata) = (unsafe { cdata_receiver(args[0]) }) else {
		return type_error("resize() argument must be ctypes data");
	};
	if size < cdata.kind.size() {
		return value_error("minimum size is too small");
	}
	none()
}

unsafe extern "C" fn ctypes_buffer_info_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "buffer_info") {
		Ok([typ]) => [*typ],
		Ok(args) => {
			return type_error(&format!("buffer_info() expected 1 argument, got {}", args.len()));
		},
		Err(error) => return error,
	};
	let Some(kind) = c_kind_from_type_object(args[0]) else {
		return type_error("buffer_info() argument must be a ctypes scalar type");
	};
	alloc_tuple(vec![py_str(kind.typecode()), py_int(0), none()])
}

unsafe extern "C" fn ctypes_string_at_raw(ptr_value: usize, size: isize) -> *mut PyObject {
	if ptr_value == 0 {
		return value_error("NULL pointer access");
	}
	let ptr = ptr_value as *const c_char;
	if size < 0 {
		py_bytes(unsafe { CStr::from_ptr(ptr) }.to_bytes())
	} else {
		py_bytes(unsafe { core::slice::from_raw_parts(ptr.cast::<u8>(), size as usize) })
	}
}

unsafe extern "C" fn ctypes_wstring_at_raw(ptr_value: usize, size: isize) -> *mut PyObject {
	if ptr_value == 0 {
		return value_error("NULL pointer access");
	}
	let mut out = String::new();
	let ptr = ptr_value as *const libc::wchar_t;
	let mut index = 0usize;
	loop {
		if size >= 0 && index >= size as usize {
			break;
		}
		let value = unsafe { *ptr.add(index) };
		if size < 0 && value == 0 {
			break;
		}
		if let Some(ch) = char::from_u32(value as u32) {
			out.push(ch);
		}
		index += 1;
	}
	py_str(&out)
}

unsafe extern "C" fn ctypes_memoryview_at_raw(
	ptr_value: usize,
	size: isize,
	_readonly: usize,
) -> *mut PyObject {
	if ptr_value == 0 {
		return value_error("NULL pointer access");
	}
	if size < 0 {
		return value_error("memoryview_at size must be non-negative");
	}
	let bytes =
		py_bytes(unsafe { core::slice::from_raw_parts(ptr_value as *const u8, size as usize) });
	match unsafe { memoryview_type::boxed_memoryview_from_object(bytes) } {
		Ok(view) => view.cast::<PyObject>(),
		Err(message) => runtime_error(&message),
	}
}

unsafe extern "C" fn ctypes_cast_raw(
	address: usize,
	_source: *mut PyObject,
	target: *mut PyObject,
) -> *mut PyObject {
	let Some(kind) = c_kind_from_type_object(target) else {
		return type_error("cast() target must be a ctypes type");
	};
	let value = py_int(address as i64);
	cdata_new_for_type(kind, target.cast::<PyType>(), Some(value))
}

fn ctypes_attrs(name: &str) -> Result<Vec<(u32, *mut PyObject)>, String> {
	let mut attrs = vec![
		str_attr("__name__", name)?,
		str_attr("__version__", "1.1.0")?,
		int_attr("RTLD_LOCAL", libc::RTLD_LOCAL as i64)?,
		int_attr("RTLD_GLOBAL", libc::RTLD_GLOBAL as i64)?,
		int_attr("FUNCFLAG_CDECL", 1)?,
		int_attr("FUNCFLAG_PYTHONAPI", 4)?,
		int_attr("FUNCFLAG_USE_ERRNO", 8)?,
		int_attr("FUNCFLAG_USE_LASTERROR", 16)?,
		int_attr("SIZEOF_TIME_T", core::mem::size_of::<libc::time_t>() as i64)?,
		(intern("_SimpleCData"), simple_cdata_type().cast::<PyObject>()),
		(intern("_Pointer"), pointer_type().cast::<PyObject>()),
		(intern("CFuncPtr"), cfunc_type().cast::<PyObject>()),
		(intern("Array"), array_type().cast::<PyObject>()),
		(intern("Structure"), structure_type().cast::<PyObject>()),
		(intern("Union"), union_type().cast::<PyObject>()),
		(intern("CField"), cfield_type().cast::<PyObject>()),
		(intern("CDLL"), cdll_type().cast::<PyObject>()),
		(intern("PyDLL"), cdll_type().cast::<PyObject>()),
		int_attr("CTYPES_MAX_ARGCOUNT", 1024)?,
		int_attr("_memmove_addr", libc::memmove as *const () as usize as i64)?,
		int_attr("_memset_addr", libc::memset as *const () as usize as i64)?,
		int_attr("_string_at_addr", ctypes_string_at_raw as *const () as usize as i64)?,
		int_attr("_wstring_at_addr", ctypes_wstring_at_raw as *const () as usize as i64)?,
		int_attr("_memoryview_at_addr", ctypes_memoryview_at_raw as *const () as usize as i64)?,
		int_attr("_cast_addr", ctypes_cast_raw as *const () as usize as i64)?,
		(intern("c_int"), c_type_for_kind(CKind::Int).cast::<PyObject>()),
		(intern("c_uint"), c_type_for_kind(CKind::UInt).cast::<PyObject>()),
		(intern("c_long"), c_type_for_kind(CKind::Long).cast::<PyObject>()),
		(intern("c_ulong"), c_type_for_kind(CKind::ULong).cast::<PyObject>()),
		(intern("c_longlong"), c_type_for_kind(CKind::LongLong).cast::<PyObject>()),
		(intern("c_ulonglong"), c_type_for_kind(CKind::ULongLong).cast::<PyObject>()),
		(intern("c_short"), c_type_for_kind(CKind::Short).cast::<PyObject>()),
		(intern("c_ushort"), c_type_for_kind(CKind::UShort).cast::<PyObject>()),
		(intern("c_byte"), c_type_for_kind(CKind::Byte).cast::<PyObject>()),
		(intern("c_ubyte"), c_type_for_kind(CKind::UByte).cast::<PyObject>()),
		(intern("c_bool"), c_type_for_kind(CKind::Bool).cast::<PyObject>()),
		(intern("c_size_t"), c_type_for_kind(CKind::SizeT).cast::<PyObject>()),
		(intern("c_ssize_t"), c_type_for_kind(CKind::Long).cast::<PyObject>()),
		(intern("c_void_p"), c_type_for_kind(CKind::VoidP).cast::<PyObject>()),
		(intern("c_char_p"), c_type_for_kind(CKind::CharP).cast::<PyObject>()),
		function_attr("sizeof", "sizeof", ctypes_sizeof_entry)?,
		function_attr("alignment", "alignment", ctypes_alignment_entry)?,
		function_attr("addressof", "addressof", ctypes_addressof_entry)?,
		function_attr("byref", "byref", ctypes_byref_entry)?,
		function_attr("pointer", "pointer", ctypes_pointer_entry)?,
		function_attr("POINTER", "POINTER", ctypes_pointer_type_entry)?,
		function_attr("dlopen", "dlopen", ctypes_dlopen_entry)?,
		function_attr("dlsym", "dlsym", ctypes_dlsym_entry)?,
		function_attr("dlclose", "dlclose", ctypes_dlclose_entry)?,
		function_attr("LoadLibrary", "LoadLibrary", ctypes_load_library_entry)?,
		function_attr("resize", "resize", ctypes_resize_entry)?,
		function_attr("buffer_info", "buffer_info", ctypes_buffer_info_entry)?,
		function_attr("get_errno", "get_errno", ctypes_get_errno_entry)?,
		function_attr("set_errno", "set_errno", ctypes_set_errno_entry)?,
		function_attr(
			"_dyld_shared_cache_contains_path",
			"_dyld_shared_cache_contains_path",
			ctypes_dyld_shared_cache_contains_path_entry,
		)?,
	];
	let argument_error = exception_class("ctypes", "ArgumentError", "Exception")?;
	attrs.push((intern("ArgumentError"), argument_error));
	Ok(attrs)
}

pub(super) fn make_ctypes_underscore_module() -> Result<*mut PyObject, String> {
	install_module("_ctypes", ctypes_attrs("_ctypes")?)
}
