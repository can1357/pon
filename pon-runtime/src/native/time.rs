//! Native `time` module seed for WS-IMPORT.

use crate::abi::{pon_const_int, pon_const_str, pon_make_function, return_null_with_error};
use crate::intern::intern;
use crate::object::PyObject;

use super::install_module;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let attrs = [
        string_attr("__name__", "time"),
        int_attr("timezone", 0),
        tzname_attr(),
        int_attr("daylight", 0),
        int_attr("altzone", 0),
        function_attr("time", time_time),
        function_attr("time_ns", time_time_ns),
        function_attr("sleep", time_sleep),
        function_attr("gmtime", time_gmtime),
        function_attr("localtime", time_localtime),
        function_attr("asctime", time_asctime),
        function_attr("mktime", time_mktime),
        function_attr("strftime", time_strftime),
        function_attr("perf_counter", time_perf_counter),
        function_attr("perf_counter_ns", time_perf_counter_ns),
        function_attr("monotonic", time_monotonic),
        function_attr("monotonic_ns", time_monotonic_ns),
    ];
    let mut attrs = attrs.into_iter().collect::<Result<Vec<_>, _>>()?;
    // `gmtime`/`localtime` construct instances of this class, so a failure
    // here is a loud init error rather than a broken first call (see the
    // `time.struct_time` section comment at the end of this file).
    attrs.push((intern("struct_time"), struct_time_class()?));
    install_module("time", attrs)
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

fn tzname_attr() -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let utc = unsafe { pon_const_str("UTC".as_ptr(), 3) };
    if utc.is_null() {
        return Err("failed to allocate time.tzname element".to_owned());
    }
    let mut items = [utc, utc];
    // SAFETY: `items` is a live window for the duration of the call.
    let tuple = unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) };
    (!tuple.is_null())
        .then_some((intern("tzname"), tuple))
        .ok_or_else(|| "failed to allocate time.tzname".to_owned())
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

/// The nine CPython `struct_time` fields (`tm_year..tm_isdst` in the Python
/// tuple convention: 1-based month/yday, `tm_wday` Monday=0) for `seconds`
/// since the Unix epoch under the pinned `TZ=UTC` environment.
fn utc_fields(seconds: f64) -> [i64; 9] {
    let total = seconds.floor() as i64;
    let days = total.div_euclid(86_400);
    let secs_of_day = total.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let weekday = (days + 3).rem_euclid(7);
    let yday = days - days_from_civil(year, 1, 1) + 1;
    [
        year,
        i64::from(month),
        i64::from(day),
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
        weekday,
        yday,
        0,
    ]
}

/// `time.gmtime([secs])` / `time.localtime([secs])` under the pinned UTC
/// environment: the nine `struct_time` fields (`tm_year..tm_isdst`;
/// `tm_wday` is Monday=0, `tm_isdst` 0 for UTC) as a `time.struct_time`
/// instance.
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
    let fields = utc_fields(seconds);
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
    let values = unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) };
    if values.is_null() {
        return core::ptr::null_mut();
    }
    let class = match struct_time_class() {
        Ok(class) => class,
        Err(message) => return return_null_with_error(message),
    };
    let mut call_argv = [values];
    // SAFETY: The class is a live tuple-derived heap class; calling it
    // routes through `tuple.__new__` construction over the value tuple.
    unsafe { crate::abi::pon_call(class, call_argv.as_mut_ptr(), call_argv.len()) }
}

unsafe extern "C" fn time_gmtime(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { utc_tuple_entry("gmtime", argv, argc) }
}

/// `TZ=UTC` is pinned for every conformance run, so local time IS UTC.
unsafe extern "C" fn time_localtime(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { utc_tuple_entry("localtime", argv, argc) }
}

