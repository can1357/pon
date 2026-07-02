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

/// `tp_getattro` for property instances: serves the descriptor-protocol
/// dunders (`__get__`/`__set__`/`__delete__`) as callable bound methods plus
/// the `fget`/`fset`/`fdel`/`__doc__` fields; anything else falls through to
/// the generic path (pre-existing behavior).  enum's `_is_descriptor`
/// classifies class-body values via `hasattr(value, '__get__')`, so the
/// protocol must be visible as ATTRIBUTES, not only through the type slots
/// (`@property` members of `_simple_enum` classes — the http import chain).
unsafe extern "C" fn property_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(text) = (unsafe { crate::types::type_::unicode_text(crate::tag::untag_arg(name)) }) else {
        let message = "attribute name must be str";
        return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    };
    let property = unsafe { &*object.cast::<PyProperty>() };
    let field = |value: *mut PyObject| {
        if value.is_null() {
            // SAFETY: Singleton accessor.
            unsafe { abi::pon_none() }
        } else {
            value
        }
    };
    match text {
        "__get__" => bound_entry(object, text, property_dunder_get_entry),
        "__set__" => bound_entry(object, text, property_dunder_set_entry),
        "__delete__" => bound_entry(object, text, property_dunder_delete_entry),
        "fget" => field(property.fget),
        "fset" => field(property.fset),
        "fdel" => field(property.fdel),
        "__doc__" => field(property.doc),
        "getter" => bound_entry(object, text, property_getter_entry),
        "setter" => bound_entry(object, text, property_setter_entry),
        "deleter" => bound_entry(object, text, property_deleter_entry),
        // Universal `__class__` (the slotless-native default this getattro
        // replaced).  NO `generic_get_attr` fallback: the property type is a
        // metatype-less native whose instances carry no dict storage, so the
        // generic MRO/instance-dict walk misreads the PyProperty layout.
        "__class__" => unsafe { (*object).ob_type.cast_mut().cast::<PyObject>() },
        _ => {
            let message = format!("'property' object has no attribute '{text}'");
            crate::abi::exc::raise_kind_error_text(crate::types::exc::ExceptionKind::AttributeError, &message)
        }
    }
}

/// `tp_setattro` for property instances: `__doc__` is a writable member
/// (CPython parity — dis.py assigns namedtuple field docs onto the
/// pure-Python `_tuplegetter` property fallback, sched.py sets
/// `Event.*.__doc__`); a NULL value is deletion and reads back as None.
/// Everything else is rejected INLINE with CPython's texts — property
/// instances carry no dict storage, so the generic walk must never run
/// (same layout trap as `property_getattro`).
unsafe extern "C" fn property_setattro(object: *mut PyObject, name: *mut PyObject, value: *mut PyObject) -> c_int {
    let Some(text) = (unsafe { crate::types::type_::unicode_text(crate::tag::untag_arg(name)) }) else {
        let message = "attribute name must be str";
        unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
        return -1;
    };
    let raise_attribute_error = |message: &str| -> c_int {
        crate::abi::exc::raise_kind_error_text(crate::types::exc::ExceptionKind::AttributeError, message);
        -1
    };
    match text {
        "__doc__" => {
            // SAFETY: The receiver is a live PyProperty; the value (possibly
            // NULL for deletion) is stored verbatim, matching `new_property`.
            unsafe { (*object.cast::<PyProperty>()).doc = value };
            0
        }
        "fget" | "fset" | "fdel" => raise_attribute_error("readonly attribute"),
        _ => raise_attribute_error(&format!(
            "'property' object has no attribute '{text}' and no __dict__ for setting new attributes"
        )),
    }
}

/// Binds `entry` to `receiver` as a method pair (receiver rides in `argv[0]`).
fn bound_entry(
    receiver: *mut PyObject,
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> *mut PyObject {
    // SAFETY: `entry` is a live builtin entry point with the runtime calling
    // convention.
    let function =
        unsafe { abi::pon_make_function(entry as *const u8, crate::builtins::variadic_arity(), crate::intern::intern(name)) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match crate::types::method::new_bound_method(function, receiver) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => raise_property(&message),
    }
}

unsafe fn entry_args<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
    if argv.is_null() {
        return (argc == 0).then_some(&[]);
    }
    Some(unsafe { core::slice::from_raw_parts(argv.cast_const(), argc) })
}

fn raise_arity(message: &str) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(crate::types::exc::ExceptionKind::TypeError, message)
}

