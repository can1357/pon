//! Sequence helper family namespace.
//!
//! Tier-0 sequence values are boxed `*mut PyObject` values.  Helpers follow the
//! runtime-wide NULL-sentinel convention: fallible object helpers set the thread
//! state's current error and return NULL, while status helpers return `-1`.

use core::cmp::Ordering;
use core::ffi::c_int;
use core::mem;
use core::ptr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;

use pon_gc::{GcTypeInfo, TypeId};

use crate::abstract_op::{self, RICH_EQ, RICH_GE, RICH_GT, RICH_LE, RICH_LT, RICH_NE};
use crate::feedback::FeedbackCell;
use crate::object::{PyLong, PyMappingMethods, PyObject, PyObjectHeader, PySequenceMethods, PyType, PyUnicode, as_object_ptr, is_exact_type};
use crate::thread_state::{pon_err_clear, pon_err_occurred, pon_err_set};
use crate::types::dict;
use crate::types::list::{self, PyList};
use crate::types::range_::{self, PyRange};
use crate::types::slice_::{self, PySlice, SliceIndices};
use crate::types::tuple::{self, PyTuple};
use crate::types::{method, type_};

use super::{Runtime, alloc_long, catch_object_helper, catch_status_helper, return_minus_one_with_error, return_null_with_error, with_runtime};

/// Sequence lengths and indexes use the platform `isize` sentinel convention.
pub type SequenceSize = isize;

const TYPE_ID_LIST: TypeId = TypeId(21);
const TYPE_ID_TUPLE: TypeId = TypeId(22);
const TYPE_ID_RANGE: TypeId = TypeId(23);
const TYPE_ID_SLICE: TypeId = TypeId(24);
const TYPE_ID_SEQ_ITER: TypeId = TypeId(25);

static LIST_TYPE: OnceLock<usize> = OnceLock::new();
static TUPLE_TYPE: OnceLock<usize> = OnceLock::new();
static RANGE_TYPE: OnceLock<usize> = OnceLock::new();
static SLICE_TYPE: OnceLock<usize> = OnceLock::new();
static SEQ_ITER_TYPE: OnceLock<usize> = OnceLock::new();

fn list_type() -> *mut PyType {
    LIST_TYPE.get_or_init(|| {
        let sequence = Box::leak(Box::new(PySequenceMethods {
            sq_length: Some(list_len_slot),
            sq_concat: None,
            sq_repeat: Some(list_repeat_slot),
            sq_item: Some(list_item_slot),
            sq_ass_item: Some(list_ass_item_slot),
            sq_contains: Some(list_contains_slot),
            sq_inplace_concat: None,
            sq_inplace_repeat: None,
            sq_iter: Some(seq_iter_slot),
            sq_iternext: None,
        }));
        let mapping = Box::leak(Box::new(PyMappingMethods {
            mp_length: Some(list_len_slot),
            mp_subscript: Some(list_subscript_slot),
            mp_ass_subscript: Some(list_ass_subscript_slot),
        }));
        let mut ty = PyType::new(ptr::null(), "list", mem::size_of::<PyList>());
        ty.tp_richcmp = Some(list_richcmp_slot);
        ty.tp_as_sequence = sequence;
        ty.tp_as_mapping = mapping;
        ty.tp_getattro = Some(list_getattro_slot);
        ty.gc_type_id = TYPE_ID_LIST.0 as usize;
        Box::into_raw(Box::new(ty)) as usize
    });
    (*LIST_TYPE.get().expect("list type initialized")) as *mut PyType
}

fn tuple_type() -> *mut PyType {
    TUPLE_TYPE.get_or_init(|| {
        let sequence = Box::leak(Box::new(PySequenceMethods {
            sq_length: Some(tuple_len_slot),
            sq_concat: Some(tuple_concat_slot),
            sq_repeat: Some(tuple_repeat_slot),
            sq_item: Some(tuple_item_slot),
            sq_ass_item: None,
            sq_contains: Some(tuple_contains_slot),
            sq_inplace_concat: None,
            sq_inplace_repeat: None,
            sq_iter: Some(seq_iter_slot),
            sq_iternext: None,
        }));
        let mapping = Box::leak(Box::new(PyMappingMethods {
            mp_length: Some(tuple_len_slot),
            mp_subscript: Some(tuple_subscript_slot),
            mp_ass_subscript: None,
        }));
        let mut ty = PyType::new(ptr::null(), "tuple", mem::size_of::<PyTuple>());
        ty.tp_hash = Some(tuple_hash_slot);
        ty.tp_richcmp = Some(tuple_richcmp_slot);
        ty.tp_as_sequence = sequence;
        ty.tp_as_mapping = mapping;
        ty.tp_getattro = Some(tuple_getattro_slot);
        ty.gc_type_id = TYPE_ID_TUPLE.0 as usize;
        Box::into_raw(Box::new(ty)) as usize
    });
    (*TUPLE_TYPE.get().expect("tuple type initialized")) as *mut PyType
}

fn range_type() -> *mut PyType {
    RANGE_TYPE.get_or_init(|| {
        let sequence = Box::leak(Box::new(PySequenceMethods {
            sq_length: Some(range_len_slot),
            sq_concat: None,
            sq_repeat: None,
            sq_item: Some(range_item_slot),
            sq_ass_item: None,
            sq_contains: None,
            sq_inplace_concat: None,
            sq_inplace_repeat: None,
            sq_iter: Some(seq_iter_slot),
            sq_iternext: None,
        }));
        let mapping = Box::leak(Box::new(PyMappingMethods {
            mp_length: Some(range_len_slot),
            mp_subscript: Some(range_subscript_slot),
            mp_ass_subscript: None,
        }));
        let mut ty = PyType::new(ptr::null(), "range", mem::size_of::<PyRange>());
        ty.tp_as_sequence = sequence;
        ty.tp_as_mapping = mapping;
        ty.gc_type_id = TYPE_ID_RANGE.0 as usize;
        Box::into_raw(Box::new(ty)) as usize
    });
    (*RANGE_TYPE.get().expect("range type initialized")) as *mut PyType
}

fn slice_type() -> *mut PyType {
    SLICE_TYPE.get_or_init(|| {
        let mut ty = PyType::new(ptr::null(), "slice", mem::size_of::<PySlice>());
        slice_::install_slice_slots(&mut ty);
        ty.gc_type_id = TYPE_ID_SLICE.0 as usize;
        Box::into_raw(Box::new(ty)) as usize
    });
    (*SLICE_TYPE.get().expect("slice type initialized")) as *mut PyType
}

#[repr(C)]
struct PySeqIter {
    ob_base: PyObjectHeader,
    seq: *mut PyObject,
    index: usize,
}

fn seq_iter_type() -> *mut PyType {
    SEQ_ITER_TYPE.get_or_init(|| {
        let mut ty = PyType::new(ptr::null(), "sequence_iterator", mem::size_of::<PySeqIter>());
        ty.tp_iter = Some(seq_iter_identity_slot);
        ty.tp_iternext = Some(seq_iter_next_slot);
        ty.gc_type_id = TYPE_ID_SEQ_ITER.0 as usize;
        Box::into_raw(Box::new(ty)) as usize
    });
    (*SEQ_ITER_TYPE.get().expect("sequence iterator type initialized")) as *mut PyType
}

fn register_seq_gc_types(runtime: &Runtime) {
    runtime.heap.register_type(
        TYPE_ID_LIST,
        GcTypeInfo {
            size: mem::size_of::<PyList>(),
            trace: list::trace_list,
            finalize: Some(list::finalize_list),
        },
    );
    runtime.heap.register_type(
        TYPE_ID_TUPLE,
        GcTypeInfo {
            size: mem::size_of::<PyTuple>(),
            trace: tuple::trace_tuple,
            finalize: Some(tuple::finalize_tuple),
        },
    );
    runtime.heap.register_type(
        TYPE_ID_RANGE,
        GcTypeInfo {
            size: mem::size_of::<PyRange>(),
            trace: range_::trace_range,
            finalize: None,
        },
    );
    runtime.heap.register_type(
        TYPE_ID_SLICE,
        GcTypeInfo {
            size: mem::size_of::<PySlice>(),
            trace: slice_::trace_slice,
            finalize: None,
        },
    );
    runtime.heap.register_type(
        TYPE_ID_SEQ_ITER,
        GcTypeInfo {
            size: mem::size_of::<PySeqIter>(),
            trace: trace_seq_iter,
            finalize: None,
        },
    );
}

fn leak_slots(cap: usize) -> Result<*mut *mut PyObject, String> {
    if cap == 0 {
        return Ok(ptr::null_mut());
    }
    let mut values = Vec::new();
    values.try_reserve_exact(cap).map_err(|_| "sequence backing allocation failed".to_owned())?;
    values.resize(cap, ptr::null_mut());
    let items = values.as_mut_ptr();
    mem::forget(values);
    Ok(items)
}

