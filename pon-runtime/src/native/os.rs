//! Native `os` module seed for WS-IMPORT.

use crate::abi::pon_const_str;
use crate::intern::intern;
use crate::object::PyObject;
use crate::types::exc::ExceptionKind;

use num_traits::ToPrimitive;

use super::install_module;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    install_module("os", build_attrs("os")?)
}

/// Attr set shared by the curated `os` and `posix` modules.
///
/// On POSIX hosts CPython's `os.py` re-exports the C `posix` module wholesale
/// (`from posix import *`), so both names must serve one surface; `posix.rs`
/// installs this same set under the other name.
pub(super) fn build_attrs(module: &'static str) -> Result<Vec<(u32, *mut PyObject)>, String> {
    let sep = if cfg!(windows) { "\\" } else { "/" };
    let linesep = if cfg!(windows) { "\r\n" } else { "\n" };
    let attrs = [
        string_attr(module, "__name__", module),
        string_attr(module, "name", os_name()),
        string_attr(module, "sep", sep),
        string_attr(module, "pathsep", if cfg!(windows) { ";" } else { ":" }),
        string_attr(module, "linesep", linesep),
        string_attr(module, "curdir", "."),
        string_attr(module, "pardir", ".."),
    ];
    let mut attrs = attrs.into_iter().collect::<Result<Vec<_>, _>>()?;
    for &(name, value) in [OPEN_FLAGS, ACCESS_FLAGS].into_iter().flatten() {
        // SAFETY: Integer boxing helper; NULL is checked below.
        let boxed = unsafe { crate::abi::pon_const_int(i64::from(value)) };
        if boxed.is_null() {
            return Err(format!("failed to allocate {module}.{name}"));
        }
        attrs.push((intern(name), boxed));
    }
    attrs.push((intern("environ"), environ_snapshot(module)?));
    // SAFETY: Live builtin entry points with the runtime calling convention.
    let fspath = unsafe { crate::abi::pon_make_function(os_fspath as *const u8, 1, intern("fspath")) };
    if fspath.is_null() {
        return Err(format!("failed to allocate {module}.fspath"));
    }
    attrs.push((intern("fspath"), fspath));
    let stat = unsafe { crate::abi::pon_make_function(os_stat as *const u8, 1, intern("stat")) };
    if stat.is_null() {
        return Err(format!("failed to allocate {module}.stat"));
    }
    attrs.push((intern("stat"), stat));
    // `random` imports `urandom` at module top for its default seeding path.
    let urandom = unsafe { crate::abi::pon_make_function(os_urandom as *const u8, 1, intern("urandom")) };
    if urandom.is_null() {
        return Err(format!("failed to allocate {module}.urandom"));
    }
    attrs.push((intern("urandom"), urandom));
    // `shutil` and `pathlib._os` probe `os.stat_result` attributes at import
    // time (`hasattr(os.stat_result, 'st_file_attributes')`), so the native
    // result type is published like CPython's structseq class.
    attrs.push((intern("stat_result"), stat_result_type().cast::<PyObject>()));
    // POSIX fd/path syscall surface shared with `posix` (CPython's `os.py`
    // re-exports these names from the C `posix` module wholesale).
    for &(name, entry, arity) in SYSCALL_FUNCTIONS {
        // SAFETY: Live builtin entry points with the runtime calling convention.
        let function = unsafe { crate::abi::pon_make_function(entry as *const u8, arity, intern(name)) };
        if function.is_null() {
            return Err(format!("failed to allocate {module}.{name}"));
        }
        attrs.push((intern(name), function));
    }
    // `terminal_size` is defined by CPython's C `posix` module, so both
    // names serve the shared class object (see the section comment for why
    // `get_terminal_size` itself stays absent).
    attrs.push((intern("terminal_size"), terminal_size_class()?));
    if module == "os" {
        // `os.py`-level surface that CPython does NOT re-export into `posix`.
        //
        // The empty capability sets are the honest non-fd contract: pon's
        // syscall wrappers implement no `dir_fd`/`fd`/`follow_symlinks`
        // variants, so membership probes (`os.stat in
        // os.supports_follow_symlinks` in tempfile, `{os.open, ...} <=
        // os.supports_dir_fd` in shutil) answer False and callers take their
        // portable fallback paths instead of the fd-relative ones.  Plain
        // mutable sets, exactly CPython's `os.py` (`supports_dir_fd = set()`
        // populated per-platform); an empty frozenset would flunk
        // `type(os.supports_dir_fd)` probes for no benefit.
        for name in ["supports_dir_fd", "supports_fd", "supports_follow_symlinks"] {
            let mut entries: Vec<*mut PyObject> = Vec::new();
            // SAFETY: A zero-element build reads nothing through the pointer.
            let set = unsafe { crate::abi::map::pon_build_set(entries.as_mut_ptr(), 0) };
            if set.is_null() {
                return Err(format!("failed to allocate {module}.{name}"));
            }
            attrs.push((intern(name), set));
        }
        // CPython defines `_get_exports_list` in `os.py` itself (never
        // re-exported into `posix`); `socket.py` consumes it at module body:
        // `__all__.extend(os._get_exports_list(_socket))`.
        // SAFETY: Live builtin entry point with the runtime calling convention.
        let exports_list = unsafe {
            crate::abi::pon_make_function(os_get_exports_list as *const u8, 1, intern("_get_exports_list"))
        };
        if exports_list.is_null() {
            return Err(format!("failed to allocate {module}._get_exports_list"));
        }
        attrs.push((intern("_get_exports_list"), exports_list));
        attrs.push((intern("PathLike"), pathlike_class()?));
    }
    Ok(attrs)
}

