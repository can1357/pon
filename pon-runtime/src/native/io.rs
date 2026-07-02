//! Native `_io` module seed plus the `open()` file-object backing store.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::ptr;
use std::sync::LazyLock;

use crate::abi::{self, pon_const_str};
use crate::builtins;
use crate::intern::intern;
use crate::object::{PyLong, PyObject, PyObjectHeader, PyType};
use crate::thread_state::{pon_err_clear, pon_err_message, pon_err_occurred, pon_err_set};
use crate::types::{bytearray_, bytes_, method, type_};

use super::builtins_mod::VARIADIC_ARITY;
use super::install_module;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NewlineMode {
    /// `newline=None`: recognize `\n`, `\r\n`, and `\r`, returning `\n` in text mode.
    UniversalTranslate,
    /// Any explicit newline value: keep bytes as they appear on disk for this phase.
    Preserve,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct OpenMode {
    display: String,
    binary: bool,
    readable: bool,
    writable: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
}

#[repr(C)]
#[derive(Debug)]
pub(crate) struct PyNativeFile {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Native host file handle. `None` is the closed state.
    file: Option<File>,
    /// Python-visible `name` attribute, stored as UTF-8 path text.
    name: String,
    /// Python-visible mode string as supplied/normalized by `open()`.
    mode: String,
    /// `true` for binary mode; `false` for text mode.
    binary: bool,
    /// Read operations are permitted.
    readable: bool,
    /// Write operations are permitted.
    writable: bool,
    /// Writes use append semantics.
    append: bool,
    /// Text encoding name. `None` for binary files; text files default to UTF-8.
    encoding: Option<String>,
    /// Text newline handling policy.
    newline: NewlineMode,
}

unsafe impl Send for PyNativeFile {}

static TEXT_FILE_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = Box::new(PyType::new(
        abi::runtime_type_type().cast_const(),
        "TextIOWrapper",
        std::mem::size_of::<PyNativeFile>(),
    ));
    ty.tp_getattro = Some(file_getattro);
    ty.tp_iter = Some(file_iter_slot);
    ty.tp_iternext = Some(file_iternext_slot);
    Box::into_raw(ty) as usize
});

static BINARY_FILE_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = Box::new(PyType::new(
        abi::runtime_type_type().cast_const(),
        "FileIO",
        std::mem::size_of::<PyNativeFile>(),
    ));
    ty.tp_getattro = Some(file_getattro);
    ty.tp_iter = Some(file_iter_slot);
    ty.tp_iternext = Some(file_iternext_slot);
    Box::into_raw(ty) as usize
});

fn text_file_type() -> *mut PyType {
    *TEXT_FILE_TYPE as *mut PyType
}

fn binary_file_type() -> *mut PyType {
    *BINARY_FILE_TYPE as *mut PyType
}

/// Stream methods stubbed on `_io._IOBase`: `import io` only needs the heap
/// classes to exist for subclassing/ABC registration, so unimplemented
/// operations raise an honest `NotImplementedError` when actually called.
const STREAM_METHOD_STUBS: &[&str] = &[
    "read",
    "read1",
    "readinto",
    "readline",
    "readlines",
    "write",
    "writelines",
    "seek",
    "tell",
    "truncate",
    "flush",
    "close",
    "detach",
    "fileno",
    "isatty",
    "readable",
    "writable",
    "seekable",
    "getvalue",
];

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let os_error = builtin_class("OSError")?;
    let value_error = builtin_class("ValueError")?;
    let io_base = heap_class(
        "_IOBase",
        &[],
        "The abstract base class for all I/O classes.",
        STREAM_METHOD_STUBS,
    )?;
    let raw_io_base = heap_class("_RawIOBase", &[io_base], "Base class for raw binary I/O.", &[])?;
    let buffered_io_base = heap_class("_BufferedIOBase", &[io_base], "Base class for buffered IO objects.", &[])?;
    let text_io_base = heap_class("_TextIOBase", &[io_base], "Base class for text I/O.", &[])?;
    // Link the pinned native file types under the fresh abstract bases so
    // `FileIO.__mro__`/`isinstance` walk the CPython-shaped chain. Guarded for
    // idempotence: the statics survive module re-creation.
    unsafe {
        let binary = binary_file_type();
        if (*binary).tp_base.is_null() {
            (*binary).tp_base = raw_io_base.cast::<PyType>();
            (*binary).bump_version();
        }
        let text = text_file_type();
        if (*text).tp_base.is_null() {
            (*text).tp_base = text_io_base.cast::<PyType>();
            (*text).bump_version();
        }
    }
    let attrs = vec![
        string_attr("__name__", "_io")?,
        int_attr("DEFAULT_BUFFER_SIZE", 131_072)?,
        string_attr("stdout", "<stdout>")?,
        function_attr("open", builtin_open, VARIADIC_ARITY)?,
        function_attr("open_code", open_code_entry, 1)?,
        function_attr("text_encoding", text_encoding_entry, VARIADIC_ARITY)?,
        (intern("BlockingIOError"), builtin_class("BlockingIOError")?),
        (
            intern("UnsupportedOperation"),
            heap_class(
                "UnsupportedOperation",
                &[os_error, value_error],
                "The stream does not support this operation.",
                &[],
            )?,
        ),
        (intern("_IOBase"), io_base),
        (intern("_RawIOBase"), raw_io_base),
        (intern("_BufferedIOBase"), buffered_io_base),
        (intern("_TextIOBase"), text_io_base),
        (intern("FileIO"), binary_file_type().cast::<PyObject>()),
        (intern("TextIOWrapper"), text_file_type().cast::<PyObject>()),
        (
            intern("BytesIO"),
            heap_class(
                "BytesIO",
                &[buffered_io_base],
                "Buffered I/O implementation using an in-memory bytes buffer.",
                &[],
            )?,
        ),
        (
            intern("StringIO"),
            heap_class("StringIO", &[text_io_base], "Text I/O implementation using an in-memory buffer.", &[])?,
        ),
        (
            intern("BufferedReader"),
            heap_class(
                "BufferedReader",
                &[buffered_io_base],
                "Create a new buffered reader using the given readable raw IO object.",
                &[],
            )?,
        ),
        (
            intern("BufferedWriter"),
            heap_class(
                "BufferedWriter",
                &[buffered_io_base],
                "A buffer for a writeable sequential RawIO object.",
                &[],
            )?,
        ),
        (
            intern("BufferedRandom"),
            heap_class(
                "BufferedRandom",
                &[buffered_io_base],
                "A buffered interface to random access streams.",
                &[],
            )?,
        ),
        (
            intern("BufferedRWPair"),
            heap_class(
                "BufferedRWPair",
                &[buffered_io_base],
                "A buffered reader and writer object together.",
                &[],
            )?,
        ),
        (
            intern("IncrementalNewlineDecoder"),
            heap_class(
                "IncrementalNewlineDecoder",
                &[],
                "Codec used when reading a file in universal newlines mode.",
                &[],
            )?,
        ),
    ];
    install_module("_io", attrs)
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate _io.{name}"))
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: `pon_const_int` returns NULL with a diagnostic on failure.
    let object = unsafe { abi::pon_const_int(value) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate _io.{name}"))
}

