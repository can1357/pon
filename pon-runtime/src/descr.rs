//! Descriptor protocol and generic attribute access.

use core::ffi::c_int;
use core::{mem, ptr};
use std::sync::LazyLock;

use crate::abi;
use crate::feedback::{ATTR_DESCR_BLIND, ATTR_DESCR_PROBE_DICT, AttrCacheKind, AttrIC, FeedbackCell};
use crate::intern;
use crate::mro;
use crate::object::{PyObject, PyType, update_slot_from_dunder};
use crate::sync;
use crate::thread_state::pon_err_set;
use crate::types::{dict, type_::{self, PyClassDict, PyHeapInstance}, typealias};

fn raise_attr_error(message: impl Into<String>) -> *mut PyObject {
    pon_err_set(message);
    ptr::null_mut()
}

fn raise_attr_status(message: impl Into<String>) -> c_int {
    pon_err_set(message);
    -1
}

/// Type of `object` for descriptor probing, or NULL when `object` carries no
/// dereferenceable type.
///
/// Tagged immediates report NULL rather than being dereferenced: every caller
/// already routes NULL through its non-descriptor path (an immediate found in
/// a class dict is a plain value — it never has `__get__`/`__set__`), which is
/// the tag-discipline contract for this module's entry points.
unsafe fn object_type(object: *mut PyObject) -> *mut PyType {
    if object.is_null() || !crate::tag::is_heap(object) {
        ptr::null_mut()
    } else {
        unsafe { (*object).ob_type.cast_mut() }
    }
}

unsafe fn name_id(name: *mut PyObject) -> Option<u32> {
    let text = unsafe { type_::unicode_text(name)? };
    Some(intern::intern(text))
}

unsafe fn dict_from_ptr(dict: *mut PyObject) -> Option<&'static mut PyClassDict> {
    if dict.is_null() {
        None
    } else {
        Some(unsafe { &mut *dict.cast::<PyClassDict>() })
    }
}

fn raise_type_status(message: impl AsRef<str>) -> c_int {
    let message = message.as_ref();
    unsafe {
        abi::exc::pon_raise_type_error(message.as_ptr(), message.len());
    }
    -1
}

fn raise_missing_attr_status(object: *mut PyObject, name_id: u32) -> c_int {
    let _ = unsafe { abi::pon_raise_attribute_error(object, name_id) };
    -1
}

unsafe fn object_type_display(object: *mut PyObject) -> String {
    if object.is_null() {
        return "NULL".to_owned();
    }
    let ty = unsafe { object_type(object) };
    if ty.is_null() {
        "object".to_owned()
    } else {
        unsafe { (*ty).name() }.to_owned()
    }
}

pub(crate) unsafe fn class_dict_to_dict(class_dict: *mut PyClassDict) -> *mut PyObject {
    if class_dict.is_null() {
        return ptr::null_mut();
    }
    let out = unsafe { abi::map::pon_build_map(ptr::null_mut(), 0) };
    if out.is_null() {
        return ptr::null_mut();
    }
    for (name, value) in unsafe { (&*class_dict).iter() } {
        let Some(spelling) = intern::resolve(name) else {
            continue;
        };
        let key = unsafe { abi::pon_const_str(spelling.as_ptr(), spelling.len()) };
        if key.is_null() {
            return ptr::null_mut();
        }
        if unsafe { abi::map::pon_dict_set_item_status(out, key, value) } < 0 {
            return ptr::null_mut();
        }
    }
    out
}

unsafe fn synthetic_builtin_descriptor() -> *mut PyObject {
    unsafe { abi::pon_load_global(intern::intern("len"), ptr::null_mut()) }
}

unsafe fn set_str_key(dict: *mut PyObject, name: &str, value: *mut PyObject) -> bool {
    if dict.is_null() || value.is_null() {
        return false;
    }
    let key = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
    if key.is_null() {
        return false;
    }
    unsafe { abi::map::pon_dict_set_item_status(dict, key, value) >= 0 }
}

/// `object.__eq__(self, other)` default: identity `True`, else `NotImplemented`.
///
/// Raw (possibly tagged) pointers are compared: identical tagged immediates
/// ARE the same value, mirroring CPython's small-int/str caches making
/// identity hold for equal immediates.
unsafe extern "C" fn object_dunder_eq_native(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { object_dunder_cmp_args(argv, argc, "__eq__") } {
        Ok(args) => args,
        Err(raised) => return raised,
    };
    if args[0] == args[1] {
        return unsafe { abi::pon_const_bool(1) };
    }
    unsafe { abi::pon_not_implemented() }
}

/// `object.__ne__(self, other)` default: delegate to self's `__eq__` and
/// invert, passing `NotImplemented` through (CPython `object_richcompare`).
unsafe extern "C" fn object_dunder_ne_native(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { object_dunder_cmp_args(argv, argc, "__ne__") } {
        Ok(args) => args,
        Err(raised) => return raised,
    };
    let (raw_self, other) = (args[0], args[1]);
    let this = crate::tag::untag_arg(raw_self);
    if this.is_null() {
        return ptr::null_mut();
    }
    let self_ty = unsafe { object_type(this) };
    let eq_descr = if self_ty.is_null() {
        ptr::null_mut()
    } else {
        unsafe { lookup_in_type(self_ty, intern::intern("__eq__")) }
    };
    if eq_descr.is_null() {
        // No `__eq__` anywhere in the MRO: apply object's identity default.
        if raw_self == other {
            return unsafe { abi::pon_const_bool(0) };
        }
        return unsafe { abi::pon_not_implemented() };
    }
    let bound = unsafe { descriptor_get(eq_descr, this, self_ty) };
    if bound.is_null() {
        return ptr::null_mut();
    }
    let mut argv = [other];
    let result = unsafe { abi::pon_call(bound, argv.as_mut_ptr(), argv.len()) };
    if result.is_null() || result == unsafe { abi::pon_not_implemented() } {
        return result;
    }
    match unsafe { abi::object::pon_is_true(result) } {
        -1 => ptr::null_mut(),
        truth => unsafe { abi::pon_const_bool(i32::from(truth == 0)) },
    }
}

/// Shape validation for the object-default comparison natives; raises the
/// CPython slot-wrapper TypeErrors.
unsafe fn object_dunder_cmp_args<'a>(
    argv: *mut *mut PyObject,
    argc: usize,
    name: &str,
) -> Result<&'a [*mut PyObject], *mut PyObject> {
    let message = if argv.is_null() && argc != 0 {
        format!("object.{name} received a null argv pointer")
    } else if argc == 0 {
        format!("descriptor '{name}' of 'object' object needs an argument")
    } else if argc != 2 {
        format!("{name} expected 1 argument, got {}", argc - 1)
    } else {
        return Ok(unsafe { core::slice::from_raw_parts(argv.cast_const(), argc) });
    };
    Err(unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) })
}

/// Last-resort type-level fallback for `object`'s slot methods: a full-MRO
/// miss on a TYPE receiver resolves `__eq__`/`__ne__` to fresh unbound
/// natives implementing object's defaults, mirroring CPython where these
/// wrappers live in `object`'s tp_dict at the end of every MRO
/// (`MutableMapping.__ne__` in collections resolves there).  Kept out of any
/// real tp_dict so instance-side rich-compare dispatch is untouched.
unsafe fn object_slot_method_fallback(name_id: u32) -> *mut PyObject {
    let entry = if name_id == intern::intern("__eq__") {
        object_dunder_eq_native as *const u8
    } else if name_id == intern::intern("__ne__") {
        object_dunder_ne_native as *const u8
    } else {
        return ptr::null_mut();
    };
    unsafe { abi::pon_make_function(entry, crate::builtins::variadic_arity(), name_id) }
}

unsafe fn type_dict_object(ty: *mut PyType) -> *mut PyObject {
    if ty.is_null() {
        return ptr::null_mut();
    }
    let dict = unsafe { (*ty).tp_dict.cast::<PyClassDict>() };
    let out = if dict.is_null() {
        unsafe { abi::map::pon_build_map(ptr::null_mut(), 0) }
    } else {
        unsafe { class_dict_to_dict(dict) }
    };
    if out.is_null() {
        return out;
    }
    if unsafe { (*ty).name() } == "dict" {
        let fromkeys = crate::native::builtins_mod::dict_fromkeys_function();
        if unsafe { !set_str_key(out, "fromkeys", fromkeys) } {
            return ptr::null_mut();
        }
    }
    out
}

