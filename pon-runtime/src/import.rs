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

use crate::abi::{
    pon_const_int, pon_const_str, pon_none, pon_store_global, raise_import_error_text, return_minus_one_with_error, return_null_with_error,
};
use crate::abi::exc::raise_attribute_error_text;
use crate::intern::{intern, resolve};
use crate::object::{PyObject, PyObjectHeader, PyType, PyUnicode, as_object_ptr};
use crate::thread_state::{pon_err_clear, pon_err_occurred};

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
    /// Per-`current_modules` entry: the compiled-call stack depth captured at
    /// `begin_module_execution`.  Call-stack entries at or above this floor
    /// were pushed while the module body ran, so global loads/stores made by
    /// them scope to their own defining module, while a bare depth==floor
    /// context means the module toplevel itself is executing.
    current_module_floors: Vec<usize>,
    /// Live `sys.modules` dict mirroring `modules`; NULL until first use.
    modules_dict: *mut PyObject,
}

unsafe impl Send for ImportState {}

static IMPORT_STATE: LazyLock<Mutex<ImportState>> = LazyLock::new(|| Mutex::new(ImportState::new()));

impl ImportState {
    fn new() -> Self {
        let mut ty = Box::new(PyType::new(ptr::null(), "module", mem::size_of::<PyModuleObject>()));
        ty.tp_getattro = Some(module_getattro);
        ty.tp_setattro = Some(module_setattro);
        Self {
            modules: HashMap::new(),
            source_loader: None,
            module_type: Box::into_raw(ty),
            current_modules: Vec::new(),
            current_module_floors: Vec::new(),
            modules_dict: ptr::null_mut(),
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

/// Resets import state to its post-`pon_runtime_init` baseline. Intended for focused tests.
///
/// Clears the module cache, source loader, and importer stack. When the runtime
/// is already initialized this re-registers the curated native modules, because
/// `pon_runtime_init` is idempotent and will not restore them on a later call;
/// dropping them here would leave the process without `sys` for every
/// subsequently scheduled test (e.g. `pon_sys_set_argv` callers).
pub fn reset_import_state_for_tests() {
    {
        let mut state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
        state.modules.clear();
        state.source_loader = None;
        state.current_modules.clear();
        state.current_module_floors.clear();
        state.modules_dict = ptr::null_mut();
    }
    if crate::abi::runtime_is_initialized() {
        register_native_modules().expect("re-registering native modules after import-state reset");
    }
}

/// Installs the curated native modules into the import cache after core runtime
/// allocation is available.
pub fn register_native_modules() -> Result<(), String> {
    crate::native::register_modules()
}

/// Compiled top-level body of one AoT-embedded module.
///
/// Matches the zero-argument wrapper the AoT backend exports per embedded
/// reachability unit: runs the module body and returns a non-NULL object, or
/// NULL with the thread-state diagnostic set.
pub type EmbeddedModuleBody = unsafe extern "C" fn() -> *mut PyObject;

struct EmbeddedModule {
    is_package: bool,
    body: EmbeddedModuleBody,
}

/// AoT-embedded module registry keyed by fully-qualified dotted import name.
///
/// Populated by the generated `pon_aot_init_modules` hook before runtime
/// initialization; consulted by `import_module_by_name` after native curated
/// modules and the C-accelerated refusal list so an embedded file never
/// shadows either, mirroring JIT source-import resolution order.
static EMBEDDED_MODULES: LazyLock<Mutex<HashMap<String, EmbeddedModule>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Registers one AoT-embedded module body under its dotted import name.
///
/// Called from the generated `pon_aot_init_modules` hook with build-time
/// constant data, before `pon_runtime_init`. Invalid input records a
/// thread-state diagnostic and skips the entry, matching
/// `pon_aot_intern_name`'s error posture.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_aot_register_module(
    name: *const u8,
    name_len: usize,
    is_package: c_int,
    body: Option<EmbeddedModuleBody>,
) {
    // TAG-OK: build-time constant byte pointer and function pointer, never tagged values.
    let Some(body) = body else {
        crate::thread_state::pon_err_set("AoT module registrar received a null body pointer");
        return;
    };
    if name.is_null() {
        crate::thread_state::pon_err_set("AoT module registrar received a null name pointer");
        return;
    }
    // SAFETY: The generated registrar passes `name_len` contiguous constant bytes.
    let bytes = unsafe { core::slice::from_raw_parts(name, name_len) };
    let Ok(name) = std::str::from_utf8(bytes) else {
        crate::thread_state::pon_err_set("AoT module registrar received invalid UTF-8");
        return;
    };
    let mut modules = EMBEDDED_MODULES.lock().unwrap_or_else(|poison| poison.into_inner());
    modules.insert(
        name.to_owned(),
        EmbeddedModule {
            is_package: is_package != 0,
            body,
        },
    );
}

fn embedded_module(name: &str) -> Option<(bool, EmbeddedModuleBody)> {
    let modules = EMBEDDED_MODULES.lock().unwrap_or_else(|poison| poison.into_inner());
    modules.get(name).map(|module| (module.is_package, module.body))
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
        Err(message) => return raise_import_error_text(&message),
    };

    let imported = import_module_by_name(&name);
    let module = match imported {
        Ok(module) => module,
        Err(message) => return raise_import_error_text(&message),
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
    crate::untag_prelude!(module);
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
        match import_module_by_name(&child_name) {
            Ok(child) => return child,
            // A missing submodule falls through to the historical
            // cannot-import-name diagnostic; every other failure propagates
            // (CPython `_handle_fromlist` swallows only a ModuleNotFoundError
            // naming the child itself, so a deeper missing module raised
            // while executing the child's body must surface verbatim).
            Err(message) if message != format!("No module named '{child_name}'") => {
                return raise_import_error_text(&message);
            }
            Err(_) => {}
        }
    }
    raise_import_error_text(&format!("cannot import name '{attr}' from '{module_name}'"))
}

/// Imports all public module attributes into the active globals dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_import_star(module: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(module);
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

/// Returns the live `sys.modules` dict, allocating it on first use.
///
/// The dict mirrors the interned-name import cache: the runtime publishes
/// every module it registers, and `import` consults the dict as the
/// user-visible authority so `sys.modules[name] = module` (e.g. `collections`
/// publishing `collections.abc`) and `del sys.modules[name]` behave like
/// CPython.
pub fn sys_modules_dict() -> Result<*mut PyObject, String> {
    {
        let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
        if !state.modules_dict.is_null() {
            return Ok(state.modules_dict);
        }
    }
    // Allocate outside the state lock: dict construction takes its own locks.
    // SAFETY: A NULL item array with a zero pair count builds an empty dict.
    let dict = unsafe { crate::abi::map::pon_build_map(ptr::null_mut(), 0) };
    if dict.is_null() {
        return Err("failed to allocate the sys.modules dict".to_owned());
    }
    let mut state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    if state.modules_dict.is_null() {
        state.modules_dict = dict;
    }
    Ok(state.modules_dict)
}

/// Publishes a registered module into the live `sys.modules` dict.
///
/// Called with the import-state lock released: dict insertion takes the
/// dict's own critical section and must never nest inside `IMPORT_STATE`.
fn mirror_module_registration(name: &str, module: *mut PyObject) -> Result<(), String> {
    let dict = sys_modules_dict()?;
    let key = runtime_string(name)?;
    let _guard = crate::sync::begin_critical_section(dict);
    // SAFETY: `dict` is an exact runtime dict; `key` and `module` are valid.
    unsafe { crate::types::dict::dict_insert(dict, key, module) }
}

/// Reads the `sys.modules` binding for `name`, when the dict already exists.
fn sys_modules_entry(name: &str) -> Result<Option<*mut PyObject>, String> {
    let dict = {
        let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
        state.modules_dict
    };
    if dict.is_null() {
        return Ok(None);
    }
    let key = runtime_string(name)?;
    let _guard = crate::sync::begin_critical_section(dict);
    // SAFETY: `dict` is an exact runtime dict and `key` is a valid string.
    unsafe { crate::types::dict::dict_get(dict, key) }
}

/// Creates or replaces a module object in `sys.modules` with the supplied attrs.
pub fn install_module(name: &str, attrs: impl IntoIterator<Item = (u32, *mut PyObject)>) -> Result<*mut PyObject, String> {
    create_module(name, false, attrs)
}

fn import_module_by_name(name: &str) -> Result<*mut PyObject, String> {
    let module = resolve_module_by_name(name)?;
    if name == "os" {
        ensure_os_path_alias();
    }
    Ok(module)
}

/// CPython's `os.py` executes `import posixpath as path` and publishes
/// `sys.modules['os.path']`, so a plain `import os` already makes `os.path`
/// usable (`glob` reads `os.path.lexists` in a class body).  pon's `os` is a
/// native seed registered during runtime init, when the source importer that
/// serves `posixpath` cannot run yet — and resolving it inside the factory
/// would recurse, because `posixpath`'s own body does `import os`.
///
/// The alias is therefore installed right after any successful `os`
/// resolution: at that point `os` is cached, so posixpath's `import os` is a
/// cache hit and cannot recurse.  The in-progress flag breaks the remaining
/// cycle (`os.path` -> parent `os` -> this hook -> `os.path`), and the
/// native `os.path` row registers the module under both names and binds the
/// parent's `path` attribute (`bind_child_to_parent`).
///
/// Failure policy: embeddings without the vendored stdlib (runtime unit
/// tests) cannot resolve `posixpath`; the hook clears the pending diagnostic
/// and leaves `os.path` unbound — exactly the pre-alias behavior — instead
/// of failing `import os`.  A direct `import os.path` still surfaces the
/// real error loudly.
fn ensure_os_path_alias() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static IN_PROGRESS: AtomicBool = AtomicBool::new(false);
    let alias_id = intern("os.path");
    let alias_cached = {
        let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
        state.modules.contains_key(&alias_id)
    };
    if alias_cached || IN_PROGRESS.swap(true, Ordering::AcqRel) {
        return;
    }
    let result = import_module_by_name("os.path");
    IN_PROGRESS.store(false, Ordering::Release);
    if result.is_err() && pon_err_occurred() {
        pon_err_clear();
    }
}

