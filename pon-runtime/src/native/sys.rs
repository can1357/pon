//! Native `sys` module seed for WS-IMPORT.

use std::sync::atomic::{AtomicUsize, Ordering};

use num_traits::ToPrimitive;

use crate::abi::{pon_const_bool, pon_const_int, pon_const_str, pon_make_function, return_null_with_error};
use crate::intern::intern;
use crate::object::PyObject;

use super::install_module;

/// CPython `sys.platform` (PEP 11 build-time names): `"darwin"` on macOS and
/// `"win32"` on Windows, where Rust's `std::env::consts::OS` (`"macos"`,
/// `"windows"`) diverges; elsewhere the Rust name already matches (`"linux"`,
/// `"freebsd"`, …).  Vendored stdlib branches on the CPython spelling
/// (`platform.py`, `test.support`, `posixpath` selection).
const PLATFORM: &str = if cfg!(target_os = "macos") {
    "darwin"
} else if cfg!(target_os = "windows") {
    "win32"
} else {
    std::env::consts::OS
};

// ---------------------------------------------------------------------------
// Interpreter version pin
//
// HOST-ORACLE COUPLING: the differential suites compare pon's stdout against
// the host `python3.14` byte-for-byte, and `corpus/import_sys_version.py`
// prints the full `sys.version_info` repr, so these constants must equal the
// HOST oracle's values — currently CPython 3.14.6 — not the vendored stdlib
// tag (`v3.14.0`, `pon-conformance/vendor/cpython-3.14/REVISION`), which only
// pins the `Lib/` sources.  Re-pinning after a host `python3.14` upgrade is
// this one block; `sys.version`, `sys.hexversion`, and `sys.implementation`
// derive from it below.
// ---------------------------------------------------------------------------

const VERSION_INFO_MAJOR: i64 = 3;
const VERSION_INFO_MINOR: i64 = 14;
const VERSION_INFO_MICRO: i64 = 6;
const VERSION_INFO_RELEASELEVEL: &str = "final";
const VERSION_INFO_SERIAL: i64 = 0;

/// CPython `PY_VERSION_HEX` (Include/patchlevel.h): `0xMMmmppRS` with the
/// release-level nibble `0xF` for 'final' and the serial in the low nibble.
const HEXVERSION: i64 = (VERSION_INFO_MAJOR << 24)
    | (VERSION_INFO_MINOR << 16)
    | (VERSION_INFO_MICRO << 8)
    | (0xF << 4)
    | VERSION_INFO_SERIAL;

/// The CPython structseq repr shared by the `version_info.__repr__` slot and
/// the `sys.implementation` seed text.
fn format_version_info(major: i64, minor: i64, micro: i64, releaselevel: &str, serial: i64) -> String {
    format!(
        "sys.version_info(major={major}, minor={minor}, micro={micro}, releaselevel='{releaselevel}', serial={serial})"
    )
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let attrs = [
        string_attr(
            "version",
            &format!("{VERSION_INFO_MAJOR}.{VERSION_INFO_MINOR}.{VERSION_INFO_MICRO} (pon)"),
        ),
        version_info_attr(),
        implementation_attr(),
        int_attr("hexversion", HEXVERSION),
        int_attr("maxsize", i64::MAX),
        string_attr("platform", PLATFORM),
        string_attr("byteorder", if cfg!(target_endian = "little") { "little" } else { "big" }),
        string_attr("executable", "pon"),
        string_attr("prefix", ""),
        string_attr("base_prefix", ""),
        string_attr("exec_prefix", ""),
        string_attr("base_exec_prefix", ""),
        // POSIX default-build ABI flags ('' since 3.8's pymalloc-flag drop);
        // `test.support` reads it at import.
        string_attr("abiflags", ""),
        // Platform library directory name; "lib" on every CPython POSIX
        // default build (configure can override to lib64, pon never does).
        // `sysconfig._init_config_vars` reads it while populating config
        // vars (`_CONFIG_VARS['platlibdir'] = sys.platlibdir`).
        string_attr("platlibdir", "lib"),
        flags_attr(),
        hash_info_attr(),
        jit_attr(),
        warnoptions_attr(),
        meta_path_attr(),
        modules_attr(),
        builtin_module_names_attr(),
        function_attr("_getframe", sys_getframe),
        function_attr("intern", sys_intern),
        function_attr("exception", sys_exception),
        function_attr("exc_info", sys_exc_info),
        function_attr("excepthook", sys_excepthook),
        function_attr("getrecursionlimit", sys_getrecursionlimit),
        function_attr("setrecursionlimit", sys_setrecursionlimit),
        std_stream_attr("stdout", 1),
        std_stream_attr("stderr", 2),
    ];
    let mut attrs = attrs.into_iter().collect::<Result<Vec<_>, _>>()?;
    if cfg!(target_os = "macos") {
        // CPython macOS framework builds expose `sys._framework` ("Python");
        // the vendored sysconfig reads it at import inside its darwin branch
        // (`sysconfig/__init__.py:296`), and the local python3.14 oracle the
        // differential suites compare against reports 'Python'.
        attrs.push(string_attr("_framework", "Python")?);
    }
    install_module("sys", attrs)
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate sys.{name}"))
}

/// `sys.stdout`/`sys.stderr`: writable text streams over the process fds
/// (see `io::std_stream_object`).  `print()` writes host stdout directly and
/// does not consult `sys.stdout`; these objects serve explicit stream users
/// (unittest's TextTestRunner writes to `sys.stderr`).
fn std_stream_attr(name: &str, fd: i32) -> Result<(u32, *mut PyObject), String> {
    let object = super::io::std_stream_object(fd, &format!("<{name}>"));
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate sys.{name}"))
}

