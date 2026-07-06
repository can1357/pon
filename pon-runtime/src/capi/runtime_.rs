//! Runtime family: memory-adjacent process services, capsules, imports,
//! modules, and sys access.

use core::{
	ffi::{c_char, c_int, c_void},
	mem, ptr,
};
use std::{
	collections::HashMap,
	ffi::{CStr, CString},
	sync::{LazyLock, Mutex},
};

use num_traits::ToPrimitive;
use pon_gc::{GcTypeInfo, TypeId};

use super::{
	PyModuleDef, c_string,
	twin::{self, ForeignTypeObject},
};
use crate::{
	abi,
	intern::{intern, resolve},
	object::{PyObject, PyObjectHeader, PyType, is_exact_type},
	thread_state::{pon_err_clear, pon_err_message, pon_err_occurred},
	types::{
		async_generator::ensure_async_generator_type,
		coroutine::ensure_coroutine_type,
		exc::ExceptionKind,
		frame::{TYPE_ID_FRAME, ensure_frame_type, finalize_frame, trace_frame},
		generator::ensure_generator_type,
	},
};

pub(crate) type PyCapsuleDestructor = Option<unsafe extern "C" fn(*mut PyObject)>;
type PySendResult = c_int;
const PYGEN_RETURN: PySendResult = 0;
const PYGEN_ERROR: PySendResult = -1;
const PYGEN_NEXT: PySendResult = 1;

/// C mirror: `include/pon_capi/runtime.h` `PyPonCapiRuntime`.
#[repr(C)]
pub(crate) struct PyPonCapiRuntime {
	eval_save_thread:           unsafe extern "C" fn() -> *mut c_void,
	eval_restore_thread:        unsafe extern "C" fn(*mut c_void),
	capsule_new:
		unsafe extern "C" fn(*mut c_void, *const c_char, PyCapsuleDestructor) -> *mut PyObject,
	capsule_get_pointer:        unsafe extern "C" fn(*mut PyObject, *const c_char) -> *mut c_void,
	capsule_is_valid:           unsafe extern "C" fn(*mut PyObject, *const c_char) -> c_int,
	capsule_set_context:        unsafe extern "C" fn(*mut PyObject, *mut c_void) -> c_int,
	capsule_get_context:        unsafe extern "C" fn(*mut PyObject) -> *mut c_void,
	capsule_import:             unsafe extern "C" fn(*const c_char, c_int) -> *mut c_void,
	import_import_module:       unsafe extern "C" fn(*const c_char) -> *mut PyObject,
	import_add_module:          unsafe extern "C" fn(*const c_char) -> *mut PyObject,
	module_get_dict:            unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	module_get_state:           unsafe extern "C" fn(*mut PyObject) -> *mut c_void,
	module_get_name:            unsafe extern "C" fn(*mut PyObject) -> *const c_char,
	sys_get_object:             unsafe extern "C" fn(*const c_char) -> *mut PyObject,
	module_def_init:            unsafe extern "C" fn(*mut PyModuleDef) -> *mut PyObject,
	thread_state_get:           unsafe extern "C" fn() -> *mut c_void,
	thread_state_get_frame:     unsafe extern "C" fn(*mut c_void) -> *mut c_void,
	interpreter_state_main:     unsafe extern "C" fn() -> *mut c_void,
	eval_get_builtins:          unsafe extern "C" fn() -> *mut PyObject,
	frame_get_back:             unsafe extern "C" fn(*mut c_void) -> *mut c_void,
	frame_get_code:             unsafe extern "C" fn(*mut c_void) -> *mut c_void,
	contextvar_new:             unsafe extern "C" fn(*const c_char, *mut PyObject) -> *mut PyObject,
	contextvar_get: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut *mut PyObject) -> c_int,
	datetime_capi_import:       unsafe extern "C" fn() -> *mut c_void,
	datetime_get_attr_int:      unsafe extern "C" fn(*mut PyObject, *const c_char) -> c_int,
	capsule_set_name:           unsafe extern "C" fn(*mut PyObject, *const c_char) -> c_int,
	import_import:              unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
	#[cfg(test)]
	test_collect_pin_count:     unsafe extern "C" fn(*mut PyObject) -> isize,
	contextvar_set:             unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
	frame_new:
		unsafe extern "C" fn(*mut c_void, *mut c_void, *mut PyObject, *mut PyObject) -> *mut c_void,
	traceback_here:             unsafe extern "C" fn(*mut c_void) -> c_int,
	traceback_check:            unsafe extern "C" fn(*mut PyObject) -> c_int,
	code_new_empty: unsafe extern "C" fn(*const c_char, *const c_char, c_int) -> *mut c_void,
	code_new: unsafe extern "C" fn(
		c_int,
		c_int,
		c_int,
		c_int,
		c_int,
		*mut PyObject,
		*mut PyObject,
		*mut PyObject,
		*mut PyObject,
		*mut PyObject,
		*mut PyObject,
		*mut PyObject,
		*mut PyObject,
		*mut PyObject,
		c_int,
		*mut PyObject,
		*mut PyObject,
	) -> *mut c_void,
	code_new_with_posonly_args: unsafe extern "C" fn(
		c_int,
		c_int,
		c_int,
		c_int,
		c_int,
		c_int,
		*mut PyObject,
		*mut PyObject,
		*mut PyObject,
		*mut PyObject,
		*mut PyObject,
		*mut PyObject,
		*mut PyObject,
		*mut PyObject,
		*mut PyObject,
		c_int,
		*mut PyObject,
		*mut PyObject,
	) -> *mut c_void,
	code_get_num_free:          unsafe extern "C" fn(*mut c_void) -> c_int,
	code_has_free_vars:         unsafe extern "C" fn(*mut c_void) -> c_int,
	import_import_module_level: unsafe extern "C" fn(
		*const c_char,
		*mut PyObject,
		*mut PyObject,
		*mut PyObject,
		c_int,
	) -> *mut PyObject,
	iter_send:
		unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut *mut PyObject) -> PySendResult,
	async_gen_check_exact:      unsafe extern "C" fn(*mut PyObject) -> c_int,
}

unsafe impl Send for PyPonCapiRuntime {}
unsafe impl Sync for PyPonCapiRuntime {}

#[repr(C)]
struct PyCapsule {
	ob_base:    PyObjectHeader,
	pointer:    *mut c_void,
	name:       *const c_char,
	destructor: PyCapsuleDestructor,
	context:    *mut c_void,
}

unsafe impl Send for PyCapsule {}
unsafe impl Sync for PyCapsule {}

#[repr(C)]
struct PyInterpreterState {
	_private: u8,
}

unsafe impl Send for PyInterpreterState {}
unsafe impl Sync for PyInterpreterState {}

#[repr(C)]
struct PyErrStackItem {
	exc_value:     *mut PyObject,
	previous_item: *mut PyErrStackItem,
}

unsafe impl Send for PyErrStackItem {}
unsafe impl Sync for PyErrStackItem {}

#[repr(C)]
struct PyThreadState {
	interp:    *mut PyInterpreterState,
	exc_info:  *mut PyErrStackItem,
	exc_state: PyErrStackItem,
}

unsafe impl Send for PyThreadState {}
unsafe impl Sync for PyThreadState {}

/// GC id for C-API-created code objects.  It lives in the C-extension carrier
/// range next to the other capi-only heap payloads.
const TYPE_ID_CAPI_CODE: TypeId = TypeId(143);
const TYPE_ID_CAPI_CAPSULE: TypeId = TypeId(144);

#[repr(C)]
struct PyCapiCodeObject {
	ob_base:            PyObjectHeader,
	_co_firsttraceable: c_int,
	co_firstlineno:     c_int,
	co_filename:        *mut PyObject,
	co_name:            *mut PyObject,
	co_qualname:        *mut PyObject,
	co_nfreevars:       c_int,
}

unsafe impl Send for PyCapiCodeObject {}
unsafe impl Sync for PyCapiCodeObject {}

#[repr(C)]
struct PyDateTimeCapi {
	date_type: *mut ForeignTypeObject,
	datetime_type: *mut ForeignTypeObject,
	time_type: *mut ForeignTypeObject,
	delta_type: *mut ForeignTypeObject,
	tzinfo_type: *mut ForeignTypeObject,
	timezone_utc: *mut PyObject,
	date_from_date:
		unsafe extern "C" fn(c_int, c_int, c_int, *mut ForeignTypeObject) -> *mut PyObject,
	datetime_from_date_and_time: unsafe extern "C" fn(
		c_int,
		c_int,
		c_int,
		c_int,
		c_int,
		c_int,
		c_int,
		*mut PyObject,
		*mut ForeignTypeObject,
	) -> *mut PyObject,
	time_from_time: unsafe extern "C" fn(
		c_int,
		c_int,
		c_int,
		c_int,
		*mut PyObject,
		*mut ForeignTypeObject,
	) -> *mut PyObject,
	delta_from_delta:
		unsafe extern "C" fn(c_int, c_int, c_int, c_int, *mut ForeignTypeObject) -> *mut PyObject,
	timezone_from_timezone: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
	datetime_from_timestamp:
		unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject,
	date_from_timestamp: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
	datetime_from_date_and_time_and_fold: unsafe extern "C" fn(
		c_int,
		c_int,
		c_int,
		c_int,
		c_int,
		c_int,
		c_int,
		*mut PyObject,
		c_int,
		*mut ForeignTypeObject,
	) -> *mut PyObject,
	time_from_time_and_fold: unsafe extern "C" fn(
		c_int,
		c_int,
		c_int,
		c_int,
		*mut PyObject,
		c_int,
		*mut ForeignTypeObject,
	) -> *mut PyObject,
}

unsafe impl Send for PyDateTimeCapi {}
unsafe impl Sync for PyDateTimeCapi {}

static DATETIME_CAPI: LazyLock<Mutex<Option<usize>>> = LazyLock::new(|| Mutex::new(None));
const DATETIME_CAPSULE_NAME: &str = "datetime.datetime_CAPI";

static MAIN_INTERPRETER_STATE: LazyLock<PyInterpreterState> =
	LazyLock::new(|| PyInterpreterState { _private: 0 });
static MAIN_THREAD_STATE: LazyLock<usize> = LazyLock::new(|| {
	let mut state = Box::new(PyThreadState {
		interp:    interpreter_state_main(),
		exc_info:  ptr::null_mut(),
		exc_state: PyErrStackItem { exc_value: ptr::null_mut(), previous_item: ptr::null_mut() },
	});
	state.exc_info = &mut state.exc_state;
	Box::into_raw(state) as usize
});
static MODULE_STATES: LazyLock<Mutex<HashMap<usize, Box<[u8]>>>> =
	LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn build() -> PyPonCapiRuntime {
	PyPonCapiRuntime {
		eval_save_thread: capi_eval_save_thread,
		eval_restore_thread: capi_eval_restore_thread,
		capsule_new: capi_capsule_new,
		capsule_get_pointer: capi_capsule_get_pointer,
		capsule_is_valid: capi_capsule_is_valid,
		capsule_set_context: capi_capsule_set_context,
		capsule_get_context: capi_capsule_get_context,
		capsule_import: capi_capsule_import,
		import_import_module: capi_import_import_module,
		import_add_module: capi_import_add_module,
		module_get_dict: capi_module_get_dict,
		module_get_state: capi_module_get_state,
		module_get_name: capi_module_get_name,
		sys_get_object: capi_sys_get_object,
		module_def_init: super::py_module_def_init,
		thread_state_get: capi_thread_state_get,
		thread_state_get_frame: capi_thread_state_get_frame,
		interpreter_state_main: capi_interpreter_state_main,
		eval_get_builtins: capi_eval_get_builtins,
		frame_get_back: capi_frame_get_back,
		frame_get_code: capi_frame_get_code,
		contextvar_new: capi_contextvar_new,
		contextvar_get: capi_contextvar_get,
		datetime_capi_import: capi_datetime_capi_import,
		datetime_get_attr_int: capi_datetime_get_attr_int,
		capsule_set_name: capi_capsule_set_name,
		import_import: capi_import_import,
		#[cfg(test)]
		test_collect_pin_count: capi_test_collect_pin_count,
		contextvar_set: capi_contextvar_set,
		frame_new: capi_frame_new,
		traceback_here: capi_traceback_here,
		traceback_check: capi_traceback_check,
		code_new_empty: capi_code_new_empty,
		code_new: capi_code_new,
		code_new_with_posonly_args: capi_code_new_with_posonly_args,
		code_get_num_free: capi_code_get_num_free,
		code_has_free_vars: capi_code_has_free_vars,
		import_import_module_level: capi_import_import_module_level,
		iter_send: capi_iter_send,
		async_gen_check_exact: capi_async_gen_check_exact,
	}
}

#[must_use]
pub(crate) fn capsule_type() -> *mut PyType {
	static CAPSULE_TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut ty = PyType::new(ptr::null(), "PyCapsule", mem::size_of::<PyCapsule>());
		ty.gc_type_id = TYPE_ID_CAPI_CAPSULE.0 as usize;
		Box::into_raw(Box::new(ty)) as usize
	});
	*CAPSULE_TYPE as *mut PyType
}