fn resolve_module_by_name(name: &str) -> Result<*mut PyObject, String> {
    if name.is_empty() {
        return Err("No module named ''".to_owned());
    }

    let name_id = intern(name);
    let cached = {
        let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
        state.modules.get(&name_id).copied()
    };
    match (cached, sys_modules_entry(name)?) {
        // Steady state: the cache and `sys.modules` agree.
        (Some(module), Some(entry)) if module == entry => return Ok(module),
        // A binding the user inserted or replaced through `sys.modules` wins.
        (_, Some(entry)) => {
            let mut state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
            state.modules.insert(name_id, entry);
            drop(state);
            // J0.3 GlobalIC site: the module behind this name changed.
            crate::abi::bump_namespace_version();
            return Ok(entry);
        }
        // The user deleted the binding: forget the cache entry and re-import.
        (Some(_), None) => {
            let mut state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
            state.modules.remove(&name_id);
            drop(state);
            // J0.3 GlobalIC site: the module behind this name was dropped.
            crate::abi::bump_namespace_version();
        }
        (None, None) => {}
    }

    if let Some(parent) = parent_module_name(name) {
        import_module_by_name(parent)?;
        // CPython bootstrap's "crazy side-effects" re-check: executing the
        // parent may have published this very name into `sys.modules`
        // (e.g. `collections/__init__.py` registers `collections.abc` as an
        // alias of `_collections_abc`). Adopt that binding instead of
        // resolving the child from disk.
        if let Some(entry) = sys_modules_entry(name)? {
            let mut state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
            state.modules.insert(name_id, entry);
            drop(state);
            // J0.3 GlobalIC site: a new name -> module binding appeared.
            crate::abi::bump_namespace_version();
            return Ok(entry);
        }
    }

    if let Some(module) = native_module(name)? {
        let mut state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
        state.modules.insert(name_id, module);
        drop(state);
        mirror_module_registration(name, module)?;
        bind_child_to_parent(name, module);
        return Ok(module);
    }

    if is_unsupported_c_accelerated(name) {
        return Err(format!("module '{name}' is C-accelerated and unsupported"));
    }

    if let Some((is_package, body)) = embedded_module(name) {
        let module = create_module(name, is_package, [])?;
        bind_child_to_parent(name, module);
        begin_module_execution(name)?;
        // SAFETY: The body is compiled top-level code registered by this
        // process's AoT image; it follows the NULL-sentinel error contract.
        let loaded = unsafe { body() };
        end_module_execution(name);
        if loaded.is_null() {
            if pon_err_occurred() {
                return Err(format!("embedded module '{name}' returned NULL"));
            }
            return Err(format!("embedded module '{name}' returned NULL without setting an exception"));
        }
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
            // CPython sets `__file__` on source-imported modules, as an
            // absolute path (sys.path[0] is absolutized since 3.11; abspath,
            // not realpath). The JIT-loader path is the only importer that
            // knows a source path (AoT-embedded bodies have none).
            let file_path = std::path::absolute(&spec.path).unwrap_or_else(|_| spec.path.clone());
            let file_object = runtime_string(&file_path.to_string_lossy())?;
            let module = create_module(name, spec.is_package, [(intern("__file__"), file_object)])?;
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

    Err(format!("No module named '{name}'"))
}

fn native_module(name: &str) -> Result<Option<*mut PyObject>, String> {
    crate::native::make_module(name)
}

/// Names refused with a precise diagnostic instead of a confusing source-import
/// failure. Consulted AFTER the native registry, so landing a native module
/// (one `NATIVE_MODULES` row) shadows its entry here; delete the stale entry in
/// the same change.
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

/// Environment override for the vendored-stdlib search root (HANDOFF J0.4).
/// When set it is authoritative: the value is used as the stdlib root if that
/// directory exists, and the built-in locations are not consulted.
pub const STDLIB_PATH_ENV_VAR: &str = "PON_STDLIB_PATH";

/// Workspace-relative location of the vendored CPython `Lib/` tree (L0 lands
/// the real vendoring; the directory currently holds a stub).
const VENDORED_STDLIB_SUFFIX: &str = "pon-conformance/vendor/cpython-3.14/Lib";

fn search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut push_root = |root: PathBuf| {
        if !roots.contains(&root) {
            roots.push(root);
        }
    };
    if let Ok(cwd) = env::current_dir() {
        push_root(cwd.clone());
        push_root(cwd.join(".pon").join("packages").join("site-packages"));
        push_root(cwd.join("pon-conformance").join("corpus"));
    }
    for var in ["PONPATH", "PON_IMPORT_PATH"] {
        if let Ok(extra) = env::var(var) {
            for root in env::split_paths(&extra) {
                push_root(root);
            }
        }
    }
    if let Some(stdlib) = vendored_stdlib_root() {
        push_root(stdlib);
    }
    roots
}

