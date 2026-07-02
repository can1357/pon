//! Dynamic code execution builtins and runtime/JIT callback seam.
//!
//! `pon-runtime` deliberately does not depend on `pon-ir` or `pon-jit`.  The
//! embedding frontend installs small function-pointer hooks that validate and
//! execute source through the normal lowering/JIT pipeline.  This module owns the
//! Python-visible code object shell plus namespace defaulting for
//! `compile`/`eval`/`exec`, `globals`, `locals`, and `__import__`.

use std::collections::HashMap;
use std::mem;
use std::ptr;
use std::sync::{LazyLock, Mutex};

use num_traits::ToPrimitive;

use crate::abi::{self, map, pon_const_str, pon_none, return_null_with_error};
use crate::intern::{intern, resolve};
use crate::object::{PyObject, PyObjectHeader, PyType, PyUnicode};
use crate::types::{dict, int};

/// Dynamic code compilation mode accepted by Python's `compile` builtin.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DynCodeMode {
    /// Expression mode used by `eval`.
    Eval = 0,
    /// Module/statement mode used by `exec`.
    Exec = 1,
    /// Interactive single-input mode.  Pon currently executes it like `exec`.
    Single = 2,
}

impl DynCodeMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eval => "eval",
            Self::Exec => "exec",
            Self::Single => "single",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "eval" => Some(Self::Eval),
            "exec" => Some(Self::Exec),
            "single" => Some(Self::Single),
            _ => None,
        }
    }
}

/// Host-side compile validation request.
pub struct DynCompileRequest<'a> {
    pub source: &'a str,
    pub filename: &'a str,
    pub mode: DynCodeMode,
}

/// Host-side execution request.
pub struct DynExecuteRequest<'a> {
    pub source: &'a str,
    pub filename: &'a str,
    pub mode: DynCodeMode,
    pub globals: *mut PyObject,
    pub locals: *mut PyObject,
}

/// Validate dynamic source without running it.
pub type DynCompileHook = for<'a> fn(DynCompileRequest<'a>) -> Result<(), String>;
/// Compile and execute dynamic source.
pub type DynExecuteHook = for<'a> fn(DynExecuteRequest<'a>) -> Result<*mut PyObject, String>;

#[derive(Default)]
struct DynHooks {
    compile: Option<DynCompileHook>,
    execute: Option<DynExecuteHook>,
}

static DYN_HOOKS: LazyLock<Mutex<DynHooks>> = LazyLock::new(|| Mutex::new(DynHooks::default()));

/// Install the host callbacks used by `compile`, `eval`, and `exec`.
pub fn set_dynamic_code_hooks(compile: DynCompileHook, execute: DynExecuteHook) {
    let mut hooks = DYN_HOOKS.lock().unwrap_or_else(|poison| poison.into_inner());
    hooks.compile = Some(compile);
    hooks.execute = Some(execute);
}

#[repr(C)]
#[derive(Debug)]
pub struct PyCodeObject {
    /// Common object header; must remain first.
    pub ob_base: PyObjectHeader,
    source: String,
    filename: String,
    mode: DynCodeMode,
}

unsafe impl Send for PyCodeObject {}

fn code_type() -> *mut PyType {
    static CODE_TYPE: LazyLock<usize> = LazyLock::new(|| {
        let ty = PyType::new(ptr::null(), "code", mem::size_of::<PyCodeObject>());
        Box::into_raw(Box::new(ty)) as usize
    });
    *CODE_TYPE as *mut PyType
}

fn alloc_code_object(source: String, filename: String, mode: DynCodeMode) -> *mut PyObject {
    Box::into_raw(Box::new(PyCodeObject {
        ob_base: PyObjectHeader::new(code_type()),
        source,
        filename,
        mode,
    }))
    .cast::<PyObject>()
}

unsafe fn as_code_object<'a>(object: *mut PyObject) -> Option<&'a PyCodeObject> {
    if object.is_null() || unsafe { !int::type_name_is(object, "code") } {
        return None;
    }
    Some(unsafe { &*object.cast::<PyCodeObject>() })
}