unsafe fn copy_heap_pointer_slice(dst: *mut *mut PyObject, values: &[*mut PyObject]) {
    for (index, value) in values.iter().copied().enumerate() {
        unsafe { crate::sync::store_heap_pointer(dst.add(index), value) };
    }
}

unsafe fn copy_heap_pointer_range(dst: *mut *mut PyObject, src: *mut *mut PyObject, len: usize) {
    for index in 0..len {
        let value = unsafe { *src.add(index) };
        unsafe { crate::sync::store_heap_pointer(dst.add(index), value) };
    }
}

fn free_slots(items: *mut *mut PyObject, cap: usize) {
    if !items.is_null() && cap != 0 {
        unsafe { drop(Vec::from_raw_parts(items, cap, cap)) };
    }
}

fn alloc_list_from_slice(runtime: &Runtime, values: &[*mut PyObject]) -> Result<*mut PyObject, String> {
    register_seq_gc_types(runtime);
    let items = leak_slots(values.len())?;
    if !items.is_null() {
        unsafe { copy_heap_pointer_slice(items, values) };
    }
    let object = runtime.heap.alloc(mem::size_of::<PyList>(), TYPE_ID_LIST).cast::<PyList>();
    unsafe {
        ptr::write(
            object,
            PyList {
                ob_base: PyObjectHeader::new(list_type().cast_const()),
                len: values.len(),
                cap: values.len(),
                items,
            },
        );
    }
    Ok(as_object_ptr(object))
}

pub(crate) fn alloc_tuple_from_slice(runtime: &Runtime, values: &[*mut PyObject]) -> Result<*mut PyObject, String> {
    register_seq_gc_types(runtime);
    let items = leak_slots(values.len())?;
    if !items.is_null() {
        unsafe { copy_heap_pointer_slice(items, values) };
    }
    let object = runtime.heap.alloc(mem::size_of::<PyTuple>(), TYPE_ID_TUPLE).cast::<PyTuple>();
    unsafe {
        ptr::write(
            object,
            PyTuple {
                ob_base: PyObjectHeader::new(tuple_type().cast_const()),
                len: values.len(),
                items,
            },
        );
    }
    Ok(as_object_ptr(object))
}

fn alloc_range(runtime: &Runtime, start: i64, stop: i64, step: i64) -> Result<*mut PyObject, String> {
    if step == 0 {
        return Err("range() arg 3 must not be zero".to_owned());
    }
    register_seq_gc_types(runtime);
    let len = range_len(start, stop, step)?;
    let object = runtime.heap.alloc(mem::size_of::<PyRange>(), TYPE_ID_RANGE).cast::<PyRange>();
    unsafe {
        ptr::write(
            object,
            PyRange {
                ob_base: PyObjectHeader::new(range_type().cast_const()),
                start,
                stop,
                step,
                len,
            },
        );
    }
    Ok(as_object_ptr(object))
}

fn alloc_slice(runtime: &Runtime, start: *mut PyObject, stop: *mut PyObject, step: *mut PyObject) -> *mut PyObject {
    register_seq_gc_types(runtime);
    let object = runtime.heap.alloc(mem::size_of::<PySlice>(), TYPE_ID_SLICE).cast::<PySlice>();
    unsafe {
        ptr::write(
            object,
            PySlice {
                ob_base: PyObjectHeader::new(slice_type().cast_const()),
                start,
                stop,
                step,
            },
        );
    }
    as_object_ptr(object)
}

fn alloc_seq_iter(runtime: &Runtime, seq: *mut PyObject) -> Result<*mut PyObject, String> {
    if seq.is_null() {
        return Err("sequence iterator received NULL sequence".to_owned());
    }
    register_seq_gc_types(runtime);
    let object = runtime.heap.alloc(mem::size_of::<PySeqIter>(), TYPE_ID_SEQ_ITER).cast::<PySeqIter>();
    unsafe {
        ptr::write(
            object,
            PySeqIter {
                ob_base: PyObjectHeader::new(seq_iter_type().cast_const()),
                seq,
                index: 0,
            },
        );
    }
    Ok(as_object_ptr(object))
}

unsafe extern "C" fn trace_seq_iter(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    let iter = unsafe { &*object.cast::<PySeqIter>() };
    if !iter.seq.is_null() {
        visitor(iter.seq.cast::<u8>());
    }
}

fn argv_as_slice<'a>(argv: *mut *mut PyObject, n: usize) -> Result<&'a [*mut PyObject], String> {
    if argv.is_null() {
        if n == 0 {
            Ok(&[])
        } else {
            Err("sequence builder received NULL argv".to_owned())
        }
    } else {
        Ok(unsafe { core::slice::from_raw_parts(argv, n) })
    }
}

fn is_list(object: *mut PyObject) -> bool {
    unsafe { !object.is_null() && (*object).ob_type == list_type().cast_const() }
}

fn is_tuple(object: *mut PyObject) -> bool {
    unsafe { !object.is_null() && (*object).ob_type == tuple_type().cast_const() }
}

fn is_range(object: *mut PyObject) -> bool {
    unsafe { !object.is_null() && (*object).ob_type == range_type().cast_const() }
}

fn is_slice(object: *mut PyObject) -> bool {
    unsafe { !object.is_null() && (*object).ob_type == slice_type().cast_const() }
}

fn is_seq_iter(object: *mut PyObject) -> bool {
    unsafe { !object.is_null() && (*object).ob_type == seq_iter_type().cast_const() }
}

fn object_type_name(object: *mut PyObject) -> String {
    if object.is_null() {
        return "NULL".to_owned();
    }
    unsafe {
        let ty = (*object).ob_type;
        if ty.is_null() {
            "<null-type>".to_owned()
        } else {
            (*ty).name().to_owned()
        }
    }
}
fn is_sequence_index_error(message: &str) -> bool {
    message == "sequence index out of range"
}

fn return_null_with_sequence_error(message: String) -> *mut PyObject {
    if is_sequence_index_error(&message) {
        super::exc::raise_index_error_text(&message)
    } else {
        return_null_with_error(message)
    }
}

fn return_minus_one_with_sequence_error(message: String) -> c_int {
    if is_sequence_index_error(&message) {
        super::exc::raise_index_error_text(&message);
        -1
    } else {
        return_minus_one_with_error(message)
    }
}


fn is_none(object: *mut PyObject) -> bool {
    with_runtime(|runtime| unsafe { is_exact_type(object, runtime.none_type) }).unwrap_or(false)
}

fn none_object() -> Result<*mut PyObject, String> {
    with_runtime(|runtime| as_object_ptr(runtime.none)).ok_or_else(|| "runtime is not initialized".to_owned())
}
unsafe extern "C" fn list_richcmp_slot(left: *mut PyObject, right: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe { sequence_richcmp(left, right, op, false) }
}

unsafe extern "C" fn tuple_richcmp_slot(left: *mut PyObject, right: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe { sequence_richcmp(left, right, op, true) }
}

unsafe fn sequence_richcmp(left: *mut PyObject, right: *mut PyObject, op: c_int, tuple_kind: bool) -> *mut PyObject {
    let same_kind = if tuple_kind { is_tuple(right) } else { is_list(right) };
    if !same_kind {
        return unsafe { super::pon_not_implemented() };
    }

    let Ok(op) = u8::try_from(op) else {
        return return_null_with_error("unknown rich comparison operation");
    };
    if !matches!(op, RICH_LT | RICH_LE | RICH_EQ | RICH_NE | RICH_GT | RICH_GE) {
        return return_null_with_error("unknown rich comparison operation");
    }

    let left_items = if tuple_kind {
        unsafe { (&*left.cast::<PyTuple>()).as_slice() }
    } else {
        unsafe { (&*left.cast::<PyList>()).as_slice() }
    };
    let right_items = if tuple_kind {
        unsafe { (&*right.cast::<PyTuple>()).as_slice() }
    } else {
        unsafe { (&*right.cast::<PyList>()).as_slice() }
    };

    for index in 0..left_items.len().min(right_items.len()) {
        let equal = unsafe { abstract_op::rich_compare(RICH_EQ, left_items[index], right_items[index]) };
        if equal.is_null() {
            return ptr::null_mut();
        }
        let truth = unsafe { abstract_op::is_true(equal) };
        if truth < 0 {
            return ptr::null_mut();
        }
        if truth == 0 {
            return match op {
                RICH_EQ => unsafe { super::number::pon_const_bool(0) },
                RICH_NE => unsafe { super::number::pon_const_bool(1) },
                RICH_LT | RICH_LE | RICH_GT | RICH_GE => unsafe {
                    abstract_op::rich_compare(op, left_items[index], right_items[index])
                },
                _ => unreachable!(),
            };
        }
    }

    let result = match op {
        RICH_EQ => left_items.len() == right_items.len(),
        RICH_NE => left_items.len() != right_items.len(),
        RICH_LT => left_items.len() < right_items.len(),
        RICH_LE => left_items.len() <= right_items.len(),
        RICH_GT => left_items.len() > right_items.len(),
        RICH_GE => left_items.len() >= right_items.len(),
        _ => unreachable!(),
    };
    unsafe { super::number::pon_const_bool(i32::from(result)) }
}
fn long_value(object: *mut PyObject) -> Result<i64, String> {
    if object.is_null() {
        return Err("integer operand is NULL".to_owned());
    }
    let Some(result) = with_runtime(|runtime| unsafe {
        if is_exact_type(object, runtime.long_type) {
            Ok((*object.cast::<PyLong>()).value)
        } else {
            Err(format!("expected int, got {}", object_type_name(object)))
        }
    }) else {
        return Err("runtime is not initialized".to_owned());
    };
    result
}