/// `sys.modules` is the live import-registry dict owned by `crate::import`;
/// mutations through it (insert/replace/delete) steer later imports.
fn modules_attr() -> Result<(u32, *mut PyObject), String> {
    crate::import::sys_modules_dict().map(|object| (intern("modules"), object))
}

/// `sys.builtin_module_names`: sorted tuple of the curated native module
/// names — pon's builtins.  Consumed by `importlib._bootstrap._setup` to
/// stamp `BuiltinImporter` specs onto already-imported native modules and by
/// `_imp.is_builtin` callers; dotted registry aliases (`os.path`) are not
/// module names and are excluded.
fn builtin_module_names_attr() -> Result<(u32, *mut PyObject), String> {
    let mut names: Vec<&str> = super::NATIVE_MODULES
        .iter()
        .map(|&(name, _)| name)
        .filter(|name| !name.contains('.'))
        .collect();
    names.sort_unstable();
    let mut items = Vec::with_capacity(names.len());
    for name in names {
        // SAFETY: Allocation helper; NULL is checked immediately.
        let object = unsafe { pon_const_str(name.as_ptr(), name.len()) };
        if object.is_null() {
            return Err(format!("failed to allocate sys.builtin_module_names entry '{name}'"));
        }
        items.push(object);
    }
    // SAFETY: `items` is a live window for the duration of the call.
    let tuple = unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) };
    (!tuple.is_null())
        .then_some((intern("builtin_module_names"), tuple))
        .ok_or_else(|| "failed to allocate sys.builtin_module_names".to_owned())
}

/// `sys.warnoptions`: no `-W` options reach an embedded interpreter, so the
/// list `warnings._processoptions` consumes at import is always empty.
fn warnoptions_attr() -> Result<(u32, *mut PyObject), String> {
    empty_list_attr("warnoptions")
}

/// `sys.meta_path`: the import-protocol finder chain, born empty (CPython's
/// pre-`_install` state).  pon's native importer resolves `import`
/// statements itself and never consults this list (documented divergence);
/// the list serves the vendored `importlib._bootstrap`, whose `_find_spec`
/// reads it whenever stdlib code routes an import through
/// `importlib.import_module` (e.g. `sysconfig._get_sysconfigdata`).  CPython
/// seeds `[BuiltinImporter, FrozenImporter]` via `_bootstrap._install` at
/// interpreter init; the vendored `importlib/__init__.py` source-fallback
/// branch only runs `_setup`, so `crate::import::seed_meta_path_finders`
/// mirrors the `_install` append right after `importlib._bootstrap` first
/// loads.  `PathFinder` (CPython's third entry, installed by
/// `_install_external_importers`) is deliberately absent: it lives in
/// `_bootstrap_external`, whose file-loading machinery pon does not run.
fn meta_path_attr() -> Result<(u32, *mut PyObject), String> {
    empty_list_attr("meta_path")
}

/// A freshly-allocated empty runtime list bound as `sys.<name>`.
fn empty_list_attr(name: &str) -> Result<(u32, *mut PyObject), String> {
    let object = super::builtins_mod::alloc_list(Vec::new());
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate sys.{name}"))
}

/// `sys.flags` singleton exposing the interpreter flags the vendored stdlib
/// reads at import time.  Only the consumed subset is served — an unknown
/// attribute raises `AttributeError` so the next frontier is loud, not a
/// silently wrong default.  `context_aware_warnings` is 0 to match the
/// reference CPython default (GIL) build `_py_warnings` is compared against.
fn flags_attr() -> Result<(u32, *mut PyObject), String> {
    static FLAGS_TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        let mut ty = crate::object::PyType::new(
            std::ptr::null(),
            "sys.flags",
            std::mem::size_of::<crate::object::PyObjectHeader>(),
        );
        ty.tp_getattro = Some(flags_getattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    static FLAGS: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        Box::into_raw(Box::new(crate::object::PyObjectHeader::new(*FLAGS_TYPE as *mut crate::object::PyType)))
            as usize
    });
    Ok((intern("flags"), *FLAGS as *mut PyObject))
}

unsafe extern "C" fn flags_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let name = crate::tag::untag_arg(name);
    let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        crate::thread_state::pon_err_set("attribute name must be str");
        return std::ptr::null_mut();
    };
    match name_text {
        // Plain `python3` invocation values (no -X/-O/-W switches reach pon).
        // `thread_inherit_context` is 0 to match the reference CPython
        // default (GIL) build: threads start with an empty context.
        "context_aware_warnings" | "debug" | "optimize" | "verbose" | "dev_mode" | "ignore_environment"
        | "thread_inherit_context" => unsafe { pon_const_int(0) },
        // SAFETY: Raise helper with the interned attribute name.
        _ => unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
    }
}

/// `sys.hash_info` singleton. The numeric fields are the CPython 64-bit
/// values pon's int/float hashing already implements (Mersenne prime
/// `2**61 - 1` modulus scheme); `fractions` reads `.modulus`/`.inf` at
/// import. Unknown attributes raise so the next frontier is loud.
fn hash_info_attr() -> Result<(u32, *mut PyObject), String> {
    static HASH_INFO_TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        let mut ty = crate::object::PyType::new(
            std::ptr::null(),
            "sys.hash_info",
            std::mem::size_of::<crate::object::PyObjectHeader>(),
        );
        ty.tp_getattro = Some(hash_info_getattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    static HASH_INFO: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        Box::into_raw(Box::new(crate::object::PyObjectHeader::new(*HASH_INFO_TYPE as *mut crate::object::PyType)))
            as usize
    });
    Ok((intern("hash_info"), *HASH_INFO as *mut PyObject))
}