/// `time.asctime([t])`: the classic C `ctime` text over a nine-field time
/// tuple, defaulting to the current local time (which is UTC here).
unsafe extern "C" fn time_asctime(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc > 1 {
        return raise_type_error(&format!("asctime expected at most 1 argument, got {argc}"));
    }
    let fields = if argc == 0 {
        utc_fields(since_epoch().as_secs_f64())
    } else {
        let tuple = crate::tag::untag_arg(unsafe { *argv });
        if tuple.is_null() {
            return core::ptr::null_mut();
        }
        match time_tuple_fields(
            tuple,
            "Tuple or struct_time argument required",
            "asctime(): illegal time tuple argument",
        ) {
            Ok(fields) => fields,
            Err(error) => return error,
        }
    };
    if let Err(error) = check_year_range(fields[0]) {
        return error;
    }
    let tm = match tm_from_fields(fields) {
        Ok(tm) => tm,
        Err(error) => return error,
    };
    let mut text = String::with_capacity(24);
    render("%a %b %e %H:%M:%S %Y", &tm, &mut text);
    // SAFETY: String allocation helper follows the NULL-sentinel contract.
    unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

/// `time.mktime(t)`: seconds since the Unix epoch for a local-time tuple.
/// With `TZ=UTC` pinned for the conformance runs, this reduces to UTC civil
/// arithmetic plus CPython's forward-only overflow normalization.
unsafe extern "C" fn time_mktime(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 {
        return raise_type_error(&format!("time.mktime() takes exactly one argument ({argc} given)"));
    }
    let tuple = crate::tag::untag_arg(unsafe { *argv });
    if tuple.is_null() {
        return core::ptr::null_mut();
    }
    let fields = match time_tuple_fields(
        tuple,
        "Tuple or struct_time argument required",
        "mktime(): illegal time tuple argument",
    ) {
        Ok(fields) => fields,
        Err(error) => return error,
    };
    let seconds = match mktime_seconds(fields) {
        Ok(seconds) => seconds,
        Err(error) => return error,
    };
    // SAFETY: Float boxing helper follows the NULL-sentinel contract.
    unsafe { crate::abi::number::pon_const_float(seconds as f64) }
}

// ---------------------------------------------------------------------------
// time.strftime
//
// C-locale formatting over the nine-field time tuple.  The conformance
// oracle is CPython 3.14 on macOS, whose strftime is Apple's tzcode
// derivative, so this is a faithful Rust port of that engine rather than a
// call into libc strftime (which would smuggle host locale state into
// differential runs).  Scope notes, all pinned against the oracle:
//
// * C/POSIX locale only: day/month names and the `%c %x %X %r %v %+`
//   compositions are the hard-coded C-locale layouts.
// * `TZ=UTC` is pinned by the conformance runner, so `%Z` is literally
//   "UTC" and `%z` "+0000"; the tuple's `tm_isdst` field is ignored.
// * Unknown conversions drop the `%` and emit the remainder verbatim
//   (`"%4Y"` -> `"4Y"`, `"%q"` -> `"q"`), a trailing lone `%` (optionally
//   with a dangling flag/modifier) survives as its last character, one
//   optional `-`/`_`/`0` padding flag is honored on fixed-width numeric
//   conversions, and one `E`/`O` modifier is accepted and ignored -- all
//   exactly as Apple's engine behaves.  `test.support` probes
//   `strftime('%4Y') != '%4Y'` at import to set `has_strftime_extensions`,
//   which is therefore True here, matching the host oracle.

const DAYS_ABBR: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const DAYS_FULL: [&str; 7] =
    ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS_ABBR: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTHS_FULL: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Normalized time fields in C `struct tm` conventions (0-based month and
/// yday, Sunday=0 weekday), after CPython's `gettmarg` shifts and
/// `time_strftime` range normalization.
struct Tm {
    year: i64,
    mon0: i64,
    mday: i64,
    hour: i64,
    min: i64,
    sec: i64,
    c_wday: i64,
    yday0: i64,
}

impl Tm {
    fn hour12(&self) -> i64 {
        match self.hour % 12 {
            0 => 12,
            h => h,
        }
    }

    /// Seconds since the Unix epoch from the civil fields (`%s`): with
    /// `TZ=UTC` pinned, `mktime` degenerates to pure calendar arithmetic
    /// over the actual year/month/day fields (`tm_yday`/`tm_wday` are
    /// ignored, as in the C engine).
    fn epoch_seconds(&self) -> i64 {
        days_from_civil(self.year, (self.mon0 + 1) as u32, self.mday as u32) * 86_400
            + self.hour * 3600
            + self.min * 60
            + self.sec
    }
}

/// Raises `TypeError` with `message`; always returns NULL.
fn raise_type_error(message: &str) -> *mut PyObject {
    // SAFETY: `message` is a live buffer for the duration of the call.
    unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

/// Raises `ValueError` with `message`; always returns NULL.
fn raise_value_error(message: &str) -> *mut PyObject {
    // SAFETY: `message` is a live buffer for the duration of the call.
    unsafe { crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) }
}

/// Raises `OverflowError` with `message`; always returns NULL.
fn raise_overflow_error(message: &str) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(crate::types::exc::ExceptionKind::OverflowError, message)
}

/// Type name for error messages (callers pass heap-or-NULL post-`untag_arg`;
/// the small-int guard keeps a stray tagged value from being dereferenced).
unsafe fn error_type_name(object: *mut PyObject) -> &'static str {
    if object.is_null() {
        return "NoneType";
    }
    if crate::tag::is_small_int(object) {
        return "int";
    }
    // SAFETY: Heap pointer with a live header per the guards above.
    unsafe { crate::types::dict::type_name(object) }.unwrap_or("object")
}

