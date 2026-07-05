//! Numbers family: int/bool/float/complex construction and extraction.

use core::ffi::{c_double, c_int, c_long, c_longlong, c_ulong, c_ulonglong, c_void};
use core::ptr;

use num_bigint::{BigInt, Sign};
use num_traits::{One, ToPrimitive};

use crate::abi;
use crate::object::{PyObject, PyType};
use crate::types::exc::ExceptionKind;

use super::twin::{self, ForeignTypeObject};

/// C mirror: `include/pon_capi/numbers.h` `PyPonCapiNumbers`.
#[repr(C)]
pub(crate) struct PyPonCapiNumbers {
    long_from_long: unsafe extern "C" fn(c_long) -> *mut PyObject,
    long_as_long: unsafe extern "C" fn(*mut PyObject) -> c_long,
    long_from_long_long: unsafe extern "C" fn(c_longlong) -> *mut PyObject,
    long_from_unsigned_long: unsafe extern "C" fn(c_ulong) -> *mut PyObject,
    long_from_unsigned_long_long: unsafe extern "C" fn(c_ulonglong) -> *mut PyObject,
    long_from_ssize_t: unsafe extern "C" fn(isize) -> *mut PyObject,
    long_from_size_t: unsafe extern "C" fn(usize) -> *mut PyObject,
    long_from_double: unsafe extern "C" fn(c_double) -> *mut PyObject,
    long_as_long_long: unsafe extern "C" fn(*mut PyObject) -> c_longlong,
    long_as_unsigned_long: unsafe extern "C" fn(*mut PyObject) -> c_ulong,
    long_as_unsigned_long_mask: unsafe extern "C" fn(*mut PyObject) -> c_ulong,
    long_as_ssize_t: unsafe extern "C" fn(*mut PyObject) -> isize,
    long_as_size_t: unsafe extern "C" fn(*mut PyObject) -> usize,
    long_as_double: unsafe extern "C" fn(*mut PyObject) -> c_double,
    long_as_long_and_overflow: unsafe extern "C" fn(*mut PyObject, *mut c_int) -> c_long,
    long_from_void_ptr: unsafe extern "C" fn(*mut c_void) -> *mut PyObject,
    long_as_void_ptr: unsafe extern "C" fn(*mut PyObject) -> *mut c_void,
    bool_from_long: unsafe extern "C" fn(c_long) -> *mut PyObject,
    float_from_double: unsafe extern "C" fn(c_double) -> *mut PyObject,
    float_as_double: unsafe extern "C" fn(*mut PyObject) -> c_double,
    complex_from_doubles: unsafe extern "C" fn(c_double, c_double) -> *mut PyObject,
    complex_real_as_double: unsafe extern "C" fn(*mut PyObject) -> c_double,
    complex_imag_as_double: unsafe extern "C" fn(*mut PyObject) -> c_double,
    index_check: unsafe extern "C" fn(*mut PyObject) -> c_int,
    number_index: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    number_long: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    number_float: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    number_as_ssize_t: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> isize,
    type_check: unsafe extern "C" fn(*mut PyObject, c_int) -> c_int,
    long_as_unsigned_long_long: unsafe extern "C" fn(*mut PyObject) -> c_ulonglong,
}

unsafe impl Send for PyPonCapiNumbers {}
unsafe impl Sync for PyPonCapiNumbers {}

pub(crate) fn build() -> PyPonCapiNumbers {
    PyPonCapiNumbers {
        long_from_long: capi_long_from_long,
        long_as_long: capi_long_as_long,
        long_from_long_long: capi_long_from_long_long,
        long_from_unsigned_long: capi_long_from_unsigned_long,
        long_from_unsigned_long_long: capi_long_from_unsigned_long_long,
        long_from_ssize_t: capi_long_from_ssize_t,
        long_from_size_t: capi_long_from_size_t,
        long_from_double: capi_long_from_double,
        long_as_long_long: capi_long_as_long_long,
        long_as_unsigned_long: capi_long_as_unsigned_long,
        long_as_unsigned_long_mask: capi_long_as_unsigned_long_mask,
        long_as_ssize_t: capi_long_as_ssize_t,
        long_as_size_t: capi_long_as_size_t,
        long_as_double: capi_long_as_double,
        long_as_long_and_overflow: capi_long_as_long_and_overflow,
        long_from_void_ptr: capi_long_from_void_ptr,
        long_as_void_ptr: capi_long_as_void_ptr,
        bool_from_long: capi_bool_from_long,
        float_from_double: capi_float_from_double,
        float_as_double: capi_float_as_double,
        complex_from_doubles: capi_complex_from_doubles,
        complex_real_as_double: capi_complex_real_as_double,
        complex_imag_as_double: capi_complex_imag_as_double,
        index_check: capi_index_check,
        number_index: capi_number_index,
        number_long: capi_number_long,
        number_float: capi_number_float,
        number_as_ssize_t: capi_number_as_ssize_t,
        type_check: capi_type_check,
        long_as_unsigned_long_long: capi_long_as_unsigned_long_long,
    }
}