fn interpreter_state_main() -> *mut PyInterpreterState {
	ptr::from_ref(&*MAIN_INTERPRETER_STATE).cast_mut()
}

fn thread_state_singleton() -> *mut PyThreadState {
	*MAIN_THREAD_STATE as *mut PyThreadState
}
pub(super) fn register_module_state(module: *mut PyObject, size: usize) -> Result<(), String> {
	if module.is_null() {
		return Err("cannot allocate module state for NULL module".to_owned());
	}
	let allocation_len = size.max(1);
	let mut bytes = Vec::new();
	bytes
		.try_reserve_exact(allocation_len)
		.map_err(|_| format!("failed to allocate {size} bytes of module state"))?;
	bytes.resize(allocation_len, 0);
	MODULE_STATES
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.insert(module as usize, bytes.into_boxed_slice());
	Ok(())
}

pub(super) fn unregister_module_state(module: *mut PyObject) {
	MODULE_STATES
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.remove(&(module as usize));
}

fn new_reference(object: *mut PyObject) -> *mut PyObject {
	super::pin_new_reference(object)
}

unsafe extern "C" fn capi_eval_save_thread() -> *mut c_void {
	thread_state_singleton().cast::<c_void>()
}

unsafe extern "C" fn capi_eval_restore_thread(_state: *mut c_void) {}

unsafe extern "C" fn capi_thread_state_get() -> *mut c_void {
	thread_state_singleton().cast::<c_void>()
}

/// Pon does not expose materialized frame objects through the C API yet.
/// CPython documents a NULL return here when no current frame is available, so
/// this is a semantically valid degenerate result rather than a fake frame.
unsafe extern "C" fn capi_thread_state_get_frame(_state: *mut c_void) -> *mut c_void {
	ptr::null_mut()
}

unsafe extern "C" fn capi_interpreter_state_main() -> *mut c_void {
	interpreter_state_main().cast::<c_void>()
}

unsafe extern "C" fn capi_eval_get_builtins() -> *mut PyObject {
	let builtins = import_module_text("builtins");
	if builtins.is_null() {
		return ptr::null_mut();
	}
	unsafe { capi_module_get_dict(builtins) }
}

unsafe extern "C" fn capi_frame_get_back(frame: *mut c_void) -> *mut c_void {
	if frame.is_null() {
		raise_system_error("PyFrame_GetBack called with NULL frame");
		return ptr::null_mut();
	}
	let back = crate::types::frame::frame_get_back_for_capi(frame.cast::<PyObject>());
	if back.is_null() {
		ptr::null_mut()
	} else {
		new_reference(back).cast::<c_void>()
	}
}

unsafe extern "C" fn capi_frame_get_code(frame: *mut c_void) -> *mut c_void {
	if frame.is_null() {
		raise_system_error("PyFrame_GetCode called with NULL frame");
		return ptr::null_mut();
	}
	let frame = frame.cast::<abi::PyFrame>();
	let code = unsafe { (*frame).parent };
	if !code.is_null() && unsafe { is_capi_code_object(code) } {
		return new_reference(code).cast::<c_void>();
	}
	let code = crate::types::frame::frame_get_code_for_capi(frame.cast::<PyObject>());
	if code.is_null() {
		ptr::null_mut()
	} else {
		new_reference(code).cast::<c_void>()
	}
}

unsafe extern "C" fn capi_frame_new(
	_tstate: *mut c_void,
	code: *mut c_void,
	_globals: *mut PyObject,
	_locals: *mut PyObject,
) -> *mut c_void {
	if code.is_null() {
		return raise_system_error_null("PyFrame_New called with NULL code").cast::<c_void>();
	}
	let frame_type = ensure_frame_type(abi::runtime_type_type());
	let info = GcTypeInfo {
		size:     mem::size_of::<abi::PyFrame>(),
		trace:    trace_frame,
		finalize: Some(finalize_frame),
	};
	let block = match abi::alloc_gc_object(TYPE_ID_FRAME, info) {
		Ok(block) => block,
		Err(message) => return raise_system_error_null(&message).cast::<c_void>(),
	};
	let frame = block.cast::<abi::PyFrame>();
	let firstlineno = unsafe { (*code.cast::<PyCapiCodeObject>()).co_firstlineno };
	let line = if firstlineno > 0 {
		firstlineno as u32
	} else {
		0
	};
	unsafe {
		ptr::write(frame, abi::PyFrame::new(frame_type.cast_const(), 0, ptr::null_mut()));
		(*frame).line = line;
		(*frame).parent = code.cast::<PyObject>();
	}
	new_reference(frame.cast::<PyObject>()).cast::<c_void>()
}

unsafe extern "C" fn capi_traceback_here(frame: *mut c_void) -> c_int {
	match abi::exc::prepend_traceback_for_frame(frame.cast::<abi::PyFrame>()) {
		Ok(()) => 0,
		Err(message) => {
			raise_system_error(&message);
			-1
		},
	}
}

unsafe extern "C" fn capi_traceback_check(object: *mut PyObject) -> c_int {
	if object.is_null() || !crate::tag::is_heap(object) {
		return 0;
	}
	let traceback_type = crate::traceback::ensure_traceback_type(abi::runtime_type_type());
	(unsafe { (*object).ob_type } == traceback_type.cast_const()) as c_int
}

unsafe extern "C" fn capi_code_new_empty(
	filename: *const c_char,
	funcname: *const c_char,
	firstlineno: c_int,
) -> *mut c_void {
	let Some(filename) = c_string(filename) else {
		return raise_system_error_null("PyCode_NewEmpty called with invalid filename")
			.cast::<c_void>();
	};
	let Some(funcname) = c_string(funcname) else {
		return raise_system_error_null("PyCode_NewEmpty called with invalid function name")
			.cast::<c_void>();
	};
	let Some(filename_object) = unicode_object_from_str(&filename) else {
		return ptr::null_mut();
	};
	let Some(name_object) = unicode_object_from_str(&funcname) else {
		return ptr::null_mut();
	};
	alloc_capi_code_object(filename_object, name_object, name_object, firstlineno, 0)
}

unsafe extern "C" fn capi_code_new(
	_argcount: c_int,
	_kwonlyargcount: c_int,
	_nlocals: c_int,
	_stacksize: c_int,
	_flags: c_int,
	_code: *mut PyObject,
	_consts: *mut PyObject,
	_names: *mut PyObject,
	_varnames: *mut PyObject,
	_freevars: *mut PyObject,
	_cellvars: *mut PyObject,
	filename: *mut PyObject,
	name: *mut PyObject,
	qualname: *mut PyObject,
	firstlineno: c_int,
	_linetable: *mut PyObject,
	_exceptiontable: *mut PyObject,
) -> *mut c_void {
	unsafe { capi_code_new_common(filename, name, qualname, firstlineno) }
}

unsafe extern "C" fn capi_code_new_with_posonly_args(
	_argcount: c_int,
	_posonlyargcount: c_int,
	_kwonlyargcount: c_int,
	_nlocals: c_int,
	_stacksize: c_int,
	_flags: c_int,
	_code: *mut PyObject,
	_consts: *mut PyObject,
	_names: *mut PyObject,
	_varnames: *mut PyObject,
	_freevars: *mut PyObject,
	_cellvars: *mut PyObject,
	filename: *mut PyObject,
	name: *mut PyObject,
	qualname: *mut PyObject,
	firstlineno: c_int,
	_linetable: *mut PyObject,
	_exceptiontable: *mut PyObject,
) -> *mut c_void {
	unsafe { capi_code_new_common(filename, name, qualname, firstlineno) }
}

unsafe fn capi_code_new_common(
	filename: *mut PyObject,
	name: *mut PyObject,
	qualname: *mut PyObject,
	firstlineno: c_int,
) -> *mut c_void {
	let Some(filename) = (unsafe { code_text_arg(filename, "filename") }) else {
		return ptr::null_mut();
	};
	let Some(name) = (unsafe { code_text_arg(name, "name") }) else {
		return ptr::null_mut();
	};
	let qualname = if qualname.is_null() {
		name
	} else {
		let Some(qualname) = (unsafe { code_text_arg(qualname, "qualname") }) else {
			return ptr::null_mut();
		};
		qualname
	};
	alloc_capi_code_object(filename, name, qualname, firstlineno, 0)
}

unsafe fn code_text_arg(object: *mut PyObject, label: &str) -> Option<*mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() {
		raise_type_error(&format!("PyCode_New {label} must be str, not NULL"));
		return None;
	}
	if unsafe { crate::types::type_::unicode_text(object) }.is_none() {
		raise_type_error(&format!("PyCode_New {label} must be str"));
		return None;
	}
	Some(object)
}

fn unicode_object_from_str(text: &str) -> Option<*mut PyObject> {
	let object = unsafe { abi::pon_const_str(text.as_ptr(), text.len()) };
	if object.is_null() { None } else { Some(object) }
}

fn alloc_capi_code_object(
	filename: *mut PyObject,
	name: *mut PyObject,
	qualname: *mut PyObject,
	firstlineno: c_int,
	nfreevars: c_int,
) -> *mut c_void {
	let info = GcTypeInfo {
		size:     mem::size_of::<PyCapiCodeObject>(),
		trace:    trace_capi_code,
		finalize: None,
	};
	let block = match abi::alloc_gc_object(TYPE_ID_CAPI_CODE, info) {
		Ok(block) => block,
		Err(message) => return raise_system_error_null(&message).cast::<c_void>(),
	};
	let code = block.cast::<PyCapiCodeObject>();
	unsafe {
		ptr::write(code, PyCapiCodeObject {
			ob_base:            PyObjectHeader::new(capi_code_type().cast_const()),
			_co_firsttraceable: 0,
			co_firstlineno:     firstlineno,
			co_filename:        filename,
			co_name:            name,
			co_qualname:        qualname,
			co_nfreevars:       if nfreevars < 0 { 0 } else { nfreevars },
		});
	}
	new_reference(code.cast::<PyObject>()).cast::<c_void>()
}

fn capi_code_type() -> *mut PyType {
	static CODE_TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut ty = PyType::new(
			abi::runtime_type_type().cast_const(),
			"code",
			mem::size_of::<PyCapiCodeObject>(),
		);
		ty.gc_type_id = TYPE_ID_CAPI_CODE.0 as usize;
		ty.tp_getattro = Some(capi_code_getattro);
		Box::into_raw(Box::new(ty)) as usize
	});
	*CODE_TYPE as *mut PyType
}

unsafe fn is_capi_code_object(object: *mut PyObject) -> bool {
	if object.is_null() || crate::tag::is_small_int(object) || !crate::tag::is_heap(object) {
		return false;
	}
	unsafe { (*object).ob_type == capi_code_type().cast_const() }
}

unsafe extern "C" fn capi_code_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(attr) = (unsafe { crate::types::type_::unicode_text(crate::tag::untag_arg(name)) })
	else {
		raise_type_error("code attribute name must be str");
		return ptr::null_mut();
	};
	let code = unsafe { &*object.cast::<PyCapiCodeObject>() };
	match attr {
		"co_filename" => new_reference(code.co_filename),
		"co_name" => new_reference(code.co_name),
		"co_qualname" => new_reference(code.co_qualname),
		"co_firstlineno" => unsafe { abi::pon_const_int(i64::from(code.co_firstlineno)) },
		_ => unsafe { abi::pon_raise_attribute_error(object, intern(attr)) },
	}
}

unsafe extern "C" fn trace_capi_code(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
	if object.is_null() {
		return;
	}
	let code = unsafe { &*object.cast::<PyCapiCodeObject>() };
	for value in [code.co_filename, code.co_name, code.co_qualname] {
		if !value.is_null() {
			visitor(value.cast::<u8>());
		}
	}
}

unsafe extern "C" fn capi_code_get_num_free(code: *mut c_void) -> c_int {
	if code.is_null() {
		raise_system_error("PyCode_GetNumFree called with NULL code");
		return 0;
	}
	unsafe { (*code.cast::<PyCapiCodeObject>()).co_nfreevars }
}

unsafe extern "C" fn capi_code_has_free_vars(code: *mut c_void) -> c_int {
	(unsafe { capi_code_get_num_free(code) } > 0) as c_int
}

unsafe extern "C" fn capi_contextvar_new(
	name: *const c_char,
	default: *mut PyObject,
) -> *mut PyObject {
	let Some(name) = c_string(name) else {
		return raise_system_error_null("PyContextVar_New called with invalid name");
	};
	new_reference(crate::native::contextvars::capi_contextvar_new(&name, default))
}

unsafe extern "C" fn capi_contextvar_get(
	var: *mut PyObject,
	default: *mut PyObject,
	value: *mut *mut PyObject,
) -> c_int {
	let status = unsafe { crate::native::contextvars::capi_contextvar_get(var, default, value) };
	if status == 0 && !value.is_null() {
		let object = unsafe { *value };
		if !object.is_null() {
			super::pin_object(object);
		}
	}
	status
}

