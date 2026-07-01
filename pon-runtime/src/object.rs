//! Boxed Python object and type-slot layouts for the Phase-B runtime.
//!
//! The model intentionally mirrors CPython's object-header shape while omitting
//! reference-count storage: ownership is delegated to `pon-gc`, and every value
//! crossing the compiled-code ABI is a boxed `*mut PyObject` whose first field is the common
//! header.

use core::cell::UnsafeCell;
use core::ffi::{c_int, c_void};
use core::mem::{offset_of, size_of};
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU8, AtomicU32};

use crate::intern;
use crate::feedback::FeedbackVec;

/// Per-object GC metadata reserved for the stop-the-world heap and later
/// free-threaded coordination.
///
/// This is not a reference count.  It is deliberately one machine word so the
/// GIL/default object header remains byte-identical to the pre-Phase-E layout:
/// `ob_type` followed immediately by this flags word.  Future free-threading
/// object-state bits must be carved out of `flags` rather than adding fields to
/// [`PyObjectHeader`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GcMeta {
    /// Reserved collector/free-threading bits.  Runtime code must not encode ownership here.
    pub flags: usize,
}

impl GcMeta {
    /// Empty metadata for newly initialized objects.
    pub const EMPTY: Self = Self { flags: 0 };
}

/// Header present at offset zero in every concrete boxed value.
#[repr(C)]
#[derive(Debug)]
pub struct PyObjectHeader {
    /// Runtime type descriptor for dispatch and diagnostics.
    pub ob_type: *const PyType,
    /// Stop-the-world GC metadata slot; it is not a reference-count field.
    pub gc_meta: GcMeta,
}

impl PyObjectHeader {
    /// Builds a header for a concrete object of `ob_type`.
    #[must_use]
    pub const fn new(ob_type: *const PyType) -> Self {
        Self {
            ob_type,
            gc_meta: GcMeta::EMPTY,
        }
    }
}

/// The ABI base type for boxed values.
///
/// Pointers to concrete values are passed through compiled code as
/// `*mut PyObject`; the header is the full prefix shared by all concrete object
/// layouts.
pub type PyObject = PyObjectHeader;

/// Python unary slot returning an object or NULL with a thread-state error.
pub type UnaryFunc = unsafe extern "C" fn(*mut PyObject) -> *mut PyObject;
/// Python binary slot returning an object or NULL with a thread-state error.
pub type BinaryFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject;
/// Python ternary slot returning an object or NULL with a thread-state error.
pub type TernaryFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject;
/// Python length slot.  Negative values are errors when a thread-state error is set.
pub type LenFunc = unsafe extern "C" fn(*mut PyObject) -> isize;
/// Python truth-value slot returning `0` or `1`, or `-1` with a thread-state error.
pub type InquiryFunc = unsafe extern "C" fn(*mut PyObject) -> c_int;
/// Python hash slot.  `-1` is an error when a thread-state error is set.
pub type HashFunc = unsafe extern "C" fn(*mut PyObject) -> isize;
/// Python rich-comparison slot; the final argument is a CPython-compatible comparison op.
pub type RichCmpFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, c_int) -> *mut PyObject;
/// Attribute lookup slot.
pub type GetAttrFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject;
/// Attribute assignment/deletion slot.  A NULL value means deletion.
pub type SetAttrFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int;
/// Callable slot.  The second and third arguments are tuple/list-like positional and keyword carriers.
pub type CallFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject;
/// Descriptor `__get__` slot.
pub type DescrGetFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject;
/// Descriptor `__set__`/`__delete__` slot.  A NULL value means deletion.
pub type DescrSetFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int;
/// Object initializer slot.
pub type InitFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int;
/// Object allocator/constructor slot.
pub type NewFunc = unsafe extern "C" fn(*mut PyType, *mut PyObject, *mut PyObject) -> *mut PyObject;
/// Indexed sequence getter.
pub type SSizeArgFunc = unsafe extern "C" fn(*mut PyObject, isize) -> *mut PyObject;
/// Indexed sequence setter/deleter.  A NULL value means deletion.
pub type SSizeObjArgProc = unsafe extern "C" fn(*mut PyObject, isize, *mut PyObject) -> c_int;
/// Object/object predicate or setter slot.
pub type ObjObjProc = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int;
/// Mapping setter/deleter.  A NULL value means deletion.
pub type ObjObjArgProc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int;
/// Coroutine send slot.
pub type SendFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut *mut PyObject) -> c_int;