/// True when `object` is the `None` singleton (tag-tolerant).
fn is_none_arg(object: *mut PyObject) -> bool {
    // SAFETY: Singleton accessor.
    crate::tag::untag_arg(object) == unsafe { abi::pon_none() }
}

/// `property.__get__(instance, owner=None)`: a `None`/absent instance returns
/// the property itself (CPython class-access semantics).
unsafe extern "C" fn property_dunder_get_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { entry_args(argv, argc) }) else {
        return raise_property("property.__get__ received a NULL argv pointer");
    };
    let (&receiver, rest) = args.split_first().unwrap_or((&ptr::null_mut(), &[]));
    if rest.is_empty() || rest.len() > 2 {
        return raise_arity("__get__(instance, owner=None) takes 1 or 2 arguments");
    }
    let obj = if is_none_arg(rest[0]) { ptr::null_mut() } else { rest[0] };
    let owner = rest.get(1).copied().unwrap_or(ptr::null_mut());
    // SAFETY: Slot implementation follows the NULL-sentinel error contract.
    unsafe { property_descr_get(receiver, obj, owner) }
}

/// `property.__set__(instance, value)`.
unsafe extern "C" fn property_dunder_set_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { entry_args(argv, argc) }) else {
        return raise_property("property.__set__ received a NULL argv pointer");
    };
    let &[receiver, obj, value] = args else {
        return raise_arity("__set__(instance, value) takes exactly 2 arguments");
    };
    // SAFETY: Slot implementation follows the negative-status error contract.
    if unsafe { property_descr_set(receiver, obj, value) } < 0 {
        return ptr::null_mut();
    }
    // SAFETY: Singleton accessor.
    unsafe { abi::pon_none() }
}

/// `property.__delete__(instance)`.
unsafe extern "C" fn property_dunder_delete_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { entry_args(argv, argc) }) else {
        return raise_property("property.__delete__ received a NULL argv pointer");
    };
    let &[receiver, obj] = args else {
        return raise_arity("__delete__(instance) takes exactly 1 argument");
    };
    // SAFETY: Slot implementation follows the negative-status error contract.
    if unsafe { property_descr_set(receiver, obj, ptr::null_mut()) } < 0 {
        return ptr::null_mut();
    }
    // SAFETY: Singleton accessor.
    unsafe { abi::pon_none() }
}

/// Which accessor `property_copy_with` replaces.
#[derive(Clone, Copy)]
enum AccessorSlot {
    Get,
    Set,
    Del,
}

/// `property.getter/setter/deleter(callable)`: a copy of the property with
/// one accessor replaced and the doc carried over (CPython
/// `type(self)(fget, fset, fdel, doc)` semantics); `None` clears the slot.
unsafe extern "C" fn property_copy_with(
    argv: *mut *mut PyObject,
    argc: usize,
    slot: AccessorSlot,
    name: &str,
) -> *mut PyObject {
    let Some(args) = (unsafe { entry_args(argv, argc) }) else {
        return raise_property("property accessor decorator received a NULL argv pointer");
    };
    let (&receiver, rest) = args.split_first().unwrap_or((&ptr::null_mut(), &[]));
    if receiver.is_null() {
        return raise_property("property accessor decorator is missing its receiver");
    }
    if rest.len() != 1 {
        return raise_arity(&format!("{name}(accessor) takes exactly 1 argument"));
    }
    let accessor = if is_none_arg(rest[0]) { ptr::null_mut() } else { rest[0] };
    let property = unsafe { &*receiver.cast::<PyProperty>() };
    let (mut fget, mut fset, mut fdel) = (property.fget, property.fset, property.fdel);
    match slot {
        AccessorSlot::Get => fget = accessor,
        AccessorSlot::Set => fset = accessor,
        AccessorSlot::Del => fdel = accessor,
    }
    // SAFETY: The receiver's own type stands in for `type(self)`.
    unsafe { new_property((*receiver).ob_type, fget, fset, fdel, property.doc) }
}

unsafe extern "C" fn property_getter_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { property_copy_with(argv, argc, AccessorSlot::Get, "getter") }
}

unsafe extern "C" fn property_setter_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { property_copy_with(argv, argc, AccessorSlot::Set, "setter") }
}

unsafe extern "C" fn property_deleter_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { property_copy_with(argv, argc, AccessorSlot::Del, "deleter") }
}

/// Populate the slots on a `property` type descriptor.
pub fn install_property_slots(ty: &mut PyType) {
    ty.tp_descr_get = Some(property_descr_get);
    ty.tp_descr_set = Some(property_descr_set);
    ty.tp_getattro = Some(property_getattro);
    ty.tp_setattro = Some(property_setattro);
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