unsafe fn synthetic_type_attr(ty: *mut PyType, name_id: u32) -> *mut PyObject {
    if ty.is_null() {
        return ptr::null_mut();
    }
    let type_name = unsafe { (*ty).name() };
    // Builtin type receivers materialize their native tp_dict method surface
    // on first type-level access: the unbound `dict.__setitem__(d, k, v)` /
    // `list.append(lst, x)` patterns (collections.OrderedDict default args)
    // then resolve through the regular MRO lookup below.
    if type_name == "dict" {
        dict::ensure_dict_subclass_methods_installed();
    } else if type_name == "list" {
        crate::abi::seq::ensure_list_type_methods_installed(ty);
    } else if type_name == "str" {
        crate::abi::str_::ensure_str_type_methods_installed(ty);
    } else if type_name == "bytes" {
        crate::abi::str_::ensure_bytes_type_methods_installed(ty);
    } else if type_name == "bytearray" {
        crate::abi::str_::ensure_bytearray_type_methods_installed(ty);
    }
    if (type_name == "dict" || unsafe { dict::type_is_dict_subclass(ty) }) && name_id == intern::intern("fromkeys") {
        return crate::native::builtins_mod::dict_fromkeys_function();
    }
    if type_name == "int" && name_id == intern::intern("from_bytes") {
        return crate::types::int::from_bytes_function();
    }
    let is_known_descriptor = type_name == "object" && name_id == intern::intern("__init__");
    if is_known_descriptor {
        return unsafe { synthetic_builtin_descriptor() };
    }
    ptr::null_mut()
}

unsafe fn set_instance_dict(object: *mut PyObject, value: *mut PyObject) -> c_int {
    let instance = object.cast::<PyHeapInstance>();
    if unsafe { (*instance).dict.is_null() } {
        return raise_missing_attr_status(object, intern::intern("__dict__"));
    }
    let replacement = type_::new_namespace();
    if !value.is_null() {
        if unsafe { !dict::is_dict(value) } {
            let got = unsafe { object_type_display(value) };
            return raise_type_status(format!("__dict__ must be set to a dictionary, not a '{got}'"));
        }
        let entries = match unsafe { dict::dict_entries_snapshot(value) } {
            Ok(entries) => entries,
            Err(message) => return raise_type_status(message),
        };
        for entry in entries {
            if let Some(text) = unsafe { type_::unicode_text(entry.key) } {
                unsafe {
                    (&mut *replacement).set(intern::intern(text), entry.value);
                }
            }
        }
    }
    unsafe {
        (*instance).dict = replacement;
    }
    0
}

unsafe fn heap_type_layout_compatible(current: *mut PyType, replacement: *mut PyType) -> bool {
    if current.is_null() || replacement.is_null() {
        return false;
    }
    let current = unsafe { &*current };
    let replacement = unsafe { &*replacement };
    let heap_basicsize = mem::size_of::<PyHeapInstance>();
    current.tp_basicsize == heap_basicsize
        && replacement.tp_basicsize == heap_basicsize
        && current.tp_itemsize == replacement.tp_itemsize
        && current.tp_dictoffset == replacement.tp_dictoffset
}

unsafe fn set_instance_class(object: *mut PyObject, current_ty: *mut PyType, value: *mut PyObject) -> c_int {
    if value.is_null() {
        return raise_type_status("can't delete __class__ attribute");
    }
    if unsafe { !is_type_object(value) } {
        let got = unsafe { object_type_display(value) };
        return raise_type_status(format!("__class__ must be set to a class, not '{got}' object"));
    }
    let replacement = value.cast::<PyType>();
    if unsafe { !heap_type_layout_compatible(current_ty, replacement) } {
        return raise_type_status("__class__ assignment only supported for mutable types or ModuleType subclasses");
    }
    unsafe {
        (*object).ob_type = replacement;
    }
    0
}

/// Look up `name` in `ty` and its MRO without invoking descriptor binding.
#[must_use]
pub unsafe fn lookup_in_type(ty: *mut PyType, name: u32) -> *mut PyObject {
    if ty.is_null() {
        return ptr::null_mut();
    }
    for cls in unsafe { mro::mro_entries(ty) } {
        if cls.is_null() {
            continue;
        }
        let dict = unsafe { (*cls).tp_dict };
        if let Some(dict) = unsafe { dict_from_ptr(dict) } {
            if let Some(value) = dict.get(name) {
                return value;
            }
        }
    }
    ptr::null_mut()
}

/// Invoke `descr.__get__(obj, owner)` when a descriptor slot or Python dunder exists.
#[must_use]
pub unsafe fn descriptor_get(descr: *mut PyObject, obj: *mut PyObject, owner: *mut PyType) -> *mut PyObject {
    if descr.is_null() {
        return ptr::null_mut();
    }
    let ty = unsafe { object_type(descr) };
    if ty.is_null() {
        return descr;
    }
    if let Some(get) = unsafe { (*ty).tp_descr_get } {
        return unsafe { get(descr, obj, owner.cast::<PyObject>()) };
    }
    let get = unsafe { lookup_in_type(ty, intern::intern("__get__")) };
    if get.is_null() {
        return descr;
    }
    let obj_arg = if obj.is_null() {
        unsafe { abi::pon_none() }
    } else {
        obj
    };
    if obj_arg.is_null() {
        return ptr::null_mut();
    }
    let owner_arg = if owner.is_null() {
        unsafe { abi::pon_none() }
    } else {
        owner.cast::<PyObject>()
    };
    if owner_arg.is_null() {
        return ptr::null_mut();
    }
    let mut argv = [descr, obj_arg, owner_arg];
    unsafe { abi::pon_call(get, argv.as_mut_ptr(), argv.len()) }
}

/// Invoke `descr.__set__`/`__delete__` when a descriptor setter slot or Python dunder exists.
pub unsafe fn descriptor_set(descr: *mut PyObject, obj: *mut PyObject, value: *mut PyObject) -> c_int {
    if descr.is_null() {
        return raise_attr_status("descriptor is NULL");
    }
    let ty = unsafe { object_type(descr) };
    if ty.is_null() {
        return raise_attr_status("descriptor has no type");
    }
    if let Some(set) = unsafe { (*ty).tp_descr_set } {
        return unsafe { set(descr, obj, value) };
    }
    let dunder = if value.is_null() { "__delete__" } else { "__set__" };
    let method = unsafe { lookup_in_type(ty, intern::intern(dunder)) };
    if method.is_null() {
        return raise_attr_status(if value.is_null() {
            "can't delete attribute"
        } else {
            "attribute is read-only"
        });
    }
    let result = if value.is_null() {
        let mut argv = [descr, obj];
        unsafe { abi::pon_call(method, argv.as_mut_ptr(), argv.len()) }
    } else {
        let mut argv = [descr, obj, value];
        unsafe { abi::pon_call(method, argv.as_mut_ptr(), argv.len()) }
    };
    if result.is_null() { -1 } else { 0 }
}

#[must_use]
pub(crate) unsafe fn is_data_descriptor(descr: *mut PyObject) -> bool {
    if descr.is_null() {
        return false;
    }
    let ty = unsafe { object_type(descr) };
    if ty.is_null() {
        return false;
    }
    unsafe {
        (*ty).tp_descr_set.is_some()
            || !lookup_in_type(ty, intern::intern("__set__")).is_null()
            || !lookup_in_type(ty, intern::intern("__delete__")).is_null()
    }
}

unsafe fn is_type_object(object: *mut PyObject) -> bool {
    if object.is_null() {
        return false;
    }
    let meta = unsafe { object_type(object) };
    if meta.is_null() {
        return false;
    }
    unsafe { (*meta).name() == "type" || mro::mro_entries(meta).iter().any(|ty| !ty.is_null() && (**ty).name() == "type") }
}

unsafe fn instance_dict(object: *mut PyObject) -> *mut PyClassDict {
    if object.is_null() {
        return ptr::null_mut();
    }
    if unsafe { is_type_object(object) } {
        let ty = object.cast::<PyType>();
        return unsafe { (*ty).tp_dict.cast::<PyClassDict>() };
    }
    let instance = object.cast::<PyHeapInstance>();
    unsafe { (*instance).dict }
}