/// Extracts the text of a `str` (or subclass) argument; `None` otherwise.
unsafe fn text_argument(object: *mut PyObject) -> Option<String> {
    if object.is_null() || crate::tag::is_small_int(object) {
        return None;
    }
    // SAFETY: Heap pointer per the guard above; the type chain is live.
    let mut ty = unsafe { (*object).ob_type };
    while !ty.is_null() {
        // SAFETY: Live type object.
        if unsafe { (*ty).name() } == "str" {
            // SAFETY: A str (sub)type instance carries the PyUnicode layout.
            return unsafe { (*object.cast::<crate::object::PyUnicode>()).as_str() }.map(ToOwned::to_owned);
        }
        // SAFETY: Live type object; the base chain is NULL-terminated.
        ty = unsafe { (*ty).tp_base };
    }
    None
}

/// Parses a `tuple`/`struct_time` argument into the canonical nine integer
/// fields used by `strftime`/`asctime`/`mktime`.
fn time_tuple_fields(
    tuple: *mut PyObject,
    type_error: &str,
    illegal_error: &str,
) -> Result<[i64; 9], *mut PyObject> {
    // SAFETY: Heap-or-NULL after caller-side `untag_arg`; the storage resolver
    // accepts exact tuples and tuple-subclass instances (struct_time shape) and
    // returns `None` for everything else.
    let Some(items) = (unsafe { crate::abi::seq::tuple_storage_slice(tuple) }) else {
        return Err(raise_type_error(type_error));
    };
    if items.len() != 9 {
        return Err(raise_type_error(illegal_error));
    }
    let mut fields = [0i64; 9];
    for (slot, &item) in fields.iter_mut().zip(items) {
        let item = crate::tag::untag_arg(item);
        if item.is_null() {
            return Err(core::ptr::null_mut());
        }
        // SAFETY: Heap-or-NULL after `untag_arg`.
        let parsed = unsafe { crate::types::int::to_bigint_including_bool(item) };
        let Some(parsed) = parsed else {
            return Err(raise_type_error(&format!(
                "'{}' object cannot be interpreted as an integer",
                // SAFETY: As above.
                unsafe { error_type_name(item) }
            )));
        };
        let Some(value) = num_traits::ToPrimitive::to_i64(&parsed) else {
            return Err(raise_overflow_error("Python int too large to convert to C int"));
        };
        *slot = value;
    }
    Ok(fields)
}

/// CPython's year bounds for tuple-based `time` APIs: the tuple carries the
/// actual year, but the C runtime stores it in `struct tm.tm_year`.
fn check_year_range(year: i64) -> Result<(), *mut PyObject> {
    if year > i32::MAX as i64 {
        return Err(raise_overflow_error("signed integer is greater than maximum"));
    }
    if year < i32::MIN as i64 + 1900 {
        return Err(raise_overflow_error("year out of range"));
    }
    Ok(())
}

/// CPython's `gettmarg` field shifts plus `time_strftime`'s normalization
/// and range checks, with its exact `ValueError` messages.
fn tm_from_fields(fields: [i64; 9]) -> Result<Tm, *mut PyObject> {
    let year = fields[0];
    let mut mon0 = fields[1] - 1;
    let mut mday = fields[2];
    let (hour, min, sec) = (fields[3], fields[4], fields[5]);
    // C truncating `%`, exactly `(tm_wday + 1) % 7` in gettmarg.
    let c_wday = (fields[6] + 1) % 7;
    let mut yday0 = fields[7] - 1;
    // fields[8] (tm_isdst) is deliberately unused: TZ=UTC is pinned.
    if mon0 == -1 {
        mon0 = 0;
    } else if !(0..=11).contains(&mon0) {
        return Err(raise_value_error("month out of range"));
    }
    if mday == 0 {
        mday = 1;
    } else if !(0..=31).contains(&mday) {
        return Err(raise_value_error("day of month out of range"));
    }
    if !(0..=23).contains(&hour) {
        return Err(raise_value_error("hour out of range"));
    }
    if !(0..=59).contains(&min) {
        return Err(raise_value_error("minute out of range"));
    }
    if !(0..=61).contains(&sec) {
        return Err(raise_value_error("seconds out of range"));
    }
    if !(0..=6).contains(&c_wday) {
        return Err(raise_value_error("day of week out of range"));
    }
    if yday0 == -1 {
        yday0 = 0;
    } else if !(0..=365).contains(&yday0) {
        return Err(raise_value_error("day of year out of range"));
    }
    Ok(Tm { year, mon0, mday, hour, min, sec, c_wday, yday0 })
}

