//! Structural pattern-matching helper family namespace.
//!
//! Predicates (`pon_match_sequence`, `pon_match_mapping`, `pon_match_len_ge`)
//! return the boxed `bool` singletons.  Extractors (`pon_match_keys`,
//! `pon_match_class`) return a real tuple of extracted values on success,
//! `None` for a clean non-match, and NULL with a thread-state error otherwise,
//! mirroring CPython's `MATCH_KEYS`/`MATCH_CLASS` result contract.

use core::ptr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::vec::Vec;

use super::exc::{pending_exception_is, pending_exception_object};
use crate::abstract_op;
use crate::descr;
use crate::feedback::FeedbackCell;
use crate::intern;
use crate::mro;
use crate::object::{as_object_ptr, PyFunction, PyObject, PyType, PyUnicode};
use crate::thread_state::{pon_err_clear, pon_err_occurred, pon_err_set_object};
use crate::types::exc::PyBaseException;
use crate::types::{bool_, dict, type_::PyClassDict};

/// Builtin constructor names accepted as class patterns.
///
/// pon exposes builtin classes as named native constructor functions rather
/// than type objects; class patterns resolve them by name, mirroring the
/// runtime-wide `isinstance` convention.
const BUILTIN_CLASS_NAMES: &[&str] = &[
    "bool",
    "bytearray",
    "bytes",
    "dict",
    "float",
    "frozenset",
    "int",
    "list",
    "object",
    "range",
    "set",
    "str",
    "tuple",
];

/// Builtin classes that match a single positional subpattern against the
/// subject itself (CPython `_Py_TPFLAGS_MATCH_SELF` set, minus types pon
/// cannot construct).
const SELF_MATCH_NAMES: &[&str] = &[
    "bool",
    "bytearray",
    "bytes",
    "dict",
    "float",
    "frozenset",
    "int",
    "list",
    "set",
    "str",
    "tuple",
];

/// Type names accepted by sequence patterns (CPython `Py_TPFLAGS_SEQUENCE`
/// carriers representable in pon; str/bytes/bytearray are deliberately not
/// sequence-pattern candidates).
const SEQUENCE_PATTERN_NAMES: &[&str] = &["list", "tuple", "range"];

/// Type names accepted by mapping patterns (CPython `Py_TPFLAGS_MAPPING`).
const MAPPING_PATTERN_NAMES: &[&str] = &["dict"];

