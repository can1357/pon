//! Runtime family: memory-adjacent process services, capsules, imports, modules, and sys access.

use core::ffi::{c_char, c_int, c_void};
use core::mem;
use core::ptr;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::sync::{LazyLock, Mutex};

use num_traits::ToPrimitive;

use crate::abi;
use crate::intern::{intern, resolve};
use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::thread_state::{pon_err_clear, pon_err_message, pon_err_occurred};
use crate::types::exc::ExceptionKind;

use super::twin::{self, ForeignTypeObject};
use super::{PyModuleDef, c_string};

pub(crate) type PyCapsuleDestructor = Option<unsafe extern "C" fn(*mut PyObject)>;

/// C mirror: `include/pon_capi/runtime.h` `PyPonCapiRuntime`.
#[repr(C)]
pub(crate) struct PyPonCapiRuntime {
    eval_save_thread: unsafe extern "C" fn() -> *mut c_void,
    eval_restore_thread: unsafe extern "C" fn(*mut c_void),
    capsule_new: unsafe extern "C" fn(*mut c_void, *const c_char, PyCapsuleDestructor) -> *mut PyObject,
    capsule_get_pointer: unsafe extern "C" fn(*mut PyObject, *const c_char) -> *mut c_void,
    capsule_is_valid: unsafe extern "C" fn(*mut PyObject, *const c_char) -> c_int,
    capsule_set_context: unsafe extern "C" fn(*mut PyObject, *mut c_void) -> c_int,
    capsule_get_context: unsafe extern "C" fn(*mut PyObject) -> *mut c_void,
    capsule_import: unsafe extern "C" fn(*const c_char, c_int) -> *mut c_void,
    import_import_module: unsafe extern "C" fn(*const c_char) -> *mut PyObject,
    import_add_module: unsafe extern "C" fn(*const c_char) -> *mut PyObject,
    module_get_dict: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    module_get_state: unsafe extern "C" fn(*mut PyObject) -> *mut c_void,
    module_get_name: unsafe extern "C" fn(*mut PyObject) -> *const c_char,
    sys_get_object: unsafe extern "C" fn(*const c_char) -> *mut PyObject,
    module_def_init: unsafe extern "C" fn(*mut PyModuleDef) -> *mut PyObject,
    thread_state_get: unsafe extern "C" fn() -> *mut c_void,
    thread_state_get_frame: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    interpreter_state_main: unsafe extern "C" fn() -> *mut c_void,
    eval_get_builtins: unsafe extern "C" fn() -> *mut PyObject,
    frame_get_back: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    frame_get_code: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    contextvar_new: unsafe extern "C" fn(*const c_char, *mut PyObject) -> *mut PyObject,
    contextvar_get: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut *mut PyObject) -> c_int,
    datetime_capi_import: unsafe extern "C" fn() -> *mut c_void,
    datetime_get_attr_int: unsafe extern "C" fn(*mut PyObject, *const c_char) -> c_int,
    capsule_set_name: unsafe extern "C" fn(*mut PyObject, *const c_char) -> c_int,
    import_import: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    #[cfg(test)]
    test_collect_pin_count: unsafe extern "C" fn(*mut PyObject) -> isize,
    contextvar_set: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
}

unsafe impl Send for PyPonCapiRuntime {}
unsafe impl Sync for PyPonCapiRuntime {}

