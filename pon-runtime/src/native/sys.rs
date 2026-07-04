//! Native `sys` module seed for WS-IMPORT.

use std::sync::atomic::{AtomicUsize, Ordering};

use num_traits::ToPrimitive;

use crate::abi::{pon_const_bool, pon_const_int, pon_const_str, pon_make_function, return_null_with_error};
use crate::intern::intern;
use crate::object::{PyObject, PyType};
use crate::types::type_::PyHeapInstance;
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

/// Absolute path to the Python-compatible interpreter for `sys.executable`.
/// CPython code (notably `mesonpy`/`subprocess` spawning
/// `[sys.executable, script]`) requires a spawnable Python runner, not merely
/// the embedding process.  Embedders may provide `PON_SYS_EXECUTABLE`; the CLI
/// path remains the fallback.
fn sys_executable_path() -> String {
    if let Some(path) = std::env::var_os("PON_SYS_EXECUTABLE").filter(|path| !path.is_empty()) {
        return path.to_string_lossy().into_owned();
    }
    std::env::current_exe()
        .ok()
        .and_then(|path| path.to_str().map(str::to_owned))
        .unwrap_or_else(|| "pon".to_owned())
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let attrs = [
        // Shape must satisfy `platform._sys_version`'s parser
        // (`version (buildno, builddate, buildtime) [compiler]`): meson's
        // CommandLineParser calls `platform.python_version()` while building
        // argparse subcommands.  The build fields are fixed strings — pon has
        // no build number — and the oracle check is
        // `platform._sys_version('3.14.6 (pon, Jan  1 2026, 00:00:00) [pon]')`.
        string_attr(
            "version",
            &format!("{VERSION_INFO_MAJOR}.{VERSION_INFO_MINOR}.{VERSION_INFO_MICRO} (pon, Jan  1 2026, 00:00:00) [pon]"),
        ),
        version_info_attr(),
        implementation_attr(),
        int_attr("hexversion", HEXVERSION),
        int_attr("maxsize", i64::MAX),
        // Wide-Unicode max codepoint; CPython has shipped only wide builds
        // since 3.3 (`mesonbuild.mtest` branches on it at import).
        int_attr("maxunicode", 0x0010_FFFF),
        string_attr("platform", PLATFORM),
        string_attr("byteorder", if cfg!(target_endian = "little") { "little" } else { "big" }),
        string_attr("executable", &sys_executable_path()),
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
        float_info_attr(),
        int_info_attr(),
        thread_info_attr(),
        // `repr(float)` uses the shortest round-tripping form — CPython's
        // only style since 3.1 on every platform pon builds for (the
        // 'legacy' style needs a pre-C99 double parser).  `test.test_float`
        // gates its short-repr tests on this at import time.
        string_attr("float_repr_style", "short"),
        jit_attr(),
        monitoring_attr(),
        warnoptions_attr(),
        meta_path_attr(),
        path_importer_cache_attr(),
        path_attr(),
        modules_attr(),
        builtin_module_names_attr(),
        function_attr("_getframe", sys_getframe),
        function_attr("exit", sys_exit),
        function_attr("intern", sys_intern),
        function_attr("exception", sys_exception),
        function_attr("exc_info", sys_exc_info),
        function_attr("excepthook", sys_excepthook),
        function_attr("getrecursionlimit", sys_getrecursionlimit),
        function_attr("setrecursionlimit", sys_setrecursionlimit),
        function_attr("getfilesystemencoding", sys_getfilesystemencoding),
        function_attr("getfilesystemencodeerrors", sys_getfilesystemencodeerrors),
        function_attr("_clear_type_descriptors", sys_clear_type_descriptors),
        function_attr("audit", sys_audit),
        function_attr("addaudithook", sys_addaudithook),
        function_attr("getsizeof", sys_getsizeof),
        std_stream_attr("stdin", 0),
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

/// `sys.stdin`/`sys.stdout`/`sys.stderr`: text streams over the process
/// fds (see `io::std_stream_object`).  `print()` writes host stdout
/// directly and does not consult `sys.stdout`; these objects serve
/// explicit stream users (unittest's TextTestRunner writes to
/// `sys.stderr`, `test.test_univnewlines` probes `sys.stdin.newlines` at
/// import).  fd 0 is the read side; 1/2 stay write-only.
fn std_stream_attr(name: &str, fd: i32) -> Result<(u32, *mut PyObject), String> {
    let object = super::io::std_stream_object(fd, &format!("<{name}>"), fd == 0);
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

/// `sys.path_importer_cache`: the path-entry finder cache `runpy`'s
/// `_get_code_from_file` machinery reads and writes.  pon's native importer
/// never consults it (documented divergence, like `meta_path`), so the
/// honest surface is one identity-stable empty dict that Python code may
/// populate freely.
fn path_importer_cache_attr() -> Result<(u32, *mut PyObject), String> {
    static CACHE: std::sync::LazyLock<Result<usize, String>> = std::sync::LazyLock::new(|| {
        // SAFETY: Map builder allocates an empty runtime dict.
        let dict = unsafe { crate::abi::map::pon_build_map(std::ptr::null_mut(), 0) };
        if dict.is_null() {
            return Err("failed to allocate sys.path_importer_cache".to_owned());
        }
        Ok(dict as usize)
    });
    CACHE
        .clone()
        .map(|object| (intern("path_importer_cache"), object as *mut PyObject))
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
        // Plain `python3` invocation values (no -X/-O/-W switches reach
        // pon).  `thread_inherit_context` is 0 to match the reference
        // CPython default (GIL) build: threads start with an empty context.
        "context_aware_warnings" | "debug" | "optimize" | "verbose" | "ignore_environment"
        | "thread_inherit_context" | "inspect" | "interactive" | "no_user_site" | "no_site"
        | "bytes_warning" | "quiet" | "isolated" | "utf8_mode" | "warn_default_encoding" => unsafe {
            pon_const_int(0)
        },
        // CPython types these two as bool (`sys.flags(... dev_mode=False,
        // safe_path=False ...)`); printed fields must match the oracle's
        // False, not 0.
        "dev_mode" | "safe_path" => unsafe { pon_const_bool(0) },
        // The int->str conversion guard default, matching the host oracle;
        // pon does not enforce the limit (see the int_info section note).
        "int_max_str_digits" => unsafe { pon_const_int(4300) },
        // Env-derived flags: the CT driver exports PYTHONDONTWRITEBYTECODE=1
        // and PYTHONHASHSEED=0 to BOTH engines, so oracle parity requires
        // reading the live environment rather than pinning either value.
        "dont_write_bytecode" => {
            let set = std::env::var("PYTHONDONTWRITEBYTECODE").is_ok_and(|value| !value.is_empty());
            // SAFETY: Integer boxing helper follows the NULL-sentinel contract.
            unsafe { pon_const_int(i64::from(set)) }
        }
        // CPython: randomization is OFF exactly when PYTHONHASHSEED names
        // the fixed seed 0 (`use_hash_seed && hash_seed == 0`); any other
        // state — unset, empty, "random", a nonzero seed — reports 1.
        "hash_randomization" => {
            let disabled = std::env::var("PYTHONHASHSEED").is_ok_and(|value| value.trim() == "0");
            // SAFETY: Integer boxing helper follows the NULL-sentinel contract.
            unsafe { pon_const_int(i64::from(!disabled)) }
        }
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
// sys.monitoring (PEP 669)
//
// CPython 3.12+ ships `sys.monitoring` as a real module wired into the
// bytecode instrumentation machinery.  pon compiles calls natively and has
// no instrumentation hooks, so only the CONSTANT surface is served: `bdb`
// reads `sys.monitoring.events` at import time (module-level `E =
// sys.monitoring.events`, then class-body dict keys / bitwise-ORs over the
// event flags) on the `doctest -> pdb -> bdb` chain, and
// `bdb._MonitoringTracer.__init__` reads the tool-id constants at
// instantiation.  Values are the CPython 3.14.6 oracle table
// (Include/cpython/monitoring.h: each event flag is `1 << event_id`).  The
// callable surface (`use_tool_id`, `register_callback`, `set_events`, ...)
// is deliberately unserved until a walk consumes it: pon never fires
// monitoring events, so a no-op tool registry would be a silently wrong
// default — unknown attributes raise so that frontier is loud.
// ---------------------------------------------------------------------------

/// `sys.monitoring` singleton (the flags/hash_info pattern: an opaque leaked
/// object whose getattro serves the consumed constant set).
fn monitoring_attr() -> Result<(u32, *mut PyObject), String> {
    static MONITORING_TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        let mut ty = crate::object::PyType::new(
            std::ptr::null(),
            "sys.monitoring",
            std::mem::size_of::<crate::object::PyObjectHeader>(),
        );
        ty.tp_getattro = Some(monitoring_getattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    static MONITORING: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        Box::into_raw(Box::new(crate::object::PyObjectHeader::new(
            *MONITORING_TYPE as *mut crate::object::PyType,
        ))) as usize
    });
    Ok((intern("monitoring"), *MONITORING as *mut PyObject))
}

unsafe extern "C" fn monitoring_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let name = crate::tag::untag_arg(name);
    let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        crate::thread_state::pon_err_set("attribute name must be str");
        return std::ptr::null_mut();
    };
    match name_text {
        "events" => monitoring_events_object(),
        // CPython tool-id slots (PY_MONITORING_*_ID); 3 and 4 are unnamed.
        "DEBUGGER_ID" => unsafe { pon_const_int(0) },
        "COVERAGE_ID" => unsafe { pon_const_int(1) },
        "PROFILER_ID" => unsafe { pon_const_int(2) },
        "OPTIMIZER_ID" => unsafe { pon_const_int(5) },
        // SAFETY: Raise helper with the interned attribute name.
        _ => unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
    }
}

/// CPython 3.14.6 `sys.monitoring.events` flag table (each event `1 << id`),
/// verified against the host oracle.  `NO_EVENTS` is the zero sentinel;
/// `BRANCH` is 3.14's deprecated alias event with its own bit.
const MONITORING_EVENTS: &[(&str, i64)] = &[
    ("NO_EVENTS", 0),
    ("PY_START", 1),
    ("PY_RESUME", 2),
    ("PY_RETURN", 4),
    ("PY_YIELD", 8),
    ("CALL", 16),
    ("LINE", 32),
    ("INSTRUCTION", 64),
    ("JUMP", 128),
    ("BRANCH_LEFT", 256),
    ("BRANCH_RIGHT", 512),
    ("STOP_ITERATION", 1024),
    ("RAISE", 2048),
    ("EXCEPTION_HANDLED", 4096),
    ("PY_UNWIND", 8192),
    ("PY_THROW", 16384),
    ("RERAISE", 32768),
    ("C_RETURN", 65536),
    ("C_RAISE", 131072),
    ("BRANCH", 262144),
];

/// The `sys.monitoring.events` singleton.  CPython types it
/// `types.SimpleNamespace` (the `sys.implementation` precedent); consumers
/// only read the flag constants, which must be real ints — `bdb` uses them
/// as dict keys and ORs them into event masks.
fn monitoring_events_object() -> *mut PyObject {
    static EVENTS_TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        let mut ty = crate::object::PyType::new(
            std::ptr::null(),
            "types.SimpleNamespace",
            std::mem::size_of::<crate::object::PyObjectHeader>(),
        );
        ty.tp_getattro = Some(monitoring_events_getattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    static EVENTS: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        Box::into_raw(Box::new(crate::object::PyObjectHeader::new(
            *EVENTS_TYPE as *mut crate::object::PyType,
        ))) as usize
    });
    *EVENTS as *mut PyObject
}