unsafe extern "C" fn capi_long_from_long(value: c_long) -> *mut PyObject {
    crate::types::int::from_bigint(BigInt::from(value))
}

unsafe extern "C" fn capi_long_as_long(object: *mut PyObject) -> c_long {
    let Some(value) = (unsafe { required_integer(object, "an integer is required") }) else {
        return -1;
    };
    match bigint_to_c_long(&value) {
        Some(value) => value,
        None => {
            raise_overflow("Python int too large to convert to C long");
            -1
        }
    }
}

unsafe extern "C" fn capi_long_from_long_long(value: c_longlong) -> *mut PyObject {
    crate::types::int::from_bigint(BigInt::from(value))
}

unsafe extern "C" fn capi_long_from_unsigned_long(value: c_ulong) -> *mut PyObject {
    crate::types::int::from_bigint(BigInt::from(value))
}

unsafe extern "C" fn capi_long_from_unsigned_long_long(value: c_ulonglong) -> *mut PyObject {
    crate::types::int::from_bigint(BigInt::from(value))
}

unsafe extern "C" fn capi_long_from_ssize_t(value: isize) -> *mut PyObject {
    crate::types::int::from_bigint(BigInt::from(value))
}

unsafe extern "C" fn capi_long_from_size_t(value: usize) -> *mut PyObject {
    crate::types::int::from_bigint(BigInt::from(value))
}

unsafe extern "C" fn capi_long_from_double(value: c_double) -> *mut PyObject {
    if value.is_nan() {
        raise_value("cannot convert float NaN to integer");
        return ptr::null_mut();
    }
    if value.is_infinite() {
        raise_overflow("cannot convert float infinity to integer");
        return ptr::null_mut();
    }
    match crate::types::int::bigint_from_f64_trunc(value) {
        Some(value) => crate::types::int::from_bigint(value),
        None => {
            raise_overflow("cannot convert float infinity to integer");
            ptr::null_mut()
        }
    }
}

unsafe extern "C" fn capi_long_as_long_long(object: *mut PyObject) -> c_longlong {
    let Some(value) = (unsafe { required_integer(object, "an integer is required") }) else {
        return -1;
    };
    match bigint_to_c_longlong(&value) {
        Some(value) => value,
        None => {
            raise_overflow("int too big to convert");
            -1
        }
    }
}

unsafe extern "C" fn capi_long_as_unsigned_long(object: *mut PyObject) -> c_ulong {
    let Some(value) = (unsafe { required_integer(object, "an integer is required") }) else {
        return c_ulong::MAX;
    };
    match bigint_to_c_ulong(&value) {
        Ok(value) => value,
        Err(UnsignedError::Negative) => {
            raise_overflow("can't convert negative value to unsigned int");
            c_ulong::MAX
        }
        Err(UnsignedError::TooLarge) => {
            raise_overflow("Python int too large to convert to C unsigned long");
            c_ulong::MAX
        }
    }
}

unsafe extern "C" fn capi_long_as_unsigned_long_long(object: *mut PyObject) -> c_ulonglong {
    let Some(value) = (unsafe { required_integer(object, "an integer is required") }) else {
        return c_ulonglong::MAX;
    };
    match bigint_to_c_ulonglong(&value) {
        Ok(value) => value,
        Err(UnsignedError::Negative) => {
            raise_overflow("can't convert negative int to unsigned");
            c_ulonglong::MAX
        }
        Err(UnsignedError::TooLarge) => {
            raise_overflow("int too big to convert");
            c_ulonglong::MAX
        }
    }
}

unsafe extern "C" fn capi_long_as_unsigned_long_mask(object: *mut PyObject) -> c_ulong {
    let Some(value) = (unsafe { required_integer(object, "an integer is required") }) else {
        return c_ulong::MAX;
    };
    bigint_to_c_ulong_mask(&value)
}

unsafe extern "C" fn capi_long_as_ssize_t(object: *mut PyObject) -> isize {
    let Some(value) = (unsafe { required_integer(object, "an integer is required") }) else {
        return -1;
    };
    match value.to_isize() {
        Some(value) => value,
        None => {
            raise_overflow("Python int too large to convert to C ssize_t");
            -1
        }
    }
}

