//! Phase-B abstract protocol dispatch.
//!
//! This module is the runtime's tier-0 equivalent of CPython's abstract-object
//! helpers: exported ABI thunks pass through here, the dispatcher reads the
//! `PyType` slot fields and protocol tables installed by B0-SLOT, and failures
//! use the B05 exception core instead of panicking or fabricating placeholder
//! objects.
//!
//! Feedback cells are deliberately ignored at this layer.  Their pointer is part
//! of the ABI now so tier-1 can attach speculation later without changing helper
//! signatures.

use core::ffi::c_int;
use core::{mem, ptr};

use crate::abi;
use crate::object::{BinaryFunc, PyLong, PyObject, PyType, PyUnicode, RichCmpFunc, UnaryFunc};
use crate::thread_state::pon_err_occurred;

/// Binary operation selectors shared with the Phase-B IR lowering contract.
pub const BINARY_ADD: u8 = 0;
pub const BINARY_SUB: u8 = 1;
pub const BINARY_MUL: u8 = 2;
pub const BINARY_MATMUL: u8 = 3;
pub const BINARY_DIV: u8 = 4;
pub const BINARY_FLOORDIV: u8 = 5;
pub const BINARY_MOD: u8 = 6;
pub const BINARY_POW: u8 = 7;
pub const BINARY_LSHIFT: u8 = 8;
pub const BINARY_RSHIFT: u8 = 9;
pub const BINARY_AND: u8 = 10;
pub const BINARY_OR: u8 = 11;
pub const BINARY_XOR: u8 = 12;

/// Unary operation selectors shared with the Phase-B IR lowering contract.
pub const UNARY_NEG: u8 = 0;
pub const UNARY_POS: u8 = 1;
pub const UNARY_INVERT: u8 = 2;

/// Rich-comparison selectors passed to `tp_richcmp` slots.
///
/// These values intentionally match CPython's `Py_LT`, `Py_LE`, `Py_EQ`,
/// `Py_NE`, `Py_GT`, and `Py_GE` ordering so native slot implementations can
/// consume the ABI byte directly.
pub const RICH_LT: u8 = 0;
pub const RICH_LE: u8 = 1;
pub const RICH_EQ: u8 = 2;
pub const RICH_NE: u8 = 3;
pub const RICH_GT: u8 = 4;
pub const RICH_GE: u8 = 5;

enum SlotOutcome {
    Value(*mut PyObject),
    NotImplemented,
    Error,
    Missing,
}

/// Dispatches a Python binary operation through exact PyLong fast paths,
/// numeric protocol slots, reflected slots, and finally the sequence concat
/// seam for `+`.
///
/// Reflected ordering follows CPython's shape: when the right operand's type is
/// a strict subtype of the left operand's type, its reflected slot gets the
/// first chance; otherwise the left forward slot runs before the right reflected
/// slot.  A slot result that is `NotImplemented` falls through to the next
/// candidate.  A NULL slot result is treated as an error sentinel and propagated.
pub unsafe fn binary_op(op: u8, a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { binary_op_flavored(op, a, b, false) }
}

/// [`binary_op`] with the terminal-TypeError flavor threaded through:
/// `inplace` selects the `=`-suffixed operator spelling (`|=` vs `|`),
/// matching CPython's `binary_iop` wording after the in-place slot has
/// already been tried and declined.
pub(crate) unsafe fn binary_op_flavored(op: u8, a: *mut PyObject, b: *mut PyObject, inplace: bool) -> *mut PyObject {
    if op == BINARY_ADD && unsafe { is_exact_pylong(a) && is_exact_pylong(b) } {
        // SAFETY: The exact-type checks above prove both operands use PyLong's layout.
        let left = unsafe { (*a.cast::<PyLong>()).value };
        let right = unsafe { (*b.cast::<PyLong>()).value };
        // Wide ints are PyLong shells whose inline payload is 0 with the real
        // value out of line (`types::int::from_bigint`), so the allocation-free
        // fast path is only sound when both inline payloads are nonzero.
        // Zeros and shells take the exact BigInt route below; i64 overflow
        // promotes instead of raising (CPython ints never overflow).
        if left != 0 && right != 0 {
            if let Some(sum) = left.checked_add(right) {
                return unsafe { abi::pon_const_int(sum) };
            }
        }
        // SAFETY: Exact ints always carry an extractable payload.
        if let (Some(left), Some(right)) =
            unsafe { (crate::types::int::to_bigint(a), crate::types::int::to_bigint(b)) }
        {
            return crate::types::int::from_bigint(left + right);
        }
    }

    let Some(left_type) = (unsafe { object_type(a) }) else {
        return raise_type_error("left operand is NULL or has no type");
    };
    let Some(right_type) = (unsafe { object_type(b) }) else {
        return raise_type_error("right operand is NULL or has no type");
    };

    // `str % args` — CPython unicode `nb_remainder` (%-formatting).  Keyed by
    // the exact type name, matching the str rich-compare fast path below.
    if op == BINARY_MOD && unsafe { (*left_type).name() == "str" } {
        return unsafe { abi::format::percent_format(a, b) };
    }

    if op == BINARY_MUL {
        match unsafe { try_sequence_repeat(left_type, a, b) } {
            SlotOutcome::Value(value) => return value,
            SlotOutcome::Error => return ptr::null_mut(),
            SlotOutcome::Missing | SlotOutcome::NotImplemented => {}
        }
        match unsafe { try_sequence_repeat(right_type, b, a) } {
            SlotOutcome::Value(value) => return value,
            SlotOutcome::Error => return ptr::null_mut(),
            SlotOutcome::Missing | SlotOutcome::NotImplemented => {}
        }
    }

    let same_type = left_type == right_type;
    let left_forward_slot = unsafe { forward_binary_slot(left_type, op) };
    let right_reflected_slot = if same_type {
        None
    } else {
        unsafe { reflected_binary_slot(right_type, op) }
    };
    let right_subtype = !same_type && unsafe { is_subtype(right_type, left_type) };
    let distinct_right_reflected =
        right_reflected_slot.is_some() && !same_binary_slot(left_forward_slot, right_reflected_slot);

    if right_subtype && distinct_right_reflected {
        match unsafe { call_binary_slot(right_reflected_slot, b, a) } {
            SlotOutcome::Value(value) => return value,
            SlotOutcome::Error => return ptr::null_mut(),
            SlotOutcome::Missing | SlotOutcome::NotImplemented => {}
        }
    }

    match unsafe { try_forward_binary(left_type, op, a, b) } {
        SlotOutcome::Value(value) => return value,
        SlotOutcome::Error => return ptr::null_mut(),
        SlotOutcome::Missing | SlotOutcome::NotImplemented => {}
    }

    if !right_subtype && distinct_right_reflected {
        match unsafe { call_binary_slot(right_reflected_slot, b, a) } {
            SlotOutcome::Value(value) => return value,
            SlotOutcome::Error => return ptr::null_mut(),
            SlotOutcome::Missing | SlotOutcome::NotImplemented => {}
        }
    }

    // Python-level binary dunders (heap classes: `IntFlag.__or__`, user
    // `__add__`, ...) — native types express theirs through the slots above.
    if let Some((forward, reflected)) = binary_dunder_names(op) {
        if right_subtype && !same_type {
            match unsafe { call_binary_dunder(right_type, reflected, b, a) } {
                SlotOutcome::Value(value) => return value,
                SlotOutcome::Error => return ptr::null_mut(),
                SlotOutcome::Missing | SlotOutcome::NotImplemented => {}
            }
        }
        match unsafe { call_binary_dunder(left_type, forward, a, b) } {
            SlotOutcome::Value(value) => return value,
            SlotOutcome::Error => return ptr::null_mut(),
            SlotOutcome::Missing | SlotOutcome::NotImplemented => {}
        }
        if !right_subtype && !same_type {
            match unsafe { call_binary_dunder(right_type, reflected, b, a) } {
                SlotOutcome::Value(value) => return value,
                SlotOutcome::Error => return ptr::null_mut(),
                SlotOutcome::Missing | SlotOutcome::NotImplemented => {}
            }
        }
    }

    // Payload-subclass terminus: `int`/`str`-subclass instances without a
    // Python-level override compute through their canonical payload (CPython
    // inherits the base's number/sequence slots; results are the plain base
    // type, `int`-subclass `+` returning exact `int`).
    let left_payload = unsafe { crate::types::type_::payload_subclass_value(a) };
    let right_payload = unsafe { crate::types::type_::payload_subclass_value(b) };
    if left_payload.is_some() || right_payload.is_some() {
        // Full numeric entry, not a bare `binary_op` recursion: the slot
        // tables have no forward `**` wiring (`nb_power` is ternary), while
        // `pon_binary_op` computes every numeric op directly and falls back
        // here for non-numeric payloads (str concat, `%`-format).
        return unsafe { abi::number::binary_op_with_flavor(op, left_payload.unwrap_or(a), right_payload.unwrap_or(b), inplace) };
    }

    unsafe { raise_binary_unsupported(op, a, b, inplace) }
}

