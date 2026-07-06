//! Type object, heap-instance, class namespace, and class-building support.

use core::{ffi::c_int, mem, ptr};
use std::{
	collections::{HashMap, HashSet},
	sync::LazyLock,
};

use pon_gc::{GcTypeInfo, TypeId};

use crate::{
	abi, descr, intern, mro,
	object::{
		NewFunc, PyMappingMethods, PyObject, PyObjectHeader, PySequenceMethods, PyType, PyUnicode,
		as_object_ptr, update_slot_from_dunder,
	},
	thread_state::pon_err_set,
	types::{dict, function, list::PyList, tuple::PyTuple},
};

/// Lightweight dictionary used for type dictionaries and instance dictionaries
/// until the mapping workstream's concrete `dict` object is wired in.
#[repr(C)]
#[derive(Debug)]
pub struct PyClassDict {
	/// Common object header.  The carrier is internal and may have a NULL type.
	pub ob_base: PyObjectHeader,
	entries:     HashMap<u32, *mut PyObject>,
	/// Insertion order of live keys (Python 3.7+ dict ordering guarantee;
	/// class namespaces drive `__set_name__`/enum member order).
	order:       Vec<u32>,
}

impl PyClassDict {
	/// Create an empty namespace dictionary.
	#[must_use]
	pub fn new() -> Self {
		Self {
			ob_base: PyObjectHeader::new(ptr::null()),
			entries: HashMap::new(),
			order:   Vec::new(),
		}
	}

	/// Return a stored value by interned name.
	#[must_use]
	pub fn get(&self, name: u32) -> Option<*mut PyObject> {
		self.entries.get(&name).copied()
	}

	/// Store or replace one interned-name value.  Overwrites keep the
	/// original insertion position (CPython dict semantics).
	pub fn set(&mut self, name: u32, value: *mut PyObject) {
		if self.entries.insert(name, value).is_none() {
			self.order.push(name);
		}
	}

	/// Delete one interned-name value.
	pub fn del(&mut self, name: u32) -> bool {
		if self.entries.remove(&name).is_some() {
			self.order.retain(|&existing| existing != name);
			true
		} else {
			false
		}
	}

	/// Iterate over namespace entries in insertion order.
	pub fn iter(&self) -> impl Iterator<Item = (u32, *mut PyObject)> + '_ {
		self
			.order
			.iter()
			.filter_map(|name| self.entries.get(name).map(|value| (*name, *value)))
	}
}

impl Default for PyClassDict {
	fn default() -> Self {
		Self::new()
	}
}

/// GC type id for C-facing live views over class namespaces.
pub const TYPE_ID_CLASS_DICT_VIEW: TypeId = TypeId(14);

/// Dict-shaped object handed to C as `PyTypeObject.tp_dict`.
#[repr(C)]
pub(crate) struct PyClassDictView {
	ob_base: PyObjectHeader,
	/// Native type whose `tp_dict` is reflected by this view.
	owner:   *mut PyType,
}

static CLASS_DICT_VIEW_MAPPING: PyMappingMethods = PyMappingMethods {
	mp_length:        Some(class_dict_view_len_slot),
	mp_subscript:     Some(class_dict_view_subscript_slot),
	mp_ass_subscript: Some(class_dict_view_ass_subscript_slot),
};

static CLASS_DICT_VIEW_SEQUENCE: PySequenceMethods = PySequenceMethods {
	sq_length: Some(class_dict_view_len_slot),
	sq_contains: Some(class_dict_view_contains_slot),
	..PySequenceMethods::EMPTY
};

static CLASS_DICT_VIEW_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"type_dict",
		mem::size_of::<PyClassDictView>(),
	);
	ty.gc_type_id = TYPE_ID_CLASS_DICT_VIEW.0 as usize;
	ty.tp_as_mapping = ptr::addr_of!(CLASS_DICT_VIEW_MAPPING).cast_mut();
	ty.tp_as_sequence = ptr::addr_of!(CLASS_DICT_VIEW_SEQUENCE).cast_mut();
	ty.tp_repr = Some(class_dict_view_repr_slot);
	Box::into_raw(Box::new(ty)) as usize
});

unsafe extern "C" fn trace_class_dict_view(_object: *mut u8, _visitor: &mut dyn FnMut(*mut u8)) {}

/// Allocate a live view over `owner.tp_dict` for exposure through C `tp_dict`.
///
/// The backing namespace remains the native `PyClassDict`; this PyObject only
/// supplies a real header and dict-like protocol surface for C API calls.
#[must_use]
pub(crate) unsafe fn new_class_dict_view(owner: *mut PyType) -> *mut PyObject {
	if owner.is_null() {
		return raise_object("class dict view owner is NULL");
	}
	if unsafe { (*owner).tp_dict }.is_null() {
		return raise_object("class dict view owner has no namespace");
	}
	let info = GcTypeInfo {
		size:     mem::size_of::<PyClassDictView>(),
		trace:    trace_class_dict_view,
		finalize: None,
	};
	let object = match abi::alloc_gc_object(TYPE_ID_CLASS_DICT_VIEW, info) {
		Ok(object) => object.cast::<PyClassDictView>(),
		Err(message) => return raise_object(message),
	};
	unsafe {
		ptr::write(object, PyClassDictView {
			ob_base: PyObjectHeader::new(*CLASS_DICT_VIEW_TYPE as *mut PyType),
			owner,
		});
	}
	object.cast::<PyObject>()
}

unsafe fn as_class_dict_view<'a>(object: *mut PyObject) -> Option<&'a PyClassDictView> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || !crate::tag::is_heap(object) {
		return None;
	}
	let Some(&view_type) = LazyLock::get(&CLASS_DICT_VIEW_TYPE) else {
		return None;
	};
	if unsafe { (*object).ob_type } != (view_type as *mut PyType).cast_const() {
		return None;
	}
	Some(unsafe { &*object.cast::<PyClassDictView>() })
}

#[must_use]
pub(crate) unsafe fn is_class_dict_view(object: *mut PyObject) -> bool {
	unsafe { as_class_dict_view(object) }.is_some()
}

unsafe fn class_dict_view_parts(
	object: *mut PyObject,
) -> Result<(*mut PyType, *mut PyClassDict), String> {
	let Some(view) = (unsafe { as_class_dict_view(object) }) else {
		return Err("expected C type tp_dict view".to_owned());
	};
	if view.owner.is_null() {
		return Err("C type tp_dict view owner is NULL".to_owned());
	}
	let dict = unsafe { (*view.owner).tp_dict }.cast::<PyClassDict>();
	if dict.is_null() {
		return Err("C type tp_dict backing namespace is NULL".to_owned());
	}
	Ok((view.owner, dict))
}

unsafe fn class_dict_key_id(key: *mut PyObject) -> Result<u32, String> {
	let key = crate::tag::untag_arg(key);
	let Some(text) = (unsafe { unicode_text(key) }) else {
		return Err("C type tp_dict keys must be strings".to_owned());
	};
	Ok(intern::intern(text))
}

fn class_dict_key_object(name: u32) -> Result<*mut PyObject, String> {
	let spelling =
		intern::resolve(name).ok_or_else(|| format!("unresolved interned class dict key {name}"))?;
	let key = unsafe { abi::pon_const_str(spelling.as_ptr(), spelling.len()) };
	if key.is_null() {
		Err("failed to allocate class dict key object".to_owned())
	} else {
		Ok(key)
	}
}

unsafe fn class_dict_view_set_id(
	owner: *mut PyType,
	dict: *mut PyClassDict,
	name: u32,
	value: *mut PyObject,
) {
	unsafe { (&mut *dict).set(name, value) };
	let _ = update_slot_from_dunder(owner, name, value);
	crate::sync::type_modified(owner);
}

unsafe fn class_dict_view_del_id(owner: *mut PyType, dict: *mut PyClassDict, name: u32) -> bool {
	let removed = unsafe { (&mut *dict).del(name) };
	if removed {
		let _ = update_slot_from_dunder(owner, name, ptr::null_mut());
		crate::sync::type_modified(owner);
	}
	removed
}

pub(crate) unsafe fn class_dict_view_get_item(
	object: *mut PyObject,
	key: *mut PyObject,
) -> Result<Option<*mut PyObject>, String> {
	let (_, dict) = unsafe { class_dict_view_parts(object) }?;
	let name = unsafe { class_dict_key_id(key) }?;
	Ok(unsafe { (&*dict).get(name) })
}

pub(crate) unsafe fn class_dict_view_set_item(
	object: *mut PyObject,
	key: *mut PyObject,
	value: *mut PyObject,
) -> Result<(), String> {
	if value.is_null() {
		return Err("C type tp_dict value must not be NULL".to_owned());
	}
	let (owner, dict) = unsafe { class_dict_view_parts(object) }?;
	let name = unsafe { class_dict_key_id(key) }?;
	unsafe { class_dict_view_set_id(owner, dict, name, value) };
	Ok(())
}

pub(crate) unsafe fn class_dict_view_del_item(
	object: *mut PyObject,
	key: *mut PyObject,
) -> Result<bool, String> {
	let (owner, dict) = unsafe { class_dict_view_parts(object) }?;
	let name = unsafe { class_dict_key_id(key) }?;
	Ok(unsafe { class_dict_view_del_id(owner, dict, name) })
}

pub(crate) unsafe fn class_dict_view_contains(
	object: *mut PyObject,
	key: *mut PyObject,
) -> Result<bool, String> {
	let (_, dict) = unsafe { class_dict_view_parts(object) }?;
	let key = crate::tag::untag_arg(key);
	let Some(text) = (unsafe { unicode_text(key) }) else {
		return Ok(false);
	};
	Ok(unsafe { (&*dict).get(intern::intern(text)).is_some() })
}

pub(crate) unsafe fn class_dict_view_len(object: *mut PyObject) -> Result<usize, String> {
	let (_, dict) = unsafe { class_dict_view_parts(object) }?;
	Ok(unsafe { (&*dict).iter().count() })
}

pub(crate) unsafe fn class_dict_view_entries_snapshot(
	object: *mut PyObject,
) -> Result<Vec<dict::DictEntry>, String> {
	let (_, dict) = unsafe { class_dict_view_parts(object) }?;
	let pairs = unsafe { (&*dict).iter().collect::<Vec<_>>() };
	let mut entries = Vec::with_capacity(pairs.len());
	for (name, value) in pairs {
		entries.push(dict::DictEntry { key: class_dict_key_object(name)?, value, hash: 0 });
	}
	Ok(entries)
}

pub(crate) unsafe fn class_dict_view_clear(object: *mut PyObject) -> Result<(), String> {
	let (owner, dict) = unsafe { class_dict_view_parts(object) }?;
	let keys = unsafe { (&*dict).iter().map(|(name, _)| name).collect::<Vec<_>>() };
	for name in keys {
		unsafe { class_dict_view_del_id(owner, dict, name) };
	}
	Ok(())
}

pub(crate) unsafe fn merge_tp_dict_into_class(owner: *mut PyType, source: *mut PyObject) -> bool {
	if owner.is_null() || source.is_null() {
		return true;
	}
	let source = crate::tag::untag_arg(source);
	let entries = if unsafe { is_class_dict_view(source) } {
		match unsafe { class_dict_view_entries_snapshot(source) } {
			Ok(entries) => entries,
			Err(message) => {
				pon_err_set(message);
				return false;
			},
		}
	} else if unsafe { dict::has_dict_storage(source) } {
		match unsafe { dict::dict_entries_snapshot(source) } {
			Ok(entries) => entries,
			Err(message) => {
				pon_err_set(message);
				return false;
			},
		}
	} else {
		pon_err_set("PyType_Ready: tp_dict must be a dict");
		return false;
	};
	let class_dict = unsafe { (*owner).tp_dict }.cast::<PyClassDict>();
	if class_dict.is_null() {
		pon_err_set("PyType_Ready: native class namespace is NULL");
		return false;
	}
	for entry in entries {
		let name = match unsafe { class_dict_key_id(entry.key) } {
			Ok(name) => name,
			Err(message) => {
				pon_err_set(format!("PyType_Ready: {message}"));
				return false;
			},
		};
		unsafe { class_dict_view_set_id(owner, class_dict, name, entry.value) };
	}
	true
}

