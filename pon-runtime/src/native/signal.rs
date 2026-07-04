//! Native `_signal` module (unittest chain: `unittest.signals` -> `signal`).
//!
//! Serves exactly the surface `Lib/signal.py` and `Lib/unittest/signals.py`
//! consume: the host's `SIG*` numbers (via `libc`, mirroring `errno`), the
//! `SIG_DFL`/`SIG_IGN` sentinel ints, `NSIG`, `signal()`/`getsignal()`
//! registration, and `default_int_handler`.
//!
//! DIVERGENCE (documented, deliberate): pon installs NO real OS signal
//! handlers.  The runtime has no safe asynchronous reentry point — a signal
//! can arrive while compiled code holds the thread-state or GC locks, so
//! there is no point at which a Python-level handler could run.  `signal()`
//! is registration bookkeeping only: it validates, swaps the process-level
//! handler table entry, and returns the prior handler with CPython's
//! semantics (including `SIG_DFL`/`SIG_IGN` pass-through), but a registered
//! handler is never invoked by an actual signal.  `default_int_handler`
//! raises `KeyboardInterrupt` when *called* (unittest's `_InterruptHandler`
//! delegates to it explicitly).  CPython's main-thread-only restriction on
//! `signal()` is not enforced, and handler callability is checked
//! permissively (ints other than the two sentinels and `None` are rejected;
//! any other object is accepted) because pon has no generic
//! `PyCallable_Check` equivalent.

use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

use crate::abi::exc::raise_kind_error_no_args;
use crate::abi::{pon_const_int, pon_make_function, return_null_with_error};
use crate::intern::intern;
use crate::object::{PyObject, PyType};
use crate::types::exc::{ExceptionKind, PyBaseException};

use super::install_module;

/// `SIG_DFL` sentinel value (CPython exposes it as the int 0).
const SIG_DFL: i64 = 0;
/// `SIG_IGN` sentinel value (CPython exposes it as the int 1).
const SIG_IGN: i64 = 1;

/// Highest signal number + 1, matching the host CPython's `signal.NSIG`.
#[cfg(target_os = "macos")]
const NSIG: i64 = 32;
#[cfg(not(target_os = "macos"))]
const NSIG: i64 = 65;

/// `SIG*` numbers shared by every supported host, host-correct via `libc`
/// (the `errno` precedent).  `signal.py` turns these module attrs into the
/// `Signals` IntEnum by name filtering, so plain int attrs are the contract.
const CONSTANTS: &[(&str, i32)] = &[
    ("SIGHUP", libc::SIGHUP),
    ("SIGINT", libc::SIGINT),
    ("SIGQUIT", libc::SIGQUIT),
    ("SIGILL", libc::SIGILL),
    ("SIGTRAP", libc::SIGTRAP),
    ("SIGABRT", libc::SIGABRT),
    ("SIGBUS", libc::SIGBUS),
    ("SIGFPE", libc::SIGFPE),
    ("SIGKILL", libc::SIGKILL),
    ("SIGUSR1", libc::SIGUSR1),
    ("SIGSEGV", libc::SIGSEGV),
    ("SIGUSR2", libc::SIGUSR2),
    ("SIGPIPE", libc::SIGPIPE),
    ("SIGALRM", libc::SIGALRM),
    ("SIGTERM", libc::SIGTERM),
    ("SIGCHLD", libc::SIGCHLD),
    ("SIGCONT", libc::SIGCONT),
    ("SIGSTOP", libc::SIGSTOP),
    ("SIGTSTP", libc::SIGTSTP),
    ("SIGTTIN", libc::SIGTTIN),
    ("SIGTTOU", libc::SIGTTOU),
    ("SIGURG", libc::SIGURG),
    ("SIGXCPU", libc::SIGXCPU),
    ("SIGXFSZ", libc::SIGXFSZ),
    ("SIGVTALRM", libc::SIGVTALRM),
    ("SIGPROF", libc::SIGPROF),
    ("SIGWINCH", libc::SIGWINCH),
    ("SIGIO", libc::SIGIO),
    ("SIGSYS", libc::SIGSYS),
];