/// Resolves the vendored-stdlib root, always LAST in import resolution order
/// (native curated -> installed packages -> source roots -> vendored stdlib).
///
/// `PON_STDLIB_PATH` is authoritative when set: a missing directory there
/// disables the root rather than falling back. Otherwise the workspace vendor
/// tree is located from this crate's compile-time manifest path, then relative
/// to the current directory. An absent directory is silently skipped so
/// deployments without the vendor tree keep working.
fn vendored_stdlib_root() -> Option<PathBuf> {
    if let Ok(value) = env::var(STDLIB_PATH_ENV_VAR) {
        if value.is_empty() {
            return None;
        }
        let root = PathBuf::from(value);
        return root.is_dir().then_some(root);
    }
    let mut candidates = Vec::with_capacity(2);
    if let Some(workspace) = Path::new(env!("CARGO_MANIFEST_DIR")).parent() {
        candidates.push(workspace.join(VENDORED_STDLIB_SUFFIX));
    }
    if let Ok(cwd) = env::current_dir() {
        candidates.push(cwd.join(VENDORED_STDLIB_SUFFIX));
    }
    candidates.into_iter().find(|root| root.is_dir())
}

/// Ordered source-import roots the runtime consults for pure-Python modules:
/// current directory, installed packages, the conformance corpus,
/// `PONPATH`/`PON_IMPORT_PATH` entries, then the vendored stdlib last.
/// Exposed so AoT reachability resolves static imports with exactly the
/// runtime's search order and embeds what the runtime would otherwise have to
/// source-load.
#[must_use]
pub fn source_search_roots() -> Vec<PathBuf> {
    search_roots()
}

