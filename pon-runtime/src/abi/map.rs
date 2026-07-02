//! Mapping and set helper family namespace.
//!
//! WS-MAP owns concrete dict/set/frozenset helpers. They follow the runtime-wide
//! sentinel discipline: object helpers return NULL with a thread-state error and
//! status/predicate helpers return `-1` with a thread-state error.

use core::{ffi::c_int, mem, ptr};
use std::panic::{catch_unwind, AssertUnwindSafe};

use pon_gc::{GcTypeInfo, TypeId};

use crate::object::{as_object_ptr, PyLong, PyObject, PyType, PyUnicode};
use crate::thread_state::pon_err_set;
use crate::types::{dict, frozenset, method, set_};

/// Mapping/set status helpers return `0` on success and `-1` on error.
pub type MapStatus = i32;

const TYPE_ID_DICT: TypeId = TypeId(101);
const TYPE_ID_DICT_ITER: TypeId = TypeId(102);
const TYPE_ID_DICT_ITEM: TypeId = TypeId(103);
const TYPE_ID_SET: TypeId = TypeId(104);
const TYPE_ID_SET_ITER: TypeId = TypeId(105);
const TYPE_ID_FROZENSET: TypeId = TypeId(106);

fn register_map_types(runtime: &super::Runtime) {
    runtime.heap.register_type(
        TYPE_ID_DICT,
        GcTypeInfo {
            size: mem::size_of::<dict::PyDict>(),
            trace: dict::trace_dict,
            finalize: Some(dict::finalize_dict),
        },
    );
    runtime.heap.register_type(
        TYPE_ID_DICT_ITER,
        GcTypeInfo {
            size: mem::size_of::<dict::PyDictIter>(),
            trace: dict::trace_dict_iter,
            finalize: None,
        },
    );
    runtime.heap.register_type(
        TYPE_ID_DICT_ITEM,
        GcTypeInfo {
            size: mem::size_of::<dict::PyDictItem>(),
            trace: dict::trace_dict_item,
            finalize: None,
        },
    );
    runtime.heap.register_type(
        TYPE_ID_SET,
        GcTypeInfo {
            size: mem::size_of::<set_::PySet>(),
            trace: set_::trace_set,
            finalize: Some(set_::finalize_set),
        },
    );
    runtime.heap.register_type(
        TYPE_ID_SET_ITER,
        GcTypeInfo {
            size: mem::size_of::<set_::PySetIter>(),
            trace: set_::trace_set_iter,
            finalize: None,
        },
    );
    runtime.heap.register_type(
        TYPE_ID_FROZENSET,
        GcTypeInfo {
            size: mem::size_of::<frozenset::PyFrozenSet>(),
            trace: frozenset::trace_frozenset,
            finalize: Some(frozenset::finalize_frozenset),
        },
    );
}

fn ensure_runtime_for_map() -> Result<(), String> {
    super::ensure_runtime_initialized()
}

fn null_error(message: impl Into<String>) -> *mut PyObject {
    super::return_null_with_error(message)
}

fn duplicate_keyword_error(key: *mut PyObject) -> *mut PyObject {
    if unsafe { dict::type_name(key) } != Some("str") {
        return null_error("keywords must be strings");
    }
    let Some(name) = (unsafe { (&*key.cast::<PyUnicode>()).as_str() }) else {
        return null_error("keyword name is not valid UTF-8");
    };
    null_error(format!("got multiple values for keyword argument '{name}'"))
}

fn status_error(message: impl Into<String>) -> c_int {
    super::return_minus_one_with_error(message)
}

fn raise_key_error(key: *mut PyObject) -> *mut PyObject {
    unsafe { super::exc::pon_raise_key_error(key) }
}

fn raise_stop_iteration() -> *mut PyObject {
    unsafe { super::exc::pon_raise_stop_iteration(ptr::null_mut()) }
}

fn alloc_dict(runtime: &super::Runtime, capacity: usize) -> *mut PyObject {
    register_map_types(runtime);
    let object = runtime.heap.alloc(mem::size_of::<dict::PyDict>(), TYPE_ID_DICT).cast::<dict::PyDict>();
    unsafe { dict::init_dict(object, dict::dict_type(runtime._type_type), capacity) };
    as_object_ptr(object)
}

