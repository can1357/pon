//! Native `weakref` module seed.
//!
//! Beyond the `ref`/`proxy` type aliases, this serves a native
//! `WeakKeyDictionary` — the piece `unittest.signals` instantiates at import
//! (`_results = weakref.WeakKeyDictionary()`).  Keys are held through real
//! `weakref.ref` objects, so entries drop once the collector clears a dead
//! key's referent.  Documented divergences from `Lib/weakref.py`: lookups are
//! O(n) equality scans (no hash table), `keys()` returns a materialized list
//! rather than a view, and instances are immortal leaked boxes (the
//! `_collections` deque pattern) whose held values/refs are reported through
//! [`gc_held_roots`].

use core::ffi::c_int;
use std::ptr;
use std::sync::{LazyLock, Mutex};

use crate::abi;
use crate::abstract_op::RICH_EQ;
use crate::intern::intern;
use crate::object::{PyMappingMethods, PyObject, PyObjectHeader, PyType};
use crate::types::exc::ExceptionKind;
use crate::types::type_::unicode_text;

use super::install_module;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    make_module_named("weakref")
}

pub(super) fn make_underscore_module() -> Result<*mut PyObject, String> {
    make_module_named("_weakref")
}

fn make_module_named(name: &'static str) -> Result<*mut PyObject, String> {
    let mut attrs = vec![
        (intern("__name__"), unsafe { crate::abi::pon_const_str(name.as_ptr(), name.len()) }),
        (intern("ref"), crate::types::weakref::weakref_ref_type()),
        (intern("ReferenceType"), crate::types::weakref::weakref_ref_type()),
        (intern("proxy"), crate::types::weakref::weakref_proxy_type()),
        (intern("ProxyType"), crate::types::weakref::weakref_proxy_type()),
        (intern("CallableProxyType"), crate::types::weakref::weakref_proxy_type()),
    ];
    if name == "weakref" {
        // CPython defines WeakKeyDictionary/WeakValueDictionary in
        // `Lib/weakref.py`, not `_weakref`.
        attrs.push((intern("WeakKeyDictionary"), wkd_type().cast::<PyObject>()));
        attrs.push((intern("WeakValueDictionary"), wvd_type().cast::<PyObject>()));
        // `WeakSet` lives in the vendored pure-Python `_weakrefset` (its only
        // native need is `_weakref.ref`, served by this module's underscore
        // twin).  This factory runs on first `import weakref`, so importing
        // here matches `Lib/weakref.py`'s `from _weakrefset import WeakSet`
        // re-export.  A broken `_weakrefset` degrades to the pre-existing
        // surface (no `WeakSet`) instead of poisoning the whole module.
        // SAFETY: Import entry point follows the NULL-sentinel error contract.
        let weakrefset = unsafe { crate::import::pon_import_name(intern("_weakrefset"), ptr::null(), 0, 0) };
        if weakrefset.is_null() {
            crate::thread_state::pon_err_clear();
        } else {
            // SAFETY: Attribute lookup follows the NULL-sentinel error contract.
            let weak_set = unsafe { crate::abstract_op::get_attr(weakrefset, intern("WeakSet")) };
            if weak_set.is_null() {
                crate::thread_state::pon_err_clear();
            } else {
                attrs.push((intern("WeakSet"), weak_set));
            }
        }
    }
    install_module(name, attrs)
}

// ---------------------------------------------------------------------------
// WeakKeyDictionary

#[repr(C)]
struct PyWeakKeyDict {
    ob_base: PyObjectHeader,
    /// `(weakref.ref object, value)` pairs; dead entries pruned on access.
    entries: Vec<(*mut PyObject, *mut PyObject)>,
}

static WKD_MAPPING: PyMappingMethods = PyMappingMethods {
    mp_length: Some(wkd_len_slot),
    mp_subscript: Some(wkd_subscript_slot),
    mp_ass_subscript: Some(wkd_ass_subscript_slot),
};

