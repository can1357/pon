//! Object family: generic object protocol, calls, attributes, iteration, and
//! type checks.

use core::{
	ffi::{c_char, c_int, c_void},
	mem, ptr,
};
use std::{
	cell::RefCell,
	collections::HashMap,
	ffi::CString,
	panic::{AssertUnwindSafe, catch_unwind},
};

use num_bigint::{BigInt, Sign};
use num_traits::cast::ToPrimitive;

use super::{
	c_string,
	twin::{self, ForeignTypeObject},
};
use crate::{
	abi,
	intern::intern,
	object::{PyObject, PyType},
	thread_state::{pon_err_clear, pon_err_occurred},
	types::exc::ExceptionKind,
};

type VectorcallFunc =
	unsafe extern "C" fn(*mut PyObject, *const *mut PyObject, usize, *mut PyObject) -> *mut PyObject;

const PY_VECTORCALL_ARGUMENTS_OFFSET: usize = 1usize << (usize::BITS - 1);
const TPFLAGS_HAVE_VECTORCALL: u64 = 1 << 11;
const PY_PRINT_RAW: c_int = 1;

type PySsizeT = isize;
type GetBufferProc = unsafe extern "C" fn(*mut PyObject, *mut PyBuffer, c_int) -> c_int;
type ReleaseBufferProc = unsafe extern "C" fn(*mut PyObject, *mut PyBuffer);
type VisitProc = unsafe extern "C" fn(*mut PyObject, *mut c_void) -> c_int;

#[repr(C)]
pub(crate) struct PyBuffer {
	buf:        *mut c_void,
	obj:        *mut PyObject,
	len:        PySsizeT,
	itemsize:   PySsizeT,
	readonly:   c_int,
	ndim:       c_int,
	format:     *mut c_char,
	shape:      *mut PySsizeT,
	strides:    *mut PySsizeT,
	suboffsets: *mut PySsizeT,
	internal:   *mut c_void,
}

impl PyBuffer {
	fn empty() -> Self {
		Self {
			buf:        ptr::null_mut(),
			obj:        ptr::null_mut(),
			len:        0,
			itemsize:   0,
			readonly:   0,
			ndim:       0,
			format:     ptr::null_mut(),
			shape:      ptr::null_mut(),
			strides:    ptr::null_mut(),
			suboffsets: ptr::null_mut(),
			internal:   ptr::null_mut(),
		}
	}
}

#[repr(C)]
struct PyBufferProcs {
	bf_getbuffer:     *mut c_void,
	bf_releasebuffer: *mut c_void,
}

const PYBUF_SIMPLE: c_int = 0;
const PYBUF_WRITABLE: c_int = 0x0001;
const PYBUF_FORMAT: c_int = 0x0004;
const PYBUF_ND: c_int = 0x0008;
const PYBUF_STRIDES: c_int = 0x0010 | PYBUF_ND;
const PYBUF_READ: c_int = 0x100;
const PYBUF_WRITE: c_int = 0x200;

static BUFFER_FORMAT_B: &[u8; 2] = b"B\0";
static BUFFER_FORMAT_I: &[u8; 2] = b"I\0";

thread_local! {
	 static MEMORYVIEW_BUFFER_CACHE: RefCell<HashMap<usize, Box<PyBuffer>>> = RefCell::new(HashMap::new());
}

/// C mirror: `include/pon_capi/object.h` `PyPonCapiObject`.
#[repr(C)]
pub(crate) struct PyPonCapiObject {
	get_attr:               unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
	get_attr_string:        unsafe extern "C" fn(*mut PyObject, *const c_char) -> *mut PyObject,
	set_attr: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int,
	set_attr_string: unsafe extern "C" fn(*mut PyObject, *const c_char, *mut PyObject) -> c_int,
	has_attr:               unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int,
	has_attr_string:        unsafe extern "C" fn(*mut PyObject, *const c_char) -> c_int,
	call: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject,
	call_object:            unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
	call_no_args:           unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	call_one_arg:           unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
	call_varargs: unsafe extern "C" fn(
		*mut PyObject,
		*mut PyObject,
		*mut *mut PyObject,
		usize,
	) -> *mut PyObject,
	repr:                   unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	str_:                   unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	is_true:                unsafe extern "C" fn(*mut PyObject) -> c_int,
	not_:                   unsafe extern "C" fn(*mut PyObject) -> c_int,
	rich_compare: unsafe extern "C" fn(*mut PyObject, *mut PyObject, c_int) -> *mut PyObject,
	rich_compare_bool:      unsafe extern "C" fn(*mut PyObject, *mut PyObject, c_int) -> c_int,
	get_item:               unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
	set_item: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int,
	del_item:               unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int,
	get_iter:               unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	iter_next:              unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	size:                   unsafe extern "C" fn(*mut PyObject) -> isize,
	hash:                   unsafe extern "C" fn(*mut PyObject) -> isize,
	callable_check:         unsafe extern "C" fn(*mut PyObject) -> c_int,
	is_instance:            unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int,
	is_subclass:            unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int,
	type_:                  unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	self_iter:              unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	get_optional_attr:
		unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut *mut PyObject) -> c_int,
	as_file_descriptor:     unsafe extern "C" fn(*mut PyObject) -> c_int,
	vectorcall: unsafe extern "C" fn(
		*mut PyObject,
		*const *mut PyObject,
		usize,
		*mut PyObject,
	) -> *mut PyObject,
	vectorcall_dict: unsafe extern "C" fn(
		*mut PyObject,
		*const *mut PyObject,
		usize,
		*mut PyObject,
	) -> *mut PyObject,
	vectorcall_call:
		unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject,
	vectorcall_function:    unsafe extern "C" fn(*mut PyObject) -> *mut (),
	get_buffer:             unsafe extern "C" fn(*mut PyObject, *mut PyBuffer, c_int) -> c_int,
	release_buffer:         unsafe extern "C" fn(*mut PyBuffer),
	buffer_fill_info: unsafe extern "C" fn(
		*mut PyBuffer,
		*mut PyObject,
		*mut c_void,
		PySsizeT,
		c_int,
		c_int,
	) -> c_int,
	buffer_is_contiguous:   unsafe extern "C" fn(*const PyBuffer, c_char) -> c_int,
	check_buffer:           unsafe extern "C" fn(*mut PyObject) -> c_int,
	memoryview_from_object: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	memoryview_from_buffer: unsafe extern "C" fn(*const PyBuffer) -> *mut PyObject,
	memoryview_get_buffer:  unsafe extern "C" fn(*mut PyObject) -> *mut PyBuffer,
	memoryview_get_base:    unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	type_check:             unsafe extern "C" fn(*mut PyObject) -> c_int,
	iter_check:             unsafe extern "C" fn(*mut PyObject) -> c_int,
	generic_get_attr:       unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
	generic_set_attr: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int,
	generic_get_dict:       unsafe extern "C" fn(*mut PyObject, *mut c_void) -> *mut PyObject,
	print:                  unsafe extern "C" fn(*mut PyObject, *mut libc::FILE, c_int) -> c_int,
	format:                 unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
	clear_weakrefs:         unsafe extern "C" fn(*mut PyObject),
	seq_iter_new:           unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	method_new:             unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
	bytes:                  unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	length_hint:            unsafe extern "C" fn(*mut PyObject, isize) -> isize,
	cfunction_new_ex:
		unsafe extern "C" fn(*mut super::PyMethodDef, *mut PyObject, *mut PyObject) -> *mut PyObject,
	generic_set_dict:       unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut c_void) -> c_int,
	clear_managed_dict:     unsafe extern "C" fn(*mut PyObject) -> c_int,
	visit_managed_dict:     unsafe extern "C" fn(*mut PyObject, VisitProc, *mut c_void) -> c_int,
	memoryview_from_memory: unsafe extern "C" fn(*mut c_char, PySsizeT, c_int) -> *mut PyObject,
}

unsafe impl Send for PyPonCapiObject {}
unsafe impl Sync for PyPonCapiObject {}

pub(crate) fn build() -> PyPonCapiObject {
	PyPonCapiObject {
		get_attr:               capi_get_attr,
		get_attr_string:        capi_get_attr_string,
		set_attr:               capi_set_attr,
		set_attr_string:        capi_set_attr_string,
		has_attr:               capi_has_attr,
		has_attr_string:        capi_has_attr_string,
		call:                   capi_call,
		call_object:            capi_call_object,
		call_no_args:           capi_call_no_args,
		call_one_arg:           capi_call_one_arg,
		call_varargs:           capi_call_varargs,
		repr:                   capi_repr,
		str_:                   capi_str,
		is_true:                capi_is_true,
		not_:                   capi_not,
		rich_compare:           capi_rich_compare,
		rich_compare_bool:      capi_rich_compare_bool,
		get_item:               capi_get_item,
		set_item:               capi_set_item,
		del_item:               capi_del_item,
		get_iter:               capi_get_iter,
		iter_next:              capi_iter_next,
		size:                   capi_size,
		hash:                   capi_hash,
		callable_check:         capi_callable_check,
		is_instance:            capi_is_instance,
		is_subclass:            capi_is_subclass,
		type_:                  capi_type,
		self_iter:              capi_self_iter,
		get_optional_attr:      capi_get_optional_attr,
		as_file_descriptor:     capi_as_file_descriptor,
		vectorcall:             capi_vectorcall,
		vectorcall_dict:        capi_vectorcall_dict,
		vectorcall_call:        capi_vectorcall_call,
		vectorcall_function:    capi_vectorcall_function,
		get_buffer:             capi_get_buffer,
		release_buffer:         capi_release_buffer,
		buffer_fill_info:       capi_buffer_fill_info,
		buffer_is_contiguous:   capi_buffer_is_contiguous,
		check_buffer:           capi_check_buffer,
		memoryview_from_object: capi_memoryview_from_object,
		memoryview_from_buffer: capi_memoryview_from_buffer,
		memoryview_get_buffer:  capi_memoryview_get_buffer,
		memoryview_get_base:    capi_memoryview_get_base,
		type_check:             capi_type_check,
		iter_check:             capi_iter_check,
		generic_get_attr:       capi_generic_get_attr,
		generic_set_attr:       capi_generic_set_attr,
		generic_get_dict:       capi_generic_get_dict,
		print:                  capi_print,
		format:                 capi_format,
		clear_weakrefs:         capi_clear_weakrefs,
		seq_iter_new:           capi_seq_iter_new,
		method_new:             capi_method_new,
		bytes:                  capi_bytes,
		length_hint:            capi_length_hint,
		cfunction_new_ex:       capi_cfunction_new_ex,
		generic_set_dict:       capi_generic_set_dict,
		clear_managed_dict:     capi_clear_managed_dict,
		visit_managed_dict:     capi_visit_managed_dict,
		memoryview_from_memory: capi_memoryview_from_memory,
	}
}

fn catch_object(f: impl FnOnce() -> *mut PyObject) -> *mut PyObject {
	match catch_unwind(AssertUnwindSafe(f)) {
		Ok(value) => super::pin_new_reference(value),
		Err(_) => abi::return_null_with_error("object C-API helper panicked"),
	}
}

fn catch_status(f: impl FnOnce() -> c_int) -> c_int {
	match catch_unwind(AssertUnwindSafe(f)) {
		Ok(value) => value,
		Err(_) => abi::return_minus_one_with_error("object C-API helper panicked"),
	}
}

fn catch_isize(f: impl FnOnce() -> isize) -> isize {
	match catch_unwind(AssertUnwindSafe(f)) {
		Ok(value) => value,
		Err(_) => {
			let _ = abi::return_null_with_error("object C-API helper panicked");
			-1
		},
	}
}