unsafe extern "C" fn capi_contextvar_set(
	var: *mut PyObject,
	value: *mut PyObject,
) -> *mut PyObject {
	let method = unsafe { abi::pon_get_attr(var, intern("set"), ptr::null_mut()) };
	if method.is_null() {
		return ptr::null_mut();
	}
	let value = super::object_::normalize_object_arg(value);
	let mut argv = [value];
	new_reference(unsafe { abi::pon_call(method, argv.as_mut_ptr(), argv.len()) })
}

unsafe extern "C" fn capi_datetime_capi_import() -> *mut c_void {
	match datetime_capi_ptr() {
		Ok(capi) => capi.cast::<c_void>(),
		Err(message) => raise_import_error_void(&message),
	}
}

unsafe extern "C" fn capi_datetime_get_attr_int(
	object: *mut PyObject,
	name: *const c_char,
) -> c_int {
	let Some(name) = c_string(name) else {
		raise_system_error("PyDateTime attribute accessor called with invalid attribute name");
		return -1;
	};
	match unsafe { datetime_int_attr_raw(object, &name) } {
		Ok(value) => value,
		Err(message) => {
			if !pon_err_occurred() {
				raise_type_error(&message);
			}
			-1
		},
	}
}

fn datetime_capi_ptr() -> Result<*mut PyDateTimeCapi, String> {
	let mut cached = DATETIME_CAPI
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	if let Some(ptr) = *cached {
		return Ok(ptr as *mut PyDateTimeCapi);
	}

	let capi = build_datetime_capi()?;
	let ptr = Box::into_raw(Box::new(capi)) as usize;
	*cached = Some(ptr);
	Ok(ptr as *mut PyDateTimeCapi)
}

#[repr(C)]
struct PonDateObject {
	ob_base: PyObjectHeader,
	year:    c_int,
	month:   c_int,
	day:     c_int,
}

#[repr(C)]
struct PonDateTimeObject {
	ob_base:     PyObjectHeader,
	year:        c_int,
	month:       c_int,
	day:         c_int,
	hour:        c_int,
	minute:      c_int,
	second:      c_int,
	microsecond: c_int,
	fold:        c_int,
	tzinfo:      *mut PyObject,
}

#[repr(C)]
struct PonTimeObject {
	ob_base:     PyObjectHeader,
	hour:        c_int,
	minute:      c_int,
	second:      c_int,
	microsecond: c_int,
	fold:        c_int,
	tzinfo:      *mut PyObject,
}

#[repr(C)]
struct PonDeltaObject {
	ob_base:      PyObjectHeader,
	days:         c_int,
	seconds:      c_int,
	microseconds: c_int,
}

#[repr(C)]
struct PonTimezoneObject {
	ob_base: PyObjectHeader,
}

unsafe impl Send for PonDateObject {}
unsafe impl Sync for PonDateObject {}
unsafe impl Send for PonDateTimeObject {}
unsafe impl Sync for PonDateTimeObject {}
unsafe impl Send for PonTimeObject {}
unsafe impl Sync for PonTimeObject {}
unsafe impl Send for PonDeltaObject {}
unsafe impl Sync for PonDeltaObject {}
unsafe impl Send for PonTimezoneObject {}
unsafe impl Sync for PonTimezoneObject {}

fn build_datetime_capi() -> Result<PyDateTimeCapi, String> {
	let capi = PyDateTimeCapi {
		date_type: twin::foreign_of_native(datetime_date_type()),
		datetime_type: twin::foreign_of_native(datetime_datetime_type()),
		time_type: twin::foreign_of_native(datetime_time_type()),
		delta_type: twin::foreign_of_native(datetime_delta_type()),
		tzinfo_type: twin::foreign_of_native(datetime_tzinfo_type()),
		timezone_utc: datetime_utc(),
		date_from_date: capi_datetime_date_from_date,
		datetime_from_date_and_time: capi_datetime_datetime_from_date_and_time,
		time_from_time: capi_datetime_time_from_time,
		delta_from_delta: capi_datetime_delta_from_delta,
		timezone_from_timezone: capi_datetime_unsupported_timezone_from_timezone,
		datetime_from_timestamp: capi_datetime_datetime_from_timestamp,
		date_from_timestamp: capi_datetime_date_from_timestamp,
		datetime_from_date_and_time_and_fold: capi_datetime_datetime_from_date_and_time_and_fold,
		time_from_time_and_fold: capi_datetime_time_from_time_and_fold,
	};

	verify_datetime_capi(&capi)?;
	Ok(capi)
}

fn runtime_object_type() -> *mut PyType {
	abi::runtime_global(intern("object")).map_or(ptr::null_mut(), |object| object.cast::<PyType>())
}

fn datetime_date_type() -> *mut PyType {
	static TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut ty = PyType::new(
			abi::runtime_type_type().cast_const(),
			"date",
			mem::size_of::<PonDateObject>(),
		);
		ty.tp_base = runtime_object_type();
		ty.tp_new = Some(pon_datetime_date_new);
		ty.tp_getattro = Some(pon_datetime_date_getattro);
		Box::into_raw(Box::new(ty)) as usize
	});
	*TYPE as *mut PyType
}

fn datetime_datetime_type() -> *mut PyType {
	static TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut ty = PyType::new(
			abi::runtime_type_type().cast_const(),
			"datetime",
			mem::size_of::<PonDateTimeObject>(),
		);
		ty.tp_base = datetime_date_type();
		ty.tp_new = Some(pon_datetime_datetime_new);
		ty.tp_getattro = Some(pon_datetime_datetime_getattro);
		Box::into_raw(Box::new(ty)) as usize
	});
	*TYPE as *mut PyType
}

fn datetime_time_type() -> *mut PyType {
	static TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut ty = PyType::new(
			abi::runtime_type_type().cast_const(),
			"time",
			mem::size_of::<PonTimeObject>(),
		);
		ty.tp_base = runtime_object_type();
		ty.tp_new = Some(pon_datetime_time_new);
		ty.tp_getattro = Some(pon_datetime_time_getattro);
		Box::into_raw(Box::new(ty)) as usize
	});
	*TYPE as *mut PyType
}

fn datetime_delta_type() -> *mut PyType {
	static TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut ty = PyType::new(
			abi::runtime_type_type().cast_const(),
			"timedelta",
			mem::size_of::<PonDeltaObject>(),
		);
		ty.tp_base = runtime_object_type();
		ty.tp_new = Some(pon_datetime_delta_new);
		ty.tp_getattro = Some(pon_datetime_delta_getattro);
		Box::into_raw(Box::new(ty)) as usize
	});
	*TYPE as *mut PyType
}

fn datetime_tzinfo_type() -> *mut PyType {
	static TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut ty = PyType::new(
			abi::runtime_type_type().cast_const(),
			"tzinfo",
			mem::size_of::<PyObjectHeader>(),
		);
		ty.tp_base = runtime_object_type();
		Box::into_raw(Box::new(ty)) as usize
	});
	*TYPE as *mut PyType
}

fn datetime_timezone_type() -> *mut PyType {
	static TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut ty = PyType::new(
			abi::runtime_type_type().cast_const(),
			"timezone",
			mem::size_of::<PonTimezoneObject>(),
		);
		ty.tp_base = datetime_tzinfo_type();
		Box::into_raw(Box::new(ty)) as usize
	});
	*TYPE as *mut PyType
}

fn datetime_utc() -> *mut PyObject {
	static UTC: LazyLock<usize> = LazyLock::new(|| {
		Box::into_raw(Box::new(PonTimezoneObject {
			ob_base: PyObjectHeader::new(datetime_timezone_type().cast_const()),
		})) as usize
	});
	*UTC as *mut PyObject
}

unsafe extern "C" fn pon_datetime_date_new(
	cls: *mut PyType,
	args: *mut PyObject,
	_kwargs: *mut PyObject,
) -> *mut PyObject {
	let Ok(positional) = (unsafe { datetime_positional_args(args, 3, "date") }) else {
		return ptr::null_mut();
	};
	let Some(year) = (unsafe { datetime_c_int(positional[0], "year") }) else {
		return ptr::null_mut();
	};
	let Some(month) = (unsafe { datetime_c_int(positional[1], "month") }) else {
		return ptr::null_mut();
	};
	let Some(day) = (unsafe { datetime_c_int(positional[2], "day") }) else {
		return ptr::null_mut();
	};
	alloc_datetime_date(cls, year, month, day)
}

unsafe extern "C" fn pon_datetime_datetime_new(
	cls: *mut PyType,
	args: *mut PyObject,
	_kwargs: *mut PyObject,
) -> *mut PyObject {
	let Ok(positional) = (unsafe { datetime_positional_args(args, 8, "datetime") }) else {
		return ptr::null_mut();
	};
	let Some(year) = (unsafe { datetime_c_int(positional[0], "year") }) else {
		return ptr::null_mut();
	};
	let Some(month) = (unsafe { datetime_c_int(positional[1], "month") }) else {
		return ptr::null_mut();
	};
	let Some(day) = (unsafe { datetime_c_int(positional[2], "day") }) else {
		return ptr::null_mut();
	};
	let Some(hour) = (unsafe { datetime_c_int(positional[3], "hour") }) else {
		return ptr::null_mut();
	};
	let Some(minute) = (unsafe { datetime_c_int(positional[4], "minute") }) else {
		return ptr::null_mut();
	};
	let Some(second) = (unsafe { datetime_c_int(positional[5], "second") }) else {
		return ptr::null_mut();
	};
	let Some(microsecond) = (unsafe { datetime_c_int(positional[6], "microsecond") }) else {
		return ptr::null_mut();
	};
	alloc_datetime_datetime(
		cls,
		year,
		month,
		day,
		hour,
		minute,
		second,
		microsecond,
		0,
		positional[7],
	)
}

unsafe extern "C" fn pon_datetime_time_new(
	cls: *mut PyType,
	args: *mut PyObject,
	_kwargs: *mut PyObject,
) -> *mut PyObject {
	let Ok(positional) = (unsafe { datetime_positional_args(args, 5, "time") }) else {
		return ptr::null_mut();
	};
	let Some(hour) = (unsafe { datetime_c_int(positional[0], "hour") }) else {
		return ptr::null_mut();
	};
	let Some(minute) = (unsafe { datetime_c_int(positional[1], "minute") }) else {
		return ptr::null_mut();
	};
	let Some(second) = (unsafe { datetime_c_int(positional[2], "second") }) else {
		return ptr::null_mut();
	};
	let Some(microsecond) = (unsafe { datetime_c_int(positional[3], "microsecond") }) else {
		return ptr::null_mut();
	};
	alloc_datetime_time(cls, hour, minute, second, microsecond, 0, positional[4])
}

unsafe extern "C" fn pon_datetime_delta_new(
	cls: *mut PyType,
	args: *mut PyObject,
	_kwargs: *mut PyObject,
) -> *mut PyObject {
	let Ok(positional) = (unsafe { datetime_positional_args(args, 3, "timedelta") }) else {
		return ptr::null_mut();
	};
	let Some(days) = (unsafe { datetime_c_int(positional[0], "days") }) else {
		return ptr::null_mut();
	};
	let Some(seconds) = (unsafe { datetime_c_int(positional[1], "seconds") }) else {
		return ptr::null_mut();
	};
	let Some(microseconds) = (unsafe { datetime_c_int(positional[2], "microseconds") }) else {
		return ptr::null_mut();
	};
	alloc_datetime_delta(cls, days, seconds, microseconds)
}

fn alloc_datetime_date(cls: *mut PyType, year: c_int, month: c_int, day: c_int) -> *mut PyObject {
	let ty = if cls.is_null() {
		datetime_date_type()
	} else {
		cls
	};
	Box::into_raw(Box::new(PonDateObject {
		ob_base: PyObjectHeader::new(ty.cast_const()),
		year,
		month,
		day,
	}))
	.cast::<PyObject>()
}

fn alloc_datetime_datetime(
	cls: *mut PyType,
	year: c_int,
	month: c_int,
	day: c_int,
	hour: c_int,
	minute: c_int,
	second: c_int,
	microsecond: c_int,
	fold: c_int,
	tzinfo: *mut PyObject,
) -> *mut PyObject {
	let ty = if cls.is_null() {
		datetime_datetime_type()
	} else {
		cls
	};
	let tzinfo = if tzinfo.is_null() {
		unsafe { abi::pon_none() }
	} else {
		tzinfo
	};
	if tzinfo.is_null() {
		return ptr::null_mut();
	}
	Box::into_raw(Box::new(PonDateTimeObject {
		ob_base: PyObjectHeader::new(ty.cast_const()),
		year,
		month,
		day,
		hour,
		minute,
		second,
		microsecond,
		fold,
		tzinfo,
	}))
	.cast::<PyObject>()
}

