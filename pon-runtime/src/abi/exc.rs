//! Exception helper family namespace.
//!
//! Helpers here follow the runtime-wide NULL-sentinel discipline: fallible object
//! helpers set `PonThreadState.current_exc` and return NULL, while status helpers
//! return `-1` on helper misuse.  No native unwinding crosses the C ABI.

use core::ffi::c_int;
use core::mem::size_of;
use core::ptr;
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicPtr, Ordering};

use pon_gc::GcTypeInfo;

use crate::intern;
use crate::object::{PyObject, PyType, as_object_ptr, is_exact_type};
use crate::thread_state::{ExcStarFrame, pon_err_clear, pon_err_occurred, pon_err_set_object, pon_err_set_object_lazy_display, thread_state_lock};
use crate::traceback::{PyTraceback, TYPE_ID_TRACEBACK, ensure_traceback_type, trace_traceback};
use crate::types::exc::{
    EXC_GROUP_METHOD_DERIVE, EXC_GROUP_METHOD_SPLIT, EXC_GROUP_METHOD_SUBGROUP, ExceptionKind, ExceptionTypeSet,
    PyBaseException, PyExceptionGroup, as_exception_group, is_exception_group_instance, is_exception_instance,
    is_exception_subclass,
};
use crate::types::frame::{TYPE_ID_FRAME, ensure_frame_type, finalize_frame, trace_frame};
use crate::types::function::KeywordArgs;
use crate::types::tuple::PyTuple;

use super::{HandlerInfo, PyFrame, Runtime, TYPE_ID_EXCEPTION, TYPE_ID_EXCEPTION_GROUP};

/// Exception-handler kind selector; concrete values are assigned by lowering.
pub type HandlerKind = u8;

fn catch_i32_helper(f: impl FnOnce() -> c_int) -> c_int {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(_) => super::return_minus_one_with_error("runtime helper panicked"),
    }
}

fn ensure_runtime_for_exc() -> Result<(), String> {
    super::ensure_runtime_initialized()
}

fn bytes_from_raw<'a>(ptr: *const u8, len: usize) -> Result<&'a [u8], String> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() {
        return Err("exception message pointer is null".to_owned());
    }
    // SAFETY: The helper ABI requires callers to pass `len` readable bytes.
    Ok(unsafe { core::slice::from_raw_parts(ptr, len) })
}

fn diagnostic_sentinel() -> *mut PyObject {
    core::ptr::NonNull::<PyObject>::dangling().as_ptr()
}

static BASE_EXCEPTION_TYPE: AtomicPtr<PyType> = AtomicPtr::new(ptr::null_mut());
static BASE_EXCEPTION_GROUP_TYPE: AtomicPtr<PyType> = AtomicPtr::new(ptr::null_mut());

pub(crate) fn is_diagnostic_sentinel(value: *mut PyObject) -> bool {
    value == diagnostic_sentinel()
}

/// Returns the pending exception when it is a live boxed exception object.
///
/// Message-only diagnostics raised through `pon_err_set` install a dangling
/// sentinel in `current_exc`; that sentinel is never a dereferenceable object
/// and is reported here as `None`. All readers that dereference the pending
/// exception must route through this accessor.
pub(crate) fn pending_exception_object() -> Option<*mut PyObject> {
    let current = thread_state_lock().current_exc;
    if current.is_null() || is_diagnostic_sentinel(current) {
        None
    } else {
        Some(current)
    }
}

/// Returns true when the pending exception is an instance of the named
/// builtin exception type (raw MRO walk, matching except-clause semantics).
pub(crate) fn pending_exception_is(name: &str) -> bool {
    let Some(exception) = pending_exception_object() else {
        return false;
    };
    // SAFETY: `exception` is a live boxed object per `pending_exception_object`.
    unsafe { crate::types::exc::exception_type_named((*exception).ob_type, name) }
}

/// Publishes process-lifetime builtin exception roots for lock-free subtype checks.
pub(super) fn publish_exception_type_roots(types: ExceptionTypeSet) {
    BASE_EXCEPTION_TYPE.store(types.base_exception, Ordering::Release);
    BASE_EXCEPTION_GROUP_TYPE.store(types.base_exception_group, Ordering::Release);
}

/// Returns true when `ty` derives `BaseException` (raw-MRO walk against the
/// runtime's builtin hierarchy); false when the runtime is not initialized.
pub(crate) fn type_derives_base_exception(ty: *const PyType) -> bool {
    let base = BASE_EXCEPTION_TYPE.load(Ordering::Acquire);
    if base.is_null() {
        return false;
    }
    // SAFETY: `base` is a process-lifetime builtin type published after
    // runtime construction; callers pass NULL or a live type descriptor.
    unsafe { is_exception_subclass(ty, base.cast_const()) }
}

/// Returns true when `ty` derives `BaseExceptionGroup`.
pub(crate) fn type_derives_exception_group(ty: *const PyType) -> bool {
    let base = BASE_EXCEPTION_GROUP_TYPE.load(Ordering::Acquire);
    if base.is_null() {
        return false;
    }
    // SAFETY: `base` is a process-lifetime builtin type published after
    // runtime construction; callers pass NULL or a live type descriptor.
    unsafe { is_exception_subclass(ty, base.cast_const()) }
}

/// Allocates an exception-layout instance of `cls` with constructor argument
/// semantics, for instantiation paths outside the dedicated exception call
/// branch (`type.__new__` on exception-derived heap classes).
///
/// Must not be called while the runtime lock is already held.
pub(crate) fn alloc_exception_instance(cls: *mut PyType, args: &[*mut PyObject]) -> *mut PyObject {
    match super::with_runtime(|runtime| {
        let message = args.first().copied().unwrap_or(ptr::null_mut());
        match alloc_exception_object(runtime, cls, message, ptr::null_mut()) {
            Ok(exception) => {
                if args.len() >= 2 {
                    match super::seq::alloc_tuple_from_slice(runtime, args) {
                        Ok(tuple) => unsafe { (*exception.cast::<PyBaseException>()).args = tuple },
                        Err(message) => return super::return_null_with_error(message),
                    }
                }
                exception
            }
            Err(message) => super::return_null_with_error(message),
        }
    }) {
        Some(result) => result,
        None => super::return_null_with_error("runtime is not initialized"),
    }
}

/// Applies call-site keywords to a freshly built builtin-init exception (no
/// user `__init__` between the class and `BaseException`), mirroring the
/// CPython 3.14 `tp_init` surface:
///
/// - the `ImportError` family binds `name=`/`path=`/`name_from=`
///   (`ImportError.__init__`'s clinic keywords) onto the instance, readable
///   through the fixed attribute surface with a None default;
/// - every other builtin init rejects keywords with the typed, catchable
///   TypeError `_PyArg_NoKeywords` raises.
///
/// Returns `instance`, or NULL with the TypeError pending.  The family and
/// the rejection message dispatch on the INSTANCE type, matching CPython's
/// `type_call`, which re-reads `Py_TYPE(obj)` before running `tp_init`.
pub(super) fn apply_builtin_exception_keywords(instance: *mut PyObject, keywords: KeywordArgs<'_>) -> *mut PyObject {
    // SAFETY: `instance` is a live boxed exception from the caller.
    let ty = unsafe { (*instance).ob_type };
    let is_import_error_family = super::with_runtime(|runtime| unsafe {
        is_exception_subclass(ty, runtime.exception_types.import_error.cast_const())
    })
    .unwrap_or(false);
    if !is_import_error_family {
        // SAFETY: exception instances always carry a live type descriptor.
        let name = unsafe { (*ty).name() };
        return raise_type_error_text(&format!("{name}() takes no keyword arguments"));
    }
    // Validate every keyword before storing any: CPython's clinic parse
    // rejects the whole call without observable partial writes.
    for &name_id in keywords.names {
        if !matches!(intern::resolve(name_id).as_deref(), Some("name" | "path" | "name_from")) {
            let spelling = intern::resolve(name_id).unwrap_or_else(|| format!("<interned:{name_id}>"));
            // CPython 3.14's `ImportError.__init__` reports the clinic name
            // `ImportError()` for the whole family, ModuleNotFoundError included.
            return raise_type_error_text(&format!("ImportError() got an unexpected keyword argument '{spelling}'"));
        }
    }
    for (&name_id, &value) in keywords.names.iter().zip(keywords.values.iter()) {
        // SAFETY: `instance` is a live exception-layout allocation.
        unsafe { crate::types::exc::set_exception_instance_attr(instance, name_id, value) };
    }
    instance
}

fn active_context() -> *mut PyObject {
    pending_exception_object().unwrap_or(ptr::null_mut())
}