fn raise_type_error(message: &str) -> *mut PyObject {
	// SAFETY: The exception helper copies the message bytes before returning.
	unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn type_error_status(message: &str) -> c_int {
	let _ = raise_type_error(message);
	-1
}

fn type_error_isize(message: &str) -> isize {
	let _ = raise_type_error(message);
	-1
}

fn value_error_status(message: &str) -> c_int {
	let _ = abi::exc::raise_kind_error_text(ExceptionKind::ValueError, message);
	-1
}

fn overflow_error_status(message: &str) -> c_int {
	let _ = abi::exc::raise_kind_error_text(ExceptionKind::OverflowError, message);
	-1
}

fn value_error_isize(message: &str) -> isize {
	let _ = abi::exc::raise_kind_error_text(ExceptionKind::ValueError, message);
	-1
}

fn overflow_error_isize(message: &str) -> isize {
	let _ = abi::exc::raise_kind_error_text(ExceptionKind::OverflowError, message);
	-1
}

fn file_descriptor_from_bigint(value: &BigInt) -> c_int {
	if value.sign() == Sign::Minus {
		return value_error_status(&format!(
			"file descriptor cannot be a negative integer ({value})"
		));
	}
	value
		.to_i32()
		.map(|value| value as c_int)
		.unwrap_or_else(|| overflow_error_status("Python int too large to convert to C int"))
}

unsafe fn file_descriptor_from_required_integer(object: *mut PyObject, type_error: &str) -> c_int {
	let object = normalize_object_arg(object);
	let object = crate::tag::untag_arg(object);
	let Some(value) = (unsafe { crate::types::int::to_bigint_including_bool(object) }) else {
		return type_error_status(type_error);
	};
	file_descriptor_from_bigint(&value)
}

// C-side stores and call-arg unpacking must translate foreign PyTypeObject
// twins into runtime-native classes before values enter Pon data structures.
pub(crate) fn normalize_object_arg(object: *mut PyObject) -> *mut PyObject {
	twin::registered_native_of_foreign(object.cast::<ForeignTypeObject>())
		.map_or(object, |native| native.cast::<PyObject>())
}

unsafe fn name_object_to_interned(name: *mut PyObject) -> Result<u32, *mut PyObject> {
	let name = normalize_object_arg(name);
	let name = crate::tag::untag_arg(name);
	let Some(text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		return Err(raise_type_error("attribute name must be string"));
	};
	Ok(crate::intern::intern(text))
}

fn name_string_to_interned(name: *const c_char) -> Result<u32, *mut PyObject> {
	let Some(text) = c_string(name) else {
		return Err(raise_type_error("attribute name must be string"));
	};
	Ok(crate::intern::intern(&text))
}

unsafe fn normalize_argv(
	argv: *mut *mut PyObject,
	argc: usize,
) -> Result<Vec<*mut PyObject>, *mut PyObject> {
	if argv.is_null() && argc != 0 {
		return Err(abi::return_null_with_error("argv pointer is NULL"));
	}
	let mut out = Vec::with_capacity(argc);
	for index in 0..argc {
		// SAFETY: The caller supplied an array with `argc` readable entries.
		let value = unsafe { *argv.add(index) };
		out.push(normalize_object_arg(value));
	}
	Ok(out)
}

fn argv_ptr(args: &mut [*mut PyObject]) -> *mut *mut PyObject {
	if args.is_empty() {
		ptr::null_mut()
	} else {
		args.as_mut_ptr()
	}
}

unsafe fn call_with_argv(
	callee: *mut PyObject,
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let callee = normalize_object_arg(callee);
	let mut args = match unsafe { normalize_argv(argv, argc) } {
		Ok(args) => args,
		Err(error) => return error,
	};
	// SAFETY: `args` lives for the duration of the call and its pointer is NULL
	// only for zero args.
	unsafe { abi::pon_call(callee, argv_ptr(&mut args), args.len()) }
}

unsafe fn call_method_with_argv(
	object: *mut PyObject,
	name: *mut PyObject,
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let object = normalize_object_arg(object);
	let name = match unsafe { name_object_to_interned(name) } {
		Ok(name) => name,
		Err(error) => return error,
	};
	// SAFETY: Attribute dispatch tolerates a NULL feedback cell.
	let method = unsafe { abi::pon_get_attr(object, name, ptr::null_mut()) };
	if method.is_null() {
		return ptr::null_mut();
	}
	// SAFETY: `method` is a live callable returned by attribute lookup.
	unsafe { call_with_argv(method, argv, argc) }
}

fn vectorcall_nargs(nargsf: usize) -> usize {
	nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET
}

unsafe fn normalize_vectorcall_slice(
	args: *const *mut PyObject,
	offset: usize,
	count: usize,
) -> Result<Vec<*mut PyObject>, *mut PyObject> {
	if args.is_null() && count != 0 {
		return Err(abi::return_null_with_error("vectorcall args pointer is NULL"));
	}
	let mut out = Vec::with_capacity(count);
	for index in 0..count {
		// SAFETY: The caller supplied a vectorcall array with `offset + count`
		// readable object-pointer slots.
		let value = unsafe { *args.add(offset + index) };
		out.push(normalize_object_arg(value));
	}
	Ok(out)
}

unsafe fn keyword_names_from_kwnames(kwnames: *mut PyObject) -> Result<Vec<u32>, *mut PyObject> {
	if kwnames.is_null() {
		return Ok(Vec::new());
	}
	let kwnames = crate::tag::untag_arg(normalize_object_arg(kwnames));
	let Some(entries) = (unsafe { abi::seq::exact_tuple_slice(kwnames) }) else {
		return Err(raise_type_error("vectorcall kwnames must be a tuple"));
	};
	let mut names = Vec::with_capacity(entries.len());
	for &entry in entries {
		let name = crate::tag::untag_arg(normalize_object_arg(entry));
		let Some(text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
			return Err(raise_type_error("vectorcall keyword names must be strings"));
		};
		names.push(intern(text));
	}
	Ok(names)
}

unsafe fn vectorcall_through_pon(
	callable: *mut PyObject,
	args: *const *mut PyObject,
	nargsf: usize,
	kwnames: *mut PyObject,
) -> *mut PyObject {
	let callable = normalize_object_arg(callable);
	if callable.is_null() {
		return abi::return_null_with_error("PyObject_Vectorcall received NULL callable");
	}
	let nargs = vectorcall_nargs(nargsf);
	let names = match unsafe { keyword_names_from_kwnames(kwnames) } {
		Ok(names) => names,
		Err(error) => return error,
	};
	let mut positional = match unsafe { normalize_vectorcall_slice(args, 0, nargs) } {
		Ok(values) => values,
		Err(error) => return error,
	};
	if names.is_empty() {
		// SAFETY: `positional` lives for the duration of the call.
		return unsafe { abi::pon_call(callable, argv_ptr(&mut positional), positional.len()) };
	}
	let mut kw_values = match unsafe { normalize_vectorcall_slice(args, nargs, names.len()) } {
		Ok(values) => values,
		Err(error) => return error,
	};
	// SAFETY: The positional and keyword-value vectors live for the duration
	// of the call; keyword names are interned ids derived from `kwnames`.
	unsafe {
		abi::call::pon_call_ex(
			callable,
			argv_ptr(&mut positional),
			positional.len(),
			ptr::null_mut(),
			names.as_ptr(),
			argv_ptr(&mut kw_values),
			names.len(),
			ptr::null_mut(),
			ptr::null_mut(),
		)
	}
}

unsafe fn vectorcall_through_pon_dict(
	callable: *mut PyObject,
	args: *const *mut PyObject,
	nargsf: usize,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	let callable = normalize_object_arg(callable);
	if callable.is_null() {
		return abi::return_null_with_error("PyObject_VectorcallDict received NULL callable");
	}
	let nargs = vectorcall_nargs(nargsf);
	let mut positional = match unsafe { normalize_vectorcall_slice(args, 0, nargs) } {
		Ok(values) => values,
		Err(error) => return error,
	};
	let kwargs = normalize_object_arg(kwargs);
	if kwargs.is_null() {
		// SAFETY: `positional` lives for the duration of the call.
		return unsafe { abi::pon_call(callable, argv_ptr(&mut positional), positional.len()) };
	}
	// SAFETY: `positional` lives for the duration of the call; kwargs is
	// delegated through the existing `**kwargs` path.
	unsafe {
		abi::call::pon_call_ex(
			callable,
			argv_ptr(&mut positional),
			positional.len(),
			ptr::null_mut(),
			ptr::null(),
			ptr::null_mut(),
			0,
			kwargs,
			ptr::null_mut(),
		)
	}
}

unsafe fn vectorcall_function_for(callable: *mut PyObject) -> Option<VectorcallFunc> {
	let callable = normalize_object_arg(callable);
	if callable.is_null() || crate::tag::is_small_int(callable) || !crate::tag::is_heap(callable) {
		return None;
	}
	// SAFETY: heap-tagged objects carry a readable Pon object header.
	let native_type = unsafe { (*callable).ob_type.cast_mut() };
	let foreign = twin::registered_foreign_of_native(native_type)?;
	// SAFETY: registered foreign type objects are process-lifetime statics.
	let foreign_ref = unsafe { &*foreign };
	if foreign_ref.tp_flags & TPFLAGS_HAVE_VECTORCALL == 0 || foreign_ref.tp_vectorcall_offset <= 0 {
		return None;
	}
	// SAFETY: the type opted into vectorcall and advertised a positive offset
	// into its C instance layout.
	let slot = unsafe {
		callable
			.cast::<u8>()
			.offset(foreign_ref.tp_vectorcall_offset)
			.cast::<*mut ()>()
			.read()
	};
	if slot.is_null() {
		None
	} else {
		// SAFETY: the slot is declared as `vectorcallfunc` by the type.
		Some(unsafe { mem::transmute::<*mut (), VectorcallFunc>(slot) })
	}
}

unsafe fn dict_keywords_for_vectorcall(
	dict: *mut PyObject,
	argv: &mut Vec<*mut PyObject>,
) -> Result<*mut PyObject, *mut PyObject> {
	if dict.is_null() {
		return Ok(ptr::null_mut());
	}
	let dict = normalize_object_arg(dict);
	let entries = match unsafe { crate::types::dict::dict_entries_snapshot(dict) } {
		Ok(entries) => entries,
		Err(message) => return Err(raise_type_error(&message)),
	};
	if entries.is_empty() {
		return Ok(ptr::null_mut());
	}
	let mut names = Vec::with_capacity(entries.len());
	for entry in entries {
		let key = crate::tag::untag_arg(normalize_object_arg(entry.key));
		if unsafe { crate::types::type_::unicode_text(key) }.is_none() {
			return Err(raise_type_error("vectorcall keyword names must be strings"));
		}
		names.push(key);
		argv.push(normalize_object_arg(entry.value));
	}
	// SAFETY: `names` lives through tuple construction; the resulting tuple
	// owns references in Pon's GC model by reachability for this call.
	let tuple = unsafe { abi::seq::pon_build_tuple(argv_ptr(&mut names), names.len()) };
	if tuple.is_null() {
		Err(ptr::null_mut())
	} else {
		Ok(tuple)
	}
}

unsafe fn vectorcall_fallback_would_recurse(callable: *mut PyObject) -> bool {
	let callable = normalize_object_arg(callable);
	if callable.is_null() || crate::tag::is_small_int(callable) || !crate::tag::is_heap(callable) {
		return false;
	}
	let ty = unsafe { (*callable).ob_type.cast_mut() };
	!ty.is_null()
		&& unsafe {
			(*ty)
				.tp_call
				.map(|call| call as *const () == capi_vectorcall_call as *const ())
				.unwrap_or(false)
		}
}

unsafe fn positional_args_from_object(
	args: *mut PyObject,
) -> Result<Vec<*mut PyObject>, *mut PyObject> {
	if args.is_null() {
		return Ok(Vec::new());
	}
	let args = normalize_object_arg(args);
	let mut positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
		Ok(values) => values,
		Err(message) => return Err(raise_type_error(&message)),
	};
	for value in &mut positional {
		*value = normalize_object_arg(*value);
	}
	Ok(positional)
}

fn valid_rich_compare_op(op: c_int) -> bool {
	matches!(
		op as u8,
		abi::object::RICH_LT
			| abi::object::RICH_LE
			| abi::object::RICH_EQ
			| abi::object::RICH_NE
			| abi::object::RICH_GT
			| abi::object::RICH_GE
	) && (0..=5).contains(&op)
}

unsafe fn object_native_type(object: *mut PyObject) -> Result<*mut PyType, *mut PyObject> {
	let object = normalize_object_arg(object);
	if object.is_null() {
		return Err(abi::return_null_with_error("PyObject_Type received NULL"));
	}
	if crate::tag::is_small_int(object) {
		let ty = abi::runtime_long_type();
		return if ty.is_null() {
			Err(abi::return_null_with_error("runtime is not initialized"))
		} else {
			Ok(ty)
		};
	}
	if !crate::tag::is_heap(object) {
		return Err(abi::return_null_with_error("object pointer is not a heap object"));
	}
	// SAFETY: Heap-tagged, non-NULL objects carry a readable header.
	let ty = unsafe { (*object).ob_type }.cast_mut();
	if ty.is_null() {
		Err(abi::return_null_with_error("object has NULL type"))
	} else {
		Ok(ty)
	}
}

unsafe fn is_instance_impl(object: *mut PyObject, classinfo: *mut PyObject) -> c_int {
	let object = normalize_object_arg(object);
	let classinfo = normalize_object_arg(classinfo);
	if !classinfo.is_null() && crate::tag::is_heap(classinfo) {
		// SAFETY: Heap-tagged runtime tuples expose stable element storage.
		if let Some(entries) = unsafe { abi::seq::exact_tuple_slice(classinfo) } {
			for entry in entries.iter().copied() {
				let result = unsafe { is_instance_impl(object, entry) };
				if result != 0 {
					return result;
				}
			}
			return 0;
		}
	}
	// SAFETY: `classinfo` has been translated when it is a registered foreign type
	// twin.
	unsafe { abi::attr::pon_isinstance(object, classinfo) }
}

unsafe fn is_subclass_impl(cls: *mut PyObject, classinfo: *mut PyObject) -> c_int {
	let cls = normalize_object_arg(cls);
	let classinfo = normalize_object_arg(classinfo);
	if !classinfo.is_null() && crate::tag::is_heap(classinfo) {
		// SAFETY: Heap-tagged runtime tuples expose stable element storage.
		if let Some(entries) = unsafe { abi::seq::exact_tuple_slice(classinfo) } {
			for entry in entries.iter().copied() {
				let result = unsafe { is_subclass_impl(cls, entry) };
				if result != 0 {
					return result;
				}
			}
			return 0;
		}
	}
	// SAFETY: `cls`/`classinfo` have been translated when they are registered
	// foreign type twins.
	unsafe { abi::attr::pon_issubclass(cls, classinfo) }
}

/// Attribute lookup shared by the C getters and the `hasattr` probes: no
/// pin, so probe-style callers do not accumulate owned references.
unsafe fn get_attr_unpinned(object: *mut PyObject, name: u32) -> *mut PyObject {
	let object = normalize_object_arg(object);
	// SAFETY: Attribute dispatch tolerates a NULL feedback cell.
	unsafe { abi::pon_get_attr(object, name, ptr::null_mut()) }
}

unsafe extern "C" fn capi_get_attr(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	catch_object(|| {
		let name = match unsafe { name_object_to_interned(name) } {
			Ok(name) => name,
			Err(error) => return error,
		};
		unsafe { get_attr_unpinned(object, name) }
	})
}

unsafe extern "C" fn capi_get_attr_string(
	object: *mut PyObject,
	name: *const c_char,
) -> *mut PyObject {
	catch_object(|| {
		let name = match name_string_to_interned(name) {
			Ok(name) => name,
			Err(error) => return error,
		};
		unsafe { get_attr_unpinned(object, name) }
	})
}

unsafe extern "C" fn capi_get_optional_attr(
	object: *mut PyObject,
	name: *mut PyObject,
	result: *mut *mut PyObject,
) -> c_int {
	catch_status(|| {
		if result.is_null() {
			return type_error_status("PyObject_GetOptionalAttr result pointer must not be NULL");
		}
		unsafe {
			*result = ptr::null_mut();
		}
		let name = match unsafe { name_object_to_interned(name) } {
			Ok(name) => name,
			Err(_) => return -1,
		};
		let value = unsafe { get_attr_unpinned(object, name) };
		if !value.is_null() {
			// Owned-reference out-param: mirror capi_get_attr's pinned result.
			super::pin_object(value);
			unsafe {
				*result = super::foreignize_type_result(value);
			}
			return 1;
		}
		if !pon_err_occurred() || abi::exc::pending_exception_is("AttributeError") {
			pon_err_clear();
			return 0;
		}
		-1
	})
}