/// `os._get_exports_list(module)`: CPython os.py's own helper, served
/// natively because pon's `os` is a curated seed rather than the source
/// module.  `list(module.__all__)` when the module defines `__all__`, else
/// the sorted non-underscore namespace names — exactly os.py's
/// `[n for n in dir(module) if n[0] != '_']`.
unsafe extern "C" fn os_get_exports_list(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 || argv.is_null() {
        return crate::abi::return_null_with_error("os._get_exports_list expected one argument");
    }
    // SAFETY: One live argument slot per the check above.
    let module = crate::tag::untag_arg(unsafe { *argv });
    // `__all__` arm: any iterable, materialized as a fresh list (CPython's
    // `list(module.__all__)`).
    if let Some(all) = unsafe { super::builtins_batch::try_get_attr(module, "__all__") } {
        return match super::builtins_batch::collect_iterable(all) {
            // SAFETY: List builder reads exactly `len` live slots.
            Ok(mut values) => unsafe { crate::abi::seq::pon_build_list(values.as_mut_ptr(), values.len()) },
            // SAFETY: Typed raise helper.
            Err(message) => unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) },
        };
    }
    // `dir()` fallback arm: modules enumerate their registered namespace
    // dict (the `builtin_dir` module arm); anything else walks the MRO.
    let names = match crate::import::module_namespace_for_object(module) {
        Some(Ok(namespace)) => match unsafe { super::builtins_batch::names_from_mapping(namespace) } {
            Ok(names) => names,
            Err(message) => return crate::abi::return_null_with_error(message),
        },
        Some(Err(message)) => return crate::abi::return_null_with_error(message),
        None => super::builtins_batch::names_for_object(module),
    };
    let mut names: Vec<String> = names.into_iter().filter(|name| !name.starts_with('_')).collect();
    names.sort();
    names.dedup();
    super::builtins_batch::build_str_list(names)
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

/// `os.urandom(size)`: `size` cryptographically random bytes from the OS
/// entropy source (`getentropy(2)`, chunked at its 256-byte call limit).
unsafe extern "C" fn os_urandom(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 || argv.is_null() {
        return crate::abi::return_null_with_error("os.urandom expected one argument");
    }
    // SAFETY: One live argument slot per the check above.
    let size = crate::tag::untag_arg(unsafe { *argv });
    // SAFETY: Type probe tolerates any live object.
    let Some(size) = (unsafe { crate::types::int::to_bigint_including_bool(size) }) else {
        let message = "os.urandom expected an int argument";
        // SAFETY: Typed raise helper.
        return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    };
    use num_traits::{Signed, ToPrimitive};
    if size.is_negative() {
        let message = "negative argument not allowed";
        // SAFETY: Typed raise helper.
        return unsafe { crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) };
    }
    let Some(size) = size.to_usize() else {
        let message = "os.urandom size out of range";
        // SAFETY: Typed raise helper.
        return unsafe { crate::abi::exc::pon_raise_os_error(message.as_ptr(), message.len()) };
    };
    let mut bytes = vec![0u8; size];
    for chunk in bytes.chunks_mut(256) {
        // SAFETY: `chunk` is a live writable buffer of the passed length.
        if unsafe { libc::getentropy(chunk.as_mut_ptr().cast(), chunk.len()) } != 0 {
            let message = "getentropy failed";
            // SAFETY: Typed raise helper.
            return unsafe { crate::abi::exc::pon_raise_os_error(message.as_ptr(), message.len()) };
        }
    }
    // SAFETY: Runtime allocation helper; NULL on failure with the error set.
    unsafe { crate::abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) }
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
        let mut ty = crate::object::PyType::new(
            crate::abi::runtime_type_type().cast_const(),
            "os.stat_result",
            std::mem::size_of::<PyStatResult>(),
        );
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
        Ok(metadata) => stat_result_object(&metadata),
        Err(error) => raise_errno(error.raw_os_error().unwrap_or(libc::EIO), Some(path_text)),
    }
}

