//! Numeric helper family namespace.
//!
//! The Phase-B numeric ABI owns the tier-0 behavior for built-in numeric
//! objects.  It keeps the legacy `pon_const_int`/`pon_binary_add` path working
//! while routing concrete int/bool/float/complex operands through real numeric
//! semantics before falling back to generic slot dispatch for foreign objects.

use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{Signed, ToPrimitive, Zero};

use crate::abstract_op;
use crate::feedback::FeedbackCell;
use crate::object::PyObject;
use crate::types::{bool_, complex_, float, int};

/// Numeric operation selector passed through the helper ABI.
pub type NumberOp = u8;

pub use abstract_op::{
    BINARY_ADD, BINARY_AND, BINARY_DIV, BINARY_FLOORDIV, BINARY_LSHIFT, BINARY_MATMUL, BINARY_MOD, BINARY_MUL,
    BINARY_OR, BINARY_POW, BINARY_RSHIFT, BINARY_SUB, BINARY_XOR, UNARY_INVERT, UNARY_NEG, UNARY_POS,
};

enum NumericValue {
    Int(BigInt),
    Float(f64),
    Complex(f64, f64),
}

/// Creates a boxed Python `float`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_const_float(value: f64) -> *mut PyObject {
    super::catch_object_helper(|| float::from_f64(value))
}

/// Creates a boxed Python `bool` singleton.  Any non-zero argument is true.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_const_bool(value: i32) -> *mut PyObject {
    super::catch_object_helper(|| bool_::from_bool(value != 0))
}

/// Creates a boxed Python `complex`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_const_complex(real: f64, imag: f64) -> *mut PyObject {
    super::catch_object_helper(|| complex_::from_f64s(real, imag))
}

/// Creates a boxed Python `int` from an integer-literal token wider than
/// `i64`: `len` UTF-8 bytes at `ptr`, decimal or `0b`/`0o`/`0x` prefixed,
/// `_` separators allowed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_const_bigint(ptr: *const u8, len: usize) -> *mut PyObject {
    super::catch_object_helper(|| {
        if ptr.is_null() {
            return super::return_null_with_error("bigint literal pointer is null");
        }
        // SAFETY: The caller supplies `len` bytes at non-null `ptr`.
        let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
        let Ok(text) = core::str::from_utf8(bytes) else {
            return super::return_null_with_error("bigint literal is not valid UTF-8");
        };
        int::from_literal_token(text)
    })
}

/// Dispatches a Python binary operation and returns NULL with the current
/// exception set on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_binary_op(
    op: NumberOp,
    a: *mut PyObject,
    b: *mut PyObject,
    feedback: *mut FeedbackCell,
) -> *mut PyObject {
    crate::untag_prelude!(a, b);
    unsafe { super::record_feedback_binary(feedback, a, b) };
    unsafe { binary_op_with_flavor(op, a, b, false) }
}

/// Shared binary entry with the terminal-TypeError flavor threaded through
/// (`|` vs `|=`).  Numeric pairs compute directly; a numeric-domain
/// `NotImplemented` (`1.5 | 2`, `1 @ 2`) re-enters the full slot dispatch so
/// reflected slots and Python dunders still get a look before the
/// operand-typed TypeError.
pub(crate) unsafe fn binary_op_with_flavor(
    op: NumberOp,
    a: *mut PyObject,
    b: *mut PyObject,
    inplace: bool,
) -> *mut PyObject {
    super::catch_object_helper(|| {
        unsafe {
            install_runtime_int_slots(a);
            install_runtime_int_slots(b);
        }

        match unsafe { (numeric_value(a), numeric_value(b)) } {
            (Some(left), Some(right)) => {
                let result = binary_numeric(op, left, right);
                if !result.is_null() && unsafe { abstract_op::is_not_implemented(result) } {
                    unsafe { abstract_op::binary_op_flavored(op, a, b, inplace) }
                } else {
                    result
                }
            }
            (Some(_), None) | (None, Some(_))
                if (op == BINARY_MATMUL || op == BINARY_POW)
                    && !unsafe {
                        crate::types::type_::is_payload_subclass_instance(a)
                            || crate::types::type_::is_payload_subclass_instance(b)
                    } =>
            {
                unsafe { abstract_op::raise_binary_unsupported(op, a, b, inplace) }
            }
            _ => unsafe { abstract_op::binary_op_flavored(op, a, b, inplace) },
        }
    })
}

