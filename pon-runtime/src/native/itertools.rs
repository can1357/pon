//! Native `itertools` module (HANDOFF Track L, J0.4 lazy registry row).
//!
//! Lazy iterator types following the `types::lazy_iter` / `native::sre`
//! pattern: each CPython-named iterator is a boxed `#[repr(C)]` object whose
//! `PyType` carries identity `tp_iter` and a dedicated `tp_iternext`.  Every
//! iterator pulls from its source lazily through the runtime iterator
//! protocol (`pon_get_iter` / `pon_iter_next`) and terminates with a typed
//! `StopIteration`, so the vendored `collections` / `traceback` chain can
//! consume these exactly like CPython's C implementations.
//!
//! Surface: count, cycle, repeat, chain (+ chain.from_iterable), islice,
//! starmap, tee, zip_longest, product, permutations, combinations,
//! accumulate, filterfalse, takewhile, dropwhile, compress, pairwise,
//! batched, groupby (with its `_grouper`).
//!
//! Keyword arguments arrive either pre-bound to a fixed positional shape
//! (`bind_optional_named_keywords` in `types::function`) or, for the variadic
//! constructors `zip_longest` / `product`, as a trailing
//! `types::lazy_iter::PyKwMarker` carrier appended by the binder.

use std::collections::VecDeque;
use std::ptr;
use std::sync::LazyLock;

use num_traits::ToPrimitive;

use crate::abi::{self, pon_call, pon_get_iter, pon_iter_next};
use crate::abstract_op::{self, BINARY_ADD, RICH_EQ};
use crate::gcroot::{HeldRoots, RootRegistry};
use crate::intern::intern;
use crate::object::{PyObject, PyObjectHeader, PyType, UnaryFunc};
use crate::thread_state::{pon_err_clear, thread_state_lock};
use crate::types::lazy_iter;

use super::builtins_mod::VARIADIC_ARITY;
use super::install_module;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

// ---------------------------------------------------------------------------
// Module factory

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name_value = "itertools";
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let name_object = unsafe { abi::pon_const_str(name_value.as_ptr(), name_value.len()) };
    if name_object.is_null() {
        return Err("failed to allocate itertools.__name__".to_owned());
    }
    let mut attrs = vec![(intern("__name__"), name_object)];
    let functions: [(&str, BuiltinFn); 19] = [
        ("accumulate", itertools_accumulate),
        ("batched", itertools_batched),
        ("combinations", itertools_combinations),
        ("combinations_with_replacement", itertools_combinations_with_replacement),
        ("compress", itertools_compress),
        ("count", itertools_count),
        ("cycle", itertools_cycle),
        ("dropwhile", itertools_dropwhile),
        ("filterfalse", itertools_filterfalse),
        ("groupby", itertools_groupby),
        ("islice", itertools_islice),
        ("pairwise", itertools_pairwise),
        ("permutations", itertools_permutations),
        ("product", itertools_product),
        ("repeat", itertools_repeat),
        ("starmap", itertools_starmap),
        ("takewhile", itertools_takewhile),
        ("tee", itertools_tee),
        ("zip_longest", itertools_zip_longest),
    ];
    for (name, entry) in functions {
        attrs.push(function_attr(name, entry)?);
    }
    attrs.push((intern("_grouper"), (*GROUPER_TYPE as *mut PyType).cast::<PyObject>()));
    attrs.push((intern("_tee"), (*TEE_TYPE as *mut PyType).cast::<PyObject>()));
    attrs.push((intern("_tee_dataobject"), (*TEE_DATAOBJECT_TYPE as *mut PyType).cast::<PyObject>()));
    attrs.push((intern("chain"), make_chain_callable()?));
    install_module("itertools", attrs)
}

fn function_attr(name: &str, entry: BuiltinFn) -> Result<(u32, *mut PyObject), String> {
    Ok((intern(name), make_function_object(name, entry)?))
}

fn make_function_object(name: &str, entry: BuiltinFn) -> Result<*mut PyObject, String> {
    // SAFETY: `entry` is a live builtin entry point with the runtime calling convention.
    let object = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    (!object.is_null())
        .then_some(object)
        .ok_or_else(|| format!("failed to allocate itertools.{name}"))
}

/// Builds the `chain` callable with its `from_iterable` attribute attached,
/// mirroring CPython's `chain.from_iterable` classmethod surface.
fn make_chain_callable() -> Result<*mut PyObject, String> {
    let chain_object = make_function_object("chain", itertools_chain)?;
    let from_iterable = make_function_object("from_iterable", chain_from_iterable)?;
    let attr = "from_iterable";
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let attr_name = unsafe { abi::pon_const_str(attr.as_ptr(), attr.len()) };
    if attr_name.is_null() {
        return Err("failed to allocate chain.from_iterable name".to_owned());
    }
    // SAFETY: `chain_object` is a live function object; setattr stores into its attr dict.
    let status = unsafe { crate::types::function::function_setattro(chain_object, attr_name, from_iterable) };
    if status != 0 {
        return Err("failed to attach chain.from_iterable".to_owned());
    }
    Ok(chain_object)
}

// ---------------------------------------------------------------------------
// Shared small helpers

fn untag(object: *mut PyObject) -> *mut PyObject {
    crate::tag::untag_arg(object)
}

fn none() -> *mut PyObject {
    // SAFETY: `pon_none` allocates/returns the shared None singleton.
    unsafe { abi::pon_none() }
}

unsafe fn is_none(object: *mut PyObject) -> bool {
    let ty = unsafe { object.as_ref().and_then(|object| object.ob_type.as_ref()) };
    ty.is_some_and(|ty| ty.name() == "NoneType")
}

/// `true` for absent-or-None optional arguments (binder fills gaps with None).
unsafe fn is_absent(args: &[*mut PyObject], index: usize) -> bool {
    match args.get(index) {
        Some(&value) => value.is_null() || unsafe { is_none(value) },
        None => true,
    }
}

fn raise_stop_iteration() -> *mut PyObject {
    // SAFETY: NULL value produces a plain StopIteration.
    unsafe { abi::pon_raise_stop_iteration(ptr::null_mut()) }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    // SAFETY: Message bytes are a live UTF-8 slice for the duration of the call.
    unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn raise_value_error(message: &str) -> *mut PyObject {
    // SAFETY: Message bytes are a live UTF-8 slice for the duration of the call.
    unsafe { abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) }
}

unsafe fn current_exception_is(name: &str) -> bool {
    let current = thread_state_lock().current_exc;
    if current.is_null() || current == core::ptr::NonNull::<PyObject>::dangling().as_ptr() {
        return false;
    }
    let ty = unsafe { (*current).ob_type };
    !ty.is_null() && unsafe { (*ty).name() == name }
}

/// Collects the raw (untagged) argument window of a builtin call.
unsafe fn arg_vec(argv: *mut *mut PyObject, argc: usize) -> Option<Vec<*mut PyObject>> {
    if argv.is_null() {
        return (argc == 0).then(Vec::new);
    }
    // SAFETY: The caller passes a live argv window of length argc.
    let raw = unsafe { core::slice::from_raw_parts(argv, argc) };
    Some(raw.iter().copied().map(untag).collect())
}

/// Splits a trailing keyword-carrier marker appended by the native binder.
fn split_kw_marker(args: &[*mut PyObject]) -> (&[*mut PyObject], Vec<(String, *mut PyObject)>) {
    if let Some((&last, rest)) = args.split_last() {
        // SAFETY: `kw_marker_pairs` type-checks before downcasting.
        if let Some(pairs) = unsafe { lazy_iter::kw_marker_pairs(last) } {
            let named = pairs
                .iter()
                .map(|&(name, value)| (crate::intern::resolve(name).unwrap_or_default(), untag(value)))
                .collect();
            return (rest, named);
        }
    }
    (args, Vec::new())
}