unsafe extern "C" fn hash_info_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let name = crate::tag::untag_arg(name);
    let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        crate::thread_state::pon_err_set("attribute name must be str");
        return std::ptr::null_mut();
    };
    let int_value = |value: i64| unsafe { pon_const_int(value) };
    match name_text {
        "width" | "hash_bits" => int_value(64),
        "modulus" => int_value((1 << 61) - 1),
        "inf" => int_value(314_159),
        "nan" => int_value(0),
        "imag" => int_value(1_000_003),
        "seed_bits" => int_value(128),
        "cutoff" => int_value(0),
        // The declared algorithm for str/bytes hashing; pon's numeric hashes
        // match CPython's scheme, which is all the stdlib consumes here.
        "algorithm" => unsafe { pon_const_str("siphash13".as_ptr(), "siphash13".len()) },
        // SAFETY: Raise helper with the interned attribute name.
        _ => unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
    }
}

/// `sys._jit` singleton (the flags/hash_info pattern: an opaque leaked
/// object whose getattro serves the consumed method set).  CPython 3.14
/// ships it as a real module introspecting CPython's OWN experimental
/// tier-2/uop JIT; `test.support` calls `is_enabled()` at import to gate
/// CPython-JIT-specific tests (`requires_jit_enabled` /
/// `requires_jit_disabled`).  pon has a JIT (tier-up), but it is not the
/// JIT those tests probe, so `is_enabled()` honestly answers `False` and
/// routes them to skip — the same answer the host oracle (a non-JIT
/// CPython build) gives, keeping the differential suites stable.  The
/// wider CPython surface (`is_available`, `is_active`) is deliberately
/// unserved until a walk consumes it (`test.libregrtest.utils`,
/// `test_sys`): unknown attributes raise so that frontier is loud.
fn jit_attr() -> Result<(u32, *mut PyObject), String> {
    static JIT_TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        let mut ty = crate::object::PyType::new(
            std::ptr::null(),
            "sys._jit",
            std::mem::size_of::<crate::object::PyObjectHeader>(),
        );
        ty.tp_getattro = Some(jit_getattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    static JIT: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        Box::into_raw(Box::new(crate::object::PyObjectHeader::new(*JIT_TYPE as *mut crate::object::PyType)))
            as usize
    });
    Ok((intern("_jit"), *JIT as *mut PyObject))
}

unsafe extern "C" fn jit_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let name = crate::tag::untag_arg(name);
    let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        crate::thread_state::pon_err_set("attribute name must be str");
        return std::ptr::null_mut();
    };
    match name_text {
        "is_enabled" => {
            // One function object for the process, like the singleton itself.
            static IS_ENABLED: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
                // SAFETY: Live builtin entry point with the runtime calling
                // convention; NULL propagates as allocation failure below.
                let function = unsafe {
                    pon_make_function(
                        jit_is_enabled as *const u8,
                        crate::builtins::variadic_arity(),
                        intern("is_enabled"),
                    )
                };
                function as usize
            });
            let function = *IS_ENABLED as *mut PyObject;
            if function.is_null() {
                return return_null_with_error("failed to allocate sys._jit.is_enabled");
            }
            function
        }
        // SAFETY: Raise helper with the interned attribute name.
        _ => unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
    }
}

/// `sys._jit.is_enabled()`: `False` — see [`jit_attr`] for why.
unsafe extern "C" fn jit_is_enabled(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let _ = argv;
    if argc != 0 {
        return return_null_with_error(format!("is_enabled() takes no arguments ({argc} given)"));
    }
    // SAFETY: Boolean constant helper follows the NULL-sentinel contract.
    unsafe { pon_const_bool(0) }
}

// ---------------------------------------------------------------------------
// sys.version_info
//
// CPython's `sys.version_info` is a structseq: a tuple subclass with named
// read-only fields (`major`, `minor`, `micro`, `releaselevel`, `serial`), so
// indexing, slicing, iteration, len, hashing, and tuple comparison all work
// through the tuple protocol while attribute reads resolve the same
// elements.  The vendored stdlib depends on the tuple shape at import time
// (`sysconfig` builds `f"{sys.version_info[0]}.{sys.version_info[1]}"`; a
// plain-string stand-in silently produced `'s.y'` paths).  pon builds the
// same shape through the tuple-embedding heap-class machinery — mirroring
// `os.terminal_size` — with `major = property(self[0])`-style getters and
// the CPython structseq repr.
// ---------------------------------------------------------------------------

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

/// The `sys.version_info` singleton, built once and reused across import
/// re-registration (the class and instance both live for the process).
fn version_info_attr() -> Result<(u32, *mut PyObject), String> {
    version_info_object().map(|object| (intern("version_info"), object))
}

/// The structseq singleton object shared by `sys.version_info` and
/// `sys.implementation.version` — identity-shared exactly like CPython.
fn version_info_object() -> Result<*mut PyObject, String> {
    static VERSION_INFO: std::sync::LazyLock<Result<usize, String>> =
        std::sync::LazyLock::new(|| build_version_info().map(|object| object as usize));
    VERSION_INFO.clone().map(|object| object as *mut PyObject)
}