/// Numeric protocol slot table.
///
/// This table is `repr(C)` and intentionally mirrors CPython's `PyNumberMethods`
/// shape while adding explicit reflected-operation slots for the Phase-B
/// workstreams.  A NULL/`None` slot means the operation is unsupported by this
/// type at the slot layer.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PyNumberMethods {
    /// `__add__`.
    pub nb_add: Option<BinaryFunc>,
    /// `__sub__`.
    pub nb_subtract: Option<BinaryFunc>,
    /// `__mul__`.
    pub nb_multiply: Option<BinaryFunc>,
    /// `__mod__`.
    pub nb_remainder: Option<BinaryFunc>,
    /// `__divmod__`.
    pub nb_divmod: Option<BinaryFunc>,
    /// `__pow__`.
    pub nb_power: Option<TernaryFunc>,
    /// `__neg__`.
    pub nb_negative: Option<UnaryFunc>,
    /// `__pos__`.
    pub nb_positive: Option<UnaryFunc>,
    /// `__abs__`.
    pub nb_absolute: Option<UnaryFunc>,
    /// `__bool__`.
    pub nb_bool: Option<InquiryFunc>,
    /// `__invert__`.
    pub nb_invert: Option<UnaryFunc>,
    /// `__lshift__`.
    pub nb_lshift: Option<BinaryFunc>,
    /// `__rshift__`.
    pub nb_rshift: Option<BinaryFunc>,
    /// `__and__`.
    pub nb_and: Option<BinaryFunc>,
    /// `__xor__`.
    pub nb_xor: Option<BinaryFunc>,
    /// `__or__`.
    pub nb_or: Option<BinaryFunc>,
    /// `__int__`.
    pub nb_int: Option<UnaryFunc>,
    /// `__float__`.
    pub nb_float: Option<UnaryFunc>,
    /// `__iadd__`.
    pub nb_inplace_add: Option<BinaryFunc>,
    /// `__isub__`.
    pub nb_inplace_subtract: Option<BinaryFunc>,
    /// `__imul__`.
    pub nb_inplace_multiply: Option<BinaryFunc>,
    /// `__imod__`.
    pub nb_inplace_remainder: Option<BinaryFunc>,
    /// `__ipow__`.
    pub nb_inplace_power: Option<TernaryFunc>,
    /// `__ilshift__`.
    pub nb_inplace_lshift: Option<BinaryFunc>,
    /// `__irshift__`.
    pub nb_inplace_rshift: Option<BinaryFunc>,
    /// `__iand__`.
    pub nb_inplace_and: Option<BinaryFunc>,
    /// `__ixor__`.
    pub nb_inplace_xor: Option<BinaryFunc>,
    /// `__ior__`.
    pub nb_inplace_or: Option<BinaryFunc>,
    /// `__floordiv__`.
    pub nb_floor_divide: Option<BinaryFunc>,
    /// `__truediv__`.
    pub nb_true_divide: Option<BinaryFunc>,
    /// `__ifloordiv__`.
    pub nb_inplace_floor_divide: Option<BinaryFunc>,
    /// `__itruediv__`.
    pub nb_inplace_true_divide: Option<BinaryFunc>,
    /// `__index__`.
    pub nb_index: Option<UnaryFunc>,
    /// `__matmul__`.
    pub nb_matrix_multiply: Option<BinaryFunc>,
    /// `__imatmul__`.
    pub nb_inplace_matrix_multiply: Option<BinaryFunc>,
    /// `__radd__`.
    pub nb_reflected_add: Option<BinaryFunc>,
    /// `__rsub__`.
    pub nb_reflected_subtract: Option<BinaryFunc>,
    /// `__rmul__`.
    pub nb_reflected_multiply: Option<BinaryFunc>,
    /// `__rmod__`.
    pub nb_reflected_remainder: Option<BinaryFunc>,
    /// `__rdivmod__`.
    pub nb_reflected_divmod: Option<BinaryFunc>,
    /// `__rpow__`.
    pub nb_reflected_power: Option<TernaryFunc>,
    /// `__rlshift__`.
    pub nb_reflected_lshift: Option<BinaryFunc>,
    /// `__rrshift__`.
    pub nb_reflected_rshift: Option<BinaryFunc>,
    /// `__rand__`.
    pub nb_reflected_and: Option<BinaryFunc>,
    /// `__rxor__`.
    pub nb_reflected_xor: Option<BinaryFunc>,
    /// `__ror__`.
    pub nb_reflected_or: Option<BinaryFunc>,
    /// `__rfloordiv__`.
    pub nb_reflected_floor_divide: Option<BinaryFunc>,
    /// `__rtruediv__`.
    pub nb_reflected_true_divide: Option<BinaryFunc>,
    /// `__rmatmul__`.
    pub nb_reflected_matrix_multiply: Option<BinaryFunc>,
}