fn to_i64(object: *mut PyObject) -> Option<i64> {
    if object.is_null() {
        return None;
    }
    // SAFETY: `object` is heap-or-NULL after untagging and NULL was rejected.
    unsafe { crate::types::int::to_bigint_including_bool(object) }.and_then(|value| value.to_i64())
}

#[derive(Clone, Copy, Debug)]
enum NextItem {
    Value(*mut PyObject),
    Stop,
    Error,
}

/// Pulls one item, normalizing tagged immediates so callers may dereference.
unsafe fn next_item(iter: *mut PyObject) -> NextItem {
    // SAFETY: `pon_iter_next` self-normalizes its argument.
    let value = unsafe { pon_iter_next(iter, ptr::null_mut()) };
    if !value.is_null() {
        return NextItem::Value(untag(value));
    }
    if unsafe { current_exception_is("StopIteration") } {
        pon_err_clear();
        NextItem::Stop
    } else {
        NextItem::Error
    }
}

/// `iter(object)` with a CPython-shaped TypeError naming the caller.
unsafe fn get_iter_checked(object: *mut PyObject, function_name: &str) -> *mut PyObject {
    // SAFETY: `pon_get_iter` self-normalizes its argument.
    let iter = unsafe { pon_get_iter(object, ptr::null_mut()) };
    if iter.is_null() {
        pon_err_clear();
        return raise_type_error(&format!("{function_name} argument must support iteration"));
    }
    untag(iter)
}

/// Materializes an iterable, propagating genuine errors (unlike exhaustion).
unsafe fn collect_iterable(object: *mut PyObject, function_name: &str) -> Result<Vec<*mut PyObject>, ()> {
    let iter = unsafe { get_iter_checked(object, function_name) };
    if iter.is_null() {
        return Err(());
    }
    let mut items = Vec::new();
    loop {
        match unsafe { next_item(iter) } {
            NextItem::Value(value) => items.push(value),
            NextItem::Stop => return Ok(items),
            NextItem::Error => return Err(()),
        }
    }
}

/// Calls `function(args...)`, returning an untagged heap-or-NULL result.
unsafe fn call_function(function: *mut PyObject, args: &mut [*mut PyObject]) -> *mut PyObject {
    let argv = if args.is_empty() { ptr::null_mut() } else { args.as_mut_ptr() };
    // SAFETY: `pon_call` self-normalizes callee and dispatches by target kind.
    untag(unsafe { pon_call(function, argv, args.len()) })
}

/// `bool(object)` for predicate results: `Ok(bool)` or `Err` with error set.
unsafe fn truth(object: *mut PyObject) -> Result<bool, ()> {
    // SAFETY: `pon_is_true` self-normalizes its argument.
    match unsafe { abi::object::pon_is_true(object) } {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(()),
    }
}

/// `a == b` with the CPython identity shortcut; `Err` when comparison raised.
unsafe fn objects_equal(a: *mut PyObject, b: *mut PyObject) -> Result<bool, ()> {
    if a == b {
        return Ok(true);
    }
    // SAFETY: Both operands are heap-or-NULL; rich_compare rejects NULL itself.
    let verdict = unsafe { abstract_op::rich_compare(RICH_EQ, a, b) };
    if verdict.is_null() {
        return Err(());
    }
    unsafe { truth(untag(verdict)) }
}

fn tuple_from(mut items: Vec<*mut PyObject>) -> *mut PyObject {
    let argv = if items.is_empty() { ptr::null_mut() } else { items.as_mut_ptr() };
    // SAFETY: `argv` is a live window for the duration of the call; the
    // result is a real `PyTuple` (subscriptable, unpackable), unlike the
    // builtins-module NativeObject sequences.
    unsafe { abi::seq::pon_build_tuple(argv, items.len()) }
}

unsafe extern "C" fn identity_iter(object: *mut PyObject) -> *mut PyObject {
    object
}

fn iterator_type(name: &'static str, size: usize, next: UnaryFunc) -> usize {
    let mut ty = PyType::new(abi::runtime_type_type().cast_const(), name, size);
    ty.tp_iter = Some(identity_iter);
    ty.tp_iternext = Some(next);
    ty.tp_getattro = Some(iterator_getattro);
    Box::into_raw(Box::new(ty)) as usize
}

/// `tp_getattro` shared by the itertools iterator types: `threading` binds
/// `_count(1).__next__` as its name-counter factory at import, so the
/// iterator slots must be reachable as bound methods; every other name
/// raises AttributeError.
unsafe extern "C" fn iterator_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let name = untag(name);
    let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        return raise_type_error("attribute name must be str");
    };
    let entry: BuiltinFn = match name_text {
        "__next__" => iterator_next_method,
        "__iter__" => iterator_iter_method,
        // SAFETY: Raise helper with the interned attribute name.
        _ => return unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
    };
    // SAFETY: `entry` is a live builtin entry point with the runtime calling convention.
    let function = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name_text)) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match crate::types::method::new_bound_method(function, object) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => {
            crate::thread_state::pon_err_set(message);
            ptr::null_mut()
        }
    }
}

/// Bound `iterator.__next__()`: forwards to the runtime iterator protocol
/// (pon iterator slots raise their own typed `StopIteration`).
unsafe extern "C" fn iterator_next_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 || argv.is_null() {
        return raise_type_error(&format!("__next__() takes no arguments ({} given)", argc.saturating_sub(1)));
    }
    // SAFETY: The call helper supplies `argv` with at least one entry.
    unsafe { pon_iter_next(*argv, ptr::null_mut()) }
}

/// Bound `iterator.__iter__()`: identity, mirroring the `tp_iter` slot.
unsafe extern "C" fn iterator_iter_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 || argv.is_null() {
        return raise_type_error(&format!("__iter__() takes no arguments ({} given)", argc.saturating_sub(1)));
    }
    // SAFETY: The call helper supplies `argv` with at least one entry.
    unsafe { *argv }
}

/// Every itertools iterator allocation, for GC root reporting: the leaked
/// boxes hold source iterators, callables, and saved values that live on the
/// GC heap and are invisible to marking (`crate::gcroot`).  Objects are
/// immortal, so the registry only grows.
static REGISTRY: RootRegistry = RootRegistry::new();

/// References held by live itertools iterators.  Consumed by
/// `crate::abi::collect` while the runtime lock is held.
pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
    REGISTRY.held_roots()
}

fn alloc_object<T: HeldRoots>(value: T) -> *mut PyObject {
    REGISTRY.register::<T>(Box::into_raw(Box::new(value)).cast::<PyObject>())
}

// GC-held references per iterator layout.  Exhausted iterators null their
// source slots, so reporting raw fields naturally stops pinning consumed
// inputs; Vec-held items (cycle saves, zip_longest sources, pools) stay
// pinned for the object's lifetime because `tp_iternext` re-reads them.

impl HeldRoots for PyItertoolsCount {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        push(self.current);
        push(self.step);
    }
}

impl HeldRoots for PyItertoolsCycle {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        push(self.source);
        for &saved in &self.saved {
            push(saved);
        }
    }
}

impl HeldRoots for PyItertoolsRepeat {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        push(self.object);
    }
}

impl HeldRoots for PyItertoolsChain {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        push(self.outer);
        push(self.inner);
    }
}

impl HeldRoots for PyItertoolsISlice {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        push(self.source);
    }
}

impl HeldRoots for PyItertoolsStarmap {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        push(self.function);
        push(self.source);
    }
}

impl HeldRoots for PyItertoolsZipLongest {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        for &source in &self.sources {
            push(source);
        }
        push(self.fillvalue);
    }
}

impl HeldRoots for PyItertoolsProduct {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        for pool in &self.pools {
            for &item in pool {
                push(item);
            }
        }
    }
}

impl HeldRoots for PyItertoolsPermutations {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        for &item in &self.pool {
            push(item);
        }
    }
}

impl HeldRoots for PyItertoolsCombinations {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        for &item in &self.pool {
            push(item);
        }
    }
}

impl HeldRoots for PyItertoolsCombinationsWithReplacement {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        for &item in &self.pool {
            push(item);
        }
    }
}

