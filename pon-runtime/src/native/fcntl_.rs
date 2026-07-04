//! Native `fcntl` module seed backed by host `flock(2)`.
//!
//! meson's `mesonbuild.utils.posix.DirectoryLock` guards build directories
//! with `fcntl.flock(lockfile, LOCK_EX | LOCK_NB)`, so Pon exposes the lock
//! constants, the portable `F_*` command constants, and a real `flock` with
//! CPython's fd coercion (an int, or any object with a `fileno()` method).
//! `fcntl`/`lockf`/`ioctl` are deliberately absent until real code exercises
//! them: `from fcntl import ioctl` failing loudly beats a subtly wrong wrapper.

use crate::abi::{pon_call, pon_const_int, pon_const_str, pon_make_function};
use crate::intern::intern;
use crate::object::PyObject;
use crate::types::exc::ExceptionKind;

use super::install_module;

const VARIADIC_ARITY: usize = crate::native::builtins_mod::VARIADIC_ARITY;

/// Constants shared by macOS and Linux, sorted by name.  The lock flags are
/// the surface meson reads at import; the `F_*` rows cover the common
/// `fcntl(2)` command vocabulary CPython always exposes.
const CONSTANTS: &[(&str, i64)] = &[
    ("FD_CLOEXEC", libc::FD_CLOEXEC as i64),
    ("F_DUPFD", libc::F_DUPFD as i64),
    ("F_DUPFD_CLOEXEC", libc::F_DUPFD_CLOEXEC as i64),
    ("F_GETFD", libc::F_GETFD as i64),
    ("F_GETFL", libc::F_GETFL as i64),
    ("F_GETLK", libc::F_GETLK as i64),
    ("F_GETOWN", libc::F_GETOWN as i64),
    ("F_RDLCK", libc::F_RDLCK as i64),
    ("F_SETFD", libc::F_SETFD as i64),
    ("F_SETFL", libc::F_SETFL as i64),
    ("F_SETLK", libc::F_SETLK as i64),
    ("F_SETLKW", libc::F_SETLKW as i64),
    ("F_SETOWN", libc::F_SETOWN as i64),
    ("F_UNLCK", libc::F_UNLCK as i64),
    ("F_WRLCK", libc::F_WRLCK as i64),
    ("LOCK_EX", libc::LOCK_EX as i64),
    ("LOCK_NB", libc::LOCK_NB as i64),
    ("LOCK_SH", libc::LOCK_SH as i64),
    ("LOCK_UN", libc::LOCK_UN as i64),
];

#[cfg(target_os = "macos")]
const OS_CONSTANTS: &[(&str, i64)] = &[
    ("F_FULLFSYNC", libc::F_FULLFSYNC as i64),
    ("F_GETPATH", libc::F_GETPATH as i64),
    ("F_NOCACHE", libc::F_NOCACHE as i64),
];

#[cfg(target_os = "linux")]
const OS_CONSTANTS: &[(&str, i64)] = &[
    ("F_OFD_GETLK", libc::F_OFD_GETLK as i64),
    ("F_OFD_SETLK", libc::F_OFD_SETLK as i64),
    ("F_OFD_SETLKW", libc::F_OFD_SETLKW as i64),
];

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "fcntl";
    // SAFETY: Runtime allocation helper; NULL is checked below.
    let name_obj = unsafe { pon_const_str(name.as_ptr(), name.len()) };
    if name_obj.is_null() {
        return Err("failed to allocate fcntl.__name__".to_owned());
    }
    let mut attrs = vec![(intern("__name__"), name_obj)];
    for &(const_name, value) in CONSTANTS.iter().chain(OS_CONSTANTS) {
        // SAFETY: Runtime allocation helper; NULL is checked below.
        let object = unsafe { pon_const_int(value) };
        if object.is_null() {
            return Err(format!("failed to allocate fcntl.{const_name}"));
        }
        attrs.push((intern(const_name), object));
    }
    // SAFETY: Live builtin entry point with the runtime calling convention.
    let flock = unsafe { pon_make_function(fcntl_flock as *const u8, VARIADIC_ARITY, intern("flock")) };
    if flock.is_null() {
        return Err("failed to allocate fcntl.flock".to_owned());
    }
    attrs.push((intern("flock"), flock));
    install_module(name, attrs)
}

