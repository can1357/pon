//! Containers family: tuple/list/dict/set/slice and abstract container helpers.

use core::{
	ffi::{c_char, c_int},
	mem, ptr,
};
use std::{
	collections::HashMap,
	panic::{AssertUnwindSafe, catch_unwind},
	sync::{LazyLock, Mutex},
};

use num_bigint::Sign;
use num_traits::ToPrimitive;
use pon_gc::{GcTypeInfo, TypeId};

use super::{c_string, object_::normalize_object_arg, twin::ForeignTypeObject};
use crate::{
	abi,
	intern::intern,
	object::{PyMappingMethods, PyObject, PyObjectHeader, PyType},
	thread_state::{pon_err_clear, pon_err_occurred},
	types::{dict, dict::DictEntry, exc::ExceptionKind, frozenset, list, set_, tuple, type_},
};

/// C mirror: `include/pon_capi/containers.h` `PyPonCapiContainers`.
#[repr(C)]
pub(crate) struct PyPonCapiContainers {
	tuple_new:                unsafe extern "C" fn(isize) -> *mut PyObject,
	tuple_size:               unsafe extern "C" fn(*mut PyObject) -> isize,
	tuple_get_item:           unsafe extern "C" fn(*mut PyObject, isize) -> *mut PyObject,
	tuple_set_item:           unsafe extern "C" fn(*mut PyObject, isize, *mut PyObject) -> c_int,
	tuple_pack:               unsafe extern "C" fn(*mut *mut PyObject, isize) -> *mut PyObject,
	tuple_get_slice:          unsafe extern "C" fn(*mut PyObject, isize, isize) -> *mut PyObject,
	list_new:                 unsafe extern "C" fn(isize) -> *mut PyObject,
	list_size:                unsafe extern "C" fn(*mut PyObject) -> isize,
	list_get_item:            unsafe extern "C" fn(*mut PyObject, isize) -> *mut PyObject,
	list_set_item:            unsafe extern "C" fn(*mut PyObject, isize, *mut PyObject) -> c_int,
	list_append:              unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int,
	list_insert:              unsafe extern "C" fn(*mut PyObject, isize, *mut PyObject) -> c_int,
	list_as_tuple:            unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	list_sort:                unsafe extern "C" fn(*mut PyObject) -> c_int,
	dict_new:                 unsafe extern "C" fn() -> *mut PyObject,
	dict_set_item: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int,
	dict_set_item_string: unsafe extern "C" fn(*mut PyObject, *const c_char, *mut PyObject) -> c_int,
	dict_get_item:            unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
	dict_get_item_string:     unsafe extern "C" fn(*mut PyObject, *const c_char) -> *mut PyObject,
	dict_get_item_with_error: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
	dict_del_item:            unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int,
	dict_contains:            unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int,
	dict_size:                unsafe extern "C" fn(*mut PyObject) -> isize,
	dict_keys:                unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	dict_values:              unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	dict_items:               unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	dict_next: unsafe extern "C" fn(
		*mut PyObject,
		*mut isize,
		*mut *mut PyObject,
		*mut *mut PyObject,
	) -> c_int,
	dict_merge:               unsafe extern "C" fn(*mut PyObject, *mut PyObject, c_int) -> c_int,
	dict_update:              unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int,
	dict_copy:                unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	dict_clear:               unsafe extern "C" fn(*mut PyObject),
	set_new:                  unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	set_add:                  unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int,
	set_contains:             unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int,
	set_size:                 unsafe extern "C" fn(*mut PyObject) -> isize,
	slice_new: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject,
	slice_unpack: unsafe extern "C" fn(*mut PyObject, *mut isize, *mut isize, *mut isize) -> c_int,
	slice_adjust_indices:     unsafe extern "C" fn(isize, *mut isize, *mut isize, isize) -> isize,
	sequence_check:           unsafe extern "C" fn(*mut PyObject) -> c_int,
	sequence_size:            unsafe extern "C" fn(*mut PyObject) -> isize,
	sequence_get_item:        unsafe extern "C" fn(*mut PyObject, isize) -> *mut PyObject,
	sequence_set_item:        unsafe extern "C" fn(*mut PyObject, isize, *mut PyObject) -> c_int,
	sequence_contains:        unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int,
	sequence_tuple:           unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	sequence_list:            unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	sequence_fast:            unsafe extern "C" fn(*mut PyObject, *const c_char) -> *mut PyObject,
	sequence_fast_items:      unsafe extern "C" fn(*mut PyObject, *mut isize) -> *mut *mut PyObject,
	mapping_check:            unsafe extern "C" fn(*mut PyObject) -> c_int,
	mapping_keys:             unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	mapping_get_item_string:  unsafe extern "C" fn(*mut PyObject, *const c_char) -> *mut PyObject,
	mapping_set_item_string:
		unsafe extern "C" fn(*mut PyObject, *const c_char, *mut PyObject) -> c_int,
	dict_get_item_ref:
		unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut *mut PyObject) -> c_int,
	dict_del_item_string:     unsafe extern "C" fn(*mut PyObject, *const c_char) -> c_int,
	dict_contains_string:     unsafe extern "C" fn(*mut PyObject, *const c_char) -> c_int,
	dict_set_default_ref: unsafe extern "C" fn(
		*mut PyObject,
		*mut PyObject,
		*mut PyObject,
		*mut *mut PyObject,
	) -> c_int,
	dict_get_item_string_ref:
		unsafe extern "C" fn(*mut PyObject, *const c_char, *mut *mut PyObject) -> c_int,
	dict_proxy_new:           unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	dict_proxy_type:          unsafe extern "C" fn() -> *mut ForeignTypeObject,
	sequence_concat:          unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
	sequence_repeat:          unsafe extern "C" fn(*mut PyObject, isize) -> *mut PyObject,
	sequence_inplace_repeat:  unsafe extern "C" fn(*mut PyObject, isize) -> *mut PyObject,
	sequence_inplace_concat:  unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
	list_get_item_ref:        unsafe extern "C" fn(*mut PyObject, isize) -> *mut PyObject,
	list_set_slice: unsafe extern "C" fn(*mut PyObject, isize, isize, *mut PyObject) -> c_int,
	dict_set_default:
		unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject,
}

unsafe impl Send for PyPonCapiContainers {}
unsafe impl Sync for PyPonCapiContainers {}

pub(crate) fn build() -> PyPonCapiContainers {
	PyPonCapiContainers {
		tuple_new:                capi_tuple_new,
		tuple_size:               capi_tuple_size,
		tuple_get_item:           capi_tuple_get_item,
		tuple_set_item:           capi_tuple_set_item,
		tuple_pack:               capi_tuple_pack,
		tuple_get_slice:          capi_tuple_get_slice,
		list_new:                 capi_list_new,
		list_size:                capi_list_size,
		list_get_item:            capi_list_get_item,
		list_set_item:            capi_list_set_item,
		list_append:              capi_list_append,
		list_insert:              capi_list_insert,
		list_as_tuple:            capi_list_as_tuple,
		list_sort:                capi_list_sort,
		dict_new:                 capi_dict_new,
		dict_set_item:            capi_dict_set_item,
		dict_set_item_string:     capi_dict_set_item_string,
		dict_get_item:            capi_dict_get_item,
		dict_get_item_string:     capi_dict_get_item_string,
		dict_get_item_with_error: capi_dict_get_item_with_error,
		dict_del_item:            capi_dict_del_item,
		dict_contains:            capi_dict_contains,
		dict_size:                capi_dict_size,
		dict_keys:                capi_dict_keys,
		dict_values:              capi_dict_values,
		dict_items:               capi_dict_items,
		dict_next:                capi_dict_next,
		dict_merge:               capi_dict_merge,
		dict_update:              capi_dict_update,
		dict_copy:                capi_dict_copy,
		dict_clear:               capi_dict_clear,
		set_new:                  capi_set_new,
		set_add:                  capi_set_add,
		set_contains:             capi_set_contains,
		set_size:                 capi_set_size,
		slice_new:                capi_slice_new,
		slice_unpack:             capi_slice_unpack,
		slice_adjust_indices:     capi_slice_adjust_indices,
		sequence_check:           capi_sequence_check,
		sequence_size:            capi_sequence_size,
		sequence_get_item:        capi_sequence_get_item,
		sequence_set_item:        capi_sequence_set_item,
		sequence_contains:        capi_sequence_contains,
		sequence_tuple:           capi_sequence_tuple,
		sequence_list:            capi_sequence_list,
		sequence_fast:            capi_sequence_fast,
		sequence_fast_items:      capi_sequence_fast_items,
		mapping_check:            capi_mapping_check,
		mapping_keys:             capi_mapping_keys,
		mapping_get_item_string:  capi_mapping_get_item_string,
		mapping_set_item_string:  capi_mapping_set_item_string,
		dict_get_item_ref:        capi_dict_get_item_ref,
		dict_del_item_string:     capi_dict_del_item_string,
		dict_contains_string:     capi_dict_contains_string,
		dict_set_default_ref:     capi_dict_set_default_ref,
		dict_get_item_string_ref: capi_dict_get_item_string_ref,
		dict_proxy_new:           capi_dict_proxy_new,
		dict_proxy_type:          capi_dict_proxy_type,
		sequence_concat:          capi_sequence_concat,
		sequence_repeat:          capi_sequence_repeat,
		sequence_inplace_repeat:  capi_sequence_inplace_repeat,
		sequence_inplace_concat:  capi_sequence_inplace_concat,
		list_get_item_ref:        capi_list_get_item_ref,
		list_set_slice:           capi_list_set_slice,
		dict_set_default:         capi_dict_set_default,
	}
}

