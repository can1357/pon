//! Descriptor protocol and generic attribute access.

use core::ffi::c_int;
use core::{mem, ptr};

use crate::abi;
use crate::feedback::{ATTR_DESCR_BLIND, ATTR_DESCR_PROBE_DICT, AttrCacheKind, AttrIC, FeedbackCell};
use crate::intern;
use crate::mro;
use crate::object::{PyObject, PyType, update_slot_from_dunder};
use crate::sync;
use crate::thread_state::pon_err_set;
use crate::types::{dict, type_::{self, PyClassDict, PyHeapInstance}};

fn raise_attr_error(message: impl Into<String>) -> *mut PyObject {
    pon_err_set(message);
    ptr::null_mut()
}

fn raise_attr_status(message: impl Into<String>) -> c_int {
    pon_err_set(message);
    -1
}

unsafe fn object_type(object: *mut PyObject) -> *mut PyType {
    if object.is_null() {
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

unsafe fn class_dict_to_dict(class_dict: *mut PyClassDict) -> *mut PyObject {
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
unsafe fn is_data_descriptor(descr: *mut PyObject) -> bool {
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
    if is_type && name_id == intern::intern("__name__") {
        let type_name = unsafe { (*object.cast::<PyType>()).name() };
        return unsafe { abi::pon_const_str(type_name.as_ptr(), type_name.len()) };
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
    if is_type && name_id == intern::intern("__annotations__") {
        // PEP 649 lazy class annotations: own-dict cache hit, else materialize
        // by calling the class's own `__annotate__(1)` (VALUE format) and
        // cache into tp_dict.  Never MRO-inherited: each class materializes
        // its own dict (empty when the class body had no annotations).
        let ty = object.cast::<PyType>();
        let dict = unsafe { (*ty).tp_dict.cast::<PyClassDict>() };
        if !dict.is_null() {
            if let Some(value) = unsafe { (&*dict).get(name_id) } {
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
        return annotations;
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
        if !is_type {
            unsafe { record_attr_translation(cell, obj_ty, version, ATTR_DESCR_BLIND, class_descr) };
        }
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
        if !is_type {
            unsafe { record_attr_translation(cell, obj_ty, version, ATTR_DESCR_PROBE_DICT, class_descr) };
        }
        return unsafe { descriptor_get(class_descr, object, obj_ty) };
    }
    if !meta_descr.is_null() {
        return unsafe { descriptor_get(meta_descr, object, obj_ty) };
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
                return unsafe { descriptor_get(value, obj, owner) };
            }
        }
    }
    raise_attr_error("super attribute was not found")
}

/// Core hook for `issubclass(cls, base)`.
pub unsafe fn issubclass(cls: *mut PyObject, base: *mut PyObject) -> c_int {
    if cls.is_null() || base.is_null() || unsafe { !is_type_object(cls) || !is_type_object(base) } {
        return raise_attr_status("issubclass() arguments must be classes");
    }
    i32::from(unsafe { mro::is_subtype(cls.cast::<PyType>(), base.cast::<PyType>()) })
}

/// Core hook for `isinstance(obj, cls)`.
pub unsafe fn isinstance(obj: *mut PyObject, cls: *mut PyObject) -> c_int {
    if obj.is_null() || cls.is_null() || unsafe { !is_type_object(cls) } {
        return raise_attr_status("isinstance() arg 2 must be a class");
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