static WKD_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(
        abi::runtime_type_type().cast_const(),
        "weakref.WeakKeyDictionary",
        std::mem::size_of::<PyWeakKeyDict>(),
    );
    ty.tp_base = abi::runtime_global(intern("object"))
        .map_or(ptr::null_mut(), |object| object.cast::<PyType>());
    ty.tp_new = Some(wkd_new);
    ty.tp_getattro = Some(wkd_getattro);
    ty.tp_bool = Some(wkd_bool);
    ty.tp_iter = Some(wkd_iter);
    ty.tp_as_mapping = ptr::addr_of!(WKD_MAPPING).cast_mut();
    Box::into_raw(Box::new(ty)) as usize
});

fn wkd_type() -> *mut PyType {
    *WKD_TYPE as *mut PyType
}

/// Every WeakKeyDictionary allocation, for GC root reporting.  Objects are
/// immortal leaked boxes, so the registry only grows.
static REGISTRY: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// GC roots held by native WeakKeyDictionaries: the `weakref.ref` key objects
/// (NOT their referents — that is the weakness) and the stored values.
/// Consumed by `crate::abi::collect` under the runtime lock; must not
/// re-enter the runtime.  The type pointer is read without forcing the
/// `LazyLock` (uninitialized type means no instances exist yet).
pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
    let mut roots = Vec::new();
    if LazyLock::get(&WKD_TYPE).is_some() {
        let registry = REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
        for &object in registry.iter() {
            let dict = object as *mut PyWeakKeyDict;
            // SAFETY: Registry members are live leaked boxes of PyWeakKeyDict layout.
            for &(weak, value) in unsafe { &(*dict).entries } {
                if !weak.is_null() {
                    roots.push(weak);
                }
                if !value.is_null() {
                    roots.push(value);
                }
            }
        }
    }
    if LazyLock::get(&WVD_TYPE).is_some() {
        let registry = WVD_REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
        for &object in registry.iter() {
            let dict = object as *mut PyWeakValueDict;
            // SAFETY: Registry members are live leaked boxes of PyWeakValueDict layout.
            for &(key, weak) in unsafe { &(*dict).entries } {
                if !key.is_null() {
                    roots.push(key);
                }
                if !weak.is_null() {
                    roots.push(weak);
                }
            }
        }
    }
    roots
}

fn untag(object: *mut PyObject) -> *mut PyObject {
    crate::tag::untag_arg(object)
}

fn fail(message: impl Into<String>) -> *mut PyObject {
    crate::thread_state::pon_err_set(message);
    ptr::null_mut()
}