/// Host-specific extras CPython also exposes.
#[cfg(target_os = "macos")]
const OS_CONSTANTS: &[(&str, i32)] = &[("SIGEMT", libc::SIGEMT), ("SIGINFO", libc::SIGINFO)];
#[cfg(target_os = "linux")]
const OS_CONSTANTS: &[(&str, i32)] = &[
    ("SIGSTKFLT", libc::SIGSTKFLT),
    ("SIGPWR", libc::SIGPWR),
    ("SIGPOLL", libc::SIGPOLL),
];
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const OS_CONSTANTS: &[(&str, i32)] = &[];

const POSIX_CONSTANTS: &[(&str, i64)] = &[
    ("ITIMER_PROF", 2),
    ("ITIMER_REAL", 0),
    ("ITIMER_VIRTUAL", 1),
    ("SIGIOT", libc::SIGABRT as i64),
    ("SIG_BLOCK", 1),
    ("SIG_SETMASK", 3),
    ("SIG_UNBLOCK", 2),
];

static WAKEUP_FD: AtomicI32 = AtomicI32::new(-1);

static ITIMER_ERROR_TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
    let base = crate::import::module_attr(intern("builtins"), intern("OSError"))
        .map_or(std::ptr::null_mut(), |object| object.cast::<PyType>());
    let mut ty = PyType::new(
        crate::abi::runtime_type_type().cast_const(),
        "signal.ItimerError",
        std::mem::size_of::<PyBaseException>(),
    );
    ty.tp_base = base;
    ty.tp_getattro = Some(crate::types::exc::exception_getattro);
    ty.tp_setattro = Some(crate::types::exc::exception_setattro);
    Box::into_raw(Box::new(ty)) as usize
});

fn itimer_error_type() -> *mut PyType {
    *ITIMER_ERROR_TYPE as *mut PyType
}

/// One handler-table slot.  Python handler objects are stored as raw
/// addresses; [`gc_held_roots`] reports them so the collector keeps them
/// alive (the `_contextvars` pattern for native statics).
#[derive(Clone, Copy)]
enum StoredHandler {
    Dfl,
    Ign,
    Object(usize),
}

impl StoredHandler {
    /// Boxes the slot back into the Python value `signal()`/`getsignal()`
    /// return: the sentinel ints or the registered object itself.
    fn to_object(self) -> *mut PyObject {
        match self {
            // SAFETY: Integer boxing helper; NULL propagates to the caller's check.
            Self::Dfl => unsafe { pon_const_int(SIG_DFL) },
            // SAFETY: As above.
            Self::Ign => unsafe { pon_const_int(SIG_IGN) },
            Self::Object(address) => address as *mut PyObject,
        }
    }
}

/// Process-level handler table indexed by signal number (`0..NSIG`), lazily
/// sized on first touch.  Slot 0 is unused (signal numbers start at 1).
static HANDLERS: Mutex<Vec<StoredHandler>> = Mutex::new(Vec::new());

fn handlers_lock() -> std::sync::MutexGuard<'static, Vec<StoredHandler>> {
    let mut table = HANDLERS.lock().unwrap_or_else(|poison| poison.into_inner());
    if table.is_empty() {
        table.resize(NSIG as usize, StoredHandler::Dfl);
    }
    table
}

