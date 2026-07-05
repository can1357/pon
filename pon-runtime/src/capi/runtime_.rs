//! Runtime family: memory-adjacent process services, capsules, imports, modules, and sys access.

use core::ffi::{c_char, c_int, c_void};
use core::mem;
use core::ptr;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::sync::{LazyLock, Mutex};

use crate::abi;
use crate::intern::{intern, resolve};
use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::thread_state::pon_err_clear;
use crate::types::exc::ExceptionKind;

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
    crate::native::contextvars::capi_contextvar_new(&name, default)
}

unsafe extern "C" fn capi_contextvar_get(
    var: *mut PyObject,
    default: *mut PyObject,
    value: *mut *mut PyObject,
) -> c_int {
    unsafe { crate::native::contextvars::capi_contextvar_get(var, default, value) }
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
    Box::into_raw(Box::new(PyCapsule {
        ob_base: PyObjectHeader::new(capsule_type()),
        pointer,
        name,
        destructor,
        context: ptr::null_mut(),
    }))
    .cast::<PyObject>()
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
    import_module_text(&name)
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
