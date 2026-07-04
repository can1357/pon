//! Minimal native `zlib` shim for stdlib imports.
//!
//! The vendored `gzip` module imports `zlib` at module load and binds a small
//! constant/function surface into class definitions. Pon serves that import with
//! a native module so pure-Python packages such as `mesonpy` can import their
//! stdlib helpers. Only `crc32` is implemented today; compression entry points
//! stay loud `NotImplementedError`s until a concrete caller needs them.

use core::mem;
use std::sync::LazyLock;

use num_traits::ToPrimitive;

use crate::intern::intern;
use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::types::exc::ExceptionKind;

use super::builtins_mod::VARIADIC_ARITY;
use super::install_module;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

const DEFLATED: i64 = 8;
const MAX_WBITS: i64 = 15;
const DEF_MEM_LEVEL: i64 = 8;
const DEF_BUF_SIZE: i64 = 16_384;
const Z_BEST_COMPRESSION: i64 = 9;
const Z_BEST_SPEED: i64 = 1;
const Z_BLOCK: i64 = 5;
const Z_DEFAULT_COMPRESSION: i64 = -1;
const Z_DEFAULT_STRATEGY: i64 = 0;
const Z_FILTERED: i64 = 1;
const Z_FINISH: i64 = 4;
const Z_FIXED: i64 = 4;
const Z_FULL_FLUSH: i64 = 3;
const Z_HUFFMAN_ONLY: i64 = 2;
const Z_NO_COMPRESSION: i64 = 0;
const Z_NO_FLUSH: i64 = 0;
const Z_PARTIAL_FLUSH: i64 = 1;
const Z_RLE: i64 = 3;
const Z_SYNC_FLUSH: i64 = 2;
const Z_TREES: i64 = 6;
const ZLIB_VERSION: &str = "1.2.12";
const PY_ZLIB_VERSION: &str = "1.0";
static CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut index = 0usize;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 0 {
                value >> 1
            } else {
                0xEDB8_8320u32 ^ (value >> 1)
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
};

fn crc32_core(data: &[u8], crc: u32) -> u32 {
    let mut value = !crc;
    for byte in data {
        let index = ((value ^ u32::from(*byte)) & 0xff) as usize;
        value = CRC32_TABLE[index] ^ (value >> 8);
    }
    !value
}

fn adler32_core(data: &[u8], adler: u32) -> u32 {
    const BASE: u32 = 65_521;
    let mut s1 = adler & 0xffff;
    let mut s2 = (adler >> 16) & 0xffff;
    for chunk in data.chunks(5_552) {
        for &byte in chunk {
            s1 += u32::from(byte);
            s2 += s1;
        }
        s1 %= BASE;
        s2 %= BASE;
    }
    (s2 << 16) | s1
}

fn zlib_decompressor_type() -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let type_type = crate::abi::runtime_type_type();
        Box::into_raw(Box::new(PyType::new(
            type_type.cast_const(),
            "_ZlibDecompressor",
            mem::size_of::<PyObjectHeader>(),
        ))) as usize
    });
    *TYPE as *mut PyType
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "zlib";
    let name_object = unsafe { crate::abi::pon_const_str(name.as_ptr(), name.len()) };
    if name_object.is_null() {
        return Err("failed to allocate zlib.__name__".to_owned());
    }
    let error = crate::import::module_attr(intern("builtins"), intern("RuntimeError"))
        .ok_or_else(|| "builtins.RuntimeError is not available for zlib.error".to_owned())?;
    let mut attrs = vec![
        (intern("__name__"), name_object),
        (intern("error"), error),
        (intern("_ZlibDecompressor"), zlib_decompressor_type().cast::<PyObject>()),
    ];
    for &(const_name, value) in &[
        ("DEFLATED", DEFLATED),
        ("DEF_BUF_SIZE", DEF_BUF_SIZE),
        ("DEF_MEM_LEVEL", DEF_MEM_LEVEL),
        ("MAX_WBITS", MAX_WBITS),
        ("Z_BEST_COMPRESSION", Z_BEST_COMPRESSION),
        ("Z_BEST_SPEED", Z_BEST_SPEED),
        ("Z_BLOCK", Z_BLOCK),
        ("Z_DEFAULT_COMPRESSION", Z_DEFAULT_COMPRESSION),
        ("Z_DEFAULT_STRATEGY", Z_DEFAULT_STRATEGY),
        ("Z_FILTERED", Z_FILTERED),
        ("Z_FINISH", Z_FINISH),
        ("Z_FIXED", Z_FIXED),
        ("Z_FULL_FLUSH", Z_FULL_FLUSH),
        ("Z_HUFFMAN_ONLY", Z_HUFFMAN_ONLY),
        ("Z_NO_COMPRESSION", Z_NO_COMPRESSION),
        ("Z_NO_FLUSH", Z_NO_FLUSH),
        ("Z_PARTIAL_FLUSH", Z_PARTIAL_FLUSH),
        ("Z_RLE", Z_RLE),
        ("Z_SYNC_FLUSH", Z_SYNC_FLUSH),
        ("Z_TREES", Z_TREES),
    ] {
        let object = unsafe { crate::abi::pon_const_int(value) };
        if object.is_null() {
            return Err(format!("failed to allocate zlib.{const_name}"));
        }
        attrs.push((intern(const_name), object));
    }
    for &(const_name, value) in &[
        ("ZLIB_VERSION", ZLIB_VERSION),
        ("ZLIB_RUNTIME_VERSION", ZLIB_VERSION),
        ("__version__", PY_ZLIB_VERSION),
    ] {
        let object = unsafe { crate::abi::pon_const_str(value.as_ptr(), value.len()) };
        if object.is_null() {
            return Err(format!("failed to allocate zlib.{const_name}"));
        }
        attrs.push((intern(const_name), object));
    }
    for &(function_name, entry) in &[
        ("adler32", adler32_entry as BuiltinFn),
        ("compress", compress_entry as BuiltinFn),
        ("compressobj", compressobj_entry as BuiltinFn),
        ("crc32", crc32_entry as BuiltinFn),
        ("decompress", decompress_entry as BuiltinFn),
        ("decompressobj", decompressobj_entry as BuiltinFn),
    ] {
        let function = unsafe { crate::abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(function_name)) };
        if function.is_null() {
            return Err(format!("failed to allocate zlib.{function_name}"));
        }
        attrs.push((intern(function_name), function));
    }
    install_module(name, attrs)
}