thread_local! {
    static STOP_ITERATION_TRACEBACK_SUPPRESS_DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub(crate) struct StopIterationTracebackGuard;

pub(crate) fn suppress_stop_iteration_traceback() -> StopIterationTracebackGuard {
    STOP_ITERATION_TRACEBACK_SUPPRESS_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
    StopIterationTracebackGuard
}

impl Drop for StopIterationTracebackGuard {
    fn drop(&mut self) {
        STOP_ITERATION_TRACEBACK_SUPPRESS_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

fn stop_iteration_traceback_suppressed() -> bool {
    STOP_ITERATION_TRACEBACK_SUPPRESS_DEPTH.with(|depth| depth.get() != 0)
}

pub(super) fn alloc_exception_object(
    runtime: &Runtime,
    ty: *mut PyType,
    message: *mut PyObject,
    cause: *mut PyObject,
) -> Result<*mut PyObject, String> {
    if ty.is_null() {
        return Err("exception type is null".to_owned());
    }

    let object = runtime
        .heap
        .alloc(core::mem::size_of::<PyBaseException>(), TYPE_ID_EXCEPTION)
        .cast::<PyBaseException>();
    let context = active_context();
    // SAFETY: `object` points to a freshly allocated zeroed block of the right size.
    unsafe {
        ptr::write(
            object,
            PyBaseException::new(ty.cast_const(), message, cause, context, ptr::null_mut()),
        );
    }
    Ok(as_object_ptr(object))
}

fn alloc_exception_group_object(
    runtime: &Runtime,
    ty: *mut PyType,
    message: *mut PyObject,
    exceptions: *mut PyObject,
    cause: *mut PyObject,
) -> Result<*mut PyObject, String> {
    if ty.is_null() {
        return Err("exception group type is null".to_owned());
    }
    if exceptions.is_null() {
        return Err("exception group members tuple is null".to_owned());
    }

    let object = runtime
        .heap
        .alloc(core::mem::size_of::<PyExceptionGroup>(), TYPE_ID_EXCEPTION_GROUP)
        .cast::<PyExceptionGroup>();
    let context = active_context();
    unsafe {
        ptr::write(
            object,
            PyExceptionGroup {
                base: PyBaseException::new(ty.cast_const(), message, cause, context, ptr::null_mut()),
                exceptions,
            },
        );
    }
    Ok(as_object_ptr(object))
}

fn alloc_exception_group_from_members(
    runtime: &Runtime,
    ty: *mut PyType,
    message: *mut PyObject,
    members: &[*mut PyObject],
) -> Result<*mut PyObject, String> {
    let exceptions = super::seq::alloc_tuple_from_slice(runtime, members)?;
    alloc_exception_group_object(runtime, ty, message, exceptions, ptr::null_mut())
}

/// Snapshots the active Python call stack into a traceback chain on `exception`.
///
/// CPython appends one entry per frame the exception propagates through; pon's
/// NULL-return unwinding has no per-frame hook, so the whole chain (module
/// level plus one entry per active compiled Python call) is materialized at
/// raise time instead.  A raised-again exception keeps its previous chain as
/// the deeper suffix, matching CPython's most-recent-call-last ordering.
///
/// Line precision: the raise-site entry reads the live `pon_current_line`
/// cell; every outer frame reports the statement line it was executing when it
/// made the next-inner compiled call (saved by `CurrentFunctionGuard` at
/// push).  Lines are statement-granular, `0` when code was lowered without
/// source text.
///
/// Known divergences (best effort): frames above the eventual handler are
/// indistinguishable from propagated frames, native builtin shims count as one
/// call level, and generator resume frames are neither counted nor
/// line-restored (a raise later in the resuming statement reports the
/// generator body's last stored line).
fn attach_current_traceback(runtime: &Runtime, exception: *mut PyObject) {
    if exception.is_null() || is_diagnostic_sentinel(exception) {
        return;
    }
    runtime.heap.register_type(
        TYPE_ID_TRACEBACK,
        GcTypeInfo {
            size: size_of::<PyTraceback>(),
            trace: trace_traceback,
            finalize: None,
        },
    );
    // Frames may be allocated before any generator ran; mirror the legacy
    // registration in `abi::gen` (`register_type` replaces idempotently).
    runtime.heap.register_type(
        TYPE_ID_FRAME,
        GcTypeInfo {
            size: size_of::<PyFrame>(),
            trace: trace_frame,
            finalize: Some(finalize_frame),
        },
    );
    let frame_type = ensure_frame_type(runtime._type_type);
    let traceback_type = ensure_traceback_type(runtime._type_type);
    let (python_calls, caller_lines) = super::CURRENT_FUNCTION_STACK.with(|stack| {
        let stack = stack.borrow();
        let lines: Vec<u32> = stack.iter().map(|call| call.caller_line).collect();
        (stack.len(), lines)
    });

    // SAFETY: Raise-path callers pass a live boxed exception instance.
    let slot = unsafe { &mut (*exception.cast::<PyBaseException>()).traceback };
    let mut next = *slot;
    // Innermost (raise site) first: its tb_next is the surviving prior chain;
    // the last-built entry is the outermost frame and becomes the new head.
    // Entry `depth` sits `depth` call levels above the raise site: depth 0
    // reads the live line cell, depth k the line saved when the call chain's
    // k-th-innermost guard was pushed (= that frame's active call statement).
    for depth in 0..=python_calls {
        let line = if depth == 0 {
            super::current_line()
        } else {
            caller_lines[python_calls - depth]
        };
        let frame = runtime.heap.alloc(size_of::<PyFrame>(), TYPE_ID_FRAME).cast::<PyFrame>();
        // SAFETY: `frame` points to a freshly allocated zeroed block of the right size.
        unsafe {
            ptr::write(frame, PyFrame::new(frame_type.cast_const(), 0, ptr::null_mut()));
            (*frame).line = line;
        }
        let entry = runtime
            .heap
            .alloc(size_of::<PyTraceback>(), TYPE_ID_TRACEBACK)
            .cast::<PyTraceback>();
        // SAFETY: `entry` points to a freshly allocated zeroed block of the right size.
        unsafe {
            ptr::write(
                entry,
                PyTraceback::new(traceback_type.cast_const(), frame.cast::<PyObject>(), i64::from(line), next),
            );
        }
        next = entry.cast::<PyObject>();
    }
    *slot = next;
}

fn attach_innermost_traceback(runtime: &Runtime, exception: *mut PyObject) {
    if exception.is_null() || is_diagnostic_sentinel(exception) {
        return;
    }
    runtime.heap.register_type(
        TYPE_ID_TRACEBACK,
        GcTypeInfo {
            size: size_of::<PyTraceback>(),
            trace: trace_traceback,
            finalize: None,
        },
    );
    runtime.heap.register_type(
        TYPE_ID_FRAME,
        GcTypeInfo {
            size: size_of::<PyFrame>(),
            trace: trace_frame,
            finalize: Some(finalize_frame),
        },
    );
    let frame_type = ensure_frame_type(runtime._type_type);
    let traceback_type = ensure_traceback_type(runtime._type_type);
    let line = super::current_line();

    // SAFETY: Raise-path callers pass a live boxed exception instance.
    let slot = unsafe { &mut (*exception.cast::<PyBaseException>()).traceback };
    let next = *slot;
    let frame = runtime.heap.alloc(size_of::<PyFrame>(), TYPE_ID_FRAME).cast::<PyFrame>();
    // SAFETY: `frame` points to a freshly allocated zeroed block of the right size.
    unsafe {
        ptr::write(frame, PyFrame::new(frame_type.cast_const(), 0, ptr::null_mut()));
        (*frame).line = line;
    }
    let entry = runtime
        .heap
        .alloc(size_of::<PyTraceback>(), TYPE_ID_TRACEBACK)
        .cast::<PyTraceback>();
    // SAFETY: `entry` points to a freshly allocated zeroed block of the right size.
    unsafe {
        ptr::write(
            entry,
            PyTraceback::new(traceback_type.cast_const(), frame.cast::<PyObject>(), i64::from(line), next),
        );
    }
    *slot = entry.cast::<PyObject>();
}

/// Installs `exception` as pending WITHOUT touching its traceback.
///
/// Restore-flavored paths (`pon_exc_restore`, `except*` bookkeeping) reinstall
/// exceptions that already own their chain; CPython appends traceback entries
/// only on raise, so only [`raise_current_exception`] attaches.
fn set_current_exception(runtime: &Runtime, exception: *mut PyObject) {
    let (diagnostic, lazy_display) = exception_diagnostic(runtime, exception);
    if lazy_display {
        pon_err_set_object_lazy_display(exception, diagnostic);
    } else {
        pon_err_set_object(exception, diagnostic);
    }
}

/// Raise-flavored install: appends the current-stack traceback segment first.
fn raise_current_exception(runtime: &Runtime, exception: *mut PyObject) {
    attach_current_traceback(runtime, exception);
    set_current_exception(runtime, exception);
}

fn raise_builtin_value(runtime: &Runtime, kind: ExceptionKind, value: *mut PyObject, diagnostic: String) -> *mut PyObject {
    match alloc_exception_object(runtime, runtime.exception_types.get(kind), value, ptr::null_mut()) {
        Ok(exception) => {
            attach_current_traceback(runtime, exception);
            pon_err_set_object(exception, diagnostic);
            ptr::null_mut()
        }
        Err(message) => super::return_null_with_error(message),
    }
}

/// Raises a builtin exception without attaching a traceback.
///
/// Internal iterator exhaustion is caught and cleared by loop/unpack
/// consumers; materializing the full Python call chain for every exhausted
/// iterator is pure allocator load.  Explicit user `raise` still routes
/// through `pon_raise`/`raise_current_exception` and keeps the full walk.
fn raise_builtin_value_without_traceback(
    runtime: &Runtime,
    kind: ExceptionKind,
    value: *mut PyObject,
    diagnostic: String,
) -> *mut PyObject {
    match alloc_exception_object(runtime, runtime.exception_types.get(kind), value, ptr::null_mut()) {
        Ok(exception) => {
            pon_err_set_object(exception, diagnostic);
            ptr::null_mut()
        }
        Err(message) => super::return_null_with_error(message),
    }
}

fn raise_builtin_value_with_innermost_traceback(
    runtime: &Runtime,
    kind: ExceptionKind,
    value: *mut PyObject,
    diagnostic: String,
) -> *mut PyObject {
    match alloc_exception_object(runtime, runtime.exception_types.get(kind), value, ptr::null_mut()) {
        Ok(exception) => {
            attach_innermost_traceback(runtime, exception);
            pon_err_set_object(exception, diagnostic);
            ptr::null_mut()
        }
        Err(message) => super::return_null_with_error(message),
    }
}

fn raise_builtin_text(runtime: &Runtime, kind: ExceptionKind, text: &str) -> *mut PyObject {
    match super::alloc_unicode(runtime, text.as_bytes()) {
        Ok(message) => raise_builtin_value(runtime, kind, message, format!("{}: {text}", exception_kind_name(kind))),
        Err(message) => super::return_null_with_error(message),
    }
}

fn raise_type_error_text(text: &str) -> *mut PyObject {
    match ensure_runtime_for_exc() {
        Ok(()) => match super::with_runtime(|runtime| raise_builtin_text(runtime, ExceptionKind::TypeError, text)) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}

fn import_error_name(text: &str) -> Option<&str> {
    text.strip_prefix("No module named '")
        .and_then(|suffix| suffix.strip_suffix('\''))
        .or_else(|| {
            text.strip_prefix("import of ")
                .and_then(|suffix| suffix.strip_suffix(" halted; None in sys.modules"))
        })
}

/// Raises the typed import failure for `text`: a missing-module diagnostic
/// (`No module named '...'`, the exact text `resolve_module_by_name` emits)
/// raises `ModuleNotFoundError` like CPython — `subprocess` gates its whole
/// Windows surface on `except ModuleNotFoundError: import msvcrt` — as does
/// the blocked-import halt (`import of X halted; None in sys.modules`, the
/// `sys.modules[name] = None` sentinel `test.support.import_helper` plants;
/// stdlib accelerator fallbacks catch it as ImportError).  For those
/// `ModuleNotFoundError` cases, mirror CPython's `exc.name` payload too so
/// guards like `importlib.abc`'s `if exc.name != '_frozen_importlib': raise`
/// can distinguish their own optional import from a deeper failure.
pub fn raise_import_error_text(text: &str) -> *mut PyObject {
    let kind = if text.starts_with("No module named ")
        || (text.starts_with("import of ") && text.ends_with(" halted; None in sys.modules"))
    {
        ExceptionKind::ModuleNotFoundError
    } else {
        ExceptionKind::ImportError
    };
    match ensure_runtime_for_exc() {
        Ok(()) => match super::with_runtime(|runtime| {
            let result = raise_builtin_text(runtime, kind, text);
            if matches!(kind, ExceptionKind::ModuleNotFoundError)
                && let Some(name) = import_error_name(text)
                && let Some(exception) = pending_exception_object()
                && let Ok(name_object) = super::alloc_unicode(runtime, name.as_bytes())
            {
                unsafe {
                    crate::types::exc::set_exception_instance_attr(exception, intern::intern("name"), name_object);
                }
            }
            result
        }) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}

/// Raises a typed `AttributeError(text)` for failed attribute lookups whose
/// caller formats the message itself (e.g. module attributes, where CPython
/// says `module 'x' has no attribute 'y'`).
pub fn raise_attribute_error_text(text: &str) -> *mut PyObject {
    match ensure_runtime_for_exc() {
        Ok(()) => match super::with_runtime(|runtime| raise_builtin_text(runtime, ExceptionKind::AttributeError, text)) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}

/// Raises a typed `NameError(text)` for failed name/global/builtin lookups.
pub(super) fn raise_name_error_text(text: &str) -> *mut PyObject {
    match ensure_runtime_for_exc() {
        Ok(()) => match super::with_runtime(|runtime| raise_builtin_text(runtime, ExceptionKind::NameError, text)) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}

/// Raises a typed `IndexError(text)` for failed sequence indexes.
pub(super) fn raise_index_error_text(text: &str) -> *mut PyObject {
    match ensure_runtime_for_exc() {
        Ok(()) => match super::with_runtime(|runtime| raise_builtin_text(runtime, ExceptionKind::IndexError, text)) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}

/// Raises a typed `LookupError(text)` (native `_contextvars.ContextVar.get`
/// with no binding, call default, or constructor default).
pub(crate) fn raise_lookup_error_text(text: &str) -> *mut PyObject {
    match ensure_runtime_for_exc() {
        Ok(()) => match super::with_runtime(|runtime| raise_builtin_text(runtime, ExceptionKind::LookupError, text)) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}

/// Raises a typed `RuntimeError(text)` (e.g. re-entered `Context.run` or a
/// reused `Token`).
pub(crate) fn raise_runtime_error_text(text: &str) -> *mut PyObject {
    match ensure_runtime_for_exc() {
        Ok(()) => match super::with_runtime(|runtime| raise_builtin_text(runtime, ExceptionKind::RuntimeError, text)) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}

/// Raises a typed builtin exception carrying a plain-text message (native
/// `_codecs`: `Unicode*Error` codec failures, `LookupError` registry misses,
/// `NotImplementedError` stubs).
pub(crate) fn raise_kind_error_text(kind: ExceptionKind, text: &str) -> *mut PyObject {
    match ensure_runtime_for_exc() {
        Ok(()) => match super::with_runtime(|runtime| raise_builtin_text(runtime, kind, text)) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}
/// Converts a legacy `"BuiltinException: message"` diagnostic into a boxed,
/// catchable builtin exception when the prefix names a known exception type.
pub(crate) fn raise_prefixed_diagnostic_text(text: &str) -> bool {
    if pending_exception_object().is_some() {
        return true;
    }
    let Some((kind, payload)) = prefixed_exception_payload(text) else {
        return false;
    };
    let Ok(()) = ensure_runtime_for_exc() else {
        return false;
    };
    super::with_runtime(|runtime| {
        raise_builtin_text(runtime, kind, payload);
        true
    })
    .unwrap_or(false)
}

fn prefixed_exception_payload(text: &str) -> Option<(ExceptionKind, &str)> {
    let (name, payload) = text.split_once(':')?;
    let kind = exception_kind_from_name(name)?;
    Some((kind, payload.strip_prefix(' ').unwrap_or(payload)))
}


/// Raises a typed builtin exception with NO argument payload (`args == ()`),
/// e.g. native `_signal.default_int_handler`'s bare `KeyboardInterrupt`.
pub(crate) fn raise_kind_error_no_args(kind: ExceptionKind) -> *mut PyObject {
    match ensure_runtime_for_exc() {
        Ok(()) => match super::with_runtime(|runtime| raise_builtin_value(runtime, kind, ptr::null_mut(), exception_kind_name(kind).to_owned())) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}

/// Raises `SystemExit(code)` for native `sys.exit`.  `code` is the `.code`
/// payload carried in `args`: NULL yields `args == ()` (`.code` is `None`,
/// exit status 0); an int gives that status; any other object is printed by
/// the top-level handler before exiting with status 1.
pub fn raise_system_exit(code: *mut PyObject) -> *mut PyObject {
    match ensure_runtime_for_exc() {
        Ok(()) => match super::with_runtime(|runtime| {
            raise_builtin_value(runtime, ExceptionKind::SystemExit, code, "SystemExit".to_owned())
        }) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}

/// Consume a pending `SystemExit` and map it to a process exit status
/// (CPython `sys.exit` semantics): a `None`/absent `.code` → 0, an int → that
/// value, any other object → 1 after printing its `str()` to stderr.  Returns
/// `None` when the pending exception is not a `SystemExit`, leaving it in place
/// for the normal uncaught-error report.
pub fn take_pending_system_exit() -> Option<i32> {
    if !pending_exception_is("SystemExit") {
        return None;
    }
    let code = pending_exception_object()
        .map(|exc| unsafe { (*exc.cast::<PyBaseException>()).message })
        .unwrap_or(ptr::null_mut());
    let status = system_exit_status(code);
    pon_err_clear();
    Some(status)
}

/// Maps a `SystemExit.code` payload to a process exit status.
fn system_exit_status(code: *mut PyObject) -> i32 {
    let raw = crate::tag::untag_arg(code);
    if raw.is_null() || unsafe { crate::types::dict::type_name(raw) } == Some("NoneType") {
        return 0;
    }
    use num_traits::ToPrimitive;
    if let Some(value) = unsafe { crate::types::int::to_bigint_including_bool(raw) }.and_then(|v| v.to_i64()) {
        return value as i32;
    }
    let text = unsafe { crate::types::type_::unicode_text(raw) }
        .map(str::to_owned)
        .unwrap_or_else(|| crate::native::builtins_mod::repr_text(raw));
    eprintln!("{text}");
    1
}

fn raise_message_exception(kind: ExceptionKind, ptr: *const u8, len: usize) -> *mut PyObject {
    let bytes = match bytes_from_raw(ptr, len) {
        Ok(bytes) => bytes,
        Err(message) => return raise_type_error_text(&message),
    };

    match ensure_runtime_for_exc() {
        Ok(()) => match super::with_runtime(|runtime| match super::alloc_unicode(runtime, bytes) {
            Ok(message) => raise_builtin_value(runtime, kind, message, exception_diagnostic_from_unicode(runtime, kind, message)),
            Err(message) => raise_builtin_text(runtime, ExceptionKind::TypeError, &message),
        }) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}

fn raise_value_exception(kind: ExceptionKind, value: *mut PyObject, diagnostic: String) -> *mut PyObject {
    match ensure_runtime_for_exc() {
        Ok(()) => match super::with_runtime(|runtime| raise_builtin_value(runtime, kind, value, diagnostic)) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}

/// Fast path for iterator/generator exhaustion.
///
/// User-visible `next()`/`tp_iternext` exhaustion gets a single innermost
/// traceback entry, matching CPython's caught `StopIteration` shape without
/// walking the whole call stack.  Internal loop/unpack/delegation consumers
/// bracket their iterator step with [`suppress_stop_iteration_traceback`] and
/// get no traceback at all because they catch-and-clear immediately.
fn raise_stop_iteration(value: *mut PyObject) -> *mut PyObject {
    match ensure_runtime_for_exc() {
        Ok(()) => match super::with_runtime(|runtime| {
            let diagnostic = "StopIteration".to_owned();
            if stop_iteration_traceback_suppressed() {
                raise_builtin_value_without_traceback(runtime, ExceptionKind::StopIteration, value, diagnostic)
            } else {
                raise_builtin_value_with_innermost_traceback(runtime, ExceptionKind::StopIteration, value, diagnostic)
            }
        }) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}

pub(super) fn is_type_object(runtime: &Runtime, object: *mut PyObject) -> bool {
    if object.is_null() {
        return false;
    }
    // SAFETY: A non-NULL `object` is expected to be a live boxed object from the ABI.
    unsafe { (*object).ob_type == runtime._type_type.cast_const() }
}

fn exception_kind_name(kind: ExceptionKind) -> &'static str {
    match kind {
        ExceptionKind::BaseException => "BaseException",
        ExceptionKind::BaseExceptionGroup => "BaseExceptionGroup",
        ExceptionKind::GeneratorExit => "GeneratorExit",
        ExceptionKind::KeyboardInterrupt => "KeyboardInterrupt",
        ExceptionKind::SystemExit => "SystemExit",
        ExceptionKind::Exception => "Exception",
        ExceptionKind::ArithmeticError => "ArithmeticError",
        ExceptionKind::FloatingPointError => "FloatingPointError",
        ExceptionKind::OverflowError => "OverflowError",
        ExceptionKind::ZeroDivisionError => "ZeroDivisionError",
        ExceptionKind::AssertionError => "AssertionError",
        ExceptionKind::AttributeError => "AttributeError",
        ExceptionKind::BufferError => "BufferError",
        ExceptionKind::EOFError => "EOFError",
        ExceptionKind::ImportError => "ImportError",
        ExceptionKind::ModuleNotFoundError => "ModuleNotFoundError",
        ExceptionKind::LookupError => "LookupError",
        ExceptionKind::IndexError => "IndexError",
        ExceptionKind::KeyError => "KeyError",
        ExceptionKind::MemoryError => "MemoryError",
        ExceptionKind::NameError => "NameError",
        ExceptionKind::UnboundLocalError => "UnboundLocalError",
        ExceptionKind::OSError => "OSError",
        ExceptionKind::BlockingIOError => "BlockingIOError",
        ExceptionKind::ChildProcessError => "ChildProcessError",
        ExceptionKind::ConnectionError => "ConnectionError",
        ExceptionKind::BrokenPipeError => "BrokenPipeError",
        ExceptionKind::ConnectionAbortedError => "ConnectionAbortedError",
        ExceptionKind::ConnectionRefusedError => "ConnectionRefusedError",
        ExceptionKind::ConnectionResetError => "ConnectionResetError",
        ExceptionKind::FileExistsError => "FileExistsError",
        ExceptionKind::FileNotFoundError => "FileNotFoundError",
        ExceptionKind::InterruptedError => "InterruptedError",
        ExceptionKind::IsADirectoryError => "IsADirectoryError",
        ExceptionKind::NotADirectoryError => "NotADirectoryError",
        ExceptionKind::PermissionError => "PermissionError",
        ExceptionKind::ProcessLookupError => "ProcessLookupError",
        ExceptionKind::TimeoutError => "TimeoutError",
        ExceptionKind::ReferenceError => "ReferenceError",
        ExceptionKind::RuntimeError => "RuntimeError",
        ExceptionKind::NotImplementedError => "NotImplementedError",
        ExceptionKind::PythonFinalizationError => "PythonFinalizationError",
        ExceptionKind::RecursionError => "RecursionError",
        ExceptionKind::StopAsyncIteration => "StopAsyncIteration",
        ExceptionKind::StopIteration => "StopIteration",
        ExceptionKind::SyntaxError => "SyntaxError",
        ExceptionKind::IndentationError => "IndentationError",
        ExceptionKind::TabError => "TabError",
        ExceptionKind::SystemError => "SystemError",
        ExceptionKind::TypeError => "TypeError",
        ExceptionKind::ValueError => "ValueError",
        ExceptionKind::UnicodeError => "UnicodeError",
        ExceptionKind::UnicodeDecodeError => "UnicodeDecodeError",
        ExceptionKind::UnicodeEncodeError => "UnicodeEncodeError",
        ExceptionKind::UnicodeTranslateError => "UnicodeTranslateError",
        ExceptionKind::Warning => "Warning",
        ExceptionKind::BytesWarning => "BytesWarning",
        ExceptionKind::DeprecationWarning => "DeprecationWarning",
        ExceptionKind::EncodingWarning => "EncodingWarning",
        ExceptionKind::FutureWarning => "FutureWarning",
        ExceptionKind::ImportWarning => "ImportWarning",
        ExceptionKind::PendingDeprecationWarning => "PendingDeprecationWarning",
        ExceptionKind::ResourceWarning => "ResourceWarning",
        ExceptionKind::RuntimeWarning => "RuntimeWarning",
        ExceptionKind::SyntaxWarning => "SyntaxWarning",
        ExceptionKind::UnicodeWarning => "UnicodeWarning",
        ExceptionKind::UserWarning => "UserWarning",
        ExceptionKind::ExceptionGroup => "ExceptionGroup",
    }
}
fn exception_kind_from_name(name: &str) -> Option<ExceptionKind> {
    Some(match name {
        "BaseException" => ExceptionKind::BaseException,
        "BaseExceptionGroup" => ExceptionKind::BaseExceptionGroup,
        "GeneratorExit" => ExceptionKind::GeneratorExit,
        "KeyboardInterrupt" => ExceptionKind::KeyboardInterrupt,
        "SystemExit" => ExceptionKind::SystemExit,
        "Exception" => ExceptionKind::Exception,
        "ArithmeticError" => ExceptionKind::ArithmeticError,
        "FloatingPointError" => ExceptionKind::FloatingPointError,
        "OverflowError" => ExceptionKind::OverflowError,
        "ZeroDivisionError" => ExceptionKind::ZeroDivisionError,
        "AssertionError" => ExceptionKind::AssertionError,
        "AttributeError" => ExceptionKind::AttributeError,
        "BufferError" => ExceptionKind::BufferError,
        "EOFError" => ExceptionKind::EOFError,
        "ImportError" => ExceptionKind::ImportError,
        "ModuleNotFoundError" => ExceptionKind::ModuleNotFoundError,
        "LookupError" => ExceptionKind::LookupError,
        "IndexError" => ExceptionKind::IndexError,
        "KeyError" => ExceptionKind::KeyError,
        "MemoryError" => ExceptionKind::MemoryError,
        "NameError" => ExceptionKind::NameError,
        "UnboundLocalError" => ExceptionKind::UnboundLocalError,
        "OSError" => ExceptionKind::OSError,
        "BlockingIOError" => ExceptionKind::BlockingIOError,
        "ChildProcessError" => ExceptionKind::ChildProcessError,
        "ConnectionError" => ExceptionKind::ConnectionError,
        "BrokenPipeError" => ExceptionKind::BrokenPipeError,
        "ConnectionAbortedError" => ExceptionKind::ConnectionAbortedError,
        "ConnectionRefusedError" => ExceptionKind::ConnectionRefusedError,
        "ConnectionResetError" => ExceptionKind::ConnectionResetError,
        "FileExistsError" => ExceptionKind::FileExistsError,
        "FileNotFoundError" => ExceptionKind::FileNotFoundError,
        "InterruptedError" => ExceptionKind::InterruptedError,
        "IsADirectoryError" => ExceptionKind::IsADirectoryError,
        "NotADirectoryError" => ExceptionKind::NotADirectoryError,
        "PermissionError" => ExceptionKind::PermissionError,
        "ProcessLookupError" => ExceptionKind::ProcessLookupError,
        "TimeoutError" => ExceptionKind::TimeoutError,
        "ReferenceError" => ExceptionKind::ReferenceError,
        "RuntimeError" => ExceptionKind::RuntimeError,
        "NotImplementedError" => ExceptionKind::NotImplementedError,
        "PythonFinalizationError" => ExceptionKind::PythonFinalizationError,
        "RecursionError" => ExceptionKind::RecursionError,
        "StopAsyncIteration" => ExceptionKind::StopAsyncIteration,
        "StopIteration" => ExceptionKind::StopIteration,
        "SyntaxError" => ExceptionKind::SyntaxError,
        "IndentationError" => ExceptionKind::IndentationError,
        "TabError" => ExceptionKind::TabError,
        "SystemError" => ExceptionKind::SystemError,
        "TypeError" => ExceptionKind::TypeError,
        "ValueError" => ExceptionKind::ValueError,
        "UnicodeError" => ExceptionKind::UnicodeError,
        "UnicodeDecodeError" => ExceptionKind::UnicodeDecodeError,
        "UnicodeEncodeError" => ExceptionKind::UnicodeEncodeError,
        "UnicodeTranslateError" => ExceptionKind::UnicodeTranslateError,
        "Warning" => ExceptionKind::Warning,
        "BytesWarning" => ExceptionKind::BytesWarning,
        "DeprecationWarning" => ExceptionKind::DeprecationWarning,
        "EncodingWarning" => ExceptionKind::EncodingWarning,
        "FutureWarning" => ExceptionKind::FutureWarning,
        "ImportWarning" => ExceptionKind::ImportWarning,
        "PendingDeprecationWarning" => ExceptionKind::PendingDeprecationWarning,
        "ResourceWarning" => ExceptionKind::ResourceWarning,
        "RuntimeWarning" => ExceptionKind::RuntimeWarning,
        "SyntaxWarning" => ExceptionKind::SyntaxWarning,
        "UnicodeWarning" => ExceptionKind::UnicodeWarning,
        "UserWarning" => ExceptionKind::UserWarning,
        "ExceptionGroup" => ExceptionKind::ExceptionGroup,
        _ => return None,
    })
}


fn exception_diagnostic_from_unicode(runtime: &Runtime, kind: ExceptionKind, value: *mut PyObject) -> String {
    let name = exception_kind_name(kind);
    if value.is_null() {
        return name.to_owned();
    }

    // SAFETY: `value` is a live boxed object allocated by the runtime.
    unsafe {
        if is_exact_type(value, runtime.unicode_type.cast_const()) {
            if let Some(text) = (*value.cast::<crate::object::PyUnicode>()).as_str() {
                return format!("{name}: {text}");
            }
        }
    }
    name.to_owned()
}

pub(crate) fn exception_display_diagnostic(exception: *mut PyObject, placeholder: &str) -> String {
    if exception.is_null() || is_diagnostic_sentinel(exception) {
        return placeholder.to_owned();
    }

    // SAFETY: Callers pass a live boxed exception instance.
    unsafe {
        let ty = (*exception).ob_type;
        let name = if ty.is_null() { "BaseException" } else { (*ty).name() };
        let text = crate::native::builtins_mod::str_text(exception);
        if text.is_empty() {
            name.to_owned()
        } else {
            format!("{name}: {text}")
        }
    }
}

fn exception_uses_python_str_override(exception: *mut PyObject) -> bool {
    if exception.is_null() || is_diagnostic_sentinel(exception) {
        return false;
    }
    // SAFETY: Callers pass a live boxed exception instance.
    unsafe {
        let ty = (*exception).ob_type.cast_mut();
        if !crate::types::type_::type_dispatches_python_dunders(ty.cast_const()) {
            return false;
        }
        let hook = crate::descr::lookup_in_type(ty, crate::intern::intern("__str__"));
        !hook.is_null() && hook != super::object_dunder_str_carrier()
    }
}

fn exception_diagnostic(runtime: &Runtime, exception: *mut PyObject) -> (String, bool) {
    if exception.is_null() {
        return ("NULL exception".to_owned(), false);
    }
    if is_diagnostic_sentinel(exception) {
        return ("diagnostic exception".to_owned(), false);
    }

    // SAFETY: Callers pass a live boxed exception instance.
    unsafe {
        let ty = (*exception).ob_type;
        let name = if ty.is_null() { "BaseException" } else { (*ty).name() };
        let exception_ref = &*exception.cast::<PyBaseException>();
        if exception_uses_python_str_override(exception) {
            return (name.to_owned(), true);
        }
        if !exception_ref.args.is_null() {
            return (name.to_owned(), true);
        }
        let message = exception_ref.message;
        if !message.is_null() && is_exact_type(message, runtime.unicode_type.cast_const()) {
            if let Some(text) = (*message.cast::<crate::object::PyUnicode>()).as_str() {
                return (format!("{name}: {text}"), false);
            }
        }
        if !message.is_null() {
            return (name.to_owned(), true);
        }
        (name.to_owned(), false)
    }
}

unsafe fn set_exception_links(exception: *mut PyObject, cause: *mut PyObject) {
    if exception.is_null() || is_diagnostic_sentinel(exception) {
        return;
    }
    let context = active_context();
    // SAFETY: Caller validated that `exception` is a live base-exception instance.
    let exception = unsafe { &mut *exception.cast::<PyBaseException>() };
    exception.cause = cause;
    if !cause.is_null() {
        // An explicit `raise ... from ...` (even `from None`) suppresses the
        // implicit-context display (PEP 409).
        exception.suppress_context = true;
    }
    if !context.is_null() && !core::ptr::eq(context, exception as *mut PyBaseException as *mut PyObject) {
        exception.context = context;
    }
}

/// Raises an existing exception instance or exception type, records `cause`, and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_raise(exc: *mut PyObject, cause: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(exc, cause);
    super::catch_object_helper(|| {
        if exc.is_null() {
            // A NULL operand means the raise expression itself failed (e.g.
            // the exception constructor rejected its keywords): propagate
            // that pending error instead of masking it with the
            // derive-from-BaseException morph.
            if pon_err_occurred() {
                return ptr::null_mut();
            }
            return raise_type_error_text("exceptions must derive from BaseException");
        }
        if is_diagnostic_sentinel(exc) {
            // The compiler's cleanup/reraise path may hand back the current
            // message-only sentinel. It is not a Python object; keep reporting
            // the original diagnostic instead of morphing it into TypeError.
            return ptr::null_mut();
        }
        if let Err(message) = ensure_runtime_for_exc() {
            return super::return_null_with_error(message);
        }

        match super::with_runtime(|runtime| {
            if is_type_object(runtime, exc) {
                let ty = exc.cast::<PyType>();
                // SAFETY: `ty` is a live type object and the root type is initialized.
                if unsafe { !is_exception_subclass(ty.cast_const(), runtime.exception_types.base_exception.cast_const()) } {
                    return raise_builtin_text(runtime, ExceptionKind::TypeError, "exceptions must derive from BaseException");
                }
                match alloc_exception_object(runtime, ty, ptr::null_mut(), cause) {
                    Ok(exception) => {
                        if !cause.is_null() {
                            // SAFETY: Freshly allocated base-exception layout.
                            unsafe { (*exception.cast::<PyBaseException>()).suppress_context = true };
                        }
                        raise_current_exception(runtime, exception);
                        ptr::null_mut()
                    }
                    Err(message) => super::return_null_with_error(message),
                }
            } else {
                // SAFETY: `exc` is a live boxed object from the ABI.
                if unsafe { !is_exception_instance(exc, runtime.exception_types.base_exception.cast_const()) } {
                    return raise_builtin_text(runtime, ExceptionKind::TypeError, "exceptions must derive from BaseException");
                }
                // SAFETY: The branch above validated the exception instance layout.
                unsafe {
                    set_exception_links(exc, cause);
                }
                raise_current_exception(runtime, exc);
                ptr::null_mut()
            }
        }) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        }
    })
}

/// Re-raises the pending exception and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_reraise() -> *mut PyObject {
    super::catch_object_helper(|| {
        if pon_err_occurred() {
            ptr::null_mut()
        } else {
            match ensure_runtime_for_exc() {
                Ok(()) => match super::with_runtime(|runtime| raise_builtin_text(runtime, ExceptionKind::RuntimeError, "no active exception to reraise")) {
                    Some(result) => result,
                    None => super::return_null_with_error("runtime is not initialized"),
                },
                Err(message) => super::return_null_with_error(message),
            }
        }
    })
}

/// Raises `TypeError(message)` and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_raise_type_error(ptr: *const u8, len: usize) -> *mut PyObject {
    super::catch_object_helper(|| raise_message_exception(ExceptionKind::TypeError, ptr, len))
}

/// Raises `ValueError(message)` and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_raise_value_error(ptr: *const u8, len: usize) -> *mut PyObject {
    super::catch_object_helper(|| raise_message_exception(ExceptionKind::ValueError, ptr, len))
}

/// Raises `ZeroDivisionError(message)` and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_raise_zero_division_error(ptr: *const u8, len: usize) -> *mut PyObject {
    super::catch_object_helper(|| raise_message_exception(ExceptionKind::ZeroDivisionError, ptr, len))
}

/// Raises `IndexError(message)` and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_raise_index_error(ptr: *const u8, len: usize) -> *mut PyObject {
    super::catch_object_helper(|| raise_message_exception(ExceptionKind::IndexError, ptr, len))
}

/// Raises `ReferenceError(message)` and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_raise_reference_error(ptr: *const u8, len: usize) -> *mut PyObject {
    super::catch_object_helper(|| raise_message_exception(ExceptionKind::ReferenceError, ptr, len))
}

/// Raises `OSError(message)` and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_raise_os_error(ptr: *const u8, len: usize) -> *mut PyObject {
    super::catch_object_helper(|| raise_message_exception(ExceptionKind::OSError, ptr, len))
}

/// Raises `KeyError(key)` and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_raise_key_error(key: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(key);
    super::catch_object_helper(|| raise_value_exception(ExceptionKind::KeyError, key, "KeyError".to_owned()))
}

/// Raises `AttributeError` for `obj.name` and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_raise_attribute_error(obj: *mut PyObject, name: u32) -> *mut PyObject {
    crate::untag_prelude!(obj);
    super::catch_object_helper(|| {
        let attribute = intern::resolve(name).unwrap_or_else(|| format!("<intern:{name}>"));
        let object_name = if obj.is_null() {
            "NULL".to_owned()
        } else if is_diagnostic_sentinel(obj) {
            "diagnostic".to_owned()
        } else {
            // SAFETY: A non-NULL non-sentinel `obj` is expected to be a live boxed object.
            unsafe {
                let ty = (*obj).ob_type;
                if ty.is_null() {
                    "object".to_owned()
                } else {
                    (*ty).name().to_owned()
                }
            }
        };
        let text = format!("'{object_name}' object has no attribute '{attribute}'");
        match ensure_runtime_for_exc() {
            Ok(()) => match super::with_runtime(|runtime| raise_builtin_text(runtime, ExceptionKind::AttributeError, &text)) {
                Some(result) => result,
                None => super::return_null_with_error("runtime is not initialized"),
            },
            Err(message) => super::return_null_with_error(message),
        }
    })
}

/// Raises `StopIteration(value)` and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_raise_stop_iteration(value: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(value);
    super::catch_object_helper(|| raise_stop_iteration(value))
}

/// Returns `1` when the current exception matches `exc_type`, `0` for no match, and `-1` on misuse.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_exc_matches(exc_type: *mut PyObject) -> c_int {
    crate::untag_prelude!(err = -1; exc_type);
    catch_i32_helper(|| {
        if exc_type.is_null() {
            raise_type_error_text("catching classes that do not inherit from BaseException is not allowed");
            return -1;
        }
        // CPython `except (A, B):` — a tuple target matches when any element
        // matches, walked in order; a non-type element reached before a match
        // raises the same TypeError as a scalar non-type target.
        if let Some(targets) = unsafe { super::seq::exact_tuple_slice(exc_type) } {
            for &target in targets {
                match unsafe { pon_exc_matches(target) } {
                    0 => {}
                    result => return result,
                }
            }
            return 0;
        }
        let current = thread_state_lock().current_exc;
        if current.is_null() {
            return 0;
        }
        if let Err(message) = ensure_runtime_for_exc() {
            super::return_minus_one_with_error(message);
            return -1;
        }

        match super::with_runtime(|runtime| {
            if !is_type_object(runtime, exc_type) {
                raise_builtin_text(runtime, ExceptionKind::TypeError, "catch target must be an exception type");
                return -1;
            }
            let ty = exc_type.cast::<PyType>();
            // SAFETY: `ty` is a live type object.
            if unsafe { !is_exception_subclass(ty.cast_const(), runtime.exception_types.base_exception.cast_const()) } {
                raise_builtin_text(runtime, ExceptionKind::TypeError, "catching classes that do not inherit from BaseException is not allowed");
                return -1;
            }
            if is_diagnostic_sentinel(current) {
                return 0;
            }
            // SAFETY: `current` is a live boxed object and `ty` is a live type object.
            if unsafe { is_exception_instance(current, ty.cast_const()) } {
                1
            } else {
                0
            }
        }) {
            Some(result) => result,
            None => {
                super::return_minus_one_with_error("runtime is not initialized");
                -1
            }
        }
    })
}

/// Takes the current exception, clears it, pushes it on the exception-state stack, and returns it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_exc_fetch() -> *mut PyObject {
    super::catch_object_helper(|| {
        let fetched = {
            let mut state = thread_state_lock();
            let fetched = state.current_exc;
            state.current_exc = ptr::null_mut();
            if !fetched.is_null() {
                state.push_exception_state(fetched);
            }
            fetched
        };
        pon_err_clear();
        fetched
    })
}

/// Restores a saved exception, consuming the matching saved state stack entry when present.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_exc_restore(saved: *mut PyObject) -> c_int {
    crate::untag_prelude!(err = -1; saved);
    catch_i32_helper(|| {
        let stacked = {
            let mut state = thread_state_lock();
            state.pop_exception_state()
        };
        let restored = if saved.is_null() {
            stacked.unwrap_or(ptr::null_mut())
        } else {
            saved
        };

        if restored.is_null() {
            pon_err_clear();
        } else if is_diagnostic_sentinel(restored) {
            pon_err_set_object(restored, "restored diagnostic exception");
        } else if let Some(()) = super::with_runtime(|runtime| set_current_exception(runtime, restored)) {
        } else {
            pon_err_set_object(restored, "restored exception");
        }
        0
    })
}

