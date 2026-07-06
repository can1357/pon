//! Live `instance.__dict__` view (CPython parity for heap instances).
//!
//! Instance attributes live in an internal [`PyClassDict`] (interned-key,
//! insertion-ordered), not in a Python dict object.  CPython hands out THE
//! backing dict for `obj.__dict__`, so mutations through it are attribute
//! mutations (`unittest.mock` initializes every mock via
//! `self.__dict__['_mock_name'] = …`).  This module provides a dict-shaped
//! view object that proxies every operation to the owning instance's live
//! `PyClassDict`, replacing both prior behaviors: the raw
//! `PyClassDict`-cast-as-`PyObject` from the `__slots__` member descriptor
//! (typeless — any use dereferenced garbage) and the disconnected snapshot
//! from the generic getattr arm (writes were silently lost).
//!
//! The view binds the INSTANCE, not the `PyClassDict` pointer: a later
//! wholesale `obj.__dict__ = {…}` assignment swaps the backing pointer, and
//! re-reading it per operation keeps old views memory-safe (they track the
//! replacement — a documented divergence from CPython, where a captured
//! `__dict__` keeps aliasing the replaced dict).  A fresh view is allocated
//! per access, so `obj.__dict__ is obj.__dict__` is `False` (CPython:
//! `True`) — contents and mutation semantics are identical, only identity
//! diverges.
//!
//! Keys are interned strings; non-`str` keys raise `TypeError` (CPython
//! instance dicts accept arbitrary hashables — divergence accepted until a
//! real dict backs instance storage).
//!
//! Views and key iterators are REAL GC-heap objects ([`TypeId`]s below) with
//! precise trace functions: a view keeps its instance alive (and with it the
//! attribute values), and dead views are swept like any other object — no
//! immortal registries on this hot per-access path.

use core::{ffi::c_int, ptr};
use std::sync::LazyLock;

use pon_gc::{GcTypeInfo, TypeId};

use crate::{
	abi, intern,
	object::{PyMappingMethods, PyObject, PyObjectHeader, PySequenceMethods, PyType},
	types::type_::{PyClassDict, PyHeapInstance, new_namespace, unicode_text},
};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

/// GC type id for live `__dict__` views.
pub const TYPE_ID_INSTANCE_DICT_VIEW: TypeId = TypeId(12);
/// GC type id for `__dict__` key iterators.
pub const TYPE_ID_INSTANCE_DICT_ITER: TypeId = TypeId(13);

// ---------------------------------------------------------------------------
// Layouts

/// Live view over one heap instance's attribute namespace.
#[repr(C)]
pub struct PyInstanceDict {
	ob_base:  PyObjectHeader,
	/// Owning instance; the backing `PyClassDict` is re-read through it on
	/// every operation (NEVER cached — `__dict__` assignment replaces it).
	instance: *mut PyHeapInstance,
}

/// Key iterator over a captured key snapshot.  Keys are interned ids (`u32`),
/// not object pointers, so the snapshot cannot go stale under GC; a snapshot
/// also sidesteps CPython's mutation-during-iteration RuntimeError guard
/// while keeping insertion order.
#[repr(C)]
struct PyInstanceDictIter {
	ob_base: PyObjectHeader,
	keys:    Vec<u32>,
	index:   usize,
}

// ---------------------------------------------------------------------------
// Types

static VIEW_MAPPING: PyMappingMethods = PyMappingMethods {
	mp_length:        Some(view_len_slot),
	mp_subscript:     Some(view_subscript_slot),
	mp_ass_subscript: Some(view_ass_subscript_slot),
};

static VIEW_SEQUENCE: PySequenceMethods = PySequenceMethods {
	sq_length: Some(view_len_slot),
	sq_contains: Some(view_contains_slot),
	..PySequenceMethods::EMPTY
};

