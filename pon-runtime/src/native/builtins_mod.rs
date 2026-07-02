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
use num_traits::ToPrimitive;

use crate::abi::{self, pon_get_iter, pon_iter_next};
use crate::intern::{intern, resolve};
use crate::object::{PyFunction, PyObject, PyObjectHeader, PyType, PyUnicode};
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
    builtin!("__import__", VARIADIC_ARITY, builtin___import__);
    builtin!("vars", VARIADIC_ARITY, super::builtins_batch::builtin_vars);
    builtin!("ord", 1, super::builtins_batch::builtin_ord);
    builtin!("bin", 1, super::builtins_batch::builtin_bin);
    builtin!("oct", 1, super::builtins_batch::builtin_oct);
    builtin!("hex", 1, super::builtins_batch::builtin_hex);
    builtin!("reversed", 1, super::builtins_batch::builtin_reversed);
    builtin!("bytearray", VARIADIC_ARITY, builtin_bytearray);
    builtin!("memoryview", 1, builtin_memoryview);
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
    Set,
    Dict,
}

#[derive(Debug)]
enum NativePayload {
    Sequence { kind: SequenceKind, items: Vec<*mut PyObject> },
    Range { start: i64, stop: i64, step: i64 },
    RangeIterator { current: i64, stop: i64, step: i64 },
    VecIterator { items: Vec<*mut PyObject>, index: usize },
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
static SET_TYPE: OnceLock<usize> = OnceLock::new();
static DICT_TYPE: OnceLock<usize> = OnceLock::new();
static RANGE_TYPE: OnceLock<usize> = OnceLock::new();
static RANGE_ITER_TYPE: OnceLock<usize> = OnceLock::new();
static SEQ_ITER_TYPE: OnceLock<usize> = OnceLock::new();
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
        ty.tp_bool = Some(native_bool_slot);
        ty.tp_hash = Some(native_hash_slot);
        Box::into_raw(ty) as usize
    }) as *mut PyType
}

fn list_type() -> *mut PyType {
    let ty = type_from(&LIST_TYPE, "list", Some(sequence_iter_slot), None);
    unsafe {
        (*ty).tp_richcmp = Some(native_list_richcmp_slot);
    }
    ty
}

fn tuple_type() -> *mut PyType {
    let ty = type_from(&TUPLE_TYPE, "tuple", Some(sequence_iter_slot), None);
    unsafe {
        (*ty).tp_richcmp = Some(native_tuple_richcmp_slot);
    }
    ty
}

fn set_type() -> *mut PyType {
    type_from(&SET_TYPE, "set", Some(sequence_iter_slot), None)
}

fn dict_type() -> *mut PyType {
    type_from(&DICT_TYPE, "dict", Some(sequence_iter_slot), None)
}

fn range_type() -> *mut PyType {
    type_from(&RANGE_TYPE, "range", Some(range_iter_slot), None)
}

fn range_iter_type() -> *mut PyType {
    type_from(&RANGE_ITER_TYPE, "range_iterator", Some(identity_iter_slot), Some(native_next_slot))
}

fn seq_iter_type() -> *mut PyType {
    type_from(&SEQ_ITER_TYPE, "list_iterator", Some(identity_iter_slot), Some(native_next_slot))
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

fn property_type() -> *mut PyType {
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
        "dict" => dict_type(),
        "set" => set_type(),
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
    match name {
        "__str__" | "__repr__" => bound_placeholder_method(object, name, placeholder_str_method),
        _ => fail(format!("attribute '{name}' was not found")),
    }
}

fn bound_placeholder_method(
    receiver: *mut PyObject,
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> *mut PyObject {
    let function = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match crate::types::method::new_bound_method(function, receiver) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => fail(message),
    }
}

unsafe extern "C" fn placeholder_str_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 1, "object.__str__") }) else {
        return ptr::null_mut();
    };
    alloc_str(&repr_text(args[0]))
}


fn alloc_native(payload: NativePayload, ty: *mut PyType) -> *mut PyObject {
    Box::into_raw(Box::new(NativeObject {
        ob_base: PyObjectHeader::new(ty),
        payload,
    }))
    .cast::<PyObject>()
}

