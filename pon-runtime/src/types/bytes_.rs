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
    find_range(haystack, needle, 0, haystack.len()).unwrap_or(-1)
}

#[must_use]
pub fn find_range(bytes: &[u8], needle: &[u8], start: usize, end: usize) -> Option<isize> {
    let (slice, offset) = bounded_slice(bytes, start, end);
    if needle.is_empty() {
        return Some(offset as isize);
    }
    slice.windows(needle.len()).position(|window| window == needle).map(|pos| (offset + pos) as isize)
}

#[must_use]
pub fn rfind_range(bytes: &[u8], needle: &[u8], start: usize, end: usize) -> Option<isize> {
    let (slice, offset) = bounded_slice(bytes, start, end);
    if needle.is_empty() {
        return Some((offset + slice.len()) as isize);
    }
    slice.windows(needle.len()).rposition(|window| window == needle).map(|pos| (offset + pos) as isize)
}

/// Python `bytes.startswith` for one prefix.
#[must_use]
pub fn startswith(bytes: &[u8], prefix: &[u8]) -> bool {
    startswith_range(bytes, prefix, 0, bytes.len())
}

#[must_use]
pub fn startswith_range(bytes: &[u8], prefix: &[u8], start: usize, end: usize) -> bool {
    bounded_slice(bytes, start, end).0.starts_with(prefix)
}

#[must_use]
pub fn endswith_range(bytes: &[u8], suffix: &[u8], start: usize, end: usize) -> bool {
    bounded_slice(bytes, start, end).0.ends_with(suffix)
}

#[must_use]
pub fn count_range(bytes: &[u8], needle: &[u8], start: usize, end: usize) -> usize {
    let (slice, _) = bounded_slice(bytes, start, end);
    if needle.is_empty() {
        return slice.len() + 1;
    }
    let mut count = 0usize;
    let mut rest = slice;
    while let Some(pos) = rest.windows(needle.len()).position(|window| window == needle) {
        count += 1;
        rest = &rest[pos + needle.len()..];
    }
    count
}

/// Python `bytes.replace` for the common unlimited-count path.
#[must_use]
pub fn replace(bytes: &[u8], old: &[u8], new: &[u8]) -> Vec<u8> {
    replace_count(bytes, old, new, None)
}

#[must_use]
pub fn replace_count(bytes: &[u8], old: &[u8], new: &[u8], count: Option<isize>) -> Vec<u8> {
    let limit = count.and_then(|value| usize::try_from(value).ok());
    if matches!(limit, Some(0)) {
        return bytes.to_vec();
    }
    if old.is_empty() {
        let max_insertions = bytes.len() + 1;
        let wanted = limit.unwrap_or(max_insertions).min(max_insertions);
        let mut inserted = 0usize;
        let mut out = Vec::with_capacity(bytes.len() + new.len().saturating_mul(wanted));
        if inserted < wanted {
            out.extend_from_slice(new);
            inserted += 1;
        }
        for byte in bytes {
            out.push(*byte);
            if inserted < wanted {
                out.extend_from_slice(new);
                inserted += 1;
            }
        }
        return out;
    }

    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    let mut done = 0usize;
    while limit.is_none_or(|max| done < max) {
        let Some(pos) = rest.windows(old.len()).position(|window| window == old) else { break; };
        out.extend_from_slice(&rest[..pos]);
        out.extend_from_slice(new);
        rest = &rest[pos + old.len()..];
        done += 1;
    }
    out.extend_from_slice(rest);
    out
}

/// Python `bytes.split` for a concrete separator or ASCII whitespace.
#[must_use]
pub fn split(bytes: &[u8], sep: Option<&[u8]>) -> Vec<Vec<u8>> {
    split_limited(bytes, sep, -1)
}

#[must_use]
pub fn split_limited(bytes: &[u8], sep: Option<&[u8]>, maxsplit: isize) -> Vec<Vec<u8>> {
    match sep {
        Some(sep) => split_on(bytes, sep, maxsplit),
        None => split_whitespace(bytes, maxsplit),
    }
}

#[must_use]
pub fn rsplit_limited(bytes: &[u8], sep: Option<&[u8]>, maxsplit: isize) -> Vec<Vec<u8>> {
    match sep {
        Some(sep) => rsplit_on(bytes, sep, maxsplit),
        None => rsplit_whitespace(bytes, maxsplit),
    }
}

#[must_use]
pub fn splitlines(bytes: &[u8], keepends: bool) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let mut next = i + 1;
        let end = match bytes[i] {
            b'\n' | 0x0B | 0x0C | 0x1C | 0x1D | 0x1E | 0x85 => Some(if keepends { next } else { i }),
            b'\r' => {
                if next < bytes.len() && bytes[next] == b'\n' {
                    next += 1;
                }
                Some(if keepends { next } else { i })
            }
            _ => None,
        };
        if let Some(end) = end {
            out.push(bytes[start..end].to_vec());
            start = next;
        }
        i = next;
    }
    if start < bytes.len() {
        out.push(bytes[start..].to_vec());
    }
    out
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

