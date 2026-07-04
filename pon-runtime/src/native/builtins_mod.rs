//! Native Phase-B builtin function surface.
//!
//! These helpers deliberately return boxed `*mut PyObject` values and use the
//! existing thread-state NULL-sentinel error convention.  The sequence and
//! iterator objects here are small native runtime objects that provide enough
//! slot surface for the Phase-B builtin corpus while the family-owned concrete
//! type modules grow full CPython-compatible implementations.

use std::cmp::Ordering;
use core::ffi::c_int;
use std::io::{self, Write};
use std::ptr;
use std::sync::{LazyLock, OnceLock};
use num_bigint::BigInt;
use num_traits::{One, Signed, ToPrimitive, Zero};

use crate::abi::{self, pon_get_iter, pon_iter_next};
use crate::gcroot::{HeldRoots, RootRegistry};
use crate::intern::{intern, resolve};
use crate::object::{PyFunction, PyMappingMethods, PyObject, PyObjectHeader, PySequenceMethods, PyType, PyUnicode, UnaryFunc};
use crate::thread_state::{pon_err_clear, pon_err_message, pon_err_occurred, pon_err_set};
use crate::types::{bool_, property};
use crate::types::exc::PyBaseException;

pub const VARIADIC_ARITY: usize = usize::MAX;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

pub fn for_each_builtin(mut f: impl FnMut(&'static str, usize, *const u8)) {
    macro_rules! builtin {
        ($name:literal, $arity:expr, $func:path) => {
            let code: BuiltinFn = $func;
            f($name, $arity, code as *const u8);
        };
    }

    builtin!("print", VARIADIC_ARITY, builtin_print);
    builtin!("__build_class__", VARIADIC_ARITY, builtin_build_class);
    builtin!("len", 1, builtin_len);
    builtin!("range", VARIADIC_ARITY, builtin_range);
    builtin!("iter", VARIADIC_ARITY, builtin_iter);
    builtin!("next", VARIADIC_ARITY, builtin_next);
    builtin!("isinstance", 2, builtin_isinstance);
    builtin!("type", VARIADIC_ARITY, builtin_type);
    builtin!("getattr", VARIADIC_ARITY, builtin_getattr);
    builtin!("setattr", 3, builtin_setattr);
    builtin!("hasattr", 2, builtin_hasattr);
    builtin!("repr", 1, builtin_repr);
    builtin!("str", VARIADIC_ARITY, builtin_str);
    builtin!("format", VARIADIC_ARITY, super::builtins_batch::builtin_format);
    builtin!("hash", 1, builtin_hash);
    builtin!("id", 1, builtin_id);
    builtin!("sorted", VARIADIC_ARITY, super::builtins_batch::builtin_sorted);
    builtin!("enumerate", VARIADIC_ARITY, builtin_enumerate);
    builtin!("zip", VARIADIC_ARITY, super::builtins_batch::builtin_zip);
    builtin!("map", VARIADIC_ARITY, super::builtins_batch::builtin_map);
    builtin!("filter", 2, super::builtins_batch::builtin_filter);
    builtin!("all", 1, builtin_all);
    builtin!("any", 1, builtin_any);
    builtin!("sum", VARIADIC_ARITY, super::builtins_batch::builtin_sum);
    builtin!("min", VARIADIC_ARITY, super::builtins_batch::builtin_min);
    builtin!("max", VARIADIC_ARITY, super::builtins_batch::builtin_max);
    builtin!("abs", 1, builtin_abs);
    builtin!("round", VARIADIC_ARITY, super::builtins_batch::builtin_round);
    builtin!("divmod", 2, super::builtins_batch::builtin_divmod);
    builtin!("pow", VARIADIC_ARITY, super::builtins_batch::builtin_pow);
    builtin!("bytes", VARIADIC_ARITY, builtin_bytes);
    builtin!("chr", 1, super::builtins_batch::builtin_chr);
    builtin!("dir", VARIADIC_ARITY, super::builtins_batch::builtin_dir);
    builtin!("int", VARIADIC_ARITY, builtin_int);
    builtin!("issubclass", 2, builtin_issubclass);
    builtin!("float", VARIADIC_ARITY, builtin_float);
    builtin!("complex", VARIADIC_ARITY, builtin_complex);
    builtin!("bool", VARIADIC_ARITY, builtin_bool);
    builtin!("list", VARIADIC_ARITY, builtin_list);
    builtin!("tuple", VARIADIC_ARITY, builtin_tuple);
    builtin!("dict", VARIADIC_ARITY, builtin_dict);
    builtin!("set", VARIADIC_ARITY, builtin_set);
    builtin!("slice", VARIADIC_ARITY, super::builtins_batch::builtin_slice);
    builtin!("object", VARIADIC_ARITY, builtin_object);
    builtin!("super", VARIADIC_ARITY, builtin_super);
    builtin!("property", VARIADIC_ARITY, builtin_property);
    builtin!("classmethod", 1, builtin_classmethod);
    builtin!("staticmethod", 1, builtin_staticmethod);
    builtin!("callable", 1, builtin_callable);
    builtin!("globals", 0, builtin_globals);
    builtin!("locals", 0, builtin_locals);
    builtin!("open", VARIADIC_ARITY, builtin_open);
    builtin!("input", VARIADIC_ARITY, builtin_input);
    builtin!("compile", VARIADIC_ARITY, builtin_compile);
    builtin!("eval", VARIADIC_ARITY, builtin_eval);
    builtin!("exec", VARIADIC_ARITY, builtin_exec);
    builtin!("__import__", VARIADIC_ARITY, builtin_dunder_import);
    builtin!("vars", VARIADIC_ARITY, super::builtins_batch::builtin_vars);
    builtin!("ord", 1, super::builtins_batch::builtin_ord);
    builtin!("bin", 1, super::builtins_batch::builtin_bin);
    builtin!("oct", 1, super::builtins_batch::builtin_oct);
    builtin!("hex", 1, super::builtins_batch::builtin_hex);
    builtin!("reversed", 1, super::builtins_batch::builtin_reversed);
    builtin!("bytearray", VARIADIC_ARITY, builtin_bytearray);
    builtin!("memoryview", 1, builtin_memoryview);
    builtin!("frozenset", VARIADIC_ARITY, builtin_frozenset);
    builtin!("delattr", 2, builtin_delattr);
}

pub(crate) fn make_module() -> Result<*mut PyObject, String> {
    let mut attrs = Vec::new();
    for_each_builtin(|builtin_name, _arity, _code| {
        let name = crate::intern::intern(builtin_name);
        let value = unsafe { abi::pon_load_global(name, core::ptr::null_mut()) };
        if !value.is_null() {
            attrs.push((name, value));
        }
    });
    for exception_name in [
        "BaseException",
        "BaseExceptionGroup",
        "GeneratorExit",
        "KeyboardInterrupt",
        "SystemExit",
        "Exception",
        "ArithmeticError",
        "FloatingPointError",
        "OverflowError",
        "ZeroDivisionError",
        "AssertionError",
        "AttributeError",
        "BufferError",
        "EOFError",
        "ImportError",
        "ModuleNotFoundError",
        "LookupError",
        "IndexError",
        "KeyError",
        "MemoryError",
        "NameError",
        "UnboundLocalError",
        "OSError",
        "BlockingIOError",
        "ChildProcessError",
        "ConnectionError",
        "BrokenPipeError",
        "ConnectionAbortedError",
        "ConnectionRefusedError",
        "ConnectionResetError",
        "FileExistsError",
        "FileNotFoundError",
        "InterruptedError",
        "IsADirectoryError",
        "NotADirectoryError",
        "PermissionError",
        "ProcessLookupError",
        "TimeoutError",
        "ReferenceError",
        "RuntimeError",
        "NotImplementedError",
        "PythonFinalizationError",
        "RecursionError",
        "StopAsyncIteration",
        "StopIteration",
        "SyntaxError",
        "IndentationError",
        "TabError",
        "SystemError",
        "TypeError",
        "ValueError",
        "UnicodeError",
        "UnicodeDecodeError",
        "UnicodeEncodeError",
        "UnicodeTranslateError",
        "Warning",
        "BytesWarning",
        "DeprecationWarning",
        "EncodingWarning",
        "FutureWarning",
        "ImportWarning",
        "PendingDeprecationWarning",
        "ResourceWarning",
        "RuntimeWarning",
        "SyntaxWarning",
        "UnicodeWarning",
        "UserWarning",
        "ExceptionGroup",
        "EnvironmentError",
        "IOError",
    ] {
        let name = crate::intern::intern(exception_name);
        let value = unsafe { abi::pon_load_global(name, core::ptr::null_mut()) };
        if value.is_null() {
            return Err(format!("builtin exception '{exception_name}' is not registered"));
        }
        attrs.push((name, value));
    }
    super::install_module("builtins", attrs)
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SequenceKind {
    List,
    Tuple,
}

#[derive(Debug)]
enum NativePayload {
    Range { start: i64, stop: i64, step: i64 },
    RangeIterator { current: i64, stop: i64, step: i64 },
    LongRange { start: BigInt, stop: BigInt, step: BigInt },
    LongRangeIterator { current: BigInt, stop: BigInt, step: BigInt },
    Enumerate { iter: *mut PyObject, index: i64 },
    Zip { iters: Vec<*mut PyObject> },
    Map { function: *mut PyObject, iters: Vec<*mut PyObject> },
    Filter { function: *mut PyObject, iter: *mut PyObject },
    CallableSentinelIterator { callable: *mut PyObject, sentinel: *mut PyObject },
    Placeholder(&'static str),
}

#[repr(C)]
#[derive(Debug)]
struct NativeObject {
    ob_base: PyObjectHeader,
    payload: NativePayload,
}

unsafe impl Send for NativeObject {}

static LIST_TYPE: OnceLock<usize> = OnceLock::new();
static TUPLE_TYPE: OnceLock<usize> = OnceLock::new();
static RANGE_TYPE: OnceLock<usize> = OnceLock::new();
static RANGE_ITER_TYPE: OnceLock<usize> = OnceLock::new();
static ENUMERATE_TYPE: OnceLock<usize> = OnceLock::new();
static ZIP_TYPE: OnceLock<usize> = OnceLock::new();
static MAP_TYPE: OnceLock<usize> = OnceLock::new();
static FILTER_TYPE: OnceLock<usize> = OnceLock::new();
static PLACEHOLDER_TYPE: OnceLock<usize> = OnceLock::new();
static PROPERTY_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(ptr::null(), "property", std::mem::size_of::<property::PyProperty>());
    property::install_property_slots(&mut ty);
    Box::into_raw(Box::new(ty)) as usize
});
static SUPER_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(ptr::null(), "super", std::mem::size_of::<crate::types::super_::PySuper>());
    crate::types::super_::install_super_slots(&mut ty);
    Box::into_raw(Box::new(ty)) as usize
});
static CALL_SENTINEL_ITER_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = Box::new(PyType::new(
        ptr::null(),
        "callable_iterator",
        std::mem::size_of::<NativeObject>(),
    ));
    ty.tp_iter = Some(identity_iter_slot);
    ty.tp_iternext = Some(native_next_slot);
    ty.tp_getattro = Some(crate::abstract_op::iterator_dunder_getattro);
    ty.tp_bool = Some(native_bool_slot);
    ty.tp_hash = Some(native_hash_slot);
    Box::into_raw(ty) as usize
});
static LONGRANGE_ITER_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = Box::new(PyType::new(
        ptr::null(),
        "longrange_iterator",
        std::mem::size_of::<NativeObject>(),
    ));
    ty.tp_iter = Some(identity_iter_slot);
    ty.tp_iternext = Some(native_next_slot);
    ty.tp_getattro = Some(crate::abstract_op::iterator_dunder_getattro);
    ty.tp_bool = Some(native_bool_slot);
    ty.tp_hash = Some(native_hash_slot);
    Box::into_raw(ty) as usize
});

fn type_from(
    cell: &'static OnceLock<usize>,
    name: &'static str,
    iter: Option<crate::object::UnaryFunc>,
    iternext: Option<crate::object::UnaryFunc>,
) -> *mut PyType {
    *cell.get_or_init(|| {
        let mut ty = Box::new(PyType::new(ptr::null(), name, std::mem::size_of::<NativeObject>()));
        ty.tp_iter = iter;
        ty.tp_iternext = iternext;
        if iternext.is_some() {
            ty.tp_getattro = Some(crate::abstract_op::iterator_dunder_getattro);
        }
        ty.tp_bool = Some(native_bool_slot);
        ty.tp_hash = Some(native_hash_slot);
        Box::into_raw(ty) as usize
    }) as *mut PyType
}

fn list_type() -> *mut PyType {
    type_from(&LIST_TYPE, "list", None, None)
}

fn tuple_type() -> *mut PyType {
    type_from(&TUPLE_TYPE, "tuple", None, None)
}


fn range_type() -> *mut PyType {
    // Unlike the other `type_from` natives, range carries real
    // sequence/mapping tables: `subscript_get` dispatches `r[i]`/`r[a:b]`
    // through `mp_subscript`, so the tables are installed inside the
    // once-init to keep the leaked boxes single-shot.
    *RANGE_TYPE.get_or_init(|| {
        let mut ty = Box::new(PyType::new(ptr::null(), "range", std::mem::size_of::<NativeObject>()));
        ty.tp_iter = Some(range_iter_slot);
        ty.tp_bool = Some(native_bool_slot);
        ty.tp_hash = Some(native_range_hash_slot);
        ty.tp_richcmp = Some(native_range_richcmp_slot);
        ty.tp_as_sequence = Box::into_raw(Box::new(PySequenceMethods {
            sq_length: Some(native_range_len_slot),
            sq_concat: None,
            sq_repeat: None,
            sq_item: Some(native_range_item_slot),
            sq_ass_item: None,
            sq_contains: Some(native_range_contains_slot),
            sq_inplace_concat: None,
            sq_inplace_repeat: None,
            sq_iter: None,
            sq_iternext: None,
        }));
        ty.tp_as_mapping = Box::into_raw(Box::new(PyMappingMethods {
            mp_length: Some(native_range_len_slot),
            mp_subscript: Some(native_range_subscript_slot),
            mp_ass_subscript: None,
        }));
        Box::into_raw(ty) as usize
    }) as *mut PyType
}

