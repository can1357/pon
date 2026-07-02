//! String, bytes, f-string, and template-string helper family namespace.
//!
//! Shared raw part layouts live in [`crate::abi::FStrPartRaw`] and
//! [`crate::abi::TStrPartRaw`].  These helpers follow the runtime-wide NULL
//! sentinel contract: success returns a boxed Python object, failure records the
//! thread-state error and returns NULL.

use core::ffi::c_int;
use core::mem;
use core::ptr;
use std::sync::{LazyLock, OnceLock};

use crate::object::{PyLong, PyMappingMethods, PyObject, PyObjectHeader, PySequenceMethods, PyType, PyUnicode, as_object_ptr, is_exact_type};
use crate::types::{bytearray_ as bytearray_type, bytes_ as bytes_type, memoryview as memoryview_type, method, slice_::PySlice, str_ as str_type, type_};

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
pub const STR_METHOD_RSPLIT: StrMethodId = 12;
pub const STR_METHOD_SPLITLINES: StrMethodId = 13;
pub const STR_METHOD_LSTRIP: StrMethodId = 14;
pub const STR_METHOD_RSTRIP: StrMethodId = 15;
pub const STR_METHOD_RFIND: StrMethodId = 16;
pub const STR_METHOD_INDEX: StrMethodId = 17;
pub const STR_METHOD_RINDEX: StrMethodId = 18;
pub const STR_METHOD_COUNT: StrMethodId = 19;
pub const STR_METHOD_CAPITALIZE: StrMethodId = 20;
pub const STR_METHOD_CASEFOLD: StrMethodId = 21;
pub const STR_METHOD_SWAPCASE: StrMethodId = 22;
pub const STR_METHOD_CENTER: StrMethodId = 23;
pub const STR_METHOD_LJUST: StrMethodId = 24;
pub const STR_METHOD_RJUST: StrMethodId = 25;
pub const STR_METHOD_ZFILL: StrMethodId = 26;
pub const STR_METHOD_EXPANDTABS: StrMethodId = 27;
pub const STR_METHOD_PARTITION: StrMethodId = 28;
pub const STR_METHOD_RPARTITION: StrMethodId = 29;
pub const STR_METHOD_REMOVEPREFIX: StrMethodId = 30;
pub const STR_METHOD_REMOVESUFFIX: StrMethodId = 31;
pub const STR_METHOD_ISDECIMAL: StrMethodId = 32;
pub const STR_METHOD_ISDIGIT: StrMethodId = 33;
pub const STR_METHOD_ISNUMERIC: StrMethodId = 34;
pub const STR_METHOD_ISALPHA: StrMethodId = 35;
pub const STR_METHOD_ISALNUM: StrMethodId = 36;
pub const STR_METHOD_ISSPACE: StrMethodId = 37;
pub const STR_METHOD_ISUPPER: StrMethodId = 38;
pub const STR_METHOD_ISLOWER: StrMethodId = 39;
pub const STR_METHOD_ISTITLE: StrMethodId = 40;
pub const STR_METHOD_ISIDENTIFIER: StrMethodId = 41;
pub const STR_METHOD_ISPRINTABLE: StrMethodId = 42;
pub const STR_METHOD_ISASCII: StrMethodId = 43;
pub const STR_METHOD_TRANSLATE: StrMethodId = 44;
pub const STR_METHOD_MAKETRANS: StrMethodId = 45;
pub const STR_METHOD_FORMAT_MAP: StrMethodId = 46;

/// Bytes/bytearray method selector passed through the helper ABI.
pub type BytesMethodId = u16;

pub const BYTES_METHOD_SPLIT: BytesMethodId = 1;
pub const BYTES_METHOD_JOIN: BytesMethodId = 2;
pub const BYTES_METHOD_REPLACE: BytesMethodId = 3;
pub const BYTES_METHOD_FIND: BytesMethodId = 4;
pub const BYTES_METHOD_STARTSWITH: BytesMethodId = 5;
pub const BYTES_METHOD_DECODE: BytesMethodId = 6;
pub const BYTES_METHOD_ENDSWITH: BytesMethodId = 7;
pub const BYTES_METHOD_RSPLIT: BytesMethodId = 8;
pub const BYTES_METHOD_SPLITLINES: BytesMethodId = 9;
pub const BYTES_METHOD_STRIP: BytesMethodId = 10;
pub const BYTES_METHOD_LSTRIP: BytesMethodId = 11;
pub const BYTES_METHOD_RSTRIP: BytesMethodId = 12;
pub const BYTES_METHOD_RFIND: BytesMethodId = 13;
pub const BYTES_METHOD_INDEX: BytesMethodId = 14;
pub const BYTES_METHOD_RINDEX: BytesMethodId = 15;
pub const BYTES_METHOD_COUNT: BytesMethodId = 16;
pub const BYTES_METHOD_UPPER: BytesMethodId = 17;
pub const BYTES_METHOD_LOWER: BytesMethodId = 18;
pub const BYTES_METHOD_TITLE: BytesMethodId = 19;
pub const BYTES_METHOD_CAPITALIZE: BytesMethodId = 20;
pub const BYTES_METHOD_SWAPCASE: BytesMethodId = 21;
pub const BYTES_METHOD_CENTER: BytesMethodId = 22;
pub const BYTES_METHOD_LJUST: BytesMethodId = 23;
pub const BYTES_METHOD_RJUST: BytesMethodId = 24;
pub const BYTES_METHOD_ZFILL: BytesMethodId = 25;
pub const BYTES_METHOD_EXPANDTABS: BytesMethodId = 26;
pub const BYTES_METHOD_PARTITION: BytesMethodId = 27;
pub const BYTES_METHOD_RPARTITION: BytesMethodId = 28;
pub const BYTES_METHOD_REMOVEPREFIX: BytesMethodId = 29;
pub const BYTES_METHOD_REMOVESUFFIX: BytesMethodId = 30;
pub const BYTES_METHOD_ISALPHA: BytesMethodId = 31;
pub const BYTES_METHOD_ISALNUM: BytesMethodId = 32;
pub const BYTES_METHOD_ISDIGIT: BytesMethodId = 33;
pub const BYTES_METHOD_ISSPACE: BytesMethodId = 34;
pub const BYTES_METHOD_ISUPPER: BytesMethodId = 35;
pub const BYTES_METHOD_ISLOWER: BytesMethodId = 36;
pub const BYTES_METHOD_ISTITLE: BytesMethodId = 37;
pub const BYTES_METHOD_ISASCII: BytesMethodId = 38;
pub const BYTES_METHOD_HEX: BytesMethodId = 39;
pub const BYTES_METHOD_FROMHEX: BytesMethodId = 40;
pub const BYTES_METHOD_APPEND: BytesMethodId = 41;
pub const BYTES_METHOD_EXTEND: BytesMethodId = 42;
pub const BYTES_METHOD_INSERT: BytesMethodId = 43;
pub const BYTES_METHOD_POP: BytesMethodId = 44;
pub const BYTES_METHOD_REMOVE: BytesMethodId = 45;
pub const BYTES_METHOD_CLEAR: BytesMethodId = 46;

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

#[repr(C)]
struct PyStrTranslateTable {
    ob_base: PyObjectHeader,
    table: str_type::TranslationTable,
}

static TEMPLATE_TYPE: OnceLock<usize> = OnceLock::new();
static INTERPOLATION_TYPE: OnceLock<usize> = OnceLock::new();
static STR_TRANSLATE_TABLE_TYPE: OnceLock<usize> = OnceLock::new();

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

fn str_translate_table_type() -> *mut PyType {
    *STR_TRANSLATE_TABLE_TYPE.get_or_init(|| {
        let ty = Box::new(PyType::new(runtime_type_type(), "str.translate_table", mem::size_of::<PyStrTranslateTable>()));
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

static BYTES_SEQUENCE_METHODS: LazyLock<usize> = LazyLock::new(|| {
    let methods = PySequenceMethods {
        sq_length: Some(bytes_len_slot),
        sq_concat: Some(pon_bytes_concat),
        sq_repeat: Some(bytes_repeat_slot),
        sq_item: Some(bytes_item_slot),
        ..PySequenceMethods::EMPTY
    };
    Box::into_raw(Box::new(methods)) as usize
});

static BYTES_MAPPING_METHODS: LazyLock<usize> = LazyLock::new(|| {
    let methods = PyMappingMethods {
        mp_length: Some(bytes_len_slot),
        mp_subscript: Some(bytes_subscript_slot),
        mp_ass_subscript: None,
    };
    Box::into_raw(Box::new(methods)) as usize
});

static BYTEARRAY_SEQUENCE_METHODS: LazyLock<usize> = LazyLock::new(|| {
    let methods = PySequenceMethods {
        sq_length: Some(bytearray_len_slot),
        sq_concat: Some(pon_bytearray_concat),
        sq_repeat: Some(bytearray_repeat_slot),
        sq_item: Some(bytearray_item_slot),
        sq_ass_item: Some(bytearray_ass_item_slot),
        ..PySequenceMethods::EMPTY
    };
    Box::into_raw(Box::new(methods)) as usize
});

static BYTEARRAY_MAPPING_METHODS: LazyLock<usize> = LazyLock::new(|| {
    let methods = PyMappingMethods {
        mp_length: Some(bytearray_len_slot),
        mp_subscript: Some(bytearray_subscript_slot),
        mp_ass_subscript: Some(bytearray_ass_subscript_slot),
    };
    Box::into_raw(Box::new(methods)) as usize
});

static MEMORYVIEW_SEQUENCE_METHODS: LazyLock<usize> = LazyLock::new(|| {
    let methods = PySequenceMethods {
        sq_length: Some(memoryview_len_slot),
        sq_item: Some(memoryview_item_slot),
        sq_ass_item: Some(memoryview_ass_item_slot),
        ..PySequenceMethods::EMPTY
    };
    Box::into_raw(Box::new(methods)) as usize
});

static MEMORYVIEW_MAPPING_METHODS: LazyLock<usize> = LazyLock::new(|| {
    let methods = PyMappingMethods {
        mp_length: Some(memoryview_len_slot),
        mp_subscript: Some(memoryview_subscript_slot),
        mp_ass_subscript: Some(memoryview_ass_subscript_slot),
    };
    Box::into_raw(Box::new(methods)) as usize
});

fn install_bytes_slots() -> Result<(), String> {
    if let Err(message) = super::ensure_runtime_initialized() {
        return Err(message);
    }
    unsafe {
        (*bytes_type::bytes_type()).tp_getattro = Some(bytes_getattro);
        (*bytes_type::bytes_type()).tp_as_sequence = *BYTES_SEQUENCE_METHODS as *mut PySequenceMethods;
        (*bytes_type::bytes_type()).tp_as_mapping = *BYTES_MAPPING_METHODS as *mut PyMappingMethods;
        (*bytearray_type::bytearray_type()).tp_getattro = Some(bytearray_getattro);
        (*bytearray_type::bytearray_type()).tp_as_sequence = *BYTEARRAY_SEQUENCE_METHODS as *mut PySequenceMethods;
        (*bytearray_type::bytearray_type()).tp_as_mapping = *BYTEARRAY_MAPPING_METHODS as *mut PyMappingMethods;
    }
    Ok(())
}

fn install_memoryview_slots() -> Result<(), String> {
    if let Err(message) = super::ensure_runtime_initialized() {
        return Err(message);
    }
    unsafe {
        (*memoryview_type::memoryview_type()).tp_getattro = Some(memoryview_getattro);
        (*memoryview_type::memoryview_type()).tp_as_sequence = *MEMORYVIEW_SEQUENCE_METHODS as *mut PySequenceMethods;
        (*memoryview_type::memoryview_type()).tp_as_mapping = *MEMORYVIEW_MAPPING_METHODS as *mut PyMappingMethods;
    }
    Ok(())
}

unsafe extern "C" fn bytes_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        return super::return_null_with_error("bytes attribute name must be str");
    };
    match bytes_method_entry_for_name(name) {
        Some(entry) => bound_bytes_method(object, name, entry),
        None => super::return_null_with_error(format!("attribute '{name}' was not found")),
    }
}

unsafe extern "C" fn bytearray_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        return super::return_null_with_error("bytearray attribute name must be str");
    };
    match bytearray_method_entry_for_name(name) {
        Some(entry) => bound_bytes_method(object, name, entry),
        None => super::return_null_with_error(format!("attribute '{name}' was not found")),
    }
}

