//! Slice object implementation.

use core::mem::offset_of;
use core::ptr;

use num_traits::ToPrimitive;

use crate::intern::intern;
use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::types::{method, type_};

/// Boxed Python `slice` object.
#[repr(C)]
#[derive(Debug)]
pub struct PySlice {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Lower bound object, usually `None`.
    pub start: *mut PyObject,
    /// Upper bound object, usually `None`.
    pub stop: *mut PyObject,
    /// Step object, usually `None`.
    pub step: *mut PyObject,
}

/// Normalized `slice.indices(len)` result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SliceIndices {
    /// First index visited, already clamped to Python's slice rules.
    pub start: isize,
    /// Sentinel stop index, already clamped to Python's slice rules.
    pub stop: isize,
    /// Non-zero step.
    pub step: isize,
    /// Number of selected elements.
    pub len: usize,
}
/// Installs Python-visible slice attributes and methods on the runtime slice type.
pub fn install_slice_slots(ty: &mut PyType) {
    ty.tp_getattro = Some(slice_getattro);
}

unsafe extern "C" fn slice_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        return raise_type_error("slice attribute name must be str");
    };
    let slice = unsafe { &*object.cast::<PySlice>() };
    match name {
        "start" => slice.start,
        "stop" => slice.stop,
        "step" => slice.step,
        "indices" => bound_indices_method(object),
        _ => raise_type_error(&format!("'slice' object has no attribute '{name}'")),
    }
}

fn bound_indices_method(receiver: *mut PyObject) -> *mut PyObject {
    let function = unsafe { crate::abi::pon_make_function(slice_indices_entry as *const u8, 2, intern("indices")) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match method::new_bound_method(function, receiver) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => raise_type_error(&message),
    }
}

unsafe extern "C" fn slice_indices_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return raise_type_error("slice.indices() received a null argv pointer");
    };
    if args.len() != 2 {
        return raise_type_error(&format!("slice.indices() takes exactly one argument ({} given)", args.len().saturating_sub(1)));
    }
    let slice = unsafe { &*args[0].cast::<PySlice>() };
    let Some(length) = (unsafe { index_isize(args[1]) }) else {
        return raise_type_error(&index_error(args[1]));
    };
    if length < 0 {
        return raise_value_error("length should not be negative");
    }
    match normalize_indices(slice, length) {
        Ok((start, stop, step)) => {
            let mut values = [
                crate::types::int::from_i64(start as i64),
                crate::types::int::from_i64(stop as i64),
                crate::types::int::from_i64(step as i64),
            ];
            unsafe { crate::abi::seq::pon_build_tuple(values.as_mut_ptr(), values.len()) }
        }
        Err(message) => raise_value_error(&message),
    }
}

fn normalize_indices(slice: &PySlice, len: isize) -> Result<(isize, isize, isize), String> {
    let step = if unsafe { is_none(slice.step) } {
        1
    } else {
        unsafe { index_isize(slice.step) }.ok_or_else(|| "slice indices must be integers or None or have an __index__ method".to_owned())?
    };
    if step == 0 {
        return Err("slice step cannot be zero".to_owned());
    }
    let (start, stop) = if step < 0 {
        (
            normalize_bound(slice.start, len, len - 1, -1, len - 1)?,
            normalize_bound(slice.stop, len, -1, -1, len - 1)?,
        )
    } else {
        (
            normalize_bound(slice.start, len, 0, 0, len)?,
            normalize_bound(slice.stop, len, len, 0, len)?,
        )
    };
    Ok((start, stop, step))
}

fn normalize_bound(value: *mut PyObject, len: isize, default_none: isize, lower: isize, upper: isize) -> Result<isize, String> {
    if unsafe { is_none(value) } {
        return Ok(default_none.clamp(lower, upper));
    }
    let mut value = unsafe { index_isize(value) }
        .ok_or_else(|| "slice indices must be integers or None or have an __index__ method".to_owned())?;
    if value < 0 {
        value = value.saturating_add(len);
    }
    Ok(value.clamp(lower, upper))
}

unsafe fn index_isize(object: *mut PyObject) -> Option<isize> {
    if let Some(value) = unsafe { crate::types::bool_::to_bool(object) } {
        return Some(isize::from(value));
    }
    unsafe { crate::types::int::to_bigint(object).and_then(|value| value.to_isize()) }
}

fn index_error(object: *mut PyObject) -> String {
    format!("'{}' object cannot be interpreted as an integer", type_name(object))
}

unsafe fn is_none(object: *mut PyObject) -> bool {
    type_name(object) == "NoneType"
}

fn type_name(object: *mut PyObject) -> &'static str {
    if object.is_null() {
        return "NULL";
    }
    let ty = unsafe { (*object).ob_type };
    if ty.is_null() {
        return "object";
    }
    unsafe { (*ty).name() }
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

fn raise_type_error(message: &str) -> *mut PyObject {
    unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn raise_value_error(message: &str) -> *mut PyObject {
    unsafe { crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) }
}

/// Traces the boxed bound values stored in a slice.
///
/// # Safety
///
/// `object` must be NULL or point to a live [`PySlice`] allocation.
pub unsafe extern "C" fn trace_slice(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    let slice = unsafe { &*object.cast::<PySlice>() };
    for child in [slice.start, slice.stop, slice.step] {
        if !child.is_null() {
            visitor(child.cast::<u8>());
        }
    }
}

const _: () = {
    assert!(offset_of!(PySlice, ob_base) == 0);
};
