//! Dictionary mapping implementation.

use core::{
	ffi::c_int,
	mem::{offset_of, size_of},
	ops::RangeInclusive,
	ptr,
	sync::atomic::{AtomicBool, Ordering},
};
use std::{cell::RefCell, sync::LazyLock};

use num_bigint::BigInt;

use crate::{
	capi::twin,
	object::{
		PyMappingMethods, PyNumberMethods, PyObject, PyObjectHeader, PySequenceMethods, PyType,
		PyUnicode,
	},
	thread_state::pon_err_set,
};

/// Boxed insertion-ordered Python `dict`.
#[repr(C)]
#[derive(Debug)]
pub struct PyDict {
	/// Common object header; this field must remain first.
	pub ob_base: PyObjectHeader,
	/// Live entries in insertion order. Updating an existing key preserves its
	/// slot.
	pub entries: Vec<DictEntry>,
	/// Open-addressed key index table. Buckets store indexes into `entries`.
	pub buckets: Vec<Option<usize>>,
}

/// One insertion-ordered dictionary entry.
#[derive(Clone, Copy, Debug)]
pub struct DictEntry {
	/// Hashable Python key.
	pub key:   *mut PyObject,
	/// Associated Python value.
	pub value: *mut PyObject,
	/// Cached normalized hash for open-addressed lookup.
	pub hash:  isize,
}

/// Concrete dict payload shared by exact dicts and dict-subclass instances.
///
/// Layout contract: `PyDict`'s `entries`/`buckets` tail is exactly this pair
/// (const-asserted at the bottom of this module), so both layouts resolve to
/// the same storage view through [`dict_mut`]/[`dict_ref`].
#[repr(C)]
#[derive(Debug)]
pub struct PyDictStorage {
	/// Live entries in insertion order. Updating an existing key preserves its
	/// slot.
	pub entries: Vec<DictEntry>,
	/// Open-addressed key index table. Buckets store indexes into `entries`.
	pub buckets: Vec<Option<usize>>,
}

/// Heap-class instance layout for classes deriving from `dict`.
///
/// The generic heap-instance prefix keeps every instance-attribute, slot, and
/// weakref path working unchanged (they all cast to `PyHeapInstance`), while
/// the embedded storage powers the native dict protocol on the same object.
#[repr(C)]
#[derive(Debug)]
pub struct PyDictSubclassInstance {
	/// Generic heap-instance prefix; must remain first.
	pub base:    crate::types::type_::PyHeapInstance,
	/// Embedded native dict payload.
	pub storage: PyDictStorage,
}

/// Iterator over dictionary keys, values, or items.
#[repr(C)]
#[derive(Debug)]
pub struct PyDictIter {
	/// Common object header; this field must remain first.
	pub ob_base: PyObjectHeader,
	/// Dictionary being traversed.
	pub dict:    *mut PyObject,
	/// Next insertion-order index.
	pub index:   usize,
	/// Projection yielded by this iterator.
	pub kind:    DictIterKind,
}

/// Live dictionary view over keys, values, or items.
#[repr(C)]
#[derive(Debug)]
pub struct PyDictView {
	/// Common object header; this field must remain first.
	pub ob_base: PyObjectHeader,
	/// Dictionary reflected by this view.
	pub dict:    *mut PyObject,
	/// Projection yielded by this view.
	pub kind:    DictIterKind,
}

/// Dictionary iterator projection.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DictIterKind {
	/// Yield keys.
	Keys   = 0,
	/// Yield values.
	Values = 1,
	/// Yield key/value pairs as compact item objects.
	Items  = 2,
}

/// Builds the runtime type object for dictionaries.
#[must_use]
pub fn dict_type(type_type: *const PyType) -> *mut PyType {
	static TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut mapping = PyMappingMethods::EMPTY;
		mapping.mp_length = Some(dict_len_slot);
		mapping.mp_subscript = Some(dict_subscript_slot);
		mapping.mp_ass_subscript = Some(dict_ass_subscript_slot);

		let mut number = PyNumberMethods::EMPTY;
		number.nb_or = Some(dict_or_slot);
		number.nb_reflected_or = Some(dict_ror_slot);
		number.nb_inplace_or = Some(dict_ior_slot);

		let mut ty = PyType::new(ptr::null(), "dict", size_of::<PyDict>());
		ty.tp_as_mapping = Box::into_raw(Box::new(mapping));
		ty.tp_as_number = Box::into_raw(Box::new(number));
		ty.tp_richcmp = Some(dict_richcmp_slot);
		ty.tp_iter = Some(dict_iter_slot);
		ty.tp_getattro = Some(dict_getattro_slot);
		Box::into_raw(Box::new(ty)) as usize
	});
	let ty = *TYPE as *mut PyType;
	unsafe { install_type_type(ty, type_type) };
	ty
}

/// Builds the runtime type object for dictionary iterators.
#[must_use]
pub fn dict_iter_type(type_type: *const PyType) -> *mut PyType {
	static TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut ty = PyType::new(ptr::null(), "dict_keyiterator", size_of::<PyDictIter>());
		ty.tp_iternext = Some(dict_iter_next_slot);
		ty.tp_iter = Some(dict_iter_identity_slot);
		ty.tp_getattro = Some(crate::abstract_op::iterator_dunder_getattro);
		Box::into_raw(Box::new(ty)) as usize
	});
	let ty = *TYPE as *mut PyType;
	unsafe { install_type_type(ty, type_type) };
	ty
}

/// Builds the runtime type object for `dict_keys` views.
#[must_use]
pub fn dict_keys_type(type_type: *const PyType) -> *mut PyType {
	static TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut sequence = PySequenceMethods::EMPTY;
		sequence.sq_length = Some(dict_view_len_slot);
		sequence.sq_contains = Some(dict_view_contains_slot);

		let mut number = PyNumberMethods::EMPTY;
		number.nb_subtract = Some(dict_view_difference_slot);
		number.nb_and = Some(dict_view_intersection_slot);
		number.nb_or = Some(dict_view_union_slot);
		number.nb_xor = Some(dict_view_symmetric_difference_slot);
		number.nb_reflected_subtract = Some(dict_view_reflected_difference_slot);
		number.nb_reflected_and = Some(dict_view_reflected_intersection_slot);
		number.nb_reflected_or = Some(dict_view_reflected_union_slot);
		number.nb_reflected_xor = Some(dict_view_reflected_symmetric_difference_slot);

		let mut ty = PyType::new(ptr::null(), "dict_keys", size_of::<PyDictView>());
		ty.tp_as_sequence = Box::into_raw(Box::new(sequence));
		ty.tp_as_number = Box::into_raw(Box::new(number));
		ty.tp_iter = Some(dict_view_iter_slot);
		ty.tp_repr = Some(dict_view_repr_slot);
		ty.tp_richcmp = Some(dict_view_richcmp_slot);
		ty.tp_getattro = Some(dict_view_getattro_slot);
		Box::into_raw(Box::new(ty)) as usize
	});
	let ty = *TYPE as *mut PyType;
	unsafe { install_type_type(ty, type_type) };
	ty
}

/// Builds the runtime type object for `dict_values` views.
#[must_use]
pub fn dict_values_type(type_type: *const PyType) -> *mut PyType {
	static TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut sequence = PySequenceMethods::EMPTY;
		sequence.sq_length = Some(dict_view_len_slot);
		sequence.sq_contains = Some(dict_view_contains_slot);

		let mut ty = PyType::new(ptr::null(), "dict_values", size_of::<PyDictView>());
		ty.tp_as_sequence = Box::into_raw(Box::new(sequence));
		ty.tp_iter = Some(dict_view_iter_slot);
		ty.tp_repr = Some(dict_view_repr_slot);
		ty.tp_getattro = Some(dict_view_getattro_slot);
		Box::into_raw(Box::new(ty)) as usize
	});
	let ty = *TYPE as *mut PyType;
	unsafe { install_type_type(ty, type_type) };
	ty
}

/// Builds the runtime type object for `dict_items` views.
#[must_use]
pub fn dict_items_type(type_type: *const PyType) -> *mut PyType {
	static TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut sequence = PySequenceMethods::EMPTY;
		sequence.sq_length = Some(dict_view_len_slot);
		sequence.sq_contains = Some(dict_view_contains_slot);

		let mut number = PyNumberMethods::EMPTY;
		number.nb_subtract = Some(dict_view_difference_slot);
		number.nb_and = Some(dict_view_intersection_slot);
		number.nb_or = Some(dict_view_union_slot);
		number.nb_xor = Some(dict_view_symmetric_difference_slot);
		number.nb_reflected_subtract = Some(dict_view_reflected_difference_slot);
		number.nb_reflected_and = Some(dict_view_reflected_intersection_slot);
		number.nb_reflected_or = Some(dict_view_reflected_union_slot);
		number.nb_reflected_xor = Some(dict_view_reflected_symmetric_difference_slot);

		let mut ty = PyType::new(ptr::null(), "dict_items", size_of::<PyDictView>());
		ty.tp_as_sequence = Box::into_raw(Box::new(sequence));
		ty.tp_as_number = Box::into_raw(Box::new(number));
		ty.tp_iter = Some(dict_view_iter_slot);
		ty.tp_repr = Some(dict_view_repr_slot);
		ty.tp_richcmp = Some(dict_view_richcmp_slot);
		ty.tp_getattro = Some(dict_view_getattro_slot);
		Box::into_raw(Box::new(ty)) as usize
	});
	let ty = *TYPE as *mut PyType;
	unsafe { install_type_type(ty, type_type) };
	ty
}

/// Selects the concrete dict-view type for an iterator projection.
#[must_use]
pub fn dict_view_type(type_type: *const PyType, kind: DictIterKind) -> *mut PyType {
	match kind {
		DictIterKind::Keys => dict_keys_type(type_type),
		DictIterKind::Values => dict_values_type(type_type),
		DictIterKind::Items => dict_items_type(type_type),
	}
}

/// Traces dictionary keys and values.
pub unsafe extern "C" fn trace_dict(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
	if object.is_null() {
		return;
	}
	let dict = unsafe { &*object.cast::<PyDict>() };
	for entry in &dict.entries {
		if !entry.key.is_null() {
			visitor(entry.key.cast::<u8>());
		}
		if !entry.value.is_null() {
			visitor(entry.value.cast::<u8>());
		}
	}
}

/// Drops dictionary-owned Rust storage.
pub unsafe extern "C" fn finalize_dict(object: *mut u8) {
	if object.is_null() {
		return;
	}
	let dict = unsafe { &mut *object.cast::<PyDict>() };
	unsafe { ptr::drop_in_place(ptr::addr_of_mut!(dict.entries)) };
	unsafe { ptr::drop_in_place(ptr::addr_of_mut!(dict.buckets)) };
}

