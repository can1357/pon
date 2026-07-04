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

#[cfg(target_os = "macos")]
const KQUEUE_CONSTANTS: &[(&str, i64)] = &[
    ("KQ_EV_ADD", 1),
    ("KQ_EV_CLEAR", 32),
    ("KQ_EV_DELETE", 2),
    ("KQ_EV_DISABLE", 8),
    ("KQ_EV_ENABLE", 4),
    ("KQ_EV_EOF", 32768),
    ("KQ_EV_ERROR", 16384),
    ("KQ_EV_FLAG1", 8192),
    ("KQ_EV_ONESHOT", 16),
    ("KQ_EV_SYSFLAGS", 61440),
    ("KQ_FILTER_AIO", -3),
    ("KQ_FILTER_PROC", -5),
    ("KQ_FILTER_READ", -1),
    ("KQ_FILTER_SIGNAL", -6),
    ("KQ_FILTER_TIMER", -7),
    ("KQ_FILTER_VNODE", -4),
    ("KQ_FILTER_WRITE", -2),
    ("KQ_NOTE_ATTRIB", 8),
    ("KQ_NOTE_CHILD", 4),
    ("KQ_NOTE_DELETE", 1),
    ("KQ_NOTE_EXEC", 536870912),
    ("KQ_NOTE_EXIT", 2147483648),
    ("KQ_NOTE_EXTEND", 4),
    ("KQ_NOTE_FORK", 1073741824),
    ("KQ_NOTE_LINK", 16),
    ("KQ_NOTE_LOWAT", 1),
    ("KQ_NOTE_PCTRLMASK", -1048576),
    ("KQ_NOTE_PDATAMASK", 1048575),
    ("KQ_NOTE_RENAME", 32),
    ("KQ_NOTE_REVOKE", 64),
    ("KQ_NOTE_TRACK", 1),
    ("KQ_NOTE_TRACKERR", 2),
    ("KQ_NOTE_WRITE", 2),
];

#[cfg(not(target_os = "macos"))]
const KQUEUE_CONSTANTS: &[(&str, i64)] = &[];

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

#[repr(C)]
#[derive(Debug)]
struct PyKqueue {
    ob_base: PyObjectHeader,
    fd: libc::c_int,
    closed: bool,
}

#[repr(C)]
#[derive(Debug)]
struct PyKevent {
    ob_base: PyObjectHeader,
    ident: usize,
    filter: i16,
    flags: u16,
    fflags: u32,
    data: isize,
    udata: usize,
}

static KQUEUE_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(
        crate::abi::runtime_type_type().cast_const(),
        "select.kqueue",
        std::mem::size_of::<PyKqueue>(),
    );
    ty.tp_getattro = Some(kqueue_getattro);
    Box::into_raw(Box::new(ty)) as usize
});

static KEVENT_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(
        crate::abi::runtime_type_type().cast_const(),
        "select.kevent",
        std::mem::size_of::<PyKevent>(),
    );
    ty.tp_getattro = Some(kevent_getattro);
    ty.tp_repr = Some(kevent_repr);
    Box::into_raw(Box::new(ty)) as usize
});

fn kqueue_type() -> *mut PyType {
    *KQUEUE_TYPE as *mut PyType
}