/// Boxes host metadata as the `os.stat_result` shape shared by `os.stat`
/// and `os.lstat`.
fn stat_result_object(metadata: &std::fs::Metadata) -> *mut PyObject {
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

fn string_attr(module: &str, name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate {module}.{name}"))
}

/// `open(2)` flag constants shared by macOS and Linux (errno.rs's
/// portable-POSIX policy), sorted by name. Values come from libc, so they
/// always match the host CPython's.
const OPEN_FLAGS: &[(&str, i32)] = &[
    ("O_ACCMODE", libc::O_ACCMODE),
    ("O_APPEND", libc::O_APPEND),
    ("O_ASYNC", libc::O_ASYNC),
    ("O_CLOEXEC", libc::O_CLOEXEC),
    ("O_CREAT", libc::O_CREAT),
    ("O_DIRECTORY", libc::O_DIRECTORY),
    ("O_DSYNC", libc::O_DSYNC),
    ("O_EXCL", libc::O_EXCL),
    ("O_NDELAY", libc::O_NDELAY),
    ("O_NOCTTY", libc::O_NOCTTY),
    ("O_NOFOLLOW", libc::O_NOFOLLOW),
    ("O_NONBLOCK", libc::O_NONBLOCK),
    ("O_RDONLY", libc::O_RDONLY),
    ("O_RDWR", libc::O_RDWR),
    ("O_SYNC", libc::O_SYNC),
    ("O_TRUNC", libc::O_TRUNC),
    ("O_WRONLY", libc::O_WRONLY),
];

/// `os.access(2)` mode constants (`shutil.which`'s default `mode` argument
/// is evaluated at module body: `mode=os.F_OK | os.X_OK`).
const ACCESS_FLAGS: &[(&str, i32)] = &[
    ("F_OK", libc::F_OK),
    ("R_OK", libc::R_OK),
    ("W_OK", libc::W_OK),
    ("X_OK", libc::X_OK),
];

/// Snapshot of the process environment as a plain str->str dict.
///
/// CPython's `os.environ` is a live `os._Environ` mapping whose writes call
/// `putenv`/`unsetenv`; this seed is read-focused — mutating the dict never
/// writes back to the process environment, and the snapshot is taken when the
/// module is created. Non-UTF-8 entries are decoded lossily rather than with
/// CPython's `surrogateescape`.
fn environ_snapshot(module: &str) -> Result<*mut PyObject, String> {
    let mut pairs: Vec<*mut PyObject> = Vec::new();
    for (key, value) in std::env::vars_os() {
        let key = key.to_string_lossy();
        let value = value.to_string_lossy();
        // SAFETY: String allocation helpers copy the bytes; NULL is checked below.
        let key_obj = unsafe { pon_const_str(key.as_ptr(), key.len()) };
        let value_obj = unsafe { pon_const_str(value.as_ptr(), value.len()) };
        if key_obj.is_null() || value_obj.is_null() {
            return Err(format!("failed to allocate {module}.environ entry"));
        }
        pairs.push(key_obj);
        pairs.push(value_obj);
    }
    let pair_count = pairs.len() / 2;
    // SAFETY: `pairs` holds `pair_count` live key/value pairs.
    let environ = unsafe { crate::abi::map::pon_build_map(pairs.as_mut_ptr(), pair_count) };
    if environ.is_null() {
        return Err(format!("failed to allocate {module}.environ"));
    }
    Ok(environ)
}

// ---------------------------------------------------------------------------
// POSIX syscall surface: open/close/read/write/unlink/rmdir/lstat plus the
// scandir frontier stub.  Raw libc calls over the same process fd space the
// `_io` native files wrap (`File::from_raw_fd`), with errno mapped onto
// CPython's OSError subclass hierarchy (PEP 3151).

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

/// Name / entry / arity rows consumed by [`build_attrs`].  `open` and `lstat`
/// are variadic: `open` has an optional `mode` positional, and both accept a
/// keyword-only `dir_fd` that the native keyword binder flattens into a
/// trailing positional None slot.
const SYSCALL_FUNCTIONS: &[(&str, BuiltinFn, usize)] = &[
    ("close", os_close, 1),
    ("getcwd", os_getcwd, 0),
    ("lstat", os_lstat, crate::native::builtins_mod::VARIADIC_ARITY),
    ("open", os_open, crate::native::builtins_mod::VARIADIC_ARITY),
    ("read", os_read, 2),
    ("readlink", os_readlink, 1),
    ("rmdir", os_rmdir, 1),
    ("scandir", os_scandir, 1),
    ("unlink", os_unlink, 1),
    ("write", os_write, 2),
];

/// `os.getcwd()` over `std::env::current_dir` (`sysconfig` calls it at
/// module scope via `_safe_realpath(os.getcwd())`).  Non-UTF-8 components
/// are decoded lossily rather than with CPython's `surrogateescape`.
unsafe extern "C" fn os_getcwd(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    match std::env::current_dir() {
        Ok(path) => {
            let text = path.to_string_lossy();
            // SAFETY: String allocation helper follows the NULL-sentinel contract.
            unsafe { pon_const_str(text.as_ptr(), text.len()) }
        }
        Err(error) => raise_errno(error.raw_os_error().unwrap_or(libc::EIO), None),
    }
}

/// `os.readlink(path)` over `std::fs::read_link` (`posixpath.realpath`'s
/// symlink resolution, reached from `sysconfig._safe_realpath`).  Non-link
/// paths surface the host errno (EINVAL) like `readlink(2)`.
unsafe extern "C" fn os_readlink(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 1 {
        return crate::abi::return_null_with_error("os.readlink expected one argument");
    }
    let path = match path_arg(args[0], "readlink") {
        Ok(path) => path,
        Err(error) => return error,
    };
    match std::fs::read_link(&path) {
        Ok(target) => {
            let text = target.to_string_lossy();
            // SAFETY: String allocation helper follows the NULL-sentinel contract.
            unsafe { pon_const_str(text.as_ptr(), text.len()) }
        }
        Err(error) => raise_errno(error.raw_os_error().unwrap_or(libc::EIO), Some(&path)),
    }
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

/// `int`-typed argument (bool included, like CPython's implicit acceptance).
fn int_arg(object: *mut PyObject, what: &str) -> Result<i64, *mut PyObject> {
    if crate::tag::is_small_int(object) {
        return Ok(crate::tag::untag_small_int(object));
    }
    // SAFETY: Non-immediate pointers are boxed objects; conversion type-checks.
    match unsafe { crate::types::int::to_bigint_including_bool(object) } {
        Some(value) => value.to_i64().ok_or_else(|| {
            crate::abi::exc::raise_kind_error_text(
                ExceptionKind::OverflowError,
                &format!("{what} is too large to fit in a C integer"),
            )
        }),
        None => Err(crate::abi::exc::raise_kind_error_text(
            ExceptionKind::TypeError,
            &format!("{what} must be an integer"),
        )),
    }
}

/// Path argument: str passes through, other objects defer to `__fspath__`
/// (so `pathlib.Path` works).  Divergence: CPython also accepts `bytes`
/// paths; pon's path surface is str-only and raises the fspath TypeError.
fn path_arg(object: *mut PyObject, what: &str) -> Result<String, *mut PyObject> {
    let raw = crate::tag::untag_arg(object);
    if !raw.is_null() && !crate::tag::is_small_int(raw) {
        // SAFETY: Heap pointer with a live header after the tag checks.
        if let Some(text) = unsafe { crate::types::type_::unicode_text(raw) } {
            return Ok(text.to_owned());
        }
        // SAFETY: Live header per the checks above.
        let ty = unsafe { (*raw).ob_type.cast_mut() };
        let hook = unsafe { crate::descr::lookup_in_type(ty, intern("__fspath__")) };
        if !hook.is_null() {
            let bound = unsafe { crate::descr::descriptor_get(hook, raw, ty) };
            if bound.is_null() {
                return Err(std::ptr::null_mut());
            }
            // SAFETY: Call helper follows the NULL-sentinel error contract.
            let result = unsafe { crate::abi::pon_call(bound, std::ptr::null_mut(), 0) };
            if result.is_null() {
                return Err(std::ptr::null_mut());
            }
            let result = crate::tag::untag_arg(result);
            if !result.is_null() && !crate::tag::is_small_int(result) {
                // SAFETY: Boxed pointer per the checks above.
                if let Some(text) = unsafe { crate::types::type_::unicode_text(result) } {
                    return Ok(text.to_owned());
                }
            }
        }
    }
    Err(crate::abi::exc::raise_kind_error_text(
        ExceptionKind::TypeError,
        &format!("{what}: path should be a str or an os.PathLike object"),
    ))
}

/// NUL-checked C path, matching CPython's embedded-NUL ValueError.
fn c_path(path: &str) -> Result<std::ffi::CString, *mut PyObject> {
    std::ffi::CString::new(path)
        .map_err(|_| crate::abi::exc::raise_kind_error_text(ExceptionKind::ValueError, "embedded null byte"))
}

/// Optional trailing argument: absent and None (the native keyword binder
/// fills absent slots with None) both read as "not supplied".
fn optional_arg(args: &[*mut PyObject], index: usize) -> Option<*mut PyObject> {
    let value = args.get(index).copied()?;
    if value.is_null() {
        return None;
    }
    let raw = crate::tag::untag_arg(value);
    if raw.is_null() || crate::tag::is_small_int(raw) {
        return Some(value);
    }
    // SAFETY: Heap pointer with a live header after the tag checks.
    if unsafe { crate::types::dict::type_name(raw) } == Some("NoneType") {
        return None;
    }
    Some(value)
}

/// Raises the CPython OSError subclass for `errno` (PEP 3151) with the
/// `[Errno N] strerror` message shape and optional filename context.
fn raise_errno(errno: i32, path: Option<&str>) -> *mut PyObject {
    let kind = match errno {
        libc::EEXIST => ExceptionKind::FileExistsError,
        libc::ENOENT => ExceptionKind::FileNotFoundError,
        libc::EISDIR => ExceptionKind::IsADirectoryError,
        libc::ENOTDIR => ExceptionKind::NotADirectoryError,
        libc::EACCES | libc::EPERM => ExceptionKind::PermissionError,
        libc::EINTR => ExceptionKind::InterruptedError,
        libc::EPIPE => ExceptionKind::BrokenPipeError,
        libc::ECHILD => ExceptionKind::ChildProcessError,
        libc::ESRCH => ExceptionKind::ProcessLookupError,
        libc::EAGAIN => ExceptionKind::BlockingIOError,
        libc::ETIMEDOUT => ExceptionKind::TimeoutError,
        libc::ECONNABORTED => ExceptionKind::ConnectionAbortedError,
        libc::ECONNREFUSED => ExceptionKind::ConnectionRefusedError,
        libc::ECONNRESET => ExceptionKind::ConnectionResetError,
        _ => ExceptionKind::OSError,
    };
    // SAFETY: `strerror` returns a NUL-terminated entry of the static
    // message table; the text is copied before any other libc call.
    let detail = unsafe { std::ffi::CStr::from_ptr(libc::strerror(errno)) }
        .to_string_lossy()
        .into_owned();
    let message = match path {
        Some(path) => format!("[Errno {errno}] {detail}: '{path}'"),
        None => format!("[Errno {errno}] {detail}"),
    };
    crate::abi::exc::raise_kind_error_text(kind, &message)
}

fn last_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}

/// Honest refusal for the keyword-only fd-relative parameters: CPython
/// raises NotImplementedError when `dir_fd` is unavailable on a platform,
/// and pon's capability sets (`os.supports_dir_fd`) are empty.
fn reject_dir_fd(args: &[*mut PyObject], index: usize, what: &str) -> Result<(), *mut PyObject> {
    if optional_arg(args, index).is_none() {
        return Ok(());
    }
    Err(crate::abi::exc::raise_kind_error_text(
        ExceptionKind::NotImplementedError,
        &format!("{what}: dir_fd unavailable on this platform"),
    ))
}

/// `os.open(path, flags, mode=0o777, *, dir_fd=None)` over `open(2)`;
/// returns the raw fd as int.
unsafe extern "C" fn os_open(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if !(2..=4).contains(&args.len()) {
        let message = "os.open expected 2 to 3 arguments (path, flags, mode=0o777)";
        return crate::abi::return_null_with_error(message);
    }
    let path = match path_arg(args[0], "open") {
        Ok(path) => path,
        Err(error) => return error,
    };
    let flags = match int_arg(args[1], "open flags") {
        Ok(flags) => flags,
        Err(error) => return error,
    };
    let mode = match optional_arg(args, 2).map(|object| int_arg(object, "open mode")) {
        None => 0o777,
        Some(Ok(mode)) => mode,
        Some(Err(error)) => return error,
    };
    if let Err(error) = reject_dir_fd(args, 3, "open") {
        return error;
    }
    let c_path = match c_path(&path) {
        Ok(c_path) => c_path,
        Err(error) => return error,
    };
    // SAFETY: `c_path` is NUL-terminated; the variadic mode argument uses the
    // default-promoted c_uint width `open(2)` expects.
    let fd = unsafe { libc::open(c_path.as_ptr(), flags as libc::c_int, mode as libc::c_uint) };
    if fd < 0 {
        return raise_errno(last_errno(), Some(&path));
    }
    // SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
    unsafe { crate::abi::pon_const_int(i64::from(fd)) }
}

/// `os.close(fd)` over `close(2)`.
unsafe extern "C" fn os_close(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 1 {
        return crate::abi::return_null_with_error("os.close expected one argument");
    }
    let fd = match int_arg(args[0], "close fd") {
        Ok(fd) => fd,
        Err(error) => return error,
    };
    // SAFETY: Plain syscall; the fd is validated by the kernel.
    if unsafe { libc::close(fd as libc::c_int) } < 0 {
        return raise_errno(last_errno(), None);
    }
    // SAFETY: Singleton accessor.
    unsafe { crate::abi::pon_none() }
}

/// `os.read(fd, n)` over `read(2)`: at most `n` bytes as a bytes object.
unsafe extern "C" fn os_read(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 2 {
        return crate::abi::return_null_with_error("os.read expected two arguments");
    }
    let fd = match int_arg(args[0], "read fd") {
        Ok(fd) => fd,
        Err(error) => return error,
    };
    let size = match int_arg(args[1], "read size") {
        Ok(size) => size,
        Err(error) => return error,
    };
    if size < 0 {
        // CPython surfaces a negative length as EINVAL from the syscall layer.
        return raise_errno(libc::EINVAL, None);
    }
    let mut buffer = vec![0u8; size as usize];
    // SAFETY: `buffer` owns `size` writable bytes for the syscall to fill.
    let count = unsafe { libc::read(fd as libc::c_int, buffer.as_mut_ptr().cast(), buffer.len()) };
    if count < 0 {
        return raise_errno(last_errno(), None);
    }
    // SAFETY: The syscall wrote `count` bytes; allocation copies them.
    unsafe { crate::abi::str_::pon_const_bytes(buffer.as_ptr(), count as usize) }
}

/// `os.write(fd, data)` over `write(2)`: returns the byte count written.
unsafe extern "C" fn os_write(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 2 {
        return crate::abi::return_null_with_error("os.write expected two arguments");
    }
    let fd = match int_arg(args[0], "write fd") {
        Ok(fd) => fd,
        Err(error) => return error,
    };
    let data = crate::tag::untag_arg(args[1]);
    let Some(payload) = bytes_payload(data) else {
        return crate::abi::exc::raise_kind_error_text(
            ExceptionKind::TypeError,
            "a bytes-like object is required",
        );
    };
    // SAFETY: `payload` borrows live object bytes for the syscall to read.
    let count = unsafe { libc::write(fd as libc::c_int, payload.as_ptr().cast(), payload.len()) };
    if count < 0 {
        return raise_errno(last_errno(), None);
    }
    // SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
    unsafe { crate::abi::pon_const_int(count as i64) }
}

/// Borrows a bytes/bytearray payload; `None` for other types.
fn bytes_payload<'a>(object: *mut PyObject) -> Option<&'a [u8]> {
    if object.is_null() || crate::tag::is_small_int(object) {
        return None;
    }
    // SAFETY: Heap pointer with a live header after the tag checks.
    let ty = unsafe { (*object).ob_type };
    if crate::types::bytes_::is_bytes_type(ty) {
        // SAFETY: The type check proved PyBytes layout.
        Some(unsafe { (*object.cast::<crate::types::bytes_::PyBytes>()).as_slice() })
    } else if crate::types::bytearray_::is_bytearray_type(ty) {
        // SAFETY: The type check proved PyByteArray layout.
        Some(unsafe { (*object.cast::<crate::types::bytearray_::PyByteArray>()).as_slice() })
    } else {
        None
    }
}

/// `os.unlink(path)` over `unlink(2)`.
unsafe extern "C" fn os_unlink(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 1 {
        return crate::abi::return_null_with_error("os.unlink expected one argument");
    }
    let path = match path_arg(args[0], "unlink") {
        Ok(path) => path,
        Err(error) => return error,
    };
    let c_path = match c_path(&path) {
        Ok(c_path) => c_path,
        Err(error) => return error,
    };
    // SAFETY: `c_path` is NUL-terminated.
    if unsafe { libc::unlink(c_path.as_ptr()) } < 0 {
        return raise_errno(last_errno(), Some(&path));
    }
    // SAFETY: Singleton accessor.
    unsafe { crate::abi::pon_none() }
}

/// `os.rmdir(path)` over `rmdir(2)`.
unsafe extern "C" fn os_rmdir(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 1 {
        return crate::abi::return_null_with_error("os.rmdir expected one argument");
    }
    let path = match path_arg(args[0], "rmdir") {
        Ok(path) => path,
        Err(error) => return error,
    };
    let c_path = match c_path(&path) {
        Ok(c_path) => c_path,
        Err(error) => return error,
    };
    // SAFETY: `c_path` is NUL-terminated.
    if unsafe { libc::rmdir(c_path.as_ptr()) } < 0 {
        return raise_errno(last_errno(), Some(&path));
    }
    // SAFETY: Singleton accessor.
    unsafe { crate::abi::pon_none() }
}

/// `os.lstat(path, *, dir_fd=None)` over `symlink_metadata` (never follows
/// the final symlink, exactly `lstat(2)`); `posixpath.lexists` catches the
/// OSError for missing paths.
unsafe extern "C" fn os_lstat(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if !(1..=2).contains(&args.len()) {
        return crate::abi::return_null_with_error("os.lstat expected one argument");
    }
    let path = match path_arg(args[0], "lstat") {
        Ok(path) => path,
        Err(error) => return error,
    };
    if let Err(error) = reject_dir_fd(args, 1, "lstat") {
        return error;
    }
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) => stat_result_object(&metadata),
        Err(error) => raise_errno(error.raw_os_error().unwrap_or(libc::EIO), Some(&path)),
    }
}

