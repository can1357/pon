//! Native `_codecs` — codec registry plus builtin utf-8 / ascii / latin-1
//! codecs (WS-IMPORT: `codecs.py` -> `tokenize.py` -> `unittest` chain).
//!
//! # Layering (mirrors CPython, adapted to pon)
//!
//! CPython's `_codecs.lookup` only drives *search functions*; the builtin
//! codecs are found because interpreter init imports the pure-Python
//! `encodings` package, whose `__init__` registers a search function over
//! `encodings.<name>` modules (each `getregentry()` builds a
//! `codecs.CodecInfo`, a tuple subclass defined in `codecs.py`).
//!
//! pon keeps that protocol but front-loads the three encodings the runtime
//! itself guarantees (`utf-8`, `ascii`, `latin-1`):
//!
//! 1. `lookup(name)` normalizes the name (lowercase, spaces -> underscores: the
//!    C-level normalization; the cache is keyed by this form).
//! 2. A hit in the in-tree builtin table (aggressive alias normalization,
//!    aliases from `Lib/encodings/aliases.py`) constructs a real
//!    `codecs.CodecInfo` — fetched from the already-imported `codecs` module —
//!    wrapping this module's own `utf_8_encode`/... function objects, exactly
//!    the pairing `encodings/utf_8.py` would produce. (`import codecs` never
//!    calls `lookup` at module scope, so the class exists by the time any
//!    lookup can run.)
//! 3. Otherwise registered search functions run in registration order; on a
//!    full miss the vendored `encodings` package is imported once (lazily, like
//!    CPython's registry init) and the search functions run again.
//! 4. Still nothing -> `LookupError: unknown encoding: {name}`.
//!
//! Divergence (documented): the builtin table shadows user search functions
//! for utf-8/ascii/latin-1 names, and search results are not strictly
//! validated as 4-tuples (pon tuple subclasses do not support subscripting
//! yet, so consumers use the `CodecInfo` attribute surface instead).
//!
//! `str.encode` / `bytes.decode` / `bytes(str, enc)` / `bytearray(str, enc)`
//! route through [`encode_str_to_vec`] / [`decode_bytes_to_string`]: builtin
//! encodings hit the Rust cores directly (typed `Unicode*Error` failures with
//! CPython-shaped messages), anything else falls back to the registry.
//!
//! Error handlers: `strict`, `ignore`, `replace` are fully implemented (both
//! as handler objects and inside the builtin cores, which additionally accept
//! `backslashreplace` and — encode-side — `xmlcharrefreplace`).
//! `backslashreplace`/`xmlcharrefreplace`/`namereplace`/`surrogateescape`/
//! `surrogatepass` exist as registered handler *objects* so
//! `codecs.lookup_error(...)` succeeds at `import codecs` time, but raise
//! `NotImplementedError` when invoked (pon strings are Rust UTF-8 strings and
//! cannot carry the lone surrogates `surrogateescape`/`surrogatepass`
//! produce).  Decode-side, the builtin cores degrade a requested
//! `surrogateescape` to the strict-mode `UnicodeDecodeError` instead of a
//! `LookupError` — the CPython-shaped FAILURE for the fs-encoding probes
//! that only catch `UnicodeDecodeError` (documented divergence: CPython
//! would succeed with lone surrogates; see [`utf8_decode_core`]).
//! Exotic codec functions (`utf_16_*`, `utf_32_*`, `utf_7_*`,
//! `charmap_*`, `escape_*`, `readbuffer_encode`) are exported so
//! `from _codecs import *` succeeds, and raise `NotImplementedError` when
//! called.
//!
//! GC: registered search functions, error handlers, cached `CodecInfo`
//! objects and the builtin codec function objects live in native statics that
//! the collector cannot trace; [`gc_held_roots`] reports them, mirroring
//! `_contextvars`.

use std::{borrow::Cow, collections::HashMap, ptr, sync::Mutex};

use super::{
	builtins_mod::{VARIADIC_ARITY, alloc_tuple},
	install_module,
};
use crate::{
	abi,
	intern::intern,
	object::PyObject,
	types::{
		bytearray_ as bytearray_type, bytes_ as bytes_type, exc::ExceptionKind, type_ as type_mod,
	},
};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

// ---------------------------------------------------------------------------
// Registry state

#[derive(Default)]
struct State {
	/// Registered search functions, in registration order.
	search_functions: Vec<usize>,
	/// C-normalized encoding name -> CodecInfo object.
	cache:            HashMap<String, usize>,
	/// Error handler name -> callable object.
	error_handlers:   HashMap<String, usize>,
	/// The module's own codec function objects, reused when constructing
	/// builtin `CodecInfo`s (index by [`BuiltinCodec`]).
	builtin_fns:      Option<[usize; 10]>,
	/// Whether the lazy one-shot `import encodings` ran (or failed) already.
	encodings_probed: bool,
}

static STATE: std::sync::LazyLock<Mutex<State>> =
	std::sync::LazyLock::new(|| Mutex::new(State::default()));

fn state() -> std::sync::MutexGuard<'static, State> {
	STATE.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// GC roots held by `_codecs` state: search functions, error handlers,
/// cached `CodecInfo` objects, and the builtin codec function objects.
/// Consumed by `crate::abi::collect` while the runtime lock is held; takes
/// only this module's mutex and never re-enters the runtime.
pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
	let state = state();
	let mut roots = Vec::with_capacity(
		state.search_functions.len() + state.cache.len() + state.error_handlers.len() + 10,
	);
	roots.extend(state.search_functions.iter().map(|&p| p as *mut PyObject));
	roots.extend(state.cache.values().map(|&p| p as *mut PyObject));
	roots.extend(state.error_handlers.values().map(|&p| p as *mut PyObject));
	if let Some(fns) = state.builtin_fns {
		roots.extend(fns.iter().map(|&p| p as *mut PyObject));
	}
	roots.retain(|p| !p.is_null());
	roots
}

// ---------------------------------------------------------------------------
// Module construction

#[derive(Clone, Copy)]
enum BuiltinCodec {
	Utf8             = 0,
	Ascii            = 1,
	Latin1           = 2,
	UnicodeEscape    = 3,
	RawUnicodeEscape = 4,
}

impl BuiltinCodec {
	/// The `CodecInfo.name` CPython's `encodings` package advertises.
	fn canonical_name(self) -> &'static str {
		match self {
			BuiltinCodec::Utf8 => "utf-8",
			BuiltinCodec::Ascii => "ascii",
			BuiltinCodec::Latin1 => "iso8859-1",
			BuiltinCodec::UnicodeEscape => "unicode-escape",
			BuiltinCodec::RawUnicodeEscape => "raw-unicode-escape",
		}
	}

	/// Vendored `encodings.<name>` module supplying the incremental and stream
	/// helper classes paired with the builtin encode/decode functions.
	fn encodings_module(self) -> &'static str {
		match self {
			BuiltinCodec::Utf8 => "encodings.utf_8",
			BuiltinCodec::Ascii => "encodings.ascii",
			BuiltinCodec::Latin1 => "encodings.latin_1",
			BuiltinCodec::UnicodeEscape => "encodings.unicode_escape",
			BuiltinCodec::RawUnicodeEscape => "encodings.raw_unicode_escape",
		}
	}

	/// The quoted codec name used in CPython Unicode error messages.
	fn error_name(self) -> &'static str {
		match self {
			BuiltinCodec::Utf8 => "utf-8",
			BuiltinCodec::Ascii => "ascii",
			BuiltinCodec::Latin1 => "latin-1",
			BuiltinCodec::UnicodeEscape => "unicodeescape",
			BuiltinCodec::RawUnicodeEscape => "rawunicodeescape",
		}
	}
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let mut attrs: Vec<(u32, *mut PyObject)> = Vec::with_capacity(52);
	// SAFETY: Runtime allocation helper; NULL is checked below.
	let name_obj = unsafe { abi::pon_const_str(b"_codecs".as_ptr(), 7) };
	if name_obj.is_null() {
		return Err("failed to allocate _codecs.__name__".to_owned());
	}
	attrs.push((intern("__name__"), name_obj));

	let mut module_fn = |name: &str, entry: BuiltinFn| -> Result<*mut PyObject, String> {
		// SAFETY: `entry` is a live builtin entry point with the runtime
		// calling convention.
		let function =
			unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
		if function.is_null() {
			return Err(format!("failed to allocate _codecs.{name}"));
		}
		attrs.push((intern(name), function));
		Ok(function)
	};

	module_fn("register", register_entry)?;
	module_fn("unregister", unregister_entry)?;
	module_fn("lookup", lookup_entry)?;
	module_fn("encode", encode_entry)?;
	module_fn("decode", decode_entry)?;
	module_fn("register_error", register_error_entry)?;
	module_fn("lookup_error", lookup_error_entry)?;
	module_fn("_unregister_error", unregister_error_entry)?;

	let utf_8_encode = module_fn("utf_8_encode", utf_8_encode_entry)?;
	let utf_8_decode = module_fn("utf_8_decode", utf_8_decode_entry)?;
	let ascii_encode = module_fn("ascii_encode", ascii_encode_entry)?;
	let ascii_decode = module_fn("ascii_decode", ascii_decode_entry)?;
	let latin_1_encode = module_fn("latin_1_encode", latin_1_encode_entry)?;
	let latin_1_decode = module_fn("latin_1_decode", latin_1_decode_entry)?;
	let unicode_escape_encode = module_fn("unicode_escape_encode", unicode_escape_encode_entry)?;
	let unicode_escape_decode = module_fn("unicode_escape_decode", unicode_escape_decode_entry)?;
	let raw_unicode_escape_encode =
		module_fn("raw_unicode_escape_encode", raw_unicode_escape_encode_entry)?;
	let raw_unicode_escape_decode =
		module_fn("raw_unicode_escape_decode", raw_unicode_escape_decode_entry)?;

	for (name, entry) in STUB_CODEC_FNS {
		module_fn(name, *entry)?;
	}
	module_fn("charmap_encode", charmap_encode_entry)?;
	module_fn("charmap_decode", charmap_decode_entry)?;
	module_fn("charmap_build", charmap_build_entry)?;

	let mut handlers: Vec<(&str, *mut PyObject)> = Vec::with_capacity(8);
	for (name, entry) in [
		("strict", strict_errors_entry as BuiltinFn),
		("ignore", ignore_errors_entry),
		("replace", replace_errors_entry),
		("backslashreplace", backslashreplace_errors_entry),
		("xmlcharrefreplace", xmlcharrefreplace_errors_entry),
		("namereplace", namereplace_errors_entry),
		("surrogateescape", surrogateescape_errors_entry),
		("surrogatepass", surrogatepass_errors_entry),
	] {
		// SAFETY: `entry` is a live builtin entry point.
		let handler = unsafe {
			abi::pon_make_function(
				entry as *const u8,
				VARIADIC_ARITY,
				intern(&format!("{name}_errors")),
			)
		};
		if handler.is_null() {
			return Err(format!("failed to allocate the '{name}' error handler"));
		}
		handlers.push((name, handler));
	}

	{
		let mut state = state();
		state.builtin_fns = Some([
			utf_8_encode as usize,
			utf_8_decode as usize,
			ascii_encode as usize,
			ascii_decode as usize,
			latin_1_encode as usize,
			latin_1_decode as usize,
			unicode_escape_encode as usize,
			unicode_escape_decode as usize,
			raw_unicode_escape_encode as usize,
			raw_unicode_escape_decode as usize,
		]);
		for (name, handler) in handlers {
			state
				.error_handlers
				.entry(name.to_owned())
				.or_insert(handler as usize);
		}
	}

	install_module("_codecs", attrs)
}

