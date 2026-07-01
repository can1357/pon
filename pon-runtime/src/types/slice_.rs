//! Slice object implementation.

use core::mem::offset_of;

use crate::object::{PyObject, PyObjectHeader};

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
