//! Native `zlib` shim for stdlib imports.
//!
//! The vendored `gzip` module imports `zlib` at module load and binds a small
//! constant/function surface into class definitions.  `crc32`/`adler32` are
//! pure-Rust; `compress`/`decompress` run on `flate2` (Cython's Code.py
//! compresses its C string tables with `zlib.compress(..., level=9)`).  The
//! streaming `compressobj`/`decompressobj` surface stays a loud
//! `NotImplementedError` until a concrete caller needs it.

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
    let error = zlib_error_class();
    if error.is_null() {
        return Err("failed to build zlib.error".to_owned());
    }
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

/// `zlib.compress(data, /, level=-1, wbits=MAX_WBITS)` — the keyword binder
/// delivers the canonical `[data, level, wbits]` layout (absent slots None).
unsafe extern "C" fn compress_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return raise_type_error("compress received an invalid argument window");
    };
    if args.is_empty() {
        return raise_type_error("compress() missing required argument 'data' (pos 1)");
    }
    let Ok(data) = crate::abi::str_::expect_bytes_like(crate::tag::untag_arg(args[0])) else {
        let got = unsafe { crate::types::dict::type_name(crate::tag::untag_arg(args[0])) }.unwrap_or("object");
        return raise_type_error(&format!("a bytes-like object is required, not '{got}'"));
    };
    let level = match optional_i64(args.get(1).copied(), "level") {
        Ok(value) => value.unwrap_or(Z_DEFAULT_COMPRESSION),
        Err(raised) => return raised,
    };
    if !(level == Z_DEFAULT_COMPRESSION || (0..=9).contains(&level)) {
        return raise_zlib_error("Bad compression level");
    }
    let wbits = match optional_i64(args.get(2).copied(), "wbits") {
        Ok(value) => value.unwrap_or(MAX_WBITS),
        Err(raised) => return raised,
    };
    let level = if level == Z_DEFAULT_COMPRESSION {
        flate2::Compression::default()
    } else {
        flate2::Compression::new(level as u32)
    };
    use std::io::Write;
    let out = match wbits {
        9..=15 => {
            let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), level);
            encoder.write_all(&data).and_then(|()| encoder.finish())
        }
        -15..=-9 => {
            let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), level);
            encoder.write_all(&data).and_then(|()| encoder.finish())
        }
        25..=31 => {
            let mut encoder = flate2::write::GzEncoder::new(Vec::new(), level);
            encoder.write_all(&data).and_then(|()| encoder.finish())
        }
        _ => return raise_zlib_error(&format!("Invalid initialization option: wbits={wbits}")),
    };
    match out {
        Ok(bytes) => alloc_bytes(&bytes),
        Err(error) => raise_zlib_error(&format!("Error {error} while compressing data")),
    }
}

/// Window-bits interpretation shared by `compressobj`/`decompressobj`:
/// negative -> raw deflate, 9..=15 -> zlib wrapper, 25..=31 -> gzip wrapper.
#[derive(Clone, Copy, PartialEq)]
enum Window {
	Raw,
	Zlib,
	Gzip,
}

fn window_from_wbits(wbits: i64) -> Option<Window> {
	match wbits {
		-15..=-9 => Some(Window::Raw),
		9..=15 => Some(Window::Zlib),
		25..=31 => Some(Window::Gzip),
		_ => None,
	}
}

/// `zlib.compressobj(...)` product: a streaming deflate encoder.
#[repr(C)]
struct PyZlibCompressor {
	ob_base: PyObjectHeader,
	stream:  std::sync::Mutex<flate2::Compress>,
}

/// `zlib.decompressobj(...)` product: a streaming inflate decoder with the
/// CPython-visible `eof`/`unconsumed_tail` state.
#[repr(C)]
struct PyZlibDecompressorObj {
	ob_base: PyObjectHeader,
	state:   std::sync::Mutex<InflateState>,
}

struct InflateState {
	stream:     flate2::Decompress,
	unconsumed: Vec<u8>,
	eof:        bool,
}

fn zlib_compressor_type() -> *mut PyType {
	static TYPE: LazyLock<usize> = LazyLock::new(|| {
		let type_type = crate::abi::runtime_type_type();
		let mut ty = PyType::new(type_type.cast_const(), "zlib.Compress", mem::size_of::<PyZlibCompressor>());
		ty.tp_getattro = Some(compressor_getattro);
		Box::into_raw(Box::new(ty)) as usize
	});
	*TYPE as *mut PyType
}

