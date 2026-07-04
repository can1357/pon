//! Native `os` module seed for WS-IMPORT.

use crate::abi::{CodeInfo, ParamSpec, pon_const_str};
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
    if module == "os" {
        attrs.push((intern("altsep"), unsafe { crate::abi::pon_none() }));
    }
    for &(name, value) in [OPEN_FLAGS, ACCESS_FLAGS, WAIT_OPTIONS, SEEK_MODES].into_iter().flatten() {
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
        // `os.py`'s fs-codec pair (`fsencode`/`fsdecode`, never re-exported
        // into `posix`); `test.support.os_helper` consumes both at module
        // body in its FS_NONASCII probe loop.
        for (name, entry) in [("fsencode", os_fsencode as BuiltinFn), ("fsdecode", os_fsdecode as BuiltinFn)] {
            // SAFETY: Live builtin entry points with the runtime calling convention.
            let function = unsafe { crate::abi::pon_make_function(entry as *const u8, 1, intern(name)) };
            if function.is_null() {
                return Err(format!("failed to allocate {module}.{name}"));
            }
            attrs.push((intern(name), function));
        }
        // `os.py`'s `getenv` (never re-exported into `posix`, exactly like
        // the fs-codec pair): `environ.get(key, default)` over the LIVE
        // `os.environ` binding — see `os_getenv` for the read-through
        // contract.
        let getenv = unsafe {
            crate::abi::pon_make_function(
                os_getenv as *const u8,
                crate::native::builtins_mod::VARIADIC_ARITY,
                intern("getenv"),
            )
        };
        if getenv.is_null() {
            return Err(format!("failed to allocate {module}.getenv"));
        }
        attrs.push((intern("getenv"), getenv));
        // `os.get_exec_path(env=None)`: subprocess uses this to build the
        // `_posixsubprocess.fork_exec` executable candidate tuple when the
        // requested program name has no directory component.
        let get_exec_path = unsafe {
            crate::abi::pon_make_function(
                os_get_exec_path as *const u8,
                crate::native::builtins_mod::VARIADIC_ARITY,
                intern("get_exec_path"),
            )
        };
        if get_exec_path.is_null() {
            return Err(format!("failed to allocate {module}.get_exec_path"));
        }
        attrs.push((intern("get_exec_path"), get_exec_path));
        // `os.walk` is an os.py-level generator in CPython.  Build backends
        // prune `dirnames` in place, so the iterator below yields before
        // enqueuing top-down children and reads the same list on resume.
        let mut walk_defaults = unsafe {
            [
                crate::abi::pon_const_bool(1),
                crate::abi::pon_none(),
                crate::abi::pon_const_bool(0),
            ]
        };
        if walk_defaults.iter().any(|value| value.is_null()) {
            return Err(format!("failed to allocate {module}.walk defaults"));
        }
        attrs.push(phase_b_function_attr(
            "walk",
            os_walk,
            &["top", "topdown", "onerror", "followlinks"],
            &mut walk_defaults,
        )?);
        attrs.push((intern("_walk_symlinks_as_files"), walk_symlinks_as_files()));
        let mut makedirs_defaults = unsafe { [crate::abi::pon_const_int(0o777), crate::abi::pon_const_bool(0)] };
        if makedirs_defaults.iter().any(|value| value.is_null()) {
            return Err(format!("failed to allocate {module}.makedirs defaults"));
        }
        attrs.push(phase_b_function_attr(
            "makedirs",
            os_makedirs,
            &["name", "mode", "exist_ok"],
            &mut makedirs_defaults,
        )?);
        // `importlib.resources._common` keeps a direct `_os_remove=os.remove`
        // reference for late finalization cleanup, so publish the CPython
        // alias alongside the underlying `unlink` syscall wrapper.
        let remove = unsafe { crate::abi::pon_make_function(os_unlink as *const u8, 1, intern("remove")) };
        if remove.is_null() {
            return Err(format!("failed to allocate {module}.remove"));
        }
        attrs.push((intern("remove"), remove));
        // `os.py`-level names never re-exported into `posix`: the portable
        // seek trio (see [`SEEK_MODES`]) and the null-device path (os.py
        // takes it from `posixpath.devnull`; `test.test_py_compile` probes
        // `os.path.exists(os.devnull)` at class-body time).
        for &(name, value) in SEEK_POSITIONS {
            // SAFETY: Integer boxing helper; NULL is checked below.
            let boxed = unsafe { crate::abi::pon_const_int(i64::from(value)) };
            if boxed.is_null() {
                return Err(format!("failed to allocate {module}.{name}"));
            }
            attrs.push((intern(name), boxed));
        }
        attrs.push(string_attr(module, "defpath", if cfg!(windows) { ".;C:\\\\bin" } else { "/bin:/usr/bin" })?);
        attrs.push(string_attr(module, "devnull", if cfg!(windows) { "nul" } else { "/dev/null" })?);
        attrs.push((intern("PathLike"), pathlike_class()?));
    }
    Ok(attrs)
}

