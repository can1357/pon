//! Descriptor protocol and generic attribute access.

use core::ffi::c_int;
use core::ptr;

use crate::abi;
use crate::intern;
use crate::mro;
use crate::object::{PyObject, PyType, update_slot_from_dunder};
use crate::thread_state::pon_err_set;
use crate::types::type_::{self, PyClassDict, PyHeapInstance};

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

/// Invoke `descr.__get__(obj, owner)` when a descriptor slot exists.
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
    descr
}

/// Invoke `descr.__set__`/`__delete__` when a descriptor setter slot exists.
pub unsafe fn descriptor_set(descr: *mut PyObject, obj: *mut PyObject, value: *mut PyObject) -> c_int {
    if descr.is_null() {
        return raise_attr_status("descriptor is NULL");
    }
    let ty = unsafe { object_type(descr) };
    if ty.is_null() {
        return raise_attr_status("descriptor has no type");
    }
    let Some(set) = (unsafe { (*ty).tp_descr_set }) else {
        return raise_attr_status("attribute is read-only");
    };
    unsafe { set(descr, obj, value) }
}

#[must_use]
unsafe fn is_data_descriptor(descr: *mut PyObject) -> bool {
    if descr.is_null() {
        return false;
    }
    let ty = unsafe { object_type(descr) };
    !ty.is_null() && unsafe { (*ty).tp_descr_set.is_some() }
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
    if name_id == intern::intern("__name__") && unsafe { is_type_object(object) } {
        let type_name = unsafe { (*object.cast::<PyType>()).name() };
        return unsafe { abi::pon_const_str(type_name.as_ptr(), type_name.len()) };
    }

    let obj_ty = unsafe { object_type(object) };
    if obj_ty.is_null() {
        return raise_attr_error("attribute receiver has no type");
    }

    let meta_descr = if unsafe { is_type_object(object) } {
        unsafe { lookup_in_type(obj_ty, name_id) }
    } else {
        ptr::null_mut()
    };
    if unsafe { is_data_descriptor(meta_descr) } {
        return unsafe { descriptor_get(meta_descr, object, obj_ty) };
    }

    let class_descr = if unsafe { is_type_object(object) } {
        unsafe { lookup_in_type(object.cast::<PyType>(), name_id) }
    } else {
        unsafe { lookup_in_type(obj_ty, name_id) }
    };
    if unsafe { is_data_descriptor(class_descr) } {
        return unsafe { descriptor_get(class_descr, object, obj_ty) };
    }

    let dict = unsafe { instance_dict(object) };
    if !dict.is_null() {
        if let Some(value) = unsafe { (&*dict).get(name_id) } {
            return value;
        }
    }

    if !class_descr.is_null() {
        return unsafe { descriptor_get(class_descr, object, obj_ty) };
    }
    if !meta_descr.is_null() {
        return unsafe { descriptor_get(meta_descr, object, obj_ty) };
    }

    let spelling = intern::resolve(name_id).unwrap_or_else(|| format!("<interned:{name_id}>"));
    raise_attr_error(format!("attribute '{spelling}' was not found"))
}

/// Generic attribute assignment/deletion with data-descriptor and slots support.
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

    let descr = if unsafe { is_type_object(object) } {
        unsafe { lookup_in_type(obj_ty, name_id) }
    } else {
        unsafe { lookup_in_type(obj_ty, name_id) }
    };
    if unsafe { is_data_descriptor(descr) } {
        return unsafe { descriptor_set(descr, object, value) };
    }

    if unsafe { !is_type_object(object) && type_::instance_set_slot(object.cast::<PyHeapInstance>(), name_id, value) } {
        return 0;
    }

    let dict = unsafe { instance_dict(object) };
    if dict.is_null() {
        let spelling = intern::resolve(name_id).unwrap_or_else(|| format!("<interned:{name_id}>"));
        return raise_attr_status(format!("attribute '{spelling}' cannot be assigned"));
    }

    if value.is_null() {
        if unsafe { (&mut *dict).del(name_id) } {
            if unsafe { is_type_object(object) } {
                let _ = update_slot_from_dunder(object.cast::<PyType>(), name_id, ptr::null_mut());
            }
            0
        } else {
            raise_attr_status("attribute does not exist")
        }
    } else {
        unsafe { (&mut *dict).set(name_id, value) };
        if unsafe { is_type_object(object) } {
            let _ = update_slot_from_dunder(object.cast::<PyType>(), name_id, value);
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