fn function_attr(
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
    arity: usize,
) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: `pon_make_function` returns NULL with a diagnostic on failure.
    let object = unsafe { abi::pon_make_function(entry as *const u8, arity, intern(name)) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate _io.{name}"))
}

/// Resolves a builtin class object (exception types live in the builtin
/// globals) for re-export from `_io`; CPython's `_io.BlockingIOError` IS
/// `builtins.BlockingIOError`.
fn builtin_class(name: &str) -> Result<*mut PyObject, String> {
    // SAFETY: `pon_load_global` returns NULL with a raised NameError on miss.
    let object = unsafe { abi::pon_load_global(intern(name), ptr::null_mut()) };
    if object.is_null() {
        pon_err_clear();
        return Err(format!("builtin class '{name}' is not registered"));
    }
    Ok(object)
}

/// Builds one minimally-correct `_io` heap class: real `type` instance with
/// `__doc__`/`__module__` set (vendored `io.py` copies `__doc__` from the
/// abstract bases) plus optional honest-failure method stubs.
fn heap_class(
    name: &str,
    bases: &[*mut PyObject],
    doc: &str,
    method_stubs: &[&str],
) -> Result<*mut PyObject, String> {
    let namespace = type_::new_namespace();
    if namespace.is_null() {
        return Err(format!("failed to allocate _io.{name} namespace"));
    }
    let doc_object = unsafe { pon_const_str(doc.as_ptr(), doc.len()) };
    if doc_object.is_null() {
        return Err(format!("failed to allocate _io.{name}.__doc__"));
    }
    let module_object = unsafe { pon_const_str("_io".as_ptr(), "_io".len()) };
    if module_object.is_null() {
        return Err(format!("failed to allocate _io.{name}.__module__"));
    }
    // SAFETY: `new_namespace` returned a live namespace box.
    unsafe {
        (*namespace).set(intern("__doc__"), doc_object);
        (*namespace).set(intern("__module__"), module_object);
    }
    for &method_name in method_stubs {
        let function =
            unsafe { abi::pon_make_function(io_stub_method as *const u8, VARIADIC_ARITY, intern(method_name)) };
        if function.is_null() {
            return Err(format!("failed to allocate _io.{name}.{method_name}"));
        }
        // SAFETY: Namespace is live; the function object is a valid attr value.
        unsafe { (*namespace).set(intern(method_name), function) };
    }
    // SAFETY: Bases are live class objects owned by the runtime.
    let class = unsafe { type_::build_class_from_namespace(name, bases, namespace, &[]) };
    if class.is_null() {
        let detail = pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
        pon_err_clear();
        return Err(format!("failed to create _io.{name}: {detail}"));
    }
    // SAFETY: Freshly built class object; mirror `pon_build_class`'s ob_type fix.
    unsafe {
        if (*class).ob_type.is_null() {
            (*class).ob_type = abi::runtime_type_type().cast_const();
        }
    }
    Ok(class)
}

/// `_io.open_code(path)`: CPython semantics minus audit hooks — a binary
/// read-only stream over `path`.
unsafe extern "C" fn open_code_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { argv_slice(argv, argc) } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    if args.len() != 1 {
        return raise_type_error(&format!("open_code() takes 1 positional argument but {} were given", args.len()));
    }
    let mode = alloc_str("rb");
    if mode.is_null() {
        return ptr::null_mut();
    }
    match open_from_args(&[args[0], mode]) {
        Ok(object) => object,
        Err(OpenError::Type(message)) => raise_type_error(&message),
        Err(OpenError::Value(message)) => raise_value_error(&message),
        Err(OpenError::Io(message)) => raise_io_error(&message),
    }
}