fn zlib_decompressorobj_type() -> *mut PyType {
	static TYPE: LazyLock<usize> = LazyLock::new(|| {
		let type_type = crate::abi::runtime_type_type();
		let mut ty = PyType::new(type_type.cast_const(), "zlib.Decompress", mem::size_of::<PyZlibDecompressorObj>());
		ty.tp_getattro = Some(decompressor_getattro);
		Box::into_raw(Box::new(ty)) as usize
	});
	*TYPE as *mut PyType
}

unsafe fn compressor_ref<'a>(object: *mut PyObject) -> Option<&'a PyZlibCompressor> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || !crate::tag::is_heap(object) {
		return None;
	}
	// SAFETY: heap-tagged object with a readable header.
	if unsafe { (*object).ob_type } != zlib_compressor_type().cast_const() {
		return None;
	}
	Some(unsafe { &*object.cast::<PyZlibCompressor>() })
}

unsafe fn decompressor_ref<'a>(object: *mut PyObject) -> Option<&'a PyZlibDecompressorObj> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || !crate::tag::is_heap(object) {
		return None;
	}
	// SAFETY: heap-tagged object with a readable header.
	if unsafe { (*object).ob_type } != zlib_decompressorobj_type().cast_const() {
		return None;
	}
	Some(unsafe { &*object.cast::<PyZlibDecompressorObj>() })
}

fn bound_zlib_method(
	receiver: *mut PyObject,
	name: &str,
	entry: BuiltinFn,
) -> *mut PyObject {
	let function = unsafe { crate::abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	if function.is_null() {
		return core::ptr::null_mut();
	}
	match crate::types::method::new_bound_method(function, receiver) {
		Ok(method) => method.cast::<PyObject>(),
		Err(message) => raise_type_error(&message),
	}
}

fn raise_attribute(name: &str) -> *mut PyObject {
	crate::abi::exc::raise_kind_error_text(ExceptionKind::AttributeError, &format!("no attribute '{name}'"))
}

unsafe extern "C" fn compressor_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let Some(name) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		return raise_type_error("attribute name must be str");
	};
	match name {
		"compress" => bound_zlib_method(object, name, compressor_compress_entry),
		"flush" => bound_zlib_method(object, name, compressor_flush_entry),
		_ => raise_attribute(name),
	}
}

unsafe extern "C" fn decompressor_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let Some(name) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		return raise_type_error("attribute name must be str");
	};
	let Some(decompressor) = (unsafe { decompressor_ref(object) }) else {
		return raise_type_error("receiver is not a zlib decompressor");
	};
	match name {
		"decompress" => bound_zlib_method(object, name, decompressor_decompress_entry),
		"flush" => bound_zlib_method(object, name, decompressor_flush_entry),
		"eof" => {
			let state = decompressor.state.lock().unwrap_or_else(|poison| poison.into_inner());
			// SAFETY: Singleton bool constants.
			unsafe { crate::abi::pon_const_bool(i32::from(state.eof)) }
		},
		"unconsumed_tail" => {
			let state = decompressor.state.lock().unwrap_or_else(|poison| poison.into_inner());
			alloc_bytes(&state.unconsumed)
		},
		"unused_data" => alloc_bytes(&[]),
		_ => raise_attribute(name),
	}
}

/// Streams `input` through `stream` into a growing buffer. `flush` follows
/// flate2 semantics (`None` for incremental feed, `Finish` to terminate).
fn deflate_chunks(
	stream: &mut flate2::Compress,
	input: &[u8],
	flush: flate2::FlushCompress,
) -> Result<Vec<u8>, String> {
	let mut out = Vec::new();
	let mut scratch = vec![0u8; 64 * 1024];
	let mut consumed = 0usize;
	loop {
		let before_in = stream.total_in();
		let before_out = stream.total_out();
		let status = stream
			.compress(&input[consumed..], &mut scratch, flush)
			.map_err(|error| format!("Error {error} while compressing data"))?;
		consumed += (stream.total_in() - before_in) as usize;
		let produced = (stream.total_out() - before_out) as usize;
		out.extend_from_slice(&scratch[..produced]);
		match status {
			flate2::Status::StreamEnd => break,
			_ if flush == flate2::FlushCompress::None && consumed == input.len() && produced == 0 => break,
			_ => {},
		}
	}
	Ok(out)
}