/// `mktime()`'s UTC-pinned civil arithmetic.  CPython/libc normalize positive
/// month/day/time overflow forward (month 13 -> next January, hour 24 -> next
/// day) but reject underflow legs such as month 0 or hour -1 with the shared
/// `mktime argument out of range` wording.
fn mktime_seconds(fields: [i64; 9]) -> Result<i64, *mut PyObject> {
    let month = i128::from(fields[1]);
    let mday = i128::from(fields[2]);
    let hour = i128::from(fields[3]);
    let min = i128::from(fields[4]);
    let sec = i128::from(fields[5]);
    if month <= 0 || mday <= 0 || hour < 0 || min < 0 || sec < 0 {
        return Err(raise_overflow_error("mktime argument out of range"));
    }

    let month0 = month - 1;
    let year = i128::from(fields[0]) + month0 / 12;
    let year = i64::try_from(year).map_err(|_| {
        if year.is_negative() {
            raise_overflow_error("year out of range")
        } else {
            raise_overflow_error("signed integer is greater than maximum")
        }
    })?;
    check_year_range(year)?;

    let mon0 = (month0 % 12) as u32;
    let total_seconds = hour * 3_600 + min * 60 + sec;
    let day_offset = (mday - 1) + total_seconds / 86_400;
    let second_of_day = total_seconds % 86_400;
    let base_days = i128::from(days_from_civil(year, mon0 + 1, 1));
    let epoch_seconds = (base_days + day_offset) * 86_400 + second_of_day;
    i64::try_from(epoch_seconds).map_err(|_| raise_overflow_error("mktime argument out of range"))
}

/// Padding override parsed from a `-`/`_`/`0` conversion flag.
#[derive(Clone, Copy)]
enum Pad {
    Default,
    Suppress,
    Space,
    Zero,
}

/// Fixed-width decimal conversion with the directive's default fill,
/// honoring a flag override (the tzcode `_conv` + FreeBSD padding table).
fn conv(value: i64, width: usize, default_fill: char, pad: Pad, out: &mut String) {
    use std::fmt::Write as _;
    let fill = match pad {
        Pad::Default => default_fill,
        Pad::Suppress => {
            let _ = write!(out, "{value}");
            return;
        }
        Pad::Space => ' ',
        Pad::Zero => '0',
    };
    let _ = if fill == '0' { write!(out, "{value:0width$}") } else { write!(out, "{value:width$}") };
}

/// tzcode's `_yconv`: splits `year` into century and year-of-century so
/// `%Y` is effectively 4-digit zero-padded for 0..=9999 while five-digit
/// and negative years match the C engine (`12345` -> "12345", `-100` ->
/// "-100").
fn yconv(year: i64, century: bool, two_digit: bool) -> String {
    use std::fmt::Write as _;
    // Split as `(year - 1900) + 1900` with C truncating division, exactly
    // like the engine's `_yconv(t->tm_year, TM_YEAR_BASE, ..)` call.
    let a = year - 1900;
    let mut trail = a % 100 + 1900 % 100;
    let mut lead = a / 100 + 1900 / 100 + trail / 100;
    trail %= 100;
    if trail < 0 && lead > 0 {
        trail += 100;
        lead -= 1;
    } else if lead < 0 && trail > 0 {
        trail -= 100;
        lead += 1;
    }
    let mut out = String::new();
    if century {
        if lead == 0 && trail < 0 {
            out.push_str("-0");
        } else {
            let _ = write!(out, "{lead:02}");
        }
    }
    if two_digit {
        let _ = write!(out, "{:02}", trail.abs());
    }
    out
}

fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// ISO 8601 week-based year and week number (`%G`/`%g`/`%V`), computed from
/// `tm_year`/`tm_yday`/`tm_wday` exactly like the tzcode engine.
fn iso_week(tm: &Tm) -> (i64, i64) {
    let mut year = tm.year;
    let mut yday = tm.yday0;
    loop {
        let len = if is_leap(year) { 366 } else { 365 };
        // What yday (-3 ..= 3) does the ISO year begin on?
        let bot = ((yday + 11 - tm.c_wday) % 7) - 3;
        // What yday does the NEXT ISO year begin on?
        let mut top = bot - (len % 7);
        if top < -3 {
            top += 7;
        }
        top += len;
        if yday >= top {
            return (year + 1, 1);
        }
        if yday >= bot {
            return (year, 1 + (yday - bot) / 7);
        }
        year -= 1;
        yday += if is_leap(year) { 366 } else { 365 };
    }
}

