//! Native `_tokenize` surface backed by Ruff's Python lexer.
//!
//! CPython's `_tokenize` is the C tokenizer (`Python/Python-tokenize.c`);
//! vendored `Lib/tokenize.py` imports `_tokenize.TokenizerIter` and expects it
//! to iterate raw `(type, string, start, end, line)` tuples, which
//! `TokenInfo._make` wraps.  This implementation consumes the supplied
//! readline callable, decodes UTF-8/Latin-1 byte streams when `encoding` is
//! supplied, tokenizes the complete source through Ruff's Python 3.14 lexer
//! output, and yields tuples shaped for `Lib/tokenize.py`.
//!
//! Product boundary: non-UTF-8/non-Latin-1 encodings are rejected with a typed
//! `NotImplementedError`; returning a guessed decoding would be silently wrong.

use std::{mem, ptr, sync::LazyLock};

use ruff_python_ast::PythonVersion;
use ruff_python_parser::{Mode, ParseOptions, TokenKind, parse_unchecked};

use super::install_module;
use crate::{
	abi::{self, pon_call},
	intern::intern,
	object::{PyObject, PyObjectHeader, PyType},
	thread_state::pon_err_clear,
	types::{bytes_::PyBytes, dict, exc::ExceptionKind, type_},
};

const ENDMARKER: i64 = 0;
const NAME: i64 = 1;
const NUMBER: i64 = 2;
const STRING: i64 = 3;
const NEWLINE: i64 = 4;
const INDENT: i64 = 5;
const DEDENT: i64 = 6;
const OP: i64 = 55;
const FSTRING_START: i64 = 59;
const FSTRING_MIDDLE: i64 = 60;
const FSTRING_END: i64 = 61;
const TSTRING_START: i64 = 62;
const TSTRING_MIDDLE: i64 = 63;
const TSTRING_END: i64 = 64;
const COMMENT: i64 = 65;
const NL: i64 = 66;
const ERRORTOKEN: i64 = 67;

#[derive(Clone)]
struct NativeToken {
	kind:  i64,
	text:  String,
	start: (i64, i64),
	end:   (i64, i64),
	line:  String,
}

#[repr(C)]
struct PyTokenizerIter {
	ob_base: PyObjectHeader,
	tokens:  Vec<NativeToken>,
	index:   usize,
}

struct TokenizerArgs {
	source:       *mut PyObject,
	encoding:     Option<String>,
	extra_tokens: bool,
}

struct LineIndex {
	starts: Vec<usize>,
	lines:  Vec<String>,
}

static TOKENIZER_ITER_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"TokenizerIter",
		mem::size_of::<PyTokenizerIter>(),
	);
	ty.tp_base = abi::runtime_global(intern("object"))
		.map_or(ptr::null_mut(), |object| object.cast::<PyType>());
	ty.tp_new = Some(tokenizer_iter_new);
	ty.tp_iter = Some(tokenizer_iter_identity);
	ty.tp_iternext = Some(tokenizer_iter_next);
	ty.tp_getattro = Some(crate::abstract_op::iterator_dunder_getattro);
	Box::into_raw(Box::new(ty)) as usize
});

unsafe extern "C" fn tokenizer_iter_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	let parsed = match unsafe { parse_tokenizer_args(args, kwargs) } {
		Ok(parsed) => parsed,
		Err(raised) => return raised,
	};
	let source = match unsafe { collect_source(parsed.source, parsed.encoding.as_deref()) } {
		Ok(source) => source,
		Err(raised) => return raised,
	};
	let tokens = tokenize_source(&source, parsed.extra_tokens);
	Box::into_raw(Box::new(PyTokenizerIter {
		ob_base: PyObjectHeader::new(*TOKENIZER_ITER_TYPE as *mut PyType),
		tokens,
		index: 0,
	}))
	.cast::<PyObject>()
}

unsafe extern "C" fn tokenizer_iter_identity(object: *mut PyObject) -> *mut PyObject {
	object
}

