//! Import runtime support for WS-IMPORT.
//!
//! This module owns import state and module-object behavior. Exported helpers
//! follow the same NULL-sentinel contract as the rest of the runtime: failures
//! set the thread-state diagnostic and return NULL, never unwind into generated
//! code.

use std::collections::HashMap;
use std::env;
use std::ffi::c_int;
use std::fs;
use std::mem;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{LazyLock, Mutex};

use crate::abi::{pon_const_int, pon_const_str, pon_none, pon_store_global, return_minus_one_with_error, return_null_with_error};
use crate::intern::{intern, resolve};
use crate::object::{PyObject, PyObjectHeader, PyType, PyUnicode, as_object_ptr};
use crate::thread_state::pon_err_occurred;

/// Host callback used by the CLI/JIT integration pass to execute a source module
/// through the normal ruff -> IR -> JIT pipeline and return its module object.
pub type SourceModuleLoader = for<'a> fn(SourceModuleRequest<'a>) -> Result<*mut PyObject, String>;

/// Pure-Python source module found by the import resolver.
pub struct SourceModuleRequest<'a> {
    /// Fully-qualified import name.
    pub name: &'a str,
    /// Resolved UTF-8 source path.
    pub path: &'a Path,
    /// Source text to compile and execute.
    pub source: &'a str,
    /// Whether `path` is a package `__init__.py`.
    pub is_package: bool,
}

#[repr(C)]
pub struct PyModuleObject {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Interned module name.
    pub name: u32,
    /// Attribute table keyed by runtime interned name ids.
    pub attrs: HashMap<u32, *mut PyObject>,
}

struct ImportState {
    modules: HashMap<u32, *mut PyObject>,
    source_loader: Option<SourceModuleLoader>,
    module_type: *mut PyType,
    current_modules: Vec<u32>,
}

unsafe impl Send for ImportState {}

static IMPORT_STATE: LazyLock<Mutex<ImportState>> = LazyLock::new(|| Mutex::new(ImportState::new()));

impl ImportState {
    fn new() -> Self {
        let mut ty = Box::new(PyType::new(ptr::null(), "module", mem::size_of::<PyModuleObject>()));
        ty.tp_getattro = Some(module_getattro);
        Self {
            modules: HashMap::new(),
            source_loader: None,
            module_type: Box::into_raw(ty),
            current_modules: Vec::new(),
        }
    }
}

/// Installs the host loader that compiles pure-Python imports with the normal
/// frontend/codegen/JIT pipeline.  The callback is optional so native imports and
/// unsupported diagnostics stay usable before CLI integration is wired.
pub fn set_source_module_loader(loader: SourceModuleLoader) {
    let mut state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    state.source_loader = Some(loader);
}

/// Clears the import cache and source loader. Intended for focused tests.
pub fn reset_import_state_for_tests() {
    let mut state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    state.modules.clear();
    state.source_loader = None;
    state.current_modules.clear();
}

/// Installs the curated native modules into the import cache after core runtime
/// allocation is available.
pub fn register_native_modules() -> Result<(), String> {
    crate::native::register_modules()
}

/// Imports a module by interned dotted name.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_import_name(
    name_interned: u32,
    fromlist: *const u32,
    fromlist_len: usize,
    level: u32,
) -> *mut PyObject {
    if fromlist.is_null() && fromlist_len != 0 {
        return return_null_with_error("import fromlist pointer is NULL");
    }

    let Some(raw_name) = resolve(name_interned) else {
        return return_null_with_error(format!("import name id {name_interned} is not interned"));
    };

    let requested_fromlist = if fromlist_len == 0 {
        &[][..]
    } else {
        // SAFETY: The caller supplies `fromlist_len` contiguous interned ids.
        unsafe { core::slice::from_raw_parts(fromlist, fromlist_len) }
    };

    let importer_package = (level != 0).then(current_importer_package).flatten();
    let name = match resolve_import_name(&raw_name, level, importer_package.as_deref()) {
        Ok(name) => name,
        Err(message) => return return_null_with_error(message),
    };

    let imported = import_module_by_name(&name);
    let module = match imported {
        Ok(module) => module,
        Err(message) => return return_null_with_error(message),
    };

    if requested_fromlist.is_empty() {
        if let Some(root_name) = name.split('.').next() {
            let root_id = intern(root_name);
            let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
            if let Some(root) = state.modules.get(&root_id).copied() {
                return root;
            }
        }
    }

    module
}