/// One conversion character, post flag/modifier parsing.  Unknown
/// conversions emit the character itself (the `%` was already dropped).
fn emit(directive: char, pad: Pad, tm: &Tm, out: &mut String) {
    use std::fmt::Write as _;
    match directive {
        'a' => out.push_str(DAYS_ABBR[tm.c_wday as usize]),
        'A' => out.push_str(DAYS_FULL[tm.c_wday as usize]),
        'b' | 'h' => out.push_str(MONTHS_ABBR[tm.mon0 as usize]),
        'B' => out.push_str(MONTHS_FULL[tm.mon0 as usize]),
        'c' => render("%a %b %d %H:%M:%S %Y", tm, out),
        'C' => out.push_str(&yconv(tm.year, true, false)),
        'd' => conv(tm.mday, 2, '0', pad, out),
        'D' | 'x' => render("%m/%d/%y", tm, out),
        'e' => conv(tm.mday, 2, ' ', pad, out),
        'F' => render("%Y-%m-%d", tm, out),
        'g' => out.push_str(&yconv(iso_week(tm).0, false, true)),
        'G' => out.push_str(&yconv(iso_week(tm).0, true, true)),
        'H' => conv(tm.hour, 2, '0', pad, out),
        'I' => conv(tm.hour12(), 2, '0', pad, out),
        'j' => conv(tm.yday0 + 1, 3, '0', pad, out),
        'k' => conv(tm.hour, 2, ' ', pad, out),
        'l' => conv(tm.hour12(), 2, ' ', pad, out),
        'm' => conv(tm.mon0 + 1, 2, '0', pad, out),
        'M' => conv(tm.min, 2, '0', pad, out),
        'n' => out.push('\n'),
        'p' => out.push_str(if tm.hour >= 12 { "PM" } else { "AM" }),
        'r' => render("%I:%M:%S %p", tm, out),
        'R' => render("%H:%M", tm, out),
        's' => {
            let _ = write!(out, "{}", tm.epoch_seconds());
        }
        'S' => conv(tm.sec, 2, '0', pad, out),
        't' => out.push('\t'),
        'T' | 'X' => render("%H:%M:%S", tm, out),
        'u' => conv(if tm.c_wday == 0 { 7 } else { tm.c_wday }, 1, '0', pad, out),
        'U' => conv((tm.yday0 + 7 - tm.c_wday) / 7, 2, '0', pad, out),
        'v' => render("%e-%b-%Y", tm, out),
        'V' => conv(iso_week(tm).1, 2, '0', pad, out),
        'w' => conv(tm.c_wday, 1, '0', pad, out),
        'W' => conv((tm.yday0 + 7 - ((tm.c_wday + 6) % 7)) / 7, 2, '0', pad, out),
        'y' => out.push_str(&yconv(tm.year, false, true)),
        'Y' => out.push_str(&yconv(tm.year, true, true)),
        'z' => out.push_str("+0000"),
        'Z' => out.push_str("UTC"),
        '+' => render("%a %b %e %H:%M:%S %Z %Y", tm, out),
        '%' => out.push('%'),
        other => out.push(other),
    }
}

/// Apple's `strftime` accepts a bare decimal field width for `%y`/`%Y`
/// (`%3y` -> `025`, `%4Y` -> `0001`).  Other directives keep the historical
/// "drop `%`, keep the remainder verbatim" fallback.
fn emit_year_width(directive: char, pad: Pad, width: usize, tm: &Tm, out: &mut String) {
    let text = match directive {
        'y' => yconv(tm.year, false, true),
        'Y' => yconv(tm.year, true, true),
        other => {
            out.push_str(&width.to_string());
            emit(other, pad, tm, out);
            return;
        }
    };
    if matches!(pad, Pad::Suppress) || text.len() >= width {
        out.push_str(&text);
        return;
    }
    let fill = match pad {
        Pad::Default | Pad::Zero => '0',
        Pad::Space => ' ',
        Pad::Suppress => unreachable!(),
    };
    for _ in 0..(width - text.len()) {
        out.push(fill);
    }
    out.push_str(&text);
}

