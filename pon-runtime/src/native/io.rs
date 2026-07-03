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
use crate::types::exc::ExceptionKind;
use crate::types::{bytearray_, bytes_, memoryview, method, type_};

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
    ty.tp_setattro = Some(file_setattro);
    ty.tp_iter = Some(file_iter_slot);
    ty.tp_iternext = Some(file_iternext_slot);
    ty.tp_new = Some(text_file_new);
    Box::into_raw(ty) as usize
});

static BINARY_FILE_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = Box::new(PyType::new(
        abi::runtime_type_type().cast_const(),
        "FileIO",
        std::mem::size_of::<PyNativeFile>(),
    ));
    ty.tp_getattro = Some(file_getattro);
    ty.tp_setattro = Some(file_setattro);
    ty.tp_iter = Some(file_iter_slot);
    ty.tp_iternext = Some(file_iternext_slot);
    Box::into_raw(ty) as usize
});

/// `tp_new` for `_io.TextIOWrapper(buffer, encoding=None, ...)`: wraps an
/// open native binary stream by duplicating its host handle (dup semantics:
/// shared offset, exactly what `tokenize.open`'s `buffer.seek(0)` +
/// wrap-then-readlines sequence expects).  Extra positional/keyword options
/// beyond `encoding` are accepted and ignored: pon's native text stream is
/// always line-translating UTF-8.
unsafe extern "C" fn text_file_new(_cls: *mut PyType, args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
    let positional = match unsafe { type_::positional_args_from_object(args) } {
        Ok(args) => args,
        Err(message) => {
            pon_err_set(message);
            return ptr::null_mut();
        }
    };
    if positional.is_empty() {
        return raise_type_error("TextIOWrapper() missing required argument 'buffer'");
    }
    if let Some(&encoding) = positional.get(1) {
        if !is_none(encoding) {
            let Some(text) = (unsafe { type_::unicode_text(encoding) }) else {
                return raise_type_error("TextIOWrapper() encoding must be str or None");
            };
            if !text.eq_ignore_ascii_case("utf-8") && !text.eq_ignore_ascii_case("utf8") {
                return raise_io_error(&format!("unsupported encoding: {text}"));
            }
        }
    }
    let Some(buffer) = (unsafe { as_file(positional[0]) }) else {
        return raise_type_error("TextIOWrapper() buffer must be an open native file");
    };
    let Some(handle) = buffer.file.as_ref() else {
        return raise_value_error("I/O operation on closed file.");
    };
    let Ok(clone) = handle.try_clone() else {
        return raise_io_error("failed to duplicate stream handle");
    };
    Box::into_raw(Box::new(PyNativeFile {
        ob_base: PyObjectHeader::new(text_file_type()),
        file: Some(clone),
        name: buffer.name.clone(),
        mode: "r".to_owned(),
        binary: false,
        readable: buffer.readable,
        writable: buffer.writable,
        append: buffer.append,
        encoding: Some("utf-8".to_owned()),
        newline: NewlineMode::UniversalTranslate,
    }))
    .cast::<PyObject>()
}

/// `tp_setattro` for native files: only the Python-visible `mode` label is
/// assignable (`tokenize.open` stamps `text.mode = 'r'`); everything else
/// raises AttributeError to keep the frontier loud.
unsafe extern "C" fn file_setattro(object: *mut PyObject, name: *mut PyObject, value: *mut PyObject) -> core::ffi::c_int {
    let Some(attr) = (unsafe { type_::unicode_text(name) }) else {
        pon_err_set("file attribute name must be str");
        return -1;
    };
    let Some(file) = (unsafe { as_file(object) }) else {
        pon_err_set("file attribute receiver is not a native file");
        return -1;
    };
    if attr == "mode" {
        let Some(text) = (unsafe { type_::unicode_text(crate::tag::untag_arg(value)) }) else {
            pon_err_set("file mode must be str");
            return -1;
        };
        file.mode = text.to_owned();
        return 0;
    }
    let message = format!("'{}' object attribute '{attr}' is read-only", unsafe { (*(*object).ob_type).name() });
    // SAFETY-free typed raise: catchable AttributeError with the CPython text.
    let _ = crate::abi::exc::raise_attribute_error_text(&message);
    -1
}

fn text_file_type() -> *mut PyType {
    *TEXT_FILE_TYPE as *mut PyType
}

fn binary_file_type() -> *mut PyType {
    *BINARY_FILE_TYPE as *mut PyType
}

// ---------------------------------------------------------------------------
// BytesIO: real in-memory binary stream (CPython `Modules/_io/bytesio.c`
// semantics).  The backing store is a growable byte vector; the position may
// park beyond EOF (reads see empty, writes zero-fill the gap); live
// `getbuffer()` exports pin the buffer size so exported window pointers stay
// valid — size-changing operations raise BufferError exactly like CPython's
// CHECK_EXPORTS.

#[repr(C)]
#[derive(Debug)]
pub(crate) struct PyBytesIO {
    /// Common object header; this field must remain first.
    ob_base: PyObjectHeader,
    /// Backing byte buffer. `None` is the closed state.
    buffer: Option<Vec<u8>>,
    /// Absolute stream position; `seek` may park it beyond the buffer end.
    pos: usize,
    /// Live buffer exports: `getbuffer()` views plus views derived from them
    /// (copies, casts, step-1 slices).  While non-zero, `write`/`truncate`/
    /// `close` raise BufferError, which keeps every exported data pointer
    /// stable (the vector never reallocates in place-preserving writes).
    exports: usize,
}

unsafe impl Send for PyBytesIO {}

static BYTES_IO_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = Box::new(PyType::new(
        abi::runtime_type_type().cast_const(),
        // Dotted tp_name (the `pickle.PickleBuffer` discipline): `repr(type)`
        // shows the CPython path while `__name__` exposes the tail component.
        "_io.BytesIO",
        std::mem::size_of::<PyBytesIO>(),
    ));
    ty.tp_new = Some(bytesio_new);
    ty.tp_getattro = Some(bytesio_getattro);
    ty.tp_setattro = Some(bytesio_setattro);
    ty.tp_iter = Some(bytesio_iter_slot);
    ty.tp_iternext = Some(bytesio_iternext_slot);
    // pon's `__module__` getter defaults static types to "builtins"; carry
    // the CPython value (and the abstract-base `__doc__`) explicitly.
    let namespace = type_::new_namespace();
    if !namespace.is_null() {
        let module = unsafe { pon_const_str("_io".as_ptr(), "_io".len()) };
        let doc = "Buffered I/O implementation using an in-memory bytes buffer.";
        let doc_object = unsafe { pon_const_str(doc.as_ptr(), doc.len()) };
        if !module.is_null() && !doc_object.is_null() {
            // SAFETY: Freshly allocated namespace box; values are live objects.
            unsafe {
                (*namespace).set(intern("__module__"), module);
                (*namespace).set(intern("__doc__"), doc_object);
            }
            ty.tp_dict = namespace.cast::<PyObject>();
        }
    }
    Box::into_raw(ty) as usize
});

fn bytesio_type() -> *mut PyType {
    *BYTES_IO_TYPE as *mut PyType
}

unsafe fn as_bytesio<'a>(object: *mut PyObject) -> Option<&'a mut PyBytesIO> {
    let object = crate::tag::untag_arg(object);
    if object.is_null() {
        return None;
    }
    // Non-forcing type fetch: before the first `_io` import no instance can
    // exist (the pickle.rs `as_picklebuffer` discipline).
    let ty = LazyLock::get(&BYTES_IO_TYPE).map_or(ptr::null(), |&ty| ty as *const PyType);
    if ty.is_null() {
        return None;
    }
    // SAFETY: NULL was rejected above; the type check gates the downcast.
    (unsafe { (*object).ob_type } == ty).then(|| unsafe { &mut *object.cast::<PyBytesIO>() })
}

/// Registers one freshly-derived live view with its exporter (called from the
/// `abi/str_.rs` view-derivation seams: `memoryview(view)`, `view.cast(..)`,
/// step-1 slicing).  Only BytesIO exporters track the count; every other
/// `base` ignores the signal.
pub(crate) fn bytesio_export_cloned(base: *mut PyObject) {
    if let Some(bio) = unsafe { as_bytesio(base) } {
        bio.exports += 1;
    }
}

/// Drops one live view on the `released: false -> true` transition (the
/// `release()`/`__exit__` seams in `abi/str_.rs`).  Views dropped without an
/// explicit release keep the export pinned — pon has no finalizers — which
/// only ever errs toward CPython's stricter BufferError side.
pub(crate) fn bytesio_export_released(base: *mut PyObject) {
    if let Some(bio) = unsafe { as_bytesio(base) } {
        bio.exports = bio.exports.saturating_sub(1);
    }
}

/// BytesIO failure kinds, split by the CPython exception type they raise.
#[derive(Debug)]
enum BioError {
    /// ValueError: closed-file operations, negative absolute seeks,
    /// released-view sources.
    Value(String),
    /// TypeError: argument-type misuse.
    Type(String),
    /// BufferError: a live export pins the buffer size.
    Buffer,
}

fn closed_bio() -> BioError {
    BioError::Value("I/O operation on closed file.".to_owned())
}

