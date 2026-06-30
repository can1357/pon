//! Boxed Python object layouts for the Phase-A runtime.
//!
//! The model intentionally mirrors CPython's object-header shape while omitting
//! reference-count storage: ownership is delegated to `pon-gc`, and every value
//! crossing the compiled-code ABI is a boxed `*mut PyObject` whose first field is the common
//! header.

use core::mem::{offset_of, size_of};
use core::ptr;

/// Per-object GC metadata reserved for the stop-the-world heap.
///
/// This is not a reference count.  Phase A only needs a stable header slot that
/// both runtime and GC can agree on while the collector owns the actual mark and
/// allocation metadata out of line.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GcMeta {
    /// Reserved collector bits.  Runtime code must not encode ownership here.
    pub flags: usize,
}

impl GcMeta {
    /// Empty metadata for newly initialized objects.
    pub const EMPTY: Self = Self { flags: 0 };
}

/// Header present at offset zero in every concrete boxed value.
#[repr(C)]
#[derive(Debug)]
pub struct PyObjectHeader {
    /// Runtime type descriptor for dispatch and diagnostics.
    pub ob_type: *const PyType,
    /// Stop-the-world GC metadata slot; it is not a reference-count field.
    pub gc_meta: GcMeta,
}

impl PyObjectHeader {
    /// Builds a header for a concrete object of `ob_type`.
    #[must_use]
    pub const fn new(ob_type: *const PyType) -> Self {
        Self {
            ob_type,
            gc_meta: GcMeta::EMPTY,
        }
    }
}

/// The ABI base type for boxed values.
///
/// Pointers to concrete values are passed through compiled code as
/// `*mut PyObject`; the header is the full prefix shared by all concrete object
/// layouts.
pub type PyObject = PyObjectHeader;

/// Runtime type object.
#[repr(C)]
#[derive(Debug)]
pub struct PyType {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// UTF-8 type name bytes.
    pub name: *const u8,
    /// Byte length for `name`.
    pub name_len: usize,
    /// Size in bytes of instances for this type.
    pub instance_size: usize,
}

impl PyType {
    /// Creates an immortal type descriptor.
    #[must_use]
    pub const fn new(type_type: *const PyType, name: &'static str, instance_size: usize) -> Self {
        Self {
            ob_base: PyObjectHeader::new(type_type),
            name: name.as_ptr(),
            name_len: name.len(),
            instance_size,
        }
    }

    /// Returns the UTF-8 type name.
    #[must_use]
    pub fn name(&self) -> &str {
        // SAFETY: Type objects are created only from `'static str` names.
        unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(self.name, self.name_len)) }
    }
}

/// Boxed Python integer for Phase A.
#[repr(C)]
#[derive(Debug)]
pub struct PyLong {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Signed 64-bit payload used by the Phase-A integer subset.
    pub value: i64,
}

/// Boxed Python Unicode string.
#[repr(C)]
#[derive(Debug)]
pub struct PyUnicode {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// UTF-8 byte length.
    pub len: usize,
    /// UTF-8 byte storage.  This may borrow rodata or point to owned heap bytes.
    pub data: *const u8,
    /// Whether `data` owns a leaked boxed byte slice that the GC finalizer frees.
    pub owns_data: bool,
}

impl PyUnicode {
    /// Returns the string as UTF-8 when the payload is valid.
    #[must_use]
    pub unsafe fn as_str(&self) -> Option<&str> {
        if self.data.is_null() && self.len != 0 {
            return None;
        }
        // SAFETY: The caller guarantees that `self` is a live `PyUnicode`; the
        // UTF-8 validity check below handles arbitrary bytes defensively.
        let bytes = unsafe { core::slice::from_raw_parts(self.data, self.len) };
        core::str::from_utf8(bytes).ok()
    }
}

/// ABI function pointer type used by compiled Python functions.
pub type PyCodeFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

/// Boxed Python function.
#[repr(C)]
#[derive(Debug)]
pub struct PyFunction {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Raw entrypoint address for a `PyCodeFn`.
    pub code: *const u8,
    /// Positional arity enforced by `pon_call`.
    pub arity: usize,
    /// Interned function name.
    pub name_interned: u32,
}

/// The immortal `None` object layout.
#[repr(C)]
#[derive(Debug)]
pub struct PyNone {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
}

/// Casts a concrete object pointer to the ABI base pointer.
#[must_use]
pub fn as_object_ptr<T>(value: *mut T) -> *mut PyObject {
    value.cast::<PyObject>()
}

/// Returns true when `object` has exactly the requested runtime type pointer.
#[must_use]
pub unsafe fn is_exact_type(object: *mut PyObject, ty: *const PyType) -> bool {
    if object.is_null() {
        return false;
    }
    // SAFETY: Non-null boxed values always begin with `PyObjectHeader`.
    unsafe { ptr::addr_of!((*object).ob_type).read() == ty }
}

const _: () = {
    assert!(offset_of!(PyObjectHeader, ob_type) == 0);
    assert!(offset_of!(PyType, ob_base) == 0);
    assert!(offset_of!(PyLong, ob_base) == 0);
    assert!(offset_of!(PyUnicode, ob_base) == 0);
    assert!(offset_of!(PyFunction, ob_base) == 0);
    assert!(offset_of!(PyNone, ob_base) == 0);
    assert!(size_of::<PyObject>() == size_of::<PyObjectHeader>());
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concrete_headers_are_first() {
        assert_eq!(offset_of!(PyLong, ob_base), 0);
        assert_eq!(offset_of!(PyUnicode, ob_base), 0);
        assert_eq!(offset_of!(PyFunction, ob_base), 0);
        assert_eq!(offset_of!(PyNone, ob_base), 0);
    }
}