/// Exported-but-not-implemented `_codecs` functions: enough surface for
/// `from _codecs import *` (and `encodings.*` module bodies that merely bind
/// them); calling one raises `NotImplementedError`.
macro_rules! stub_codec_fns {
    ($(($name:literal, $entry:ident)),+ $(,)?) => {
        $(
            unsafe extern "C" fn $entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
                raise_kind(
                    ExceptionKind::NotImplementedError,
                    concat!("_codecs.", $name, " is not implemented in pon (utf-8/ascii/latin-1 are native; see native/codecs.rs)"),
                )
            }
        )+
        static STUB_CODEC_FNS: &[(&str, BuiltinFn)] = &[$(($name, $entry as BuiltinFn)),+];
    };
}

stub_codec_fns!(
	("escape_encode", escape_encode_entry),
	("escape_decode", escape_decode_entry),
	("utf_7_encode", utf_7_encode_entry),
	("utf_7_decode", utf_7_decode_entry),
	("utf_16_encode", utf_16_encode_entry),
	("utf_16_le_encode", utf_16_le_encode_entry),
	("utf_16_be_encode", utf_16_be_encode_entry),
	("utf_16_decode", utf_16_decode_entry),
	("utf_16_le_decode", utf_16_le_decode_entry),
	("utf_16_be_decode", utf_16_be_decode_entry),
	("utf_16_ex_decode", utf_16_ex_decode_entry),
	("utf_32_encode", utf_32_encode_entry),
	("utf_32_le_encode", utf_32_le_encode_entry),
	("utf_32_be_encode", utf_32_be_encode_entry),
	("utf_32_decode", utf_32_decode_entry),
	("utf_32_le_decode", utf_32_le_decode_entry),
	("utf_32_be_decode", utf_32_be_decode_entry),
	("utf_32_ex_decode", utf_32_ex_decode_entry),
	("readbuffer_encode", readbuffer_encode_entry),
);

// ---------------------------------------------------------------------------
// Small helpers (contextvars idioms)

fn untag(object: *mut PyObject) -> *mut PyObject {
	crate::tag::untag_arg(object)
}

