//! Native `os` module seed for WS-IMPORT.

use crate::abi::pon_const_str;
use crate::intern::intern;
use crate::object::PyObject;

use super::install_module;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let sep = if cfg!(windows) { "\\" } else { "/" };
    let linesep = if cfg!(windows) { "\r\n" } else { "\n" };
    let attrs = [
        string_attr("__name__", "os"),
        string_attr("name", os_name()),
        string_attr("sep", sep),
        string_attr("pathsep", if cfg!(windows) { ";" } else { ":" }),
        string_attr("linesep", linesep),
        string_attr("curdir", "."),
        string_attr("pardir", ".."),
    ];
    let mut attrs = attrs.into_iter().collect::<Result<Vec<_>, _>>()?;
    // SAFETY: Live builtin entry point with the runtime calling convention.
    let fspath = unsafe { crate::abi::pon_make_function(os_fspath as *const u8, 1, intern("fspath")) };
    if fspath.is_null() {
        return Err("failed to allocate os.fspath".to_owned());
    }
    attrs.push((intern("fspath"), fspath));
    install_module("os", attrs)
}

/// `os.fspath(path)`: str/bytes pass through unchanged; other objects defer
/// to their type's `__fspath__`; everything else raises CPython's TypeError.
unsafe extern "C" fn os_fspath(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 || argv.is_null() {
        return crate::abi::return_null_with_error("os.fspath expected one argument");
    }
    // SAFETY: One live argument slot per the check above.
    let path = unsafe { *argv };
    let raw = crate::tag::untag_arg(path);
    if !raw.is_null() && !crate::tag::is_small_int(raw) {
        // SAFETY: Heap pointer with a live header after the tag checks.
        if matches!(unsafe { crate::types::dict::type_name(raw) }, Some("str" | "bytes")) {
            return path;
        }
        // SAFETY: Live header per the checks above.
        let ty = unsafe { (*raw).ob_type.cast_mut() };
        let hook = unsafe { crate::descr::lookup_in_type(ty, intern("__fspath__")) };
        if !hook.is_null() {
            let bound = unsafe { crate::descr::descriptor_get(hook, raw, ty) };
            if bound.is_null() {
                return std::ptr::null_mut();
            }
            // SAFETY: Call helper follows the NULL-sentinel error contract.
            return unsafe { crate::abi::pon_call(bound, std::ptr::null_mut(), 0) };
        }
    }
    let display = if raw.is_null() {
        "NoneType"
    } else if crate::tag::is_small_int(raw) {
        "int"
    } else {
        // SAFETY: Heap pointer with a live header after the tag checks.
        unsafe { crate::types::dict::type_name(raw) }.unwrap_or("object")
    };
    let message = format!("expected str, bytes or os.PathLike object, not {display}");
    // SAFETY: Typed raise helper.
    unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

/// `os.path`: CPython's `os.py` publishes `sys.modules['os.path'] =
/// posixpath`; the native seed mirrors that aliasing lazily by resolving the
/// vendored `posixpath` source module on first import.  The importer then
/// binds it under both names and as the parent's `path` attribute.
pub(super) fn make_path_module() -> Result<*mut PyObject, String> {
    // SAFETY: Import entry point follows the NULL-sentinel error contract.
    let module = unsafe {
        crate::import::pon_import_name(intern(if cfg!(windows) { "ntpath" } else { "posixpath" }), std::ptr::null(), 0, 0)
    };
    if module.is_null() {
        return Err("failed to import posixpath for os.path".to_owned());
    }
    Ok(module)
}

fn os_name() -> &'static str {
    if cfg!(windows) {
        "nt"
    } else {
        "posix"
    }
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate os.{name}"))
}
