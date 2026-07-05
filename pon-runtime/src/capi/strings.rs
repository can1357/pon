//! Strings family: str/bytes/bytearray construction and extraction.

use core::ffi::{c_char, c_int, c_void};
use core::ptr;
use std::collections::HashMap;
use std::ffi::CStr;
use std::sync::{LazyLock, Mutex};

use crate::abi;
use crate::object::PyObject;
use crate::types::exc::ExceptionKind;

use super::c_string;

type PySsizeT = isize;

/// C mirror: `include/pon_capi/strings.h` `PyPonCapiStrings`.
#[repr(C)]
pub(crate) struct PyPonCapiStrings {
    unicode_from_string: unsafe extern "C" fn(*const c_char) -> *mut PyObject,
    unicode_from_string_and_size: unsafe extern "C" fn(*const c_char, PySsizeT) -> *mut PyObject,
    unicode_as_utf8: unsafe extern "C" fn(*mut PyObject) -> *const c_char,
    unicode_as_utf8_and_size: unsafe extern "C" fn(*mut PyObject, *mut PySsizeT) -> *const c_char,
    unicode_get_length: unsafe extern "C" fn(*mut PyObject) -> PySsizeT,
    unicode_decode_utf8: unsafe extern "C" fn(*const c_char, PySsizeT, *const c_char) -> *mut PyObject,
    unicode_decode_ascii: unsafe extern "C" fn(*const c_char, PySsizeT, *const c_char) -> *mut PyObject,
    unicode_decode_latin1: unsafe extern "C" fn(*const c_char, PySsizeT, *const c_char) -> *mut PyObject,
    unicode_as_utf8_string: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    unicode_as_ascii_string: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    unicode_intern_from_string: unsafe extern "C" fn(*const c_char) -> *mut PyObject,
    unicode_compare: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int,
    unicode_compare_with_ascii_string: unsafe extern "C" fn(*mut PyObject, *const c_char) -> c_int,
    unicode_concat: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
    bytes_from_string_and_size: unsafe extern "C" fn(*const c_char, PySsizeT) -> *mut PyObject,
    bytes_from_string: unsafe extern "C" fn(*const c_char) -> *mut PyObject,
    bytes_size: unsafe extern "C" fn(*mut PyObject) -> PySsizeT,
    bytes_as_string: unsafe extern "C" fn(*mut PyObject) -> *mut c_char,
    bytes_as_string_and_size: unsafe extern "C" fn(*mut PyObject, *mut *mut c_char, *mut PySsizeT) -> c_int,
    bytes_concat: unsafe extern "C" fn(*mut *mut PyObject, *mut PyObject),
    bytearray_from_string_and_size: unsafe extern "C" fn(*const c_char, PySsizeT) -> *mut PyObject,
    bytearray_size: unsafe extern "C" fn(*mut PyObject) -> PySsizeT,
    bytearray_as_string: unsafe extern "C" fn(*mut PyObject) -> *mut c_char,
    unicode_check: unsafe extern "C" fn(*mut PyObject) -> c_int,
    unicode_check_exact: unsafe extern "C" fn(*mut PyObject) -> c_int,
    bytes_check: unsafe extern "C" fn(*mut PyObject) -> c_int,
    bytes_check_exact: unsafe extern "C" fn(*mut PyObject) -> c_int,
    bytearray_check: unsafe extern "C" fn(*mut PyObject) -> c_int,
    bytearray_check_exact: unsafe extern "C" fn(*mut PyObject) -> c_int,
    unicode_from_utf8: unsafe extern "C" fn(*const c_char, PySsizeT) -> *mut PyObject,
    object_str: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    object_repr: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    unicode_kind: unsafe extern "C" fn(*mut PyObject) -> c_int,
    unicode_data: unsafe extern "C" fn(*mut PyObject) -> *const c_void,
    unicode_read_char: unsafe extern "C" fn(*mut PyObject, PySsizeT) -> u32,
    unicode_is_ascii: unsafe extern "C" fn(*mut PyObject) -> c_int,
    unicode_as_latin1_string: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
}

unsafe impl Send for PyPonCapiStrings {}
unsafe impl Sync for PyPonCapiStrings {}

/// NUL-terminated UTF-8/bytes C views keyed by the Python object address.
/// Inserting a view also CAPI-pins the object, so the view's intended lifetime
/// is the object's C-API lifetime.  The cache never evicts; this deliberately
/// prefers pointer stability over reclaiming compatibility-shim scratch space.
static UTF8_CACHE: LazyLock<Mutex<HashMap<usize, Box<[u8]>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static BYTES_CACHE: LazyLock<Mutex<HashMap<usize, Box<[u8]>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static INTERNED_UNICODE: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static UNICODE_DATA_CACHE: LazyLock<Mutex<HashMap<usize, UnicodeDataCache>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

enum UnicodeDataCache {
    Ucs1(Box<[u8]>),
    Ucs2(Box<[u16]>),
    Ucs4(Box<[u32]>),
}

impl UnicodeDataCache {
    fn as_ptr(&self) -> *const c_void {
        match self {
            Self::Ucs1(data) => data.as_ptr().cast::<c_void>(),
            Self::Ucs2(data) => data.as_ptr().cast::<c_void>(),
            Self::Ucs4(data) => data.as_ptr().cast::<c_void>(),
        }
    }
}

