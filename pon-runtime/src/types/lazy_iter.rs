//! Native lazy iterator objects for builtins that must not materialize inputs.
//!
//! These are intentionally small CPython-shaped iterator types (`map`,
//! `filter`, `zip`, and `list_reverseiterator`) instead of generator frames.
//! They keep the builtin surface lazy while using the runtime's normal
//! iterator/call/sequence protocols for every step.

use core::{ffi::c_int, mem, ptr};
use std::sync::LazyLock;

use crate::{
	abi,
	gcroot::{HeldRoots, RootRegistry},
	object::{PyObject, PyObjectHeader, PyType},
	thread_state::{pon_err_clear, thread_state_lock},
};

#[repr(C)]
#[derive(Debug)]
pub struct PyMap {
	pub ob_base: PyObjectHeader,
	function:    *mut PyObject,
	iters:       Vec<*mut PyObject>,
}

#[repr(C)]
#[derive(Debug)]
pub struct PyFilter {
	pub ob_base: PyObjectHeader,
	function:    *mut PyObject,
	iter:        *mut PyObject,
}

#[repr(C)]
#[derive(Debug)]
pub struct PyZip {
	pub ob_base: PyObjectHeader,
	iters:       Vec<*mut PyObject>,
	strict:      bool,
}

#[repr(C)]
#[derive(Debug)]
pub struct PyReversed {
	pub ob_base: PyObjectHeader,
	seq:         *mut PyObject,
	index:       isize,
}

/// Legacy sequence iterator (CPython `PySeqIter`): wraps an object exposing
/// `__getitem__` but no `__iter__` and steps indexes 0, 1, 2, ... until
/// IndexError.  `seq` is nulled once exhausted so further `next()` calls stop
/// cleanly without re-entering `__getitem__`.
#[repr(C)]
#[derive(Debug)]
pub struct PySeqIter {
	pub ob_base: PyObjectHeader,
	seq:         *mut PyObject,
	index:       i64,
}

#[repr(C)]
#[derive(Debug)]
pub struct PyZipStrictMarker {
	pub ob_base: PyObjectHeader,
	strict:      bool,
}

#[repr(C)]
#[derive(Debug)]
pub struct PyMinMaxOptions {
	pub ob_base: PyObjectHeader,
	key:         *mut PyObject,
	default:     *mut PyObject,
	has_default: bool,
}
#[repr(C)]
#[derive(Debug)]
pub struct PySortOptions {
	pub ob_base: PyObjectHeader,
	key:         *mut PyObject,
	reverse:     bool,
}

/// Keyword-carrier appended by the native binder for variadic builtins whose
/// keyword surface cannot be flattened into fixed positional slots
/// (`itertools.zip_longest(fillvalue=...)`, `itertools.product(repeat=...)`).
/// Pairs are `(interned name, value)` in call order.
#[repr(C)]
#[derive(Debug)]
pub struct PyKwMarker {
	pub ob_base: PyObjectHeader,
	pairs:       Vec<(u32, *mut PyObject)>,
}

#[derive(Clone, Copy, Debug)]
pub struct MinMaxOptions {
	pub key:         *mut PyObject,
	pub default:     *mut PyObject,
	pub has_default: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct SortOptions {
	pub key:     *mut PyObject,
	pub reverse: bool,
}

static MAP_TYPE: LazyLock<usize> =
	LazyLock::new(|| iterator_type("map", mem::size_of::<PyMap>()) as usize);
static FILTER_TYPE: LazyLock<usize> =
	LazyLock::new(|| iterator_type("filter", mem::size_of::<PyFilter>()) as usize);
static ZIP_TYPE: LazyLock<usize> =
	LazyLock::new(|| iterator_type("zip", mem::size_of::<PyZip>()) as usize);
static REVERSED_TYPE: LazyLock<usize> =
	LazyLock::new(|| iterator_type("list_reverseiterator", mem::size_of::<PyReversed>()) as usize);
// CPython spells this type `iterator` (`type(iter(HasGetitemOnly()))`).
static SEQ_ITER_TYPE: LazyLock<usize> =
	LazyLock::new(|| iterator_type("iterator", mem::size_of::<PySeqIter>()) as usize);
static ZIP_STRICT_MARKER_TYPE: LazyLock<usize> =
	LazyLock::new(|| plain_type("zip_strict_marker", mem::size_of::<PyZipStrictMarker>()) as usize);
static MINMAX_OPTIONS_TYPE: LazyLock<usize> =
	LazyLock::new(|| plain_type("minmax_options", mem::size_of::<PyMinMaxOptions>()) as usize);
static SORT_OPTIONS_TYPE: LazyLock<usize> =
	LazyLock::new(|| plain_type("sort_options", mem::size_of::<PySortOptions>()) as usize);
static KW_MARKER_TYPE: LazyLock<usize> =
	LazyLock::new(|| plain_type("kwargs_marker", mem::size_of::<PyKwMarker>()) as usize);

fn iterator_type(name: &'static str, size: usize) -> *mut PyType {
	let mut ty = PyType::new(ptr::null(), name, size);
	ty.tp_iter = Some(identity_iter);
	ty.tp_iternext = Some(iterator_next);
	ty.tp_getattro = Some(crate::abstract_op::iterator_dunder_getattro);
	Box::into_raw(Box::new(ty))
}

fn plain_type(name: &'static str, size: usize) -> *mut PyType {
	Box::into_raw(Box::new(PyType::new(ptr::null(), name, size)))
}

/// Every lazy-iterator / options-carrier allocation, for GC root reporting:
/// the leaked boxes hold source iterators, callables, and option values that
/// live on the GC heap and are invisible to marking (`crate::gcroot`).
/// Objects are immortal, so the registry only grows.  `PyZipStrictMarker`
/// holds no references and is never registered.
static REGISTRY: RootRegistry = RootRegistry::new();

/// References held by live lazy iterators and option carriers.  Consumed by
/// `crate::abi::collect` while the runtime lock is held.
pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
	REGISTRY.held_roots()
}

