//! Native `_thread` module: the surface the vendored 3.14 `threading.py`
//! binds at import plus the original free-threading hooks.
//!
//! Real OS threads exist only under the `free-threading` feature (the GC and
//! type machinery require registered threads).  Default builds run
//! `start_joinable_thread` bodies inline on the calling thread under a
//! synthetic `get_ident()` override, which keeps `threading.Thread`
//! start/join, `current_thread()` bookkeeping, and `RLock` owner accounting
//! coherent and deterministic for single-threaded conformance runs.
//!
//! Objects are immortal leaked boxes like the other native seeds; the Python
//! values held by `_local` instances are reported as GC roots through
//! [`gc_held_roots`] (the `_contextvars` pattern).

use std::cell::Cell;
use std::collections::HashMap;
use std::ptr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};

use num_traits::ToPrimitive;

use crate::abi::exc::pon_raise_attribute_error;
use crate::abi::number::{pon_const_bool, pon_const_float};
use crate::abi::{
    self, pon_call, pon_const_int, pon_is_true, pon_load_global, pon_make_function, pon_none, pon_thread_start_new,
};
use crate::intern::intern;
use crate::native::builtins_mod::VARIADIC_ARITY;
use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::thread_state::pon_err_set;
use crate::types::{method, type_};

use super::install_module;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

/// `_thread.TIMEOUT_MAX` (CPython 3.14 darwin: `PY_TIMEOUT_MAX` microseconds
/// as seconds).
const TIMEOUT_MAX: f64 = 9_223_372_036.0;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    // Pin the main-thread ident while make_module still runs on it (the
    // `_thread` seed is in `EAGER_MODULES`, installed during runtime init).
    let _ = *MAIN_THREAD_IDENT;
    let error_type = {
        let value = unsafe { pon_load_global(intern("RuntimeError"), ptr::null_mut()) };
        if value.is_null() {
            return Err("RuntimeError is not registered for _thread.error".to_string());
        }
        value
    };
    let timeout_max = unsafe { pon_const_float(TIMEOUT_MAX) };
    if timeout_max.is_null() {
        return Err("failed to allocate _thread.TIMEOUT_MAX".to_string());
    }
    install_module(
        "_thread",
        vec![
            (intern("start_new_thread"), module_function("start_new_thread", native_start_new_thread)?),
            (
                intern("start_joinable_thread"),
                module_function("start_joinable_thread", native_start_joinable_thread)?,
            ),
            (intern("daemon_threads_allowed"), module_function("daemon_threads_allowed", native_daemon_threads_allowed)?),
            (intern("allocate_lock"), module_function("allocate_lock", native_allocate_lock)?),
            (intern("get_ident"), module_function("get_ident", native_get_ident)?),
            (intern("_get_main_thread_ident"), module_function("_get_main_thread_ident", native_get_main_thread_ident)?),
            (intern("_is_main_interpreter"), module_function("_is_main_interpreter", native_is_main_interpreter)?),
            (intern("_shutdown"), module_function("_shutdown", native_shutdown)?),
            (intern("_make_thread_handle"), module_function("_make_thread_handle", native_make_thread_handle)?),
            (intern("stack_size"), module_function("stack_size", native_stack_size)?),
            (intern("RLock"), module_function("RLock", native_rlock_new)?),
            (intern("LockType"), lock_type().cast::<PyObject>()),
            (intern("_ThreadHandle"), thread_handle_type().cast::<PyObject>()),
            (intern("_local"), local_type().cast::<PyObject>()),
            (intern("error"), error_type),
            (intern("TIMEOUT_MAX"), timeout_max),
        ],
    )
}

fn module_function(name: &str, entry: BuiltinFn) -> Result<*mut PyObject, String> {
    // SAFETY: `entry` is a live builtin entry with the runtime calling convention.
    let function = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    if function.is_null() {
        return Err(format!("failed to allocate _thread.{name}"));
    }
    Ok(function)
}

/// Base type for the native seeds below (`object`), wiring the generic
/// keyword call path for types with a custom `tp_new`.
fn runtime_object_type() -> *mut PyType {
    abi::runtime_global(intern("object")).map_or(ptr::null_mut(), |object| object.cast::<PyType>())
}