unsafe extern "C" fn class_dict_view_len_slot(object: *mut PyObject) -> isize {
	match unsafe { class_dict_view_len(object) } {
		Ok(len) => isize::try_from(len).unwrap_or_else(|_| {
			pon_err_set("C type tp_dict is too large");
			-1
		}),
		Err(message) => {
			pon_err_set(message);
			-1
		},
	}
}

unsafe extern "C" fn class_dict_view_subscript_slot(
	object: *mut PyObject,
	key: *mut PyObject,
) -> *mut PyObject {
	match unsafe { class_dict_view_get_item(object, key) } {
		Ok(Some(value)) => value,
		Ok(None) => unsafe { abi::exc::pon_raise_key_error(key) },
		Err(message) => raise_object(message),
	}
}

unsafe extern "C" fn class_dict_view_ass_subscript_slot(
	object: *mut PyObject,
	key: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	if value.is_null() {
		match unsafe { class_dict_view_del_item(object, key) } {
			Ok(true) => 0,
			Ok(false) => {
				unsafe { abi::exc::pon_raise_key_error(key) };
				-1
			},
			Err(message) => {
				pon_err_set(message);
				-1
			},
		}
	} else {
		match unsafe { class_dict_view_set_item(object, key, value) } {
			Ok(()) => 0,
			Err(message) => {
				pon_err_set(message);
				-1
			},
		}
	}
}

unsafe extern "C" fn class_dict_view_contains_slot(
	object: *mut PyObject,
	key: *mut PyObject,
) -> c_int {
	match unsafe { class_dict_view_contains(object, key) } {
		Ok(present) => c_int::from(present),
		Err(message) => {
			pon_err_set(message);
			-1
		},
	}
}

unsafe extern "C" fn class_dict_view_repr_slot(object: *mut PyObject) -> *mut PyObject {
	let entries = match unsafe { class_dict_view_entries_snapshot(object) } {
		Ok(entries) => entries,
		Err(message) => return raise_object(message),
	};
	let mut out = String::from("{");
	for (index, entry) in entries.into_iter().enumerate() {
		if index > 0 {
			out.push_str(", ");
		}
		let key_text = unsafe { unicode_text(entry.key) }.unwrap_or("<non-string>");
		out.push_str(&format!("'{}': ", key_text));
		out.push_str(&crate::native::builtins_mod::repr_text(entry.value));
	}
	out.push('}');
	unsafe { abi::pon_const_str(out.as_ptr(), out.len()) }
}

/// One heap-instance slot value configured by `__slots__`.
#[derive(Clone, Copy, Debug)]
pub struct PySlotValue {
	/// Interned slot name.
	pub name:  u32,
	/// Current boxed value, or NULL when unassigned.
	pub value: *mut PyObject,
}

/// Generic heap instance used by Python classes.
#[repr(C)]
#[derive(Debug)]
pub struct PyHeapInstance {
	/// Common object header; must remain first.
	pub ob_base:   PyObjectHeader,
	/// Per-instance dictionary, or NULL for slot-only instances.
	pub dict:      *mut PyClassDict,
	/// Slot storage in class-defined order.
	pub slots:     Vec<PySlotValue>,
	/// Head of the runtime weakref list (owned by J4c weakref/finalization).
	pub weakrefs:  *mut PyObject,
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
	pub owner:   *mut PyType,
	/// Interned slot name.
	pub name:    u32,
	/// Descriptor flavor.
	pub kind:    PyMemberKind,
}

/// Class keyword argument pair consumed by the runtime class builder.
#[derive(Clone, Copy, Debug)]
pub struct ClassKeyword {
	/// Interned keyword name.
	pub name:  u32,
	/// Evaluated keyword value.
	pub value: *mut PyObject,
}

/// GC type id for Python heap instances allocated by [`type_new`].
pub const TYPE_ID_HEAP_INSTANCE: TypeId = TypeId(10);

/// Returns true when instances of `ty` belong to the Python class-instance
/// family whose MRO may carry Python-level dunder hooks: plain heap instances
/// (including dict/payload extended layouts, which share the id) plus
/// BaseException-derived heap classes (boxed-exception layout, stamped by
/// `construct_class`).
#[must_use]
pub fn type_dispatches_python_dunders(ty: *const PyType) -> bool {
	if ty.is_null() {
		return false;
	}
	let id = unsafe { (*ty).gc_type_id };
	id == TYPE_ID_HEAP_INSTANCE.0 as usize
		|| id == crate::abi::TYPE_ID_EXCEPTION.0 as usize
		|| id == crate::abi::TYPE_ID_EXCEPTION_GROUP.0 as usize
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

/// GC type id for heap instances embedding a builtin data-type payload
/// (`str`/`int`/`bytes` subclasses); class-instance family next to
/// [`TYPE_ID_HEAP_INSTANCE`].
pub const TYPE_ID_PAYLOAD_SUBCLASS_INSTANCE: TypeId = TypeId(108);

/// Heap-class instance carrying a canonical builtin payload (`str`/`int`/
/// `bytes` subclasses). Mirrors `PyDictSubclassInstance`: the generic
/// heap-instance prefix keeps every instance-attribute, slot, and weakref path
/// working unchanged, while `value` holds the canonical builtin object the
/// native protocol reads through.
#[repr(C)]
#[derive(Debug)]
pub struct PyPayloadSubclassInstance {
	/// Generic heap-instance prefix; must remain first.
	pub base:  PyHeapInstance,
	/// Canonical builtin payload: heap `str`, exact `bytes`, or a tagged/heap
	/// `int`.
	pub value: *mut PyObject,
}

/// Names of builtin data types whose Python subclasses embed a canonical
/// payload ([`PyPayloadSubclassInstance`] layout).
const PAYLOAD_BASE_NAMES: [&str; 3] = ["str", "int", "bytes"];

/// Returns whether `ty` is a heap class using the payload-subclass layout.
#[must_use]
pub unsafe fn type_is_payload_subclass(ty: *mut PyType) -> bool {
	if ty.is_null() {
		return false;
	}
	unsafe {
		(*ty).gc_type_id == TYPE_ID_HEAP_INSTANCE.0 as usize
			&& (*ty).tp_basicsize == mem::size_of::<PyPayloadSubclassInstance>()
	}
}

/// Returns whether `object` is a payload-subclass heap instance.
#[must_use]
pub unsafe fn is_payload_subclass_instance(object: *mut PyObject) -> bool {
	!object.is_null()
		&& crate::tag::is_heap(object)
		&& unsafe { type_is_payload_subclass((*object).ob_type.cast_mut()) }
}

/// Embedded canonical payload of a `str`/`int`/`bytes`-subclass instance, when
/// set.
#[must_use]
pub unsafe fn payload_subclass_value(object: *mut PyObject) -> Option<*mut PyObject> {
	if unsafe { !is_payload_subclass_instance(object) } {
		return None;
	}
	let value = unsafe { (*object.cast::<PyPayloadSubclassInstance>()).value };
	if value.is_null() { None } else { Some(value) }
}

/// Returns whether a class built over `bases` embeds a builtin data-type
/// payload: some base linearizes over `str`, `int`, or `bytes`.  Mirrors
/// `dict::class_bases_embed_dict` (non-heap name match).
#[must_use]
pub unsafe fn class_bases_embed_payload(bases: &[*mut PyType]) -> bool {
	bases.iter().copied().any(|base| {
		unsafe { crate::mro::mro_entries(base) }
			.iter()
			.any(|entry| {
				!entry.is_null()
					&& unsafe {
						(**entry).gc_type_id != TYPE_ID_HEAP_INSTANCE.0 as usize
							&& PAYLOAD_BASE_NAMES.contains(&(**entry).name())
					}
			})
	})
}

/// Traces GC references of a payload-subclass instance: the heap-instance
/// prefix plus the embedded payload (heap payloads only; tagged ints and
/// non-GC boxed bytes carry no allocation in the managed heap).
pub unsafe extern "C" fn trace_payload_subclass_instance(
	object: *mut u8,
	visitor: &mut dyn FnMut(*mut u8),
) {
	if object.is_null() {
		return;
	}
	unsafe { crate::types::weakref::trace_heap_instance(object, visitor) };
	let value = unsafe { (*object.cast::<PyPayloadSubclassInstance>()).value };
	if !value.is_null() && crate::tag::is_heap(value) {
		visitor(value.cast::<u8>());
	}
}

/// Finalizes a payload-subclass instance: heap-instance semantics, plus
/// detaching an embedded canonical weakref payload so referent death can
/// never call back through the freed wrapper.
pub unsafe extern "C" fn finalize_payload_subclass_instance(object: *mut u8) {
	if object.is_null() {
		return;
	}
	unsafe { crate::types::weakref::detach_wrapper_payload(object.cast::<PyObject>()) };
	unsafe { crate::types::weakref::finalize_heap_instance(object) };
}

/// Allocate a payload-subclass instance of `cls` carrying `value`, with the
/// instance dict/slot storage `cls` prescribes.
pub(crate) unsafe fn alloc_payload_instance_for_class(
	cls: *mut PyType,
	value: *mut PyObject,
) -> Result<*mut PyObject, String> {
	let dict = if unsafe { (*cls).tp_dictoffset != 0 } {
		new_namespace()
	} else {
		ptr::null_mut()
	};
	unsafe { abi::alloc_payload_subclass_instance(cls, dict, slot_storage(cls), value) }
}

/// Allocate a tuple-subclass instance of `cls` carrying `values`, with the
/// instance dict/slot storage `cls` prescribes (`tuple.__new__` carrier path).
pub(crate) unsafe fn alloc_tuple_instance_for_class(
	cls: *mut PyType,
	values: &[*mut PyObject],
) -> Result<*mut PyObject, String> {
	let dict = if unsafe { (*cls).tp_dictoffset != 0 } {
		new_namespace()
	} else {
		ptr::null_mut()
	};
	crate::abi::seq::alloc_tuple_subclass_instance(cls, dict, slot_storage(cls), values)
}

/// Best-effort extraction of UTF-8 text from a runtime string object.
/// Str-subclass instances read through their canonical payload.
#[must_use]
pub unsafe fn unicode_text<'a>(object: *mut PyObject) -> Option<&'a str> {
	if object.is_null() || !crate::tag::is_heap(object) {
		return None;
	}
	// Foreign-safe type identification: C-extension instances (numpy dtype
	// objects, ...) carry raw `PyTypeObject` headers that must never be read
	// through the pon `PyType` layout.
	if unsafe { crate::types::dict::type_name(object) }? == "str" {
		return unsafe { (*object.cast::<PyUnicode>()).as_str() };
	}
	let value = unsafe { payload_subclass_value(object) }?;
	unsafe { unicode_text(value) }
}

/// Type of `object`, or NULL when `object` is NULL or a tagged immediate
/// (immediates carry no dereferenceable type; callers route NULL through
/// their existing error/fallthrough paths).
unsafe fn object_type(object: *mut PyObject) -> *mut PyType {
	if object.is_null() || !crate::tag::is_heap(object) {
		return ptr::null_mut();
	}
	let ty = unsafe { (*object).ob_type.cast_mut() };
	crate::capi::twin::registered_native_of_foreign(ty.cast()).unwrap_or(ty)
}

/// Copy positional arguments out of the tuple/list carrier used by
/// CPython-style call slots. A NULL carrier represents zero explicit positional
/// arguments.
pub unsafe fn positional_args_from_object(
	args: *mut PyObject,
) -> Result<Vec<*mut PyObject>, String> {
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

pub(crate) unsafe fn is_type_object(object: *mut PyObject) -> bool {
	if object.is_null() {
		return false;
	}
	let meta = unsafe { object_type(object) };
	if meta.is_null() {
		return false;
	}
	unsafe {
		(*meta).name() == "type"
			|| mro::mro_entries(meta)
				.iter()
				.any(|ty| !ty.is_null() && (**ty).name() == "type")
	}
}

unsafe fn object_type_display(object: *mut PyObject) -> String {
	let ty = unsafe { object_type(object) };
	if ty.is_null() {
		"NULL".to_owned()
	} else {
		unsafe { (*ty).name() }.to_owned()
	}
}

/// Resolves PEP 560 `__mro_entries__` bases.  Returns the resolved base
/// types plus the ORIGINAL bases tuple (a GC allocation) when any base
/// needed resolution, NULL otherwise — callers publishing
/// `__orig_bases__` (CPython `__build_class__`) consume the tuple; other
/// callers discard it.
unsafe fn normalize_bases(bases: &[*mut PyObject]) -> Option<(Vec<*mut PyType>, *mut PyObject)> {
	let needs_mro_entries = bases
		.iter()
		.copied()
		.any(|base| unsafe { !is_type_object(base) });
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
			},
		};
		for replacement in replacements {
			if replacement.is_null() || unsafe { !is_type_object(replacement) } {
				pon_err_set("__mro_entries__ must return a tuple of classes");
				return None;
			}
			out.push(replacement.cast::<PyType>());
		}
	}
	Some((out, original_tuple))
}