unsafe extern "C" fn monitoring_events_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let name = crate::tag::untag_arg(name);
    let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        crate::thread_state::pon_err_set("attribute name must be str");
        return std::ptr::null_mut();
    };
    match MONITORING_EVENTS.iter().find(|(event, _)| *event == name_text) {
        // SAFETY: Integer boxing helper follows the NULL-sentinel contract.
        Some((_, value)) => unsafe { pon_const_int(*value) },
        // SAFETY: Raise helper with the interned attribute name.
        None => unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
    }
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
// sys.float_info / sys.int_info / sys.thread_info
//
// The remaining `sys` structseqs, built through the same tuple-embedding
// heap-class machinery as `sys.version_info` above but generalized over a
// field table: per-index property getters shared across the classes, and
// the CPython structseq repr (`sys.float_info(max=1.797...e+308, ...)`)
// rendered through the runtime's own repr dispatch so element formatting
// (float shortest-repr, str quoting, None) matches `builtins.repr` exactly.
//
// Values, verified against the host CPython 3.14.6 oracle:
// - `float_info`: IEEE-754 binary64 constants, honestly pon's own float
//   type (Rust `f64` is the C `double` on every pon target), taken from
//   `f64::` consts so they can never drift from the implementation.
//   `test.test_long`/`test_complex`/`test_ast` read fields at import.
// - `int_info`: the CPython oracle row (30-bit digits, 4-byte digit,
//   4300/640 str-conversion guards).  DOCUMENTED DIVERGENCE: the digit
//   fields describe CPython's bignum limb layout, not pon's (i64 fast
//   path + heap bigint); in-cohort consumers only use them to SIZE test
//   values (`test_long`'s `SHIFT`/`BASE`), and pon does not enforce the
//   str-digit guards (`sys.set_int_max_str_digits` is unserved).
// - `thread_info`: `name='pthread'` honestly describes pon threads
//   (std::thread over pthreads on every POSIX pon target);
//   `lock='mutex+cond'` is the host oracle's value and describes
//   CPython's lock implementation, not pon's (documented divergence —
//   `test.test_threadsignals` branches on exactly this pair at import);
//   `version=None` matches the macOS oracle (no pthread version string).
// ---------------------------------------------------------------------------