impl HeldRoots for PyItertoolsAccumulate {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        push(self.source);
        push(self.function);
        push(self.total);
        push(self.initial);
    }
}

impl HeldRoots for PyItertoolsFilterFalse {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        push(self.predicate);
        push(self.source);
    }
}

impl HeldRoots for PyItertoolsTakewhile {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        push(self.predicate);
        push(self.source);
    }
}

impl HeldRoots for PyItertoolsDropwhile {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        push(self.predicate);
        push(self.source);
    }
}

impl HeldRoots for PyItertoolsCompress {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        push(self.data);
        push(self.selectors);
    }
}

impl HeldRoots for PyItertoolsPairwise {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        push(self.source);
        push(self.previous);
    }
}

impl HeldRoots for PyItertoolsBatched {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        push(self.source);
    }
}

impl HeldRoots for PyItertoolsGroupBy {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        push(self.source);
        push(self.keyfunc);
        push(self.currkey);
        push(self.currvalue);
        push(self.tgtkey);
        // `currgrouper` is a leaked `_grouper` box: reported harmlessly.
        push(self.currgrouper);
    }
}

impl HeldRoots for PyItertoolsGrouper {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        // `parent` is a leaked groupby box: reported harmlessly.
        push(self.parent);
        push(self.tgtkey);
    }
}
impl HeldRoots for PyItertoolsTee {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        // SAFETY: `shared` is a leaked box owned jointly by the sibling
        // clones, which are themselves immortal registry entries.
        let shared = unsafe { &*self.shared };
        push(shared.source);
        for &item in &shared.buffer {
            push(item);
        }
    }
}


// ---------------------------------------------------------------------------
// count(start=0, step=1)

#[repr(C)]
struct PyItertoolsCount {
    ob_base: PyObjectHeader,
    current: *mut PyObject,
    step: *mut PyObject,
}

static COUNT_TYPE: LazyLock<usize> =
    LazyLock::new(|| iterator_type("count", size_of::<PyItertoolsCount>(), count_next));

unsafe extern "C" fn itertools_count(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("count received an invalid argument window");
    };
    if args.len() > 2 {
        return raise_type_error("count() takes at most 2 arguments");
    }
    let current = if unsafe { is_absent(&args, 0) } {
        crate::types::int::from_i64(0)
    } else {
        args[0]
    };
    let step = if unsafe { is_absent(&args, 1) } {
        crate::types::int::from_i64(1)
    } else {
        args[1]
    };
    if current.is_null() || step.is_null() {
        return raise_type_error("count() failed to allocate defaults");
    }
    alloc_object(PyItertoolsCount {
        ob_base: PyObjectHeader::new(*COUNT_TYPE as *const PyType),
        current,
        step,
    })
}

unsafe extern "C" fn count_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsCount>() };
    let result = state.current;
    // SAFETY: Both operands are live heap objects held by this iterator.
    let advanced = untag(unsafe { abstract_op::binary_op(BINARY_ADD, state.current, state.step) });
    if advanced.is_null() {
        return ptr::null_mut();
    }
    state.current = advanced;
    result
}

// ---------------------------------------------------------------------------
// cycle(iterable)

#[repr(C)]
struct PyItertoolsCycle {
    ob_base: PyObjectHeader,
    source: *mut PyObject,
    saved: Vec<*mut PyObject>,
    index: usize,
}

static CYCLE_TYPE: LazyLock<usize> =
    LazyLock::new(|| iterator_type("cycle", size_of::<PyItertoolsCycle>(), cycle_next));

unsafe extern "C" fn itertools_cycle(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("cycle received an invalid argument window");
    };
    if args.len() != 1 {
        return raise_type_error(&format!("cycle expected 1 argument, got {}", args.len()));
    }
    let source = unsafe { get_iter_checked(args[0], "cycle") };
    if source.is_null() {
        return ptr::null_mut();
    }
    alloc_object(PyItertoolsCycle {
        ob_base: PyObjectHeader::new(*CYCLE_TYPE as *const PyType),
        source,
        saved: Vec::new(),
        index: 0,
    })
}

unsafe extern "C" fn cycle_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsCycle>() };
    if !state.source.is_null() {
        match unsafe { next_item(state.source) } {
            NextItem::Value(value) => {
                state.saved.push(value);
                return value;
            }
            NextItem::Error => return ptr::null_mut(),
            NextItem::Stop => state.source = ptr::null_mut(),
        }
    }
    if state.saved.is_empty() {
        return raise_stop_iteration();
    }
    let value = state.saved[state.index];
    state.index = (state.index + 1) % state.saved.len();
    value
}

// ---------------------------------------------------------------------------
// repeat(object[, times])

#[repr(C)]
struct PyItertoolsRepeat {
    ob_base: PyObjectHeader,
    object: *mut PyObject,
    /// Remaining yields; negative means infinite.
    remaining: i64,
}

static REPEAT_TYPE: LazyLock<usize> =
    LazyLock::new(|| iterator_type("repeat", size_of::<PyItertoolsRepeat>(), repeat_next));

unsafe extern "C" fn itertools_repeat(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("repeat received an invalid argument window");
    };
    if args.is_empty() || args.len() > 2 {
        return raise_type_error(&format!("repeat expected 1 or 2 arguments, got {}", args.len()));
    }
    let remaining = if unsafe { is_absent(&args, 1) } {
        -1
    } else {
        let Some(times) = to_i64(args[1]) else {
            return raise_type_error("'times' argument for repeat() must be an integer");
        };
        times.max(0)
    };
    alloc_object(PyItertoolsRepeat {
        ob_base: PyObjectHeader::new(*REPEAT_TYPE as *const PyType),
        object: args[0],
        remaining,
    })
}

unsafe extern "C" fn repeat_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsRepeat>() };
    if state.remaining == 0 {
        return raise_stop_iteration();
    }
    if state.remaining > 0 {
        state.remaining -= 1;
    }
    state.object
}

// ---------------------------------------------------------------------------
// chain(*iterables) and chain.from_iterable(iterable)

#[repr(C)]
struct PyItertoolsChain {
    ob_base: PyObjectHeader,
    /// Iterator over the iterables themselves; NULL once exhausted.
    outer: *mut PyObject,
    /// Current inner iterator; NULL when the next iterable must be opened.
    inner: *mut PyObject,
}

static CHAIN_TYPE: LazyLock<usize> =
    LazyLock::new(|| iterator_type("chain", size_of::<PyItertoolsChain>(), chain_next));

fn alloc_chain(outer: *mut PyObject) -> *mut PyObject {
    alloc_object(PyItertoolsChain {
        ob_base: PyObjectHeader::new(*CHAIN_TYPE as *const PyType),
        outer,
        inner: ptr::null_mut(),
    })
}

unsafe extern "C" fn itertools_chain(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("chain received an invalid argument window");
    };
    let mut args = args;
    let holder_argv = if args.is_empty() { ptr::null_mut() } else { args.as_mut_ptr() };
    // SAFETY: `holder_argv` is a live window for the duration of the call.
    let holder = unsafe { abi::seq::pon_build_list(holder_argv, args.len()) };
    if holder.is_null() {
        return ptr::null_mut();
    }
    let outer = unsafe { get_iter_checked(holder, "chain") };
    if outer.is_null() {
        return ptr::null_mut();
    }
    alloc_chain(outer)
}

unsafe extern "C" fn chain_from_iterable(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("chain.from_iterable received an invalid argument window");
    };
    if args.len() != 1 {
        return raise_type_error(&format!("from_iterable expected 1 argument, got {}", args.len()));
    }
    let outer = unsafe { get_iter_checked(args[0], "chain.from_iterable") };
    if outer.is_null() {
        return ptr::null_mut();
    }
    alloc_chain(outer)
}

