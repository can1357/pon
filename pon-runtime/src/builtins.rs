//! Phase-A builtins exposed through the normal `PyFunction` ABI.

use std::ptr;

use crate::abi::pon_print;
use crate::intern::intern;
use crate::object::PyObject;
use crate::thread_state::pon_err_set;

/// Interned global name for the Phase-A `print` builtin.
#[must_use]
pub fn print_name_interned() -> u32 {
    intern("print")
}

/// Trampoline used by the builtin `print` function object.
///
/// Phase A supports the one-argument form required by generated `hello.py`.
pub unsafe extern "C" fn print_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 {
        pon_err_set(format!("print() expected 1 argument, got {argc}"));
        return ptr::null_mut();
    }
    if argv.is_null() {
        pon_err_set("print() received a null argv pointer");
        return ptr::null_mut();
    }

    // SAFETY: The caller supplied at least one argument by contract above.
    let value = unsafe { *argv };
    // SAFETY: `pon_print` is the public C ABI helper for printing one object.
    unsafe { pon_print(value) }
}
