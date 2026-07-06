//! Tuple sequence implementation.

use core::mem::offset_of;

use crate::object::{PyObject, PyObjectHeader};

/// Boxed Python `tuple` with immutable Rust-owned pointer storage.
#[repr(C)]
#[derive(Debug)]
pub struct PyTuple {
	/// Common object header; this field must remain first.
	pub ob_base: PyObjectHeader,
	/// Number of elements.
	pub len:     usize,
	/// Pointer to `len` boxed-object slots, or NULL when empty.
	pub items:   *mut *mut PyObject,
}

impl PyTuple {
	/// Returns tuple elements.
	///
	/// # Safety
	///
	/// `self.items` must either be NULL with `self.len == 0`, or point to at
	/// least `self.len` initialized slots.
	#[must_use]
	pub unsafe fn as_slice(&self) -> &[*mut PyObject] {
		if self.len == 0 {
			&[]
		} else {
			debug_assert!(
				!self.items.is_null() && self.items.is_aligned(),
				"PyTuple::as_slice: items pointer {:p} (len {}) is null or misaligned — a tagged or \
				 uninitialized tuple reached iteration",
				self.items,
				self.len,
			);
			unsafe { core::slice::from_raw_parts(self.items, self.len) }
		}
	}
}

/// Traces every object reference contained in a tuple.
///
/// # Safety
///
/// `object` must be NULL or point to a live [`PyTuple`] allocation.
pub unsafe extern "C" fn trace_tuple(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
	if object.is_null() {
		return;
	}
	let tuple = unsafe { &*object.cast::<PyTuple>() };
	for child in unsafe { tuple.as_slice() }.iter().copied() {
		if !child.is_null() {
			visitor(child.cast::<u8>());
		}
	}
}

/// Drops the Rust-owned tuple element vector.
///
/// # Safety
///
/// `object` must be NULL or point to a live [`PyTuple`] allocation whose
/// `items` pointer was produced by this module's vector-leak convention.
pub unsafe extern "C" fn finalize_tuple(object: *mut u8) {
	if object.is_null() {
		return;
	}
	let tuple = unsafe { &mut *object.cast::<PyTuple>() };
	if !tuple.items.is_null() && tuple.len != 0 {
		unsafe { drop(Vec::from_raw_parts(tuple.items, tuple.len, tuple.len)) };
		tuple.items = core::ptr::null_mut();
		tuple.len = 0;
	}
}

/// Contiguous tuple cell block: the field pair shared by the exact
/// [`PyTuple`] tail and tuple-subclass instances.  Raw tuple helpers operate
/// on this view so both layouts share one implementation.
#[repr(C)]
#[derive(Debug)]
pub struct PyTupleStorage {
	/// Number of elements.
	pub len:   usize,
	/// Pointer to `len` boxed-object slots, or NULL when empty.
	pub items: *mut *mut PyObject,
}

impl PyTupleStorage {
	/// Returns the elements as an immutable slice.
	///
	/// # Safety
	///
	/// `self.items` must either be NULL with `self.len == 0`, or point to at
	/// least `self.len` initialized slots.
	#[must_use]
	pub unsafe fn as_slice(&self) -> &[*mut PyObject] {
		if self.len == 0 {
			&[]
		} else {
			unsafe { core::slice::from_raw_parts(self.items, self.len) }
		}
	}
}

/// Heap-class instance embedding native tuple storage (`class T(tuple)`,
/// `collections.namedtuple`).  Mirrors `PyListSubclassInstance`: the generic
/// heap-instance prefix keeps every instance-attribute, slot, and weakref
/// path working unchanged, while `storage` carries the immutable tuple cells
/// the native protocol reads through (populated once by `tuple.__new__`).
#[derive(Debug)]
pub struct PyTupleSubclassInstance {
	/// Generic heap-instance prefix; must remain first.
	pub base:    crate::types::type_::PyHeapInstance,
	/// Embedded native tuple storage.
	pub storage: PyTupleStorage,
}

/// Returns whether `ty` is a heap class using the tuple-subclass layout: the
/// heap-instance GC id combined with the extended `PyTupleSubclassInstance`
/// size.  Deliberately lock-free so it is safe on every tuple fast path.
#[must_use]
pub unsafe fn type_is_tuple_subclass(ty: *mut crate::object::PyType) -> bool {
	if ty.is_null() {
		return false;
	}
	unsafe {
		(*ty).gc_type_id == crate::types::type_::TYPE_ID_HEAP_INSTANCE.0 as usize
			&& (*ty).tp_basicsize == core::mem::size_of::<PyTupleSubclassInstance>()
	}
}

