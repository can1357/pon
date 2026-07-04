//! Minimal `_tracemalloc` surface for Pon's allocator state.
//!
//! Pon does not currently tag heap allocations with Python traceback metadata,
//! so tracing cannot be enabled faithfully. The observable disabled-state APIs
//! are real (`is_tracing() == False`, zero memory counters, no object
//! traceback); attempts to start tracing raise a precise `NotImplementedError`
//! instead of pretending to collect data.

use std::ptr;

use crate::intern::intern;
use crate::object::PyObject;
use crate::types::exc::ExceptionKind;

use super::builtins_mod::{VARIADIC_ARITY, alloc_list, alloc_tuple};
use super::install_module;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let mut attrs = vec![(intern("__name__"), str_object("_tracemalloc")?)];
    for (name, entry) in [
        ("_get_object_traceback", get_object_traceback_entry as BuiltinFn),
        ("_get_traces", get_traces_entry),
        ("clear_traces", clear_traces_entry),
        ("get_traceback_limit", get_traceback_limit_entry),
        ("get_traced_memory", get_traced_memory_entry),
        ("get_tracemalloc_memory", get_tracemalloc_memory_entry),
        ("is_tracing", is_tracing_entry),
        ("reset_peak", reset_peak_entry),
        ("start", start_entry),
        ("stop", stop_entry),
    ] {
        attrs.push(function_attr(name, entry)?);
    }
    install_module("_tracemalloc", attrs)
}

unsafe extern "C" fn is_tracing_entry(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return raise_type_error("is_tracing() takes no arguments");
    }
    unsafe { crate::abi::number::pon_const_bool(0) }
}

unsafe extern "C" fn get_traceback_limit_entry(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return raise_type_error("get_traceback_limit() takes no arguments");
    }
    unsafe { crate::abi::pon_const_int(1) }
}

unsafe extern "C" fn get_traced_memory_entry(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return raise_type_error("get_traced_memory() takes no arguments");
    }
    let current = unsafe { crate::abi::pon_const_int(0) };
    let peak = unsafe { crate::abi::pon_const_int(0) };
    if current.is_null() || peak.is_null() {
        return ptr::null_mut();
    }
    alloc_tuple(vec![current, peak])
}

unsafe extern "C" fn get_tracemalloc_memory_entry(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return raise_type_error("get_tracemalloc_memory() takes no arguments");
    }
    unsafe { crate::abi::pon_const_int(0) }
}

unsafe extern "C" fn get_traces_entry(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return raise_type_error("_get_traces() takes no arguments");
    }
    alloc_list(Vec::new())
}

unsafe extern "C" fn get_object_traceback_entry(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 {
        return raise_type_error("_get_object_traceback() takes exactly one argument");
    }
    unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn clear_traces_entry(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return raise_type_error("clear_traces() takes no arguments");
    }
    unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn reset_peak_entry(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return raise_type_error("reset_peak() takes no arguments");
    }
    unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn stop_entry(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return raise_type_error("stop() takes no arguments");
    }
    unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn start_entry(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc > 1 {
        return raise_type_error("start() takes at most 1 argument");
    }
    crate::abi::exc::raise_kind_error_text(
        ExceptionKind::NotImplementedError,
        "tracemalloc.start() requires allocation traceback tracking, which Pon's allocator does not yet expose",
    )
}

fn function_attr(name: &str, entry: BuiltinFn) -> Result<(u32, *mut PyObject), String> {
    let function = unsafe { crate::abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    (!function.is_null()).then_some((intern(name), function)).ok_or_else(|| format!("failed to allocate _tracemalloc.{name}"))
}

fn str_object(text: &str) -> Result<*mut PyObject, String> {
    let object = unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) };
    (!object.is_null()).then_some(object).ok_or_else(|| format!("failed to allocate string {text:?}"))
}

fn raise_type_error(message: &str) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}
