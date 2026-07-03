//! Native `_imp` plus its importlib bootstrap companions (`_warnings`,
//! `marshal`).
//!
//! `Lib/importlib/__init__.py` cannot import without three C-only modules pon
//! never had: `_imp` itself (`_bootstrap._setup`), and `_warnings` + `marshal`
//! (module-top imports of `_bootstrap_external`).  All three factories live
//! here because they exist for exactly that bootstrap chain, as does the
//! fourth companion `_pon_source_importer` — pon's `PathFinder` stand-in that
//! `crate::import::seed_meta_path_finders` appends to `sys.meta_path` so
//! `importlib.import_module` can reach the embedded/source modules the
//! `import` statement already serves.
//!
//! `_imp` is an honest projection of pon's import machinery, not an emulation
//! of CPython's interpreter internals:
//!
//! - "builtin" means "curated native registry row" (`is_builtin` returns 1/0;
//!   CPython's -1 legacy-single-phase distinction is unobservable through
//!   `BuiltinImporter`'s truthiness check);
//! - `create_builtin(spec)` delegates `spec.name` to the normal import
//!   machinery — the registry row IS pon's builtin loader — and
//!   `exec_builtin` is a no-op returning 0 like CPython's for an already
//!   initialized module;
//! - pon freezes nothing: `is_frozen` is always False, `_frozen_module_names`
//!   is empty, and the frozen-object accessors raise CPython's exact
//!   `No such frozen object named '...'` ImportError;
//! - extension modules compiled against Pon's source C-API shim are loadable:
//!   `extension_suffixes()` advertises Pon-compatible suffixes and
//!   `create_dynamic` injects the C-API table before calling `PyInit_*`;
//! - `source_hash` is the real keyed SipHash-1-3 from CPython's
//!   `_Py_KeyedHash` (k0 = key, k1 = 0), byte-for-byte comparable with
//!   CPython pycs; `pyc_magic_number_token` carries the CPython 3.14 value;
//! - the import lock is a recursion counter: pon executes imports on one
//!   thread, but `lock_held`/`acquire_lock`/`release_lock` keep CPython's
//!   observable protocol (RuntimeError on over-release).
//!
//! `_warnings`: CPython 3.14 keeps all warnings state (filters, registries,
//! context) in `Lib/_py_warnings.py`; the C `_warnings` module accelerates
//! the same functions over that shared state.  pon's `_warnings` therefore
//! imports `_py_warnings` and re-exports its surface — the function objects
//! are identical, so `warnings`, `_py_warnings`, and `_warnings` observe one
//! coherent state.
//!
//! `marshal`: pon has no bytecode objects to serialize, so the module carries
//! the format `version` constant and four entry points that raise a typed
//! `NotImplementedError` naming the gap.  Import always succeeds; only actual
//! (de)serialization is refused, loudly.

use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicI64, Ordering};

use num_traits::ToPrimitive;

use crate::abi;
use crate::abi::exc::raise_kind_error_text;
use crate::intern::{intern, resolve};
use crate::object::{PyObject, PyUnicode};
use crate::thread_state::{pon_err_clear, pon_err_message};
use crate::types::exc::ExceptionKind;

use super::builtins_mod::VARIADIC_ARITY;
use super::install_module;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

/// CPython 3.14's `_imp.pyc_magic_number_token` (little-endian
/// `MAGIC_NUMBER` with the `\r\n` guard bytes in the high half).
const PYC_MAGIC_NUMBER_TOKEN: i64 = 168_627_755; // 0x0A0D_0E2B

/// Import-lock recursion depth (single import thread; protocol fidelity only).
static IMPORT_LOCK_DEPTH: AtomicI64 = AtomicI64::new(0);

// ---------------------------------------------------------------------------
// Shared small helpers

fn untag(object: *mut PyObject) -> *mut PyObject {
    crate::tag::untag_arg(object)
}

fn none() -> *mut PyObject {
    // SAFETY: Singleton accessor.
    unsafe { abi::pon_none() }
}

fn int_object(value: i64) -> *mut PyObject {
    untag(crate::types::int::from_i64(value))
}

fn bool_object(value: bool) -> *mut PyObject {
    // SAFETY: Bool constructor returns the shared singleton.
    unsafe { abi::number::pon_const_bool(i32::from(value)) }
}