fn range_iter_type() -> *mut PyType {
    type_from(&RANGE_ITER_TYPE, "range_iterator", Some(identity_iter_slot), Some(native_next_slot))
}

fn longrange_iter_type() -> *mut PyType {
    *LONGRANGE_ITER_TYPE as *mut PyType
}

fn enumerate_type() -> *mut PyType {
    type_from(&ENUMERATE_TYPE, "enumerate", Some(identity_iter_slot), Some(native_next_slot))
}

fn zip_type() -> *mut PyType {
    type_from(&ZIP_TYPE, "zip", Some(identity_iter_slot), Some(native_next_slot))
}

fn map_type() -> *mut PyType {
    type_from(&MAP_TYPE, "map", Some(identity_iter_slot), Some(native_next_slot))
}

fn filter_type() -> *mut PyType {
    type_from(&FILTER_TYPE, "filter", Some(identity_iter_slot), Some(native_next_slot))
}
fn call_sentinel_iter_type() -> *mut PyType {
    *CALL_SENTINEL_ITER_TYPE as *mut PyType
}


fn placeholder_type() -> *mut PyType {
    let ty = type_from(&PLACEHOLDER_TYPE, "object", None, None);
    unsafe {
        (*ty).tp_getattro = Some(placeholder_getattro_slot);
    }
    ty
}

pub(crate) fn property_type() -> *mut PyType {
    *PROPERTY_TYPE as *mut PyType
}

fn super_type() -> *mut PyType {
    *SUPER_TYPE as *mut PyType
}
pub(crate) fn builtin_native_type(name: &str) -> Option<*mut PyType> {
    Some(match name {
        "object" => placeholder_type(),
        "list" => list_type(),
        "tuple" => tuple_type(),
        "range" => range_type(),
        "enumerate" => enumerate_type(),
        "zip" => zip_type(),
        "map" => map_type(),
        "filter" => filter_type(),
        "property" => property_type(),
        "super" => super_type(),
        _ => return None,
    })
}

unsafe extern "C" fn placeholder_getattro_slot(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        return fail("object attribute name must be str");
    };
    // Uniform resolution through object's tp_dict (the `install_object_dunders`
    // carriers: `__repr__`/`__str__`/`__format__`/`__reduce_ex__`/`__init__`
    // plus the `__new__` staticmethod), bound through the descriptor protocol
    // exactly like heap-instance lookup; previously only a hardcoded
    // `__str__`/`__repr__` pair resolved here, so `object().__init__` raised.
    let ty = unsafe { (*object).ob_type.cast_mut() };
    if !ty.is_null() {
        let hook = unsafe { crate::descr::lookup_in_type(ty, intern(name)) };
        if !hook.is_null() {
            return unsafe { crate::descr::descriptor_get(hook, object, ty) };
        }
    }
    fail(format!("attribute '{name}' was not found"))
}


/// Ref-holding native-payload allocations (`enumerate`, JIT-surface
/// `zip`/`map`/`filter`, `iter(callable, sentinel)`), for GC root reporting:
/// the leaked boxes hold source iterators and callables that live on the GC
/// heap and are invisible to marking (`crate::gcroot`).  Numeric payloads
/// (range families) and placeholders hold no references and are never
/// registered.  Objects are immortal, so the registry only grows.
static REGISTRY: RootRegistry = RootRegistry::new();

/// References held by live ref-holding native payloads.  Consumed by
/// `crate::abi::collect` while the runtime lock is held.
pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
    REGISTRY.held_roots()
}

impl HeldRoots for NativeObject {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        match &self.payload {
            NativePayload::Enumerate { iter, .. } => push(*iter),
            NativePayload::Zip { iters } => {
                for &iter in iters {
                    push(iter);
                }
            }
            NativePayload::Map { function, iters } => {
                push(*function);
                for &iter in iters {
                    push(iter);
                }
            }
            NativePayload::Filter { function, iter } => {
                push(*function);
                push(*iter);
            }
            NativePayload::CallableSentinelIterator { callable, sentinel } => {
                push(*callable);
                push(*sentinel);
            }
            NativePayload::Range { .. }
            | NativePayload::RangeIterator { .. }
            | NativePayload::LongRange { .. }
            | NativePayload::LongRangeIterator { .. }
            | NativePayload::Placeholder(_) => {}
        }
    }
}

fn alloc_native(payload: NativePayload, ty: *mut PyType) -> *mut PyObject {
    let holds_refs = matches!(
        payload,
        NativePayload::Enumerate { .. }
            | NativePayload::Zip { .. }
            | NativePayload::Map { .. }
            | NativePayload::Filter { .. }
            | NativePayload::CallableSentinelIterator { .. }
    );
    let object = Box::into_raw(Box::new(NativeObject {
        ob_base: PyObjectHeader::new(ty),
        payload,
    }))
    .cast::<PyObject>();
    if holds_refs { REGISTRY.register::<NativeObject>(object) } else { object }
}

fn alloc_sequence(kind: SequenceKind, mut items: Vec<*mut PyObject>) -> *mut PyObject {
    // Constructor results are real, full-protocol seq-family objects.
    // SAFETY: The builders copy `items.len()` slots and follow the
    // NULL-sentinel error contract; an empty Vec's dangling pointer is legal
    // for a zero count.
    match kind {
        SequenceKind::List => unsafe { crate::abi::seq::pon_build_list(items.as_mut_ptr(), items.len()) },
        SequenceKind::Tuple => unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) },
    }
}

pub(crate) fn alloc_tuple(items: Vec<*mut PyObject>) -> *mut PyObject {
    alloc_sequence(SequenceKind::Tuple, items)
}
pub(crate) fn alloc_list(items: Vec<*mut PyObject>) -> *mut PyObject {
    alloc_sequence(SequenceKind::List, items)
}

fn alloc_callable_sentinel_iter(callable: *mut PyObject, sentinel: *mut PyObject) -> *mut PyObject {
    alloc_native(
        NativePayload::CallableSentinelIterator { callable, sentinel },
        call_sentinel_iter_type(),
    )
}


fn alloc_range(start: i64, stop: i64, step: i64) -> *mut PyObject {
    alloc_native(NativePayload::Range { start, stop, step }, range_type())
}

fn alloc_range_iter(current: i64, stop: i64, step: i64) -> *mut PyObject {
    alloc_native(NativePayload::RangeIterator { current, stop, step }, range_iter_type())
}

fn alloc_longrange(start: BigInt, stop: BigInt, step: BigInt) -> *mut PyObject {
    alloc_native(NativePayload::LongRange { start, stop, step }, range_type())
}

fn alloc_longrange_iter(current: BigInt, stop: BigInt, step: BigInt) -> *mut PyObject {
    alloc_native(NativePayload::LongRangeIterator { current, stop, step }, longrange_iter_type())
}

fn alloc_placeholder(name: &'static str) -> *mut PyObject {
    alloc_native(NativePayload::Placeholder(name), placeholder_type())
}

unsafe fn as_native<'a>(object: *mut PyObject) -> Option<&'a mut NativeObject> {
    if object.is_null() {
        return None;
    }
    let ty = unsafe { (*object).ob_type };
    if ty.is_null() {
        return None;
    }
    let native_ty = [
        list_type(),
        tuple_type(),
        range_type(),
        range_iter_type(),
        longrange_iter_type(),
        enumerate_type(),
        zip_type(),
        map_type(),
        filter_type(),
        call_sentinel_iter_type(),
        placeholder_type(),
    ]
    .contains(&ty.cast_mut());
    native_ty.then(|| unsafe { &mut *object.cast::<NativeObject>() })
}

unsafe extern "C" fn identity_iter_slot(object: *mut PyObject) -> *mut PyObject {
    object
}

unsafe extern "C" fn range_iter_slot(object: *mut PyObject) -> *mut PyObject {
    let Some(native) = (unsafe { as_native(object) }) else {
        return fail("range iterator receiver is not native");
    };
    match &native.payload {
        NativePayload::Range { start, stop, step } => alloc_range_iter(*start, *stop, *step),
        NativePayload::LongRange { start, stop, step } => alloc_longrange_iter(start.clone(), stop.clone(), step.clone()),
        _ => fail("range iterator receiver is not a range"),
    }
}

/// Start/step/length of a native range payload, promoted to `BigInt` so the
/// `Range` (i64) and `LongRange` arms share one arithmetic path.  Returns
/// `None` for non-range payloads.
fn range_payload_parts(payload: &NativePayload) -> Option<(BigInt, BigInt, BigInt)> {
    match payload {
        NativePayload::Range { start, stop, step } => {
            Some((BigInt::from(*start), BigInt::from(*step), BigInt::from(range_len(*start, *stop, *step))))
        }
        NativePayload::LongRange { start, stop, step } => {
            Some((start.clone(), step.clone(), longrange_len(start, stop, step)))
        }
        _ => None,
    }
}

/// `(len, start, step)` comparison key of any range object — the native
/// `Range`/`LongRange` payloads here or the abi seq-family `PyRange` — so
/// both representations (and cross-representation pairs) compare and hash
/// through one authority.  `None` when `object` is not a range.
pub(crate) fn range_cmp_key(object: *mut PyObject) -> Option<(BigInt, BigInt, BigInt)> {
    if object.is_null() || !crate::tag::is_heap(object) {
        return None;
    }
    if let Some(native) = unsafe { as_native(object) } {
        let (start, step, len) = range_payload_parts(&native.payload)?;
        return Some((len, start, step));
    }
    crate::abi::seq::abi_range_cmp_key(object)
}

/// CPython `range_equals`: ranges compare as the sequences they denote —
/// lengths must match, then `start` matters only for non-empty ranges and
/// `step` only past the first element (`range(0, 3, 2) == range(0, 4, 2)`).
pub(crate) fn range_keys_equal(left: &(BigInt, BigInt, BigInt), right: &(BigInt, BigInt, BigInt)) -> bool {
    let (left_len, left_start, left_step) = left;
    let (right_len, right_start, right_step) = right;
    if left_len != right_len {
        return false;
    }
    if left_len.is_zero() {
        return true;
    }
    if left_start != right_start {
        return false;
    }
    left_len.is_one() || left_step == right_step
}

/// Equality-consistent range hash over the normalized key (CPython
/// `range_hash` hashes `(len, start, step)` with components that don't
/// affect the denoted sequence blanked out).  Every entry point — both
/// `tp_hash` slots, dict/set keying, the `hash()` builtin — routes here, so
/// equal ranges can never land in different dict slots.  The value carries
/// the dict-domain `-1 -> -2` normalization already, keeping it identical
/// across normalizing and raw callers.
pub(crate) fn range_hash_value(object: *mut PyObject) -> Option<isize> {
    let (len, start, step) = range_cmp_key(object)?;
    let text = if len.is_zero() {
        "range:0".to_owned()
    } else if len.is_one() {
        format!("range:1:{start}")
    } else {
        format!("range:{len}:{start}:{step}")
    };
    let hash = stable_hash(&text) as isize;
    Some(if hash == -1 { -2 } else { hash })
}

/// Shared `tp_richcmp` for both range representations: EQ/NE compare
/// structurally when both operands are ranges; ordering selectors and
/// non-range operands return NotImplemented so the abstract fallback raises
/// the standard ordering TypeError (CPython `range_richcompare`).
pub(crate) unsafe fn range_richcmp(left: *mut PyObject, right: *mut PyObject, op: c_int) -> *mut PyObject {
    let selector = match u8::try_from(op) {
        Ok(selector @ (crate::abstract_op::RICH_EQ | crate::abstract_op::RICH_NE)) => selector,
        _ => return unsafe { abi::pon_not_implemented() },
    };
    let (Some(left_key), Some(right_key)) = (range_cmp_key(left), range_cmp_key(right)) else {
        return unsafe { abi::pon_not_implemented() };
    };
    let equal = range_keys_equal(&left_key, &right_key);
    let result = equal == (selector == crate::abstract_op::RICH_EQ);
    unsafe { abi::number::pon_const_bool(i32::from(result)) }
}

unsafe extern "C" fn native_range_richcmp_slot(left: *mut PyObject, right: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe { range_richcmp(left, right, op) }
}

unsafe extern "C" fn native_range_hash_slot(object: *mut PyObject) -> isize {
    match range_hash_value(object) {
        Some(hash) => hash,
        None => unsafe { native_hash_slot(object) },
    }
}

/// Boxes a `BigInt` as a Python int, using the fixed-width constructor for
/// values that fit i64.
fn bigint_result(value: BigInt) -> *mut PyObject {
    match value.to_i64() {
        Some(fixed) => unsafe { abi::pon_const_int(fixed) },
        None => crate::types::int::from_bigint(value),
    }
}

fn raise_range_index_error() -> *mut PyObject {
    let message = "range object index out of range";
    unsafe { crate::abi::exc::pon_raise_index_error(message.as_ptr(), message.len()) }
}

/// `range[index]` with Python index semantics (negative wraps once); `None`
/// means out of range.
fn range_payload_item(start: &BigInt, step: &BigInt, len: &BigInt, mut index: BigInt) -> Option<BigInt> {
    if index.is_negative() {
        index += len;
    }
    if index.is_negative() || index >= *len {
        return None;
    }
    Some(start + index * step)
}

unsafe extern "C" fn native_range_len_slot(object: *mut PyObject) -> isize {
    let Some(native) = (unsafe { as_native(object) }) else {
        pon_err_set("range length receiver is not a range");
        return -1;
    };
    let Some((_, _, len)) = range_payload_parts(&native.payload) else {
        pon_err_set("range length receiver is not a range");
        return -1;
    };
    match len.to_isize() {
        Some(value) => value,
        None => {
            pon_err_set("Python int too large to convert to C ssize_t");
            -1
        }
    }
}

unsafe extern "C" fn native_range_item_slot(object: *mut PyObject, index: isize) -> *mut PyObject {
    let Some(native) = (unsafe { as_native(object) }) else {
        return fail("range item receiver is not a range");
    };
    let Some((start, step, len)) = range_payload_parts(&native.payload) else {
        return fail("range item receiver is not a range");
    };
    match range_payload_item(&start, &step, &len, BigInt::from(index)) {
        Some(value) => bigint_result(value),
        None => raise_range_index_error(),
    }
}