unsafe extern "C" fn compressor_compress_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return raise_type_error("compress received an invalid argument window");
	};
	if args.len() != 2 {
		return raise_type_error("compress() takes exactly one argument");
	}
	let Some(compressor) = (unsafe { compressor_ref(args[0]) }) else {
		return raise_type_error("receiver is not a zlib compressor");
	};
	let Some(data) = (unsafe { bytes_arg(args[1]) }) else {
		let got = unsafe { crate::types::dict::type_name(crate::tag::untag_arg(args[1])) }.unwrap_or("object");
		return raise_type_error(&format!("a bytes-like object is required, not '{got}'"));
	};
	let mut stream = compressor.stream.lock().unwrap_or_else(|poison| poison.into_inner());
	match deflate_chunks(&mut stream, data, flate2::FlushCompress::None) {
		Ok(bytes) => alloc_bytes(&bytes),
		Err(message) => raise_zlib_error(&message),
	}
}

unsafe extern "C" fn compressor_flush_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return raise_type_error("flush received an invalid argument window");
	};
	if args.is_empty() || args.len() > 2 {
		return raise_type_error("flush() takes at most one argument");
	}
	let Some(compressor) = (unsafe { compressor_ref(args[0]) }) else {
		return raise_type_error("receiver is not a zlib compressor");
	};
	// Mode argument: only Z_FINISH (the default) terminates; other modes are
	// accepted and treated as a full sync flush of pending output.
	let mode = match optional_i64(args.get(1).copied(), "mode") {
		Ok(value) => value.unwrap_or(Z_FINISH),
		Err(raised) => return raised,
	};
	let flush = if mode == Z_FINISH { flate2::FlushCompress::Finish } else { flate2::FlushCompress::Sync };
	let mut stream = compressor.stream.lock().unwrap_or_else(|poison| poison.into_inner());
	match deflate_chunks(&mut stream, &[], flush) {
		Ok(bytes) => alloc_bytes(&bytes),
		Err(message) => raise_zlib_error(&message),
	}
}

unsafe extern "C" fn decompressor_decompress_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return raise_type_error("decompress received an invalid argument window");
	};
	if args.len() < 2 || args.len() > 3 {
		return raise_type_error("decompress() takes 1 or 2 arguments");
	}
	let Some(decompressor) = (unsafe { decompressor_ref(args[0]) }) else {
		return raise_type_error("receiver is not a zlib decompressor");
	};
	let Some(data) = (unsafe { bytes_arg(args[1]) }) else {
		let got = unsafe { crate::types::dict::type_name(crate::tag::untag_arg(args[1])) }.unwrap_or("object");
		return raise_type_error(&format!("a bytes-like object is required, not '{got}'"));
	};
	let max_length = match optional_i64(args.get(2).copied(), "max_length") {
		Ok(None) => None,
		Ok(Some(value)) if value < 0 => return raise_value_error_zlib("max_length must be non-negative"),
		Ok(Some(0)) => None,
		Ok(Some(value)) => Some(value as usize),
		Err(raised) => return raised,
	};
	let mut state = decompressor.state.lock().unwrap_or_else(|poison| poison.into_inner());
	let state = &mut *state;
	let mut out = Vec::new();
	let mut scratch = vec![0u8; 64 * 1024];
	let mut consumed = 0usize;
	loop {
		let remaining_budget = match max_length {
			Some(max) => max.saturating_sub(out.len()),
			None => scratch.len(),
		};
		if remaining_budget == 0 {
			break;
		}
		let window = remaining_budget.min(scratch.len());
		let before_in = state.stream.total_in();
		let before_out = state.stream.total_out();
		let status = match state.stream.decompress(
			&data[consumed..],
			&mut scratch[..window],
			flate2::FlushDecompress::None,
		) {
			Ok(status) => status,
			Err(error) => return raise_zlib_error(&format!("Error {error} while decompressing data")),
		};
		consumed += (state.stream.total_in() - before_in) as usize;
		let produced = (state.stream.total_out() - before_out) as usize;
		out.extend_from_slice(&scratch[..produced]);
		match status {
			flate2::Status::StreamEnd => {
				state.eof = true;
				break;
			},
			_ if consumed == data.len() && produced == 0 => break,
			_ => {},
		}
	}
	state.unconsumed = if state.eof { Vec::new() } else { data[consumed..].to_vec() };
	alloc_bytes(&out)
}

unsafe extern "C" fn decompressor_flush_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return raise_type_error("flush received an invalid argument window");
	};
	if args.is_empty() || args.len() > 2 {
		return raise_type_error("flush() takes at most one argument");
	}
	if unsafe { decompressor_ref(args[0]) }.is_none() {
		return raise_type_error("receiver is not a zlib decompressor");
	}
	// Pending output is always drained eagerly by `decompress`; nothing is
	// buffered stream-side.
	alloc_bytes(&[])
}

fn raise_value_error_zlib(message: &str) -> *mut PyObject {
	crate::abi::exc::raise_kind_error_text(ExceptionKind::ValueError, message)
}