static VIEW_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"instance_dict",
		std::mem::size_of::<PyInstanceDict>(),
	);
	ty.gc_type_id = TYPE_ID_INSTANCE_DICT_VIEW.0 as usize;
	ty.tp_getattro = Some(view_getattro);
	ty.tp_repr = Some(view_repr);
	ty.tp_str = Some(view_repr);
	ty.tp_bool = Some(view_bool);
	ty.tp_iter = Some(view_iter);
	ty.tp_richcmp = Some(view_richcmp_slot);
	ty.tp_as_mapping = ptr::addr_of!(VIEW_MAPPING).cast_mut();
	ty.tp_as_sequence = ptr::addr_of!(VIEW_SEQUENCE).cast_mut();
	Box::into_raw(Box::new(ty)) as usize
});

static VIEW_ITER_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"instance_dict_keyiterator",
		std::mem::size_of::<PyInstanceDictIter>(),
	);
	ty.gc_type_id = TYPE_ID_INSTANCE_DICT_ITER.0 as usize;
	ty.tp_iter = Some(identity_iter);
	ty.tp_iternext = Some(view_iter_next);
	Box::into_raw(Box::new(ty)) as usize
});

// ---------------------------------------------------------------------------
// GC integration and allocation

/// Precise trace: a view holds exactly its owning instance (attribute values
/// are traced through the instance's own trace fn).  A zeroed just-allocated
/// block traces as empty via the NULL guard.
unsafe extern "C" fn trace_view(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
	if object.is_null() {
		return;
	}
	let view = unsafe { &*object.cast::<PyInstanceDict>() };
	if !view.instance.is_null() {
		visitor(view.instance.cast::<u8>());
	}
}

/// Iterators hold interned key ids only — nothing to trace.
unsafe extern "C" fn trace_iter(_object: *mut u8, _visitor: &mut dyn FnMut(*mut u8)) {}

/// Drop the iterator's Rust-owned key buffer when the GC sweeps it.
unsafe extern "C" fn finalize_iter(object: *mut u8) {
	if object.is_null() {
		return;
	}
	let iter = object.cast::<PyInstanceDictIter>();
	unsafe { ptr::drop_in_place(ptr::addr_of_mut!((*iter).keys)) };
}

/// Allocate a live `__dict__` view for `instance`, materializing an empty
/// namespace first when attribute storage was never allocated (CPython:
/// reading `obj.__dict__` materializes the dict so writes through it stick).
#[must_use]
pub unsafe fn new_view(instance: *mut PyHeapInstance) -> *mut PyObject {
	if instance.is_null() {
		return fail("__dict__ receiver is NULL");
	}
	if unsafe { (*instance).dict.is_null() } {
		unsafe { (*instance).dict = new_namespace() };
	}
	let info = GcTypeInfo {
		size:     std::mem::size_of::<PyInstanceDict>(),
		trace:    trace_view,
		finalize: None,
	};
	let object = match abi::alloc_gc_object(TYPE_ID_INSTANCE_DICT_VIEW, info) {
		Ok(object) => object.cast::<PyInstanceDict>(),
		Err(message) => return fail(message),
	};
	unsafe {
		ptr::write(object, PyInstanceDict {
			ob_base: PyObjectHeader::new(*VIEW_TYPE as *mut PyType),
			instance,
		});
	}
	object.cast::<PyObject>()
}

// ---------------------------------------------------------------------------
// Helpers

fn fail(message: impl Into<String>) -> *mut PyObject {
	crate::thread_state::pon_err_set(message);
	ptr::null_mut()
}

fn raise_type_error(message: &str) -> *mut PyObject {
	unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

unsafe fn as_view<'a>(object: *mut PyObject) -> Option<&'a PyInstanceDict> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || !crate::tag::is_heap(object) {
		return None;
	}
	if unsafe { (*object).ob_type } != (*VIEW_TYPE as *mut PyType).cast_const() {
		return None;
	}
	Some(unsafe { &*object.cast::<PyInstanceDict>() })
}

/// The view's live backing namespace (materialized non-null at creation, but
/// a later `__dict__ = …` swap re-reads through the instance).
unsafe fn backing<'a>(view: &PyInstanceDict) -> Option<&'a mut PyClassDict> {
	let dict = unsafe { (*view.instance).dict };
	if dict.is_null() {
		None
	} else {
		Some(unsafe { &mut *dict })
	}
}

