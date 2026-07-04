//! Sequence helper family namespace.
//!
//! Tier-0 sequence values are boxed `*mut PyObject` values.  Helpers follow the
//! runtime-wide NULL-sentinel convention: fallible object helpers set the thread
//! state's current error and return NULL, while status helpers return `-1`.

use core::ffi::c_int;
use core::mem;
use core::ptr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;

use pon_gc::{GcTypeInfo, TypeId};
use num_bigint::BigInt;

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
use crate::types::exc::ExceptionKind;

use super::{Runtime, alloc_long, catch_object_helper, catch_status_helper, return_minus_one_with_error, return_null_with_error, with_runtime};

/// Sequence lengths and indexes use the platform `isize` sentinel convention.
pub type SequenceSize = isize;

const TYPE_ID_LIST: TypeId = TypeId(21);
const TYPE_ID_TUPLE: TypeId = TypeId(22);
const TYPE_ID_RANGE: TypeId = TypeId(23);
const TYPE_ID_SLICE: TypeId = TypeId(24);
const TYPE_ID_SEQ_ITER: TypeId = TypeId(25);
/// Heap-class instances embedding list storage (`PyListSubclassInstance`);
/// class-instance family next to the dict-subclass id 107 (abi/map.rs) and
/// the payload-subclass id 108 (types/type_.rs).
const TYPE_ID_LIST_SUBCLASS_INSTANCE: TypeId = TypeId(109);
/// Heap-class instances embedding tuple storage (`PyTupleSubclassInstance`,
/// the `collections.namedtuple` substrate); next free id after the
/// list-subclass id 109.
const TYPE_ID_TUPLE_SUBCLASS_INSTANCE: TypeId = TypeId(110);

static LIST_TYPE: OnceLock<usize> = OnceLock::new();
static TUPLE_TYPE: OnceLock<usize> = OnceLock::new();
static RANGE_TYPE: OnceLock<usize> = OnceLock::new();
static SLICE_TYPE: OnceLock<usize> = OnceLock::new();
static SEQ_ITER_TYPE: OnceLock<usize> = OnceLock::new();

