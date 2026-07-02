//! Integer numeric tower implementation.

use core::ffi::c_int;
use core::ptr;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use num_bigint::{BigInt, Sign};
use num_traits::{One, Signed, ToPrimitive, Zero};

use crate::abi;
use crate::object::{PyLong, PyNumberMethods, PyObject, PyObjectHeader};

static BIG_INTS: LazyLock<Mutex<HashMap<usize, BigInt>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static INT_NUMBER_METHODS: LazyLock<usize> =
    LazyLock::new(|| Box::into_raw(Box::new(make_number_methods())) as usize);

/// Returns true when `object` has a runtime type whose name bytes match `expected`.
#[must_use]
pub unsafe fn type_name_is(object: *mut PyObject, expected: &str) -> bool {
    if object.is_null() {
        return false;
    }
    let ty = unsafe { (*object).ob_type };
    if ty.is_null() {
        return false;
    }
    let ty = unsafe { &*ty };
    if ty.name.is_null() && ty.name_len != 0 {
        return false;
    }
    let name = unsafe { core::slice::from_raw_parts(ty.name, ty.name_len) };
    name == expected.as_bytes()
}

/// Returns true for exact `int` objects, not for `bool`.
#[must_use]
pub unsafe fn is_exact_int(object: *mut PyObject) -> bool {
    unsafe { type_name_is(object, "int") }
}

/// Extracts the arbitrary-precision integer payload for an exact `int` or an
/// int-subclass instance (IntEnum members, `_NamedIntConstant`, ...), reading
/// the latter through its embedded canonical payload.
#[must_use]
pub unsafe fn to_bigint(object: *mut PyObject) -> Option<BigInt> {
    let object = unsafe { crate::types::type_::payload_subclass_value(object) }
        .map(crate::tag::untag_arg)
        .unwrap_or(object);
    if !unsafe { is_exact_int(object) } {
        return None;
    }
    let key = object as usize;
    if let Some(value) = BIG_INTS.lock().unwrap_or_else(|poison| poison.into_inner()).get(&key) {
        return Some(value.clone());
    }
    Some(BigInt::from(unsafe { (*object.cast::<PyLong>()).value }))
}

/// Boxes an arbitrary-precision integer as a `PyLong`.
///
/// Values that fit in the Phase-A inline `i64` payload keep using
/// `pon_const_int`, preserving the existing small-integer path. Wider values are
/// represented by a normal `PyLong` shell plus an out-of-line BigInt payload
/// keyed by object address.
#[must_use]
pub fn from_bigint(value: BigInt) -> *mut PyObject {
    if let Some(inline) = value.to_i64() {
        return unsafe { abi::pon_const_int(inline) };
    }
    let template = unsafe { abi::pon_const_int(0) };
    if template.is_null() {
        return template;
    }
    let ty = unsafe { (*template).ob_type };
    let object = Box::into_raw(Box::new(PyLong {
        ob_base: PyObjectHeader::new(ty),
        value: 0,
    })) as *mut PyObject;
    BIG_INTS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(object as usize, value);
    object
}

/// Boxes a signed 64-bit integer through the compatibility constructor.
#[must_use]
pub fn from_i64(value: i64) -> *mut PyObject {
    unsafe { abi::pon_const_int(value) }
}

/// Boxes the value of a compiler-validated integer-literal token wider than
/// `i64` (decimal or `0b`/`0o`/`0x` prefixed, `_` separators allowed).
///
/// Returns NULL with a `ValueError` set when the token does not parse, which
/// only happens for callers bypassing the Python lexer.
#[must_use]
pub fn from_literal_token(text: &str) -> *mut PyObject {
    match parse_int_text(text, 0) {
        Ok(value) => from_bigint(value),
        Err(message) => raise_value_error(&message),
    }
}

/// Extracts an integer payload from exact `int` and `bool` objects.
#[must_use]
pub unsafe fn to_bigint_including_bool(object: *mut PyObject) -> Option<BigInt> {
    if let Some(value) = unsafe { crate::types::bool_::to_bool(object) } {
        return Some(BigInt::from(i32::from(value)));
    }
    unsafe { to_bigint(object) }
}