unsafe extern "C" fn chain_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsChain>() };
    loop {
        if state.inner.is_null() {
            if state.outer.is_null() {
                return raise_stop_iteration();
            }
            match unsafe { next_item(state.outer) } {
                NextItem::Value(iterable) => {
                    let inner = unsafe { get_iter_checked(iterable, "chain") };
                    if inner.is_null() {
                        return ptr::null_mut();
                    }
                    state.inner = inner;
                }
                NextItem::Stop => {
                    state.outer = ptr::null_mut();
                    return raise_stop_iteration();
                }
                NextItem::Error => return ptr::null_mut(),
            }
        }
        match unsafe { next_item(state.inner) } {
            NextItem::Value(value) => return value,
            NextItem::Stop => state.inner = ptr::null_mut(),
            NextItem::Error => return ptr::null_mut(),
        }
    }
}

// ---------------------------------------------------------------------------
// islice(iterable, stop) / islice(iterable, start, stop[, step])

#[repr(C)]
struct PyItertoolsISlice {
    ob_base: PyObjectHeader,
    /// Source iterator; NULL once the slice is exhausted.
    source: *mut PyObject,
    /// Next absolute source index to yield.
    next: i64,
    /// Exclusive stop index, or -1 for unbounded.
    stop: i64,
    step: i64,
    /// Count of items consumed from the source so far.
    cnt: i64,
}

static ISLICE_TYPE: LazyLock<usize> =
    LazyLock::new(|| iterator_type("islice", size_of::<PyItertoolsISlice>(), islice_next));

const ISLICE_INDEX_MESSAGE: &str =
    "Indices for islice() must be None or an integer: 0 <= x <= sys.maxsize.";
const ISLICE_STOP_MESSAGE: &str =
    "Stop argument for islice() must be None or an integer: 0 <= x <= sys.maxsize.";
const ISLICE_STEP_MESSAGE: &str = "Step for islice() must be a positive integer or None.";

fn islice_index(value: *mut PyObject, absent_default: i64) -> Result<i64, ()> {
    if value.is_null() || unsafe { is_none(value) } {
        return Ok(absent_default);
    }
    match to_i64(value) {
        Some(index) if index >= 0 => Ok(index),
        _ => Err(()),
    }
}

unsafe extern "C" fn itertools_islice(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("islice received an invalid argument window");
    };
    if args.len() < 2 || args.len() > 4 {
        return raise_type_error(&format!("islice expected 2 to 4 arguments, got {}", args.len()));
    }
    let (start, stop) = if args.len() == 2 {
        let Ok(stop) = islice_index(args[1], -1) else {
            return raise_value_error(ISLICE_STOP_MESSAGE);
        };
        (0, stop)
    } else {
        let Ok(start) = islice_index(args[1], 0) else {
            return raise_value_error(ISLICE_INDEX_MESSAGE);
        };
        let Ok(stop) = islice_index(args[2], -1) else {
            return raise_value_error(ISLICE_INDEX_MESSAGE);
        };
        (start, stop)
    };
    let step = if args.len() == 4 {
        match islice_index(args[3], 1) {
            Ok(step) if step >= 1 => step,
            _ => return raise_value_error(ISLICE_STEP_MESSAGE),
        }
    } else {
        1
    };
    let source = unsafe { get_iter_checked(args[0], "islice") };
    if source.is_null() {
        return ptr::null_mut();
    }
    alloc_object(PyItertoolsISlice {
        ob_base: PyObjectHeader::new(*ISLICE_TYPE as *const PyType),
        source,
        next: start,
        stop,
        step,
        cnt: 0,
    })
}

unsafe extern "C" fn islice_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsISlice>() };
    if state.source.is_null() {
        return raise_stop_iteration();
    }
    while state.cnt < state.next {
        match unsafe { next_item(state.source) } {
            NextItem::Value(_) => state.cnt += 1,
            NextItem::Stop => {
                state.source = ptr::null_mut();
                return raise_stop_iteration();
            }
            NextItem::Error => return ptr::null_mut(),
        }
    }
    if state.stop != -1 && state.cnt >= state.stop {
        state.source = ptr::null_mut();
        return raise_stop_iteration();
    }
    let value = match unsafe { next_item(state.source) } {
        NextItem::Value(value) => value,
        NextItem::Stop => {
            state.source = ptr::null_mut();
            return raise_stop_iteration();
        }
        NextItem::Error => return ptr::null_mut(),
    };
    state.cnt += 1;
    let previous = state.next;
    state.next = state.next.saturating_add(state.step);
    if state.next < previous || (state.stop != -1 && state.next > state.stop) {
        state.next = state.stop;
    }
    value
}

// ---------------------------------------------------------------------------
// starmap(function, iterable)

#[repr(C)]
struct PyItertoolsStarmap {
    ob_base: PyObjectHeader,
    function: *mut PyObject,
    source: *mut PyObject,
}

static STARMAP_TYPE: LazyLock<usize> =
    LazyLock::new(|| iterator_type("starmap", size_of::<PyItertoolsStarmap>(), starmap_next));

unsafe extern "C" fn itertools_starmap(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("starmap received an invalid argument window");
    };
    if args.len() != 2 {
        return raise_type_error(&format!("starmap expected 2 arguments, got {}", args.len()));
    }
    let source = unsafe { get_iter_checked(args[1], "starmap") };
    if source.is_null() {
        return ptr::null_mut();
    }
    alloc_object(PyItertoolsStarmap {
        ob_base: PyObjectHeader::new(*STARMAP_TYPE as *const PyType),
        function: args[0],
        source,
    })
}

unsafe extern "C" fn starmap_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsStarmap>() };
    let bundle = match unsafe { next_item(state.source) } {
        NextItem::Value(value) => value,
        NextItem::Stop => return raise_stop_iteration(),
        NextItem::Error => return ptr::null_mut(),
    };
    let Ok(mut call_args) = (unsafe { collect_iterable(bundle, "starmap") }) else {
        return ptr::null_mut();
    };
    let result = unsafe { call_function(state.function, &mut call_args) };
    if result.is_null() {
        return ptr::null_mut();
    }
    result
}

// ---------------------------------------------------------------------------
// zip_longest(*iterables, fillvalue=None)

#[repr(C)]
struct PyItertoolsZipLongest {
    ob_base: PyObjectHeader,
    /// Source iterators; a NULL slot marks an exhausted source.
    sources: Vec<*mut PyObject>,
    active: usize,
    fillvalue: *mut PyObject,
}

static ZIP_LONGEST_TYPE: LazyLock<usize> =
    LazyLock::new(|| iterator_type("zip_longest", size_of::<PyItertoolsZipLongest>(), zip_longest_next));

unsafe extern "C" fn itertools_zip_longest(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("zip_longest received an invalid argument window");
    };
    let (positional, keywords) = split_kw_marker(&args);
    let mut fillvalue = none();
    for (name, value) in keywords {
        match name.as_str() {
            "fillvalue" => fillvalue = value,
            other => {
                return raise_type_error(&format!(
                    "zip_longest() got an unexpected keyword argument '{other}'"
                ))
            }
        }
    }
    if fillvalue.is_null() {
        return ptr::null_mut();
    }
    let mut sources = Vec::with_capacity(positional.len());
    for (index, &iterable) in positional.iter().enumerate() {
        let iter = unsafe { get_iter_checked(iterable, "zip_longest") };
        if iter.is_null() {
            pon_err_clear();
            return raise_type_error(&format!(
                "zip_longest argument #{} must support iteration",
                index + 1
            ));
        }
        sources.push(iter);
    }
    let active = sources.len();
    alloc_object(PyItertoolsZipLongest {
        ob_base: PyObjectHeader::new(*ZIP_LONGEST_TYPE as *const PyType),
        sources,
        active,
        fillvalue,
    })
}