fn alloc_datetime_time(
	cls: *mut PyType,
	hour: c_int,
	minute: c_int,
	second: c_int,
	microsecond: c_int,
	fold: c_int,
	tzinfo: *mut PyObject,
) -> *mut PyObject {
	let ty = if cls.is_null() {
		datetime_time_type()
	} else {
		cls
	};
	let tzinfo = if tzinfo.is_null() {
		unsafe { abi::pon_none() }
	} else {
		tzinfo
	};
	if tzinfo.is_null() {
		return ptr::null_mut();
	}
	Box::into_raw(Box::new(PonTimeObject {
		ob_base: PyObjectHeader::new(ty.cast_const()),
		hour,
		minute,
		second,
		microsecond,
		fold,
		tzinfo,
	}))
	.cast::<PyObject>()
}

fn alloc_datetime_delta(
	cls: *mut PyType,
	days: c_int,
	seconds: c_int,
	microseconds: c_int,
) -> *mut PyObject {
	let ty = if cls.is_null() {
		datetime_delta_type()
	} else {
		cls
	};
	Box::into_raw(Box::new(PonDeltaObject {
		ob_base: PyObjectHeader::new(ty.cast_const()),
		days,
		seconds,
		microseconds,
	}))
	.cast::<PyObject>()
}

unsafe fn datetime_positional_args(
	args: *mut PyObject,
	expected: usize,
	symbol: &str,
) -> Result<Vec<*mut PyObject>, ()> {
	match unsafe { crate::types::type_::positional_args_from_object(args) } {
		Ok(positional) if positional.len() == expected => Ok(positional),
		Ok(positional) => {
			raise_type_error(&format!(
				"{symbol} expected {expected} positional arguments, got {}",
				positional.len()
			));
			Err(())
		},
		Err(message) => {
			raise_type_error(&message);
			Err(())
		},
	}
}

unsafe fn datetime_c_int(object: *mut PyObject, label: &str) -> Option<c_int> {
	let object = crate::tag::untag_arg(object);
	let Some(integer) = (unsafe { crate::types::int::to_bigint_including_bool(object) }) else {
		raise_type_error(&format!("{label} must be an integer"));
		return None;
	};
	let Some(value) = integer.to_i32() else {
		raise_type_error(&format!("{label} is outside the C int range"));
		return None;
	};
	Some(value)
}

unsafe fn datetime_attr_name<'a>(name: *mut PyObject) -> Option<&'a str> {
	let name = crate::tag::untag_arg(name);
	let Some(text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		raise_type_error("datetime attribute name must be str");
		return None;
	};
	Some(text)
}

unsafe extern "C" fn pon_datetime_date_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(name) = (unsafe { datetime_attr_name(name) }) else {
		return ptr::null_mut();
	};
	let date = unsafe { &*object.cast::<PonDateObject>() };
	match name {
		"year" => unsafe { abi::pon_const_int(i64::from(date.year)) },
		"month" => unsafe { abi::pon_const_int(i64::from(date.month)) },
		"day" => unsafe { abi::pon_const_int(i64::from(date.day)) },
		_ => datetime_attribute_error(name),
	}
}

unsafe extern "C" fn pon_datetime_datetime_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(name) = (unsafe { datetime_attr_name(name) }) else {
		return ptr::null_mut();
	};
	let datetime = unsafe { &*object.cast::<PonDateTimeObject>() };
	match name {
		"year" => unsafe { abi::pon_const_int(i64::from(datetime.year)) },
		"month" => unsafe { abi::pon_const_int(i64::from(datetime.month)) },
		"day" => unsafe { abi::pon_const_int(i64::from(datetime.day)) },
		"hour" => unsafe { abi::pon_const_int(i64::from(datetime.hour)) },
		"minute" => unsafe { abi::pon_const_int(i64::from(datetime.minute)) },
		"second" => unsafe { abi::pon_const_int(i64::from(datetime.second)) },
		"microsecond" => unsafe { abi::pon_const_int(i64::from(datetime.microsecond)) },
		"fold" => unsafe { abi::pon_const_int(i64::from(datetime.fold)) },
		"tzinfo" => datetime.tzinfo,
		_ => datetime_attribute_error(name),
	}
}

unsafe extern "C" fn pon_datetime_time_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(name) = (unsafe { datetime_attr_name(name) }) else {
		return ptr::null_mut();
	};
	let time = unsafe { &*object.cast::<PonTimeObject>() };
	match name {
		"hour" => unsafe { abi::pon_const_int(i64::from(time.hour)) },
		"minute" => unsafe { abi::pon_const_int(i64::from(time.minute)) },
		"second" => unsafe { abi::pon_const_int(i64::from(time.second)) },
		"microsecond" => unsafe { abi::pon_const_int(i64::from(time.microsecond)) },
		"fold" => unsafe { abi::pon_const_int(i64::from(time.fold)) },
		"tzinfo" => time.tzinfo,
		_ => datetime_attribute_error(name),
	}
}

unsafe extern "C" fn pon_datetime_delta_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(name) = (unsafe { datetime_attr_name(name) }) else {
		return ptr::null_mut();
	};
	let delta = unsafe { &*object.cast::<PonDeltaObject>() };
	match name {
		"days" => unsafe { abi::pon_const_int(i64::from(delta.days)) },
		"seconds" => unsafe { abi::pon_const_int(i64::from(delta.seconds)) },
		"microseconds" => unsafe { abi::pon_const_int(i64::from(delta.microseconds)) },
		_ => datetime_attribute_error(name),
	}
}

fn datetime_attribute_error(name: &str) -> *mut PyObject {
	let _ = abi::exc::raise_kind_error_text(
		ExceptionKind::AttributeError,
		&format!("datetime object has no attribute {name}"),
	);
	ptr::null_mut()
}

fn verify_datetime_capi(capi: &PyDateTimeCapi) -> Result<(), String> {
	let date = unsafe { capi_datetime_date_from_date(2020, 1, 2, capi.date_type) };
	if date.is_null() {
		let detail = pending_error_detail();
		pon_err_clear();
		return Err(format!("PyDateTime_IMPORT datetime.date(2020, 1, 2) failed: {detail}"));
	}
	verify_datetime_attr(date, "year", 2020, "datetime.date.year")?;
	verify_datetime_attr(date, "month", 1, "datetime.date.month")?;
	verify_datetime_attr(date, "day", 2, "datetime.date.day")?;

	let none = unsafe { abi::pon_none() };
	let datetime = unsafe {
		capi_datetime_datetime_from_date_and_time(2020, 1, 2, 3, 4, 5, 6, none, capi.datetime_type)
	};
	if datetime.is_null() {
		let detail = pending_error_detail();
		pon_err_clear();
		return Err(format!("PyDateTime_IMPORT datetime.datetime(...) failed: {detail}"));
	}
	for (attr, expected) in [
		("year", 2020),
		("month", 1),
		("day", 2),
		("hour", 3),
		("minute", 4),
		("second", 5),
		("microsecond", 6),
	] {
		verify_datetime_attr(datetime, attr, expected, &format!("datetime.datetime.{attr}"))?;
	}

	let delta = unsafe { capi_datetime_delta_from_delta(1, 2, 3, 1, capi.delta_type) };
	if delta.is_null() {
		let detail = pending_error_detail();
		pon_err_clear();
		return Err(format!("PyDateTime_IMPORT datetime.timedelta(1, 2, 3) failed: {detail}"));
	}
	verify_datetime_attr(delta, "days", 1, "datetime.timedelta.days")?;
	verify_datetime_attr(delta, "seconds", 2, "datetime.timedelta.seconds")?;
	verify_datetime_attr(delta, "microseconds", 3, "datetime.timedelta.microseconds")?;

	if capi.timezone_utc.is_null() {
		return Err("PyDateTime_IMPORT datetime.UTC is NULL".to_owned());
	}
	Ok(())
}

fn verify_datetime_attr(
	object: *mut PyObject,
	attr: &str,
	expected: c_int,
	label: &str,
) -> Result<(), String> {
	match unsafe { datetime_int_attr_raw(object, attr) } {
		Ok(actual) if actual == expected => Ok(()),
		Ok(actual) => {
			Err(format!("PyDateTime_IMPORT {label} returned {actual}, expected {expected}"))
		},
		Err(message) => {
			pon_err_clear();
			Err(format!("PyDateTime_IMPORT could not read {label}: {message}"))
		},
	}
}

unsafe fn datetime_int_attr_raw(object: *mut PyObject, attr: &str) -> Result<c_int, String> {
	if object.is_null() {
		return Err(format!("datetime attribute {attr} read received NULL object"));
	}
	let value = unsafe { abi::pon_get_attr(object, intern(attr), ptr::null_mut()) };
	if value.is_null() {
		return Err(pending_error_detail());
	}
	let value = crate::tag::untag_arg(value);
	let Some(integer) = (unsafe { crate::types::int::to_bigint_including_bool(value) }) else {
		return Err(format!("datetime attribute {attr} is not an integer"));
	};
	integer
		.to_i32()
		.ok_or_else(|| format!("datetime attribute {attr} is outside the C int range"))
}

unsafe extern "C" fn capi_datetime_date_from_date(
	year: c_int,
	month: c_int,
	day: c_int,
	type_: *mut ForeignTypeObject,
) -> *mut PyObject {
	let Some(callee) = (unsafe { datetime_constructor_type(type_, "PyDateTimeAPI->Date_FromDate") })
	else {
		return ptr::null_mut();
	};
	let Some(mut args) = (unsafe { datetime_int_args3(year, month, day) }) else {
		return ptr::null_mut();
	};
	new_reference(unsafe { abi::pon_call(callee, args.as_mut_ptr(), args.len()) })
}

unsafe extern "C" fn capi_datetime_datetime_from_date_and_time(
	year: c_int,
	month: c_int,
	day: c_int,
	hour: c_int,
	minute: c_int,
	second: c_int,
	usecond: c_int,
	tzinfo: *mut PyObject,
	type_: *mut ForeignTypeObject,
) -> *mut PyObject {
	unsafe {
		call_datetime_datetime_constructor(
			year, month, day, hour, minute, second, usecond, tzinfo, None, type_,
		)
	}
}

unsafe extern "C" fn capi_datetime_datetime_from_date_and_time_and_fold(
	year: c_int,
	month: c_int,
	day: c_int,
	hour: c_int,
	minute: c_int,
	second: c_int,
	usecond: c_int,
	tzinfo: *mut PyObject,
	fold: c_int,
	type_: *mut ForeignTypeObject,
) -> *mut PyObject {
	unsafe {
		call_datetime_datetime_constructor(
			year,
			month,
			day,
			hour,
			minute,
			second,
			usecond,
			tzinfo,
			Some(fold),
			type_,
		)
	}
}

unsafe fn call_datetime_datetime_constructor(
	year: c_int,
	month: c_int,
	day: c_int,
	hour: c_int,
	minute: c_int,
	second: c_int,
	usecond: c_int,
	tzinfo: *mut PyObject,
	fold: Option<c_int>,
	type_: *mut ForeignTypeObject,
) -> *mut PyObject {
	let Some(callee) =
		(unsafe { datetime_constructor_type(type_, "PyDateTimeAPI->DateTime_FromDateAndTime") })
	else {
		return ptr::null_mut();
	};
	let Some(tzinfo) = (unsafe { datetime_tzinfo_arg(tzinfo) }) else {
		return ptr::null_mut();
	};
	let Some(mut args) = (unsafe {
		datetime_int_args7_with_object(year, month, day, hour, minute, second, usecond, tzinfo)
	}) else {
		return ptr::null_mut();
	};
	if let Some(fold) = fold {
		let Some(mut fold_values) = (unsafe { datetime_int_args1(fold) }) else {
			return ptr::null_mut();
		};
		let fold_name = [intern("fold")];
		let result = unsafe {
			abi::call::pon_call_ex(
				callee,
				args.as_mut_ptr(),
				args.len(),
				ptr::null_mut(),
				fold_name.as_ptr(),
				fold_values.as_mut_ptr(),
				fold_values.len(),
				ptr::null_mut(),
				ptr::null_mut(),
			)
		};
		return new_reference(result);
	}
	new_reference(unsafe { abi::pon_call(callee, args.as_mut_ptr(), args.len()) })
}

unsafe extern "C" fn capi_datetime_time_from_time(
	hour: c_int,
	minute: c_int,
	second: c_int,
	usecond: c_int,
	tzinfo: *mut PyObject,
	type_: *mut ForeignTypeObject,
) -> *mut PyObject {
	unsafe { call_datetime_time_constructor(hour, minute, second, usecond, tzinfo, None, type_) }
}

unsafe extern "C" fn capi_datetime_time_from_time_and_fold(
	hour: c_int,
	minute: c_int,
	second: c_int,
	usecond: c_int,
	tzinfo: *mut PyObject,
	fold: c_int,
	type_: *mut ForeignTypeObject,
) -> *mut PyObject {
	unsafe {
		call_datetime_time_constructor(hour, minute, second, usecond, tzinfo, Some(fold), type_)
	}
}