/// The format walker: literal text, `%%`, and `% [-_0]? [EO]? conv` specs.
/// A spec cut off by end-of-string emits its last consumed character
/// verbatim (`"abc%"` -> `"abc%"`, `"a%-"` -> `"a-"`, `"%E"` -> `"E"`),
/// matching the Apple engine.
fn render(format: &str, tm: &Tm, out: &mut String) {
    let chars: Vec<char> = format.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(&next) = chars.get(i) else {
            out.push('%');
            break;
        };
        let mut cursor = next;
        i += 1;
        let mut pad = Pad::Default;
        if matches!(cursor, '-' | '_' | '0') {
            pad = match cursor {
                '-' => Pad::Suppress,
                '_' => Pad::Space,
                _ => Pad::Zero,
            };
            match chars.get(i) {
                Some(&after_flag) => {
                    cursor = after_flag;
                    i += 1;
                }
                None => {
                    out.push(cursor);
                    break;
                }
            }
        }
        if matches!(cursor, 'E' | 'O') {
            match chars.get(i) {
                Some(&after_modifier) => {
                    cursor = after_modifier;
                    i += 1;
                }
                None => {
                    out.push(cursor);
                    break;
                }
            }
        }
        if matches!(pad, Pad::Default) && cursor.is_ascii_digit() {
            let mut width_text = String::from(cursor);
            while let Some(&digit) = chars.get(i) {
                if !digit.is_ascii_digit() {
                    break;
                }
                width_text.push(digit);
                i += 1;
            }
            let Some(&after_width) = chars.get(i) else {
                out.push_str(&width_text);
                break;
            };
            cursor = after_width;
            i += 1;
            let width = width_text.parse::<usize>().unwrap_or(0);
            emit_year_width(cursor, pad, width, tm, out);
            continue;
        }
        emit(cursor, pad, tm, out);
    }
}