/// `os.scandir` frontier stub.  Deliberately a loud NotImplementedError, not
/// the OSError the successor spec sketched: `glob`/`shutil` wrap scandir in
/// `except OSError` and would silently degrade to wrong (empty) listings —
/// the module's discipline is a loud frontier, never silently wrong.
unsafe extern "C" fn os_scandir(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(
        ExceptionKind::NotImplementedError,
        "os.scandir is not implemented in pon",
    )
}

// ---------------------------------------------------------------------------
// os.PathLike
//
// CPython defines `PathLike` in `os.py` as an ABC (metaclass `abc.ABCMeta`)
// whose `__subclasshook__` answers "does the class implement `__fspath__`?".
// The native seed cannot construct it through the `abc` module: `os` is one
// of the frozen EAGER_MODULES, registered during runtime init before any
// source module (like the vendored `abc.py`) can be imported.  The same
// contract is served structurally instead: a dedicated metaclass heap type
// carries native `__instancecheck__`/`__subclasscheck__` hooks probing
// `__fspath__` on the candidate's MRO — `descr::isinstance`/`issubclass`
// dispatch through exactly those metaclass hooks, the same path a
// Python-level ABCMeta override takes.  Both classes are built by
// `build_class_from_namespace`, the machinery behind `class` statements.
//
// Documented divergences from CPython:
// * `type(os.PathLike)` is the private `os._PathLikeMeta`, not `abc.ABCMeta`,
//   and the ABC registry API (`PathLike.register`) does not exist.
// * Instantiating `os.PathLike()` is not blocked (no abstractmethod
//   machinery); CPython raises TypeError.  Calling the inherited
//   `__fspath__` raises NotImplementedError like CPython's abstract body.