/// Python handler objects held by the native table, reported as GC roots
/// (mirrors `_contextvars`/`_codecs`/`_collections`).  Locks only this
/// module's mutex and never re-enters the runtime.
pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
    HANDLERS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .iter()
        .filter_map(|slot| match slot {
            StoredHandler::Object(address) => Some(*address as *mut PyObject),
            _ => None,
        })
        .collect()
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "_signal";
    // SAFETY: Runtime allocation helper; NULL is checked below.
    let name_obj = unsafe { crate::abi::pon_const_str(name.as_ptr(), name.len()) };
    if name_obj.is_null() {
        return Err("failed to allocate _signal.__name__".to_owned());
    }
    let mut attrs = vec![(intern("__name__"), name_obj)];
    for &(const_name, value) in CONSTANTS.iter().chain(OS_CONSTANTS) {
        attrs.push(int_attr(const_name, i64::from(value))?);
    }
    for &(const_name, value) in POSIX_CONSTANTS {
        attrs.push(int_attr(const_name, value)?);
    }
    attrs.push(int_attr("SIG_DFL", SIG_DFL)?);
    attrs.push(int_attr("SIG_IGN", SIG_IGN)?);
    attrs.push(int_attr("NSIG", NSIG)?);
    attrs.push((intern("ItimerError"), itimer_error_type().cast::<PyObject>()));
    let default_int_handler = function_attr("default_int_handler", signal_default_int_handler)?;
    // CPython's module init registers `default_int_handler` for SIGINT; the
    // table mirrors that so `getsignal(SIGINT)` observes it at startup.
    handlers_lock()[libc::SIGINT as usize] = StoredHandler::Object(default_int_handler.1 as usize);
    attrs.push(default_int_handler);
    attrs.push(function_attr("signal", signal_signal)?);
    attrs.push(function_attr("getsignal", signal_getsignal)?);
    attrs.push(function_attr("alarm", signal_alarm)?);
    attrs.push(function_attr("getitimer", signal_getitimer)?);
    attrs.push(function_attr("setitimer", signal_setitimer)?);
    attrs.push(function_attr("pause", signal_pause)?);
    attrs.push(function_attr("pthread_kill", signal_pthread_kill)?);
    attrs.push(function_attr("pthread_sigmask", signal_pthread_sigmask)?);
    attrs.push(function_attr("raise_signal", signal_raise_signal)?);
    attrs.push(function_attr("set_wakeup_fd", signal_set_wakeup_fd)?);
    attrs.push(function_attr("siginterrupt", signal_siginterrupt)?);
    attrs.push(function_attr("sigpending", signal_sigpending)?);
    attrs.push(function_attr("sigwait", signal_sigwait)?);
    attrs.push(function_attr("strsignal", signal_strsignal)?);
    attrs.push(function_attr("valid_signals", signal_valid_signals)?);
    install_module(name, attrs)
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { pon_const_int(value) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate _signal.{name}"))
}

fn function_attr(
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Live builtin entry point with the runtime calling convention.
    let object = unsafe { pon_make_function(entry as *const u8, crate::builtins::variadic_arity(), intern(name)) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate _signal.{name}"))
}

/// Reads an `int`-coercible argument (tagged immediate, boxed int, bool, or
/// int subclass such as an IntEnum member) as an `i64`.
unsafe fn arg_as_i64(object: *mut PyObject) -> Option<i64> {
    if crate::tag::is_small_int(object) {
        return Some(crate::tag::untag_small_int(object));
    }
    unsafe { crate::types::int::to_bigint_including_bool(object) }.and_then(|value| num_traits::ToPrimitive::to_i64(&value))
}

/// Best-effort receiver type name for CPython-shaped TypeError messages.
unsafe fn display_type_name(object: *mut PyObject) -> &'static str {
    if object.is_null() {
        return "NoneType";
    }
    if crate::tag::is_small_int(object) {
        return "int";
    }
    // SAFETY: Heap-or-NULL after the immediate test; NULL handled above.
    let ty = unsafe { (*object).ob_type };
    if ty.is_null() {
        return "object";
    }
    // SAFETY: Live type pointer per object-header invariant.
    unsafe { (*ty).name() }
}

/// Validates `signalnum` per CPython: an int in `1..NSIG`.
unsafe fn parse_signalnum(object: *mut PyObject, function: &str) -> Result<usize, *mut PyObject> {
    let Some(signalnum) = (unsafe { arg_as_i64(object) }) else {
        let message = format!(
            "{function}: '{}' object cannot be interpreted as an integer",
            unsafe { display_type_name(object) }
        );
        return Err(return_null_with_error(message));
    };
    if signalnum < 1 || signalnum >= NSIG {
        let message = "signal number out of range";
        // SAFETY: Typed raise helper with a static message.
        return Err(unsafe { crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) });
    }
    Ok(signalnum as usize)
}