unsafe extern "C" fn zip_longest_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsZipLongest>() };
    if state.sources.is_empty() || state.active == 0 {
        return raise_stop_iteration();
    }
    let mut items = Vec::with_capacity(state.sources.len());
    for slot in 0..state.sources.len() {
        let source = state.sources[slot];
        if source.is_null() {
            items.push(state.fillvalue);
            continue;
        }
        match unsafe { next_item(source) } {
            NextItem::Value(value) => items.push(value),
            NextItem::Error => return ptr::null_mut(),
            NextItem::Stop => {
                state.sources[slot] = ptr::null_mut();
                state.active -= 1;
                if state.active == 0 {
                    return raise_stop_iteration();
                }
                items.push(state.fillvalue);
            }
        }
    }
    tuple_from(items)
}

// ---------------------------------------------------------------------------
// product(*iterables, repeat=1)

#[repr(C)]
struct PyItertoolsProduct {
    ob_base: PyObjectHeader,
    pools: Vec<Vec<*mut PyObject>>,
    indices: Vec<usize>,
    started: bool,
    done: bool,
}

static PRODUCT_TYPE: LazyLock<usize> =
    LazyLock::new(|| iterator_type("product", size_of::<PyItertoolsProduct>(), product_next));

unsafe extern "C" fn itertools_product(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("product received an invalid argument window");
    };
    let (positional, keywords) = split_kw_marker(&args);
    let mut repeat = 1_i64;
    for (name, value) in keywords {
        match name.as_str() {
            "repeat" => {
                if unsafe { is_none(value) } {
                    continue;
                }
                let Some(count) = to_i64(value) else {
                    return raise_type_error("'repeat' argument for product() must be an integer");
                };
                repeat = count;
            }
            other => {
                return raise_type_error(&format!(
                    "product() got an unexpected keyword argument '{other}'"
                ))
            }
        }
    }
    if repeat < 0 {
        return raise_value_error("repeat argument cannot be negative");
    }
    let mut base_pools = Vec::with_capacity(positional.len());
    for &iterable in positional {
        let Ok(pool) = (unsafe { collect_iterable(iterable, "product") }) else {
            return ptr::null_mut();
        };
        base_pools.push(pool);
    }
    let mut pools = Vec::with_capacity(base_pools.len() * repeat as usize);
    for _ in 0..repeat {
        pools.extend(base_pools.iter().cloned());
    }
    let indices = vec![0; pools.len()];
    alloc_object(PyItertoolsProduct {
        ob_base: PyObjectHeader::new(*PRODUCT_TYPE as *const PyType),
        pools,
        indices,
        started: false,
        done: false,
    })
}

unsafe extern "C" fn product_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsProduct>() };
    if state.done {
        return raise_stop_iteration();
    }
    if !state.started {
        state.started = true;
        if state.pools.iter().any(Vec::is_empty) {
            state.done = true;
            return raise_stop_iteration();
        }
    } else {
        let mut slot = state.pools.len();
        loop {
            if slot == 0 {
                state.done = true;
                return raise_stop_iteration();
            }
            slot -= 1;
            state.indices[slot] += 1;
            if state.indices[slot] < state.pools[slot].len() {
                break;
            }
            state.indices[slot] = 0;
        }
    }
    let items = state
        .indices
        .iter()
        .zip(state.pools.iter())
        .map(|(&index, pool)| pool[index])
        .collect();
    tuple_from(items)
}

// ---------------------------------------------------------------------------
// permutations(iterable, r=None)

#[repr(C)]
struct PyItertoolsPermutations {
    ob_base: PyObjectHeader,
    pool: Vec<*mut PyObject>,
    r: usize,
    indices: Vec<usize>,
    cycles: Vec<usize>,
    started: bool,
    done: bool,
}

static PERMUTATIONS_TYPE: LazyLock<usize> = LazyLock::new(|| {
    iterator_type("permutations", size_of::<PyItertoolsPermutations>(), permutations_next)
});

unsafe extern "C" fn itertools_permutations(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("permutations received an invalid argument window");
    };
    if args.is_empty() || args.len() > 2 {
        return raise_type_error(&format!("permutations expected 1 or 2 arguments, got {}", args.len()));
    }
    let Ok(pool) = (unsafe { collect_iterable(args[0], "permutations") }) else {
        return ptr::null_mut();
    };
    let n = pool.len();
    let r = if unsafe { is_absent(&args, 1) } {
        n
    } else {
        let Some(r) = to_i64(args[1]) else {
            return raise_type_error("Expected int as r");
        };
        if r < 0 {
            return raise_value_error("r must be non-negative");
        }
        r as usize
    };
    let done = r > n;
    alloc_object(PyItertoolsPermutations {
        ob_base: PyObjectHeader::new(*PERMUTATIONS_TYPE as *const PyType),
        indices: (0..n).collect(),
        cycles: (0..r).map(|index| n - index).collect(),
        pool,
        r,
        started: false,
        done,
    })
}

unsafe extern "C" fn permutations_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsPermutations>() };
    if state.done {
        return raise_stop_iteration();
    }
    if !state.started {
        state.started = true;
    } else {
        let n = state.pool.len();
        let mut produced = false;
        for slot in (0..state.r).rev() {
            state.cycles[slot] -= 1;
            if state.cycles[slot] == 0 {
                state.indices[slot..].rotate_left(1);
                state.cycles[slot] = n - slot;
            } else {
                let swap = n - state.cycles[slot];
                state.indices.swap(slot, swap);
                produced = true;
                break;
            }
        }
        if !produced {
            state.done = true;
            return raise_stop_iteration();
        }
    }
    let items = state.indices[..state.r].iter().map(|&index| state.pool[index]).collect();
    tuple_from(items)
}

// ---------------------------------------------------------------------------
// combinations(iterable, r)

#[repr(C)]
struct PyItertoolsCombinations {
    ob_base: PyObjectHeader,
    pool: Vec<*mut PyObject>,
    r: usize,
    indices: Vec<usize>,
    started: bool,
    done: bool,
}

static COMBINATIONS_TYPE: LazyLock<usize> = LazyLock::new(|| {
    iterator_type("combinations", size_of::<PyItertoolsCombinations>(), combinations_next)
});

unsafe extern "C" fn itertools_combinations(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("combinations received an invalid argument window");
    };
    if args.len() != 2 {
        return raise_type_error(&format!("combinations expected 2 arguments, got {}", args.len()));
    }
    let Ok(pool) = (unsafe { collect_iterable(args[0], "combinations") }) else {
        return ptr::null_mut();
    };
    let Some(r) = to_i64(args[1]) else {
        return raise_type_error("Expected int as r");
    };
    if r < 0 {
        return raise_value_error("r must be non-negative");
    }
    let r = r as usize;
    let done = r > pool.len();
    alloc_object(PyItertoolsCombinations {
        ob_base: PyObjectHeader::new(*COMBINATIONS_TYPE as *const PyType),
        indices: (0..r).collect(),
        pool,
        r,
        started: false,
        done,
    })
}

unsafe extern "C" fn combinations_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsCombinations>() };
    if state.done {
        return raise_stop_iteration();
    }
    let n = state.pool.len();
    if !state.started {
        state.started = true;
    } else {
        let mut slot = state.r;
        loop {
            if slot == 0 {
                state.done = true;
                return raise_stop_iteration();
            }
            slot -= 1;
            if state.indices[slot] != slot + n - state.r {
                break;
            }
        }
        state.indices[slot] += 1;
        for follow in slot + 1..state.r {
            state.indices[follow] = state.indices[follow - 1] + 1;
        }
    }
    let items = state.indices.iter().map(|&index| state.pool[index]).collect();
    tuple_from(items)
}

// ---------------------------------------------------------------------------
// combinations_with_replacement(iterable, r)

#[repr(C)]
struct PyItertoolsCombinationsWithReplacement {
    ob_base: PyObjectHeader,
    pool: Vec<*mut PyObject>,
    r: usize,
    indices: Vec<usize>,
    started: bool,
    done: bool,
}

static COMBINATIONS_WITH_REPLACEMENT_TYPE: LazyLock<usize> = LazyLock::new(|| {
    iterator_type(
        "combinations_with_replacement",
        size_of::<PyItertoolsCombinationsWithReplacement>(),
        combinations_with_replacement_next,
    )
});

