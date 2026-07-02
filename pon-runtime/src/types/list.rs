//! List sequence implementation.

use core::mem::offset_of;

use crate::object::{PyObject, PyObjectHeader};

/// Boxed Python `list` with Rust-owned contiguous pointer storage.
///
/// `items` points to `cap` initialized `*mut PyObject` slots allocated from a
/// leaked `Vec`; the GC finalizer reconstructs and drops that vector.  The
/// object trace hook visits only `len` live entries.
#[repr(C)]
#[derive(Debug)]
pub struct PyList {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Number of live elements.
    pub len: usize,
    /// Allocated item slots.
    pub cap: usize,
    /// Pointer to `cap` boxed-object slots, or NULL when `cap == 0`.
    pub items: *mut *mut PyObject,
}

impl PyList {
    /// Returns the live elements as an immutable slice.
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

    /// Returns the live elements as a mutable slice.
    ///
    /// # Safety
    ///
    /// `self.items` must either be NULL with `self.len == 0`, or point to at
    /// least `self.len` initialized slots not aliased for mutation elsewhere.
    pub unsafe fn as_mut_slice(&mut self) -> &mut [*mut PyObject] {
        if self.len == 0 {
            &mut []
        } else {
            unsafe { core::slice::from_raw_parts_mut(self.items, self.len) }
        }
    }
}

/// Formats an exact runtime list with Python list display syntax.
///
/// # Safety
///
/// `object` must point to a live [`PyList`] allocation.
pub unsafe fn list_repr(object: *mut PyObject) -> Result<String, String> {
    if object.is_null() {
        return Err("list repr received NULL".to_owned());
    }
    let list = unsafe { &*object.cast::<PyList>() };
    let items = unsafe { list.as_slice() };
    let mut out = String::from("[");
    for (index, item) in items.iter().copied().enumerate() {
        if index != 0 {
            out.push_str(", ");
        }
        match crate::native::builtins_mod::try_repr_text(item) {
            Ok(text) => out.push_str(&text),
            Err(()) => return Err("list element repr raised".to_owned()),
        }
    }
    out.push(']');
    Ok(out)
}

/// Traces the live object references contained in a list.
///
/// # Safety
///
/// `object` must be NULL or point to a live [`PyList`] allocation.
pub unsafe extern "C" fn trace_list(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    let list = unsafe { &*object.cast::<PyList>() };
    for child in unsafe { list.as_slice() }.iter().copied() {
        if !child.is_null() {
            visitor(child.cast::<u8>());
        }
    }
}

/// Drops the Rust-owned backing vector for a list.
///
/// # Safety
///
/// `object` must be NULL or point to a live [`PyList`] allocation whose `items`
/// pointer was produced by this module's vector-leak convention.
pub unsafe extern "C" fn finalize_list(object: *mut u8) {
    if object.is_null() {
        return;
    }
    let list = unsafe { &mut *object.cast::<PyList>() };
    if !list.items.is_null() && list.cap != 0 {
        unsafe { drop(Vec::from_raw_parts(list.items, list.cap, list.cap)) };
        list.items = core::ptr::null_mut();
        list.len = 0;
        list.cap = 0;
    }
}

/// Contiguous list cell block: the field triple shared by the exact
/// [`PyList`] tail and list-subclass instances.  Raw list helpers operate on
/// this view so both layouts share one implementation.
#[repr(C)]
#[derive(Debug)]
pub struct PyListStorage {
    /// Number of live elements.
    pub len: usize,
    /// Allocated item slots.
    pub cap: usize,
    /// Pointer to `cap` boxed-object slots, or NULL when `cap == 0`.
    pub items: *mut *mut PyObject,
}

impl PyListStorage {
    /// Returns the live elements as an immutable slice.
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

    /// Returns the live elements as a mutable slice.
    ///
    /// # Safety
    ///
    /// `self.items` must either be NULL with `self.len == 0`, or point to at
    /// least `self.len` initialized slots not aliased for mutation elsewhere.
    pub unsafe fn as_mut_slice(&mut self) -> &mut [*mut PyObject] {
        if self.len == 0 {
            &mut []
        } else {
            unsafe { core::slice::from_raw_parts_mut(self.items, self.len) }
        }
    }
}

/// Heap-class instance embedding native list storage (`class L(list)`).
/// Mirrors `PyDictSubclassInstance`: the generic heap-instance prefix keeps
/// every instance-attribute, slot, and weakref path working unchanged, while
/// `storage` carries the list cells the native protocol reads through.
#[derive(Debug)]
pub struct PyListSubclassInstance {
    /// Generic heap-instance prefix; must remain first.
    pub base: crate::types::type_::PyHeapInstance,
    /// Embedded native list storage.
    pub storage: PyListStorage,
}