fn phase_b_function_attr(
    name: &str,
    entry: BuiltinFn,
    names: &[&str],
    defaults: &mut [*mut PyObject],
) -> Result<(u32, *mut PyObject), String> {
    let interned_names: Vec<u32> = names.iter().map(|name| intern(name)).collect();
    let params = ParamSpec {
        names: if interned_names.is_empty() {
            std::ptr::null()
        } else {
            interned_names.as_ptr()
        },
        total_param_count: interned_names.len() as u32,
        positional_only_count: 0,
        positional_count: interned_names.len() as u32,
        keyword_only_count: 0,
        varargs_name: 0,
        varkw_name: 0,
    };
    let code = CodeInfo {
        entry: entry as *const u8,
        params: &params,
        name_interned: intern(name),
        n_locals: 0,
        n_feedback: 0,
        flags: 0,
    };
    let function = unsafe {
        crate::abi::call::pon_make_function_full(
            &code,
            if defaults.is_empty() {
                std::ptr::null_mut()
            } else {
                defaults.as_mut_ptr()
            },
            defaults.len(),
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
        )
    };
    (!function.is_null())
        .then_some((intern(name), function))
        .ok_or_else(|| format!("failed to allocate os.{name}"))
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

/// `os.fsencode(filename)`: `os.py`'s fs-codec pair, served natively.
/// fspath coercion first (str/bytes pass, `__fspath__` defers), then str
/// encodes with the filesystem encoding.  Divergence: pon's filesystem
/// encoding is strict UTF-8 with no `surrogateescape` — pon str never
/// carries lone surrogates, so the encode step itself is total.
unsafe extern "C" fn os_fsencode(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Delegated coercion under the caller's own live argv contract.
    let coerced = unsafe { os_fspath(argv, argc) };
    if coerced.is_null() {
        return std::ptr::null_mut();
    }
    let raw = crate::tag::untag_arg(coerced);
    if !raw.is_null() && !crate::tag::is_small_int(raw) {
        // SAFETY: Heap pointer with a live header after the tag checks.
        if let Some(text) = unsafe { crate::types::type_::unicode_text(raw) } {
            // SAFETY: Bytes allocation helper follows the NULL-sentinel contract.
            return unsafe { crate::abi::str_::pon_const_bytes(text.as_ptr(), text.len()) };
        }
        // SAFETY: Live header per the checks above.
        if crate::types::bytes_::is_bytes_type(unsafe { (*raw).ob_type }) {
            return coerced;
        }
    }
    fs_codec_hook_type_error("fsencode", raw)
}

/// `os.fsdecode(filename)`: bytes decode with the filesystem encoding
/// (strict UTF-8 — see [`os_fsencode`] for the surrogateescape divergence),
/// str passes through.
unsafe extern "C" fn os_fsdecode(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Delegated coercion under the caller's own live argv contract.
    let coerced = unsafe { os_fspath(argv, argc) };
    if coerced.is_null() {
        return std::ptr::null_mut();
    }
    let raw = crate::tag::untag_arg(coerced);
    if !raw.is_null() && !crate::tag::is_small_int(raw) {
        // SAFETY: Heap pointer with a live header after the tag checks.
        if unsafe { crate::types::type_::unicode_text(raw) }.is_some() {
            return coerced;
        }
        // SAFETY: Live header per the checks above.
        if crate::types::bytes_::is_bytes_type(unsafe { (*raw).ob_type }) {
            // SAFETY: The type check proved PyBytes layout.
            let payload = unsafe { (*raw.cast::<crate::types::bytes_::PyBytes>()).as_slice() };
            return match super::codecs::utf8_decode_core(payload, "strict", true) {
                Ok((text, _)) => {
                    // SAFETY: String allocation helper follows the NULL-sentinel contract.
                    unsafe { pon_const_str(text.as_ptr(), text.len()) }
                }
                Err(error) => error.raise(),
            };
        }
    }
    fs_codec_hook_type_error("fsdecode", raw)
}

/// TypeError for a `__fspath__` hook that returned a non-str/bytes object.
/// Direct non-path arguments already raised inside the fspath coercion;
/// CPython raises this shape check inside `fspath` itself (`expected
/// X.__fspath__() to return str or bytes, not Y`), pon's message names the
/// consuming codec instead because the coercion returns hook results
/// unvalidated.
fn fs_codec_hook_type_error(what: &str, raw: *mut PyObject) -> *mut PyObject {
    let display = if raw.is_null() {
        "NoneType"
    } else if crate::tag::is_small_int(raw) {
        "int"
    } else {
        // SAFETY: Heap pointer with a live header per the caller's checks.
        unsafe { crate::types::dict::type_name(raw) }.unwrap_or("object")
    };
    crate::abi::exc::raise_kind_error_text(
        ExceptionKind::TypeError,
        &format!("os.{what}: __fspath__() must return str or bytes, not {display}"),
    )
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

/// `os.stat_result` shape: serve the POSIX fields consumed by the vendored
/// stdlib and Meson (`st_size`/`st_mtime`, permission/owner bits, and stable
/// file identity). Unknown attributes still raise AttributeError so the next
/// frontier is loud, not silently wrong (`_pyio` reads `st_blksize` through
/// `getattr(..., 0)`, which that AttributeError serves correctly).
#[repr(C)]
struct PyStatResult {
    ob_base: crate::object::PyObjectHeader,
    st_size: i64,
    st_atime: f64,
    st_mtime: f64,
    st_ctime: f64,
    st_mode: i64,
    st_ino: i64,
    st_dev: i64,
    st_nlink: i64,
    st_uid: i64,
    st_gid: i64,
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
        "st_atime" => unsafe { crate::abi::number::pon_const_float((*stat).st_atime) },
        "st_mtime" => unsafe { crate::abi::number::pon_const_float((*stat).st_mtime) },
        "st_ctime" => unsafe { crate::abi::number::pon_const_float((*stat).st_ctime) },
        "st_mode" => unsafe { crate::abi::pon_const_int((*stat).st_mode) },
        "st_ino" => unsafe { crate::abi::pon_const_int((*stat).st_ino) },
        "st_dev" => unsafe { crate::abi::pon_const_int((*stat).st_dev) },
        "st_nlink" => unsafe { crate::abi::pon_const_int((*stat).st_nlink) },
        "st_uid" => unsafe { crate::abi::pon_const_int((*stat).st_uid) },
        "st_gid" => unsafe { crate::abi::pon_const_int((*stat).st_gid) },
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
    let path_text = match path_arg(unsafe { *argv }, "stat") {
        Ok(path) => path,
        Err(error) => return error,
    };
    match std::fs::metadata(&path_text) {
        Ok(metadata) => stat_result_object(&metadata),
        Err(error) => raise_errno(error.raw_os_error().unwrap_or(libc::EIO), Some(path_text.as_str())),
    }
}

/// Boxes host metadata as the `os.stat_result` shape shared by `os.stat`
/// and `os.lstat`.
fn stat_result_object(metadata: &std::fs::Metadata) -> *mut PyObject {
    #[cfg(unix)]
    let fields = {
        use std::os::unix::fs::MetadataExt;
        StatFields {
            st_size: stat_i64(metadata.len()),
            st_atime: stat_timestamp(metadata.atime(), metadata.atime_nsec()),
            st_mtime: stat_timestamp(metadata.mtime(), metadata.mtime_nsec()),
            st_ctime: stat_timestamp(metadata.ctime(), metadata.ctime_nsec()),
            st_mode: stat_i64(metadata.mode()),
            st_ino: stat_i64(metadata.ino()),
            st_dev: stat_i64(metadata.dev()),
            st_nlink: stat_i64(metadata.nlink()),
            st_uid: stat_i64(metadata.uid()),
            st_gid: stat_i64(metadata.gid()),
        }
    };
    #[cfg(not(unix))]
    let fields = StatFields {
        st_size: stat_i64(metadata.len()),
        st_atime: metadata
            .accessed()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0.0, |duration| duration.as_secs_f64()),
        st_mtime: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0.0, |duration| duration.as_secs_f64()),
        st_ctime: metadata
            .created()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0.0, |duration| duration.as_secs_f64()),
        st_mode: 0,
        st_ino: 0,
        st_dev: 0,
        st_nlink: 0,
        st_uid: 0,
        st_gid: 0,
    };
    stat_result_from_fields(fields)
}

#[derive(Clone, Copy)]
struct StatFields {
    st_size: i64,
    st_atime: f64,
    st_mtime: f64,
    st_ctime: f64,
    st_mode: i64,
    st_ino: i64,
    st_dev: i64,
    st_nlink: i64,
    st_uid: i64,
    st_gid: i64,
}

