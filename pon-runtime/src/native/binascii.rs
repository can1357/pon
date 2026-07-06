//! Native `binascii` (CT wave 2: the email/base64/quopri import chain).
//!
//! CPython's `binascii` is a C extension; the vendored stdlib has no pure
//! fallback and `base64`, `quopri`, `email.*`, and `test_binascii` import it
//! at module scope.  This seed implements the full 3.14 surface in pure Rust:
//! `a2b_base64`/`b2a_base64`, `a2b_hex`/`b2a_hex` (+ the `hexlify`/
//! `unhexlify` aliases), `a2b_qp`/`b2a_qp`, `a2b_uu`/`b2a_uu`, `crc32`,
//! `crc_hqx`, and the `Error`/`Incomplete` exception classes.
//!
//! The transcoding cores mirror `Modules/binascii.c` byte for byte (including
//! error message text) so differential runs against python3.14 agree:
//! non-strict base64 skips invalid characters, quoted-printable encode keeps
//! CPython's 76-column soft breaks and end-of-line whitespace protection, and
//! uuencode tolerates `` ` `` as zero plus eaten trailing spaces.
//!
//! `Error` is a heap class deriving from `ValueError` (CPython parity) and
//! `Incomplete` derives from `Exception`; both are built through
//! `build_class_from_namespace` — the same machinery a Python-level `class`
//! statement uses — so `except binascii.Error` matching and `str(exc)` work
//! unchanged.  Keyword calls (`b2a_base64(s, newline=False)`,
//! `a2b_base64(s, strict_mode=True)`, `b2a_qp(data, istext=False)`, ...) are
//! bound by name rows in `types::function::bind_native_keywords_for_name`;
//! absent optionals arrive as `None` and fall back to their defaults here.

use std::{ptr, sync::LazyLock};

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{
	abi,
	intern::intern,
	object::PyObject,
	thread_state::{pon_err_clear, pon_err_message},
	types::{
		bytearray_ as bytearray_type, bytes_ as bytes_type,
		exc::ExceptionKind,
		type_::{self as type_mod, unicode_text},
	},
};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

// ---------------------------------------------------------------------------
// Module exception classes
//
// Built lazily on first touch (module creation or a raise site).  `0` is the
// build-failure sentinel: `make_module` surfaces it as a module-creation
// error and raise sites degrade to plain `ValueError`/`Exception` text.

static ERROR_CLASS: LazyLock<usize> =
	LazyLock::new(|| exception_class("Error", "ValueError").map_or(0, |class| class as usize));

static INCOMPLETE_CLASS: LazyLock<usize> =
	LazyLock::new(|| exception_class("Incomplete", "Exception").map_or(0, |class| class as usize));

/// Builds one `binascii` exception heap class deriving from the named
/// builtin, with `__module__` set — the `_io.UnsupportedOperation` recipe.
fn exception_class(name: &str, base: &str) -> Result<*mut PyObject, String> {
	// SAFETY: `pon_load_global` returns NULL with a raised NameError on miss.
	let base_class = unsafe { abi::pon_load_global(intern(base), ptr::null_mut()) };
	if base_class.is_null() {
		pon_err_clear();
		return Err(format!("builtin class '{base}' is not registered"));
	}
	let namespace = type_mod::new_namespace();
	if namespace.is_null() {
		return Err(format!("failed to allocate binascii.{name} namespace"));
	}
	let module_object = alloc_str_object("binascii");
	if module_object.is_null() {
		return Err(format!("failed to allocate binascii.{name}.__module__"));
	}
	// SAFETY: `new_namespace` returned a live namespace box.
	unsafe { (*namespace).set(intern("__module__"), module_object) };
	// SAFETY: The base is a live class object owned by the runtime.
	let class = unsafe { type_mod::build_class_from_namespace(name, &[base_class], namespace, &[]) };
	if class.is_null() {
		let detail = pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
		pon_err_clear();
		return Err(format!("failed to create binascii.{name}: {detail}"));
	}
	// SAFETY: Freshly built class object; mirror `pon_build_class`'s ob_type fix.
	unsafe {
		if (*class).ob_type.is_null() {
			(*class).ob_type = abi::runtime_type_type().cast_const();
		}
	}
	Ok(class)
}

/// Raises `binascii.Error(text)` (falling back to `ValueError` when the heap
/// class could not be built) and returns NULL.
fn raise_error(text: &str) -> *mut PyObject {
	let class = *ERROR_CLASS;
	if class == 0 {
		return abi::exc::raise_kind_error_text(ExceptionKind::ValueError, text);
	}
	let message = alloc_str_object(text);
	if message.is_null() {
		return ptr::null_mut();
	}
	let mut argv = [message];
	// SAFETY: The class object is live and callable; argv holds one live slot.
	let instance = unsafe { abi::pon_call(class as *mut PyObject, argv.as_mut_ptr(), argv.len()) };
	if instance.is_null() {
		return ptr::null_mut();
	}
	// SAFETY: `instance` is a live exception instance.
	unsafe { abi::exc::pon_raise(instance, ptr::null_mut()) }
}