fn list_type() -> *mut PyType {
    LIST_TYPE.get_or_init(|| {
        let sequence = Box::leak(Box::new(PySequenceMethods {
            sq_length: Some(list_len_slot),
            sq_concat: Some(list_concat_slot),
            sq_repeat: Some(list_repeat_slot),
            sq_item: Some(list_item_slot),
            sq_ass_item: Some(list_ass_item_slot),
            sq_contains: Some(list_contains_slot),
            sq_inplace_concat: Some(list_inplace_concat_slot),
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

/// Returns the elements of an exact seq-family `PyTuple`, or `None` when
/// `object` is not one (e.g. the native builtins_mod tuple representation,
/// which shares the "tuple" type name but not the `PyTuple` layout).
pub(crate) unsafe fn exact_tuple_slice<'a>(object: *mut PyObject) -> Option<&'a [*mut PyObject]> {
    if object.is_null() || unsafe { (*object).ob_type } != tuple_type().cast_const() {
        return None;
    }
    Some(unsafe { (*object.cast::<PyTuple>()).as_slice() })
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
        ty.tp_hash = Some(range_hash_slot);
        ty.tp_richcmp = Some(range_richcmp_slot);
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
        ty.tp_getattro = Some(crate::abstract_op::iterator_dunder_getattro);
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
    runtime.heap.register_type(
        TYPE_ID_LIST_SUBCLASS_INSTANCE,
        GcTypeInfo {
            size: mem::size_of::<list::PyListSubclassInstance>(),
            trace: list::trace_list_subclass_instance,
            finalize: Some(list::finalize_list_subclass_instance),
        },
    );
    runtime.heap.register_type(
        TYPE_ID_TUPLE_SUBCLASS_INSTANCE,
        GcTypeInfo {
            size: mem::size_of::<tuple::PyTupleSubclassInstance>(),
            trace: tuple::trace_tuple_subclass_instance,
            finalize: Some(tuple::finalize_tuple_subclass_instance),
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

/// Allocates a heap instance of a list-derived class: the generic
/// heap-instance prefix plus empty embedded list storage.
pub(crate) fn alloc_list_subclass_instance(
    cls: *mut PyType,
    instance_dict: *mut type_::PyClassDict,
    slots: Vec<type_::PySlotValue>,
) -> Result<*mut PyObject, String> {
    with_runtime(|runtime| {
        register_seq_gc_types(runtime);
        let object = runtime
            .heap
            .alloc(mem::size_of::<list::PyListSubclassInstance>(), TYPE_ID_LIST_SUBCLASS_INSTANCE)
            .cast::<list::PyListSubclassInstance>();
        unsafe {
            ptr::write(
                object,
                list::PyListSubclassInstance {
                    base: type_::PyHeapInstance {
                        ob_base: PyObjectHeader::new(cls),
                        dict: instance_dict,
                        slots,
                        weakrefs: ptr::null_mut(),
                        finalized: false,
                    },
                    storage: list::PyListStorage {
                        len: 0,
                        cap: 0,
                        items: ptr::null_mut(),
                    },
                },
            );
        }
        Ok(as_object_ptr(object))
    })
    .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
}

/// Allocates a heap instance of a tuple-derived class: the generic
/// heap-instance prefix plus embedded tuple storage populated from `values`
/// (tuples are immutable — contents are fixed at construction, unlike the
/// list-subclass lane where `list.__init__` fills the empty storage later).
pub(crate) fn alloc_tuple_subclass_instance(
    cls: *mut PyType,
    instance_dict: *mut type_::PyClassDict,
    slots: Vec<type_::PySlotValue>,
    values: &[*mut PyObject],
) -> Result<*mut PyObject, String> {
    with_runtime(|runtime| {
        register_seq_gc_types(runtime);
        let items = leak_slots(values.len())?;
        if !items.is_null() {
            unsafe { copy_heap_pointer_slice(items, values) };
        }
        let object = runtime
            .heap
            .alloc(mem::size_of::<tuple::PyTupleSubclassInstance>(), TYPE_ID_TUPLE_SUBCLASS_INSTANCE)
            .cast::<tuple::PyTupleSubclassInstance>();
        unsafe {
            ptr::write(
                object,
                tuple::PyTupleSubclassInstance {
                    base: type_::PyHeapInstance {
                        ob_base: PyObjectHeader::new(cls),
                        dict: instance_dict,
                        slots,
                        weakrefs: ptr::null_mut(),
                        finalized: false,
                    },
                    storage: tuple::PyTupleStorage {
                        len: values.len(),
                        items,
                    },
                },
            );
        }
        Ok(as_object_ptr(object))
    })
    .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
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

/// Resolves the list cell block of either list layout: the exact seq-family
/// `PyList` (tail overlay) or a list-subclass heap instance (embedded
/// storage).  `None` when `object` is neither.
unsafe fn list_cells_ptr(object: *mut PyObject) -> Option<*mut list::PyListStorage> {
    if is_list(object) {
        let cells = unsafe { object.cast::<u8>().add(mem::offset_of!(PyList, len)) };
        return Some(cells.cast::<list::PyListStorage>());
    }
    if unsafe { list::is_list_subclass_instance(object) } {
        let instance = object.cast::<list::PyListSubclassInstance>();
        return Some(unsafe { ptr::addr_of_mut!((*instance).storage) });
    }
    None
}

/// Mutable view over [`list_cells_ptr`] for the single-receiver mutation
/// helpers (the exclusive-receiver contract the raw list helpers assume).
unsafe fn list_cells<'a>(object: *mut PyObject) -> Option<&'a mut list::PyListStorage> {
    unsafe { list_cells_ptr(object).map(|cells| &mut *cells) }
}

/// Returns whether `object` carries concrete list storage: an exact list or
/// a list-subclass instance.  Dispatch fast paths that must honor user
/// method overrides keep using the exact [`is_list`] check instead.
pub(crate) fn has_list_storage(object: *mut PyObject) -> bool {
    is_list(object) || unsafe { list::is_list_subclass_instance(object) }
}

fn is_tuple(object: *mut PyObject) -> bool {
    unsafe { !object.is_null() && (*object).ob_type == tuple_type().cast_const() }
}

/// Resolves the tuple cell block of either tuple layout: the exact
/// seq-family `PyTuple` (tail overlay) or a tuple-subclass heap instance
/// (embedded storage).  `None` when `object` is neither.
unsafe fn tuple_storage_ptr(object: *mut PyObject) -> Option<*mut tuple::PyTupleStorage> {
    if is_tuple(object) {
        let cells = unsafe { object.cast::<u8>().add(mem::offset_of!(PyTuple, len)) };
        return Some(cells.cast::<tuple::PyTupleStorage>());
    }
    if unsafe { tuple::is_tuple_subclass_instance(object) } {
        let instance = object.cast::<tuple::PyTupleSubclassInstance>();
        return Some(unsafe { ptr::addr_of_mut!((*instance).storage) });
    }
    None
}

/// Immutable items view over [`tuple_storage_ptr`]: the storage slice of an
/// exact tuple or a tuple-subclass instance, `None` otherwise.  Shared with
/// sibling modules (percent-format tuple spreading, dict keying).
pub(crate) unsafe fn tuple_storage_slice<'a>(object: *mut PyObject) -> Option<&'a [*mut PyObject]> {
    unsafe { tuple_storage_ptr(object).map(|cells| (&*cells).as_slice()) }
}

/// Returns whether `object` carries concrete tuple storage: an exact tuple
/// or a tuple-subclass instance.  Dispatch fast paths that must honor user
/// method overrides keep using the exact [`is_tuple`] check instead.
pub(crate) fn has_tuple_storage(object: *mut PyObject) -> bool {
    is_tuple(object) || unsafe { tuple::is_tuple_subclass_instance(object) }
}

fn is_range(object: *mut PyObject) -> bool {
    unsafe { !object.is_null() && (*object).ob_type == range_type().cast_const() }
}

/// `(len, start, step)` comparison key of an abi seq-family `PyRange`
/// (`None` for anything else), promoted to `BigInt` for the shared
/// range-equality authority in `native::builtins_mod`.
pub(crate) fn abi_range_cmp_key(object: *mut PyObject) -> Option<(BigInt, BigInt, BigInt)> {
    if !is_range(object) {
        return None;
    }
    let range = unsafe { &*object.cast::<PyRange>() };
    Some((BigInt::from(range.len), BigInt::from(range.start), BigInt::from(range.step)))
}

unsafe extern "C" fn range_richcmp_slot(left: *mut PyObject, right: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe { crate::native::builtins_mod::range_richcmp(left, right, op) }
}

unsafe extern "C" fn range_hash_slot(object: *mut PyObject) -> isize {
    crate::native::builtins_mod::range_hash_value(object).unwrap_or(object as usize as isize)
}

pub(crate) fn is_slice(object: *mut PyObject) -> bool {
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

/// Sequence subscript/store funnels: out-of-range sentinels become typed
/// IndexError, zero-step slices ValueError, non-int keys TypeError;
/// receiver invariants keep the bare diagnostic.
fn return_null_with_sequence_error(message: String) -> *mut PyObject {
    if is_sequence_index_error(&message) || message == "sequence index is out of range for this platform" {
        super::exc::raise_index_error_text(&message)
    } else if message == "slice step cannot be zero" {
        raise_typed(ExceptionKind::ValueError, &message)
    } else if message.starts_with("expected int, got ") {
        raise_typed(ExceptionKind::TypeError, &message)
    } else {
        return_null_with_error(message)
    }
}

fn return_minus_one_with_sequence_error(message: String) -> c_int {
    let _ = return_null_with_sequence_error(message);
    -1
}

/// Raises a typed builtin exception carrying the diagnostic text — unless a
/// live boxed exception is already pending (advisory Err strings such as
/// `iteration raised an exception`), which stays authoritative, mirroring
/// `pon_err_set`'s preserve discipline.
fn raise_typed(kind: ExceptionKind, message: &str) -> *mut PyObject {
    if super::exc::pending_exception_object().is_some() {
        return ptr::null_mut();
    }
    super::exc::raise_kind_error_text(kind, message)
}

/// Typed `TypeError` for arity and argument-type misuse of list/tuple
/// methods and constructors (CPython `except TypeError:` must fire).
fn raise_seq_type_error(message: impl AsRef<str>) -> *mut PyObject {
    raise_typed(ExceptionKind::TypeError, message.as_ref())
}

/// Classifies list/tuple method `Result` streams by CPython kind: the
/// index/remove miss sentinels are ValueError, out-of-range indices
/// IndexError, non-int index arguments and receiver mismatches TypeError;
/// unrecognized diagnostics (advisory iteration/comparison failures,
/// internal invariants) keep the bare fallback.
fn raise_seq_stream_error(message: String) -> *mut PyObject {
    if message == "list.index(x): x not in list" || message == "tuple.index(x): x not in tuple" {
        raise_typed(ExceptionKind::ValueError, &message)
    } else if is_sequence_index_error(&message) || message == "sequence index is out of range for this platform" {
        super::exc::raise_index_error_text(&message)
    } else if message.starts_with("expected int, got ")
        || message.contains(" expected list, got ")
        || message.contains(" expected tuple, got ")
    {
        raise_typed(ExceptionKind::TypeError, &message)
    } else {
        return_null_with_error(message)
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
    let same_kind = if tuple_kind { has_tuple_storage(right) } else { has_list_storage(right) };
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
        let Some(items) = (unsafe { tuple_storage_slice(left) }) else {
            return unsafe { super::pon_not_implemented() };
        };
        items
    } else {
        let Some(cells) = (unsafe { list_cells_ptr(left) }) else {
            return unsafe { super::pon_not_implemented() };
        };
        unsafe { (&*cells).as_slice() }
    };
    let right_items = if tuple_kind {
        let Some(items) = (unsafe { tuple_storage_slice(right) }) else {
            return unsafe { super::pon_not_implemented() };
        };
        items
    } else {
        let Some(cells) = (unsafe { list_cells_ptr(right) }) else {
            return unsafe { super::pon_not_implemented() };
        };
        unsafe { (&*cells).as_slice() }
    };

    for index in 0..left_items.len().min(right_items.len()) {
        // CPython `PyObject_RichCompareBool` identity fast path, scoped to
        // the EQ probe only: an element identical to its counterpart counts
        // as equal without calling `__eq__` (NaN parity — `[x] == [x]` is
        // True even though `x == x` is False; side-effecting `__eq__` is
        // skipped for identical objects). Small-int immediates compare
        // bitwise, matching CPython's small-int singletons. The ordering
        // fallback below still runs a real compare.
        if left_items[index] == right_items[index] {
            continue;
        }
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
    // `bool <: int`: True/False are valid everywhere an int operand is
    // (sequence indexes, slice bounds, repeat counts — CPython reads them
    // through the shared long payload).
    if let Some(value) = unsafe { crate::types::bool_::to_bool(object) } {
        return Ok(i64::from(value));
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
        if let Some(cells) = list_cells_ptr(object) {
            return Ok((*cells).len);
        }
        if let Some(items) = tuple_storage_slice(object) {
            return Ok(items.len());
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
        let Some(cells) = list_cells_ptr(object) else {
            return Err(format!("list indexing expected list, got {}", object_type_name(object)));
        };
        let list = &*cells;
        let index = normalize_index(index, list.len)?;
        Ok(*list.items.add(index))
    }
}

fn tuple_item_object(object: *mut PyObject, index: isize) -> Result<*mut PyObject, String> {
    let Some(items) = (unsafe { tuple_storage_slice(object) }) else {
        return Err(format!("tuple indexing expected tuple, got {}", object_type_name(object)));
    };
    let index = normalize_index(index, items.len())?;
    Ok(items[index])
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
    if has_list_storage(object) {
        return list_item_object(object, index);
    }
    if has_tuple_storage(object) {
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
                    // StopIteration is exhaustion; any other pending
                    // exception is a genuine error and must stay set for the
                    // caller (`pon_err_set` never replaces a live boxed
                    // exception, so the `Err` string below is advisory).
                    if !super::exc::pending_exception_is("StopIteration") {
                        return Err("iteration raised an exception".to_owned());
                    }
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

pub(crate) fn normalize_slice(slice: &PySlice, len: usize) -> Result<SliceIndices, String> {
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
    if has_tuple_storage(object) {
        Ok(crate::native::builtins_mod::alloc_tuple(values))
    } else {
        with_runtime(|runtime| alloc_list_from_slice(runtime, &values))
            .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
    }
}

fn list_resize(list: &mut list::PyListStorage, new_cap: usize) -> Result<(), String> {
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

/// Length of a runtime list, or `None` when `object` is not a list.
/// Untagged native callers only (`crate::import`'s `sys.meta_path` seeding).
pub(crate) fn list_len(object: *mut PyObject) -> Option<usize> {
    unsafe { list_cells(object) }.map(|cells| cells.len)
}

/// Appends one owned heap object to a runtime list.  `pub(crate)` for
/// `crate::import`'s `sys.meta_path` seeding; tagged Python-level appends go
/// through `pon_list_append`.
pub(crate) fn list_append_raw(list_object: *mut PyObject, item: *mut PyObject) -> Result<(), String> {
    let Some(list) = (unsafe { list_cells(list_object) }) else {
        return Err(format!("list append expected list, got {}", object_type_name(list_object)));
    };
    if item.is_null() {
        return Err("cannot append NULL to list".to_owned());
    }
    let _guard = crate::sync::begin_critical_section(list_object);
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


fn replace_list_contents(list: &mut list::PyListStorage, values: &[*mut PyObject]) -> Result<(), String> {
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
    let Some(list) = (unsafe { list_cells(list_object) }) else {
        return Err(format!("list deletion expected list, got {}", object_type_name(list_object)));
    };
    let _guard = crate::sync::begin_critical_section(list_object);
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
    if value.is_null() {
        return list_delete_index_raw(list_object, index);
    }
    let Some(list) = (unsafe { list_cells(list_object) }) else {
        return Err(format!("list assignment expected list, got {}", object_type_name(list_object)));
    };
    let _guard = crate::sync::begin_critical_section(list_object);
    let index = normalize_index(index, list.len)?;
    unsafe { crate::sync::store_heap_pointer(list.items.add(index), value) };
    Ok(())
}

fn list_assign_slice_raw(list_object: *mut PyObject, key: *mut PyObject, value: *mut PyObject) -> Result<(), String> {
    if !is_slice(key) {
        return Err("slice assignment key is not a slice".to_owned());
    }
    let Some(list) = (unsafe { list_cells(list_object) }) else {
        return Err(format!("slice assignment expected list, got {}", object_type_name(list_object)));
    };
    let _guard = crate::sync::begin_critical_section(list_object);
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
    let Some(list) = (unsafe { list_cells(list_object) }) else {
        return Err(format!("list pop expected list, got {}", object_type_name(list_object)));
    };
    let _guard = crate::sync::begin_critical_section(list_object);
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
    let Some(list) = (unsafe { list_cells_ptr(list_object) }) else {
        return Err(format!("list.index expected list, got {}", object_type_name(list_object)));
    };
    let list = unsafe { &*list };
    for (index, item) in unsafe { list.as_slice() }.iter().copied().enumerate() {
        if unsafe { crate::types::dict::object_equal(item, needle)? } {
            return Ok(index);
        }
    }
    Err("list.index(x): x not in list".to_owned())
}

fn list_count_raw(list_object: *mut PyObject, needle: *mut PyObject) -> Result<usize, String> {
    let Some(list) = (unsafe { list_cells_ptr(list_object) }) else {
        return Err(format!("list.count expected list, got {}", object_type_name(list_object)));
    };
    let list = unsafe { &*list };
    let mut count = 0usize;
    for item in unsafe { list.as_slice() }.iter().copied() {
        if unsafe { crate::types::dict::object_equal(item, needle)? } {
            count += 1;
        }
    }
    Ok(count)
}

fn tuple_index_raw(tuple_object: *mut PyObject, needle: *mut PyObject) -> Result<usize, String> {
    let Some(items) = (unsafe { tuple_storage_slice(tuple_object) }) else {
        return Err(format!("tuple.index expected tuple, got {}", object_type_name(tuple_object)));
    };
    for (index, item) in items.iter().copied().enumerate() {
        if unsafe { crate::types::dict::object_equal(item, needle)? } {
            return Ok(index);
        }
    }
    Err("tuple.index(x): x not in tuple".to_owned())
}

fn tuple_count_raw(tuple_object: *mut PyObject, needle: *mut PyObject) -> Result<usize, String> {
    let Some(items) = (unsafe { tuple_storage_slice(tuple_object) }) else {
        return Err(format!("tuple.count expected tuple, got {}", object_type_name(tuple_object)));
    };
    let mut count = 0usize;
    for item in items.iter().copied() {
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
        let receiver = match ensure_list_method_receiver(args, "append") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        if args.len() != 2 {
            return raise_seq_type_error(format!("list.append() takes exactly one argument ({} given)", args.len().saturating_sub(1)));
        }
        match list_append_raw(receiver, args[1]) {
            Ok(()) => seq_none(),
            Err(message) => raise_seq_stream_error(message),
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
            return raise_seq_type_error(format!("list.extend() takes exactly one argument ({} given)", args.len().saturating_sub(1)));
        }
        let values = match sequence_to_vec(args[1]) {
            Ok(values) => values,
            Err(message) => return raise_seq_type_error(message),
        };
        for value in values {
            if let Err(message) = list_append_raw(args[0], value) {
                return raise_seq_stream_error(message);
            }
        }
        seq_none()
    })
}

/// `sq_inplace_concat`: CPython `list_inplace_concat` — extend the receiver
/// with ANY iterable and return the SAME object (`x += (2,)` works where
/// binary `+` demands a list operand).  Consulted by `try_inplace_binary`
/// before the plain binary path, mirroring `binary_iop1`.
unsafe extern "C" fn list_inplace_concat_slot(receiver: *mut PyObject, other: *mut PyObject) -> *mut PyObject {
    let values = match sequence_to_vec(other) {
        Ok(values) => values,
        Err(message) => return raise_seq_type_error(message),
    };
    for value in values {
        if let Err(message) = list_append_raw(receiver, value) {
            return raise_seq_stream_error(message);
        }
    }
    receiver
}

unsafe extern "C" fn list_sort_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match method_args(argv, argc, "list.sort") {
        Ok(args) => args,
        Err(message) => return return_null_with_error(message),
    };
    // Keyword form (`key=`/`reverse=`): the binder flattens the keywords
    // into a trailing sort-options marker, mirroring `sorted()`.
    if args.len() == 2 {
        if let Some(options) = unsafe { crate::types::lazy_iter::sort_options_value(args[1]) } {
            return unsafe { list_sort_with_options(args[0], options.key, options.reverse) };
        }
    }
    if args.len() != 1 {
        return raise_seq_type_error(format!("list.sort expected 0 arguments, got {}", args.len().saturating_sub(1)));
    }
    unsafe { pon_list_sort(args[0]) }
}

/// In-place `list.sort(key=…, reverse=…)`.
///
/// Items are copied out, ordered by the shared `stable_sort` (key calls run
/// arbitrary Python, so no list critical section is held around them), and
/// written back.  A length change observed at write-back raises CPython's
/// "list modified during sort" ValueError.
unsafe fn list_sort_with_options(list: *mut PyObject, key: *mut PyObject, reverse: bool) -> *mut PyObject {
    let list = crate::tag::untag_arg(list);
    if list.is_null() {
        return ptr::null_mut();
    }
    if !has_list_storage(list) {
        return raise_seq_type_error(format!("list.sort expected list, got {}", object_type_name(list)));
    }
    let mut items = {
        let _guard = crate::sync::begin_critical_section(list);
        // SAFETY: `has_list_storage` proved a resolvable cell block.
        unsafe { (&*list_cells_ptr(list).expect("storage checked above")).as_slice() }.to_vec()
    };
    if crate::native::builtins_batch::stable_sort(&mut items, key, reverse).is_err() {
        return ptr::null_mut();
    }
    let _guard = crate::sync::begin_critical_section(list);
    // SAFETY: As above; the critical section serializes the write-back.
    let values = unsafe { (&mut *list_cells_ptr(list).expect("storage checked above")).as_mut_slice() };
    if values.len() != items.len() {
        let message = "list modified during sort";
        // SAFETY: Typed raise helper with a static message.
        return unsafe { super::exc::pon_raise_value_error(message.as_ptr(), message.len()) };
    }
    values.copy_from_slice(&items);
    seq_none()
}

unsafe extern "C" fn list_reverse_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.reverse") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        if args.len() != 1 {
            return raise_seq_type_error(format!("list.reverse expected 0 arguments, got {}", args.len().saturating_sub(1)));
        }
        let Some(list) = (unsafe { list_cells(args[0]) }) else {
            return raise_seq_type_error(format!("list.reverse expected list, got {}", object_type_name(args[0])));
        };
        let _guard = crate::sync::begin_critical_section(args[0]);
        unsafe { list.as_mut_slice() }.reverse();
        seq_none()
    })
}
unsafe extern "C" fn list_clear_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.clear") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_list_method_receiver(args, "clear") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        if args.len() != 1 {
            return raise_seq_type_error(format!("list.clear() takes no arguments ({} given)", args.len().saturating_sub(1)));
        }
        let Some(list) = (unsafe { list_cells(receiver) }) else {
            return raise_seq_type_error(format!("list.clear expected list, got {}", object_type_name(receiver)));
        };
        let _guard = crate::sync::begin_critical_section(receiver);
        for index in 0..list.len {
            unsafe { crate::sync::store_heap_pointer(list.items.add(index), ptr::null_mut()) };
        }
        list.len = 0;
        seq_none()
    })
}
/// `list.copy()`: a shallow copy as a fresh exact list.
unsafe extern "C" fn list_copy_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.copy") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_list_method_receiver(args, "copy") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        if args.len() != 1 {
            return raise_seq_type_error(format!("list.copy() takes no arguments ({} given)", args.len().saturating_sub(1)));
        }
        let Some(list) = (unsafe { list_cells(receiver) }) else {
            return raise_seq_type_error(format!("list.copy expected list, got {}", object_type_name(receiver)));
        };
        let mut items = {
            let _guard = crate::sync::begin_critical_section(receiver);
            unsafe { list.as_mut_slice() }.to_vec()
        };
        let argv = if items.is_empty() { ptr::null_mut() } else { items.as_mut_ptr() };
        // SAFETY: List builder reads exactly `len` live slots.
        unsafe { pon_build_list(argv, items.len()) }
    })
}


unsafe extern "C" fn list_pop_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.pop") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        if !(args.len() == 1 || args.len() == 2) {
            return raise_seq_type_error(format!("pop expected at most 1 argument, got {}", args.len().saturating_sub(1)));
        }
        let index = if args.len() == 2 {
            match index_value(args[1]) {
                Ok(index) => index,
                Err(message) => return raise_seq_stream_error(message),
            }
        } else {
            -1
        };
        match list_pop_raw(args[0], index) {
            Ok(value) => value,
            Err(message) => raise_seq_stream_error(message),
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
            return raise_seq_type_error(format!("list.index expected 1 argument, got {}", args.len().saturating_sub(1)));
        }
        match list_index_raw(args[0], args[1]) {
            Ok(index) => unsafe { super::pon_const_int(index as i64) },
            Err(message) => raise_seq_stream_error(message),
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
            return raise_seq_type_error(format!("list.count() takes exactly one argument ({} given)", args.len().saturating_sub(1)));
        }
        match list_count_raw(args[0], args[1]) {
            Ok(count) => unsafe { super::pon_const_int(count as i64) },
            Err(message) => raise_seq_stream_error(message),
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
            return raise_seq_type_error(format!("list.insert expected 2 arguments, got {}", args.len().saturating_sub(1)));
        }
        let index = match index_value(args[1]) {
            Ok(index) => index,
            Err(message) => return raise_seq_stream_error(message),
        };
        let Some(list) = (unsafe { list_cells(args[0]) }) else {
            return raise_seq_type_error(format!("list.insert expected list, got {}", object_type_name(args[0])));
        };
        let _guard = crate::sync::begin_critical_section(args[0]);
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
            return raise_seq_type_error(format!("list.remove() takes exactly one argument ({} given)", args.len().saturating_sub(1)));
        }
        let index = match list_index_raw(args[0], args[1]) {
            Ok(index) => index,
            Err(message) => return raise_seq_stream_error(message),
        };
        match list_delete_index_raw(args[0], index as isize) {
            Ok(()) => seq_none(),
            Err(message) => raise_seq_stream_error(message),
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
            return raise_seq_type_error(format!("tuple.count expected 1 argument, got {}", args.len().saturating_sub(1)));
        }
        match tuple_count_raw(args[0], args[1]) {
            Ok(count) => unsafe { super::pon_const_int(count as i64) },
            Err(message) => raise_seq_stream_error(message),
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
            return raise_seq_type_error(format!("tuple.index expected 1 argument, got {}", args.len().saturating_sub(1)));
        }
        match tuple_index_raw(args[0], args[1]) {
            Ok(index) => unsafe { super::pon_const_int(index as i64) },
            Err(message) => raise_seq_stream_error(message),
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
            // CPython seed-0 value parity, shared with the dict/set paths
            // (`crate::pyhash`): keeps tuple hashes containing str lanes
            // bit-identical to CPython.
            return Ok(crate::pyhash::str_hash(text) as isize);
        }
        if is_exact_type(object, runtime.none_type) {
            return Ok(0x9e3779b97f4a7c15_u64 as isize);
        }
        if has_tuple_storage(object) {
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
    let items = unsafe { tuple_storage_slice(object) }
        .ok_or_else(|| format!("tuple hash expected tuple, got {}", object_type_name(object)))?;
    let mut acc = 0x27d4eb2f165667c5_u64;
    for item in items {
        let lane = py_hash(*item)? as u64;
        acc = acc.wrapping_add(lane.wrapping_mul(0xc2b2ae3d27d4eb4f));
        acc = acc.rotate_left(31);
        acc = acc.wrapping_mul(0x9e3779b185ebca87);
    }
    acc = acc.wrapping_add((items.len() as u64) ^ (0x27d4eb2f165667c5_u64 ^ 3527539));
    let hash = acc as isize;
    Ok(if hash == -1 { 1546275796 } else { hash })
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
    let Some(items) = (unsafe { tuple_storage_slice(object) }) else {
        pon_err_set("tuple length slot received a non-tuple");
        return -1;
    };
    isize::try_from(items.len()).unwrap_or_else(|_| {
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
    let Some(items) = (unsafe { tuple_storage_slice(object) }) else {
        pon_err_set("tuple contains slot received a non-tuple");
        return -1;
    };
    contains_slice(items, item)
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
    let (Some(left_items), Some(right_items)) =
        (unsafe { tuple_storage_slice(left) }, unsafe { tuple_storage_slice(right) })
    else {
        return raise_seq_type_error("can only concatenate tuple to tuple");
    };
    let mut values = Vec::with_capacity(left_items.len().saturating_add(right_items.len()));
    values.extend_from_slice(left_items);
    values.extend_from_slice(right_items);
    crate::native::builtins_mod::alloc_tuple(values)
}

unsafe extern "C" fn list_concat_slot(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    // CPython `list_concat` accepts any list-layout operand (subclass
    // instances included, via `PyList_Check`); the result is an exact list.
    let Some(left_cells) = (unsafe { list_cells_ptr(left) }) else {
        let name = unsafe { crate::types::dict::type_name(left) }.unwrap_or("object");
        return raise_seq_type_error(format!("can only concatenate list (not \"{name}\") to list"));
    };
    let Some(right_cells) = (unsafe { list_cells_ptr(right) }) else {
        let name = unsafe { crate::types::dict::type_name(right) }.unwrap_or("object");
        return raise_seq_type_error(format!("can only concatenate list (not \"{name}\") to list"));
    };
    // Sequential shared reads: `left is right` (`a + a`) must not alias two
    // mutable borrows of the same storage.
    let mut values;
    {
        // SAFETY: `list_cells_ptr` proved the storage layout.
        let left_items = unsafe { (*left_cells).as_slice() };
        values = Vec::with_capacity(left_items.len().saturating_add(unsafe { (*right_cells).as_slice() }.len()));
        values.extend_from_slice(left_items);
    }
    // SAFETY: `list_cells_ptr` proved the storage layout.
    values.extend_from_slice(unsafe { (*right_cells).as_slice() });
    match with_runtime(|runtime| alloc_list_from_slice(runtime, &values)) {
        Some(Ok(object)) => object,
        Some(Err(message)) => return_null_with_error(message),
        None => return_null_with_error("runtime is not initialized"),
    }
}

unsafe extern "C" fn list_repeat_slot(object: *mut PyObject, count: *mut PyObject) -> *mut PyObject {
    if !is_list(object) {
        return raise_seq_type_error("can only repeat list");
    }
    let count = match repeat_count_value(count) {
        Ok(count) => count,
        Err(message) => return raise_seq_repeat_error(message),
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
    let Some(items) = (unsafe { tuple_storage_slice(object) }) else {
        return raise_seq_type_error("can only repeat tuple");
    };
    let count = match repeat_count_value(count) {
        Ok(count) => count,
        Err(message) => return raise_seq_repeat_error(message),
    };
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

/// Sequence-repeat count failures: non-int counts are CPython TypeError;
/// counts beyond the index range are OverflowError.
fn raise_seq_repeat_error(message: String) -> *mut PyObject {
    if message == "repeat count is out of range" {
        raise_typed(ExceptionKind::OverflowError, &message)
    } else {
        raise_typed(ExceptionKind::TypeError, &message)
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
        "clear" => bound_seq_method(object, &name, list_clear_method),
        "copy" => bound_seq_method(object, &name, list_copy_method),
        "pop" => bound_seq_method(object, &name, list_pop_method),
        "index" => bound_seq_method(object, &name, list_index_method),
        "count" => bound_seq_method(object, &name, list_count_method),
        "insert" => bound_seq_method(object, &name, list_insert_method),
        "remove" => bound_seq_method(object, &name, list_remove_method),
        _ => super::exc::raise_attribute_error_text(&format!("attribute '{name}' was not found")),
    }
}

/// Unbound-receiver validation for list method descriptors reached off the
/// type (`list.append(x, …)`): CPython raises the mismatch TypeError before
/// the method body runs.  `name` is the bare method name.  Returns the
/// untagged receiver, or the raised NULL sentinel.
fn ensure_list_method_receiver(args: &[*mut PyObject], name: &str) -> Result<*mut PyObject, *mut PyObject> {
    if args.is_empty() {
        let message = format!("unbound method list.{name}() needs an argument");
        return Err(unsafe { super::exc::pon_raise_type_error(message.as_ptr(), message.len()) });
    }
    let receiver = crate::tag::untag_arg(args[0]);
    if receiver.is_null() {
        return Err(ptr::null_mut());
    }
    if !has_list_storage(receiver) {
        let ty = unsafe { dict::type_name(receiver) }.unwrap_or("object");
        let message = format!("descriptor '{name}' for 'list' objects doesn't apply to a '{ty}' object");
        return Err(unsafe { super::exc::pon_raise_type_error(message.as_ptr(), message.len()) });
    }
    Ok(receiver)
}

/// One-shot installer for the builtin `list` type object's `tp_dict` method
/// surface, so type-level access (the `list.append(lst, x)` unbound pattern)
/// resolves through the regular MRO lookup.
///
/// `ty` is the GLOBAL `list` type object (the receiver of the type-attr
/// lookup) — distinct from the seq-family instance type returned by
/// [`list_type`]; the trampolines validate the instance layout themselves.
/// Existing `tp_dict` entries are kept: only missing names are added.
pub(crate) fn ensure_list_type_methods_installed(ty: *mut PyType) {
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if ty.is_null() || INSTALLED.load(AtomicOrdering::SeqCst) {
        return;
    }
    // Pre-runtime call sites must not latch a no-op install: the function
    // allocations below need a live runtime.
    if crate::abi::runtime_type_type().is_null() {
        return;
    }
    if INSTALLED.swap(true, AtomicOrdering::SeqCst) {
        return;
    }
    let namespace = unsafe { (*ty).tp_dict.cast::<type_::PyClassDict>() };
    let namespace = if namespace.is_null() { type_::new_namespace() } else { namespace };
    let natives: &[(&str, *const u8)] = &[
        ("append", list_append_method as *const u8),
        ("extend", list_extend_method as *const u8),
        ("sort", list_sort_method as *const u8),
        ("reverse", list_reverse_method as *const u8),
        ("clear", list_clear_method as *const u8),
        ("copy", list_copy_method as *const u8),
        ("pop", list_pop_method as *const u8),
        ("index", list_index_method as *const u8),
        ("count", list_count_method as *const u8),
        ("insert", list_insert_method as *const u8),
        ("remove", list_remove_method as *const u8),
        ("__init__", list_dunder_init as *const u8),
        ("__len__", list_dunder_len as *const u8),
        ("__getitem__", list_dunder_getitem as *const u8),
        ("__setitem__", list_dunder_setitem as *const u8),
        ("__delitem__", list_dunder_delitem as *const u8),
        ("__iter__", list_dunder_iter as *const u8),
        ("__contains__", list_dunder_contains as *const u8),
        ("__add__", list_dunder_add as *const u8),
        ("__iadd__", list_dunder_iadd as *const u8),
        ("__eq__", list_dunder_eq as *const u8),
        ("__ne__", list_dunder_ne as *const u8),
        ("__lt__", list_dunder_lt as *const u8),
        ("__le__", list_dunder_le as *const u8),
        ("__gt__", list_dunder_gt as *const u8),
        ("__ge__", list_dunder_ge as *const u8),
        ("__repr__", list_dunder_repr as *const u8),
    ];
    for (name, code) in natives {
        let interned = crate::intern::intern(name);
        if unsafe { (&*namespace).get(interned) }.is_some() {
            continue;
        }
        let function = unsafe { crate::abi::pon_make_function(*code, crate::builtins::variadic_arity(), interned) };
        if !function.is_null() {
            unsafe { (&mut *namespace).set(interned, function) };
        }
    }
    unsafe {
        (*ty).tp_dict = namespace.cast::<PyObject>();
    }
    // GC rooting for the namespace values plus IC invalidation for any
    // AttrIC guarding the type object.
    crate::sync::register_namespaced_type(ty);
    crate::sync::type_modified(ty);
}

/// Ensures the global `list` type object carries the full method/dunder
/// surface list-derived heap classes resolve through their MRO.  Idempotent;
/// called from class construction when a class linearizes over `list`.
pub(crate) fn ensure_list_subclass_surface() {
    if let Some(ty) = crate::native::builtins_mod::builtin_native_type("list") {
        ensure_list_type_methods_installed(ty);
    }
}

/// Unbound-receiver validation for tuple method descriptors reached off the
/// type (`tuple.index(t, …)`) or through a subclass MRO — mirrors
/// `ensure_list_method_receiver`.  Returns the untagged receiver, or the
/// raised NULL sentinel.
fn ensure_tuple_method_receiver(args: &[*mut PyObject], name: &str) -> Result<*mut PyObject, *mut PyObject> {
    if args.is_empty() {
        let message = format!("unbound method tuple.{name}() needs an argument");
        return Err(unsafe { super::exc::pon_raise_type_error(message.as_ptr(), message.len()) });
    }
    let receiver = crate::tag::untag_arg(args[0]);
    if receiver.is_null() {
        return Err(ptr::null_mut());
    }
    if !has_tuple_storage(receiver) {
        let ty = unsafe { dict::type_name(receiver) }.unwrap_or("object");
        let message = format!("descriptor '{name}' for 'tuple' objects doesn't apply to a '{ty}' object");
        return Err(unsafe { super::exc::pon_raise_type_error(message.as_ptr(), message.len()) });
    }
    Ok(receiver)
}

/// `tuple.__new__(cls, iterable=())` argument shape: zero or one positional
/// argument.  Returns the materialized item vector.
pub(crate) fn tuple_ctor_values(ctor_args: &[*mut PyObject]) -> Result<Vec<*mut PyObject>, String> {
    if ctor_args.len() > 1 {
        return Err(format!("tuple expected at most 1 argument, got {}", ctor_args.len()));
    }
    match ctor_args.first() {
        Some(&iterable) => sequence_to_vec(iterable),
        None => Ok(Vec::new()),
    }
}

/// `tuple.__new__` core: build the values, then allocate the layout `cls`
/// prescribes — the exact seq-family tuple for the builtin, the extended
/// subclass layout for tuple-derived heap classes.
pub(crate) fn construct_tuple_for_class(cls: *mut PyType, ctor_args: &[*mut PyObject]) -> Result<*mut PyObject, String> {
    let values = tuple_ctor_values(ctor_args)?;
    if unsafe { tuple::type_is_tuple_subclass(cls) } {
        return unsafe { type_::alloc_tuple_instance_for_class(cls, &values) };
    }
    // `cls` is the builtin itself (the global constructor type or the
    // seq-family instance type): the canonical exact tuple IS the instance.
    with_runtime(|runtime| alloc_tuple_from_slice(runtime, &values))
        .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
}

/// Python-visible `tuple.__new__(cls, iterable=())` staticmethod carrier —
/// the construction terminus `collections.namedtuple` captures at import
/// time (`_tuple_new = tuple.__new__`) and every tuple-derived class reaches
/// through `call_type_from_argv`'s MRO `__new__` lookup.  Tuples are
/// immutable, so this consumes the iterable completely; the permissive
/// `object.__init__` leg that follows never re-runs construction.
unsafe extern "C" fn tuple_dunder_new(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "tuple.__new__") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        if args.is_empty() {
            return raise_seq_type_error("tuple.__new__(): not enough arguments");
        }
        let cls = args[0];
        if unsafe { !type_::is_type_object(cls) } {
            return raise_seq_type_error("tuple.__new__(X): X is not a type object");
        }
        let cls_ty = cls.cast::<PyType>();
        let is_tuple_subtype = unsafe { tuple::type_is_tuple_subclass(cls_ty) }
            || unsafe { crate::mro::mro_entries(cls_ty) }.iter().any(|entry| {
                !entry.is_null()
                    && unsafe {
                        (**entry).gc_type_id != type_::TYPE_ID_HEAP_INSTANCE.0 as usize
                            && (**entry).name() == "tuple"
                    }
            });
        if !is_tuple_subtype {
            let cls_name = unsafe { (*cls_ty).name() };
            return raise_seq_type_error(format!("tuple.__new__({cls_name}): {cls_name} is not a subtype of tuple"));
        }
        match construct_tuple_for_class(cls_ty, &args[1..]) {
            Ok(object) => object,
            Err(message) => raise_seq_type_error(message),
        }
    })
}

/// `tuple.__len__(self)`.
unsafe extern "C" fn tuple_dunder_len(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "tuple.__len__") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_tuple_method_receiver(args, "__len__") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        match sequence_len_raw(receiver) {
            Ok(len) => unsafe { super::pon_const_int(len as i64) },
            Err(message) => return_null_with_error(message),
        }
    })
}