/// Dispatches a Python unary operation through the numeric protocol table.
pub unsafe fn unary_op(op: u8, operand: *mut PyObject) -> *mut PyObject {
    let Some(ty) = (unsafe { object_type(operand) }) else {
        return raise_type_error("operand is NULL or has no type");
    };

    let slot = unsafe {
        (*ty).tp_as_number.as_ref().and_then(|methods| match op {
            UNARY_NEG => methods.nb_negative,
            UNARY_POS => methods.nb_positive,
            UNARY_INVERT => methods.nb_invert,
            _ => None,
        })
    };

    match unsafe { call_unary_slot(slot, operand) } {
        SlotOutcome::Value(value) => return value,
        SlotOutcome::Error => return ptr::null_mut(),
        SlotOutcome::Missing | SlotOutcome::NotImplemented => {}
    }

    // Python-level unary dunders (heap classes: `Flag.__invert__`, ...).
    let name = match op {
        UNARY_NEG => "__neg__",
        UNARY_POS => "__pos__",
        UNARY_INVERT => "__invert__",
        _ => return raise_type_error(unary_unsupported_message(op)),
    };
    match unsafe { call_unary_dunder(ty, name, operand) } {
        SlotOutcome::Value(value) => return value,
        SlotOutcome::Error => return ptr::null_mut(),
        SlotOutcome::Missing | SlotOutcome::NotImplemented => {}
    }

    // Payload-subclass terminus (see `binary_op`).
    if let Some(payload) = unsafe { crate::types::type_::payload_subclass_value(operand) } {
        return unsafe { unary_op(op, payload) };
    }

    raise_type_error(unary_unsupported_message(op))
}

/// Dispatches a rich comparison.  Exact PyLong comparisons are handled directly;
/// other objects use `tp_richcmp` with the same reflected-subtype ordering shape
/// as binary operations, swapping the comparison op for the right-hand call.
pub unsafe fn rich_compare(op: u8, a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    if let Some(result) = unsafe { rich_compare_numeric(op, a, b) } {
        return result;
    }

    let Some(left_type) = (unsafe { object_type(a) }) else {
        return raise_type_error("left operand is NULL or has no type");
    };
    let Some(right_type) = (unsafe { object_type(b) }) else {
        return raise_type_error("right operand is NULL or has no type");
    };

    if unsafe { (*left_type).name() == "str" && (*right_type).name() == "str" } {
        let left = unsafe { unicode_bytes(&*a.cast::<PyUnicode>()) };
        let right = unsafe { unicode_bytes(&*b.cast::<PyUnicode>()) };
        let result = match op {
            RICH_EQ => left == right,
            RICH_NE => left != right,
            RICH_LT => left < right,
            RICH_LE => left <= right,
            RICH_GT => left > right,
            RICH_GE => left >= right,
            _ => return raise_type_error("unknown rich comparison operation"),
        };
        return const_bool_object(result);
    }

    let same_type = left_type == right_type;
    let left_richcmp_slot = unsafe { (*left_type).tp_richcmp };
    let right_richcmp_slot = if same_type {
        None
    } else {
        unsafe { (*right_type).tp_richcmp }
    };
    let right_subtype = !same_type && unsafe { is_subtype(right_type, left_type) };
    let distinct_right_richcmp =
        right_richcmp_slot.is_some() && !same_richcmp_slot(left_richcmp_slot, right_richcmp_slot);

    if right_subtype && distinct_right_richcmp {
        match unsafe { call_richcmp_slot(right_richcmp_slot, swapped_rich_op(op), b, a) } {
            SlotOutcome::Value(value) => return value,
            SlotOutcome::Error => return ptr::null_mut(),
            SlotOutcome::Missing | SlotOutcome::NotImplemented => {}
        }
    }

    match unsafe { call_richcmp_slot(left_richcmp_slot, op, a, b) } {
        SlotOutcome::Value(value) => return value,
        SlotOutcome::Error => return ptr::null_mut(),
        SlotOutcome::Missing | SlotOutcome::NotImplemented => {}
    }

    if !right_subtype && distinct_right_richcmp {
        match unsafe { call_richcmp_slot(right_richcmp_slot, swapped_rich_op(op), b, a) } {
            SlotOutcome::Value(value) => return value,
            SlotOutcome::Error => return ptr::null_mut(),
            SlotOutcome::Missing | SlotOutcome::NotImplemented => {}
        }
    }

    let left_dunder = unsafe { rich_dunder(left_type, op) };
    let right_dunder = if same_type {
        ptr::null_mut()
    } else {
        unsafe { rich_dunder(right_type, swapped_rich_op(op)) }
    };
    let distinct_right_dunder = !right_dunder.is_null() && left_dunder != right_dunder;

    if right_subtype && distinct_right_dunder {
        match unsafe { call_rich_dunder(right_dunder, b, a, right_type) } {
            SlotOutcome::Value(value) => return value,
            SlotOutcome::Error => return ptr::null_mut(),
            SlotOutcome::Missing | SlotOutcome::NotImplemented => {}
        }
    }

    match unsafe { call_rich_dunder(left_dunder, a, b, left_type) } {
        SlotOutcome::Value(value) => return value,
        SlotOutcome::Error => return ptr::null_mut(),
        SlotOutcome::Missing | SlotOutcome::NotImplemented => {}
    }

    if !right_subtype && distinct_right_dunder {
        match unsafe { call_rich_dunder(right_dunder, b, a, right_type) } {
            SlotOutcome::Value(value) => return value,
            SlotOutcome::Error => return ptr::null_mut(),
            SlotOutcome::Missing | SlotOutcome::NotImplemented => {}
        }
    }

    // Payload-subclass terminus: an `int`/`str`-subclass operand without a
    // Python-level override compares through its canonical payload (CPython
    // inherits the base's `tp_richcompare`: `IntEnum.B == 2`).
    let left_payload = unsafe { crate::types::type_::payload_subclass_value(a) };
    let right_payload = unsafe { crate::types::type_::payload_subclass_value(b) };
    if left_payload.is_some() || right_payload.is_some() {
        return unsafe { rich_compare(op, left_payload.unwrap_or(a), right_payload.unwrap_or(b)) };
    }

    match op {
        RICH_EQ => const_bool_object(a == b),
        RICH_NE => {
            // `object.__ne__` default: delegate to the operand's own
            // `__eq__` and invert, NotImplemented passing through (CPython
            // `object_richcompare`).  Reached only after every `__ne__`
            // slot/dunder attempt above fell through; subtype priority
            // mirrors those attempts.  Identity stays the final default.
            let ordered = if right_subtype { [(right_type, b, a), (left_type, a, b)] } else { [(left_type, a, b), (right_type, b, a)] };
            for (ty, obj, other) in ordered {
                if same_type && !core::ptr::eq(obj, a) {
                    continue;
                }
                if let Some(result) = unsafe { ne_delegates_to_eq(ty, obj, other) } {
                    return result;
                }
            }
            const_bool_object(a != b)
        }
        _ => {
            let message = unsafe { rich_unsupported_message(op, a, b) };
            raise_type_error(&message)
        }
    }
}

