//! Minimal flat memoryview implementation over Pon bytes/bytearray and native
//! `array.array` exporters.
//!
//! The view stores a borrowed base object pointer plus a raw contiguous byte
//! window.  Bytes-backed views are readonly; bytearray- and array-backed views
//! are writable until their exporter reallocates.  The surface covers basic
//! construction, len/index/slice, `tobytes()`, readonly enforcement, and the
//! flat `cast`/`itemsize`/`tolist` trio.  Native-endian numeric PEP-3118
//! formats are decoded honestly; the unicode `w` format is exposed but
//! `tolist()` raises `NotImplementedError` at the ABI boundary rather than
//! pretending it is an integer format.

use std::{
	ffi::c_void,
	ptr,
	sync::LazyLock,
};

use crate::{
	native::array as array_,
	object::{PyObject, PyObjectHeader, PyType},
	types::{bytearray_, bytes_},
};

pub const READONLY_WRITE_ERROR: &str = "cannot modify read-only memory";

/// CPython's diagnostic for any operation on a released view.
pub const RELEASED_ERROR: &str = "operation forbidden on released memoryview object";

/// Boxed Python `memoryview` over a contiguous byte window.
#[repr(C)]
#[derive(Debug)]
pub struct PyMemoryView {
	/// Common object header; this field must remain first.
	pub ob_base:  PyObjectHeader,
	/// Borrowed exporter object, kept so repr/tracing seams can retain identity
	/// later.
	pub base:     *mut PyObject,
	/// Raw byte pointer for the first visible element.
	pub data:     *mut u8,
	/// Visible byte length.
	pub len:      usize,
	/// Whether writes through this view are forbidden.
	pub readonly: bool,
	/// Element format code (`B` for byte views, numeric/native PEP-3118 codes
	/// for array views).  Only codes accepted by [`item_width`] are stored.
	pub format:   u8,
	/// Optional owned `Py_buffer` copy for foreign exporters.  It is opaque to
	/// this module; the C-API layer supplies the matching release callback.
	pub foreign_buffer: *mut c_void,
	/// Releases `foreign_buffer` exactly once when `release()` is called.
	pub foreign_release: Option<unsafe extern "C" fn(*mut PyObject, *mut c_void)>,
	/// Whether `release()` (or `__exit__`) ran; every subsequent buffer
	/// operation raises `ValueError` with [`RELEASED_ERROR`].
	pub released: bool,
}

impl PyMemoryView {
	/// Returns the visible bytes.
	///
	/// # Safety
	///
	/// The exporter must outlive this view and the stored pointer must cover
	/// `len` bytes.
	#[must_use]
	pub unsafe fn as_slice(&self) -> &[u8] {
		if self.data.is_null() && self.len != 0 {
			return &[];
		}
		unsafe { core::slice::from_raw_parts(self.data.cast_const(), self.len) }
	}

	/// Returns the visible bytes mutably when the exporter is writable.
	///
	/// # Safety
	///
	/// The exporter must be mutable, unique for the duration of the borrow, and
	/// cover `len` bytes.
	pub unsafe fn as_mut_slice(&mut self) -> Result<&mut [u8], String> {
		if self.readonly {
			return Err(READONLY_WRITE_ERROR.to_owned());
		}
		if self.data.is_null() && self.len != 0 {
			return Err("memoryview data pointer is null".to_owned());
		}
		Ok(unsafe { core::slice::from_raw_parts_mut(self.data, self.len) })
	}

	/// Returns the per-element width in bytes for this view's format.
	#[must_use]
	pub fn itemsize(&self) -> usize {
		item_width(self.format).unwrap_or(1)
	}
}

static MEMORYVIEW_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let ty = Box::new(PyType::new(ptr::null(), "memoryview", core::mem::size_of::<PyMemoryView>()));
	Box::into_raw(ty) as usize
});

/// Returns the process-lifetime memoryview type descriptor.
#[must_use]
pub fn memoryview_type() -> *mut PyType {
	*MEMORYVIEW_TYPE as *mut PyType
}

/// Returns true when `object_type` is the K2 memoryview type descriptor.
#[must_use]
pub fn is_memoryview_type(object_type: *const PyType) -> bool {
	object_type == memoryview_type().cast_const()
}

