//! Dictionary mapping implementation.

use core::ffi::c_int;
use core::mem::{offset_of, size_of};
use core::ptr;
use std::cell::RefCell;
use std::sync::LazyLock;

use num_bigint::BigInt;

use crate::object::{PyMappingMethods, PyObject, PyObjectHeader, PyType, PyUnicode};
use crate::thread_state::pon_err_set;

/// Boxed insertion-ordered Python `dict`.
#[repr(C)]
#[derive(Debug)]
pub struct PyDict {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Live entries in insertion order. Updating an existing key preserves its slot.
    pub entries: Vec<DictEntry>,
    /// Open-addressed key index table. Buckets store indexes into `entries`.
    pub buckets: Vec<Option<usize>>,
}

/// One insertion-ordered dictionary entry.
#[derive(Clone, Copy, Debug)]
pub struct DictEntry {
    /// Hashable Python key.
    pub key: *mut PyObject,
    /// Associated Python value.
    pub value: *mut PyObject,
    /// Cached normalized hash for open-addressed lookup.
    pub hash: isize,
}

/// Iterator over dictionary keys, values, or items.
#[repr(C)]
#[derive(Debug)]
pub struct PyDictIter {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Dictionary being traversed.
    pub dict: *mut PyObject,
    /// Next insertion-order index.
    pub index: usize,
    /// Projection yielded by this iterator.
    pub kind: DictIterKind,
}

/// Dictionary iterator projection.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DictIterKind {
    /// Yield keys.
    Keys = 0,
    /// Yield values.
    Values = 1,
    /// Yield key/value pairs as compact item objects.
    Items = 2,
}

/// Builds the runtime type object for dictionaries.
#[must_use]
pub fn dict_type(type_type: *const PyType) -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut mapping = PyMappingMethods::EMPTY;
        mapping.mp_length = Some(dict_len_slot);
        mapping.mp_subscript = Some(dict_subscript_slot);
        mapping.mp_ass_subscript = Some(dict_ass_subscript_slot);

        let mut ty = PyType::new(ptr::null(), "dict", size_of::<PyDict>());
        ty.tp_as_mapping = Box::into_raw(Box::new(mapping));
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
        Box::into_raw(Box::new(ty)) as usize
    });
    let ty = *TYPE as *mut PyType;
    unsafe { install_type_type(ty, type_type) };
    ty
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

/// Initializes a freshly allocated dictionary object.
pub unsafe fn init_dict(ptr: *mut PyDict, ob_type: *const PyType, capacity: usize) {
    unsafe {
        ptr::write(
            ptr,
            PyDict {
                ob_base: PyObjectHeader::new(ob_type),
                entries: Vec::with_capacity(capacity),
                buckets: Vec::with_capacity(bucket_capacity(capacity)),
            },
        );
    }
}

/// Initializes a freshly allocated dictionary iterator.
pub unsafe fn init_dict_iter(ptr: *mut PyDictIter, ob_type: *const PyType, dict: *mut PyObject, kind: DictIterKind) {
    unsafe {
        ptr::write(
            ptr,
            PyDictIter {
                ob_base: PyObjectHeader::new(ob_type),
                dict,
                index: 0,
                kind,
            },
        );
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

/// Borrows an exact dictionary mutably.
pub unsafe fn dict_mut(object: *mut PyObject) -> Result<&'static mut PyDict, String> {
    if !unsafe { is_dict(object) } {
        return Err("expected dict object".to_owned());
    }
    Ok(unsafe { &mut *object.cast::<PyDict>() })
}

/// Borrows an exact dictionary immutably.
pub unsafe fn dict_ref(object: *mut PyObject) -> Result<&'static PyDict, String> {
    if !unsafe { is_dict(object) } {
        return Err("expected dict object".to_owned());
    }
    Ok(unsafe { &*object.cast::<PyDict>() })
}

