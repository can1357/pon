//! Mapping and set helper family namespace.
//!
//! WS-MAP owns concrete dict/set/frozenset helpers. They follow the runtime-wide
//! sentinel discipline: object helpers return NULL with a thread-state error and
//! status/predicate helpers return `-1` with a thread-state error.

use core::{ffi::c_int, mem, ptr};
use std::panic::{catch_unwind, AssertUnwindSafe};

use pon_gc::{GcTypeInfo, TypeId};

use crate::object::{as_object_ptr, PyObject, PyUnicode};
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
    super::catch_status_helper(|| {
        let _guard = crate::sync::begin_critical_section(map);
        match unsafe { dict::dict_insert(map, key, value) } {
            Ok(()) => 0,
            Err(message) => status_error(message),
        }
    })
}

/// Fetches `map[key]`, raising KeyError on a miss.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_get_item(map: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
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
    super::catch_object_helper(|| {
        if unsafe { dict::is_dict(object) } {
            let _guard = crate::sync::begin_critical_section(object);
            match unsafe { dict::dict_remove(object, key) } {
                Ok(Some(_)) => unsafe { super::pon_none() },
                Ok(None) => raise_key_error(key),
                Err(message) => null_error(message),
            }
        } else {
            null_error("object does not support item deletion")
        }
    })
}
/// Stores `object[key] = value` and returns `value`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_subscript_set(object: *mut PyObject, key: *mut PyObject, value: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        if unsafe { dict::is_dict(object) } {
            let _guard = crate::sync::begin_critical_section(object);
            match unsafe { dict::dict_insert(object, key, value) } {
                Ok(()) => value,
                Err(message) => null_error(message),
            }
        } else {
            null_error("object does not support item assignment")
        }
    })
}
/// Status form used by mapping slots.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_del_item_status(map: *mut PyObject, key: *mut PyObject) -> c_int {
    super::catch_status_helper(|| {
        let _guard = crate::sync::begin_critical_section(map);
        match unsafe { dict::dict_remove(map, key) } {
            Ok(Some(_)) => 0,
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
    unsafe { pon_dict_merge(map, other) }
}

/// `dict.get(key, default=None)` helper.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_get_method(map: *mut PyObject, key: *mut PyObject, default: *mut PyObject) -> *mut PyObject {
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

unsafe extern "C" fn dict_get_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    super::catch_object_helper(|| {
        if argv.is_null() && argc != 0 {
            return null_error("dict.get() received a NULL argv pointer");
        }
        if argc != 2 {
            let explicit = argc.saturating_sub(1);
            return null_error(format!("dict.get() expected 1 argument, got {explicit}"));
        }
        let args = unsafe { core::slice::from_raw_parts(argv, argc) };
        unsafe { pon_dict_get_method(args[0], args[1], ptr::null_mut()) }
    })
}

/// Returns a bound `dict.get` method object for attribute lookup.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_get_bound_method(map: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        if let Err(message) = ensure_runtime_for_map() {
            return null_error(message);
        }
        let function = match super::with_runtime(|runtime| {
            super::alloc_function(
                runtime,
                dict_get_method_trampoline as *const () as *const u8,
                2,
                crate::intern::intern("get"),
            )
        }) {
            Some(Ok(function)) => function,
            Some(Err(message)) => return null_error(message),
            None => return null_error("runtime is not initialized"),
        };
        match method::new_bound_method(function, map) {
            Ok(bound) => bound.cast::<PyObject>(),
            Err(message) => null_error(message),
        }
    })
}

/// `dict.setdefault(key, default=None)` helper.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_setdefault(map: *mut PyObject, key: *mut PyObject, default: *mut PyObject) -> *mut PyObject {
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
    unsafe { pon_dict_iter_keys(map) }
}

/// Returns an insertion-order value iterator for a dict.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_values(map: *mut PyObject) -> *mut PyObject {
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
            dict::DictIterKind::Items => match super::with_runtime(|runtime| alloc_dict_item(runtime, entry.key, entry.value)) {
                Some(item) => item,
                None => null_error("runtime is not initialized"),
            },
        }
    })
}

/// Adds an element to a set and returns the receiver.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_set_add(set: *mut PyObject, item: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        let _guard = crate::sync::begin_critical_section(set);
        match unsafe { set_::set_add(set, item) } {
            Ok(()) => set,
            Err(message) => null_error(message),
        }
    })
}

/// Contains predicate for dict/set/frozenset. Returns `1`, `0`, or `-1` on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_contains(container: *mut PyObject, item: *mut PyObject) -> c_int {
    super::catch_status_helper(|| {
        let _guard = crate::sync::begin_critical_section(container);
        let result = if unsafe { dict::is_dict(container) } {
            unsafe { dict::dict_contains(container, item) }
        } else if unsafe { set_::is_any_set(container) } {
            unsafe { set_::set_contains(container, item) }
        } else {
            Err("object does not support containment".to_owned())
        };
        match result {
            Ok(true) => 1,
            Ok(false) => 0,
            Err(message) => status_error(message),
        }
    })
}

/// Returns an iterator over a set/frozenset.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_set_iter(set: *mut PyObject) -> *mut PyObject {
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

/// Hashes a frozenset. Returns `-1` on error with a thread-state error set.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_frozenset_hash(object: *mut PyObject) -> isize {
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