fn fail(message: impl Into<String>) -> *mut PyObject {
	crate::thread_state::pon_err_set(message);
	ptr::null_mut()
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

/// Borrows a `str` argument's text.
fn str_arg<'a>(object: *mut PyObject, what: &str) -> Result<&'a str, String> {
	let object = untag(object);
	// SAFETY: `untag` normalized the pointer; `unicode_text` type-checks.
	unsafe { type_mod::unicode_text(object) }
		.ok_or_else(|| format!("{what} must be str, not {}", value_type_name(object)))
}

/// Borrows a bytes-like argument's payload (bytes or bytearray).
fn bytes_arg<'a>(object: *mut PyObject, what: &str) -> Result<&'a [u8], String> {
	let object = untag(object);
	if object.is_null() {
		return Err(format!("{what} must be a bytes-like object, not NULL"));
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
	Err(format!("{what} must be a bytes-like object, not {}", value_type_name(object)))
}

/// Borrows codec input accepted by CPython's escape decoders: bytes,
/// bytearray, or a str reinterpreted as its UTF-8 bytes.
fn bytes_or_str_arg<'a>(object: *mut PyObject, what: &str) -> Result<Cow<'a, [u8]>, String> {
	let object = untag(object);
	if object.is_null() {
		return Err(format!("{what} must be a bytes-like object, not NULL"));
	}
	// SAFETY: `untag` normalized the pointer; `unicode_text` type-checks.
	if let Some(text) = unsafe { type_mod::unicode_text(object) } {
		return Ok(Cow::Borrowed(text.as_bytes()));
	}
	// SAFETY: A non-NULL heap object carries a live header.
	let ty = unsafe { (*object).ob_type };
	if bytes_type::is_bytes_type(ty) {
		// SAFETY: Type check above proved the layout.
		return Ok(Cow::Borrowed(unsafe { (*object.cast::<bytes_type::PyBytes>()).as_slice() }));
	}
	if bytearray_type::is_bytearray_type(ty) {
		// SAFETY: Type check above proved the layout.
		return Ok(Cow::Borrowed(unsafe {
			(*object.cast::<bytearray_type::PyByteArray>()).as_slice()
		}));
	}
	Err(format!("{what} must be a bytes-like object, not {}", value_type_name(object)))
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

/// `errors` argument at `args[idx]`: missing or `None` -> `"strict"`.
fn errors_arg<'a>(args: &[*mut PyObject], idx: usize) -> Result<&'a str, String> {
	match args.get(idx).copied() {
		None => Ok("strict"),
		Some(value) if value.is_null() || is_none(untag(value)) => Ok("strict"),
		Some(value) => str_arg(value, "errors"),
	}
}

fn int_of(object: *mut PyObject) -> Option<i64> {
	unsafe { crate::types::int::to_i64(object) }
}

fn getattr(object: *mut PyObject, name: &str) -> *mut PyObject {
	// SAFETY: Attribute dispatch tolerates a null feedback cell.
	unsafe { abi::pon_get_attr(object, intern(name), ptr::null_mut()) }
}

/// `(payload, consumed)` result tuple shared by every codec function.
fn codec_result(payload: *mut PyObject, consumed: usize) -> *mut PyObject {
	if payload.is_null() {
		return ptr::null_mut();
	}
	let consumed = alloc_int_object(consumed as i64);
	if consumed.is_null() {
		return ptr::null_mut();
	}
	alloc_tuple(vec![payload, consumed])
}

// ---------------------------------------------------------------------------
// Builtin codec cores

/// A failed builtin codec call, mapped onto the matching typed exception.
#[derive(Debug)]
pub(crate) enum CoreError {
	/// -> `UnicodeDecodeError` with a CPython-shaped message.
	Decode(String),
	/// -> `UnicodeEncodeError` with a CPython-shaped message.
	Encode(String),
	/// Unknown/unsupported error handler -> `LookupError`, like CPython's
	/// lazy handler resolution (only consulted once an error actually occurs).
	Handler(String),
}

impl CoreError {
	pub(crate) fn raise(&self) -> *mut PyObject {
		match self {
			CoreError::Decode(message) => raise_kind(ExceptionKind::UnicodeDecodeError, message),
			CoreError::Encode(message) => raise_kind(ExceptionKind::UnicodeEncodeError, message),
			CoreError::Handler(message) => raise_kind(ExceptionKind::LookupError, message),
		}
	}
}

fn unknown_handler(errors: &str) -> CoreError {
	CoreError::Handler(format!("unknown error handler name '{errors}'"))
}

/// CPython's repr of the offending character in encode error messages.
fn escape_char(ch: char) -> String {
	let code = ch as u32;
	if code < 0x100 {
		format!("\\x{code:02x}")
	} else if code < 0x1_0000 {
		format!("\\u{code:04x}")
	} else {
		format!("\\U{code:08x}")
	}
}

fn encode_error_message(
	codec: BuiltinCodec,
	text: &str,
	start: usize,
	end: usize,
	limit: u32,
) -> String {
	if end - start == 1 {
		let ch = text.chars().nth(start).unwrap_or('\u{FFFD}');
		format!(
			"'{}' codec can't encode character '{}' in position {}: ordinal not in range({})",
			codec.error_name(),
			escape_char(ch),
			start,
			limit,
		)
	} else {
		format!(
			"'{}' codec can't encode characters in position {}-{}: ordinal not in range({})",
			codec.error_name(),
			start,
			end - 1,
			limit,
		)
	}
}

/// Shared single-byte-range encoder: `ascii` (`limit` 128) and `latin-1`
/// (`limit` 256).  Positions are code point indices, as in CPython.
fn encode_charrange(
	codec: BuiltinCodec,
	text: &str,
	errors: &str,
	limit: u32,
) -> Result<Vec<u8>, CoreError> {
	let mut out = Vec::with_capacity(text.len());
	let chars: Vec<char> = text.chars().collect();
	let mut pos = 0usize;
	while pos < chars.len() {
		let ch = chars[pos];
		if (ch as u32) < limit {
			out.push(ch as u8);
			pos += 1;
			continue;
		}
		let start = pos;
		let mut end = pos;
		while end < chars.len() && (chars[end] as u32) >= limit {
			end += 1;
		}
		match errors {
			"strict" => {
				return Err(CoreError::Encode(encode_error_message(codec, text, start, end, limit)));
			},
			"ignore" => {},
			"replace" => out.extend(std::iter::repeat_n(b'?', end - start)),
			"backslashreplace" => {
				for &ch in &chars[start..end] {
					out.extend_from_slice(escape_char(ch).as_bytes());
				}
			},
			"xmlcharrefreplace" => {
				for &ch in &chars[start..end] {
					out.extend_from_slice(format!("&#{};", ch as u32).as_bytes());
				}
			},
			_ => return Err(unknown_handler(errors)),
		}
		pos = end;
	}
	Ok(out)
}

/// `utf-8` encode never fails: pon strings are Rust UTF-8 strings, so lone
/// surrogates (the only strict-mode failure) cannot occur.
pub(crate) fn utf8_encode_core(text: &str) -> Vec<u8> {
	text.as_bytes().to_vec()
}

pub(crate) fn ascii_encode_core(text: &str, errors: &str) -> Result<Vec<u8>, CoreError> {
	encode_charrange(BuiltinCodec::Ascii, text, errors, 128)
}

pub(crate) fn latin1_encode_core(text: &str, errors: &str) -> Result<Vec<u8>, CoreError> {
	encode_charrange(BuiltinCodec::Latin1, text, errors, 256)
}

fn utf8_decode_error_message(bytes: &[u8], start: usize, span: usize, reason: &str) -> String {
	if span == 1 {
		format!(
			"'utf-8' codec can't decode byte 0x{:02x} in position {}: {}",
			bytes[start], start, reason
		)
	} else {
		format!(
			"'utf-8' codec can't decode bytes in position {}-{}: {}",
			start,
			start + span - 1,
			reason
		)
	}
}

/// Incremental `utf-8` decode: returns the decoded prefix and the number of
/// bytes consumed.  With `final_` unset, an incomplete trailing sequence is
/// left unconsumed (incremental-decoder contract); otherwise it is an
/// `unexpected end of data` error.
pub(crate) fn utf8_decode_core(
	bytes: &[u8],
	errors: &str,
	final_: bool,
) -> Result<(String, usize), CoreError> {
	let mut out = String::with_capacity(bytes.len());
	let mut pos = 0usize;
	while pos < bytes.len() {
		match core::str::from_utf8(&bytes[pos..]) {
			Ok(tail) => {
				out.push_str(tail);
				pos = bytes.len();
			},
			Err(error) => {
				let valid = error.valid_up_to();
				// SAFETY: `valid_up_to` bytes were just validated as UTF-8.
				out.push_str(unsafe { core::str::from_utf8_unchecked(&bytes[pos..pos + valid]) });
				pos += valid;
				let (span, reason) = match error.error_len() {
					Some(len) => {
						let reason = match bytes[pos] {
							0x80..=0xc1 | 0xf5..=0xff => "invalid start byte",
							_ => "invalid continuation byte",
						};
						(len, reason)
					},
					None if final_ => (bytes.len() - pos, "unexpected end of data"),
					None => return Ok((out, pos)),
				};
				match errors {
					"strict" => {
						return Err(CoreError::Decode(utf8_decode_error_message(
							bytes, pos, span, reason,
						)));
					},
					"ignore" => {},
					"replace" => out.push('\u{FFFD}'),
					"backslashreplace" => {
						for byte in &bytes[pos..pos + span] {
							out.push_str(&format!("\\x{byte:02x}"));
						}
					},
					// pon str cannot carry the lone surrogates PEP 383
					// produces, so a requested 'surrogateescape' degrades to
					// the strict-mode UnicodeDecodeError rather than a
					// LookupError (documented divergence: CPython SUCCEEDS
					// here, mapping the bytes to U+DC80..U+DCFF).  Callers
					// probing fs-encoding decodability — os.fsdecode and
					// `test.support.os_helper`'s TESTFN_UNDECODABLE loop
					// (`except UnicodeDecodeError`) — keep their CPython
					// control flow: the except arm engages instead of a
					// LookupError escaping the probe.
					"surrogateescape" => {
						return Err(CoreError::Decode(utf8_decode_error_message(
							bytes, pos, span, reason,
						)));
					},
					_ => return Err(unknown_handler(errors)),
				}
				pos += span;
			},
		}
	}
	Ok((out, pos))
}

pub(crate) fn ascii_decode_core(bytes: &[u8], errors: &str) -> Result<String, CoreError> {
	let mut out = String::with_capacity(bytes.len());
	for (pos, &byte) in bytes.iter().enumerate() {
		if byte.is_ascii() {
			out.push(char::from(byte));
			continue;
		}
		match errors {
			"strict" => {
				return Err(CoreError::Decode(format!(
					"'ascii' codec can't decode byte 0x{byte:02x} in position {pos}: ordinal not in \
					 range(128)"
				)));
			},
			"ignore" => {},
			"replace" => out.push('\u{FFFD}'),
			"backslashreplace" => out.push_str(&format!("\\x{byte:02x}")),
			// Degrades to the strict-mode UnicodeDecodeError; see the
			// `utf8_decode_core` surrogateescape arm for the rationale.
			"surrogateescape" => {
				return Err(CoreError::Decode(format!(
					"'ascii' codec can't decode byte 0x{byte:02x} in position {pos}: ordinal not in \
					 range(128)"
				)));
			},
			_ => return Err(unknown_handler(errors)),
		}
	}
	Ok(out)
}

/// `latin-1` decode is total: every byte maps to U+00..U+FF.
pub(crate) fn latin1_decode_core(bytes: &[u8]) -> String {
	bytes.iter().map(|&byte| char::from(byte)).collect()
}

#[derive(Clone, Copy)]
enum EscapeCodec {
	Unicode,
	RawUnicode,
}

impl EscapeCodec {
	fn error_name(self) -> &'static str {
		match self {
			EscapeCodec::Unicode => "unicodeescape",
			EscapeCodec::RawUnicode => "rawunicodeescape",
		}
	}

	fn out_of_range_reason(self) -> &'static str {
		match self {
			EscapeCodec::Unicode => "illegal Unicode character",
			EscapeCodec::RawUnicode => "\\Uxxxxxxxx out of range",
		}
	}
}

fn escape_decode_error_message(
	codec: EscapeCodec,
	bytes: &[u8],
	start: usize,
	end: usize,
	reason: &str,
) -> String {
	if end == start + 1 {
		format!(
			"'{}' codec can't decode byte 0x{:02x} in position {}: {}",
			codec.error_name(),
			bytes[start],
			start,
			reason,
		)
	} else {
		format!(
			"'{}' codec can't decode bytes in position {}-{}: {}",
			codec.error_name(),
			start,
			end - 1,
			reason,
		)
	}
}

fn handle_escape_decode_error(
	out: &mut String,
	bytes: &[u8],
	start: usize,
	end: usize,
	errors: &str,
	message: String,
) -> Result<(), CoreError> {
	match errors {
		"strict" => Err(CoreError::Decode(message)),
		"ignore" => Ok(()),
		"replace" => {
			out.push('\u{FFFD}');
			Ok(())
		},
		"backslashreplace" => {
			for byte in &bytes[start..end] {
				out.push_str(&format!("\\x{byte:02x}"));
			}
			Ok(())
		},
		"surrogateescape" => Err(CoreError::Decode(message)),
		_ => Err(unknown_handler(errors)),
	}
}

fn hex_value(byte: u8) -> Option<u32> {
	match byte {
		b'0'..=b'9' => Some(u32::from(byte - b'0')),
		b'a'..=b'f' => Some(u32::from(byte - b'a') + 10),
		b'A'..=b'F' => Some(u32::from(byte - b'A') + 10),
		_ => None,
	}
}

fn parse_hex_escape(
	bytes: &[u8],
	start: usize,
	digits: usize,
	final_: bool,
	codec: EscapeCodec,
	truncated_reason: &'static str,
) -> Result<Option<(u32, usize)>, CoreError> {
	let mut value = 0u32;
	let mut cursor = start + 2;
	let end = start + 2 + digits;
	while cursor < bytes.len() && cursor < end {
		let Some(digit) = hex_value(bytes[cursor]) else {
			break;
		};
		value = (value << 4) | digit;
		cursor += 1;
	}
	if cursor == end {
		return Ok(Some((value, cursor)));
	}
	if cursor == bytes.len() && !final_ {
		return Ok(None);
	}
	let error_end = cursor.max(start + 2);
	Err(CoreError::Decode(escape_decode_error_message(
		codec,
		bytes,
		start,
		error_end,
		truncated_reason,
	)))
}

fn push_unicode_codepoint(
	out: &mut String,
	bytes: &[u8],
	start: usize,
	end: usize,
	value: u32,
	codec: EscapeCodec,
	errors: &str,
) -> Result<(), CoreError> {
	if let Some(ch) = char::from_u32(value) {
		out.push(ch);
		return Ok(());
	}
	let message = escape_decode_error_message(codec, bytes, start, end, codec.out_of_range_reason());
	handle_escape_decode_error(out, bytes, start, end, errors, message)
}

fn unicode_name_lookup(name: &str) -> Option<char> {
	match name {
		"BULLET" => Some('\u{2022}'),
		_ => None,
	}
}

fn decode_unicode_name_escape(
	out: &mut String,
	bytes: &[u8],
	start: usize,
	final_: bool,
	errors: &str,
) -> Result<Option<usize>, CoreError> {
	if start + 2 >= bytes.len() {
		if final_ {
			let end = bytes.len();
			let message = escape_decode_error_message(
				EscapeCodec::Unicode,
				bytes,
				start,
				end,
				"malformed \\N character escape",
			);
			handle_escape_decode_error(out, bytes, start, end, errors, message)?;
			return Ok(Some(end));
		}
		return Ok(None);
	}
	if bytes[start + 2] != b'{' {
		let end = start + 2;
		let message = escape_decode_error_message(
			EscapeCodec::Unicode,
			bytes,
			start,
			end,
			"malformed \\N character escape",
		);
		handle_escape_decode_error(out, bytes, start, end, errors, message)?;
		return Ok(Some(end));
	}
	let mut cursor = start + 3;
	while cursor < bytes.len() && bytes[cursor] != b'}' {
		cursor += 1;
	}
	if cursor == bytes.len() {
		if !final_ {
			return Ok(None);
		}
		let message = escape_decode_error_message(
			EscapeCodec::Unicode,
			bytes,
			start,
			bytes.len(),
			"malformed \\N character escape",
		);
		handle_escape_decode_error(out, bytes, start, bytes.len(), errors, message)?;
		return Ok(Some(bytes.len()));
	}
	let name = core::str::from_utf8(&bytes[start + 3..cursor]).unwrap_or("");
	if let Some(ch) = unicode_name_lookup(name) {
		out.push(ch);
		return Ok(Some(cursor + 1));
	}
	let message = escape_decode_error_message(
		EscapeCodec::Unicode,
		bytes,
		start,
		cursor + 1,
		"unknown Unicode character name",
	);
	handle_escape_decode_error(out, bytes, start, cursor + 1, errors, message)?;
	Ok(Some(cursor + 1))
}

fn decode_unicode_escape_core(
	bytes: &[u8],
	errors: &str,
	final_: bool,
	codec: EscapeCodec,
) -> Result<(String, usize), CoreError> {
	let mut out = String::with_capacity(bytes.len());
	let mut pos = 0usize;
	while pos < bytes.len() {
		let byte = bytes[pos];
		if byte != b'\\' {
			out.push(char::from(byte));
			pos += 1;
			continue;
		}
		let start = pos;
		if pos + 1 >= bytes.len() {
			if !final_ {
				return Ok((out, start));
			}
			if matches!(codec, EscapeCodec::RawUnicode) {
				out.push('\\');
				pos = start + 1;
				continue;
			}
			let message =
				escape_decode_error_message(codec, bytes, start, start + 1, "\\ at end of string");
			handle_escape_decode_error(&mut out, bytes, start, start + 1, errors, message)?;
			pos = start + 1;
			continue;
		}
		let escaped = bytes[pos + 1];
		if matches!(codec, EscapeCodec::RawUnicode) {
			match escaped {
				b'u' | b'U' => {},
				_ => {
					out.push('\\');
					out.push(char::from(escaped));
					pos += 2;
					continue;
				},
			}
		}
		match escaped {
			b'\n' if matches!(codec, EscapeCodec::Unicode) => {
				pos += 2;
			},
			b'\'' if matches!(codec, EscapeCodec::Unicode) => {
				out.push('\'');
				pos += 2;
			},
			b'"' if matches!(codec, EscapeCodec::Unicode) => {
				out.push('"');
				pos += 2;
			},
			b'\\' if matches!(codec, EscapeCodec::Unicode) => {
				out.push('\\');
				pos += 2;
			},
			b'a' if matches!(codec, EscapeCodec::Unicode) => {
				out.push('\u{0007}');
				pos += 2;
			},
			b'b' if matches!(codec, EscapeCodec::Unicode) => {
				out.push('\u{0008}');
				pos += 2;
			},
			b'f' if matches!(codec, EscapeCodec::Unicode) => {
				out.push('\u{000C}');
				pos += 2;
			},
			b'n' if matches!(codec, EscapeCodec::Unicode) => {
				out.push('\n');
				pos += 2;
			},
			b'r' if matches!(codec, EscapeCodec::Unicode) => {
				out.push('\r');
				pos += 2;
			},
			b't' if matches!(codec, EscapeCodec::Unicode) => {
				out.push('\t');
				pos += 2;
			},
			b'v' if matches!(codec, EscapeCodec::Unicode) => {
				out.push('\u{000B}');
				pos += 2;
			},
			b'0'..=b'7' if matches!(codec, EscapeCodec::Unicode) => {
				let mut value = u32::from(escaped - b'0');
				let mut cursor = pos + 2;
				let limit = bytes.len().min(pos + 4);
				while cursor < limit {
					match bytes[cursor] {
						b'0'..=b'7' => {
							value = (value << 3) | u32::from(bytes[cursor] - b'0');
							cursor += 1;
						},
						_ => break,
					}
				}
				out.push(char::from_u32(value).unwrap_or('\u{FFFD}'));
				pos = cursor;
			},
			b'x' if matches!(codec, EscapeCodec::Unicode) => {
				match parse_hex_escape(bytes, start, 2, final_, codec, "truncated \\xXX escape")? {
					Some((value, end)) => {
						out.push(char::from_u32(value).unwrap_or('\u{FFFD}'));
						pos = end;
					},
					None => return Ok((out, start)),
				}
			},
			b'u' => {
				match parse_hex_escape(bytes, start, 4, final_, codec, "truncated \\uXXXX escape")? {
					Some((value, end)) => {
						push_unicode_codepoint(&mut out, bytes, start, end, value, codec, errors)?;
						pos = end;
					},
					None => return Ok((out, start)),
				}
			},
			b'U' => {
				match parse_hex_escape(bytes, start, 8, final_, codec, "truncated \\UXXXXXXXX escape")?
				{
					Some((value, end)) => {
						push_unicode_codepoint(&mut out, bytes, start, end, value, codec, errors)?;
						pos = end;
					},
					None => return Ok((out, start)),
				}
			},
			b'N' if matches!(codec, EscapeCodec::Unicode) => {
				match decode_unicode_name_escape(&mut out, bytes, start, final_, errors)? {
					Some(end) => pos = end,
					None => return Ok((out, start)),
				}
			},
			_ => {
				out.push('\\');
				out.push(char::from(escaped));
				pos += 2;
			},
		}
	}
	Ok((out, pos))
}

pub(crate) fn unicode_escape_decode_core(
	bytes: &[u8],
	errors: &str,
	final_: bool,
) -> Result<(String, usize), CoreError> {
	decode_unicode_escape_core(bytes, errors, final_, EscapeCodec::Unicode)
}

pub(crate) fn raw_unicode_escape_decode_core(
	bytes: &[u8],
	errors: &str,
	final_: bool,
) -> Result<(String, usize), CoreError> {
	decode_unicode_escape_core(bytes, errors, final_, EscapeCodec::RawUnicode)
}

fn push_unicode_escape(out: &mut Vec<u8>, code: u32) {
	if code < 0x100 {
		out.extend_from_slice(format!("\\x{code:02x}").as_bytes());
	} else if code < 0x1_0000 {
		out.extend_from_slice(format!("\\u{code:04x}").as_bytes());
	} else {
		out.extend_from_slice(format!("\\U{code:08x}").as_bytes());
	}
}

pub(crate) fn unicode_escape_encode_core(text: &str, _errors: &str) -> Result<Vec<u8>, CoreError> {
	let mut out = Vec::with_capacity(text.len());
	for ch in text.chars() {
		match ch {
			'\\' => out.extend_from_slice(b"\\\\"),
			'\t' => out.extend_from_slice(b"\\t"),
			'\n' => out.extend_from_slice(b"\\n"),
			'\r' => out.extend_from_slice(b"\\r"),
			' '..='~' => out.push(ch as u8),
			_ => push_unicode_escape(&mut out, ch as u32),
		}
	}
	Ok(out)
}

pub(crate) fn raw_unicode_escape_encode_core(
	text: &str,
	_errors: &str,
) -> Result<Vec<u8>, CoreError> {
	let mut out = Vec::with_capacity(text.len());
	for ch in text.chars() {
		let code = ch as u32;
		if code <= 0xff {
			out.push(code as u8);
		} else {
			push_unicode_escape(&mut out, code);
		}
	}
	Ok(out)
}

// ---------------------------------------------------------------------------
// Name normalization and the builtin table

/// CPython's C-level normalization (`normalizestring`): lowercase, spaces to
/// underscores.  This form keys the cache and is passed to search functions.
fn c_normalize(encoding: &str) -> String {
	encoding
		.chars()
		.map(|ch| {
			if ch == ' ' {
				'_'
			} else {
				ch.to_ascii_lowercase()
			}
		})
		.collect()
}

/// `encodings.normalize_encoding`-style collapse used only for the builtin
/// table: runs of non-alphanumerics become single underscores.
fn collapse_normalize(encoding: &str) -> String {
	let mut out = String::with_capacity(encoding.len());
	let mut pending_sep = false;
	for ch in encoding.chars() {
		if ch.is_ascii_alphanumeric() {
			if pending_sep && !out.is_empty() {
				out.push('_');
			}
			pending_sep = false;
			out.push(ch.to_ascii_lowercase());
		} else {
			pending_sep = true;
		}
	}
	out
}

/// Builtin table: collapsed alias -> codec (aliases from
/// `Lib/encodings/aliases.py`).
fn builtin_codec(collapsed: &str) -> Option<BuiltinCodec> {
	match collapsed {
		// "locale" (PEP 597): the current locale encoding — UTF-8 on every
		// host pon supports (`locale.getencoding()` on macOS/Linux).
		"utf_8" | "u8" | "utf" | "utf8" | "utf8_ucs2" | "utf8_ucs4" | "cp65001" | "locale" => {
			Some(BuiltinCodec::Utf8)
		},
		"ascii" | "646" | "ansi_x3_4_1968" | "ansi_x3_4_1986" | "cp367" | "csascii" | "ibm367"
		| "iso646_us" | "iso_646_irv_1991" | "iso_ir_6" | "us" | "us_ascii" => Some(BuiltinCodec::Ascii),
		"latin_1" | "8859" | "cp819" | "csisolatin1" | "ibm819" | "iso8859" | "iso8859_1"
		| "iso_8859_1" | "iso_8859_1_1987" | "iso_ir_100" | "l1" | "latin" | "latin1" => {
			Some(BuiltinCodec::Latin1)
		},
		"unicode_escape" | "unicodeescape" => Some(BuiltinCodec::UnicodeEscape),
		"raw_unicode_escape" | "rawunicodeescape" => Some(BuiltinCodec::RawUnicodeEscape),
		_ => None,
	}
}

/// Canonical name for a natively-served text encoding, or `None` for names
/// the builtin table cannot resolve.  Consumed by `native::io` so `open()` /
/// `TextIOWrapper` accept every encoding the read/write paths can actually
/// decode/encode (Cython opens sources with `encoding='ASCII'`).
pub(crate) fn canonical_text_encoding(name: &str) -> Option<&'static str> {
	builtin_codec(&collapse_normalize(name)).map(BuiltinCodec::canonical_name)
}