/// Traces the dictionary retained by an iterator.
pub unsafe extern "C" fn trace_dict_iter(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
	if object.is_null() {
		return;
	}
	let iter = unsafe { &*object.cast::<PyDictIter>() };
	if !iter.dict.is_null() {
		visitor(iter.dict.cast::<u8>());
	}
}

/// Traces the dictionary retained by a view.
pub unsafe extern "C" fn trace_dict_view(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
	if object.is_null() {
		return;
	}
	let view = unsafe { &*object.cast::<PyDictView>() };
	if !view.dict.is_null() {
		visitor(view.dict.cast::<u8>());
	}
}

/// Initializes a freshly allocated dictionary object.
pub unsafe fn init_dict(ptr: *mut PyDict, ob_type: *const PyType, capacity: usize) {
	unsafe {
		ptr::write(ptr, PyDict {
			ob_base: PyObjectHeader::new(ob_type),
			entries: Vec::with_capacity(capacity),
			buckets: Vec::with_capacity(bucket_capacity(capacity)),
		});
	}
}

/// Initializes a freshly allocated dictionary iterator.
pub unsafe fn init_dict_iter(
	ptr: *mut PyDictIter,
	ob_type: *const PyType,
	dict: *mut PyObject,
	kind: DictIterKind,
) {
	unsafe {
		ptr::write(ptr, PyDictIter { ob_base: PyObjectHeader::new(ob_type), dict, index: 0, kind });
	}
}

/// Initializes a freshly allocated dictionary view.
pub unsafe fn init_dict_view(
	ptr: *mut PyDictView,
	ob_type: *const PyType,
	dict: *mut PyObject,
	kind: DictIterKind,
) {
	unsafe {
		ptr::write(ptr, PyDictView { ob_base: PyObjectHeader::new(ob_type), dict, kind });
	}
}

/// Returns whether `object` is an exact runtime dictionary.
#[must_use]
pub unsafe fn is_dict(object: *mut PyObject) -> bool {
	(unsafe { type_name(object) }) == Some("dict")
}

/// Returns whether `object` is a runtime dictionary iterator.
#[must_use]
pub unsafe fn is_dict_iter(object: *mut PyObject) -> bool {
	(unsafe { type_name(object) }) == Some("dict_keyiterator")
}

/// Returns whether `object` is any runtime dictionary view.
#[must_use]
pub unsafe fn is_dict_view(object: *mut PyObject) -> bool {
	matches!(unsafe { type_name(object) }, Some("dict_keys" | "dict_values" | "dict_items"))
}

/// Returns whether `object` is a set-like dictionary view.
#[must_use]
pub unsafe fn is_setlike_dict_view(object: *mut PyObject) -> bool {
	matches!(unsafe { type_name(object) }, Some("dict_keys" | "dict_items"))
}

/// Borrows a dictionary view payload.
#[must_use]
pub unsafe fn dict_view_ref(object: *mut PyObject) -> Option<&'static PyDictView> {
	if unsafe { is_dict_view(object) } {
		Some(unsafe { &*object.cast::<PyDictView>() })
	} else {
		None
	}
}

/// Returns a dictionary view's projection kind.
#[must_use]
pub unsafe fn dict_view_kind(object: *mut PyObject) -> Option<DictIterKind> {
	unsafe { dict_view_ref(object).map(|view| view.kind) }
}

/// Returns the dictionary reflected by a dictionary view.
#[must_use]
pub unsafe fn dict_view_dict(object: *mut PyObject) -> Option<*mut PyObject> {
	unsafe { dict_view_ref(object).map(|view| view.dict) }
}

/// Returns whether `ty` is a heap class whose instances embed dict storage.
///
/// The marker is the basicsize stamped by class construction for classes with
/// the builtin `dict` in their MRO; ordinary heap classes keep the plain
/// `PyHeapInstance` size.  Deliberately lock-free (no LazyLock, no runtime
/// lock) so it is safe on every dict fast path.
#[must_use]
pub unsafe fn type_is_dict_subclass(ty: *mut PyType) -> bool {
	if ty.is_null() {
		return false;
	}
	unsafe {
		(*ty).gc_type_id == crate::types::type_::TYPE_ID_HEAP_INSTANCE.0 as usize
			&& (*ty).tp_basicsize == size_of::<PyDictSubclassInstance>()
	}
}

/// Returns whether `object` is a dict-subclass heap instance.
#[must_use]
pub unsafe fn is_dict_subclass_instance(object: *mut PyObject) -> bool {
	if object.is_null() {
		return false;
	}
	unsafe { type_is_dict_subclass((*object).ob_type.cast_mut()) }
}

/// Returns whether `object` carries concrete dict storage: an exact dict or a
/// dict-subclass instance.  Dispatch fast paths that must honor user method
/// overrides keep using the exact [`is_dict`] check instead.
#[must_use]
pub unsafe fn has_dict_storage(object: *mut PyObject) -> bool {
	unsafe { is_dict(object) || is_dict_subclass_instance(object) }
}

/// Returns whether a class built over `bases` embeds native dict storage:
/// some base linearizes over the builtin `dict` type.  The name match is
/// restricted to non-heap types so helper-family shadow dict type objects
/// count while a user class merely NAMED "dict" does not.  Lock-free.
#[must_use]
pub unsafe fn class_bases_embed_dict(bases: &[*mut PyType]) -> bool {
	bases.iter().copied().any(|base| {
		unsafe { crate::mro::mro_entries(base) }
			.iter()
			.any(|entry| {
				!entry.is_null()
					&& unsafe {
						(**entry).gc_type_id != crate::types::type_::TYPE_ID_HEAP_INSTANCE.0 as usize
							&& (**entry).name() == "dict"
					}
			})
	})
}

/// Resolves the embedded [`PyDictStorage`] for both dict layouts.
unsafe fn dict_storage_ptr(object: *mut PyObject) -> Option<*mut PyDictStorage> {
	if unsafe { is_dict(object) } {
		return Some(
			unsafe { object.cast::<u8>().add(offset_of!(PyDict, entries)) }.cast::<PyDictStorage>(),
		);
	}
	if unsafe { is_dict_subclass_instance(object) } {
		return Some(unsafe {
			ptr::addr_of_mut!((*object.cast::<PyDictSubclassInstance>()).storage)
		});
	}
	None
}

/// Borrows the concrete dict storage of a dict-layout object mutably.
pub unsafe fn dict_mut(object: *mut PyObject) -> Result<&'static mut PyDictStorage, String> {
	match unsafe { dict_storage_ptr(object) } {
		Some(storage) => Ok(unsafe { &mut *storage }),
		None => Err("expected dict object".to_owned()),
	}
}

/// Borrows the concrete dict storage of a dict-layout object immutably.
pub unsafe fn dict_ref(object: *mut PyObject) -> Result<&'static PyDictStorage, String> {
	match unsafe { dict_storage_ptr(object) } {
		Some(storage) => Ok(unsafe { &*storage }),
		None => Err("expected dict object".to_owned()),
	}
}

/// Inserts or updates `key` in insertion order. Existing equal keys keep their
/// original slot.
///
/// Key hashing and equality may dispatch Python-level `__hash__`/`__eq__`
/// hooks (arbitrary user code): every storage borrow is scoped so none lives
/// across a dispatch, and `find_entry_index` re-validates indices itself.
pub unsafe fn dict_insert(
	dict: *mut PyObject,
	key: *mut PyObject,
	value: *mut PyObject,
) -> Result<(), String> {
	if key.is_null() {
		return Err("dict key is NULL".to_owned());
	}
	if value.is_null() {
		return Err("dict value is NULL".to_owned());
	}
	let hash = unsafe { hash_dict_key(key)? };
	ensure_dict_buckets(unsafe { dict_mut(dict)? })?;
	match unsafe { find_entry_index(dict, key, hash)? } {
		Some(index) => {
			let storage = unsafe { dict_mut(dict)? };
			storage.entries[index].value = value;
		},
		None => {
			let storage = unsafe { dict_mut(dict)? };
			ensure_dict_insert_capacity(storage)?;
			let index = storage.entries.len();
			storage.entries.push(DictEntry { key, value, hash });
			insert_bucket(&mut storage.buckets, &storage.entries, index)?;
		},
	}
	Ok(())
}

/// Hashes a prospective dict key, wrapping hash failures the way CPython 3.14
/// reports them: `cannot use 'tuple' as a dict key (unhashable type: 'list')`.
unsafe fn hash_dict_key(key: *mut PyObject) -> Result<isize, String> {
	unsafe { hash_object(key) }.map_err(|message| {
		if message.starts_with("unhashable type") {
			let name = unsafe { type_name(key) }.unwrap_or("object");
			format!("cannot use '{name}' as a dict key ({message})")
		} else {
			message
		}
	})
}

/// Gets a dictionary value without raising on a miss.
pub unsafe fn dict_get(
	dict: *mut PyObject,
	key: *mut PyObject,
) -> Result<Option<*mut PyObject>, String> {
	if key.is_null() {
		return Err("dict key is NULL".to_owned());
	}
	let hash = unsafe { hash_dict_key(key)? };
	ensure_dict_buckets(unsafe { dict_mut(dict)? })?;
	Ok(match unsafe { find_entry_index(dict, key, hash)? } {
		Some(index) => Some(unsafe { dict_ref(dict)? }.entries[index].value),
		None => None,
	})
}

/// Removes and returns a dictionary value without raising on a miss.
pub unsafe fn dict_remove(
	dict: *mut PyObject,
	key: *mut PyObject,
) -> Result<Option<*mut PyObject>, String> {
	if key.is_null() {
		return Err("dict key is NULL".to_owned());
	}
	let hash = unsafe { hash_dict_key(key)? };
	ensure_dict_buckets(unsafe { dict_mut(dict)? })?;
	Ok(match unsafe { find_entry_index(dict, key, hash)? } {
		Some(index) => {
			let storage = unsafe { dict_mut(dict)? };
			let value = storage.entries.remove(index).value;
			rebuild_dict_buckets(storage)?;
			Some(value)
		},
		None => None,
	})
}
/// Removes and returns the most-recently-inserted entry (`dict.popitem` LIFO
/// order), or `None` for an empty dict.
pub unsafe fn dict_pop_last(
	dict: *mut PyObject,
) -> Result<Option<(*mut PyObject, *mut PyObject)>, String> {
	let storage = unsafe { dict_mut(dict)? };
	let Some(entry) = storage.entries.pop() else {
		return Ok(None);
	};
	rebuild_dict_buckets(storage)?;
	Ok(Some((entry.key, entry.value)))
}

