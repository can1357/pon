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
const TYPE_ID_SET: TypeId = TypeId(104);
const TYPE_ID_SET_ITER: TypeId = TypeId(105);
const TYPE_ID_FROZENSET: TypeId = TypeId(106);
/// Heap-class instances embedding dict storage (`PyDictSubclassInstance`).
/// 103 is retired (former `PyDictItem`); 107 avoids any resurrection clash.
const TYPE_ID_DICT_SUBCLASS_INSTANCE: TypeId = TypeId(107);

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
    runtime.heap.register_type(
        TYPE_ID_DICT_SUBCLASS_INSTANCE,
        GcTypeInfo {
            size: mem::size_of::<dict::PyDictSubclassInstance>(),
            trace: dict::trace_dict_subclass_instance,
            finalize: Some(dict::finalize_dict_subclass_instance),
        },
    );
}

fn ensure_runtime_for_map() -> Result<(), String> {
    super::ensure_runtime_initialized()
}

fn null_error(message: impl Into<String>) -> *mut PyObject {
    let message = message.into();
    // Hash failures must surface as catchable TypeError objects (CPython:
    // `except TypeError` around `d[[1]] = 1`), not opaque diagnostics. Covers
    // both the bare hash message and the dict-key/set-element wrapped form.
    if message.starts_with("unhashable type") || message.starts_with("cannot use '") {
        return unsafe { super::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    }
    super::return_null_with_error(message)
}

/// Typed `TypeError` for dict/set method arity and keyword-shape misuse
/// (CPython `except TypeError:` must fire) — unless a live boxed exception
/// is already pending, which stays authoritative.
fn raise_map_type_error(message: impl AsRef<str>) -> *mut PyObject {
    if super::exc::pending_exception_object().is_some() {
        return ptr::null_mut();
    }
    super::exc::raise_kind_error_text(crate::types::exc::ExceptionKind::TypeError, message.as_ref())
}

fn duplicate_keyword_error(key: *mut PyObject) -> *mut PyObject {
    if unsafe { dict::type_name(key) } != Some("str") {
        return raise_map_type_error("keywords must be strings");
    }
    let Some(name) = (unsafe { (&*key.cast::<PyUnicode>()).as_str() }) else {
        return null_error("keyword name is not valid UTF-8");
    };
    raise_map_type_error(format!("got multiple values for keyword argument '{name}'"))
}

fn status_error(message: impl Into<String>) -> c_int {
    let _ = null_error(message);
    -1
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
///
/// CPython `dict_subscript` parity: a miss on a dict-SUBCLASS instance
/// consults the type's `__missing__` hook (MRO lookup, exact dicts never
/// pay for it) before raising KeyError — the seam `defaultdict` and
/// `Counter.__getitem__`-style classes rely on.  The hook re-enters Python,
/// so the storage probe's critical section is scoped to release first.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_dict_get_item(map: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(map, key);
    super::catch_object_helper(|| {
        {
            let _guard = crate::sync::begin_critical_section(map);
            match unsafe { dict::dict_get(map, key) } {
                Ok(Some(value)) => return value,
                Ok(None) => {}
                Err(message) => return null_error(message),
            }
        }
        if unsafe { dict::is_dict_subclass_instance(map) } {
            let ty = unsafe { (*map).ob_type.cast_mut() };
            let missing = unsafe { crate::descr::lookup_in_type(ty, crate::intern::intern("__missing__")) };
            if !missing.is_null() {
                let bound = unsafe { crate::descr::descriptor_get(missing, map, ty) };
                if bound.is_null() {
                    return ptr::null_mut();
                }
                return unsafe { crate::descr::call_with_one(bound, key) };
            }
        }
        raise_key_error(key)
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
                    return raise_map_type_error("sequence index must be an int");
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
                // Python-level `__setitem__` (heap instances, incl. dict
                // subclasses reaching the natives installed in dict's
                // tp_dict; user overrides win by MRO order).  Mirrors the
                // `__delitem__` fallback in `abstract_op::subscript_del`.
                let setitem = unsafe { crate::descr::lookup_in_type(ty, crate::intern::intern("__setitem__")) };
                if setitem.is_null() {
                    let message = format!("'{}' object does not support item assignment", unsafe { (*ty).name() });
                    // SAFETY: Raising TypeError follows the NULL-sentinel contract.
                    return unsafe { super::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
                }
                let callable = unsafe { crate::descr::descriptor_get(setitem, object, ty) };
                if callable.is_null() {
                    return ptr::null_mut();
                }
                let mut argv = [key, value];
                let result = unsafe { super::pon_call(callable, argv.as_mut_ptr(), argv.len()) };
                if result.is_null() {
                    return ptr::null_mut();
                }
                value
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

/// Unbound-receiver validation for dict method descriptors reached off the
/// type (`dict.get(x, …)`): CPython raises the mismatch TypeError before the
/// method body runs.  `name` is the bare method name.  Returns the untagged
/// receiver, or the raised NULL sentinel.
fn ensure_dict_method_receiver(args: &[*mut PyObject], name: &str) -> Result<*mut PyObject, *mut PyObject> {
    if args.is_empty() {
        let message = format!("unbound method dict.{name}() needs an argument");
        return Err(unsafe { super::exc::pon_raise_type_error(message.as_ptr(), message.len()) });
    }
    let receiver = crate::tag::untag_arg(args[0]);
    if receiver.is_null() {
        return Err(ptr::null_mut());
    }
    if unsafe { !dict::has_dict_storage(receiver) } {
        let ty = unsafe { dict::type_name(receiver) }.unwrap_or("object");
        let message = format!("descriptor '{name}' for 'dict' objects doesn't apply to a '{ty}' object");
        return Err(unsafe { super::exc::pon_raise_type_error(message.as_ptr(), message.len()) });
    }
    Ok(receiver)
}

/// Allocates a heap instance of a dict-derived class: the generic
/// heap-instance prefix plus empty embedded dict storage.
pub(crate) fn alloc_dict_subclass_instance(
    cls: *mut crate::object::PyType,
    instance_dict: *mut crate::types::type_::PyClassDict,
    slots: Vec<crate::types::type_::PySlotValue>,
) -> Result<*mut PyObject, String> {
    super::with_runtime(|runtime| {
        register_map_types(runtime);
        let object = runtime
            .heap
            .alloc(mem::size_of::<dict::PyDictSubclassInstance>(), TYPE_ID_DICT_SUBCLASS_INSTANCE)
            .cast::<dict::PyDictSubclassInstance>();
        unsafe {
            ptr::write(
                object,
                dict::PyDictSubclassInstance {
                    base: crate::types::type_::PyHeapInstance {
                        ob_base: crate::object::PyObjectHeader::new(cls),
                        dict: instance_dict,
                        slots,
                        weakrefs: ptr::null_mut(),
                        finalized: false,
                    },
                    storage: dict::PyDictStorage {
                        entries: Vec::new(),
                        buckets: Vec::new(),
                    },
                },
            );
        }
        Ok(as_object_ptr(object))
    })
    .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
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

pub(crate) unsafe extern "C" fn dict_get_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject { super::catch_object_helper(|| {
    let args = match map_method_args(argv, argc, "dict.get") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    let receiver = match ensure_dict_method_receiver(args, "get") {
        Ok(receiver) => receiver,
        Err(raised) => return raised,
    };
    if !(args.len() == 2 || args.len() == 3) {
        return raise_map_type_error(format!("dict.get() expected 1 or 2 arguments, got {}", args.len().saturating_sub(1)));
    }
    let default = args.get(2).copied().unwrap_or(ptr::null_mut());
    unsafe { pon_dict_get_method(receiver, args[1], default) }
}) }

pub(crate) unsafe extern "C" fn dict_keys_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject { let args = match map_method_args(argv, argc, "dict.keys") {
    Ok(args) => args,
    Err(message) => return null_error(message),
};
let receiver = match ensure_dict_method_receiver(args, "keys") {
    Ok(receiver) => receiver,
    Err(raised) => return raised,
};
if args.len() != 1 {
    return raise_map_type_error(format!("dict.keys() expected 0 arguments, got {}", args.len().saturating_sub(1)));
}
unsafe { pon_dict_keys(receiver) } }

pub(crate) unsafe extern "C" fn dict_values_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject { let args = match map_method_args(argv, argc, "dict.values") {
    Ok(args) => args,
    Err(message) => return null_error(message),
};
let receiver = match ensure_dict_method_receiver(args, "values") {
    Ok(receiver) => receiver,
    Err(raised) => return raised,
};
if args.len() != 1 {
    return raise_map_type_error(format!("dict.values() expected 0 arguments, got {}", args.len().saturating_sub(1)));
}
unsafe { pon_dict_values(receiver) } }

pub(crate) unsafe extern "C" fn dict_items_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject { let args = match map_method_args(argv, argc, "dict.items") {
    Ok(args) => args,
    Err(message) => return null_error(message),
};
let receiver = match ensure_dict_method_receiver(args, "items") {
    Ok(receiver) => receiver,
    Err(raised) => return raised,
};
if args.len() != 1 {
    return raise_map_type_error(format!("dict.items() expected 0 arguments, got {}", args.len().saturating_sub(1)));
}
unsafe { pon_dict_items(receiver) } }

pub(crate) unsafe extern "C" fn dict_copy_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "dict.copy") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    let receiver = match ensure_dict_method_receiver(args, "copy") {
        Ok(receiver) => receiver,
        Err(raised) => return raised,
    };
    if args.len() != 1 {
        return raise_map_type_error(format!("dict.copy() expected 0 arguments, got {}", args.len().saturating_sub(1)));
    }
    // CPython `dict.copy`: a shallow EXACT dict, also for subclass receivers.
    let entries = match unsafe { dict::dict_entries_snapshot(receiver) } {
        Ok(entries) => entries,
        Err(message) => return null_error(message),
    };
    let mut pairs = Vec::with_capacity(entries.len() * 2);
    for entry in entries {
        pairs.push(entry.key);
        pairs.push(entry.value);
    }
    unsafe { pon_build_map(pairs.as_mut_ptr(), pairs.len() / 2) }
}

pub(crate) unsafe extern "C" fn dict_setdefault_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject { let args = match map_method_args(argv, argc, "dict.setdefault") {
    Ok(args) => args,
    Err(message) => return null_error(message),
};
let receiver = match ensure_dict_method_receiver(args, "setdefault") {
    Ok(receiver) => receiver,
    Err(raised) => return raised,
};
if !(args.len() == 2 || args.len() == 3) {
    return raise_map_type_error(format!("dict.setdefault() expected 1 or 2 arguments, got {}", args.len().saturating_sub(1)));
}
let default = args.get(2).copied().unwrap_or(ptr::null_mut());
unsafe { pon_dict_setdefault(receiver, args[1], default) } }

pub(crate) unsafe extern "C" fn dict_pop_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject { let args = match map_method_args(argv, argc, "dict.pop") {
    Ok(args) => args,
    Err(message) => return null_error(message),
};
let receiver = match ensure_dict_method_receiver(args, "pop") {
    Ok(receiver) => receiver,
    Err(raised) => return raised,
};
if !(args.len() == 2 || args.len() == 3) {
    return raise_map_type_error(format!("dict.pop() expected 1 or 2 arguments, got {}", args.len().saturating_sub(1)));
}
let default = args.get(2).copied().unwrap_or(ptr::null_mut());
unsafe { pon_dict_pop(receiver, args[1], default) } }

pub(crate) unsafe extern "C" fn dict_update_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject { let args = match map_method_args(argv, argc, "dict.update") {
    Ok(args) => args,
    Err(message) => return null_error(message),
};
let receiver = match ensure_dict_method_receiver(args, "update") {
    Ok(receiver) => receiver,
    Err(raised) => return raised,
};
if args.len() != 2 {
    return raise_map_type_error(format!("dict.update() expected 1 argument, got {}", args.len().saturating_sub(1)));
}
let updated = unsafe { pon_dict_update(receiver, args[1]) };
if updated.is_null() {
    updated
} else {
    crate::dynexec::sync_globals_dict_bulk(receiver);
    map_none()
} }

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
        "copy" => alloc_bound_native_method(map, name, dict_copy_method_trampoline),
        _ => super::exc::raise_attribute_error_text(&format!("attribute '{name}' was not found")),
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
        if unsafe { !dict::has_dict_storage(map) } {
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
            dict::DictIterKind::Items => {
                match super::with_runtime(|runtime| super::seq::alloc_tuple_from_slice(runtime, &[entry.key, entry.value])) {
                    Some(Ok(pair)) => pair,
                    Some(Err(message)) => return null_error(message),
                    None => return null_error("runtime is not initialized"),
                }
            }
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

/// Adds every element of `iterable` to `set` and returns the receiver.
///
/// Backs `InstKind::SetUpdate` for starred set displays (`{*a, b}`): items
/// are added one at a time AS the iterable is advanced, so hash/eq side
/// effects interleave with iteration exactly like CPython's `SET_UPDATE`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_set_update(set: *mut PyObject, iterable: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(set, iterable);
    let iter = unsafe { super::iter::pon_get_iter(iterable, ptr::null_mut()) };
    if iter.is_null() {
        // Mirror `sequence_to_vec`: iterables without `tp_iter` (indexed
        // sequences such as str) fall back to materialized indexing.
        if crate::thread_state::pon_err_occurred() {
            crate::thread_state::pon_err_clear();
        }
        let values = match super::seq::sequence_to_vec(iterable) {
            Ok(values) => values,
            Err(message) => return null_error(message),
        };
        for item in values {
            if unsafe { pon_set_add(set, item) }.is_null() {
                return ptr::null_mut();
            }
        }
        return set;
    }
    loop {
        let item = unsafe { super::iter::pon_iter_next(iter, ptr::null_mut()) };
        if item.is_null() {
            // Exhaustion convention shared with `sequence_to_vec`: a NULL from
            // `pon_iter_next` ends iteration and clears any pending marker.
            if crate::thread_state::pon_err_occurred() {
                crate::thread_state::pon_err_clear();
            }
            return set;
        }
        // Delegates per-element semantics (untag, critical section, error
        // path) to the existing SetAdd helper.
        if unsafe { pon_set_add(set, item) }.is_null() {
            return ptr::null_mut();
        }
    }
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
        if let Some(slot) = unsafe { sequence_contains_slot(container) } {
            let status = unsafe { slot(container, item) };
            return if status < 0 {
                -1
            } else if status == 0 {
                0
            } else {
                1
            };
        }
        // Python-level `__contains__` (heap instances, e.g. WeakSet).
        let container_type = unsafe { (*container).ob_type.cast_mut() };
        let hook = unsafe { crate::descr::lookup_in_type(container_type, crate::intern::intern("__contains__")) };
        if hook.is_null() {
            // CPython `PySequence_Contains` -> `_PySequence_IterSearch`:
            // without `__contains__`, membership is equality over iteration
            // (including the legacy `__getitem__` sequence protocol).
            let iter = unsafe { super::iter::pon_get_iter(container, ptr::null_mut()) };
            if iter.is_null() {
                crate::thread_state::pon_err_clear();
                let message = format!("argument of type '{}' is not a container or iterable", unsafe { (*container_type).name() });
                unsafe { super::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
                return -1;
            }
            loop {
                let candidate = unsafe { super::iter::pon_iter_next(iter, ptr::null_mut()) };
                if candidate.is_null() {
                    if super::exc::pending_exception_is("StopIteration") {
                        crate::thread_state::pon_err_clear();
                        return 0;
                    }
                    return -1;
                }
                // Identity shortcut mirrors CPython `PyObject_RichCompareBool`.
                if candidate == item {
                    return 1;
                }
                let verdict = unsafe { crate::abstract_op::rich_compare(crate::abstract_op::RICH_EQ, item, candidate) };
                if verdict.is_null() {
                    return -1;
                }
                match unsafe { super::pon_is_true(verdict) } {
                    0 => {}
                    1 => return 1,
                    _ => return -1,
                }
            }
        }
        let bound = unsafe { crate::descr::descriptor_get(hook, container, container_type) };
        if bound.is_null() {
            return -1;
        }
        let result = unsafe { crate::descr::call_with_one(bound, item) };
        if result.is_null() {
            return -1;
        }
        unsafe { super::pon_is_true(result) }
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
    let result = unsafe {
        set_::entries_snapshot(left).and_then(|left_entries| {
            let right_entries = set_::entries_snapshot(right)?;
            set_::entries_subset(&left_entries, &right_entries)
        })
    };
    match result {
        Ok(value) => unsafe { super::number::pon_const_bool(c_int::from(value)) },
        Err(message) => null_error(message),
    }
}

unsafe extern "C" fn set_contains_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "set.__contains__") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 2 {
        return raise_map_type_error(format!(
            "set.__contains__() expected 1 argument, got {}",
            args.len().saturating_sub(1)
        ));
    }
    match unsafe { set_::set_contains(args[0], args[1]) } {
        Ok(value) => unsafe { super::number::pon_const_bool(c_int::from(value)) },
        Err(message) => null_error(message),
    }
}

unsafe extern "C" fn set_add_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "set.add") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 2 {
        return raise_map_type_error(format!("set.add() expected 1 argument, got {}", args.len().saturating_sub(1)));
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
        return raise_map_type_error(format!("set.discard() expected 1 argument, got {}", args.len().saturating_sub(1)));
    }
    unsafe { pon_set_discard(args[0], args[1]) }
}

unsafe extern "C" fn set_union_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "set.union") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 2 {
        return raise_map_type_error(format!("set.union() expected 1 argument, got {}", args.len().saturating_sub(1)));
    }
    unsafe { pon_set_union(args[0], args[1]) }
}