// ---------------------------------------------------------------------------
// Registry: lookup

/// Constructs a `codecs.CodecInfo` for a builtin codec by calling the Python
/// class with this module's own codec function objects — the same pairing
/// `encodings/<name>.py` would produce.
fn build_builtin_codec_info(codec: BuiltinCodec) -> *mut PyObject {
	let builtin_fns = match state().builtin_fns {
		Some(fns) => fns,
		None => return fail("_codecs module state is not initialized"),
	};
	let (encode_fn, decode_fn) = match codec {
		BuiltinCodec::Utf8 => (builtin_fns[0], builtin_fns[1]),
		BuiltinCodec::Ascii => (builtin_fns[2], builtin_fns[3]),
		BuiltinCodec::Latin1 => (builtin_fns[4], builtin_fns[5]),
		BuiltinCodec::UnicodeEscape => (builtin_fns[6], builtin_fns[7]),
		BuiltinCodec::RawUnicodeEscape => (builtin_fns[8], builtin_fns[9]),
	};

	// `codecs` may not be imported yet when `_codecs.lookup` is called
	// directly; importing it here cannot recurse into `lookup` (module scope
	// only *defines* names and resolves error handlers).
	let codecs_module = match crate::import::cached_module(intern("codecs")) {
		Some(module) => module,
		// SAFETY: Import entry point follows the NULL-sentinel error contract.
		None => unsafe { crate::import::pon_import_name(intern("codecs"), ptr::null(), 0, 0) },
	};
	if codecs_module.is_null() {
		return ptr::null_mut();
	}
	let codec_info_cls = getattr(codecs_module, "CodecInfo");
	if codec_info_cls.is_null() {
		return ptr::null_mut();
	}
	let encodings_name = intern(codec.encodings_module());
	let encodings_module = match crate::import::cached_module(encodings_name) {
		Some(module) => module,
		// SAFETY: Non-empty `fromlist` keeps the dotted import result on the
		// leaf module (`encodings.utf_8`), matching Python's `__import__`.
		None => unsafe {
			let fromlist = [intern("StreamWriter")];
			crate::import::pon_import_name(encodings_name, fromlist.as_ptr(), fromlist.len(), 0)
		},
	};
	if encodings_module.is_null() {
		return ptr::null_mut();
	}
	let incremental_encoder = getattr(encodings_module, "IncrementalEncoder");
	let incremental_decoder = getattr(encodings_module, "IncrementalDecoder");
	let stream_reader = getattr(encodings_module, "StreamReader");
	let stream_writer = getattr(encodings_module, "StreamWriter");
	if incremental_encoder.is_null()
		|| incremental_decoder.is_null()
		|| stream_reader.is_null()
		|| stream_writer.is_null()
	{
		return ptr::null_mut();
	}
	let name_obj = alloc_str_object(codec.canonical_name());
	if name_obj.is_null() {
		return ptr::null_mut();
	}
	let mut call_args = [
		encode_fn as *mut PyObject,
		decode_fn as *mut PyObject,
		incremental_encoder,
		incremental_decoder,
		stream_reader,
		stream_writer,
		name_obj,
	];
	// SAFETY: `call_args` is a live positional argument array.
	unsafe { abi::pon_call(codec_info_cls, call_args.as_mut_ptr(), call_args.len()) }
}