#[derive(Clone, Copy)]
struct GlobalsBinding {
    module_name: u32,
}

#[derive(Default)]
struct GlobalsRegistry {
    by_module: HashMap<u32, usize>,
    by_dict: HashMap<usize, GlobalsBinding>,
}

static GLOBALS_REGISTRY: LazyLock<Mutex<GlobalsRegistry>> = LazyLock::new(|| Mutex::new(GlobalsRegistry::default()));

/// GC roots for module globals dictionaries returned by `globals()`.
pub(crate) fn rooted_globals_dicts() -> Vec<*mut PyObject> {
    let registry = GLOBALS_REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
    registry
        .by_dict
        .keys()
        .copied()
        .map(|addr| addr as *mut PyObject)
        .collect()
}

fn argv_slice<'a>(argv: *mut *mut PyObject, argc: usize, name: &str) -> Result<&'a [*mut PyObject], String> {
    if argv.is_null() && argc != 0 {
        return Err(format!("{name}() received a NULL argv pointer"));
    }
    Ok(if argc == 0 {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(argv.cast_const(), argc) }
    })
}

unsafe fn str_text(object: *mut PyObject) -> Option<String> {
    if unsafe { !int::type_name_is(object, "str") } {
        return None;
    }
    let unicode = unsafe { &*object.cast::<PyUnicode>() };
    if unicode.data.is_null() && unicode.len != 0 {
        return None;
    }
    let bytes = unsafe { core::slice::from_raw_parts(unicode.data, unicode.len) };
    core::str::from_utf8(bytes).ok().map(ToOwned::to_owned)
}

unsafe fn is_none(object: *mut PyObject) -> bool {
    unsafe { int::type_name_is(object, "NoneType") }
}

fn const_str_object(value: &str) -> Result<*mut PyObject, String> {
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    if object.is_null() {
        Err(format!("failed to allocate string '{value}'"))
    } else {
        Ok(object)
    }
}

fn empty_dict() -> Result<*mut PyObject, String> {
    let dict = unsafe { map::pon_build_map(ptr::null_mut(), 0) };
    if dict.is_null() {
        Err("failed to allocate dict".to_owned())
    } else {
        Ok(dict)
    }
}

unsafe fn require_dict(object: *mut PyObject, name: &str) -> Result<*mut PyObject, String> {
    if unsafe { dict::is_dict(object) } {
        Ok(object)
    } else {
        Err(format!("{name} must be a dict"))
    }
}

fn module_name_for_globals() -> u32 {
    crate::import::active_module_name_id().unwrap_or_else(|| intern("__main__"))
}

fn sync_module_attrs_into_dict(module_name: u32, dict_object: *mut PyObject) -> Result<(), String> {
    let Some(attrs) = crate::import::module_attrs_snapshot(module_name) else {
        return Ok(());
    };
    for (name, value) in attrs {
        let Some(name_text) = resolve(name) else {
            continue;
        };
        let key = const_str_object(&name_text)?;
        unsafe { dict::dict_insert(dict_object, key, value)? };
    }
    Ok(())
}

fn module_globals_dict() -> Result<*mut PyObject, String> {
    module_namespace_dict(module_name_for_globals())
}

/// Live namespace dict for one module, registered so mutations through it
/// sync back into the module's attrs (`globals()` for the active module,
/// `some_module.__dict__` for any module).
pub(crate) fn module_namespace_dict(module_name: u32) -> Result<*mut PyObject, String> {
    if let Some(dict_addr) = GLOBALS_REGISTRY
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .by_module
        .get(&module_name)
        .copied()
    {
        let dict_object = dict_addr as *mut PyObject;
        sync_module_attrs_into_dict(module_name, dict_object)?;
        return Ok(dict_object);
    }

    let dict_object = empty_dict()?;
    sync_module_attrs_into_dict(module_name, dict_object)?;
    let mut registry = GLOBALS_REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
    registry.by_module.insert(module_name, dict_object as usize);
    registry
        .by_dict
        .insert(dict_object as usize, GlobalsBinding { module_name });
    Ok(dict_object)
}