#[repr(C)]
struct PyCapsule {
    ob_base: PyObjectHeader,
    pointer: *mut c_void,
    name: *const c_char,
    destructor: PyCapsuleDestructor,
    context: *mut c_void,
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
struct PyThreadState {
    interp: *mut PyInterpreterState,
}

unsafe impl Send for PyThreadState {}
unsafe impl Sync for PyThreadState {}

#[repr(C)]
struct PyDateTimeCapi {
    date_type: *mut ForeignTypeObject,
    datetime_type: *mut ForeignTypeObject,
    time_type: *mut ForeignTypeObject,
    delta_type: *mut ForeignTypeObject,
    tzinfo_type: *mut ForeignTypeObject,
    timezone_utc: *mut PyObject,
    date_from_date: unsafe extern "C" fn(c_int, c_int, c_int, *mut ForeignTypeObject) -> *mut PyObject,
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
    time_from_time: unsafe extern "C" fn(c_int, c_int, c_int, c_int, *mut PyObject, *mut ForeignTypeObject) -> *mut PyObject,
    delta_from_delta: unsafe extern "C" fn(c_int, c_int, c_int, c_int, *mut ForeignTypeObject) -> *mut PyObject,
    timezone_from_timezone: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
    datetime_from_timestamp: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject,
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
    time_from_time_and_fold: unsafe extern "C" fn(c_int, c_int, c_int, c_int, *mut PyObject, c_int, *mut ForeignTypeObject) -> *mut PyObject,
}

unsafe impl Send for PyDateTimeCapi {}
unsafe impl Sync for PyDateTimeCapi {}

static DATETIME_CAPI: LazyLock<Mutex<Option<usize>>> = LazyLock::new(|| Mutex::new(None));
const DATETIME_CAPSULE_NAME: &str = "datetime.datetime_CAPI";

static MAIN_INTERPRETER_STATE: LazyLock<PyInterpreterState> = LazyLock::new(|| PyInterpreterState { _private: 0 });
static MAIN_THREAD_STATE: LazyLock<PyThreadState> = LazyLock::new(|| PyThreadState {
    interp: interpreter_state_main(),
});
static MODULE_STATES: LazyLock<Mutex<HashMap<usize, Box<[u8]>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));


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
    }
}

#[must_use]
pub(crate) fn capsule_type() -> *mut PyType {
    static CAPSULE_TYPE: LazyLock<usize> = LazyLock::new(|| {
        let ty = PyType::new(ptr::null(), "PyCapsule", mem::size_of::<PyCapsule>());
        Box::into_raw(Box::new(ty)) as usize
    });
    *CAPSULE_TYPE as *mut PyType
}

fn interpreter_state_main() -> *mut PyInterpreterState {
    ptr::from_ref(&*MAIN_INTERPRETER_STATE).cast_mut()
}

fn thread_state_singleton() -> *mut PyThreadState {
    ptr::from_ref(&*MAIN_THREAD_STATE).cast_mut()
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

unsafe extern "C" fn capi_frame_get_back(_frame: *mut c_void) -> *mut c_void {
    raise_system_error("PyFrame_GetBack is not implemented: Pon exposes no C-visible frame objects");
    ptr::null_mut()
}

unsafe extern "C" fn capi_frame_get_code(_frame: *mut c_void) -> *mut c_void {
    raise_system_error("PyFrame_GetCode is not implemented: Pon exposes no C-visible frame objects");
    ptr::null_mut()
}

unsafe extern "C" fn capi_contextvar_new(name: *const c_char, default: *mut PyObject) -> *mut PyObject {
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

unsafe extern "C" fn capi_contextvar_set(var: *mut PyObject, value: *mut PyObject) -> *mut PyObject {
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

unsafe extern "C" fn capi_datetime_get_attr_int(object: *mut PyObject, name: *const c_char) -> c_int {
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
        }
    }
}

fn datetime_capi_ptr() -> Result<*mut PyDateTimeCapi, String> {
    let mut cached = DATETIME_CAPI.lock().unwrap_or_else(|poison| poison.into_inner());
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
    year: c_int,
    month: c_int,
    day: c_int,
}

#[repr(C)]
struct PonDateTimeObject {
    ob_base: PyObjectHeader,
    year: c_int,
    month: c_int,
    day: c_int,
    hour: c_int,
    minute: c_int,
    second: c_int,
    microsecond: c_int,
    fold: c_int,
    tzinfo: *mut PyObject,
}

#[repr(C)]
struct PonTimeObject {
    ob_base: PyObjectHeader,
    hour: c_int,
    minute: c_int,
    second: c_int,
    microsecond: c_int,
    fold: c_int,
    tzinfo: *mut PyObject,
}

#[repr(C)]
struct PonDeltaObject {
    ob_base: PyObjectHeader,
    days: c_int,
    seconds: c_int,
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
        datetime_from_timestamp: capi_datetime_unsupported_datetime_from_timestamp,
        date_from_timestamp: capi_datetime_unsupported_date_from_timestamp,
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
        let mut ty = PyType::new(abi::runtime_type_type().cast_const(), "date", mem::size_of::<PonDateObject>());
        ty.tp_base = runtime_object_type();
        ty.tp_new = Some(pon_datetime_date_new);
        ty.tp_getattro = Some(pon_datetime_date_getattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    *TYPE as *mut PyType
}

fn datetime_datetime_type() -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(abi::runtime_type_type().cast_const(), "datetime", mem::size_of::<PonDateTimeObject>());
        ty.tp_base = datetime_date_type();
        ty.tp_new = Some(pon_datetime_datetime_new);
        ty.tp_getattro = Some(pon_datetime_datetime_getattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    *TYPE as *mut PyType
}

fn datetime_time_type() -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(abi::runtime_type_type().cast_const(), "time", mem::size_of::<PonTimeObject>());
        ty.tp_base = runtime_object_type();
        ty.tp_new = Some(pon_datetime_time_new);
        ty.tp_getattro = Some(pon_datetime_time_getattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    *TYPE as *mut PyType
}

fn datetime_delta_type() -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(abi::runtime_type_type().cast_const(), "timedelta", mem::size_of::<PonDeltaObject>());
        ty.tp_base = runtime_object_type();
        ty.tp_new = Some(pon_datetime_delta_new);
        ty.tp_getattro = Some(pon_datetime_delta_getattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    *TYPE as *mut PyType
}

fn datetime_tzinfo_type() -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(abi::runtime_type_type().cast_const(), "tzinfo", mem::size_of::<PyObjectHeader>());
        ty.tp_base = runtime_object_type();
        Box::into_raw(Box::new(ty)) as usize
    });
    *TYPE as *mut PyType
}

