//! Native `math` module (HANDOFF Track L, J0.4 lazy registry row).
//!
//! f64-backed CPython 3.14 parity. Algorithms and error semantics are ported
//! from `Modules/mathmodule.c` at tag v3.14.0:
//!
//! * one-argument libm wrappers follow `math_1` (NaN from non-NaN input is a
//!   `ValueError` domain violation carrying the 3.14 per-function message,
//!   infinity from a finite input is `OverflowError('math range error')` when
//!   the function can overflow);
//! * `gamma`/`lgamma` are the Lanczos ports CPython carries to work around
//!   libm quality issues (gh-70309); `erf`/`erfc` call the system libm just
//!   like 3.14 does;
//! * `fsum` is Shewchuk partials, `hypot`/`dist` use the scaled
//!   Neumaier-corrected `vector_norm`, and `sumprod` uses the Ogita-Rump-Oishi
//!   triple-length accumulation, all bit-for-bit ports;
//! * `factorial`/`comb`/`perm`/`gcd`/`lcm`/`isqrt` are exact BigInt
//!   arithmetic through `types::int`, and `floor`/`ceil`/`trunc` return exact
//!   ints (dispatching `__floor__`/`__ceil__`/`__trunc__` for non-floats).
//!
//! Keyword-only parameters (`isclose`, `nextafter`, `prod`) arrive pre-bound
//! to a fixed positional shape with None filling absent slots
//! (`bind_optional_named_keywords` in `types::function`).

use core::ffi::c_int;
use std::ptr;

use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{One, Signed, ToPrimitive, Zero};

use crate::abi::{self, pon_call, pon_get_iter, pon_iter_next};
use crate::abstract_op::{self, BINARY_ADD, BINARY_MUL};
use crate::intern::intern;
use crate::object::PyObject;
use crate::thread_state::{pon_err_clear, thread_state_lock};
use crate::types::exc::ExceptionKind;
use crate::types::float::{from_f64, repr_f64, to_f64};
use crate::types::int::{from_bigint, is_exact_int, to_bigint, to_bigint_including_bool};
use crate::types::{bool_, dict};

use super::builtins_mod::VARIADIC_ARITY;
use super::install_module;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

// System libm entry points CPython 3.14 also calls directly: `erf`/`erfc`
// (FUNC1A rows) and the `frexp`/`ldexp` pair. Linking against libm is part of
// the default Rust target runtime on every supported host.
unsafe extern "C" {
    #[link_name = "erf"]
    fn libm_erf(x: f64) -> f64;
    #[link_name = "erfc"]
    fn libm_erfc(x: f64) -> f64;
    #[link_name = "frexp"]
    fn libm_frexp(x: f64, exp: *mut c_int) -> f64;
    #[link_name = "ldexp"]
    fn libm_ldexp(x: f64, exp: c_int) -> f64;
}

// ---------------------------------------------------------------------------
// Module factory

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name_value = "math";
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let name_object = unsafe { abi::pon_const_str(name_value.as_ptr(), name_value.len()) };
    if name_object.is_null() {
        return Err("failed to allocate math.__name__".to_owned());
    }
    let mut attrs = vec![(intern("__name__"), name_object)];
    let constants: [(&str, f64); 5] = [
        ("pi", std::f64::consts::PI),
        ("e", std::f64::consts::E),
        ("tau", std::f64::consts::TAU),
        ("inf", f64::INFINITY),
        ("nan", f64::NAN),
    ];
    for (name, value) in constants {
        let object = from_f64(value);
        if object.is_null() {
            return Err(format!("failed to allocate math.{name}"));
        }
        attrs.push((intern(name), object));
    }
    let functions: [(&str, BuiltinFn); 57] = [
        ("acos", math_acos),
        ("acosh", math_acosh),
        ("asin", math_asin),
        ("asinh", math_asinh),
        ("atan", math_atan),
        ("atan2", math_atan2),
        ("atanh", math_atanh),
        ("cbrt", math_cbrt),
        ("ceil", math_ceil),
        ("comb", math_comb),
        ("copysign", math_copysign),
        ("cos", math_cos),
        ("cosh", math_cosh),
        ("degrees", math_degrees),
        ("dist", math_dist),
        ("erf", math_erf),
        ("erfc", math_erfc),
        ("exp", math_exp),
        ("exp2", math_exp2),
        ("expm1", math_expm1),
        ("fabs", math_fabs),
        ("factorial", math_factorial),
        ("floor", math_floor),
        ("fma", math_fma),
        ("fmod", math_fmod),
        ("frexp", math_frexp),
        ("fsum", math_fsum),
        ("gamma", math_gamma),
        ("gcd", math_gcd),
        ("hypot", math_hypot),
        ("isclose", math_isclose),
        ("isfinite", math_isfinite),
        ("isinf", math_isinf),
        ("isnan", math_isnan),
        ("isqrt", math_isqrt),
        ("lcm", math_lcm),
        ("ldexp", math_ldexp),
        ("lgamma", math_lgamma),
        ("log", math_log),
        ("log10", math_log10),
        ("log1p", math_log1p),
        ("log2", math_log2),
        ("modf", math_modf),
        ("nextafter", math_nextafter),
        ("perm", math_perm),
        ("pow", math_pow),
        ("prod", math_prod),
        ("radians", math_radians),
        ("remainder", math_remainder),
        ("sin", math_sin),
        ("sinh", math_sinh),
        ("sqrt", math_sqrt),
        ("sumprod", math_sumprod),
        ("tan", math_tan),
        ("tanh", math_tanh),
        ("trunc", math_trunc),
        ("ulp", math_ulp),
    ];
    for (name, entry) in functions {
        // SAFETY: `entry` is a live builtin entry point with the runtime calling convention.
        let object = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
        if object.is_null() {
            return Err(format!("failed to allocate math.{name}"));
        }
        attrs.push((intern(name), object));
    }
    install_module(name_value, attrs)
}

// ---------------------------------------------------------------------------
// Shared small helpers

fn untag(object: *mut PyObject) -> *mut PyObject {
    crate::tag::untag_arg(object)
}

/// Collects the raw (untagged) argument window of a builtin call.
unsafe fn arg_vec(argv: *mut *mut PyObject, argc: usize) -> Vec<*mut PyObject> {
    if argv.is_null() {
        return Vec::new();
    }
    // SAFETY: The caller passes a live argv window of length argc.
    let raw = unsafe { core::slice::from_raw_parts(argv, argc) };
    raw.iter().copied().map(untag).collect()
}

/// Raises a typed builtin exception carrying the diagnostic text — unless a
/// live boxed exception is already pending, which stays authoritative,
/// mirroring `pon_err_set`'s preserve discipline.
fn raise(kind: ExceptionKind, message: &str) -> *mut PyObject {
    if crate::abi::exc::pending_exception_object().is_some() {
        return ptr::null_mut();
    }
    crate::abi::exc::raise_kind_error_text(kind, message)
}

fn raise_type(message: &str) -> *mut PyObject {
    raise(ExceptionKind::TypeError, message)
}

fn raise_value(message: &str) -> *mut PyObject {
    raise(ExceptionKind::ValueError, message)
}

fn raise_overflow(message: &str) -> *mut PyObject {
    raise(ExceptionKind::OverflowError, message)
}

fn type_name(object: *mut PyObject) -> &'static str {
    // SAFETY: `type_name` tolerates NULL and untyped objects.
    unsafe { dict::type_name(object) }.unwrap_or("object")
}

fn is_none(object: *mut PyObject) -> bool {
    type_name(object) == "NoneType"
}

/// Attribute lookup that swallows the AttributeError raised for a miss.
unsafe fn try_get_attr(object: *mut PyObject, name: &str) -> Option<*mut PyObject> {
    // SAFETY: `get_attr` dispatches through the receiver's tp_getattro.
    let result = unsafe { abstract_op::get_attr(object, intern(name)) };
    if result.is_null() {
        // Clears the AttributeError left by the failed lookup.
        pon_err_clear();
        return None;
    }
    Some(untag(result))
}

/// CPython `PyFloat_AsDouble` shape: exact floats and ints (plus bool) are
/// converted directly; other objects go through their `__float__`; anything
/// else is the 3.14 `TypeError`. Ints wider than the double range raise
/// `OverflowError` exactly like `PyLong_AsDouble`.
unsafe fn coerce_f64(object: *mut PyObject) -> Result<f64, *mut PyObject> {
    // SAFETY: Type probes tolerate any live object.
    if let Some(value) = unsafe { to_f64(object) } {
        return Ok(value);
    }
    // SAFETY: Same contract as above.
    if let Some(value) = unsafe { bool_::to_bool(object) } {
        return Ok(if value { 1.0 } else { 0.0 });
    }
    // SAFETY: Same contract as above.
    if let Some(value) = unsafe { to_bigint(object) } {
        let converted = value.to_f64().unwrap_or(f64::INFINITY);
        if converted.is_infinite() {
            return Err(raise_overflow("int too large to convert to float"));
        }
        return Ok(converted);
    }
    // SAFETY: `try_get_attr` clears the miss diagnostic; `pon_call` reports its own errors.
    if let Some(method) = unsafe { try_get_attr(object, "__float__") } {
        // SAFETY: Bound method invoked with zero arguments.
        let result = untag(unsafe { pon_call(method, ptr::null_mut(), 0) });
        if result.is_null() {
            return Err(ptr::null_mut());
        }
        // SAFETY: Result probe tolerates any live object.
        if let Some(value) = unsafe { to_f64(result) } {
            return Ok(value);
        }
        return Err(raise_type(&format!(
            "{}.__float__ returned non-float (type {})",
            type_name(object),
            type_name(result)
        )));
    }
    Err(raise_type(&format!("must be real number, not {}", type_name(object))))
}

