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
        string_attr("platform", std::env::consts::OS),
        string_attr("executable", "pon"),
        string_attr("prefix", ""),
        string_attr("base_prefix", ""),
        string_attr("modules", "<sys.modules>"),
        function_attr("_getframe", sys_getframe),
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