/// One structseq family: class identity plus its named-field table.
struct StructSeqSpec {
    /// Class `__name__`, also the `sys` attribute name (`"float_info"`).
    name: &'static str,
    /// Repr prefix and `__doc__` headline (`"sys.float_info"`).
    qualname: &'static str,
    /// Named fields in tuple order; length picks the getter per index.
    fields: &'static [&'static str],
    /// Class-specific `__repr__` entry (a thin shim over
    /// [`structseq_repr_body`] monomorphized by spec).
    repr: BuiltinFn,
}

/// Element values a structseq seed can carry; allocated on construction.
enum SeqValue {
    Int(i64),
    Float(f64),
    Str(&'static str),
    None,
}

impl SeqValue {
    /// Boxes the value through the matching runtime allocation helper.
    fn allocate(&self, what: &str) -> Result<*mut PyObject, String> {
        // SAFETY: Allocation helpers follow the NULL-sentinel error contract.
        let object = match *self {
            Self::Int(value) => unsafe { pon_const_int(value) },
            Self::Float(value) => unsafe { crate::abi::number::pon_const_float(value) },
            Self::Str(value) => unsafe { pon_const_str(value.as_ptr(), value.len()) },
            Self::None => unsafe { crate::abi::pon_none() },
        };
        (!object.is_null()).then_some(object).ok_or_else(|| format!("failed to allocate {what}"))
    }
}

/// Shared per-index structseq property getters: `self[N]` through the same
/// subscript dispatch as the version_info getters.  The receiver's class
/// binds the field NAME to an index, so one getter set serves every
/// structseq family; `STRUCTSEQ_GETTERS[i]` is the fget for element `i`.
macro_rules! structseq_getters {
    ($($getter:ident => $index:literal),* $(,)?) => {
        $(
            unsafe extern "C" fn $getter(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
                // SAFETY: Forwarded argument slots per the runtime calling
                // convention.
                unsafe { version_info_field(argv, argc, $index, "structseq field") }
            }
        )*
        const STRUCTSEQ_GETTERS: &[BuiltinFn] = &[$($getter),*];
    };
}

structseq_getters!(
    structseq_get_0 => 0,
    structseq_get_1 => 1,
    structseq_get_2 => 2,
    structseq_get_3 => 3,
    structseq_get_4 => 4,
    structseq_get_5 => 5,
    structseq_get_6 => 6,
    structseq_get_7 => 7,
    structseq_get_8 => 8,
    structseq_get_9 => 9,
    structseq_get_10 => 10,
);

/// `class <name>(tuple)` with the CPython structseq surface over `spec`:
/// field properties reading `self[i]` and the `sys.<name>(...)` repr —
/// the version_info class builder generalized over the field table.
fn build_structseq_class(spec: &StructSeqSpec) -> Result<*mut PyObject, String> {
    debug_assert!(spec.fields.len() <= STRUCTSEQ_GETTERS.len());
    // SAFETY: `pon_load_global` returns NULL with a raised NameError on miss.
    let tuple_class = unsafe { crate::abi::pon_load_global(intern("tuple"), std::ptr::null_mut()) };
    if tuple_class.is_null() {
        crate::thread_state::pon_err_clear();
        return Err(format!("builtin 'tuple' is not registered for sys.{}", spec.name));
    }
    // SAFETY: Same contract for the builtin `property` constructor.
    let property_class = unsafe { crate::abi::pon_load_global(intern("property"), std::ptr::null_mut()) };
    if property_class.is_null() {
        crate::thread_state::pon_err_clear();
        return Err(format!("builtin 'property' is not registered for sys.{}", spec.name));
    }
    let namespace = crate::types::type_::new_namespace();
    if namespace.is_null() {
        return Err(format!("failed to allocate the sys.{} namespace", spec.name));
    }
    class_str_attr(namespace, "__module__", "sys")?;
    class_str_attr(namespace, "__doc__", &format!("{}\n\nA named tuple.", spec.qualname))?;
    class_function_attr(namespace, "__repr__", spec.repr)?;
    for (index, name) in spec.fields.iter().enumerate() {
        // SAFETY: Live builtin entry point with the runtime calling convention.
        let fget = unsafe { pon_make_function(STRUCTSEQ_GETTERS[index] as *const u8, 1, intern(name)) };
        if fget.is_null() {
            return Err(format!("failed to allocate sys.{}.{name} getter", spec.name));
        }
        let mut argv = [fget];
        // SAFETY: The builtin `property` class is callable with one fget slot.
        let descriptor = unsafe { crate::abi::pon_call(property_class, argv.as_mut_ptr(), argv.len()) };
        if descriptor.is_null() {
            let detail = crate::thread_state::pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
            crate::thread_state::pon_err_clear();
            return Err(format!("failed to build sys.{}.{name} property: {detail}", spec.name));
        }
        // SAFETY: `new_namespace` returned a live namespace box.
        unsafe { (&mut *namespace).set(intern(name), descriptor) };
    }
    // SAFETY: The base is the live builtin `tuple` class object.
    let class = unsafe {
        crate::types::type_::build_class_from_namespace(spec.name, &[tuple_class], namespace, &[])
    };
    if class.is_null() {
        let detail = crate::thread_state::pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
        crate::thread_state::pon_err_clear();
        return Err(format!("failed to create sys.{}: {detail}", spec.name));
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

/// Allocates a structseq singleton: builds the class, boxes the seed
/// values, and calls the class with the value tuple exactly like
/// `tuple.__new__` construction (the build_version_info shape).
fn build_structseq(spec: &StructSeqSpec, values: &[SeqValue]) -> Result<*mut PyObject, String> {
    debug_assert_eq!(spec.fields.len(), values.len());
    let class = build_structseq_class(spec)?;
    let mut items = Vec::with_capacity(values.len());
    for (value, field) in values.iter().zip(spec.fields) {
        items.push(value.allocate(&format!("sys.{}.{field}", spec.name))?);
    }
    // SAFETY: The slots above are live objects per the call ABI.
    let tuple = unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) };
    if tuple.is_null() {
        return Err(format!("failed to allocate the sys.{} value tuple", spec.name));
    }
    let mut argv = [tuple];
    // SAFETY: The class is a live tuple-derived heap class; calling it routes
    // through `tuple.__new__` construction over the iterable argument.
    let instance = unsafe { crate::abi::pon_call(class, argv.as_mut_ptr(), argv.len()) };
    if instance.is_null() {
        let detail = crate::thread_state::pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
        crate::thread_state::pon_err_clear();
        return Err(format!("failed to construct sys.{}: {detail}", spec.name));
    }
    Ok(instance)
}

/// Shared repr body: `sys.<name>(field=repr(self[i]), ...)` with element
/// text from the runtime's `builtins.repr` dispatch, so formatting matches
/// what printing the elements individually would produce.
unsafe fn structseq_repr_body(argv: *mut *mut PyObject, argc: usize, spec: &StructSeqSpec) -> *mut PyObject {
    use std::fmt::Write as _;
    let mut text = format!("{}(", spec.qualname);
    for (index, field) in spec.fields.iter().enumerate() {
        // SAFETY: Forwarded argument slots per the runtime calling convention.
        let element = unsafe { version_info_field(argv, argc, index as i64, field) };
        if element.is_null() {
            return core::ptr::null_mut();
        }
        let mut slot = [element];
        // SAFETY: One live argument slot; `builtin_repr` follows the
        // NULL-sentinel error contract.
        let repr_object = unsafe { super::builtins_mod::builtin_repr(slot.as_mut_ptr(), slot.len()) };
        if repr_object.is_null() {
            return core::ptr::null_mut();
        }
        let repr_object = crate::tag::untag_arg(repr_object);
        // SAFETY: `builtin_repr` returns a str object on success.
        let Some(repr_text) = (unsafe { crate::types::type_::unicode_text(repr_object) }) else {
            return return_null_with_error(format!("repr of sys.{}.{field} is not a str", spec.name));
        };
        let separator = if index == 0 { "" } else { ", " };
        let _ = write!(text, "{separator}{field}={repr_text}");
    }
    text.push(')');
    // SAFETY: String allocation helper follows the NULL-sentinel contract.
    unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

/// `sys.float_info` spec: IEEE-754 binary64, CPython field order.
static FLOAT_INFO_SPEC: StructSeqSpec = StructSeqSpec {
    name: "float_info",
    qualname: "sys.float_info",
    fields: &[
        "max",
        "max_exp",
        "max_10_exp",
        "min",
        "min_exp",
        "min_10_exp",
        "dig",
        "mant_dig",
        "epsilon",
        "radix",
        "rounds",
    ],
    repr: float_info_repr,
};

/// `sys.float_info.__repr__` entry: [`structseq_repr_body`] over the spec.
unsafe extern "C" fn float_info_repr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded argument slots per the runtime calling convention.
    unsafe { structseq_repr_body(argv, argc, &FLOAT_INFO_SPEC) }
}

/// The `sys.float_info` singleton, built once and reused across import
/// re-registration (the version_info lifetime pattern).
fn float_info_attr() -> Result<(u32, *mut PyObject), String> {
    static FLOAT_INFO: std::sync::LazyLock<Result<usize, String>> = std::sync::LazyLock::new(|| {
        build_structseq(
            &FLOAT_INFO_SPEC,
            &[
                SeqValue::Float(f64::MAX),
                SeqValue::Int(f64::MAX_EXP.into()),
                SeqValue::Int(f64::MAX_10_EXP.into()),
                SeqValue::Float(f64::MIN_POSITIVE),
                SeqValue::Int(f64::MIN_EXP.into()),
                SeqValue::Int(f64::MIN_10_EXP.into()),
                SeqValue::Int(f64::DIGITS.into()),
                SeqValue::Int(f64::MANTISSA_DIGITS.into()),
                SeqValue::Float(f64::EPSILON),
                SeqValue::Int(f64::RADIX.into()),
                // FLT_ROUNDS: round-to-nearest, the only mode pon runs in.
                SeqValue::Int(1),
            ],
        )
        .map(|object| object as usize)
    });
    FLOAT_INFO.clone().map(|object| (intern("float_info"), object as *mut PyObject))
}

/// `sys.int_info` spec: CPython field order.
static INT_INFO_SPEC: StructSeqSpec = StructSeqSpec {
    name: "int_info",
    qualname: "sys.int_info",
    fields: &["bits_per_digit", "sizeof_digit", "default_max_str_digits", "str_digits_check_threshold"],
    repr: int_info_repr,
};

/// `sys.int_info.__repr__` entry: [`structseq_repr_body`] over the spec.
unsafe extern "C" fn int_info_repr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded argument slots per the runtime calling convention.
    unsafe { structseq_repr_body(argv, argc, &INT_INFO_SPEC) }
}