type DictNextKey = (usize, usize);
type DictNextSnapshot = Vec<(usize, usize)>;

static DICT_NEXT_SNAPSHOTS: LazyLock<Mutex<HashMap<DictNextKey, DictNextSnapshot>>> =
	LazyLock::new(|| Mutex::new(HashMap::new()));

const TYPE_ID_DICT_PROXY: TypeId = TypeId(112);

#[repr(C)]
struct PyDictProxy {
	ob_base: PyObjectHeader,
	mapping: *mut PyObject,
}

static DICT_PROXY_MAPPING: PyMappingMethods = PyMappingMethods {
	mp_length:        Some(dict_proxy_len_slot),
	mp_subscript:     Some(dict_proxy_subscript_slot),
	mp_ass_subscript: Some(dict_proxy_ass_subscript_slot),
};

static DICT_PROXY_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty =
		PyType::new(abi::runtime_type_type(), "mappingproxy", mem::size_of::<PyDictProxy>());
	ty.gc_type_id = TYPE_ID_DICT_PROXY.0 as usize;
	ty.tp_as_mapping = ptr::addr_of!(DICT_PROXY_MAPPING).cast_mut();
	Box::into_raw(Box::new(ty)) as usize
});

fn catch_object(f: impl FnOnce() -> *mut PyObject) -> *mut PyObject {
	catch_borrowed_object(|| super::pin_new_reference(f()))
}

fn catch_borrowed_object(f: impl FnOnce() -> *mut PyObject) -> *mut PyObject {
	match catch_unwind(AssertUnwindSafe(f)) {
		Ok(object) => super::foreignize_type_result(object),
		Err(_) => abi::return_null_with_error("container C-API helper panicked"),
	}
}

fn catch_status(f: impl FnOnce() -> c_int) -> c_int {
	match catch_unwind(AssertUnwindSafe(f)) {
		Ok(value) => value,
		Err(_) => abi::return_minus_one_with_error("container C-API helper panicked"),
	}
}

fn catch_size(f: impl FnOnce() -> isize) -> isize {
	match catch_unwind(AssertUnwindSafe(f)) {
		Ok(value) => value,
		Err(_) => {
			abi::return_minus_one_with_error("container C-API helper panicked");
			-1
		},
	}
}

fn tuple_slice(object: *mut PyObject) -> Option<&'static [*mut PyObject]> {
	if object.is_null() || !crate::tag::is_heap(object) {
		return None;
	}
	unsafe { abi::seq::tuple_storage_slice(object) }
}


fn exact_tuple_mut(object: *mut PyObject) -> Option<&'static mut tuple::PyTuple> {
	if object.is_null() || !crate::tag::is_heap(object) {
		return None;
	}
	if unsafe { (*object).ob_type } != abi::seq::tuple_type().cast_const() {
		return None;
	}
	Some(unsafe { &mut *object.cast::<tuple::PyTuple>() })
}

fn list_storage(object: *mut PyObject) -> Option<&'static list::PyListStorage> {
	if object.is_null() || !crate::tag::is_heap(object) {
		return None;
	}
	if unsafe { (*object).ob_type } == abi::seq::list_type().cast_const() {
		let storage = unsafe {
			object
				.cast::<u8>()
				.add(core::mem::offset_of!(list::PyList, len))
		};
		return Some(unsafe { &*storage.cast::<list::PyListStorage>() });
	}
	if unsafe { list::is_list_subclass_instance(object) } {
		return Some(unsafe { &(*object.cast::<list::PyListSubclassInstance>()).storage });
	}
	None
}

fn list_storage_mut(object: *mut PyObject) -> Option<&'static mut list::PyListStorage> {
	if object.is_null() || !crate::tag::is_heap(object) {
		return None;
	}
	if unsafe { (*object).ob_type } == abi::seq::list_type().cast_const() {
		let storage = unsafe {
			object
				.cast::<u8>()
				.add(core::mem::offset_of!(list::PyList, len))
		};
		return Some(unsafe { &mut *storage.cast::<list::PyListStorage>() });
	}
	if unsafe { list::is_list_subclass_instance(object) } {
		return Some(unsafe { &mut (*object.cast::<list::PyListSubclassInstance>()).storage });
	}
	None
}

fn list_resize(storage: &mut list::PyListStorage, new_cap: usize) -> Result<(), String> {
	if new_cap == storage.cap {
		return Ok(());
	}
	if new_cap < storage.len {
		return Err("cannot shrink list backing below length".to_owned());
	}
	let new_items = allocate_slots(new_cap)?;
	for index in 0..storage.len {
		let value = unsafe { *storage.items.add(index) };
		unsafe { crate::sync::store_heap_pointer(new_items.add(index), value) };
	}
	free_slots(storage.items, storage.cap);
	storage.items = new_items;
	storage.cap = new_cap;
	Ok(())
}

fn allocate_slots(cap: usize) -> Result<*mut *mut PyObject, String> {
	if cap == 0 {
		return Ok(ptr::null_mut());
	}
	let mut values = Vec::new();
	values
		.try_reserve_exact(cap)
		.map_err(|_| "container backing allocation failed".to_owned())?;
	values.resize(cap, ptr::null_mut());
	let items = values.as_mut_ptr();
	core::mem::forget(values);
	Ok(items)
}

fn free_slots(items: *mut *mut PyObject, cap: usize) {
	if !items.is_null() && cap != 0 {
		unsafe { drop(Vec::from_raw_parts(items, cap, cap)) };
	}
}

fn normalize_nonnegative_size(size: isize, what: &str) -> Result<usize, *mut PyObject> {
	if size < 0 {
		return Err(type_error(&format!("{what} size must be non-negative")));
	}
	usize::try_from(size).map_err(|_| type_error(&format!("{what} size is too large")))
}

fn checked_len(len: usize) -> isize {
	isize::try_from(len).unwrap_or_else(|_| {
		abi::return_minus_one_with_error("container size exceeds Py_ssize_t");
		-1
	})
}

fn checked_index(index: isize, len: usize, kind: &str) -> Result<usize, *mut PyObject> {
	if index < 0 {
		return Err(index_error(&format!("{kind} index out of range")));
	}
	let index =
		usize::try_from(index).map_err(|_| index_error(&format!("{kind} index out of range")))?;
	if index >= len {
		return Err(index_error(&format!("{kind} index out of range")));
	}
	Ok(index)
}

fn type_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

fn value_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::ValueError, message)
}

fn index_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::IndexError, message)
}

fn status_type_error(message: &str) -> c_int {
	let _ = type_error(message);
	-1
}

fn status_value_error(message: &str) -> c_int {
	let _ = value_error(message);
	-1
}

fn status_error(message: impl Into<String>) -> c_int {
	abi::return_minus_one_with_error(message)
}

unsafe extern "C" fn capi_tuple_new(size: isize) -> *mut PyObject {
	catch_object(|| {
		let size = match normalize_nonnegative_size(size, "tuple") {
			Ok(size) => size,
			Err(error) => return error,
		};
		let none = unsafe { abi::pon_none() };
		if none.is_null() {
			return ptr::null_mut();
		}
		let mut values = vec![none; size];
		unsafe { abi::seq::pon_build_tuple(values.as_mut_ptr(), values.len()) }
	})
}

unsafe extern "C" fn capi_tuple_size(tuple: *mut PyObject) -> isize {
	catch_size(|| {
		let tuple = crate::tag::untag_arg(tuple);
		let Some(items) = tuple_slice(tuple) else {
			let _ = type_error("expected tuple object");
			return -1;
		};
		checked_len(items.len())
	})
}

unsafe extern "C" fn capi_tuple_get_item(tuple: *mut PyObject, index: isize) -> *mut PyObject {
	catch_borrowed_object(|| {
		let tuple = crate::tag::untag_arg(tuple);
		let Some(items) = tuple_slice(tuple) else {
			return type_error("expected tuple object");
		};
		let index = match checked_index(index, items.len(), "tuple") {
			Ok(index) => index,
			Err(error) => return error,
		};
		items[index]
	})
}

unsafe extern "C" fn capi_tuple_set_item(
	tuple: *mut PyObject,
	index: isize,
	item: *mut PyObject,
) -> c_int {
	catch_status(|| {
		let tuple = crate::tag::untag_arg(tuple);
		let original_item = item;
		let item = crate::tag::untag_arg(item);
		if item.is_null() {
			return status_type_error("tuple item must not be NULL");
		}
		let Some(tuple_object) = exact_tuple_mut(tuple) else {
			super::unpin_object(original_item);
			return status_type_error("expected exact tuple object");
		};
		let index = match checked_index(index, tuple_object.len, "tuple") {
			Ok(index) => index,
			Err(_) => {
				super::unpin_object(original_item);
				return -1;
			},
		};
		let _guard = crate::sync::begin_critical_section(tuple);
		unsafe { crate::sync::store_heap_pointer(tuple_object.items.add(index), item) };
		super::unpin_object(original_item);
		0
	})
}

