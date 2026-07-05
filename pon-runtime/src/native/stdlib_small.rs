//! Small native stdlib modules that close top-level CPython accelerator gaps.
//!
//! These are intentionally real implementations, not import shells:
//! `_functools` exposes working `reduce`/`cmp_to_key`, `_json` provides the
//! string scanner and string encoders used by `json`, `_locale` delegates to
//! the platform C locale, and `_datetime` re-exports the vendored pure-Python
//! datetime types when the C API capsule is not available.

use core::{ffi::c_int, ptr};
use std::{
	ffi::{CStr, CString},
	sync::LazyLock,
};

use num_traits::ToPrimitive;

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{
	abi::{self, pon_call, pon_get_iter, pon_iter_next},
	abstract_op::{RICH_EQ, RICH_GE, RICH_GT, RICH_LE, RICH_LT, RICH_NE},
	gcroot::{HeldRoots, RootRegistry},
	intern::intern,
	object::{PyObject, PyObjectHeader, PyType},
	thread_state::{pon_err_clear, pon_err_message},
	types::{exc::ExceptionKind, type_::unicode_text},
};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

// ---------------------------------------------------------------------------
// Shared helpers

fn py_str(text: &str) -> *mut PyObject {
	unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn py_int(value: i64) -> *mut PyObject {
	unsafe { abi::pon_const_int(value) }
}

fn py_bool(value: bool) -> *mut PyObject {
	unsafe { abi::pon_const_bool(c_int::from(value)) }
}

fn py_none() -> *mut PyObject {
	unsafe { abi::pon_none() }
}

fn is_none(object: *mut PyObject) -> bool {
	crate::tag::untag_arg(object) == py_none()
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

fn overflow_error(message: &str) -> *mut PyObject {
	raise(ExceptionKind::OverflowError, message)
}

fn module_name_attr(name: &str) -> Result<(u32, *mut PyObject), String> {
	let object = py_str(name);
	(!object.is_null())
		.then_some((intern("__name__"), object))
		.ok_or_else(|| format!("failed to allocate {name}.__name__"))
}

fn function_attr(name: &str, entry: BuiltinFn) -> Result<(u32, *mut PyObject), String> {
	let function =
		unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	(!function.is_null())
		.then_some((intern(name), function))
		.ok_or_else(|| format!("failed to allocate native function {name}"))
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
	let object = py_int(value);
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate integer attribute {name}"))
}

fn object_type() -> *mut PyType {
	abi::runtime_global(intern("object")).map_or(ptr::null_mut(), |object| object.cast::<PyType>())
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

fn str_arg(object: *mut PyObject, what: &str) -> Result<String, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	unsafe { unicode_text(object) }
		.map(str::to_owned)
		.ok_or_else(|| type_error(&format!("{what} must be str")))
}

fn int_arg(object: *mut PyObject, what: &str) -> Result<i64, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	let Some(value) = (unsafe { crate::types::int::to_bigint_including_bool(object) }) else {
		return Err(type_error(&format!("{what} must be an integer")));
	};
	value
		.to_i64()
		.ok_or_else(|| overflow_error(&format!("{what} is too large")))
}

fn truth_arg(object: *mut PyObject, what: &str) -> Result<bool, *mut PyObject> {
	let value = unsafe { crate::abstract_op::is_true(crate::tag::untag_arg(object)) };
	match value {
		0 => Ok(false),
		1 => Ok(true),
		_ => Err(type_error(&format!("truth-value testing failed for {what}"))),
	}
}

fn import_module_attr(module: &str, attr: &str) -> Result<*mut PyObject, String> {
	let module_name = intern(module);
	let imported = unsafe { crate::import::pon_import_name(module_name, ptr::null(), 0, 0) };
	if imported.is_null() {
		let detail = pon_err_message().unwrap_or_else(|| "unknown import error".to_owned());
		pon_err_clear();
		return Err(format!("failed to import {module}: {detail}"));
	}
	crate::import::module_attr(module_name, intern(attr))
		.ok_or_else(|| format!("{module} did not define {attr}"))
}

fn string_list(values: &[&str]) -> *mut PyObject {
	let mut items = Vec::with_capacity(values.len());
	for value in values {
		let object = py_str(value);
		if object.is_null() {
			return ptr::null_mut();
		}
		items.push(object);
	}
	unsafe {
		abi::seq::pon_build_list(
			if items.is_empty() {
				ptr::null_mut()
			} else {
				items.as_mut_ptr()
			},
			items.len(),
		)
	}
}

fn tuple2(first: *mut PyObject, second: *mut PyObject) -> *mut PyObject {
	let mut items = [first, second];
	unsafe { abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

fn list_from_ints(values: &[i64]) -> *mut PyObject {
	let mut items = Vec::with_capacity(values.len());
	for &value in values {
		let object = py_int(value);
		if object.is_null() {
			return ptr::null_mut();
		}
		items.push(object);
	}
	unsafe {
		abi::seq::pon_build_list(
			if items.is_empty() {
				ptr::null_mut()
			} else {
				items.as_mut_ptr()
			},
			items.len(),
		)
	}
}

fn dict_from_pairs(pairs: Vec<(&str, *mut PyObject)>) -> *mut PyObject {
	let mut flat = Vec::with_capacity(pairs.len() * 2);
	for (key, value) in pairs {
		if value.is_null() {
			return ptr::null_mut();
		}
		let key = py_str(key);
		if key.is_null() {
			return ptr::null_mut();
		}
		flat.push(key);
		flat.push(value);
	}
	unsafe {
		abi::map::pon_build_map(
			if flat.is_empty() {
				ptr::null_mut()
			} else {
				flat.as_mut_ptr()
			},
			flat.len() / 2,
		)
	}
}

// ---------------------------------------------------------------------------
// `_functools`

#[repr(C)]
struct PyCmpKeyFactory {
	ob_base: PyObjectHeader,
	cmp:     *mut PyObject,
}

#[repr(C)]
struct PyCmpKey {
	ob_base: PyObjectHeader,
	cmp:     *mut PyObject,
	obj:     *mut PyObject,
}

static CMP_KEY_REGISTRY: RootRegistry = RootRegistry::new();

impl HeldRoots for PyCmpKeyFactory {
	unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
		push(self.cmp);
	}
}

impl HeldRoots for PyCmpKey {
	unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
		push(self.cmp);
		push(self.obj);
	}
}

pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
	CMP_KEY_REGISTRY.held_roots()
}