unsafe extern "C" fn native_range_subscript_slot(object: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    let Some(native) = (unsafe { as_native(object) }) else {
        return fail("range subscript receiver is not a range");
    };
    let Some((start, step, len)) = range_payload_parts(&native.payload) else {
        return fail("range subscript receiver is not a range");
    };
    if crate::abi::seq::is_slice(key) {
        return range_payload_slice(&start, &step, &len, key);
    }
    let index = match unsafe { bool_::to_bool(key) } {
        Some(flag) => Some(BigInt::from(i64::from(flag))),
        None => unsafe { crate::types::int::to_bigint(key) },
    };
    let Some(index) = index else {
        let message = format!(
            "range indices must be integers or slices, not {}",
            unsafe { type_name(key) }.unwrap_or("object")
        );
        return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    };
    match range_payload_item(&start, &step, &len, index) {
        Some(value) => bigint_result(value),
        None => raise_range_index_error(),
    }
}

/// `range[a:b:c]` -> a new range: CPython's `compute_slice` composes the
/// receiver's start/step with the clamped slice indices.
fn range_payload_slice(start: &BigInt, step: &BigInt, len: &BigInt, key: *mut PyObject) -> *mut PyObject {
    let Some(len) = len.to_usize() else {
        return fail("range is too large to slice");
    };
    let indices = match crate::abi::seq::normalize_slice(unsafe { &*key.cast::<crate::types::slice_::PySlice>() }, len) {
        Ok(indices) => indices,
        Err(message) => {
            return if message == "slice step cannot be zero" {
                unsafe { crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) }
            } else {
                unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
            };
        }
    };
    let sub_start = start + BigInt::from(indices.start) * step;
    let sub_stop = start + BigInt::from(indices.stop) * step;
    let sub_step = step * BigInt::from(indices.step);
    match (sub_start.to_i64(), sub_stop.to_i64(), sub_step.to_i64()) {
        (Some(start), Some(stop), Some(step)) => alloc_range(start, stop, step),
        _ => alloc_longrange(sub_start, sub_stop, sub_step),
    }
}

/// `sq_contains` for range: int-like members resolve arithmetically
/// (CPython `range_contains`); any other needle falls back to the linear
/// equality scan over produced values.
unsafe extern "C" fn native_range_contains_slot(object: *mut PyObject, item: *mut PyObject) -> c_int {
    let Some(native) = (unsafe { as_native(object) }) else {
        pon_err_set("range contains receiver is not a range");
        return -1;
    };
    let Some((start, step, len)) = range_payload_parts(&native.payload) else {
        pon_err_set("range contains receiver is not a range");
        return -1;
    };
    if let Some(value) = unsafe { crate::types::int::to_bigint_including_bool(item) } {
        if len.is_zero() {
            return 0;
        }
        let offset = value - &start;
        if (&offset % &step).is_zero() {
            let index = offset / &step;
            return c_int::from(!index.is_negative() && index < len);
        }
        return 0;
    }
    // Non-int needle: equality scan, mirroring CPython's fallback to
    // `PySequence_Contains` semantics (`3.0 in range(5)` is True).
    let Some(count) = len.to_u64() else {
        pon_err_set("range is too large to scan for containment");
        return -1;
    };
    let mut value = start;
    for _ in 0..count {
        let candidate = bigint_result(value.clone());
        if candidate.is_null() {
            return -1;
        }
        match unsafe { crate::types::dict::object_equal(candidate, item) } {
            Ok(true) => return 1,
            Ok(false) => {}
            Err(message) => {
                pon_err_set(message);
                return -1;
            }
        }
        value += &step;
    }
    0
}

unsafe extern "C" fn native_next_slot(object: *mut PyObject) -> *mut PyObject {
    let Some(native) = (unsafe { as_native(object) }) else {
        return fail("iterator receiver is not native");
    };
    match &mut native.payload {
        NativePayload::RangeIterator { current, stop, step } => {
            if (*step > 0 && *current >= *stop) || (*step < 0 && *current <= *stop) {
                return stop_iteration();
            }
            let value = *current;
            *current += *step;
            unsafe { abi::pon_const_int(value) }
        }
        NativePayload::LongRangeIterator { current, stop, step } => {
            let done = if step.is_positive() { *current >= *stop } else { *current <= *stop };
            if done {
                return stop_iteration();
            }
            let value = current.clone();
            *current += &*step;
            crate::types::int::from_bigint(value)
        }
        NativePayload::Enumerate { iter, index } => {
            let value = unsafe { pon_iter_next(*iter, ptr::null_mut()) };
            if value.is_null() {
                return ptr::null_mut();
            }
            let pair = alloc_sequence(
                SequenceKind::Tuple,
                vec![unsafe { abi::pon_const_int(*index) }, value],
            );
            *index += 1;
            pair
        }
        NativePayload::Zip { iters } => {
            let mut items = Vec::with_capacity(iters.len());
            for iter in iters.iter().copied() {
                let value = unsafe { pon_iter_next(iter, ptr::null_mut()) };
                if value.is_null() {
                    return ptr::null_mut();
                }
                items.push(value);
            }
            alloc_sequence(SequenceKind::Tuple, items)
        }
        NativePayload::Map { function, iters } => {
            let mut items = Vec::with_capacity(iters.len());
            for iter in iters.iter().copied() {
                let value = unsafe { pon_iter_next(iter, ptr::null_mut()) };
                if value.is_null() {
                    return ptr::null_mut();
                }
                items.push(value);
            }
            unsafe { call_function(*function, &mut items) }
        }
        NativePayload::Filter { function, iter } => loop {
            let value = unsafe { pon_iter_next(*iter, ptr::null_mut()) };
            if value.is_null() {
                return ptr::null_mut();
            }
            let keep = if function.is_null() || is_none(*function) {
                unsafe { truth(value) }
            } else {
                let mut args = [value];
                let result = unsafe { call_function(*function, &mut args) };
                if result.is_null() {
                    return ptr::null_mut();
                }
                unsafe { truth(result) }
            };
            match keep {
                Ok(true) => return value,
                Ok(false) => {}
                Err(message) => return fail_preserving(message),
            }
        },
        NativePayload::CallableSentinelIterator { callable, sentinel } => {
            let mut args = [];
            let value = unsafe { call_function(*callable, &mut args) };
            if value.is_null() {
                return ptr::null_mut();
            }
            let equal = unsafe {
                abi::pon_rich_compare(crate::abstract_op::RICH_EQ, value, *sentinel, ptr::null_mut())
            };
            if equal.is_null() {
                return ptr::null_mut();
            }
            match unsafe { truth(equal) } {
                Ok(true) => stop_iteration(),
                Ok(false) => value,
                Err(message) => fail_preserving(message),
            }
        }
        _ => fail("object is not an iterator"),
    }
}

unsafe extern "C" fn native_bool_slot(object: *mut PyObject) -> i32 {
    let Some(native) = (unsafe { as_native(object) }) else {
        return 1;
    };
    match &native.payload {
        NativePayload::Range { start, stop, step } => {
            if (*step > 0 && *start < *stop) || (*step < 0 && *start > *stop) { 1 } else { 0 }
        }
        NativePayload::LongRange { start, stop, step } => {
            if (step.is_positive() && *start < *stop) || (step.is_negative() && *start > *stop) { 1 } else { 0 }
        }
        _ => 1,
    }
}

unsafe extern "C" fn native_hash_slot(object: *mut PyObject) -> isize {
    stable_hash(&repr_text(object)) as isize
}

pub unsafe extern "C" fn builtin_print(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("print() received a null argv pointer");
    };
    // `print(*objects, sep=' ', end='\n', file=None, flush=False)`: keywords
    // ride a trailing marker appended by the keyword binder; plain positional
    // calls arrive without one.
    let (args, kw_pairs) = match args.split_last() {
        Some((&last, rest)) => match unsafe { crate::types::lazy_iter::kw_marker_pairs(last) } {
            Some(pairs) => (rest, pairs),
            None => (args, &[][..]),
        },
        None => (args, &[][..]),
    };
    let mut sep: Option<String> = None;
    let mut end: Option<String> = None;
    let mut file: *mut PyObject = ptr::null_mut();
    let mut flush = false;
    for &(name, value) in kw_pairs {
        let Some(keyword) = crate::intern::resolve(name) else {
            return fail("print() keyword name is not interned");
        };
        match keyword.as_str() {
            slot @ ("sep" | "end") => {
                if !is_none(value) {
                    let Some(text) = (unsafe { crate::types::type_::unicode_text(value) }) else {
                        return abi::exc::raise_kind_error_text(
                            crate::types::exc::ExceptionKind::TypeError,
                            &format!(
                                "{slot} must be None or a string, not {}",
                                unsafe { type_name(value) }.unwrap_or("object")
                            ),
                        );
                    };
                    if slot == "sep" {
                        sep = Some(text.to_owned());
                    } else {
                        end = Some(text.to_owned());
                    }
                }
            }
            "file" => {
                if !is_none(value) {
                    file = value;
                }
            }
            "flush" => {
                flush = match unsafe { truth(value) } {
                    Ok(value) => value,
                    Err(message) => return fail(message),
                };
            }
            other => {
                // The keyword binder already rejects unknown names; keep the
                // typed rejection for direct native callers.
                return abi::exc::raise_kind_error_text(
                    crate::types::exc::ExceptionKind::TypeError,
                    &format!("'{other}' is an invalid keyword argument for print()"),
                );
            }
        }
    }
    // Stringify before taking the stdout lock: `__str__` dispatch can run
    // arbitrary Python (including a nested `print`).
    let mut texts = Vec::with_capacity(args.len());
    for value in args.iter().copied() {
        match try_str_text(value) {
            Ok(text) => texts.push(text),
            Err(()) => return ptr::null_mut(),
        }
    }
    let mut payload = texts.join(sep.as_deref().unwrap_or(" "));
    payload.push_str(end.as_deref().unwrap_or("\n"));
    if !file.is_null() {
        // Explicit `file=`: route through the object's own `write` (and
        // `flush`) methods so swapped streams (io.StringIO capture shims)
        // observe the output, matching CPython's `print`.
        let payload_object = alloc_str(&payload);
        if payload_object.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: Attribute dispatch tolerates a null feedback cell.
        let write_method = unsafe { abi::pon_get_attr(file, crate::intern::intern("write"), ptr::null_mut()) };
        if write_method.is_null() {
            return ptr::null_mut();
        }
        let mut call_args = [payload_object];
        // SAFETY: Call helper follows the NULL-sentinel error contract.
        if unsafe { abi::pon_call(write_method, call_args.as_mut_ptr(), 1) }.is_null() {
            return ptr::null_mut();
        }
        if flush {
            // SAFETY: Attribute dispatch tolerates a null feedback cell.
            let flush_method = unsafe { abi::pon_get_attr(file, crate::intern::intern("flush"), ptr::null_mut()) };
            if flush_method.is_null() {
                return ptr::null_mut();
            }
            // SAFETY: Call helper follows the NULL-sentinel error contract.
            if unsafe { abi::pon_call(flush_method, ptr::null_mut(), 0) }.is_null() {
                return ptr::null_mut();
            }
        }
        return unsafe { abi::pon_none() };
    }
    let mut stdout = io::stdout().lock();
    if write!(stdout, "{payload}").is_err() {
        return fail("failed to write stdout");
    }
    if stdout.flush().is_err() {
        return fail("failed to write stdout");
    }
    unsafe { abi::pon_none() }
}
pub unsafe extern "C" fn builtin_open(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { super::io::builtin_open(argv, argc) }
}

pub unsafe extern "C" fn builtin_input(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { super::io::builtin_input(argv, argc) }
}

pub unsafe extern "C" fn builtin_compile(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { crate::dynexec::builtin_compile(argv, argc) }
}

pub unsafe extern "C" fn builtin_eval(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { crate::dynexec::builtin_eval(argv, argc) }
}

pub unsafe extern "C" fn builtin_exec(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { crate::dynexec::builtin_exec(argv, argc) }
}

pub unsafe extern "C" fn builtin_dunder_import(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { crate::dynexec::builtin_dunder_import(argv, argc) }
}


pub unsafe extern "C" fn builtin_len(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 1, "len") }) else {
        return ptr::null_mut();
    };
    match length(args[0]) {
        Ok(value) => unsafe { abi::pon_const_int(value) },
        Err(message) => fail(message),
    }
}

pub unsafe extern "C" fn builtin_range(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("range() received a null argv pointer");
    };
    let (start, stop, step) = match args.len() {
        1 => {
            let Ok(stop) = arg_bigint(args[0]) else {
                return ptr::null_mut();
            };
            (BigInt::from(0), stop, BigInt::from(1))
        }
        2 => {
            let Ok(start) = arg_bigint(args[0]) else {
                return ptr::null_mut();
            };
            let Ok(stop) = arg_bigint(args[1]) else {
                return ptr::null_mut();
            };
            (start, stop, BigInt::from(1))
        }
        3 => {
            let Ok(step) = arg_bigint(args[2]) else {
                return ptr::null_mut();
            };
            if step.is_zero() {
                let message = "range() arg 3 must not be zero";
                return unsafe { crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) };
            }
            let Ok(start) = arg_bigint(args[0]) else {
                return ptr::null_mut();
            };
            let Ok(stop) = arg_bigint(args[1]) else {
                return ptr::null_mut();
            };
            (start, stop, step)
        }
        _ => return fail(format!("range() expected 1 to 3 arguments, got {}", args.len())),
    };
    match (start.to_i64(), stop.to_i64(), step.to_i64()) {
        (Some(start), Some(stop), Some(step)) => alloc_range(start, stop, step),
        _ => alloc_longrange(start, stop, step),
    }
}

pub unsafe extern "C" fn builtin_iter(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("iter() received a null argv pointer");
    };
    match args.len() {
        1 => unsafe { pon_get_iter(args[0], ptr::null_mut()) },
        2 => {
            if !unsafe { is_callable_object(args[0]) } {
                return fail("iter(v, w): v must be callable");
            }
            alloc_callable_sentinel_iter(args[0], args[1])
        }
        _ => fail(format!("iter() expected 1 or 2 arguments, got {}", args.len())),
    }
}

pub unsafe extern "C" fn builtin_next(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("next() received a null argv pointer");
    };
    if !(1..=2).contains(&args.len()) {
        return fail(format!("next() expected 1 or 2 arguments, got {}", args.len()));
    }
    let value = unsafe { pon_iter_next(args[0], ptr::null_mut()) };
    if value.is_null() && args.len() == 2 && stop_iteration_pending() {
        pon_err_clear();
        args[1]
    } else {
        value
    }
}