/// Implements the built-in `int()` constructor once the builtin shim has sliced argv.
#[must_use]
pub fn construct_from_args(args: &[*mut PyObject]) -> *mut PyObject {
    match args.len() {
        0 => from_i64(0),
        1 => unsafe { construct_one(args[0]) },
        2 => unsafe { construct_with_base(args[0], args[1]) },
        len => raise_type_error(&format!("int() expected at most 2 arguments, got {len}")),
    }
}

/// Converts a finite `f64` to the exact integer obtained by truncating toward zero.
#[must_use]
pub fn bigint_from_f64_trunc(value: f64) -> Option<BigInt> {
    if !value.is_finite() {
        return None;
    }
    if value == 0.0 {
        return Some(BigInt::zero());
    }

    let bits = value.to_bits();
    let negative = bits >> 63 != 0;
    let exp_bits = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1_u64 << 52) - 1);
    let (mantissa, exponent) = if exp_bits == 0 {
        (frac, 1 - 1023 - 52)
    } else {
        ((1_u64 << 52) | frac, exp_bits - 1023 - 52)
    };
    let mut value = BigInt::from(mantissa);
    if exponent >= 0 {
        value <<= exponent as usize;
    } else {
        value >>= (-exponent) as usize;
    }
    if negative {
        value = -value;
    }
    Some(value)
}

unsafe fn construct_one(object: *mut PyObject) -> *mut PyObject {
    // `int`/`str`-subclass instances (IntEnum/StrEnum members, ...) convert
    // through their embedded canonical payload (CPython `int(x)` reads the
    // base value of an int subclass).
    let object = unsafe { crate::types::type_::payload_subclass_value(object) }.unwrap_or(object);
    if let Some(value) = unsafe { to_bigint_including_bool(object) } {
        return from_bigint(value);
    }
    if let Some(value) = unsafe { crate::types::float::to_f64(object) } {
        if value.is_nan() {
            return raise_value_error("cannot convert float NaN to integer");
        }
        if value.is_infinite() {
            return raise_value_error("cannot convert float infinity to integer");
        }
        return match bigint_from_f64_trunc(value) {
            Some(value) => from_bigint(value),
            None => raise_value_error("cannot convert float infinity to integer"),
        };
    }
    if let Some(text) = unsafe { crate::types::type_::unicode_text(object) } {
        return match parse_int_text(text, 10) {
            Ok(value) => from_bigint(value),
            Err(message) => raise_value_error(&message),
        };
    }
    if let Some(bytes) = unsafe { bytes_like_slice(object) } {
        return match bytes_like_text(bytes, 10).and_then(|text| parse_int_text(text, 10)) {
            Ok(value) => from_bigint(value),
            Err(message) => raise_value_error(&message),
        };
    }
    raise_type_error("int() argument must be a string, a bytes-like object or a real number, not object")
}

unsafe fn construct_with_base(object: *mut PyObject, base_object: *mut PyObject) -> *mut PyObject {
    let unicode = unsafe { crate::types::type_::unicode_text(object) };
    let bytes = if unicode.is_none() { unsafe { bytes_like_slice(object) } } else { None };
    if unicode.is_none() && bytes.is_none() {
        return raise_type_error("int() can't convert non-string with explicit base");
    }
    let Some(base) = (unsafe { to_bigint_including_bool(base_object).and_then(|value| value.to_i32()) }) else {
        return raise_value_error("int() base must be >= 2 and <= 36, or 0");
    };
    if base != 0 && !(2..=36).contains(&base) {
        return raise_value_error("int() base must be >= 2 and <= 36, or 0");
    }
    let text = match (unicode, bytes) {
        (Some(text), _) => text,
        (None, Some(bytes)) => match bytes_like_text(bytes, base) {
            Ok(text) => text,
            Err(message) => return raise_value_error(&message),
        },
        (None, None) => unreachable!("guarded above"),
    };
    match parse_int_text(text, base) {
        Ok(value) => from_bigint(value),
        Err(message) => raise_value_error(&message),
    }
}