/// The `sys.int_info` singleton (see the section comment for the digit-
/// layout divergence record).
fn int_info_attr() -> Result<(u32, *mut PyObject), String> {
    static INT_INFO: std::sync::LazyLock<Result<usize, String>> = std::sync::LazyLock::new(|| {
        build_structseq(
            &INT_INFO_SPEC,
            &[SeqValue::Int(30), SeqValue::Int(4), SeqValue::Int(4300), SeqValue::Int(640)],
        )
        .map(|object| object as usize)
    });
    INT_INFO.clone().map(|object| (intern("int_info"), object as *mut PyObject))
}

/// `sys.thread_info` spec: CPython field order.
static THREAD_INFO_SPEC: StructSeqSpec = StructSeqSpec {
    name: "thread_info",
    qualname: "sys.thread_info",
    fields: &["name", "lock", "version"],
    repr: thread_info_repr,
};

/// `sys.thread_info.__repr__` entry: [`structseq_repr_body`] over the spec.
unsafe extern "C" fn thread_info_repr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded argument slots per the runtime calling convention.
    unsafe { structseq_repr_body(argv, argc, &THREAD_INFO_SPEC) }
}

/// The `sys.thread_info` singleton (see the section comment for the lock-
/// name divergence record).
fn thread_info_attr() -> Result<(u32, *mut PyObject), String> {
    static THREAD_INFO: std::sync::LazyLock<Result<usize, String>> = std::sync::LazyLock::new(|| {
        build_structseq(
            &THREAD_INFO_SPEC,
            &[SeqValue::Str("pthread"), SeqValue::Str("mutex+cond"), SeqValue::None],
        )
        .map(|object| object as usize)
    });
    THREAD_INFO.clone().map(|object| (intern("thread_info"), object as *mut PyObject))
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

/// The `sys.implementation` singleton, backed by the same mutable
/// `types.SimpleNamespace` type that vendored `types.py` re-exports via
/// `type(sys.implementation)`.
fn implementation_attr() -> Result<(u32, *mut PyObject), String> {
    // Force the shared structseq singleton so the seed below can never
    // observe a failed build: module init surfaces the error instead.
    version_info_object()?;
    let ty = simple_namespace_type()?;
    static IMPLEMENTATION: std::sync::LazyLock<Result<usize, String>> = std::sync::LazyLock::new(|| {
        let object = empty_simple_namespace_instance(simple_namespace_type()?)?;
        let name = unsafe { pon_const_str(IMPLEMENTATION_NAME.as_ptr(), IMPLEMENTATION_NAME.len()) };
        if name.is_null() {
            return Err("failed to allocate sys.implementation.name".to_owned());
        }
        simple_namespace_store_attr(object, "name", name)?;
        let cache_tag_text = implementation_cache_tag();
        let cache_tag = unsafe { pon_const_str(cache_tag_text.as_ptr(), cache_tag_text.len()) };
        if cache_tag.is_null() {
            return Err("failed to allocate sys.implementation.cache_tag".to_owned());
        }
        simple_namespace_store_attr(object, "cache_tag", cache_tag)?;
        let version = version_info_object()?;
        simple_namespace_store_attr(object, "version", version)?;
        let hexversion = unsafe { pon_const_int(HEXVERSION) };
        if hexversion.is_null() {
            return Err("failed to allocate sys.implementation.hexversion".to_owned());
        }
        simple_namespace_store_attr(object, "hexversion", hexversion)?;
        Ok(object as usize)
    });
    let _ = ty;
    IMPLEMENTATION.clone().map(|object| (intern("implementation"), object as *mut PyObject))
}

fn simple_namespace_type() -> Result<*mut PyType, String> {
    static TYPE: std::sync::LazyLock<Result<usize, String>> = std::sync::LazyLock::new(|| {
        let namespace = crate::types::type_::new_namespace();
        if namespace.is_null() {
            return Err("failed to allocate types.SimpleNamespace namespace".to_owned());
        }
        class_str_attr(namespace, "__module__", "types")?;
        class_str_attr(namespace, "__doc__", "A simple attribute-based namespace.")?;
        let class =
            unsafe { crate::types::type_::build_class_from_namespace("SimpleNamespace", &[], namespace, &[]) };
        if class.is_null() {
            let detail = crate::thread_state::pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
            crate::thread_state::pon_err_clear();
            return Err(format!("failed to create types.SimpleNamespace: {detail}"));
        }
        unsafe {
            if (*class).ob_type.is_null() {
                (*class).ob_type = crate::abi::runtime_type_type().cast_const();
            }
            let ty = class.cast::<PyType>();
            (*ty).tp_new = Some(simple_namespace_new);
            (*ty).tp_repr = Some(simple_namespace_repr);
            (*ty).tp_str = Some(simple_namespace_repr);
        }
        Ok(class as usize)
    });
    TYPE.clone().map(|object| object as *mut PyType)
}

fn empty_simple_namespace_instance(ty: *mut PyType) -> Result<*mut PyObject, String> {
    let object = unsafe { crate::types::type_::type_new(ty, std::ptr::null_mut(), std::ptr::null_mut()) };
    if object.is_null() {
        let detail = crate::thread_state::pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
        crate::thread_state::pon_err_clear();
        Err(format!("failed to allocate types.SimpleNamespace instance: {detail}"))
    } else {
        Ok(object)
    }
}

fn simple_namespace_store_attr(object: *mut PyObject, name: &str, value: *mut PyObject) -> Result<(), String> {
    if value.is_null() {
        return Err(format!("refused to store NULL types.SimpleNamespace.{name}"));
    }
    let dict = unsafe { simple_namespace_dict(object)? };
    unsafe { (&mut *dict).set(intern(name), value) };
    Ok(())
}

unsafe fn simple_namespace_dict(object: *mut PyObject) -> Result<*mut crate::types::type_::PyClassDict, String> {
    if object.is_null() {
        return Err("types.SimpleNamespace instance is NULL".to_owned());
    }
    let instance = object.cast::<PyHeapInstance>();
    let mut dict = unsafe { (*instance).dict };
    if dict.is_null() {
        dict = crate::types::type_::new_namespace();
        if dict.is_null() {
            return Err("failed to allocate types.SimpleNamespace.__dict__".to_owned());
        }
        unsafe { (*instance).dict = dict };
    }
    Ok(dict)
}

unsafe extern "C" fn simple_namespace_new(
    cls: *mut PyType,
    args: *mut PyObject,
    kwargs: *mut PyObject,
) -> *mut PyObject {
    if cls.is_null() {
        return return_null_with_error("types.SimpleNamespace constructor received NULL type");
    }
    let positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
        Ok(args) => args,
        Err(message) => return return_null_with_error(message),
    };
    if positional.len() > 1 {
        return return_null_with_error("types.SimpleNamespace() takes at most 1 positional argument");
    }
    let object = match empty_simple_namespace_instance(cls) {
        Ok(object) => object,
        Err(message) => return return_null_with_error(message),
    };
    let mut pairs = Vec::new();
    if let Some(&source) = positional.first() {
        if unsafe { super::builtins_mod::collect_dict_update_pairs(source, &mut pairs) }.is_err() {
            return std::ptr::null_mut();
        }
    }
    if !kwargs.is_null() && unsafe { super::builtins_mod::collect_dict_update_pairs(kwargs, &mut pairs) }.is_err() {
        return std::ptr::null_mut();
    }
    let dict = match unsafe { simple_namespace_dict(object) } {
        Ok(dict) => dict,
        Err(message) => return return_null_with_error(message),
    };
    for pair in pairs.chunks_exact(2) {
        let key = crate::tag::untag_arg(pair[0]);
        let Some(name) = (unsafe { crate::types::type_::unicode_text(key) }) else {
            return return_null_with_error("types.SimpleNamespace keys must be strings");
        };
        unsafe { (&mut *dict).set(intern(name), pair[1]) };
    }
    object
}

