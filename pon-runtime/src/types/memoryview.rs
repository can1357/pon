//! Minimal memoryview implementation over the K2 bytes/bytearray carriers.
//!
//! The view stores a borrowed base object pointer plus a raw contiguous byte
//! window.  Bytes-backed views are readonly; bytearray-backed views are writable
//! until bytearray reallocation.  The Wave-2 surface only promises basic
//! construction, len/index/slice, `tobytes()`, and readonly enforcement.

use std::ptr;
use std::sync::LazyLock;

use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::types::{bytearray_, bytes_};

pub const READONLY_WRITE_ERROR: &str = "cannot modify read-only memory";

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
pub fn boxed_memoryview_from_raw(base: *mut PyObject, data: *mut u8, len: usize, readonly: bool) -> *mut PyMemoryView {
    Box::into_raw(Box::new(PyMemoryView {
        ob_base: PyObjectHeader::new(memoryview_type()),
        base,
        data,
        len,
        readonly,
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
        return Ok(boxed_memoryview_from_raw(object, slice.as_ptr().cast_mut(), slice.len(), true));
    }
    if bytearray_::is_bytearray_type(ty) {
        let bytearray = unsafe { &mut *object.cast::<bytearray_::PyByteArray>() };
        return Ok(boxed_memoryview_from_raw(object, bytearray.bytes.as_mut_ptr(), bytearray.bytes.len(), false));
    }
    if is_memoryview_type(ty) {
        let view = unsafe { &*object.cast::<PyMemoryView>() };
        return Ok(boxed_memoryview_from_raw(view.base, view.data, view.len, view.readonly));
    }
    Err("memoryview() argument must be a bytes-like object".to_owned())
}

/// Copies the visible byte window into an owned bytes vector.
#[must_use]
pub unsafe fn tobytes(view: *mut PyMemoryView) -> Vec<u8> {
    unsafe { (*view).as_slice().to_vec() }
}
