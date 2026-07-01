//! Set implementation.

use core::ffi::c_int;
use core::mem::{offset_of, size_of};
use core::ptr;
use std::sync::LazyLock;

use crate::object::{PyNumberMethods, PyObject, PyObjectHeader, PySequenceMethods, PyType};
use crate::types::{dict::{hash_object, object_equal, type_name}, type_};

/// Boxed mutable Python `set`.
#[repr(C)]
#[derive(Debug)]
pub struct PySet {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Unique elements in insertion order.
    pub entries: Vec<*mut PyObject>,
    /// Open-addressed element index table. Buckets store indexes into `entries`.
    pub buckets: Vec<Option<usize>>,
}

/// Iterator over a set or frozenset snapshot.
#[repr(C)]
#[derive(Debug)]
pub struct PySetIter {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Traversed set/frozenset object.
    pub set: *mut PyObject,
    /// Next insertion-order index.
    pub index: usize,
}

/// Builds the runtime type object for mutable sets.
#[must_use]
pub fn set_type(type_type: *const PyType) -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut sequence = PySequenceMethods::EMPTY;
        sequence.sq_length = Some(set_len_slot);
        sequence.sq_contains = Some(set_contains_slot);

        let mut number = PyNumberMethods::EMPTY;
        number.nb_or = Some(set_union_slot);
        number.nb_and = Some(set_intersection_slot);
        number.nb_subtract = Some(set_difference_slot);

        let mut ty = PyType::new(ptr::null(), "set", size_of::<PySet>());
        ty.tp_as_sequence = Box::into_raw(Box::new(sequence));
        ty.tp_as_number = Box::into_raw(Box::new(number));
        ty.tp_iter = Some(set_iter_slot);
        ty.tp_getattro = Some(set_getattro_slot);
        Box::into_raw(Box::new(ty)) as usize
    });
    let ty = *TYPE as *mut PyType;
    unsafe { install_type_type(ty, type_type) };
    ty
}

/// Builds the runtime type object for set iterators.
#[must_use]
pub fn set_iter_type(type_type: *const PyType) -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(ptr::null(), "set_iterator", size_of::<PySetIter>());
        ty.tp_iternext = Some(set_iter_next_slot);
        Box::into_raw(Box::new(ty)) as usize
    });
    let ty = *TYPE as *mut PyType;
    unsafe { install_type_type(ty, type_type) };
    ty
}

/// Traces mutable set elements.
pub unsafe extern "C" fn trace_set(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    let set = unsafe { &*object.cast::<PySet>() };
    for value in &set.entries {
        if !value.is_null() {
            visitor((*value).cast::<u8>());
        }
    }
}

/// Drops set-owned Rust storage.
pub unsafe extern "C" fn finalize_set(object: *mut u8) {
    if object.is_null() {
        return;
    }
    let set = unsafe { &mut *object.cast::<PySet>() };
    unsafe { ptr::drop_in_place(ptr::addr_of_mut!(set.entries)) };
    unsafe { ptr::drop_in_place(ptr::addr_of_mut!(set.buckets)) };
}

/// Traces the set retained by a set iterator.
pub unsafe extern "C" fn trace_set_iter(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    let iter = unsafe { &*object.cast::<PySetIter>() };
    if !iter.set.is_null() {
        visitor(iter.set.cast::<u8>());
    }
}

/// Initializes a freshly allocated mutable set object.
pub unsafe fn init_set(ptr: *mut PySet, ob_type: *const PyType, capacity: usize) {
    unsafe {
        ptr::write(
            ptr,
            PySet {
                ob_base: PyObjectHeader::new(ob_type),
                entries: Vec::with_capacity(capacity),
                buckets: Vec::with_capacity(bucket_capacity(capacity)),
            },
        );
    }
}

/// Initializes a freshly allocated set iterator.
pub unsafe fn init_set_iter(ptr: *mut PySetIter, ob_type: *const PyType, set: *mut PyObject) {
    unsafe {
        ptr::write(
            ptr,
            PySetIter {
                ob_base: PyObjectHeader::new(ob_type),
                set,
                index: 0,
            },
        );
    }
}

/// Returns whether `object` is an exact mutable set.
#[must_use]
pub unsafe fn is_set(object: *mut PyObject) -> bool {
    (unsafe { type_name(object) }) == Some("set")
}

