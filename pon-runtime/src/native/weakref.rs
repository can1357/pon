//! Native `weakref` module seed.

use crate::intern::intern;
use crate::object::PyObject;

use super::install_module;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    install_module(
        "weakref",
        vec![
            (intern("__name__"), unsafe { crate::abi::pon_const_str(b"weakref".as_ptr(), 7) }),
            (intern("ref"), crate::types::weakref::weakref_ref_type()),
            (intern("ReferenceType"), crate::types::weakref::weakref_ref_type()),
        ],
    )
}