unsafe extern "C" fn capi_long_as_size_t(object: *mut PyObject) -> usize {
    let Some(value) = (unsafe { required_integer(object, "an integer is required") }) else {
        return usize::MAX;
    };
    match bigint_to_usize(&value) {
        Ok(value) => value,
        Err(UnsignedError::Negative) => {
            raise_overflow("can't convert negative value to size_t");
            usize::MAX
        }
        Err(UnsignedError::TooLarge) => {
            raise_overflow("Python int too large to convert to C size_t");
            usize::MAX
        }
    }
}

unsafe extern "C" fn capi_long_as_double(object: *mut PyObject) -> c_double {
    let Some(value) = (unsafe { required_integer(object, "an integer is required") }) else {
        return -1.0;
    };
    match bigint_to_f64(&value) {
        Some(value) => value,
        None => {
            raise_overflow("int too large to convert to float");
            -1.0
        }
    }
}

unsafe extern "C" fn capi_long_as_long_and_overflow(object: *mut PyObject, overflow: *mut c_int) -> c_long {
    if !overflow.is_null() {
        unsafe {
            *overflow = 0;
        }
    }
    let Some(value) = (unsafe { required_integer(object, "'object' cannot be interpreted as an integer") }) else {
        return -1;
    };
    if let Some(value) = bigint_to_c_long(&value) {
        return value;
    }
    if !overflow.is_null() {
        unsafe {
            *overflow = if value.sign() == Sign::Minus { -1 } else { 1 };
        }
    }
    -1
}

unsafe extern "C" fn capi_long_from_void_ptr(value: *mut c_void) -> *mut PyObject {
    crate::types::int::from_bigint(BigInt::from(value as usize))
}

unsafe extern "C" fn capi_long_as_void_ptr(object: *mut PyObject) -> *mut c_void {
    let Some(value) = (unsafe { required_integer(object, "an integer is required") }) else {
        return ptr::null_mut();
    };
    if value.sign() == Sign::Minus {
        return match bigint_to_c_long(&value) {
            Some(value) => (value as isize as usize) as *mut c_void,
            None => {
                raise_overflow("Python int too large to convert to C long");
                ptr::null_mut()
            }
        };
    }
    match bigint_to_c_ulong(&value) {
        Ok(value) => (value as usize) as *mut c_void,
        Err(UnsignedError::Negative) => unreachable!("negative handled above"),
        Err(UnsignedError::TooLarge) => {
            raise_overflow("Python int too large to convert to C unsigned long");
            ptr::null_mut()
        }
    }
}

unsafe extern "C" fn capi_bool_from_long(value: c_long) -> *mut PyObject {
    crate::types::bool_::from_bool(value != 0)
}

unsafe extern "C" fn capi_float_from_double(value: c_double) -> *mut PyObject {
    crate::types::float::from_f64(value)
}

unsafe extern "C" fn capi_float_as_double(object: *mut PyObject) -> c_double {
    let Some(object) = normalize_arg(object) else {
        return -1.0;
    };
    match unsafe { coerce_f64(object) } {
        Ok(value) => value,
        Err(()) => -1.0,
    }
}

unsafe extern "C" fn capi_complex_from_doubles(real: c_double, imag: c_double) -> *mut PyObject {
    crate::types::complex_::from_f64s(real, imag)
}

unsafe extern "C" fn capi_complex_real_as_double(object: *mut PyObject) -> c_double {
    let Some(object) = normalize_arg(object) else {
        return -1.0;
    };
    if let Some((real, _)) = unsafe { crate::types::complex_::to_f64s(object) } {
        return real;
    }
    match unsafe { coerce_f64(object) } {
        Ok(value) => value,
        Err(()) => -1.0,
    }
}

unsafe extern "C" fn capi_complex_imag_as_double(object: *mut PyObject) -> c_double {
    let Some(object) = normalize_arg(object) else {
        return -1.0;
    };
    if let Some((_, imag)) = unsafe { crate::types::complex_::to_f64s(object) } {
        return imag;
    }
    match unsafe { coerce_f64(object) } {
        Ok(_) => 0.0,
        Err(()) => -1.0,
    }
}

unsafe extern "C" fn capi_index_check(object: *mut PyObject) -> c_int {
    let Some(object) = normalize_arg(object) else {
        return 0;
    };
    if object.is_null() {
        return 0;
    }
    if unsafe { crate::types::int::to_bigint_including_bool(object) }.is_some() {
        return 1;
    }
    if !crate::tag::is_heap(object) {
        return 0;
    }
    let slot = unsafe {
        object
            .as_ref()
            .and_then(|object| object.ob_type.as_ref())
            .and_then(|ty| ty.tp_as_number.as_ref())
            .and_then(|methods| methods.nb_index)
    };
    c_int::from(slot.is_some() || unsafe { has_attr(object, "__index__") })
}