/// Removes every entry (`dict.clear`): both the entry vector and the bucket
/// index table are reset so stale indexes can never resolve.
pub unsafe fn dict_clear(dict: *mut PyObject) -> Result<Vec<*mut PyObject>, String> {
	let storage = unsafe { dict_mut(dict)? };
	let keys = storage.entries.iter().map(|entry| entry.key).collect();
	storage.entries.clear();
	storage.buckets.clear();
	Ok(keys)
}

/// Returns true if `key` is present in the dictionary.
pub unsafe fn dict_contains(dict: *mut PyObject, key: *mut PyObject) -> Result<bool, String> {
	unsafe { dict_get(dict, key).map(|value| value.is_some()) }
}

/// Merges exact-dict entries from `other` into `dict`.
pub unsafe fn dict_merge_exact(dict: *mut PyObject, other: *mut PyObject) -> Result<(), String> {
	let other_entries = unsafe { dict_ref(other)? }.entries.clone();
	for entry in other_entries {
		unsafe { dict_insert(dict, entry.key, entry.value)? };
	}
	Ok(())
}
/// Phase-1 accumulator for dict displays (`pon_build_map`): hashes `key` and
/// resolves duplicate keys with full Python equality BEFORE any runtime lock
/// is taken — user `__hash__`/`__eq__` re-enter runtime helpers that take the
/// runtime mutex, so they must never run inside `with_runtime`.  CPython
/// display semantics: the first duplicate key keeps its slot, the last value
/// wins.  `entries` is caller-local, so hook re-entrancy cannot touch it.
pub unsafe fn collect_prehashed_entry(
	entries: &mut Vec<DictEntry>,
	key: *mut PyObject,
	value: *mut PyObject,
) -> Result<(), String> {
	if key.is_null() {
		return Err("dict key is NULL".to_owned());
	}
	if value.is_null() {
		return Err("dict value is NULL".to_owned());
	}
	let hash = unsafe { hash_dict_key(key)? };
	for entry in entries.iter_mut() {
		if entry.hash == hash && unsafe { object_equal(entry.key, key)? } {
			entry.value = value;
			return Ok(());
		}
	}
	entries.push(DictEntry { key, value, hash });
	Ok(())
}

/// Phase-2 bulk fill for `pon_build_map`, safe under the runtime lock: keys
/// arrive pre-hashed and pre-deduplicated, so this is pure structural work
/// (the dict bucket build reads stored hashes, never objects).
pub unsafe fn dict_fill_prehashed(
	dict: *mut PyObject,
	entries: &[DictEntry],
) -> Result<(), String> {
	let storage = unsafe { dict_mut(dict)? };
	storage.entries.extend_from_slice(entries);
	rebuild_dict_buckets(storage)
}

/// Returns a stable insertion-order snapshot of dictionary entries.
pub unsafe fn dict_entries_snapshot(dict: *mut PyObject) -> Result<Vec<DictEntry>, String> {
	Ok(unsafe { dict_ref(dict)? }.entries.clone())
}

/// Returns true when two boxed objects compare equal for the Phase-B mapping
/// key domain.
///
/// Structural fast paths (numeric, weakref, str/bytes/tuple/frozenset) never
/// run Python code; a pair involving a heap-class operand whose MRO defines a
/// user `__eq__` dispatches the full rich-compare protocol instead (reflected
/// operand and identity fallback included), so user equality decides dict/set
/// key identity exactly like CPython's `lookdict`.  Callers holding a storage
/// borrow must use [`object_equal_structural`] and defer the `None` outcome.
pub unsafe fn object_equal(left: *mut PyObject, right: *mut PyObject) -> Result<bool, String> {
	match unsafe { object_equal_structural(left, right) } {
		Some(result) => result,
		None => unsafe { dispatch_user_eq(left, right) },
	}
}

unsafe fn bytes_payload_slice<'a>(object: *mut PyObject) -> Option<&'a [u8]> {
	let object = unsafe { crate::types::type_::payload_subclass_value(object) }.unwrap_or(object);
	if object.is_null() || !crate::tag::is_heap(object) {
		return None;
	}
	if unsafe { type_name(object) } != Some("bytes") {
		return None;
	}
	Some(unsafe { (*object.cast::<crate::types::bytes_::PyBytes>()).as_slice() })
}

/// Structural-only equality: `None` means the pair needs Python-level
/// `__eq__` dispatch, which the caller must run WITHOUT holding a storage
/// borrow or the runtime lock.
unsafe fn object_equal_structural(
	left: *mut PyObject,
	right: *mut PyObject,
) -> Option<Result<bool, String>> {
	if left == right {
		return Some(Ok(true));
	}
	if left.is_null() || right.is_null() {
		return Some(Ok(false));
	}

	// The user-override probe precedes the numeric arms: `UserEq(1) == 1`
	// must consult the override (CPython dispatches the reflected `__eq__`),
	// never the numeric-domain fast path.
	if unsafe { !user_dunder(left, "__eq__").is_null() || !user_dunder(right, "__eq__").is_null() } {
		return None;
	}

	if let Some(equal) = unsafe { numeric_object_equal(left, right) } {
		return Some(Ok(equal));
	}

	// Weak references compare through live referents (CPython: both live ->
	// referent equality; either dead -> identity, handled above). Kept after
	// the numeric arm so immediate-friendly checks stay first in line.
	if unsafe { crate::types::weakref::is_weakref(left) && crate::types::weakref::is_weakref(right) }
	{
		let left_referent = unsafe { crate::types::weakref::weakref_target(left) };
		let right_referent = unsafe { crate::types::weakref::weakref_target(right) };
		if left_referent.is_null() || right_referent.is_null() {
			return Some(Ok(false));
		}
		return unsafe { object_equal_structural(left_referent, right_referent) };
	}

	if let (Some(left_text), Some(right_text)) =
		(unsafe { crate::types::type_::unicode_text(left) }, unsafe {
			crate::types::type_::unicode_text(right)
		}) {
		return Some(Ok(left_text.as_bytes() == right_text.as_bytes()));
	}

	if let (Some(left_bytes), Some(right_bytes)) =
		(unsafe { bytes_payload_slice(left) }, unsafe { bytes_payload_slice(right) })
	{
		return Some(Ok(left_bytes == right_bytes));
	}

	// Tuple-storage keying parity across layouts: an exact tuple and a
	// tuple-subclass instance (namedtuple) with equal contents are the same
	// dict key, matching CPython's inherited `tuple.__eq__`.  The
	// exact/exact pair stays in the name-match arm below.
	if unsafe { crate::types::tuple::is_tuple_subclass_instance(left) }
		|| unsafe { crate::types::tuple::is_tuple_subclass_instance(right) }
	{
		let (Some(l), Some(r)) = (unsafe { crate::abi::seq::tuple_storage_slice(left) }, unsafe {
			crate::abi::seq::tuple_storage_slice(right)
		}) else {
			return Some(Ok(false));
		};
		return unsafe { slices_equal_structural(l, r) };
	}
	// Ranges (either runtime representation) key structurally through the
	// shared sequence-key authority, consistent with the `Some("range")`
	// hash arm below (CPython: `range_equals` + `range_hash` agree).
	{
		let left_key = crate::native::builtins_mod::range_cmp_key(left);
		let right_key = crate::native::builtins_mod::range_cmp_key(right);
		if let (Some(left_key), Some(right_key)) = (left_key, right_key) {
			return Some(Ok(crate::native::builtins_mod::range_keys_equal(&left_key, &right_key)));
		}
	}
	match (unsafe { type_name(left) }, unsafe { type_name(right) }) {
		(Some("str"), Some("str")) => {
			let l = unsafe { &*left.cast::<PyUnicode>() };
			let r = unsafe { &*right.cast::<PyUnicode>() };
			Some(Ok(unsafe { unicode_bytes(l) == unicode_bytes(r) }))
		},
		(Some("bytes"), Some("bytes")) => {
			let l = unsafe { &*left.cast::<crate::types::bytes_::PyBytes>() };
			let r = unsafe { &*right.cast::<crate::types::bytes_::PyBytes>() };
			Some(Ok(unsafe { l.as_slice() == r.as_slice() }))
		},
		(Some("frozenset"), Some("frozenset")) => {
			Some(crate::types::frozenset::frozenset_equal(left, right))
		},
		// `set` pairs (and set/frozenset mixes) compare by contents exactly
		// like `==` (CPython set_richcompare): container membership (`x in
		// (a, b)`) must agree with direct equality.
		(Some("set"), Some("set"))
		| (Some("set"), Some("frozenset"))
		| (Some("frozenset"), Some("set")) => Some(unsafe { crate::types::set_::set_equal(left, right) }),
		(Some("tuple"), Some("tuple")) => {
			let (Some(l), Some(r)) = (unsafe { crate::abi::seq::exact_tuple_slice(left) }, unsafe {
				crate::abi::seq::exact_tuple_slice(right)
			}) else {
				return Some(Ok(false));
			};
			unsafe { slices_equal_structural(l, r) }
		},
		_ => Some(Ok(false)),
	}
}

/// Element-wise structural equality for tuple storage: any element pair
/// needing Python-level dispatch defers the WHOLE comparison (`None`), so the
/// deferred pass re-runs it through the tuple rich-compare protocol with no
/// borrow held.
unsafe fn slices_equal_structural(
	l: &[*mut PyObject],
	r: &[*mut PyObject],
) -> Option<Result<bool, String>> {
	if l.len() != r.len() {
		return Some(Ok(false));
	}
	for (a, b) in l.iter().zip(r.iter()) {
		match unsafe { object_equal_structural(*a, *b) } {
			Some(Ok(true)) => {},
			other => return other,
		}
	}
	Some(Ok(true))
}

/// Runs Python-level equality through the full rich-compare protocol.
/// Error convention: a boxed exception raised by user `__eq__` stays
/// authoritative on the thread state (`pon_err_set` preserves it); the
/// returned message only feeds diagnostics.
unsafe fn dispatch_user_eq(left: *mut PyObject, right: *mut PyObject) -> Result<bool, String> {
	let result =
		unsafe { crate::abstract_op::rich_compare(crate::abstract_op::RICH_EQ, left, right) };
	if result.is_null() {
		return Err("__eq__ raised an exception".to_owned());
	}
	match unsafe { crate::abstract_op::is_true(result) } {
		truth if truth < 0 => Err("__eq__ result truth test raised an exception".to_owned()),
		0 => Ok(false),
		_ => Ok(true),
	}
}

