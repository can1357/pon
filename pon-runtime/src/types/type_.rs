//! Type object, heap-instance, class namespace, and class-building support.

use core::ffi::c_int;
use core::mem;
use core::ptr;
use std::collections::HashMap;

use crate::abi;
use crate::descr;
use crate::intern;
use crate::mro;
use crate::object::{PyObject, PyObjectHeader, PyType, PyUnicode, as_object_ptr, update_slot_from_dunder};
use crate::types::{list::PyList, tuple::PyTuple};
use crate::thread_state::pon_err_set;

/// Lightweight dictionary used for type dictionaries and instance dictionaries
/// until the mapping workstream's concrete `dict` object is wired in.
#[repr(C)]
#[derive(Debug)]
pub struct PyClassDict {
    /// Common object header.  The carrier is internal and may have a NULL type.
    pub ob_base: PyObjectHeader,
    entries: HashMap<u32, *mut PyObject>,
}

impl PyClassDict {
    /// Create an empty namespace dictionary.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ob_base: PyObjectHeader::new(ptr::null()),
            entries: HashMap::new(),
        }
    }

    /// Return a stored value by interned name.
    #[must_use]
    pub fn get(&self, name: u32) -> Option<*mut PyObject> {
        self.entries.get(&name).copied()
    }

    /// Store or replace one interned-name value.
    pub fn set(&mut self, name: u32, value: *mut PyObject) {
        self.entries.insert(name, value);
    }

    /// Delete one interned-name value.
    pub fn del(&mut self, name: u32) -> bool {
        self.entries.remove(&name).is_some()
    }

    /// Iterate over namespace entries.
    pub fn iter(&self) -> impl Iterator<Item = (u32, *mut PyObject)> + '_ {
        self.entries.iter().map(|(name, value)| (*name, *value))
    }
}

impl Default for PyClassDict {
    fn default() -> Self {
        Self::new()
    }
}

/// One heap-instance slot value configured by `__slots__`.
#[derive(Clone, Copy, Debug)]
pub struct PySlotValue {
    /// Interned slot name.
    pub name: u32,
    /// Current boxed value, or NULL when unassigned.
    pub value: *mut PyObject,
}

/// Generic heap instance used by Python classes.
#[repr(C)]
#[derive(Debug)]
pub struct PyHeapInstance {
    /// Common object header; must remain first.
    pub ob_base: PyObjectHeader,
    /// Per-instance dictionary, or NULL for slot-only instances.
    pub dict: *mut PyClassDict,
    /// Slot storage in class-defined order.
    pub slots: Vec<PySlotValue>,
}

/// Metadata carrier installed in a heap type dictionary for slot descriptors.
#[repr(C)]
#[derive(Debug)]
pub struct PyMemberDescriptor {
    /// Common object header.
    pub ob_base: PyObjectHeader,
    /// Interned slot name.
    pub name: u32,
}

/// Class keyword argument pair consumed by the runtime class builder.
#[derive(Clone, Copy, Debug)]
pub struct ClassKeyword {
    /// Interned keyword name.
    pub name: u32,
    /// Evaluated keyword value.
    pub value: *mut PyObject,
}

fn raise_object(message: impl Into<String>) -> *mut PyObject {
    pon_err_set(message);
    ptr::null_mut()
}

/// Allocate an empty class/instance namespace carrier.
#[must_use]
pub fn new_namespace() -> *mut PyClassDict {
    Box::into_raw(Box::new(PyClassDict::new()))
}

/// Best-effort extraction of UTF-8 text from a runtime string object.
#[must_use]
pub unsafe fn unicode_text<'a>(object: *mut PyObject) -> Option<&'a str> {
    if object.is_null() {
        return None;
    }
    let ty = unsafe { (*object).ob_type };
    if ty.is_null() || unsafe { (*ty).name() != "str" } {
        return None;
    }
    unsafe { (*object.cast::<PyUnicode>()).as_str() }
}

unsafe fn object_type(object: *mut PyObject) -> *mut PyType {
    if object.is_null() {
        ptr::null_mut()
    } else {
        unsafe { (*object).ob_type.cast_mut() }
    }
}

/// Copy positional arguments out of the tuple/list carrier used by CPython-style
/// call slots. A NULL carrier represents zero explicit positional arguments.
pub unsafe fn positional_args_from_object(args: *mut PyObject) -> Result<Vec<*mut PyObject>, String> {
    if args.is_null() {
        return Ok(Vec::new());
    }
    let ty = unsafe { object_type(args) };
    if ty.is_null() {
        return Err("call argument carrier has no type".to_owned());
    }
    match unsafe { (*ty).name() } {
        "tuple" => Ok(unsafe { (&*args.cast::<PyTuple>()).as_slice() }.to_vec()),
        "list" => Ok(unsafe { (&*args.cast::<PyList>()).as_slice() }.to_vec()),
        _ => Err("call argument carrier must be a tuple or list".to_owned()),
    }
}

fn leak_type_name(name: &str) -> &'static str {
    Box::leak(name.to_owned().into_boxed_str())
}