static CMP_KEY_FACTORY_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"functools.KeyWrapperFactory",
		core::mem::size_of::<PyCmpKeyFactory>(),
	);
	ty.tp_base = object_type();
	ty.tp_call = Some(cmp_key_factory_call);
	Box::into_raw(Box::new(ty)) as usize
});

static CMP_KEY_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"functools.KeyWrapper",
		core::mem::size_of::<PyCmpKey>(),
	);
	ty.tp_base = object_type();
	ty.tp_getattro = Some(cmp_key_getattro);
	ty.tp_richcmp = Some(cmp_key_richcmp);
	Box::into_raw(Box::new(ty)) as usize
});

fn alloc_cmp_key_factory(cmp: *mut PyObject) -> *mut PyObject {
	let object = Box::into_raw(Box::new(PyCmpKeyFactory {
		ob_base: PyObjectHeader::new((*CMP_KEY_FACTORY_TYPE as *mut PyType).cast_const()),
		cmp:     crate::tag::untag_arg(cmp),
	}));
	CMP_KEY_REGISTRY.register::<PyCmpKeyFactory>(object.cast::<PyObject>())
}

fn alloc_cmp_key(cmp: *mut PyObject, obj: *mut PyObject) -> *mut PyObject {
	let object = Box::into_raw(Box::new(PyCmpKey {
		ob_base: PyObjectHeader::new((*CMP_KEY_TYPE as *mut PyType).cast_const()),
		cmp:     crate::tag::untag_arg(cmp),
		obj:     crate::tag::untag_arg(obj),
	}));
	CMP_KEY_REGISTRY.register::<PyCmpKey>(object.cast::<PyObject>())
}

unsafe extern "C" fn cmp_key_factory_call(
	callee: *mut PyObject,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	if !kwargs.is_null() {
		return type_error("cmp_to_key key wrapper takes no keyword arguments");
	}
	let positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
		Ok(positional) => positional,
		Err(message) => return type_error(&message),
	};
	if positional.len() != 1 {
		return type_error(&format!("K() takes exactly one argument ({} given)", positional.len()));
	}
	let factory = unsafe { &*callee.cast::<PyCmpKeyFactory>() };
	alloc_cmp_key(factory.cmp, positional[0])
}

unsafe extern "C" fn cmp_key_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let Some(name) = (unsafe { unicode_text(crate::tag::untag_arg(name)) }) else {
		return type_error("attribute name must be str");
	};
	if name == "obj" {
		return unsafe { (*object.cast::<PyCmpKey>()).obj };
	}
	abi::exc::raise_attribute_error_text(&format!(
		"'functools.KeyWrapper' object has no attribute '{name}'"
	))
}

fn cmp_result_value(object: *mut PyObject) -> Result<i32, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if let Some(value) = unsafe { crate::types::int::to_bigint_including_bool(object) }
		.and_then(|value| value.to_i64())
	{
		return Ok(value.signum() as i32);
	}
	if let Some(value) = unsafe { crate::types::float::to_f64(object) } {
		return Ok(if value < 0.0 {
			-1
		} else if value > 0.0 {
			1
		} else {
			0
		});
	}
	Err(type_error("comparison function must return a number"))
}

