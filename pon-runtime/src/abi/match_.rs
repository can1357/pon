//! Structural pattern-matching helper family namespace.

use core::ptr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::vec::Vec;

use crate::abstract_op;
use crate::feedback::FeedbackCell;
use crate::intern;
use crate::object::{as_object_ptr, PyObject, PyObjectHeader, PyType, PyUnicode};
use crate::thread_state::{pon_err_clear, pon_err_occurred, pon_err_set_object};
use crate::types::exc::PyBaseException;

/// Pattern-match predicate status: `0` false, `1` true, `-1` error when applicable.
pub type MatchStatus = i32;

/// Boxed carrier for values extracted by `MATCH_KEYS`/`MATCH_CLASS`.
///
/// Later lowering can consume this with the same raw layout as a compact tuple of
/// borrowed `PyObject*` slots without depending on list/tuple construction being
/// available in the runtime bootstrap.
#[repr(C)]
#[derive(Debug)]
pub struct PyMatchValues {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Number of extracted values.
    pub len: usize,
    /// Pointer to `len` boxed-object slots, or NULL when empty.
    pub items: *mut *mut PyObject,
}

fn catch_match_object(f: impl FnOnce() -> *mut PyObject) -> *mut PyObject {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(_) => super::return_null_with_error("match helper panicked"),
    }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    unsafe { super::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn truth_object(value: bool) -> *mut PyObject {
    unsafe { super::pon_const_int(if value { 1 } else { 0 }) }
}

unsafe fn object_type(object: *mut PyObject) -> Option<*mut PyType> {
    if object.is_null() {
        return None;
    }
    let ty = unsafe { (*object).ob_type };
    if ty.is_null() {
        None
    } else {
        Some(ty.cast_mut())
    }
}

unsafe fn is_exact_type_name(object: *mut PyObject, name: &str) -> bool {
    let Some(ty) = (unsafe { object_type(object) }) else {
        return false;
    };
    unsafe { (*ty).name() == name }
}

unsafe fn is_type_object(object: *mut PyObject) -> bool {
    unsafe { is_exact_type_name(object, "type") }
}

unsafe fn is_instance_of(subject: *mut PyObject, cls: *mut PyType) -> bool {
    let Some(mut ty) = (unsafe { object_type(subject) }) else {
        return false;
    };
    while !ty.is_null() {
        if ty == cls {
            return true;
        }
        ty = unsafe { (*ty).tp_base };
    }
    false
}
unsafe fn has_sequence_protocol(subject: *mut PyObject) -> Result<bool, *mut PyObject> {
    let Some(ty) = (unsafe { object_type(subject) }) else {
        return Err(raise_type_error("match subject is NULL or has no type"));
    };
    Ok(unsafe {
        (*ty)
            .tp_as_sequence
            .as_ref()
            .is_some_and(|methods| methods.sq_length.is_some() && methods.sq_item.is_some())
    })
}

unsafe fn has_mapping_protocol(subject: *mut PyObject) -> Result<bool, *mut PyObject> {
    let Some(ty) = (unsafe { object_type(subject) }) else {
        return Err(raise_type_error("match subject is NULL or has no type"));
    };
    Ok(unsafe {
        (*ty)
            .tp_as_mapping
            .as_ref()
            .is_some_and(|methods| methods.mp_length.is_some() && methods.mp_subscript.is_some())
    })
}


unsafe fn sequence_len(subject: *mut PyObject) -> Result<Option<isize>, *mut PyObject> {
    let Some(ty) = (unsafe { object_type(subject) }) else {
        return Err(raise_type_error("match subject is NULL or has no type"));
    };
    let Some(slot) = (unsafe { (*ty).tp_as_sequence.as_ref().and_then(|methods| methods.sq_length) }) else {
        return Ok(None);
    };
    let len = unsafe { slot(subject) };
    if len < 0 {
        if !pon_err_occurred() {
            return Err(raise_type_error("sequence length returned a negative value"));
        }
        return Err(ptr::null_mut());
    }
    Ok(Some(len))
}

unsafe fn mapping_len(subject: *mut PyObject) -> Result<Option<isize>, *mut PyObject> {
    let Some(ty) = (unsafe { object_type(subject) }) else {
        return Err(raise_type_error("match subject is NULL or has no type"));
    };
    let Some(slot) = (unsafe { (*ty).tp_as_mapping.as_ref().and_then(|methods| methods.mp_length) }) else {
        return Ok(None);
    };
    let len = unsafe { slot(subject) };
    if len < 0 {
        if !pon_err_occurred() {
            return Err(raise_type_error("mapping length returned a negative value"));
        }
        return Err(ptr::null_mut());
    }
    Ok(Some(len))
}

unsafe fn match_len(subject: *mut PyObject) -> Result<isize, *mut PyObject> {
    if let Some(len) = unsafe { sequence_len(subject)? } {
        return Ok(len);
    }
    if let Some(len) = unsafe { mapping_len(subject)? } {
        return Ok(len);
    }
    Err(raise_type_error("match subject has no length"))
}

unsafe fn read_sequence_item(subject: *mut PyObject, index: isize) -> *mut PyObject {
    let Some(ty) = (unsafe { object_type(subject) }) else {
        return raise_type_error("sequence item receiver is NULL or has no type");
    };
    let Some(slot) = (unsafe { (*ty).tp_as_sequence.as_ref().and_then(|methods| methods.sq_item) }) else {
        return raise_type_error("object does not support sequence item access");
    };
    let item = unsafe { slot(subject, index) };
    if item.is_null() && !pon_err_occurred() {
        return raise_type_error("sequence item slot returned NULL without setting an exception");
    }
    item
}

fn boxed_match_values(mut values: Vec<*mut PyObject>) -> *mut PyObject {
    let len = values.len();
    let items = if values.is_empty() {
        ptr::null_mut()
    } else {
        values.shrink_to_fit();
        debug_assert_eq!(values.len(), values.capacity());
        let ptr = values.as_mut_ptr();
        std::mem::forget(values);
        ptr
    };
    let ty = match_values_type();
    as_object_ptr(Box::into_raw(Box::new(PyMatchValues {
        ob_base: PyObjectHeader::new(ty.cast_const()),
        len,
        items,
    })))
}

fn match_values_type() -> *mut PyType {
    use std::sync::LazyLock;
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let ty = PyType::new(ptr::null(), "match_values", core::mem::size_of::<PyMatchValues>());
        Box::into_raw(Box::new(ty)) as usize
    });
    *TYPE as *mut PyType
}

