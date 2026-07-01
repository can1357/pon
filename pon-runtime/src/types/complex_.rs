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

/// Returns the complex protocol slot table.
#[must_use]
pub fn number_methods_ptr() -> *mut PyNumberMethods {
    *COMPLEX_NUMBER_METHODS as *mut PyNumberMethods
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
