//! Float numeric tower implementation.

use core::ffi::c_int;
use core::ptr;
use std::sync::LazyLock;

use crate::object::{PyNumberMethods, PyObject, PyObjectHeader, PyType};

#[repr(C)]
#[derive(Debug)]
pub struct PyFloat {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// IEEE-754 double payload.
    pub value: f64,
}

static FLOAT_NUMBER_METHODS: LazyLock<usize> =
    LazyLock::new(|| Box::into_raw(Box::new(make_number_methods())) as usize);
static FLOAT_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(ptr::null(), "float", core::mem::size_of::<PyFloat>());
    ty.tp_hash = Some(hash_slot);
    ty.tp_bool = Some(bool_slot);
    ty.tp_as_number = number_methods_ptr();
    Box::into_raw(Box::new(ty)) as usize
});

/// Boxes an IEEE-754 double as a Python `float`.
#[must_use]
pub fn from_f64(value: f64) -> *mut PyObject {
    Box::into_raw(Box::new(PyFloat {
        ob_base: PyObjectHeader::new(*FLOAT_TYPE as *const PyType),
        value,
    })) as *mut PyObject
}

/// Returns true for exact `float` objects.
#[must_use]
pub unsafe fn is_exact_float(object: *mut PyObject) -> bool {
    unsafe { crate::types::int::type_name_is(object, "float") }
}

/// Extracts a float payload from an exact `float`.
#[must_use]
pub unsafe fn to_f64(object: *mut PyObject) -> Option<f64> {
    if unsafe { is_exact_float(object) } {
        Some(unsafe { (*object.cast::<PyFloat>()).value })
    } else {
        None
    }
}

/// Formats an `f64` using CPython's `repr(float)` rules.
///
/// CPython prints the shortest decimal string that round-trips to the same
/// double, choosing fixed notation for `-4 < decpt <= 16` and scientific
/// notation otherwise, where `decpt` is the position of the decimal point
/// relative to the significant digits. Rust's `{:e}` supplies the same shortest
/// round-trip digits; this function only applies CPython's placement and
/// exponent-padding rules so `print(x)` and `repr(x)` match the reference
/// interpreter (e.g. `2.0`, `-0.0`, `1e-05`, `1e+16`, `nan`, `inf`).
#[must_use]
pub fn repr_f64(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-inf".to_owned() } else { "inf".to_owned() };
    }

    let negative = value.is_sign_negative();
    let formatted = format!("{:e}", value.abs());
    let (mantissa, exp) = formatted.split_once('e').expect("{:e} always emits an exponent");
    let exp: i32 = exp.parse().expect("{:e} exponent is a valid integer");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let body = place_decimal(&digits, exp + 1);
    if negative { format!("-{body}") } else { body }
}

/// Places the decimal point (or renders scientific notation) for shortest
/// round-trip `digits` whose decimal point sits after `decpt` significant digits.
fn place_decimal(digits: &str, decpt: i32) -> String {
    let len = digits.len() as i32;
    if decpt <= -4 || decpt > 16 {
        let sci_exp = decpt - 1;
        let mut out = String::with_capacity(digits.len() + 5);
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if sci_exp < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", sci_exp.unsigned_abs()));
        out
    } else if decpt <= 0 {
        format!("0.{}{}", "0".repeat((-decpt) as usize), digits)
    } else if decpt >= len {
        format!("{}{}.0", digits, "0".repeat((decpt - len) as usize))
    } else {
        let split = decpt as usize;
        format!("{}.{}", &digits[..split], &digits[split..])
    }
}

