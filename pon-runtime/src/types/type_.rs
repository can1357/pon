//! Type object, heap-instance, class namespace, and class-building support.

use core::ffi::c_int;
use core::mem;
use core::ptr;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use pon_gc::TypeId;

use crate::abi;
use crate::descr;
use crate::intern;
use crate::mro;
use crate::object::{PyObject, PyObjectHeader, PyType, PyUnicode, as_object_ptr, update_slot_from_dunder};
use crate::types::{dict, function, list::PyList, tuple::PyTuple};
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
    /// Head of the runtime weakref list (owned by J4c weakref/finalization).
    pub weakrefs: *mut PyObject,
    /// Exactly-once finalization guard for `__del__`/weakref clearing.
    pub finalized: bool,
}

/// Slot descriptor flavor stored in a heap type namespace.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PyMemberKind {
    /// Normal `__slots__` data descriptor backed by [`PyHeapInstance::slots`].
    Slot = 0,
    /// Synthetic `__dict__` descriptor; instances carry a dictionary pointer.
    Dict = 1,
}

/// Metadata carrier installed in a heap type dictionary for slot descriptors.
#[repr(C)]
#[derive(Debug)]
pub struct PyMemberDescriptor {
    /// Common object header.
    pub ob_base: PyObjectHeader,
    /// Type that owns this descriptor.
    pub owner: *mut PyType,
    /// Interned slot name.
    pub name: u32,
    /// Descriptor flavor.
    pub kind: PyMemberKind,
}

/// Class keyword argument pair consumed by the runtime class builder.
#[derive(Clone, Copy, Debug)]
pub struct ClassKeyword {
    /// Interned keyword name.
    pub name: u32,
    /// Evaluated keyword value.
    pub value: *mut PyObject,
}

/// GC type id for Python heap instances allocated by [`type_new`].
pub const TYPE_ID_HEAP_INSTANCE: TypeId = TypeId(7);

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

unsafe fn object_type_display(object: *mut PyObject) -> String {
    let ty = unsafe { object_type(object) };
    if ty.is_null() {
        "NULL".to_owned()
    } else {
        unsafe { (*ty).name() }.to_owned()
    }
}

unsafe fn normalize_bases(bases: &[*mut PyObject]) -> Option<Vec<*mut PyType>> {
    let needs_mro_entries = bases.iter().copied().any(|base| unsafe { !is_type_object(base) });
    let mut original_bases = bases.to_vec();
    let original_tuple = if needs_mro_entries {
        unsafe {
            abi::seq::pon_build_tuple(
                if original_bases.is_empty() {
                    ptr::null_mut()
                } else {
                    original_bases.as_mut_ptr()
                },
                original_bases.len(),
            )
        }
    } else {
        ptr::null_mut()
    };
    if needs_mro_entries && original_tuple.is_null() {
        return None;
    }

    let mut out = Vec::with_capacity(bases.len());
    for base in bases.iter().copied() {
        if base.is_null() {
            pon_err_set("class base is NULL");
            return None;
        }
        if unsafe { is_type_object(base) } {
            out.push(base.cast::<PyType>());
            continue;
        }
        let replacements = match unsafe { resolve_mro_entries(base, original_tuple) } {
            Ok(replacements) => replacements,
            Err(message) => {
                pon_err_set(message);
                return None;
            }
        };
        for replacement in replacements {
            if replacement.is_null() || unsafe { !is_type_object(replacement) } {
                pon_err_set("__mro_entries__ must return a tuple of classes");
                return None;
            }
            out.push(replacement.cast::<PyType>());
        }
    }
    Some(out)
}