unsafe extern "C" fn cmp_key_richcmp(
	left: *mut PyObject,
	right: *mut PyObject,
	op: c_int,
) -> *mut PyObject {
	let left = unsafe { &*left.cast::<PyCmpKey>() };
	if right.is_null()
		|| unsafe { (*crate::tag::untag_arg(right)).ob_type }
			!= (*CMP_KEY_TYPE as *mut PyType).cast_const()
	{
		return unsafe { abi::pon_not_implemented() };
	}
	let right = unsafe { &*crate::tag::untag_arg(right).cast::<PyCmpKey>() };
	let mut argv = [left.obj, right.obj];
	let result = unsafe { pon_call(left.cmp, argv.as_mut_ptr(), argv.len()) };
	if result.is_null() {
		return ptr::null_mut();
	}
	let cmp = match cmp_result_value(result) {
		Ok(value) => value,
		Err(error) => return error,
	};
	let truth = match op as u8 {
		RICH_LT => cmp < 0,
		RICH_LE => cmp <= 0,
		RICH_EQ => cmp == 0,
		RICH_NE => cmp != 0,
		RICH_GT => cmp > 0,
		RICH_GE => cmp >= 0,
		_ => return unsafe { abi::pon_not_implemented() },
	};
	py_bool(truth)
}

unsafe extern "C" fn functools_cmp_to_key(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { argv_slice(argv, argc) } {
		Some(args) => args,
		None => return type_error("cmp_to_key() received a null argument vector"),
	};
	if args.len() != 1 {
		return type_error(&format!("cmp_to_key expected 1 argument, got {}", args.len()));
	}
	alloc_cmp_key_factory(args[0])
}

unsafe extern "C" fn functools_reduce(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { argv_slice(argv, argc) } {
		Some(args) => args,
		None => return type_error("reduce() received a null argument vector"),
	};
	if !(2..=3).contains(&args.len()) {
		return type_error(&format!("reduce expected at least 2 arguments, got {}", args.len()));
	}
	let function = crate::tag::untag_arg(args[0]);
	let iterable = crate::tag::untag_arg(args[1]);
	let iterator = unsafe { pon_get_iter(iterable, ptr::null_mut()) };
	if iterator.is_null() {
		return ptr::null_mut();
	}
	let mut value = if args.len() == 3 {
		crate::tag::untag_arg(args[2])
	} else {
		let first = unsafe { pon_iter_next(iterator, ptr::null_mut()) };
		if first.is_null() {
			if abi::exc::pending_exception_is("StopIteration") {
				pon_err_clear();
				return type_error("reduce() of empty iterable with no initial value");
			}
			return ptr::null_mut();
		}
		crate::tag::untag_arg(first)
	};
	loop {
		let item = unsafe { pon_iter_next(iterator, ptr::null_mut()) };
		if item.is_null() {
			if abi::exc::pending_exception_is("StopIteration") {
				pon_err_clear();
				return value;
			}
			return ptr::null_mut();
		}
		let mut call_args = [value, crate::tag::untag_arg(item)];
		let next = unsafe { pon_call(function, call_args.as_mut_ptr(), call_args.len()) };
		if next.is_null() {
			return ptr::null_mut();
		}
		value = crate::tag::untag_arg(next);
	}
}

const FUNCTOOLS_PY_ALIASES: &[&str] =
	&["partial", "Placeholder", "_PlaceholderType", "_lru_cache_wrapper"];
const FUNCTOOLS_DIR: &[&str] =
	&["Placeholder", "_PlaceholderType", "_lru_cache_wrapper", "cmp_to_key", "partial", "reduce"];

unsafe extern "C" fn functools_getattr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { argv_slice(argv, argc) } {
		Some(args) => args,
		None => return type_error("_functools.__getattr__() received a null argument vector"),
	};
	if args.len() != 1 {
		return type_error(&format!(
			"_functools.__getattr__ expected 1 argument, got {}",
			args.len()
		));
	}
	let name = match str_arg(args[0], "name") {
		Ok(name) => name,
		Err(error) => return error,
	};
	if !FUNCTOOLS_PY_ALIASES.contains(&name.as_str()) {
		return raise(
			ExceptionKind::AttributeError,
			&format!("module '_functools' has no attribute '{name}'"),
		);
	}
	let value = match import_module_attr("functools", &name) {
		Ok(value) => value,
		Err(message) => return abi::return_null_with_error(message),
	};
	let _ = crate::import::store_module_attr(intern("_functools"), intern(&name), value);
	value
}

unsafe extern "C" fn functools_dir(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { argv_slice(argv, argc) } {
		Some(args) => args,
		None => return type_error("_functools.__dir__() received a null argument vector"),
	};
	if !args.is_empty() {
		return type_error(&format!("_functools.__dir__ expected 0 arguments, got {}", args.len()));
	}
	string_list(FUNCTOOLS_DIR)
}