fn index_value(object: *mut PyObject) -> Result<isize, String> {
    isize::try_from(long_value(object)?).map_err(|_| "sequence index is out of range for this platform".to_owned())
}

fn normalize_index(index: isize, len: usize) -> Result<usize, String> {
    let len_isize = isize::try_from(len).map_err(|_| "sequence is too large for this platform".to_owned())?;
    let adjusted = if index < 0 { index.saturating_add(len_isize) } else { index };
    if adjusted < 0 || adjusted >= len_isize {
        Err("sequence index out of range".to_owned())
    } else {
        Ok(adjusted as usize)
    }
}

fn sequence_len_raw(object: *mut PyObject) -> Result<usize, String> {
    object_len_raw(object, false)
}

fn object_len_raw(object: *mut PyObject, allow_mapping: bool) -> Result<usize, String> {
    if object.is_null() {
        return Err("sequence is NULL".to_owned());
    }
    unsafe {
        if is_list(object) {
            return Ok((*object.cast::<PyList>()).len);
        }
        if is_tuple(object) {
            return Ok((*object.cast::<PyTuple>()).len);
        }
        if is_range(object) {
            return Ok((*object.cast::<PyRange>()).len);
        }
        let ty = (*object).ob_type;
        if !ty.is_null() {
            if let Some(slot) = (*ty).tp_as_sequence.as_ref().and_then(|methods| methods.sq_length) {
                let len = slot(object);
                if len < 0 {
                    if !pon_err_occurred() {
                        pon_err_set("sequence length slot returned a negative value");
                    }
                    return Err("sequence length failed".to_owned());
                }
                return usize::try_from(len).map_err(|_| "sequence length is out of range".to_owned());
            }
            if allow_mapping {
                if let Some(slot) = (*ty).tp_as_mapping.as_ref().and_then(|methods| methods.mp_length) {
                    let len = slot(object);
                    if len < 0 {
                        if !pon_err_occurred() {
                            pon_err_set("mapping length slot returned a negative value");
                        }
                        return Err("mapping length failed".to_owned());
                    }
                    return usize::try_from(len).map_err(|_| "mapping length is out of range".to_owned());
                }
            }
        }
    }
    if allow_mapping {
        Err(format!("object of type {} has no length", object_type_name(object)))
    } else {
        Err(format!("object of type {} is not a sequence", object_type_name(object)))
    }
}

fn list_item_object(object: *mut PyObject, index: isize) -> Result<*mut PyObject, String> {
    unsafe {
        let list = &*object.cast::<PyList>();
        let index = normalize_index(index, list.len)?;
        Ok(*list.items.add(index))
    }
}

fn tuple_item_object(object: *mut PyObject, index: isize) -> Result<*mut PyObject, String> {
    unsafe {
        let tuple = &*object.cast::<PyTuple>();
        let index = normalize_index(index, tuple.len)?;
        Ok(*tuple.items.add(index))
    }
}

fn range_item_value(range: &PyRange, index: isize) -> Result<i64, String> {
    let index = normalize_index(index, range.len)?;
    let offset = i64::try_from(index).map_err(|_| "range index is too large".to_owned())?;
    range
        .start
        .checked_add(offset.checked_mul(range.step).ok_or_else(|| "range item overflow".to_owned())?)
        .ok_or_else(|| "range item overflow".to_owned())
}

fn range_item_object(object: *mut PyObject, index: isize) -> Result<*mut PyObject, String> {
    let value = unsafe { range_item_value(&*object.cast::<PyRange>(), index)? };
    with_runtime(|runtime| alloc_long(runtime, value))
        .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
}

fn sequence_item_raw(object: *mut PyObject, index: isize) -> Result<*mut PyObject, String> {
    if object.is_null() {
        return Err("sequence is NULL".to_owned());
    }
    if is_list(object) {
        return list_item_object(object, index);
    }
    if is_tuple(object) {
        return tuple_item_object(object, index);
    }
    if is_range(object) {
        return range_item_object(object, index);
    }
    unsafe {
        let ty = (*object).ob_type;
        if !ty.is_null() {
            if let Some(slot) = (*ty).tp_as_sequence.as_ref().and_then(|methods| methods.sq_item) {
                let result = slot(object, index);
                if result.is_null() {
                    if !pon_err_occurred() {
                        pon_err_set("sequence item slot returned NULL without setting an exception");
                    }
                    return Err("sequence item failed".to_owned());
                }
                return Ok(result);
            }
        }
    }
    Err(format!("object of type {} does not support sequence indexing", object_type_name(object)))
}

pub(crate) fn sequence_to_vec(object: *mut PyObject) -> Result<Vec<*mut PyObject>, String> {
    let iter = unsafe { super::iter::pon_get_iter(object, ptr::null_mut()) };
    if !iter.is_null() {
        let mut out = Vec::new();
        loop {
            let value = unsafe { super::iter::pon_iter_next(iter, ptr::null_mut()) };
            if value.is_null() {
                if pon_err_occurred() {
                    pon_err_clear();
                }
                break;
            }
            out.push(value);
        }
        return Ok(out);
    }
    if pon_err_occurred() {
        pon_err_clear();
    }

    let len = sequence_len_raw(object)?;
    let mut out = Vec::new();
    out.try_reserve_exact(len)
        .map_err(|_| "sequence unpack allocation failed".to_owned())?;
    for index in 0..len {
        out.push(sequence_item_raw(object, index as isize)?);
    }
    Ok(out)
}

fn range_len(start: i64, stop: i64, step: i64) -> Result<usize, String> {
    if step == 0 {
        return Err("range() arg 3 must not be zero".to_owned());
    }
    let len = if step > 0 {
        if start >= stop {
            0
        } else {
            let diff = i128::from(stop) - i128::from(start) - 1;
            diff / i128::from(step) + 1
        }
    } else if start <= stop {
        0
    } else {
        let diff = i128::from(start) - i128::from(stop) - 1;
        diff / i128::from(-step) + 1
    };
    usize::try_from(len).map_err(|_| "range is too large".to_owned())
}

fn normalize_slice_bound(value: *mut PyObject, len: isize, default_none: isize, lower: isize, upper: isize) -> Result<isize, String> {
    if is_none(value) {
        return Ok(default_none.clamp(lower, upper));
    }
    let mut value = index_value(value)?;
    if value < 0 {
        value = value.saturating_add(len);
    }
    Ok(value.clamp(lower, upper))
}

fn normalize_slice(slice: &PySlice, len: usize) -> Result<SliceIndices, String> {
    let len = isize::try_from(len).map_err(|_| "sequence is too large for slice indices".to_owned())?;
    let step = if is_none(slice.step) { 1 } else { index_value(slice.step)? };
    if step == 0 {
        return Err("slice step cannot be zero".to_owned());
    }

    let (start, stop) = if step > 0 {
        (
            normalize_slice_bound(slice.start, len, 0, 0, len)?,
            normalize_slice_bound(slice.stop, len, len, 0, len)?,
        )
    } else {
        (
            normalize_slice_bound(slice.start, len, len - 1, -1, len - 1)?,
            normalize_slice_bound(slice.stop, len, -1, -1, len - 1)?,
        )
    };

    let slice_len = if step > 0 {
        if stop <= start {
            0
        } else {
            ((stop - start - 1) / step + 1) as usize
        }
    } else if stop >= start {
        0
    } else {
        ((start - stop - 1) / (-step) + 1) as usize
    };

    Ok(SliceIndices {
        start,
        stop,
        step,
        len: slice_len,
    })
}