fn pathlike_class() -> Result<*mut PyObject, String> {
    static CLASS: std::sync::LazyLock<Result<usize, String>> =
        std::sync::LazyLock::new(|| build_pathlike_class().map(|class| class as usize));
    CLASS.clone().map(|class| class as *mut PyObject)
}

fn class_str_attr(namespace: *mut crate::types::type_::PyClassDict, name: &str, value: &str) -> Result<(), String> {
    // SAFETY: String allocation helper; NULL is checked below.
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    if object.is_null() {
        return Err(format!("failed to allocate os.PathLike attribute '{name}'"));
    }
    // SAFETY: The caller passes a live namespace box.
    unsafe { (&mut *namespace).set(intern(name), object) };
    Ok(())
}

fn class_function_attr(
    namespace: *mut crate::types::type_::PyClassDict,
    name: &str,
    entry: BuiltinFn,
) -> Result<(), String> {
    // SAFETY: Live builtin entry point with the runtime calling convention.
    let function = unsafe {
        crate::abi::pon_make_function(entry as *const u8, crate::native::builtins_mod::VARIADIC_ARITY, intern(name))
    };
    if function.is_null() {
        return Err(format!("failed to allocate os.PathLike method '{name}'"));
    }
    // SAFETY: The caller passes a live namespace box.
    unsafe { (&mut *namespace).set(intern(name), function) };
    Ok(())
}