unsafe fn resolve_mro_entries(
	base: *mut PyObject,
	original_bases: *mut PyObject,
) -> Result<Vec<*mut PyObject>, String> {
	let method =
		unsafe { abi::pon_get_attr(base, intern::intern("__mro_entries__"), ptr::null_mut()) };
	if method.is_null() {
		crate::thread_state::pon_err_clear();
		return Err(format!("{} is not an acceptable base type", unsafe {
			object_type_display(base)
		}));
	}
	let mut argv = [original_bases];
	let replacement = unsafe { abi::pon_call(method, argv.as_mut_ptr(), 1) };
	if replacement.is_null() {
		return Err("__mro_entries__ failed".to_owned());
	}
	unsafe { positional_args_from_object(replacement) }
		.map_err(|_| "__mro_entries__ must return a tuple".to_owned())
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
		let message = "metaclass must be a type";
		unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
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
		let message = "metaclass conflict: the metaclass of a derived class must be a (non-strict) \
		               subclass of the metaclasses of all its bases";
		unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
		return None;
	}
	Some(winner)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TypingSpecialBase {
	NamedTuple,
	TypedDict,
}

unsafe fn typing_special_base(bases: &[*mut PyObject]) -> Option<TypingSpecialBase> {
	if bases.len() != 1 {
		return None;
	}
	let base = bases[0];
	if base.is_null() || unsafe { object_type_display(base) != "function" } {
		return None;
	}
	let module = unsafe { abi::pon_get_attr(base, intern::intern("__module__"), ptr::null_mut()) };
	let name = unsafe { abi::pon_get_attr(base, intern::intern("__name__"), ptr::null_mut()) };
	if module.is_null() || name.is_null() {
		crate::thread_state::pon_err_clear();
		return None;
	}
	let Some(module_text) = (unsafe { unicode_text(module) }) else {
		return None;
	};
	let Some(name_text) = (unsafe { unicode_text(name) }) else {
		return None;
	};
	if module_text != "typing" {
		return None;
	}
	match name_text {
		"NamedTuple" => Some(TypingSpecialBase::NamedTuple),
		"TypedDict" => Some(TypingSpecialBase::TypedDict),
		_ => None,
	}
}

#[derive(Clone, Debug, Default)]
struct SlotSpec {
	declared:   bool,
	names:      Vec<u32>,
	wants_dict: bool,
}