fn alloc_dict_iter(runtime: &super::Runtime, source: *mut PyObject, kind: dict::DictIterKind) -> *mut PyObject {
    register_map_types(runtime);
    let object = runtime
        .heap
        .alloc(mem::size_of::<dict::PyDictIter>(), TYPE_ID_DICT_ITER)
        .cast::<dict::PyDictIter>();
    unsafe { dict::init_dict_iter(object, dict::dict_iter_type(runtime._type_type), source, kind) };
    as_object_ptr(object)
}

fn alloc_dict_item(runtime: &super::Runtime, key: *mut PyObject, value: *mut PyObject) -> *mut PyObject {
    register_map_types(runtime);
    let object = runtime
        .heap
        .alloc(mem::size_of::<dict::PyDictItem>(), TYPE_ID_DICT_ITEM)
        .cast::<dict::PyDictItem>();
    unsafe { dict::init_dict_item(object, dict::dict_item_type(runtime._type_type), key, value) };
    as_object_ptr(object)
}

fn alloc_set(runtime: &super::Runtime, capacity: usize) -> *mut PyObject {
    register_map_types(runtime);
    let object = runtime.heap.alloc(mem::size_of::<set_::PySet>(), TYPE_ID_SET).cast::<set_::PySet>();
    unsafe { set_::init_set(object, set_::set_type(runtime._type_type), capacity) };
    as_object_ptr(object)
}

fn alloc_set_iter(runtime: &super::Runtime, source: *mut PyObject) -> *mut PyObject {
    register_map_types(runtime);
    let object = runtime
        .heap
        .alloc(mem::size_of::<set_::PySetIter>(), TYPE_ID_SET_ITER)
        .cast::<set_::PySetIter>();
    unsafe { set_::init_set_iter(object, set_::set_iter_type(runtime._type_type), source) };
    as_object_ptr(object)
}

fn alloc_frozenset(runtime: &super::Runtime, entries: Vec<*mut PyObject>) -> *mut PyObject {
    register_map_types(runtime);
    let object = runtime
        .heap
        .alloc(mem::size_of::<frozenset::PyFrozenSet>(), TYPE_ID_FROZENSET)
        .cast::<frozenset::PyFrozenSet>();
    unsafe { frozenset::init_frozenset(object, frozenset::frozenset_type(runtime._type_type), entries) };
    as_object_ptr(object)
}

fn none_object(runtime: &super::Runtime) -> *mut PyObject {
    as_object_ptr(runtime.none)
}

fn build_set_from_entries(runtime: &super::Runtime, left: *mut PyObject, entries: Vec<*mut PyObject>) -> *mut PyObject {
    if unsafe { frozenset::is_frozenset(left) } {
        alloc_frozenset(runtime, entries)
    } else {
        let result = alloc_set(runtime, entries.len());
        // SAFETY: The freshly allocated object is an exact set.
        unsafe { set_::set_mut(result).expect("fresh set").entries = entries };
        result
    }
}

unsafe fn object_array<'a>(items: *mut *mut PyObject, count: usize) -> Result<&'a [*mut PyObject], String> {
    if items.is_null() && count != 0 {
        return Err("object array pointer is NULL".to_owned());
    }
    Ok(if count == 0 {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(items, count) }
    })
}

/// Builds an insertion-ordered dict from `pair_count` key/value pairs stored as
/// `[key0, value0, key1, value1, ...]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_build_map(items: *mut *mut PyObject, pair_count: usize) -> *mut PyObject {
    super::catch_object_helper(|| {
        if let Err(message) = ensure_runtime_for_map() {
            return null_error(message);
        }
        let Some(array_len) = pair_count.checked_mul(2) else {
            return null_error("dict item count overflow");
        };
        let pairs = match unsafe { object_array(items, array_len) } {
            Ok(pairs) => pairs,
            Err(message) => return null_error(message),
        };
        let result: Option<Result<*mut PyObject, String>> = super::with_runtime(|runtime| {
            let dict_obj = alloc_dict(runtime, pair_count);
            for pair in pairs.chunks_exact(2) {
                unsafe { dict::dict_insert(dict_obj, pair[0], pair[1])? };
            }
            Ok(dict_obj)
        });
        match result {
            Some(Ok(object)) => object,
            Some(Err(message)) => null_error(message),
            None => null_error("runtime is not initialized"),
        }
    })
}

