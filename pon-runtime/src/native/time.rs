//! Native `time` module seed for WS-IMPORT.

use crate::abi::{pon_const_int, pon_const_str};
use crate::intern::intern;
use crate::object::PyObject;

use super::install_module;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let attrs = [
        string_attr("__name__", "time"),
        string_attr("timezone", "0"),
        string_attr("tzname", "('UTC', 'UTC')"),
        int_attr("daylight", 0),
        int_attr("altzone", 0),
    ];
    install_module("time", attrs.into_iter().collect::<Result<Vec<_>, _>>()?)
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate time.{name}"))
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { pon_const_int(value) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate time.{name}"))
}
