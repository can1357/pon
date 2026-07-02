//! Native `sys` module seed for WS-IMPORT.

use crate::abi::{pon_const_int, pon_const_str, pon_make_function, return_null_with_error};
use crate::intern::intern;
use crate::object::PyObject;

use super::install_module;

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
        string_attr("platform", std::env::consts::OS),
        string_attr("byteorder", if cfg!(target_endian = "little") { "little" } else { "big" }),
        string_attr("executable", "pon"),
        string_attr("prefix", ""),
        string_attr("base_prefix", ""),
        flags_attr(),
        warnoptions_attr(),
        modules_attr(),
        function_attr("_getframe", sys_getframe),
        function_attr("intern", sys_intern),
        function_attr("exception", sys_exception),
        function_attr("exc_info", sys_exc_info),
    ];
    install_module("sys", attrs.into_iter().collect::<Result<Vec<_>, _>>()?)
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate sys.{name}"))
}

/// `sys.modules` is the live import-registry dict owned by `crate::import`;
/// mutations through it (insert/replace/delete) steer later imports.
fn modules_attr() -> Result<(u32, *mut PyObject), String> {
    crate::import::sys_modules_dict().map(|object| (intern("modules"), object))
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
        "context_aware_warnings" | "debug" | "optimize" | "verbose" | "dev_mode" | "ignore_environment" => unsafe {
            pon_const_int(0)
        },
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
/// per call.  The optional `depth` argument is accepted and loosely honored:
/// it must be an `int` (mirroring CPython's TypeError contract) but every
/// depth observes the same synthesized current frame — no `f_back` chain
/// exists to walk.  `_collections_abc`'s PEP 667 probe
/// (`type(sys._getframe().f_locals)`) only needs the frame's `f_locals` type
/// identity, served by `crate::types::frame::frame_getattro`.
unsafe extern "C" fn sys_getframe(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc > 1 {
        return return_null_with_error(format!("_getframe() takes at most 1 argument ({argc} given)"));
    }
    if argc == 1 {
        if argv.is_null() {
            return return_null_with_error("argv pointer is null");
        }
        // SAFETY: `argv` carries `argc` argument slots per the call ABI.
        let depth = crate::tag::untag_arg(unsafe { *argv });
        if depth.is_null() {
            // Boxing a tagged immediate failed; the error is already recorded.
            return core::ptr::null_mut();
        }
        if unsafe { crate::types::int::to_bigint_including_bool(depth) }.is_none() {
            return return_null_with_error("_getframe() argument must be int");
        }
    }
    crate::types::frame::synthesize_frame_object()
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

/// `sys.exception()` (CPython 3.12+).
///
/// Returns the exception being handled or `None`.  pon keeps the caught
/// exception installed in `PonThreadState.current_exc` while an `except`
/// body runs (the same slot `except ... as e` binding and implicit
/// `__context__` chaining read), so the object-safe pending exception IS the
/// handled exception whenever compiled Python code can observe it: a call
/// only happens while no exception is propagating.
unsafe extern "C" fn sys_exception(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let _ = argv;
    if argc != 0 {
        return return_null_with_error(format!("exception() takes no arguments ({argc} given)"));
    }
    match crate::abi::exc::pending_exception_object() {
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
    let items = match crate::abi::exc::pending_exception_object() {
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
