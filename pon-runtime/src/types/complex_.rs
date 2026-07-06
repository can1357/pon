//! Complex numeric tower implementation.

use core::{ffi::c_int, ptr};
use std::sync::LazyLock;

use num_traits::ToPrimitive;

use crate::{
	object::{PyNumberMethods, PyObject, PyObjectHeader, PyType},
	types::method,
};

#[repr(C)]
#[derive(Debug)]
pub struct PyComplex {
	/// Common object header; this field must remain first.
	pub ob_base: PyObjectHeader,
	/// Real component.
	pub real:    f64,
	/// Imaginary component.
	pub imag:    f64,
}

static COMPLEX_NUMBER_METHODS: LazyLock<usize> =
	LazyLock::new(|| Box::into_raw(Box::new(make_number_methods())) as usize);
static COMPLEX_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(ptr::null(), "complex", core::mem::size_of::<PyComplex>());
	ty.tp_hash = Some(hash_slot);
	ty.tp_bool = Some(bool_slot);
	ty.tp_as_number = number_methods_ptr();
	ty.tp_getattro = Some(complex_getattro);
	Box::into_raw(Box::new(ty)) as usize
});

/// Returns the process-wide `complex` type object (C-API twin registration).
#[must_use]
pub fn complex_type() -> *mut PyType {
	*COMPLEX_TYPE as *mut PyType
}

/// Boxes a pair of IEEE-754 doubles as a Python `complex`.
#[must_use]
pub fn from_f64s(real: f64, imag: f64) -> *mut PyObject {
	Box::into_raw(Box::new(PyComplex {
		ob_base: PyObjectHeader::new(*COMPLEX_TYPE as *const PyType),
		real,
		imag,
	})) as *mut PyObject
}

/// Returns true for exact `complex` objects.
#[must_use]
pub unsafe fn is_exact_complex(object: *mut PyObject) -> bool {
	unsafe { crate::types::int::type_name_is(object, "complex") }
}

/// Extracts a complex payload from an exact `complex`.
#[must_use]
pub unsafe fn to_f64s(object: *mut PyObject) -> Option<(f64, f64)> {
	if unsafe { is_exact_complex(object) } {
		let value = unsafe { &*object.cast::<PyComplex>() };
		Some((value.real, value.imag))
	} else {
		None
	}
}

/// Implements the built-in `complex()` constructor once the builtin shim has
/// sliced argv.
#[must_use]
pub fn construct_from_args(args: &[*mut PyObject]) -> *mut PyObject {
	match args.len() {
		0 => from_f64s(0.0, 0.0),
		1 => unsafe { construct_one(args[0]) },
		2 => unsafe { construct_two(args[0], args[1]) },
		len => raise_type_error(&format!("complex() expected at most 2 arguments, got {len}")),
	}
}

/// CPython-compatible complex hash composition.
#[must_use]
pub fn hash_complex(real: f64, imag: f64) -> isize {
	const HASH_IMAG: isize = 1_000_003;
	let hash = crate::types::float::hash_f64(real)
		.wrapping_add(HASH_IMAG.wrapping_mul(crate::types::float::hash_f64(imag)));
	if hash == -1 { -2 } else { hash }
}

unsafe fn construct_one(object: *mut PyObject) -> *mut PyObject {
	if let Some(text) = unsafe { crate::types::type_::unicode_text(object) } {
		return match parse_complex_text(text) {
			Ok((real, imag)) => from_f64s(real, imag),
			Err(message) => raise_value_error(message),
		};
	}
	match unsafe { object_to_complex_parts(object) } {
		Some((real, imag)) => from_f64s(real, imag),
		None => raise_type_error("complex() first argument must be a number"),
	}
}