unsafe fn normalize_bases(bases: &[*mut PyObject]) -> Option<Vec<*mut PyType>> {
    let mut out = Vec::with_capacity(bases.len());
    for base in bases {
        if base.is_null() {
            pon_err_set("class base is NULL");
            return None;
        }
        out.push(base.cast::<PyType>());
    }
    Some(out)
}

unsafe fn metaclass_from_bases(bases: &[*mut PyType], explicit: *mut PyObject) -> *mut PyType {
    if !explicit.is_null() {
        return explicit.cast::<PyType>();
    }
    bases
        .first()
        .map(|base| unsafe { object_type((*base).cast::<PyObject>()) })
        .filter(|meta| !meta.is_null())
        .unwrap_or(ptr::null_mut())
}

fn slot_names_from_namespace(namespace: &PyClassDict) -> Vec<u32> {
    let slots_name = intern::intern("__slots__");
    let Some(raw) = namespace.get(slots_name) else {
        return Vec::new();
    };
    unsafe {
        if let Some(text) = unicode_text(raw) {
            return text
                .split_whitespace()
                .filter(|name| *name != "__dict__")
                .map(intern::intern)
                .collect();
        }
    }
    Vec::new()
}

fn namespace_allows_dict(namespace: &PyClassDict) -> bool {
    let slots_name = intern::intern("__slots__");
    let Some(raw) = namespace.get(slots_name) else {
        return true;
    };
    unsafe { unicode_text(raw).is_some_and(|text| text.split_whitespace().any(|slot| slot == "__dict__")) }
}

/// Allocate a heap type with a pre-populated namespace and C3 MRO.
#[must_use]
pub unsafe fn build_class_from_namespace(
    name: &str,
    bases: &[*mut PyObject],
    namespace: *mut PyClassDict,
    keywords: &[ClassKeyword],
) -> *mut PyObject {
    if namespace.is_null() {
        return raise_object("class namespace is NULL");
    }
    let Some(base_types) = (unsafe { normalize_bases(bases) }) else {
        return ptr::null_mut();
    };
    let explicit_meta = keywords
        .iter()
        .find(|keyword| intern::resolve(keyword.name).as_deref() == Some("metaclass"))
        .map(|keyword| keyword.value)
        .unwrap_or(ptr::null_mut());
    let metaclass = unsafe { metaclass_from_bases(&base_types, explicit_meta) };

    let static_name = leak_type_name(name);
    let mut ty = PyType::new(metaclass, static_name, mem::size_of::<PyHeapInstance>());
    ty.tp_base = base_types.first().copied().unwrap_or(ptr::null_mut());
    ty.tp_bases = ptr::null_mut();
    ty.tp_dict = namespace.cast::<PyObject>();
    ty.tp_dictoffset = if namespace_allows_dict(unsafe { &*namespace }) { 1 } else { 0 };
    ty.tp_getattro = Some(descr::generic_get_attr);
    ty.tp_setattro = Some(descr::generic_set_attr);
    ty.tp_new = Some(type_new);
    ty.tp_init = Some(type_init);

    let ty = Box::into_raw(Box::new(ty));
    if unsafe { mro::set_c3_mro(ty, &base_types) } < 0 {
        return ptr::null_mut();
    }

    install_slot_descriptors(ty, namespace, slot_names_from_namespace(unsafe { &*namespace }));
    for (name, value) in unsafe { (&*namespace).iter().collect::<Vec<_>>() } {
        let _ = update_slot_from_dunder(ty, name, value);
    }
    ty.cast::<PyObject>()
}

fn install_slot_descriptors(ty: *mut PyType, namespace: *mut PyClassDict, slots: Vec<u32>) {
    if slots.is_empty() || namespace.is_null() {
        return;
    }
    for slot in slots {
        let descr = Box::into_raw(Box::new(PyMemberDescriptor {
            ob_base: PyObjectHeader::new(unsafe { (*ty).ob_base.ob_type }),
            name: slot,
        }));
        unsafe {
            (&mut *namespace).set(slot, descr.cast::<PyObject>());
        }
    }
}

/// Generic `type.__new__` used for ordinary Python classes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn type_new(cls: *mut PyType, _args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
    if cls.is_null() {
        return raise_object("cannot instantiate NULL type");
    }
    let dict = if unsafe { (*cls).tp_dictoffset != 0 } {
        new_namespace()
    } else {
        ptr::null_mut()
    };
    let slots = slot_storage(cls);
    let object = Box::into_raw(Box::new(PyHeapInstance {
        ob_base: PyObjectHeader::new(cls),
        dict,
        slots,
    }));
    as_object_ptr(object)
}

fn slot_storage(cls: *mut PyType) -> Vec<PySlotValue> {
    if cls.is_null() {
        return Vec::new();
    }
    let mut slots = Vec::new();
    for ty in unsafe { mro::mro_entries(cls) } {
        if ty.is_null() {
            continue;
        }
        let dict = unsafe { (*ty).tp_dict.cast::<PyClassDict>() };
        if dict.is_null() {
            continue;
        }
        for (name, value) in unsafe { (&*dict).iter() } {
            if !value.is_null() && unsafe { (*value).ob_type == (*ty).ob_base.ob_type } {
                if !slots.iter().any(|slot: &PySlotValue| slot.name == name) {
                    slots.push(PySlotValue {
                        name,
                        value: ptr::null_mut(),
                    });
                }
            }
        }
    }
    slots
}