fn build_pathlike_class() -> Result<*mut PyObject, String> {
    let type_type = crate::abi::runtime_type_type();
    if type_type.is_null() {
        return Err("builtin 'type' is not initialized for os.PathLike".to_owned());
    }
    let meta_namespace = crate::types::type_::new_namespace();
    if meta_namespace.is_null() {
        return Err("failed to allocate the os._PathLikeMeta namespace".to_owned());
    }
    class_str_attr(meta_namespace, "__module__", "os")?;
    class_function_attr(meta_namespace, "__instancecheck__", pathlike_instancecheck)?;
    class_function_attr(meta_namespace, "__subclasscheck__", pathlike_subclasscheck)?;
    class_function_attr(meta_namespace, "__getitem__", pathlike_class_getitem)?;
    class_function_attr(meta_namespace, "register", pathlike_register)?;
    // SAFETY: The base is the live builtin `type` object.
    let meta = unsafe {
        crate::types::type_::build_class_from_namespace(
            "_PathLikeMeta",
            &[type_type.cast::<PyObject>()],
            meta_namespace,
            &[],
        )
    };
    let meta = finish_class(meta, "_PathLikeMeta", type_type)?;

    let namespace = crate::types::type_::new_namespace();
    if namespace.is_null() {
        return Err("failed to allocate the os.PathLike namespace".to_owned());
    }
    class_str_attr(namespace, "__module__", "os")?;
    class_str_attr(namespace, "__doc__", "Abstract base class for implementing the file system path protocol.")?;
    class_function_attr(namespace, "__fspath__", pathlike_fspath_abstract)?;
    class_function_attr(namespace, "__class_getitem__", pathlike_class_getitem)?;
    let keywords = [crate::types::type_::ClassKeyword { name: intern("metaclass"), value: meta }];
    // SAFETY: Implicit `object` base; the metaclass keyword is a live class.
    let class = unsafe { crate::types::type_::build_class_from_namespace("PathLike", &[], namespace, &keywords) };
    finish_class(class, "PathLike", meta.cast::<crate::object::PyType>())
}

/// Shared post-construction checks: surface the pending diagnostic as a
/// module-creation error and mirror `pon_build_class`'s ob_type fix-up.
fn finish_class(class: *mut PyObject, name: &str, metaclass: *mut crate::object::PyType) -> Result<*mut PyObject, String> {
    if class.is_null() {
        let detail = crate::thread_state::pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
        crate::thread_state::pon_err_clear();
        return Err(format!("failed to create os.{name}: {detail}"));
    }
    // SAFETY: Freshly built class object owned by this module build.
    unsafe {
        if (*class).ob_type.is_null() {
            (*class).ob_type = metaclass.cast_const();
        }
    }
    Ok(class)
}

/// True when `object`'s type carries `__fspath__` anywhere on its MRO.
fn implements_fspath(object: *mut PyObject) -> bool {
    let raw = crate::tag::untag_arg(object);
    if raw.is_null() || crate::tag::is_small_int(raw) {
        return false;
    }
    // SAFETY: Heap pointer with a live header after the tag checks.
    let ty = unsafe { (*raw).ob_type.cast_mut() };
    !unsafe { crate::descr::lookup_in_type(ty, intern("__fspath__")) }.is_null()
}

