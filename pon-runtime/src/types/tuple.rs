//! Tuple sequence implementation.

use core::mem::offset_of;

use crate::object::{PyObject, PyObjectHeader};

/// Boxed Python `tuple` with immutable Rust-owned pointer storage.
#[repr(C)]
#[derive(Debug)]
pub struct PyTuple {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Number of elements.
    pub len: usize,
    /// Pointer to `len` boxed-object slots, or NULL when empty.
    pub items: *mut *mut PyObject,
}

impl PyTuple {
    /// Returns tuple elements.
    ///
    /// # Safety
    ///
    /// `self.items` must either be NULL with `self.len == 0`, or point to at
    /// least `self.len` initialized slots.
    #[must_use]
    pub unsafe fn as_slice(&self) -> &[*mut PyObject] {
        if self.len == 0 {
            &[]
        } else {
            debug_assert!(
                !self.items.is_null() && self.items.is_aligned(),
                "PyTuple::as_slice: items pointer {:p} (len {}) is null or misaligned — a tagged or uninitialized tuple reached iteration",
                self.items,
                self.len,
            );
            unsafe { core::slice::from_raw_parts(self.items, self.len) }
        }
    }
}

/// Traces every object reference contained in a tuple.
///
/// # Safety
///
/// `object` must be NULL or point to a live [`PyTuple`] allocation.
pub unsafe extern "C" fn trace_tuple(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    let tuple = unsafe { &*object.cast::<PyTuple>() };
    for child in unsafe { tuple.as_slice() }.iter().copied() {
        if !child.is_null() {
            visitor(child.cast::<u8>());
        }
    }
}

/// Drops the Rust-owned tuple element vector.
///
/// # Safety
///
/// `object` must be NULL or point to a live [`PyTuple`] allocation whose `items`
/// pointer was produced by this module's vector-leak convention.
pub unsafe extern "C" fn finalize_tuple(object: *mut u8) {
    if object.is_null() {
        return;
    }
    let tuple = unsafe { &mut *object.cast::<PyTuple>() };
    if !tuple.items.is_null() && tuple.len != 0 {
        unsafe { drop(Vec::from_raw_parts(tuple.items, tuple.len, tuple.len)) };
        tuple.items = core::ptr::null_mut();
        tuple.len = 0;
    }
}

const _: () = {
    assert!(offset_of!(PyTuple, ob_base) == 0);
};