unsafe extern "C" fn capi_number_index(object: *mut PyObject) -> *mut PyObject {
    let Some(object) = normalize_arg(object) else {
        return ptr::null_mut();
    };
    match unsafe { coerce_index_bigint(object) } {
        Ok(value) => crate::types::int::from_bigint(value),
        Err(()) => ptr::null_mut(),
    }
}

unsafe extern "C" fn capi_number_long(object: *mut PyObject) -> *mut PyObject {
    let Some(object) = normalize_arg(object) else {
        return ptr::null_mut();
    };
    crate::types::int::construct_from_args(&[object])
}

unsafe extern "C" fn capi_number_float(object: *mut PyObject) -> *mut PyObject {
    let Some(object) = normalize_arg(object) else {
        return ptr::null_mut();
    };
    match unsafe { coerce_f64(object) } {
        Ok(value) => crate::types::float::from_f64(value),
        Err(()) => ptr::null_mut(),
    }
}

unsafe extern "C" fn capi_number_as_ssize_t(object: *mut PyObject, exc: *mut PyObject) -> isize {
    let Some(object) = normalize_arg(object) else {
        return -1;
    };
    let value = match unsafe { coerce_index_bigint(object) } {
        Ok(value) => value,
        Err(()) => return -1,
    };
    if let Some(value) = value.to_isize() {
        return value;
    }
    if exc.is_null() {
        if value.sign() == Sign::Minus {
            isize::MIN
        } else {
            isize::MAX
        }
    } else {
        unsafe { raise_foreign_exception(exc, "cannot fit 'int' into an index-sized integer") };
        -1
    }
}

unsafe extern "C" fn capi_type_check(object: *mut PyObject, tid: c_int) -> c_int {
    if object.is_null() {
        return 0;
    }
    if crate::tag::is_small_int(object) {
        return c_int::from(tid == twin::TID_LONG as c_int);
    }
    if !crate::tag::is_heap(object) {
        return 0;
    }
    if exact_builtin_type_id(object) == Some(tid as usize) {
        return 1;
    }
    let Some(base) = native_builtin_type(tid) else {
        return 0;
    };
    let Some(ty) = (unsafe { object.as_ref().and_then(|object| object.ob_type.as_ref()) }) else {
        return 0;
    };
    c_int::from(unsafe { crate::mro::is_subtype((ty as *const PyType).cast_mut(), base) })
}

unsafe fn required_integer(object: *mut PyObject, type_error: &str) -> Option<BigInt> {
    let object = normalize_arg(object)?;
    match unsafe { crate::types::int::to_bigint_including_bool(object) } {
        Some(value) => Some(value),
        None => {
            raise_type(type_error);
            None
        }
    }
}

fn normalize_arg(object: *mut PyObject) -> Option<*mut PyObject> {
    let normalized = crate::tag::untag_arg(object);
    if crate::tag::is_small_int(object) && normalized.is_null() {
        None
    } else {
        Some(normalized)
    }
}

fn bigint_to_c_long(value: &BigInt) -> Option<c_long> {
    if value < &BigInt::from(c_long::MIN) || value > &BigInt::from(c_long::MAX) {
        return None;
    }
    value.to_i64().map(|value| value as c_long)
}

fn bigint_to_c_longlong(value: &BigInt) -> Option<c_longlong> {
    if value < &BigInt::from(c_longlong::MIN) || value > &BigInt::from(c_longlong::MAX) {
        return None;
    }
    value.to_i64().map(|value| value as c_longlong)
}

enum UnsignedError {
    Negative,
    TooLarge,
}

fn bigint_to_c_ulong(value: &BigInt) -> Result<c_ulong, UnsignedError> {
    if value.sign() == Sign::Minus {
        return Err(UnsignedError::Negative);
    }
    if value > &BigInt::from(c_ulong::MAX) {
        return Err(UnsignedError::TooLarge);
    }
    value.to_u64().map(|value| value as c_ulong).ok_or(UnsignedError::TooLarge)
}

fn bigint_to_c_ulonglong(value: &BigInt) -> Result<c_ulonglong, UnsignedError> {
    if value.sign() == Sign::Minus {
        return Err(UnsignedError::Negative);
    }
    if value > &BigInt::from(c_ulonglong::MAX) {
        return Err(UnsignedError::TooLarge);
    }
    value.to_u64().map(|value| value as c_ulonglong).ok_or(UnsignedError::TooLarge)
}