unsafe extern "C" fn simple_namespace_repr(object: *mut PyObject) -> *mut PyObject {
    use std::fmt::Write as _;

    let text = if object.is_null() {
        "namespace()".to_owned()
    } else {
        let instance = object.cast::<PyHeapInstance>();
        let dict = unsafe { (*instance).dict };
        if dict.is_null() {
            "namespace()".to_owned()
        } else {
            let mut text = "namespace(".to_owned();
            for (index, (name_id, value)) in unsafe { (&*dict).iter() }.enumerate() {
                let separator = if index == 0 { "" } else { ", " };
                let name = crate::intern::resolve(name_id).unwrap_or_else(|| format!("<interned:{name_id}>"));
                let _ = write!(text, "{separator}{name}={}", super::builtins_mod::repr_text(value));
            }
            text.push(')');
            text
        }
    };
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
/// TypeError contract) and selects which compiled call-stack entry the frame
/// describes; the frame carries the whole captured call chain
/// (`abi::frame_chain_for_depth`), so `f_globals`/`f_lineno`/`f_code.co_name`
/// read the entry `depth` levels above the `_getframe` call and `f_back`
/// walks caller by caller to the module toplevel, then `None`.  A too-deep
/// `depth` clamps to the toplevel frame (CPython raises ValueError; loosened
/// here) and negative depths clamp to the current frame like CPython.
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
    crate::types::frame::synthesize_frame_object(crate::abi::frame_chain_for_depth(depth, sys_getframe as *const u8))
}

