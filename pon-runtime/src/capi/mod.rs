//! CPython-source compatibility shim for recompiled native extensions.
//!
//! This is not CPython's binary ABI. Extensions include Pon's `Python.h`, link
//! the bootstrap object once, and the loader injects this process's function
//! tables before calling `PyInit_*`.
//!
//! Dispatch is grouped into per-family tables (see `include/pon_capi/*.h`);
//! the top-level [`PyPonCapi`] only aggregates family-table pointers plus a
//! `size` drift guard, so families evolve independently.

mod containers;
mod err;
mod numbers;
mod strings;
mod runtime_;
mod object_;
mod typeobj;
#[cfg(test)]
mod args_test;
pub(crate) mod twin;

pub(crate) use typeobj::is_capi_class;

use core::ffi::{c_char, c_int, c_void};
use core::mem;
use core::ptr;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::path::Path;
use std::sync::{LazyLock, Mutex, OnceLock};

use crate::abi;
use crate::intern::intern;
use crate::object::{CallFunc, PyObject, PyObjectHeader, PyType, as_object_ptr};
use crate::thread_state::{pon_err_message, pon_err_occurred, pon_err_set};

const METH_VARARGS: c_int = 0x0001;
const METH_KEYWORDS: c_int = 0x0002;
const METH_NOARGS: c_int = 0x0004;
const METH_O: c_int = 0x0008;
const METH_CLASS: c_int = 0x0010;
const METH_STATIC: c_int = 0x0020;
const METH_FASTCALL: c_int = 0x0080;

const PYTHON_API_VERSION: c_int = 1013;

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

/// Function-table hub injected into recompiled extension modules.
///
/// `size` guards layout drift at load time; the bootstrap rejects a table
/// whose size differs from the header it was compiled against. Family
/// pointers only: append new families at the end, never reorder.
#[repr(C)]
pub struct PyPonCapi {
    size: usize,
    core: *const PyPonCapiCore,
    err: *const err::PyPonCapiErr,
    numbers: *const numbers::PyPonCapiNumbers,
    strings: *const strings::PyPonCapiStrings,
    containers: *const containers::PyPonCapiContainers,
    runtime_: *const runtime_::PyPonCapiRuntime,
    object_: *const object_::PyPonCapiObject,
    typeobj: *const typeobj::PyPonCapiTypeObj,
}

unsafe impl Sync for PyPonCapi {}
unsafe impl Send for PyPonCapi {}

/// C mirror: `include/pon_capi/core.h` `PyPonCapiCore`.
#[repr(C)]
struct PyPonCapiCore {
    module_create2: unsafe extern "C" fn(*mut PyModuleDef, c_int) -> *mut PyObject,
    module_add_object: unsafe extern "C" fn(*mut PyObject, *const c_char, *mut PyObject) -> c_int,
    inc_ref: unsafe extern "C" fn(*mut PyObject),
    dec_ref: unsafe extern "C" fn(*mut PyObject),
    none: unsafe extern "C" fn() -> *mut PyObject,
    bool_true: unsafe extern "C" fn() -> *mut PyObject,
    bool_false: unsafe extern "C" fn() -> *mut PyObject,
    not_implemented: unsafe extern "C" fn() -> *mut PyObject,
    register_local_twins: unsafe extern "C" fn(*const *mut twin::ForeignTypeObject, c_int) -> c_int,
    builtin_type_id: unsafe extern "C" fn(*mut PyObject) -> c_int,
    foreign_of: unsafe extern "C" fn(*mut PyObject) -> *mut twin::ForeignTypeObject,
}

unsafe impl Sync for PyPonCapiCore {}
unsafe impl Send for PyPonCapiCore {}

#[repr(C)]
struct PyCFunctionObject {
    ob_base: PyObjectHeader,
    method: PyCFunction,
    flags: c_int,
    self_object: *mut PyObject,
    name: u32,
}

/// GC type id for C-function carriers (registry: pon-gc ids live in
/// `abi::register_gc_types` and per-module constants; 141 is next to the
/// native-file id 120 and capi-instance id 140).
const TYPE_ID_CAPI_CFUNCTION: pon_gc::TypeId = pon_gc::TypeId(141);