/// Builds a bound native method for a `tp_getattro` hit.
fn bound_method(receiver: *mut PyObject, name: &str, entry: BuiltinFn) -> *mut PyObject {
    // SAFETY: `entry` is a live builtin entry with the runtime calling convention.
    let function = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match method::new_bound_method(function, receiver) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => {
            pon_err_set(message);
            ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Idents

/// CPython `_thread.get_ident()`: a nonzero integer unique per live thread.
unsafe extern "C" fn native_get_ident(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    unsafe { pon_const_int(current_ident()) }
}

thread_local! {
    /// Nonzero while an inline joinable-thread body runs on this OS thread
    /// (single-threaded builds); the synthetic value stands in for the ident
    /// the body would observe on its own OS thread.
    static IDENT_OVERRIDE: Cell<i64> = const { Cell::new(0) };
}

/// Synthetic idents for inline joinable threads.  The base is far below any
/// mapped address on supported targets, so values never collide with the
/// anchor-address idents of real threads.
static SYNTHETIC_IDENT: AtomicI64 = AtomicI64::new(0x7061_0001);

/// Nonzero integer unique per live thread: the address of a thread-local
/// cell, stable for the thread's lifetime (shared by `get_ident` and the
/// `RLock` owner bookkeeping), unless an inline joinable-thread override is
/// active.
fn current_ident() -> i64 {
    let overridden = IDENT_OVERRIDE.with(Cell::get);
    if overridden != 0 {
        return overridden;
    }
    thread_local! {
        static IDENT_ANCHOR: u8 = const { 0 };
    }
    IDENT_ANCHOR.with(|slot| core::ptr::from_ref(slot) as i64)
}

/// The ident of the thread that installed the native modules, pinned by
/// `make_module` before any Python code runs.
static MAIN_THREAD_IDENT: LazyLock<i64> = LazyLock::new(current_ident);

unsafe extern "C" fn native_get_main_thread_ident(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    unsafe { pon_const_int(*MAIN_THREAD_IDENT) }
}

unsafe extern "C" fn native_is_main_interpreter(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    unsafe { pon_const_bool(1) }
}

/// `_thread.daemon_threads_allowed()`: always true in the main interpreter.
unsafe extern "C" fn native_daemon_threads_allowed(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    unsafe { pon_const_bool(1) }
}

/// `_thread._shutdown()`: joining of straggler non-daemon threads at exit.
/// Inline threads finish before `start()` returns and real threads are only
/// spawned under free-threading where process exit does not wait, so there
/// is nothing to join.
unsafe extern "C" fn native_shutdown(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    unsafe { pon_none() }
}

/// `_thread.stack_size([size])`: pon never adjusts stack sizes; 0 means
/// "platform default" like CPython.
unsafe extern "C" fn native_stack_size(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc > 1 {
        pon_err_set(format!("stack_size() takes at most 1 argument ({argc} given)"));
        return ptr::null_mut();
    }
    unsafe { pon_const_int(0) }
}

// ---------------------------------------------------------------------------
// start_new_thread (free-threading stress surface)

unsafe extern "C" fn native_start_new_thread(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc < 2 || argv.is_null() {
        pon_err_set("_thread.start_new_thread requires a callable and args tuple");
        return ptr::null_mut();
    }
    if argc > 2 {
        pon_err_set("_thread.start_new_thread kwargs are not supported");
        return ptr::null_mut();
    }
    // SAFETY: The call helper supplies `argv` with at least `argc` entries.
    let callable = unsafe { *argv };
    let args = unsafe { *argv.add(1) };
    let args = match crate::abi::seq::sequence_to_vec(args) {
        Ok(args) => args,
        Err(message) => {
            pon_err_set(message);
            return ptr::null_mut();
        }
    };
    let call = Box::new(ThreadCall { callable, args });
    let call_arg = Box::into_raw(call).cast::<PyObject>();
    let status = unsafe { pon_thread_start_new(start_new_trampoline as *const u8, call_arg) };
    if status != 0 {
        unsafe { drop(Box::from_raw(call_arg.cast::<ThreadCall>())) };
        return ptr::null_mut();
    }
    unsafe { pon_const_int(1) }
}

struct ThreadCall {
    callable: *mut PyObject,
    args: Vec<*mut PyObject>,
}

unsafe extern "C" fn start_new_trampoline(call: *mut PyObject) -> *mut PyObject {
    if call.is_null() {
        pon_err_set("_thread.start_new_thread call record is null");
        return ptr::null_mut();
    }
    let mut call = unsafe { Box::from_raw(call.cast::<ThreadCall>()) };
    let argc = call.args.len();
    let argv = if argc == 0 { ptr::null_mut() } else { call.args.as_mut_ptr() };
    unsafe { pon_call(call.callable, argv, argc) }
}

// ---------------------------------------------------------------------------
// Thread handles (`_ThreadHandle`, `start_joinable_thread`, `_make_thread_handle`)

#[repr(C)]
struct PyThreadHandle {
    _ob_base: PyObjectHeader,
    state: Arc<HandleState>,
}

struct HandleState {
    inner: Mutex<HandleInner>,
    done: Condvar,
}

struct HandleInner {
    ident: i64,
    done: bool,
}

impl HandleState {
    fn new(ident: i64) -> Self {
        Self {
            inner: Mutex::new(HandleInner { ident, done: false }),
            done: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HandleInner> {
        self.inner.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn ident(&self) -> i64 {
        self.lock().ident
    }

    fn set_ident(&self, ident: i64) {
        self.lock().ident = ident;
    }

    fn is_done(&self) -> bool {
        self.lock().done
    }

    fn set_done(&self) {
        self.lock().done = true;
        self.done.notify_all();
    }

    /// Blocks until done; a timeout returns without error like CPython
    /// (`Thread.join` re-checks `is_alive` afterwards).
    fn join(&self, timeout: Option<f64>) {
        let mut inner = self.lock();
        match timeout {
            None => {
                while !inner.done {
                    inner = self.done.wait(inner).unwrap_or_else(|poison| poison.into_inner());
                }
            }
            Some(seconds) => {
                let deadline = Instant::now() + Duration::from_secs_f64(seconds.max(0.0));
                while !inner.done {
                    let now = Instant::now();
                    if now >= deadline {
                        break;
                    }
                    let (guard, _timed_out) = self
                        .done
                        .wait_timeout(inner, deadline - now)
                        .unwrap_or_else(|poison| poison.into_inner());
                    inner = guard;
                }
            }
        }
    }
}

static THREAD_HANDLE_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(
        abi::runtime_type_type().cast_const(),
        "_ThreadHandle",
        std::mem::size_of::<PyThreadHandle>(),
    );
    ty.tp_base = runtime_object_type();
    ty.tp_new = Some(thread_handle_new);
    ty.tp_getattro = Some(thread_handle_getattro);
    Box::into_raw(Box::new(ty)) as usize
});

fn thread_handle_type() -> *mut PyType {
    *THREAD_HANDLE_TYPE as *mut PyType
}

fn alloc_thread_handle(state: Arc<HandleState>) -> *mut PyObject {
    Box::into_raw(Box::new(PyThreadHandle {
        _ob_base: PyObjectHeader::new(thread_handle_type().cast_const()),
        state,
    }))
    .cast::<PyObject>()
}

/// `_thread._ThreadHandle()`: a fresh not-started handle (`threading.Thread.
/// __init__` allocates one per thread object).
unsafe extern "C" fn thread_handle_new(_cls: *mut PyType, args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
    match unsafe { type_::positional_args_from_object(args) } {
        Ok(positional) if positional.is_empty() => alloc_thread_handle(Arc::new(HandleState::new(0))),
        Ok(positional) => {
            pon_err_set(format!("_ThreadHandle() takes no arguments ({} given)", positional.len()));
            ptr::null_mut()
        }
        Err(message) => {
            pon_err_set(message);
            ptr::null_mut()
        }
    }
}

unsafe extern "C" fn thread_handle_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let name = crate::tag::untag_arg(name);
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        pon_err_set("_ThreadHandle attribute name must be str");
        return ptr::null_mut();
    };
    let entry: BuiltinFn = match name {
        "ident" => {
            let Some(state) = handle_state(object) else {
                pon_err_set("_ThreadHandle receiver is invalid");
                return ptr::null_mut();
            };
            return unsafe { pon_const_int(state.ident()) };
        }
        "join" => handle_join_entry,
        "is_done" => handle_is_done_entry,
        "_set_done" => handle_set_done_entry,
        // SAFETY: Raise helper with the interned attribute name.
        _ => return unsafe { pon_raise_attribute_error(object, intern(name)) },
    };
    bound_method(object, name, entry)
}

fn handle_state(object: *mut PyObject) -> Option<&'static HandleState> {
    if object.is_null() || unsafe { (*object).ob_type } != thread_handle_type().cast_const() {
        return None;
    }
    Some(unsafe { &*(*object.cast::<PyThreadHandle>()).state })
}

