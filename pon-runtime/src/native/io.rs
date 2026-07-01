//! Native `_io` module seed for WS-IMPORT.

use crate::abi::pon_const_str;
use crate::intern::intern;
use crate::object::PyObject;

use super::install_module;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let attrs = [
        string_attr("__name__", "_io"),
        string_attr("DEFAULT_BUFFER_SIZE", "8192"),
        string_attr("TextIOWrapper", "<class '_io.TextIOWrapper'>"),
        string_attr("FileIO", "<class '_io.FileIO'>"),
        string_attr("stdout", "<stdout>"),
    ];
    install_module("_io", attrs.into_iter().collect::<Result<Vec<_>, _>>()?)
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate _io.{name}"))
}