unsafe extern "C" fn capi_tuple_pack(items: *mut *mut PyObject, size: isize) -> *mut PyObject {
	catch_object(|| {
		let size = match normalize_nonnegative_size(size, "tuple") {
			Ok(size) => size,
			Err(error) => return error,
		};
		if items.is_null() && size != 0 {
			return abi::return_null_with_error("PyTuple_Pack received NULL items");
		}
		let mut values = Vec::with_capacity(size);
		for index in 0..size {
			let value = crate::tag::untag_arg(unsafe { *items.add(index) });
			if value.is_null() {
				return type_error("tuple item must not be NULL");
			}
			values.push(value);
		}
		unsafe { abi::seq::pon_build_tuple(values.as_mut_ptr(), values.len()) }
	})
}

unsafe extern "C" fn capi_tuple_get_slice(
	tuple: *mut PyObject,
	start: isize,
	stop: isize,
) -> *mut PyObject {
	catch_object(|| {
		let tuple = crate::tag::untag_arg(tuple);
		let Some(items) = tuple_slice(tuple) else {
			return type_error("expected tuple object");
		};
		let len = items.len() as isize;
		let start = start.clamp(0, len) as usize;
		let stop = stop.clamp(0, len) as usize;
		let (start, stop) = if stop < start {
			(start, start)
		} else {
			(start, stop)
		};
		let mut values = items[start..stop].to_vec();
		unsafe { abi::seq::pon_build_tuple(values.as_mut_ptr(), values.len()) }
	})
}

unsafe extern "C" fn capi_list_new(size: isize) -> *mut PyObject {
	catch_object(|| {
		let size = match normalize_nonnegative_size(size, "list") {
			Ok(size) => size,
			Err(error) => return error,
		};
		let none = unsafe { abi::pon_none() };
		if none.is_null() {
			return ptr::null_mut();
		}
		let mut values = vec![none; size];
		unsafe { abi::seq::pon_build_list(values.as_mut_ptr(), values.len()) }
	})
}

unsafe extern "C" fn capi_list_size(list: *mut PyObject) -> isize {
	catch_size(|| {
		let list = crate::tag::untag_arg(list);
		let Some(storage) = list_storage(list) else {
			let _ = type_error("expected list object");
			return -1;
		};
		checked_len(storage.len)
	})
}

unsafe extern "C" fn capi_list_get_item(list: *mut PyObject, index: isize) -> *mut PyObject {
	catch_borrowed_object(|| {
		let list = crate::tag::untag_arg(list);
		let Some(storage) = list_storage(list) else {
			return type_error("expected list object");
		};
		let index = match checked_index(index, storage.len, "list") {
			Ok(index) => index,
			Err(error) => return error,
		};
		unsafe { *storage.items.add(index) }
	})
}

unsafe extern "C" fn capi_list_get_item_ref(list: *mut PyObject, index: isize) -> *mut PyObject {
	catch_object(|| {
		let list = crate::tag::untag_arg(list);
		let Some(storage) = list_storage(list) else {
			return type_error("expected list object");
		};
		let index = match checked_index(index, storage.len, "list") {
			Ok(index) => index,
			Err(error) => return error,
		};
		unsafe { *storage.items.add(index) }
	})
}
unsafe extern "C" fn capi_list_set_item(
	list: *mut PyObject,
	index: isize,
	item: *mut PyObject,
) -> c_int {
	catch_status(|| {
		let list = crate::tag::untag_arg(list);
		let original_item = item;
		let item = crate::tag::untag_arg(normalize_object_arg(item));
		if item.is_null() {
			return status_type_error("list item must not be NULL");
		}
		let Some(storage) = list_storage_mut(list) else {
			super::unpin_object(original_item);
			return status_type_error("expected list object");
		};
		let index = match checked_index(index, storage.len, "list") {
			Ok(index) => index,
			Err(_) => {
				super::unpin_object(original_item);
				return -1;
			},
		};
		let _guard = crate::sync::begin_critical_section(list);
		unsafe { crate::sync::store_heap_pointer(storage.items.add(index), item) };
		super::unpin_object(original_item);
		0
	})
}

unsafe extern "C" fn capi_list_append(list: *mut PyObject, item: *mut PyObject) -> c_int {
	catch_status(|| {
		let list = crate::tag::untag_arg(list);
		let item = crate::tag::untag_arg(normalize_object_arg(item));
		if item.is_null() {
			return status_type_error("list item must not be NULL");
		}
		let result = unsafe { abi::seq::pon_list_append(list, item) };
		if result.is_null() { -1 } else { 0 }
	})
}

unsafe extern "C" fn capi_list_insert(
	list: *mut PyObject,
	index: isize,
	item: *mut PyObject,
) -> c_int {
	catch_status(|| {
		let list = crate::tag::untag_arg(list);
		let item = crate::tag::untag_arg(normalize_object_arg(item));
		if item.is_null() {
			return status_type_error("list item must not be NULL");
		}
		let Some(storage) = list_storage_mut(list) else {
			return status_type_error("expected list object");
		};
		let _guard = crate::sync::begin_critical_section(list);
		if storage.len == storage.cap {
			let new_cap = if storage.cap == 0 {
				4
			} else {
				storage.cap.saturating_mul(2)
			};
			if new_cap <= storage.cap {
				return status_error("list is too large");
			}
			if let Err(message) = list_resize(storage, new_cap) {
				return status_error(message);
			}
		}
		let len = storage.len as isize;
		let index = index.clamp(0, len) as usize;
		unsafe {
			for pos in (index..storage.len).rev() {
				let shifted = *storage.items.add(pos);
				crate::sync::store_heap_pointer(storage.items.add(pos + 1), shifted);
			}
			crate::sync::store_heap_pointer(storage.items.add(index), item);
		}
		storage.len += 1;
		0
	})
}

unsafe extern "C" fn capi_list_set_slice(
	list: *mut PyObject,
	low: isize,
	high: isize,
	items: *mut PyObject,
) -> c_int {
	catch_status(|| {
		let list = crate::tag::untag_arg(list);
		if list_storage(list).is_none() {
			return status_type_error("expected list object");
		}
		let start = unsafe { abi::pon_const_int(low as i64) };
		if start.is_null() {
			return -1;
		}
		let stop = unsafe { abi::pon_const_int(high as i64) };
		if stop.is_null() {
			return -1;
		}
		let slice = unsafe { abi::seq::pon_build_slice(start, stop, abi::pon_none()) };
		if slice.is_null() {
			return -1;
		}
		let items = if items.is_null() {
			ptr::null_mut()
		} else {
			crate::tag::untag_arg(normalize_object_arg(items))
		};
		unsafe { abi::seq::pon_seq_set_item(list, slice, items) }
	})
}

unsafe extern "C" fn capi_list_as_tuple(list: *mut PyObject) -> *mut PyObject {
	catch_object(|| {
		let list = crate::tag::untag_arg(list);
		let Some(storage) = list_storage(list) else {
			return type_error("expected list object");
		};
		let mut values = unsafe { storage.as_slice() }.to_vec();
		unsafe { abi::seq::pon_build_tuple(values.as_mut_ptr(), values.len()) }
	})
}

unsafe extern "C" fn capi_list_sort(list: *mut PyObject) -> c_int {
	catch_status(|| {
		let list = crate::tag::untag_arg(list);
		let method = unsafe { abi::pon_get_attr(list, intern("sort"), ptr::null_mut()) };
		if method.is_null() {
			return -1;
		}
		let result = unsafe { abi::pon_call(method, ptr::null_mut(), 0) };
		if result.is_null() { -1 } else { 0 }
	})
}

unsafe extern "C" fn capi_dict_new() -> *mut PyObject {
	catch_object(|| unsafe { abi::map::pon_build_map(ptr::null_mut(), 0) })
}

unsafe extern "C" fn capi_dict_set_item(
	dict: *mut PyObject,
	key: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	catch_status(|| {
		let dict = crate::tag::untag_arg(dict);
		let key = crate::tag::untag_arg(normalize_object_arg(key));
		let value = crate::tag::untag_arg(normalize_object_arg(value));
		dict_set_item_capi(dict, key, value)
	})
}

unsafe extern "C" fn capi_dict_set_item_string(
	dict: *mut PyObject,
	key: *const c_char,
	value: *mut PyObject,
) -> c_int {
	catch_status(|| {
		let Some(key) = string_key(key) else {
			return -1;
		};
		unsafe { capi_dict_set_item(dict, key, value) }
	})
}

unsafe extern "C" fn capi_dict_get_item(dict: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
	catch_borrowed_object(|| dict_get_impl(dict, key, true))
}