fn raise_bio(error: BioError) -> *mut PyObject {
    match error {
        BioError::Value(message) => raise_value_error(&message),
        BioError::Type(message) => raise_type_error(&message),
        BioError::Buffer => crate::abi::exc::raise_kind_error_text(
            ExceptionKind::BufferError,
            "Existing exports of data: object cannot be re-sized",
        ),
    }
}

impl PyBytesIO {
    /// Splits the open stream into `(buffer, position)` borrows, or the
    /// closed-file ValueError.
    fn open_parts(&mut self) -> Result<(&mut Vec<u8>, &mut usize), BioError> {
        let Self { buffer, pos, .. } = self;
        buffer.as_mut().map(|buffer| (buffer, pos)).ok_or_else(closed_bio)
    }

    /// `read`/`read1`: up to `size` bytes from the current position
    /// (`None`/negative reads to EOF); a position parked past EOF reads empty
    /// without moving.
    fn read_bytes(&mut self, size: Option<i64>) -> Result<Vec<u8>, BioError> {
        let (buffer, pos) = self.open_parts()?;
        let start = (*pos).min(buffer.len());
        let available = buffer.len() - start;
        let count = match size {
            Some(size) if size >= 0 => (size as usize).min(available),
            _ => available,
        };
        let out = buffer[start..start + count].to_vec();
        *pos += count;
        Ok(out)
    }

    /// `readline`: bytes through the next `\n` (inclusive), capped by `size`.
    fn read_line(&mut self, size: Option<i64>) -> Result<Vec<u8>, BioError> {
        let (buffer, pos) = self.open_parts()?;
        let start = (*pos).min(buffer.len());
        let available = buffer.len() - start;
        let limit = match size {
            Some(size) if size >= 0 => (size as usize).min(available),
            _ => available,
        };
        let window = &buffer[start..start + limit];
        let count = window.iter().position(|&byte| byte == b'\n').map_or(limit, |at| at + 1);
        let out = window[..count].to_vec();
        *pos += count;
        Ok(out)
    }

    /// `write`: overwrite/extend at the current position, zero-filling any
    /// gap left by a past-EOF seek.  Checked closed -> exports first, exactly
    /// like CPython's `write_bytes` (BufferError wins over the argument's
    /// TypeError, which the entry parses afterwards).
    fn write_bytes(&mut self, data: &[u8]) -> Result<usize, BioError> {
        if self.buffer.is_none() {
            return Err(closed_bio());
        }
        if self.exports > 0 {
            return Err(BioError::Buffer);
        }
        let (buffer, pos) = self.open_parts()?;
        if data.is_empty() {
            return Ok(0);
        }
        let end = *pos + data.len();
        if end > buffer.len() {
            buffer.resize(end, 0);
        }
        buffer[*pos..end].copy_from_slice(data);
        *pos = end;
        Ok(data.len())
    }

    /// `readinto`: fill `dst_len` bytes at `dst`, returning the count read.
    /// Raw-pointer memmove because the destination may alias this very
    /// buffer (`b.readinto(b.getbuffer())`).
    fn read_into_raw(&mut self, dst: *mut u8, dst_len: usize) -> Result<usize, BioError> {
        let (buffer, pos) = self.open_parts()?;
        let start = (*pos).min(buffer.len());
        let count = dst_len.min(buffer.len() - start);
        if count > 0 {
            // SAFETY: `dst` covers `dst_len >= count` writable bytes (the
            // entry validated the target); overlapping ranges are defined
            // under `ptr::copy`.
            unsafe { ptr::copy(buffer.as_ptr().add(start), dst, count) };
        }
        *pos += count;
        Ok(count)
    }

    /// `seek`: absolute negative positions raise; cur/end-relative results
    /// clamp at zero (CPython `_io_BytesIO_seek_impl`).
    fn seek_to(&mut self, offset: i64, whence: i64) -> Result<usize, BioError> {
        let (buffer, pos) = self.open_parts()?;
        let target = match whence {
            0 => {
                if offset < 0 {
                    return Err(BioError::Value(format!("negative seek value {offset}")));
                }
                offset
            }
            1 => (*pos as i64).saturating_add(offset).max(0),
            2 => (buffer.len() as i64).saturating_add(offset).max(0),
            _ => {
                return Err(BioError::Value(format!("invalid whence ({whence}, should be 0, 1 or 2)")));
            }
        };
        *pos = target as usize;
        Ok(*pos)
    }

    /// `truncate`: shrink-only resize that returns the REQUESTED size and
    /// never moves the position (CPython contract).
    fn truncate_to(&mut self, size: Option<i64>) -> Result<i64, BioError> {
        if self.buffer.is_none() {
            return Err(closed_bio());
        }
        if self.exports > 0 {
            return Err(BioError::Buffer);
        }
        let (buffer, pos) = self.open_parts()?;
        let size = size.unwrap_or(*pos as i64);
        if size < 0 {
            return Err(BioError::Value(format!("negative size value {size}")));
        }
        if (size as usize) < buffer.len() {
            buffer.truncate(size as usize);
        }
        Ok(size)
    }
}

/// Copies out a bytes-like argument (bytes, bytearray, memoryview,
/// PickleBuffer) with the CPython diagnostics for released views and
/// non-buffer types.
fn bytes_like_bytes(object: *mut PyObject) -> Result<Vec<u8>, BioError> {
    let object = crate::tag::untag_arg(object);
    if object.is_null() {
        return Err(BioError::Type("a bytes-like object is required, not 'NoneType'".to_owned()));
    }
    // SAFETY: `object` is a live untagged pointer; type checks gate downcasts.
    let ty = unsafe { (*object).ob_type };
    if bytes_::is_bytes_type(ty) {
        let bytes = unsafe { &*object.cast::<bytes_::PyBytes>() };
        return Ok(unsafe { bytes.as_slice() }.to_vec());
    }
    if bytearray_::is_bytearray_type(ty) {
        let bytes = unsafe { &*object.cast::<bytearray_::PyByteArray>() };
        return Ok(bytes.as_slice().to_vec());
    }
    if memoryview::is_memoryview_type(ty) {
        let view = unsafe { &*object.cast::<memoryview::PyMemoryView>() };
        if view.released {
            return Err(BioError::Value(memoryview::RELEASED_ERROR.to_owned()));
        }
        return Ok(unsafe { view.as_slice() }.to_vec());
    }
    if let Some(result) = crate::native::pickle::picklebuffer_bytes(object) {
        return result.map_err(BioError::Value);
    }
    let type_name = unsafe { crate::types::dict::type_name(object) }.unwrap_or("object");
    Err(BioError::Type(format!("a bytes-like object is required, not '{type_name}'")))
}

/// Integer argument with CPython's index-coercion diagnostic.
fn bio_index_arg(object: *mut PyObject) -> Result<i64, BioError> {
    let object = crate::tag::untag_arg(object);
    if object.is_null() {
        return Err(BioError::Type("'NoneType' object cannot be interpreted as an integer".to_owned()));
    }
    // SAFETY: Untagged live pointer; the name check gates the PyLong read.
    let ty = unsafe { (*object).ob_type };
    if !ty.is_null() && unsafe { (*ty).name() == "int" || (*ty).name() == "bool" } {
        return Ok(unsafe { (*object.cast::<PyLong>()).value });
    }
    let type_name = unsafe { crate::types::dict::type_name(object) }.unwrap_or("object");
    Err(BioError::Type(format!("'{type_name}' object cannot be interpreted as an integer")))
}

/// Optional size argument (`read`/`readline`/`truncate`): missing or `None`
/// pass through; anything non-integer raises CPython's clinic diagnostic.
fn bio_optional_size(object: Option<*mut PyObject>) -> Result<Option<i64>, BioError> {
    let Some(object) = object else {
        return Ok(None);
    };
    let object = crate::tag::untag_arg(object);
    if is_none(object) {
        return Ok(None);
    }
    bio_index_arg(object).map(Some).map_err(|_| {
        let type_name = unsafe { crate::types::dict::type_name(object) }.unwrap_or("object");
        BioError::Type(format!("argument should be integer or None, not '{type_name}'"))
    })
}

/// `tp_new` for `_io.BytesIO(initial_bytes=b"")`.
unsafe extern "C" fn bytesio_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    let positional = match unsafe { type_::positional_args_from_object(args) } {
        Ok(args) => args,
        Err(message) => {
            pon_err_set(message);
            return ptr::null_mut();
        }
    };
    if positional.len() > 1 {
        return raise_type_error(&format!("BytesIO() takes at most 1 argument ({} given)", positional.len()));
    }
    let mut initial = positional.first().copied();
    if !kwargs.is_null() {
        let entries = match unsafe { crate::types::dict::dict_entries_snapshot(kwargs) } {
            Ok(entries) => entries,
            Err(message) => return raise_type_error(&message),
        };
        for entry in entries {
            let Some(key) = (unsafe { type_::unicode_text(entry.key) }) else {
                return raise_type_error("keywords must be strings");
            };
            if key != "initial_bytes" {
                return raise_type_error(&format!("'{key}' is an invalid keyword argument for BytesIO()"));
            }
            if initial.is_some() {
                return raise_type_error("argument for BytesIO() given by name ('initial_bytes') and position (1)");
            }
            initial = Some(entry.value);
        }
    }
    let data = match initial.map(crate::tag::untag_arg) {
        None => Vec::new(),
        Some(object) if is_none(object) => Vec::new(),
        Some(object) => match bytes_like_bytes(object) {
            Ok(data) => data,
            Err(error) => return raise_bio(error),
        },
    };
    Box::into_raw(Box::new(PyBytesIO {
        ob_base: PyObjectHeader::new(bytesio_type()),
        buffer: Some(data),
        pos: 0,
        exports: 0,
    }))
    .cast::<PyObject>()
}