pub(super) fn make_functools_module() -> Result<*mut PyObject, String> {
	install_module("_functools", vec![
		module_name_attr("_functools")?,
		function_attr("cmp_to_key", functools_cmp_to_key)?,
		function_attr("reduce", functools_reduce)?,
		function_attr("__getattr__", functools_getattr)?,
		function_attr("__dir__", functools_dir)?,
	])
}

// ---------------------------------------------------------------------------
// `_datetime`

pub(super) fn make_datetime_module() -> Result<*mut PyObject, String> {
	let pydatetime_name = intern("_pydatetime");
	let module = unsafe { crate::import::pon_import_name(pydatetime_name, ptr::null(), 0, 0) };
	if module.is_null() {
		let detail = pon_err_message().unwrap_or_else(|| "unknown import error".to_owned());
		pon_err_clear();
		return Err(format!("failed to import _pydatetime fallback: {detail}"));
	}

	let mut attrs = vec![module_name_attr("_datetime")?];
	for name in
		["MINYEAR", "MAXYEAR", "UTC", "date", "datetime", "time", "timedelta", "timezone", "tzinfo"]
	{
		let Some(value) = crate::import::module_attr(pydatetime_name, intern(name)) else {
			return Err(format!("_pydatetime did not define {name}"));
		};
		attrs.push((intern(name), value));
	}
	if let Some(all) = crate::import::module_attr(pydatetime_name, intern("__all__")) {
		attrs.push((intern("__all__"), all));
	}
	install_module("_datetime", attrs)
}

// ---------------------------------------------------------------------------
// `_json`

#[repr(C)]
struct PyJsonFloatStr {
	ob_base:   PyObjectHeader,
	allow_nan: bool,
}

static JSON_FLOATSTR_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"_json.FloatStringifier",
		core::mem::size_of::<PyJsonFloatStr>(),
	);
	ty.tp_base = object_type();
	ty.tp_call = Some(json_floatstr_call);
	Box::into_raw(Box::new(ty)) as usize
});

fn alloc_json_floatstr(allow_nan: bool) -> *mut PyObject {
	Box::into_raw(Box::new(PyJsonFloatStr {
		ob_base: PyObjectHeader::new((*JSON_FLOATSTR_TYPE as *mut PyType).cast_const()),
		allow_nan,
	}))
	.cast::<PyObject>()
}

unsafe extern "C" fn json_floatstr_call(
	callee: *mut PyObject,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	if !kwargs.is_null() {
		return type_error("_json floatstr takes no keyword arguments");
	}
	let positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
		Ok(positional) => positional,
		Err(message) => return type_error(&message),
	};
	if positional.len() != 1 {
		return type_error(&format!("_json floatstr expected 1 argument, got {}", positional.len()));
	}
	let allow_nan = unsafe { (*callee.cast::<PyJsonFloatStr>()).allow_nan };
	json_floatstr_value(crate::tag::untag_arg(positional[0]), allow_nan)
}

fn json_floatstr_value(object: *mut PyObject, allow_nan: bool) -> *mut PyObject {
	let Some(value) = (unsafe { crate::types::float::to_f64(object) }) else {
		return match super::builtins_mod::try_repr_text(object) {
			Ok(text) => py_str(&text),
			Err(()) => ptr::null_mut(),
		};
	};
	let text = if value.is_nan() {
		"NaN".to_owned()
	} else if value == f64::INFINITY {
		"Infinity".to_owned()
	} else if value == f64::NEG_INFINITY {
		"-Infinity".to_owned()
	} else {
		match super::builtins_mod::try_repr_text(object) {
			Ok(text) => text,
			Err(()) => return ptr::null_mut(),
		}
	};
	if !value.is_finite() && !allow_nan {
		let repr = super::builtins_mod::repr_text(object);
		return value_error(&format!("Out of range float values are not JSON compliant: {repr}"));
	}
	py_str(&text)
}

fn encode_json_string(text: &str, ensure_ascii: bool) -> String {
	let mut out = String::with_capacity(text.len() + 2);
	out.push('"');
	for ch in text.chars() {
		match ch {
			'"' => out.push_str("\\\""),
			'\\' => out.push_str("\\\\"),
			'\u{08}' => out.push_str("\\b"),
			'\u{0c}' => out.push_str("\\f"),
			'\n' => out.push_str("\\n"),
			'\r' => out.push_str("\\r"),
			'\t' => out.push_str("\\t"),
			ch if ch <= '\u{1f}' => out.push_str(&format!("\\u{:04x}", ch as u32)),
			ch if ensure_ascii && (ch as u32) > 0x7f => {
				let code = ch as u32;
				if code <= 0xffff {
					out.push_str(&format!("\\u{code:04x}"));
				} else {
					let value = code - 0x1_0000;
					let high = 0xd800 + ((value >> 10) & 0x3ff);
					let low = 0xdc00 + (value & 0x3ff);
					out.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
				}
			},
			ch => out.push(ch),
		}
	}
	out.push('"');
	out
}