/// Builds a mutable set from `count` elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_build_set(items: *mut *mut PyObject, count: usize) -> *mut PyObject {
    super::catch_object_helper(|| {
        if let Err(message) = ensure_runtime_for_map() {
            return null_error(message);
        }
        let values = match unsafe { object_array(items, count) } {
            Ok(values) => values,
            Err(message) => return null_error(message),
        };
        let result: Option<Result<*mut PyObject, String>> = super::with_runtime(|runtime| {
            let set_obj = alloc_set(runtime, count);
            for value in values {
                unsafe { set_::set_add(set_obj, *value)? };
            }
            Ok(set_obj)
        });
        match result {
            Some(Ok(object)) => object,
            Some(Err(message)) => null_error(message),
            None => null_error("runtime is not initialized"),
        }
    })
}

/// Builds a frozenset from `count` elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_build_frozenset(items: *mut *mut PyObject, count: usize) -> *mut PyObject {
    super::catch_object_helper(|| {
        if let Err(message) = ensure_runtime_for_map() {
            return null_error(message);
        }
        let values = match unsafe { object_array(items, count) } {
            Ok(values) => values,
            Err(message) => return null_error(message),
        };
        let entries = match unsafe { frozenset::unique_entries(values) } {
            Ok(entries) => entries,
            Err(message) => return null_error(message),
        };
        match super::with_runtime(|runtime| alloc_frozenset(runtime, entries)) {
            Some(object) => object,
            None => null_error("runtime is not initialized"),
        }
    })
}

/// Inserts a key/value pair into a dict and returns the dict.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_map_insert(map: *mut PyObject, key: *mut PyObject, value: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(map, key, value);
    super::catch_object_helper(|| {
        let _guard = crate::sync::begin_critical_section(map);
        match unsafe { dict::dict_insert(map, key, value) } {
            Ok(()) => map,
            Err(message) => null_error(message),
        }
    })
}

/// Status form used by mapping slots.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_set_item_status(map: *mut PyObject, key: *mut PyObject, value: *mut PyObject) -> c_int {
    crate::untag_prelude!(err = -1; map, key, value);
    super::catch_status_helper(|| {
        let _guard = crate::sync::begin_critical_section(map);
        match unsafe { dict::dict_insert(map, key, value) } {
            Ok(()) => {
                crate::dynexec::sync_globals_dict_set(map, key, value);
                0
            }
            Err(message) => status_error(message),
        }
    })
}

/// Fetches `map[key]`, raising KeyError on a miss.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_get_item(map: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(map, key);
    super::catch_object_helper(|| {
        let _guard = crate::sync::begin_critical_section(map);
        match unsafe { dict::dict_get(map, key) } {
            Ok(Some(value)) => value,
            Ok(None) => raise_key_error(key),
            Err(message) => null_error(message),
        }
    })
}

/// Deletes `map[key]` and returns None.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_subscript_del(object: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(object, key);
    super::catch_object_helper(|| {
        if unsafe { dict::is_dict(object) } {
            let _guard = crate::sync::begin_critical_section(object);
            match unsafe { dict::dict_remove(object, key) } {
                Ok(Some(_)) => {
                    crate::dynexec::sync_globals_dict_delete(object, key);
                    unsafe { super::pon_none() }
                }
                Ok(None) => raise_key_error(key),
                Err(message) => null_error(message),
            }
        } else {
            unsafe { crate::abstract_op::subscript_del(object, key) }
        }
    })
}
/// Stores `object[key] = value` and returns `value`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_subscript_set(object: *mut PyObject, key: *mut PyObject, value: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(object, key, value);
    super::catch_object_helper(|| {
        if unsafe { dict::is_dict(object) } {
            let _guard = crate::sync::begin_critical_section(object);
            match unsafe { dict::dict_insert(object, key, value) } {
                Ok(()) => {
                    crate::dynexec::sync_globals_dict_set(object, key, value);
                    value
                }
                Err(message) => null_error(message),
            }
        } else {
            if object.is_null() {
                return null_error("subscription receiver is NULL or has no type");
            }
            let ty = unsafe { (*object).ob_type.cast_mut() };
            if let Some(slot) = unsafe { (*ty).tp_as_mapping.as_ref().and_then(|methods| methods.mp_ass_subscript) } {
                if unsafe { slot(object, key, value) } < 0 {
                    ptr::null_mut()
                } else {
                    value
                }
            } else if let Some(slot) = unsafe { (*ty).tp_as_sequence.as_ref().and_then(|methods| methods.sq_ass_item) } {
                if unsafe { dict::type_name(key) } != Some("int") {
                    return null_error("sequence index must be an int");
                }
                let index = unsafe { (*key.cast::<PyLong>()).value };
                let Ok(index) = isize::try_from(index) else {
                    return null_error("sequence index is out of range for this platform");
                };
                if unsafe { slot(object, index, value) } < 0 {
                    ptr::null_mut()
                } else {
                    value
                }
            } else {
                null_error("object does not support item assignment")
            }
        }
    })
}
/// Status form used by mapping slots.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_del_item_status(map: *mut PyObject, key: *mut PyObject) -> c_int {
    crate::untag_prelude!(err = -1; map, key);
    super::catch_status_helper(|| {
        let _guard = crate::sync::begin_critical_section(map);
        match unsafe { dict::dict_remove(map, key) } {
            Ok(Some(_)) => {
                crate::dynexec::sync_globals_dict_delete(map, key);
                0
            }
            Ok(None) => {
                let _ = raise_key_error(key);
                -1
            }
            Err(message) => status_error(message),
        }
    })
}