unsafe extern "C" fn tokenizer_iter_next(object: *mut PyObject) -> *mut PyObject {
	if object.is_null() {
		return raise_type_error("TokenizerIter.__next__ received NULL");
	}
	let iter = unsafe { &mut *crate::tag::untag_arg(object).cast::<PyTokenizerIter>() };
	let Some(token) = iter.tokens.get(iter.index).cloned() else {
		return unsafe { abi::exc::pon_raise_stop_iteration(ptr::null_mut()) };
	};
	iter.index += 1;
	token_tuple(&token)
}

unsafe fn parse_tokenizer_args(
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> Result<TokenizerArgs, *mut PyObject> {
	let positional = match unsafe { type_::positional_args_from_object(args) } {
		Ok(positional) => positional,
		Err(message) => return Err(raise_type_error(&message)),
	};
	if positional.is_empty() || positional.len() > 3 {
		return Err(raise_type_error(&format!(
			"TokenizerIter() expected 1 to 3 arguments, got {}",
			positional.len()
		)));
	}

	let source = positional[0];
	let mut encoding = positional.get(1).copied();
	let mut extra_tokens = positional.get(2).copied();

	if !kwargs.is_null() {
		let entries = match unsafe { dict::dict_entries_snapshot(kwargs) } {
			Ok(entries) => entries,
			Err(message) => return Err(raise_type_error(&message)),
		};
		for entry in entries {
			let key = crate::tag::untag_arg(entry.key);
			let Some(name) = (unsafe { type_::unicode_text(key) }) else {
				return Err(raise_type_error("TokenizerIter() keywords must be strings"));
			};
			match name {
				"encoding" => {
					if positional.len() >= 2 {
						return Err(raise_type_error(
							"TokenizerIter() got multiple values for argument 'encoding'",
						));
					}
					encoding = Some(entry.value);
				},
				"extra_tokens" => {
					if positional.len() >= 3 {
						return Err(raise_type_error(
							"TokenizerIter() got multiple values for argument 'extra_tokens'",
						));
					}
					extra_tokens = Some(entry.value);
				},
				other => {
					return Err(raise_type_error(&format!(
						"TokenizerIter() got an unexpected keyword argument '{other}'"
					)));
				},
			}
		}
	}

	let encoding = match encoding {
		Some(value) if !unsafe { is_none(value) } => {
			let value = crate::tag::untag_arg(value);
			let Some(text) = (unsafe { type_::unicode_text(value) }) else {
				return Err(raise_type_error("TokenizerIter() encoding must be str or None"));
			};
			Some(text.to_owned())
		},
		_ => None,
	};
	let extra_tokens = match extra_tokens {
		Some(value) => match unsafe { abi::pon_is_true(crate::tag::untag_arg(value)) } {
			0 => false,
			1 => true,
			_ => return Err(ptr::null_mut()),
		},
		None => false,
	};

	Ok(TokenizerArgs { source, encoding, extra_tokens })
}

unsafe fn collect_source(
	source: *mut PyObject,
	encoding: Option<&str>,
) -> Result<String, *mut PyObject> {
	let mut out = String::new();
	loop {
		let line_obj = unsafe { pon_call(source, ptr::null_mut(), 0) };
		if line_obj.is_null() {
			if abi::exc::pending_exception_is("StopIteration") {
				pon_err_clear();
				break;
			}
			return Err(ptr::null_mut());
		}
		let line = unsafe { line_object_to_text(line_obj, encoding) }?;
		if line.is_empty() {
			break;
		}
		out.push_str(&line);
	}
	Ok(out)
}

unsafe fn line_object_to_text(
	object: *mut PyObject,
	encoding: Option<&str>,
) -> Result<String, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	if let Some(text) = unsafe { type_::unicode_text(object) } {
		return Ok(text.to_owned());
	}
	if unsafe { dict::type_name(object) } == Some("bytes") {
		let bytes = unsafe { (&*object.cast::<PyBytes>()).as_slice() };
		let Some(encoding) = encoding else {
			return Err(raise_type_error("TokenizerIter() source returned bytes without an encoding"));
		};
		return decode_bytes_line(bytes, encoding);
	}
	Err(raise_type_error("TokenizerIter() source must return str, bytes, or stop iteration"))
}

