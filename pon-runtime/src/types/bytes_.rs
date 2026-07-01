//! Bytes implementation.
//!
//! The Phase-B hub has not wired a GC-managed bytes type yet, so WS-STR keeps
//! the concrete layout and pure operations here.  ABI helpers may box this type
//! behind `*mut PyObject` for representative tier-0 behavior.

use std::ptr;
use std::sync::LazyLock;

use crate::object::{PyObjectHeader, PyType};

/// Boxed immutable Python `bytes` value used by WS-STR helper entry points.
#[repr(C)]
#[derive(Debug)]
pub struct PyBytes {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Byte length.
    pub len: usize,
    /// Owned byte storage.
    pub data: *const u8,
}

impl PyBytes {
    /// Returns the immutable payload bytes.
    ///
    /// # Safety
    ///
    /// The caller must pass a live `PyBytes` object allocated by the string ABI.
    #[must_use]
    pub unsafe fn as_slice(&self) -> &[u8] {
        if self.data.is_null() && self.len != 0 {
            return &[];
        }
        // SAFETY: The object constructor stores exactly `len` bytes at `data`.
        unsafe { core::slice::from_raw_parts(self.data, self.len) }
    }
}

static BYTES_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let ty = Box::new(PyType::new(ptr::null(), "bytes", core::mem::size_of::<PyBytes>()));
    Box::into_raw(ty) as usize
});

/// Returns the process-lifetime type descriptor used for boxed `bytes` helpers.
#[must_use]
pub fn bytes_type() -> *mut PyType {
    *BYTES_TYPE as *mut PyType
}

/// Allocates a boxed bytes object outside the GC-managed heap.
#[must_use]
pub fn boxed_bytes(bytes: &[u8]) -> *mut PyBytes {
    let data = bytes.to_vec().into_boxed_slice();
    let len = data.len();
    let data = Box::into_raw(data).cast::<u8>();
    Box::into_raw(Box::new(PyBytes {
        ob_base: PyObjectHeader::new(bytes_type()),
        len,
        data,
    }))
}

/// Returns true when `object_type` is the WS-STR bytes type descriptor.
#[must_use]
pub fn is_bytes_type(object_type: *const PyType) -> bool {
    object_type == bytes_type().cast_const()
}

/// Concatenates Python bytes.
#[must_use]
pub fn concat(left: &[u8], right: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(left.len() + right.len());
    out.extend_from_slice(left);
    out.extend_from_slice(right);
    out
}

/// Repeats Python bytes, treating negative counts as zero.
#[must_use]
pub fn repeat(bytes: &[u8], count: isize) -> Vec<u8> {
    let count = usize::try_from(count).unwrap_or(0);
    let mut out = Vec::with_capacity(bytes.len().saturating_mul(count));
    for _ in 0..count {
        out.extend_from_slice(bytes);
    }
    out
}

/// Python `bytes.find`, returning a byte offset or `-1`.
#[must_use]
pub fn find(haystack: &[u8], needle: &[u8]) -> isize {
    if needle.is_empty() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
        .map_or(-1, |index| index as isize)
}

/// Python `bytes.startswith` for one prefix.
#[must_use]
pub fn startswith(bytes: &[u8], prefix: &[u8]) -> bool {
    bytes.starts_with(prefix)
}

/// Python `bytes.replace` for the common unlimited-count path.
#[must_use]
pub fn replace(bytes: &[u8], old: &[u8], new: &[u8]) -> Vec<u8> {
    if old.is_empty() {
        let mut out = Vec::with_capacity(bytes.len() + new.len().saturating_mul(bytes.len() + 1));
        out.extend_from_slice(new);
        for byte in bytes {
            out.push(*byte);
            out.extend_from_slice(new);
        }
        return out;
    }

    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some(pos) = rest.windows(old.len()).position(|window| window == old) {
        out.extend_from_slice(&rest[..pos]);
        out.extend_from_slice(new);
        rest = &rest[pos + old.len()..];
    }
    out.extend_from_slice(rest);
    out
}

/// Python `bytes.split` for a concrete separator or ASCII whitespace.
#[must_use]
pub fn split(bytes: &[u8], sep: Option<&[u8]>) -> Vec<Vec<u8>> {
    match sep {
        Some(sep) => split_on(bytes, sep),
        None => bytes
            .split(u8::is_ascii_whitespace)
            .filter(|part| !part.is_empty())
            .map(<[u8]>::to_vec)
            .collect(),
    }
}

fn split_on(bytes: &[u8], sep: &[u8]) -> Vec<Vec<u8>> {
    if sep.is_empty() {
        return vec![bytes.to_vec()];
    }
    let mut parts = Vec::new();
    let mut rest = bytes;
    while let Some(pos) = rest.windows(sep.len()).position(|window| window == sep) {
        parts.push(rest[..pos].to_vec());
        rest = &rest[pos + sep.len()..];
    }
    parts.push(rest.to_vec());
    parts
}

/// Python `bytes.join` over bytes-like items.
#[must_use]
pub fn join(sep: &[u8], items: &[Vec<u8>]) -> Vec<u8> {
    let payload_len: usize = items.iter().map(Vec::len).sum();
    let sep_len = sep.len().saturating_mul(items.len().saturating_sub(1));
    let mut out = Vec::with_capacity(payload_len + sep_len);
    for (index, item) in items.iter().enumerate() {
        if index != 0 {
            out.extend_from_slice(sep);
        }
        out.extend_from_slice(item);
    }
    out
}

/// CPython-shaped `repr(bytes)` for representative byte payloads.
#[must_use]
pub fn repr(bytes: &[u8]) -> String {
    let contains_single = bytes.contains(&b'\'');
    let contains_double = bytes.contains(&b'"');
    let quote = if contains_single && !contains_double { b'"' } else { b'\'' };
    let mut out = String::with_capacity(bytes.len() + 3);
    out.push('b');
    out.push(char::from(quote));
    for byte in bytes {
        match *byte {
            b'\\' => out.push_str("\\\\"),
            b'\'' if quote == b'\'' => out.push_str("\\'"),
            b'"' if quote == b'"' => out.push_str("\\\""),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7e => out.push(char::from(*byte)),
            byte => out.push_str(&format!("\\x{byte:02x}")),
        }
    }
    out.push(char::from(quote));
    out
}

/// Formats a list of bytes like CPython's list repr.
#[must_use]
pub fn repr_bytes_list(items: &[Vec<u8>]) -> String {
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
    fn bytes_repr_escapes_non_printable_bytes() {
        assert_eq!(repr(b"a\n\xff"), "b'a\\n\\xff'");
    }

    #[test]
    fn split_replace_and_find_use_byte_offsets() {
        assert_eq!(split(b"a--b--", Some(b"--")), vec![b"a".to_vec(), b"b".to_vec(), b"".to_vec()]);
        assert_eq!(replace(b"banana", b"na", b"X"), b"baXX".to_vec());
        assert_eq!(find(b"\xc3\xa9x", b"x"), 2);
    }
}
