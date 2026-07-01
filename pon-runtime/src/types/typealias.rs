//! Runtime type-alias implementation.

use core::mem::{offset_of, size_of};
use std::sync::LazyLock;

use crate::object::{as_object_ptr, PyObject, PyObjectHeader, PyType};

/// Representative runtime object for Python 3.12+ `type X = ...` aliases.
///
/// The object stores the interned alias name and the already-evaluated value used
/// by the current tier-0 lowering.  Full lazy evaluation and type-parameter scope
/// handling belong to the later typing workstream; this shape gives compiled code
/// a real boxed `TypeAliasType` value instead of treating aliases as plain globals.
#[repr(C)]
#[derive(Debug)]
pub struct PyTypeAlias {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Interned alias name.
    pub name_interned: u32,
    /// Evaluated alias target object.
    pub value: *mut PyObject,
}

impl PyTypeAlias {
    /// Builds a type-alias payload for an allocated object slot.
    #[must_use]
    pub const fn new(ty: *const PyType, name_interned: u32, value: *mut PyObject) -> Self {
        Self {
            ob_base: PyObjectHeader::new(ty),
            name_interned,
            value,
        }
    }
}

/// Returns the process-lifetime `TypeAliasType` descriptor.
///
/// The representative descriptor is intentionally self-contained for now; the
/// runtime bootstrap can patch in the real metatype when aliases become a
/// registered GC allocation family.
#[must_use]
pub fn type_alias_type(type_type: *const PyType) -> *mut PyType {
    let _ = type_type;
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let ty = PyType::new(core::ptr::null(), "TypeAliasType", size_of::<PyTypeAlias>());
        Box::into_raw(Box::new(ty)) as usize
    });
    *TYPE as *mut PyType
}

/// Allocates a boxed `TypeAliasType` representative.
///
/// This intentionally uses a leaked box until the runtime bootstrap registers a
/// dedicated GC type id for aliases.  The object is immutable and contains only
/// borrowed boxed-object pointers, so leaking here is preferable to inventing an
/// unregistered GC allocation path.
#[must_use]
pub fn new_type_alias(name_interned: u32, value: *mut PyObject, type_type: *const PyType) -> *mut PyObject {
    let ty = type_alias_type(type_type);
    as_object_ptr(Box::into_raw(Box::new(PyTypeAlias::new(ty.cast_const(), name_interned, value))))
}

/// C ABI constructor used by the post-wave lowering hook for `type X = expr`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_make_type_alias(name_interned: u32, value: *mut PyObject) -> *mut PyObject {
    new_type_alias(name_interned, value, core::ptr::null())
}

const _: () = {
    assert!(offset_of!(PyTypeAlias, ob_base) == 0);
};
