//! Mutable bytearray implementation.
//!
//! WS-STR keeps bytearray as an owned mutable byte vector with Python-shaped
//! formatting and method helpers.  Hub integration can later move allocation
//! into the GC without changing this operation layer.

use std::{ptr, sync::LazyLock};

use crate::{
	object::{PyObjectHeader, PyType},
	types::bytes_,
};

/// Boxed mutable Python `bytearray` value used by WS-STR helper entry points.
#[repr(C)]
#[derive(Debug)]
pub struct PyByteArray {
	/// Common object header; this field must remain first.
	pub ob_base: PyObjectHeader,
	/// Owned byte storage.
	pub bytes:   Vec<u8>,
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
	let ty = Box::new(PyType::new(ptr::null(), "bytearray", core::mem::size_of::<PyByteArray>()));
	Box::into_raw(ty) as usize
});

/// Returns the process-lifetime type descriptor used for boxed bytearray
/// helpers.
#[must_use]
pub fn bytearray_type() -> *mut PyType {
	*BYTEARRAY_TYPE as *mut PyType
}

/// Allocates a boxed bytearray object outside the GC-managed heap.
#[must_use]
pub fn boxed_bytearray(bytes: &[u8]) -> *mut PyByteArray {
	Box::into_raw(Box::new(PyByteArray {
		ob_base: PyObjectHeader::new(bytearray_type()),
		bytes:   bytes.to_vec(),
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

#[must_use]
pub fn find_range(bytes: &[u8], needle: &[u8], start: usize, end: usize) -> Option<isize> {
	bytes_::find_range(bytes, needle, start, end)
}

#[must_use]
pub fn rfind_range(bytes: &[u8], needle: &[u8], start: usize, end: usize) -> Option<isize> {
	bytes_::rfind_range(bytes, needle, start, end)
}

/// Python `bytearray.startswith` for one prefix.
#[must_use]
pub fn startswith(bytes: &[u8], prefix: &[u8]) -> bool {
	bytes_::startswith(bytes, prefix)
}

#[must_use]
pub fn startswith_range(bytes: &[u8], prefix: &[u8], start: usize, end: usize) -> bool {
	bytes_::startswith_range(bytes, prefix, start, end)
}

#[must_use]
pub fn endswith_range(bytes: &[u8], suffix: &[u8], start: usize, end: usize) -> bool {
	bytes_::endswith_range(bytes, suffix, start, end)
}

#[must_use]
pub fn count_range(bytes: &[u8], needle: &[u8], start: usize, end: usize) -> usize {
	bytes_::count_range(bytes, needle, start, end)
}

/// Python `bytearray.replace` for the common unlimited-count path.
#[must_use]
pub fn replace(bytes: &[u8], old: &[u8], new: &[u8]) -> Vec<u8> {
	bytes_::replace(bytes, old, new)
}

#[must_use]
pub fn replace_count(bytes: &[u8], old: &[u8], new: &[u8], count: Option<isize>) -> Vec<u8> {
	bytes_::replace_count(bytes, old, new, count)
}

/// Python `bytearray.split` for a concrete separator or ASCII whitespace.
#[must_use]
pub fn split(bytes: &[u8], sep: Option<&[u8]>) -> Vec<Vec<u8>> {
	bytes_::split(bytes, sep)
}

#[must_use]
pub fn split_limited(bytes: &[u8], sep: Option<&[u8]>, maxsplit: isize) -> Vec<Vec<u8>> {
	bytes_::split_limited(bytes, sep, maxsplit)
}

#[must_use]
pub fn rsplit_limited(bytes: &[u8], sep: Option<&[u8]>, maxsplit: isize) -> Vec<Vec<u8>> {
	bytes_::rsplit_limited(bytes, sep, maxsplit)
}

#[must_use]
pub fn splitlines(bytes: &[u8], keepends: bool) -> Vec<Vec<u8>> {
	bytes_::splitlines(bytes, keepends)
}

/// Python `bytearray.join` over bytes-like items.
#[must_use]
pub fn join(sep: &[u8], items: &[Vec<u8>]) -> Vec<u8> {
	bytes_::join(sep, items)
}

#[must_use]
pub fn strip(bytes: &[u8], chars: Option<&[u8]>) -> Vec<u8> {
	bytes_::strip(bytes, chars)
}

#[must_use]
pub fn lstrip(bytes: &[u8], chars: Option<&[u8]>) -> Vec<u8> {
	bytes_::lstrip(bytes, chars)
}

#[must_use]
pub fn rstrip(bytes: &[u8], chars: Option<&[u8]>) -> Vec<u8> {
	bytes_::rstrip(bytes, chars)
}

#[must_use]
pub fn lower(bytes: &[u8]) -> Vec<u8> {
	bytes_::lower(bytes)
}

#[must_use]
pub fn upper(bytes: &[u8]) -> Vec<u8> {
	bytes_::upper(bytes)
}

#[must_use]
pub fn capitalize(bytes: &[u8]) -> Vec<u8> {
	bytes_::capitalize(bytes)
}

#[must_use]
pub fn swapcase(bytes: &[u8]) -> Vec<u8> {
	bytes_::swapcase(bytes)
}

#[must_use]
pub fn title(bytes: &[u8]) -> Vec<u8> {
	bytes_::title(bytes)
}

#[must_use]
pub fn center(bytes: &[u8], width: usize, fill: u8) -> Vec<u8> {
	bytes_::center(bytes, width, fill)
}

#[must_use]
pub fn ljust(bytes: &[u8], width: usize, fill: u8) -> Vec<u8> {
	bytes_::ljust(bytes, width, fill)
}

#[must_use]
pub fn rjust(bytes: &[u8], width: usize, fill: u8) -> Vec<u8> {
	bytes_::rjust(bytes, width, fill)
}

#[must_use]
pub fn zfill(bytes: &[u8], width: usize) -> Vec<u8> {
	bytes_::zfill(bytes, width)
}

#[must_use]
pub fn expandtabs(bytes: &[u8], tabsize: isize) -> Vec<u8> {
	bytes_::expandtabs(bytes, tabsize)
}

#[must_use]
pub fn partition(bytes: &[u8], sep: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
	bytes_::partition(bytes, sep)
}

#[must_use]
pub fn rpartition(bytes: &[u8], sep: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
	bytes_::rpartition(bytes, sep)
}

#[must_use]
pub fn removeprefix(bytes: &[u8], prefix: &[u8]) -> Vec<u8> {
	bytes_::removeprefix(bytes, prefix)
}

#[must_use]
pub fn removesuffix(bytes: &[u8], suffix: &[u8]) -> Vec<u8> {
	bytes_::removesuffix(bytes, suffix)
}

#[must_use]
pub fn hex(bytes: &[u8]) -> String {
	bytes_::hex(bytes)
}

pub fn fromhex(text: &str) -> Result<Vec<u8>, String> {
	bytes_::fromhex(text)
}

#[must_use]
pub fn is_alpha(bytes: &[u8]) -> bool {
	bytes_::is_alpha(bytes)
}

#[must_use]
pub fn is_alnum(bytes: &[u8]) -> bool {
	bytes_::is_alnum(bytes)
}

#[must_use]
pub fn is_digit(bytes: &[u8]) -> bool {
	bytes_::is_digit(bytes)
}

#[must_use]
pub fn is_space(bytes: &[u8]) -> bool {
	bytes_::is_space(bytes)
}

#[must_use]
pub fn is_ascii(bytes: &[u8]) -> bool {
	bytes_::is_ascii(bytes)
}

#[must_use]
pub fn is_lower(bytes: &[u8]) -> bool {
	bytes_::is_lower(bytes)
}

#[must_use]
pub fn is_upper(bytes: &[u8]) -> bool {
	bytes_::is_upper(bytes)
}

#[must_use]
pub fn is_title(bytes: &[u8]) -> bool {
	bytes_::is_title(bytes)
}

pub fn append(array: &mut PyByteArray, value: u8) {
	array.bytes.push(value);
}

pub fn extend(array: &mut PyByteArray, values: &[u8]) {
	array.bytes.extend_from_slice(values);
}

pub fn insert(array: &mut PyByteArray, index: isize, value: u8) {
	let len = array.bytes.len() as isize;
	let mut index = if index < 0 {
		index.saturating_add(len)
	} else {
		index
	};
	index = index.clamp(0, len);
	array.bytes.insert(index as usize, value);
}

pub fn pop(array: &mut PyByteArray, index: Option<isize>) -> Result<u8, String> {
	if array.bytes.is_empty() {
		return Err("pop from empty bytearray".to_owned());
	}
	let index = normalize_existing_index(index.unwrap_or(-1), array.bytes.len())
		.map_err(|_| "pop index out of range".to_owned())?;
	Ok(array.bytes.remove(index))
}

pub fn remove(array: &mut PyByteArray, value: u8) -> Result<(), String> {
	let Some(index) = array.bytes.iter().position(|byte| *byte == value) else {
		return Err("value not found in bytearray".to_owned());
	};
	array.bytes.remove(index);
	Ok(())
}

pub fn clear(array: &mut PyByteArray) {
	array.bytes.clear();
}

pub fn resize(array: &mut PyByteArray, size: usize) {
	array.bytes.resize(size, 0);
}

pub fn set_index(array: &mut PyByteArray, index: isize, value: u8) -> Result<(), String> {
	let index = normalize_existing_index(index, array.bytes.len())?;
	array.bytes[index] = value;
	Ok(())
}

pub fn set_slice(array: &mut PyByteArray, start: usize, end: usize, values: &[u8]) {
	let start = start.min(array.bytes.len());
	let end = end.min(array.bytes.len()).max(start);
	array.bytes.splice(start..end, values.iter().copied());
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

fn normalize_existing_index(index: isize, len: usize) -> Result<usize, String> {
	let len_isize =
		isize::try_from(len).map_err(|_| "bytearray is too large for this platform".to_owned())?;
	let index = if index < 0 {
		index.saturating_add(len_isize)
	} else {
		index
	};
	if index < 0 || index >= len_isize {
		Err("bytearray index out of range".to_owned())
	} else {
		Ok(index as usize)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn repr_matches_cpython_shape() {
		assert_eq!(repr(b"a\n"), "bytearray(b'a\\n')");
	}

	#[test]
	fn mutators_update_backing_vec() {
		let mut array =
			PyByteArray { ob_base: PyObjectHeader::new(bytearray_type()), bytes: b"abc".to_vec() };
		append(&mut array, b'd');
		insert(&mut array, 1, b'Z');
		set_slice(&mut array, 2, 4, b"YY");
		assert_eq!(array.bytes, b"aZYYd");
		assert_eq!(pop(&mut array, None).unwrap(), b'd');
		remove(&mut array, b'Z').unwrap();
		assert_eq!(array.bytes, b"aYY");
		clear(&mut array);
		assert!(array.bytes.is_empty());
	}
}