/// Registry lookup by encoding name; returns a cached `CodecInfo`-shaped
/// object or NULL with the exception set (`LookupError` on a full miss).
pub(crate) fn lookup_object(encoding: &str) -> *mut PyObject {
	let c_norm = c_normalize(encoding);
	if let Some(&cached) = state().cache.get(&c_norm) {
		return cached as *mut PyObject;
	}

	if let Some(codec) = builtin_codec(&collapse_normalize(&c_norm)) {
		let info = build_builtin_codec_info(codec);
		if info.is_null() {
			return ptr::null_mut();
		}
		state().cache.insert(c_norm, info as usize);
		return info;
	}

	match run_search_functions(&c_norm) {
		Ok(info) if !info.is_null() => {
			state().cache.insert(c_norm, info as usize);
			info
		},
		Ok(_) => raise_kind(ExceptionKind::LookupError, &format!("unknown encoding: {encoding}")),
		Err(()) => ptr::null_mut(),
	}
}

/// Runs registered search functions against the C-normalized name; on a full
/// miss lazily imports the vendored `encodings` package once (mirroring
/// CPython's registry init) and retries.  `Ok(NULL)` means "not found".
fn run_search_functions(c_norm: &str) -> Result<*mut PyObject, ()> {
	let name_obj = alloc_str_object(c_norm);
	if name_obj.is_null() {
		return Err(());
	}
	loop {
		let (snapshot, probed) = {
			let state = state();
			(state.search_functions.clone(), state.encodings_probed)
		};
		for function in snapshot {
			let mut call_args = [name_obj];
			// SAFETY: Live one-slot positional argument array.
			let result =
				unsafe { abi::pon_call(function as *mut PyObject, call_args.as_mut_ptr(), 1) };
			if result.is_null() {
				return Err(());
			}
			if !is_none(untag(result)) {
				return Ok(result);
			}
		}
		if probed {
			return Ok(ptr::null_mut());
		}
		state().encodings_probed = true;
		// Lazy one-shot `import encodings`: its `__init__` registers the
		// search function that serves the rest of the vendored stdlib
		// codecs.  Import failures propagate — a broken encodings package
		// should be loud, not a silent LookupError.
		// SAFETY: Import entry point follows the NULL-sentinel error contract.
		let module =
			unsafe { crate::import::pon_import_name(intern("encodings"), ptr::null(), 0, 0) };
		if module.is_null() {
			return Err(());
		}
	}
}

// ---------------------------------------------------------------------------
// Registry entry points

unsafe extern "C" fn register_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("_codecs.register received a NULL argv pointer");
	};
	let &[function] = args else {
		return raise_type_error("register() takes exactly one argument");
	};
	let function = untag(function);
	if function.is_null() {
		return raise_type_error("argument must be callable");
	}
	state().search_functions.push(function as usize);
	none()
}

unsafe extern "C" fn unregister_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("_codecs.unregister received a NULL argv pointer");
	};
	let &[function] = args else {
		return raise_type_error("unregister() takes exactly one argument");
	};
	let function = untag(function) as usize;
	{
		let mut state = state();
		state
			.search_functions
			.retain(|&registered| registered != function);
		// CPython clears the whole cache so entries served by the removed
		// search function cannot survive.
		state.cache.clear();
	}
	none()
}

unsafe extern "C" fn lookup_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("_codecs.lookup received a NULL argv pointer");
	};
	let &[encoding] = args else {
		return raise_type_error("lookup() takes exactly one argument");
	};
	let encoding = match str_arg(encoding, "lookup() argument") {
		Ok(text) => text,
		Err(message) => return raise_type_error(&message),
	};
	lookup_object(encoding)
}

unsafe extern "C" fn register_error_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("_codecs.register_error received a NULL argv pointer");
	};
	let &[name, handler] = args else {
		return raise_type_error("register_error() takes exactly 2 arguments");
	};
	let name = match str_arg(name, "name") {
		Ok(text) => text,
		Err(message) => return raise_type_error(&message),
	};
	let handler = untag(handler);
	if handler.is_null() {
		return raise_type_error("handler must be callable");
	}
	state()
		.error_handlers
		.insert(name.to_owned(), handler as usize);
	none()
}

unsafe extern "C" fn lookup_error_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("_codecs.lookup_error received a NULL argv pointer");
	};
	let &[name] = args else {
		return raise_type_error("lookup_error() takes exactly one argument");
	};
	let name = match str_arg(name, "name") {
		Ok(text) => text,
		Err(message) => return raise_type_error(&message),
	};
	match state().error_handlers.get(name).copied() {
		Some(handler) => handler as *mut PyObject,
		None => {
			raise_kind(ExceptionKind::LookupError, &format!("unknown error handler name '{name}'"))
		},
	}
}

/// Built-in error handler names refused by `_unregister_error`, mirroring
/// CPython's `codecs_builtin_error_handlers` table (the same eight seeded
/// into `state.error_handlers` by [`make_module`]).
const BUILTIN_ERROR_HANDLERS: [&str; 8] = [
	"strict",
	"ignore",
	"replace",
	"backslashreplace",
	"xmlcharrefreplace",
	"namereplace",
	"surrogateescape",
	"surrogatepass",
];

/// `_codecs._unregister_error(name)`: removes a handler registered through
/// `register_error`, returning whether one was removed.  Built-in handler
/// names raise ValueError (CPython `_PyCodec_UnregisterError` parity).
unsafe extern "C" fn unregister_error_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("_codecs._unregister_error received a NULL argv pointer");
	};
	let &[name] = args else {
		return raise_type_error("_unregister_error() takes exactly one argument");
	};
	let name = match str_arg(name, "name") {
		Ok(text) => text,
		Err(message) => return raise_type_error(&message),
	};
	if BUILTIN_ERROR_HANDLERS.contains(&name) {
		return raise_kind(
			ExceptionKind::ValueError,
			&format!("cannot un-register built-in error handler '{name}'"),
		);
	}
	let removed = state().error_handlers.remove(name).is_some();
	// SAFETY: Boolean constant allocator.
	unsafe { abi::pon_const_bool(removed.into()) }
}

// ---------------------------------------------------------------------------
// encode / decode module functions

/// Calls `info.encode`/`info.decode` and returns the transformed object
/// (element 0 of the `(result, consumed)` tuple the codec returns).
fn call_codec_info(
	info: *mut PyObject,
	method: &str,
	object: *mut PyObject,
	errors: &str,
) -> *mut PyObject {
	let function = getattr(info, method);
	if function.is_null() {
		return ptr::null_mut();
	}
	let errors_obj = alloc_str_object(errors);
	if errors_obj.is_null() {
		return ptr::null_mut();
	}
	let mut call_args = [object, errors_obj];
	// SAFETY: Live positional argument array.
	let result = unsafe { abi::pon_call(function, call_args.as_mut_ptr(), call_args.len()) };
	if result.is_null() {
		return ptr::null_mut();
	}
	let result = untag(result);
	// SAFETY: Heap pointer with a live header.
	if unsafe { crate::types::dict::type_name(result) } != Some("tuple") {
		return raise_type_error(&format!("codec {method}r must return a tuple"));
	}
	// SAFETY: Type check above proved the layout.
	let items = unsafe { (*result.cast::<crate::types::tuple::PyTuple>()).as_slice() };
	match items.first() {
		Some(&payload) => payload,
		None => raise_type_error(&format!("codec {method}r must return a non-empty tuple")),
	}
}

