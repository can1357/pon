//! Native `_collections`: `deque` + `defaultdict` (WS-IMPORT: `contextlib` /
//! `pprint` -> `unittest`).
//!
//! The vendored `Lib/collections/__init__.py` guards every `_collections`
//! import with `try/except ImportError`.  Only two names lack a pure-Python
//! fallback and are hard requirements by downstream modules: `deque`
//! (`contextlib` does `from collections import deque` at module scope) and
//! `defaultdict` (`pprint` reads `collections.defaultdict` at import).  The
//! `OrderedDict`/`_tuplegetter`/`_count_elements` accelerators stay absent —
//! the except-ImportError arms in `collections/__init__.py` cover them.
//!
//! The deque is a correct simple model over `VecDeque`: `maxlen` discards
//! from the opposite end on `append`/`appendleft` like CPython.  Instances
//! are immortal leaked boxes (the `_contextvars` pattern); the Python values
//! they hold are reported as GC roots through [`gc_held_roots`].
//!
//! `defaultdict` is a real dict-layout heap class built over the
//! dict-subclass substrate (`PyDictSubclassInstance`): instances inherit the
//! whole native dict protocol, `default_factory` lives in the instance
//! attribute dict, and the factory fires through the `__missing__` hook in
//! `pon_dict_get_item`'s miss path (CPython `dict_subscript` parity).

use core::ffi::c_int;
use std::collections::VecDeque;
use std::ptr;
use std::sync::{LazyLock, Mutex};

use crate::abi;
use crate::abstract_op::{RICH_EQ, RICH_NE};
use crate::intern::intern;
use crate::object::{PyObject, PyObjectHeader, PySequenceMethods, PyType};
use crate::thread_state::pon_err_clear;
use crate::types::exc::ExceptionKind;
use crate::types::type_::unicode_text;

use super::builtins_mod::VARIADIC_ARITY;
use super::install_module;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

// ---------------------------------------------------------------------------
// Layouts

#[repr(C)]
struct PyDeque {
    ob_base: PyObjectHeader,
    entries: VecDeque<*mut PyObject>,
    maxlen: Option<usize>,
}

#[repr(C)]
struct PyDequeIter {
    ob_base: PyObjectHeader,
    deque: *mut PyDeque,
    index: usize,
}

// ---------------------------------------------------------------------------
// Types

static DEQUE_SEQUENCE: PySequenceMethods = PySequenceMethods {
    sq_length: Some(deque_len_slot),
    sq_contains: Some(deque_contains_slot),
    ..PySequenceMethods::EMPTY
};

static DEQUE_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(
        abi::runtime_type_type().cast_const(),
        "collections.deque",
        std::mem::size_of::<PyDeque>(),
    );
    ty.tp_base = abi::runtime_global(intern("object"))
        .map_or(ptr::null_mut(), |object| object.cast::<PyType>());
    ty.tp_new = Some(deque_new);
    ty.tp_getattro = Some(deque_getattro);
    ty.tp_repr = Some(deque_repr);
    ty.tp_str = Some(deque_repr);
    ty.tp_bool = Some(deque_bool);
    ty.tp_iter = Some(deque_iter);
    ty.tp_richcmp = Some(deque_richcmp_slot);
    ty.tp_as_sequence = ptr::addr_of!(DEQUE_SEQUENCE).cast_mut();
    Box::into_raw(Box::new(ty)) as usize
});

static DEQUE_ITER_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(
        abi::runtime_type_type().cast_const(),
        "_collections._deque_iterator",
        std::mem::size_of::<PyDequeIter>(),
    );
    ty.tp_iter = Some(identity_iter);
    ty.tp_iternext = Some(deque_iter_next);
    Box::into_raw(Box::new(ty)) as usize
});

fn deque_type() -> *mut PyType {
    *DEQUE_TYPE as *mut PyType
}

// ---------------------------------------------------------------------------
// Registry (GC roots) and allocation