/// Allocates the singleton: builds the `sys.version_info` class, then calls
/// it with the pinned five-tuple exactly like `tuple.__new__` construction.
fn build_version_info() -> Result<*mut PyObject, String> {
    let class = build_version_info_class()?;
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let releaselevel = unsafe {
        pon_const_str(VERSION_INFO_RELEASELEVEL.as_ptr(), VERSION_INFO_RELEASELEVEL.len())
    };
    if releaselevel.is_null() {
        return Err("failed to allocate sys.version_info.releaselevel".to_owned());
    }
    let mut items = [
        // SAFETY: Integer boxing helper follows the NULL-sentinel contract.
        unsafe { pon_const_int(VERSION_INFO_MAJOR) },
        unsafe { pon_const_int(VERSION_INFO_MINOR) },
        unsafe { pon_const_int(VERSION_INFO_MICRO) },
        releaselevel,
        unsafe { pon_const_int(VERSION_INFO_SERIAL) },
    ];
    if items.iter().any(|item| item.is_null()) {
        return Err("failed to allocate sys.version_info elements".to_owned());
    }
    // SAFETY: The slots above are live objects per the call ABI.
    let values = unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) };
    if values.is_null() {
        return Err("failed to allocate the sys.version_info value tuple".to_owned());
    }
    let mut argv = [values];
    // SAFETY: The class is a live tuple-derived heap class; calling it routes
    // through `tuple.__new__` construction over the iterable argument.
    let instance = unsafe { crate::abi::pon_call(class, argv.as_mut_ptr(), argv.len()) };
    if instance.is_null() {
        let detail = crate::thread_state::pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
        crate::thread_state::pon_err_clear();
        return Err(format!("failed to construct sys.version_info: {detail}"));
    }
    Ok(instance)
}

/// `class version_info(tuple)` with the CPython structseq surface: field
/// properties reading `self[i]` and the `sys.version_info(...)` repr.
fn build_version_info_class() -> Result<*mut PyObject, String> {
    // SAFETY: `pon_load_global` returns NULL with a raised NameError on miss.
    let tuple_class = unsafe { crate::abi::pon_load_global(intern("tuple"), std::ptr::null_mut()) };
    if tuple_class.is_null() {
        crate::thread_state::pon_err_clear();
        return Err("builtin 'tuple' is not registered for sys.version_info".to_owned());
    }
    // SAFETY: Same contract for the builtin `property` constructor.
    let property_class = unsafe { crate::abi::pon_load_global(intern("property"), std::ptr::null_mut()) };
    if property_class.is_null() {
        crate::thread_state::pon_err_clear();
        return Err("builtin 'property' is not registered for sys.version_info".to_owned());
    }
    let namespace = crate::types::type_::new_namespace();
    if namespace.is_null() {
        return Err("failed to allocate the sys.version_info namespace".to_owned());
    }
    class_str_attr(namespace, "__module__", "sys")?;
    class_str_attr(namespace, "__doc__", "sys.version_info\n\nVersion information as a named tuple.")?;
    class_function_attr(namespace, "__repr__", version_info_repr)?;
    for (name, entry) in [
        ("major", version_info_major as BuiltinFn),
        ("minor", version_info_minor as BuiltinFn),
        ("micro", version_info_micro as BuiltinFn),
        ("releaselevel", version_info_releaselevel as BuiltinFn),
        ("serial", version_info_serial as BuiltinFn),
    ] {
        // SAFETY: Live builtin entry point with the runtime calling convention.
        let fget = unsafe { pon_make_function(entry as *const u8, 1, intern(name)) };
        if fget.is_null() {
            return Err(format!("failed to allocate sys.version_info.{name} getter"));
        }
        let mut argv = [fget];
        // SAFETY: The builtin `property` class is callable with one fget slot.
        let descriptor = unsafe { crate::abi::pon_call(property_class, argv.as_mut_ptr(), argv.len()) };
        if descriptor.is_null() {
            let detail = crate::thread_state::pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
            crate::thread_state::pon_err_clear();
            return Err(format!("failed to build sys.version_info.{name} property: {detail}"));
        }
        // SAFETY: `new_namespace` returned a live namespace box.
        unsafe { (&mut *namespace).set(intern(name), descriptor) };
    }
    // SAFETY: The base is the live builtin `tuple` class object.
    let class = unsafe {
        crate::types::type_::build_class_from_namespace("version_info", &[tuple_class], namespace, &[])
    };
    if class.is_null() {
        let detail = crate::thread_state::pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
        crate::thread_state::pon_err_clear();
        return Err(format!("failed to create sys.version_info: {detail}"));
    }
    // SAFETY: Freshly built class object owned by this module build; mirror
    // `pon_build_class`'s ob_type fix-up for a metaclass-less construction.
    unsafe {
        if (*class).ob_type.is_null() {
            (*class).ob_type = crate::abi::runtime_type_type().cast_const();
        }
    }
    Ok(class)
}

/// Seeds a str-valued class attribute into a class namespace under build.
fn class_str_attr(
    namespace: *mut crate::types::type_::PyClassDict,
    name: &str,
    value: &str,
) -> Result<(), String> {
    // SAFETY: String allocation helper; NULL is checked below.
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    if object.is_null() {
        return Err(format!("failed to allocate sys.version_info attribute '{name}'"));
    }
    // SAFETY: The caller passes a live namespace box.
    unsafe { (&mut *namespace).set(intern(name), object) };
    Ok(())
}