/// Interned id for a `str` key object, or `None` for non-string keys.
unsafe fn key_id(key: *mut PyObject) -> Option<u32> {
	let text = unsafe { unicode_text(crate::tag::untag_arg(key)) }?;
	Some(intern::intern(text))
}

fn alloc_str(text: &str) -> *mut PyObject {
	unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn key_spelling(key: u32) -> String {
	intern::resolve(key).unwrap_or_else(|| format!("<interned:{key}>"))
}

/// Snapshot of `(key, value)` pairs in insertion order.  Value pointers stay
/// rooted by the instance itself for the duration of the native call (the
/// caller's frame keeps the view — and through it the instance — alive).
unsafe fn entries_snapshot(view: &PyInstanceDict) -> Vec<(u32, *mut PyObject)> {
	match unsafe { backing(view) } {
		Some(dict) => dict.iter().collect(),
		None => Vec::new(),
	}
}

// ---------------------------------------------------------------------------
// Protocol slots

unsafe extern "C" fn view_len_slot(object: *mut PyObject) -> isize {
	match unsafe { as_view(object) } {
		Some(view) => unsafe { entries_snapshot(view) }.len() as isize,
		None => {
			fail("instance __dict__ receiver is invalid");
			-1
		},
	}
}

unsafe extern "C" fn view_bool(object: *mut PyObject) -> c_int {
	match unsafe { as_view(object) } {
		Some(view) => c_int::from(!unsafe { entries_snapshot(view) }.is_empty()),
		None => -1,
	}
}

unsafe extern "C" fn view_subscript_slot(
	object: *mut PyObject,
	key: *mut PyObject,
) -> *mut PyObject {
	let Some(view) = (unsafe { as_view(object) }) else {
		return fail("instance __dict__ receiver is invalid");
	};
	let Some(name) = (unsafe { key_id(key) }) else {
		return raise_type_error("pon instance __dict__ keys must be strings");
	};
	match unsafe { backing(view) }.and_then(|dict| dict.get(name)) {
		Some(value) => value,
		None => unsafe { abi::exc::pon_raise_key_error(key) },
	}
}

unsafe extern "C" fn view_ass_subscript_slot(
	object: *mut PyObject,
	key: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	let Some(view) = (unsafe { as_view(object) }) else {
		fail("instance __dict__ receiver is invalid");
		return -1;
	};
	let Some(name) = (unsafe { key_id(key) }) else {
		raise_type_error("pon instance __dict__ keys must be strings");
		return -1;
	};
	let Some(dict) = (unsafe { backing(view) }) else {
		fail("instance __dict__ storage is unallocated");
		return -1;
	};
	if value.is_null() {
		// Deletion (`del d[k]`): missing keys raise KeyError like CPython.
		if dict.del(name) {
			0
		} else {
			unsafe { abi::exc::pon_raise_key_error(key) };
			-1
		}
	} else {
		dict.set(name, crate::tag::untag_arg(value));
		0
	}
}

unsafe extern "C" fn view_contains_slot(object: *mut PyObject, key: *mut PyObject) -> c_int {
	let Some(view) = (unsafe { as_view(object) }) else {
		fail("instance __dict__ receiver is invalid");
		return -1;
	};
	// Non-string keys are never present (they cannot be stored).
	let Some(name) = (unsafe { key_id(key) }) else {
		return 0;
	};
	c_int::from(
		unsafe { backing(view) }
			.and_then(|dict| dict.get(name))
			.is_some(),
	)
}

unsafe extern "C" fn identity_iter(object: *mut PyObject) -> *mut PyObject {
	object
}

unsafe extern "C" fn view_iter(object: *mut PyObject) -> *mut PyObject {
	let Some(view) = (unsafe { as_view(object) }) else {
		return fail("instance __dict__ receiver is invalid");
	};
	let keys: Vec<u32> = unsafe { entries_snapshot(view) }
		.into_iter()
		.map(|(key, _)| key)
		.collect();
	let info = GcTypeInfo {
		size:     std::mem::size_of::<PyInstanceDictIter>(),
		trace:    trace_iter,
		finalize: Some(finalize_iter),
	};
	let iter = match abi::alloc_gc_object(TYPE_ID_INSTANCE_DICT_ITER, info) {
		Ok(object) => object.cast::<PyInstanceDictIter>(),
		Err(message) => return fail(message),
	};
	unsafe {
		ptr::write(iter, PyInstanceDictIter {
			ob_base: PyObjectHeader::new(*VIEW_ITER_TYPE as *mut PyType),
			keys,
			index: 0,
		});
	}
	iter.cast::<PyObject>()
}

unsafe extern "C" fn view_iter_next(object: *mut PyObject) -> *mut PyObject {
	let object = crate::tag::untag_arg(object);
	if object.is_null() {
		return fail("instance __dict__ iterator receiver is NULL");
	}
	// SAFETY: Receiver is a live PyInstanceDictIter allocated by `view_iter`.
	let iter = unsafe { &mut *object.cast::<PyInstanceDictIter>() };
	match iter.keys.get(iter.index) {
		Some(&key) => {
			iter.index += 1;
			alloc_str(&key_spelling(key))
		},
		None => unsafe { abi::exc::pon_raise_stop_iteration(ptr::null_mut()) },
	}
}

unsafe extern "C" fn view_repr(object: *mut PyObject) -> *mut PyObject {
	let Some(view) = (unsafe { as_view(object) }) else {
		return fail("instance __dict__ receiver is invalid");
	};
	let mut out = String::from("{");
	for (index, (key, value)) in unsafe { entries_snapshot(view) }.into_iter().enumerate() {
		if index > 0 {
			out.push_str(", ");
		}
		out.push_str(&format!("'{}': ", key_spelling(key)));
		out.push_str(&crate::native::builtins_mod::repr_text(value));
	}
	out.push('}');
	alloc_str(&out)
}

/// `==`/`!=` against another view or a real dict, by string-keyed entries;
/// other comparisons return NotImplemented so the dispatcher's identity
/// fallback and TypeError run (CPython dict semantics; the compare codegen
/// consumes RAW slot results, so booleans must be real `bool` singletons).
unsafe extern "C" fn view_richcmp_slot(
	left: *mut PyObject,
	right: *mut PyObject,
	op: c_int,
) -> *mut PyObject {
	const RICH_EQ: c_int = 2;
	const RICH_NE: c_int = 3;
	if op != RICH_EQ && op != RICH_NE {
		return unsafe { abi::pon_not_implemented() };
	}
	let Some(view) = (unsafe { as_view(left) }) else {
		return fail("instance __dict__ receiver is invalid");
	};
	let Some(other) = (unsafe { other_entries(right) }) else {
		return unsafe { abi::pon_not_implemented() };
	};
	let mine = unsafe { entries_snapshot(view) };
	let mut equal = mine.len() == other.len();
	if equal {
		for (key, value) in mine {
			let Some(&(_, theirs)) = other.iter().find(|(name, _)| *name == key) else {
				equal = false;
				break;
			};
			match value_equal(value, theirs) {
				Ok(true) => {},
				Ok(false) => {
					equal = false;
					break;
				},
				Err(()) => return ptr::null_mut(),
			}
		}
	}
	unsafe { abi::pon_const_bool(c_int::from(equal == (op == RICH_EQ))) }
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

/// String-keyed entries of a comparison/update peer: another view or a real
/// dict.  A non-string dict key maps to the `u32::MAX` sentinel (never a
/// real interned id): equality treats it as a guaranteed miss, `update`
/// rejects it.
unsafe fn other_entries(object: *mut PyObject) -> Option<Vec<(u32, *mut PyObject)>> {
	if let Some(view) = unsafe { as_view(object) } {
		return Some(unsafe { entries_snapshot(view) });
	}
	let object = crate::tag::untag_arg(object);
	if unsafe { crate::types::dict::is_dict(object) } {
		let entries = unsafe { crate::types::dict::dict_entries_snapshot(object) }.ok()?;
		let mut out = Vec::with_capacity(entries.len());
		for entry in entries {
			match unsafe { unicode_text(entry.key) } {
				Some(text) => out.push((intern::intern(text), entry.value)),
				None => out.push((u32::MAX, entry.value)),
			}
		}
		return Some(out);
	}
	None
}

// ---------------------------------------------------------------------------
// Python-level methods

unsafe extern "C" fn view_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let Some(name_text) = (unsafe { unicode_text(crate::tag::untag_arg(name)) }) else {
		return fail("attribute name must be str");
	};
	if unsafe { as_view(object) }.is_none() {
		return fail("instance __dict__ receiver is invalid");
	}
	match name_text {
		"get" => bound_method(object, name_text, view_get_method),
		"keys" => bound_method(object, name_text, view_keys_method),
		"values" => bound_method(object, name_text, view_values_method),
		"items" => bound_method(object, name_text, view_items_method),
		"update" => bound_method(object, name_text, view_update_method),
		"pop" => bound_method(object, name_text, view_pop_method),
		"setdefault" => bound_method(object, name_text, view_setdefault_method),
		"clear" => bound_method(object, name_text, view_clear_method),
		"copy" => bound_method(object, name_text, view_copy_method),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern::intern(name_text)) },
	}
}