/// Python-level dunder override for `object`, resolved through its heap
/// class's MRO — but ONLY from heap-class namespaces: builtin bases (`dict`,
/// `str`, `object`) never shadow the native fast paths.  NULL when `object`
/// is outside the class-instance family or no user entry exists.  Lock-free
/// and non-forcing (tag bits, type metadata, and namespace tables only), so
/// it is safe on every hash/equality path.
unsafe fn user_dunder(object: *mut PyObject, name: &str) -> *mut PyObject {
	if object.is_null() || !crate::tag::is_heap(object) {
		return core::ptr::null_mut();
	}
	let ty = unsafe { (*object).ob_type }.cast_mut();
	if !crate::types::type_::type_dispatches_python_dunders(ty) {
		return core::ptr::null_mut();
	}
	let name = crate::intern::intern(name);
	for cls in unsafe { crate::mro::mro_entries(ty) } {
		if cls.is_null() || !crate::types::type_::type_dispatches_python_dunders(cls) {
			continue;
		}
		let dict = unsafe { (*cls).tp_dict }.cast::<crate::types::type_::PyClassDict>();
		if dict.is_null() {
			continue;
		}
		if let Some(value) = unsafe { (*dict).get(name) } {
			return value;
		}
	}
	core::ptr::null_mut()
}

/// Calls a user `__hash__` hook: CPython `slot_tp_hash`.  The returned int
/// reduces through int hashing (arbitrary width collapses into the modular
/// hash domain), non-ints raise the CPython TypeError, and the `__hash__ =
/// None` marker (written directly or stamped by class creation for classes
/// defining `__eq__` without `__hash__`) is the unhashable signal.
unsafe fn dispatch_user_hash(object: *mut PyObject, hook: *mut PyObject) -> Result<isize, String> {
	if unsafe { type_name(hook) } == Some("NoneType") {
		let name = unsafe { type_name(object) }.unwrap_or("object");
		return Err(format!("unhashable type: '{name}'"));
	}
	let ty = unsafe { (*object).ob_type }.cast_mut();
	let bound = unsafe { crate::descr::descriptor_get(hook, object, ty) };
	if bound.is_null() {
		return Err("__hash__ descriptor binding raised an exception".to_owned());
	}
	let result = unsafe { crate::abi::pon_call(bound, core::ptr::null_mut(), 0) };
	if result.is_null() {
		return Err("__hash__ raised an exception".to_owned());
	}
	let Some(value) = (unsafe { crate::types::int::to_bigint_including_bool(result) }) else {
		return Err("__hash__ method should return an integer".to_owned());
	};
	// CPython `slot_tp_hash` value rule: a result already within `Py_hash_t`
	// range is preserved EXACTLY (so `x.__hash__() == hash(y)` implies
	// `hash(x) == hash(y)` — returning 42 hashes as 42, NOT as a reduction);
	// only out-of-range ints fall back to long hashing.  `hash_object`
	// normalizes the reserved -1 afterwards.
	Ok(match num_traits::ToPrimitive::to_isize(&value) {
		Some(hash) => hash,
		None => crate::types::int::hash_bigint(&value),
	})
}

unsafe fn numeric_object_equal(left: *mut PyObject, right: *mut PyObject) -> Option<bool> {
	if let Some(left_int) = unsafe { crate::types::int::to_bigint_including_bool(left) } {
		return Some(numeric_int_equal(&left_int, right));
	}
	if let Some(right_int) = unsafe { crate::types::int::to_bigint_including_bool(right) } {
		return Some(numeric_int_equal(&right_int, left));
	}
	if let Some(left_float) = unsafe { crate::types::float::to_f64(left) } {
		return Some(numeric_float_equal(left_float, right));
	}
	if let Some(right_float) = unsafe { crate::types::float::to_f64(right) } {
		return Some(numeric_float_equal(right_float, left));
	}
	let left_complex = unsafe { crate::types::complex_::to_f64s(left)? };
	let right_complex = unsafe { crate::types::complex_::to_f64s(right)? };
	Some(left_complex.0 == right_complex.0 && left_complex.1 == right_complex.1)
}

fn numeric_int_equal(integer: &BigInt, other: *mut PyObject) -> bool {
	if let Some(other) = unsafe { crate::types::int::to_bigint_including_bool(other) } {
		return *integer == other;
	}
	if let Some(other) = unsafe { crate::types::float::to_f64(other) } {
		return float_equals_int(other, integer);
	}
	if let Some((real, imag)) = unsafe { crate::types::complex_::to_f64s(other) } {
		return imag == 0.0 && float_equals_int(real, integer);
	}
	false
}

fn numeric_float_equal(float: f64, other: *mut PyObject) -> bool {
	if let Some(other) = unsafe { crate::types::float::to_f64(other) } {
		return float == other;
	}
	if let Some((real, imag)) = unsafe { crate::types::complex_::to_f64s(other) } {
		return imag == 0.0 && float == real;
	}
	false
}

fn float_equals_int(float: f64, integer: &BigInt) -> bool {
	if !float.is_finite() || float.fract() != 0.0 {
		return false;
	}
	crate::types::int::bigint_from_f64_trunc(float).as_ref() == Some(integer)
}

/// Computes a mapping-compatible hash for hashable Phase-B objects.
pub unsafe fn hash_object(object: *mut PyObject) -> Result<isize, String> {
	if object.is_null() {
		return Err("cannot hash NULL object".to_owned());
	}
	let hash = match unsafe { numeric_hash_object(object) } {
		Some(hash) => hash,
		None => hash_object_non_numeric(object)?,
	};
	Ok(normalize_hash(hash))
}

unsafe fn numeric_hash_object(object: *mut PyObject) -> Option<isize> {
	if let Some(value) = unsafe { crate::types::int::to_bigint_including_bool(object) } {
		return Some(crate::types::int::hash_bigint(&value));
	}
	if let Some(value) = unsafe { crate::types::float::to_f64(object) } {
		return Some(crate::types::float::hash_f64(value));
	}
	unsafe {
		crate::types::complex_::to_f64s(object)
			.map(|(real, imag)| crate::types::complex_::hash_complex(real, imag))
	}
}

/// Structural tuple hash shared by exact tuples and tuple-subclass
/// instances: equal contents collide across both layouts (CPython: tuple
/// subclasses inherit `tuple.__hash__` unless they override it).
fn structural_tuple_hash(items: &[*mut PyObject]) -> Result<isize, String> {
	let mut hash: isize = 0x345678;
	let mut mult: isize = 1_000_003;
	for &item in items {
		let item_hash = unsafe { hash_object(item)? };
		hash = (hash ^ item_hash).wrapping_mul(mult);
		mult = mult.wrapping_add(82_520_isize.wrapping_add(2 * items.len() as isize));
	}
	Ok(hash.wrapping_add(97_531))
}

fn hash_object_non_numeric(object: *mut PyObject) -> Result<isize, String> {
	// A weakref hashes like its live referent, cached across referent death
	// (CPython `wr_hash`: WeakSet discards dead refs by their cached hash).
	if unsafe { crate::types::weakref::is_weakref(object) } {
		return unsafe { crate::types::weakref::weakref_container_hash(object) };
	}
	// Python-level `__hash__` resolved from a heap class's MRO (CPython
	// `slot_tp_hash`): a user hook computes the hash; the `__hash__ = None`
	// marker makes the instance unhashable.  Classes without a user-level
	// entry fall through to the layout arms and the identity default below
	// (CPython `object.__hash__` is id-derived), so plain-object keying is
	// untouched.  The probe is lock-free and non-forcing.
	let hook = unsafe { user_dunder(object, "__hash__") };
	if !hook.is_null() {
		return unsafe { dispatch_user_hash(object, hook) };
	}
	// Dict-layout objects are unhashable exactly like exact dicts (CPython:
	// `dict.__hash__` is None, inherited by subclasses).  Checked before the
	// name match so subclass type names don't fall into the identity default.
	if unsafe { is_dict_subclass_instance(object) } {
		let name = unsafe { type_name(object) }.unwrap_or("dict");
		return Err(format!("unhashable type: '{name}'"));
	}
	// Tuple-layout instances (namedtuple) hash structurally like exact
	// tuples, so both layouts key the same dict slot.
	if unsafe { crate::types::tuple::is_tuple_subclass_instance(object) } {
		if let Some(items) = unsafe { crate::abi::seq::tuple_storage_slice(object) } {
			return structural_tuple_hash(items);
		}
	}
	if let Some(value) = unsafe { crate::types::type_::payload_subclass_value(object) } {
		if matches!(unsafe { type_name(value) }, Some("str" | "bytes")) {
			return unsafe { hash_object(value) };
		}
	}
	let hash = match unsafe { type_name(object) } {
		Some("str") => {
			let unicode = unsafe { &*object.cast::<PyUnicode>() };
			// CPython seed-0 value parity (`pyhash::str_hash`); an invalid
			// UTF-8 payload (never produced by the runtime) defensively
			// hashes its raw bytes.
			match unsafe { unicode.as_str() } {
				Some(text) => crate::pyhash::str_hash(text) as isize,
				None => crate::pyhash::bytes_hash(unsafe { unicode_bytes(unicode) }) as isize,
			}
		},
		Some("bytes") => {
			let bytes = unsafe { &*object.cast::<crate::types::bytes_::PyBytes>() };
			crate::pyhash::bytes_hash(unsafe { bytes.as_slice() }) as isize
		},
		Some("NoneType") => 0x3456_789a_isize,
		Some("frozenset") => unsafe { crate::types::frozenset::frozenset_hash_value(object)? },
		Some("dict") => return Err("unhashable type: 'dict'".to_owned()),
		Some("set") => return Err("unhashable type: 'set'".to_owned()),
		Some("list") => return Err("unhashable type: 'list'".to_owned()),
		Some("bytearray") => return Err("unhashable type: 'bytearray'".to_owned()),
		Some("tuple") => match unsafe { crate::abi::seq::exact_tuple_slice(object) } {
			// Structural tuple hash so equal tuples built at different sites
			// collide; elements recurse through `hash_object`.
			Some(items) => structural_tuple_hash(items)?,
			// Non-PyTuple "tuple" (native representation): prior pointer
			// semantics — identity keying keeps working.
			None => object as usize as isize,
		},
		// Ranges hash by the normalized sequence key so equal ranges (either
		// runtime representation) land in one dict slot; a "range"-named
		// non-range object keeps the identity default.
		Some("range") => match crate::native::builtins_mod::range_hash_value(object) {
			Some(hash) => hash,
			None => object as usize as isize,
		},
		Some(_) => object as usize as isize,
		None => return Err("object has null type".to_owned()),
	};
	Ok(hash)
}
/// Returns a boxed object's type name.
#[must_use]
pub unsafe fn type_name(object: *mut PyObject) -> Option<&'static str> {
	if object.is_null() {
		return None;
	}
	let ty = unsafe { (*object).ob_type };
	if ty.is_null() {
		return None;
	}
	if let Some(native) = twin::registered_native_of_foreign(ty.cast_mut().cast()) {
		return Some(unsafe { static_type_name(native) });
	}
	if let Some(name) = unsafe { native_type_name_if_plausible(ty) } {
		return Some(name);
	}
	Some("object")
}