fn handle_receiver(argv: *mut *mut PyObject, argc: usize) -> Option<&'static HandleState> {
    if argc == 0 || argv.is_null() {
        pon_err_set("_ThreadHandle method missing receiver");
        return None;
    }
    let state = handle_state(unsafe { *argv });
    if state.is_none() {
        pon_err_set("_ThreadHandle method receiver is not a _ThreadHandle");
    }
    state
}

/// `_ThreadHandle.join(timeout=None)`: blocks until the thread finishes; a
/// numeric timeout bounds the wait and returns None either way.
unsafe extern "C" fn handle_join_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(state) = handle_receiver(argv, argc) else {
        return ptr::null_mut();
    };
    if argc > 2 {
        pon_err_set(format!("join() takes at most 1 argument ({} given)", argc - 1));
        return ptr::null_mut();
    }
    let none = unsafe { pon_none() };
    let mut timeout = None;
    if argc == 2 {
        // SAFETY: The call helper supplies `argv` with `argc` entries.
        let value = crate::tag::untag_arg(unsafe { *argv.add(1) });
        if value.is_null() {
            return ptr::null_mut();
        }
        if value != none {
            let Some(seconds) = object_seconds(value) else {
                pon_err_set("join() timeout must be a number or None");
                return ptr::null_mut();
            };
            timeout = Some(seconds);
        }
    }
    if !state.is_done() && state.ident() == current_ident() {
        pon_err_set("Cannot join current thread");
        return ptr::null_mut();
    }
    state.join(timeout);
    none
}