pub unsafe fn pon_binary_numeric_slot(op: NumberOp, a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| match unsafe { (numeric_value(a), numeric_value(b)) } {
        (Some(left), Some(right)) => binary_numeric(op, left, right),
        // Foreign operand: report NotImplemented so `abstract_op::binary_op`
        // falls through to reflected slots, Python-level dunders, and the
        // payload-subclass terminus instead of aborting (CPython slots return
        // NotImplemented for operands they do not handle).
        _ => unsafe { super::pon_not_implemented() },
    })
}

/// Phase-B helper-table alias for binary numeric dispatch.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_number_binary(
    op: NumberOp,
    a: *mut PyObject,
    b: *mut PyObject,
    feedback: *mut FeedbackCell,
) -> *mut PyObject {
    crate::untag_prelude!(a, b);
    unsafe { pon_binary_op(op, a, b, feedback) }
}

/// Dispatches a Python unary operation and returns NULL with the current
/// exception set on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_unary_op(op: NumberOp, a: *mut PyObject, feedback: *mut FeedbackCell) -> *mut PyObject {
    crate::untag_prelude!(a);
    unsafe { super::record_feedback_unary(feedback, a) };
    super::catch_object_helper(|| {
        unsafe {
            install_runtime_int_slots(a);
        }
        match unsafe { numeric_value(a) } {
            Some(value) => unary_numeric(op, value),
            None => unsafe { abstract_op::unary_op(op, a) },
        }
    })
}

/// Phase-B helper-table alias for unary numeric dispatch.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_number_unary(op: NumberOp, a: *mut PyObject, feedback: *mut FeedbackCell) -> *mut PyObject {
    crate::untag_prelude!(a);
    unsafe { pon_unary_op(op, a, feedback) }
}

/// Augmented assignment: the receiver's in-place slot/`__i*__` dunder runs
/// first (CPython `binary_iop1`; PEP 584 `dict.__ior__` mutates and returns
/// the SAME object); anything unhandled falls back to the plain binary
/// dispatch, whose terminal TypeError uses the `=`-suffixed spelling.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_number_inplace(
    op: NumberOp,
    a: *mut PyObject,
    b: *mut PyObject,
    feedback: *mut FeedbackCell,
) -> *mut PyObject {
    crate::untag_prelude!(a, b);
    unsafe { super::record_feedback_binary(feedback, a, b) };
    super::catch_object_helper(|| {
        if let Some(result) = unsafe { abstract_op::try_inplace_binary(op, a, b) } {
            return result;
        }
        unsafe { binary_op_with_flavor(op, a, b, true) }
    })
}

/// Implements `operator.index`: exact ints pass through, bool converts to `0`
/// or `1`, and non-indexable values raise `TypeError`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_index(object: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(object);
    super::catch_object_helper(|| {
        unsafe {
            install_runtime_int_slots(object);
            if let Some(value) = int::to_bigint(object) {
                return int::from_bigint(value);
            }
            if let Some(value) = bool_::to_bool(object) {
                return int::from_i64(if value { 1 } else { 0 });
            }
        }
        raise_type_error("object cannot be interpreted as an integer")
    })
}

/// Implements `abs()` for built-in numeric objects.
#[must_use]
pub fn abs_object(object: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        unsafe {
            install_runtime_int_slots(object);
        }
        match unsafe { numeric_value(object) } {
            Some(NumericValue::Int(value)) => int::from_bigint(value.abs()),
            Some(NumericValue::Float(value)) => float::from_f64(value.abs()),
            Some(NumericValue::Complex(real, imag)) => float::from_f64(real.hypot(imag)),
            None => raise_type_error("bad operand type for abs()"),
        }
    })
}

/// Implements `divmod()` for built-in int and float operands.
#[must_use]
pub fn divmod_objects(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        unsafe {
            install_runtime_int_slots(left);
            install_runtime_int_slots(right);
        }
        match unsafe { (numeric_value(left), numeric_value(right)) } {
            (Some(NumericValue::Int(left)), Some(NumericValue::Int(right))) => divmod_ints(&left, &right),
            (Some(left), Some(right)) => divmod_reals(left, right),
            _ => raise_type_error("unsupported operands for divmod()"),
        }
    })
}

