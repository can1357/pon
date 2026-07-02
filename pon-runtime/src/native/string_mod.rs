//! Native `_string` module: the two formatter helpers behind `string.Formatter`.
//!
//! CPython implements these in `Modules/_string.c` over the shared
//! `Objects/stringlib/unicode_format.h` machinery; the semantics here are a
//! transcription of `MarkupIterator_next` / `parse_field` /
//! `FieldNameIterator_next` / `get_integer`, differential-tested against
//! CPython 3.14:
//!
//! - `formatter_parser(s)` — iterator of `(literal_text, field_name,
//!   format_spec, conversion)` 4-tuples.  `field_name`/`format_spec`/
//!   `conversion` are `None` for a trailing pure literal; a present field
//!   always carries a `str` spec (possibly empty); the conversion is a
//!   one-codepoint string or `None`.  Doubled braces terminate the literal
//!   chunk *including* the first brace and restart cleanly after the second;
//!   errors (`Single '{'`, `unmatched '{' in format spec`, ...) are raised
//!   lazily at the offending chunk, after earlier tuples were yielded.
//! - `formatter_field_name_split(name)` — `(first, rest)` where `first` is an
//!   `int` when the head is all decimal digits (else `str`, possibly empty)
//!   and `rest` iterates `(is_attribute, name)` pairs; `[index]` names become
//!   `int` under the same all-digits rule.  Digit runs above `isize::MAX`
//!   raise `Too many decimal digits in format string` exactly like
//!   `get_integer`.
//!
//! Scanning is byte-wise over the owned UTF-8 copy: every structural
//! character (`{`, `}`, `:`, `!`, `.`, `[`, `]`) is ASCII, so multi-byte
//! codepoints can never split a boundary; the conversion character is read as
//! a full codepoint (`café{x!é}` keeps `é`).  Both iterators are identity
//! iterators (`iter(it) is it`) matching the CPython iterator objects.

use std::ptr;
use std::sync::LazyLock;

use crate::abi;
use crate::intern::intern;
use crate::object::{PyObject, PyObjectHeader, PyType, PyUnicode, UnaryFunc};

use super::builtins_mod::VARIADIC_ARITY;
use super::install_module;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

// ---------------------------------------------------------------------------
// Small helpers (itertools-shaped, local so the module stays self-contained)

fn untag(object: *mut PyObject) -> *mut PyObject {
    crate::tag::untag_arg(object)
}

fn none() -> *mut PyObject {
    // SAFETY: Singleton accessor.
    unsafe { abi::pon_none() }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    // SAFETY: Message bytes are a live UTF-8 slice for the duration of the call.
    unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn raise_value_error(message: &str) -> *mut PyObject {
    // SAFETY: Message bytes are a live UTF-8 slice for the duration of the call.
    unsafe { abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) }
}

fn raise_stop_iteration() -> *mut PyObject {
    // SAFETY: NULL value produces a plain StopIteration.
    unsafe { abi::pon_raise_stop_iteration(ptr::null_mut()) }
}

