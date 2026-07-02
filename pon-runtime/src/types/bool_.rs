//! Boolean singleton implementation.

use core::ffi::c_int;
use core::ptr;
use std::sync::LazyLock;

use crate::object::{PyNumberMethods, PyObject, PyObjectHeader, PyType};

#[repr(C)]
#[derive(Debug)]
pub struct PyBool {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Canonical truth value.
    pub value: bool,
}

static BOOL_NUMBER_METHODS: LazyLock<usize> =
    LazyLock::new(|| Box::into_raw(Box::new(make_number_methods())) as usize);
static BOOL_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(ptr::null(), "bool", core::mem::size_of::<PyBool>());
    ty.tp_hash = Some(hash_slot);
    ty.tp_bool = Some(bool_slot);
    ty.tp_as_number = number_methods_ptr();
    Box::into_raw(Box::new(ty)) as usize
});
static BOOL_FALSE: LazyLock<usize> = LazyLock::new(|| Box::into_raw(Box::new(make_bool(false))) as usize);
static BOOL_TRUE: LazyLock<usize> = LazyLock::new(|| Box::into_raw(Box::new(make_bool(true))) as usize);

/// Returns the process-wide `False` or `True` singleton.
#[must_use]
pub fn from_bool(value: bool) -> *mut PyObject {
    if value {
        *BOOL_TRUE as *mut PyObject
    } else {
        *BOOL_FALSE as *mut PyObject
    }
}

/// Returns true for exact `bool` objects.
#[must_use]
pub unsafe fn is_exact_bool(object: *mut PyObject) -> bool {
    unsafe { crate::types::int::type_name_is(object, "bool") }
}

/// Extracts a bool payload from an exact `bool`.
#[must_use]
pub unsafe fn to_bool(object: *mut PyObject) -> Option<bool> {
    if unsafe { is_exact_bool(object) } {
        Some(unsafe { (*object.cast::<PyBool>()).value })
    } else {
        None
    }
}

/// Returns the bool protocol slot table.
#[must_use]
pub fn number_methods_ptr() -> *mut PyNumberMethods {
    *BOOL_NUMBER_METHODS as *mut PyNumberMethods
}

fn make_bool(value: bool) -> PyBool {
    PyBool {
        ob_base: PyObjectHeader::new(*BOOL_TYPE as *const PyType),
        value,
    }
}

unsafe extern "C" fn hash_slot(object: *mut PyObject) -> isize {
    match unsafe { to_bool(object) } {
        Some(false) => 0,
        Some(true) => 1,
        None => -1,
    }
}

unsafe extern "C" fn bool_slot(object: *mut PyObject) -> c_int {
    match unsafe { to_bool(object) } {
        Some(false) => 0,
        Some(true) => 1,
        None => -1,
    }
}

unsafe extern "C" fn nb_index(object: *mut PyObject) -> *mut PyObject {
    match unsafe { to_bool(object) } {
        Some(false) => crate::types::int::from_i64(0),
        Some(true) => crate::types::int::from_i64(1),
        None => raise_type_error("object cannot be interpreted as an integer"),
    }
}

unsafe extern "C" fn nb_int(object: *mut PyObject) -> *mut PyObject {
    unsafe { nb_index(object) }
}

unsafe extern "C" fn nb_float(object: *mut PyObject) -> *mut PyObject {
    match unsafe { to_bool(object) } {
        Some(false) => crate::types::float::from_f64(0.0),
        Some(true) => crate::types::float::from_f64(1.0),
        None => raise_type_error("bool expected"),
    }
}

unsafe extern "C" fn nb_negative(object: *mut PyObject) -> *mut PyObject {
    match unsafe { to_bool(object) } {
        Some(false) => crate::types::int::from_i64(0),
        Some(true) => crate::types::int::from_i64(-1),
        None => raise_type_error("bad operand type for unary -"),
    }
}

unsafe extern "C" fn nb_positive(object: *mut PyObject) -> *mut PyObject {
    unsafe { nb_index(object) }
}

unsafe extern "C" fn nb_absolute(object: *mut PyObject) -> *mut PyObject {
    unsafe { nb_index(object) }
}

unsafe extern "C" fn nb_invert(object: *mut PyObject) -> *mut PyObject {
    match unsafe { to_bool(object) } {
        Some(false) => crate::types::int::from_i64(-1),
        Some(true) => crate::types::int::from_i64(-2),
        None => raise_type_error("bad operand type for unary ~"),
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
        nb_invert: Some(nb_invert),
        nb_lshift: Some(crate::types::int::nb_lshift),
        nb_rshift: Some(crate::types::int::nb_rshift),
        nb_and: Some(crate::types::int::nb_and),
        nb_xor: Some(crate::types::int::nb_xor),
        nb_or: Some(crate::types::int::nb_or),
        nb_int: Some(nb_int),
        nb_float: Some(nb_float),
        nb_floor_divide: Some(crate::types::int::nb_floor_divide),
        nb_true_divide: Some(crate::types::int::nb_true_divide),
        nb_index: Some(nb_index),
        nb_reflected_add: Some(crate::types::int::nb_add),
        nb_reflected_subtract: Some(crate::types::int::nb_subtract),
        nb_reflected_multiply: Some(crate::types::int::nb_multiply),
        nb_reflected_remainder: Some(crate::types::int::nb_remainder),
        nb_reflected_power: Some(crate::types::int::nb_power),
        nb_reflected_lshift: Some(crate::types::int::nb_lshift),
        nb_reflected_rshift: Some(crate::types::int::nb_rshift),
        nb_reflected_and: Some(crate::types::int::nb_and),
        nb_reflected_xor: Some(crate::types::int::nb_xor),
        nb_reflected_or: Some(crate::types::int::nb_or),
        nb_reflected_floor_divide: Some(crate::types::int::nb_floor_divide),
        nb_reflected_true_divide: Some(crate::types::int::nb_true_divide),
        ..PyNumberMethods::EMPTY
    }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}