fn none() -> *mut PyObject {
    // SAFETY: Singleton accessor.
    unsafe { abi::pon_none() }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

unsafe fn as_wkd<'a>(object: *mut PyObject) -> Option<&'a mut PyWeakKeyDict> {
    let object = untag(object);
    if object.is_null() {
        return None;
    }
    // SAFETY: A non-NULL heap object carries a live header.
    if unsafe { (*object).ob_type } != wkd_type().cast_const() {
        return None;
    }
    Some(unsafe { &mut *object.cast::<PyWeakKeyDict>() })
}

/// Drops entries whose referent was cleared by the collector.
fn prune(dict: &mut PyWeakKeyDict) {
    dict.entries
        .retain(|&(weak, _)| !unsafe { crate::types::weakref::weakref_target(weak) }.is_null());
}

/// Index of the live entry whose key equals `key` (identity fast path, then
/// rich `==`).  `Err(())` propagates a comparison failure.
fn find_entry(dict: &PyWeakKeyDict, key: *mut PyObject) -> Result<Option<usize>, ()> {
    for (index, &(weak, _)) in dict.entries.iter().enumerate() {
        // SAFETY: Stored keys are live weakref objects (rooted by the registry).
        let target = unsafe { crate::types::weakref::weakref_target(weak) };
        if target.is_null() {
            continue;
        }
        if target == key {
            return Ok(Some(index));
        }
        // SAFETY: Rich-compare helper follows the NULL-sentinel error contract.
        let equal = unsafe { abi::object::pon_rich_compare(RICH_EQ, target, key, ptr::null_mut()) };
        if equal.is_null() {
            return Err(());
        }
        match unsafe { abi::pon_is_true(equal) } {
            1 => return Ok(Some(index)),
            0 => {}
            _ => return Err(()),
        }
    }
    Ok(None)
}

/// Builds a `weakref.ref(target)` through the generic call path, so the
/// weakrefability TypeError matches `weakref.ref` exactly.
fn make_weak_ref(target: *mut PyObject) -> *mut PyObject {
    let mut argv = [target];
    // SAFETY: The weakref type object is immortal; one live argument slot.
    unsafe { abi::pon_call(crate::types::weakref::weakref_ref_type(), argv.as_mut_ptr(), 1) }
}

/// Materialized list of live keys (divergence: CPython returns a view).
fn live_keys(dict: &mut PyWeakKeyDict) -> *mut PyObject {
    prune(dict);
    let keys = dict
        .entries
        .iter()
        .map(|&(weak, _)| unsafe { crate::types::weakref::weakref_target(weak) })
        .filter(|target| !target.is_null())
        .collect::<Vec<_>>();
    super::builtins_mod::alloc_list(keys)
}

// ---------------------------------------------------------------------------
// WeakKeyDictionary slots

unsafe extern "C" fn wkd_new(_cls: *mut PyType, args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
    let positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
        Ok(positional) => positional,
        Err(message) => return fail(message),
    };
    if !positional.is_empty() {
        // Loud frontier: the optional `dict` seed argument is unimplemented.
        return raise_type_error("WeakKeyDictionary() with a dict argument is not supported");
    }
    let object = Box::into_raw(Box::new(PyWeakKeyDict {
        ob_base: PyObjectHeader::new(wkd_type()),
        entries: Vec::new(),
    }));
    REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner()).push(object as usize);
    object.cast::<PyObject>()
}

unsafe extern "C" fn wkd_len_slot(object: *mut PyObject) -> isize {
    let Some(dict) = (unsafe { as_wkd(object) }) else {
        crate::thread_state::pon_err_set("WeakKeyDictionary receiver is invalid");
        return -1;
    };
    prune(dict);
    dict.entries.len() as isize
}

unsafe extern "C" fn wkd_bool(object: *mut PyObject) -> c_int {
    let Some(dict) = (unsafe { as_wkd(object) }) else {
        crate::thread_state::pon_err_set("WeakKeyDictionary receiver is invalid");
        return -1;
    };
    prune(dict);
    c_int::from(!dict.entries.is_empty())
}

unsafe extern "C" fn wkd_subscript_slot(object: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    let key = untag(key);
    if key.is_null() {
        return ptr::null_mut();
    }
    let Some(dict) = (unsafe { as_wkd(object) }) else {
        return fail("WeakKeyDictionary receiver is invalid");
    };
    prune(dict);
    match find_entry(dict, key) {
        Ok(Some(index)) => dict.entries[index].1,
        // SAFETY: Typed raise helper.
        Ok(None) => unsafe { abi::exc::pon_raise_key_error(key) },
        Err(()) => ptr::null_mut(),
    }
}

unsafe extern "C" fn wkd_ass_subscript_slot(object: *mut PyObject, key: *mut PyObject, value: *mut PyObject) -> c_int {
    let key = untag(key);
    if key.is_null() {
        return -1;
    }
    let Some(dict) = (unsafe { as_wkd(object) }) else {
        crate::thread_state::pon_err_set("WeakKeyDictionary receiver is invalid");
        return -1;
    };
    prune(dict);
    let existing = match find_entry(dict, key) {
        Ok(existing) => existing,
        Err(()) => return -1,
    };
    if value.is_null() {
        // Deletion (`del wkd[key]`).
        let Some(index) = existing else {
            // SAFETY: Typed raise helper.
            unsafe { abi::exc::pon_raise_key_error(key) };
            return -1;
        };
        dict.entries.remove(index);
        return 0;
    }
    let value = untag(value);
    match existing {
        Some(index) => dict.entries[index].1 = value,
        None => {
            let weak = make_weak_ref(key);
            if weak.is_null() {
                return -1;
            }
            dict.entries.push((weak, value));
        }
    }
    0
}