/// Merges another exact dict into `map` and returns `map`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_merge(map: *mut PyObject, other: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(map, other);
    super::catch_object_helper(|| {
        let _guard = crate::sync::begin_critical_section2(map, other);
        match unsafe { dict::dict_merge_exact(map, other) } {
            Ok(()) => map,
            Err(message) => null_error(message),
        }
    })
}

/// Merges exact-dict entries from `other` into `map`, rejecting duplicates for call kwargs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_merge_unique(map: *mut PyObject, other: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(map, other);
    super::catch_object_helper(|| {
        let _guard = crate::sync::begin_critical_section2(map, other);
        let entries = match unsafe { dict::dict_entries_snapshot(other) } {
            Ok(entries) => entries,
            Err(message) => return null_error(message),
        };
        for entry in entries {
            match unsafe { dict::dict_contains(map, entry.key) } {
                Ok(true) => return duplicate_keyword_error(entry.key),
                Ok(false) => {}
                Err(message) => return null_error(message),
            }
            if let Err(message) = unsafe { dict::dict_insert(map, entry.key, entry.value) } {
                return null_error(message);
            }
        }
        map
    })
}

/// `dict.update` exact-dict helper. Returns the receiver.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_update(map: *mut PyObject, other: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(map, other);
    unsafe { pon_dict_merge(map, other) }
}

/// `dict.get(key, default=None)` helper.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_get_method(map: *mut PyObject, key: *mut PyObject, default: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(map, key, default);
    super::catch_object_helper(|| {
        let fallback = match super::with_runtime(|runtime| if default.is_null() { none_object(runtime) } else { default }) {
            Some(value) => value,
            None => return null_error("runtime is not initialized"),
        };
        match unsafe { dict::dict_get(map, key) } {
            Ok(Some(value)) => value,
            Ok(None) => fallback,
            Err(message) => null_error(message),
        }
    })
}

fn map_method_args<'a>(argv: *mut *mut PyObject, argc: usize, name: &str) -> Result<&'a [*mut PyObject], String> {
    if argv.is_null() && argc != 0 {
        return Err(format!("{name} received a NULL argv pointer"));
    }
    Ok(if argc == 0 { &[] } else { unsafe { core::slice::from_raw_parts(argv, argc) } })
}

fn map_none() -> *mut PyObject {
    match super::with_runtime(|runtime| none_object(runtime)) {
        Some(none) => none,
        None => null_error("runtime is not initialized"),
    }
}

fn alloc_bound_native_method(
    receiver: *mut PyObject,
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> *mut PyObject {
    if let Err(message) = ensure_runtime_for_map() {
        return null_error(message);
    }
    let function = match super::with_runtime(|runtime| {
        super::alloc_function(
            runtime,
            entry as *const () as *const u8,
            crate::builtins::variadic_arity(),
            crate::intern::intern(name),
        )
    }) {
        Some(Ok(function)) => function,
        Some(Err(message)) => return null_error(message),
        None => return null_error("runtime is not initialized"),
    };
    match method::new_bound_method(function, receiver) {
        Ok(bound) => bound.cast::<PyObject>(),
        Err(message) => null_error(message),
    }
}

unsafe extern "C" fn dict_get_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    super::catch_object_helper(|| {
        let args = match map_method_args(argv, argc, "dict.get") {
            Ok(args) => args,
            Err(message) => return null_error(message),
        };
        if !(args.len() == 2 || args.len() == 3) {
            return null_error(format!("dict.get() expected 1 or 2 arguments, got {}", args.len().saturating_sub(1)));
        }
        let default = args.get(2).copied().unwrap_or(ptr::null_mut());
        unsafe { pon_dict_get_method(args[0], args[1], default) }
    })
}