unsafe extern "C" fn memoryview_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        return super::return_null_with_error("memoryview attribute name must be str");
    };
    match name {
        "tobytes" => bound_memoryview_method(object, name, memoryview_tobytes_entry),
        "readonly" => unsafe { super::pon_const_bool(i32::from((*object.cast::<memoryview_type::PyMemoryView>()).readonly)) },
        _ => super::return_null_with_error(format!("attribute '{name}' was not found")),
    }
}

fn bytes_method_entry_for_name(name: &str) -> Option<unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject> {
    Some(match name {
        "split" => bytes_split_entry,
        "rsplit" => bytes_rsplit_entry,
        "splitlines" => bytes_splitlines_entry,
        "join" => bytes_join_entry,
        "replace" => bytes_replace_entry,
        "find" => bytes_find_entry,
        "rfind" => bytes_rfind_entry,
        "index" => bytes_index_entry,
        "rindex" => bytes_rindex_entry,
        "count" => bytes_count_entry,
        "startswith" => bytes_startswith_entry,
        "endswith" => bytes_endswith_entry,
        "decode" => bytes_decode_entry,
        "strip" => bytes_strip_entry,
        "lstrip" => bytes_lstrip_entry,
        "rstrip" => bytes_rstrip_entry,
        "upper" => bytes_upper_entry,
        "lower" => bytes_lower_entry,
        "title" => bytes_title_entry,
        "capitalize" => bytes_capitalize_entry,
        "swapcase" => bytes_swapcase_entry,
        "center" => bytes_center_entry,
        "ljust" => bytes_ljust_entry,
        "rjust" => bytes_rjust_entry,
        "zfill" => bytes_zfill_entry,
        "expandtabs" => bytes_expandtabs_entry,
        "partition" => bytes_partition_entry,
        "rpartition" => bytes_rpartition_entry,
        "removeprefix" => bytes_removeprefix_entry,
        "removesuffix" => bytes_removesuffix_entry,
        "isalpha" => bytes_isalpha_entry,
        "isalnum" => bytes_isalnum_entry,
        "isdigit" => bytes_isdigit_entry,
        "isspace" => bytes_isspace_entry,
        "isupper" => bytes_isupper_entry,
        "islower" => bytes_islower_entry,
        "istitle" => bytes_istitle_entry,
        "isascii" => bytes_isascii_entry,
        "hex" => bytes_hex_entry,
        "fromhex" => bytes_fromhex_entry,
        _ => return None,
    })
}

