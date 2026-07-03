//! CPython-source compatibility shim for recompiled native extensions.
//!
//! This is not CPython's binary ABI. Extensions include Pon's `Python.h`, link
//! the bootstrap object once, and the loader injects this process's function
//! table before calling `PyInit_*`.

use core::ffi::{c_char, c_int, c_long, c_void};
use core::mem;
use core::ptr;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use num_traits::ToPrimitive;

use crate::abi;
use crate::intern::intern;
use crate::object::{CallFunc, PyObject, PyObjectHeader, PyType, as_object_ptr};
use crate::thread_state::{pon_err_message, pon_err_occurred, pon_err_set};
use crate::types::exc::ExceptionKind;

const METH_VARARGS: c_int = 0x0001;
const METH_KEYWORDS: c_int = 0x0002;
const METH_NOARGS: c_int = 0x0004;
const METH_O: c_int = 0x0008;

const PYTHON_API_VERSION: c_int = 1013;
const PON_EXCEPTION_RUNTIME_ERROR: usize = 1;
const PON_EXCEPTION_TYPE_ERROR: usize = 2;
const PON_EXCEPTION_VALUE_ERROR: usize = 3;
const PON_EXCEPTION_IMPORT_ERROR: usize = 4;

/// C signature used by classic `PyMethodDef` function entries.
pub type PyCFunction = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject;

type PyPonSetCapi = unsafe extern "C" fn(*const PyPonCapi) -> c_int;
type PyInitFunc = unsafe extern "C" fn() -> *mut PyObject;

/// Minimal `PyMethodDef` layout consumed by [`PyModuleDef`].
#[repr(C)]
pub struct PyMethodDef {
    /// NUL-terminated Python attribute name.
    pub ml_name: *const c_char,
    /// C callable implementing the method.
    pub ml_meth: Option<PyCFunction>,
    /// CPython `METH_*` flag mask.
    pub ml_flags: c_int,
    /// Optional NUL-terminated docstring.
    pub ml_doc: *const c_char,
}

/// Prefix used by CPython's `PyModuleDef_HEAD_INIT` initializer.
#[repr(C)]
pub struct PyModuleDefBase {
    ob_base: PyObjectHeader,
    m_init: *mut c_void,
    m_index: isize,
    m_copy: *mut PyObject,
}

/// Minimal single-phase module definition accepted by `PyModule_Create2`.
#[repr(C)]
pub struct PyModuleDef {
    base: PyModuleDefBase,
    m_name: *const c_char,
    m_doc: *const c_char,
    m_size: isize,
    m_methods: *const PyMethodDef,
    m_slots: *mut c_void,
    m_traverse: *mut c_void,
    m_clear: *mut c_void,
    m_free: *mut c_void,
}

/// Function table injected into recompiled extension modules.
#[repr(C)]
pub struct PyPonCapi {
    module_create2: unsafe extern "C" fn(*mut PyModuleDef, c_int) -> *mut PyObject,
    module_add_object: unsafe extern "C" fn(*mut PyObject, *const c_char, *mut PyObject) -> c_int,
    long_from_long: unsafe extern "C" fn(c_long) -> *mut PyObject,
    long_as_long: unsafe extern "C" fn(*mut PyObject) -> c_long,
    unicode_from_string: unsafe extern "C" fn(*const c_char) -> *mut PyObject,
    inc_ref: unsafe extern "C" fn(*mut PyObject),
    dec_ref: unsafe extern "C" fn(*mut PyObject),
    none: unsafe extern "C" fn() -> *mut PyObject,
    err_set_string: unsafe extern "C" fn(*mut PyObject, *const c_char),
    err_occurred: unsafe extern "C" fn() -> *mut PyObject,
    exc_runtime_error: *mut PyObject,
    exc_type_error: *mut PyObject,
    exc_value_error: *mut PyObject,
    exc_import_error: *mut PyObject,
}

unsafe impl Sync for PyPonCapi {}
unsafe impl Send for PyPonCapi {}

#[repr(C)]
struct PyCFunctionObject {
    ob_base: PyObjectHeader,
    method: PyCFunction,
    flags: c_int,
    self_object: *mut PyObject,
    name: u32,
}

