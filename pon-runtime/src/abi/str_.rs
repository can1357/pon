//! String, bytes, f-string, and template-string helper family namespace.
//!
//! Shared raw part layouts live in [`crate::abi::FStrPartRaw`] and
//! [`crate::abi::TStrPartRaw`].  These helpers follow the runtime-wide NULL
//! sentinel contract: success returns a boxed Python object, failure records the
//! thread-state error and returns NULL.

use core::mem;
use core::ptr;
use std::sync::{LazyLock, OnceLock};

use crate::object::{PyLong, PyMappingMethods, PyObject, PyObjectHeader, PySequenceMethods, PyType, PyUnicode, as_object_ptr, is_exact_type};
use crate::types::{bytearray_ as bytearray_type, bytes_ as bytes_type, method, slice_::PySlice, str_ as str_type, type_};

/// String method selector passed through the helper ABI.
pub type StrMethodId = u16;

pub const STR_METHOD_SPLIT: StrMethodId = 1;
pub const STR_METHOD_JOIN: StrMethodId = 2;
pub const STR_METHOD_REPLACE: StrMethodId = 3;
pub const STR_METHOD_FIND: StrMethodId = 4;
pub const STR_METHOD_STARTSWITH: StrMethodId = 5;
pub const STR_METHOD_ENCODE: StrMethodId = 6;
pub const STR_METHOD_STRIP: StrMethodId = 7;
pub const STR_METHOD_LOWER: StrMethodId = 8;
pub const STR_METHOD_UPPER: StrMethodId = 9;
pub const STR_METHOD_ENDSWITH: StrMethodId = 10;
pub const STR_METHOD_TITLE: StrMethodId = 11;

/// Bytes/bytearray method selector passed through the helper ABI.
pub type BytesMethodId = u16;

pub const BYTES_METHOD_SPLIT: BytesMethodId = 1;
pub const BYTES_METHOD_JOIN: BytesMethodId = 2;
pub const BYTES_METHOD_REPLACE: BytesMethodId = 3;
pub const BYTES_METHOD_FIND: BytesMethodId = 4;
pub const BYTES_METHOD_STARTSWITH: BytesMethodId = 5;
pub const BYTES_METHOD_DECODE: BytesMethodId = 6;

const TEMPLATE_LITERAL_CONVERSION: u8 = u8::MAX;

#[repr(C)]
struct PyTemplate {
    ob_base: PyObjectHeader,
    strings: *mut PyObject,
    interpolations: *mut PyObject,
}

#[repr(C)]
struct PyInterpolation {
    ob_base: PyObjectHeader,
    value: *mut PyObject,
    expression: *mut PyObject,
    conversion: *mut PyObject,
    format_spec: *mut PyObject,
}

static TEMPLATE_TYPE: OnceLock<usize> = OnceLock::new();
static INTERPOLATION_TYPE: OnceLock<usize> = OnceLock::new();

fn runtime_type_type() -> *mut PyType {
    super::with_runtime(|runtime| runtime._type_type).unwrap_or(ptr::null_mut())
}

fn template_type() -> *mut PyType {
    *TEMPLATE_TYPE.get_or_init(|| {
        let mut ty = Box::new(PyType::new(runtime_type_type(), "Template", mem::size_of::<PyTemplate>()));
        ty.tp_getattro = Some(template_getattro);
        Box::into_raw(ty) as usize
    }) as *mut PyType
}

fn interpolation_type() -> *mut PyType {
    *INTERPOLATION_TYPE.get_or_init(|| {
        let mut ty = Box::new(PyType::new(runtime_type_type(), "Interpolation", mem::size_of::<PyInterpolation>()));
        ty.tp_getattro = Some(interpolation_getattro);
        Box::into_raw(ty) as usize
    }) as *mut PyType
}

unsafe extern "C" fn template_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        return super::return_null_with_error("template attribute name must be str");
    };
    let template = unsafe { &*object.cast::<PyTemplate>() };
    match name {
        "strings" => template.strings,
        "interpolations" => template.interpolations,
        _ => super::return_null_with_error(format!("attribute '{name}' was not found")),
    }
}

unsafe extern "C" fn interpolation_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        return super::return_null_with_error("interpolation attribute name must be str");
    };
    let interpolation = unsafe { &*object.cast::<PyInterpolation>() };
    match name {
        "value" => interpolation.value,
        "expression" => interpolation.expression,
        "conversion" => interpolation.conversion,
        "format_spec" => interpolation.format_spec,
        _ => super::return_null_with_error(format!("attribute '{name}' was not found")),
    }
}

/// Sequence protocol table for `str`, exposing `+` through `sq_concat`.
///
/// `abstract_op::binary_op` falls back to `sq_concat` when a type has no numeric
/// `nb_add` slot, so wiring this table makes `"a" + "b"` reach [`pon_str_concat`]
/// with CPython's sequence-concatenation semantics. The pointer is stored as a
/// `usize` so the static satisfies `Sync`.
static STR_SEQUENCE_METHODS: LazyLock<usize> = LazyLock::new(|| {
    let methods = PySequenceMethods {
        sq_length: Some(str_len_slot),
        sq_concat: Some(pon_str_concat),
        sq_item: Some(str_item_slot),
        ..PySequenceMethods::EMPTY
    };
    Box::into_raw(Box::new(methods)) as usize
});

static STR_MAPPING_METHODS: LazyLock<usize> = LazyLock::new(|| {
    let methods = PyMappingMethods {
        mp_length: Some(str_len_slot),
        mp_subscript: Some(str_subscript_slot),
        mp_ass_subscript: None,
    };
    Box::into_raw(Box::new(methods)) as usize
});