/// True when `import name` never reaches source-root resolution at runtime
/// because the curated native registry or the C-accelerated refusal list
/// serves it first. AoT reachability consults this so a same-named `.py` on a
/// source root is never embedded: the runtime would never execute it.
/// Installed-package fixtures also shadow source files but depend on process
/// environment, so they are deliberately not reflected here; a unit they
/// shadow is dead weight in the binary, not a behavior change.
#[must_use]
pub fn import_shadowed_from_source(name: &str) -> bool {
    crate::native::is_native_module(name) || is_unsupported_c_accelerated(name)
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
    drop(state);
    mirror_module_registration(name, object)?;
    // J0.3 GlobalIC site: a (re)installed module can replace the module whose
    // attrs overlay `pon_load_global` currently consults.
    crate::abi::bump_namespace_version();
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
    // J0.3 GlobalIC site: parent-module attr overlay insert.
    crate::abi::bump_namespace_version();
}

pub fn begin_module_execution(name: &str) -> Result<(), String> {
    let name_id = intern(name);
    // Captured before taking the import lock: the depth belongs to the
    // executing thread's compiled-call stack.
    let floor = crate::abi::current_function_stack_depth();
    let mut state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    if !state.modules.contains_key(&name_id) {
        return Err(format!("cannot execute uncached module '{name}'"));
    }
    state.current_modules.push(name_id);
    state.current_module_floors.push(floor);
    // J0.3 GlobalIC site: context switch changes which attr overlay
    // `pon_load_global` consults.
    crate::abi::bump_namespace_version();
    Ok(())
}