fn raise_not_implemented(name: &str) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(ExceptionKind::NotImplementedError, &format!("zlib.{name} is not implemented yet"))
}

unsafe fn arg_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
    if argc == 0 {
        return Some(&[]);
    }
    if argv.is_null() {
        return None;
    }
    Some(unsafe { core::slice::from_raw_parts(argv, argc) })
}

unsafe fn bytes_arg<'a>(object: *mut PyObject) -> Option<&'a [u8]> {
    let object = crate::tag::untag_arg(object);
    if object.is_null() {
        return None;
    }
    let ty = unsafe { (*object).ob_type };
    if crate::types::bytes_::is_bytes_type(ty) {
        return Some(unsafe { (*object.cast::<crate::types::bytes_::PyBytes>()).as_slice() });
    }
    if crate::types::bytearray_::is_bytearray_type(ty) {
        return Some(unsafe { (*object.cast::<crate::types::bytearray_::PyByteArray>()).as_slice() });
    }
    None
}

unsafe extern "C" fn crc32_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return crate::abi::return_null_with_error("zlib.crc32 received a null argv pointer");
    };
    if !(1..=2).contains(&args.len()) {
        return crate::abi::return_null_with_error(&format!("crc32() takes 1 or 2 arguments ({} given)", args.len()));
    }
    let Some(data) = (unsafe { bytes_arg(args[0]) }) else {
        return crate::abi::exc::raise_kind_error_text(ExceptionKind::TypeError, "crc32() argument 1 must be bytes-like");
    };
    let seed = if args.len() == 2 {
        match unsafe { crate::types::int::to_bigint_including_bool(crate::tag::untag_arg(args[1])) }
            .and_then(|value| value.to_u32())
        {
            Some(value) => value,
            None => {
                return crate::abi::exc::raise_kind_error_text(
                    ExceptionKind::TypeError,
                    "crc32() argument 2 must be an integer",
                );
            }
        }
    } else {
        0
    };
    unsafe { crate::abi::pon_const_int(i64::from(crc32_core(data, seed))) }
}

unsafe extern "C" fn adler32_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return crate::abi::return_null_with_error("zlib.adler32 received a null argv pointer");
    };
    if !(1..=2).contains(&args.len()) {
        return crate::abi::return_null_with_error(&format!("adler32() takes 1 or 2 arguments ({} given)", args.len()));
    }
    let Some(data) = (unsafe { bytes_arg(args[0]) }) else {
        return crate::abi::exc::raise_kind_error_text(ExceptionKind::TypeError, "adler32() argument 1 must be bytes-like");
    };
    let seed = if args.len() == 2 {
        match unsafe { crate::types::int::to_bigint_including_bool(crate::tag::untag_arg(args[1])) }
            .and_then(|value| value.to_u32())
        {
            Some(value) => value,
            None => {
                return crate::abi::exc::raise_kind_error_text(
                    ExceptionKind::TypeError,
                    "adler32() argument 2 must be an integer",
                );
            }
        }
    } else {
        1
    };
    unsafe { crate::abi::pon_const_int(i64::from(adler32_core(data, seed))) }
}

unsafe extern "C" fn compress_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    raise_not_implemented("compress")
}

unsafe extern "C" fn compressobj_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    raise_not_implemented("compressobj")
}

unsafe extern "C" fn decompress_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    raise_not_implemented("decompress")
}

unsafe extern "C" fn decompressobj_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    raise_not_implemented("decompressobj")
}