fn str_object(text: &str) -> *mut PyObject {
    // SAFETY: `text` is a live UTF-8 slice; the runtime copies the bytes.
    unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn empty_list() -> *mut PyObject {
    // SAFETY: A zero-length window is valid for the list builder.
    unsafe { abi::seq::pon_build_list(ptr::null_mut(), 0) }
}

fn list_from_strings(values: &[&str]) -> *mut PyObject {
    let mut items = Vec::with_capacity(values.len());
    for value in values {
        let object = str_object(value);
        if object.is_null() {
            return ptr::null_mut();
        }
        items.push(object);
    }
    let ptr = if items.is_empty() { ptr::null_mut() } else { items.as_mut_ptr() };
    // SAFETY: `items` is live for the duration of the call; the runtime copies
    // the pointer slice into list storage.
    unsafe { abi::seq::pon_build_list(ptr, items.len()) }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    // SAFETY: Message bytes are a live UTF-8 slice for the duration of the call.
    unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

unsafe fn arg_window<'a>(argv: *mut *mut PyObject, argc: usize) -> &'a [*mut PyObject] {
    if argc == 0 || argv.is_null() {
        &[]
    } else {
        // SAFETY: The caller passed `argc` live argument slots.
        unsafe { std::slice::from_raw_parts(argv, argc) }
    }
}

/// Extracts the text of a `str` (or subclass) argument; `None` otherwise.
unsafe fn text_argument(object: *mut PyObject) -> Option<String> {
    if object.is_null() {
        return None;
    }
    let mut ty = unsafe { (*object).ob_type };
    while !ty.is_null() {
        if unsafe { (*ty).name() } == "str" {
            // SAFETY: A str (sub)type instance carries the PyUnicode layout.
            return unsafe { (*object.cast::<PyUnicode>()).as_str() }.map(ToOwned::to_owned);
        }
        ty = unsafe { (*ty).tp_base };
    }
    None
}

/// Untags and reads the single `str` argument shared by the name-taking
/// `_imp` entry points.
unsafe fn single_name_argument(
    argv: *mut *mut PyObject,
    argc: usize,
    function_name: &str,
) -> Result<String, *mut PyObject> {
    let args = unsafe { arg_window(argv, argc) };
    if args.is_empty() {
        return Err(raise_type_error(&format!(
            "{function_name}() takes exactly one argument (0 given)"
        )));
    }
    let value = untag(args[0]);
    match unsafe { text_argument(value) } {
        Some(text) => Ok(text),
        None => Err(raise_type_error(&format!("{function_name}() argument must be str"))),
    }
}

fn no_such_frozen_object(name: &str) -> *mut PyObject {
    raise_kind_error_text(
        ExceptionKind::ImportError,
        &format!("No such frozen object named '{name}'"),
    )
}

// ---------------------------------------------------------------------------
// Import lock (recursion counter)

unsafe extern "C" fn acquire_lock_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    IMPORT_LOCK_DEPTH.fetch_add(1, Ordering::SeqCst);
    none()
}

unsafe extern "C" fn release_lock_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    let previous = IMPORT_LOCK_DEPTH.fetch_sub(1, Ordering::SeqCst);
    if previous <= 0 {
        IMPORT_LOCK_DEPTH.fetch_add(1, Ordering::SeqCst);
        return raise_kind_error_text(ExceptionKind::RuntimeError, "not holding the import lock");
    }
    none()
}

unsafe extern "C" fn lock_held_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    bool_object(IMPORT_LOCK_DEPTH.load(Ordering::SeqCst) > 0)
}

// ---------------------------------------------------------------------------
// Builtin surface

unsafe extern "C" fn is_builtin_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let name = match unsafe { single_name_argument(argv, argc, "is_builtin") } {
        Ok(name) => name,
        Err(raised) => return raised,
    };
    int_object(i64::from(crate::native::is_native_module(&name)))
}

