//! Mutable bytearray implementation.
//!
//! WS-STR keeps bytearray as an owned mutable byte vector with Python-shaped
//! formatting and method helpers.  Hub integration can later move allocation into
//! the GC without changing this operation layer.

use std::ptr;
use std::sync::LazyLock;

use crate::object::{PyObjectHeader, PyType};
use crate::types::bytes_;

/// Boxed mutable Python `bytearray` value used by WS-STR helper entry points.
#[repr(C)]
#[derive(Debug)]
pub struct PyByteArray {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Owned byte storage.
    pub bytes: Vec<u8>,
}

impl PyByteArray {
    /// Returns the current byte payload.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the current mutable byte payload.
    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
}

static BYTEARRAY_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let ty = Box::new(PyType::new(
        ptr::null(),
        "bytearray",
        core::mem::size_of::<PyByteArray>(),
    ));
    Box::into_raw(ty) as usize
});

/// Returns the process-lifetime type descriptor used for boxed bytearray helpers.
#[must_use]
pub fn bytearray_type() -> *mut PyType {
    *BYTEARRAY_TYPE as *mut PyType
}

/// Allocates a boxed bytearray object outside the GC-managed heap.
#[must_use]
pub fn boxed_bytearray(bytes: &[u8]) -> *mut PyByteArray {
    Box::into_raw(Box::new(PyByteArray {
        ob_base: PyObjectHeader::new(bytearray_type()),
        bytes: bytes.to_vec(),
    }))
}

/// Returns true when `object_type` is the WS-STR bytearray type descriptor.
#[must_use]
pub fn is_bytearray_type(object_type: *const PyType) -> bool {
    object_type == bytearray_type().cast_const()
}

/// Concatenates bytearrays under Python's bytearray `+` behavior.
#[must_use]
pub fn concat(left: &[u8], right: &[u8]) -> Vec<u8> {
    bytes_::concat(left, right)
}

/// Repeats a bytearray, treating negative counts as zero.
#[must_use]
pub fn repeat(bytes: &[u8], count: isize) -> Vec<u8> {
    bytes_::repeat(bytes, count)
}

/// Python `bytearray.find`, returning a byte offset or `-1`.
#[must_use]
pub fn find(haystack: &[u8], needle: &[u8]) -> isize {
    bytes_::find(haystack, needle)
}

/// Python `bytearray.startswith` for one prefix.
#[must_use]
pub fn startswith(bytes: &[u8], prefix: &[u8]) -> bool {
    bytes_::startswith(bytes, prefix)
}

/// Python `bytearray.replace` for the common unlimited-count path.
#[must_use]
pub fn replace(bytes: &[u8], old: &[u8], new: &[u8]) -> Vec<u8> {
    bytes_::replace(bytes, old, new)
}

/// Python `bytearray.split` for a concrete separator or ASCII whitespace.
#[must_use]
pub fn split(bytes: &[u8], sep: Option<&[u8]>) -> Vec<Vec<u8>> {
    bytes_::split(bytes, sep)
}

/// Python `bytearray.join` over bytes-like items.
#[must_use]
pub fn join(sep: &[u8], items: &[Vec<u8>]) -> Vec<u8> {
    bytes_::join(sep, items)
}

/// CPython-shaped `repr(bytearray(...))` for representative byte payloads.
#[must_use]
pub fn repr(bytes: &[u8]) -> String {
    format!("bytearray({})", bytes_::repr(bytes))
}

/// Formats a list of bytearrays like CPython's list repr.
#[must_use]
pub fn repr_bytearray_list(items: &[Vec<u8>]) -> String {
    let mut out = String::from("[");
    for (index, item) in items.iter().enumerate() {
        if index != 0 {
            out.push_str(", ");
        }
        out.push_str(&repr(item));
    }
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repr_matches_cpython_shape() {
        assert_eq!(repr(b"a\n"), "bytearray(b'a\\n')");
    }
}