unsafe extern "C" fn handle_is_done_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(state) = handle_receiver(argv, argc) else {
        return ptr::null_mut();
    };
    unsafe { pon_const_bool(i32::from(state.is_done())) }
}

unsafe extern "C" fn handle_set_done_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(state) = handle_receiver(argv, argc) else {
        return ptr::null_mut();
    };
    state.set_done();
    unsafe { pon_none() }
}

/// `_thread._make_thread_handle(ident)`: a handle for an already-running
/// thread (`_MainThread` and `_DummyThread` bookkeeping).
unsafe extern "C" fn native_make_thread_handle(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 || argv.is_null() {
        pon_err_set(format!("_make_thread_handle() takes exactly 1 argument ({argc} given)"));
        return ptr::null_mut();
    }
    // SAFETY: The call helper supplies `argv` with at least one entry.
    let ident_object = crate::tag::untag_arg(unsafe { *argv });
    if ident_object.is_null() {
        return ptr::null_mut();
    }
    let Some(ident) = (unsafe { crate::types::int::to_bigint_including_bool(ident_object) }).and_then(|value| value.to_i64())
    else {
        pon_err_set("_make_thread_handle() ident must be an int");
        return ptr::null_mut();
    };
    alloc_thread_handle(Arc::new(HandleState::new(ident)))
}

/// Payload for a spawned joinable thread (free-threading builds).
struct JoinableCall {
    callable: *mut PyObject,
    state: Arc<HandleState>,
}

unsafe extern "C" fn joinable_trampoline(call: *mut PyObject) -> *mut PyObject {
    if call.is_null() {
        pon_err_set("joinable thread call record is null");
        return ptr::null_mut();
    }
    let call = unsafe { Box::from_raw(call.cast::<JoinableCall>()) };
    call.state.set_ident(current_ident());
    let result = unsafe { pon_call(call.callable, ptr::null_mut(), 0) };
    // Done is flagged even when the body raised: `Thread._bootstrap` already
    // reported the exception and joiners must not hang.
    call.state.set_done();
    result
}

/// `_thread.start_joinable_thread(function, handle=None, daemon=True)`.
///
/// Free-threading builds spawn a registered OS thread.  Default builds run
/// `function` inline under a synthetic `get_ident()` override (see module
/// docs); the handle is done when the call returns, so `join` never blocks.
unsafe extern "C" fn native_start_joinable_thread(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc == 0 || argv.is_null() {
        pon_err_set("start_joinable_thread() requires a callable");
        return ptr::null_mut();
    }
    if argc > 3 {
        pon_err_set(format!("start_joinable_thread() takes at most 3 arguments ({argc} given)"));
        return ptr::null_mut();
    }
    let none = unsafe { pon_none() };
    // SAFETY: The call helper supplies `argv` with `argc` entries; the
    // keyword binder canonicalizes absent slots to None.
    let callable = unsafe { *argv };
    let mut handle_object = if argc >= 2 { crate::tag::untag_arg(unsafe { *argv.add(1) }) } else { none };
    // Slot 3 (`daemon`) is accepted and ignored: inline threads finish
    // eagerly and free-threading exit never waits on threads.
    if handle_object.is_null() {
        return ptr::null_mut();
    }
    if handle_object == none {
        handle_object = alloc_thread_handle(Arc::new(HandleState::new(0)));
    } else if handle_state(handle_object).is_none() {
        pon_err_set("start_joinable_thread() handle must be a _ThreadHandle");
        return ptr::null_mut();
    }
    if handle_state(handle_object).is_none() {
        return ptr::null_mut();
    }
    // The trampoline and joiners must share the one Arc stored in the
    // handle object.
    let state = unsafe { &(*handle_object.cast::<PyThreadHandle>()).state }.clone();
    if cfg!(feature = "free-threading") {
        let call = Box::new(JoinableCall { callable, state });
        let call_arg = Box::into_raw(call).cast::<PyObject>();
        let status = unsafe { pon_thread_start_new(joinable_trampoline as *const u8, call_arg) };
        if status != 0 {
            unsafe { drop(Box::from_raw(call_arg.cast::<JoinableCall>())) };
            return ptr::null_mut();
        }
        return handle_object;
    }
    let synthetic = SYNTHETIC_IDENT.fetch_add(2, Ordering::Relaxed);
    let previous = IDENT_OVERRIDE.with(|cell| cell.replace(synthetic));
    state.set_ident(synthetic);
    let result = unsafe { pon_call(callable, ptr::null_mut(), 0) };
    IDENT_OVERRIDE.with(|cell| cell.set(previous));
    state.set_done();
    if result.is_null() {
        return ptr::null_mut();
    }
    handle_object
}