/// `create_builtin(spec)`: loads `spec.name` through pon's import machinery
/// (the curated native registry is pon's builtin loader).
unsafe extern "C" fn create_builtin_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = unsafe { arg_window(argv, argc) };
    if args.is_empty() {
        return raise_type_error("create_builtin() takes exactly one argument (0 given)");
    }
    let spec = untag(args[0]);
    // SAFETY: Generic attribute read on a live object; NULL propagates below.
    let name_object = unsafe { abi::object::pon_get_attr(spec, intern("name"), ptr::null_mut()) };
    if name_object.is_null() {
        return ptr::null_mut();
    }
    let Some(name) = (unsafe { text_argument(untag(name_object)) }) else {
        return raise_type_error("spec.name must be a str");
    };
    // SAFETY: Absolute import of a dotted-name module; NULL-with-error contract.
    unsafe { crate::import::pon_import_name(intern(&name), ptr::null(), 0, 0) }
}

unsafe extern "C" fn exec_builtin_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    // Native factories execute eagerly at creation; CPython returns 0 for an
    // already initialized module.
    int_object(0)
}

// ---------------------------------------------------------------------------
// Frozen surface (pon freezes nothing)

unsafe extern "C" fn is_frozen_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if let Err(raised) = unsafe { single_name_argument(argv, argc, "is_frozen") } {
        return raised;
    }
    bool_object(false)
}

unsafe extern "C" fn find_frozen_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if let Err(raised) = unsafe { single_name_argument(argv, argc, "find_frozen") } {
        return raised;
    }
    none()
}

unsafe extern "C" fn is_frozen_package_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    match unsafe { single_name_argument(argv, argc, "is_frozen_package") } {
        Ok(name) => no_such_frozen_object(&name),
        Err(raised) => raised,
    }
}

unsafe extern "C" fn get_frozen_object_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    match unsafe { single_name_argument(argv, argc, "get_frozen_object") } {
        Ok(name) => no_such_frozen_object(&name),
        Err(raised) => raised,
    }
}

unsafe extern "C" fn init_frozen_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if let Err(raised) = unsafe { single_name_argument(argv, argc, "init_frozen") } {
        return raised;
    }
    // CPython returns None when the name is not frozen; nothing is.
    none()
}

unsafe extern "C" fn frozen_module_names_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    empty_list()
}

unsafe extern "C" fn override_frozen_modules_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    // Accepted and ignored: there are no frozen modules to override.
    none()
}

unsafe extern "C" fn override_multi_interp_check_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    // CPython raises this in the main interpreter; pon has exactly one.
    raise_kind_error_text(
        ExceptionKind::RuntimeError,
        "_imp._override_multi_interp_extensions_check() cannot be used in the main interpreter",
    )
}

// ---------------------------------------------------------------------------
// Extension-module surface for source-recompiled Pon C-API modules.

unsafe extern "C" fn extension_suffixes_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    list_from_strings(crate::capi::extension_suffixes())
}

unsafe extern "C" fn create_dynamic_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = unsafe { arg_window(argv, argc) };
    let Some(&spec) = args.first() else {
        return raise_type_error("create_dynamic() takes exactly one argument (0 given)");
    };
    let spec = untag(spec);
    let name_object = unsafe { abi::object::pon_get_attr(spec, intern("name"), ptr::null_mut()) };
    if name_object.is_null() {
        return ptr::null_mut();
    }
    let Some(name) = (unsafe { text_argument(untag(name_object)) }) else {
        return raise_type_error("spec.name must be a str");
    };
    let origin_object = unsafe { abi::object::pon_get_attr(spec, intern("origin"), ptr::null_mut()) };
    if origin_object.is_null() {
        return ptr::null_mut();
    }
    let Some(origin) = (unsafe { text_argument(untag(origin_object)) }) else {
        return raise_type_error("spec.origin must be a str");
    };
    match crate::capi::load_extension_module(&name, &PathBuf::from(origin)) {
        Ok(module) => module,
        Err(message) => raise_kind_error_text(ExceptionKind::ImportError, &message),
    }
}

unsafe extern "C" fn exec_dynamic_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    // CPython returns 0 for objects without an exec slot.
    int_object(0)
}

// ---------------------------------------------------------------------------
// pyc helpers

unsafe extern "C" fn fix_co_filename_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    // pon code objects carry no marshaled filename to patch.
    none()
}

fn to_i64(object: *mut PyObject) -> Option<i64> {
    if object.is_null() {
        return None;
    }
    // SAFETY: Heap-or-NULL after untagging; NULL was rejected.
    unsafe { crate::types::int::to_bigint_including_bool(object) }.and_then(|value| value.to_i64())
}