#[must_use]
pub fn strip(bytes: &[u8], chars: Option<&[u8]>) -> Vec<u8> {
    strip_sides(bytes, chars, true, true)
}

#[must_use]
pub fn lstrip(bytes: &[u8], chars: Option<&[u8]>) -> Vec<u8> {
    strip_sides(bytes, chars, true, false)
}

#[must_use]
pub fn rstrip(bytes: &[u8], chars: Option<&[u8]>) -> Vec<u8> {
    strip_sides(bytes, chars, false, true)
}

#[must_use]
pub fn lower(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(u8::to_ascii_lowercase).collect()
}

#[must_use]
pub fn upper(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(u8::to_ascii_uppercase).collect()
}

#[must_use]
pub fn capitalize(bytes: &[u8]) -> Vec<u8> {
    let Some((&first, rest)) = bytes.split_first() else { return Vec::new(); };
    let mut out = Vec::with_capacity(bytes.len());
    out.push(first.to_ascii_uppercase());
    out.extend(rest.iter().map(u8::to_ascii_lowercase));
    out
}

#[must_use]
pub fn swapcase(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(|byte| {
        if byte.is_ascii_lowercase() {
            byte.to_ascii_uppercase()
        } else if byte.is_ascii_uppercase() {
            byte.to_ascii_lowercase()
        } else {
            *byte
        }
    }).collect()
}

#[must_use]
pub fn title(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut new_word = true;
    for byte in bytes {
        if byte.is_ascii_alphanumeric() {
            if new_word {
                out.push(byte.to_ascii_uppercase());
                new_word = false;
            } else {
                out.push(byte.to_ascii_lowercase());
            }
        } else {
            out.push(*byte);
            new_word = true;
        }
    }
    out
}

#[must_use]
pub fn center(bytes: &[u8], width: usize, fill: u8) -> Vec<u8> {
    pad(bytes, width, fill, PadMode::Center)
}

#[must_use]
pub fn ljust(bytes: &[u8], width: usize, fill: u8) -> Vec<u8> {
    pad(bytes, width, fill, PadMode::Left)
}

#[must_use]
pub fn rjust(bytes: &[u8], width: usize, fill: u8) -> Vec<u8> {
    pad(bytes, width, fill, PadMode::Right)
}

#[must_use]
pub fn zfill(bytes: &[u8], width: usize) -> Vec<u8> {
    if width <= bytes.len() {
        return bytes.to_vec();
    }
    let fill = width - bytes.len();
    let mut out = Vec::with_capacity(width);
    if matches!(bytes.first(), Some(b'+') | Some(b'-')) {
        out.push(bytes[0]);
        out.extend(std::iter::repeat_n(b'0', fill));
        out.extend_from_slice(&bytes[1..]);
    } else {
        out.extend(std::iter::repeat_n(b'0', fill));
        out.extend_from_slice(bytes);
    }
    out
}

#[must_use]
pub fn expandtabs(bytes: &[u8], tabsize: isize) -> Vec<u8> {
    let tabsize = usize::try_from(tabsize).unwrap_or(0);
    let mut out = Vec::with_capacity(bytes.len());
    let mut column = 0usize;
    for byte in bytes {
        match *byte {
            b'\t' => {
                let spaces = if tabsize == 0 { 0 } else { tabsize - (column % tabsize) };
                out.extend(std::iter::repeat_n(b' ', spaces));
                column += spaces;
            }
            b'\n' | b'\r' => {
                out.push(*byte);
                column = 0;
            }
            _ => {
                out.push(*byte);
                column += 1;
            }
        }
    }
    out
}

#[must_use]
pub fn partition(bytes: &[u8], sep: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    match find_range(bytes, sep, 0, bytes.len()) {
        Some(pos) if !sep.is_empty() => {
            let pos = pos as usize;
            (bytes[..pos].to_vec(), sep.to_vec(), bytes[pos + sep.len()..].to_vec())
        }
        _ => (bytes.to_vec(), Vec::new(), Vec::new()),
    }
}

#[must_use]
pub fn rpartition(bytes: &[u8], sep: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    match rfind_range(bytes, sep, 0, bytes.len()) {
        Some(pos) if !sep.is_empty() => {
            let pos = pos as usize;
            (bytes[..pos].to_vec(), sep.to_vec(), bytes[pos + sep.len()..].to_vec())
        }
        _ => (Vec::new(), Vec::new(), bytes.to_vec()),
    }
}