fn divmod_ints(left: &BigInt, right: &BigInt) -> *mut PyObject {
    if right.is_zero() {
        return raise_zero_division_error("division by zero");
    }
    let (quotient, remainder) = left.div_mod_floor(right);
    let mut values = [int::from_bigint(quotient), int::from_bigint(remainder)];
    unsafe { crate::abi::seq::pon_build_tuple(values.as_mut_ptr(), values.len()) }
}

fn divmod_reals(left: NumericValue, right: NumericValue) -> *mut PyObject {
    let Some(left) = real_as_f64(left) else {
        return raise_type_error("int too large to convert to float");
    };
    let Some(right) = real_as_f64(right) else {
        return raise_type_error("int too large to convert to float");
    };
    if right == 0.0 {
        return raise_zero_division_error("division by zero");
    }
    let quotient = (left / right).floor();
    let remainder = left - quotient * right;
    let mut values = [float::from_f64(quotient), float::from_f64(remainder)];
    unsafe { crate::abi::seq::pon_build_tuple(values.as_mut_ptr(), values.len()) }
}

/// Converts a Python numeric object to `f64`.
///
/// On error this follows the scalar-sentinel ABI shape: it sets the current
/// exception and returns `NaN`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_number_as_f64(object: *mut PyObject) -> f64 {
    crate::untag_prelude!(err = f64::NAN; object);
    match super::catch_object_helper(|| {
        unsafe {
            install_runtime_int_slots(object);
        }
        match unsafe { numeric_value(object) } {
            Some(NumericValue::Int(value)) => match value.to_f64() {
                Some(value) => float::from_f64(value),
                None => raise_type_error("int too large to convert to float"),
            },
            Some(NumericValue::Float(value)) => float::from_f64(value),
            Some(NumericValue::Complex(_, _)) => raise_type_error("can't convert complex to float"),
            None => raise_type_error("a real number is required"),
        }
    }) {
        value if value.is_null() => f64::NAN,
        value => unsafe { float::to_f64(value).unwrap_or(f64::NAN) },
    }
}

unsafe fn install_runtime_int_slots(object: *mut PyObject) {
    unsafe { int::install_slots_for_object(object) };
}

unsafe fn numeric_value(object: *mut PyObject) -> Option<NumericValue> {
    if let Some(value) = unsafe { bool_::to_bool(object) } {
        return Some(NumericValue::Int(BigInt::from(if value { 1 } else { 0 })));
    }
    if let Some(value) = unsafe { int::to_bigint(object) } {
        return Some(NumericValue::Int(value));
    }
    if let Some(value) = unsafe { float::to_f64(object) } {
        return Some(NumericValue::Float(value));
    }
    unsafe { complex_::to_f64s(object).map(|(real, imag)| NumericValue::Complex(real, imag)) }
}

fn binary_numeric(op: NumberOp, left: NumericValue, right: NumericValue) -> *mut PyObject {
    match (&left, &right) {
        (NumericValue::Complex(_, _), _) | (_, NumericValue::Complex(_, _)) => binary_complex(op, left, right),
        (NumericValue::Float(_), _) | (_, NumericValue::Float(_)) => binary_float(op, left, right),
        (NumericValue::Int(left), NumericValue::Int(right)) => binary_int(op, left, right),
    }
}