/// `_io.text_encoding(encoding, stacklevel=2)`: pass a concrete encoding
/// through; `None` selects "locale" (CPython default without UTF-8 mode,
/// which pon does not model).
unsafe extern "C" fn text_encoding_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { argv_slice(argv, argc) } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    if args.is_empty() || args.len() > 2 {
        return raise_type_error(&format!(
            "text_encoding() takes 1 or 2 positional arguments but {} were given",
            args.len()
        ));
    }
    if is_none(args[0]) {
        alloc_str("locale")
    } else {
        args[0]
    }
}

/// Honest shared failure body for `_io` heap-class stream method stubs.
unsafe extern "C" fn io_stub_method(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    abi::return_null_with_error("NotImplementedError: this _io stream method is not implemented in pon".to_owned())
}

/// Builtin `open()` entry point registered by `builtins_mod`.
pub(super) unsafe extern "C" fn builtin_open(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { argv_slice(argv, argc) } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    match open_from_args(args) {
        Ok(object) => object,
        Err(OpenError::Type(message)) => raise_type_error(&message),
        Err(OpenError::Value(message)) => raise_value_error(&message),
        Err(OpenError::Io(message)) => raise_io_error(&message),
    }
}

/// Builtin `input()` entry point registered by `builtins_mod`.
pub(super) unsafe extern "C" fn builtin_input(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { argv_slice(argv, argc) } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    if args.len() > 1 {
        return raise_type_error(&format!("input() expected at most 1 argument, got {}", args.len()));
    }
    if let Some(&prompt) = args.first() {
        let Some(text) = (unsafe { type_::unicode_text(prompt) }) else {
            return raise_type_error("input() prompt must be str");
        };
        let mut stdout = std::io::stdout().lock();
        if write!(stdout, "{text}").and_then(|()| stdout.flush()).is_err() {
            return raise_io_error("failed to write stdout");
        }
    }

    let mut line = String::new();
    finish_input_read(std::io::stdin().read_line(&mut line), &mut line)
}

fn open_from_args(args: &[*mut PyObject]) -> Result<*mut PyObject, OpenError> {
    if args.is_empty() || args.len() > 8 {
        return Err(OpenError::Type(format!("open() expected 1 to 8 arguments, got {}", args.len())));
    }
    let path = expect_str(args[0], "open() file must be str")?.to_owned();
    let mode_text = if let Some(&mode) = args.get(1) {
        expect_str(mode, "open() mode must be str")?.to_owned()
    } else {
        "r".to_owned()
    };
    let mode = parse_mode(&mode_text)?;

    if let Some(&buffering) = args.get(2) {
        if !is_none(buffering) {
            let _ = expect_int(buffering, "open() buffering must be int").map_err(OpenError::Type)?;
        }
    }

    let encoding = if mode.binary {
        if args.get(3).copied().is_some_and(|value| !is_none(value)) {
            return Err(OpenError::Value("binary mode doesn't take an encoding argument".to_owned()));
        }
        None
    } else if let Some(&encoding) = args.get(3) {
        if is_none(encoding) {
            Some("utf-8".to_owned())
        } else {
            let text = expect_str(encoding, "open() encoding must be str")?;
            if !text.eq_ignore_ascii_case("utf-8") && !text.eq_ignore_ascii_case("utf8") {
                return Err(OpenError::Value(format!("unsupported encoding: {text}")));
            }
            Some("utf-8".to_owned())
        }
    } else {
        Some("utf-8".to_owned())
    };

    if args.get(4).copied().is_some_and(|errors| !is_none(errors)) {
        return Err(OpenError::Value("open() errors argument is not implemented".to_owned()));
    }

    let newline = if mode.binary {
        if args.get(5).copied().is_some_and(|value| !is_none(value)) {
            return Err(OpenError::Value("binary mode doesn't take a newline argument".to_owned()));
        }
        NewlineMode::Preserve
    } else if let Some(&newline) = args.get(5) {
        if is_none(newline) {
            NewlineMode::UniversalTranslate
        } else {
            let text = expect_str(newline, "open() newline must be str or None")?;
            match text {
                "" | "\n" | "\r" | "\r\n" => NewlineMode::Preserve,
                _ => return Err(OpenError::Value("illegal newline value".to_owned())),
            }
        }
    } else {
        NewlineMode::UniversalTranslate
    };

    if args.get(6).copied().is_some_and(|closefd| is_false(closefd)) {
        return Err(OpenError::Value("open() closefd=False is not supported".to_owned()));
    }
    if args.get(7).copied().is_some_and(|opener| !is_none(opener)) {
        return Err(OpenError::Value("open() opener argument is not supported".to_owned()));
    }

    let file = open_host_file(&path, &mode)?;
    Ok(alloc_file(file, path, mode, encoding, newline))
}

fn open_host_file(path: &str, mode: &OpenMode) -> Result<File, OpenError> {
    let mut options = OpenOptions::new();
    options.read(mode.readable);
    if mode.append {
        options.append(true);
    } else {
        options.write(mode.writable);
    }
    options.create(mode.create);
    options.truncate(mode.truncate);
    options.create_new(mode.create_new);
    options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            OpenError::Io(format!("FileExistsError: [Errno 17] File exists: '{path}'"))
        } else if error.kind() == std::io::ErrorKind::NotFound {
            OpenError::Io(format!("FileNotFoundError: [Errno 2] No such file or directory: '{path}'"))
        } else {
            OpenError::Io(format!("OSError: {error}"))
        }
    })
}