unsafe extern "C" fn bytesio_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(attr) = (unsafe { type_::unicode_text(name) }) else {
        return raise_type_error("BytesIO attribute name must be str");
    };
    let Some(bio) = (unsafe { as_bytesio(object) }) else {
        return raise_type_error("BytesIO method receiver is not a BytesIO");
    };
    match attr {
        "closed" => unsafe { abi::number::pon_const_bool(i32::from(bio.buffer.is_none())) },
        "read" | "read1" => bound_file_method(object, attr, bytesio_read_method),
        "readline" => bound_file_method(object, attr, bytesio_readline_method),
        "readlines" => bound_file_method(object, attr, bytesio_readlines_method),
        "readinto" | "readinto1" => bound_file_method(object, attr, bytesio_readinto_method),
        "write" => bound_file_method(object, attr, bytesio_write_method),
        "writelines" => bound_file_method(object, attr, bytesio_writelines_method),
        "seek" => bound_file_method(object, attr, bytesio_seek_method),
        "tell" => bound_file_method(object, attr, bytesio_tell_method),
        "truncate" => bound_file_method(object, attr, bytesio_truncate_method),
        "flush" => bound_file_method(object, attr, bytesio_flush_method),
        "close" => bound_file_method(object, attr, bytesio_close_method),
        "getvalue" => bound_file_method(object, attr, bytesio_getvalue_method),
        "getbuffer" => bound_file_method(object, attr, bytesio_getbuffer_method),
        "readable" | "writable" | "seekable" => bound_file_method(object, attr, bytesio_true_flag_method),
        "isatty" => bound_file_method(object, attr, bytesio_isatty_method),
        "fileno" => bound_file_method(object, attr, bytesio_fileno_method),
        "detach" => bound_file_method(object, attr, bytesio_detach_method),
        "__enter__" => bound_file_method(object, attr, bytesio_enter_method),
        "__exit__" => bound_file_method(object, attr, bytesio_exit_method),
        "__iter__" => bound_file_method(object, attr, bytesio_iter_method),
        "__next__" => bound_file_method(object, attr, bytesio_next_method),
        _ => raise_attribute_error(attr),
    }
}

/// BytesIO instances carry no writable attributes (CPython: no `__dict__`).
unsafe extern "C" fn bytesio_setattro(object: *mut PyObject, name: *mut PyObject, _value: *mut PyObject) -> core::ffi::c_int {
    let attr = unsafe { type_::unicode_text(name) }.unwrap_or("?");
    let type_name = unsafe { crate::types::dict::type_name(crate::tag::untag_arg(object)) }.unwrap_or("_io.BytesIO");
    let _ = crate::abi::exc::raise_attribute_error_text(&format!("'{type_name}' object has no attribute '{attr}'"));
    -1
}

unsafe extern "C" fn bytesio_iter_slot(object: *mut PyObject) -> *mut PyObject {
    let Some(bio) = (unsafe { as_bytesio(object) }) else {
        return raise_type_error("BytesIO iterator receiver is not a BytesIO");
    };
    if bio.buffer.is_none() {
        return raise_bio(closed_bio());
    }
    object
}

unsafe extern "C" fn bytesio_iternext_slot(object: *mut PyObject) -> *mut PyObject {
    let Some(bio) = (unsafe { as_bytesio(object) }) else {
        return raise_type_error("BytesIO iterator receiver is not a BytesIO");
    };
    match bio.read_line(None) {
        Ok(bytes) if bytes.is_empty() => unsafe { abi::pon_raise_stop_iteration(ptr::null_mut()) },
        Ok(bytes) => unsafe { abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) },
        Err(error) => raise_bio(error),
    }
}

/// Shared entry preamble: bounds-checks arity and downcasts the receiver.
unsafe fn bytesio_method_args<'a>(
    argv: *mut *mut PyObject,
    argc: usize,
    name: &str,
    max_extra: usize,
) -> Result<(&'a mut PyBytesIO, &'a [*mut PyObject]), *mut PyObject> {
    let args = match unsafe { method_args(argv, argc, name) } {
        Ok(args) => args,
        Err(message) => return Err(raise_type_error(&message)),
    };
    if args.len() > 1 + max_extra {
        return Err(raise_type_error(&format!(
            "{name}() expected at most {max_extra} arguments, got {}",
            args.len() - 1
        )));
    }
    let Some(bio) = (unsafe { as_bytesio(args[0]) }) else {
        return Err(raise_type_error(&format!("{name}() receiver is not a BytesIO")));
    };
    Ok((bio, &args[1..]))
}

unsafe extern "C" fn bytesio_read_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (bio, args) = match unsafe { bytesio_method_args(argv, argc, "read", 1) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    let size = match bio_optional_size(args.first().copied()) {
        Ok(size) => size,
        Err(error) => return raise_bio(error),
    };
    match bio.read_bytes(size) {
        Ok(bytes) => unsafe { abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) },
        Err(error) => raise_bio(error),
    }
}

unsafe extern "C" fn bytesio_readline_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (bio, args) = match unsafe { bytesio_method_args(argv, argc, "readline", 1) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    let size = match bio_optional_size(args.first().copied()) {
        Ok(size) => size,
        Err(error) => return raise_bio(error),
    };
    match bio.read_line(size) {
        Ok(bytes) => unsafe { abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) },
        Err(error) => raise_bio(error),
    }
}

unsafe extern "C" fn bytesio_readlines_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (bio, args) = match unsafe { bytesio_method_args(argv, argc, "readlines", 1) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    let hint = match bio_optional_size(args.first().copied()) {
        Ok(hint) => hint,
        Err(error) => return raise_bio(error),
    };
    let hint = hint.filter(|&hint| hint > 0);
    let mut lines = Vec::new();
    let mut total = 0_usize;
    loop {
        match bio.read_line(None) {
            Ok(bytes) if bytes.is_empty() => break,
            Ok(bytes) => {
                total += bytes.len();
                let line = unsafe { abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) };
                if line.is_null() {
                    return ptr::null_mut();
                }
                lines.push(line);
                if hint.is_some_and(|hint| total as i64 >= hint) {
                    break;
                }
            }
            Err(error) => return raise_bio(error),
        }
    }
    super::builtins_mod::alloc_list(lines)
}

unsafe extern "C" fn bytesio_readinto_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (bio, args) = match unsafe { bytesio_method_args(argv, argc, "readinto", 1) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    let Some(&target) = args.first() else {
        return raise_type_error("readinto() takes exactly one argument (0 given)");
    };
    let target = crate::tag::untag_arg(target);
    if target.is_null() {
        return raise_type_error("readinto() argument must be read-write bytes-like object, not 'NoneType'");
    }
    // SAFETY: Untagged live pointer; type checks gate each downcast.
    let ty = unsafe { (*target).ob_type };
    let (dst, dst_len) = if bytearray_::is_bytearray_type(ty) {
        let bytearray = unsafe { &mut *target.cast::<bytearray_::PyByteArray>() };
        (bytearray.bytes.as_mut_ptr(), bytearray.bytes.len())
    } else if memoryview::is_memoryview_type(ty) {
        let view = unsafe { &mut *target.cast::<memoryview::PyMemoryView>() };
        if view.released {
            return raise_value_error(memoryview::RELEASED_ERROR);
        }
        if view.readonly {
            return raise_type_error("readinto() argument must be read-write bytes-like object, not memoryview");
        }
        (view.data, view.len)
    } else {
        let type_name = unsafe { crate::types::dict::type_name(target) }.unwrap_or("object");
        return raise_type_error(&format!(
            "readinto() argument must be read-write bytes-like object, not {type_name}"
        ));
    };
    match bio.read_into_raw(dst, dst_len) {
        Ok(count) => unsafe { abi::pon_const_int(count as i64) },
        Err(error) => raise_bio(error),
    }
}

unsafe extern "C" fn bytesio_write_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (bio, args) = match unsafe { bytesio_method_args(argv, argc, "write", 1) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    let Some(&data) = args.first() else {
        return raise_type_error("write() takes exactly one argument (0 given)");
    };
    // CPython order: closed, then exports, then the buffer-protocol check.
    if bio.buffer.is_none() {
        return raise_bio(closed_bio());
    }
    if bio.exports > 0 {
        return raise_bio(BioError::Buffer);
    }
    let data = match bytes_like_bytes(data) {
        Ok(data) => data,
        Err(error) => return raise_bio(error),
    };
    match bio.write_bytes(&data) {
        Ok(count) => unsafe { abi::pon_const_int(count as i64) },
        Err(error) => raise_bio(error),
    }
}