fn alloc<T: HeldRoots>(value: T) -> *mut PyObject {
	REGISTRY.register::<T>(Box::into_raw(Box::new(value)).cast::<PyObject>())
}

impl HeldRoots for PyMap {
	unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
		push(self.function);
		for &iter in &self.iters {
			push(iter);
		}
	}
}

impl HeldRoots for PyFilter {
	unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
		push(self.function);
		push(self.iter);
	}
}

impl HeldRoots for PyZip {
	unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
		for &iter in &self.iters {
			push(iter);
		}
	}
}

impl HeldRoots for PyReversed {
	unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
		push(self.seq);
	}
}

impl HeldRoots for PySeqIter {
	unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
		push(self.seq);
	}
}

impl HeldRoots for PyMinMaxOptions {
	unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
		push(self.key);
		push(self.default);
	}
}

impl HeldRoots for PySortOptions {
	unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
		push(self.key);
	}
}

impl HeldRoots for PyKwMarker {
	unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
		for &(_, value) in &self.pairs {
			push(value);
		}
	}
}

pub fn new_map(function: *mut PyObject, iters: Vec<*mut PyObject>) -> *mut PyObject {
	alloc(PyMap { ob_base: PyObjectHeader::new(*MAP_TYPE as *const PyType), function, iters })
}

pub fn new_filter(function: *mut PyObject, iter: *mut PyObject) -> *mut PyObject {
	alloc(PyFilter { ob_base: PyObjectHeader::new(*FILTER_TYPE as *const PyType), function, iter })
}

pub fn new_zip(iters: Vec<*mut PyObject>, strict: bool) -> *mut PyObject {
	alloc(PyZip { ob_base: PyObjectHeader::new(*ZIP_TYPE as *const PyType), iters, strict })
}

pub fn new_reversed(seq: *mut PyObject, len: isize) -> *mut PyObject {
	alloc(PyReversed {
		ob_base: PyObjectHeader::new(*REVERSED_TYPE as *const PyType),
		seq,
		index: len.saturating_sub(1),
	})
}

pub fn new_seq_iter(seq: *mut PyObject) -> *mut PyObject {
	alloc(PySeqIter { ob_base: PyObjectHeader::new(*SEQ_ITER_TYPE as *const PyType), seq, index: 0 })
}

/// Never registered: the marker carries only a flag, no GC references.
pub fn new_zip_strict_marker(strict: bool) -> *mut PyObject {
	Box::into_raw(Box::new(PyZipStrictMarker {
		ob_base: PyObjectHeader::new(*ZIP_STRICT_MARKER_TYPE as *const PyType),
		strict,
	}))
	.cast::<PyObject>()
}

pub fn new_minmax_options(
	key: *mut PyObject,
	default: *mut PyObject,
	has_default: bool,
) -> *mut PyObject {
	alloc(PyMinMaxOptions {
		ob_base: PyObjectHeader::new(*MINMAX_OPTIONS_TYPE as *const PyType),
		key,
		default,
		has_default,
	})
}

pub fn new_sort_options(key: *mut PyObject, reverse: bool) -> *mut PyObject {
	alloc(PySortOptions {
		ob_base: PyObjectHeader::new(*SORT_OPTIONS_TYPE as *const PyType),
		key,
		reverse,
	})
}