fn decode_bytes_line(bytes: &[u8], encoding: &str) -> Result<String, *mut PyObject> {
	let normalized = encoding.to_ascii_lowercase().replace('_', "-");
	if normalized == "utf-8" || normalized.starts_with("utf-8-") {
		return std::str::from_utf8(bytes)
			.map(str::to_owned)
			.map_err(|error| {
				abi::exc::raise_kind_error_text(
					ExceptionKind::UnicodeDecodeError,
					&format!("'utf-8' codec can't decode source line: {error}"),
				)
			});
	}
	if matches!(normalized.as_str(), "latin-1" | "iso-8859-1" | "iso-latin-1")
		|| normalized.starts_with("latin-1-")
		|| normalized.starts_with("iso-8859-1-")
		|| normalized.starts_with("iso-latin-1-")
	{
		return Ok(bytes.iter().map(|&byte| char::from(byte)).collect());
	}
	Err(abi::exc::raise_kind_error_text(
		ExceptionKind::NotImplementedError,
		&format!("_tokenize.TokenizerIter does not support source encoding '{encoding}'"),
	))
}

fn tokenize_source(source: &str, extra_tokens: bool) -> Vec<NativeToken> {
	let options = ParseOptions::from(Mode::Module).with_target_version(PythonVersion::PY314);
	let parsed = parse_unchecked(source, options);
	let lines = LineIndex::new(source);
	let mut tokens = Vec::new();

	for token in parsed.tokens() {
		let (kind, range) = token.as_tuple();
		let Some(token_kind) = python_token_kind(kind, extra_tokens) else {
			continue;
		};
		let start = range.start().to_usize().min(source.len());
		let end = range.end().to_usize().min(source.len());
		let text = token_text(source, kind, start, end);
		let mut start_pos = lines.position(start);
		let mut end_pos =
			if matches!(kind, TokenKind::Newline | TokenKind::NonLogicalNewline) && end > start {
				(start_pos.0, start_pos.1 + text.chars().count() as i64)
			} else {
				lines.position(end)
			};
		if matches!(kind, TokenKind::Newline) && start == end && implicit_final_newline(source) {
			end_pos.1 += 1;
		}
		if matches!(kind, TokenKind::Dedent) {
			start_pos.1 = -1;
			end_pos.1 = -1;
		}
		tokens.push(NativeToken {
			kind: token_kind,
			text,
			start: start_pos,
			end: end_pos,
			line: lines.line_at(start).to_owned(),
		});
	}

	let eof = eof_position(source, &lines);
	tokens.push(NativeToken {
		kind:  ENDMARKER,
		text:  String::new(),
		start: eof,
		end:   eof,
		line:  String::new(),
	});

	tokens
}

fn python_token_kind(kind: TokenKind, extra_tokens: bool) -> Option<i64> {
	Some(match kind {
		TokenKind::EndOfFile => ENDMARKER,
		TokenKind::Name => NAME,
		kind if kind.is_keyword() => NAME,
		TokenKind::Int | TokenKind::Float | TokenKind::Complex => NUMBER,
		TokenKind::String => STRING,
		TokenKind::FStringStart => FSTRING_START,
		TokenKind::FStringMiddle => FSTRING_MIDDLE,
		TokenKind::FStringEnd => FSTRING_END,
		TokenKind::TStringStart => TSTRING_START,
		TokenKind::TStringMiddle => TSTRING_MIDDLE,
		TokenKind::TStringEnd => TSTRING_END,
		TokenKind::Newline => NEWLINE,
		TokenKind::NonLogicalNewline if extra_tokens => NL,
		TokenKind::NonLogicalNewline => return None,
		TokenKind::Indent => INDENT,
		TokenKind::Dedent => DEDENT,
		TokenKind::Comment if extra_tokens => COMMENT,
		TokenKind::Comment => return None,
		kind if kind.is_operator() => OP,
		_ => ERRORTOKEN,
	})
}

fn token_text(source: &str, kind: TokenKind, start: usize, end: usize) -> String {
	if matches!(kind, TokenKind::Dedent | TokenKind::EndOfFile) || start >= end {
		String::new()
	} else {
		source[start..end].to_owned()
	}
}

fn implicit_final_newline(source: &str) -> bool {
	!source.is_empty() && !source.ends_with('\n') && !source.ends_with('\r')
}

fn eof_position(source: &str, lines: &LineIndex) -> (i64, i64) {
	if implicit_final_newline(source) {
		let (row, _) = lines.position(source.len());
		(row + 1, 0)
	} else {
		lines.position(source.len())
	}
}

