//! Native `sys` module seed for WS-IMPORT.

use crate::abi::{pon_const_int, pon_const_str};
use crate::intern::intern;
use crate::object::PyObject;

use super::install_module;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let attrs = [
        string_attr("version", "3.14.0 (pon)"),
        string_attr(
            "version_info",
            "sys.version_info(major=3, minor=14, micro=0, releaselevel='final', serial=0)",
        ),
        string_attr("implementation", "namespace(name='pon', version=sys.version_info(major=3, minor=14, micro=0, releaselevel='final', serial=0))"),
        int_attr("hexversion", 0x030e00f0),
        string_attr("platform", std::env::consts::OS),
        string_attr("executable", "pon"),
        string_attr("prefix", ""),
        string_attr("base_prefix", ""),
        string_attr("modules", "<sys.modules>"),
    ];
    install_module("sys", attrs.into_iter().collect::<Result<Vec<_>, _>>()?)
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate sys.{name}"))
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { pon_const_int(value) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate sys.{name}"))
}
