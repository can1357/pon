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
        number.nb_xor = Some(set_symmetric_difference_slot);
        number.nb_subtract = Some(set_difference_slot);
        let mut ty = PyType::new(ptr::null(), "set", size_of::<PySet>());
        ty.tp_as_sequence = Box::into_raw(Box::new(sequence));
        ty.tp_as_number = Box::into_raw(Box::new(number));
        ty.tp_iter = Some(set_iter_slot);
        ty.tp_richcmp = Some(set_richcmp_slot);
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
        ty.tp_iter = Some(set_iter_identity_slot);
        ty.tp_getattro = Some(crate::abstract_op::iterator_dunder_getattro);
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
///
/// Element hashing and equality may dispatch Python-level hooks (user code):
/// borrows are scoped so none lives across a dispatch, and the bucket insert
/// reuses the already-computed hash instead of rehashing the element.
pub unsafe fn set_add(set: *mut PyObject, item: *mut PyObject) -> Result<(), String> {
    if item.is_null() {
        return Err("set item is NULL".to_owned());
    }
    let hash = unsafe { hash_set_element(item)? };
    ensure_set_buckets(unsafe { set_mut(set)? })?;
    if unsafe { find_set_index(set, item, hash)? }.is_none() {
        let storage = unsafe { set_mut(set)? };
        ensure_set_insert_capacity(storage)?;
        let index = storage.entries.len();
        storage.entries.push(item);
        insert_bucket_hashed(&mut storage.buckets, hash, index)?;
    }
    Ok(())
}

/// Hashes a prospective set element, wrapping hash failures the way CPython
/// 3.14 reports them: `cannot use 'list' as a set element (unhashable type: 'list')`.
pub unsafe fn hash_set_element(item: *mut PyObject) -> Result<isize, String> {
    unsafe { hash_object(item) }.map_err(|message| {
        if message.starts_with("unhashable type") {
            let name = unsafe { type_name(item) }.unwrap_or("object");
            format!("cannot use '{name}' as a set element ({message})")
        } else {
            message
        }
    })
}

/// Phase-1 accumulator for set displays (`pon_build_set`): hashes and dedups
/// BEFORE the runtime lock is taken — user `__hash__`/`__eq__` re-enter
/// runtime helpers that take the runtime mutex, so they must never run
/// inside `with_runtime`.  First occurrence wins (CPython set semantics).
/// `entries` is caller-local, so hook re-entrancy cannot touch it.
pub unsafe fn collect_prehashed_element(entries: &mut Vec<(*mut PyObject, isize)>, item: *mut PyObject) -> Result<(), String> {
    if item.is_null() {
        return Err("set item is NULL".to_owned());
    }
    let hash = unsafe { hash_set_element(item)? };
    for (entry, entry_hash) in entries.iter() {
        if *entry_hash == hash && unsafe { object_equal(*entry, item)? } {
            return Ok(());
        }
    }
    entries.push((item, hash));
    Ok(())
}

/// Phase-2 bulk fill for `pon_build_set`, safe under the runtime lock:
/// entries and buckets are built from the pre-computed hashes with NO rehash
/// (the set layout stores no hashes, and `insert_bucket`'s rehash would
/// re-enter user `__hash__` inside `with_runtime`).
pub unsafe fn set_fill_prehashed(set: *mut PyObject, elements: &[(*mut PyObject, isize)]) -> Result<(), String> {
    let storage = unsafe { set_mut(set)? };
    storage.entries.extend(elements.iter().map(|(item, _)| *item));
    let mut buckets = vec![None; bucket_capacity(storage.entries.len())];
    for (index, (_, hash)) in elements.iter().enumerate() {
        insert_bucket_hashed(&mut buckets, *hash, index)?;
    }
    storage.buckets = buckets;
    Ok(())
}

/// Removes an element from a mutable set if present.
pub unsafe fn set_discard(set: *mut PyObject, item: *mut PyObject) -> Result<(), String> {
    unsafe { set_remove(set, item) }.map(|_| ())
}