pub(crate) fn build() -> PyPonCapiStrings {
    PyPonCapiStrings {
        unicode_from_string: capi_unicode_from_string,
        unicode_from_string_and_size: capi_unicode_from_string_and_size,
        unicode_as_utf8: capi_unicode_as_utf8,
        unicode_as_utf8_and_size: capi_unicode_as_utf8_and_size,
        unicode_get_length: capi_unicode_get_length,
        unicode_decode_utf8: capi_unicode_decode_utf8,
        unicode_decode_ascii: capi_unicode_decode_ascii,
        unicode_decode_latin1: capi_unicode_decode_latin1,
        unicode_as_utf8_string: capi_unicode_as_utf8_string,
        unicode_as_ascii_string: capi_unicode_as_ascii_string,
        unicode_intern_from_string: capi_unicode_intern_from_string,
        unicode_compare: capi_unicode_compare,
        unicode_compare_with_ascii_string: capi_unicode_compare_with_ascii_string,
        unicode_concat: capi_unicode_concat,
        bytes_from_string_and_size: capi_bytes_from_string_and_size,
        bytes_from_string: capi_bytes_from_string,
        bytes_size: capi_bytes_size,
        bytes_as_string: capi_bytes_as_string,
        bytes_as_string_and_size: capi_bytes_as_string_and_size,
        bytes_concat: capi_bytes_concat,
        bytearray_from_string_and_size: capi_bytearray_from_string_and_size,
        bytearray_size: capi_bytearray_size,
        bytearray_as_string: capi_bytearray_as_string,
        unicode_check: capi_unicode_check,
        unicode_check_exact: capi_unicode_check_exact,
        bytes_check: capi_bytes_check,
        bytes_check_exact: capi_bytes_check_exact,
        bytearray_check: capi_bytearray_check,
        bytearray_check_exact: capi_bytearray_check_exact,
        unicode_from_utf8: capi_unicode_from_utf8,
        object_str: capi_object_str,
        object_repr: capi_object_repr,
        unicode_kind: capi_unicode_kind,
        unicode_data: capi_unicode_data,
        unicode_read_char: capi_unicode_read_char,
        unicode_is_ascii: capi_unicode_is_ascii,
        unicode_as_latin1_string: capi_unicode_as_latin1_string,
    }
}