unsafe fn static_type_name(ty: *const PyType) -> &'static str {
	unsafe { core::mem::transmute::<&str, &'static str>((*ty).name()) }
}

pub(crate) unsafe fn native_type_name_if_plausible(ty: *const PyType) -> Option<&'static str> {
	let type_ref = unsafe { &*ty };
	if type_ref.name.is_null() || type_ref.name_len == 0 || type_ref.name_len > 256 {
		return None;
	}
	Some(unsafe { static_type_name(ty) })
}

/// Storage-shape witness for the deferred-equality probe: any key-structure
/// mutation (insert grows `entries`; removal shrinks it AND swaps in a fresh
/// bucket allocation via `rebuild_dict_buckets_with_capacity`) changes at
/// least one component.  Value overwrites keep every component — they never
/// invalidate probe indices.
fn storage_witness(storage: &PyDictStorage) -> (usize, usize, usize, usize) {
	(
		storage.entries.len(),
		storage.entries.as_ptr() as usize,
		storage.buckets.len(),
		storage.buckets.as_ptr() as usize,
	)
}

/// Locates `key`'s entry index for `hash`.
///
/// Structural candidates resolve inside the collection pass; a candidate
/// needing Python-level `__eq__` is deferred and dispatched with NO storage
/// borrow held (user code may re-enter the dict mutators).  Every
/// affirmative deferred match is re-validated by slot identity, and the
/// probe restarts whenever the storage shape changed underneath a dispatch —
/// CPython `lookdict`'s `goto restart` discipline.
unsafe fn find_entry_index(
	dict: *mut PyObject,
	key: *mut PyObject,
	hash: isize,
) -> Result<Option<usize>, String> {
	'restart: loop {
		let mut deferred: Vec<(usize, *mut PyObject)> = Vec::new();
		let witness;
		{
			let storage = unsafe { dict_ref(dict)? };
			if storage.buckets.is_empty() {
				return Ok(None);
			}
			witness = storage_witness(storage);
			let mut bucket = bucket_index(hash, storage.buckets.len());
			let mut matched = None;
			for _ in 0..storage.buckets.len() {
				let Some(index) = storage.buckets[bucket] else {
					break;
				};
				let entry = storage.entries[index];
				if entry.hash == hash {
					match unsafe { object_equal_structural(entry.key, key) } {
						Some(Ok(true)) => {
							matched = Some(index);
							break;
						},
						Some(Ok(false)) => {},
						Some(Err(message)) => return Err(message),
						None => deferred.push((index, entry.key)),
					}
				}
				bucket = (bucket + 1) & (storage.buckets.len() - 1);
			}
			if let Some(index) = matched {
				return Ok(Some(index));
			}
			if deferred.is_empty() {
				return Ok(None);
			}
		}
		// Deferred pass: Python-level `__eq__` runs with the borrow released.
		let _deferred_guard = crate::abi::scoped_roots(&deferred as *const _);
		for &(index, entry_key) in deferred.iter() {
			let equal = unsafe { dispatch_user_eq(entry_key, key)? };
			let storage = unsafe { dict_ref(dict)? };
			if equal {
				if storage.entries.len() > index && storage.entries[index].key == entry_key {
					return Ok(Some(index));
				}
				continue 'restart;
			}
			if storage_witness(storage) != witness {
				continue 'restart;
			}
		}
		return Ok(None);
	}
}

fn ensure_dict_buckets(dict: &mut PyDictStorage) -> Result<(), String> {
	if dict.buckets.len() < bucket_capacity(dict.entries.len()) {
		rebuild_dict_buckets(dict)?;
	}
	Ok(())
}

fn ensure_dict_insert_capacity(dict: &mut PyDictStorage) -> Result<(), String> {
	let occupied_after_insert = dict.entries.len().saturating_add(1);
	if occupied_after_insert.saturating_mul(3) >= dict.buckets.len().saturating_mul(2) {
		rebuild_dict_buckets_with_capacity(dict, bucket_capacity(occupied_after_insert))?;
	}
	Ok(())
}

fn rebuild_dict_buckets(dict: &mut PyDictStorage) -> Result<(), String> {
	rebuild_dict_buckets_with_capacity(dict, bucket_capacity(dict.entries.len()))
}

fn rebuild_dict_buckets_with_capacity(
	dict: &mut PyDictStorage,
	capacity: usize,
) -> Result<(), String> {
	let mut buckets = vec![None; capacity];
	for index in 0..dict.entries.len() {
		insert_bucket(&mut buckets, &dict.entries, index)?;
	}
	dict.buckets = buckets;
	Ok(())
}

fn insert_bucket(
	buckets: &mut [Option<usize>],
	entries: &[DictEntry],
	index: usize,
) -> Result<(), String> {
	if buckets.is_empty() {
		return Err("dict bucket table is empty".to_owned());
	}
	let hash = entries[index].hash;
	let mut bucket = bucket_index(hash, buckets.len());
	for _ in 0..buckets.len() {
		if buckets[bucket].is_none() {
			buckets[bucket] = Some(index);
			return Ok(());
		}
		bucket = (bucket + 1) & (buckets.len() - 1);
	}
	Err("dict bucket table is full".to_owned())
}

fn bucket_capacity(len: usize) -> usize {
	len.saturating_mul(2).max(8).next_power_of_two()
}

fn bucket_index(hash: isize, bucket_count: usize) -> usize {
	(hash as usize) & (bucket_count - 1)
}

unsafe extern "C" fn dict_len_slot(object: *mut PyObject) -> isize {
	match unsafe { dict_ref(object) } {
		Ok(dict) => isize::try_from(dict.entries.len()).unwrap_or(isize::MAX),
		Err(_) => -1,
	}
}

unsafe extern "C" fn dict_subscript_slot(
	object: *mut PyObject,
	key: *mut PyObject,
) -> *mut PyObject {
	unsafe { crate::abi::map::pon_dict_get_item(object, key) }
}

unsafe extern "C" fn dict_ass_subscript_slot(
	object: *mut PyObject,
	key: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	if value.is_null() {
		unsafe { crate::abi::map::pon_dict_del_item_status(object, key) }
	} else {
		unsafe { crate::abi::map::pon_dict_set_item_status(object, key, value) }
	}
}

/// `tp_richcmp` for exact dicts: CPython content equality — equal lengths and,
/// for every left entry, an equal-keyed right entry whose value compares equal
/// through the full rich-compare dispatch (user `__eq__` fires). Ordering ops
/// and non-dict operands defer with `NotImplemented` so the dispatcher applies
/// the `==`/`!=` identity fallback or raises the CPython ordering `TypeError`.
unsafe extern "C" fn dict_richcmp_slot(
	left: *mut PyObject,
	right: *mut PyObject,
	op: c_int,
) -> *mut PyObject {
	use crate::abstract_op::{RICH_EQ, RICH_NE};

	if !matches!(u8::try_from(op), Ok(RICH_EQ | RICH_NE))
		|| unsafe { !is_dict(left) || !is_dict(right) }
	{
		return unsafe { crate::abi::pon_not_implemented() };
	}
	match unsafe { dict_equal(left, right) } {
		Ok(equal) => {
			let truth = equal == (op == c_int::from(RICH_EQ));
			unsafe { crate::abi::pon_const_bool(c_int::from(truth)) }
		},
		Err(DictEqualError::Raised) => ptr::null_mut(),
		Err(DictEqualError::Message(message)) => crate::abi::return_null_with_error(message),
	}
}

/// How a dict content comparison failed: a nested rich-compare already raised
/// on the thread state, or a fresh message that still needs raising.
enum DictEqualError {
	Raised,
	Message(String),
}

thread_local! {
	 /// `(left, right)` dict pairs currently being compared on this thread.
	 static DICT_EQUAL_IN_PROGRESS: RefCell<Vec<(usize, usize)>> = const { RefCell::new(Vec::new()) };
}

/// Compares two exact dicts by content: equal length and, for every left
/// entry, an equal-keyed right entry whose value rich-compares equal.
unsafe fn dict_equal(left: *mut PyObject, right: *mut PyObject) -> Result<bool, DictEqualError> {
	if left == right {
		return Ok(true);
	}
	let left_entries = unsafe { dict_entries_snapshot(left) }.map_err(DictEqualError::Message)?;
	let right_len = unsafe { dict_ref(right) }
		.map_err(DictEqualError::Message)?
		.entries
		.len();
	if left_entries.len() != right_len {
		return Ok(false);
	}
	// Cycle guard: a pair already being compared deeper in this thread's stack
	// is presumed equal (Py_ReprEnter-style), so self-referencing dicts
	// terminate instead of recursing forever.
	let pair = (left as usize, right as usize);
	let entered = DICT_EQUAL_IN_PROGRESS.with(|stack| {
		let mut stack = stack.borrow_mut();
		if stack.contains(&pair) {
			false
		} else {
			stack.push(pair);
			true
		}
	});
	if !entered {
		return Ok(true);
	}
	let result = unsafe { dict_entries_equal(&left_entries, right) };
	DICT_EQUAL_IN_PROGRESS.with(|stack| {
		stack.borrow_mut().pop();
	});
	result
}

/// Returns whether every `left` entry has an equal-valued match in `right`.
unsafe fn dict_entries_equal(
	left_entries: &[DictEntry],
	right: *mut PyObject,
) -> Result<bool, DictEqualError> {
	for entry in left_entries {
		let Some(right_value) =
			unsafe { dict_get(right, entry.key) }.map_err(DictEqualError::Message)?
		else {
			return Ok(false);
		};
		// Identity implies equal before dispatch, mirroring CPython's
		// `PyObject_RichCompareBool` (observable with shared NaN values; also
		// the escape hatch that keeps shared-cycle comparisons finite).
		if entry.value == right_value {
			continue;
		}
		let equal = unsafe {
			crate::abstract_op::rich_compare(crate::abstract_op::RICH_EQ, entry.value, right_value)
		};
		if equal.is_null() {
			return Err(DictEqualError::Raised);
		}
		let truth = unsafe { crate::abstract_op::is_true(equal) };
		if truth < 0 {
			return Err(DictEqualError::Raised);
		}
		if truth == 0 {
			return Ok(false);
		}
	}
	Ok(true)
}

unsafe extern "C" fn dict_iter_slot(object: *mut PyObject) -> *mut PyObject {
	unsafe { crate::abi::map::pon_dict_iter_keys(object) }
}

unsafe extern "C" fn dict_iter_identity_slot(iterator: *mut PyObject) -> *mut PyObject {
	iterator
}

unsafe extern "C" fn dict_iter_next_slot(iterator: *mut PyObject) -> *mut PyObject {
	unsafe { crate::abi::map::pon_dict_iter_next(iterator) }
}