unsafe extern "C" fn dict_keys_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "dict.keys") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 1 {
        return null_error(format!("dict.keys() expected 0 arguments, got {}", args.len().saturating_sub(1)));
    }
    unsafe { pon_dict_keys(args[0]) }
}

unsafe extern "C" fn dict_values_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "dict.values") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 1 {
        return null_error(format!("dict.values() expected 0 arguments, got {}", args.len().saturating_sub(1)));
    }
    unsafe { pon_dict_values(args[0]) }
}

unsafe extern "C" fn dict_items_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "dict.items") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 1 {
        return null_error(format!("dict.items() expected 0 arguments, got {}", args.len().saturating_sub(1)));
    }
    unsafe { pon_dict_items(args[0]) }
}

unsafe extern "C" fn dict_setdefault_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "dict.setdefault") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if !(args.len() == 2 || args.len() == 3) {
        return null_error(format!("dict.setdefault() expected 1 or 2 arguments, got {}", args.len().saturating_sub(1)));
    }
    let default = args.get(2).copied().unwrap_or(ptr::null_mut());
    unsafe { pon_dict_setdefault(args[0], args[1], default) }
}

unsafe extern "C" fn dict_pop_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "dict.pop") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if !(args.len() == 2 || args.len() == 3) {
        return null_error(format!("dict.pop() expected 1 or 2 arguments, got {}", args.len().saturating_sub(1)));
    }
    let default = args.get(2).copied().unwrap_or(ptr::null_mut());
    unsafe { pon_dict_pop(args[0], args[1], default) }
}

unsafe extern "C" fn dict_update_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "dict.update") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 2 {
        return null_error(format!("dict.update() expected 1 argument, got {}", args.len().saturating_sub(1)));
    }
    let updated = unsafe { pon_dict_update(args[0], args[1]) };
    if updated.is_null() {
        updated
    } else {
        map_none()
    }
}

/// Returns a bound dict method object for attribute lookup.
pub unsafe fn pon_dict_bound_method(map: *mut PyObject, name: &str) -> *mut PyObject {
    match name {
        "get" => alloc_bound_native_method(map, name, dict_get_method_trampoline),
        "keys" => alloc_bound_native_method(map, name, dict_keys_method_trampoline),
        "values" => alloc_bound_native_method(map, name, dict_values_method_trampoline),
        "items" => alloc_bound_native_method(map, name, dict_items_method_trampoline),
        "setdefault" => alloc_bound_native_method(map, name, dict_setdefault_method_trampoline),
        "pop" => alloc_bound_native_method(map, name, dict_pop_method_trampoline),
        "update" => alloc_bound_native_method(map, name, dict_update_method_trampoline),
        _ => null_error(format!("attribute '{name}' was not found")),
    }
}

/// Returns a bound `dict.get` method object for attribute lookup.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_get_bound_method(map: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(map);
    unsafe { pon_dict_bound_method(map, "get") }
}

/// `dict.setdefault(key, default=None)` helper.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_setdefault(map: *mut PyObject, key: *mut PyObject, default: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(map, key, default);
    super::catch_object_helper(|| {
        let value = match super::with_runtime(|runtime| if default.is_null() { none_object(runtime) } else { default }) {
            Some(value) => value,
            None => return null_error("runtime is not initialized"),
        };
        let _guard = crate::sync::begin_critical_section(map);
        match unsafe { dict::dict_get(map, key) } {
            Ok(Some(existing)) => existing,
            Ok(None) => match unsafe { dict::dict_insert(map, key, value) } {
                Ok(()) => value,
                Err(message) => null_error(message),
            },
            Err(message) => null_error(message),
        }
    })
}

/// `dict.pop(key[, default])` helper. A NULL `default` means no default was supplied.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_pop(map: *mut PyObject, key: *mut PyObject, default: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(map, key, default);
    super::catch_object_helper(|| {
        let _guard = crate::sync::begin_critical_section(map);
        match unsafe { dict::dict_remove(map, key) } {
            Ok(Some(value)) => value,
            Ok(None) if !default.is_null() => default,
            Ok(None) => raise_key_error(key),
            Err(message) => null_error(message),
        }
    })
}
/// Returns an insertion-order key iterator for a dict.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_keys(map: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(map);
    unsafe { pon_dict_iter_keys(map) }
}