fn binary_int(op: NumberOp, left: &BigInt, right: &BigInt) -> *mut PyObject {
    match op {
        BINARY_ADD => int::from_bigint(left + right),
        BINARY_SUB => int::from_bigint(left - right),
        BINARY_MUL => int::from_bigint(left * right),
        BINARY_FLOORDIV => {
            if right.is_zero() {
                return raise_zero_division_error("division by zero");
            }
            int::from_bigint(left.div_floor(right))
        }
        BINARY_MOD => {
            if right.is_zero() {
                return raise_zero_division_error("division by zero");
            }
            int::from_bigint(left.mod_floor(right))
        }
        BINARY_POW => pow_int(left, right),
        BINARY_LSHIFT => shift_int(left, right, true),
        BINARY_RSHIFT => shift_int(left, right, false),
        BINARY_AND => int::from_bigint(left & right),
        BINARY_OR => int::from_bigint(left | right),
        BINARY_XOR => int::from_bigint(left ^ right),
        BINARY_DIV => match (left.to_f64(), right.to_f64()) {
            (_, Some(0.0)) => raise_zero_division_error("division by zero"),
            (Some(left), Some(right)) => float::from_f64(left / right),
            _ => raise_type_error("int too large to convert to float"),
        },
        // Numeric domain rejection (`1 @ 2`): NotImplemented lets the caller
        // run the full dispatch and raise the operand-typed TypeError.
        _ => unsafe { super::pon_not_implemented() },
    }
}

fn pow_int(left: &BigInt, right: &BigInt) -> *mut PyObject {
    if right.is_negative() {
        return match (left.to_f64(), right.to_f64()) {
            (Some(0.0), _) => raise_zero_division_error("zero to a negative power"),
            (Some(left), Some(right)) => float::from_f64(left.powf(right)),
            _ => raise_type_error("int too large to convert to float"),
        };
    }
    let Some(exponent) = right.to_u32() else {
        return raise_value_error("exponent too large");
    };
    int::from_bigint(left.pow(exponent))
}

fn shift_int(left: &BigInt, right: &BigInt, left_shift: bool) -> *mut PyObject {
    if right.is_negative() {
        return raise_value_error("negative shift count");
    }
    let Some(shift) = right.to_usize() else {
        return raise_value_error("shift count too large");
    };
    if left_shift {
        int::from_bigint(left << shift)
    } else {
        int::from_bigint(left >> shift)
    }
}

fn binary_float(op: NumberOp, left: NumericValue, right: NumericValue) -> *mut PyObject {
    let Some(left) = real_as_f64(left) else {
        return raise_type_error("int too large to convert to float");
    };
    let Some(right) = real_as_f64(right) else {
        return raise_type_error("int too large to convert to float");
    };

    match op {
        BINARY_ADD => float::from_f64(left + right),
        BINARY_SUB => float::from_f64(left - right),
        BINARY_MUL => float::from_f64(left * right),
        BINARY_DIV => {
            if right == 0.0 {
                raise_zero_division_error("division by zero")
            } else {
                float::from_f64(left / right)
            }
        }
        BINARY_FLOORDIV => {
            if right == 0.0 {
                raise_zero_division_error("division by zero")
            } else {
                float::from_f64((left / right).floor())
            }
        }
        BINARY_MOD => {
            if right == 0.0 {
                raise_zero_division_error("division by zero")
            } else {
                float::from_f64(left - (left / right).floor() * right)
            }
        }
        BINARY_POW => {
            // `0.0 ** w` raises for finite negative `w`; infinite or NaN
            // exponents keep the IEEE special-case results (float_pow).
            if left == 0.0 && right < 0.0 && right.is_finite() {
                raise_zero_division_error("zero to a negative power")
            } else {
                float::from_f64(left.powf(right))
            }
        }
        _ => unsafe { super::pon_not_implemented() },
    }
}

fn binary_complex(op: NumberOp, left: NumericValue, right: NumericValue) -> *mut PyObject {
    let Some((left_real, left_imag)) = as_complex(left) else {
        return raise_type_error("int too large to convert to complex");
    };
    let Some((right_real, right_imag)) = as_complex(right) else {
        return raise_type_error("int too large to convert to complex");
    };

    match op {
        BINARY_ADD => complex_::from_f64s(left_real + right_real, left_imag + right_imag),
        BINARY_SUB => complex_::from_f64s(left_real - right_real, left_imag - right_imag),
        BINARY_MUL => complex_::from_f64s(
            left_real * right_real - left_imag * right_imag,
            left_real * right_imag + left_imag * right_real,
        ),
        BINARY_DIV => {
            let denominator = right_real.mul_add(right_real, right_imag * right_imag);
            if denominator == 0.0 {
                raise_zero_division_error("division by zero")
            } else {
                complex_::from_f64s(
                    (left_real * right_real + left_imag * right_imag) / denominator,
                    (left_imag * right_real - left_real * right_imag) / denominator,
                )
            }
        }
        BINARY_POW => pow_complex(left_real, left_imag, right_real, right_imag),
        _ => unsafe { super::pon_not_implemented() },
    }
}