unsafe fn resolve_mro_entries(base: *mut PyObject, original_bases: *mut PyObject) -> Result<Vec<*mut PyObject>, String> {
    let base_ty = unsafe { object_type(base) };
    if base_ty.is_null() {
        return Err("class base has no type".to_owned());
    }
    let method = unsafe { descr::lookup_in_type(base_ty, intern::intern("__mro_entries__")) };
    if method.is_null() {
        return Err(format!("{} is not an acceptable base type", unsafe { object_type_display(base) }));
    }
    let bound = unsafe { descr::descriptor_get(method, base, base_ty) };
    if bound.is_null() {
        return Err("__mro_entries__ descriptor binding failed".to_owned());
    }
    let mut argv = [original_bases];
    let replacement = unsafe { abi::pon_call(bound, argv.as_mut_ptr(), 1) };
    if replacement.is_null() {
        return Err("__mro_entries__ failed".to_owned());
    }
    unsafe { positional_args_from_object(replacement) }.map_err(|_| "__mro_entries__ must return a tuple".to_owned())
}

unsafe fn select_metaclass(bases: &[*mut PyType], explicit: *mut PyObject) -> Option<*mut PyType> {
    let mut winner = if explicit.is_null() {
        bases
            .first()
            .map(|base| unsafe { object_type((*base).cast::<PyObject>()) })
            .filter(|meta| !meta.is_null())
            .unwrap_or_else(abi::runtime_type_type)
    } else if unsafe { is_type_object(explicit) } {
        explicit.cast::<PyType>()
    } else {
        pon_err_set("metaclass must be a type");
        return None;
    };

    for base in bases.iter().copied() {
        let base_meta = unsafe { object_type(base.cast::<PyObject>()) };
        if base_meta.is_null() {
            continue;
        }
        if winner.is_null() {
            winner = base_meta;
            continue;
        }
        if unsafe { mro::is_subtype(winner, base_meta) } {
            continue;
        }
        if unsafe { mro::is_subtype(base_meta, winner) } {
            winner = base_meta;
            continue;
        }
        pon_err_set("metaclass conflict: the metaclass of a derived class must be a (non-strict) subclass of the metaclasses of all its bases");
        return None;
    }
    Some(winner)
}

#[derive(Clone, Debug, Default)]
struct SlotSpec {
    declared: bool,
    names: Vec<u32>,
    wants_dict: bool,
}

fn slot_spec_from_namespace(namespace: &PyClassDict) -> Result<SlotSpec, String> {
    let slots_name = intern::intern("__slots__");
    let Some(raw) = namespace.get(slots_name) else {
        return Ok(SlotSpec {
            declared: false,
            names: Vec::new(),
            wants_dict: true,
        });
    };
    let mut spec = SlotSpec {
        declared: true,
        names: Vec::new(),
        wants_dict: false,
    };
    let mut seen = HashSet::new();
    unsafe {
        if let Some(text) = unicode_text(raw) {
            add_slot_name(&mut spec, &mut seen, text)?;
            return Ok(spec);
        }
        let items = positional_args_from_object(raw).map_err(|_| "__slots__ must be a string or iterable of strings".to_owned())?;
        for item in items {
            let Some(text) = unicode_text(item) else {
                return Err("__slots__ items must be strings".to_owned());
            };
            add_slot_name(&mut spec, &mut seen, text)?;
        }
    }
    Ok(spec)
}

fn add_slot_name(spec: &mut SlotSpec, seen: &mut HashSet<u32>, text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Err("__slots__ entry cannot be empty".to_owned());
    }
    if text == "__dict__" {
        spec.wants_dict = true;
        return Ok(());
    }
    if text == "__weakref__" {
        return Ok(());
    }
    let name = intern::intern(text);
    if !seen.insert(name) {
        return Err(format!("duplicate slot name: '{text}'"));
    }
    spec.names.push(name);
    Ok(())
}

unsafe fn validate_slot_layout(namespace: &PyClassDict, bases: &[*mut PyType], spec: &SlotSpec) -> bool {
    for slot in &spec.names {
        if namespace.get(*slot).is_some() {
            let spelling = intern::resolve(*slot).unwrap_or_else(|| format!("<interned:{slot}>"));
            pon_err_set(format!("'{spelling}' in __slots__ conflicts with class variable"));
            return false;
        }
    }
    if spec.declared && spec.wants_dict && bases.iter().any(|base| unsafe { !base.is_null() && (**base).tp_dictoffset != 0 }) {
        pon_err_set("__dict__ slot disallowed: we already got one");
        return false;
    }
    let slotted_bases = bases
        .iter()
        .copied()
        .filter(|base| unsafe { own_slot_count(*base) != 0 })
        .count();
    if slotted_bases > 1 {
        pon_err_set("multiple bases have instance lay-out conflict");
        return false;
    }
    true
}