fn install_str_slots() -> Result<(), String> {
    if let Err(message) = super::ensure_runtime_initialized() {
        return Err(message);
    }
    super::with_runtime(|runtime| unsafe {
        (*runtime.unicode_type).tp_getattro = Some(str_getattro);
        (*runtime.unicode_type).tp_as_sequence = *STR_SEQUENCE_METHODS as *mut PySequenceMethods;
        (*runtime.unicode_type).tp_as_mapping = *STR_MAPPING_METHODS as *mut PyMappingMethods;
    })
    .ok_or_else(|| "runtime is not initialized".to_owned())
}

fn install_bytes_slots() -> Result<(), String> {
    if let Err(message) = super::ensure_runtime_initialized() {
        return Err(message);
    }
    unsafe {
        (*bytes_type::bytes_type()).tp_getattro = Some(bytes_getattro);
    }
    Ok(())
}

unsafe extern "C" fn bytes_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        return super::return_null_with_error("bytes attribute name must be str");
    };
    match name {
        "decode" => bound_bytes_method(object, name, bytes_decode_entry),
        _ => super::return_null_with_error(format!("attribute '{name}' was not found")),
    }
}

fn bound_bytes_method(
    receiver: *mut PyObject,
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> *mut PyObject {
    let function = match alloc_native_bytes_function(name, entry) {
        Ok(function) => function,
        Err(message) => return super::return_null_with_error(message),
    };
    match method::new_bound_method(function, receiver) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => super::return_null_with_error(message),
    }
}

fn alloc_native_bytes_function(
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Result<*mut PyObject, String> {
    let name_interned = crate::intern::intern(name);
    super::with_runtime(|runtime| super::alloc_function(runtime, entry as *const u8, 1, name_interned))
        .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
}


unsafe extern "C" fn str_len_slot(object: *mut PyObject) -> isize {
    match expect_str(object) {
        Ok(text) => isize::try_from(str_type::codepoint_len(&text)).unwrap_or(isize::MAX),
        Err(_) => -1,
    }
}

unsafe extern "C" fn str_item_slot(object: *mut PyObject, index: isize) -> *mut PyObject {
    match str_item_object(object, index) {
        Ok(value) => value,
        Err(message) => super::return_null_with_error(message),
    }
}

unsafe extern "C" fn str_subscript_slot(object: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    if unsafe { crate::types::dict::type_name(key) } == Some("slice") {
        match str_slice_object(object, key) {
            Ok(value) => value,
            Err(message) => super::return_null_with_error(message),
        }
    } else {
        match str_index_value(key).and_then(|index| str_item_object(object, index)) {
            Ok(value) => value,
            Err(message) => super::return_null_with_error(message),
        }
    }
}

unsafe extern "C" fn str_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        return super::return_null_with_error("str attribute name must be str");
    };
    match name {
        "split" => bound_str_method(object, name, str_split_entry),
        "join" => bound_str_method(object, name, str_join_entry),
        "replace" => bound_str_method(object, name, str_replace_entry),
        "find" => bound_str_method(object, name, str_find_entry),
        "startswith" => bound_str_method(object, name, str_startswith_entry),
        "endswith" => bound_str_method(object, name, str_endswith_entry),
        "strip" => bound_str_method(object, name, str_strip_entry),
        "lower" => bound_str_method(object, name, str_lower_entry),
        "upper" => bound_str_method(object, name, str_upper_entry),
        "title" => bound_str_method(object, name, str_title_entry),
        "encode" => bound_str_method(object, name, str_encode_entry),
        _ => super::return_null_with_error(format!("attribute '{name}' was not found")),
    }
}

fn bound_str_method(
    receiver: *mut PyObject,
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> *mut PyObject {
    let function = match alloc_native_str_function(name, entry) {
        Ok(function) => function,
        Err(message) => return super::return_null_with_error(message),
    };
    match method::new_bound_method(function, receiver) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => super::return_null_with_error(message),
    }
}

fn alloc_native_str_function(
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Result<*mut PyObject, String> {
    let name_interned = crate::intern::intern(name);
    super::with_runtime(|runtime| {
        super::alloc_function(
            runtime,
            entry as *const u8,
            str_method_arity(name),
            name_interned,
        )
    })
    .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
}

fn str_method_arity(_name: &str) -> usize {
    crate::builtins::variadic_arity()
}

unsafe extern "C" fn str_split_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { str_method_entry(STR_METHOD_SPLIT, argv, argc) }
}

unsafe extern "C" fn str_join_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { str_method_entry(STR_METHOD_JOIN, argv, argc) }
}

unsafe extern "C" fn str_replace_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { str_method_entry(STR_METHOD_REPLACE, argv, argc) }
}

unsafe extern "C" fn str_find_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { str_method_entry(STR_METHOD_FIND, argv, argc) }
}

unsafe extern "C" fn str_startswith_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { str_method_entry(STR_METHOD_STARTSWITH, argv, argc) }
}

unsafe extern "C" fn str_endswith_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { str_method_entry(STR_METHOD_ENDSWITH, argv, argc) }
}

unsafe extern "C" fn str_strip_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { str_method_entry(STR_METHOD_STRIP, argv, argc) }
}

unsafe extern "C" fn str_lower_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { str_method_entry(STR_METHOD_LOWER, argv, argc) }
}

unsafe extern "C" fn str_upper_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { str_method_entry(STR_METHOD_UPPER, argv, argc) }
}

unsafe extern "C" fn str_title_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { str_method_entry(STR_METHOD_TITLE, argv, argc) }
}

unsafe extern "C" fn str_encode_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { str_method_entry(STR_METHOD_ENCODE, argv, argc) }
}

unsafe extern "C" fn bytes_decode_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() {
        return super::return_null_with_error("bytes.decode argv pointer is null");
    }
    if argc == 0 {
        return super::return_null_with_error("bytes.decode missing receiver");
    }
    let receiver = unsafe { *argv };
    let explicit_argc = argc - 1;
    let explicit_argv = if explicit_argc == 0 {
        ptr::null_mut()
    } else {
        unsafe { argv.add(1) }
    };
    unsafe { pon_bytes_method(BYTES_METHOD_DECODE, receiver, explicit_argv, explicit_argc) }
}