/// Borrows the payload of an exact bytes or bytearray object.
unsafe fn bytes_like_slice<'a>(object: *mut PyObject) -> Option<&'a [u8]> {
    if object.is_null() {
        return None;
    }
    let ty = unsafe { (*object).ob_type };
    if crate::types::bytes_::is_bytes_type(ty) {
        return Some(unsafe { (*object.cast::<crate::types::bytes_::PyBytes>()).as_slice() });
    }
    if crate::types::bytearray_::is_bytearray_type(ty) {
        return Some(unsafe { (*object.cast::<crate::types::bytearray_::PyByteArray>()).as_slice() });
    }
    None
}

/// Decodes an int literal payload from a bytes-like object, mirroring
/// CPython's ASCII requirement for `int(b'...', base)`.
fn bytes_like_text(bytes: &[u8], base: i32) -> Result<&str, String> {
    core::str::from_utf8(bytes).map_err(|_| invalid_int_literal(&crate::types::bytes_::repr(bytes), base))
}

fn parse_int_text(text: &str, requested_base: i32) -> Result<BigInt, String> {
    let trimmed = text.trim();
    let invalid = || invalid_int_literal(text, requested_base);
    if trimmed.is_empty() {
        return Err(invalid());
    }

    let mut rest = trimmed;
    let mut negative = false;
    if let Some(after) = rest.strip_prefix('+') {
        rest = after;
    } else if let Some(after) = rest.strip_prefix('-') {
        negative = true;
        rest = after;
    }
    if rest.is_empty() {
        return Err(invalid());
    }

    let (base, digits, prefixed) = detect_base(rest, requested_base)?;
    let value = parse_digits(digits, base, prefixed).ok_or_else(invalid)?;
    if requested_base == 0 && !prefixed && decimal_base_zero_is_invalid(digits, &value) {
        return Err(invalid());
    }
    Ok(if negative { -value } else { value })
}

fn detect_base(rest: &str, requested_base: i32) -> Result<(u32, &str, bool), String> {
    if requested_base != 0 && !(2..=36).contains(&requested_base) {
        return Err("int() base must be >= 2 and <= 36, or 0".to_owned());
    }

    let lower = rest.as_bytes();
    let prefix_base = if lower.len() >= 2 && lower[0] == b'0' {
        match lower[1].to_ascii_lowercase() {
            b'b' => Some(2),
            b'o' => Some(8),
            b'x' => Some(16),
            _ => None,
        }
    } else {
        None
    };

    match (requested_base, prefix_base) {
        (0, Some(base)) => Ok((base, &rest[2..], true)),
        (0, None) => Ok((10, rest, false)),
        (base, Some(prefix)) if base as u32 == prefix => Ok((prefix, &rest[2..], true)),
        (base, _) => Ok((base as u32, rest, false)),
    }
}

fn parse_digits(digits: &str, base: u32, prefixed: bool) -> Option<BigInt> {
    let mut value = BigInt::zero();
    let mut saw_digit = false;
    let mut previous_digit = false;
    let mut after_prefix = prefixed;
    for ch in digits.chars() {
        if ch == '_' {
            if !previous_digit && !after_prefix {
                return None;
            }
            previous_digit = false;
            after_prefix = false;
            continue;
        }
        let digit = digit_value(ch)?;
        if digit >= base {
            return None;
        }
        value = value * base + digit;
        saw_digit = true;
        previous_digit = true;
        after_prefix = false;
    }
    if !saw_digit || !previous_digit {
        return None;
    }
    Some(value)
}

fn digit_value(ch: char) -> Option<u32> {
    match ch {
        '0'..='9' => Some(u32::from(ch as u8 - b'0')),
        'a'..='z' => Some(u32::from(ch as u8 - b'a') + 10),
        'A'..='Z' => Some(u32::from(ch as u8 - b'A') + 10),
        _ => None,
    }
}

fn decimal_base_zero_is_invalid(digits: &str, value: &BigInt) -> bool {
    digits.starts_with('0') && !value.is_zero()
}

fn invalid_int_literal(text: &str, base: i32) -> String {
    format!("invalid literal for int() with base {base}: {}", python_string_repr(text))
}