fn namespace_allows_dict(bases: &[*mut PyType], spec: &SlotSpec) -> bool {
    !spec.declared || spec.wants_dict || bases.iter().any(|base| unsafe { !base.is_null() && (**base).tp_dictoffset != 0 })
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
    let Some(metaclass) = (unsafe { select_metaclass(&base_types, explicit_meta) }) else {
        return ptr::null_mut();
    };
    let slot_spec = match slot_spec_from_namespace(unsafe { &*namespace }) {
        Ok(spec) => spec,
        Err(message) => return raise_object(message),
    };
    if unsafe { !validate_slot_layout(&*namespace, &base_types, &slot_spec) } {
        return ptr::null_mut();
    }

    let static_name = leak_type_name(name);
    let mut ty = PyType::new(metaclass, static_name, mem::size_of::<PyHeapInstance>());
    ty.tp_base = base_types.first().copied().unwrap_or(ptr::null_mut());
    ty.tp_bases = ptr::null_mut();
    ty.tp_dict = namespace.cast::<PyObject>();
    ty.tp_dictoffset = if namespace_allows_dict(&base_types, &slot_spec) { 1 } else { 0 };
    ty.tp_getattro = Some(descr::generic_get_attr);
    ty.tp_setattro = Some(descr::generic_set_attr);
    ty.tp_new = Some(type_new);
    ty.tp_init = Some(type_init);
    ty.gc_type_id = TYPE_ID_HEAP_INSTANCE.0 as usize;

    let ty = Box::into_raw(Box::new(ty));
    if unsafe { mro::set_c3_mro(ty, &base_types) } < 0 {
        return ptr::null_mut();
    }
    // J0.3 §6 note A: register the new type with every MRO ancestor so a
    // later ancestor mutation transitively invalidates this type's AttrICs
    // (lookup traverses the whole MRO, not just direct bases).
    for ancestor in unsafe { mro::mro_entries(ty) } {
        if !ancestor.is_null() && ancestor != ty {
            crate::sync::register_subclass(ancestor, ty);
        }
    }

    install_slot_descriptors(ty, namespace, &slot_spec);
    for (name, value) in unsafe { (&*namespace).iter().collect::<Vec<_>>() } {
        let _ = update_slot_from_dunder(ty, name, value);
    }
    if unsafe { !call_set_names(ty, namespace) } {
        return ptr::null_mut();
    }
    if unsafe { !call_init_subclass(ty, &base_types, keywords) } {
        return ptr::null_mut();
    }
    ty.cast::<PyObject>()
}

fn member_descriptor_type() -> *mut PyType {
    static MEMBER_DESCRIPTOR_TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(abi::runtime_type_type(), "member_descriptor", mem::size_of::<PyMemberDescriptor>());
        ty.tp_descr_get = Some(member_descriptor_get);
        ty.tp_descr_set = Some(member_descriptor_set);
        Box::into_raw(Box::new(ty)) as usize
    });
    *MEMBER_DESCRIPTOR_TYPE as *mut PyType
}

fn install_slot_descriptors(ty: *mut PyType, namespace: *mut PyClassDict, spec: &SlotSpec) {
    if namespace.is_null() {
        return;
    }
    for slot in &spec.names {
        let descr = Box::into_raw(Box::new(PyMemberDescriptor {
            ob_base: PyObjectHeader::new(member_descriptor_type()),
            owner: ty,
            name: *slot,
            kind: PyMemberKind::Slot,
        }));
        unsafe {
            (&mut *namespace).set(*slot, descr.cast::<PyObject>());
        }
    }
    if spec.wants_dict {
        let name = intern::intern("__dict__");
        let descr = Box::into_raw(Box::new(PyMemberDescriptor {
            ob_base: PyObjectHeader::new(member_descriptor_type()),
            owner: ty,
            name,
            kind: PyMemberKind::Dict,
        }));
        unsafe {
            (&mut *namespace).set(name, descr.cast::<PyObject>());
        }
    }
}

