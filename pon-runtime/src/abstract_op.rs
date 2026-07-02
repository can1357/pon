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
    if op == BINARY_ADD && unsafe { is_exact_pylong(a) && is_exact_pylong(b) } {
        // SAFETY: The exact-type checks above prove both operands use PyLong's layout.
        let left = unsafe { (*a.cast::<PyLong>()).value };
        let right = unsafe { (*b.cast::<PyLong>()).value };
        return match left.checked_add(right) {
            Some(sum) => unsafe { abi::pon_const_int(sum) },
            None => raise_type_error("integer addition overflow"),
        };
    }

    let Some(left_type) = (unsafe { object_type(a) }) else {
        return raise_type_error("left operand is NULL or has no type");
    };
    let Some(right_type) = (unsafe { object_type(b) }) else {
        return raise_type_error("right operand is NULL or has no type");
    };

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

    raise_type_error(binary_unsupported_message(op))
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
        SlotOutcome::Value(value) => value,
        SlotOutcome::Error => ptr::null_mut(),
        SlotOutcome::Missing | SlotOutcome::NotImplemented => raise_type_error(unary_unsupported_message(op)),
    }
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
        return unsafe { abi::pon_const_int(if result { 1 } else { 0 }) };
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

    match op {
        RICH_EQ => unsafe { abi::pon_const_int(i64::from(a == b)) },
        RICH_NE => unsafe { abi::pon_const_int(i64::from(a != b)) },
        _ => {
            let message = unsafe { rich_unsupported_message(op, a, b) };
            raise_type_error(&message)
        }
    }
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
            unsafe { abi::pon_const_int(i64::from(if op == RICH_EQ { equal } else { !equal })) }
        }
        _ => raise_type_error("complex numbers are not orderable"),
    }
}

fn rich_compare_complex(op: u8, left_real: f64, left_imag: f64, right_real: f64, right_imag: f64) -> *mut PyObject {
    match op {
        RICH_EQ | RICH_NE => {
            let equal = left_real == right_real && left_imag == right_imag;
            unsafe { abi::pon_const_int(i64::from(if op == RICH_EQ { equal } else { !equal })) }
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
    unsafe { abi::pon_const_int(i64::from(result)) }
}

fn rich_compare_int_float(op: u8, integer: &num_bigint::BigInt, float: f64) -> *mut PyObject {
    if float.is_nan() {
        let result = op == RICH_NE;
        return unsafe { abi::pon_const_int(i64::from(result)) };
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
    unsafe { abi::pon_const_int(i64::from(result)) }
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

    1
}

/// Dispatches attribute lookup through `tp_getattro`.
pub unsafe fn get_attr(object: *mut PyObject, name: u32) -> *mut PyObject {
    let Some(ty) = (unsafe { object_type(object) }) else {
        return raise_type_error("attribute receiver is NULL or has no type");
    };
    let Some(slot) = (unsafe { (*ty).tp_getattro }) else {
        return raise_type_error("object does not support attribute lookup");
    };
    let Some(name_object) = interned_name_object(name) else {
        return raise_type_error("attribute name is not interned");
    };

    let result = unsafe { slot(object, name_object) };
    if result.is_null() {
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
        SlotOutcome::Missing | SlotOutcome::NotImplemented => raise_type_error("object is not iterable"),
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
        SlotOutcome::Missing | SlotOutcome::NotImplemented => raise_type_error("object is not an iterator"),
    }
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

    raise_type_error("object is not subscriptable")
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

    raise_type_error("object does not support item deletion")
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

unsafe fn is_not_implemented(object: *mut PyObject) -> bool {
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

fn binary_unsupported_message(op: u8) -> &'static str {
    match op {
        BINARY_ADD => "unsupported operands for +",
        BINARY_SUB => "unsupported operands for -",
        BINARY_MUL => "unsupported operands for *",
        BINARY_MATMUL => "unsupported operands for @",
        BINARY_DIV => "unsupported operands for /",
        BINARY_FLOORDIV => "unsupported operands for //",
        BINARY_MOD => "unsupported operands for %",
        BINARY_POW => "unsupported operands for **",
        BINARY_LSHIFT => "unsupported operands for <<",
        BINARY_RSHIFT => "unsupported operands for >>",
        BINARY_AND => "unsupported operands for &",
        BINARY_OR => "unsupported operands for |",
        BINARY_XOR => "unsupported operands for ^",
        _ => "unknown binary operation",
    }
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