fn python_string_repr(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push_str(&format!("\\x{:02x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('\'');
    out
}

/// Installs integer slots on the runtime `int` type reached from an object.
pub unsafe fn install_slots_for_object(object: *mut PyObject) {
    if !unsafe { is_exact_int(object) } {
        return;
    }
    let ty = unsafe { (*object).ob_type.cast_mut() };
    if ty.is_null() {
        return;
    }
    unsafe {
        (*ty).tp_hash = Some(hash_slot);
        (*ty).tp_bool = Some(bool_slot);
        (*ty).tp_as_number = number_methods_ptr();
    }
}

/// Instance attribute surface for exact `int`/`bool` receivers (slotless
/// native types reach here from `abstract_op::get_attr`): `bit_length`/
/// `bit_count`/`__index__`/`__trunc__` bound methods plus the numeric-tower
/// value attributes (`operator.index` calls `a.__index__()` in the vendored
/// stdlib, so the dunder must resolve as an instance attribute).
pub unsafe fn int_instance_attr(object: *mut PyObject, name: u32) -> Option<*mut PyObject> {
    let value = unsafe { to_bigint_including_bool(crate::tag::untag_arg(object)) }?;
    let name_text = crate::intern::resolve(name)?;
    match name_text.as_str() {
        "bit_length" => bound_int_method(object, name, int_bit_length_method),
        "bit_count" => bound_int_method(object, name, int_bit_count_method),
        "to_bytes" => bound_int_method(object, name, int_to_bytes_method),
        "__index__" | "__int__" | "__trunc__" | "__floor__" | "__ceil__" => {
            bound_int_method(object, name, int_identity_method)
        }
        "numerator" | "real" => Some(from_bigint(value)),
        "denominator" => Some(from_bigint(BigInt::from(1))),
        "imag" => Some(from_bigint(BigInt::from(0))),
        _ => None,
    }
}

fn bound_int_method(
    receiver: *mut PyObject,
    name: u32,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Option<*mut PyObject> {
    let function = unsafe { crate::abi::pon_make_function(entry as *const u8, crate::builtins::variadic_arity(), name) };
    if function.is_null() {
        return Some(core::ptr::null_mut());
    }
    match crate::types::method::new_bound_method(function, receiver) {
        Ok(method) => Some(method.cast::<PyObject>()),
        Err(message) => Some(raise_type_error(&message)),
    }
}
/// `int.__index__`/`__int__`/`__trunc__`/`__floor__`/`__ceil__`: identity on
/// exact ints (CPython returns self; the runtime's canonical boxing keeps
/// value identity).
unsafe extern "C" fn int_identity_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    match unsafe { int_method_receiver(argv, argc, "__index__") } {
        Ok(value) => from_bigint(value),
        Err(error) => error,
    }
}

unsafe fn int_method_receiver(argv: *mut *mut PyObject, argc: usize, name: &str) -> Result<BigInt, *mut PyObject> {
    if argc != 1 || argv.is_null() {
        return Err(raise_type_error(&format!("int.{name}() takes no arguments")));
    }
    let receiver = unsafe { crate::tag::untag_arg(*argv) };
    match unsafe { to_bigint_including_bool(receiver) } {
        Some(value) => Ok(value),
        None => Err(raise_type_error(&format!("int.{name}() receiver must be int"))),
    }
}

unsafe extern "C" fn int_bit_length_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    match unsafe { int_method_receiver(argv, argc, "bit_length") } {
        Ok(value) => from_bigint(BigInt::from(value.bits())),
        Err(error) => error,
    }
}

unsafe extern "C" fn int_bit_count_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    match unsafe { int_method_receiver(argv, argc, "bit_count") } {
        Ok(value) => from_bigint(BigInt::from(value.magnitude().count_ones())),
        Err(error) => error,
    }
}