unsafe extern "C" fn itertools_combinations_with_replacement(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("combinations_with_replacement received an invalid argument window");
    };
    if args.len() != 2 {
        return raise_type_error(&format!(
            "combinations_with_replacement expected 2 arguments, got {}",
            args.len()
        ));
    }
    let Ok(pool) = (unsafe { collect_iterable(args[0], "combinations_with_replacement") }) else {
        return ptr::null_mut();
    };
    let Some(r) = to_i64(args[1]) else {
        return raise_type_error("Expected int as r");
    };
    if r < 0 {
        return raise_value_error("r must be non-negative");
    }
    let r = r as usize;
    let done = pool.is_empty() && r > 0;
    alloc_object(PyItertoolsCombinationsWithReplacement {
        ob_base: PyObjectHeader::new(*COMBINATIONS_WITH_REPLACEMENT_TYPE as *const PyType),
        indices: vec![0; r],
        pool,
        r,
        started: false,
        done,
    })
}

unsafe extern "C" fn combinations_with_replacement_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsCombinationsWithReplacement>() };
    if state.done {
        return raise_stop_iteration();
    }
    let n = state.pool.len();
    if !state.started {
        state.started = true;
    } else {
        let mut slot = state.r;
        loop {
            if slot == 0 {
                state.done = true;
                return raise_stop_iteration();
            }
            slot -= 1;
            if state.indices[slot] != n - 1 {
                break;
            }
        }
        let next = state.indices[slot] + 1;
        for follow in slot..state.r {
            state.indices[follow] = next;
        }
    }
    let items = state.indices.iter().map(|&index| state.pool[index]).collect();
    tuple_from(items)
}

// ---------------------------------------------------------------------------
// accumulate(iterable, func=None, *, initial=None)

#[repr(C)]
struct PyItertoolsAccumulate {
    ob_base: PyObjectHeader,
    source: *mut PyObject,
    /// Combining callable; NULL means operator addition.
    function: *mut PyObject,
    /// Running total; NULL until the first value is produced.
    total: *mut PyObject,
    /// Pending `initial` value to emit first; NULL when absent/consumed.
    initial: *mut PyObject,
}

static ACCUMULATE_TYPE: LazyLock<usize> = LazyLock::new(|| {
    iterator_type("accumulate", size_of::<PyItertoolsAccumulate>(), accumulate_next)
});

unsafe extern "C" fn itertools_accumulate(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("accumulate received an invalid argument window");
    };
    if args.is_empty() || args.len() > 3 {
        return raise_type_error(&format!("accumulate expected 1 to 3 arguments, got {}", args.len()));
    }
    let source = unsafe { get_iter_checked(args[0], "accumulate") };
    if source.is_null() {
        return ptr::null_mut();
    }
    let function = if unsafe { is_absent(&args, 1) } {
        ptr::null_mut()
    } else {
        args[1]
    };
    let initial = if unsafe { is_absent(&args, 2) } {
        ptr::null_mut()
    } else {
        args[2]
    };
    alloc_object(PyItertoolsAccumulate {
        ob_base: PyObjectHeader::new(*ACCUMULATE_TYPE as *const PyType),
        source,
        function,
        total: ptr::null_mut(),
        initial,
    })
}

unsafe extern "C" fn accumulate_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsAccumulate>() };
    if !state.initial.is_null() {
        state.total = state.initial;
        state.initial = ptr::null_mut();
        return state.total;
    }
    let value = match unsafe { next_item(state.source) } {
        NextItem::Value(value) => value,
        NextItem::Stop => return raise_stop_iteration(),
        NextItem::Error => return ptr::null_mut(),
    };
    if state.total.is_null() {
        state.total = value;
        return value;
    }
    let combined = if state.function.is_null() {
        // SAFETY: Both operands are live heap objects held by this iterator.
        untag(unsafe { abstract_op::binary_op(BINARY_ADD, state.total, value) })
    } else {
        let mut call_args = [state.total, value];
        unsafe { call_function(state.function, &mut call_args) }
    };
    if combined.is_null() {
        return ptr::null_mut();
    }
    state.total = combined;
    combined
}

// ---------------------------------------------------------------------------
// filterfalse(predicate, iterable)

#[repr(C)]
struct PyItertoolsFilterFalse {
    ob_base: PyObjectHeader,
    /// Predicate; NULL/None means truth-test the value itself.
    predicate: *mut PyObject,
    source: *mut PyObject,
}

static FILTERFALSE_TYPE: LazyLock<usize> = LazyLock::new(|| {
    iterator_type("filterfalse", size_of::<PyItertoolsFilterFalse>(), filterfalse_next)
});

unsafe extern "C" fn itertools_filterfalse(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("filterfalse received an invalid argument window");
    };
    if args.len() != 2 {
        return raise_type_error(&format!("filterfalse expected 2 arguments, got {}", args.len()));
    }
    let source = unsafe { get_iter_checked(args[1], "filterfalse") };
    if source.is_null() {
        return ptr::null_mut();
    }
    alloc_object(PyItertoolsFilterFalse {
        ob_base: PyObjectHeader::new(*FILTERFALSE_TYPE as *const PyType),
        predicate: args[0],
        source,
    })
}

unsafe extern "C" fn filterfalse_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsFilterFalse>() };
    loop {
        let value = match unsafe { next_item(state.source) } {
            NextItem::Value(value) => value,
            NextItem::Stop => return raise_stop_iteration(),
            NextItem::Error => return ptr::null_mut(),
        };
        let verdict = if state.predicate.is_null() || unsafe { is_none(state.predicate) } {
            value
        } else {
            let mut call_args = [value];
            let result = unsafe { call_function(state.predicate, &mut call_args) };
            if result.is_null() {
                return ptr::null_mut();
            }
            result
        };
        match unsafe { truth(verdict) } {
            Ok(false) => return value,
            Ok(true) => {}
            Err(()) => return ptr::null_mut(),
        }
    }
}

// ---------------------------------------------------------------------------
// takewhile(predicate, iterable) / dropwhile(predicate, iterable)

#[repr(C)]
struct PyItertoolsTakewhile {
    ob_base: PyObjectHeader,
    predicate: *mut PyObject,
    source: *mut PyObject,
    done: bool,
}

static TAKEWHILE_TYPE: LazyLock<usize> =
    LazyLock::new(|| iterator_type("takewhile", size_of::<PyItertoolsTakewhile>(), takewhile_next));

unsafe extern "C" fn itertools_takewhile(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("takewhile received an invalid argument window");
    };
    if args.len() != 2 {
        return raise_type_error(&format!("takewhile expected 2 arguments, got {}", args.len()));
    }
    let source = unsafe { get_iter_checked(args[1], "takewhile") };
    if source.is_null() {
        return ptr::null_mut();
    }
    alloc_object(PyItertoolsTakewhile {
        ob_base: PyObjectHeader::new(*TAKEWHILE_TYPE as *const PyType),
        predicate: args[0],
        source,
        done: false,
    })
}

unsafe extern "C" fn takewhile_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsTakewhile>() };
    if state.done {
        return raise_stop_iteration();
    }
    let value = match unsafe { next_item(state.source) } {
        NextItem::Value(value) => value,
        NextItem::Stop => {
            state.done = true;
            return raise_stop_iteration();
        }
        NextItem::Error => return ptr::null_mut(),
    };
    let mut call_args = [value];
    let verdict = unsafe { call_function(state.predicate, &mut call_args) };
    if verdict.is_null() {
        return ptr::null_mut();
    }
    match unsafe { truth(verdict) } {
        Ok(true) => value,
        Ok(false) => {
            state.done = true;
            raise_stop_iteration()
        }
        Err(()) => ptr::null_mut(),
    }
}

#[repr(C)]
struct PyItertoolsDropwhile {
    ob_base: PyObjectHeader,
    predicate: *mut PyObject,
    source: *mut PyObject,
    dropping: bool,
}

static DROPWHILE_TYPE: LazyLock<usize> =
    LazyLock::new(|| iterator_type("dropwhile", size_of::<PyItertoolsDropwhile>(), dropwhile_next));