/// Generic CPython-order attribute lookup: data descriptors, instance/class
/// namespace, non-data descriptors, then missing-attribute error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn generic_get_attr(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    if object.is_null() {
        return raise_attr_error("attribute receiver is NULL");
    }
    let Some(name_id) = (unsafe { name_id(name) }) else {
        return raise_attr_error("attribute name must be a string");
    };
    unsafe { generic_get_attr_cached(object, name_id, ptr::null()) }
}

/// IC-aware core of [`generic_get_attr`] (J0.3 tier-0 consultation).
///
/// With a non-NULL `cell`, a validated [`AttrIC`] record replays the cached
/// translation directly — no MRO walk, no data-descriptor re-check — and a
/// miss runs the full CPython-order lookup, then publishes a fresh record.
///
/// Records are published ONLY for non-type receivers.  `is_type_object` is a
/// pure function of the receiver's type (the cell's identity guard), so a hit
/// proves the receiver is a plain heap instance of the guarded class and the
/// `PyHeapInstance` dict cast below is exactly the slow path's
/// `instance_dict` cast.
///
/// Known accepted staleness (mirrors the J0.3 pin and CPython): the cached
/// descriptor's data-ness is guarded by the RECEIVER type's version only.
/// Deleting `__set__` from the descriptor's own class flips its precedence
/// without bumping the receiver type; the record keeps replaying the old
/// precedence until any receiver-type mutation re-records it.
pub unsafe fn generic_get_attr_cached(object: *mut PyObject, name_id: u32, cell: *const FeedbackCell) -> *mut PyObject {
    if object.is_null() {
        return raise_attr_error("attribute receiver is NULL");
    }
    let obj_ty = unsafe { object_type(object) };
    if obj_ty.is_null() {
        return raise_attr_error("attribute receiver has no type");
    }

    if let Some(cell) = unsafe { cell.as_ref() } {
        if let Some(ic) = cell.attr_hit(obj_ty as usize, unsafe { (*obj_ty).version() }) {
            match ic.kind {
                AttrCacheKind::DictOffset => {
                    // The record proved the name resolves from the instance
                    // dict (no shadowing data descriptor at this version).
                    let dict = unsafe { (*object.cast::<PyHeapInstance>()).dict };
                    if !dict.is_null() {
                        if let Some(value) = unsafe { (&*dict).get(name_id) } {
                            return value;
                        }
                    }
                    // Instance mutation is not version-guarded: the name is
                    // gone from this instance — take the slow path (which
                    // re-records the new translation).
                }
                AttrCacheKind::Descriptor => {
                    if ic.offset == ATTR_DESCR_PROBE_DICT {
                        let dict = unsafe { (*object.cast::<PyHeapInstance>()).dict };
                        if !dict.is_null() {
                            if let Some(value) = unsafe { (&*dict).get(name_id) } {
                                return value;
                            }
                        }
                    }
                    return unsafe { descriptor_get(ic.descriptor as *mut PyObject, object, obj_ty) };
                }
                // Slot records are a tier-1 (O4) shape; tier-0 never
                // publishes them, so treat one as a miss.
                AttrCacheKind::Slot => {}
            }
        }
    }

    let is_type = unsafe { is_type_object(object) };
    if is_type && (name_id == intern::intern("__name__") || name_id == intern::intern("__qualname__")) {
        // An explicit class-body assignment (`__qualname__ = ...`) lands in
        // tp_dict and wins over the synthetic value, matching CPython.
        let dict = unsafe { (*object.cast::<PyType>()).tp_dict.cast::<PyClassDict>() };
        if !dict.is_null() {
            if let Some(value) = unsafe { (&*dict).get(name_id) } {
                return value;
            }
        }
        let full = unsafe { (*object.cast::<PyType>()).name() };
        // CPython `type.__name__`: static tp_names are dotted
        // (`collections.deque`); the getter exposes only the tail component,
        // while `repr(type)` keeps the full dotted path.
        //
        // `__qualname__`: CPython's compiler threads lexical nesting into the
        // class body (`Outer.Inner`, `f.<locals>.C`); pon's frontend carries
        // no nesting info anywhere in the pipeline, so nested classes degrade
        // to their bare `__name__` here. Top-level classes (the common case,
        // and all unittest needs) are exact.
        let type_name = full.rsplit('.').next().unwrap_or(full);
        return unsafe { abi::pon_const_str(type_name.as_ptr(), type_name.len()) };
    }
    if is_type && name_id == intern::intern("__module__") {
        // Heap classes carry the defining module in tp_dict (class machinery
        // stores it from the namespace); static/native types are builtins,
        // matching CPython's default for C types.
        let dict = unsafe { (*object.cast::<PyType>()).tp_dict.cast::<PyClassDict>() };
        if !dict.is_null() {
            if let Some(value) = unsafe { (&*dict).get(name_id) } {
                return value;
            }
        }
        return unsafe { abi::pon_const_str("builtins".as_ptr(), "builtins".len()) };
    }
    if is_type && name_id == intern::intern("__dict__") {
        return unsafe { type_dict_object(object.cast::<PyType>()) };
    }
    if is_type && name_id == intern::intern("__mro__") {
        let mut entries = unsafe { mro::mro_entries(object.cast::<PyType>()) }
            .into_iter()
            .map(|ty| ty.cast::<PyObject>())
            .collect::<Vec<_>>();
        return unsafe {
            abi::seq::pon_build_tuple(
                if entries.is_empty() { ptr::null_mut() } else { entries.as_mut_ptr() },
                entries.len(),
            )
        };
    }
    if is_type && name_id == intern::intern("__subclasses__") {
        return unsafe { type_subclasses_method(object) };
    }
    if is_type {
        let value = unsafe { synthetic_type_attr(object.cast::<PyType>(), name_id) };
        if !value.is_null() {
            return value;
        }
    }
    if is_type && name_id == intern::intern("__annotate__") {
        // PEP 649: `__annotate__` is an own-dict-only class attribute — never
        // MRO-inherited (probed: `B.__annotate__ is None` for a subclass of
        // an annotated base).
        let dict = unsafe { (*object.cast::<PyType>()).tp_dict.cast::<PyClassDict>() };
        if !dict.is_null() {
            if let Some(value) = unsafe { (&*dict).get(name_id) } {
                return value;
            }
        }
        return unsafe { abi::pon_none() };
    }
    if is_type && name_id == intern::intern("__doc__") {
        // CPython `type.__doc__` getset: the class's OWN tp_dict entry or
        // None — docstrings are never MRO-inherited (`class B(A): pass` has
        // `B.__doc__ is None` even when A carries one).
        let dict = unsafe { (*object.cast::<PyType>()).tp_dict.cast::<PyClassDict>() };
        if !dict.is_null() {
            if let Some(value) = unsafe { (&*dict).get(name_id) } {
                return value;
            }
        }
        return unsafe { abi::pon_none() };
    }
    if is_type && name_id == intern::intern("__annotations__") {
        // PEP 649 lazy class annotations, shared with the
        // `type.__dict__['__annotations__']` getset descriptor below.
        return unsafe { type_annotations_get(object.cast::<PyType>()) };
    }

    // J0.3 capture discipline: the guard version is loaded BEFORE the slow
    // lookup, so a concurrent mutation makes the record miss, never lie.
    let version = unsafe { (*obj_ty).version() };

    let meta_descr = if is_type {
        unsafe { lookup_in_type(obj_ty, name_id) }
    } else {
        ptr::null_mut()
    };
    if unsafe { is_data_descriptor(meta_descr) } {
        return unsafe { descriptor_get(meta_descr, object, obj_ty) };
    }

    let class_descr = if is_type {
        unsafe { lookup_in_type(object.cast::<PyType>(), name_id) }
    } else {
        unsafe { lookup_in_type(obj_ty, name_id) }
    };
    if unsafe { is_data_descriptor(class_descr) } {
        if is_type {
            // CPython type_getattro: own-MRO hits on a type receiver bind as
            // `__get__(NULL, cls)` (classmethods bind the class, functions
            // and properties come back unbound).
            return unsafe { descriptor_get(class_descr, ptr::null_mut(), object.cast::<PyType>()) };
        }
        unsafe { record_attr_translation(cell, obj_ty, version, ATTR_DESCR_BLIND, class_descr) };
        return unsafe { descriptor_get(class_descr, object, obj_ty) };
    }

    if !is_type && name_id == intern::intern("__class__") {
        return obj_ty.cast::<PyObject>();
    }
    if !is_type && name_id == intern::intern("__dict__") {
        let dict = unsafe { instance_dict(object) };
        if dict.is_null() {
            return unsafe { abi::pon_raise_attribute_error(object, name_id) };
        }
        return unsafe { class_dict_to_dict(dict) };
    }

    if is_type {
        if !class_descr.is_null() {
            return unsafe { descriptor_get(class_descr, ptr::null_mut(), object.cast::<PyType>()) };
        }
        if !meta_descr.is_null() {
            return unsafe { descriptor_get(meta_descr, object, obj_ty) };
        }
        let fallback = unsafe { object_slot_method_fallback(name_id) };
        if !fallback.is_null() {
            return fallback;
        }
        return unsafe { abi::pon_raise_attribute_error(object, name_id) };
    }

    let dict = unsafe { instance_dict(object) };
    if !dict.is_null() {
        if let Some(value) = unsafe { (&*dict).get(name_id) } {
            if !is_type {
                unsafe { record_dict_translation(cell, obj_ty, version) };
            }
            return value;
        }
    }

    if !class_descr.is_null() {
        unsafe { record_attr_translation(cell, obj_ty, version, ATTR_DESCR_PROBE_DICT, class_descr) };
        return unsafe { descriptor_get(class_descr, object, obj_ty) };
    }

    // CPython `slot_tp_getattr_hook`: once regular resolution misses on an
    // instance receiver, a Python-level `__getattr__` on the type is the
    // last-chance fallback (`_WritelnDecorator`-style delegation wrappers).
    let getattr_hook = unsafe { lookup_in_type(obj_ty, intern::intern("__getattr__")) };
    if !getattr_hook.is_null() {
        let bound = unsafe { descriptor_get(getattr_hook, object, obj_ty) };
        if bound.is_null() {
            return ptr::null_mut();
        }
        let spelling = intern::resolve(name_id).unwrap_or_else(|| format!("<interned:{name_id}>"));
        let name_object = unsafe { abi::pon_const_str(spelling.as_ptr(), spelling.len()) };
        if name_object.is_null() {
            return ptr::null_mut();
        }
        let mut argv = [name_object];
        return unsafe { abi::pon_call(bound, argv.as_mut_ptr(), 1) };
    }
    unsafe { abi::pon_raise_attribute_error(object, name_id) }
}