// ---------------------------------------------------------------------------
// Module construction

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let name = "binascii";
	let error_class = *ERROR_CLASS;
	if error_class == 0 {
		return Err("failed to create binascii.Error".to_owned());
	}
	let incomplete_class = *INCOMPLETE_CLASS;
	if incomplete_class == 0 {
		return Err("failed to create binascii.Incomplete".to_owned());
	}
	// SAFETY: Runtime allocation helper; NULL is checked below.
	let name_obj = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
	if name_obj.is_null() {
		return Err("failed to allocate binascii.__name__".to_owned());
	}
	let mut attrs: Vec<(u32, *mut PyObject)> = vec![
		(intern("__name__"), name_obj),
		(intern("Error"), error_class as *mut PyObject),
		(intern("Incomplete"), incomplete_class as *mut PyObject),
	];
	for (fn_name, entry) in [
		("a2b_base64", a2b_base64_entry as BuiltinFn),
		("b2a_base64", b2a_base64_entry),
		("a2b_hex", a2b_hex_entry),
		("b2a_hex", b2a_hex_entry),
		("hexlify", hexlify_entry),
		("unhexlify", unhexlify_entry),
		("a2b_qp", a2b_qp_entry),
		("b2a_qp", b2a_qp_entry),
		("a2b_uu", a2b_uu_entry),
		("b2a_uu", b2a_uu_entry),
		("crc32", crc32_entry),
		("crc_hqx", crc_hqx_entry),
	] {
		// SAFETY: `entry` is a live builtin entry point with the runtime
		// calling convention.
		let function =
			unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(fn_name)) };
		if function.is_null() {
			return Err(format!("failed to allocate binascii.{fn_name}"));
		}
		attrs.push((intern(fn_name), function));
	}
	install_module(name, attrs)
}

// ---------------------------------------------------------------------------
// Small helpers (codecs idioms)

fn untag(object: *mut PyObject) -> *mut PyObject {
	crate::tag::untag_arg(object)
}

fn none() -> *mut PyObject {
	// SAFETY: Singleton accessor.
	unsafe { abi::pon_none() }
}

fn is_none(object: *mut PyObject) -> bool {
	object == none()
}

fn alloc_str_object(text: &str) -> *mut PyObject {
	// SAFETY: Runtime allocation helper; NULL on failure with the error set.
	unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn alloc_bytes_object(bytes: &[u8]) -> *mut PyObject {
	// SAFETY: Runtime allocation helper; NULL on failure with the error set.
	unsafe { abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) }
}

fn alloc_int_object(value: i64) -> *mut PyObject {
	// SAFETY: Runtime allocation helper; NULL on failure with the error set.
	unsafe { abi::pon_const_int(value) }
}

fn raise_kind(kind: ExceptionKind, text: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(kind, text)
}

fn raise_type_error(message: &str) -> *mut PyObject {
	raise_kind(ExceptionKind::TypeError, message)
}

fn raise_value_error(message: &str) -> *mut PyObject {
	raise_kind(ExceptionKind::ValueError, message)
}

unsafe fn arg_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
	if argc == 0 {
		Some(&[])
	} else if argv.is_null() {
		None
	} else {
		// SAFETY: The caller passed `argc` live argument slots.
		Some(unsafe { std::slice::from_raw_parts(argv, argc) })
	}
}

fn value_type_name(object: *mut PyObject) -> &'static str {
	if object.is_null() {
		return "NULL";
	}
	if crate::tag::is_small_int(object) {
		return "int";
	}
	// SAFETY: Heap pointer with a live header.
	unsafe { crate::types::dict::type_name(object) }.unwrap_or("object")
}

/// An optional argument slot: absent or `None` means "use the default"
/// (the keyword binder fills unset slots with `None`).
fn opt_arg(args: &[*mut PyObject], idx: usize) -> Option<*mut PyObject> {
	match args.get(idx).copied() {
		None => None,
		Some(value) if value.is_null() => None,
		Some(value) => {
			let value = untag(value);
			if is_none(value) { None } else { Some(value) }
		},
	}
}

/// Truthiness of an optional bool-ish argument with a default.
fn truthy_arg(args: &[*mut PyObject], idx: usize, default: bool) -> Result<bool, ()> {
	match opt_arg(args, idx) {
		None => Ok(default),
		// SAFETY: Truthiness helper follows the error-sentinel contract.
		Some(value) => match unsafe { abi::pon_is_true(value) } {
			0 => Ok(false),
			1 => Ok(true),
			_ => Err(()),
		},
	}
}

/// Borrows a bytes-like argument's payload (CPython `Py_buffer` converter:
/// str is rejected).
fn buffer_arg<'a>(object: *mut PyObject, what: &str) -> Result<&'a [u8], *mut PyObject> {
	let object = untag(object);
	if object.is_null() {
		return Err(raise_type_error(&format!("{what}: argument is NULL")));
	}
	// SAFETY: A non-NULL heap object carries a live header.
	let ty = unsafe { (*object).ob_type };
	if bytes_type::is_bytes_type(ty) {
		// SAFETY: Type check above proved the layout.
		return Ok(unsafe { (*object.cast::<bytes_type::PyBytes>()).as_slice() });
	}
	if bytearray_type::is_bytearray_type(ty) {
		// SAFETY: Type check above proved the layout.
		return Ok(unsafe { (*object.cast::<bytearray_type::PyByteArray>()).as_slice() });
	}
	Err(raise_type_error(&format!(
		"a bytes-like object is required, not '{}'",
		value_type_name(object)
	)))
}

/// Borrows a bytes/bytearray payload without raising; `None` for other types.
fn bytes_like<'a>(object: *mut PyObject) -> Option<&'a [u8]> {
	if object.is_null() {
		return None;
	}
	// SAFETY: A non-NULL heap object carries a live header.
	let ty = unsafe { (*object).ob_type };
	if bytes_type::is_bytes_type(ty) {
		// SAFETY: Type check above proved the layout.
		return Some(unsafe { (*object.cast::<bytes_type::PyBytes>()).as_slice() });
	}
	if bytearray_type::is_bytearray_type(ty) {
		// SAFETY: Type check above proved the layout.
		return Some(unsafe { (*object.cast::<bytearray_type::PyByteArray>()).as_slice() });
	}
	None
}