fn bytearray_method_entry_for_name(name: &str) -> Option<unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject> {
    Some(match name {
        "append" => bytes_append_entry,
        "extend" => bytes_extend_entry,
        "insert" => bytes_insert_entry,
        "pop" => bytes_pop_entry,
        "remove" => bytes_remove_entry,
        "clear" => bytes_clear_entry,
        _ => bytes_method_entry_for_name(name)?,
    })
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

fn bound_memoryview_method(
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
    super::with_runtime(|runtime| super::alloc_function(runtime, entry as *const u8, crate::builtins::variadic_arity(), name_interned))
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

unsafe extern "C" fn bytes_len_slot(object: *mut PyObject) -> isize {
    match expect_bytes_like(object) {
        Ok(bytes) => isize::try_from(bytes.len()).unwrap_or(isize::MAX),
        Err(_) => -1,
    }
}

unsafe extern "C" fn bytes_item_slot(object: *mut PyObject, index: isize) -> *mut PyObject {
    match bytes_item_object(object, index) {
        Ok(value) => value,
        Err(message) => super::return_null_with_error(message),
    }
}

unsafe extern "C" fn bytes_subscript_slot(object: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    if unsafe { crate::types::dict::type_name(key) } == Some("slice") {
        match bytes_slice_object(object, key, false) {
            Ok(value) => value,
            Err(message) => super::return_null_with_error(message),
        }
    } else {
        match str_index_value(key).and_then(|index| bytes_item_object(object, index)) {
            Ok(value) => value,
            Err(message) => super::return_null_with_error(message),
        }
    }
}

unsafe extern "C" fn bytes_repeat_slot(object: *mut PyObject, count_object: *mut PyObject) -> *mut PyObject {
    match str_long_value(count_object).and_then(|value| isize::try_from(value).map_err(|_| "repeat count is out of range".to_owned())) {
        Ok(count) => unsafe { pon_bytes_repeat(object, count) },
        Err(message) => super::return_null_with_error(message),
    }
}

unsafe extern "C" fn bytearray_len_slot(object: *mut PyObject) -> isize {
    unsafe { bytes_len_slot(object) }
}

unsafe extern "C" fn bytearray_item_slot(object: *mut PyObject, index: isize) -> *mut PyObject {
    unsafe { bytes_item_slot(object, index) }
}

unsafe extern "C" fn bytearray_subscript_slot(object: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    if unsafe { crate::types::dict::type_name(key) } == Some("slice") {
        match bytes_slice_object(object, key, true) {
            Ok(value) => value,
            Err(message) => super::return_null_with_error(message),
        }
    } else {
        match str_index_value(key).and_then(|index| bytes_item_object(object, index)) {
            Ok(value) => value,
            Err(message) => super::return_null_with_error(message),
        }
    }
}

unsafe extern "C" fn bytearray_ass_item_slot(object: *mut PyObject, index: isize, value: *mut PyObject) -> c_int {
    match expect_byte(value).and_then(|byte| bytearray_object_mut(object).and_then(|array| bytearray_type::set_index(array, index, byte))) {
        Ok(()) => 0,
        Err(message) => super::return_minus_one_with_error(message),
    }
}

unsafe extern "C" fn bytearray_ass_subscript_slot(object: *mut PyObject, key: *mut PyObject, value: *mut PyObject) -> c_int {
    if unsafe { crate::types::dict::type_name(key) } == Some("slice") {
        let replacement = match expect_bytes_like(value) {
            Ok(bytes) => bytes,
            Err(message) => return super::return_minus_one_with_error(message),
        };
        match bytearray_assign_slice(object, key, &replacement) {
            Ok(()) => 0,
            Err(message) => super::return_minus_one_with_error(message),
        }
    } else {
        unsafe { bytearray_ass_item_slot(object, match str_index_value(key) { Ok(index) => index, Err(message) => return super::return_minus_one_with_error(message) }, value) }
    }
}

unsafe extern "C" fn bytearray_repeat_slot(object: *mut PyObject, count_object: *mut PyObject) -> *mut PyObject {
    match str_long_value(count_object).and_then(|value| isize::try_from(value).map_err(|_| "repeat count is out of range".to_owned())) {
        Ok(count) => unsafe { pon_bytearray_repeat(object, count) },
        Err(message) => super::return_null_with_error(message),
    }
}

unsafe extern "C" fn memoryview_len_slot(object: *mut PyObject) -> isize {
    if object.is_null() || !memoryview_type::is_memoryview_type(unsafe { (*object).ob_type }) {
        return -1;
    }
    isize::try_from(unsafe { (*object.cast::<memoryview_type::PyMemoryView>()).len }).unwrap_or(isize::MAX)
}

unsafe extern "C" fn memoryview_item_slot(object: *mut PyObject, index: isize) -> *mut PyObject {
    match memoryview_item_object(object, index) {
        Ok(value) => value,
        Err(message) => super::return_null_with_error(message),
    }
}

unsafe extern "C" fn memoryview_subscript_slot(object: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    if unsafe { crate::types::dict::type_name(key) } == Some("slice") {
        match memoryview_slice_object(object, key) {
            Ok(value) => value,
            Err(message) => super::return_null_with_error(message),
        }
    } else {
        match str_index_value(key).and_then(|index| memoryview_item_object(object, index)) {
            Ok(value) => value,
            Err(message) => super::return_null_with_error(message),
        }
    }
}

unsafe extern "C" fn memoryview_ass_item_slot(object: *mut PyObject, index: isize, value: *mut PyObject) -> c_int {
    match expect_byte(value).and_then(|byte| memoryview_set_index(object, index, byte)) {
        Ok(()) => 0,
        Err(message) => super::return_minus_one_with_error(message),
    }
}

unsafe extern "C" fn memoryview_ass_subscript_slot(object: *mut PyObject, key: *mut PyObject, value: *mut PyObject) -> c_int {
    if unsafe { crate::types::dict::type_name(key) } == Some("slice") {
        let replacement = match expect_bytes_like(value) {
            Ok(bytes) => bytes,
            Err(message) => return super::return_minus_one_with_error(message),
        };
        match memoryview_assign_slice(object, key, &replacement) {
            Ok(()) => 0,
            Err(message) => super::return_minus_one_with_error(message),
        }
    } else {
        unsafe { memoryview_ass_item_slot(object, match str_index_value(key) { Ok(index) => index, Err(message) => return super::return_minus_one_with_error(message) }, value) }
    }
}

unsafe extern "C" fn str_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        return super::return_null_with_error("str attribute name must be str");
    };
    match name {
        "split" => bound_str_method(object, name, str_split_entry),
        "rsplit" => bound_str_method(object, name, str_rsplit_entry),
        "splitlines" => bound_str_method(object, name, str_splitlines_entry),
        "join" => bound_str_method(object, name, str_join_entry),
        "replace" => bound_str_method(object, name, str_replace_entry),
        "find" => bound_str_method(object, name, str_find_entry),
        "rfind" => bound_str_method(object, name, str_rfind_entry),
        "index" => bound_str_method(object, name, str_index_entry),
        "rindex" => bound_str_method(object, name, str_rindex_entry),
        "count" => bound_str_method(object, name, str_count_entry),
        "startswith" => bound_str_method(object, name, str_startswith_entry),
        "endswith" => bound_str_method(object, name, str_endswith_entry),
        "strip" => bound_str_method(object, name, str_strip_entry),
        "lstrip" => bound_str_method(object, name, str_lstrip_entry),
        "rstrip" => bound_str_method(object, name, str_rstrip_entry),
        "lower" => bound_str_method(object, name, str_lower_entry),
        "upper" => bound_str_method(object, name, str_upper_entry),
        "title" => bound_str_method(object, name, str_title_entry),
        "capitalize" => bound_str_method(object, name, str_capitalize_entry),
        "casefold" => bound_str_method(object, name, str_casefold_entry),
        "swapcase" => bound_str_method(object, name, str_swapcase_entry),
        "center" => bound_str_method(object, name, str_center_entry),
        "ljust" => bound_str_method(object, name, str_ljust_entry),
        "rjust" => bound_str_method(object, name, str_rjust_entry),
        "zfill" => bound_str_method(object, name, str_zfill_entry),
        "expandtabs" => bound_str_method(object, name, str_expandtabs_entry),
        "partition" => bound_str_method(object, name, str_partition_entry),
        "rpartition" => bound_str_method(object, name, str_rpartition_entry),
        "encode" => bound_str_method(object, name, str_encode_entry),
        "removeprefix" => bound_str_method(object, name, str_removeprefix_entry),
        "removesuffix" => bound_str_method(object, name, str_removesuffix_entry),
        "isdecimal" => bound_str_method(object, name, str_isdecimal_entry),
        "isdigit" => bound_str_method(object, name, str_isdigit_entry),
        "isnumeric" => bound_str_method(object, name, str_isnumeric_entry),
        "isalpha" => bound_str_method(object, name, str_isalpha_entry),
        "isalnum" => bound_str_method(object, name, str_isalnum_entry),
        "isspace" => bound_str_method(object, name, str_isspace_entry),
        "isupper" => bound_str_method(object, name, str_isupper_entry),
        "islower" => bound_str_method(object, name, str_islower_entry),
        "istitle" => bound_str_method(object, name, str_istitle_entry),
        "isidentifier" => bound_str_method(object, name, str_isidentifier_entry),
        "isprintable" => bound_str_method(object, name, str_isprintable_entry),
        "isascii" => bound_str_method(object, name, str_isascii_entry),
        "translate" => bound_str_method(object, name, str_translate_entry),
        "maketrans" => bound_str_method(object, name, str_maketrans_entry),
        "format_map" => bound_str_method(object, name, str_format_map_entry),
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

macro_rules! str_entry {
    ($func:ident, $id:ident) => {
        unsafe extern "C" fn $func(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
            unsafe { str_method_entry($id, argv, argc) }
        }
    };
}

str_entry!(str_split_entry, STR_METHOD_SPLIT);
str_entry!(str_join_entry, STR_METHOD_JOIN);
str_entry!(str_replace_entry, STR_METHOD_REPLACE);
str_entry!(str_find_entry, STR_METHOD_FIND);
str_entry!(str_startswith_entry, STR_METHOD_STARTSWITH);
str_entry!(str_endswith_entry, STR_METHOD_ENDSWITH);
str_entry!(str_strip_entry, STR_METHOD_STRIP);
str_entry!(str_lower_entry, STR_METHOD_LOWER);
str_entry!(str_upper_entry, STR_METHOD_UPPER);
str_entry!(str_title_entry, STR_METHOD_TITLE);
str_entry!(str_encode_entry, STR_METHOD_ENCODE);
str_entry!(str_rsplit_entry, STR_METHOD_RSPLIT);
str_entry!(str_splitlines_entry, STR_METHOD_SPLITLINES);
str_entry!(str_lstrip_entry, STR_METHOD_LSTRIP);
str_entry!(str_rstrip_entry, STR_METHOD_RSTRIP);
str_entry!(str_rfind_entry, STR_METHOD_RFIND);
str_entry!(str_index_entry, STR_METHOD_INDEX);
str_entry!(str_rindex_entry, STR_METHOD_RINDEX);
str_entry!(str_count_entry, STR_METHOD_COUNT);
str_entry!(str_capitalize_entry, STR_METHOD_CAPITALIZE);
str_entry!(str_casefold_entry, STR_METHOD_CASEFOLD);
str_entry!(str_swapcase_entry, STR_METHOD_SWAPCASE);
str_entry!(str_center_entry, STR_METHOD_CENTER);
str_entry!(str_ljust_entry, STR_METHOD_LJUST);
str_entry!(str_rjust_entry, STR_METHOD_RJUST);
str_entry!(str_zfill_entry, STR_METHOD_ZFILL);
str_entry!(str_expandtabs_entry, STR_METHOD_EXPANDTABS);
str_entry!(str_partition_entry, STR_METHOD_PARTITION);
str_entry!(str_rpartition_entry, STR_METHOD_RPARTITION);
str_entry!(str_removeprefix_entry, STR_METHOD_REMOVEPREFIX);
str_entry!(str_removesuffix_entry, STR_METHOD_REMOVESUFFIX);
str_entry!(str_isdecimal_entry, STR_METHOD_ISDECIMAL);
str_entry!(str_isdigit_entry, STR_METHOD_ISDIGIT);
str_entry!(str_isnumeric_entry, STR_METHOD_ISNUMERIC);
str_entry!(str_isalpha_entry, STR_METHOD_ISALPHA);
str_entry!(str_isalnum_entry, STR_METHOD_ISALNUM);
str_entry!(str_isspace_entry, STR_METHOD_ISSPACE);
str_entry!(str_isupper_entry, STR_METHOD_ISUPPER);
str_entry!(str_islower_entry, STR_METHOD_ISLOWER);
str_entry!(str_istitle_entry, STR_METHOD_ISTITLE);
str_entry!(str_isidentifier_entry, STR_METHOD_ISIDENTIFIER);
str_entry!(str_isprintable_entry, STR_METHOD_ISPRINTABLE);
str_entry!(str_isascii_entry, STR_METHOD_ISASCII);
str_entry!(str_translate_entry, STR_METHOD_TRANSLATE);
str_entry!(str_maketrans_entry, STR_METHOD_MAKETRANS);
str_entry!(str_format_map_entry, STR_METHOD_FORMAT_MAP);

macro_rules! bytes_entry {
    ($func:ident, $id:ident) => {
        unsafe extern "C" fn $func(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
            unsafe { bytes_method_entry($id, argv, argc) }
        }
    };
}

bytes_entry!(bytes_split_entry, BYTES_METHOD_SPLIT);
bytes_entry!(bytes_join_entry, BYTES_METHOD_JOIN);
bytes_entry!(bytes_replace_entry, BYTES_METHOD_REPLACE);
bytes_entry!(bytes_find_entry, BYTES_METHOD_FIND);
bytes_entry!(bytes_startswith_entry, BYTES_METHOD_STARTSWITH);
bytes_entry!(bytes_decode_entry, BYTES_METHOD_DECODE);
bytes_entry!(bytes_endswith_entry, BYTES_METHOD_ENDSWITH);
bytes_entry!(bytes_rsplit_entry, BYTES_METHOD_RSPLIT);
bytes_entry!(bytes_splitlines_entry, BYTES_METHOD_SPLITLINES);
bytes_entry!(bytes_strip_entry, BYTES_METHOD_STRIP);
bytes_entry!(bytes_lstrip_entry, BYTES_METHOD_LSTRIP);
bytes_entry!(bytes_rstrip_entry, BYTES_METHOD_RSTRIP);
bytes_entry!(bytes_rfind_entry, BYTES_METHOD_RFIND);
bytes_entry!(bytes_index_entry, BYTES_METHOD_INDEX);
bytes_entry!(bytes_rindex_entry, BYTES_METHOD_RINDEX);
bytes_entry!(bytes_count_entry, BYTES_METHOD_COUNT);
bytes_entry!(bytes_upper_entry, BYTES_METHOD_UPPER);
bytes_entry!(bytes_lower_entry, BYTES_METHOD_LOWER);
bytes_entry!(bytes_title_entry, BYTES_METHOD_TITLE);
bytes_entry!(bytes_capitalize_entry, BYTES_METHOD_CAPITALIZE);
bytes_entry!(bytes_swapcase_entry, BYTES_METHOD_SWAPCASE);
bytes_entry!(bytes_center_entry, BYTES_METHOD_CENTER);
bytes_entry!(bytes_ljust_entry, BYTES_METHOD_LJUST);
bytes_entry!(bytes_rjust_entry, BYTES_METHOD_RJUST);
bytes_entry!(bytes_zfill_entry, BYTES_METHOD_ZFILL);
bytes_entry!(bytes_expandtabs_entry, BYTES_METHOD_EXPANDTABS);
bytes_entry!(bytes_partition_entry, BYTES_METHOD_PARTITION);
bytes_entry!(bytes_rpartition_entry, BYTES_METHOD_RPARTITION);
bytes_entry!(bytes_removeprefix_entry, BYTES_METHOD_REMOVEPREFIX);
bytes_entry!(bytes_removesuffix_entry, BYTES_METHOD_REMOVESUFFIX);
bytes_entry!(bytes_isalpha_entry, BYTES_METHOD_ISALPHA);
bytes_entry!(bytes_isalnum_entry, BYTES_METHOD_ISALNUM);
bytes_entry!(bytes_isdigit_entry, BYTES_METHOD_ISDIGIT);
bytes_entry!(bytes_isspace_entry, BYTES_METHOD_ISSPACE);
bytes_entry!(bytes_isupper_entry, BYTES_METHOD_ISUPPER);
bytes_entry!(bytes_islower_entry, BYTES_METHOD_ISLOWER);
bytes_entry!(bytes_istitle_entry, BYTES_METHOD_ISTITLE);
bytes_entry!(bytes_isascii_entry, BYTES_METHOD_ISASCII);
bytes_entry!(bytes_hex_entry, BYTES_METHOD_HEX);
bytes_entry!(bytes_fromhex_entry, BYTES_METHOD_FROMHEX);
bytes_entry!(bytes_append_entry, BYTES_METHOD_APPEND);
bytes_entry!(bytes_extend_entry, BYTES_METHOD_EXTEND);
bytes_entry!(bytes_insert_entry, BYTES_METHOD_INSERT);
bytes_entry!(bytes_pop_entry, BYTES_METHOD_POP);
bytes_entry!(bytes_remove_entry, BYTES_METHOD_REMOVE);
bytes_entry!(bytes_clear_entry, BYTES_METHOD_CLEAR);

unsafe extern "C" fn memoryview_tobytes_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() {
        return super::return_null_with_error("memoryview.tobytes argv pointer is null");
    }
    if argc != 1 {
        return super::return_null_with_error("memoryview.tobytes expected no arguments");
    }
    let receiver = unsafe { *argv };
    match memoryview_bytes(receiver) {
        Ok(bytes) => as_object_ptr(bytes_type::boxed_bytes(&bytes)),
        Err(message) => super::return_null_with_error(message),
    }
}

unsafe fn bytes_method_entry(method: BytesMethodId, argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() {
        return super::return_null_with_error("bytes method argv pointer is null");
    }
    if argc == 0 {
        return super::return_null_with_error("bytes method missing receiver");
    }
    let receiver = unsafe { *argv };
    let explicit_argc = argc - 1;
    let explicit_argv = if explicit_argc == 0 { ptr::null_mut() } else { unsafe { argv.add(1) } };
    unsafe { pon_bytes_method(method, receiver, explicit_argv, explicit_argc) }
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
        if let Err(message) = install_bytes_slots() {
            return super::return_null_with_error(message);
        }
        as_object_ptr(bytearray_type::boxed_bytearray(bytes))
    })
}

/// Concatenates two boxed strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_str_concat(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(left, right);
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
    crate::untag_prelude!(value);
    super::catch_object_helper(|| match expect_str(value) {
        Ok(text) => alloc_str_object(&str_type::repeat(&text, count)),
        Err(message) => super::return_null_with_error(message),
    })
}

/// Concatenates two boxed bytes objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_bytes_concat(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(left, right);
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
    crate::untag_prelude!(value);
    super::catch_object_helper(|| match expect_bytes_like(value) {
        Ok(bytes) => as_object_ptr(bytes_type::boxed_bytes(&bytes_type::repeat(&bytes, count))),
        Err(message) => super::return_null_with_error(message),
    })
}