unsafe fn str_method_entry(method: StrMethodId, argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() {
        return super::return_null_with_error("str method argv pointer is null");
    }
    if argc == 0 {
        return super::return_null_with_error("str method missing receiver");
    }
    let receiver = unsafe { *argv };
    let explicit_argc = argc - 1;
    let explicit_argv = if explicit_argc == 0 {
        ptr::null_mut()
    } else {
        unsafe { argv.add(1) }
    };
    unsafe { pon_str_method(method, receiver, explicit_argv, explicit_argc) }
}

/// Creates a boxed UTF-8 bytes object from raw bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_const_bytes(ptr: *const u8, len: usize) -> *mut PyObject {
    super::catch_object_helper(|| {
        let Some(bytes) = raw_bytes(ptr, len) else {
            return super::return_null_with_error("bytes pointer is null");
        };
        if let Err(message) = install_bytes_slots() {
            return super::return_null_with_error(message);
        }
        as_object_ptr(bytes_type::boxed_bytes(bytes))
    })
}

/// Creates a boxed mutable bytearray object from raw bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_const_bytearray(ptr: *const u8, len: usize) -> *mut PyObject {
    super::catch_object_helper(|| {
        let Some(bytes) = raw_bytes(ptr, len) else {
            return super::return_null_with_error("bytearray pointer is null");
        };
        as_object_ptr(bytearray_type::boxed_bytearray(bytes))
    })
}

/// Concatenates two boxed strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_str_concat(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        let (Ok(left), Ok(right)) = (expect_str(left), expect_str(right)) else {
            return super::return_null_with_error("str concatenation requires str operands");
        };
        alloc_str_object(&str_type::concat(&left, &right))
    })
}

/// Repeats a boxed string by `count`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_str_repeat(value: *mut PyObject, count: isize) -> *mut PyObject {
    super::catch_object_helper(|| match expect_str(value) {
        Ok(text) => alloc_str_object(&str_type::repeat(&text, count)),
        Err(message) => super::return_null_with_error(message),
    })
}

/// Concatenates two boxed bytes objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_bytes_concat(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        let (Ok(left), Ok(right)) = (expect_bytes_like(left), expect_bytes_like(right)) else {
            return super::return_null_with_error("bytes concatenation requires bytes-like operands");
        };
        as_object_ptr(bytes_type::boxed_bytes(&bytes_type::concat(&left, &right)))
    })
}

/// Repeats a boxed bytes object by `count`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_bytes_repeat(value: *mut PyObject, count: isize) -> *mut PyObject {
    super::catch_object_helper(|| match expect_bytes_like(value) {
        Ok(bytes) => as_object_ptr(bytes_type::boxed_bytes(&bytes_type::repeat(&bytes, count))),
        Err(message) => super::return_null_with_error(message),
    })
}

/// Concatenates two boxed bytearray objects and returns a bytearray.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_bytearray_concat(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        let (Ok(left), Ok(right)) = (expect_bytes_like(left), expect_bytes_like(right)) else {
            return super::return_null_with_error("bytearray concatenation requires bytes-like operands");
        };
        as_object_ptr(bytearray_type::boxed_bytearray(&bytearray_type::concat(&left, &right)))
    })
}

/// Repeats a boxed bytearray object by `count`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_bytearray_repeat(value: *mut PyObject, count: isize) -> *mut PyObject {
    super::catch_object_helper(|| match expect_bytes_like(value) {
        Ok(bytes) => as_object_ptr(bytearray_type::boxed_bytearray(&bytearray_type::repeat(&bytes, count))),
        Err(message) => super::return_null_with_error(message),
    })
}

/// Formats one value as an f-string interpolation result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_format_value(
    value: *mut PyObject,
    conversion: u8,
    format_spec: *mut PyObject,
) -> *mut PyObject {
    super::catch_object_helper(|| match format_value_to_text(value, conversion, format_spec) {
        Ok(text) => alloc_str_object(&text),
        Err(message) => super::return_null_with_error(message),
    })
}

/// Builds a Python f-string from raw parts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_build_fstring(parts: *const super::FStrPartRaw, len: usize) -> *mut PyObject {
    super::catch_object_helper(|| match render_fstring(parts, len) {
        Ok(text) => alloc_str_object(&text),
        Err(message) => super::return_null_with_error(message),
    })
}

/// Stable helper-table spelling for f-string building.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_build_string(parts: *const super::FStrPartRaw, len: usize) -> *mut PyObject {
    unsafe { pon_build_fstring(parts, len) }
}

/// Builds a representative template-string object from raw parts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_build_template(parts: *const super::TStrPartRaw, len: usize) -> *mut PyObject {
    super::catch_object_helper(|| match build_template(parts, len) {
        Ok(template) => template,
        Err(message) => super::return_null_with_error(message),
    })
}

/// Dispatches representative `str` methods.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_str_method(
    method: StrMethodId,
    receiver: *mut PyObject,
    argv: *mut *mut PyObject,
    argc: usize,
) -> *mut PyObject {
    super::catch_object_helper(|| {
        let Ok(receiver) = expect_str(receiver) else {
            return super::return_null_with_error("str method receiver must be str");
        };
        let Some(args) = raw_args(argv, argc) else {
            return super::return_null_with_error("str method argv pointer is null");
        };
        match method {
            STR_METHOD_SPLIT => str_split_method(&receiver, args),
            STR_METHOD_JOIN => str_join_method(&receiver, args),
            STR_METHOD_REPLACE => str_replace_method(&receiver, args),
            STR_METHOD_FIND => str_find_method(&receiver, args),
            STR_METHOD_STARTSWITH => str_startswith_method(&receiver, args),
            STR_METHOD_ENDSWITH => str_endswith_method(&receiver, args),
            STR_METHOD_STRIP => str_strip_method(&receiver, args),
            STR_METHOD_LOWER => str_lower_method(&receiver, args),
            STR_METHOD_UPPER => str_upper_method(&receiver, args),
            STR_METHOD_TITLE => str_title_method(&receiver, args),
            STR_METHOD_ENCODE => str_encode_method(&receiver, args),
            _ => super::return_null_with_error("unknown str method selector"),
        }
    })
}