/// `tuple.__getitem__(self, index_or_slice)`.
unsafe extern "C" fn tuple_dunder_getitem(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "tuple.__getitem__") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_tuple_method_receiver(args, "__getitem__") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        if args.len() != 2 {
            return raise_seq_type_error(format!(
                "tuple.__getitem__ expected 1 argument, got {}",
                args.len().saturating_sub(1)
            ));
        }
        match tuple_subscript_raw(receiver, args[1]) {
            Ok(value) => value,
            Err(message) => return_null_with_sequence_error(message),
        }
    })
}

/// `tuple.__iter__(self)`: an index-tracking sequence iterator over either
/// tuple layout (`seq_iter_next_slot` reads through the widened raw helpers).
unsafe extern "C" fn tuple_dunder_iter(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "tuple.__iter__") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_tuple_method_receiver(args, "__iter__") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        match with_runtime(|runtime| alloc_seq_iter(runtime, receiver)) {
            Some(Ok(iterator)) => iterator,
            Some(Err(message)) => return_null_with_error(message),
            None => return_null_with_error("runtime is not initialized"),
        }
    })
}

/// `tuple.__contains__(self, item)`.
unsafe extern "C" fn tuple_dunder_contains(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "tuple.__contains__") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_tuple_method_receiver(args, "__contains__") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        if args.len() != 2 {
            return raise_seq_type_error(format!(
                "tuple.__contains__ expected 1 argument, got {}",
                args.len().saturating_sub(1)
            ));
        }
        match tuple_count_raw(receiver, args[1]) {
            Ok(count) => unsafe { super::number::pon_const_bool(i32::from(count != 0)) },
            Err(message) => raise_seq_stream_error(message),
        }
    })
}