unsafe extern "C" fn capi_unicode_from_string(value: *const c_char) -> *mut PyObject {
    let Some(text) = c_string(value) else {
        return abi::return_null_with_error("PyUnicode_FromString received invalid UTF-8");
    };
    unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

unsafe extern "C" fn capi_unicode_from_string_and_size(value: *const c_char, size: PySsizeT) -> *mut PyObject {
    unsafe { capi_unicode_from_utf8(value, size) }
}

unsafe extern "C" fn capi_unicode_from_utf8(value: *const c_char, size: PySsizeT) -> *mut PyObject {
    let bytes = match unsafe { raw_c_bytes(value, size, "unicode input") } {
        Ok(bytes) => bytes,
        Err(message) => return abi::return_null_with_error(message),
    };
    if core::str::from_utf8(bytes).is_err() {
        return raise_null(ExceptionKind::UnicodeDecodeError, "UnicodeDecodeError: 'utf-8' codec can't decode input");
    }
    unsafe { abi::pon_const_str(bytes.as_ptr(), bytes.len()) }
}

unsafe extern "C" fn capi_unicode_as_utf8(object: *mut PyObject) -> *const c_char {
    unsafe { capi_unicode_as_utf8_and_size(object, ptr::null_mut()) }
}

unsafe extern "C" fn capi_unicode_as_utf8_and_size(object: *mut PyObject, size: *mut PySsizeT) -> *const c_char {
    let Some(text) = (unsafe { crate::types::type_::unicode_text(object) }) else {
        raise_null::<PyObject>(ExceptionKind::TypeError, "bad argument type for PyUnicode_AsUTF8");
        return ptr::null();
    };
    if !size.is_null() {
        // SAFETY: `size` is an optional out-parameter supplied by the C caller.
        unsafe { *size = text.len() as PySsizeT };
    }
    cache_nul_terminated(&UTF8_CACHE, object, text.as_bytes()).cast::<c_char>()
}

unsafe extern "C" fn capi_unicode_get_length(object: *mut PyObject) -> PySsizeT {
    let Some(text) = (unsafe { crate::types::type_::unicode_text(object) }) else {
        raise_null::<PyObject>(ExceptionKind::TypeError, "bad argument type for PyUnicode_GetLength");
        return -1;
    };
    text.chars().count() as PySsizeT
}

unsafe extern "C" fn capi_unicode_decode_utf8(value: *const c_char, size: PySsizeT, errors: *const c_char) -> *mut PyObject {
    let bytes = match unsafe { raw_c_bytes(value, size, "UTF-8 input") } {
        Ok(bytes) => bytes,
        Err(message) => return abi::return_null_with_error(message),
    };
    match decode_error_handler(errors) {
        Ok(DecodeErrors::Strict) => match core::str::from_utf8(bytes) {
            Ok(text) => unsafe { abi::pon_const_str(text.as_ptr(), text.len()) },
            Err(_) => raise_null(ExceptionKind::UnicodeDecodeError, "UnicodeDecodeError: 'utf-8' codec can't decode input"),
        },
        Ok(DecodeErrors::Replace) => {
            let text = String::from_utf8_lossy(bytes);
            unsafe { abi::pon_const_str(text.as_bytes().as_ptr(), text.len()) }
        }
        Ok(DecodeErrors::Ignore) => {
            let text = utf8_decode_ignore(bytes);
            unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
        }
        Err(message) => abi::return_null_with_error(message),
    }
}

unsafe extern "C" fn capi_unicode_decode_ascii(value: *const c_char, size: PySsizeT, errors: *const c_char) -> *mut PyObject {
    let bytes = match unsafe { raw_c_bytes(value, size, "ASCII input") } {
        Ok(bytes) => bytes,
        Err(message) => return abi::return_null_with_error(message),
    };
    match decode_error_handler(errors) {
        Ok(DecodeErrors::Strict) => {
            if bytes.iter().any(|byte| !byte.is_ascii()) {
                return raise_null(ExceptionKind::UnicodeDecodeError, "UnicodeDecodeError: 'ascii' codec can't decode input");
            }
            unsafe { abi::pon_const_str(bytes.as_ptr(), bytes.len()) }
        }
        Ok(DecodeErrors::Replace) => {
            let mut text = String::with_capacity(bytes.len());
            for &byte in bytes {
                if byte.is_ascii() { text.push(byte as char) } else { text.push('\u{fffd}') }
            }
            unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
        }
        Ok(DecodeErrors::Ignore) => {
            let text: Vec<u8> = bytes.iter().copied().filter(u8::is_ascii).collect();
            unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
        }
        Err(message) => abi::return_null_with_error(message),
    }
}

unsafe extern "C" fn capi_unicode_decode_latin1(value: *const c_char, size: PySsizeT, errors: *const c_char) -> *mut PyObject {
    let bytes = match unsafe { raw_c_bytes(value, size, "Latin-1 input") } {
        Ok(bytes) => bytes,
        Err(message) => return abi::return_null_with_error(message),
    };
    if let Err(message) = decode_error_handler(errors) {
        return abi::return_null_with_error(message);
    }
    let mut text = String::with_capacity(bytes.len());
    for &byte in bytes {
        text.push(char::from(byte));
    }
    unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

unsafe extern "C" fn capi_unicode_as_utf8_string(object: *mut PyObject) -> *mut PyObject {
    let Some(text) = (unsafe { crate::types::type_::unicode_text(object) }) else {
        return raise_null(ExceptionKind::TypeError, "bad argument type for PyUnicode_AsUTF8String");
    };
    unsafe { abi::str_::pon_const_bytes(text.as_ptr(), text.len()) }
}

unsafe extern "C" fn capi_unicode_as_ascii_string(object: *mut PyObject) -> *mut PyObject {
    let Some(text) = (unsafe { crate::types::type_::unicode_text(object) }) else {
        return raise_null(ExceptionKind::TypeError, "bad argument type for PyUnicode_AsASCIIString");
    };
    if !text.is_ascii() {
        return raise_null(ExceptionKind::UnicodeEncodeError, "UnicodeEncodeError: 'ascii' codec can't encode character");
    }
    unsafe { abi::str_::pon_const_bytes(text.as_ptr(), text.len()) }
}

unsafe extern "C" fn capi_unicode_as_latin1_string(object: *mut PyObject) -> *mut PyObject {
    let Some(text) = (unsafe { crate::types::type_::unicode_text(object) }) else {
        return raise_null(ExceptionKind::TypeError, "bad argument type for PyUnicode_AsLatin1String");
    };
    let mut bytes = Vec::with_capacity(text.len());
    for ch in text.chars() {
        let code = u32::from(ch);
        if code > 0xFF {
            return raise_null(
                ExceptionKind::UnicodeEncodeError,
                "UnicodeEncodeError: 'latin-1' codec can't encode character",
            );
        }
        bytes.push(code as u8);
    }
    unsafe { abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) }
}
unsafe extern "C" fn capi_unicode_kind(object: *mut PyObject) -> c_int {
    let Some(text) = (unsafe { crate::types::type_::unicode_text(object) }) else {
        raise_null::<PyObject>(ExceptionKind::TypeError, "bad argument type for PyUnicode_KIND");
        return 0;
    };
    unicode_kind_for_text(text)
}

unsafe extern "C" fn capi_unicode_data(object: *mut PyObject) -> *const c_void {
    let Some(text) = (unsafe { crate::types::type_::unicode_text(object) }) else {
        raise_null::<PyObject>(ExceptionKind::TypeError, "bad argument type for PyUnicode_DATA");
        return ptr::null();
    };
    cache_unicode_data(object, text)
}

unsafe extern "C" fn capi_unicode_read_char(object: *mut PyObject, index: PySsizeT) -> u32 {
    let Some(text) = (unsafe { crate::types::type_::unicode_text(object) }) else {
        raise_null::<PyObject>(ExceptionKind::TypeError, "bad argument type for PyUnicode_READ_CHAR");
        return u32::MAX;
    };
    let Ok(index) = usize::try_from(index) else {
        raise_null::<PyObject>(ExceptionKind::IndexError, "PyUnicode_READ_CHAR index out of range");
        return u32::MAX;
    };
    match text.chars().nth(index) {
        Some(ch) => ch as u32,
        None => {
            raise_null::<PyObject>(ExceptionKind::IndexError, "PyUnicode_READ_CHAR index out of range");
            u32::MAX
        }
    }
}

unsafe extern "C" fn capi_unicode_is_ascii(object: *mut PyObject) -> c_int {
    let Some(text) = (unsafe { crate::types::type_::unicode_text(object) }) else {
        raise_null::<PyObject>(ExceptionKind::TypeError, "bad argument type for PyUnicode_IS_ASCII");
        return 0;
    };
    c_int::from(text.is_ascii())
}


unsafe extern "C" fn capi_unicode_intern_from_string(value: *const c_char) -> *mut PyObject {
    let Some(text) = c_string(value) else {
        return abi::return_null_with_error("PyUnicode_InternFromString received invalid UTF-8");
    };
    let _ = crate::intern::intern(&text);
    {
        let interned = INTERNED_UNICODE.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(&object) = interned.get(&text) {
            return object as *mut PyObject;
        }
    }
    let object = unsafe { abi::pon_const_str(text.as_ptr(), text.len()) };
    if object.is_null() {
        return object;
    }
    // SAFETY: Pinning gives the interned C-API object process-lifetime reachability.
    unsafe { super::py_inc_ref(object) };
    let mut interned = INTERNED_UNICODE.lock().unwrap_or_else(|poison| poison.into_inner());
    *interned.entry(text).or_insert(object as usize) as *mut PyObject
}

unsafe extern "C" fn capi_unicode_compare(left: *mut PyObject, right: *mut PyObject) -> c_int {
    let Some(left_text) = (unsafe { crate::types::type_::unicode_text(left) }) else {
        raise_null::<PyObject>(ExceptionKind::TypeError, "first argument must be str");
        return -1;
    };
    let Some(right_text) = (unsafe { crate::types::type_::unicode_text(right) }) else {
        raise_null::<PyObject>(ExceptionKind::TypeError, "second argument must be str");
        return -1;
    };
    ordering_to_c_int(left_text.cmp(right_text))
}

unsafe extern "C" fn capi_unicode_compare_with_ascii_string(left: *mut PyObject, right: *const c_char) -> c_int {
    let Some(left_text) = (unsafe { crate::types::type_::unicode_text(left) }) else {
        raise_null::<PyObject>(ExceptionKind::TypeError, "first argument must be str");
        return -1;
    };
    let Some(right_text) = c_string(right) else {
        raise_null::<PyObject>(ExceptionKind::UnicodeDecodeError, "ASCII comparison string is invalid");
        return -1;
    };
    if !right_text.is_ascii() {
        raise_null::<PyObject>(ExceptionKind::UnicodeDecodeError, "ASCII comparison string contains non-ASCII data");
        return -1;
    }
    ordering_to_c_int(left_text.cmp(right_text.as_str()))
}

unsafe extern "C" fn capi_unicode_concat(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    let Some(left_text) = (unsafe { crate::types::type_::unicode_text(left) }) else {
        return raise_null(ExceptionKind::TypeError, "first argument must be str");
    };
    let Some(right_text) = (unsafe { crate::types::type_::unicode_text(right) }) else {
        return raise_null(ExceptionKind::TypeError, "second argument must be str");
    };
    let mut out = String::with_capacity(left_text.len() + right_text.len());
    out.push_str(left_text);
    out.push_str(right_text);
    unsafe { abi::pon_const_str(out.as_ptr(), out.len()) }
}

unsafe extern "C" fn capi_bytes_from_string_and_size(value: *const c_char, size: PySsizeT) -> *mut PyObject {
    let bytes = match unsafe { raw_or_zeroed_c_bytes(value, size, "bytes input") } {
        Ok(bytes) => bytes,
        Err(message) => return abi::return_null_with_error(message),
    };
    unsafe { abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) }
}