/// Every deque/iterator allocation, for GC root reporting.  Objects are
/// immortal leaked boxes, so the registry only grows.
static REGISTRY: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// GC roots held by native deques: every stored entry.  Consumed by
/// `crate::abi::collect` while the runtime lock is held; must not re-enter
/// the runtime.  Type pointers are read without forcing the `LazyLock`
/// (uninitialized types mean no deques exist yet).
pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
    let registry = REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut roots = Vec::new();
    for &object in registry.iter() {
        let deque = object as *mut PyDeque;
        // SAFETY: Registry members are live leaked boxes of PyDeque layout.
        for &entry in unsafe { &(*deque).entries } {
            if !entry.is_null() {
                roots.push(entry);
            }
        }
    }
    roots
}

fn alloc_deque(entries: VecDeque<*mut PyObject>, maxlen: Option<usize>) -> *mut PyObject {
    let object = Box::into_raw(Box::new(PyDeque {
        ob_base: PyObjectHeader::new(deque_type()),
        entries,
        maxlen,
    }));
    REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner()).push(object as usize);
    object.cast::<PyObject>()
}

unsafe fn as_deque<'a>(object: *mut PyObject) -> Option<&'a mut PyDeque> {
    let object = untag(object);
    if object.is_null() {
        return None;
    }
    // SAFETY: A non-NULL heap object carries a live header.
    if unsafe { (*object).ob_type } != deque_type().cast_const() {
        return None;
    }
    // SAFETY: The type check above proved the layout.
    Some(unsafe { &mut *object.cast::<PyDeque>() })
}

// ---------------------------------------------------------------------------
// Helpers (contextvars idioms)

fn untag(object: *mut PyObject) -> *mut PyObject {
    crate::tag::untag_arg(object)
}

fn fail(message: impl Into<String>) -> *mut PyObject {
    crate::thread_state::pon_err_set(message);
    ptr::null_mut()
}

fn none() -> *mut PyObject {
    // SAFETY: Singleton accessor.
    unsafe { abi::pon_none() }
}

fn alloc_str_object(text: &str) -> *mut PyObject {
    // SAFETY: Runtime allocation helper; NULL on failure with the error set.
    unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

fn raise_index_error(message: &str) -> *mut PyObject {
    abi::exc::raise_kind_error_text(ExceptionKind::IndexError, message)
}

unsafe fn arg_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
    if argc == 0 {
        Some(&[])
    } else if argv.is_null() {
        None
    } else {
        // SAFETY: The caller passed `argc` live argument slots.
        Some(unsafe { std::slice::from_raw_parts(argv, argc) })
    }
}

fn bound_method(receiver: *mut PyObject, name: &str, entry: BuiltinFn) -> *mut PyObject {
    // SAFETY: `entry` is a live builtin entry point with the runtime calling convention.
    let function = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match crate::types::method::new_bound_method(function, receiver) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => fail(message),
    }
}

/// Drains a Python iterable into a VecDeque of untagged entries.
///
/// `pon_iter_next` signals exhaustion by RAISING a typed StopIteration
/// (itertools convention); it is consumed here, while any other pending
/// exception propagates to the caller as `Err`.
fn collect_iterable(iterable: *mut PyObject) -> Result<VecDeque<*mut PyObject>, ()> {
    // SAFETY: Iterator helpers follow the NULL-sentinel error contract.
    let iterator = unsafe { abi::pon_get_iter(iterable, ptr::null_mut()) };
    if iterator.is_null() {
        return Err(());
    }
    let mut out = VecDeque::new();
    loop {
        // SAFETY: `iterator` is live; NULL signals exhaustion or error.
        let item = unsafe { abi::pon_iter_next(iterator, ptr::null_mut()) };
        if item.is_null() {
            if abi::exc::pending_exception_is("StopIteration") {
                pon_err_clear();
                break;
            }
            if crate::thread_state::pon_err_occurred() {
                return Err(());
            }
            break;
        }
        out.push_back(untag(item));
    }
    Ok(out)
}