fn datetime_timezone_type() -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(abi::runtime_type_type().cast_const(), "timezone", mem::size_of::<PonTimezoneObject>());
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

unsafe extern "C" fn pon_datetime_date_new(cls: *mut PyType, args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
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

unsafe extern "C" fn pon_datetime_datetime_new(cls: *mut PyType, args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
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
    alloc_datetime_datetime(cls, year, month, day, hour, minute, second, microsecond, 0, positional[7])
}

unsafe extern "C" fn pon_datetime_time_new(cls: *mut PyType, args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
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

unsafe extern "C" fn pon_datetime_delta_new(cls: *mut PyType, args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
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
    let ty = if cls.is_null() { datetime_date_type() } else { cls };
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
    let ty = if cls.is_null() { datetime_datetime_type() } else { cls };
    let tzinfo = if tzinfo.is_null() { unsafe { abi::pon_none() } } else { tzinfo };
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
    let ty = if cls.is_null() { datetime_time_type() } else { cls };
    let tzinfo = if tzinfo.is_null() { unsafe { abi::pon_none() } } else { tzinfo };
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

fn alloc_datetime_delta(cls: *mut PyType, days: c_int, seconds: c_int, microseconds: c_int) -> *mut PyObject {
    let ty = if cls.is_null() { datetime_delta_type() } else { cls };
    Box::into_raw(Box::new(PonDeltaObject {
        ob_base: PyObjectHeader::new(ty.cast_const()),
        days,
        seconds,
        microseconds,
    }))
    .cast::<PyObject>()
}

unsafe fn datetime_positional_args(args: *mut PyObject, expected: usize, symbol: &str) -> Result<Vec<*mut PyObject>, ()> {
    match unsafe { crate::types::type_::positional_args_from_object(args) } {
        Ok(positional) if positional.len() == expected => Ok(positional),
        Ok(positional) => {
            raise_type_error(&format!("{symbol} expected {expected} positional arguments, got {}", positional.len()));
            Err(())
        }
        Err(message) => {
            raise_type_error(&message);
            Err(())
        }
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

unsafe extern "C" fn pon_datetime_date_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
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

unsafe extern "C" fn pon_datetime_datetime_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
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

unsafe extern "C" fn pon_datetime_time_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
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

unsafe extern "C" fn pon_datetime_delta_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
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
    let _ = abi::exc::raise_kind_error_text(ExceptionKind::AttributeError, &format!("datetime object has no attribute {name}"));
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
    let datetime = unsafe { capi_datetime_datetime_from_date_and_time(2020, 1, 2, 3, 4, 5, 6, none, capi.datetime_type) };
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

fn verify_datetime_attr(object: *mut PyObject, attr: &str, expected: c_int, label: &str) -> Result<(), String> {
    match unsafe { datetime_int_attr_raw(object, attr) } {
        Ok(actual) if actual == expected => Ok(()),
        Ok(actual) => Err(format!("PyDateTime_IMPORT {label} returned {actual}, expected {expected}")),
        Err(message) => {
            pon_err_clear();
            Err(format!("PyDateTime_IMPORT could not read {label}: {message}"))
        }
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
    let Some(callee) = (unsafe { datetime_constructor_type(type_, "PyDateTimeAPI->Date_FromDate") }) else {
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
    unsafe { call_datetime_datetime_constructor(year, month, day, hour, minute, second, usecond, tzinfo, None, type_) }
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
    unsafe { call_datetime_datetime_constructor(year, month, day, hour, minute, second, usecond, tzinfo, Some(fold), type_) }
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
    let Some(callee) = (unsafe { datetime_constructor_type(type_, "PyDateTimeAPI->DateTime_FromDateAndTime") }) else {
        return ptr::null_mut();
    };
    let Some(tzinfo) = (unsafe { datetime_tzinfo_arg(tzinfo) }) else {
        return ptr::null_mut();
    };
    let Some(mut args) = (unsafe { datetime_int_args7_with_object(year, month, day, hour, minute, second, usecond, tzinfo) }) else {
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
    unsafe { call_datetime_time_constructor(hour, minute, second, usecond, tzinfo, Some(fold), type_) }
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
    let Some(callee) = (unsafe { datetime_constructor_type(type_, "PyDateTimeAPI->Time_FromTime") }) else {
        return ptr::null_mut();
    };
    let Some(tzinfo) = (unsafe { datetime_tzinfo_arg(tzinfo) }) else {
        return ptr::null_mut();
    };
    let Some(mut args) = (unsafe { datetime_int_args4_with_object(hour, minute, second, usecond, tzinfo) }) else {
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
        raise_not_implemented("PyDateTimeAPI->Delta_FromDelta with normalize=0 is not implemented by Pon's Python-backed datetime shim");
        return ptr::null_mut();
    }
    let Some(callee) = (unsafe { datetime_constructor_type(type_, "PyDateTimeAPI->Delta_FromDelta") }) else {
        return ptr::null_mut();
    };
    let Some(mut args) = (unsafe { datetime_int_args3(days, seconds, useconds) }) else {
        return ptr::null_mut();
    };
    new_reference(unsafe { abi::pon_call(callee, args.as_mut_ptr(), args.len()) })
}

unsafe fn datetime_constructor_type(type_: *mut ForeignTypeObject, symbol: &str) -> Option<*mut PyObject> {
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
    Some([
        unsafe { datetime_int_arg(a) }?,
        unsafe { datetime_int_arg(b) }?,
        unsafe { datetime_int_arg(c) }?,
    ])
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

unsafe extern "C" fn capi_datetime_unsupported_timezone_from_timezone(_offset: *mut PyObject, _name: *mut PyObject) -> *mut PyObject {
    raise_not_implemented("PyDateTimeAPI->TimeZone_FromTimeZone is not implemented by Pon's numpy datetime C-API surface");
    ptr::null_mut()
}

unsafe extern "C" fn capi_datetime_unsupported_datetime_from_timestamp(
    _cls: *mut PyObject,
    _args: *mut PyObject,
    _kwargs: *mut PyObject,
) -> *mut PyObject {
    raise_not_implemented("PyDateTimeAPI->DateTime_FromTimestamp is not implemented by Pon's numpy datetime C-API surface");
    ptr::null_mut()
}

unsafe extern "C" fn capi_datetime_unsupported_date_from_timestamp(_cls: *mut PyObject, _args: *mut PyObject) -> *mut PyObject {
    raise_not_implemented("PyDateTimeAPI->Date_FromTimestamp is not implemented by Pon's numpy datetime C-API surface");
    ptr::null_mut()
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
    new_reference(
        Box::into_raw(Box::new(PyCapsule {
            ob_base: PyObjectHeader::new(capsule_type()),
            pointer,
            name,
            destructor,
            context: ptr::null_mut(),
        }))
        .cast::<PyObject>(),
    )
}

unsafe extern "C" fn capi_capsule_get_pointer(capsule: *mut PyObject, name: *const c_char) -> *mut c_void {
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

unsafe extern "C" fn capi_capsule_set_context(capsule: *mut PyObject, context: *mut c_void) -> c_int {
    let Some(capsule) = (unsafe { checked_capsule_any_name(capsule, "PyCapsule_SetContext") }) else {
        return -1;
    };
    capsule.context = context;
    0
}

unsafe extern "C" fn capi_capsule_get_context(capsule: *mut PyObject) -> *mut c_void {
    let Some(capsule) = (unsafe { checked_capsule_any_name(capsule, "PyCapsule_GetContext") }) else {
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
    let Some(text) = (unsafe { crate::types::type_::unicode_text(crate::tag::untag_arg(name)) }) else {
        return raise_import_error_null("PyImport_Import expects a str module name");
    };
    new_reference(import_module_text(&text.to_owned()))
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
        let mut states = MODULE_STATES.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(state) = states.get_mut(&(module as usize)) {
            return state.as_mut_ptr().cast::<c_void>();
        }
    }
    if module.is_null() || !crate::tag::is_heap(module) || crate::import::module_object_registry_key(module).is_none() {
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

#[cfg(test)]
unsafe extern "C" fn capi_test_collect_pin_count(object: *mut PyObject) -> isize {
    match abi::collect() {
        Ok(()) => super::pin_count(object) as isize,
        Err(message) => {
            raise_system_error(&message);
            -1
        }
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

unsafe fn checked_capsule<'a>(capsule: *mut PyObject, name: *const c_char, api: &str) -> Option<&'a mut PyCapsule> {
    let capsule_ref = unsafe { checked_capsule_any_name(capsule, api) }?;
    if unsafe { capsule_name_matches(capsule_ref.name, name) } {
        return Some(capsule_ref);
    }
    raise_value_error(&format!("{api} called with incorrect name"));
    None
}

unsafe fn checked_capsule_any_name<'a>(capsule: *mut PyObject, api: &str) -> Option<&'a mut PyCapsule> {
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
    // SAFETY: PyCapsule names are process-lifetime NUL-terminated C strings per CPython's API contract.
    let stored = unsafe { CStr::from_ptr(stored) }.to_bytes();
    // SAFETY: Caller supplies a NUL-terminated name pointer for comparison.
    let requested = unsafe { CStr::from_ptr(requested) }.to_bytes();
    stored == requested
}

fn cached_c_string(key: u32, text: &str) -> *const c_char {
    static CACHE: LazyLock<Mutex<HashMap<u32, usize>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
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

    use super::super::tests::{compile_extension, ResetImportStateOnDrop, TempExtensionRoot};
    use super::super::load_extension_module;
    use crate::abi::{format_object_for_print, pon_call, pon_runtime_init};
    use crate::import::{module_attr, reset_import_state_for_tests};
    use crate::intern::intern;
    use crate::thread_state::{pon_err_message, test_state_lock};
    use crate::types::exc::PyBaseException;

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
    FRAME_CODE_SYSTEM_ERROR = 1L << 11
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
            .unwrap_or_else(|message| panic!("failed to load structural runtime C extension: {message}"));
        assert!(!module.is_null(), "extension loader returned NULL module");

        let module_name = intern("capi_runtime_structural_ext");
        assert_noargs_text(module_name, "runtime_structural_mask", "4095");

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
    EXACT_CHECKS = 1L << 17
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
        assert_noargs_text(module_name, "datetime_mask", "262143");

        reset_import_state_for_tests();
    }

    fn assert_noargs_text(module_name: u32, method_name: &str, expected: &str) {
        let result = call_noargs(module_name, method_name);
        assert_eq!(format_object_for_print(result).as_deref(), Ok(expected));
    }

    fn call_noargs(module_name: u32, method_name: &str) -> *mut crate::object::PyObject {
        let method = module_attr(module_name, intern(method_name)).unwrap_or_else(|| panic!("{method_name} method registered"));
        let result = unsafe { pon_call(method, ptr::null_mut(), 0) };
        assert!(
            !result.is_null(),
            "{method_name} returned NULL: {:?}",
            pon_err_message()
        );
        result
    }
}