fn bound_method(receiver: *mut PyObject, name: &str, entry: BuiltinFn) -> *mut PyObject {
	// SAFETY: `entry` is a live builtin entry point with the runtime calling
	// convention.
	let function = unsafe {
		abi::pon_make_function(
			entry as *const u8,
			crate::native::builtins_mod::VARIADIC_ARITY,
			intern::intern(name),
		)
	};
	if function.is_null() {
		return ptr::null_mut();
	}
	match crate::types::method::new_bound_method(function, receiver) {
		Ok(method) => method.cast::<PyObject>(),
		Err(message) => fail(message),
	}
}

unsafe fn method_view_and_args<'a>(
	argv: *mut *mut PyObject,
	argc: usize,
	method: &str,
) -> Result<(&'a PyInstanceDict, &'a [*mut PyObject]), *mut PyObject> {
	if argv.is_null() || argc == 0 {
		return Err(fail(format!("__dict__.{method} received no receiver")));
	}
	let args = unsafe { std::slice::from_raw_parts(argv, argc) };
	let Some(view) = (unsafe { as_view(args[0]) }) else {
		return Err(fail(format!("__dict__.{method} receiver is invalid")));
	};
	Ok((view, &args[1..]))
}

unsafe extern "C" fn view_get_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (view, args) = match unsafe { method_view_and_args(argv, argc, "get") } {
		Ok(pair) => pair,
		Err(raised) => return raised,
	};
	if args.is_empty() || args.len() > 2 {
		return raise_type_error("get expected 1 or 2 arguments");
	}
	let default = args
		.get(1)
		.copied()
		.unwrap_or_else(|| unsafe { abi::pon_none() });
	let Some(name) = (unsafe { key_id(args[0]) }) else {
		return default;
	};
	unsafe { backing(view) }
		.and_then(|dict| dict.get(name))
		.unwrap_or(default)
}