/// Seeds a native-function class attribute into a class namespace under build.
fn class_function_attr(
    namespace: *mut crate::types::type_::PyClassDict,
    name: &str,
    entry: BuiltinFn,
) -> Result<(), String> {
    // SAFETY: Live builtin entry point with the runtime calling convention.
    let function =
        unsafe { pon_make_function(entry as *const u8, crate::builtins::variadic_arity(), intern(name)) };
    if function.is_null() {
        return Err(format!("failed to allocate sys.version_info method '{name}'"));
    }
    // SAFETY: The caller passes a live namespace box.
    unsafe { (&mut *namespace).set(intern(name), function) };
    Ok(())
}

/// Borrows the argv slots as a slice; NULL argv reads as empty.
unsafe fn call_args<'a>(argv: *mut *mut PyObject, argc: usize) -> &'a [*mut PyObject] {
    if argv.is_null() || argc == 0 {
        &[]
    } else {
        // SAFETY: The caller passed `argc` live argument slots.
        unsafe { std::slice::from_raw_parts(argv, argc) }
    }
}

/// Shared property-getter body: `self[index]` through subscript dispatch,
/// which resolves the tuple-embedded heap-class layout.
unsafe fn version_info_field(argv: *mut *mut PyObject, argc: usize, index: i64, what: &str) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 1 {
        return return_null_with_error(format!("{what} expected only a receiver"));
    }
    // SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
    let key = unsafe { pon_const_int(index) };
    if key.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: Subscript dispatch resolves the tuple-embedded layout.
    unsafe { crate::abstract_op::subscript_get(args[0], key) }
}

/// `version_info.major` property getter: `self[0]`.
unsafe extern "C" fn version_info_major(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded argument slots per the runtime calling convention.
    unsafe { version_info_field(argv, argc, 0, "version_info.major") }
}

/// `version_info.minor` property getter: `self[1]`.
unsafe extern "C" fn version_info_minor(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded argument slots per the runtime calling convention.
    unsafe { version_info_field(argv, argc, 1, "version_info.minor") }
}

/// `version_info.micro` property getter: `self[2]`.
unsafe extern "C" fn version_info_micro(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded argument slots per the runtime calling convention.
    unsafe { version_info_field(argv, argc, 2, "version_info.micro") }
}

/// `version_info.releaselevel` property getter: `self[3]`.
unsafe extern "C" fn version_info_releaselevel(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded argument slots per the runtime calling convention.
    unsafe { version_info_field(argv, argc, 3, "version_info.releaselevel") }
}

/// `version_info.serial` property getter: `self[4]`.
unsafe extern "C" fn version_info_serial(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded argument slots per the runtime calling convention.
    unsafe { version_info_field(argv, argc, 4, "version_info.serial") }
}

/// Reads element `index` of a version_info receiver as an i64.
unsafe fn version_info_int(argv: *mut *mut PyObject, argc: usize, index: i64, what: &str) -> Result<i64, *mut PyObject> {
    // SAFETY: Forwarded argument slots per the runtime calling convention.
    let element = unsafe { version_info_field(argv, argc, index, what) };
    if element.is_null() {
        return Err(core::ptr::null_mut());
    }
    if crate::tag::is_small_int(element) {
        return Ok(crate::tag::untag_small_int(element));
    }
    // SAFETY: Non-immediate pointers are boxed objects; conversion type-checks.
    match unsafe { crate::types::int::to_bigint_including_bool(element) } {
        Some(value) => value.to_i64().ok_or_else(|| return_null_with_error(format!("{what} does not fit in an i64"))),
        None => Err(return_null_with_error(format!("{what} must be an integer"))),
    }
}

/// CPython's structseq repr:
/// `sys.version_info(major=3, minor=14, micro=6, releaselevel='final', serial=0)`.
unsafe extern "C" fn version_info_repr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded argument slots per the runtime calling convention.
    let major = match unsafe { version_info_int(argv, argc, 0, "version_info.major") } {
        Ok(value) => value,
        Err(error) => return error,
    };
    // SAFETY: Same forwarding contract for the remaining elements.
    let minor = match unsafe { version_info_int(argv, argc, 1, "version_info.minor") } {
        Ok(value) => value,
        Err(error) => return error,
    };
    // SAFETY: Same forwarding contract for the remaining elements.
    let micro = match unsafe { version_info_int(argv, argc, 2, "version_info.micro") } {
        Ok(value) => value,
        Err(error) => return error,
    };
    // SAFETY: Same forwarding contract for the remaining elements.
    let level_object = unsafe { version_info_field(argv, argc, 3, "version_info.releaselevel") };
    if level_object.is_null() {
        return core::ptr::null_mut();
    }
    let level_object = crate::tag::untag_arg(level_object);
    let Some(releaselevel) = (unsafe { crate::types::type_::unicode_text(level_object) }) else {
        return return_null_with_error("version_info.releaselevel must be a str");
    };
    // SAFETY: Same forwarding contract for the remaining elements.
    let serial = match unsafe { version_info_int(argv, argc, 4, "version_info.serial") } {
        Ok(value) => value,
        Err(error) => return error,
    };
    let text = format_version_info(major, minor, micro, releaselevel, serial);
    // SAFETY: String allocation helper follows the NULL-sentinel contract.
    unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