/// `_signal.signal(signalnum, handler)`: validate, swap the table entry, and
/// return the prior handler.
unsafe extern "C" fn signal_signal(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 2 || argv.is_null() {
        return return_null_with_error(format!("signal() takes exactly 2 arguments ({argc} given)"));
    }
    // SAFETY: `argv` carries `argc` argument slots per the call ABI.
    let (signalnum, handler) = unsafe { (*argv, *argv.add(1)) };
    let signalnum = match unsafe { parse_signalnum(signalnum, "signal") } {
        Ok(signalnum) => signalnum,
        Err(raised) => return raised,
    };
    let new_slot = match unsafe { arg_as_i64(handler) } {
        Some(SIG_DFL) => StoredHandler::Dfl,
        Some(SIG_IGN) => StoredHandler::Ign,
        Some(_) => {
            let message = "signal handler must be signal.SIG_IGN, signal.SIG_DFL, or a callable object";
            // SAFETY: Typed raise helper with a static message.
            return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
        }
        None => {
            if handler.is_null() || unsafe { crate::types::int::type_name_is(handler, "NoneType") } {
                let message = "signal handler must be signal.SIG_IGN, signal.SIG_DFL, or a callable object";
                // SAFETY: Typed raise helper with a static message.
                return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
            }
            StoredHandler::Object(handler as usize)
        }
    };
    let previous = {
        let mut table = handlers_lock();
        core::mem::replace(&mut table[signalnum], new_slot)
    };
    previous.to_object()
}

/// `_signal.getsignal(signalnum)`: read the table entry.
unsafe extern "C" fn signal_getsignal(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 || argv.is_null() {
        return return_null_with_error(format!("getsignal() takes exactly 1 argument ({argc} given)"));
    }
    // SAFETY: One live argument slot per the check above.
    let signalnum = match unsafe { parse_signalnum(*argv, "getsignal") } {
        Ok(signalnum) => signalnum,
        Err(raised) => return raised,
    };
    handlers_lock()[signalnum].to_object()
}

/// `_signal.default_int_handler(*args)`: raises `KeyboardInterrupt` when
/// called (CPython ignores the `(signalnum, frame)` arguments too).
unsafe extern "C" fn signal_default_int_handler(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    raise_kind_error_no_args(ExceptionKind::KeyboardInterrupt)
}

unsafe extern "C" fn signal_alarm(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 || argv.is_null() {
        return return_null_with_error(format!("alarm() takes exactly 1 argument ({argc} given)"));
    }
    let seconds = match unsigned_int_arg(unsafe { *argv }, "seconds") {
        Ok(seconds) => seconds,
        Err(error) => return error,
    };
    let previous = unsafe { libc::alarm(seconds) };
    unsafe { pon_const_int(i64::from(previous)) }
}

unsafe extern "C" fn signal_getitimer(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 || argv.is_null() {
        return return_null_with_error(format!("getitimer() takes exactly 1 argument ({argc} given)"));
    }
    let which = match c_int_arg(unsafe { *argv }, "which") {
        Ok(which) => which,
        Err(error) => return error,
    };
    let mut current = std::mem::MaybeUninit::<libc::itimerval>::uninit();
    if unsafe { libc::getitimer(which, current.as_mut_ptr()) } != 0 {
        return raise_itimer_errno();
    }
    itimer_tuple(unsafe { current.assume_init() })
}

unsafe extern "C" fn signal_setitimer(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { arg_slice(argv, argc) } {
        Some(args) if (2..=3).contains(&args.len()) => args,
        _ => return return_null_with_error(format!("setitimer() takes 2 or 3 arguments ({argc} given)")),
    };
    let which = match c_int_arg(args[0], "which") {
        Ok(which) => which,
        Err(error) => return error,
    };
    let seconds = match float_arg(args[1], "seconds") {
        Ok(seconds) => seconds,
        Err(error) => return error,
    };
    let interval = match args.get(2).copied() {
        Some(object) => match float_arg(object, "interval") {
            Ok(interval) => interval,
            Err(error) => return error,
        },
        None => 0.0,
    };
    let new_value = libc::itimerval {
        it_interval: seconds_to_timeval(interval),
        it_value: seconds_to_timeval(seconds),
    };
    let mut old_value = std::mem::MaybeUninit::<libc::itimerval>::uninit();
    if unsafe { libc::setitimer(which, &new_value, old_value.as_mut_ptr()) } != 0 {
        return raise_itimer_errno();
    }
    itimer_tuple(unsafe { old_value.assume_init() })
}