unsafe fn call_datetime_time_constructor(
	hour: c_int,
	minute: c_int,
	second: c_int,
	usecond: c_int,
	tzinfo: *mut PyObject,
	fold: Option<c_int>,
	type_: *mut ForeignTypeObject,
) -> *mut PyObject {
	let Some(callee) = (unsafe { datetime_constructor_type(type_, "PyDateTimeAPI->Time_FromTime") })
	else {
		return ptr::null_mut();
	};
	let Some(tzinfo) = (unsafe { datetime_tzinfo_arg(tzinfo) }) else {
		return ptr::null_mut();
	};
	let Some(mut args) =
		(unsafe { datetime_int_args4_with_object(hour, minute, second, usecond, tzinfo) })
	else {
		return ptr::null_mut();
	};
	if let Some(fold) = fold {
		let Some(mut fold_values) = (unsafe { datetime_int_args1(fold) }) else {
			return ptr::null_mut();
		};
		let fold_name = [intern("fold")];
		let result = unsafe {
			abi::call::pon_call_ex(
				callee,
				args.as_mut_ptr(),
				args.len(),
				ptr::null_mut(),
				fold_name.as_ptr(),
				fold_values.as_mut_ptr(),
				fold_values.len(),
				ptr::null_mut(),
				ptr::null_mut(),
			)
		};
		return new_reference(result);
	}
	new_reference(unsafe { abi::pon_call(callee, args.as_mut_ptr(), args.len()) })
}

unsafe extern "C" fn capi_datetime_delta_from_delta(
	days: c_int,
	seconds: c_int,
	useconds: c_int,
	normalize: c_int,
	type_: *mut ForeignTypeObject,
) -> *mut PyObject {
	if normalize != 1 {
		raise_not_implemented(
			"PyDateTimeAPI->Delta_FromDelta with normalize=0 is not implemented by Pon's \
			 Python-backed datetime shim",
		);
		return ptr::null_mut();
	}
	let Some(callee) =
		(unsafe { datetime_constructor_type(type_, "PyDateTimeAPI->Delta_FromDelta") })
	else {
		return ptr::null_mut();
	};
	let Some(mut args) = (unsafe { datetime_int_args3(days, seconds, useconds) }) else {
		return ptr::null_mut();
	};
	new_reference(unsafe { abi::pon_call(callee, args.as_mut_ptr(), args.len()) })
}

unsafe fn datetime_constructor_type(
	type_: *mut ForeignTypeObject,
	symbol: &str,
) -> Option<*mut PyObject> {
	if type_.is_null() {
		raise_type_error(&format!("{symbol} received NULL type"));
		return None;
	}
	let Some(native) = twin::native_of_foreign(type_) else {
		raise_type_error(&format!("{symbol} received a type object that is not registered with Pon"));
		return None;
	};
	Some(native.cast::<PyObject>())
}

unsafe fn datetime_tzinfo_arg(tzinfo: *mut PyObject) -> Option<*mut PyObject> {
	if tzinfo.is_null() {
		let none = unsafe { abi::pon_none() };
		return (!none.is_null()).then_some(none);
	}
	Some(crate::tag::untag_arg(tzinfo))
}

unsafe fn datetime_int_arg(value: c_int) -> Option<*mut PyObject> {
	let object = unsafe { abi::pon_const_int(i64::from(value)) };
	(!object.is_null()).then_some(object)
}

unsafe fn datetime_int_args1(a: c_int) -> Option<[*mut PyObject; 1]> {
	Some([unsafe { datetime_int_arg(a) }?])
}

unsafe fn datetime_int_args3(a: c_int, b: c_int, c: c_int) -> Option<[*mut PyObject; 3]> {
	Some([unsafe { datetime_int_arg(a) }?, unsafe { datetime_int_arg(b) }?, unsafe {
		datetime_int_arg(c)
	}?])
}

unsafe fn datetime_int_args7_with_object(
	a: c_int,
	b: c_int,
	c: c_int,
	d: c_int,
	e: c_int,
	f: c_int,
	g: c_int,
	object: *mut PyObject,
) -> Option<[*mut PyObject; 8]> {
	Some([
		unsafe { datetime_int_arg(a) }?,
		unsafe { datetime_int_arg(b) }?,
		unsafe { datetime_int_arg(c) }?,
		unsafe { datetime_int_arg(d) }?,
		unsafe { datetime_int_arg(e) }?,
		unsafe { datetime_int_arg(f) }?,
		unsafe { datetime_int_arg(g) }?,
		object,
	])
}

unsafe fn datetime_int_args4_with_object(
	a: c_int,
	b: c_int,
	c: c_int,
	d: c_int,
	object: *mut PyObject,
) -> Option<[*mut PyObject; 5]> {
	Some([
		unsafe { datetime_int_arg(a) }?,
		unsafe { datetime_int_arg(b) }?,
		unsafe { datetime_int_arg(c) }?,
		unsafe { datetime_int_arg(d) }?,
		object,
	])
}

unsafe extern "C" fn capi_datetime_unsupported_timezone_from_timezone(
	_offset: *mut PyObject,
	_name: *mut PyObject,
) -> *mut PyObject {
	raise_not_implemented(
		"PyDateTimeAPI->TimeZone_FromTimeZone is not implemented by Pon's numpy datetime C-API \
		 surface",
	);
	ptr::null_mut()
}

unsafe extern "C" fn capi_datetime_datetime_from_timestamp(
	cls: *mut PyObject,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	if !kwargs.is_null() && kwargs != unsafe { abi::pon_none() } {
		raise_not_implemented(
			"PyDateTimeAPI->DateTime_FromTimestamp with keyword arguments is not implemented",
		);
		return ptr::null_mut();
	}
	let Some(cls) =
		(unsafe { datetime_type_arg(cls, datetime_datetime_type(), "DateTime_FromTimestamp") })
	else {
		return ptr::null_mut();
	};
	let Some((timestamp, tzinfo)) =
		(unsafe { datetime_timestamp_args(args, true, "DateTime_FromTimestamp") })
	else {
		return ptr::null_mut();
	};
	let Some(parts) = timestamp_to_parts(timestamp, tzinfo == datetime_utc()) else {
		return ptr::null_mut();
	};
	alloc_datetime_datetime(
		cls,
		parts.year,
		parts.month,
		parts.day,
		parts.hour,
		parts.minute,
		parts.second,
		parts.microsecond,
		0,
		tzinfo,
	)
}

unsafe extern "C" fn capi_datetime_date_from_timestamp(
	cls: *mut PyObject,
	args: *mut PyObject,
) -> *mut PyObject {
	let Some(cls) = (unsafe { datetime_type_arg(cls, datetime_date_type(), "Date_FromTimestamp") })
	else {
		return ptr::null_mut();
	};
	let Some((timestamp, _tzinfo)) =
		(unsafe { datetime_timestamp_args(args, false, "Date_FromTimestamp") })
	else {
		return ptr::null_mut();
	};
	let Some(parts) = timestamp_to_parts(timestamp, false) else {
		return ptr::null_mut();
	};
	alloc_datetime_date(cls, parts.year, parts.month, parts.day)
}

struct TimestampParts {
	year:        c_int,
	month:       c_int,
	day:         c_int,
	hour:        c_int,
	minute:      c_int,
	second:      c_int,
	microsecond: c_int,
}

unsafe fn datetime_type_arg(
	cls: *mut PyObject,
	default: *mut PyType,
	symbol: &str,
) -> Option<*mut PyType> {
	if cls.is_null() {
		return Some(default);
	}
	if let Some(native) = twin::registered_native_of_foreign(cls.cast::<ForeignTypeObject>()) {
		return Some(native);
	}
	let cls = crate::tag::untag_arg(cls);
	if !cls.is_null()
		&& crate::tag::is_heap(cls)
		&& unsafe { crate::types::type_::is_type_object(cls) }
	{
		return Some(cls.cast::<PyType>());
	}
	raise_type_error(&format!("PyDateTimeAPI->{symbol} received a non-type cls"));
	None
}

unsafe fn datetime_timestamp_args(
	args: *mut PyObject,
	allow_tz: bool,
	symbol: &str,
) -> Option<(f64, *mut PyObject)> {
	let positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
		Ok(positional) => positional,
		Err(message) => {
			raise_type_error(&message);
			return None;
		},
	};
	let max = if allow_tz { 2 } else { 1 };
	if positional.is_empty() || positional.len() > max {
		raise_type_error(&format!(
			"PyDateTimeAPI->{symbol} expected 1{} positional argument{}, got {}",
			if allow_tz { " or 2" } else { "" },
			if allow_tz { "s" } else { "" },
			positional.len()
		));
		return None;
	}
	let timestamp = unsafe { timestamp_to_f64(positional[0]) }?;
	let tzinfo = positional
		.get(1)
		.copied()
		.unwrap_or_else(|| unsafe { abi::pon_none() });
	if tzinfo.is_null() {
		return None;
	}
	if allow_tz && tzinfo != unsafe { abi::pon_none() } && tzinfo != datetime_utc() {
		raise_not_implemented(
			"PyDateTimeAPI->DateTime_FromTimestamp only supports tz=None or datetime.timezone.utc",
		);
		return None;
	}
	Some((timestamp, tzinfo))
}

unsafe fn timestamp_to_f64(object: *mut PyObject) -> Option<f64> {
	let object = crate::tag::untag_arg(object);
	if let Some(value) = unsafe { crate::types::float::to_f64(object) } {
		return Some(value);
	}
	if let Some(value) = unsafe { crate::types::int::to_bigint_including_bool(object) } {
		return value.to_f64().or_else(|| {
			raise_type_error("timestamp is too large to convert to float");
			None
		});
	}
	raise_type_error("timestamp must be int or float");
	None
}

fn timestamp_to_parts(timestamp: f64, utc: bool) -> Option<TimestampParts> {
	if !timestamp.is_finite() {
		raise_value_error("timestamp out of range for platform time_t");
		return None;
	}
	let mut seconds = timestamp.floor();
	let mut microsecond = ((timestamp - seconds) * 1_000_000.0).round() as c_int;
	if microsecond >= 1_000_000 {
		seconds += 1.0;
		microsecond -= 1_000_000;
	}
	if seconds < libc::time_t::MIN as f64 || seconds > libc::time_t::MAX as f64 {
		raise_value_error("timestamp out of range for platform time_t");
		return None;
	}
	let time = seconds as libc::time_t;
	let mut out: libc::tm = unsafe { mem::zeroed() };
	let tm = if utc {
		unsafe { libc::gmtime_r(&time, &mut out) }
	} else {
		unsafe { libc::localtime_r(&time, &mut out) }
	};
	if tm.is_null() {
		raise_value_error("timestamp out of range for platform time conversion");
		return None;
	}
	Some(TimestampParts {
		year: out.tm_year + 1900,
		month: out.tm_mon + 1,
		day: out.tm_mday,
		hour: out.tm_hour,
		minute: out.tm_min,
		second: out.tm_sec,
		microsecond,
	})
}

unsafe extern "C" fn capi_capsule_new(
	pointer: *mut c_void,
	name: *const c_char,
	destructor: PyCapsuleDestructor,
) -> *mut PyObject {
	if pointer.is_null() {
		raise_value_error("PyCapsule_New called with null pointer");
		return ptr::null_mut();
	}
	let info = GcTypeInfo {
		size:     mem::size_of::<PyCapsule>(),
		trace:    trace_capsule,
		finalize: Some(finalize_capsule),
	};
	let block = match abi::alloc_gc_object(TYPE_ID_CAPI_CAPSULE, info) {
		Ok(block) => block,
		Err(message) => return raise_system_error_null(&message),
	};
	let capsule = block.cast::<PyCapsule>();
	unsafe {
		ptr::write(capsule, PyCapsule {
			ob_base: PyObjectHeader::new(capsule_type()),
			pointer,
			name,
			destructor,
			context: ptr::null_mut(),
		});
	}
	new_reference(capsule.cast::<PyObject>())
}

unsafe extern "C" fn trace_capsule(_object: *mut u8, _visitor: &mut dyn FnMut(*mut u8)) {}

unsafe extern "C" fn finalize_capsule(object: *mut u8) {
	if object.is_null() {
		return;
	}
	let capsule = unsafe { &mut *object.cast::<PyCapsule>() };
	let Some(destructor) = capsule.destructor.take() else {
		return;
	};
	unsafe { destructor(object.cast::<PyObject>()) };
}

unsafe extern "C" fn capi_capsule_get_pointer(
	capsule: *mut PyObject,
	name: *const c_char,
) -> *mut c_void {
	let Some(capsule) = (unsafe { checked_capsule(capsule, name, "PyCapsule_GetPointer") }) else {
		return ptr::null_mut();
	};
	capsule.pointer
}