pub fn new_kw_marker(pairs: Vec<(u32, *mut PyObject)>) -> *mut PyObject {
	alloc(PyKwMarker { ob_base: PyObjectHeader::new(*KW_MARKER_TYPE as *const PyType), pairs })
}

pub unsafe fn zip_strict_marker_value(object: *mut PyObject) -> Option<bool> {
	if unsafe { is_exact_type(object, *ZIP_STRICT_MARKER_TYPE as *const PyType) } {
		Some(unsafe { (*object.cast::<PyZipStrictMarker>()).strict })
	} else {
		None
	}
}

pub unsafe fn minmax_options_value(object: *mut PyObject) -> Option<MinMaxOptions> {
	if unsafe { is_exact_type(object, *MINMAX_OPTIONS_TYPE as *const PyType) } {
		let options = unsafe { &*object.cast::<PyMinMaxOptions>() };
		Some(MinMaxOptions {
			key:         options.key,
			default:     options.default,
			has_default: options.has_default,
		})
	} else {
		None
	}
}
pub unsafe fn sort_options_value(object: *mut PyObject) -> Option<SortOptions> {
	if unsafe { is_exact_type(object, *SORT_OPTIONS_TYPE as *const PyType) } {
		let options = unsafe { &*object.cast::<PySortOptions>() };
		Some(SortOptions { key: options.key, reverse: options.reverse })
	} else {
		None
	}
}

pub unsafe fn kw_marker_pairs<'a>(object: *mut PyObject) -> Option<&'a [(u32, *mut PyObject)]> {
	if unsafe { is_exact_type(object, *KW_MARKER_TYPE as *const PyType) } {
		Some(unsafe { (*object.cast::<PyKwMarker>()).pairs.as_slice() })
	} else {
		None
	}
}

unsafe extern "C" fn identity_iter(object: *mut PyObject) -> *mut PyObject {
	object
}

unsafe extern "C" fn iterator_next(object: *mut PyObject) -> *mut PyObject {
	if object.is_null() {
		return raise_type_error("iterator receiver is NULL");
	}
	let ty = unsafe { (*object).ob_type };
	if ty == *MAP_TYPE as *const PyType {
		unsafe { map_next(object.cast::<PyMap>()) }
	} else if ty == *FILTER_TYPE as *const PyType {
		unsafe { filter_next(object.cast::<PyFilter>()) }
	} else if ty == *ZIP_TYPE as *const PyType {
		unsafe { zip_next(object.cast::<PyZip>()) }
	} else if ty == *REVERSED_TYPE as *const PyType {
		unsafe { reversed_next(object.cast::<PyReversed>()) }
	} else if ty == *SEQ_ITER_TYPE as *const PyType {
		unsafe { seq_iter_next(object.cast::<PySeqIter>()) }
	} else {
		raise_type_error("object is not an iterator")
	}
}

unsafe fn map_next(iter: *mut PyMap) -> *mut PyObject {
	let iter = unsafe { &mut *iter };
	let mut args = Vec::with_capacity(iter.iters.len());
	for source in iter.iters.iter().copied() {
		match unsafe { next_item(source) } {
			NextItem::Value(value) => args.push(value),
			NextItem::Stop => return raise_stop_iteration(),
			NextItem::Error => return ptr::null_mut(),
		}
	}
	unsafe { abi::pon_call(iter.function, args.as_mut_ptr(), args.len()) }
}

unsafe fn filter_next(iter: *mut PyFilter) -> *mut PyObject {
	let iter = unsafe { &mut *iter };
	loop {
		let value = match unsafe { next_item(iter.iter) } {
			NextItem::Value(value) => value,
			NextItem::Stop => return raise_stop_iteration(),
			NextItem::Error => return ptr::null_mut(),
		};
		let keep_object = if iter.function.is_null() || unsafe { is_none(iter.function) } {
			value
		} else {
			let mut args = [value];
			let result = unsafe { abi::pon_call(iter.function, args.as_mut_ptr(), args.len()) };
			if result.is_null() {
				return ptr::null_mut();
			}
			result
		};
		match unsafe { abi::pon_is_true(keep_object) } {
			1 => return value,
			0 => {},
			_ => return ptr::null_mut(),
		}
	}
}