// ---------------------------------------------------------------------------
// sys.implementation
//
// CPython builds this as a `types.SimpleNamespace` in `_PySys_InitCore`, and
// the vendored `types.py` defines `SimpleNamespace = type(sys.implementation)`,
// so the singleton's type is named `types.SimpleNamespace`.  `test.support`
// reads `sys.implementation.name` at import time (`check_impl_detail()` is
// `guards.get(sys.implementation.name, default)`), and `importlib._bootstrap`'s
// FrozenImporter would call `type(sys.implementation)(...)` only after
// `_imp.find_frozen` returns non-None — pon's always returns None, so the
// type never needs to be callable.
//
// Served fields, with documented divergences from the CPython oracle:
// - `name`: 'pon' (CPython 'cpython').  The honest implementation name:
//   `check_impl_detail()` then returns False, so CPython-implementation-
//   detail tests guard/skip — correct under pon.  Corpus modules must stay
//   shape-only here; printing `.name` diverges from the oracle.
// - `cache_tag`: 'pon-314' (CPython 'cpython-314'), the same
//   `{name}-{major}{minor}` derivation off the version pin.
// - `version`: THE `sys.version_info` structseq singleton, identity-shared
//   like CPython (`sys.implementation.version is sys.version_info`).
// - `hexversion`: the `HEXVERSION` pin.
// - `_multiarch`: ABSENT (CPython darwin: 'darwin').  `sysconfig` probes it
//   with `hasattr`/`getattr(..., '_multiarch', '')`, so the AttributeError
//   raise below selects the default — keeping the `_sysconfigdata__darwin_`
//   module name `native/sysconfigdata.rs` serves.
// - `supports_isolated_interpreters` (CPython 3.14: True) and every other
//   name: ABSENT — unknown attributes raise so the next frontier is loud,
//   not a silently wrong default.
// ---------------------------------------------------------------------------

const IMPLEMENTATION_NAME: &str = "pon";

/// `sys.implementation.cache_tag`: CPython's `{name}-{major}{minor}` shape
/// over pon's implementation name and the pinned version block.
fn implementation_cache_tag() -> String {
    format!("{IMPLEMENTATION_NAME}-{VERSION_INFO_MAJOR}{VERSION_INFO_MINOR}")
}

/// `sys.implementation` singleton (the flags/hash_info pattern: an opaque
/// leaked object whose getattro serves the consumed field set).
fn implementation_attr() -> Result<(u32, *mut PyObject), String> {
    // Force the shared structseq singleton so the getattro below can never
    // observe a failed build: module init surfaces the error instead.
    version_info_object()?;
    static IMPLEMENTATION_TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        let mut ty = crate::object::PyType::new(
            std::ptr::null(),
            "types.SimpleNamespace",
            std::mem::size_of::<crate::object::PyObjectHeader>(),
        );
        ty.tp_getattro = Some(implementation_getattro);
        ty.tp_repr = Some(implementation_repr);
        Box::into_raw(Box::new(ty)) as usize
    });
    static IMPLEMENTATION: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        Box::into_raw(Box::new(crate::object::PyObjectHeader::new(
            *IMPLEMENTATION_TYPE as *mut crate::object::PyType,
        ))) as usize
    });
    Ok((intern("implementation"), *IMPLEMENTATION as *mut PyObject))
}

unsafe extern "C" fn implementation_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let name = crate::tag::untag_arg(name);
    let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        crate::thread_state::pon_err_set("attribute name must be str");
        return std::ptr::null_mut();
    };
    match name_text {
        // SAFETY: Runtime allocation helpers return NULL with the error set.
        "name" => unsafe { pon_const_str(IMPLEMENTATION_NAME.as_ptr(), IMPLEMENTATION_NAME.len()) },
        "cache_tag" => {
            let tag = implementation_cache_tag();
            // SAFETY: As above; the String outlives the copying allocation.
            unsafe { pon_const_str(tag.as_ptr(), tag.len()) }
        }
        // SAFETY: Integer boxing helper follows the NULL-sentinel contract.
        "hexversion" => unsafe { pon_const_int(HEXVERSION) },
        // Identity-shared with `sys.version_info`; the Err arm is unreachable
        // in practice — `implementation_attr` forced the singleton at init.
        "version" => match version_info_object() {
            Ok(object) => object,
            Err(message) => {
                crate::thread_state::pon_err_set(&message);
                std::ptr::null_mut()
            }
        },
        // SAFETY: Raise helper with the interned attribute name.
        _ => unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
    }
}