/// Publishes a descriptor-shaped [`AttrIC`] record (`mode` selects blind vs
/// probe-dict-first replay; see [`ATTR_DESCR_BLIND`]/[`ATTR_DESCR_PROBE_DICT`]).
unsafe fn record_attr_translation(cell: *const FeedbackCell, ty: *mut PyType, version: u32, mode: u32, descr: *mut PyObject) {
    if let Some(cell) = unsafe { cell.as_ref() } {
        cell.record_attr(
            ty as usize,
            AttrIC {
                type_version: version,
                kind: AttrCacheKind::Descriptor,
                offset: mode,
                descriptor: descr as usize,
            },
        );
    }
}

/// Publishes an instance-dict [`AttrIC`] record ("MRO walk skippable").
unsafe fn record_dict_translation(cell: *const FeedbackCell, ty: *mut PyType, version: u32) {
    if let Some(cell) = unsafe { cell.as_ref() } {
        cell.record_attr(
            ty as usize,
            AttrIC {
                type_version: version,
                kind: AttrCacheKind::DictOffset,
                offset: 0,
                descriptor: 0,
            },
        );
    }
}

/// Generic attribute assignment/deletion with data-descriptor and slots support.
///
/// Type-receiver mutation branches call [`sync::type_modified`] AFTER the
/// write (J0.3 §6 sites #1/#2), invalidating every AttrIC guarding the type or
/// a transitive subclass.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn generic_set_attr(object: *mut PyObject, name: *mut PyObject, value: *mut PyObject) -> c_int {
    if object.is_null() {
        return raise_attr_status("attribute receiver is NULL");
    }
    let Some(name_id) = (unsafe { name_id(name) }) else {
        return raise_attr_status("attribute name must be a string");
    };
    let obj_ty = unsafe { object_type(object) };
    if obj_ty.is_null() {
        return raise_attr_status("attribute receiver has no type");
    }
    let is_type = unsafe { is_type_object(object) };
    if !is_type && name_id == intern::intern("__dict__") {
        return unsafe { set_instance_dict(object, value) };
    }
    if !is_type && name_id == intern::intern("__class__") {
        return unsafe { set_instance_class(object, obj_ty, value) };
    }


    let descr = unsafe { lookup_in_type(obj_ty, name_id) };
    if unsafe { is_data_descriptor(descr) } {
        let status = unsafe { descriptor_set(descr, object, value) };
        if status == 0 && is_type {
            // J0.3 §6 #7 contract: a metatype data descriptor just mutated
            // type state through `SomeClass.attr = v`.
            sync::type_modified(object.cast::<PyType>());
        }
        return status;
    }

    // §6 explicit non-site: instance slots are instance state, never bumped.
    if !is_type && unsafe { type_::instance_set_slot(object.cast::<PyHeapInstance>(), name_id, value) } {
        return 0;
    }

    let dict = unsafe { instance_dict(object) };
    if dict.is_null() {
        return raise_missing_attr_status(object, name_id);
    }

    if value.is_null() {
        if unsafe { (&mut *dict).del(name_id) } {
            if is_type {
                let ty = object.cast::<PyType>();
                let _ = update_slot_from_dunder(ty, name_id, ptr::null_mut());
                // J0.3 §6 site #2: type-dict delete.
                sync::type_modified(ty);
            }
            0
        } else {
            raise_missing_attr_status(object, name_id)
        }
    } else {
        unsafe { (&mut *dict).set(name_id, value) };
        if is_type {
            let ty = object.cast::<PyType>();
            let _ = update_slot_from_dunder(ty, name_id, value);
            // J0.3 §6 site #1: type-dict set.
            sync::type_modified(ty);
        }
        0
    }
}

/// Lookup used by `super`: start after `start` inside `owner`'s MRO and bind the
/// descriptor to `obj`/`owner`.
#[must_use]
pub unsafe fn super_lookup(start: *mut PyType, obj: *mut PyObject, owner: *mut PyType, name: u32) -> *mut PyObject {
    if start.is_null() || owner.is_null() {
        return raise_attr_error("super() has incomplete type state");
    }
    let mro = unsafe { mro::mro_entries(owner) };
    let Some(index) = mro.iter().position(|ty| *ty == start) else {
        return raise_attr_error("super(type, obj): obj is not an instance or subtype of type");
    };
    for cls in mro.iter().skip(index + 1).copied() {
        if cls.is_null() {
            continue;
        }
        let dict = unsafe { (*cls).tp_dict };
        if let Some(dict) = unsafe { dict_from_ptr(dict) } {
            if let Some(value) = dict.get(name) {
                // CPython super_getattro: a class-bound proxy (obj IS the
                // owner type, e.g. zero-arg super in a metaclass `__new__` or
                // a classmethod) binds as `__get__(NULL, owner)` so functions
                // come back unbound; instance-bound proxies bind the receiver.
                let bind_obj = if obj == owner.cast::<PyObject>() { ptr::null_mut() } else { obj };
                return unsafe { descriptor_get(value, bind_obj, owner) };
            }
        }
    }
    // CPython parity: `object.__init_subclass__` is a no-op classmethod that
    // pon's builtin object type dict never materializes, so the chained
    // `super().__init_subclass__(*args, **kwargs)` every cooperative hook
    // ends with (unittest.TestCase) exhausts the MRO and lands here.
    if intern::resolve(name).as_deref() == Some("__init_subclass__") {
        // SAFETY: Live builtin entry point with the runtime calling convention.
        let function = unsafe {
            abi::pon_make_function(
                object_init_subclass_noop as *const u8,
                crate::builtins::variadic_arity(),
                name,
            )
        };
        if !function.is_null() {
            return function;
        }
    }
    // CPython super_getattro falls back to generic lookup on the proxy
    // itself; a miss there raises a REAL AttributeError.  A message-only
    // sentinel would be uncatchable (`except AttributeError` never matches),
    // which broke importlib's `KeyedRef.__new__` chain outright.
    let attr = intern::resolve(name).unwrap_or_default();
    abi::exc::raise_attribute_error_text(&format!("'super' object has no attribute '{attr}'"))
}