unsafe extern "C" fn capi_bytes_from_string(value: *const c_char) -> *mut PyObject {
    if value.is_null() {
        return abi::return_null_with_error("PyBytes_FromString received NULL");
    }
    // SAFETY: `value` is a non-NULL NUL-terminated C string by API contract.
    let bytes = unsafe { CStr::from_ptr(value) }.to_bytes();
    unsafe { abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) }
}

unsafe extern "C" fn capi_bytes_size(object: *mut PyObject) -> PySsizeT {
    match unsafe { bytes_payload_slice(object) } {
        Some(bytes) => bytes.len() as PySsizeT,
        None => {
            raise_null::<PyObject>(ExceptionKind::TypeError, "expected bytes object");
            -1
        }
    }
}

unsafe extern "C" fn capi_bytes_as_string(object: *mut PyObject) -> *mut c_char {
    let Some(bytes) = (unsafe { bytes_payload_slice(object) }) else {
        raise_null::<PyObject>(ExceptionKind::TypeError, "expected bytes object");
        return ptr::null_mut();
    };
    cache_nul_terminated(&BYTES_CACHE, object, bytes).cast::<c_char>().cast_mut()
}

unsafe extern "C" fn capi_bytes_as_string_and_size(
    object: *mut PyObject,
    buffer: *mut *mut c_char,
    size: *mut PySsizeT,
) -> c_int {
    if buffer.is_null() {
        return status_error("PyBytes_AsStringAndSize received NULL buffer out-parameter");
    }
    let Some(bytes) = (unsafe { bytes_payload_slice(object) }) else {
        return status_type_error("expected bytes object");
    };
    if size.is_null() && bytes.contains(&0) {
        return status_value_error("embedded null byte");
    }
    let pointer = unsafe { capi_bytes_as_string(object) };
    if pointer.is_null() {
        return -1;
    }
    // SAFETY: `buffer`/`size` are optional C out-parameters validated above.
    unsafe {
        *buffer = pointer;
        if !size.is_null() {
            *size = bytes.len() as PySsizeT;
        }
    }
    0
}

unsafe extern "C" fn capi_bytes_concat(slot: *mut *mut PyObject, newpart: *mut PyObject) {
    if slot.is_null() {
        let _ = status_error("PyBytes_Concat received NULL slot");
        return;
    }
    // SAFETY: `slot` is non-NULL by the check above.
    let left = unsafe { *slot };
    let Some(left_bytes) = (unsafe { bytes_payload_slice(left) }) else {
        let _ = status_type_error("expected bytes object");
        unsafe { *slot = ptr::null_mut() };
        return;
    };
    let Some(right_bytes) = (unsafe { bytes_payload_slice(newpart) }) else {
        let _ = status_type_error("expected bytes object");
        unsafe { *slot = ptr::null_mut() };
        return;
    };
    let mut out = Vec::with_capacity(left_bytes.len() + right_bytes.len());
    out.extend_from_slice(left_bytes);
    out.extend_from_slice(right_bytes);
    let result = unsafe { abi::str_::pon_const_bytes(out.as_ptr(), out.len()) };
    // SAFETY: `slot` is non-NULL by the check above.
    unsafe { *slot = result };
}

unsafe extern "C" fn capi_bytearray_from_string_and_size(value: *const c_char, size: PySsizeT) -> *mut PyObject {
    let bytes = match unsafe { raw_or_zeroed_c_bytes(value, size, "bytearray input") } {
        Ok(bytes) => bytes,
        Err(message) => return abi::return_null_with_error(message),
    };
    unsafe { abi::str_::pon_const_bytearray(bytes.as_ptr(), bytes.len()) }
}

unsafe extern "C" fn capi_bytearray_size(object: *mut PyObject) -> PySsizeT {
    match unsafe { bytearray_ref(object) } {
        Some(bytearray) => bytearray.as_slice().len() as PySsizeT,
        None => {
            raise_null::<PyObject>(ExceptionKind::TypeError, "expected bytearray object");
            -1
        }
    }
}

unsafe extern "C" fn capi_bytearray_as_string(object: *mut PyObject) -> *mut c_char {
    let Some(bytearray) = (unsafe { bytearray_mut(object) }) else {
        raise_null::<PyObject>(ExceptionKind::TypeError, "expected bytearray object");
        return ptr::null_mut();
    };
    bytearray.as_mut_slice().as_mut_ptr().cast::<c_char>()
}

unsafe extern "C" fn capi_unicode_check(object: *mut PyObject) -> c_int {
    c_int::from(unsafe { is_builtin_or_subclass(object, super::twin::TID_UNICODE, "str") })
}

unsafe extern "C" fn capi_unicode_check_exact(object: *mut PyObject) -> c_int {
    c_int::from(unsafe { is_exact_builtin(object, super::twin::TID_UNICODE) })
}

unsafe extern "C" fn capi_bytes_check(object: *mut PyObject) -> c_int {
    c_int::from(unsafe { is_builtin_or_subclass(object, super::twin::TID_BYTES, "bytes") })
}

