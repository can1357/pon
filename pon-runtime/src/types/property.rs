//! Property descriptor implementation.

use core::ffi::c_int;
use core::ptr;

use crate::abi;
use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::thread_state::pon_err_set;

/// Python `property` object.
#[repr(C)]
#[derive(Debug)]
pub struct PyProperty {
    /// Common object header; must remain first.
    pub ob_base: PyObjectHeader,
    /// Getter callable, or NULL.
    pub fget: *mut PyObject,
    /// Setter callable, or NULL.
    pub fset: *mut PyObject,
    /// Deleter callable, or NULL.
    pub fdel: *mut PyObject,
    /// Documentation object, or NULL.
    pub doc: *mut PyObject,
}

fn raise_property(message: &str) -> *mut PyObject {
    pon_err_set(message);
    ptr::null_mut()
}

fn raise_property_status(message: &str) -> c_int {
    pon_err_set(message);
    -1
}

/// Allocate a property descriptor.
#[must_use]
pub unsafe fn new_property(
    property_type: *const PyType,
    fget: *mut PyObject,
    fset: *mut PyObject,
    fdel: *mut PyObject,
    doc: *mut PyObject,
) -> *mut PyObject {
    Box::into_raw(Box::new(PyProperty {
        ob_base: PyObjectHeader::new(property_type),
        fget,
        fset,
        fdel,
        doc,
    }))
    .cast::<PyObject>()
}

/// Descriptor `property.__get__`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn property_descr_get(descr: *mut PyObject, obj: *mut PyObject, _owner: *mut PyObject) -> *mut PyObject {
    if descr.is_null() {
        return raise_property("property descriptor is NULL");
    }
    if obj.is_null() {
        return descr;
    }
    let property = unsafe { &*descr.cast::<PyProperty>() };
    if property.fget.is_null() {
        return raise_property("unreadable attribute");
    }
    let mut argv = [obj];
    unsafe { abi::pon_call(property.fget, argv.as_mut_ptr(), 1) }
}

/// Descriptor `property.__set__`/`property.__delete__`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn property_descr_set(descr: *mut PyObject, obj: *mut PyObject, value: *mut PyObject) -> c_int {
    if descr.is_null() || obj.is_null() {
        return raise_property_status("property assignment has NULL operand");
    }
    let property = unsafe { &*descr.cast::<PyProperty>() };
    if value.is_null() {
        if property.fdel.is_null() {
            return raise_property_status("can't delete attribute");
        }
        let mut argv = [obj];
        let result = unsafe { abi::pon_call(property.fdel, argv.as_mut_ptr(), 1) };
        return if result.is_null() { -1 } else { 0 };
    }
    if property.fset.is_null() {
        return raise_property_status("can't set attribute");
    }
    let mut argv = [obj, value];
    let result = unsafe { abi::pon_call(property.fset, argv.as_mut_ptr(), 2) };
    if result.is_null() { -1 } else { 0 }
}

/// Populate the slots on a `property` type descriptor.
pub fn install_property_slots(ty: &mut PyType) {
    ty.tp_descr_get = Some(property_descr_get);
    ty.tp_descr_set = Some(property_descr_set);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_without_getter_is_data_descriptor_error() {
        let mut property_type = PyType::new(ptr::null(), "property", core::mem::size_of::<PyProperty>());
        install_property_slots(&mut property_type);
        let property = unsafe { new_property(&property_type, ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut()) };
        assert!(unsafe { property_descr_get(property, 1usize as *mut PyObject, ptr::null_mut()) }.is_null());
    }
}