/// Traces the bound receiver so a carrier can never outlive it.
///
/// # Safety
///
/// `object` points to a live `PyCFunctionObject` allocation.
unsafe extern "C" fn trace_cfunction(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    // SAFETY: caller contract — live carrier allocation.
    let receiver = unsafe { (*object.cast::<PyCFunctionObject>()).self_object };
    if !receiver.is_null() && crate::tag::is_heap(receiver.cast()) {
        visitor(receiver.cast());
    }
}

static C_FUNCTION_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(ptr::null(), "builtin_function_or_method", mem::size_of::<PyCFunctionObject>());
    ty.tp_call = Some(cfunction_call as CallFunc);
    // C methods installed in a Ready'd type's namespace bind their receiver
    // through the descriptor protocol (METH_CLASS binds the type,
    // METH_STATIC stays unbound).
    ty.tp_descr_get = Some(cfunction_descr_get);
    Box::into_raw(Box::new(ty)) as usize
});

static CAPI_PINS: LazyLock<Mutex<HashMap<usize, usize>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static EXTENSION_HANDLES: LazyLock<Mutex<Vec<usize>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Owns every family table. Built on first extension load: the err family
/// fabricates `PyExc_*` twins and therefore requires an initialized runtime
/// (`OnceLock`, not `LazyLock`: runtime input).
struct Families {
    core: PyPonCapiCore,
    err: err::PyPonCapiErr,
    numbers: numbers::PyPonCapiNumbers,
    strings: strings::PyPonCapiStrings,
    containers: containers::PyPonCapiContainers,
    runtime_: runtime_::PyPonCapiRuntime,
    object_: object_::PyPonCapiObject,
    typeobj: typeobj::PyPonCapiTypeObj,
}

static FAMILIES: OnceLock<Families> = OnceLock::new();
static CAPI: OnceLock<PyPonCapi> = OnceLock::new();

/// Assembles (once) and returns the process-lifetime injected table.
fn capi_table() -> *const PyPonCapi {
    let families = FAMILIES.get_or_init(|| Families {
        core: PyPonCapiCore {
            module_create2: py_module_create2,
            module_add_object: py_module_add_object,
            inc_ref: py_inc_ref,
            dec_ref: py_dec_ref,
            none: py_none,
            bool_true: py_true,
            bool_false: py_false,
            not_implemented: py_not_implemented,
            register_local_twins: twin::capi_register_local_twins,
            builtin_type_id: twin::capi_builtin_type_id,
            foreign_of: twin::capi_foreign_of,
        },
        err: err::build(),
        numbers: numbers::build(),
        strings: strings::build(),
        containers: containers::build(),
        runtime_: runtime_::build(),
        object_: object_::build(),
        typeobj: typeobj::build(),
    });
    CAPI.get_or_init(|| PyPonCapi {
        size: mem::size_of::<PyPonCapi>(),
        core: &families.core,
        err: &families.err,
        numbers: &families.numbers,
        strings: &families.strings,
        containers: &families.containers,
        runtime_: &families.runtime_,
        object_: &families.object_,
        typeobj: &families.typeobj,
    })
}

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
    let set_result = unsafe { set_capi(capi_table()) };
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
    // Extensions publish their static types as module attributes
    // (`PyModule_AddObject(m, "Counter", (PyObject *)&CounterType)`); foreign
    // statics must never enter the runtime object graph — swap in the native
    // type they were Ready'd into.
    let value = match twin::registered_native_of_foreign(value.cast::<twin::ForeignTypeObject>()) {
        Some(native) => native.cast::<PyObject>(),
        None => value,
    };
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

unsafe extern "C" fn py_true() -> *mut PyObject {
    crate::types::bool_::from_bool(true)
}

unsafe extern "C" fn py_false() -> *mut PyObject {
    crate::types::bool_::from_bool(false)
}

unsafe extern "C" fn py_not_implemented() -> *mut PyObject {
    unsafe { abi::pon_not_implemented() }
}

/// Pins `object` as an explicit GC root (C-side owned reference); counted,
/// so pins nest. No-op for NULL, sentinel low addresses, and immediates.
pub(super) fn pin_object(object: *mut PyObject) {
    if object.is_null() || object.addr() < 4096 || !crate::tag::is_heap(object) {
        return;
    }
    let mut pins = CAPI_PINS.lock().unwrap_or_else(|poison| poison.into_inner());
    *pins.entry(object as usize).or_insert(0) += 1;
}