fn sliced_values(object: *mut PyObject, indices: SliceIndices) -> Result<Vec<*mut PyObject>, String> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(indices.len)
        .map_err(|_| "slice result allocation failed".to_owned())?;
    let mut index = indices.start;
    for _ in 0..indices.len {
        values.push(sequence_item_raw(object, index)?);
        index = index.saturating_add(indices.step);
    }
    Ok(values)
}

fn sequence_slice_raw(object: *mut PyObject, key: *mut PyObject) -> Result<*mut PyObject, String> {
    if !is_slice(key) {
        return Err("slice key is not a slice".to_owned());
    }
    let len = sequence_len_raw(object)?;
    let indices = unsafe { normalize_slice(&*key.cast::<PySlice>(), len)? };
    let values = sliced_values(object, indices)?;
    if is_tuple(object) {
        Ok(crate::native::builtins_mod::alloc_tuple(values))
    } else {
        with_runtime(|runtime| alloc_list_from_slice(runtime, &values))
            .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
    }
}

fn list_resize(list: &mut PyList, new_cap: usize) -> Result<(), String> {
    if new_cap == list.cap {
        return Ok(());
    }
    if new_cap < list.len {
        return Err("cannot shrink list backing below length".to_owned());
    }
    let new_items = leak_slots(new_cap)?;
    if list.len != 0 {
        unsafe { copy_heap_pointer_range(new_items, list.items, list.len) };
    }
    free_slots(list.items, list.cap);
    list.items = new_items;
    list.cap = new_cap;
    Ok(())
}

fn list_append_raw(list_object: *mut PyObject, item: *mut PyObject) -> Result<(), String> {
    if !is_list(list_object) {
        return Err(format!("list append expected list, got {}", object_type_name(list_object)));
    }
    if item.is_null() {
        return Err("cannot append NULL to list".to_owned());
    }
    let _guard = crate::sync::begin_critical_section(list_object);
    let list = unsafe { &mut *list_object.cast::<PyList>() };
    if list.len == list.cap {
        let new_cap = if list.cap == 0 { 4 } else { list.cap.saturating_mul(2) };
        if new_cap <= list.cap {
            return Err("list is too large".to_owned());
        }
        list_resize(list, new_cap)?;
    }
    unsafe { crate::sync::store_heap_pointer(list.items.add(list.len), item) };
    list.len += 1;
    Ok(())
}


fn replace_list_contents(list: &mut PyList, values: &[*mut PyObject]) -> Result<(), String> {
    let new_items = leak_slots(values.len())?;
    if !new_items.is_null() {
        unsafe { copy_heap_pointer_slice(new_items, values) };
    }
    free_slots(list.items, list.cap);
    list.items = new_items;
    list.len = values.len();
    list.cap = values.len();
    Ok(())
}

fn list_delete_index_raw(list_object: *mut PyObject, index: isize) -> Result<(), String> {
    if !is_list(list_object) {
        return Err(format!("list deletion expected list, got {}", object_type_name(list_object)));
    }
    let _guard = crate::sync::begin_critical_section(list_object);
    let list = unsafe { &mut *list_object.cast::<PyList>() };
    let index = normalize_index(index, list.len)?;
    unsafe {
        for pos in index..list.len - 1 {
            let shifted = *list.items.add(pos + 1);
            crate::sync::store_heap_pointer(list.items.add(pos), shifted);
        }
        crate::sync::store_heap_pointer(list.items.add(list.len - 1), ptr::null_mut());
    }
    list.len -= 1;
    Ok(())
}

fn list_set_index_raw(list_object: *mut PyObject, index: isize, value: *mut PyObject) -> Result<(), String> {
    if !is_list(list_object) {
        return Err(format!("list assignment expected list, got {}", object_type_name(list_object)));
    }
    if value.is_null() {
        return list_delete_index_raw(list_object, index);
    }
    let _guard = crate::sync::begin_critical_section(list_object);
    let list = unsafe { &mut *list_object.cast::<PyList>() };
    let index = normalize_index(index, list.len)?;
    unsafe { crate::sync::store_heap_pointer(list.items.add(index), value) };
    Ok(())
}

fn list_assign_slice_raw(list_object: *mut PyObject, key: *mut PyObject, value: *mut PyObject) -> Result<(), String> {
    if !is_list(list_object) {
        return Err(format!("slice assignment expected list, got {}", object_type_name(list_object)));
    }
    if !is_slice(key) {
        return Err("slice assignment key is not a slice".to_owned());
    }
    let _guard = crate::sync::begin_critical_section(list_object);
    let list = unsafe { &mut *list_object.cast::<PyList>() };
    let indices = unsafe { normalize_slice(&*key.cast::<PySlice>(), list.len)? };
    let mut current = unsafe { list.as_slice() }.to_vec();

    if value.is_null() {
        let mut positions = Vec::with_capacity(indices.len);
        let mut pos = indices.start;
        for _ in 0..indices.len {
            positions.push(pos as usize);
            pos = pos.saturating_add(indices.step);
        }
        positions.sort_unstable_by(|a, b| b.cmp(a));
        for pos in positions {
            current.remove(pos);
        }
        return replace_list_contents(list, &current);
    }

    let replacement = sequence_to_vec(value)?;
    if indices.step == 1 {
        let start = indices.start as usize;
        let stop = indices.stop as usize;
        current.splice(start..stop, replacement);
        return replace_list_contents(list, &current);
    }

    if replacement.len() != indices.len {
        return Err(format!(
            "attempt to assign sequence of size {} to extended slice of size {}",
            replacement.len(), indices.len
        ));
    }
    let mut pos = indices.start;
    for item in replacement {
        current[pos as usize] = item;
        pos = pos.saturating_add(indices.step);
    }
    replace_list_contents(list, &current)
}

fn list_subscript_raw(object: *mut PyObject, key: *mut PyObject) -> Result<*mut PyObject, String> {
    if is_slice(key) {
        sequence_slice_raw(object, key)
    } else {
        sequence_item_raw(object, index_value(key)?)
    }
}

fn list_ass_subscript_raw(object: *mut PyObject, key: *mut PyObject, value: *mut PyObject) -> Result<(), String> {
    if is_slice(key) {
        list_assign_slice_raw(object, key, value)
    } else {
        list_set_index_raw(object, index_value(key)?, value)
    }
}

fn tuple_subscript_raw(object: *mut PyObject, key: *mut PyObject) -> Result<*mut PyObject, String> {
    if is_slice(key) {
        sequence_slice_raw(object, key)
    } else {
        sequence_item_raw(object, index_value(key)?)
    }
}

fn range_subscript_raw(object: *mut PyObject, key: *mut PyObject) -> Result<*mut PyObject, String> {
    if is_slice(key) {
        sequence_slice_raw(object, key)
    } else {
        sequence_item_raw(object, index_value(key)?)
    }
}

fn list_pop_raw(list_object: *mut PyObject, index: isize) -> Result<*mut PyObject, String> {
    if !is_list(list_object) {
        return Err(format!("list pop expected list, got {}", object_type_name(list_object)));
    }
    let _guard = crate::sync::begin_critical_section(list_object);
    let list = unsafe { &mut *list_object.cast::<PyList>() };
    let index = normalize_index(index, list.len)?;
    let value = unsafe { *list.items.add(index) };
    unsafe {
        for pos in index..list.len - 1 {
            let shifted = *list.items.add(pos + 1);
            crate::sync::store_heap_pointer(list.items.add(pos), shifted);
        }
        crate::sync::store_heap_pointer(list.items.add(list.len - 1), ptr::null_mut());
    }
    list.len -= 1;
    Ok(value)
}

fn list_index_raw(list_object: *mut PyObject, needle: *mut PyObject) -> Result<usize, String> {
    if !is_list(list_object) {
        return Err(format!("list.index expected list, got {}", object_type_name(list_object)));
    }
    let list = unsafe { &*list_object.cast::<PyList>() };
    for (index, item) in unsafe { list.as_slice() }.iter().copied().enumerate() {
        if unsafe { crate::types::dict::object_equal(item, needle)? } {
            return Ok(index);
        }
    }
    Err("list.index(x): x not in list".to_owned())
}

fn list_count_raw(list_object: *mut PyObject, needle: *mut PyObject) -> Result<usize, String> {
    if !is_list(list_object) {
        return Err(format!("list.count expected list, got {}", object_type_name(list_object)));
    }
    let list = unsafe { &*list_object.cast::<PyList>() };
    let mut count = 0usize;
    for item in unsafe { list.as_slice() }.iter().copied() {
        if unsafe { crate::types::dict::object_equal(item, needle)? } {
            count += 1;
        }
    }
    Ok(count)
}