unsafe fn zip_next(iter: *mut PyZip) -> *mut PyObject {
	let iter = unsafe { &mut *iter };
	if iter.iters.is_empty() {
		return raise_stop_iteration();
	}

	let mut items = Vec::with_capacity(iter.iters.len());
	for (index, source) in iter.iters.iter().copied().enumerate() {
		match unsafe { next_item(source) } {
			NextItem::Value(value) => items.push(value),
			NextItem::Error => return ptr::null_mut(),
			NextItem::Stop => {
				if !iter.strict {
					return raise_stop_iteration();
				}
				if items.is_empty() {
					for (later_index, later) in iter.iters.iter().copied().enumerate().skip(index + 1) {
						match unsafe { next_item(later) } {
							NextItem::Value(_) => {
								return raise_value_error(&zip_mismatch_message(later_index, "longer"));
							},
							NextItem::Stop => {},
							NextItem::Error => return ptr::null_mut(),
						}
					}
					return raise_stop_iteration();
				}
				return raise_value_error(&zip_mismatch_message(index, "shorter"));
			},
		}
	}

	let mut tuple_items = items;
	unsafe { abi::seq::pon_build_tuple(tuple_items.as_mut_ptr(), tuple_items.len()) }
}

unsafe fn reversed_next(iter: *mut PyReversed) -> *mut PyObject {
	let iter = unsafe { &mut *iter };
	if iter.index < 0 {
		return raise_stop_iteration();
	}
	let index = crate::types::int::from_i64(iter.index as i64);
	if index.is_null() {
		return ptr::null_mut();
	}
	let result = unsafe { abi::object::pon_subscript_get(iter.seq, index, ptr::null_mut()) };
	if result.is_null() {
		return ptr::null_mut();
	}
	iter.index -= 1;
	result
}

unsafe fn seq_iter_next(iter: *mut PySeqIter) -> *mut PyObject {
	let iter = unsafe { &mut *iter };
	if iter.seq.is_null() {
		return raise_stop_iteration();
	}
	let index = crate::types::int::from_i64(iter.index);
	if index.is_null() {
		return ptr::null_mut();
	}
	// Full subscript path so user-defined `__getitem__` overrides fire.
	let result = unsafe { abi::object::pon_subscript_get(iter.seq, index, ptr::null_mut()) };
	if result.is_null() {
		// CPython `iterobject.c:seqiter_next`: IndexError or StopIteration
		// raised by `__getitem__` is clean exhaustion; anything else (including
		// message-only diagnostics) propagates to the caller.
		if crate::abi::exc::pending_exception_is("IndexError")
			|| crate::abi::exc::pending_exception_is("StopIteration")
		{
			pon_err_clear();
			iter.seq = ptr::null_mut();
			return raise_stop_iteration();
		}
		return ptr::null_mut();
	}
	iter.index += 1;
	result
}

#[derive(Clone, Copy, Debug)]
enum NextItem {
	Value(*mut PyObject),
	Stop,
	Error,
}

unsafe fn next_item(iter: *mut PyObject) -> NextItem {
	let value = unsafe { abi::pon_iter_next(iter, ptr::null_mut()) };
	if !value.is_null() {
		return NextItem::Value(value);
	}
	if unsafe { current_exception_is("StopIteration") } {
		pon_err_clear();
		NextItem::Stop
	} else {
		NextItem::Error
	}
}

unsafe fn current_exception_is(name: &str) -> bool {
	let current = thread_state_lock().current_exc;
	if current.is_null() || current == core::ptr::NonNull::<PyObject>::dangling().as_ptr() {
		return false;
	}
	let ty = unsafe { (*current).ob_type };
	!ty.is_null() && unsafe { (*ty).name() == name }
}

fn zip_mismatch_message(index: usize, relation: &str) -> String {
	let arg = index + 1;
	if index == 1 {
		format!("zip() argument {arg} is {relation} than argument 1")
	} else {
		format!("zip() argument {arg} is {relation} than arguments 1-{index}")
	}
}

unsafe fn is_none(object: *mut PyObject) -> bool {
	if object.is_null() || !crate::tag::is_heap(object) {
		return false;
	}
	let ty = unsafe { object.as_ref().and_then(|object| object.ob_type.as_ref()) };
	ty.is_some_and(|ty| ty.name() == "NoneType")
}

unsafe fn is_exact_type(object: *mut PyObject, ty: *const PyType) -> bool {
	!object.is_null() && crate::tag::is_heap(object) && unsafe { (*object).ob_type == ty }
}

fn raise_stop_iteration() -> *mut PyObject {
	unsafe { abi::pon_raise_stop_iteration(ptr::null_mut()) }
}

fn raise_type_error(message: &str) -> *mut PyObject {
	unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn raise_value_error(message: &str) -> *mut PyObject {
	unsafe { abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) }
}

#[allow(dead_code)]
unsafe extern "C" fn truth_slot(_object: *mut PyObject) -> c_int {
	1
}