/// `object.__init_subclass__` surrogate: accepts anything, does nothing.
unsafe extern "C" fn object_init_subclass_noop(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    // SAFETY: Singleton accessor.
    unsafe { abi::pon_none() }
}

/// Python-level metaclass hook (`__instancecheck__`/`__subclasscheck__`)
/// defined strictly below the builtin `type` in `cls`'s metatype MRO.
unsafe fn metaclass_check_hook(cls: *mut PyObject, name: u32) -> *mut PyObject {
    let meta = unsafe { object_type(cls) };
    let type_type = abi::runtime_type_type();
    if meta.is_null() || meta == type_type {
        return ptr::null_mut();
    }
    for entry in unsafe { mro::mro_entries(meta) } {
        if entry == type_type {
            break;
        }
        if entry.is_null() {
            continue;
        }
        let dict = unsafe { (*entry).tp_dict.cast::<PyClassDict>() };
        if dict.is_null() {
            continue;
        }
        if let Some(value) = unsafe { (&*dict).get(name) } {
            return value;
        }
    }
    ptr::null_mut()
}

/// Bind and call one metaclass check hook; returns the truth value.
unsafe fn call_metaclass_check_hook(hook: *mut PyObject, cls: *mut PyObject, arg: *mut PyObject) -> c_int {
    let meta = unsafe { object_type(cls) };
    let bound = unsafe { descriptor_get(hook, cls, meta) };
    if bound.is_null() {
        return -1;
    }
    let result = unsafe { call_with_one(bound, arg) };
    if result.is_null() {
        return -1;
    }
    unsafe { abi::pon_is_true(result) }
}

/// `cls.__subclasses__()` support: a bound native method over the runtime's
/// direct-subclass registry.
unsafe fn type_subclasses_method(cls: *mut PyObject) -> *mut PyObject {
    let function = unsafe {
        abi::pon_make_function(
            type_subclasses_native as *const u8,
            crate::builtins::variadic_arity(),
            intern::intern("__subclasses__"),
        )
    };
    if function.is_null() {
        return ptr::null_mut();
    }
    match crate::types::method::new_bound_method(function, cls) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => raise_attr_error(message),
    }
}

unsafe extern "C" fn type_subclasses_native(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() || argc != 1 {
        return raise_attr_error("__subclasses__() takes no arguments");
    }
    let cls = unsafe { *argv };
    if cls.is_null() || unsafe { !is_type_object(cls) } {
        return raise_attr_error("__subclasses__ receiver must be a class");
    }
    let mut entries = sync::direct_subclasses(cls.cast::<PyType>())
        .into_iter()
        .map(|ty| ty.cast::<PyObject>())
        .collect::<Vec<_>>();
    unsafe {
        abi::seq::pon_build_list(
            if entries.is_empty() { ptr::null_mut() } else { entries.as_mut_ptr() },
            entries.len(),
        )
    }
}

/// Core hook for `issubclass(cls, base)`.
pub unsafe fn issubclass(cls: *mut PyObject, base: *mut PyObject) -> c_int {
    if cls.is_null() || base.is_null() || unsafe { !is_type_object(cls) || !is_type_object(base) } {
        return raise_attr_status("issubclass() arguments must be classes");
    }
    let hook = unsafe { metaclass_check_hook(base, intern::intern("__subclasscheck__")) };
    if !hook.is_null() {
        return unsafe { call_metaclass_check_hook(hook, base, cls) };
    }
    i32::from(unsafe { mro::is_subtype(cls.cast::<PyType>(), base.cast::<PyType>()) })
}

/// Core hook for `isinstance(obj, cls)`.
pub unsafe fn isinstance(obj: *mut PyObject, cls: *mut PyObject) -> c_int {
    // CPython `PyObject_IsInstance` fast-paths `type(obj) is cls` before any
    // dispatch, so a metaclass `__instancecheck__` hook is NOT consulted for
    // exact-type matches.
    if !obj.is_null() && !cls.is_null() && unsafe { (*obj).ob_type }.cast_mut().cast::<PyObject>() == cls {
        return 1;
    }
    if typealias::is_union_type(cls) {
        for arg in typealias::union_args(cls) {
            if unsafe { isinstance(obj, *arg) } > 0 {
                return 1;
            }
        }
        return 0;
    }
    if obj.is_null() || cls.is_null() || unsafe { !is_type_object(cls) } {
        return raise_attr_status("isinstance() arg 2 must be a class");
    }
    let hook = unsafe { metaclass_check_hook(cls, intern::intern("__instancecheck__")) };
    if !hook.is_null() {
        return unsafe { call_metaclass_check_hook(hook, cls, obj) };
    }
    let ty = unsafe { object_type(obj) };
    i32::from(unsafe { mro::is_subtype(ty, cls.cast::<PyType>()) })
}

/// Convenience binder used by descriptors that call through normal Python call ABI.
#[must_use]
pub unsafe fn call_with_one(callable: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    let mut argv = [arg];
    unsafe { abi::pon_call(callable, argv.as_mut_ptr(), 1) }
}

// ---------------------------------------------------------------------------
// `getset_descriptor`: the shared native descriptor type (CPython
// `PyGetSetDescr`)
// ---------------------------------------------------------------------------
//
// One instance family fronts the builtin `type`'s dict entries
// (`__annotations__` / `__mro__` / `__dict__`), the other the `function`
// type's slot descriptors (`__code__`, `__globals__`, ...).  They share ONE
// Python-visible type because the stdlib checks identity against
// `types.GetSetDescriptorType = type(FunctionType.__code__)`: inspect's
// `getattr_static` recognizes the legitimate `type.__dict__['__dict__']`
// getset that way (`_shadowed_dict` also verifies `__objclass__`), and
// annotationlib captures `type.__dict__['__annotations__'].__get__` at module
// scope.  Function-instance attribute traffic keeps flowing through
// `function_getattro`/`function_setattro`; the function payload here only
// serves class-level reads and direct descriptor-protocol calls.

/// Which `type` getset a descriptor instance fronts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypeGetSetKind {
    /// `type.__dict__['__annotations__']` — writable PEP 649 storage
    /// (annotationlib captures `.__get__` at module scope).
    Annotations,
    /// `type.__dict__['__mro__']` — read-only MRO tuple
    /// (inspect: `_static_getmro = type.__dict__['__mro__'].__get__`).
    Mro,
    /// `type.__dict__['__dict__']` — read-only namespace snapshot
    /// (inspect: `_get_dunder_dict_of_class = type.__dict__["__dict__"].__get__`).
    DunderDict,
}

impl TypeGetSetKind {
    const fn attr_name(self) -> &'static str {
        match self {
            Self::Annotations => "__annotations__",
            Self::Mro => "__mro__",
            Self::DunderDict => "__dict__",
        }
    }
}

/// Which surface a `getset_descriptor` instance fronts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GetSetPayload {
    /// Builtin `type` dict getsets (per-kind get/set semantics below).
    Type(TypeGetSetKind),
    /// `function` slot descriptors keyed by interned attribute name; get/set
    /// delegate to `types::function` so the semantics stay in that module.
    FunctionAttr(u32),
}

/// A `getset_descriptor` instance (CPython `PyGetSetDescr`: the applicable
/// class and attribute name ride per instance; behavior is payload dispatch).
#[repr(C)]
struct PyGetSetDescr {
    ob_base: crate::object::PyObjectHeader,
    /// CPython `d_type`: the class the descriptor applies to (`type` or
    /// `function`); stamped by the runtime installers via
    /// [`finalize_getset_descriptors`] / the function factory, read back
    /// through `__objclass__` (inspect's `_shadowed_dict` verifies it).
    objclass: *mut PyType,
    payload: GetSetPayload,
}