unsafe extern "C" fn signal_pause(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return return_null_with_error(format!("pause() takes no arguments ({argc} given)"));
    }
    let result = unsafe { libc::pause() };
    if result == -1 {
        let errno = last_errno();
        if errno == libc::EINTR {
            return unsafe { crate::abi::pon_none() };
        }
        return raise_errno_text(errno);
    }
    unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn signal_pthread_kill(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 2 || argv.is_null() {
        return return_null_with_error(format!("pthread_kill() takes exactly 2 arguments ({argc} given)"));
    }
    let args = unsafe { std::slice::from_raw_parts(argv, argc) };
    let thread = match thread_id_arg(args[0]) {
        Ok(thread) => thread,
        Err(error) => return error,
    };
    let signalnum = match unsafe { parse_signalnum(args[1], "pthread_kill") } {
        Ok(signalnum) => signalnum as libc::c_int,
        Err(error) => return error,
    };
    let errno = unsafe { libc::pthread_kill(thread, signalnum) };
    if errno != 0 {
        return raise_errno_text(errno);
    }
    unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn signal_pthread_sigmask(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 2 || argv.is_null() {
        return return_null_with_error(format!("pthread_sigmask() takes exactly 2 arguments ({argc} given)"));
    }
    let args = unsafe { std::slice::from_raw_parts(argv, argc) };
    let how = match c_int_arg(args[0], "how") {
        Ok(how) => how,
        Err(error) => return error,
    };
    let set = match signal_set_arg(args[1], "pthread_sigmask") {
        Ok(set) => set,
        Err(error) => return error,
    };
    let mut old = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
    let set_ptr = set.as_ref().map_or(std::ptr::null(), |value| value as *const libc::sigset_t);
    let errno = unsafe { libc::pthread_sigmask(how, set_ptr, old.as_mut_ptr()) };
    if errno != 0 {
        return raise_errno_text(errno);
    }
    signal_set_to_object(unsafe { old.assume_init() })
}

unsafe extern "C" fn signal_raise_signal(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 || argv.is_null() {
        return return_null_with_error(format!("raise_signal() takes exactly 1 argument ({argc} given)"));
    }
    let signalnum = match unsafe { parse_signalnum(*argv, "raise_signal") } {
        Ok(signalnum) => signalnum as libc::c_int,
        Err(error) => return error,
    };
    if unsafe { libc::raise(signalnum) } != 0 {
        return raise_errno_text(last_errno());
    }
    unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn signal_set_wakeup_fd(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { arg_slice(argv, argc) } {
        Some(args) if (1..=2).contains(&args.len()) => args,
        _ => return return_null_with_error(format!("set_wakeup_fd() takes 1 or 2 arguments ({argc} given)")),
    };
    let fd = match int_arg(args[0], "fd") {
        Ok(fd) => fd,
        Err(error) => return error,
    };
    if fd < -1 || fd > i64::from(i32::MAX) {
        return unsafe { crate::abi::exc::pon_raise_value_error(b"fd must be -1 or a valid file descriptor".as_ptr(), 37) };
    }
    let previous = WAKEUP_FD.swap(fd as i32, Ordering::SeqCst);
    unsafe { pon_const_int(i64::from(previous)) }
}

unsafe extern "C" fn signal_siginterrupt(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 2 || argv.is_null() {
        return return_null_with_error(format!("siginterrupt() takes exactly 2 arguments ({argc} given)"));
    }
    let args = unsafe { std::slice::from_raw_parts(argv, argc) };
    let signalnum = match unsafe { parse_signalnum(args[0], "siginterrupt") } {
        Ok(signalnum) => signalnum as libc::c_int,
        Err(error) => return error,
    };
    let flag = match truth_arg(args[1]) {
        Ok(flag) => flag,
        Err(error) => return error,
    };
    let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
    if unsafe { libc::sigaction(signalnum, std::ptr::null(), action.as_mut_ptr()) } != 0 {
        return raise_errno_text(last_errno());
    }
    let mut action = unsafe { action.assume_init() };
    if flag == 1 {
        action.sa_flags &= !libc::SA_RESTART;
    } else {
        action.sa_flags |= libc::SA_RESTART;
    }
    if unsafe { libc::sigaction(signalnum, &action, std::ptr::null_mut()) } != 0 {
        return raise_errno_text(last_errno());
    }
    unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn signal_sigpending(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return return_null_with_error(format!("sigpending() takes no arguments ({argc} given)"));
    }
    let mut pending = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
    if unsafe { libc::sigpending(pending.as_mut_ptr()) } != 0 {
        return raise_errno_text(last_errno());
    }
    signal_set_to_object(unsafe { pending.assume_init() })
}

unsafe extern "C" fn signal_sigwait(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 || argv.is_null() {
        return return_null_with_error(format!("sigwait() takes exactly 1 argument ({argc} given)"));
    }
    let set = match signal_set_arg(unsafe { *argv }, "sigwait") {
        Ok(Some(set)) => set,
        Ok(None) => {
            return unsafe { crate::abi::exc::pon_raise_type_error(b"sigwait() arg must be an iterable of signals".as_ptr(), 43) };
        }
        Err(error) => return error,
    };
    let mut signum = 0;
    let errno = unsafe { libc::sigwait(&set, &mut signum) };
    if errno != 0 {
        return raise_errno_text(errno);
    }
    unsafe { pon_const_int(i64::from(signum)) }
}

unsafe extern "C" fn signal_strsignal(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 || argv.is_null() {
        return return_null_with_error(format!("strsignal() takes exactly 1 argument ({argc} given)"));
    }
    let signalnum = match c_int_arg(unsafe { *argv }, "signalnum") {
        Ok(signalnum) => signalnum,
        Err(error) => return error,
    };
    let ptr = unsafe { libc::strsignal(signalnum) };
    if ptr.is_null() {
        return unsafe { crate::abi::pon_none() };
    }
    let text = unsafe { std::ffi::CStr::from_ptr(ptr) }.to_string_lossy();
    unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) }
}