/// The SimpleNamespace repr over the served fields in CPython's insertion
/// order (name, cache_tag, version, hexversion); honest — the name and
/// cache_tag VALUES diverge from the CPython oracle by design.
unsafe extern "C" fn implementation_repr(_object: *mut PyObject) -> *mut PyObject {
    let text = format!(
        "namespace(name='{IMPLEMENTATION_NAME}', cache_tag='{}', version={}, hexversion={HEXVERSION})",
        implementation_cache_tag(),
        format_version_info(
            VERSION_INFO_MAJOR,
            VERSION_INFO_MINOR,
            VERSION_INFO_MICRO,
            VERSION_INFO_RELEASELEVEL,
            VERSION_INFO_SERIAL,
        ),
    );
    // SAFETY: Runtime string allocation helper; NULL on failure with the error set.
    unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { pon_const_int(value) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate sys.{name}"))
}

fn function_attr(
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { pon_make_function(entry as *const u8, crate::builtins::variadic_arity(), intern(name)) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate sys.{name}"))
}

/// `sys._getframe([depth])`.
///
/// pon materializes Python frames only on generator resume and raise paths,
/// so this synthesizes a fresh empty frame of the shared runtime `frame` type
/// per call.  The `depth` argument must be an `int` (mirroring CPython's
/// TypeError contract) and selects which compiled call-stack entry donates
/// the frame's `f_globals` namespace: the defining module of the function
/// `depth` levels above the `_getframe` call, the active module when the
/// walk runs past the tracked stack (module-toplevel frames; also CPython's
/// too-deep ValueError case, loosened here).  Negative depths clamp to the
/// current frame like CPython.  No `f_back` chain exists to walk;
/// `_collections_abc`'s PEP 667 probe (`type(sys._getframe().f_locals)`)
/// only needs the frame's `f_locals` type identity, served by
/// `crate::types::frame::frame_getattro`.
unsafe extern "C" fn sys_getframe(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc > 1 {
        return return_null_with_error(format!("_getframe() takes at most 1 argument ({argc} given)"));
    }
    let mut depth = 0usize;
    if argc == 1 {
        if argv.is_null() {
            return return_null_with_error("argv pointer is null");
        }
        // SAFETY: `argv` carries `argc` argument slots per the call ABI.
        let depth_object = crate::tag::untag_arg(unsafe { *argv });
        if depth_object.is_null() {
            // Boxing a tagged immediate failed; the error is already recorded.
            return core::ptr::null_mut();
        }
        let Some(value) = (unsafe { crate::types::int::to_bigint_including_bool(depth_object) }) else {
            return return_null_with_error("_getframe() argument must be int");
        };
        depth = value.to_usize().unwrap_or(0);
    }
    let globals_module = crate::abi::frame_defining_module_for_depth(depth, sys_getframe as *const u8)
        .or_else(crate::import::active_module_name_id);
    crate::types::frame::synthesize_frame_object(globals_module)
}

/// `sys.intern(string)`.
///
/// pon has no canonical-identity string table, so the argument itself is
/// returned after the CPython str type check: `intern` only promises an
/// equal string usable as a dict key, and the embedded stdlib (`collections`
/// namedtuple) never relies on cross-call identity folding.
unsafe extern "C" fn sys_intern(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 {
        return return_null_with_error(format!("intern() takes exactly 1 argument ({argc} given)"));
    }
    if argv.is_null() {
        return return_null_with_error("argv pointer is null");
    }
    // SAFETY: `argv` carries `argc` argument slots per the call ABI.
    let value = crate::tag::untag_arg(unsafe { *argv });
    if value.is_null() {
        // Boxing a tagged immediate failed; the error is already recorded.
        return core::ptr::null_mut();
    }
    if unsafe { !crate::types::int::type_name_is(value, "str") } {
        return return_null_with_error("intern() argument must be str");
    }
    value
}

/// Source of `sys.exception()` / `sys.exc_info()`: the object-safe pending
/// exception when one is installed (a handler-entry helper running before
/// any call), else the thread's parked handled exception
/// (`PonThreadState.handled_exc`) — parked by `pon_match_exc` /
/// `pon_get_current_exc` / `pon_exc_star_match` at handler entry and
/// saved/restored around every call boundary by `abi::HandledExcGuard`.
///
/// Documented divergence: CPython resets the handled exception when the
/// `except` BLOCK exits; pon resets when the catching FRAME returns, so a
/// read later in the SAME frame (e.g. module level after a module-level
/// `try`) still reports the last caught exception.  Function-scoped
/// handlers — the shape `unittest`/`contextlib` use — observe CPython
/// behavior exactly.
fn handled_exception() -> Option<*mut PyObject> {
    crate::abi::exc::pending_exception_object().or_else(|| {
        let handled = crate::thread_state::thread_state_lock().handled_exc;
        (!handled.is_null()).then_some(handled)
    })
}

/// `sys.exception()` (CPython 3.12+): the exception being handled, or `None`.
unsafe extern "C" fn sys_exception(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let _ = argv;
    if argc != 0 {
        return return_null_with_error(format!("exception() takes no arguments ({argc} given)"));
    }
    match handled_exception() {
        Some(exception) => exception,
        // SAFETY: Singleton accessor.
        None => unsafe { crate::abi::pon_none() },
    }
}

/// `sys.exc_info()`: the `(type, value, traceback)` triple for the handled
/// exception (same source as [`sys_exception`]), or `(None, None, None)`
/// outside handlers.  The traceback element is the exception's own
/// `__traceback__` (None-or-chain contract).
unsafe extern "C" fn sys_exc_info(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let _ = argv;
    if argc != 0 {
        return return_null_with_error(format!("exc_info() takes no arguments ({argc} given)"));
    }
    // SAFETY: Singleton accessor.
    let none = unsafe { crate::abi::pon_none() };
    let items = match handled_exception() {
        Some(exception) => {
            let mut slot = exception;
            // SAFETY: One live argument slot; `builtin_type` reports errors via NULL.
            let exc_type = unsafe { crate::types::type_::builtin_type(&mut slot, 1) };
            if exc_type.is_null() {
                return core::ptr::null_mut();
            }
            // SAFETY: Every raise path allocates the `PyBaseException` layout,
            // so the object-safe pending exception carries the traceback slot.
            let traceback = unsafe { (*exception.cast::<crate::types::exc::PyBaseException>()).traceback };
            vec![exc_type, exception, if traceback.is_null() { none } else { traceback }]
        }
        None => vec![none, none, none],
    };
    super::builtins_mod::alloc_tuple(items)
}

/// `sys.excepthook(exc_type, exc_value, traceback)`.
///
/// `threading._make_invoke_excepthook` snapshots this at import and only
/// calls it when `threading.excepthook` itself fails, so a compact
/// `Type: message` report to stderr (the shape pon prints for uncaught
/// errors) is the honest surface; no Python traceback rendering exists to
/// reuse here.
unsafe extern "C" fn sys_excepthook(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    use std::io::Write;

    if argc != 3 {
        return return_null_with_error(format!("excepthook() takes exactly 3 arguments ({argc} given)"));
    }
    if argv.is_null() {
        return return_null_with_error("argv pointer is null");
    }
    // SAFETY: `argv` carries `argc` argument slots per the call ABI.
    let value = crate::tag::untag_arg(unsafe { *argv.add(1) });
    if value.is_null() {
        // Boxing a tagged immediate failed; the error is already recorded.
        return core::ptr::null_mut();
    }
    let type_name = unsafe {
        let ty = (*value).ob_type;
        if ty.is_null() { "<unknown>" } else { (*ty).name() }
    };
    let message = super::builtins_mod::str_text(value);
    let mut stderr = std::io::stderr().lock();
    let outcome = if message.is_empty() {
        writeln!(stderr, "{type_name}")
    } else {
        writeln!(stderr, "{type_name}: {message}")
    };
    if let Err(error) = outcome.and_then(|()| stderr.flush()) {
        return return_null_with_error(format!("excepthook() failed to write stderr: {error}"));
    }
    // SAFETY: Singleton accessor.
    unsafe { crate::abi::pon_none() }
}

// ---------------------------------------------------------------------------
// sys.getrecursionlimit / sys.setrecursionlimit
//
// CPython stores the recursion limit on the thread state and enforces it on
// every interpreter frame push.  pon compiles calls natively and never
// consults this value on its call paths, so the limit is STORED BUT NOT
// ENFORCED (documented divergence): no depth of pon recursion raises
// RecursionError from this limit.  The stored value exists because
// `inspect.unwrap()` reads `sys.getrecursionlimit()` as a pure loop bound —
// reached at import time by `import asyncio` (asyncio.graph's frozen
// dataclasses resolve annotations through `annotationlib`/`functools`, which
// import `inspect`) — and stdlib callers of `setrecursionlimit` expect the
// next `getrecursionlimit` to reflect the write.  If a RecursionError
// consumer ever appears, the enforcement hook is the compiled call-depth
// tracking already counted by `crate::abi::current_function_stack_depth()`.
// CPython's second `setrecursionlimit` guard — RecursionError("cannot set
// the recursion limit to N at the recursion depth D: the limit is too low")
// when the new limit does not clear the current depth — is unimplemented
// for the same reason.  Validation wording matches the host CPython 3.14.6
// oracle exactly.
// ---------------------------------------------------------------------------

/// CPython `Py_DEFAULT_RECURSION_LIMIT`.
const DEFAULT_RECURSION_LIMIT: usize = 1000;

/// The stored `sys.setrecursionlimit` value; see the section comment for
/// the enforcement divergence.  Relaxed suffices: readers only need to
/// observe a value some `setrecursionlimit` call stored, and the value
/// carries no happens-before payload.
static RECURSION_LIMIT: AtomicUsize = AtomicUsize::new(DEFAULT_RECURSION_LIMIT);

/// `sys.getrecursionlimit()`: the stored limit (default 1000).
unsafe extern "C" fn sys_getrecursionlimit(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let _ = argv;
    if argc != 0 {
        return raise_type_error(&format!("getrecursionlimit() takes no arguments ({argc} given)"));
    }
    // SAFETY: Runtime allocation helper returns NULL with a diagnostic on failure.
    unsafe { pon_const_int(RECURSION_LIMIT.load(Ordering::Relaxed) as i64) }
}

/// `sys.setrecursionlimit(limit)`: validates a positive int and stores it;
/// never enforced (see the section comment).
unsafe extern "C" fn sys_setrecursionlimit(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 {
        return raise_type_error(&format!("setrecursionlimit() takes exactly one argument ({argc} given)"));
    }
    if argv.is_null() {
        return return_null_with_error("argv pointer is null");
    }
    // SAFETY: `argv` carries `argc` argument slots per the call ABI.
    let limit_object = crate::tag::untag_arg(unsafe { *argv });
    if limit_object.is_null() {
        // Boxing a tagged immediate failed; the error is already recorded.
        return core::ptr::null_mut();
    }
    // CPython converts through `__index__` (bool included); int/bool payload
    // extraction is the same acceptance set for the types pon has.
    let Some(value) = (unsafe { crate::types::int::to_bigint_including_bool(limit_object) }) else {
        let type_name = unsafe { crate::types::dict::type_name(limit_object) }.unwrap_or("object");
        return raise_type_error(&format!("'{type_name}' object cannot be interpreted as an integer"));
    };
    if value.sign() != num_bigint::Sign::Plus {
        return raise_value_error("recursion limit must be greater or equal than 1");
    }
    let Some(limit) = value.to_usize() else {
        // Positive but unstorable; CPython's C-int conversion overflow leg.
        return return_null_with_error("Python int too large to convert to C int");
    };
    RECURSION_LIMIT.store(limit, Ordering::Relaxed);
    // SAFETY: Singleton accessor.
    unsafe { crate::abi::pon_none() }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    // SAFETY: Message bytes are a live UTF-8 slice for the duration of the call.
    unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn raise_value_error(message: &str) -> *mut PyObject {
    // SAFETY: Message bytes are a live UTF-8 slice for the duration of the call.
    unsafe { crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) }
}