unsafe extern "C" fn capi_dict_get_item_string(
	dict: *mut PyObject,
	key: *const c_char,
) -> *mut PyObject {
	catch_borrowed_object(|| {
		let Some(key) = string_key(key) else {
			return ptr::null_mut();
		};
		dict_get_impl(dict, key, true)
	})
}

unsafe extern "C" fn capi_dict_get_item_with_error(
	dict: *mut PyObject,
	key: *mut PyObject,
) -> *mut PyObject {
	catch_borrowed_object(|| dict_get_impl(dict, key, false))
}

unsafe extern "C" fn capi_dict_get_item_ref(
	dict: *mut PyObject,
	key: *mut PyObject,
	result: *mut *mut PyObject,
) -> c_int {
	catch_status(|| {
		if result.is_null() {
			return status_type_error("PyDict_GetItemRef result pointer must not be NULL");
		}
		unsafe {
			*result = ptr::null_mut();
		}
		match dict_get_result(dict, key) {
			Ok(Some(value)) => {
				super::pin_object(value);
				unsafe {
					*result = super::foreignize_type_result(value);
				}
				1
			},
			Ok(None) => {
				if pon_err_occurred() {
					pon_err_clear();
				}
				0
			},
			Err(message) => status_error(message),
		}
	})
}

fn dict_get_impl(dict: *mut PyObject, key: *mut PyObject, clear_miss: bool) -> *mut PyObject {
	match dict_get_result(dict, key) {
		Ok(Some(value)) => value,
		Ok(None) => {
			if clear_miss || pon_err_occurred() {
				pon_err_clear();
			}
			ptr::null_mut()
		},
		Err(message) => abi::return_null_with_error(message),
	}
}

fn dict_get_result(
	dict: *mut PyObject,
	key: *mut PyObject,
) -> Result<Option<*mut PyObject>, String> {
	let dict = crate::tag::untag_arg(dict);
	// Foreign type faces must never reach pon hashing raw: read paths
	// normalize exactly like `dict_set_item_capi` so lookups agree with
	// normalized inserts.
	let key = crate::tag::untag_arg(normalize_object_arg(key));
	if unsafe { type_::is_class_dict_view(dict) } {
		unsafe { type_::class_dict_view_get_item(dict, key) }
	} else {
		unsafe { dict::dict_get(dict, key) }
	}
}

fn dict_set_item_capi(dict: *mut PyObject, key: *mut PyObject, value: *mut PyObject) -> c_int {
	let key = crate::tag::untag_arg(normalize_object_arg(key));
	let value = crate::tag::untag_arg(normalize_object_arg(value));
	if unsafe { type_::is_class_dict_view(dict) } {
		match unsafe { type_::class_dict_view_set_item(dict, key, value) } {
			Ok(()) => 0,
			Err(message) => status_error(message),
		}
	} else {
		unsafe { abi::map::pon_dict_set_item_status(dict, key, value) }
	}
}

fn dict_remove_capi(dict: *mut PyObject, key: *mut PyObject) -> Result<bool, String> {
	if unsafe { type_::is_class_dict_view(dict) } {
		unsafe { type_::class_dict_view_del_item(dict, key) }
	} else {
		unsafe { dict::dict_remove(dict, key).map(|removed| removed.is_some()) }
	}
}

fn dict_contains_capi(dict: *mut PyObject, key: *mut PyObject) -> Result<bool, String> {
	if unsafe { type_::is_class_dict_view(dict) } {
		unsafe { type_::class_dict_view_contains(dict, key) }
	} else {
		unsafe { dict::dict_contains(dict, key) }
	}
}

fn dict_entries_snapshot_capi(dict: *mut PyObject) -> Result<Vec<DictEntry>, String> {
	if unsafe { type_::is_class_dict_view(dict) } {
		unsafe { type_::class_dict_view_entries_snapshot(dict) }
	} else {
		unsafe { dict::dict_entries_snapshot(dict) }
	}
}

unsafe extern "C" fn capi_dict_del_item(dict: *mut PyObject, key: *mut PyObject) -> c_int {
	catch_status(|| {
		let dict = crate::tag::untag_arg(dict);
		let key = crate::tag::untag_arg(normalize_object_arg(key));
		match dict_remove_capi(dict, key) {
			Ok(true) => 0,
			Ok(false) => {
				let _ = unsafe { abi::exc::pon_raise_key_error(key) };
				-1
			},
			Err(message) => status_error(message),
		}
	})
}

unsafe extern "C" fn capi_dict_contains(dict: *mut PyObject, key: *mut PyObject) -> c_int {
	catch_status(|| {
		let dict = crate::tag::untag_arg(dict);
		let key = crate::tag::untag_arg(normalize_object_arg(key));
		match dict_contains_capi(dict, key) {
			Ok(true) => 1,
			Ok(false) => 0,
			Err(message) => status_error(message),
		}
	})
}

unsafe extern "C" fn capi_dict_size(dict: *mut PyObject) -> isize {
	catch_size(|| {
		let dict = crate::tag::untag_arg(dict);
		if unsafe { type_::is_class_dict_view(dict) } {
			return match unsafe { type_::class_dict_view_len(dict) } {
				Ok(len) => checked_len(len),
				Err(message) => {
					let _ = type_error(&message);
					-1
				},
			};
		}
		match unsafe { dict::dict_ref(dict) } {
			Ok(storage) => checked_len(storage.entries.len()),
			Err(message) => {
				let _ = type_error(&message);
				-1
			},
		}
	})
}

unsafe extern "C" fn capi_dict_keys(dict: *mut PyObject) -> *mut PyObject {
	catch_object(|| dict_projection_list(dict, DictProjection::Keys))
}

unsafe extern "C" fn capi_dict_values(dict: *mut PyObject) -> *mut PyObject {
	catch_object(|| dict_projection_list(dict, DictProjection::Values))
}

unsafe extern "C" fn capi_dict_items(dict: *mut PyObject) -> *mut PyObject {
	catch_object(|| dict_projection_list(dict, DictProjection::Items))
}

#[derive(Clone, Copy)]
enum DictProjection {
	Keys,
	Values,
	Items,
}

fn dict_projection_list(dict: *mut PyObject, projection: DictProjection) -> *mut PyObject {
	let dict = crate::tag::untag_arg(dict);
	let entries = match dict_entries_snapshot_capi(dict) {
		Ok(entries) => entries,
		Err(message) => return abi::return_null_with_error(message),
	};
	let mut values = Vec::with_capacity(entries.len());
	for entry in entries {
		match projection {
			DictProjection::Keys => values.push(entry.key),
			DictProjection::Values => values.push(entry.value),
			DictProjection::Items => {
				let mut pair = [entry.key, entry.value];
				let tuple = unsafe { abi::seq::pon_build_tuple(pair.as_mut_ptr(), pair.len()) };
				if tuple.is_null() {
					return ptr::null_mut();
				}
				values.push(tuple);
			},
		}
	}
	unsafe { abi::seq::pon_build_list(values.as_mut_ptr(), values.len()) }
}

unsafe extern "C" fn capi_dict_next(
	dict: *mut PyObject,
	pos: *mut isize,
	key_out: *mut *mut PyObject,
	value_out: *mut *mut PyObject,
) -> c_int {
	catch_status(|| {
		let dict = crate::tag::untag_arg(dict);
		if pos.is_null() || dict.is_null() {
			return 0;
		}
		let cursor = unsafe { *pos };
		if cursor < 0 {
			return 0;
		}
		let snapshot_key = (dict as usize, pos as usize);
		let mut snapshots = DICT_NEXT_SNAPSHOTS
			.lock()
			.expect("dict next snapshot lock poisoned");
		if cursor == 0 {
			let entries = match dict_entries_snapshot_capi(dict) {
				Ok(entries) => entries,
				Err(_) => return 0,
			};
			snapshots.insert(
				snapshot_key,
				entries
					.into_iter()
					.map(|entry| (entry.key as usize, entry.value as usize))
					.collect(),
			);
		}
		let Some(snapshot) = snapshots.get(&snapshot_key) else {
			return 0;
		};
		let index = cursor as usize;
		if index >= snapshot.len() {
			snapshots.remove(&snapshot_key);
			return 0;
		}
		let (key, value) = snapshot[index];
		let should_clear = index + 1 >= snapshot.len();
		if !key_out.is_null() {
			unsafe { *key_out = key as *mut PyObject };
		}
		if !value_out.is_null() {
			unsafe { *value_out = value as *mut PyObject };
		}
		unsafe { *pos = cursor + 1 };
		if should_clear {
			snapshots.remove(&snapshot_key);
		}
		1
	})
}