/// `int.to_bytes(length=1, byteorder='big', *, signed=False)` — bound
/// instance method: `argv[0]` is the receiver; keyword slots arrive
/// positionally with None filling absent values (`types::function` binder
/// arm).  `importlib._bootstrap_external` calls it at module scope
/// (`MAGIC_NUMBER = (3610).to_bytes(2, 'little')`, pyc header tokens).
unsafe extern "C" fn int_to_bytes_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() || argc == 0 || argc > 4 {
        return raise_type_error(&format!("to_bytes() takes 0 to 3 arguments ({} given)", argc.saturating_sub(1)));
    }
    // SAFETY: The caller passes a live argv window of length argc.
    let args: Vec<*mut PyObject> =
        unsafe { core::slice::from_raw_parts(argv, argc) }.iter().map(|&arg| crate::tag::untag_arg(arg)).collect();
    let Some(value) = (unsafe { to_bigint_including_bool(args[0]) }) else {
        return raise_type_error("to_bytes() receiver must be an integer");
    };
    let is_none = |object: *mut PyObject| {
        // SAFETY: Type probe tolerates any live object.
        let name = unsafe { crate::types::dict::type_name(object) };
        name == Some("NoneType")
    };
    let length = match args.get(1).copied().filter(|&len| !is_none(len)) {
        None => 1usize, // length defaults to 1
        Some(len) => {
            let Some(len) = (unsafe { to_bigint_including_bool(len) }) else {
                return raise_type_error("to_bytes() length argument must be an integer");
            };
            match len.to_isize() {
                Some(len) if len >= 0 => len as usize,
                Some(_) => return raise_value_error("length argument must be non-negative"),
                None => return raise_overflow_error("Python int too large to convert to C ssize_t"),
            }
        }
    };
    let little = match args.get(2).copied().filter(|&order| !is_none(order)) {
        None => false, // byteorder defaults to 'big'
        Some(order) => {
            // SAFETY: `unicode_text` type-checks its argument.
            match unsafe { crate::types::type_::unicode_text(order) } {
                Some("big") => false,
                Some("little") => true,
                _ => return raise_value_error("byteorder must be either 'little' or 'big'"),
            }
        }
    };
    let signed = match args.get(3).copied().filter(|&flag| !is_none(flag)) {
        None => false,
        // SAFETY: Truth helper follows the NULL-sentinel error contract.
        Some(flag) => match unsafe { crate::abstract_op::is_true(flag) } {
            negative if negative < 0 => return ptr::null_mut(),
            truth => truth != 0,
        },
    };
    let mut bytes = if value.sign() == Sign::NoSign {
        // Zero fits any width, including `(0).to_bytes(0, ...)` -> b''.
        vec![0u8; length]
    } else if value.sign() == Sign::Minus && !signed {
        return raise_overflow_error("can't convert negative int to unsigned");
    } else if signed {
        let mut le = value.to_signed_bytes_le();
        if le.len() > length {
            return raise_overflow_error("int too big to convert");
        }
        let fill = if value.sign() == Sign::Minus { 0xFF } else { 0x00 };
        le.resize(length, fill);
        le
    } else {
        let (_, mut le) = value.to_bytes_le();
        if le.len() > length {
            return raise_overflow_error("int too big to convert");
        }
        le.resize(length, 0x00);
        le
    };
    if !little {
        bytes.reverse();
    }
    // SAFETY: Runtime allocation helper; NULL on failure with the error set.
    unsafe { abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) }
}
/// Cached `int.from_bytes` function object served by type-level attribute
/// lookup (`descr::synthetic_type_attr`); classmethod semantics degenerate to
/// a plain function because the type receiver is not passed through.
#[must_use]
pub fn from_bytes_function() -> *mut PyObject {
    static FUNCTION: LazyLock<usize> = LazyLock::new(|| {
        let name = crate::intern::intern("from_bytes");
        // SAFETY: Live builtin entry point with the runtime calling convention.
        let function =
            unsafe { abi::pon_make_function(int_from_bytes_entry as *const u8, crate::builtins::variadic_arity(), name) };
        function as usize
    });
    *FUNCTION as *mut PyObject
}

