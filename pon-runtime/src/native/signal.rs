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

use crate::abi::exc::raise_kind_error_no_args;
use crate::abi::{pon_const_int, pon_make_function, return_null_with_error};
use crate::intern::intern;
use crate::object::PyObject;
use crate::types::exc::ExceptionKind;

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
    attrs.push(int_attr("SIG_DFL", SIG_DFL)?);
    attrs.push(int_attr("SIG_IGN", SIG_IGN)?);
    attrs.push(int_attr("NSIG", NSIG)?);
    let default_int_handler = function_attr("default_int_handler", signal_default_int_handler)?;
    // CPython's module init registers `default_int_handler` for SIGINT; the
    // table mirrors that so `getsignal(SIGINT)` observes it at startup.
    handlers_lock()[libc::SIGINT as usize] = StoredHandler::Object(default_int_handler.1 as usize);
    attrs.push(default_int_handler);
    attrs.push(function_attr("signal", signal_signal)?);
    attrs.push(function_attr("getsignal", signal_getsignal)?);
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