impl LineIndex {
	fn new(source: &str) -> Self {
		let mut starts = vec![0];
		let mut lines = Vec::new();
		let mut line_start = 0usize;
		for (index, ch) in source.char_indices() {
			if ch == '\n' {
				let next = index + ch.len_utf8();
				lines.push(source[line_start..next].to_owned());
				line_start = next;
				starts.push(line_start);
			}
		}
		if line_start < source.len() {
			lines.push(source[line_start..].to_owned());
		} else {
			lines.push(String::new());
		}
		Self { starts, lines }
	}

	fn line_index(&self, offset: usize) -> usize {
		self
			.starts
			.partition_point(|&start| start <= offset)
			.saturating_sub(1)
			.min(self.lines.len().saturating_sub(1))
	}

	fn position(&self, offset: usize) -> (i64, i64) {
		let index = self.line_index(offset);
		let line_start = self.starts[index];
		let byte_col = offset
			.saturating_sub(line_start)
			.min(self.lines[index].len());
		let col = self.lines[index][..byte_col].chars().count();
		((index + 1) as i64, col as i64)
	}

	fn line_at(&self, offset: usize) -> &str {
		&self.lines[self.line_index(offset)]
	}
}

fn token_tuple(token: &NativeToken) -> *mut PyObject {
	let kind = crate::types::int::from_i64(token.kind);
	let text = string_object(&token.text);
	let start = position_tuple(token.start);
	let end = position_tuple(token.end);
	let line = string_object(&token.line);
	if kind.is_null() || text.is_null() || start.is_null() || end.is_null() || line.is_null() {
		return ptr::null_mut();
	}
	let mut items = [kind, text, start, end, line];
	unsafe { abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

fn position_tuple(position: (i64, i64)) -> *mut PyObject {
	let row = crate::types::int::from_i64(position.0);
	let col = crate::types::int::from_i64(position.1);
	if row.is_null() || col.is_null() {
		return ptr::null_mut();
	}
	let mut items = [row, col];
	unsafe { abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

fn string_object(text: &str) -> *mut PyObject {
	unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

unsafe fn is_none(object: *mut PyObject) -> bool {
	crate::tag::untag_arg(object) == unsafe { abi::pon_none() }
}

fn raise_type_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let name = "_tokenize";
	// SAFETY: Runtime allocation helper; NULL is checked below.
	let name_obj = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
	if name_obj.is_null() {
		return Err("failed to allocate _tokenize.__name__".to_owned());
	}
	install_module(name, vec![
		(intern("__name__"), name_obj),
		(intern("TokenizerIter"), (*TOKENIZER_ITER_TYPE as *mut PyType).cast::<PyObject>()),
	])
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn basic_tokens_have_tokenize_tuple_shape() {
		let tokens = tokenize_source("1 + 1\n", true);

		assert_eq!(tokens.len(), 5);
		assert_token(&tokens[0], NUMBER, "1", (1, 0), (1, 1), "1 + 1\n");
		assert_token(&tokens[1], OP, "+", (1, 2), (1, 3), "1 + 1\n");
		assert_token(&tokens[2], NUMBER, "1", (1, 4), (1, 5), "1 + 1\n");
		assert_token(&tokens[3], NEWLINE, "\n", (1, 5), (1, 6), "1 + 1\n");
		assert_token(&tokens[4], ENDMARKER, "", (2, 0), (2, 0), "");
	}

	#[test]
	fn implicit_newline_advances_past_missing_line_break() {
		let tokens = tokenize_source("1+1", true);

		assert_eq!(tokens.len(), 5);
		assert_token(&tokens[3], NEWLINE, "", (1, 3), (1, 4), "1+1");
		assert_token(&tokens[4], ENDMARKER, "", (2, 0), (2, 0), "");
	}

	fn assert_token(
		token: &NativeToken,
		kind: i64,
		text: &str,
		start: (i64, i64),
		end: (i64, i64),
		line: &str,
	) {
		assert_eq!(token.kind, kind);
		assert_eq!(token.text, text);
		assert_eq!(token.start, start);
		assert_eq!(token.end, end);
		assert_eq!(token.line, line);
	}
}