/// `tuple.__eq__`/`__ne__`/`__lt__`/`__le__`/`__gt__`/`__ge__` share the
/// widened sequence comparator (lexicographic ordering; elements dispatch
/// through `rich_compare`); non-tuple operands yield NotImplemented.
unsafe fn tuple_dunder_compare(argv: *mut *mut PyObject, argc: usize, name: &str, op: u8) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, name) {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_tuple_method_receiver(args, name.trim_start_matches("tuple.")) {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        if args.len() != 2 {
            return raise_seq_type_error(format!("{name} expected 1 argument, got {}", args.len().saturating_sub(1)));
        }
        let other = crate::tag::untag_arg(args[1]);
        if other.is_null() {
            return ptr::null_mut();
        }
        unsafe { sequence_richcmp(receiver, other, c_int::from(op), true) }
    })
}

unsafe extern "C" fn tuple_dunder_eq(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { tuple_dunder_compare(argv, argc, "tuple.__eq__", RICH_EQ) }
}

unsafe extern "C" fn tuple_dunder_ne(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { tuple_dunder_compare(argv, argc, "tuple.__ne__", RICH_NE) }
}

unsafe extern "C" fn tuple_dunder_lt(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { tuple_dunder_compare(argv, argc, "tuple.__lt__", RICH_LT) }
}