/// Concatenates two boxed bytearray objects and returns a bytearray.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_bytearray_concat(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(left, right);
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
    crate::untag_prelude!(value);
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
    crate::untag_prelude!(value, format_spec);
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
    crate::untag_prelude!(receiver);
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
            STR_METHOD_RSPLIT => str_rsplit_method(&receiver, args),
            STR_METHOD_SPLITLINES => str_splitlines_method(&receiver, args),
            STR_METHOD_LSTRIP => str_lstrip_method(&receiver, args),
            STR_METHOD_RSTRIP => str_rstrip_method(&receiver, args),
            STR_METHOD_RFIND => str_rfind_method(&receiver, args),
            STR_METHOD_INDEX => str_index_method(&receiver, args),
            STR_METHOD_RINDEX => str_rindex_method(&receiver, args),
            STR_METHOD_COUNT => str_count_method(&receiver, args),
            STR_METHOD_CAPITALIZE => str_capitalize_method(&receiver, args),
            STR_METHOD_CASEFOLD => str_casefold_method(&receiver, args),
            STR_METHOD_SWAPCASE => str_swapcase_method(&receiver, args),
            STR_METHOD_CENTER => str_center_method(&receiver, args),
            STR_METHOD_LJUST => str_ljust_method(&receiver, args),
            STR_METHOD_RJUST => str_rjust_method(&receiver, args),
            STR_METHOD_ZFILL => str_zfill_method(&receiver, args),
            STR_METHOD_EXPANDTABS => str_expandtabs_method(&receiver, args),
            STR_METHOD_PARTITION => str_partition_method(&receiver, args),
            STR_METHOD_RPARTITION => str_rpartition_method(&receiver, args),
            STR_METHOD_REMOVEPREFIX => str_removeprefix_method(&receiver, args),
            STR_METHOD_REMOVESUFFIX => str_removesuffix_method(&receiver, args),
            STR_METHOD_ISDECIMAL => str_predicate_method(args, str_type::is_decimal_str(&receiver)),
            STR_METHOD_ISDIGIT => str_predicate_method(args, str_type::is_digit_str(&receiver)),
            STR_METHOD_ISNUMERIC => str_predicate_method(args, str_type::is_numeric_str(&receiver)),
            STR_METHOD_ISALPHA => str_predicate_method(args, str_type::is_alpha_str(&receiver)),
            STR_METHOD_ISALNUM => str_predicate_method(args, str_type::is_alnum_str(&receiver)),
            STR_METHOD_ISSPACE => str_predicate_method(args, str_type::is_space_str(&receiver)),
            STR_METHOD_ISUPPER => str_predicate_method(args, str_type::is_upper_str(&receiver)),
            STR_METHOD_ISLOWER => str_predicate_method(args, str_type::is_lower_str(&receiver)),
            STR_METHOD_ISTITLE => str_predicate_method(args, str_type::is_title_str(&receiver)),
            STR_METHOD_ISIDENTIFIER => str_predicate_method(args, str_type::is_identifier_str(&receiver)),
            STR_METHOD_ISPRINTABLE => str_predicate_method(args, str_type::is_printable_str(&receiver)),
            STR_METHOD_ISASCII => str_predicate_method(args, str_type::is_ascii_str(&receiver)),
            STR_METHOD_TRANSLATE => str_translate_method(&receiver, args),
            STR_METHOD_MAKETRANS => str_maketrans_method(args),
            STR_METHOD_FORMAT_MAP => str_format_map_method(&receiver, args),
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
    crate::untag_prelude!(receiver);
    super::catch_object_helper(|| {
        let receiver_object = receiver;
        let Ok((receiver, mutable_receiver)) = expect_bytes_receiver(receiver) else {
            return super::return_null_with_error("bytes method receiver must be bytes-like");
        };
        let Some(args) = raw_args(argv, argc) else {
            return super::return_null_with_error("bytes method argv pointer is null");
        };
        match method {
            BYTES_METHOD_SPLIT => bytes_split_method(&receiver, args, mutable_receiver),
            BYTES_METHOD_JOIN => bytes_join_method(&receiver, args, mutable_receiver),
            BYTES_METHOD_REPLACE => bytes_replace_method(&receiver, args, mutable_receiver),
            BYTES_METHOD_FIND => bytes_find_method(&receiver, args),
            BYTES_METHOD_STARTSWITH => bytes_startswith_method(&receiver, args),
            BYTES_METHOD_DECODE => bytes_decode_method(&receiver, args),
            BYTES_METHOD_ENDSWITH => bytes_endswith_method(&receiver, args),
            BYTES_METHOD_RSPLIT => bytes_rsplit_method(&receiver, args, mutable_receiver),
            BYTES_METHOD_SPLITLINES => bytes_splitlines_method(&receiver, args, mutable_receiver),
            BYTES_METHOD_STRIP => bytes_strip_method(&receiver, args, mutable_receiver),
            BYTES_METHOD_LSTRIP => bytes_lstrip_method(&receiver, args, mutable_receiver),
            BYTES_METHOD_RSTRIP => bytes_rstrip_method(&receiver, args, mutable_receiver),
            BYTES_METHOD_RFIND => bytes_rfind_method(&receiver, args),
            BYTES_METHOD_INDEX => bytes_index_method(&receiver, args),
            BYTES_METHOD_RINDEX => bytes_rindex_method(&receiver, args),
            BYTES_METHOD_COUNT => bytes_count_method(&receiver, args),
            BYTES_METHOD_UPPER => bytes_unary_bytes_method(&receiver, args, mutable_receiver, bytes_type::upper),
            BYTES_METHOD_LOWER => bytes_unary_bytes_method(&receiver, args, mutable_receiver, bytes_type::lower),
            BYTES_METHOD_TITLE => bytes_unary_bytes_method(&receiver, args, mutable_receiver, bytes_type::title),
            BYTES_METHOD_CAPITALIZE => bytes_unary_bytes_method(&receiver, args, mutable_receiver, bytes_type::capitalize),
            BYTES_METHOD_SWAPCASE => bytes_unary_bytes_method(&receiver, args, mutable_receiver, bytes_type::swapcase),
            BYTES_METHOD_CENTER => bytes_pad_method(&receiver, args, mutable_receiver, bytes_type::center),
            BYTES_METHOD_LJUST => bytes_pad_method(&receiver, args, mutable_receiver, bytes_type::ljust),
            BYTES_METHOD_RJUST => bytes_pad_method(&receiver, args, mutable_receiver, bytes_type::rjust),
            BYTES_METHOD_ZFILL => bytes_zfill_method(&receiver, args, mutable_receiver),
            BYTES_METHOD_EXPANDTABS => bytes_expandtabs_method(&receiver, args, mutable_receiver),
            BYTES_METHOD_PARTITION => bytes_partition_method(&receiver, args, mutable_receiver),
            BYTES_METHOD_RPARTITION => bytes_rpartition_method(&receiver, args, mutable_receiver),
            BYTES_METHOD_REMOVEPREFIX => bytes_removeprefix_method(&receiver, args, mutable_receiver),
            BYTES_METHOD_REMOVESUFFIX => bytes_removesuffix_method(&receiver, args, mutable_receiver),
            BYTES_METHOD_ISALPHA => bytes_predicate_method(args, bytes_type::is_alpha(&receiver)),
            BYTES_METHOD_ISALNUM => bytes_predicate_method(args, bytes_type::is_alnum(&receiver)),
            BYTES_METHOD_ISDIGIT => bytes_predicate_method(args, bytes_type::is_digit(&receiver)),
            BYTES_METHOD_ISSPACE => bytes_predicate_method(args, bytes_type::is_space(&receiver)),
            BYTES_METHOD_ISUPPER => bytes_predicate_method(args, bytes_type::is_upper(&receiver)),
            BYTES_METHOD_ISLOWER => bytes_predicate_method(args, bytes_type::is_lower(&receiver)),
            BYTES_METHOD_ISTITLE => bytes_predicate_method(args, bytes_type::is_title(&receiver)),
            BYTES_METHOD_ISASCII => bytes_predicate_method(args, bytes_type::is_ascii(&receiver)),
            BYTES_METHOD_HEX => bytes_hex_method(&receiver, args),
            BYTES_METHOD_FROMHEX => bytes_fromhex_method(args, mutable_receiver),
            BYTES_METHOD_APPEND => bytearray_append_method(receiver_object, args),
            BYTES_METHOD_EXTEND => bytearray_extend_method(receiver_object, args),
            BYTES_METHOD_INSERT => bytearray_insert_method(receiver_object, args),
            BYTES_METHOD_POP => bytearray_pop_method(receiver_object, args),
            BYTES_METHOD_REMOVE => bytearray_remove_method(receiver_object, args),
            BYTES_METHOD_CLEAR => bytearray_clear_method(receiver_object, args),
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
    let (sep, maxsplit) = match str_split_args(args, "str.split") {
        Ok(values) => values,
        Err(message) => return super::return_null_with_error(message),
    };
    alloc_str_list(str_type::split_limited(receiver, sep.as_deref(), maxsplit))
}

fn str_rsplit_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    let (sep, maxsplit) = match str_split_args(args, "str.rsplit") {
        Ok(values) => values,
        Err(message) => return super::return_null_with_error(message),
    };
    alloc_str_list(str_type::rsplit_limited(receiver, sep.as_deref(), maxsplit))
}

fn str_splitlines_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() > 1 {
        return super::return_null_with_error("str.splitlines expected at most one argument");
    }
    let keepends = if let Some(arg) = args.first().copied() { match object_truth(arg) { Ok(value) => value, Err(message) => return super::return_null_with_error(message) } } else { false };
    alloc_str_list(str_type::splitlines(receiver, keepends))
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
    let count = match args.get(2).copied().map(str_long_value).transpose() {
        Ok(value) => value.map(|value| value as isize),
        Err(message) => return super::return_null_with_error(message),
    };
    alloc_str_object(&str_type::replace_count(receiver, &old, &new, count))
}

fn str_find_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_find_like(receiver, args, false, false)
}

fn str_rfind_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_find_like(receiver, args, true, false)
}

fn str_index_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_find_like(receiver, args, false, true)
}

fn str_rindex_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_find_like(receiver, args, true, true)
}

fn str_find_like(receiver: &str, args: &[*mut PyObject], reverse: bool, index_mode: bool) -> *mut PyObject {
    if !(1..=3).contains(&args.len()) {
        return super::return_null_with_error("str.find/index expected one to three arguments");
    }
    let needle = match expect_str(args[0]) {
        Ok(needle) => needle,
        Err(message) => return super::return_null_with_error(message),
    };
    let (start, end) = match normalize_bounds_args(&args[1..], str_type::codepoint_len(receiver)) {
        Ok(bounds) => bounds,
        Err(message) => return super::return_null_with_error(message),
    };
    let found = if reverse { str_type::rfind_range(receiver, &needle, start, end) } else { str_type::find_range(receiver, &needle, start, end) };
    match found {
        Some(index) => unsafe { super::pon_const_int(index as i64) },
        None if index_mode => super::return_null_with_error("substring not found"),
        None => unsafe { super::pon_const_int(-1) },
    }
}

fn str_count_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if !(1..=3).contains(&args.len()) {
        return super::return_null_with_error("str.count expected one to three arguments");
    }
    let needle = match expect_str(args[0]) {
        Ok(needle) => needle,
        Err(message) => return super::return_null_with_error(message),
    };
    let (start, end) = match normalize_bounds_args(&args[1..], str_type::codepoint_len(receiver)) {
        Ok(bounds) => bounds,
        Err(message) => return super::return_null_with_error(message),
    };
    unsafe { super::pon_const_int(str_type::count_range(receiver, &needle, start, end) as i64) }
}