/// True when the class object `candidate` defines `__fspath__` on its MRO.
fn class_implements_fspath(candidate: *mut PyObject) -> bool {
    if candidate.is_null() || crate::tag::is_small_int(candidate) {
        return false;
    }
    !unsafe { crate::descr::lookup_in_type(candidate.cast::<crate::object::PyType>(), intern("__fspath__")) }.is_null()
}

fn bool_object(value: bool) -> *mut PyObject {
    // SAFETY: Singleton accessor.
    unsafe { crate::abi::pon_const_bool(i32::from(value)) }
}

/// `_PathLikeMeta.__instancecheck__(cls, instance)`: MRO subtype first, then
/// the `__fspath__` structural probe — but the probe only answers for
/// `PathLike` itself, mirroring `PathLike.__subclasshook__`'s
/// `if cls is PathLike` guard (subclasses get plain MRO semantics).
unsafe extern "C" fn pathlike_instancecheck(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 2 {
        return crate::abi::return_null_with_error("__instancecheck__ expected (cls, instance)");
    }
    let cls = args[0];
    let object = crate::tag::untag_arg(args[1]);
    if object.is_null() {
        return bool_object(false);
    }
    if !crate::tag::is_small_int(object) {
        // SAFETY: Heap pointer with a live header after the tag checks.
        let ty = unsafe { (*object).ob_type.cast_mut() };
        if unsafe { crate::mro::is_subtype(ty, cls.cast::<crate::object::PyType>()) } {
            return bool_object(true);
        }
    }
    let receiver_is_pathlike = pathlike_class().is_ok_and(|pathlike| pathlike == cls);
    bool_object(receiver_is_pathlike && (implements_fspath(args[1]) || instance_is_registered(args[1])))
}

/// `_PathLikeMeta.__subclasscheck__(cls, candidate)`: see
/// [`pathlike_instancecheck`] for the `cls is PathLike` guard rationale.
unsafe extern "C" fn pathlike_subclasscheck(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 2 {
        return crate::abi::return_null_with_error("__subclasscheck__ expected (cls, candidate)");
    }
    let cls = args[0];
    let candidate = crate::tag::untag_arg(args[1]);
    if candidate.is_null() || crate::tag::is_small_int(candidate) {
        return bool_object(false);
    }
    // SAFETY: `issubclass` validated both operands as classes before
    // dispatching to this hook.
    if unsafe { crate::mro::is_subtype(candidate.cast::<crate::object::PyType>(), cls.cast::<crate::object::PyType>()) } {
        return bool_object(true);
    }
    let receiver_is_pathlike = pathlike_class().is_ok_and(|pathlike| pathlike == cls);
    bool_object(receiver_is_pathlike && (class_implements_fspath(candidate) || class_is_registered(candidate)))
}

/// ABC registry backing `PathLike.register` (`pathlib` registers `PurePath`).
/// Registered classes are process-lifetime class objects, stored as raw
/// addresses; the checks walk `is_subtype` against every entry, matching
/// ABCMeta's registry semantics minus the negative cache.
static PATHLIKE_REGISTRY: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// `_PathLikeMeta.register(cls, subclass)`: records a virtual subclass and
/// returns it (CPython's decorator contract).
unsafe extern "C" fn pathlike_register(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 2 {
        return crate::abi::return_null_with_error("register expected one argument (a class)");
    }
    let subclass = crate::tag::untag_arg(args[1]);
    let is_class = !subclass.is_null() && !crate::tag::is_small_int(subclass) && {
        // SAFETY: Heap pointer with a live header after the tag checks; a
        // class object's own type linearizes over the builtin `type`.
        let meta = unsafe { (*subclass).ob_type.cast_mut() };
        unsafe { crate::mro::is_subtype(meta, crate::abi::runtime_type_type()) }
    };
    if !is_class {
        return crate::abi::exc::raise_kind_error_text(ExceptionKind::TypeError, "Can only register classes");
    }
    let mut registry = PATHLIKE_REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
    let entry = subclass as usize;
    if !registry.contains(&entry) {
        registry.push(entry);
    }
    drop(registry);
    args[1]
}

/// True when `object`'s type derives a `PathLike.register`ed class.
fn instance_is_registered(object: *mut PyObject) -> bool {
    let raw = crate::tag::untag_arg(object);
    if raw.is_null() || crate::tag::is_small_int(raw) {
        return false;
    }
    // SAFETY: Heap pointer with a live header after the tag checks.
    class_is_registered(unsafe { (*raw).ob_type.cast_mut() }.cast::<PyObject>())
}

/// True when the class object `candidate` derives a registered class.
fn class_is_registered(candidate: *mut PyObject) -> bool {
    let registry = PATHLIKE_REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
    registry.iter().any(|&registered| {
        // SAFETY: Registry entries are live process-lifetime class objects.
        unsafe { crate::mro::is_subtype(candidate.cast::<crate::object::PyType>(), registered as *mut crate::object::PyType) }
    })
}

/// `PathLike[str]`: served both as `_PathLikeMeta.__getitem__` (the subscript
/// dispatch path for class receivers) and as `PathLike.__class_getitem__`
/// (CPython publishes `classmethod(GenericAlias)` under that name).
unsafe extern "C" fn pathlike_class_getitem(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    let (origin, key) = match args.len() {
        // Unbound `PathLike.__class_getitem__(item)` call shape.
        1 => match pathlike_class() {
            Ok(class) => (class, args[0]),
            Err(message) => return crate::abi::return_null_with_error(message),
        },
        2 => (args[0], args[1]),
        _ => return crate::abi::return_null_with_error("__class_getitem__ expected one argument"),
    };
    let key = crate::tag::untag_arg(key);
    if key.is_null() {
        return std::ptr::null_mut();
    }
    let key_is_tuple = !crate::tag::is_small_int(key)
        // SAFETY: Heap pointer with a live header after the tag checks.
        && unsafe { crate::types::dict::type_name(key) } == Some("tuple");
    let key_args = if key_is_tuple {
        // SAFETY: The type check proved PyTuple layout.
        unsafe { (*key.cast::<crate::types::tuple::PyTuple>()).as_slice() }.to_vec()
    } else {
        vec![key]
    };
    crate::types::typealias::new_generic_alias(origin, key_args)
}

