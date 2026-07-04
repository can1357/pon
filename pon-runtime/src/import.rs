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
use std::sync::atomic::{AtomicU64, Ordering};

use crate::abi::{
    pon_const_int, pon_const_str, pon_none, pon_store_global, raise_import_error_text, return_minus_one_with_error, return_null_with_error,
};
use crate::abi::exc::{pon_raise_type_error, raise_attribute_error_text};
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
    /// Interned registry identity keying the dynexec globals registry
    /// (`module.__dict__` / `dir` / attr-store mirroring) and attr-snapshot
    /// lookups.  Equal to `name` for imported/installed modules; unique per
    /// instance for synthetic `types.ModuleType(...)` modules so a synthetic
    /// module named like a real one never aliases the real module's
    /// namespace dict.
    pub registry_key: u32,
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
        // Direct calls on the module type (`types.ModuleType(name, doc)`)
        // MUST NOT fall back to the generic `type_new` heap-instance
        // allocator: the attr hooks above reinterpret the instance as
        // `PyModuleObject`, and a `PyHeapInstance`'s bytes read as a garbage
        // attrs `HashMap` (UB on first insert).
        ty.tp_new = Some(module_tp_new);
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
    // Late-bind the module type's base: `object` exists only once core
    // runtime globals are installed, while the module type is created with
    // the import state (possibly earlier).  The base wires the generic
    // keyword-call path (`types.ModuleType(name, doc=...)`): with a custom
    // `tp_new`, `call_type_with_keywords` must resolve `__init__` to the
    // inherited `object.__init__` carrier (the `_contextvars` pattern).
    // Safe to bind late: the module type never owns a `tp_mro` carrier, so
    // every MRO walk reads the live `tp_base` chain.
    let module_type = {
        let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
        state.module_type
    };
    // SAFETY: The module type is an immortal leaked box created by
    // `ImportState::new`; no Python executes concurrently with runtime init.
    unsafe {
        if (*module_type).tp_base.is_null() {
            (*module_type).tp_base = crate::abi::runtime_global(intern("object"))
                .map_or(ptr::null_mut(), |object| object.cast::<PyType>());
        }
    }
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

/// Imports module attributes into the active globals dictionary, honoring
/// `__all__` exactly like CPython's `import_all_from`: when the module
/// defines `__all__`, precisely those names are copied — underscored names
/// included, non-str items raise TypeError, and names the module lacks
/// raise AttributeError (after a package-submodule import attempt, per
/// `_handle_fromlist`'s `*` expansion).  Only a module without `__all__`
/// falls back to the public (non-underscore) attribute snapshot.
///
/// Honoring `__all__` is load-bearing for packages whose `__init__` reads
/// sibling-submodule bindings after star-imports: `asyncio/__init__` runs
/// `from .subprocess import *` and later `subprocess.__all__` — the
/// submodule's own `import subprocess` global must not leak through the
/// star-copy and clobber the package's `subprocess` -> `asyncio.subprocess`
/// child binding.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_import_star(module: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(module);
    if module.is_null() {
        return return_null_with_error("cannot import * from NULL module");
    }
    let Some(module) = as_module(module) else {
        return return_null_with_error("import-star receiver is not a module");
    };
    // Aliasing discipline: the stores and the package-submodule import below
    // re-enter machinery that may mutate this very module's attr map
    // (`pon_store_global` when a package star-imports in its own body,
    // `bind_child_to_parent` inside `import_module_by_name`), so no borrow
    // of the module may live across them — attrs are snapshotted or
    // re-borrowed per access through the raw pointer.
    // SAFETY: `as_module` proved the layout; the borrow ends at `.copied()`.
    let all = unsafe { (*module).attrs.get(&intern("__all__")).copied() };
    let Some(all) = all else {
        // No `__all__`: copy the public attribute snapshot.
        // SAFETY: The borrow ends when the snapshot Vec is collected.
        let entries: Vec<(u32, *mut PyObject)> = unsafe {
            (*module)
                .attrs
                .iter()
                .filter(|&(&name, _)| is_public_name(name))
                .map(|(&name, &value)| (name, value))
                .collect()
        };
        for (name, value) in entries {
            // SAFETY: Store helper enforces the NULL-sentinel error contract.
            let stored = unsafe { pon_store_global(name, value) };
            if stored.is_null() {
                return ptr::null_mut();
            }
        }
        // SAFETY: `pon_none` returns the initialized singleton or NULL with an error.
        return unsafe { pon_none() };
    };
    // SAFETY: Reading the interned name id copies a u32 out of the borrow.
    let module_name = {
        let name = unsafe { (*module).name };
        resolve(name).unwrap_or_else(|| format!("<module:{name}>"))
    };
    // SAFETY: The borrow ends when `module_is_package` returns.
    let is_package = unsafe { module_is_package(&*module) };
    let Some(items) = sequence_items(all) else {
        return return_null_with_error(format!("{module_name}.__all__ is not a tuple or list"));
    };
    for item in items {
        let Some(text) = (unsafe { exact_str_text(item) }) else {
            // SAFETY: Type-name probe tolerates any live object.
            let kind = unsafe { crate::types::dict::type_name(item) }.unwrap_or("<unknown>");
            return crate::abi::exc::raise_kind_error_text(
                crate::types::exc::ExceptionKind::TypeError,
                &format!("Item in {module_name}.__all__ must be str, not {kind}"),
            );
        };
        let name = intern(&text);
        // SAFETY: The borrow ends at `.copied()`, before any re-entrant call.
        let value = match unsafe { (*module).attrs.get(&name).copied() } {
            Some(value) => value,
            None => {
                // A package `__all__` may name submodules that only importing
                // makes visible: CPython's `_handle_fromlist` imports them for
                // `from pkg import *` and swallows only the child's own
                // ModuleNotFoundError (deeper failures surface verbatim).
                let child_name = format!("{module_name}.{text}");
                match (is_package, import_module_by_name(&child_name)) {
                    (true, Ok(child)) => child,
                    (true, Err(message)) if message != format!("No module named '{child_name}'") => {
                        return raise_import_error_text(&message);
                    }
                    _ => {
                        return raise_attribute_error_text(&format!(
                            "module '{module_name}' has no attribute '{text}'"
                        ));
                    }
                }
            }
        };
        // SAFETY: Store helper enforces the NULL-sentinel error contract.
        let stored = unsafe { pon_store_global(name, value) };
        if stored.is_null() {
            return ptr::null_mut();
        }
    }
    // SAFETY: `pon_none` returns the initialized singleton or NULL with an error.
    unsafe { pon_none() }
}