pub unsafe extern "C" fn builtin_isinstance(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 2, "isinstance") }) else {
        return ptr::null_mut();
    };
    let result = unsafe { object_is_instance(args[0], args[1]) };
    if result < 0 {
        return ptr::null_mut();
    }
    unsafe { abi::number::pon_const_bool(result) }
}

pub unsafe extern "C" fn builtin_type(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { crate::types::type_::builtin_type(argv, argc) }
}

pub unsafe extern "C" fn builtin_getattr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("getattr() received a null argv pointer");
    };
    if !(2..=3).contains(&args.len()) {
        return fail(format!("getattr() expected 2 or 3 arguments, got {}", args.len()));
    }
    let Some(name) = object_to_string(args[1]) else {
        return fail("getattr() attribute name must be str");
    };
    let result = unsafe { abi::pon_get_attr(args[0], intern(&name), ptr::null_mut()) };
    if result.is_null() && args.len() == 3 {
        pon_err_clear();
        args[2]
    } else {
        result
    }
}

pub unsafe extern "C" fn builtin_setattr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 3, "setattr") }) else {
        return ptr::null_mut();
    };
    let Some(name) = object_to_string(args[1]) else {
        return fail("setattr() attribute name must be str");
    };
    let status = unsafe { abi::pon_set_attr(args[0], intern(&name), args[2]) };
    if status < 0 {
        ptr::null_mut()
    } else {
        unsafe { abi::pon_none() }
    }
}

pub unsafe extern "C" fn builtin_delattr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 2, "delattr") }) else {
        return ptr::null_mut();
    };
    let Some(name) = object_to_string(args[1]) else {
        return fail("delattr() attribute name must be str");
    };
    let status = unsafe { abi::object::pon_del_attr(args[0], intern(&name)) };
    if status < 0 {
        ptr::null_mut()
    } else {
        unsafe { abi::pon_none() }
    }
}

pub unsafe extern "C" fn builtin_hasattr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 2, "hasattr") }) else {
        return ptr::null_mut();
    };
    let Some(name) = object_to_string(args[1]) else {
        return fail("hasattr() attribute name must be str");
    };
    let result = unsafe { abi::pon_get_attr(args[0], intern(&name), ptr::null_mut()) };
    let has_attr = !result.is_null();
    if !has_attr {
        pon_err_clear();
    }
    unsafe { abi::number::pon_const_bool(i32::from(has_attr)) }
}

pub unsafe extern "C" fn builtin_repr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 1, "repr") }) else {
        return ptr::null_mut();
    };
    match try_repr_text(args[0]) {
        Ok(text) => alloc_str(&text),
        Err(()) => ptr::null_mut(),
    }
}

pub unsafe extern "C" fn builtin_str(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("str() received a null argv pointer");
    };
    match args.len() {
        0 => alloc_str(""),
        1 => match try_str_text(args[0]) {
            Ok(text) => alloc_str(&text),
            Err(()) => ptr::null_mut(),
        },
        2 | 3 => crate::native::codecs::builtin_str_decode(args[0], args[1], args.get(2).copied()),
        _ => fail("str() takes at most 3 arguments"),
    }
}

pub unsafe extern "C" fn builtin_format(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("format() received a null argv pointer");
    };
    if !(1..=2).contains(&args.len()) {
        return fail(format!("format() expected 1 or 2 arguments, got {}", args.len()));
    }
    let spec = if let Some(spec) = args.get(1).copied() {
        let Some(spec) = object_to_string(spec) else {
            return fail("format() argument 2 must be str");
        };
        spec
    } else {
        String::new()
    };
    match abi::str_::format_object_with_spec(args[0], &spec) {
        Ok(text) => alloc_str(&text),
        Err(message) => fail(message),
    }
}

pub unsafe extern "C" fn builtin_hash(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 1, "hash") }) else {
        return ptr::null_mut();
    };
    match unsafe { builtin_hash_value(args[0]) } {
        Ok(value) => unsafe { abi::pon_const_int(value) },
        // Every native `hash()` failure is a TypeError in CPython (unhashable
        // containers, dead weakrefs); raise typed so `except TypeError` works.
        // A boxed exception a user `__hash__` already raised is authoritative
        // — the advisory message must not repackage it as a TypeError.
        Err(message) => {
            if crate::abi::exc::pending_exception_object().is_some() {
                return ptr::null_mut();
            }
            unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
        }
    }
}

/// `id(object)`: the object's address (CPython contract: an integer unique
/// and constant for the object's lifetime).  Tagged small ints use their
/// tagged bits directly — equal immediates share an id, mirroring CPython's
/// small-int interning.
pub unsafe extern "C" fn builtin_id(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 1, "id") }) else {
        return ptr::null_mut();
    };
    unsafe { abi::pon_const_int(args[0] as i64) }
}

/// `hash()`-builtin value domain, shared by the boxed entry point and the
/// weakref delegation (which must recurse on values to cache them).
unsafe fn builtin_hash_value(object: *mut PyObject) -> Result<i64, String> {
    let value = if let Some(value) = unsafe { bool_::to_bool(object) } {
        i64::from(value)
    } else if let Some(value) = unsafe { crate::types::int::to_bigint(object) } {
        crate::types::int::hash_bigint(&value) as i64
    } else if unsafe { crate::types::float::is_exact_float(object) } {
        let float = unsafe { &*object.cast::<crate::types::float::PyFloat>() };
        crate::types::float::hash_f64(float.value) as i64
    } else if let Some((real, imag)) = unsafe { crate::types::complex_::to_f64s(object) } {
        crate::types::complex_::hash_complex(real, imag) as i64
    } else if crate::types::typealias::is_union_type(object) {
        crate::types::typealias::union_hash(object) as i64
    } else if unsafe { crate::types::frozenset::is_frozenset(object) } {
        unsafe { crate::types::frozenset::frozenset_hash_value(object)? as i64 }
    } else if unsafe { crate::types::set_::is_set(object) } {
        return Err("unhashable type: 'set'".to_owned());
    } else if matches!(
        unsafe { crate::types::dict::type_name(object) },
        Some("str" | "bytes" | "dict" | "list" | "bytearray" | "tuple" | "range")
    ) || unsafe { crate::types::dict::is_dict_subclass_instance(object) }
    {
        // `hash_object` owns the content-hash (str/bytes CPython seed-0
        // siphash13 via `crate::pyhash`, structural tuple hashing shared
        // with tuple-subclass instances) and the CPython
        // `unhashable type: '...'` rejections (dict/list/bytearray).
        unsafe { crate::types::dict::hash_object(object)? as i64 }
    } else if unsafe { crate::types::weakref::is_weakref(object) } {
        // CPython: hash(ref) == hash(referent), cached while the referent is
        // alive so it survives referent death; dead and never hashed raises.
        if let Some(hash) = unsafe { crate::types::weakref::weakref_cached_builtin_hash(object) } {
            hash
        } else {
            let referent = unsafe { crate::types::weakref::weakref_target(object) };
            if referent.is_null() {
                return Err("weak object has gone away".to_owned());
            }
            let hash = unsafe { builtin_hash_value(referent)? };
            unsafe { crate::types::weakref::weakref_store_builtin_hash(object, hash) };
            hash
        }
    } else if !object.is_null()
        && crate::tag::is_heap(object)
        && crate::types::type_::type_dispatches_python_dunders(unsafe { (*object).ob_type })
    {
        // Python class instances share ONE hash authority with the dict/set
        // key domain (`types::dict::hash_object`): user `__hash__` hooks
        // dispatch, the `__hash__ = None` marker raises the CPython
        // `unhashable type: '...'` TypeError, and plain classes keep the
        // identity default — `hash(obj)` and `d[obj]` can never disagree.
        unsafe { crate::types::dict::hash_object(object)? as i64 }
    } else {
        stable_hash(&repr_text(object))
    };
    Ok(value)
}

pub unsafe extern "C" fn builtin_sorted(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("sorted() received a null argv pointer");
    };
    if !(1..=3).contains(&args.len()) {
        return fail(format!("sorted() expected 1 to 3 positional arguments, got {}", args.len()));
    }
    let mut items = match collect_iterable(args[0]) {
        Ok(items) => items,
        Err(message) => return fail(message),
    };
    let key_func = args.get(1).copied().filter(|key| !is_none(*key));
    items.sort_by(|a, b| compare_for_sort(*a, *b, key_func));
    if let Some(reverse) = args.get(2).copied() {
        match unsafe { truth(reverse) } {
            Ok(true) => items.reverse(),
            Ok(false) => {}
            Err(message) => return fail_preserving(message),
        }
    }
    alloc_sequence(SequenceKind::List, items)
}

pub unsafe extern "C" fn builtin_enumerate(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("enumerate() received a null argv pointer");
    };
    if !(1..=2).contains(&args.len()) {
        return fail(format!("enumerate() expected 1 or 2 arguments, got {}", args.len()));
    }
    let iter = unsafe { pon_get_iter(args[0], ptr::null_mut()) };
    if iter.is_null() {
        return ptr::null_mut();
    }
    let start = if args.len() == 2 {
        let Ok(start) = arg_i64(args[1], "enumerate") else {
            return ptr::null_mut();
        };
        start
    } else {
        0
    };
    alloc_native(NativePayload::Enumerate { iter, index: start }, enumerate_type())
}

pub unsafe extern "C" fn builtin_zip(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("zip() received a null argv pointer");
    };
    let mut iters = Vec::with_capacity(args.len());
    for arg in args.iter().copied() {
        let iter = unsafe { pon_get_iter(arg, ptr::null_mut()) };
        if iter.is_null() {
            return ptr::null_mut();
        }
        iters.push(iter);
    }
    alloc_native(NativePayload::Zip { iters }, zip_type())
}

pub unsafe extern "C" fn builtin_map(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("map() received a null argv pointer");
    };
    if args.len() < 2 {
        return fail(format!("map() expected at least 2 arguments, got {}", args.len()));
    }
    let mut iters = Vec::with_capacity(args.len() - 1);
    for arg in args[1..].iter().copied() {
        let iter = unsafe { pon_get_iter(arg, ptr::null_mut()) };
        if iter.is_null() {
            return ptr::null_mut();
        }
        iters.push(iter);
    }
    alloc_native(NativePayload::Map { function: args[0], iters }, map_type())
}

pub unsafe extern "C" fn builtin_filter(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 2, "filter") }) else {
        return ptr::null_mut();
    };
    let iter = unsafe { pon_get_iter(args[1], ptr::null_mut()) };
    if iter.is_null() {
        return ptr::null_mut();
    }
    alloc_native(NativePayload::Filter { function: args[0], iter }, filter_type())
}

pub unsafe extern "C" fn builtin_all(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 1, "all") }) else {
        return ptr::null_mut();
    };
    match iterate_truth(args[0], true) {
        Ok(value) => unsafe { abi::number::pon_const_bool(i32::from(value)) },
        Err(message) => fail_preserving(message),
    }
}

pub unsafe extern "C" fn builtin_any(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 1, "any") }) else {
        return ptr::null_mut();
    };
    match iterate_truth(args[0], false) {
        Ok(value) => unsafe { abi::number::pon_const_bool(i32::from(value)) },
        Err(message) => fail_preserving(message),
    }
}

pub unsafe extern "C" fn builtin_sum(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("sum() received a null argv pointer");
    };
    if !(1..=2).contains(&args.len()) {
        return fail(format!("sum() expected 1 or 2 arguments, got {}", args.len()));
    }
    let mut total = if args.len() == 2 {
        let Ok(total) = arg_i64(args[1], "sum") else {
            return ptr::null_mut();
        };
        total
    } else {
        0
    };
    let items = match collect_iterable(args[0]) {
        Ok(items) => items,
        Err(message) => return fail(message),
    };
    for item in items {
        let Ok(value) = arg_i64(item, "sum") else {
            return ptr::null_mut();
        };
        total = total.saturating_add(value);
    }
    unsafe { abi::pon_const_int(total) }
}

pub unsafe extern "C" fn builtin_min(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { min_max(argv, argc, false) }
}

pub unsafe extern "C" fn builtin_max(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { min_max(argv, argc, true) }
}

pub unsafe extern "C" fn builtin_abs(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 1, "abs") }) else {
        return ptr::null_mut();
    };
    crate::abi::number::abs_object(args[0])
}

pub unsafe extern "C" fn builtin_round(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("round() received a null argv pointer");
    };
    if !(1..=2).contains(&args.len()) {
        return fail(format!("round() expected 1 or 2 arguments, got {}", args.len()));
    }
    if let Some(value) = object_to_i64(args[0]) {
        return unsafe { abi::pon_const_int(value) };
    }
    let Some(value) = object_to_f64(args[0]) else {
        return fail("round() expected int or float argument");
    };
    if args.len() == 1 {
        return unsafe { abi::pon_const_int(value.round() as i64) };
    }
    let Ok(ndigits) = arg_i64(args[1], "round") else {
        return ptr::null_mut();
    };
    let factor = 10_f64.powi(ndigits.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32);
    crate::types::float::from_f64((value * factor).round() / factor)
}

pub unsafe extern "C" fn builtin_divmod(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 2, "divmod") }) else {
        return ptr::null_mut();
    };
    crate::abi::number::divmod_objects(args[0], args[1])
}

pub unsafe extern "C" fn builtin_pow(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("pow() received a null argv pointer");
    };
    if !(2..=3).contains(&args.len()) {
        return fail(format!("pow() expected 2 or 3 arguments, got {}", args.len()));
    }
    let Ok(base) = arg_i64(args[0], "pow") else {
        return ptr::null_mut();
    };
    let Ok(exp) = arg_i64(args[1], "pow") else {
        return ptr::null_mut();
    };
    if exp < 0 {
        return fail("negative integer exponent is not implemented");
    }
    let mut value = base.saturating_pow(exp as u32);
    if args.len() == 3 {
        let Ok(modulus) = arg_i64(args[2], "pow") else {
            return ptr::null_mut();
        };
        if modulus == 0 {
            return fail("pow() 3rd argument cannot be 0");
        }
        value = value.rem_euclid(modulus);
    }
    unsafe { abi::pon_const_int(value) }
}

pub unsafe extern "C" fn builtin_bytes(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { crate::abi::str_::builtin_bytes(argv, argc) }
}
pub unsafe extern "C" fn builtin_bytearray(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { crate::abi::str_::builtin_bytearray(argv, argc) }
}