enum SplitCond {
    Types(Vec<*mut PyType>),
    Callable(*mut PyObject),
}

struct SplitOutcome {
    matched: *mut PyObject,
    rest: *mut PyObject,
}

fn type_name_is(object: *mut PyObject, expected: &str) -> bool {
    if object.is_null() {
        return false;
    }
    let ty = unsafe { (*object).ob_type };
    !ty.is_null() && unsafe { (*ty).name() == expected }
}

fn validate_exception_type(runtime: &Runtime, ty: *mut PyType, forbid_groups: bool) -> Result<(), *mut PyObject> {
    if ty.is_null() || unsafe { !is_exception_subclass(ty.cast_const(), runtime.exception_types.base_exception.cast_const()) } {
        return Err(raise_builtin_text(
            runtime,
            ExceptionKind::TypeError,
            "catching classes that do not inherit from BaseException is not allowed",
        ));
    }
    if forbid_groups && unsafe { is_exception_subclass(ty.cast_const(), runtime.exception_types.base_exception_group.cast_const()) } {
        return Err(raise_builtin_text(
            runtime,
            ExceptionKind::TypeError,
            "catching ExceptionGroup with except* is not allowed",
        ));
    }
    Ok(())
}

fn split_condition(types: *mut PyObject, allow_callable: bool, forbid_groups: bool) -> Result<SplitCond, *mut PyObject> {
    if types.is_null() {
        return Err(raise_type_error_text("exception-group split target is null"));
    }
    super::with_runtime(|runtime| {
        if is_type_object(runtime, types) {
            let ty = types.cast::<PyType>();
            validate_exception_type(runtime, ty, forbid_groups)?;
            return Ok(SplitCond::Types(vec![ty]));
        }
        if type_name_is(types, "tuple") || type_name_is(types, "list") {
            let values = match unsafe { crate::types::type_::positional_args_from_object(types) } {
                Ok(values) => values,
                Err(message) => return Err(raise_builtin_text(runtime, ExceptionKind::TypeError, &message)),
            };
            let mut out = Vec::with_capacity(values.len());
            for value in values {
                if !is_type_object(runtime, value) {
                    return Err(raise_builtin_text(
                        runtime,
                        ExceptionKind::TypeError,
                        "catch target must be an exception type",
                    ));
                }
                let ty = value.cast::<PyType>();
                validate_exception_type(runtime, ty, forbid_groups)?;
                out.push(ty);
            }
            return Ok(SplitCond::Types(out));
        }
        if allow_callable {
            Ok(SplitCond::Callable(types))
        } else {
            Err(raise_builtin_text(
                runtime,
                ExceptionKind::TypeError,
                "catch target must be an exception type or tuple of exception types",
            ))
        }
    })
    .unwrap_or_else(|| Err(super::return_null_with_error("runtime is not initialized")))
}

