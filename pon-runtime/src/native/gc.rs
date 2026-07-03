//! Native `gc` module seed for deterministic conformance collection.

use crate::intern::intern;
use crate::object::PyObject;

use super::install_module;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    install_module(
        "gc",
        vec![
            (intern("__name__"), unsafe { crate::abi::pon_const_str(b"gc".as_ptr(), 2) }),
            (intern("collect"), unsafe { crate::abi::pon_make_function(gc_collect as *const u8, 0, intern("collect")) }),
        ],
    )
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