fn trim_to_maxlen(deque: &mut PyDeque, from_left: bool) {
    if let Some(maxlen) = deque.maxlen {
        while deque.entries.len() > maxlen {
            if from_left {
                deque.entries.pop_front();
            } else {
                deque.entries.pop_back();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// deque slots

unsafe extern "C" fn deque_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    let positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
        Ok(positional) => positional,
        Err(message) => return fail(message),
    };
    if positional.len() > 2 {
        return raise_type_error("deque() takes at most 2 arguments");
    }
    let mut iterable = positional.first().copied().map(untag);
    let mut maxlen_obj = positional.get(1).copied().map(untag);
    if !kwargs.is_null() {
        let entries = match unsafe { crate::types::dict::dict_entries_snapshot(kwargs) } {
            Ok(entries) => entries,
            Err(message) => return fail(message),
        };
        for entry in entries {
            match unsafe { unicode_text(untag(entry.key)) } {
                Some("iterable") => iterable = Some(untag(entry.value)),
                Some("maxlen") => maxlen_obj = Some(untag(entry.value)),
                Some(other) => {
                    return raise_type_error(&format!("deque() got an unexpected keyword argument '{other}'"));
                }
                None => return raise_type_error("deque() keywords must be strings"),
            }
        }
    }
    let maxlen = match maxlen_obj {
        None => None,
        Some(value) if value == none() => None,
        Some(value) => match int_of(value) {
            Some(maxlen) if maxlen >= 0 => Some(maxlen as usize),
            Some(_) => {
                return abi::exc::raise_kind_error_text(ExceptionKind::ValueError, "maxlen must be non-negative");
            }
            None => return raise_type_error("an integer is required"),
        },
    };
    let entries = match iterable {
        None => VecDeque::new(),
        Some(value) if value == none() => VecDeque::new(),
        Some(value) => match collect_iterable(value) {
            Ok(entries) => entries,
            Err(()) => return ptr::null_mut(),
        },
    };
    let object = alloc_deque(entries, maxlen);
    if let Some(deque) = unsafe { as_deque(object) } {
        trim_to_maxlen(deque, true);
    }
    object
}

fn int_of(object: *mut PyObject) -> Option<i64> {
    if crate::tag::is_small_int(object) {
        return Some(crate::tag::untag_small_int(object));
    }
    if object.is_null() {
        return None;
    }
    // SAFETY: Heap pointer with a live header; layout proved by the name check.
    (unsafe { crate::types::dict::type_name(object) } == Some("int"))
        .then(|| unsafe { (*object.cast::<crate::object::PyLong>()).value })
}

unsafe extern "C" fn deque_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { unicode_text(untag(name)) }) else {
        return fail("attribute name must be str");
    };
    let Some(deque) = (unsafe { as_deque(object) }) else {
        return fail("deque receiver is invalid");
    };
    match name_text {
        "maxlen" => match deque.maxlen {
            // SAFETY: Runtime allocation helper.
            Some(maxlen) => unsafe { abi::pon_const_int(maxlen as i64) },
            None => none(),
        },
        "append" => bound_method(object, name_text, deque_append_method),
        "appendleft" => bound_method(object, name_text, deque_appendleft_method),
        "extend" => bound_method(object, name_text, deque_extend_method),
        "extendleft" => bound_method(object, name_text, deque_extendleft_method),
        "pop" => bound_method(object, name_text, deque_pop_method),
        "popleft" => bound_method(object, name_text, deque_popleft_method),
        "clear" => bound_method(object, name_text, deque_clear_method),
        "copy" => bound_method(object, name_text, deque_copy_method),
        "count" => bound_method(object, name_text, deque_count_method),
        "index" => bound_method(object, name_text, deque_index_method),
        "remove" => bound_method(object, name_text, deque_remove_method),
        "rotate" => bound_method(object, name_text, deque_rotate_method),
        // SAFETY: Raise helper with the interned attribute name.
        _ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
    }
}

unsafe extern "C" fn deque_repr(object: *mut PyObject) -> *mut PyObject {
    let Some(deque) = (unsafe { as_deque(object) }) else {
        return fail("deque receiver is invalid");
    };
    let mut out = String::from("deque([");
    for (index, &entry) in deque.entries.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&super::builtins_mod::repr_text(entry));
    }
    out.push_str("])");
    if let Some(maxlen) = deque.maxlen {
        out.truncate(out.len() - 1);
        out.push_str(&format!(", maxlen={maxlen})"));
    }
    alloc_str_object(&out)
}

unsafe extern "C" fn deque_bool(object: *mut PyObject) -> c_int {
    match unsafe { as_deque(object) } {
        Some(deque) => c_int::from(!deque.entries.is_empty()),
        None => -1,
    }
}