/// CPython `_PyNumber_Index` shape: exact ints and bools convert directly,
/// other objects go through the `nb_index` slot / `__index__`; everything
/// else raises the 3.14 `TypeError`.
unsafe fn coerce_index(object: *mut PyObject) -> Result<BigInt, *mut PyObject> {
    // SAFETY: Type probes tolerate any live object.
    if let Some(value) = unsafe { to_bigint_including_bool(object) } {
        return Ok(value);
    }
    // SAFETY: Live object headers reached from a call argument.
    let slot = unsafe {
        object
            .as_ref()
            .and_then(|object| object.ob_type.as_ref())
            .and_then(|ty| ty.tp_as_number.as_ref())
            .and_then(|methods| methods.nb_index)
    };
    if let Some(slot) = slot {
        // SAFETY: nb_index receives its own receiver.
        let result = unsafe { slot(object) };
        if result.is_null() {
            return Err(ptr::null_mut());
        }
        // SAFETY: Result probe tolerates any live object.
        if let Some(value) = unsafe { to_bigint_including_bool(untag(result)) } {
            return Ok(value);
        }
    } else if let Some(method) = unsafe { try_get_attr(object, "__index__") } {
        // SAFETY: Bound method invoked with zero arguments.
        let result = untag(unsafe { pon_call(method, ptr::null_mut(), 0) });
        if result.is_null() {
            return Err(ptr::null_mut());
        }
        // SAFETY: Result probe tolerates any live object.
        if let Some(value) = unsafe { to_bigint_including_bool(result) } {
            return Ok(value);
        }
    }
    Err(raise_type(&format!(
        "'{}' object cannot be interpreted as an integer",
        type_name(object)
    )))
}

/// Fixed-arity argument window: exactly `expected` positional arguments.
unsafe fn fixed_args<const N: usize>(
    argv: *mut *mut PyObject,
    argc: usize,
    name: &str,
) -> Result<[*mut PyObject; N], *mut PyObject> {
    // SAFETY: Caller passes a live argv window.
    let args = unsafe { arg_vec(argv, argc) };
    if args.len() != N {
        let plural = if N == 1 { "argument" } else { "arguments" };
        return Err(raise_type(&format!("{name}() takes exactly {N} {plural} ({argc} given)")));
    }
    let mut out = [ptr::null_mut(); N];
    out.copy_from_slice(&args);
    Ok(out)
}

enum NextItem {
    Value(*mut PyObject),
    Stop,
    Error,
}

unsafe fn current_exception_is(name: &str) -> bool {
    let current = thread_state_lock().current_exc;
    if current.is_null() || current == core::ptr::NonNull::<PyObject>::dangling().as_ptr() {
        return false;
    }
    // SAFETY: A live current exception has a live type.
    let ty = unsafe { (*current).ob_type };
    !ty.is_null() && unsafe { (*ty).name() == name }
}

/// Pulls one item, normalizing tagged immediates so callers may dereference.
unsafe fn next_item(iter: *mut PyObject) -> NextItem {
    // SAFETY: `pon_iter_next` self-normalizes its argument.
    let value = unsafe { pon_iter_next(iter, ptr::null_mut()) };
    if !value.is_null() {
        return NextItem::Value(untag(value));
    }
    // SAFETY: Distinguishes exhaustion from a raised error.
    if unsafe { current_exception_is("StopIteration") } {
        // Consumes the StopIteration terminator.
        pon_err_clear();
        return NextItem::Stop;
    }
    if crate::abi::exc::pending_exception_object().is_some() {
        return NextItem::Error;
    }
    NextItem::Stop
}

fn tuple2(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    let mut items = [a, b];
    // SAFETY: `items` is a live window for the duration of the call.
    unsafe { abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

// ---------------------------------------------------------------------------
// math_1 / math_2 wrappers (Modules/mathmodule.c error discipline)

/// Formats a 3.14 per-function domain message: `%s` receives `repr(x)` with
/// `.0` appended for integral values (`PyOS_double_to_string(x, 'r', 0,
/// Py_DTSF_ADD_DOT_0)`), which is exactly `repr_f64`.
fn domain_error(err_msg: Option<&str>, x: f64) -> *mut PyObject {
    match err_msg {
        Some(template) => raise_value(&template.replace("%s", &repr_f64(x))),
        None => raise_value("math domain error"),
    }
}

/// `math_1`: NaN from non-NaN input is a domain violation; infinity from a
/// finite input overflows (`can_overflow`) or is a singularity.
unsafe fn math_1(
    argv: *mut *mut PyObject,
    argc: usize,
    name: &str,
    can_overflow: bool,
    err_msg: Option<&str>,
    func: impl Fn(f64) -> f64,
) -> *mut PyObject {
    // SAFETY: Caller passes a live argv window.
    let [arg] = match unsafe { fixed_args::<1>(argv, argc, name) } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: `arg` is a live untagged object.
    let x = match unsafe { coerce_f64(arg) } {
        Ok(x) => x,
        Err(error) => return error,
    };
    let r = func(x);
    if r.is_nan() && !x.is_nan() {
        return domain_error(err_msg, x);
    }
    if r.is_infinite() && x.is_finite() {
        if can_overflow {
            return raise_overflow("math range error");
        }
        return domain_error(err_msg, x);
    }
    from_f64(r)
}

/// Result channel for the `math_1a` ports (`gamma`/`lgamma`) that mirror the
/// C `errno` protocol: `Dom` is EDOM, `Range` is ERANGE.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Errno {
    Ok,
    Dom,
    Range,
}

/// `math_1a`: the wrapped function reports EDOM/ERANGE itself.
unsafe fn math_1a(
    argv: *mut *mut PyObject,
    argc: usize,
    name: &str,
    err_msg: Option<&str>,
    func: impl Fn(f64) -> (f64, Errno),
) -> *mut PyObject {
    // SAFETY: Caller passes a live argv window.
    let [arg] = match unsafe { fixed_args::<1>(argv, argc, name) } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: `arg` is a live untagged object.
    let x = match unsafe { coerce_f64(arg) } {
        Ok(x) => x,
        Err(error) => return error,
    };
    let (r, errno) = func(x);
    match errno {
        Errno::Ok => from_f64(r),
        Errno::Dom => domain_error(err_msg, x),
        // `is_error`: ERANGE with |r| < 1.5 is a suppressed underflow.
        Errno::Range if r.abs() < 1.5 => from_f64(r),
        Errno::Range => raise_overflow("math range error"),
    }
}

/// `math_2`: NaN from non-NaN inputs is EDOM (`ValueError`), infinity from
/// finite inputs is ERANGE (`OverflowError`).
unsafe fn math_2(argv: *mut *mut PyObject, argc: usize, name: &str, func: impl Fn(f64, f64) -> f64) -> *mut PyObject {
    // SAFETY: Caller passes a live argv window.
    let [a, b] = match unsafe { fixed_args::<2>(argv, argc, name) } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: Arguments are live untagged objects.
    let x = match unsafe { coerce_f64(a) } {
        Ok(x) => x,
        Err(error) => return error,
    };
    // SAFETY: Same contract as above.
    let y = match unsafe { coerce_f64(b) } {
        Ok(y) => y,
        Err(error) => return error,
    };
    let r = func(x, y);
    if r.is_nan() && !x.is_nan() && !y.is_nan() {
        return raise_value("math domain error");
    }
    if r.is_infinite() && x.is_finite() && y.is_finite() {
        return raise_overflow("math range error");
    }
    from_f64(r)
}

macro_rules! func1 {
    ($entry:ident, $pyname:literal, $can_overflow:expr, $err:expr, $f:expr) => {
        unsafe extern "C" fn $entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
            // SAFETY: Runtime builtin calling convention passes a live argv window.
            unsafe { math_1(argv, argc, $pyname, $can_overflow, $err, $f) }
        }
    };
}

macro_rules! func2 {
    ($entry:ident, $pyname:literal, $f:expr) => {
        unsafe extern "C" fn $entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
            // SAFETY: Runtime builtin calling convention passes a live argv window.
            unsafe { math_2(argv, argc, $pyname, $f) }
        }
    };
}

// FUNC1/FUNC1D rows, 3.14 messages verbatim.
func1!(math_acos, "acos", false, Some("expected a number in range from -1 up to 1, got %s"), f64::acos);
func1!(math_acosh, "acosh", false, Some("expected argument value not less than 1, got %s"), f64::acosh);
func1!(math_asin, "asin", false, Some("expected a number in range from -1 up to 1, got %s"), f64::asin);
func1!(math_asinh, "asinh", false, None, f64::asinh);
func1!(math_atan, "atan", false, None, f64::atan);
func1!(math_atanh, "atanh", false, Some("expected a number between -1 and 1, got %s"), f64::atanh);
func1!(math_cbrt, "cbrt", false, None, f64::cbrt);
func1!(math_cos, "cos", false, Some("expected a finite input, got %s"), f64::cos);
func1!(math_cosh, "cosh", true, None, f64::cosh);
func1!(math_exp, "exp", true, None, f64::exp);
func1!(math_exp2, "exp2", true, None, f64::exp2);
func1!(math_expm1, "expm1", true, None, f64::exp_m1);
func1!(math_fabs, "fabs", false, None, f64::abs);
func1!(math_log1p, "log1p", false, Some("expected argument value > -1, got %s"), f64::ln_1p);
func1!(math_sin, "sin", false, Some("expected a finite input, got %s"), f64::sin);
func1!(math_sinh, "sinh", true, None, f64::sinh);
func1!(math_sqrt, "sqrt", false, Some("expected a nonnegative input, got %s"), f64::sqrt);
func1!(math_tan, "tan", false, Some("expected a finite input, got %s"), f64::tan);
func1!(math_tanh, "tanh", false, None, f64::tanh);
func1!(math_degrees, "degrees", false, None, |x| x * (180.0 / std::f64::consts::PI));
func1!(math_radians, "radians", false, None, |x| x * (std::f64::consts::PI / 180.0));
// FUNC1A rows: the system libm, exactly like 3.14. Neither function can
// report EDOM/ERANGE in a way `is_error` would surface (erfc underflow is
// suppressed by the |r| < 1.5 rule), so the plain wrapper fits.
func1!(math_erf, "erf", false, None, |x| {
    // SAFETY: Pure libm function.
    libm_erf(x)
});
func1!(math_erfc, "erfc", false, None, |x| {
    // SAFETY: Pure libm function.
    libm_erfc(x)
});

// FUNC2 rows.
func2!(math_atan2, "atan2", f64::atan2);
func2!(math_copysign, "copysign", f64::copysign);
func2!(math_remainder, "remainder", m_remainder);

unsafe extern "C" fn math_gamma(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    unsafe { math_1a(argv, argc, "gamma", Some("expected a noninteger or positive integer, got %s"), m_tgamma) }
}

unsafe extern "C" fn math_lgamma(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    unsafe { math_1a(argv, argc, "lgamma", Some("expected a noninteger or positive integer, got %s"), m_lgamma) }
}

// ---------------------------------------------------------------------------
// Gamma family (Lanczos ports, mathmodule.c lines 189-523)