/// `time.strftime(format[, t])`: C-locale formatting of a nine-field time
/// tuple (default: the current time, which is UTC under the pinned
/// environment), with CPython's exact argument errors.
unsafe extern "C" fn time_strftime(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc == 0 {
        return raise_type_error("strftime() takes at least 1 argument (0 given)");
    }
    if argc > 2 {
        return raise_type_error(&format!("strftime() takes at most 2 arguments ({argc} given)"));
    }
    // SAFETY: The call helper supplies `argc` live argument slots.
    let format_obj = crate::tag::untag_arg(unsafe { *argv });
    if format_obj.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: Heap-or-NULL after `untag_arg`.
    let Some(format) = (unsafe { text_argument(format_obj) }) else {
        return raise_type_error(&format!(
            "strftime() argument 1 must be str, not {}",
            // SAFETY: As above.
            unsafe { error_type_name(format_obj) }
        ));
    };
    let fields = if argc == 2 {
        let tuple = crate::tag::untag_arg(unsafe { *argv.add(1) });
        if tuple.is_null() {
            return core::ptr::null_mut();
        }
        match time_tuple_fields(
            tuple,
            "Tuple or struct_time argument required",
            "strftime(): illegal time tuple argument",
        ) {
            Ok(fields) => fields,
            Err(error) => return error,
        }
    } else {
        utc_fields(since_epoch().as_secs_f64())
    };
    let tm = match tm_from_fields(fields) {
        Ok(tm) => tm,
        Err(raised) => return raised,
    };
    let mut text = String::with_capacity(format.len() * 2);
    render(&format, &tm, &mut text);
    // SAFETY: String allocation helper follows the NULL-sentinel contract.
    unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

// ---------------------------------------------------------------------------
// time.struct_time
//
// CPython's `time.struct_time` is a structseq (a tuple subclass with named
// read-only fields) defined by the C `time` module; `gmtime`/`localtime`
// construct it and `strftime` accepts it (`test_strftime` reads `tm_year`
// off `localtime()`'s result, and `logging.Formatter.formatTime` feeds the
// value back into `strftime`).  pon builds the same shape through the
// tuple-embedding heap-class machinery — mirroring `os.terminal_size` and
// `sys.version_info` — with `tm_year = property(self[0])`-style getters and
// the CPython structseq repr.
//
// Deliberate divergences from the C structseq, pinned by consumers: the
// constructor is inherited `tuple.__new__` (any iterable of any length is
// accepted; CPython requires a 9..=11-item sequence), and the extended
// non-sequence fields `tm_zone`/`tm_gmtoff` — plus the `n_fields`/
// `n_sequence_fields`/`n_unnamed_fields` class ints that would promise
// them — are absent: a tuple-embedding instance cannot hold 11 slots while
// reporting `len() == 9`, and under the pinned `TZ=UTC` environment no
// vendored consumer reads them.

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

/// The nine named sequence fields, in tuple order.
const STRUCT_TIME_FIELDS: [&str; 9] =
    ["tm_year", "tm_mon", "tm_mday", "tm_hour", "tm_min", "tm_sec", "tm_wday", "tm_yday", "tm_isdst"];

/// Getter entry points aligned with [`STRUCT_TIME_FIELDS`].
const STRUCT_TIME_GETTERS: [BuiltinFn; 9] = [
    struct_time_tm_year,
    struct_time_tm_mon,
    struct_time_tm_mday,
    struct_time_tm_hour,
    struct_time_tm_min,
    struct_time_tm_sec,
    struct_time_tm_wday,
    struct_time_tm_yday,
    struct_time_tm_isdst,
];

/// CPython's `time.struct_time.__doc__`, verbatim (the oracle prints it).
const STRUCT_TIME_DOC: &str = "The time value as returned by gmtime(), localtime(), and strptime(), and\n accepted by asctime(), mktime() and strftime().  May be considered as a\n sequence of 9 integers.\n\n Note that several fields' values are not the same as those defined by\n the C language standard for struct tm.  For example, the value of the\n field tm_year is the actual year, not year - 1900.  See individual\n fields' descriptions for details.";

/// The `time.struct_time` class, built once and reused across import
/// re-registration (a CPython static type has the same lifetime).
fn struct_time_class() -> Result<*mut PyObject, String> {
    static CLASS: std::sync::LazyLock<Result<usize, String>> =
        std::sync::LazyLock::new(|| build_struct_time_class().map(|class| class as usize));
    CLASS.clone().map(|class| class as *mut PyObject)
}

/// `class struct_time(tuple)` with the CPython structseq surface: field
/// properties reading `self[i]` and the `time.struct_time(...)` repr.
fn build_struct_time_class() -> Result<*mut PyObject, String> {
    // SAFETY: `pon_load_global` returns NULL with a raised NameError on miss.
    let tuple_class = unsafe { crate::abi::pon_load_global(intern("tuple"), std::ptr::null_mut()) };
    if tuple_class.is_null() {
        crate::thread_state::pon_err_clear();
        return Err("builtin 'tuple' is not registered for time.struct_time".to_owned());
    }
    // SAFETY: Same contract for the builtin `property` constructor.
    let property_class = unsafe { crate::abi::pon_load_global(intern("property"), std::ptr::null_mut()) };
    if property_class.is_null() {
        crate::thread_state::pon_err_clear();
        return Err("builtin 'property' is not registered for time.struct_time".to_owned());
    }
    let namespace = crate::types::type_::new_namespace();
    if namespace.is_null() {
        return Err("failed to allocate the time.struct_time namespace".to_owned());
    }
    class_str_attr(namespace, "__module__", "time")?;
    class_str_attr(namespace, "__doc__", STRUCT_TIME_DOC)?;
    class_function_attr(namespace, "__repr__", struct_time_repr)?;
    for (index, name) in STRUCT_TIME_FIELDS.iter().enumerate() {
        // SAFETY: Live builtin entry point with the runtime calling convention.
        let fget = unsafe { pon_make_function(STRUCT_TIME_GETTERS[index] as *const u8, 1, intern(name)) };
        if fget.is_null() {
            return Err(format!("failed to allocate time.struct_time.{name} getter"));
        }
        let mut argv = [fget];
        // SAFETY: The builtin `property` class is callable with one fget slot.
        let descriptor = unsafe { crate::abi::pon_call(property_class, argv.as_mut_ptr(), argv.len()) };
        if descriptor.is_null() {
            let detail = crate::thread_state::pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
            crate::thread_state::pon_err_clear();
            return Err(format!("failed to build time.struct_time.{name} property: {detail}"));
        }
        // SAFETY: `new_namespace` returned a live namespace box.
        unsafe { (&mut *namespace).set(intern(name), descriptor) };
    }
    // SAFETY: The base is the live builtin `tuple` class object.
    let class = unsafe {
        crate::types::type_::build_class_from_namespace("struct_time", &[tuple_class], namespace, &[])
    };
    if class.is_null() {
        let detail = crate::thread_state::pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
        crate::thread_state::pon_err_clear();
        return Err(format!("failed to create time.struct_time: {detail}"));
    }
    // SAFETY: Freshly built class object owned by this module build; mirror
    // `pon_build_class`'s ob_type fix-up for a metaclass-less construction.
    unsafe {
        if (*class).ob_type.is_null() {
            (*class).ob_type = crate::abi::runtime_type_type().cast_const();
        }
    }
    Ok(class)
}

/// Seeds a str-valued class attribute into a class namespace under build.
fn class_str_attr(
    namespace: *mut crate::types::type_::PyClassDict,
    name: &str,
    value: &str,
) -> Result<(), String> {
    // SAFETY: String allocation helper; NULL is checked below.
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    if object.is_null() {
        return Err(format!("failed to allocate time.struct_time attribute '{name}'"));
    }
    // SAFETY: The caller passes a live namespace box.
    unsafe { (&mut *namespace).set(intern(name), object) };
    Ok(())
}

/// Seeds a native-function class attribute into a class namespace under build.
fn class_function_attr(
    namespace: *mut crate::types::type_::PyClassDict,
    name: &str,
    entry: BuiltinFn,
) -> Result<(), String> {
    // SAFETY: Live builtin entry point with the runtime calling convention.
    let function =
        unsafe { pon_make_function(entry as *const u8, crate::builtins::variadic_arity(), intern(name)) };
    if function.is_null() {
        return Err(format!("failed to allocate time.struct_time method '{name}'"));
    }
    // SAFETY: The caller passes a live namespace box.
    unsafe { (&mut *namespace).set(intern(name), function) };
    Ok(())
}

/// Borrows the argv slots as a slice; NULL argv reads as empty.
unsafe fn call_args<'a>(argv: *mut *mut PyObject, argc: usize) -> &'a [*mut PyObject] {
    if argv.is_null() || argc == 0 {
        &[]
    } else {
        // SAFETY: The caller passed `argc` live argument slots.
        unsafe { std::slice::from_raw_parts(argv, argc) }
    }
}

/// Element `index` of a struct_time receiver, as stored (heap-or-NULL after
/// untagging).  CPython's structseq getters return the stored object, so a
/// user-constructed `struct_time(('a', ...))` reads back `'a'` — no int
/// coercion here.
fn struct_time_element(
    args: &[*mut PyObject],
    index: usize,
    what: &str,
) -> Result<*mut PyObject, *mut PyObject> {
    if args.len() != 1 {
        return Err(return_null_with_error(format!("{what} expected only a receiver")));
    }
    // SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
    let key = unsafe { pon_const_int(index as i64) };
    if key.is_null() {
        return Err(std::ptr::null_mut());
    }
    // SAFETY: Subscript dispatch resolves the tuple-embedded layout.
    let element = unsafe { crate::abstract_op::subscript_get(args[0], key) };
    if element.is_null() {
        return Err(std::ptr::null_mut());
    }
    let element = crate::tag::untag_arg(element);
    if element.is_null() {
        return Err(std::ptr::null_mut());
    }
    Ok(element)
}

/// Shared property-getter core: `self[index]`.
unsafe fn struct_time_field(
    argv: *mut *mut PyObject,
    argc: usize,
    index: usize,
    what: &str,
) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    match struct_time_element(args, index, what) {
        Ok(element) => element,
        Err(error) => error,
    }
}