unsafe extern "C" fn json_encode_basestring_ascii(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	json_encode_basestring_impl(argv, argc, true)
}

unsafe extern "C" fn json_encode_basestring(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	json_encode_basestring_impl(argv, argc, false)
}

fn json_encode_basestring_impl(
	argv: *mut *mut PyObject,
	argc: usize,
	ensure_ascii: bool,
) -> *mut PyObject {
	let args = match unsafe { argv_slice(argv, argc) } {
		Some(args) => args,
		None => return type_error("encoder received a null argument vector"),
	};
	if args.len() != 1 {
		return type_error(&format!("encode_basestring expected 1 argument, got {}", args.len()));
	}
	let text = match str_arg(args[0], "s") {
		Ok(text) => text,
		Err(error) => return error,
	};
	py_str(&encode_json_string(&text, ensure_ascii))
}

fn hex_value(ch: char) -> Option<u32> {
	match ch {
		'0'..='9' => Some(ch as u32 - '0' as u32),
		'a'..='f' => Some(ch as u32 - 'a' as u32 + 10),
		'A'..='F' => Some(ch as u32 - 'A' as u32 + 10),
		_ => None,
	}
}

fn decode_u_escape(chars: &[char], pos: usize) -> Result<u32, String> {
	if pos + 4 > chars.len() {
		return Err("Invalid \\uXXXX escape".to_owned());
	}
	let mut value = 0u32;
	for &ch in &chars[pos..pos + 4] {
		let Some(digit) = hex_value(ch) else {
			return Err("Invalid \\uXXXX escape".to_owned());
		};
		value = (value << 4) | digit;
	}
	Ok(value)
}

fn push_codepoint(out: &mut String, codepoint: u32) {
	if let Some(ch) = char::from_u32(codepoint) {
		out.push(ch);
	} else {
		// Pon's `str` payload is UTF-8, so lone surrogate code points cannot be
		// represented directly.  Valid surrogate pairs are combined before this
		// path; malformed lone surrogates degrade to the Unicode replacement
		// character rather than fabricating invalid UTF-8.
		out.push('\u{fffd}');
	}
}

fn scan_json_string(text: &str, end: usize, strict: bool) -> Result<(String, usize), String> {
	let chars: Vec<char> = text.chars().collect();
	let begin = end.saturating_sub(1);
	let mut index = end;
	let mut out = String::new();
	while index < chars.len() {
		let ch = chars[index];
		match ch {
			'"' => return Ok((out, index + 1)),
			'\\' => {
				index += 1;
				if index >= chars.len() {
					return Err("Unterminated string starting at".to_owned());
				}
				match chars[index] {
					'"' => out.push('"'),
					'\\' => out.push('\\'),
					'/' => out.push('/'),
					'b' => out.push('\u{08}'),
					'f' => out.push('\u{0c}'),
					'n' => out.push('\n'),
					'r' => out.push('\r'),
					't' => out.push('\t'),
					'u' => {
						let high = decode_u_escape(&chars, index + 1)?;
						if (0xd800..=0xdbff).contains(&high)
							&& index + 10 < chars.len()
							&& chars[index + 5] == '\\'
							&& chars[index + 6] == 'u'
						{
							if let Ok(low) = decode_u_escape(&chars, index + 7) {
								if (0xdc00..=0xdfff).contains(&low) {
									let codepoint = 0x1_0000 + (((high - 0xd800) << 10) | (low - 0xdc00));
									push_codepoint(&mut out, codepoint);
									index += 10;
								} else {
									push_codepoint(&mut out, high);
									index += 4;
								}
							} else {
								push_codepoint(&mut out, high);
								index += 4;
							}
						} else {
							push_codepoint(&mut out, high);
							index += 4;
						}
					},
					other => return Err(format!("Invalid \\escape: {other:?}")),
				}
				index += 1;
			},
			ch if ch <= '\u{1f}' => {
				if strict {
					return Err(format!("Invalid control character {ch:?} at"));
				}
				out.push(ch);
				index += 1;
			},
			ch => {
				out.push(ch);
				index += 1;
			},
		}
	}
	let _ = begin;
	Err("Unterminated string starting at".to_owned())
}