unsafe extern "C" fn capi_bytes_check_exact(object: *mut PyObject) -> c_int {
    c_int::from(unsafe { is_exact_builtin(object, super::twin::TID_BYTES) })
}

unsafe extern "C" fn capi_bytearray_check(object: *mut PyObject) -> c_int {
    c_int::from(unsafe { is_builtin_or_subclass(object, super::twin::TID_BYTEARRAY, "bytearray") })
}

unsafe extern "C" fn capi_bytearray_check_exact(object: *mut PyObject) -> c_int {
    c_int::from(unsafe { is_exact_builtin(object, super::twin::TID_BYTEARRAY) })
}

unsafe extern "C" fn capi_object_str(object: *mut PyObject) -> *mut PyObject {
    match crate::native::builtins_mod::try_str_text(object) {
        Ok(text) => unsafe { abi::pon_const_str(text.as_ptr(), text.len()) },
        Err(()) => ptr::null_mut(),
    }
}

unsafe extern "C" fn capi_object_repr(object: *mut PyObject) -> *mut PyObject {
    match crate::native::builtins_mod::try_repr_text(object) {
        Ok(text) => unsafe { abi::pon_const_str(text.as_ptr(), text.len()) },
        Err(()) => ptr::null_mut(),
    }
}

unsafe fn raw_c_bytes<'a>(value: *const c_char, size: PySsizeT, label: &str) -> Result<&'a [u8], String> {
    if size < 0 {
        return Err(format!("{label} length is negative"));
    }
    let len = size as usize;
    if value.is_null() {
        return if len == 0 { Ok(&[]) } else { Err(format!("{label} pointer is NULL")) };
    }
    // SAFETY: The C API caller promises `len` readable bytes at `value`.
    Ok(unsafe { core::slice::from_raw_parts(value.cast::<u8>(), len) })
}

unsafe fn raw_or_zeroed_c_bytes(value: *const c_char, size: PySsizeT, label: &str) -> Result<Vec<u8>, String> {
    if size < 0 {
        return Err(format!("{label} length is negative"));
    }
    let len = size as usize;
    if value.is_null() {
        return Ok(vec![0; len]);
    }
    // SAFETY: Delegates to the raw slice validator above.
    Ok(unsafe { raw_c_bytes(value, size, label) }?.to_vec())
}

fn cache_nul_terminated(cache: &LazyLock<Mutex<HashMap<usize, Box<[u8]>>>>, object: *mut PyObject, bytes: &[u8]) -> *const u8 {
    let key = object as usize;
    let mut cache = cache.lock().unwrap_or_else(|poison| poison.into_inner());
    let entry = cache.entry(key).or_insert_with(|| {
        // SAFETY: The key is a live object address that the caller just inspected.
        unsafe { super::py_inc_ref(object) };
        let mut out = Vec::with_capacity(bytes.len() + 1);
        out.extend_from_slice(bytes);
        out.push(0);
        out.into_boxed_slice()
    });
    entry.as_ptr()
}
fn unicode_kind_for_text(text: &str) -> c_int {
    let mut kind = 1;
    for ch in text.chars() {
        let code = ch as u32;
        if code > 0xffff {
            return 4;
        }
        if code > 0xff {
            kind = 2;
        }
    }
    kind
}

fn unicode_data_for_text(text: &str) -> UnicodeDataCache {
    match unicode_kind_for_text(text) {
        1 => {
            let mut out = Vec::with_capacity(text.chars().count() + 1);
            for ch in text.chars() {
                out.push(ch as u8);
            }
            out.push(0);
            UnicodeDataCache::Ucs1(out.into_boxed_slice())
        }
        2 => {
            let mut out = Vec::with_capacity(text.chars().count() + 1);
            for ch in text.chars() {
                out.push(ch as u16);
            }
            out.push(0);
            UnicodeDataCache::Ucs2(out.into_boxed_slice())
        }
        _ => {
            let mut out = Vec::with_capacity(text.chars().count() + 1);
            for ch in text.chars() {
                out.push(ch as u32);
            }
            out.push(0);
            UnicodeDataCache::Ucs4(out.into_boxed_slice())
        }
    }
}

fn cache_unicode_data(object: *mut PyObject, text: &str) -> *const c_void {
    let key = object as usize;
    let mut cache = UNICODE_DATA_CACHE.lock().unwrap_or_else(|poison| poison.into_inner());
    let entry = cache.entry(key).or_insert_with(|| {
        // SAFETY: The key is a live unicode object address that the caller just inspected.
        unsafe { super::py_inc_ref(object) };
        unicode_data_for_text(text)
    });
    entry.as_ptr()
}


#[derive(Clone, Copy)]
enum DecodeErrors {
    Strict,
    Ignore,
    Replace,
}

fn decode_error_handler(errors: *const c_char) -> Result<DecodeErrors, String> {
    if errors.is_null() {
        return Ok(DecodeErrors::Strict);
    }
    let Some(errors) = c_string(errors) else {
        return Err("decode errors handler is not valid UTF-8".to_owned());
    };
    match errors.as_str() {
        "strict" => Ok(DecodeErrors::Strict),
        "ignore" => Ok(DecodeErrors::Ignore),
        "replace" => Ok(DecodeErrors::Replace),
        _ => Err(format!("unsupported decode errors handler '{errors}'")),
    }
}

fn utf8_decode_ignore(mut bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    while !bytes.is_empty() {
        match core::str::from_utf8(bytes) {
            Ok(valid) => {
                out.push_str(valid);
                break;
            }
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                // SAFETY: `valid_up_to` is guaranteed to split at valid UTF-8.
                out.push_str(unsafe { core::str::from_utf8_unchecked(&bytes[..valid_up_to]) });
                let skip = error.error_len().unwrap_or(1);
                bytes = &bytes[(valid_up_to + skip).min(bytes.len())..];
            }
        }
    }
    out
}