unsafe extern "C" fn itertools_dropwhile(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("dropwhile received an invalid argument window");
    };
    if args.len() != 2 {
        return raise_type_error(&format!("dropwhile expected 2 arguments, got {}", args.len()));
    }
    let source = unsafe { get_iter_checked(args[1], "dropwhile") };
    if source.is_null() {
        return ptr::null_mut();
    }
    alloc_object(PyItertoolsDropwhile {
        ob_base: PyObjectHeader::new(*DROPWHILE_TYPE as *const PyType),
        predicate: args[0],
        source,
        dropping: true,
    })
}

unsafe extern "C" fn dropwhile_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsDropwhile>() };
    loop {
        let value = match unsafe { next_item(state.source) } {
            NextItem::Value(value) => value,
            NextItem::Stop => return raise_stop_iteration(),
            NextItem::Error => return ptr::null_mut(),
        };
        if !state.dropping {
            return value;
        }
        let mut call_args = [value];
        let verdict = unsafe { call_function(state.predicate, &mut call_args) };
        if verdict.is_null() {
            return ptr::null_mut();
        }
        match unsafe { truth(verdict) } {
            Ok(true) => {}
            Ok(false) => {
                state.dropping = false;
                return value;
            }
            Err(()) => return ptr::null_mut(),
        }
    }
}

// ---------------------------------------------------------------------------
// compress(data, selectors)

#[repr(C)]
struct PyItertoolsCompress {
    ob_base: PyObjectHeader,
    data: *mut PyObject,
    selectors: *mut PyObject,
}

static COMPRESS_TYPE: LazyLock<usize> =
    LazyLock::new(|| iterator_type("compress", size_of::<PyItertoolsCompress>(), compress_next));

unsafe extern "C" fn itertools_compress(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("compress received an invalid argument window");
    };
    if args.len() != 2 {
        return raise_type_error(&format!("compress expected 2 arguments, got {}", args.len()));
    }
    let data = unsafe { get_iter_checked(args[0], "compress") };
    if data.is_null() {
        return ptr::null_mut();
    }
    let selectors = unsafe { get_iter_checked(args[1], "compress") };
    if selectors.is_null() {
        return ptr::null_mut();
    }
    alloc_object(PyItertoolsCompress {
        ob_base: PyObjectHeader::new(*COMPRESS_TYPE as *const PyType),
        data,
        selectors,
    })
}

unsafe extern "C" fn compress_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsCompress>() };
    loop {
        let value = match unsafe { next_item(state.data) } {
            NextItem::Value(value) => value,
            NextItem::Stop => return raise_stop_iteration(),
            NextItem::Error => return ptr::null_mut(),
        };
        let selector = match unsafe { next_item(state.selectors) } {
            NextItem::Value(selector) => selector,
            NextItem::Stop => return raise_stop_iteration(),
            NextItem::Error => return ptr::null_mut(),
        };
        match unsafe { truth(selector) } {
            Ok(true) => return value,
            Ok(false) => {}
            Err(()) => return ptr::null_mut(),
        }
    }
}

// ---------------------------------------------------------------------------
// pairwise(iterable)

#[repr(C)]
struct PyItertoolsPairwise {
    ob_base: PyObjectHeader,
    source: *mut PyObject,
    previous: *mut PyObject,
}

static PAIRWISE_TYPE: LazyLock<usize> =
    LazyLock::new(|| iterator_type("pairwise", size_of::<PyItertoolsPairwise>(), pairwise_next));

unsafe extern "C" fn itertools_pairwise(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("pairwise received an invalid argument window");
    };
    if args.len() != 1 {
        return raise_type_error(&format!("pairwise expected 1 argument, got {}", args.len()));
    }
    let source = unsafe { get_iter_checked(args[0], "pairwise") };
    if source.is_null() {
        return ptr::null_mut();
    }
    alloc_object(PyItertoolsPairwise {
        ob_base: PyObjectHeader::new(*PAIRWISE_TYPE as *const PyType),
        source,
        previous: ptr::null_mut(),
    })
}

unsafe extern "C" fn pairwise_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsPairwise>() };
    if state.source.is_null() {
        return raise_stop_iteration();
    }
    if state.previous.is_null() {
        match unsafe { next_item(state.source) } {
            NextItem::Value(value) => state.previous = value,
            NextItem::Stop => {
                state.source = ptr::null_mut();
                return raise_stop_iteration();
            }
            NextItem::Error => return ptr::null_mut(),
        }
    }
    let current = match unsafe { next_item(state.source) } {
        NextItem::Value(value) => value,
        NextItem::Stop => {
            state.source = ptr::null_mut();
            state.previous = ptr::null_mut();
            return raise_stop_iteration();
        }
        NextItem::Error => return ptr::null_mut(),
    };
    let pair = tuple_from(vec![state.previous, current]);
    state.previous = current;
    pair
}

// ---------------------------------------------------------------------------
// batched(iterable, n, *, strict=False)

#[repr(C)]
struct PyItertoolsBatched {
    ob_base: PyObjectHeader,
    source: *mut PyObject,
    n: usize,
    strict: bool,
}

static BATCHED_TYPE: LazyLock<usize> =
    LazyLock::new(|| iterator_type("batched", size_of::<PyItertoolsBatched>(), batched_next));

unsafe extern "C" fn itertools_batched(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("batched received an invalid argument window");
    };
    if args.len() < 2 || args.len() > 3 {
        return raise_type_error(&format!("batched expected 2 or 3 arguments, got {}", args.len()));
    }
    let Some(n) = to_i64(args[1]) else {
        return raise_type_error("expected int as n");
    };
    if n < 1 {
        return raise_value_error("n must be at least one");
    }
    let strict = if unsafe { is_absent(&args, 2) } {
        false
    } else {
        match unsafe { truth(args[2]) } {
            Ok(strict) => strict,
            Err(()) => return ptr::null_mut(),
        }
    };
    let source = unsafe { get_iter_checked(args[0], "batched") };
    if source.is_null() {
        return ptr::null_mut();
    }
    alloc_object(PyItertoolsBatched {
        ob_base: PyObjectHeader::new(*BATCHED_TYPE as *const PyType),
        source,
        n: n as usize,
        strict,
    })
}

unsafe extern "C" fn batched_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsBatched>() };
    if state.source.is_null() {
        return raise_stop_iteration();
    }
    let mut items = Vec::with_capacity(state.n);
    while items.len() < state.n {
        match unsafe { next_item(state.source) } {
            NextItem::Value(value) => items.push(value),
            NextItem::Stop => {
                state.source = ptr::null_mut();
                break;
            }
            NextItem::Error => return ptr::null_mut(),
        }
    }
    if items.is_empty() {
        return raise_stop_iteration();
    }
    if items.len() < state.n && state.strict {
        return raise_value_error("batched(): incomplete batch");
    }
    tuple_from(items)
}

// ---------------------------------------------------------------------------
// groupby(iterable, key=None) and its _grouper

#[repr(C)]
struct PyItertoolsGroupBy {
    ob_base: PyObjectHeader,
    source: *mut PyObject,
    /// Key callable; NULL/None means identity.
    keyfunc: *mut PyObject,
    currkey: *mut PyObject,
    currvalue: *mut PyObject,
    tgtkey: *mut PyObject,
    /// Identity of the grouper currently allowed to consume values.
    currgrouper: *mut PyObject,
}

#[repr(C)]
struct PyItertoolsGrouper {
    ob_base: PyObjectHeader,
    parent: *mut PyObject,
    tgtkey: *mut PyObject,
}

static GROUPBY_TYPE: LazyLock<usize> =
    LazyLock::new(|| iterator_type("groupby", size_of::<PyItertoolsGroupBy>(), groupby_next));

static GROUPER_TYPE: LazyLock<usize> =
    LazyLock::new(|| iterator_type("_grouper", size_of::<PyItertoolsGrouper>(), grouper_next));

