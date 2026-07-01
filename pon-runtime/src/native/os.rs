//! Native `os` module seed for WS-IMPORT.

use crate::abi::pon_const_str;
use crate::intern::intern;
use crate::object::PyObject;

use super::install_module;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let sep = if cfg!(windows) { "\\" } else { "/" };
    let linesep = if cfg!(windows) { "\r\n" } else { "\n" };
    let attrs = [
        string_attr("__name__", "os"),
        string_attr("name", os_name()),
        string_attr("sep", sep),
        string_attr("pathsep", if cfg!(windows) { ";" } else { ":" }),
        string_attr("linesep", linesep),
        string_attr("curdir", "."),
        string_attr("pardir", ".."),
    ];
    install_module("os", attrs.into_iter().collect::<Result<Vec<_>, _>>()?)
}

fn os_name() -> &'static str {
    if cfg!(windows) {
        "nt"
    } else {
        "posix"
    }
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate os.{name}"))
}