unsafe extern "C" fn capi_capsule_is_valid(capsule: *mut PyObject, name: *const c_char) -> c_int {
	let Some(capsule) = (unsafe { capsule_ref(capsule) }) else {
		return 0;
	};
	(!capsule.pointer.is_null() && unsafe { capsule_name_matches(capsule.name, name) }) as c_int
}

unsafe extern "C" fn capi_capsule_set_context(
	capsule: *mut PyObject,
	context: *mut c_void,
) -> c_int {
	let Some(capsule) = (unsafe { checked_capsule_any_name(capsule, "PyCapsule_SetContext") })
	else {
		return -1;
	};
	capsule.context = context;
	0
}

unsafe extern "C" fn capi_capsule_get_context(capsule: *mut PyObject) -> *mut c_void {
	let Some(capsule) = (unsafe { checked_capsule_any_name(capsule, "PyCapsule_GetContext") })
	else {
		return ptr::null_mut();
	};
	capsule.context
}

unsafe extern "C" fn capi_capsule_import(name: *const c_char, _no_block: c_int) -> *mut c_void {
	let Some(full_name) = c_string(name) else {
		return raise_value_error_null("PyCapsule_Import called with invalid name");
	};
	if full_name == DATETIME_CAPSULE_NAME {
		return unsafe { capi_datetime_capi_import() };
	}
	let mut parts = full_name.split('.');
	let Some(module_name) = parts.next().filter(|part| !part.is_empty()) else {
		return raise_value_error_null("PyCapsule_Import called with invalid name");
	};
	let mut object = import_module_text(module_name);
	if object.is_null() {
		return ptr::null_mut();
	}
	for attr in parts {
		if attr.is_empty() {
			return raise_value_error_null("PyCapsule_Import called with invalid name");
		}
		object = unsafe { abi::pon_get_attr(object, intern(attr), ptr::null_mut()) };
		if object.is_null() {
			return ptr::null_mut();
		}
	}
	unsafe { capi_capsule_get_pointer(object, name) }
}

unsafe extern "C" fn capi_import_import_module(name: *const c_char) -> *mut PyObject {
	let Some(name) = c_string(name) else {
		return raise_import_error_null("PyImport_ImportModule called with invalid module name");
	};
	new_reference(import_module_text(&name))
}

/// `PyImport_Import`: object-name variant of PyImport_ImportModule.
unsafe extern "C" fn capi_import_import(name: *mut PyObject) -> *mut PyObject {
	let Some(text) = (unsafe { crate::types::type_::unicode_text(crate::tag::untag_arg(name)) })
	else {
		return raise_import_error_null("PyImport_Import expects a str module name");
	};
	new_reference(import_module_text(&text.to_owned()))
}

unsafe extern "C" fn capi_import_import_module_level(
	name: *const c_char,
	_globals: *mut PyObject,
	_locals: *mut PyObject,
	fromlist: *mut PyObject,
	level: c_int,
) -> *mut PyObject {
	let Some(name) = c_string(name) else {
		return raise_import_error_null("PyImport_ImportModuleLevel called with invalid module name");
	};
	let fromlist = crate::tag::untag_arg(fromlist);
	let none = unsafe { abi::pon_none() };
	let has_fromlist = !fromlist.is_null() && fromlist != none;
	let star = [intern("*")];
	let (fromlist_ptr, fromlist_len) = if has_fromlist {
		(star.as_ptr(), star.len())
	} else {
		(ptr::null(), 0)
	};
	let level = if level <= 0 { 0 } else { level as u32 };
	new_reference(unsafe {
		crate::import::pon_import_name(intern(&name), fromlist_ptr, fromlist_len, level)
	})
}

/// `PyCapsule_SetName`: replaces the stored name pointer (caller keeps the
/// storage alive, CPython contract).
unsafe extern "C" fn capi_capsule_set_name(capsule: *mut PyObject, name: *const c_char) -> c_int {
	let Some(capsule) = (unsafe { checked_capsule_any_name(capsule, "PyCapsule_SetName") }) else {
		return -1;
	};
	capsule.name = name;
	0
}

unsafe extern "C" fn capi_import_add_module(name: *const c_char) -> *mut PyObject {
	let Some(name) = c_string(name) else {
		return abi::return_null_with_error("PyImport_AddModule called with invalid module name");
	};
	let name_id = intern(&name);
	if let Some(module) = crate::import::cached_module(name_id) {
		return module;
	}
	match crate::import::install_module(&name, []) {
		Ok(module) => module,
		Err(message) => abi::return_null_with_error(message),
	}
}

unsafe extern "C" fn capi_module_get_dict(module: *mut PyObject) -> *mut PyObject {
	let Some(module_name) = crate::import::module_object_registry_key(module) else {
		return raise_system_error_null("PyModule_GetDict called with non-module object");
	};
	match crate::dynexec::module_namespace_dict(module_name) {
		Ok(dict) => dict,
		Err(message) => abi::return_null_with_error(message),
	}
}

unsafe extern "C" fn capi_module_get_state(module: *mut PyObject) -> *mut c_void {
	{
		let mut states = MODULE_STATES
			.lock()
			.unwrap_or_else(|poison| poison.into_inner());
		if let Some(state) = states.get_mut(&(module as usize)) {
			return state.as_mut_ptr().cast::<c_void>();
		}
	}
	if module.is_null()
		|| !crate::tag::is_heap(module)
		|| crate::import::module_object_registry_key(module).is_none()
	{
		raise_system_error("PyModule_GetState called with non-module object");
	}
	ptr::null_mut()
}

unsafe extern "C" fn capi_module_get_name(module: *mut PyObject) -> *const c_char {
	let Some(module_name) = crate::import::module_object_registry_key(module) else {
		raise_system_error("PyModule_GetName called with non-module object");
		return ptr::null();
	};
	let Some(name) = resolve(module_name) else {
		raise_system_error("PyModule_GetName could not resolve module name");
		return ptr::null();
	};
	cached_c_string(module_name, &name)
}

unsafe extern "C" fn capi_sys_get_object(name: *const c_char) -> *mut PyObject {
	let Some(name) = c_string(name) else {
		return ptr::null_mut();
	};
	let sys = import_module_text("sys");
	if sys.is_null() {
		return ptr::null_mut();
	}
	let object = unsafe { abi::pon_get_attr(sys, intern(&name), ptr::null_mut()) };
	if object.is_null() {
		pon_err_clear();
	}
	object
}

fn import_module_text(name: &str) -> *mut PyObject {
	let name_id = intern(name);
	let fromlist = [intern("*")];
	unsafe { crate::import::pon_import_name(name_id, fromlist.as_ptr(), fromlist.len(), 0) }
}

unsafe fn is_none_object(object: *mut PyObject) -> bool {
	let object = crate::tag::untag_arg(object);
	object.is_null() || object == unsafe { abi::pon_none() }
}

unsafe fn is_exact_generator_family(object: *mut PyObject) -> bool {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || !crate::tag::is_heap(object) {
		return false;
	}
	let type_type = abi::runtime_type_type();
	if type_type.is_null() {
		return false;
	}
	let generator = ensure_generator_type(type_type).cast_const();
	let coroutine = ensure_coroutine_type(type_type).cast_const();
	let async_generator = ensure_async_generator_type(type_type).cast_const();
	unsafe {
		is_exact_type(object, generator)
			|| is_exact_type(object, coroutine)
			|| is_exact_type(object, async_generator)
	}
}

fn finish_iter_send_return(presult: *mut *mut PyObject) -> PySendResult {
	if abi::exc::pending_exception_is("StopIteration") {
		let value = unsafe { abi::r#gen::pon_gen_stop_value() };
		if value.is_null() {
			return PYGEN_ERROR;
		}
		unsafe { *presult = new_reference(value) };
		return PYGEN_RETURN;
	}
	if abi::exc::pending_exception_is("StopAsyncIteration") {
		pon_err_clear();
		let none = unsafe { abi::pon_none() };
		if none.is_null() {
			return PYGEN_ERROR;
		}
		unsafe { *presult = new_reference(none) };
		return PYGEN_RETURN;
	}
	PYGEN_ERROR
}

unsafe extern "C" fn capi_iter_send(
	iter: *mut PyObject,
	arg: *mut PyObject,
	presult: *mut *mut PyObject,
) -> PySendResult {
	if presult.is_null() {
		raise_system_error("PyIter_Send result pointer must not be NULL");
		return PYGEN_ERROR;
	}
	unsafe { *presult = ptr::null_mut() };
	let iter = crate::tag::untag_arg(iter);
	let arg = if arg.is_null() {
		unsafe { abi::pon_none() }
	} else {
		crate::tag::untag_arg(arg)
	};
	if arg.is_null() {
		return PYGEN_ERROR;
	}
	if unsafe { is_exact_generator_family(iter) } {
		let result = unsafe { abi::r#gen::pon_gen_send(iter, arg) };
		if !result.is_null() {
			unsafe { *presult = new_reference(result) };
			return PYGEN_NEXT;
		}
		return finish_iter_send_return(presult);
	}
	if !unsafe { is_none_object(arg) } {
		raise_type_error("PyIter_Send with a non-None value requires a generator or coroutine");
		return PYGEN_ERROR;
	}
	let result = unsafe { abi::pon_iter_next(iter, ptr::null_mut()) };
	if !result.is_null() {
		unsafe { *presult = new_reference(result) };
		return PYGEN_NEXT;
	}
	finish_iter_send_return(presult)
}

unsafe extern "C" fn capi_async_gen_check_exact(object: *mut PyObject) -> c_int {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || !crate::tag::is_heap(object) {
		return 0;
	}
	let type_type = abi::runtime_type_type();
	if type_type.is_null() {
		return 0;
	}
	let async_generator = ensure_async_generator_type(type_type).cast_const();
	unsafe { is_exact_type(object, async_generator) as c_int }
}

#[cfg(test)]
unsafe extern "C" fn capi_test_collect_pin_count(object: *mut PyObject) -> isize {
	match abi::collect() {
		Ok(()) => super::pin_count(object) as isize,
		Err(message) => {
			raise_system_error(&message);
			-1
		},
	}
}

unsafe fn capsule_ref<'a>(capsule: *mut PyObject) -> Option<&'a mut PyCapsule> {
	if capsule.is_null() || !crate::tag::is_heap(capsule) {
		return None;
	}
	// SAFETY: The heap-tag guard above makes the object header readable.
	if unsafe { (*capsule).ob_type } != capsule_type() {
		return None;
	}
	// SAFETY: Capsule objects are allocated by `capi_capsule_new` with this layout.
	Some(unsafe { &mut *capsule.cast::<PyCapsule>() })
}

unsafe fn checked_capsule<'a>(
	capsule: *mut PyObject,
	name: *const c_char,
	api: &str,
) -> Option<&'a mut PyCapsule> {
	let capsule_ref = unsafe { checked_capsule_any_name(capsule, api) }?;
	if unsafe { capsule_name_matches(capsule_ref.name, name) } {
		return Some(capsule_ref);
	}
	raise_value_error(&format!("{api} called with incorrect name"));
	None
}

unsafe fn checked_capsule_any_name<'a>(
	capsule: *mut PyObject,
	api: &str,
) -> Option<&'a mut PyCapsule> {
	let Some(capsule_ref) = (unsafe { capsule_ref(capsule) }) else {
		raise_value_error(&format!("{api} called with invalid PyCapsule object"));
		return None;
	};
	if capsule_ref.pointer.is_null() {
		raise_value_error(&format!("{api} called with invalid PyCapsule object"));
		return None;
	}
	Some(capsule_ref)
}

unsafe fn capsule_name_matches(stored: *const c_char, requested: *const c_char) -> bool {
	if stored.is_null() || requested.is_null() {
		return stored == requested;
	}
	// SAFETY: PyCapsule names are process-lifetime NUL-terminated C strings per
	// CPython's API contract.
	let stored = unsafe { CStr::from_ptr(stored) }.to_bytes();
	// SAFETY: Caller supplies a NUL-terminated name pointer for comparison.
	let requested = unsafe { CStr::from_ptr(requested) }.to_bytes();
	stored == requested
}

fn cached_c_string(key: u32, text: &str) -> *const c_char {
	static CACHE: LazyLock<Mutex<HashMap<u32, usize>>> =
		LazyLock::new(|| Mutex::new(HashMap::new()));
	let mut cache = CACHE.lock().unwrap_or_else(|poison| poison.into_inner());
	if let Some(&ptr) = cache.get(&key) {
		return ptr as *const c_char;
	}
	let Ok(c_string) = CString::new(text) else {
		return ptr::null();
	};
	let ptr = c_string.into_raw() as usize;
	cache.insert(key, ptr);
	ptr as *const c_char
}

fn raise_value_error_null(message: &str) -> *mut c_void {
	raise_value_error(message);
	ptr::null_mut()
}

fn raise_import_error_null(message: &str) -> *mut PyObject {
	let _ = abi::exc::raise_kind_error_text(ExceptionKind::ImportError, message);
	ptr::null_mut()
}

fn raise_import_error_void(message: &str) -> *mut c_void {
	let _ = abi::exc::raise_kind_error_text(ExceptionKind::ImportError, message);
	ptr::null_mut()
}

