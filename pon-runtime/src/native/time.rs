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
        function_attr("time", time_time),
        function_attr("time_ns", time_time_ns),
        function_attr("sleep", time_sleep),
        function_attr("gmtime", time_gmtime),
        function_attr("localtime", time_localtime),
        function_attr("perf_counter", time_perf_counter),
        function_attr("perf_counter_ns", time_perf_counter_ns),
        function_attr("monotonic", time_monotonic),
        function_attr("monotonic_ns", time_monotonic_ns),
    ];
    install_module("time", attrs.into_iter().collect::<Result<Vec<_>, _>>()?)
}

/// Process-start epoch shared by the monotonic clock family: CPython's
/// contract is an arbitrary reference point where only differences are
/// meaningful, and a single anchor keeps `monotonic()`, `perf_counter()` and
/// their `_ns` variants mutually coherent.
static ANCHOR: std::sync::LazyLock<std::time::Instant> = std::sync::LazyLock::new(std::time::Instant::now);

/// Shared zero-argument clock core: seconds as float, or nanoseconds as int,
/// since [`ANCHOR`].
unsafe fn clock_entry(name: &str, argc: usize, nanos: bool) -> *mut PyObject {
    if argc != 0 {
        return return_null_with_error(format!("{name}() takes no arguments ({argc} given)"));
    }
    if nanos {
        // i64 nanoseconds overflow ~292 years after process start.
        unsafe { pon_const_int(ANCHOR.elapsed().as_nanos() as i64) }
    } else {
        unsafe { crate::abi::number::pon_const_float(ANCHOR.elapsed().as_secs_f64()) }
    }
}

/// `time.perf_counter()`: monotonic clock with an arbitrary (process-start)
/// reference point — only differences are meaningful, exactly CPython's
/// contract.
unsafe extern "C" fn time_perf_counter(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { clock_entry("perf_counter", argc, false) }
}

unsafe extern "C" fn time_perf_counter_ns(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { clock_entry("perf_counter_ns", argc, true) }
}

/// `time.monotonic()`: same clock and anchor as `perf_counter` (CPython also
/// serves both from one monotonic OS clock).
unsafe extern "C" fn time_monotonic(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { clock_entry("monotonic", argc, false) }
}

unsafe extern "C" fn time_monotonic_ns(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { clock_entry("monotonic_ns", argc, true) }
}

/// Wall-clock duration since the Unix epoch (`time.time`/`time.time_ns`
/// source; the system clock CPython reads through `clock_gettime`).
fn since_epoch() -> std::time::Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
}

/// `time.time()`: seconds since the Unix epoch as a float.
unsafe extern "C" fn time_time(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return return_null_with_error(format!("time() takes no arguments ({argc} given)"));
    }
    unsafe { crate::abi::number::pon_const_float(since_epoch().as_secs_f64()) }
}

/// `time.time_ns()`: nanoseconds since the Unix epoch as an int (`logging`
/// stamps `_startTime` with it at import).
unsafe extern "C" fn time_time_ns(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return return_null_with_error(format!("time_ns() takes no arguments ({argc} given)"));
    }
    // i64 nanoseconds cover the epoch range until the year 2262.
    unsafe { pon_const_int(since_epoch().as_nanos() as i64) }
}

/// `time.sleep(seconds)`: real suspension of the calling OS thread.
unsafe extern "C" fn time_sleep(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 || argv.is_null() {
        return return_null_with_error(format!("sleep() takes exactly 1 argument ({argc} given)"));
    }
    // SAFETY: The call helper supplies `argv` with at least one entry.
    let value = crate::tag::untag_arg(unsafe { *argv });
    if value.is_null() {
        return core::ptr::null_mut();
    }
    let seconds = if let Some(seconds) = unsafe { crate::types::float::to_f64(value) } {
        Some(seconds)
    } else {
        unsafe { crate::types::int::to_bigint_including_bool(value) }
            .and_then(|value| num_traits::ToPrimitive::to_f64(&value))
    };
    let Some(seconds) = seconds else {
        return return_null_with_error("sleep() argument must be a number");
    };
    if seconds < 0.0 {
        return return_null_with_error("sleep length must be non-negative");
    }
    std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
    // SAFETY: Singleton accessor.
    unsafe { crate::abi::pon_none() }
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

/// Civil date from days since the Unix epoch (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Days since the Unix epoch for a civil date (inverse of
/// [`civil_from_days`]).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = (year - era * 400) as u64;
    let doy = (153 * u64::from(if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + u64::from(day - 1);
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// `time.gmtime([secs])` / `time.localtime([secs])` under the pinned UTC
/// environment: the nine `struct_time` fields as a plain tuple
/// (`tm_year..tm_isdst`; `tm_wday` is Monday=0, `tm_isdst` 0 for UTC).
unsafe fn utc_tuple_entry(name: &str, argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc > 1 {
        return return_null_with_error(format!("{name}() takes at most 1 argument ({argc} given)"));
    }
    let mut seconds = since_epoch().as_secs_f64();
    if argc == 1 && !argv.is_null() {
        // SAFETY: The call helper supplies `argv` with at least one entry.
        let value = crate::tag::untag_arg(unsafe { *argv });
        if value.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: Singleton accessor.
        if value != unsafe { crate::abi::pon_none() } {
            let parsed = if let Some(parsed) = unsafe { crate::types::float::to_f64(value) } {
                Some(parsed)
            } else {
                unsafe { crate::types::int::to_bigint_including_bool(value) }
                    .and_then(|value| num_traits::ToPrimitive::to_f64(&value))
            };
            let Some(parsed) = parsed else {
                return return_null_with_error(format!("{name}() argument must be a number or None"));
            };
            seconds = parsed;
        }
    }
    let total = seconds.floor() as i64;
    let days = total.div_euclid(86_400);
    let secs_of_day = total.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let weekday = (days + 3).rem_euclid(7);
    let yday = days - days_from_civil(year, 1, 1) + 1;
    let fields = [
        year,
        i64::from(month),
        i64::from(day),
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
        weekday,
        yday,
        0,
    ];
    let mut items = Vec::with_capacity(fields.len());
    for field in fields {
        // SAFETY: Allocation helper; NULL is checked immediately.
        let object = unsafe { pon_const_int(field) };
        if object.is_null() {
            return core::ptr::null_mut();
        }
        items.push(object);
    }
    // SAFETY: `items` is a live window for the duration of the call.
    unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

unsafe extern "C" fn time_gmtime(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { utc_tuple_entry("gmtime", argv, argc) }
}

/// `TZ=UTC` is pinned for every conformance run, so local time IS UTC.
unsafe extern "C" fn time_localtime(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { utc_tuple_entry("localtime", argv, argc) }
}
