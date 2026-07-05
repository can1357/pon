//! Native multibyte codec families backed by the host `iconv(3)` tables.
//!
//! CPython implements `_codecs_cn`, `_codecs_hk`, `_codecs_iso2022`,
//! `_codecs_jp`, `_codecs_kr`, `_codecs_tw`, and `_multibytecodec` in C over
//! generated mapping tables.  Pon uses the platform `iconv` implementation as a
//! real table-driven codec engine for the same encodings: each family module
//! exposes `getcodec(name)`, returning a codec object with `encode` and
//! `decode` methods that follow the `(payload, consumed)` multibyte-codec
//! contract used by `Lib/encodings/*.py`.
//!
//! The pure-Python `_multibytecodec` fallback in the vendored Lib layer
//! supplies incremental and stream base classes.  This file deliberately does
//! not fake unsupported iconv labels: the codec object can be imported so
//! stdlib codec modules bind successfully, while the first actual conversion
//! reports the missing host label loudly.

use core::{
	ffi::{c_char, c_int, c_void},
	ptr,
};
use std::{
	ffi::CString,
	io,
	sync::{LazyLock, Mutex},
};

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{
	abi,
	intern::intern,
	object::{PyObject, PyObjectHeader, PyType},
	types::{exc::ExceptionKind, type_::unicode_text},
};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;
type IconvT = *mut c_void;

#[cfg_attr(target_os = "macos", link(name = "iconv"))]
unsafe extern "C" {
	fn iconv_open(tocode: *const c_char, fromcode: *const c_char) -> IconvT;
	fn iconv(
		cd: IconvT,
		inbuf: *mut *mut c_char,
		inbytesleft: *mut usize,
		outbuf: *mut *mut c_char,
		outbytesleft: *mut usize,
	) -> usize;
	fn iconv_close(cd: IconvT) -> c_int;
}

#[derive(Clone, Copy, Debug)]
struct CodecSpec {
	py_name:    &'static str,
	iconv_name: &'static str,
}

#[repr(C)]
struct PyMultibyteCodec {
	ob_base: PyObjectHeader,
	spec:    CodecSpec,
}

static CODEC_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"MultibyteCodec",
		core::mem::size_of::<PyMultibyteCodec>(),
	);
	ty.tp_getattro = Some(codec_getattro);
	Box::into_raw(Box::new(ty)) as usize
});

fn codec_type() -> *mut PyType {
	*CODEC_TYPE as *mut PyType
}

static CODEC_ROOTS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn alloc_codec(spec: CodecSpec) -> *mut PyObject {
	let object = Box::into_raw(Box::new(PyMultibyteCodec {
		ob_base: PyObjectHeader::new(codec_type()),
		spec,
	}));
	CODEC_ROOTS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.push(object as usize);
	object.cast::<PyObject>()
}

fn codec_from_object(object: *mut PyObject) -> Option<&'static PyMultibyteCodec> {
	if object.is_null() || crate::tag::is_small_int(object) {
		return None;
	}
	let ty = unsafe { (*object).ob_type };
	if ty == codec_type().cast_const() {
		Some(unsafe { &*object.cast::<PyMultibyteCodec>() })
	} else {
		None
	}
}

pub(super) fn make_codecs_cn_module() -> Result<*mut PyObject, String> {
	make_family_module("_codecs_cn", CN_CODECS)
}

pub(super) fn make_codecs_hk_module() -> Result<*mut PyObject, String> {
	make_family_module("_codecs_hk", HK_CODECS)
}

pub(super) fn make_codecs_iso2022_module() -> Result<*mut PyObject, String> {
	make_family_module("_codecs_iso2022", ISO2022_CODECS)
}

pub(super) fn make_codecs_jp_module() -> Result<*mut PyObject, String> {
	make_family_module("_codecs_jp", JP_CODECS)
}