unsafe extern "C" fn wkd_iter(object: *mut PyObject) -> *mut PyObject {
    let Some(dict) = (unsafe { as_wkd(object) }) else {
        return fail("WeakKeyDictionary receiver is invalid");
    };
    let keys = live_keys(dict);
    if keys.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: Iterator helper follows the NULL-sentinel error contract.
    unsafe { abi::pon_get_iter(keys, ptr::null_mut()) }
}

unsafe extern "C" fn wkd_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { unicode_text(untag(name)) }) else {
        return fail("attribute name must be str");
    };
    if unsafe { as_wkd(object) }.is_none() {
        return fail("WeakKeyDictionary receiver is invalid");
    }
    match name_text {
        "keys" => bound_method(object, name_text, wkd_keys_method),
        "pop" => bound_method(object, name_text, wkd_pop_method),
        "get" => bound_method(object, name_text, wkd_get_method),
        // Unknown attributes stay loud so the next stdlib frontier surfaces.
        _ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
    }
}

fn bound_method(receiver: *mut PyObject, name: &str, entry: BuiltinFn) -> *mut PyObject {
    // SAFETY: `entry` is a live builtin entry point with the runtime calling convention.
    let function = unsafe { abi::pon_make_function(entry as *const u8, super::builtins_mod::VARIADIC_ARITY, intern(name)) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match crate::types::method::new_bound_method(function, receiver) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => fail(message),
    }
}

// ---------------------------------------------------------------------------
// WeakKeyDictionary methods (receiver rides in args[0] via the bound method)

unsafe fn wkd_receiver_and_args<'a>(
    argv: *mut *mut PyObject,
    argc: usize,
    method: &str,
) -> Result<(&'a mut PyWeakKeyDict, &'a [*mut PyObject]), *mut PyObject> {
    if argv.is_null() {
        return Err(fail(format!("WeakKeyDictionary.{method} received a NULL argv pointer")));
    }
    // SAFETY: The caller passed `argc` live argument slots.
    let args = unsafe { std::slice::from_raw_parts(argv, argc) };
    let Some((&receiver, rest)) = args.split_first() else {
        return Err(fail(format!("WeakKeyDictionary.{method} requires a receiver")));
    };
    let Some(dict) = (unsafe { as_wkd(receiver) }) else {
        return Err(fail(format!("WeakKeyDictionary.{method} receiver is invalid")));
    };
    Ok((dict, rest))
}

unsafe extern "C" fn wkd_keys_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (dict, args) = match unsafe { wkd_receiver_and_args(argv, argc, "keys") } {
        Ok(pair) => pair,
        Err(raised) => return raised,
    };
    if !args.is_empty() {
        return raise_type_error("keys() takes no arguments");
    }
    live_keys(dict)
}

unsafe extern "C" fn wkd_pop_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (dict, args) = match unsafe { wkd_receiver_and_args(argv, argc, "pop") } {
        Ok(pair) => pair,
        Err(raised) => return raised,
    };
    let (key, default) = match args {
        &[key] => (untag(key), None),
        &[key, default] => (untag(key), Some(untag(default))),
        _ => return raise_type_error("pop() expected 1 or 2 arguments"),
    };
    if key.is_null() {
        return ptr::null_mut();
    }
    prune(dict);
    match find_entry(dict, key) {
        Ok(Some(index)) => dict.entries.remove(index).1,
        Ok(None) => match default {
            Some(default) => default,
            // SAFETY: Typed raise helper.
            None => unsafe { abi::exc::pon_raise_key_error(key) },
        },
        Err(()) => ptr::null_mut(),
    }
}

