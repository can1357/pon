//! Native `select` module seed backed by host `poll(2)`/`select(2)`.
//!
//! The stdlib `selectors` module prefers `select.poll()` on POSIX.  `subprocess`
//! uses that path to drive `Popen.communicate()` for captured stdout/stderr, so
//! Pon exposes the small CPython-compatible surface that real subprocess code
//! exercises: `poll` objects with register/modify/unregister/poll and a
//! `select.select` fallback.

use std::mem::MaybeUninit;
use std::ptr;
use std::sync::LazyLock;

use num_traits::ToPrimitive;

use crate::abi::{pon_const_int, pon_const_str, pon_load_global, pon_make_function};
use crate::intern::intern;
use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::types::exc::ExceptionKind;

use super::install_module;

const VARIADIC_ARITY: usize = crate::native::builtins_mod::VARIADIC_ARITY;

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

#[derive(Clone, Copy, Debug)]
struct PollEntry {
    fd: libc::c_int,
    events: libc::c_short,
}

#[repr(C)]
#[derive(Debug)]
struct PyPoll {
    ob_base: PyObjectHeader,
    entries: Vec<PollEntry>,
}

static POLL_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(
        crate::abi::runtime_type_type().cast_const(),
        "select.poll",
        std::mem::size_of::<PyPoll>(),
    );
    ty.tp_getattro = Some(poll_getattro);
    Box::into_raw(Box::new(ty)) as usize
});

fn poll_type() -> *mut PyType {
    *POLL_TYPE as *mut PyType
}

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
    let object = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate select.{name}"))
}

unsafe extern "C" fn select_poll(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return raise_type_error("select.poll() takes no arguments");
    }
    Box::into_raw(Box::new(PyPoll {
        ob_base: PyObjectHeader::new(poll_type()),
        entries: Vec::new(),
    }))
    .cast::<PyObject>()
}

unsafe extern "C" fn poll_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { crate::types::type_::unicode_text(crate::tag::untag_arg(name)) }) else {
        return raise_type_error("attribute name must be str");
    };
    match name_text {
        "register" => bound_method(object, name_text, poll_register_method),
        "modify" => bound_method(object, name_text, poll_modify_method),
        "unregister" => bound_method(object, name_text, poll_unregister_method),
        "poll" => bound_method(object, name_text, poll_poll_method),
        _ => unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
    }
}

fn bound_method(receiver: *mut PyObject, name: &str, entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject) -> *mut PyObject {
    // SAFETY: `entry` is a live builtin entry point with the runtime calling convention.
    let function = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match crate::types::method::new_bound_method(function, receiver) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => crate::abi::exc::raise_kind_error_text(ExceptionKind::RuntimeError, &message),
    }
}

unsafe extern "C" fn poll_register_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { arg_slice(argv, argc) } {
        Some(args) => args,
        None => return crate::abi::return_null_with_error("poll.register received a null argv pointer"),
    };
    if !(2..=3).contains(&args.len()) {
        return raise_type_error("poll.register expected fd and optional eventmask");
    }
    let Some(poll) = (unsafe { poll_receiver(args[0]) }) else {
        return raise_type_error("poll.register receiver is invalid");
    };
    let fd = match fd_arg(args[1], "poll.register fd") {
        Ok(fd) => fd,
        Err(error) => return error,
    };
    let events = if let Some(&event_obj) = args.get(2) {
        match eventmask_arg(event_obj, "poll.register eventmask") {
            Ok(events) => events,
            Err(error) => return error,
        }
    } else {
        (libc::POLLIN | libc::POLLPRI | libc::POLLOUT) as libc::c_short
    };
    upsert_poll_entry(poll, fd, events);
    none()
}

unsafe extern "C" fn poll_modify_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { arg_slice(argv, argc) } {
        Some(args) => args,
        None => return crate::abi::return_null_with_error("poll.modify received a null argv pointer"),
    };
    if args.len() != 3 {
        return raise_type_error("poll.modify expected fd and eventmask");
    }
    let Some(poll) = (unsafe { poll_receiver(args[0]) }) else {
        return raise_type_error("poll.modify receiver is invalid");
    };
    let fd = match fd_arg(args[1], "poll.modify fd") {
        Ok(fd) => fd,
        Err(error) => return error,
    };
    let events = match eventmask_arg(args[2], "poll.modify eventmask") {
        Ok(events) => events,
        Err(error) => return error,
    };
    if let Some(entry) = poll.entries.iter_mut().find(|entry| entry.fd == fd) {
        entry.events = events;
        none()
    } else {
        raise_key_error(&format!("{fd} is not registered"))
    }
}