/// Coerces a Python number to seconds.
fn object_seconds(value: *mut PyObject) -> Option<f64> {
    if let Some(float) = unsafe { crate::types::float::to_f64(value) } {
        return Some(float);
    }
    unsafe { crate::types::int::to_bigint_including_bool(value) }.and_then(|value| value.to_f64())
}

// ---------------------------------------------------------------------------
// Lock

#[repr(C)]
struct PyLock {
    _ob_base: PyObjectHeader,
    state: Box<LockState>,
}

struct LockState {
    locked: Mutex<bool>,
    available: Condvar,
}

impl LockState {
    fn new() -> Self {
        Self {
            locked: Mutex::new(false),
            available: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, bool> {
        self.locked.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn acquire(&self) {
        let mut locked = self.lock();
        while *locked {
            locked = self.available.wait(locked).unwrap_or_else(|poison| poison.into_inner());
        }
        *locked = true;
    }

    fn try_acquire(&self) -> bool {
        let mut locked = self.lock();
        if *locked {
            false
        } else {
            *locked = true;
            true
        }
    }

    fn acquire_timeout(&self, timeout: f64) -> bool {
        let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
        let mut locked = self.lock();
        while *locked {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (guard, _timed_out) = self
                .available
                .wait_timeout(locked, deadline - now)
                .unwrap_or_else(|poison| poison.into_inner());
            locked = guard;
        }
        *locked = true;
        true
    }

    fn is_locked(&self) -> bool {
        *self.lock()
    }

    fn release(&self) -> Result<(), &'static str> {
        let mut locked = self.lock();
        if !*locked {
            return Err("release unlocked lock");
        }
        *locked = false;
        self.available.notify_one();
        Ok(())
    }
}

static LOCK_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(abi::runtime_type_type().cast_const(), "lock", std::mem::size_of::<PyLock>());
    ty.tp_base = runtime_object_type();
    ty.tp_new = Some(lock_new);
    ty.tp_getattro = Some(lock_getattro);
    Box::into_raw(Box::new(ty)) as usize
});

fn lock_type() -> *mut PyType {
    *LOCK_TYPE as *mut PyType
}

fn alloc_lock() -> *mut PyObject {
    Box::into_raw(Box::new(PyLock {
        _ob_base: PyObjectHeader::new(lock_type().cast_const()),
        state: Box::new(LockState::new()),
    }))
    .cast::<PyObject>()
}

/// `_thread.LockType()` (`threading.Lock`): a fresh unlocked lock.
unsafe extern "C" fn lock_new(_cls: *mut PyType, args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
    match unsafe { type_::positional_args_from_object(args) } {
        Ok(positional) if positional.is_empty() => alloc_lock(),
        Ok(positional) => {
            pon_err_set(format!("lock() takes no arguments ({} given)", positional.len()));
            ptr::null_mut()
        }
        Err(message) => {
            pon_err_set(message);
            ptr::null_mut()
        }
    }
}

unsafe extern "C" fn lock_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let name = crate::tag::untag_arg(name);
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        pon_err_set("lock attribute name must be str");
        return ptr::null_mut();
    };
    let entry: BuiltinFn = match name {
        "acquire" | "__enter__" => lock_acquire_entry,
        "release" => lock_release_entry,
        "__exit__" => lock_exit_entry,
        "locked" => lock_locked_entry,
        // SAFETY: Raise helper with the interned attribute name (Condition's
        // `_release_save`/`_acquire_restore`/`_is_owned` probes must see
        // AttributeError to engage their Python fallbacks).
        _ => return unsafe { pon_raise_attribute_error(object, intern(name)) },
    };
    bound_method(object, name, entry)
}

unsafe extern "C" fn native_allocate_lock(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        pon_err_set(format!("allocate_lock() takes no arguments ({argc} given)"));
        return ptr::null_mut();
    }
    alloc_lock()
}