/// Canonical `bool` result for a synthesized (non-dunder) rich comparison.
///
/// Builtin comparisons yield real `True`/`False` objects in CPython. Codegen
/// no longer coerces comparison results, so every path that fabricates a
/// truth value here must hand back the bool singletons; only user dunders may
/// inject other result types, and those pass through the dispatcher raw.
fn const_bool_object(value: bool) -> *mut PyObject {
    unsafe { abi::number::pon_const_bool(i32::from(value)) }
}

enum RichNumericValue {
    Int(num_bigint::BigInt),
    Float(f64),
    Complex(f64, f64),
}

unsafe fn rich_compare_numeric(op: u8, a: *mut PyObject, b: *mut PyObject) -> Option<*mut PyObject> {
    let left = unsafe { rich_numeric_value(a)? };
    let right = unsafe { rich_numeric_value(b)? };
    Some(match (left, right) {
        (RichNumericValue::Complex(left_real, left_imag), RichNumericValue::Complex(right_real, right_imag)) => {
            rich_compare_complex(op, left_real, left_imag, right_real, right_imag)
        }
        (RichNumericValue::Complex(real, imag), other) => rich_compare_complex_to_real(op, real, imag, other),
        (other, RichNumericValue::Complex(real, imag)) => {
            rich_compare_complex_to_real(swapped_rich_op(op), real, imag, other)
        }
        (RichNumericValue::Int(left), RichNumericValue::Int(right)) => rich_compare_ordering(op, left.cmp(&right)),
        (RichNumericValue::Int(left), RichNumericValue::Float(right)) => rich_compare_int_float(op, &left, right),
        (RichNumericValue::Float(left), RichNumericValue::Int(right)) => {
            rich_compare_int_float(swapped_rich_op(op), &right, left)
        }
        (RichNumericValue::Float(left), RichNumericValue::Float(right)) => rich_compare_floats(op, left, right),
    })
}

unsafe fn rich_numeric_value(object: *mut PyObject) -> Option<RichNumericValue> {
    if let Some(value) = unsafe { crate::types::int::to_bigint_including_bool(object) } {
        return Some(RichNumericValue::Int(value));
    }
    if let Some(value) = unsafe { crate::types::float::to_f64(object) } {
        return Some(RichNumericValue::Float(value));
    }
    unsafe { crate::types::complex_::to_f64s(object).map(|(real, imag)| RichNumericValue::Complex(real, imag)) }
}

fn rich_compare_complex_to_real(op: u8, real: f64, imag: f64, other: RichNumericValue) -> *mut PyObject {
    match op {
        RICH_EQ | RICH_NE => {
            let real_equal = match other {
                RichNumericValue::Int(value) => compare_int_float(&value, real).is_some_and(core::cmp::Ordering::is_eq),
                RichNumericValue::Float(value) => real == value,
                RichNumericValue::Complex(_, _) => false,
            };
            let equal = imag == 0.0 && real_equal;
            const_bool_object(if op == RICH_EQ { equal } else { !equal })
        }
        _ => raise_type_error("complex numbers are not orderable"),
    }
}

fn rich_compare_complex(op: u8, left_real: f64, left_imag: f64, right_real: f64, right_imag: f64) -> *mut PyObject {
    match op {
        RICH_EQ | RICH_NE => {
            let equal = left_real == right_real && left_imag == right_imag;
            const_bool_object(if op == RICH_EQ { equal } else { !equal })
        }
        _ => raise_type_error("complex numbers are not orderable"),
    }
}

fn rich_compare_floats(op: u8, left: f64, right: f64) -> *mut PyObject {
    let result = match op {
        RICH_EQ => left == right,
        RICH_NE => left != right,
        RICH_LT => left < right,
        RICH_LE => left <= right,
        RICH_GT => left > right,
        RICH_GE => left >= right,
        _ => return raise_type_error("unknown rich comparison operation"),
    };
    const_bool_object(result)
}

fn rich_compare_int_float(op: u8, integer: &num_bigint::BigInt, float: f64) -> *mut PyObject {
    if float.is_nan() {
        let result = op == RICH_NE;
        return const_bool_object(result);
    }
    let Some(ordering) = compare_int_float(integer, float) else {
        return raise_type_error("unknown rich comparison operation");
    };
    rich_compare_ordering(op, ordering)
}

fn rich_compare_ordering(op: u8, ordering: core::cmp::Ordering) -> *mut PyObject {
    let result = match op {
        RICH_EQ => ordering.is_eq(),
        RICH_NE => !ordering.is_eq(),
        RICH_LT => ordering.is_lt(),
        RICH_LE => !ordering.is_gt(),
        RICH_GT => ordering.is_gt(),
        RICH_GE => !ordering.is_lt(),
        _ => return raise_type_error("unknown rich comparison operation"),
    };
    const_bool_object(result)
}

fn compare_int_float(integer: &num_bigint::BigInt, float: f64) -> Option<core::cmp::Ordering> {
    if float.is_nan() {
        return None;
    }
    if float == f64::INFINITY {
        return Some(core::cmp::Ordering::Less);
    }
    if float == f64::NEG_INFINITY {
        return Some(core::cmp::Ordering::Greater);
    }
    let bits = float.to_bits();
    let negative = bits >> 63 != 0;
    let exp_bits = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1_u64 << 52) - 1);
    let (mantissa, exponent) = if exp_bits == 0 {
        (frac, 1 - 1023 - 52)
    } else {
        ((1_u64 << 52) | frac, exp_bits - 1023 - 52)
    };
    let mut numerator = num_bigint::BigInt::from(mantissa);
    if negative {
        numerator = -numerator;
    }
    if exponent >= 0 {
        Some(integer.cmp(&(numerator << exponent as usize)))
    } else {
        Some((integer << (-exponent) as usize).cmp(&numerator))
    }
}

/// Computes truth using `tp_bool`, number `nb_bool`, sequence/mapping length,
/// and small built-in fast paths.
///
/// Returns `1` for true, `0` for false, and `-1` with the current exception set
/// on error.
pub unsafe fn is_true(object: *mut PyObject) -> i32 {
    let Some(ty) = (unsafe { object_type(object) }) else {
        return raise_type_error_status("truth operand is NULL or has no type");
    };

    if unsafe { is_exact_pylong(object) } {
        // SAFETY: The exact-type check above proves PyLong layout.
        return i32::from(unsafe { (*object.cast::<PyLong>()).value != 0 });
    }

    if unsafe { (*ty).name() == "NoneType" } {
        return 0;
    }

    if let Some(slot) = unsafe { (*ty).tp_bool } {
        return unsafe { normalize_inquiry(slot(object), "__bool__ returned an error without setting an exception") };
    }

    if let Some(slot) = unsafe { (*ty).tp_as_number.as_ref().and_then(|methods| methods.nb_bool) } {
        return unsafe { normalize_inquiry(slot(object), "nb_bool returned an error without setting an exception") };
    }

    if let Some(slot) = unsafe { (*ty).tp_as_sequence.as_ref().and_then(|methods| methods.sq_length) } {
        return unsafe { len_to_truth(slot(object), "__len__ returned a negative value without setting an exception") };
    }

    if let Some(slot) = unsafe { (*ty).tp_as_mapping.as_ref().and_then(|methods| methods.mp_length) } {
        return unsafe { len_to_truth(slot(object), "mapping __len__ returned a negative value without setting an exception") };
    }

    // Python-level `__bool__`/`__len__` on heap instances (slotless types;
    // e.g. dict subclasses reaching dict's tp_dict `__len__` native).  Order
    // matches CPython: `__bool__` wins over `__len__`, and `__bool__` must
    // return a strict bool — anything else is a TypeError, never recursed.
    let bool_hook = unsafe { crate::descr::lookup_in_type(ty, crate::intern::intern("__bool__")) };
    if !bool_hook.is_null() {
        let result = unsafe { call_truth_hook(bool_hook, object, ty) };
        if result.is_null() {
            return -1;
        }
        let Some(value) = (unsafe { crate::types::bool_::to_bool(crate::tag::untag_arg(result)) }) else {
            let returned = unsafe { crate::types::dict::type_name(crate::tag::untag_arg(result)) }.unwrap_or("object");
            return raise_type_error_status(&format!("__bool__ should return bool, returned {returned}"));
        };
        return i32::from(value);
    }
    let len_hook = unsafe { crate::descr::lookup_in_type(ty, crate::intern::intern("__len__")) };
    if !len_hook.is_null() {
        let result = unsafe { call_truth_hook(len_hook, object, ty) };
        if result.is_null() {
            return -1;
        }
        let len = unsafe { is_true_len_value(result) };
        if len < 0 {
            return -1;
        }
        return i32::from(len > 0);
    }

    1
}