/// Loads one named attribute from an imported module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_import_from(module: *mut PyObject, name_interned: u32) -> *mut PyObject {
    if module.is_null() {
        return return_null_with_error("cannot import from NULL module");
    }
    let Some(module_ptr) = as_module(module) else {
        return return_null_with_error("import-from receiver is not a module");
    };
    // SAFETY: `as_module` proved the layout.
    let module_ref = unsafe { &mut *module_ptr };
    if let Some(value) = module_ref.attrs.get(&name_interned).copied() {
        return value;
    }

    let module_name = resolve(module_ref.name).unwrap_or_else(|| format!("<module:{}>", module_ref.name));
    let attr = resolve(name_interned).unwrap_or_else(|| format!("<interned:{name_interned}>"));
    if module_is_package(module_ref) {
        let child_name = format!("{module_name}.{attr}");
        if let Ok(child) = import_module_by_name(&child_name) {
            return child;
        }
    }
    return_null_with_error(format!("cannot import name '{attr}' from '{module_name}'"))
}

/// Imports all public module attributes into the active globals dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_import_star(module: *mut PyObject) -> *mut PyObject {
    if module.is_null() {
        return return_null_with_error("cannot import * from NULL module");
    }
    let Some(module) = as_module(module) else {
        return return_null_with_error("import-star receiver is not a module");
    };
    // SAFETY: `as_module` proved the layout.
    let module = unsafe { &*module };
    for (name, value) in &module.attrs {
        if is_public_name(*name) {
            // SAFETY: Store helper enforces the NULL-sentinel error contract.
            let stored = unsafe { pon_store_global(*name, *value) };
            if stored.is_null() {
                return ptr::null_mut();
            }
        }
    }
    // SAFETY: `pon_none` returns the initialized singleton or NULL with an error.
    unsafe { pon_none() }
}

/// Returns a cached module by interned name for tests and hub integration.
pub fn cached_module(name: u32) -> Option<*mut PyObject> {
    let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    state.modules.get(&name).copied()
}

/// Creates or replaces a module object in `sys.modules` with the supplied attrs.
pub fn install_module(name: &str, attrs: impl IntoIterator<Item = (u32, *mut PyObject)>) -> Result<*mut PyObject, String> {
    create_module(name, false, attrs)
}

fn import_module_by_name(name: &str) -> Result<*mut PyObject, String> {
    if name.is_empty() {
        return Err("no module named ''".to_owned());
    }

    let name_id = intern(name);
    {
        let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(module) = state.modules.get(&name_id).copied() {
            return Ok(module);
        }
    }

    if is_unsupported_c_accelerated(name) {
        return Err(format!("module '{name}' is C-accelerated and unsupported"));
    }

    if let Some(parent) = parent_module_name(name) {
        import_module_by_name(parent)?;
    }

    if let Some(module) = native_module(name)? {
        let mut state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
        state.modules.insert(name_id, module);
        drop(state);
        bind_child_to_parent(name, module);
        return Ok(module);
    }

    if let Some(spec) = find_source_module(name) {
        let source = fs::read_to_string(&spec.path)
            .map_err(|error| format!("failed to read source module '{}': {error}", spec.path.display()))?;
        let loader = {
            let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
            state.source_loader
        };
        if let Some(loader) = loader {
            let module = create_module(name, spec.is_package, [])?;
            bind_child_to_parent(name, module);
            begin_module_execution(name)?;
            let loaded = loader(SourceModuleRequest {
                name,
                path: &spec.path,
                source: &source,
                is_package: spec.is_package,
            });
            end_module_execution(name);
            let loaded = loaded?;
            if loaded.is_null() {
                if pon_err_occurred() {
                    return Err(format!("source module '{name}' returned NULL"));
                }
                return Err(format!("source module '{name}' returned NULL without setting an exception"));
            }
            return Ok(module);
        }

        let module = load_curated_assignment_module(name, &source, spec.is_package)?;
        bind_child_to_parent(name, module);
        return Ok(module);
    }

    Err(format!("no module named '{name}'"))
}