unsafe extern "C" fn dict_view_len_slot(object: *mut PyObject) -> isize {
	let Some(view) = (unsafe { dict_view_ref(object) }) else {
		return -1;
	};
	match unsafe { dict_ref(view.dict) } {
		Ok(dict) => isize::try_from(dict.entries.len()).unwrap_or(isize::MAX),
		Err(_) => -1,
	}
}

unsafe extern "C" fn dict_view_contains_slot(object: *mut PyObject, item: *mut PyObject) -> c_int {
	unsafe { crate::abi::map::pon_dict_view_contains_status(object, item) }
}

unsafe extern "C" fn dict_view_iter_slot(object: *mut PyObject) -> *mut PyObject {
	unsafe { crate::abi::map::pon_dict_view_iter(object) }
}

unsafe extern "C" fn dict_view_repr_slot(object: *mut PyObject) -> *mut PyObject {
	unsafe { crate::abi::map::pon_dict_view_repr(object) }
}

unsafe extern "C" fn dict_view_difference_slot(
	left: *mut PyObject,
	right: *mut PyObject,
) -> *mut PyObject {
	unsafe { crate::abi::map::pon_dict_view_difference(left, right) }
}

unsafe extern "C" fn dict_view_intersection_slot(
	left: *mut PyObject,
	right: *mut PyObject,
) -> *mut PyObject {
	unsafe { crate::abi::map::pon_dict_view_intersection(left, right) }
}

unsafe extern "C" fn dict_view_union_slot(
	left: *mut PyObject,
	right: *mut PyObject,
) -> *mut PyObject {
	unsafe { crate::abi::map::pon_dict_view_union(left, right) }
}

unsafe extern "C" fn dict_view_symmetric_difference_slot(
	left: *mut PyObject,
	right: *mut PyObject,
) -> *mut PyObject {
	unsafe { crate::abi::map::pon_dict_view_symmetric_difference(left, right) }
}

unsafe extern "C" fn dict_view_reflected_difference_slot(
	view: *mut PyObject,
	other: *mut PyObject,
) -> *mut PyObject {
	unsafe { crate::abi::map::pon_dict_view_reflected_difference(view, other) }
}

unsafe extern "C" fn dict_view_reflected_intersection_slot(
	view: *mut PyObject,
	other: *mut PyObject,
) -> *mut PyObject {
	unsafe { crate::abi::map::pon_dict_view_reflected_intersection(view, other) }
}

unsafe extern "C" fn dict_view_reflected_union_slot(
	view: *mut PyObject,
	other: *mut PyObject,
) -> *mut PyObject {
	unsafe { crate::abi::map::pon_dict_view_reflected_union(view, other) }
}

unsafe extern "C" fn dict_view_reflected_symmetric_difference_slot(
	view: *mut PyObject,
	other: *mut PyObject,
) -> *mut PyObject {
	unsafe { crate::abi::map::pon_dict_view_reflected_symmetric_difference(view, other) }
}

unsafe extern "C" fn dict_view_richcmp_slot(
	left: *mut PyObject,
	right: *mut PyObject,
	op: c_int,
) -> *mut PyObject {
	unsafe { crate::abi::map::pon_dict_view_richcompare(left, right, op) }
}

unsafe extern "C" fn dict_view_getattro_slot(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(attr) = (unsafe { unicode_attr_name_display(name) }) else {
		pon_err_set("dict view attribute name must be str");
		return ptr::null_mut();
	};
	match attr.as_str() {
		"isdisjoint" if unsafe { is_setlike_dict_view(object) } => unsafe {
			crate::abi::map::pon_dict_view_bound_method(object, &attr)
		},
		_ => {
			crate::abi::exc::raise_attribute_error_text(&format!("attribute '{attr}' was not found"))
		},
	}
}

unsafe extern "C" fn dict_getattro_slot(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(attr) = (unsafe { unicode_attr_name_display(name) }) else {
		pon_err_set("dict attribute name must be str");
		return ptr::null_mut();
	};
	match attr.as_str() {
		"get" | "keys" | "values" | "items" | "setdefault" | "pop" | "popitem" | "clear"
		| "update" | "copy" => unsafe { crate::abi::map::pon_dict_bound_method(object, &attr) },
		// `fromkeys` is a classmethod in CPython: the receiver only supplies the
		// class, so the plain (unbound) constructor function is the right value.
		"fromkeys" => crate::native::builtins_mod::dict_fromkeys_function(),
		_ => {
			// Fall back to the type's `tp_dict` natives (`__len__`,
			// `__contains__`, ...) through the descriptor protocol so
			// bound-dunder reads like functools' `cache.__len__` work on
			// exact dicts.
			ensure_dict_subclass_methods_installed();
			let ty = unsafe { (*object).ob_type.cast_mut() };
			let hook = unsafe { crate::descr::lookup_in_type(ty, crate::intern::intern(&attr)) };
			if !hook.is_null() {
				return unsafe { crate::descr::descriptor_get(hook, object, ty) };
			}
			crate::abi::exc::raise_attribute_error_text(&format!("attribute '{attr}' was not found"))
		},
	}
}

unsafe fn unicode_attr_name_display(name: *mut PyObject) -> Option<String> {
	if unsafe { type_name(name) } != Some("str") {
		return None;
	}
	let unicode = unsafe { &*name.cast::<PyUnicode>() };
	Some(String::from_utf8_lossy(unsafe { unicode_bytes(unicode) }).into_owned())
}

unsafe fn unicode_bytes(unicode: &PyUnicode) -> &[u8] {
	if unicode.data.is_null() && unicode.len != 0 {
		&[]
	} else {
		unsafe { core::slice::from_raw_parts(unicode.data, unicode.len) }
	}
}

fn normalize_hash(hash: isize) -> isize {
	if hash == -1 { -2 } else { hash }
}

unsafe fn install_type_type(ty: *mut PyType, type_type: *const PyType) {
	if !ty.is_null() && !type_type.is_null() {
		unsafe { (*ty).ob_base.ob_type = type_type };
	}
}

// ---- dict-subclass method surface ------------------------------------------
//
// Unbound native methods installed into the builtin dict type's `tp_dict` so
// MRO-based dispatch (`generic_get_attr`, `super_lookup`, the rich-compare
// dunder fallback, and the `__len__`/`__iter__`/`__contains__`/item-protocol
// fallbacks in the abstract ops) serves native dict behavior to heap
// subclasses.  Plain dict receivers never take these paths: their
// `tp_getattro` remains the closed native table and their protocol slots
// dispatch directly.

/// Validates argv shape for a dict dunder: `arity` counts the receiver.
unsafe fn dunder_args<'a>(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
	arity: RangeInclusive<usize>,
) -> Result<&'a [*mut PyObject], String> {
	if argv.is_null() && argc != 0 {
		return Err(format!("dict.{name} received a null argv pointer"));
	}
	if argc == 0 {
		// CPython zero-arg wording for a dict descriptor reached unbound.
		return Err(format!("descriptor '{name}' of 'dict' object needs an argument"));
	}
	if !arity.contains(&argc) {
		// CPython slot-wrapper arity wording: counts exclude the receiver.
		let expected = *arity.end() - 1;
		let got = argc - 1;
		let plural = if expected == 1 { "" } else { "s" };
		return Err(format!("{name} expected {expected} argument{plural}, got {got}"));
	}
	Ok(if argc == 0 {
		&[]
	} else {
		unsafe { core::slice::from_raw_parts(argv.cast_const(), argc) }
	})
}