/// Borrows a bytes-like or ASCII-str argument's payload (CPython
/// `ascii_buffer` converter used by every `a2b_*` function).
fn ascii_buffer_arg<'a>(object: *mut PyObject, what: &str) -> Result<&'a [u8], *mut PyObject> {
	let object = untag(object);
	if object.is_null() {
		return Err(raise_type_error(&format!("{what}: argument is NULL")));
	}
	// SAFETY: `untag` normalized the pointer; `unicode_text` type-checks.
	if let Some(text) = unsafe { unicode_text(object) } {
		if !text.is_ascii() {
			return Err(raise_value_error("string argument should contain only ASCII characters"));
		}
		return Ok(text.as_bytes());
	}
	// SAFETY: A non-NULL heap object carries a live header.
	let ty = unsafe { (*object).ob_type };
	if bytes_type::is_bytes_type(ty) {
		// SAFETY: Type check above proved the layout.
		return Ok(unsafe { (*object.cast::<bytes_type::PyBytes>()).as_slice() });
	}
	if bytearray_type::is_bytearray_type(ty) {
		// SAFETY: Type check above proved the layout.
		return Ok(unsafe { (*object.cast::<bytearray_type::PyByteArray>()).as_slice() });
	}
	Err(raise_type_error(&format!(
		"argument should be bytes, buffer or ASCII string, not '{}'",
		value_type_name(object)
	)))
}

/// Shared "N required argument(s)" prologue: every entry rejects the zero-arg
/// call with a `TypeError` (asserted by `test_binascii.test_functions`).
fn require_data<'a>(args: &'a [*mut PyObject], name: &str) -> Result<*mut PyObject, *mut PyObject> {
	match args.first().copied() {
		Some(value) => Ok(value),
		None => Err(raise_type_error(&format!("{name}() missing required argument 'data' (pos 1)"))),
	}
}

fn int_of(object: *mut PyObject) -> Option<i64> {
	unsafe { crate::types::int::to_i64(object) }
}

// ---------------------------------------------------------------------------
// base64 cores (mirrors Modules/binascii.c)

const PAD: u8 = b'=';
const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_value(ch: u8) -> Option<u8> {
	match ch {
		b'A'..=b'Z' => Some(ch - b'A'),
		b'a'..=b'z' => Some(ch - b'a' + 26),
		b'0'..=b'9' => Some(ch - b'0' + 52),
		b'+' => Some(62),
		b'/' => Some(63),
		_ => None,
	}
}

fn b2a_base64_core(data: &[u8], newline: bool) -> Vec<u8> {
	let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4 + 1);
	for chunk in data.chunks(3) {
		let b0 = chunk[0];
		let b1 = chunk.get(1).copied();
		let b2 = chunk.get(2).copied();
		out.push(B64_ALPHABET[usize::from(b0 >> 2)]);
		out.push(B64_ALPHABET[usize::from(((b0 & 0x03) << 4) | (b1.unwrap_or(0) >> 4))]);
		match b1 {
			None => {
				out.push(PAD);
				out.push(PAD);
			},
			Some(b1) => {
				out.push(B64_ALPHABET[usize::from(((b1 & 0x0f) << 2) | (b2.unwrap_or(0) >> 6))]);
				match b2 {
					None => out.push(PAD),
					Some(b2) => out.push(B64_ALPHABET[usize::from(b2 & 0x3f)]),
				}
			},
		}
	}
	if newline {
		out.push(b'\n');
	}
	out
}

/// Decode core mirroring 3.14's `binascii_a2b_base64_impl` exactly: pads
/// accumulate without terminating the scan, a valid data character resets
/// the pad run (non-strict) or raises (strict), and the final tally is
/// `quad_pos + pads < 4` -> "Incorrect padding".
fn a2b_base64_core(data: &[u8], strict_mode: bool) -> Result<Vec<u8>, String> {
	let mut out = Vec::with_capacity(data.len().div_ceil(4) * 3);
	let mut quad_pos = 0u32;
	let mut leftchar = 0u8;
	let mut pads = 0u32;
	for (index, &raw) in data.iter().enumerate() {
		if raw == PAD {
			pads += 1;
			if quad_pos >= 2 && quad_pos + pads <= 4 {
				continue;
			}
			// RFC 4648 §3.3: pads before the end of the data and excess
			// pads MAY be ignored — non-strict mode does exactly that.
			if !strict_mode {
				continue;
			}
			if quad_pos == 1 {
				// Falls through to the "1 more than a multiple of 4" error.
				break;
			}
			return Err(if quad_pos == 0 && index == 0 {
				"Leading padding not allowed".to_owned()
			} else {
				"Excess padding not allowed".to_owned()
			});
		}
		let Some(value) = b64_value(raw) else {
			if strict_mode {
				return Err("Only base64 data is allowed".to_owned());
			}
			continue;
		};
		// Data characters in the middle of a pad run reset it (non-strict)
		// or are rejected (strict).
		if pads != 0 && strict_mode {
			return Err(if quad_pos + pads == 4 {
				"Excess data after padding".to_owned()
			} else {
				"Discontinuous padding not allowed".to_owned()
			});
		}
		pads = 0;
		match quad_pos {
			0 => {
				quad_pos = 1;
				leftchar = value;
			},
			1 => {
				quad_pos = 2;
				out.push((leftchar << 2) | (value >> 4));
				leftchar = value & 0x0f;
			},
			2 => {
				quad_pos = 3;
				out.push((leftchar << 4) | (value >> 2));
				leftchar = value & 0x03;
			},
			_ => {
				quad_pos = 0;
				out.push((leftchar << 6) | value);
				leftchar = 0;
			},
		}
	}
	if quad_pos == 1 {
		return Err(format!(
			"Invalid base64-encoded string: number of data characters ({}) cannot be 1 more than a \
			 multiple of 4",
			out.len() / 3 * 4 + 1
		));
	}
	if quad_pos != 0 && quad_pos + pads < 4 {
		return Err("Incorrect padding".to_owned());
	}
	Ok(out)
}