/// Dispatches representative `bytes` methods and returns bytes, int, or visible text.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_bytes_method(
    method: BytesMethodId,
    receiver: *mut PyObject,
    argv: *mut *mut PyObject,
    argc: usize,
) -> *mut PyObject {
    super::catch_object_helper(|| {
        let Ok(receiver) = expect_bytes_like(receiver) else {
            return super::return_null_with_error("bytes method receiver must be bytes-like");
        };
        let Some(args) = raw_args(argv, argc) else {
            return super::return_null_with_error("bytes method argv pointer is null");
        };
        match method {
            BYTES_METHOD_SPLIT => bytes_split_method(&receiver, args),
            BYTES_METHOD_JOIN => bytes_join_method(&receiver, args),
            BYTES_METHOD_REPLACE => bytes_replace_method(&receiver, args),
            BYTES_METHOD_FIND => bytes_find_method(&receiver, args),
            BYTES_METHOD_STARTSWITH => bytes_startswith_method(&receiver, args),
            BYTES_METHOD_DECODE => bytes_decode_method(&receiver, args),
            _ => super::return_null_with_error("unknown bytes method selector"),
        }
    })
}

fn render_fstring(parts: *const super::FStrPartRaw, len: usize) -> Result<String, String> {
    let parts = raw_fstring_parts(parts, len)?;
    let mut out = String::new();
    for part in parts {
        if part.value.is_null() {
            out.push_str(raw_utf8(part.literal, part.literal_len)?);
        } else {
            out.push_str(&format_value_to_text(part.value, part.conversion, part.format_spec)?);
        }
    }
    Ok(out)
}


fn build_template(parts: *const super::TStrPartRaw, len: usize) -> Result<*mut PyObject, String> {
    let parts = raw_template_parts(parts, len)?;
    let mut strings = Vec::new();
    let mut interpolations = Vec::new();
    let mut pending_literal = String::new();

    for part in parts {
        if part.conversion == TEMPLATE_LITERAL_CONVERSION {
            pending_literal.push_str(&expect_str(part.value)?);
            continue;
        }
        if part.value.is_null() {
            pending_literal.push_str(raw_utf8(part.literal, part.literal_len)?);
            continue;
        }
        strings.push(boxed_str(&pending_literal)?);
        pending_literal.clear();
        interpolations.push(boxed_interpolation(part)?);
    }
    strings.push(boxed_str(&pending_literal)?);

    let object = Box::into_raw(Box::new(PyTemplate {
        ob_base: PyObjectHeader::new(template_type()),
        strings: crate::native::builtins_mod::alloc_tuple(strings),
        interpolations: crate::native::builtins_mod::alloc_tuple(interpolations),
    }));
    Ok(as_object_ptr(object))
}

fn boxed_interpolation(part: &super::TStrPartRaw) -> Result<*mut PyObject, String> {
    let expression = if part.expression_interned == 0 {
        boxed_str("")?
    } else {
        let Some(text) = crate::intern::resolve(part.expression_interned) else {
            return Err(format!("template interpolation expression id {} is not interned", part.expression_interned));
        };
        boxed_str(&text)?
    };
    let conversion = conversion_object(part.conversion)?;
    let format_spec = if part.format_spec.is_null() {
        unsafe { super::pon_none() }
    } else {
        part.format_spec
    };
    if format_spec.is_null() {
        return Err("failed to allocate template interpolation format_spec".to_owned());
    }
    let object = Box::into_raw(Box::new(PyInterpolation {
        ob_base: PyObjectHeader::new(interpolation_type()),
        value: part.value,
        expression,
        conversion,
        format_spec,
    }));
    Ok(as_object_ptr(object))
}

fn conversion_object(conversion: u8) -> Result<*mut PyObject, String> {
    match conversion {
        0 => {
            let none = unsafe { super::pon_none() };
            if none.is_null() {
                Err("failed to allocate template interpolation conversion".to_owned())
            } else {
                Ok(none)
            }
        }
        b's' => boxed_str("s"),
        b'r' => boxed_str("r"),
        b'a' => boxed_str("a"),
        _ => Err("unsupported template-string conversion".to_owned()),
    }
}

fn boxed_str(text: &str) -> Result<*mut PyObject, String> {
    let object = alloc_str_object(text);
    if object.is_null() {
        Err("failed to allocate template string attribute".to_owned())
    } else {
        Ok(object)
    }
}

fn format_value_to_text(value: *mut PyObject, conversion: u8, format_spec: *mut PyObject) -> Result<String, String> {
    if format_spec.is_null() {
        return match conversion {
            0 | b's' => object_to_str(value),
            b'r' => object_to_repr(value),
            b'a' => Ok(str_type::escape_non_ascii(&object_to_repr(value)?)),
            _ => Err("unsupported f-string conversion".to_owned()),
        };
    }

    let spec = expect_str(format_spec)?;
    match conversion {
        0 => format_object_with_spec(value, &spec),
        b's' => apply_format_spec(&object_to_str(value)?, &spec, FormatValueKind::Text),
        b'r' => apply_format_spec(&object_to_repr(value)?, &spec, FormatValueKind::Text),
        b'a' => apply_format_spec(&str_type::escape_non_ascii(&object_to_repr(value)?), &spec, FormatValueKind::Text),
        _ => Err("unsupported f-string conversion".to_owned()),
    }
}