/// Boxed CPython-parity TypeError for dict native methods.
fn raise_dict_type_error(message: String) -> *mut PyObject {
	unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

/// CPython self-check wording flavor: slot wrappers (`__setitem__`) say
/// "requires a 'dict' object but received a 'list'", method descriptors
/// (`get`, `__contains__`) say "for 'dict' objects doesn't apply to a
/// 'list' object".
#[derive(Clone, Copy)]
enum DictDescrFlavor {
	SlotWrapper,
	MethodDescriptor,
}

/// Unbound-receiver validation for dict natives reached off the type
/// (`dict.__setitem__(x, …)`): raises the CPython-parity TypeError and
/// reports false when `receiver` (already untagged) has no dict layout.
unsafe fn ensure_dict_receiver(
	receiver: *mut PyObject,
	name: &str,
	flavor: DictDescrFlavor,
) -> bool {
	if unsafe { has_dict_storage(receiver) } {
		return true;
	}
	let ty = unsafe { type_name(receiver) }.unwrap_or("object");
	let message = match flavor {
		DictDescrFlavor::SlotWrapper => {
			format!("descriptor '{name}' requires a 'dict' object but received a '{ty}'")
		},
		DictDescrFlavor::MethodDescriptor => {
			format!("descriptor '{name}' for 'dict' objects doesn't apply to a '{ty}' object")
		},
	};
	let _ = raise_dict_type_error(message);
	false
}

/// `dict.__init__(self, source=None)`: CPython dict-update semantics.
unsafe extern "C" fn dict_dunder_init(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { dunder_args(argv, argc, "__init__", 1..=2) } {
		Ok(args) => args,
		Err(message) => return raise_dict_type_error(message),
	};
	let receiver = args[0];
	crate::untag_prelude!(receiver);
	if unsafe { !ensure_dict_receiver(receiver, "__init__", DictDescrFlavor::SlotWrapper) } {
		return ptr::null_mut();
	}
	if let Some(&source) = args.get(1) {
		let source = crate::tag::untag_arg(source);
		let mut pairs = Vec::new();
		if unsafe { crate::native::builtins_mod::collect_dict_update_pairs(source, &mut pairs) }
			.is_err()
		{
			return ptr::null_mut();
		}
		for pair in pairs.chunks_exact(2) {
			if unsafe { crate::abi::map::pon_dict_set_item_status(receiver, pair[0], pair[1]) } < 0 {
				return ptr::null_mut();
			}
		}
	}
	unsafe { crate::abi::pon_none() }
}

/// `dict.__getitem__(self, key)`: raises `KeyError` on a miss.
unsafe extern "C" fn dict_dunder_getitem(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { dunder_args(argv, argc, "__getitem__", 2..=2) } {
		Ok(args) => args,
		Err(message) => return raise_dict_type_error(message),
	};
	let receiver = args[0];
	crate::untag_prelude!(receiver);
	if unsafe { !ensure_dict_receiver(receiver, "__getitem__", DictDescrFlavor::MethodDescriptor) } {
		return ptr::null_mut();
	}
	unsafe { crate::abi::map::pon_dict_get_item(receiver, args[1]) }
}

/// `dict.__setitem__(self, key, value)`.
unsafe extern "C" fn dict_dunder_setitem(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { dunder_args(argv, argc, "__setitem__", 3..=3) } {
		Ok(args) => args,
		Err(message) => return raise_dict_type_error(message),
	};
	let receiver = args[0];
	crate::untag_prelude!(receiver);
	if unsafe { !ensure_dict_receiver(receiver, "__setitem__", DictDescrFlavor::SlotWrapper) } {
		return ptr::null_mut();
	}
	if unsafe { crate::abi::map::pon_dict_set_item_status(receiver, args[1], args[2]) } < 0 {
		return ptr::null_mut();
	}
	unsafe { crate::abi::pon_none() }
}

/// `dict.__delitem__(self, key)`: raises `KeyError` on a miss.
unsafe extern "C" fn dict_dunder_delitem(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { dunder_args(argv, argc, "__delitem__", 2..=2) } {
		Ok(args) => args,
		Err(message) => return raise_dict_type_error(message),
	};
	let receiver = args[0];
	crate::untag_prelude!(receiver);
	if unsafe { !ensure_dict_receiver(receiver, "__delitem__", DictDescrFlavor::SlotWrapper) } {
		return ptr::null_mut();
	}
	if unsafe { crate::abi::map::pon_dict_del_item_status(receiver, args[1]) } < 0 {
		return ptr::null_mut();
	}
	unsafe { crate::abi::pon_none() }
}

/// `dict.__contains__(self, key)`.
unsafe extern "C" fn dict_dunder_contains(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { dunder_args(argv, argc, "__contains__", 2..=2) } {
		Ok(args) => args,
		Err(message) => return raise_dict_type_error(message),
	};
	let receiver = args[0];
	let key = args[1];
	crate::untag_prelude!(receiver, key);
	if unsafe { !ensure_dict_receiver(receiver, "__contains__", DictDescrFlavor::MethodDescriptor) }
	{
		return ptr::null_mut();
	}
	let _guard = crate::sync::begin_critical_section(receiver);
	match unsafe { dict_contains(receiver, key) } {
		Ok(found) => unsafe { crate::abi::pon_const_bool(c_int::from(found)) },
		Err(message) => crate::abi::return_null_with_error(message),
	}
}

/// `dict.__len__(self)`.
unsafe extern "C" fn dict_dunder_len(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { dunder_args(argv, argc, "__len__", 1..=1) } {
		Ok(args) => args,
		Err(message) => return raise_dict_type_error(message),
	};
	let receiver = args[0];
	crate::untag_prelude!(receiver);
	if unsafe { !ensure_dict_receiver(receiver, "__len__", DictDescrFlavor::SlotWrapper) } {
		return ptr::null_mut();
	}
	match unsafe { dict_ref(receiver) } {
		Ok(storage) => unsafe { crate::abi::pon_const_int(storage.entries.len() as i64) },
		Err(message) => crate::abi::return_null_with_error(message),
	}
}

/// `dict.__iter__(self)`: insertion-order key iterator.
unsafe extern "C" fn dict_dunder_iter(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { dunder_args(argv, argc, "__iter__", 1..=1) } {
		Ok(args) => args,
		Err(message) => return raise_dict_type_error(message),
	};
	let receiver = args[0];
	crate::untag_prelude!(receiver);
	if unsafe { !ensure_dict_receiver(receiver, "__iter__", DictDescrFlavor::SlotWrapper) } {
		return ptr::null_mut();
	}
	unsafe { crate::abi::map::pon_dict_iter_keys(receiver) }
}

/// Shared `__eq__`/`__ne__` body: content equality against any dict-layout
/// operand, `NotImplemented` otherwise (the dispatcher then applies identity
/// or the reflected operation, mirroring `dict_richcmp_slot`).
unsafe fn dict_dunder_compare(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
	want_equal: bool,
) -> *mut PyObject {
	let args = match unsafe { dunder_args(argv, argc, name, 2..=2) } {
		Ok(args) => args,
		Err(message) => return raise_dict_type_error(message),
	};
	let receiver = args[0];
	let other = args[1];
	crate::untag_prelude!(receiver, other);
	if unsafe { !ensure_dict_receiver(receiver, name, DictDescrFlavor::SlotWrapper) } {
		return ptr::null_mut();
	}
	if unsafe { !has_dict_storage(other) } {
		return unsafe { crate::abi::pon_not_implemented() };
	}
	match unsafe { dict_equal(receiver, other) } {
		Ok(equal) => unsafe { crate::abi::pon_const_bool(c_int::from(equal == want_equal)) },
		Err(DictEqualError::Raised) => ptr::null_mut(),
		Err(DictEqualError::Message(message)) => crate::abi::return_null_with_error(message),
	}
}

/// `dict.__eq__(self, other)`.
unsafe extern "C" fn dict_dunder_eq(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { dict_dunder_compare(argv, argc, "__eq__", true) }
}

/// `dict.__ne__(self, other)`.
unsafe extern "C" fn dict_dunder_ne(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { dict_dunder_compare(argv, argc, "__ne__", false) }
}

// ---- PEP 584 dict union
// ------------------------------------------------------

/// PEP 584 `|` core: a NEW exact dict holding `left`'s entries updated by
/// `right`'s — insertion order is left-then-right, right wins on key
/// conflicts, and the result is a plain dict even for subclass operands
/// (CPython `dict_or` via `PyDict_Copy`).  Reports `NotImplemented` unless
/// BOTH operands carry concrete dict storage so the dispatcher can continue
/// to reflected candidates and the operand-typed TypeError.
unsafe fn dict_union(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
	if unsafe { !has_dict_storage(left) || !has_dict_storage(right) } {
		return unsafe { crate::abi::pon_not_implemented() };
	}
	let mut pairs = Vec::new();
	for source in [left, right] {
		let entries = match unsafe { dict_entries_snapshot(source) } {
			Ok(entries) => entries,
			Err(message) => return crate::abi::return_null_with_error(message),
		};
		pairs.reserve(entries.len() * 2);
		for entry in entries {
			pairs.push(entry.key);
			pairs.push(entry.value);
		}
	}
	unsafe { crate::abi::map::pon_build_map(pairs.as_mut_ptr(), pairs.len() / 2) }
}

/// PEP 584 `|=` and `dict.update` core: update semantics over any mapping or
/// iterable of key-value pairs, mutating `receiver` and returning the SAME
/// object (NULL with the update error raised on failure — `d |= 5` reports
/// CPython's "'int' object is not iterable", not an unsupported-operand
/// TypeError).  No outer critical section: pair collection can re-enter
/// Python (`keys()`, `__getitem__`), so inserts lock per item.
pub(crate) unsafe fn dict_inplace_union(
	receiver: *mut PyObject,
	other: *mut PyObject,
) -> *mut PyObject {
	let mut pairs = Vec::new();
	if unsafe { crate::native::builtins_mod::collect_dict_update_pairs(other, &mut pairs) }.is_err()
	{
		return ptr::null_mut();
	}
	for pair in pairs.chunks_exact(2) {
		if unsafe { crate::abi::map::pon_dict_set_item_status(receiver, pair[0], pair[1]) } < 0 {
			return ptr::null_mut();
		}
	}
	receiver
}

/// `nb_or` slot: `self | other`.
unsafe extern "C" fn dict_or_slot(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
	unsafe { dict_union(left, right) }
}

/// `nb_reflected_or` slot: the receiver is the RIGHT operand of `other | self`,
/// so the union runs other-first with the receiver's entries winning conflicts.
unsafe extern "C" fn dict_ror_slot(receiver: *mut PyObject, other: *mut PyObject) -> *mut PyObject {
	unsafe { dict_union(other, receiver) }
}

/// `nb_inplace_or` slot: `self |= other`.
unsafe extern "C" fn dict_ior_slot(receiver: *mut PyObject, other: *mut PyObject) -> *mut PyObject {
	if unsafe { !has_dict_storage(receiver) } {
		return unsafe { crate::abi::pon_not_implemented() };
	}
	unsafe { dict_inplace_union(receiver, other) }
}

/// Shared `__or__`/`__ror__` body for the subclass method surface.
unsafe fn dict_dunder_union(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
	reflected: bool,
) -> *mut PyObject {
	let args = match unsafe { dunder_args(argv, argc, name, 2..=2) } {
		Ok(args) => args,
		Err(message) => return raise_dict_type_error(message),
	};
	let receiver = args[0];
	let other = args[1];
	crate::untag_prelude!(receiver, other);
	if unsafe { !ensure_dict_receiver(receiver, name, DictDescrFlavor::SlotWrapper) } {
		return ptr::null_mut();
	}
	if reflected {
		unsafe { dict_union(other, receiver) }
	} else {
		unsafe { dict_union(receiver, other) }
	}
}

/// `dict.__or__(self, other)`.
unsafe extern "C" fn dict_dunder_or(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { dict_dunder_union(argv, argc, "__or__", false) }
}

/// `dict.__ror__(self, other)`.
unsafe extern "C" fn dict_dunder_ror(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { dict_dunder_union(argv, argc, "__ror__", true) }
}

/// `dict.__ior__(self, other)`: in-place update, returning the receiver.
unsafe extern "C" fn dict_dunder_ior(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { dunder_args(argv, argc, "__ior__", 2..=2) } {
		Ok(args) => args,
		Err(message) => return raise_dict_type_error(message),
	};
	let receiver = args[0];
	let other = args[1];
	crate::untag_prelude!(receiver, other);
	if unsafe { !ensure_dict_receiver(receiver, "__ior__", DictDescrFlavor::SlotWrapper) } {
		return ptr::null_mut();
	}
	unsafe { dict_inplace_union(receiver, other) }
}

/// One-shot installer for the builtin dict type's `tp_dict` method surface.
///
/// Runs lazily from class construction the first time a dict-derived class is
/// built (always outside the runtime lock).  The namespace values are rooted
/// through `register_namespaced_type`, matching how class namespaces keep
/// their GC values alive.
pub fn ensure_dict_subclass_methods_installed() {
	static INSTALLED: AtomicBool = AtomicBool::new(false);
	if INSTALLED.load(Ordering::SeqCst) {
		return;
	}
	// Pre-runtime call sites must not latch a no-op install: the function
	// allocations below need a live runtime, and `runtime_type_type` reports
	// NULL until the runtime is initialized.
	let type_type = crate::abi::runtime_type_type();
	if type_type.is_null() {
		return;
	}
	let ty = dict_type(type_type);
	if ty.is_null() {
		return;
	}
	if INSTALLED.swap(true, Ordering::SeqCst) {
		return;
	}
	let namespace = crate::types::type_::new_namespace();
	let natives: &[(&str, *const u8)] = &[
		("__init__", dict_dunder_init as *const u8),
		("__getitem__", dict_dunder_getitem as *const u8),
		("__setitem__", dict_dunder_setitem as *const u8),
		("__delitem__", dict_dunder_delitem as *const u8),
		("__contains__", dict_dunder_contains as *const u8),
		("__len__", dict_dunder_len as *const u8),
		("__iter__", dict_dunder_iter as *const u8),
		("__eq__", dict_dunder_eq as *const u8),
		("__ne__", dict_dunder_ne as *const u8),
		("__or__", dict_dunder_or as *const u8),
		("__ror__", dict_dunder_ror as *const u8),
		("__ior__", dict_dunder_ior as *const u8),
		("get", crate::abi::map::dict_get_method_trampoline as *const u8),
		("keys", crate::abi::map::dict_keys_method_trampoline as *const u8),
		("values", crate::abi::map::dict_values_method_trampoline as *const u8),
		("items", crate::abi::map::dict_items_method_trampoline as *const u8),
		("setdefault", crate::abi::map::dict_setdefault_method_trampoline as *const u8),
		("pop", crate::abi::map::dict_pop_method_trampoline as *const u8),
		("update", crate::abi::map::dict_update_method_trampoline as *const u8),
		("copy", crate::abi::map::dict_copy_method_trampoline as *const u8),
		("popitem", crate::abi::map::dict_popitem_method_trampoline as *const u8),
		("clear", crate::abi::map::dict_clear_method_trampoline as *const u8),
	];
	for (name, code) in natives {
		let interned = crate::intern::intern(name);
		let function = unsafe {
			crate::abi::pon_make_function(*code, crate::builtins::variadic_arity(), interned)
		};
		if !function.is_null() {
			crate::types::function::mark_native_method_descriptor(function);
			unsafe { (&mut *namespace).set(interned, function) };
		}
	}
	unsafe {
		(*ty).tp_dict = namespace.cast::<PyObject>();
	}
	// GC rooting for the namespace values plus IC invalidation for any
	// AttrIC guarding a type whose MRO now resolves differently.
	crate::sync::register_namespaced_type(ty);
	crate::sync::type_modified(ty);
}

/// Traces GC references of a dict-subclass instance: the heap-instance prefix
/// (instance dict values, slots) plus the embedded dict storage.
pub unsafe extern "C" fn trace_dict_subclass_instance(
	object: *mut u8,
	visitor: &mut dyn FnMut(*mut u8),
) {
	if object.is_null() {
		return;
	}
	unsafe { crate::types::weakref::trace_heap_instance(object, visitor) };
	let storage = unsafe { &(*object.cast::<PyDictSubclassInstance>()).storage };
	for entry in &storage.entries {
		if !entry.key.is_null() {
			visitor(entry.key.cast::<u8>());
		}
		if !entry.value.is_null() {
			visitor(entry.value.cast::<u8>());
		}
	}
}

/// Finalizes a dict-subclass instance: heap-instance semantics (`__del__`,
/// weakrefs, instance dict, slots) plus the embedded dict storage vectors.
pub unsafe extern "C" fn finalize_dict_subclass_instance(object: *mut u8) {
	if object.is_null() {
		return;
	}
	unsafe { crate::types::weakref::finalize_heap_instance(object) };
	let storage = unsafe { &mut (*object.cast::<PyDictSubclassInstance>()).storage };
	unsafe {
		ptr::drop_in_place(ptr::addr_of_mut!(storage.entries));
		ptr::drop_in_place(ptr::addr_of_mut!(storage.buckets));
	}
}

const _: () = {
	assert!(offset_of!(PyDict, ob_base) == 0);
	assert!(offset_of!(PyDictIter, ob_base) == 0);
	assert!(offset_of!(PyDictView, ob_base) == 0);
	assert!(offset_of!(PyDictStorage, entries) == 0);
	assert!(
		offset_of!(PyDict, buckets) - offset_of!(PyDict, entries)
			== offset_of!(PyDictStorage, buckets)
	);
	assert!(size_of::<PyDict>() == offset_of!(PyDict, entries) + size_of::<PyDictStorage>());
	// The heap-instance prefix cast contract for dict-subclass instances.
	assert!(offset_of!(PyDictSubclassInstance, base) == 0);
};

#[cfg(test)]
mod tests {
	use num_bigint::BigInt;
	use num_traits::One;

	use super::*;
	use crate::{
		abi::{
			map::pon_build_map, pon_const_int, pon_const_str, pon_runtime_init, seq::pon_build_list,
		},
		object::PyObject,
		thread_state::test_state_lock,
		types::{bool_ as bool_type, complex_ as complex_type, float as float_type, int as int_type},
	};

	#[track_caller]
	fn assert_same_hash_and_equal(left: *mut PyObject, right: *mut PyObject) {
		unsafe {
			assert_eq!(hash_object(left).expect("left hash"), hash_object(right).expect("right hash"));
			assert!(object_equal(left, right).expect("left equals right"));
			assert!(object_equal(right, left).expect("right equals left"));
		}
	}

	#[track_caller]
	fn str_object(text: &str) -> *mut PyObject {
		let object = unsafe { pon_const_str(text.as_ptr(), text.len()) };
		assert!(!object.is_null(), "failed to allocate test str {text:?}");
		object
	}

	#[test]
	fn dict_numeric_one_hashes_and_compares_equal_across_bool_int_float_complex() {
		let _guard = test_state_lock();
		unsafe {
			assert_eq!(pon_runtime_init(), 0);
		}

		let one = int_type::from_i64(1);
		let true_ = bool_type::from_bool(true);
		let one_float = float_type::from_f64(1.0);
		let one_complex = complex_type::from_f64s(1.0, 0.0);

		assert_same_hash_and_equal(one, true_);
		assert_same_hash_and_equal(one, one_float);
		assert_same_hash_and_equal(one, one_complex);
	}

	#[test]
	fn dict_numeric_equal_one_keys_lookup_and_update_the_same_entry() {
		let _guard = test_state_lock();
		unsafe {
			assert_eq!(pon_runtime_init(), 0);

			let one = int_type::from_i64(1);
			let true_ = bool_type::from_bool(true);
			let one_float = float_type::from_f64(1.0);
			let one_complex = complex_type::from_f64s(1.0, 0.0);
			let original_value = pon_const_int(41);
			let replacement_value = pon_const_int(42);
			let mut pairs = [one, original_value];
			let dict = pon_build_map(pairs.as_mut_ptr(), 1);

			assert!(!dict.is_null());
			assert_eq!(dict_get(dict, true_).expect("lookup by True"), Some(original_value));
			assert_eq!(dict_get(dict, one_float).expect("lookup by 1.0"), Some(original_value));
			assert_eq!(dict_get(dict, one_complex).expect("lookup by 1+0j"), Some(original_value));

			dict_insert(dict, true_, replacement_value).expect("update by equal bool key");

			assert_eq!(dict_ref(dict).expect("dict ref").entries.len(), 1);
			assert_eq!(
				dict_get(dict, one).expect("lookup by int after update"),
				Some(replacement_value)
			);
			assert_eq!(
				dict_get(dict, one_float).expect("lookup by float after update"),
				Some(replacement_value)
			);
			assert_eq!(
				dict_get(dict, one_complex).expect("lookup by complex after update"),
				Some(replacement_value)
			);
		}
	}

	#[test]
	fn dict_numeric_bigint_spill_key_remains_findable_by_fresh_equal_bigint() {
		let _guard = test_state_lock();
		unsafe {
			assert_eq!(pon_runtime_init(), 0);
		}

		let value = BigInt::one() << 100_usize;
		let inserted_key = int_type::from_bigint(value.clone());
		let lookup_key = int_type::from_bigint(value);
		assert_ne!(inserted_key, lookup_key);
		assert_same_hash_and_equal(inserted_key, lookup_key);

		unsafe {
			let stored_value = pon_const_int(100);
			let mut pairs = [inserted_key, stored_value];
			let dict = pon_build_map(pairs.as_mut_ptr(), 1);

			assert!(!dict.is_null());
			assert_eq!(
				dict_get(dict, lookup_key).expect("lookup by fresh BigInt"),
				Some(stored_value)
			);
			assert_eq!(dict_ref(dict).expect("dict ref").entries.len(), 1);
		}
	}

	#[test]
	fn dict_prehash_collect_dedups_duplicate_keys_and_fill_round_trips() {
		let _guard = test_state_lock();
		unsafe {
			assert_eq!(pon_runtime_init(), 0);

			let key_a_first = str_object("a");
			let key_b = str_object("b");
			let key_a_dup = str_object("a");
			assert_ne!(key_a_first, key_a_dup, "duplicate key must be a distinct object");
			let value_1 = pon_const_int(1);
			let value_2 = pon_const_int(2);
			let value_3 = pon_const_int(3);

			let mut entries: Vec<DictEntry> = Vec::new();
			collect_prehashed_entry(&mut entries, key_a_first, value_1).expect("collect (a, 1)");
			collect_prehashed_entry(&mut entries, key_b, value_2).expect("collect (b, 2)");
			collect_prehashed_entry(&mut entries, key_a_dup, value_3)
				.expect("collect duplicate (a, 3)");

			assert_eq!(entries.len(), 2);
			assert_eq!(entries[0].key, key_a_first, "duplicate key must keep the FIRST key object");
			assert_eq!(entries[0].value, value_3, "duplicate key must take the LAST value");
			assert_eq!(entries[1].key, key_b);
			assert_eq!(entries[1].value, value_2);

			let dict = pon_build_map(core::ptr::null_mut(), 0);
			assert!(!dict.is_null());
			dict_fill_prehashed(dict, &entries).expect("fill prehashed entries");

			assert_eq!(dict_ref(dict).expect("dict ref").entries.len(), 2);
			assert_eq!(dict_get(dict, str_object("a")).expect("lookup by fresh 'a'"), Some(value_3));
			assert_eq!(dict_get(dict, str_object("b")).expect("lookup by fresh 'b'"), Some(value_2));
			assert_eq!(dict_get(dict, str_object("c")).expect("lookup by absent 'c'"), None);
		}
	}

	#[test]
	fn dict_prehash_collect_rejects_unhashable_key_with_wrapped_message() {
		let _guard = test_state_lock();
		unsafe {
			assert_eq!(pon_runtime_init(), 0);

			let list_key = pon_build_list(core::ptr::null_mut(), 0);
			assert!(!list_key.is_null());
			let value = pon_const_int(1);

			let mut entries: Vec<DictEntry> = Vec::new();
			let message = collect_prehashed_entry(&mut entries, list_key, value)
				.expect_err("list keys are unhashable");

			assert!(message.starts_with("cannot use '"), "got: {message}");
			assert!(message.contains("unhashable type"), "got: {message}");
			assert!(message.contains("as a dict key"), "got: {message}");
			assert!(entries.is_empty(), "failed collect must not leave partial entries");
		}
	}
}
