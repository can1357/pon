//! Native `sys` module seed for WS-IMPORT.

use num_traits::ToPrimitive;

use crate::abi::{pon_const_int, pon_const_str, pon_make_function, return_null_with_error};
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

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let attrs = [
        string_attr("version", "3.14.0 (pon)"),
        string_attr(
            "version_info",
            "sys.version_info(major=3, minor=14, micro=0, releaselevel='final', serial=0)",
        ),
        string_attr("implementation", "namespace(name='pon', version=sys.version_info(major=3, minor=14, micro=0, releaselevel='final', serial=0))"),
        int_attr("hexversion", 0x030e00f0),
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
        flags_attr(),
        hash_info_attr(),
        warnoptions_attr(),
        modules_attr(),
        builtin_module_names_attr(),
        function_attr("_getframe", sys_getframe),
        function_attr("intern", sys_intern),
        function_attr("exception", sys_exception),
        function_attr("exc_info", sys_exc_info),
        function_attr("excepthook", sys_excepthook),
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
    let object = super::builtins_mod::alloc_list(Vec::new());
    (!object.is_null())
        .then_some((intern("warnoptions"), object))
        .ok_or_else(|| "failed to allocate sys.warnoptions".to_owned())
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