/// Generic no-op initializer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn type_init(_self: *mut PyObject, _args: *mut PyObject, _kwargs: *mut PyObject) -> c_int {
    0
}

/// `type.__call__`: allocate via `__new__`, then invoke `__init__` when present.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn type_call(cls_obj: *mut PyObject, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    if cls_obj.is_null() {
        return raise_object("type call receiver is NULL");
    }
    if !kwargs.is_null() {
        return raise_object("type.__call__ keyword carrier is not supported yet");
    }
    let cls = cls_obj.cast::<PyType>();
    let explicit_args = match unsafe { positional_args_from_object(args) } {
        Ok(args) => args,
        Err(message) => return raise_object(message),
    };
    let new = unsafe { (*cls).tp_new.unwrap_or(type_new) };
    let instance = unsafe { new(cls, args, kwargs) };
    if instance.is_null() {
        return ptr::null_mut();
    }

    let init_name = intern::intern("__init__");
    let init = unsafe { descr::lookup_in_type(cls, init_name) };
    if !init.is_null() {
        let bound = unsafe { descr::descriptor_get(init, instance, cls) };
        if bound.is_null() {
            return ptr::null_mut();
        }
        let mut argv = explicit_args;
        let result = unsafe {
            abi::pon_call(
                bound,
                if argv.is_empty() { ptr::null_mut() } else { argv.as_mut_ptr() },
                argv.len(),
            )
        };
        if result.is_null() {
            return ptr::null_mut();
        }
    } else if let Some(init_slot) = unsafe { (*cls).tp_init } {
        if unsafe { init_slot(instance, args, kwargs) } < 0 {
            return ptr::null_mut();
        }
    }
    instance
}

/// Store/delete an instance slot.  Returns true when the name was handled.
pub unsafe fn instance_set_slot(instance: *mut PyHeapInstance, name: u32, value: *mut PyObject) -> bool {
    if instance.is_null() {
        return false;
    }
    let instance = unsafe { &mut *instance };
    let Some(slot) = instance.slots.iter_mut().find(|slot| slot.name == name) else {
        return false;
    };
    slot.value = value;
    true
}

/// Load an instance slot by interned name.
#[must_use]
pub unsafe fn instance_get_slot(instance: *mut PyHeapInstance, name: u32) -> *mut PyObject {
    if instance.is_null() {
        return ptr::null_mut();
    }
    unsafe { (&*instance).slots.iter().find(|slot| slot.name == name).map(|slot| slot.value).unwrap_or(ptr::null_mut()) }
}

/// Core hook for `issubclass` using C3 MRO.
pub unsafe fn issubclass(cls: *mut PyObject, base: *mut PyObject) -> c_int {
    unsafe { descr::issubclass(cls, base) }
}

/// Core hook for `isinstance` using object type plus C3 MRO.
pub unsafe fn isinstance(obj: *mut PyObject, cls: *mut PyObject) -> c_int {
    unsafe { descr::isinstance(obj, cls) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_namespace_stores_attrs_and_dunders() {
        let mut type_type = PyType::new(ptr::null(), "type", mem::size_of::<PyType>());
        let type_ptr = &mut type_type as *mut PyType;
        type_type.ob_base.ob_type = type_ptr;
        let ns = new_namespace();
        let value = 1usize as *mut PyObject;
        unsafe {
            (&mut *ns).set(intern::dunder_call(), value);
            let cls = build_class_from_namespace("C", &[], ns, &[]).cast::<PyType>();
            assert!(!cls.is_null());
            assert_eq!((*cls).dunder_slots.call, value);
        }
    }

    #[test]
    fn slot_only_instance_rejects_unknown_dict_storage() {
        let ns = new_namespace();
        unsafe {
            (&mut *ns).set(intern::intern("__slots__"), fake_str("x"));
            let cls = build_class_from_namespace("S", &[], ns, &[]).cast::<PyType>();
            assert!(!cls.is_null());
            let obj = type_new(cls, ptr::null_mut(), ptr::null_mut()).cast::<PyHeapInstance>();
            assert!(!obj.is_null());
            assert!((*obj).dict.is_null());
            assert!(instance_set_slot(obj, intern::intern("x"), 2usize as *mut PyObject));
            assert_eq!(instance_get_slot(obj, intern::intern("x")), 2usize as *mut PyObject);
        }
    }

    unsafe fn fake_str(text: &'static str) -> *mut PyObject {
        static mut STR_TYPE: PyType = PyType::new(ptr::null(), "str", mem::size_of::<PyUnicode>());
        let ptr = unsafe { &raw mut STR_TYPE };
        unsafe { (*ptr).ob_base.ob_type = ptr };
        Box::into_raw(Box::new(PyUnicode {
            ob_base: PyObjectHeader::new(ptr),
            len: text.len(),
            data: text.as_ptr(),
            owns_data: false,
        }))
        .cast::<PyObject>()
    }
}
