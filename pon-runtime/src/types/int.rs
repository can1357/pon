//! Integer numeric tower implementation.

use core::ffi::c_int;
use core::ptr;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use num_bigint::{BigInt, Sign};
use num_traits::{One, Signed, ToPrimitive};

use crate::abi;
use crate::object::{PyLong, PyNumberMethods, PyObject, PyObjectHeader};

static BIG_INTS: LazyLock<Mutex<HashMap<usize, BigInt>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static INT_NUMBER_METHODS: LazyLock<usize> =
    LazyLock::new(|| Box::into_raw(Box::new(make_number_methods())) as usize);

/// Returns true when `object` has a runtime type whose name bytes match `expected`.
#[must_use]
pub unsafe fn type_name_is(object: *mut PyObject, expected: &str) -> bool {
    if object.is_null() {
        return false;
    }
    let ty = unsafe { (*object).ob_type };
    if ty.is_null() {
        return false;
    }
    let ty = unsafe { &*ty };
    if ty.name.is_null() && ty.name_len != 0 {
        return false;
    }
    let name = unsafe { core::slice::from_raw_parts(ty.name, ty.name_len) };
    name == expected.as_bytes()
}

/// Returns true for exact `int` objects, not for `bool`.
#[must_use]
pub unsafe fn is_exact_int(object: *mut PyObject) -> bool {
    unsafe { type_name_is(object, "int") }
}

/// Extracts the arbitrary-precision integer payload for an exact `int`.
#[must_use]
pub unsafe fn to_bigint(object: *mut PyObject) -> Option<BigInt> {
    if !unsafe { is_exact_int(object) } {
        return None;
    }
    let key = object as usize;
    if let Some(value) = BIG_INTS.lock().unwrap_or_else(|poison| poison.into_inner()).get(&key) {
        return Some(value.clone());
    }
    Some(BigInt::from(unsafe { (*object.cast::<PyLong>()).value }))
}

/// Boxes an arbitrary-precision integer as a `PyLong`.
///
/// Values that fit in the Phase-A inline `i64` payload keep using
/// `pon_const_int`, preserving the existing small-integer path. Wider values are
/// represented by a normal `PyLong` shell plus an out-of-line BigInt payload
/// keyed by object address.
#[must_use]
pub fn from_bigint(value: BigInt) -> *mut PyObject {
    if let Some(inline) = value.to_i64() {
        return unsafe { abi::pon_const_int(inline) };
    }
    let template = unsafe { abi::pon_const_int(0) };
    if template.is_null() {
        return template;
    }
    let ty = unsafe { (*template).ob_type };
    let object = Box::into_raw(Box::new(PyLong {
        ob_base: PyObjectHeader::new(ty),
        value: 0,
    })) as *mut PyObject;
    BIG_INTS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(object as usize, value);
    object
}

/// Boxes a signed 64-bit integer through the compatibility constructor.
#[must_use]
pub fn from_i64(value: i64) -> *mut PyObject {
    unsafe { abi::pon_const_int(value) }
}

/// Installs integer slots on the runtime `int` type reached from an object.
pub unsafe fn install_slots_for_object(object: *mut PyObject) {
    if !unsafe { is_exact_int(object) } {
        return;
    }
    let ty = unsafe { (*object).ob_type.cast_mut() };
    if ty.is_null() {
        return;
    }
    unsafe {
        (*ty).tp_hash = Some(hash_slot);
        (*ty).tp_bool = Some(bool_slot);
        (*ty).tp_as_number = number_methods_ptr();
    }
}

/// Returns the integer protocol slot table.
#[must_use]
pub fn number_methods_ptr() -> *mut PyNumberMethods {
    *INT_NUMBER_METHODS as *mut PyNumberMethods
}

/// CPython-style integer hash reduction using the 64-bit `PyHASH_MODULUS`.
#[must_use]
pub fn hash_bigint(value: &BigInt) -> isize {
    const HASH_BITS: usize = 61;
    let modulus = (BigInt::one() << HASH_BITS) - BigInt::one();
    let mut reduced = (value.abs() % &modulus).to_isize().unwrap_or(0);
    if value.sign() == Sign::Minus {
        reduced = -reduced;
    }
    if reduced == -1 { -2 } else { reduced }
}