unsafe extern "C" fn bytesio_writelines_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (bio, args) = match unsafe { bytesio_method_args(argv, argc, "writelines", 1) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    let Some(&lines) = args.first() else {
        return raise_type_error("writelines() takes exactly one argument (0 given)");
    };
    if bio.buffer.is_none() {
        return raise_bio(closed_bio());
    }
    let iter = unsafe { abi::pon_get_iter(lines, ptr::null_mut()) };
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
        if bio.exports > 0 {
            return raise_bio(BioError::Buffer);
        }
        let data = match bytes_like_bytes(item) {
            Ok(data) => data,
            Err(error) => return raise_bio(error),
        };
        if let Err(error) = bio.write_bytes(&data) {
            return raise_bio(error);
        }
    }
    unsafe { abi::pon_none() }
}

unsafe extern "C" fn bytesio_seek_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (bio, args) = match unsafe { bytesio_method_args(argv, argc, "seek", 2) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    let Some(&offset) = args.first() else {
        return raise_type_error("seek() takes at least 1 argument (0 given)");
    };
    let offset = match bio_index_arg(offset) {
        Ok(offset) => offset,
        Err(error) => return raise_bio(error),
    };
    let whence = match args.get(1) {
        Some(&whence) => match bio_index_arg(whence) {
            Ok(whence) => whence,
            Err(error) => return raise_bio(error),
        },
        None => 0,
    };
    match bio.seek_to(offset, whence) {
        Ok(position) => unsafe { abi::pon_const_int(position as i64) },
        Err(error) => raise_bio(error),
    }
}

unsafe extern "C" fn bytesio_tell_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (bio, _) = match unsafe { bytesio_method_args(argv, argc, "tell", 0) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    if bio.buffer.is_none() {
        return raise_bio(closed_bio());
    }
    unsafe { abi::pon_const_int(bio.pos as i64) }
}

unsafe extern "C" fn bytesio_truncate_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (bio, args) = match unsafe { bytesio_method_args(argv, argc, "truncate", 1) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    let size = match bio_optional_size(args.first().copied()) {
        Ok(size) => size,
        Err(error) => return raise_bio(error),
    };
    match bio.truncate_to(size) {
        Ok(size) => unsafe { abi::pon_const_int(size) },
        Err(error) => raise_bio(error),
    }
}

unsafe extern "C" fn bytesio_flush_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (bio, _) = match unsafe { bytesio_method_args(argv, argc, "flush", 0) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    if bio.buffer.is_none() {
        return raise_bio(closed_bio());
    }
    unsafe { abi::pon_none() }
}

unsafe extern "C" fn bytesio_close_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (bio, _) = match unsafe { bytesio_method_args(argv, argc, "close", 0) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    if bio.exports > 0 {
        return raise_bio(BioError::Buffer);
    }
    bio.buffer = None;
    unsafe { abi::pon_none() }
}

unsafe extern "C" fn bytesio_getvalue_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (bio, _) = match unsafe { bytesio_method_args(argv, argc, "getvalue", 0) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    let Some(buffer) = bio.buffer.as_ref() else {
        return raise_bio(closed_bio());
    };
    unsafe { abi::str_::pon_const_bytes(buffer.as_ptr(), buffer.len()) }
}

/// `getbuffer()`: a writable B-format memoryview aliasing the live buffer.
/// The export count pins the buffer size until every derived view releases.
unsafe extern "C" fn bytesio_getbuffer_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "getbuffer") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    if args.len() != 1 {
        return raise_type_error(&format!("getbuffer() expected 0 arguments, got {}", args.len() - 1));
    }
    let receiver = crate::tag::untag_arg(args[0]);
    let Some(bio) = (unsafe { as_bytesio(receiver) }) else {
        return raise_type_error("getbuffer() receiver is not a BytesIO");
    };
    let Some(buffer) = bio.buffer.as_mut() else {
        return raise_bio(closed_bio());
    };
    if let Err(message) = crate::abi::str_::install_memoryview_slots() {
        return abi::return_null_with_error(message);
    }
    let view = memoryview::boxed_memoryview_from_raw(receiver, buffer.as_mut_ptr(), buffer.len(), false, b'B');
    bio.exports += 1;
    view.cast::<PyObject>()
}

/// `readable()`/`writable()`/`seekable()`: `True`, once open is proven.
unsafe extern "C" fn bytesio_true_flag_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (bio, _) = match unsafe { bytesio_method_args(argv, argc, "readable", 0) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    if bio.buffer.is_none() {
        return raise_bio(closed_bio());
    }
    unsafe { abi::number::pon_const_bool(1) }
}

unsafe extern "C" fn bytesio_isatty_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (bio, _) = match unsafe { bytesio_method_args(argv, argc, "isatty", 0) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    if bio.buffer.is_none() {
        return raise_bio(closed_bio());
    }
    unsafe { abi::number::pon_const_bool(0) }
}

/// `fileno()`: no host descriptor exists.  CPython raises
/// `io.UnsupportedOperation` (an OSError/ValueError subclass); pon raises the
/// OSError leg with the same message text.
unsafe extern "C" fn bytesio_fileno_method(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    unsafe { abi::exc::pon_raise_os_error("fileno".as_ptr(), "fileno".len()) }
}

/// `detach()`: same UnsupportedOperation contract as `fileno()`.
unsafe extern "C" fn bytesio_detach_method(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    unsafe { abi::exc::pon_raise_os_error("detach".as_ptr(), "detach".len()) }
}

unsafe extern "C" fn bytesio_enter_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "__enter__") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    let Some(bio) = (unsafe { as_bytesio(args[0]) }) else {
        return raise_type_error("__enter__() receiver is not a BytesIO");
    };
    if bio.buffer.is_none() {
        return raise_bio(closed_bio());
    }
    args[0]
}

unsafe extern "C" fn bytesio_exit_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "__exit__") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    let Some(bio) = (unsafe { as_bytesio(args[0]) }) else {
        return raise_type_error("__exit__() receiver is not a BytesIO");
    };
    if bio.exports > 0 {
        return raise_bio(BioError::Buffer);
    }
    bio.buffer = None;
    unsafe { abi::pon_none() }
}

unsafe extern "C" fn bytesio_iter_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "__iter__") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    unsafe { bytesio_iter_slot(args[0]) }
}

unsafe extern "C" fn bytesio_next_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "__next__") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    unsafe { bytesio_iternext_slot(args[0]) }
}

// ---------------------------------------------------------------------------
// StringIO: real in-memory text stream (CPython `Modules/_io/stringio.c`
// semantics).  The backing store is a growable code-point vector, so
// `tell`/`seek` offsets count code points like CPython's UCS4 buffer; the
// position may park beyond EOF (reads see empty, writes NUL-fill the gap).
// Newline handling follows the constructor's `newline=` mode: `None` decodes
// universal newlines on write, `'\r'`/`'\r\n'` translate `'\n'` on write, and
// `''`/`'\n'` pass text through verbatim.

/// Constructor `newline=` mode (CPython `stringio.c` write/readline pairing).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SioNewline {
    /// `newline=None`: writes decode `\r\n`/`\r` to `\n`; lines end at `\n`.
    Universal,
    /// `newline=''`: verbatim writes; lines end at any of `\r\n`/`\r`/`\n`.
    Verbatim,
    /// `newline='\n'`: verbatim writes; lines end at `\n`.
    Lf,
    /// `newline='\r'`: writes translate `\n` to `\r`; lines end at `\r`.
    Cr,
    /// `newline='\r\n'`: writes translate `\n` to `\r\n`; lines end at `\r\n`.
    CrLf,
}

#[repr(C)]
#[derive(Debug)]
struct PyStringIO {
    /// Common object header; this field must remain first.
    ob_base: PyObjectHeader,
    /// Backing code-point buffer. `None` is the closed state.
    buffer: Option<Vec<char>>,
    /// Absolute stream position; `seek` may park it beyond the buffer end.
    pos: usize,
    /// Newline translation mode fixed at construction.
    newline: SioNewline,
}

unsafe impl Send for PyStringIO {}

static STRING_IO_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = Box::new(PyType::new(
        abi::runtime_type_type().cast_const(),
        // Dotted tp_name (the `pickle.PickleBuffer` discipline): `repr(type)`
        // shows the CPython path while `__name__` exposes the tail component.
        "_io.StringIO",
        std::mem::size_of::<PyStringIO>(),
    ));
    ty.tp_new = Some(stringio_new);
    ty.tp_getattro = Some(stringio_getattro);
    ty.tp_setattro = Some(stringio_setattro);
    ty.tp_iter = Some(stringio_iter_slot);
    ty.tp_iternext = Some(stringio_iternext_slot);
    // pon's `__module__` getter defaults static types to "builtins"; carry
    // the CPython value (and the abstract-base `__doc__`) explicitly.
    let namespace = type_::new_namespace();
    if !namespace.is_null() {
        let module = unsafe { pon_const_str("_io".as_ptr(), "_io".len()) };
        let doc = "Text I/O implementation using an in-memory buffer.";
        let doc_object = unsafe { pon_const_str(doc.as_ptr(), doc.len()) };
        if !module.is_null() && !doc_object.is_null() {
            // SAFETY: Freshly allocated namespace box; values are live objects.
            unsafe {
                (*namespace).set(intern("__module__"), module);
                (*namespace).set(intern("__doc__"), doc_object);
            }
            ty.tp_dict = namespace.cast::<PyObject>();
        }
    }
    Box::into_raw(ty) as usize
});