/// `zlib.compressobj(level=-1, method=DEFLATED, wbits=MAX_WBITS, memLevel=8,
/// strategy=0, zdict=None)`: memLevel/strategy are validated by range only
/// (flate2 exposes no knobs for them); zdict is unsupported.
unsafe extern "C" fn compressobj_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return raise_type_error("compressobj received an invalid argument window");
	};
	if args.len() > 6 {
		return raise_type_error("compressobj() takes at most 6 arguments");
	}
	let level = match optional_i64(args.first().copied(), "level") {
		Ok(value) => value.unwrap_or(Z_DEFAULT_COMPRESSION),
		Err(raised) => return raised,
	};
	if !(level == Z_DEFAULT_COMPRESSION || (0..=9).contains(&level)) {
		return raise_zlib_error("Bad compression level");
	}
	let method = match optional_i64(args.get(1).copied(), "method") {
		Ok(value) => value.unwrap_or(DEFLATED),
		Err(raised) => return raised,
	};
	if method != DEFLATED {
		return raise_zlib_error(&format!("Invalid initialization option: method={method}"));
	}
	let wbits = match optional_i64(args.get(2).copied(), "wbits") {
		Ok(value) => value.unwrap_or(MAX_WBITS),
		Err(raised) => return raised,
	};
	let Some(window) = window_from_wbits(wbits) else {
		return raise_zlib_error(&format!("Invalid initialization option: wbits={wbits}"));
	};
	if args.len() > 5 && !args[5].is_null() && args[5] != unsafe { crate::abi::pon_none() } {
		return raise_not_implemented("compressobj(zdict=...)");
	}
	let level = if level == Z_DEFAULT_COMPRESSION {
		flate2::Compression::default()
	} else {
		flate2::Compression::new(level as u32)
	};
	let stream = match window {
		Window::Raw => flate2::Compress::new(level, false),
		Window::Zlib => flate2::Compress::new(level, true),
		Window::Gzip => flate2::Compress::new_gzip(level, 15),
	};
	Box::into_raw(Box::new(PyZlibCompressor {
		ob_base: PyObjectHeader::new(zlib_compressor_type().cast_const()),
		stream:  std::sync::Mutex::new(stream),
	}))
	.cast::<PyObject>()
}

/// `zlib.decompress(data, /, wbits=MAX_WBITS, bufsize=DEF_BUF_SIZE)` — the
/// keyword binder delivers `[data, wbits, bufsize]` (absent slots None).
/// `bufsize` is a hint CPython uses for the initial output allocation; the
/// streaming decoder here needs no hint, so it is validated and ignored.
unsafe extern "C" fn decompress_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return raise_type_error("decompress received an invalid argument window");
    };
    if args.is_empty() {
        return raise_type_error("decompress() missing required argument 'data' (pos 1)");
    }
    let Ok(data) = crate::abi::str_::expect_bytes_like(crate::tag::untag_arg(args[0])) else {
        let got = unsafe { crate::types::dict::type_name(crate::tag::untag_arg(args[0])) }.unwrap_or("object");
        return raise_type_error(&format!("a bytes-like object is required, not '{got}'"));
    };
    let wbits = match optional_i64(args.get(1).copied(), "wbits") {
        Ok(value) => value.unwrap_or(MAX_WBITS),
        Err(raised) => return raised,
    };
    if let Err(raised) = optional_i64(args.get(2).copied(), "bufsize") {
        return raised;
    }
    use std::io::Read;
    let mut out = Vec::new();
    let result = match wbits {
        // 32+n: zlib-or-gzip auto-detection (CPython accepts 32..=47).
        9..=15 | 32..=47 => flate2::read::ZlibDecoder::new(data.as_slice()).read_to_end(&mut out).map(|_| ()),
        -15..=-9 => flate2::read::DeflateDecoder::new(data.as_slice()).read_to_end(&mut out).map(|_| ()),
        25..=31 => flate2::read::GzDecoder::new(data.as_slice()).read_to_end(&mut out).map(|_| ()),
        _ => return raise_zlib_error(&format!("Invalid initialization option: wbits={wbits}")),
    };
    match result {
        Ok(()) => alloc_bytes(&out),
        Err(error) => raise_zlib_error(&format!("Error {error} while decompressing data")),
    }
}

