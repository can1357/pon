//! Float numeric tower implementation.

use core::ffi::c_int;
use core::ptr;
use std::sync::LazyLock;

use crate::object::{PyNumberMethods, PyObject, PyObjectHeader, PyType};

#[repr(C)]
#[derive(Debug)]
pub struct PyFloat {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// IEEE-754 double payload.
    pub value: f64,
}

static FLOAT_NUMBER_METHODS: LazyLock<usize> =
    LazyLock::new(|| Box::into_raw(Box::new(make_number_methods())) as usize);
static FLOAT_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(ptr::null(), "float", core::mem::size_of::<PyFloat>());
    ty.tp_hash = Some(hash_slot);
    ty.tp_bool = Some(bool_slot);
    ty.tp_as_number = number_methods_ptr();
    Box::into_raw(Box::new(ty)) as usize
});

/// Boxes an IEEE-754 double as a Python `float`.
#[must_use]
pub fn from_f64(value: f64) -> *mut PyObject {
    Box::into_raw(Box::new(PyFloat {
        ob_base: PyObjectHeader::new(*FLOAT_TYPE as *const PyType),
        value,
    })) as *mut PyObject
}

/// Returns true for exact `float` objects.
#[must_use]
pub unsafe fn is_exact_float(object: *mut PyObject) -> bool {
    unsafe { crate::types::int::type_name_is(object, "float") }
}

/// Extracts a float payload from an exact `float`.
#[must_use]
pub unsafe fn to_f64(object: *mut PyObject) -> Option<f64> {
    if unsafe { is_exact_float(object) } {
        Some(unsafe { (*object.cast::<PyFloat>()).value })
    } else {
        None
    }
}

/// Formats an `f64` using CPython's `repr(float)` rules.
///
/// CPython prints the shortest decimal string that round-trips to the same
/// double, choosing fixed notation for `-4 < decpt <= 16` and scientific
/// notation otherwise, where `decpt` is the position of the decimal point
/// relative to the significant digits. Rust's `{:e}` supplies the same shortest
/// round-trip digits; this function only applies CPython's placement and
/// exponent-padding rules so `print(x)` and `repr(x)` match the reference
/// interpreter (e.g. `2.0`, `-0.0`, `1e-05`, `1e+16`, `nan`, `inf`).
#[must_use]
pub fn repr_f64(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-inf".to_owned() } else { "inf".to_owned() };
    }

    let negative = value.is_sign_negative();
    let formatted = format!("{:e}", value.abs());
    let (mantissa, exp) = formatted.split_once('e').expect("{:e} always emits an exponent");
    let exp: i32 = exp.parse().expect("{:e} exponent is a valid integer");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let body = place_decimal(&digits, exp + 1);
    if negative { format!("-{body}") } else { body }
}

/// Places the decimal point (or renders scientific notation) for shortest
/// round-trip `digits` whose decimal point sits after `decpt` significant digits.
fn place_decimal(digits: &str, decpt: i32) -> String {
    let len = digits.len() as i32;
    if decpt <= -4 || decpt > 16 {
        let sci_exp = decpt - 1;
        let mut out = String::with_capacity(digits.len() + 5);
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if sci_exp < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", sci_exp.unsigned_abs()));
        out
    } else if decpt <= 0 {
        format!("0.{}{}", "0".repeat((-decpt) as usize), digits)
    } else if decpt >= len {
        format!("{}{}.0", digits, "0".repeat((decpt - len) as usize))
    } else {
        let split = decpt as usize;
        format!("{}.{}", &digits[..split], &digits[split..])
    }
}

/// Returns the float protocol slot table.
#[must_use]
pub fn number_methods_ptr() -> *mut PyNumberMethods {
    *FLOAT_NUMBER_METHODS as *mut PyNumberMethods
}

unsafe extern "C" fn hash_slot(object: *mut PyObject) -> isize {
    match unsafe { to_f64(object) } {
        Some(value) => {
            if value == 0.0 {
                0
            } else if value.is_finite() {
                let hash = value.to_bits() as isize;
                if hash == -1 { -2 } else { hash }
            } else {
                let hash = value.to_bits() as isize;
                if hash == -1 { -2 } else { hash }
            }
        }
        None => -1,
    }
}

unsafe extern "C" fn bool_slot(object: *mut PyObject) -> c_int {
    match unsafe { to_f64(object) } {
        Some(0.0) => 0,
        Some(_) => 1,
        None => -1,
    }
}

unsafe extern "C" fn nb_float(object: *mut PyObject) -> *mut PyObject {
    match unsafe { to_f64(object) } {
        Some(value) => from_f64(value),
        None => raise_type_error("float expected"),
    }
}

unsafe extern "C" fn nb_negative(object: *mut PyObject) -> *mut PyObject {
    match unsafe { to_f64(object) } {
        Some(value) => from_f64(-value),
        None => raise_type_error("bad operand type for unary -"),
    }
}

unsafe extern "C" fn nb_positive(object: *mut PyObject) -> *mut PyObject {
    unsafe { nb_float(object) }
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
        nb_bool: Some(bool_slot),
        nb_float: Some(nb_float),
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