fn alloc_file(file: File, name: String, mode: OpenMode, encoding: Option<String>, newline: NewlineMode) -> *mut PyObject {
    let ty = if mode.binary { binary_file_type() } else { text_file_type() };
    Box::into_raw(Box::new(PyNativeFile {
        ob_base: PyObjectHeader::new(ty),
        file: Some(file),
        name,
        mode: mode.display,
        binary: mode.binary,
        readable: mode.readable,
        writable: mode.writable,
        append: mode.append,
        encoding,
        newline,
    }))
    .cast::<PyObject>()
}

fn parse_mode(mode: &str) -> Result<OpenMode, OpenError> {
    if mode.is_empty() {
        return Err(OpenError::Value("Must have exactly one of create/read/write/append mode".to_owned()));
    }

    let mut primary = None;
    let mut binary = false;
    let mut text = false;
    let mut plus = false;
    for ch in mode.chars() {
        match ch {
            'r' | 'w' | 'a' | 'x' => {
                if primary.replace(ch).is_some() {
                    return Err(OpenError::Value("Must have exactly one of create/read/write/append mode".to_owned()));
                }
            }
            'b' => {
                if binary || text {
                    return Err(OpenError::Value("can't have text and binary mode at once".to_owned()));
                }
                binary = true;
            }
            't' => {
                if text || binary {
                    return Err(OpenError::Value("can't have text and binary mode at once".to_owned()));
                }
                text = true;
            }
            '+' => {
                if plus {
                    return Err(OpenError::Value("invalid mode: duplicate '+'".to_owned()));
                }
                plus = true;
            }
            _ => return Err(OpenError::Value(format!("invalid mode: {mode}"))),
        }
    }
    let Some(primary) = primary else {
        return Err(OpenError::Value("Must have exactly one of create/read/write/append mode".to_owned()));
    };

    let (mut readable, writable, append, truncate, create, create_new) = match primary {
        'r' => (true, false, false, false, false, false),
        'w' => (false, true, false, true, true, false),
        'a' => (false, true, true, false, true, false),
        'x' => (false, true, false, false, false, true),
        _ => unreachable!(),
    };
    let mut writable = writable;
    if plus {
        readable = true;
        writable = true;
    }

    Ok(OpenMode {
        display: mode.to_owned(),
        binary,
        readable,
        writable,
        append,
        truncate,
        create,
        create_new,
    })
}

unsafe fn as_file<'a>(object: *mut PyObject) -> Option<&'a mut PyNativeFile> {
    if object.is_null() {
        return None;
    }
    let ty = unsafe { (*object).ob_type };
    if ty == text_file_type().cast_const() || ty == binary_file_type().cast_const() {
        Some(unsafe { &mut *object.cast::<PyNativeFile>() })
    } else {
        None
    }
}

unsafe extern "C" fn file_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(attr) = (unsafe { type_::unicode_text(name) }) else {
        return raise_type_error("file attribute name must be str");
    };
    let Some(file) = (unsafe { as_file(object) }) else {
        return raise_type_error("file method receiver is not a native file");
    };
    match attr {
        "closed" => unsafe { abi::number::pon_const_bool(i32::from(file.file.is_none())) },
        "name" => alloc_str(&file.name),
        "mode" => alloc_str(&file.mode),
        "encoding" => file.encoding.as_deref().map_or_else(|| unsafe { abi::pon_none() }, alloc_str),
        "newlines" => unsafe { abi::pon_none() },
        "read" => bound_file_method(object, "read", file_read_method),
        "readline" => bound_file_method(object, "readline", file_readline_method),
        "readlines" => bound_file_method(object, "readlines", file_readlines_method),
        "write" => bound_file_method(object, "write", file_write_method),
        "writelines" => bound_file_method(object, "writelines", file_writelines_method),
        "seek" => bound_file_method(object, "seek", file_seek_method),
        "tell" => bound_file_method(object, "tell", file_tell_method),
        "close" => bound_file_method(object, "close", file_close_method),
        "flush" => bound_file_method(object, "flush", file_flush_method),
        "__enter__" => bound_file_method(object, "__enter__", file_enter_method),
        "__exit__" => bound_file_method(object, "__exit__", file_exit_method),
        "__iter__" => bound_file_method(object, "__iter__", file_iter_method),
        "__next__" => bound_file_method(object, "__next__", file_next_method),
        _ => raise_attribute_error(attr),
    }
}