fn condition_matches(cond: &SplitCond, node: *mut PyObject) -> Result<bool, ()> {
    match cond {
        SplitCond::Types(types) => Ok(types
            .iter()
            .copied()
            .any(|ty| unsafe { is_exception_instance(node, ty.cast_const()) })),
        SplitCond::Callable(callable) => {
            let mut argv = [node];
            let result = unsafe { super::pon_call(*callable, argv.as_mut_ptr(), 1) };
            if result.is_null() {
                return Err(());
            }
            let truth = unsafe { super::object::pon_is_true(result) };
            if truth < 0 {
                Err(())
            } else {
                Ok(truth != 0)
            }
        }
    }
}

fn group_members(group: *mut PyObject) -> Result<Vec<*mut PyObject>, ()> {
    let Some(group_ref) = (unsafe { as_exception_group(group) }) else {
        super::return_null_with_error("exception group payload is not a group");
        return Err(());
    };
    if group_ref.exceptions.is_null() {
        super::return_null_with_error("exception group members tuple is null");
        return Err(());
    }
    let tuple = unsafe { &*group_ref.exceptions.cast::<PyTuple>() };
    Ok(unsafe { tuple.as_slice() }.to_vec())
}

fn alloc_group_like(
    runtime: &Runtime,
    source: *mut PyObject,
    members: &[*mut PyObject],
    copy_metadata: bool,
) -> Result<*mut PyObject, ()> {
    let Some(source_group) = (unsafe { as_exception_group(source) }) else {
        super::return_null_with_error("derive source is not an exception group");
        return Err(());
    };
    let ty = source_group.base.ob_base.ob_type.cast_mut();
    let message = source_group.base.message;
    let group = match alloc_exception_group_from_members(runtime, ty, message, members) {
        Ok(group) => group,
        Err(message) => {
            super::return_null_with_error(message);
            return Err(());
        }
    };
    if copy_metadata {
        unsafe {
            let derived = &mut *group.cast::<PyBaseException>();
            derived.cause = source_group.base.cause;
            derived.context = source_group.base.context;
            derived.traceback = source_group.base.traceback;
            derived.suppress_context = source_group.base.suppress_context;
        }
    }
    Ok(group)
}