unsafe extern "C" fn capi_set_attr(
	object: *mut PyObject,
	name: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	catch_status(|| {
		let object = normalize_object_arg(object);
		let name = match unsafe { name_object_to_interned(name) } {
			Ok(name) => name,
			Err(_) => return -1,
		};
		let value = normalize_object_arg(value);
		if value.is_null() {
			// SAFETY: Attribute deletion dispatch tolerates a live receiver and interned
			// name.
			unsafe { abi::pon_del_attr(object, name) }
		} else {
			// SAFETY: Attribute assignment dispatch tolerates a live receiver/value and
			// interned name.
			unsafe { abi::pon_set_attr(object, name, value) }
		}
	})
}

unsafe extern "C" fn capi_set_attr_string(
	object: *mut PyObject,
	name: *const c_char,
	value: *mut PyObject,
) -> c_int {
	catch_status(|| {
		let object = normalize_object_arg(object);
		let name = match name_string_to_interned(name) {
			Ok(name) => name,
			Err(_) => return -1,
		};
		let value = normalize_object_arg(value);
		let status = if value.is_null() {
			// SAFETY: Attribute deletion dispatch tolerates a live receiver and interned
			// name.
			unsafe { abi::pon_del_attr(object, name) }
		} else {
			// SAFETY: Attribute assignment dispatch tolerates a live receiver/value and
			// interned name.
			unsafe { abi::pon_set_attr(object, name, value) }
		};
		if status < 0 {
			// TEMP diagnostic for numpy bring-up.
			let ty = unsafe { crate::types::dict::type_name(object) }.unwrap_or("<untyped>");
			let attr = crate::intern::resolve(name).unwrap_or_default();
			eprintln!(
				"[pon-diag] capi setattr failed: type '{}' attr '{}' err {:?}",
				ty,
				attr,
				crate::thread_state::pon_err_message()
			);
		}
		status
	})
}

unsafe extern "C" fn capi_has_attr(object: *mut PyObject, name: *mut PyObject) -> c_int {
	catch_status(|| {
		let name = match unsafe { name_object_to_interned(name) } {
			Ok(name) => name,
			Err(_) => {
				pon_err_clear();
				return 0;
			},
		};
		// Probe only (PyObject_HasAttr returns no reference): the unpinned
		// lookup keeps successful probes from leaking owned references.
		let value = unsafe { get_attr_unpinned(object, name) };
		if value.is_null() {
			pon_err_clear();
			0
		} else {
			1
		}
	})
}

unsafe extern "C" fn capi_has_attr_string(object: *mut PyObject, name: *const c_char) -> c_int {
	catch_status(|| {
		let name = match name_string_to_interned(name) {
			Ok(name) => name,
			Err(_) => {
				pon_err_clear();
				return 0;
			},
		};
		// Probe only, mirroring `capi_has_attr`.
		let value = unsafe { get_attr_unpinned(object, name) };
		if value.is_null() {
			pon_err_clear();
			0
		} else {
			1
		}
	})
}

unsafe extern "C" fn capi_call(
	callee: *mut PyObject,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	catch_object(|| {
		let callee = normalize_object_arg(callee);
		let mut positional = match unsafe { positional_args_from_object(args) } {
			Ok(values) => values,
			Err(error) => return error,
		};
		let kwargs = normalize_object_arg(kwargs);
		if kwargs.is_null() {
			// SAFETY: `positional` lives for the duration of the call.
			unsafe { abi::pon_call(callee, argv_ptr(&mut positional), positional.len()) }
		} else {
			// SAFETY: `positional` lives for the duration of the call; kwargs is delegated
			// as `**kwargs`.
			unsafe {
				abi::call::pon_call_ex(
					callee,
					argv_ptr(&mut positional),
					positional.len(),
					ptr::null_mut(),
					ptr::null(),
					ptr::null_mut(),
					0,
					kwargs,
					ptr::null_mut(),
				)
			}
		}
	})
}

unsafe extern "C" fn capi_call_object(callee: *mut PyObject, args: *mut PyObject) -> *mut PyObject {
	// SAFETY: Same contract as PyObject_Call with NULL kwargs.
	unsafe { capi_call(callee, args, ptr::null_mut()) }
}

unsafe extern "C" fn capi_call_no_args(callee: *mut PyObject) -> *mut PyObject {
	catch_object(|| {
		// SAFETY: NULL argv with zero argc denotes no positional arguments.
		unsafe { call_with_argv(callee, ptr::null_mut(), 0) }
	})
}

unsafe extern "C" fn capi_call_one_arg(callee: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
	catch_object(|| {
		let mut argv = [arg];
		// SAFETY: `argv` has one live slot for the duration of the call.
		unsafe { call_with_argv(callee, argv.as_mut_ptr(), 1) }
	})
}

unsafe extern "C" fn capi_call_varargs(
	target: *mut PyObject,
	name: *mut PyObject,
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	catch_object(|| {
		if name.is_null() {
			// SAFETY: The inline C wrapper supplied `argv`/`argc` from a bounded local
			// array.
			unsafe { call_with_argv(target, argv, argc) }
		} else {
			// SAFETY: The inline C wrapper supplied `argv`/`argc` from a bounded local
			// array.
			unsafe { call_method_with_argv(target, name, argv, argc) }
		}
	})
}

unsafe extern "C" fn capi_vectorcall(
	callable: *mut PyObject,
	args: *const *mut PyObject,
	nargsf: usize,
	kwnames: *mut PyObject,
) -> *mut PyObject {
	catch_object(|| unsafe { vectorcall_through_pon(callable, args, nargsf, kwnames) })
}

unsafe extern "C" fn capi_vectorcall_dict(
	callable: *mut PyObject,
	args: *const *mut PyObject,
	nargsf: usize,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	catch_object(|| unsafe { vectorcall_through_pon_dict(callable, args, nargsf, kwargs) })
}

unsafe extern "C" fn capi_vectorcall_call(
	callable: *mut PyObject,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	catch_object(|| {
		let callable = normalize_object_arg(callable);
		if callable.is_null() {
			return abi::return_null_with_error("PyVectorcall_Call received NULL callable");
		}
		let mut positional = match unsafe { positional_args_from_object(args) } {
			Ok(values) => values,
			Err(error) => return error,
		};
		if let Some(vectorcall) = unsafe { vectorcall_function_for(callable) } {
			let positional_count = positional.len();
			let kwnames = match unsafe { dict_keywords_for_vectorcall(kwargs, &mut positional) } {
				Ok(kwnames) => kwnames,
				Err(error) => return error,
			};
			let argv = if positional.is_empty() {
				ptr::null()
			} else {
				positional.as_ptr()
			};
			// SAFETY: `argv` and `kwnames` live for the duration of the call;
			// the function pointer was read from the callable's vectorcall slot.
			let result = unsafe { vectorcall(callable, argv, positional_count, kwnames) };
			super::unpin_object(result);
			return result;
		}
		if unsafe { vectorcall_fallback_would_recurse(callable) } {
			return raise_type_error(
				"PyVectorcall_Call callable does not provide a vectorcall function",
			);
		}
		if kwargs.is_null() {
			unsafe { call_with_argv(callable, argv_ptr(&mut positional), positional.len()) }
		} else {
			unsafe {
				abi::call::pon_call_ex(
					callable,
					argv_ptr(&mut positional),
					positional.len(),
					ptr::null_mut(),
					ptr::null(),
					ptr::null_mut(),
					0,
					kwargs,
					ptr::null_mut(),
				)
			}
		}
	})
}

unsafe extern "C" fn capi_vectorcall_function(callable: *mut PyObject) -> *mut () {
	match catch_unwind(AssertUnwindSafe(|| unsafe { vectorcall_function_for(callable) })) {
		Ok(Some(function)) => function as *mut (),
		_ => ptr::null_mut(),
	}
}
unsafe extern "C" fn capi_repr(object: *mut PyObject) -> *mut PyObject {
	catch_object(|| {
		let object = normalize_object_arg(object);
		let text = match crate::native::builtins_mod::try_repr_text(object) {
			Ok(text) => text,
			Err(()) => return ptr::null_mut(),
		};
		// SAFETY: The string helper copies the UTF-8 bytes into a runtime str.
		unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
	})
}

unsafe extern "C" fn capi_str(object: *mut PyObject) -> *mut PyObject {
	catch_object(|| {
		let object = normalize_object_arg(object);
		let text = match crate::native::builtins_mod::try_str_text(object) {
			Ok(text) => text,
			Err(()) => return ptr::null_mut(),
		};
		// SAFETY: The string helper copies the UTF-8 bytes into a runtime str.
		unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
	})
}

unsafe extern "C" fn capi_bytes(object: *mut PyObject) -> *mut PyObject {
	catch_object(|| {
		let object = normalize_object_arg(object);
		let mut argv = [object];
		// SAFETY: `argv` is a live one-element positional argument vector for
		// `bytes(object)`.
		unsafe { crate::native::builtins_mod::builtin_bytes(argv.as_mut_ptr(), argv.len()) }
	})
}

unsafe extern "C" fn capi_is_true(object: *mut PyObject) -> c_int {
	let object = normalize_object_arg(object);
	// SAFETY: Delegates truth dispatch to the runtime helper.
	unsafe { abi::pon_is_true(object) }
}

unsafe extern "C" fn capi_not(object: *mut PyObject) -> c_int {
	let truth = unsafe { capi_is_true(object) };
	if truth < 0 {
		-1
	} else {
		c_int::from(truth == 0)
	}
}

unsafe extern "C" fn capi_rich_compare(
	left: *mut PyObject,
	right: *mut PyObject,
	op: c_int,
) -> *mut PyObject {
	catch_object(|| {
		if !valid_rich_compare_op(op) {
			return raise_type_error("unknown rich comparison operation");
		}
		let left = normalize_object_arg(left);
		let right = normalize_object_arg(right);
		// SAFETY: Rich comparison dispatch tolerates a NULL feedback cell.
		unsafe { abi::pon_rich_compare(op as u8, left, right, ptr::null_mut()) }
	})
}

unsafe extern "C" fn capi_rich_compare_bool(
	left: *mut PyObject,
	right: *mut PyObject,
	op: c_int,
) -> c_int {
	catch_status(|| {
		if !valid_rich_compare_op(op) {
			return type_error_status("unknown rich comparison operation");
		}
		let left = normalize_object_arg(left);
		let right = normalize_object_arg(right);
		if left == right {
			if op as u8 == abi::object::RICH_EQ {
				return 1;
			}
			if op as u8 == abi::object::RICH_NE {
				return 0;
			}
		}
		// SAFETY: Delegates to the object result form, then coerces through truth
		// testing.
		let result = unsafe { capi_rich_compare(left, right, op) };
		if result.is_null() {
			return -1;
		}
		let truth = unsafe { abi::pon_is_true(result) };
		super::unpin_object(result);
		truth
	})
}

unsafe extern "C" fn capi_get_item(object: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
	catch_object(|| {
		let object = normalize_object_arg(object);
		let key = normalize_object_arg(key);
		// SAFETY: Subscription dispatch tolerates a NULL feedback cell.
		unsafe { abi::pon_subscript_get(object, key, ptr::null_mut()) }
	})
}

unsafe extern "C" fn capi_set_item(
	object: *mut PyObject,
	key: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	catch_status(|| {
		let object = normalize_object_arg(object);
		let key = normalize_object_arg(key);
		let value = normalize_object_arg(value);
		// SAFETY: Runtime helper implements mapping/sequence assignment and returns
		// NULL on failure.
		let result = unsafe { abi::map::pon_subscript_set(object, key, value) };
		if result.is_null() { -1 } else { 0 }
	})
}

unsafe extern "C" fn capi_del_item(object: *mut PyObject, key: *mut PyObject) -> c_int {
	catch_status(|| {
		let object = normalize_object_arg(object);
		let key = normalize_object_arg(key);
		// SAFETY: Runtime helper implements mapping/sequence deletion and returns NULL
		// on failure.
		let result = unsafe { abi::map::pon_subscript_del(object, key) };
		if result.is_null() { -1 } else { 0 }
	})
}

unsafe extern "C" fn capi_get_iter(object: *mut PyObject) -> *mut PyObject {
	catch_object(|| {
		let object = normalize_object_arg(object);
		// SAFETY: Iterator dispatch tolerates a NULL feedback cell.
		unsafe { abi::pon_get_iter(object, ptr::null_mut()) }
	})
}

unsafe extern "C" fn capi_iter_next(iterator: *mut PyObject) -> *mut PyObject {
	catch_object(|| {
		let iterator = normalize_object_arg(iterator);
		// SAFETY: Iterator dispatch tolerates a NULL feedback cell.
		let result = unsafe { abi::pon_iter_next(iterator, ptr::null_mut()) };
		if result.is_null() && abi::exc::pending_exception_is("StopIteration") {
			pon_err_clear();
		}
		result
	})
}

unsafe extern "C" fn capi_size(object: *mut PyObject) -> isize {
	catch_isize(|| {
		let object = normalize_object_arg(object);
		// SAFETY: Runtime length helper returns a boxed integer or NULL with an error
		// set.
		let result = unsafe { abi::seq::pon_get_len(object, ptr::null_mut()) };
		if result.is_null() {
			return -1;
		}
		let Some(length) = (unsafe { crate::types::int::to_bigint_including_bool(result) }) else {
			return type_error_isize("__len__() should return an integer");
		};
		length
			.to_isize()
			.unwrap_or_else(|| type_error_isize("__len__() result is too large"))
	})
}