pub fn end_module_execution(name: &str) {
    let name_id = intern(name);
    let mut state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    if state.current_modules.last().copied() == Some(name_id) {
        state.current_modules.pop();
        state.current_module_floors.pop();
        // J0.3 GlobalIC site: context switch (see begin_module_execution).
        crate::abi::bump_namespace_version();
    }
}

/// Compiled-call stack depth captured when the innermost active module body
/// began executing; `0` when no module body is active.  Call-stack entries at
/// or above this floor were pushed by calls made during the module body, so
/// only they may scope a global load/store to their defining module.
#[must_use]
pub fn active_module_call_floor() -> usize {
    let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    state.current_module_floors.last().copied().unwrap_or(0)
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

pub fn active_module_name_id() -> Option<u32> {
    let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    state.current_modules.last().copied()
}

pub fn module_attrs_snapshot(module_name: u32) -> Option<Vec<(u32, *mut PyObject)>> {
    let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    let module = state.modules.get(&module_name).copied()?;
    let module = module_from_object_locked(&state, module)?;
    // SAFETY: The import state proved the object uses `PyModuleObject` layout.
    let module = unsafe { &*module };
    Some(module.attrs.iter().map(|(name, value)| (*name, *value)).collect())
}

/// GC roots held by the import registry: every registered module's attribute
/// values plus the live `sys.modules` dict.  Module objects are immortal
/// leaked boxes ([`create_module`] never frees them), so marking cannot reach
/// the GC-heap values their attrs hold; without these roots an explicit
/// `gc.collect()` frees live module globals (any module-scope binding, every
/// module).  Non-module `sys.modules` entries (arbitrary objects installed
/// via `sys.modules[name] = obj`) are rooted directly, like CPython's dict
/// reference keeps them alive.
///
/// Consumed by `crate::abi::collect` while the runtime lock is held: takes
/// only the import-state mutex and never re-enters the runtime.  `collect`
/// runs solely from explicit `gc.collect()` calls, and Python code never
/// executes while `IMPORT_STATE` is locked, so the mutex is always free here.
pub fn gc_held_roots() -> Vec<*mut PyObject> {
    let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut roots = Vec::new();
    if !state.modules_dict.is_null() {
        roots.push(state.modules_dict);
    }
    for &object in state.modules.values() {
        match module_from_object_locked(&state, object) {
            Some(module) => {
                // SAFETY: The layout check proved `PyModuleObject`; attrs are
                // enumerated under the import-state lock.
                for (_, &value) in unsafe { (*module).attrs.iter() } {
                    if !value.is_null() && crate::tag::is_heap(value) {
                        roots.push(value);
                    }
                }
            }
            None => {
                if !object.is_null() && crate::tag::is_heap(object) {
                    roots.push(object);
                }
            }
        }
    }
    roots
}

pub fn active_module_attrs_snapshot() -> Option<Vec<(u32, *mut PyObject)>> {
    let module_name = active_module_name_id()?;
    module_attrs_snapshot(module_name)
}

pub fn active_module_attr(name: u32) -> Option<*mut PyObject> {
    let current = {
        let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
        state.current_modules.last().copied()?
    };
    module_attr(current, name)
}

/// Live attribute binding of one cached module, by interned module name.
pub fn module_attr(module_name: u32, name: u32) -> Option<*mut PyObject> {
    let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    let module = state.modules.get(&module_name).copied()?;
    let module = module_from_object_locked(&state, module)?;
    // SAFETY: The import state proved the object uses `PyModuleObject` layout.
    unsafe { (&*module).attrs.get(&name).copied() }
}

pub fn store_active_module_attr(name: u32, value: *mut PyObject) -> bool {
    let current = {
        let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
        let Some(current) = state.current_modules.last().copied() else {
            return false;
        };
        current
    };
    store_module_attr(current, name, value)
}

/// Store one attribute binding into a cached module, by interned module name.
pub fn store_module_attr(module_name: u32, name: u32, value: *mut PyObject) -> bool {
    let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    let Some(module) = state.modules.get(&module_name).copied() else {
        return false;
    };
    let Some(module) = module_from_object_locked(&state, module) else {
        return false;
    };
    // SAFETY: The import state proved the object uses `PyModuleObject` layout.
    unsafe {
        (&mut *module).attrs.insert(name, value);
    }
    // J0.3 GlobalIC site: module attr overlay insert/replace.
    crate::abi::bump_namespace_version();
    true
}

pub fn delete_active_module_attr(name: u32) -> bool {
    let current = {
        let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
        let Some(current) = state.current_modules.last().copied() else {
            return false;
        };
        current
    };
    delete_module_attr(current, name)
}

/// Delete one attribute binding from a cached module, by interned module name.
pub fn delete_module_attr(module_name: u32, name: u32) -> bool {
    let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    let Some(module) = state.modules.get(&module_name).copied() else {
        return false;
    };
    let Some(module) = module_from_object_locked(&state, module) else {
        return false;
    };
    // SAFETY: The import state proved the object uses `PyModuleObject` layout.
    let removed = unsafe { (&mut *module).attrs.remove(&name).is_some() };
    if removed {
        // J0.3 GlobalIC site: module attr overlay removal.
        crate::abi::bump_namespace_version();
    }
    removed
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
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process;
    use std::ptr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{STDLIB_PATH_ENV_VAR, pon_import_from, pon_import_name, reset_import_state_for_tests, resolve_import_name};
    use crate::abi::{format_object_for_print, pon_runtime_init};
    use crate::intern::intern;
    use crate::thread_state::{pon_err_clear, pon_err_message, test_state_lock};

    static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

    struct TempImportRoot {
        path: PathBuf,
    }

    impl TempImportRoot {
        fn new() -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!("pon-import-source-root-{}-{id}", process::id()));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempImportRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    struct EnvVarGuard {
        name: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(name: &'static str, value: &Path) -> Self {
            let previous = env::var_os(name);
            unsafe {
                env::set_var(name, value);
            }
            Self { name, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(previous) = &self.previous {
                    env::set_var(self.name, previous);
                } else {
                    env::remove_var(self.name);
                }
            }
        }
    }

    struct ResetImportStateOnDrop;

    impl Drop for ResetImportStateOnDrop {
        fn drop(&mut self) {
            reset_import_state_for_tests();
        }
    }
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

    #[test]
    fn pon_import_path_root_loads_curated_source_module() {
        let _guard = test_state_lock();
        let _reset = ResetImportStateOnDrop;
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
        }
        pon_err_clear();
        reset_import_state_for_tests();

        let root = TempImportRoot::new();
        let module_name = format!("pon_import_path_{}_source", process::id());
        let module_path = root.path().join(format!("{module_name}.py"));
        fs::write(&module_path, "marker = 'loaded-via-pon-import-path'\nanswer = 42\n").unwrap();
        let _env = EnvVarGuard::set("PON_IMPORT_PATH", root.path());

        let module = unsafe { pon_import_name(intern(&module_name), ptr::null(), 0, 0) };
        assert!(
            !module.is_null(),
            "importing source module from PON_IMPORT_PATH failed: {:?}",
            pon_err_message()
        );

        let marker = unsafe { pon_import_from(module, intern("marker")) };
        assert_eq!(
            format_object_for_print(marker).as_deref(),
            Ok("loaded-via-pon-import-path")
        );
        let answer = unsafe { pon_import_from(module, intern("answer")) };
        assert_eq!(format_object_for_print(answer).as_deref(), Ok("42"));
    }

    #[test]
    fn vendored_stdlib_root_loads_module_by_default() {
        let _guard = test_state_lock();
        let _reset = ResetImportStateOnDrop;
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
        }
        pon_err_clear();
        reset_import_state_for_tests();

        let module = unsafe { pon_import_name(intern("pon_tiny"), ptr::null(), 0, 0) };
        assert!(
            !module.is_null(),
            "importing pon_tiny from the vendored stdlib root failed: {:?}",
            pon_err_message()
        );
        let name = unsafe { pon_import_from(module, intern("name")) };
        assert_eq!(format_object_for_print(name).as_deref(), Ok("tiny"));
    }

    #[test]
    fn stdlib_path_env_var_overrides_vendored_root() {
        let _guard = test_state_lock();
        let _reset = ResetImportStateOnDrop;
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
        }
        pon_err_clear();
        reset_import_state_for_tests();

        let root = TempImportRoot::new();
        fs::write(root.path().join("pon_tiny.py"), "name = 'override'\n").unwrap();
        let _env = EnvVarGuard::set(STDLIB_PATH_ENV_VAR, root.path());

        let module = unsafe { pon_import_name(intern("pon_tiny"), ptr::null(), 0, 0) };
        assert!(
            !module.is_null(),
            "importing pon_tiny via PON_STDLIB_PATH failed: {:?}",
            pon_err_message()
        );
        let name = unsafe { pon_import_from(module, intern("name")) };
        assert_eq!(format_object_for_print(name).as_deref(), Ok("override"));
    }

    #[test]
    fn missing_stdlib_override_skips_vendored_root() {
        let _guard = test_state_lock();
        let _reset = ResetImportStateOnDrop;
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
        }
        pon_err_clear();
        reset_import_state_for_tests();

        let missing = env::temp_dir().join(format!("pon-stdlib-missing-{}", process::id()));
        let _env = EnvVarGuard::set(STDLIB_PATH_ENV_VAR, &missing);

        let module = unsafe { pon_import_name(intern("pon_tiny"), ptr::null(), 0, 0) };
        assert!(
            module.is_null(),
            "pon_tiny import should fail when PON_STDLIB_PATH points at a missing dir"
        );
        pon_err_clear();
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