unsafe extern "C" fn tuple_dunder_le(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { tuple_dunder_compare(argv, argc, "tuple.__le__", RICH_LE) }
}

unsafe extern "C" fn tuple_dunder_gt(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { tuple_dunder_compare(argv, argc, "tuple.__gt__", RICH_GT) }
}

unsafe extern "C" fn tuple_dunder_ge(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { tuple_dunder_compare(argv, argc, "tuple.__ge__", RICH_GE) }
}

/// `tuple.__hash__(self)`: the structural tuple hash (namedtuples are dict
/// keys; equal contents must collide across both layouts).
unsafe extern "C" fn tuple_dunder_hash(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "tuple.__hash__") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_tuple_method_receiver(args, "__hash__") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        match tuple_hash_impl(receiver) {
            Ok(hash) => unsafe { super::pon_const_int(hash as i64) },
            Err(message) => return_null_with_error(message),
        }
    })
}

/// `tuple.__repr__(self)`: Python tuple display over either storage layout,
/// with the single-element trailing comma (`(1,)`).
unsafe extern "C" fn tuple_dunder_repr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "tuple.__repr__") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_tuple_method_receiver(args, "__repr__") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        let Some(items) = (unsafe { tuple_storage_slice(receiver) }) else {
            return raise_seq_type_error(format!("tuple.__repr__ expected tuple, got {}", object_type_name(receiver)));
        };
        let mut out = String::from("(");
        for (index, item) in items.iter().copied().enumerate() {
            if index != 0 {
                out.push_str(", ");
            }
            match crate::native::builtins_mod::try_repr_text(item) {
                Ok(text) => out.push_str(&text),
                Err(()) => return ptr::null_mut(),
            }
        }
        if items.len() == 1 {
            out.push(',');
        }
        out.push(')');
        unsafe { crate::abi::pon_const_str(out.as_ptr(), out.len()) }
    })
}