unsafe extern "C" fn view_keys_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (view, _) = match unsafe { method_view_and_args(argv, argc, "keys") } {
		Ok(pair) => pair,
		Err(raised) => return raised,
	};
	let mut keys = unsafe { entries_snapshot(view) }
		.into_iter()
		.map(|(key, _)| alloc_str(&key_spelling(key)))
		.collect::<Vec<_>>();
	unsafe { abi::seq::pon_build_list(keys.as_mut_ptr(), keys.len()) }
}

unsafe extern "C" fn view_values_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (view, _) = match unsafe { method_view_and_args(argv, argc, "values") } {
		Ok(pair) => pair,
		Err(raised) => return raised,
	};
	let mut values = unsafe { entries_snapshot(view) }
		.into_iter()
		.map(|(_, value)| value)
		.collect::<Vec<_>>();
	unsafe { abi::seq::pon_build_list(values.as_mut_ptr(), values.len()) }
}

unsafe extern "C" fn view_items_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (view, _) = match unsafe { method_view_and_args(argv, argc, "items") } {
		Ok(pair) => pair,
		Err(raised) => return raised,
	};
	let mut pairs = Vec::new();
	for (key, value) in unsafe { entries_snapshot(view) } {
		let key = alloc_str(&key_spelling(key));
		let mut pair = [key, value];
		pairs.push(unsafe { abi::seq::pon_build_tuple(pair.as_mut_ptr(), pair.len()) });
	}
	unsafe { abi::seq::pon_build_list(pairs.as_mut_ptr(), pairs.len()) }
}