fn bound_file_method(
    receiver: *mut PyObject,
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> *mut PyObject {
    let function = unsafe { abi::pon_make_function(entry as *const u8, builtins::variadic_arity(), intern(name)) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match method::new_bound_method(function, receiver) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => raise_type_error(&message),
    }
}

unsafe extern "C" fn file_iter_slot(object: *mut PyObject) -> *mut PyObject {
    let Some(file) = (unsafe { as_file(object) }) else {
        return raise_type_error("file iterator receiver is not a native file");
    };
    if file.file.is_none() {
        return raise_value_error("I/O operation on closed file.");
    }
    if !file.readable {
        return raise_value_error("not readable");
    }
    object
}

unsafe extern "C" fn file_iternext_slot(object: *mut PyObject) -> *mut PyObject {
    let Some(file) = (unsafe { as_file(object) }) else {
        return raise_type_error("file iterator receiver is not a native file");
    };
    match read_line_raw(file, None) {
        Ok(bytes) if bytes.is_empty() => unsafe { abi::pon_raise_stop_iteration(ptr::null_mut()) },
        Ok(bytes) => bytes_to_python(file, bytes),
        Err(FileOpError::Value(message)) => raise_value_error(&message),
        Err(FileOpError::Io(message)) => raise_io_error(&message),
    }
}

unsafe extern "C" fn file_read_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "read") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    if args.len() > 2 {
        return raise_type_error(&format!("read() expected at most 1 argument, got {}", args.len().saturating_sub(1)));
    }
    let size = match optional_size(args.get(1).copied(), "read") {
        Ok(size) => size,
        Err(message) => return raise_type_error(&message),
    };
    let Some(file) = (unsafe { as_file(args[0]) }) else {
        return raise_type_error("read() receiver is not a native file");
    };
    match read_raw(file, size) {
        Ok(bytes) => bytes_to_python(file, bytes),
        Err(FileOpError::Value(message)) => raise_value_error(&message),
        Err(FileOpError::Io(message)) => raise_io_error(&message),
    }
}

unsafe extern "C" fn file_readline_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "readline") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    if args.len() > 2 {
        return raise_type_error(&format!("readline() expected at most 1 argument, got {}", args.len().saturating_sub(1)));
    }
    let size = match optional_size(args.get(1).copied(), "readline") {
        Ok(size) => size,
        Err(message) => return raise_type_error(&message),
    };
    let Some(file) = (unsafe { as_file(args[0]) }) else {
        return raise_type_error("readline() receiver is not a native file");
    };
    match read_line_raw(file, size) {
        Ok(bytes) => bytes_to_python(file, bytes),
        Err(FileOpError::Value(message)) => raise_value_error(&message),
        Err(FileOpError::Io(message)) => raise_io_error(&message),
    }
}

unsafe extern "C" fn file_readlines_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "readlines") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    if args.len() > 2 {
        return raise_type_error(&format!("readlines() expected at most 1 argument, got {}", args.len().saturating_sub(1)));
    }
    let Some(file) = (unsafe { as_file(args[0]) }) else {
        return raise_type_error("readlines() receiver is not a native file");
    };
    let mut lines = Vec::new();
    loop {
        match read_line_raw(file, None) {
            Ok(bytes) if bytes.is_empty() => break,
            Ok(bytes) => {
                let line = bytes_to_python(file, bytes);
                if line.is_null() {
                    return ptr::null_mut();
                }
                lines.push(line);
            }
            Err(FileOpError::Value(message)) => return raise_value_error(&message),
            Err(FileOpError::Io(message)) => return raise_io_error(&message),
        }
    }
    super::builtins_mod::alloc_list(lines)
}

unsafe extern "C" fn file_write_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "write") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    if args.len() != 2 {
        return raise_type_error(&format!("write() expected 1 argument, got {}", args.len().saturating_sub(1)));
    }
    let Some(file) = (unsafe { as_file(args[0]) }) else {
        return raise_type_error("write() receiver is not a native file");
    };
    match write_object(file, args[1]) {
        Ok(count) => unsafe { abi::pon_const_int(count) },
        Err(FileOpError::Value(message)) => raise_value_error(&message),
        Err(FileOpError::Io(message)) => raise_io_error(&message),
    }
}

unsafe extern "C" fn file_writelines_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "writelines") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    if args.len() != 2 {
        return raise_type_error(&format!("writelines() expected 1 argument, got {}", args.len().saturating_sub(1)));
    }
    let Some(file) = (unsafe { as_file(args[0]) }) else {
        return raise_type_error("writelines() receiver is not a native file");
    };
    let iter = unsafe { abi::pon_get_iter(args[1], ptr::null_mut()) };
    if iter.is_null() {
        return ptr::null_mut();
    }
    loop {
        let item = unsafe { abi::pon_iter_next(iter, ptr::null_mut()) };
        if item.is_null() {
            if stop_iteration_pending() || !pon_err_occurred() {
                pon_err_clear();
                break;
            }
            return ptr::null_mut();
        }
        if let Err(error) = write_object(file, item) {
            return match error {
                FileOpError::Value(message) => raise_value_error(&message),
                FileOpError::Io(message) => raise_io_error(&message),
            };
        }
    }
    unsafe { abi::pon_none() }
}

unsafe extern "C" fn file_seek_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "seek") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    if !(args.len() == 2 || args.len() == 3) {
        return raise_type_error(&format!("seek() expected 1 or 2 arguments, got {}", args.len().saturating_sub(1)));
    }
    let offset = match expect_int(args[1], "seek() offset must be int") {
        Ok(offset) => offset,
        Err(message) => return raise_type_error(&message),
    };
    let whence = if let Some(&whence) = args.get(2) {
        match expect_int(whence, "seek() whence must be int") {
            Ok(whence) => whence,
            Err(message) => return raise_type_error(&message),
        }
    } else {
        0
    };
    let Some(file) = (unsafe { as_file(args[0]) }) else {
        return raise_type_error("seek() receiver is not a native file");
    };
    match seek_file(file, offset, whence) {
        Ok(position) => unsafe { abi::pon_const_int(position as i64) },
        Err(FileOpError::Value(message)) => raise_value_error(&message),
        Err(FileOpError::Io(message)) => raise_io_error(&message),
    }
}