pub(super) fn make_codecs_kr_module() -> Result<*mut PyObject, String> {
	make_family_module("_codecs_kr", KR_CODECS)
}

pub(super) fn make_codecs_tw_module() -> Result<*mut PyObject, String> {
	make_family_module("_codecs_tw", TW_CODECS)
}

fn make_family_module(
	name: &'static str,
	specs: &'static [CodecSpec],
) -> Result<*mut PyObject, String> {
	let mut attrs = Vec::with_capacity(3);
	attrs.push((intern("__name__"), str_object(name)));
	attrs.push((
		intern("__doc__"),
		str_object("Pon multibyte codec family backed by host iconv tables"),
	));
	attrs.push((intern("getcodec"), family_getcodec_function(name, specs)?));
	install_module(name, attrs)
}

fn family_getcodec_function(
	module_name: &'static str,
	specs: &'static [CodecSpec],
) -> Result<*mut PyObject, String> {
	let entry = match module_name {
		"_codecs_cn" => getcodec_cn_entry as BuiltinFn,
		"_codecs_hk" => getcodec_hk_entry,
		"_codecs_iso2022" => getcodec_iso2022_entry,
		"_codecs_jp" => getcodec_jp_entry,
		"_codecs_kr" => getcodec_kr_entry,
		"_codecs_tw" => getcodec_tw_entry,
		_ => return Err(format!("unknown multibyte codec family {module_name}")),
	};
	if specs.is_empty() {
		return Err(format!("empty multibyte codec family {module_name}"));
	}
	let function =
		unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern("getcodec")) };
	if function.is_null() {
		Err(format!("failed to allocate {module_name}.getcodec"))
	} else {
		Ok(function)
	}
}

const CN_CODECS: &[CodecSpec] = &[
	CodecSpec { py_name: "gb18030", iconv_name: "GB18030" },
	CodecSpec { py_name: "gb2312", iconv_name: "GB2312" },
	CodecSpec { py_name: "gbk", iconv_name: "GBK" },
	CodecSpec { py_name: "hz", iconv_name: "HZ-GB-2312" },
];

const HK_CODECS: &[CodecSpec] = &[CodecSpec { py_name: "big5hkscs", iconv_name: "BIG5-HKSCS" }];

const ISO2022_CODECS: &[CodecSpec] = &[
	CodecSpec { py_name: "iso2022_jp", iconv_name: "ISO-2022-JP" },
	CodecSpec { py_name: "iso2022_jp_1", iconv_name: "ISO-2022-JP-1" },
	CodecSpec { py_name: "iso2022_jp_2", iconv_name: "ISO-2022-JP-2" },
	CodecSpec { py_name: "iso2022_jp_2004", iconv_name: "ISO-2022-JP-2004" },
	CodecSpec { py_name: "iso2022_jp_3", iconv_name: "ISO-2022-JP-3" },
	CodecSpec { py_name: "iso2022_jp_ext", iconv_name: "ISO-2022-JP-EXT" },
	CodecSpec { py_name: "iso2022_kr", iconv_name: "ISO-2022-KR" },
];

const JP_CODECS: &[CodecSpec] = &[
	CodecSpec { py_name: "cp932", iconv_name: "CP932" },
	CodecSpec { py_name: "euc_jis_2004", iconv_name: "EUC-JIS-2004" },
	CodecSpec { py_name: "euc_jisx0213", iconv_name: "EUC-JISX0213" },
	CodecSpec { py_name: "euc_jp", iconv_name: "EUC-JP" },
	CodecSpec { py_name: "shift_jis", iconv_name: "SHIFT_JIS" },
	CodecSpec { py_name: "shift_jis_2004", iconv_name: "SHIFT_JIS-2004" },
	CodecSpec { py_name: "shift_jisx0213", iconv_name: "SHIFT_JISX0213" },
];