unsafe extern "C" fn view_update_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (view, args) = match unsafe { method_view_and_args(argv, argc, "update") } {
		Ok(pair) => pair,
		Err(raised) => return raised,
	};
	if args.len() != 1 {
		return raise_type_error("update expected exactly 1 argument");
	}
	let Some(entries) = (unsafe { other_entries(args[0]) }) else {
		return raise_type_error("update argument must be a dict or instance __dict__");
	};
	let Some(dict) = (unsafe { backing(view) }) else {
		return fail("instance __dict__ storage is unallocated");
	};
	for (key, value) in entries {
		if key == u32::MAX {
			return raise_type_error("pon instance __dict__ keys must be strings");
		}
		dict.set(key, crate::tag::untag_arg(value));
	}
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn view_pop_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (view, args) = match unsafe { method_view_and_args(argv, argc, "pop") } {
		Ok(pair) => pair,
		Err(raised) => return raised,
	};
	if args.is_empty() || args.len() > 2 {
		return raise_type_error("pop expected 1 or 2 arguments");
	}
	let name = unsafe { key_id(args[0]) };
	let Some(dict) = (unsafe { backing(view) }) else {
		return fail("instance __dict__ storage is unallocated");
	};
	if let Some(name) = name {
		if let Some(value) = dict.get(name) {
			dict.del(name);
			return value;
		}
	}
	match args.get(1) {
		Some(&default) => default,
		None => unsafe { abi::exc::pon_raise_key_error(args[0]) },
	}
}

unsafe extern "C" fn view_setdefault_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let (view, args) = match unsafe { method_view_and_args(argv, argc, "setdefault") } {
		Ok(pair) => pair,
		Err(raised) => return raised,
	};
	if args.is_empty() || args.len() > 2 {
		return raise_type_error("setdefault expected 1 or 2 arguments");
	}
	let Some(name) = (unsafe { key_id(args[0]) }) else {
		return raise_type_error("pon instance __dict__ keys must be strings");
	};
	let default = args
		.get(1)
		.copied()
		.unwrap_or_else(|| unsafe { abi::pon_none() });
	let Some(dict) = (unsafe { backing(view) }) else {
		return fail("instance __dict__ storage is unallocated");
	};
	match dict.get(name) {
		Some(value) => value,
		None => {
			dict.set(name, crate::tag::untag_arg(default));
			default
		},
	}
}

unsafe extern "C" fn view_clear_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (view, _) = match unsafe { method_view_and_args(argv, argc, "clear") } {
		Ok(pair) => pair,
		Err(raised) => return raised,
	};
	if let Some(dict) = unsafe { backing(view) } {
		let keys = dict.iter().map(|(key, _)| key).collect::<Vec<_>>();
		for key in keys {
			dict.del(key);
		}
	}
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn view_copy_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (view, _) = match unsafe { method_view_and_args(argv, argc, "copy") } {
		Ok(pair) => pair,
		Err(raised) => return raised,
	};
	// CPython `dict.copy`: a REAL detached dict.
	let out = unsafe { abi::map::pon_build_map(ptr::null_mut(), 0) };
	if out.is_null() {
		return ptr::null_mut();
	}
	for (key, value) in unsafe { entries_snapshot(view) } {
		let key = alloc_str(&key_spelling(key));
		if key.is_null() || unsafe { abi::map::pon_dict_set_item_status(out, key, value) } < 0 {
			return ptr::null_mut();
		}
	}
	out
}