/// Optional trailing argument as i64: absent slot or None reads as `None`.
fn optional_i64(value: Option<*mut PyObject>, what: &str) -> Result<Option<i64>, *mut PyObject> {
    let Some(value) = value else { return Ok(None) };
    if value.is_null() {
        return Ok(None);
    }
    let raw = crate::tag::untag_arg(value);
    if !raw.is_null() && unsafe { crate::types::dict::type_name(raw) } == Some("NoneType") {
        return Ok(None);
    }
    match unsafe { crate::types::int::to_bigint_including_bool(raw) }.and_then(|v| v.to_i64()) {
        Some(number) => Ok(Some(number)),
        None => Err(crate::abi::exc::raise_kind_error_text(
            ExceptionKind::TypeError,
            &format!("{what} must be an integer"),
        )),
    }
}

fn alloc_bytes(payload: &[u8]) -> *mut PyObject {
    crate::types::bytes_::boxed_bytes(payload).cast::<PyObject>()
}

/// The `zlib.error` heap class (`class error(Exception)`, `__module__` =
/// 'zlib'), built once — the binascii exception-class recipe.
static ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
    // SAFETY: `pon_load_global` returns NULL with a raised NameError on miss.
    let base = unsafe { crate::abi::pon_load_global(intern("Exception"), core::ptr::null_mut()) };
    if base.is_null() {
        crate::thread_state::pon_err_clear();
        return 0;
    }
    let namespace = crate::types::type_::new_namespace();
    if namespace.is_null() {
        return 0;
    }
    let module_name = "zlib";
    // SAFETY: String allocation helper follows the NULL-sentinel contract.
    let module_object = unsafe { crate::abi::pon_const_str(module_name.as_ptr(), module_name.len()) };
    if module_object.is_null() {
        return 0;
    }
    // SAFETY: `new_namespace` returned a live namespace box.
    unsafe { (*namespace).set(intern("__module__"), module_object) };
    // SAFETY: The base is a live class object owned by the runtime.
    let class = unsafe { crate::types::type_::build_class_from_namespace("error", &[base], namespace, &[]) };
    if class.is_null() {
        crate::thread_state::pon_err_clear();
        return 0;
    }
    // SAFETY: Freshly built class object; mirror `pon_build_class`'s ob_type fix.
    unsafe {
        if (*class).ob_type.is_null() {
            (*class).ob_type = crate::abi::runtime_type_type().cast_const();
        }
    }
    class as usize
});

fn zlib_error_class() -> *mut PyObject {
    *ERROR_CLASS as *mut PyObject
}

/// Raises `zlib.error(text)` (ValueError fallback while the heap class is
/// unavailable, e.g. pre-runtime tests).
fn raise_zlib_error(message: &str) -> *mut PyObject {
    let class = zlib_error_class();
    if class.is_null() {
        return crate::abi::exc::raise_kind_error_text(ExceptionKind::ValueError, message);
    }
    // SAFETY: String allocation helper follows the NULL-sentinel contract.
    let text = unsafe { crate::abi::pon_const_str(message.as_ptr(), message.len()) };
    if text.is_null() {
        return core::ptr::null_mut();
    }
    let mut argv = [text];
    // SAFETY: The class object is live and callable; argv holds one live slot.
    let instance = unsafe { crate::abi::pon_call(class, argv.as_mut_ptr(), argv.len()) };
    if instance.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `instance` is a live exception instance.
    unsafe { crate::abi::exc::pon_raise(instance, core::ptr::null_mut()) }
}

/// TypeError raise with the module's exception plumbing.
fn raise_type_error(message: &str) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

/// `zlib.decompressobj(wbits=MAX_WBITS, zdict=None)`.
unsafe extern "C" fn decompressobj_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return raise_type_error("decompressobj received an invalid argument window");
	};
	if args.len() > 2 {
		return raise_type_error("decompressobj() takes at most 2 arguments");
	}
	let wbits = match optional_i64(args.first().copied(), "wbits") {
		Ok(value) => value.unwrap_or(MAX_WBITS),
		Err(raised) => return raised,
	};
	let Some(window) = window_from_wbits(wbits) else {
		return raise_zlib_error(&format!("Invalid initialization option: wbits={wbits}"));
	};
	if args.len() > 1 && !args[1].is_null() && args[1] != unsafe { crate::abi::pon_none() } {
		return raise_not_implemented("decompressobj(zdict=...)");
	}
	let stream = match window {
		Window::Raw => flate2::Decompress::new(false),
		Window::Zlib => flate2::Decompress::new(true),
		Window::Gzip => flate2::Decompress::new_gzip(15),
	};
	Box::into_raw(Box::new(PyZlibDecompressorObj {
		ob_base: PyObjectHeader::new(zlib_decompressorobj_type().cast_const()),
		state:   std::sync::Mutex::new(InflateState { stream, unconsumed: Vec::new(), eof: false }),
	}))
	.cast::<PyObject>()
}