/// `sys.exit([status])`: raise `SystemExit(status)`.  Accepts zero or one
/// argument; `status` defaults to `None` (exit code 0).  The top-level runner
/// maps the `SystemExit` payload to the process exit status.
unsafe extern "C" fn sys_exit(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc > 1 {
        return return_null_with_error(format!("exit() takes at most 1 argument ({argc} given)"));
    }
    let code = if argc == 1 && !argv.is_null() {
        // SAFETY: One live argument slot per the arity check.
        unsafe { *argv }
    } else {
        std::ptr::null_mut()
    };
    crate::abi::exc::raise_system_exit(code)
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

/// `sys.getfilesystemencoding()`: `'utf-8'`.
///
/// CPython derives the filesystem encoding from the locale / PEP 529/540
/// machinery; on macOS (and every UTF-8-mode POSIX build) the answer is
/// `'utf-8'`, which is also the only encoding pon's fs surface actually
/// uses (`os.fsencode`/`os.fsdecode` are strict UTF-8 — `native/os.rs`).
/// `test.support.os_helper` calls it at import (:142/:145) to build the
/// `TESTFN_UNDECODABLE` probe.
unsafe extern "C" fn sys_getfilesystemencoding(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let _ = argv;
    if argc != 0 {
        return raise_type_error(&format!("sys.getfilesystemencoding() takes no arguments ({argc} given)"));
    }
    const ENCODING: &str = "utf-8";
    // SAFETY: Runtime allocation helper returns NULL with a diagnostic on failure.
    unsafe { pon_const_str(ENCODING.as_ptr(), ENCODING.len()) }
}

/// `sys.getfilesystemencodeerrors()`: `'surrogateescape'`.
///
/// The CPython POSIX contract (PEP 383): `os.fsdecode` maps undecodable
/// bytes to lone surrogates.  pon reports the same handler NAME — stdlib
/// readers branch on it — but pon str cannot carry lone surrogates, so the
/// builtin codec cores degrade a requested-but-unsupported
/// `'surrogateescape'` into the strict-mode `UnicodeDecodeError` instead
/// (documented divergence; see `native/codecs.rs::utf8_decode_core`).
/// `test.support.os_helper:145-147` consumes exactly that shape: its
/// `except UnicodeDecodeError` arm engages and `TESTFN_UNDECODABLE`
/// honestly stays `None` (CPython would bind a surrogate-carrying name).
unsafe extern "C" fn sys_getfilesystemencodeerrors(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let _ = argv;
    if argc != 0 {
        return raise_type_error(&format!("sys.getfilesystemencodeerrors() takes no arguments ({argc} given)"));
    }
    const ERRORS: &str = "surrogateescape";
    // SAFETY: Runtime allocation helper returns NULL with a diagnostic on failure.
    unsafe { pon_const_str(ERRORS.as_ptr(), ERRORS.len()) }
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

// ---------------------------------------------------------------------------
// sys.audit / sys.addaudithook (PEP 578)
//
// CPython's runtime audit system: hooks registered via `addaudithook`
// receive every audit event, and `sys.audit(event, *args)` raises events
// from Python code.  pon's compiled runtime FIRES NO BUILT-IN EVENTS
// (documented divergence: there are no import/open/compile/exec hook
// points to raise from, and `addaudithook` itself does not raise the
// `sys.addaudithook` event to existing hooks), so the served contract is
// exactly the user-visible half: `addaudithook` stores the callable,
// `audit` validates the event name, calls every stored hook with
// `(event, args_tuple)` — CPython's hook signature — and propagates the
// first hook exception.  With no hooks registered, `audit` is a no-op
// returning None; `test.test_cmd_line` reads `sys.audit` at import and
// `test.test_audit`'s hasattr gate expects both names.  Hooks are
// process-lifetime (CPython cannot remove them either); the raw pointers
// are rooted through [`gc_held_roots`] (the `_contextvars` pattern) so a
// stored hook is never swept.
// ---------------------------------------------------------------------------

/// Registered audit hooks in registration order (raw addresses; immortal,
/// rooted via [`gc_held_roots`]).
static AUDIT_HOOKS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// GC roots held by native `sys` state: the registered audit hooks.
/// Consumed by `crate::abi::collect` (the `_contextvars` pattern).
pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
    AUDIT_HOOKS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .iter()
        .map(|&address| address as *mut PyObject)
        .collect()
}

/// `sys.addaudithook(hook)`: stores the hook.  CPython performs no
/// callability check at registration (a non-callable fails at event time),
/// and neither does pon.
unsafe extern "C" fn sys_addaudithook(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.is_empty() {
        return raise_type_error("addaudithook() missing required argument 'hook' (pos 1)");
    }
    if args.len() > 1 {
        return raise_type_error(&format!("addaudithook() takes at most 1 argument ({} given)", args.len()));
    }
    AUDIT_HOOKS.lock().unwrap_or_else(|poison| poison.into_inner()).push(args[0] as usize);
    // SAFETY: Singleton fetch follows the NULL-sentinel contract.
    unsafe { crate::abi::pon_none() }
}

/// `sys.audit(event, *args)`: validates the str event name, then calls
/// every registered hook with `(event, args_tuple)`; the first hook
/// exception propagates, exactly CPython's dispatch.  No hooks: None.
unsafe extern "C" fn sys_audit(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    let Some(&event) = args.first() else {
        return raise_type_error("audit expected at least 1 argument, got 0");
    };
    let event_object = crate::tag::untag_arg(event);
    // SAFETY: Type probe on a live object (tagged immediates untagged above).
    if unsafe { crate::types::type_::unicode_text(event_object) }.is_none() {
        let type_name = if crate::tag::is_heap(event) {
            // SAFETY: Heap pointer with a live header after the tag check.
            unsafe { crate::types::dict::type_name(event) }.unwrap_or("object")
        } else {
            "int"
        };
        return raise_type_error(&format!("audit() argument 1 must be str, not {type_name}"));
    }
    let hooks: Vec<usize> = {
        let held = AUDIT_HOOKS.lock().unwrap_or_else(|poison| poison.into_inner());
        held.clone()
    };
    if hooks.is_empty() {
        // SAFETY: Singleton fetch follows the NULL-sentinel contract.
        return unsafe { crate::abi::pon_none() };
    }
    let mut rest: Vec<*mut PyObject> = args[1..].to_vec();
    // SAFETY: The slots are live objects per the call ABI (empty reads none).
    let args_tuple = unsafe { crate::abi::seq::pon_build_tuple(rest.as_mut_ptr(), rest.len()) };
    if args_tuple.is_null() {
        return core::ptr::null_mut();
    }
    for hook in hooks {
        let mut hook_argv = [event, args_tuple];
        // SAFETY: Call dispatch over a live callee and two live slots; a
        // NULL result carries the hook's raised exception.
        let result = unsafe { crate::abi::pon_call(hook as *mut PyObject, hook_argv.as_mut_ptr(), 2) };
        if result.is_null() {
            return core::ptr::null_mut();
        }
    }
    // SAFETY: Singleton fetch follows the NULL-sentinel contract.
    unsafe { crate::abi::pon_none() }
}

// ---------------------------------------------------------------------------
// sys.getsizeof
//
// IMPLEMENTATION-DEFINED SIZES (documented divergence): CPython returns
// its own object-layout byte counts (28 for a small int, 49 + length for
// a str, ...); pon returns ITS real fixed allocation — the tagged machine
// word for immediates (8), the type's `tp_basicsize` for heap objects.
// Out-of-line payloads (str/bytes buffers, list/dict tables, bigint
// limbs) are NOT accounted.  In-cohort consumers only need an int
// (`test.test_marshal` computes a `@support.bigmemtest(memuse=...)`
// decorator argument at class-body time); CPython-layout equality tests
// fail honestly, and corpus modules must never pin numeric sizes — only
// int-ness/positivity, which hold on both engines.  The optional
// `default` is CPython's fallback for objects that cannot report a size;
// every pon value has a sized type, so it is accepted and never engaged.
// ---------------------------------------------------------------------------

/// `sys.getsizeof(object, default=...)`: see the section comment.
unsafe extern "C" fn sys_getsizeof(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    let Some(&object) = args.first() else {
        return raise_type_error("getsizeof() missing required argument 'object' (pos 1)");
    };
    if args.len() > 2 {
        return raise_type_error(&format!("getsizeof expected at most 2 arguments, got {}", args.len()));
    }
    if !crate::tag::is_heap(object) {
        // A tagged immediate occupies exactly its machine word.
        return unsafe { pon_const_int(std::mem::size_of::<usize>() as i64) };
    }
    // SAFETY: Heap pointer with a live header per the tag check above.
    let ty = unsafe { (*object).ob_type };
    let size = if ty.is_null() {
        std::mem::size_of::<crate::object::PyObjectHeader>()
    } else {
        // SAFETY: Type pointers are live for the process lifetime.
        unsafe { (*ty).tp_basicsize }
    };
    // SAFETY: Integer boxing helper follows the NULL-sentinel contract.
    unsafe { pon_const_int(size as i64) }
}

// ---------------------------------------------------------------------------
// sys.path
//
// A REAL mutable list (stdlib mutates it: test modules append fixture
// directories, `pkgutil`/`zipimport` walk it), seeded once per process
// with the runtime's actual source-import search order.  The CLI prepends
// the script directory to `PONPATH` before runtime init, so script execution
// starts with that directory first like CPython; embedded/AoT runs expose the
// environment roots they actually search.  Later user insertions are visible
// to both Python readers and pon's source resolver, which snapshots this list
// before each source-module lookup and merges non-default entries ahead of
// installed-package roots.  The singleton list lives for the process
// (identity stable across `import sys` re-registration), exactly like the
// version_info instance above.
// ---------------------------------------------------------------------------

/// The `sys.path` singleton list over the runtime's search roots.
fn path_attr() -> Result<(u32, *mut PyObject), String> {
    static PATH: std::sync::LazyLock<Result<usize, String>> = std::sync::LazyLock::new(|| {
        let roots = crate::import::source_search_roots();
        let mut items: Vec<*mut PyObject> = Vec::with_capacity(roots.len());
        for root in &roots {
            let text = root.to_string_lossy();
            // SAFETY: String allocation helper follows the NULL-sentinel contract.
            let object = unsafe { pon_const_str(text.as_ptr(), text.len()) };
            if object.is_null() {
                return Err("failed to allocate a sys.path entry".to_owned());
            }
            items.push(object);
        }
        // SAFETY: List builder reads exactly `len` live slots.
        let list = unsafe { crate::abi::seq::pon_build_list(items.as_mut_ptr(), items.len()) };
        if list.is_null() {
            return Err("failed to allocate sys.path".to_owned());
        }
        Ok(list as usize)
    });
    PATH.clone().map(|object| (intern("path"), object as *mut PyObject))
}
// `sys._clear_type_descriptors(type)`.
//
// CPython 3.14 added this as a dataclasses-specific workaround for
// `@dataclass(slots=True)`: after synthesizing the replacement slotted class,
// it clears descriptor entries from the ORIGINAL class dict so lingering
// references to that namespace do not keep the old class alive. pon has no
// type-lookup cache here and never installs per-type `__weakref__`
// descriptors, so the honest minimal surface is a best-effort delete of the
// synthetic `__dict__` descriptor when present, a no-op miss for
// `__weakref__`, and a type-version bump so any cached class-dict reads
// re-resolve. `cls.__dict__` itself keeps working because type attribute
// reads are served by `descr::generic_get_attr`'s special case, not by this
// raw dict entry.
unsafe extern "C" fn sys_clear_type_descriptors(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 {
        return return_null_with_error(format!(
            "_clear_type_descriptors() takes exactly 1 argument ({argc} given)"
        ));
    }
    let ty = unsafe { call_args(argv, argc)[0] };
    if ty.is_null() || unsafe { !crate::types::type_::is_type_object(ty) } {
        return raise_type_error("argument must be a type");
    }
    let ty = ty.cast::<crate::object::PyType>();
    let dict = unsafe { (*ty).tp_dict.cast::<crate::types::type_::PyClassDict>() };
    if !dict.is_null() {
        unsafe {
            (&mut *dict).del(intern("__dict__"));
            (&mut *dict).del(intern("__weakref__"));
        }
    }
    crate::sync::type_modified(ty);
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::thread_state::{pon_err_clear, test_state_lock};

    fn init_runtime() {
        assert_eq!(unsafe { crate::abi::pon_runtime_init() }, 0);
        pon_err_clear();
    }

    #[test]
    fn clear_type_descriptors_removes_heap_type_dunder_dict_descriptor() {
        let _guard = test_state_lock();
        init_runtime();
        let namespace = crate::types::type_::new_namespace();
        assert!(!namespace.is_null());
        let object = crate::abi::runtime_global(intern("object")).expect("object type should exist");
        let cls = unsafe { crate::types::type_::build_class_from_namespace("SlotsProbe", &[object], namespace, &[]) };
        assert!(!cls.is_null());

        let ty = cls.cast::<crate::object::PyType>();
        let dict = unsafe { (*ty).tp_dict.cast::<crate::types::type_::PyClassDict>() };
        assert!(!dict.is_null());
        assert!(unsafe { (&*dict).get(intern("__dict__")) }.is_some());
        assert!(unsafe { (&*dict).get(intern("__weakref__")) }.is_none());

        let before = unsafe { (*ty).version() };
        let mut argv = [cls];
        let result = unsafe { sys_clear_type_descriptors(argv.as_mut_ptr(), argv.len()) };
        assert_eq!(result, unsafe { crate::abi::pon_none() });
        assert!(!crate::thread_state::pon_err_occurred());
        assert!(unsafe { (&*dict).get(intern("__dict__")) }.is_none());
        assert!(unsafe { (*ty).version() } > before);

        let result = unsafe { sys_clear_type_descriptors(argv.as_mut_ptr(), argv.len()) };
        assert_eq!(result, unsafe { crate::abi::pon_none() });
        assert!(!crate::thread_state::pon_err_occurred());
    }
}