fn split_exception(runtime: &Runtime, node: *mut PyObject, cond: &SplitCond) -> Result<SplitOutcome, ()> {
    if condition_matches(cond, node)? {
        return Ok(SplitOutcome {
            matched: node,
            rest: ptr::null_mut(),
        });
    }
    if unsafe { !is_exception_group_instance(node) } {
        return Ok(SplitOutcome {
            matched: ptr::null_mut(),
            rest: node,
        });
    }

    let mut matched_parts = Vec::new();
    let mut rest_parts = Vec::new();
    for child in group_members(node)? {
        let split = split_exception(runtime, child, cond)?;
        if !split.matched.is_null() {
            matched_parts.push(split.matched);
        }
        if !split.rest.is_null() {
            rest_parts.push(split.rest);
        }
    }

    let matched = if matched_parts.is_empty() {
        ptr::null_mut()
    } else {
        alloc_group_like(runtime, node, &matched_parts, true)?
    };
    let rest = if rest_parts.is_empty() {
        ptr::null_mut()
    } else {
        alloc_group_like(runtime, node, &rest_parts, true)?
    };
    Ok(SplitOutcome { matched, rest })
}

fn none_or_object(object: *mut PyObject) -> *mut PyObject {
    if object.is_null() {
        unsafe { super::pon_none() }
    } else {
        object
    }
}