unsafe extern "C" fn capi_dict_merge(
	dict: *mut PyObject,
	other: *mut PyObject,
	override_flag: c_int,
) -> c_int {
	catch_status(|| {
		let dict = crate::tag::untag_arg(dict);
		let other = crate::tag::untag_arg(other);
		if unsafe { dict::has_dict_storage(other) } || unsafe { type_::is_class_dict_view(other) } {
			return merge_entries(dict, dict_entries_snapshot_capi(other), override_flag != 0);
		}
		let keys = unsafe { capi_mapping_keys(other) };
		if keys.is_null() {
			return -1;
		}
		let keys = match abi::seq::sequence_to_vec(keys) {
			Ok(keys) => keys,
			Err(message) => return status_error(message),
		};
		for key in keys {
			let stored_key = crate::tag::untag_arg(normalize_object_arg(key));
			if override_flag == 0 && dict_contains_capi(dict, stored_key).unwrap_or(false) {
				continue;
			}
			let value = unsafe { abi::pon_subscript_get(other, key, ptr::null_mut()) };
			if value.is_null() {
				return -1;
			}
			let value = crate::tag::untag_arg(normalize_object_arg(value));
			if dict_set_item_capi(dict, stored_key, value) < 0 {
				return -1;
			}
		}
		0
	})
}

fn merge_entries(
	dict: *mut PyObject,
	entries: Result<Vec<DictEntry>, String>,
	override_existing: bool,
) -> c_int {
	let entries = match entries {
		Ok(entries) => entries,
		Err(message) => return status_error(message),
	};
	for entry in entries {
		let key = crate::tag::untag_arg(normalize_object_arg(entry.key));
		let value = crate::tag::untag_arg(normalize_object_arg(entry.value));
		if !override_existing {
			match dict_contains_capi(dict, key) {
				Ok(true) => continue,
				Ok(false) => {},
				Err(message) => return status_error(message),
			}
		}
		if dict_set_item_capi(dict, key, value) < 0 {
			return -1;
		}
	}
	0
}

unsafe extern "C" fn capi_dict_update(dict: *mut PyObject, other: *mut PyObject) -> c_int {
	catch_status(|| {
		let dict = crate::tag::untag_arg(dict);
		let other = crate::tag::untag_arg(other);
		unsafe { capi_dict_merge(dict, other, 1) }
	})
}

unsafe extern "C" fn capi_dict_copy(dict: *mut PyObject) -> *mut PyObject {
	catch_object(|| {
		let dict = crate::tag::untag_arg(dict);
		let entries = match dict_entries_snapshot_capi(dict) {
			Ok(entries) => entries,
			Err(message) => return abi::return_null_with_error(message),
		};
		let mut pairs = Vec::with_capacity(entries.len().saturating_mul(2));
		for entry in entries {
			pairs.push(entry.key);
			pairs.push(entry.value);
		}
		unsafe { abi::map::pon_build_map(pairs.as_mut_ptr(), pairs.len() / 2) }
	})
}

unsafe extern "C" fn capi_dict_clear(dict: *mut PyObject) {
	let _ = catch_unwind(AssertUnwindSafe(|| {
		let dict = crate::tag::untag_arg(dict);
		if unsafe { type_::is_class_dict_view(dict) } {
			if let Err(message) = unsafe { type_::class_dict_view_clear(dict) } {
				let _ = type_error(&message);
			}
			return;
		}
		if let Err(message) = unsafe { dict::dict_clear(dict) } {
			let _ = type_error(&message);
		}
	}));
}

unsafe extern "C" fn capi_dict_del_item_string(dict: *mut PyObject, key: *const c_char) -> c_int {
	catch_status(|| {
		let Some(key) = string_key(key) else {
			return -1;
		};
		unsafe { capi_dict_del_item(dict, key) }
	})
}

unsafe extern "C" fn capi_dict_contains_string(dict: *mut PyObject, key: *const c_char) -> c_int {
	catch_status(|| {
		let Some(key) = string_key(key) else {
			return -1;
		};
		unsafe { capi_dict_contains(dict, key) }
	})
}

unsafe extern "C" fn capi_dict_set_default_ref(
	dict: *mut PyObject,
	key: *mut PyObject,
	default_value: *mut PyObject,
	result: *mut *mut PyObject,
) -> c_int {
	catch_status(|| {
		if !result.is_null() {
			unsafe { *result = ptr::null_mut() };
		}
		if default_value.is_null() {
			return status_type_error("PyDict_SetDefaultRef default value must not be NULL");
		}
		let key = crate::tag::untag_arg(normalize_object_arg(key));
		match dict_get_result(dict, key) {
			Ok(Some(value)) => {
				if !result.is_null() {
					super::pin_object(value);
					unsafe { *result = super::foreignize_type_result(value) };
				}
				1
			},
			Ok(None) => {
				if pon_err_occurred() {
					pon_err_clear();
				}
				let dict = crate::tag::untag_arg(dict);
				let default_value = crate::tag::untag_arg(normalize_object_arg(default_value));
				let status = dict_set_item_capi(dict, key, default_value);
				if status < 0 {
					return -1;
				}
				if !result.is_null() {
					super::pin_object(default_value);
					unsafe { *result = super::foreignize_type_result(default_value) };
				}
				0
			},
			Err(message) => status_error(message),
		}
	})
}

unsafe extern "C" fn capi_dict_set_default(
	dict: *mut PyObject,
	key: *mut PyObject,
	default_value: *mut PyObject,
) -> *mut PyObject {
	catch_borrowed_object(|| {
		if default_value.is_null() {
			return type_error("PyDict_SetDefault default value must not be NULL");
		}
		let key = crate::tag::untag_arg(normalize_object_arg(key));
		match dict_get_result(dict, key) {
			Ok(Some(value)) => value,
			Ok(None) => {
				if pon_err_occurred() {
					pon_err_clear();
				}
				let dict = crate::tag::untag_arg(dict);
				let default_value = crate::tag::untag_arg(normalize_object_arg(default_value));
				let status = dict_set_item_capi(dict, key, default_value);
				if status < 0 {
					return ptr::null_mut();
				}
				default_value
			},
			Err(message) => abi::return_null_with_error(message),
		}
	})
}

unsafe extern "C" fn capi_dict_get_item_string_ref(
	dict: *mut PyObject,
	key: *const c_char,
	result: *mut *mut PyObject,
) -> c_int {
	catch_status(|| {
		let Some(key) = string_key(key) else {
			if !result.is_null() {
				unsafe { *result = ptr::null_mut() };
			}
			return -1;
		};
		unsafe { capi_dict_get_item_ref(dict, key, result) }
	})
}

fn dict_proxy_type() -> *mut PyType {
	*DICT_PROXY_TYPE as *mut PyType
}

unsafe fn dict_proxy_ref<'a>(object: *mut PyObject) -> Option<&'a PyDictProxy> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || !crate::tag::is_heap(object) {
		return None;
	}
	if unsafe { (*object).ob_type } != dict_proxy_type().cast_const() {
		return None;
	}
	Some(unsafe { &*object.cast::<PyDictProxy>() })
}

unsafe extern "C" fn trace_dict_proxy(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
	if object.is_null() {
		return;
	}
	let mapping = unsafe { (*object.cast::<PyDictProxy>()).mapping };
	if !mapping.is_null() && crate::tag::is_heap(mapping) {
		visitor(mapping.cast::<u8>());
	}
}

unsafe extern "C" fn dict_proxy_len_slot(object: *mut PyObject) -> isize {
	let Some(proxy) = (unsafe { dict_proxy_ref(object) }) else {
		let _ = type_error("mappingproxy receiver is invalid");
		return -1;
	};
	let mapping = crate::tag::untag_arg(proxy.mapping);
	if mapping.is_null() || !crate::tag::is_heap(mapping) {
		let _ = type_error("mappingproxy wrapped mapping is invalid");
		return -1;
	}
	if unsafe { dict::has_dict_storage(mapping) } || unsafe { type_::is_class_dict_view(mapping) } {
		return unsafe { capi_dict_size(mapping) };
	}
	let ty = unsafe { (*mapping).ob_type };
	if let Some(slot) = unsafe {
		ty.as_ref()
			.and_then(|ty| ty.tp_as_mapping.as_ref())
			.and_then(|methods| methods.mp_length)
	} {
		return unsafe { slot(mapping) };
	}
	let _ = type_error("mappingproxy wrapped object has no mapping length");
	-1
}

unsafe extern "C" fn dict_proxy_subscript_slot(
	object: *mut PyObject,
	key: *mut PyObject,
) -> *mut PyObject {
	let Some(proxy) = (unsafe { dict_proxy_ref(object) }) else {
		return type_error("mappingproxy receiver is invalid");
	};
	unsafe { abi::pon_subscript_get(proxy.mapping, key, ptr::null_mut()) }
}

unsafe extern "C" fn dict_proxy_ass_subscript_slot(
	_object: *mut PyObject,
	_key: *mut PyObject,
	_value: *mut PyObject,
) -> c_int {
	status_type_error("'mappingproxy' object does not support item assignment")
}

unsafe extern "C" fn capi_dict_proxy_new(mapping: *mut PyObject) -> *mut PyObject {
	catch_object(|| {
		let mapping = crate::tag::untag_arg(normalize_object_arg(mapping));
		if mapping.is_null() {
			return type_error("PyDictProxy_New received NULL mapping");
		}
		let info = GcTypeInfo {
			size:     mem::size_of::<PyDictProxy>(),
			trace:    trace_dict_proxy,
			finalize: None,
		};
		let object = match abi::alloc_gc_object(TYPE_ID_DICT_PROXY, info) {
			Ok(object) => object.cast::<PyDictProxy>(),
			Err(message) => return abi::return_null_with_error(message),
		};
		unsafe {
			ptr::write(object, PyDictProxy {
				ob_base: PyObjectHeader::new(dict_proxy_type()),
				mapping,
			});
		}
		object.cast::<PyObject>()
	})
}