/// Value bound in `module_name`'s registered namespace dict, or `None` when
/// no dict was ever materialized or it lacks `name`. Read-only peek that
/// never creates the dict: module attr lookups fall back here so dict-only
/// bindings (e.g. `vars(mod)["k"] = v`) resolve like CPython, where the
/// module `__dict__` IS the attribute namespace.
pub(crate) fn peek_module_namespace_value(module_name: u32, name: &str) -> Option<*mut PyObject> {
    let dict_addr = GLOBALS_REGISTRY
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .by_module
        .get(&module_name)
        .copied()?;
    let key = const_str_object(name).ok()?;
    unsafe { dict::dict_get(dict_addr as *mut PyObject, key) }.ok().flatten()
}

fn binding_for_dict(dict_object: *mut PyObject) -> Option<GlobalsBinding> {
    GLOBALS_REGISTRY
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .by_dict
        .get(&(dict_object as usize))
        .copied()
}

fn key_name_id(key: *mut PyObject) -> Option<u32> {
    let text = unsafe { str_text(key) }?;
    Some(intern(&text))
}

/// Mirror a compiled global store into `module_name`'s previously-returned
/// globals dict (defining-module scoping: the store may target a module other
/// than the active one when a cross-module function body rebinds a global).
pub(crate) fn sync_global_store_for_module(module_name: u32, name: u32, value: *mut PyObject) {
    if value.is_null() {
        return;
    }
    let dict_addr = {
        let registry = GLOBALS_REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
        registry.by_module.get(&module_name).copied()
    };
    let Some(dict_addr) = dict_addr else {
        return;
    };
    let Some(name_text) = resolve(name) else {
        return;
    };
    if let Ok(key) = const_str_object(&name_text) {
        let _ = unsafe { dict::dict_insert(dict_addr as *mut PyObject, key, value) };
    }
}

/// Mirror a compiled global deletion into `module_name`'s previously-returned
/// globals dict (defining-module scoping, matching `sync_global_store_for_module`).
pub(crate) fn sync_global_delete_for_module(module_name: u32, name: u32) {
    let dict_addr = {
        let registry = GLOBALS_REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
        registry.by_module.get(&module_name).copied()
    };
    let Some(dict_addr) = dict_addr else {
        return;
    };
    let Some(name_text) = resolve(name) else {
        return;
    };
    if let Ok(key) = const_str_object(&name_text) {
        let _ = unsafe { dict::dict_remove(dict_addr as *mut PyObject, key) };
    }
}

/// Called by dict item-assignment helpers after a successful write.
pub(crate) fn sync_globals_dict_set(dict_object: *mut PyObject, key: *mut PyObject, value: *mut PyObject) {
    if dict_object.is_null() || value.is_null() {
        return;
    }
    let Some(binding) = binding_for_dict(dict_object) else {
        return;
    };
    if crate::import::active_module_name_id() != Some(binding.module_name) {
        return;
    }
    let Some(name) = key_name_id(key) else {
        return;
    };
    crate::import::store_active_module_attr(name, value);
}

/// Called by dict item-deletion helpers after a successful delete.
pub(crate) fn sync_globals_dict_delete(dict_object: *mut PyObject, key: *mut PyObject) {
    if dict_object.is_null() {
        return;
    }
    let Some(binding) = binding_for_dict(dict_object) else {
        return;
    };
    if crate::import::active_module_name_id() != Some(binding.module_name) {
        return;
    }
    let Some(name) = key_name_id(key) else {
        return;
    };
    crate::import::delete_active_module_attr(name);
}