/// Returns an insertion-order value iterator for a dict.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_values(map: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(map);
    super::catch_object_helper(|| match super::with_runtime(|runtime| {
        register_map_types(runtime);
        alloc_dict_iter(runtime, map, dict::DictIterKind::Values)
    }) {
        Some(iter) => iter,
        None => null_error("runtime is not initialized"),
    })
}

/// Returns an insertion-order item iterator for a dict.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_items(map: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(map);
    super::catch_object_helper(|| match super::with_runtime(|runtime| {
        register_map_types(runtime);
        alloc_dict_iter(runtime, map, dict::DictIterKind::Items)
    }) {
        Some(iter) => iter,
        None => null_error("runtime is not initialized"),
    })
}

/// Returns an insertion-order key iterator for a dict.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_iter_keys(map: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(map);
    super::catch_object_helper(|| {
        if unsafe { !dict::is_dict(map) } {
            return null_error("expected dict object");
        }
        match super::with_runtime(|runtime| {
            register_map_types(runtime);
            alloc_dict_iter(runtime, map, dict::DictIterKind::Keys)
        }) {
            Some(iter) => iter,
            None => null_error("runtime is not initialized"),
        }
    })
}

/// Advances a dictionary iterator.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_iter_next(iterator: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(iterator);
    super::catch_object_helper(|| {
        if unsafe { !dict::is_dict_iter(iterator) } {
            return null_error("expected dict iterator object");
        }
        let dict = unsafe { (*iterator.cast::<dict::PyDictIter>()).dict };
        let _guard = crate::sync::begin_critical_section2(iterator, dict);
        let iter = unsafe { &mut *iterator.cast::<dict::PyDictIter>() };
        let entries = match unsafe { dict::dict_entries_snapshot(iter.dict) } {
            Ok(entries) => entries,
            Err(message) => return null_error(message),
        };
        let Some(entry) = entries.get(iter.index).copied() else {
            return raise_stop_iteration();
        };
        iter.index += 1;
        match iter.kind {
            dict::DictIterKind::Keys => entry.key,
            dict::DictIterKind::Values => entry.value,
            dict::DictIterKind::Items => crate::native::builtins_mod::alloc_tuple(vec![entry.key, entry.value]),
        }
    })
}

/// Adds an element to a set and returns the receiver.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_set_add(set: *mut PyObject, item: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(set, item);
    super::catch_object_helper(|| {
        let _guard = crate::sync::begin_critical_section(set);
        match unsafe { set_::set_add(set, item) } {
            Ok(()) => set,
            Err(message) => null_error(message),
        }
    })
}

/// Discards an element from a set and returns None.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_set_discard(set: *mut PyObject, item: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(set, item);
    super::catch_object_helper(|| {
        let _guard = crate::sync::begin_critical_section(set);
        match unsafe { set_::set_discard(set, item) } {
            Ok(()) => map_none(),
            Err(message) => null_error(message),
        }
    })
}

/// Contains predicate for sequence/dict/set/frozenset. Returns `1`, `0`, or `-1` on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_contains(container: *mut PyObject, item: *mut PyObject) -> c_int {
    crate::untag_prelude!(err = -1; container, item);
    super::catch_status_helper(|| {
        let _guard = crate::sync::begin_critical_section(container);
        if unsafe { dict::is_dict(container) } {
            return contains_result(unsafe { dict::dict_contains(container, item) });
        }
        if unsafe { set_::is_any_set(container) } {
            return contains_result(unsafe { set_::set_contains(container, item) });
        }
        let Some(slot) = (unsafe { sequence_contains_slot(container) }) else {
            return status_error("object does not support containment");
        };
        let status = unsafe { slot(container, item) };
        if status < 0 {
            -1
        } else if status == 0 {
            0
        } else {
            1
        }
    })
}

fn contains_result(result: Result<bool, String>) -> c_int {
    match result {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(message) => status_error(message),
    }
}

unsafe fn sequence_contains_slot(
    object: *mut PyObject,
) -> Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int> {
    if object.is_null() {
        return None;
    }
    let ty = unsafe { (*object).ob_type.cast_mut() };
    if ty.is_null() {
        return None;
    }
    unsafe { (*ty.cast::<PyType>()).tp_as_sequence.as_ref().and_then(|methods| methods.sq_contains) }
}

