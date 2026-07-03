//! Minimal native shims for packaging's platform-tag helper modules.
//!
//! `packaging.tags` imports `packaging._manylinux` and `packaging._musllinux`
//! unconditionally, even on platforms that never consult their tag generators.
//! Pon serves lightweight modules that expose the expected `platform_tags`
//! callable so pure-Python build backends can import `packaging.tags` on macOS.

use crate::intern::intern;
use crate::object::PyObject;

use super::install_module;

fn empty_list() -> *mut PyObject {
    unsafe { crate::abi::seq::pon_build_list(std::ptr::null_mut(), 0) }
}

unsafe extern "C" fn platform_tags_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    empty_list()
}

fn make_module(name: &'static str) -> Result<*mut PyObject, String> {
    let name_object = unsafe { crate::abi::pon_const_str(name.as_ptr(), name.len()) };
    if name_object.is_null() {
        return Err(format!("failed to allocate {name}.__name__"));
    }
    let function = unsafe {
        crate::abi::pon_make_function(
            platform_tags_entry as *const u8,
            crate::native::builtins_mod::VARIADIC_ARITY,
            intern("platform_tags"),
        )
    };
    if function.is_null() {
        return Err(format!("failed to allocate {name}.platform_tags"));
    }
    install_module(name, [(intern("__name__"), name_object), (intern("platform_tags"), function)])
}

pub(super) fn make_manylinux_module() -> Result<*mut PyObject, String> {
    make_module("packaging._manylinux")
}

pub(super) fn make_musllinux_module() -> Result<*mut PyObject, String> {
    make_module("packaging._musllinux")
}