unsafe extern "C" fn capi_dict_proxy_type() -> *mut ForeignTypeObject {
	super::twin::foreign_of_native(dict_proxy_type())
}

unsafe extern "C" fn capi_set_new(iterable: *mut PyObject) -> *mut PyObject {
	catch_object(|| {
		let iterable = crate::tag::untag_arg(iterable);
		if iterable.is_null() {
			return unsafe { abi::map::pon_build_set(ptr::null_mut(), 0) };
		}
		let mut values = match abi::seq::sequence_to_vec(iterable) {
			Ok(values) => values,
			Err(message) => return abi::return_null_with_error(message),
		};
		for value in &mut values {
			*value = crate::tag::untag_arg(normalize_object_arg(*value));
		}
		unsafe { abi::map::pon_build_set(values.as_mut_ptr(), values.len()) }
	})
}

unsafe extern "C" fn capi_set_add(set: *mut PyObject, item: *mut PyObject) -> c_int {
	catch_status(|| {
		let set = crate::tag::untag_arg(set);
		let item = crate::tag::untag_arg(normalize_object_arg(item));
		let result = unsafe { abi::map::pon_set_add(set, item) };
		if result.is_null() { -1 } else { 0 }
	})
}

unsafe extern "C" fn capi_set_contains(set: *mut PyObject, item: *mut PyObject) -> c_int {
	catch_status(|| {
		let set = crate::tag::untag_arg(set);
		let item = crate::tag::untag_arg(item);
		unsafe { abi::map::pon_contains(set, item) }
	})
}

unsafe extern "C" fn capi_set_size(set: *mut PyObject) -> isize {
	catch_size(|| {
		let set = crate::tag::untag_arg(set);
		if unsafe { set_::is_set(set) } {
			return checked_len(
				unsafe { set_::set_ref(set) }
					.map(|set| set.entries.len())
					.unwrap_or(0),
			);
		}
		if unsafe { frozenset::is_frozenset(set) } {
			return checked_len(
				unsafe { frozenset::entries_snapshot(set) }
					.map(|entries| entries.len())
					.unwrap_or(0),
			);
		}
		let _ = type_error("expected set object");
		-1
	})
}

unsafe extern "C" fn capi_slice_new(
	start: *mut PyObject,
	stop: *mut PyObject,
	step: *mut PyObject,
) -> *mut PyObject {
	catch_object(|| {
		let start = crate::tag::untag_arg(normalize_object_arg(start));
		let stop = crate::tag::untag_arg(normalize_object_arg(stop));
		let step = crate::tag::untag_arg(normalize_object_arg(step));
		unsafe { abi::seq::pon_build_slice(start, stop, step) }
	})
}

unsafe extern "C" fn capi_slice_unpack(
	slice: *mut PyObject,
	start: *mut isize,
	stop: *mut isize,
	step: *mut isize,
) -> c_int {
	catch_status(|| {
		let slice = crate::tag::untag_arg(slice);
		if start.is_null() || stop.is_null() || step.is_null() {
			return status_type_error("PySlice_Unpack received NULL output pointer");
		}
		if !abi::seq::is_slice(slice) {
			return status_type_error("expected slice object");
		}
		let slice = unsafe { &*slice.cast::<crate::types::slice_::PySlice>() };
		let mut step_value = if is_none(slice.step) {
			1
		} else {
			match slice_index_saturating(slice.step) {
				Ok(value) => value,
				Err(error) => return error,
			}
		};
		if step_value == 0 {
			return status_value_error("slice step cannot be zero");
		}
		if step_value < -isize::MAX {
			step_value = -isize::MAX;
		}
		let start_value = if is_none(slice.start) {
			if step_value < 0 { isize::MAX } else { 0 }
		} else {
			match slice_index_saturating(slice.start) {
				Ok(value) => value,
				Err(error) => return error,
			}
		};
		let stop_value = if is_none(slice.stop) {
			if step_value < 0 {
				isize::MIN
			} else {
				isize::MAX
			}
		} else {
			match slice_index_saturating(slice.stop) {
				Ok(value) => value,
				Err(error) => return error,
			}
		};
		unsafe {
			*start = start_value;
			*stop = stop_value;
			*step = step_value;
		}
		0
	})
}

unsafe extern "C" fn capi_slice_adjust_indices(
	length: isize,
	start: *mut isize,
	stop: *mut isize,
	step: isize,
) -> isize {
	catch_size(|| {
		if start.is_null() || stop.is_null() || step == 0 || length < 0 {
			return 0;
		}
		let mut start_value = unsafe { *start };
		let mut stop_value = unsafe { *stop };
		if start_value < 0 {
			start_value = start_value.saturating_add(length);
			if start_value < 0 {
				start_value = if step < 0 { -1 } else { 0 };
			}
		} else if start_value >= length {
			start_value = if step < 0 { length - 1 } else { length };
		}
		if stop_value < 0 {
			stop_value = stop_value.saturating_add(length);
			if stop_value < 0 {
				stop_value = if step < 0 { -1 } else { 0 };
			}
		} else if stop_value >= length {
			stop_value = if step < 0 { length - 1 } else { length };
		}
		unsafe {
			*start = start_value;
			*stop = stop_value;
		}
		if step < 0 {
			if stop_value < start_value {
				(start_value - stop_value - 1) / (-step) + 1
			} else {
				0
			}
		} else if start_value < stop_value {
			(stop_value - start_value - 1) / step + 1
		} else {
			0
		}
	})
}

unsafe extern "C" fn capi_sequence_check(object: *mut PyObject) -> c_int {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || !crate::tag::is_heap(object) {
		return 0;
	}
	if list_storage(object).is_some() || tuple_slice(object).is_some() {
		return 1;
	}
	unsafe {
		let ty = (*object).ob_type;
		if ty.is_null() {
			return 0;
		}
		let methods = (*ty).tp_as_sequence.as_ref();
		if methods.is_some_and(|methods| methods.sq_length.is_some() || methods.sq_item.is_some()) {
			1
		} else {
			0
		}
	}
}

unsafe extern "C" fn capi_sequence_size(object: *mut PyObject) -> isize {
	catch_size(|| {
		let object = crate::tag::untag_arg(object);
		unsafe { abi::seq::pon_seq_len(object) }
	})
}

unsafe extern "C" fn capi_sequence_get_item(object: *mut PyObject, index: isize) -> *mut PyObject {
	catch_object(|| {
		let object = crate::tag::untag_arg(object);
		let index = unsafe { abi::pon_const_int(index as i64) };
		if index.is_null() {
			return ptr::null_mut();
		}
		unsafe { abi::seq::pon_seq_get_item(object, index) }
	})
}

unsafe extern "C" fn capi_sequence_set_item(
	object: *mut PyObject,
	index: isize,
	value: *mut PyObject,
) -> c_int {
	catch_status(|| {
		let object = crate::tag::untag_arg(object);
		let value = crate::tag::untag_arg(normalize_object_arg(value));
		let index = unsafe { abi::pon_const_int(index as i64) };
		if index.is_null() {
			return -1;
		}
		unsafe { abi::seq::pon_seq_set_item(object, index, value) }
	})
}

unsafe extern "C" fn capi_sequence_contains(object: *mut PyObject, value: *mut PyObject) -> c_int {
	catch_status(|| {
		let object = crate::tag::untag_arg(object);
		let value = crate::tag::untag_arg(value);
		unsafe { abi::map::pon_contains(object, value) }
	})
}

unsafe extern "C" fn capi_sequence_concat(
	left: *mut PyObject,
	right: *mut PyObject,
) -> *mut PyObject {
	catch_object(|| {
		let left = crate::tag::untag_arg(left);
		let right = crate::tag::untag_arg(right);
		unsafe { crate::abstract_op::binary_op(crate::abstract_op::BINARY_ADD, left, right) }
	})
}

unsafe extern "C" fn capi_sequence_repeat(object: *mut PyObject, count: isize) -> *mut PyObject {
	catch_object(|| {
		let object = crate::tag::untag_arg(object);
		let count = sequence_repeat_count_object(count);
		if count.is_null() {
			return ptr::null_mut();
		}
		unsafe { crate::abstract_op::binary_op(crate::abstract_op::BINARY_MUL, object, count) }
	})
}

unsafe extern "C" fn capi_sequence_inplace_repeat(
	object: *mut PyObject,
	count: isize,
) -> *mut PyObject {
	catch_object(|| {
		let object = crate::tag::untag_arg(object);
		let count = sequence_repeat_count_object(count);
		if count.is_null() {
			return ptr::null_mut();
		}
		unsafe {
			abi::number::pon_number_inplace(abi::number::BINARY_MUL, object, count, ptr::null_mut())
		}
	})
}