unsafe extern "C" fn wkd_get_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (dict, args) = match unsafe { wkd_receiver_and_args(argv, argc, "get") } {
        Ok(pair) => pair,
        Err(raised) => return raised,
    };
    let (key, default) = match args {
        &[key] => (untag(key), none()),
        &[key, default] => (untag(key), untag(default)),
        _ => return raise_type_error("get() expected 1 or 2 arguments"),
    };
    if key.is_null() {
        return ptr::null_mut();
    }
    prune(dict);
    match find_entry(dict, key) {
        Ok(Some(index)) => dict.entries[index].1,
        Ok(None) => default,
        Err(()) => ptr::null_mut(),
    }
}

// ---------------------------------------------------------------------------
// WeakValueDictionary

/// Value-weak mirror of [`PyWeakKeyDict`]: `logging` keeps its named-handler
/// map in one (`_handlers = weakref.WeakValueDictionary()`).
#[repr(C)]
struct PyWeakValueDict {
    ob_base: PyObjectHeader,
    /// `(strong key, weak value ref)` pairs; dead values prune lazily.
    entries: Vec<(*mut PyObject, *mut PyObject)>,
}

static WVD_MAPPING: PyMappingMethods = PyMappingMethods {
    mp_length: Some(wvd_len_slot),
    mp_subscript: Some(wvd_subscript_slot),
    mp_ass_subscript: Some(wvd_ass_subscript_slot),
};

static WVD_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(
        abi::runtime_type_type().cast_const(),
        "weakref.WeakValueDictionary",
        std::mem::size_of::<PyWeakValueDict>(),
    );
    ty.tp_base = abi::runtime_global(intern("object"))
        .map_or(ptr::null_mut(), |object| object.cast::<PyType>());
    ty.tp_new = Some(wvd_new);
    ty.tp_getattro = Some(wvd_getattro);
    ty.tp_bool = Some(wvd_bool);
    ty.tp_iter = Some(wvd_iter);
    ty.tp_as_mapping = ptr::addr_of!(WVD_MAPPING).cast_mut();
    Box::into_raw(Box::new(ty)) as usize
});

fn wvd_type() -> *mut PyType {
    *WVD_TYPE as *mut PyType
}

/// Every WeakValueDictionary allocation, for GC root reporting (the
/// [`REGISTRY`] pattern).
static WVD_REGISTRY: Mutex<Vec<usize>> = Mutex::new(Vec::new());

unsafe fn as_wvd<'a>(object: *mut PyObject) -> Option<&'a mut PyWeakValueDict> {
    let object = untag(object);
    if object.is_null() {
        return None;
    }
    // SAFETY: A non-NULL heap object carries a live header.
    if unsafe { (*object).ob_type } != wvd_type().cast_const() {
        return None;
    }
    Some(unsafe { &mut *object.cast::<PyWeakValueDict>() })
}

/// Drops entries whose value referent was cleared by the collector.
fn wvd_prune(dict: &mut PyWeakValueDict) {
    dict.entries
        .retain(|&(_, weak)| !unsafe { crate::types::weakref::weakref_target(weak) }.is_null());
}

/// Index of the live entry whose key equals `key` (identity fast path, then
/// rich `==`).  `Err(())` propagates a comparison failure.
fn wvd_find_entry(dict: &PyWeakValueDict, key: *mut PyObject) -> Result<Option<usize>, ()> {
    for (index, &(stored, weak)) in dict.entries.iter().enumerate() {
        // SAFETY: Stored refs are live weakref objects (rooted by the registry).
        if unsafe { crate::types::weakref::weakref_target(weak) }.is_null() {
            continue;
        }
        if stored == key {
            return Ok(Some(index));
        }
        // SAFETY: Rich-compare helper follows the NULL-sentinel error contract.
        let equal = unsafe { abi::object::pon_rich_compare(RICH_EQ, stored, key, ptr::null_mut()) };
        if equal.is_null() {
            return Err(());
        }
        match unsafe { abi::pon_is_true(equal) } {
            1 => return Ok(Some(index)),
            0 => {}
            _ => return Err(()),
        }
    }
    Ok(None)
}