unsafe fn bytes_payload_slice<'a>(object: *mut PyObject) -> Option<&'a [u8]> {
    let object = unsafe { crate::types::type_::payload_subclass_value(object) }.unwrap_or(object);
    if object.is_null() || !crate::tag::is_heap(object) {
        return None;
    }
    // SAFETY: `object` is heap-tagged and can be read as a PyObject header.
    let ty = unsafe { (*object).ob_type };
    if !crate::types::bytes_::is_bytes_type(ty) {
        return None;
    }
    // SAFETY: The type check above proves the concrete bytes layout.
    Some(unsafe { (&*object.cast::<crate::types::bytes_::PyBytes>()).as_slice() })
}

unsafe fn bytearray_ref<'a>(object: *mut PyObject) -> Option<&'a crate::types::bytearray_::PyByteArray> {
    if object.is_null() || !crate::tag::is_heap(object) {
        return None;
    }
    // SAFETY: `object` is heap-tagged and can be read as a PyObject header.
    let ty = unsafe { (*object).ob_type };
    if !crate::types::bytearray_::is_bytearray_type(ty) {
        return None;
    }
    // SAFETY: The type check above proves the concrete bytearray layout.
    Some(unsafe { &*object.cast::<crate::types::bytearray_::PyByteArray>() })
}

unsafe fn bytearray_mut<'a>(object: *mut PyObject) -> Option<&'a mut crate::types::bytearray_::PyByteArray> {
    if object.is_null() || !crate::tag::is_heap(object) {
        return None;
    }
    // SAFETY: `object` is heap-tagged and can be read as a PyObject header.
    let ty = unsafe { (*object).ob_type };
    if !crate::types::bytearray_::is_bytearray_type(ty) {
        return None;
    }
    // SAFETY: The type check above proves the concrete bytearray layout and the C API hands out a mutable buffer.
    Some(unsafe { &mut *object.cast::<crate::types::bytearray_::PyByteArray>() })
}

unsafe fn is_exact_builtin(object: *mut PyObject, tid: usize) -> bool {
    if object.is_null() {
        return false;
    }
    // SAFETY: `builtin_type_id` performs the tagged-pointer heap check before dereferencing.
    unsafe { super::twin::capi_builtin_type_id(object) == tid as c_int }
}

unsafe fn is_builtin_or_subclass(object: *mut PyObject, tid: usize, type_name: &str) -> bool {
    if unsafe { is_exact_builtin(object, tid) } {
        return true;
    }
    if object.is_null() || !crate::tag::is_heap(object) {
        return false;
    }
    // SAFETY: `object` is heap-tagged and can be read as a PyObject header.
    let ty = unsafe { (*object).ob_type.cast_mut() };
    unsafe { crate::mro::mro_entries(ty) }
        .into_iter()
        .any(|entry| !entry.is_null() && unsafe { (*entry).name() == type_name })
}

fn ordering_to_c_int(ordering: core::cmp::Ordering) -> c_int {
    match ordering {
        core::cmp::Ordering::Less => -1,
        core::cmp::Ordering::Equal => 0,
        core::cmp::Ordering::Greater => 1,
    }
}

fn raise_null<T>(kind: ExceptionKind, message: &str) -> *mut T {
    abi::exc::raise_kind_error_text(kind, message).cast::<T>()
}

fn status_error(message: &str) -> c_int {
    abi::return_minus_one_with_error(message)
}

fn status_type_error(message: &str) -> c_int {
    let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message);
    -1
}

fn status_value_error(message: &str) -> c_int {
    let _ = abi::exc::raise_kind_error_text(ExceptionKind::ValueError, message);
    -1
}

#[cfg(test)]
mod tests {
    use core::ptr;

    use super::super::load_extension_module;
    use super::super::tests::{compile_extension, ResetImportStateOnDrop, TempExtensionRoot};
    use crate::abi::{format_object_for_print, pon_call, pon_runtime_init};
    use crate::import::module_attr;
    use crate::intern::intern;
    use crate::thread_state::{pon_err_message, test_state_lock};

    #[test]
    fn c_extension_exercises_unicode_bytes_and_bytearray_api() {
        let _guard = test_state_lock();
        let _reset = ResetImportStateOnDrop;
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
        }