unsafe extern "C" fn capi_sequence_inplace_concat(
	left: *mut PyObject,
	right: *mut PyObject,
) -> *mut PyObject {
	catch_object(|| {
		let left = crate::tag::untag_arg(left);
		let right = crate::tag::untag_arg(right);
		unsafe {
			abi::number::pon_number_inplace(abi::number::BINARY_ADD, left, right, ptr::null_mut())
		}
	})
}

fn sequence_repeat_count_object(count: isize) -> *mut PyObject {
	let Ok(count) = i64::try_from(count) else {
		return abi::return_null_with_error("sequence repeat count is out of range");
	};
	unsafe { abi::pon_const_int(count) }
}

unsafe extern "C" fn capi_sequence_tuple(object: *mut PyObject) -> *mut PyObject {
	catch_object(|| {
		let object = crate::tag::untag_arg(object);
		if let Some(items) = tuple_slice(object) {
			let mut values = items.to_vec();
			return unsafe { abi::seq::pon_build_tuple(values.as_mut_ptr(), values.len()) };
		}
		let mut values = match abi::seq::sequence_to_vec(object) {
			Ok(values) => values,
			Err(message) => return abi::return_null_with_error(message),
		};
		unsafe { abi::seq::pon_build_tuple(values.as_mut_ptr(), values.len()) }
	})
}

unsafe extern "C" fn capi_sequence_list(object: *mut PyObject) -> *mut PyObject {
	catch_object(|| {
		let object = crate::tag::untag_arg(object);
		let mut values = match abi::seq::sequence_to_vec(object) {
			Ok(values) => values,
			Err(message) => return abi::return_null_with_error(message),
		};
		unsafe { abi::seq::pon_build_list(values.as_mut_ptr(), values.len()) }
	})
}

unsafe extern "C" fn capi_sequence_fast(
	object: *mut PyObject,
	message: *const c_char,
) -> *mut PyObject {
	catch_object(|| {
		let object = crate::tag::untag_arg(object);
		if list_storage(object).is_some() || tuple_slice(object).is_some() {
			return object;
		}
		let mut values = match abi::seq::sequence_to_vec(object) {
			Ok(values) => values,
			Err(_) => {
				if !message.is_null() {
					if let Some(message) = c_string(message) {
						return type_error(&message);
					}
				}
				return ptr::null_mut();
			},
		};
		unsafe { abi::seq::pon_build_tuple(values.as_mut_ptr(), values.len()) }
	})
}

unsafe extern "C" fn capi_sequence_fast_items(
	object: *mut PyObject,
	len_out: *mut isize,
) -> *mut *mut PyObject {
	let object = crate::tag::untag_arg(object);
	if !len_out.is_null() {
		unsafe { *len_out = 0 };
	}
	if object.is_null() || !crate::tag::is_heap(object) {
		return ptr::null_mut();
	}
	if let Some(storage) = list_storage(object) {
		if !len_out.is_null() {
			unsafe { *len_out = checked_len(storage.len) };
		}
		return storage.items;
	}
	if unsafe { (*object).ob_type } == abi::seq::tuple_type().cast_const() {
		let tuple = unsafe { &*object.cast::<tuple::PyTuple>() };
		if !len_out.is_null() {
			unsafe { *len_out = checked_len(tuple.len) };
		}
		return tuple.items;
	}
	if unsafe { tuple::is_tuple_subclass_instance(object) } {
		let storage = unsafe { &(*object.cast::<tuple::PyTupleSubclassInstance>()).storage };
		if !len_out.is_null() {
			unsafe { *len_out = checked_len(storage.len) };
		}
		return storage.items;
	}
	ptr::null_mut()
}

unsafe extern "C" fn capi_mapping_check(object: *mut PyObject) -> c_int {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || !crate::tag::is_heap(object) {
		return 0;
	}
	if unsafe { dict::has_dict_storage(object) } {
		return 1;
	}
	unsafe {
		let ty = (*object).ob_type;
		if ty.is_null() {
			return 0;
		}
		let methods = (*ty).tp_as_mapping.as_ref();
		if methods.is_some_and(|methods| methods.mp_subscript.is_some()) {
			1
		} else {
			0
		}
	}
}

unsafe extern "C" fn capi_mapping_keys(object: *mut PyObject) -> *mut PyObject {
	catch_object(|| {
		let object = crate::tag::untag_arg(object);
		if unsafe { dict::has_dict_storage(object) } {
			return dict_projection_list(object, DictProjection::Keys);
		}
		let method = unsafe { abi::pon_get_attr(object, intern("keys"), ptr::null_mut()) };
		if method.is_null() {
			return ptr::null_mut();
		}
		let result = unsafe { abi::pon_call(method, ptr::null_mut(), 0) };
		if result.is_null() {
			return ptr::null_mut();
		}
		let mut values = match abi::seq::sequence_to_vec(result) {
			Ok(values) => values,
			Err(message) => return abi::return_null_with_error(message),
		};
		unsafe { abi::seq::pon_build_list(values.as_mut_ptr(), values.len()) }
	})
}

unsafe extern "C" fn capi_mapping_get_item_string(
	object: *mut PyObject,
	key: *const c_char,
) -> *mut PyObject {
	catch_object(|| {
		let object = crate::tag::untag_arg(object);
		let Some(key) = string_key(key) else {
			return ptr::null_mut();
		};
		unsafe { abi::pon_subscript_get(object, key, ptr::null_mut()) }
	})
}

unsafe extern "C" fn capi_mapping_set_item_string(
	object: *mut PyObject,
	key: *const c_char,
	value: *mut PyObject,
) -> c_int {
	catch_status(|| {
		let object = crate::tag::untag_arg(object);
		let value = crate::tag::untag_arg(normalize_object_arg(value));
		let Some(key) = string_key(key) else {
			return -1;
		};
		let result = unsafe { abi::map::pon_subscript_set(object, key, value) };
		if result.is_null() { -1 } else { 0 }
	})
}

fn string_key(key: *const c_char) -> Option<*mut PyObject> {
	let text = match c_string(key) {
		Some(text) => text,
		None => {
			let _ = type_error("string key must be valid UTF-8");
			return None;
		},
	};
	let key = unsafe { abi::pon_const_str(text.as_ptr(), text.len()) };
	if key.is_null() { None } else { Some(key) }
}

fn is_none(object: *mut PyObject) -> bool {
	object == unsafe { abi::pon_none() }
}

fn slice_index_saturating(object: *mut PyObject) -> Result<isize, c_int> {
	let object = crate::tag::untag_arg(object);
	let Some(value) = (unsafe { crate::types::int::to_bigint_including_bool(object) }) else {
		return Err(status_type_error(
			"slice indices must be integers or None or have an __index__ method",
		));
	};
	if let Some(value) = value.to_isize() {
		return Ok(value);
	}
	if value.sign() == Sign::Minus {
		Ok(isize::MIN)
	} else {
		Ok(isize::MAX)
	}
}

#[cfg(test)]
mod tests {
	use std::ptr;

	use super::super::{
		load_extension_module,
		tests::{ResetImportStateOnDrop, TempExtensionRoot, compile_extension},
	};
	use crate::{
		abi::{format_object_for_print, pon_call, pon_runtime_init},
		import::module_attr,
		intern::intern,
		thread_state::{pon_err_message, test_state_lock},
	};