impl PyNumberMethods {
    /// A numeric table with every slot unsupported.
    pub const EMPTY: Self = Self {
        nb_add: None,
        nb_subtract: None,
        nb_multiply: None,
        nb_remainder: None,
        nb_divmod: None,
        nb_power: None,
        nb_negative: None,
        nb_positive: None,
        nb_absolute: None,
        nb_bool: None,
        nb_invert: None,
        nb_lshift: None,
        nb_rshift: None,
        nb_and: None,
        nb_xor: None,
        nb_or: None,
        nb_int: None,
        nb_float: None,
        nb_inplace_add: None,
        nb_inplace_subtract: None,
        nb_inplace_multiply: None,
        nb_inplace_remainder: None,
        nb_inplace_power: None,
        nb_inplace_lshift: None,
        nb_inplace_rshift: None,
        nb_inplace_and: None,
        nb_inplace_xor: None,
        nb_inplace_or: None,
        nb_floor_divide: None,
        nb_true_divide: None,
        nb_inplace_floor_divide: None,
        nb_inplace_true_divide: None,
        nb_index: None,
        nb_matrix_multiply: None,
        nb_inplace_matrix_multiply: None,
        nb_reflected_add: None,
        nb_reflected_subtract: None,
        nb_reflected_multiply: None,
        nb_reflected_remainder: None,
        nb_reflected_divmod: None,
        nb_reflected_power: None,
        nb_reflected_lshift: None,
        nb_reflected_rshift: None,
        nb_reflected_and: None,
        nb_reflected_xor: None,
        nb_reflected_or: None,
        nb_reflected_floor_divide: None,
        nb_reflected_true_divide: None,
        nb_reflected_matrix_multiply: None,
    };
}

/// Sequence protocol slot table.
///
/// Slots use CPython-compatible signatures.  A NULL/`None` slot means this type
/// does not provide that operation through the sequence protocol.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PySequenceMethods {
    /// `__len__`.
    pub sq_length: Option<LenFunc>,
    /// Sequence concatenation.
    pub sq_concat: Option<BinaryFunc>,
    /// Sequence repeat.
    pub sq_repeat: Option<BinaryFunc>,
    /// Integer-indexed `__getitem__`.
    pub sq_item: Option<SSizeArgFunc>,
    /// Integer-indexed `__setitem__`/`__delitem__`.
    pub sq_ass_item: Option<SSizeObjArgProc>,
    /// `__contains__`.
    pub sq_contains: Option<ObjObjProc>,
    /// In-place sequence concatenation.
    pub sq_inplace_concat: Option<BinaryFunc>,
    /// In-place sequence repeat.
    pub sq_inplace_repeat: Option<BinaryFunc>,
    /// Sequence iterator construction for types that expose iteration here.
    pub sq_iter: Option<UnaryFunc>,
    /// Sequence iterator next slot for iterator-like sequence adapters.
    pub sq_iternext: Option<UnaryFunc>,
}

impl PySequenceMethods {
    /// A sequence table with every slot unsupported.
    pub const EMPTY: Self = Self {
        sq_length: None,
        sq_concat: None,
        sq_repeat: None,
        sq_item: None,
        sq_ass_item: None,
        sq_contains: None,
        sq_inplace_concat: None,
        sq_inplace_repeat: None,
        sq_iter: None,
        sq_iternext: None,
    };
}

/// Mapping protocol slot table.
///
/// Slots use CPython-compatible signatures.  A NULL/`None` slot means this type
/// does not provide that operation through the mapping protocol.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PyMappingMethods {
    /// Mapping `__len__`.
    pub mp_length: Option<LenFunc>,
    /// Object-keyed `__getitem__`.
    pub mp_subscript: Option<BinaryFunc>,
    /// Object-keyed `__setitem__`/`__delitem__`.
    pub mp_ass_subscript: Option<ObjObjArgProc>,
}

impl PyMappingMethods {
    /// A mapping table with every slot unsupported.
    pub const EMPTY: Self = Self {
        mp_length: None,
        mp_subscript: None,
        mp_ass_subscript: None,
    };
}