/// Parsed `acquire(blocking=True, timeout=-1)` shape shared by Lock entries.
enum AcquireMode {
    NonBlocking,
    Blocking,
    Timed(f64),
}

fn parse_acquire_mode(argv: *mut *mut PyObject, argc: usize) -> Result<AcquireMode, ()> {
    if argc > 3 {
        pon_err_set(format!("acquire() takes at most 2 arguments ({} given)", argc - 1));
        return Err(());
    }
    let mut blocking = true;
    if argc >= 2 {
        // SAFETY: The call helper supplies `argv` with `argc` entries;
        // `pon_is_true` self-normalizes its argument.
        match unsafe { pon_is_true(*argv.add(1)) } {
            -1 => return Err(()),
            0 => blocking = false,
            _ => {}
        }
    }
    let mut timeout = None;
    if argc == 3 {
        // SAFETY: As above.
        let value = crate::tag::untag_arg(unsafe { *argv.add(2) });
        if value.is_null() {
            return Err(());
        }
        if value != unsafe { pon_none() } {
            let Some(seconds) = object_seconds(value) else {
                pon_err_set("acquire() timeout must be a number or None");
                return Err(());
            };
            if seconds >= 0.0 {
                timeout = Some(seconds);
            }
        }
    }
    match (blocking, timeout) {
        (false, Some(_)) => {
            pon_err_set("can't specify a timeout for a non-blocking call");
            Err(())
        }
        (false, None) => Ok(AcquireMode::NonBlocking),
        (true, None) => Ok(AcquireMode::Blocking),
        (true, Some(seconds)) => Ok(AcquireMode::Timed(seconds)),
    }
}

/// `lock.acquire(blocking=True, timeout=-1)` / `lock.__enter__()`: returns
/// whether the lock was acquired (`Condition.wait` drives the timed path
/// through its waiter locks).
unsafe extern "C" fn lock_acquire_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(lock) = lock_receiver(argv, argc) else {
        return ptr::null_mut();
    };
    let Ok(mode) = parse_acquire_mode(argv, argc) else {
        return ptr::null_mut();
    };
    let state = &unsafe { &*lock }.state;
    let acquired = match mode {
        AcquireMode::NonBlocking => state.try_acquire(),
        AcquireMode::Blocking => {
            state.acquire();
            true
        }
        AcquireMode::Timed(seconds) => state.acquire_timeout(seconds),
    };
    unsafe { pon_const_bool(i32::from(acquired)) }
}

unsafe extern "C" fn lock_release_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(lock) = lock_receiver(argv, argc) else {
        return ptr::null_mut();
    };
    match unsafe { &*lock }.state.release() {
        Ok(()) => unsafe { pon_none() },
        Err(message) => {
            pon_err_set(message);
            ptr::null_mut()
        }
    }
}

/// `lock.__exit__(exc_type, exc_value, traceback)`: releases and returns
/// None so exceptions propagate.
unsafe extern "C" fn lock_exit_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc == 0 || argv.is_null() {
        pon_err_set("lock.__exit__ missing receiver");
        return ptr::null_mut();
    }
    unsafe { lock_release_entry(argv, 1) }
}

unsafe extern "C" fn lock_locked_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(lock) = lock_receiver(argv, argc) else {
        return ptr::null_mut();
    };
    unsafe { pon_const_bool(i32::from((*lock).state.is_locked())) }
}

fn lock_receiver(argv: *mut *mut PyObject, argc: usize) -> Option<*mut PyLock> {
    if argc == 0 || argv.is_null() {
        pon_err_set("lock method missing receiver");
        return None;
    }
    let receiver = unsafe { *argv };
    if receiver.is_null() || unsafe { (*receiver).ob_type } != lock_type().cast_const() {
        pon_err_set("lock method receiver is not a lock");
        return None;
    }
    Some(receiver.cast::<PyLock>())
}

// ---------------------------------------------------------------------------
// RLock

#[repr(C)]
struct PyRLock {
    _ob_base: PyObjectHeader,
    state: Box<RLockState>,
}

struct RLockState {
    inner: Mutex<RLockInner>,
    available: Condvar,
}

/// `owner` is a `current_ident()` value (never 0 for a live thread); 0 marks
/// the unowned state.
struct RLockInner {
    owner: i64,
    count: usize,
}

impl RLockState {
    fn new() -> Self {
        Self {
            inner: Mutex::new(RLockInner { owner: 0, count: 0 }),
            available: Condvar::new(),
        }
    }

