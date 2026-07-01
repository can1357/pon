//! Classmethod and staticmethod descriptor implementation.

use core::ffi::c_int;
use core::ptr;

use crate::abi;
use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::thread_state::pon_err_set;

/// Python `classmethod` descriptor.
#[repr(C)]
#[derive(Debug)]
pub struct PyClassMethod {
    /// Common object header; must remain first.
    pub ob_base: PyObjectHeader,
    /// Wrapped callable.
    pub callable: *mut PyObject,
}

/// Python `staticmethod` descriptor.
#[repr(C)]
#[derive(Debug)]
pub struct PyStaticMethod {
    /// Common object header; must remain first.
    pub ob_base: PyObjectHeader,
    /// Wrapped callable.
    pub callable: *mut PyObject,
}

/// Bound classmethod wrapper.  Calling it prepends the class object to argv.
#[repr(C)]
#[derive(Debug)]
pub struct PyClassMethodBinding {
    /// Common object header; must remain first.
    pub ob_base: PyObjectHeader,
    /// Wrapped callable.
    pub callable: *mut PyObject,
    /// Owner class passed as first argument.
    pub owner: *mut PyObject,
}

fn raise_method(message: &str) -> *mut PyObject {
    pon_err_set(message);
    ptr::null_mut()
}

/// Allocate a classmethod descriptor.
#[must_use]
pub unsafe fn new_classmethod(classmethod_type: *const PyType, callable: *mut PyObject) -> *mut PyObject {
    Box::into_raw(Box::new(PyClassMethod {
        ob_base: PyObjectHeader::new(classmethod_type),
        callable,
    }))
    .cast::<PyObject>()
}

/// Allocate a staticmethod descriptor.
#[must_use]
pub unsafe fn new_staticmethod(staticmethod_type: *const PyType, callable: *mut PyObject) -> *mut PyObject {
    Box::into_raw(Box::new(PyStaticMethod {
        ob_base: PyObjectHeader::new(staticmethod_type),
        callable,
    }))
    .cast::<PyObject>()
}

/// Descriptor `classmethod.__get__`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn classmethod_descr_get(descr: *mut PyObject, obj: *mut PyObject, owner: *mut PyObject) -> *mut PyObject {
    if descr.is_null() {
        return raise_method("classmethod descriptor is NULL");
    }
    let owner = if !owner.is_null() {
        owner
    } else if !obj.is_null() {
        unsafe { (*obj).ob_type.cast_mut().cast::<PyObject>() }
    } else {
        return raise_method("classmethod needs an owner");
    };
    let classmethod = unsafe { &*descr.cast::<PyClassMethod>() };
    if classmethod.callable.is_null() {
        return raise_method("classmethod wraps NULL callable");
    }
    let binding_type = unsafe { (*descr).ob_type };
    Box::into_raw(Box::new(PyClassMethodBinding {
        ob_base: PyObjectHeader::new(binding_type),
        callable: classmethod.callable,
        owner,
    }))
    .cast::<PyObject>()
}

/// Descriptor `staticmethod.__get__`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn staticmethod_descr_get(descr: *mut PyObject, _obj: *mut PyObject, _owner: *mut PyObject) -> *mut PyObject {
    if descr.is_null() {
        return raise_method("staticmethod descriptor is NULL");
    }
    let staticmethod = unsafe { &*descr.cast::<PyStaticMethod>() };
    if staticmethod.callable.is_null() {
        raise_method("staticmethod wraps NULL callable")
    } else {
        staticmethod.callable
    }
}

/// Call slot for bound classmethod wrappers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn classmethod_binding_call(binding: *mut PyObject, argv_obj: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
    let binding = binding.cast::<PyClassMethodBinding>();
    if binding.is_null() {
        return raise_method("classmethod binding is NULL");
    }
    let binding = unsafe { &*binding };
    if binding.callable.is_null() || binding.owner.is_null() {
        return raise_method("classmethod binding is incomplete");
    }

    let explicit_args = match unsafe { crate::types::type_::positional_args_from_object(argv_obj) } {
        Ok(args) => args,
        Err(message) => return raise_method(&message),
    };
    let mut argv = Vec::with_capacity(explicit_args.len().saturating_add(1));
    argv.push(binding.owner);
    argv.extend_from_slice(&explicit_args);
    unsafe { abi::pon_call(binding.callable, argv.as_mut_ptr(), argv.len()) }
}

/// Populate slots on the `classmethod` type descriptor.
pub fn install_classmethod_slots(ty: &mut PyType) {
    ty.tp_descr_get = Some(classmethod_descr_get);
}

/// Populate slots on the `staticmethod` type descriptor.
pub fn install_staticmethod_slots(ty: &mut PyType) {
    ty.tp_descr_get = Some(staticmethod_descr_get);
}

/// Populate slots on the bound classmethod wrapper type descriptor.
pub fn install_classmethod_binding_slots(ty: &mut PyType) {
    ty.tp_call = Some(classmethod_binding_call);
}

/// Classmethod and staticmethod have no descriptor setter.
pub unsafe extern "C" fn readonly_descr_set(_descr: *mut PyObject, _obj: *mut PyObject, _value: *mut PyObject) -> c_int {
    pon_err_set("descriptor is read-only");
    -1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staticmethod_returns_wrapped_callable() {
        let mut ty = PyType::new(ptr::null(), "staticmethod", core::mem::size_of::<PyStaticMethod>());
        install_staticmethod_slots(&mut ty);
        let callable = 7usize as *mut PyObject;
        let descr = unsafe { new_staticmethod(&ty, callable) };
        assert_eq!(unsafe { staticmethod_descr_get(descr, ptr::null_mut(), ptr::null_mut()) }, callable);
    }
}