/// Allocates a memoryview from an already-normalized contiguous byte window.
#[must_use]
pub fn boxed_memoryview_from_raw(
	base: *mut PyObject,
	data: *mut u8,
	len: usize,
	readonly: bool,
	format: u8,
) -> *mut PyMemoryView {
	boxed_memoryview_from_exporter(base, data, len, readonly, format, ptr::null_mut(), None)
}

/// Allocates a memoryview over a contiguous exporter window, optionally owning
/// an opaque C `Py_buffer` copy whose release callback must run on
/// `memoryview.release()`.
#[must_use]
pub fn boxed_memoryview_from_exporter(
	base: *mut PyObject,
	data: *mut u8,
	len: usize,
	readonly: bool,
	format: u8,
	foreign_buffer: *mut c_void,
	foreign_release: Option<unsafe extern "C" fn(*mut PyObject, *mut c_void)>,
) -> *mut PyMemoryView {
	Box::into_raw(Box::new(PyMemoryView {
		ob_base: PyObjectHeader::new(memoryview_type()),
		base,
		data,
		len,
		readonly,
		format,
		foreign_buffer,
		foreign_release,
		released: false,
	}))
}

/// Constructs a memoryview over bytes, bytearray, or another memoryview.
pub unsafe fn boxed_memoryview_from_object(
	object: *mut PyObject,
) -> Result<*mut PyMemoryView, String> {
	if object.is_null() {
		return Err("memoryview() argument is NULL".to_owned());
	}
	let ty = unsafe { (*object).ob_type };
	if bytes_::is_bytes_type(ty) {
		let bytes = unsafe { &*object.cast::<bytes_::PyBytes>() };
		let slice = unsafe { bytes.as_slice() };
		return Ok(boxed_memoryview_from_raw(
			object,
			slice.as_ptr().cast_mut(),
			slice.len(),
			true,
			b'B',
		));
	}
	if bytearray_::is_bytearray_type(ty) {
		let bytearray = unsafe { &mut *object.cast::<bytearray_::PyByteArray>() };
		return Ok(boxed_memoryview_from_raw(
			object,
			bytearray.bytes.as_mut_ptr(),
			bytearray.bytes.len(),
			false,
			b'B',
		));
	}
	if let Some(array) = unsafe { array_::buffer_view(object) } {
		debug_assert_eq!(array.len % array.itemsize, 0);
		return Ok(boxed_memoryview_from_raw(
			object,
			array.data,
			array.len,
			false,
			array.format,
		));
	}
	if is_memoryview_type(ty) {
		let view = unsafe { &*object.cast::<PyMemoryView>() };
		if view.released {
			return Err(RELEASED_ERROR.to_owned());
		}
		return Ok(boxed_memoryview_from_raw(
			view.base,
			view.data,
			view.len,
			view.readonly,
			view.format,
		));
	}
	Err("memoryview() argument must be a bytes-like object".to_owned())
}

/// Runs the foreign-exporter release callback for a view that owns a copied
/// `Py_buffer`.  Idempotent; callers still mark the view released separately.
pub unsafe fn release_foreign_export(view: &mut PyMemoryView) {
	let Some(release) = view.foreign_release.take() else {
		return;
	};
	let buffer = view.foreign_buffer;
	view.foreign_buffer = ptr::null_mut();
	if !buffer.is_null() {
		unsafe { release(view.base, buffer) };
	}
}

/// Copies the visible byte window into an owned bytes vector.
#[must_use]
pub unsafe fn tobytes(view: *mut PyMemoryView) -> Vec<u8> {
	unsafe { (*view).as_slice().to_vec() }
}

/// Byte width of the supported flat native-format codes.
///
/// The set matches the numeric `array.array` buffer formats that Pon stores in
/// raw native-endian form.  The unicode `w` format has a known width so
/// `itemsize`, `shape`, and `tobytes()` stay honest, but element decoding is
/// rejected by [`tolist`] because CPython also refuses `memoryview(...).tolist`
/// for `w`.
#[must_use]
pub fn item_width(format: u8) -> Option<usize> {
	match format {
		b'b' | b'B' => Some(1),
		b'h' | b'H' => Some(2),
		b'i' | b'I' | b'f' | b'w' => Some(4),
		b'l' | b'L' | b'q' | b'Q' | b'd' => Some(8),
		_ => None,
	}
}