fn alloc_sequence(kind: SequenceKind, items: Vec<*mut PyObject>) -> *mut PyObject {
    let ty = match kind {
        SequenceKind::List => list_type(),
        SequenceKind::Tuple => tuple_type(),
        SequenceKind::Set => set_type(),
        SequenceKind::Dict => dict_type(),
    };
    alloc_native(NativePayload::Sequence { kind, items }, ty)
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
        set_type(),
        dict_type(),
        range_type(),
        range_iter_type(),
        seq_iter_type(),
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

unsafe extern "C" fn sequence_iter_slot(object: *mut PyObject) -> *mut PyObject {
    let Some(native) = (unsafe { as_native(object) }) else {
        return fail("sequence iterator receiver is not native");
    };
    let NativePayload::Sequence { items, .. } = &native.payload else {
        return fail("sequence iterator receiver is not a sequence");
    };
    alloc_native(
        NativePayload::VecIterator {
            items: items.clone(),
            index: 0,
        },
        seq_iter_type(),
    )
}

unsafe extern "C" fn range_iter_slot(object: *mut PyObject) -> *mut PyObject {
    let Some(native) = (unsafe { as_native(object) }) else {
        return fail("range iterator receiver is not native");
    };
    let (start, stop, step) = match &native.payload {
        NativePayload::Range { start, stop, step } => (*start, *stop, *step),
        _ => return fail("range iterator receiver is not a range"),
    };
    alloc_range_iter(start, stop, step)
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
        NativePayload::VecIterator { items, index } => {
            let Some(value) = items.get(*index).copied() else {
                return stop_iteration();
            };
            *index += 1;
            value
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
                Err(message) => return fail(message),
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
                Err(message) => fail(message),
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
        NativePayload::Sequence { items, .. } => if items.is_empty() { 0 } else { 1 },
        NativePayload::Range { start, stop, step } => {
            if (*step > 0 && *start < *stop) || (*step < 0 && *start > *stop) { 1 } else { 0 }
        }
        _ => 1,
    }
}

unsafe extern "C" fn native_hash_slot(object: *mut PyObject) -> isize {
    stable_hash(&repr_text(object)) as isize
}
unsafe extern "C" fn native_list_richcmp_slot(left: *mut PyObject, right: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe { native_sequence_richcmp(left, right, op, SequenceKind::List) }
}

unsafe extern "C" fn native_tuple_richcmp_slot(left: *mut PyObject, right: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe { native_sequence_richcmp(left, right, op, SequenceKind::Tuple) }
}

unsafe fn native_sequence_richcmp(
    left: *mut PyObject,
    right: *mut PyObject,
    op: c_int,
    kind: SequenceKind,
) -> *mut PyObject {
    let Some(left_items) = (unsafe { native_sequence_items(left, kind) }) else {
        return unsafe { abi::pon_not_implemented() };
    };
    let Some(right_items) = (unsafe { native_sequence_items(right, kind) }) else {
        return unsafe { abi::pon_not_implemented() };
    };
    let Ok(op) = u8::try_from(op) else {
        return fail("unknown rich comparison operation");
    };
    if !matches!(
        op,
        crate::abstract_op::RICH_LT
            | crate::abstract_op::RICH_LE
            | crate::abstract_op::RICH_EQ
            | crate::abstract_op::RICH_NE
            | crate::abstract_op::RICH_GT
            | crate::abstract_op::RICH_GE
    ) {
        return fail("unknown rich comparison operation");
    }

    for index in 0..left_items.len().min(right_items.len()) {
        let equal = unsafe {
            abi::pon_rich_compare(
                crate::abstract_op::RICH_EQ,
                left_items[index],
                right_items[index],
                ptr::null_mut(),
            )
        };
        if equal.is_null() {
            return ptr::null_mut();
        }
        let is_equal = match unsafe { truth(equal) } {
            Ok(value) => value,
            Err(message) => return fail(message),
        };
        if !is_equal {
            return match op {
                crate::abstract_op::RICH_EQ => unsafe { abi::number::pon_const_bool(0) },
                crate::abstract_op::RICH_NE => unsafe { abi::number::pon_const_bool(1) },
                crate::abstract_op::RICH_LT
                | crate::abstract_op::RICH_LE
                | crate::abstract_op::RICH_GT
                | crate::abstract_op::RICH_GE => unsafe {
                    abi::pon_rich_compare(op, left_items[index], right_items[index], ptr::null_mut())
                },
                _ => unreachable!(),
            };
        }
    }

    let result = match op {
        crate::abstract_op::RICH_EQ => left_items.len() == right_items.len(),
        crate::abstract_op::RICH_NE => left_items.len() != right_items.len(),
        crate::abstract_op::RICH_LT => left_items.len() < right_items.len(),
        crate::abstract_op::RICH_LE => left_items.len() <= right_items.len(),
        crate::abstract_op::RICH_GT => left_items.len() > right_items.len(),
        crate::abstract_op::RICH_GE => left_items.len() >= right_items.len(),
        _ => unreachable!(),
    };
    unsafe { abi::number::pon_const_bool(i32::from(result)) }
}

unsafe fn native_sequence_items(object: *mut PyObject, kind: SequenceKind) -> Option<Vec<*mut PyObject>> {
    let native = unsafe { as_native(object) }?;
    match &native.payload {
        NativePayload::Sequence { kind: actual, items } if *actual == kind => Some(items.clone()),
        _ => None,
    }
}


pub unsafe extern "C" fn builtin_print(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("print() received a null argv pointer");
    };
    let mut stdout = io::stdout().lock();
    for (index, value) in args.iter().copied().enumerate() {
        if index != 0 && write!(stdout, " ").is_err() {
            return fail("failed to write stdout");
        }
        let text = str_text(value);
        if write!(stdout, "{text}").is_err() {
            return fail("failed to write stdout");
        }
    }
    if writeln!(stdout).and_then(|()| stdout.flush()).is_err() {
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

pub unsafe extern "C" fn builtin___import__(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { crate::dynexec::builtin___import__(argv, argc) }
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
            let Ok(stop) = arg_i64(args[0], "range") else {
                return ptr::null_mut();
            };
            (0, stop, 1)
        }
        2 => {
            let Ok(start) = arg_i64(args[0], "range") else {
                return ptr::null_mut();
            };
            let Ok(stop) = arg_i64(args[1], "range") else {
                return ptr::null_mut();
            };
            (start, stop, 1)
        }
        3 => {
            let Ok(step) = arg_i64(args[2], "range") else {
                return ptr::null_mut();
            };
            if step == 0 {
                return fail("range() arg 3 must not be zero");
            }
            let Ok(start) = arg_i64(args[0], "range") else {
                return ptr::null_mut();
            };
            let Ok(stop) = arg_i64(args[1], "range") else {
                return ptr::null_mut();
            };
            (start, stop, step)
        }
        _ => return fail(format!("range() expected 1 to 3 arguments, got {}", args.len())),
    };
    alloc_range(start, stop, step)
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
    unsafe { abi::number::pon_const_bool(i32::from(result)) }
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
    alloc_str(&repr_text(args[0]))
}

