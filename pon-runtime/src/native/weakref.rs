//! Native `weakref` module seed.

use crate::intern::intern;
use crate::object::PyObject;

use super::install_module;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    make_module_named("weakref")
}

pub(super) fn make_underscore_module() -> Result<*mut PyObject, String> {
    make_module_named("_weakref")
}

fn make_module_named(name: &'static str) -> Result<*mut PyObject, String> {
    install_module(
        name,
        vec![
            (intern("__name__"), unsafe { crate::abi::pon_const_str(name.as_ptr(), name.len()) }),
            (intern("ref"), crate::types::weakref::weakref_ref_type()),
            (intern("ReferenceType"), crate::types::weakref::weakref_ref_type()),
        ],
    )
}