/// Binds and invokes a zero-argument truth hook (`__bool__`/`__len__`).
unsafe fn call_truth_hook(hook: *mut PyObject, object: *mut PyObject, ty: *mut PyType) -> *mut PyObject {
    let bound = unsafe { crate::descr::descriptor_get(hook, object, ty) };
    if bound.is_null() {
        return ptr::null_mut();
    }
    unsafe { abi::pon_call(bound, ptr::null_mut(), 0) }
}

/// Extracts a non-negative length from a `__len__` result for truth testing.
unsafe fn is_true_len_value(result: *mut PyObject) -> i64 {
    let Some(value) = (unsafe { crate::types::int::to_bigint_including_bool(crate::tag::untag_arg(result)) }) else {
        return raise_type_error_status("'object' object cannot be interpreted as an integer").into();
    };
    use num_traits::ToPrimitive;
    match value.to_i64() {
        Some(len) if len >= 0 => len,
        Some(_) => {
            let message = "__len__() should return >= 0";
            let _ = unsafe { abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) };
            -1
        }
        None => raise_type_error_status("cannot fit '__len__' result into an index-sized integer").into(),
    }
}

/// Dispatches attribute lookup through `tp_getattro`.
pub unsafe fn get_attr(object: *mut PyObject, name: u32) -> *mut PyObject {
    let Some(ty) = (unsafe { object_type(object) }) else {
        return raise_type_error("attribute receiver is NULL or has no type");
    };
    let Some(slot) = (unsafe { (*ty).tp_getattro }) else {
        // Slotless native receivers (e.g. int) still expose the universal
        // `__class__` (CPython: `object.__class__` getset).
        if name == crate::intern::intern("__class__") {
            // Canonicalize helper-family shadow types so `x.__class__ is
            // list` holds (the `type(x)` builtin applies the same repair).
            return unsafe { crate::types::type_::canonical_type_object(ty) }.cast::<PyObject>();
        }
        // Exact `int`/`bool` receivers: the narrow instance-method surface
        // (`bit_length`, `bit_count`, `numerator`/`denominator`/...).
        if let Some(result) = unsafe { crate::types::int::int_instance_attr(object, name) } {
            return result;
        }
        if name == crate::intern::intern("__doc__") {
            return unsafe { crate::abi::pon_none() };
        }
        let attr = crate::intern::resolve(name).unwrap_or_else(|| format!("<interned:{name}>"));
        return raise_type_error(&format!(
            "'{}' object does not support attribute lookup (attribute '{attr}')",
            unsafe { (*ty).name() },
        ));
    };
    let Some(name_object) = interned_name_object(name) else {
        return raise_type_error("attribute name is not interned");
    };

    let result = unsafe { slot(object, name_object) };
    if result.is_null() {
        // Native resolvers answer only their method tables; `__class__` is
        // universal, so recover it here.  Heap types go through
        // `generic_get_attr`, which serves `__class__` itself (and lets a
        // data-descriptor override win), so it is excluded from the rescue.
        if name == crate::intern::intern("__class__")
            && !core::ptr::fn_addr_eq(slot, crate::descr::generic_get_attr as unsafe extern "C" fn(_, _) -> _)
        {
            crate::thread_state::pon_err_clear();
            return unsafe { crate::types::type_::canonical_type_object(ty) }.cast::<PyObject>();
        }
        ensure_exception("attribute lookup returned NULL without setting an exception");
    }
    result
}

/// Dispatches attribute assignment through `tp_setattro`.
pub unsafe fn set_attr(object: *mut PyObject, name: u32, value: *mut PyObject) -> i32 {
    unsafe { set_or_del_attr(object, name, value, "object does not support attribute assignment") }
}

/// Dispatches attribute deletion through `tp_setattro` with a NULL value.
pub unsafe fn del_attr(object: *mut PyObject, name: u32) -> i32 {
    unsafe { set_or_del_attr(object, name, ptr::null_mut(), "object does not support attribute deletion") }
}

/// Builds an iterator through `tp_iter`, falling back to the sequence iterator
/// seam when present.
pub unsafe fn get_iter(object: *mut PyObject) -> *mut PyObject {
    let Some(ty) = (unsafe { object_type(object) }) else {
        return raise_type_error("iterable operand is NULL or has no type");
    };

    let slot = unsafe { (*ty).tp_iter.or_else(|| (*ty).tp_as_sequence.as_ref().and_then(|methods| methods.sq_iter)) };
    match unsafe { call_unary_slot(slot, object) } {
        SlotOutcome::Value(value) => value,
        SlotOutcome::Error => ptr::null_mut(),
        SlotOutcome::Missing | SlotOutcome::NotImplemented => {
            // Python-level `__iter__` (heap instances, e.g. WeakSet).
            let hook = unsafe { crate::descr::lookup_in_type(ty, crate::intern::intern("__iter__")) };
            if hook.is_null() {
                // Legacy sequence-iteration protocol (CPython
                // `PyObject_GetIter` -> `PySeqIter_New`): a type providing
                // `__getitem__` — native `sq_item` or a Python-level dunder
                // resolved on the class MRO, never the instance — iterates by
                // calling `__getitem__(0)`, `__getitem__(1)`, ... until
                // IndexError (vendored `re._parser.SubPattern`).
                let sq_item = unsafe { (*ty).tp_as_sequence.as_ref().and_then(|methods| methods.sq_item) };
                if sq_item.is_some()
                    || !unsafe { crate::descr::lookup_in_type(ty, crate::intern::intern("__getitem__")) }.is_null()
                {
                    return crate::types::lazy_iter::new_seq_iter(object);
                }
                return raise_type_error(&format!("'{}' object is not iterable", unsafe { (*ty).name() }));
            }
            let bound = unsafe { crate::descr::descriptor_get(hook, object, ty) };
            if bound.is_null() {
                return ptr::null_mut();
            }
            unsafe { abi::pon_call(bound, ptr::null_mut(), 0) }
        }
    }
}