pub unsafe extern "C" fn builtin_memoryview(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { crate::abi::str_::builtin_memoryview(argv, argc) }
}


pub unsafe extern "C" fn builtin_chr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 1, "chr") }) else {
        return ptr::null_mut();
    };
    let Ok(value) = arg_i64(args[0], "chr") else {
        return ptr::null_mut();
    };
    let Some(ch) = char::from_u32(value as u32) else {
        return fail("chr() arg not in range");
    };
    alloc_str(&ch.to_string())
}

pub unsafe extern "C" fn builtin_dir(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    alloc_sequence(SequenceKind::List, Vec::new())
}

pub unsafe extern "C" fn builtin_issubclass(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 2, "issubclass") }) else {
        return ptr::null_mut();
    };
    let result = unsafe { class_is_subclass(args[0], args[1]) };
    if result < 0 {
        return ptr::null_mut();
    }
    unsafe { abi::number::pon_const_bool(result) }
}

/// `issubclass(cls, classinfo)` core: tuple-of-classes recursion (first hit
/// wins), then real class objects (metatype below `type`) get full MRO plus
/// metaclass `__subclasscheck__` semantics; bare native shims keep the
/// historical name comparison.  Returns 1/0 and -1 with a pending exception.
unsafe fn class_is_subclass(cls: *mut PyObject, classinfo: *mut PyObject) -> c_int {
    if let Some(entries) = unsafe { exact_tuple_entries(classinfo) } {
        for entry in entries.iter().copied() {
            let result = unsafe { class_is_subclass(cls, entry) };
            if result != 0 {
                return result;
            }
        }
        return 0;
    }
    if unsafe { is_real_class(cls) && is_real_class(classinfo) } {
        return unsafe { crate::descr::issubclass(cls, classinfo) };
    }
    i32::from(unsafe { type_object_name(cls) == type_object_name(classinfo) })
}

/// True for objects whose metatype inherits from the builtin `type`.
unsafe fn is_real_class(object: *mut PyObject) -> bool {
    if object.is_null() {
        return false;
    }
    let meta = unsafe { (*object).ob_type.cast_mut() };
    !meta.is_null() && unsafe { crate::mro::is_subtype(meta, abi::runtime_type_type()) }
}

pub unsafe extern "C" fn builtin_int(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("int() received a null argv pointer");
    };
    crate::types::int::construct_from_args(args)
}

pub unsafe extern "C" fn builtin_float(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { numeric_constructor(argv, argc, "float") }
}

pub unsafe extern "C" fn builtin_complex(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("complex() received a null argv pointer");
    };
    crate::types::complex_::construct_from_args(args)
}

pub unsafe extern "C" fn builtin_bool(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("bool() received a null argv pointer");
    };
    match args.len() {
        0 => unsafe { abi::number::pon_const_bool(0) },
        1 => match unsafe { truth(args[0]) } {
            Ok(value) => unsafe { abi::number::pon_const_bool(i32::from(value)) },
            Err(message) => fail_preserving(message),
        },
        _ => fail(format!("bool() expected at most 1 argument, got {}", args.len())),
    }
}

pub unsafe extern "C" fn builtin_list(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { sequence_constructor(argv, argc, SequenceKind::List, "list") }
}

pub unsafe extern "C" fn builtin_tuple(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { sequence_constructor(argv, argc, SequenceKind::Tuple, "tuple") }
}

pub unsafe extern "C" fn builtin_dict(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("dict() received a null argv pointer");
    };
    // `dict(*args, **kwargs)`: keyword entries ride a trailing marker
    // appended by the keyword binder and merge AFTER the positional source
    // (later duplicates win, matching CPython's update order).
    let (args, kw_pairs) = match args.split_last() {
        Some((&last, rest)) => match unsafe { crate::types::lazy_iter::kw_marker_pairs(last) } {
            Some(pairs) => (rest, pairs),
            None => (args, &[][..]),
        },
        None => (args, &[][..]),
    };
    if args.len() > 1 {
        return fail(format!("dict expected at most 1 argument, got {}", args.len()));
    }
    let mut pairs = Vec::new();
    if let Some(&source) = args.first() {
        if unsafe { collect_dict_update_pairs(source, &mut pairs) }.is_err() {
            return ptr::null_mut();
        }
    }
    for &(name, value) in kw_pairs {
        let Some(text) = crate::intern::resolve(name) else {
            return fail("dict() keyword name is not interned");
        };
        let key = alloc_str(&text);
        if key.is_null() {
            return ptr::null_mut();
        }
        pairs.push(key);
        pairs.push(value);
    }
    unsafe { abi::map::pon_build_map(pairs.as_mut_ptr(), pairs.len() / 2) }
}

/// Flattens `source` into `[k0, v0, k1, v1, ...]` per CPython's dict-update
/// protocol: exact dicts copy entries in insertion order; anything else must be
/// an iterable of length-2 iterables. On failure the CPython-shaped
/// TypeError/ValueError is already raised and `Err(())` is returned.
pub(crate) unsafe fn collect_dict_update_pairs(source: *mut PyObject, pairs: &mut Vec<*mut PyObject>) -> Result<(), ()> {
    if unsafe { collect_mapping_pairs(source, pairs) }? {
        return Ok(());
    }
    let Ok(elements) = collect_iterable(source) else {
        let name = unsafe { crate::types::dict::type_name(source) }.unwrap_or("object");
        let message = format!("'{name}' object is not iterable");
        let _ = unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
        return Err(());
    };
    pairs.reserve(elements.len() * 2);
    for (index, element) in elements.into_iter().enumerate() {
        let Ok(pair) = collect_iterable(element) else {
            // CPython 3.14 surfaces the bare iteration failure here (no
            // element index): `dict([42])` -> TypeError: object is not iterable
            let message = "object is not iterable";
            let _ = unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
            return Err(());
        };
        if pair.len() != 2 {
            let message = format!(
                "dictionary update sequence element #{index} has length {}; 2 is required",
                pair.len()
            );
            let _ = unsafe { crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) };
            return Err(());
        }
        pairs.extend(pair);
    }
    Ok(())
}

/// Mapping-only pair collection, the first two legs of the dict-update
/// protocol: dict-layout storage snapshot, then the `keys()` mapping
/// protocol (`for key in source.keys(): source[key]`).  `Ok(true)` means
/// `pairs` was filled; `Ok(false)` means `source` is not a mapping — the
/// caller picks its own diagnostic (`dict.update` falls through to the
/// pairs-iterable leg, `f(**x)` raises CPython's "argument after ** must be
/// a mapping").  `Err(())` propagates with the error already raised.
pub(crate) unsafe fn collect_mapping_pairs(source: *mut PyObject, pairs: &mut Vec<*mut PyObject>) -> Result<bool, ()> {
    // Dict-layout sources (exact dicts AND dict-subclass instances) copy
    // concrete storage in insertion order, mirroring CPython's
    // `PyDict_Merge` which reads `ma_keys` directly for dict subclasses.
    if unsafe { crate::types::dict::has_dict_storage(source) } {
        let entries = match unsafe { crate::types::dict::dict_entries_snapshot(source) } {
            Ok(entries) => entries,
            Err(message) => {
                let _ = fail(message);
                return Err(());
            }
        };
        pairs.reserve(entries.len() * 2);
        for entry in entries {
            pairs.push(entry.key);
            pairs.push(entry.value);
        }
        return Ok(true);
    }
    match unsafe { mapping_keys_attr(source) } {
        Err(()) => Err(()),
        Ok(Some(keys_method)) => {
            let keys_iterable = unsafe { abi::pon_call(keys_method, ptr::null_mut(), 0) };
            if keys_iterable.is_null() {
                return Err(());
            }
            let keys = match collect_iterable(keys_iterable) {
                Ok(keys) => keys,
                Err(message) => {
                    let _ = fail(message);
                    return Err(());
                }
            };
            pairs.reserve(keys.len() * 2);
            for key in keys {
                // SAFETY: Subscript dispatch follows the NULL-sentinel error contract.
                let value = unsafe { crate::abstract_op::subscript_get(source, key) };
                if value.is_null() {
                    return Err(());
                }
                pairs.push(key);
                pairs.push(value);
            }
            Ok(true)
        }
        Ok(None) => Ok(false),
    }
}

/// Fetches `source.keys` when present: `Ok(Some)` is the bound attribute,
/// `Ok(None)` a clean miss (AttributeError cleared, per CPython's
/// `dict_update_arg` hasattr probe), `Err` a propagating lookup error.
unsafe fn mapping_keys_attr(source: *mut PyObject) -> Result<Option<*mut PyObject>, ()> {
    // Slotless native receivers (int, float, ...) cannot carry `keys` at all;
    // skipping the probe lets the pairs leg report CPython's "'int' object is
    // not iterable" instead of the slotless attribute-lookup diagnostic.
    let ty = unsafe { (*source).ob_type };
    if ty.is_null() || unsafe { (*ty).tp_getattro }.is_none() {
        return Ok(None);
    }
    let method = unsafe { crate::abstract_op::get_attr(source, intern("keys")) };
    if !method.is_null() {
        return Ok(Some(method));
    }
    if crate::abi::exc::pending_exception_object().is_none() || crate::abi::exc::pending_exception_is("AttributeError") {
        crate::thread_state::pon_err_clear();
        return Ok(None);
    }
    Err(())
}

/// `dict.fromkeys(iterable, value=None)`. A classmethod in CPython, so the
/// callable is a plain function: the receiver (type or instance) never joins
/// `argv`, and the result is always an exact runtime dict.
pub unsafe extern "C" fn builtin_dict_fromkeys(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("fromkeys() received a null argv pointer");
    };
    if args.is_empty() {
        return fail("fromkeys expected at least 1 argument, got 0");
    }
    if args.len() > 2 {
        return fail(format!("fromkeys expected at most 2 arguments, got {}", args.len()));
    }
    let value = args.get(1).copied().unwrap_or_else(|| unsafe { abi::pon_none() });
    let keys = match collect_iterable(args[0]) {
        Ok(keys) => keys,
        Err(message) => return fail(message),
    };
    let mut pairs = Vec::with_capacity(keys.len() * 2);
    for key in keys {
        pairs.push(key);
        pairs.push(value);
    }
    unsafe { abi::map::pon_build_map(pairs.as_mut_ptr(), pairs.len() / 2) }
}

/// Returns the plain function object backing `dict.fromkeys`.
pub(crate) fn dict_fromkeys_function() -> *mut PyObject {
    unsafe { abi::pon_make_function(builtin_dict_fromkeys as *const u8, VARIADIC_ARITY, intern("fromkeys")) }
}

pub unsafe extern "C" fn builtin_set(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { set_flavor_constructor(argv, argc, "set") }
}

pub unsafe extern "C" fn builtin_frozenset(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("frozenset() received a null argv pointer");
    };
    if args.len() == 1 && unsafe { crate::types::frozenset::is_frozenset(args[0]) } {
        return args[0];
    }
    unsafe { set_flavor_constructor(argv, argc, "frozenset") }
}

/// Builds a real runtime set/frozenset from an optional iterable argument.
unsafe fn set_flavor_constructor(argv: *mut *mut PyObject, argc: usize, name: &str) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail(format!("{name}() received a null argv pointer"));
    };
    if args.len() > 1 {
        return fail(format!("{name}() expected at most 1 argument, got {}", args.len()));
    }
    let mut items = if args.is_empty() {
        Vec::new()
    } else {
        match collect_iterable(args[0]) {
            Ok(items) => items,
            Err(message) => return fail(message),
        }
    };
    if name == "frozenset" {
        unsafe { abi::map::pon_build_frozenset(items.as_mut_ptr(), items.len()) }
    } else {
        unsafe { abi::map::pon_build_set(items.as_mut_ptr(), items.len()) }
    }
}

pub unsafe extern "C" fn builtin_slice(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("slice() received a null argv pointer");
    };
    if !(1..=3).contains(&args.len()) {
        return fail(format!("slice() expected 1 to 3 arguments, got {}", args.len()));
    }
    alloc_placeholder("slice")
}

pub unsafe extern "C" fn builtin_object(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 {
        return fail(format!("object() expected no arguments, got {argc}"));
    }
    let _ = argv;
    alloc_placeholder("object")
}

pub unsafe extern "C" fn builtin_super(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("super() received a null argv pointer");
    };
    let (start, obj) = match args.len() {
        0 => match infer_zero_arg_super() {
            Ok(pair) => pair,
            Err(message) => return fail(message),
        },
        2 => {
            let Some(start) = (unsafe { type_object(args[0]) }) else {
                return fail("super() argument 1 must be a type");
            };
            (start, args[1])
        }
        _ => return fail(format!("super() expected 0 or 2 arguments, got {}", args.len())),
    };
    unsafe { crate::types::super_::new_super(super_type(), start, obj) }
}

pub unsafe extern "C" fn builtin_property(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("property() received a null argv pointer");
    };
    if args.len() > 4 {
        return fail(format!("property() expected at most 4 arguments, got {}", args.len()));
    }
    // CPython's `property(fget=None, fset=None, fdel=None, doc=None)` treats
    // an explicit None exactly like an absent argument; `PyProperty` marks
    // absence with NULL, so normalize at the constructor boundary.
    let slot = |index: usize| {
        let value = args.get(index).copied().unwrap_or(ptr::null_mut());
        if is_none(value) { ptr::null_mut() } else { value }
    };
    unsafe { property::new_property(property_type(), slot(0), slot(1), slot(2), slot(3)) }
}

pub unsafe extern "C" fn builtin_classmethod(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 1, "classmethod") }) else {
        return ptr::null_mut();
    };
    args[0]
}

pub unsafe extern "C" fn builtin_staticmethod(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 1, "staticmethod") }) else {
        return ptr::null_mut();
    };
    args[0]
}

pub unsafe extern "C" fn builtin_build_class(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("__build_class__() received a null argv pointer");
    };
    if args.len() < 2 {
        return fail(format!("__build_class__() expected at least 2 arguments, got {}", args.len()));
    }
    let Some(name) = object_to_string(args[1]) else {
        return fail("__build_class__() class name must be str");
    };
    let name_interned = crate::intern::intern(&name);
    let bases = &args[2..];
    unsafe { abi::pon_build_class(args[0], name_interned, bases.as_ptr(), bases.len()) }
}