    fn acquire(&self) {
        let me = current_ident();
        let mut inner = self.inner.lock().unwrap_or_else(|poison| poison.into_inner());
        if inner.owner == me {
            inner.count += 1;
            return;
        }
        while inner.count != 0 {
            inner = self.available.wait(inner).unwrap_or_else(|poison| poison.into_inner());
        }
        inner.owner = me;
        inner.count = 1;
    }

    fn release(&self) -> Result<(), &'static str> {
        let me = current_ident();
        let mut inner = self.inner.lock().unwrap_or_else(|poison| poison.into_inner());
        if inner.count == 0 || inner.owner != me {
            return Err("cannot release un-acquired lock");
        }
        inner.count -= 1;
        if inner.count == 0 {
            inner.owner = 0;
            self.available.notify_one();
        }
        Ok(())
    }

    fn is_owned(&self) -> bool {
        let me = current_ident();
        let inner = self.inner.lock().unwrap_or_else(|poison| poison.into_inner());
        inner.count != 0 && inner.owner == me
    }
    fn is_locked(&self) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|poison| poison.into_inner());
        inner.count != 0
    }
}

static RLOCK_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(ptr::null(), "RLock", std::mem::size_of::<PyRLock>());
    ty.tp_getattro = Some(rlock_getattro);
    Box::into_raw(Box::new(ty)) as usize
});

fn rlock_type() -> *mut PyType {
    *RLOCK_TYPE as *mut PyType
}

/// `_thread.RLock()`: allocates a reentrant lock (arguments are rejected like
/// CPython's `RLock` type constructor).
unsafe extern "C" fn native_rlock_new(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        pon_err_set(format!("RLock() takes no arguments ({argc} given)"));
        return ptr::null_mut();
    }
    Box::into_raw(Box::new(PyRLock {
        _ob_base: PyObjectHeader::new(rlock_type().cast_const()),
        state: Box::new(RLockState::new()),
    }))
    .cast::<PyObject>()
}

unsafe extern "C" fn rlock_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let name = crate::tag::untag_arg(name);
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        pon_err_set("RLock attribute name must be str");
        return ptr::null_mut();
    };
    let entry: BuiltinFn = match name {
        "acquire" | "__enter__" => rlock_acquire_entry,
        "release" => rlock_release_entry,
        "__exit__" => rlock_exit_entry,
        "_is_owned" => rlock_is_owned_entry,
        "locked" => rlock_locked_entry,
        // SAFETY: Raise helper with the interned attribute name (Condition's
        // `_release_save`/`_acquire_restore` probes fall back on miss).
        _ => return unsafe { pon_raise_attribute_error(object, intern(name)) },
    };
    bound_method(object, name, entry)
}

/// `RLock.acquire(blocking=True, timeout=-1)` / `RLock.__enter__()`: always
/// blocks until owned (the only mode the embedded stdlib exercises) and
/// returns True.
unsafe extern "C" fn rlock_acquire_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(rlock) = rlock_receiver(argv, argc) else {
        return ptr::null_mut();
    };
    unsafe { &*rlock }.state.acquire();
    unsafe { pon_const_bool(1) }
}

unsafe extern "C" fn rlock_release_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(rlock) = rlock_receiver(argv, argc) else {
        return ptr::null_mut();
    };
    match unsafe { &*rlock }.state.release() {
        Ok(()) => unsafe { pon_none() },
        Err(message) => {
            pon_err_set(message);
            ptr::null_mut()
        }
    }
}

/// `RLock.__exit__(exc_type, exc_value, traceback)`: releases and returns
/// None so exceptions propagate.
unsafe extern "C" fn rlock_exit_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc == 0 || argv.is_null() {
        pon_err_set("RLock.__exit__ missing receiver");
        return ptr::null_mut();
    }
    unsafe { rlock_release_entry(argv, 1) }
}

/// `RLock._is_owned()`: whether the calling thread holds the lock
/// (`threading.Condition` consults it before wait/notify).
unsafe extern "C" fn rlock_is_owned_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(rlock) = rlock_receiver(argv, argc) else {
        return ptr::null_mut();
    };
    unsafe { pon_const_bool(i32::from((*rlock).state.is_owned())) }
}
unsafe extern "C" fn rlock_locked_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(rlock) = rlock_receiver(argv, argc) else {
        return ptr::null_mut();
    };
    unsafe { pon_const_bool(i32::from((*rlock).state.is_locked())) }
}

fn rlock_receiver(argv: *mut *mut PyObject, argc: usize) -> Option<*mut PyRLock> {
    if argc == 0 || argv.is_null() {
        pon_err_set("RLock method missing receiver");
        return None;
    }
    let receiver = unsafe { *argv };
    if receiver.is_null() || unsafe { (*receiver).ob_type } != rlock_type().cast_const() {
        pon_err_set("RLock method receiver is not an RLock");
        return None;
    }
    Some(receiver.cast::<PyRLock>())
}