fn empty_message(runtime: &Runtime) -> Result<*mut PyObject, String> {
    super::alloc_unicode(runtime, b"")
}

fn wrap_naked_for_exc_star(runtime: &Runtime, exception: *mut PyObject) -> Result<*mut PyObject, String> {
    let message = empty_message(runtime)?;
    let ty = if unsafe { is_exception_instance(exception, runtime.exception_types.exception.cast_const()) } {
        runtime.exception_types.exception_group
    } else {
        runtime.exception_types.base_exception_group
    };
    alloc_exception_group_from_members(runtime, ty, message, &[exception])
}

fn unwrap_exc_star_rest(was_naked: bool, rest: *mut PyObject) -> *mut PyObject {
    if !was_naked || rest.is_null() {
        return rest;
    }
    let Ok(members) = group_members(rest) else {
        return rest;
    };
    if members.len() == 1 {
        members[0]
    } else {
        rest
    }
}

/// Conservatively splits the current exception against `types` (legacy representative flavor).
///
/// Pin J0.6 §4.1/§6.5: this current-exception flavor stays conservative until the
/// `CheckExcStar` lowering is retired — an actual group is returned wholly as the
/// match when the group itself satisfies `types`; otherwise the whole pending
/// exception is returned through `out_rest`. Non-groups never report a fake
/// successful match; the recursive PEP 654 split belongs to `.split()`/`.subgroup()`
/// and `pon_exc_star_match`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_exc_group_split(types: *mut PyObject, out_rest: *mut *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(types);
    super::catch_object_helper(|| {
        if out_rest.is_null() {
            return raise_type_error_text("exception-group split rest pointer is null");
        }
        unsafe { *out_rest = ptr::null_mut() };

        let cond = match split_condition(types, true, false) {
            Ok(cond) => cond,
            Err(result) => return result,
        };
        let current = thread_state_lock().current_exc;
        if current.is_null() {
            return ptr::null_mut();
        }
        if is_diagnostic_sentinel(current) || unsafe { !is_exception_group_instance(current) } {
            unsafe { *out_rest = current };
            return ptr::null_mut();
        }
        match condition_matches(&cond, current) {
            Ok(true) => current,
            Ok(false) => {
                unsafe { *out_rest = current };
                ptr::null_mut()
            }
            Err(()) => ptr::null_mut(),
        }
    })
}

/// Pushes an active exception-handler record and returns `None`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_push_exc_info(target: u32, stack_depth: u32, kind: HandlerKind) -> *mut PyObject {
    super::catch_object_helper(|| {
        let frame = thread_state_lock().current_frame().unwrap_or(ptr::null_mut());
        thread_state_lock().push_handler(HandlerInfo::new(frame, target, stack_depth, kind));
        unsafe { super::pon_none() }
    })
}

/// Pops the innermost active exception-handler record and returns `None`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_pop_exc_info() -> *mut PyObject {
    super::catch_object_helper(|| {
        if thread_state_lock().pop_handler().is_none() {
            return raise_type_error_text("exception handler stack underflow");
        }
        unsafe { super::pon_none() }
    })
}

/// Returns the active exception object when it matches `exc_type`, or `None` on miss.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_match_exc(exc_type: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(exc_type);
    super::catch_object_helper(|| {
        let matched = unsafe { pon_exc_matches(exc_type) };
        if matched < 0 {
            return ptr::null_mut();
        }
        if matched == 0 {
            return unsafe { super::pon_none() };
        }

        let current = thread_state_lock().current_exc;
        if current.is_null() || is_diagnostic_sentinel(current) {
            unsafe { super::pon_none() }
        } else {
            park_handled_exception(current);
            current
        }
    })
}

/// Legacy representative `except*` split; retained for older IR.
///
/// Rides the conservative [`pon_exc_group_split`]: returns the whole active group
/// on a whole-group match, `None` on a miss, and NULL only when the split itself
/// raised. It never touches the `exc_star_stack`, so `CheckExcStar` sites without
/// an `ExcStarEnter` bracket keep working until Pin J0.6 §6.5 retires them.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_check_exc_star(exc_types: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(exc_types);
    super::catch_object_helper(|| {
        let before = thread_state_lock().current_exc;
        let mut rest = ptr::null_mut();
        let matched = unsafe { pon_exc_group_split(exc_types, &mut rest) };
        if !matched.is_null() {
            return matched;
        }
        let after = thread_state_lock().current_exc;
        if core::ptr::eq(before, after) {
            unsafe { super::pon_none() }
        } else {
            ptr::null_mut()
        }
    })
}

/// Enter an `except*` dispatcher for the pending exception.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_exc_star_enter() -> *mut PyObject {
    super::catch_object_helper(|| {
        let current = thread_state_lock().current_exc;
        if current.is_null() || is_diagnostic_sentinel(current) {
            return raise_type_error_text("except* on a non-object exception");
        }
        thread_state_lock().exc_star_stack.push(ExcStarFrame::new(current));
        unsafe { super::pon_none() }
    })
}

fn exc_star_split_current(runtime: &Runtime, rest: *mut PyObject, cond: &SplitCond) -> Result<SplitOutcome, ()> {
    if rest.is_null() {
        return Ok(SplitOutcome {
            matched: ptr::null_mut(),
            rest: ptr::null_mut(),
        });
    }
    let was_naked = unsafe { !is_exception_group_instance(rest) };
    let subject = if was_naked {
        match wrap_naked_for_exc_star(runtime, rest) {
            Ok(group) => group,
            Err(message) => {
                super::return_null_with_error(message);
                return Err(());
            }
        }
    } else {
        rest
    };
    let mut split = split_exception(runtime, subject, cond)?;
    split.rest = unwrap_exc_star_rest(was_naked, split.rest);
    Ok(split)
}

/// Split the active `except*` frame remainder against one clause type expression.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_exc_star_match(exc_types: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(exc_types);
    super::catch_object_helper(|| {
        let cond = match split_condition(exc_types, false, true) {
            Ok(cond) => cond,
            Err(result) => {
                thread_state_lock().exc_star_stack.pop();
                return result;
            }
        };
        let rest = match thread_state_lock().exc_star_stack.last().map(|frame| frame.rest) {
            Some(rest) => rest,
            None => return raise_type_error_text("except* stack underflow"),
        };
        let split = match super::with_runtime(|runtime| exc_star_split_current(runtime, rest, &cond)) {
            Some(Ok(split)) => split,
            Some(Err(())) => return ptr::null_mut(),
            None => return super::return_null_with_error("runtime is not initialized"),
        };
        if split.matched.is_null() {
            return unsafe { super::pon_none() };
        }
        {
            let mut state = thread_state_lock();
            let Some(frame) = state.exc_star_stack.last_mut() else {
                return raise_type_error_text("except* stack underflow");
            };
            frame.rest = split.rest;
        }
        match super::with_runtime(|runtime| set_current_exception(runtime, split.matched)) {
            Some(()) => {
                park_handled_exception(split.matched);
                split.matched
            }
            None => super::return_null_with_error("runtime is not initialized"),
        }
    })
}