pub unsafe extern "C" fn builtin_callable(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 1, "callable") }) else {
        return ptr::null_mut();
    };
    let result = unsafe { is_callable_object(args[0]) };
    unsafe { abi::number::pon_const_bool(i32::from(result)) }
}

pub unsafe extern "C" fn builtin_globals(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { crate::dynexec::builtin_globals(argv, argc) }
}

pub unsafe extern "C" fn builtin_locals(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { crate::dynexec::builtin_locals(argv, argc) }
}

pub fn str_text(object: *mut PyObject) -> String {
    try_str_text(object).unwrap_or_else(|()| {
        pon_err_clear();
        "<object>".to_owned()
    })
}

/// `str(object)` with Python-level `__str__`/`__repr__` dispatch for heap
/// instances (CPython `PyObject_Str`).  `Err(())` leaves a Python exception
/// pending.
pub fn try_str_text(object: *mut PyObject) -> Result<String, ()> {
    if let Some(result) = dispatch_str_dunder(object, "__str__", crate::abi::object_dunder_str_carrier()) {
        return result;
    }
    if let Some(result) = dispatch_str_dunder(object, "__repr__", crate::abi::object_dunder_repr_carrier()) {
        return result;
    }
    if let Some(text) = object_to_string(object) {
        return Ok(text);
    }
    if let Some(result) = dispatch_text_slot(object, "__str__", |ty| ty.tp_str.or(ty.tp_repr)) {
        return result;
    }
    repr_text_no_dispatch(object)
}

pub fn repr_text(object: *mut PyObject) -> String {
    try_repr_text(object).unwrap_or_else(|()| {
        pon_err_clear();
        "<object>".to_owned()
    })
}

/// `repr(object)` with Python-level `__repr__` dispatch for heap instances
/// (CPython `PyObject_Repr`).  `Err(())` leaves a Python exception pending.
pub fn try_repr_text(object: *mut PyObject) -> Result<String, ()> {
    if let Some(result) = dispatch_str_dunder(object, "__repr__", crate::abi::object_dunder_repr_carrier()) {
        return result;
    }
    if let Some(result) = dispatch_text_slot(object, "__repr__", |ty| ty.tp_repr) {
        return result;
    }
    repr_text_no_dispatch(object)
}

/// Python-level `__str__`/`__repr__` dispatch: a heap-class receiver whose
/// MRO resolves `name` past object's default carrier calls the hook through
/// the descriptor protocol.  `None` keeps the native fallback text; builtin
/// receivers never dispatch (their reprs are the native branches below).
fn dispatch_str_dunder(object: *mut PyObject, name: &str, terminus: *mut PyObject) -> Option<Result<String, ()>> {
    if object.is_null() || !crate::tag::is_heap(object) {
        return None;
    }
    let ty = unsafe { (*object).ob_type.cast_mut() };
    if !crate::types::type_::type_dispatches_python_dunders(ty.cast_const()) {
        return None;
    }
    let hook = unsafe { crate::descr::lookup_in_type(ty, intern(name)) };
    if hook.is_null() || hook == terminus {
        return None;
    }
    let bound = unsafe { crate::descr::descriptor_get(hook, object, ty) };
    if bound.is_null() {
        return Some(Err(()));
    }
    let result = unsafe { abi::pon_call(bound, ptr::null_mut(), 0) };
    if result.is_null() {
        return Some(Err(()));
    }
    let Some(text) = object_to_string(result) else {
        let _ = fail(format!("{name} returned non-string"));
        return Some(Err(()));
    };
    Some(Ok(text))
}

/// Native `tp_repr`/`tp_str` slot dispatch (CPython `PyObject_Repr`/
/// `PyObject_Str` for extension types such as `re.Pattern`/`re.Match`): a
/// boxed receiver whose type registers the picked slot produces its text
/// through it.  `None` means "no slot registered" and keeps the native
/// whitelist fallback; a NULL slot result propagates the slot's pending
/// exception as `Err(())`.  `str()` picks `tp_str.or(tp_repr)`, mirroring
/// CPython's `PyObject_Str` fallback to `PyObject_Repr`.
fn dispatch_text_slot(
    object: *mut PyObject,
    name: &str,
    pick: fn(&PyType) -> Option<UnaryFunc>,
) -> Option<Result<String, ()>> {
    if object.is_null() || !crate::tag::is_heap(object) {
        return None;
    }
    // SAFETY: `object` is a heap pointer per the tag check above.
    let ty = unsafe { (*object).ob_type };
    if ty.is_null() {
        return None;
    }
    // SAFETY: non-NULL `ob_type` references a live type object.
    let slot = pick(unsafe { &*ty })?;
    // SAFETY: `slot` is a live slot function registered by the receiver's type.
    let result = unsafe { slot(object) };
    if result.is_null() {
        return Some(Err(()));
    }
    let Some(text) = object_to_string(result) else {
        let _ = fail(format!("{name} returned non-string"));
        return Some(Err(()));
    };
    Some(Ok(text))
}

/// Native repr whitelist (no Python-level dispatch).  `Err(())` propagates a
/// pending exception raised by a container element's `__repr__`.
pub(crate) fn repr_text_no_dispatch(object: *mut PyObject) -> Result<String, ()> {
    if object.is_null() {
        return Ok("<NULL>".to_owned());
    }
    if let Some(value) = unsafe { bool_::to_bool(object) } {
        return Ok(if value { "True".to_owned() } else { "False".to_owned() });
    }
    if let Some(value) = unsafe { crate::types::int::to_bigint(object) } {
        return Ok(value.to_string());
    }
    if let Some(value) = object_to_i64(object) {
        return Ok(value.to_string());
    }
    if unsafe { crate::types::float::is_exact_float(object) } {
        let float = unsafe { &*object.cast::<crate::types::float::PyFloat>() };
        return Ok(crate::types::float::repr_f64(float.value));
    }
    if let Some((real, imag)) = unsafe { crate::types::complex_::to_f64s(object) } {
        return Ok(crate::types::complex_::repr_complex(real, imag));
    }
    if let Some(text) = object_to_string(object) {
        return Ok(crate::types::str_::repr(&text));
    }
    if is_none(object) {
        return Ok("None".to_owned());
    }
    // `Ellipsis`/`NotImplemented`: named singleton reprs (the print path in
    // `format_object_for_print` already special-cases both; `ast.dump` and
    // container reprs reach them through `repr()`).
    if unsafe { crate::types::int::type_name_is(object, "ellipsis") } {
        return Ok("Ellipsis".to_owned());
    }
    if unsafe { crate::types::int::type_name_is(object, "NotImplementedType") } {
        return Ok("NotImplemented".to_owned());
    }
    if crate::types::typealias::is_type_alias(object) {
        return Ok(crate::types::typealias::type_alias_repr(object));
    }
    if crate::types::typealias::is_typevar(object) {
        return Ok(crate::types::typealias::typevar_repr(object));
    }
    if crate::types::typealias::is_generic_alias(object) {
        return Ok(crate::types::typealias::generic_alias_repr(object));
    }
    if crate::types::typealias::is_union_type(object) {
        return Ok(crate::types::typealias::union_repr(object));
    }
    unsafe {
        if let Some(native) = as_native(object) {
            return match &native.payload {
                NativePayload::Range { start, stop, step } => {
                    if *step == 1 {
                        Ok(format!("range({start}, {stop})"))
                    } else {
                        Ok(format!("range({start}, {stop}, {step})"))
                    }
                }
                NativePayload::LongRange { start, stop, step } => {
                    if *step == BigInt::from(1) {
                        Ok(format!("range({start}, {stop})"))
                    } else {
                        Ok(format!("range({start}, {stop}, {step})"))
                    }
                }
                NativePayload::RangeIterator { .. } => Ok("<range_iterator object>".to_owned()),
                NativePayload::LongRangeIterator { .. } => Ok("<longrange_iterator object>".to_owned()),
                NativePayload::Enumerate { .. } => Ok("<enumerate object>".to_owned()),
                NativePayload::Zip { .. } => Ok("<zip object>".to_owned()),
                NativePayload::Map { .. } => Ok("<map object>".to_owned()),
                NativePayload::Filter { .. } => Ok("<filter object>".to_owned()),
                NativePayload::CallableSentinelIterator { .. } => Ok("<callable_iterator object>".to_owned()),
                NativePayload::Placeholder(name) => Ok(format!("<{name} object>")),
            };
        }
        // Dict-subclass instances repr like dicts (CPython inherits
        // `dict.__repr__`).  Checked before the name whitelist below, which
        // returns None for user-defined class names.
        if crate::types::dict::is_dict_subclass_instance(object) {
            return dict_repr(object);
        }
        if let Some(name) = type_name(object) {
            if name == "function" {
                let function = &*object.cast::<PyFunction>();
                let fname = resolve(function.name_interned).unwrap_or_else(|| "<lambda>".to_owned());
                if matches!(fname.as_str(), "int" | "str" | "bool" | "float") {
                    return Ok(format!("<class '{fname}'>"));
                }
                return Ok(format!("<function {fname}>"));
            }
            if name == "type" {
                let ty = &*object.cast::<PyType>();
                return Ok(format!("<class '{}'>", ty.name()));
            }
            if name == "list" {
                match crate::types::list::list_repr(object) {
                    Ok(text) => return Ok(text),
                    Err(_) if pon_err_occurred() => return Err(()),
                    Err(_) => {}
                }
            }
            if name == "tuple" {
                let tuple = &*object.cast::<crate::types::tuple::PyTuple>();
                return format_sequence(SequenceKind::Tuple, tuple.as_slice());
            }
            if name == "dict" {
                return dict_repr(object);
            }
            if name == "bytes" {
                let bytes = &*object.cast::<crate::types::bytes_::PyBytes>();
                return Ok(crate::types::bytes_::repr(bytes.as_slice()));
            }
            if name == "bytearray" {
                let bytearray = &*object.cast::<crate::types::bytearray_::PyByteArray>();
                return Ok(crate::types::bytearray_::repr(bytearray.as_slice()));
            }
            if name == "memoryview" {
                let view = object.cast::<crate::types::memoryview::PyMemoryView>();
                return Ok(format!("<memory at {}>", crate::types::bytes_::repr(&crate::types::memoryview::tobytes(view))));
            }
            if name == "set" {
                if let Ok(entries) = crate::types::set_::entries_snapshot(object) {
                    return if entries.is_empty() {
                        Ok("set()".to_owned())
                    } else {
                        Ok(format!("{{{}}}", join_repr(&entries)?))
                    };
                }
            }
            if name == "frozenset" {
                if let Ok(entries) = crate::types::frozenset::entries_snapshot(object) {
                    return if entries.is_empty() {
                        Ok("frozenset()".to_owned())
                    } else {
                        Ok(format!("frozenset({{{}}})", join_repr(&entries)?))
                    };
                }
            }
            return Ok(format!("<{name} object>"));
        }
    }
    Ok("<object>".to_owned())
}

fn format_sequence(kind: SequenceKind, items: &[*mut PyObject]) -> Result<String, ()> {
    Ok(match kind {
        SequenceKind::List => format!("[{}]", join_repr(items)?),
        SequenceKind::Tuple => {
            if items.len() == 1 {
                format!("({},)", try_repr_text(items[0])?)
            } else {
                format!("({})", join_repr(items)?)
            }
        }
    })
}

fn join_repr(items: &[*mut PyObject]) -> Result<String, ()> {
    let mut parts = Vec::with_capacity(items.len());
    for item in items.iter().copied() {
        parts.push(try_repr_text(item)?);
    }
    Ok(parts.join(", "))
}

fn dict_repr(object: *mut PyObject) -> Result<String, ()> {
    let entries = match unsafe { crate::types::dict::dict_entries_snapshot(object) } {
        Ok(entries) => entries,
        Err(message) => {
            let _ = fail(message);
            return Err(());
        }
    };
    let mut parts = Vec::with_capacity(entries.len());
    for entry in entries {
        parts.push(format!("{}: {}", try_repr_text(entry.key)?, try_repr_text(entry.value)?));
    }
    Ok(format!("{{{}}}", parts.join(", ")))
}

unsafe fn argv_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
    if argc == 0 {
        Some(&[])
    } else if argv.is_null() {
        None
    } else {
        Some(unsafe { std::slice::from_raw_parts(argv, argc) })
    }
}

unsafe fn exact_args<'a>(argv: *mut *mut PyObject, argc: usize, expected: usize, name: &str) -> Option<&'a [*mut PyObject]> {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        pon_err_set(format!("{name}() received a null argv pointer"));
        return None;
    };
    if args.len() != expected {
        pon_err_set(format!("{name}() expected {expected} arguments, got {}", args.len()));
        return None;
    }
    Some(args)
}

fn fail(message: impl Into<String>) -> *mut PyObject {
    pon_err_set(message);
    ptr::null_mut()
}

/// Like [`fail`], but a pending typed exception (raised by a nested protocol
/// call such as `__bool__`/`__len__` dispatch) wins over the fallback
/// message, so `except TypeError:`-style handlers keep working.
fn fail_preserving(message: impl Into<String>) -> *mut PyObject {
    if !pon_err_occurred() {
        pon_err_set(message);
    }
    ptr::null_mut()
}

fn stop_iteration() -> *mut PyObject {
    unsafe { abi::pon_raise_stop_iteration(ptr::null_mut()) }
}
fn stop_iteration_pending() -> bool {
    pon_err_message().is_some_and(|message| message.starts_with("StopIteration"))
}