/// `fcntl.flock(fd, operation)`: host `flock(2)` with an EINTR retry,
/// raising the PEP 3151 OSError subclass on failure (`LOCK_NB` conflicts
/// surface as `BlockingIOError` through the shared errno mapping).
unsafe extern "C" fn fcntl_flock(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 2 || argv.is_null() {
        return raise_type_error(&format!("flock expected 2 arguments, got {argc}"));
    }
    // SAFETY: The caller passed two live argument slots.
    let args = unsafe { std::slice::from_raw_parts(argv, argc) };
    let fd = match fd_arg(args[0]) {
        Ok(fd) => fd,
        Err(raised) => return raised,
    };
    let operation = match int_arg(args[1], "operation") {
        Ok(value) => value as libc::c_int,
        Err(raised) => return raised,
    };
    loop {
        // SAFETY: Plain syscall; the kernel validates the descriptor.
        if unsafe { libc::flock(fd, operation) } == 0 {
            return unsafe { crate::abi::pon_none() };
        }
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO);
        if errno != libc::EINTR {
            return super::os::raise_errno(errno, None);
        }
    }
}

/// CPython `PyObject_AsFileDescriptor`: an int is the descriptor itself;
/// otherwise `fileno()` is called and must return an int.
fn fd_arg(object: *mut PyObject) -> Result<libc::c_int, *mut PyObject> {
    if let Ok(fd) = int_arg(object, "fd") {
        return c_int_range(fd);
    }
    crate::thread_state::pon_err_clear();
    let raw = crate::tag::untag_arg(object);
    if raw.is_null() {
        return Err(std::ptr::null_mut());
    }
    // SAFETY: Attribute dispatch follows the NULL-sentinel error contract.
    let method = unsafe { crate::abstract_op::get_attr(raw, intern("fileno")) };
    if method.is_null() {
        crate::thread_state::pon_err_clear();
        return Err(raise_type_error("argument must be an int, or have a fileno() method."));
    }
    // SAFETY: `pon_call` self-normalizes callee and dispatches by target kind.
    let result = unsafe { pon_call(method, std::ptr::null_mut(), 0) };
    if result.is_null() {
        return Err(std::ptr::null_mut());
    }
    match int_arg(result, "fd") {
        Ok(fd) => c_int_range(fd),
        Err(_) => {
            crate::thread_state::pon_err_clear();
            Err(raise_type_error("fileno() returned a non-integer"))
        }
    }
}

fn c_int_range(fd: i64) -> Result<libc::c_int, *mut PyObject> {
    if fd < i64::from(i32::MIN) || fd > i64::from(i32::MAX) {
        return Err(raise_value_error("file descriptor is out of range"));
    }
    Ok(fd as libc::c_int)
}

fn int_arg(object: *mut PyObject, what: &str) -> Result<i64, *mut PyObject> {
    if crate::tag::is_small_int(object) {
        return Ok(crate::tag::untag_small_int(object));
    }
    let raw = crate::tag::untag_arg(object);
    if raw.is_null() {
        return Err(std::ptr::null_mut());
    }
    // SAFETY: Heap pointer with a live header after the tag checks.
    match unsafe { crate::types::int::to_bigint_including_bool(raw) } {
        Some(value) => num_traits::ToPrimitive::to_i64(&value)
            .ok_or_else(|| raise_value_error(&format!("{what} is too large"))),
        None => Err(raise_type_error(&format!("{what} must be an integer"))),
    }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

fn raise_value_error(message: &str) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(ExceptionKind::ValueError, message)
}