pub(crate) fn format_object_with_spec(value: *mut PyObject, spec: &str) -> Result<String, String> {
    let parsed = ParsedFormatSpec::parse(spec)?;
    let (body, kind) = match parsed.ty {
        Some('d') => {
            let value = object_to_i64(value).ok_or_else(|| "format code 'd' requires int".to_owned())?;
            (value.to_string(), FormatValueKind::Number)
        }
        Some('f') => {
            let value = object_to_f64(value).ok_or_else(|| "format code 'f' requires int or float".to_owned())?;
            let precision = parsed.precision.unwrap_or(6);
            (format!("{value:.precision$}"), FormatValueKind::Number)
        }
        Some('s') => {
            let text = if let Some(precision) = parsed.precision {
                truncate_to_precision(&object_to_str(value)?, precision)
            } else {
                object_to_str(value)?
            };
            (text, FormatValueKind::Text)
        }
        None => {
            let text = object_to_str(value)?;
            let text = if let Some(precision) = parsed.precision {
                truncate_to_precision(&text, precision)
            } else {
                text
            };
            (text, FormatValueKind::Text)
        }
        Some(_) => return Err("unsupported format specification".to_owned()),
    };
    apply_parsed_format(&body, parsed, kind)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FormatValueKind {
    Text,
    Number,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FormatAlign {
    Left,
    Right,
    Center,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ParsedFormatSpec {
    fill: char,
    align: Option<FormatAlign>,
    zero: bool,
    width: Option<usize>,
    precision: Option<usize>,
    ty: Option<char>,
}

impl ParsedFormatSpec {
    fn parse(spec: &str) -> Result<Self, String> {
        let mut chars = spec.chars().peekable();
        let mut fill = ' ';
        let mut align = None;
        let mut zero = false;

        let mut clone = chars.clone();
        let first = clone.next();
        let second = clone.next();
        if let (Some(fill_ch), Some(align_ch)) = (first, second) {
            if let Some(parsed) = parse_align(align_ch) {
                fill = fill_ch;
                align = Some(parsed);
                chars.next();
                chars.next();
            }
        }
        if align.is_none() {
            if let Some(parsed) = chars.peek().copied().and_then(parse_align) {
                align = Some(parsed);
                chars.next();
            }
        }
        if chars.peek() == Some(&'0') {
            zero = true;
            fill = '0';
            if align.is_none() {
                align = Some(FormatAlign::Right);
            }
            chars.next();
        }

        let width = parse_digits(&mut chars)?;
        let precision = if chars.peek() == Some(&'.') {
            chars.next();
            Some(parse_digits(&mut chars)?.unwrap_or(0))
        } else {
            None
        };
        let ty = chars.next();
        if chars.next().is_some() {
            return Err("unsupported format specification".to_owned());
        }
        if let Some(ty) = ty {
            if !matches!(ty, 'd' | 'f' | 's') {
                return Err("unsupported format specification".to_owned());
            }
        }
        Ok(Self {
            fill,
            align,
            zero,
            width,
            precision,
            ty,
        })
    }
}

fn parse_align(ch: char) -> Option<FormatAlign> {
    match ch {
        '<' => Some(FormatAlign::Left),
        '>' => Some(FormatAlign::Right),
        '^' => Some(FormatAlign::Center),
        _ => None,
    }
}

fn parse_digits(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<Option<usize>, String> {
    let mut value = 0usize;
    let mut saw_digit = false;
    while let Some(ch) = chars.peek().copied() {
        if !ch.is_ascii_digit() {
            break;
        }
        saw_digit = true;
        let digit = ch.to_digit(10).expect("ASCII digit") as usize;
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(digit))
            .ok_or_else(|| "format width is too large".to_owned())?;
        chars.next();
    }
    Ok(saw_digit.then_some(value))
}

fn apply_format_spec(value: &str, spec: &str, kind: FormatValueKind) -> Result<String, String> {
    let parsed = ParsedFormatSpec::parse(spec)?;
    if !matches!(parsed.ty, None | Some('s')) {
        return Err("unsupported format specification".to_owned());
    }
    let value = if let Some(precision) = parsed.precision {
        truncate_to_precision(value, precision)
    } else {
        value.to_owned()
    };
    apply_parsed_format(&value, parsed, kind)
}

fn apply_parsed_format(value: &str, spec: ParsedFormatSpec, kind: FormatValueKind) -> Result<String, String> {
    let Some(width) = spec.width else {
        return Ok(value.to_owned());
    };
    let len = str_type::codepoint_len(value);
    let pad = width.saturating_sub(len);
    if pad == 0 {
        return Ok(value.to_owned());
    }
    let align = spec.align.unwrap_or(match kind {
        FormatValueKind::Text => FormatAlign::Left,
        FormatValueKind::Number => FormatAlign::Right,
    });
    if spec.zero && align == FormatAlign::Right && value.starts_with(['-', '+']) {
        let mut out = String::with_capacity(value.len() + pad);
        out.push_str(&value[..1]);
        push_fill(&mut out, spec.fill, pad);
        out.push_str(&value[1..]);
        return Ok(out);
    }
    let mut out = String::with_capacity(value.len() + pad * spec.fill.len_utf8());
    match align {
        FormatAlign::Left => {
            out.push_str(value);
            push_fill(&mut out, spec.fill, pad);
        }
        FormatAlign::Right => {
            push_fill(&mut out, spec.fill, pad);
            out.push_str(value);
        }
        FormatAlign::Center => {
            let left = pad / 2;
            let right = pad - left;
            push_fill(&mut out, spec.fill, left);
            out.push_str(value);
            push_fill(&mut out, spec.fill, right);
        }
    }
    Ok(out)
}

fn push_fill(out: &mut String, fill: char, count: usize) {
    for _ in 0..count {
        out.push(fill);
    }
}

fn truncate_to_precision(value: &str, precision: usize) -> String {
    value.chars().take(precision).collect()
}

fn object_to_i64(value: *mut PyObject) -> Option<i64> {
    if value.is_null() {
        return None;
    }
    if let Some(value) = unsafe { crate::types::bool_::to_bool(value) } {
        return Some(i64::from(value));
    }
    super::with_runtime(|runtime| unsafe {
        is_exact_type(value, runtime.long_type).then(|| (*value.cast::<PyLong>()).value)
    })
    .flatten()
}

fn object_to_f64(value: *mut PyObject) -> Option<f64> {
    if let Some(value) = unsafe { crate::types::float::to_f64(value) } {
        return Some(value);
    }
    object_to_i64(value).map(|value| value as f64)
}

fn str_split_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() > 1 {
        return super::return_null_with_error("str.split expected at most one argument");
    }
    let sep = if let Some(arg) = args.first().copied() {
        match expect_str(arg) {
            Ok(sep) => Some(sep),
            Err(message) => return super::return_null_with_error(message),
        }
    } else {
        None
    };
    let pieces = str_type::split(receiver, sep.as_deref());
    let mut objects = Vec::with_capacity(pieces.len());
    for piece in pieces {
        match boxed_str(&piece) {
            Ok(object) => objects.push(object),
            Err(message) => return super::return_null_with_error(message),
        }
    }
    unsafe { super::seq::pon_build_list(objects.as_mut_ptr(), objects.len()) }
}

fn str_join_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() != 1 {
        return super::return_null_with_error("str.join expected exactly one argument");
    }
    let values = match super::seq::sequence_to_vec(args[0]) {
        Ok(values) => values,
        Err(message) => return super::return_null_with_error(message),
    };
    let mut items = Vec::with_capacity(values.len());
    for value in values {
        match expect_str(value) {
            Ok(item) => items.push(item),
            Err(_) => return super::return_null_with_error("str.join expected every item to be str"),
        }
    }
    alloc_str_object(&str_type::join(receiver, &items))
}

fn str_replace_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if !(args.len() == 2 || args.len() == 3) {
        return super::return_null_with_error("str.replace expected two or three arguments");
    }
    let (Ok(old), Ok(new)) = (expect_str(args[0]), expect_str(args[1])) else {
        return super::return_null_with_error("str.replace arguments must be str");
    };
    if let Some(count) = args.get(2).copied() {
        let count = match str_long_value(count) {
            Ok(value) if value >= 0 => value as usize,
            Ok(_) => return alloc_str_object(&receiver.replace(&old, &new)),
            Err(message) => return super::return_null_with_error(message),
        };
        alloc_str_object(&receiver.replacen(&old, &new, count))
    } else {
        alloc_str_object(&str_type::replace(receiver, &old, &new))
    }
}