/// Async protocol slot table.
///
/// Slots use CPython-compatible signatures and cover await, async iteration,
/// async next, and coroutine send.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PyAsyncMethods {
    /// `__await__`.
    pub am_await: Option<UnaryFunc>,
    /// `__aiter__`.
    pub am_aiter: Option<UnaryFunc>,
    /// `__anext__`.
    pub am_anext: Option<UnaryFunc>,
    /// Coroutine send operation.
    pub am_send: Option<SendFunc>,
}

impl PyAsyncMethods {
    /// An async table with every slot unsupported.
    pub const EMPTY: Self = Self {
        am_await: None,
        am_aiter: None,
        am_anext: None,
        am_send: None,
    };
}

/// Object-valued dunder definitions that back Phase-B slot updates.
///
/// CPython slot function pointers are installed by later lowering workstreams.
/// This cache records the Python-level descriptor object associated with each
/// dunder today, so updating a class dictionary has a concrete per-type effect
/// without inventing fake slot trampolines.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PyDunderSlots {
    /// Cached `__add__` descriptor.
    pub add: *mut PyObject,
    /// Cached `__radd__` descriptor.
    pub reflected_add: *mut PyObject,
    /// Cached `__iter__` descriptor.
    pub iter: *mut PyObject,
    /// Cached `__next__` descriptor.
    pub next: *mut PyObject,
    /// Cached `__len__` descriptor.
    pub len: *mut PyObject,
    /// Cached `__getitem__` descriptor.
    pub getitem: *mut PyObject,
    /// Cached `__setitem__` descriptor.
    pub setitem: *mut PyObject,
    /// Cached `__call__` descriptor.
    pub call: *mut PyObject,
    /// Cached descriptor-protocol `__get__`.
    pub get: *mut PyObject,
    /// Cached descriptor-protocol `__set__`.
    pub set: *mut PyObject,
}

impl PyDunderSlots {
    /// Empty object-valued dunder cache.
    pub const EMPTY: Self = Self {
        add: ptr::null_mut(),
        reflected_add: ptr::null_mut(),
        iter: ptr::null_mut(),
        next: ptr::null_mut(),
        len: ptr::null_mut(),
        getitem: ptr::null_mut(),
        setitem: ptr::null_mut(),
        call: ptr::null_mut(),
        get: ptr::null_mut(),
        set: ptr::null_mut(),
    };
}

/// Runtime type object.
#[repr(C)]
#[derive(Debug)]
pub struct PyType {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// UTF-8 type name bytes.
    pub name: *const u8,
    /// Byte length for `name`.
    pub name_len: usize,
    /// Phase-A compatibility mirror of `tp_basicsize`.
    pub instance_size: usize,
    /// CPython-style `tp_basicsize`: bytes in the fixed-size instance prefix.
    pub tp_basicsize: usize,
    /// CPython-style `tp_itemsize`: bytes per variable-sized item, or zero.
    pub tp_itemsize: isize,
    /// Type flags reserved for Phase-B semantic and GC properties.
    pub tp_flags: usize,
    /// Direct base type, or NULL for the root during bootstrap.
    pub tp_base: *mut PyType,
    /// Method-resolution-order object, or NULL until class machinery owns it.
    pub tp_mro: *mut PyObject,
    /// Bases tuple/list object, or NULL until class machinery owns it.
    pub tp_bases: *mut PyObject,
    /// Type dictionary object, or NULL for Phase-A static descriptors.
    pub tp_dict: *mut PyObject,
    /// Offset of an instance dictionary pointer, or zero when instances do not carry one.
    pub tp_dictoffset: isize,
    /// Hash slot.
    pub tp_hash: Option<HashFunc>,
    /// Rich-comparison slot.
    pub tp_richcmp: Option<RichCmpFunc>,
    /// `repr(obj)` slot.
    pub tp_repr: Option<UnaryFunc>,
    /// `str(obj)` slot.
    pub tp_str: Option<UnaryFunc>,
    /// Call slot.
    pub tp_call: Option<CallFunc>,
    /// Attribute lookup slot.
    pub tp_getattro: Option<GetAttrFunc>,
    /// Attribute assignment/deletion slot.
    pub tp_setattro: Option<SetAttrFunc>,
    /// Iterator construction slot.
    pub tp_iter: Option<UnaryFunc>,
    /// Iterator next slot.
    pub tp_iternext: Option<UnaryFunc>,
    /// Descriptor `__get__` slot.
    pub tp_descr_get: Option<DescrGetFunc>,
    /// Descriptor `__set__`/`__delete__` slot.
    pub tp_descr_set: Option<DescrSetFunc>,
    /// Instance initialization slot.
    pub tp_init: Option<InitFunc>,
    /// Instance allocation/construction slot.
    pub tp_new: Option<NewFunc>,
    /// Truth-value slot.
    pub tp_bool: Option<InquiryFunc>,
    /// Numeric protocol table, or NULL when absent.
    pub tp_as_number: *mut PyNumberMethods,
    /// Sequence protocol table, or NULL when absent.
    pub tp_as_sequence: *mut PySequenceMethods,
    /// Mapping protocol table, or NULL when absent.
    pub tp_as_mapping: *mut PyMappingMethods,
    /// Async protocol table, or NULL when absent.
    pub tp_as_async: *mut PyAsyncMethods,
    /// Runtime GC type id associated with instances of this type.
    pub gc_type_id: usize,
    /// Object-valued dunder definitions that drive slot refreshes.
    pub dunder_slots: PyDunderSlots,
}