fn slot_spec_from_namespace(namespace: &PyClassDict) -> Result<SlotSpec, String> {
	let slots_name = intern::intern("__slots__");
	let Some(raw) = namespace.get(slots_name) else {
		return Ok(SlotSpec { declared: false, names: Vec::new(), wants_dict: true });
	};
	let mut spec = SlotSpec { declared: true, names: Vec::new(), wants_dict: false };
	let mut seen = HashSet::new();
	unsafe {
		if let Some(text) = unicode_text(raw) {
			add_slot_name(&mut spec, &mut seen, text)?;
			return Ok(spec);
		}
		if dict::has_dict_storage(raw) {
			let entries = dict::dict_entries_snapshot(raw)
				.map_err(|_| "__slots__ must be a string or iterable of strings".to_owned())?;
			for entry in entries {
				let Some(text) = unicode_text(entry.key) else {
					return Err("__slots__ items must be strings".to_owned());
				};
				add_slot_name(&mut spec, &mut seen, text)?;
			}
			return Ok(spec);
		}
		let items = positional_args_from_object(raw)
			.map_err(|_| "__slots__ must be a string or iterable of strings".to_owned())?;
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

pub(crate) unsafe fn set_declared_bases_during_construction(
	ty: *mut PyType,
	value: *mut PyObject,
) -> c_int {
	if value.is_null() {
		let _ = unsafe {
			abi::exc::pon_raise_type_error(
				"__bases__ may not be deleted".as_ptr(),
				"__bases__ may not be deleted".len(),
			)
		};
		return -1;
	}
	if !abi::class_construction_active() {
		let message = "attribute '__bases__' of 'type' objects is not writable";
		let _ =
			abi::exc::raise_kind_error_text(crate::types::exc::ExceptionKind::AttributeError, message);
		return -1;
	}
	let bases = match unsafe { positional_args_from_object(value) } {
		Ok(bases) => bases,
		Err(_) => {
			let _ = unsafe {
				abi::exc::pon_raise_type_error(
					"__bases__ must be set to a tuple of types".as_ptr(),
					"__bases__ must be set to a tuple of types".len(),
				)
			};
			return -1;
		},
	};
	let mut typed = Vec::with_capacity(bases.len());
	for base in bases {
		if base.is_null() || unsafe { !is_type_object(base) } {
			let _ = unsafe {
				abi::exc::pon_raise_type_error(
					"__bases__ must be set to a tuple of types".as_ptr(),
					"__bases__ must be set to a tuple of types".len(),
				)
			};
			return -1;
		}
		typed.push(base.cast::<PyType>());
	}
	unsafe { mro::set_declared_bases(ty, &typed) };
	0
}

unsafe fn validate_slot_layout(
	namespace: &PyClassDict,
	bases: &[*mut PyType],
	spec: &SlotSpec,
) -> bool {
	for slot in &spec.names {
		if namespace.get(*slot).is_some() {
			let spelling = intern::resolve(*slot).unwrap_or_else(|| format!("<interned:{slot}>"));
			let message = format!("'{spelling}' in __slots__ conflicts with class variable");
			unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
			return false;
		}
	}
	if spec.declared
		&& spec.wants_dict
		&& bases
			.iter()
			.any(|base| unsafe { !base.is_null() && (**base).tp_dictoffset != 0 })
	{
		let message = "__dict__ slot disallowed: we already got one";
		unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
		return false;
	}
	let slotted_bases = bases
		.iter()
		.copied()
		.filter(|base| unsafe { own_slot_count(*base) != 0 })
		.count();
	if slotted_bases > 1 {
		let message = "multiple bases have instance lay-out conflict";
		unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
		return false;
	}
	true
}

fn namespace_allows_dict(bases: &[*mut PyType], spec: &SlotSpec) -> bool {
	!spec.declared
		|| spec.wants_dict
		|| bases
			.iter()
			.any(|base| unsafe { !base.is_null() && (**base).tp_dictoffset != 0 })
}

/// Python-level construction hooks (`__new__`/`__init__`) found on a
/// metaclass strictly below the builtin `type` in MRO order.
struct MetaclassHooks {
	new_hook:  *mut PyObject,
	init_hook: *mut PyObject,
}

impl MetaclassHooks {
	fn any(&self) -> bool {
		!self.new_hook.is_null() || !self.init_hook.is_null()
	}
}

/// Scan `meta`'s MRO for Python construction hooks, stopping at the builtin
/// `type` whose entries only describe default construction.
unsafe fn metaclass_construction_hooks(meta: *mut PyType) -> MetaclassHooks {
	let mut hooks = MetaclassHooks { new_hook: ptr::null_mut(), init_hook: ptr::null_mut() };
	let type_type = abi::runtime_type_type();
	if meta.is_null() || meta == type_type {
		return hooks;
	}
	let new_id = intern::intern("__new__");
	let init_id = intern::intern("__init__");
	for cls in unsafe { mro::mro_entries(meta) } {
		if cls == type_type
			|| unsafe {
				!cls.is_null()
					&& (*cls).gc_type_id != TYPE_ID_HEAP_INSTANCE.0 as usize
					&& (*cls).name() == "type"
			} {
			break;
		}
		if cls.is_null() {
			continue;
		}
		let dict = unsafe { (*cls).tp_dict.cast::<PyClassDict>() };
		if dict.is_null() {
			continue;
		}
		if hooks.new_hook.is_null() {
			if let Some(value) = unsafe { (&*dict).get(new_id) } {
				hooks.new_hook = value;
			}
		}
		if hooks.init_hook.is_null() {
			if let Some(value) = unsafe { (&*dict).get(init_id) } {
				hooks.init_hook = value;
			}
		}
	}
	hooks
}

/// Materialize an internal class namespace as a Python dict object.
unsafe fn namespace_to_dict_object(namespace: *mut PyClassDict) -> *mut PyObject {
	let out = unsafe { abi::map::pon_build_map(ptr::null_mut(), 0) };
	if out.is_null() || namespace.is_null() {
		return out;
	}
	for (name, value) in unsafe { (&*namespace).iter() } {
		let Some(spelling) = intern::resolve(name) else {
			continue;
		};
		let key = unsafe { abi::pon_const_str(spelling.as_ptr(), spelling.len()) };
		if key.is_null() {
			return ptr::null_mut();
		}
		if unsafe { abi::map::pon_dict_set_item_status(out, key, value) } < 0 {
			return ptr::null_mut();
		}
	}
	out
}

/// Call one resolved metaclass hook with `(head, name, bases, ns)` plus class
/// keywords. The caller passes the same bound callable Python attribute access
/// would produce (`meta.__prepare__`, `meta.__new__`, `meta.__init__`).
unsafe fn call_constructor_hook(
	callable: *mut PyObject,
	argv: &[*mut PyObject],
	keywords: &[ClassKeyword],
) -> *mut PyObject {
	if callable.is_null() {
		return ptr::null_mut();
	}
	if keywords.is_empty() {
		let mut call_argv = argv.to_vec();
		return unsafe { abi::pon_call(callable, call_argv.as_mut_ptr(), call_argv.len()) };
	}
	let kw_names = keywords
		.iter()
		.map(|keyword| keyword.name)
		.collect::<Vec<_>>();
	let mut kw_values = keywords
		.iter()
		.map(|keyword| keyword.value)
		.collect::<Vec<_>>();
	let mut call_argv = argv.to_vec();
	unsafe {
		abi::call::pon_call_ex(
			callable,
			if call_argv.is_empty() {
				ptr::null_mut()
			} else {
				call_argv.as_mut_ptr()
			},
			call_argv.len(),
			ptr::null_mut(),
			kw_names.as_ptr(),
			kw_values.as_mut_ptr(),
			kw_names.len(),
			ptr::null_mut(),
			ptr::null_mut(),
		)
	}
}

/// CPython `__build_class__` parity: invoke `meta(name, bases, ns, **kwds)`
/// through the metaclass's Python `__new__`/`__init__` hooks.
unsafe fn call_metaclass_constructor(
	metaclass: *mut PyType,
	name: &str,
	base_types: &[*mut PyType],
	namespace: *mut PyClassDict,
	ns_override: *mut PyObject,
	keywords: &[ClassKeyword],
	hooks: &MetaclassHooks,
) -> *mut PyObject {
	let name_object = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
	if name_object.is_null() {
		return ptr::null_mut();
	}
	let mut base_objects = base_types
		.iter()
		.map(|base| base.cast::<PyObject>())
		.collect::<Vec<_>>();
	let bases_tuple = unsafe {
		abi::seq::pon_build_tuple(
			if base_objects.is_empty() {
				ptr::null_mut()
			} else {
				base_objects.as_mut_ptr()
			},
			base_objects.len(),
		)
	};
	if bases_tuple.is_null() {
		return ptr::null_mut();
	}
	// A `__prepare__`-provided mapping passes through to the hooks intact
	// (CPython hands the metaclass the very namespace the body executed in);
	// the plain fast path materializes the internal namespace as a dict.
	let ns_object = if ns_override.is_null() {
		unsafe { namespace_to_dict_object(namespace) }
	} else {
		ns_override
	};
	if ns_object.is_null() {
		return ptr::null_mut();
	}
	let class_keywords = keywords
		.iter()
		.copied()
		.filter(|keyword| intern::resolve(keyword.name).as_deref() != Some("metaclass"))
		.collect::<Vec<_>>();

	let cls = if hooks.new_hook.is_null() {
		unsafe { construct_class(metaclass, name, base_types, namespace, keywords) }
	} else {
		let argv = [metaclass.cast::<PyObject>(), name_object, bases_tuple, ns_object];
		let callable = unsafe {
			abi::pon_get_attr(metaclass.cast::<PyObject>(), intern::intern("__new__"), ptr::null_mut())
		};
		unsafe { call_constructor_hook(callable, &argv, &class_keywords) }
	};
	if cls.is_null() {
		return ptr::null_mut();
	}

	if !hooks.init_hook.is_null() {
		let cls_type = unsafe { object_type(cls) };
		if !cls_type.is_null() && unsafe { mro::is_subtype(cls_type, metaclass) } {
			let argv = [cls, name_object, bases_tuple, ns_object];
			let callable = unsafe {
				abi::pon_get_attr(
					metaclass.cast::<PyObject>(),
					intern::intern("__init__"),
					ptr::null_mut(),
				)
			};
			let result = unsafe { call_constructor_hook(callable, &argv, &class_keywords) };
			if result.is_null() {
				return ptr::null_mut();
			}
		}
	}
	cls
}

/// Python-visible `type.__new__`, installed on the builtin `type` object as a
/// staticmethod: `type.__new__(type, x)` queries a type; the 4-argument form
/// constructs a class with the requested metaclass (no hook re-dispatch, so
/// `super().__new__(mcls, ...)` inside a metaclass `__new__` terminates here).
pub unsafe extern "C" fn type_dunder_new(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { raw_arg_slice(argv, argc) } {
		Ok(args) => args,
		Err(message) => return raise_object(message),
	};
	if args.is_empty() {
		return raise_object("type.__new__(): not enough arguments");
	}
	let mcls = args[0];
	if mcls.is_null() || unsafe { !is_type_object(mcls) } {
		return raise_object("type.__new__(X): X is not a type object");
	}
	match args.len() {
		2 => {
			let ty = unsafe { object_type(args[1]) };
			if ty.is_null() {
				return raise_object("type() argument has no type");
			}
			unsafe { canonical_type_object(ty) }.cast::<PyObject>()
		},
		4 => {
			let Some(name) = (unsafe { unicode_text(args[1]) }) else {
				return raise_object("type.__new__() argument 1 must be str");
			};
			let bases = match unsafe { positional_args_from_object(args[2]) } {
				Ok(bases) => bases,
				Err(_) => return raise_object("type.__new__() argument 2 must be tuple"),
			};
			let namespace = match unsafe { namespace_from_mapping(args[3]) } {
				Ok(namespace) => namespace,
				Err(message) => return raise_object(message),
			};
			let Some((base_types, _)) = (unsafe { normalize_bases(&bases) }) else {
				return ptr::null_mut();
			};
			let Some(winner) = (unsafe { select_metaclass(&base_types, mcls) }) else {
				return ptr::null_mut();
			};
			unsafe { construct_class(winner, name, &base_types, namespace, &[]) }
		},
		n => raise_object(format!("type.__new__() takes exactly 3 arguments ({} given)", n - 1)),
	}
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
	unsafe { seed_namespace_module(namespace) };
	let Some((base_types, _)) = (unsafe { normalize_bases(bases) }) else {
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
	let hooks = unsafe { metaclass_construction_hooks(metaclass) };
	if hooks.any() {
		return unsafe {
			call_metaclass_constructor(
				metaclass,
				name,
				&base_types,
				namespace,
				ptr::null_mut(),
				keywords,
				&hooks,
			)
		};
	}
	unsafe { construct_class(metaclass, name, &base_types, namespace, keywords) }
}

/// CPython class bodies always see `__module__`: the compiler seeds
/// `__module__ = __name__` before the body runs, and `type.__new__` falls
/// back to the caller's globals when the namespace lacks it.  pon's lowering
/// emits neither, so class construction first consults the active module
/// object's live `__name__` attribute (important for modules that rebind it,
/// e.g. `_pydecimal` sets `__name__ = 'decimal'` for pickling) and only then
/// falls back to the cached import key / `__main__`.
unsafe fn seed_namespace_module(namespace: *mut PyClassDict) {
	if namespace.is_null() {
		return;
	}
	let module_id = intern::intern("__module__");
	if unsafe { (&*namespace).get(module_id) }.is_some() {
		return;
	}
	let active_name = crate::import::active_module_attr(intern::intern("__name__"))
		.and_then(|value| unsafe { unicode_text(value).map(str::to_owned) });
	let name = active_name
		.or_else(|| crate::import::active_module_name_id().and_then(intern::resolve))
		.unwrap_or_else(|| "__main__".to_owned());
	let value = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
	if value.is_null() {
		// Allocation failure is already recorded; construction surfaces it.
		return;
	}
	unsafe { (&mut *namespace).set(module_id, value) };
}

/// MRO scan for a Python-level `__prepare__` strictly below the builtin
/// `type`: the builtin default (a fresh empty dict) is exactly what the
/// plain fast path provides, so only user hooks select the mapping protocol.
unsafe fn find_prepare_hook(meta: *mut PyType) -> *mut PyObject {
	let type_type = abi::runtime_type_type();
	if meta.is_null() || meta == type_type {
		return ptr::null_mut();
	}
	let prepare_id = intern::intern("__prepare__");
	for cls in unsafe { mro::mro_entries(meta) } {
		if cls == type_type
			|| unsafe {
				!cls.is_null()
					&& (*cls).gc_type_id != TYPE_ID_HEAP_INSTANCE.0 as usize
					&& (*cls).name() == "type"
			} {
			break;
		}
		if cls.is_null() {
			continue;
		}
		let dict = unsafe { (*cls).tp_dict.cast::<PyClassDict>() };
		if dict.is_null() {
			continue;
		}
		if let Some(value) = unsafe { (&*dict).get(prepare_id) } {
			return value;
		}
	}
	ptr::null_mut()
}

/// Pre-body scope for one class statement, from [`prepare_class_scope`].
pub(crate) struct PreparedClassScope {
	/// `__prepare__`-provided namespace mapping; NULL selects the internal
	/// `PyClassDict` fast path.
	pub mapping:        *mut PyObject,
	/// Bases with `__mro_entries__` already resolved (CPython `update_bases`
	/// runs once, before `__prepare__` and the body).
	pub bases:          Vec<*mut PyObject>,
	/// Typing helpers detected on the original bases (`typing.NamedTuple` /
	/// `typing.TypedDict`), for a targeted post-body functional-syntax detour.
	pub typing_special: Option<TypingSpecialBase>,
	/// Original bases tuple when `__mro_entries__` resolution fired (the
	/// class body publishes it as `__orig_bases__`), NULL when bases were
	/// used as written.  Rooted by the caller's `ClassBodyFrame` across the
	/// body-execution and construction windows.
	pub orig_bases:     *mut PyObject,
}

/// CPython `__build_class__` prepare step: resolve the winning metaclass and
/// call its Python-level `__prepare__(name, bases, **kwds)` when one exists
/// below the builtin `type`.  Ordinary classes skip the call entirely — the
/// internal class namespace IS `type.__prepare__`'s empty dict.  Returns
/// `Err(())` with the exception set when resolution or the hook fails.
pub(crate) unsafe fn prepare_class_scope(
	name: &str,
	bases: &[*mut PyObject],
	keywords: &[ClassKeyword],
) -> Result<PreparedClassScope, ()> {
	let typing_special = unsafe { typing_special_base(bases) };
	let Some((base_types, orig_bases)) = (unsafe { normalize_bases(bases) }) else {
		return Err(());
	};
	let resolved_bases = base_types
		.iter()
		.map(|base| base.cast::<PyObject>())
		.collect::<Vec<_>>();
	let explicit_meta = keywords
		.iter()
		.find(|keyword| intern::resolve(keyword.name).as_deref() == Some("metaclass"))
		.map(|keyword| keyword.value)
		.unwrap_or(ptr::null_mut());
	let Some(metaclass) = (unsafe { select_metaclass(&base_types, explicit_meta) }) else {
		return Err(());
	};
	let hook = unsafe { find_prepare_hook(metaclass) };
	if hook.is_null() {
		return Ok(PreparedClassScope {
			mapping: ptr::null_mut(),
			bases: resolved_bases,
			typing_special,
			orig_bases,
		});
	}
	let name_object = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
	if name_object.is_null() {
		return Err(());
	}
	let mut base_objects = resolved_bases.clone();
	let bases_tuple = unsafe {
		abi::seq::pon_build_tuple(
			if base_objects.is_empty() {
				ptr::null_mut()
			} else {
				base_objects.as_mut_ptr()
			},
			base_objects.len(),
		)
	};
	if bases_tuple.is_null() {
		return Err(());
	}
	let class_keywords = keywords
		.iter()
		.copied()
		.filter(|keyword| intern::resolve(keyword.name).as_deref() != Some("metaclass"))
		.collect::<Vec<_>>();
	let argv = [name_object, bases_tuple];
	let callable = unsafe {
		abi::pon_get_attr(
			metaclass.cast::<PyObject>(),
			intern::intern("__prepare__"),
			ptr::null_mut(),
		)
	};
	let mapping = unsafe { call_constructor_hook(callable, &argv, &class_keywords) };
	if mapping.is_null() {
		return Err(());
	}
	// CPython validates before running the body: the namespace must be a
	// mapping (concrete dict storage, or `__getitem__` reachable through the
	// MRO — the dict method natives install lazily, so storage is checked
	// first).
	let mapping_type = unsafe { object_type(mapping) };
	let supports_items = unsafe { dict::has_dict_storage(mapping) }
		|| (!mapping_type.is_null()
			&& unsafe {
				!descr::lookup_in_type(mapping_type, intern::intern("__getitem__")).is_null()
			});
	if !supports_items {
		let message = format!(
			"{}.__prepare__() must return a mapping, not {}",
			unsafe { (*metaclass).name() },
			unsafe { object_type_display(mapping) },
		);
		unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
		return Err(());
	}
	Ok(PreparedClassScope { mapping, bases: resolved_bases, typing_special, orig_bases })
}

unsafe fn typing_annotations_dict(namespace: *mut PyClassDict) -> *mut PyObject {
	if namespace.is_null() {
		return ptr::null_mut();
	}
	if let Some(existing) = unsafe { (&*namespace).get(intern::intern("__annotations__")) } {
		return existing;
	}
	// PEP 649: class-body annotations are deferred into the synthesized
	// `__annotate__` function rather than stored eagerly in `__annotations__`
	// (see `pon-ir` class lowering).  Materialize them in VALUE format,
	// mirroring `typing.NamedTupleMeta`'s `get_annotate_from_class_namespace`
	// path, so field names are recovered from the class body.
	if let Some(annotate) = unsafe { (&*namespace).get(intern::intern("__annotate__")) } {
		if !annotate.is_null() && annotate != unsafe { abi::pon_none() } {
			// Return the annotate result directly: a NULL (with its pending
			// exception) propagates a real annotation failure instead of being
			// flattened into an empty field set.
			let mut argv = [unsafe { abi::pon_const_int(1) }];
			return unsafe { abi::pon_call(annotate, argv.as_mut_ptr(), argv.len()) };
		}
	}
	// No `__annotate__`: a genuinely empty annotation set.
	unsafe { abi::map::pon_build_map(ptr::null_mut(), 0) }
}

pub(crate) unsafe fn build_typing_special_class(
	name: &str,
	namespace: *mut PyClassDict,
	keywords: &[ClassKeyword],
	kind: TypingSpecialBase,
) -> *mut PyObject {
	let typing = intern::intern("typing");
	let callable_name = match kind {
		TypingSpecialBase::NamedTuple => "NamedTuple",
		TypingSpecialBase::TypedDict => "TypedDict",
	};
	let Some(callable) = crate::import::module_attr(typing, intern::intern(callable_name)) else {
		return raise_object(format!("typing.{callable_name} is not available"));
	};
	let name_object = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
	if name_object.is_null() {
		return ptr::null_mut();
	}
	let annotations = unsafe { typing_annotations_dict(namespace) };
	if annotations.is_null() {
		return ptr::null_mut();
	}
	match kind {
		TypingSpecialBase::NamedTuple => {
			let Some(make_eager_annotate) =
				crate::import::module_attr(typing, intern::intern("_make_eager_annotate"))
			else {
				return raise_object("typing._make_eager_annotate is not available");
			};
			let Some(make_nmtuple) =
				crate::import::module_attr(typing, intern::intern("_make_nmtuple"))
			else {
				return raise_object("typing._make_nmtuple is not available");
			};
			let annotate_func = {
				let mut argv = [annotations];
				let result =
					unsafe { abi::pon_call(make_eager_annotate, argv.as_mut_ptr(), argv.len()) };
				if result.is_null() {
					return result;
				}
				result
			};
			let entries = match unsafe { dict::dict_entries_snapshot(annotations) } {
				Ok(entries) => entries,
				Err(message) => return raise_object(message),
			};
			let mut field_names = entries.iter().map(|entry| entry.key).collect::<Vec<_>>();
			let fields_list = unsafe {
				abi::seq::pon_build_list(
					if field_names.is_empty() {
						ptr::null_mut()
					} else {
						field_names.as_mut_ptr()
					},
					field_names.len(),
				)
			};
			if fields_list.is_null() {
				return ptr::null_mut();
			}
			let module_name = crate::import::active_module_name_id()
				.and_then(intern::resolve)
				.unwrap_or_else(|| "__main__".to_owned());
			let module_object = unsafe { abi::pon_const_str(module_name.as_ptr(), module_name.len()) };
			if module_object.is_null() {
				return ptr::null_mut();
			}
			// PEP 649 defaults: a field also bound in the class body is a
			// namedtuple default (CPython `NamedTupleMeta`).  Defaults must form
			// a contiguous trailing run.
			let mut default_values = Vec::new();
			for entry in &entries {
				let Some(field_name) = (unsafe { unicode_text(entry.key) }) else {
					return raise_object("NamedTuple field name is not a string");
				};
				match unsafe { (&*namespace).get(intern::intern(field_name)) } {
					Some(value) => default_values.push(value),
					None if !default_values.is_empty() => {
						return raise_object(format!(
							"Non-default namedtuple field {field_name} cannot follow default field"
						));
					},
					None => {},
				}
			}
			let defaults_list = unsafe {
				abi::seq::pon_build_list(
					if default_values.is_empty() {
						ptr::null_mut()
					} else {
						default_values.as_mut_ptr()
					},
					default_values.len(),
				)
			};
			if defaults_list.is_null() {
				return ptr::null_mut();
			}
			let mut argv = [name_object, fields_list, annotate_func, module_object, defaults_list];
			unsafe { abi::pon_call(make_nmtuple, argv.as_mut_ptr(), argv.len()) }
		},
		TypingSpecialBase::TypedDict => {
			let class_keywords = keywords
				.iter()
				.copied()
				.filter(|keyword| intern::resolve(keyword.name).as_deref() != Some("metaclass"))
				.collect::<Vec<_>>();
			let mut argv = [name_object, annotations];
			if class_keywords.is_empty() {
				return unsafe { abi::pon_call(callable, argv.as_mut_ptr(), argv.len()) };
			}
			let kw_names = class_keywords
				.iter()
				.map(|keyword| keyword.name)
				.collect::<Vec<_>>();
			let mut kw_values = class_keywords
				.iter()
				.map(|keyword| keyword.value)
				.collect::<Vec<_>>();
			unsafe {
				abi::call::pon_call_ex(
					callable,
					argv.as_mut_ptr(),
					argv.len(),
					ptr::null_mut(),
					kw_names.as_ptr(),
					kw_values.as_mut_ptr(),
					kw_names.len(),
					ptr::null_mut(),
					ptr::null_mut(),
				)
			}
		},
	}
}

/// Build a class whose body executed into a `__prepare__`-provided mapping.
/// Metaclass constructor hooks receive the mapping object itself (CPython
/// passes the prepared namespace through); default construction snapshots it
/// into the internal class namespace, exactly like `type.__new__` does.
#[must_use]
pub unsafe fn build_class_from_prepared_mapping(
	name: &str,
	bases: &[*mut PyObject],
	mapping: *mut PyObject,
	keywords: &[ClassKeyword],
) -> *mut PyObject {
	let Some((base_types, _)) = (unsafe { normalize_bases(bases) }) else {
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
	let hooks = unsafe { metaclass_construction_hooks(metaclass) };
	// The internal snapshot is consumed only by default construction; a
	// Python `__new__` hook receives the mapping itself and any conversion
	// happens inside the `super().__new__` chain (CPython `type.__new__`
	// copies the mapping at that point, after hook-side mutation).
	let namespace = if hooks.new_hook.is_null() {
		match unsafe { namespace_from_mapping(mapping) } {
			Ok(namespace) => namespace,
			Err(_) => {
				return raise_object(format!(
					"__prepare__ namespace of type '{}' does not convert to a class namespace",
					unsafe { object_type_display(mapping) },
				));
			},
		}
	} else {
		ptr::null_mut()
	};
	if hooks.any() {
		return unsafe {
			call_metaclass_constructor(
				metaclass,
				name,
				&base_types,
				namespace,
				mapping,
				keywords,
				&hooks,
			)
		};
	}
	unsafe { construct_class(metaclass, name, &base_types, namespace, keywords) }
}

/// CPython `type_new_staticmethod` parity: a plain function `__new__` in the
/// class namespace is implicitly wrapped in a `staticmethod` carrier before
/// the namespace becomes `tp_dict`, so `cls.__dict__['__new__']` exposes
/// `__func__`/`__wrapped__` (enum's `_simple_enum` and `_find_new_` read it)
/// and `cls.__new__` lookups never bind a receiver.  Instantiation is
/// unaffected: `call_type_from_argv` resolves the entry through
/// `descr::descriptor_get`, which pierces the carrier.  Anything that is not
/// exactly a plain function (already-wrapped carriers, arbitrary callables)
/// is left as written, matching CPython's `PyFunction_Check` gate.
unsafe fn wrap_dunder_new_as_staticmethod(namespace: *mut PyClassDict) {
	let new_id = intern::intern("__new__");
	let Some(value) = (unsafe { (&*namespace).get(new_id) }) else {
		return;
	};
	if unsafe { object_type_display(value) } != "function" {
		return;
	}
	let Some(carrier_type) = abi::runtime_global(intern::intern("staticmethod")) else {
		return;
	};
	if unsafe { !is_type_object(carrier_type) } {
		return;
	}
	// SAFETY: `carrier_type` is the builtin staticmethod type object and
	// `value` is a live function object owned by the namespace.  The carrier
	// box is kept alive by the namespace entry; the collector pierces it
	// (`push_namespace_value_roots`) to keep the wrapped function alive.
	let carrier =
		unsafe { crate::types::classmethod::new_staticmethod(carrier_type.cast::<PyType>(), value) };
	if !carrier.is_null() {
		unsafe { (&mut *namespace).set(new_id, carrier) };
	}
}

/// CPython `type_new_set_attrs` parity: a plain function `__init_subclass__`
/// in the class namespace is implicitly wrapped in a `classmethod` carrier
/// before the namespace becomes `tp_dict` (PEP 487).  Without the carrier a
/// chained `super().__init_subclass__(**kwargs)` resolves the parent hook as
/// an unbound plain function and loses the `cls` argument.  The
/// `PyFunction_Check` gate matches `wrap_dunder_new_as_staticmethod`.
unsafe fn wrap_init_subclass_as_classmethod(namespace: *mut PyClassDict) {
	let init_id = intern::intern("__init_subclass__");
	let Some(value) = (unsafe { (&*namespace).get(init_id) }) else {
		return;
	};
	if unsafe { object_type_display(value) } != "function" {
		return;
	}
	let Some(carrier_type) = abi::runtime_global(intern::intern("classmethod")) else {
		return;
	};
	if unsafe { !is_type_object(carrier_type) } {
		return;
	}
	// SAFETY: `carrier_type` is the builtin classmethod type object and
	// `value` is a live function object owned by the namespace (the
	// `wrap_dunder_new_as_staticmethod` contract).
	let carrier =
		unsafe { crate::types::classmethod::new_classmethod(carrier_type.cast::<PyType>(), value) };
	if !carrier.is_null() {
		unsafe { (&mut *namespace).set(init_id, carrier) };
	}
}

/// CPython `type_new` rule: a class whose namespace defines `__eq__` without
/// defining `__hash__` gets `__hash__ = None` stamped into the namespace
/// before it becomes `tp_dict` — instances then resolve the None marker
/// through the MRO and raise `unhashable type: '...'` when hashed.
/// Subclasses re-enable hashing by defining `__hash__` themselves.
unsafe fn stamp_unhashable_for_eq_without_hash(namespace: *mut PyClassDict) {
	let ns = unsafe { &mut *namespace };
	if ns.get(intern::intern("__eq__")).is_none() {
		return;
	}
	let hash_name = intern::intern("__hash__");
	if ns.get(hash_name).is_some() {
		return;
	}
	let none = unsafe { abi::pon_none() };
	if !none.is_null() {
		ns.set(hash_name, none);
	}
}

/// CPython `type_new_set_attrs` parity: a plain function `__class_getitem__`
/// in the class namespace is implicitly wrapped in a `classmethod` carrier
/// before the namespace becomes `tp_dict` (PEP 560).  Without the carrier
/// the subscript fallback in `abstract_op::subscript_get` would resolve the
/// hook unbound and call it without `cls`.  The `function` gate matches
/// `wrap_init_subclass_as_classmethod`.
unsafe fn wrap_class_getitem_as_classmethod(namespace: *mut PyClassDict) {
	let getitem_id = intern::intern("__class_getitem__");
	let Some(value) = (unsafe { (&*namespace).get(getitem_id) }) else {
		return;
	};
	if unsafe { object_type_display(value) } != "function" {
		return;
	}
	let Some(carrier_type) = abi::runtime_global(intern::intern("classmethod")) else {
		return;
	};
	if unsafe { !is_type_object(carrier_type) } {
		return;
	}
	// SAFETY: `carrier_type` is the builtin classmethod type object and
	// `value` is a live function object owned by the namespace (the
	// `wrap_dunder_new_as_staticmethod` contract).
	let carrier =
		unsafe { crate::types::classmethod::new_classmethod(carrier_type.cast::<PyType>(), value) };
	if !carrier.is_null() {
		unsafe { (&mut *namespace).set(getitem_id, carrier) };
	}
}

/// CPython compile-time `__qualname__` approximation: every class-body
/// callable — plain functions plus the payloads of
/// classmethod/staticmethod/property carriers — gets `ClassName.attr`
/// stamped unless a qualname was already assigned.  pickle's `save_global`
/// resolves staticmethods through exactly this dotted path (meson pickles
/// `EnvironmentVariables._set`).
unsafe fn stamp_member_qualnames(class_name: &str, namespace: *mut PyClassDict) {
	// Pre-runtime class construction (native-module seeds) must not allocate
	// attr dicts; the stamp is for user classes built after init.
	if abi::runtime_type_type().is_null() {
		return;
	}
	let ns = unsafe { &*namespace };
	for (name_id, value) in ns.iter() {
		let Some(attr_name) = intern::resolve(name_id) else {
			continue;
		};
		if value.is_null() || !crate::tag::is_heap(value) {
			continue;
		}
		let qualname = format!("{class_name}.{attr_name}");
		crate::types::function::stamp_function_qualname(value, &qualname);
		if value.is_null() || !crate::tag::is_heap(value) {
			continue;
		}
		// EXACT builtin carrier types only (pointer identity, never type
		// NAME): enum defines a Python class literally named `property`,
		// whose instances are heap instances, not `PyProperty` boxes.
		// SAFETY: Heap pointer with a live header per the guard above.
		let value_type = unsafe { (*value).ob_type.cast_mut() };
		if crate::native::builtins_mod::carrier_types().contains(&value_type) {
			// SAFETY: Carrier identity proved; PyClassMethod and
			// PyStaticMethod share the wrapped-callable offset.
			let callable =
				unsafe { (*value.cast::<crate::types::classmethod::PyClassMethod>()).callable };
			crate::types::function::stamp_function_qualname(callable, &qualname);
		} else if value_type == crate::native::builtins_mod::property_type() {
			// SAFETY: Exact-type identity proved the PyProperty layout.
			let property = unsafe { &*value.cast::<crate::types::property::PyProperty>() };
			for target in [property.fget, property.fset, property.fdel] {
				crate::types::function::stamp_function_qualname(target, &qualname);
			}
		}
	}
}

/// `type.__new__` core: allocate and publish the heap type object.
#[must_use]
unsafe fn construct_class(
	metaclass: *mut PyType,
	name: &str,
	base_types: &[*mut PyType],
	namespace: *mut PyClassDict,
	keywords: &[ClassKeyword],
) -> *mut PyObject {
	if namespace.is_null() {
		return raise_object("class namespace is NULL");
	}
	unsafe { wrap_dunder_new_as_staticmethod(namespace) };
	unsafe { wrap_init_subclass_as_classmethod(namespace) };
	unsafe { wrap_class_getitem_as_classmethod(namespace) };
	unsafe { stamp_unhashable_for_eq_without_hash(namespace) };
	unsafe { stamp_member_qualnames(name, namespace) };
	// CPython: `class C:` means `class C(object):` — the implicit terminus
	// applies to the CONSTRUCTED type (tp_base, MRO, registries) while the
	// Python-visible `bases` tuple handed to metaclasses stays as written.
	let object_default: [*mut PyType; 1];
	let base_types: &[*mut PyType] = if base_types.is_empty() {
		match abi::runtime_global(intern::intern("object")) {
			Some(object_type)
				if unsafe { is_type_object(object_type) }
					&& object_type.cast::<PyType>() != metaclass =>
			{
				object_default = [object_type.cast::<PyType>()];
				&object_default
			},
			_ => base_types,
		}
	} else {
		base_types
	};
	let slot_spec = match slot_spec_from_namespace(unsafe { &*namespace }) {
		Ok(spec) => spec,
		Err(message) => return raise_object(message),
	};
	if unsafe { !validate_slot_layout(&*namespace, base_types, &slot_spec) } {
		return ptr::null_mut();
	}

	// Every builtin ancestor's native tp_dict method surface must exist
	// before MRO lookups on the new class resolve through it: truth-testing
	// an empty `str` subclass needs `str.__len__` visible even when the
	// builtin type was never touched at type level in this process.
	for &base in base_types {
		for entry in unsafe { crate::mro::mro_entries(base) } {
			if !entry.is_null() && unsafe { (*entry).gc_type_id != TYPE_ID_HEAP_INSTANCE.0 as usize } {
				unsafe { crate::descr::ensure_builtin_type_surface(entry) };
			}
		}
	}

	let static_name = leak_type_name(name);
	// Classes linearizing over the builtin `dict` embed native dict storage in
	// their instances; the distinct basicsize doubles as the layout marker
	// (`dict::type_is_dict_subclass`), and the dict type's method surface must
	// exist before MRO lookups on the new class can resolve through it.
	let embeds_dict = unsafe { crate::types::dict::class_bases_embed_dict(base_types) };
	// Classes deriving BaseException share the boxed-exception instance layout
	// (their instances are built by the exception allocators, never
	// `alloc_heap_instance`); basicsize and gc_type_id are the layout markers.
	let derives_exception = base_types
		.iter()
		.any(|base| crate::abi::exc::type_derives_base_exception(base.cast_const()));
	let derives_exception_group = derives_exception
		&& base_types
			.iter()
			.any(|base| crate::abi::exc::type_derives_exception_group(base.cast_const()));
	let instance_size = if embeds_dict {
		crate::types::dict::ensure_dict_subclass_methods_installed();
		mem::size_of::<crate::types::dict::PyDictSubclassInstance>()
	} else if unsafe { crate::types::list::class_bases_embed_list(base_types) } {
		// Classes linearizing over the builtin `list` embed native list
		// storage in their instances; the distinct basicsize is the layout
		// marker (`list::type_is_list_subclass`), and the list type's
		// method/dunder surface must exist before MRO lookups on the new
		// class can resolve through it.
		crate::abi::seq::ensure_list_subclass_surface();
		mem::size_of::<crate::types::list::PyListSubclassInstance>()
	} else if unsafe { crate::types::tuple::class_bases_embed_tuple(base_types) } {
		// Classes linearizing over the builtin `tuple` embed native tuple
		// storage in their instances; the distinct basicsize is the layout
		// marker (`tuple::type_is_tuple_subclass`), and the tuple type's
		// method/dunder/`__new__` surface must exist before MRO lookups on
		// the new class can resolve through it.
		crate::abi::seq::ensure_tuple_subclass_surface();
		mem::size_of::<crate::types::tuple::PyTupleSubclassInstance>()
	} else if unsafe { crate::types::weakref::class_bases_embed_weakref(base_types) } {
		// Classes linearizing over `weakref.ref` (importlib bootstrap's
		// `KeyedRef`) reuse the payload layout with a canonical registered
		// ref as the payload; the ref type's `__new__`/`__call__` surface
		// must exist before MRO lookups on the new class resolve through it.
		crate::types::weakref::ensure_weakref_subclass_surface();
		mem::size_of::<PyPayloadSubclassInstance>()
	} else if unsafe { class_bases_embed_payload(base_types) } {
		// Classes linearizing over `str`/`int`/`bytes` embed a canonical
		// payload slot; the distinct basicsize is the layout marker
		// (`type_is_payload_subclass`).
		mem::size_of::<PyPayloadSubclassInstance>()
	} else if derives_exception_group {
		mem::size_of::<crate::types::exc::PyExceptionGroup>()
	} else if derives_exception {
		mem::size_of::<crate::types::exc::PyBaseException>()
	} else {
		mem::size_of::<PyHeapInstance>()
	};
	let mut ty = PyType::new(metaclass, static_name, instance_size);
	ty.tp_base = base_types.first().copied().unwrap_or(ptr::null_mut());
	ty.tp_dict = namespace.cast::<PyObject>();
	ty.tp_dictoffset = if namespace_allows_dict(base_types, &slot_spec) {
		1
	} else {
		0
	};
	if derives_exception {
		ty.tp_getattro = Some(crate::types::exc::exception_getattro);
		ty.tp_setattro = Some(crate::types::exc::exception_setattro);
		ty.gc_type_id = if derives_exception_group {
			crate::abi::TYPE_ID_EXCEPTION_GROUP.0 as usize
		} else {
			crate::abi::TYPE_ID_EXCEPTION.0 as usize
		};
	} else {
		ty.tp_getattro = Some(descr::generic_get_attr);
		ty.tp_setattro = Some(descr::generic_set_attr);
		ty.gc_type_id = TYPE_ID_HEAP_INSTANCE.0 as usize;
	}
	ty.tp_new = Some(type_new);
	ty.tp_init = Some(type_init);

	let ty = Box::into_raw(Box::new(ty));
	// GC visibility: the type box is malloc'd, so the collector can only keep
	// the namespace's GC values alive through the namespaced-type root set.
	crate::sync::register_namespaced_type(ty);
	// Declared-bases construction record for `cls.__bases__`: `tp_base` keeps
	// only the leading base, and the implicit-`object` default above is
	// Python-visible there too (CPython: `class C: pass` → `C.__bases__ ==
	// (object,)`).
	unsafe { mro::set_declared_bases(ty, base_types) };
	if unsafe { mro::set_c3_mro(ty, base_types) } < 0 {
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
	// Declared-base registry backing `cls.__subclasses__()`.
	for base in base_types.iter().copied() {
		if !base.is_null() && base != ty {
			crate::sync::register_direct_subclass(base, ty);
		}
	}

	install_slot_descriptors(ty, namespace, &slot_spec);
	for (name, value) in unsafe { (&*namespace).iter().collect::<Vec<_>>() } {
		let _ = update_slot_from_dunder(ty, name, value);
	}
	if unsafe { !call_set_names(ty, namespace) } {
		return ptr::null_mut();
	}
	if unsafe { !call_init_subclass(ty, base_types, keywords) } {
		return ptr::null_mut();
	}
	ty.cast::<PyObject>()
}

/// C-API `PyType_Ready` core: publishes a native type for a C-defined layout
/// WITHOUT the heap-class layout decision tree (`construct_class` bakes
/// `PyHeapInstance`-family sizes, dict offsets, and gc markers up front;
/// C-extension instances have their own `tp_basicsize` layout and no
/// instance dict).
///
/// The type gets `metaclass` as its native `ob_type` (NULL means plain
/// `type`). Slot bridging (`tp_new`/`tp_init`/`tp_repr`/...) is the caller's
/// follow-up; entries already in `namespace` participate in attribute lookup
/// immediately.
///
/// `PyType_Ready` parity: `__set_name__`/`__init_subclass__` do NOT run for
/// C-defined types.
///
/// # Safety
///
/// `metaclass` (when non-NULL) and `base_types` entries must be live type
/// objects and `namespace` a live class dict; the runtime must be initialized.
pub(crate) unsafe fn construct_capi_class(
	metaclass: *mut PyType,
	name: &str,
	base_types: &[*mut PyType],
	namespace: *mut PyClassDict,
	basicsize: usize,
	itemsize: isize,
	gc_type_id: usize,
) -> *mut PyObject {
	if namespace.is_null() {
		return raise_object("class namespace is NULL");
	}
	// Builtin ancestors' native method surfaces must exist before MRO
	// lookups on the new class resolve through them.
	for &base in base_types {
		for entry in unsafe { crate::mro::mro_entries(base) } {
			if !entry.is_null() && unsafe { (*entry).gc_type_id != TYPE_ID_HEAP_INSTANCE.0 as usize } {
				unsafe { crate::descr::ensure_builtin_type_surface(entry) };
			}
		}
	}
	let metaclass = if metaclass.is_null() {
		abi::runtime_type_type()
	} else {
		metaclass
	};
	let static_name = leak_type_name(name);
	let mut ty = PyType::new(metaclass, static_name, basicsize);
	ty.tp_base = base_types.first().copied().unwrap_or(ptr::null_mut());
	ty.tp_dict = namespace.cast::<PyObject>();
	ty.tp_dictoffset = 0;
	ty.tp_itemsize = itemsize;
	ty.tp_getattro = Some(descr::generic_get_attr);
	ty.tp_setattro = Some(descr::generic_set_attr);
	ty.gc_type_id = gc_type_id;

	let ty = Box::into_raw(Box::new(ty));
	unsafe { mro::set_declared_bases(ty, base_types) };
	if unsafe { mro::set_c3_mro(ty, base_types) } < 0 {
		// Nothing was published yet; reclaim the box so a failed
		// `PyType_Ready` leaves no stale roots behind.
		// SAFETY: `ty` came from `Box::into_raw` above and never escaped.
		drop(unsafe { Box::from_raw(ty) });
		return ptr::null_mut();
	}
	// Publication only after every fallible step succeeded.
	// GC visibility: the type box is malloc'd, so the collector can only keep
	// the namespace's GC values alive through the namespaced-type root set.
	crate::sync::register_namespaced_type(ty);
	for ancestor in unsafe { mro::mro_entries(ty) } {
		if !ancestor.is_null() && ancestor != ty {
			crate::sync::register_subclass(ancestor, ty);
		}
	}
	for base in base_types.iter().copied() {
		if !base.is_null() && base != ty {
			crate::sync::register_direct_subclass(base, ty);
		}
	}
	for (name, value) in unsafe { (&*namespace).iter().collect::<Vec<_>>() } {
		let _ = update_slot_from_dunder(ty, name, value);
	}
	ty.cast::<PyObject>()
}

fn member_descriptor_type() -> *mut PyType {
	static MEMBER_DESCRIPTOR_TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut ty = PyType::new(
			abi::runtime_type_type(),
			"member_descriptor",
			mem::size_of::<PyMemberDescriptor>(),
		);
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
			owner:   ty,
			name:    *slot,
			kind:    PyMemberKind::Slot,
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

/// Returns true when `object`'s class marks the boxed-exception instance
/// layout (builtin exception classes leave `gc_type_id` unset; heap classes
/// deriving `BaseException` are stamped by `construct_class`).
unsafe fn instance_uses_exception_layout(object: *mut PyObject) -> bool {
	let ty = unsafe { object_type(object) };
	!ty.is_null()
		&& matches!(
			 unsafe { (*ty).gc_type_id },
			 id if id == crate::abi::TYPE_ID_EXCEPTION.0 as usize || id == crate::abi::TYPE_ID_EXCEPTION_GROUP.0 as usize
		)
}

unsafe fn is_member_descriptor(value: *mut PyObject) -> bool {
	!value.is_null()
		&& crate::tag::is_heap(value)
		&& unsafe { (*value).ob_type == member_descriptor_type().cast_const() }
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

unsafe extern "C" fn member_descriptor_get(
	descr: *mut PyObject,
	obj: *mut PyObject,
	_owner: *mut PyObject,
) -> *mut PyObject {
	if descr.is_null() {
		return raise_object("member descriptor is NULL");
	}
	if obj.is_null() {
		return descr;
	}
	let descr = unsafe { &*descr.cast::<PyMemberDescriptor>() };
	// Exception-layout receivers carry their own slot vector instead of the
	// generic PyHeapInstance prefix.
	if unsafe { instance_uses_exception_layout(obj) } {
		return match descr.kind {
			PyMemberKind::Slot => {
				let exception = unsafe { &*obj.cast::<crate::types::exc::PyBaseException>() };
				let value = slot_value_get(&exception.slots, descr.name);
				if value.is_null() {
					unsafe { crate::abi::exc::pon_raise_attribute_error(obj, descr.name) }
				} else {
					value
				}
			},
			PyMemberKind::Dict => {
				let exception = unsafe { &mut *obj.cast::<crate::types::exc::PyBaseException>() };
				let dict = if exception.dict.is_null() {
					exception.dict = new_namespace();
					exception.dict
				} else {
					exception.dict
				};
				unsafe { crate::descr::class_dict_to_dict(dict) }
			},
		};
	}
	let obj_ty = unsafe { object_type(obj) };
	if obj_ty.is_null() || unsafe { !mro::is_subtype(obj_ty, descr.owner) } {
		return raise_object("descriptor does not apply to this object");
	}
	let instance = obj.cast::<PyHeapInstance>();
	match descr.kind {
		PyMemberKind::Slot => {
			let value = unsafe { instance_get_slot(instance, descr.name) };
			if value.is_null() {
				// Unset `__slots__` slot: raise a catchable `AttributeError`
				// (CPython parity).  The lazy `try: self._x except AttributeError`
				// cache pattern — e.g. pathlib's `PurePath.__str__` caching
				// `_str` — depends on catching this, so it must NOT be an
				// uncatchable runtime error.
				return unsafe { crate::abi::exc::pon_raise_attribute_error(obj, descr.name) };
			}
			value
		},
		PyMemberKind::Dict => {
			// Live view, never the raw internal `PyClassDict` (typeless) —
			// and never a snapshot: mock-style `self.__dict__[k] = v` must
			// land in attribute storage.  Materializes an empty namespace
			// for fresh instances (CPython parity).
			unsafe { crate::types::instance_dict::new_view(instance) }
		},
	}
}

unsafe extern "C" fn member_descriptor_set(
	descr: *mut PyObject,
	obj: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
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
	if unsafe { instance_uses_exception_layout(obj) } {
		let exception = unsafe { &mut *obj.cast::<crate::types::exc::PyBaseException>() };
		return match descr.kind {
			PyMemberKind::Slot => {
				if slot_value_set(&mut exception.slots, descr.name, value) {
					0
				} else {
					pon_err_set("slot storage is missing");
					-1
				}
			},
			PyMemberKind::Dict => {
				pon_err_set("__dict__ attribute is read-only");
				-1
			},
		};
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
		},
		PyMemberKind::Dict => {
			pon_err_set("__dict__ attribute is read-only");
			-1
		},
	}
}

/// Generic `type.__new__` used for ordinary Python classes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn type_new(
	cls: *mut PyType,
	args: *mut PyObject,
	_kwargs: *mut PyObject,
) -> *mut PyObject {
	if cls.is_null() {
		return raise_object("cannot instantiate NULL type");
	}
	// PEP 3119: ABCMeta stores a non-empty `__abstractmethods__` frozenset in
	// the class's own dict; instantiating such a class is a TypeError.
	let type_dict = unsafe { (*cls).tp_dict.cast::<PyClassDict>() };
	if !type_dict.is_null() {
		if let Some(abstracts) = unsafe { (&*type_dict).get(intern::intern("__abstractmethods__")) } {
			if unsafe { abi::pon_is_true(abstracts) } == 1 {
				let message = format!(
					"Can't instantiate abstract class {} without an implementation for its abstract \
					 methods",
					unsafe { (*cls).name() }
				);
				unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
				return ptr::null_mut();
			}
		}
	}
	// Exception-derived classes never use the heap-instance layout: route to
	// the boxed-exception allocator so every instance is attribute- and
	// raise-compatible (`E('x').args`, `raise E(...)`).
	if crate::abi::exc::type_derives_base_exception(cls.cast_const()) {
		if crate::abi::exc::type_derives_exception_group(cls.cast_const()) {
			const MESSAGE: &str =
				"exception groups must be created by calling the class with (message, exceptions)";
			unsafe { abi::exc::pon_raise_type_error(MESSAGE.as_ptr(), MESSAGE.len()) };
			return ptr::null_mut();
		}
		let ctor_args = match unsafe { positional_args_from_object(args) } {
			Ok(ctor_args) => ctor_args,
			Err(message) => return raise_object(message),
		};
		return crate::abi::exc::alloc_exception_instance(cls, &ctor_args);
	}
	let dict = if unsafe { (*cls).tp_dictoffset != 0 } {
		new_namespace()
	} else {
		ptr::null_mut()
	};
	let slots = slot_storage(cls);
	if unsafe { crate::types::dict::type_is_dict_subclass(cls) } {
		// Dict-derived classes allocate the extended layout: heap-instance
		// prefix plus embedded dict storage.
		return match crate::abi::map::alloc_dict_subclass_instance(cls, dict, slots) {
			Ok(object) => object,
			Err(message) => raise_object(message),
		};
	}
	if unsafe { crate::types::list::type_is_list_subclass(cls) } {
		// List-derived classes allocate the extended layout: heap-instance
		// prefix plus empty embedded list storage.
		return match crate::abi::seq::alloc_list_subclass_instance(cls, dict, slots) {
			Ok(object) => object,
			Err(message) => raise_object(message),
		};
	}
	if unsafe { crate::types::tuple::type_is_tuple_subclass(cls) } {
		// Tuple-derived classes inherit `tuple.__new__` construction: the
		// instance embeds the iterable's items at allocation time (tuples
		// are immutable — no `__init__` leg populates them later).
		let ctor_args = match unsafe { positional_args_from_object(args) } {
			Ok(ctor_args) => ctor_args,
			Err(message) => return raise_object(message),
		};
		let values = match crate::abi::seq::tuple_ctor_values(&ctor_args) {
			Ok(values) => values,
			Err(message) => return raise_object(message),
		};
		return match crate::abi::seq::alloc_tuple_subclass_instance(cls, dict, slots, &values) {
			Ok(object) => object,
			Err(message) => raise_object(message),
		};
	}
	if unsafe { type_is_payload_subclass(cls) } {
		// Payload-derived classes allocate the extended layout; the payload
		// slot starts empty and is filled by
		// `str.__new__`/`int.__new__`/`bytes.__new__`.
		return match unsafe {
			abi::alloc_payload_subclass_instance(cls, dict, slots, ptr::null_mut())
		} {
			Ok(object) => object,
			Err(message) => raise_object(message),
		};
	}
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
		},
		Err(message) => raise_object(message),
	}
}

pub(crate) fn slot_storage(cls: *mut PyType) -> Vec<PySlotValue> {
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
				if descr.kind == PyMemberKind::Slot
					&& descr.owner == ty
					&& !slots.iter().any(|slot: &PySlotValue| slot.name == name)
				{
					slots.push(PySlotValue { name, value: ptr::null_mut() });
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
					function::KeywordArgs { names: &[], values: &[] },
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
			// CPython 3.12+: the original exception propagates (with a note
			// attached); never replace it.  Only synthesize the context
			// message when the callee failed without raising.
			if !crate::thread_state::pon_err_occurred() {
				pon_err_set(format!(
					"Error calling __set_name__ on '{}' instance '{}' in '{}'",
					unsafe { (*value_ty).name() },
					spelling,
					unsafe { (*ty).name() }
				));
			}
			return false;
		}
	}
	true
}

unsafe fn call_init_subclass(
	ty: *mut PyType,
	base_types: &[*mut PyType],
	keywords: &[ClassKeyword],
) -> bool {
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

	let kw_names = init_keywords
		.iter()
		.map(|keyword| keyword.name)
		.collect::<Vec<_>>();
	let mut kw_values = init_keywords
		.iter()
		.map(|keyword| keyword.value)
		.collect::<Vec<_>>();
	let keywords =
		function::KeywordArgs { names: kw_names.as_slice(), values: kw_values.as_slice() };
	let cls_object = ty.cast::<PyObject>();
	let result = if unsafe { object_type_display(init) == "function" } {
		let argv = [cls_object];
		unsafe {
			function::call_bound_function(init, &argv, keywords, None, None)
				.unwrap_or_else(|message| raise_object(message))
		}
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

/// Canonicalizes a helper-family "shadow" builtin type object (identified by
/// its missing metatype) to the installed builtin global of the same name, so
/// `type(x)` preserves identity (`type([]) is list`) and attribute access on
/// the result works.  Properly constructed types — user classes and installed
/// builtins — carry a metatype and pass through untouched.
pub(crate) unsafe fn canonical_type_object(ty: *mut PyType) -> *mut PyType {
	if ty.is_null() || !unsafe { (*ty).ob_base.ob_type.is_null() } {
		return ty;
	}
	let name = unsafe { (*ty).name() };
	if let Some(global) = abi::runtime_global(intern::intern(name)) {
		let meta = unsafe { (*global).ob_type };
		if !meta.is_null() && unsafe { (*meta).name() } == "type" {
			let global_ty = global.cast::<PyType>();
			if unsafe { (*global_ty).name() } == name {
				return global_ty;
			}
		}
	}
	// No installed global under this name: repair the missing metatype in
	// place so attribute access (`.__name__`, ...) works on the shadow type.
	let meta = abi::runtime_type_type();
	if !meta.is_null() {
		unsafe { (*ty).ob_base.ob_type = meta };
	}
	ty
}

pub unsafe fn builtin_type(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { raw_arg_slice(argv, argc) } {
		Ok(args) => args,
		Err(message) => return raise_object(message),
	};
	// A trailing keyword marker carries `type(name, bases, ns, **kwds)` class
	// keywords from the phase-A binder (`metaclass`, PEP 487 keywords, enum's
	// `boundary`/`_simple`, ...); they ride to the metaclass constructor.
	let (args, keywords) = match args.split_last() {
		Some((&last, rest)) => match unsafe { crate::types::lazy_iter::kw_marker_pairs(last) } {
			Some(pairs) => (rest, pairs),
			None => (args, &[][..]),
		},
		None => (args, &[][..]),
	};
	match args.len() {
		1 if keywords.is_empty() => {
			let object = args[0];
			if object.is_null() {
				return raise_object("type() argument is NULL");
			}
			let ty = unsafe { object_type(object) };
			if ty.is_null() {
				return raise_object("type() argument has no type");
			}
			unsafe { canonical_type_object(ty) }.cast::<PyObject>()
		},
		3 => unsafe { build_class_from_type_args(args[0], args[1], args[2], keywords) },
		n => raise_object(format!("type() takes 1 or 3 arguments, got {n}")),
	}
}

unsafe fn raw_arg_slice<'a>(
	argv: *mut *mut PyObject,
	argc: usize,
) -> Result<&'a [*mut PyObject], String> {
	if argv.is_null() && argc != 0 {
		return Err("argv pointer is null".to_owned());
	}
	Ok(if argc == 0 {
		&[]
	} else {
		unsafe { core::slice::from_raw_parts(argv.cast_const(), argc) }
	})
}

unsafe fn build_class_from_type_args(
	name: *mut PyObject,
	bases: *mut PyObject,
	namespace: *mut PyObject,
	keywords: &[(u32, *mut PyObject)],
) -> *mut PyObject {
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
	let keywords = keywords
		.iter()
		.map(|&(name, value)| ClassKeyword { name, value })
		.collect::<Vec<_>>();
	unsafe { build_class_from_namespace(name, &bases, namespace, &keywords) }
}

unsafe fn namespace_from_mapping(namespace: *mut PyObject) -> Result<*mut PyClassDict, String> {
	if namespace.is_null() {
		return Err("type.__new__() argument 3 must be dict".to_owned());
	}
	let ty = unsafe { object_type(namespace) };
	if ty.is_null() || unsafe { !dict::has_dict_storage(namespace) } {
		return Err("type.__new__() argument 3 must be dict".to_owned());
	}
	let entries = unsafe { dict::dict_entries_snapshot(namespace) }
		.map_err(|_| "type.__new__() argument 3 must be dict".to_owned())?;
	let out = new_namespace();
	for entry in entries {
		let Some(name) = (unsafe { unicode_text(entry.key) }) else {
			return Err("type.__new__() argument 3 keys must be str".to_owned());
		};
		// Values are copied raw: they may be tagged immediates, which class
		// dicts tolerate (descriptor probes, slot installers, and GC rooting
		// all guard with `tag::is_heap` before dereferencing).  Boxing here is
		// NOT an option — this runs inside the type-call path where the
		// runtime lock may be held, and boxing allocates through it.
		unsafe { (&mut *out).set(intern::intern(name), entry.value) };
	}
	Ok(out)
}

/// Generic no-op initializer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn type_init(
	_self: *mut PyObject,
	_args: *mut PyObject,
	_kwargs: *mut PyObject,
) -> c_int {
	0
}

/// True when construction proceeds to `__init__`/`tp_init` after `new`
/// produced `instance`. Shared by both constructor paths (`type_call` and
/// `abi::call_type_from_argv`) so their semantics cannot drift.
///
/// Builtin native constructors (`tp_new != type_new`) perform COMPLETE
/// construction: the returned object is fully initialized from the call
/// arguments, and a class-dict `__init__` resolving through the builtin's
/// MRO must not run a second construction pass — `list(map(...))` would
/// re-consume the exhausted iterator. C-extension classes are the
/// exception: their `tp_new` bridge only allocates (CPython `type_call`
/// semantics), so initialization still runs — but only when `tp_new`
/// produced an instance of `cls`, matching CPython's guard.
pub(crate) unsafe fn construction_runs_init(
	cls: *mut PyType,
	new: NewFunc,
	instance: *mut PyObject,
) -> bool {
	if new as *const () as usize == type_new as *const () as usize {
		return true;
	}
	if unsafe { !crate::capi::is_capi_class(cls) } || !crate::tag::is_heap(instance) {
		return false;
	}
	// SAFETY: heap-tagged instances carry a readable header.
	let instance_type = crate::capi::twin::registered_native_of_foreign(unsafe {
		(*instance)
			.ob_type
			.cast_mut()
			.cast::<crate::capi::twin::ForeignTypeObject>()
	})
	.unwrap_or_else(|| unsafe { (*instance).ob_type.cast_mut() });
	!instance_type.is_null() && unsafe { crate::mro::is_subtype(instance_type, cls) }
}

/// `__init__` resolution for the constructor paths: plain MRO lookup, except
/// that object's permissive default carrier must not shadow a C-extension
/// class's bridged `tp_init` — CPython's `PyType_Ready` installs a slot
/// wrapper in the class's own dict, which is more derived than
/// `object.__init__`. Any real `__init__` (own dict or a base class's)
/// still wins.
pub(crate) unsafe fn construction_init_override(cls: *mut PyType) -> *mut PyObject {
	let init = unsafe { descr::lookup_in_type(cls, intern::intern("__init__")) };
	if !init.is_null()
		&& init == abi::object_dunder_init_carrier()
		&& unsafe { crate::capi::is_capi_class(cls) }
		&& unsafe { (*cls).tp_init }.is_some()
	{
		return ptr::null_mut();
	}
	init
}
/// CPython `type_call` parity: `__init__` runs on the type of the object
/// `__new__` actually produced (`Py_TYPE(obj)`), not the class being called —
/// a `__new__` returning a subclass instance initializes through the subclass.
pub(crate) unsafe fn construction_init_receiver_type(
	cls: *mut PyType,
	instance: *mut PyObject,
) -> *mut PyType {
	if !crate::tag::is_heap(instance) {
		return cls;
	}
	// SAFETY: heap-tagged instances carry a readable header.
	let ty = crate::capi::twin::registered_native_of_foreign(unsafe {
		(*instance)
			.ob_type
			.cast_mut()
			.cast::<crate::capi::twin::ForeignTypeObject>()
	})
	.unwrap_or_else(|| unsafe { (*instance).ob_type.cast_mut() });
	if ty.is_null() { cls } else { ty }
}

/// `type.__call__`: allocate via `__new__`, then invoke `__init__` when
/// present.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn type_call(
	cls_obj: *mut PyObject,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	if cls_obj.is_null() {
		return raise_object("type call receiver is NULL");
	}
	let cls = cls_obj.cast::<PyType>();
	let explicit_args = match unsafe { positional_args_from_object(args) } {
		Ok(args) => args,
		Err(message) => return raise_object(message),
	};
	if !kwargs.is_null() && crate::tag::untag_arg(kwargs) != unsafe { abi::pon_none() } {
		let entries = match unsafe { dict::dict_entries_snapshot(crate::tag::untag_arg(kwargs)) } {
			Ok(entries) => entries,
			Err(message) => return raise_object(message),
		};
		let mut names = Vec::with_capacity(entries.len());
		let mut values = Vec::with_capacity(entries.len());
		for entry in entries {
			let key = crate::tag::untag_arg(entry.key);
			let Some(text) = (unsafe { unicode_text(key) }) else {
				return raise_object("type.__call__ keyword names must be str");
			};
			names.push(intern::intern(text));
			values.push(entry.value);
		}
		return unsafe {
			abi::call_type_with_keywords(
				cls_obj,
				&explicit_args,
				function::KeywordArgs { names: &names, values: &values },
			)
		};
	}
	let new = unsafe { (*cls).tp_new.unwrap_or(type_new) };
	let instance = unsafe { new(cls, args, kwargs) };
	if instance.is_null() {
		return ptr::null_mut();
	}
	// CPython `type_call`: `type(x)` with exactly one argument is a pure
	// type query — the answer never runs `__init__`.
	if explicit_args.len() == 1 && cls == abi::runtime_type_type() {
		return instance;
	}

	if !unsafe { construction_runs_init(cls, new, instance) } {
		return instance;
	}

	let init_cls = unsafe { construction_init_receiver_type(cls, instance) };
	let init = unsafe { construction_init_override(init_cls) };
	if !init.is_null() {
		let bound = unsafe { descr::descriptor_get(init, instance, init_cls) };
		if bound.is_null() {
			return ptr::null_mut();
		}
		let mut argv = explicit_args;
		let result = unsafe {
			abi::pon_call(
				bound,
				if argv.is_empty() {
					ptr::null_mut()
				} else {
					argv.as_mut_ptr()
				},
				argv.len(),
			)
		};
		if result.is_null() {
			return ptr::null_mut();
		}
	} else if let Some(init_slot) = unsafe { (*init_cls).tp_init } {
		// A C-extension class's bridged `tp_init` follows the CPython
		// contract: a real (possibly empty) tuple, never NULL.
		let args_object = if args.is_null() && unsafe { crate::capi::is_capi_class(init_cls) } {
			match abi::empty_args_tuple() {
				Ok(tuple) => tuple,
				Err(message) => return raise_object(message),
			}
		} else {
			args
		};
		if unsafe { init_slot(instance, args_object, kwargs) } < 0 {
			return ptr::null_mut();
		}
	}
	instance
}

