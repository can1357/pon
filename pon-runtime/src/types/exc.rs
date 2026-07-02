//! Boxed exception objects and the Phase-B builtin exception type hierarchy.
//!
//! Exception instances are ordinary boxed Python objects with no refcount field.
//! The runtime owns allocation through `pon-gc`; this module only defines the
//! layout, immortal type descriptors, and hierarchy queries shared by ABI helpers.

use core::mem::{offset_of, size_of};
use core::ptr;
use std::sync::LazyLock;

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

/// Boxed exception-group payload: a BaseException plus its immutable member tuple.
#[repr(C)]
#[derive(Debug)]
pub struct PyExceptionGroup {
    /// Common exception payload; must remain first.
    pub base: PyBaseException,
    /// Boxed tuple of member exceptions. Non-NULL for valid groups.
    pub exceptions: *mut PyObject,
}

#[repr(C)]
#[derive(Debug)]
pub struct PyExceptionGroupMethod {
    pub ob_base: PyObjectHeader,
    pub receiver: *mut PyObject,
    pub kind: u8,
}

pub const EXC_GROUP_METHOD_SPLIT: u8 = 0;
pub const EXC_GROUP_METHOD_SUBGROUP: u8 = 1;
pub const EXC_GROUP_METHOD_DERIVE: u8 = 2;

/// Builtin exception class selector used by raising helpers and tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExceptionKind {
    BaseException,
    Exception,
    ImportError,
    EOFError,
    TypeError,
    ValueError,
    KeyError,
    IndexError,
    NameError,
    UnboundLocalError,
    NotImplementedError,
    AttributeError,
    StopIteration,
    GeneratorExit,
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
    pub eof_error: *mut PyType,
    pub type_error: *mut PyType,
    pub value_error: *mut PyType,
    pub key_error: *mut PyType,
    pub index_error: *mut PyType,
    pub attribute_error: *mut PyType,
    pub name_error: *mut PyType,
    pub unbound_local_error: *mut PyType,
    pub not_implemented_error: *mut PyType,
    pub stop_iteration: *mut PyType,
    pub generator_exit: *mut PyType,
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
        let eof_error = new_exception_type(type_type, "EOFError", exception);
        let type_error = new_exception_type(type_type, "TypeError", exception);
        let value_error = new_exception_type(type_type, "ValueError", exception);
        let key_error = new_exception_type(type_type, "KeyError", exception);
        let index_error = new_exception_type(type_type, "IndexError", exception);
        let attribute_error = new_exception_type(type_type, "AttributeError", exception);
        let name_error = new_exception_type(type_type, "NameError", exception);
        let unbound_local_error = new_exception_type(type_type, "UnboundLocalError", name_error);
        let runtime_error = new_exception_type(type_type, "RuntimeError", exception);
        let not_implemented_error = new_exception_type(type_type, "NotImplementedError", runtime_error);
        let stop_iteration = new_exception_type(type_type, "StopIteration", exception);
        let generator_exit = new_exception_type(type_type, "GeneratorExit", base_exception);
        let os_error = new_exception_type(type_type, "OSError", exception);
        let assertion_error = new_exception_type(type_type, "AssertionError", exception);
        let base_exception_group = new_exception_group_type(type_type, "BaseExceptionGroup", base_exception);
        let exception_group = new_exception_group_type(type_type, "ExceptionGroup", base_exception_group);

        Self {
            base_exception,
            exception,
            import_error,
            eof_error,
            type_error,
            value_error,
            key_error,
            index_error,
            attribute_error,
            name_error,
            unbound_local_error,
            not_implemented_error,
            stop_iteration,
            generator_exit,
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
            ExceptionKind::EOFError => self.eof_error,
            ExceptionKind::TypeError => self.type_error,
            ExceptionKind::ValueError => self.value_error,
            ExceptionKind::KeyError => self.key_error,
            ExceptionKind::IndexError => self.index_error,
            ExceptionKind::AttributeError => self.attribute_error,
            ExceptionKind::NameError => self.name_error,
            ExceptionKind::UnboundLocalError => self.unbound_local_error,
            ExceptionKind::NotImplementedError => self.not_implemented_error,
            ExceptionKind::StopIteration => self.stop_iteration,
            ExceptionKind::GeneratorExit => self.generator_exit,
            ExceptionKind::RuntimeError => self.runtime_error,
            ExceptionKind::OSError => self.os_error,
            ExceptionKind::AssertionError => self.assertion_error,
            ExceptionKind::BaseExceptionGroup => self.base_exception_group,
            ExceptionKind::ExceptionGroup => self.exception_group,
        }
    }

    /// Returns every core builtin exception type required by B05-EXC-CORE and wave-2 compat.
    #[must_use]
    pub fn core_types(self) -> [(ExceptionKind, *mut PyType); 19] {
        [
            (ExceptionKind::BaseException, self.base_exception),
            (ExceptionKind::Exception, self.exception),
            (ExceptionKind::ImportError, self.import_error),
            (ExceptionKind::EOFError, self.eof_error),
            (ExceptionKind::TypeError, self.type_error),
            (ExceptionKind::ValueError, self.value_error),
            (ExceptionKind::KeyError, self.key_error),
            (ExceptionKind::IndexError, self.index_error),
            (ExceptionKind::AttributeError, self.attribute_error),
            (ExceptionKind::NameError, self.name_error),
            (ExceptionKind::UnboundLocalError, self.unbound_local_error),
            (ExceptionKind::NotImplementedError, self.not_implemented_error),
            (ExceptionKind::StopIteration, self.stop_iteration),
            (ExceptionKind::GeneratorExit, self.generator_exit),
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

fn new_exception_group_type(type_type: *mut PyType, name: &'static str, base: *mut PyType) -> *mut PyType {
    let mut ty = PyType::new(type_type.cast_const(), name, size_of::<PyExceptionGroup>());
    ty.tp_base = base;
    ty.tp_getattro = Some(exception_getattro);
    Box::into_raw(Box::new(ty))
}

fn exception_group_method_type() -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(ptr::null(), "exception_group_method", size_of::<PyExceptionGroupMethod>());
        ty.tp_call = Some(exception_group_method_call);
        Box::into_raw(Box::new(ty)) as usize
    });
    *TYPE as *mut PyType
}