unsafe extern "C" fn capi_length_hint(object: *mut PyObject, default_value: isize) -> isize {
	catch_isize(|| {
		let object = normalize_object_arg(object);
		if object.is_null() {
			return type_error_isize("PyObject_LengthHint called with NULL object");
		}
		if default_value < 0 {
			return value_error_isize("PyObject_LengthHint default must be non-negative");
		}

		match unsafe { length_hint_slot_len(object) } {
			LengthHintOutcome::Value(len) => return len,
			LengthHintOutcome::Error => return -1,
			LengthHintOutcome::Missing => {},
		}

		match unsafe { call_optional_length_method(object, "__len__") } {
			Ok(Some(result)) => return length_hint_result_to_isize(result, "__len__()"),
			Ok(None) => {},
			Err(()) => return -1,
		}

		match unsafe { call_optional_length_method(object, "__length_hint__") } {
			Ok(Some(result)) => {
				let result = crate::tag::untag_arg(result);
				if unsafe { crate::abstract_op::is_not_implemented(result) } {
					default_value
				} else {
					length_hint_result_to_isize(result, "__length_hint__()")
				}
			},
			Ok(None) => default_value,
			Err(()) => -1,
		}
	})
}

enum LengthHintOutcome {
	Missing,
	Value(isize),
	Error,
}

unsafe fn length_hint_slot_len(object: *mut PyObject) -> LengthHintOutcome {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || !crate::tag::is_heap(object) {
		return LengthHintOutcome::Missing;
	}
	let ty = unsafe { (*object).ob_type };
	if ty.is_null() {
		return LengthHintOutcome::Missing;
	}
	let slot = unsafe {
		(*ty)
			.tp_as_sequence
			.as_ref()
			.and_then(|methods| methods.sq_length)
	}
	.or_else(|| unsafe {
		(*ty)
			.tp_as_mapping
			.as_ref()
			.and_then(|methods| methods.mp_length)
	});
	let Some(slot) = slot else {
		return LengthHintOutcome::Missing;
	};
	let len = unsafe { slot(object) };
	if len >= 0 {
		return LengthHintOutcome::Value(len);
	}
	if pon_err_occurred() && abi::exc::pending_exception_is("TypeError") {
		pon_err_clear();
		return LengthHintOutcome::Missing;
	}
	if !pon_err_occurred() {
		let _ = value_error_isize("__len__() should return >= 0");
	}
	LengthHintOutcome::Error
}

unsafe fn call_optional_length_method(
	object: *mut PyObject,
	name: &str,
) -> Result<Option<*mut PyObject>, ()> {
	let method = unsafe { abi::pon_get_attr(object, intern(name), ptr::null_mut()) };
	if method.is_null() {
		if !pon_err_occurred() || abi::exc::pending_exception_is("AttributeError") {
			pon_err_clear();
			return Ok(None);
		}
		return Err(());
	}
	let result = unsafe { abi::pon_call(method, ptr::null_mut(), 0) };
	if result.is_null() {
		if abi::exc::pending_exception_is("TypeError") {
			pon_err_clear();
			return Ok(None);
		}
		return Err(());
	}
	Ok(Some(result))
}

fn length_hint_result_to_isize(result: *mut PyObject, source: &str) -> isize {
	let result = crate::tag::untag_arg(result);
	let Some(length) = (unsafe { crate::types::int::to_bigint_including_bool(result) }) else {
		return type_error_isize(&format!("{source} should return an integer"));
	};
	if length.sign() == Sign::Minus {
		return value_error_isize(&format!("{source} should return >= 0"));
	}
	length
		.to_isize()
		.unwrap_or_else(|| overflow_error_isize(&format!("{source} result is too large")))
}

unsafe extern "C" fn capi_hash(object: *mut PyObject) -> isize {
	catch_isize(|| {
		let object = normalize_object_arg(object);
		match unsafe { crate::types::dict::hash_object(object) } {
			Ok(hash) => hash,
			Err(message) => {
				if !pon_err_occurred() {
					let _ = raise_type_error(&message);
				}
				-1
			},
		}
	})
}

unsafe extern "C" fn capi_as_file_descriptor(object: *mut PyObject) -> c_int {
	catch_status(|| {
		let object = normalize_object_arg(object);
		let object = crate::tag::untag_arg(object);
		if let Some(value) = unsafe { crate::types::int::to_bigint_including_bool(object) } {
			return file_descriptor_from_bigint(&value);
		}

		let method = unsafe { get_attr_unpinned(object, intern("fileno")) };
		if method.is_null() {
			if !pon_err_occurred() || abi::exc::pending_exception_is("AttributeError") {
				pon_err_clear();
				return type_error_status("argument must be an int, or have a fileno() method.");
			}
			return -1;
		}

		let result = unsafe { call_with_argv(method, ptr::null_mut(), 0) };
		if result.is_null() {
			return -1;
		}
		unsafe { file_descriptor_from_required_integer(result, "fileno() returned a non-integer") }
	})
}

unsafe extern "C" fn capi_callable_check(object: *mut PyObject) -> c_int {
	let object = normalize_object_arg(object);
	c_int::from(abi::call::is_callable_object(object))
}

unsafe extern "C" fn capi_is_instance(object: *mut PyObject, classinfo: *mut PyObject) -> c_int {
	catch_status(|| unsafe { is_instance_impl(object, classinfo) })
}

unsafe extern "C" fn capi_is_subclass(cls: *mut PyObject, classinfo: *mut PyObject) -> c_int {
	catch_status(|| unsafe { is_subclass_impl(cls, classinfo) })
}

unsafe extern "C" fn capi_type(object: *mut PyObject) -> *mut PyObject {
	catch_object(|| {
		let ty = match unsafe { object_native_type(object) } {
			Ok(ty) => ty,
			Err(error) => return error,
		};
		twin::foreign_of_native(ty).cast::<PyObject>()
	})
}

unsafe extern "C" fn capi_self_iter(object: *mut PyObject) -> *mut PyObject {
	super::pin_new_reference(object)
}

fn raise_status(kind: ExceptionKind, message: &str) -> c_int {
	let _ = abi::exc::raise_kind_error_text(kind, message);
	-1
}

unsafe fn slot_function<F>(field: *mut c_void) -> Option<F> {
	if field.is_null() {
		None
	} else {
		// SAFETY: the caller chooses `F` to match the C slot's declared ABI.
		Some(unsafe { core::mem::transmute_copy::<*mut c_void, F>(&field) })
	}
}

unsafe fn object_type_for_buffer(object: *mut PyObject) -> Option<*mut PyType> {
	let object = normalize_object_arg(object);
	if object.is_null() || crate::tag::is_small_int(object) || !crate::tag::is_heap(object) {
		return None;
	}
	// SAFETY: heap-tagged objects have a readable Pon object header.
	let ty = unsafe { (*object).ob_type }.cast_mut();
	(!ty.is_null()).then_some(ty)
}

unsafe fn foreign_buffer_procs(object: *mut PyObject) -> Option<*mut PyBufferProcs> {
	let ty = unsafe { object_type_for_buffer(object) }?;
	let foreign = twin::registered_foreign_of_native(ty)?;
	// SAFETY: registered foreign type objects are process-lifetime C structs.
	let procs = unsafe { (*foreign).tp_as_buffer }.cast::<PyBufferProcs>();
	(!procs.is_null()).then_some(procs)
}

unsafe fn foreign_getbuffer(object: *mut PyObject) -> Option<GetBufferProc> {
	let procs = unsafe { foreign_buffer_procs(object) }?;
	// SAFETY: non-NULL `tp_as_buffer` points at a `PyBufferProcs` table.
	unsafe { slot_function((*procs).bf_getbuffer) }
}

unsafe fn foreign_releasebuffer(object: *mut PyObject) -> Option<ReleaseBufferProc> {
	let procs = unsafe { foreign_buffer_procs(object) }?;
	// SAFETY: non-NULL `tp_as_buffer` points at a `PyBufferProcs` table.
	unsafe { slot_function((*procs).bf_releasebuffer) }
}

unsafe fn pon_bytes_buffer(object: *mut PyObject) -> Option<(*mut c_void, PySsizeT, c_int)> {
	let object = normalize_object_arg(object);
	if object.is_null() || crate::tag::is_small_int(object) || !crate::tag::is_heap(object) {
		return None;
	}
	// SAFETY: heap-tagged objects have a readable Pon object header.
	let ty = unsafe { (*object).ob_type };
	if crate::types::bytes_::is_bytes_type(ty) {
		// SAFETY: the exact type check proves the boxed bytes layout.
		let bytes = unsafe { (&*object.cast::<crate::types::bytes_::PyBytes>()).as_slice() };
		return Some((bytes.as_ptr().cast::<c_void>().cast_mut(), bytes.len() as PySsizeT, 1));
	}
	if crate::types::bytearray_::is_bytearray_type(ty) {
		// SAFETY: the exact type check proves the boxed bytearray layout; the buffer
		// API exposes its mutable storage.
		let bytes =
			unsafe { (&mut *object.cast::<crate::types::bytearray_::PyByteArray>()).as_mut_slice() };
		return Some((bytes.as_mut_ptr().cast::<c_void>(), bytes.len() as PySsizeT, 0));
	}
	None
}

unsafe fn fill_buffer_info_impl(
	view: *mut PyBuffer,
	object: *mut PyObject,
	buf: *mut c_void,
	len: PySsizeT,
	readonly: c_int,
	flags: c_int,
) -> c_int {
	if view.is_null() {
		return raise_status(
			ExceptionKind::BufferError,
			"PyBuffer_FillInfo: view==NULL argument is obsolete",
		);
	}
	if flags != PYBUF_SIMPLE {
		if flags == PYBUF_READ || flags == PYBUF_WRITE {
			return raise_status(ExceptionKind::SystemError, "bad argument to internal function");
		}
		if (flags & PYBUF_WRITABLE) == PYBUF_WRITABLE && readonly != 0 {
			return raise_status(ExceptionKind::BufferError, "Object is not writable.");
		}
	}
	let readonly = c_int::from(readonly != 0);
	// SAFETY: caller supplied a writable `Py_buffer` out-parameter.
	unsafe {
		(*view).obj = object;
		(*view).buf = buf;
		(*view).len = len;
		(*view).readonly = readonly;
		(*view).itemsize = 1;
		(*view).format = if (flags & PYBUF_FORMAT) == PYBUF_FORMAT {
			BUFFER_FORMAT_B.as_ptr().cast::<c_char>().cast_mut()
		} else {
			ptr::null_mut()
		};
		(*view).ndim = 1;
		(*view).shape = if (flags & PYBUF_ND) == PYBUF_ND {
			ptr::addr_of_mut!((*view).len)
		} else {
			ptr::null_mut()
		};
		(*view).strides = if (flags & PYBUF_STRIDES) == PYBUF_STRIDES {
			ptr::addr_of_mut!((*view).itemsize)
		} else {
			ptr::null_mut()
		};
		(*view).suboffsets = ptr::null_mut();
		(*view).internal = ptr::null_mut();
	}
	super::pin_object(object);
	0
}

unsafe fn get_buffer_impl(object: *mut PyObject, view: *mut PyBuffer, flags: c_int) -> c_int {
	if view.is_null() {
		return raise_status(ExceptionKind::SystemError, "PyObject_GetBuffer: view must not be NULL");
	}
	if flags != PYBUF_SIMPLE && (flags == PYBUF_READ || flags == PYBUF_WRITE) {
		return raise_status(ExceptionKind::SystemError, "bad argument to internal function");
	}
	let object = normalize_object_arg(object);
	if let Some(getbuffer) = unsafe { foreign_getbuffer(object) } {
		// SAFETY: foreign slot ABI is `getbufferproc`; the extension owns the callback.
		return unsafe { getbuffer(object, view, flags) };
	}
	if let Some((buf, len, readonly)) = unsafe { pon_bytes_buffer(object) } {
		return unsafe { fill_buffer_info_impl(view, object, buf, len, readonly, flags) };
	}
	type_error_status("a bytes-like object is required")
}

unsafe fn object_supports_buffer(object: *mut PyObject) -> bool {
	let object = normalize_object_arg(object);
	unsafe { foreign_getbuffer(object).is_some() || pon_bytes_buffer(object).is_some() }
}

unsafe fn zero_buffer(view: *mut PyBuffer) {
	if !view.is_null() {
		// SAFETY: all-zero is the desired released state for this C POD struct.
		unsafe { ptr::write_bytes(view, 0, 1) };
	}
}

unsafe fn is_c_contiguous(view: &PyBuffer) -> bool {
	if view.len == 0 || view.strides.is_null() {
		return true;
	}
	if view.ndim <= 0 || view.shape.is_null() {
		return false;
	}
	let mut stride = view.itemsize;
	for index in (0..view.ndim as usize).rev() {
		// SAFETY: `shape`/`strides` arrays are at least `ndim` long by buffer-protocol
		// contract.
		let dim = unsafe { *view.shape.add(index) };
		let actual = unsafe { *view.strides.add(index) };
		if dim > 1 && actual != stride {
			return false;
		}
		stride = stride.saturating_mul(dim);
	}
	true
}

unsafe fn is_fortran_contiguous(view: &PyBuffer) -> bool {
	if view.len == 0 {
		return true;
	}
	if view.strides.is_null() {
		if view.ndim <= 1 {
			return true;
		}
		if view.shape.is_null() {
			return false;
		}
		let mut significant = 0;
		for index in 0..view.ndim as usize {
			// SAFETY: `shape` is at least `ndim` long by buffer-protocol contract.
			if unsafe { *view.shape.add(index) } > 1 {
				significant += 1;
			}
		}
		return significant <= 1;
	}
	if view.ndim <= 0 || view.shape.is_null() {
		return false;
	}
	let mut stride = view.itemsize;
	for index in 0..view.ndim as usize {
		// SAFETY: `shape`/`strides` arrays are at least `ndim` long by buffer-protocol
		// contract.
		let dim = unsafe { *view.shape.add(index) };
		let actual = unsafe { *view.strides.add(index) };
		if dim > 1 && actual != stride {
			return false;
		}
		stride = stride.saturating_mul(dim);
	}
	true
}

unsafe fn is_buffer_contiguous_impl(view: *const PyBuffer, order: c_char) -> c_int {
	if view.is_null() {
		return 0;
	}
	// SAFETY: caller supplied a live `Py_buffer` pointer.
	let view = unsafe { &*view };
	if !view.suboffsets.is_null() {
		return 0;
	}
	if order == b'C' as c_char {
		c_int::from(unsafe { is_c_contiguous(view) })
	} else if order == b'F' as c_char {
		c_int::from(unsafe { is_fortran_contiguous(view) })
	} else if order == b'A' as c_char {
		c_int::from(unsafe { is_c_contiguous(view) || is_fortran_contiguous(view) })
	} else {
		0
	}
}