fn str_object(text: &str) -> *mut PyObject {
    // SAFETY: `text` is a live UTF-8 slice; the runtime copies the bytes.
    unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn int_object(value: i64) -> *mut PyObject {
    untag(crate::types::int::from_i64(value))
}

fn bool_object(value: bool) -> *mut PyObject {
    // SAFETY: Bool constructor returns the shared singleton.
    unsafe { abi::number::pon_const_bool(i32::from(value)) }
}

fn tuple_from(mut items: Vec<*mut PyObject>) -> *mut PyObject {
    if items.iter().any(|item| item.is_null()) {
        return raise_type_error("_string failed to allocate a result tuple item");
    }
    // SAFETY: `items` is a live window for the duration of the call.
    unsafe { abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

unsafe extern "C" fn identity_iter(object: *mut PyObject) -> *mut PyObject {
    object
}

fn iterator_type(name: &'static str, size: usize, next: UnaryFunc) -> usize {
    let mut ty = PyType::new(abi::runtime_type_type().cast_const(), name, size);
    ty.tp_iter = Some(identity_iter);
    ty.tp_iternext = Some(next);
    Box::into_raw(Box::new(ty)) as usize
}

fn alloc_object<T>(value: T) -> *mut PyObject {
    Box::into_raw(Box::new(value)).cast::<PyObject>()
}

/// Extracts the text of a `str` (or `str` subclass, whose layout embeds
/// `PyUnicode`) argument; `None` for non-strings.
unsafe fn text_argument(object: *mut PyObject) -> Option<String> {
    if object.is_null() {
        return None;
    }
    let mut ty = unsafe { (*object).ob_type };
    while !ty.is_null() {
        if unsafe { (*ty).name() } == "str" {
            // SAFETY: A str (sub)type instance carries the PyUnicode layout.
            return unsafe { (*object.cast::<PyUnicode>()).as_str() }.map(ToOwned::to_owned);
        }
        ty = unsafe { (*ty).tp_base };
    }
    None
}

fn type_name(object: *mut PyObject) -> &'static str {
    if object.is_null() {
        return "NULL";
    }
    let ty = unsafe { (*object).ob_type };
    if ty.is_null() { "<unknown>" } else { unsafe { (*ty).name() } }
}

/// Untags and validates the single `str` argument of the two entry points.
unsafe fn single_text_argument(
    argv: *mut *mut PyObject,
    argc: usize,
    function_name: &str,
) -> Result<String, *mut PyObject> {
    let args = if argc == 0 || argv.is_null() {
        &[][..]
    } else {
        // SAFETY: The caller passed `argc` live argument slots.
        unsafe { std::slice::from_raw_parts(argv, argc) }
    };
    if args.len() != 1 {
        return Err(raise_type_error(&format!(
            "{function_name}() takes exactly one argument ({} given)",
            args.len()
        )));
    }
    let value = untag(args[0]);
    match unsafe { text_argument(value) } {
        Some(text) => Ok(text),
        None => Err(raise_type_error(&format!("expected str, got {}", type_name(value)))),
    }
}

// ---------------------------------------------------------------------------
// get_integer: all-decimal-digits -> index, with the CPython overflow error

/// `Ok(Some(i))` for a non-empty all-ASCII-digit run, `Ok(None)` for anything
/// else (used as a `str` key), `Err(())` on `isize` overflow (caller raises
/// "Too many decimal digits in format string").
fn get_integer(text: &str) -> Result<Option<i64>, ()> {
    if text.is_empty() {
        return Ok(None);
    }
    let mut accumulator: i64 = 0;
    for byte in text.bytes() {
        if !byte.is_ascii_digit() {
            return Ok(None);
        }
        let digit = i64::from(byte - b'0');
        if accumulator > (i64::MAX - digit) / 10 {
            return Err(());
        }
        accumulator = accumulator * 10 + digit;
    }
    Ok(Some(accumulator))
}

const TOO_MANY_DIGITS: &str = "Too many decimal digits in format string";

// ---------------------------------------------------------------------------
// formatter_parser: MarkupIterator over (literal, field, spec, conversion)

#[repr(C)]
struct PyFormatterIter {
    ob_base: PyObjectHeader,
    text: String,
    pos: usize,
}

static FORMATTER_ITER_TYPE: LazyLock<usize> =
    LazyLock::new(|| iterator_type("formatteriterator", size_of::<PyFormatterIter>(), formatter_iter_next));

unsafe extern "C" fn formatter_parser_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let text = match unsafe { single_text_argument(argv, argc, "formatter_parser") } {
        Ok(text) => text,
        Err(raised) => return raised,
    };
    alloc_object(PyFormatterIter {
        ob_base: PyObjectHeader::new(*FORMATTER_ITER_TYPE as *const PyType),
        text,
        pos: 0,
    })
}

/// One `MarkupIterator_next` step: yields the next 4-tuple or raises.
unsafe extern "C" fn formatter_iter_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyFormatterIter>() };
    let bytes = state.text.as_bytes();
    let len = bytes.len();
    if state.pos >= len {
        return raise_stop_iteration();
    }

    // Literal scan: up to the end or the first '{' / '}'.
    let literal_start = state.pos;
    let mut brace: u8 = 0;
    while state.pos < len {
        let ch = bytes[state.pos];
        state.pos += 1;
        if ch == b'{' || ch == b'}' {
            brace = ch;
            break;
        }
    }
    let at_end = state.pos >= len;
    let mut literal_len = state.pos - literal_start;
    let mut markup_follows = brace != 0;

    if brace == b'}' && (at_end || bytes[state.pos] != b'}') {
        return raise_value_error("Single '}' encountered in format string");
    }
    if at_end && brace == b'{' {
        return raise_value_error("Single '{' encountered in format string");
    }
    if !at_end && markup_follows {
        if bytes[state.pos] == brace {
            // Doubled brace: keep the first in the literal, skip the second.
            state.pos += 1;
            markup_follows = false;
        } else {
            // A real field opener: the '{' is not part of the literal.
            literal_len -= 1;
        }
    }
    let literal_end = literal_start + literal_len;

    if !markup_follows {
        let literal = str_object(&state.text[literal_start..literal_end]);
        return tuple_from(vec![literal, none(), none(), none()]);
    }

    // parse_field: field name until top-level '}' / ':' / '!' ('[..]' shields).
    let field_start = state.pos;
    let mut terminator: u8 = 0;
    while state.pos < len {
        let ch = bytes[state.pos];
        state.pos += 1;
        match ch {
            b'{' => return raise_value_error("unexpected '{' in field name"),
            b'[' => {
                while state.pos < len && bytes[state.pos] != b']' {
                    state.pos += 1;
                }
            }
            b'}' | b':' | b'!' => {
                terminator = ch;
                break;
            }
            _ => {}
        }
    }
    let field_end = if terminator == 0 { state.pos } else { state.pos - 1 };

    let mut conversion: Option<char> = None;
    let mut spec_range: Option<(usize, usize)> = None;
    if terminator == b'!' || terminator == b':' {
        let mut done_after_conversion = false;
        if terminator == b'!' {
            if state.pos >= len {
                return raise_value_error("end of string while looking for conversion specifier");
            }
            // The conversion specifier is one full codepoint.
            let ch = state.text[state.pos..].chars().next().unwrap_or('\u{fffd}');
            conversion = Some(ch);
            state.pos += ch.len_utf8();
            if state.pos < len {
                let after = bytes[state.pos];
                state.pos += 1;
                if after == b'}' {
                    done_after_conversion = true;
                } else if after != b':' {
                    return raise_value_error("expected ':' after conversion specifier");
                }
            }
        }
        if !done_after_conversion {
            // Format spec runs to the matching '}' (nested braces count).
            let spec_start = state.pos;
            let mut depth: usize = 1;
            let mut closed = false;
            while state.pos < len {
                let ch = bytes[state.pos];
                state.pos += 1;
                match ch {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            spec_range = Some((spec_start, state.pos - 1));
                            closed = true;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if !closed {
                return raise_value_error("unmatched '{' in format spec");
            }
        }
    } else if terminator != b'}' {
        return raise_value_error("expected '}' before end of string");
    }

    let literal = str_object(&state.text[literal_start..literal_end]);
    let field_name = str_object(&state.text[field_start..field_end]);
    let format_spec = match spec_range {
        Some((start, end)) => str_object(&state.text[start..end]),
        // Field present without an explicit spec: empty string, not None.
        None => str_object(""),
    };
    let conversion_object = match conversion {
        Some(ch) => {
            let mut buffer = [0u8; 4];
            str_object(ch.encode_utf8(&mut buffer))
        }
        None => none(),
    };
    tuple_from(vec![literal, field_name, format_spec, conversion_object])
}

// ---------------------------------------------------------------------------
// formatter_field_name_split: (first, iterator over (is_attribute, name))

#[repr(C)]
struct PyFieldNameIter {
    ob_base: PyObjectHeader,
    text: String,
    pos: usize,
}

static FIELD_NAME_ITER_TYPE: LazyLock<usize> =
    LazyLock::new(|| iterator_type("fieldnameiterator", size_of::<PyFieldNameIter>(), field_name_iter_next));

unsafe extern "C" fn formatter_field_name_split_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let text = match unsafe { single_text_argument(argv, argc, "formatter_field_name_split") } {
        Ok(text) => text,
        Err(raised) => return raised,
    };
    let bytes = text.as_bytes();
    let mut head_end = 0usize;
    while head_end < bytes.len() && bytes[head_end] != b'.' && bytes[head_end] != b'[' {
        head_end += 1;
    }
    let first = match get_integer(&text[..head_end]) {
        Ok(Some(index)) => int_object(index),
        Ok(None) => str_object(&text[..head_end]),
        Err(()) => return raise_value_error(TOO_MANY_DIGITS),
    };
    let rest = alloc_object(PyFieldNameIter {
        ob_base: PyObjectHeader::new(*FIELD_NAME_ITER_TYPE as *const PyType),
        text,
        pos: head_end,
    });
    tuple_from(vec![first, rest])
}