/// `tuple.__add__(self, other)`: concatenation over either storage layout;
/// the result is always an exact tuple (CPython `sq_concat`).
unsafe extern "C" fn tuple_dunder_add(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "tuple.__add__") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_tuple_method_receiver(args, "__add__") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        if args.len() != 2 {
            return raise_seq_type_error(format!(
                "tuple.__add__ expected 1 argument, got {}",
                args.len().saturating_sub(1)
            ));
        }
        let other = crate::tag::untag_arg(args[1]);
        if other.is_null() {
            return ptr::null_mut();
        }
        unsafe { tuple_concat_slot(receiver, other) }
    })
}

/// `tuple.__mul__(self, count)` / `tuple.__rmul__(self, count)`: repetition
/// over either storage layout; the result is always an exact tuple.
unsafe fn tuple_dunder_repeat(argv: *mut *mut PyObject, argc: usize, name: &str) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, name) {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_tuple_method_receiver(args, name.trim_start_matches("tuple.")) {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        if args.len() != 2 {
            return raise_seq_type_error(format!("{name} expected 1 argument, got {}", args.len().saturating_sub(1)));
        }
        unsafe { tuple_repeat_slot(receiver, args[1]) }
    })
}

unsafe extern "C" fn tuple_dunder_mul(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { tuple_dunder_repeat(argv, argc, "tuple.__mul__") }
}

unsafe extern "C" fn tuple_dunder_rmul(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { tuple_dunder_repeat(argv, argc, "tuple.__rmul__") }
}

/// One-shot installer for the builtin `tuple` type object's `tp_dict`
/// surface: the method/dunder set tuple-derived heap classes resolve through
/// their MRO, plus the Python-visible `tuple.__new__` staticmethod carrier
/// `collections.namedtuple` captures at import time (`_tuple_new`).
///
/// `ty` is the GLOBAL `tuple` type object — distinct from the seq-family
/// instance type returned by [`tuple_type`]; the trampolines validate the
/// instance layout themselves.  Existing `tp_dict` entries are kept.
pub(crate) fn ensure_tuple_type_methods_installed(ty: *mut PyType) {
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if ty.is_null() || INSTALLED.load(AtomicOrdering::SeqCst) {
        return;
    }
    // Pre-runtime call sites must not latch a no-op install: the function
    // allocations below need a live runtime.
    if crate::abi::runtime_type_type().is_null() {
        return;
    }
    if INSTALLED.swap(true, AtomicOrdering::SeqCst) {
        return;
    }
    let namespace = unsafe { (*ty).tp_dict.cast::<type_::PyClassDict>() };
    let namespace = if namespace.is_null() { type_::new_namespace() } else { namespace };
    // `__new__` is a staticmethod carrier (CPython: implicitly static), so
    // `tuple.__new__` and `cls.__new__` lookups never bind the receiver.
    let new_name = crate::intern::intern("__new__");
    if unsafe { (&*namespace).get(new_name) }.is_none() {
        if let Ok(function) = alloc_native_seq_function("__new__", tuple_dunder_new) {
            let descriptor =
                unsafe { crate::types::classmethod::new_staticmethod(super::staticmethod_builtin_type(), function) };
            if !descriptor.is_null() {
                unsafe { (&mut *namespace).set(new_name, descriptor.cast::<PyObject>()) };
            }
        }
    }
    let natives: &[(&str, *const u8)] = &[
        ("count", tuple_count_method as *const u8),
        ("index", tuple_index_method as *const u8),
        ("__len__", tuple_dunder_len as *const u8),
        ("__getitem__", tuple_dunder_getitem as *const u8),
        ("__iter__", tuple_dunder_iter as *const u8),
        ("__contains__", tuple_dunder_contains as *const u8),
        ("__eq__", tuple_dunder_eq as *const u8),
        ("__ne__", tuple_dunder_ne as *const u8),
        ("__lt__", tuple_dunder_lt as *const u8),
        ("__le__", tuple_dunder_le as *const u8),
        ("__gt__", tuple_dunder_gt as *const u8),
        ("__ge__", tuple_dunder_ge as *const u8),
        ("__hash__", tuple_dunder_hash as *const u8),
        ("__repr__", tuple_dunder_repr as *const u8),
        ("__add__", tuple_dunder_add as *const u8),
        ("__mul__", tuple_dunder_mul as *const u8),
        ("__rmul__", tuple_dunder_rmul as *const u8),
    ];
    for (name, code) in natives {
        let interned = crate::intern::intern(name);
        if unsafe { (&*namespace).get(interned) }.is_some() {
            continue;
        }
        let function = unsafe { crate::abi::pon_make_function(*code, crate::builtins::variadic_arity(), interned) };
        if !function.is_null() {
            unsafe { (&mut *namespace).set(interned, function) };
        }
    }
    unsafe {
        (*ty).tp_dict = namespace.cast::<PyObject>();
    }
    // GC rooting for the namespace values plus IC invalidation for any
    // AttrIC guarding the type object.
    crate::sync::register_namespaced_type(ty);
    crate::sync::type_modified(ty);
}