unsafe extern "C" fn json_scanstring(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { argv_slice(argv, argc) } {
		Some(args) => args,
		None => return type_error("scanstring() received a null argument vector"),
	};
	if !(2..=3).contains(&args.len()) {
		return type_error(&format!("scanstring expected 2 or 3 arguments, got {}", args.len()));
	}
	let text = match str_arg(args[0], "s") {
		Ok(text) => text,
		Err(error) => return error,
	};
	let end = match int_arg(args[1], "end") {
		Ok(value) if value >= 0 => value as usize,
		Ok(_) => return value_error("end is out of bounds"),
		Err(error) => return error,
	};
	let strict = if let Some(&strict) = args.get(2) {
		match truth_arg(strict, "strict") {
			Ok(value) => value,
			Err(error) => return error,
		}
	} else {
		true
	};
	let (decoded, next) = match scan_json_string(&text, end, strict) {
		Ok(result) => result,
		Err(message) => return value_error(&message),
	};
	let decoded = py_str(&decoded);
	if decoded.is_null() {
		return ptr::null_mut();
	}
	let next = py_int(next as i64);
	if next.is_null() {
		return ptr::null_mut();
	}
	tuple2(decoded, next)
}

unsafe extern "C" fn json_make_scanner(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { argv_slice(argv, argc) } {
		Some(args) => args,
		None => return type_error("make_scanner() received a null argument vector"),
	};
	if args.len() != 1 {
		return type_error(&format!("make_scanner expected 1 argument, got {}", args.len()));
	}
	let py_make_scanner = match import_module_attr("json.scanner", "py_make_scanner") {
		Ok(function) => function,
		Err(message) => return abi::return_null_with_error(message),
	};
	let mut call_args = [crate::tag::untag_arg(args[0])];
	unsafe { pon_call(py_make_scanner, call_args.as_mut_ptr(), call_args.len()) }
}

unsafe extern "C" fn json_make_encoder(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { argv_slice(argv, argc) } {
		Some(args) => args,
		None => return type_error("make_encoder() received a null argument vector"),
	};
	if args.len() != 9 {
		return type_error(&format!("make_encoder expected 9 arguments, got {}", args.len()));
	}
	let allow_nan = match truth_arg(args[8], "allow_nan") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let make_iterencode = match import_module_attr("json.encoder", "_make_iterencode") {
		Ok(function) => function,
		Err(message) => return abi::return_null_with_error(message),
	};
	let floatstr = alloc_json_floatstr(allow_nan);
	if floatstr.is_null() {
		return ptr::null_mut();
	}
	let one_shot = py_bool(true);
	if one_shot.is_null() {
		return ptr::null_mut();
	}
	let mut call_args = [
		crate::tag::untag_arg(args[0]),
		crate::tag::untag_arg(args[1]),
		crate::tag::untag_arg(args[2]),
		crate::tag::untag_arg(args[3]),
		floatstr,
		crate::tag::untag_arg(args[4]),
		crate::tag::untag_arg(args[5]),
		crate::tag::untag_arg(args[6]),
		crate::tag::untag_arg(args[7]),
		one_shot,
	];
	unsafe { pon_call(make_iterencode, call_args.as_mut_ptr(), call_args.len()) }
}

pub(super) fn make_json_module() -> Result<*mut PyObject, String> {
	install_module("_json", vec![
		module_name_attr("_json")?,
		function_attr("encode_basestring", json_encode_basestring)?,
		function_attr("encode_basestring_ascii", json_encode_basestring_ascii)?,
		function_attr("scanstring", json_scanstring)?,
		function_attr("make_encoder", json_make_encoder)?,
		function_attr("make_scanner", json_make_scanner)?,
	])
}

// ---------------------------------------------------------------------------
// `_locale`