/// One `FieldNameIterator_next` step: `(is_attribute, name)` or raises.
unsafe extern "C" fn field_name_iter_next(object: *mut PyObject) -> *mut PyObject {
    let state = unsafe { &mut *object.cast::<PyFieldNameIter>() };
    let bytes = state.text.as_bytes();
    let len = bytes.len();
    if state.pos >= len {
        return raise_stop_iteration();
    }
    let lead = bytes[state.pos];
    state.pos += 1;
    let (is_attribute, name_start, name_end) = match lead {
        b'.' => {
            let start = state.pos;
            while state.pos < len && bytes[state.pos] != b'.' && bytes[state.pos] != b'[' {
                state.pos += 1;
            }
            (true, start, state.pos)
        }
        b'[' => {
            let start = state.pos;
            let mut bracket_seen = false;
            while state.pos < len {
                let ch = bytes[state.pos];
                state.pos += 1;
                if ch == b']' {
                    bracket_seen = true;
                    break;
                }
            }
            if !bracket_seen {
                return raise_value_error("Missing ']' in format string");
            }
            (false, start, state.pos - 1)
        }
        _ => {
            return raise_value_error("Only '.' or '[' may follow ']' in format field specifier");
        }
    };
    let name = &state.text[name_start..name_end];
    let name_object = if is_attribute {
        if name.is_empty() {
            return raise_value_error("Empty attribute in format string");
        }
        str_object(name)
    } else {
        match get_integer(name) {
            Ok(Some(index)) => int_object(index),
            Ok(None) => {
                if name.is_empty() {
                    return raise_value_error("Empty attribute in format string");
                }
                str_object(name)
            }
            Err(()) => return raise_value_error(TOO_MANY_DIGITS),
        }
    };
    let flag = bool_object(is_attribute);
    tuple_from(vec![flag, name_object])
}

// ---------------------------------------------------------------------------
// Module factory

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "_string";
    let name_object = str_object(name);
    if name_object.is_null() {
        return Err("failed to allocate _string.__name__".to_owned());
    }
    let doc = "string helper module";
    let doc_object = str_object(doc);
    if doc_object.is_null() {
        return Err("failed to allocate _string.__doc__".to_owned());
    }
    let mut attrs = vec![(intern("__name__"), name_object), (intern("__doc__"), doc_object)];
    for (function_name, entry) in [
        ("formatter_field_name_split", formatter_field_name_split_entry as BuiltinFn),
        ("formatter_parser", formatter_parser_entry),
    ] {
        // SAFETY: `entry` is a live builtin entry point.
        let function =
            unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(function_name)) };
        if function.is_null() {
            return Err(format!("failed to allocate _string.{function_name}"));
        }
        attrs.push((intern(function_name), function));
    }
    install_module(name, attrs)
}