unsafe extern "C" fn file_tell_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "tell") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    if args.len() != 1 {
        return raise_type_error(&format!("tell() expected 0 arguments, got {}", args.len().saturating_sub(1)));
    }
    let Some(file) = (unsafe { as_file(args[0]) }) else {
        return raise_type_error("tell() receiver is not a native file");
    };
    match seek_file(file, 0, 1) {
        Ok(position) => unsafe { abi::pon_const_int(position as i64) },
        Err(FileOpError::Value(message)) => raise_value_error(&message),
        Err(FileOpError::Io(message)) => raise_io_error(&message),
    }
}

unsafe extern "C" fn file_close_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "close") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    if args.len() != 1 {
        return raise_type_error(&format!("close() expected 0 arguments, got {}", args.len().saturating_sub(1)));
    }
    let Some(file) = (unsafe { as_file(args[0]) }) else {
        return raise_type_error("close() receiver is not a native file");
    };
    if let Some(mut handle) = file.file.take() {
        if handle.flush().is_err() {
            return raise_io_error("failed to flush file during close");
        }
    }
    unsafe { abi::pon_none() }
}

unsafe extern "C" fn file_flush_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "flush") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    if args.len() != 1 {
        return raise_type_error(&format!("flush() expected 0 arguments, got {}", args.len().saturating_sub(1)));
    }
    let Some(file) = (unsafe { as_file(args[0]) }) else {
        return raise_type_error("flush() receiver is not a native file");
    };
    let Some(handle) = file.file.as_mut() else {
        return raise_value_error("I/O operation on closed file.");
    };
    if handle.flush().is_err() {
        return raise_io_error("failed to flush file");
    }
    unsafe { abi::pon_none() }
}

unsafe extern "C" fn file_enter_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "__enter__") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    if args.len() != 1 {
        return raise_type_error(&format!("__enter__() expected 0 arguments, got {}", args.len().saturating_sub(1)));
    }
    let Some(file) = (unsafe { as_file(args[0]) }) else {
        return raise_type_error("__enter__() receiver is not a native file");
    };
    if file.file.is_none() {
        return raise_value_error("I/O operation on closed file.");
    }
    args[0]
}

unsafe extern "C" fn file_exit_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "__exit__") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    if args.len() != 4 {
        return raise_type_error(&format!("__exit__() expected 3 arguments, got {}", args.len().saturating_sub(1)));
    }
    let Some(file) = (unsafe { as_file(args[0]) }) else {
        return raise_type_error("__exit__() receiver is not a native file");
    };
    if let Some(mut handle) = file.file.take() {
        if handle.flush().is_err() {
            return raise_io_error("failed to flush file during close");
        }
    }
    unsafe { abi::number::pon_const_bool(0) }
}

unsafe extern "C" fn file_iter_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "__iter__") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    if args.len() != 1 {
        return raise_type_error(&format!("__iter__() expected 0 arguments, got {}", args.len().saturating_sub(1)));
    }
    unsafe { file_iter_slot(args[0]) }
}

unsafe extern "C" fn file_next_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "__next__") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    if args.len() != 1 {
        return raise_type_error(&format!("__next__() expected 0 arguments, got {}", args.len().saturating_sub(1)));
    }
    unsafe { file_iternext_slot(args[0]) }
}

fn read_raw(file: &mut PyNativeFile, size: Option<usize>) -> Result<Vec<u8>, FileOpError> {
    ensure_readable(file)?;
    let handle = file.file.as_mut().ok_or_else(closed_error)?;
    let mut out = Vec::new();
    match size {
        Some(size) => {
            out.resize(size, 0);
            let n = handle.read(&mut out).map_err(io_op_error)?;
            out.truncate(n);
        }
        None => {
            handle.read_to_end(&mut out).map_err(io_op_error)?;
        }
    }
    Ok(out)
}

fn read_line_raw(file: &mut PyNativeFile, size: Option<usize>) -> Result<Vec<u8>, FileOpError> {
    ensure_readable(file)?;
    let handle = file.file.as_mut().ok_or_else(closed_error)?;
    let mut out = Vec::new();
    let limit = size.unwrap_or(usize::MAX);
    if limit == 0 {
        return Ok(out);
    }
    while out.len() < limit {
        let mut byte = [0_u8; 1];
        let n = handle.read(&mut byte).map_err(io_op_error)?;
        if n == 0 {
            break;
        }
        out.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
        if !file.binary && file.newline == NewlineMode::UniversalTranslate && byte[0] == b'\r' {
            if out.len() >= limit {
                break;
            }
            let mut next = [0_u8; 1];
            let n = handle.read(&mut next).map_err(io_op_error)?;
            if n == 0 {
                break;
            }
            if next[0] == b'\n' {
                out.push(next[0]);
            } else {
                handle.seek(SeekFrom::Current(-1)).map_err(io_op_error)?;
            }
            break;
        }
    }
    Ok(out)
}