impl PyGetSetDescr {
    /// Attribute name the descriptor serves (`__mro__`, `__code__`, ...).
    fn name(&self) -> String {
        match self.payload {
            GetSetPayload::Type(kind) => kind.attr_name().to_owned(),
            GetSetPayload::FunctionAttr(name_id) => {
                intern::resolve(name_id).unwrap_or_else(|| format!("<interned:{name_id}>"))
            }
        }
    }

    /// `d_type` display name for error messages and `__qualname__`.
    fn objclass_display(&self) -> &str {
        if self.objclass.is_null() {
            "?"
        } else {
            // SAFETY: `objclass` is a leaked builtin type stamped at install.
            unsafe { (*self.objclass).name() }
        }
    }
}

fn getset_descriptor_type() -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(ptr::null(), "getset_descriptor", mem::size_of::<PyGetSetDescr>());
        ty.tp_descr_get = Some(getset_descr_get);
        ty.tp_descr_set = Some(getset_descr_set);
        ty.tp_getattro = Some(getset_descr_getattro);
        ty.tp_repr = Some(getset_descr_repr);
        Box::into_raw(Box::new(ty)) as usize
    });
    *TYPE as *mut PyType
}

fn new_getset_descriptor(objclass: *mut PyType, payload: GetSetPayload) -> *mut PyObject {
    Box::into_raw(Box::new(PyGetSetDescr {
        ob_base: crate::object::PyObjectHeader::new(getset_descriptor_type()),
        objclass,
        payload,
    }))
    .cast::<PyObject>()
}

/// Per-kind singletons for the `type` dict (leaked; they live in the builtin
/// `type`'s tp_dict for the whole process, outside the GC heap).  `objclass`
/// starts NULL and is stamped by [`finalize_getset_descriptors`].
fn type_getset_descriptor(kind: TypeGetSetKind) -> *mut PyObject {
    static ANNOTATIONS: LazyLock<usize> =
        LazyLock::new(|| new_getset_descriptor(ptr::null_mut(), GetSetPayload::Type(TypeGetSetKind::Annotations)) as usize);
    static MRO: LazyLock<usize> =
        LazyLock::new(|| new_getset_descriptor(ptr::null_mut(), GetSetPayload::Type(TypeGetSetKind::Mro)) as usize);
    static DUNDER_DICT: LazyLock<usize> =
        LazyLock::new(|| new_getset_descriptor(ptr::null_mut(), GetSetPayload::Type(TypeGetSetKind::DunderDict)) as usize);
    (match kind {
        TypeGetSetKind::Annotations => *ANNOTATIONS,
        TypeGetSetKind::Mro => *MRO,
        TypeGetSetKind::DunderDict => *DUNDER_DICT,
    }) as *mut PyObject
}

/// The `type.__dict__['__annotations__']` singleton (identity guard for the
/// builtin `type` in [`type_annotations_get`]; also the abi.rs install hook).
#[must_use]
pub fn annotations_descriptor() -> *mut PyObject {
    type_getset_descriptor(TypeGetSetKind::Annotations)
}