unsafe extern "C" fn signal_valid_signals(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return return_null_with_error(format!("valid_signals() takes no arguments ({argc} given)"));
    }
    let mut set = empty_sigset();
    for signum in 1..NSIG {
        unsafe { libc::sigaddset(&mut set, signum as libc::c_int) };
    }
    signal_set_to_object(set)
}

fn itimer_tuple(value: libc::itimerval) -> *mut PyObject {
    let mut items = [
        unsafe { crate::abi::number::pon_const_float(timeval_to_seconds(value.it_value)) },
        unsafe { crate::abi::number::pon_const_float(timeval_to_seconds(value.it_interval)) },
    ];
    unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

fn timeval_to_seconds(value: libc::timeval) -> f64 {
    value.tv_sec as f64 + value.tv_usec as f64 * 1e-6
}

fn seconds_to_timeval(seconds: f64) -> libc::timeval {
    let seconds = if seconds.is_sign_negative() { 0.0 } else { seconds };
    let whole = seconds.floor();
    let mut usec = ((seconds - whole) * 1_000_000.0).round() as i64;
    let mut sec = whole as i64;
    if usec >= 1_000_000 {
        sec += 1;
        usec -= 1_000_000;
    }
    libc::timeval {
        tv_sec: sec as libc::time_t,
        tv_usec: usec as libc::suseconds_t,
    }
}

fn signal_set_arg(object: *mut PyObject, function: &str) -> Result<Option<libc::sigset_t>, *mut PyObject> {
    if object.is_null() || unsafe { crate::types::int::type_name_is(object, "NoneType") } {
        return Ok(None);
    }
    let mut set = empty_sigset();
    let values = match super::builtins_batch::collect_iterable(object) {
        Ok(values) => values,
        Err(message) => return Err(return_null_with_error(message)),
    };
    for value in values {
        let signalnum = match unsafe { parse_signalnum(value, function) } {
            Ok(signalnum) => signalnum as libc::c_int,
            Err(error) => return Err(error),
        };
        unsafe { libc::sigaddset(&mut set, signalnum) };
    }
    Ok(Some(set))
}

fn signal_set_to_object(set: libc::sigset_t) -> *mut PyObject {
    let mut items = Vec::new();
    for signum in 1..NSIG {
        let present = unsafe { libc::sigismember(&set as *const libc::sigset_t, signum as libc::c_int) };
        if present == 1 {
            let object = unsafe { pon_const_int(signum) };
            if object.is_null() {
                return std::ptr::null_mut();
            }
            items.push(object);
        }
    }
    let iterable = unsafe {
        crate::abi::seq::pon_build_list(
            if items.is_empty() { std::ptr::null_mut() } else { items.as_mut_ptr() },
            items.len(),
        )
    };
    if iterable.is_null() {
        return std::ptr::null_mut();
    }
    let set_ctor = unsafe { crate::abi::pon_load_global(intern("set"), std::ptr::null_mut()) };
    if set_ctor.is_null() {
        return std::ptr::null_mut();
    }
    let mut args = [iterable];
    unsafe { crate::abi::pon_call(set_ctor, args.as_mut_ptr(), args.len()) }
}

fn empty_sigset() -> libc::sigset_t {
    let mut set = unsafe { std::mem::zeroed::<libc::sigset_t>() };
    unsafe { libc::sigemptyset(&mut set) };
    set
}

unsafe fn arg_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
    if argc == 0 {
        Some(&[])
    } else if argv.is_null() {
        None
    } else {
        Some(unsafe { std::slice::from_raw_parts(argv, argc) })
    }
}