#[must_use]
pub fn new_exception_group_method(receiver: *mut PyObject, kind: u8) -> *mut PyObject {
    Box::into_raw(Box::new(PyExceptionGroupMethod {
        ob_base: PyObjectHeader::new(exception_group_method_type()),
        receiver,
        kind,
    }))
    .cast::<PyObject>()
}

unsafe extern "C" fn exception_group_method_call(callee: *mut PyObject, args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
    if callee.is_null() {
        crate::thread_state::pon_err_set("exception group method receiver is NULL");
        return ptr::null_mut();
    }
    let method = unsafe { &*callee.cast::<PyExceptionGroupMethod>() };
    unsafe { crate::abi::exc::call_exception_group_method(method.receiver, method.kind, args) }
}

#[must_use]
pub unsafe fn is_exception_group_type_ptr(mut ty: *const PyType) -> bool {
    while !ty.is_null() {
        let name = unsafe { (*ty).name() };
        if name == "BaseExceptionGroup" || name == "ExceptionGroup" {
            return true;
        }
        ty = unsafe { (*ty).tp_base.cast_const() };
    }
    false
}

#[must_use]
pub unsafe fn is_exception_group_instance(object: *mut PyObject) -> bool {
    !object.is_null() && unsafe { is_exception_group_type_ptr((*object).ob_type) }
}

#[must_use]
pub unsafe fn as_exception_group<'a>(object: *mut PyObject) -> Option<&'a PyExceptionGroup> {
    if unsafe { is_exception_group_instance(object) } {
        Some(unsafe { &*object.cast::<PyExceptionGroup>() })
    } else {
        None
    }
}

unsafe extern "C" fn exception_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        crate::thread_state::pon_err_set("exception attribute name must be str");
        return ptr::null_mut();
    };
    let exception = unsafe { &*object.cast::<PyBaseException>() };
    let is_group = unsafe { is_exception_group_instance(object) };
    match name {
        "args" => {
            if is_group {
                let group = unsafe { &*object.cast::<PyExceptionGroup>() };
                crate::native::builtins_mod::alloc_tuple(vec![exception.message, group.exceptions])
            } else if exception.message.is_null() {
                crate::native::builtins_mod::alloc_tuple(Vec::new())
            } else {
                crate::native::builtins_mod::alloc_tuple(vec![exception.message])
            }
        }
        "message" => {
            if exception.message.is_null() {
                unsafe { crate::abi::pon_none() }
            } else {
                exception.message
            }
        }
        "exceptions" if is_group => unsafe { (&*object.cast::<PyExceptionGroup>()).exceptions },
        "split" if is_group => new_exception_group_method(object, EXC_GROUP_METHOD_SPLIT),
        "subgroup" if is_group => new_exception_group_method(object, EXC_GROUP_METHOD_SUBGROUP),
        "derive" if is_group => new_exception_group_method(object, EXC_GROUP_METHOD_DERIVE),
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

    let wants_exception = unsafe { (*base).name() == "Exception" };
    while !sub.is_null() {
        if sub == base {
            return true;
        }
        if wants_exception && unsafe { (*sub).name() == "ExceptionGroup" } {
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

/// Traces the boxed pointers stored in a `PyExceptionGroup`.
///
/// # Safety
///
/// `object` must be NULL or point to a live `PyExceptionGroup` allocation.
pub unsafe extern "C" fn trace_exception_group(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    unsafe { trace_base_exception(object, visitor) };
    let group = unsafe { &*object.cast::<PyExceptionGroup>() };
    if !group.exceptions.is_null() {
        visitor(group.exceptions.cast::<u8>());
    }
}

const _: () = {
    assert!(offset_of!(PyBaseException, ob_base) == 0);
    assert!(offset_of!(PyExceptionGroup, base) == 0);
    assert!(size_of::<PyObject>() == size_of::<PyObjectHeader>());
};