// ---------------------------------------------------------------------------
// hex cores

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

fn hex_digit_value(ch: u8) -> Option<u8> {
	match ch {
		b'0'..=b'9' => Some(ch - b'0'),
		b'a'..=b'f' => Some(ch - b'a' + 10),
		b'A'..=b'F' => Some(ch - b'A' + 10),
		_ => None,
	}
}

/// `b2a_hex(data[, sep[, bytes_per_sep]])`: CPython `_Py_strhex_with_sep`.
/// Positive `bytes_per_sep` groups from the right, negative from the left.
fn b2a_hex_core(data: &[u8], sep: Option<u8>, bytes_per_sep: i64) -> Vec<u8> {
	let group = usize::try_from(bytes_per_sep.unsigned_abs()).unwrap_or(usize::MAX);
	let grouping_active =
		sep.is_some() && bytes_per_sep != 0 && group < data.len() && !data.is_empty();
	let mut out = Vec::with_capacity(data.len() * 2 + data.len() / group.max(1));
	for (index, &byte) in data.iter().enumerate() {
		if grouping_active && index != 0 {
			// Right-anchored groups for positive `bytes_per_sep`, left for negative.
			let boundary = if bytes_per_sep > 0 {
				(data.len() - index) % group == 0
			} else {
				index % group == 0
			};
			if boundary {
				out.push(sep.unwrap_or(b'_'));
			}
		}
		out.push(HEX_DIGITS[usize::from(byte >> 4)]);
		out.push(HEX_DIGITS[usize::from(byte & 0x0f)]);
	}
	out
}

fn a2b_hex_core(data: &[u8]) -> Result<Vec<u8>, String> {
	if data.len() % 2 != 0 {
		return Err("Odd-length string".to_owned());
	}
	let mut out = Vec::with_capacity(data.len() / 2);
	for pair in data.chunks_exact(2) {
		let (Some(hi), Some(lo)) = (hex_digit_value(pair[0]), hex_digit_value(pair[1])) else {
			return Err("Non-hexadecimal digit found".to_owned());
		};
		out.push((hi << 4) | lo);
	}
	Ok(out)
}

// ---------------------------------------------------------------------------
// quoted-printable cores (mirrors Modules/binascii.c)

const MAXLINESIZE: usize = 76;

fn a2b_qp_core(data: &[u8], header: bool) -> Vec<u8> {
	let mut out = Vec::with_capacity(data.len());
	let mut i = 0;
	while i < data.len() {
		let ch = data[i];
		if ch == b'=' {
			i += 1;
			if i >= data.len() {
				break;
			}
			if data[i] == b'\n' || data[i] == b'\r' {
				// Soft line break: swallow through the '\n'.
				if data[i] != b'\n' {
					while i < data.len() && data[i] != b'\n' {
						i += 1;
					}
				}
				if i < data.len() {
					i += 1;
				}
			} else if data[i] == b'=' {
				// Broken output from old Python quopri encoders.
				out.push(b'=');
				i += 1;
			} else if i + 1 < data.len()
				&& hex_digit_value(data[i]).is_some()
				&& hex_digit_value(data[i + 1]).is_some()
			{
				let value = (hex_digit_value(data[i]).unwrap_or(0) << 4)
					| hex_digit_value(data[i + 1]).unwrap_or(0);
				out.push(value);
				i += 2;
			} else {
				out.push(b'=');
			}
		} else if header && ch == b'_' {
			out.push(b' ');
			i += 1;
		} else {
			out.push(ch);
			i += 1;
		}
	}
	out
}