fn slot_value_set(slots: &mut [PySlotValue], name: u32, value: *mut PyObject) -> bool {
	let Some(slot) = slots.iter_mut().find(|slot| slot.name == name) else {
		return false;
	};
	slot.value = value;
	true
}

#[must_use]
fn slot_value_get(slots: &[PySlotValue], name: u32) -> *mut PyObject {
	slots
		.iter()
		.find(|slot| slot.name == name)
		.map(|slot| slot.value)
		.unwrap_or(ptr::null_mut())
}

/// Store/delete an instance slot.  Returns true when the name was handled.
pub unsafe fn instance_set_slot(
	instance: *mut PyHeapInstance,
	name: u32,
	value: *mut PyObject,
) -> bool {
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
	unsafe {
		(&*instance)
			.slots
			.iter()
			.find(|slot| slot.name == name)
			.map(|slot| slot.value)
			.unwrap_or(ptr::null_mut())
	}
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
	use crate::thread_state::{pon_err_clear, test_state_lock};

	#[test]
	fn class_namespace_stores_attrs_and_dunders() {
		let _guard = test_state_lock();
		pon_err_clear();
		let ns = new_namespace();
		let value = unsafe { fake_str("callable") };
		unsafe {
			(&mut *ns).set(intern::dunder_call(), value);
			let cls = build_class_from_namespace("C", &[], ns, &[]).cast::<PyType>();
			assert!(!cls.is_null());
			assert_eq!((*cls).dunder_slots.call, value);
		}
	}

	#[test]
	fn slot_only_instance_rejects_unknown_dict_storage() {
		let _guard = test_state_lock();
		pon_err_clear();
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
		let _guard = test_state_lock();
		pon_err_clear();
		let mut type_type = PyType::new(ptr::null(), "type", mem::size_of::<PyType>());
		let type_ptr = &mut type_type as *mut PyType;
		unsafe { (*type_ptr).ob_base.ob_type = type_ptr };

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
		let _guard = test_state_lock();
		pon_err_clear();
		let mut type_type = PyType::new(ptr::null(), "type", mem::size_of::<PyType>());
		let type_ptr = &mut type_type as *mut PyType;
		unsafe { (*type_ptr).ob_base.ob_type = type_ptr };

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
		let _guard = test_state_lock();
		pon_err_clear();
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
		let ptr = &raw mut STR_TYPE;
		unsafe { (*ptr).ob_base.ob_type = ptr };
		Box::into_raw(Box::new(PyUnicode {
			ob_base:   PyObjectHeader::new(ptr),
			len:       text.len(),
			data:      text.as_ptr(),
			owns_data: false,
		}))
		.cast::<PyObject>()
	}
}