/// Every `(name, descriptor)` pair belonging in the builtin `type`'s dict.
#[must_use]
pub fn type_getset_entries() -> [(&'static str, *mut PyObject); 3] {
    [TypeGetSetKind::Annotations, TypeGetSetKind::Mro, TypeGetSetKind::DunderDict]
        .map(|kind| (kind.attr_name(), type_getset_descriptor(kind)))
}

/// `function` slot descriptor factory (`types::function` install path); the
/// `function` type rides in as `objclass` for `__objclass__` and messages.
#[must_use]
pub(crate) fn new_function_getset_descriptor(name_id: u32, objclass: *mut PyType) -> *mut PyObject {
    new_getset_descriptor(objclass, GetSetPayload::FunctionAttr(name_id))
}

/// Stamps the runtime-init identities descriptors can't reach at
/// construction: the shared descriptor type's metatype and the builtin
/// `type` as the type-getsets' `__objclass__`.  Idempotent; both installers
/// (abi.rs type setup, `install_function_type_attrs`) call it so the result
/// is order-independent.
pub(crate) unsafe fn finalize_getset_descriptors(type_type: *mut PyType) {
    unsafe { (*getset_descriptor_type()).ob_base.ob_type = type_type };
    for kind in [TypeGetSetKind::Annotations, TypeGetSetKind::Mro, TypeGetSetKind::DunderDict] {
        let descr = type_getset_descriptor(kind).cast::<PyGetSetDescr>();
        unsafe { (*descr).objclass = type_type };
    }
}

/// PEP 649 lazy class annotations for `ty`: own-dict cache hit, else
/// materialize by calling the class's own `__annotate__(1)` (VALUE format)
/// and cache into tp_dict.  Never MRO-inherited: each class materializes its
/// own dict (empty when the class body had no annotations).
///
/// The builtin `type` itself is the one class whose own dict holds the
/// descriptor singleton rather than an annotations dict; CPython's getset
/// raises AttributeError for static types there, and so do we (annotationlib
/// relies on that branch to classify static types).
pub(crate) unsafe fn type_annotations_get(ty: *mut PyType) -> *mut PyObject {
    let name_id = intern::intern("__annotations__");
    let dict = unsafe { (*ty).tp_dict.cast::<PyClassDict>() };
    if !dict.is_null() {
        if let Some(value) = unsafe { (&*dict).get(name_id) } {
            if value == annotations_descriptor() {
                const MESSAGE: &str = "type object 'type' has no attribute '__annotations__'";
                return crate::abi::exc::raise_kind_error_text(crate::types::exc::ExceptionKind::AttributeError, MESSAGE);
            }
            return value;
        }
    }
    let annotate = if dict.is_null() {
        None
    } else {
        unsafe { (&*dict).get(intern::intern("__annotate__")) }
    };
    let annotations = match annotate {
        Some(annotate) => {
            let format = unsafe { abi::pon_const_int(1) };
            if format.is_null() {
                return ptr::null_mut();
            }
            let mut argv = [format];
            let result = unsafe { abi::pon_call(annotate, argv.as_mut_ptr(), 1) };
            if result.is_null() {
                // Propagate NameError/NotImplementedError from the
                // annotate body without caching a partial dict.
                return ptr::null_mut();
            }
            result
        }
        None => unsafe { abi::map::pon_build_map(ptr::null_mut(), 0) },
    };
    if annotations.is_null() || dict.is_null() {
        return annotations;
    }
    unsafe { (&mut *dict).set(name_id, annotations) };
    // J0.3 §6 site #1 (type-dict set): the cache insert mutates the class
    // namespace, so stale replays must re-resolve.
    sync::type_modified(ty);
    annotations
}

/// Class-level `__annotations__` write/delete (the writable getset arm):
/// stores into the class's own tp_dict entry — the same storage the getter
/// consults.  Receiver guards live in [`getset_descr_set`].
unsafe fn type_annotations_set(ty: *mut PyType, value: *mut PyObject) -> c_int {
    let name_id = intern::intern("__annotations__");
    let dict = unsafe { (*ty).tp_dict.cast::<PyClassDict>() };
    let own_entry = if dict.is_null() {
        None
    } else {
        unsafe { (&*dict).get(name_id) }
    };
    if own_entry == Some(annotations_descriptor()) {
        // The builtin `type` — the descriptor's home, recognizable by the
        // singleton in its own dict — is immutable, as in CPython.
        return raise_type_status("cannot set '__annotations__' attribute of immutable type 'type'");
    }
    if value.is_null() {
        // Delete: CPython raises a bare AttributeError('__annotations__')
        // when nothing was cached or assigned.
        if dict.is_null() || unsafe { !(&mut *dict).del(name_id) } {
            let _ = crate::abi::exc::raise_kind_error_text(crate::types::exc::ExceptionKind::AttributeError, "__annotations__");
            return -1;
        }
    } else {
        let dict = if dict.is_null() {
            let fresh = type_::new_namespace();
            unsafe { (*ty).tp_dict = fresh.cast::<PyObject>() };
            fresh
        } else {
            dict
        };
        unsafe { (&mut *dict).set(name_id, value) };
    }
    // J0.3 §6 sites #1/#2: type-dict mutation through the descriptor must
    // invalidate stale attr replays (direct `descr.__set__(cls, v)` calls
    // bypass `generic_set_attr`'s bump).
    sync::type_modified(ty);
    0
}

/// CPython receiver-mismatch text: `descriptor 'X' for 'T' objects doesn't
/// apply to a 'Y' object`.
unsafe fn getset_receiver_mismatch(descr: *mut PyGetSetDescr, obj: *mut PyObject) -> String {
    let got = unsafe { object_type_display(obj) };
    let (name, objclass) = unsafe { ((*descr).name(), (*descr).objclass_display()) };
    format!("descriptor '{name}' for '{objclass}' objects doesn't apply to a '{got}' object")
}

/// `descriptor.__get__(obj, owner=None)` slot: a NULL/absent instance returns
/// the descriptor itself (CPython getset class-access semantics); receivers
/// are validated per payload.
unsafe extern "C" fn getset_descr_get(descr: *mut PyObject, obj: *mut PyObject, _owner: *mut PyObject) -> *mut PyObject {
    if obj.is_null() {
        return descr;
    }
    let d = descr.cast::<PyGetSetDescr>();
    match unsafe { (*d).payload } {
        GetSetPayload::Type(kind) => {
            if unsafe { !is_type_object(obj) } {
                let message = unsafe { getset_receiver_mismatch(d, obj) };
                return unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
            }
            let ty = obj.cast::<PyType>();
            match kind {
                TypeGetSetKind::Annotations => unsafe { type_annotations_get(ty) },
                TypeGetSetKind::Mro => {
                    // Same tuple the `__mro__` fast path in
                    // `generic_get_attr_cached` builds (inspect's
                    // `_static_getmro` must agree with `C.__mro__`).
                    let mut entries = unsafe { mro::mro_entries(ty) }
                        .into_iter()
                        .map(|entry| entry.cast::<PyObject>())
                        .collect::<Vec<_>>();
                    unsafe {
                        abi::seq::pon_build_tuple(
                            if entries.is_empty() { ptr::null_mut() } else { entries.as_mut_ptr() },
                            entries.len(),
                        )
                    }
                }
                TypeGetSetKind::DunderDict => unsafe { type_dict_object(ty) },
            }
        }
        GetSetPayload::FunctionAttr(name_id) => {
            if unsafe { is_type_object(obj) } {
                // Class-level read (`FunctionType.__code__`): the descriptor
                // itself, exactly as before unification.
                return descr;
            }
            if !crate::types::function::is_function_object(obj) {
                let message = unsafe { getset_receiver_mismatch(d, obj) };
                return unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
            }
            unsafe { crate::types::function::getset_slot_get(obj, name_id) }
        }
    }
}

/// `descriptor.__set__(obj, value)` / `__delete__(obj)` slot.  Of the `type`
/// getsets only `__annotations__` is writable; `__mro__`/`__dict__` raise the
/// CPython read-only AttributeError.  Function slots delegate to
/// `function_setattro` — the same semantics as a plain attribute write.
unsafe extern "C" fn getset_descr_set(descr: *mut PyObject, obj: *mut PyObject, value: *mut PyObject) -> c_int {
    let d = descr.cast::<PyGetSetDescr>();
    match unsafe { (*d).payload } {
        GetSetPayload::Type(kind) => {
            if obj.is_null() || unsafe { !is_type_object(obj) } {
                return raise_type_status(unsafe { getset_receiver_mismatch(d, obj) });
            }
            if kind != TypeGetSetKind::Annotations {
                let message = format!("attribute '{}' of 'type' objects is not writable", kind.attr_name());
                let _ = crate::abi::exc::raise_kind_error_text(crate::types::exc::ExceptionKind::AttributeError, &message);
                return -1;
            }
            unsafe { type_annotations_set(obj.cast::<PyType>(), value) }
        }
        GetSetPayload::FunctionAttr(name_id) => {
            if !crate::types::function::is_function_object(obj) {
                return raise_type_status(unsafe { getset_receiver_mismatch(d, obj) });
            }
            unsafe { crate::types::function::getset_slot_set(obj, name_id, value) }
        }
    }
}

/// repr parity: `<attribute '<name>' of '<objclass>' objects>`.
unsafe extern "C" fn getset_descr_repr(descr: *mut PyObject) -> *mut PyObject {
    let d = descr.cast::<PyGetSetDescr>();
    let text = unsafe { format!("<attribute '{}' of '{}' objects>", (*d).name(), (*d).objclass_display()) };
    unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

/// `tp_getattro` for the descriptor: the protocol dunders are served as
/// callable bound methods (annotationlib and inspect store `descr.__get__`
/// and call it later), plus the introspective name fields.  Unknown names
/// raise AttributeError DIRECTLY — getset descriptors carry no instance
/// dict, and the generic path's instance-dict probe assumes a
/// `PyHeapInstance` layout these header-only payloads don't have (falling
/// through used to read a garbage `dict` pointer and abort).
unsafe extern "C" fn getset_descr_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(text) = (unsafe { type_::unicode_text(crate::tag::untag_arg(name)) }) else {
        const MESSAGE: &str = "attribute name must be str";
        return unsafe { abi::exc::pon_raise_type_error(MESSAGE.as_ptr(), MESSAGE.len()) };
    };
    let d = object.cast::<PyGetSetDescr>();
    match text {
        "__get__" => getset_descr_bound_entry(object, text, getset_descr_dunder_get_entry),
        "__set__" => getset_descr_bound_entry(object, text, getset_descr_dunder_set_entry),
        "__delete__" => getset_descr_bound_entry(object, text, getset_descr_dunder_delete_entry),
        "__name__" => {
            let name = unsafe { (*d).name() };
            unsafe { abi::pon_const_str(name.as_ptr(), name.len()) }
        }
        "__qualname__" => {
            let qualname = unsafe { format!("{}.{}", (*d).objclass_display(), (*d).name()) };
            unsafe { abi::pon_const_str(qualname.as_ptr(), qualname.len()) }
        }
        "__objclass__" => {
            let objclass = unsafe { (*d).objclass };
            if objclass.is_null() {
                return unsafe { abi::pon_raise_attribute_error(object, intern::intern(text)) };
            }
            objclass.cast::<PyObject>()
        }
        "__class__" => unsafe { (*object).ob_type }.cast_mut().cast::<PyObject>(),
        "__doc__" => unsafe { abi::pon_none() },
        _ => unsafe { abi::pon_raise_attribute_error(object, intern::intern(text)) },
    }
}

/// Binds `entry` to `receiver` as a method pair (receiver rides in `argv[0]`).
fn getset_descr_bound_entry(
    receiver: *mut PyObject,
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> *mut PyObject {
    // SAFETY: `entry` is a live builtin entry point with the runtime calling
    // convention.
    let function =
        unsafe { abi::pon_make_function(entry as *const u8, crate::builtins::variadic_arity(), intern::intern(name)) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match crate::types::method::new_bound_method(function, receiver) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => raise_attr_error(message),
    }
}

unsafe fn getset_descr_entry_args<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
    if argv.is_null() {
        return (argc == 0).then_some(&[]);
    }
    Some(unsafe { core::slice::from_raw_parts(argv.cast_const(), argc) })
}

/// True when `object` is the `None` singleton (tag-tolerant).
fn getset_descr_none_arg(object: *mut PyObject) -> bool {
    // SAFETY: Singleton accessor.
    crate::tag::untag_arg(object) == unsafe { abi::pon_none() }
}

/// `descr.__get__(obj, owner=None)` — annotationlib's and inspect's captured
/// getters call this with one argument.
unsafe extern "C" fn getset_descr_dunder_get_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { getset_descr_entry_args(argv, argc) }) else {
        return raise_attr_error("__get__ received a NULL argv pointer");
    };
    let (&receiver, rest) = args.split_first().unwrap_or((&ptr::null_mut(), &[]));
    if rest.is_empty() || rest.len() > 2 {
        const MESSAGE: &str = "__get__(instance, owner=None) takes 1 or 2 arguments";
        return unsafe { abi::exc::pon_raise_type_error(MESSAGE.as_ptr(), MESSAGE.len()) };
    }
    let obj = if getset_descr_none_arg(rest[0]) { ptr::null_mut() } else { rest[0] };
    let owner = rest.get(1).copied().unwrap_or(ptr::null_mut());
    // SAFETY: Slot implementation follows the NULL-sentinel error contract.
    unsafe { getset_descr_get(receiver, obj, owner) }
}