/// Mark an `except*` clause body as completed normally.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_exc_star_body_ok() -> *mut PyObject {
    super::catch_object_helper(|| {
        let original = match thread_state_lock().exc_star_stack.last().map(|frame| frame.original) {
            Some(original) => original,
            None => return raise_type_error_text("except* stack underflow"),
        };
        match super::with_runtime(|runtime| set_current_exception(runtime, original)) {
            Some(()) => unsafe { super::pon_none() },
            None => super::return_null_with_error("runtime is not initialized"),
        }
    })
}

/// Collect a new exception raised by an `except*` clause body.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_exc_star_body_raised() -> *mut PyObject {
    super::catch_object_helper(|| {
        let raised = thread_state_lock().current_exc;
        let original = {
            let mut state = thread_state_lock();
            let Some(frame) = state.exc_star_stack.last_mut() else {
                return raise_type_error_text("except* stack underflow");
            };
            if !raised.is_null() && !is_diagnostic_sentinel(raised) {
                frame.raised.push(raised);
            }
            frame.original
        };
        match super::with_runtime(|runtime| set_current_exception(runtime, original)) {
            Some(()) => unsafe { super::pon_none() },
            None => super::return_null_with_error("runtime is not initialized"),
        }
    })
}

fn finish_raised_group(runtime: &Runtime, frame: &ExcStarFrame) -> Result<*mut PyObject, String> {
    let mut members = frame.raised.clone();
    if !frame.rest.is_null() {
        members.push(frame.rest);
    }
    let message = empty_message(runtime)?;
    let ty = if members
        .iter()
        .copied()
        .all(|member| unsafe { is_exception_instance(member, runtime.exception_types.exception.cast_const()) })
    {
        runtime.exception_types.exception_group
    } else {
        runtime.exception_types.base_exception_group
    };
    let group = alloc_exception_group_from_members(runtime, ty, message, &members)?;
    unsafe {
        (*group.cast::<PyBaseException>()).context = frame.original;
    }
    Ok(group)
}

/// Pop an `except*` frame and install the recomposed remainder/raised exception.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_exc_star_finish() -> *mut PyObject {
    super::catch_object_helper(|| {
        let frame = match thread_state_lock().exc_star_stack.pop() {
            Some(frame) => frame,
            None => return raise_type_error_text("except* stack underflow"),
        };
        if frame.raised.is_empty() && frame.rest.is_null() {
            pon_err_clear();
            return unsafe { super::pon_none() };
        }
        let reraised = if frame.raised.is_empty() {
            frame.rest
        } else if frame.raised.len() == 1 && frame.rest.is_null() && unsafe { !is_exception_group_instance(frame.original) } {
            frame.raised[0]
        } else {
            match super::with_runtime(|runtime| finish_raised_group(runtime, &frame)) {
                Some(Ok(group)) => group,
                Some(Err(message)) => return super::return_null_with_error(message),
                None => return super::return_null_with_error("runtime is not initialized"),
            }
        };
        match super::with_runtime(|runtime| set_current_exception(runtime, reraised)) {
            Some(()) => ptr::null_mut(),
            None => super::return_null_with_error("runtime is not initialized"),
        }
    })
}

/// Returns the current object-safe exception, or `None` when there is none.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_get_current_exc() -> *mut PyObject {
    super::catch_object_helper(|| {
        let current = thread_state_lock().current_exc;
        if current.is_null() || is_diagnostic_sentinel(current) {
            unsafe { super::pon_none() }
        } else {
            park_handled_exception(current);
            current
        }
    })
}

/// Parks `exception` as the thread's handled exception — the
/// `sys.exception()` / `sys.exc_info()` source (CPython
/// `exc_info->exc_value`).
///
/// Handler-entry helpers ([`pon_match_exc`], [`pon_get_current_exc`],
/// [`pon_exc_star_match`]) call this BEFORE the handler body's first call
/// boundary clears the pending slot; `crate::abi::HandledExcGuard` then
/// saves/restores the parked value around every compiled-code call so it
/// scopes to the catching frame.
fn park_handled_exception(exception: *mut PyObject) {
    thread_state_lock().handled_exc = exception;
}

pub(super) fn build_exception_group_checked(runtime: &Runtime, cls: *mut PyType, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() != 2 {
        return raise_builtin_text(runtime, ExceptionKind::TypeError, "BaseExceptionGroup() takes exactly 2 arguments");
    }
    let message = args[0];
    if message.is_null() || unsafe { !is_exact_type(message, runtime.unicode_type.cast_const()) } {
        return raise_builtin_text(runtime, ExceptionKind::TypeError, "BaseExceptionGroup() argument 1 must be str");
    }
    let values = match unsafe { crate::types::type_::positional_args_from_object(args[1]) } {
        Ok(values) => values,
        Err(error) => return raise_builtin_text(runtime, ExceptionKind::TypeError, &error),
    };
    if values.is_empty() {
        return raise_builtin_text(
            runtime,
            ExceptionKind::ValueError,
            "second argument (exceptions) must be a non-empty sequence",
        );
    }
    for value in values.iter().copied() {
        if value.is_null() || unsafe { !is_exception_instance(value, runtime.exception_types.base_exception.cast_const()) } {
            return raise_builtin_text(runtime, ExceptionKind::TypeError, "second argument (exceptions) must contain only exceptions");
        }
    }
    let all_exception = values
        .iter()
        .copied()
        .all(|value| unsafe { is_exception_instance(value, runtime.exception_types.exception.cast_const()) });
    let ty = if cls == runtime.exception_types.base_exception_group && all_exception {
        runtime.exception_types.exception_group
    } else {
        cls
    };
    if cls == runtime.exception_types.exception_group && !all_exception {
        return raise_builtin_text(runtime, ExceptionKind::TypeError, "Cannot nest BaseExceptions in an ExceptionGroup");
    }
    match alloc_exception_group_from_members(runtime, ty, message, &values) {
        Ok(group) => group,
        Err(message) => super::return_null_with_error(message),
    }
}

pub unsafe fn call_exception_group_method(receiver: *mut PyObject, kind: u8, args: *mut PyObject) -> *mut PyObject {
    let positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
        Ok(args) => args,
        Err(message) => return super::return_null_with_error(message),
    };
    if positional.len() != 1 {
        return raise_type_error_text("ExceptionGroup method expected exactly one argument");
    }
    match kind {
        EXC_GROUP_METHOD_SPLIT | EXC_GROUP_METHOD_SUBGROUP => {
            let cond = match split_condition(positional[0], true, false) {
                Ok(cond) => cond,
                Err(result) => return result,
            };
            let split = match super::with_runtime(|runtime| split_exception(runtime, receiver, &cond)) {
                Some(Ok(split)) => split,
                Some(Err(())) => return ptr::null_mut(),
                None => return super::return_null_with_error("runtime is not initialized"),
            };
            if kind == EXC_GROUP_METHOD_SUBGROUP {
                none_or_object(split.matched)
            } else {
                let values = [none_or_object(split.matched), none_or_object(split.rest)];
                match super::with_runtime(|runtime| super::seq::alloc_tuple_from_slice(runtime, &values)) {
                    Some(Ok(tuple)) => tuple,
                    Some(Err(message)) => super::return_null_with_error(message),
                    None => super::return_null_with_error("runtime is not initialized"),
                }
            }
        }
        EXC_GROUP_METHOD_DERIVE => {
            let values = match super::seq::sequence_to_vec(positional[0]) {
                Ok(values) => values,
                Err(message) => return super::return_null_with_error(message),
            };
            if values.is_empty() {
                return raise_type_error_text("second argument (exceptions) must be a non-empty sequence");
            }
            match super::with_runtime(|runtime| alloc_group_like(runtime, receiver, &values, false)) {
                Some(Ok(group)) => group,
                Some(Err(())) => ptr::null_mut(),
                None => super::return_null_with_error("runtime is not initialized"),
            }
        }
        _ => raise_type_error_text("unknown ExceptionGroup method"),
    }
}