/// Called by `dict.update` after a successful bulk write.
///
/// `globals().update({...})` mutates the registered globals dict without
/// going through the item-assignment helpers, so the new bindings must be
/// copied back into the active module's attrs for compiled name lookups to
/// see them (re._constants injects its opcode constants this way).
pub(crate) fn sync_globals_dict_bulk(dict_object: *mut PyObject) {
    if dict_object.is_null() {
        return;
    }
    let Some(binding) = binding_for_dict(dict_object) else {
        return;
    };
    if crate::import::active_module_name_id() != Some(binding.module_name) {
        return;
    }
    let _ = copy_dict_to_active_module(dict_object);
}

fn compile_source(source: String, filename: String, mode: DynCodeMode) -> Result<*mut PyObject, String> {
    let hook = DYN_HOOKS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .compile;
    let Some(hook) = hook else {
        return Err("dynamic code compilation is not available in this runtime".to_owned());
    };
    hook(DynCompileRequest {
        source: &source,
        filename: &filename,
        mode,
    })
    .map_err(|message| format!("SyntaxError in {filename}: {message}"))?;
    Ok(alloc_code_object(source, filename, mode))
}

fn execute_code(code: &PyCodeObject, globals: *mut PyObject, locals: *mut PyObject) -> Result<*mut PyObject, String> {
    let hook = DYN_HOOKS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .execute;
    let Some(hook) = hook else {
        return Err("dynamic code execution is not available in this runtime".to_owned());
    };
    if unsafe { dict::is_dict(globals) } {
        copy_dict_to_active_module(globals)?;
    }
    if locals != globals && unsafe { dict::is_dict(locals) } {
        copy_dict_to_active_module(locals)?;
    }
    // The dynamic body executes as module-toplevel code of the module whose
    // namespace backs `globals` (its stores/loads must land in that module's
    // attrs, and the result probe reads them back from there).  Re-enter that
    // module's execution context so the defining-module call floor masks the
    // suspended caller frames, exactly as a real module import would.
    let context_module = resolve(module_name_for_globals())
        .filter(|name| crate::import::begin_module_execution(name).is_ok());
    let result = hook(DynExecuteRequest {
        source: &code.source,
        filename: &code.filename,
        mode: code.mode,
        globals,
        locals,
    });
    if let Some(name) = context_module {
        crate::import::end_module_execution(&name);
    }
    let result = result?;
    let module_name = module_name_for_globals();
    if unsafe { dict::is_dict(globals) } {
        sync_module_attrs_into_dict(module_name, globals)?;
    }
    if locals != globals && unsafe { dict::is_dict(locals) } {
        sync_module_attrs_into_dict(module_name, locals)?;
    }
    if result.is_null() {
        Err("dynamic code execution returned NULL".to_owned())
    } else {
        Ok(result)
    }
}

fn copy_dict_to_active_module(dict_object: *mut PyObject) -> Result<(), String> {
    let entries = unsafe { dict::dict_entries_snapshot(dict_object)? };
    for entry in entries {
        let Some(name) = key_name_id(entry.key) else {
            continue;
        };
        crate::import::store_active_module_attr(name, entry.value);
    }
    Ok(())
}

fn namespace_args(args: &[*mut PyObject], name: &str) -> Result<(*mut PyObject, *mut PyObject), String> {
    if args.len() > 3 {
        return Err(format!("{name}() expected at most 3 arguments, got {}", args.len()));
    }
    let globals = if let Some(&globals) = args.get(1) {
        if unsafe { is_none(globals) } {
            module_globals_dict()?
        } else {
            unsafe { require_dict(globals, "globals")? }
        }
    } else {
        module_globals_dict()?
    };
    let locals = if let Some(&locals) = args.get(2) {
        if unsafe { is_none(locals) } {
            globals
        } else {
            unsafe { require_dict(locals, "locals")? }
        }
    } else {
        globals
    };
    Ok((globals, locals))
}

