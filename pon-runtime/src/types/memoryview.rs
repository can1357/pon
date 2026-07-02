//! Minimal memoryview implementation over the K2 bytes/bytearray carriers.
//!
//! The view stores a borrowed base object pointer plus a raw contiguous byte
//! window.  Bytes-backed views are readonly; bytearray-backed views are writable
//! until bytearray reallocation.  The surface covers basic construction,
//! len/index/slice, `tobytes()`, readonly enforcement, and the flat
//! `cast`/`itemsize`/`tolist` trio that re._compiler's `_bytes_to_codes`
//! drives (`memoryview(mapping).cast('I').tolist()`).

use std::ptr;
use std::sync::LazyLock;

use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::types::{bytearray_, bytes_};

pub const READONLY_WRITE_ERROR: &str = "cannot modify read-only memory";

/// CPython's diagnostic for any operation on a released view.
pub const RELEASED_ERROR: &str = "operation forbidden on released memoryview object";

/// Boxed Python `memoryview` over a contiguous byte window.
#[repr(C)]
#[derive(Debug)]
pub struct PyMemoryView {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Borrowed exporter object, kept so repr/tracing seams can retain identity later.
    pub base: *mut PyObject,
    /// Raw byte pointer for the first visible element.
    pub data: *mut u8,
    /// Visible byte length.
    pub len: usize,
    /// Whether writes through this view are forbidden.
    pub readonly: bool,
    /// Element format code: `b'B'` for plain byte views, `b'I'` after
    /// `cast('I')`.  Only codes accepted by [`item_width`] are stored.
    pub format: u8,
    /// Whether `release()` (or `__exit__`) ran; every subsequent buffer
    /// operation raises `ValueError` with [`RELEASED_ERROR`].
    pub released: bool,
}

impl PyMemoryView {
    /// Returns the visible bytes.
    ///
    /// # Safety
    ///
    /// The exporter must outlive this view and the stored pointer must cover `len` bytes.
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
    /// The exporter must be mutable, unique for the duration of the borrow, and cover `len` bytes.
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
pub fn boxed_memoryview_from_raw(base: *mut PyObject, data: *mut u8, len: usize, readonly: bool, format: u8) -> *mut PyMemoryView {
    Box::into_raw(Box::new(PyMemoryView {
        ob_base: PyObjectHeader::new(memoryview_type()),
        base,
        data,
        len,
        readonly,
        format,
        released: false,
    }))
}

/// Constructs a memoryview over bytes, bytearray, or another memoryview.
pub unsafe fn boxed_memoryview_from_object(object: *mut PyObject) -> Result<*mut PyMemoryView, String> {
    if object.is_null() {
        return Err("memoryview() argument is NULL".to_owned());
    }
    let ty = unsafe { (*object).ob_type };
    if bytes_::is_bytes_type(ty) {
        let bytes = unsafe { &*object.cast::<bytes_::PyBytes>() };
        let slice = unsafe { bytes.as_slice() };
        return Ok(boxed_memoryview_from_raw(object, slice.as_ptr().cast_mut(), slice.len(), true, b'B'));
    }
    if bytearray_::is_bytearray_type(ty) {
        let bytearray = unsafe { &mut *object.cast::<bytearray_::PyByteArray>() };
        return Ok(boxed_memoryview_from_raw(object, bytearray.bytes.as_mut_ptr(), bytearray.bytes.len(), false, b'B'));
    }
    if is_memoryview_type(ty) {
        let view = unsafe { &*object.cast::<PyMemoryView>() };
        if view.released {
            return Err(RELEASED_ERROR.to_owned());
        }
        return Ok(boxed_memoryview_from_raw(view.base, view.data, view.len, view.readonly, view.format));
    }
    Err("memoryview() argument must be a bytes-like object".to_owned())
}

/// Copies the visible byte window into an owned bytes vector.
#[must_use]
pub unsafe fn tobytes(view: *mut PyMemoryView) -> Vec<u8> {
    unsafe { (*view).as_slice().to_vec() }
}

/// Byte width of the supported `memoryview.cast` format codes.
///
/// Deliberately covers only what the vendored stdlib exercises: `'B'`
/// (unsigned byte) and `'I'` (native 32-bit unsigned, re._compiler's
/// `_bytes_to_codes`).  Unknown codes surface an honest error at the ABI
/// boundary instead of a silently wrong width.
#[must_use]
pub fn item_width(format: u8) -> Option<usize> {
    match format {
        b'B' => Some(1),
        b'I' => Some(4),
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

/// Python `memoryview.tolist()`: one int per element in the view's format.
///
/// # Safety
///
/// The exporter must outlive the view and the stored pointer must cover `len` bytes.
pub unsafe fn tolist(view: &PyMemoryView) -> Result<Vec<i64>, String> {
    let bytes = unsafe { view.as_slice() };
    match view.itemsize() {
        1 => Ok(bytes.iter().map(|byte| i64::from(*byte)).collect()),
        4 => Ok(bytes
            .chunks_exact(4)
            .map(|chunk| i64::from(u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])))
            .collect()),
        _ => Err("memoryview.tolist is not supported for this format".to_owned()),
    }
}