#[cfg(any(target_os = "macos", target_os = "linux"))]
const LANGINFO_CONSTANTS: &[(&str, libc::nl_item)] = &[
	("ABDAY_1", libc::ABDAY_1),
	("ABDAY_2", libc::ABDAY_2),
	("ABDAY_3", libc::ABDAY_3),
	("ABDAY_4", libc::ABDAY_4),
	("ABDAY_5", libc::ABDAY_5),
	("ABDAY_6", libc::ABDAY_6),
	("ABDAY_7", libc::ABDAY_7),
	("ABMON_1", libc::ABMON_1),
	("ABMON_2", libc::ABMON_2),
	("ABMON_3", libc::ABMON_3),
	("ABMON_4", libc::ABMON_4),
	("ABMON_5", libc::ABMON_5),
	("ABMON_6", libc::ABMON_6),
	("ABMON_7", libc::ABMON_7),
	("ABMON_8", libc::ABMON_8),
	("ABMON_9", libc::ABMON_9),
	("ABMON_10", libc::ABMON_10),
	("ABMON_11", libc::ABMON_11),
	("ABMON_12", libc::ABMON_12),
	("ALT_DIGITS", libc::ALT_DIGITS),
	("AM_STR", libc::AM_STR),
	("CODESET", libc::CODESET),
	("CRNCYSTR", libc::CRNCYSTR),
	("DAY_1", libc::DAY_1),
	("DAY_2", libc::DAY_2),
	("DAY_3", libc::DAY_3),
	("DAY_4", libc::DAY_4),
	("DAY_5", libc::DAY_5),
	("DAY_6", libc::DAY_6),
	("DAY_7", libc::DAY_7),
	("D_FMT", libc::D_FMT),
	("D_T_FMT", libc::D_T_FMT),
	("ERA", libc::ERA),
	("ERA_D_FMT", libc::ERA_D_FMT),
	("ERA_D_T_FMT", libc::ERA_D_T_FMT),
	("ERA_T_FMT", libc::ERA_T_FMT),
	("MON_1", libc::MON_1),
	("MON_2", libc::MON_2),
	("MON_3", libc::MON_3),
	("MON_4", libc::MON_4),
	("MON_5", libc::MON_5),
	("MON_6", libc::MON_6),
	("MON_7", libc::MON_7),
	("MON_8", libc::MON_8),
	("MON_9", libc::MON_9),
	("MON_10", libc::MON_10),
	("MON_11", libc::MON_11),
	("MON_12", libc::MON_12),
	("NOEXPR", libc::NOEXPR),
	("PM_STR", libc::PM_STR),
	("RADIXCHAR", libc::RADIXCHAR),
	("THOUSEP", libc::THOUSEP),
	("T_FMT", libc::T_FMT),
	("T_FMT_AMPM", libc::T_FMT_AMPM),
	("YESEXPR", libc::YESEXPR),
];

fn c_string(ptr: *const libc::c_char) -> String {
	if ptr.is_null() {
		String::new()
	} else {
		unsafe { CStr::from_ptr(ptr) }
			.to_string_lossy()
			.into_owned()
	}
}

fn cstring_arg(text: &str, what: &str) -> Result<CString, *mut PyObject> {
	CString::new(text).map_err(|_| value_error(&format!("embedded null character in {what}")))
}

fn locale_error(message: &str) -> *mut PyObject {
	value_error(message)
}

fn category_arg(object: *mut PyObject) -> Result<libc::c_int, *mut PyObject> {
	let value = int_arg(object, "category")?;
	libc::c_int::try_from(value).map_err(|_| value_error("invalid locale category"))
}

unsafe extern "C" fn locale_setlocale(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { argv_slice(argv, argc) } {
		Some(args) => args,
		None => return type_error("setlocale() received a null argument vector"),
	};
	if !(1..=2).contains(&args.len()) {
		return type_error(&format!("setlocale expected 1 or 2 arguments, got {}", args.len()));
	}
	let category = match category_arg(args[0]) {
		Ok(category) => category,
		Err(error) => return error,
	};
	let result = if args.len() == 1 || is_none(args[1]) {
		unsafe { libc::setlocale(category, ptr::null()) }
	} else {
		let locale = match str_arg(args[1], "locale") {
			Ok(locale) => locale,
			Err(error) => return error,
		};
		let c_locale = match cstring_arg(&locale, "locale") {
			Ok(locale) => locale,
			Err(error) => return error,
		};
		unsafe { libc::setlocale(category, c_locale.as_ptr()) }
	};
	if result.is_null() {
		return locale_error("unsupported locale setting");
	}
	py_str(&c_string(result))
}

fn grouping_list(ptr: *const libc::c_char) -> *mut PyObject {
	let mut values = Vec::new();
	if !ptr.is_null() {
		let mut index = 0usize;
		loop {
			let value = unsafe { *ptr.add(index) };
			if value == 0 {
				break;
			}
			values.push(i64::from(value));
			if value == 127 {
				break;
			}
			index += 1;
			if index > 64 {
				break;
			}
		}
	}
	list_from_ints(&values)
}

fn char_field(value: libc::c_char) -> *mut PyObject {
	py_int(i64::from(value))
}

unsafe extern "C" fn locale_localeconv(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 || !argv.is_null() {
		return type_error(&format!("localeconv expected no arguments, got {argc}"));
	}
	let raw = unsafe { libc::localeconv() };
	if raw.is_null() {
		return locale_error("localeconv failed");
	}
	let conv = unsafe { &*raw };
	dict_from_pairs(vec![
		("decimal_point", py_str(&c_string(conv.decimal_point))),
		("thousands_sep", py_str(&c_string(conv.thousands_sep))),
		("grouping", grouping_list(conv.grouping)),
		("int_curr_symbol", py_str(&c_string(conv.int_curr_symbol))),
		("currency_symbol", py_str(&c_string(conv.currency_symbol))),
		("mon_decimal_point", py_str(&c_string(conv.mon_decimal_point))),
		("mon_thousands_sep", py_str(&c_string(conv.mon_thousands_sep))),
		("mon_grouping", grouping_list(conv.mon_grouping)),
		("positive_sign", py_str(&c_string(conv.positive_sign))),
		("negative_sign", py_str(&c_string(conv.negative_sign))),
		("int_frac_digits", char_field(conv.int_frac_digits)),
		("frac_digits", char_field(conv.frac_digits)),
		("p_cs_precedes", char_field(conv.p_cs_precedes)),
		("p_sep_by_space", char_field(conv.p_sep_by_space)),
		("n_cs_precedes", char_field(conv.n_cs_precedes)),
		("n_sep_by_space", char_field(conv.n_sep_by_space)),
		("p_sign_posn", char_field(conv.p_sign_posn)),
		("n_sign_posn", char_field(conv.n_sign_posn)),
	])
}