/// `int.from_bytes(bytes, byteorder='big', *, signed=False)`; keyword slots
/// arrive positionally with None filling absent values (`types::function`
/// binder arm), and the `random.py` str-seed path calls it one-argument.
unsafe extern "C" fn int_from_bytes_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() || argc == 0 || argc > 3 {
        return raise_type_error(&format!("from_bytes() takes 1 to 3 arguments ({argc} given)"));
    }
    // SAFETY: The caller passes a live argv window of length argc.
    let args: Vec<*mut PyObject> =
        unsafe { core::slice::from_raw_parts(argv, argc) }.iter().map(|&arg| crate::tag::untag_arg(arg)).collect();
    let payload: Vec<u8> = {
        let data = args[0];
        // SAFETY: A non-NULL heap object carries a live header.
        let ty = unsafe { data.as_ref().map_or(ptr::null(), |object| object.ob_type) };
        if crate::types::bytes_::is_bytes_type(ty) {
            // SAFETY: Type check above proved the layout.
            unsafe { (*data.cast::<crate::types::bytes_::PyBytes>()).as_slice().to_vec() }
        } else if crate::types::bytearray_::is_bytearray_type(ty) {
            // SAFETY: Type check above proved the layout.
            unsafe { (*data.cast::<crate::types::bytearray_::PyByteArray>()).as_slice().to_vec() }
        } else {
            return raise_type_error("cannot convert non-bytes object to int");
        }
    };
    let is_none = |object: *mut PyObject| {
        // SAFETY: Type probe tolerates any live object.
        let name = unsafe { crate::types::dict::type_name(object) };
        name == Some("NoneType")
    };
    let little = match args.get(1).copied().filter(|&order| !is_none(order)) {
        None => false, // byteorder defaults to 'big'
        Some(order) => {
            // SAFETY: `unicode_text` type-checks its argument.
            match unsafe { crate::types::type_::unicode_text(order) } {
                Some("big") => false,
                Some("little") => true,
                _ => return raise_value_error("byteorder must be either 'little' or 'big'"),
            }
        }
    };
    let signed = match args.get(2).copied().filter(|&flag| !is_none(flag)) {
        None => false,
        // SAFETY: Truth helper follows the NULL-sentinel error contract.
        Some(flag) => match unsafe { crate::abstract_op::is_true(flag) } {
            negative if negative < 0 => return ptr::null_mut(),
            truth => truth != 0,
        },
    };
    let value = match (little, signed) {
        (false, false) => BigInt::from_bytes_be(Sign::Plus, &payload),
        (true, false) => BigInt::from_bytes_le(Sign::Plus, &payload),
        (false, true) => BigInt::from_signed_bytes_be(&payload),
        (true, true) => BigInt::from_signed_bytes_le(&payload),
    };
    from_bigint(value)
}

/// Returns the integer protocol slot table.
#[must_use]
pub fn number_methods_ptr() -> *mut PyNumberMethods {
    *INT_NUMBER_METHODS as *mut PyNumberMethods
}

/// CPython-style integer hash reduction using the 64-bit `PyHASH_MODULUS`.
#[must_use]
pub fn hash_bigint(value: &BigInt) -> isize {
    const HASH_BITS: usize = 61;
    let modulus = (BigInt::one() << HASH_BITS) - BigInt::one();
    let mut reduced = (value.abs() % &modulus).to_isize().unwrap_or(0);
    if value.sign() == Sign::Minus {
        reduced = -reduced;
    }
    if reduced == -1 { -2 } else { reduced }
}

unsafe extern "C" fn hash_slot(object: *mut PyObject) -> isize {
    match unsafe { to_bigint(object) } {
        Some(value) => hash_bigint(&value),
        None => -1,
    }
}

unsafe extern "C" fn bool_slot(object: *mut PyObject) -> c_int {
    match unsafe { to_bigint(object) } {
        Some(value) if value == BigInt::from(0) => 0,
        Some(_) => 1,
        None => -1,
    }
}

pub unsafe extern "C" fn nb_index(object: *mut PyObject) -> *mut PyObject {
    match unsafe { to_bigint(object) } {
        Some(value) => from_bigint(value),
        None => raise_type_error("object cannot be interpreted as an integer"),
    }
}

