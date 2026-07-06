//! Heap Python frame implementation.
//!
//! Generator and coroutine bodies are stackless in Phase B: compiled resume
//! functions receive a heap frame, dispatch on its `state`, and keep every local
//! suspend-crossing temporary in `locals`.

use core::{mem, ptr};
use std::collections::HashMap;
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

/// One captured call-chain frame: interned name ids and a line number only,
/// never GC pointers, so the side table roots no objects.
#[derive(Clone, Copy)]
pub struct FrameLink {
    /// Interned defining-module name backing `f_globals`; `None` reads fall
    /// back to the active module's namespace.
    pub module: Option<u32>,
    /// Interned function name backing `f_code.co_name`; `None` is a module
    /// toplevel frame (`"<module>"`).
    pub name: Option<u32>,
    /// 1-based line the frame is executing, `0` when unknown.
    pub line: u32,
}

/// Frame allocation address → call chain captured at `sys._getframe` time
/// (the live stack at a later attribute read no longer describes the
/// captured depth): `chain[0]` describes the frame itself, `chain[1..]` its
/// callers outward, and the last entry is always the module-toplevel frame.
/// `f_back` peels one link per hop and a one-link chain answers `None`;
/// `finalize_frame` drops the record with the allocation.  Links are plain
/// interned ids and lines, so the table roots no GC objects (mirroring
/// `types::function::FUNCTION_MODULES`).
static FRAME_RECORDS: LazyLock<Mutex<HashMap<usize, Box<[FrameLink]>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// `chain[0]` of the frame's captured record: the frame's own module, name,
/// and line.  `None` for traceback and generator frames, which never record
/// a chain.
fn frame_link(frame: *mut PyObject) -> Option<FrameLink> {
    FRAME_RECORDS
        .lock()
        .ok()
        .and_then(|table| table.get(&(frame as usize)).and_then(|chain| chain.first().copied()))
}
/// Attaches a captured call-chain record to an existing frame allocation
/// (`chain[0]` = the frame itself). Traceback frames use this at raise time
/// so `f_code.co_name`/`co_filename` resolve to real names; `finalize_frame`
/// drops the record with the allocation.
pub(crate) fn record_frame_chain(frame: *mut PyObject, chain: Box<[FrameLink]>) {
    if frame.is_null() || chain.is_empty() {
        return;
    }
    if let Ok(mut table) = FRAME_RECORDS.lock() {
        table.insert(frame as usize, chain);
    }
}

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
            line: 0,
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
    if let Ok(mut table) = FRAME_RECORDS.lock() {
        table.remove(&(object as usize));
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
/// needed.  The type carries a small mapping-method surface in its tp_dict
/// (`get`/`keys`/`values`/`items`/`__contains__`/`__len__`): PEP 667 proxies
/// are full mappings, and Cython's compiler probes `f_locals.get(...)`.
pub fn ensure_frame_locals_proxy_type(type_type: *mut PyType) -> *mut PyType {
    let mut slot = FRAME_LOCALS_PROXY_TYPE.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(ptr) = *slot {
        return ptr as *mut PyType;
    }

    let mut ty = PyType::new(type_type.cast_const(), "FrameLocalsProxy", mem::size_of::<PyFrameLocalsProxy>());
    ty.gc_type_id = TYPE_ID_FRAME_LOCALS_PROXY.0 as usize;
    ty.tp_as_mapping = *FRAME_LOCALS_PROXY_MAPPING_METHODS as *mut PyMappingMethods;
    let namespace = crate::types::type_::new_namespace();
    if !namespace.is_null() {
        type Entry = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;
        let methods: [(&str, Entry); 6] = [
            ("get", frame_locals_proxy_get_method),
            ("keys", frame_locals_proxy_keys_method),
            ("values", frame_locals_proxy_values_method),
            ("items", frame_locals_proxy_items_method),
            ("__contains__", frame_locals_proxy_contains_method),
            ("__len__", frame_locals_proxy_len_method),
        ];
        for (name, entry) in methods {
            let name_id = crate::intern::intern(name);
            // SAFETY: Live builtin entry point with the runtime calling convention.
            let function = unsafe {
                crate::abi::pon_make_function(entry as *const u8, crate::builtins::variadic_arity(), name_id)
            };
            if function.is_null() {
                continue;
            }
            crate::types::function::mark_native_function(function);
            crate::types::function::mark_native_method_descriptor(function);
            // SAFETY: Freshly allocated namespace dict.
            unsafe { (&mut *namespace).set(name_id, function) };
        }
        ty.tp_dict = namespace.cast::<PyObject>();
    }
    let ptr = Box::into_raw(Box::new(ty));
    crate::sync::register_namespaced_type(ptr);
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

/// Bound-method receiver plus argument window for the proxy method entries.
unsafe fn proxy_method_args<'a>(argv: *mut *mut PyObject, argc: usize) -> &'a [*mut PyObject] {
    if argv.is_null() || argc == 0 {
        &[]
    } else {
        // SAFETY: The caller passed `argc` live argument slots.
        unsafe { std::slice::from_raw_parts(argv, argc) }
    }
}

fn proxy_mapping(receiver: *mut PyObject) -> Option<*mut PyObject> {
    let raw = crate::tag::untag_arg(receiver);
    if raw.is_null() {
        return None;
    }
    // SAFETY: Method dispatch binds only PyFrameLocalsProxy receivers.
    Some(unsafe { (*raw.cast::<PyFrameLocalsProxy>()).mapping })
}

/// `proxy.get(key, default=None)`.
unsafe extern "C" fn frame_locals_proxy_get_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = unsafe { proxy_method_args(argv, argc) };
    if !(2..=3).contains(&args.len()) {
        pon_err_set(format!("get expected 1 or 2 arguments, got {}", args.len().saturating_sub(1)));
        return ptr::null_mut();
    }
    let Some(mapping) = proxy_mapping(args[0]) else {
        pon_err_set("FrameLocalsProxy receiver is NULL");
        return ptr::null_mut();
    };
    match unsafe { crate::types::dict::dict_get(mapping, args[1]) } {
        Ok(Some(value)) => value,
        Ok(None) => args.get(2).copied().unwrap_or_else(|| unsafe { crate::abi::pon_none() }),
        Err(message) => {
            pon_err_set(message);
            ptr::null_mut()
        }
    }
}

/// `key in proxy`.
unsafe extern "C" fn frame_locals_proxy_contains_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = unsafe { proxy_method_args(argv, argc) };
    if args.len() != 2 {
        pon_err_set(format!("__contains__ expected 1 argument, got {}", args.len().saturating_sub(1)));
        return ptr::null_mut();
    }
    let Some(mapping) = proxy_mapping(args[0]) else {
        pon_err_set("FrameLocalsProxy receiver is NULL");
        return ptr::null_mut();
    };
    match unsafe { crate::types::dict::dict_get(mapping, args[1]) } {
        Ok(found) => unsafe { crate::abi::number::pon_const_bool(i32::from(found.is_some())) },
        Err(message) => {
            pon_err_set(message);
            ptr::null_mut()
        }
    }
}