fn str_startswith_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_affix_method(receiver, args, true)
}

fn str_endswith_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_affix_method(receiver, args, false)
}

fn str_affix_method(receiver: &str, args: &[*mut PyObject], starts: bool) -> *mut PyObject {
    if !(1..=3).contains(&args.len()) {
        return super::return_null_with_error("str.startswith/endswith expected one to three arguments");
    }
    let prefixes = match str_affix_values(args[0]) {
        Ok(prefixes) => prefixes,
        Err(message) => return super::return_null_with_error(message),
    };
    let (start, end) = match normalize_bounds_args(&args[1..], str_type::codepoint_len(receiver)) {
        Ok(bounds) => bounds,
        Err(message) => return super::return_null_with_error(message),
    };
    let result = prefixes.iter().any(|prefix| {
        if starts {
            str_type::startswith_range(receiver, prefix, start, end) == str_type::StrPredicate::True
        } else {
            str_type::endswith_range(receiver, prefix, start, end) == str_type::StrPredicate::True
        }
    });
    unsafe { super::pon_const_bool(i32::from(result)) }
}

fn str_strip_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_strip_like(receiver, args, StripKind::Both)
}

fn str_lstrip_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_strip_like(receiver, args, StripKind::Left)
}

fn str_rstrip_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_strip_like(receiver, args, StripKind::Right)
}

#[derive(Clone, Copy)]
enum StripKind { Left, Right, Both }

fn str_strip_like(receiver: &str, args: &[*mut PyObject], kind: StripKind) -> *mut PyObject {
    if args.len() > 1 {
        return super::return_null_with_error("str.strip expected at most one argument");
    }
    let chars = match optional_str_arg(args.first().copied()) {
        Ok(chars) => chars,
        Err(message) => return super::return_null_with_error(message),
    };
    let out = match kind {
        StripKind::Left => str_type::lstrip(receiver, chars.as_deref()),
        StripKind::Right => str_type::rstrip(receiver, chars.as_deref()),
        StripKind::Both => str_type::strip(receiver, chars.as_deref()),
    };
    alloc_str_object(&out)
}

fn str_lower_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_unary_text_method(args, &str_type::lower(receiver), "str.lower")
}

fn str_upper_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_unary_text_method(args, &str_type::upper(receiver), "str.upper")
}

fn str_title_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_unary_text_method(args, &str_type::title(receiver), "str.title")
}

fn str_capitalize_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_unary_text_method(args, &str_type::capitalize(receiver), "str.capitalize")
}

fn str_casefold_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_unary_text_method(args, &str_type::casefold(receiver), "str.casefold")
}

fn str_swapcase_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_unary_text_method(args, &str_type::swapcase(receiver), "str.swapcase")
}

fn str_unary_text_method(args: &[*mut PyObject], value: &str, name: &str) -> *mut PyObject {
    if !args.is_empty() {
        return super::return_null_with_error(format!("{name} expected no arguments"));
    }
    alloc_str_object(value)
}

fn str_center_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_pad_method(receiver, args, str_type::center, "str.center")
}

fn str_ljust_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_pad_method(receiver, args, str_type::ljust, "str.ljust")
}

fn str_rjust_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_pad_method(receiver, args, str_type::rjust, "str.rjust")
}

fn str_pad_method(receiver: &str, args: &[*mut PyObject], f: fn(&str, usize, char) -> String, name: &str) -> *mut PyObject {
    if !(1..=2).contains(&args.len()) {
        return super::return_null_with_error(format!("{name} expected one or two arguments"));
    }
    let width = match usize_arg(args[0], "width") {
        Ok(width) => width,
        Err(message) => return super::return_null_with_error(message),
    };
    let fill = if let Some(arg) = args.get(1).copied() {
        let fill = match expect_str(arg) { Ok(fill) => fill, Err(message) => return super::return_null_with_error(message) };
        let mut chars = fill.chars();
        let Some(fill) = chars.next() else { return super::return_null_with_error("fill character must be exactly one character long"); };
        if chars.next().is_some() { return super::return_null_with_error("fill character must be exactly one character long"); }
        fill
    } else { ' ' };
    alloc_str_object(&f(receiver, width, fill))
}

fn str_zfill_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() != 1 {
        return super::return_null_with_error("str.zfill expected exactly one argument");
    }
    let width = match usize_arg(args[0], "width") { Ok(width) => width, Err(message) => return super::return_null_with_error(message) };
    alloc_str_object(&str_type::zfill(receiver, width))
}

fn str_expandtabs_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() > 1 {
        return super::return_null_with_error("str.expandtabs expected at most one argument");
    }
    let tabsize = match args.first().copied().map(str_long_value).transpose() {
        Ok(Some(value)) => value as isize,
        Ok(None) => 8,
        Err(message) => return super::return_null_with_error(message),
    };
    alloc_str_object(&str_type::expandtabs(receiver, tabsize))
}

fn str_partition_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_partition_like(receiver, args, false)
}

fn str_rpartition_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    str_partition_like(receiver, args, true)
}

fn str_partition_like(receiver: &str, args: &[*mut PyObject], reverse: bool) -> *mut PyObject {
    if args.len() != 1 {
        return super::return_null_with_error("str.partition expected exactly one argument");
    }
    let sep = match expect_str(args[0]) { Ok(sep) => sep, Err(message) => return super::return_null_with_error(message) };
    if sep.is_empty() {
        return super::return_null_with_error("empty separator");
    }
    let (a, b, c) = if reverse { str_type::rpartition(receiver, &sep) } else { str_type::partition(receiver, &sep) };
    alloc_tuple3(alloc_str_object(&a), alloc_str_object(&b), alloc_str_object(&c))
}

fn str_removeprefix_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() != 1 { return super::return_null_with_error("str.removeprefix expected exactly one argument"); }
    match expect_str(args[0]) {
        Ok(prefix) => alloc_str_object(&str_type::removeprefix(receiver, &prefix)),
        Err(message) => super::return_null_with_error(message),
    }
}

fn str_removesuffix_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() != 1 { return super::return_null_with_error("str.removesuffix expected exactly one argument"); }
    match expect_str(args[0]) {
        Ok(suffix) => alloc_str_object(&str_type::removesuffix(receiver, &suffix)),
        Err(message) => super::return_null_with_error(message),
    }
}

fn str_predicate_method(args: &[*mut PyObject], result: bool) -> *mut PyObject {
    if !args.is_empty() {
        return super::return_null_with_error("str predicate expected no arguments");
    }
    unsafe { super::pon_const_bool(i32::from(result)) }
}

fn str_translate_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() != 1 { return super::return_null_with_error("str.translate expected exactly one argument"); }
    let table = match expect_translate_table(args[0]) { Ok(table) => table, Err(message) => return super::return_null_with_error(message) };
    alloc_str_object(&str_type::translate(receiver, table))
}

fn str_maketrans_method(args: &[*mut PyObject]) -> *mut PyObject {
    if !(2..=3).contains(&args.len()) {
        return super::return_null_with_error("str.maketrans expected two or three arguments");
    }
    let from = match expect_str(args[0]) { Ok(value) => value, Err(message) => return super::return_null_with_error(message) };
    let to = match expect_str(args[1]) { Ok(value) => value, Err(message) => return super::return_null_with_error(message) };
    let delete = match args.get(2).copied().map(expect_str).transpose() { Ok(value) => value, Err(message) => return super::return_null_with_error(message) };
    let table = match str_type::maketrans(&from, &to, delete.as_deref()) { Ok(table) => table, Err(message) => return super::return_null_with_error(message) };
    as_object_ptr(Box::into_raw(Box::new(PyStrTranslateTable { ob_base: PyObjectHeader::new(str_translate_table_type()), table })))
}

fn str_format_map_method(_receiver: &str, _args: &[*mut PyObject]) -> *mut PyObject {
    super::return_null_with_error("str.format_map is deferred to the shared format-spec engine")
}

fn str_encode_method(receiver: &str, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() > 2 {
        return super::return_null_with_error("str.encode expected at most two arguments");
    }
    let encoding = match optional_str_arg(args.first().copied()) {
        Ok(Some(encoding)) => encoding,
        Ok(None) => "utf-8".to_owned(),
        Err(message) => return super::return_null_with_error(message),
    };
    let errors = match optional_str_arg(args.get(1).copied()) {
        Ok(Some(errors)) => errors,
        Ok(None) => "strict".to_owned(),
        Err(message) => return super::return_null_with_error(message),
    };
    let encoded = if encoding.eq_ignore_ascii_case("idna") {
        if errors != "strict" { return super::return_null_with_error(format!("unsupported str.encode errors handler '{errors}'")); }
        match encode_idna_ascii(receiver) { Ok(encoded) => encoded, Err(message) => return super::return_null_with_error(message) }
    } else {
        match bytes_type::encode(receiver, &encoding, &errors) { Ok(encoded) => encoded, Err(message) => return super::return_null_with_error(message) }
    };
    unsafe { pon_const_bytes(encoded.as_ptr(), encoded.len()) }
}

fn str_split_args(args: &[*mut PyObject], name: &str) -> Result<(Option<String>, isize), String> {
    if args.len() > 2 {
        return Err(format!("{name} expected at most two arguments"));
    }
    let sep = optional_str_arg(args.first().copied())?;
    if sep.as_deref() == Some("") {
        return Err("empty separator".to_owned());
    }
    let maxsplit = match args.get(1).copied() {
        Some(value) => str_long_value(value)? as isize,
        None => -1,
    };
    Ok((sep, maxsplit))
}

fn str_affix_values(value: *mut PyObject) -> Result<Vec<String>, String> {
    if unsafe { crate::types::dict::type_name(value) } == Some("tuple") {
        let tuple = unsafe { &*value.cast::<crate::types::tuple::PyTuple>() };
        let mut out = Vec::with_capacity(tuple.len);
        for item in unsafe { tuple.as_slice() } {
            out.push(expect_str(*item)?);
        }
        Ok(out)
    } else {
        Ok(vec![expect_str(value)?])
    }
}

fn normalize_bounds_args(args: &[*mut PyObject], len: usize) -> Result<(usize, usize), String> {
    if args.len() > 2 {
        return Err("too many slice bounds".to_owned());
    }
    let start = match args.first().copied() {
        Some(value) if !is_none(value) => normalize_bound_index(str_long_value(value)?, len),
        _ => 0,
    };
    let end = match args.get(1).copied() {
        Some(value) if !is_none(value) => normalize_bound_index(str_long_value(value)?, len),
        _ => len,
    };
    Ok((start, end))
}

fn normalize_bound_index(value: i64, len: usize) -> usize {
    let len_i64 = i64::try_from(len).unwrap_or(i64::MAX);
    let adjusted = if value < 0 { value.saturating_add(len_i64) } else { value };
    adjusted.clamp(0, len_i64) as usize
}

fn optional_str_arg(value: Option<*mut PyObject>) -> Result<Option<String>, String> {
    match value {
        Some(value) if !is_none(value) => expect_str(value).map(Some),
        _ => Ok(None),
    }
}

fn usize_arg(value: *mut PyObject, name: &str) -> Result<usize, String> {
    let value = str_long_value(value)?;
    if value < 0 {
        Ok(0)
    } else {
        usize::try_from(value).map_err(|_| format!("{name} is out of range"))
    }
}