pub unsafe extern "C" fn builtin_str(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("str() received a null argv pointer");
    };
    match args.len() {
        0 => alloc_str(""),
        1 => alloc_str(&str_text(args[0])),
        _ => fail("str() encoding/errors forms are not implemented"),
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
    let value = if let Some(value) = unsafe { bool_::to_bool(args[0]) } {
        i64::from(value)
    } else if let Some(value) = unsafe { crate::types::int::to_bigint(args[0]) } {
        crate::types::int::hash_bigint(&value) as i64
    } else if unsafe { crate::types::float::is_exact_float(args[0]) } {
        let float = unsafe { &*args[0].cast::<crate::types::float::PyFloat>() };
        crate::types::float::hash_f64(float.value) as i64
    } else if let Some((real, imag)) = unsafe { crate::types::complex_::to_f64s(args[0]) } {
        crate::types::complex_::hash_complex(real, imag) as i64
    } else {
        stable_hash(&repr_text(args[0]))
    };
    unsafe { abi::pon_const_int(value) }
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
            Err(message) => return fail(message),
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
        Err(message) => fail(message),
    }
}

pub unsafe extern "C" fn builtin_any(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 1, "any") }) else {
        return ptr::null_mut();
    };
    match iterate_truth(args[0], false) {
        Ok(value) => unsafe { abi::number::pon_const_bool(i32::from(value)) },
        Err(message) => fail(message),
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
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return fail("bytes() received a null argv pointer");
    };
    if args.len() > 1 {
        return fail(format!("bytes() expected at most 1 argument, got {}", args.len()));
    }
    alloc_placeholder("bytes")
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
    let result = unsafe { type_object_name(args[0]) == type_object_name(args[1]) };
    unsafe { abi::number::pon_const_bool(i32::from(result)) }
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
            Err(message) => fail(message),
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
    if args.len() > 1 {
        return fail(format!("dict() expected at most 1 argument, got {}", args.len()));
    }
    alloc_sequence(SequenceKind::Dict, Vec::new())
}