fn int_arg(object: *mut PyObject, what: &str) -> Result<i64, *mut PyObject> {
    let Some(value) = (unsafe { arg_as_i64(object) }) else {
        return Err(return_null_with_error(format!("{what} must be an integer")));
    };
    Ok(value)
}

fn float_arg(object: *mut PyObject, what: &str) -> Result<f64, *mut PyObject> {
    if let Some(value) = unsafe { crate::types::float::to_f64(object) } {
        if value.is_finite() && value >= 0.0 {
            return Ok(value);
        }
        return Err(unsafe { crate::abi::exc::pon_raise_value_error(format!("{what} must be non-negative and finite").as_ptr(), format!("{what} must be non-negative and finite").len()) });
    }
    match unsafe { arg_as_i64(object) } {
        Some(value) if value >= 0 => Ok(value as f64),
        Some(_) => Err(unsafe { crate::abi::exc::pon_raise_value_error(format!("{what} must be non-negative").as_ptr(), format!("{what} must be non-negative").len()) }),
        None => Err(unsafe { crate::abi::exc::pon_raise_type_error(format!("{what} must be a number").as_ptr(), format!("{what} must be a number").len()) }),
    }
}
fn c_int_arg(object: *mut PyObject, what: &str) -> Result<libc::c_int, *mut PyObject> {
    let Some(value) = (unsafe { arg_as_i64(object) }) else {
        return Err(return_null_with_error(format!("{what} must be an integer")));
    };
    libc::c_int::try_from(value).map_err(|_| unsafe {
        crate::abi::exc::pon_raise_value_error(format!("{what} is out of range").as_ptr(), format!("{what} is out of range").len())
    })
}
fn unsigned_int_arg(object: *mut PyObject, what: &str) -> Result<libc::c_uint, *mut PyObject> {
    let Some(value) = (unsafe { arg_as_i64(object) }) else {
        return Err(return_null_with_error(format!("{what} must be an integer")));
    };
    libc::c_uint::try_from(value).map_err(|_| unsafe {
        crate::abi::exc::pon_raise_value_error(format!("{what} is out of range").as_ptr(), format!("{what} is out of range").len())
    })
}
fn thread_id_arg(object: *mut PyObject) -> Result<libc::pthread_t, *mut PyObject> {
    let Some(value) = (unsafe { arg_as_i64(object) }) else {
        return Err(return_null_with_error("thread_id must be an integer"));
    };
    #[allow(clippy::cast_sign_loss)]
    Ok(value as libc::pthread_t)
}

fn truth_arg(object: *mut PyObject) -> Result<libc::c_int, *mut PyObject> {
    match unsafe { crate::abi::pon_is_true(object) } {
        0 => Ok(0),
        1 => Ok(1),
        _ => Err(std::ptr::null_mut()),
    }
}

fn last_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}

fn raise_errno_text(errno: i32) -> *mut PyObject {
    let detail = unsafe { std::ffi::CStr::from_ptr(libc::strerror(errno)) }.to_string_lossy();
    crate::abi::exc::raise_kind_error_text(ExceptionKind::OSError, &format!("[Errno {errno}] {detail}"))
}

fn raise_itimer_errno() -> *mut PyObject {
    let errno = last_errno();
    let detail = unsafe { std::ffi::CStr::from_ptr(libc::strerror(errno)) }.to_string_lossy();
    let message = format!("[Errno {errno}] {detail}");
    let message_obj = unsafe { crate::abi::pon_const_str(message.as_ptr(), message.len()) };
    if message_obj.is_null() {
        return std::ptr::null_mut();
    }
    let mut args = [message_obj];
    let exception = unsafe { crate::abi::pon_call(itimer_error_type().cast::<PyObject>(), args.as_mut_ptr(), args.len()) };
    if exception.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { crate::abi::pon_raise(exception, std::ptr::null_mut()) }
}