/// Live namespace dict for a module OBJECT, or `None` when `object` is not a
/// module. This is the same dict `module.__dict__` serves (mutations through
/// it sync back into module attrs); `dir(module)` enumerates it exactly like
/// CPython's `module.__dir__`, which returns `list(module.__dict__)`.
pub(crate) fn module_namespace_for_object(object: *mut PyObject) -> Option<Result<*mut PyObject, String>> {
    let module = as_module(object)?;
    // SAFETY: `as_module` proved the `PyModuleObject` layout.
    Some(crate::dynexec::module_namespace_dict(unsafe { (*module).name }))
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
    if name_text == "__dict__" {
        // The live namespace view: mutations through it sync back into the
        // module attrs via the dynexec globals-registry hooks (CPython's
        // module `__dict__` IS the module namespace).
        return match crate::dynexec::module_namespace_dict(unsafe { (*module).name }) {
            Ok(dict) => dict,
            Err(message) => return_null_with_error(message),
        };
    }
    let name_id = intern(name_text);
    // SAFETY: `as_module` proved the layout.
    let module_ref = unsafe { &*module };
    if let Some(value) = module_ref.attrs.get(&name_id).copied() {
        return value;
    }
    // Attrs miss: consult the registered namespace dict. CPython module
    // attribute lookup IS a `__dict__` lookup, so bindings created only
    // through the dict view (`vars(mod)["k"] = v`) must resolve here too.
    if let Some(value) = crate::dynexec::peek_module_namespace_value(module_ref.name, name_text) {
        return value;
    }
    let module_name = resolve(module_ref.name).unwrap_or_else(|| format!("<module:{}>", module_ref.name));
    raise_attribute_error_text(&format!("module '{module_name}' has no attribute '{name_text}'"))
}