impl PyType {
    /// Creates an immortal type descriptor with every protocol slot unsupported.
    #[must_use]
    pub const fn new(type_type: *const PyType, name: &'static str, instance_size: usize) -> Self {
        Self {
            ob_base: PyObjectHeader::new(type_type),
            name: name.as_ptr(),
            name_len: name.len(),
            instance_size,
            tp_basicsize: instance_size,
            tp_itemsize: 0,
            tp_flags: 0,
            tp_base: ptr::null_mut(),
            tp_mro: ptr::null_mut(),
            tp_bases: ptr::null_mut(),
            tp_dict: ptr::null_mut(),
            tp_dictoffset: 0,
            tp_hash: None,
            tp_richcmp: None,
            tp_repr: None,
            tp_str: None,
            tp_call: None,
            tp_getattro: None,
            tp_setattro: None,
            tp_iter: None,
            tp_iternext: None,
            tp_descr_get: None,
            tp_descr_set: None,
            tp_init: None,
            tp_new: None,
            tp_bool: None,
            tp_as_number: ptr::null_mut(),
            tp_as_sequence: ptr::null_mut(),
            tp_as_mapping: ptr::null_mut(),
            tp_as_async: ptr::null_mut(),
            gc_type_id: 0,
            dunder_slots: PyDunderSlots::EMPTY,
        }
    }

    /// Returns the UTF-8 type name.
    #[must_use]
    pub fn name(&self) -> &str {
        // SAFETY: Type objects are created only from `'static str` names.
        unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(self.name, self.name_len)) }
    }
}

/// Error returned when a dunder-to-slot refresh cannot be applied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlotUpdateError {
    /// The provided type pointer was NULL.
    NullType,
}