pub unsafe extern "C" fn builtin_set(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { sequence_constructor(argv, argc, SequenceKind::Set, "set") }
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
    unsafe {
        property::new_property(
            property_type(),
            args.first().copied().unwrap_or(ptr::null_mut()),
            args.get(1).copied().unwrap_or(ptr::null_mut()),
            args.get(2).copied().unwrap_or(ptr::null_mut()),
            args.get(3).copied().unwrap_or(ptr::null_mut()),
        )
    }
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
    let result = unsafe { type_name(args[0]).is_some_and(|name| matches!(name, "function" | "method" | "type")) };
    unsafe { abi::number::pon_const_bool(i32::from(result)) }
}

pub unsafe extern "C" fn builtin_globals(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { crate::dynexec::builtin_globals(argv, argc) }
}

pub unsafe extern "C" fn builtin_locals(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { crate::dynexec::builtin_locals(argv, argc) }
}

pub fn str_text(object: *mut PyObject) -> String {
    if let Some(text) = object_to_string(object) {
        return text;
    }
    repr_text(object)
}

pub fn repr_text(object: *mut PyObject) -> String {
    if object.is_null() {
        return "<NULL>".to_owned();
    }
    if let Some(value) = unsafe { bool_::to_bool(object) } {
        return if value { "True".to_owned() } else { "False".to_owned() };
    }
    if let Some(value) = unsafe { crate::types::int::to_bigint(object) } {
        return value.to_string();
    }
    if let Some(value) = object_to_i64(object) {
        return value.to_string();
    }
    if unsafe { crate::types::float::is_exact_float(object) } {
        let float = unsafe { &*object.cast::<crate::types::float::PyFloat>() };
        return crate::types::float::repr_f64(float.value);
    }
    if let Some((real, imag)) = unsafe { crate::types::complex_::to_f64s(object) } {
        return crate::types::complex_::repr_complex(real, imag);
    }
    if let Some(text) = object_to_string(object) {
        return format!("'{text}'");
    }
    if is_none(object) {
        return "None".to_owned();
    }
    if crate::types::typealias::is_type_alias(object) {
        return crate::types::typealias::type_alias_repr(object);
    }
    if crate::types::typealias::is_typevar(object) {
        return crate::types::typealias::typevar_repr(object);
    }
    if crate::types::typealias::is_generic_alias(object) {
        return crate::types::typealias::generic_alias_repr(object);
    }
    unsafe {
        if let Some(native) = as_native(object) {
            return match &native.payload {
                NativePayload::Sequence { kind, items } => format_sequence(*kind, items),
                NativePayload::Range { start, stop, step } => {
                    if *step == 1 {
                        format!("range({start}, {stop})")
                    } else {
                        format!("range({start}, {stop}, {step})")
                    }
                }
                NativePayload::RangeIterator { .. } => "<range_iterator object>".to_owned(),
                NativePayload::VecIterator { .. } => "<list_iterator object>".to_owned(),
                NativePayload::Enumerate { .. } => "<enumerate object>".to_owned(),
                NativePayload::Zip { .. } => "<zip object>".to_owned(),
                NativePayload::Map { .. } => "<map object>".to_owned(),
                NativePayload::Filter { .. } => "<filter object>".to_owned(),
                NativePayload::CallableSentinelIterator { .. } => "<callable_iterator object>".to_owned(),
                NativePayload::Placeholder(name) => format!("<{name} object>"),
            };
        }
        if let Some(name) = type_name(object) {
            if name == "function" {
                let function = &*object.cast::<PyFunction>();
                let fname = resolve(function.name_interned).unwrap_or_else(|| "<lambda>".to_owned());
                if matches!(fname.as_str(), "int" | "str" | "bool" | "float") {
                    return format!("<class '{fname}'>");
                }
                return format!("<function {fname}>");
            }
            if name == "type" {
                let ty = &*object.cast::<PyType>();
                return format!("<class '{}'>", ty.name());
            }
            if name == "list" {
                if let Ok(text) = crate::types::list::list_repr(object) {
                    return text;
                }
            }
            if name == "tuple" {
                let tuple = &*object.cast::<crate::types::tuple::PyTuple>();
                return format_sequence(SequenceKind::Tuple, tuple.as_slice());
            }
            if name == "dict" {
                if let Ok(text) = dict_repr(object) {
                    return text;
                }
            }
            if name == "bytes" {
                let bytes = &*object.cast::<crate::types::bytes_::PyBytes>();
                return crate::types::bytes_::repr(bytes.as_slice());
            }
            if name == "bytearray" {
                let bytearray = &*object.cast::<crate::types::bytearray_::PyByteArray>();
                return crate::types::bytearray_::repr(bytearray.as_slice());
            }
            if name == "memoryview" {
                let view = object.cast::<crate::types::memoryview::PyMemoryView>();
                return format!("<memory at {}>", crate::types::bytes_::repr(&crate::types::memoryview::tobytes(view)));
            }
            return format!("<{name} object>");
        }
    }
    "<object>".to_owned()
}