fn stringio_type() -> *mut PyType {
    *STRING_IO_TYPE as *mut PyType
}

unsafe fn as_stringio<'a>(object: *mut PyObject) -> Option<&'a mut PyStringIO> {
    let object = crate::tag::untag_arg(object);
    if object.is_null() {
        return None;
    }
    // Non-forcing type fetch: before the first `_io` import no instance can
    // exist (the `as_bytesio` discipline).
    let ty = LazyLock::get(&STRING_IO_TYPE).map_or(ptr::null(), |&ty| ty as *const PyType);
    if ty.is_null() {
        return None;
    }
    // SAFETY: NULL was rejected above; the type check gates the downcast.
    (unsafe { (*object).ob_type } == ty).then(|| unsafe { &mut *object.cast::<PyStringIO>() })
}

impl PyStringIO {
    /// Splits the open stream into `(buffer, position)` borrows, or the
    /// closed-file ValueError.
    fn open_parts(&mut self) -> Result<(&mut Vec<char>, &mut usize), BioError> {
        let Self { buffer, pos, .. } = self;
        buffer.as_mut().map(|buffer| (buffer, pos)).ok_or_else(closed_bio)
    }

    /// Applies the constructor's `newline=` mode to outgoing text.
    fn translated(&self, text: &str) -> String {
        match self.newline {
            SioNewline::Universal => text.replace("\r\n", "\n").replace('\r', "\n"),
            SioNewline::Verbatim | SioNewline::Lf => text.to_owned(),
            SioNewline::Cr => text.replace('\n', "\r"),
            SioNewline::CrLf => text.replace('\n', "\r\n"),
        }
    }

    /// `write`: overwrite/extend at the current position, NUL-filling any
    /// gap left by a past-EOF seek.  Returns the code-point length of the
    /// ORIGINAL text (CPython returns `len(s)` before translation).
    fn write_text(&mut self, text: &str) -> Result<usize, BioError> {
        let written: Vec<char> = self.translated(text).chars().collect();
        let (buffer, pos) = self.open_parts()?;
        if !written.is_empty() {
            if *pos > buffer.len() {
                buffer.resize(*pos, '\0');
            }
            let end = *pos + written.len();
            if end > buffer.len() {
                buffer.resize(end, '\0');
            }
            buffer[*pos..end].copy_from_slice(&written);
            *pos = end;
        }
        Ok(text.chars().count())
    }

    /// `read`: up to `size` code points from the current position
    /// (`None`/negative reads to EOF); a position parked past EOF reads
    /// empty without moving.
    fn read_text(&mut self, size: Option<i64>) -> Result<String, BioError> {
        let (buffer, pos) = self.open_parts()?;
        let start = (*pos).min(buffer.len());
        let available = buffer.len() - start;
        let count = match size {
            Some(size) if size >= 0 => (size as usize).min(available),
            _ => available,
        };
        let out = buffer[start..start + count].iter().collect();
        *pos += count;
        Ok(out)
    }

    /// Length of the line starting `window[0]`, INCLUDING its line ending,
    /// per the `newline=` mode; `window.len()` when no ending is found.
    fn line_length(&self, window: &[char]) -> usize {
        let limit = window.len();
        match self.newline {
            SioNewline::Universal | SioNewline::Lf => {
                window.iter().position(|&ch| ch == '\n').map_or(limit, |at| at + 1)
            }
            SioNewline::Verbatim => {
                for (index, &ch) in window.iter().enumerate() {
                    if ch == '\n' {
                        return index + 1;
                    }
                    if ch == '\r' {
                        // `\r\n` counts as one ending; lone `\r` ends a line.
                        return if window.get(index + 1) == Some(&'\n') { index + 2 } else { index + 1 };
                    }
                }
                limit
            }
            SioNewline::Cr => window.iter().position(|&ch| ch == '\r').map_or(limit, |at| at + 1),
            SioNewline::CrLf => window
                .windows(2)
                .position(|pair| pair == ['\r', '\n'])
                .map_or(limit, |at| at + 2),
        }
    }

    /// `readline`: code points through the next line ending (inclusive),
    /// capped by `size`.
    fn read_line(&mut self, size: Option<i64>) -> Result<String, BioError> {
        let limit = {
            let (buffer, pos) = self.open_parts()?;
            let start = (*pos).min(buffer.len());
            let available = buffer.len() - start;
            match size {
                Some(size) if size >= 0 => (size as usize).min(available),
                _ => available,
            }
        };
        let start = self.pos.min(self.buffer.as_ref().map_or(0, Vec::len));
        let window: Vec<char> = {
            let buffer = self.buffer.as_ref().ok_or_else(closed_bio)?;
            buffer[start..start + limit].to_vec()
        };
        let count = self.line_length(&window);
        self.pos = start + count;
        Ok(window[..count].iter().collect())
    }

    /// `seek`: absolute negative positions raise; cur/end-relative seeks
    /// accept only offset 0 (CPython `_io_StringIO_seek_impl`).
    fn seek_to(&mut self, offset: i64, whence: i64) -> Result<usize, BioError> {
        let (buffer, pos) = self.open_parts()?;
        match whence {
            0 => {
                if offset < 0 {
                    return Err(BioError::Value(format!("Negative seek position {offset}")));
                }
                *pos = offset as usize;
            }
            1 => {
                if offset != 0 {
                    return Err(BioError::Value("Can't do nonzero cur-relative seeks".to_owned()));
                }
            }
            2 => {
                if offset != 0 {
                    return Err(BioError::Value("Can't do nonzero end-relative seeks".to_owned()));
                }
                *pos = buffer.len();
            }
            _ => {
                return Err(BioError::Value(format!("Invalid whence ({whence}, should be 0, 1 or 2)")));
            }
        }
        Ok(*pos)
    }

    /// `truncate`: shrink-only resize that returns the REQUESTED size and
    /// never moves the position (CPython contract).
    fn truncate_to(&mut self, size: Option<i64>) -> Result<i64, BioError> {
        let (buffer, pos) = self.open_parts()?;
        let size = size.unwrap_or(*pos as i64);
        if size < 0 {
            return Err(BioError::Value(format!("Negative size value {size}")));
        }
        if (size as usize) < buffer.len() {
            buffer.truncate(size as usize);
        }
        Ok(size)
    }
}

/// Parses the constructor/`__init__` `newline=` argument.
fn stringio_newline_mode(object: Option<*mut PyObject>) -> Result<SioNewline, *mut PyObject> {
    let Some(object) = object.map(crate::tag::untag_arg) else {
        return Ok(SioNewline::Lf);
    };
    if is_none(object) {
        return Ok(SioNewline::Universal);
    }
    let Some(text) = (unsafe { type_::unicode_text(object) }) else {
        let type_name = unsafe { crate::types::dict::type_name(object) }.unwrap_or("object");
        return Err(raise_type_error(&format!("newline must be str or None, not {type_name}")));
    };
    match text {
        "" => Ok(SioNewline::Verbatim),
        "\n" => Ok(SioNewline::Lf),
        "\r" => Ok(SioNewline::Cr),
        "\r\n" => Ok(SioNewline::CrLf),
        other => Err(raise_value_error(&format!("illegal newline value: '{other}'"))),
    }
}

/// `tp_new` for `_io.StringIO(initial_value='', newline='\n')`.
unsafe extern "C" fn stringio_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    let positional = match unsafe { type_::positional_args_from_object(args) } {
        Ok(args) => args,
        Err(message) => {
            pon_err_set(message);
            return ptr::null_mut();
        }
    };
    if positional.len() > 2 {
        return raise_type_error(&format!("StringIO() takes at most 2 arguments ({} given)", positional.len()));
    }
    let mut initial = positional.first().copied();
    let mut newline = positional.get(1).copied();
    if !kwargs.is_null() {
        let entries = match unsafe { crate::types::dict::dict_entries_snapshot(kwargs) } {
            Ok(entries) => entries,
            Err(message) => return raise_type_error(&message),
        };
        for entry in entries {
            let Some(key) = (unsafe { type_::unicode_text(entry.key) }) else {
                return raise_type_error("keywords must be strings");
            };
            let (slot, position) = match key {
                "initial_value" => (&mut initial, 1),
                "newline" => (&mut newline, 2),
                other => {
                    return raise_type_error(&format!("'{other}' is an invalid keyword argument for StringIO()"));
                }
            };
            if slot.is_some() {
                return raise_type_error(&format!(
                    "argument for StringIO() given by name ('{key}') and position ({position})"
                ));
            }
            *slot = Some(entry.value);
        }
    }
    let newline = match stringio_newline_mode(newline) {
        Ok(mode) => mode,
        Err(raised) => return raised,
    };
    let mut sio = PyStringIO {
        ob_base: PyObjectHeader::new(stringio_type()),
        buffer: Some(Vec::new()),
        pos: 0,
        newline,
    };
    match initial.map(crate::tag::untag_arg) {
        None => {}
        Some(object) if is_none(object) => {}
        Some(object) => {
            let Some(text) = (unsafe { type_::unicode_text(object) }) else {
                let type_name = unsafe { crate::types::dict::type_name(object) }.unwrap_or("object");
                return raise_type_error(&format!("initial_value must be str or None, not {type_name}"));
            };
            // CPython seeds via `write(initial_value)` (translation applies)
            // and rewinds to position 0.
            if let Err(error) = sio.write_text(text) {
                return raise_bio(error);
            }
            sio.pos = 0;
        }
    }
    Box::into_raw(Box::new(sio)).cast::<PyObject>()
}