fn native_module(name: &str) -> Result<Option<*mut PyObject>, String> {
    crate::native::make_module(name)
}

fn is_unsupported_c_accelerated(name: &str) -> bool {
    matches!(
        name,
        "_json"
            | "_pickle"
            | "_socket"
            | "_ssl"
            | "_sqlite3"
            | "_hashlib"
            | "_bz2"
            | "_lzma"
            | "_ctypes"
            | "math"
    )
}

struct SourceSpec {
    path: PathBuf,
    is_package: bool,
}

fn find_source_module(name: &str) -> Option<SourceSpec> {
    if name.is_empty() {
        return None;
    }
    let mut relative = PathBuf::new();
    for part in name.split('.') {
        relative.push(part);
    }
    search_roots().into_iter().find_map(|root| {
        let package_init = root.join(&relative).join("__init__.py");
        if package_init.is_file() {
            return Some(SourceSpec {
                path: package_init,
                is_package: true,
            });
        }
        let mut module_path = root.join(&relative);
        module_path.set_extension("py");
        module_path.is_file().then_some(SourceSpec {
            path: module_path,
            is_package: false,
        })
    })
}

fn search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(cwd) = env::current_dir() {
        roots.push(cwd.clone());
        roots.push(cwd.join("pon-conformance").join("corpus"));
        roots.push(cwd.join("pon-conformance").join("vendor").join("cpython-3.14").join("Lib"));
    }
    if let Ok(extra) = env::var("PONPATH") {
        roots.extend(env::split_paths(&extra));
    }
    roots
}

fn load_curated_assignment_module(name: &str, source: &str, is_package: bool) -> Result<*mut PyObject, String> {
    let mut attrs = Vec::new();
    for line in source.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((lhs, rhs)) = line.split_once('=') else {
            return Err(format!("pure-Python module '{name}' requires the host JIT loader for unsupported statement: {line}"));
        };
        let lhs = lhs.trim();
        if !is_identifier(lhs) {
            return Err(format!("unsupported assignment target '{lhs}' in pure-Python module '{name}'"));
        }
        let value = parse_curated_literal(rhs.trim())?;
        attrs.push((intern(lhs), value));
    }
    create_module(name, is_package, attrs)
}

fn create_module(
    name: &str,
    is_package: bool,
    attrs: impl IntoIterator<Item = (u32, *mut PyObject)>,
) -> Result<*mut PyObject, String> {
    let name_id = intern(name);
    let mut attr_map = HashMap::new();
    for (key, value) in attrs {
        if value.is_null() {
            let attr = resolve(key).unwrap_or_else(|| format!("<interned:{key}>"));
            return Err(format!("module attribute '{attr}' for '{name}' is NULL"));
        }
        attr_map.insert(key, value);
    }

    let name_object = runtime_string(name)?;
    let package = module_package_name(name, is_package);
    let package_object = runtime_string(&package)?;
    attr_map.insert(intern("__name__"), name_object);
    attr_map.insert(intern("__package__"), package_object);

    let mut state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    let object = Box::new(PyModuleObject {
        ob_base: PyObjectHeader::new(state.module_type),
        name: name_id,
        attrs: attr_map,
    });
    let object = as_object_ptr(Box::into_raw(object));
    state.modules.insert(name_id, object);
    Ok(object)
}

fn runtime_string(value: &str) -> Result<*mut PyObject, String> {
    // SAFETY: `pon_const_str` returns NULL with a thread-state error on failure.
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    (!object.is_null()).then_some(object).ok_or_else(|| format!("failed to allocate string literal '{value}'"))
}

fn parent_module_name(name: &str) -> Option<&str> {
    name.rsplit_once('.').map(|(parent, _)| parent)
}

fn module_package_name(name: &str, is_package: bool) -> String {
    if name == "__main__" {
        String::new()
    } else if is_package {
        name.to_owned()
    } else {
        parent_module_name(name).unwrap_or_default().to_owned()
    }
}

fn module_from_object_locked(state: &ImportState, object: *mut PyObject) -> Option<*mut PyModuleObject> {
    if object.is_null() {
        return None;
    }
    // SAFETY: Non-NULL boxed values begin with `PyObjectHeader`.
    let is_module = unsafe { (*object).ob_type == state.module_type };
    is_module.then_some(object.cast::<PyModuleObject>())
}