const KR_CODECS: &[CodecSpec] = &[
	CodecSpec { py_name: "cp949", iconv_name: "CP949" },
	CodecSpec { py_name: "euc_kr", iconv_name: "EUC-KR" },
	CodecSpec { py_name: "johab", iconv_name: "JOHAB" },
];

const TW_CODECS: &[CodecSpec] = &[CodecSpec { py_name: "big5", iconv_name: "BIG5" }, CodecSpec {
	py_name:    "cp950",
	iconv_name: "CP950",
}];

unsafe extern "C" fn getcodec_cn_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	getcodec_entry(argv, argc, CN_CODECS, "_codecs_cn")
}

unsafe extern "C" fn getcodec_hk_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	getcodec_entry(argv, argc, HK_CODECS, "_codecs_hk")
}

unsafe extern "C" fn getcodec_iso2022_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	getcodec_entry(argv, argc, ISO2022_CODECS, "_codecs_iso2022")
}

unsafe extern "C" fn getcodec_jp_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	getcodec_entry(argv, argc, JP_CODECS, "_codecs_jp")
}

unsafe extern "C" fn getcodec_kr_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	getcodec_entry(argv, argc, KR_CODECS, "_codecs_kr")
}

unsafe extern "C" fn getcodec_tw_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	getcodec_entry(argv, argc, TW_CODECS, "_codecs_tw")
}

fn getcodec_entry(
	argv: *mut *mut PyObject,
	argc: usize,
	specs: &'static [CodecSpec],
	module_name: &str,
) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) => args,
		None => return raise_type_error(&format!("{module_name}.getcodec() missing codec name")),
	};
	if args.len() != 1 {
		return raise_type_error(&format!(
			"{module_name}.getcodec() takes exactly one argument ({} given)",
			args.len()
		));
	}
	let name_obj = untag(args[0]);
	let Some(name) = (unsafe { unicode_text(name_obj) }) else {
		return raise_type_error("codec name must be a str");
	};
	let normalized = normalize_codec_name(name);
	let Some(spec) = specs
		.iter()
		.copied()
		.find(|spec| spec.py_name == normalized)
	else {
		return raise_lookup_error(&format!("unknown multibyte codec: {name}"));
	};
	alloc_codec(spec)
}