unsafe extern "C" fn capi_get_buffer(
	object: *mut PyObject,
	view: *mut PyBuffer,
	flags: c_int,
) -> c_int {
	catch_status(|| unsafe { get_buffer_impl(object, view, flags) })
}

unsafe extern "C" fn capi_release_buffer(view: *mut PyBuffer) {
	let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
		if view.is_null() {
			return;
		}
		let object = (*view).obj;
		if !object.is_null() {
			if let Some(releasebuffer) = foreign_releasebuffer(object) {
				// SAFETY: foreign slot ABI is `releasebufferproc`; the extension owns the
				// callback.
				releasebuffer(object, view);
			}
		}
		zero_buffer(view);
	}));
}

unsafe extern "C" fn capi_buffer_fill_info(
	view: *mut PyBuffer,
	object: *mut PyObject,
	buf: *mut c_void,
	len: PySsizeT,
	readonly: c_int,
	flags: c_int,
) -> c_int {
	catch_status(|| unsafe {
		fill_buffer_info_impl(view, normalize_object_arg(object), buf, len, readonly, flags)
	})
}

unsafe extern "C" fn capi_buffer_is_contiguous(view: *const PyBuffer, order: c_char) -> c_int {
	catch_status(|| unsafe { is_buffer_contiguous_impl(view, order) })
}

unsafe extern "C" fn capi_check_buffer(object: *mut PyObject) -> c_int {
	catch_status(|| c_int::from(unsafe { object_supports_buffer(object) }))
}

unsafe extern "C" fn capi_memoryview_from_object(object: *mut PyObject) -> *mut PyObject {
	catch_object(|| unsafe {
		let object = normalize_object_arg(object);
		if foreign_getbuffer(object).is_some() {
			return abi::exc::raise_kind_error_text(
				ExceptionKind::NotImplementedError,
				"PyMemoryView_FromObject for foreign buffer exporters is not implemented yet",
			);
		}
		if let Err(message) = crate::abi::str_::install_memoryview_slots() {
			return abi::return_null_with_error(message);
		}
		match crate::types::memoryview::boxed_memoryview_from_object(object) {
			Ok(view) => view.cast::<PyObject>(),
			Err(message) if message == crate::types::memoryview::RELEASED_ERROR => {
				abi::exc::raise_kind_error_text(ExceptionKind::ValueError, &message)
			},
			Err(message) => abi::exc::raise_kind_error_text(ExceptionKind::TypeError, &message),
		}
	})
}

unsafe extern "C" fn capi_memoryview_from_buffer(_view: *const PyBuffer) -> *mut PyObject {
	catch_object(|| {
		abi::exc::raise_kind_error_text(
			ExceptionKind::NotImplementedError,
			"PyMemoryView_FromBuffer is not implemented yet",
		)
	})
}

unsafe extern "C" fn capi_memoryview_from_memory(
	_mem: *mut c_char,
	_size: PySsizeT,
	_flags: c_int,
) -> *mut PyObject {
	catch_object(|| {
		abi::exc::raise_kind_error_text(
			ExceptionKind::NotImplementedError,
			"PyMemoryView_FromMemory is not implemented yet: wrapping raw foreign memory is not safe \
			 under Pon",
		)
	})
}

fn memoryview_format_pointer(format: u8) -> *mut c_char {
	match format {
		b'I' => BUFFER_FORMAT_I.as_ptr().cast::<c_char>().cast_mut(),
		_ => BUFFER_FORMAT_B.as_ptr().cast::<c_char>().cast_mut(),
	}
}

unsafe fn memoryview_ref(
	object: *mut PyObject,
) -> Option<&'static crate::types::memoryview::PyMemoryView> {
	let object = normalize_object_arg(object);
	if object.is_null() || crate::tag::is_small_int(object) || !crate::tag::is_heap(object) {
		return None;
	}
	// SAFETY: heap-tagged objects have a readable Pon object header.
	let ty = unsafe { (*object).ob_type };
	if !crate::types::memoryview::is_memoryview_type(ty) {
		return None;
	}
	// SAFETY: exact type check proves the boxed memoryview layout.
	Some(unsafe { &*object.cast::<crate::types::memoryview::PyMemoryView>() })
}

unsafe extern "C" fn capi_memoryview_get_buffer(object: *mut PyObject) -> *mut PyBuffer {
	let object = normalize_object_arg(object);
	let Some(view) = (unsafe { memoryview_ref(object) }) else {
		return ptr::null_mut();
	};
	if view.released {
		return ptr::null_mut();
	}
	MEMORYVIEW_BUFFER_CACHE.with(|cache| {
		let mut cache = cache.borrow_mut();
		let entry = cache
			.entry(object as usize)
			.or_insert_with(|| Box::new(PyBuffer::empty()));
		entry.obj = view.base;
		entry.buf = view.data.cast::<c_void>();
		entry.len = view.len as PySsizeT;
		entry.itemsize = view.itemsize() as PySsizeT;
		entry.readonly = c_int::from(view.readonly);
		entry.ndim = 1;
		entry.format = memoryview_format_pointer(view.format);
		entry.shape = ptr::addr_of_mut!(entry.len);
		entry.strides = ptr::addr_of_mut!(entry.itemsize);
		entry.suboffsets = ptr::null_mut();
		entry.internal = ptr::null_mut();
		entry.as_mut() as *mut PyBuffer
	})
}

unsafe extern "C" fn capi_memoryview_get_base(object: *mut PyObject) -> *mut PyObject {
	match unsafe { memoryview_ref(object) } {
		Some(view) if !view.released => view.base,
		_ => ptr::null_mut(),
	}
}

unsafe extern "C" fn capi_type_check(object: *mut PyObject) -> c_int {
	let object = normalize_object_arg(object);
	if object.is_null() {
		return 0;
	}
	if let Some(native) = twin::registered_native_of_foreign(object.cast::<ForeignTypeObject>()) {
		return c_int::from(unsafe {
			crate::types::type_::is_type_object(native.cast::<PyObject>())
		});
	}
	if crate::tag::is_small_int(object) || !crate::tag::is_heap(object) {
		return 0;
	}
	c_int::from(unsafe { crate::types::type_::is_type_object(object) })
}

unsafe extern "C" fn capi_iter_check(object: *mut PyObject) -> c_int {
	let object = normalize_object_arg(object);
	if object.is_null() || crate::tag::is_small_int(object) || !crate::tag::is_heap(object) {
		return 0;
	}
	let ty = unsafe { (*object).ob_type };
	if ty.is_null() {
		return 0;
	}
	c_int::from(unsafe {
		(*ty).tp_iternext.is_some()
			|| (*ty)
				.tp_as_sequence
				.as_ref()
				.is_some_and(|methods| methods.sq_iternext.is_some())
	})
}

unsafe extern "C" fn capi_generic_get_attr(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	catch_object(|| {
		let object = normalize_object_arg(object);
		let name = normalize_object_arg(name);
		unsafe { crate::descr::generic_get_attr(object, name) }
	})
}

unsafe extern "C" fn capi_generic_set_attr(
	object: *mut PyObject,
	name: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	catch_status(|| {
		let object = normalize_object_arg(object);
		let name = normalize_object_arg(name);
		let value = normalize_object_arg(value);
		unsafe { crate::descr::generic_set_attr(object, name, value) }
	})
}

unsafe extern "C" fn capi_generic_get_dict(
	object: *mut PyObject,
	_context: *mut c_void,
) -> *mut PyObject {
	catch_object(|| {
		let object = normalize_object_arg(object);
		if object.is_null() {
			return abi::return_null_with_error("PyObject_GenericGetDict(NULL)");
		}
		// SAFETY: normalized live receiver.
		let ty = unsafe { (*object).ob_type };
		if unsafe { crate::capi::is_capi_class(ty) } && unsafe { (*ty).tp_dictoffset } > 0 {
			// Read the tp_dictoffset slot DIRECTLY: routing through attribute
			// lookup re-enters the type's own `__dict__` getset (numpy wires
			// it to this very function) and recurses to a stack overflow.
			return match unsafe { crate::descr::ensure_capi_instance_dict(object, ty) } {
				Ok(dict) if !dict.is_null() => dict,
				Ok(_) => abi::exc::raise_kind_error_text(
					ExceptionKind::NotImplementedError,
					"PyObject_GenericGetDict receiver has no instance dict slot",
				),
				Err(message) => abi::return_null_with_error(message),
			};
		}
		let name = unsafe { abi::pon_const_str(b"__dict__".as_ptr(), b"__dict__".len()) };
		if name.is_null() {
			return ptr::null_mut();
		}
		let result = unsafe { crate::descr::generic_get_attr(object, name) };
		if !result.is_null() {
			return result;
		}
		if abi::exc::pending_exception_is("AttributeError") || !pon_err_occurred() {
			pon_err_clear();
			return abi::exc::raise_kind_error_text(
				ExceptionKind::NotImplementedError,
				"PyObject_GenericGetDict is only implemented for objects with a Pon-managed __dict__ \
				 view",
			);
		}
		ptr::null_mut()
	})
}

unsafe extern "C" fn capi_generic_set_dict(
	object: *mut PyObject,
	value: *mut PyObject,
	_context: *mut c_void,
) -> c_int {
	catch_status(|| {
		let object = normalize_object_arg(object);
		let value = normalize_object_arg(value);
		let name = unsafe { abi::pon_const_str(b"__dict__".as_ptr(), b"__dict__".len()) };
		if name.is_null() {
			return -1;
		}
		let status = unsafe { crate::descr::generic_set_attr(object, name, value) };
		if status < 0 && (abi::exc::pending_exception_is("AttributeError") || !pon_err_occurred()) {
			pon_err_clear();
			return raise_status(
				ExceptionKind::NotImplementedError,
				"PyObject_GenericSetDict is only implemented for objects with a Pon-managed __dict__ \
				 view",
			);
		}
		status
	})
}

unsafe extern "C" fn capi_print(
	object: *mut PyObject,
	file: *mut libc::FILE,
	flags: c_int,
) -> c_int {
	catch_status(|| {
		if file.is_null() {
			return type_error_status("PyObject_Print received NULL FILE*");
		}
		let rendered = if flags & PY_PRINT_RAW != 0 {
			unsafe { capi_str(object) }
		} else {
			unsafe { capi_repr(object) }
		};
		if rendered.is_null() {
			return -1;
		}
		let Some(text) = (unsafe { crate::types::type_::unicode_text(rendered) }) else {
			super::unpin_object(rendered);
			return type_error_status("PyObject_Print rendering did not produce str");
		};
		let c_text = match CString::new(text.as_bytes()) {
			Ok(text) => text,
			Err(_) => {
				super::unpin_object(rendered);
				return value_error_status(
					"PyObject_Print cannot write strings containing NUL through fputs",
				);
			},
		};
		let status = if unsafe { libc::fputs(c_text.as_ptr(), file) } == libc::EOF {
			raise_status(ExceptionKind::OSError, "PyObject_Print failed to write to FILE*")
		} else {
			0
		};
		super::unpin_object(rendered);
		status
	})
}

unsafe extern "C" fn capi_format(
	object: *mut PyObject,
	format_spec: *mut PyObject,
) -> *mut PyObject {
	catch_object(|| {
		let object = normalize_object_arg(object);
		let spec = if format_spec.is_null() {
			""
		} else {
			let format_spec = normalize_object_arg(format_spec);
			let Some(spec) = (unsafe { crate::types::type_::unicode_text(format_spec) }) else {
				return raise_type_error("PyObject_Format format_spec must be str or NULL");
			};
			spec
		};
		match abi::str_::format_object_with_spec(object, spec) {
			Ok(text) => unsafe { abi::pon_const_str(text.as_ptr(), text.len()) },
			Err(message) => abi::return_null_with_error(message),
		}
	})
}

unsafe extern "C" fn capi_clear_weakrefs(object: *mut PyObject) {
	let _ = catch_unwind(AssertUnwindSafe(|| {
		let object = normalize_object_arg(object);
		if object.is_null() {
			return;
		}
		// Pon heap/C-extension instances share the runtime weakref registry;
		// objects without a registry row are a documented no-op here.
		crate::types::weakref::clear_weakrefs(object);
	}));
}

/// Pon has no CPython split managed-dict storage: instance namespaces are
/// traced directly by the GC, so there is no C-level dict pointer to clear
/// during cycle breaking. Keep the API as an explicit no-op for extension
/// tp_clear code.
unsafe extern "C" fn capi_clear_managed_dict(_object: *mut PyObject) -> c_int {
	0
}

unsafe extern "C" fn capi_visit_managed_dict(
	object: *mut PyObject,
	visit: VisitProc,
	arg: *mut c_void,
) -> c_int {
	catch_status(|| {
		let object = normalize_object_arg(object);
		if object.is_null() {
			return 0;
		}
		let name = unsafe { abi::pon_const_str(b"__dict__".as_ptr(), b"__dict__".len()) };
		if name.is_null() {
			return -1;
		}
		let dict = unsafe { crate::descr::generic_get_attr(object, name) };
		if dict.is_null() {
			if abi::exc::pending_exception_is("AttributeError") || !pon_err_occurred() {
				pon_err_clear();
				return 0;
			}
			return -1;
		}
		super::pin_object(dict);
		let status = unsafe { visit(dict, arg) };
		super::unpin_object(dict);
		status
	})
}

unsafe extern "C" fn capi_cfunction_new_ex(
	method_def: *mut super::PyMethodDef,
	self_object: *mut PyObject,
	_module: *mut PyObject,
) -> *mut PyObject {
	catch_object(|| {
		let method_def_ptr = method_def;
		let Some(method_def) = (unsafe { method_def_ptr.as_ref() }) else {
			return raise_type_error("PyCFunction_NewEx method definition must not be NULL");
		};
		if method_def.ml_meth.is_none() {
			return raise_type_error("PyCFunction_NewEx ml_meth must not be NULL");
		}
		let Some(name) = c_string(method_def.ml_name) else {
			return raise_type_error("PyCFunction_NewEx ml_name must not be NULL");
		};
		let self_object = normalize_object_arg(self_object);
		super::alloc_cfunction_from_method_def(method_def_ptr, self_object, &name)
	})
}