/// CPython-compatible numeric hash for finite floats and infinities.
#[must_use]
pub fn hash_f64(value: f64) -> isize {
    const HASH_BITS: i32 = 61;
    const HASH_MODULUS: u128 = (1_u128 << HASH_BITS) - 1;
    const HASH_INF: isize = 314_159;

    if value == 0.0 {
        return 0;
    }
    if value.is_nan() {
        let hash = value.to_bits() as isize;
        return if hash == -1 { -2 } else { hash };
    }
    if value.is_infinite() {
        return if value.is_sign_negative() { -HASH_INF } else { HASH_INF };
    }

    let bits = value.to_bits();
    let negative = bits >> 63 != 0;
    let exp_bits = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1_u64 << 52) - 1);
    let (mut mantissa, mut exponent) = if exp_bits == 0 {
        (frac, 1 - 1023 - 52)
    } else {
        ((1_u64 << 52) | frac, exp_bits - 1023 - 52)
    };
    while exponent < 0 && mantissa & 1 == 0 {
        mantissa >>= 1;
        exponent += 1;
    }

    let mut hash = (u128::from(mantissa) % HASH_MODULUS) as i128;
    if exponent >= 0 {
        hash = (hash * pow2_mod(exponent as u32) as i128) % HASH_MODULUS as i128;
    } else {
        let denom_power = (-exponent) % HASH_BITS;
        let inverse_power = if denom_power == 0 { 0 } else { HASH_BITS - denom_power };
        hash = (hash * pow2_mod(inverse_power as u32) as i128) % HASH_MODULUS as i128;
    }
    if negative {
        hash = -hash;
    }
    if hash == -1 { -2 } else { hash as isize }
}

fn pow2_mod(exponent: u32) -> u128 {
    const HASH_BITS: u32 = 61;
    const HASH_MODULUS: u128 = (1_u128 << HASH_BITS) - 1;
    let shift = exponent % HASH_BITS;
    if shift == 0 { 1 } else { (1_u128 << shift) % HASH_MODULUS }
}

/// Returns the float protocol slot table.
#[must_use]
pub fn number_methods_ptr() -> *mut PyNumberMethods {
    *FLOAT_NUMBER_METHODS as *mut PyNumberMethods
}

unsafe extern "C" fn hash_slot(object: *mut PyObject) -> isize {
    match unsafe { to_f64(object) } {
        Some(value) => hash_f64(value),
        None => -1,
    }
}

unsafe extern "C" fn bool_slot(object: *mut PyObject) -> c_int {
    match unsafe { to_f64(object) } {
        Some(0.0) => 0,
        Some(_) => 1,
        None => -1,
    }
}

unsafe extern "C" fn nb_float(object: *mut PyObject) -> *mut PyObject {
    match unsafe { to_f64(object) } {
        Some(value) => from_f64(value),
        None => raise_type_error("float expected"),
    }
}

unsafe extern "C" fn nb_negative(object: *mut PyObject) -> *mut PyObject {
    match unsafe { to_f64(object) } {
        Some(value) => from_f64(-value),
        None => raise_type_error("bad operand type for unary -"),
    }
}

unsafe extern "C" fn nb_absolute(object: *mut PyObject) -> *mut PyObject {
    match unsafe { to_f64(object) } {
        Some(value) => from_f64(value.abs()),
        None => raise_type_error("bad operand type for abs()"),
    }
}

unsafe extern "C" fn nb_positive(object: *mut PyObject) -> *mut PyObject {
    unsafe { nb_float(object) }
}