/// CPython `PyCF_ONLY_AST` (`ast.PyCF_ONLY_AST` / `_ast` re-export): compile
/// to an AST object instead of a code object.  pon's `compile` builds code
/// objects only; `ast.parse` routes here and gets the typed refusal below.
const PYCF_ONLY_AST: i64 = 0x400;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn builtin_compile(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match argv_slice(argv, argc, "compile") {
        Ok(args) => args,
        Err(message) => return return_null_with_error(message),
    };
    // `compile(source, filename, mode, flags=0, dont_inherit=False,
    // optimize=-1, *, _feature_version=-1)`: the keyword binder flattens the
    // full signature to seven slots (absent = NULL); positional callers pass
    // three to six.  `dont_inherit`/`optimize`/`_feature_version` select
    // CPython pipeline variants pon does not model and are accepted unread
    // (`ast.parse` passes their defaults).
    if args.len() < 3 || args.len() > 7 {
        return return_null_with_error(format!("compile() expected 3 to 6 arguments, got {}", args.len()));
    }
    let Some(source) = (unsafe { str_text(args[0]) }) else {
        return return_null_with_error("compile() arg 1 must be a string");
    };
    let Some(filename) = (unsafe { str_text(args[1]) }) else {
        return return_null_with_error("compile() arg 2 must be a string");
    };
    let Some(mode_text) = (unsafe { str_text(args[2]) }) else {
        return return_null_with_error("compile() arg 3 must be a string");
    };
    let flags = match optional_int_arg(args, 3, "flags") {
        Ok(flags) => flags,
        Err(message) => return return_null_with_error(message),
    };
    if flags & PYCF_ONLY_AST != 0 {
        const MESSAGE: &str =
            "pon does not support compile() with ast.PyCF_ONLY_AST (ast.parse); only code-object compilation is available";
        return abi::exc::raise_kind_error_text(crate::types::exc::ExceptionKind::NotImplementedError, MESSAGE);
    }
    let Some(mode) = DynCodeMode::from_str(&mode_text) else {
        return return_null_with_error("compile() mode must be 'exec', 'eval', or 'single'");
    };
    match compile_source(source, filename, mode) {
        Ok(code) => code,
        Err(message) => return_null_with_error(message),
    }
}

/// Reads an optional int slot from a flattened native argv: absent (short
/// argv), NULL (keyword-binder fill), and None all mean "default 0".
fn optional_int_arg(args: &[*mut PyObject], index: usize, name: &str) -> Result<i64, String> {
    let Some(&object) = args.get(index) else {
        return Ok(0);
    };
    if object.is_null() {
        return Ok(0);
    }
    if let Some(value) = int_of(object) {
        return Ok(value);
    }
    if unsafe { is_none(object) } {
        return Ok(0);
    }
    Err(format!("compile() {name} must be an int"))
}