/// `len(proxy)`.
unsafe extern "C" fn frame_locals_proxy_len_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = unsafe { proxy_method_args(argv, argc) };
    let Some(mapping) = args.first().copied().and_then(proxy_mapping) else {
        pon_err_set("FrameLocalsProxy receiver is NULL");
        return ptr::null_mut();
    };
    match unsafe { crate::types::dict::dict_entries_snapshot(mapping) } {
        Ok(entries) => unsafe { crate::abi::pon_const_int(entries.len() as i64) },
        Err(message) => {
            pon_err_set(message);
            ptr::null_mut()
        }
    }
}

/// Shared list-building core for `keys`/`values`/`items`.
unsafe fn proxy_snapshot_list(
    argv: *mut *mut PyObject,
    argc: usize,
    which: u8,
) -> *mut PyObject {
    let args = unsafe { proxy_method_args(argv, argc) };
    let Some(mapping) = args.first().copied().and_then(proxy_mapping) else {
        pon_err_set("FrameLocalsProxy receiver is NULL");
        return ptr::null_mut();
    };
    let entries = match unsafe { crate::types::dict::dict_entries_snapshot(mapping) } {
        Ok(entries) => entries,
        Err(message) => {
            pon_err_set(message);
            return ptr::null_mut();
        }
    };
    let mut items = Vec::with_capacity(entries.len());
    for entry in entries {
        let item = match which {
            0 => entry.key,
            1 => entry.value,
            _ => {
                let mut pair = [entry.key, entry.value];
                // SAFETY: Two live slots for the tuple builder.
                let tuple = unsafe { crate::abi::seq::pon_build_tuple(pair.as_mut_ptr(), pair.len()) };
                if tuple.is_null() {
                    return ptr::null_mut();
                }
                tuple
            }
        };
        items.push(item);
    }
    let ptr_slot = if items.is_empty() { ptr::null_mut() } else { items.as_mut_ptr() };
    // SAFETY: `items` is live for the duration of the call.
    unsafe { crate::abi::seq::pon_build_list(ptr_slot, items.len()) }
}