fn bytes_to_python(file: &PyNativeFile, bytes: Vec<u8>) -> *mut PyObject {
    if file.binary {
        unsafe { abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) }
    } else {
        let bytes = if file.newline == NewlineMode::UniversalTranslate {
            translate_universal_newlines(&bytes)
        } else {
            bytes
        };
        match String::from_utf8(bytes) {
            Ok(text) => alloc_str(&text),
            Err(_) => raise_value_error("file contents are not valid UTF-8"),
        }
    }
}

fn write_object(file: &mut PyNativeFile, object: *mut PyObject) -> Result<i64, FileOpError> {
    ensure_writable(file)?;
    let binary = file.binary;
    let handle = file.file.as_mut().ok_or_else(closed_error)?;
    if binary {
        let bytes = object_bytes(object).ok_or_else(|| FileOpError::Value("a bytes-like object is required, not 'str'".to_owned()))?;
        handle.write_all(&bytes).map_err(io_op_error)?;
        Ok(bytes.len() as i64)
    } else {
        let text = unsafe { type_::unicode_text(object) }
            .ok_or_else(|| FileOpError::Value("write() argument must be str, not bytes".to_owned()))?;
        handle.write_all(text.as_bytes()).map_err(io_op_error)?;
        Ok(text.chars().count() as i64)
    }
}

fn seek_file(file: &mut PyNativeFile, offset: i64, whence: i64) -> Result<u64, FileOpError> {
    let handle = file.file.as_mut().ok_or_else(closed_error)?;
    let seek_from = match whence {
        0 => {
            if offset < 0 {
                return Err(FileOpError::Value("negative seek position".to_owned()));
            }
            SeekFrom::Start(offset as u64)
        }
        1 => SeekFrom::Current(offset),
        2 => SeekFrom::End(offset),
        _ => return Err(FileOpError::Value("invalid whence".to_owned())),
    };
    handle.seek(seek_from).map_err(io_op_error)
}

fn ensure_readable(file: &PyNativeFile) -> Result<(), FileOpError> {
    if file.file.is_none() {
        Err(closed_error())
    } else if !file.readable {
        Err(FileOpError::Value("not readable".to_owned()))
    } else {
        Ok(())
    }
}

fn ensure_writable(file: &PyNativeFile) -> Result<(), FileOpError> {
    if file.file.is_none() {
        Err(closed_error())
    } else if !file.writable {
        Err(FileOpError::Value("not writable".to_owned()))
    } else {
        Ok(())
    }
}

fn object_bytes(object: *mut PyObject) -> Option<Vec<u8>> {
    if object.is_null() {
        return None;
    }
    let ty = unsafe { (*object).ob_type };
    if bytes_::is_bytes_type(ty) {
        let bytes = unsafe { &*object.cast::<bytes_::PyBytes>() };
        Some(unsafe { bytes.as_slice() }.to_vec())
    } else if bytearray_::is_bytearray_type(ty) {
        let bytes = unsafe { &*object.cast::<bytearray_::PyByteArray>() };
        Some(bytes.as_slice().to_vec())
    } else {
        None
    }
}

fn translate_universal_newlines(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                out.push(b'\n');
                index += 1;
                if bytes.get(index) == Some(&b'\n') {
                    index += 1;
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    out
}

unsafe fn argv_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Result<&'a [*mut PyObject], String> {
    if argv.is_null() && argc != 0 {
        return Err("argv pointer is null".to_owned());
    }
    Ok(if argc == 0 { &[] } else { unsafe { std::slice::from_raw_parts(argv, argc) } })
}

unsafe fn method_args<'a>(argv: *mut *mut PyObject, argc: usize, name: &str) -> Result<&'a [*mut PyObject], String> {
    let args = unsafe { argv_slice(argv, argc) }?;
    if args.is_empty() {
        return Err(format!("{name}() missing receiver"));
    }
    Ok(args)
}

fn optional_size(object: Option<*mut PyObject>, owner: &str) -> Result<Option<usize>, String> {
    let Some(object) = object else {
        return Ok(None);
    };
    let value = expect_int(object, &format!("{owner}() size must be int"))?;
    if value < 0 {
        Ok(None)
    } else {
        Ok(Some(value as usize))
    }
}

fn expect_str<'a>(object: *mut PyObject, message: &str) -> Result<&'a str, OpenError> {
    unsafe { type_::unicode_text(object) }.ok_or_else(|| OpenError::Type(message.to_owned()))
}

fn expect_int(object: *mut PyObject, message: &str) -> Result<i64, String> {
    if object.is_null() {
        return Err(message.to_owned());
    }
    let ty = unsafe { (*object).ob_type };
    if ty.is_null() || unsafe { (*ty).name() != "int" && (*ty).name() != "bool" } {
        return Err(message.to_owned());
    }
    Some(unsafe { (*object.cast::<PyLong>()).value }).ok_or_else(|| message.to_owned())
}

fn is_none(object: *mut PyObject) -> bool {
    if object.is_null() {
        return false;
    }
    let ty = unsafe { (*object).ob_type };
    !ty.is_null() && unsafe { (*ty).name() == "NoneType" }
}

fn is_false(object: *mut PyObject) -> bool {
    if object.is_null() {
        return false;
    }
    let ty = unsafe { (*object).ob_type };
    !ty.is_null() && unsafe { (*ty).name() == "bool" } && unsafe { (*object.cast::<PyLong>()).value == 0 }
}

fn stop_iteration_pending() -> bool {
    pon_err_message().is_some_and(|message| message.starts_with("StopIteration"))
}

