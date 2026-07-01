//! Boxed exception objects and the Phase-B builtin exception type hierarchy.
//!
//! Exception instances are ordinary boxed Python objects with no refcount field.
//! The runtime owns allocation through `pon-gc`; this module only defines the
//! layout, immortal type descriptors, and hierarchy queries shared by ABI helpers.

use core::mem::{offset_of, size_of};
use core::ptr;

use crate::object::{PyObject, PyObjectHeader, PyType, as_object_ptr};

/// Minimal boxed exception payload shared by every builtin exception class.
#[repr(C)]
#[derive(Debug)]
pub struct PyBaseException {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Boxed message/value payload.  Message-raising helpers store `str`; value
    /// exceptions such as `KeyError` and `StopIteration` store the carried value.
    pub message: *mut PyObject,
    /// Explicit exception cause (`raise ... from ...`), or NULL.
    pub cause: *mut PyObject,
    /// Implicit exception context, or NULL.
    pub context: *mut PyObject,
    /// Traceback object slot reserved for the traceback workstream, or NULL.
    pub traceback: *mut PyObject,
}

impl PyBaseException {
    /// Builds an exception object payload for `ty`.
    #[must_use]
    pub const fn new(
        ty: *const PyType,
        message: *mut PyObject,
        cause: *mut PyObject,
        context: *mut PyObject,
        traceback: *mut PyObject,
    ) -> Self {
        Self {
            ob_base: PyObjectHeader::new(ty),
            message,
            cause,
            context,
            traceback,
        }
    }
}

/// Builtin exception class selector used by raising helpers and tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExceptionKind {
    BaseException,
    Exception,
    ImportError,
    TypeError,
    ValueError,
    KeyError,
    IndexError,
    AttributeError,
    StopIteration,
    RuntimeError,
    OSError,
    AssertionError,
    BaseExceptionGroup,
    ExceptionGroup,
}

/// Immortal builtin exception type descriptors created during runtime init.
#[derive(Clone, Copy, Debug)]
pub struct ExceptionTypeSet {
    pub base_exception: *mut PyType,
    pub exception: *mut PyType,
    pub import_error: *mut PyType,
    pub type_error: *mut PyType,
    pub value_error: *mut PyType,
    pub key_error: *mut PyType,
    pub index_error: *mut PyType,
    pub attribute_error: *mut PyType,
    pub stop_iteration: *mut PyType,
    pub runtime_error: *mut PyType,
    pub os_error: *mut PyType,
    pub assertion_error: *mut PyType,
    pub base_exception_group: *mut PyType,
    pub exception_group: *mut PyType,
}

impl ExceptionTypeSet {
    /// Creates the builtin hierarchy rooted at `BaseException`.
    #[must_use]
    pub fn new(type_type: *mut PyType) -> Self {
        let base_exception = new_exception_type(type_type, "BaseException", ptr::null_mut());
        let exception = new_exception_type(type_type, "Exception", base_exception);
        let import_error = new_exception_type(type_type, "ImportError", exception);
        let type_error = new_exception_type(type_type, "TypeError", exception);
        let value_error = new_exception_type(type_type, "ValueError", exception);
        let key_error = new_exception_type(type_type, "KeyError", exception);
        let index_error = new_exception_type(type_type, "IndexError", exception);
        let attribute_error = new_exception_type(type_type, "AttributeError", exception);
        let stop_iteration = new_exception_type(type_type, "StopIteration", exception);
        let runtime_error = new_exception_type(type_type, "RuntimeError", exception);
        let os_error = new_exception_type(type_type, "OSError", exception);
        let assertion_error = new_exception_type(type_type, "AssertionError", exception);
        let base_exception_group = new_exception_type(type_type, "BaseExceptionGroup", base_exception);
        let exception_group = new_exception_type(type_type, "ExceptionGroup", base_exception_group);

        Self {
            base_exception,
            exception,
            import_error,
            type_error,
            value_error,
            key_error,
            index_error,
            attribute_error,
            stop_iteration,
            runtime_error,
            os_error,
            assertion_error,
            base_exception_group,
            exception_group,
        }
    }

    /// Returns the type descriptor for a builtin exception selector.
    #[must_use]
    pub fn get(self, kind: ExceptionKind) -> *mut PyType {
        match kind {
            ExceptionKind::BaseException => self.base_exception,
            ExceptionKind::Exception => self.exception,
            ExceptionKind::ImportError => self.import_error,
            ExceptionKind::TypeError => self.type_error,
            ExceptionKind::ValueError => self.value_error,
            ExceptionKind::KeyError => self.key_error,
            ExceptionKind::IndexError => self.index_error,
            ExceptionKind::AttributeError => self.attribute_error,
            ExceptionKind::StopIteration => self.stop_iteration,
            ExceptionKind::RuntimeError => self.runtime_error,
            ExceptionKind::OSError => self.os_error,
            ExceptionKind::AssertionError => self.assertion_error,
            ExceptionKind::BaseExceptionGroup => self.base_exception_group,
            ExceptionKind::ExceptionGroup => self.exception_group,
        }
    }

