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
    // SAFETY: Live builtin entry points with the runtime calling convention.
    let fspath = unsafe { crate::abi::pon_make_function(os_fspath as *const u8, 1, intern("fspath")) };
    if fspath.is_null() {
        return Err("failed to allocate os.fspath".to_owned());
    }
    attrs.push((intern("fspath"), fspath));
    let stat = unsafe { crate::abi::pon_make_function(os_stat as *const u8, 1, intern("stat")) };
    if stat.is_null() {
        return Err("failed to allocate os.stat".to_owned());
    }
    attrs.push((intern("stat"), stat));
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

/// `os.stat_result` shape: only the fields the vendored stdlib consumes are
/// served (`linecache` reads `st_size`/`st_mtime`); unknown attributes raise
/// AttributeError so the next frontier is loud, not silently wrong.
#[repr(C)]
struct PyStatResult {
    ob_base: crate::object::PyObjectHeader,
    st_size: i64,
    st_mtime: f64,
    st_mode: u32,
}

fn stat_result_type() -> *mut crate::object::PyType {
    static STAT_TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        let mut ty = crate::object::PyType::new(std::ptr::null(), "os.stat_result", std::mem::size_of::<PyStatResult>());
        ty.tp_getattro = Some(stat_result_getattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    *STAT_TYPE as *mut crate::object::PyType
}

unsafe extern "C" fn stat_result_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let name = crate::tag::untag_arg(name);
    let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        crate::thread_state::pon_err_set("attribute name must be str");
        return std::ptr::null_mut();
    };
    let stat = object.cast::<PyStatResult>();
    match name_text {
        // SAFETY: Receivers of this getattro are PyStatResult allocations.
        "st_size" => unsafe { crate::abi::pon_const_int((*stat).st_size) },
        "st_mtime" => unsafe { crate::abi::number::pon_const_float((*stat).st_mtime) },
        "st_mode" => unsafe { crate::abi::pon_const_int(i64::from((*stat).st_mode)) },
        _ => unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
    }
}

/// `os.stat(path)` over `std::fs::metadata`: follows symlinks like CPython's
/// default. Missing/unreadable paths raise OSError (`linecache` catches it).
unsafe extern "C" fn os_stat(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 || argv.is_null() {
        return crate::abi::return_null_with_error("os.stat expected one argument");
    }
    // SAFETY: One live argument slot per the check above.
    let path = crate::tag::untag_arg(unsafe { *argv });
    let Some(path_text) = (unsafe { crate::types::type_::unicode_text(path) }) else {
        let message = "os.stat() path must be str";
        // SAFETY: Typed raise helper.
        return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    };
    match std::fs::metadata(path_text) {
        Ok(metadata) => {
            let mtime = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0.0, |duration| duration.as_secs_f64());
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode()
            };
            #[cfg(not(unix))]
            let mode = 0u32;
            Box::into_raw(Box::new(PyStatResult {
                ob_base: crate::object::PyObjectHeader::new(stat_result_type()),
                st_size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
                st_mtime: mtime,
                st_mode: mode,
            }))
            .cast::<PyObject>()
        }
        Err(error) => {
            let message = format!("[Errno {}] {}: '{}'", error.raw_os_error().unwrap_or(2), error, path_text);
            // SAFETY: Typed raise helper.
            unsafe { crate::abi::exc::pon_raise_os_error(message.as_ptr(), message.len()) }
        }
    }
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