unsafe extern "C" fn poll_unregister_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { arg_slice(argv, argc) } {
        Some(args) => args,
        None => return crate::abi::return_null_with_error("poll.unregister received a null argv pointer"),
    };
    if args.len() != 2 {
        return raise_type_error("poll.unregister expected fd");
    }
    let Some(poll) = (unsafe { poll_receiver(args[0]) }) else {
        return raise_type_error("poll.unregister receiver is invalid");
    };
    let fd = match fd_arg(args[1], "poll.unregister fd") {
        Ok(fd) => fd,
        Err(error) => return error,
    };
    if let Some(index) = poll.entries.iter().position(|entry| entry.fd == fd) {
        poll.entries.remove(index);
        none()
    } else {
        raise_key_error(&format!("{fd} is not registered"))
    }
}

unsafe extern "C" fn poll_poll_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { arg_slice(argv, argc) } {
        Some(args) => args,
        None => return crate::abi::return_null_with_error("poll.poll received a null argv pointer"),
    };
    if args.len() > 2 || args.is_empty() {
        return raise_type_error("poll.poll expected optional timeout");
    }
    let Some(poll) = (unsafe { poll_receiver(args[0]) }) else {
        return raise_type_error("poll.poll receiver is invalid");
    };
    let timeout = if let Some(&timeout) = args.get(1) {
        match timeout_ms_arg(timeout) {
            Ok(timeout) => timeout,
            Err(error) => return error,
        }
    } else {
        -1
    };
    let mut pfds = poll
        .entries
        .iter()
        .map(|entry| libc::pollfd { fd: entry.fd, events: entry.events, revents: 0 })
        .collect::<Vec<_>>();
    // SAFETY: `pfds` is either empty or a writable pollfd array.
    let rc = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, timeout) };
    if rc < 0 {
        return raise_errno(last_errno());
    }
    build_poll_result(&pfds)
}

unsafe fn poll_receiver<'a>(object: *mut PyObject) -> Option<&'a mut PyPoll> {
    let object = crate::tag::untag_arg(object);
    if object.is_null() || crate::tag::is_small_int(object) {
        return None;
    }
    if unsafe { (*object).ob_type } != poll_type().cast_const() {
        return None;
    }
    Some(unsafe { &mut *object.cast::<PyPoll>() })
}

fn upsert_poll_entry(poll: &mut PyPoll, fd: libc::c_int, events: libc::c_short) {
    if let Some(entry) = poll.entries.iter_mut().find(|entry| entry.fd == fd) {
        entry.events = events;
    } else {
        poll.entries.push(PollEntry { fd, events });
    }
}

fn build_poll_result(pfds: &[libc::pollfd]) -> *mut PyObject {
    let mut rows = Vec::new();
    for pfd in pfds.iter().copied().filter(|pfd| pfd.revents != 0) {
        let fd = unsafe { pon_const_int(i64::from(pfd.fd)) };
        let events = unsafe { pon_const_int(i64::from(pfd.revents)) };
        if fd.is_null() || events.is_null() {
            return ptr::null_mut();
        }
        let mut pair = [fd, events];
        let tuple = unsafe { crate::abi::seq::pon_build_tuple(pair.as_mut_ptr(), pair.len()) };
        if tuple.is_null() {
            return ptr::null_mut();
        }
        rows.push(tuple);
    }
    unsafe { crate::abi::seq::pon_build_list(rows.as_mut_ptr(), rows.len()) }
}