fn b2a_qp_core(data: &[u8], quotetabs: bool, istext: bool, header: bool) -> Vec<u8> {
	// CPython detects CRLF line endings from the first '\n' and normalizes
	// every emitted line ending to that style.
	let crlf = match data.iter().position(|&byte| byte == b'\n') {
		Some(pos) if pos > 0 => data[pos - 1] == b'\r',
		_ => false,
	};
	let needs_quoting = |i: usize, linelen: usize| -> bool {
		let ch = data[i];
		ch > 126
			|| ch == b'='
			|| (header && ch == b'_')
			|| (ch == b'.'
				&& linelen == 0
				&& (i + 1 == data.len()
					|| data[i + 1] == b'\n'
					|| data[i + 1] == b'\r'
					|| data[i + 1] == 0))
			|| (!istext && (ch == b'\r' || ch == b'\n'))
			|| ((ch == b'\t' || ch == b' ') && i + 1 == data.len())
			|| (ch < 33 && ch != b'\r' && ch != b'\n' && (quotetabs || (ch != b'\t' && ch != b' ')))
	};
	let soft_break = |out: &mut Vec<u8>| {
		out.push(b'=');
		if crlf {
			out.push(b'\r');
		}
		out.push(b'\n');
	};
	let push_quoted = |out: &mut Vec<u8>, ch: u8| {
		out.push(b'=');
		out.push(HEX_DIGITS[usize::from(ch >> 4)].to_ascii_uppercase());
		out.push(HEX_DIGITS[usize::from(ch & 0x0f)].to_ascii_uppercase());
	};
	let mut out: Vec<u8> = Vec::with_capacity(data.len() + data.len() / 2);
	let mut linelen = 0usize;
	let mut i = 0usize;
	while i < data.len() {
		let ch = data[i];
		if needs_quoting(i, linelen) {
			if linelen + 3 >= MAXLINESIZE {
				soft_break(&mut out);
				linelen = 0;
			}
			push_quoted(&mut out, ch);
			i += 1;
			linelen += 3;
		} else if istext
			&& (ch == b'\n' || (ch == b'\r' && i + 1 < data.len() && data[i + 1] == b'\n'))
		{
			linelen = 0;
			// Protect against whitespace on end of line.
			if let Some(&last) = out.last() {
				if last == b' ' || last == b'\t' {
					out.pop();
					push_quoted(&mut out, last);
				}
			}
			if crlf {
				out.push(b'\r');
			}
			out.push(b'\n');
			i += if ch == b'\r' { 2 } else { 1 };
		} else {
			if i + 1 != data.len() && data[i + 1] != b'\n' && linelen + 1 >= MAXLINESIZE {
				soft_break(&mut out);
				linelen = 0;
			}
			linelen += 1;
			if header && ch == b' ' {
				out.push(b'_');
			} else {
				out.push(ch);
			}
			i += 1;
		}
	}
	out
}

// ---------------------------------------------------------------------------
// uuencode cores (mirrors Modules/binascii.c)

fn a2b_uu_core(data: &[u8]) -> Result<Vec<u8>, String> {
	if data.is_empty() {
		return Err("Missing length byte".to_owned());
	}
	let mut bin_len = usize::from((data[0].wrapping_sub(b' ')) & 0o77);
	let mut out = Vec::with_capacity(bin_len);
	let mut leftchar: u32 = 0;
	let mut leftbits: u32 = 0;
	let mut index = 1usize;
	while bin_len > 0 {
		let this_ch = if index >= data.len() {
			// Spaces eaten at end-of-line: treat as zero bits.
			0
		} else {
			let raw = data[index];
			if raw == b'\n' || raw == b'\r' {
				0
			} else {
				// '`' (0x60) is tolerated as zero alongside ' ' (0x20).
				if !(b' '..=b' ' + 64).contains(&raw) {
					return Err("Illegal char".to_owned());
				}
				(raw.wrapping_sub(b' ')) & 0o77
			}
		};
		leftchar = (leftchar << 6) | u32::from(this_ch);
		leftbits += 6;
		if leftbits >= 8 {
			leftbits -= 8;
			out.push(((leftchar >> leftbits) & 0xff) as u8);
			leftchar &= (1 << leftbits) - 1;
			bin_len -= 1;
		}
		index += 1;
	}
	// Anything left on the line must be whitespace (or pad '`').
	while index < data.len() {
		let raw = data[index];
		if raw != b' ' && raw != b' ' + 64 && raw != b'\n' && raw != b'\r' {
			return Err("Trailing garbage".to_owned());
		}
		index += 1;
	}
	Ok(out)
}

fn b2a_uu_core(data: &[u8], backtick: bool) -> Result<Vec<u8>, String> {
	if data.len() > 45 {
		return Err("At most 45 bytes at once".to_owned());
	}
	let encode = |value: u8| -> u8 {
		if backtick && value == 0 {
			0x60
		} else {
			value + b' '
		}
	};
	let mut out = Vec::with_capacity(2 + data.len().div_ceil(3) * 4);
	out.push(encode(data.len() as u8));
	let mut leftchar: u32 = 0;
	let mut leftbits: u32 = 0;
	let mut remaining = data.len();
	let mut cursor = 0usize;
	while remaining > 0 || leftbits != 0 {
		// Shift the data (or zero padding) into the accumulator.
		if remaining > 0 {
			leftchar = (leftchar << 8) | u32::from(data[cursor]);
			cursor += 1;
			remaining -= 1;
		} else {
			leftchar <<= 8;
		}
		leftbits += 8;
		while leftbits >= 6 {
			let value = ((leftchar >> (leftbits - 6)) & 0x3f) as u8;
			leftbits -= 6;
			out.push(encode(value));
		}
	}
	out.push(b'\n');
	Ok(out)
}

// ---------------------------------------------------------------------------
// CRC cores

/// IEEE CRC-32 (reflected, poly 0xEDB88320) — the zlib/`binascii.crc32` CRC.
static CRC32_TABLE: [u32; 256] = {
	let mut table = [0u32; 256];
	let mut i = 0usize;
	while i < 256 {
		let mut value = i as u32;
		let mut bit = 0;
		while bit < 8 {
			value = if value & 1 != 0 {
				0xedb8_8320 ^ (value >> 1)
			} else {
				value >> 1
			};
			bit += 1;
		}
		table[i] = value;
		i += 1;
	}
	table
};

fn crc32_core(data: &[u8], crc: u32) -> u32 {
	let mut value = !crc;
	for &byte in data {
		value = CRC32_TABLE[usize::from((value ^ u32::from(byte)) as u8)] ^ (value >> 8);
	}
	!value
}

/// CRC-CCITT (poly 0x1021, MSB-first) — the binhex4 `crc_hqx` CRC.
static CRC_HQX_TABLE: [u16; 256] = {
	let mut table = [0u16; 256];
	let mut i = 0usize;
	while i < 256 {
		let mut value = (i as u16) << 8;
		let mut bit = 0;
		while bit < 8 {
			value = if value & 0x8000 != 0 {
				(value << 1) ^ 0x1021
			} else {
				value << 1
			};
			bit += 1;
		}
		table[i] = value;
		i += 1;
	}
	table
};