unsafe extern "C" fn set_intersection_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "set.intersection") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 2 {
        return raise_map_type_error(format!("set.intersection() expected 1 argument, got {}", args.len().saturating_sub(1)));
    }
    unsafe { pon_set_intersection(args[0], args[1]) }
}

unsafe extern "C" fn set_difference_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "set.difference") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 2 {
        return raise_map_type_error(format!("set.difference() expected 1 argument, got {}", args.len().saturating_sub(1)));
    }
    unsafe { pon_set_difference(args[0], args[1]) }
}

unsafe extern "C" fn set_issubset_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "set.issubset") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 2 {
        return raise_map_type_error(format!("set.issubset() expected 1 argument, got {}", args.len().saturating_sub(1)));
    }
    set_is_subset(args[0], args[1])
}

unsafe extern "C" fn set_copy_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "set.copy") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 1 {
        return raise_map_type_error(format!("set.copy() expected 0 arguments, got {}", args.len().saturating_sub(1)));
    }
    let receiver = args[0];
    // CPython returns the receiver itself for exact frozensets.
    if unsafe { frozenset::is_frozenset(receiver) } {
        return receiver;
    }
    let _guard = crate::sync::begin_critical_section(receiver);
    let entries = match unsafe { set_::entries_snapshot(receiver) } {
        Ok(entries) => entries,
        Err(message) => return null_error(message),
    };
    match super::with_runtime(|runtime| build_set_from_entries(runtime, receiver, entries)) {
        Some(object) => object,
        None => null_error("runtime is not initialized"),
    }
}