fn strip_input_newline(line: &mut String) {
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    } else if line.ends_with('\r') {
        line.pop();
    }
}
fn finish_input_read(result: std::io::Result<usize>, line: &mut String) -> *mut PyObject {
    match result {
        Ok(0) => raise_eof_error("EOF when reading a line"),
        Ok(_) => {
            strip_input_newline(line);
            alloc_str(line)
        }
        Err(error) => raise_io_error(&format!("failed to read stdin: {error}")),
    }
}


fn alloc_str(text: &str) -> *mut PyObject {
    unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn raise_value_error(message: &str) -> *mut PyObject {
    unsafe { abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) }
}

fn raise_attribute_error(name: &str) -> *mut PyObject {
    abi::return_null_with_error(format!("AttributeError: attribute '{name}' was not found"))
}

fn raise_io_error(message: &str) -> *mut PyObject {
    abi::return_null_with_error(message.to_owned())
}

fn raise_eof_error(message: &str) -> *mut PyObject {
    let eof_type = unsafe { abi::pon_load_global(intern("EOFError"), ptr::null_mut()) };
    if !eof_type.is_null() {
        return unsafe { abi::pon_raise(eof_type, ptr::null_mut()) };
    }
    pon_err_clear();
    pon_err_set(format!("EOFError: {message}"));
    ptr::null_mut()
}

fn closed_error() -> FileOpError {
    FileOpError::Value("I/O operation on closed file.".to_owned())
}

fn io_op_error(error: std::io::Error) -> FileOpError {
    FileOpError::Io(format!("OSError: {error}"))
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum OpenError {
    Type(String),
    Value(String),
    Io(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum FileOpError {
    Value(String),
    Io(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread_state::{pon_err_clear, pon_err_message, test_state_lock};

    fn init_runtime() {
        assert_eq!(unsafe { abi::pon_runtime_init() }, 0);
        pon_err_clear();
    }

    fn tmp_path(name: &str) -> String {
        let mut path = std::env::temp_dir();
        path.push(format!("pon-native-io-{name}-{}", std::process::id()));
        path.to_string_lossy().into_owned()
    }

    fn str_obj(text: &str) -> *mut PyObject {
        let object = unsafe { abi::pon_const_str(text.as_ptr(), text.len()) };
        assert!(!object.is_null());
        object
    }

    fn int_obj(value: i64) -> *mut PyObject {
        let object = unsafe { abi::pon_const_int(value) };
        assert!(!object.is_null());
        object
    }

    #[test]
    fn x_mode_collision_reports_error() {
        let _guard = test_state_lock();
        init_runtime();
        let path = tmp_path("x-collision.txt");
        std::fs::write(&path, b"already here").unwrap();
        let mut args = [str_obj(&path), str_obj("x")];
        let result = unsafe { builtin_open(args.as_mut_ptr(), args.len()) };
        assert!(result.is_null());
        assert!(pon_err_message().unwrap_or_default().contains("FileExistsError"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn closed_file_read_raises_value_error() {
        let _guard = test_state_lock();
        init_runtime();
        let path = tmp_path("closed.txt");
        std::fs::write(&path, b"abc").unwrap();
        let mut args = [str_obj(&path), str_obj("r")];
        let object = unsafe { builtin_open(args.as_mut_ptr(), args.len()) };
        assert!(!object.is_null());
        let mut close_args = [object];
        assert!(!unsafe { file_close_method(close_args.as_mut_ptr(), close_args.len()) }.is_null());
        pon_err_clear();
        let mut read_args = [object];
        let result = unsafe { file_read_method(read_args.as_mut_ptr(), read_args.len()) };
        assert!(result.is_null());
        assert!(pon_err_message().unwrap_or_default().contains("ValueError: I/O operation on closed file."));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn seek_tell_round_trip() {
        let _guard = test_state_lock();
        init_runtime();
        let path = tmp_path("seek.txt");
        std::fs::write(&path, b"abcdef").unwrap();
        let mut args = [str_obj(&path), str_obj("r")];
        let object = unsafe { builtin_open(args.as_mut_ptr(), args.len()) };
        assert!(!object.is_null());
        let mut seek_args = [object, int_obj(3)];
        let position = unsafe { file_seek_method(seek_args.as_mut_ptr(), seek_args.len()) };
        assert!(!position.is_null());
        assert_eq!(unsafe { (*position.cast::<PyLong>()).value }, 3);
        let mut tell_args = [object];
        let tell = unsafe { file_tell_method(tell_args.as_mut_ptr(), tell_args.len()) };
        assert!(!tell.is_null());
        assert_eq!(unsafe { (*tell.cast::<PyLong>()).value }, 3);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn input_newline_stripping_matches_builtin_contract() {
        let _guard = test_state_lock();
        let mut line = "hello\r\n".to_owned();
        strip_input_newline(&mut line);
        assert_eq!(line, "hello");
        let mut line = "hello\n".to_owned();
        strip_input_newline(&mut line);
        assert_eq!(line, "hello");
        let mut line = "hello".to_owned();
        strip_input_newline(&mut line);
        assert_eq!(line, "hello");

        init_runtime();
        let mut eof_line = String::new();
        let eof = finish_input_read(Ok(0), &mut eof_line);
        assert!(eof.is_null());
        assert!(pon_err_message().unwrap_or_default().starts_with("EOFError"));
    }
}