const LANCZOS_N: usize = 13;
const LANCZOS_G: f64 = 6.024680040776729583740234375;
const LANCZOS_G_MINUS_HALF: f64 = 5.524680040776729583740234375;
#[rustfmt::skip]
const LANCZOS_NUM_COEFFS: [f64; LANCZOS_N] = [
    23531376880.410759688572007674451636754734846804940,
    42919803642.649098768957899047001988850926355848959,
    35711959237.355668049440185451547166705960488635843,
    17921034426.037209699919755754458931112671403265390,
    6039542586.3520280050642916443072979210699388420708,
    1439720407.3117216736632230727949123939715485786772,
    248874557.86205415651146038641322942321632125127801,
    31426415.585400194380614231628318205362874684987640,
    2876370.6289353724412254090516208496135991145378768,
    186056.26539522349504029498971604569928220784236328,
    8071.6720023658162106380029022722506138218516325024,
    210.82427775157934587250973392071336271166969580291,
    2.5066282746310002701649081771338373386264310793408,
];
#[rustfmt::skip]
const LANCZOS_DEN_COEFFS: [f64; LANCZOS_N] = [
    0.0, 39916800.0, 120543840.0, 150917976.0, 105258076.0, 45995730.0,
    13339535.0, 2637558.0, 357423.0, 32670.0, 1925.0, 66.0, 1.0,
];
const NGAMMA_INTEGRAL: usize = 23;
#[rustfmt::skip]
const GAMMA_INTEGRAL: [f64; NGAMMA_INTEGRAL] = [
    1.0, 1.0, 2.0, 6.0, 24.0, 120.0, 720.0, 5040.0, 40320.0, 362880.0,
    3628800.0, 39916800.0, 479001600.0, 6227020800.0, 87178291200.0,
    1307674368000.0, 20922789888000.0, 355687428096000.0,
    6402373705728000.0, 121645100408832000.0, 2432902008176640000.0,
    51090942171709440000.0, 1124000727777607680000.0,
];
const LOGPI: f64 = 1.144729885849400174143427351353058711647;

/// `sin(pi*x)`, accurate for x integral or close to an integer.
fn m_sinpi(x: f64) -> f64 {
    debug_assert!(x.is_finite());
    let y = x.abs() % 2.0;
    let n = (2.0 * y).round() as i32;
    let r = match n {
        0 => (std::f64::consts::PI * y).sin(),
        1 => (std::f64::consts::PI * (y - 0.5)).cos(),
        // N.B. -sin(pi*(y-1.0)) is *not* equivalent: it would give -0.0
        // instead of 0.0 when y == 1.0.
        2 => (std::f64::consts::PI * (1.0 - y)).sin(),
        3 => -(std::f64::consts::PI * (y - 1.5)).cos(),
        _ => (std::f64::consts::PI * (y - 2.0)).sin(),
    };
    1f64.copysign(x) * r
}

/// Lanczos' sum L_g(x), for positive x, evaluated as a rational function
/// with rescaling by x**(1-LANCZOS_N) for large x.
fn lanczos_sum(x: f64) -> f64 {
    debug_assert!(x > 0.0);
    let mut num = 0.0;
    let mut den = 0.0;
    if x < 5.0 {
        for i in (0..LANCZOS_N).rev() {
            num = num * x + LANCZOS_NUM_COEFFS[i];
            den = den * x + LANCZOS_DEN_COEFFS[i];
        }
    } else {
        for i in 0..LANCZOS_N {
            num = num / x + LANCZOS_NUM_COEFFS[i];
            den = den / x + LANCZOS_DEN_COEFFS[i];
        }
    }
    num / den
}

fn m_tgamma(x: f64) -> (f64, Errno) {
    // special cases
    if !x.is_finite() {
        if x.is_nan() || x > 0.0 {
            return (x, Errno::Ok); // tgamma(nan) = nan, tgamma(inf) = inf
        }
        return (f64::NAN, Errno::Dom); // tgamma(-inf) = nan, invalid
    }
    if x == 0.0 {
        // tgamma(+-0.0) = +-inf, divide-by-zero
        return (f64::INFINITY.copysign(x), Errno::Dom);
    }

    // integer arguments
    if x == x.floor() {
        if x < 0.0 {
            return (f64::NAN, Errno::Dom); // tgamma(n) invalid for negative integers n
        }
        if x <= NGAMMA_INTEGRAL as f64 {
            return (GAMMA_INTEGRAL[x as usize - 1], Errno::Ok);
        }
    }
    let absx = x.abs();

    // tiny arguments: tgamma(x) ~ 1/x for x near 0
    if absx < 1e-20 {
        let r = 1.0 / x;
        let errno = if r.is_infinite() { Errno::Range } else { Errno::Ok };
        return (r, errno);
    }

    // large arguments: tgamma(x) overflows for x > 200, underflows to +-0.0
    // for x < -200 (not a negative integer).
    if absx > 200.0 {
        if x < 0.0 {
            return (0.0 / m_sinpi(x), Errno::Ok);
        }
        return (f64::INFINITY, Errno::Range);
    }

    let y = absx + LANCZOS_G_MINUS_HALF;
    // compute error in sum
    let mut z = if absx > LANCZOS_G_MINUS_HALF {
        let q = y - absx;
        q - LANCZOS_G_MINUS_HALF
    } else {
        let q = y - LANCZOS_G_MINUS_HALF;
        q - absx
    };
    z = z * LANCZOS_G / y;
    let mut r;
    if x < 0.0 {
        r = -std::f64::consts::PI / m_sinpi(absx) / absx * y.exp() / lanczos_sum(absx);
        r -= z * r;
        if absx < 140.0 {
            r /= y.powf(absx - 0.5);
        } else {
            let sqrtpow = y.powf(absx / 2.0 - 0.25);
            r /= sqrtpow;
            r /= sqrtpow;
        }
    } else {
        r = lanczos_sum(absx) / y.exp();
        r += z * r;
        if absx < 140.0 {
            r *= y.powf(absx - 0.5);
        } else {
            let sqrtpow = y.powf(absx / 2.0 - 0.25);
            r *= sqrtpow;
            r *= sqrtpow;
        }
    }
    let errno = if r.is_infinite() { Errno::Range } else { Errno::Ok };
    (r, errno)
}

fn m_lgamma(x: f64) -> (f64, Errno) {
    // special cases
    if !x.is_finite() {
        if x.is_nan() {
            return (x, Errno::Ok); // lgamma(nan) = nan
        }
        return (f64::INFINITY, Errno::Ok); // lgamma(+-inf) = +inf
    }

    // integer arguments
    if x == x.floor() && x <= 2.0 {
        if x <= 0.0 {
            return (f64::INFINITY, Errno::Dom); // lgamma(n) invalid for integers n <= 0
        }
        return (0.0, Errno::Ok); // lgamma(1) = lgamma(2) = 0.0
    }

    let absx = x.abs();
    // tiny arguments: lgamma(x) ~ -log(fabs(x)) for small x
    if absx < 1e-20 {
        return (-absx.ln(), Errno::Ok);
    }

    // Lanczos' formula.
    let mut r = lanczos_sum(absx).ln() - LANCZOS_G;
    r += (absx - 0.5) * ((absx + LANCZOS_G - 0.5).ln() - 1.0);
    if x < 0.0 {
        // reflection formula for negative x
        r = LOGPI - m_sinpi(absx).abs().ln() - absx.ln() - r;
    }
    let errno = if r.is_infinite() { Errno::Range } else { Errno::Ok };
    (r, errno)
}

// ---------------------------------------------------------------------------
// remainder / fmod / pow / fma

/// IEEE 754-style remainder: x - n*y with n*y the nearest multiple of y,
/// ties to even n. Exact port of `m_remainder`.
fn m_remainder(x: f64, y: f64) -> f64 {
    if x.is_finite() && y.is_finite() {
        if y == 0.0 {
            return f64::NAN;
        }
        let absx = x.abs();
        let absy = y.abs();
        let m = absx % absy;
        // Compare m against absy - m instead of 0.5*absy (which may not be
        // representable); see mathmodule.c for the Sterbenz argument.
        let c = absy - m;
        let r = if m < c {
            m
        } else if m > c {
            -c
        } else {
            // absx is exactly halfway between two multiples of absy; choose
            // the even multiple. All steps below are exact.
            m - 2.0 * ((0.5 * (absx - m)) % absy)
        };
        return 1f64.copysign(x) * r;
    }
    if x.is_nan() {
        return x;
    }
    if y.is_nan() {
        return y;
    }
    if x.is_infinite() {
        return f64::NAN;
    }
    x
}

unsafe extern "C" fn math_fmod(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    unsafe {
        math_2(argv, argc, "fmod", |x, y| {
            // fmod(x, +/-Inf) returns x for finite x.
            if y.is_infinite() && x.is_finite() {
                return x;
            }
            x % y
        })
    }
}

unsafe extern "C" fn math_pow(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let [a, b] = match unsafe { fixed_args::<2>(argv, argc, "pow") } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: Arguments are live untagged objects.
    let x = match unsafe { coerce_f64(a) } {
        Ok(x) => x,
        Err(error) => return error,
    };
    // SAFETY: Same contract as above.
    let y = match unsafe { coerce_f64(b) } {
        Ok(y) => y,
        Err(error) => return error,
    };
    let r;
    // deal directly with IEEE specials (math_pow_impl port)
    if !x.is_finite() || !y.is_finite() {
        if x.is_nan() {
            r = if y == 0.0 { 1.0 } else { x }; // nan**0 = 1
        } else if y.is_nan() {
            r = if x == 1.0 { 1.0 } else { y }; // 1**nan = 1
        } else if x.is_infinite() {
            let odd_y = y.is_finite() && y.abs() % 2.0 == 1.0;
            r = if y > 0.0 {
                if odd_y { x } else { x.abs() }
            } else if y == 0.0 {
                1.0
            } else if odd_y {
                0f64.copysign(x)
            } else {
                0.0
            };
        } else {
            debug_assert!(y.is_infinite());
            r = if x.abs() == 1.0 {
                1.0
            } else if y > 0.0 && x.abs() > 1.0 {
                y
            } else if y < 0.0 && x.abs() < 1.0 {
                -y // +inf
            } else {
                0.0
            };
        }
    } else {
        r = x.powf(y);
        if r.is_nan() {
            // (-ve)**(finite non-integer)
            return raise_value("math domain error");
        }
        if r.is_infinite() {
            // (+/-0)**negative is a domain error; anything else overflowed.
            if x == 0.0 {
                return raise_value("math domain error");
            }
            return raise_overflow("math range error");
        }
    }
    from_f64(r)
}

