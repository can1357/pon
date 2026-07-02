//! List sequence implementation.

use core::mem::offset_of;

use crate::object::{PyObject, PyObjectHeader};

/// Boxed Python `list` with Rust-owned contiguous pointer storage.
///
/// `items` points to `cap` initialized `*mut PyObject` slots allocated from a
/// leaked `Vec`; the GC finalizer reconstructs and drops that vector.  The
/// object trace hook visits only `len` live entries.
#[repr(C)]
#[derive(Debug)]
pub struct PyList {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Number of live elements.
    pub len: usize,
    /// Allocated item slots.
    pub cap: usize,
    /// Pointer to `cap` boxed-object slots, or NULL when `cap == 0`.
    pub items: *mut *mut PyObject,
}

impl PyList {
    /// Returns the live elements as an immutable slice.
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
            unsafe { core::slice::from_raw_parts(self.items, self.len) }
        }
    }

    /// Returns the live elements as a mutable slice.
    ///
    /// # Safety
    ///
    /// `self.items` must either be NULL with `self.len == 0`, or point to at
    /// least `self.len` initialized slots not aliased for mutation elsewhere.
    pub unsafe fn as_mut_slice(&mut self) -> &mut [*mut PyObject] {
        if self.len == 0 {
            &mut []
        } else {
            unsafe { core::slice::from_raw_parts_mut(self.items, self.len) }
        }
    }
}

/// Formats an exact runtime list with Python list display syntax.
///
/// # Safety
///
/// `object` must point to a live [`PyList`] allocation.
pub unsafe fn list_repr(object: *mut PyObject) -> Result<String, String> {
    if object.is_null() {
        return Err("list repr received NULL".to_owned());
    }
    let list = unsafe { &*object.cast::<PyList>() };
    let items = unsafe { list.as_slice() };
    let mut out = String::from("[");
    for (index, item) in items.iter().copied().enumerate() {
        if index != 0 {
            out.push_str(", ");
        }
        match crate::native::builtins_mod::try_repr_text(item) {
            Ok(text) => out.push_str(&text),
            Err(()) => return Err("list element repr raised".to_owned()),
        }
    }
    out.push(']');
    Ok(out)
}

/// Traces the live object references contained in a list.
///
/// # Safety
///
/// `object` must be NULL or point to a live [`PyList`] allocation.
pub unsafe extern "C" fn trace_list(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    let list = unsafe { &*object.cast::<PyList>() };
    for child in unsafe { list.as_slice() }.iter().copied() {
        if !child.is_null() {
            visitor(child.cast::<u8>());
        }
    }
}

/// Drops the Rust-owned backing vector for a list.
///
/// # Safety
///
/// `object` must be NULL or point to a live [`PyList`] allocation whose `items`
/// pointer was produced by this module's vector-leak convention.
pub unsafe extern "C" fn finalize_list(object: *mut u8) {
    if object.is_null() {
        return;
    }
    let list = unsafe { &mut *object.cast::<PyList>() };
    if !list.items.is_null() && list.cap != 0 {
        unsafe { drop(Vec::from_raw_parts(list.items, list.cap, list.cap)) };
        list.items = core::ptr::null_mut();
        list.len = 0;
        list.cap = 0;
    }
}

const _: () = {
    assert!(offset_of!(PyList, ob_base) == 0);
};