unsafe extern "C" fn deque_len_slot(object: *mut PyObject) -> isize {
    match unsafe { as_deque(object) } {
        Some(deque) => deque.entries.len() as isize,
        None => -1,
    }
}

unsafe extern "C" fn deque_iter(object: *mut PyObject) -> *mut PyObject {
    if unsafe { as_deque(object) }.is_none() {
        return fail("deque receiver is invalid");
    }
    let iter = Box::into_raw(Box::new(PyDequeIter {
        ob_base: PyObjectHeader::new(*DEQUE_ITER_TYPE as *mut PyType),
        deque: untag(object).cast::<PyDeque>(),
        index: 0,
    }));
    iter.cast::<PyObject>()
}

unsafe extern "C" fn identity_iter(object: *mut PyObject) -> *mut PyObject {
    object
}

unsafe extern "C" fn deque_iter_next(object: *mut PyObject) -> *mut PyObject {
    let object = untag(object);
    if object.is_null() {
        return fail("deque iterator receiver is NULL");
    }
    // SAFETY: Receiver is a live PyDequeIter allocated by `deque_iter`.
    let iter = unsafe { &mut *object.cast::<PyDequeIter>() };
    // SAFETY: The referenced deque is an immortal leaked box.
    let deque = unsafe { &*iter.deque };
    match deque.entries.get(iter.index) {
        Some(&entry) => {
            iter.index += 1;
            entry
        }
        // Typed StopIteration: consumers (`for`, `list`, the `in` fallback in
        // `pon_contains`) distinguish exhaustion from failure by the pending
        // exception's type.
        None => unsafe { abi::exc::pon_raise_stop_iteration(ptr::null_mut()) },
    }
}

// ---------------------------------------------------------------------------
// deque methods

/// Shared receiver/argument prologue for single-value deque methods.
unsafe fn deque_receiver_and_args<'a>(
    argv: *mut *mut PyObject,
    argc: usize,
    method: &str,
) -> Result<(&'a mut PyDeque, &'a [*mut PyObject]), *mut PyObject> {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return Err(fail(format!("deque.{method} received a NULL argv pointer")));
    };
    let Some((&receiver, rest)) = args.split_first() else {
        return Err(fail(format!("deque.{method} requires a receiver")));
    };
    let Some(deque) = (unsafe { as_deque(receiver) }) else {
        return Err(fail(format!("deque.{method} receiver is invalid")));
    };
    Ok((deque, rest))
}

unsafe extern "C" fn deque_append_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (deque, args) = match unsafe { deque_receiver_and_args(argv, argc, "append") } {
        Ok(pair) => pair,
        Err(raised) => return raised,
    };
    let &[value] = args else {
        return raise_type_error("append() takes exactly one argument");
    };
    deque.entries.push_back(untag(value));
    trim_to_maxlen(deque, true);
    none()
}

unsafe extern "C" fn deque_appendleft_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (deque, args) = match unsafe { deque_receiver_and_args(argv, argc, "appendleft") } {
        Ok(pair) => pair,
        Err(raised) => return raised,
    };
    let &[value] = args else {
        return raise_type_error("appendleft() takes exactly one argument");
    };
    deque.entries.push_front(untag(value));
    trim_to_maxlen(deque, false);
    none()
}

unsafe extern "C" fn deque_extend_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (deque, args) = match unsafe { deque_receiver_and_args(argv, argc, "extend") } {
        Ok(pair) => pair,
        Err(raised) => return raised,
    };
    let &[iterable] = args else {
        return raise_type_error("extend() takes exactly one argument");
    };
    let items = match collect_iterable(untag(iterable)) {
        Ok(items) => items,
        Err(()) => return ptr::null_mut(),
    };
    deque.entries.extend(items);
    trim_to_maxlen(deque, true);
    none()
}

unsafe extern "C" fn deque_extendleft_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (deque, args) = match unsafe { deque_receiver_and_args(argv, argc, "extendleft") } {
        Ok(pair) => pair,
        Err(raised) => return raised,
    };
    let &[iterable] = args else {
        return raise_type_error("extendleft() takes exactly one argument");
    };
    let items = match collect_iterable(untag(iterable)) {
        Ok(items) => items,
        Err(()) => return ptr::null_mut(),
    };
    for item in items {
        deque.entries.push_front(item);
    }
    trim_to_maxlen(deque, false);
    none()
}