fn alloc_str(text: &str) -> *mut PyObject {
    unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn object_to_i64(object: *mut PyObject) -> Option<i64> {
    if object.is_null() {
        return None;
    }
    if let Some(value) = unsafe { bool_::to_bool(object) } {
        return Some(i64::from(value));
    }
    unsafe { crate::types::int::to_bigint(object).and_then(|value| value.to_i64()) }
}

fn object_to_f64(object: *mut PyObject) -> Option<f64> {
    if let Some(value) = unsafe { crate::types::float::to_f64(object) } {
        return Some(value);
    }
    object_to_i64(object).map(|value| value as f64)
}

fn object_to_string(object: *mut PyObject) -> Option<String> {
    if object.is_null() {
        return None;
    }
    unsafe {
        let ty = (*object).ob_type;
        if ty.is_null() {
            return None;
        }
        if (*ty).name() == "str" {
            return (*object.cast::<PyUnicode>()).as_str().map(ToOwned::to_owned);
        }
        exception_message_text(object, ty)
    }
}

unsafe fn exception_message_text(object: *mut PyObject, mut ty: *const PyType) -> Option<String> {
    let original_name = unsafe { (*ty).name() };
    let mut derives_os_error = false;
    while !ty.is_null() {
        if unsafe { (*ty).name() == "OSError" } {
            derives_os_error = true;
        }
        if unsafe { (*ty).name() == "BaseException" } {
            // SAFETY: Reaching BaseException in the type chain proves compatible layout.
            let exception = unsafe { &*object.cast::<PyBaseException>() };
            // CPython `BaseException.__str__`: `len(args) != 1` stringifies
            // the whole args tuple; the stored tuple is non-NULL exactly for
            // multi-argument constructors.
            if !exception.args.is_null() {
                // `OSError.__str__` overrides the tuple shape for the
                // errno-carrying constructions (2..=5 args).
                if derives_os_error {
                    if let Some(text) = unsafe { os_error_str(exception.args) } {
                        return Some(text);
                    }
                }
                return Some(repr_text(exception.args));
            }
            let message = exception.message;
            if message.is_null() {
                return Some(String::new());
            }
            if original_name == "KeyError" {
                return Some(repr_text(message));
            }
            return object_to_string(message);
        }
        // SAFETY: `ty` is a live type object from an object header.
        ty = unsafe { (*ty).tp_base.cast_const() };
    }
    None
}

/// CPython `OSError.__str__` for errno-carrying constructions: 2..=5
/// positional args render `[Errno n] strerror`, appending the optional
/// filename (`args[2]`) and filename2 (`args[4]`) as reprs
/// (`: 'src' -> 'dst'`); filename2 only prints alongside filename, exactly
/// the C `oserror_str` branch order.  `None` falls back to
/// `BaseException.__str__`'s args-tuple repr.
unsafe fn os_error_str(args: *mut PyObject) -> Option<String> {
    let items = unsafe { exact_tuple_entries(args) }?;
    if !(2..=5).contains(&items.len()) {
        return None;
    }
    let errno_text = str_text(items[0]);
    let strerror_text = str_text(items[1]);
    // pon spells None as a NULL slot (post-untag), so "filename is not None"
    // reads as a non-NULL check.
    let filename = items.get(2).copied().filter(|&slot| !crate::tag::untag_arg(slot).is_null());
    let filename2 = (items.len() == 5)
        .then(|| items[4])
        .filter(|&slot| !crate::tag::untag_arg(slot).is_null());
    Some(match (filename, filename2) {
        (Some(name), Some(name2)) => {
            format!("[Errno {errno_text}] {strerror_text}: {} -> {}", repr_text(name), repr_text(name2))
        }
        (Some(name), None) => format!("[Errno {errno_text}] {strerror_text}: {}", repr_text(name)),
        (None, _) => format!("[Errno {errno_text}] {strerror_text}"),
    })
}

unsafe fn type_name(object: *mut PyObject) -> Option<&'static str> {
    if object.is_null() {
        return None;
    }
    let ty = unsafe { (*object).ob_type };
    if ty.is_null() {
        return None;
    }
    let name = unsafe { (*ty).name() };
    Some(match name {
        "int" => "int",
        "bool" => "bool",
        "str" => "str",
        "function" => "function",
        "NoneType" => "NoneType",
        "type" => "type",
        "list" => "list",
        "tuple" => "tuple",
        "set" => "set",
        "frozenset" => "frozenset",
        "bytes" => "bytes",
        "bytearray" => "bytearray",
        "memoryview" => "memoryview",
        "dict" => "dict",
        "range" => "range",
        "range_iterator" => "range_iterator",
        "list_iterator" => "list_iterator",
        "enumerate" => "enumerate",
        "zip" => "zip",
        "map" => "map",
        "filter" => "filter",
        "method" => "method",
        "object" => "object",
        _ => return None,
    })
}

fn is_none(object: *mut PyObject) -> bool {
    unsafe { type_name(object).is_some_and(|name| name == "NoneType") }
}

fn arg_i64(object: *mut PyObject, owner: &str) -> Result<i64, *mut PyObject> {
    object_to_i64(object).ok_or_else(|| fail(format!("{owner}() expected int argument")))
}

/// Coerces a range bound to a `BigInt`, accepting exact ints and bools like
/// CPython; other objects raise a CPython-shaped, catchable `TypeError`.
fn arg_bigint(object: *mut PyObject) -> Result<BigInt, *mut PyObject> {
    if object.is_null() {
        return Err(fail("range() expected int argument"));
    }
    if let Some(value) = unsafe { crate::types::int::to_bigint_including_bool(object) } {
        return Ok(value);
    }
    let name = unsafe { crate::types::dict::type_name(object) }.unwrap_or("object");
    let message = format!("'{name}' object cannot be interpreted as an integer");
    Err(unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) })
}

unsafe fn type_object(object: *mut PyObject) -> Option<*mut PyType> {
    // Metatype-MRO-aware: accepts classes whose metatype is a `type` subclass
    // (ABCMeta, user metaclasses), matching `super(SomeABC, obj)` in CPython.
    if unsafe { crate::types::type_::is_type_object(object) } {
        Some(object.cast::<PyType>())
    } else {
        None
    }
}
unsafe fn is_callable_object(object: *mut PyObject) -> bool {
    // CPython `callable(o)` is `type(o)->tp_call != NULL`.  pon's function/
    // method/type objects dispatch through dedicated `pon_call` fast paths
    // without tp_call slots, so they are named explicitly; everything else is
    // callable when its type carries tp_call or its MRO defines `__call__`
    // (heap instances go through the DunderCall path).
    // Tagged small ints are ints: never callable, and must not be
    // dereferenced as heap pointers.
    if crate::tag::is_small_int(object) {
        return false;
    }
    if unsafe { type_name(object).is_some_and(|name| matches!(name, "function" | "method" | "type")) } {
        return true;
    }
    if object.is_null() {
        return false;
    }
    let ty = unsafe { (*object).ob_type.cast_mut() };
    if ty.is_null() {
        return false;
    }
    if unsafe { (*ty).tp_call.is_some() } {
        return true;
    }
    !unsafe { crate::descr::lookup_in_type(ty, crate::intern::intern(crate::intern::DUNDER_CALL)) }.is_null()
}


/// The builtin classmethod/staticmethod carrier type objects, from the
/// runtime global table (NULL entries when the runtime is not initialized
/// never match).
fn carrier_types() -> [*mut PyType; 2] {
    let lookup = |name: &str| {
        abi::runtime_global(crate::intern::intern(name))
            .map_or(core::ptr::null_mut(), |object| object.cast::<PyType>())
    };
    [lookup("classmethod"), lookup("staticmethod")]
}

/// The wrapped callable when `value` is a classmethod/staticmethod carrier
/// (exact type match against `carrier_types`).  Zero-arg `super()` must see
/// through carriers: class creation implicitly wraps a plain-function
/// `__new__` in a staticmethod (`wrap_dunder_new_as_staticmethod`), and
/// `classmethod(f)`/`staticmethod(f)` store carriers in the class dict while
/// the executing frame holds the naked function.
fn carrier_payload(value: *mut PyObject, carrier_types: &[*mut PyType; 2]) -> Option<*mut PyObject> {
    if !crate::tag::is_heap(value) {
        return None;
    }
    let ty = unsafe { (*value).ob_type.cast_mut() };
    if ty.is_null() || !carrier_types.contains(&ty) {
        return None;
    }
    // SAFETY: Carrier layout verified above; PyClassMethod and PyStaticMethod
    // carry the wrapped callable at the same offset.
    Some(unsafe { (*value.cast::<crate::types::classmethod::PyClassMethod>()).callable })
}

fn find_defining_class(function: *mut PyObject, ty: *mut PyType) -> Option<*mut PyType> {
    let carriers = carrier_types();
    for class in unsafe { crate::mro::mro_entries(ty) } {
        if class.is_null() {
            continue;
        }
        let dict = unsafe { (*class).tp_dict };
        if dict.is_null() {
            continue;
        }
        let dict = unsafe { &*dict.cast::<crate::types::type_::PyClassDict>() };
        if dict.iter().any(|(_, value)| value == function || carrier_payload(value, &carriers) == Some(function)) {
            return Some(class);
        }
    }
    None
}

fn infer_zero_arg_super() -> Result<(*mut PyType, *mut PyObject), String> {
    let mut saw_call_with_args = false;
    for (function, self_arg) in abi::current_call_snapshots() {
        saw_call_with_args = true;
        let obj_type = unsafe { (*self_arg).ob_type.cast_mut() };
        if obj_type.is_null() {
            continue;
        }
        if let Some(class) = find_defining_class(function, obj_type) {
            return Ok((class, self_arg));
        }
        // Metaclass methods receive a class as `self`; the defining class
        // then lives in the receiver's own MRO, not its metatype's.
        if unsafe { crate::mro::is_subtype(obj_type, abi::runtime_type_type()) } {
            if let Some(class) = find_defining_class(function, self_arg.cast::<PyType>()) {
                return Ok((class, self_arg));
            }
        }
    }
    if saw_call_with_args {
        Err("super(): current function was not found in the receiver MRO".to_owned())
    } else if abi::current_function_object().is_null() {
        Err("super(): no current function".to_owned())
    } else {
        Err("super(): no arguments".to_owned())
    }
}

fn length(object: *mut PyObject) -> Result<i64, String> {
    // `len(str)` counts Unicode code points, not UTF-8 bytes; read through
    // payload subclasses and never treat exception messages as sized.
    if let Some(text) = unsafe { crate::types::type_::unicode_text(object) } {
        return Ok(crate::types::str_::codepoint_len(text) as i64);
    }
    unsafe {
        if let Some(native) = as_native(object) {
            return match &native.payload {
                NativePayload::Range { start, stop, step } => Ok(range_len(*start, *stop, *step)),
                NativePayload::LongRange { start, stop, step } => longrange_len(start, stop, step)
                    .to_i64()
                    .ok_or_else(|| "Python int too large to convert to C ssize_t".to_owned()),
                _ => Err(len_type_error(object)),
            };
        }
        let ty = (*object).ob_type;
        if !ty.is_null() {
            if let Some(slot) = (*ty).tp_as_sequence.as_ref().and_then(|methods| methods.sq_length) {
                let len = slot(object);
                if len >= 0 {
                    return Ok(len as i64);
                }
                return Err("__len__ returned a negative value".to_owned());
            }
            if let Some(slot) = (*ty).tp_as_mapping.as_ref().and_then(|methods| methods.mp_length) {
                let len = slot(object);
                if len >= 0 {
                    return Ok(len as i64);
                }
                return Err("mapping __len__ returned a negative value".to_owned());
            }
        }
        // Python-level `__len__` on heap instances (e.g. WeakSet).
        if !ty.is_null() {
            let hook = crate::descr::lookup_in_type(ty.cast_mut(), intern("__len__"));
            if !hook.is_null() {
                let bound = crate::descr::descriptor_get(hook, object, ty.cast_mut());
                if bound.is_null() {
                    return Err("__len__ descriptor binding failed".to_owned());
                }
                let result = abi::pon_call(bound, ptr::null_mut(), 0);
                if result.is_null() {
                    return Err("__len__ call failed".to_owned());
                }
                let Some(len) = object_to_i64(result) else {
                    return Err("__len__ must return an int".to_owned());
                };
                if len < 0 {
                    return Err("__len__ returned a negative value".to_owned());
                }
                return Ok(len);
            }
        }
    }
    Err(len_type_error(object))
}

/// CPython text for unsized receivers: `object of type 'X' has no len()`.
fn len_type_error(object: *mut PyObject) -> String {
    let name = unsafe {
        if object.is_null() || (*object).ob_type.is_null() { "object" } else { (*(*object).ob_type).name() }
    };
    format!("object of type '{name}' has no len()")
}

fn range_len(start: i64, stop: i64, step: i64) -> i64 {
    if step > 0 {
        if start >= stop {
            0
        } else {
            ((stop - start - 1) / step) + 1
        }
    } else if start <= stop {
        0
    } else {
        ((start - stop - 1) / -step) + 1
    }
}

fn longrange_len(start: &BigInt, stop: &BigInt, step: &BigInt) -> BigInt {
    let (diff, step_abs) = if step.is_positive() {
        (stop - start, step.clone())
    } else {
        (start - stop, -step.clone())
    };
    if diff.is_positive() {
        (diff + &step_abs - BigInt::from(1)) / step_abs
    } else {
        BigInt::from(0)
    }
}

unsafe fn truth(object: *mut PyObject) -> Result<bool, String> {
    let result = unsafe { abi::pon_is_true(object) };
    match result {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("truth-value testing failed".to_owned()),
    }
}

fn collect_iterable(object: *mut PyObject) -> Result<Vec<*mut PyObject>, String> {
    let iter = unsafe { pon_get_iter(object, ptr::null_mut()) };
    if iter.is_null() {
        return Err("object is not iterable".to_owned());
    }
    let mut items = Vec::new();
    loop {
        let value = unsafe { pon_iter_next(iter, ptr::null_mut()) };
        if value.is_null() {
            if pon_err_occurred() {
                // StopIteration is exhaustion; any other pending exception is
                // a genuine error and must stay set for the caller (the
                // `Err` message below is discarded by `pon_err_set`, which
                // never replaces a live boxed exception).
                if !crate::abi::exc::pending_exception_is("StopIteration") {
                    return Err("iteration raised an exception".to_owned());
                }
                pon_err_clear();
            }
            break;
        }
        items.push(value);
    }
    Ok(items)
}

unsafe fn call_function(function: *mut PyObject, args: &mut [*mut PyObject]) -> *mut PyObject {
    unsafe { abi::pon_call(function, args.as_mut_ptr(), args.len()) }
}

fn compare_for_sort(a: *mut PyObject, b: *mut PyObject, key_func: Option<*mut PyObject>) -> Ordering {
    let key_a = key_for_sort(a, key_func);
    let key_b = key_for_sort(b, key_func);
    key_a.cmp(&key_b)
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SortKey {
    Int(i64),
    Text(String),
}

impl Ord for SortKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Int(lhs), Self::Int(rhs)) => lhs.cmp(rhs),
            (Self::Text(lhs), Self::Text(rhs)) => lhs.cmp(rhs),
            (Self::Int(_), Self::Text(_)) => Ordering::Less,
            (Self::Text(_), Self::Int(_)) => Ordering::Greater,
        }
    }
}