pub unsafe extern "C" fn nb_int(object: *mut PyObject) -> *mut PyObject {
    unsafe { nb_index(object) }
}

pub unsafe extern "C" fn nb_float(object: *mut PyObject) -> *mut PyObject {
    match unsafe { to_bigint(object).and_then(|value| value.to_f64()) } {
        Some(value) => crate::types::float::from_f64(value),
        None => raise_type_error("int too large to convert to float"),
    }
}

pub unsafe extern "C" fn nb_add(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_ADD, a, b) }
}

pub unsafe extern "C" fn nb_subtract(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_SUB, a, b) }
}

pub unsafe extern "C" fn nb_multiply(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_MUL, a, b) }
}

pub unsafe extern "C" fn nb_remainder(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_MOD, a, b) }
}

pub unsafe extern "C" fn nb_negative(object: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_unary_op(crate::abstract_op::UNARY_NEG, object, ptr::null_mut()) }
}

pub unsafe extern "C" fn nb_absolute(object: *mut PyObject) -> *mut PyObject {
    match unsafe { to_bigint(object) } {
        Some(value) => from_bigint(value.abs()),
        None => raise_type_error("bad operand type for abs()"),
    }
}

pub unsafe extern "C" fn nb_positive(object: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_unary_op(crate::abstract_op::UNARY_POS, object, ptr::null_mut()) }
}

pub unsafe extern "C" fn nb_invert(object: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_unary_op(crate::abstract_op::UNARY_INVERT, object, ptr::null_mut()) }
}

pub unsafe extern "C" fn nb_lshift(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_LSHIFT, a, b) }
}

pub unsafe extern "C" fn nb_rshift(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_RSHIFT, a, b) }
}

pub unsafe extern "C" fn nb_and(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_AND, a, b) }
}

pub unsafe extern "C" fn nb_xor(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_XOR, a, b) }
}

pub unsafe extern "C" fn nb_or(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_OR, a, b) }
}

pub unsafe extern "C" fn nb_floor_divide(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_FLOORDIV, a, b) }
}

pub unsafe extern "C" fn nb_true_divide(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_DIV, a, b) }
}

pub unsafe extern "C" fn nb_power(a: *mut PyObject, b: *mut PyObject, _modulo: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_POW, a, b) }
}

fn make_number_methods() -> PyNumberMethods {
    PyNumberMethods {
        nb_add: Some(nb_add),
        nb_subtract: Some(nb_subtract),
        nb_multiply: Some(nb_multiply),
        nb_remainder: Some(nb_remainder),
        nb_power: Some(nb_power),
        nb_negative: Some(nb_negative),
        nb_positive: Some(nb_positive),
        nb_absolute: Some(nb_absolute),
        nb_bool: Some(bool_slot),
        nb_invert: Some(nb_invert),
        nb_lshift: Some(nb_lshift),
        nb_rshift: Some(nb_rshift),
        nb_and: Some(nb_and),
        nb_xor: Some(nb_xor),
        nb_or: Some(nb_or),
        nb_int: Some(nb_int),
        nb_float: Some(nb_float),
        nb_floor_divide: Some(nb_floor_divide),
        nb_true_divide: Some(nb_true_divide),
        nb_index: Some(nb_index),
        nb_reflected_add: Some(nb_add),
        nb_reflected_subtract: Some(nb_subtract),
        nb_reflected_multiply: Some(nb_multiply),
        nb_reflected_remainder: Some(nb_remainder),
        nb_reflected_power: Some(nb_power),
        nb_reflected_lshift: Some(nb_lshift),
        nb_reflected_rshift: Some(nb_rshift),
        nb_reflected_and: Some(nb_and),
        nb_reflected_xor: Some(nb_xor),
        nb_reflected_or: Some(nb_or),
        nb_reflected_floor_divide: Some(nb_floor_divide),
        nb_reflected_true_divide: Some(nb_true_divide),
        ..PyNumberMethods::EMPTY
    }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn raise_value_error(message: &str) -> *mut PyObject {
    unsafe { abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) }
}

fn raise_overflow_error(message: &str) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(crate::types::exc::ExceptionKind::OverflowError, message)
}