static C_FUNCTION_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(ptr::null(), "builtin_function_or_method", mem::size_of::<PyCFunctionObject>());
    ty.tp_call = Some(cfunction_call as CallFunc);
    Box::into_raw(Box::new(ty)) as usize
});

static CAPI_PINS: LazyLock<Mutex<HashMap<usize, usize>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static EXTENSION_HANDLES: LazyLock<Mutex<Vec<usize>>> = LazyLock::new(|| Mutex::new(Vec::new()));

static PON_CAPI: PyPonCapi = PyPonCapi {
    module_create2: py_module_create2,
    module_add_object: py_module_add_object,
    long_from_long: py_long_from_long,
    long_as_long: py_long_as_long,
    unicode_from_string: py_unicode_from_string,
    inc_ref: py_inc_ref,
    dec_ref: py_dec_ref,
    none: py_none,
    err_set_string: py_err_set_string,
    err_occurred: py_err_occurred,
    exc_runtime_error: PON_EXCEPTION_RUNTIME_ERROR as *mut PyObject,
    exc_type_error: PON_EXCEPTION_TYPE_ERROR as *mut PyObject,
    exc_value_error: PON_EXCEPTION_VALUE_ERROR as *mut PyObject,
    exc_import_error: PON_EXCEPTION_IMPORT_ERROR as *mut PyObject,
};

/// Extension suffixes Pon will consider for source-recompiled modules.
#[must_use]
pub fn extension_suffixes() -> &'static [&'static str] {
    #[cfg(target_os = "macos")]
    {
        &[".pon.so", ".cpython-314-darwin.so", ".abi3.so", ".so"]
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        &[".pon.so", ".cpython-314-x86_64-linux-gnu.so", ".abi3.so", ".so"]
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        &[".pon.so", ".cpython-314-aarch64-linux-gnu.so", ".abi3.so", ".so"]
    }
    #[cfg(not(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))))]
    {
        &[".pon.so", ".so"]
    }
}

/// Current C-extension pins exposed to the collector as explicit roots.
#[must_use]
pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
    CAPI_PINS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .keys()
        .copied()
        .filter(|&addr| addr >= 4096)
        .map(|addr| addr as *mut PyObject)
        .filter(|&object| crate::tag::is_heap(object))
        .collect()
}

/// Loads a source-recompiled extension module and calls its `PyInit_*` entry.
pub(crate) fn load_extension_module(name: &str, path: &Path) -> Result<*mut PyObject, String> {
    let path_text = path
        .to_str()
        .ok_or_else(|| format!("extension path is not UTF-8: {}", path.display()))?;
    let c_path = CString::new(path_text).map_err(|_| format!("extension path contains NUL: {}", path.display()))?;
    let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        return Err(format!("failed to load extension '{}': {}", path.display(), dlerror_text()));
    }

    let set_capi = unsafe { symbol::<PyPonSetCapi>(handle, "PyPon_SetCapi") }?;
    let set_result = unsafe { set_capi(&raw const PON_CAPI) };
    if set_result != 0 {
        unsafe { libc::dlclose(handle) };
        return Err(format!("extension '{}' rejected Pon C API table", path.display()));
    }

    let short_name = name.rsplit('.').next().unwrap_or(name);
    let init_symbol = format!("PyInit_{short_name}");
    let init = unsafe { symbol::<PyInitFunc>(handle, &init_symbol) }?;
    let module = unsafe { init() };
    if module.is_null() {
        let message = if pon_err_occurred() {
            pon_err_message().unwrap_or_else(|| "extension init failed".to_owned())
        } else {
            "extension init returned NULL without setting an exception".to_owned()
        };
        unsafe { libc::dlclose(handle) };
        return Err(message);
    }

    EXTENSION_HANDLES
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .push(handle as usize);
    Ok(module)
}

unsafe fn symbol<T: Copy>(handle: *mut c_void, name: &str) -> Result<T, String> {
    let c_name = CString::new(name).map_err(|_| format!("symbol name contains NUL: {name}"))?;
    let ptr = unsafe { libc::dlsym(handle, c_name.as_ptr()) };
    if ptr.is_null() {
        return Err(format!("missing extension symbol '{name}': {}", dlerror_text()));
    }
    Ok(unsafe { mem::transmute_copy(&ptr) })
}