fn catch_match_object(f: impl FnOnce() -> *mut PyObject) -> *mut PyObject {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(_) => super::return_null_with_error("match helper panicked"),
    }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    unsafe { super::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn raise_value_error(message: &str) -> *mut PyObject {
    unsafe { super::exc::pon_raise_value_error(message.as_ptr(), message.len()) }
}

fn truth_object(value: bool) -> *mut PyObject {
    bool_::from_bool(value)
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

/// Returns true when the type of `object` (or an MRO ancestor) has one of the
/// given names.  Name-based identity is the runtime-wide convention for
/// builtin classes, which exist as several equivalent static descriptors.
unsafe fn type_name_in(object: *mut PyObject, names: &[&str]) -> bool {
    let Some(ty) = (unsafe { object_type(object) }) else {
        return false;
    };
    unsafe { mro::mro_entries(ty) }
        .iter()
        .any(|entry| !entry.is_null() && names.contains(&unsafe { (**entry).name() }))
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

/// Boxes extracted match values as a real tuple so lowering can consume the
/// result with plain subscript instructions.
fn values_tuple(mut values: Vec<*mut PyObject>) -> *mut PyObject {
    let len = values.len();
    let argv = if len == 0 { ptr::null_mut() } else { values.as_mut_ptr() };
    unsafe { super::seq::pon_build_tuple(argv, len) }
}

unsafe fn intern_unicode(object: *mut PyObject) -> Option<u32> {
    if !unsafe { is_exact_type_name(object, "str") } {
        return None;
    }
    let text = unsafe { (*object.cast::<PyUnicode>()).as_str()? };
    Some(intern::intern(text))
}

/// Fetches `subject.<name>`, distinguishing a clean attribute miss (`None`)
/// from a propagating error (`Err(NULL)`).
///
/// Attribute misses surface either as a diagnostic sentinel (bootstrap
/// descriptors) or a boxed `AttributeError`; both mean "no match" per
/// CPython's `match_class_attr`.  Any other live exception propagates.
unsafe fn subject_attr(subject: *mut PyObject, name: u32) -> Result<Option<*mut PyObject>, *mut PyObject> {
    let value = unsafe { abstract_op::get_attr(subject, name) };
    if !value.is_null() {
        return Ok(Some(value));
    }
    if pending_exception_object().is_none() || pending_exception_is("AttributeError") {
        pon_err_clear();
        return Ok(None);
    }
    Err(ptr::null_mut())
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
///
/// CPython gates sequence patterns on `Py_TPFLAGS_SEQUENCE`: list, tuple, and
/// range qualify while str, bytes, bytearray, dict, set, and iterators do not.
/// pon additionally requires live length/item slots so partially-wired native
/// containers degrade to a non-match instead of failing during extraction.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_match_sequence(subject: *mut PyObject, feedback: *mut FeedbackCell) -> *mut PyObject {
    crate::untag_prelude!(subject);
    unsafe { super::record_feedback_unary(feedback, subject) };
    catch_match_object(|| {
        if !unsafe { type_name_in(subject, SEQUENCE_PATTERN_NAMES) } {
            return truth_object(false);
        }
        let Some(ty) = (unsafe { object_type(subject) }) else {
            return truth_object(false);
        };
        let has_slots = unsafe {
            (*ty)
                .tp_as_sequence
                .as_ref()
                .is_some_and(|methods| methods.sq_length.is_some() && methods.sq_item.is_some())
        };
        truth_object(has_slots)
    })
}

/// Returns boxed true when `subject` is a mapping-pattern candidate.
///
/// CPython gates mapping patterns on `Py_TPFLAGS_MAPPING`; in pon only `dict`
/// carries mapping-pattern semantics.  Live length/subscript slots are also
/// required so partially-wired native dicts degrade to a non-match.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_match_mapping(subject: *mut PyObject, feedback: *mut FeedbackCell) -> *mut PyObject {
    crate::untag_prelude!(subject);
    unsafe { super::record_feedback_unary(feedback, subject) };
    catch_match_object(|| truth_object(unsafe { is_mapping_pattern_candidate(subject) }))
}

unsafe fn is_mapping_pattern_candidate(subject: *mut PyObject) -> bool {
    if !unsafe { type_name_in(subject, MAPPING_PATTERN_NAMES) } {
        return false;
    }
    let Some(ty) = (unsafe { object_type(subject) }) else {
        return false;
    };
    unsafe {
        (*ty)
            .tp_as_mapping
            .as_ref()
            .is_some_and(|methods| methods.mp_length.is_some() && methods.mp_subscript.is_some())
    }
}

/// Returns the boxed length used by sequence and mapping patterns.
///
/// The ABI-level `pon_get_len` export is owned by `seq`; keep this Rust-level
/// shim pointed at that implementation so the runtime exposes a single symbol.
pub unsafe extern "C" fn pon_match_get_len(subject: *mut PyObject, feedback: *mut FeedbackCell) -> *mut PyObject {
    crate::untag_prelude!(subject);
    unsafe { super::seq::pon_get_len(subject, feedback) }
}

/// Returns boxed true when the subject length satisfies the pattern threshold.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_match_len_ge(subject: *mut PyObject, n: usize, exact: u8) -> *mut PyObject {
    crate::untag_prelude!(subject);
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

/// Extracts mapping values for `keys`.
///
/// Returns a tuple of the extracted values in key order, `None` when any key
/// is absent, and NULL with an error for duplicate keys or propagating
/// failures, matching CPython `MATCH_KEYS` semantics.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_match_keys(
    subject: *mut PyObject,
    keys: *mut *mut PyObject,
    nkeys: usize,
) -> *mut PyObject {
    crate::untag_prelude!(subject);
    catch_match_object(|| {
        if nkeys != 0 && keys.is_null() {
            return raise_type_error("MATCH_KEYS received a NULL key array");
        }
        let keys = if nkeys == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(keys, nkeys) }
        };
        for (index, key) in keys.iter().enumerate() {
            for earlier in &keys[..index] {
                match unsafe { dict::object_equal(*earlier, *key) } {
                    Ok(true) => return raise_value_error("mapping pattern checks duplicate key"),
                    Ok(false) => {}
                    Err(message) => return super::return_null_with_error(message),
                }
            }
        }

        let exact_dict = unsafe { dict::is_dict(subject) } && unsafe { is_mapping_pattern_candidate(subject) };
        let mut values = Vec::with_capacity(nkeys);
        for key in keys {
            if exact_dict {
                match unsafe { dict::dict_get(subject, *key) } {
                    Ok(Some(value)) => values.push(value),
                    Ok(None) => return unsafe { super::pon_none() },
                    Err(message) => return super::return_null_with_error(message),
                }
            } else {
                let value = unsafe { abstract_op::subscript_get(subject, *key) };
                if !value.is_null() {
                    values.push(value);
                    continue;
                }
                if pending_exception_object().is_none() || pending_exception_is("KeyError") {
                    pon_err_clear();
                    return unsafe { super::pon_none() };
                }
                return ptr::null_mut();
            }
        }
        values_tuple(values)
    })
}