fn object_truth(value: *mut PyObject) -> Result<bool, String> {
    match unsafe { super::pon_is_true(value) } {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("truth conversion failed".to_owned()),
    }
}

fn alloc_str_list(pieces: Vec<String>) -> *mut PyObject {
    let mut objects = Vec::with_capacity(pieces.len());
    for piece in pieces {
        match boxed_str(&piece) {
            Ok(object) => objects.push(object),
            Err(message) => return super::return_null_with_error(message),
        }
    }
    unsafe { super::seq::pon_build_list(objects.as_mut_ptr(), objects.len()) }
}

fn alloc_tuple3(a: *mut PyObject, b: *mut PyObject, c: *mut PyObject) -> *mut PyObject {
    if a.is_null() || b.is_null() || c.is_null() {
        return ptr::null_mut();
    }
    let mut items = [a, b, c];
    unsafe { super::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

fn expect_translate_table(value: *mut PyObject) -> Result<&'static str_type::TranslationTable, String> {
    if value.is_null() || unsafe { (*value).ob_type } != str_translate_table_type().cast_const() {
        return Err("str.translate expected a table returned by str.maketrans".to_owned());
    }
    Ok(unsafe { &(*value.cast::<PyStrTranslateTable>()).table })
}

fn encode_idna_ascii(text: &str) -> Result<Vec<u8>, String> {
    let mut out = String::new();
    for (index, label) in text.split('.').enumerate() {
        if index != 0 {
            out.push('.');
        }
        out.push_str(&encode_idna_label(label)?);
    }
    Ok(out.into_bytes())
}

fn encode_idna_label(label: &str) -> Result<String, String> {
    if label.is_ascii() {
        return Ok(label.to_owned());
    }
    Ok(format!("xn--{}", punycode_encode(label)?))
}

fn punycode_encode(input: &str) -> Result<String, String> {
    const BASE: u32 = 36;
    const TMIN: u32 = 1;
    const TMAX: u32 = 26;
    const INITIAL_BIAS: u32 = 72;
    const INITIAL_N: u32 = 128;

    let codepoints = input.chars().map(u32::from).collect::<Vec<_>>();
    let mut output = String::new();
    for ch in input.chars().filter(char::is_ascii) {
        output.push(ch);
    }

    let basic_count = output.chars().count() as u32;
    let mut handled = basic_count;
    if basic_count > 0 {
        output.push('-');
    }

    let mut n = INITIAL_N;
    let mut delta = 0u32;
    let mut bias = INITIAL_BIAS;
    let input_len = u32::try_from(codepoints.len()).map_err(|_| "idna label is too long".to_owned())?;

    while handled < input_len {
        let mut m = u32::MAX;
        for codepoint in &codepoints {
            if *codepoint >= n && *codepoint < m {
                m = *codepoint;
            }
        }
        if m == u32::MAX {
            return Err("idna punycode encoder made no progress".to_owned());
        }

        delta = delta
            .checked_add((m - n).checked_mul(handled + 1).ok_or_else(|| "idna label overflow".to_owned())?)
            .ok_or_else(|| "idna label overflow".to_owned())?;
        n = m;

        for codepoint in &codepoints {
            if *codepoint < n {
                delta = delta.checked_add(1).ok_or_else(|| "idna label overflow".to_owned())?;
            }
            if *codepoint == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = if k <= bias {
                        TMIN
                    } else if k >= bias + TMAX {
                        TMAX
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    let code = t + ((q - t) % (BASE - t));
                    output.push(encode_punycode_digit(code)?);
                    q = (q - t) / (BASE - t);
                    k = k.checked_add(BASE).ok_or_else(|| "idna label overflow".to_owned())?;
                }
                output.push(encode_punycode_digit(q)?);
                bias = adapt_punycode_bias(delta, handled + 1, handled == basic_count);
                delta = 0;
                handled += 1;
            }
        }
        delta = delta.checked_add(1).ok_or_else(|| "idna label overflow".to_owned())?;
        n = n.checked_add(1).ok_or_else(|| "idna label overflow".to_owned())?;
    }

    Ok(output)
}

fn adapt_punycode_bias(mut delta: u32, points: u32, first_time: bool) -> u32 {
    const BASE: u32 = 36;
    const TMIN: u32 = 1;
    const TMAX: u32 = 26;
    const SKEW: u32 = 38;
    const DAMP: u32 = 700;

    delta = if first_time { delta / DAMP } else { delta / 2 };
    delta += delta / points;
    let mut k = 0;
    while delta > ((BASE - TMIN) * TMAX) / 2 {
        delta /= BASE - TMIN;
        k += BASE;
    }
    k + (((BASE - TMIN + 1) * delta) / (delta + SKEW))
}

fn encode_punycode_digit(value: u32) -> Result<char, String> {
    match value {
        0..=25 => char::from_u32(u32::from(b'a') + value).ok_or_else(|| "invalid punycode digit".to_owned()),
        26..=35 => char::from_u32(u32::from(b'0') + value - 26).ok_or_else(|| "invalid punycode digit".to_owned()),
        _ => Err("invalid punycode digit".to_owned()),
    }
}

fn bytes_split_method(receiver: &[u8], args: &[*mut PyObject], mutable_receiver: bool) -> *mut PyObject {
    let (sep, maxsplit) = match bytes_split_args(args, "bytes.split") { Ok(values) => values, Err(message) => return super::return_null_with_error(message) };
    alloc_binary_list(bytes_type::split_limited(receiver, sep.as_deref(), maxsplit), mutable_receiver)
}

fn bytes_rsplit_method(receiver: &[u8], args: &[*mut PyObject], mutable_receiver: bool) -> *mut PyObject {
    let (sep, maxsplit) = match bytes_split_args(args, "bytes.rsplit") { Ok(values) => values, Err(message) => return super::return_null_with_error(message) };
    alloc_binary_list(bytes_type::rsplit_limited(receiver, sep.as_deref(), maxsplit), mutable_receiver)
}

fn bytes_splitlines_method(receiver: &[u8], args: &[*mut PyObject], mutable_receiver: bool) -> *mut PyObject {
    if args.len() > 1 { return super::return_null_with_error("bytes.splitlines expected at most one argument"); }
    let keepends = if let Some(arg) = args.first().copied() { match object_truth(arg) { Ok(value) => value, Err(message) => return super::return_null_with_error(message) } } else { false };
    alloc_binary_list(bytes_type::splitlines(receiver, keepends), mutable_receiver)
}

fn bytes_join_method(receiver: &[u8], args: &[*mut PyObject], mutable_receiver: bool) -> *mut PyObject {
    if args.len() != 1 { return super::return_null_with_error("bytes.join expected exactly one argument"); }
    let values = match super::seq::sequence_to_vec(args[0]) { Ok(values) => values, Err(message) => return super::return_null_with_error(message) };
    let mut items = Vec::with_capacity(values.len());
    for value in values {
        match expect_bytes_like(value) { Ok(item) => items.push(item), Err(_) => return super::return_null_with_error("bytes.join expected every item to be bytes-like") }
    }
    alloc_binary_object(&bytes_type::join(receiver, &items), mutable_receiver)
}

fn bytes_replace_method(receiver: &[u8], args: &[*mut PyObject], mutable_receiver: bool) -> *mut PyObject {
    if !(args.len() == 2 || args.len() == 3) { return super::return_null_with_error("bytes.replace expected two or three arguments"); }
    let (Ok(old), Ok(new)) = (expect_bytes_like(args[0]), expect_bytes_like(args[1])) else { return super::return_null_with_error("bytes.replace arguments must be bytes-like"); };
    let count = match args.get(2).copied().map(str_long_value).transpose() { Ok(value) => value.map(|v| v as isize), Err(message) => return super::return_null_with_error(message) };
    alloc_binary_object(&bytes_type::replace_count(receiver, &old, &new, count), mutable_receiver)
}

fn bytes_find_method(receiver: &[u8], args: &[*mut PyObject]) -> *mut PyObject { bytes_find_like(receiver, args, false, false) }
fn bytes_rfind_method(receiver: &[u8], args: &[*mut PyObject]) -> *mut PyObject { bytes_find_like(receiver, args, true, false) }
fn bytes_index_method(receiver: &[u8], args: &[*mut PyObject]) -> *mut PyObject { bytes_find_like(receiver, args, false, true) }
fn bytes_rindex_method(receiver: &[u8], args: &[*mut PyObject]) -> *mut PyObject { bytes_find_like(receiver, args, true, true) }

fn bytes_find_like(receiver: &[u8], args: &[*mut PyObject], reverse: bool, index_mode: bool) -> *mut PyObject {
    if !(1..=3).contains(&args.len()) { return super::return_null_with_error("bytes.find/index expected one to three arguments"); }
    let needle = match expect_bytes_like(args[0]) { Ok(needle) => needle, Err(message) => return super::return_null_with_error(message) };
    let (start, end) = match normalize_bounds_args(&args[1..], receiver.len()) { Ok(bounds) => bounds, Err(message) => return super::return_null_with_error(message) };
    let found = if reverse { bytes_type::rfind_range(receiver, &needle, start, end) } else { bytes_type::find_range(receiver, &needle, start, end) };
    match found {
        Some(index) => unsafe { super::pon_const_int(index as i64) },
        None if index_mode => super::return_null_with_error("subsection not found"),
        None => unsafe { super::pon_const_int(-1) },
    }
}

fn bytes_count_method(receiver: &[u8], args: &[*mut PyObject]) -> *mut PyObject {
    if !(1..=3).contains(&args.len()) { return super::return_null_with_error("bytes.count expected one to three arguments"); }
    let needle = match expect_bytes_like(args[0]) { Ok(needle) => needle, Err(message) => return super::return_null_with_error(message) };
    let (start, end) = match normalize_bounds_args(&args[1..], receiver.len()) { Ok(bounds) => bounds, Err(message) => return super::return_null_with_error(message) };
    unsafe { super::pon_const_int(bytes_type::count_range(receiver, &needle, start, end) as i64) }
}

fn bytes_startswith_method(receiver: &[u8], args: &[*mut PyObject]) -> *mut PyObject { bytes_affix_method(receiver, args, true) }
fn bytes_endswith_method(receiver: &[u8], args: &[*mut PyObject]) -> *mut PyObject { bytes_affix_method(receiver, args, false) }

fn bytes_affix_method(receiver: &[u8], args: &[*mut PyObject], starts: bool) -> *mut PyObject {
    if !(1..=3).contains(&args.len()) { return super::return_null_with_error("bytes.startswith/endswith expected one to three arguments"); }
    let prefixes = match bytes_affix_values(args[0]) { Ok(prefixes) => prefixes, Err(message) => return super::return_null_with_error(message) };
    let (start, end) = match normalize_bounds_args(&args[1..], receiver.len()) { Ok(bounds) => bounds, Err(message) => return super::return_null_with_error(message) };
    let result = prefixes.iter().any(|prefix| if starts { bytes_type::startswith_range(receiver, prefix, start, end) } else { bytes_type::endswith_range(receiver, prefix, start, end) });
    unsafe { super::pon_const_bool(i32::from(result)) }
}