fn bind_child_to_parent(name: &str, module: *mut PyObject) {
    let Some((parent_name, child_name)) = name.rsplit_once('.') else {
        return;
    };
    let parent_id = intern(parent_name);
    let child_id = intern(child_name);
    let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    let Some(parent) = state.modules.get(&parent_id).copied() else {
        return;
    };
    let Some(parent) = module_from_object_locked(&state, parent) else {
        return;
    };
    // SAFETY: The import state proved the object uses `PyModuleObject` layout.
    unsafe {
        (&mut *parent).attrs.insert(child_id, module);
    }
}

pub fn begin_module_execution(name: &str) -> Result<(), String> {
    let name_id = intern(name);
    let mut state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    if !state.modules.contains_key(&name_id) {
        return Err(format!("cannot execute uncached module '{name}'"));
    }
    state.current_modules.push(name_id);
    Ok(())
}

pub fn end_module_execution(name: &str) {
    let name_id = intern(name);
    let mut state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    if state.current_modules.last().copied() == Some(name_id) {
        state.current_modules.pop();
    }
}

fn current_importer_package() -> Option<String> {
    let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    let current = state.current_modules.last().copied()?;
    let module = state.modules.get(&current).copied()?;
    let module = module_from_object_locked(&state, module)?;
    // SAFETY: The import state proved the object uses `PyModuleObject` layout.
    let package = unsafe { (&*module).attrs.get(&intern("__package__")).copied()? };
    unicode_text(package).map(str::to_owned)
}

pub fn active_module_attr(name: u32) -> Option<*mut PyObject> {
    let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    let current = state.current_modules.last().copied()?;
    let module = state.modules.get(&current).copied()?;
    let module = module_from_object_locked(&state, module)?;
    // SAFETY: The import state proved the object uses `PyModuleObject` layout.
    unsafe { (&*module).attrs.get(&name).copied() }
}

pub fn store_active_module_attr(name: u32, value: *mut PyObject) -> bool {
    let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    let Some(current) = state.current_modules.last().copied() else {
        return false;
    };
    let Some(module) = state.modules.get(&current).copied() else {
        return false;
    };
    let Some(module) = module_from_object_locked(&state, module) else {
        return false;
    };
    // SAFETY: The import state proved the object uses `PyModuleObject` layout.
    unsafe {
        (&mut *module).attrs.insert(name, value);
    }
    true
}

fn unicode_text(object: *mut PyObject) -> Option<&'static str> {
    if object.is_null() {
        return None;
    }
    // SAFETY: Module identity attrs are allocated as `PyUnicode` and live for the process.
    unsafe { (&*object.cast::<PyUnicode>()).as_str() }
}

fn module_is_package(module: &PyModuleObject) -> bool {
    let name = resolve(module.name);
    let package = module.attrs.get(&intern("__package__")).copied().and_then(unicode_text);
    matches!((name.as_deref(), package), (Some(name), Some(package)) if name == package)
}

fn resolve_import_name(name: &str, level: u32, importer_package: Option<&str>) -> Result<String, String> {
    if level == 0 {
        return Ok(name.to_owned());
    }
    let Some(package) = importer_package.filter(|package| !package.is_empty()) else {
        return Err("attempted relative import with no known parent package".to_owned());
    };
    let mut parts = package.split('.').collect::<Vec<_>>();
    let strip = level.saturating_sub(1) as usize;
    if strip >= parts.len() {
        return Err("attempted relative import beyond top-level package".to_owned());
    }
    parts.truncate(parts.len() - strip);
    if !name.is_empty() {
        parts.extend(name.split('.'));
    }
    Ok(parts.join("."))
}

#[cfg(test)]
mod tests {
    use super::resolve_import_name;

    #[test]
    fn absolute_import_keeps_name() {
        assert_eq!(resolve_import_name("pkg.sub", 0, None).unwrap(), "pkg.sub");
    }

    #[test]
    fn level_one_resolves_from_current_package() {
        assert_eq!(resolve_import_name("sib", 1, Some("pkg.sub")).unwrap(), "pkg.sub.sib");
    }