unsafe extern "C" fn encode_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("_codecs.encode received a NULL argv pointer");
	};
	if args.is_empty() || args.len() > 3 {
		return raise_type_error("encode() takes from 1 to 3 arguments");
	}
	let object = args[0];
	let encoding = match args.get(1).copied() {
		None => "utf-8",
		Some(value) => match str_arg(value, "encoding") {
			Ok(text) => text,
			Err(message) => return raise_type_error(&message),
		},
	};
	let errors = match errors_arg(args, 2) {
		Ok(errors) => errors,
		Err(message) => return raise_type_error(&message),
	};

	if let Some(codec) = builtin_codec(&collapse_normalize(encoding)) {
		if let Ok(text) = str_arg(object, "argument") {
			let encoded = match codec {
				BuiltinCodec::Utf8 => Ok(utf8_encode_core(text)),
				BuiltinCodec::Ascii => ascii_encode_core(text, errors),
				BuiltinCodec::Latin1 => latin1_encode_core(text, errors),
				BuiltinCodec::UnicodeEscape => unicode_escape_encode_core(text, errors),
				BuiltinCodec::RawUnicodeEscape => raw_unicode_escape_encode_core(text, errors),
			};
			return match encoded {
				Ok(bytes) => alloc_bytes_object(&bytes),
				Err(error) => error.raise(),
			};
		}
	}
	let info = lookup_object(encoding);
	if info.is_null() {
		return ptr::null_mut();
	}
	call_codec_info(info, "encode", object, errors)
}

unsafe extern "C" fn decode_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("_codecs.decode received a NULL argv pointer");
	};
	if args.is_empty() || args.len() > 3 {
		return raise_type_error("decode() takes from 1 to 3 arguments");
	}
	let object = args[0];
	let encoding = match args.get(1).copied() {
		None => "utf-8",
		Some(value) => match str_arg(value, "encoding") {
			Ok(text) => text,
			Err(message) => return raise_type_error(&message),
		},
	};
	let errors = match errors_arg(args, 2) {
		Ok(errors) => errors,
		Err(message) => return raise_type_error(&message),
	};

	if let Some(codec) = builtin_codec(&collapse_normalize(encoding)) {
		match codec {
			BuiltinCodec::UnicodeEscape | BuiltinCodec::RawUnicodeEscape => {
				if let Ok(data) = bytes_or_str_arg(object, "argument") {
					let decoded = match codec {
						BuiltinCodec::UnicodeEscape => {
							unicode_escape_decode_core(&data, errors, true).map(|(text, _)| text)
						},
						BuiltinCodec::RawUnicodeEscape => {
							raw_unicode_escape_decode_core(&data, errors, true).map(|(text, _)| text)
						},
						_ => unreachable!(),
					};
					return match decoded {
						Ok(text) => alloc_str_object(&text),
						Err(error) => error.raise(),
					};
				}
			},
			BuiltinCodec::Utf8 | BuiltinCodec::Ascii | BuiltinCodec::Latin1 => {
				if let Ok(bytes) = bytes_arg(object, "argument") {
					let decoded = match codec {
						BuiltinCodec::Utf8 => utf8_decode_core(bytes, errors, true).map(|(text, _)| text),
						BuiltinCodec::Ascii => ascii_decode_core(bytes, errors),
						BuiltinCodec::Latin1 => Ok(latin1_decode_core(bytes)),
						_ => unreachable!(),
					};
					return match decoded {
						Ok(text) => alloc_str_object(&text),
						Err(error) => error.raise(),
					};
				}
			},
		}
	}
	let info = lookup_object(encoding);
	if info.is_null() {
		return ptr::null_mut();
	}
	call_codec_info(info, "decode", object, errors)
}

// ---------------------------------------------------------------------------
// Builtin codec functions (the `encodings/utf_8.py`-visible surface)

unsafe extern "C" fn utf_8_encode_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("_codecs.utf_8_encode received a NULL argv pointer");
	};
	if args.is_empty() || args.len() > 2 {
		return raise_type_error("utf_8_encode() takes 1 or 2 arguments");
	}
	let text = match str_arg(args[0], "argument") {
		Ok(text) => text,
		Err(message) => return raise_type_error(&message),
	};
	if let Err(message) = errors_arg(args, 1) {
		return raise_type_error(&message);
	}
	codec_result(alloc_bytes_object(&utf8_encode_core(text)), text.chars().count())
}

unsafe extern "C" fn utf_8_decode_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("_codecs.utf_8_decode received a NULL argv pointer");
	};
	if args.is_empty() || args.len() > 3 {
		return raise_type_error("utf_8_decode() takes from 1 to 3 arguments");
	}
	let bytes = match bytes_arg(args[0], "argument") {
		Ok(bytes) => bytes,
		Err(message) => return raise_type_error(&message),
	};
	let errors = match errors_arg(args, 1) {
		Ok(errors) => errors,
		Err(message) => return raise_type_error(&message),
	};
	let final_ = match args.get(2).copied() {
		None => false,
		// SAFETY: Truthiness helper follows the error-sentinel contract.
		Some(value) => match unsafe { abi::pon_is_true(value) } {
			0 => false,
			1 => true,
			_ => return ptr::null_mut(),
		},
	};
	match utf8_decode_core(bytes, errors, final_) {
		Ok((text, consumed)) => codec_result(alloc_str_object(&text), consumed),
		Err(error) => error.raise(),
	}
}

fn escape_encode_entry_impl(
	args: &[*mut PyObject],
	pyname: &str,
	core: fn(&str, &str) -> Result<Vec<u8>, CoreError>,
) -> *mut PyObject {
	if args.is_empty() || args.len() > 2 {
		return raise_type_error(&format!("{pyname}_encode() takes 1 or 2 arguments"));
	}
	let text = match str_arg(args[0], "argument") {
		Ok(text) => text,
		Err(message) => return raise_type_error(&message),
	};
	let errors = match errors_arg(args, 1) {
		Ok(errors) => errors,
		Err(message) => return raise_type_error(&message),
	};
	match core(text, errors) {
		Ok(bytes) => codec_result(alloc_bytes_object(&bytes), text.chars().count()),
		Err(error) => error.raise(),
	}
}

fn escape_decode_entry_impl(
	args: &[*mut PyObject],
	pyname: &str,
	core: fn(&[u8], &str, bool) -> Result<(String, usize), CoreError>,
) -> *mut PyObject {
	if args.is_empty() || args.len() > 3 {
		return raise_type_error(&format!("{pyname}_decode() takes from 1 to 3 arguments"));
	}
	let data = match bytes_or_str_arg(args[0], "argument") {
		Ok(data) => data,
		Err(message) => return raise_type_error(&message),
	};
	let errors = match errors_arg(args, 1) {
		Ok(errors) => errors,
		Err(message) => return raise_type_error(&message),
	};
	let final_ = match args.get(2).copied() {
		None => true,
		// SAFETY: Truthiness helper follows the error-sentinel contract.
		Some(value) => match unsafe { abi::pon_is_true(value) } {
			0 => false,
			1 => true,
			_ => return ptr::null_mut(),
		},
	};
	match core(&data, errors, final_) {
		Ok((text, consumed)) => codec_result(alloc_str_object(&text), consumed),
		Err(error) => error.raise(),
	}
}

unsafe extern "C" fn unicode_escape_encode_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("_codecs.unicode_escape_encode received a NULL argv pointer");
	};
	escape_encode_entry_impl(args, "unicode_escape", unicode_escape_encode_core)
}

unsafe extern "C" fn unicode_escape_decode_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("_codecs.unicode_escape_decode received a NULL argv pointer");
	};
	escape_decode_entry_impl(args, "unicode_escape", unicode_escape_decode_core)
}

unsafe extern "C" fn raw_unicode_escape_encode_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("_codecs.raw_unicode_escape_encode received a NULL argv pointer");
	};
	escape_encode_entry_impl(args, "raw_unicode_escape", raw_unicode_escape_encode_core)
}

unsafe extern "C" fn raw_unicode_escape_decode_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("_codecs.raw_unicode_escape_decode received a NULL argv pointer");
	};
	escape_decode_entry_impl(args, "raw_unicode_escape", raw_unicode_escape_decode_core)
}

// ---------------------------------------------------------------------------
// Charmap codecs (`encodings/cp*.py` generated modules and zipfile's cp437)

/// Decoded mapping value for one byte: a produced string or "undefined".
fn charmap_mapped_char(mapping: *mut PyObject, byte: u8) -> Result<Option<String>, ()> {
	if is_none(mapping) {
		// CPython: a missing mapping decodes as latin-1.
		return Ok(Some(char::from(byte).to_string()));
	}
	if let Ok(table) = str_arg(mapping, "mapping") {
		let ch = table.chars().nth(usize::from(byte));
		return Ok(match ch {
			None | Some('\u{fffe}') => None,
			Some(ch) => Some(ch.to_string()),
		});
	}
	// Dict-style mapping: byte -> codepoint int, str, or None.
	let key = alloc_int_object(i64::from(byte));
	if key.is_null() {
		return Err(());
	}
	// SAFETY: live mapping and key; NULL result leaves a pending error.
	let value = unsafe { abi::pon_subscript_get(mapping, key, ptr::null_mut()) };
	if value.is_null() {
		crate::thread_state::pon_err_clear();
		return Ok(None);
	}
	if is_none(value) {
		return Ok(None);
	}
	if let Some(code) = object_int_value(value) {
		if !(0..=0x10_ffff).contains(&code) {
			return Ok(None);
		}
		return Ok(match char::from_u32(code as u32) {
			None | Some('\u{fffe}') => None,
			Some(ch) => Some(ch.to_string()),
		});
	}
	if let Ok(text) = str_arg(value, "mapping value") {
		if text == "\u{fffe}" {
			return Ok(None);
		}
		return Ok(Some(text.to_owned()));
	}
	Ok(None)
}

fn object_int_value(object: *mut PyObject) -> Option<i64> {
	// SAFETY: tolerant int readers accept any live (or tagged) object.
	unsafe { crate::types::int::to_bigint(object) }
		.and_then(|value| num_traits::ToPrimitive::to_i64(&value))
}

unsafe extern "C" fn charmap_decode_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("_codecs.charmap_decode received a NULL argv pointer");
	};
	if args.is_empty() || args.len() > 3 {
		return raise_type_error("charmap_decode() takes from 1 to 3 arguments");
	}
	let bytes = match bytes_arg(args[0], "argument") {
		Ok(bytes) => bytes,
		Err(message) => return raise_type_error(&message),
	};
	let errors = match errors_arg(args, 1) {
		Ok(errors) => errors,
		Err(message) => return raise_type_error(&message),
	};
	let mapping = args.get(2).copied().map_or_else(none, untag);
	let mut out = String::with_capacity(bytes.len());
	for (position, &byte) in bytes.iter().enumerate() {
		match charmap_mapped_char(mapping, byte) {
			Err(()) => return ptr::null_mut(),
			Ok(Some(text)) => out.push_str(&text),
			Ok(None) => match errors {
				"strict" => {
					return CoreError::Decode(format!(
						"'charmap' codec can't decode byte 0x{byte:02x} in position {position}: \
						 character maps to <undefined>"
					))
					.raise();
				},
				"ignore" => {},
				"replace" => out.push('\u{fffd}'),
				other => return unknown_handler(other).raise(),
			},
		}
	}
	codec_result(alloc_str_object(&out), bytes.len())
}