fn bigint_to_usize(value: &BigInt) -> Result<usize, UnsignedError> {
    if value.sign() == Sign::Minus {
        return Err(UnsignedError::Negative);
    }
    value.to_usize().ok_or(UnsignedError::TooLarge)
}

fn bigint_to_c_ulong_mask(value: &BigInt) -> c_ulong {
    let bits = core::mem::size_of::<c_ulong>() * 8;
    let modulus = BigInt::one() << bits;
    let mut masked = value % &modulus;
    if masked.sign() == Sign::Minus {
        masked += modulus;
    }
    masked.to_u64().unwrap_or(0) as c_ulong
}

fn bigint_to_f64(value: &BigInt) -> Option<f64> {
    match value.to_f64() {
        Some(value) if value.is_finite() => Some(value),
        _ => None,
    }
}

unsafe fn coerce_f64(object: *mut PyObject) -> Result<f64, ()> {
    if object.is_null() || !crate::tag::is_heap(object) {
        raise_type("must be real number, not object");
        return Err(());
    }
    if let Some(value) = unsafe { crate::types::float::to_f64(object) } {
        return Ok(value);
    }
    if let Some(value) = unsafe { crate::types::int::to_bigint_including_bool(object) } {
        return bigint_to_f64(&value).ok_or_else(|| {
            raise_overflow("int too large to convert to float");
        });
    }
    if let Some(method) = unsafe { try_get_attr(object, "__float__") } {
        let result = crate::tag::untag_arg(unsafe { abi::pon_call(method, ptr::null_mut(), 0) });
        if result.is_null() {
            return Err(());
        }
        if let Some(value) = unsafe { crate::types::float::to_f64(result) } {
            return Ok(value);
        }
        raise_type(&format!("{}.__float__ returned non-float (type {})", type_name(object), type_name(result)));
        return Err(());
    }
    raise_type(&format!("must be real number, not {}", type_name(object)));
    Err(())
}

unsafe fn coerce_index_bigint(object: *mut PyObject) -> Result<BigInt, ()> {
    if object.is_null() || !crate::tag::is_heap(object) {
        raise_type("'object' object cannot be interpreted as an integer");
        return Err(());
    }
    if let Some(value) = unsafe { crate::types::int::to_bigint_including_bool(object) } {
        return Ok(value);
    }
    let slot = unsafe {
        object
            .as_ref()
            .and_then(|object| object.ob_type.as_ref())
            .and_then(|ty| ty.tp_as_number.as_ref())
            .and_then(|methods| methods.nb_index)
    };
    if let Some(slot) = slot {
        let result = crate::tag::untag_arg(unsafe { slot(object) });
        if result.is_null() {
            return Err(());
        }
        if let Some(value) = unsafe { crate::types::int::to_bigint_including_bool(result) } {
            return Ok(value);
        }
        raise_type(&format!("__index__ returned non-int (type {})", type_name(result)));
        return Err(());
    }
    if let Some(method) = unsafe { try_get_attr(object, "__index__") } {
        let result = crate::tag::untag_arg(unsafe { abi::pon_call(method, ptr::null_mut(), 0) });
        if result.is_null() {
            return Err(());
        }
        if let Some(value) = unsafe { crate::types::int::to_bigint_including_bool(result) } {
            return Ok(value);
        }
        raise_type(&format!("__index__ returned non-int (type {})", type_name(result)));
        return Err(());
    }
    raise_type(&format!("'{}' object cannot be interpreted as an integer", type_name(object)));
    Err(())
}

unsafe fn try_get_attr(object: *mut PyObject, name: &str) -> Option<*mut PyObject> {
    let result = unsafe { crate::abstract_op::get_attr(object, crate::intern::intern(name)) };
    if result.is_null() {
        crate::thread_state::pon_err_clear();
        None
    } else {
        Some(crate::tag::untag_arg(result))
    }
}

unsafe fn has_attr(object: *mut PyObject, name: &str) -> bool {
    unsafe { try_get_attr(object, name) }.is_some()
}

fn type_name(object: *mut PyObject) -> &'static str {
    unsafe { crate::types::dict::type_name(object) }.unwrap_or("object")
}

fn raise_type(message: &str) {
    let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message);
}

fn raise_value(message: &str) {
    let _ = abi::exc::raise_kind_error_text(ExceptionKind::ValueError, message);
}

fn raise_overflow(message: &str) {
    let _ = abi::exc::raise_kind_error_text(ExceptionKind::OverflowError, message);
}