/// Class-pattern callee classification.
enum ClassRef {
    /// A real runtime type object (user class or builtin exception type).
    Type(*mut PyType),
    /// A builtin constructor function standing in for a builtin class.
    Builtin(String),
}

unsafe fn classify_class_object(cls: *mut PyObject) -> Option<ClassRef> {
    let ty = unsafe { object_type(cls) }?;
    let ty_name = unsafe { (*ty).name() };
    if ty_name == "type" {
        let name = unsafe { (*cls.cast::<PyType>()).name() };
        if BUILTIN_CLASS_NAMES.contains(&name) {
            return Some(ClassRef::Builtin(name.to_owned()));
        }
        return Some(ClassRef::Type(cls.cast::<PyType>()));
    }
    if ty_name == "function" {
        let name = intern::resolve(unsafe { (*cls.cast::<PyFunction>()).name_interned })?;
        if BUILTIN_CLASS_NAMES.contains(&name.as_str()) {
            return Some(ClassRef::Builtin(name));
        }
    }
    None
}

/// Name-based `isinstance` for builtin constructor classes, mirroring the
/// runtime `isinstance` builtin (plus `bool <: int` numeric-tower widening).
unsafe fn builtin_isinstance(subject: *mut PyObject, class_name: &str) -> bool {
    if subject.is_null() {
        return false;
    }
    if class_name == "object" {
        return true;
    }
    let Some(ty) = (unsafe { object_type(subject) }) else {
        return false;
    };
    let subject_name = unsafe { (*ty).name() };
    subject_name == class_name || (subject_name == "bool" && class_name == "int")
}

/// Reads `__match_args__` from a type's MRO, bypassing instance attribute
/// machinery the way CPython reads it directly off the type.
unsafe fn class_match_args(ty: *mut PyType) -> Option<*mut PyObject> {
    let name_id = intern::intern("__match_args__");
    for entry in unsafe { mro::mro_entries(ty) } {
        if entry.is_null() {
            continue;
        }
        let dict = unsafe { (*entry).tp_dict };
        if dict.is_null() {
            continue;
        }
        let namespace = unsafe { &*dict.cast::<PyClassDict>() };
        if let Some(value) = namespace.get(name_id) {
            return Some(value);
        }
    }
    None
}