/// Returns whether `object` is a mutable set or frozenset.
#[must_use]
pub unsafe fn is_any_set(object: *mut PyObject) -> bool {
    matches!(unsafe { type_name(object) }, Some("set" | "frozenset"))
}

/// Borrows an exact mutable set mutably.
pub unsafe fn set_mut(object: *mut PyObject) -> Result<&'static mut PySet, String> {
    if !unsafe { is_set(object) } {
        return Err("expected set object".to_owned());
    }
    Ok(unsafe { &mut *object.cast::<PySet>() })
}

/// Borrows an exact mutable set immutably.
pub unsafe fn set_ref(object: *mut PyObject) -> Result<&'static PySet, String> {
    if !unsafe { is_set(object) } {
        return Err("expected set object".to_owned());
    }
    Ok(unsafe { &*object.cast::<PySet>() })
}

/// Returns an insertion-order snapshot of any supported set flavor.
pub unsafe fn entries_snapshot(object: *mut PyObject) -> Result<Vec<*mut PyObject>, String> {
    match unsafe { type_name(object) } {
        Some("set") => Ok(unsafe { set_ref(object)? }.entries.clone()),
        Some("frozenset") => unsafe { crate::types::frozenset::entries_snapshot(object) },
        _ => Err("expected set or frozenset object".to_owned()),
    }
}

/// Adds an element to a mutable set, preserving original insertion position for duplicates.
pub unsafe fn set_add(set: *mut PyObject, item: *mut PyObject) -> Result<(), String> {
    if item.is_null() {
        return Err("set item is NULL".to_owned());
    }
    let hash = unsafe { hash_object(item)? };
    let set = unsafe { set_mut(set)? };
    ensure_set_buckets(set)?;
    if unsafe { find_set_index(set, item, hash)? }.is_none() {
        ensure_set_insert_capacity(set)?;
        let index = set.entries.len();
        set.entries.push(item);
        insert_bucket(&mut set.buckets, &set.entries, index)?;
    }
    Ok(())
}

/// Removes an element from a mutable set if present.
pub unsafe fn set_discard(set: *mut PyObject, item: *mut PyObject) -> Result<(), String> {
    if item.is_null() {
        return Err("set item is NULL".to_owned());
    }
    let set = unsafe { set_mut(set)? };
    if let Some(index) = unsafe { find_element_index(&set.entries, item)? } {
        set.entries.remove(index);
        rebuild_set_buckets(set)?;
    }
    Ok(())
}

/// Returns true when `item` is present in any supported set flavor.
pub unsafe fn set_contains(set: *mut PyObject, item: *mut PyObject) -> Result<bool, String> {
    if item.is_null() {
        return Err("set item is NULL".to_owned());
    }
    let hash = unsafe { hash_object(item)? };
    if unsafe { is_set(set) } {
        let set = unsafe { set_mut(set)? };
        ensure_set_buckets(set)?;
        Ok(unsafe { find_set_index(set, item, hash)? }.is_some())
    } else if unsafe { crate::types::frozenset::is_frozenset(set) } {
        unsafe { crate::types::frozenset::frozenset_contains(set, item) }
    } else {
        let entries = unsafe { entries_snapshot(set)? };
        Ok(unsafe { find_element_index(&entries, item)? }.is_some())
    }
}