#[must_use]
pub fn removeprefix(bytes: &[u8], prefix: &[u8]) -> Vec<u8> {
    bytes.strip_prefix(prefix).unwrap_or(bytes).to_vec()
}

#[must_use]
pub fn removesuffix(bytes: &[u8], suffix: &[u8]) -> Vec<u8> {
    bytes.strip_suffix(suffix).unwrap_or(bytes).to_vec()
}

/// Python `bytes.translate`: deletes `delete` members, then maps every
/// remaining byte through the 256-entry `table` (`None` keeps bytes as-is).
pub fn translate(bytes: &[u8], table: Option<&[u8]>, delete: &[u8]) -> Result<Vec<u8>, String> {
    if let Some(table) = table {
        if table.len() != 256 {
            return Err("translation table must be 256 characters long".to_owned());
        }
    }
    let mut out = Vec::with_capacity(bytes.len());
    for byte in bytes {
        if delete.contains(byte) {
            continue;
        }
        out.push(match table {
            Some(table) => table[usize::from(*byte)],
            None => *byte,
        });
    }
    Ok(out)
}

#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(nibble_to_hex(byte >> 4));
        out.push(nibble_to_hex(byte & 0x0f));
    }
    out
}

pub fn fromhex(text: &str) -> Result<Vec<u8>, String> {
    let digits = text.chars().filter(|ch| ch.is_ascii_whitespace()).fold(text.chars().count(), |acc, _| acc - 1);
    if digits % 2 != 0 {
        return Err("non-hexadecimal number found in fromhex() arg at position 0".to_owned());
    }
    let mut out = Vec::with_capacity(digits / 2);
    let mut high: Option<u8> = None;
    for ch in text.chars() {
        if ch.is_ascii_whitespace() {
            continue;
        }
        let Some(value) = ch.to_digit(16) else {
            return Err("non-hexadecimal number found in fromhex() arg".to_owned());
        };
        let value = value as u8;
        if let Some(hi) = high.take() {
            out.push((hi << 4) | value);
        } else {
            high = Some(value);
        }
    }
    Ok(out)
}

#[must_use]
pub fn is_alpha(bytes: &[u8]) -> bool {
    nonempty_all(bytes, u8::is_ascii_alphabetic)
}

#[must_use]
pub fn is_alnum(bytes: &[u8]) -> bool {
    nonempty_all(bytes, u8::is_ascii_alphanumeric)
}

#[must_use]
pub fn is_digit(bytes: &[u8]) -> bool {
    nonempty_all(bytes, u8::is_ascii_digit)
}

#[must_use]
pub fn is_space(bytes: &[u8]) -> bool {
    nonempty_all(bytes, u8::is_ascii_whitespace)
}

#[must_use]
pub fn is_ascii(bytes: &[u8]) -> bool {
    bytes.is_ascii()
}

#[must_use]
pub fn is_lower(bytes: &[u8]) -> bool {
    let mut cased = false;
    for byte in bytes {
        if byte.is_ascii_uppercase() {
            return false;
        }
        if byte.is_ascii_lowercase() {
            cased = true;
        }
    }
    cased
}

#[must_use]
pub fn is_upper(bytes: &[u8]) -> bool {
    let mut cased = false;
    for byte in bytes {
        if byte.is_ascii_lowercase() {
            return false;
        }
        if byte.is_ascii_uppercase() {
            cased = true;
        }
    }
    cased
}

#[must_use]
pub fn is_title(bytes: &[u8]) -> bool {
    let mut cased = false;
    let mut expect_upper = true;
    for byte in bytes {
        if byte.is_ascii_uppercase() {
            if !expect_upper {
                return false;
            }
            cased = true;
            expect_upper = false;
        } else if byte.is_ascii_lowercase() {
            if expect_upper {
                return false;
            }
            cased = true;
        } else if !byte.is_ascii_alphabetic() {
            expect_upper = true;
        }
    }
    cased
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

fn bounded_slice(bytes: &[u8], start: usize, end: usize) -> (&[u8], usize) {
    let start = start.min(bytes.len());
    let end = end.min(bytes.len()).max(start);
    (&bytes[start..end], start)
}

fn split_on(bytes: &[u8], sep: &[u8], maxsplit: isize) -> Vec<Vec<u8>> {
    if sep.is_empty() {
        return vec![bytes.to_vec()];
    }
    let limit = usize::try_from(maxsplit).ok();
    let mut parts = Vec::new();
    let mut rest = bytes;
    while limit.is_none_or(|max| parts.len() < max) {
        let Some(pos) = rest.windows(sep.len()).position(|window| window == sep) else { break; };
        parts.push(rest[..pos].to_vec());
        rest = &rest[pos + sep.len()..];
    }
    parts.push(rest.to_vec());
    parts
}

fn rsplit_on(bytes: &[u8], sep: &[u8], maxsplit: isize) -> Vec<Vec<u8>> {
    if sep.is_empty() {
        return vec![bytes.to_vec()];
    }
    let limit = usize::try_from(maxsplit).ok();
    let mut parts = Vec::new();
    let mut rest = bytes;
    while limit.is_none_or(|max| parts.len() < max) {
        let Some(pos) = rest.windows(sep.len()).rposition(|window| window == sep) else { break; };
        parts.push(rest[pos + sep.len()..].to_vec());
        rest = &rest[..pos];
    }
    parts.push(rest.to_vec());
    parts.reverse();
    parts
}

fn split_whitespace(bytes: &[u8], maxsplit: isize) -> Vec<Vec<u8>> {
    let limit = usize::try_from(maxsplit).ok();
    if matches!(limit, Some(0)) {
        let start = bytes.iter().position(|byte| !byte.is_ascii_whitespace()).unwrap_or(bytes.len());
        return if start == bytes.len() { Vec::new() } else { vec![bytes[start..].to_vec()] };
    }
    let mut parts = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i == bytes.len() {
            break;
        }
        if limit.is_some_and(|max| parts.len() >= max) {
            parts.push(bytes[i..].to_vec());
            return parts;
        }
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        parts.push(bytes[start..i].to_vec());
    }
    parts
}