unsafe extern "C" fn stringio_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(attr) = (unsafe { type_::unicode_text(name) }) else {
        return raise_type_error("StringIO attribute name must be str");
    };
    let Some(sio) = (unsafe { as_stringio(object) }) else {
        return raise_type_error("StringIO method receiver is not a StringIO");
    };
    match attr {
        "closed" => unsafe { abi::number::pon_const_bool(i32::from(sio.buffer.is_none())) },
        // pon does not track the newline kinds seen; CPython starts at None.
        "newlines" => unsafe { abi::pon_none() },
        "line_buffering" => unsafe { abi::number::pon_const_bool(0) },
        "read" => bound_file_method(object, attr, stringio_read_method),
        "readline" => bound_file_method(object, attr, stringio_readline_method),
        "readlines" => bound_file_method(object, attr, stringio_readlines_method),
        "write" => bound_file_method(object, attr, stringio_write_method),
        "writelines" => bound_file_method(object, attr, stringio_writelines_method),
        "seek" => bound_file_method(object, attr, stringio_seek_method),
        "tell" => bound_file_method(object, attr, stringio_tell_method),
        "truncate" => bound_file_method(object, attr, stringio_truncate_method),
        "flush" => bound_file_method(object, attr, stringio_flush_method),
        "close" => bound_file_method(object, attr, stringio_close_method),
        "getvalue" => bound_file_method(object, attr, stringio_getvalue_method),
        "readable" | "writable" | "seekable" => bound_file_method(object, attr, stringio_true_flag_method),
        "isatty" => bound_file_method(object, attr, stringio_isatty_method),
        "fileno" => bound_file_method(object, attr, stringio_fileno_method),
        "detach" => bound_file_method(object, attr, stringio_detach_method),
        "__enter__" => bound_file_method(object, attr, stringio_enter_method),
        "__exit__" => bound_file_method(object, attr, stringio_exit_method),
        "__iter__" => bound_file_method(object, attr, stringio_iter_method),
        "__next__" => bound_file_method(object, attr, stringio_next_method),
        _ => raise_attribute_error(attr),
    }
}

/// StringIO instances carry no writable attributes (CPython: no `__dict__`).
unsafe extern "C" fn stringio_setattro(object: *mut PyObject, name: *mut PyObject, _value: *mut PyObject) -> core::ffi::c_int {
    let attr = unsafe { type_::unicode_text(name) }.unwrap_or("?");
    let type_name = unsafe { crate::types::dict::type_name(crate::tag::untag_arg(object)) }.unwrap_or("_io.StringIO");
    let _ = crate::abi::exc::raise_attribute_error_text(&format!("'{type_name}' object has no attribute '{attr}'"));
    -1
}

unsafe extern "C" fn stringio_iter_slot(object: *mut PyObject) -> *mut PyObject {
    let Some(sio) = (unsafe { as_stringio(object) }) else {
        return raise_type_error("StringIO iterator receiver is not a StringIO");
    };
    if sio.buffer.is_none() {
        return raise_value_error("I/O operation on closed file.");
    }
    object
}

unsafe extern "C" fn stringio_iternext_slot(object: *mut PyObject) -> *mut PyObject {
    let Some(sio) = (unsafe { as_stringio(object) }) else {
        return raise_type_error("StringIO iterator receiver is not a StringIO");
    };
    match sio.read_line(None) {
        Ok(text) if text.is_empty() => unsafe { abi::pon_raise_stop_iteration(ptr::null_mut()) },
        Ok(text) => alloc_str(&text),
        Err(error) => raise_bio(error),
    }
}

/// Shared entry preamble: bounds-checks arity and downcasts the receiver.
unsafe fn stringio_method_args<'a>(
    argv: *mut *mut PyObject,
    argc: usize,
    name: &str,
    max_extra: usize,
) -> Result<(&'a mut PyStringIO, &'a [*mut PyObject]), *mut PyObject> {
    let args = match unsafe { method_args(argv, argc, name) } {
        Ok(args) => args,
        Err(message) => return Err(raise_type_error(&message)),
    };
    if args.len() > 1 + max_extra {
        return Err(raise_type_error(&format!(
            "{name}() expected at most {max_extra} arguments, got {}",
            args.len() - 1
        )));
    }
    let Some(sio) = (unsafe { as_stringio(args[0]) }) else {
        return Err(raise_type_error(&format!("{name}() receiver is not a StringIO")));
    };
    Ok((sio, &args[1..]))
}

unsafe extern "C" fn stringio_read_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (sio, args) = match unsafe { stringio_method_args(argv, argc, "read", 1) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    let size = match bio_optional_size(args.first().copied()) {
        Ok(size) => size,
        Err(error) => return raise_bio(error),
    };
    match sio.read_text(size) {
        Ok(text) => alloc_str(&text),
        Err(error) => raise_bio(error),
    }
}

unsafe extern "C" fn stringio_readline_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (sio, args) = match unsafe { stringio_method_args(argv, argc, "readline", 1) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    let size = match bio_optional_size(args.first().copied()) {
        Ok(size) => size,
        Err(error) => return raise_bio(error),
    };
    match sio.read_line(size) {
        Ok(text) => alloc_str(&text),
        Err(error) => raise_bio(error),
    }
}

unsafe extern "C" fn stringio_readlines_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (sio, args) = match unsafe { stringio_method_args(argv, argc, "readlines", 1) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    let hint = match bio_optional_size(args.first().copied()) {
        Ok(size) => size,
        Err(error) => return raise_bio(error),
    };
    let mut lines = Vec::new();
    let mut total = 0i64;
    loop {
        let line = match sio.read_line(None) {
            Ok(line) => line,
            Err(error) => return raise_bio(error),
        };
        if line.is_empty() {
            break;
        }
        total += line.chars().count() as i64;
        let object = alloc_str(&line);
        if object.is_null() {
            return ptr::null_mut();
        }
        lines.push(object);
        if matches!(hint, Some(hint) if hint > 0 && total >= hint) {
            break;
        }
    }
    unsafe { abi::seq::pon_build_list(lines.as_mut_ptr(), lines.len()) }
}

unsafe extern "C" fn stringio_write_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (sio, args) = match unsafe { stringio_method_args(argv, argc, "write", 1) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    let Some(&value) = args.first() else {
        return raise_type_error("write() missing 1 required positional argument: 's'");
    };
    let value = crate::tag::untag_arg(value);
    let Some(text) = (unsafe { type_::unicode_text(value) }) else {
        let type_name = unsafe { crate::types::dict::type_name(value) }.unwrap_or("object");
        return raise_type_error(&format!("string argument expected, got '{type_name}'"));
    };
    match sio.write_text(text) {
        Ok(count) => unsafe { abi::pon_const_int(count as i64) },
        Err(error) => raise_bio(error),
    }
}

unsafe extern "C" fn stringio_writelines_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (sio, args) = match unsafe { stringio_method_args(argv, argc, "writelines", 1) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    if sio.buffer.is_none() {
        return raise_bio(closed_bio());
    }
    let Some(&lines) = args.first() else {
        return raise_type_error("writelines() missing 1 required positional argument: 'lines'");
    };
    let receiver = args[0];
    // SAFETY: Iteration helpers follow the NULL-sentinel error contract.
    let iterator = unsafe { crate::abstract_op::get_iter(lines) };
    if iterator.is_null() {
        return ptr::null_mut();
    }
    loop {
        // SAFETY: `iterator` is live; NULL return distinguishes exhaustion via
        // the pending-StopIteration check below.
        let item = unsafe { crate::abstract_op::iter_next(iterator) };
        if item.is_null() {
            if stop_iteration_pending() || !pon_err_occurred() {
                pon_err_clear();
                break;
            }
            return ptr::null_mut();
        }
        let mut write_args = [receiver, item];
        if unsafe { stringio_write_method(write_args.as_mut_ptr(), write_args.len()) }.is_null() {
            return ptr::null_mut();
        }
    }
    unsafe { abi::pon_none() }
}

unsafe extern "C" fn stringio_seek_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (sio, args) = match unsafe { stringio_method_args(argv, argc, "seek", 2) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    let Some(&target) = args.first() else {
        return raise_type_error("seek() missing 1 required positional argument: 'pos'");
    };
    let offset = match bio_index_arg(target) {
        Ok(offset) => offset,
        Err(error) => return raise_bio(error),
    };
    let whence = match args.get(1).map(|&object| bio_index_arg(object)) {
        None => 0,
        Some(Ok(whence)) => whence,
        Some(Err(error)) => return raise_bio(error),
    };
    match sio.seek_to(offset, whence) {
        Ok(position) => unsafe { abi::pon_const_int(position as i64) },
        Err(error) => raise_bio(error),
    }
}