fn bytes_decode_method(receiver: &[u8], args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() > 2 { return super::return_null_with_error("bytes.decode expected at most two arguments"); }
    let encoding = match optional_str_arg(args.first().copied()) { Ok(Some(value)) => value, Ok(None) => "utf-8".to_owned(), Err(message) => return super::return_null_with_error(message) };
    let errors = match optional_str_arg(args.get(1).copied()) { Ok(Some(value)) => value, Ok(None) => "strict".to_owned(), Err(message) => return super::return_null_with_error(message) };
    match bytes_type::decode(receiver, &encoding, &errors) { Ok(text) => alloc_str_object(&text), Err(message) => super::return_null_with_error(message) }
}

fn bytes_strip_method(receiver: &[u8], args: &[*mut PyObject], mutable_receiver: bool) -> *mut PyObject { bytes_strip_like(receiver, args, mutable_receiver, StripKind::Both) }
fn bytes_lstrip_method(receiver: &[u8], args: &[*mut PyObject], mutable_receiver: bool) -> *mut PyObject { bytes_strip_like(receiver, args, mutable_receiver, StripKind::Left) }
fn bytes_rstrip_method(receiver: &[u8], args: &[*mut PyObject], mutable_receiver: bool) -> *mut PyObject { bytes_strip_like(receiver, args, mutable_receiver, StripKind::Right) }

fn bytes_strip_like(receiver: &[u8], args: &[*mut PyObject], mutable_receiver: bool, kind: StripKind) -> *mut PyObject {
    if args.len() > 1 { return super::return_null_with_error("bytes.strip expected at most one argument"); }
    let chars = match optional_bytes_arg(args.first().copied()) { Ok(chars) => chars, Err(message) => return super::return_null_with_error(message) };
    let out = match kind { StripKind::Left => bytes_type::lstrip(receiver, chars.as_deref()), StripKind::Right => bytes_type::rstrip(receiver, chars.as_deref()), StripKind::Both => bytes_type::strip(receiver, chars.as_deref()) };
    alloc_binary_object(&out, mutable_receiver)
}

fn bytes_unary_bytes_method(receiver: &[u8], args: &[*mut PyObject], mutable_receiver: bool, f: fn(&[u8]) -> Vec<u8>) -> *mut PyObject {
    if !args.is_empty() { return super::return_null_with_error("bytes method expected no arguments"); }
    alloc_binary_object(&f(receiver), mutable_receiver)
}

fn bytes_pad_method(receiver: &[u8], args: &[*mut PyObject], mutable_receiver: bool, f: fn(&[u8], usize, u8) -> Vec<u8>) -> *mut PyObject {
    if !(1..=2).contains(&args.len()) { return super::return_null_with_error("bytes pad expected one or two arguments"); }
    let width = match usize_arg(args[0], "width") { Ok(width) => width, Err(message) => return super::return_null_with_error(message) };
    let fill = if let Some(arg) = args.get(1).copied() { match expect_single_byte(arg) { Ok(byte) => byte, Err(message) => return super::return_null_with_error(message) } } else { b' ' };
    alloc_binary_object(&f(receiver, width, fill), mutable_receiver)
}

fn bytes_zfill_method(receiver: &[u8], args: &[*mut PyObject], mutable_receiver: bool) -> *mut PyObject {
    if args.len() != 1 { return super::return_null_with_error("bytes.zfill expected exactly one argument"); }
    let width = match usize_arg(args[0], "width") { Ok(width) => width, Err(message) => return super::return_null_with_error(message) };
    alloc_binary_object(&bytes_type::zfill(receiver, width), mutable_receiver)
}

fn bytes_expandtabs_method(receiver: &[u8], args: &[*mut PyObject], mutable_receiver: bool) -> *mut PyObject {
    if args.len() > 1 { return super::return_null_with_error("bytes.expandtabs expected at most one argument"); }
    let tabsize = match args.first().copied().map(str_long_value).transpose() { Ok(Some(value)) => value as isize, Ok(None) => 8, Err(message) => return super::return_null_with_error(message) };
    alloc_binary_object(&bytes_type::expandtabs(receiver, tabsize), mutable_receiver)
}

fn bytes_partition_method(receiver: &[u8], args: &[*mut PyObject], mutable_receiver: bool) -> *mut PyObject { bytes_partition_like(receiver, args, mutable_receiver, false) }
fn bytes_rpartition_method(receiver: &[u8], args: &[*mut PyObject], mutable_receiver: bool) -> *mut PyObject { bytes_partition_like(receiver, args, mutable_receiver, true) }

fn bytes_partition_like(receiver: &[u8], args: &[*mut PyObject], mutable_receiver: bool, reverse: bool) -> *mut PyObject {
    if args.len() != 1 { return super::return_null_with_error("bytes.partition expected exactly one argument"); }
    let sep = match expect_bytes_like(args[0]) { Ok(sep) => sep, Err(message) => return super::return_null_with_error(message) };
    if sep.is_empty() { return super::return_null_with_error("empty separator"); }
    let (a, b, c) = if reverse { bytes_type::rpartition(receiver, &sep) } else { bytes_type::partition(receiver, &sep) };
    alloc_tuple3(alloc_binary_object(&a, mutable_receiver), alloc_binary_object(&b, mutable_receiver), alloc_binary_object(&c, mutable_receiver))
}

fn bytes_removeprefix_method(receiver: &[u8], args: &[*mut PyObject], mutable_receiver: bool) -> *mut PyObject {
    if args.len() != 1 { return super::return_null_with_error("bytes.removeprefix expected exactly one argument"); }
    match expect_bytes_like(args[0]) { Ok(prefix) => alloc_binary_object(&bytes_type::removeprefix(receiver, &prefix), mutable_receiver), Err(message) => super::return_null_with_error(message) }
}

fn bytes_removesuffix_method(receiver: &[u8], args: &[*mut PyObject], mutable_receiver: bool) -> *mut PyObject {
    if args.len() != 1 { return super::return_null_with_error("bytes.removesuffix expected exactly one argument"); }
    match expect_bytes_like(args[0]) { Ok(suffix) => alloc_binary_object(&bytes_type::removesuffix(receiver, &suffix), mutable_receiver), Err(message) => super::return_null_with_error(message) }
}

fn bytes_predicate_method(args: &[*mut PyObject], result: bool) -> *mut PyObject {
    if !args.is_empty() { return super::return_null_with_error("bytes predicate expected no arguments"); }
    unsafe { super::pon_const_bool(i32::from(result)) }
}

fn bytes_hex_method(receiver: &[u8], args: &[*mut PyObject]) -> *mut PyObject {
    if !args.is_empty() { return super::return_null_with_error("representative bytes.hex does not support separators"); }
    alloc_str_object(&bytes_type::hex(receiver))
}

fn bytes_fromhex_method(args: &[*mut PyObject], mutable_receiver: bool) -> *mut PyObject {
    if args.len() != 1 { return super::return_null_with_error("bytes.fromhex expected exactly one argument"); }
    let text = match expect_str(args[0]) { Ok(text) => text, Err(message) => return super::return_null_with_error(message) };
    match bytes_type::fromhex(&text) { Ok(bytes) => alloc_binary_object(&bytes, mutable_receiver), Err(message) => super::return_null_with_error(message) }
}

fn bytearray_append_method(receiver: *mut PyObject, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() != 1 { return super::return_null_with_error("bytearray.append expected exactly one argument"); }
    let byte = match expect_byte(args[0]) { Ok(byte) => byte, Err(message) => return super::return_null_with_error(message) };
    match bytearray_object_mut(receiver) { Ok(array) => bytearray_type::append(array, byte), Err(message) => return super::return_null_with_error(message) }
    unsafe { super::pon_none() }
}

fn bytearray_extend_method(receiver: *mut PyObject, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() != 1 { return super::return_null_with_error("bytearray.extend expected exactly one argument"); }
    let values = match expect_bytes_like(args[0]) { Ok(values) => values, Err(message) => return super::return_null_with_error(message) };
    match bytearray_object_mut(receiver) { Ok(array) => bytearray_type::extend(array, &values), Err(message) => return super::return_null_with_error(message) }
    unsafe { super::pon_none() }
}

fn bytearray_insert_method(receiver: *mut PyObject, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() != 2 { return super::return_null_with_error("bytearray.insert expected exactly two arguments"); }
    let index = match str_long_value(args[0]) { Ok(value) => value as isize, Err(message) => return super::return_null_with_error(message) };
    let byte = match expect_byte(args[1]) { Ok(byte) => byte, Err(message) => return super::return_null_with_error(message) };
    match bytearray_object_mut(receiver) { Ok(array) => bytearray_type::insert(array, index, byte), Err(message) => return super::return_null_with_error(message) }
    unsafe { super::pon_none() }
}

fn bytearray_pop_method(receiver: *mut PyObject, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() > 1 { return super::return_null_with_error("bytearray.pop expected at most one argument"); }
    let index = match args.first().copied().map(str_long_value).transpose() { Ok(value) => value.map(|v| v as isize), Err(message) => return super::return_null_with_error(message) };
    match bytearray_object_mut(receiver).and_then(|array| bytearray_type::pop(array, index)) { Ok(byte) => unsafe { super::pon_const_int(i64::from(byte)) }, Err(message) => super::return_null_with_error(message) }
}

fn bytearray_remove_method(receiver: *mut PyObject, args: &[*mut PyObject]) -> *mut PyObject {
    if args.len() != 1 { return super::return_null_with_error("bytearray.remove expected exactly one argument"); }
    let byte = match expect_byte(args[0]) { Ok(byte) => byte, Err(message) => return super::return_null_with_error(message) };
    match bytearray_object_mut(receiver).and_then(|array| bytearray_type::remove(array, byte)) { Ok(()) => unsafe { super::pon_none() }, Err(message) => super::return_null_with_error(message) }
}

fn bytearray_clear_method(receiver: *mut PyObject, args: &[*mut PyObject]) -> *mut PyObject {
    if !args.is_empty() { return super::return_null_with_error("bytearray.clear expected no arguments"); }
    match bytearray_object_mut(receiver) { Ok(array) => bytearray_type::clear(array), Err(message) => return super::return_null_with_error(message) }
    unsafe { super::pon_none() }
}

pub unsafe extern "C" fn builtin_bytearray(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    super::catch_object_helper(|| {
        if let Err(message) = install_bytes_slots() { return super::return_null_with_error(message); }
        let Some(args) = raw_args(argv, argc) else { return super::return_null_with_error("bytearray() received a null argv pointer"); };
        let bytes = match args.len() {
            0 => Vec::new(),
            1 => {
                if let Some(count) = object_to_i64(args[0]) {
                    if count < 0 { return super::return_null_with_error("negative count"); }
                    vec![0; count as usize]
                } else {
                    match expect_bytes_like(args[0]) { Ok(bytes) => bytes, Err(message) => return super::return_null_with_error(message) }
                }
            }
            2 | 3 => {
                let text = match expect_str(args[0]) { Ok(text) => text, Err(message) => return super::return_null_with_error(message) };
                let encoding = match expect_str(args[1]) { Ok(text) => text, Err(message) => return super::return_null_with_error(message) };
                let errors = match args.get(2).copied().map(expect_str).transpose() { Ok(Some(text)) => text, Ok(None) => "strict".to_owned(), Err(message) => return super::return_null_with_error(message) };
                match bytes_type::encode(&text, &encoding, &errors) { Ok(bytes) => bytes, Err(message) => return super::return_null_with_error(message) }
            }
            _ => return super::return_null_with_error("bytearray() expected at most three arguments"),
        };
        as_object_ptr(bytearray_type::boxed_bytearray(&bytes))
    })
}