fn raise_system_error_null(message: &str) -> *mut PyObject {
	raise_system_error(message);
	ptr::null_mut()
}

fn raise_value_error(message: &str) {
	let _ = abi::exc::raise_kind_error_text(ExceptionKind::ValueError, message);
}

fn raise_system_error(message: &str) {
	let _ = abi::exc::raise_kind_error_text(ExceptionKind::SystemError, message);
}

fn raise_type_error(message: &str) {
	let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message);
}

fn raise_not_implemented(message: &str) {
	let _ = abi::exc::raise_kind_error_text(ExceptionKind::NotImplementedError, message);
}

fn pending_error_detail() -> String {
	pon_err_message().unwrap_or_else(|| "unknown error".to_owned())
}

#[cfg(test)]
mod tests {
	use core::ptr;

	use super::super::{
		load_extension_module,
		tests::{ResetImportStateOnDrop, TempExtensionRoot, compile_extension},
	};
	use crate::{
		abi::{format_object_for_print, pon_call, pon_runtime_init},
		import::{module_attr, reset_import_state_for_tests},
		intern::intern,
		thread_state::{pon_err_message, test_state_lock},
		types::exc::PyBaseException,
	};

	#[test]
	fn runtime_family_c_api_load_test() {
		let _guard = test_state_lock();
		let _reset = ResetImportStateOnDrop;
		unsafe {
			assert_eq!(pon_runtime_init(), 0);
		}

		let temp = TempExtensionRoot::new();
		let module_path = compile_extension(
			&temp,
			"capi_runtime_test_ext",
			r#"
#include <Python.h>

static int capsule_payload = 123;
static int capsule_context = 19;
static int capsule_destructor_count = 0;

static void capsule_destructor(PyObject *capsule) {
    if (PyCapsule_CheckExact(capsule)) {
        capsule_destructor_count += 1;
    }
}

static PyObject *fail(const char *message) {
    PyErr_SetString(PyExc_RuntimeError, message);
    return NULL;
}

static PyObject *capsule_roundtrip(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    PyObject *capsule = PyCapsule_New(&capsule_payload, "pon.runtime.payload", NULL);
    if (capsule == NULL) {
        return NULL;
    }
    if (!PyCapsule_IsValid(capsule, "pon.runtime.payload")) {
        return fail("capsule did not validate");
    }
    if (PyCapsule_IsValid(capsule, "pon.runtime.wrong")) {
        return fail("capsule validated with wrong name");
    }
    if (PyCapsule_SetContext(capsule, &capsule_context) < 0) {
        return NULL;
    }
    if (PyCapsule_GetContext(capsule) != &capsule_context) {
        return fail("capsule context did not round-trip");
    }
    void *wrong = PyCapsule_GetPointer(capsule, "pon.runtime.wrong");
    if (wrong != NULL) {
        return fail("capsule wrong-name lookup unexpectedly succeeded");
    }
    if (!PyErr_ExceptionMatches(PyExc_ValueError)) {
        return NULL;
    }
    PyErr_Clear();
    int *payload = (int *)PyCapsule_GetPointer(capsule, "pon.runtime.payload");
    if (payload == NULL) {
        return NULL;
    }
    return PyLong_FromLong(*payload);
}

static PyObject *make_destructor_capsule(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    return PyCapsule_New(&capsule_payload, "pon.runtime.destructor", capsule_destructor);
}

static PyObject *destructor_count(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    return PyLong_FromLong(capsule_destructor_count);
}

static PyObject *format_error_value(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    PyObject *result = PyErr_Format(PyExc_TypeError, "bad %s %d %zd %u %c %%", "thing", -7, (Py_ssize_t)8, 9u, 'X');
    if (result != NULL) {
        return fail("PyErr_Format returned a non-NULL object");
    }
    if (!PyErr_ExceptionMatches(PyExc_TypeError)) {
        return NULL;
    }
    PyObject *type = NULL;
    PyObject *value = NULL;
    PyObject *tb = NULL;
    PyErr_Fetch(&type, &value, &tb);
    if (type == NULL || value == NULL || tb != NULL) {
        return fail("PyErr_Fetch did not return the expected type/value/tb triple");
    }
    if (!PyErr_GivenExceptionMatches(type, PyExc_TypeError)) {
        return fail("fetched exception type did not match TypeError");
    }
    return value;
}

static PyObject *exception_matches_subclass(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    PyErr_SetString(PyExc_IndexError, "index boom");
    int ok = PyErr_ExceptionMatches(PyExc_LookupError);
    int no = PyErr_ExceptionMatches(PyExc_OverflowError);
    PyErr_Clear();
    return PyLong_FromLong(ok == 1 && no == 0);
}

static PyObject *thread_bracket(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    PyThreadState *save = PyEval_SaveThread();
    if (save == NULL) {
        return fail("PyEval_SaveThread returned NULL");
    }
    PyEval_RestoreThread(save);
    PyGILState_STATE gil = PyGILState_Ensure();
    PyGILState_Release(gil);
    Py_BEGIN_ALLOW_THREADS
    Py_END_ALLOW_THREADS
    return PyLong_FromLong(1);
}

static PyObject *import_sys_maxsize(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    PyObject *sys = PyImport_ImportModule("sys");
    if (sys == NULL) {
        return NULL;
    }
    PyObject *added = PyImport_AddModule("runtime_added");
    if (added == NULL) {
        return NULL;
    }
    const char *name = PyModule_GetName(added);
    if (name == NULL || strcmp(name, "runtime_added") != 0) {
        return fail("PyModule_GetName did not return runtime_added");
    }
    if (PyModule_GetDict(added) == NULL) {
        return NULL;
    }
    if (PyModule_GetState(added) != NULL) {
        return fail("PyModule_GetState should report unsupported state as NULL");
    }
    PyObject *maxsize = PySys_GetObject("maxsize");
    if (maxsize == NULL) {
        return fail("PySys_GetObject did not find sys.maxsize");
    }
    Py_INCREF(maxsize);
    return maxsize;
}

static PyMethodDef methods[] = {
    {"capsule_roundtrip", capsule_roundtrip, METH_NOARGS, "exercise capsules"},
    {"make_destructor_capsule", make_destructor_capsule, METH_NOARGS, "create a capsule with a destructor"},
    {"destructor_count", destructor_count, METH_NOARGS, "report destructor calls"},
    {"format_error_value", format_error_value, METH_NOARGS, "return a fetched formatted error value"},
    {"exception_matches_subclass", exception_matches_subclass, METH_NOARGS, "exercise exception subclass matching"},
    {"thread_bracket", thread_bracket, METH_NOARGS, "exercise thread no-op brackets"},
    {"import_sys_maxsize", import_sys_maxsize, METH_NOARGS, "exercise import/sys/module helpers"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_runtime_test_ext",
    "Pon runtime C-API test extension",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_runtime_test_ext(void) {
    return PyModule_Create(&module);
}
"#,
		);

		let module = load_extension_module("capi_runtime_test_ext", &module_path)
			.unwrap_or_else(|message| panic!("failed to load C extension: {message}"));
		assert!(!module.is_null(), "extension loader returned NULL module");

		let module_name = intern("capi_runtime_test_ext");
		assert_noargs_text(module_name, "capsule_roundtrip", "123");
		assert_noargs_text(module_name, "exception_matches_subclass", "1");
		assert_noargs_text(module_name, "thread_bracket", "1");
		assert_noargs_text(module_name, "import_sys_maxsize", "9223372036854775807");

		let value = call_noargs(module_name, "format_error_value");
		let message = unsafe { (*value.cast::<PyBaseException>()).message };
		assert_eq!(format_object_for_print(message).as_deref(), Ok("bad thing -7 8 9 X %"));

		create_and_drop_destructor_capsule(module_name);
		crate::abi::collect().expect("first collect");
		crate::abi::collect().expect("second collect");
		assert_noargs_text(module_name, "destructor_count", "1");

		reset_import_state_for_tests();
	}

	#[test]
	fn runtime_structural_c_api_test() {
		let _guard = test_state_lock();
		let _reset = ResetImportStateOnDrop;
		unsafe {
			assert_eq!(pon_runtime_init(), 0);
		}

		let temp = TempExtensionRoot::new();
		let module_path = compile_extension(
			&temp,
			"capi_runtime_structural_ext",
			r#"
#include <Python.h>
#include <frameobject.h>
#include <traceback.h>

enum {
    THREAD_STATE_STABLE = 1L << 0,
    THREAD_STATE_INTERP_MAIN = 1L << 1,
    THREAD_STATE_FRAME_NULL_NO_ERROR = 1L << 2,
    EVAL_SAVE_RESTORE = 1L << 3,
    MUTEX_SEQUENCE = 1L << 4,
    VECTORCALL_NARGS_MASK = 1L << 5,
    CONTEXTVAR_CONSTRUCTOR_DEFAULT = 1L << 6,
    CONTEXTVAR_EXPLICIT_DEFAULT = 1L << 7,
    CONTEXTVAR_NULL_DEFAULT = 1L << 8,
    BUILTINS_LEN = 1L << 9,
    FRAME_BACK_SYSTEM_ERROR = 1L << 10,
    FRAME_CODE_SYSTEM_ERROR = 1L << 11,
    FRAME_NEW_CODE_ROUNDTRIP = 1L << 12,
    FRAME_NEW_BACK_NULL = 1L << 13,
    TRACEBACK_HERE_PRESERVES_EXCEPTION = 1L << 14
};

static PyObject *runtime_structural_mask(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;

    long mask = 0;

    PyThreadState *first = PyThreadState_Get();
    PyThreadState *second = PyThreadState_Get();
    if (first != NULL && first == second) {
        mask |= THREAD_STATE_STABLE;
    }
    if (first != NULL && first->interp == PyInterpreterState_Main()) {
        mask |= THREAD_STATE_INTERP_MAIN;
    }

    PyFrameObject *current_frame = PyThreadState_GetFrame(first);
    if (current_frame == NULL && PyErr_Occurred() == NULL) {
        mask |= THREAD_STATE_FRAME_NULL_NO_ERROR;
    } else if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }

    PyThreadState *saved = PyEval_SaveThread();
    if (saved != NULL) {
        mask |= EVAL_SAVE_RESTORE;
        PyEval_RestoreThread(saved);
    }

    PyMutex mutex = {0};
    PyMutex_Lock(&mutex);
    PyMutex_Unlock(&mutex);
    PyMutex_Lock(&mutex);
    PyMutex_Unlock(&mutex);
    mask |= MUTEX_SEQUENCE;

    if (PyVectorcall_NARGS(PY_VECTORCALL_ARGUMENTS_OFFSET | (size_t)37) == 37) {
        mask |= VECTORCALL_NARGS_MASK;
    }

    PyObject *constructor_default = PyLong_FromLong(17);
    if (constructor_default == NULL) {
        return NULL;
    }
    PyObject *with_constructor_default = PyContextVar_New("with_constructor_default", constructor_default);
    if (with_constructor_default == NULL) {
        return NULL;
    }
    PyObject *value = NULL;
    if (PyContextVar_Get(with_constructor_default, NULL, &value) == 0 && value == constructor_default) {
        mask |= CONTEXTVAR_CONSTRUCTOR_DEFAULT;
    }

    PyObject *without_default = PyContextVar_New("without_default", NULL);
    if (without_default == NULL) {
        return NULL;
    }
    PyObject *explicit_default = PyLong_FromLong(29);
    if (explicit_default == NULL) {
        return NULL;
    }
    value = NULL;
    if (PyContextVar_Get(without_default, explicit_default, &value) == 0 && value == explicit_default) {
        mask |= CONTEXTVAR_EXPLICIT_DEFAULT;
    }
    value = constructor_default;
    if (PyContextVar_Get(without_default, NULL, &value) == 0 && value == NULL) {
        mask |= CONTEXTVAR_NULL_DEFAULT;
    }

    PyObject *builtins = PyEval_GetBuiltins();
    if (builtins != NULL && PyDict_Check(builtins) && PyDict_GetItemString(builtins, "len") != NULL) {
        mask |= BUILTINS_LEN;
    }

    PyFrameObject *back = PyFrame_GetBack(NULL);
    if (back == NULL && PyErr_ExceptionMatches(PyExc_SystemError)) {
        mask |= FRAME_BACK_SYSTEM_ERROR;
    }
    PyErr_Clear();

    PyCodeObject *code = PyFrame_GetCode(NULL);
    if (code == NULL && PyErr_ExceptionMatches(PyExc_SystemError)) {
        mask |= FRAME_CODE_SYSTEM_ERROR;
    }
    PyErr_Clear();

    PyCodeObject *made_code = PyCode_NewEmpty("capi_file.py", "capi_func", 123);
    PyFrameObject *made_frame = made_code == NULL ? NULL : PyFrame_New(first, made_code, NULL, NULL);
    PyCodeObject *roundtrip_code = made_frame == NULL ? NULL : PyFrame_GetCode(made_frame);
    if (made_code != NULL && made_frame != NULL && roundtrip_code == made_code) {
        mask |= FRAME_NEW_CODE_ROUNDTRIP;
    } else if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }
    Py_XDECREF(roundtrip_code);

    PyFrameObject *made_back = made_frame == NULL ? NULL : PyFrame_GetBack(made_frame);
    if (made_frame != NULL && made_back == NULL && PyErr_Occurred() == NULL) {
        mask |= FRAME_NEW_BACK_NULL;
    } else if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }
    Py_XDECREF(made_back);

    if (made_frame != NULL) {
        made_frame->f_lineno = 321;
        PyErr_SetString(PyExc_ValueError, "traceback marker");
        if (PyTraceBack_Here(made_frame) == 0 && PyErr_ExceptionMatches(PyExc_ValueError)) {
            PyObject *ptype = NULL;
            PyObject *pvalue = NULL;
            PyObject *ptb = NULL;
            PyErr_Fetch(&ptype, &pvalue, &ptb);
            if (ptype != NULL
                    && PyErr_GivenExceptionMatches(ptype, PyExc_ValueError)
                    && ptb != NULL) {
                mask |= TRACEBACK_HERE_PRESERVES_EXCEPTION;
            }
            Py_XDECREF(ptype);
            Py_XDECREF(pvalue);
            Py_XDECREF(ptb);
        } else {
            PyErr_Clear();
        }
    }
    Py_XDECREF(made_frame);
    Py_XDECREF(made_code);

    if (PyErr_Occurred() != NULL) {
        return NULL;
    }
    return PyLong_FromLong(mask);
}

static PyMethodDef methods[] = {
    {"runtime_structural_mask", runtime_structural_mask, METH_NOARGS, "exercise structural runtime C-API helpers"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_runtime_structural_ext",
    "Pon structural runtime C-API test extension",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_runtime_structural_ext(void) {
    return PyModule_Create(&module);
}
"#,
		);

		let module = load_extension_module("capi_runtime_structural_ext", &module_path)
			.unwrap_or_else(|message| {
				panic!("failed to load structural runtime C extension: {message}")
			});
		assert!(!module.is_null(), "extension loader returned NULL module");

		let module_name = intern("capi_runtime_structural_ext");
		assert_noargs_text(module_name, "runtime_structural_mask", "16383");

		reset_import_state_for_tests();
	}

	#[test]
	fn runtime_datetime_c_api_load_test() {
		let _guard = test_state_lock();
		let _reset = ResetImportStateOnDrop;
		unsafe {
			assert_eq!(pon_runtime_init(), 0);
		}

		let temp = TempExtensionRoot::new();
		let module_path = compile_extension(
			&temp,
			"capi_runtime_datetime_ext",
			r#"
#include <Python.h>
#include <datetime.h>

enum {
    IMPORT_OK = 1L << 0,
    API_TYPES = 1L << 1,
    DATE_CONSTRUCT = 1L << 2,
    DATE_YMD = 1L << 3,
    DATE_CHECKS = 1L << 4,
    DATE_TWIN_INSTANCE = 1L << 5,
    DATETIME_CONSTRUCT = 1L << 6,
    DATETIME_FIELDS = 1L << 7,
    DATETIME_CHECKS = 1L << 8,
    DELTA_CONSTRUCT = 1L << 9,
    DELTA_FIELDS = 1L << 10,
    DELTA_CHECKS = 1L << 11,
    TIME_CONSTRUCT = 1L << 12,
    TIME_FIELDS = 1L << 13,
    TIME_CHECKS = 1L << 14,
    UTC_TZINFO = 1L << 15,
    CAPSULE_DIRECT = 1L << 16,
    EXACT_CHECKS = 1L << 17,
    DATETIME_FROM_TIMESTAMP_LOCAL = 1L << 18,
    DATE_FROM_TIMESTAMP_LOCAL = 1L << 19,
    DATETIME_FROM_TIMESTAMP_UTC = 1L << 20
};

static void clear_unexpected_error(void) {
    if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }
}

static PyObject *datetime_mask(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;

    long mask = 0;
    PyDateTime_IMPORT;
    if (PyDateTimeAPI == NULL) {
        clear_unexpected_error();
        return PyLong_FromLong(mask);
    }
    mask |= IMPORT_OK;

    if (PyDateTimeAPI->DateType != NULL &&
            PyDateTimeAPI->DateTimeType != NULL &&
            PyDateTimeAPI->TimeType != NULL &&
            PyDateTimeAPI->DeltaType != NULL &&
            PyDateTimeAPI->TZInfoType != NULL &&
            PyDateTime_TimeZone_UTC != NULL) {
        mask |= API_TYPES;
    }

    void *direct = PyCapsule_Import(PyDateTime_CAPSULE_NAME, 0);
    if (direct == PyDateTimeAPI) {
        mask |= CAPSULE_DIRECT;
    } else {
        clear_unexpected_error();
    }

    PyObject *date = PyDate_FromDate(2020, 1, 2);
    if (date != NULL) {
        mask |= DATE_CONSTRUCT;
        if (PyDateTime_GET_YEAR(date) == 2020 &&
                PyDateTime_GET_MONTH(date) == 1 &&
                PyDateTime_GET_DAY(date) == 2 &&
                PyErr_Occurred() == NULL) {
            mask |= DATE_YMD;
        } else {
            clear_unexpected_error();
        }
        if (PyDate_Check(date) == 1 && PyDateTime_Check(date) == 0) {
            mask |= DATE_CHECKS;
        } else {
            clear_unexpected_error();
        }
        if (PyObject_IsInstance(date, (PyObject *)PyDateTimeAPI->DateType) == 1) {
            mask |= DATE_TWIN_INSTANCE;
        } else {
            clear_unexpected_error();
        }
    } else {
        clear_unexpected_error();
    }

    PyObject *dt = PyDateTime_FromDateAndTime(2020, 1, 2, 3, 4, 5, 6);
    if (dt != NULL) {
        mask |= DATETIME_CONSTRUCT;
        if (PyDateTime_GET_YEAR(dt) == 2020 &&
                PyDateTime_GET_MONTH(dt) == 1 &&
                PyDateTime_GET_DAY(dt) == 2 &&
                PyDateTime_DATE_GET_HOUR(dt) == 3 &&
                PyDateTime_DATE_GET_MINUTE(dt) == 4 &&
                PyDateTime_DATE_GET_SECOND(dt) == 5 &&
                PyDateTime_DATE_GET_MICROSECOND(dt) == 6 &&
                PyErr_Occurred() == NULL) {
            mask |= DATETIME_FIELDS;
        } else {
            clear_unexpected_error();
        }
        if (PyDateTime_Check(dt) == 1 && PyDate_Check(dt) == 1) {
            mask |= DATETIME_CHECKS;
        } else {
            clear_unexpected_error();
        }
    } else {
        clear_unexpected_error();
    }

    if (date != NULL && dt != NULL &&
            PyDate_CheckExact(date) &&
            !PyDateTime_CheckExact(date) &&
            PyDateTime_CheckExact(dt) &&
            !PyDate_CheckExact(dt)) {
        mask |= EXACT_CHECKS;
    }

    PyObject *delta = PyDelta_FromDSU(1, 2, 3);
    if (delta != NULL) {
        mask |= DELTA_CONSTRUCT;
        if (PyDateTime_DELTA_GET_DAYS(delta) == 1 &&
                PyDateTime_DELTA_GET_SECONDS(delta) == 2 &&
                PyDateTime_DELTA_GET_MICROSECONDS(delta) == 3 &&
                PyErr_Occurred() == NULL) {
            mask |= DELTA_FIELDS;
        } else {
            clear_unexpected_error();
        }
        if (PyDelta_Check(delta) == 1 &&
                PyDelta_CheckExact(delta) &&
                PyObject_IsInstance(delta, (PyObject *)PyDateTimeAPI->DeltaType) == 1) {
            mask |= DELTA_CHECKS;
        } else {
            clear_unexpected_error();
        }
    } else {
        clear_unexpected_error();
    }

    PyObject *time = PyTime_FromTime(4, 5, 6, 7);
    if (time != NULL) {
        mask |= TIME_CONSTRUCT;
        if (PyDateTime_TIME_GET_HOUR(time) == 4 &&
                PyDateTime_TIME_GET_MINUTE(time) == 5 &&
                PyDateTime_TIME_GET_SECOND(time) == 6 &&
                PyDateTime_TIME_GET_MICROSECOND(time) == 7 &&
                PyErr_Occurred() == NULL) {
            mask |= TIME_FIELDS;
        } else {
            clear_unexpected_error();
        }
        if (PyTime_Check(time) == 1 && PyTime_CheckExact(time)) {
            mask |= TIME_CHECKS;
        } else {
            clear_unexpected_error();
        }
    } else {
        clear_unexpected_error();
    }

    if (PyTZInfo_Check(PyDateTime_TimeZone_UTC) == 1 &&
            PyObject_IsInstance(PyDateTime_TimeZone_UTC, (PyObject *)PyDateTimeAPI->TZInfoType) == 1) {
        mask |= UTC_TZINFO;
    } else {
        clear_unexpected_error();
    }


    PyObject *epoch = PyLong_FromLong(0);
    PyObject *timestamp_args = epoch == NULL ? NULL : PyTuple_Pack(1, epoch);
    PyObject *timestamp_dt = timestamp_args == NULL ? NULL : PyDateTime_FromTimestamp(timestamp_args);
    if (timestamp_dt != NULL && PyDateTime_CheckExact(timestamp_dt)) {
        mask |= DATETIME_FROM_TIMESTAMP_LOCAL;
    } else {
        clear_unexpected_error();
    }

    PyObject *timestamp_date = timestamp_args == NULL ? NULL : PyDate_FromTimestamp(timestamp_args);
    if (timestamp_date != NULL && PyDate_CheckExact(timestamp_date) && !PyDateTime_CheckExact(timestamp_date)) {
        mask |= DATE_FROM_TIMESTAMP_LOCAL;
    } else {
        clear_unexpected_error();
    }

    PyObject *utc_args = epoch == NULL ? NULL : PyTuple_Pack(2, epoch, PyDateTime_TimeZone_UTC);
    PyObject *utc_dt = utc_args == NULL ? NULL
        : PyDateTimeAPI->DateTime_FromTimestamp((PyObject *)PyDateTimeAPI->DateTimeType, utc_args, NULL);
    if (utc_dt != NULL
            && PyDateTime_CheckExact(utc_dt)
            && PyDateTime_GET_YEAR(utc_dt) == 1970
            && PyDateTime_GET_MONTH(utc_dt) == 1
            && PyDateTime_GET_DAY(utc_dt) == 1
            && PyDateTime_DATE_GET_HOUR(utc_dt) == 0
            && PyDateTime_DATE_GET_MINUTE(utc_dt) == 0
            && PyDateTime_DATE_GET_SECOND(utc_dt) == 0) {
        mask |= DATETIME_FROM_TIMESTAMP_UTC;
    } else {
        clear_unexpected_error();
    }
    Py_XDECREF(utc_dt);
    Py_XDECREF(utc_args);
    Py_XDECREF(timestamp_date);
    Py_XDECREF(timestamp_dt);
    Py_XDECREF(timestamp_args);
    Py_XDECREF(epoch);
    clear_unexpected_error();
    return PyLong_FromLong(mask);
}

static PyMethodDef methods[] = {
    {"datetime_mask", datetime_mask, METH_NOARGS, "exercise datetime C-API shim"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_runtime_datetime_ext",
    "Pon datetime C-API test extension",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_runtime_datetime_ext(void) {
    return PyModule_Create(&module);
}
"#,
		);

		let module = load_extension_module("capi_runtime_datetime_ext", &module_path)
			.unwrap_or_else(|message| panic!("failed to load datetime C extension: {message}"));
		assert!(!module.is_null(), "extension loader returned NULL module");

		let module_name = intern("capi_runtime_datetime_ext");
		assert_noargs_text(module_name, "datetime_mask", "2097151");

		reset_import_state_for_tests();
	}

	#[inline(never)]
	fn create_and_drop_destructor_capsule(module_name: u32) {
		let capsule = call_noargs(module_name, "make_destructor_capsule");
		assert!(!capsule.is_null(), "make_destructor_capsule returned NULL");
	}

	fn assert_noargs_text(module_name: u32, method_name: &str, expected: &str) {
		let result = call_noargs(module_name, method_name);
		assert_eq!(format_object_for_print(result).as_deref(), Ok(expected));
	}

	fn call_noargs(module_name: u32, method_name: &str) -> *mut crate::object::PyObject {
		let method = module_attr(module_name, intern(method_name))
			.unwrap_or_else(|| panic!("{method_name} method registered"));
		let result = unsafe { pon_call(method, ptr::null_mut(), 0) };
		assert!(!result.is_null(), "{method_name} returned NULL: {:?}", pon_err_message());
		result
	}
}