unsafe extern "C" fn stringio_tell_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (sio, _) = match unsafe { stringio_method_args(argv, argc, "tell", 0) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    if sio.buffer.is_none() {
        return raise_bio(closed_bio());
    }
    unsafe { abi::pon_const_int(sio.pos as i64) }
}

unsafe extern "C" fn stringio_truncate_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (sio, args) = match unsafe { stringio_method_args(argv, argc, "truncate", 1) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    let size = match bio_optional_size(args.first().copied()) {
        Ok(size) => size,
        Err(error) => return raise_bio(error),
    };
    match sio.truncate_to(size) {
        Ok(size) => unsafe { abi::pon_const_int(size) },
        Err(error) => raise_bio(error),
    }
}

unsafe extern "C" fn stringio_flush_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (sio, _) = match unsafe { stringio_method_args(argv, argc, "flush", 0) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    if sio.buffer.is_none() {
        return raise_bio(closed_bio());
    }
    unsafe { abi::pon_none() }
}

unsafe extern "C" fn stringio_close_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (sio, _) = match unsafe { stringio_method_args(argv, argc, "close", 0) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    sio.buffer = None;
    unsafe { abi::pon_none() }
}

unsafe extern "C" fn stringio_getvalue_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (sio, _) = match unsafe { stringio_method_args(argv, argc, "getvalue", 0) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    match &sio.buffer {
        Some(buffer) => alloc_str(&buffer.iter().collect::<String>()),
        None => raise_bio(closed_bio()),
    }
}

/// `readable()`/`writable()`/`seekable()`: `True`, once open is proven.
unsafe extern "C" fn stringio_true_flag_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (sio, _) = match unsafe { stringio_method_args(argv, argc, "readable", 0) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    if sio.buffer.is_none() {
        return raise_bio(closed_bio());
    }
    unsafe { abi::number::pon_const_bool(1) }
}

unsafe extern "C" fn stringio_isatty_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (sio, _) = match unsafe { stringio_method_args(argv, argc, "isatty", 0) } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    if sio.buffer.is_none() {
        return raise_bio(closed_bio());
    }
    unsafe { abi::number::pon_const_bool(0) }
}

/// `fileno()`: no OS descriptor backs the buffer; CPython raises
/// `io.UnsupportedOperation`, an OSError subclass — pon reuses the plain
/// OSError leg with the same message text.
unsafe extern "C" fn stringio_fileno_method(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    unsafe { abi::exc::pon_raise_os_error("fileno".as_ptr(), "fileno".len()) }
}

/// `detach()`: same UnsupportedOperation contract as `fileno()`.
unsafe extern "C" fn stringio_detach_method(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    unsafe { abi::exc::pon_raise_os_error("detach".as_ptr(), "detach".len()) }
}

unsafe extern "C" fn stringio_enter_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "__enter__") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    let Some(sio) = (unsafe { as_stringio(args[0]) }) else {
        return raise_type_error("__enter__() receiver is not a StringIO");
    };
    if sio.buffer.is_none() {
        return raise_bio(closed_bio());
    }
    args[0]
}

unsafe extern "C" fn stringio_exit_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "__exit__") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    let Some(sio) = (unsafe { as_stringio(args[0]) }) else {
        return raise_type_error("__exit__() receiver is not a StringIO");
    };
    sio.buffer = None;
    unsafe { abi::number::pon_const_bool(0) }
}

unsafe extern "C" fn stringio_iter_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "__iter__") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    unsafe { stringio_iter_slot(args[0]) }
}