/// Materialized list of keys with live values (divergence: CPython returns a
/// view).
fn wvd_live_keys(dict: &mut PyWeakValueDict) -> *mut PyObject {
    wvd_prune(dict);
    let keys = dict.entries.iter().map(|&(key, _)| key).collect::<Vec<_>>();
    super::builtins_mod::alloc_list(keys)
}

// ---------------------------------------------------------------------------
// WeakValueDictionary slots

unsafe extern "C" fn wvd_new(_cls: *mut PyType, args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
    let positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
        Ok(positional) => positional,
        Err(message) => return fail(message),
    };
    if !positional.is_empty() {
        // Loud frontier: the optional `dict` seed argument is unimplemented.
        return raise_type_error("WeakValueDictionary() with a dict argument is not supported");
    }
    let object = Box::into_raw(Box::new(PyWeakValueDict {
        ob_base: PyObjectHeader::new(wvd_type()),
        entries: Vec::new(),
    }));
    WVD_REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner()).push(object as usize);
    object.cast::<PyObject>()
}

unsafe extern "C" fn wvd_len_slot(object: *mut PyObject) -> isize {
    let Some(dict) = (unsafe { as_wvd(object) }) else {
        crate::thread_state::pon_err_set("WeakValueDictionary receiver is invalid");
        return -1;
    };
    wvd_prune(dict);
    dict.entries.len() as isize
}

unsafe extern "C" fn wvd_bool(object: *mut PyObject) -> c_int {
    let Some(dict) = (unsafe { as_wvd(object) }) else {
        crate::thread_state::pon_err_set("WeakValueDictionary receiver is invalid");
        return -1;
    };
    wvd_prune(dict);
    c_int::from(!dict.entries.is_empty())
}

unsafe extern "C" fn wvd_subscript_slot(object: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    let key = untag(key);
    if key.is_null() {
        return ptr::null_mut();
    }
    let Some(dict) = (unsafe { as_wvd(object) }) else {
        return fail("WeakValueDictionary receiver is invalid");
    };
    match wvd_find_entry(dict, key) {
        // SAFETY: The find step proved the referent live.
        Ok(Some(index)) => unsafe { crate::types::weakref::weakref_target(dict.entries[index].1) },
        // SAFETY: Typed raise helper.
        Ok(None) => unsafe {
            abi::exc::pon_raise_key_error(key);
            ptr::null_mut()
        },
        Err(()) => ptr::null_mut(),
    }
}

unsafe extern "C" fn wvd_ass_subscript_slot(object: *mut PyObject, key: *mut PyObject, value: *mut PyObject) -> c_int {
    let key = untag(key);
    if key.is_null() {
        return -1;
    }
    let Some(dict) = (unsafe { as_wvd(object) }) else {
        crate::thread_state::pon_err_set("WeakValueDictionary receiver is invalid");
        return -1;
    };
    wvd_prune(dict);
    let existing = match wvd_find_entry(dict, key) {
        Ok(existing) => existing,
        Err(()) => return -1,
    };
    if value.is_null() {
        // Deletion (`del wvd[key]`).
        let Some(index) = existing else {
            // SAFETY: Typed raise helper.
            unsafe { abi::exc::pon_raise_key_error(key) };
            return -1;
        };
        dict.entries.remove(index);
        return 0;
    }
    let weak = make_weak_ref(untag(value));
    if weak.is_null() {
        return -1;
    }
    match existing {
        Some(index) => dict.entries[index].1 = weak,
        None => dict.entries.push((key, weak)),
    }
    0
}

unsafe extern "C" fn wvd_iter(object: *mut PyObject) -> *mut PyObject {
    let Some(dict) = (unsafe { as_wvd(object) }) else {
        return fail("WeakValueDictionary receiver is invalid");
    };
    let keys = wvd_live_keys(dict);
    if keys.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: Iterator helper follows the NULL-sentinel error contract.
    unsafe { abi::pon_get_iter(keys, ptr::null_mut()) }
}