unsafe extern "C" fn math_fma(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let [a, b, c] = match unsafe { fixed_args::<3>(argv, argc, "fma") } {
        Ok(args) => args,
        Err(error) => return error,
    };
    let mut values = [0f64; 3];
    for (slot, object) in values.iter_mut().zip([a, b, c]) {
        // SAFETY: Arguments are live untagged objects.
        match unsafe { coerce_f64(object) } {
            Ok(value) => *slot = value,
            Err(error) => return error,
        }
    }
    let [x, y, z] = values;
    let r = x.mul_add(y, z);
    if r.is_finite() {
        return from_f64(r);
    }
    if r.is_nan() {
        if !x.is_nan() && !y.is_nan() && !z.is_nan() {
            return raise_value("invalid operation in fma");
        }
    } else if x.is_finite() && y.is_finite() && z.is_finite() {
        return raise_overflow("overflow in fma");
    }
    from_f64(r)
}

// ---------------------------------------------------------------------------
// log family (loghelper: exact for arbitrarily large ints)

/// `m_log`-family core: `log_fn` is the positive-finite branch; zeros and
/// negative values are domain errors carrying the 3.14 message.
fn log_1(x: f64, log_fn: impl Fn(f64) -> f64) -> Result<f64, ()> {
    if x.is_nan() {
        return Ok(x); // log(nan) = nan
    }
    if x > 0.0 {
        return Ok(if x.is_infinite() { x } else { log_fn(x) });
    }
    Err(()) // log(0) and log(-ve): divide-by-zero / invalid
}

/// `loghelper`: ints are handled exactly (frexp-style split for values wider
/// than a double); everything else goes through the float path.
unsafe fn loghelper(arg: *mut PyObject, log_fn: impl Fn(f64) -> f64) -> Result<f64, *mut PyObject> {
    // SAFETY: Type probes tolerate any live object.
    if let Some(value) = unsafe { to_bigint_including_bool(arg) } {
        if !value.is_positive() {
            // The input can be an arbitrarily large integer, so the message
            // omits the value (loghelper, mathmodule.c line 2245).
            return Err(raise_value("expected a positive input"));
        }
        let as_float = value.to_f64().unwrap_or(f64::INFINITY);
        if as_float.is_finite() {
            return Ok(log_fn(as_float));
        }
        // Value is ~= m * 2**e with 0.5 <= m < 1, so log ~= log(m) + log(2)*e.
        let bits = value.bits();
        let top = (&value >> (bits - 64)).to_u64().unwrap_or(u64::MAX);
        let m = (top as f64) / (u64::MAX as f64 + 1.0);
        return Ok(log_fn(m) + log_fn(2.0) * (bits as f64));
    }
    // SAFETY: `arg` is a live untagged object.
    let x = match unsafe { coerce_f64(arg) } {
        Ok(x) => x,
        Err(error) => return Err(error),
    };
    match log_1(x, log_fn) {
        Ok(r) => Ok(r),
        Err(()) => Err(domain_error(Some("expected a positive input, got %s"), x)),
    }
}

unsafe extern "C" fn math_log(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Caller passes a live argv window.
    let args = unsafe { arg_vec(argv, argc) };
    if args.is_empty() || args.len() > 2 {
        return raise_type(&format!("log expected 1 or 2 arguments, got {argc}"));
    }
    // SAFETY: Arguments are live untagged objects.
    let num = match unsafe { loghelper(args[0], f64::ln) } {
        Ok(value) => value,
        Err(error) => return error,
    };
    if args.len() == 1 {
        return from_f64(num);
    }
    // SAFETY: Same contract as above.
    let den = match unsafe { loghelper(args[1], f64::ln) } {
        Ok(value) => value,
        Err(error) => return error,
    };
    if den == 0.0 {
        // PyNumber_TrueDivide(float, float) with a zero denominator; 3.14
        // unified the division-by-zero messages (gh-87999).
        return raise(ExceptionKind::ZeroDivisionError, "division by zero");
    }
    from_f64(num / den)
}

/// `m_log2` uses exact `log2` so powers of two stay exact.
unsafe extern "C" fn math_log2(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let [arg] = match unsafe { fixed_args::<1>(argv, argc, "log2") } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: `arg` is a live untagged object.
    match unsafe { loghelper(arg, f64::log2) } {
        Ok(value) => from_f64(value),
        Err(error) => error,
    }
}

unsafe extern "C" fn math_log10(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let [arg] = match unsafe { fixed_args::<1>(argv, argc, "log10") } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: `arg` is a live untagged object.
    match unsafe { loghelper(arg, f64::log10) } {
        Ok(value) => from_f64(value),
        Err(error) => error,
    }
}

// ---------------------------------------------------------------------------
// floor / ceil / trunc (return exact ints)

/// `PyLong_FromDouble` error contract for the int-returning functions.
fn float_to_exact_int(value: f64) -> *mut PyObject {
    if value.is_nan() {
        return raise_value("cannot convert float NaN to integer");
    }
    if value.is_infinite() {
        return raise_overflow("cannot convert float infinity to integer");
    }
    match crate::types::int::bigint_from_f64_trunc(value) {
        Some(big) => from_bigint(big),
        None => raise_value("cannot convert float NaN to integer"),
    }
}

/// Shared floor/ceil shape: float fast path, int identity, `__floor__` /
/// `__ceil__` dispatch, then the generic float conversion.
unsafe fn floor_ceil(argv: *mut *mut PyObject, argc: usize, name: &str, method: &str, f: fn(f64) -> f64) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let [arg] = match unsafe { fixed_args::<1>(argv, argc, name) } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: Type probes tolerate any live object.
    if let Some(value) = unsafe { to_f64(arg) } {
        return float_to_exact_int(f(value));
    }
    // int.__floor__ / int.__ceil__ return self; bools stay bool in CPython
    // and the runtime's canonical singletons make reboxing identity-safe.
    // SAFETY: Same contract as above.
    if let Some(value) = unsafe { bool_::to_bool(arg) } {
        return bool_::from_bool(value);
    }
    // SAFETY: Same contract as above.
    if unsafe { is_exact_int(arg) } {
        return arg;
    }
    // SAFETY: `try_get_attr` clears the miss diagnostic.
    if let Some(hook) = unsafe { try_get_attr(arg, method) } {
        // SAFETY: Bound special method invoked with zero arguments.
        return untag(unsafe { pon_call(hook, ptr::null_mut(), 0) });
    }
    // SAFETY: `arg` is a live untagged object.
    match unsafe { coerce_f64(arg) } {
        Ok(value) => float_to_exact_int(f(value)),
        Err(error) => error,
    }
}

unsafe extern "C" fn math_floor(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    unsafe { floor_ceil(argv, argc, "floor", "__floor__", f64::floor) }
}

unsafe extern "C" fn math_ceil(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    unsafe { floor_ceil(argv, argc, "ceil", "__ceil__", f64::ceil) }
}

unsafe extern "C" fn math_trunc(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let [arg] = match unsafe { fixed_args::<1>(argv, argc, "trunc") } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: Type probes tolerate any live object.
    if let Some(value) = unsafe { to_f64(arg) } {
        return float_to_exact_int(value.trunc());
    }
    // SAFETY: Same contract as above.
    if let Some(value) = unsafe { bool_::to_bool(arg) } {
        return bool_::from_bool(value);
    }
    // SAFETY: Same contract as above.
    if unsafe { is_exact_int(arg) } {
        return arg;
    }
    // SAFETY: `try_get_attr` clears the miss diagnostic.
    if let Some(hook) = unsafe { try_get_attr(arg, "__trunc__") } {
        // SAFETY: Bound special method invoked with zero arguments.
        return untag(unsafe { pon_call(hook, ptr::null_mut(), 0) });
    }
    raise_type(&format!("type {} doesn't define __trunc__ method", type_name(arg)))
}

// ---------------------------------------------------------------------------
// frexp / ldexp / modf

unsafe extern "C" fn math_frexp(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let [arg] = match unsafe { fixed_args::<1>(argv, argc, "frexp") } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: `arg` is a live untagged object.
    let x = match unsafe { coerce_f64(arg) } {
        Ok(x) => x,
        Err(error) => return error,
    };
    // NaNs, zeros and infinities keep exponent 0 (math_frexp_impl).
    let (m, e) = if x.is_nan() || x.is_infinite() || x == 0.0 {
        (x, 0)
    } else {
        let mut e: c_int = 0;
        // SAFETY: `e` is a live out-parameter for the libm call.
        let m = unsafe { libm_frexp(x, &raw mut e) };
        (m, e)
    };
    tuple2(from_f64(m), crate::types::int::from_i64(i64::from(e)))
}

unsafe extern "C" fn math_ldexp(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let [a, b] = match unsafe { fixed_args::<2>(argv, argc, "ldexp") } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: `a` is a live untagged object.
    let x = match unsafe { coerce_f64(a) } {
        Ok(x) => x,
        Err(error) => return error,
    };
    // SAFETY: Type probes tolerate any live object.
    let Some(exp) = (unsafe { to_bigint_including_bool(b) }) else {
        return raise_type("Expected an int as second argument to ldexp.");
    };
    let r = if x == 0.0 || !x.is_finite() {
        x // NaNs, zeros and infinities are unchanged
    } else {
        match exp.to_i32() {
            // SAFETY: Pure libm scaling call.
            Some(exp) => unsafe { libm_ldexp(x, exp) },
            None if exp.is_positive() => return raise_overflow("math range error"),
            None => 0f64.copysign(x), // huge negative: underflow to +-0
        }
    };
    if r.is_infinite() && x.is_finite() {
        return raise_overflow("math range error");
    }
    from_f64(r)
}

unsafe extern "C" fn math_modf(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let [arg] = match unsafe { fixed_args::<1>(argv, argc, "modf") } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: `arg` is a live untagged object.
    let x = match unsafe { coerce_f64(arg) } {
        Ok(x) => x,
        Err(error) => return error,
    };
    if x.is_infinite() {
        return tuple2(from_f64(0f64.copysign(x)), from_f64(x));
    }
    if x.is_nan() {
        return tuple2(from_f64(x), from_f64(x));
    }
    let int_part = x.trunc();
    let mut frac = x - int_part;
    if frac == 0.0 {
        frac = 0f64.copysign(x); // C modf keeps the sign of x on the fraction
    }
    tuple2(from_f64(frac), from_f64(int_part))
}