fn make_number_methods() -> PyNumberMethods {
    PyNumberMethods {
        nb_add: Some(crate::types::int::nb_add),
        nb_subtract: Some(crate::types::int::nb_subtract),
        nb_multiply: Some(crate::types::int::nb_multiply),
        nb_remainder: Some(crate::types::int::nb_remainder),
        nb_power: Some(crate::types::int::nb_power),
        nb_negative: Some(nb_negative),
        nb_positive: Some(nb_positive),
        nb_absolute: Some(nb_absolute),
        nb_bool: Some(bool_slot),
        nb_float: Some(nb_float),
        nb_floor_divide: Some(crate::types::int::nb_floor_divide),
        nb_true_divide: Some(crate::types::int::nb_true_divide),
        nb_reflected_add: Some(crate::types::int::nb_add),
        nb_reflected_subtract: Some(crate::types::int::nb_subtract),
        nb_reflected_multiply: Some(crate::types::int::nb_multiply),
        nb_reflected_remainder: Some(crate::types::int::nb_remainder),
        nb_reflected_power: Some(crate::types::int::nb_power),
        nb_reflected_floor_divide: Some(crate::types::int::nb_floor_divide),
        nb_reflected_true_divide: Some(crate::types::int::nb_true_divide),
        ..PyNumberMethods::EMPTY
    }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

// ---------------------------------------------------------------------------
// float type-object surface
// ---------------------------------------------------------------------------

/// One-shot installer for the builtin `float` type object's `tp_dict`
/// surface.  CPython exposes `__getformat__` and `fromhex` as type-level
/// callables; pon carries both as staticmethod descriptors around
/// receiverless native entries exactly like `bytes.fromhex`
/// (`abi::str_::install_binary_type_methods`).  `ty` is the GLOBAL `float`
/// type object ([`FLOAT_TYPE`], registered by
/// `abi::register_builtin_type_globals`); `descr::synthetic_type_attr`
/// triggers this on first type-level attribute access, and existing
/// `tp_dict` entries are kept.
pub(crate) fn ensure_float_type_methods_installed(ty: *mut PyType) {
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if ty.is_null() || INSTALLED.load(AtomicOrdering::SeqCst) {
        return;
    }
    // Pre-runtime call sites must not latch a no-op install: the function
    // allocations below need a live runtime.
    if crate::abi::runtime_type_type().is_null() {
        return;
    }
    if INSTALLED.swap(true, AtomicOrdering::SeqCst) {
        return;
    }
    let namespace = unsafe { (*ty).tp_dict.cast::<crate::types::type_::PyClassDict>() };
    let namespace = if namespace.is_null() { crate::types::type_::new_namespace() } else { namespace };
    for (method_name, entry) in [
        ("__getformat__", float_getformat_entry as unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject),
        ("fromhex", float_fromhex_entry as unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject),
    ] {
        let interned = crate::intern::intern(method_name);
        if unsafe { (&*namespace).get(interned) }.is_some() {
            continue;
        }
        // SAFETY: Live builtin entry point with the runtime calling convention.
        let function = unsafe {
            crate::abi::pon_make_function(
                entry as *const u8,
                crate::builtins::variadic_arity(),
                interned,
            )
        };
        if function.is_null() {
            continue;
        }
        // SAFETY: Staticmethod carrier over the fresh receiverless entry.
        let descriptor = unsafe {
            crate::types::classmethod::new_staticmethod(crate::abi::staticmethod_builtin_type(), function)
        };
        if !descriptor.is_null() {
            unsafe { (&mut *namespace).set(interned, descriptor) };
        }
    }
    unsafe {
        (*ty).tp_dict = namespace.cast::<PyObject>();
    }
    // GC rooting for the namespace values plus IC invalidation for any
    // AttrIC guarding the type object.
    crate::sync::register_namespaced_type(ty);
    crate::sync::type_modified(ty);
}

/// `float.__getformat__(typestr)`: `'IEEE, little-endian'` /
/// `'IEEE, big-endian'` selected by target endianness — Rust `f64`/`f32` are
/// IEEE 754 by definition, so the CPython "unknown" detection arm cannot
/// occur.  Message texts mirror the CPython 3.14 oracle byte-for-byte; the
/// staticmethod carrier means no receiver reaches the entry (type-level and
/// instance-level access both pass the format string alone).
unsafe extern "C" fn float_getformat_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() || argc != 1 {
        return raise_type_error(&format!("float.__getformat__() takes exactly one argument ({argc} given)"));
    }
    // SAFETY: One live argument slot per the check above; tagged immediates
    // box through `untag_arg` so the type-name probe below reads a header.
    let argument = crate::tag::untag_arg(unsafe { *argv });
    if argument.is_null() {
        return ptr::null_mut();
    }
    let Some(kind) = (unsafe { crate::types::type_::unicode_text(argument) }) else {
        let got = unsafe { crate::types::dict::type_name(argument) }.unwrap_or("object");
        return raise_type_error(&format!("__getformat__() argument must be str, not {got}"));
    };
    if kind != "double" && kind != "float" {
        const MESSAGE: &str = "__getformat__() argument 1 must be 'double' or 'float'";
        // SAFETY: Raise helper with a static message.
        return unsafe { crate::abi::exc::pon_raise_value_error(MESSAGE.as_ptr(), MESSAGE.len()) };
    }
    const FORMAT: &str = if cfg!(target_endian = "little") { "IEEE, little-endian" } else { "IEEE, big-endian" };
    // SAFETY: Runtime string allocation helper; NULL on failure with the error set.
    unsafe { crate::abi::pon_const_str(FORMAT.as_ptr(), FORMAT.len()) }
}

