//! Frozen set implementation.

use core::mem::{offset_of, size_of};
use core::ptr;
use std::sync::LazyLock;

use crate::object::{PyNumberMethods, PyObject, PyObjectHeader, PySequenceMethods, PyType};
use crate::types::dict::{hash_object, type_name};
use crate::types::set_::{insert_unique_entries, set_equal};

/// Boxed immutable Python `frozenset`.
#[repr(C)]
#[derive(Debug)]
pub struct PyFrozenSet {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Unique elements in insertion order.
    pub entries: Vec<*mut PyObject>,
    /// Open-addressed element index table. Buckets store indexes into `entries`.
    pub buckets: Vec<Option<usize>>,
    /// Cached hash. Valid only when `hash_computed` is true.
    pub hash: isize,
    /// Whether `hash` contains a computed value.
    pub hash_computed: bool,
}

/// Builds the runtime type object for frozensets.
#[must_use]
pub fn frozenset_type(type_type: *const PyType) -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut sequence = PySequenceMethods::EMPTY;
        sequence.sq_length = Some(frozenset_len_slot);
        sequence.sq_contains = Some(frozenset_contains_slot);

        let mut number = PyNumberMethods::EMPTY;
        number.nb_or = Some(frozenset_union_slot);
        number.nb_and = Some(frozenset_intersection_slot);
        number.nb_subtract = Some(frozenset_difference_slot);

        let mut ty = PyType::new(ptr::null(), "frozenset", size_of::<PyFrozenSet>());
        ty.tp_hash = Some(frozenset_hash_slot);
        ty.tp_as_sequence = Box::into_raw(Box::new(sequence));
        ty.tp_as_number = Box::into_raw(Box::new(number));
        ty.tp_iter = Some(frozenset_iter_slot);
        ty.tp_richcmp = Some(crate::types::set_::set_richcmp_slot);
        ty.tp_getattro = Some(frozenset_getattro_slot);
        Box::into_raw(Box::new(ty)) as usize
    });
    let ty = *TYPE as *mut PyType;
    unsafe { install_type_type(ty, type_type) };
    ty
}

/// Traces frozenset elements.
pub unsafe extern "C" fn trace_frozenset(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    let set = unsafe { &*object.cast::<PyFrozenSet>() };
    for value in &set.entries {
        if !value.is_null() {
            visitor((*value).cast::<u8>());
        }
    }
}

/// Drops frozenset-owned Rust storage.
pub unsafe extern "C" fn finalize_frozenset(object: *mut u8) {
    if object.is_null() {
        return;
    }
    let set = unsafe { &mut *object.cast::<PyFrozenSet>() };
    unsafe { ptr::drop_in_place(ptr::addr_of_mut!(set.entries)) };
    unsafe { ptr::drop_in_place(ptr::addr_of_mut!(set.buckets)) };
}

/// Initializes a freshly allocated frozenset from already unique entries.
pub unsafe fn init_frozenset(ptr: *mut PyFrozenSet, ob_type: *const PyType, entries: Vec<*mut PyObject>) {
    let buckets = build_buckets(&entries).unwrap_or_default();
    unsafe {
        ptr::write(
            ptr,
            PyFrozenSet {
                ob_base: PyObjectHeader::new(ob_type),
                entries,
                buckets,
                hash: 0,
                hash_computed: false,
            },
        );
    }
}

/// Returns whether `object` is an exact frozenset.
#[must_use]
pub unsafe fn is_frozenset(object: *mut PyObject) -> bool {
    (unsafe { type_name(object) }) == Some("frozenset")
}

/// Borrows an exact frozenset immutably.
pub unsafe fn frozenset_ref(object: *mut PyObject) -> Result<&'static PyFrozenSet, String> {
    if !unsafe { is_frozenset(object) } {
        return Err("expected frozenset object".to_owned());
    }
    Ok(unsafe { &*object.cast::<PyFrozenSet>() })
}

/// Borrows an exact frozenset mutably.
pub unsafe fn frozenset_mut(object: *mut PyObject) -> Result<&'static mut PyFrozenSet, String> {
    if !unsafe { is_frozenset(object) } {
        return Err("expected frozenset object".to_owned());
    }
    Ok(unsafe { &mut *object.cast::<PyFrozenSet>() })
}

/// Returns a frozenset insertion-order snapshot.
pub unsafe fn entries_snapshot(object: *mut PyObject) -> Result<Vec<*mut PyObject>, String> {
    Ok(unsafe { frozenset_ref(object)? }.entries.clone())
}

/// Builds a unique insertion-order element vector from arbitrary values.
pub unsafe fn unique_entries(items: &[*mut PyObject]) -> Result<Vec<*mut PyObject>, String> {
    let mut entries = Vec::with_capacity(items.len());
    unsafe { insert_unique_entries(&mut entries, items)? };
    Ok(entries)
}

