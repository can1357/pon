//! Native `_posixshmem` backed by POSIX `shm_open(2)` / `shm_unlink(2)`.
//!
//! The module is intentionally small because CPython exposes only these two
//! syscalls. Returned file descriptors are marked close-on-exec to match
//! CPython's non-inheritable descriptor policy.

use std::ffi::CString;

use num_traits::ToPrimitive;

use crate::intern::intern;
use crate::object::PyObject;
use crate::types::exc::ExceptionKind;
use crate::types::{dict, int, type_};

use super::builtins_mod::VARIADIC_ARITY;
use super::install_module;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let mut attrs = vec![(intern("__name__"), str_object("_posixshmem")?)];
    attrs.push(function_attr("shm_open", shm_open_entry)?);
    attrs.push(function_attr("shm_unlink", shm_unlink_entry)?);
    install_module("_posixshmem", attrs)
}

unsafe extern "C" fn shm_open_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() || !(2..=3).contains(&argc) {
        return raise_type_error(if argc < 2 {
            "shm_open() missing required argument 'flags' (pos 2)"
        } else {
            "shm_open() takes at most 3 arguments"
        });
    }
    let args = unsafe { std::slice::from_raw_parts(argv, argc) };
    let path = match path_arg(crate::tag::untag_arg(args[0]), "shm_open() argument 1 must be str, not") {
        Ok(path) => path,
        Err(error) => return error,
    };
    let flags = match int_arg(crate::tag::untag_arg(args[1]), "shm_open() argument 2 must be int, not") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let mode = if argc == 3 {
        match int_arg(crate::tag::untag_arg(args[2]), "shm_open() argument 3 must be int, not") {
            Ok(value) => value,
            Err(error) => return error,
        }
    } else {
        0o777
    };

    let c_path = match CString::new(path.as_str()) {
        Ok(path) => path,
        Err(_) => return crate::abi::exc::raise_kind_error_text(ExceptionKind::ValueError, "embedded null character"),
    };
    let fd = unsafe { libc::shm_open(c_path.as_ptr(), flags as libc::c_int, mode as libc::c_uint) };
    if fd < 0 {
        return crate::native::os::raise_errno(last_errno(), Some(&path));
    }
    if let Err(errno) = set_fd_cloexec(fd) {
        unsafe { libc::close(fd) };
        return crate::native::os::raise_errno(errno, Some(&path));
    }
    unsafe { crate::abi::pon_const_int(i64::from(fd)) }
}

unsafe extern "C" fn shm_unlink_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() || argc != 1 {
        return raise_type_error("shm_unlink() takes exactly one argument");
    }
    let path = match path_arg(crate::tag::untag_arg(unsafe { *argv }), "shm_unlink() argument must be str, not") {
        Ok(path) => path,
        Err(error) => return error,
    };
    let c_path = match CString::new(path.as_str()) {
        Ok(path) => path,
        Err(_) => return crate::abi::exc::raise_kind_error_text(ExceptionKind::ValueError, "embedded null character"),
    };
    if unsafe { libc::shm_unlink(c_path.as_ptr()) } != 0 {
        return crate::native::os::raise_errno(last_errno(), Some(&path));
    }
    unsafe { crate::abi::pon_none() }
}

fn path_arg(object: *mut PyObject, prefix: &str) -> Result<String, *mut PyObject> {
    match unsafe { type_::unicode_text(object) } {
        Some(text) => Ok(text.to_owned()),
        None => Err(raise_type_error(&format!("{prefix} {}", type_name(object)))),
    }
}

fn int_arg(object: *mut PyObject, prefix: &str) -> Result<i64, *mut PyObject> {
    let Some(value) = (unsafe { int::to_bigint_including_bool(object) }) else {
        return Err(raise_type_error(&format!("{prefix} {}", type_name(object))));
    };
    value.to_i64().ok_or_else(|| crate::abi::exc::raise_kind_error_text(ExceptionKind::OverflowError, "Python int too large to convert to C int"))
}

fn set_fd_cloexec(fd: libc::c_int) -> Result<(), i32> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(last_errno());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(last_errno());
    }
    Ok(())
}

fn last_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}

fn function_attr(name: &str, entry: BuiltinFn) -> Result<(u32, *mut PyObject), String> {
    let function = unsafe { crate::abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    (!function.is_null()).then_some((intern(name), function)).ok_or_else(|| format!("failed to allocate _posixshmem.{name}"))
}

fn str_object(text: &str) -> Result<*mut PyObject, String> {
    let object = unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) };
    (!object.is_null()).then_some(object).ok_or_else(|| format!("failed to allocate string {text:?}"))
}

fn type_name(object: *mut PyObject) -> &'static str {
    unsafe { dict::type_name(object) }.unwrap_or("object")
}

fn raise_type_error(message: &str) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}