// ---------------------------------------------------------------------------
// _local

#[repr(C)]
struct PyLocal {
    _ob_base: PyObjectHeader,
    /// Per-thread attribute namespaces: `current_ident()` → interned name →
    /// value address (possibly tagged).
    cells: Mutex<HashMap<i64, HashMap<u32, usize>>>,
}

static LOCAL_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(abi::runtime_type_type().cast_const(), "_local", std::mem::size_of::<PyLocal>());
    ty.tp_base = runtime_object_type();
    ty.tp_new = Some(local_new);
    ty.tp_getattro = Some(local_getattro);
    ty.tp_setattro = Some(local_setattro);
    Box::into_raw(Box::new(ty)) as usize
});

fn local_type() -> *mut PyType {
    *LOCAL_TYPE as *mut PyType
}

/// Every `_local` allocation, for GC root reporting.  Instances are immortal
/// leaked boxes, so the registry only grows.
static LOCAL_REGISTRY: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// GC roots held by `_local` instances (the values stored per thread).
/// Consumed by `crate::abi::collect` while the runtime lock is held, so this
/// must not re-enter the runtime.
pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
    let registry = LOCAL_REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut roots = Vec::new();
    for &addr in registry.iter() {
        // SAFETY: Registry members are live leaked `PyLocal` allocations.
        let cells = unsafe { &(*(addr as *mut PyLocal)).cells };
        let cells = cells.lock().unwrap_or_else(|poison| poison.into_inner());
        for namespace in cells.values() {
            for &value in namespace.values() {
                let value = value as *mut PyObject;
                if !value.is_null() && crate::tag::is_heap(value) {
                    roots.push(value);
                }
            }
        }
    }
    roots
}

/// `_thread._local()`: thread-local attribute namespace (`threading` keeps
/// its dummy-thread tracker in one).  Constructor arguments are accepted and
/// ignored (CPython forwards them to a subclass `__init__`).
unsafe extern "C" fn local_new(_cls: *mut PyType, _args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
    let object = Box::into_raw(Box::new(PyLocal {
        _ob_base: PyObjectHeader::new(local_type().cast_const()),
        cells: Mutex::new(HashMap::new()),
    }))
    .cast::<PyObject>();
    LOCAL_REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner()).push(object as usize);
    object
}

fn local_cells(object: *mut PyObject) -> Option<&'static Mutex<HashMap<i64, HashMap<u32, usize>>>> {
    if object.is_null() || unsafe { (*object).ob_type } != local_type().cast_const() {
        return None;
    }
    Some(unsafe { &(*object.cast::<PyLocal>()).cells })
}

unsafe extern "C" fn local_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let name = crate::tag::untag_arg(name);
    let Some(name_text) = (unsafe { type_::unicode_text(name) }) else {
        pon_err_set("_local attribute name must be str");
        return ptr::null_mut();
    };
    let Some(cells) = local_cells(object) else {
        pon_err_set("_local receiver is invalid");
        return ptr::null_mut();
    };
    let interned = intern(name_text);
    let cells = cells.lock().unwrap_or_else(|poison| poison.into_inner());
    match cells.get(&current_ident()).and_then(|namespace| namespace.get(&interned)) {
        Some(&value) => value as *mut PyObject,
        // SAFETY: Raise helper with the interned attribute name.
        None => unsafe { pon_raise_attribute_error(object, interned) },
    }
}

unsafe extern "C" fn local_setattro(object: *mut PyObject, name: *mut PyObject, value: *mut PyObject) -> core::ffi::c_int {
    let name = crate::tag::untag_arg(name);
    let Some(name_text) = (unsafe { type_::unicode_text(name) }) else {
        pon_err_set("_local attribute name must be str");
        return -1;
    };
    let Some(cells) = local_cells(object) else {
        pon_err_set("_local receiver is invalid");
        return -1;
    };
    let interned = intern(name_text);
    let mut cells = cells.lock().unwrap_or_else(|poison| poison.into_inner());
    let ident = current_ident();
    if value.is_null() {
        // Deletion (`del local.attr`).
        let removed = cells.get_mut(&ident).and_then(|namespace| namespace.remove(&interned));
        if removed.is_none() {
            // SAFETY: Raise helper with the interned attribute name.
            unsafe { pon_raise_attribute_error(object, interned) };
            return -1;
        }
        return 0;
    }
    cells.entry(ident).or_default().insert(interned, value as usize);
    0
}