unsafe fn bytes_argument<'a>(object: *mut PyObject) -> Option<&'a [u8]> {
    if object.is_null() {
        return None;
    }
    let ty = unsafe { (*object).ob_type };
    if crate::types::bytes_::is_bytes_type(ty) {
        // SAFETY: Exact bytes carry the PyBytes layout.
        return Some(unsafe { (*object.cast::<crate::types::bytes_::PyBytes>()).as_slice() });
    }
    None
}

/// `source_hash(key, source)` -> 8 little-endian bytes of the keyed hash.
unsafe extern "C" fn source_hash_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = unsafe { arg_window(argv, argc) };
    if args.len() != 2 {
        return raise_type_error(&format!(
            "source_hash() takes exactly 2 arguments ({} given)",
            args.len()
        ));
    }
    let Some(key) = to_i64(untag(args[0])) else {
        return raise_type_error("source_hash() key must be an int");
    };
    let Some(source) = (unsafe { bytes_argument(untag(args[1])) }) else {
        return raise_type_error("source_hash() source must be bytes");
    };
    let digest = crate::pyhash::siphash13(key as u64, 0, source).to_le_bytes();
    // SAFETY: `digest` is a live 8-byte buffer; the runtime copies it.
    unsafe { abi::str_::pon_const_bytes(digest.as_ptr(), digest.len()) }
}

// ---------------------------------------------------------------------------
// Module factories

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "_imp";
    let name_object = str_object(name);
    if name_object.is_null() {
        return Err("failed to allocate _imp.__name__".to_owned());
    }
    let check_hash_based_pycs = str_object("default");
    if check_hash_based_pycs.is_null() {
        return Err("failed to allocate _imp.check_hash_based_pycs".to_owned());
    }
    let magic_token = int_object(PYC_MAGIC_NUMBER_TOKEN);
    if magic_token.is_null() {
        return Err("failed to allocate _imp.pyc_magic_number_token".to_owned());
    }
    let mut attrs = vec![
        (intern("__name__"), name_object),
        (intern("check_hash_based_pycs"), check_hash_based_pycs),
        (intern("pyc_magic_number_token"), magic_token),
    ];
    let functions: [(&str, BuiltinFn); 19] = [
        ("_fix_co_filename", fix_co_filename_entry),
        ("_frozen_module_names", frozen_module_names_entry),
        ("_override_frozen_modules_for_tests", override_frozen_modules_entry),
        ("_override_multi_interp_extensions_check", override_multi_interp_check_entry),
        ("acquire_lock", acquire_lock_entry),
        ("create_builtin", create_builtin_entry),
        ("create_dynamic", create_dynamic_entry),
        ("exec_builtin", exec_builtin_entry),
        ("exec_dynamic", exec_dynamic_entry),
        ("extension_suffixes", extension_suffixes_entry),
        ("find_frozen", find_frozen_entry),
        ("get_frozen_object", get_frozen_object_entry),
        ("init_frozen", init_frozen_entry),
        ("is_builtin", is_builtin_entry),
        ("is_frozen", is_frozen_entry),
        ("is_frozen_package", is_frozen_package_entry),
        ("lock_held", lock_held_entry),
        ("release_lock", release_lock_entry),
        ("source_hash", source_hash_entry),
    ];
    for (function_name, entry) in functions {
        // SAFETY: `entry` is a live builtin entry point.
        let function =
            unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(function_name)) };
        if function.is_null() {
            return Err(format!("failed to allocate _imp.{function_name}"));
        }
        attrs.push((intern(function_name), function));
    }
    install_module(name, attrs)
}

/// Attribute names owned by the module object itself, not re-exported when
/// mirroring `_py_warnings` under the `_warnings` name.
const MODULE_IDENTITY_ATTRS: [&str; 6] =
    ["__name__", "__package__", "__spec__", "__loader__", "__file__", "__builtins__"];