unsafe extern "C" fn set_remove_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "set.remove") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 2 {
        return raise_map_type_error(format!("set.remove() expected 1 argument, got {}", args.len().saturating_sub(1)));
    }
    let _guard = crate::sync::begin_critical_section(args[0]);
    match unsafe { set_::set_remove(args[0], args[1]) } {
        Ok(true) => map_none(),
        Ok(false) => raise_key_error(args[1]),
        Err(message) => null_error(message),
    }
}

unsafe extern "C" fn set_pop_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "set.pop") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 1 {
        return raise_map_type_error(format!("set.pop() expected 0 arguments, got {}", args.len().saturating_sub(1)));
    }
    let _guard = crate::sync::begin_critical_section(args[0]);
    match unsafe { set_::set_pop(args[0]) } {
        Ok(Some(value)) => value,
        Ok(None) => {
            let message = "pop from an empty set";
            let message = unsafe { crate::abi::pon_const_str(message.as_ptr(), message.len()) };
            raise_key_error(message)
        }
        Err(message) => null_error(message),
    }
}

unsafe extern "C" fn set_clear_method_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match map_method_args(argv, argc, "set.clear") {
        Ok(args) => args,
        Err(message) => return null_error(message),
    };
    if args.len() != 1 {
        return raise_map_type_error(format!("set.clear() expected 0 arguments, got {}", args.len().saturating_sub(1)));
    }
    let _guard = crate::sync::begin_critical_section(args[0]);
    match unsafe { set_::set_clear(args[0]) } {
        Ok(()) => map_none(),
        Err(message) => null_error(message),
    }
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
        "__contains__" => alloc_bound_native_method(set, name, set_contains_method_trampoline),
        "copy" => alloc_bound_native_method(set, name, set_copy_method_trampoline),
        "remove" => alloc_bound_native_method(set, name, set_remove_method_trampoline),
        "clear" => alloc_bound_native_method(set, name, set_clear_method_trampoline),
        "pop" => alloc_bound_native_method(set, name, set_pop_method_trampoline),
        _ => super::exc::raise_attribute_error_text(&format!("attribute '{name}' was not found")),
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
