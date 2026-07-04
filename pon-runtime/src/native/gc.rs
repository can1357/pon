//! Native `gc` module seed for deterministic conformance collection.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::intern::intern;
use crate::object::PyObject;

use super::install_module;

/// Automatic-collection enabled flag (CPython `gc.enable`/`gc.disable`/
/// `gc.isenabled`).  pon collects only on explicit `gc.collect()`, so the flag
/// is pure state that stdlib callers (`timeit`, Cython inline, atexit hooks)
/// read and toggle; it never gates the manual collector.
static GC_ENABLED: AtomicBool = AtomicBool::new(true);

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    install_module(
        "gc",
        vec![
            (intern("__name__"), unsafe { crate::abi::pon_const_str(b"gc".as_ptr(), 2) }),
            (intern("collect"), unsafe { crate::abi::pon_make_function(gc_collect as *const u8, 0, intern("collect")) }),
            (intern("enable"), unsafe { crate::abi::pon_make_function(gc_enable as *const u8, 0, intern("enable")) }),
            (intern("disable"), unsafe { crate::abi::pon_make_function(gc_disable as *const u8, 0, intern("disable")) }),
            (intern("isenabled"), unsafe { crate::abi::pon_make_function(gc_isenabled as *const u8, 0, intern("isenabled")) }),
        ],
    )
}

unsafe extern "C" fn gc_enable(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return crate::abi::return_null_with_error("gc.enable expected no arguments");
    }
    GC_ENABLED.store(true, Ordering::Relaxed);
    unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn gc_disable(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return crate::abi::return_null_with_error("gc.disable expected no arguments");
    }
    GC_ENABLED.store(false, Ordering::Relaxed);
    unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn gc_isenabled(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return crate::abi::return_null_with_error("gc.isenabled expected no arguments");
    }
    unsafe { crate::abi::number::pon_const_bool(i32::from(GC_ENABLED.load(Ordering::Relaxed))) }
}

unsafe extern "C" fn gc_collect(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return crate::abi::return_null_with_error("gc.collect expected no arguments");
    }
    // Scrub before any collection frame is pushed: `abi::collect` re-scrubs,
    // but its own wrapper frame would otherwise sit in unscrubbed territory
    // still holding ghosts of the previous deep call chain (see
    // `abi::scrub_dead_stack_below`).  Scrubbing from the native entry point
    // pushes the ghost boundary up to this frame.
    crate::abi::scrub_dead_stack_below();
    match crate::abi::collect() {
        Ok(()) => unsafe { crate::abi::pon_const_int(0) },
        Err(message) => crate::abi::return_null_with_error(message),
    }
}