fn pow_complex(left_real: f64, left_imag: f64, right_real: f64, right_imag: f64) -> *mut PyObject {
    if left_real == 0.0 && left_imag == 0.0 {
        if right_real == 0.0 && right_imag == 0.0 {
            return complex_::from_f64s(1.0, 0.0);
        }
        if right_real < 0.0 || right_imag != 0.0 {
            return raise_zero_division_error("zero to a negative or complex power");
        }
        return complex_::from_f64s(0.0, 0.0);
    }

    let radius = left_real.hypot(left_imag);
    let theta = left_imag.atan2(left_real);
    let log_real = radius.ln();
    let exp_real = right_real * log_real - right_imag * theta;
    let exp_imag = right_real * theta + right_imag * log_real;
    let scale = exp_real.exp();
    complex_::from_f64s(scale * exp_imag.cos(), scale * exp_imag.sin())
}

fn unary_numeric(op: NumberOp, value: NumericValue) -> *mut PyObject {
    match value {
        NumericValue::Int(value) => match op {
            UNARY_NEG => int::from_bigint(-value),
            UNARY_POS => int::from_bigint(value),
            UNARY_INVERT => int::from_bigint(!value),
            _ => raise_type_error(unary_unsupported_message(op)),
        },
        NumericValue::Float(value) => match op {
            UNARY_NEG => float::from_f64(-value),
            UNARY_POS => float::from_f64(value),
            UNARY_INVERT => raise_type_error(unary_unsupported_message(op)),
            _ => raise_type_error(unary_unsupported_message(op)),
        },
        NumericValue::Complex(real, imag) => match op {
            UNARY_NEG => complex_::from_f64s(-real, -imag),
            UNARY_POS => complex_::from_f64s(real, imag),
            UNARY_INVERT => raise_type_error(unary_unsupported_message(op)),
            _ => raise_type_error(unary_unsupported_message(op)),
        },
    }
}

fn real_as_f64(value: NumericValue) -> Option<f64> {
    match value {
        NumericValue::Int(value) => value.to_f64(),
        NumericValue::Float(value) => Some(value),
        NumericValue::Complex(_, _) => None,
    }
}

fn as_complex(value: NumericValue) -> Option<(f64, f64)> {
    match value {
        NumericValue::Int(value) => value.to_f64().map(|value| (value, 0.0)),
        NumericValue::Float(value) => Some((value, 0.0)),
        NumericValue::Complex(real, imag) => Some((real, imag)),
    }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn raise_value_error(message: &str) -> *mut PyObject {
    unsafe { crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) }
}

fn raise_zero_division_error(message: &str) -> *mut PyObject {
    unsafe { crate::abi::exc::pon_raise_zero_division_error(message.as_ptr(), message.len()) }
}

fn unary_unsupported_message(op: NumberOp) -> &'static str {
    match op {
        UNARY_NEG => "bad operand type for unary -",
        UNARY_POS => "bad operand type for unary +",
        UNARY_INVERT => "bad operand type for unary ~",
        _ => "unknown unary numeric operation",
    }
}

#[cfg(test)]
mod tests {
    use core::ptr;
    use std::sync::MutexGuard;

    use num_bigint::BigInt;
    use num_traits::One;

    use super::*;
    use crate::object::PyObject;
    use crate::thread_state::{pon_err_clear, test_state_lock};

