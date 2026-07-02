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
    match crate::abi::collect() {
        Ok(()) => unsafe { crate::abi::pon_const_int(0) },
        Err(message) => crate::abi::return_null_with_error(message),
    }
}
