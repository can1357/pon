//! Super proxy implementation.

use core::ptr;

use crate::descr;
use crate::intern;
use crate::mro;
use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::thread_state::pon_err_set;

/// Python `super` proxy.
#[repr(C)]
#[derive(Debug)]
pub struct PySuper {
    /// Common object header; must remain first.
    pub ob_base: PyObjectHeader,
    /// Type after which lookup begins.
    pub start: *mut PyType,
    /// Bound object or class.
    pub obj: *mut PyObject,
    /// Dynamic owner type used for MRO traversal.
    pub obj_type: *mut PyType,
}

fn raise_super(message: &str) -> *mut PyObject {
    pon_err_set(message);
    ptr::null_mut()
}

unsafe fn object_type(object: *mut PyObject) -> *mut PyType {
    if object.is_null() {
        ptr::null_mut()
    } else {
        unsafe { (*object).ob_type.cast_mut() }
    }
}

/// Allocate a bound `super(type, obj)` proxy.
#[must_use]
pub unsafe fn new_super(super_type: *const PyType, start: *mut PyType, obj: *mut PyObject) -> *mut PyObject {
    if start.is_null() || obj.is_null() {
        return raise_super("super() arguments must not be NULL");
    }
    let meta = unsafe { object_type(obj) };
    if meta.is_null() {
        return raise_super("super(type, obj): obj is not an instance or subtype of type");
    }
    // CPython supercheck ordering: a class receiver that is itself a subtype
    // of `start` anchors MRO traversal at the class (classmethod / metaclass
    // super), even when its metatype is a `type` subclass like ABCMeta;
    // otherwise the receiver must be an instance of `start`.
    let receiver_is_class = unsafe { crate::types::type_::is_type_object(obj) };
    let obj_type = if receiver_is_class && unsafe { mro::is_subtype(obj.cast::<PyType>(), start) } {
        obj.cast::<PyType>()
    } else {
        meta
    };
    if unsafe { !mro::is_subtype(obj_type, start) } {
        return raise_super("super(type, obj): obj is not an instance or subtype of type");
    }
    Box::into_raw(Box::new(PySuper {
        ob_base: PyObjectHeader::new(super_type),
        start,
        obj,
        obj_type,
    }))
    .cast::<PyObject>()
}

/// Descriptor-aware attribute lookup for `super` proxies.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn super_getattro(proxy: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    if proxy.is_null() {
        return raise_super("super proxy is NULL");
    }
    let Some(text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        return raise_super("super attribute name must be a string");
    };
    let name = intern::intern(text);
    let proxy = unsafe { &*proxy.cast::<PySuper>() };
    unsafe { descr::super_lookup(proxy.start, proxy.obj, proxy.obj_type, name) }
}

/// Populate slots on a `super` type descriptor.
pub fn install_super_slots(ty: &mut PyType) {
    ty.tp_getattro = Some(super_getattro);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::PyType;

    #[test]
    fn rejects_unrelated_type() {
        let mut type_type = PyType::new(ptr::null(), "type", core::mem::size_of::<PyType>());
        let type_ptr = &mut type_type as *mut PyType;
        type_type.ob_base.ob_type = type_ptr;
        let mut a = PyType::new(type_ptr, "A", 0);
        let mut b = PyType::new(type_ptr, "B", 0);
        let obj = (&mut b as *mut PyType).cast::<PyObject>();
        assert!(unsafe { new_super(type_ptr, &mut a, obj) }.is_null());
    }
}