// ---------------------------------------------------------------------------
// Classification / isclose

unsafe fn classify(argv: *mut *mut PyObject, argc: usize, name: &str, f: fn(f64) -> bool) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let [arg] = match unsafe { fixed_args::<1>(argv, argc, name) } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: `arg` is a live untagged object.
    match unsafe { coerce_f64(arg) } {
        Ok(x) => bool_::from_bool(f(x)),
        Err(error) => error,
    }
}

unsafe extern "C" fn math_isfinite(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    unsafe { classify(argv, argc, "isfinite", f64::is_finite) }
}

unsafe extern "C" fn math_isinf(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    unsafe { classify(argv, argc, "isinf", f64::is_infinite) }
}

unsafe extern "C" fn math_isnan(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    unsafe { classify(argv, argc, "isnan", f64::is_nan) }
}

/// `isclose(a, b, *, rel_tol=1e-09, abs_tol=0.0)`; the keyword-only slots
/// arrive positionally with None filling absent values.
unsafe extern "C" fn math_isclose(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Caller passes a live argv window.
    let args = unsafe { arg_vec(argv, argc) };
    if args.len() < 2 || args.len() > 4 {
        return raise_type(&format!("isclose expected 2 arguments, got {}", args.len()));
    }
    let mut values = [0.0, 0.0, 1e-09, 0.0];
    for (index, &object) in args.iter().enumerate() {
        if index >= 2 && is_none(object) {
            continue; // absent keyword slot keeps its default
        }
        // SAFETY: Arguments are live untagged objects.
        match unsafe { coerce_f64(object) } {
            Ok(value) => values[index] = value,
            Err(error) => return error,
        }
    }
    let [a, b, rel_tol, abs_tol] = values;
    if rel_tol < 0.0 || abs_tol < 0.0 {
        return raise_value("tolerances must be non-negative");
    }
    if a == b {
        return bool_::from_bool(true);
    }
    if a.is_infinite() || b.is_infinite() {
        return bool_::from_bool(false);
    }
    let diff = (b - a).abs();
    bool_::from_bool(diff <= (rel_tol * b).abs() || diff <= (rel_tol * a).abs() || diff <= abs_tol)
}

// ---------------------------------------------------------------------------
// Exact integer functions: factorial / isqrt / gcd / lcm / comb / perm

/// Product of the inclusive range [lo, hi] by binary splitting (balanced
/// operand sizes keep the BigInt multiplications cheap).
fn range_product(lo: u64, hi: u64) -> BigInt {
    if hi < lo {
        return BigInt::one();
    }
    if hi - lo < 8 {
        let mut product = BigInt::from(lo);
        for value in lo + 1..=hi {
            product *= value;
        }
        return product;
    }
    let mid = lo + (hi - lo) / 2;
    range_product(lo, mid) * range_product(mid + 1, hi)
}

unsafe extern "C" fn math_factorial(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let [arg] = match unsafe { fixed_args::<1>(argv, argc, "factorial") } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // factorial() takes ints directly (no index protocol in 3.14).
    // SAFETY: Type probes tolerate any live object.
    let Some(n) = (unsafe { to_bigint_including_bool(arg) }) else {
        return raise_type(&format!(
            "'{}' object cannot be interpreted as an integer",
            type_name(arg)
        ));
    };
    if n.is_negative() {
        return raise_value("factorial() not defined for negative values");
    }
    let Some(n) = n.to_u64().filter(|&n| n <= i64::MAX as u64) else {
        return raise_overflow(&format!("factorial() argument should not exceed {}", i64::MAX));
    };
    if n < 2 {
        return from_bigint(BigInt::one());
    }
    from_bigint(range_product(2, n))
}

unsafe extern "C" fn math_isqrt(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let [arg] = match unsafe { fixed_args::<1>(argv, argc, "isqrt") } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: `arg` is a live untagged object.
    let n = match unsafe { coerce_index(arg) } {
        Ok(n) => n,
        Err(error) => return error,
    };
    if n.is_negative() {
        return raise_value("isqrt() argument must be nonnegative");
    }
    from_bigint(num_integer::Roots::sqrt(&n))
}

/// `gcd(*integers)` / `lcm(*integers)` share the fold shape.
unsafe fn gcd_lcm(argv: *mut *mut PyObject, argc: usize, is_lcm: bool) -> *mut PyObject {
    // SAFETY: Caller passes a live argv window.
    let args = unsafe { arg_vec(argv, argc) };
    let mut values = Vec::with_capacity(args.len());
    for object in args {
        // SAFETY: Arguments are live untagged objects.
        match unsafe { coerce_index(object) } {
            Ok(value) => values.push(value),
            Err(error) => return error,
        }
    }
    let mut result = if is_lcm { BigInt::one() } else { BigInt::zero() };
    let Some(first) = values.first() else {
        return from_bigint(result);
    };
    result = first.abs();
    for value in &values[1..] {
        if is_lcm {
            if result.is_zero() || value.is_zero() {
                result = BigInt::zero();
                continue;
            }
            result = (&result / result.gcd(value)) * value.abs();
        } else {
            if result.is_one() {
                continue; // gcd is already 1; only argument validation remains
            }
            result = result.gcd(value);
        }
    }
    from_bigint(result)
}

unsafe extern "C" fn math_gcd(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    unsafe { gcd_lcm(argv, argc, false) }
}

unsafe extern "C" fn math_lcm(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    unsafe { gcd_lcm(argv, argc, true) }
}

/// P(n, k): n * (n-1) * ... * (n-k+1), exact.
fn falling_factorial(n: &BigInt, k: u64) -> BigInt {
    let mut result = BigInt::one();
    let mut factor = n.clone();
    for _ in 0..k {
        result *= &factor;
        factor -= 1;
    }
    result
}

unsafe extern "C" fn math_perm(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Caller passes a live argv window.
    let args = unsafe { arg_vec(argv, argc) };
    if args.is_empty() || args.len() > 2 {
        return raise_type(&format!("perm expected 1 or 2 arguments, got {}", args.len()));
    }
    if args.len() == 2 && is_none(args[1]) {
        // perm(n, None) == factorial(n)
        let mut solo = [args[0]];
        // SAFETY: Forwarding a live single-argument window.
        return unsafe { math_factorial(solo.as_mut_ptr(), 1) };
    }
    // SAFETY: Arguments are live untagged objects.
    let n = match unsafe { coerce_index(args[0]) } {
        Ok(n) => n,
        Err(error) => return error,
    };
    if args.len() == 1 {
        let mut solo = [args[0]];
        // SAFETY: Forwarding a live single-argument window.
        return unsafe { math_factorial(solo.as_mut_ptr(), 1) };
    }
    // SAFETY: Same contract as above.
    let k = match unsafe { coerce_index(args[1]) } {
        Ok(k) => k,
        Err(error) => return error,
    };
    if n.is_negative() {
        return raise_value("n must be a non-negative integer");
    }
    if k.is_negative() {
        return raise_value("k must be a non-negative integer");
    }
    if k > n {
        return from_bigint(BigInt::zero());
    }
    let Some(k) = k.to_u64() else {
        return raise_overflow(&format!("k must not exceed {}", i64::MAX));
    };
    from_bigint(falling_factorial(&n, k))
}

unsafe extern "C" fn math_comb(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let [a, b] = match unsafe { fixed_args::<2>(argv, argc, "comb") } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: Arguments are live untagged objects.
    let n = match unsafe { coerce_index(a) } {
        Ok(n) => n,
        Err(error) => return error,
    };
    // SAFETY: Same contract as above.
    let k = match unsafe { coerce_index(b) } {
        Ok(k) => k,
        Err(error) => return error,
    };
    if n.is_negative() {
        return raise_value("n must be a non-negative integer");
    }
    if k.is_negative() {
        return raise_value("k must be a non-negative integer");
    }
    if k > n {
        return from_bigint(BigInt::zero());
    }
    // k = min(k, n - k); the reduced k always fits a machine word for any
    // computation that could finish.
    let reduced = (&n - &k).min(k);
    let Some(k) = reduced.to_u64() else {
        return raise_overflow(&format!("min(n - k, k) must not exceed {}", i64::MAX));
    };
    // C(n, k) = prod_{i=1..k} (n - k + i) / i, exact at every step.
    let mut result = BigInt::one();
    let base = &n - k;
    for i in 1..=k {
        result = result * (&base + i) / i;
    }
    from_bigint(result)
}

// ---------------------------------------------------------------------------
// fsum (Shewchuk partials, math_fsum port)

unsafe extern "C" fn math_fsum(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let [seq] = match unsafe { fixed_args::<1>(argv, argc, "fsum") } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: `pon_get_iter` self-normalizes its argument.
    let iter = unsafe { pon_get_iter(seq, ptr::null_mut()) };
    if iter.is_null() {
        return ptr::null_mut();
    }
    let mut partials: Vec<f64> = Vec::with_capacity(32);
    let mut special_sum = 0.0;
    let mut inf_sum = 0.0;
    loop {
        // SAFETY: `iter` is a live iterator object.
        let item = match unsafe { next_item(iter) } {
            NextItem::Value(item) => item,
            NextItem::Stop => break,
            NextItem::Error => return ptr::null_mut(),
        };
        // SAFETY: `item` is a live untagged object.
        let mut x = match unsafe { coerce_f64(item) } {
            Ok(x) => x,
            Err(error) => return error,
        };
        let xsave = x;
        let mut i = 0;
        for j in 0..partials.len() {
            let mut y = partials[j];
            if x.abs() < y.abs() {
                core::mem::swap(&mut x, &mut y);
            }
            let hi = x + y;
            let lo = y - (hi - x);
            if lo != 0.0 {
                partials[i] = lo;
                i += 1;
            }
            x = hi;
        }
        partials.truncate(i);
        if x != 0.0 {
            if !x.is_finite() {
                // Non-finite partial: intermediate overflow or a special in
                // the summands.
                if xsave.is_finite() {
                    return raise_overflow("intermediate overflow in fsum");
                }
                if xsave.is_infinite() {
                    inf_sum += xsave;
                }
                special_sum += xsave;
                partials.clear();
            } else {
                partials.push(x);
            }
        }
    }
    if special_sum != 0.0 {
        if inf_sum.is_nan() {
            return raise_value("-inf + inf in fsum");
        }
        return from_f64(special_sum);
    }
    let mut hi = 0.0;
    if let Some(&last) = partials.last() {
        hi = last;
        let mut n = partials.len() - 1;
        let mut lo = 0.0;
        while n > 0 {
            let x = hi;
            n -= 1;
            let y = partials[n];
            debug_assert!(y.abs() < x.abs());
            hi = x + y;
            lo = y - (hi - x);
            if lo != 0.0 {
                break;
            }
        }
        // Half-even rounding across multiple partials (sum([1e-16, 1, 1e16])).
        if n > 0 && ((lo < 0.0 && partials[n - 1] < 0.0) || (lo > 0.0 && partials[n - 1] > 0.0)) {
            let y = lo * 2.0;
            let x = hi + y;
            if y == x - hi {
                hi = x;
            }
        }
    }
    from_f64(hi)
}