/// `descr.__set__(obj, value)`.
unsafe extern "C" fn getset_descr_dunder_set_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { getset_descr_entry_args(argv, argc) }) else {
        return raise_attr_error("__set__ received a NULL argv pointer");
    };
    let &[receiver, obj, value] = args else {
        const MESSAGE: &str = "__set__(instance, value) takes exactly 2 arguments";
        return unsafe { abi::exc::pon_raise_type_error(MESSAGE.as_ptr(), MESSAGE.len()) };
    };
    // SAFETY: Slot implementation follows the negative-status error contract.
    if unsafe { getset_descr_set(receiver, obj, value) } < 0 {
        return ptr::null_mut();
    }
    // SAFETY: Singleton accessor.
    unsafe { abi::pon_none() }
}

/// `descr.__delete__(obj)`.
unsafe extern "C" fn getset_descr_dunder_delete_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { getset_descr_entry_args(argv, argc) }) else {
        return raise_attr_error("__delete__ received a NULL argv pointer");
    };
    let &[receiver, obj] = args else {
        const MESSAGE: &str = "__delete__(instance) takes exactly 1 argument";
        return unsafe { abi::exc::pon_raise_type_error(MESSAGE.as_ptr(), MESSAGE.len()) };
    };
    // SAFETY: Slot implementation follows the negative-status error contract.
    if unsafe { getset_descr_set(receiver, obj, ptr::null_mut()) } < 0 {
        return ptr::null_mut();
    }
    // SAFETY: Singleton accessor.
    unsafe { abi::pon_none() }
}
#[cfg(test)]
mod tests {
    use core::mem;

    use super::*;
    use crate::feedback::{FeedbackCell, GlobalIC};
    use crate::object::PyUnicode;
    use crate::types::type_::{build_class_from_namespace, new_namespace, type_new};

    fn metatype() -> *mut PyType {
        let ty = Box::into_raw(Box::new(PyType::new(ptr::null(), "type", mem::size_of::<PyType>())));
        unsafe {
            (*ty).ob_base.ob_type = ty;
        }
        ty
    }

    unsafe fn fake_str(text: &'static str) -> *mut PyObject {
        static mut STR_TYPE: PyType = PyType::new(ptr::null(), "str", mem::size_of::<PyUnicode>());
        let ptr = &raw mut STR_TYPE;
        unsafe { (*ptr).ob_base.ob_type = ptr };
        Box::into_raw(Box::new(PyUnicode {
            ob_base: crate::object::PyObjectHeader::new(ptr),
            len: text.len(),
            data: text.as_ptr(),
            owns_data: false,
        }))
        .cast::<PyObject>()
    }

    unsafe fn class_with_attr(meta: *mut PyType, name: &str, attr: u32, value: *mut PyObject, bases: &[*mut PyObject]) -> *mut PyType {
        let ns = new_namespace();
        unsafe {
            (&mut *ns).set(attr, value);
        }
        let cls = unsafe { build_class_from_namespace(name, bases, ns, &[]) }.cast::<PyType>();
        assert!(!cls.is_null());
        unsafe {
            if (*cls).ob_base.ob_type.is_null() {
                (*cls).ob_base.ob_type = meta;
            }
        }
        cls
    }

    #[test]
    fn type_dict_set_invalidates_recorded_attr_ic() {
        let meta = metatype();
        let attr = intern::intern("payload");
        let old_value = unsafe { fake_str("old") };
        let new_value = unsafe { fake_str("new") };
        let cls = unsafe { class_with_attr(meta, "C", attr, old_value, &[]) };
        let instance = unsafe { type_new(cls, ptr::null_mut(), ptr::null_mut()) };
        assert!(!instance.is_null());

        let cell = FeedbackCell::EMPTY;
        // Miss + record.
        assert_eq!(unsafe { generic_get_attr_cached(instance, attr, &cell) }, old_value);
        let live = unsafe { (*cls).version() };
        assert!(cell.attr_hit(cls as usize, live).is_some(), "record published");
        // Hit replays the cached translation.
        assert_eq!(unsafe { generic_get_attr_cached(instance, attr, &cell) }, old_value);

        // Mutate the TYPE dict through generic_set_attr (J0.3 §6 site #1).
        let name_obj = unsafe { fake_str("payload") };
        assert_eq!(unsafe { generic_set_attr(cls.cast::<PyObject>(), name_obj, new_value) }, 0);
        assert!(unsafe { (*cls).version() } > live, "version bumped");
        assert!(cell.attr_hit(cls as usize, unsafe { (*cls).version() }).is_none(), "stale record misses");
        // Slow path re-resolves and re-records the new translation.
        assert_eq!(unsafe { generic_get_attr_cached(instance, attr, &cell) }, new_value);
        assert_eq!(unsafe { generic_get_attr_cached(instance, attr, &cell) }, new_value);
    }

    #[test]
    fn instance_mutation_does_not_invalidate_attr_ic() {
        let meta = metatype();
        let attr = intern::intern("field");
        let class_value = unsafe { fake_str("class") };
        let inst_value = unsafe { fake_str("inst") };
        let cls = unsafe { class_with_attr(meta, "I", attr, class_value, &[]) };
        let instance = unsafe { type_new(cls, ptr::null_mut(), ptr::null_mut()) };

        let cell = FeedbackCell::EMPTY;
        assert_eq!(unsafe { generic_get_attr_cached(instance, attr, &cell) }, class_value);
        let live = unsafe { (*cls).version() };
        assert!(cell.attr_hit(cls as usize, live).is_some());

        // Instance-dict store is a §6 explicit NON-site: no bump...
        let name_obj = unsafe { fake_str("field") };
        assert_eq!(unsafe { generic_set_attr(instance, name_obj, inst_value) }, 0);
        assert_eq!(unsafe { (*cls).version() }, live, "instance store must not bump the type");
        // ...and the probe-dict replay still honors instance shadowing.
        assert_eq!(unsafe { generic_get_attr_cached(instance, attr, &cell) }, inst_value);
    }

    #[test]
    fn base_mutation_invalidates_subclass_attr_ic() {
        let meta = metatype();
        let attr = intern::intern("shared");
        let old_value = unsafe { fake_str("base-old") };
        let new_value = unsafe { fake_str("base-new") };
        let base = unsafe { class_with_attr(meta, "B", attr, old_value, &[]) };
        let derived_ns = new_namespace();
        let derived = unsafe { build_class_from_namespace("D", &[base.cast::<PyObject>()], derived_ns, &[]) }.cast::<PyType>();
        assert!(!derived.is_null());
        unsafe {
            if (*derived).ob_base.ob_type.is_null() {
                (*derived).ob_base.ob_type = meta;
            }
        }
        let instance = unsafe { type_new(derived, ptr::null_mut(), ptr::null_mut()) };

        let cell = FeedbackCell::EMPTY;
        // The record guards DERIVED's tag even though the value lives on B.
        assert_eq!(unsafe { generic_get_attr_cached(instance, attr, &cell) }, old_value);
        let live = unsafe { (*derived).version() };
        assert!(cell.attr_hit(derived as usize, live).is_some());

        // Mutating the BASE must transitively bump the subclass (§6 note A).
        let name_obj = unsafe { fake_str("shared") };
        assert_eq!(unsafe { generic_set_attr(base.cast::<PyObject>(), name_obj, new_value) }, 0);
        assert!(unsafe { (*derived).version() } > live, "subclass version bumped transitively");
        assert!(cell.attr_hit(derived as usize, unsafe { (*derived).version() }).is_none());
        assert_eq!(unsafe { generic_get_attr_cached(instance, attr, &cell) }, new_value);
    }

    #[test]
    fn namespace_version_bump_invalidates_global_ic() {
        let cell = FeedbackCell::EMPTY;
        let identity = crate::abi::namespace_identity_for_tests();
        let version = crate::abi::namespace_version();
        let value = unsafe { fake_str("g") };
        cell.record_global(
            identity,
            GlobalIC {
                dict_version: version,
                builtins_version: 0,
                value_ptr: value as usize,
            },
        );
        assert!(cell.global_hit(identity, version, 0).is_some(), "fresh record hits");
        crate::abi::bump_namespace_version();
        assert!(
            cell.global_hit(identity, crate::abi::namespace_version(), 0).is_none(),
            "any namespace mutation invalidates the record"
        );
    }
}