/// Returns whether `ty` is a heap class using the list-subclass layout: the
/// heap-instance GC id combined with the extended `PyListSubclassInstance`
/// size.  Deliberately lock-free so it is safe on every list fast path.
#[must_use]
pub unsafe fn type_is_list_subclass(ty: *mut crate::object::PyType) -> bool {
    if ty.is_null() {
        return false;
    }
    unsafe {
        (*ty).gc_type_id == crate::types::type_::TYPE_ID_HEAP_INSTANCE.0 as usize
            && (*ty).tp_basicsize == core::mem::size_of::<PyListSubclassInstance>()
    }
}

/// Returns whether `object` is a list-subclass heap instance.
#[must_use]
pub unsafe fn is_list_subclass_instance(object: *mut PyObject) -> bool {
    if object.is_null() || !crate::tag::is_heap(object) {
        return false;
    }
    unsafe { type_is_list_subclass((*object).ob_type.cast_mut()) }
}

/// Returns whether a class built over `bases` embeds native list storage:
/// some base linearizes over the builtin `list` type.  Mirrors
/// `dict::class_bases_embed_dict` (non-heap name match).
#[must_use]
pub unsafe fn class_bases_embed_list(bases: &[*mut crate::object::PyType]) -> bool {
    bases.iter().copied().any(|base| {
        unsafe { crate::mro::mro_entries(base) }.iter().any(|entry| {
            !entry.is_null()
                && unsafe {
                    (**entry).gc_type_id != crate::types::type_::TYPE_ID_HEAP_INSTANCE.0 as usize
                        && (**entry).name() == "list"
                }
        })
    })
}

/// Traces GC references of a list-subclass instance: the heap-instance
/// prefix (instance dict values, slots) plus the embedded list items.
pub unsafe extern "C" fn trace_list_subclass_instance(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    unsafe { crate::types::weakref::trace_heap_instance(object, visitor) };
    let storage = unsafe { &(*object.cast::<PyListSubclassInstance>()).storage };
    for child in unsafe { storage.as_slice() }.iter().copied() {
        if !child.is_null() {
            visitor(child.cast::<u8>());
        }
    }
}

/// Finalizes a list-subclass instance: heap-instance semantics (`__del__`,
/// weakrefs, instance dict, slots) plus the leaked list backing vector.
pub unsafe extern "C" fn finalize_list_subclass_instance(object: *mut u8) {
    if object.is_null() {
        return;
    }
    unsafe { crate::types::weakref::finalize_heap_instance(object) };
    let storage = unsafe { &mut (*object.cast::<PyListSubclassInstance>()).storage };
    if !storage.items.is_null() && storage.cap != 0 {
        unsafe { drop(Vec::from_raw_parts(storage.items, storage.cap, storage.cap)) };
        storage.items = core::ptr::null_mut();
        storage.len = 0;
        storage.cap = 0;
    }
}

const _: () = {
    assert!(offset_of!(PyList, ob_base) == 0);
    // `list_cells` overlays `PyListStorage` on `PyList`'s tail.
    assert!(offset_of!(PyListStorage, len) == 0);
    assert!(offset_of!(PyList, cap) - offset_of!(PyList, len) == offset_of!(PyListStorage, cap));
    assert!(offset_of!(PyList, items) - offset_of!(PyList, len) == offset_of!(PyListStorage, items));
    assert!(core::mem::size_of::<PyList>() == offset_of!(PyList, len) + core::mem::size_of::<PyListStorage>());
    // The heap-instance prefix cast contract for list-subclass instances.
    assert!(offset_of!(PyListSubclassInstance, base) == 0);
    // Layout detection is keyed on `tp_basicsize` under the shared
    // heap-instance GC id: every extended layout must stay distinct.
    assert!(
        core::mem::size_of::<PyListSubclassInstance>() != core::mem::size_of::<crate::types::type_::PyHeapInstance>()
    );
    assert!(
        core::mem::size_of::<PyListSubclassInstance>()
            != core::mem::size_of::<crate::types::type_::PyPayloadSubclassInstance>()
    );
    assert!(
        core::mem::size_of::<PyListSubclassInstance>()
            != core::mem::size_of::<crate::types::dict::PyDictSubclassInstance>()
    );
};