/// Ensures the global `tuple` type object carries the method/dunder/`__new__`
/// surface tuple-derived heap classes resolve through their MRO.  Idempotent;
/// called eagerly at runtime bootstrap (collections captures `tuple.__new__`
/// at import time) and from class construction when a class linearizes over
/// `tuple`.
pub(crate) fn ensure_tuple_subclass_surface() {
    if let Some(ty) = crate::native::builtins_mod::builtin_native_type("tuple") {
        ensure_tuple_type_methods_installed(ty);
    }
}

/// `list.__init__(self, iterable=())`: replaces the receiver's contents,
/// CPython's list-init semantics for both exact lists and subclasses.
unsafe extern "C" fn list_dunder_init(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.__init__") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_list_method_receiver(args, "__init__") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        if args.len() > 2 {
            return raise_seq_type_error(format!("list expected at most 1 argument, got {}", args.len().saturating_sub(1)));
        }
        let values = if args.len() == 2 {
            match sequence_to_vec(args[1]) {
                Ok(values) => values,
                Err(message) => return raise_seq_type_error(message),
            }
        } else {
            Vec::new()
        };
        let Some(cells) = (unsafe { list_cells(receiver) }) else {
            return raise_seq_type_error(format!("list.__init__ expected list, got {}", object_type_name(receiver)));
        };
        let _guard = crate::sync::begin_critical_section(receiver);
        match replace_list_contents(cells, &values) {
            Ok(()) => seq_none(),
            Err(message) => return_null_with_error(message),
        }
    })
}

/// `list.__len__(self)`.
unsafe extern "C" fn list_dunder_len(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.__len__") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_list_method_receiver(args, "__len__") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        match sequence_len_raw(receiver) {
            Ok(len) => unsafe { super::pon_const_int(len as i64) },
            Err(message) => return_null_with_error(message),
        }
    })
}

/// `list.__getitem__(self, index_or_slice)`.
unsafe extern "C" fn list_dunder_getitem(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.__getitem__") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_list_method_receiver(args, "__getitem__") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        if args.len() != 2 {
            return raise_seq_type_error(format!("list.__getitem__ expected 1 argument, got {}", args.len().saturating_sub(1)));
        }
        match list_subscript_raw(receiver, args[1]) {
            Ok(value) => value,
            Err(message) => return_null_with_sequence_error(message),
        }
    })
}

/// `list.__setitem__(self, index_or_slice, value)`.
unsafe extern "C" fn list_dunder_setitem(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.__setitem__") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_list_method_receiver(args, "__setitem__") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        if args.len() != 3 {
            return raise_seq_type_error(format!("list.__setitem__ expected 2 arguments, got {}", args.len().saturating_sub(1)));
        }
        match list_ass_subscript_raw(receiver, args[1], args[2]) {
            Ok(()) => seq_none(),
            Err(message) => return_null_with_sequence_error(message),
        }
    })
}

/// `list.__delitem__(self, index_or_slice)`.
unsafe extern "C" fn list_dunder_delitem(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.__delitem__") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_list_method_receiver(args, "__delitem__") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        if args.len() != 2 {
            return raise_seq_type_error(format!("list.__delitem__ expected 1 argument, got {}", args.len().saturating_sub(1)));
        }
        match list_ass_subscript_raw(receiver, args[1], ptr::null_mut()) {
            Ok(()) => seq_none(),
            Err(message) => return_null_with_sequence_error(message),
        }
    })
}

/// `list.__iter__(self)`: a live index-tracking sequence iterator, so
/// mutations during iteration are observed like CPython's list_iterator.
unsafe extern "C" fn list_dunder_iter(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.__iter__") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_list_method_receiver(args, "__iter__") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        match with_runtime(|runtime| alloc_seq_iter(runtime, receiver)) {
            Some(Ok(iterator)) => iterator,
            Some(Err(message)) => return_null_with_error(message),
            None => return_null_with_error("runtime is not initialized"),
        }
    })
}

/// `list.__contains__(self, item)`.
unsafe extern "C" fn list_dunder_contains(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.__contains__") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_list_method_receiver(args, "__contains__") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        if args.len() != 2 {
            return raise_seq_type_error(format!("list.__contains__ expected 1 argument, got {}", args.len().saturating_sub(1)));
        }
        match list_count_raw(receiver, args[1]) {
            Ok(count) => unsafe { super::number::pon_const_bool(i32::from(count != 0)) },
            Err(message) => raise_seq_stream_error(message),
        }
    })
}

/// `list.__eq__`/`__ne__`/`__lt__`/`__le__`/`__gt__`/`__ge__` share the
/// widened sequence comparator (lexicographic ordering; elements dispatch
/// through `rich_compare`); non-list operands yield NotImplemented.
unsafe fn list_dunder_compare(argv: *mut *mut PyObject, argc: usize, name: &str, op: u8) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, name) {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_list_method_receiver(args, name.trim_start_matches("list.")) {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        if args.len() != 2 {
            return raise_seq_type_error(format!("{name} expected 1 argument, got {}", args.len().saturating_sub(1)));
        }
        let other = crate::tag::untag_arg(args[1]);
        if other.is_null() {
            return ptr::null_mut();
        }
        unsafe { sequence_richcmp(receiver, other, c_int::from(op), false) }
    })
}

unsafe extern "C" fn list_dunder_eq(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { list_dunder_compare(argv, argc, "list.__eq__", RICH_EQ) }
}

unsafe extern "C" fn list_dunder_ne(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { list_dunder_compare(argv, argc, "list.__ne__", RICH_NE) }
}

unsafe extern "C" fn list_dunder_lt(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { list_dunder_compare(argv, argc, "list.__lt__", RICH_LT) }
}

unsafe extern "C" fn list_dunder_le(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { list_dunder_compare(argv, argc, "list.__le__", RICH_LE) }
}

unsafe extern "C" fn list_dunder_gt(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { list_dunder_compare(argv, argc, "list.__gt__", RICH_GT) }
}

unsafe extern "C" fn list_dunder_ge(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { list_dunder_compare(argv, argc, "list.__ge__", RICH_GE) }
}

/// `list.__add__(self, other)`: dunder seam for the concat slot, so
/// list-SUBCLASS instances (whose heap types carry no `sq_concat`) resolve
/// `+` through MRO lookup exactly like CPython's inherited `list.__add__`.
unsafe extern "C" fn list_dunder_add(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.__add__") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_list_method_receiver(args, "__add__") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        if args.len() != 2 {
            return raise_seq_type_error(format!("list.__add__ expected 1 argument, got {}", args.len().saturating_sub(1)));
        }
        let other = crate::tag::untag_arg(args[1]);
        if other.is_null() {
            return ptr::null_mut();
        }
        unsafe { list_concat_slot(receiver, other) }
    })
}

/// `list.__iadd__(self, iterable)`: in-place extend with ANY iterable and
/// return the receiver (CPython `list_inplace_concat`); the dunder seam
/// gives list-subclass instances `+=` through MRO lookup.
unsafe extern "C" fn list_dunder_iadd(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.__iadd__") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_list_method_receiver(args, "__iadd__") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        if args.len() != 2 {
            return raise_seq_type_error(format!("list.__iadd__ expected 1 argument, got {}", args.len().saturating_sub(1)));
        }
        let extended = unsafe { list_extend_method(argv, argc) };
        if extended.is_null() {
            return ptr::null_mut();
        }
        receiver
    })
}

/// `list.__repr__(self)`: Python list display over the embedded storage.
unsafe extern "C" fn list_dunder_repr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        let args = match method_args(argv, argc, "list.__repr__") {
            Ok(args) => args,
            Err(message) => return return_null_with_error(message),
        };
        let receiver = match ensure_list_method_receiver(args, "__repr__") {
            Ok(receiver) => receiver,
            Err(raised) => return raised,
        };
        let items = {
            let _guard = crate::sync::begin_critical_section(receiver);
            match unsafe { list_cells_ptr(receiver) } {
                Some(cells) => unsafe { (&*cells).as_slice() }.to_vec(),
                None => return raise_seq_type_error(format!("list.__repr__ expected list, got {}", object_type_name(receiver))),
            }
        };
        let mut out = String::from("[");
        for (index, item) in items.iter().copied().enumerate() {
            if index != 0 {
                out.push_str(", ");
            }
            match crate::native::builtins_mod::try_repr_text(item) {
                Ok(text) => out.push_str(&text),
                Err(()) => return ptr::null_mut(),
            }
        }
        out.push(']');
        unsafe { crate::abi::pon_const_str(out.as_ptr(), out.len()) }
    })
}