unsafe extern "C" fn hash_slot(object: *mut PyObject) -> isize {
    match unsafe { to_bigint(object) } {
        Some(value) => hash_bigint(&value),
        None => -1,
    }
}

unsafe extern "C" fn bool_slot(object: *mut PyObject) -> c_int {
    match unsafe { to_bigint(object) } {
        Some(value) if value == BigInt::from(0) => 0,
        Some(_) => 1,
        None => -1,
    }
}

pub unsafe extern "C" fn nb_index(object: *mut PyObject) -> *mut PyObject {
    match unsafe { to_bigint(object) } {
        Some(value) => from_bigint(value),
        None => raise_type_error("object cannot be interpreted as an integer"),
    }
}

pub unsafe extern "C" fn nb_int(object: *mut PyObject) -> *mut PyObject {
    unsafe { nb_index(object) }
}

pub unsafe extern "C" fn nb_float(object: *mut PyObject) -> *mut PyObject {
    match unsafe { to_bigint(object).and_then(|value| value.to_f64()) } {
        Some(value) => crate::types::float::from_f64(value),
        None => raise_type_error("int too large to convert to float"),
    }
}

pub unsafe extern "C" fn nb_add(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_ADD, a, b) }
}

pub unsafe extern "C" fn nb_subtract(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_SUB, a, b) }
}

pub unsafe extern "C" fn nb_multiply(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_MUL, a, b) }
}

pub unsafe extern "C" fn nb_remainder(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_MOD, a, b) }
}

pub unsafe extern "C" fn nb_negative(object: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_unary_op(crate::abstract_op::UNARY_NEG, object, ptr::null_mut()) }
}

pub unsafe extern "C" fn nb_positive(object: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_unary_op(crate::abstract_op::UNARY_POS, object, ptr::null_mut()) }
}

pub unsafe extern "C" fn nb_invert(object: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_unary_op(crate::abstract_op::UNARY_INVERT, object, ptr::null_mut()) }
}

pub unsafe extern "C" fn nb_lshift(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_LSHIFT, a, b) }
}

pub unsafe extern "C" fn nb_rshift(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_RSHIFT, a, b) }
}

pub unsafe extern "C" fn nb_and(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_AND, a, b) }
}

pub unsafe extern "C" fn nb_xor(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_XOR, a, b) }
}

pub unsafe extern "C" fn nb_or(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_OR, a, b) }
}

pub unsafe extern "C" fn nb_floor_divide(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_FLOORDIV, a, b) }
}

pub unsafe extern "C" fn nb_true_divide(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_DIV, a, b) }
}

pub unsafe extern "C" fn nb_power(a: *mut PyObject, b: *mut PyObject, _modulo: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_POW, a, b) }
}

fn make_number_methods() -> PyNumberMethods {
    PyNumberMethods {
        nb_add: Some(nb_add),
        nb_subtract: Some(nb_subtract),
        nb_multiply: Some(nb_multiply),
        nb_remainder: Some(nb_remainder),
        nb_power: Some(nb_power),
        nb_negative: Some(nb_negative),
        nb_positive: Some(nb_positive),
        nb_bool: Some(bool_slot),
        nb_invert: Some(nb_invert),
        nb_lshift: Some(nb_lshift),
        nb_rshift: Some(nb_rshift),
        nb_and: Some(nb_and),
        nb_xor: Some(nb_xor),
        nb_or: Some(nb_or),
        nb_int: Some(nb_int),
        nb_float: Some(nb_float),
        nb_floor_divide: Some(nb_floor_divide),
        nb_true_divide: Some(nb_true_divide),
        nb_index: Some(nb_index),
        nb_reflected_add: Some(nb_add),
        nb_reflected_subtract: Some(nb_subtract),
        nb_reflected_multiply: Some(nb_multiply),
        nb_reflected_remainder: Some(nb_remainder),
        nb_reflected_power: Some(nb_power),
        nb_reflected_lshift: Some(nb_lshift),
        nb_reflected_rshift: Some(nb_rshift),
        nb_reflected_and: Some(nb_and),
        nb_reflected_xor: Some(nb_xor),
        nb_reflected_or: Some(nb_or),
        nb_reflected_floor_divide: Some(nb_floor_divide),
        nb_reflected_true_divide: Some(nb_true_divide),
        ..PyNumberMethods::EMPTY
    }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}