unsafe extern "C" fn select_select(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { arg_slice(argv, argc) } {
        Some(args) => args,
        None => return crate::abi::return_null_with_error("select.select received a null argv pointer"),
    };
    if !(3..=4).contains(&args.len()) {
        return raise_type_error("select.select expected 3 or 4 arguments");
    }
    let read = match fd_sequence(args[0], "rlist") {
        Ok(fds) => fds,
        Err(error) => return error,
    };
    let write = match fd_sequence(args[1], "wlist") {
        Ok(fds) => fds,
        Err(error) => return error,
    };
    let except = match fd_sequence(args[2], "xlist") {
        Ok(fds) => fds,
        Err(error) => return error,
    };
    let timeout = if let Some(&timeout) = args.get(3) {
        match timeout_timeval(timeout) {
            Ok(timeout) => timeout,
            Err(error) => return error,
        }
    } else {
        None
    };

    let mut read_set = zero_fd_set();
    let mut write_set = zero_fd_set();
    let mut except_set = zero_fd_set();
    let mut max_fd = -1;
    if let Err(error) = fill_fd_set(&read, &mut read_set, &mut max_fd) {
        return error;
    }
    if let Err(error) = fill_fd_set(&write, &mut write_set, &mut max_fd) {
        return error;
    }
    if let Err(error) = fill_fd_set(&except, &mut except_set, &mut max_fd) {
        return error;
    }
    let mut timeout_storage = timeout;
    let timeout_ptr = timeout_storage
        .as_mut()
        .map_or(ptr::null_mut(), |timeout| timeout as *mut libc::timeval);
    // SAFETY: fd_sets are initialized and timeout points to live storage or NULL.
    let rc = unsafe {
        libc::select(
            max_fd + 1,
            &mut read_set,
            &mut write_set,
            &mut except_set,
            timeout_ptr,
        )
    };
    if rc < 0 {
        return raise_errno(last_errno());
    }
    let rready = ready_list(&read, &read_set);
    if rready.is_null() {
        return ptr::null_mut();
    }
    let wready = ready_list(&write, &write_set);
    if wready.is_null() {
        return ptr::null_mut();
    }
    let xready = ready_list(&except, &except_set);
    if xready.is_null() {
        return ptr::null_mut();
    }
    let mut result = [rready, wready, xready];
    unsafe { crate::abi::seq::pon_build_tuple(result.as_mut_ptr(), result.len()) }
}

fn zero_fd_set() -> libc::fd_set {
    let mut set = MaybeUninit::<libc::fd_set>::uninit();
    // SAFETY: `set` is an out-slot for FD_ZERO to initialize.
    unsafe { libc::FD_ZERO(set.as_mut_ptr()) };
    // SAFETY: FD_ZERO initialized the set.
    unsafe { set.assume_init() }
}

fn fill_fd_set(fds: &[libc::c_int], set: &mut libc::fd_set, max_fd: &mut libc::c_int) -> Result<(), *mut PyObject> {
    for &fd in fds {
        if fd < 0 || fd as usize >= libc::FD_SETSIZE {
            return Err(raise_value_error("filedescriptor out of range in select()"));
        }
        // SAFETY: Range check above keeps fd within fd_set capacity.
        unsafe { libc::FD_SET(fd, set) };
        *max_fd = (*max_fd).max(fd);
    }
    Ok(())
}

fn ready_list(fds: &[libc::c_int], set: &libc::fd_set) -> *mut PyObject {
    let mut ready = Vec::new();
    for &fd in fds {
        // SAFETY: fds came through `fill_fd_set`, so each fd is in range.
        if unsafe { libc::FD_ISSET(fd, set) } {
            let object = unsafe { pon_const_int(i64::from(fd)) };
            if object.is_null() {
                return ptr::null_mut();
            }
            ready.push(object);
        }
    }
    unsafe { crate::abi::seq::pon_build_list(ready.as_mut_ptr(), ready.len()) }
}

fn fd_sequence(object: *mut PyObject, what: &str) -> Result<Vec<libc::c_int>, *mut PyObject> {
    let raw = crate::tag::untag_arg(object);
    if raw.is_null() || crate::tag::is_small_int(raw) {
        return Err(raise_type_error(&format!("{what} must be a list or tuple")));
    }
    let items = match unsafe { crate::types::dict::type_name(raw) } {
        Some("list") => unsafe { (*raw.cast::<crate::types::list::PyList>()).as_slice() },
        Some("tuple") => unsafe { (*raw.cast::<crate::types::tuple::PyTuple>()).as_slice() },
        _ => return Err(raise_type_error(&format!("{what} must be a list or tuple"))),
    };
    items
        .iter()
        .copied()
        .map(|item| fd_arg(item, what))
        .collect()
}