fn kevent_type() -> *mut PyType {
    *KEVENT_TYPE as *mut PyType
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
    for &(const_name, value) in KQUEUE_CONSTANTS {
        attrs.push(int_attr(const_name, value)?);
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
    #[cfg(target_os = "macos")]
    {
        attrs.push(function_attr("kqueue", select_kqueue)?);
        attrs.push(function_attr("kevent", select_kevent)?);
    }
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

#[cfg(target_os = "macos")]
unsafe extern "C" fn select_kqueue(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return raise_type_error("select.kqueue() takes no arguments");
    }
    let fd = unsafe { libc::kqueue() };
    if fd < 0 {
        return raise_errno(last_errno());
    }
    Box::into_raw(Box::new(PyKqueue {
        ob_base: PyObjectHeader::new(kqueue_type()),
        fd,
        closed: false,
    }))
    .cast::<PyObject>()
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn select_kevent(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { arg_slice(argv, argc) } {
        Some(args) if (1..=6).contains(&args.len()) => args,
        _ => return raise_type_error(&format!("kevent expected 1 to 6 arguments, got {argc}")),
    };
    let ident = match int_arg(args[0], "ident").and_then(|value| {
        usize::try_from(value).map_err(|_| raise_value_error("ident is out of range"))
    }) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let filter = match args.get(1).copied() {
        Some(object) => match i16_arg(object, "filter") {
            Ok(value) => value,
            Err(error) => return error,
        },
        None => libc::EVFILT_READ,
    };
    let flags = match args.get(2).copied() {
        Some(object) => match u16_arg(object, "flags") {
            Ok(value) => value,
            Err(error) => return error,
        },
        None => libc::EV_ADD as u16,
    };
    let fflags = match args.get(3).copied() {
        Some(object) => match u32_arg(object, "fflags") {
            Ok(value) => value,
            Err(error) => return error,
        },
        None => 0,
    };
    let data = match args.get(4).copied() {
        Some(object) => match isize_arg(object, "data") {
            Ok(value) => value,
            Err(error) => return error,
        },
        None => 0,
    };
    let udata = match args.get(5).copied() {
        Some(object) => match int_arg(object, "udata").and_then(|value| {
            usize::try_from(value).map_err(|_| raise_value_error("udata is out of range"))
        }) {
            Ok(value) => value,
            Err(error) => return error,
        },
        None => 0,
    };
    kevent_object(ident, filter, flags, fflags, data, udata)
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn kqueue_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { crate::types::type_::unicode_text(crate::tag::untag_arg(name)) }) else {
        return raise_type_error("attribute name must be str");
    };
    match name_text {
        "close" => bound_method(object, name_text, kqueue_close_method),
        "fileno" => bound_method(object, name_text, kqueue_fileno_method),
        "control" => bound_method(object, name_text, kqueue_control_method),
        _ => unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn kqueue_close_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (queue, _) = match unsafe { kqueue_receiver(argv, argc, "close", 0) } {
        Ok(value) => value,
        Err(error) => return error,
    };
    if !queue.closed {
        unsafe { libc::close(queue.fd) };
        queue.closed = true;
        queue.fd = -1;
    }
    none()
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn kqueue_fileno_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (queue, _) = match unsafe { kqueue_receiver(argv, argc, "fileno", 0) } {
        Ok(value) => value,
        Err(error) => return error,
    };
    unsafe { pon_const_int(i64::from(queue.fd)) }
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn kqueue_control_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (queue, args) = match unsafe { kqueue_receiver(argv, argc, "control", 3) } {
        Ok(value) => value,
        Err(error) => return error,
    };
    if args.len() < 2 {
        return raise_type_error("control() requires changelist and max_events");
    }
    let changes = match kevent_list_arg(args[0]) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let max_events = match int_arg(args[1], "max_events").and_then(|value| {
        libc::c_int::try_from(value).map_err(|_| raise_value_error("max_events is out of range"))
    }) {
        Ok(value) => value,
        Err(error) => return error,
    };
    if max_events < 0 {
        return raise_value_error("max_events must be positive");
    }
    let timeout = match args.get(2).copied() {
        Some(object) => match timeout_timespec(object) {
            Ok(value) => value,
            Err(error) => return error,
        },
        None => None,
    };
    let mut events = vec![unsafe { std::mem::zeroed::<libc::kevent>() }; max_events as usize];
    let timeout_ptr = timeout.as_ref().map_or(ptr::null(), |value| value as *const libc::timespec);
    let result = unsafe {
        libc::kevent(
            queue.fd,
            if changes.is_empty() { ptr::null() } else { changes.as_ptr() },
            changes.len() as libc::c_int,
            if events.is_empty() { ptr::null_mut() } else { events.as_mut_ptr() },
            max_events,
            timeout_ptr,
        )
    };
    if result < 0 {
        return raise_errno(last_errno());
    }
    let mut objects = Vec::with_capacity(result as usize);
    for event in events.into_iter().take(result as usize) {
        let object = kevent_object(event.ident as usize, event.filter, event.flags, event.fflags, event.data, event.udata as usize);
        if object.is_null() {
            return ptr::null_mut();
        }
        objects.push(object);
    }
    unsafe { crate::abi::seq::pon_build_list(if objects.is_empty() { ptr::null_mut() } else { objects.as_mut_ptr() }, objects.len()) }
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn kevent_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { crate::types::type_::unicode_text(crate::tag::untag_arg(name)) }) else {
        return raise_type_error("attribute name must be str");
    };
    let event = unsafe { &*object.cast::<PyKevent>() };
    match name_text {
        "ident" => unsafe { pon_const_int(event.ident as i64) },
        "filter" => unsafe { pon_const_int(i64::from(event.filter)) },
        "flags" => unsafe { pon_const_int(i64::from(event.flags)) },
        "fflags" => unsafe { pon_const_int(i64::from(event.fflags)) },
        "data" => unsafe { pon_const_int(event.data as i64) },
        "udata" => unsafe { pon_const_int(event.udata as i64) },
        _ => unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn kevent_repr(object: *mut PyObject) -> *mut PyObject {
    let event = unsafe { &*object.cast::<PyKevent>() };
    let text = format!(
        "select.kevent(ident={}, filter={}, flags={}, fflags={}, data={}, udata={})",
        event.ident, event.filter, event.flags, event.fflags, event.data, event.udata
    );
    unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

#[cfg(target_os = "macos")]
unsafe fn kqueue_receiver<'a>(
    argv: *mut *mut PyObject,
    argc: usize,
    function: &str,
    max_extra: usize,
) -> Result<(&'a mut PyKqueue, &'a [*mut PyObject]), *mut PyObject> {
    let args = unsafe { arg_slice(argv, argc) }.ok_or_else(|| raise_type_error("invalid argument vector"))?;
    if args.is_empty() || args.len() > max_extra + 1 {
        return Err(raise_type_error(&format!("{function} expected at most {max_extra} arguments")));
    }
    let object = crate::tag::untag_arg(args[0]);
    if object.is_null() || unsafe { (*object).ob_type } != kqueue_type().cast_const() {
        return Err(raise_type_error("kqueue method called with invalid receiver"));
    }
    let queue = unsafe { &mut *object.cast::<PyKqueue>() };
    if queue.closed {
        return Err(raise_value_error("I/O operation on closed kqueue object"));
    }
    Ok((queue, &args[1..]))
}

#[cfg(target_os = "macos")]
fn kevent_list_arg(object: *mut PyObject) -> Result<Vec<libc::kevent>, *mut PyObject> {
    if is_none(object) {
        return Ok(Vec::new());
    }
    let items = object_sequence(object, "changelist")?;
    items
        .iter()
        .copied()
        .map(|item| {
            let item = crate::tag::untag_arg(item);
            if item.is_null() || unsafe { (*item).ob_type } != kevent_type().cast_const() {
                return Err(raise_type_error("changelist must contain select.kevent objects"));
            }
            let event = unsafe { &*item.cast::<PyKevent>() };
            Ok(raw_kevent(event.ident, event.filter, event.flags, event.fflags, event.data, event.udata))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn object_sequence(object: *mut PyObject, what: &str) -> Result<Vec<*mut PyObject>, *mut PyObject> {
    let raw = crate::tag::untag_arg(object);
    if raw.is_null() || crate::tag::is_small_int(raw) {
        return Err(raise_type_error(&format!("{what} must be a list or tuple")));
    }
    match unsafe { crate::types::dict::type_name(raw) } {
        Some("list") => Ok(unsafe { (*raw.cast::<crate::types::list::PyList>()).as_slice() }.to_vec()),
        Some("tuple") => Ok(unsafe { (*raw.cast::<crate::types::tuple::PyTuple>()).as_slice() }.to_vec()),
        _ => Err(raise_type_error(&format!("{what} must be a list or tuple"))),
    }
}

#[cfg(target_os = "macos")]
fn raw_kevent(ident: usize, filter: i16, flags: u16, fflags: u32, data: isize, udata: usize) -> libc::kevent {
    libc::kevent {
        ident: ident as libc::uintptr_t,
        filter,
        flags,
        fflags,
        data: data as libc::intptr_t,
        udata: udata as *mut libc::c_void,
    }
}

#[cfg(target_os = "macos")]
fn kevent_object(ident: usize, filter: i16, flags: u16, fflags: u32, data: isize, udata: usize) -> *mut PyObject {
    Box::into_raw(Box::new(PyKevent {
        ob_base: PyObjectHeader::new(kevent_type()),
        ident,
        filter,
        flags,
        fflags,
        data,
        udata,
    }))
    .cast::<PyObject>()
}

#[cfg(target_os = "macos")]
fn timeout_timespec(object: *mut PyObject) -> Result<Option<libc::timespec>, *mut PyObject> {
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
    let mut nsec = ((value - seconds) * 1_000_000_000.0).ceil() as i64;
    let mut sec = seconds as i64;
    if nsec >= 1_000_000_000 {
        sec += 1;
        nsec -= 1_000_000_000;
    }
    Ok(Some(libc::timespec {
        tv_sec: sec as libc::time_t,
        tv_nsec: nsec as libc::c_long,
    }))
}

#[cfg(target_os = "macos")]
fn i16_arg(object: *mut PyObject, what: &str) -> Result<i16, *mut PyObject> {
    let value = int_arg(object, what)?;
    i16::try_from(value).map_err(|_| raise_value_error(&format!("{what} is out of range")))
}

#[cfg(target_os = "macos")]
fn u16_arg(object: *mut PyObject, what: &str) -> Result<u16, *mut PyObject> {
    let value = int_arg(object, what)?;
    u16::try_from(value).map_err(|_| raise_value_error(&format!("{what} is out of range")))
}

#[cfg(target_os = "macos")]
fn u32_arg(object: *mut PyObject, what: &str) -> Result<u32, *mut PyObject> {
    let value = int_arg(object, what)?;
    u32::try_from(value).map_err(|_| raise_value_error(&format!("{what} is out of range")))
}

#[cfg(target_os = "macos")]
fn isize_arg(object: *mut PyObject, what: &str) -> Result<isize, *mut PyObject> {
    let value = int_arg(object, what)?;
    isize::try_from(value).map_err(|_| raise_value_error(&format!("{what} is out of range")))
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