unsafe extern "C" fn py_inc_ref(object: *mut PyObject) {
    pin_object(object);
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


unsafe extern "C" fn cfunction_call(callee: *mut PyObject, args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
    if callee.is_null() {
        return abi::return_null_with_error("NULL C function object");
    }
    let function = unsafe { &*callee.cast::<PyCFunctionObject>() };
    let positional = match unsafe { tuple_args(args) } {
        Ok(values) => values,
        Err(message) => return abi::return_null_with_error(message),
    };
    if function.flags & METH_KEYWORDS != 0 && function.flags & METH_FASTCALL == 0 {
        // METH_VARARGS|METH_KEYWORDS: (self, args_tuple, kwargs_dict_or_NULL).
        let with_keywords: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject =
            // SAFETY: the METH_KEYWORDS flag certifies the C entry was
            // declared with the PyCFunctionWithKeywords signature.
            unsafe { mem::transmute(function.method) };
        let tuple = if args.is_null() {
            unsafe { abi::seq::pon_build_tuple(ptr::null_mut(), 0) }
        } else {
            args
        };
        return unsafe { with_keywords(function.self_object, tuple, _kwargs) };
    }
    if function.flags & METH_FASTCALL != 0 {
        if function.flags & METH_KEYWORDS != 0 {
            let fastcall_kw: unsafe extern "C" fn(*mut PyObject, *const *mut PyObject, isize, *mut PyObject) -> *mut PyObject =
                // SAFETY: METH_FASTCALL|METH_KEYWORDS certifies the
                // _PyCFunctionFastWithKeywords signature.
                unsafe { mem::transmute(function.method) };
            return unsafe { fastcall_kw(function.self_object, positional.as_ptr(), positional.len() as isize, ptr::null_mut()) };
        }
        let fastcall: unsafe extern "C" fn(*mut PyObject, *const *mut PyObject, isize) -> *mut PyObject =
            // SAFETY: METH_FASTCALL certifies the _PyCFunctionFast signature.
            unsafe { mem::transmute(function.method) };
        return unsafe { fastcall(function.self_object, positional.as_ptr(), positional.len() as isize) };
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

/// Binds a C method carrier to its receiver: instance access clones the
/// carrier with `self_object` filled; class access and METH_STATIC return the
/// carrier unbound; METH_CLASS binds the owning type.
unsafe extern "C" fn cfunction_descr_get(descriptor: *mut PyObject, instance: *mut PyObject, owner: *mut PyObject) -> *mut PyObject {
    if descriptor.is_null() {
        return abi::return_null_with_error("NULL C function descriptor");
    }
    // SAFETY: the descriptor protocol dispatches here only for live
    // PyCFunctionObject values (C_FUNCTION_TYPE's tp_descr_get).
    let function = unsafe { &*descriptor.cast::<PyCFunctionObject>() };
    if function.flags & METH_STATIC != 0 {
        return descriptor;
    }
    if function.flags & METH_CLASS != 0 {
        let receiver = if owner.is_null() { instance } else { owner };
        return alloc_cfunction_named(function.method, function.flags, receiver, function.name);
    }
    if instance.is_null() {
        return descriptor;
    }
    alloc_cfunction_named(function.method, function.flags, instance, function.name)
}

pub(super) fn alloc_cfunction(function: PyCFunction, flags: c_int, self_object: *mut PyObject, name: &str) -> *mut PyObject {
    alloc_cfunction_named(function, flags, self_object, intern(name))
}

fn alloc_cfunction_named(function: PyCFunction, flags: c_int, self_object: *mut PyObject, name: u32) -> *mut PyObject {
    let info = pon_gc::GcTypeInfo {
        size: mem::size_of::<PyCFunctionObject>(),
        trace: trace_cfunction,
        finalize: None,
    };
    let Ok(block) = abi::alloc_gc_object(TYPE_ID_CAPI_CFUNCTION, info) else {
        return abi::return_null_with_error("runtime is not initialized");
    };
    let object = block.cast::<PyCFunctionObject>();
    // SAFETY: `block` is a fresh zeroed allocation of the carrier's size.
    unsafe {
        object.write(PyCFunctionObject {
            ob_base: PyObjectHeader::new(*C_FUNCTION_TYPE as *const PyType),
            method: function,
            flags,
            self_object,
            name,
        });
    }
    as_object_ptr(object)
}

pub(super) fn c_string(ptr: *const c_char) -> Option<String> {
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

    pub(super) struct TempExtensionRoot {
        path: PathBuf,
    }

    impl TempExtensionRoot {
        pub(super) fn new() -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!("pon-capi-extension-{}-{id}", process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create temporary C-extension root");
            Self { path }
        }

        pub(super) fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempExtensionRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    pub(super) struct ResetImportStateOnDrop;

    impl Drop for ResetImportStateOnDrop {
        fn drop(&mut self) {
            reset_import_state_for_tests();
        }
    }

    pub(super) fn compile_extension(temp: &TempExtensionRoot, module_name: &str, source: &str) -> PathBuf {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let source_path = temp.path().join(format!("{module_name}.c"));
        let output_path = temp.path().join(format!("{module_name}.pon.so"));
        fs::write(&source_path, source).expect("write temporary C extension source");

        let include_path = manifest.join("include");
        let bootstrap_path = manifest.join("capi").join("pon_capi_bootstrap.c");
        let args_path = manifest.join("capi").join("pon_capi_args.c");
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
        args.push(args_path.as_os_str().to_owned());
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
    #[test]
    fn capi_type_and_error_identity_holds_across_the_boundary() {
        let _guard = test_state_lock();
        let _reset = ResetImportStateOnDrop;
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
        }

        let temp = TempExtensionRoot::new();
        let module_path = compile_extension(
            &temp,
            "capi_twin_ext",
            r#"
#include <Python.h>

/* Returns a bitmask of passed checks; Rust asserts the full mask. */
static PyObject *identity_checks(PyObject *self, PyObject *args) {
    long ok = 0;
    (void)self;
    (void)args;

    PyObject *seven = PyLong_FromLong(7);
    if (Py_TYPE(seven) == &PyLong_Type) ok |= 1L << 0;
    if (Py_TYPE(seven) == Py_TYPE(seven)) ok |= 1L << 1;
    if (Py_TYPE(Py_None) == &_PyNone_Type) ok |= 1L << 2;
    if (Py_TYPE(Py_True) == &PyBool_Type) ok |= 1L << 3;
    if (PyLong_Type.tp_name != 0 && strcmp(PyLong_Type.tp_name, "int") == 0) ok |= 1L << 4;
    if (PyLong_Type.tp_basicsize > 0) ok |= 1L << 5;

    PyErr_SetString(PyExc_ValueError, "twin identity probe");
    if (PyErr_Occurred() == PyExc_ValueError) ok |= 1L << 6;
    if (((PyTypeObject *)PyExc_ValueError)->tp_flags & Py_TPFLAGS_BASE_EXC_SUBCLASS) ok |= 1L << 7;
    PyErr_Clear();
    if (PyErr_Occurred() == 0) ok |= 1L << 8;

    return PyLong_FromLong(ok);
}

static PyMethodDef twin_methods[] = {
    {"identity_checks", identity_checks, METH_NOARGS, 0},
    {0, 0, 0, 0},
};

static struct PyModuleDef twin_module = {
    PyModuleDef_HEAD_INIT,
    "capi_twin_ext",
    0,
    -1,
    twin_methods,
    0,
    0,
    0,
    0,
};

PyMODINIT_FUNC PyInit_capi_twin_ext(void) {
    return PyModule_Create(&twin_module);
}
"#,
        );

        let module = load_extension_module("capi_twin_ext", &module_path)
            .unwrap_or_else(|message| panic!("failed to load C extension: {message}"));
        assert!(!module.is_null(), "extension loader returned NULL module");

        let module_name = intern("capi_twin_ext");
        let checks = module_attr(module_name, intern("identity_checks")).expect("identity_checks registered");
        let result = unsafe { pon_call(checks, ptr::null_mut(), 0) };
        assert!(!result.is_null(), "identity_checks() returned NULL: {:?}", pon_err_message());
        // All nine identity bits must hold; a partial mask names the failure.
        assert_eq!(format_object_for_print(result).as_deref(), Ok("511"), "twin identity bitmask mismatch");
    }
}