fn rsplit_whitespace(bytes: &[u8], maxsplit: isize) -> Vec<Vec<u8>> {
    let limit = usize::try_from(maxsplit).ok();
    if matches!(limit, Some(0)) {
        let end = bytes.iter().rposition(|byte| !byte.is_ascii_whitespace()).map_or(0, |pos| pos + 1);
        return if end == 0 { Vec::new() } else { vec![bytes[..end].to_vec()] };
    }
    let mut parts = Vec::new();
    let mut i = bytes.len();
    while i > 0 {
        while i > 0 && bytes[i - 1].is_ascii_whitespace() {
            i -= 1;
        }
        if i == 0 {
            break;
        }
        if limit.is_some_and(|max| parts.len() >= max) {
            parts.push(bytes[..i].to_vec());
            break;
        }
        let end = i;
        while i > 0 && !bytes[i - 1].is_ascii_whitespace() {
            i -= 1;
        }
        parts.push(bytes[i..end].to_vec());
    }
    parts.reverse();
    parts
}

fn strip_sides(bytes: &[u8], chars: Option<&[u8]>, left: bool, right: bool) -> Vec<u8> {
    let should_strip = |byte: u8| match chars {
        Some(chars) => chars.contains(&byte),
        None => byte.is_ascii_whitespace(),
    };
    let mut start = 0usize;
    let mut end = bytes.len();
    if left {
        while start < end && should_strip(bytes[start]) {
            start += 1;
        }
    }
    if right {
        while end > start && should_strip(bytes[end - 1]) {
            end -= 1;
        }
    }
    bytes[start..end].to_vec()
}

#[derive(Clone, Copy)]
enum PadMode {
    Left,
    Right,
    Center,
}

fn pad(bytes: &[u8], width: usize, fill: u8, mode: PadMode) -> Vec<u8> {
    if width <= bytes.len() {
        return bytes.to_vec();
    }
    let total = width - bytes.len();
    let (left, right) = match mode {
        PadMode::Left => (0, total),
        PadMode::Right => (total, 0),
        PadMode::Center => (total / 2, total - (total / 2)),
    };
    let mut out = Vec::with_capacity(width);
    out.extend(std::iter::repeat_n(fill, left));
    out.extend_from_slice(bytes);
    out.extend(std::iter::repeat_n(fill, right));
    out
}

fn nibble_to_hex(value: u8) -> char {
    char::from(if value < 10 { b'0' + value } else { b'a' + value - 10 })
}

fn nonempty_all(bytes: &[u8], predicate: impl Fn(&u8) -> bool) -> bool {
    let mut iter = bytes.iter();
    let Some(first) = iter.next() else { return false; };
    predicate(first) && iter.all(predicate)
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
        assert_eq!(rsplit_limited(b"a--b--c", Some(b"--"), 1), vec![b"a--b".to_vec(), b"c".to_vec()]);
        assert_eq!(replace_count(b"banana", b"na", b"X", Some(1)), b"baXna".to_vec());
        assert_eq!(find(b"\xc3\xa9x", b"x"), 2);
    }

    #[test]
    fn hex_decode_and_ascii_case_match_python_surface() {
        assert_eq!(hex(b"\x00\x7f\xff"), "007fff");
        assert_eq!(fromhex("00 7f ff").unwrap(), b"\x00\x7f\xff");
        assert_eq!(title(b"hello world"), b"Hello World".to_vec());
    }
}