/// Advances an iterator through `tp_iternext`, falling back to the sequence
/// next seam when present.  A NULL slot result is preserved so iterator slots can
/// signal either StopIteration or another current exception.
pub unsafe fn iter_next(iterator: *mut PyObject) -> *mut PyObject {
    let Some(ty) = (unsafe { object_type(iterator) }) else {
        return raise_type_error("iterator operand is NULL or has no type");
    };

    let slot = unsafe { (*ty).tp_iternext.or_else(|| (*ty).tp_as_sequence.as_ref().and_then(|methods| methods.sq_iternext)) };
    match unsafe { call_unary_slot(slot, iterator) } {
        SlotOutcome::Value(value) => value,
        SlotOutcome::Error => ptr::null_mut(),
        SlotOutcome::Missing | SlotOutcome::NotImplemented => {
            // Python-level `__next__` (heap instances, e.g. tempfile's
            // `_RandomNameSequence`): heap classes carry the dunder in their
            // namespace rather than tp_iternext, mirroring `get_iter`'s
            // `__iter__` fallback above.
            let hook = unsafe { crate::descr::lookup_in_type(ty, crate::intern::intern("__next__")) };
            if hook.is_null() {
                return raise_type_error(&format!("'{}' object is not an iterator", unsafe { (*ty).name() }));
            }
            let bound = unsafe { crate::descr::descriptor_get(hook, iterator, ty) };
            if bound.is_null() {
                return ptr::null_mut();
            }
            unsafe { abi::pon_call(bound, ptr::null_mut(), 0) }
        }
    }
}

/// Shared `tp_getattro` for built-in iterator types: serves `__next__` and
/// `__iter__` as bound methods forwarding through the runtime iterator
/// protocol, and raises `AttributeError` for every other name.  CPython
/// exposes these as type-dict slot wrappers; pon's native iterator types have
/// no type dicts, so stdlib idioms like `iter(x).__next__` bind here instead.
pub(crate) unsafe extern "C" fn iterator_dunder_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let name = crate::tag::untag_arg(name);
    let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        return raise_type_error("attribute name must be str");
    };
    let entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject = match name_text {
        "__next__" => iterator_dunder_next_method,
        "__iter__" => iterator_dunder_iter_method,
        _ => return unsafe { abi::exc::pon_raise_attribute_error(object, crate::intern::intern(name_text)) },
    };
    // SAFETY: `entry` is a live builtin entry point with the runtime calling convention.
    let function = unsafe { abi::pon_make_function(entry as *const u8, crate::builtins::variadic_arity(), crate::intern::intern(name_text)) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match crate::types::method::new_bound_method(function, object) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => {
            crate::thread_state::pon_err_set(message);
            ptr::null_mut()
        }
    }
}

/// Bound `iterator.__next__()`: forwards to [`iter_next`]; iterator slots
/// raise their own typed `StopIteration`.
unsafe extern "C" fn iterator_dunder_next_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 || argv.is_null() {
        return raise_type_error(&format!("__next__() takes no arguments ({} given)", argc.saturating_sub(1)));
    }
    // SAFETY: The call helper supplies `argv` with at least one live entry.
    unsafe { iter_next(crate::tag::untag_arg(*argv)) }
}

/// Bound `iterator.__iter__()`: identity, mirroring the `tp_iter` slot.
unsafe extern "C" fn iterator_dunder_iter_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 || argv.is_null() {
        return raise_type_error(&format!("__iter__() takes no arguments ({} given)", argc.saturating_sub(1)));
    }
    // SAFETY: The call helper supplies `argv` with at least one live entry.
    unsafe { crate::tag::untag_arg(*argv) }
}

/// Dispatches subscription through mapping `mp_subscript`, then through the
/// integer sequence-item seam when the key is an exact PyLong.
pub unsafe fn subscript_get(object: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    let Some(ty) = (unsafe { object_type(object) }) else {
        return raise_type_error("subscription receiver is NULL or has no type");
    };

    if let Some(slot) = unsafe { (*ty).tp_as_mapping.as_ref().and_then(|methods| methods.mp_subscript) } {
        let result = unsafe { slot(object, key) };
        if result.is_null() {
            ensure_exception("mapping subscript returned NULL without setting an exception");
        }
        return result;
    }

    if let Some(slot) = unsafe { (*ty).tp_as_sequence.as_ref().and_then(|methods| methods.sq_item) } {
        if !unsafe { is_exact_pylong(key) } {
            return raise_type_error("sequence index must be an int");
        }
        // SAFETY: The exact-type check above proves PyLong layout.
        let value = unsafe { (*key.cast::<PyLong>()).value };
        let Ok(index) = isize::try_from(value) else {
            return raise_type_error("sequence index is out of range for this platform");
        };
        let result = unsafe { slot(object, index) };
        if result.is_null() {
            ensure_exception("sequence item slot returned NULL without setting an exception");
        }
        return result;
    }

    // PEP 695/585 fallback: pon builtin constructors (`list`, `dict`, ...)
    // are `PyFunction` objects without mapping/sequence tables, but
    // `list[int]` must still produce a `types.GenericAlias`.
    if let Some(alias) = unsafe { builtin_constructor_generic_alias(object, key) } {
        return alias;
    }

    if let Some(alias) = unsafe { builtin_type_generic_alias(object, key) } {
        return alias;
    }

    // Python-level `__getitem__` (heap instances, incl. dict subclasses
    // reaching the natives installed in dict's tp_dict; user overrides win
    // by MRO order).  Mirrors the `__delitem__` fallback in `subscript_del`.
    let getitem = unsafe { crate::descr::lookup_in_type(ty, crate::intern::intern("__getitem__")) };
    if !getitem.is_null() {
        let callable = unsafe { crate::descr::descriptor_get(getitem, object, ty) };
        if callable.is_null() {
            return ptr::null_mut();
        }
        let mut argv = [key];
        return unsafe { abi::pon_call(callable, argv.as_mut_ptr(), argv.len()) };
    }

    // PEP 560: subscription of a class object dispatches to its
    // `__class_getitem__` (CPython `PyObject_GetItem`'s type-receiver
    // fallback, after every mapping/sequence/metatype-`__getitem__` leg
    // missed).  The hook is looked up on the class's own MRO and bound as
    // `__get__(NULL, cls)`, so classmethod descriptors receive the
    // *subscripted* class: `IO[bytes]` passes `cls=IO` even though the hook
    // lives on `Generic`.
    if unsafe { crate::types::type_::is_type_object(object) } {
        let hook = unsafe { crate::descr::lookup_in_type(object.cast::<PyType>(), crate::intern::intern("__class_getitem__")) };
        if !hook.is_null() {
            let bound = unsafe { crate::descr::descriptor_get(hook, ptr::null_mut(), object.cast::<PyType>()) };
            if bound.is_null() {
                return ptr::null_mut();
            }
            let mut argv = [key];
            return unsafe { abi::pon_call(bound, argv.as_mut_ptr(), argv.len()) };
        }
    }

    raise_type_error(&format!("'{}' object is not subscriptable", unsafe { (*ty).name() }))
}

/// Deletes a subscription through mapping/sequence assignment slots or `__delitem__`.
pub unsafe fn subscript_del(object: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    let Some(ty) = (unsafe { object_type(object) }) else {
        return raise_type_error("subscription receiver is NULL or has no type");
    };

    if let Some(slot) = unsafe { (*ty).tp_as_mapping.as_ref().and_then(|methods| methods.mp_ass_subscript) } {
        let status = unsafe { slot(object, key, ptr::null_mut()) };
        if status < 0 {
            ensure_exception("mapping delete slot returned an error without setting an exception");
            return ptr::null_mut();
        }
        return unsafe { abi::pon_none() };
    }

    if let Some(slot) = unsafe { (*ty).tp_as_sequence.as_ref().and_then(|methods| methods.sq_ass_item) } {
        if !unsafe { is_exact_pylong(key) } {
            return raise_type_error("sequence index must be an int");
        }
        let value = unsafe { (*key.cast::<PyLong>()).value };
        let Ok(index) = isize::try_from(value) else {
            return raise_type_error("sequence index is out of range for this platform");
        };
        let status = unsafe { slot(object, index, ptr::null_mut()) };
        if status < 0 {
            ensure_exception("sequence delete slot returned an error without setting an exception");
            return ptr::null_mut();
        }
        return unsafe { abi::pon_none() };
    }

    let delitem = unsafe { crate::descr::lookup_in_type(ty, crate::intern::intern("__delitem__")) };
    if !delitem.is_null() {
        let callable = unsafe { crate::descr::descriptor_get(delitem, object, ty) };
        if callable.is_null() {
            return ptr::null_mut();
        }
        let mut argv = [key];
        let result = unsafe { abi::pon_call(callable, argv.as_mut_ptr(), argv.len()) };
        if result.is_null() {
            return ptr::null_mut();
        }
        return unsafe { abi::pon_none() };
    }

    // CPython wording differs by key kind: integer keys reach the
    // `PySequence_DelItem` error ("doesn't"), everything else the
    // `PyObject_DelItem` error ("does not").
    let verb = if unsafe { is_exact_pylong(key) } { "doesn't" } else { "does not" };
    raise_type_error(&format!("'{}' object {verb} support item deletion", unsafe { (*ty).name() }))
}