/// Builds an `ExceptionGroup` from boxed exception values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_build_exc_group(excs: *mut *mut PyObject, len: usize) -> *mut PyObject {
    super::catch_object_helper(|| {
        if len == 0 {
            return raise_type_error_text("ExceptionGroup requires at least one exception");
        }
        if excs.is_null() {
            return raise_type_error_text("ExceptionGroup exception array is null");
        }
        match super::with_runtime(|runtime| {
            let values = unsafe { core::slice::from_raw_parts(excs, len) };
            let message = match super::alloc_unicode(runtime, b"exception group") {
                Ok(message) => message,
                Err(message) => return super::return_null_with_error(message),
            };
            build_exception_group_checked(
                runtime,
                runtime.exception_types.exception_group,
                &[message, match super::seq::alloc_tuple_from_slice(runtime, values) {
                    Ok(tuple) => tuple,
                    Err(message) => return super::return_null_with_error(message),
                }],
            )
        }) {
            Some(group) => group,
            None => super::return_null_with_error("runtime is not initialized"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::intern;
    use crate::thread_state::{pon_err_clear, pon_err_message, pon_err_occurred, pon_err_set, test_state_lock};

    fn reset_exception_state() {
        pon_err_clear();
        thread_state_lock().exception_state_stack.clear();
        thread_state_lock().handler_chain.clear();
        thread_state_lock().frame_stack.clear();
        thread_state_lock().exc_star_stack.clear();
    }

    fn exception_types() -> crate::types::exc::ExceptionTypeSet {
        super::ensure_runtime_for_exc().unwrap();
        super::super::with_runtime(|runtime| runtime.exception_types).unwrap()
    }

    #[test]
    fn type_derives_base_exception_runs_while_runtime_mutex_is_held() {
        let _guard = test_state_lock();
        reset_exception_state();
        super::ensure_runtime_for_exc().unwrap();

        let derives_base = super::super::with_runtime(|runtime| {
            type_derives_base_exception(runtime.exception_types.value_error.cast_const())
        })
        .unwrap();

        assert!(derives_base);
        reset_exception_state();
    }

    #[test]
    fn pending_exception_object_reports_message_only_diagnostics_as_none() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            ensure_runtime_for_exc().unwrap();
            assert_eq!(pending_exception_object(), None, "clear state has no pending object");

            pon_err_set("message-only diagnostic");
            assert!(pon_err_occurred(), "diagnostic must be pending");
            assert_eq!(
                pending_exception_object(),
                None,
                "dangling sentinel is not a dereferenceable exception object",
            );

            reset_exception_state();
            assert!(pon_raise_value_error(b"real".as_ptr(), 4).is_null());
            let current = thread_state_lock().current_exc;
            assert_eq!(pending_exception_object(), Some(current));
            reset_exception_state();
        }
    }

    #[test]
    fn prefixed_diagnostic_text_installs_catchable_exception() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            let types = exception_types();

            assert!(raise_prefixed_diagnostic_text("FileNotFoundError: [Errno 2] No such file or directory"));
            let current = thread_state_lock().current_exc;
            assert!(!current.is_null());
            assert!(!is_diagnostic_sentinel(current));
            assert_eq!(pon_exc_matches(types.file_not_found_error.cast::<PyObject>()), 1);
            assert_eq!(pon_exc_matches(types.os_error.cast::<PyObject>()), 1);
            assert_eq!(
                pon_err_message().as_deref(),
                Some("FileNotFoundError: [Errno 2] No such file or directory"),
            );
            reset_exception_state();
        }
    }

    #[test]
    fn pon_raise_preserves_message_only_sentinel() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            ensure_runtime_for_exc().unwrap();
            pon_err_set("legacy diagnostic");
            let sentinel = thread_state_lock().current_exc;
            assert!(is_diagnostic_sentinel(sentinel));

            assert!(pon_raise(sentinel, ptr::null_mut()).is_null());
            assert_eq!(thread_state_lock().current_exc, sentinel);
            assert_eq!(pon_err_message().as_deref(), Some("legacy diagnostic"));
            reset_exception_state();
        }
    }

    #[test]
    fn pon_raise_matches_every_core_exception_type() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            let types = exception_types();
            for (kind, ty) in types.core_types() {
                reset_exception_state();
                assert!(pon_raise(ty.cast::<PyObject>(), ptr::null_mut()).is_null(), "{kind:?}");
                assert_eq!(pon_exc_matches(ty.cast::<PyObject>()), 1, "{kind:?}");
                assert_eq!(pon_exc_matches(types.base_exception.cast::<PyObject>()), 1, "{kind:?}");
            }
            reset_exception_state();
        }
    }

    #[test]
    fn concrete_raise_helpers_install_expected_types() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            let types = exception_types();
            assert!(pon_raise_type_error(b"bad type".as_ptr(), 8).is_null());
            assert_eq!(pon_exc_matches(types.type_error.cast::<PyObject>()), 1);
            reset_exception_state();

            assert!(pon_raise_value_error(b"bad value".as_ptr(), 9).is_null());
            assert_eq!(pon_exc_matches(types.value_error.cast::<PyObject>()), 1);
            reset_exception_state();

            assert!(pon_raise_index_error(b"bad index".as_ptr(), 9).is_null());
            assert_eq!(pon_exc_matches(types.index_error.cast::<PyObject>()), 1);
            reset_exception_state();

            let key = super::super::pon_const_str(b"missing".as_ptr(), 7);
            assert!(pon_raise_key_error(key).is_null());
            assert_eq!(pon_exc_matches(types.key_error.cast::<PyObject>()), 1);
            reset_exception_state();

            let obj = super::super::pon_none();
            assert!(pon_raise_attribute_error(obj, intern("field")).is_null());
            assert_eq!(pon_exc_matches(types.attribute_error.cast::<PyObject>()), 1);
            reset_exception_state();

            assert!(pon_raise_stop_iteration(obj).is_null());
            assert_eq!(pon_exc_matches(types.stop_iteration.cast::<PyObject>()), 1);
            reset_exception_state();
        }
    }

    #[test]
    fn fetch_restore_round_trips_through_exception_state_stack() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            let types = exception_types();
            assert!(pon_raise_value_error(b"round trip".as_ptr(), 10).is_null());
            assert_eq!(pon_exc_matches(types.value_error.cast::<PyObject>()), 1);

            let saved = pon_exc_fetch();
            assert!(!saved.is_null());
            assert!(!pon_err_occurred());
            assert_eq!(thread_state_lock().exception_states(), &[saved]);

            assert_eq!(pon_exc_restore(saved), 0);
            assert!(pon_err_occurred());
            assert_eq!(thread_state_lock().current_exc, saved);
            assert!(thread_state_lock().exception_states().is_empty());
            reset_exception_state();
        }
    }

    #[test]
    fn group_split_does_not_match_plain_exceptions_as_groups() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            let types = exception_types();
            assert!(pon_raise_value_error(b"plain".as_ptr(), 5).is_null());
            let current = thread_state_lock().current_exc;
            let mut rest = ptr::null_mut();

            let matched = pon_exc_group_split(types.value_error.cast::<PyObject>(), &mut rest);

            assert!(matched.is_null());
            assert_eq!(rest, current);
            assert_eq!(thread_state_lock().current_exc, current);
            reset_exception_state();
        }
    }

    #[test]
    fn raise_from_sets_explicit_cause() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            let types = exception_types();
            assert!(pon_raise_value_error(b"cause".as_ptr(), 5).is_null());
            let cause = thread_state_lock().current_exc;
            assert!(!cause.is_null());

            assert!(pon_raise(types.value_error.cast::<PyObject>(), cause).is_null());
            let raised = thread_state_lock().current_exc;
            assert!(!raised.is_null());
            assert_eq!((*raised.cast::<PyBaseException>()).cause, cause);
            reset_exception_state();
        }
    }

    #[test]
    fn object_safe_match_and_current_exception_helpers_return_none_on_miss() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            let types = exception_types();
            let none = super::super::pon_none();

            assert_eq!(pon_get_current_exc(), none);
            assert!(pon_raise_value_error(b"value".as_ptr(), 5).is_null());
            let current = thread_state_lock().current_exc;
            assert_eq!(pon_match_exc(types.value_error.cast::<PyObject>()), current);
            assert_eq!(pon_match_exc(types.type_error.cast::<PyObject>()), none);
            reset_exception_state();
        }
    }


    #[test]
    fn representative_exception_group_matches_except_star_type() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            let types = exception_types();
            assert!(pon_raise_value_error(b"member".as_ptr(), 6).is_null());
            let member = thread_state_lock().current_exc;
            pon_err_clear();
            let mut members = [member];
            let group = pon_build_exc_group(members.as_mut_ptr(), members.len());
            assert!(!group.is_null());

            assert!(pon_raise(group, ptr::null_mut()).is_null());
            assert_eq!(pon_check_exc_star(types.exception_group.cast::<PyObject>()), group);
            assert_eq!(pon_check_exc_star(types.value_error.cast::<PyObject>()), super::super::pon_none());
            reset_exception_state();
        }
    }
    #[test]
    fn push_pop_exc_info_round_trips_handler_chain() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            assert!(!pon_push_exc_info(42, 7, 3).is_null());
            let handlers = thread_state_lock().handlers().to_vec();
            assert_eq!(handlers.len(), 1);
            assert_eq!(handlers[0], HandlerInfo::new(ptr::null_mut(), 42, 7, 3));

            assert!(!pon_pop_exc_info().is_null());
            assert!(thread_state_lock().handlers().is_empty());
            reset_exception_state();
        }
    }

    fn traceback_chain_len(exception: *mut PyObject) -> usize {
        let mut length = 0;
        let mut entry = unsafe { (*exception.cast::<PyBaseException>()).traceback };
        while !entry.is_null() {
            length += 1;
            entry = unsafe { (*entry.cast::<crate::traceback::PyTraceback>()).tb_next };
        }
        length
    }

    fn traceback_linenos(exception: *mut PyObject) -> Vec<i64> {
        let mut linenos = Vec::new();
        let mut entry = unsafe { (*exception.cast::<PyBaseException>()).traceback };
        while !entry.is_null() {
            let node = unsafe { &*entry.cast::<crate::traceback::PyTraceback>() };
            linenos.push(node.lineno);
            entry = node.tb_next;
        }
        linenos
    }

    #[test]
    fn raising_attaches_module_level_traceback_chain() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            super::super::set_current_line(7);

            assert!(pon_raise_value_error(b"with frame".as_ptr(), 10).is_null());

            let exception = thread_state_lock().current_exc;
            let head = (*exception.cast::<PyBaseException>()).traceback;
            assert!(!head.is_null());
            let entry = &*head.cast::<crate::traceback::PyTraceback>();
            assert!(!entry.frame.is_null());
            assert_eq!(entry.lineno, 7, "module-level raise reads the live line cell");
            assert_eq!((*entry.frame.cast::<PyFrame>()).line, 7, "synthesized frame mirrors the entry line");
            assert!(entry.tb_next.is_null());
            super::super::set_current_line(0);
            reset_exception_state();
        }
    }

    #[test]
    fn raising_snapshots_one_entry_per_active_python_call() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            let function = core::ptr::NonNull::<crate::object::PyFunction>::dangling().as_ptr();
            super::super::set_current_line(2);
            let outer = super::super::push_current_call(function, ptr::null_mut(), 0);
            super::super::set_current_line(4);
            let inner = super::super::push_current_call(function, ptr::null_mut(), 0);
            super::super::set_current_line(6);

            assert!(pon_raise_value_error(b"deep".as_ptr(), 4).is_null());

            let exception = thread_state_lock().current_exc;
            assert_eq!(traceback_chain_len(exception), 3);
            // Head is the outermost (module) frame at its call statement; the
            // raise-site entry carries the live cell value.
            assert_eq!(traceback_linenos(exception), vec![2, 4, 6]);

            drop(inner);
            assert_eq!(super::super::current_line(), 4, "popping a call restores its caller's line");
            drop(outer);
            assert_eq!(super::super::current_line(), 2);

            super::super::set_current_line(0);
            reset_exception_state();
        }
    }

    #[test]
    fn raising_again_appends_a_traceback_segment() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            super::super::set_current_line(6);
            assert!(pon_raise_value_error(b"origin".as_ptr(), 6).is_null());
            let exception = thread_state_lock().current_exc;
            assert_eq!(traceback_chain_len(exception), 1);

            pon_err_clear();
            super::super::set_current_line(9);
            assert!(pon_raise(exception, ptr::null_mut()).is_null());
            assert_eq!(thread_state_lock().current_exc, exception);
            assert_eq!(traceback_chain_len(exception), 2);
            // The re-raise entry is the new head; the original stays deeper.
            assert_eq!(traceback_linenos(exception), vec![9, 6]);
            super::super::set_current_line(0);
            reset_exception_state();
        }
    }
}