    fn init() -> MutexGuard<'static, ()> {
        let guard = test_state_lock();
        unsafe {
            assert_eq!(crate::abi::pon_runtime_init(), 0);
        }
        pon_err_clear();
        guard
    }

    fn str_object(text: &str) -> *mut PyObject {
        let object = unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) };
        assert!(!object.is_null(), "failed to allocate test str {text:?}");
        object
    }

    fn parse_bigint(text: &str) -> BigInt {
        BigInt::parse_bytes(text.as_bytes(), 10).expect("valid decimal BigInt literal")
    }

    #[track_caller]
    fn assert_bigint_object(object: *mut PyObject, expected: BigInt) {
        assert!(!object.is_null());
        assert_eq!(unsafe { int::to_bigint(object) }, Some(expected));
    }

    #[track_caller]
    fn assert_binary_bigint(op: NumberOp, left: &BigInt, right: &BigInt, expected: BigInt) {
        let result = unsafe {
            pon_binary_op(
                op,
                int::from_bigint(left.clone()),
                int::from_bigint(right.clone()),
                ptr::null_mut(),
            )
        };
        assert_bigint_object(result, expected);
    }

    #[track_caller]
    fn assert_unary_bigint(op: NumberOp, value: &BigInt, expected: BigInt) {
        let result = unsafe { pon_unary_op(op, int::from_bigint(value.clone()), ptr::null_mut()) };
        assert_bigint_object(result, expected);
    }

    #[test]
    fn k3_numeric_bigint_spill_binary_and_unary_ops_keep_arbitrary_precision() {
        let _guard = init();
        let left = (BigInt::one() << 80_usize) + BigInt::from(123);
        let right = (BigInt::one() << 75_usize) - BigInt::from(77);
        assert_binary_bigint(BINARY_ADD, &left, &right, left.clone() + right.clone());
        assert_binary_bigint(BINARY_MUL, &left, &right, left.clone() * right.clone());
        assert_binary_bigint(BINARY_LSHIFT, &left, &BigInt::from(17), left.clone() << 17_usize);
        assert_binary_bigint(BINARY_RSHIFT, &left, &BigInt::from(9), left.clone() >> 9_usize);

        let negative = -((BigInt::one() << 84_usize) + BigInt::from(0x5a5a_u32));
        let mask = (BigInt::one() << 82_usize) - BigInt::from(0x1234_u32);
        assert_binary_bigint(BINARY_AND, &negative, &mask, negative.clone() & mask.clone());
        assert_binary_bigint(BINARY_OR, &negative, &mask, negative.clone() | mask.clone());
        assert_binary_bigint(BINARY_XOR, &negative, &mask, negative.clone() ^ mask.clone());
        assert_unary_bigint(UNARY_NEG, &negative, -negative.clone());
        assert_unary_bigint(UNARY_INVERT, &negative, !negative.clone());
    }

    #[test]
    fn k3_numeric_int_hash_matches_cpython_width_boundary_canaries() {
        let cases = [
            ((BigInt::one() << 61_usize) - BigInt::from(1), 0_isize),
            (BigInt::one() << 61_usize, 1_isize),
            (BigInt::from(-1), -2_isize),
            (BigInt::one() << 64_usize, 8_isize),
        ];

        for (value, expected) in cases {
            assert_eq!(int::hash_bigint(&value), expected, "hash({value})");
        }
    }

    #[test]
    fn k3_numeric_float_repr_matches_shortest_roundtrip_canaries() {
        let cases = [
            (0.1, "0.1"),
            (1.0, "1.0"),
            (-0.0, "-0.0"),
            (1e16, "1e+16"),
            (1e-5, "1e-05"),
        ];

        for (value, expected) in cases {
            assert_eq!(float::repr_f64(value), expected, "repr({value:?})");
        }
        let rounded = "9007199254740993.0".parse::<f64>().expect("valid f64 literal");
        assert_eq!(float::repr_f64(rounded), "9007199254740992.0");
    }

    #[test]
    fn k3_numeric_int_construct_base_zero_accepts_prefix_underscores_and_rejects_legacy_octal() {
        let _guard = init();
        let cases = [
            ("  +0b_1010", BigInt::from(10)),
            ("0o7_5", BigInt::from(61)),
            ("0x_Ff", BigInt::from(255)),
            ("1_234_567", BigInt::from(1_234_567)),
        ];

        for (text, expected) in cases {
            let args = [str_object(text), unsafe { crate::abi::pon_const_int(0) }];
            let result = int::construct_from_args(&args);
            assert_bigint_object(result, expected);
        }

        let invalid_args = [str_object("010"), unsafe { crate::abi::pon_const_int(0) }];
        assert!(int::construct_from_args(&invalid_args).is_null());
        pon_err_clear();
    }

    #[test]
    fn k3_numeric_int_construct_from_float_truncates_toward_zero_without_i64_limit() {
        let _guard = init();
        let cases = [
            (3.9, BigInt::from(3)),
            (-3.9, BigInt::from(-3)),
            (2.0_f64.powi(63), BigInt::one() << 63_usize),
        ];

        for (value, expected) in cases {
            let args = [float::from_f64(value)];
            let result = int::construct_from_args(&args);
            assert_bigint_object(result, expected);
        }
    }

    #[test]
    fn k3_numeric_complex_constructor_and_repr_preserve_nan_inf_and_negative_zero_canaries() {
        let _guard = init();

        let parsed = complex_::construct_from_args(&[str_object("nan+infj")]);
        assert!(!parsed.is_null());
        let (real, imag) = unsafe { complex_::to_f64s(parsed) }.expect("complex object");
        assert!(real.is_nan());
        assert_eq!(imag, f64::INFINITY);
        assert_eq!(complex_::repr_complex(real, imag), "(nan+infj)");

        let negative_zero = complex_::construct_from_args(&[str_object("-0-0j")]);
        let (real, imag) = unsafe { complex_::to_f64s(negative_zero) }.expect("complex object");
        assert!(real.is_sign_negative());
        assert!(imag.is_sign_negative());
        assert_eq!(complex_::repr_complex(real, imag), "(-0-0j)");

        assert_eq!(complex_::repr_complex(-0.0, 0.0), "(-0+0j)");
        assert_eq!(complex_::repr_complex(0.0, -0.0), "-0j");
    }

    #[test]
    fn k3_numeric_divmod_objects_returns_exact_quotient_and_remainder_for_huge_ints() {
        let _guard = init();
        let left = (BigInt::one() << 130_usize) + BigInt::from(123);
        let right = (BigInt::one() << 65_usize) - BigInt::from(1);
        let result = divmod_objects(int::from_bigint(left), int::from_bigint(right));

        assert!(!result.is_null());
        assert!(unsafe { int::type_name_is(result, "tuple") });
        let tuple = unsafe { &*result.cast::<crate::types::tuple::PyTuple>() };
        let items = unsafe { tuple.as_slice() };
        assert_eq!(items.len(), 2);
        assert_eq!(unsafe { int::to_bigint(items[0]) }, Some(parse_bigint("36893488147419103233")));
        assert_eq!(unsafe { int::to_bigint(items[1]) }, Some(BigInt::from(124)));
    }

    #[test]
    fn pow_keeps_large_ints_exact() {
        unsafe {
            let _guard = init();
            let result = pon_binary_op(
                BINARY_POW,
                crate::abi::pon_const_int(2),
                crate::abi::pon_const_int(1000),
                ptr::null_mut(),
            );

            assert!(!result.is_null());
            assert_eq!(int::to_bigint(result), Some(BigInt::one() << 1000_usize));
        }
    }

    #[test]
    fn floor_division_and_modulo_follow_python_sign_rules() {
        unsafe {
            let _guard = init();
            let left = crate::abi::pon_const_int(-7);
            let right = crate::abi::pon_const_int(2);

            let div = pon_binary_op(BINARY_FLOORDIV, left, right, ptr::null_mut());
            let modulo = pon_binary_op(BINARY_MOD, left, right, ptr::null_mut());

            assert_eq!(int::to_bigint(div), Some(BigInt::from(-4)));
            assert_eq!(int::to_bigint(modulo), Some(BigInt::from(1)));
        }
    }

    #[test]
    fn bool_is_distinct_but_numeric() {
        unsafe {
            let _guard = init();
            let truth = pon_const_bool(1);
            assert!(bool_::is_exact_bool(truth));
            assert!(!int::is_exact_int(truth));

            let sum = pon_binary_op(BINARY_ADD, truth, crate::abi::pon_const_int(1), ptr::null_mut());
            assert_eq!(int::to_bigint(sum), Some(BigInt::from(2)));
        }
    }

    #[test]
    fn unsupported_numeric_operation_raises_type_error() {
        unsafe {
            let _guard = init();
            let result = pon_binary_op(
                BINARY_MATMUL,
                crate::abi::pon_const_int(1),
                crate::abi::pon_const_int(2),
                ptr::null_mut(),
            );

            assert!(result.is_null());
        }
    }
}