/// Returns an iterator over a set/frozenset.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_set_iter(set: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(set);
    super::catch_object_helper(|| {
        if unsafe { !set_::is_any_set(set) } {
            return null_error("expected set or frozenset object");
        }
        match super::with_runtime(|runtime| alloc_set_iter(runtime, set)) {
            Some(iter) => iter,
            None => null_error("runtime is not initialized"),
        }
    })
}

/// Advances a set/frozenset iterator.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_set_iter_next(iterator: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(iterator);
    super::catch_object_helper(|| {
        if !matches!(unsafe { dict::type_name(iterator) }, Some("set_iterator")) {
            return null_error("expected set iterator object");
        }
        let set = unsafe { (*iterator.cast::<set_::PySetIter>()).set };
        let _guard = crate::sync::begin_critical_section2(iterator, set);
        let iter = unsafe { &mut *iterator.cast::<set_::PySetIter>() };
        let entries = match unsafe { set_::entries_snapshot(iter.set) } {
            Ok(entries) => entries,
            Err(message) => return null_error(message),
        };
        let Some(value) = entries.get(iter.index).copied() else {
            return raise_stop_iteration();
        };
        iter.index += 1;
        value
    })
}

/// Returns `left | right` for set/frozenset operands.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_set_union(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(left, right);
    super::catch_object_helper(|| {
        let _guard = crate::sync::begin_critical_section2(left, right);
        let left_entries = match unsafe { set_::entries_snapshot(left) } {
            Ok(entries) => entries,
            Err(message) => return null_error(message),
        };
        let right_entries = match unsafe { set_::entries_snapshot(right) } {
            Ok(entries) => entries,
            Err(message) => return null_error(message),
        };
        let mut entries = left_entries;
        if let Err(message) = unsafe { set_::insert_unique_entries(&mut entries, &right_entries) } {
            return null_error(message);
        }
        match super::with_runtime(|runtime| build_set_from_entries(runtime, left, entries)) {
            Some(object) => object,
            None => null_error("runtime is not initialized"),
        }
    })
}

/// Returns `left & right` for set/frozenset operands.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_set_intersection(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(left, right);
    super::catch_object_helper(|| {
        let _guard = crate::sync::begin_critical_section2(left, right);
        let left_entries = match unsafe { set_::entries_snapshot(left) } {
            Ok(entries) => entries,
            Err(message) => return null_error(message),
        };
        let right_entries = match unsafe { set_::entries_snapshot(right) } {
            Ok(entries) => entries,
            Err(message) => return null_error(message),
        };
        let mut entries = Vec::new();
        for item in left_entries {
            match unsafe { set_::find_element_index(&right_entries, item) } {
                Ok(Some(_)) => entries.push(item),
                Ok(None) => {}
                Err(message) => return null_error(message),
            }
        }
        match super::with_runtime(|runtime| build_set_from_entries(runtime, left, entries)) {
            Some(object) => object,
            None => null_error("runtime is not initialized"),
        }
    })
}

/// Returns `left - right` for set/frozenset operands.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_set_difference(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(left, right);
    super::catch_object_helper(|| {
        let _guard = crate::sync::begin_critical_section2(left, right);
        let left_entries = match unsafe { set_::entries_snapshot(left) } {
            Ok(entries) => entries,
            Err(message) => return null_error(message),
        };
        let right_entries = match unsafe { set_::entries_snapshot(right) } {
            Ok(entries) => entries,
            Err(message) => return null_error(message),
        };
        let mut entries = Vec::new();
        for item in left_entries {
            match unsafe { set_::find_element_index(&right_entries, item) } {
                Ok(Some(_)) => {}
                Ok(None) => entries.push(item),
                Err(message) => return null_error(message),
            }
        }
        match super::with_runtime(|runtime| build_set_from_entries(runtime, left, entries)) {
            Some(object) => object,
            None => null_error("runtime is not initialized"),
        }
    })
}