fn stat_i64<T>(value: T) -> i64
where
    i64: TryFrom<T>,
{
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[allow(clippy::cast_precision_loss)]
fn stat_timestamp(seconds: i64, nanoseconds: i64) -> f64 {
    seconds as f64 + nanoseconds as f64 * 1e-9
}

/// Boxes explicit field values as an `os.stat_result`; shared by the
/// metadata path above and the raw `fstat(2)` path below.
fn stat_result_from_fields(fields: StatFields) -> *mut PyObject {
    Box::into_raw(Box::new(PyStatResult {
        ob_base: crate::object::PyObjectHeader::new(stat_result_type()),
        st_size: fields.st_size,
        st_atime: fields.st_atime,
        st_mtime: fields.st_mtime,
        st_ctime: fields.st_ctime,
        st_mode: fields.st_mode,
        st_ino: fields.st_ino,
        st_dev: fields.st_dev,
        st_nlink: fields.st_nlink,
        st_uid: fields.st_uid,
        st_gid: fields.st_gid,
    }))
    .cast::<PyObject>()
}

/// `os.fstat(fd)` over `fstat(2)`: the stat_result for an open descriptor
/// (`_pyio.FileIO.__init__` probes `S_ISDIR(st_mode)`; `netrc`'s security
/// check compares `st_uid` against `os.getuid()`).
unsafe extern "C" fn os_fstat(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 1 {
        return crate::abi::return_null_with_error("os.fstat expected one argument");
    }
    let fd = match int_arg(args[0], "fstat fd") {
        Ok(fd) => fd,
        Err(error) => return error,
    };
    let mut raw = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `raw` is a live out-buffer; failure reports through errno below.
    if unsafe { libc::fstat(fd as libc::c_int, raw.as_mut_ptr()) } < 0 {
        return raise_errno(last_errno(), None);
    }
    // SAFETY: fstat(2) success fills the whole struct.
    let raw = unsafe { raw.assume_init() };
    let fields = StatFields {
        st_size: stat_i64(raw.st_size),
        st_atime: stat_timestamp(raw.st_atime, raw.st_atime_nsec),
        st_mtime: stat_timestamp(raw.st_mtime, raw.st_mtime_nsec),
        st_ctime: stat_timestamp(raw.st_ctime, raw.st_ctime_nsec),
        st_mode: stat_i64(raw.st_mode),
        st_ino: stat_i64(raw.st_ino),
        st_dev: stat_i64(raw.st_dev),
        st_nlink: stat_i64(raw.st_nlink),
        st_uid: stat_i64(raw.st_uid),
        st_gid: stat_i64(raw.st_gid),
    };
    stat_result_from_fields(fields)
}

/// `os.chmod(path, mode)` over `chmod(2)` (`test.support.os_helper.can_chmod`
/// round-trips it against `os.stat().st_mode`).
unsafe extern "C" fn os_chmod(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 2 {
        return crate::abi::return_null_with_error("os.chmod expected two arguments (path, mode)");
    }
    let path = match path_arg(args[0], "chmod") {
        Ok(path) => path,
        Err(error) => return error,
    };
    let mode = match int_arg(args[1], "chmod mode") {
        Ok(mode) => mode,
        Err(error) => return error,
    };
    let c_path = match c_path(&path) {
        Ok(c_path) => c_path,
        Err(error) => return error,
    };
    // SAFETY: `c_path` is NUL-terminated.
    if unsafe { libc::chmod(c_path.as_ptr(), mode as libc::mode_t) } < 0 {
        return raise_errno(last_errno(), Some(&path));
    }
    // SAFETY: Singleton accessor.
    unsafe { crate::abi::pon_none() }
}

/// `os.umask(mask)` sets the process umask and returns the previous mask.
unsafe extern "C" fn os_umask(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 1 {
        return crate::abi::return_null_with_error("os.umask expected one argument");
    }
    let mask = match int_arg(args[0], "umask mask") {
        Ok(mask) => mask,
        Err(error) => return error,
    };
    // SAFETY: umask(2) cannot fail; it returns the previous process mask.
    let previous = unsafe { libc::umask(mask as libc::mode_t) };
    unsafe { crate::abi::pon_const_int(i64::from(previous)) }
}

/// `os.access(path, mode)` over `access(2)`: reports whether the process can
/// access `path` under `mode` (an `F_OK`/`R_OK`/`W_OK`/`X_OK` combination).
/// Never raises for an inaccessible path — a failing check returns `False`,
/// matching CPython.
unsafe extern "C" fn os_access(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 2 {
        return crate::abi::return_null_with_error("os.access expected two arguments (path, mode)");
    }
    let path = match path_arg(args[0], "access") {
        Ok(path) => path,
        Err(error) => return error,
    };
    let mode = match int_arg(args[1], "access mode") {
        Ok(mode) => mode,
        Err(error) => return error,
    };
    let c_path = match c_path(&path) {
        Ok(c_path) => c_path,
        Err(error) => return error,
    };
    // SAFETY: `c_path` is NUL-terminated; `access(2)` returning nonzero (with
    // errno set) means "not accessible", which CPython folds into False.
    let ok = unsafe { libc::access(c_path.as_ptr(), mode as libc::c_int) } == 0;
    // SAFETY: Singleton accessor.
    unsafe { crate::abi::number::pon_const_bool(i32::from(ok)) }
}

/// `os.getuid()` over `getuid(2)` (`netrc._can_security_check` gates on its
/// presence; the check itself compares it to the file owner).
unsafe extern "C" fn os_getuid(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    // SAFETY: getuid(2) cannot fail; integer boxing follows the NULL-sentinel contract.
    unsafe { crate::abi::pon_const_int(i64::from(libc::getuid())) }
}

/// `os.isatty(fd)` over `isatty(3)` (`_pyio.open`'s default-buffering path
/// probes `raw.isatty()` to pick line buffering).
unsafe extern "C" fn os_isatty(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 1 {
        return crate::abi::return_null_with_error("os.isatty expected one argument");
    }
    let fd = match int_arg(args[0], "isatty fd") {
        Ok(fd) => fd,
        Err(error) => return error,
    };
    // SAFETY: Plain fd probe; a non-tty (or bad fd) answers 0 with errno,
    // which CPython folds into False rather than raising.
    let is_tty = unsafe { libc::isatty(fd as libc::c_int) } != 0;
    // SAFETY: Singleton accessor.
    unsafe { crate::abi::number::pon_const_bool(i32::from(is_tty)) }
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

/// `waitpid(2)` option constants: `subprocess._del_safe` binds `WNOHANG` at
/// import time (`Popen.__del__`'s non-blocking reap), and asyncio's child
/// watchers pass it on every poll.
const WAIT_OPTIONS: &[(&str, i32)] = &[("WNOHANG", libc::WNOHANG)];

/// `lseek(2)` whence constants served by the C `posix` module on the host
/// oracle: `SEEK_HOLE`/`SEEK_DATA` (sparse-file navigation) live on BOTH
/// `os` and `posix`, while the portable trio `SEEK_SET`/`SEEK_CUR`/
/// `SEEK_END` is defined by `os.py` itself and never re-exported into
/// `posix` — [`build_attrs`] adds the trio under its `module == "os"`
/// branch.  `zipfile` consumes `os.SEEK_SET`/`os.SEEK_CUR` at import time
/// (module-level `_EndRecData` helpers seed `whence` defaults), and
/// `importlib.metadata`/`pkgutil`/`zipimport` reach it through the zipfile
/// chain.  Values come from libc, so they always match the host CPython's
/// (darwin: HOLE=3/DATA=4; linux swaps them).
const SEEK_MODES: &[(&str, i32)] = &[("SEEK_DATA", libc::SEEK_DATA), ("SEEK_HOLE", libc::SEEK_HOLE)];

/// `os.py`-level `SEEK_SET`/`SEEK_CUR`/`SEEK_END` (see [`SEEK_MODES`]).
const SEEK_POSITIONS: &[(&str, i32)] = &[
    ("SEEK_SET", libc::SEEK_SET),
    ("SEEK_CUR", libc::SEEK_CUR),
    ("SEEK_END", libc::SEEK_END),
];

/// Snapshot of the process environment as a plain str->str dict.
///
/// DECISION: the dict stays; `os._Environ` is not modelled.  CPython's
/// `os.environ` is a live `MutableMapping` whose item writes update the real
/// process environment; pon's snapshot intentionally does not.  Callers that
/// need child-environment parity must pass an explicit `env` mapping to
/// `subprocess` or use the native `putenv`/`unsetenv` write-through helpers.
/// In-cohort consumers still work over dict semantics: `tempfile` reads
/// through `os.getenv`, `subprocess` can pass explicit env dicts, and
/// `test.support.os_helper.EnvironmentVarGuard` uses plain mapping ops
/// (`[]=`, `del`, `keys`, iteration — all served by dict, `setdefault`
/// included) plus an `os.environ = ...` rebinding that the live module-attr
/// read in [`os_getenv`] honors.
///
/// Remaining documented divergences: mutating `os.environ` never reaches
/// the real process environment (visible to native env readers and spawned children);
/// `repr(os.environ)` is a dict repr, not `environ({...})`; `os.environb`,
/// the `_Environ.encodekey` family, and non-str-key TypeErrors on item ops
/// are absent; `posix.environ` is a second str->str snapshot rather than
/// CPython's bytes-keyed raw dict; non-UTF-8 entries are decoded lossily
/// rather than with CPython's `surrogateescape`; and the snapshot is taken
/// when the module is created.
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
// POSIX syscall surface: open/close/read/write/unlink/rmdir/lstat, the
// waitpid/wait-status family, plus the scandir frontier stub.  Raw libc
// calls over the same process fd space the `_io` native files wrap
// (`File::from_raw_fd`), with errno mapped onto CPython's OSError subclass
// hierarchy (PEP 3151).

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

static WALK_REGISTRY: crate::gcroot::RootRegistry = crate::gcroot::RootRegistry::new();

/// Python objects held by live `os.walk` iterators.
pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
    WALK_REGISTRY.held_roots()
}

fn walk_symlinks_as_files() -> *mut PyObject {
    static SENTINEL: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        Box::into_raw(Box::new(crate::object::PyObjectHeader::new(runtime_object_type()))).cast::<PyObject>() as usize
    });
    *SENTINEL as *mut PyObject
}

fn runtime_object_type() -> *mut crate::object::PyType {
    crate::abi::runtime_global(intern("object")).map_or(std::ptr::null_mut(), |object| object.cast::<crate::object::PyType>())
}

#[repr(C)]
struct PyWalk {
    ob_base: crate::object::PyObjectHeader,
    stack: Vec<WalkStackEntry>,
    pending_topdown: Option<PendingTopDown>,
    topdown: bool,
    followlinks: bool,
    symlinks_as_files: bool,
    onerror: *mut PyObject,
}

enum WalkStackEntry {
    Path(String),
    Yield {
        top: String,
        dirs: Vec<String>,
        files: Vec<String>,
    },
}

struct PendingTopDown {
    top: String,
    dirnames: *mut PyObject,
}

struct WalkScan {
    dirs: Vec<String>,
    files: Vec<String>,
    walk_dirs: Vec<String>,
}

struct WalkIoError {
    errno: i32,
    path: String,
}

impl crate::gcroot::HeldRoots for PyWalk {
    unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
        push(self.onerror);
        if let Some(pending) = &self.pending_topdown {
            push(pending.dirnames);
        }
    }
}