unsafe fn construct_two(real_object: *mut PyObject, imag_object: *mut PyObject) -> *mut PyObject {
	if unsafe { crate::types::type_::unicode_text(real_object).is_some() } {
		return raise_type_error("complex() argument 'real' must be a real number, not str");
	}
	if unsafe { crate::types::type_::unicode_text(imag_object).is_some() } {
		return raise_type_error("complex() argument 'imag' must be a real number, not str");
	}
	let Some((real_real, real_imag)) = (unsafe { object_to_complex_parts(real_object) }) else {
		return raise_type_error("complex() first argument must be a number");
	};
	let Some((imag_real, imag_imag)) = (unsafe { object_to_complex_parts(imag_object) }) else {
		return raise_type_error("complex() second argument must be a real number");
	};
	let imag = real_imag + imag_real;
	let imag = if imag == 0.0 && real_imag == 0.0 {
		imag_real
	} else {
		imag
	};
	from_f64s(real_real - imag_imag, imag)
}

unsafe fn object_to_complex_parts(object: *mut PyObject) -> Option<(f64, f64)> {
	if let Some(value) = unsafe { to_f64s(object) } {
		return Some(value);
	}
	if let Some(value) = unsafe { crate::types::float::to_f64(object) } {
		return Some((value, 0.0));
	}
	let value = unsafe { crate::types::int::to_bigint_including_bool(object)? };
	value.to_f64().map(|value| (value, 0.0))
}

fn parse_complex_text(text: &str) -> Result<(f64, f64), &'static str> {
	let mut s = text.trim();
	if s.starts_with('(') || s.ends_with(')') {
		let Some(inner) = s
			.strip_prefix('(')
			.and_then(|value| value.strip_suffix(')'))
		else {
			return Err("complex() arg is a malformed string");
		};
		s = inner.trim();
	}
	if s.is_empty() || s.chars().any(char::is_whitespace) {
		return Err("complex() arg is a malformed string");
	}

	if let Some(no_j) = s.strip_suffix('j').or_else(|| s.strip_suffix('J')) {
		if let Some(split) = complex_separator(no_j) {
			let real = parse_float_token(&no_j[..split])?;
			let imag = parse_imag_token(&no_j[split..])?;
			Ok((real, imag))
		} else {
			Ok((0.0, parse_imag_token(no_j)?))
		}
	} else {
		Ok((parse_float_token(s)?, 0.0))
	}
}

fn complex_separator(text: &str) -> Option<usize> {
	let bytes = text.as_bytes();
	for index in (1..bytes.len()).rev() {
		if (bytes[index] == b'+' || bytes[index] == b'-') && !matches!(bytes[index - 1], b'e' | b'E')
		{
			return Some(index);
		}
	}
	None
}

fn parse_imag_token(token: &str) -> Result<f64, &'static str> {
	match token {
		"" | "+" => Ok(1.0),
		"-" => Ok(-1.0),
		_ => parse_float_token(token),
	}
}

fn parse_float_token(token: &str) -> Result<f64, &'static str> {
	let lower = token.to_ascii_lowercase();
	match lower.as_str() {
		"inf" | "+inf" | "infinity" | "+infinity" => Ok(f64::INFINITY),
		"-inf" | "-infinity" => Ok(f64::NEG_INFINITY),
		"nan" | "+nan" | "-nan" => Ok(f64::NAN),
		_ => token
			.parse::<f64>()
			.map_err(|_| "complex() arg is a malformed string"),
	}
}

#[must_use]
pub fn repr_complex(real: f64, imag: f64) -> String {
	if real == 0.0 && !real.is_sign_negative() {
		return format!("{}j", repr_part(imag));
	}
	let sign = if imag.is_sign_negative() { "-" } else { "+" };
	format!("({}{sign}{}j)", repr_part(real), repr_part(imag.abs()))
}

fn repr_part(value: f64) -> String {
	let mut text = crate::types::float::repr_f64(value);
	if text.ends_with(".0") {
		text.truncate(text.len() - 2);
	}
	text
}

/// Returns the complex protocol slot table.
#[must_use]
pub fn number_methods_ptr() -> *mut PyNumberMethods {
	*COMPLEX_NUMBER_METHODS as *mut PyNumberMethods
}