/// `proxy.keys()` (list snapshot; CPython hands out a view).
unsafe extern "C" fn frame_locals_proxy_keys_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { proxy_snapshot_list(argv, argc, 0) }
}

/// `proxy.values()` (list snapshot).
unsafe extern "C" fn frame_locals_proxy_values_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { proxy_snapshot_list(argv, argc, 1) }
}

/// `proxy.items()` (list-of-pairs snapshot).
unsafe extern "C" fn frame_locals_proxy_items_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { proxy_snapshot_list(argv, argc, 2) }
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

/// Serves `f_globals` on frame objects: the live namespace dict of the
/// module recorded for the frame at `sys._getframe` time — the same
/// registered dict `globals()` returns inside that module, so mutations
/// through it surface as module globals.  Frames without a record
/// (traceback and generator frames) approximate with the active module's
/// namespace, mirroring the `f_locals` proxy.
fn new_frame_globals_dict(frame: *mut PyObject) -> *mut PyObject {
    let Some(module) = frame_link(frame).and_then(|link| link.module) else {
        // builtin_globals records the thread-state error on failure.
        return unsafe { crate::dynexec::builtin_globals(ptr::null_mut(), 0) };
    };
    match crate::dynexec::module_namespace_dict(module) {
        Ok(dict) => dict,
        Err(message) => {
            pon_err_set(message);
            ptr::null_mut()
        }
    }
}

/// Serves `f_back`: the caller frame one link up the chain captured at
/// `sys._getframe` time, `None` at the module-toplevel frame and on frames
/// without a record (traceback and generator frames, whose callers are
/// unknown).  Every read synthesizes a fresh frame object; the chain
/// consumers (`logging.Logger.findCaller`, `_py_warnings._next_external_frame`)
/// only walk toward `None` and read attributes — none compare frame identity.
fn new_frame_back(frame: *mut PyObject) -> *mut PyObject {
    let back: Option<Box<[FrameLink]>> = FRAME_RECORDS
        .lock()
        .ok()
        .and_then(|table| table.get(&(frame as usize)).and_then(|chain| (chain.len() > 1).then(|| chain[1..].into())));
    match back {
        Some(chain) => synthesize_frame_object(chain),
        // SAFETY: Immortal singleton accessor; NULL propagates with the error set.
        None => unsafe { crate::abi::pon_none() },
    }
}

/// Serves frame introspection (`f_locals`, `f_globals`, `f_code`, `f_back`,
/// `f_lineno`) on frame objects (both `PyFrame` and resumable `GenFrame`
/// allocations share the runtime `frame` type, so this slot must never read
/// past the shared object header — per-frame data lives in `FRAME_RECORDS`).
///
/// Wider frame introspection (`f_trace`, ...) is intentionally not served
/// yet and raises `AttributeError`.
unsafe extern "C" fn frame_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        pon_err_set("frame attribute name must be str");
        return ptr::null_mut();
    };
    match name {
        "f_locals" => new_frame_locals_proxy(),
        "f_globals" => new_frame_globals_dict(object),
        "f_code" => new_code_object(frame_link(object)),
        "f_back" => new_frame_back(object),
        // `0` for frames without a captured chain (a traceback frame's line
        // lives on the traceback entry itself as `tb_lineno`).
        // SAFETY: Integer boxing helper.
        "f_lineno" => unsafe { crate::abi::pon_const_int(i64::from(frame_link(object).map_or(0, |link| link.line))) },
        _ => unsafe { crate::abi::pon_raise_attribute_error(object, crate::intern::intern(name)) },
    }
}