/// Builds `types.GenericAlias` for `builtin[key]` subscripts on constructor
/// functions (`list[int]`, `dict[str, int]`).  Tuple keys flatten into the
/// alias argument list; non-constructor receivers return `None` so the caller
/// can raise the ordinary TypeError.
unsafe fn builtin_constructor_generic_alias(object: *mut PyObject, key: *mut PyObject) -> Option<*mut PyObject> {
    let ty = unsafe { object_type(object)? };
    if unsafe { (*ty).name() } != "function" {
        return None;
    }
    let function = unsafe { &*object.cast::<crate::object::PyFunction>() };
    let name = crate::intern::resolve(function.name_interned)?;
    if !crate::types::typealias::is_subscriptable_builtin_constructor(&name) {
        return None;
    }
    let key_is_tuple = unsafe { object_type(key) }.is_some_and(|key_ty| unsafe { (*key_ty).name() } == "tuple");
    let args = if key_is_tuple {
        unsafe { (&*key.cast::<crate::types::tuple::PyTuple>()).as_slice() }.to_vec()
    } else {
        vec![key]
    };
    Some(crate::types::typealias::new_generic_alias(object, args))
}

unsafe fn builtin_type_generic_alias(object: *mut PyObject, key: *mut PyObject) -> Option<*mut PyObject> {
    let ty = unsafe { object_type(object)? };
    if unsafe { (*ty).name() } != "type" {
        return None;
    }
    let name = unsafe { (*object.cast::<PyType>()).name() };
    if !crate::types::typealias::is_subscriptable_builtin_constructor(name) {
        return None;
    }
    let key_is_tuple = unsafe { object_type(key) }.is_some_and(|key_ty| unsafe { (*key_ty).name() } == "tuple");
    let args = if key_is_tuple {
        unsafe { (&*key.cast::<crate::types::tuple::PyTuple>()).as_slice() }.to_vec()
    } else {
        vec![key]
    };
    Some(crate::types::typealias::new_generic_alias(object, args))
}

unsafe fn set_or_del_attr(object: *mut PyObject, name: u32, value: *mut PyObject, missing_message: &str) -> i32 {
    let Some(ty) = (unsafe { object_type(object) }) else {
        return raise_type_error_status("attribute receiver is NULL or has no type");
    };
    let Some(slot) = (unsafe { (*ty).tp_setattro }) else {
        return raise_type_error_status(missing_message);
    };
    let Some(name_object) = interned_name_object(name) else {
        return raise_type_error_status("attribute name is not interned");
    };

    let status = unsafe { slot(object, name_object, value) };
    if status < 0 {
        ensure_exception("attribute setter returned an error without setting an exception");
        -1
    } else {
        0
    }
}

unsafe fn forward_binary_slot(ty: *mut PyType, op: u8) -> Option<BinaryFunc> {
    unsafe {
        (*ty).tp_as_number.as_ref().and_then(|methods| match op {
            BINARY_ADD => methods.nb_add,
            BINARY_SUB => methods.nb_subtract,
            BINARY_MUL => methods.nb_multiply,
            BINARY_DIV => methods.nb_true_divide,
            BINARY_FLOORDIV => methods.nb_floor_divide,
            BINARY_MOD => methods.nb_remainder,
            BINARY_POW => None,
            BINARY_LSHIFT => methods.nb_lshift,
            BINARY_RSHIFT => methods.nb_rshift,
            BINARY_AND => methods.nb_and,
            BINARY_OR => methods.nb_or,
            BINARY_XOR => methods.nb_xor,
            BINARY_MATMUL => methods.nb_matrix_multiply,
            _ => None,
        })
    }
}

unsafe fn reflected_binary_slot(ty: *mut PyType, op: u8) -> Option<BinaryFunc> {
    unsafe {
        (*ty).tp_as_number.as_ref().and_then(|methods| match op {
            BINARY_ADD => methods.nb_reflected_add,
            BINARY_SUB => methods.nb_reflected_subtract,
            BINARY_MUL => methods.nb_reflected_multiply,
            BINARY_DIV => methods.nb_reflected_true_divide,
            BINARY_FLOORDIV => methods.nb_reflected_floor_divide,
            BINARY_MOD => methods.nb_reflected_remainder,
            BINARY_POW => None,
            BINARY_LSHIFT => methods.nb_reflected_lshift,
            BINARY_RSHIFT => methods.nb_reflected_rshift,
            BINARY_AND => methods.nb_reflected_and,
            BINARY_OR => methods.nb_reflected_or,
            BINARY_XOR => methods.nb_reflected_xor,
            BINARY_MATMUL => methods.nb_reflected_matrix_multiply,
            _ => None,
        })
    }
}

unsafe fn inplace_binary_slot(ty: *mut PyType, op: u8) -> Option<BinaryFunc> {
    unsafe {
        (*ty).tp_as_number.as_ref().and_then(|methods| match op {
            BINARY_ADD => methods.nb_inplace_add,
            BINARY_SUB => methods.nb_inplace_subtract,
            BINARY_MUL => methods.nb_inplace_multiply,
            BINARY_DIV => methods.nb_inplace_true_divide,
            BINARY_FLOORDIV => methods.nb_inplace_floor_divide,
            BINARY_MOD => methods.nb_inplace_remainder,
            // `nb_inplace_power` is ternary; `**=` falls back to the binary path.
            BINARY_POW => None,
            BINARY_LSHIFT => methods.nb_inplace_lshift,
            BINARY_RSHIFT => methods.nb_inplace_rshift,
            BINARY_AND => methods.nb_inplace_and,
            BINARY_OR => methods.nb_inplace_or,
            BINARY_XOR => methods.nb_inplace_xor,
            BINARY_MATMUL => methods.nb_inplace_matrix_multiply,
            _ => None,
        })
    }
}

/// Augmented-assignment dunder spellings for a binary numeric op.
fn inplace_dunder_name(op: u8) -> Option<&'static str> {
    Some(match op {
        BINARY_ADD => "__iadd__",
        BINARY_SUB => "__isub__",
        BINARY_MUL => "__imul__",
        BINARY_DIV => "__itruediv__",
        BINARY_FLOORDIV => "__ifloordiv__",
        BINARY_MOD => "__imod__",
        BINARY_POW => "__ipow__",
        BINARY_LSHIFT => "__ilshift__",
        BINARY_RSHIFT => "__irshift__",
        BINARY_AND => "__iand__",
        BINARY_OR => "__ior__",
        BINARY_XOR => "__ixor__",
        BINARY_MATMUL => "__imatmul__",
        _ => return None,
    })
}