unsafe fn is_member_descriptor(value: *mut PyObject) -> bool {
    !value.is_null() && unsafe { (*value).ob_type == member_descriptor_type().cast_const() }
}

unsafe fn own_slot_count(ty: *mut PyType) -> usize {
    if ty.is_null() {
        return 0;
    }
    let dict = unsafe { (*ty).tp_dict.cast::<PyClassDict>() };
    if dict.is_null() {
        return 0;
    }
    unsafe { (&*dict).iter() }
        .filter(|(_, value)| unsafe {
            is_member_descriptor(*value)
                && (*value.cast::<PyMemberDescriptor>()).owner == ty
                && (*value.cast::<PyMemberDescriptor>()).kind == PyMemberKind::Slot
        })
        .count()
}

unsafe extern "C" fn member_descriptor_get(descr: *mut PyObject, obj: *mut PyObject, _owner: *mut PyObject) -> *mut PyObject {
    if descr.is_null() {
        return raise_object("member descriptor is NULL");
    }
    if obj.is_null() {
        return descr;
    }
    let descr = unsafe { &*descr.cast::<PyMemberDescriptor>() };
    let obj_ty = unsafe { object_type(obj) };
    if obj_ty.is_null() || unsafe { !mro::is_subtype(obj_ty, descr.owner) } {
        return raise_object("descriptor does not apply to this object");
    }
    let instance = obj.cast::<PyHeapInstance>();
    match descr.kind {
        PyMemberKind::Slot => {
            let value = unsafe { instance_get_slot(instance, descr.name) };
            if value.is_null() {
                let spelling = intern::resolve(descr.name).unwrap_or_else(|| format!("<interned:{}>", descr.name));
                return raise_object(format!("'{}' object has no attribute '{spelling}'", unsafe { (*obj_ty).name() }));
            }
            value
        }
        PyMemberKind::Dict => {
            let dict = unsafe { (*instance).dict };
            if dict.is_null() {
                unsafe { abi::pon_none() }
            } else {
                dict.cast::<PyObject>()
            }
        }
    }
}