fn walk_type() -> *mut crate::object::PyType {
    static WALK_TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        let mut ty = crate::object::PyType::new(
            crate::abi::runtime_type_type().cast_const(),
            "os.walk",
            std::mem::size_of::<PyWalk>(),
        );
        ty.tp_iter = Some(walk_iter);
        ty.tp_iternext = Some(walk_next);
        ty.tp_getattro = Some(crate::abstract_op::iterator_dunder_getattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    *WALK_TYPE as *mut crate::object::PyType
}

unsafe extern "C" fn walk_iter(object: *mut PyObject) -> *mut PyObject {
    object
}

unsafe fn walk_receiver<'a>(object: *mut PyObject) -> Option<&'a mut PyWalk> {
    let object = crate::tag::untag_arg(object);
    if object.is_null() || crate::tag::is_small_int(object) {
        return None;
    }
    if unsafe { (*object).ob_type } == walk_type().cast_const() {
        Some(unsafe { &mut *object.cast::<PyWalk>() })
    } else {
        None
    }
}

fn alloc_walk(top: String, topdown: bool, onerror: *mut PyObject, followlinks: bool, symlinks_as_files: bool) -> *mut PyObject {
    let object = Box::into_raw(Box::new(PyWalk {
        ob_base: crate::object::PyObjectHeader::new(walk_type()),
        stack: vec![WalkStackEntry::Path(top)],
        pending_topdown: None,
        topdown,
        followlinks,
        symlinks_as_files,
        onerror,
    }))
    .cast::<PyObject>();
    WALK_REGISTRY.register::<PyWalk>(object)
}

/// `os.walk(top, topdown=True, onerror=None, followlinks=False)`.
unsafe extern "C" fn os_walk(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 4 {
        return crate::abi::return_null_with_error("os.walk expected 1 to 4 arguments");
    }
    let top = match path_arg(args[0], "walk") {
        Ok(path) => path,
        Err(error) => return error,
    };
    let topdown = match truth_arg(args[1]) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let onerror = if is_none_value(args[2]) {
        std::ptr::null_mut()
    } else {
        args[2]
    };
    let follow_arg = crate::tag::untag_arg(args[3]);
    let (followlinks, symlinks_as_files) = if follow_arg == walk_symlinks_as_files() {
        (false, true)
    } else {
        match truth_arg(args[3]) {
            Ok(value) => (value, false),
            Err(error) => return error,
        }
    };
    alloc_walk(top, topdown, onerror, followlinks, symlinks_as_files)
}

fn truth_arg(object: *mut PyObject) -> Result<bool, *mut PyObject> {
    match unsafe { crate::abi::pon_is_true(object) } {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(std::ptr::null_mut()),
    }
}

unsafe extern "C" fn walk_next(object: *mut PyObject) -> *mut PyObject {
    let Some(walk) = (unsafe { walk_receiver(object) }) else {
        return crate::abi::return_null_with_error("os.walk iterator receiver is invalid");
    };
    if let Err(error) = enqueue_pending_topdown(walk) {
        return error;
    }
    loop {
        let Some(entry) = walk.stack.pop() else {
            return unsafe { crate::abi::exc::pon_raise_stop_iteration(std::ptr::null_mut()) };
        };
        match entry {
            WalkStackEntry::Yield { top, dirs, files } => return build_walk_tuple(&top, dirs, files),
            WalkStackEntry::Path(top) => {
                let scan = match scan_walk_dir(&top, walk.topdown, walk.followlinks, walk.symlinks_as_files) {
                    Ok(scan) => scan,
                    Err(error) => {
                        if let Err(raised) = call_walk_onerror(walk, error) {
                            return raised;
                        }
                        continue;
                    }
                };
                if walk.topdown {
                    return yield_topdown(walk, top, scan);
                }
                walk.stack.push(WalkStackEntry::Yield {
                    top,
                    dirs: scan.dirs,
                    files: scan.files,
                });
                for child in scan.walk_dirs.into_iter().rev() {
                    walk.stack.push(WalkStackEntry::Path(child));
                }
            }
        }
    }
}

fn enqueue_pending_topdown(walk: &mut PyWalk) -> Result<(), *mut PyObject> {
    let Some(pending) = walk.pending_topdown.take() else {
        return Ok(());
    };
    let names = match crate::abi::seq::sequence_to_vec(pending.dirnames) {
        Ok(names) => names,
        Err(message) => return Err(crate::abi::return_null_with_error(message)),
    };
    for name_object in names.into_iter().rev() {
        let name = match path_arg(name_object, "walk") {
            Ok(name) => name,
            Err(error) => return Err(error),
        };
        let child = walk_join(&pending.top, &name);
        if walk.followlinks || walk.symlinks_as_files || !path_is_symlink(&child) {
            walk.stack.push(WalkStackEntry::Path(child));
        }
    }
    Ok(())
}

fn yield_topdown(walk: &mut PyWalk, top: String, scan: WalkScan) -> *mut PyObject {
    let top_object = walk_str(&top);
    let dirnames = super::builtins_batch::build_str_list(scan.dirs);
    let filenames = super::builtins_batch::build_str_list(scan.files);
    if top_object.is_null() || dirnames.is_null() || filenames.is_null() {
        return std::ptr::null_mut();
    }
    walk.pending_topdown = Some(PendingTopDown { top, dirnames });
    build_walk_tuple_objects(top_object, dirnames, filenames)
}

fn build_walk_tuple(top: &str, dirs: Vec<String>, files: Vec<String>) -> *mut PyObject {
    let top_object = walk_str(top);
    let dirnames = super::builtins_batch::build_str_list(dirs);
    let filenames = super::builtins_batch::build_str_list(files);
    build_walk_tuple_objects(top_object, dirnames, filenames)
}

fn build_walk_tuple_objects(top: *mut PyObject, dirs: *mut PyObject, files: *mut PyObject) -> *mut PyObject {
    if top.is_null() || dirs.is_null() || files.is_null() {
        return std::ptr::null_mut();
    }
    let mut items = [top, dirs, files];
    unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

fn walk_str(text: &str) -> *mut PyObject {
    unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

fn scan_walk_dir(top: &str, topdown: bool, followlinks: bool, symlinks_as_files: bool) -> Result<WalkScan, WalkIoError> {
    let entries = std::fs::read_dir(top).map_err(|error| walk_io_error(error, top))?;
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut walk_dirs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| walk_io_error(error, top))?;
        let file_type = entry.file_type();
        let is_dir = match &file_type {
            Ok(file_type) if file_type.is_dir() => true,
            Ok(file_type) if file_type.is_symlink() && !symlinks_as_files => {
                std::fs::metadata(entry.path()).is_ok_and(|metadata| metadata.is_dir())
            }
            Ok(_) | Err(_) => false,
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_dir {
            dirs.push(name);
            if !topdown {
                let is_symlink = file_type.as_ref().is_ok_and(|file_type| file_type.is_symlink());
                if followlinks || !is_symlink {
                    walk_dirs.push(entry.path().to_string_lossy().into_owned());
                }
            }
        } else {
            files.push(name);
        }
    }
    Ok(WalkScan { dirs, files, walk_dirs })
}

fn walk_io_error(error: std::io::Error, path: &str) -> WalkIoError {
    WalkIoError {
        errno: error.raw_os_error().unwrap_or(libc::EIO),
        path: path.to_owned(),
    }
}

fn call_walk_onerror(walk: &PyWalk, error: WalkIoError) -> Result<(), *mut PyObject> {
    if walk.onerror.is_null() {
        return Ok(());
    }
    let exception = match alloc_errno_exception(error.errno, Some(&error.path)) {
        Ok(exception) => exception,
        Err(raised) => return Err(raised),
    };
    let mut args = [exception];
    let result = unsafe { crate::abi::pon_call(walk.onerror, args.as_mut_ptr(), args.len()) };
    if result.is_null() {
        Err(std::ptr::null_mut())
    } else {
        Ok(())
    }
}

fn walk_join(top: &str, name: &str) -> String {
    if name.starts_with('/') {
        name.to_owned()
    } else if top.is_empty() || top.ends_with('/') {
        format!("{top}{name}")
    } else {
        format!("{top}/{name}")
    }
}

fn path_is_symlink(path: &str) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

/// Name / entry / arity rows consumed by [`build_attrs`].  `fdopen`, `dup2`,
/// `open`, and `lstat` are variadic: `fdopen`/`dup2` have optional trailing
/// positionals, `open` has optional `mode`, and path functions may accept a
/// keyword-only `dir_fd` that the native keyword binder flattens into a
/// trailing positional None slot.
const SYSCALL_FUNCTIONS: &[(&str, BuiltinFn, usize)] = &[
    ("WEXITSTATUS", os_wexitstatus, 1),
    ("WIFCONTINUED", os_wifcontinued, 1),
    ("WIFEXITED", os_wifexited, 1),
    ("WIFSIGNALED", os_wifsignaled, 1),
    ("WIFSTOPPED", os_wifstopped, 1),
    ("WSTOPSIG", os_wstopsig, 1),
    ("WTERMSIG", os_wtermsig, 1),
    ("access", os_access, 2),
    ("chdir", os_chdir, 1),
    ("chmod", os_chmod, 2),
    ("close", os_close, 1),
    ("dup", os_dup, 1),
    ("dup2", os_dup2, crate::native::builtins_mod::VARIADIC_ARITY),
    ("fdopen", os_fdopen, crate::native::builtins_mod::VARIADIC_ARITY),
    ("fstat", os_fstat, 1),
    ("getcwd", os_getcwd, 0),
    ("getpid", os_getpid, 0),
    ("getuid", os_getuid, 0),
    ("isatty", os_isatty, 1),
    ("lseek", os_lseek, 3),
    ("lstat", os_lstat, crate::native::builtins_mod::VARIADIC_ARITY),
    ("mkdir", os_mkdir, crate::native::builtins_mod::VARIADIC_ARITY),
    ("open", os_open, crate::native::builtins_mod::VARIADIC_ARITY),
    ("pipe", os_pipe, 0),
    ("putenv", os_putenv, 2),
    ("read", os_read, 2),
    ("readinto", os_readinto, 2),
    ("readlink", os_readlink, 1),
    ("rmdir", os_rmdir, 1),
    ("scandir", os_scandir, 1),
    ("strerror", os_strerror, 1),
    ("umask", os_umask, 1),
    ("unlink", os_unlink, 1),
    ("unsetenv", os_unsetenv, 1),
    ("waitpid", os_waitpid, 2),
    ("waitstatus_to_exitcode", os_waitstatus_to_exitcode, 1),
    ("write", os_write, 2),
];

/// `os.chdir(path)` over `chdir(2)`.
unsafe extern "C" fn os_chdir(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 1 {
        return crate::abi::return_null_with_error("os.chdir expected one argument");
    }
    let path = match path_arg(args[0], "chdir") {
        Ok(path) => path,
        Err(error) => return error,
    };
    let c_path = match c_path(&path) {
        Ok(c_path) => c_path,
        Err(error) => return error,
    };
    // SAFETY: `c_path` is NUL-terminated.
    if unsafe { libc::chdir(c_path.as_ptr()) } < 0 {
        return raise_errno(last_errno(), Some(&path));
    }
    // SAFETY: Singleton accessor.
    unsafe { crate::abi::pon_none() }
}

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

/// `os.getpid()` over `std::process::id` (`test.support.os_helper` reads it
/// at module body: `TESTFN_ASCII` embeds the pid to disambiguate parallel
/// test runs).
unsafe extern "C" fn os_getpid(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    // SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
    unsafe { crate::abi::pon_const_int(i64::from(std::process::id())) }
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
/// Shared with sibling fd-syscall modules (`fcntl`).
pub(crate) fn raise_errno(errno: i32, path: Option<&str>) -> *mut PyObject {
    match alloc_errno_exception(errno, path) {
        Ok(exception) => unsafe { crate::abi::exc::pon_raise(exception, std::ptr::null_mut()) },
        Err(error) => error,
    }
}

fn alloc_errno_exception(errno: i32, path: Option<&str>) -> Result<*mut PyObject, *mut PyObject> {
    let (kind, class_name) = errno_exception(errno);
    let detail = errno_detail(errno);
    let message = errno_message(errno, &detail, path);
    let errno_obj = unsafe { crate::abi::pon_const_int(i64::from(errno)) };
    if errno_obj.is_null() {
        return Err(crate::abi::exc::raise_kind_error_text(kind, &message));
    }
    let detail_obj = unsafe { pon_const_str(detail.as_ptr(), detail.len()) };
    if detail_obj.is_null() {
        return Err(crate::abi::exc::raise_kind_error_text(kind, &message));
    }
    let mut args = vec![errno_obj, detail_obj];
    if let Some(path) = path {
        let path_obj = unsafe { pon_const_str(path.as_ptr(), path.len()) };
        if path_obj.is_null() {
            return Err(crate::abi::exc::raise_kind_error_text(kind, &message));
        }
        args.push(path_obj);
    }
    let Some(class) = crate::abi::runtime_global(intern(class_name)) else {
        return Err(crate::abi::exc::raise_kind_error_text(kind, &message));
    };
    let exception = crate::abi::exc::alloc_exception_instance(class.cast::<crate::object::PyType>(), &args);
    if exception.is_null() {
        Err(std::ptr::null_mut())
    } else {
        Ok(exception)
    }
}

fn errno_exception(errno: i32) -> (ExceptionKind, &'static str) {
    match errno {
        libc::EEXIST => (ExceptionKind::FileExistsError, "FileExistsError"),
        libc::ENOENT => (ExceptionKind::FileNotFoundError, "FileNotFoundError"),
        libc::EISDIR => (ExceptionKind::IsADirectoryError, "IsADirectoryError"),
        libc::ENOTDIR => (ExceptionKind::NotADirectoryError, "NotADirectoryError"),
        libc::EACCES | libc::EPERM => (ExceptionKind::PermissionError, "PermissionError"),
        libc::EINTR => (ExceptionKind::InterruptedError, "InterruptedError"),
        libc::EPIPE => (ExceptionKind::BrokenPipeError, "BrokenPipeError"),
        libc::ECHILD => (ExceptionKind::ChildProcessError, "ChildProcessError"),
        libc::ESRCH => (ExceptionKind::ProcessLookupError, "ProcessLookupError"),
        libc::EAGAIN => (ExceptionKind::BlockingIOError, "BlockingIOError"),
        libc::ETIMEDOUT => (ExceptionKind::TimeoutError, "TimeoutError"),
        libc::ECONNABORTED => (ExceptionKind::ConnectionAbortedError, "ConnectionAbortedError"),
        libc::ECONNREFUSED => (ExceptionKind::ConnectionRefusedError, "ConnectionRefusedError"),
        libc::ECONNRESET => (ExceptionKind::ConnectionResetError, "ConnectionResetError"),
        _ => (ExceptionKind::OSError, "OSError"),
    }
}

fn errno_detail(errno: i32) -> String {
    unsafe { std::ffi::CStr::from_ptr(libc::strerror(errno)) }
        .to_string_lossy()
        .into_owned()
}

fn errno_message(errno: i32, detail: &str, path: Option<&str>) -> String {
    match path {
        Some(path) => format!("[Errno {errno}] {detail}: '{path}'"),
        None => format!("[Errno {errno}] {detail}"),
    }
}

fn last_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}

fn set_fd_cloexec(fd: libc::c_int, cloexec: bool) -> Result<(), i32> {
    // SAFETY: Plain fcntl query; the fd is validated by the kernel.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(last_errno());
    }
    let new_flags = if cloexec {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    // SAFETY: Plain fcntl update; the fd is validated by the kernel.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, new_flags) } < 0 {
        Err(last_errno())
    } else {
        Ok(())
    }
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

/// `os.dup(fd)` over `dup(2)`, matching CPython's non-inheritable default.
unsafe extern "C" fn os_dup(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 1 {
        return crate::abi::return_null_with_error("os.dup expected one argument");
    }
    let fd = match int_arg(args[0], "dup fd") {
        Ok(fd) => fd,
        Err(error) => return error,
    };
    // SAFETY: Plain syscall; the fd is validated by the kernel.
    let duplicated = unsafe { libc::dup(fd as libc::c_int) };
    if duplicated < 0 {
        return raise_errno(last_errno(), None);
    }
    if let Err(errno) = set_fd_cloexec(duplicated, true) {
        // SAFETY: Best-effort cleanup for the just-created descriptor.
        unsafe { libc::close(duplicated) };
        return raise_errno(errno, None);
    }
    // SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
    unsafe { crate::abi::pon_const_int(i64::from(duplicated)) }
}

/// `os.dup2(fd, fd2, inheritable=True)` over `dup2(2)`.
unsafe extern "C" fn os_dup2(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if !(2..=3).contains(&args.len()) {
        return crate::abi::return_null_with_error("os.dup2 expected two or three arguments");
    }
    let fd = match int_arg(args[0], "dup2 fd") {
        Ok(fd) => fd,
        Err(error) => return error,
    };
    let fd2 = match int_arg(args[1], "dup2 fd2") {
        Ok(fd) => fd,
        Err(error) => return error,
    };
    let inheritable = if let Some(&value) = args.get(2) {
        match int_arg(value, "dup2 inheritable") {
            Ok(value) => value != 0,
            Err(error) => return error,
        }
    } else {
        true
    };
    // SAFETY: Plain syscall; descriptors are validated by the kernel.
    let duplicated = unsafe { libc::dup2(fd as libc::c_int, fd2 as libc::c_int) };
    if duplicated < 0 {
        return raise_errno(last_errno(), None);
    }
    if let Err(errno) = set_fd_cloexec(duplicated, !inheritable) {
        return raise_errno(errno, None);
    }
    // SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
    unsafe { crate::abi::pon_const_int(i64::from(duplicated)) }
}

/// `os.fdopen(fd, ...)`: CPython's thin alias for `open(fd, ...)`.
unsafe extern "C" fn os_fdopen(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc == 0 {
        return crate::abi::return_null_with_error("os.fdopen expected at least one argument");
    }
    // SAFETY: Same argv/argc contract as builtin `open`.
    unsafe { super::io::builtin_open(argv, argc) }
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
/// `os.readinto(fd, buffer)` over `read(2)`: fills a writable bytes-like
/// target in place and returns the byte count. `_pyio.FileIO.readinto`
/// dispatches here directly.
unsafe extern "C" fn os_readinto(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 2 {
        return crate::abi::return_null_with_error("os.readinto expected two arguments");
    }
    let fd = match int_arg(args[0], "readinto fd") {
        Ok(fd) => fd,
        Err(error) => return error,
    };
    let target = crate::tag::untag_arg(args[1]);
    let (dst, dst_len) = match writable_bytes_target(target) {
        Ok(parts) => parts,
        Err(error) => return error,
    };
    // SAFETY: `dst` addresses `dst_len` writable bytes for the syscall fill.
    let count = unsafe { libc::read(fd as libc::c_int, dst.cast(), dst_len) };
    if count < 0 {
        return raise_errno(last_errno(), None);
    }
    // SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
    unsafe { crate::abi::pon_const_int(count as i64) }
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

/// `os.lseek(fd, position, whence)` over `lseek(2)`: returns the resulting
/// offset.  The whence argument takes the `SEEK_*` constants above;
/// validation is the host's (EINVAL for junk whence/offset combinations),
/// exactly like CPython's thin wrapper.
unsafe extern "C" fn os_lseek(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 3 {
        return crate::abi::return_null_with_error("os.lseek expected three arguments");
    }
    let fd = match int_arg(args[0], "lseek fd") {
        Ok(fd) => fd,
        Err(error) => return error,
    };
    let position = match int_arg(args[1], "lseek position") {
        Ok(position) => position,
        Err(error) => return error,
    };
    let whence = match int_arg(args[2], "lseek whence") {
        Ok(whence) => whence,
        Err(error) => return error,
    };
    // SAFETY: Plain fd syscall; failure reports through errno below.
    let offset = unsafe { libc::lseek(fd as libc::c_int, position, whence as libc::c_int) };
    if offset < 0 {
        return raise_errno(last_errno(), None);
    }
    // SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
    unsafe { crate::abi::pon_const_int(offset) }
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
/// Borrows a writable bytearray/memoryview target for `os.readinto`.
fn writable_bytes_target(object: *mut PyObject) -> Result<(*mut u8, usize), *mut PyObject> {
    if object.is_null() {
        return Err(crate::abi::exc::raise_kind_error_text(
            ExceptionKind::TypeError,
            "readinto() argument must be read-write bytes-like object, not 'NoneType'",
        ));
    }
    if crate::tag::is_small_int(object) {
        return Err(crate::abi::exc::raise_kind_error_text(
            ExceptionKind::TypeError,
            "readinto() argument must be read-write bytes-like object, not int",
        ));
    }
    // SAFETY: Heap pointer with a live header after the tag checks.
    let ty = unsafe { (*object).ob_type };
    if crate::types::bytearray_::is_bytearray_type(ty) {
        let bytearray = unsafe { &mut *object.cast::<crate::types::bytearray_::PyByteArray>() };
        return Ok((bytearray.bytes.as_mut_ptr(), bytearray.bytes.len()));
    }
    if crate::types::memoryview::is_memoryview_type(ty) {
        let view = unsafe { &mut *object.cast::<crate::types::memoryview::PyMemoryView>() };
        if view.released {
            return Err(unsafe {
                crate::abi::exc::pon_raise_value_error(
                    crate::types::memoryview::RELEASED_ERROR.as_ptr(),
                    crate::types::memoryview::RELEASED_ERROR.len(),
                )
            });
        }
        if view.readonly {
            return Err(crate::abi::exc::raise_kind_error_text(
                ExceptionKind::TypeError,
                "readinto() argument must be read-write bytes-like object, not memoryview",
            ));
        }
        return Ok((view.data, view.len));
    }
    let type_name = unsafe { crate::types::dict::type_name(object) }.unwrap_or("object");
    Err(crate::abi::exc::raise_kind_error_text(
        ExceptionKind::TypeError,
        &format!("readinto() argument must be read-write bytes-like object, not {type_name}"),
    ))
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

/// `os.pipe()` over `pipe(2)`: returns the `(read_fd, write_fd)` pair.
unsafe extern "C" fn os_pipe(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` is the 2-element array `pipe(2)` writes into.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
        return raise_errno(last_errno(), None);
    }
    if let Err(errno) = set_fd_cloexec(fds[0], true).and_then(|()| set_fd_cloexec(fds[1], true)) {
        // SAFETY: Best-effort cleanup for the just-created descriptors.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        return raise_errno(errno, None);
    }
    // SAFETY: Singleton/boxing accessors follow the NULL-sentinel contract.
    let mut items = unsafe {
        [
            crate::abi::pon_const_int(i64::from(fds[0])),
            crate::abi::pon_const_int(i64::from(fds[1])),
        ]
    };
    // SAFETY: `items` holds two live boxed ints.
    unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

struct MakedirsFailure {
    errno: i32,
    path: String,
}

/// `os.makedirs(name, mode=0o777, exist_ok=False)`; creates missing parents
/// with the default directory mode and applies `mode` only to the leaf.
unsafe extern "C" fn os_makedirs(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if !(1..=3).contains(&args.len()) {
        return crate::abi::return_null_with_error("os.makedirs expected 1 to 3 arguments (name, mode=0o777, exist_ok=False)");
    }
    let path = match path_arg(args[0], "makedirs") {
        Ok(path) => path,
        Err(error) => return error,
    };
    if let Err(error) = c_path(&path) {
        return error;
    }
    let mode = match optional_arg(args, 1).map(|object| int_arg(object, "makedirs mode")) {
        None => 0o777,
        Some(Ok(mode)) => mode,
        Some(Err(error)) => return error,
    };
    let exist_ok = match optional_arg(args, 2) {
        None => false,
        Some(object) => match unsafe { crate::abi::pon_is_true(object) } {
            0 => false,
            1 => true,
            _ => return std::ptr::null_mut(),
        },
    };
    match makedirs_impl(&path, mode as libc::mode_t, exist_ok) {
        Ok(()) => unsafe { crate::abi::pon_none() },
        Err(error) => raise_errno(error.errno, Some(error.path.as_str())),
    }
}

fn makedirs_impl(path: &str, mode: libc::mode_t, exist_ok: bool) -> Result<(), MakedirsFailure> {
    let (mut head, mut tail) = split_posix_path(path);
    if tail.is_empty() {
        let split = split_posix_path(&head);
        head = split.0;
        tail = split.1;
    }
    if !head.is_empty() && !tail.is_empty() && !path_exists(&head) {
        if let Err(error) = makedirs_impl(&head, 0o777 as libc::mode_t, exist_ok) {
            if error.errno != libc::EEXIST {
                return Err(error);
            }
        }
        if tail == "." {
            return Ok(());
        }
    }
    match mkdir_errno(path, mode) {
        Ok(()) => Ok(()),
        Err(_) if exist_ok && path_is_dir(path) => Ok(()),
        Err(errno) => Err(MakedirsFailure {
            errno,
            path: path.to_owned(),
        }),
    }
}

fn split_posix_path(path: &str) -> (String, String) {
    let Some(last_sep) = path.rfind('/') else {
        return (String::new(), path.to_owned());
    };
    let split_at = last_sep + 1;
    let mut head = &path[..split_at];
    let tail = &path[split_at..];
    if !head.is_empty() && !head.bytes().all(|byte| byte == b'/') {
        head = head.trim_end_matches('/');
    }
    (head.to_owned(), tail.to_owned())
}

fn path_exists(path: &str) -> bool {
    std::fs::metadata(path).is_ok()
}

fn path_is_dir(path: &str) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

fn mkdir_errno(path: &str, mode: libc::mode_t) -> Result<(), i32> {
    let c_path = std::ffi::CString::new(path).expect("makedirs path was prechecked for NUL");
    // SAFETY: `c_path` is NUL-terminated.
    if unsafe { libc::mkdir(c_path.as_ptr(), mode) } < 0 {
        Err(last_errno())
    } else {
        Ok(())
    }
}

/// `os.mkdir(path, mode=0o777, *, dir_fd=None)` over `mkdir(2)`; the mode is
/// masked by the process umask exactly like the syscall.
unsafe extern "C" fn os_mkdir(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if !(1..=3).contains(&args.len()) {
        let message = "os.mkdir expected 1 to 2 arguments (path, mode=0o777)";
        return crate::abi::return_null_with_error(message);
    }
    let path = match path_arg(args[0], "mkdir") {
        Ok(path) => path,
        Err(error) => return error,
    };
    let mode = match optional_arg(args, 1).map(|object| int_arg(object, "mkdir mode")) {
        None => 0o777,
        Some(Ok(mode)) => mode,
        Some(Err(error)) => return error,
    };
    if let Err(error) = reject_dir_fd(args, 2, "mkdir") {
        return error;
    }
    let c_path = match c_path(&path) {
        Ok(c_path) => c_path,
        Err(error) => return error,
    };
    // SAFETY: `c_path` is NUL-terminated.
    if unsafe { libc::mkdir(c_path.as_ptr(), mode as libc::mode_t) } < 0 {
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

/// `os.strerror(errno)`: host strerror table exposed as a Python string.
unsafe extern "C" fn os_strerror(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 1 {
        return crate::abi::return_null_with_error("os.strerror expected one argument");
    }
    let errno = match int_arg(args[0], "strerror code") {
        Ok(errno) => errno,
        Err(error) => return error,
    };
    // SAFETY: `strerror` returns a NUL-terminated static message.
    let detail = unsafe { std::ffi::CStr::from_ptr(libc::strerror(errno as libc::c_int)) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: String allocation helper follows the NULL-sentinel contract.
    unsafe { pon_const_str(detail.as_ptr(), detail.len()) }
}

/// Single int `status` word shared by the wait-status inspectors.
fn status_arg(argv: *mut *mut PyObject, argc: usize, what: &str) -> Result<libc::c_int, *mut PyObject> {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 1 {
        return Err(crate::abi::return_null_with_error(format!("os.{what} expected one argument")));
    }
    int_arg(args[0], what).map(|status| status as libc::c_int)
}

/// `os.waitpid(pid, options)` over `waitpid(2)`: `(pid, status)` tuple.
/// With nothing to reap the host answers ECHILD, surfaced as CPython's
/// ChildProcessError — exactly what `subprocess.Popen.__del__`'s reaper and
/// asyncio's child watchers catch on their no-child paths.
unsafe extern "C" fn os_waitpid(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() != 2 {
        return crate::abi::return_null_with_error("os.waitpid expected two arguments");
    }
    let pid = match int_arg(args[0], "waitpid pid") {
        Ok(pid) => pid,
        Err(error) => return error,
    };
    let options = match int_arg(args[1], "waitpid options") {
        Ok(options) => options,
        Err(error) => return error,
    };
    let mut status: libc::c_int = 0;
    // SAFETY: `status` is a live out-slot for the syscall to fill.
    let reaped = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, options as libc::c_int) };
    if reaped < 0 {
        return raise_errno(last_errno(), None);
    }
    // SAFETY: Integer boxing helpers follow the NULL-sentinel error contract.
    let mut items = [unsafe { crate::abi::pon_const_int(i64::from(reaped)) }, unsafe {
        crate::abi::pon_const_int(i64::from(status))
    }];
    if items.iter().any(|item| item.is_null()) {
        return std::ptr::null_mut();
    }
    // SAFETY: `items` is a live window for the duration of the call.
    unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

/// `os.WIFEXITED(status)`: true when the child exited normally.
unsafe extern "C" fn os_wifexited(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    match status_arg(argv, argc, "WIFEXITED") {
        Ok(status) => bool_object(libc::WIFEXITED(status)),
        Err(error) => error,
    }
}

/// `os.WEXITSTATUS(status)`: the low 8-bit exit status from a normal exit.
unsafe extern "C" fn os_wexitstatus(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    match status_arg(argv, argc, "WEXITSTATUS") {
        Ok(status) => {
            // SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
            unsafe { crate::abi::pon_const_int(i64::from(libc::WEXITSTATUS(status))) }
        }
        Err(error) => error,
    }
}

/// `os.WIFSIGNALED(status)`: true when the child was terminated by a signal.
unsafe extern "C" fn os_wifsignaled(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    match status_arg(argv, argc, "WIFSIGNALED") {
        Ok(status) => bool_object(libc::WIFSIGNALED(status)),
        Err(error) => error,
    }
}

/// `os.WTERMSIG(status)`: the signal that terminated the child.
unsafe extern "C" fn os_wtermsig(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    match status_arg(argv, argc, "WTERMSIG") {
        Ok(status) => {
            // SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
            unsafe { crate::abi::pon_const_int(i64::from(libc::WTERMSIG(status))) }
        }
        Err(error) => error,
    }
}

/// `os.WIFCONTINUED(status)`: true when the child resumed after job-control stop.
unsafe extern "C" fn os_wifcontinued(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    match status_arg(argv, argc, "WIFCONTINUED") {
        Ok(status) => bool_object(libc::WIFCONTINUED(status)),
        Err(error) => error,
    }
}

/// `os.waitstatus_to_exitcode(status)`: pure status-word math, exactly
/// CPython's `os_waitstatus_to_exitcode_impl` — `WEXITSTATUS` for a normal
/// exit, `-WTERMSIG` for a signal death, ValueError for stopped/invalid
/// words.  `subprocess._handle_exitstatus` calls it on every reaped status.
unsafe extern "C" fn os_waitstatus_to_exitcode(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let status = match status_arg(argv, argc, "waitstatus_to_exitcode") {
        Ok(status) => status,
        Err(error) => return error,
    };
    let exitcode = if libc::WIFEXITED(status) {
        i64::from(libc::WEXITSTATUS(status))
    } else if libc::WIFSIGNALED(status) {
        -i64::from(libc::WTERMSIG(status))
    } else if libc::WIFSTOPPED(status) {
        return crate::abi::exc::raise_kind_error_text(
            ExceptionKind::ValueError,
            &format!("process stopped by delivery of signal {}", libc::WSTOPSIG(status)),
        );
    } else {
        return crate::abi::exc::raise_kind_error_text(
            ExceptionKind::ValueError,
            &format!("invalid wait status: {status}"),
        );
    };
    // SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
    unsafe { crate::abi::pon_const_int(exitcode) }
}

/// `os.WIFSTOPPED(status)`: true when the word reports a stopped child.
/// `subprocess._del_safe` binds it at import time for the `__del__` reaper.
unsafe extern "C" fn os_wifstopped(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    match status_arg(argv, argc, "WIFSTOPPED") {
        Ok(status) => bool_object(libc::WIFSTOPPED(status)),
        Err(error) => error,
    }
}

/// `os.WSTOPSIG(status)`: the signal that stopped the child (import-time
/// `subprocess._del_safe` binding, read next to `WIFSTOPPED`).
unsafe extern "C" fn os_wstopsig(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    match status_arg(argv, argc, "WSTOPSIG") {
        Ok(status) => {
            // SAFETY: Integer boxing helper follows the NULL-sentinel error contract.
            unsafe { crate::abi::pon_const_int(i64::from(libc::WSTOPSIG(status))) }
        }
        Err(error) => error,
    }
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

// ---------------------------------------------------------------------------
// Environment write-through: `putenv`/`unsetenv` (C `posix` pair, shared
// with the `posix` module like the rest of the syscall table) and the
// `os.py`-level `getenv`.  See [`environ_snapshot`] for the decision record
// on why `os.environ` itself stays a plain dict.

/// str/bytes/PathLike argument for the environment write-through pair:
/// `os.fspath` coercion first (with its exact CPython TypeError), then the
/// byte payload (str as UTF-8, bytes raw).
unsafe fn env_bytes_arg(slot: *mut PyObject, what: &str) -> Result<Vec<u8>, *mut PyObject> {
    let mut argv = [slot];
    // SAFETY: One live argument slot built above.
    let coerced = unsafe { os_fspath(argv.as_mut_ptr(), 1) };
    if coerced.is_null() {
        return Err(std::ptr::null_mut());
    }
    let raw = crate::tag::untag_arg(coerced);
    if !raw.is_null() && !crate::tag::is_small_int(raw) {
        // SAFETY: Heap pointer with a live header after the tag checks.
        if let Some(text) = unsafe { crate::types::type_::unicode_text(raw) } {
            return Ok(text.as_bytes().to_vec());
        }
        // SAFETY: Live header per the checks above.
        if crate::types::bytes_::is_bytes_type(unsafe { (*raw).ob_type }) {
            // SAFETY: The type check proved PyBytes layout.
            return Ok(unsafe { (*raw.cast::<crate::types::bytes_::PyBytes>()).as_slice() }.to_vec());
        }
    }
    Err(fs_codec_hook_type_error(what, raw))
}

/// `os.putenv(name, value)` / `posix.putenv`: writes through to the REAL
/// process environment (`setenv(3)` shape).  Exactly like CPython, the call
/// does NOT touch the `os.environ` dict — CPython documents that direct
/// `putenv` calls "don't update os.environ" — so `putenv(k, v); getenv(k)`
/// returns None on both engines (see [`environ_snapshot`]).
unsafe extern "C" fn os_putenv(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 2 || argv.is_null() {
        return crate::abi::return_null_with_error(format!("putenv expected 2 arguments, got {argc}"));
    }
    // SAFETY: Two live argument slots per the check above.
    let name = match unsafe { env_bytes_arg(*argv, "putenv") } {
        Ok(bytes) => bytes,
        Err(raised) => return raised,
    };
    // SAFETY: As above.
    let value = match unsafe { env_bytes_arg(*argv.add(1), "putenv") } {
        Ok(bytes) => bytes,
        Err(raised) => return raised,
    };
    if name.contains(&0) || value.contains(&0) {
        let message = "embedded null byte";
        // SAFETY: Typed raise helper.
        return unsafe { crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) };
    }
    if name.contains(&b'=') {
        let message = "illegal environment variable name";
        // SAFETY: Typed raise helper.
        return unsafe { crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) };
    }
    if name.is_empty() {
        // macOS setenv(3) rejects an empty name; CPython surfaces the errno.
        return raise_errno(libc::EINVAL, None);
    }
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: The `set_var` panic preconditions (empty name, '=', NUL) are
    // pre-checked above and raise Python errors instead; the remaining
    // concurrent-getenv data-race contract is setenv(3)'s own, which
    // CPython's putenv shares.
    unsafe {
        std::env::set_var(
            std::ffi::OsStr::from_bytes(&name),
            std::ffi::OsStr::from_bytes(&value),
        );
    }
    // SAFETY: Singleton accessor.
    unsafe { crate::abi::pon_none() }
}

/// `os.unsetenv(name)` / `posix.unsetenv`: removes `name` from the REAL
/// process environment (`unsetenv(3)` shape); the `os.environ` dict is —
/// like CPython — left untouched.
unsafe extern "C" fn os_unsetenv(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 || argv.is_null() {
        return crate::abi::return_null_with_error(format!("unsetenv expected 1 argument, got {argc}"));
    }
    // SAFETY: One live argument slot per the check above.
    let name = match unsafe { env_bytes_arg(*argv, "unsetenv") } {
        Ok(bytes) => bytes,
        Err(raised) => return raised,
    };
    if name.contains(&0) {
        let message = "embedded null byte";
        // SAFETY: Typed raise helper.
        return unsafe { crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) };
    }
    if name.is_empty() || name.contains(&b'=') {
        // macOS unsetenv(3) rejects empty names and embedded '='; CPython
        // surfaces the errno.
        return raise_errno(libc::EINVAL, None);
    }
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: The `remove_var` panic preconditions (empty name, '=', NUL)
    // are pre-checked above; the concurrent-getenv data-race contract is
    // unsetenv(3)'s own, which CPython shares.
    unsafe { std::env::remove_var(std::ffi::OsStr::from_bytes(&name)) };
    // SAFETY: Singleton accessor.
    unsafe { crate::abi::pon_none() }
}

/// `os.get_exec_path(env=None)`: returns the PATH search directories used
/// by `subprocess` when the executable name has no directory component.
unsafe extern "C" fn os_get_exec_path(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Live argument slots per the runtime calling convention.
    let args = unsafe { call_args(argv, argc) };
    if args.len() > 1 {
        return crate::abi::return_null_with_error("get_exec_path() takes at most 1 argument");
    }
    let path = if args.first().copied().is_none_or(is_none_value) {
        std::env::var("PATH").ok()
    } else {
        match env_mapping_path(args[0]) {
            Ok(path) => path,
            Err(error) => return error,
        }
    }
    .unwrap_or_else(default_exec_path);
    super::builtins_batch::build_str_list(path.split(':').map(str::to_owned).collect())
}

fn default_exec_path() -> String {
    if cfg!(windows) {
        ".;C:\\\\bin".to_owned()
    } else {
        "/bin:/usr/bin".to_owned()
    }
}

fn env_mapping_path(env: *mut PyObject) -> Result<Option<String>, *mut PyObject> {
    let key = unsafe { pon_const_str(b"PATH".as_ptr(), 4) };
    if key.is_null() {
        return Err(std::ptr::null_mut());
    }
    let default = unsafe { crate::abi::pon_none() };
    if default.is_null() {
        return Err(std::ptr::null_mut());
    }
    // SAFETY: Live mapping object; missing/failing `.get` propagates like
    // `os.py`'s `env.get('PATH')` expression.
    let get = unsafe { crate::abi::pon_get_attr(env, intern("get"), std::ptr::null_mut()) };
    if get.is_null() {
        return Err(std::ptr::null_mut());
    }
    let mut call_argv = [key, default];
    // SAFETY: Live bound method and two live argument slots.
    let value = unsafe { crate::abi::pon_call(get, call_argv.as_mut_ptr(), call_argv.len()) };
    if value.is_null() {
        return Err(std::ptr::null_mut());
    }
    path_string_from_value(value).map(Some)
}

fn path_string_from_value(value: *mut PyObject) -> Result<String, *mut PyObject> {
    if is_none_value(value) {
        return Ok(default_exec_path());
    }
    let raw = crate::tag::untag_arg(value);
    if raw.is_null() || crate::tag::is_small_int(raw) {
        return Err(crate::abi::exc::raise_kind_error_text(
            ExceptionKind::TypeError,
            "PATH must be str or bytes",
        ));
    }
    // SAFETY: Heap pointer with a live header after the checks above.
    if let Some(text) = unsafe { crate::types::type_::unicode_text(raw) } {
        return Ok(text.to_owned());
    }
    if let Some(payload) = bytes_payload(raw) {
        return match std::str::from_utf8(payload) {
            Ok(text) => Ok(text.to_owned()),
            Err(_) => Err(crate::abi::exc::raise_kind_error_text(
                ExceptionKind::UnicodeDecodeError,
                "PATH bytes are not valid UTF-8",
            )),
        };
    }
    Err(crate::abi::exc::raise_kind_error_text(
        ExceptionKind::TypeError,
        "PATH must be str or bytes",
    ))
}

fn is_none_value(object: *mut PyObject) -> bool {
    if object.is_null() {
        return true;
    }
    let raw = crate::tag::untag_arg(object);
    if raw.is_null() || crate::tag::is_small_int(raw) {
        return false;
    }
    // SAFETY: Heap pointer with a live header after the checks above.
    unsafe { crate::types::dict::type_name(raw) == Some("NoneType") }
}

/// `os.getenv(key, default=None)`: `os.py`'s Python-level helper, served
/// natively.  Reads the LIVE `os.environ` module binding — rebinding
/// `os.environ`, as `test.support.os_helper.EnvironmentVarGuard.__exit__`
/// does, changes what getenv consults, exactly like the os.py module-global
/// read — then defers to `environ.get(key, default)` through attribute
/// dispatch so any mapping works.  The key must be str, matching
/// `_Environ.encodekey`'s check (a plain dict `.get` would silently return
/// the default).
unsafe extern "C" fn os_getenv(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc == 0 || argv.is_null() {
        let message = "getenv() missing 1 required positional argument: 'key'";
        // SAFETY: Typed raise helper.
        return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    }
    if argc > 2 {
        let message = format!("getenv() takes from 1 to 2 positional arguments but {argc} were given");
        // SAFETY: Typed raise helper.
        return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    }
    // SAFETY: `argc` live argument slots per the checks above.
    let key = unsafe { *argv };
    let raw_key = crate::tag::untag_arg(key);
    if raw_key.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: Heap-or-NULL after `untag_arg`; NULL was handled above.
    if crate::tag::is_small_int(raw_key) || unsafe { crate::types::type_::unicode_text(raw_key) }.is_none() {
        let display = if crate::tag::is_small_int(raw_key) {
            "int"
        } else {
            // SAFETY: Heap pointer with a live header after the tag checks.
            unsafe { crate::types::dict::type_name(raw_key) }.unwrap_or("object")
        };
        let message = format!("str expected, not {display}");
        // SAFETY: Typed raise helper.
        return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    }
    let default = if argc == 2 {
        // SAFETY: Two live argument slots per the argc check.
        unsafe { *argv.add(1) }
    } else {
        // SAFETY: Singleton accessor.
        unsafe { crate::abi::pon_none() }
    };
    let Some(environ) = crate::import::module_attr(intern("os"), intern("environ")) else {
        // `del os.environ` leaves getenv reading a missing global — the
        // exact failure os.py's `environ.get` produces.
        let message = "name 'environ' is not defined";
        return crate::abi::exc::raise_kind_error_text(ExceptionKind::NameError, message);
    };
    // SAFETY: Live environ binding; a missing or failing `get` attribute
    // propagates its own AttributeError, exactly like os.py's
    // `environ.get(key, default)` expression.
    let get = unsafe { crate::abi::pon_get_attr(environ, intern("get"), std::ptr::null_mut()) };
    if get.is_null() {
        return std::ptr::null_mut();
    }
    let mut call_argv = [key, default];
    // SAFETY: Live bound method and two live argument slots.
    unsafe { crate::abi::pon_call(get, call_argv.as_mut_ptr(), call_argv.len()) }
}