unsafe extern "C" fn wvd_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { unicode_text(untag(name)) }) else {
        return fail("attribute name must be str");
    };
    if unsafe { as_wvd(object) }.is_none() {
        return fail("WeakValueDictionary receiver is invalid");
    }
    match name_text {
        "keys" => bound_method(object, name_text, wvd_keys_method),
        "pop" => bound_method(object, name_text, wvd_pop_method),
        "get" => bound_method(object, name_text, wvd_get_method),
        // Unknown attributes stay loud so the next stdlib frontier surfaces.
        _ => {
            let message = format!("'WeakValueDictionary' object has no attribute '{name_text}'");
            abi::exc::raise_kind_error_text(ExceptionKind::AttributeError, &message)
        }
    }
}

// ---------------------------------------------------------------------------
// WeakValueDictionary methods (receiver rides in args[0] via the bound method)

unsafe fn wvd_receiver_and_args<'a>(
    argv: *mut *mut PyObject,
    argc: usize,
    method: &str,
) -> Result<(&'a mut PyWeakValueDict, &'a [*mut PyObject]), *mut PyObject> {
    if argv.is_null() {
        return Err(fail(format!("WeakValueDictionary.{method} received a NULL argv pointer")));
    }
    // SAFETY: The call helper supplies `argv` with `argc` entries.
    let args = unsafe { core::slice::from_raw_parts(argv.cast_const(), argc) };
    let Some((&receiver, rest)) = args.split_first() else {
        return Err(fail(format!("WeakValueDictionary.{method} requires a receiver")));
    };
    let Some(dict) = (unsafe { as_wvd(receiver) }) else {
        return Err(fail(format!("WeakValueDictionary.{method} receiver is invalid")));
    };
    Ok((dict, rest))
}

unsafe extern "C" fn wvd_keys_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (dict, args) = match unsafe { wvd_receiver_and_args(argv, argc, "keys") } {
        Ok(pair) => pair,
        Err(error) => return error,
    };
    if !args.is_empty() {
        return raise_type_error("keys() takes no arguments");
    }
    wvd_live_keys(dict)
}

/// `WeakValueDictionary.pop(key[, default])`.
unsafe extern "C" fn wvd_pop_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (dict, args) = match unsafe { wvd_receiver_and_args(argv, argc, "pop") } {
        Ok(pair) => pair,
        Err(error) => return error,
    };
    if args.is_empty() || args.len() > 2 {
        return raise_type_error("pop(key[, default]) takes 1 or 2 arguments");
    }
    let key = untag(args[0]);
    wvd_prune(dict);
    match wvd_find_entry(dict, key) {
        Ok(Some(index)) => {
            let (_, weak) = dict.entries.remove(index);
            // SAFETY: The find step proved the referent live.
            unsafe { crate::types::weakref::weakref_target(weak) }
        }
        Ok(None) => match args.get(1) {
            Some(&default) => default,
            // SAFETY: Typed raise helper.
            None => unsafe {
                abi::exc::pon_raise_key_error(key);
                ptr::null_mut()
            },
        },
        Err(()) => ptr::null_mut(),
    }
}

/// `WeakValueDictionary.get(key, default=None)`.
unsafe extern "C" fn wvd_get_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (dict, args) = match unsafe { wvd_receiver_and_args(argv, argc, "get") } {
        Ok(pair) => pair,
        Err(error) => return error,
    };
    if args.is_empty() || args.len() > 2 {
        return raise_type_error("get(key, default=None) takes 1 or 2 arguments");
    }
    let key = untag(args[0]);
    let default = args.get(1).copied().unwrap_or_else(none);
    match wvd_find_entry(dict, key) {
        // SAFETY: The find step proved the referent live.
        Ok(Some(index)) => unsafe { crate::types::weakref::weakref_target(dict.entries[index].1) },
        Ok(None) => default,
        Err(()) => ptr::null_mut(),
    }
}