/// Snapshot of the element slots of an exact tuple or list receiver; `None`
/// for any other layout (subclasses included — stdlib `__all__` is always
/// exact).  A copy, not a borrow: the star-import caller re-enters runtime
/// code between elements, which may reallocate a list's storage.
fn sequence_items(object: *mut PyObject) -> Option<Vec<*mut PyObject>> {
    // SAFETY: Type-name probes tolerate any live object; the casts below are
    // guarded by the exact layout checks, and the slices are copied before
    // the borrow ends.
    unsafe {
        if crate::types::int::type_name_is(object, "tuple") {
            return Some((&*object.cast::<crate::types::tuple::PyTuple>()).as_slice().to_vec());
        }
        if crate::types::int::type_name_is(object, "list") {
            return Some((&*object.cast::<crate::types::list::PyList>()).as_slice().to_vec());
        }
    }
    None
}

/// Exact-`str` payload (no `__str__` dispatch); `None` for other layouts.
unsafe fn exact_str_text(object: *mut PyObject) -> Option<String> {
    // SAFETY: Caller passes a live object; the cast is guarded by the exact
    // layout check.
    unsafe {
        if !crate::types::int::type_name_is(object, "str") {
            return None;
        }
        let unicode = &*object.cast::<PyUnicode>();
        if unicode.data.is_null() && unicode.len != 0 {
            return None;
        }
        let bytes = core::slice::from_raw_parts(unicode.data, unicode.len);
        core::str::from_utf8(bytes).ok().map(ToOwned::to_owned)
    }
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
    if name == "importlib._bootstrap" {
        ensure_source_importlib_alias("_frozen_importlib", module)?;
        seed_meta_path_finders();
    }
    Ok(module)
}

/// After pon's source fallback imports `importlib._bootstrap`, later stdlib
/// modules still absolute-import `_frozen_importlib` for the bootstrap classes
/// it exposes (`importlib.abc` registers them with its ABCs).  Mirror the live
/// source bootstrap module under that legacy top-level key once it exists, but
/// only while the alias slot is still empty: user-inserted `sys.modules`
/// bindings keep winning.
fn ensure_source_importlib_alias(alias: &str, module: *mut PyObject) -> Result<(), String> {
    if sys_modules_entry(alias)?.is_some() {
        return Ok(());
    }
    let alias_id = intern(alias);
    let mut state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    state.modules.insert(alias_id, module);
    drop(state);
    mirror_module_registration(alias, module)
}