/// Inserts or updates `key` in insertion order. Existing equal keys keep their original slot.
pub unsafe fn dict_insert(dict: *mut PyObject, key: *mut PyObject, value: *mut PyObject) -> Result<(), String> {
    if key.is_null() {
        return Err("dict key is NULL".to_owned());
    }
    if value.is_null() {
        return Err("dict value is NULL".to_owned());
    }
    let hash = unsafe { hash_dict_key(key)? };
    let dict = unsafe { dict_mut(dict)? };
    ensure_dict_buckets(dict)?;
    if let Some(index) = unsafe { find_entry_index(dict, key, hash)? } {
        dict.entries[index].value = value;
    } else {
        ensure_dict_insert_capacity(dict)?;
        let index = dict.entries.len();
        dict.entries.push(DictEntry { key, value, hash });
        insert_bucket(&mut dict.buckets, &dict.entries, index)?;
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
pub unsafe fn dict_get(dict: *mut PyObject, key: *mut PyObject) -> Result<Option<*mut PyObject>, String> {
    if key.is_null() {
        return Err("dict key is NULL".to_owned());
    }
    let hash = unsafe { hash_dict_key(key)? };
    let dict = unsafe { dict_mut(dict)? };
    ensure_dict_buckets(dict)?;
    Ok(match unsafe { find_entry_index(dict, key, hash)? } {
        Some(index) => Some(dict.entries[index].value),
        None => None,
    })
}

/// Removes and returns a dictionary value without raising on a miss.
pub unsafe fn dict_remove(dict: *mut PyObject, key: *mut PyObject) -> Result<Option<*mut PyObject>, String> {
    if key.is_null() {
        return Err("dict key is NULL".to_owned());
    }
    let hash = unsafe { hash_dict_key(key)? };
    let dict = unsafe { dict_mut(dict)? };
    ensure_dict_buckets(dict)?;
    Ok(match unsafe { find_entry_index(dict, key, hash)? } {
        Some(index) => {
            let value = dict.entries.remove(index).value;
            rebuild_dict_buckets(dict)?;
            Some(value)
        }
        None => None,
    })
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

/// Returns a stable insertion-order snapshot of dictionary entries.
pub unsafe fn dict_entries_snapshot(dict: *mut PyObject) -> Result<Vec<DictEntry>, String> {
    Ok(unsafe { dict_ref(dict)? }.entries.clone())
}

/// Returns true when two boxed objects compare equal for the Phase-B mapping key domain.
pub unsafe fn object_equal(left: *mut PyObject, right: *mut PyObject) -> Result<bool, String> {
    if left == right {
        return Ok(true);
    }
    if left.is_null() || right.is_null() {
        return Ok(false);
    }

    if let Some(equal) = unsafe { numeric_object_equal(left, right) } {
        return Ok(equal);
    }

    match (unsafe { type_name(left) }, unsafe { type_name(right) }) {
        (Some("str"), Some("str")) => {
            let l = unsafe { &*left.cast::<PyUnicode>() };
            let r = unsafe { &*right.cast::<PyUnicode>() };
            Ok(unsafe { unicode_bytes(l) == unicode_bytes(r) })
        }
        (Some("bytes"), Some("bytes")) => {
            let l = unsafe { &*left.cast::<crate::types::bytes_::PyBytes>() };
            let r = unsafe { &*right.cast::<crate::types::bytes_::PyBytes>() };
            Ok(unsafe { l.as_slice() == r.as_slice() })
        }
        (Some("frozenset"), Some("frozenset")) => crate::types::frozenset::frozenset_equal(left, right),
        (Some("tuple"), Some("tuple")) => {
            let (Some(l), Some(r)) = (
                unsafe { crate::abi::seq::exact_tuple_slice(left) },
                unsafe { crate::abi::seq::exact_tuple_slice(right) },
            ) else {
                return Ok(false);
            };
            if l.len() != r.len() {
                return Ok(false);
            }
            for (a, b) in l.iter().zip(r.iter()) {
                if !unsafe { object_equal(*a, *b)? } {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        _ => Ok(false),
    }
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
    unsafe { crate::types::complex_::to_f64s(object).map(|(real, imag)| crate::types::complex_::hash_complex(real, imag)) }
}

fn hash_object_non_numeric(object: *mut PyObject) -> Result<isize, String> {
    let hash = match unsafe { type_name(object) } {
        Some("str") => {
            let unicode = unsafe { &*object.cast::<PyUnicode>() };
            hash_bytes(unsafe { unicode_bytes(unicode) }) as isize
        }
        Some("bytes") => {
            let bytes = unsafe { &*object.cast::<crate::types::bytes_::PyBytes>() };
            hash_bytes(unsafe { bytes.as_slice() }) as isize
        }
        Some("NoneType") => 0x3456_789a_isize,
        Some("frozenset") => unsafe { crate::types::frozenset::frozenset_hash_value(object)? },
        Some("dict") => return Err("unhashable type: 'dict'".to_owned()),
        Some("set") => return Err("unhashable type: 'set'".to_owned()),
        Some("list") => return Err("unhashable type: 'list'".to_owned()),
        Some("bytearray") => return Err("unhashable type: 'bytearray'".to_owned()),
        Some("tuple") => match unsafe { crate::abi::seq::exact_tuple_slice(object) } {
            // Structural tuple hash so equal tuples built at different sites
            // collide; elements recurse through `hash_object`.
            Some(items) => {
                let mut hash: isize = 0x345678;
                let mut mult: isize = 1_000_003;
                for &item in items {
                    let item_hash = unsafe { hash_object(item)? };
                    hash = (hash ^ item_hash).wrapping_mul(mult);
                    mult = mult.wrapping_add(82_520_isize.wrapping_add(2 * items.len() as isize));
                }
                hash.wrapping_add(97_531)
            }
            // Non-PyTuple "tuple" (native representation): prior pointer
            // semantics — identity keying keeps working.
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
    Some(unsafe { core::mem::transmute::<&str, &'static str>((*ty).name()) })
}

unsafe fn find_entry_index(dict: &PyDict, key: *mut PyObject, hash: isize) -> Result<Option<usize>, String> {
    if dict.buckets.is_empty() {
        return Ok(None);
    }
    let mut bucket = bucket_index(hash, dict.buckets.len());
    for _ in 0..dict.buckets.len() {
        let Some(index) = dict.buckets[bucket] else {
            return Ok(None);
        };
        let entry = dict.entries[index];
        if entry.hash == hash && unsafe { object_equal(entry.key, key)? } {
            return Ok(Some(index));
        }
        bucket = (bucket + 1) & (dict.buckets.len() - 1);
    }
    Ok(None)
}

fn ensure_dict_buckets(dict: &mut PyDict) -> Result<(), String> {
    if dict.buckets.len() < bucket_capacity(dict.entries.len()) {
        rebuild_dict_buckets(dict)?;
    }
    Ok(())
}

fn ensure_dict_insert_capacity(dict: &mut PyDict) -> Result<(), String> {
    let occupied_after_insert = dict.entries.len().saturating_add(1);
    if occupied_after_insert.saturating_mul(3) >= dict.buckets.len().saturating_mul(2) {
        rebuild_dict_buckets_with_capacity(dict, bucket_capacity(occupied_after_insert))?;
    }
    Ok(())
}

fn rebuild_dict_buckets(dict: &mut PyDict) -> Result<(), String> {
    rebuild_dict_buckets_with_capacity(dict, bucket_capacity(dict.entries.len()))
}

fn rebuild_dict_buckets_with_capacity(dict: &mut PyDict, capacity: usize) -> Result<(), String> {
    let mut buckets = vec![None; capacity];
    for index in 0..dict.entries.len() {
        insert_bucket(&mut buckets, &dict.entries, index)?;
    }
    dict.buckets = buckets;
    Ok(())
}

fn insert_bucket(buckets: &mut [Option<usize>], entries: &[DictEntry], index: usize) -> Result<(), String> {
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

unsafe extern "C" fn dict_subscript_slot(object: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abi::map::pon_dict_get_item(object, key) }
}

unsafe extern "C" fn dict_ass_subscript_slot(object: *mut PyObject, key: *mut PyObject, value: *mut PyObject) -> c_int {
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
unsafe extern "C" fn dict_richcmp_slot(left: *mut PyObject, right: *mut PyObject, op: c_int) -> *mut PyObject {
    use crate::abstract_op::{RICH_EQ, RICH_NE};

    if !matches!(u8::try_from(op), Ok(RICH_EQ | RICH_NE)) || unsafe { !is_dict(left) || !is_dict(right) } {
        return unsafe { crate::abi::pon_not_implemented() };
    }
    match unsafe { dict_equal(left, right) } {
        Ok(equal) => {
            let truth = equal == (op == c_int::from(RICH_EQ));
            unsafe { crate::abi::pon_const_bool(c_int::from(truth)) }
        }
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
    let right_len = unsafe { dict_ref(right) }.map_err(DictEqualError::Message)?.entries.len();
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
unsafe fn dict_entries_equal(left_entries: &[DictEntry], right: *mut PyObject) -> Result<bool, DictEqualError> {
    for entry in left_entries {
        let Some(right_value) = unsafe { dict_get(right, entry.key) }.map_err(DictEqualError::Message)? else {
            return Ok(false);
        };
        // Identity implies equal before dispatch, mirroring CPython's
        // `PyObject_RichCompareBool` (observable with shared NaN values; also
        // the escape hatch that keeps shared-cycle comparisons finite).
        if entry.value == right_value {
            continue;
        }
        let equal = unsafe { crate::abstract_op::rich_compare(crate::abstract_op::RICH_EQ, entry.value, right_value) };
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

unsafe extern "C" fn dict_getattro_slot(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(attr) = (unsafe { unicode_attr_name_display(name) }) else {
        pon_err_set("dict attribute name must be str");
        return ptr::null_mut();
    };
    match attr.as_str() {
        "get" | "keys" | "values" | "items" | "setdefault" | "pop" | "update" => unsafe {
            crate::abi::map::pon_dict_bound_method(object, &attr)
        },
        // `fromkeys` is a classmethod in CPython: the receiver only supplies the
        // class, so the plain (unbound) constructor function is the right value.
        "fromkeys" => crate::native::builtins_mod::dict_fromkeys_function(),
        _ => {
            pon_err_set(format!("attribute '{attr}' was not found"));
            ptr::null_mut()
        }
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

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn normalize_hash(hash: isize) -> isize {
    if hash == -1 { -2 } else { hash }
}

unsafe fn install_type_type(ty: *mut PyType, type_type: *const PyType) {
    if !ty.is_null() && !type_type.is_null() {
        unsafe { (*ty).ob_base.ob_type = type_type };
    }
}

const _: () = {
    assert!(offset_of!(PyDict, ob_base) == 0);
    assert!(offset_of!(PyDictIter, ob_base) == 0);
};

#[cfg(test)]
mod tests {
    use num_bigint::BigInt;
    use num_traits::One;

    use super::*;
    use crate::abi::{map::pon_build_map, pon_const_int, pon_runtime_init};
    use crate::object::PyObject;
    use crate::thread_state::test_state_lock;
    use crate::types::{bool_ as bool_type, complex_ as complex_type, float as float_type, int as int_type};

    #[track_caller]
    fn assert_same_hash_and_equal(left: *mut PyObject, right: *mut PyObject) {
        unsafe {
            assert_eq!(hash_object(left).expect("left hash"), hash_object(right).expect("right hash"));
            assert!(object_equal(left, right).expect("left equals right"));
            assert!(object_equal(right, left).expect("right equals left"));
        }
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
            assert_eq!(dict_get(dict, one).expect("lookup by int after update"), Some(replacement_value));
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
            assert_eq!(dict_get(dict, lookup_key).expect("lookup by fresh BigInt"), Some(stored_value));
            assert_eq!(dict_ref(dict).expect("dict ref").entries.len(), 1);
        }
    }
}