// ---------------------------------------------------------------------------
// hypot / dist (vector_norm port) and sumprod (triple-length accumulation)

#[derive(Clone, Copy)]
struct DoubleLength {
    hi: f64,
    lo: f64,
}

/// Algorithm 1.1: compensated summation, requires |a| >= |b|.
fn dl_fast_sum(a: f64, b: f64) -> DoubleLength {
    debug_assert!(a.abs() >= b.abs() || !a.is_finite() || !b.is_finite());
    let x = a + b;
    DoubleLength { hi: x, lo: (a - x) + b }
}

/// Algorithm 3.1: error-free transformation of the sum.
fn dl_sum(a: f64, b: f64) -> DoubleLength {
    let x = a + b;
    let z = x - a;
    DoubleLength { hi: x, lo: (a - (x - z)) + (b - z) }
}

/// Algorithm 3.5: error-free transformation of a product (fma form; arm64
/// and x86-64 both have reliable hardware fma).
fn dl_mul(x: f64, y: f64) -> DoubleLength {
    let z = x * y;
    DoubleLength { hi: z, lo: x.mul_add(y, -z) }
}

#[derive(Clone, Copy)]
struct TripleLength {
    hi: f64,
    lo: f64,
    tiny: f64,
}

const TL_ZERO: TripleLength = TripleLength { hi: 0.0, lo: 0.0, tiny: 0.0 };

/// Algorithm 5.10 with SumKVert for K=3.
fn tl_fma(x: f64, y: f64, total: TripleLength) -> TripleLength {
    let pr = dl_mul(x, y);
    let sm = dl_sum(total.hi, pr.hi);
    let r1 = dl_sum(total.lo, pr.lo);
    let r2 = dl_sum(r1.hi, sm.lo);
    TripleLength { hi: sm.hi, lo: r2.hi, tiny: total.tiny + r1.lo + r2.lo }
}

fn tl_to_d(total: TripleLength) -> f64 {
    let last = dl_sum(total.lo, total.hi);
    total.tiny + last.lo + last.hi
}

/// `vector_norm`: scaled, Neumaier-compensated, differentially-corrected
/// sqrt of a sum of squares. `max` is the largest |x|; `found_nan` reports a
/// NaN member.
fn vector_norm(vec: &mut [f64], max: f64, found_nan: bool) -> f64 {
    if max.is_infinite() {
        return max;
    }
    if found_nan {
        return f64::NAN;
    }
    if max == 0.0 || vec.len() <= 1 {
        return max;
    }
    let mut max_e: c_int = 0;
    // SAFETY: `max_e` is a live out-parameter for the libm call.
    unsafe { libm_frexp(max, &raw mut max_e) };
    if max_e < -1023 {
        // ldexp(1.0, -max_e) would overflow: normalize subnormals first.
        for value in vec.iter_mut() {
            *value /= f64::MIN_POSITIVE;
        }
        let rescaled_max = max / f64::MIN_POSITIVE;
        return f64::MIN_POSITIVE * vector_norm(vec, rescaled_max, found_nan);
    }
    // SAFETY: Pure libm scaling call.
    let scale = unsafe { libm_ldexp(1.0, -max_e) };
    let mut csum = 1.0;
    let mut frac1 = 0.0;
    let mut frac2 = 0.0;
    for &x in vec.iter() {
        let x = x * scale; // lossless scaling
        let pr = dl_mul(x, x); // lossless squaring
        let sm = dl_fast_sum(csum, pr.hi); // lossless addition
        csum = sm.hi;
        frac1 += pr.lo; // lossy addition
        frac2 += sm.lo; // lossy addition
    }
    let mut h = (csum - 1.0 + (frac1 + frac2)).sqrt();
    let pr = dl_mul(-h, h);
    let sm = dl_fast_sum(csum, pr.hi);
    csum = sm.hi;
    frac1 += pr.lo;
    frac2 += sm.lo;
    let x = csum - 1.0 + (frac1 + frac2);
    h += x / (2.0 * h); // differential correction
    h / scale
}

unsafe extern "C" fn math_hypot(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Caller passes a live argv window.
    let args = unsafe { arg_vec(argv, argc) };
    let mut coordinates = Vec::with_capacity(args.len());
    let mut max = 0.0f64;
    let mut found_nan = false;
    for object in args {
        // SAFETY: Arguments are live untagged objects.
        let x = match unsafe { coerce_f64(object) } {
            Ok(x) => x.abs(),
            Err(error) => return error,
        };
        found_nan |= x.is_nan();
        if x > max {
            max = x;
        }
        coordinates.push(x);
    }
    from_f64(vector_norm(&mut coordinates, max, found_nan))
}

/// Drains an iterable into a Vec of raw items.
unsafe fn collect_items(object: *mut PyObject) -> Result<Vec<*mut PyObject>, *mut PyObject> {
    // SAFETY: `pon_get_iter` self-normalizes its argument.
    let iter = unsafe { pon_get_iter(object, ptr::null_mut()) };
    if iter.is_null() {
        return Err(ptr::null_mut());
    }
    let mut items = Vec::new();
    loop {
        // SAFETY: `iter` is a live iterator object.
        match unsafe { next_item(iter) } {
            NextItem::Value(item) => items.push(item),
            NextItem::Stop => return Ok(items),
            NextItem::Error => return Err(ptr::null_mut()),
        }
    }
}

unsafe extern "C" fn math_dist(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let [p, q] = match unsafe { fixed_args::<2>(argv, argc, "dist") } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: Arguments are live untagged objects.
    let p_items = match unsafe { collect_items(p) } {
        Ok(items) => items,
        Err(error) => return error,
    };
    // SAFETY: Same contract as above.
    let q_items = match unsafe { collect_items(q) } {
        Ok(items) => items,
        Err(error) => return error,
    };
    if p_items.len() != q_items.len() {
        return raise_value("both points must have the same number of dimensions");
    }
    let mut diffs = Vec::with_capacity(p_items.len());
    let mut max = 0.0f64;
    let mut found_nan = false;
    for (&pi, &qi) in p_items.iter().zip(&q_items) {
        // SAFETY: Items are live untagged objects.
        let px = match unsafe { coerce_f64(pi) } {
            Ok(x) => x,
            Err(error) => return error,
        };
        // SAFETY: Same contract as above.
        let qx = match unsafe { coerce_f64(qi) } {
            Ok(x) => x,
            Err(error) => return error,
        };
        let x = (px - qx).abs();
        found_nan |= x.is_nan();
        if x > max {
            max = x;
        }
        diffs.push(x);
    }
    from_f64(vector_norm(&mut diffs, max, found_nan))
}

/// One operand of a sumprod step, classified for the fast paths.
enum Operand {
    Int(BigInt),
    Float(f64),
    Other,
}

unsafe fn classify_operand(object: *mut PyObject) -> Operand {
    // SAFETY: Type probes tolerate any live object.
    if let Some(value) = unsafe { to_f64(object) } {
        return Operand::Float(value);
    }
    // SAFETY: Same contract as above.
    if let Some(value) = unsafe { to_bigint_including_bool(object) } {
        return Operand::Int(value);
    }
    Operand::Other
}

unsafe extern "C" fn math_sumprod(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let [p, q] = match unsafe { fixed_args::<2>(argv, argc, "sumprod") } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: `pon_get_iter` self-normalizes its argument.
    let p_it = unsafe { pon_get_iter(p, ptr::null_mut()) };
    if p_it.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: Same contract as above.
    let q_it = unsafe { pon_get_iter(q, ptr::null_mut()) };
    if q_it.is_null() {
        return ptr::null_mut();
    }
    let mut total = crate::types::int::from_i64(0);
    let mut int_path_enabled = true;
    let mut flt_path_enabled = true;
    let mut int_total = BigInt::zero();
    let mut int_total_in_use = false;
    let mut flt_total = TL_ZERO;
    let mut flt_total_in_use = false;
    loop {
        // SAFETY: Iterators are live objects.
        let p_i = match unsafe { next_item(p_it) } {
            NextItem::Value(item) => Some(item),
            NextItem::Stop => None,
            NextItem::Error => return ptr::null_mut(),
        };
        // SAFETY: Same contract as above.
        let q_i = match unsafe { next_item(q_it) } {
            NextItem::Value(item) => Some(item),
            NextItem::Stop => None,
            NextItem::Error => return ptr::null_mut(),
        };
        let (finished, p_i, q_i) = match (p_i, q_i) {
            (Some(p_i), Some(q_i)) => (false, p_i, q_i),
            (None, None) => (true, ptr::null_mut(), ptr::null_mut()),
            _ => return raise_value("Inputs are not the same length"),
        };
        // SAFETY: Live items (NULL only when finished, and then unused).
        let (p_op, q_op) = if finished {
            (Operand::Other, Operand::Other)
        } else {
            (unsafe { classify_operand(p_i) }, unsafe { classify_operand(q_i) })
        };
        if int_path_enabled {
            if let (false, Operand::Int(a), Operand::Int(b)) = (finished, &p_op, &q_op) {
                int_total += a * b;
                int_total_in_use = true;
                continue;
            }
            // Finished or a non-int pair: fold the exact subtotal into total.
            int_path_enabled = false;
            if int_total_in_use {
                let term = from_bigint(core::mem::take(&mut int_total));
                // SAFETY: Numeric-tower addition over live boxed operands.
                total = untag(unsafe { abstract_op::binary_op(BINARY_ADD, total, term) });
                if total.is_null() {
                    return ptr::null_mut();
                }
                int_total_in_use = false;
            }
        }
        if flt_path_enabled {
            if !finished {
                // float*float, float*int and int*float pairs ride the
                // extended-precision accumulator (ints must fit a double).
                let pair = match (&p_op, &q_op) {
                    (Operand::Float(a), Operand::Float(b)) => Some((*a, *b)),
                    (Operand::Float(a), Operand::Int(b)) => b.to_f64().filter(|b| b.is_finite()).map(|b| (*a, b)),
                    (Operand::Int(a), Operand::Float(b)) => a.to_f64().filter(|a| a.is_finite()).map(|a| (a, *b)),
                    _ => None,
                };
                if let Some((flt_p, flt_q)) = pair {
                    let new_total = tl_fma(flt_p, flt_q, flt_total);
                    if new_total.hi.is_finite() {
                        flt_total = new_total;
                        flt_total_in_use = true;
                        continue;
                    }
                }
            }
            // Finished, non-float pair, or non-finite accumulation.
            flt_path_enabled = false;
            if flt_total_in_use {
                let term = from_f64(tl_to_d(flt_total));
                // SAFETY: Numeric-tower addition over live boxed operands.
                total = untag(unsafe { abstract_op::binary_op(BINARY_ADD, total, term) });
                if total.is_null() {
                    return ptr::null_mut();
                }
                flt_total = TL_ZERO;
                flt_total_in_use = false;
            }
        }
        if finished {
            return total;
        }
        // SAFETY: Numeric-tower ops over live boxed operands.
        let term = untag(unsafe { abstract_op::binary_op(BINARY_MUL, p_i, q_i) });
        if term.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: Same contract as above.
        total = untag(unsafe { abstract_op::binary_op(BINARY_ADD, total, term) });
        if total.is_null() {
            return ptr::null_mut();
        }
    }
}