fn dlerror_text() -> String {
    let error = unsafe { libc::dlerror() };
    if error.is_null() {
        "unknown dynamic loader error".to_owned()
    } else {
        unsafe { CStr::from_ptr(error) }.to_string_lossy().into_owned()
    }
}

unsafe extern "C" fn py_module_create2(def: *mut PyModuleDef, api_version: c_int) -> *mut PyObject {
    if api_version != PYTHON_API_VERSION || def.is_null() {
        return abi::return_null_with_error("invalid PyModuleDef");
    }
    let def_ref = unsafe { &*def };
    if !def_ref.m_slots.is_null() {
        return abi::return_null_with_error("multi-phase extension modules are not supported yet");
    }
    let Some(name) = c_string(def_ref.m_name) else {
        return abi::return_null_with_error("module definition has no name");
    };
    let mut attrs = Vec::new();
    if let Some(doc) = c_string(def_ref.m_doc) {
        let doc_object = unsafe { abi::pon_const_str(doc.as_ptr(), doc.len()) };
        if doc_object.is_null() {
            return ptr::null_mut();
        }
        attrs.push((intern("__doc__"), doc_object));
    }
    if !def_ref.m_methods.is_null() {
        let mut cursor = def_ref.m_methods;
        loop {
            let method = unsafe { &*cursor };
            if method.ml_name.is_null() {
                break;
            }
            let Some(method_name) = c_string(method.ml_name) else {
                return abi::return_null_with_error("method definition has invalid name");
            };
            let Some(function) = method.ml_meth else {
                return abi::return_null_with_error(format!("method '{method_name}' has no function"));
            };
            let object = alloc_cfunction(function, method.ml_flags, ptr::null_mut(), &method_name);
            attrs.push((intern(&method_name), object));
            cursor = unsafe { cursor.add(1) };
        }
    }
    match crate::import::install_module(&name, attrs) {
        Ok(module) => module,
        Err(message) => abi::return_null_with_error(message),
    }
}

unsafe extern "C" fn py_module_add_object(module: *mut PyObject, name: *const c_char, value: *mut PyObject) -> c_int {
    if module.is_null() || value.is_null() {
        pon_err_set("PyModule_AddObject received NULL".to_owned());
        return -1;
    }
    let Some(attr) = c_string(name) else {
        pon_err_set("PyModule_AddObject name is not valid UTF-8".to_owned());
        return -1;
    };
    let module = module.cast::<crate::import::PyModuleObject>();
    let module_name = unsafe { (*module).name };
    if crate::import::store_module_attr(module_name, intern(&attr), value) {
        0
    } else {
        pon_err_set(format!("PyModule_AddObject target is not a module for '{attr}'"));
        -1
    }
}

unsafe extern "C" fn py_long_from_long(value: c_long) -> *mut PyObject {
    crate::types::int::from_i64(value as i64)
}

unsafe extern "C" fn py_long_as_long(object: *mut PyObject) -> c_long {
    let object = crate::tag::untag_arg(object);
    match unsafe { crate::types::int::to_bigint_including_bool(object) }.and_then(|value| value.to_i64()) {
        Some(value) => value as c_long,
        None => {
            let _ = crate::abi::exc::raise_kind_error_text(ExceptionKind::TypeError, "an integer is required");
            -1
        }
    }
}

unsafe extern "C" fn py_unicode_from_string(value: *const c_char) -> *mut PyObject {
    let Some(text) = c_string(value) else {
        return abi::return_null_with_error("PyUnicode_FromString received invalid UTF-8");
    };
    unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

unsafe extern "C" fn py_inc_ref(object: *mut PyObject) {
    if object.is_null() || object.addr() < 4096 || !crate::tag::is_heap(object) {
        return;
    }
    let mut pins = CAPI_PINS.lock().unwrap_or_else(|poison| poison.into_inner());
    *pins.entry(object as usize).or_insert(0) += 1;
}

unsafe extern "C" fn py_dec_ref(object: *mut PyObject) {
    if object.is_null() || object.addr() < 4096 || !crate::tag::is_heap(object) {
        return;
    }
    let mut pins = CAPI_PINS.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(count) = pins.get_mut(&(object as usize)) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            pins.remove(&(object as usize));
        }
    }
}

unsafe extern "C" fn py_none() -> *mut PyObject {
    unsafe { abi::pon_none() }
}