unsafe fn raise_foreign_exception(exception: *mut PyObject, message: &str) {
    let Some(class) = twin::native_of_foreign(exception.cast::<ForeignTypeObject>()) else {
        raise_type(message);
        return;
    };
    let message = unsafe { abi::pon_const_str(message.as_ptr(), message.len()) };
    if message.is_null() {
        return;
    }
    let mut argv = [message];
    let instance = unsafe { abi::pon_call(class.cast::<PyObject>(), argv.as_mut_ptr(), argv.len()) };
    if instance.is_null() {
        return;
    }
    let _ = unsafe { abi::exc::pon_raise(instance, ptr::null_mut()) };
}

fn exact_builtin_type_id(object: *mut PyObject) -> Option<usize> {
    if crate::tag::is_small_int(object) {
        return Some(twin::TID_LONG);
    }
    if object.is_null() || !crate::tag::is_heap(object) {
        return None;
    }
    let ty = unsafe { (*object).ob_type.cast_mut() };
    native_tid(ty)
}

fn native_tid(ty: *mut PyType) -> Option<usize> {
    if ty.is_null() {
        return None;
    }
    for (tid, native) in [
        (twin::TID_LONG, abi::runtime_long_type()),
        (twin::TID_BOOL, crate::types::bool_::bool_type()),
        (twin::TID_FLOAT, crate::types::float::float_type()),
        (twin::TID_COMPLEX, crate::types::complex_::complex_type()),
    ] {
        if ty == native {
            return Some(tid);
        }
    }
    None
}