/// CPython `binary_iop1` first phase: the receiver's in-place slot (native
/// types: PEP 584 `dict.__ior__`) or `__i*__` dunder (heap classes) sees the
/// operands before the plain binary dispatch.  `Some` carries the handled
/// result (possibly NULL with an exception set); `None` means "not handled —
/// fall through to the binary path".
pub(crate) unsafe fn try_inplace_binary(op: u8, a: *mut PyObject, b: *mut PyObject) -> Option<*mut PyObject> {
    let ty = unsafe { object_type(a) }?;
    match unsafe { call_binary_slot(inplace_binary_slot(ty, op), a, b) } {
        SlotOutcome::Value(value) => return Some(value),
        SlotOutcome::Error => return Some(ptr::null_mut()),
        SlotOutcome::Missing | SlotOutcome::NotImplemented => {}
    }
    if let Some(name) = inplace_dunder_name(op) {
        match unsafe { call_binary_dunder(ty, name, a, b) } {
            SlotOutcome::Value(value) => return Some(value),
            SlotOutcome::Error => return Some(ptr::null_mut()),
            SlotOutcome::Missing | SlotOutcome::NotImplemented => {}
        }
    }
    None
}

unsafe fn rich_dunder(ty: *mut PyType, op: u8) -> *mut PyObject {
    let name = match op {
        RICH_LT => "__lt__",
        RICH_LE => "__le__",
        RICH_EQ => "__eq__",
        RICH_NE => "__ne__",
        RICH_GT => "__gt__",
        RICH_GE => "__ge__",
        _ => return ptr::null_mut(),
    };
    unsafe { crate::descr::lookup_in_type(ty, crate::intern::intern(name)) }
}

unsafe fn call_rich_dunder(method: *mut PyObject, obj: *mut PyObject, other: *mut PyObject, owner: *mut PyType) -> SlotOutcome {
    if method.is_null() {
        return SlotOutcome::Missing;
    }
    let callable = unsafe { crate::descr::descriptor_get(method, obj, owner) };
    if callable.is_null() {
        return SlotOutcome::Error;
    }
    let mut argv = [other];
    let result = unsafe { abi::pon_call(callable, argv.as_mut_ptr(), argv.len()) };
    slot_result(result, "rich comparison method returned NULL without setting an exception")
}
/// `object.__ne__` default for one operand of the rich-compare terminus:
/// resolve `ty`'s Python-level `__eq__`, call it as `obj.__eq__(other)`, and
/// invert a non-`NotImplemented` result's truth value.  `None` keeps the
/// dispatcher falling through (no `__eq__`, or it reported NotImplemented);
/// a raised error surfaces as `Some(NULL)`.
unsafe fn ne_delegates_to_eq(ty: *mut PyType, obj: *mut PyObject, other: *mut PyObject) -> Option<*mut PyObject> {
    let eq = unsafe { rich_dunder(ty, RICH_EQ) };
    if eq.is_null() {
        return None;
    }
    match unsafe { call_rich_dunder(eq, obj, other, ty) } {
        SlotOutcome::Value(value) => {
            let truth = unsafe { is_true(value) };
            if truth < 0 {
                return Some(ptr::null_mut());
            }
            Some(unsafe { abi::number::pon_const_bool(i32::from(truth == 0)) })
        }
        SlotOutcome::Error => Some(ptr::null_mut()),
        SlotOutcome::Missing | SlotOutcome::NotImplemented => None,
    }
}

/// Forward/reflected Python dunder spellings for a binary numeric op.
fn binary_dunder_names(op: u8) -> Option<(&'static str, &'static str)> {
    Some(match op {
        BINARY_ADD => ("__add__", "__radd__"),
        BINARY_SUB => ("__sub__", "__rsub__"),
        BINARY_MUL => ("__mul__", "__rmul__"),
        BINARY_DIV => ("__truediv__", "__rtruediv__"),
        BINARY_FLOORDIV => ("__floordiv__", "__rfloordiv__"),
        BINARY_MOD => ("__mod__", "__rmod__"),
        BINARY_POW => ("__pow__", "__rpow__"),
        BINARY_LSHIFT => ("__lshift__", "__rlshift__"),
        BINARY_RSHIFT => ("__rshift__", "__rrshift__"),
        BINARY_AND => ("__and__", "__rand__"),
        BINARY_OR => ("__or__", "__ror__"),
        BINARY_XOR => ("__xor__", "__rxor__"),
        BINARY_MATMUL => ("__matmul__", "__rmatmul__"),
        _ => return None,
    })
}

/// Calls a Python-level binary dunder resolved from a HEAP class's MRO.
/// Native receivers report `Missing` (their behavior lives in slots).
unsafe fn call_binary_dunder(ty: *mut PyType, name: &str, receiver: *mut PyObject, other: *mut PyObject) -> SlotOutcome {
    if !crate::types::type_::type_dispatches_python_dunders(ty.cast_const()) {
        return SlotOutcome::Missing;
    }
    let hook = unsafe { crate::descr::lookup_in_type(ty, crate::intern::intern(name)) };
    if hook.is_null() {
        return SlotOutcome::Missing;
    }
    let callable = unsafe { crate::descr::descriptor_get(hook, receiver, ty) };
    if callable.is_null() {
        return SlotOutcome::Error;
    }
    let mut argv = [other];
    let result = unsafe { abi::pon_call(callable, argv.as_mut_ptr(), argv.len()) };
    slot_result(result, "binary dunder returned NULL without setting an exception")
}

/// Calls a Python-level unary dunder resolved from a HEAP class's MRO.
unsafe fn call_unary_dunder(ty: *mut PyType, name: &str, receiver: *mut PyObject) -> SlotOutcome {
    if !crate::types::type_::type_dispatches_python_dunders(ty.cast_const()) {
        return SlotOutcome::Missing;
    }
    let hook = unsafe { crate::descr::lookup_in_type(ty, crate::intern::intern(name)) };
    if hook.is_null() {
        return SlotOutcome::Missing;
    }
    let callable = unsafe { crate::descr::descriptor_get(hook, receiver, ty) };
    if callable.is_null() {
        return SlotOutcome::Error;
    }
    let result = unsafe { abi::pon_call(callable, ptr::null_mut(), 0) };
    slot_result(result, "unary dunder returned NULL without setting an exception")
}


fn same_binary_slot(left: Option<BinaryFunc>, right: Option<BinaryFunc>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => (left as usize) == (right as usize),
        _ => false,
    }
}

unsafe fn unicode_bytes(value: &PyUnicode) -> &[u8] {
    if value.data.is_null() && value.len != 0 {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(value.data, value.len) }
    }
}

unsafe fn try_sequence_repeat(ty: *mut PyType, sequence: *mut PyObject, count: *mut PyObject) -> SlotOutcome {
    let slot = unsafe { (*ty).tp_as_sequence.as_ref().and_then(|methods| methods.sq_repeat) };
    unsafe { call_binary_slot(slot, sequence, count) }
}

fn same_richcmp_slot(left: Option<RichCmpFunc>, right: Option<RichCmpFunc>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => (left as usize) == (right as usize),
        _ => false,
    }
}

unsafe fn try_forward_binary(ty: *mut PyType, op: u8, a: *mut PyObject, b: *mut PyObject) -> SlotOutcome {
    let number_slot = unsafe {
        (*ty).tp_as_number.as_ref().and_then(|methods| match op {
            BINARY_ADD => methods.nb_add,
            BINARY_SUB => methods.nb_subtract,
            BINARY_MUL => methods.nb_multiply,
            BINARY_DIV => methods.nb_true_divide,
            BINARY_FLOORDIV => methods.nb_floor_divide,
            BINARY_MOD => methods.nb_remainder,
            BINARY_POW => None,
            BINARY_LSHIFT => methods.nb_lshift,
            BINARY_RSHIFT => methods.nb_rshift,
            BINARY_AND => methods.nb_and,
            BINARY_OR => methods.nb_or,
            BINARY_XOR => methods.nb_xor,
            BINARY_MATMUL => methods.nb_matrix_multiply,
            _ => None,
        })
    };

    match unsafe { call_binary_slot(number_slot, a, b) } {
        SlotOutcome::Missing | SlotOutcome::NotImplemented if op == BINARY_ADD => {
            let sequence_slot = unsafe { (*ty).tp_as_sequence.as_ref().and_then(|methods| methods.sq_concat) };
            unsafe { call_binary_slot(sequence_slot, a, b) }
        }
        outcome => outcome,
    }
}