unsafe extern "C" fn py_err_set_string(exception: *mut PyObject, message: *const c_char) {
    let text = c_string(message).unwrap_or_else(|| "C extension error".to_owned());
    let kind = match exception as usize {
        PON_EXCEPTION_TYPE_ERROR => ExceptionKind::TypeError,
        PON_EXCEPTION_VALUE_ERROR => ExceptionKind::ValueError,
        PON_EXCEPTION_IMPORT_ERROR => ExceptionKind::ImportError,
        _ => ExceptionKind::RuntimeError,
    };
    let _ = crate::abi::exc::raise_kind_error_text(kind, &text);
}

unsafe extern "C" fn py_err_occurred() -> *mut PyObject {
    if pon_err_occurred() {
        PON_EXCEPTION_RUNTIME_ERROR as *mut PyObject
    } else {
        ptr::null_mut()
    }
}

unsafe extern "C" fn cfunction_call(callee: *mut PyObject, args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
    if callee.is_null() {
        return abi::return_null_with_error("NULL C function object");
    }
    let function = unsafe { &*callee.cast::<PyCFunctionObject>() };
    let positional = match unsafe { tuple_args(args) } {
        Ok(values) => values,
        Err(message) => return abi::return_null_with_error(message),
    };
    if function.flags & METH_KEYWORDS != 0 {
        return abi::return_null_with_error("METH_KEYWORDS extension calls are not supported yet");
    }
    if function.flags & METH_NOARGS != 0 {
        if !positional.is_empty() {
            return abi::return_null_with_error(format!("{}() takes no arguments", crate::intern::resolve(function.name).unwrap_or_default()));
        }
        return unsafe { (function.method)(function.self_object, ptr::null_mut()) };
    }
    if function.flags & METH_O != 0 {
        if positional.len() != 1 {
            return abi::return_null_with_error(format!("{}() takes exactly one argument", crate::intern::resolve(function.name).unwrap_or_default()));
        }
        return unsafe { (function.method)(function.self_object, positional[0]) };
    }
    if function.flags & METH_VARARGS != 0 {
        let tuple = if args.is_null() {
            unsafe { abi::seq::pon_build_tuple(ptr::null_mut(), 0) }
        } else {
            args
        };
        return unsafe { (function.method)(function.self_object, tuple) };
    }
    abi::return_null_with_error("unsupported C function calling convention")
}

unsafe fn tuple_args<'a>(args: *mut PyObject) -> Result<&'a [*mut PyObject], String> {
    if args.is_null() {
        return Ok(&[]);
    }
    unsafe { crate::abi::seq::exact_tuple_slice(args) }.ok_or_else(|| "C function call args were not a tuple".to_owned())
}

fn alloc_cfunction(function: PyCFunction, flags: c_int, self_object: *mut PyObject, name: &str) -> *mut PyObject {
    let object = Box::new(PyCFunctionObject {
        ob_base: PyObjectHeader::new(*C_FUNCTION_TYPE as *const PyType),
        method: function,
        flags,
        self_object,
        name: intern(name),
    });
    as_object_ptr(Box::into_raw(object))
}

