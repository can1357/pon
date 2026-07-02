//! Native `select` module seed: import surface only.
//!
//! The CT frontier needs `import select` (directly in tests) and `import
//! selectors` (stdlib) to succeed; actual I/O multiplexing is not wired yet,
//! so `select()` and `poll()` raise OSError honestly on use. `selectors`
//! tolerates exactly that: its `_can_use('poll')` probe calls `select.poll()`
//! inside `try/except OSError` and falls back to `SelectSelector`, whose
//! class body only *references* `select.select`. `kqueue`/`epoll`/`devpoll`
//! are deliberately absent so `hasattr` guards (selectors' backend choice,
//! test_kqueue's SkipTest) take their clean fallback paths.

use crate::abi::{pon_const_int, pon_const_str, pon_load_global, pon_make_function};
use crate::intern::intern;
use crate::object::PyObject;

use super::install_module;

/// `poll(2)` event-mask constants shared by macOS and Linux, sorted by name.
/// `selectors.PollSelector`'s class body reads `POLLIN`/`POLLOUT` at import
/// time whenever `select.poll` exists.
const POLL_EVENTS: &[(&str, i16)] = &[
    ("POLLERR", libc::POLLERR),
    ("POLLHUP", libc::POLLHUP),
    ("POLLIN", libc::POLLIN),
    ("POLLNVAL", libc::POLLNVAL),
    ("POLLOUT", libc::POLLOUT),
    ("POLLPRI", libc::POLLPRI),
    ("POLLRDBAND", libc::POLLRDBAND),
    ("POLLRDNORM", libc::POLLRDNORM),
    ("POLLWRBAND", libc::POLLWRBAND),
    ("POLLWRNORM", libc::POLLWRNORM),
];

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "select";
    // SAFETY: Runtime allocation helper; NULL is checked below.
    let name_obj = unsafe { pon_const_str(name.as_ptr(), name.len()) };
    if name_obj.is_null() {
        return Err("failed to allocate select.__name__".to_owned());
    }
    let mut attrs = vec![(intern("__name__"), name_obj)];
    for &(const_name, value) in POLL_EVENTS {
        attrs.push(int_attr(const_name, i64::from(value))?);
    }
    attrs.push(int_attr("PIPE_BUF", libc::PIPE_BUF as i64)?);
    // `select.error` has been an alias of the builtin OSError since 3.3;
    // loading the registered builtin keeps `select.error is OSError` true.
    // SAFETY: Global lookup helper; NULL is checked below.
    let error = unsafe { pon_load_global(intern("OSError"), core::ptr::null_mut()) };
    if error.is_null() {
        return Err("builtin OSError is not registered for select.error".to_owned());
    }
    attrs.push((intern("error"), error));
    attrs.push(function_attr("select", select_select)?);
    attrs.push(function_attr("poll", select_poll)?);
    install_module(name, attrs)
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { pon_const_int(value) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate select.{name}"))
}

fn function_attr(
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Live builtin entry point with the runtime calling convention.
    let object = unsafe { pon_make_function(entry as *const u8, crate::builtins::variadic_arity(), intern(name)) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate select.{name}"))
}

unsafe extern "C" fn select_select(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    raise_not_wired("select.select")
}

unsafe extern "C" fn select_poll(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    raise_not_wired("select.poll")
}

/// Honest failure for the not-yet-wired syscall entry points. OSError (rather
/// than NotImplementedError) is deliberate: `selectors._can_use` catches
/// exactly OSError when probing backends, so importing `selectors` degrades
/// to `SelectSelector` instead of dying.
fn raise_not_wired(which: &str) -> *mut PyObject {
    let message = format!("{which} is not wired to the host yet in the pon runtime");
    // SAFETY: Typed raise helper; the message bytes are copied.
    unsafe { crate::abi::exc::pon_raise_os_error(message.as_ptr(), message.len()) }
}