unsafe extern "C" fn member_descriptor_set(descr: *mut PyObject, obj: *mut PyObject, value: *mut PyObject) -> c_int {
    if descr.is_null() || obj.is_null() {
        pon_err_set("member descriptor assignment received NULL");
        return -1;
    }
    let descr = unsafe { &*descr.cast::<PyMemberDescriptor>() };
    let obj_ty = unsafe { object_type(obj) };
    if obj_ty.is_null() || unsafe { !mro::is_subtype(obj_ty, descr.owner) } {
        pon_err_set("descriptor does not apply to this object");
        return -1;
    }
    let instance = obj.cast::<PyHeapInstance>();
    match descr.kind {
        PyMemberKind::Slot => {
            if unsafe { instance_set_slot(instance, descr.name, value) } {
                0
            } else {
                pon_err_set("slot storage is missing");
                -1
            }
        }
        PyMemberKind::Dict => {
            pon_err_set("__dict__ attribute is read-only");
            -1
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
    match abi::alloc_heap_instance(cls, dict, slots) {
        Ok(object) => object,
        Err(_message) if !abi::runtime_is_initialized() => {
            let object = Box::into_raw(Box::new(PyHeapInstance {
                ob_base: PyObjectHeader::new(cls),
                dict,
                slots: slot_storage(cls),
                weakrefs: ptr::null_mut(),
                finalized: false,
            }));
            as_object_ptr(object)
        }
        Err(message) => raise_object(message),
    }
}

fn slot_storage(cls: *mut PyType) -> Vec<PySlotValue> {
    if cls.is_null() {
        return Vec::new();
    }
    let mut slots = Vec::new();
    let mut mro = unsafe { mro::mro_entries(cls) };
    mro.reverse();
    for ty in mro {
        if ty.is_null() {
            continue;
        }
        let dict = unsafe { (*ty).tp_dict.cast::<PyClassDict>() };
        if dict.is_null() {
            continue;
        }
        for (name, value) in unsafe { (&*dict).iter() } {
            if unsafe { is_member_descriptor(value) } {
                let descr = unsafe { &*value.cast::<PyMemberDescriptor>() };
                if descr.kind == PyMemberKind::Slot && descr.owner == ty && !slots.iter().any(|slot: &PySlotValue| slot.name == name) {
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

unsafe fn call_set_names(ty: *mut PyType, namespace: *mut PyClassDict) -> bool {
    if ty.is_null() || namespace.is_null() {
        return true;
    }
    let set_name_id = intern::intern("__set_name__");
    let entries = unsafe { (&*namespace).iter().collect::<Vec<_>>() };
    for (name_id, value) in entries {
        if value.is_null() {
            continue;
        }
        let value_ty = unsafe { object_type(value) };
        if value_ty.is_null() {
            continue;
        }
        let set_name = unsafe { descr::lookup_in_type(value_ty, set_name_id) };
        if set_name.is_null() {
            continue;
        }
        let spelling = intern::resolve(name_id).unwrap_or_else(|| format!("<interned:{name_id}>"));
        let name_object = unsafe { abi::pon_const_str(spelling.as_ptr(), spelling.len()) };
        if name_object.is_null() {
            return false;
        }
        let owner = ty.cast::<PyObject>();
        let result = if unsafe { object_type_display(set_name) == "function" } {
            let argv = [value, owner, name_object];
            unsafe {
                function::call_bound_function(
                    set_name,
                    &argv,
                    function::KeywordArgs {
                        names: &[],
                        values: &[],
                    },
                    None,
                    None,
                )
                .unwrap_or_else(|message| raise_object(message))
            }
        } else {
            let bound = unsafe { descr::descriptor_get(set_name, value, value_ty) };
            if bound.is_null() {
                ptr::null_mut()
            } else {
                let mut argv = [owner, name_object];
                unsafe { abi::pon_call(bound, argv.as_mut_ptr(), argv.len()) }
            }
        };
        if result.is_null() {
            pon_err_set(format!(
                "Error calling __set_name__ on '{}' instance '{}' in '{}'",
                unsafe { (*value_ty).name() },
                spelling,
                unsafe { (*ty).name() }
            ));
            return false;
        }
    }
    true
}

unsafe fn call_init_subclass(ty: *mut PyType, base_types: &[*mut PyType], keywords: &[ClassKeyword]) -> bool {
    let init_keywords = keywords
        .iter()
        .copied()
        .filter(|keyword| intern::resolve(keyword.name).as_deref() != Some("metaclass"))
        .collect::<Vec<_>>();
    if base_types.is_empty() {
        if init_keywords.is_empty() {
            return true;
        }
        pon_err_set("object.__init_subclass__() takes no keyword arguments");
        return false;
    }

    let init_id = intern::intern("__init_subclass__");
    let mut init = ptr::null_mut();
    for base in unsafe { mro::mro_entries(ty) }.into_iter().skip(1) {
        if base.is_null() {
            continue;
        }
        let dict = unsafe { (*base).tp_dict.cast::<PyClassDict>() };
        if dict.is_null() {
            continue;
        }
        if let Some(value) = unsafe { (&*dict).get(init_id) } {
            init = value;
            break;
        }
    }
    if init.is_null() {
        if init_keywords.is_empty() {
            return true;
        }
        pon_err_set("object.__init_subclass__() takes no keyword arguments");
        return false;
    }

    let kw_names = init_keywords.iter().map(|keyword| keyword.name).collect::<Vec<_>>();
    let mut kw_values = init_keywords.iter().map(|keyword| keyword.value).collect::<Vec<_>>();
    let keywords = function::KeywordArgs {
        names: kw_names.as_slice(),
        values: kw_values.as_slice(),
    };
    let cls_object = ty.cast::<PyObject>();
    let result = if unsafe { object_type_display(init) == "function" } {
        let argv = [cls_object];
        unsafe { function::call_bound_function(init, &argv, keywords, None, None).unwrap_or_else(|message| raise_object(message)) }
    } else {
        let init_ty = unsafe { object_type(init) };
        let bound = unsafe { descr::descriptor_get(init, cls_object, ty) };
        if bound.is_null() || init_ty.is_null() {
            ptr::null_mut()
        } else if kw_names.is_empty() {
            unsafe { abi::pon_call(bound, ptr::null_mut(), 0) }
        } else {
            unsafe {
                abi::call::pon_call_ex(
                    bound,
                    ptr::null_mut(),
                    0,
                    ptr::null_mut(),
                    kw_names.as_ptr(),
                    kw_values.as_mut_ptr(),
                    kw_names.len(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            }
        }
    };
    !result.is_null()
}

pub unsafe fn builtin_type(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { raw_arg_slice(argv, argc) } {
        Ok(args) => args,
        Err(message) => return raise_object(message),
    };
    match args.len() {
        1 => {
            let object = args[0];
            if object.is_null() {
                return raise_object("type() argument is NULL");
            }
            let ty = unsafe { object_type(object) };
            if ty.is_null() {
                return raise_object("type() argument has no type");
            }
            ty.cast::<PyObject>()
        }
        3 => unsafe { build_class_from_type_args(args[0], args[1], args[2]) },
        n => raise_object(format!("type() takes 1 or 3 arguments, got {n}")),
    }
}

unsafe fn raw_arg_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Result<&'a [*mut PyObject], String> {
    if argv.is_null() && argc != 0 {
        return Err("argv pointer is null".to_owned());
    }
    Ok(if argc == 0 {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(argv.cast_const(), argc) }
    })
}

unsafe fn build_class_from_type_args(name: *mut PyObject, bases: *mut PyObject, namespace: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { unicode_text(name) }) else {
        return raise_object("type.__new__() argument 1 must be str");
    };
    let bases = match unsafe { positional_args_from_object(bases) } {
        Ok(bases) => bases,
        Err(_) => return raise_object("type.__new__() argument 2 must be tuple"),
    };
    let namespace = match unsafe { namespace_from_mapping(namespace) } {
        Ok(namespace) => namespace,
        Err(message) => return raise_object(message),
    };
    unsafe { build_class_from_namespace(name, &bases, namespace, &[]) }
}

unsafe fn namespace_from_mapping(namespace: *mut PyObject) -> Result<*mut PyClassDict, String> {
    if namespace.is_null() {
        return Err("type.__new__() argument 3 must be dict".to_owned());
    }
    let ty = unsafe { object_type(namespace) };
    if ty.is_null() || unsafe { (*ty).name() } != "dict" {
        return Err("type.__new__() argument 3 must be dict".to_owned());
    }
    let entries = unsafe { dict::dict_entries_snapshot(namespace) }.map_err(|_| "type.__new__() argument 3 must be dict".to_owned())?;
    let out = new_namespace();
    for entry in entries {
        let Some(name) = (unsafe { unicode_text(entry.key) }) else {
            return Err("type.__new__() argument 3 keys must be str".to_owned());
        };
        unsafe { (&mut *out).set(intern::intern(name), entry.value) };
    }
    Ok(out)
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

    #[test]
    fn most_derived_metaclass_wins_across_bases() {
        let mut type_type = PyType::new(ptr::null(), "type", mem::size_of::<PyType>());
        let type_ptr = &mut type_type as *mut PyType;
        type_type.ob_base.ob_type = type_ptr;

        let mut meta_base = PyType::new(type_ptr, "MetaBase", mem::size_of::<PyType>());
        meta_base.tp_base = type_ptr;
        let meta_base_ptr = &mut meta_base as *mut PyType;
        let mut meta_derived = PyType::new(type_ptr, "MetaDerived", mem::size_of::<PyType>());
        meta_derived.tp_base = meta_base_ptr;
        let meta_derived_ptr = &mut meta_derived as *mut PyType;

        let mut base_a = PyType::new(meta_base_ptr, "BaseA", mem::size_of::<PyHeapInstance>());
        let base_a_ptr = &mut base_a as *mut PyType;
        let mut base_b = PyType::new(meta_derived_ptr, "BaseB", mem::size_of::<PyHeapInstance>());
        let base_b_ptr = &mut base_b as *mut PyType;

        unsafe {
            assert_eq!(mro::set_c3_mro(meta_base_ptr, &[type_ptr]), 0);
            assert_eq!(mro::set_c3_mro(meta_derived_ptr, &[meta_base_ptr]), 0);
            assert_eq!(mro::set_c3_mro(base_a_ptr, &[]), 0);
            assert_eq!(mro::set_c3_mro(base_b_ptr, &[]), 0);
            let ns = new_namespace();
            let cls = build_class_from_namespace(
                "D",
                &[base_a_ptr.cast::<PyObject>(), base_b_ptr.cast::<PyObject>()],
                ns,
                &[],
            )
            .cast::<PyType>();
            assert!(!cls.is_null());
            assert_eq!((*cls).ob_base.ob_type, meta_derived_ptr.cast_const());
        }
    }

    #[test]
    fn unrelated_base_metaclasses_conflict() {
        let mut type_type = PyType::new(ptr::null(), "type", mem::size_of::<PyType>());
        let type_ptr = &mut type_type as *mut PyType;
        type_type.ob_base.ob_type = type_ptr;

        let mut meta_a = PyType::new(type_ptr, "MetaA", mem::size_of::<PyType>());
        meta_a.tp_base = type_ptr;
        let meta_a_ptr = &mut meta_a as *mut PyType;
        let mut meta_b = PyType::new(type_ptr, "MetaB", mem::size_of::<PyType>());
        meta_b.tp_base = type_ptr;
        let meta_b_ptr = &mut meta_b as *mut PyType;
        let mut base_a = PyType::new(meta_a_ptr, "BaseA", mem::size_of::<PyHeapInstance>());
        let base_a_ptr = &mut base_a as *mut PyType;
        let mut base_b = PyType::new(meta_b_ptr, "BaseB", mem::size_of::<PyHeapInstance>());
        let base_b_ptr = &mut base_b as *mut PyType;

        unsafe {
            assert_eq!(mro::set_c3_mro(meta_a_ptr, &[type_ptr]), 0);
            assert_eq!(mro::set_c3_mro(meta_b_ptr, &[type_ptr]), 0);
            assert_eq!(mro::set_c3_mro(base_a_ptr, &[]), 0);
            assert_eq!(mro::set_c3_mro(base_b_ptr, &[]), 0);
            let ns = new_namespace();
            let cls = build_class_from_namespace(
                "Bad",
                &[base_a_ptr.cast::<PyObject>(), base_b_ptr.cast::<PyObject>()],
                ns,
                &[],
            );
            assert!(cls.is_null());
        }
    }

    #[test]
    fn multiple_slotted_bases_report_layout_conflict() {
        unsafe {
            let base_a_ns = new_namespace();
            (&mut *base_a_ns).set(intern::intern("__slots__"), fake_str("a"));
            let base_a = build_class_from_namespace("SlotA", &[], base_a_ns, &[]).cast::<PyType>();
            assert!(!base_a.is_null());

            let base_b_ns = new_namespace();
            (&mut *base_b_ns).set(intern::intern("__slots__"), fake_str("b"));
            let base_b = build_class_from_namespace("SlotB", &[], base_b_ns, &[]).cast::<PyType>();
            assert!(!base_b.is_null());

            let derived_ns = new_namespace();
            let derived = build_class_from_namespace(
                "Derived",
                &[base_a.cast::<PyObject>(), base_b.cast::<PyObject>()],
                derived_ns,
                &[],
            );
            assert!(derived.is_null());
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