fn c_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::ffi::OsStr;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{self, Command, Output};
    use std::ptr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::load_extension_module;
    use crate::abi::{format_object_for_print, pon_call, pon_const_int, pon_none, pon_runtime_init};
    use crate::import::{module_attr, reset_import_state_for_tests};
    use crate::intern::intern;
    use crate::thread_state::{pon_err_message, test_state_lock};

    static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

    struct TempExtensionRoot {
        path: PathBuf,
    }

    impl TempExtensionRoot {
        fn new() -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!("pon-capi-extension-{}-{id}", process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create temporary C-extension root");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempExtensionRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    struct ResetImportStateOnDrop;

    impl Drop for ResetImportStateOnDrop {
        fn drop(&mut self) {
            reset_import_state_for_tests();
        }
    }

    fn compile_extension(temp: &TempExtensionRoot, module_name: &str, source: &str) -> PathBuf {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let source_path = temp.path().join(format!("{module_name}.c"));
        let output_path = temp.path().join(format!("{module_name}.pon.so"));
        fs::write(&source_path, source).expect("write temporary C extension source");

        let include_path = manifest.join("include");
        let bootstrap_path = manifest.join("capi").join("pon_capi_bootstrap.c");
        let mut args = vec![
            OsStr::new("-fPIC").to_owned(),
            OsStr::new("-I").to_owned(),
            include_path.as_os_str().to_owned(),
        ];
        if cfg!(target_os = "macos") {
            args.push(OsStr::new("-dynamiclib").to_owned());
            args.push(OsStr::new("-undefined").to_owned());
            args.push(OsStr::new("dynamic_lookup").to_owned());
        } else {
            args.push(OsStr::new("-shared").to_owned());
        }
        args.push(source_path.as_os_str().to_owned());
        args.push(bootstrap_path.as_os_str().to_owned());
        args.push(OsStr::new("-o").to_owned());
        args.push(output_path.as_os_str().to_owned());

        match run_compiler("cc", &args).or_else(|cc_error| {
            run_compiler("clang", &args).map_err(|clang_error| format!("{cc_error}\n\nclang fallback:\n{clang_error}"))
        }) {
            Ok(()) => output_path,
            Err(message) => panic!("{message}"),
        }
    }

    fn run_compiler(compiler: &str, args: &[std::ffi::OsString]) -> Result<(), String> {
        let output = Command::new(compiler)
            .args(args)
            .output()
            .map_err(|error| format!("failed to run {compiler}: {error}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format_compiler_failure(compiler, &output))
        }
    }

    fn format_compiler_failure(compiler: &str, output: &Output) -> String {
        format!(
            "{compiler} failed with status {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    #[test]
    fn capi_loads_recompiled_extension_and_calls_exported_methods() {
        let _guard = test_state_lock();
        let _reset = ResetImportStateOnDrop;
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
        }

        let temp = TempExtensionRoot::new();
        let module_path = compile_extension(
            &temp,
            "capi_test_ext",
            r#"
#include <Python.h>

static PyObject *answer(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    return PyLong_FromLong(42);
}

static PyObject *none_roundtrip(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    Py_INCREF(Py_None);
    Py_DECREF(Py_None);
    Py_RETURN_NONE;
}

static PyObject *echo(PyObject *self, PyObject *arg) {
    (void)self;
    Py_INCREF(arg);
    Py_DECREF(arg);
    Py_INCREF(arg);
    return arg;
}

static PyMethodDef methods[] = {
    {"answer", answer, METH_NOARGS, "return the answer"},
    {"none_roundtrip", none_roundtrip, METH_NOARGS, "exercise Py_None refs"},
    {"echo", echo, METH_O, "echo one object"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_test_ext",
    "Pon C-API test extension",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_test_ext(void) {
    PyObject *m = PyModule_Create(&module);
    if (m == NULL) {
        return NULL;
    }
    if (PyModule_AddObject(m, "meaning", PyLong_FromLong(7)) < 0) {
        return NULL;
    }
    return m;
}
"#,
        );

        let module = load_extension_module("capi_test_ext", &module_path)
            .unwrap_or_else(|message| panic!("failed to load C extension: {message}"));
        assert!(!module.is_null(), "extension loader returned NULL module");

        let module_name = intern("capi_test_ext");
        let answer = module_attr(module_name, intern("answer")).expect("answer method registered");
        let result = unsafe { pon_call(answer, ptr::null_mut(), 0) };
        assert!(
            !result.is_null(),
            "answer() returned NULL: {:?}",
            pon_err_message()
        );
        assert_eq!(format_object_for_print(result).as_deref(), Ok("42"));

        let meaning = module_attr(module_name, intern("meaning")).expect("module constant registered");
        assert_eq!(format_object_for_print(meaning).as_deref(), Ok("7"));

        let none_roundtrip = module_attr(module_name, intern("none_roundtrip")).expect("none_roundtrip method registered");
        let none_result = unsafe { pon_call(none_roundtrip, ptr::null_mut(), 0) };
        assert_eq!(none_result, unsafe { pon_none() });

        let echo = module_attr(module_name, intern("echo")).expect("echo method registered");
        let argument = unsafe { pon_const_int(99) };
        let mut argv = [argument];
        let echoed = unsafe { pon_call(echo, argv.as_mut_ptr(), argv.len()) };
        assert!(
            !echoed.is_null(),
            "echo(99) returned NULL: {:?}",
            pon_err_message()
        );
        assert_eq!(format_object_for_print(echoed).as_deref(), Ok("99"));
    }
}