/// Tagged-immediate-aware i64 extraction (the `_collections` idiom).
fn int_of(object: *mut PyObject) -> Option<i64> {
    if crate::tag::is_small_int(object) {
        return Some(crate::tag::untag_small_int(object));
    }
    if object.is_null() {
        return None;
    }
    // SAFETY: Heap pointer with a live header; layout proved by the name check.
    (unsafe { crate::types::dict::type_name(object) } == Some("int"))
        .then(|| unsafe { (*object.cast::<crate::object::PyLong>()).value })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn builtin_eval(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match argv_slice(argv, argc, "eval") {
        Ok(args) => args,
        Err(message) => return return_null_with_error(message),
    };
    if args.is_empty() {
        return return_null_with_error("eval() expected at least 1 argument, got 0");
    }
    let (globals, locals) = match namespace_args(args, "eval") {
        Ok(namespaces) => namespaces,
        Err(message) => return return_null_with_error(message),
    };
    let code_object = if let Some(code) = unsafe { as_code_object(args[0]) } {
        code
    } else {
        let Some(source) = (unsafe { str_text(args[0]) }) else {
            return return_null_with_error("eval() arg 1 must be a string or code object");
        };
        let code = match compile_source(source, "<string>".to_owned(), DynCodeMode::Eval) {
            Ok(code) => code,
            Err(message) => return return_null_with_error(message),
        };
        unsafe { &*code.cast::<PyCodeObject>() }
    };
    match execute_code(code_object, globals, locals) {
        Ok(result) => result,
        Err(message) => return_null_with_error(message),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn builtin_exec(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match argv_slice(argv, argc, "exec") {
        Ok(args) => args,
        Err(message) => return return_null_with_error(message),
    };
    if args.is_empty() {
        return return_null_with_error("exec() expected at least 1 argument, got 0");
    }
    let (globals, locals) = match namespace_args(args, "exec") {
        Ok(namespaces) => namespaces,
        Err(message) => return return_null_with_error(message),
    };
    let code_object = if let Some(code) = unsafe { as_code_object(args[0]) } {
        code
    } else {
        let Some(source) = (unsafe { str_text(args[0]) }) else {
            return return_null_with_error("exec() arg 1 must be a string or code object");
        };
        let code = match compile_source(source, "<string>".to_owned(), DynCodeMode::Exec) {
            Ok(code) => code,
            Err(message) => return return_null_with_error(message),
        };
        unsafe { &*code.cast::<PyCodeObject>() }
    };
    match execute_code(code_object, globals, locals) {
        Ok(_) => unsafe { pon_none() },
        Err(message) => return_null_with_error(message),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn builtin_globals(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match argv_slice(argv, argc, "globals") {
        Ok(args) => args,
        Err(message) => return return_null_with_error(message),
    };
    if !args.is_empty() {
        return return_null_with_error(format!("globals() expected no arguments, got {}", args.len()));
    }
    match module_globals_dict() {
        Ok(dict) => dict,
        Err(message) => return_null_with_error(message),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn builtin_locals(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match argv_slice(argv, argc, "locals") {
        Ok(args) => args,
        Err(message) => return return_null_with_error(message),
    };
    if !args.is_empty() {
        return return_null_with_error(format!("locals() expected no arguments, got {}", args.len()));
    }
    match module_globals_dict() {
        Ok(dict) => dict,
        Err(message) => return_null_with_error(message),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn builtin_dunder_import(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match argv_slice(argv, argc, "__import__") {
        Ok(args) => args,
        Err(message) => return return_null_with_error(message),
    };
    if args.is_empty() || args.len() > 5 {
        return return_null_with_error(format!(
            "__import__() expected 1 to 5 arguments, got {}",
            args.len()
        ));
    }
    let Some(name) = (unsafe { str_text(args[0]) }) else {
        return return_null_with_error("__import__() name must be str");
    };
    let level = match args.get(4) {
        // Keyword binding fills absent optionals with None (CPython default 0).
        Some(&level_object) if unsafe { !is_none(level_object) } => {
            match unsafe { int::to_bigint(level_object) }.and_then(|value| value.to_u32()) {
                Some(level) => level,
                None => return return_null_with_error("__import__() level must be int"),
            }
        }
        _ => 0,
    };
    let mut fromlist_names = Vec::new();
    if let Some(&fromlist) = args.get(3) {
        if unsafe { !is_none(fromlist) } {
            collect_fromlist_names(fromlist, &mut fromlist_names);
        }
    }
    let name_id = intern(&name);
    unsafe { crate::import::pon_import_name(name_id, fromlist_names.as_ptr(), fromlist_names.len(), level) }
}

fn collect_fromlist_names(fromlist: *mut PyObject, out: &mut Vec<u32>) {
    if fromlist.is_null() {
        return;
    }
    if unsafe { int::type_name_is(fromlist, "str") } {
        if let Some(text) = unsafe { str_text(fromlist) } {
            out.push(intern(&text));
        }
        return;
    }
    let iter = unsafe { abi::pon_get_iter(fromlist, ptr::null_mut()) };
    if iter.is_null() {
        return;
    }
    loop {
        let item = unsafe { abi::pon_iter_next(iter, ptr::null_mut()) };
        if item.is_null() {
            break;
        }
        if let Some(text) = unsafe { str_text(item) } {
            out.push(intern(&text));
        }
    }
}