/// Extracts class-pattern positional and keyword attributes.
///
/// Returns a tuple of extracted values (positionals first), `None` for a
/// clean non-match, and NULL with an error otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_match_class(
    subject: *mut PyObject,
    cls: *mut PyObject,
    nargs: usize,
    kw: *const u32,
    nkw: usize,
) -> *mut PyObject {
    crate::untag_prelude!(subject, cls);
    catch_match_object(|| {
        if nkw != 0 && kw.is_null() {
            return raise_type_error("MATCH_CLASS received a NULL keyword array");
        }
        let keywords = if nkw == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(kw, nkw) }
        };
        let Some(class_ref) = (unsafe { classify_class_object(cls) }) else {
            return raise_type_error("called match pattern must be a class");
        };

        let mut seen: Vec<u32> = Vec::with_capacity(nargs + nkw);
        let mut values = Vec::with_capacity(nargs + nkw);
        match class_ref {
            ClassRef::Type(cls_ty) => {
                match unsafe { descr::isinstance(subject, cls_ty.cast::<PyObject>()) } {
                    1 => {}
                    0 => return unsafe { super::pon_none() },
                    _ => return ptr::null_mut(),
                }
                if nargs != 0 {
                    let Some(match_args) = (unsafe { class_match_args(cls_ty) }) else {
                        let name = unsafe { (*cls_ty).name() };
                        return raise_type_error(&format!("{name}() accepts 0 positional sub-patterns ({nargs} given)"));
                    };
                    if !unsafe { is_exact_type_name(match_args, "tuple") } {
                        return raise_type_error("__match_args__ must be a tuple");
                    }
                    let Ok(available) = (match unsafe { sequence_len(match_args) } {
                        Ok(Some(len)) => usize::try_from(len),
                        Ok(None) => return raise_type_error("__match_args__ must be a tuple"),
                        Err(error) => return error,
                    }) else {
                        return raise_type_error("__match_args__ length is negative");
                    };
                    if nargs > available {
                        let name = unsafe { (*cls_ty).name() };
                        return raise_type_error(&format!(
                            "{name}() accepts {available} positional sub-patterns ({nargs} given)"
                        ));
                    }
                    for index in 0..nargs {
                        let name_obj = unsafe { read_sequence_item(match_args, index as isize) };
                        if name_obj.is_null() {
                            return ptr::null_mut();
                        }
                        let Some(name_id) = (unsafe { intern_unicode(name_obj) }) else {
                            return raise_type_error("__match_args__ elements must be strings");
                        };
                        seen.push(name_id);
                        match unsafe { subject_attr(subject, name_id) } {
                            Ok(Some(value)) => values.push(value),
                            Ok(None) => return unsafe { super::pon_none() },
                            Err(error) => return error,
                        }
                    }
                }
            }
            ClassRef::Builtin(class_name) => {
                if !unsafe { builtin_isinstance(subject, &class_name) } {
                    return unsafe { super::pon_none() };
                }
                if nargs != 0 {
                    if SELF_MATCH_NAMES.contains(&class_name.as_str()) {
                        if nargs > 1 {
                            return raise_type_error(&format!(
                                "{class_name}() accepts 1 positional sub-pattern ({nargs} given)"
                            ));
                        }
                        values.push(subject);
                    } else {
                        return raise_type_error(&format!(
                            "{class_name}() accepts 0 positional sub-patterns ({nargs} given)"
                        ));
                    }
                }
            }
        }

        for name in keywords {
            if seen.contains(name) {
                let spelling = intern::resolve(*name).unwrap_or_else(|| format!("<interned:{name}>"));
                return raise_type_error(&format!("class pattern got multiple sub-patterns for attribute '{spelling}'"));
            }
            seen.push(*name);
            match unsafe { subject_attr(subject, *name) } {
                Ok(Some(value)) => values.push(value),
                Ok(None) => return unsafe { super::pon_none() },
                Err(error) => return error,
            }
        }

        values_tuple(values)
    })
}

/// Implements `assert test, msg` once lowering has evaluated both operands.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_assert(test: *mut PyObject, message: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(test, message);
    catch_match_object(|| match unsafe { abstract_op::is_true(test) } {
        1 => unsafe { super::pon_none() },
        0 => raise_assertion_error(message),
        _ => ptr::null_mut(),
    })
}