unsafe extern "C" fn locale_strcoll(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { argv_slice(argv, argc) } {
		Some(args) => args,
		None => return type_error("strcoll() received a null argument vector"),
	};
	if args.len() != 2 {
		return type_error(&format!("strcoll expected 2 arguments, got {}", args.len()));
	}
	let left = match str_arg(args[0], "a") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let right = match str_arg(args[1], "b") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let left = match cstring_arg(&left, "a") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let right = match cstring_arg(&right, "b") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let result = unsafe { libc::strcoll(left.as_ptr(), right.as_ptr()) };
	py_int(i64::from(result.signum()))
}

unsafe extern "C" fn locale_strxfrm(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { argv_slice(argv, argc) } {
		Some(args) => args,
		None => return type_error("strxfrm() received a null argument vector"),
	};
	if args.len() != 1 {
		return type_error(&format!("strxfrm expected 1 argument, got {}", args.len()));
	}
	let text = match str_arg(args[0], "s") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let input = match cstring_arg(&text, "s") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let needed = unsafe { libc::strxfrm(ptr::null_mut(), input.as_ptr(), 0) };
	let mut buffer = vec![0 as libc::c_char; needed.saturating_add(1)];
	unsafe { libc::strxfrm(buffer.as_mut_ptr(), input.as_ptr(), buffer.len()) };
	py_str(&c_string(buffer.as_ptr()))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
unsafe extern "C" fn locale_nl_langinfo(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { argv_slice(argv, argc) } {
		Some(args) => args,
		None => return type_error("nl_langinfo() received a null argument vector"),
	};
	if args.len() != 1 {
		return type_error(&format!("nl_langinfo expected 1 argument, got {}", args.len()));
	}
	let item = match int_arg(args[0], "item") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let item = item as libc::nl_item;
	let value = unsafe { libc::nl_langinfo(item) };
	if value.is_null() {
		return value_error("unsupported langinfo constant");
	}
	py_str(&c_string(value))
}

unsafe extern "C" fn locale_getencoding(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 || !argv.is_null() {
		return type_error(&format!("getencoding expected no arguments, got {argc}"));
	}
	#[cfg(any(target_os = "macos", target_os = "linux"))]
	{
		let value = unsafe { libc::nl_langinfo(libc::CODESET) };
		let text = c_string(value);
		if !text.is_empty() {
			return py_str(&text);
		}
	}
	py_str("UTF-8")
}

pub(super) fn make_locale_module() -> Result<*mut PyObject, String> {
	let mut attrs = vec![
		module_name_attr("_locale")?,
		int_attr("CHAR_MAX", 127)?,
		int_attr("LC_ALL", libc::LC_ALL as i64)?,
		int_attr("LC_COLLATE", libc::LC_COLLATE as i64)?,
		int_attr("LC_CTYPE", libc::LC_CTYPE as i64)?,
		int_attr("LC_MONETARY", libc::LC_MONETARY as i64)?,
		int_attr("LC_NUMERIC", libc::LC_NUMERIC as i64)?,
		int_attr("LC_TIME", libc::LC_TIME as i64)?,
		function_attr("getencoding", locale_getencoding)?,
		function_attr("localeconv", locale_localeconv)?,
		function_attr("setlocale", locale_setlocale)?,
		function_attr("strcoll", locale_strcoll)?,
		function_attr("strxfrm", locale_strxfrm)?,
	];
	#[cfg(any(target_os = "macos", target_os = "linux"))]
	{
		attrs.push(function_attr("nl_langinfo", locale_nl_langinfo)?);
		for &(name, value) in LANGINFO_CONSTANTS {
			attrs.push(int_attr(name, value as i64)?);
		}
	}
	#[cfg(any(target_os = "macos", target_os = "linux"))]
	{
		attrs.push(int_attr("LC_MESSAGES", libc::LC_MESSAGES as i64)?);
	}
	let value_error = unsafe { abi::pon_load_global(intern("ValueError"), ptr::null_mut()) };
	if value_error.is_null() {
		return Err("builtin ValueError is not registered".to_owned());
	}
	attrs.push((intern("Error"), value_error));
	install_module("_locale", attrs)
}