unsafe fn call_richcmp_slot(
    slot: Option<RichCmpFunc>,
    op: u8,
    a: *mut PyObject,
    b: *mut PyObject,
) -> SlotOutcome {
    let Some(slot) = slot else {
        return SlotOutcome::Missing;
    };
    if !matches!(op, RICH_LT | RICH_LE | RICH_EQ | RICH_NE | RICH_GT | RICH_GE) {
        return SlotOutcome::Missing;
    }
    let result = unsafe { slot(a, b, c_int::from(op)) };
    slot_result(result, "rich comparison slot returned NULL without setting an exception")
}

unsafe fn call_binary_slot(slot: Option<BinaryFunc>, a: *mut PyObject, b: *mut PyObject) -> SlotOutcome {
    let Some(slot) = slot else {
        return SlotOutcome::Missing;
    };
    let result = unsafe { slot(a, b) };
    slot_result(result, "binary slot returned NULL without setting an exception")
}

unsafe fn call_unary_slot(slot: Option<UnaryFunc>, object: *mut PyObject) -> SlotOutcome {
    let Some(slot) = slot else {
        return SlotOutcome::Missing;
    };
    let result = unsafe { slot(object) };
    slot_result(result, "unary slot returned NULL without setting an exception")
}

fn slot_result(result: *mut PyObject, missing_exception_message: &str) -> SlotOutcome {
    if result.is_null() {
        ensure_exception(missing_exception_message);
        SlotOutcome::Error
    } else if unsafe { is_not_implemented(result) } {
        SlotOutcome::NotImplemented
    } else {
        SlotOutcome::Value(result)
    }
}

fn interned_name_object(name: u32) -> Option<*mut PyObject> {
    let name = crate::intern::resolve(name)?;
    let object = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
    (!object.is_null()).then_some(object)
}

unsafe fn object_type(object: *mut PyObject) -> Option<*mut PyType> {
    if object.is_null() {
        return None;
    }
    // SAFETY: Non-NULL boxed values are required to begin with PyObjectHeader.
    let ty = unsafe { (*object).ob_type.cast_mut() };
    (!ty.is_null()).then_some(ty)
}

unsafe fn is_exact_pylong(object: *mut PyObject) -> bool {
    let Some(ty) = (unsafe { object_type(object) }) else {
        return false;
    };
    // SAFETY: `ty` comes from a live object header.
    let ty = unsafe { &*ty };
    ty.tp_basicsize == mem::size_of::<PyLong>() && ty.name() == "int"
}

pub(crate) unsafe fn is_not_implemented(object: *mut PyObject) -> bool {
    let Some(ty) = (unsafe { object_type(object) }) else {
        return false;
    };
    // SAFETY: `ty` comes from a live object header.  B0 has no NotImplemented
    // singleton yet; this type-name hook makes the fallback path correct as
    // soon as the singleton type lands.
    unsafe { (*ty).name() == "NotImplementedType" }
}

unsafe fn is_subtype(candidate: *mut PyType, base: *mut PyType) -> bool {
    let mut current = candidate;
    while !current.is_null() {
        if current == base {
            return true;
        }
        // SAFETY: `current` is either a live type pointer from a type chain or NULL.
        current = unsafe { (*current).tp_base };
    }
    false
}

fn swapped_rich_op(op: u8) -> u8 {
    match op {
        RICH_LT => RICH_GT,
        RICH_LE => RICH_GE,
        RICH_GT => RICH_LT,
        RICH_GE => RICH_LE,
        RICH_EQ | RICH_NE => op,
        _ => op,
    }
}

unsafe fn normalize_inquiry(value: c_int, missing_exception_message: &str) -> i32 {
    match value {
        0 => 0,
        1 => 1,
        -1 => {
            ensure_exception(missing_exception_message);
            -1
        }
        _ => raise_type_error_status("__bool__ returned a non-boolean value"),
    }
}

unsafe fn len_to_truth(value: isize, missing_exception_message: &str) -> i32 {
    if value < 0 {
        ensure_exception(missing_exception_message);
        -1
    } else {
        i32::from(value != 0)
    }
}

fn ensure_exception(message: &str) {
    if !pon_err_occurred() {
        let _ = raise_type_error(message);
    }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn raise_type_error_status(message: &str) -> i32 {
    let _ = raise_type_error(message);
    -1
}

/// Operator spelling for the terminal binary TypeError: augmented forms carry
/// the `=` suffix and forward `**` reports "** or pow()", both per CPython.
fn binary_op_spelling(op: u8, inplace: bool) -> Option<&'static str> {
    Some(match (op, inplace) {
        (BINARY_ADD, false) => "+",
        (BINARY_ADD, true) => "+=",
        (BINARY_SUB, false) => "-",
        (BINARY_SUB, true) => "-=",
        (BINARY_MUL, false) => "*",
        (BINARY_MUL, true) => "*=",
        (BINARY_MATMUL, false) => "@",
        (BINARY_MATMUL, true) => "@=",
        (BINARY_DIV, false) => "/",
        (BINARY_DIV, true) => "/=",
        (BINARY_FLOORDIV, false) => "//",
        (BINARY_FLOORDIV, true) => "//=",
        (BINARY_MOD, false) => "%",
        (BINARY_MOD, true) => "%=",
        (BINARY_POW, false) => "** or pow()",
        (BINARY_POW, true) => "**=",
        (BINARY_LSHIFT, false) => "<<",
        (BINARY_LSHIFT, true) => "<<=",
        (BINARY_RSHIFT, false) => ">>",
        (BINARY_RSHIFT, true) => ">>=",
        (BINARY_AND, false) => "&",
        (BINARY_AND, true) => "&=",
        (BINARY_OR, false) => "|",
        (BINARY_OR, true) => "|=",
        (BINARY_XOR, false) => "^",
        (BINARY_XOR, true) => "^=",
        _ => return None,
    })
}

/// Raises the CPython-shaped terminal TypeError for a binary operation no
/// candidate handled: `unsupported operand type(s) for |: 'dict' and 'list'`.
pub(crate) unsafe fn raise_binary_unsupported(op: u8, a: *mut PyObject, b: *mut PyObject, inplace: bool) -> *mut PyObject {
    let Some(spelling) = binary_op_spelling(op, inplace) else {
        return raise_type_error("unknown binary operation");
    };
    let left = unsafe { object_type(a) }.map_or("object", |ty| unsafe { (*ty).name() });
    let right = unsafe { object_type(b) }.map_or("object", |ty| unsafe { (*ty).name() });
    raise_type_error(&format!("unsupported operand type(s) for {spelling}: '{left}' and '{right}'"))
}

fn unary_unsupported_message(op: u8) -> &'static str {
    match op {
        UNARY_NEG => "unsupported operand for unary -",
        UNARY_POS => "unsupported operand for unary +",
        UNARY_INVERT => "unsupported operand for ~",
        _ => "unknown unary operation",
    }
}

fn rich_op_symbol(op: u8) -> Option<&'static str> {
    match op {
        RICH_LT => Some("<"),
        RICH_LE => Some("<="),
        RICH_GT => Some(">"),
        RICH_GE => Some(">="),
        _ => None,
    }
}

unsafe fn rich_unsupported_message(op: u8, a: *mut PyObject, b: *mut PyObject) -> String {
    let Some(symbol) = rich_op_symbol(op) else {
        return "unknown rich comparison operation".to_owned();
    };
    let left = unsafe { object_type(a) }
        .map(|ty| unsafe { (*ty).name() }.to_owned())
        .unwrap_or_else(|| "object".to_owned());
    let right = unsafe { object_type(b) }
        .map(|ty| unsafe { (*ty).name() }.to_owned())
        .unwrap_or_else(|| "object".to_owned());
    format!("'{symbol}' not supported between instances of '{left}' and '{right}'")
}