fn tuple_index_raw(tuple_object: *mut PyObject, needle: *mut PyObject) -> Result<usize, String> {
    if !is_tuple(tuple_object) {
        return Err(format!("tuple.index expected tuple, got {}", object_type_name(tuple_object)));
    }
    let tuple = unsafe { &*tuple_object.cast::<PyTuple>() };
    for (index, item) in unsafe { tuple.as_slice() }.iter().copied().enumerate() {
        if unsafe { crate::types::dict::object_equal(item, needle)? } {
            return Ok(index);
        }
    }
    Err("tuple.index(x): x not in tuple".to_owned())
}

fn tuple_count_raw(tuple_object: *mut PyObject, needle: *mut PyObject) -> Result<usize, String> {
    if !is_tuple(tuple_object) {
        return Err(format!("tuple.count expected tuple, got {}", object_type_name(tuple_object)));
    }
    let tuple = unsafe { &*tuple_object.cast::<PyTuple>() };
    let mut count = 0usize;
    for item in unsafe { tuple.as_slice() }.iter().copied() {
        if unsafe { crate::types::dict::object_equal(item, needle)? } {
            count += 1;
        }
    }
    Ok(count)
}

fn attr_name(name: *mut PyObject) -> Result<String, String> {
    unsafe { type_::unicode_text(name) }
        .map(ToOwned::to_owned)
        .ok_or_else(|| "attribute name must be str".to_owned())
}

fn native_method_arity() -> usize {
    crate::builtins::variadic_arity()
}

fn alloc_native_seq_function(
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Result<*mut PyObject, String> {
    let name_interned = crate::intern::intern(name);
    with_runtime(|runtime| super::alloc_function(runtime, entry as *const u8, native_method_arity(), name_interned))
        .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
}

fn bound_seq_method(
    receiver: *mut PyObject,
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> *mut PyObject {
    let function = match alloc_native_seq_function(name, entry) {
        Ok(function) => function,
        Err(message) => return return_null_with_error(message),
    };
    match method::new_bound_method(function, receiver) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => return_null_with_error(message),
    }
}

fn method_args<'a>(argv: *mut *mut PyObject, argc: usize, name: &str) -> Result<&'a [*mut PyObject], String> {
    if argv.is_null() && argc != 0 {
        return Err(format!("{name} received a NULL argv pointer"));
    }
    Ok(if argc == 0 { &[] } else { unsafe { core::slice::from_raw_parts(argv, argc) } })
}

fn seq_none() -> *mut PyObject {
    match none_object() {
        Ok(none) => none,
        Err(message) => return_null_with_error(message),
    }
}

unsafe extern "C" fn list_append_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.append") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        if args.len() != 2 {
            return return_null_with_error(format!("list.append expected 1 argument, got {}", args.len().saturating_sub(1)));
        }
        match list_append_raw(args[0], args[1]) {
            Ok(()) => seq_none(),
            Err(message) => return_null_with_error(message),
        }
    })
}

unsafe extern "C" fn list_extend_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.extend") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        if args.len() != 2 {
            return return_null_with_error(format!("list.extend expected 1 argument, got {}", args.len().saturating_sub(1)));
        }
        let values = match sequence_to_vec(args[1]) {
            Ok(values) => values,
            Err(message) => return return_null_with_error(message),
        };
        for value in values {
            if let Err(message) = list_append_raw(args[0], value) {
                return return_null_with_error(message);
            }
        }
        seq_none()
    })
}

unsafe extern "C" fn list_sort_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match method_args(argv, argc, "list.sort") {
        Ok(args) => args,
        Err(message) => return return_null_with_error(message),
    };
    if args.len() != 1 {
        return return_null_with_error(format!("list.sort expected 0 arguments, got {}", args.len().saturating_sub(1)));
    }
    unsafe { pon_list_sort(args[0]) }
}

unsafe extern "C" fn list_reverse_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.reverse") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        if args.len() != 1 {
            return return_null_with_error(format!("list.reverse expected 0 arguments, got {}", args.len().saturating_sub(1)));
        }
        if !is_list(args[0]) {
            return return_null_with_error(format!("list.reverse expected list, got {}", object_type_name(args[0])));
        }
        let _guard = crate::sync::begin_critical_section(args[0]);
        let list = unsafe { &mut *args[0].cast::<PyList>() };
        unsafe { list.as_mut_slice() }.reverse();
        seq_none()
    })
}

unsafe extern "C" fn list_pop_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.pop") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        if !(args.len() == 1 || args.len() == 2) {
            return return_null_with_error(format!("list.pop expected at most 1 argument, got {}", args.len().saturating_sub(1)));
        }
        let index = if args.len() == 2 {
            match index_value(args[1]) {
                Ok(index) => index,
                Err(message) => return return_null_with_error(message),
            }
        } else {
            -1
        };
        match list_pop_raw(args[0], index) {
            Ok(value) => value,
            Err(message) => return_null_with_error(message),
        }
    })
}

unsafe extern "C" fn list_index_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.index") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        if args.len() != 2 {
            return return_null_with_error(format!("list.index expected 1 argument, got {}", args.len().saturating_sub(1)));
        }
        match list_index_raw(args[0], args[1]) {
            Ok(index) => unsafe { super::pon_const_int(index as i64) },
            Err(message) => return_null_with_error(message),
        }
    })
}

unsafe extern "C" fn list_count_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.count") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        if args.len() != 2 {
            return return_null_with_error(format!("list.count expected 1 argument, got {}", args.len().saturating_sub(1)));
        }
        match list_count_raw(args[0], args[1]) {
            Ok(count) => unsafe { super::pon_const_int(count as i64) },
            Err(message) => return_null_with_error(message),
        }
    })
}

unsafe extern "C" fn list_insert_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.insert") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        if args.len() != 3 {
            return return_null_with_error(format!("list.insert expected 2 arguments, got {}", args.len().saturating_sub(1)));
        }
        let index = match index_value(args[1]) {
            Ok(index) => index,
            Err(message) => return return_null_with_error(message),
        };
        if !is_list(args[0]) {
            return return_null_with_error(format!("list.insert expected list, got {}", object_type_name(args[0])));
        }
        let _guard = crate::sync::begin_critical_section(args[0]);
        let list = unsafe { &mut *args[0].cast::<PyList>() };
        let len = match isize::try_from(list.len) {
            Ok(len) => len,
            Err(_) => return return_null_with_error("list length is too large"),
        };
        let pos = index.clamp(0, len) as usize;
        let mut values = unsafe { list.as_slice() }.to_vec();
        values.insert(pos, args[2]);
        match replace_list_contents(list, &values) {
            Ok(()) => seq_none(),
            Err(message) => return_null_with_error(message),
        }
    })
}

unsafe extern "C" fn list_remove_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.remove") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        if args.len() != 2 {
            return return_null_with_error(format!("list.remove expected 1 argument, got {}", args.len().saturating_sub(1)));
        }
        let index = match list_index_raw(args[0], args[1]) {
            Ok(index) => index,
            Err(message) => return return_null_with_error(message),
        };
        match list_delete_index_raw(args[0], index as isize) {
            Ok(()) => seq_none(),
            Err(message) => return_null_with_error(message),
        }
    })
}

unsafe extern "C" fn tuple_count_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "tuple.count") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        if args.len() != 2 {
            return return_null_with_error(format!("tuple.count expected 1 argument, got {}", args.len().saturating_sub(1)));
        }
        match tuple_count_raw(args[0], args[1]) {
            Ok(count) => unsafe { super::pon_const_int(count as i64) },
            Err(message) => return_null_with_error(message),
        }
    })
}

unsafe extern "C" fn tuple_index_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "tuple.index") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        if args.len() != 2 {
            return return_null_with_error(format!("tuple.index expected 1 argument, got {}", args.len().saturating_sub(1)));
        }
        match tuple_index_raw(args[0], args[1]) {
            Ok(index) => unsafe { super::pon_const_int(index as i64) },
            Err(message) => return_null_with_error(message),
        }
    })
}

fn py_hash(object: *mut PyObject) -> Result<isize, String> {
    if object.is_null() {
        return Err("cannot hash NULL".to_owned());
    }
    with_runtime(|runtime| unsafe {
        if is_exact_type(object, runtime.long_type) {
            let value = (*object.cast::<PyLong>()).value;
            return Ok(if value == -1 { -2 } else { value as isize });
        }
        if is_exact_type(object, runtime.unicode_type) {
            let unicode = &*object.cast::<PyUnicode>();
            let text = unicode
                .as_str()
                .ok_or_else(|| "unicode object contains invalid UTF-8".to_owned())?;
            let mut hash = 0xcbf29ce484222325_u64;
            for byte in text.as_bytes() {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x100000001b3);
            }
            let value = hash as isize;
            return Ok(if value == -1 { -2 } else { value });
        }
        if is_exact_type(object, runtime.none_type) {
            return Ok(0x9e3779b97f4a7c15_u64 as isize);
        }
        if is_tuple(object) {
            let hash = tuple_hash_impl(object)?;
            return Ok(hash);
        }
        let ty = (*object).ob_type;
        if !ty.is_null() {
            if let Some(slot) = (*ty).tp_hash {
                let hash = slot(object);
                if hash == -1 && pon_err_occurred() {
                    return Err("hash slot failed".to_owned());
                }
                return Ok(hash);
            }
        }
        Err(format!("unhashable type: '{}'", object_type_name(object)))
    })
    .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
}