unsafe extern "C" fn stringio_next_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { method_args(argv, argc, "__next__") } {
        Ok(args) => args,
        Err(message) => return raise_type_error(&message),
    };
    unsafe { stringio_iternext_slot(args[0]) }
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
        let bytes_io = bytesio_type();
        if (*bytes_io).tp_base.is_null() {
            (*bytes_io).tp_base = buffered_io_base.cast::<PyType>();
            (*bytes_io).bump_version();
        }
        let string_io = stringio_type();
        if (*string_io).tp_base.is_null() {
            (*string_io).tp_base = text_io_base.cast::<PyType>();
            (*string_io).bump_version();
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
        (intern("BytesIO"), bytesio_type().cast::<PyObject>()),
        (intern("StringIO"), stringio_type().cast::<PyObject>()),
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
    // The keyword binder flattens the full `open(file, mode='r',
    // buffering=-1, encoding=None, errors=None, newline=None, closefd=True,
    // opener=None)` signature into eight positional slots with None filling
    // every absent optional, so a None mode selects the default exactly like
    // an absent slot does.
    let mode_text = match args.get(1) {
        Some(&mode) if !is_none(mode) => expect_str(mode, "open() mode must be str")?.to_owned(),
        _ => "r".to_owned(),
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

    if let Some(&errors) = args.get(4) {
        if !is_none(errors) {
            let text = expect_str(errors, "open() errors must be str or None")?;
            if mode.binary {
                return Err(OpenError::Value("binary mode doesn't take an errors argument".to_owned()));
            }
            // The native text stream decodes strict UTF-8: 'strict' is the
            // one handler that machinery honors; every other policy is
            // refused honestly instead of decoding with the wrong behavior.
            if text != "strict" {
                return Err(OpenError::Value(format!("open() errors='{text}' is not implemented")));
            }
        }
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

/// Process-level std stream (`sys.stdin`/`sys.stdout`/`sys.stderr`) as a
/// text-mode native file over the raw fd.  The object lives in the `sys`
/// module for the process lifetime, so the `File` never drops (the fd is
/// never closed underneath libc); an explicit Python-level `close()` closes
/// the real stream, exactly like CPython.  `readable` selects the fd-0
/// shape (mode `"r"`, read side only); writers pass `false` and keep the
/// write-only stdout/stderr contract.
pub(super) fn std_stream_object(fd: i32, name: &str, readable: bool) -> *mut PyObject {
    use std::os::fd::FromRawFd;
    // SAFETY: fds 0/1/2 are open for the process lifetime; ownership is
    // parked in a static module attribute, never dropped.
    let file = unsafe { File::from_raw_fd(fd) };
    Box::into_raw(Box::new(PyNativeFile {
        ob_base: PyObjectHeader::new(text_file_type()),
        file: Some(file),
        name: name.to_owned(),
        mode: if readable { "r" } else { "w" }.to_owned(),
        binary: false,
        readable,
        writable: !readable,
        append: false,
        encoding: Some("utf-8".to_owned()),
        newline: NewlineMode::Preserve,
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
        "readable" => bound_file_method(object, "readable", file_readable_method),
        "writable" => bound_file_method(object, "writable", file_writable_method),
        "seekable" => bound_file_method(object, "seekable", file_seekable_method),
        "fileno" => bound_file_method(object, "fileno", file_fileno_method),
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
    // Text mode counts `size` in CHARACTERS (CPython `TextIOWrapper.read`);
    // only binary mode and unsized reads take the raw byte path.
    let result = match size {
        Some(count) if !file.binary => read_chars_raw(file, count),
        _ => read_raw(file, size),
    };
    match result {
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

/// Shared zero-argument receiver decode for the IOBase flag methods below.
unsafe fn file_flag_receiver<'a>(
    argv: *mut *mut PyObject,
    argc: usize,
    what: &str,
) -> Result<&'a mut PyNativeFile, *mut PyObject> {
    // SAFETY: Forwarded argument slots per the runtime calling convention.
    let args = match unsafe { method_args(argv, argc, what) } {
        Ok(args) => args,
        Err(message) => return Err(raise_type_error(&message)),
    };
    if args.len() != 1 {
        return Err(raise_type_error(&format!(
            "{what}() expected 0 arguments, got {}",
            args.len().saturating_sub(1)
        )));
    }
    // SAFETY: Receiver slot is live per the call ABI.
    match unsafe { as_file(args[0]) } {
        Some(file) => Ok(file),
        None => Err(raise_type_error(&format!("{what}() receiver is not a native file"))),
    }
}

/// `file.readable()`: the open-mode read flag (CPython `IOBase.readable`).
unsafe extern "C" fn file_readable_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded argument slots per the runtime calling convention.
    let file = match unsafe { file_flag_receiver(argv, argc, "readable") } {
        Ok(file) => file,
        Err(error) => return error,
    };
    // SAFETY: Boolean boxing helper follows the NULL-sentinel contract.
    unsafe { abi::number::pon_const_bool(i32::from(file.readable)) }
}

/// `file.writable()`: the open-mode write flag (CPython `IOBase.writable`).
unsafe extern "C" fn file_writable_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Forwarded argument slots per the runtime calling convention.
    let file = match unsafe { file_flag_receiver(argv, argc, "writable") } {
        Ok(file) => file,
        Err(error) => return error,
    };
    // SAFETY: Boolean boxing helper follows the NULL-sentinel contract.
    unsafe { abi::number::pon_const_bool(i32::from(file.writable)) }
}

/// `file.seekable()`: the honest host probe — a zero-displacement
/// `lseek(fd, 0, SEEK_CUR)`, exactly CPython's `_io` check: regular files
/// answer True, pipes/sockets (ESPIPE) answer False, so a piped stdin
/// reports unseekable on both engines.  Closed files raise like the
/// sibling methods.
unsafe extern "C" fn file_seekable_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let file = match unsafe { file_flag_receiver(argv, argc, "seekable") } {
        Ok(file) => file,
        Err(error) => return error,
    };
    let Some(handle) = file.file.as_mut() else {
        return raise_value_error("I/O operation on closed file.");
    };
    let seekable = handle.seek(SeekFrom::Current(0)).is_ok();
    // SAFETY: Boolean boxing helper follows the NULL-sentinel contract.
    unsafe { abi::number::pon_const_bool(i32::from(seekable)) }
}

/// `file.fileno()`: the wrapped raw descriptor; closed files raise the
/// CPython ValueError.
unsafe extern "C" fn file_fileno_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    use std::os::fd::AsRawFd;
    let file = match unsafe { file_flag_receiver(argv, argc, "fileno") } {
        Ok(file) => file,
        Err(error) => return error,
    };
    let Some(handle) = file.file.as_ref() else {
        return raise_value_error("I/O operation on closed file.");
    };
    // SAFETY: Integer boxing helper follows the NULL-sentinel contract.
    unsafe { abi::pon_const_int(i64::from(handle.as_raw_fd())) }
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

/// Text-mode `read(size)`: `size` counts CHARACTERS, not bytes.  Reads one
/// UTF-8 sequence at a time so a multibyte character is never split at the
/// requested boundary (the old byte-counted slice ValueError'd mid-codepoint
/// on e.g. `read(1)` over `'¡'`).  In universal-translate mode a `\r\n` pair
/// counts as ONE character (it collapses to `\n` downstream) and a bare `\r`
/// uses the same peek/seek-back idiom as `read_line_raw`.  Invalid leading
/// bytes are passed through one byte at a time; `bytes_to_python`'s UTF-8
/// validation stays the single point that rejects them.
fn read_chars_raw(file: &mut PyNativeFile, count: usize) -> Result<Vec<u8>, FileOpError> {
    ensure_readable(file)?;
    let translate = file.newline == NewlineMode::UniversalTranslate;
    let handle = file.file.as_mut().ok_or_else(closed_error)?;
    let mut out = Vec::with_capacity(count);
    let mut chars = 0_usize;
    while chars < count {
        let mut byte = [0_u8; 1];
        if handle.read(&mut byte).map_err(io_op_error)? == 0 {
            break;
        }
        out.push(byte[0]);
        // Continuation bytes owed for this UTF-8 sequence (0 for ASCII and
        // for invalid leading bytes, which count as one unit each).
        let mut pending = match byte[0] {
            0xC0..=0xDF => 1_usize,
            0xE0..=0xEF => 2,
            0xF0..=0xF7 => 3,
            _ => 0,
        };
        while pending > 0 {
            let mut cont = [0_u8; 1];
            if handle.read(&mut cont).map_err(io_op_error)? == 0 {
                // EOF mid-sequence: downstream validation reports it.
                return Ok(out);
            }
            out.push(cont[0]);
            if cont[0] & 0xC0 != 0x80 {
                // Not a continuation byte: the sequence is broken; leave the
                // byte in the buffer for downstream validation to reject.
                break;
            }
            pending -= 1;
        }
        if translate && byte[0] == b'\r' {
            // `\r\n` collapses to one `\n` downstream: consume the pair as a
            // single character; a bare `\r` seeks back like `read_line_raw`.
            let mut next = [0_u8; 1];
            if handle.read(&mut next).map_err(io_op_error)? != 0 {
                if next[0] == b'\n' {
                    out.push(next[0]);
                } else {
                    handle.seek(SeekFrom::Current(-1)).map_err(io_op_error)?;
                }
            }
        }
        chars += 1;
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
        // Hermetic entry state: a stale pending error from a prior test would
        // win over this test's raise (`pon_err_set` preserve discipline).
        pon_err_clear();
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

    fn bytesio_obj(initial: &[u8]) -> *mut PyObject {
        Box::into_raw(Box::new(PyBytesIO {
            ob_base: PyObjectHeader::new(bytesio_type()),
            buffer: Some(initial.to_vec()),
            pos: 0,
            exports: 0,
        }))
        .cast::<PyObject>()
    }

    fn bytes_obj(data: &[u8]) -> *mut PyObject {
        let object = unsafe { abi::str_::pon_const_bytes(data.as_ptr(), data.len()) };
        assert!(!object.is_null());
        object
    }

    #[test]
    fn bytesio_seek_past_eof_reads_empty_and_write_zero_fills() {
        let _guard = test_state_lock();
        init_runtime();
        let object = bytesio_obj(b"ab");
        let bio = unsafe { as_bytesio(object) }.expect("receiver downcast");
        assert_eq!(bio.seek_to(5, 0).unwrap(), 5);
        // Reads past EOF see empty WITHOUT clamping the parked position.
        assert_eq!(bio.read_bytes(None).unwrap(), b"");
        assert_eq!(bio.pos, 5);
        // Writes zero-fill the gap left by the past-EOF seek.
        assert_eq!(bio.write_bytes(b"z").unwrap(), 1);
        assert_eq!(bio.buffer.as_deref().unwrap(), b"ab\x00\x00\x00z");
        // Relative seeks clamp negative results at zero (CPython contract).
        assert_eq!(bio.seek_to(-100, 1).unwrap(), 0);
        assert_eq!(bio.seek_to(-100, 2).unwrap(), 0);
        assert!(matches!(bio.seek_to(-1, 0), Err(BioError::Value(_))));
    }

    #[test]
    fn bytesio_truncate_returns_requested_size_and_keeps_position() {
        let _guard = test_state_lock();
        init_runtime();
        let object = bytesio_obj(b"abcdef");
        let bio = unsafe { as_bytesio(object) }.expect("receiver downcast");
        assert_eq!(bio.seek_to(2, 0).unwrap(), 2);
        assert_eq!(bio.truncate_to(None).unwrap(), 2);
        assert_eq!(bio.buffer.as_deref().unwrap(), b"ab");
        // Shrink-only: an oversized request returns verbatim, buffer intact.
        assert_eq!(bio.truncate_to(Some(100)).unwrap(), 100);
        assert_eq!(bio.buffer.as_deref().unwrap(), b"ab");
        assert_eq!(bio.pos, 2);
        assert!(matches!(bio.truncate_to(Some(-1)), Err(BioError::Value(_))));
    }

    #[test]
    fn bytesio_exports_pin_resizing_until_release() {
        let _guard = test_state_lock();
        init_runtime();
        pon_err_clear();
        let object = bytesio_obj(b"abc");
        let mut buffer_args = [object];
        let view = unsafe { bytesio_getbuffer_method(buffer_args.as_mut_ptr(), buffer_args.len()) };
        assert!(!view.is_null());
        // A live export blocks every resizing operation with BufferError...
        let mut write_args = [object, bytes_obj(b"q")];
        assert!(unsafe { bytesio_write_method(write_args.as_mut_ptr(), write_args.len()) }.is_null());
        assert!(pon_err_message().unwrap_or_default().contains("BufferError"));
        pon_err_clear();
        let mut close_args = [object];
        assert!(unsafe { bytesio_close_method(close_args.as_mut_ptr(), close_args.len()) }.is_null());
        assert!(pon_err_message().unwrap_or_default().contains("BufferError"));
        pon_err_clear();
        // ...while reads stay open (CPython allows them under exports).
        {
            let bio = unsafe { as_bytesio(object) }.expect("receiver downcast");
            assert_eq!(bio.read_bytes(Some(2)).unwrap(), b"ab");
            assert_eq!(bio.exports, 1);
        }
        // The str_.rs release seam decrements exactly once per view.
        bytesio_export_released(object);
        {
            let bio = unsafe { as_bytesio(object) }.expect("receiver downcast");
            assert_eq!(bio.exports, 0);
        }
        // Saturating: replayed releases never underflow.
        bytesio_export_released(object);
        let bio = unsafe { as_bytesio(object) }.expect("receiver downcast");
        assert_eq!(bio.exports, 0);
        assert_eq!(bio.write_bytes(b"z").unwrap(), 1);
    }

    #[test]
    fn bytesio_closed_operations_raise_value_error() {
        let _guard = test_state_lock();
        init_runtime();
        pon_err_clear();
        let object = bytesio_obj(b"bye");
        let mut close_args = [object];
        assert!(!unsafe { bytesio_close_method(close_args.as_mut_ptr(), close_args.len()) }.is_null());
        // Idempotent close.
        assert!(!unsafe { bytesio_close_method(close_args.as_mut_ptr(), close_args.len()) }.is_null());
        let mut read_args = [object];
        assert!(unsafe { bytesio_read_method(read_args.as_mut_ptr(), read_args.len()) }.is_null());
        assert!(
            pon_err_message()
                .unwrap_or_default()
                .contains("ValueError: I/O operation on closed file.")
        );
        pon_err_clear();
        let mut buffer_args = [object];
        assert!(unsafe { bytesio_getbuffer_method(buffer_args.as_mut_ptr(), buffer_args.len()) }.is_null());
        assert!(
            pon_err_message()
                .unwrap_or_default()
                .contains("ValueError: I/O operation on closed file.")
        );
    }
}
