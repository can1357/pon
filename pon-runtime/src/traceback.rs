//! Minimal traceback recording for NULL-sentinel exception paths.
//!
//! Phase B does not yet expose a boxed Python traceback type.  This module keeps
//! the runtime-visible frame observations in a side table so raising helpers can
//! record the active frame without changing the GC object model or the public
//! `PyBaseException` layout.  When a boxed traceback object lands, the records
//! here are the single cutover point.

use core::ptr;
use std::sync::{LazyLock, Mutex, MutexGuard};

use crate::abi::PyFrame;
use crate::object::PyObject;
use crate::thread_state::thread_state_lock;

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