fn tuple_hash_impl(object: *mut PyObject) -> Result<isize, String> {
    let tuple = unsafe { &*object.cast::<PyTuple>() };
    let mut acc = 0x27d4eb2f165667c5_u64;
    for item in unsafe { tuple.as_slice() } {
        let lane = py_hash(*item)? as u64;
        acc = acc.wrapping_add(lane.wrapping_mul(0xc2b2ae3d27d4eb4f));
        acc = acc.rotate_left(31);
        acc = acc.wrapping_mul(0x9e3779b185ebca87);
    }
    acc = acc.wrapping_add((tuple.len as u64) ^ (0x27d4eb2f165667c5_u64 ^ 3527539));
    let hash = acc as isize;
    Ok(if hash == -1 { 1546275796 } else { hash })
}

fn compare_simple(left: *mut PyObject, right: *mut PyObject) -> Result<Ordering, String> {
    with_runtime(|runtime| unsafe {
        if is_exact_type(left, runtime.long_type) && is_exact_type(right, runtime.long_type) {
            return Ok((*left.cast::<PyLong>()).value.cmp(&(*right.cast::<PyLong>()).value));
        }
        if is_exact_type(left, runtime.unicode_type) && is_exact_type(right, runtime.unicode_type) {
            let l = (*left.cast::<PyUnicode>())
                .as_str()
                .ok_or_else(|| "left unicode object contains invalid UTF-8".to_owned())?;
            let r = (*right.cast::<PyUnicode>())
                .as_str()
                .ok_or_else(|| "right unicode object contains invalid UTF-8".to_owned())?;
            return Ok(l.cmp(r));
        }
        Err(format!(
            "'<' not supported between instances of '{}' and '{}'",
            object_type_name(left),
            object_type_name(right)
        ))
    })
    .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
}

fn validate_sortable(values: &[*mut PyObject]) -> Result<(), String> {
    for window in values.windows(2) {
        let _ = compare_simple(window[0], window[1])?;
    }
    Ok(())
}

unsafe extern "C" fn list_len_slot(object: *mut PyObject) -> isize {
    if !is_list(object) {
        pon_err_set("list length slot received a non-list");
        return -1;
    }
    let len = unsafe { (*object.cast::<PyList>()).len };
    isize::try_from(len).unwrap_or_else(|_| {
        pon_err_set("list length exceeds isize");
        -1
    })
}

unsafe extern "C" fn tuple_len_slot(object: *mut PyObject) -> isize {
    if !is_tuple(object) {
        pon_err_set("tuple length slot received a non-tuple");
        return -1;
    }
    let len = unsafe { (*object.cast::<PyTuple>()).len };
    isize::try_from(len).unwrap_or_else(|_| {
        pon_err_set("tuple length exceeds isize");
        -1
    })
}

unsafe extern "C" fn range_len_slot(object: *mut PyObject) -> isize {
    if !is_range(object) {
        pon_err_set("range length slot received a non-range");
        return -1;
    }
    let len = unsafe { (*object.cast::<PyRange>()).len };
    isize::try_from(len).unwrap_or_else(|_| {
        pon_err_set("range length exceeds isize");
        -1
    })
}

unsafe extern "C" fn list_contains_slot(object: *mut PyObject, item: *mut PyObject) -> c_int {
    if !is_list(object) {
        pon_err_set("list contains slot received a non-list");
        return -1;
    }
    let list = unsafe { &*object.cast::<PyList>() };
    contains_slice(unsafe { list.as_slice() }, item)
}

unsafe extern "C" fn tuple_contains_slot(object: *mut PyObject, item: *mut PyObject) -> c_int {
    if !is_tuple(object) {
        pon_err_set("tuple contains slot received a non-tuple");
        return -1;
    }
    let tuple = unsafe { &*object.cast::<PyTuple>() };
    contains_slice(unsafe { tuple.as_slice() }, item)
}

fn contains_slice(items: &[*mut PyObject], item: *mut PyObject) -> c_int {
    for entry in items.iter().copied() {
        match unsafe { dict::object_equal(entry, item) } {
            Ok(true) => return 1,
            Ok(false) => {}
            Err(message) => {
                pon_err_set(message);
                return -1;
            }
        }
    }
    0
}

unsafe extern "C" fn list_item_slot(object: *mut PyObject, index: isize) -> *mut PyObject {
    match list_item_object(object, index) {
        Ok(value) => value,
        Err(message) => return_null_with_error(message),
    }
}

unsafe extern "C" fn tuple_item_slot(object: *mut PyObject, index: isize) -> *mut PyObject {
    match tuple_item_object(object, index) {
        Ok(value) => value,
        Err(message) => return_null_with_error(message),
    }
}

unsafe extern "C" fn range_item_slot(object: *mut PyObject, index: isize) -> *mut PyObject {
    match range_item_object(object, index) {
        Ok(value) => value,
        Err(message) => return_null_with_error(message),
    }
}

unsafe extern "C" fn tuple_concat_slot(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    if !is_tuple(left) || !is_tuple(right) {
        return return_null_with_error("can only concatenate tuple to tuple");
    }
    let left_items = unsafe { (&*left.cast::<PyTuple>()).as_slice() };
    let right_items = unsafe { (&*right.cast::<PyTuple>()).as_slice() };
    let mut values = Vec::with_capacity(left_items.len().saturating_add(right_items.len()));
    values.extend_from_slice(left_items);
    values.extend_from_slice(right_items);
    crate::native::builtins_mod::alloc_tuple(values)
}

unsafe extern "C" fn list_repeat_slot(object: *mut PyObject, count: *mut PyObject) -> *mut PyObject {
    if !is_list(object) {
        return return_null_with_error("can only repeat list");
    }
    let count = match repeat_count_value(count) {
        Ok(count) => count,
        Err(message) => return return_null_with_error(message),
    };
    let items = unsafe { (&*object.cast::<PyList>()).as_slice() };
    let values = match repeated_values(items, count) {
        Ok(values) => values,
        Err(message) => return return_null_with_error(message),
    };
    match with_runtime(|runtime| alloc_list_from_slice(runtime, &values)) {
        Some(Ok(object)) => object,
        Some(Err(message)) => return_null_with_error(message),
        None => return_null_with_error("runtime is not initialized"),
    }
}

unsafe extern "C" fn tuple_repeat_slot(object: *mut PyObject, count: *mut PyObject) -> *mut PyObject {
    if !is_tuple(object) {
        return return_null_with_error("can only repeat tuple");
    }
    let count = match repeat_count_value(count) {
        Ok(count) => count,
        Err(message) => return return_null_with_error(message),
    };
    let items = unsafe { (&*object.cast::<PyTuple>()).as_slice() };
    let values = match repeated_values(items, count) {
        Ok(values) => values,
        Err(message) => return return_null_with_error(message),
    };
    crate::native::builtins_mod::alloc_tuple(values)
}

fn repeat_count_value(count: *mut PyObject) -> Result<usize, String> {
    let index = unsafe { super::number::pon_index(count) };
    if index.is_null() {
        return Err("repeat count must be an integer".to_owned());
    }
    let count = long_value(index)?;
    if count <= 0 {
        Ok(0)
    } else {
        usize::try_from(count).map_err(|_| "repeat count is out of range".to_owned())
    }
}

fn repeated_values(items: &[*mut PyObject], count: usize) -> Result<Vec<*mut PyObject>, String> {
    let total = items.len().checked_mul(count).ok_or_else(|| "repeated sequence is too large".to_owned())?;
    let mut out = Vec::with_capacity(total);
    for _ in 0..count {
        out.extend_from_slice(items);
    }
    Ok(out)
}

unsafe extern "C" fn list_getattro_slot(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let name = match attr_name(name) {
        Ok(name) => name,
        Err(message) => return return_null_with_error(message),
    };
    match name.as_str() {
        "append" => bound_seq_method(object, &name, list_append_method),
        "extend" => bound_seq_method(object, &name, list_extend_method),
        "sort" => bound_seq_method(object, &name, list_sort_method),
        "reverse" => bound_seq_method(object, &name, list_reverse_method),
        "pop" => bound_seq_method(object, &name, list_pop_method),
        "index" => bound_seq_method(object, &name, list_index_method),
        "count" => bound_seq_method(object, &name, list_count_method),
        "insert" => bound_seq_method(object, &name, list_insert_method),
        "remove" => bound_seq_method(object, &name, list_remove_method),
        _ => return_null_with_error(format!("attribute '{name}' was not found")),
    }
}