/// Refreshes Phase-B slot metadata after assigning or deleting a Python dunder.
///
/// `name` must be an interned identifier from [`crate::intern`].  For known
/// dunders this updates the object-valued cache on `ty`; deleting a dunder
/// (`value == NULL`) also clears the corresponding C-level slot pointer when
/// the relevant protocol table is present.  Non-NULL dunder values are not
/// converted into executable C slots here: later lowering workstreams install
/// the real trampolines that know how to call Python descriptors.  Unknown
/// dunder names are accepted and intentionally leave the type unchanged.
///
/// # Errors
///
/// Returns [`SlotUpdateError::NullType`] when `ty` is NULL.
pub fn update_slot_from_dunder(ty: *mut PyType, name: u32, value: *mut PyObject) -> Result<(), SlotUpdateError> {
    if ty.is_null() {
        return Err(SlotUpdateError::NullType);
    }

    // SAFETY: The NULL case is handled above; callers own synchronization for mutable type updates.
    let ty = unsafe { &mut *ty };

    let Some(slot_name) = intern::resolve(name) else {
        return Ok(());
    };

    match slot_name.as_str() {
        intern::DUNDER_ADD => {
            ty.dunder_slots.add = value;
            if value.is_null() {
                with_number_methods(ty, |methods| methods.nb_add = None);
            }
        }
        intern::DUNDER_RADD => {
            ty.dunder_slots.reflected_add = value;
            if value.is_null() {
                with_number_methods(ty, |methods| methods.nb_reflected_add = None);
            }
        }
        intern::DUNDER_ITER => {
            ty.dunder_slots.iter = value;
            if value.is_null() {
                ty.tp_iter = None;
                with_sequence_methods(ty, |methods| methods.sq_iter = None);
            }
        }
        intern::DUNDER_NEXT => {
            ty.dunder_slots.next = value;
            if value.is_null() {
                ty.tp_iternext = None;
                with_sequence_methods(ty, |methods| methods.sq_iternext = None);
            }
        }
        intern::DUNDER_LEN => {
            ty.dunder_slots.len = value;
            if value.is_null() {
                with_sequence_methods(ty, |methods| methods.sq_length = None);
                with_mapping_methods(ty, |methods| methods.mp_length = None);
            }
        }
        intern::DUNDER_GETITEM => {
            ty.dunder_slots.getitem = value;
            if value.is_null() {
                with_sequence_methods(ty, |methods| methods.sq_item = None);
                with_mapping_methods(ty, |methods| methods.mp_subscript = None);
            }
        }
        intern::DUNDER_SETITEM => {
            ty.dunder_slots.setitem = value;
            if value.is_null() {
                with_sequence_methods(ty, |methods| methods.sq_ass_item = None);
                with_mapping_methods(ty, |methods| methods.mp_ass_subscript = None);
            }
        }
        intern::DUNDER_CALL => {
            ty.dunder_slots.call = value;
            if value.is_null() {
                ty.tp_call = None;
            }
        }
        intern::DUNDER_GET => {
            ty.dunder_slots.get = value;
            if value.is_null() {
                ty.tp_descr_get = None;
            }
        }
        intern::DUNDER_SET => {
            ty.dunder_slots.set = value;
            if value.is_null() {
                ty.tp_descr_set = None;
            }
        }
        _ => {}
    }

    Ok(())
}

fn with_number_methods(ty: &mut PyType, f: impl FnOnce(&mut PyNumberMethods)) {
    // SAFETY: A non-NULL protocol pointer is owned by the type object by convention.
    if let Some(methods) = unsafe { ty.tp_as_number.as_mut() } {
        f(methods);
    }
}

fn with_sequence_methods(ty: &mut PyType, f: impl FnOnce(&mut PySequenceMethods)) {
    // SAFETY: A non-NULL protocol pointer is owned by the type object by convention.
    if let Some(methods) = unsafe { ty.tp_as_sequence.as_mut() } {
        f(methods);
    }
}

fn with_mapping_methods(ty: &mut PyType, f: impl FnOnce(&mut PyMappingMethods)) {
    // SAFETY: A non-NULL protocol pointer is owned by the type object by convention.
    if let Some(methods) = unsafe { ty.tp_as_mapping.as_mut() } {
        f(methods);
    }
}

/// Boxed Python integer for Phase A.
#[repr(C)]
#[derive(Debug)]
pub struct PyLong {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Signed 64-bit payload used by the Phase-A integer subset.
    pub value: i64,
}

/// Boxed Python Unicode string.
#[repr(C)]
#[derive(Debug)]
pub struct PyUnicode {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// UTF-8 byte length.
    pub len: usize,
    /// UTF-8 byte storage.  This may borrow rodata or point to owned heap bytes.
    pub data: *const u8,
    /// Whether `data` owns a leaked boxed byte slice that the GC finalizer frees.
    pub owns_data: bool,
}

impl PyUnicode {
    /// Returns the string as UTF-8 when the payload is valid.
    #[must_use]
    pub unsafe fn as_str(&self) -> Option<&str> {
        if self.data.is_null() && self.len != 0 {
            return None;
        }
        // SAFETY: The caller guarantees that `self` is a live `PyUnicode`; the
        // UTF-8 validity check below handles arbitrary bytes defensively.
        let bytes = unsafe { core::slice::from_raw_parts(self.data, self.len) };
        core::str::from_utf8(bytes).ok()
    }
}

/// ABI function pointer type used by compiled Python functions.
pub type PyCodeFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

/// Tier-up state value for functions still dispatching to tier-0 code.
pub const TIER_STATE_TIER0: u8 = 0;
/// Tier-up state value for functions with a tier-1 compile/install queued.
pub const TIER_STATE_QUEUED: u8 = 1;
/// Tier-up state value for functions whose dispatch cell targets tier-1 code.
pub const TIER_STATE_TIER1: u8 = 2;
/// Tier-up state value for functions that should remain on tier-0 after a
/// failed or ineligible tier-1 attempt.
pub const TIER_STATE_DISABLED: u8 = 3;