fn crc_hqx_core(data: &[u8], crc: u16) -> u16 {
	let mut value = crc;
	for &byte in data {
		value = (value << 8) ^ CRC_HQX_TABLE[usize::from(((value >> 8) as u8) ^ byte)];
	}
	value
}

// ---------------------------------------------------------------------------
// Entry points

unsafe extern "C" fn a2b_base64_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return raise_type_error("a2b_base64() received a null argv pointer");
	};
	let data = match require_data(args, "a2b_base64") {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	if args.len() > 2 {
		return raise_type_error(&format!(
			"a2b_base64() takes at most 2 arguments ({} given)",
			args.len()
		));
	}
	let data = match ascii_buffer_arg(data, "a2b_base64") {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	let strict_mode = match truthy_arg(args, 1, false) {
		Ok(value) => value,
		Err(()) => return ptr::null_mut(),
	};
	match a2b_base64_core(data, strict_mode) {
		Ok(out) => alloc_bytes_object(&out),
		Err(message) => raise_error(&message),
	}
}

unsafe extern "C" fn b2a_base64_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return raise_type_error("b2a_base64() received a null argv pointer");
	};
	let data = match require_data(args, "b2a_base64") {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	if args.len() > 2 {
		return raise_type_error(&format!(
			"b2a_base64() takes at most 2 arguments ({} given)",
			args.len()
		));
	}
	let data = match buffer_arg(data, "b2a_base64") {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	let newline = match truthy_arg(args, 1, true) {
		Ok(value) => value,
		Err(()) => return ptr::null_mut(),
	};
	alloc_bytes_object(&b2a_base64_core(data, newline))
}

/// Shared `b2a_hex`/`hexlify` body (the two names are the same function in
/// CPython).
unsafe fn hexlify_shared(argv: *mut *mut PyObject, argc: usize, name: &str) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return raise_type_error(&format!("{name}() received a null argv pointer"));
	};
	let data = match require_data(args, name) {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	if args.len() > 3 {
		return raise_type_error(&format!(
			"{name}() takes at most 3 arguments ({} given)",
			args.len()
		));
	}
	let data = match buffer_arg(data, name) {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	let sep = match opt_arg(args, 1) {
		None => None,
		Some(sep_obj) => {
			// `_Py_strhex_impl`: a str separator must be one latin-1 (1-byte
			// kind) character; a bytes separator is one arbitrary byte.
			// SAFETY: `opt_arg` untagged the pointer; `unicode_text` type-checks.
			if let Some(text) = unsafe { unicode_text(sep_obj) } {
				let mut chars = text.chars();
				let (first, extra) = (chars.next(), chars.next());
				let Some(sep_char) = first else {
					return raise_value_error("sep must be length 1.");
				};
				if extra.is_some() {
					return raise_value_error("sep must be length 1.");
				}
				if (sep_char as u32) > 255 {
					return raise_value_error("sep must be ASCII.");
				}
				Some(sep_char as u32 as u8)
			} else if let Some(sep_bytes) = bytes_like(sep_obj) {
				if sep_bytes.len() != 1 {
					return raise_value_error("sep must be length 1.");
				}
				Some(sep_bytes[0])
			} else {
				return raise_type_error("sep must be str or bytes.");
			}
		},
	};
	let bytes_per_sep = match opt_arg(args, 2) {
		None => 1,
		Some(value) => match int_of(value) {
			Some(value) => value,
			None => {
				return raise_type_error(&format!(
					"argument 'bytes_per_sep' must be int, not {}",
					value_type_name(value)
				));
			},
		},
	};
	alloc_bytes_object(&b2a_hex_core(data, sep, bytes_per_sep))
}

unsafe extern "C" fn b2a_hex_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { hexlify_shared(argv, argc, "b2a_hex") }
}

unsafe extern "C" fn hexlify_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { hexlify_shared(argv, argc, "hexlify") }
}

/// Shared `a2b_hex`/`unhexlify` body.
unsafe fn unhexlify_shared(argv: *mut *mut PyObject, argc: usize, name: &str) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return raise_type_error(&format!("{name}() received a null argv pointer"));
	};
	let data = match require_data(args, name) {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	if args.len() > 1 {
		return raise_type_error(&format!(
			"{name}() takes exactly 1 argument ({} given)",
			args.len()
		));
	}
	let data = match ascii_buffer_arg(data, name) {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	match a2b_hex_core(data) {
		Ok(out) => alloc_bytes_object(&out),
		Err(message) => raise_error(&message),
	}
}

unsafe extern "C" fn a2b_hex_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { unhexlify_shared(argv, argc, "a2b_hex") }
}

unsafe extern "C" fn unhexlify_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { unhexlify_shared(argv, argc, "unhexlify") }
}

