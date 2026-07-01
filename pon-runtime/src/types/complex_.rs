//! Complex numeric tower implementation.

use core::ffi::c_int;
use core::ptr;
use std::sync::LazyLock;

use crate::object::{PyNumberMethods, PyObject, PyObjectHeader, PyType};

#[repr(C)]
#[derive(Debug)]
pub struct PyComplex {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Real component.
    pub real: f64,
    /// Imaginary component.
    pub imag: f64,
}

static COMPLEX_NUMBER_METHODS: LazyLock<usize> =
    LazyLock::new(|| Box::into_raw(Box::new(make_number_methods())) as usize);
static COMPLEX_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(ptr::null(), "complex", core::mem::size_of::<PyComplex>());
    ty.tp_bool = Some(bool_slot);
    ty.tp_as_number = number_methods_ptr();
    ty.tp_getattro = Some(complex_getattro);
    Box::into_raw(Box::new(ty)) as usize
});

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

#[must_use]
pub fn repr_complex(real: f64, imag: f64) -> String {
    if real == 0.0 {
        return format!("{}j", repr_part(imag));
    }
    let sign = if imag.is_sign_negative() { "-" } else { "+" };
    format!("({}{sign}{}j)", repr_part(real), repr_part(imag.abs()))
}

fn repr_part(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value >= i64::MIN as f64 && value <= i64::MAX as f64 {
        (value as i64).to_string()
    } else {
        crate::types::float::repr_f64(value)
    }
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
        _ => unsafe { crate::abi::pon_raise_attribute_error(object, crate::intern::intern(name)) },
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