unsafe extern "C" fn deque_pop_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (deque, args) = match unsafe { deque_receiver_and_args(argv, argc, "pop") } {
        Ok(pair) => pair,
        Err(raised) => return raised,
    };
    if !args.is_empty() {
        return raise_type_error("pop() takes no arguments");
    }
    match deque.entries.pop_back() {
        Some(value) => value,
        None => raise_index_error("pop from an empty deque"),
    }
}

unsafe extern "C" fn deque_popleft_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (deque, args) = match unsafe { deque_receiver_and_args(argv, argc, "popleft") } {
        Ok(pair) => pair,
        Err(raised) => return raised,
    };
    if !args.is_empty() {
        return raise_type_error("popleft() takes no arguments");
    }
    match deque.entries.pop_front() {
        Some(value) => value,
        None => raise_index_error("pop from an empty deque"),
    }
}

unsafe extern "C" fn deque_clear_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (deque, args) = match unsafe { deque_receiver_and_args(argv, argc, "clear") } {
        Ok(pair) => pair,
        Err(raised) => return raised,
    };
    if !args.is_empty() {
        return raise_type_error("clear() takes no arguments");
    }
    deque.entries.clear();
    none()
}

unsafe extern "C" fn deque_copy_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (deque, args) = match unsafe { deque_receiver_and_args(argv, argc, "copy") } {
        Ok(pair) => pair,
        Err(raised) => return raised,
    };
    if !args.is_empty() {
        return raise_type_error("copy() takes no arguments");
    }
    alloc_deque(deque.entries.clone(), deque.maxlen)
}

unsafe extern "C" fn deque_count_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (deque, args) = match unsafe { deque_receiver_and_args(argv, argc, "count") } {
        Ok(pair) => pair,
        Err(raised) => return raised,
    };
    let &[needle] = args else {
        return raise_type_error("count() takes exactly one argument");
    };
    let mut count = 0i64;
    for &entry in &deque.entries {
        match value_equal(entry, needle) {
            Ok(true) => count += 1,
            Ok(false) => {}
            Err(()) => return ptr::null_mut(),
        }
    }
    // SAFETY: Runtime allocation helper.
    unsafe { abi::pon_const_int(count) }
}

unsafe extern "C" fn deque_remove_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (deque, args) = match unsafe { deque_receiver_and_args(argv, argc, "remove") } {
        Ok(pair) => pair,
        Err(raised) => return raised,
    };
    let &[needle] = args else {
        return raise_type_error("remove() takes exactly one argument");
    };
    for index in 0..deque.entries.len() {
        match value_equal(deque.entries[index], needle) {
            Ok(true) => {
                deque.entries.remove(index);
                return none();
            }
            Ok(false) => {}
            Err(()) => return ptr::null_mut(),
        }
    }
    abi::exc::raise_kind_error_text(ExceptionKind::ValueError, "deque.remove(x): x not in deque")
}

unsafe extern "C" fn deque_rotate_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (deque, args) = match unsafe { deque_receiver_and_args(argv, argc, "rotate") } {
        Ok(pair) => pair,
        Err(raised) => return raised,
    };
    if args.len() > 1 {
        return raise_type_error("rotate() takes at most one argument");
    }
    let steps = match args.first().copied().map(untag) {
        None => 1,
        Some(value) => match int_of(value) {
            Some(steps) => steps,
            None => return raise_type_error("an integer is required"),
        },
    };
    let len = deque.entries.len();
    if len == 0 {
        return none();
    }
    let steps = steps.rem_euclid(len as i64) as usize;
    deque.entries.rotate_right(steps);
    none()
}

/// `==` through the runtime rich comparison; identity short-circuits first.
fn value_equal(lhs: *mut PyObject, rhs: *mut PyObject) -> Result<bool, ()> {
    if lhs == rhs {
        return Ok(true);
    }
    // SAFETY: Comparison helper follows the NULL-sentinel error contract.
    let result = unsafe { crate::abstract_op::rich_compare(crate::abstract_op::RICH_EQ, lhs, rhs) };
    if result.is_null() {
        return Err(());
    }
    // SAFETY: Truthiness helper follows the error-sentinel contract.
    match unsafe { abi::pon_is_true(result) } {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(()),
    }
}