unsafe extern "C" fn complex_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let Some(name) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		return raise_type_error("complex attribute name must be str");
	};
	let Some((real, imag)) = (unsafe { to_f64s(object) }) else {
		return raise_type_error("complex attribute receiver is invalid");
	};
	match name {
		"real" => crate::types::float::from_f64(real),
		"imag" => crate::types::float::from_f64(imag),
		"conjugate" => bound_complex_method(object, "conjugate", complex_conjugate_entry),
		_ => unsafe { crate::abi::pon_raise_attribute_error(object, crate::intern::intern(name)) },
	}
}

fn bound_complex_method(
	receiver: *mut PyObject,
	name: &str,
	entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> *mut PyObject {
	let function =
		unsafe { crate::abi::pon_make_function(entry as *const u8, 1, crate::intern::intern(name)) };
	if function.is_null() {
		return function;
	}
	match method::new_bound_method(function, receiver) {
		Ok(method) => method.cast::<PyObject>(),
		Err(message) => raise_type_error(&message),
	}
}

unsafe extern "C" fn complex_conjugate_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	if argc != 1 {
		return raise_type_error("complex.conjugate() takes no arguments");
	}
	if argv.is_null() {
		return raise_type_error("complex.conjugate() received a null argv pointer");
	}
	let args = unsafe { core::slice::from_raw_parts(argv, argc) };
	match unsafe { to_f64s(args[0]) } {
		Some((real, imag)) => from_f64s(real, -imag),
		None => raise_type_error("complex attribute receiver is invalid"),
	}
}

unsafe extern "C" fn hash_slot(object: *mut PyObject) -> isize {
	match unsafe { to_f64s(object) } {
		Some((real, imag)) => hash_complex(real, imag),
		None => -1,
	}
}

unsafe extern "C" fn bool_slot(object: *mut PyObject) -> c_int {
	match unsafe { to_f64s(object) } {
		Some((0.0, 0.0)) => 0,
		Some(_) => 1,
		None => -1,
	}
}

unsafe extern "C" fn nb_negative(object: *mut PyObject) -> *mut PyObject {
	match unsafe { to_f64s(object) } {
		Some((real, imag)) => from_f64s(-real, -imag),
		None => raise_type_error("bad operand type for unary -"),
	}
}

unsafe extern "C" fn nb_positive(object: *mut PyObject) -> *mut PyObject {
	match unsafe { to_f64s(object) } {
		Some((real, imag)) => from_f64s(real, imag),
		None => raise_type_error("bad operand type for unary +"),
	}
}

unsafe extern "C" fn nb_absolute(object: *mut PyObject) -> *mut PyObject {
	match unsafe { to_f64s(object) } {
		Some((real, imag)) => crate::types::float::from_f64(real.hypot(imag)),
		None => raise_type_error("bad operand type for abs()"),
	}
}

fn make_number_methods() -> PyNumberMethods {
	PyNumberMethods {
		nb_add: Some(crate::types::int::nb_add),
		nb_subtract: Some(crate::types::int::nb_subtract),
		nb_multiply: Some(crate::types::int::nb_multiply),
		nb_remainder: Some(crate::types::int::nb_remainder),
		nb_power: Some(crate::types::int::nb_power),
		nb_negative: Some(nb_negative),
		nb_positive: Some(nb_positive),
		nb_absolute: Some(nb_absolute),
		nb_bool: Some(bool_slot),
		nb_floor_divide: Some(crate::types::int::nb_floor_divide),
		nb_true_divide: Some(crate::types::int::nb_true_divide),
		nb_reflected_add: Some(crate::types::int::nb_add),
		nb_reflected_subtract: Some(crate::types::int::nb_subtract),
		nb_reflected_multiply: Some(crate::types::int::nb_multiply),
		nb_reflected_remainder: Some(crate::types::int::nb_remainder),
		nb_reflected_power: Some(crate::types::int::nb_power),
		nb_reflected_floor_divide: Some(crate::types::int::nb_floor_divide),
		nb_reflected_true_divide: Some(crate::types::int::nb_true_divide),
		..PyNumberMethods::EMPTY
	}
}

fn raise_type_error(message: &str) -> *mut PyObject {
	unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn raise_value_error(message: &str) -> *mut PyObject {
	unsafe { crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) }
}