/// Runtime-owned placeholder for a finalized tier-1 code handle.
///
/// The runtime deliberately treats `handle` as opaque so it does not depend on
/// `pon-jit`.  The tier-up installer owns the concrete allocation and stores a
/// process-valid pointer here to keep the compiled body alive.
#[repr(C)]
#[derive(Debug)]
pub struct Tier1Code {
    /// Installed tier-1 entrypoint, if any.
    pub entry: *const u8,
    /// Opaque owner/handle supplied by the tier-up backend.
    pub handle: *mut c_void,
}

/// Boxed Python function.
#[repr(C)]
pub struct PyFunction {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Raw tier-0 entrypoint address for a `PyCodeFn`.
    pub code: *const u8,
    /// Positional arity enforced by `pon_call`.
    pub arity: usize,
    /// Interned function name.
    pub name_interned: u32,
    /// Dispatch cell every runtime call loads before invoking compiled code.
    pub entry: AtomicPtr<u8>,
    /// Function-entry hotness counter used by tier-up probes.
    pub hotness: AtomicU32,
    /// Loop back-edge hotness counter used by tier-up probes.
    pub loop_hotness: AtomicU32,
    /// Tier state machine: 0=Tier0, 1=Queued, 2=Tier1, 3=Disabled.
    pub tier_state: AtomicU8,
    /// Per-function feedback vector installed by profiling-aware lowering/JIT.
    pub feedback: UnsafeCell<Option<FeedbackVec>>,
    /// Opaque tier-1 code owner installed by the tier-up backend.
    pub tier1: UnsafeCell<Option<Tier1Code>>,
}