/// Encoded mapping value for one char: produced bytes or "undefined".
fn charmap_mapped_bytes(mapping: *mut PyObject, ch: char) -> Result<Option<Vec<u8>>, ()> {
	let key = alloc_int_object(i64::from(ch as u32));
	if key.is_null() {
		return Err(());
	}
	// SAFETY: live mapping and key; NULL result leaves a pending error.
	let value = unsafe { abi::pon_subscript_get(mapping, key, ptr::null_mut()) };
	if value.is_null() {
		crate::thread_state::pon_err_clear();
		return Ok(None);
	}
	if is_none(value) {
		return Ok(None);
	}
	if let Some(code) = object_int_value(value) {
		if !(0..=255).contains(&code) {
			return Ok(None);
		}
		return Ok(Some(vec![code as u8]));
	}
	if let Ok(bytes) = bytes_arg(value, "mapping value") {
		return Ok(Some(bytes.to_vec()));
	}
	Ok(None)
}

unsafe extern "C" fn charmap_encode_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("_codecs.charmap_encode received a NULL argv pointer");
	};
	if args.is_empty() || args.len() > 3 {
		return raise_type_error("charmap_encode() takes from 1 to 3 arguments");
	}
	let text = match str_arg(args[0], "argument") {
		Ok(text) => text,
		Err(message) => return raise_type_error(&message),
	};
	let errors = match errors_arg(args, 1) {
		Ok(errors) => errors,
		Err(message) => return raise_type_error(&message),
	};
	let mapping = args.get(2).copied().map_or_else(none, untag);
	if is_none(mapping) {
		// CPython: a missing mapping encodes as latin-1.
		return match encode_charrange(BuiltinCodec::Latin1, text, errors, 256) {
			Ok(bytes) => codec_result(alloc_bytes_object(&bytes), text.chars().count()),
			Err(error) => error.raise(),
		};
	}
	let mut out = Vec::with_capacity(text.len());
	for (position, ch) in text.chars().enumerate() {
		match charmap_mapped_bytes(mapping, ch) {
			Err(()) => return ptr::null_mut(),
			Ok(Some(bytes)) => out.extend_from_slice(&bytes),
			Ok(None) => match errors {
				"strict" => {
					return CoreError::Encode(format!(
						"'charmap' codec can't encode character '{}' in position {position}: character \
						 maps to <undefined>",
						escape_char(ch)
					))
					.raise();
				},
				"ignore" => {},
				"replace" => out.push(b'?'),
				other => return unknown_handler(other).raise(),
			},
		}
	}
	codec_result(alloc_bytes_object(&out), text.chars().count())
}

/// CPython returns an opaque `EncodingMap`; pon builds the equivalent plain
/// dict `{codepoint: byte}` (consumed only by `charmap_encode` above).
unsafe extern "C" fn charmap_build_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("_codecs.charmap_build received a NULL argv pointer");
	};
	if args.len() != 1 {
		return raise_type_error("charmap_build() takes exactly 1 argument");
	}
	let table = match str_arg(args[0], "argument") {
		Ok(table) => table,
		Err(message) => return raise_type_error(&message),
	};
	let mut items: Vec<*mut PyObject> = Vec::with_capacity(table.chars().count() * 2);
	for (index, ch) in table.chars().enumerate() {
		if ch == '\u{fffe}' {
			continue;
		}
		let key = alloc_int_object(i64::from(ch as u32));
		let value = alloc_int_object(index as i64);
		if key.is_null() || value.is_null() {
			return ptr::null_mut();
		}
		items.push(key);
		items.push(value);
	}
	// SAFETY: interleaved key/value slots, all live.
	unsafe { abi::map::pon_build_map(items.as_mut_ptr(), items.len() / 2) }
}

macro_rules! fixed_codec_entries {
	(
		$encode_entry:ident,
		$encode_core:expr,
		$decode_entry:ident,
		$decode_core:expr,
		$pyname:literal
	) => {
		unsafe extern "C" fn $encode_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
			let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
				return fail(concat!("_codecs.", $pyname, "_encode received a NULL argv pointer"));
			};
			if args.is_empty() || args.len() > 2 {
				return raise_type_error(concat!($pyname, "_encode() takes 1 or 2 arguments"));
			}
			let text = match str_arg(args[0], "argument") {
				Ok(text) => text,
				Err(message) => return raise_type_error(&message),
			};
			let errors = match errors_arg(args, 1) {
				Ok(errors) => errors,
				Err(message) => return raise_type_error(&message),
			};
			#[allow(clippy::redundant_closure_call)]
			match ($encode_core)(text, errors) {
				Ok(bytes) => codec_result(alloc_bytes_object(&bytes), text.chars().count()),
				Err(error) => error.raise(),
			}
		}

		unsafe extern "C" fn $decode_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
			let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
				return fail(concat!("_codecs.", $pyname, "_decode received a NULL argv pointer"));
			};
			if args.is_empty() || args.len() > 2 {
				return raise_type_error(concat!($pyname, "_decode() takes 1 or 2 arguments"));
			}
			let bytes = match bytes_arg(args[0], "argument") {
				Ok(bytes) => bytes,
				Err(message) => return raise_type_error(&message),
			};
			let errors = match errors_arg(args, 1) {
				Ok(errors) => errors,
				Err(message) => return raise_type_error(&message),
			};
			#[allow(clippy::redundant_closure_call)]
			match ($decode_core)(bytes, errors) {
				Ok(text) => codec_result(alloc_str_object(&text), bytes.len()),
				Err(error) => error.raise(),
			}
		}
	};
}

fixed_codec_entries!(
	ascii_encode_entry,
	ascii_encode_core,
	ascii_decode_entry,
	ascii_decode_core,
	"ascii"
);
fixed_codec_entries!(
	latin_1_encode_entry,
	latin1_encode_core,
	latin_1_decode_entry,
	|bytes: &[u8], _errors: &str| -> Result<String, CoreError> { Ok(latin1_decode_core(bytes)) },
	"latin_1"
);

// ---------------------------------------------------------------------------
// Error handler objects (`codecs.strict_errors` and friends)

unsafe extern "C" fn strict_errors_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("strict_errors received a NULL argv pointer");
	};
	let &[exc] = args else {
		return raise_type_error("strict_errors() takes exactly one argument");
	};
	// SAFETY: Raise entry point follows the NULL-sentinel error contract.
	unsafe { abi::exc::pon_raise(exc, ptr::null_mut()) }
}

/// Reads the `.end` attribute every Unicode error carries.
fn exception_end(exc: *mut PyObject, handler: &str) -> Result<i64, *mut PyObject> {
	let end = getattr(exc, "end");
	if end.is_null() {
		return Err(ptr::null_mut());
	}
	int_of(untag(end)).ok_or_else(|| {
		raise_type_error(&format!("{handler} error handler requires an int 'end' attribute"))
	})
}

unsafe extern "C" fn ignore_errors_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("ignore_errors received a NULL argv pointer");
	};
	let &[exc] = args else {
		return raise_type_error("ignore_errors() takes exactly one argument");
	};
	let end = match exception_end(untag(exc), "ignore") {
		Ok(end) => end,
		Err(raised) => return raised,
	};
	let replacement = alloc_str_object("");
	if replacement.is_null() {
		return ptr::null_mut();
	}
	let end = alloc_int_object(end);
	if end.is_null() {
		return ptr::null_mut();
	}
	alloc_tuple(vec![replacement, end])
}

unsafe extern "C" fn replace_errors_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("replace_errors received a NULL argv pointer");
	};
	let &[exc] = args else {
		return raise_type_error("replace_errors() takes exactly one argument");
	};
	let exc = untag(exc);
	let end = match exception_end(exc, "replace") {
		Ok(end) => end,
		Err(raised) => return raised,
	};
	let start = {
		let start = getattr(exc, "start");
		if start.is_null() {
			return ptr::null_mut();
		}
		match int_of(untag(start)) {
			Some(start) => start,
			None => {
				return raise_type_error("replace error handler requires an int 'start' attribute");
			},
		}
	};
	let span = usize::try_from(end.saturating_sub(start)).unwrap_or(0);
	// SAFETY: Heap pointer with a live header.
	let replacement = match unsafe { crate::types::dict::type_name(exc) } {
		Some("UnicodeDecodeError") => "\u{FFFD}".to_owned(),
		Some("UnicodeEncodeError") => "?".repeat(span),
		Some("UnicodeTranslateError") => "\u{FFFD}".repeat(span),
		_ => {
			return raise_type_error(
				"don't know how to handle this exception in the replace error callback",
			);
		},
	};
	let replacement = alloc_str_object(&replacement);
	if replacement.is_null() {
		return ptr::null_mut();
	}
	let end = alloc_int_object(end);
	if end.is_null() {
		return ptr::null_mut();
	}
	alloc_tuple(vec![replacement, end])
}

macro_rules! stub_error_handlers {
    ($(($name:literal, $entry:ident)),+ $(,)?) => {
        $(
            unsafe extern "C" fn $entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
                raise_kind(
                    ExceptionKind::NotImplementedError,
                    concat!("the '", $name, "' error handler is registered but not implemented in pon (see native/codecs.rs)"),
                )
            }
        )+
    };
}

stub_error_handlers!(
	("backslashreplace", backslashreplace_errors_entry),
	("xmlcharrefreplace", xmlcharrefreplace_errors_entry),
	("namereplace", namereplace_errors_entry),
	("surrogateescape", surrogateescape_errors_entry),
	("surrogatepass", surrogatepass_errors_entry),
);

// ---------------------------------------------------------------------------
// str.encode / bytes.decode integration (called from `abi::str_`)