/// `tp_richcmp` for deque: element-wise `==`/`!=` against another deque;
/// everything else (ordering, foreign operands) is NotImplemented so the
/// dispatcher applies identity/reflected fallbacks like CPython.
unsafe extern "C" fn deque_richcmp_slot(left: *mut PyObject, right: *mut PyObject, op: c_int) -> *mut PyObject {
    let want_equal = match u8::try_from(op) {
        Ok(RICH_EQ) => true,
        Ok(RICH_NE) => false,
        // SAFETY: Singleton accessor.
        _ => return unsafe { abi::pon_not_implemented() },
    };
    let (lhs, rhs) = (untag(left), untag(right));
    if unsafe { as_deque(lhs).is_none() || as_deque(rhs).is_none() } {
        // SAFETY: Singleton accessor.
        return unsafe { abi::pon_not_implemented() };
    }
    let (lhs, rhs) = (lhs.cast::<PyDeque>(), rhs.cast::<PyDeque>());
    let equal = if lhs == rhs {
        true
    } else {
        // Snapshots: `value_equal` re-enters Python, which may mutate either
        // deque mid-comparison.
        // SAFETY: Both layouts were proved by `as_deque` above.
        let a = unsafe { (*lhs).entries.iter().copied().collect::<Vec<_>>() };
        let b = unsafe { (*rhs).entries.iter().copied().collect::<Vec<_>>() };
        if a.len() == b.len() {
            let mut all = true;
            for (x, y) in a.into_iter().zip(b) {
                match value_equal(x, y) {
                    Ok(true) => {}
                    Ok(false) => {
                        all = false;
                        break;
                    }
                    Err(()) => return ptr::null_mut(),
                }
            }
            all
        } else {
            false
        }
    };
    // SAFETY: Boolean constant allocator.
    unsafe { abi::pon_const_bool(c_int::from(equal == want_equal)) }
}

/// `sq_contains` for deque: linear equality scan (`pon_contains` protocol:
/// 1 found, 0 absent, -1 error with the exception pending).
unsafe extern "C" fn deque_contains_slot(object: *mut PyObject, item: *mut PyObject) -> c_int {
    let Some(deque) = (unsafe { as_deque(object) }) else {
        let _ = fail("deque contains receiver is invalid");
        return -1;
    };
    let needle = untag(item);
    // Snapshot: `value_equal` re-enters Python, which may mutate the deque.
    let entries = deque.entries.iter().copied().collect::<Vec<_>>();
    for entry in entries {
        match value_equal(entry, needle) {
            Ok(true) => return 1,
            Ok(false) => {}
            Err(()) => return -1,
        }
    }
    0
}

/// `deque.index(x[, start[, stop]])` with CPython's negative-index wrap and
/// clamping; a miss raises CPython 3.14's fixed-text ValueError.
unsafe extern "C" fn deque_index_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (deque, args) = match unsafe { deque_receiver_and_args(argv, argc, "index") } {
        Ok(pair) => pair,
        Err(raised) => return raised,
    };
    if args.is_empty() || args.len() > 3 {
        return raise_type_error("index() takes 1 to 3 arguments");
    }
    let needle = untag(args[0]);
    let len = deque.entries.len() as i64;
    let mut bounds = [0i64, len];
    for (slot, &value) in bounds.iter_mut().zip(&args[1..]) {
        let Some(mut index) = int_of(untag(value)) else {
            return raise_type_error("an integer is required");
        };
        if index < 0 {
            index += len;
        }
        *slot = index.clamp(0, len);
    }
    let [start, stop] = bounds;
    // Snapshot: `value_equal` re-enters Python, which may mutate the deque.
    let entries = deque.entries.iter().copied().collect::<Vec<_>>();
    for index in start..stop {
        match value_equal(entries[index as usize], needle) {
            // SAFETY: Runtime allocation helper.
            Ok(true) => return unsafe { abi::pon_const_int(index) },
            Ok(false) => {}
            Err(()) => return ptr::null_mut(),
        }
    }
    abi::exc::raise_kind_error_text(ExceptionKind::ValueError, "deque.index(x): x not in deque")
}