/// `module.attr = value` / `del module.attr` (CPython module objects are
/// plain namespaces; `_py_warnings` bumps `_filters_version` on its module
/// object).  Mirrors [`store_module_attr`]/[`delete_module_attr`], including
/// the J0.3 GlobalIC bump: module attrs overlay `pon_load_global`.
unsafe extern "C" fn module_setattro(module: *mut PyObject, name: *mut PyObject, value: *mut PyObject) -> c_int {
    if module.is_null() || name.is_null() {
        return_null_with_error("module attribute assignment received NULL");
        return -1;
    }
    let Some(module) = as_module(module) else {
        return_null_with_error("attribute receiver is not a module");
        return -1;
    };
    // SAFETY: Attribute names are allocated by `abstract_op` as `PyUnicode`.
    let name_text = unsafe { (&*name.cast::<PyUnicode>()).as_str() };
    let Some(name_text) = name_text else {
        return_null_with_error("module attribute name is not valid UTF-8");
        return -1;
    };
    let name_id = intern(name_text);
    // SAFETY: `as_module` proved the layout.
    let module_name_id = unsafe { (&*module).name };
    if value.is_null() {
        // SAFETY: `as_module` proved the layout.
        let removed = unsafe { (&mut *module).attrs.remove(&name_id).is_some() };
        if !removed {
            let module_name = resolve(module_name_id).unwrap_or_else(|| format!("<module:{module_name_id}>"));
            raise_attribute_error_text(&format!("module '{module_name}' has no attribute '{name_text}'"));
            return -1;
        }
        // Keep the registered namespace dict (`module.__dict__`) coherent:
        // `dir(module)` and the getattro fallback read it.
        crate::dynexec::sync_global_delete_for_module(module_name_id, name_id);
    } else {
        // SAFETY: `as_module` proved the layout.
        unsafe {
            (&mut *module).attrs.insert(name_id, value);
        }
        crate::dynexec::sync_global_store_for_module(module_name_id, name_id, value);
    }
    // J0.3 GlobalIC site: module attr overlay insert/replace/removal.
    crate::abi::bump_namespace_version();
    0
}

/// Status helper for hub integration that reports whether a module object owns an attribute.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_module_has_attr(module: *mut PyObject, name: u32) -> c_int {
    crate::untag_prelude!(err = -1; module);
    if module.is_null() {
        return return_minus_one_with_error("cannot query NULL module");
    }
    let Some(module) = as_module(module) else {
        return return_minus_one_with_error("attribute receiver is not a module");
    };
    // SAFETY: `as_module` proved the layout.
    i32::from(unsafe { (&*module).attrs.contains_key(&name) })
}