unsafe extern "C" fn capi_seq_iter_new(sequence: *mut PyObject) -> *mut PyObject {
	catch_object(|| {
		let sequence = normalize_object_arg(sequence);
		if sequence.is_null() {
			return raise_type_error("PySeqIter_New received NULL sequence");
		}
		crate::types::lazy_iter::new_seq_iter(sequence)
	})
}

unsafe extern "C" fn capi_method_new(
	function: *mut PyObject,
	receiver: *mut PyObject,
) -> *mut PyObject {
	catch_object(|| {
		let function = normalize_object_arg(function);
		let receiver = normalize_object_arg(receiver);
		match crate::types::method::new_bound_method(function, receiver) {
			Ok(method) => method.cast::<PyObject>(),
			Err(message) => abi::return_null_with_error(message),
		}
	})
}

#[cfg(test)]
mod tests {
	use core::ptr;

	use super::super::tests::{ResetImportStateOnDrop, TempExtensionRoot, compile_extension};
	use crate::{
		abi,
		abi::{
			exc, format_object_for_print as format_object, pon_call, pon_const_int, pon_runtime_init,
		},
		import::module_attr,
		intern::intern,
		object::PyObject,
		thread_state::{pon_err_clear, pon_err_message, test_state_lock},
	};

	#[test]
	fn object_family_extension_exercises_protocol_surface() {
		let _guard = test_state_lock();
		let _reset = ResetImportStateOnDrop;
		unsafe {
			assert_eq!(pon_runtime_init(), 0);
		}
		let temp = TempExtensionRoot::new();
		let module_path = compile_extension(
			&temp,
			"capi_object_test_ext",
			r#"
#include <Python.h>

static PyObject *one_arg_bit_length(PyObject *self, PyObject *arg) {
    (void)self;
    PyObject *builtins = PyObject_GetAttrString(arg, "__class__");
    if (builtins == NULL) {
        PyErr_Clear();
    }
    PyObject *method_name = PyUnicode_FromString("bit_length");
    if (method_name == NULL) {
        return NULL;
    }
    PyObject *method = PyObject_GetAttr(arg, method_name);
    if (method == NULL) {
        return NULL;
    }
    return PyObject_CallNoArgs(method);
}

static PyObject *call_one_arg(PyObject *self, PyObject *callable) {
    (void)self;
    PyObject *value = PyLong_FromLong(-7);
    if (value == NULL) {
        return NULL;
    }
    return PyObject_CallOneArg(callable, value);
}

static PyObject *varargs_calls(PyObject *self, PyObject *callable) {
    (void)self;
    PyObject *value = PyLong_FromLong(-11);
    if (value == NULL) {
        return NULL;
    }
    PyObject *called = PyObject_CallFunctionObjArgs(callable, value, NULL);
    if (called == NULL) {
        return NULL;
    }
    long first = PyLong_AsLong(called);
    if (PyErr_Occurred()) {
        return NULL;
    }
    PyObject *method_name = PyUnicode_FromString("bit_length");
    PyObject *receiver = PyLong_FromLong(15);
    if (method_name == NULL || receiver == NULL) {
        return NULL;
    }
    PyObject *method_result = PyObject_CallMethodObjArgs(receiver, method_name, NULL);
    if (method_result == NULL) {
        return NULL;
    }
    long second = PyLong_AsLong(method_result);
    if (PyErr_Occurred()) {
        return NULL;
    }
    return PyLong_FromLong(first + second);
}

static PyObject *module_attrs(PyObject *self, PyObject *module_obj) {
    (void)self;
    PyObject *value = PyLong_FromLong(123);
    if (value == NULL) {
        return NULL;
    }
    if (PyObject_SetAttrString(module_obj, "dynamic_value", value) < 0) {
        return NULL;
    }
    if (!PyObject_HasAttrString(module_obj, "dynamic_value")) {
        PyErr_SetString(PyExc_RuntimeError, "attribute was not set");
        return NULL;
    }
    PyObject *name = PyUnicode_FromString("dynamic_value");
    if (name == NULL) {
        return NULL;
    }
    PyObject *read_back = PyObject_GetAttr(module_obj, name);
    if (read_back == NULL) {
        return NULL;
    }
    if (PyObject_SetAttr(module_obj, name, NULL) < 0) {
        return NULL;
    }
    if (PyObject_HasAttr(module_obj, name)) {
        PyErr_SetString(PyExc_RuntimeError, "attribute was not deleted");
        return NULL;
    }
    return read_back;
}

static PyObject *compare_ints(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    PyObject *left = PyLong_FromLong(3);
    PyObject *right = PyLong_FromLong(5);
    if (left == NULL || right == NULL) {
        return NULL;
    }
    int lt = PyObject_RichCompareBool(left, right, Py_LT);
    int ge = PyObject_RichCompareBool(left, right, Py_GE);
    if (lt < 0 || ge < 0) {
        return NULL;
    }
    return PyLong_FromLong((lt == 1 && ge == 0) ? 1 : 0);
}

static PyObject *iterate_and_sum(PyObject *self, PyObject *iterable) {
    (void)self;
    PyObject *iter = PyObject_GetIter(iterable);
    if (iter == NULL) {
        return NULL;
    }
    long total = 0;
    PyObject *item;
    while ((item = PyIter_Next(iter)) != NULL) {
        total += PyLong_AsLong(item);
        if (PyErr_Occurred()) {
            return NULL;
        }
    }
    if (PyErr_Occurred()) {
        return NULL;
    }
    return PyLong_FromLong(total);
}

static PyObject *is_value_error(PyObject *self, PyObject *obj) {
    (void)self;
    int result = PyObject_IsInstance(obj, PyExc_ValueError);
    if (result < 0) {
        return NULL;
    }
    return PyLong_FromLong(result);
}

static PyObject *type_is_value_error(PyObject *self, PyObject *obj) {
    (void)self;
    PyObject *ty = PyObject_Type(obj);
    if (ty == NULL) {
        return NULL;
    }
    int result = PyObject_IsSubclass(ty, PyExc_ValueError);
    if (result < 0) {
        return NULL;
    }
    return PyLong_FromLong(result);
}

static PyObject *repr_and_str_truth(PyObject *self, PyObject *obj) {
    (void)self;
    PyObject *repr_obj = PyObject_Repr(obj);
    PyObject *str_obj = PyObject_Str(obj);
    if (repr_obj == NULL || str_obj == NULL) {
        return NULL;
    }
    int truth = PyObject_IsTrue(obj);
    int not_value = PyObject_Not(obj);
    if (truth < 0 || not_value < 0) {
        return NULL;
    }
    return PyLong_FromLong((truth == 1 && not_value == 0) ? 1 : 0);
}

static PyMethodDef methods[] = {
    {"call_one_arg", call_one_arg, METH_O, "call callable with -7"},
    {"varargs_calls", varargs_calls, METH_O, "call varargs object helpers"},
    {"one_arg_bit_length", one_arg_bit_length, METH_O, "call int.bit_length with no args"},
    {"module_attrs", module_attrs, METH_O, "exercise attrs"},
    {"compare_ints", compare_ints, METH_NOARGS, "rich compare ints"},
    {"iterate_and_sum", iterate_and_sum, METH_O, "iterate and sum"},
    {"is_value_error", is_value_error, METH_O, "isinstance against ValueError twin"},
    {"type_is_value_error", type_is_value_error, METH_O, "issubclass against ValueError twin"},
    {"repr_and_str_truth", repr_and_str_truth, METH_O, "repr/str/truth"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_object_test_ext",
    "Pon object C-API test extension",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_object_test_ext(void) {
    PyObject *m = PyModule_Create(&module);
    if (m == NULL) {
        return NULL;
    }
    return m;
}
"#,
		);

		let module = super::super::load_extension_module("capi_object_test_ext", &module_path)
			.unwrap_or_else(|message| panic!("failed to load object C extension: {message}"));
		assert!(!module.is_null(), "extension loader returned NULL module");

		let module_name = intern("capi_object_test_ext");
		let module_attrs =
			module_attr(module_name, intern("module_attrs")).expect("module_attrs method registered");
		let mut argv = [module];
		let result = unsafe { pon_call(module_attrs, argv.as_mut_ptr(), 1) };
		assert_eq!(format_object(result).as_deref(), Ok("123"));

		let compare =
			module_attr(module_name, intern("compare_ints")).expect("compare_ints method registered");
		let result = unsafe { pon_call(compare, ptr::null_mut(), 0) };
		assert_eq!(format_object(result).as_deref(), Ok("1"));

		let iterate = module_attr(module_name, intern("iterate_and_sum"))
			.expect("iterate_and_sum method registered");
		let mut list_items =
			[unsafe { pon_const_int(2) }, unsafe { pon_const_int(4) }, unsafe { pon_const_int(6) }];
		let list = unsafe { abi::seq::pon_build_list(list_items.as_mut_ptr(), list_items.len()) };
		assert!(!list.is_null(), "failed to build runtime list: {:?}", pon_err_message());
		let mut argv = [list];
		let result = unsafe { pon_call(iterate, argv.as_mut_ptr(), 1) };
		assert_eq!(format_object(result).as_deref(), Ok("12"));

		let call_one_arg =
			module_attr(module_name, intern("call_one_arg")).expect("call_one_arg method registered");
		let abs_builtin = unsafe { abi::pon_load_builtin(intern("abs")) };
		assert!(!abs_builtin.is_null(), "failed to load abs builtin: {:?}", pon_err_message());
		let mut argv = [abs_builtin];
		let result = unsafe { pon_call(call_one_arg, argv.as_mut_ptr(), 1) };
		assert_eq!(format_object(result).as_deref(), Ok("7"));

		let varargs_calls = module_attr(module_name, intern("varargs_calls"))
			.expect("varargs_calls method registered");
		let mut argv = [abs_builtin];
		let result = unsafe { pon_call(varargs_calls, argv.as_mut_ptr(), 1) };
		assert_eq!(format_object(result).as_deref(), Ok("15"));

		let one_arg_bit_length = module_attr(module_name, intern("one_arg_bit_length"))
			.expect("one_arg_bit_length method registered");
		let negative = unsafe { pon_const_int(-8) };
		let mut argv = [negative];
		let result = unsafe { pon_call(one_arg_bit_length, argv.as_mut_ptr(), 1) };
		assert_eq!(format_object(result).as_deref(), Ok("4"));

		let value_error_type =
			abi::exception_type_object(crate::types::exc::ExceptionKind::ValueError)
				.cast::<PyObject>();
		let message =
			unsafe { abi::pon_const_str(b"raised instance".as_ptr(), b"raised instance".len()) };
		let mut exc_argv = [message];
		let instance = unsafe { pon_call(value_error_type, exc_argv.as_mut_ptr(), exc_argv.len()) };
		assert!(
			!instance.is_null(),
			"failed to construct ValueError instance: {:?}",
			pon_err_message()
		);
		let _ = unsafe { exc::pon_raise(instance, ptr::null_mut()) };
		assert!(exc::pending_exception_is("ValueError"), "raised instance should be pending");
		pon_err_clear();

		let is_value_error = module_attr(module_name, intern("is_value_error"))
			.expect("is_value_error method registered");
		let mut argv = [instance];
		let result = unsafe { pon_call(is_value_error, argv.as_mut_ptr(), 1) };
		assert_eq!(format_object(result).as_deref(), Ok("1"));

		let type_is_value_error = module_attr(module_name, intern("type_is_value_error"))
			.expect("type_is_value_error method registered");
		let mut argv = [instance];
		let result = unsafe { pon_call(type_is_value_error, argv.as_mut_ptr(), 1) };
		assert_eq!(format_object(result).as_deref(), Ok("1"));

		let repr_truth = module_attr(module_name, intern("repr_and_str_truth"))
			.expect("repr_and_str_truth method registered");
		let value = unsafe { pon_const_int(9) };
		let mut argv = [value];
		let result = unsafe { pon_call(repr_truth, argv.as_mut_ptr(), 1) };
		assert_eq!(format_object(result).as_deref(), Ok("1"));
	}

	#[test]
	fn protocol_gap_extension_exercises_owned_refs_and_varargs() {
		let _guard = test_state_lock();
		let _reset = ResetImportStateOnDrop;
		unsafe {
			assert_eq!(pon_runtime_init(), 0);
		}

		let temp = TempExtensionRoot::new();
		let module_path = compile_extension(
			&temp,
			"capi_protocol_gap_ext",
			r#"
#include <Python.h>
#include <limits.h>

#define BIT(n) (1L << (n))
static PyObject *module_ref = NULL;


static int long_equals(PyObject *object, long expected) {
    if (object == NULL) {
        if (PyErr_Occurred()) PyErr_Clear();
        return 0;
    }
    long value = PyLong_AsLong(object);
    if (PyErr_Occurred()) {
        PyErr_Clear();
        return 0;
    }
    return value == expected;
}

static PyObject *sum_two(PyObject *self, PyObject *args) {
    (void)self;
    int left = 0;
    int right = 0;
    if (!PyArg_ParseTuple(args, "ii", &left, &right)) {
        return NULL;
    }
    return PyLong_FromLong((long)(left + right));
}

static PyObject *no_args(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    return PyLong_FromLong(23);
}

static PyObject *returns_fd(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    return PyLong_FromLong(13);
}

static PyObject *drive(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    PyObject *module_obj = module_ref;
    long mask = 0;

    PyObject *too_big = PyLong_FromUnsignedLongLong((unsigned long long)LONG_MAX + 1ULL);
    int overflow = -99;
    long as_long = PyLong_AsLongAndOverflow(too_big, &overflow);
    if (as_long == -1 && overflow == 1 && PyErr_Occurred() == NULL) {
        mask |= BIT(0);
    } else {
        PyErr_Clear();
    }

    PyObject *dict = PyDict_New();
    PyObject *key = PyUnicode_FromString("answer");
    PyObject *missing = PyUnicode_FromString("missing");
    PyObject *value = PyLong_FromLong(42);
    PyObject *ref = NULL;
    if (dict != NULL && key != NULL && missing != NULL && value != NULL && PyDict_SetItem(dict, key, value) == 0) {
        int status = PyDict_GetItemRef(dict, key, &ref);
        if (status == 1 && long_equals(ref, 42)) {
            mask |= BIT(1);
        }
        Py_XDECREF(ref);
        ref = (PyObject *)0x1;
        status = PyDict_GetItemRef(dict, missing, &ref);
        if (status == 0 && ref == NULL && PyErr_Occurred() == NULL) {
            mask |= BIT(2);
        } else {
            PyErr_Clear();
        }
        status = PyDict_GetItemRef(value, key, &ref);
        if (status == -1 && PyErr_Occurred() != NULL) {
            mask |= BIT(3);
        }
        PyErr_Clear();
    } else {
        PyErr_Clear();
    }

    PyObject *attr_name = PyUnicode_FromString("dynamic_value");
    PyObject *missing_attr = PyUnicode_FromString("definitely_missing");
    PyObject *attr_value = PyLong_FromLong(77);
    PyObject *optional = NULL;
    if (module_obj != NULL && attr_name != NULL && missing_attr != NULL && attr_value != NULL && PyObject_SetAttr(module_obj, attr_name, attr_value) == 0) {
        int status = PyObject_GetOptionalAttr(module_obj, attr_name, &optional);
        if (status == 1 && long_equals(optional, 77)) {
            mask |= BIT(4);
        }
        Py_XDECREF(optional);
        optional = (PyObject *)0x1;
        status = PyObject_GetOptionalAttr(module_obj, missing_attr, &optional);
        if (status == 0 && optional == NULL && PyErr_Occurred() == NULL) {
            mask |= BIT(5);
        } else {
            PyErr_Clear();
        }
    } else {
        PyErr_Clear();
    }

    PyObject *sum = module_obj == NULL ? NULL : PyObject_GetAttrString(module_obj, "sum_two");
    if (sum != NULL) {
        PyObject *called = PyObject_CallFunction(sum, "(ii)", 2, 5);
        if (long_equals(called, 7)) {
            mask |= BIT(6);
        }
        Py_XDECREF(called);

        PyObject *left = PyLong_FromLong(4);
        PyObject *right = PyLong_FromLong(5);
        called = PyObject_CallFunctionObjArgs(sum, left, right, NULL);
        if (long_equals(called, 9)) {
            mask |= BIT(8);
        }
        Py_XDECREF(called);
        Py_XDECREF(left);
        Py_XDECREF(right);
    } else {
        PyErr_Clear();
    }
    Py_XDECREF(sum);

    PyObject *called = module_obj == NULL ? NULL : PyObject_CallMethod(module_obj, "no_args", NULL);
    if (long_equals(called, 23)) {
        mask |= BIT(7);
    }
    Py_XDECREF(called);

    if (PyObject_AsFileDescriptor(PyLong_FromLong(8)) == 8 && PyErr_Occurred() == NULL) {
        mask |= BIT(9);
    } else {
        PyErr_Clear();
    }
    PyObject *fd_func = module_obj == NULL ? NULL : PyObject_GetAttrString(module_obj, "returns_fd");
    if (fd_func != NULL && PyObject_SetAttrString(module_obj, "fileno", fd_func) == 0) {
        int fd = PyObject_AsFileDescriptor(module_obj);
        if (fd == 13 && PyErr_Occurred() == NULL) {
            mask |= BIT(10);
        } else {
            PyErr_Clear();
        }
    } else {
        PyErr_Clear();
    }
    Py_XDECREF(fd_func);
    if (PyObject_AsFileDescriptor(Py_None) == -1 && PyErr_Occurred() != NULL) {
        mask |= BIT(11);
    }
    PyErr_Clear();

    return PyLong_FromLong(mask);
}

static PyMethodDef methods[] = {
    {"drive", drive, METH_NOARGS, "exercise protocol C-API gaps"},
    {"sum_two", sum_two, METH_VARARGS, "sum two ints"},
    {"no_args", no_args, METH_NOARGS, "return a sentinel"},
    {"returns_fd", returns_fd, METH_NOARGS, "return a file descriptor"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_protocol_gap_ext",
    "Pon protocol-gap C-API test extension",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_protocol_gap_ext(void) {
    PyObject *m = PyModule_Create(&module);
    if (m != NULL) {
        Py_INCREF(m);
        module_ref = m;
    }
    return m;
}
"#,
		);

		let module = super::super::load_extension_module("capi_protocol_gap_ext", &module_path)
			.unwrap_or_else(|message| panic!("failed to load protocol gap C extension: {message}"));
		assert!(!module.is_null(), "extension loader returned NULL module");

		let module_name = intern("capi_protocol_gap_ext");
		let drive = module_attr(module_name, intern("drive")).expect("drive method registered");
		let result = unsafe { pon_call(drive, ptr::null_mut(), 0) };
		assert!(!result.is_null(), "drive() returned NULL: {:?}", pon_err_message());
		assert_eq!(
			format_object(result).as_deref(),
			Ok("4095"),
			"protocol gap bitmask mismatch: {:?}",
			pon_err_message()
		);
	}

	#[test]
	fn vectorcall_extension_dispatches_foreign_instance() {
		let _guard = test_state_lock();
		let _reset = ResetImportStateOnDrop;
		unsafe {
			assert_eq!(pon_runtime_init(), 0);
		}

		let temp = TempExtensionRoot::new();
		let module_path = compile_extension(
			&temp,
			"capi_vectorcall_ext",
			r#"
#include <Python.h>

#define BIT(n) (1L << (n))

typedef struct {
    PyObject_HEAD
    vectorcallfunc vectorcall;
} VectorThing;

static PyObject *sum_vectorcall(PyObject *callable, PyObject *const *args, size_t nargsf, PyObject *kwnames) {
    (void)callable;
    if (kwnames != NULL) {
        Py_ssize_t nkw = PyTuple_Size(kwnames);
        if (nkw < 0) {
            return NULL;
        }
    }
    Py_ssize_t nargs = PyVectorcall_NARGS(nargsf);
    long total = 0;
    for (Py_ssize_t i = 0; i < nargs; i++) {
        total += PyLong_AsLong(args[i]);
        if (PyErr_Occurred()) {
            return NULL;
        }
    }
    return PyLong_FromLong(total);
}

static PyObject *VectorThing_new(PyTypeObject *type, PyObject *args, PyObject *kwargs) {
    (void)args;
    (void)kwargs;
    VectorThing *self = (VectorThing *)PyType_GenericAlloc(type, 0);
    if (self != NULL) {
        self->vectorcall = sum_vectorcall;
    }
    return (PyObject *)self;
}

static PyTypeObject VectorThing_Type = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "capi_vectorcall_ext.VectorThing",
    .tp_basicsize = sizeof(VectorThing),
    .tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_VECTORCALL,
    .tp_call = PyVectorcall_Call,
    .tp_new = VectorThing_new,
    .tp_vectorcall_offset = offsetof(VectorThing, vectorcall),
};

static int long_equals(PyObject *object, long expected) {
    if (object == NULL) {
        if (PyErr_Occurred()) PyErr_Clear();
        return 0;
    }
    long value = PyLong_AsLong(object);
    if (PyErr_Occurred()) {
        PyErr_Clear();
        return 0;
    }
    return value == expected;
}

static PyObject *drive(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    long mask = 0;

    if (PyType_Ready(&VectorThing_Type) < 0) {
        PyErr_Clear();
        return PyLong_FromLong(mask);
    }

    PyObject *instance = PyObject_CallNoArgs((PyObject *)&VectorThing_Type);
    if (instance == NULL) {
        PyErr_Clear();
        return PyLong_FromLong(mask);
    }

    PyObject *two = PyLong_FromLong(2);
    PyObject *three = PyLong_FromLong(3);
    PyObject *four = PyLong_FromLong(4);
    PyObject *argv[] = {two, three, four};
    PyObject *called = PyObject_Vectorcall(
            instance, argv, ((size_t)3) | PY_VECTORCALL_ARGUMENTS_OFFSET, NULL);
    if (long_equals(called, 9)) {
        mask |= BIT(0);
    }
    Py_XDECREF(called);

    PyObject *dict_called = PyObject_VectorcallDict(instance, argv, 3, NULL);
    if (long_equals(dict_called, 9)) {
        mask |= BIT(1);
    }
    Py_XDECREF(dict_called);

    PyObject *tuple = PyTuple_New(3);
    if (tuple != NULL
            && PyTuple_SetItem(tuple, 0, PyLong_FromLong(5)) == 0
            && PyTuple_SetItem(tuple, 1, PyLong_FromLong(6)) == 0
            && PyTuple_SetItem(tuple, 2, PyLong_FromLong(7)) == 0) {
        called = PyVectorcall_Call(instance, tuple, NULL);
        if (long_equals(called, 18)) {
            mask |= BIT(2);
        }
        Py_XDECREF(called);
    } else {
        PyErr_Clear();
    }

    if (PyVectorcall_NARGS(PY_VECTORCALL_ARGUMENTS_OFFSET | (size_t)37) == 37) {
        mask |= BIT(3);
    }
    if (PyVectorcall_Function(instance) == sum_vectorcall) {
        mask |= BIT(4);
    }
    if (two != NULL && PyVectorcall_Function(two) == NULL && PyErr_Occurred() == NULL) {
        mask |= BIT(5);
    } else {
        PyErr_Clear();
    }

    return PyLong_FromLong(mask);
}

static PyMethodDef methods[] = {
    {"drive", drive, METH_NOARGS, "exercise vectorcall C-API"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_vectorcall_ext",
    "Pon vectorcall C-API test extension",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_vectorcall_ext(void) {
    return PyModule_Create(&module);
}
"#,
		);

		let module = super::super::load_extension_module("capi_vectorcall_ext", &module_path)
			.unwrap_or_else(|message| panic!("failed to load vectorcall C extension: {message}"));
		assert!(!module.is_null(), "extension loader returned NULL module");

		let drive = module_attr(intern("capi_vectorcall_ext"), intern("drive"))
			.expect("drive method registered");
		let result = unsafe { pon_call(drive, ptr::null_mut(), 0) };
		assert!(!result.is_null(), "drive() returned NULL: {:?}", pon_err_message());
		assert_eq!(
			format_object(result).as_deref(),
			Ok("63"),
			"vectorcall bitmask mismatch: {:?}",
			pon_err_message()
		);
	}

	#[test]
	fn buffer_protocol_c_extension_round_trips_exporters_and_pon_bytes() {
		let _guard = test_state_lock();
		let _reset = ResetImportStateOnDrop;
		unsafe {
			assert_eq!(pon_runtime_init(), 0);
		}

		let temp = TempExtensionRoot::new();
		let module_path = compile_extension(
			&temp,
			"capi_buffer_protocol_ext",
			r#"
#include <Python.h>
#include <string.h>

#define BIT(n) (1L << (n))

typedef struct {
    PyObject_HEAD
} ExporterObject;

static unsigned char exported[8] = {0, 1, 2, 3, 4, 5, 6, 7};
static int release_count = 0;

static int Exporter_getbuffer(PyObject *self, Py_buffer *view, int flags) {
    return PyBuffer_FillInfo(view, self, exported, 8, 0, flags);
}

static void Exporter_releasebuffer(PyObject *self, Py_buffer *view) {
    (void)self;
    (void)view;
    release_count += 1;
}

static PyBufferProcs Exporter_buffer = {
    Exporter_getbuffer,
    Exporter_releasebuffer
};

static PyTypeObject ExporterType = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "capi_buffer_protocol_ext.Exporter",
    .tp_basicsize = sizeof(ExporterObject),
    .tp_flags = Py_TPFLAGS_DEFAULT,
    .tp_new = PyType_GenericNew,
    .tp_as_buffer = &Exporter_buffer,
};

static int view_zeroed(Py_buffer *view) {
    return view->buf == NULL
        && view->obj == NULL
        && view->len == 0
        && view->itemsize == 0
        && view->readonly == 0
        && view->ndim == 0
        && view->format == NULL
        && view->shape == NULL
        && view->strides == NULL
        && view->suboffsets == NULL
        && view->internal == NULL;
}

static PyObject *drive(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    long mask = 0;

    if (PyType_Ready(&ExporterType) < 0) {
        PyErr_Clear();
        return PyLong_FromLong(mask);
    }
    PyObject *exporter = PyObject_CallNoArgs((PyObject *)&ExporterType);
    PyObject *bytes = PyBytes_FromStringAndSize("ponbytes", 8);
    PyObject *bytearray = PyByteArray_FromStringAndSize("mutable", 7);
    PyObject *nonbuffer = PyLong_FromLong(11);

    if (exporter != NULL && bytes != NULL && bytearray != NULL && nonbuffer != NULL
            && PyObject_CheckBuffer(exporter)
            && PyObject_CheckBuffer(bytes)
            && PyObject_CheckBuffer(bytearray)
            && !PyObject_CheckBuffer(nonbuffer)) {
        mask |= BIT(0);
    } else {
        PyErr_Clear();
    }

    Py_buffer view;
    memset(&view, 0x7F, sizeof(view));
    if (exporter != NULL && PyObject_GetBuffer(exporter, &view, PyBUF_SIMPLE) == 0) {
        unsigned char *buf = (unsigned char *)view.buf;
        if (view.obj == exporter
                && view.len == 8
                && view.itemsize == 1
                && view.readonly == 0
                && view.ndim == 1
                && view.format == NULL
                && view.shape == NULL
                && view.strides == NULL
                && view.suboffsets == NULL
                && buf != NULL
                && buf[0] == 0
                && buf[7] == 7
                && PyBuffer_IsContiguous(&view, 'C')
                && PyBuffer_IsContiguous(&view, 'F')
                && PyBuffer_IsContiguous(&view, 'A')) {
            mask |= BIT(1);
        }
        PyBuffer_Release(&view);
        if (release_count == 1 && view_zeroed(&view)) {
            mask |= BIT(2);
        }
    } else {
        PyErr_Clear();
    }

    memset(&view, 0x7F, sizeof(view));
    if (bytes != NULL && PyObject_GetBuffer(bytes, &view, PyBUF_FORMAT | PyBUF_ND | PyBUF_STRIDES) == 0) {
        if (view.obj == bytes
                && view.len == 8
                && view.itemsize == 1
                && view.readonly == 1
                && view.ndim == 1
                && view.format != NULL
                && strcmp(view.format, "B") == 0
                && view.shape != NULL
                && view.shape[0] == 8
                && view.strides != NULL
                && view.strides[0] == 1
                && view.suboffsets == NULL
                && memcmp(view.buf, "ponbytes", 8) == 0
                && PyBuffer_IsContiguous(&view, 'C')
                && PyBuffer_IsContiguous(&view, 'F')
                && PyBuffer_IsContiguous(&view, 'A')) {
            mask |= BIT(3);
        }
        PyBuffer_Release(&view);
    } else {
        PyErr_Clear();
    }

    memset(&view, 0x7F, sizeof(view));
    if (bytes != NULL && PyObject_GetBuffer(bytes, &view, PyBUF_WRITABLE) < 0
            && PyErr_ExceptionMatches(PyExc_BufferError)) {
        PyErr_Clear();
        mask |= BIT(4);
    } else {
        PyErr_Clear();
        if (view.obj != NULL) {
            PyBuffer_Release(&view);
        }
    }

    memset(&view, 0x7F, sizeof(view));
    if (bytearray != NULL && PyObject_GetBuffer(bytearray, &view, PyBUF_WRITEABLE | PyBUF_SIMPLE) == 0) {
        ((char *)view.buf)[0] = 'M';
        PyBuffer_Release(&view);
        char *after = PyByteArray_AsString(bytearray);
        if (after != NULL && memcmp(after, "Mutable", 7) == 0) {
            mask |= BIT(5);
        }
    } else {
        PyErr_Clear();
    }

    memset(&view, 0x7F, sizeof(view));
    if (nonbuffer != NULL && PyObject_GetBuffer(nonbuffer, &view, PyBUF_SIMPLE) < 0
            && PyErr_ExceptionMatches(PyExc_TypeError)) {
        PyErr_Clear();
        mask |= BIT(6);
    } else {
        PyErr_Clear();
        if (view.obj != NULL) {
            PyBuffer_Release(&view);
        }
    }

    Py_XDECREF(exporter);
    Py_XDECREF(bytes);
    Py_XDECREF(bytearray);
    Py_XDECREF(nonbuffer);
    if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }
    return PyLong_FromLong(mask);
}

static PyMethodDef methods[] = {
    {"drive", drive, METH_NOARGS, "exercise buffer protocol C-API"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_buffer_protocol_ext",
    "Pon buffer protocol C-API test extension",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_buffer_protocol_ext(void) {
    return PyModule_Create(&module);
}
"#,
		);

		let module = super::super::load_extension_module("capi_buffer_protocol_ext", &module_path)
			.unwrap_or_else(|message| panic!("failed to load buffer protocol C extension: {message}"));
		assert!(!module.is_null(), "extension loader returned NULL module");

		let drive = module_attr(intern("capi_buffer_protocol_ext"), intern("drive"))
			.expect("drive method registered");
		let result = unsafe { pon_call(drive, ptr::null_mut(), 0) };
		assert!(!result.is_null(), "drive() returned NULL: {:?}", pon_err_message());
		assert_eq!(
			format_object(result).as_deref(),
			Ok("127"),
			"buffer protocol bitmask mismatch: {:?}",
			pon_err_message()
		);
	}

	#[test]
	fn object_container_protocol_completion_extension_reports_full_bitmask() {
		let _guard = test_state_lock();
		let _reset = ResetImportStateOnDrop;
		unsafe {
			assert_eq!(pon_runtime_init(), 0);
		}

		let temp = TempExtensionRoot::new();
		let module_path = compile_extension(
			&temp,
			"capi_object_container_completion_ext",
			r#"
#include <Python.h>
#include <structmember.h>

#define BIT(n) (1L << (n))

typedef struct {
    PyObject_HEAD
    long value;
} CounterObject;

static PyObject *module_ref = NULL;

static int long_equals(PyObject *object, long expected) {
    if (object == NULL) {
        if (PyErr_Occurred() != NULL) {
            PyErr_Clear();
        }
        return 0;
    }
    long value = PyLong_AsLong(object);
    if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
        return 0;
    }
    return value == expected;
}

static PyObject *Counter_new(PyTypeObject *type, PyObject *args, PyObject *kwds) {
    (void)args;
    (void)kwds;
    CounterObject *self = (CounterObject *)type->tp_alloc(type, 0);
    if (self != NULL) {
        self->value = 11;
    }
    return (PyObject *)self;
}

static PyObject *Counter_add(PyObject *self, PyObject *arg) {
    long addend = PyLong_AsLong(arg);
    if (PyErr_Occurred() != NULL) {
        return NULL;
    }
    return PyLong_FromLong(((CounterObject *)self)->value + addend);
}

static PyObject *Counter_get_twice(PyObject *self, void *closure) {
    (void)closure;
    return PyLong_FromLong(((CounterObject *)self)->value * 2);
}

static PyMethodDef Counter_methods[] = {
    {"add", Counter_add, METH_O, "add to the counter value"},
    {NULL, NULL, 0, NULL}
};

static PyMemberDef Counter_members[] = {
    {"value", T_LONG, offsetof(CounterObject, value), 0, "current count"},
    {NULL, 0, 0, 0, NULL}
};

static PyGetSetDef Counter_getset[] = {
    {"twice", Counter_get_twice, NULL, "value doubled", NULL},
    {NULL, NULL, NULL, NULL, NULL}
};

static PyTypeObject CounterType = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "capi_object_container_completion_ext.Counter",
    .tp_basicsize = sizeof(CounterObject),
    .tp_flags = Py_TPFLAGS_DEFAULT,
    .tp_methods = Counter_methods,
    .tp_members = Counter_members,
    .tp_getset = Counter_getset,
    .tp_new = Counter_new,
};

static PyObject *echo_arg_plus_one(PyObject *self, PyObject *arg) {
    (void)self;
    long value = PyLong_AsLong(arg);
    if (PyErr_Occurred() != NULL) {
        return NULL;
    }
    return PyLong_FromLong(value + 1);
}

static PyObject *drive(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    long mask = 0;

    PyObject *int_instance = PyLong_FromLong(5);
    if (PyType_Check((PyObject *)&PyLong_Type) && int_instance != NULL && !PyType_Check(int_instance)) {
        mask |= BIT(0);
    }
    Py_XDECREF(int_instance);
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    PyObject *counter = PyObject_CallNoArgs((PyObject *)&CounterType);
    PyObject *value_name = PyUnicode_FromString("value");
    PyObject *twice_name = PyUnicode_FromString("twice");
    if (counter != NULL && value_name != NULL) {
        PyObject *value = PyObject_GenericGetAttr(counter, value_name);
        if (long_equals(value, 11)) {
            mask |= BIT(1);
        }
        Py_XDECREF(value);
    }
    if (PyErr_Occurred() != NULL) PyErr_Clear();
    if (counter != NULL && twice_name != NULL) {
        PyObject *twice = PyObject_GenericGetAttr(counter, twice_name);
        if (long_equals(twice, 22)) {
            mask |= BIT(2);
        }
        Py_XDECREF(twice);
    }
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    PyObject *add_name = PyUnicode_FromString("add");
    PyObject *five = PyLong_FromLong(5);
    if (counter != NULL && add_name != NULL && five != NULL) {
        PyObject *called = PyObject_CallMethodOneArg(counter, add_name, five);
        if (long_equals(called, 16)) {
            mask |= BIT(3);
        }
        Py_XDECREF(called);
    }
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    PyObject *dict = PyDict_New();
    PyObject *key = PyUnicode_FromString("alpha");
    PyObject *default_value = PyLong_FromLong(10);
    PyObject *replacement = PyLong_FromLong(20);
    PyObject *ref = NULL;
    if (dict != NULL && key != NULL && default_value != NULL
            && PyDict_SetDefaultRef(dict, key, default_value, &ref) == 0
            && long_equals(ref, 10)) {
        mask |= BIT(4);
    }
    Py_XDECREF(ref);
    ref = NULL;
    if (dict != NULL && key != NULL && replacement != NULL
            && PyDict_SetDefaultRef(dict, key, replacement, &ref) == 1
            && long_equals(ref, 10)) {
        mask |= BIT(5);
    }
    Py_XDECREF(ref);
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    PyObject *remove_value = PyLong_FromLong(30);
    if (dict != NULL && remove_value != NULL
            && PyDict_SetItemString(dict, "remove_me", remove_value) == 0
            && PyDict_DelItemString(dict, "remove_me") == 0
            && PyDict_GetItemString(dict, "remove_me") == NULL) {
        mask |= BIT(6);
    }
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    PyObject *present_value = PyLong_FromLong(31);
    if (dict != NULL && present_value != NULL
            && PyDict_SetItemString(dict, "present", present_value) == 0
            && PyDict_ContainsString(dict, "present") == 1
            && PyDict_ContainsString(dict, "absent") == 0) {
        mask |= BIT(7);
    }
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    PyObject *proxy_value = PyLong_FromLong(44);
    PyObject *proxy_key = PyUnicode_FromString("proxied");
    PyObject *mutation_value = PyLong_FromLong(55);
    if (dict != NULL && proxy_value != NULL && proxy_key != NULL && mutation_value != NULL
            && PyDict_SetItemString(dict, "proxied", proxy_value) == 0) {
        PyObject *proxy = PyDictProxy_New(dict);
        PyObject *got = proxy == NULL ? NULL : PyObject_GetItem(proxy, proxy_key);
        if (long_equals(got, 44) && PyObject_SetItem(proxy, proxy_key, mutation_value) < 0
                && PyErr_Occurred() != NULL) {
            PyErr_Clear();
            mask |= BIT(8);
        } else if (PyErr_Occurred() != NULL) {
            PyErr_Clear();
        }
        Py_XDECREF(got);
        Py_XDECREF(proxy);
    }
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    PyObject *minus_two = PyLong_FromLong(-2);
    PyObject *slice = minus_two == NULL ? NULL : PySlice_New(Py_None, Py_None, minus_two);
    Py_ssize_t start = 0;
    Py_ssize_t stop = 0;
    Py_ssize_t step = 0;
    Py_ssize_t slicelength = 0;
    if (slice != NULL
            && PySlice_GetIndicesEx(slice, 5, &start, &stop, &step, &slicelength) == 0
            && start == 4
            && stop == -1
            && step == -2
            && slicelength == 3) {
        mask |= BIT(9);
    }
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    PyObject *list = PyList_New(0);
    if (list != NULL) {
        PyObject *one = PyLong_FromLong(1);
        PyObject *two = PyLong_FromLong(2);
        PyObject *three = PyLong_FromLong(3);
        if (one != NULL && two != NULL && three != NULL
                && PyList_Append(list, one) == 0
                && PyList_Append(list, two) == 0
                && PyList_Append(list, three) == 0) {
            PyObject *iter = PySeqIter_New(list);
            long total = 0;
            long count = 0;
            PyObject *item;
            while (iter != NULL && (item = PyIter_Next(iter)) != NULL) {
                total += PyLong_AsLong(item);
                count += 1;
                Py_DECREF(item);
                if (PyErr_Occurred() != NULL) {
                    PyErr_Clear();
                    break;
                }
            }
            if (PyErr_Occurred() == NULL && total == 6 && count == 3) {
                mask |= BIT(10);
            } else if (PyErr_Occurred() != NULL) {
                PyErr_Clear();
            }
            Py_XDECREF(iter);
        }
        Py_XDECREF(one);
        Py_XDECREF(two);
        Py_XDECREF(three);
    }
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    PyObject *function = module_ref == NULL ? NULL : PyObject_GetAttrString(module_ref, "echo_arg_plus_one");
    PyObject *receiver = PyLong_FromLong(40);
    if (function != NULL && receiver != NULL) {
        PyObject *method = PyMethod_New(function, receiver);
        PyObject *called = method == NULL ? NULL : PyObject_CallNoArgs(method);
        if (long_equals(called, 41)) {
            mask |= BIT(11);
        }
        Py_XDECREF(called);
        Py_XDECREF(method);
    }
    Py_XDECREF(function);
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    Py_XDECREF(counter);
    Py_XDECREF(value_name);
    Py_XDECREF(twice_name);
    Py_XDECREF(add_name);
    Py_XDECREF(five);
    Py_XDECREF(dict);
    Py_XDECREF(key);
    Py_XDECREF(default_value);
    Py_XDECREF(replacement);
    Py_XDECREF(remove_value);
    Py_XDECREF(present_value);
    Py_XDECREF(proxy_value);
    Py_XDECREF(proxy_key);
    Py_XDECREF(mutation_value);
    Py_XDECREF(minus_two);
    Py_XDECREF(slice);
    Py_XDECREF(list);
    Py_XDECREF(receiver);

    if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }
    return PyLong_FromLong(mask);
}

static PyMethodDef methods[] = {
    {"drive", drive, METH_NOARGS, "exercise object/container protocol completion"},
    {"echo_arg_plus_one", echo_arg_plus_one, METH_O, "return argument plus one"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_object_container_completion_ext",
    "Pon object/container completion C-API test extension",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_object_container_completion_ext(void) {
    if (PyType_Ready(&CounterType) < 0) {
        return NULL;
    }
    PyObject *m = PyModule_Create(&module);
    if (m != NULL) {
        Py_INCREF(m);
        module_ref = m;
    }
    return m;
}
"#,
		);

		let module =
			super::super::load_extension_module("capi_object_container_completion_ext", &module_path)
				.unwrap_or_else(|message| {
					panic!("failed to load object/container completion C extension: {message}")
				});
		assert!(!module.is_null(), "extension loader returned NULL module");

		let drive = module_attr(intern("capi_object_container_completion_ext"), intern("drive"))
			.expect("drive method registered");
		let result = unsafe { pon_call(drive, ptr::null_mut(), 0) };
		assert!(!result.is_null(), "drive() returned NULL: {:?}", pon_err_message());
		assert_eq!(
			format_object(result).as_deref(),
			Ok("4095"),
			"object/container completion bitmask mismatch: {:?}",
			pon_err_message()
		);
	}
}