// ---------------------------------------------------------------------------
// defaultdict
//
// A real dict-layout heap class assembled through the same machinery as a
// Python-level `class defaultdict(dict)`: instances are
// `PyDictSubclassInstance` (full native dict protocol, GC-heap allocated and
// traced), `default_factory` lives in the instance attribute dict (readable
// and writable like CPython's member), and misses reach
// `defaultdict.__missing__` through the hook in `pon_dict_get_item`.

const DEFAULT_FACTORY: &str = "default_factory";

/// Reads `default_factory` straight from the instance attribute dict
/// (CPython reads the C member).  NULL means "unset", which callers treat
/// exactly like `None`.
unsafe fn defaultdict_factory(receiver: *mut PyObject) -> *mut PyObject {
    let instance = receiver.cast::<crate::types::type_::PyHeapInstance>();
    // SAFETY: The caller proved the dict-subclass layout, whose prefix is
    // `PyHeapInstance`.
    let dict = unsafe { (*instance).dict };
    if dict.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: Instance attribute dicts are live `PyClassDict` boxes.
    unsafe { (&*dict).get(intern(DEFAULT_FACTORY)) }.unwrap_or(ptr::null_mut())
}

/// Receiver prologue shared by the defaultdict methods.
unsafe fn defaultdict_receiver_and_args<'a>(
    argv: *mut *mut PyObject,
    argc: usize,
    method: &str,
) -> Result<(*mut PyObject, &'a [*mut PyObject]), *mut PyObject> {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return Err(fail(format!("defaultdict.{method} received a NULL argv pointer")));
    };
    let Some((&receiver, rest)) = args.split_first() else {
        return Err(fail(format!("defaultdict.{method} requires a receiver")));
    };
    let receiver = untag(receiver);
    if receiver.is_null() || unsafe { !crate::types::dict::is_dict_subclass_instance(receiver) } {
        return Err(raise_type_error(&format!(
            "descriptor '{method}' requires a 'collections.defaultdict' object"
        )));
    }
    Ok((receiver, rest))
}

/// `defaultdict.__init__(self, default_factory=None, mapping_or_iterable=None)`.
unsafe extern "C" fn defaultdict_init_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (receiver, args) = match unsafe { defaultdict_receiver_and_args(argv, argc, "__init__") } {
        Ok(pair) => pair,
        Err(raised) => return raised,
    };
    if args.len() > 2 {
        return raise_type_error("defaultdict expected at most 2 arguments");
    }
    let factory = match args.first().copied().map(untag) {
        None => none(),
        Some(value) if value == none() => none(),
        Some(value) => {
            // Mirrors the `callable` builtin's predicate; tagged immediates
            // are never callable and must not reach `type_name`.
            let callable = !crate::tag::is_small_int(value)
                && matches!(
                    unsafe { crate::types::dict::type_name(value) },
                    Some("function" | "method" | "type")
                );
            if !callable {
                return raise_type_error("first argument must be callable or None");
            }
            value
        }
    };
    let instance = receiver.cast::<crate::types::type_::PyHeapInstance>();
    // SAFETY: `defaultdict_receiver_and_args` proved the heap-instance prefix.
    let mut dict = unsafe { (*instance).dict };
    if dict.is_null() {
        dict = crate::types::type_::new_namespace();
        // SAFETY: Prefix write on the proved layout.
        unsafe { (*instance).dict = dict };
    }
    // SAFETY: Instance attribute dicts are live `PyClassDict` boxes.
    unsafe { (&mut *dict).set(intern(DEFAULT_FACTORY), factory) };
    if let Some(&source) = args.get(1) {
        let source = untag(source);
        let mut pairs = Vec::new();
        // SAFETY: Update-pair collection follows the error-sentinel contract.
        if unsafe { super::builtins_mod::collect_dict_update_pairs(source, &mut pairs) }.is_err() {
            return ptr::null_mut();
        }
        for pair in pairs.chunks_exact(2) {
            // SAFETY: Receiver embeds dict storage; helper self-normalizes.
            if unsafe { crate::abi::map::pon_dict_set_item_status(receiver, pair[0], pair[1]) } < 0 {
                return ptr::null_mut();
            }
        }
    }
    none()
}