/// `_warnings`: re-export of the pure-Python warnings state module (see the
/// module docs above for why this is the CPython 3.14 shape).
pub(super) fn make_warnings_module() -> Result<*mut PyObject, String> {
    let py_warnings_id = intern("_py_warnings");
    // SAFETY: Absolute import through the normal machinery; NULL-with-error.
    let py_warnings = unsafe { crate::import::pon_import_name(py_warnings_id, ptr::null(), 0, 0) };
    if py_warnings.is_null() {
        let message = pon_err_message().unwrap_or_else(|| "unknown import error".to_owned());
        pon_err_clear();
        return Err(format!("_warnings: failed to import _py_warnings: {message}"));
    }
    let snapshot = crate::import::module_attrs_snapshot(py_warnings_id)
        .ok_or_else(|| "_warnings: _py_warnings has no attribute snapshot".to_owned())?;
    let attrs: Vec<(u32, *mut PyObject)> = snapshot
        .into_iter()
        .filter(|&(key, _)| {
            resolve(key).is_none_or(|name| !MODULE_IDENTITY_ATTRS.contains(&name.as_str()))
        })
        .collect();
    install_module("_warnings", attrs)
}

fn marshal_not_implemented(operation: &str) -> *mut PyObject {
    raise_kind_error_text(
        ExceptionKind::NotImplementedError,
        &format!("marshal.{operation} is not supported: pon has no marshaled code-object format"),
    )
}

unsafe extern "C" fn marshal_dump_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    marshal_not_implemented("dump")
}

unsafe extern "C" fn marshal_dumps_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    marshal_not_implemented("dumps")
}

unsafe extern "C" fn marshal_load_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    marshal_not_implemented("load")
}

unsafe extern "C" fn marshal_loads_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    marshal_not_implemented("loads")
}

/// `marshal`: format version constant plus loudly-typed refusals (see module
/// docs; import succeeds, serialization does not).
pub(super) fn make_marshal_module() -> Result<*mut PyObject, String> {
    let name = "marshal";
    let name_object = str_object(name);
    if name_object.is_null() {
        return Err("failed to allocate marshal.__name__".to_owned());
    }
    let version = int_object(5);
    if version.is_null() {
        return Err("failed to allocate marshal.version".to_owned());
    }
    let mut attrs = vec![(intern("__name__"), name_object), (intern("version"), version)];
    for (function_name, entry) in [
        ("dump", marshal_dump_entry as BuiltinFn),
        ("dumps", marshal_dumps_entry),
        ("load", marshal_load_entry),
        ("loads", marshal_loads_entry),
    ] {
        // SAFETY: `entry` is a live builtin entry point.
        let function =
            unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(function_name)) };
        if function.is_null() {
            return Err(format!("failed to allocate marshal.{function_name}"));
        }
        attrs.push((intern(function_name), function));
    }
    install_module(name, attrs)
}

// ---------------------------------------------------------------------------
// `_pon_source_importer`: pon's PathFinder stand-in (meta_path slot three)

/// `sys.modules` name of pon's source-root finder/loader companion.
const SOURCE_IMPORTER_NAME: &str = "_pon_source_importer";

fn source_importer_module() -> Option<*mut PyObject> {
    crate::import::cached_module(intern(SOURCE_IMPORTER_NAME))
}

/// `find_spec(fullname, path=None, target=None)`: claims exactly the names
/// pon's post-registry machinery (embedded AoT bodies, then source roots)
/// would serve, building the spec through the vendored
/// `importlib._bootstrap.spec_from_loader` so `ModuleSpec` semantics
/// (`is_package` via `loader.is_package`, `parent`, `_initializing`) are the
/// bootstrap's own.  `path` and `target` are accepted and ignored: pon
/// resolves by full dotted name, not per-package search locations.
unsafe extern "C" fn source_find_spec_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = unsafe { arg_window(argv, argc) };
    if args.is_empty() {
        return raise_type_error("find_spec() takes at least 1 argument (0 given)");
    }
    let name_object = untag(args[0]);
    let Some(name) = (unsafe { text_argument(name_object) }) else {
        return raise_type_error("find_spec() argument 'fullname' must be str");
    };
    if crate::import::source_module_package_flag(&name).is_none() {
        return none();
    }
    // A missing loader module or bootstrap binding declines the claim rather
    // than failing the import: the caller then raises its own
    // ModuleNotFoundError, matching the pre-finder surface.
    let Some(loader) = source_importer_module() else {
        return none();
    };
    let Some(spec_from_loader) =
        crate::import::module_attr(intern("importlib._bootstrap"), intern("spec_from_loader"))
    else {
        return none();
    };
    let mut call_args = [name_object, loader];
    // SAFETY: `spec_from_loader` is a live callable; argv holds two live slots.
    let spec = untag(unsafe { abi::pon_call(spec_from_loader, call_args.as_mut_ptr(), call_args.len()) });
    if spec.is_null() {
        return ptr::null_mut();
    }
    if let Some(search_locations) = crate::import::source_module_search_locations(&name) {
        // SAFETY: `ModuleSpec.submodule_search_locations` is the list
        // `spec_from_loader` created for package specs.
        let locations = unsafe { abi::pon_get_attr(spec, intern("submodule_search_locations"), ptr::null_mut()) };
        if locations.is_null() {
            return ptr::null_mut();
        }
        let locations = untag(locations);
        for path in search_locations {
            let text = path.to_string_lossy();
            let path_object = str_object(&text);
            if path_object.is_null() {
                return ptr::null_mut();
            }
            if let Err(message) = crate::abi::seq::list_append_raw(locations, path_object) {
                return crate::abi::return_null_with_error(message);
            }
        }
    }
    spec
}