unsafe fn mapping_subscript_or_none(subject: *mut PyObject, key: *mut PyObject) -> Option<*mut PyObject> {
    let value = unsafe { abstract_op::subscript_get(subject, key) };
    if value.is_null() {
        pon_err_clear();
        None
    } else {
        Some(value)
    }
}

unsafe fn intern_unicode(object: *mut PyObject) -> Option<u32> {
    if !unsafe { is_exact_type_name(object, "str") } {
        return None;
    }
    let text = unsafe { (*object.cast::<PyUnicode>()).as_str()? };
    Some(intern::intern(text))
}

unsafe fn class_attr_or_none(subject: *mut PyObject, name: u32) -> Option<*mut PyObject> {
    let value = unsafe { abstract_op::get_attr(subject, name) };
    if value.is_null() {
        pon_err_clear();
        None
    } else {
        Some(value)
    }
}

fn raise_assertion_error(message: *mut PyObject) -> *mut PyObject {
    match super::ensure_runtime_initialized() {
        Ok(()) => match super::with_runtime(|runtime| {
            let object = runtime
                .heap
                .alloc(core::mem::size_of::<PyBaseException>(), super::TYPE_ID_EXCEPTION)
                .cast::<PyBaseException>();
            unsafe {
                ptr::write(
                    object,
                    PyBaseException::new(
                        runtime.exception_types.assertion_error.cast_const(),
                        message,
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                    ),
                );
            }
            let exception = as_object_ptr(object);
            pon_err_set_object(exception, "AssertionError");
            ptr::null_mut()
        }) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}

/// Returns boxed true when `subject` is a sequence-pattern candidate.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_match_sequence(subject: *mut PyObject, feedback: *mut FeedbackCell) -> *mut PyObject {
    unsafe { super::record_feedback_unary(feedback, subject) };
    catch_match_object(|| {
        if unsafe { is_exact_type_name(subject, "str") } {
            return truth_object(false);
        }
        match unsafe { has_mapping_protocol(subject) } {
            Ok(true) => return truth_object(false),
            Ok(false) => {}
            Err(error) => return error,
        }
        match unsafe { has_sequence_protocol(subject) } {
            Ok(matches) => truth_object(matches),
            Err(error) => error,
        }
    })
}