/// Removes an element from a mutable set, reporting whether it was present.
///
/// CPython parity: the probe element is hashed (unhashable probes raise the
/// wrapped TypeError) and located through the bucket chain, with any
/// Python-level hook dispatch running outside the storage borrows.
pub unsafe fn set_remove(set: *mut PyObject, item: *mut PyObject) -> Result<bool, String> {
    if item.is_null() {
        return Err("set item is NULL".to_owned());
    }
    let hash = unsafe { hash_set_element(item)? };
    ensure_set_buckets(unsafe { set_mut(set)? })?;
    if let Some(index) = unsafe { find_set_index(set, item, hash)? } {
        let storage = unsafe { set_mut(set)? };
        storage.entries.remove(index);
        rebuild_set_buckets(storage)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Removes and returns the first insertion-order element, if any.
pub unsafe fn set_pop(set: *mut PyObject) -> Result<Option<*mut PyObject>, String> {
    let set = unsafe { set_mut(set)? };
    if set.entries.is_empty() {
        return Ok(None);
    }
    let value = set.entries.remove(0);
    rebuild_set_buckets(set)?;
    Ok(Some(value))
}

/// Removes every element from a mutable set.
pub unsafe fn set_clear(set: *mut PyObject) -> Result<(), String> {
    let set = unsafe { set_mut(set)? };
    set.entries.clear();
    set.buckets.clear();
    Ok(())
}

/// Returns true when `item` is present in any supported set flavor.
pub unsafe fn set_contains(set: *mut PyObject, item: *mut PyObject) -> Result<bool, String> {
    if item.is_null() {
        return Err("set item is NULL".to_owned());
    }
    let hash = unsafe { hash_set_element(item)? };
    if unsafe { is_set(set) } {
        ensure_set_buckets(unsafe { set_mut(set)? })?;
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

/// Returns whether every element of `left_entries` is present in `right_entries`.
pub unsafe fn entries_subset(left_entries: &[*mut PyObject], right_entries: &[*mut PyObject]) -> Result<bool, String> {
    for item in left_entries {
        if unsafe { find_element_index(right_entries, *item)? }.is_none() {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Shared `tp_richcmp` slot for sets and frozensets.
///
/// Implements CPython's content-based subset/superset comparison semantics
/// across both set flavors; a non-set right operand defers with
/// `NotImplemented` so reflected dispatch and the identity fallback can run.
pub unsafe extern "C" fn set_richcmp_slot(left: *mut PyObject, right: *mut PyObject, op: c_int) -> *mut PyObject {
    if unsafe { !is_any_set(left) || !is_any_set(right) } {
        return unsafe { crate::abi::pon_not_implemented() };
    }
    match unsafe { set_richcmp_bool(left, right, op) } {
        Ok(value) => unsafe { crate::abi::pon_const_bool(c_int::from(value)) },
        Err(message) => crate::abi::return_null_with_error(message),
    }
}

unsafe fn set_richcmp_bool(left: *mut PyObject, right: *mut PyObject, op: c_int) -> Result<bool, String> {
    use crate::abstract_op::{RICH_EQ, RICH_GE, RICH_GT, RICH_LE, RICH_LT, RICH_NE};

    let left_entries = unsafe { entries_snapshot(left)? };
    let right_entries = unsafe { entries_snapshot(right)? };
    let op = u8::try_from(op).map_err(|_| "unknown rich comparison operation".to_owned())?;
    match op {
        RICH_EQ | RICH_NE => {
            let equal = left_entries.len() == right_entries.len()
                && unsafe { entries_subset(&left_entries, &right_entries)? };
            Ok(if op == RICH_EQ { equal } else { !equal })
        }
        RICH_LE => unsafe { entries_subset(&left_entries, &right_entries) },
        RICH_GE => unsafe { entries_subset(&right_entries, &left_entries) },
        RICH_LT => Ok(left_entries.len() < right_entries.len()
            && unsafe { entries_subset(&left_entries, &right_entries)? }),
        RICH_GT => Ok(right_entries.len() < left_entries.len()
            && unsafe { entries_subset(&right_entries, &left_entries)? }),
        _ => Err("unknown rich comparison operation".to_owned()),
    }
}

/// Adds every element from `other_entries` into `target_entries`.
pub unsafe fn insert_unique_entries(target_entries: &mut Vec<*mut PyObject>, other_entries: &[*mut PyObject]) -> Result<(), String> {
    for item in other_entries {
        let _ = unsafe { hash_set_element(*item)? };
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

/// Storage-shape witness for the deferred probe: every set mutation (add
/// grows `entries`; remove/pop shrink it AND swap in a fresh bucket
/// allocation; clear empties both) changes at least one component.
fn set_witness(storage: &PySet) -> (usize, usize, usize, usize) {
    (
        storage.entries.len(),
        storage.entries.as_ptr() as usize,
        storage.buckets.len(),
        storage.buckets.as_ptr() as usize,
    )
}

/// Locates `item`'s entry index for `hash`.
///
/// The set layout stores no element hashes, so candidate verification
/// (rehash + equality) may dispatch Python-level hooks: the bucket chain is
/// snapshotted under the borrow, hooks run with the borrow released, and
/// every affirmative match is re-validated by slot identity — restarting
/// whenever the storage shape changed underneath a dispatch (CPython
/// `lookdict`'s `goto restart` discipline).
unsafe fn find_set_index(set: *mut PyObject, item: *mut PyObject, hash: isize) -> Result<Option<usize>, String> {
    'restart: loop {
        let mut candidates: Vec<(usize, *mut PyObject)> = Vec::new();
        let witness;
        {
            let storage = unsafe { set_ref(set)? };
            if storage.buckets.is_empty() {
                return Ok(None);
            }
            witness = set_witness(storage);
            let mut bucket = bucket_index(hash, storage.buckets.len());
            for _ in 0..storage.buckets.len() {
                let Some(index) = storage.buckets[bucket] else {
                    break;
                };
                candidates.push((index, storage.entries[index]));
                bucket = (bucket + 1) & (storage.buckets.len() - 1);
            }
            if candidates.is_empty() {
                return Ok(None);
            }
        }
        // Deferred pass: rehash + equality run with the borrow released.
        for (index, entry) in candidates {
            if entry == item {
                return Ok(Some(index));
            }
            let matches = unsafe { hash_set_element(entry)? } == hash && unsafe { object_equal(entry, item)? };
            let storage = unsafe { set_ref(set)? };
            if matches {
                if storage.entries.len() > index && storage.entries[index] == entry {
                    return Ok(Some(index));
                }
                continue 'restart;
            }
            if set_witness(storage) != witness {
                continue 'restart;
            }
        }
        return Ok(None);
    }
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
    let hash = unsafe { hash_set_element(entries[index])? };
    insert_bucket_hashed(buckets, hash, index)
}

/// Places `index` into the bucket chain for a KNOWN hash — the dispatch-free
/// core of [`insert_bucket`], usable under the runtime lock.
fn insert_bucket_hashed(buckets: &mut [Option<usize>], hash: isize, index: usize) -> Result<(), String> {
    if buckets.is_empty() {
        return Err("set bucket table is empty".to_owned());
    }
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

/// `iter(it) is it`: set iterators are their own iterator.
unsafe extern "C" fn set_iter_identity_slot(iterator: *mut PyObject) -> *mut PyObject {
    iterator
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

unsafe extern "C" fn set_symmetric_difference_slot(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::map::pon_set_symmetric_difference(left, right) }
}

unsafe extern "C" fn set_getattro_slot(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        return crate::abi::return_null_with_error("set attribute name must be str");
    };
    match name {
        "add" | "discard" | "union" | "intersection" | "difference" | "symmetric_difference"
        | "symmetric_difference_update" | "update" | "issubset" | "issuperset" | "isdisjoint" | "__contains__"
        | "copy" | "remove" | "clear" | "pop" => unsafe {
            crate::abi::map::pon_set_bound_method(object, name)
        },
        _ => crate::abi::exc::raise_attribute_error_text(&format!("attribute '{name}' was not found")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::map::pon_build_set;
    use crate::abi::{pon_const_str, pon_runtime_init};
    use crate::thread_state::test_state_lock;

    #[track_caller]
    fn str_object(text: &str) -> *mut PyObject {
        let object = unsafe { pon_const_str(text.as_ptr(), text.len()) };
        assert!(!object.is_null(), "failed to allocate test str {text:?}");
        object
    }

    #[test]
    fn set_prehash_collect_dedups_and_keeps_first_occurrence() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);

            let x_first = str_object("x");
            let x2 = str_object("x2");
            let x_dup = str_object("x");
            assert_ne!(x_first, x_dup, "duplicate element must be a distinct object");

            let mut elements: Vec<(*mut PyObject, isize)> = Vec::new();
            collect_prehashed_element(&mut elements, x_first).expect("collect first 'x'");
            collect_prehashed_element(&mut elements, x2).expect("collect 'x2'");
            collect_prehashed_element(&mut elements, x_dup).expect("collect duplicate 'x'");

            assert_eq!(elements.len(), 2);
            assert_eq!(elements[0].0, x_first, "duplicate element must keep the FIRST object");
            assert_eq!(elements[1].0, x2);
        }
    }

    #[test]
    fn set_prehash_fill_round_trips_lookups_from_collected_hashes() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);

            let mut elements: Vec<(*mut PyObject, isize)> = Vec::new();
            collect_prehashed_element(&mut elements, str_object("x")).expect("collect 'x'");
            collect_prehashed_element(&mut elements, str_object("x2")).expect("collect 'x2'");
            collect_prehashed_element(&mut elements, str_object("x")).expect("collect duplicate 'x'");

            let set = pon_build_set(core::ptr::null_mut(), 0);
            assert!(!set.is_null());
            set_fill_prehashed(set, &elements).expect("fill prehashed elements");

            assert_eq!(set_ref(set).expect("set ref").entries.len(), 2);
            assert!(set_contains(set, str_object("x")).expect("probe fresh 'x'"));
            assert!(set_contains(set, str_object("x2")).expect("probe fresh 'x2'"));
            assert!(!set_contains(set, str_object("absent")).expect("probe absent element"));
        }
    }

    #[test]
    fn set_prehash_fill_uses_given_hash_verbatim_and_never_rehashes() {
        // Deadlock contract: the set layout stores no hashes, so
        // `set_fill_prehashed` must build buckets from the GIVEN hashes —
        // rehashing would re-enter user `__hash__` inside `with_runtime`.  A
        // deliberately skewed hash parks the element in the wrong bucket
        // chain, so an equal probe (hashed correctly) must MISS; a regression
        // that rehashes during the fill would find it.
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);

            let element = str_object("x");
            let skewed_hash = hash_set_element(element).expect("hash 'x'").wrapping_add(1);

            let set = pon_build_set(core::ptr::null_mut(), 0);
            assert!(!set.is_null());
            set_fill_prehashed(set, &[(element, skewed_hash)]).expect("fill with skewed hash");

            assert_eq!(set_ref(set).expect("set ref").entries.len(), 1);
            assert!(
                !set_contains(set, str_object("x")).expect("probe fresh 'x'"),
                "fill must place buckets by the GIVEN hash, not a recomputed one"
            );
        }
    }
}