    /// Returns every core builtin exception type required by B05-EXC-CORE.
    #[must_use]
    pub fn core_types(self) -> [(ExceptionKind, *mut PyType); 14] {
        [
            (ExceptionKind::BaseException, self.base_exception),
            (ExceptionKind::Exception, self.exception),
            (ExceptionKind::ImportError, self.import_error),
            (ExceptionKind::TypeError, self.type_error),
            (ExceptionKind::ValueError, self.value_error),
            (ExceptionKind::KeyError, self.key_error),
            (ExceptionKind::IndexError, self.index_error),
            (ExceptionKind::AttributeError, self.attribute_error),
            (ExceptionKind::StopIteration, self.stop_iteration),
            (ExceptionKind::RuntimeError, self.runtime_error),
            (ExceptionKind::OSError, self.os_error),
            (ExceptionKind::AssertionError, self.assertion_error),
            (ExceptionKind::BaseExceptionGroup, self.base_exception_group),
            (ExceptionKind::ExceptionGroup, self.exception_group),
        ]
    }

    /// Returns true when `ty` is `BaseExceptionGroup`/`ExceptionGroup` or a subclass.
    #[must_use]
    pub unsafe fn is_exception_group_type(self, ty: *const PyType) -> bool {
        // SAFETY: Delegates to hierarchy traversal with the same caller contract.
        unsafe { is_exception_subclass(ty, self.base_exception_group.cast_const()) }
    }
}

fn new_exception_type(type_type: *mut PyType, name: &'static str, base: *mut PyType) -> *mut PyType {
    let mut ty = PyType::new(type_type.cast_const(), name, size_of::<PyBaseException>());
    ty.tp_base = base;
    ty.tp_getattro = Some(exception_getattro);
    Box::into_raw(Box::new(ty))
}

unsafe extern "C" fn exception_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        crate::thread_state::pon_err_set("exception attribute name must be str");
        return ptr::null_mut();
    };
    let exception = unsafe { &*object.cast::<PyBaseException>() };
    match name {
        "args" => {
            if exception.message.is_null() {
                crate::native::builtins_mod::alloc_tuple(Vec::new())
            } else {
                crate::native::builtins_mod::alloc_tuple(vec![exception.message])
            }
        }
        "value" => {
            let is_stop_iteration = unsafe {
                !exception.ob_base.ob_type.is_null()
                    && (*exception.ob_base.ob_type).name() == "StopIteration"
            };
            if is_stop_iteration {
                if exception.message.is_null() {
                    unsafe { crate::abi::pon_none() }
                } else {
                    exception.message
                }
            } else {
                unsafe { crate::abi::pon_raise_attribute_error(object, crate::intern::intern(name)) }
            }
        }
        "__cause__" => {
            if exception.cause.is_null() {
                unsafe { crate::abi::pon_none() }
            } else {
                exception.cause
            }
        }
        "__context__" => {
            if exception.context.is_null() {
                unsafe { crate::abi::pon_none() }
            } else {
                exception.context
            }
        }
        "__traceback__" => {
            if exception.traceback.is_null() {
                unsafe { crate::abi::pon_none() }
            } else {
                exception.traceback
            }
        }
        _ => unsafe { crate::abi::pon_raise_attribute_error(object, crate::intern::intern(name)) },
    }
}

/// Returns true when `sub` is `base` or inherits from it through `tp_base`.
///
/// # Safety
///
/// Non-NULL pointers must point to live `PyType` objects.
pub unsafe fn is_exception_subclass(mut sub: *const PyType, base: *const PyType) -> bool {
    if sub.is_null() || base.is_null() {
        return false;
    }

    while !sub.is_null() {
        if sub == base {
            return true;
        }
        // SAFETY: Caller guarantees that non-NULL `sub` is a live type object.
        sub = unsafe { (*sub).tp_base.cast_const() };
    }

    false
}

/// Returns true when `object` is a boxed exception instance matching `ty`.
///
/// # Safety
///
/// Non-NULL pointers must point to live boxed objects/type descriptors.
pub unsafe fn is_exception_instance(object: *mut PyObject, ty: *const PyType) -> bool {
    if object.is_null() {
        return false;
    }
    // SAFETY: Caller guarantees `object` is a live boxed object.
    let object_type = unsafe { (*object).ob_type };
    // SAFETY: Caller guarantees the object's type is a live type descriptor.
    unsafe { is_exception_subclass(object_type, ty) }
}

/// Casts a base-exception instance to the ABI object pointer.
#[must_use]
pub fn as_exception_object(exception: *mut PyBaseException) -> *mut PyObject {
    as_object_ptr(exception)
}

/// Traces the boxed pointers stored in a `PyBaseException`.
///
/// # Safety
///
/// `object` must be NULL or point to a live `PyBaseException` allocation.
pub unsafe extern "C" fn trace_base_exception(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }

    // SAFETY: The GC registered this callback only for `PyBaseException` allocations.
    let exception = unsafe { &*object.cast::<PyBaseException>() };
    for child in [exception.message, exception.cause, exception.context, exception.traceback] {
        if !child.is_null() {
            visitor(child.cast::<u8>());
        }
    }
}

const _: () = {
    assert!(offset_of!(PyBaseException, ob_base) == 0);
    assert!(size_of::<PyObject>() == size_of::<PyObjectHeader>());
};
