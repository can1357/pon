//! Heap Python frame implementation.
//!
//! Generator and coroutine bodies are stackless in Phase B: compiled resume
//! functions receive a heap frame, dispatch on its `state`, and keep every local
//! suspend-crossing temporary in `locals`.

use core::{mem, ptr};
use std::sync::{LazyLock, Mutex};

use pon_gc::{GcTypeInfo, TypeId};

use crate::abi::PyFrame;
use crate::object::{PyMappingMethods, PyObject, PyObjectHeader, PyType};
use crate::thread_state::pon_err_set;

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
    ty.tp_getattro = Some(frame_getattro);
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

/// GC type id reserved for PEP 667 frame-locals proxy objects; sits in the
/// WS-GEN frame family next to `crate::traceback::TYPE_ID_TRACEBACK`.
pub const TYPE_ID_FRAME_LOCALS_PROXY: TypeId = TypeId(35);

static FRAME_LOCALS_PROXY_TYPE: LazyLock<Mutex<Option<usize>>> = LazyLock::new(|| Mutex::new(None));

static FRAME_LOCALS_PROXY_MAPPING_METHODS: LazyLock<usize> = LazyLock::new(|| {
    let methods = PyMappingMethods {
        mp_subscript: Some(frame_locals_proxy_subscript),
        ..PyMappingMethods::EMPTY
    };
    Box::into_raw(Box::new(methods)) as usize
});

/// PEP 667 `FrameLocalsProxy` payload: a distinctly-typed read view over a
/// backing namespace dict.
///
/// CPython 3.14 hands out a fresh `FrameLocalsProxy` on every `frame.f_locals`
/// read; only the TYPE identity must stay stable (`_collections_abc` snapshots
/// `type(sys._getframe().f_locals)` at import and later feeds it to
/// `Mapping.register`).
#[repr(C)]
pub struct PyFrameLocalsProxy {
    /// Standard boxed-object header at offset zero.
    header: PyObjectHeader,
    /// Backing namespace dict; non-NULL by construction.
    mapping: *mut PyObject,
}

/// Returns the process-lifetime `FrameLocalsProxy` type object, creating it if
/// needed.
pub fn ensure_frame_locals_proxy_type(type_type: *mut PyType) -> *mut PyType {
    let mut slot = FRAME_LOCALS_PROXY_TYPE.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(ptr) = *slot {
        return ptr as *mut PyType;
    }

    let mut ty = PyType::new(type_type.cast_const(), "FrameLocalsProxy", mem::size_of::<PyFrameLocalsProxy>());
    ty.gc_type_id = TYPE_ID_FRAME_LOCALS_PROXY.0 as usize;
    ty.tp_as_mapping = *FRAME_LOCALS_PROXY_MAPPING_METHODS as *mut PyMappingMethods;
    let ptr = Box::into_raw(Box::new(ty));
    *slot = Some(ptr as usize);
    ptr
}

/// Serves `proxy[key]` reads by delegating to the backing namespace dict.
unsafe extern "C" fn frame_locals_proxy_subscript(object: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    // SAFETY: The runtime dispatches this slot only for PyFrameLocalsProxy instances.
    let proxy = unsafe { &*object.cast::<PyFrameLocalsProxy>() };
    match unsafe { crate::types::dict::dict_get(proxy.mapping, key) } {
        Ok(Some(value)) => value,
        Ok(None) => unsafe { crate::abi::pon_raise_key_error(key) },
        Err(message) => {
            pon_err_set(message);
            ptr::null_mut()
        }
    }
}

/// Traces a `PyFrameLocalsProxy` allocation for the runtime GC.
///
/// # Safety
/// `object` must point at a live `PyFrameLocalsProxy` allocation.
pub unsafe extern "C" fn trace_frame_locals_proxy(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    // SAFETY: The GC passes the allocation start for a registered proxy.
    let proxy = unsafe { &*object.cast::<PyFrameLocalsProxy>() };
    if !proxy.mapping.is_null() {
        visitor(proxy.mapping.cast::<u8>());
    }
}

/// Allocates a fresh `FrameLocalsProxy` over the active module-globals dict.
///
/// pon does not materialize per-call Python locals namespaces, so the proxy
/// wraps the active module's globals dict — the namespace a module-level
/// `sys._getframe().f_locals` read observes in CPython too.  Function-level
/// callers therefore see module globals rather than true locals; the PEP 667
/// probes this serves only inspect the proxy's TYPE.
fn new_frame_locals_proxy() -> *mut PyObject {
    let mapping = unsafe { crate::dynexec::builtin_globals(ptr::null_mut(), 0) };
    if mapping.is_null() {
        // builtin_globals already recorded the thread-state error.
        return ptr::null_mut();
    }
    let proxy_type = ensure_frame_locals_proxy_type(crate::abi::runtime_type_type());
    let info = GcTypeInfo {
        size: mem::size_of::<PyFrameLocalsProxy>(),
        trace: trace_frame_locals_proxy,
        finalize: None,
    };
    match crate::abi::alloc_gc_object(TYPE_ID_FRAME_LOCALS_PROXY, info) {
        Ok(block) => {
            let proxy = block.cast::<PyFrameLocalsProxy>();
            // SAFETY: `block` is a freshly allocated zeroed block of the right size.
            unsafe {
                ptr::write(
                    proxy,
                    PyFrameLocalsProxy {
                        header: PyObjectHeader::new(proxy_type.cast_const()),
                        mapping,
                    },
                );
            }
            proxy.cast::<PyObject>()
        }
        Err(message) => {
            pon_err_set(message);
            ptr::null_mut()
        }
    }
}

/// Serves `f_locals` on frame objects (both `PyFrame` and resumable `GenFrame`
/// allocations share the runtime `frame` type, so this slot must never read
/// past the shared object header).
///
/// Wider frame introspection (`f_back`, `f_code`, ...) is intentionally not
/// served yet and raises `AttributeError`.
unsafe extern "C" fn frame_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        pon_err_set("frame attribute name must be str");
        return ptr::null_mut();
    };
    match name {
        "f_locals" => new_frame_locals_proxy(),
        _ => unsafe { crate::abi::pon_raise_attribute_error(object, crate::intern::intern(name)) },
    }
}

/// Synthesizes a fresh, empty heap frame of the runtime `frame` type — the
/// same object family traceback entries carry — for `sys._getframe`.
pub fn synthesize_frame_object() -> *mut PyObject {
    let frame_type = ensure_frame_type(crate::abi::runtime_type_type());
    let info = GcTypeInfo {
        size: mem::size_of::<PyFrame>(),
        trace: trace_frame,
        finalize: Some(finalize_frame),
    };
    match crate::abi::alloc_gc_object(TYPE_ID_FRAME, info) {
        Ok(block) => {
            let frame = block.cast::<PyFrame>();
            // SAFETY: `block` is a freshly allocated zeroed block of the right size.
            unsafe { ptr::write(frame, PyFrame::new(frame_type.cast_const(), 0, ptr::null_mut())) };
            frame.cast::<PyObject>()
        }
        Err(message) => {
            pon_err_set(message);
            ptr::null_mut()
        }
    }
}
