//! Heap Python frame implementation.
//!
//! Generator and coroutine bodies are stackless in Phase B: compiled resume
//! functions receive a heap frame, dispatch on its `state`, and keep every local
//! suspend-crossing temporary in `locals`.

use core::{mem, ptr};
use std::sync::{LazyLock, Mutex};

use pon_gc::TypeId;

use crate::abi::PyFrame;
use crate::object::{PyObject, PyObjectHeader, PyType};

/// Initial resume state for a frame that has never run.
pub const FRAME_STATE_INITIAL: u32 = 0;
/// Sentinel resume state for an exhausted generator/coroutine frame.
pub const FRAME_STATE_EXHAUSTED: u32 = u32::MAX;
/// GC type id reserved for heap frame objects in the WS-GEN family.
pub const TYPE_ID_FRAME: TypeId = TypeId(32);

static FRAME_TYPE: LazyLock<Mutex<Option<usize>>> = LazyLock::new(|| Mutex::new(None));

/// Returns the process-lifetime frame type object, creating it if needed.
pub fn ensure_frame_type(type_type: *mut PyType) -> *mut PyType {
    let mut slot = FRAME_TYPE.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(ptr) = *slot {
        return ptr as *mut PyType;
    }

    let mut ty = PyType::new(type_type.cast_const(), "frame", mem::size_of::<PyFrame>());
    ty.gc_type_id = TYPE_ID_FRAME.0 as usize;
    let ptr = Box::into_raw(Box::new(ty));
    *slot = Some(ptr as usize);
    ptr
}

impl PyFrame {
    /// Builds a heap-frame payload with zero-initialized local storage.
    #[must_use]
    pub fn new(frame_type: *const PyType, n_locals: u32, locals: *mut *mut PyObject) -> Self {
        Self {
            header: PyObjectHeader::new(frame_type),
            state: FRAME_STATE_INITIAL,
            n_locals,
            locals,
            parent: ptr::null_mut(),
            exc_state: ptr::null_mut(),
        }
    }

    /// Returns true once this frame can no longer be resumed.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.state == FRAME_STATE_EXHAUSTED
    }

    /// Marks the frame as permanently exhausted.
    pub fn mark_exhausted(&mut self) {
        self.state = FRAME_STATE_EXHAUSTED;
        self.parent = ptr::null_mut();
    }
}

/// Creates zeroed local slots for a heap frame.
///
/// The current GC API registers fixed-size object layouts, so the local vector is
/// owned as a boxed slice and traced from the frame.  The frame finalizer releases
/// the slice when the frame is swept; no Python reference counts are involved.
pub fn alloc_frame_locals(n_locals: u32) -> Result<*mut *mut PyObject, String> {
    let len = usize::try_from(n_locals).map_err(|_| "frame local count does not fit usize".to_owned())?;
    if len == 0 {
        return Ok(ptr::null_mut());
    }
    let mut locals = vec![ptr::null_mut(); len].into_boxed_slice();
    let ptr = locals.as_mut_ptr();
    core::mem::forget(locals);
    Ok(ptr)
}

/// Traces a `PyFrame` allocation for the runtime GC.
///
/// # Safety
/// `object` must point at a live `PyFrame` allocation.
pub unsafe extern "C" fn trace_frame(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    // SAFETY: The GC passes the allocation start for a registered PyFrame.
    let frame = unsafe { &*object.cast::<PyFrame>() };
    if !frame.locals.is_null() {
        for index in 0..frame.n_locals as usize {
            // SAFETY: `locals` has `n_locals` slots by construction.
            let value = unsafe { *frame.locals.add(index) };
            if !value.is_null() {
                visitor(value.cast::<u8>());
            }
        }
    }
    if !frame.parent.is_null() {
        visitor(frame.parent.cast::<u8>());
    }
    if !frame.exc_state.is_null() {
        visitor(frame.exc_state.cast::<u8>());
    }
}

/// Releases boxed local storage owned by a `PyFrame` allocation.
///
/// # Safety
/// `object` must point at a live, unreachable `PyFrame` allocation.
pub unsafe extern "C" fn finalize_frame(object: *mut u8) {
    if object.is_null() {
        return;
    }
    // SAFETY: The GC passes the allocation start for a registered PyFrame.
    let frame = unsafe { &mut *object.cast::<PyFrame>() };
    if !frame.locals.is_null() && frame.n_locals != 0 {
        let len = frame.n_locals as usize;
        let slice = ptr::slice_from_raw_parts_mut(frame.locals, len);
        frame.locals = ptr::null_mut();
        frame.n_locals = 0;
        // SAFETY: `alloc_frame_locals` created this boxed slice.
        unsafe {
            drop(Box::<[*mut PyObject]>::from_raw(slice));
        }
    }
}