impl core::fmt::Debug for PyFunction {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PyFunction")
            .field("ob_base", &self.ob_base)
            .field("code", &self.code)
            .field("arity", &self.arity)
            .field("name_interned", &self.name_interned)
            .field("entry", &self.entry.load(core::sync::atomic::Ordering::Relaxed))
            .field("hotness", &self.hotness.load(core::sync::atomic::Ordering::Relaxed))
            .field("loop_hotness", &self.loop_hotness.load(core::sync::atomic::Ordering::Relaxed))
            .field("tier_state", &self.tier_state.load(core::sync::atomic::Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl PyFunction {
    /// Builds a function object payload with tier-up state initialized for tier-0.
    #[must_use]
    pub fn new(ob_type: *const PyType, code: *const u8, arity: usize, name_interned: u32) -> Self {
        Self {
            ob_base: PyObjectHeader::new(ob_type),
            code,
            arity,
            name_interned,
            entry: AtomicPtr::new(code.cast_mut()),
            hotness: AtomicU32::new(0),
            loop_hotness: AtomicU32::new(0),
            tier_state: AtomicU8::new(TIER_STATE_TIER0),
            feedback: UnsafeCell::new(None),
            tier1: UnsafeCell::new(None),
        }
    }
}

/// The immortal `None` object layout.
#[repr(C)]
#[derive(Debug)]
pub struct PyNone {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
}

/// Casts a concrete object pointer to the ABI base pointer.
#[must_use]
pub fn as_object_ptr<T>(value: *mut T) -> *mut PyObject {
    value.cast::<PyObject>()
}

/// Returns true when `object` has exactly the requested runtime type pointer.
#[must_use]
pub unsafe fn is_exact_type(object: *mut PyObject, ty: *const PyType) -> bool {
    if object.is_null() {
        return false;
    }
    // SAFETY: Non-null boxed values always begin with `PyObjectHeader`.
    unsafe { ptr::addr_of!((*object).ob_type).read() == ty }
}

const _: () = {
    assert!(offset_of!(PyObjectHeader, ob_type) == 0);
    assert!(offset_of!(PyType, ob_base) == 0);
    assert!(offset_of!(PyLong, ob_base) == 0);
    assert!(offset_of!(PyUnicode, ob_base) == 0);
    assert!(offset_of!(PyFunction, ob_base) == 0);
    assert!(offset_of!(PyNone, ob_base) == 0);
    assert!(size_of::<PyObject>() == size_of::<PyObjectHeader>());
    assert!(offset_of!(GcMeta, flags) == 0);
    assert!(size_of::<GcMeta>() == size_of::<usize>());
    assert!(offset_of!(PyObjectHeader, gc_meta) == size_of::<*const PyType>());
};

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn dummy_binary(_left: *mut PyObject, _right: *mut PyObject) -> *mut PyObject {
        ptr::null_mut()
    }

    #[test]
    fn layout_headers_are_first() {
        assert_eq!(offset_of!(PyType, ob_base), 0);
        assert_eq!(offset_of!(PyLong, ob_base), 0);
        assert_eq!(offset_of!(PyUnicode, ob_base), 0);
        assert_eq!(offset_of!(PyFunction, ob_base), 0);
        assert_eq!(offset_of!(PyNone, ob_base), 0);
    }

    #[test]
    fn gc_meta_layout_remains_one_machine_word() {
        assert_eq!(offset_of!(GcMeta, flags), 0);
        assert_eq!(size_of::<GcMeta>(), size_of::<usize>());
    }

    #[test]
    fn object_header_layout_preserves_pre_phase_e_offsets() {
        assert_eq!(offset_of!(PyObjectHeader, ob_type), 0);
        assert_eq!(offset_of!(PyObjectHeader, gc_meta), size_of::<*const PyType>());
        assert_eq!(
            size_of::<PyObjectHeader>(),
            size_of::<*const PyType>() + size_of::<usize>()
        );
        assert_eq!(size_of::<PyObject>(), size_of::<PyObjectHeader>());
    }

    #[test]
    fn object_payload_offsets_follow_preserved_header() {
        assert_eq!(offset_of!(PyLong, value), size_of::<PyObjectHeader>());
        assert_eq!(offset_of!(PyUnicode, len), size_of::<PyObjectHeader>());
        assert_eq!(offset_of!(PyFunction, code), size_of::<PyObjectHeader>());
    }

    #[test]
    fn object_header_new_initializes_gc_metadata_inert() {
        let type_ptr = 0x1usize as *const PyType;
        let header = PyObjectHeader::new(type_ptr);

        assert_eq!(header.ob_type, type_ptr);
        assert_eq!(header.gc_meta, GcMeta::EMPTY);
        assert_eq!(header.gc_meta.flags, 0);
    }

    #[test]
    fn py_type_layout_keeps_dunder_slots_after_gc_type_id() {
        assert_eq!(
            offset_of!(PyType, dunder_slots),
            offset_of!(PyType, gc_type_id) + size_of::<usize>()
        );
    }

    #[test]
    fn py_type_new_initializes_layout_metadata_inert() {
        let type_ptr = 0x1usize as *const PyType;
        let ty = PyType::new(type_ptr, "dummy", 0);

        assert_eq!(ty.ob_base.ob_type, type_ptr);
        assert_eq!(ty.ob_base.gc_meta, GcMeta::EMPTY);
        assert_eq!(ty.ob_base.gc_meta.flags, 0);
        assert_eq!(ty.gc_type_id, 0);
    }

    #[test]
    fn slot_table_defaults_are_null() {
        let ty = PyType::new(ptr::null(), "dummy", 0);

        assert!(ty.tp_hash.is_none());
        assert!(ty.tp_richcmp.is_none());
        assert!(ty.tp_call.is_none());
        assert!(ty.tp_as_number.is_null());
        assert!(ty.tp_as_sequence.is_null());
        assert!(ty.tp_as_mapping.is_null());
        assert!(ty.tp_as_async.is_null());
        assert!(ty.dunder_slots.add.is_null());
    }

    #[test]
    fn dunder_slot_update_records_value_and_clears_deleted_slot() {
        let mut ty = PyType::new(ptr::null(), "dummy", 0);
        let fake_value = 1usize as *mut PyObject;

        update_slot_from_dunder(&mut ty, intern::dunder_add(), fake_value).unwrap();
        assert_eq!(ty.dunder_slots.add, fake_value);

        let mut number_methods = PyNumberMethods::EMPTY;
        number_methods.nb_add = Some(dummy_binary);
        ty.tp_as_number = &mut number_methods;

        update_slot_from_dunder(&mut ty, intern::dunder_add(), ptr::null_mut()).unwrap();
        assert!(ty.dunder_slots.add.is_null());
        assert!(number_methods.nb_add.is_none());
    }

    #[test]
    fn unknown_dunder_update_is_noop() {
        let mut ty = PyType::new(ptr::null(), "dummy", 0);
        let fake_value = 1usize as *mut PyObject;

        update_slot_from_dunder(&mut ty, intern::intern("not_a_slot"), fake_value).unwrap();
        assert!(ty.dunder_slots.add.is_null());
    }
}