// ---------------------------------------------------------------------------
// prod

/// Running product state: ints stay exact, floats stay f64, anything else
/// falls back to the numeric tower.
enum ProdState {
    Int(BigInt),
    Float(f64),
    Object(*mut PyObject),
}

/// `prod(iterable, *, start=1)`; `start` arrives positionally (None when
/// absent).
unsafe extern "C" fn math_prod(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Caller passes a live argv window.
    let args = unsafe { arg_vec(argv, argc) };
    if args.is_empty() || args.len() > 2 {
        return raise_type(&format!("prod expected 1 or 2 arguments, got {}", args.len()));
    }
    let mut state = if args.len() == 2 && !is_none(args[1]) {
        // SAFETY: `args[1]` is a live untagged object.
        match unsafe { classify_operand(args[1]) } {
            Operand::Int(value) => ProdState::Int(value),
            Operand::Float(value) => ProdState::Float(value),
            Operand::Other => ProdState::Object(args[1]),
        }
    } else {
        ProdState::Int(BigInt::one())
    };
    // SAFETY: `pon_get_iter` self-normalizes its argument.
    let iter = unsafe { pon_get_iter(args[0], ptr::null_mut()) };
    if iter.is_null() {
        return ptr::null_mut();
    }
    loop {
        // SAFETY: `iter` is a live iterator object.
        let item = match unsafe { next_item(iter) } {
            NextItem::Value(item) => item,
            NextItem::Stop => break,
            NextItem::Error => return ptr::null_mut(),
        };
        // SAFETY: `item` is a live untagged object.
        let operand = unsafe { classify_operand(item) };
        state = match (state, operand) {
            (ProdState::Int(total), Operand::Int(value)) => ProdState::Int(total * value),
            (ProdState::Int(total), Operand::Float(value)) => {
                // PyNumber_Multiply(int, float) converts the int first and
                // raises when it exceeds the double range.
                let converted = total.to_f64().unwrap_or(f64::INFINITY);
                if converted.is_infinite() {
                    return raise_overflow("int too large to convert to float");
                }
                ProdState::Float(converted * value)
            }
            (ProdState::Float(total), Operand::Float(value)) => ProdState::Float(total * value),
            (ProdState::Float(total), Operand::Int(value)) => {
                let converted = value.to_f64().unwrap_or(f64::INFINITY);
                if converted.is_infinite() {
                    return raise_overflow("int too large to convert to float");
                }
                ProdState::Float(total * converted)
            }
            (state, _) => {
                let boxed = match state {
                    ProdState::Int(total) => from_bigint(total),
                    ProdState::Float(total) => from_f64(total),
                    ProdState::Object(total) => total,
                };
                // SAFETY: Numeric-tower multiplication over live boxed operands.
                let product = untag(unsafe { abstract_op::binary_op(BINARY_MUL, boxed, item) });
                if product.is_null() {
                    return ptr::null_mut();
                }
                ProdState::Object(product)
            }
        };
    }
    match state {
        ProdState::Int(total) => from_bigint(total),
        ProdState::Float(total) => from_f64(total),
        ProdState::Object(total) => total,
    }
}

// ---------------------------------------------------------------------------
// nextafter / ulp

/// Bit-stepping core shared by `nextafter` (steps >= 0) and `ulp`. Mirrors
/// math_nextafter_impl exactly, including signed-zero behavior.
fn nextafter_steps(x: f64, y: f64, usteps: u64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if y.is_nan() {
        return y;
    }
    if usteps == 0 {
        return x;
    }
    let ux = x.to_bits();
    let uy = y.to_bits();
    if ux == uy {
        return x;
    }
    const SIGN_BIT: u64 = 1 << 63;
    let ax = ux & !SIGN_BIT;
    let ay = uy & !SIGN_BIT;
    if (ux ^ uy) & SIGN_BIT != 0 {
        // opposite signs: ax + ay cannot overflow (top bits clear)
        if ax + ay <= usteps {
            y
        } else if ax < usteps {
            // strict <: <= would get +0.0 vs -0.0 wrong
            f64::from_bits((uy & SIGN_BIT) | (usteps - ax))
        } else {
            f64::from_bits(ux - usteps)
        }
    } else if ax > ay {
        if ax - ay >= usteps { f64::from_bits(ux - usteps) } else { y }
    } else if ay - ax >= usteps {
        f64::from_bits(ux + usteps)
    } else {
        y
    }
}

/// `nextafter(x, y, /, *, steps=None)`; `steps` arrives positionally (None
/// when absent).
unsafe extern "C" fn math_nextafter(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Caller passes a live argv window.
    let args = unsafe { arg_vec(argv, argc) };
    if args.len() < 2 || args.len() > 3 {
        return raise_type(&format!("nextafter expected 2 arguments, got {}", args.len()));
    }
    // SAFETY: Arguments are live untagged objects.
    let x = match unsafe { coerce_f64(args[0]) } {
        Ok(x) => x,
        Err(error) => return error,
    };
    // SAFETY: Same contract as above.
    let y = match unsafe { coerce_f64(args[1]) } {
        Ok(y) => y,
        Err(error) => return error,
    };
    let usteps = if args.len() == 3 && !is_none(args[2]) {
        // SAFETY: Same contract as above.
        let steps = match unsafe { coerce_index(args[2]) } {
            Ok(steps) => steps,
            Err(error) => return error,
        };
        if steps.is_negative() {
            return raise_value("steps must be a non-negative integer");
        }
        // Saturating at u64::MAX covers every representable double distance.
        steps.to_u64().unwrap_or(u64::MAX)
    } else {
        1
    };
    from_f64(nextafter_steps(x, y, usteps))
}