/// Mirrors `importlib._bootstrap._install`: seeds `sys.meta_path` with
/// `BuiltinImporter` and `FrozenImporter` right after the bootstrap module
/// first loads.
///
/// CPython's interpreter init execs the frozen bootstrap and immediately
/// runs `_install(sys, _imp)`, which appends the two finders.  Under pon the
/// vendored `importlib/__init__.py` takes its source-fallback branch
/// (`import _frozen_importlib` fails), which calls only `_setup` — no code
/// path ever runs `_install` — so `_bootstrap._find_spec` would find the
/// list empty and take `_py_warnings.warn`, and every
/// `importlib.import_module` of a name pon cannot serve would derail there
/// instead of raising the CPython `ModuleNotFoundError`.  Appending here,
/// the moment the class objects exist as module attrs, lands the same end
/// state at the equivalent init moment.  `_setup` itself never reads
/// `meta_path`, so running before it (pon) vs after it (CPython) is not
/// observable.
///
/// The third slot — CPython's `PathFinder`, appended by the separate
/// `_install_external_importers` init step from `_bootstrap_external` — is
/// filled by pon's own `_pon_source_importer` module
/// (`crate::native::imp::make_source_importer_module`) instead: `PathFinder`
/// itself would route `importlib.import_module` through vendored
/// file-system loaders pon does not run, while the stand-in claims exactly
/// the names pon's embedded/source machinery serves and delegates loading to
/// it.  Documented divergence: `sys.meta_path[2]` is that module object, not
/// the `PathFinder` class, and the bootstrap classes' `__module__` is
/// `'importlib._bootstrap'`, not `'_frozen_importlib'`.
///
/// The append only fires while the list is still empty: CPython never
/// re-runs `_install` either, so a re-import after a user cleared
/// `sys.modules['importlib._bootstrap']` must not grow or reorder a list
/// the user may have replaced.
///
/// Failure policy: mirrors `ensure_os_path_alias` — a missing `sys` module,
/// `meta_path` binding, bootstrap class, or non-list value leaves the list
/// untouched and clears any pending diagnostic rather than failing the
/// `importlib` import; the loud surface is then `_find_spec`'s own
/// empty-meta_path warning.
fn seed_meta_path_finders() {
    let Some(meta_path) = module_attr(intern("sys"), intern("meta_path")) else {
        return;
    };
    if crate::abi::seq::list_len(meta_path) != Some(0) {
        return;
    }
    let bootstrap = intern("importlib._bootstrap");
    let Some(builtin_importer) = module_attr(bootstrap, intern("BuiltinImporter")) else {
        return;
    };
    let Some(frozen_importer) = module_attr(bootstrap, intern("FrozenImporter")) else {
        return;
    };
    let finders = match crate::native::imp::make_source_importer_module() {
        Ok(source_importer) => [builtin_importer, frozen_importer, source_importer],
        // Allocation failure: seed the two bootstrap classes and stay quiet
        // (failure policy above); statement imports are unaffected.
        Err(_) => [builtin_importer, frozen_importer, ptr::null_mut()],
    };
    for finder in finders {
        if finder.is_null() {
            continue;
        }
        if crate::abi::seq::list_append_raw(meta_path, finder).is_err() {
            break;
        }
    }
    if pon_err_occurred() {
        pon_err_clear();
    }
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

/// True when a `sys.modules` binding is the `None` singleton — the deliberate
/// import block `test.support.import_helper.import_fresh_module` plants for
/// each blocked name (tag-tolerant, like any generated-code dict value).
fn is_none_binding(binding: *mut PyObject) -> bool {
    // SAFETY: Singleton accessor; a NULL from pre-init failure never equals
    // a live dict binding.
    crate::tag::untag_arg(binding) == unsafe { pon_none() }
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
    let entry = sys_modules_entry(name)?;
    // CPython `_find_and_load`: a `None` binding in `sys.modules` is a
    // deliberate import block — halt with the bootstrap's exact diagnostic
    // (typed `ModuleNotFoundError` by `raise_import_error_text`) before any
    // parent import or resolution side effect, so the `except ImportError:`
    // accelerator fallbacks (bisect/queue/stat/collections/decimal) take
    // their pure-Python arm instead of receiving `None` as a module.
    if entry.is_some_and(is_none_binding) {
        return Err(format!("import of {name} halted; None in sys.modules"));
    }
    match (cached, entry) {
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
        // A `None` block planted mid-flight by the parent's own body is
        // "keep loading" in the bootstrap's crazy-side-effects recheck
        // (`sys.modules.get(name) is not None`) — never an adoptable
        // binding, and not a halt either.
        if let Some(entry) = sys_modules_entry(name)?.filter(|&entry| !is_none_binding(entry)) {
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

    if let Some(path) = find_extension_module(name) {
        let module = crate::capi::load_extension_module(name, &path)?;
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
        // Module top-level `try/except` parks the handled exception like any
        // frame; bracket the body so the park never outlives the import.
        let handled_guard = crate::abi::HandledExcGuard::enter();
        // SAFETY: The body is compiled top-level code registered by this
        // process's AoT image; it follows the NULL-sentinel error contract.
        let loaded = unsafe { body() };
        drop(handled_guard);
        end_module_execution(name);
        if loaded.is_null() {
            evict_failed_module(name, module);
            if pon_err_occurred() {
                return Err(format!("embedded module '{name}' returned NULL"));
            }
            return Err(format!("embedded module '{name}' returned NULL without setting an exception"));
        }
        return adopt_post_body_sys_modules_replacement(name, module);
    }

    if let Some(spec) = find_source_module(name) {
        let module_attrs = source_module_attrs(&spec)?;
        let Some(source_path) = spec.path.as_ref() else {
            let module = create_module(name, true, module_attrs)?;
            bind_child_to_parent(name, module);
            return Ok(module);
        };
        let source = fs::read_to_string(source_path)
            .map_err(|error| format!("failed to read source module '{}': {error}", source_path.display()))?;
        let loader = {
            let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
            state.source_loader
        };
        if let Some(loader) = loader {
            let module = create_module(name, spec.is_package, module_attrs)?;
            bind_child_to_parent(name, module);
            begin_module_execution(name)?;
            // Bracket the module body like any call boundary (see the
            // embedded-module leg above).
            let handled_guard = crate::abi::HandledExcGuard::enter();
            let loaded = loader(SourceModuleRequest {
                name,
                path: source_path,
                source: &source,
                is_package: spec.is_package,
            });
            drop(handled_guard);
            end_module_execution(name);
            let loaded = match loaded {
                Ok(loaded) => loaded,
                Err(error) => {
                    evict_failed_module(name, module);
                    return Err(error);
                }
            };
            if loaded.is_null() {
                evict_failed_module(name, module);
                if pon_err_occurred() {
                    return Err(format!("source module '{name}' returned NULL"));
                }
                return Err(format!("source module '{name}' returned NULL without setting an exception"));
            }
            if let Err(error) = finalize_source_module_identity_attrs(name, loaded, &spec) {
                evict_failed_module(name, module);
                return Err(error);
            }
            return adopt_post_body_sys_modules_replacement(name, module);
        }

        let module = load_curated_assignment_module(name, &source, spec.is_package, module_attrs)?;
        if let Err(error) = finalize_source_module_identity_attrs(name, module, &spec) {
            evict_failed_module(name, module);
            return Err(error);
        }
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
            | "_ssl"
            | "_sqlite3"
            | "_hashlib"
            | "_bz2"
            | "_lzma"
            | "_ctypes"
    )
}

struct SourceSpec {
    path: Option<PathBuf>,
    is_package: bool,
    search_locations: Option<Vec<PathBuf>>,
}

impl SourceSpec {
    fn module(path: PathBuf, is_package: bool) -> Self {
        let search_locations = is_package.then(|| {
            vec![path
                .parent()
                .expect("package __init__.py always has a parent directory")
                .to_path_buf()]
        });
        Self {
            path: Some(path),
            is_package,
            search_locations,
        }
    }

    fn namespace(search_locations: Vec<PathBuf>) -> Self {
        Self {
            path: None,
            is_package: true,
            search_locations: Some(search_locations),
        }
    }
}

fn find_source_module(name: &str) -> Option<SourceSpec> {
    if name.is_empty() {
        return None;
    }
    let mut relative = PathBuf::new();
    for part in name.split('.') {
        relative.push(part);
    }
    let mut namespace_portions = Vec::new();
    for root in search_roots() {
        let package_dir = root.join(&relative);
        let package_init = package_dir.join("__init__.py");
        if package_init.is_file() {
            return Some(SourceSpec::module(package_init, true));
        }
        let mut module_path = root.join(&relative);
        module_path.set_extension("py");
        if module_path.is_file() {
            return Some(SourceSpec::module(module_path, false));
        }
        if package_dir.is_dir() {
            namespace_portions.push(package_dir);
        }
    }
    (!namespace_portions.is_empty()).then(|| SourceSpec::namespace(namespace_portions))
}

fn find_extension_module(name: &str) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    let mut relative = PathBuf::new();
    for part in name.split('.') {
        relative.push(part);
    }
    for root in search_roots() {
        for suffix in crate::capi::extension_suffixes() {
            let mut path = root.join(&relative).into_os_string();
            path.push(suffix);
            let path = PathBuf::from(path);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

/// Environment override for the vendored-stdlib search root (HANDOFF J0.4).
/// When set it is authoritative: the value is used as the stdlib root if that
/// directory exists, and the built-in locations are not consulted.
pub const STDLIB_PATH_ENV_VAR: &str = "PON_STDLIB_PATH";

/// Workspace-relative location of the vendored CPython `Lib/` tree (L0 lands
/// the real vendoring; the directory currently holds a stub).
const VENDORED_STDLIB_SUFFIX: &str = "pon-conformance/vendor/cpython-3.14/Lib";

fn search_roots() -> Vec<PathBuf> {
    let defaults = default_search_roots();
    let mut roots = Vec::with_capacity(defaults.len());
    append_unique_roots(&mut roots, live_sys_path_extra_roots(&defaults));
    append_unique_roots(&mut roots, defaults);
    roots
}

fn default_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for var in ["PONPATH", "PON_IMPORT_PATH"] {
        if let Ok(extra) = env::var(var) {
            append_unique_roots(&mut roots, env::split_paths(&extra));
        }
    }
    if let Ok(cwd) = env::current_dir() {
        append_unique_root(&mut roots, cwd.clone());
        append_unique_root(&mut roots, cwd.join(".pon").join("packages").join("site-packages"));
        append_unique_root(&mut roots, cwd.join("pon-conformance").join("corpus"));
    }
    if let Some(stdlib) = vendored_stdlib_root() {
        append_unique_root(&mut roots, stdlib);
    }
    roots
}

fn live_sys_path_extra_roots(default_roots: &[PathBuf]) -> Vec<PathBuf> {
    let Some(path) = module_attr(intern("sys"), intern("path")) else {
        return Vec::new();
    };
    let Some(items) = sequence_items(path) else {
        return Vec::new();
    };

    let mut roots = Vec::new();
    for item in items {
        // SAFETY: The snapshot contains live list/tuple elements; non-exact
        // strings are ignored rather than dispatched through Python code while
        // resolving imports.
        let Some(text) = (unsafe { exact_str_text(item) }) else {
            continue;
        };
        if text.is_empty() {
            continue;
        }
        let root = PathBuf::from(text);
        if default_roots.contains(&root) {
            continue;
        }
        append_unique_root(&mut roots, root);
    }
    roots
}

fn append_unique_roots(roots: &mut Vec<PathBuf>, candidates: impl IntoIterator<Item = PathBuf>) {
    for root in candidates {
        append_unique_root(roots, root);
    }
}

fn append_unique_root(roots: &mut Vec<PathBuf>, root: PathBuf) {
    if !roots.contains(&root) {
        roots.push(root);
    }
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
/// live `sys.path` insertions, `PONPATH`/`PON_IMPORT_PATH` entries (the CLI
/// prepends the script directory), current directory, installed packages, the
/// conformance corpus, then the vendored stdlib last.
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

/// Package flag for a name pon's post-registry machinery would import:
/// source-recompiled extensions, embedded AoT bodies, then on-disk source roots
/// — exactly `resolve_module_by_name`'s order past the curated registry.
/// `None` means "not servable": curated-native and refused C-accelerated names
/// without a Pon extension file are excluded (`BuiltinImporter` already claims
/// the former through `_imp.is_builtin`; the latter must keep raising CPython's
/// `ModuleNotFoundError` when routed through `importlib`). Claim predicate of
/// the `_pon_source_importer` meta-path finder (`crate::native::imp`).
pub(crate) fn source_module_package_flag(name: &str) -> Option<bool> {
    if crate::native::is_native_module(name) {
        return None;
    }
    if find_extension_module(name).is_some() {
        return Some(false);
    }
    if is_unsupported_c_accelerated(name) {
        return None;
    }
    if let Some((is_package, _)) = embedded_module(name) {
        return Some(is_package);
    }
    find_source_module(name).map(|spec| spec.is_package)
}

pub(crate) fn source_module_search_locations(name: &str) -> Option<Vec<PathBuf>> {
    if crate::native::is_native_module(name) || find_extension_module(name).is_some() || is_unsupported_c_accelerated(name) {
        return None;
    }
    find_source_module(name).and_then(|spec| spec.search_locations)
}

/// Imports `name` and returns exactly that module — never the root-package
/// remap `pon_import_name` applies for empty fromlists — raising the same
/// typed import failure on error. Serves loader entry points
/// (`_pon_source_importer.create_module`) that must hand
/// `importlib._bootstrap._load` the named module itself, not its package
/// root.
pub(crate) fn import_named_module_raw(name: &str) -> *mut PyObject {
    match import_module_by_name(name) {
        Ok(module) => module,
        Err(message) => raise_import_error_text(&message),
    }
}

fn load_curated_assignment_module(
    name: &str,
    source: &str,
    is_package: bool,
    mut attrs: Vec<(u32, *mut PyObject)>,
) -> Result<*mut PyObject, String> {
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
        // Function-valued attrs at module-creation time exist only for native
        // (Rust-built) modules — user functions reach module namespaces via
        // `store_module_attr` while the compiled body executes, never through
        // this constructor.  Record them as CPython
        // `builtin_function_or_method` equivalents: non-descriptors that read
        // back bare off class attributes (see
        // `types::function::mark_native_function`).
        crate::types::function::mark_native_function(value);
        attr_map.insert(key, value);
    }

    let name_object = runtime_string(name)?;
    let package = module_package_name(name, is_package);
    let package_object = runtime_string(&package)?;
    attr_map.insert(intern("__name__"), name_object);
    attr_map.insert(intern("__package__"), package_object);
    // CPython binds `__doc__ = None` at module birth; the compiled body
    // rebinds it when a docstring executes.  pon codegen does not thread
    // docstrings, so the value stays None (accepted divergence) — but the
    // BINDING must exist: module-level `if __doc__ is not None:` (pdb.py)
    // is a NameError without it.  Native modules that pass an explicit
    // `__doc__` keep theirs.
    attr_map.entry(intern("__doc__")).or_insert_with(|| unsafe { pon_none() });

    let mut state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    let object = Box::new(PyModuleObject {
        ob_base: PyObjectHeader::new(state.module_type),
        name: name_id,
        registry_key: name_id,
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

/// Sequence source for unique synthetic-module registry identities.
static SYNTHETIC_MODULE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Synthetic modules created by calling the module type directly
/// (`types.ModuleType(name, doc=None)`), keyed by their unique
/// [`PyModuleObject::registry_key`].
///
/// These are NOT import-system modules: they never enter the import cache or
/// `sys.modules`, so `import name` never observes them and two same-named
/// instances stay distinct.  The unique key keeps the dynexec globals
/// registry (`module.__dict__` / `dir(module)` / attr-store mirroring) and
/// the GC root walk per-INSTANCE: a synthetic module named like a real one
/// (`types.ModuleType('os')`) must never read or pollute the real module's
/// namespace dict.  Objects are immortal leaked boxes exactly like
/// [`create_module`] products, stored as raw addresses (`usize`) so the
/// static is `Sync`, matching the dynexec `GLOBALS_REGISTRY` convention.
/// The mutex is held only for short non-reentrant sections while no Python
/// executes, and never while `IMPORT_STATE`'s lock is held
/// (deadlock-freedom mirrors the [`gc_held_roots`] contract).
static SYNTHETIC_MODULES: LazyLock<Mutex<HashMap<u32, usize>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// `type(sys)(name, doc=None)`: CPython `module.__new__` + `module.__init__`
/// fused into one construction pass, per this runtime's builtin-constructor
/// convention (`type_call` skips the `__init__` leg when `tp_new` is not
/// `type_new`).
///
/// Builds the real `PyModuleObject` layout with a live attrs map seeded like
/// CPython `module.__init__`: `__name__`, `__doc__`, and
/// `__package__`/`__loader__`/`__spec__` all `None`.
///
/// `cls` is honored in the object header so `type(m)` answers correctly, but
/// attr dispatch (`as_module`) recognizes only the exact module type:
/// `ModuleType` subclasses construct safely and raise on attr access instead
/// of hitting UB (subclass attr support is out of scope).
unsafe extern "C" fn module_tp_new(cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    if cls.is_null() {
        return return_null_with_error("cannot instantiate NULL module type");
    }
    let positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
        Ok(positional) => positional,
        Err(message) => return return_null_with_error(message),
    };
    if positional.len() > 2 {
        let message = format!("module() takes at most 2 arguments ({} given)", positional.len());
        return unsafe { pon_raise_type_error(message.as_ptr(), message.len()) };
    }
    let mut name_value = positional.first().copied().unwrap_or(ptr::null_mut());
    let mut doc_value = positional.get(1).copied().unwrap_or(ptr::null_mut());
    if !kwargs.is_null() {
        // `call_type_with_keywords` materializes keywords as a real dict.
        let entries = match unsafe { crate::types::dict::dict_entries_snapshot(kwargs) } {
            Ok(entries) => entries,
            Err(message) => return return_null_with_error(message),
        };
        for entry in entries {
            let (slot, position) = match unicode_text(crate::tag::untag_arg(entry.key)) {
                Some("name") => (&mut name_value, 1usize),
                Some("doc") => (&mut doc_value, 2usize),
                Some(other) => {
                    let message = format!("module() got an unexpected keyword argument '{other}'");
                    return unsafe { pon_raise_type_error(message.as_ptr(), message.len()) };
                }
                None => return return_null_with_error("module() keywords must be strings"),
            };
            if !slot.is_null() {
                let keyword = if position == 1 { "name" } else { "doc" };
                let message = format!("argument for module() given by name ('{keyword}') and position ({position})");
                return unsafe { pon_raise_type_error(message.as_ptr(), message.len()) };
            }
            *slot = entry.value;
        }
    }
    if name_value.is_null() {
        const MESSAGE: &str = "module() missing required argument 'name' (pos 1)";
        return unsafe { pon_raise_type_error(MESSAGE.as_ptr(), MESSAGE.len()) };
    }
    let name_object = crate::tag::untag_arg(name_value);
    let Some(name_text) = unicode_text(name_object) else {
        let kind = unsafe { crate::types::dict::type_name(name_object) }.unwrap_or("object");
        let message = format!("module() argument 'name' must be str, not {kind}");
        return unsafe { pon_raise_type_error(message.as_ptr(), message.len()) };
    };

    // CPython `module.__init__` namespace seed.
    let none = unsafe { pon_none() };
    let doc = if doc_value.is_null() { none } else { doc_value };
    let mut attrs = HashMap::new();
    attrs.insert(intern("__name__"), name_object);
    attrs.insert(intern("__doc__"), doc);
    attrs.insert(intern("__package__"), none);
    attrs.insert(intern("__loader__"), none);
    attrs.insert(intern("__spec__"), none);

    let sequence = SYNTHETIC_MODULE_SEQ.fetch_add(1, Ordering::Relaxed);
    // NUL prefix: importable module names cannot contain NUL, so the key
    // never collides with a real module's registry identity.
    let registry_key = intern(&format!("\0module-instance:{sequence}:{name_text}"));
    let object = Box::new(PyModuleObject {
        ob_base: PyObjectHeader::new(cls),
        name: intern(name_text),
        registry_key,
        attrs,
    });
    let object = as_object_ptr(Box::into_raw(object));
    let mut synthetic = SYNTHETIC_MODULES.lock().unwrap_or_else(|poison| poison.into_inner());
    synthetic.insert(registry_key, object as usize);
    object
}

fn runtime_string(value: &str) -> Result<*mut PyObject, String> {
    // SAFETY: `pon_const_str` returns NULL with a thread-state error on failure.
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    (!object.is_null()).then_some(object).ok_or_else(|| format!("failed to allocate string literal '{value}'"))
}

fn runtime_path_list(paths: &[PathBuf]) -> Result<*mut PyObject, String> {
    let mut items = Vec::with_capacity(paths.len());
    for path in paths {
        let text = path.to_string_lossy();
        items.push(runtime_string(&text)?);
    }
    let list = unsafe {
        crate::abi::seq::pon_build_list(
            if items.is_empty() {
                ptr::null_mut()
            } else {
                items.as_mut_ptr()
            },
            items.len(),
        )
    };
    (!list.is_null()).then_some(list).ok_or_else(|| "failed to allocate path list".to_owned())
}

fn source_module_attrs(spec: &SourceSpec) -> Result<Vec<(u32, *mut PyObject)>, String> {
    let mut attrs = Vec::with_capacity(2);
    if let Some(path) = spec.path.as_ref() {
        let file_path = std::path::absolute(path).unwrap_or_else(|_| path.clone());
        attrs.push((intern("__file__"), runtime_string(&file_path.to_string_lossy())?));
    } else {
        attrs.push((intern("__file__"), unsafe { pon_none() }));
    }
    if let Some(search_locations) = spec.search_locations.as_deref() {
        attrs.push((intern("__path__"), runtime_path_list(search_locations)?));
    }
    Ok(attrs)
}
/// Backfills `__loader__`/`__spec__` for concrete source modules once
/// `importlib._bootstrap` is itself live.  pon cannot ask the bootstrap to
/// seed these attrs before executing `importlib` and `importlib._bootstrap`
/// because those modules are the bootstrap; doing the `_spec_from_module`
/// pass immediately after body execution restores the CPython-visible surface
/// (`importlib.__spec__`, fresh-import helpers) without replaying the
/// namespace-package lane.
fn finalize_source_module_identity_attrs(
    name: &str,
    module: *mut PyObject,
    spec: &SourceSpec,
) -> Result<(), String> {
    if spec.path.is_none() {
        return Ok(());
    }
    let Some(module_ptr) = as_module(module) else {
        return Ok(());
    };
    let existing_loader = unsafe { (&*module_ptr).attrs.get(&intern("__loader__")).copied() };
    let needs_loader = existing_loader.is_none();
    let needs_spec = unsafe { !(&*module_ptr).attrs.contains_key(&intern("__spec__")) };
    if !needs_loader && !needs_spec {
        return Ok(());
    }
    let Some(spec_from_module) = module_attr(intern("importlib._bootstrap"), intern("_spec_from_module")) else {
        return Ok(());
    };
    let loader = match existing_loader {
        Some(loader) => loader,
        None => crate::native::imp::make_source_importer_module()?,
    };
    let mut argv = [module, loader];
    let spec_object = crate::tag::untag_arg(unsafe { crate::abi::pon_call(spec_from_module, argv.as_mut_ptr(), argv.len()) });
    if spec_object.is_null() {
        return Err(format!("failed to build __spec__ for source module '{name}'"));
    }
    let mut mutated = false;
    unsafe {
        let attrs = &mut (*module_ptr).attrs;
        if needs_loader {
            attrs.insert(intern("__loader__"), loader);
            mutated = true;
        }
        if needs_spec {
            attrs.insert(intern("__spec__"), spec_object);
            mutated = true;
        }
    }
    if mutated {
        crate::abi::bump_namespace_version();
    }
    Ok(())
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

fn adopt_post_body_sys_modules_replacement(name: &str, module: *mut PyObject) -> Result<*mut PyObject, String> {
    let Some(entry) = sys_modules_entry(name)? else {
        return Ok(module);
    };
    if entry == module || is_none_binding(entry) {
        return Ok(module);
    }
    let name_id = intern(name);
    let mut state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    state.modules.insert(name_id, entry);
    drop(state);
    bind_child_to_parent(name, entry);
    // J0.3 GlobalIC site: the module behind this name changed after body exec.
    crate::abi::bump_namespace_version();
    Ok(entry)
}

/// Unwinds the registration of a module whose body failed to execute.
///
/// CPython's `importlib._bootstrap._load` runs `del sys.modules[spec.name]`
/// when a module body raises, so a later import of the same name retries
/// from scratch (and re-raises) instead of observing the half-initialized
/// module. asyncio depends on exactly that: `base_events`' guarded
/// `import ssl` fails and is caught, and `sslproto`'s follow-up
/// `import ssl` must fail the same way — a cached corpse would flunk its
/// `if ssl is not None:` guard and read missing attributes.  The
/// parent-attr binding pon makes before execution (cycle support) is
/// unwound with the cache entry; extra names the failing body itself
/// published into `sys.modules` are kept, matching CPython.
fn evict_failed_module(name: &str, module: *mut PyObject) {
    let name_id = intern(name);
    let mut state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    state.modules.remove(&name_id);
    let dict = state.modules_dict;
    // Reverse of `bind_child_to_parent`, under the same lock; only a binding
    // still pointing at the failed module is removed.
    if let Some((parent_name, child_name)) = name.rsplit_once('.') {
        let parent_id = intern(parent_name);
        let child_id = intern(child_name);
        if let Some(parent) = state.modules.get(&parent_id).copied()
            && let Some(parent) = module_from_object_locked(&state, parent)
        {
            // SAFETY: The import state proved the object uses `PyModuleObject` layout.
            let attrs = unsafe { &mut (*parent).attrs };
            if attrs.get(&child_id).copied() == Some(module) {
                attrs.remove(&child_id);
            }
        }
    }
    drop(state);
    // Dict mutation takes its own critical section and must never nest
    // inside `IMPORT_STATE` (see `mirror_module_registration`).
    if !dict.is_null()
        && let Ok(key) = runtime_string(name)
    {
        let _guard = crate::sync::begin_critical_section(dict);
        // SAFETY: `dict` is an exact runtime dict; `key` is a live string.
        let _ = unsafe { crate::types::dict::dict_remove(dict, key) };
    }
    // J0.3 GlobalIC site: the name -> module binding disappeared.
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

/// Package context for a relative import, mirroring CPython's
/// calling-frame-globals rule (`_calc___package__`): the innermost executing
/// compiled function's DEFINING module wins — a function-scope
/// `from . import x` called long after its module finished importing still
/// resolves against that module — and a toplevel import statement falls back
/// to the actively executing module body.  Both locks are taken sequentially,
/// never nested: `current_defining_module` briefly takes `IMPORT_STATE`
/// itself (via `active_module_call_floor`).
fn current_importer_package() -> Option<String> {
    let module_id = crate::abi::current_defining_module().or_else(active_module_name_id)?;
    let package = {
        let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
        let module = state.modules.get(&module_id).copied()?;
        let module = module_from_object_locked(&state, module)?;
        // SAFETY: The import state proved the object uses `PyModuleObject` layout.
        unsafe { (&*module).attrs.get(&intern("__package__")).copied()? }
    };
    unicode_text(package).map(str::to_owned)
}

pub fn active_module_name_id() -> Option<u32> {
    let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    state.current_modules.last().copied()
}

pub fn module_attrs_snapshot(module_name: u32) -> Option<Vec<(u32, *mut PyObject)>> {
    {
        let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(module) = state.modules.get(&module_name).copied() {
            let module = module_from_object_locked(&state, module)?;
            // SAFETY: The import state proved the object uses `PyModuleObject` layout.
            let module = unsafe { &*module };
            return Some(module.attrs.iter().map(|(name, value)| (*name, *value)).collect());
        }
    }
    // Synthetic `types.ModuleType(...)` instances live outside the import
    // cache; resolve them by their unique registry key so namespace-dict
    // materialization (`__dict__`, `dir`) sees their attrs.
    let object = {
        let synthetic = SYNTHETIC_MODULES.lock().unwrap_or_else(|poison| poison.into_inner());
        synthetic.get(&module_name).copied()? as *mut PyObject
    };
    // SAFETY: Every synthetic-table entry was built by `module_tp_new` with
    // the `PyModuleObject` layout; attrs are read outside any lock exactly
    // like `module_getattro` reads them (Python execution is serialized).
    let module = unsafe { &*object.cast::<PyModuleObject>() };
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
    let mut roots = Vec::new();
    {
        let state = IMPORT_STATE.lock().unwrap_or_else(|poison| poison.into_inner());
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
    }
    // Synthetic `types.ModuleType(...)` modules: same immortal-box rationale —
    // marking cannot reach their attr values either.  Walked under the side
    // table's own lock AFTER the import-state section ends so the two
    // mutexes never nest.
    let synthetic = SYNTHETIC_MODULES.lock().unwrap_or_else(|poison| poison.into_inner());
    for &object in synthetic.values() {
        let object = object as *mut PyObject;
        // SAFETY: Every synthetic-table entry was built by `module_tp_new`
        // with the `PyModuleObject` layout.
        for (_, &value) in unsafe { (*object.cast::<PyModuleObject>()).attrs.iter() } {
            if !value.is_null() && crate::tag::is_heap(value) {
                roots.push(value);
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

    use super::{
        STDLIB_PATH_ENV_VAR, pon_import_from, pon_import_name, reset_import_state_for_tests, resolve_import_name,
        runtime_string, sys_modules_dict,
    };
    use crate::abi::pon_none;
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
        fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
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

    #[test]
    fn namespace_package_import_composes_roots() {
        let _guard = test_state_lock();
        let _reset = ResetImportStateOnDrop;
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
        }
        pon_err_clear();
        reset_import_state_for_tests();

        let root1 = TempImportRoot::new();
        let root2 = TempImportRoot::new();
        let pkg_name = format!(
            "pon_ns_pkg_{}_{}",
            process::id(),
            NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)
        );
        let ns1 = root1.path().join(&pkg_name);
        let ns2 = root2.path().join(&pkg_name);
        fs::create_dir_all(&ns1).unwrap();
        fs::create_dir_all(&ns2).unwrap();
        fs::write(ns1.join("alpha.py"), "VALUE = 'alpha-r1'\n").unwrap();
        fs::write(ns2.join("beta.py"), "VALUE = 'beta-r2'\n").unwrap();
        let import_path = env::join_paths([root1.path(), root2.path()]).unwrap();
        let _env = EnvVarGuard::set("PON_IMPORT_PATH", &import_path);

        let package = unsafe { pon_import_name(intern(&pkg_name), ptr::null(), 0, 0) };
        assert!(
            !package.is_null(),
            "importing namespace package failed: {:?}",
            pon_err_message()
        );
        let file = unsafe { pon_import_from(package, intern("__file__")) };
        assert_eq!(format_object_for_print(file).as_deref(), Ok("None"));
        let path = unsafe { pon_import_from(package, intern("__path__")) };
        let path_text = format_object_for_print(path).expect("namespace __path__ must format");
        assert!(
            path_text.contains(ns1.to_string_lossy().as_ref())
                && path_text.contains(ns2.to_string_lossy().as_ref()),
            "namespace path should include both roots, got {path_text}"
        );

        let alpha = unsafe { pon_import_from(package, intern("alpha")) };
        assert!(!alpha.is_null(), "importing namespace child alpha failed: {:?}", pon_err_message());
        let alpha_value = unsafe { pon_import_from(alpha, intern("VALUE")) };
        assert_eq!(format_object_for_print(alpha_value).as_deref(), Ok("alpha-r1"));

        let beta = unsafe { pon_import_from(package, intern("beta")) };
        assert!(!beta.is_null(), "importing namespace child beta failed: {:?}", pon_err_message());
        let beta_value = unsafe { pon_import_from(beta, intern("VALUE")) };
        assert_eq!(format_object_for_print(beta_value).as_deref(), Ok("beta-r2"));
    }
    #[test]
    fn source_package_import_sets_loader_and_spec() {
        let _guard = test_state_lock();
        let _reset = ResetImportStateOnDrop;
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
        }
        pon_err_clear();
        reset_import_state_for_tests();

        let module = unsafe { pon_import_name(intern("importlib"), ptr::null(), 0, 0) };
        assert!(!module.is_null(), "importing importlib failed: {:?}", pon_err_message());

        let loader = unsafe { pon_import_from(module, intern("__loader__")) };
        assert!(!loader.is_null(), "importlib.__loader__ missing: {:?}", pon_err_message());
        let loader_name = unsafe { crate::abi::object::pon_get_attr(loader, intern("__name__"), ptr::null_mut()) };
        assert!(!loader_name.is_null(), "importlib.__loader__.__name__ missing: {:?}", pon_err_message());
        assert_eq!(format_object_for_print(loader_name).as_deref(), Ok("_pon_source_importer"));

        let spec = unsafe { pon_import_from(module, intern("__spec__")) };
        assert!(!spec.is_null(), "importlib.__spec__ missing: {:?}", pon_err_message());
        let spec_name = unsafe { crate::abi::object::pon_get_attr(spec, intern("name"), ptr::null_mut()) };
        assert!(!spec_name.is_null(), "importlib.__spec__.name missing: {:?}", pon_err_message());
        assert_eq!(format_object_for_print(spec_name).as_deref(), Ok("importlib"));
        let spec_origin = unsafe { crate::abi::object::pon_get_attr(spec, intern("origin"), ptr::null_mut()) };
        assert!(!spec_origin.is_null(), "importlib.__spec__.origin missing: {:?}", pon_err_message());
        let origin_text = format_object_for_print(spec_origin).expect("importlib.__spec__.origin must format");
        assert!(
            origin_text.ends_with("/Lib/importlib/__init__.py"),
            "importlib.__spec__.origin should point at __init__.py, got {origin_text}"
        );
        let spec_loader = unsafe { crate::abi::object::pon_get_attr(spec, intern("loader"), ptr::null_mut()) };
        assert!(!spec_loader.is_null(), "importlib.__spec__.loader missing: {:?}", pon_err_message());
        assert_eq!(spec_loader, loader, "importlib.__spec__.loader should match importlib.__loader__");
        let search_locations =
            unsafe { crate::abi::object::pon_get_attr(spec, intern("submodule_search_locations"), ptr::null_mut()) };
        assert!(
            !search_locations.is_null(),
            "importlib.__spec__.submodule_search_locations missing: {:?}",
            pon_err_message()
        );
        let locations_text = format_object_for_print(search_locations)
            .expect("importlib.__spec__.submodule_search_locations must format");
        assert!(
            locations_text.contains("/Lib/importlib"),
            "importlib.__spec__.submodule_search_locations should include the package dir, got {locations_text}"
        );
    }

    #[test]
    fn none_sys_modules_binding_halts_import() {
        let _guard = test_state_lock();
        let _reset = ResetImportStateOnDrop;
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
        }
        pon_err_clear();
        reset_import_state_for_tests();

        // Plant the block `import_fresh_module(blocked=[...])` plants.
        let dict = sys_modules_dict().unwrap();
        let key = runtime_string("pon_tiny").unwrap();
        let none = unsafe { pon_none() };
        {
            let _guard = crate::sync::begin_critical_section(dict);
            unsafe { crate::types::dict::dict_insert(dict, key, none).unwrap() };
        }

        let blocked = unsafe { pon_import_name(intern("pon_tiny"), ptr::null(), 0, 0) };
        assert!(blocked.is_null(), "a None sys.modules binding must halt the import");
        let message = pon_err_message();
        assert!(
            message
                .as_deref()
                .is_some_and(|text| text.contains("import of pon_tiny halted; None in sys.modules")),
            "unexpected halt diagnostic: {message:?}"
        );
        pon_err_clear();

        // Deleting the block restores importability (fresh vendored load).
        {
            let _guard = crate::sync::begin_critical_section(dict);
            unsafe { crate::types::dict::dict_remove(dict, key).unwrap() };
        }
        let module = unsafe { pon_import_name(intern("pon_tiny"), ptr::null(), 0, 0) };
        assert!(!module.is_null(), "unblocked import failed: {:?}", pon_err_message());
        let name = unsafe { pon_import_from(module, intern("name")) };
        assert_eq!(format_object_for_print(name).as_deref(), Ok("tiny"));
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
    Some(crate::dynexec::module_namespace_dict(unsafe { (*module).registry_key }))
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
        return match crate::dynexec::module_namespace_dict(unsafe { (*module).registry_key }) {
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
    if let Some(value) = crate::dynexec::peek_module_namespace_value(module_ref.registry_key, name_text) {
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
    // SAFETY: `as_module` proved the layout.  The registry key routes the
    // namespace-dict mirror; the interned name serves error messages.
    let (module_name_id, module_registry_key) = unsafe { ((&*module).name, (&*module).registry_key) };
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
        crate::dynexec::sync_global_delete_for_module(module_registry_key, name_id);
    } else {
        // SAFETY: `as_module` proved the layout.
        unsafe {
            (&mut *module).attrs.insert(name_id, value);
        }
        crate::dynexec::sync_global_store_for_module(module_registry_key, name_id, value);
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