/// Encodes `text` for `str.encode` / `bytes(str, enc)` / `bytearray(str,
/// enc)`: builtin encodings run the Rust cores (typed `UnicodeEncodeError` /
/// `LookupError` failures); other encodings go through the registry and must
/// produce a bytes-like result.  On `Err(())` the exception is already set.
pub(crate) fn encode_str_to_vec(text: &str, encoding: &str, errors: &str) -> Result<Vec<u8>, ()> {
	if let Some(codec) = builtin_codec(&collapse_normalize(encoding)) {
		let encoded = match codec {
			BuiltinCodec::Utf8 => Ok(utf8_encode_core(text)),
			BuiltinCodec::Ascii => ascii_encode_core(text, errors),
			BuiltinCodec::Latin1 => latin1_encode_core(text, errors),
			BuiltinCodec::UnicodeEscape => unicode_escape_encode_core(text, errors),
			BuiltinCodec::RawUnicodeEscape => raw_unicode_escape_encode_core(text, errors),
		};
		return encoded.map_err(|error| {
			error.raise();
		});
	}
	let info = lookup_object(encoding);
	if info.is_null() {
		return Err(());
	}
	let text_obj = alloc_str_object(text);
	if text_obj.is_null() {
		return Err(());
	}
	let payload = call_codec_info(info, "encode", text_obj, errors);
	if payload.is_null() {
		return Err(());
	}
	match bytes_arg(payload, "encoder result") {
		Ok(bytes) => Ok(bytes.to_vec()),
		Err(_) => {
			raise_type_error(&format!(
				"'{encoding}' encoder returned '{}' instead of 'bytes'",
				value_type_name(untag(payload))
			));
			Err(())
		},
	}
}

/// Decodes `data` for `bytes.decode` / `bytearray.decode`; mirror of
/// [`encode_str_to_vec`].
pub(crate) fn decode_bytes_to_string(
	data: &[u8],
	encoding: &str,
	errors: &str,
) -> Result<String, ()> {
	if let Some(codec) = builtin_codec(&collapse_normalize(encoding)) {
		let decoded = match codec {
			BuiltinCodec::Utf8 => utf8_decode_core(data, errors, true).map(|(text, _)| text),
			BuiltinCodec::Ascii => ascii_decode_core(data, errors),
			BuiltinCodec::Latin1 => Ok(latin1_decode_core(data)),
			BuiltinCodec::UnicodeEscape => {
				unicode_escape_decode_core(data, errors, true).map(|(text, _)| text)
			},
			BuiltinCodec::RawUnicodeEscape => {
				raw_unicode_escape_decode_core(data, errors, true).map(|(text, _)| text)
			},
		};
		return decoded.map_err(|error| {
			error.raise();
		});
	}
	let info = lookup_object(encoding);
	if info.is_null() {
		return Err(());
	}
	let data_obj = alloc_bytes_object(data);
	if data_obj.is_null() {
		return Err(());
	}
	let payload = call_codec_info(info, "decode", data_obj, errors);
	if payload.is_null() {
		return Err(());
	}
	match str_arg(payload, "decoder result") {
		Ok(text) => Ok(text.to_owned()),
		Err(_) => {
			raise_type_error(&format!(
				"'{encoding}' decoder returned '{}' instead of 'str'",
				value_type_name(untag(payload))
			));
			Err(())
		},
	}
}

/// `str(object, encoding[, errors])` (the decoding form of the `str`
/// builtin): requires a bytes-like `object`, mirroring CPython's TypeError.
/// Returns the decoded str object or NULL with the exception set.
pub(crate) fn builtin_str_decode(
	object: *mut PyObject,
	encoding: *mut PyObject,
	errors: Option<*mut PyObject>,
) -> *mut PyObject {
	let encoding = match str_arg(encoding, "str() encoding") {
		Ok(text) => text,
		Err(message) => return raise_type_error(&message),
	};
	let errors = match errors {
		None => "strict",
		Some(value) => match str_arg(value, "str() errors") {
			Ok(text) => text,
			Err(message) => return raise_type_error(&message),
		},
	};
	let data = match bytes_arg(object, "argument") {
		Ok(data) => data,
		Err(_) => {
			return raise_type_error(&format!(
				"decoding to str: need a bytes-like object, {} found",
				value_type_name(untag(object))
			));
		},
	};
	match decode_bytes_to_string(data, encoding, errors) {
		Ok(text) => alloc_str_object(&text),
		Err(()) => ptr::null_mut(),
	}
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn normalizations_match_the_layering_contract() {
		assert_eq!(c_normalize("UTF 8"), "utf_8");
		assert_eq!(c_normalize("UTF-8"), "utf-8");
		assert_eq!(collapse_normalize("UTF-8"), "utf_8");
		assert_eq!(collapse_normalize("ISO-8859-1"), "iso_8859_1");
		assert_eq!(collapse_normalize("latin  1"), "latin_1");
		assert!(builtin_codec("utf_8").is_some());
		assert!(builtin_codec("us_ascii").is_some());
		assert!(builtin_codec("iso8859_1").is_some());
		assert!(builtin_codec("unicode_escape").is_some());
		assert!(builtin_codec("raw_unicode_escape").is_some());
		assert!(builtin_codec("unicodeescape").is_some());
		assert!(builtin_codec("utf_16").is_none());
	}

	#[test]
	fn utf8_decode_classifies_errors_like_cpython() {
		let error = utf8_decode_core(b"\xff", "strict", true).unwrap_err();
		let CoreError::Decode(message) = error else {
			panic!("expected decode error")
		};
		assert_eq!(message, "'utf-8' codec can't decode byte 0xff in position 0: invalid start byte");

		let error = utf8_decode_core(b"a\xc3(", "strict", true).unwrap_err();
		let CoreError::Decode(message) = error else {
			panic!("expected decode error")
		};
		assert_eq!(
			message,
			"'utf-8' codec can't decode byte 0xc3 in position 1: invalid continuation byte"
		);

		let error = utf8_decode_core(b"a\xe2\x82", "strict", true).unwrap_err();
		let CoreError::Decode(message) = error else {
			panic!("expected decode error")
		};
		assert_eq!(
			message,
			"'utf-8' codec can't decode bytes in position 1-2: unexpected end of data"
		);

		// Non-final decodes leave an incomplete tail unconsumed.
		assert_eq!(utf8_decode_core(b"ab\xe2\x82", "strict", false).unwrap(), ("ab".to_owned(), 2));
		assert_eq!(
			utf8_decode_core(b"gr\xc3\xbc\xc3\x9fe", "strict", true).unwrap(),
			("grüße".to_owned(), 7)
		);
	}

	#[test]
	fn utf8_decode_handlers_replace_and_ignore() {
		assert_eq!(utf8_decode_core(b"a\xffb", "replace", true).unwrap().0, "a\u{FFFD}b");
		assert_eq!(utf8_decode_core(b"a\xffb", "ignore", true).unwrap().0, "ab");
		assert_eq!(
			utf8_decode_core(b"a\xffb", "backslashreplace", true)
				.unwrap()
				.0,
			"a\\xffb"
		);
		assert!(matches!(utf8_decode_core(b"\xff", "bogus", true), Err(CoreError::Handler(_))));
		// Unknown handlers are resolved lazily: clean input never fails.
		assert_eq!(utf8_decode_core(b"ok", "bogus", true).unwrap().0, "ok");
	}

	#[test]
	fn unicode_escape_codecs_match_cpython_samples() {
		let data = b"a\\n\\x41\\u00e9\\N{BULLET}\\q";
		assert_eq!(
			unicode_escape_decode_core(data, "strict", true).unwrap(),
			("a\nAé•\\q".to_owned(), data.len())
		);
		let raw_data = b"a\\n\\x41\\u00e9\\U0001f600";
		assert_eq!(
			raw_unicode_escape_decode_core(raw_data, "strict", true).unwrap(),
			("a\\n\\x41é😀".to_owned(), raw_data.len())
		);

		assert_eq!(
			unicode_escape_encode_core("abc\nAé•\\q😀", "strict").unwrap(),
			b"abc\\nA\\xe9\\u2022\\\\q\\U0001f600"
		);
		assert_eq!(
			raw_unicode_escape_encode_core("abc\nAé•\\q😀", "strict").unwrap(),
			b"abc\nA\xe9\\u2022\\q\\U0001f600"
		);

		let error = unicode_escape_decode_core(b"\\u00", "strict", true).unwrap_err();
		let CoreError::Decode(message) = error else {
			panic!("expected decode error")
		};
		assert_eq!(
			message,
			"'unicodeescape' codec can't decode bytes in position 0-3: truncated \\uXXXX escape"
		);
		assert_eq!(
			unicode_escape_decode_core(b"a\\u00", "strict", false).unwrap(),
			("a".to_owned(), 1)
		);

		let error = raw_unicode_escape_decode_core(b"\\U00110000", "strict", true).unwrap_err();
		let CoreError::Decode(message) = error else {
			panic!("expected decode error")
		};
		assert_eq!(
			message,
			"'rawunicodeescape' codec can't decode bytes in position 0-9: \\Uxxxxxxxx out of range"
		);
	}

	#[test]
	fn charrange_encode_matches_cpython_messages() {
		let error = ascii_encode_core("héllo", "strict").unwrap_err();
		let CoreError::Encode(message) = error else {
			panic!("expected encode error")
		};
		assert_eq!(
			message,
			"'ascii' codec can't encode character '\\xe9' in position 1: ordinal not in range(128)"
		);

		let error = ascii_encode_core("aħħb", "strict").unwrap_err();
		let CoreError::Encode(message) = error else {
			panic!("expected encode error")
		};
		assert_eq!(
			message,
			"'ascii' codec can't encode characters in position 1-2: ordinal not in range(128)"
		);

		assert_eq!(ascii_encode_core("héllo", "replace").unwrap(), b"h?llo");
		assert_eq!(ascii_encode_core("héllo", "ignore").unwrap(), b"hllo");
		assert_eq!(ascii_encode_core("héllo", "backslashreplace").unwrap(), b"h\\xe9llo");
		assert_eq!(ascii_encode_core("héllo", "xmlcharrefreplace").unwrap(), b"h&#233;llo");
		assert_eq!(latin1_encode_core("Grüße", "strict").unwrap(), b"Gr\xfc\xdfe");
		assert_eq!(latin1_decode_core(b"Gr\xfc\xdfe"), "Grüße");
	}
}