/// Returns boxed true when `subject` is a mapping-pattern candidate.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_match_mapping(subject: *mut PyObject, feedback: *mut FeedbackCell) -> *mut PyObject {
    unsafe { super::record_feedback_unary(feedback, subject) };
    catch_match_object(|| match unsafe { has_mapping_protocol(subject) } {
        Ok(matches) => truth_object(matches),
        Err(error) => error,
    })
}

/// Returns the boxed length used by sequence and mapping patterns.
///
/// The ABI-level `pon_get_len` export is owned by `seq`; keep this Rust-level
/// shim pointed at that implementation so the runtime exposes a single symbol.
pub unsafe extern "C" fn pon_match_get_len(subject: *mut PyObject, feedback: *mut FeedbackCell) -> *mut PyObject {
    unsafe { super::seq::pon_get_len(subject, feedback) }
}

/// Returns boxed true when the subject length satisfies the pattern threshold.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_match_len_ge(subject: *mut PyObject, n: usize, exact: u8) -> *mut PyObject {
    catch_match_object(|| match unsafe { match_len(subject) } {
        Ok(len) => {
            let Ok(len) = usize::try_from(len) else {
                return raise_type_error("match length is negative");
            };
            truth_object(if exact != 0 { len == n } else { len >= n })
        }
        Err(error) => error,
    })
}

/// Extracts mapping values for `keys`, returning `None` when any key is absent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_match_keys(
    subject: *mut PyObject,
    keys: *mut *mut PyObject,
    nkeys: usize,
) -> *mut PyObject {
    catch_match_object(|| {
        if nkeys != 0 && keys.is_null() {
            return raise_type_error("MATCH_KEYS received a NULL key array");
        }
        let mut values = Vec::with_capacity(nkeys);
        for index in 0..nkeys {
            let key = unsafe { *keys.add(index) };
            let Some(value) = (unsafe { mapping_subscript_or_none(subject, key) }) else {
                return unsafe { super::pon_none() };
            };
            values.push(value);
        }
        boxed_match_values(values)
    })
}

/// Extracts class-pattern positional and keyword attributes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_match_class(
    subject: *mut PyObject,
    cls: *mut PyObject,
    nargs: usize,
    kw: *const u32,
    nkw: usize,
) -> *mut PyObject {
    catch_match_object(|| {
        if !unsafe { is_type_object(cls) } {
            return raise_type_error("MATCH_CLASS expected a type object");
        }
        let cls_ty = cls.cast::<PyType>();
        if !unsafe { is_instance_of(subject, cls_ty) } {
            return unsafe { super::pon_none() };
        }
        if nkw != 0 && kw.is_null() {
            return raise_type_error("MATCH_CLASS received a NULL keyword array");
        }

        let mut values = Vec::with_capacity(nargs + nkw);
        if nargs != 0 {
            let match_args_id = intern::intern("__match_args__");
            let match_args = unsafe { abstract_op::get_attr(cls, match_args_id) };
            if match_args.is_null() {
                return raise_type_error("class pattern needs __match_args__ for positional patterns");
            }
            for index in 0..nargs {
                let name_obj = unsafe { read_sequence_item(match_args, index as isize) };
                if name_obj.is_null() {
                    return ptr::null_mut();
                }
                let Some(name_id) = (unsafe { intern_unicode(name_obj) }) else {
                    return raise_type_error("__match_args__ entries must be strings");
                };
                let Some(value) = (unsafe { class_attr_or_none(subject, name_id) }) else {
                    return unsafe { super::pon_none() };
                };
                values.push(value);
            }
        }

        for index in 0..nkw {
            let name = unsafe { *kw.add(index) };
            let Some(value) = (unsafe { class_attr_or_none(subject, name) }) else {
                return unsafe { super::pon_none() };
            };
            values.push(value);
        }

        boxed_match_values(values)
    })
}

/// Implements `assert test, msg` once lowering has evaluated both operands.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_assert(test: *mut PyObject, message: *mut PyObject) -> *mut PyObject {
    catch_match_object(|| match unsafe { abstract_op::is_true(test) } {
        1 => unsafe { super::pon_none() },
        0 => raise_assertion_error(message),
        _ => ptr::null_mut(),
    })
}