fn native_builtin_type(tid: c_int) -> Option<*mut PyType> {
    match tid as usize {
        twin::TID_LONG => Some(abi::runtime_long_type()),
        twin::TID_BOOL => Some(crate::types::bool_::bool_type()),
        twin::TID_FLOAT => Some(crate::types::float::float_type()),
        twin::TID_COMPLEX => Some(crate::types::complex_::complex_type()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::ptr;

    use super::super::load_extension_module;
    use super::super::tests::{compile_extension, ResetImportStateOnDrop, TempExtensionRoot};
    use crate::abi::{format_object_for_print, pon_call, pon_runtime_init};
    use crate::import::module_attr;
    use crate::intern::intern;
    use crate::thread_state::{pon_err_message, test_state_lock};

    #[test]
    fn numbers_c_api_round_trips_and_errors() {
        let _guard = test_state_lock();
        let _reset = ResetImportStateOnDrop;
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
        }

        let temp = TempExtensionRoot::new();
        let module_path = compile_extension(
            &temp,
            "capi_numbers_test_ext",
            r#"
#include <Python.h>
#include <limits.h>
#include <stdint.h>

static PyObject *fail(const char *message) {
    PyErr_SetString(PyExc_RuntimeError, message);
    return NULL;
}

#define CHECK(condition, message) do { if (!(condition)) return fail(message); } while (0)
#define CHECK_NOT_NULL(value, message) do { if ((value) == NULL) return NULL; } while (0)

static PyObject *long_roundtrips(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;

    PyObject *ll = PyLong_FromLongLong(-1234567890123LL);
    CHECK_NOT_NULL(ll, "long long allocation failed");
    CHECK(PyLong_AsLongLong(ll) == -1234567890123LL, "long long round-trip failed");

    PyObject *ul = PyLong_FromUnsignedLong(ULONG_MAX);
    CHECK_NOT_NULL(ul, "unsigned long allocation failed");
    CHECK(PyLong_AsUnsignedLong(ul) == ULONG_MAX, "unsigned long round-trip failed");

    PyObject *ull = PyLong_FromUnsignedLongLong(42ULL);
    CHECK_NOT_NULL(ull, "unsigned long long allocation failed");
    CHECK(PyLong_AsUnsignedLongLong(ull) == 42ULL, "unsigned long long round-trip failed");
    CHECK(PyLong_AsUnsignedLongMask(ull) == 42UL, "unsigned long long mask failed");

    PyObject *ss = PyLong_FromSsize_t((Py_ssize_t)-12345);
    CHECK_NOT_NULL(ss, "ssize allocation failed");
    CHECK(PyLong_AsSsize_t(ss) == (Py_ssize_t)-12345, "ssize round-trip failed");

    PyObject *sz = PyLong_FromSize_t((size_t)1234567);
    CHECK_NOT_NULL(sz, "size allocation failed");
    CHECK(PyLong_AsSize_t(sz) == (size_t)1234567, "size round-trip failed");

    PyObject *from_double = PyLong_FromDouble(0x1p70);
    CHECK_NOT_NULL(from_double, "double-to-long allocation failed");
    CHECK(PyLong_AsDouble(from_double) == 0x1p70, "PyLong_AsDouble(2**70) failed");

    int overflow = -42;
    long as_long = PyLong_AsLongAndOverflow(PyLong_FromLong(123), &overflow);
    CHECK(as_long == 123 && overflow == 0, "PyLong_AsLongAndOverflow in-range failed");

    void *ptr = (void *)(uintptr_t)0x1234;
    PyObject *ptr_long = PyLong_FromVoidPtr(ptr);
    CHECK_NOT_NULL(ptr_long, "void pointer allocation failed");
    CHECK(PyLong_AsVoidPtr(ptr_long) == ptr, "void pointer round-trip failed");

    PyObject *truth = PyBool_FromLong(5);
    CHECK_NOT_NULL(truth, "bool allocation failed");
    CHECK(PyBool_Check(truth), "PyBool_Check failed");
    CHECK(PyLong_AsLong(truth) == 1, "bool-as-long failed");

    return PyLong_FromLong(1);
}

static PyObject *overflow_branch(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;

    PyObject *big = PyLong_FromDouble(0x1p70);
    CHECK_NOT_NULL(big, "big int allocation failed");
    CHECK(PyLong_AsLong(big) == -1, "PyLong_AsLong overflow sentinel failed");
    CHECK(PyErr_Occurred() == PyExc_OverflowError, "PyLong_AsLong overflow type failed");
    PyErr_Clear();

    int overflow = 0;
    CHECK(PyLong_AsLongAndOverflow(big, &overflow) == -1, "AsLongAndOverflow sentinel failed");
    CHECK(overflow == 1, "AsLongAndOverflow positive flag failed");
    CHECK(PyErr_Occurred() == NULL, "AsLongAndOverflow should not set an error");

    PyObject *not_int = PyFloat_FromDouble(1.25);
    CHECK_NOT_NULL(not_int, "float allocation failed");
    CHECK(PyLong_AsLong(not_int) == -1, "PyLong_AsLong non-int sentinel failed");
    CHECK(PyErr_Occurred() == PyExc_TypeError, "PyLong_AsLong non-int type failed");
    PyErr_Clear();

    PyObject *negative = PyLong_FromLong(-1);
    CHECK_NOT_NULL(negative, "negative allocation failed");
    CHECK(PyLong_AsSize_t(negative) == (size_t)-1, "PyLong_AsSize_t negative sentinel failed");
    CHECK(PyErr_Occurred() == PyExc_OverflowError, "PyLong_AsSize_t negative error failed");
    PyErr_Clear();

    return PyLong_FromLong(1);
}

static PyObject *float_complex_roundtrips(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;

    PyObject *flt = PyFloat_FromDouble(-3.25);
    CHECK_NOT_NULL(flt, "float allocation failed");
    CHECK(PyFloat_Check(flt), "PyFloat_Check failed");
    CHECK(PyFloat_CheckExact(flt), "PyFloat_CheckExact failed");
    CHECK(PyFloat_AsDouble(flt) == -3.25, "float round-trip failed");

    PyObject *integer = PyLong_FromLong(7);
    CHECK_NOT_NULL(integer, "integer allocation failed");
    CHECK(PyFloat_AsDouble(integer) == 7.0, "PyFloat_AsDouble integer coercion failed");

    PyObject *complex_value = PyComplex_FromDoubles(1.5, -2.25);
    CHECK_NOT_NULL(complex_value, "complex allocation failed");
    CHECK(PyComplex_Check(complex_value), "PyComplex_Check failed");
    CHECK(PyComplex_CheckExact(complex_value), "PyComplex_CheckExact failed");
    CHECK(PyComplex_RealAsDouble(complex_value) == 1.5, "complex real failed");
    CHECK(PyComplex_ImagAsDouble(complex_value) == -2.25, "complex imag failed");
    CHECK(PyComplex_RealAsDouble(integer) == 7.0, "complex real int coercion failed");
    CHECK(PyComplex_ImagAsDouble(integer) == 0.0, "complex imag int coercion failed");

    PyObject *big = PyLong_FromDouble(0x1p70);
    CHECK_NOT_NULL(big, "big int allocation failed");
    CHECK(PyLong_AsDouble(big) == 0x1p70, "PyLong_AsDouble large finite int failed");

    return PyLong_FromLong(1);
}

static PyObject *index_and_number_protocol(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;

    CHECK(PyIndex_Check(Py_True), "PyIndex_Check(bool) failed");
    PyObject *indexed = PyNumber_Index(Py_True);
    CHECK_NOT_NULL(indexed, "PyNumber_Index(bool) failed");
    CHECK(PyLong_CheckExact(indexed), "PyNumber_Index(bool) did not return exact int");
    CHECK(PyLong_AsLong(indexed) == 1, "PyNumber_Index(bool) value failed");

    PyObject *as_long = PyNumber_Long(PyFloat_FromDouble(4.75));
    CHECK_NOT_NULL(as_long, "PyNumber_Long(float) failed");
    CHECK(PyLong_CheckExact(as_long), "PyNumber_Long(float) did not return exact int");
    CHECK(PyLong_AsLong(as_long) == 4, "PyNumber_Long(float) truncation failed");

    PyObject *as_float = PyNumber_Float(PyLong_FromLong(9));
    CHECK_NOT_NULL(as_float, "PyNumber_Float(int) failed");
    CHECK(PyFloat_CheckExact(as_float), "PyNumber_Float(int) did not return exact float");
    CHECK(PyFloat_AsDouble(as_float) == 9.0, "PyNumber_Float(int) value failed");

    PyObject *big = PyLong_FromDouble(0x1p70);
    CHECK_NOT_NULL(big, "big int allocation failed");
    CHECK(PyNumber_AsSsize_t(big, NULL) == PY_SSIZE_T_MAX, "PyNumber_AsSsize_t(NULL) did not clip high");
    CHECK(PyNumber_AsSsize_t(big, PyExc_OverflowError) == -1, "PyNumber_AsSsize_t(exc) sentinel failed");
    CHECK(PyErr_Occurred() == PyExc_OverflowError, "PyNumber_AsSsize_t(exc) type failed");
    PyErr_Clear();

    return PyLong_FromLong(1);
}

static PyObject *type_check_macros(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;

    PyObject *integer = PyLong_FromLong(3);
    CHECK_NOT_NULL(integer, "integer allocation failed");
    CHECK(PyLong_Check(integer), "PyLong_Check(int) failed");
    CHECK(PyLong_CheckExact(integer), "PyLong_CheckExact(int) failed");
    CHECK(PyLong_Check(Py_True), "PyLong_Check(bool) failed");
    CHECK(!PyLong_CheckExact(Py_True), "PyLong_CheckExact(bool) should fail");
    CHECK(PyBool_Check(Py_False), "PyBool_Check(false) failed");
    CHECK(!PyBool_Check(integer), "PyBool_Check(int) should fail");

    PyObject *flt = PyFloat_FromDouble(2.0);
    CHECK_NOT_NULL(flt, "float allocation failed");
    CHECK(PyFloat_Check(flt), "PyFloat_Check(float) failed");
    CHECK(PyFloat_CheckExact(flt), "PyFloat_CheckExact(float) failed");
    CHECK(!PyFloat_Check(integer), "PyFloat_Check(int) should fail");

    PyObject *complex_value = PyComplex_FromDoubles(0.0, 1.0);
    CHECK_NOT_NULL(complex_value, "complex allocation failed");
    CHECK(PyComplex_Check(complex_value), "PyComplex_Check(complex) failed");
    CHECK(PyComplex_CheckExact(complex_value), "PyComplex_CheckExact(complex) failed");
    CHECK(!PyComplex_Check(flt), "PyComplex_Check(float) should fail");

    return PyLong_FromLong(1);
}

static PyMethodDef methods[] = {
    {"long_roundtrips", long_roundtrips, METH_NOARGS, "exercise long constructors/extractors"},
    {"overflow_branch", overflow_branch, METH_NOARGS, "exercise long overflow errors"},
    {"float_complex_roundtrips", float_complex_roundtrips, METH_NOARGS, "exercise float and complex APIs"},
    {"index_and_number_protocol", index_and_number_protocol, METH_NOARGS, "exercise index and number APIs"},
    {"type_check_macros", type_check_macros, METH_NOARGS, "exercise numeric check macros"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_numbers_test_ext",
    "Pon numbers C-API test extension",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_numbers_test_ext(void) {
    return PyModule_Create(&module);
}
"#,
        );

        let module = load_extension_module("capi_numbers_test_ext", &module_path)
            .unwrap_or_else(|message| panic!("failed to load numbers C extension: {message}"));
        assert!(!module.is_null(), "extension loader returned NULL module");

        let module_name = intern("capi_numbers_test_ext");
        for method_name in [
            "long_roundtrips",
            "overflow_branch",
            "float_complex_roundtrips",
            "index_and_number_protocol",
            "type_check_macros",
        ] {
            let method = module_attr(module_name, intern(method_name)).unwrap_or_else(|| panic!("{method_name} method registered"));
            let result = unsafe { pon_call(method, ptr::null_mut(), 0) };
            assert!(
                !result.is_null(),
                "{method_name} returned NULL: {:?}",
                pon_err_message()
            );
            assert_eq!(format_object_for_print(result).as_deref(), Ok("1"));
        }
    }
}