unsafe fn arg_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
    if argc == 0 {
        Some(&[])
    } else if argv.is_null() {
        None
    } else {
        // SAFETY: The caller passed `argc` live argument slots.
        Some(unsafe { std::slice::from_raw_parts(argv, argc) })
    }
}

fn fd_arg(object: *mut PyObject, what: &str) -> Result<libc::c_int, *mut PyObject> {
    let value = int_arg(object, what)?;
    if value < i64::from(i32::MIN) || value > i64::from(i32::MAX) {
        return Err(raise_value_error(&format!("{what} is out of range")));
    }
    Ok(value as libc::c_int)
}

fn eventmask_arg(object: *mut PyObject, what: &str) -> Result<libc::c_short, *mut PyObject> {
    let value = int_arg(object, what)?;
    if value < i64::from(i16::MIN) || value > i64::from(i16::MAX) {
        return Err(raise_value_error(&format!("{what} is out of range")));
    }
    Ok(value as libc::c_short)
}

fn int_arg(object: *mut PyObject, what: &str) -> Result<i64, *mut PyObject> {
    if crate::tag::is_small_int(object) {
        return Ok(crate::tag::untag_small_int(object));
    }
    match unsafe { crate::types::int::to_bigint_including_bool(object) } {
        Some(value) => value.to_i64().ok_or_else(|| raise_value_error(&format!("{what} is too large"))),
        None => Err(raise_type_error(&format!("{what} must be an integer"))),
    }
}

fn timeout_ms_arg(object: *mut PyObject) -> Result<libc::c_int, *mut PyObject> {
    if is_none(object) {
        return Ok(-1);
    }
    let value = number_to_f64(object).ok_or_else(|| raise_type_error("timeout must be a number or None"))?;
    if !value.is_finite() {
        return Err(raise_value_error("timeout must be finite"));
    }
    if value < 0.0 {
        return Ok(-1);
    }
    if value > f64::from(i32::MAX) {
        return Err(raise_value_error("timeout is too large"));
    }
    Ok(value.ceil() as libc::c_int)
}

fn timeout_timeval(object: *mut PyObject) -> Result<Option<libc::timeval>, *mut PyObject> {
    if is_none(object) {
        return Ok(None);
    }
    let value = number_to_f64(object).ok_or_else(|| raise_type_error("timeout must be a number or None"))?;
    if !value.is_finite() {
        return Err(raise_value_error("timeout must be finite"));
    }
    if value < 0.0 {
        return Err(raise_value_error("timeout must be non-negative"));
    }
    let seconds = value.floor();
    let mut usec = ((value - seconds) * 1_000_000.0).ceil() as i64;
    let mut sec = seconds as i64;
    if usec >= 1_000_000 {
        sec += 1;
        usec -= 1_000_000;
    }
    Ok(Some(libc::timeval {
        tv_sec: sec as libc::time_t,
        tv_usec: usec as libc::suseconds_t,
    }))
}

fn number_to_f64(object: *mut PyObject) -> Option<f64> {
    if let Some(value) = unsafe { crate::types::float::to_f64(object) } {
        return Some(value);
    }
    unsafe { crate::types::int::to_bigint_including_bool(object) }.and_then(|value| value.to_f64())
}

fn is_none(object: *mut PyObject) -> bool {
    if object.is_null() {
        return true;
    }
    let raw = crate::tag::untag_arg(object);
    if raw.is_null() || crate::tag::is_small_int(raw) {
        return false;
    }
    unsafe { crate::types::dict::type_name(raw) == Some("NoneType") }
}

fn none() -> *mut PyObject {
    unsafe { crate::abi::pon_none() }
}

fn raise_errno(errno: i32) -> *mut PyObject {
    let kind = match errno {
        libc::EINTR => ExceptionKind::InterruptedError,
        libc::EBADF => ExceptionKind::OSError,
        _ => ExceptionKind::OSError,
    };
    let detail = unsafe { std::ffi::CStr::from_ptr(libc::strerror(errno)) }.to_string_lossy();
    crate::abi::exc::raise_kind_error_text(kind, &format!("[Errno {errno}] {detail}"))
}

fn raise_type_error(message: &str) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

fn raise_value_error(message: &str) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(ExceptionKind::ValueError, message)
}

fn raise_key_error(message: &str) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(ExceptionKind::KeyError, message)
}

fn last_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}