fn str_find_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() != 1 {
        return super::return_null_with_error("str.find expected exactly one argument");
    }
    match expect_str(args[0]) {
        Ok(needle) => unsafe { super::pon_const_int(str_type::find(receiver, &needle) as i64) },
        Err(message) => super::return_null_with_error(message),
    }
}

fn str_startswith_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() != 1 {
        return super::return_null_with_error("str.startswith expected exactly one argument");
    }
    match expect_str(args[0]) {
        Ok(prefix) => alloc_str_object(str_type::startswith(receiver, &prefix).as_python_text()),
        Err(message) => super::return_null_with_error(message),
    }
}

fn str_endswith_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() != 1 {
        return super::return_null_with_error("str.endswith expected exactly one argument");
    }
    match expect_str(args[0]) {
        Ok(suffix) => alloc_str_object(if receiver.ends_with(&suffix) { "True" } else { "False" }),
        Err(message) => super::return_null_with_error(message),
    }
}

fn str_strip_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if !args.is_empty() {
        return super::return_null_with_error("representative str.strip supports default whitespace only");
    }
    alloc_str_object(receiver.trim())
}

fn str_lower_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if !args.is_empty() {
        return super::return_null_with_error("str.lower expected no arguments");
    }
    alloc_str_object(&receiver.to_lowercase())
}

fn str_upper_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if !args.is_empty() {
        return super::return_null_with_error("str.upper expected no arguments");
    }
    alloc_str_object(&receiver.to_uppercase())
}

fn str_title_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if !args.is_empty() {
        return super::return_null_with_error("str.title expected no arguments");
    }
    let mut out = String::with_capacity(receiver.len());
    let mut new_word = true;
    for ch in receiver.chars() {
        if ch.is_alphanumeric() {
            if new_word {
                out.extend(ch.to_uppercase());
                new_word = false;
            } else {
                out.extend(ch.to_lowercase());
            }
        } else {
            out.push(ch);
            new_word = true;
        }
    }
    alloc_str_object(&out)
}

fn str_encode_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() > 2 {
        return super::return_null_with_error("str.encode expected at most two arguments");
    }
    let encoding = if let Some(arg) = args.first().copied() {
        match expect_str(arg) {
            Ok(encoding) => encoding,
            Err(message) => return super::return_null_with_error(message),
        }
    } else {
        "utf-8".to_owned()
    };
    if let Some(errors) = args.get(1).copied() {
        match expect_str(errors) {
            Ok(errors) if errors == "strict" => {}
            Ok(errors) => return super::return_null_with_error(format!("unsupported str.encode errors handler '{errors}'")),
            Err(message) => return super::return_null_with_error(message),
        }
    }
    let encoded = match encoding.to_ascii_lowercase().as_str() {
        "utf-8" | "utf8" => str_type::encode_utf8(receiver),
        "ascii" => {
            if !receiver.is_ascii() {
                return super::return_null_with_error("ascii codec can't encode non-ascii character");
            }
            receiver.as_bytes().to_vec()
        }
        _ => return super::return_null_with_error(format!("unsupported str.encode encoding '{encoding}'")),
    };
    unsafe { pon_const_bytes(encoded.as_ptr(), encoded.len()) }
}