unsafe extern "C" fn itertools_groupby(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("groupby received an invalid argument window");
    };
    if args.is_empty() || args.len() > 2 {
        return raise_type_error(&format!("groupby expected 1 or 2 arguments, got {}", args.len()));
    }
    let source = unsafe { get_iter_checked(args[0], "groupby") };
    if source.is_null() {
        return ptr::null_mut();
    }
    let keyfunc = if unsafe { is_absent(&args, 1) } {
        ptr::null_mut()
    } else {
        args[1]
    };
    alloc_object(PyItertoolsGroupBy {
        ob_base: PyObjectHeader::new(*GROUPBY_TYPE as *const PyType),
        source,
        keyfunc,
        currkey: ptr::null_mut(),
        currvalue: ptr::null_mut(),
        tgtkey: ptr::null_mut(),
        currgrouper: ptr::null_mut(),
    })
}

/// Advances the shared cursor one element: fills `currvalue` / `currkey`.
/// On exhaustion raises StopIteration; on failure leaves the error set.
unsafe fn groupby_step(state: &mut PyItertoolsGroupBy) -> Result<(), ()> {
    let value = match unsafe { next_item(state.source) } {
        NextItem::Value(value) => value,
        NextItem::Stop => {
            raise_stop_iteration();
            return Err(());
        }
        NextItem::Error => return Err(()),
    };
    let key = if state.keyfunc.is_null() || unsafe { is_none(state.keyfunc) } {
        value
    } else {
        let mut call_args = [value];
        let key = unsafe { call_function(state.keyfunc, &mut call_args) };
        if key.is_null() {
            return Err(());
        }
        key
    };
    state.currvalue = value;
    state.currkey = key;
    Ok(())
}

unsafe extern "C" fn groupby_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsGroupBy>() };
    state.currgrouper = ptr::null_mut();
    // Skip the remainder of the previous group, then land on the next key.
    loop {
        if state.currkey.is_null() {
            if unsafe { groupby_step(state) }.is_err() {
                return ptr::null_mut();
            }
            continue;
        }
        if state.tgtkey.is_null() {
            break;
        }
        match unsafe { objects_equal(state.tgtkey, state.currkey) } {
            Ok(true) => {
                if unsafe { groupby_step(state) }.is_err() {
                    return ptr::null_mut();
                }
            }
            Ok(false) => break,
            Err(()) => return ptr::null_mut(),
        }
    }
    state.tgtkey = state.currkey;
    let grouper = alloc_object(PyItertoolsGrouper {
        ob_base: PyObjectHeader::new(*GROUPER_TYPE as *const PyType),
        parent: object,
        tgtkey: state.currkey,
    });
    state.currgrouper = grouper;
    tuple_from(vec![state.currkey, grouper])
}

unsafe extern "C" fn grouper_next(object: *mut PyObject) -> *mut PyObject {
    let grouper = unsafe { &mut *object.cast::<PyItertoolsGrouper>() };
    let state = unsafe { &mut *grouper.parent.cast::<PyItertoolsGroupBy>() };
    if state.currgrouper != object {
        return raise_stop_iteration();
    }
    if state.currvalue.is_null() && unsafe { groupby_step(state) }.is_err() {
        return ptr::null_mut();
    }
    match unsafe { objects_equal(state.currkey, grouper.tgtkey) } {
        Ok(true) => {}
        Ok(false) => return raise_stop_iteration(),
        Err(()) => return ptr::null_mut(),
    }
    let value = state.currvalue;
    state.currvalue = ptr::null_mut();
    value
}

// ---------------------------------------------------------------------------
// tee(iterable, n=2)

/// State shared by every clone of one `tee()` call: the single source
/// iterator plus the pulled items some clone has not consumed yet.  `base`
/// is the absolute index of `buffer[0]`; each clone records the absolute
/// index it reads next, and after every step the prefix consumed by ALL
/// clones is dropped — CPython's teedataobject chain frees exhausted blocks
/// the same way.  A clone abandoned mid-stream keeps its slot pinned
/// (itertools objects are immortal leaked boxes), retaining the buffer from
/// its position on: memory-only divergence, values are unaffected.
struct TeeShared {
    source: *mut PyObject,
    exhausted: bool,
    buffer: VecDeque<*mut PyObject>,
    base: usize,
    positions: Vec<usize>,
}

#[repr(C)]
struct PyItertoolsTee {
    ob_base: PyObjectHeader,
    /// Leaked box shared with the sibling clones of one `tee()` call.
    shared: *mut TeeShared,
    /// This clone's slot in [`TeeShared::positions`].
    slot: usize,
}

static TEE_TYPE: LazyLock<usize> = LazyLock::new(|| iterator_type("_tee", size_of::<PyItertoolsTee>(), tee_next));
static TEE_DATAOBJECT_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let ty = PyType::new(
        abi::runtime_type_type().cast_const(),
        "_tee_dataobject",
        0,
    );
    Box::into_raw(Box::new(ty)) as usize
});

unsafe extern "C" fn itertools_tee(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_vec(argv, argc) }) else {
        return raise_type_error("tee received an invalid argument window");
    };
    let (args, named) = split_kw_marker(&args);
    if !named.is_empty() {
        return raise_type_error("itertools.tee() takes no keyword arguments");
    }
    if args.is_empty() {
        return raise_type_error("tee expected at least 1 argument, got 0");
    }
    if args.len() > 2 {
        return raise_type_error(&format!("tee expected at most 2 arguments, got {}", args.len()));
    }
    let n = match args.get(1) {
        None => 2,
        Some(&value) if value.is_null() => 2,
        Some(&value) => {
            let Some(n) = to_i64(value) else {
                let name = unsafe { crate::types::dict::type_name(value) }.unwrap_or("object");
                return raise_type_error(&format!("'{name}' object cannot be interpreted as an integer"));
            };
            if n < 0 {
                return raise_value_error("n must be >= 0");
            }
            n as usize
        }
    };
    // CPython returns the empty tuple BEFORE touching the iterable.
    if n == 0 {
        return tuple_from(Vec::new());
    }
    // SAFETY: `pon_get_iter` self-normalizes its argument.
    let source = untag(unsafe { pon_get_iter(args[0], ptr::null_mut()) });
    if source.is_null() {
        pon_err_clear();
        let name = unsafe { crate::types::dict::type_name(args[0]) }.unwrap_or("object");
        return raise_type_error(&format!("'{name}' object is not iterable"));
    }
    let shared = Box::into_raw(Box::new(TeeShared {
        source,
        exhausted: false,
        buffer: VecDeque::new(),
        base: 0,
        positions: vec![0; n],
    }));
    let mut clones = Vec::with_capacity(n);
    for slot in 0..n {
        let clone = alloc_object(PyItertoolsTee {
            ob_base: PyObjectHeader::new(*TEE_TYPE as *const PyType),
            shared,
            slot,
        });
        if clone.is_null() {
            return ptr::null_mut();
        }
        clones.push(clone);
    }
    tuple_from(clones)
}

unsafe extern "C" fn tee_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyItertoolsTee>() };
    // SAFETY: `shared` is the leaked box installed at construction.
    let shared = unsafe { &mut *state.shared };
    let rel = shared.positions[state.slot] - shared.base;
    let item = if let Some(&buffered) = shared.buffer.get(rel) {
        buffered
    } else if shared.exhausted {
        return raise_stop_iteration();
    } else {
        match unsafe { next_item(shared.source) } {
            NextItem::Value(value) => {
                shared.buffer.push_back(value);
                value
            }
            NextItem::Stop => {
                shared.exhausted = true;
                return raise_stop_iteration();
            }
            NextItem::Error => return ptr::null_mut(),
        }
    };
    shared.positions[state.slot] += 1;
    // Drop the prefix every clone has consumed.
    let slowest = shared.positions.iter().copied().min().unwrap_or(shared.base);
    while shared.base < slowest {
        shared.buffer.pop_front();
        shared.base += 1;
    }
    item
}