/// `is_package(fullname)`: consulted by `spec_from_loader` while `find_spec`
/// builds a claimed spec; ImportError for unclaimed names mirrors the
/// `_requires_builtin` protocol (`spec_from_loader` catches it as
/// "undefined").
unsafe extern "C" fn source_is_package_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let name = match unsafe { single_name_argument(argv, argc, "is_package") } {
        Ok(name) => name,
        Err(error) => return error,
    };
    match crate::import::source_module_package_flag(&name) {
        Some(flag) => bool_object(flag),
        None => raise_kind_error_text(
            ExceptionKind::ImportError,
            &format!("{name} is not a pon source module"),
        ),
    }
}

/// `create_module(spec)`: loads `spec.name` through pon's import machinery
/// and returns exactly the named module (never the root-package remap), fully
/// executed — mirroring `create_builtin`, whose native factories also run
/// eagerly at creation time.
unsafe extern "C" fn source_create_module_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = unsafe { arg_window(argv, argc) };
    if args.is_empty() {
        return raise_type_error("create_module() takes exactly one argument (0 given)");
    }
    let spec = untag(args[0]);
    // SAFETY: Generic attribute read on a live object; NULL propagates below.
    let name_object = unsafe { abi::object::pon_get_attr(spec, intern("name"), ptr::null_mut()) };
    if name_object.is_null() {
        return ptr::null_mut();
    }
    let Some(name) = (unsafe { text_argument(untag(name_object)) }) else {
        return raise_type_error("spec.name must be a str");
    };
    crate::import::import_named_module_raw(&name)
}

/// `exec_module(module)`: no-op — `create_module` already executed the body
/// (the `exec_builtin` shape, returning None per the loader protocol).
unsafe extern "C" fn source_exec_module_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    none()
}

/// Builds and registers the `_pon_source_importer` module: pon's stand-in for
/// CPython's `PathFinder` meta-path slot (`crate::import`'s
/// `seed_meta_path_finders` appends it third).  The module object doubles as
/// finder and loader — module-attr lookup returns unbound native functions,
/// so `finder.find_spec(...)` and `spec.loader.create_module(...)` call
/// cleanly — and registration roots it (and its function attrs) for GC like
/// any other native module.
pub(crate) fn make_source_importer_module() -> Result<*mut PyObject, String> {
    if let Some(existing) = source_importer_module() {
        return Ok(existing);
    }
    let mut attrs = Vec::new();
    let functions: [(&str, BuiltinFn); 4] = [
        ("create_module", source_create_module_entry),
        ("exec_module", source_exec_module_entry),
        ("find_spec", source_find_spec_entry),
        ("is_package", source_is_package_entry),
    ];
    for (function_name, entry) in functions {
        // SAFETY: `entry` is a live builtin entry point.
        let function =
            unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(function_name)) };
        if function.is_null() {
            return Err(format!("failed to allocate {SOURCE_IMPORTER_NAME}.{function_name}"));
        }
        attrs.push((intern(function_name), function));
    }
    install_module(SOURCE_IMPORTER_NAME, attrs)
}