/// Computes set equality for any supported set flavor.
pub unsafe fn set_equal(left: *mut PyObject, right: *mut PyObject) -> Result<bool, String> {
    let left_entries = unsafe { entries_snapshot(left)? };
    let right_entries = unsafe { entries_snapshot(right)? };
    if left_entries.len() != right_entries.len() {
        return Ok(false);
    }
    for item in left_entries {
        if unsafe { find_element_index(&right_entries, item)? }.is_none() {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Adds every element from `other_entries` into `target_entries`.
pub unsafe fn insert_unique_entries(target_entries: &mut Vec<*mut PyObject>, other_entries: &[*mut PyObject]) -> Result<(), String> {
    for item in other_entries {
        let _ = unsafe { hash_object(*item)? };
        if unsafe { find_element_index(target_entries, *item)? }.is_none() {
            target_entries.push(*item);
        }
    }
    Ok(())
}

/// Finds an element by Python equality.
pub unsafe fn find_element_index(entries: &[*mut PyObject], item: *mut PyObject) -> Result<Option<usize>, String> {
    for (index, entry) in entries.iter().enumerate() {
        if unsafe { object_equal(*entry, item)? } {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

unsafe fn find_set_index(set: &PySet, item: *mut PyObject, hash: isize) -> Result<Option<usize>, String> {
    if set.buckets.is_empty() {
        return Ok(None);
    }
    let mut bucket = bucket_index(hash, set.buckets.len());
    for _ in 0..set.buckets.len() {
        let Some(index) = set.buckets[bucket] else {
            return Ok(None);
        };
        let entry = set.entries[index];
        if unsafe { hash_object(entry)? } == hash && unsafe { object_equal(entry, item)? } {
            return Ok(Some(index));
        }
        bucket = (bucket + 1) & (set.buckets.len() - 1);
    }
    Ok(None)
}

fn ensure_set_buckets(set: &mut PySet) -> Result<(), String> {
    if set.buckets.len() < bucket_capacity(set.entries.len()) {
        rebuild_set_buckets(set)?;
    }
    Ok(())
}

fn ensure_set_insert_capacity(set: &mut PySet) -> Result<(), String> {
    let occupied_after_insert = set.entries.len().saturating_add(1);
    if occupied_after_insert.saturating_mul(3) >= set.buckets.len().saturating_mul(2) {
        rebuild_set_buckets_with_capacity(set, bucket_capacity(occupied_after_insert))?;
    }
    Ok(())
}

fn rebuild_set_buckets(set: &mut PySet) -> Result<(), String> {
    rebuild_set_buckets_with_capacity(set, bucket_capacity(set.entries.len()))
}

fn rebuild_set_buckets_with_capacity(set: &mut PySet, capacity: usize) -> Result<(), String> {
    let mut buckets = vec![None; capacity];
    for index in 0..set.entries.len() {
        insert_bucket(&mut buckets, &set.entries, index)?;
    }
    set.buckets = buckets;
    Ok(())
}

fn insert_bucket(buckets: &mut [Option<usize>], entries: &[*mut PyObject], index: usize) -> Result<(), String> {
    if buckets.is_empty() {
        return Err("set bucket table is empty".to_owned());
    }
    let hash = unsafe { hash_object(entries[index])? };
    let mut bucket = bucket_index(hash, buckets.len());
    for _ in 0..buckets.len() {
        if buckets[bucket].is_none() {
            buckets[bucket] = Some(index);
            return Ok(());
        }
        bucket = (bucket + 1) & (buckets.len() - 1);
    }
    Err("set bucket table is full".to_owned())
}

fn bucket_capacity(len: usize) -> usize {
    len.saturating_mul(2).max(8).next_power_of_two()
}

fn bucket_index(hash: isize, bucket_count: usize) -> usize {
    (hash as usize) & (bucket_count - 1)
}

unsafe extern "C" fn set_len_slot(object: *mut PyObject) -> isize {
    match unsafe { entries_snapshot(object) } {
        Ok(entries) => isize::try_from(entries.len()).unwrap_or(isize::MAX),
        Err(_) => -1,
    }
}

unsafe extern "C" fn set_contains_slot(object: *mut PyObject, item: *mut PyObject) -> c_int {
    match unsafe { set_contains(object, item) } {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(_) => -1,
    }
}

unsafe extern "C" fn set_iter_slot(object: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::map::pon_set_iter(object) }
}

unsafe extern "C" fn set_iter_next_slot(iterator: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::map::pon_set_iter_next(iterator) }
}

unsafe extern "C" fn set_union_slot(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::map::pon_set_union(left, right) }
}

unsafe extern "C" fn set_intersection_slot(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::map::pon_set_intersection(left, right) }
}

unsafe extern "C" fn set_difference_slot(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::map::pon_set_difference(left, right) }
}

unsafe extern "C" fn set_getattro_slot(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        return crate::abi::return_null_with_error("set attribute name must be str");
    };
    match name {
        "add" | "discard" | "union" | "intersection" | "difference" | "issubset" => unsafe {
            crate::abi::map::pon_set_bound_method(object, name)
        },
        _ => crate::abi::return_null_with_error(format!("attribute '{name}' was not found")),
    }
}

unsafe fn install_type_type(ty: *mut PyType, type_type: *const PyType) {
    if !ty.is_null() && !type_type.is_null() {
        unsafe { (*ty).ob_base.ob_type = type_type };
    }
}

const _: () = {
    assert!(offset_of!(PySet, ob_base) == 0);
    assert!(offset_of!(PySetIter, ob_base) == 0);
};