fn format_sequence(kind: SequenceKind, items: &[*mut PyObject]) -> String {
    match kind {
        SequenceKind::List => format!("[{}]", join_repr(items)),
        SequenceKind::Tuple => {
            if items.len() == 1 {
                format!("({},)", repr_text(items[0]))
            } else {
                format!("({})", join_repr(items))
            }
        }
        SequenceKind::Set => {
            if items.is_empty() {
                "set()".to_owned()
            } else {
                format!("{{{}}}", join_repr(items))
            }
        }
        SequenceKind::Dict => "{}".to_owned(),
    }
}

fn join_repr(items: &[*mut PyObject]) -> String {
    items.iter().copied().map(repr_text).collect::<Vec<_>>().join(", ")
}

fn dict_repr(object: *mut PyObject) -> Result<String, String> {
    let entries = unsafe { crate::types::dict::dict_entries_snapshot(object)? };
    let mut parts = Vec::with_capacity(entries.len());
    for entry in entries {
        parts.push(format!("{}: {}", repr_text(entry.key), repr_text(entry.value)));
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
    while !ty.is_null() {
        if unsafe { (*ty).name() == "BaseException" } {
            // SAFETY: Reaching BaseException in the type chain proves compatible layout.
            let message = unsafe { (*object.cast::<PyBaseException>()).message };
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

unsafe fn type_object(object: *mut PyObject) -> Option<*mut PyType> {
    if object.is_null() {
        return None;
    }
    let ty = unsafe { (*object).ob_type };
    if ty.is_null() || unsafe { (*ty).name() } != "type" {
        return None;
    }
    Some(object.cast::<PyType>())
}
unsafe fn is_callable_object(object: *mut PyObject) -> bool {
    unsafe { type_name(object).is_some_and(|name| matches!(name, "function" | "method" | "type")) }
}


fn infer_zero_arg_super() -> Result<(*mut PyType, *mut PyObject), String> {
    let mut saw_call_with_args = false;
    for (function, self_arg) in abi::current_call_snapshots() {
        saw_call_with_args = true;
        let obj_type = unsafe { (*self_arg).ob_type.cast_mut() };
        if obj_type.is_null() {
            continue;
        }
        for class in unsafe { crate::mro::mro_entries(obj_type) } {
            if class.is_null() {
                continue;
            }
            let dict = unsafe { (*class).tp_dict };
            if dict.is_null() {
                continue;
            }
            let dict = unsafe { &*dict.cast::<crate::types::type_::PyClassDict>() };
            if dict.iter().any(|(_, value)| value == function) {
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
    if let Some(text) = object_to_string(object) {
        return Ok(text.len() as i64);
    }
    unsafe {
        if let Some(native) = as_native(object) {
            return match &native.payload {
                NativePayload::Sequence { items, .. } => Ok(items.len() as i64),
                NativePayload::Range { start, stop, step } => Ok(range_len(*start, *stop, *step)),
                _ => Err("object of this native type has no len()".to_owned()),
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
    }
    Err("object has no len()".to_owned())
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

unsafe fn object_is_instance(object: *mut PyObject, classinfo: *mut PyObject) -> bool {
    if object.is_null() || classinfo.is_null() {
        return false;
    }
    let classinfo_type = unsafe { (*classinfo).ob_type };
    if !classinfo_type.is_null() && unsafe { (*classinfo_type).name() } == "type" {
        return unsafe { crate::descr::isinstance(object, classinfo) > 0 };
    }
    let object_type = unsafe { (*object).ob_type };
    if object_type.is_null() {
        return false;
    }
    let Some(expected_name) = (unsafe { type_object_name(classinfo) }) else {
        return false;
    };
    unsafe { (*object_type).name() == expected_name }
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
}