fn bytes_split_method(receiver: &[u8], args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() > 1 {
        return super::return_null_with_error("bytes.split expected at most one argument");
    }
    let sep = if let Some(arg) = args.first().copied() {
        match expect_bytes_like(arg) {
            Ok(sep) => Some(sep),
            Err(message) => return super::return_null_with_error(message),
        }
    } else {
        None
    };
    let pieces = bytes_type::split(receiver, sep.as_deref());
    alloc_str_object(&bytes_type::repr_bytes_list(&pieces))
}

fn bytes_join_method(receiver: &[u8], args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() != 1 {
        return super::return_null_with_error("bytes.join expected exactly one argument");
    }
    let Ok(arg) = expect_bytes_like(args[0]) else {
        return super::return_null_with_error("representative bytes.join expects a bytes-like iterable");
    };
    let items = arg.iter().map(|byte| vec![*byte]).collect::<Vec<_>>();
    as_object_ptr(bytes_type::boxed_bytes(&bytes_type::join(receiver, &items)))
}

fn bytes_replace_method(receiver: &[u8], args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() != 2 {
        return super::return_null_with_error("bytes.replace expected exactly two arguments");
    }
    let (Ok(old), Ok(new)) = (expect_bytes_like(args[0]), expect_bytes_like(args[1])) else {
        return super::return_null_with_error("bytes.replace arguments must be bytes-like");
    };
    as_object_ptr(bytes_type::boxed_bytes(&bytes_type::replace(receiver, &old, &new)))
}

fn bytes_find_method(receiver: &[u8], args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() != 1 {
        return super::return_null_with_error("bytes.find expected exactly one argument");
    }
    match expect_bytes_like(args[0]) {
        Ok(needle) => unsafe { super::pon_const_int(bytes_type::find(receiver, &needle) as i64) },
        Err(message) => super::return_null_with_error(message),
    }
}

fn bytes_startswith_method(receiver: &[u8], args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() != 1 {
        return super::return_null_with_error("bytes.startswith expected exactly one argument");
    }
    match expect_bytes_like(args[0]) {
        Ok(prefix) => alloc_str_object(if bytes_type::startswith(receiver, &prefix) { "True" } else { "False" }),
        Err(message) => super::return_null_with_error(message),
    }
}

fn bytes_decode_method(receiver: &[u8], args: &[*mut PyObject]) -> *mut PyObject {
    if !args.is_empty() {
        return super::return_null_with_error("representative bytes.decode supports default UTF-8 only");
    }
    match core::str::from_utf8(receiver) {
        Ok(text) => alloc_str_object(text),
        Err(_) => super::return_null_with_error("bytes.decode expected UTF-8 bytes"),
    }
}

fn alloc_str_object(text: &str) -> *mut PyObject {
    if let Err(message) = install_str_slots() {
        return super::return_null_with_error(message);
    }
    match super::with_runtime(|runtime| super::alloc_unicode(runtime, text.as_bytes())) {
        Some(Ok(object)) => object,
        Some(Err(message)) => super::return_null_with_error(message),
        None => super::return_null_with_error("runtime is not initialized"),
    }
}

fn expect_str(value: *mut PyObject) -> Result<String, String> {
    if value.is_null() {
        return Err("expected str, got NULL".to_owned());
    }
    if let Err(message) = super::ensure_runtime_initialized() {
        return Err(message);
    }
    super::with_runtime(|runtime| unsafe {
        if !is_exact_type(value, runtime.unicode_type) {
            return Err("expected str object".to_owned());
        }
        let unicode = &*value.cast::<PyUnicode>();
        unicode
            .as_str()
            .map(ToOwned::to_owned)
            .ok_or_else(|| "unicode object contains invalid UTF-8".to_owned())
    })
    .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
}

fn str_long_value(value: *mut PyObject) -> Result<i64, String> {
    if value.is_null() {
        return Err("integer operand is NULL".to_owned());
    }
    if let Err(message) = super::ensure_runtime_initialized() {
        return Err(message);
    }
    super::with_runtime(|runtime| unsafe {
        if is_exact_type(value, runtime.long_type) {
            Ok((*value.cast::<PyLong>()).value)
        } else {
            Err("expected int object".to_owned())
        }
    })
    .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
}

fn str_index_value(value: *mut PyObject) -> Result<isize, String> {
    isize::try_from(str_long_value(value)?).map_err(|_| "string index is out of range for this platform".to_owned())
}

fn normalize_str_index(index: isize, len: usize) -> Result<usize, String> {
    let len_isize = isize::try_from(len).map_err(|_| "string is too large for this platform".to_owned())?;
    let adjusted = if index < 0 { index.saturating_add(len_isize) } else { index };
    if adjusted < 0 || adjusted >= len_isize {
        Err("string index out of range".to_owned())
    } else {
        Ok(adjusted as usize)
    }
}

fn str_item_object(object: *mut PyObject, index: isize) -> Result<*mut PyObject, String> {
    let text = expect_str(object)?;
    let index = normalize_str_index(index, str_type::codepoint_len(&text))?;
    let Some(ch) = text.chars().nth(index) else {
        return Err("string index out of range".to_owned());
    };
    let mut out = String::new();
    out.push(ch);
    Ok(alloc_str_object(&out))
}

fn is_none(value: *mut PyObject) -> bool {
    super::with_runtime(|runtime| unsafe { is_exact_type(value, runtime.none_type) }).unwrap_or(false)
}

fn normalize_slice_bound(value: *mut PyObject, len: isize, default_none: isize, lower: isize, upper: isize) -> Result<isize, String> {
    if is_none(value) {
        return Ok(default_none.clamp(lower, upper));
    }
    let mut value = str_index_value(value)?;
    if value < 0 {
        value = value.saturating_add(len);
    }
    Ok(value.clamp(lower, upper))
}