	#[test]
	fn capi_containers_extension_exercises_core_wrappers() {
		let _guard = test_state_lock();
		let _reset = ResetImportStateOnDrop;
		unsafe {
			assert_eq!(pon_runtime_init(), 0);
		}

		let temp = TempExtensionRoot::new();
		let module_path = compile_extension(
			&temp,
			"capi_containers_ext",
			r#"
#include <Python.h>

static PyObject *fail(const char *message) {
    PyErr_SetString(PyExc_RuntimeError, message);
    return NULL;
}

static int fail_status(const char *message) {
    fail(message);
    return -1;
}

static int expect_long(PyObject *object, long expected, const char *message) {
    long value = 0;
    if (object == NULL) return fail_status(message);
    value = PyLong_AsLong(object);
    if (value != expected || PyErr_Occurred() != NULL) {
        PyErr_Clear();
        fail(message);
        return -1;
    }
    return 0;
}

static int expect_tuple_123(PyObject *tuple, const char *message) {
    if (tuple == NULL || !PyTuple_Check(tuple)) {
        fail(message);
        return -1;
    }
    if (PyTuple_Size(tuple) != 3) {
        fail(message);
        return -1;
    }
    if (expect_long(PyTuple_GetItem(tuple, 0), 1, message) < 0) return -1;
    if (expect_long(PyTuple_GetItem(tuple, 1), 2, message) < 0) return -1;
    if (expect_long(PyTuple_GetItem(tuple, 2), 3, message) < 0) return -1;
    return 0;
}

static int expect_sequence_fast_123(PyObject *sequence, const char *message) {
    PyObject *fast = PySequence_Fast(sequence, message);
    PyObject **items = NULL;
    if (fast == NULL) {
        fail(message);
        return -1;
    }
    if (PySequence_Fast_GET_SIZE(fast) != 3) {
        fail(message);
        return -1;
    }
    if (expect_long(PySequence_Fast_GET_ITEM(fast, 0), 1, message) < 0) return -1;
    if (expect_long(PySequence_Fast_GET_ITEM(fast, 1), 2, message) < 0) return -1;
    if (expect_long(PySequence_Fast_GET_ITEM(fast, 2), 3, message) < 0) return -1;
    items = PySequence_Fast_ITEMS(fast);
    if (items == NULL) {
        fail(message);
        return -1;
    }
    if (expect_long(items[0], 1, message) < 0) return -1;
    if (expect_long(items[1], 2, message) < 0) return -1;
    if (expect_long(items[2], 3, message) < 0) return -1;
    return 0;
}

static int exercise_tuple_api(PyObject **tuple_out) {
    PyObject *tuple = PyTuple_New(3);
    PyObject *slice = NULL;
    if (tuple == NULL) return fail_status("PyTuple_New returned NULL");
    if (PyTuple_SetItem(tuple, 0, PyLong_FromLong(1)) < 0) return fail_status("PyTuple_SetItem failed at index 0");
    if (PyTuple_SetItem(tuple, 1, PyLong_FromLong(2)) < 0) return fail_status("PyTuple_SetItem failed at index 1");
    if (PyTuple_SetItem(tuple, 2, PyLong_FromLong(3)) < 0) return fail_status("PyTuple_SetItem failed at index 2");
    if (PyTuple_Size(tuple) != 3) {
        fail("PyTuple_Size did not report the constructed tuple length");
        return -1;
    }
    if (expect_tuple_123(tuple, "PyTuple_GetItem did not preserve SetItem order") < 0) return -1;
    slice = PyTuple_GetSlice(tuple, 1, 3);
    if (slice == NULL || PyTuple_Size(slice) != 2) {
        fail("PyTuple_GetSlice did not return the requested two-element slice");
        return -1;
    }
    if (expect_long(PyTuple_GetItem(slice, 0), 2, "PyTuple_GetSlice first item mismatch") < 0) return -1;
    if (expect_long(PyTuple_GetItem(slice, 1), 3, "PyTuple_GetSlice second item mismatch") < 0) return -1;
    *tuple_out = tuple;
    return 0;
}

static int exercise_list_api(PyObject **list_out, PyObject **tuple_from_list_out) {
    PyObject *list = PyList_New(0);
    PyObject *tuple = NULL;
    if (list == NULL) return fail_status("PyList_New returned NULL");
    if (PyList_Append(list, PyLong_FromLong(3)) < 0) return fail_status("PyList_Append failed for 3");
    if (PyList_Append(list, PyLong_FromLong(1)) < 0) return fail_status("PyList_Append failed for 1");
    if (PyList_Insert(list, 2, PyLong_FromLong(2)) < 0) return fail_status("PyList_Insert failed for 2");
    if (PyList_Size(list) != 3) {
        fail("PyList append/insert did not produce three items");
        return -1;
    }
    if (PyList_Sort(list) < 0) return fail_status("PyList_Sort failed");
    if (expect_long(PyList_GetItem(list, 0), 1, "PyList_Sort first item mismatch") < 0) return -1;
    if (expect_long(PyList_GetItem(list, 1), 2, "PyList_Sort second item mismatch") < 0) return -1;
    if (expect_long(PyList_GetItem(list, 2), 3, "PyList_Sort third item mismatch") < 0) return -1;
    tuple = PyList_AsTuple(list);
    if (expect_tuple_123(tuple, "PyList_AsTuple did not preserve sorted list values") < 0) return -1;
    *list_out = list;
    *tuple_from_list_out = tuple;
    return 0;
}

static int exercise_dict_api(void) {
    PyObject *dict = PyDict_New();
    PyObject *number_key = PyLong_FromLong(9);
    PyObject *missing_key = PyLong_FromLong(404);
    PyObject *keys = NULL;
    PyObject *values = NULL;
    PyObject *items = NULL;
    PyObject *key = NULL;
    PyObject *value = NULL;
    Py_ssize_t pos = 0;
    Py_ssize_t seen = 0;
    if (dict == NULL || number_key == NULL || missing_key == NULL) return fail_status("PyDict_New or integer key allocation failed");
    if (PyDict_SetItemString(dict, "alpha", PyLong_FromLong(10)) < 0) return fail_status("PyDict_SetItemString failed for alpha");
    if (PyDict_SetItemString(dict, "beta", PyLong_FromLong(20)) < 0) return fail_status("PyDict_SetItemString failed for beta");
    if (PyDict_SetItem(dict, number_key, PyLong_FromLong(90)) < 0) return fail_status("PyDict_SetItem failed for integer key");
    if (PyDict_Size(dict) != 3) {
        fail("PyDict size did not include string and object keys");
        return -1;
    }
    if (expect_long(PyDict_GetItemString(dict, "alpha"), 10, "PyDict_GetItemString value mismatch") < 0) return -1;
    if (PyDict_GetItemWithError(dict, missing_key) != NULL) {
        fail("PyDict_GetItemWithError returned a value for a missing key");
        return -1;
    }
    if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
        fail("PyDict_GetItemWithError set an error for a clean miss");
        return -1;
    }
    while (PyDict_Next(dict, &pos, &key, &value)) {
        if (key == NULL || value == NULL) {
            fail("PyDict_Next produced a NULL key or value");
            return -1;
        }
        seen++;
    }
    if (seen != 3) {
        fail("PyDict_Next did not iterate every entry");
        return -1;
    }
    keys = PyDict_Keys(dict);
    values = PyDict_Values(dict);
    items = PyDict_Items(dict);
    if (keys == NULL || values == NULL || items == NULL) return fail_status("PyDict Keys/Values/Items returned NULL");
    if (PyList_Size(keys) != 3 || PyList_Size(values) != 3 || PyList_Size(items) != 3) {
        fail("PyDict Keys/Values/Items projection size mismatch");
        return -1;
    }
    return 0;
}

static int exercise_slice_api(void) {
    PyObject *first = PySlice_New(PyLong_FromLong(-1), PyLong_FromLong(-7), PyLong_FromLong(-2));
    PyObject *second = PySlice_New(Py_None, Py_None, PyLong_FromLong(-1));
    Py_ssize_t start = 0;
    Py_ssize_t stop = 0;
    Py_ssize_t step = 0;
    Py_ssize_t length = 0;
    if (first == NULL || second == NULL) return fail_status("PySlice_New returned NULL");
    if (PySlice_Unpack(first, &start, &stop, &step) < 0) return fail_status("PySlice_Unpack failed for explicit negative slice");
    length = PySlice_AdjustIndices(8, &start, &stop, step);
    if (start != 7 || stop != 1 || step != -2 || length != 3) {
        fail("PySlice_AdjustIndices mismatch for slice(-1, -7, -2).indices(8)");
        return -1;
    }
    if (PySlice_Unpack(second, &start, &stop, &step) < 0) return fail_status("PySlice_Unpack failed for None negative slice");
    length = PySlice_AdjustIndices(5, &start, &stop, step);
    if (start != 4 || stop != -1 || step != -1 || length != 5) {
        fail("PySlice_AdjustIndices mismatch for slice(None, None, -1).indices(5)");
        return -1;
    }
    return 0;
}

static PyObject *exercise_containers(PyObject *self, PyObject *args) {
    PyObject *tuple = NULL;
    PyObject *list = NULL;
    PyObject *tuple_from_list = NULL;
    PyObject *result = NULL;
    (void)self;
    (void)args;
    if (exercise_tuple_api(&tuple) < 0) return NULL;
    if (exercise_list_api(&list, &tuple_from_list) < 0) return NULL;
    if (exercise_dict_api() < 0) return NULL;
    if (expect_sequence_fast_123(list, "PySequence_Fast list view mismatch") < 0) return NULL;
    if (expect_sequence_fast_123(tuple, "PySequence_Fast tuple view mismatch") < 0) return NULL;
    if (expect_tuple_123(tuple_from_list, "list tuple conversion mismatch") < 0) return NULL;
    if (exercise_slice_api() < 0) return NULL;
    result = PyLong_FromLong(1);
    if (result == NULL) return fail("PyLong_FromLong failed for success result");
    return result;
}

static PyMethodDef methods[] = {
    {"exercise_containers", exercise_containers, METH_NOARGS, "exercise container C-API wrappers"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_containers_ext",
    "Pon container C-API extension test",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_containers_ext(void) {
    return PyModule_Create(&module);
}
"#,
		);

		let module = load_extension_module("capi_containers_ext", &module_path)
			.unwrap_or_else(|message| panic!("failed to load C extension: {message}"));
		assert!(!module.is_null(), "extension loader returned NULL module");

		let module_name = intern("capi_containers_ext");
		let method = module_attr(module_name, intern("exercise_containers"))
			.expect("exercise_containers registered");
		let result = unsafe { pon_call(method, ptr::null_mut(), 0) };
		assert!(!result.is_null(), "exercise_containers() returned NULL: {:?}", pon_err_message());
		assert_eq!(
			format_object_for_print(result).as_deref(),
			Ok("1"),
			"exercise_containers() returned unexpected value: {:?}",
			pon_err_message()
		);
	}
}