fn set_is_subset(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    let left_entries = match unsafe { set_::entries_snapshot(left) } {
        Ok(entries) => entries,
        Err(message) => return null_error(message),
    };
    let right_entries = match unsafe { set_::entries_snapshot(right) } {
        Ok(entries) => entries,
        Err(message) => return null_error(message),
    };
    for item in left_entries {
        match unsafe { set_::find_element_index(&right_entries, item) } {
            Ok(Some(_)) => {}
            Ok(None) => return unsafe { super::number::pon_const_bool(0) },
            Err(message) => return null_error(message),
        }
    }
    unsafe { super::number::pon_const_bool(1) }
}

unsafe extern "C" fn set_add_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "set.add") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 2 {
        return null_error(format!("set.add() expected 1 argument, got {}", args.len().saturating_sub(1)));
    }
    let result = unsafe { pon_set_add(args[0], args[1]) };
    if result.is_null() { result } else { map_none() }
}

unsafe extern "C" fn set_discard_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "set.discard") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 2 {
        return null_error(format!("set.discard() expected 1 argument, got {}", args.len().saturating_sub(1)));
    }
    unsafe { pon_set_discard(args[0], args[1]) }
}

unsafe extern "C" fn set_union_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "set.union") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 2 {
        return null_error(format!("set.union() expected 1 argument, got {}", args.len().saturating_sub(1)));
    }
    unsafe { pon_set_union(args[0], args[1]) }
}

unsafe extern "C" fn set_intersection_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "set.intersection") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 2 {
        return null_error(format!("set.intersection() expected 1 argument, got {}", args.len().saturating_sub(1)));
    }
    unsafe { pon_set_intersection(args[0], args[1]) }
}

unsafe extern "C" fn set_difference_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "set.difference") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 2 {
        return null_error(format!("set.difference() expected 1 argument, got {}", args.len().saturating_sub(1)));
    }
    unsafe { pon_set_difference(args[0], args[1]) }
}

unsafe extern "C" fn set_issubset_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "set.issubset") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 2 {
        return null_error(format!("set.issubset() expected 1 argument, got {}", args.len().saturating_sub(1)));
    }
    set_is_subset(args[0], args[1])
}

/// Returns a bound set method object for attribute lookup.
pub unsafe fn pon_set_bound_method(set: *mut PyObject, name: &str) -> *mut PyObject {
    match name {
        "add" => alloc_bound_native_method(set, name, set_add_method_trampoline),
        "discard" => alloc_bound_native_method(set, name, set_discard_method_trampoline),
        "union" => alloc_bound_native_method(set, name, set_union_method_trampoline),
        "intersection" => alloc_bound_native_method(set, name, set_intersection_method_trampoline),
        "difference" => alloc_bound_native_method(set, name, set_difference_method_trampoline),
        "issubset" => alloc_bound_native_method(set, name, set_issubset_method_trampoline),
        _ => null_error(format!("attribute '{name}' was not found")),
    }
}

/// Hashes a frozenset. Returns `-1` on error with a thread-state error set.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_frozenset_hash(object: *mut PyObject) -> isize {
    crate::untag_prelude!(err = -1; object);
    match catch_unwind(AssertUnwindSafe(|| unsafe { frozenset::frozenset_hash_value(object) })) {
        Ok(Ok(hash)) => hash,
        Ok(Err(message)) => {
            pon_err_set(message);
            -1
        }
        Err(_) => {
            pon_err_set("runtime helper panicked");
            -1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::{pon_const_int, pon_runtime_init};
    use crate::object::PyLong;
    use crate::thread_state::test_state_lock;

    #[test]
    fn map_set_sync_mutator_paths_compile_and_update_state() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);

            let key = pon_const_int(1);
            let value = pon_const_int(2);
            let mut pairs = [key, value];
            let dict = pon_build_map(pairs.as_mut_ptr(), 1);
            assert!(!dict.is_null());

            let new_value = pon_const_int(3);
            assert_eq!(pon_map_insert(dict, key, new_value), dict);
            let loaded = pon_dict_get_item(dict, key);
            assert!(!loaded.is_null());
            assert_eq!((*loaded.cast::<PyLong>()).value, 3);

            let default_key = pon_const_int(4);
            let default_value = pon_const_int(5);
            assert_eq!(pon_dict_setdefault(dict, default_key, default_value), default_value);
            assert_eq!(pon_dict_pop(dict, default_key, ptr::null_mut()), default_value);

            let set = pon_build_set(ptr::null_mut(), 0);
            assert!(!set.is_null());
            assert_eq!(pon_set_add(set, key), set);
            assert_eq!(set_::entries_snapshot(set).expect("set entries").len(), 1);
        }
    }
}
