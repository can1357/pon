//! Process-global Phase-A thread state.
//!
//! Phase A has a single runtime thread.  The structure is shaped so it can move
//! to thread-local storage in the free-threaded runtime without changing the
//! helper ABI: fallible helpers return `NULL` and record the current exception
//! state here.

use std::ptr;
use std::sync::{LazyLock, Mutex, MutexGuard};

use crate::object::PyObject;

/// Interpreter state observed by runtime helpers.
#[derive(Debug)]
pub struct PonThreadState {
    /// Current exception object.  Non-null means an error is pending.
    pub current_exc: *mut PyObject,
    /// Python frame stack roots for later traceback and GC integration.
    pub frame_stack: Vec<*mut PyObject>,
    /// Active exception-handler chain; opaque until full frame objects exist.
    pub handler_chain: *mut u8,
    /// Conservative stack-base capture for stop-the-world collection.
    pub stack_base: *mut u8,
    diagnostic_message: Option<String>,
}

unsafe impl Send for PonThreadState {}

impl Default for PonThreadState {
    fn default() -> Self {
        Self {
            current_exc: ptr::null_mut(),
            frame_stack: Vec::new(),
            handler_chain: ptr::null_mut(),
            stack_base: ptr::null_mut(),
            diagnostic_message: None,
        }
    }
}

static THREAD_STATE: LazyLock<Mutex<PonThreadState>> = LazyLock::new(|| Mutex::new(PonThreadState::default()));

/// Returns the process-global Phase-A thread state.
#[must_use]
pub fn thread_state() -> &'static Mutex<PonThreadState> {
    &THREAD_STATE
}

/// Locks the Phase-A thread state, recovering poisoned state instead of
/// unwinding through the C ABI.
#[must_use]
pub fn thread_state_lock() -> MutexGuard<'static, PonThreadState> {
    match THREAD_STATE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Records a Phase-A diagnostic error.
///
/// Until exception classes are introduced, the diagnostic string is the stable
/// payload and `current_exc` is a non-null sentinel.  Runtime helpers that have a
/// concrete boxed exception may use [`pon_err_set_object`] instead.
pub fn pon_err_set(message: impl Into<String>) {
    let mut state = thread_state_lock();
    state.current_exc = core::ptr::NonNull::<PyObject>::dangling().as_ptr();
    state.diagnostic_message = Some(message.into());
}

/// Records an error with a concrete boxed exception object.
pub fn pon_err_set_object(exception: *mut PyObject, message: impl Into<String>) {
    let mut state = thread_state_lock();
    state.current_exc = if exception.is_null() {
        core::ptr::NonNull::<PyObject>::dangling().as_ptr()
    } else {
        exception
    };
    state.diagnostic_message = Some(message.into());
}

/// Clears the current exception state.
pub fn pon_err_clear() {
    let mut state = thread_state_lock();
    state.current_exc = ptr::null_mut();
    state.diagnostic_message = None;
}

/// Returns true when an exception is pending.
#[must_use]
pub fn pon_err_occurred() -> bool {
    !thread_state_lock().current_exc.is_null()
}

/// Returns the latest Phase-A diagnostic message, if any.
#[must_use]
pub fn pon_err_message() -> Option<String> {
    thread_state_lock().diagnostic_message.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_message_sets_exception_sentinel() {
        pon_err_clear();
        pon_err_set("boom");
        assert!(pon_err_occurred());
        assert_eq!(pon_err_message().as_deref(), Some("boom"));
        pon_err_clear();
    }
}