/// Synthetic code object served by `frame.f_code`.
///
/// pon frames carry no code metadata beyond the captured function name and
/// defining module, so the payload is two optional interned ids: `co_name`
/// resolves the name (`None` → `"<module>"`), and `co_filename` reads the
/// `"<pon:module.path>"` pseudo-file (`"<pon>"` without a module; angle
/// brackets make `linecache` treat it as source-less deterministically).
/// Together with `tb_lasti == -1` this is the exact surface
/// `traceback.StackSummary` and `logging.Logger.findCaller` read;
/// `co_positions()` stays unreached. Unknown attributes raise
/// `AttributeError` so the next introspection frontier stays loud.
#[repr(C)]
struct PyFrameCode {
    header: PyObjectHeader,
    /// Interned function name backing `co_name`; `None` → `"<module>"`.
    name: Option<u32>,
    /// Interned defining-module name backing `co_filename`; `None` → `"<pon>"`.
    module: Option<u32>,
}

fn frame_code_type() -> *mut PyType {
    static CODE_TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(
            crate::abi::runtime_type_type().cast_const(),
            "code",
            mem::size_of::<PyFrameCode>(),
        );
        ty.tp_getattro = Some(code_getattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    *CODE_TYPE as *mut PyType
}

/// Code object for one frame: the process-shared bare instance, or a fresh
/// boxed instance carrying the frame's function name and module (leaked
/// exactly like `types::function::alloc_code_object`'s per-read shells — the
/// payload is three words and reads are bounded by introspection walks).
fn new_code_object(link: Option<FrameLink>) -> *mut PyObject {
    static SHARED: LazyLock<usize> = LazyLock::new(|| {
        Box::into_raw(Box::new(PyFrameCode {
            header: PyObjectHeader::new(frame_code_type()),
            name: None,
            module: None,
        })) as usize
    });
    match link {
        None => *SHARED as *mut PyObject,
        Some(link) => Box::into_raw(Box::new(PyFrameCode {
            header: PyObjectHeader::new(frame_code_type()),
            name: link.name,
            module: link.module,
        }))
        .cast::<PyObject>(),
    }
}

unsafe extern "C" fn code_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { crate::types::type_::unicode_text(crate::tag::untag_arg(name)) }) else {
        pon_err_set("code attribute name must be str");
        return ptr::null_mut();
    };
    match name {
        // SAFETY: Runtime allocation helper; NULL propagates with the error set.
        "co_filename" => {
            // SAFETY: Only `frame_code_type` instances carry this getattro
            // slot, so `object` is a live `PyFrameCode` allocation.
            let text = unsafe { (*object.cast::<PyFrameCode>()).module }
                .and_then(crate::intern::resolve)
                .map_or_else(|| "<pon>".to_owned(), |module| format!("<pon:{module}>"));
            // SAFETY: Runtime allocation helper; NULL propagates with the error set.
            unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) }
        },
        "co_name" | "co_qualname" => {
            // SAFETY: Only `frame_code_type` instances carry this getattro
            // slot, so `object` is a live `PyFrameCode` allocation.
            let text = unsafe { (*object.cast::<PyFrameCode>()).name }
                .and_then(crate::intern::resolve)
                .unwrap_or_else(|| "<module>".to_owned());
            // SAFETY: Runtime allocation helper; NULL propagates with the error set.
            unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) }
        }
        // 1-based first line of the (synthetic) code block: only anchor
        // arithmetic in `traceback.StackSummary` consumes it.
        // SAFETY: Integer boxing helper.
        "co_firstlineno" => unsafe { crate::abi::pon_const_int(1) },
        _ => unsafe { crate::abi::pon_raise_attribute_error(object, crate::intern::intern(name)) },
    }
}

/// Synthesizes a fresh, empty heap frame of the runtime `frame` type — the
/// same object family traceback entries carry — for `sys._getframe` and
/// `f_back` hops.
///
/// `chain` is the captured call chain the frame serves (`chain[0]` = this
/// frame; see `abi::frame_chain_for_depth`), resolved by the caller from the
/// live call stack.  An empty chain leaves no record: `f_globals` falls back
/// to the active module's namespace at read time, `f_back` to `None`, and
/// `f_lineno` to `0`.
pub fn synthesize_frame_object(chain: Box<[FrameLink]>) -> *mut PyObject {
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
            if !chain.is_empty() {
                if let Ok(mut table) = FRAME_RECORDS.lock() {
                    table.insert(frame as usize, chain);
                }
            }
            frame.cast::<PyObject>()
        }
        Err(message) => {
            pon_err_set(message);
            ptr::null_mut()
        }
    }
}
