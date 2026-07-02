//! Native `time` module seed for WS-IMPORT.

use crate::abi::{pon_const_int, pon_const_str, pon_make_function, return_null_with_error};
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
        function_attr("perf_counter", time_perf_counter),
    ];
    install_module("time", attrs.into_iter().collect::<Result<Vec<_>, _>>()?)
}

/// `time.perf_counter()`: monotonic clock with an arbitrary (process-start)
/// reference point — only differences are meaningful, exactly CPython's
/// contract.
unsafe extern "C" fn time_perf_counter(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return return_null_with_error(format!("perf_counter() takes no arguments ({argc} given)"));
    }
    static ANCHOR: std::sync::LazyLock<std::time::Instant> = std::sync::LazyLock::new(std::time::Instant::now);
    let seconds = ANCHOR.elapsed().as_secs_f64();
    unsafe { crate::abi::number::pon_const_float(seconds) }
}

fn function_attr(
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { pon_make_function(entry as *const u8, crate::builtins::variadic_arity(), intern(name)) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate time.{name}"))
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