unsafe extern "C" fn struct_time_tm_year(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded slots per the runtime calling convention.
    unsafe { struct_time_field(argv, argc, 0, "struct_time.tm_year") }
}

unsafe extern "C" fn struct_time_tm_mon(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded slots per the runtime calling convention.
    unsafe { struct_time_field(argv, argc, 1, "struct_time.tm_mon") }
}

unsafe extern "C" fn struct_time_tm_mday(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded slots per the runtime calling convention.
    unsafe { struct_time_field(argv, argc, 2, "struct_time.tm_mday") }
}

unsafe extern "C" fn struct_time_tm_hour(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded slots per the runtime calling convention.
    unsafe { struct_time_field(argv, argc, 3, "struct_time.tm_hour") }
}

unsafe extern "C" fn struct_time_tm_min(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded slots per the runtime calling convention.
    unsafe { struct_time_field(argv, argc, 4, "struct_time.tm_min") }
}

unsafe extern "C" fn struct_time_tm_sec(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded slots per the runtime calling convention.
    unsafe { struct_time_field(argv, argc, 5, "struct_time.tm_sec") }
}

unsafe extern "C" fn struct_time_tm_wday(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded slots per the runtime calling convention.
    unsafe { struct_time_field(argv, argc, 6, "struct_time.tm_wday") }
}

unsafe extern "C" fn struct_time_tm_yday(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded slots per the runtime calling convention.
    unsafe { struct_time_field(argv, argc, 7, "struct_time.tm_yday") }
}

unsafe extern "C" fn struct_time_tm_isdst(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded slots per the runtime calling convention.
    unsafe { struct_time_field(argv, argc, 8, "struct_time.tm_isdst") }
}

/// CPython's structseq repr:
/// `time.struct_time(tm_year=1970, tm_mon=1, ..., tm_isdst=0)`, with
/// element reprs (user-constructed instances may hold non-ints).
unsafe extern "C" fn struct_time_repr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    let mut text = String::from("time.struct_time(");
    for (index, name) in STRUCT_TIME_FIELDS.iter().enumerate() {
        let element = match struct_time_element(args, index, "struct_time.__repr__") {
            Ok(element) => element,
            Err(error) => return error,
        };
        if index > 0 {
            text.push_str(", ");
        }
        text.push_str(name);
        text.push('=');
        text.push_str(&super::builtins_mod::repr_text(element));
    }
    text.push(')');
    // SAFETY: String allocation helper follows the NULL-sentinel contract.
    unsafe { pon_const_str(text.as_ptr(), text.len()) }
}