unsafe extern "C" fn a2b_qp_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return raise_type_error("a2b_qp() received a null argv pointer");
	};
	let data = match require_data(args, "a2b_qp") {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	if args.len() > 2 {
		return raise_type_error(&format!(
			"a2b_qp() takes at most 2 arguments ({} given)",
			args.len()
		));
	}
	let data = match ascii_buffer_arg(data, "a2b_qp") {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	let header = match truthy_arg(args, 1, false) {
		Ok(value) => value,
		Err(()) => return ptr::null_mut(),
	};
	alloc_bytes_object(&a2b_qp_core(data, header))
}

unsafe extern "C" fn b2a_qp_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return raise_type_error("b2a_qp() received a null argv pointer");
	};
	let data = match require_data(args, "b2a_qp") {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	if args.len() > 4 {
		return raise_type_error(&format!(
			"b2a_qp() takes at most 4 arguments ({} given)",
			args.len()
		));
	}
	let data = match buffer_arg(data, "b2a_qp") {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	let quotetabs = match truthy_arg(args, 1, false) {
		Ok(value) => value,
		Err(()) => return ptr::null_mut(),
	};
	let istext = match truthy_arg(args, 2, true) {
		Ok(value) => value,
		Err(()) => return ptr::null_mut(),
	};
	let header = match truthy_arg(args, 3, false) {
		Ok(value) => value,
		Err(()) => return ptr::null_mut(),
	};
	alloc_bytes_object(&b2a_qp_core(data, quotetabs, istext, header))
}

unsafe extern "C" fn a2b_uu_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return raise_type_error("a2b_uu() received a null argv pointer");
	};
	let data = match require_data(args, "a2b_uu") {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	if args.len() > 1 {
		return raise_type_error(&format!(
			"a2b_uu() takes exactly 1 argument ({} given)",
			args.len()
		));
	}
	let data = match ascii_buffer_arg(data, "a2b_uu") {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	match a2b_uu_core(data) {
		Ok(out) => alloc_bytes_object(&out),
		Err(message) => raise_error(&message),
	}
}

unsafe extern "C" fn b2a_uu_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return raise_type_error("b2a_uu() received a null argv pointer");
	};
	let data = match require_data(args, "b2a_uu") {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	if args.len() > 2 {
		return raise_type_error(&format!(
			"b2a_uu() takes at most 2 arguments ({} given)",
			args.len()
		));
	}
	let data = match buffer_arg(data, "b2a_uu") {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	let backtick = match truthy_arg(args, 1, false) {
		Ok(value) => value,
		Err(()) => return ptr::null_mut(),
	};
	match b2a_uu_core(data, backtick) {
		Ok(out) => alloc_bytes_object(&out),
		Err(message) => raise_error(&message),
	}
}

unsafe extern "C" fn crc32_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return raise_type_error("crc32() received a null argv pointer");
	};
	let data = match require_data(args, "crc32") {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	if args.len() > 2 {
		return raise_type_error(&format!(
			"crc32() takes at most 2 arguments ({} given)",
			args.len()
		));
	}
	let data = match buffer_arg(data, "crc32") {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	let crc = match opt_arg(args, 1) {
		None => 0u32,
		Some(value) => match int_of(value) {
			Some(value) => value as u32,
			None => {
				return raise_type_error(&format!(
					"argument 'crc' must be int, not {}",
					value_type_name(value)
				));
			},
		},
	};
	alloc_int_object(i64::from(crc32_core(data, crc)))
}

unsafe extern "C" fn crc_hqx_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return raise_type_error("crc_hqx() received a null argv pointer");
	};
	let data = match require_data(args, "crc_hqx") {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	if args.len() != 2 {
		return raise_type_error(&format!(
			"crc_hqx() takes exactly 2 arguments ({} given)",
			args.len()
		));
	}
	let data = match buffer_arg(data, "crc_hqx") {
		Ok(data) => data,
		Err(raised) => return raised,
	};
	let crc = match int_of(untag(args[1])) {
		Some(value) => value as u16,
		None => {
			return raise_type_error(&format!(
				"argument 'crc' must be int, not {}",
				value_type_name(untag(args[1]))
			));
		},
	};
	alloc_int_object(i64::from(crc_hqx_core(data, crc)))
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn base64_round_trip_and_padding() {
		assert_eq!(b2a_base64_core(b"", true), b"\n");
		assert_eq!(b2a_base64_core(b"f", true), b"Zg==\n");
		assert_eq!(b2a_base64_core(b"fo", true), b"Zm8=\n");
		assert_eq!(b2a_base64_core(b"foo", false), b"Zm9v");
		assert_eq!(b2a_base64_core(b"foobar", true), b"Zm9vYmFy\n");
		let raw: Vec<u8> = (0u16..=255).map(|value| value as u8).collect();
		let encoded = b2a_base64_core(&raw, true);
		assert_eq!(a2b_base64_core(&encoded, false).unwrap(), raw);
		assert_eq!(a2b_base64_core(b"Zg==", true).unwrap(), b"f");
	}

	#[test]
	fn base64_decode_errors_match_cpython_text() {
		assert_eq!(a2b_base64_core(b"Zg=", false).unwrap_err(), "Incorrect padding");
		assert_eq!(a2b_base64_core(b"Zg", false).unwrap_err(), "Incorrect padding");
		assert_eq!(
			a2b_base64_core(b"Z", false).unwrap_err(),
			"Invalid base64-encoded string: number of data characters (1) cannot be 1 more than a \
			 multiple of 4"
		);
		assert_eq!(a2b_base64_core(b"=Zg==", true).unwrap_err(), "Leading padding not allowed");
		assert_eq!(a2b_base64_core(b"Zm-9v", true).unwrap_err(), "Only base64 data is allowed");
		assert_eq!(a2b_base64_core(b"Zg==Zg==", true).unwrap_err(), "Excess data after padding");
		assert_eq!(a2b_base64_core(b"Zg=Q", true).unwrap_err(), "Discontinuous padding not allowed");
		assert_eq!(a2b_base64_core(b"Zg===", true).unwrap_err(), "Excess padding not allowed");
		// Non-strict oracle (probed against python3.14): pads never stop the
		// scan; data after a pad run resets it and keeps decoding.
		assert_eq!(a2b_base64_core(b"Zm\n9v", false).unwrap(), b"foo");
		assert_eq!(a2b_base64_core(b"Zg==junk", false).unwrap_err(), "Incorrect padding");
		assert_eq!(a2b_base64_core(b"Zg==Zg==", false).unwrap(), b"f\x06`");
		assert_eq!(a2b_base64_core(b"AB==CD==", false).unwrap(), b"\x00\x10\x83");
		assert_eq!(a2b_base64_core(b"Zg==\n", false).unwrap(), b"f");
		assert_eq!(a2b_base64_core(b"Zg=,=", false).unwrap(), b"f");
		assert_eq!(a2b_base64_core(b"Zg===", false).unwrap(), b"f");
		assert_eq!(a2b_base64_core(b"Zm9v====", false).unwrap(), b"foo");
		assert_eq!(a2b_base64_core(b"=", false).unwrap(), b"");
		assert_eq!(a2b_base64_core(b"==", false).unwrap(), b"");
		assert_eq!(
			a2b_base64_core(b"A===", false).unwrap_err(),
			"Invalid base64-encoded string: number of data characters (1) cannot be 1 more than a \
			 multiple of 4"
		);
		assert_eq!(
			a2b_base64_core(b"Zm9v==x=", false).unwrap_err(),
			"Invalid base64-encoded string: number of data characters (5) cannot be 1 more than a \
			 multiple of 4"
		);
	}

	#[test]
	fn hex_round_trip_grouping_and_errors() {
		assert_eq!(b2a_hex_core(b"\x01\xff", None, 1), b"01ff");
		assert_eq!(a2b_hex_core(b"01ff").unwrap(), b"\x01\xff");
		assert_eq!(a2b_hex_core(b"01FF").unwrap(), b"\x01\xff");
		assert_eq!(a2b_hex_core(b"abc").unwrap_err(), "Odd-length string");
		assert_eq!(a2b_hex_core(b"zz").unwrap_err(), "Non-hexadecimal digit found");
		// bytes([1,2,3,4,5]).hex('_', 2) == '01_0203_0405' (right-anchored).
		assert_eq!(b2a_hex_core(&[1, 2, 3, 4, 5], Some(b'_'), 2), b"01_0203_0405");
		assert_eq!(b2a_hex_core(&[1, 2, 3, 4, 5], Some(b'_'), -2), b"0102_0304_05");
		assert_eq!(b2a_hex_core(&[1, 2], Some(b':'), 4), b"0102");
	}

	#[test]
	fn qp_round_trip_soft_breaks_and_header() {
		assert_eq!(b2a_qp_core(b"hello", false, true, false), b"hello");
		assert_eq!(b2a_qp_core(b"caf\xe9", false, true, false), b"caf=E9");
		assert_eq!(b2a_qp_core(b"a=b", false, true, false), b"a=3Db");
		// Trailing space before a line end is protected.
		assert_eq!(b2a_qp_core(b"x \ny", false, true, false), b"x=20\ny");
		// Header mode maps space to underscore both directions.
		assert_eq!(b2a_qp_core(b"a b", false, true, true), b"a_b");
		assert_eq!(a2b_qp_core(b"a_b", true), b"a b");
		assert_eq!(a2b_qp_core(b"caf=E9", false), b"caf\xe9");
		assert_eq!(a2b_qp_core(b"a=\nb", false), b"ab");
		assert_eq!(a2b_qp_core(b"a==3D", false), b"a=3D");
		assert_eq!(a2b_qp_core(b"a=x", false), b"a=x");
		// Long lines gain "=\n" soft breaks and decode back losslessly.
		let long = vec![b'a'; 200];
		let encoded = b2a_qp_core(&long, false, true, false);
		assert!(encoded.windows(2).any(|pair| pair == b"=\n"));
		assert_eq!(a2b_qp_core(&encoded, false), long);
	}

	#[test]
	fn uu_round_trip_and_errors() {
		let line = b2a_uu_core(b"Cat", false).unwrap();
		assert_eq!(line, b"#0V%T\n");
		assert_eq!(a2b_uu_core(&line).unwrap(), b"Cat");
		let empty = b2a_uu_core(b"", false).unwrap();
		assert_eq!(empty, b" \n");
		assert_eq!(a2b_uu_core(&empty).unwrap(), b"");
		assert_eq!(b2a_uu_core(&[0u8; 46], false).unwrap_err(), "At most 45 bytes at once");
		assert_eq!(a2b_uu_core(b"#0V%\x07\n").unwrap_err(), "Illegal char");
		assert_eq!(a2b_uu_core(b"").unwrap_err(), "Missing length byte");
		let round = b2a_uu_core(&[0, 1, 2, 3, 4], true).unwrap();
		assert_eq!(a2b_uu_core(&round).unwrap(), &[0, 1, 2, 3, 4]);
	}

	#[test]
	fn crc_values_match_reference() {
		// zlib.crc32(b"The quick brown fox jumps over the lazy dog") == 0x414FA339.
		assert_eq!(crc32_core(b"The quick brown fox jumps over the lazy dog", 0), 0x414f_a339);
		assert_eq!(crc32_core(b"", 0), 0);
		// Running CRC equals whole-buffer CRC.
		let split = crc32_core(b" world", crc32_core(b"hello", 0));
		assert_eq!(split, crc32_core(b"hello world", 0));
		// binascii.crc_hqx(b"123456789", 0) == 0x31C3 (CRC-CCITT/XMODEM).
		assert_eq!(crc_hqx_core(b"123456789", 0), 0x31c3);
	}
}