        let temp = TempExtensionRoot::new();
        let module_path = compile_extension(
            &temp,
            "capi_strings_ext",
            r#"
#include <Python.h>

static PyObject *fail(const char *message) {
    PyErr_SetString(PyExc_RuntimeError, message);
    return NULL;
}

static int check_text(PyObject *object, const char *expected, Py_ssize_t expected_size) {
    Py_ssize_t size = 0;
    const char *text = PyUnicode_AsUTF8AndSize(object, &size);
    if (text == NULL) {
        return -1;
    }
    if (size != expected_size || memcmp(text, expected, (size_t)expected_size) != 0) {
        PyErr_SetString(PyExc_RuntimeError, "unicode text mismatch");
        return -1;
    }
    return 0;
}

static int check_bytes(PyObject *object, const char *expected, Py_ssize_t expected_size) {
    char *buffer = NULL;
    Py_ssize_t size = 0;
    if (PyBytes_AsStringAndSize(object, &buffer, &size) < 0) {
        return -1;
    }
    if (size != expected_size || memcmp(buffer, expected, (size_t)expected_size) != 0) {
        PyErr_SetString(PyExc_RuntimeError, "bytes payload mismatch");
        return -1;
    }
    return 0;
}

static PyObject *exercise(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;

    const char utf8[] = "Pon " "\xF0\x9F\x98\x80";
    PyObject *unicode = PyUnicode_FromStringAndSize(utf8, (Py_ssize_t)(sizeof(utf8) - 1));
    if (unicode == NULL) {
        return NULL;
    }
    if (!PyUnicode_Check(unicode) || !PyUnicode_CheckExact(unicode) || PyBytes_Check(unicode)) {
        return fail("unicode checks failed");
    }

    Py_ssize_t unicode_size = 0;
    const char *utf8_first = PyUnicode_AsUTF8AndSize(unicode, &unicode_size);
    const char *utf8_second = PyUnicode_AsUTF8(unicode);
    if (utf8_first == NULL || utf8_second == NULL) {
        return NULL;
    }
    if (utf8_first != utf8_second || unicode_size != (Py_ssize_t)(sizeof(utf8) - 1) ||
        memcmp(utf8_first, utf8, (size_t)unicode_size) != 0) {
        return fail("unicode UTF-8 cache mismatch");
    }

    PyObject *decoded = PyUnicode_DecodeUTF8(utf8_first, unicode_size, "strict");
    if (decoded == NULL) {
        return NULL;
    }
    if (PyUnicode_Compare(unicode, decoded) != 0) {
        return fail("unicode UTF-8 round-trip mismatch");
    }

    PyObject *ascii = PyUnicode_DecodeASCII("plain", 5, NULL);
    if (ascii == NULL || check_text(ascii, "plain", 5) < 0) {
        return NULL;
    }
    const char latin1[] = "\xE9";
    PyObject *latin = PyUnicode_DecodeLatin1(latin1, 1, NULL);
    if (latin == NULL || check_text(latin, "\xC3\xA9", 2) < 0) {
        return NULL;
    }

    const char astral[] = "a" "\xF0\x9D\x84\x9E";
    PyObject *astral_text = PyUnicode_FromStringAndSize(astral, (Py_ssize_t)(sizeof(astral) - 1));
    if (astral_text == NULL) {
        return NULL;
    }
    if (PyUnicode_GetLength(astral_text) != 2) {
        return fail("astral-plane length mismatch");
    }

    PyObject *utf8_bytes = PyUnicode_AsUTF8String(unicode);
    if (utf8_bytes == NULL || check_bytes(utf8_bytes, utf8, (Py_ssize_t)(sizeof(utf8) - 1)) < 0) {
        return NULL;
    }
    PyObject *ascii_bytes = PyUnicode_AsASCIIString(ascii);
    if (ascii_bytes == NULL || check_bytes(ascii_bytes, "plain", 5) < 0) {
        return NULL;
    }

    PyObject *suffix = PyUnicode_FromString("!");
    PyObject *concatenated = PyUnicode_Concat(ascii, suffix);
    if (concatenated == NULL || check_text(concatenated, "plain!", 6) < 0) {
        return NULL;
    }
    if (PyUnicode_CompareWithASCIIString(ascii, "plain") != 0) {
        return fail("ASCII comparison mismatch");
    }
    if (PyUnicode_InternFromString("interned") != PyUnicode_InternFromString("interned")) {
        return fail("interned unicode pointer changed");
    }

    const char raw_bytes[] = {'a', '\0', 'b'};
    PyObject *bytes = PyBytes_FromStringAndSize(raw_bytes, 3);
    if (bytes == NULL) {
        return NULL;
    }
    if (!PyBytes_Check(bytes) || !PyBytes_CheckExact(bytes) || PyUnicode_Check(bytes)) {
        return fail("bytes checks failed");
    }
    if (check_bytes(bytes, raw_bytes, 3) < 0) {
        return NULL;
    }
    char *bytes_first = PyBytes_AsString(bytes);
    char *bytes_second = PyBytes_AsString(bytes);
    if (bytes_first == NULL || bytes_second == NULL || bytes_first != bytes_second) {
        return fail("bytes buffer cache mismatch");
    }

    PyObject *zeroed = PyBytes_FromStringAndSize(NULL, 3);
    const char zeros[] = {'\0', '\0', '\0'};
    if (zeroed == NULL || check_bytes(zeroed, zeros, 3) < 0) {
        return NULL;
    }
    PyObject *bytes_from_cstr = PyBytes_FromString("hi");
    if (bytes_from_cstr == NULL || PyBytes_Size(bytes_from_cstr) != 2) {
        return fail("PyBytes_FromString/PyBytes_Size failed");
    }
    PyObject *bytes_concat = PyBytes_FromString("x");
    PyObject *bytes_tail = PyBytes_FromString("y");
    if (bytes_concat == NULL || bytes_tail == NULL) {
        return NULL;
    }
    PyBytes_Concat(&bytes_concat, bytes_tail);
    if (bytes_concat == NULL || check_bytes(bytes_concat, "xy", 2) < 0) {
        return NULL;
    }

    PyObject *bytearray = PyByteArray_FromStringAndSize("zz", 2);
    if (bytearray == NULL) {
        return NULL;
    }
    if (!PyByteArray_Check(bytearray) || !PyByteArray_CheckExact(bytearray) || PyBytes_Check(bytearray)) {
        return fail("bytearray checks failed");
    }
    if (PyByteArray_Size(bytearray) != 2) {
        return fail("PyByteArray_Size failed");
    }
    char *bytearray_buffer = PyByteArray_AsString(bytearray);
    if (bytearray_buffer == NULL || memcmp(bytearray_buffer, "zz", 2) != 0) {
        return fail("PyByteArray_AsString failed");
    }

    PyObject *formatted = PyUnicode_FromFormat("fmt:%s:%d:%zd", "ok", -7, (Py_ssize_t)12345);
    if (formatted == NULL || check_text(formatted, "fmt:ok:-7:12345", 15) < 0) {
        return NULL;
    }

    return PyUnicode_FromString("ok");
}

static PyMethodDef methods[] = {
    {"exercise", exercise, METH_NOARGS, "exercise strings C API"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_strings_ext",
    "Pon strings C-API test extension",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_strings_ext(void) {
    return PyModule_Create(&module);
}
"#,
        );

        let module = load_extension_module("capi_strings_ext", &module_path)
            .unwrap_or_else(|message| panic!("failed to load C extension: {message}"));
        assert!(!module.is_null(), "extension loader returned NULL module");

        let exercise = module_attr(intern("capi_strings_ext"), intern("exercise")).expect("exercise method registered");
        let result = unsafe { pon_call(exercise, ptr::null_mut(), 0) };
        assert!(
            !result.is_null(),
            "exercise() returned NULL: {:?}",
            pon_err_message()
        );
        assert_eq!(format_object_for_print(result).as_deref(), Ok("ok"));
    }

    #[test]
    fn c_extension_exercises_bytes_format_resize_ascii_and_unicode_views() {
        let _guard = test_state_lock();
        let _reset = ResetImportStateOnDrop;
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
        }

        let temp = TempExtensionRoot::new();
        let module_path = compile_extension(
            &temp,
            "capi_strings_gap_ext",
            r#"
#include <Python.h>

static int bytes_equal(PyObject *object, const char *expected, Py_ssize_t expected_size) {
    char *buffer = NULL;
    Py_ssize_t size = 0;
    if (object == NULL || PyBytes_AsStringAndSize(object, &buffer, &size) < 0) {
        return 0;
    }
    return size == expected_size && memcmp(buffer, expected, (size_t)expected_size) == 0;
}

static PyObject *format_v_helper(const char *format, ...) {
    va_list vargs;
    va_start(vargs, format);
    PyObject *result = PyBytes_FromFormatV(format, vargs);
    va_end(vargs);
    return result;
}

static PyObject *probe(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    long ok = 0;

    PyObject *formatted = PyBytes_FromFormat("%s-%zd-%ld", "pon", (Py_ssize_t)5, (long)42);
    if (bytes_equal(formatted, "pon-5-42", 8)) {
        ok |= 1L << 0;
    }

    PyObject *formatted_v = format_v_helper("%s-%zd-%ld", "pon", (Py_ssize_t)7, (long)99);
    if (bytes_equal(formatted_v, "pon-7-99", 8)) {
        ok |= 1L << 1;
    }

    PyObject *formatted_misc = PyBytes_FromFormat("%c:%d:%i:%u:%x:%%", 'A', -3, 4, (unsigned int)5, (unsigned int)0x2a);
    if (bytes_equal(formatted_misc, "A:-3:4:5:2a:%", 13)) {
        ok |= 1L << 2;
    }

    PyObject *formatted_unsigned = PyBytes_FromFormat("%lu-%zu", (unsigned long)123, (size_t)456);
    if (bytes_equal(formatted_unsigned, "123-456", 7)) {
        ok |= 1L << 3;
    }

    PyObject *resized = PyBytes_FromStringAndSize("abcdef", 6);
    if (resized != NULL && _PyBytes_Resize(&resized, 9) == 0) {
        char *buffer = NULL;
        Py_ssize_t size = 0;
        if (PyBytes_AsStringAndSize(resized, &buffer, &size) == 0 &&
                size == 9 && memcmp(buffer, "abcdef", 6) == 0) {
            ok |= 1L << 4;
        }
    }
    if (resized != NULL && _PyBytes_Resize(&resized, 3) == 0 && bytes_equal(resized, "abc", 3)) {
        ok |= 1L << 5;
    }

    PyObject *not_bytes = PyUnicode_FromString("not bytes");
    PyObject *slot = not_bytes;
    if (_PyBytes_Resize(&slot, 2) < 0 && PyErr_ExceptionMatches(PyExc_SystemError)) {
        ok |= 1L << 6;
    }
    PyErr_Clear();
    if (_PyBytes_Resize(NULL, 2) < 0 && PyErr_ExceptionMatches(PyExc_SystemError)) {
        ok |= 1L << 7;
    }
    PyErr_Clear();

    PyObject *ascii = PyUnicode_FromString("abc");
    PyObject *ascii_bytes = PyUnicode_AsASCIIString(ascii);
    if (bytes_equal(ascii_bytes, "abc", 3)) {
        ok |= 1L << 8;
    }

    const char latin1_bytes[] = "\xE9";
    PyObject *latin1 = PyUnicode_DecodeLatin1(latin1_bytes, 1, NULL);
    PyObject *latin1_ascii = PyUnicode_AsASCIIString(latin1);
    if (latin1_ascii == NULL && PyErr_ExceptionMatches(PyExc_ValueError)) {
        ok |= 1L << 9;
    }
    PyErr_Clear();

    if (ascii != NULL && PyUnicode_IS_ASCII(ascii) && PyUnicode_KIND(ascii) == PyUnicode_1BYTE_KIND &&
            PyUnicode_GET_LENGTH(ascii) == 3 && memcmp(PyUnicode_DATA(ascii), "abc", 3) == 0) {
        ok |= 1L << 10;
    }
    if (latin1 != NULL && !PyUnicode_IS_ASCII(latin1) && PyUnicode_KIND(latin1) == PyUnicode_1BYTE_KIND &&
            PyUnicode_GET_LENGTH(latin1) == 1 && PyUnicode_1BYTE_DATA(latin1)[0] == 0xE9) {
        ok |= 1L << 11;
    }

    const char euro_utf8[] = "\xE2\x82\xAC";
    PyObject *euro = PyUnicode_FromStringAndSize(euro_utf8, 3);
    if (euro != NULL && PyUnicode_KIND(euro) == PyUnicode_2BYTE_KIND &&
            PyUnicode_GET_LENGTH(euro) == 1 && PyUnicode_2BYTE_DATA(euro)[0] == 0x20AC) {
        ok |= 1L << 12;
    }

    const char astral_utf8[] = "a" "\xF0\x9D\x84\x9E";
    PyObject *astral = PyUnicode_FromStringAndSize(astral_utf8, (Py_ssize_t)(sizeof(astral_utf8) - 1));
    if (astral != NULL && PyUnicode_KIND(astral) == PyUnicode_4BYTE_KIND &&
            PyUnicode_GET_LENGTH(astral) == 2 && PyUnicode_4BYTE_DATA(astral)[1] == 0x1D11E &&
            PyUnicode_READ_CHAR(astral, 1) == 0x1D11E) {
        ok |= 1L << 13;
    }

    if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }
    return PyLong_FromLong(ok);
}

static PyMethodDef methods[] = {
    {"probe", probe, METH_NOARGS, "probe bytes/unicode C API gaps"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_strings_gap_ext",
    "Pon strings C-API gap test extension",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_strings_gap_ext(void) {
    return PyModule_Create(&module);
}
"#,
        );

        let module = load_extension_module("capi_strings_gap_ext", &module_path)
            .unwrap_or_else(|message| panic!("failed to load C extension: {message}"));
        assert!(!module.is_null(), "extension loader returned NULL module");

        let probe = module_attr(intern("capi_strings_gap_ext"), intern("probe")).expect("probe method registered");
        let result = unsafe { pon_call(probe, ptr::null_mut(), 0) };
        assert!(
            !result.is_null(),
            "probe() returned NULL: {:?}",
            pon_err_message()
        );
        assert_eq!(format_object_for_print(result).as_deref(), Ok("16383"));
    }
}