unsafe extern "C" fn tuple_getattro_slot(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let name = match attr_name(name) {
        Ok(name) => name,
        Err(message) => return return_null_with_error(message),
    };
    match name.as_str() {
        "count" => bound_seq_method(object, &name, tuple_count_method),
        "index" => bound_seq_method(object, &name, tuple_index_method),
        _ => super::exc::raise_attribute_error_text(&format!("attribute '{name}' was not found")),
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
        let start_value = if is_none(start) { 0 } else { match long_value(start) { Ok(value) => value, Err(message) => return raise_seq_type_error(message) } };
        let stop_value = match long_value(stop) {
            Ok(value) => value,
            Err(message) => return raise_seq_type_error(message),
        };
        let step_value = if is_none(step) { 1 } else { match long_value(step) { Ok(value) => value, Err(message) => return raise_seq_type_error(message) } };
        match with_runtime(|runtime| alloc_range(runtime, start_value, stop_value, step_value)) {
            Some(Ok(object)) => object,
            Some(Err(message)) if message == "range() arg 3 must not be zero" => raise_typed(ExceptionKind::ValueError, &message),
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
            Err(message) => return raise_seq_type_error(message),
        };
        for value in values {
            if let Err(message) = list_append_raw(list, value) {
                return raise_seq_stream_error(message);
            }
        }
        list
    })
}

/// Converts a display-staging list into a tuple preserving element order.
///
/// Backs `InstKind::ListToTuple`, the final step of starred tuple displays
/// (`(*a, b)`), mirroring CPython's `INTRINSIC_LIST_TO_TUPLE`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_list_to_tuple(list: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(list);
    catch_object_helper(|| {
        if !is_list(list) {
            return return_null_with_error(format!("tuple conversion expected list, got {}", object_type_name(list)));
        }
        // SAFETY: `is_list` proved the cast; staging lists own `len` live items.
        let values: &[*mut PyObject] = unsafe {
            let list = list.cast::<PyList>();
            if (*list).items.is_null() {
                &[]
            } else {
                core::slice::from_raw_parts((*list).items.cast_const(), (*list).len)
            }
        };
        match with_runtime(|runtime| alloc_tuple_from_slice(runtime, values)) {
            Some(Ok(object)) => object,
            Some(Err(message)) => return_null_with_error(message),
            None => return_null_with_error("runtime is not initialized"),
        }
    })
}

/// Stable-sort a list in place (`lst.sort()`): the shared rich-compare
/// `stable_sort`, so tuple/list/user elements order exactly like the
/// keyword form and `sorted()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_list_sort(list: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(list);
    catch_object_helper(|| unsafe { list_sort_with_options(list, ptr::null_mut(), false) })
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

/// Returns a fresh tuple containing exactly `n` unpacked elements.  The
/// element fetches for the individual targets subscript THIS tuple, never
/// the source object (mappings/enums unpack their iteration order).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_unpack_seq(value: *mut PyObject, n: usize, feedback: *mut FeedbackCell) -> *mut PyObject {
    crate::untag_prelude!(value);
    unsafe { super::record_feedback_unary(feedback, value) };
    catch_object_helper(|| {
        let mut values = match sequence_to_vec(value) {
            Ok(values) => values,
            Err(message) => return return_null_with_error(message),
        };
        if values.len() != n {
            // Typed, catchable ValueError with CPython's wording (corpus
            // scripts wrap arity mismatches in `except ValueError:`).
            let message = if values.len() < n {
                format!("not enough values to unpack (expected {}, got {})", n, values.len())
            } else {
                format!("too many values to unpack (expected {n})")
            };
            return unsafe { super::exc::pon_raise_value_error(message.as_ptr(), message.len()) };
        }
        let ptr = if values.is_empty() { ptr::null_mut() } else { values.as_mut_ptr() };
        unsafe { pon_build_tuple(ptr, values.len()) }
    })
}

/// Returns unpack-ex results as a fresh tuple: leading items, a middle
/// list, then trailing items (`before + 1 + after` elements).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_unpack_ex(value: *mut PyObject, before: usize, after: usize) -> *mut PyObject {
    crate::untag_prelude!(value);
    catch_object_helper(|| {
        let values = match sequence_to_vec(value) {
            Ok(values) => values,
            Err(message) => return return_null_with_error(message),
        };
        let required = before.saturating_add(after);
        if values.len() < required {
            let message = format!("not enough values to unpack (expected at least {}, got {})", required, values.len());
            return unsafe { super::exc::pon_raise_value_error(message.as_ptr(), message.len()) };
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
        unsafe { pon_build_tuple(out.as_mut_ptr(), out.len()) }
    })
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
            assert_eq!(long_at(out, 0), 0);
            let middle = sequence_item_raw(out, 1).unwrap();
            assert!(is_list(middle));
            assert_eq!(sequence_len_raw(middle), Ok(3));
            assert_eq!(long_at(middle, 0), 1);
            assert_eq!(long_at(out, 2), 4);
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
    /// Native `range(start, stop, step)` object (representation a): the
    /// `NativePayload::Range` family built by the `range` builtin.
    unsafe fn native_range(start: i64, stop: i64, step: i64) -> *mut PyObject {
        let mut argv = unsafe { [pon_const_int(start), pon_const_int(stop), pon_const_int(step)] };
        let object = unsafe { crate::native::builtins_mod::builtin_range(argv.as_mut_ptr(), argv.len()) };
        assert!(!object.is_null());
        object
    }

    /// Abi seq-family `PyRange` object (representation b).
    unsafe fn abi_range(start: i64, stop: i64, step: i64) -> *mut PyObject {
        let object = unsafe { pon_build_range(pon_const_int(start), pon_const_int(stop), pon_const_int(step)) };
        assert!(!object.is_null());
        object
    }

    /// Runs `range_richcmp` for an EQ/NE selector and returns the Python
    /// truth of the result (1 true, 0 false); fails on NULL/NotImplemented.
    unsafe fn range_richcmp_truth(left: *mut PyObject, right: *mut PyObject, op: u8) -> i32 {
        let result = unsafe { crate::native::builtins_mod::range_richcmp(left, right, c_int::from(op)) };
        assert!(!result.is_null());
        assert!(!unsafe { crate::abstract_op::is_not_implemented(result) });
        unsafe { abstract_op::is_true(result) }
    }

    #[test]
    fn range_richcmp_crosses_representations() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            // Same denoted sequence [0, 2] spelled with different stops
            // (CPython range_equals normalizes: len, then start, then step).
            let abi = abi_range(0, 3, 2);
            let native = native_range(0, 4, 2);
            assert_eq!(range_richcmp_truth(abi, native, RICH_EQ), 1);
            assert_eq!(range_richcmp_truth(native, abi, RICH_EQ), 1);
            assert_eq!(range_richcmp_truth(abi, native, RICH_NE), 0);
            assert_eq!(range_richcmp_truth(native, abi, RICH_NE), 0);

            // Different denoted sequences ([0, 1, 2] vs [0, 2]) are unequal.
            let abi_dense = abi_range(0, 3, 1);
            assert_eq!(range_richcmp_truth(abi_dense, native, RICH_EQ), 0);
            assert_eq!(range_richcmp_truth(native, abi_dense, RICH_EQ), 0);
            assert_eq!(range_richcmp_truth(native, abi_dense, RICH_NE), 1);

            // Empty ranges are equal no matter how start/step are spelled.
            let abi_empty = abi_range(5, 5, 1);
            let native_empty = native_range(2, 2, 3);
            assert_eq!(range_richcmp_truth(abi_empty, native_empty, RICH_EQ), 1);
            assert_eq!(range_richcmp_truth(native_empty, abi_empty, RICH_EQ), 1);

            // Ordering selectors yield the NotImplemented singleton, never a
            // bool and never NULL, so the abstract fallback owns the TypeError.
            let lt = crate::native::builtins_mod::range_richcmp(abi, native, c_int::from(RICH_LT));
            assert!(!lt.is_null());
            assert!(crate::abstract_op::is_not_implemented(lt));
        }
    }

    #[test]
    fn range_hash_crosses_representations() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            let abi = abi_range(0, 3, 2);
            let native = native_range(0, 4, 2);
            let abi_hash = crate::native::builtins_mod::range_hash_value(abi).expect("abi range must hash");
            let native_hash = crate::native::builtins_mod::range_hash_value(native).expect("native range must hash");
            assert_eq!(abi_hash, native_hash);

            // Equal empty ranges with different start/step spellings agree too.
            let abi_empty = abi_range(5, 5, 1);
            let native_empty = native_range(2, 2, 3);
            assert_eq!(
                crate::native::builtins_mod::range_hash_value(abi_empty).expect("abi empty range must hash"),
                crate::native::builtins_mod::range_hash_value(native_empty).expect("native empty range must hash"),
            );

            // Dict-domain keying agrees across representations, so equal
            // ranges land in the same dict slot regardless of spelling.
            assert_eq!(
                crate::types::dict::hash_object(abi).expect("abi range must be dict-hashable"),
                crate::types::dict::hash_object(native).expect("native range must be dict-hashable"),
            );
        }
    }
}