/// `defaultdict.__missing__(self, key)`: no factory raises `KeyError(key)`;
/// otherwise the factory result is INSERTED under `key`, then returned.
unsafe extern "C" fn defaultdict_missing_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (receiver, args) = match unsafe { defaultdict_receiver_and_args(argv, argc, "__missing__") } {
        Ok(pair) => pair,
        Err(raised) => return raised,
    };
    let &[key] = args else {
        return raise_type_error("__missing__() takes exactly one argument");
    };
    let factory = unsafe { defaultdict_factory(receiver) };
    if factory.is_null() || factory == none() {
        // SAFETY: Raise helper self-normalizes the key.
        return unsafe { abi::exc::pon_raise_key_error(key) };
    }
    // SAFETY: Call helper follows the NULL-sentinel error contract.
    let value = unsafe { abi::pon_call(factory, ptr::null_mut(), 0) };
    if value.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: Receiver embeds dict storage; helper self-normalizes.
    if unsafe { crate::abi::map::pon_dict_set_item_status(receiver, key, value) } < 0 {
        return ptr::null_mut();
    }
    value
}

/// `repr(defaultdict)` -> `defaultdict(<factory>, {...})`.
unsafe extern "C" fn defaultdict_repr_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (receiver, _) = match unsafe { defaultdict_receiver_and_args(argv, argc, "__repr__") } {
        Ok(pair) => pair,
        Err(raised) => return raised,
    };
    let factory = unsafe { defaultdict_factory(receiver) };
    let factory_repr = if factory.is_null() {
        "None".to_owned()
    } else {
        super::builtins_mod::repr_text(factory)
    };
    // No-dispatch repr renders the embedded dict storage as `{...}` without
    // re-entering this `__repr__`.
    let storage_repr = match super::builtins_mod::repr_text_no_dispatch(receiver) {
        Ok(text) => text,
        Err(()) => return ptr::null_mut(),
    };
    alloc_str_object(&format!("defaultdict({factory_repr}, {storage_repr})"))
}

/// Builds (once) the `collections.defaultdict` heap class: base `dict`, a
/// namespace of native methods, C3 MRO, and GC rooting all through
/// `build_class_from_namespace` — exactly what a Python-level
/// `class defaultdict(dict)` would get.
fn defaultdict_type() -> Result<*mut PyObject, String> {
    static TYPE: Mutex<usize> = Mutex::new(0);
    let mut slot = TYPE.lock().unwrap_or_else(|poison| poison.into_inner());
    if *slot != 0 {
        return Ok(*slot as *mut PyObject);
    }
    let type_type = abi::runtime_type_type();
    if type_type.is_null() {
        return Err("runtime type type is not initialized".to_owned());
    }
    let dict_type = crate::types::dict::dict_type(type_type);
    let namespace = crate::types::type_::new_namespace();
    let natives: &[(&str, BuiltinFn)] = &[
        ("__init__", defaultdict_init_method),
        ("__missing__", defaultdict_missing_method),
        ("__repr__", defaultdict_repr_method),
    ];
    for &(method, entry) in natives {
        let interned = intern(method);
        // SAFETY: `entry` is a live builtin entry point with the runtime
        // calling convention.
        let function = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, interned) };
        if function.is_null() {
            return Err(format!("failed to allocate defaultdict.{method}"));
        }
        // SAFETY: Freshly built namespace box.
        unsafe { (&mut *namespace).set(interned, function) };
    }
    // SAFETY: The dict type is a live type object; the namespace was built above.
    let class = unsafe {
        crate::types::type_::build_class_from_namespace(
            "collections.defaultdict",
            &[dict_type.cast::<PyObject>()],
            namespace,
            &[],
        )
    };
    if class.is_null() {
        pon_err_clear();
        return Err("failed to construct collections.defaultdict".to_owned());
    }
    *slot = class as usize;
    Ok(class)
}

// ---------------------------------------------------------------------------

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "_collections";
    // SAFETY: Runtime allocation helper; NULL is checked below.
    let name_obj = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
    if name_obj.is_null() {
        return Err("failed to allocate _collections.__name__".to_owned());
    }
    let defaultdict = defaultdict_type()?;
    install_module(
        name,
        vec![
            (intern("__name__"), name_obj),
            (intern("defaultdict"), defaultdict),
            (intern("deque"), deque_type().cast::<PyObject>()),
        ],
    )
}