/// Returns true when an item is present in a frozenset.
pub unsafe fn frozenset_contains(object: *mut PyObject, item: *mut PyObject) -> Result<bool, String> {
    let hash = unsafe { hash_object(item)? };
    let set = unsafe { frozenset_ref(object)? };
    if set.buckets.is_empty() {
        return Ok(false);
    }
    let mut bucket = bucket_index(hash, set.buckets.len());
    for _ in 0..set.buckets.len() {
        let Some(index) = set.buckets[bucket] else {
            return Ok(false);
        };
        let entry = set.entries[index];
        if unsafe { hash_object(entry)? } == hash && unsafe { crate::types::dict::object_equal(entry, item)? } {
            return Ok(true);
        }
        bucket = (bucket + 1) & (set.buckets.len() - 1);
    }
    Ok(false)
}


/// Computes frozenset equality.
pub fn frozenset_equal(left: *mut PyObject, right: *mut PyObject) -> Result<bool, String> {
    unsafe { set_equal(left, right) }
}

/// Returns the cached/computed frozenset hash.
pub unsafe fn frozenset_hash_value(object: *mut PyObject) -> Result<isize, String> {
    let set = unsafe { frozenset_mut(object)? };
    if set.hash_computed {
        return Ok(set.hash);
    }

    // Order-independent, content-based combiner mirroring CPython's
    // frozenset_hash shape so equal frozensets hash equal regardless of
    // insertion order.
    let mut mixed: usize = 0;
    for item in &set.entries {
        let item_hash = unsafe { hash_object(*item)? } as usize;
        mixed ^= shuffle_bits(item_hash);
    }
    mixed ^= set.entries.len().wrapping_add(1).wrapping_mul(1_927_868_237);
    mixed ^= (mixed >> 11) ^ (mixed >> 25);
    mixed = mixed.wrapping_mul(69069).wrapping_add(907_133_923);
    let mut hash = mixed as isize;
    if hash == -1 {
        hash = 590_923_713;
    }
    set.hash = hash;
    set.hash_computed = true;
    Ok(hash)
}

unsafe extern "C" fn frozenset_len_slot(object: *mut PyObject) -> isize {
    match unsafe { frozenset_ref(object) } {
        Ok(set) => isize::try_from(set.entries.len()).unwrap_or(isize::MAX),
        Err(_) => -1,
    }
}

unsafe extern "C" fn frozenset_contains_slot(object: *mut PyObject, item: *mut PyObject) -> core::ffi::c_int {
    let result = unsafe { crate::types::set_::set_contains(object, item) };
    match result {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(_) => -1,
    }
}

unsafe extern "C" fn frozenset_hash_slot(object: *mut PyObject) -> isize {
    match unsafe { frozenset_hash_value(object) } {
        Ok(hash) => hash,
        Err(_) => -1,
    }
}

/// Disperses element-hash bit patterns before XOR combination (CPython shape).
fn shuffle_bits(hash: usize) -> usize {
    ((hash ^ 89_869_747) ^ (hash << 16)).wrapping_mul(3_644_798_167)
}

unsafe extern "C" fn frozenset_getattro_slot(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        return crate::abi::return_null_with_error("frozenset attribute name must be str");
    };
    match name {
        "union" | "intersection" | "difference" | "issubset" | "__contains__" | "copy" => unsafe {
            crate::abi::map::pon_set_bound_method(object, name)
        },
        _ => crate::abi::return_null_with_error(format!("attribute '{name}' was not found")),
    }
}

unsafe extern "C" fn frozenset_iter_slot(object: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::map::pon_set_iter(object) }
}

unsafe extern "C" fn frozenset_union_slot(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::map::pon_set_union(left, right) }
}

unsafe extern "C" fn frozenset_intersection_slot(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::map::pon_set_intersection(left, right) }
}

unsafe extern "C" fn frozenset_difference_slot(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::map::pon_set_difference(left, right) }
}

fn build_buckets(entries: &[*mut PyObject]) -> Result<Vec<Option<usize>>, String> {
    let mut buckets = vec![None; bucket_capacity(entries.len())];
    for index in 0..entries.len() {
        let hash = unsafe { hash_object(entries[index])? };
        let mut bucket = bucket_index(hash, buckets.len());
        for _ in 0..buckets.len() {
            if buckets[bucket].is_none() {
                buckets[bucket] = Some(index);
                break;
            }
            bucket = (bucket + 1) & (buckets.len() - 1);
        }
    }
    Ok(buckets)
}

fn bucket_capacity(len: usize) -> usize {
    len.saturating_mul(2).max(8).next_power_of_two()
}

fn bucket_index(hash: isize, bucket_count: usize) -> usize {
    (hash as usize) & (bucket_count - 1)
}

unsafe fn install_type_type(ty: *mut PyType, type_type: *const PyType) {
    if !ty.is_null() && !type_type.is_null() {
        unsafe { (*ty).ob_base.ob_type = type_type };
    }
}

const _: () = {
    assert!(offset_of!(PyFrozenSet, ob_base) == 0);
};