    #[test]
    fn level_two_strips_one_component() {
        assert_eq!(resolve_import_name("sib", 2, Some("pkg.sub")).unwrap(), "pkg.sib");
    }

    #[test]
    fn empty_relative_name_resolves_to_package() {
        assert_eq!(resolve_import_name("", 1, Some("pkg.sub")).unwrap(), "pkg.sub");
    }

    #[test]
    fn relative_import_without_package_matches_cpython_text() {
        assert_eq!(
            resolve_import_name("sib", 1, Some("")).unwrap_err(),
            "attempted relative import with no known parent package"
        );
        assert_eq!(
            resolve_import_name("sib", 1, None).unwrap_err(),
            "attempted relative import with no known parent package"
        );
    }

    #[test]
    fn relative_import_beyond_top_level_matches_cpython_text() {
        assert_eq!(
            resolve_import_name("sib", 2, Some("pkg")).unwrap_err(),
            "attempted relative import beyond top-level package"
        );
    }
}

fn parse_curated_literal(text: &str) -> Result<*mut PyObject, String> {
    if let Some(value) = parse_quoted_literal(text) {
        // SAFETY: `pon_const_str` returns NULL with a thread-state error on failure.
        let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
        return (!object.is_null()).then_some(object).ok_or_else(|| "failed to allocate string literal".to_owned());
    }
    if let Ok(value) = text.parse::<i64>() {
        // SAFETY: `pon_const_int` returns NULL with a thread-state error on failure.
        let object = unsafe { pon_const_int(value) };
        return (!object.is_null()).then_some(object).ok_or_else(|| "failed to allocate integer literal".to_owned());
    }
    if text == "None" {
        // SAFETY: `pon_none` returns NULL with a thread-state error on failure.
        let object = unsafe { pon_none() };
        return (!object.is_null()).then_some(object).ok_or_else(|| "failed to load None".to_owned());
    }
    Err(format!("unsupported curated module literal '{text}'"))
}

fn parse_quoted_literal(text: &str) -> Option<&str> {
    let quote = text.as_bytes().first().copied()?;
    if quote != b'\'' && quote != b'\"' {
        return None;
    }
    (text.as_bytes().last().copied() == Some(quote) && text.len() >= 2).then_some(&text[1..text.len() - 1])
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic()) && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn is_public_name(name: u32) -> bool {
    resolve(name).is_some_and(|name| !name.starts_with('_'))
}

fn as_module(object: *mut PyObject) -> Option<*mut PyModuleObject> {
    if object.is_null() {
        return None;
    }
    let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    // SAFETY: Non-NULL boxed values begin with `PyObjectHeader`.
    let is_module = unsafe { (*object).ob_type == state.module_type };
    is_module.then_some(object.cast::<PyModuleObject>())
}

unsafe extern "C" fn module_getattro(module: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    if module.is_null() || name.is_null() {
        return return_null_with_error("module attribute lookup received NULL");
    }
    let Some(module) = as_module(module) else {
        return return_null_with_error("attribute receiver is not a module");
    };
    // SAFETY: Attribute names are allocated by `abstract_op` as `PyUnicode`.
    let name_text = unsafe { (&*name.cast::<PyUnicode>()).as_str() };
    let Some(name_text) = name_text else {
        return return_null_with_error("module attribute name is not valid UTF-8");
    };
    let name_id = intern(name_text);
    // SAFETY: `as_module` proved the layout.
    let module_ref = unsafe { &*module };
    module_ref.attrs.get(&name_id).copied().unwrap_or_else(|| {
        let module_name = resolve(module_ref.name).unwrap_or_else(|| format!("<module:{}>", module_ref.name));
        return_null_with_error(format!("module '{module_name}' has no attribute '{name_text}'"))
    })
}

/// Status helper for hub integration that reports whether a module object owns an attribute.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_module_has_attr(module: *mut PyObject, name: u32) -> c_int {
    if module.is_null() {
        return return_minus_one_with_error("cannot query NULL module");
    }
    let Some(module) = as_module(module) else {
        return return_minus_one_with_error("attribute receiver is not a module");
    };
    // SAFETY: `as_module` proved the layout.
    i32::from(unsafe { (&*module).attrs.contains_key(&name) })
}