/// `PathLike.__fspath__` abstract body: CPython's `raise NotImplementedError`.
unsafe extern "C" fn pathlike_fspath_abstract(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(ExceptionKind::NotImplementedError, "")
}

// ---------------------------------------------------------------------------
// os.terminal_size
//
// CPython's `os.terminal_size` is a structseq (a tuple subclass with named
// fields) defined by the C `posix` module; `shutil.get_terminal_size`'s
// final fallback constructs one from a 2-int sequence and `argparse` reads
// `.columns`.  pon builds the same shape through the tuple-embedding heap
// class machinery (`class terminal_size(tuple)` with `columns`/`lines`
// properties and the CPython repr).  `os.get_terminal_size` is DELIBERATELY
// absent: `shutil` catches the AttributeError and takes its deterministic
// `(80, 24)`-shaped env fallback, which keeps differential runs stable
// whether or not a real tty is attached.

fn terminal_size_class() -> Result<*mut PyObject, String> {
    static CLASS: std::sync::LazyLock<Result<usize, String>> =
        std::sync::LazyLock::new(|| build_terminal_size_class().map(|class| class as usize));
    CLASS.clone().map(|class| class as *mut PyObject)
}

fn build_terminal_size_class() -> Result<*mut PyObject, String> {
    // SAFETY: `pon_load_global` returns NULL with a raised NameError on miss.
    let tuple_class = unsafe { crate::abi::pon_load_global(intern("tuple"), std::ptr::null_mut()) };
    if tuple_class.is_null() {
        crate::thread_state::pon_err_clear();
        return Err("builtin 'tuple' is not registered for os.terminal_size".to_owned());
    }
    // SAFETY: Same contract for the builtin `property` constructor.
    let property_class = unsafe { crate::abi::pon_load_global(intern("property"), std::ptr::null_mut()) };
    if property_class.is_null() {
        crate::thread_state::pon_err_clear();
        return Err("builtin 'property' is not registered for os.terminal_size".to_owned());
    }
    let namespace = crate::types::type_::new_namespace();
    if namespace.is_null() {
        return Err("failed to allocate the os.terminal_size namespace".to_owned());
    }
    class_str_attr(namespace, "__module__", "os")?;
    class_str_attr(namespace, "__doc__", "A tuple of (columns, lines) for holding terminal window size")?;
    class_function_attr(namespace, "__repr__", terminal_size_repr)?;
    for (name, entry) in [
        ("columns", terminal_size_columns as BuiltinFn),
        ("lines", terminal_size_lines as BuiltinFn),
    ] {
        // SAFETY: Live builtin entry point with the runtime calling convention.
        let fget = unsafe {
            crate::abi::pon_make_function(entry as *const u8, 1, intern(name))
        };
        if fget.is_null() {
            return Err(format!("failed to allocate os.terminal_size.{name} getter"));
        }
        let mut argv = [fget];
        // SAFETY: The builtin `property` class is callable with one fget slot.
        let descriptor = unsafe { crate::abi::pon_call(property_class, argv.as_mut_ptr(), argv.len()) };
        if descriptor.is_null() {
            let detail = crate::thread_state::pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
            crate::thread_state::pon_err_clear();
            return Err(format!("failed to build os.terminal_size.{name} property: {detail}"));
        }
        // SAFETY: `new_namespace` returned a live namespace box.
        unsafe { (&mut *namespace).set(intern(name), descriptor) };
    }
    // SAFETY: The base is the live builtin `tuple` class object.
    let class = unsafe {
        crate::types::type_::build_class_from_namespace("terminal_size", &[tuple_class], namespace, &[])
    };
    finish_class(class, "terminal_size", crate::abi::runtime_type_type())
}

/// Reads element `index` of a terminal_size receiver as an i64.
fn terminal_size_element(args: &[*mut PyObject], index: i64, what: &str) -> Result<i64, *mut PyObject> {
    if args.len() != 1 {
        return Err(crate::abi::return_null_with_error(format!("{what} expected only a receiver")));
    }
    // SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
    let key = unsafe { crate::abi::pon_const_int(index) };
    if key.is_null() {
        return Err(std::ptr::null_mut());
    }
    // SAFETY: Subscript dispatch resolves the tuple-embedded layout.
    let element = unsafe { crate::abstract_op::subscript_get(args[0], key) };
    if element.is_null() {
        return Err(std::ptr::null_mut());
    }
    int_arg(element, what)
}

/// `terminal_size.columns` property getter: `self[0]`.
unsafe extern "C" fn terminal_size_columns(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    match terminal_size_element(args, 0, "terminal_size.columns") {
        // SAFETY: Integer boxing helper follows the NULL-sentinel contract.
        Ok(value) => unsafe { crate::abi::pon_const_int(value) },
        Err(error) => error,
    }
}

/// `terminal_size.lines` property getter: `self[1]`.
unsafe extern "C" fn terminal_size_lines(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    match terminal_size_element(args, 1, "terminal_size.lines") {
        // SAFETY: Integer boxing helper follows the NULL-sentinel contract.
        Ok(value) => unsafe { crate::abi::pon_const_int(value) },
        Err(error) => error,
    }
}

/// CPython's structseq repr: `os.terminal_size(columns=80, lines=24)`.
unsafe extern "C" fn terminal_size_repr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    let columns = match terminal_size_element(args, 0, "terminal_size.columns") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let lines = match terminal_size_element(args, 1, "terminal_size.lines") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let text = format!("os.terminal_size(columns={columns}, lines={lines})");
    // SAFETY: String allocation helper follows the NULL-sentinel contract.
    unsafe { pon_const_str(text.as_ptr(), text.len()) }
}