unsafe extern "C" fn tuple_getattro_slot(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let name = match attr_name(name) {
        Ok(name) => name,
        Err(message) => return return_null_with_error(message),
    };
    match name.as_str() {
        "count" => bound_seq_method(object, &name, tuple_count_method),
        "index" => bound_seq_method(object, &name, tuple_index_method),
        _ => return_null_with_error(format!("attribute '{name}' was not found")),
    }
}

unsafe extern "C" fn seq_iter_slot(object: *mut PyObject) -> *mut PyObject {
    match with_runtime(|runtime| alloc_seq_iter(runtime, object)) {
        Some(Ok(iterator)) => iterator,
        Some(Err(message)) => return_null_with_error(message),
        None => return_null_with_error("runtime is not initialized"),
    }
}

unsafe extern "C" fn seq_iter_identity_slot(object: *mut PyObject) -> *mut PyObject {
    if !is_seq_iter(object) {
        return return_null_with_error("sequence iterator slot received a non-iterator");
    }
    object
}

unsafe extern "C" fn seq_iter_next_slot(object: *mut PyObject) -> *mut PyObject {
    if !is_seq_iter(object) {
        return return_null_with_error("sequence iterator next slot received a non-iterator");
    }
    let seq = unsafe { (*object.cast::<PySeqIter>()).seq };
    let _guard = crate::sync::begin_critical_section2(object, seq);
    let iter = unsafe { &mut *object.cast::<PySeqIter>() };
    let len = match sequence_len_raw(iter.seq) {
        Ok(len) => len,
        Err(message) => return return_null_with_error(message),
    };
    if iter.index >= len {
        return unsafe { super::exc::pon_raise_stop_iteration(ptr::null_mut()) };
    }
    let index = match isize::try_from(iter.index) {
        Ok(index) => index,
        Err(_) => return return_null_with_error("sequence iterator index exceeds isize"),
    };
    let value = match sequence_item_raw(iter.seq, index) {
        Ok(value) => value,
        Err(message) => return return_null_with_error(message),
    };
    iter.index += 1;
    value
}

unsafe extern "C" fn list_ass_item_slot(object: *mut PyObject, index: isize, value: *mut PyObject) -> c_int {
    match list_set_index_raw(object, index, value) {
        Ok(()) => 0,
        Err(message) => return_minus_one_with_sequence_error(message),
    }
}

unsafe extern "C" fn list_subscript_slot(object: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    match list_subscript_raw(object, key) {
        Ok(value) => value,
        Err(message) => return_null_with_sequence_error(message),
    }
}

unsafe extern "C" fn tuple_subscript_slot(object: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    match tuple_subscript_raw(object, key) {
        Ok(value) => value,
        Err(message) => return_null_with_sequence_error(message),
    }
}

unsafe extern "C" fn range_subscript_slot(object: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    match range_subscript_raw(object, key) {
        Ok(value) => value,
        Err(message) => return_null_with_sequence_error(message),
    }
}

unsafe extern "C" fn list_ass_subscript_slot(object: *mut PyObject, key: *mut PyObject, value: *mut PyObject) -> c_int {
    match list_ass_subscript_raw(object, key, value) {
        Ok(()) => 0,
        Err(message) => return_minus_one_with_sequence_error(message),
    }
}

unsafe extern "C" fn tuple_hash_slot(object: *mut PyObject) -> isize {
    match tuple_hash_impl(object) {
        Ok(hash) => hash,
        Err(message) => return_minus_one_with_error(message) as isize,
    }
}

/// Builds a Python list from `n` boxed values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_build_list(argv: *mut *mut PyObject, n: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let values = match argv_as_slice(argv, n) {
            Ok(values) => values,
            Err(message) => return return_null_with_error(message),
        };
        match with_runtime(|runtime| alloc_list_from_slice(runtime, values)) {
            Some(Ok(object)) => object,
            Some(Err(message)) => return_null_with_error(message),
            None => return_null_with_error("runtime is not initialized"),
        }
    })
}

/// Builds a Python tuple from `n` boxed values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_build_tuple(argv: *mut *mut PyObject, n: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let values = match argv_as_slice(argv, n) {
            Ok(values) => values,
            Err(message) => return return_null_with_error(message),
        };
        match with_runtime(|runtime| alloc_tuple_from_slice(runtime, values)) {
            Some(Ok(object)) => object,
            Some(Err(message)) => return_null_with_error(message),
            None => return_null_with_error("runtime is not initialized"),
        }
    })
}

/// Builds a Python `range(start, stop, step)` object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_build_range(start: *mut PyObject, stop: *mut PyObject, step: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(start, stop, step);
    catch_object_helper(|| {
        let start_value = if is_none(start) { 0 } else { match long_value(start) { Ok(value) => value, Err(message) => return return_null_with_error(message) } };
        let stop_value = match long_value(stop) {
            Ok(value) => value,
            Err(message) => return return_null_with_error(message),
        };
        let step_value = if is_none(step) { 1 } else { match long_value(step) { Ok(value) => value, Err(message) => return return_null_with_error(message) } };
        match with_runtime(|runtime| alloc_range(runtime, start_value, stop_value, step_value)) {
            Some(Ok(object)) => object,
            Some(Err(message)) => return_null_with_error(message),
            None => return_null_with_error("runtime is not initialized"),
        }
    })
}

/// Builds a Python slice object.  NULL bounds are canonicalized to `None`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_build_slice(start: *mut PyObject, stop: *mut PyObject, step: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(start, stop, step);
    catch_object_helper(|| {
        let none = match none_object() {
            Ok(value) => value,
            Err(message) => return return_null_with_error(message),
        };
        let start = if start.is_null() { none } else { start };
        let stop = if stop.is_null() { none } else { stop };
        let step = if step.is_null() { none } else { step };
        match with_runtime(|runtime| alloc_slice(runtime, start, stop, step)) {
            Some(object) => object,
            None => return_null_with_error("runtime is not initialized"),
        }
    })
}

/// Appends `value` to `list` and returns the list on success.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_list_append(list: *mut PyObject, value: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(list, value);
    catch_object_helper(|| match list_append_raw(list, value) {
        Ok(()) => list,
        Err(message) => return_null_with_error(message),
    })
}

/// Extends `list` with every item in `iterable` and returns the list on success.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_list_extend(list: *mut PyObject, iterable: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(list, iterable);
    catch_object_helper(|| {
        let values = match sequence_to_vec(iterable) {
            Ok(values) => values,
            Err(message) => return return_null_with_error(message),
        };
        for value in values {
            if let Err(message) = list_append_raw(list, value) {
                return return_null_with_error(message);
            }
        }
        list
    })
}

/// Stable-sort a list containing simple comparable tier-0 values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_list_sort(list: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(list);
    catch_object_helper(|| {
        if !is_list(list) {
            return return_null_with_error(format!("list.sort expected list, got {}", object_type_name(list)));
        }
        let _guard = crate::sync::begin_critical_section(list);
        let pylist = unsafe { &mut *list.cast::<PyList>() };
        let values = unsafe { pylist.as_mut_slice() };
        if let Err(message) = validate_sortable(values) {
            return return_null_with_error(message);
        }
        values.sort_by(|left, right| compare_simple(*left, *right).unwrap_or(Ordering::Equal));
        match none_object() {
            Ok(none) => none,
            Err(message) => return_null_with_error(message),
        }
    })
}

/// Returns the length of a sequence as a C `isize` status value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_seq_len(object: *mut PyObject) -> isize {
    crate::untag_prelude!(err = -1; object);
    match catch_unwind(AssertUnwindSafe(|| sequence_len_raw(object))) {
        Ok(Ok(len)) => isize::try_from(len).unwrap_or_else(|_| return_minus_one_with_error("sequence length exceeds isize") as isize),
        Ok(Err(message)) => return_minus_one_with_error(message) as isize,
        Err(_) => return_minus_one_with_error("sequence length helper panicked") as isize,
    }
}

