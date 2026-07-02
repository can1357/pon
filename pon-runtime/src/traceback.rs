//! Minimal traceback recording for NULL-sentinel exception paths.
//!
//! Phase B does not yet expose a boxed Python traceback type.  This module keeps
//! the runtime-visible frame observations in a side table so raising helpers can
//! record the active frame without changing the GC object model or the public
//! `PyBaseException` layout.  When a boxed traceback object lands, the records
//! here are the single cutover point.

use core::{mem, ptr};
use std::sync::{LazyLock, Mutex, MutexGuard};

use crate::abi::PyFrame;
use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::thread_state::{pon_err_set, thread_state_lock};
use crate::types::frame::ensure_frame_type;


#[repr(C)]
struct PyTraceback {
    ob_base: PyObjectHeader,
    frame: *mut PyFrame,
}

fn traceback_type() -> *mut PyType {
    static TRACEBACK_TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(ptr::null(), "traceback", mem::size_of::<PyTraceback>());
        ty.tp_getattro = Some(traceback_getattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    *TRACEBACK_TYPE as *mut PyType
}

fn dummy_frame() -> *mut PyFrame {
    let frame_type = ensure_frame_type(ptr::null_mut());
    Box::into_raw(Box::new(PyFrame::new(frame_type, 0, ptr::null_mut())))
}

#[must_use]
pub fn new_traceback(frame: *mut PyFrame) -> *mut PyObject {
    let frame = if frame.is_null() { dummy_frame() } else { frame };
    Box::into_raw(Box::new(PyTraceback {
        ob_base: PyObjectHeader::new(traceback_type()),
        frame,
    }))
    .cast::<PyObject>()
}

unsafe extern "C" fn traceback_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        pon_err_set("traceback attribute name must be str");
        return ptr::null_mut();
    };
    match name {
        "tb_frame" => unsafe { (*object.cast::<PyTraceback>()).frame.cast::<PyObject>() },
        "tb_next" => unsafe { crate::abi::pon_none() },
        "tb_lineno" => unsafe { crate::abi::pon_const_int(0) },
        _ => unsafe { crate::abi::pon_raise_attribute_error(object, crate::intern::intern(name)) },
    }
}
/// One frame observed while installing an exception in `PonThreadState`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TracebackRecord {
    /// Exception object being installed, or NULL for diagnostic-only errors.
    pub exception: *mut PyObject,
    /// Active Python frame at the point the error was recorded.
    pub frame: *mut PyFrame,
}

unsafe impl Send for TracebackRecord {}

static TRACEBACK_RECORDS: LazyLock<Mutex<Vec<TracebackRecord>>> = LazyLock::new(|| Mutex::new(Vec::new()));

fn records_lock() -> MutexGuard<'static, Vec<TracebackRecord>> {
    TRACEBACK_RECORDS.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Records the current frame for a newly installed NULL-sentinel exception path.
///
/// The helper intentionally does nothing when no Python frame is active: Phase-A
/// top-level helpers run outside a managed frame, and recording a synthetic NULL
/// frame would be indistinguishable from missing traceback information.
pub fn record_current_frame(exception: *mut PyObject) {
    let frame = thread_state_lock().current_frame().unwrap_or(ptr::null_mut());
    if frame.is_null() {
        return;
    }
    records_lock().push(TracebackRecord { exception, frame });
}

/// Returns a snapshot of recorded traceback frames.
#[cfg(test)]
#[must_use]
pub fn records() -> Vec<TracebackRecord> {
    records_lock().clone()
}

/// Clears the traceback side table.
#[cfg(test)]
pub fn clear_records() {
    records_lock().clear();
}