/// Python `memoryview.cast(format)` over a flat contiguous byte window.
pub fn cast(view: &PyMemoryView, format: &str) -> Result<*mut PyMemoryView, String> {
	let [code] = format.as_bytes() else {
		return Err("memoryview: destination format must be a single character".to_owned());
	};
	let Some(width) = item_width(*code) else {
		return Err(format!("memoryview.cast does not support format '{format}'"));
	};
	if view.itemsize() != 1 && width != 1 {
		return Err("memoryview: cannot cast between two non-byte formats".to_owned());
	}
	if view.len % width != 0 {
		return Err("memoryview: length is not a multiple of itemsize".to_owned());
	}
	Ok(boxed_memoryview_from_raw(view.base, view.data, view.len, view.readonly, *code))
}

/// One scalar produced by `memoryview.tolist()`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MemoryViewListValue {
	/// Signed integer element.
	Int(i64),
	/// Unsigned integer element; the ABI layer boxes values above `i64::MAX`
	/// as wide Python ints.
	Unsigned(u64),
	/// Floating-point element.
	Float(f64),
}

fn element_count(view: &PyMemoryView) -> Result<usize, String> {
	let Some(width) = item_width(view.format) else {
		return Err(format!("memoryview: format {} not supported", char::from(view.format)));
	};
	if view.len % width != 0 {
		return Err("memoryview: length is not a multiple of itemsize".to_owned());
	}
	Ok(view.len / width)
}

/// Decodes one element in this view's native-endian format.
///
/// # Safety
///
/// The exporter must outlive the view and the stored pointer must cover `len`
/// bytes.
pub unsafe fn element_value(
	view: &PyMemoryView,
	index: usize,
) -> Result<MemoryViewListValue, String> {
	if view.format == b'w' {
		return Err("memoryview: format w not supported".to_owned());
	}
	let width = item_width(view.format)
		.ok_or_else(|| format!("memoryview: format {} not supported", char::from(view.format)))?;
	let count = element_count(view)?;
	if index >= count {
		return Err("memoryview index out of range".to_owned());
	}
	let bytes = unsafe { view.as_slice() };
	let offset = index * width;
	let chunk = &bytes[offset..offset + width];
	match view.format {
		b'b' => Ok(MemoryViewListValue::Int(i64::from(i8::from_ne_bytes([chunk[0]])))),
		b'B' => Ok(MemoryViewListValue::Unsigned(u64::from(chunk[0]))),
		b'h' => Ok(MemoryViewListValue::Int(i64::from(i16::from_ne_bytes([
			chunk[0], chunk[1],
		])))),
		b'H' => Ok(MemoryViewListValue::Unsigned(u64::from(u16::from_ne_bytes([
			chunk[0], chunk[1],
		])))),
		b'i' => Ok(MemoryViewListValue::Int(i64::from(i32::from_ne_bytes([
			chunk[0], chunk[1], chunk[2], chunk[3],
		])))),
		b'I' => Ok(MemoryViewListValue::Unsigned(u64::from(u32::from_ne_bytes([
			chunk[0], chunk[1], chunk[2], chunk[3],
		])))),
		b'l' | b'q' => {
			let mut raw = [0_u8; 8];
			raw.copy_from_slice(chunk);
			Ok(MemoryViewListValue::Int(i64::from_ne_bytes(raw)))
		},
		b'L' | b'Q' => {
			let mut raw = [0_u8; 8];
			raw.copy_from_slice(chunk);
			Ok(MemoryViewListValue::Unsigned(u64::from_ne_bytes(raw)))
		},
		b'f' => {
			let mut raw = [0_u8; 4];
			raw.copy_from_slice(chunk);
			Ok(MemoryViewListValue::Float(f64::from(f32::from_ne_bytes(raw))))
		},
		b'd' => {
			let mut raw = [0_u8; 8];
			raw.copy_from_slice(chunk);
			Ok(MemoryViewListValue::Float(f64::from_ne_bytes(raw)))
		},
		_ => Err(format!("memoryview: format {} not supported", char::from(view.format))),
	}
}

/// Python `memoryview.tolist()`: one scalar per element in the view's format.
///
/// # Safety
///
/// The exporter must outlive the view and the stored pointer must cover `len`
/// bytes.
pub unsafe fn tolist(view: &PyMemoryView) -> Result<Vec<MemoryViewListValue>, String> {
	let count = element_count(view)?;
	let mut values = Vec::with_capacity(count);
	for index in 0..count {
		values.push(unsafe { element_value(view, index)? });
	}
	Ok(values)
}