/// Returns `len(object)` as a boxed integer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_get_len(object: *mut PyObject, feedback: *mut FeedbackCell) -> *mut PyObject {
    crate::untag_prelude!(object);
    unsafe { super::record_feedback_unary(feedback, object) };
    catch_object_helper(|| {
        let len = match object_len_raw(object, true) {
            Ok(len) => len,
            Err(message) => return return_null_with_error(message),
        };
        let Ok(len) = i64::try_from(len) else {
            return return_null_with_error("sequence length exceeds i64");
        };
        match with_runtime(|runtime| alloc_long(runtime, len)) {
            Some(Ok(value)) => value,
            Some(Err(message)) => return_null_with_error(message),
            None => return_null_with_error("runtime is not initialized"),
        }
    })
}

/// Returns `object[index_or_slice]` through sequence semantics.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_seq_get_item(object: *mut PyObject, index_or_slice: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(object, index_or_slice);
    catch_object_helper(|| {
        if is_slice(index_or_slice) {
            match sequence_slice_raw(object, index_or_slice) {
                Ok(value) => value,
                Err(message) => return_null_with_sequence_error(message),
            }
        } else {
            match sequence_item_raw(object, match index_value(index_or_slice) { Ok(index) => index, Err(message) => return return_null_with_error(message) }) {
                Ok(value) => value,
                Err(message) => return_null_with_sequence_error(message),
            }
        }
    })
}

/// Stores `value` into `object[index_or_slice]`; NULL value means deletion.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_seq_set_item(object: *mut PyObject, index_or_slice: *mut PyObject, value: *mut PyObject) -> i32 {
    crate::untag_prelude!(err = -1; object, index_or_slice, value);
    catch_status_helper(|| match list_ass_subscript_raw(object, index_or_slice, value) {
        Ok(()) => 0,
        Err(message) => return_minus_one_with_sequence_error(message),
    })
}

/// Deletes `object[index_or_slice]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_seq_del_item(object: *mut PyObject, index_or_slice: *mut PyObject) -> i32 {
    crate::untag_prelude!(err = -1; object, index_or_slice);
    unsafe { pon_seq_set_item(object, index_or_slice, ptr::null_mut()) }
}

/// Returns a newly allocated C array containing exactly `n` unpacked elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_unpack_seq(value: *mut PyObject, n: usize, feedback: *mut FeedbackCell) -> *mut *mut PyObject {
    crate::untag_prelude!(value);
    unsafe { super::record_feedback_unary(feedback, value) };
    catch_object_helper(|| {
        let values = match sequence_to_vec(value) {
            Ok(values) => values,
            Err(message) => return return_null_with_error(message),
        };
        if values.len() != n {
            return return_null_with_error(format!("too {} values to unpack (expected {})", if values.len() < n { "few" } else { "many" }, n));
        }
        leak_result_array(values)
    }) as *mut *mut PyObject
}

/// Returns unpack-ex results: leading items, a middle list, then trailing items.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_unpack_ex(value: *mut PyObject, before: usize, after: usize) -> *mut *mut PyObject {
    crate::untag_prelude!(value);
    catch_object_helper(|| {
        let values = match sequence_to_vec(value) {
            Ok(values) => values,
            Err(message) => return return_null_with_error(message),
        };
        let required = before.saturating_add(after);
        if values.len() < required {
            return return_null_with_error(format!("not enough values to unpack (expected at least {}, got {})", required, values.len()));
        }
        let middle_end = values.len() - after;
        let starred = match with_runtime(|runtime| alloc_list_from_slice(runtime, &values[before..middle_end])) {
            Some(Ok(object)) => object,
            Some(Err(message)) => return return_null_with_error(message),
            None => return return_null_with_error("runtime is not initialized"),
        };
        let mut out = Vec::with_capacity(required + 1);
        out.extend_from_slice(&values[..before]);
        out.push(starred);
        out.extend_from_slice(&values[middle_end..]);
        leak_result_array(out)
    }) as *mut *mut PyObject
}

fn leak_result_array(mut values: Vec<*mut PyObject>) -> *mut PyObject {
    if values.is_empty() {
        return ptr::null_mut();
    }
    let ptr = values.as_mut_ptr();
    mem::forget(values);
    ptr.cast::<PyObject>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::{pon_const_int, pon_const_str, pon_none, pon_runtime_init};
    use crate::thread_state::test_state_lock;

    unsafe fn long_at(list: *mut PyObject, index: isize) -> i64 {
        let item = sequence_item_raw(list, index).unwrap();
        unsafe { (*item.cast::<PyLong>()).value }
    }

    #[test]
    fn list_append_index_and_reverse_slice() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            let mut items = [pon_const_int(1), pon_const_int(2), pon_const_int(3)];
            let list = pon_build_list(items.as_mut_ptr(), items.len());
            assert!(!list.is_null());
            assert_eq!(pon_list_append(list, pon_const_int(4)), list);
            assert_eq!(long_at(list, -1), 4);

            let slice = pon_build_slice(pon_none(), pon_none(), pon_const_int(-1));
            let reversed = pon_seq_get_item(list, slice);
            assert!(!reversed.is_null());
            assert_eq!(sequence_len_raw(reversed), Ok(4));
            assert_eq!(long_at(reversed, 0), 4);
            assert_eq!(long_at(reversed, 3), 1);
        }
    }

    #[test]
    fn list_full_reverse_slice_preserves_length_and_order() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            let mut items = [
                pon_const_int(10),
                pon_const_int(20),
                pon_const_int(30),
                pon_const_int(40),
            ];
            let list = pon_build_list(items.as_mut_ptr(), items.len());
            assert!(!list.is_null());

            let reversed_slice = pon_build_slice(pon_none(), pon_none(), pon_const_int(-1));
            let reversed = pon_seq_get_item(list, reversed_slice);
            assert!(!reversed.is_null());
            assert_eq!(sequence_len_raw(reversed), Ok(4));
            assert_eq!(long_at(reversed, 0), 40);
            assert_eq!(long_at(reversed, 1), 30);
            assert_eq!(long_at(reversed, 2), 20);
            assert_eq!(long_at(reversed, 3), 10);

            let full_slice = pon_build_slice(pon_none(), pon_none(), pon_const_int(1));
            let copied = pon_seq_get_item(list, full_slice);
            assert!(!copied.is_null());
            assert_eq!(sequence_len_raw(copied), Ok(4));
            assert_eq!(long_at(copied, 0), 10);
            assert_eq!(long_at(copied, 1), 20);
            assert_eq!(long_at(copied, 2), 30);
            assert_eq!(long_at(copied, 3), 40);
        }
    }

    #[test]
    fn list_sync_mutator_paths_compile_and_update_state() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            let mut items = [pon_const_int(3), pon_const_int(1)];
            let list = pon_build_list(items.as_mut_ptr(), items.len());
            assert!(!list.is_null());

            assert_eq!(pon_list_append(list, pon_const_int(2)), list);
            assert_eq!(sequence_len_raw(list), Ok(3));
            assert_eq!(pon_seq_set_item(list, pon_const_int(0), pon_const_int(4)), 0);
            assert_eq!(long_at(list, 0), 4);
            assert_eq!(pon_seq_del_item(list, pon_const_int(1)), 0);
            assert_eq!(sequence_len_raw(list), Ok(2));
            assert!(!pon_list_sort(list).is_null());
            assert_eq!(long_at(list, 0), 2);
            assert_eq!(long_at(list, 1), 4);
        }
    }

    #[test]
    fn unpack_ex_range_builds_middle_list() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            let range = pon_build_range(pon_const_int(0), pon_const_int(5), pon_const_int(1));
            let out = pon_unpack_ex(range, 1, 1);
            assert!(!out.is_null());
            let values = core::slice::from_raw_parts(out, 3);
            assert_eq!((*values[0].cast::<PyLong>()).value, 0);
            assert!(is_list(values[1]));
            assert_eq!(sequence_len_raw(values[1]), Ok(3));
            assert_eq!(long_at(values[1], 0), 1);
            assert_eq!((*values[2].cast::<PyLong>()).value, 4);
        }
    }

    #[test]
    fn tuple_hash_is_content_stable() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            let mut a = [pon_const_int(1), pon_const_str(b"x".as_ptr(), 1)];
            let t1 = pon_build_tuple(a.as_mut_ptr(), a.len());
            let t2 = pon_build_tuple(a.as_mut_ptr(), a.len());
            assert_eq!(tuple_hash_impl(t1).unwrap(), tuple_hash_impl(t2).unwrap());
        }
    }

    #[test]
    fn sort_is_stable_for_equal_simple_values() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            let first = pon_const_int(2);
            let second = pon_const_int(1);
            let third = pon_const_int(2);
            let mut items = [first, second, third];
            let list = pon_build_list(items.as_mut_ptr(), items.len());
            assert!(!pon_list_sort(list).is_null());
            let sorted = (*list.cast::<PyList>()).as_slice();
            assert_eq!(sorted[0], second);
            assert_eq!(sorted[1], first);
            assert_eq!(sorted[2], third);
        }
    }
}