unsafe extern "C" fn codec_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let Some(name_text) = (unsafe { unicode_text(untag(name)) }) else {
		return raise_type_error("attribute name must be str");
	};
	match name_text {
		"encode" => bound_method(object, "encode", codec_encode_method),
		"decode" => bound_method(object, "decode", codec_decode_method),
		"__name__" | "name" => match codec_from_object(object) {
			Some(codec) => str_object(codec.spec.py_name),
			None => raise_type_error("invalid MultibyteCodec receiver"),
		},
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

unsafe extern "C" fn codec_encode_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (codec, rest) = match unsafe { codec_receiver_and_args(argv, argc, "encode") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if rest.is_empty() || rest.len() > 2 {
		return raise_type_error(&format!("encode() takes 1 or 2 arguments ({} given)", rest.len()));
	}
	let input_obj = untag(rest[0]);
	let Some(input) = (unsafe { unicode_text(input_obj) }) else {
		return raise_type_error("MultibyteCodec.encode() argument 1 must be str");
	};
	let errors = match errors_arg(rest, 1) {
		Ok(errors) => errors,
		Err(raised) => return raised,
	};
	if errors != "strict" {
		return raise_lookup_error(&format!("unsupported multibyte error handler: {errors}"));
	}
	let encoded = match iconv_convert(codec.spec.iconv_name, "UTF-8", input.as_bytes()) {
		Ok(encoded) => encoded,
		Err(message) => return raise_unicode_encode_error(&message),
	};
	let bytes = unsafe { abi::str_::pon_const_bytes(encoded.as_ptr(), encoded.len()) };
	if bytes.is_null() {
		return ptr::null_mut();
	}
	codec_result(bytes, input.chars().count())
}

unsafe extern "C" fn codec_decode_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (codec, rest) = match unsafe { codec_receiver_and_args(argv, argc, "decode") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if rest.is_empty() || rest.len() > 2 {
		return raise_type_error(&format!("decode() takes 1 or 2 arguments ({} given)", rest.len()));
	}
	let input_obj = untag(rest[0]);
	let Some(input) = bytes_arg(input_obj) else {
		return raise_type_error("a bytes-like object is required, not str");
	};
	let errors = match errors_arg(rest, 1) {
		Ok(errors) => errors,
		Err(raised) => return raised,
	};
	if errors != "strict" {
		return raise_lookup_error(&format!("unsupported multibyte error handler: {errors}"));
	}
	let decoded = match iconv_convert("UTF-8", codec.spec.iconv_name, input) {
		Ok(decoded) => decoded,
		Err(message) => return raise_unicode_decode_error(&message),
	};
	let text = match String::from_utf8(decoded) {
		Ok(text) => text,
		Err(_) => return raise_unicode_decode_error("iconv produced invalid UTF-8"),
	};
	let text_obj = str_object(&text);
	if text_obj.is_null() {
		return ptr::null_mut();
	}
	codec_result(text_obj, input.len())
}

fn iconv_convert(to_code: &str, from_code: &str, input: &[u8]) -> Result<Vec<u8>, String> {
	let to = CString::new(to_code).map_err(|_| format!("invalid iconv target label {to_code:?}"))?;
	let from =
		CString::new(from_code).map_err(|_| format!("invalid iconv source label {from_code:?}"))?;
	let cd = unsafe { iconv_open(to.as_ptr(), from.as_ptr()) };
	if iconv_failed(cd) {
		return Err(format!("iconv codec {from_code} -> {to_code} is not available"));
	}
	let result = convert_with_open_iconv(cd, input)
		.map_err(|error| format!("{from_code} -> {to_code} conversion failed: {error}"));
	let close_rc = unsafe { iconv_close(cd) };
	if close_rc != 0 && result.is_ok() {
		return Err(format!(
			"iconv close failed for {from_code} -> {to_code}: {}",
			io::Error::last_os_error()
		));
	}
	result
}

fn iconv_failed(cd: IconvT) -> bool {
	cd as isize == -1
}

fn convert_with_open_iconv(cd: IconvT, input: &[u8]) -> io::Result<Vec<u8>> {
	let mut out = vec![0_u8; input.len().saturating_mul(4).max(64)];
	let mut written = 0_usize;
	let mut in_left = input.len();
	let mut in_ptr = input.as_ptr() as *mut c_char;

	loop {
		ensure_output_room(&mut out, written);
		let mut out_ptr = unsafe { out.as_mut_ptr().add(written) } as *mut c_char;
		let mut out_left = out.len() - written;
		let rc = unsafe { iconv(cd, &mut in_ptr, &mut in_left, &mut out_ptr, &mut out_left) };
		written = out.len() - out_left;
		if rc != usize::MAX {
			break;
		}
		let err = io::Error::last_os_error();
		if err.raw_os_error() == Some(libc::E2BIG) {
			out.resize(out.len().saturating_mul(2).max(out.len() + 64), 0);
			continue;
		}
		return Err(err);
	}

	loop {
		ensure_output_room(&mut out, written);
		let mut out_ptr = unsafe { out.as_mut_ptr().add(written) } as *mut c_char;
		let mut out_left = out.len() - written;
		let mut null_in: *mut c_char = ptr::null_mut();
		let mut zero = 0_usize;
		let rc = unsafe { iconv(cd, &mut null_in, &mut zero, &mut out_ptr, &mut out_left) };
		written = out.len() - out_left;
		if rc != usize::MAX {
			break;
		}
		let err = io::Error::last_os_error();
		if err.raw_os_error() == Some(libc::E2BIG) {
			out.resize(out.len().saturating_mul(2).max(out.len() + 64), 0);
			continue;
		}
		return Err(err);
	}

	out.truncate(written);
	Ok(out)
}

fn ensure_output_room(out: &mut Vec<u8>, written: usize) {
	if written == out.len() {
		out.resize(out.len().saturating_mul(2).max(out.len() + 64), 0);
	}
}

unsafe fn arg_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
	if argc == 0 {
		Some(&[])
	} else if argv.is_null() {
		None
	} else {
		Some(unsafe { std::slice::from_raw_parts(argv, argc) })
	}
}