unsafe extern "C" fn math_ulp(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let [arg] = match unsafe { fixed_args::<1>(argv, argc, "ulp") } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: `arg` is a live untagged object.
    let x = match unsafe { coerce_f64(arg) } {
        Ok(x) => x,
        Err(error) => return error,
    };
    if x.is_nan() {
        return from_f64(x);
    }
    let x = x.abs();
    if x.is_infinite() {
        return from_f64(x);
    }
    let x2 = nextafter_steps(x, f64::INFINITY, 1);
    if x2.is_infinite() {
        // special case: x is the largest positive representable float
        let x2 = nextafter_steps(x, f64::NEG_INFINITY, 1);
        return from_f64(x - x2);
    }
    from_f64(x2 - x)
}
#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // m_remainder: IEEE 754 remainder, ties-to-even multiple.

    #[test]
    fn remainder_ties_choose_even_multiple() {
        // 5.0 = 2.5*2.0: halfway between n=2 and n=3, even n=2 wins -> +1.0.
        assert_eq!(m_remainder(5.0, 2.0), 1.0);
        // 3.0 = 1.5*2.0: halfway between n=1 and n=2, even n=2 wins -> -1.0.
        assert_eq!(m_remainder(3.0, 2.0), -1.0);
        // 7.5 = 3.75*2.0: nearest n=4, no tie -> -0.5.
        assert_eq!(m_remainder(7.5, 2.0), -0.5);
        // Inexact operand, exact result: 2.9 - 2.0 is exact (Sterbenz).
        assert_eq!(m_remainder(2.9, 2.0), 2.9 - 2.0);
    }

    #[test]
    fn remainder_result_sign_follows_x() {
        assert_eq!(m_remainder(-5.0, 2.0), -1.0);
        let r = m_remainder(-2.0, 2.0);
        assert_eq!(r, 0.0);
        assert!(r.is_sign_negative(), "remainder(-2, 2) must be -0.0");
        let r = m_remainder(2.0, 2.0);
        assert_eq!(r, 0.0);
        assert!(r.is_sign_positive(), "remainder(2, 2) must be +0.0");
    }

    #[test]
    fn remainder_specials() {
        assert!(m_remainder(f64::INFINITY, 1.0).is_nan());
        assert!(m_remainder(f64::NEG_INFINITY, 1.0).is_nan());
        assert_eq!(m_remainder(3.0, f64::INFINITY), 3.0);
        assert_eq!(m_remainder(-3.0, f64::INFINITY), -3.0);
        assert!(m_remainder(1.0, 0.0).is_nan());
        assert!(m_remainder(f64::NAN, 2.0).is_nan());
        assert!(m_remainder(2.0, f64::NAN).is_nan());
    }

    // -----------------------------------------------------------------
    // Gamma family.

    #[test]
    fn tgamma_integer_table_is_exact() {
        for (x, expected) in [(1.0, 1.0), (2.0, 1.0), (5.0, 24.0), (6.0, 120.0), (10.0, 362_880.0)] {
            let (value, errno) = m_tgamma(x);
            assert_eq!(value, expected, "tgamma({x})");
            assert!(errno == Errno::Ok, "tgamma({x}) errno");
        }
    }

    #[test]
    fn tgamma_domain_and_range_errors() {
        let (value, errno) = m_tgamma(-3.0);
        assert!(value.is_nan() && errno == Errno::Dom, "negative-integer pole");
        let (value, errno) = m_tgamma(0.0);
        assert!(value == f64::INFINITY && errno == Errno::Dom, "tgamma(+0.0)");
        let (value, errno) = m_tgamma(-0.0);
        assert!(value == f64::NEG_INFINITY && errno == Errno::Dom, "tgamma(-0.0)");
        let (value, errno) = m_tgamma(201.0);
        assert!(value == f64::INFINITY && errno == Errno::Range, "tgamma(201.0) overflow");
        let (value, errno) = m_tgamma(f64::NEG_INFINITY);
        assert!(value.is_nan() && errno == Errno::Dom, "tgamma(-inf)");
        let (value, errno) = m_tgamma(f64::INFINITY);
        assert!(value == f64::INFINITY && errno == Errno::Ok, "tgamma(+inf)");
        let (value, errno) = m_tgamma(f64::NAN);
        assert!(value.is_nan() && errno == Errno::Ok, "tgamma(nan)");
    }

    #[test]
    fn tgamma_half_squares_to_pi() {
        // gamma(0.5) = sqrt(pi); exercises the x < 5 Lanczos branch.
        let (value, errno) = m_tgamma(0.5);
        assert!(errno == Errno::Ok);
        let rel = (value * value - std::f64::consts::PI).abs() / std::f64::consts::PI;
        assert!(rel < 1e-14, "gamma(0.5)^2 relative error {rel:e}");
    }

    #[test]
    fn lgamma_exact_zeros_and_poles() {
        assert!(m_lgamma(1.0) == (0.0, Errno::Ok));
        assert!(m_lgamma(2.0) == (0.0, Errno::Ok));
        let (value, errno) = m_lgamma(0.0);
        assert!(value == f64::INFINITY && errno == Errno::Dom, "lgamma(0.0)");
        let (value, errno) = m_lgamma(-4.0);
        assert!(value == f64::INFINITY && errno == Errno::Dom, "lgamma(-4.0)");
        let (value, errno) = m_lgamma(f64::INFINITY);
        assert!(value == f64::INFINITY && errno == Errno::Ok, "lgamma(+inf)");
        let (value, errno) = m_lgamma(f64::NEG_INFINITY);
        assert!(value == f64::INFINITY && errno == Errno::Ok, "lgamma(-inf)");
    }

    #[test]
    fn lgamma_consistent_with_log_of_tgamma() {
        // Exercises both lanczos_sum branches (x < 5 and the rescaled x >= 5).
        for x in [3.5, 6.5, 10.25] {
            let (lg, e1) = m_lgamma(x);
            let (g, e2) = m_tgamma(x);
            assert!(e1 == Errno::Ok && e2 == Errno::Ok);
            let rel = (lg - g.ln()).abs() / lg.abs();
            assert!(rel < 1e-12, "lgamma({x}) vs ln(gamma({x})) relative error {rel:e}");
        }
    }

    #[test]
    fn lanczos_sum_branches_agree_at_crossover() {
        // The x >= 5 evaluation is the same rational function rescaled by
        // x**(1-N); both branches must agree at the switch point.
        let below = lanczos_sum(5.0 - 1e-13);
        let above = lanczos_sum(5.0 + 1e-13);
        assert!(((below - above) / above).abs() < 1e-10);
    }

    #[test]
    fn sinpi_integers_and_halves_are_exact() {
        // The y == 1.0 case must produce +0.0, not -0.0 (mathmodule.c
        // comment: -sin(pi*(y-1.0)) would flip the zero sign).
        let s = m_sinpi(1.0);
        assert_eq!(s, 0.0);
        assert!(s.is_sign_positive(), "sinpi(1.0) must be +0.0");
        assert_eq!(m_sinpi(0.5), 1.0);
        assert_eq!(m_sinpi(1.5), -1.0);
        assert_eq!(m_sinpi(-0.5), -1.0);
        assert_eq!(m_sinpi(2.0), 0.0);
    }

    // -----------------------------------------------------------------
    // nextafter_steps: bit-stepping core for nextafter/ulp.

    #[test]
    fn nextafter_steps_moves_single_ulp() {
        assert_eq!(nextafter_steps(1.0, 2.0, 1).to_bits(), 1.0f64.to_bits() + 1);
        assert_eq!(nextafter_steps(1.0, 0.0, 1).to_bits(), 1.0f64.to_bits() - 1);
    }

    #[test]
    fn nextafter_steps_crosses_signed_zero() {
        let down = nextafter_steps(0.0, -1.0, 1);
        assert_eq!(down.to_bits(), (1u64 << 63) | 1, "smallest negative subnormal");
        let up = nextafter_steps(-0.0, 1.0, 1);
        assert_eq!(up.to_bits(), 1, "smallest positive subnormal");
    }

    #[test]
    fn nextafter_steps_identity_zero_steps_and_saturation() {
        assert_eq!(nextafter_steps(1.5, 1.5, 100), 1.5);
        assert_eq!(nextafter_steps(1.0, 2.0, 0), 1.0);
        assert_eq!(nextafter_steps(1.0, 2.0, u64::MAX), 2.0);
        assert_eq!(nextafter_steps(-1.0, 5.0, u64::MAX), 5.0); // clamp across zero
    }

    #[test]
    fn nextafter_steps_nan_passthrough() {
        assert!(nextafter_steps(f64::NAN, 1.0, 1).is_nan());
        assert!(nextafter_steps(1.0, f64::NAN, 1).is_nan());
    }

    // -----------------------------------------------------------------
    // vector_norm and the double/triple-length accumulation kernels.

    #[test]
    fn vector_norm_exact_pythagorean_triple() {
        assert_eq!(vector_norm(&mut [3.0, 4.0], 4.0, false), 5.0);
    }

    #[test]
    fn vector_norm_specials_and_degenerate_lengths() {
        assert!(vector_norm(&mut [1.0, f64::NAN], 1.0, true).is_nan());
        // Infinity outranks NaN (CPython hypot priority).
        assert_eq!(vector_norm(&mut [f64::INFINITY, f64::NAN], f64::INFINITY, true), f64::INFINITY);
        assert_eq!(vector_norm(&mut [], 0.0, false), 0.0);
        assert_eq!(vector_norm(&mut [7.5], 7.5, false), 7.5);
    }

    #[test]
    fn vector_norm_subnormal_rescaling_is_exact() {
        // 3e-320/4e-320/5e-320 have subnormal mantissas 6072/8096/10120 =
        // 8*253*(3,4,5), an exact Pythagorean triple, so the MIN_POSITIVE
        // renormalization branch must reproduce 5e-320 bit for bit.
        let result = vector_norm(&mut [3e-320, 4e-320], 4e-320, false);
        assert_eq!(result.to_bits(), 5e-320f64.to_bits());
    }

    #[test]
    fn dl_sum_and_fast_sum_capture_rounding_error_exactly() {
        // 1e16 + 1.0 rounds to 1e16 in f64; the error term must carry the 1.0.
        let s = dl_sum(1e16, 1.0);
        assert_eq!((s.hi, s.lo), (1e16, 1.0));
        let s = dl_sum(1.0, 1e16); // Algorithm 3.1 is order-insensitive
        assert_eq!((s.hi, s.lo), (1e16, 1.0));
        let s = dl_fast_sum(1e16, 1.0); // |a| >= |b| precondition holds
        assert_eq!((s.hi, s.lo), (1e16, 1.0));
    }

    #[test]
    fn dl_mul_error_term_recovers_exact_product() {
        let x = 1.000_000_000_000_000_2f64; // 1 + 2^-52
        let pr = dl_mul(x, x);
        // (1 + 2^-52)^2 = 1 + 2^-51 + 2^-104: hi is the rounded product,
        // lo must hold exactly the 2^-104 the rounding dropped.
        assert_eq!(pr.hi, x * x);
        assert_eq!(pr.lo, 2.0f64.powi(-104));
    }

    #[test]
    fn tl_fma_accumulates_cancelling_products_exactly() {
        // Naive f64 gives (1e16 + 1.0) - 1e16 == 0.0; the triple-length
        // accumulator must return the true sum of products, 1.0.
        let mut total = TL_ZERO;
        for (x, y) in [(1e16, 1.0), (1.0, 1.0), (-1e16, 1.0)] {
            total = tl_fma(x, y, total);
        }
        assert_eq!(tl_to_d(total), 1.0);
    }

    // -----------------------------------------------------------------
    // Exact-integer helpers.

    #[test]
    fn range_product_matches_serial_product() {
        assert_eq!(range_product(2, 10), BigInt::from(3_628_800u32));
        assert_eq!(range_product(5, 4), BigInt::one()); // empty range
        assert_eq!(range_product(7, 7), BigInt::from(7));
        // hi - lo >= 8 exercises the binary-splitting branch.
        let mut serial = BigInt::one();
        for value in 1..=25u64 {
            serial *= value;
        }
        assert_eq!(range_product(1, 25), serial);
    }

    #[test]
    fn falling_factorial_exact_products() {
        assert_eq!(falling_factorial(&BigInt::from(10), 3), BigInt::from(720));
        assert_eq!(falling_factorial(&BigInt::from(10), 0), BigInt::one());
        assert_eq!(falling_factorial(&BigInt::from(-3), 2), BigInt::from(12)); // (-3)*(-4)
    }

    // -----------------------------------------------------------------
    // log_1: loghelper domain core.

    #[test]
    fn log_1_routes_domains_correctly() {
        // A controlled closure proves the positive-finite branch reaches
        // log_fn without depending on libm rounding.
        assert_eq!(log_1(4.0, |v| v * 2.0), Ok(8.0));
        assert_eq!(log_1(0.0, f64::ln), Err(()));
        assert_eq!(log_1(-0.0, f64::ln), Err(()));
        assert_eq!(log_1(-2.5, f64::ln), Err(()));
        // Infinity and NaN pass through without touching log_fn.
        assert_eq!(log_1(f64::INFINITY, |_| -999.0), Ok(f64::INFINITY));
        assert!(log_1(f64::NAN, |_| -999.0).unwrap().is_nan());
    }
}