/// Returns whether `object` is a tuple-subclass heap instance.
#[must_use]
pub unsafe fn is_tuple_subclass_instance(object: *mut PyObject) -> bool {
	if object.is_null() || !crate::tag::is_heap(object) {
		return false;
	}
	unsafe { type_is_tuple_subclass((*object).ob_type.cast_mut()) }
}

/// Returns whether a class built over `bases` embeds native tuple storage:
/// some base linearizes over the builtin `tuple` type.  Mirrors
/// `list::class_bases_embed_list` (non-heap name match).
#[must_use]
pub unsafe fn class_bases_embed_tuple(bases: &[*mut crate::object::PyType]) -> bool {
	bases.iter().copied().any(|base| {
		unsafe { crate::mro::mro_entries(base) }
			.iter()
			.any(|entry| {
				!entry.is_null()
					&& unsafe {
						(**entry).gc_type_id != crate::types::type_::TYPE_ID_HEAP_INSTANCE.0 as usize
							&& (**entry).name() == "tuple"
					}
			})
	})
}

/// Traces GC references of a tuple-subclass instance: the heap-instance
/// prefix (instance dict values, slots) plus the embedded tuple items.
pub unsafe extern "C" fn trace_tuple_subclass_instance(
	object: *mut u8,
	visitor: &mut dyn FnMut(*mut u8),
) {
	if object.is_null() {
		return;
	}
	unsafe { crate::types::weakref::trace_heap_instance(object, visitor) };
	let storage = unsafe { &(*object.cast::<PyTupleSubclassInstance>()).storage };
	for child in unsafe { storage.as_slice() }.iter().copied() {
		if !child.is_null() {
			visitor(child.cast::<u8>());
		}
	}
}

/// Finalizes a tuple-subclass instance: heap-instance semantics (`__del__`,
/// weakrefs, instance dict, slots) plus the leaked tuple backing vector.
pub unsafe extern "C" fn finalize_tuple_subclass_instance(object: *mut u8) {
	if object.is_null() {
		return;
	}
	unsafe { crate::types::weakref::finalize_heap_instance(object) };
	let storage = unsafe { &mut (*object.cast::<PyTupleSubclassInstance>()).storage };
	if !storage.items.is_null() && storage.len != 0 {
		unsafe { drop(Vec::from_raw_parts(storage.items, storage.len, storage.len)) };
		storage.items = core::ptr::null_mut();
		storage.len = 0;
	}
}

const _: () = {
	assert!(offset_of!(PyTuple, ob_base) == 0);
	// `tuple_storage_ptr` overlays `PyTupleStorage` on `PyTuple`'s tail.
	assert!(offset_of!(PyTupleStorage, len) == 0);
	assert!(
		offset_of!(PyTuple, items) - offset_of!(PyTuple, len) == offset_of!(PyTupleStorage, items)
	);
	assert!(
		core::mem::size_of::<PyTuple>()
			== offset_of!(PyTuple, len) + core::mem::size_of::<PyTupleStorage>()
	);
	// The heap-instance prefix cast contract for tuple-subclass instances.
	assert!(offset_of!(PyTupleSubclassInstance, base) == 0);
	// Layout detection is keyed on `tp_basicsize` under the shared
	// heap-instance GC id: every extended layout must stay distinct.
	assert!(
		core::mem::size_of::<PyTupleSubclassInstance>()
			!= core::mem::size_of::<crate::types::type_::PyHeapInstance>()
	);
	assert!(
		core::mem::size_of::<PyTupleSubclassInstance>()
			!= core::mem::size_of::<crate::types::type_::PyPayloadSubclassInstance>()
	);
	assert!(
		core::mem::size_of::<PyTupleSubclassInstance>()
			!= core::mem::size_of::<crate::types::dict::PyDictSubclassInstance>()
	);
	assert!(
		core::mem::size_of::<PyTupleSubclassInstance>()
			!= core::mem::size_of::<crate::types::list::PyListSubclassInstance>()
	);
};