/// `float.fromhex(text)`: hexadecimal float parser covering CPython's public
/// grammar (`[sign]0xH[.H]p[sign]D`, plus infinities and NaNs).  Rust's
/// standard parser deliberately omits C99 hex floats, so this routine performs
/// the exact base-16 mantissa / base-2 exponent composition itself.
unsafe extern "C" fn float_fromhex_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() || argc != 1 {
        return raise_type_error(&format!("float.fromhex() takes exactly one argument ({argc} given)"));
    }
    // SAFETY: One live argument slot per the check above.
    let argument = crate::tag::untag_arg(unsafe { *argv });
    let Some(text) = (unsafe { crate::types::type_::unicode_text(argument) }) else {
        let got = unsafe { crate::types::dict::type_name(argument) }.unwrap_or("object");
        return raise_type_error(&format!("fromhex() argument must be str, not {got}"));
    };
    match parse_hex_float(text) {
        Ok(value) => from_f64(value),
        Err(()) => {
            const MESSAGE: &str = "invalid hexadecimal floating-point string";
            // SAFETY: Raise helper with a static message.
            unsafe { crate::abi::exc::pon_raise_value_error(MESSAGE.as_ptr(), MESSAGE.len()) }
        }
    }
}

fn parse_hex_float(text: &str) -> Result<f64, ()> {
    let mut s = text.trim();
    let sign = if let Some(rest) = s.strip_prefix('-') {
        s = rest;
        -1.0
    } else {
        if let Some(rest) = s.strip_prefix('+') {
            s = rest;
        }
        1.0
    };
    let lower = s.to_ascii_lowercase();
    return match lower.as_str() {
        "inf" | "infinity" => Ok(sign * f64::INFINITY),
        "nan" => Ok(f64::NAN),
        _ => parse_finite_hex_float(s).map(|value| sign * value),
    };
}

fn parse_finite_hex_float(s: &str) -> Result<f64, ()> {
    let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) else {
        return Err(());
    };
    let (mantissa_text, exponent_text) = rest.split_once('p').or_else(|| rest.split_once('P')).ok_or(())?;
    if mantissa_text.is_empty() || exponent_text.is_empty() {
        return Err(());
    }
    let exponent: i32 = exponent_text.parse().map_err(|_| ())?;
    let mut value = 0.0f64;
    let mut seen_digit = false;
    let mut seen_point = false;
    let mut fractional_digits = 0i32;
    for ch in mantissa_text.chars() {
        if ch == '.' {
            if seen_point {
                return Err(());
            }
            seen_point = true;
            continue;
        }
        let digit = ch.to_digit(16).ok_or(())?;
        seen_digit = true;
        value = value * 16.0 + f64::from(digit);
        if seen_point {
            fractional_digits = fractional_digits.saturating_add(1);
        }
    }
    if !seen_digit {
        return Err(());
    }
    let binary_exponent = exponent.saturating_sub(fractional_digits.saturating_mul(4));
    Ok(value * 2.0f64.powi(binary_exponent))
}