fn normalize_str_slice(slice: &PySlice, len: usize) -> Result<crate::types::slice_::SliceIndices, String> {
    let len = isize::try_from(len).map_err(|_| "string is too large for slice indices".to_owned())?;
    let step = if is_none(slice.step) { 1 } else { str_index_value(slice.step)? };
    if step == 0 {
        return Err("slice step cannot be zero".to_owned());
    }
    let (start, stop) = if step > 0 {
        (
            normalize_slice_bound(slice.start, len, 0, 0, len)?,
            normalize_slice_bound(slice.stop, len, len, 0, len)?,
        )
    } else {
        (
            normalize_slice_bound(slice.start, len, len - 1, -1, len - 1)?,
            normalize_slice_bound(slice.stop, len, -1, -1, len - 1)?,
        )
    };
    let slice_len = if step > 0 {
        if stop <= start { 0 } else { ((stop - start - 1) / step + 1) as usize }
    } else if stop >= start {
        0
    } else {
        ((start - stop - 1) / (-step) + 1) as usize
    };
    Ok(crate::types::slice_::SliceIndices { start, stop, step, len: slice_len })
}

fn str_slice_object(object: *mut PyObject, key: *mut PyObject) -> Result<*mut PyObject, String> {
    let text = expect_str(object)?;
    let indices = normalize_str_slice(unsafe { &*key.cast::<PySlice>() }, str_type::codepoint_len(&text))?;
    let chars = text.chars().collect::<Vec<_>>();
    let mut out = String::with_capacity(text.len());
    let mut index = indices.start;
    for _ in 0..indices.len {
        out.push(chars[index as usize]);
        index = index.saturating_add(indices.step);
    }
    Ok(alloc_str_object(&out))
}

fn expect_bytes_like(value: *mut PyObject) -> Result<Vec<u8>, String> {
    if value.is_null() {
        return Err("expected bytes-like object, got NULL".to_owned());
    }
    let ty = unsafe { (*value).ob_type };
    if bytes_type::is_bytes_type(ty) {
        let bytes = unsafe { &*value.cast::<bytes_type::PyBytes>() };
        return Ok(unsafe { bytes.as_slice() }.to_vec());
    }
    if bytearray_type::is_bytearray_type(ty) {
        let bytearray = unsafe { &*value.cast::<bytearray_type::PyByteArray>() };
        return Ok(bytearray.as_slice().to_vec());
    }
    Err("expected bytes-like object".to_owned())
}

fn object_to_str(value: *mut PyObject) -> Result<String, String> {
    if value.is_null() {
        return Err("cannot format NULL object".to_owned());
    }
    let ty = unsafe { (*value).ob_type };
    if bytes_type::is_bytes_type(ty) || bytearray_type::is_bytearray_type(ty) {
        return object_to_repr(value);
    }
    super::format_object_for_print(value)
}

fn object_to_repr(value: *mut PyObject) -> Result<String, String> {
    if value.is_null() {
        return Err("cannot repr NULL object".to_owned());
    }
    let ty = unsafe { (*value).ob_type };
    if bytes_type::is_bytes_type(ty) {
        let bytes = unsafe { &*value.cast::<bytes_type::PyBytes>() };
        return Ok(bytes_type::repr(unsafe { bytes.as_slice() }));
    }
    if bytearray_type::is_bytearray_type(ty) {
        let bytearray = unsafe { &*value.cast::<bytearray_type::PyByteArray>() };
        return Ok(bytearray_type::repr(bytearray.as_slice()));
    }
    if let Err(message) = super::ensure_runtime_initialized() {
        return Err(message);
    }
    super::with_runtime(|runtime| unsafe {
        if is_exact_type(value, runtime.unicode_type) {
            let unicode = &*value.cast::<PyUnicode>();
            return unicode
                .as_str()
                .map(str_type::repr)
                .ok_or_else(|| "unicode object contains invalid UTF-8".to_owned());
        }
        if is_exact_type(value, runtime.long_type) {
            return Ok((*value.cast::<PyLong>()).value.to_string());
        }
        if is_exact_type(value, runtime.none_type) {
            return Ok("None".to_owned());
        }
        super::format_object_for_print(value)
    })
    .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
}

fn raw_bytes<'a>(ptr: *const u8, len: usize) -> Option<&'a [u8]> {
    if ptr.is_null() {
        return (len == 0).then_some(&[]);
    }
    Some(unsafe { core::slice::from_raw_parts(ptr, len) })
}

fn raw_utf8<'a>(ptr: *const u8, len: usize) -> Result<&'a str, String> {
    let Some(bytes) = raw_bytes(ptr, len) else {
        return Err("string literal pointer is null".to_owned());
    };
    core::str::from_utf8(bytes).map_err(|_| "string literal is not valid UTF-8".to_owned())
}

fn raw_args<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
    if argv.is_null() {
        return (argc == 0).then_some(&[]);
    }
    Some(unsafe { core::slice::from_raw_parts(argv, argc) })
}

fn raw_fstring_parts<'a>(parts: *const super::FStrPartRaw, len: usize) -> Result<&'a [super::FStrPartRaw], String> {
    if parts.is_null() {
        return if len == 0 {
            Ok(&[])
        } else {
            Err("f-string parts pointer is null".to_owned())
        };
    }
    Ok(unsafe { core::slice::from_raw_parts(parts, len) })
}

fn raw_template_parts<'a>(parts: *const super::TStrPartRaw, len: usize) -> Result<&'a [super::TStrPartRaw], String> {
    if parts.is_null() {
        return if len == 0 {
            Ok(&[])
        } else {
            Err("template-string parts pointer is null".to_owned())
        };
    }
    Ok(unsafe { core::slice::from_raw_parts(parts, len) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread_state::test_state_lock;

    #[test]
    fn fstring_helper_formats_unicode_repr_and_ascii() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(super::super::pon_runtime_init(), 0);
            let value = super::super::pon_const_str("é".as_ptr(), "é".len());
            let rendered = pon_format_value(value, b'a', ptr::null_mut());
            assert_eq!(super::super::format_object_for_print(rendered).as_deref(), Ok("'\\xe9'"));
        }
    }
}