pub unsafe extern "C" fn builtin_memoryview(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    super::catch_object_helper(|| {
        if argc != 1 { return super::return_null_with_error("memoryview() expected exactly one argument"); }
        let Some(args) = raw_args(argv, argc) else { return super::return_null_with_error("memoryview() received a null argv pointer"); };
        if let Err(message) = install_memoryview_slots() { return super::return_null_with_error(message); }
        match unsafe { memoryview_type::boxed_memoryview_from_object(args[0]) } { Ok(view) => as_object_ptr(view), Err(message) => super::return_null_with_error(message) }
    })
}

fn bytes_split_args(args: &[*mut PyObject], name: &str) -> Result<(Option<Vec<u8>>, isize), String> {
    if args.len() > 2 { return Err(format!("{name} expected at most two arguments")); }
    let sep = optional_bytes_arg(args.first().copied())?;
    if sep.as_deref() == Some(&[]) { return Err("empty separator".to_owned()); }
    let maxsplit = match args.get(1).copied() { Some(value) => str_long_value(value)? as isize, None => -1 };
    Ok((sep, maxsplit))
}

fn optional_bytes_arg(value: Option<*mut PyObject>) -> Result<Option<Vec<u8>>, String> {
    match value { Some(value) if !is_none(value) => expect_bytes_like(value).map(Some), _ => Ok(None) }
}

fn bytes_affix_values(value: *mut PyObject) -> Result<Vec<Vec<u8>>, String> {
    if unsafe { crate::types::dict::type_name(value) } == Some("tuple") {
        let tuple = unsafe { &*value.cast::<crate::types::tuple::PyTuple>() };
        let mut out = Vec::with_capacity(tuple.len);
        for item in unsafe { tuple.as_slice() } { out.push(expect_bytes_like(*item)?); }
        Ok(out)
    } else { Ok(vec![expect_bytes_like(value)?]) }
}

fn expect_bytes_receiver(value: *mut PyObject) -> Result<(Vec<u8>, bool), String> {
    if value.is_null() { return Err("expected bytes-like object, got NULL".to_owned()); }
    let ty = unsafe { (*value).ob_type };
    if bytes_type::is_bytes_type(ty) {
        let bytes = unsafe { &*value.cast::<bytes_type::PyBytes>() };
        return Ok((unsafe { bytes.as_slice() }.to_vec(), false));
    }
    if bytearray_type::is_bytearray_type(ty) {
        let bytearray = unsafe { &*value.cast::<bytearray_type::PyByteArray>() };
        return Ok((bytearray.as_slice().to_vec(), true));
    }
    Err("expected bytes or bytearray receiver".to_owned())
}

fn alloc_binary_object(bytes: &[u8], mutable: bool) -> *mut PyObject {
    if mutable { as_object_ptr(bytearray_type::boxed_bytearray(bytes)) } else { as_object_ptr(bytes_type::boxed_bytes(bytes)) }
}

fn alloc_binary_list(items: Vec<Vec<u8>>, mutable: bool) -> *mut PyObject {
    let mut objects = Vec::with_capacity(items.len());
    for item in items { objects.push(alloc_binary_object(&item, mutable)); }
    unsafe { super::seq::pon_build_list(objects.as_mut_ptr(), objects.len()) }
}

fn expect_single_byte(value: *mut PyObject) -> Result<u8, String> {
    let bytes = expect_bytes_like(value)?;
    if bytes.len() != 1 { return Err("argument must be a bytes-like object of length 1".to_owned()); }
    Ok(bytes[0])
}

fn expect_byte(value: *mut PyObject) -> Result<u8, String> {
    let value = str_long_value(value)?;
    if !(0..=255).contains(&value) { return Err("byte must be in range(0, 256)".to_owned()); }
    Ok(value as u8)
}

fn bytearray_object_mut(value: *mut PyObject) -> Result<&'static mut bytearray_type::PyByteArray, String> {
    if value.is_null() || !bytearray_type::is_bytearray_type(unsafe { (*value).ob_type }) { return Err("expected bytearray object".to_owned()); }
    Ok(unsafe { &mut *value.cast::<bytearray_type::PyByteArray>() })
}

fn normalize_byte_index(index: isize, len: usize) -> Result<usize, String> {
    let len_isize = isize::try_from(len).map_err(|_| "bytes object is too large".to_owned())?;
    let adjusted = if index < 0 { index.saturating_add(len_isize) } else { index };
    if adjusted < 0 || adjusted >= len_isize { Err("index out of range".to_owned()) } else { Ok(adjusted as usize) }
}

fn bytes_item_object(object: *mut PyObject, index: isize) -> Result<*mut PyObject, String> {
    let bytes = expect_bytes_like(object)?;
    let index = normalize_byte_index(index, bytes.len())?;
    Ok(unsafe { super::pon_const_int(i64::from(bytes[index])) })
}

fn bytes_slice_object(object: *mut PyObject, key: *mut PyObject, mutable: bool) -> Result<*mut PyObject, String> {
    let bytes = expect_bytes_like(object)?;
    let indices = normalize_str_slice(unsafe { &*key.cast::<PySlice>() }, bytes.len())?;
    let mut out = Vec::with_capacity(indices.len);
    let mut index = indices.start;
    for _ in 0..indices.len { out.push(bytes[index as usize]); index = index.saturating_add(indices.step); }
    Ok(alloc_binary_object(&out, mutable))
}

fn bytearray_assign_slice(object: *mut PyObject, key: *mut PyObject, replacement: &[u8]) -> Result<(), String> {
    let array = bytearray_object_mut(object)?;
    let indices = normalize_str_slice(unsafe { &*key.cast::<PySlice>() }, array.bytes.len())?;
    if indices.step == 1 {
        bytearray_type::set_slice(array, indices.start as usize, indices.stop as usize, replacement);
        return Ok(());
    }
    if replacement.len() != indices.len { return Err("attempt to assign bytes of different size to extended slice".to_owned()); }
    let mut index = indices.start;
    for byte in replacement { array.bytes[index as usize] = *byte; index = index.saturating_add(indices.step); }
    Ok(())
}

fn memoryview_bytes(object: *mut PyObject) -> Result<Vec<u8>, String> {
    if object.is_null() || !memoryview_type::is_memoryview_type(unsafe { (*object).ob_type }) { return Err("expected memoryview object".to_owned()); }
    Ok(unsafe { memoryview_type::tobytes(object.cast::<memoryview_type::PyMemoryView>()) })
}

fn memoryview_item_object(object: *mut PyObject, index: isize) -> Result<*mut PyObject, String> {
    if object.is_null() || !memoryview_type::is_memoryview_type(unsafe { (*object).ob_type }) { return Err("expected memoryview object".to_owned()); }
    let view = unsafe { &*object.cast::<memoryview_type::PyMemoryView>() };
    let index = normalize_byte_index(index, view.len)?;
    Ok(unsafe { super::pon_const_int(i64::from(view.as_slice()[index])) })
}

fn memoryview_slice_object(object: *mut PyObject, key: *mut PyObject) -> Result<*mut PyObject, String> {
    if let Err(message) = install_memoryview_slots() { return Err(message); }
    if object.is_null() || !memoryview_type::is_memoryview_type(unsafe { (*object).ob_type }) { return Err("expected memoryview object".to_owned()); }
    let view = unsafe { &*object.cast::<memoryview_type::PyMemoryView>() };
    let indices = normalize_str_slice(unsafe { &*key.cast::<PySlice>() }, view.len)?;
    if indices.step == 1 {
        let data = unsafe { view.data.add(indices.start as usize) };
        return Ok(as_object_ptr(memoryview_type::boxed_memoryview_from_raw(view.base, data, indices.len, view.readonly)));
    }
    let slice = unsafe { view.as_slice() };
    let mut out = Vec::with_capacity(indices.len);
    let mut index = indices.start;
    for _ in 0..indices.len { out.push(slice[index as usize]); index = index.saturating_add(indices.step); }
    let base = as_object_ptr(bytes_type::boxed_bytes(&out));
    Ok(as_object_ptr(unsafe { memoryview_type::boxed_memoryview_from_object(base)? }))
}

fn memoryview_set_index(object: *mut PyObject, index: isize, value: u8) -> Result<(), String> {
    if object.is_null() || !memoryview_type::is_memoryview_type(unsafe { (*object).ob_type }) { return Err("expected memoryview object".to_owned()); }
    let view = unsafe { &mut *object.cast::<memoryview_type::PyMemoryView>() };
    let index = normalize_byte_index(index, view.len)?;
    let bytes = unsafe { view.as_mut_slice()? };
    bytes[index] = value;
    Ok(())
}

fn memoryview_assign_slice(object: *mut PyObject, key: *mut PyObject, replacement: &[u8]) -> Result<(), String> {
    if object.is_null() || !memoryview_type::is_memoryview_type(unsafe { (*object).ob_type }) { return Err("expected memoryview object".to_owned()); }
    let view = unsafe { &mut *object.cast::<memoryview_type::PyMemoryView>() };
    let indices = normalize_str_slice(unsafe { &*key.cast::<PySlice>() }, view.len)?;
    if replacement.len() != indices.len { return Err("memoryview assignment length mismatch".to_owned()); }
    let bytes = unsafe { view.as_mut_slice()? };
    let mut index = indices.start;
    for byte in replacement { bytes[index as usize] = *byte; index = index.saturating_add(indices.step); }
    Ok(())
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
    if memoryview_type::is_memoryview_type(ty) {
        return memoryview_bytes(value);
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
    use crate::thread_state::{pon_err_clear, pon_err_message, test_state_lock};
    use crate::types::bytes_::PyBytes;

    fn str_object(text: &str) -> *mut PyObject {
        unsafe { super::super::pon_const_str(text.as_ptr(), text.len()) }
    }

    fn encode_bytes(text: &str, encoding: Option<&str>) -> Vec<u8> {
        unsafe {
            let receiver = str_object(text);
            assert!(!receiver.is_null(), "failed to allocate str receiver");
            let mut args = Vec::new();
            if let Some(encoding) = encoding {
                let encoding = str_object(encoding);
                assert!(!encoding.is_null(), "failed to allocate str.encode encoding");
                args.push(encoding);
            }
            let argv = if args.is_empty() { ptr::null_mut() } else { args.as_mut_ptr() };
            pon_err_clear();
            let encoded = pon_str_method(STR_METHOD_ENCODE, receiver, argv, args.len());
            assert!(
                !encoded.is_null(),
                "str.encode({encoding:?}) failed for {text:?}: {:?}",
                pon_err_message()
            );
            (&*encoded.cast::<PyBytes>()).as_slice().to_vec()
        }
    }

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

    #[test]
    fn str_encode_emits_utf8_ascii_and_idna_bytes() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(super::super::pon_runtime_init(), 0);
        }

        assert_eq!(encode_bytes("ä", Some("idna")), b"xn--4ca");
        assert_eq!(encode_bytes("Grüße", None), "Grüße".as_bytes());
        assert_eq!(encode_bytes("plain-ascii", Some("ascii")), b"plain-ascii");
    }

    #[test]
    fn str_encode_ascii_rejects_non_ascii_text() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(super::super::pon_runtime_init(), 0);
            let receiver = str_object("ä");
            let mut args = [str_object("ascii")];
            pon_err_clear();
            let encoded = pon_str_method(STR_METHOD_ENCODE, receiver, args.as_mut_ptr(), args.len());
            assert!(encoded.is_null());
            assert_eq!(
                pon_err_message().as_deref(),
                Some("ascii codec can't encode non-ascii character")
            );
            pon_err_clear();
        }
    }
}