unsafe fn codec_receiver_and_args<'a>(
	argv: *mut *mut PyObject,
	argc: usize,
	method: &str,
) -> Result<(&'static PyMultibyteCodec, &'a [*mut PyObject]), *mut PyObject> {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return Err(raise_type_error(&format!("MultibyteCodec.{method}() missing receiver")));
	};
	let Some((receiver, rest)) = args.split_first() else {
		return Err(raise_type_error(&format!("MultibyteCodec.{method}() missing receiver")));
	};
	let receiver = untag(*receiver);
	match codec_from_object(receiver) {
		Some(codec) => Ok((codec, rest)),
		None => Err(raise_type_error(&format!(
			"descriptor '{method}' for 'MultibyteCodec' objects doesn't apply"
		))),
	}
}

fn errors_arg<'a>(args: &'a [*mut PyObject], idx: usize) -> Result<&'a str, *mut PyObject> {
	let Some(error_obj) = args.get(idx).copied().map(untag) else {
		return Ok("strict");
	};
	if error_obj == none() {
		return Ok("strict");
	}
	unsafe { unicode_text(error_obj) }.ok_or_else(|| raise_type_error("errors must be str"))
}

fn bytes_arg<'a>(object: *mut PyObject) -> Option<&'a [u8]> {
	if object.is_null() || crate::tag::is_small_int(object) {
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

fn codec_result(payload: *mut PyObject, consumed: usize) -> *mut PyObject {
	let consumed = unsafe { abi::pon_const_int(consumed as i64) };
	if consumed.is_null() {
		return ptr::null_mut();
	}
	let mut items = [payload, consumed];
	unsafe { abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

fn normalize_codec_name(name: &str) -> String {
	name
		.bytes()
		.map(|byte| match byte {
			b'A'..=b'Z' => char::from(byte + 32),
			b'-' | b' ' => '_',
			_ => char::from(byte),
		})
		.collect()
}

fn bound_method(receiver: *mut PyObject, name: &str, entry: BuiltinFn) -> *mut PyObject {
	let function =
		unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	if function.is_null() {
		return ptr::null_mut();
	}
	match crate::types::method::new_bound_method(function, receiver) {
		Ok(method) => method.cast::<PyObject>(),
		Err(message) => raise_runtime_error(&message),
	}
}

fn untag(object: *mut PyObject) -> *mut PyObject {
	crate::tag::untag_arg(object)
}

fn none() -> *mut PyObject {
	unsafe { abi::pon_none() }
}

fn str_object(text: &str) -> *mut PyObject {
	unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn raise_kind(kind: ExceptionKind, text: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(kind, text)
}

fn raise_type_error(text: &str) -> *mut PyObject {
	raise_kind(ExceptionKind::TypeError, text)
}

fn raise_lookup_error(text: &str) -> *mut PyObject {
	raise_kind(ExceptionKind::LookupError, text)
}

fn raise_runtime_error(text: &str) -> *mut PyObject {
	raise_kind(ExceptionKind::RuntimeError, text)
}

fn raise_unicode_encode_error(text: &str) -> *mut PyObject {
	raise_kind(ExceptionKind::UnicodeEncodeError, text)
}

fn raise_unicode_decode_error(text: &str) -> *mut PyObject {
	raise_kind(ExceptionKind::UnicodeDecodeError, text)
}