impl PartialOrd for SortKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn key_for_sort(object: *mut PyObject, key_func: Option<*mut PyObject>) -> SortKey {
    let keyed = if let Some(function) = key_func {
        let mut args = [object];
        let result = unsafe { call_function(function, &mut args) };
        if result.is_null() {
            object
        } else {
            result
        }
    } else {
        object
    };
    object_to_i64(keyed)
        .map(SortKey::Int)
        .unwrap_or_else(|| SortKey::Text(str_text(keyed)))
}

fn iterate_truth(object: *mut PyObject, all_mode: bool) -> Result<bool, String> {
    for item in collect_iterable(object)? {
        let item_truth = unsafe { truth(item) }?;
        if all_mode && !item_truth {
            return Ok(false);
        }
        if !all_mode && item_truth {
            return Ok(true);
        }
    }
    Ok(all_mode)
}

unsafe fn min_max(argv: *mut *mut PyObject, argc: usize, max_mode: bool) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("min/max received a null argv pointer");
    };
    if args.is_empty() {
        return fail("min/max expected at least 1 argument");
    }
    let items = if args.len() == 1 {
        match collect_iterable(args[0]) {
            Ok(items) => items,
            Err(message) => return fail(message),
        }
    } else {
        args.to_vec()
    };
    let Some(mut best) = items.first().copied() else {
        return fail("min/max arg is an empty sequence");
    };
    for item in items.into_iter().skip(1) {
        let ordering = compare_for_sort(item, best, None);
        if (max_mode && ordering.is_gt()) || (!max_mode && ordering.is_lt()) {
            best = item;
        }
    }
    best
}

unsafe fn numeric_constructor(argv: *mut *mut PyObject, argc: usize, name: &str) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail(format!("{name}() received a null argv pointer"));
    };
    match args.len() {
        0 if name == "float" => crate::types::float::from_f64(0.0),
        0 => unsafe { abi::pon_const_int(0) },
        1 if name == "float" => {
            if let Some(value) = object_to_f64(args[0]) {
                crate::types::float::from_f64(value)
            } else if let Some(text) = object_to_string(args[0]) {
                match text.parse::<f64>() {
                    Ok(value) => crate::types::float::from_f64(value),
                    Err(_) => fail(format!("could not convert string to float: {text}")),
                }
            } else {
                fail("float() expected int, float, or str argument")
            }
        }
        1 => {
            if let Some(value) = object_to_i64(args[0]) {
                unsafe { abi::pon_const_int(value) }
            } else if let Some(value) = object_to_f64(args[0]) {
                unsafe { abi::pon_const_int(value as i64) }
            } else if let Some(text) = object_to_string(args[0]) {
                match text.parse::<i64>() {
                    Ok(value) => unsafe { abi::pon_const_int(value) },
                    Err(_) => fail(format!("invalid literal for {name}(): {text}")),
                }
            } else {
                fail(format!("{name}() expected int, float, or str argument"))
            }
        }
        _ => fail(format!("{name}() expected at most 1 argument, got {}", args.len())),
    }
}

unsafe fn sequence_constructor(
    argv: *mut *mut PyObject,
    argc: usize,
    kind: SequenceKind,
    name: &str,
) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail(format!("{name}() received a null argv pointer"));
    };
    if args.len() > 1 {
        return fail(format!("{name}() expected at most 1 argument, got {}", args.len()));
    }
    let items = if args.is_empty() {
        Vec::new()
    } else {
        match collect_iterable(args[0]) {
            Ok(items) => items,
            Err(message) => return fail(message),
        }
    };
    alloc_sequence(kind, items)
}

fn is_builtin_class_name(name: &str) -> bool {
    matches!(
        name,
        "bool"
            | "bytearray"
            | "bytes"
            | "dict"
            | "float"
            | "frozenset"
            | "int"
            | "list"
            | "memoryview"
            | "object"
            | "range"
            | "set"
            | "str"
            | "tuple"
    )
}

/// `isinstance(object, classinfo)` core, shaped like CPython's
/// `PyObject_IsInstance`: exact-type fast path, tuple-of-classes recursion,
/// union args, builtin-name shims, then real-class dispatch through
/// `descr::isinstance` (metaclass `__instancecheck__` hook or default MRO
/// walk).  Returns 1/0 and -1 with a pending exception.
unsafe fn object_is_instance(object: *mut PyObject, classinfo: *mut PyObject) -> c_int {
    if object.is_null() || classinfo.is_null() {
        return 0;
    }
    // `type(obj) is cls` wins before any dispatch, so a metaclass
    // `__instancecheck__` hook is NOT consulted for exact matches.
    if unsafe { (*object).ob_type }.cast_mut().cast::<PyObject>() == classinfo {
        return 1;
    }
    // Tuple of classes: first hit short-circuits, before later entries are
    // even validated (CPython parity).
    if let Some(entries) = unsafe { exact_tuple_entries(classinfo) } {
        for entry in entries.iter().copied() {
            let result = unsafe { object_is_instance(object, entry) };
            if result != 0 {
                return result;
            }
        }
        return 0;
    }
    if crate::types::typealias::is_union_type(classinfo) {
        for arg in crate::types::typealias::union_args(classinfo).iter().copied() {
            let result = unsafe { object_is_instance(object, arg) };
            if result != 0 {
                return result;
            }
        }
        return 0;
    }
    if let Some(expected_name) = unsafe { type_object_name(classinfo) }.filter(|name| is_builtin_class_name(name)) {
        let object_type = unsafe { (*object).ob_type };
        if object_type.is_null() {
            return 0;
        }
        let matches_exact =
            unsafe { (*object_type).name() == expected_name || ((*object_type).name() == "bool" && expected_name == "int") };
        if matches_exact {
            return 1;
        }
        // Builtin-name classinfo with a subclass receiver: accept when any
        // MRO ancestor is the named builtin (e.g. `isinstance(D(), dict)`
        // for `class D(dict)`), excluding heap types so a user class merely
        // NAMED like a builtin does not qualify.
        return i32::from(unsafe { crate::mro::mro_entries(object_type.cast_mut()) }.iter().any(|ancestor| {
            !ancestor.is_null()
                && unsafe {
                    (**ancestor).gc_type_id != crate::types::type_::TYPE_ID_HEAP_INSTANCE.0 as usize
                        && (**ancestor).name() == expected_name
                }
        }));
    }
    let classinfo_type = unsafe { (*classinfo).ob_type.cast_mut() };
    if !classinfo_type.is_null() && unsafe { crate::mro::is_subtype(classinfo_type, abi::runtime_type_type()) } {
        return unsafe { crate::descr::isinstance(object, classinfo) };
    }
    let object_type = unsafe { (*object).ob_type };
    if object_type.is_null() {
        return 0;
    }
    let Some(expected_name) = (unsafe { type_object_name(classinfo) }) else {
        return 0;
    };
    i32::from(unsafe { (*object_type).name() == expected_name })
}

/// Elements of an exact seq-family `PyTuple` (isinstance classinfo,
/// exception args).
unsafe fn exact_tuple_entries<'a>(object: *mut PyObject) -> Option<&'a [*mut PyObject]> {
    unsafe { abi::seq::exact_tuple_slice(object) }
}


unsafe fn type_object_name(object: *mut PyObject) -> Option<String> {
    if object.is_null() {
        return None;
    }
    let ty = unsafe { (*object).ob_type };
    if !ty.is_null() && unsafe { (*ty).name() } == "type" {
        return Some(unsafe { (*object.cast::<PyType>()).name() }.to_owned());
    }
    if !ty.is_null() && unsafe { (*ty).name() } == "function" {
        let name = resolve(unsafe { (*object.cast::<PyFunction>()).name_interned })?;
        if matches!(
            name.as_str(),
            "bool"
                | "bytes"
                | "dict"
                | "float"
                | "int"
                | "list"
                | "memoryview"
                | "object"
                | "property"
                | "range"
                | "set"
                | "slice"
                | "str"
                | "super"
                | "tuple"
        ) {
            return Some(name);
        }
    }
    unsafe { type_name(object).map(ToOwned::to_owned) }
}


fn stable_hash(text: &str) -> i64 {
    let mut hash: i64 = 0xcbf29ce484222325_u64 as i64;
    for byte in text.bytes() {
        hash ^= i64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    if hash == -1 { -2 } else { hash }
}

#[cfg(test)]
mod tests {
    use std::ptr;
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::*;
    use crate::object::PyLong;
    use crate::thread_state::{pon_err_clear, test_state_lock};

    static CALL_COUNTER: AtomicI64 = AtomicI64::new(0);

    unsafe extern "C" fn counter_callable(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
        let value = CALL_COUNTER.fetch_add(1, Ordering::SeqCst);
        unsafe { abi::pon_const_int(value) }
    }

    fn init_runtime() {
        assert_eq!(unsafe { abi::pon_runtime_init() }, 0);
        pon_err_clear();
    }

    fn int_value(object: *mut PyObject) -> i64 {
        assert!(!object.is_null());
        unsafe { (*object.cast::<PyLong>()).value }
    }

    #[test]
    fn next_default_returns_default_after_stop_iteration() {
        let _guard = test_state_lock();
        init_runtime();
        let mut range_args = [unsafe { abi::pon_const_int(0) }];
        let range = unsafe { builtin_range(range_args.as_mut_ptr(), range_args.len()) };
        assert!(!range.is_null());
        let mut iter_args = [range];
        let iter = unsafe { builtin_iter(iter_args.as_mut_ptr(), iter_args.len()) };
        assert!(!iter.is_null());
        let default = unsafe { abi::pon_const_int(42) };
        let mut next_args = [iter, default];
        let value = unsafe { builtin_next(next_args.as_mut_ptr(), next_args.len()) };
        assert_eq!(value, default);
        assert!(!crate::thread_state::pon_err_occurred());
    }

    #[test]
    fn iter_callable_sentinel_stops_on_equal_value() {
        let _guard = test_state_lock();
        init_runtime();
        CALL_COUNTER.store(0, Ordering::SeqCst);
        let callable = unsafe { abi::pon_make_function(counter_callable as *const u8, 0, intern("counter_callable")) };
        assert!(!callable.is_null());
        let sentinel = unsafe { abi::pon_const_int(2) };
        let mut iter_args = [callable, sentinel];
        let iter = unsafe { builtin_iter(iter_args.as_mut_ptr(), iter_args.len()) };
        assert!(!iter.is_null());

        let mut next_args = [iter];
        let first = unsafe { builtin_next(next_args.as_mut_ptr(), next_args.len()) };
        assert_eq!(int_value(first), 0);
        let second = unsafe { builtin_next(next_args.as_mut_ptr(), next_args.len()) };
        assert_eq!(int_value(second), 1);

        let default = unsafe { abi::pon_const_int(99) };
        let mut default_args = [iter, default];
        let stopped = unsafe { builtin_next(default_args.as_mut_ptr(), default_args.len()) };
        assert_eq!(stopped, default);
        assert!(!crate::thread_state::pon_err_occurred());
    }

    #[test]
    fn object_placeholder_exposes_bound_str_method() {
        let _guard = test_state_lock();
        init_runtime();
        let mut args = [];
        let object = unsafe { builtin_object(args.as_mut_ptr(), args.len()) };
        assert!(!object.is_null());
        let method = unsafe { abi::pon_get_attr(object, intern("__str__"), ptr::null_mut()) };
        assert!(!method.is_null());
        assert_eq!(unsafe { type_name(method) }, Some("method"));
        let text = unsafe { abi::pon_call(method, ptr::null_mut(), 0) };
        assert!(!text.is_null());
        assert_eq!(object_to_string(text).as_deref(), Some("<object object>"));
    }

    unsafe extern "C" fn fixed_repr_slot(_object: *mut PyObject) -> *mut PyObject {
        alloc_str("<slot repr>")
    }

    unsafe extern "C" fn fixed_str_slot(_object: *mut PyObject) -> *mut PyObject {
        alloc_str("slot str")
    }

    unsafe extern "C" fn failing_repr_slot(_object: *mut PyObject) -> *mut PyObject {
        fail("slot repr failed")
    }

    /// Header-only instance of a fresh leaked native type carrying the given
    /// text slots (the `re.Pattern`/`re.Match` shape: a boxed extension type
    /// outside the repr whitelist).
    fn slot_test_object(tp_repr: Option<UnaryFunc>, tp_str: Option<UnaryFunc>) -> *mut PyObject {
        let mut ty = Box::new(PyType::new(ptr::null(), "slottest", std::mem::size_of::<PyObjectHeader>()));
        ty.tp_repr = tp_repr;
        ty.tp_str = tp_str;
        let ty = Box::into_raw(ty);
        Box::into_raw(Box::new(PyObjectHeader::new(ty))).cast::<PyObject>()
    }

    #[test]
    fn repr_and_str_consult_native_text_slots() {
        let _guard = test_state_lock();
        init_runtime();
        // No slots: the whitelist fallback stays "<object>" for unknown types.
        let bare = slot_test_object(None, None);
        assert_eq!(repr_text(bare), "<object>");
        // tp_repr only: repr() uses it and str() falls back to it (CPython
        // `PyObject_Str` with a NULL `tp_str`).
        let repr_only = slot_test_object(Some(fixed_repr_slot), None);
        assert_eq!(repr_text(repr_only), "<slot repr>");
        assert_eq!(str_text(repr_only), "<slot repr>");
        // Both slots: str() prefers tp_str, repr() keeps tp_repr.
        let both = slot_test_object(Some(fixed_repr_slot), Some(fixed_str_slot));
        assert_eq!(str_text(both), "slot str");
        assert_eq!(repr_text(both), "<slot repr>");
    }

    #[test]
    fn failing_text_slot_propagates_pending_exception() {
        let _guard = test_state_lock();
        init_runtime();
        let object = slot_test_object(Some(failing_repr_slot), None);
        assert!(try_repr_text(object).is_err());
        assert!(crate::thread_state::pon_err_occurred());
        // The infallible wrapper clears the pending error and keeps the
        // legacy fallback text.
        assert_eq!(repr_text(object), "<object>");
        assert!(!crate::thread_state::pon_err_occurred());
    }
}
