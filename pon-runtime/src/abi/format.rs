//! Python format-spec mini-language and template-string runtime support.
//!
//! This module intentionally exposes a Rust-internal API.  The baseline codegen
//! still reaches f-strings and t-strings through the existing string helpers;
//! builtin `format`, `str.format`, `str.format_map`, and numeric/string
//! `__format__` implementations should call [`format_object_with_spec`] or
//! [`format_template`] directly from Rust.

use core::ptr;
use std::sync::LazyLock;

use num_bigint::{BigInt, Sign};
use num_traits::{FromPrimitive, Signed, ToPrimitive};

use crate::intern::{intern, resolve};
use crate::object::{PyNumberMethods, PyObject, PyObjectHeader, PyType, PyUnicode, as_object_ptr, is_exact_type};
use crate::thread_state::{pon_err_clear, pon_err_message};
use crate::types::{bool_ as bool_type, bytearray_ as bytearray_type, bytes_ as bytes_type, float as float_type, int as int_type, str_ as str_type, tuple::PyTuple, type_};

use super::{FStrPartRaw, TStrPartRaw};

const TEMPLATE_LITERAL_CONVERSION: u8 = u8::MAX;

#[repr(C)]
struct PyTemplate {
    ob_base: PyObjectHeader,
    strings: *mut PyObject,
    interpolations: *mut PyObject,
    values: *mut PyObject,
}

#[repr(C)]
struct PyInterpolation {
    ob_base: PyObjectHeader,
    value: *mut PyObject,
    expression: *mut PyObject,
    conversion: *mut PyObject,
    format_spec: *mut PyObject,
}

static TEMPLATE_NUMBER_METHODS: LazyLock<usize> = LazyLock::new(|| {
    let methods = PyNumberMethods {
        nb_add: Some(template_add),
        ..PyNumberMethods::EMPTY
    };
    Box::into_raw(Box::new(methods)) as usize
});

static TEMPLATE_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(runtime_type_type(), "Template", core::mem::size_of::<PyTemplate>());
    ty.tp_getattro = Some(template_getattro);
    ty.tp_as_number = *TEMPLATE_NUMBER_METHODS as *mut PyNumberMethods;
    Box::into_raw(Box::new(ty)) as usize
});

static INTERPOLATION_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(runtime_type_type(), "Interpolation", core::mem::size_of::<PyInterpolation>());
    ty.tp_getattro = Some(interpolation_getattro);
    Box::into_raw(Box::new(ty)) as usize
});

fn runtime_type_type() -> *mut PyType {
    super::with_runtime(|runtime| runtime._type_type).unwrap_or(ptr::null_mut())
}

fn template_type() -> *mut PyType {
    *TEMPLATE_TYPE as *mut PyType
}

fn interpolation_type() -> *mut PyType {
    *INTERPOLATION_TYPE as *mut PyType
}

unsafe extern "C" fn template_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        return super::return_null_with_error("template attribute name must be str");
    };
    let template = unsafe { &*object.cast::<PyTemplate>() };
    match name {
        "strings" => template.strings,
        "interpolations" => template.interpolations,
        "values" => template.values,
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

unsafe extern "C" fn template_add(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
    match unsafe { concat_templates(left, right) } {
        Ok(object) => object,
        Err(message) => super::return_null_with_error(message),
    }
}

/// Formats a Python object using CPython's format-spec mini-language subset for
/// `str`, `int`/`bool`, and `float`, with user-defined `__format__` dispatch for
/// other objects.
pub(crate) fn format_object_with_spec(value: *mut PyObject, spec: &str) -> Result<String, String> {
    if value.is_null() {
        return Err("cannot format NULL object".to_owned());
    }
    if let Err(message) = super::ensure_runtime_initialized() {
        return Err(message);
    }

    if let Some(text) = exact_str(value)? {
        return format_str(&text, spec);
    }
    if let Some(bool_value) = unsafe { bool_type::to_bool(value) } {
        if spec.is_empty() {
            return Ok(if bool_value { "True".to_owned() } else { "False".to_owned() });
        }
        return format_int(&BigInt::from(i32::from(bool_value)), spec);
    }
    if let Some(integer) = unsafe { int_type::to_bigint(value) } {
        return format_int(&integer, spec);
    }
    if let Some(float) = unsafe { float_type::to_f64(value) } {
        return format_float(float, spec);
    }

    if let Some(text) = unsafe { call_custom_format(value, spec)? } {
        return Ok(text);
    }

    if spec.is_empty() {
        object_to_str(value)
    } else {
        Err(format!("unsupported format string passed to {}.__format__", type_name(value)))
    }
}

/// Formats one f-string or `str.format` replacement value after an optional
/// `!s`, `!r`, or `!a` conversion.
pub(crate) fn format_value_to_text(value: *mut PyObject, conversion: u8, format_spec: *mut PyObject) -> Result<String, String> {
    let spec = if format_spec.is_null() {
        None
    } else {
        Some(expect_str(format_spec)?)
    };
    format_value_with_spec_text(value, conversion, spec.as_deref().unwrap_or(""), spec.is_some())
}

fn format_value_with_spec_text(value: *mut PyObject, conversion: u8, spec: &str, _has_spec: bool) -> Result<String, String> {
    match conversion {
        0 => format_object_with_spec(value, spec),
        b's' => format_str(&object_to_str(value)?, spec),
        b'r' => format_str(&object_to_repr(value)?, spec),
        b'a' => format_str(&str_type::escape_non_ascii(&object_to_repr(value)?), spec),
        _ => Err("unsupported f-string conversion".to_owned()),
    }
}

/// Renders a `str.format`/`str.format_map` template.  `mapping` is used for
/// named fields; positional fields are resolved from `args`.
pub(crate) unsafe fn format_template(
    template: &str,
    args: &[*mut PyObject],
    mapping: Option<*mut PyObject>,
) -> Result<String, String> {
    let mut state = TemplateFormatState {
        args,
        mapping,
        auto_index: 0,
        numbering: NumberingMode::None,
    };
    unsafe { render_template(template, &mut state) }
}

/// Builds a Python 3.14 template-string object from raw codegen parts.
pub(crate) unsafe fn build_template_from_raw(parts: *const TStrPartRaw, len: usize) -> Result<*mut PyObject, String> {
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

    template_from_parts(strings, interpolations)
}

fn format_str(value: &str, spec: &str) -> Result<String, String> {
    let parsed = ParsedFormatSpec::parse(spec)?;
    if !matches!(parsed.ty, None | Some('s')) {
        return Err(format!("unknown format code '{}' for object of type 'str'", parsed.ty.unwrap_or('?')));
    }
    if parsed.sign.is_some() || parsed.alternate || parsed.grouping.is_some() || parsed.z {
        return Err("format specifier not allowed for strings".to_owned());
    }
    if matches!(parsed.align, Some(FormatAlign::SignAware)) {
        return Err("'=' alignment not allowed in string format specifier".to_owned());
    }
    let text = if let Some(precision) = parsed.precision {
        truncate_to_precision(value, precision)
    } else {
        value.to_owned()
    };
    apply_text_width(&text, &parsed)
}

fn format_int(value: &BigInt, spec: &str) -> Result<String, String> {
    let parsed = ParsedFormatSpec::parse(spec)?;
    let ty = parsed.ty.unwrap_or('d');
    if parsed.z {
        return Err("negative zero coercion is only allowed in float format specs".to_owned());
    }
    if parsed.precision.is_some() {
        return Err("precision not allowed in integer format specifier".to_owned());
    }
    if ty == 'c' {
        return format_char(value, &parsed);
    }
    let radix = match ty {
        'd' | 'n' => 10,
        'b' => 2,
        'o' => 8,
        'x' | 'X' => 16,
        _ => return Err(format!("unknown format code '{ty}' for object of type 'int'")),
    };
    if ty == 'n' && parsed.grouping.is_some() {
        return Err(format!("Cannot specify '{}' with 'n'.", parsed.grouping.unwrap()));
    }
    if parsed.grouping == Some(',') && radix != 10 {
        return Err(format!("Cannot specify ',' with '{ty}'."));
    }
    let negative = value.sign() == Sign::Minus;
    let mut digits = value.abs().to_str_radix(radix);
    if ty == 'X' {
        digits.make_ascii_uppercase();
    }
    if let Some(grouping) = parsed.grouping {
        let group = if radix == 10 { 3 } else { 4 };
        digits = group_digits(&digits, grouping, group);
    }
    let prefix = if parsed.alternate {
        match ty {
            'b' => "0b",
            'o' => "0o",
            'x' => "0x",
            'X' => "0X",
            _ => "",
        }
    } else {
        ""
    };
    let sign = sign_text(negative, parsed.sign);
    apply_number_width(sign, prefix, &digits, "", &parsed)
}

fn format_char(value: &BigInt, spec: &ParsedFormatSpec) -> Result<String, String> {
    if spec.sign.is_some() || spec.alternate || spec.grouping.is_some() || spec.zero {
        return Err("sign, alternate form, grouping, and zero padding are not allowed with integer format code 'c'".to_owned());
    }
    let Some(codepoint) = value.to_u32() else {
        return Err("%c arg not in range(0x110000)".to_owned());
    };
    let Some(ch) = char::from_u32(codepoint) else {
        return Err("%c arg not in range(0x110000)".to_owned());
    };
    apply_text_width(&ch.to_string(), spec)
}

fn format_float(value: f64, spec: &str) -> Result<String, String> {
    let parsed = ParsedFormatSpec::parse(spec)?;
    if matches!(parsed.ty, Some('n')) {
        if let Some(grouping) = parsed.grouping {
            return Err(format!("Cannot specify '{grouping}' with 'n'."));
        }
    }
    let ty = parsed.ty.unwrap_or('\0');
    if !matches!(ty, '\0' | 'e' | 'E' | 'f' | 'F' | 'g' | 'G' | 'n' | '%') {
        return Err(format!("unknown format code '{ty}' for object of type 'float'"));
    }
    if parsed.alternate && ty == '\0' {
        return Err("alternate form (#) not allowed in float empty format".to_owned());
    }

    let precision = parsed.precision.unwrap_or(6);
    let mut negative = value.is_sign_negative();
    let abs = value.abs();
    let mut suffix = "";
    let mut body = match ty {
        '\0' => float_type::repr_f64(abs),
        'e' | 'E' => format_float_exp(abs, precision, ty == 'E', parsed.alternate),
        'f' | 'F' => format_float_fixed(abs, precision, ty == 'F', parsed.alternate),
        'g' | 'G' | 'n' => format_float_general(abs, precision, parsed.alternate, ty == 'G'),
        '%' => {
            suffix = "%";
            format_float_fixed(abs * 100.0, precision, false, parsed.alternate)
        }
        _ => unreachable!(),
    };
    if parsed.z && negative && float_body_is_zero(&body) {
        negative = false;
    }
    if let Some(grouping) = parsed.grouping {
        body = group_float_body(&body, grouping);
    }
    let sign = sign_text(negative, parsed.sign);
    apply_number_width(sign, "", &body, suffix, &parsed)
}

fn format_float_fixed(value: f64, precision: usize, uppercase_special: bool, alternate: bool) -> String {
    if value.is_nan() {
        return if uppercase_special { "NAN".to_owned() } else { "nan".to_owned() };
    }
    if value.is_infinite() {
        return if uppercase_special { "INF".to_owned() } else { "inf".to_owned() };
    }
    let mut text = format!("{value:.precision$}");
    if alternate {
        ensure_float_decimal(&mut text);
    }
    text
}

fn format_float_exp(value: f64, precision: usize, uppercase: bool, alternate: bool) -> String {
    if value.is_nan() {
        return if uppercase { "NAN".to_owned() } else { "nan".to_owned() };
    }
    if value.is_infinite() {
        return if uppercase { "INF".to_owned() } else { "inf".to_owned() };
    }
    let mut text = format!("{value:.precision$e}");
    if alternate {
        ensure_float_decimal(&mut text);
    }
    normalize_float_exponent(&mut text, 'e');
    if uppercase { text.to_uppercase() } else { text }
}

fn format_float_general(value: f64, precision: usize, alternate: bool, uppercase: bool) -> String {
    if value.is_nan() {
        return if uppercase { "NAN".to_owned() } else { "nan".to_owned() };
    }
    if value.is_infinite() {
        return if uppercase { "INF".to_owned() } else { "inf".to_owned() };
    }
    let precision = precision.max(1);
    let exponent = if value == 0.0 { 0 } else { value.log10().floor() as i32 };
    let use_exp = exponent < -4 || exponent >= precision as i32;
    let mut text = if use_exp {
        let frac = precision.saturating_sub(1);
        format!("{value:.frac$e}")
    } else {
        let frac = (precision as i32 - exponent - 1).max(0) as usize;
        format!("{value:.frac$}")
    };
    if !alternate {
        trim_float_zeros(&mut text);
    } else {
        ensure_float_decimal(&mut text);
    }
    if use_exp {
        normalize_float_exponent(&mut text, 'e');
    }
    if uppercase { text.to_uppercase() } else { text }
}

fn ensure_float_decimal(text: &mut String) {
    let end = text.find('e').or_else(|| text.find('E')).unwrap_or(text.len());
    if !text[..end].contains('.') {
        text.insert(end, '.');
    }
}

fn normalize_float_exponent(text: &mut String, marker: char) {
    let Some(exp_pos) = text.find('e').or_else(|| text.find('E')) else {
        return;
    };
    let exponent = &text[exp_pos + 1..];
    let (sign, digits) = if let Some(rest) = exponent.strip_prefix('-') {
        ('-', rest)
    } else if let Some(rest) = exponent.strip_prefix('+') {
        ('+', rest)
    } else {
        ('+', exponent)
    };
    let digits = digits.trim_start_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let mut normalized = String::with_capacity(4 + digits.len());
    normalized.push(marker);
    normalized.push(sign);
    if digits.len() == 1 {
        normalized.push('0');
    }
    normalized.push_str(digits);
    text.replace_range(exp_pos.., &normalized);
}

fn trim_float_zeros(text: &mut String) {
    let exp = text.find('e');
    let end = exp.unwrap_or(text.len());
    if let Some(dot) = text[..end].find('.') {
        let mut trim_to = end;
        while trim_to > dot + 1 && text.as_bytes()[trim_to - 1] == b'0' {
            trim_to -= 1;
        }
        if trim_to == dot + 1 {
            trim_to = dot;
        }
        text.replace_range(trim_to..end, "");
    }
}

fn float_body_is_zero(body: &str) -> bool {
    let mantissa = body.split(['e', 'E']).next().unwrap_or(body);
    mantissa.chars().all(|ch| matches!(ch, '0' | '.' | ',' | '_'))
}


#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FormatAlign {
    Left,
    Right,
    Center,
    SignAware,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FormatSign {
    MinusOnly,
    Plus,
    Space,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ParsedFormatSpec {
    fill: char,
    align: Option<FormatAlign>,
    sign: Option<FormatSign>,
    z: bool,
    alternate: bool,
    zero: bool,
    width: Option<usize>,
    grouping: Option<char>,
    precision: Option<usize>,
    ty: Option<char>,
}

impl ParsedFormatSpec {
    fn parse(spec: &str) -> Result<Self, String> {
        let mut chars = spec.chars().peekable();
        let mut fill = ' ';
        let mut align = None;
        let mut clone = chars.clone();
        if let (Some(fill_ch), Some(align_ch)) = (clone.next(), clone.next()) {
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
        let sign = match chars.peek().copied() {
            Some('+') => {
                chars.next();
                Some(FormatSign::Plus)
            }
            Some('-') => {
                chars.next();
                Some(FormatSign::MinusOnly)
            }
            Some(' ') => {
                chars.next();
                Some(FormatSign::Space)
            }
            _ => None,
        };
        let z = if chars.peek() == Some(&'z') {
            chars.next();
            true
        } else {
            false
        };
        let alternate = if chars.peek() == Some(&'#') {
            chars.next();
            true
        } else {
            false
        };
        let zero = if chars.peek() == Some(&'0') {
            chars.next();
            fill = '0';
            true
        } else {
            false
        };
        let width = parse_digits(&mut chars)?;
        let grouping = match chars.peek().copied() {
            Some(ch @ (',' | '_')) => {
                chars.next();
                Some(ch)
            }
            _ => None,
        };
        let precision = if chars.peek() == Some(&'.') {
            chars.next();
            Some(parse_digits(&mut chars)?.unwrap_or(0))
        } else {
            None
        };
        let ty = chars.next();
        if chars.next().is_some() {
            return Err("Invalid format specifier".to_owned());
        }
        Ok(Self { fill, align, sign, z, alternate, zero, width, grouping, precision, ty })
    }
}

fn parse_align(ch: char) -> Option<FormatAlign> {
    match ch {
        '<' => Some(FormatAlign::Left),
        '>' => Some(FormatAlign::Right),
        '^' => Some(FormatAlign::Center),
        '=' => Some(FormatAlign::SignAware),
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

fn sign_text(negative: bool, sign: Option<FormatSign>) -> &'static str {
    if negative {
        "-"
    } else {
        match sign {
            Some(FormatSign::Plus) => "+",
            Some(FormatSign::Space) => " ",
            Some(FormatSign::MinusOnly) | None => "",
        }
    }
}

fn apply_number_width(sign: &str, prefix: &str, body: &str, suffix: &str, spec: &ParsedFormatSpec) -> Result<String, String> {
    let raw = format!("{sign}{prefix}{body}{suffix}");
    let Some(width) = spec.width else {
        return Ok(raw);
    };
    let len = str_type::codepoint_len(&raw);
    let pad = width.saturating_sub(len);
    if pad == 0 {
        return Ok(raw);
    }
    let align = if spec.zero && spec.align.is_none() {
        FormatAlign::SignAware
    } else {
        spec.align.unwrap_or(FormatAlign::Right)
    };
    if align == FormatAlign::SignAware {
        let mut out = String::with_capacity(raw.len() + pad * spec.fill.len_utf8());
        out.push_str(sign);
        out.push_str(prefix);
        push_fill(&mut out, spec.fill, pad);
        out.push_str(body);
        out.push_str(suffix);
        return Ok(out);
    }
    apply_width_with_align(&raw, spec.fill, align, pad)
}

fn apply_text_width(value: &str, spec: &ParsedFormatSpec) -> Result<String, String> {
    let Some(width) = spec.width else {
        return Ok(value.to_owned());
    };
    let len = str_type::codepoint_len(value);
    let pad = width.saturating_sub(len);
    if pad == 0 {
        return Ok(value.to_owned());
    }
    let align = spec.align.unwrap_or(FormatAlign::Left);
    if align == FormatAlign::SignAware {
        return Err("'=' alignment not allowed in this format specifier".to_owned());
    }
    apply_width_with_align(value, spec.fill, align, pad)
}

fn apply_width_with_align(value: &str, fill: char, align: FormatAlign, pad: usize) -> Result<String, String> {
    let mut out = String::with_capacity(value.len() + pad * fill.len_utf8());
    match align {
        FormatAlign::Left => {
            out.push_str(value);
            push_fill(&mut out, fill, pad);
        }
        FormatAlign::Right => {
            push_fill(&mut out, fill, pad);
            out.push_str(value);
        }
        FormatAlign::Center => {
            let left = pad / 2;
            let right = pad - left;
            push_fill(&mut out, fill, left);
            out.push_str(value);
            push_fill(&mut out, fill, right);
        }
        FormatAlign::SignAware => return Err("internal sign-aware alignment leak".to_owned()),
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

fn group_digits(digits: &str, sep: char, group: usize) -> String {
    let len = digits.chars().count();
    if len <= group {
        return digits.to_owned();
    }
    let mut out = String::with_capacity(digits.len() + len / group);
    for (index, ch) in digits.chars().enumerate() {
        if index != 0 && (len - index).is_multiple_of(group) {
            out.push(sep);
        }
        out.push(ch);
    }
    out
}

fn group_float_body(body: &str, sep: char) -> String {
    let exp_pos = body.find(['e', 'E']).unwrap_or(body.len());
    let head = &body[..exp_pos];
    let tail = &body[exp_pos..];
    let dot_pos = head.find('.').unwrap_or(head.len());
    let grouped = group_digits(&head[..dot_pos], sep, 3);
    let mut out = String::with_capacity(body.len() + grouped.len().saturating_sub(dot_pos));
    out.push_str(&grouped);
    out.push_str(&head[dot_pos..]);
    out.push_str(tail);
    out
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NumberingMode {
    None,
    Auto,
    Manual,
}

struct TemplateFormatState<'a> {
    args: &'a [*mut PyObject],
    mapping: Option<*mut PyObject>,
    auto_index: usize,
    numbering: NumberingMode,
}

unsafe fn render_template(template: &str, state: &mut TemplateFormatState<'_>) -> Result<String, String> {
    let mut out = String::new();
    let mut index = 0usize;
    while index < template.len() {
        let rest = &template[index..];
        if rest.starts_with("{{") {
            out.push('{');
            index += 2;
        } else if rest.starts_with("}}") {
            out.push('}');
            index += 2;
        } else if rest.starts_with('{') {
            let (field, next) = find_replacement_field(template, index + 1)?;
            out.push_str(&unsafe { render_field(field, state)? });
            index = next;
        } else if rest.starts_with('}') {
            return Err("Single '}' encountered in format string".to_owned());
        } else {
            let ch = rest.chars().next().expect("non-empty rest");
            out.push(ch);
            index += ch.len_utf8();
        }
    }
    Ok(out)
}

fn find_replacement_field(template: &str, mut index: usize) -> Result<(&str, usize), String> {
    let start = index;
    let mut nested = 0usize;
    while index < template.len() {
        let rest = &template[index..];
        let ch = rest.chars().next().expect("index is in bounds");
        match ch {
            '{' => {
                nested += 1;
                index += ch.len_utf8();
            }
            '}' if nested == 0 => return Ok((&template[start..index], index + 1)),
            '}' => {
                nested -= 1;
                index += ch.len_utf8();
            }
            _ => index += ch.len_utf8(),
        }
    }
    Err("expected '}' before end of string".to_owned())
}

unsafe fn render_field(field: &str, state: &mut TemplateFormatState<'_>) -> Result<String, String> {
    let (field_name, conversion, format_spec) = split_field(field)?;
    let value = unsafe { resolve_field(field_name, state)? };
    let spec = if let Some(spec) = format_spec {
        unsafe { render_template(spec, state)? }
    } else {
        String::new()
    };
    format_value_with_spec_text(value, conversion.unwrap_or(0), &spec, format_spec.is_some())
}

fn split_field(field: &str) -> Result<(&str, Option<u8>, Option<&str>), String> {
    let mut conversion_at = None;
    let mut spec_at = None;
    let mut nested = 0usize;
    for (index, ch) in field.char_indices() {
        match ch {
            '[' => nested += 1,
            ']' if nested > 0 => nested -= 1,
            '!' if nested == 0 && conversion_at.is_none() && spec_at.is_none() => conversion_at = Some(index),
            ':' if nested == 0 && spec_at.is_none() => spec_at = Some(index),
            _ => {}
        }
    }
    let name_end = conversion_at.or(spec_at).unwrap_or(field.len());
    let conversion = if let Some(index) = conversion_at {
        let end = spec_at.unwrap_or(field.len());
        let conv = &field[index + 1..end];
        match conv.as_bytes() {
            [b's'] | [b'r'] | [b'a'] => Some(conv.as_bytes()[0]),
            _ => return Err(format!("Unknown conversion specifier {conv}")),
        }
    } else {
        None
    };
    let format_spec = spec_at.map(|index| &field[index + 1..]);
    Ok((&field[..name_end], conversion, format_spec))
}

unsafe fn resolve_field(field_name: &str, state: &mut TemplateFormatState<'_>) -> Result<*mut PyObject, String> {
    let (head, mut rest) = split_field_head(field_name);
    let mut value = if head.is_empty() {
        if state.numbering == NumberingMode::Manual {
            return Err("cannot switch from manual field specification to automatic field numbering".to_owned());
        }
        state.numbering = NumberingMode::Auto;
        let index = state.auto_index;
        state.auto_index = state.auto_index.saturating_add(1);
        *state.args.get(index).ok_or_else(|| "Replacement index out of range".to_owned())?
    } else if head.chars().all(|ch| ch.is_ascii_digit()) {
        if state.numbering == NumberingMode::Auto {
            return Err("cannot switch from automatic field numbering to manual field specification".to_owned());
        }
        state.numbering = NumberingMode::Manual;
        let index = head.parse::<usize>().map_err(|_| "Replacement index out of range".to_owned())?;
        *state.args.get(index).ok_or_else(|| "Replacement index out of range".to_owned())?
    } else {
        let mapping = state.mapping.ok_or_else(|| format!("KeyError: {head}"))?;
        unsafe { mapping_get(mapping, head)? }
    };

    while !rest.is_empty() {
        if let Some(after_dot) = rest.strip_prefix('.') {
            let len = after_dot
                .find(['.', '['])
                .unwrap_or(after_dot.len());
            let attr = &after_dot[..len];
            if attr.is_empty() {
                return Err("Empty attribute in format string".to_owned());
            }
            value = unsafe { attr_get(value, attr)? };
            rest = &after_dot[len..];
        } else if let Some(after_bracket) = rest.strip_prefix('[') {
            let Some(end) = after_bracket.find(']') else {
                return Err("Missing ']' in format string".to_owned());
            };
            let key = &after_bracket[..end];
            value = unsafe { item_get(value, key)? };
            rest = &after_bracket[end + 1..];
        } else {
            return Err("Only '.', '[' may follow ']' in format field specifier".to_owned());
        }
    }
    Ok(value)
}

fn split_field_head(field_name: &str) -> (&str, &str) {
    let index = field_name.find(['.', '[']).unwrap_or(field_name.len());
    (&field_name[..index], &field_name[index..])
}

unsafe fn mapping_get(mapping: *mut PyObject, key: &str) -> Result<*mut PyObject, String> {
    let key = boxed_str(key)?;
    let value = unsafe { super::object::pon_subscript_get(mapping, key, ptr::null_mut()) };
    if value.is_null() {
        let message = pon_err_message().unwrap_or_else(|| "format mapping lookup failed".to_owned());
        pon_err_clear();
        Err(message)
    } else {
        Ok(value)
    }
}

unsafe fn attr_get(value: *mut PyObject, attr: &str) -> Result<*mut PyObject, String> {
    let object = unsafe { super::object::pon_get_attr(value, intern(attr), ptr::null_mut()) };
    if object.is_null() {
        let message = pon_err_message().unwrap_or_else(|| format!("attribute '{attr}' was not found"));
        pon_err_clear();
        Err(message)
    } else {
        Ok(object)
    }
}

unsafe fn item_get(value: *mut PyObject, key: &str) -> Result<*mut PyObject, String> {
    let key_object = if key.chars().all(|ch| ch.is_ascii_digit()) {
        let index = key.parse::<i64>().map_err(|_| "format item index is too large".to_owned())?;
        unsafe { super::pon_const_int(index) }
    } else {
        boxed_str(key)?
    };
    let object = unsafe { super::object::pon_subscript_get(value, key_object, ptr::null_mut()) };
    if object.is_null() {
        let message = pon_err_message().unwrap_or_else(|| "format item lookup failed".to_owned());
        pon_err_clear();
        Err(message)
    } else {
        Ok(object)
    }
}

unsafe fn call_custom_format(value: *mut PyObject, spec: &str) -> Result<Option<String>, String> {
    let callable = unsafe { super::object::pon_get_attr(value, intern("__format__"), ptr::null_mut()) };
    if callable.is_null() {
        pon_err_clear();
        return Ok(None);
    }
    let spec_object = boxed_str(spec)?;
    let mut args = [spec_object];
    let result = unsafe { super::pon_call(callable, args.as_mut_ptr(), args.len()) };
    if result.is_null() {
        let message = pon_err_message().unwrap_or_else(|| "__format__ call failed".to_owned());
        return Err(message);
    }
    Ok(Some(expect_str(result)?))
}

unsafe fn concat_templates(left: *mut PyObject, right: *mut PyObject) -> Result<*mut PyObject, String> {
    if !is_template(left) {
        return Err("can only concatenate string.templatelib.Template to string.templatelib.Template".to_owned());
    }
    if !is_template(right) {
        return Err(format!(
            "can only concatenate string.templatelib.Template (not \"{}\") to string.templatelib.Template",
            type_name(right)
        ));
    }
    let left = unsafe { &*left.cast::<PyTemplate>() };
    let right = unsafe { &*right.cast::<PyTemplate>() };
    let left_strings = tuple_items(left.strings)?;
    let right_strings = tuple_items(right.strings)?;
    let left_interps = tuple_items(left.interpolations)?;
    let right_interps = tuple_items(right.interpolations)?;

    let mut strings = Vec::with_capacity(left_strings.len() + right_strings.len().saturating_sub(1));
    if left_strings.is_empty() || right_strings.is_empty() {
        return Err("template strings tuple is malformed".to_owned());
    }
    strings.extend_from_slice(&left_strings[..left_strings.len() - 1]);
    let merged = format!(
        "{}{}",
        expect_str(left_strings[left_strings.len() - 1])?,
        expect_str(right_strings[0])?
    );
    strings.push(boxed_str(&merged)?);
    strings.extend_from_slice(&right_strings[1..]);

    let mut interpolations = Vec::with_capacity(left_interps.len() + right_interps.len());
    interpolations.extend_from_slice(left_interps);
    interpolations.extend_from_slice(right_interps);
    template_from_parts(strings, interpolations)
}

fn template_from_parts(strings: Vec<*mut PyObject>, interpolations: Vec<*mut PyObject>) -> Result<*mut PyObject, String> {
    let values = interpolations
        .iter()
        .map(|interp| unsafe { (*interp.cast::<PyInterpolation>()).value })
        .collect::<Vec<_>>();
    let strings_tuple = template_tuple(&strings)?;
    let values_tuple = template_tuple(&values)?;
    let interpolations_tuple = template_tuple(&interpolations)?;
    let object = Box::into_raw(Box::new(PyTemplate {
        ob_base: PyObjectHeader::new(template_type()),
        strings: strings_tuple,
        values: values_tuple,
        interpolations: interpolations_tuple,
    }));
    Ok(as_object_ptr(object))
}

fn template_tuple(items: &[*mut PyObject]) -> Result<*mut PyObject, String> {
    super::with_runtime(|runtime| super::seq::alloc_tuple_from_slice(runtime, items))
        .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
}

fn boxed_interpolation(part: &TStrPartRaw) -> Result<*mut PyObject, String> {
    let expression = if part.literal.is_null() && part.expression_interned != 0 {
        let Some(text) = resolve(part.expression_interned) else {
            return Err(format!("template interpolation expression id {} is not interned", part.expression_interned));
        };
        boxed_str(&text)?
    } else {
        boxed_str(raw_utf8(part.literal, part.literal_len).unwrap_or(""))?
    };
    let conversion = conversion_object(part.conversion)?;
    let format_spec = if part.format_spec.is_null() {
        boxed_str("")?
    } else {
        let _ = expect_str(part.format_spec)?;
        part.format_spec
    };
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
            if none.is_null() { Err("failed to allocate template interpolation conversion".to_owned()) } else { Ok(none) }
        }
        b's' => boxed_str("s"),
        b'r' => boxed_str("r"),
        b'a' => boxed_str("a"),
        _ => Err("unsupported template-string conversion".to_owned()),
    }
}

fn boxed_str(text: &str) -> Result<*mut PyObject, String> {
    let object = unsafe { super::pon_const_str(text.as_ptr(), text.len()) };
    if object.is_null() {
        Err("failed to allocate string object".to_owned())
    } else {
        Ok(object)
    }
}

fn expect_str(value: *mut PyObject) -> Result<String, String> {
    exact_str(value)?.ok_or_else(|| format!("expected str, got {}", type_name(value)))
}

fn exact_str(value: *mut PyObject) -> Result<Option<String>, String> {
    if value.is_null() {
        return Ok(None);
    }
    super::with_runtime(|runtime| unsafe {
        if is_exact_type(value, runtime.unicode_type) {
            let unicode = &*value.cast::<PyUnicode>();
            return unicode
                .as_str()
                .map(|text| Some(text.to_owned()))
                .ok_or_else(|| "unicode object contains invalid UTF-8".to_owned());
        }
        Ok(None)
    })
    .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
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
    if let Some(text) = exact_str(value)? {
        return Ok(str_type::repr(&text));
    }
    if let Some(value) = unsafe { bool_type::to_bool(value) } {
        return Ok(if value { "True".to_owned() } else { "False".to_owned() });
    }
    if let Some(value) = unsafe { int_type::to_bigint(value) } {
        return Ok(value.to_string());
    }
    if let Some(value) = unsafe { float_type::to_f64(value) } {
        return Ok(float_type::repr_f64(value));
    }
    crate::native::builtins_mod::try_repr_text(value).map_err(|()| "repr raised".to_owned())
}

fn type_name(value: *mut PyObject) -> String {
    if value.is_null() {
        return "NULL".to_owned();
    }
    unsafe {
        let ty = (*value).ob_type;
        if ty.is_null() {
            "object".to_owned()
        } else {
            (*ty).name().to_owned()
        }
    }
}

fn is_template(value: *mut PyObject) -> bool {
    unsafe { !value.is_null() && (*value).ob_type == template_type().cast_const() }
}

fn tuple_items(object: *mut PyObject) -> Result<&'static [*mut PyObject], String> {
    if object.is_null() || type_name(object) != "tuple" {
        return Err("expected tuple".to_owned());
    }
    Ok(unsafe { (&*object.cast::<PyTuple>()).as_slice() })
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

fn raw_template_parts<'a>(parts: *const TStrPartRaw, len: usize) -> Result<&'a [TStrPartRaw], String> {
    if parts.is_null() {
        return if len == 0 { Ok(&[]) } else { Err("template-string parts pointer is null".to_owned()) };
    }
    Ok(unsafe { core::slice::from_raw_parts(parts, len) })
}

#[allow(dead_code, reason = "kept adjacent to TStr raw parsing for future direct f-string raw helpers")]
fn raw_fstring_parts<'a>(parts: *const FStrPartRaw, len: usize) -> Result<&'a [FStrPartRaw], String> {
    if parts.is_null() {
        return if len == 0 { Ok(&[]) } else { Err("f-string parts pointer is null".to_owned()) };
    }
    Ok(unsafe { core::slice::from_raw_parts(parts, len) })
}

/// Positional-argument cursor for `%`-formatting, mirroring CPython's
/// `getnextarg` index arithmetic: a tuple carries `(len, 0)`, a single
/// argument carries `(-1, -2)` so it can be consumed exactly once.
struct PercentArgs<'a> {
    args: *mut PyObject,
    items: Option<&'a [*mut PyObject]>,
    arglen: isize,
    argidx: isize,
}

impl PercentArgs<'_> {
    fn next(&mut self) -> Result<*mut PyObject, String> {
        if self.argidx < self.arglen {
            let value = match self.items {
                Some(items) => items[self.argidx as usize],
                None => self.args,
            };
            self.argidx += 1;
            Ok(value)
        } else {
            Err("not enough arguments for format string".to_owned())
        }
    }

    fn leftover(&self) -> bool {
        self.argidx < self.arglen
    }
}

/// Mapping-capability probe for `%(key)s` specs and the trailing
/// "not all arguments converted" exemption (CPython `PyMapping_Check`).
fn percent_args_is_mapping(args: *mut PyObject) -> bool {
    if args.is_null() {
        return false;
    }
    if unsafe { crate::types::dict::has_dict_storage(args) } {
        return true;
    }
    let ty = unsafe { (*args).ob_type.cast_mut() };
    if ty.is_null() {
        return false;
    }
    unsafe { !crate::descr::lookup_in_type(ty, intern("__getitem__")).is_null() }
}

fn raise_percent_type_error(message: &str) -> *mut PyObject {
    unsafe { super::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn raise_percent_value_error(message: &str) -> *mut PyObject {
    unsafe { super::exc::pon_raise_value_error(message.as_ptr(), message.len()) }
}

fn pad_percent_text(text: &str, width: Option<usize>, left: bool) -> String {
    let Some(width) = width else {
        return text.to_owned();
    };
    let count = text.chars().count();
    if count >= width {
        return text.to_owned();
    }
    let pad = " ".repeat(width - count);
    if left { format!("{text}{pad}") } else { format!("{pad}{text}") }
}

/// Integer body for `%d`/`%o`/`%x`/`%X`: precision zero-extends the digits,
/// the zero flag pads between sign/prefix and digits (C `printf` rules; the
/// zero flag is ignored once a precision is given).
#[allow(clippy::too_many_arguments)]
fn render_percent_int(
    value: &BigInt,
    radix: u32,
    upper: bool,
    alternate: bool,
    plus: bool,
    space: bool,
    zero: bool,
    left: bool,
    width: Option<usize>,
    precision: Option<usize>,
) -> String {
    let negative = value.sign() == Sign::Minus;
    let mut digits = value.abs().to_str_radix(radix);
    if upper {
        digits.make_ascii_uppercase();
    }
    if let Some(precision) = precision {
        while digits.len() < precision {
            digits.insert(0, '0');
        }
    }
    let prefix = if alternate {
        match radix {
            8 => "0o",
            16 => {
                if upper {
                    "0X"
                } else {
                    "0x"
                }
            }
            _ => "",
        }
    } else {
        ""
    };
    let sign = if negative {
        "-"
    } else if plus {
        "+"
    } else if space {
        " "
    } else {
        ""
    };
    let body_len = sign.len() + prefix.len() + digits.len();
    match width {
        Some(width) if width > body_len => {
            let pad = width - body_len;
            if left {
                format!("{sign}{prefix}{digits}{}", " ".repeat(pad))
            } else if zero && precision.is_none() {
                format!("{sign}{prefix}{}{digits}", "0".repeat(pad))
            } else {
                format!("{}{sign}{prefix}{digits}", " ".repeat(pad))
            }
        }
        _ => format!("{sign}{prefix}{digits}"),
    }
}

/// `%d`/`%i`/`%u` operand: bool/int directly, float by truncation (CPython
/// routes through `PyNumber_Long`).
unsafe fn percent_int_operand(arg: *mut PyObject, allow_float: bool, ty: char) -> Result<BigInt, String> {
    if let Some(value) = unsafe { int_type::to_bigint_including_bool(arg) } {
        return Ok(value);
    }
    if allow_float {
        if let Some(value) = unsafe { float_type::to_f64(arg) } {
            return BigInt::from_f64(value.trunc())
                .ok_or_else(|| "cannot convert float infinity or NaN to integer".to_owned());
        }
        return Err(format!("%{ty} format: a real number is required, not {}", type_name(arg)));
    }
    Err(format!("%{ty} format: an integer is required, not {}", type_name(arg)))
}

/// `%e`/`%f`/`%g` operand: float directly, bool/int widened.
unsafe fn percent_float_operand(arg: *mut PyObject) -> Result<f64, String> {
    if let Some(value) = unsafe { float_type::to_f64(arg) } {
        return Ok(value);
    }
    if let Some(value) = unsafe { int_type::to_bigint_including_bool(arg) } {
        return value.to_f64().ok_or_else(|| "int too large to convert to float".to_owned());
    }
    Err(format!("must be real number, not {}", type_name(arg)))
}

/// CPython `str.__mod__` (%-formatting): renders `format % args`.  Raises the
/// matching Python exception and returns NULL on failure; mapping-key lookups
/// propagate their own exceptions (`KeyError`).
pub(crate) unsafe fn percent_format(format: *mut PyObject, args: *mut PyObject) -> *mut PyObject {
    let template = match exact_str(format) {
        Ok(Some(template)) => template,
        Ok(None) => return raise_percent_type_error("descriptor '__mod__' requires a 'str' object"),
        Err(message) => return raise_percent_type_error(&message),
    };
    // CPython `PyTuple_Check`: tuple SUBCLASS operands (namedtuple) spread as
    // individual args too; the storage view covers both tuple layouts.
    let items = unsafe { crate::abi::seq::tuple_storage_slice(args) };
    let (arglen, argidx) = match items {
        Some(items) => (items.len() as isize, 0),
        None => (-1, -2),
    };
    let mut state = PercentArgs { args, items, arglen, argidx };
    let arg_is_mapping = items.is_none() && type_name(args) != "str" && percent_args_is_mapping(args);

    let chars: Vec<char> = template.chars().collect();
    let mut out = String::with_capacity(template.len());
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if ch != '%' {
            out.push(ch);
            i += 1;
            continue;
        }
        i += 1;
        if i >= chars.len() {
            return raise_percent_value_error("incomplete format");
        }
        // Optional parenthesized mapping key, with nesting (CPython parity).
        let mut key = None;
        if chars[i] == '(' {
            let start = i + 1;
            let mut depth = 1usize;
            let mut j = start;
            while j < chars.len() && depth > 0 {
                match chars[j] {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            if depth != 0 {
                return raise_percent_value_error("incomplete format key");
            }
            key = Some(chars[start..j - 1].iter().collect::<String>());
            i = j;
        }
        let mut left = false;
        let mut plus = false;
        let mut space = false;
        let mut alternate = false;
        let mut zero = false;
        loop {
            match chars.get(i) {
                Some('-') => left = true,
                Some('+') => plus = true,
                Some(' ') => space = true,
                Some('#') => alternate = true,
                Some('0') => zero = true,
                _ => break,
            }
            i += 1;
        }
        let mut width = None;
        if chars.get(i) == Some(&'*') {
            i += 1;
            let value = match state.next() {
                Ok(value) => value,
                Err(message) => return raise_percent_type_error(&message),
            };
            let Some(value) = (unsafe { int_type::to_bigint_including_bool(value) }) else {
                return raise_percent_type_error("* wants int");
            };
            let Some(value) = value.to_isize() else {
                return raise_percent_value_error("width too big");
            };
            if value < 0 {
                left = true;
                width = Some(value.unsigned_abs());
            } else {
                width = Some(value.unsigned_abs());
            }
        } else {
            let mut value = 0usize;
            let mut any = false;
            while let Some(digit) = chars.get(i).and_then(|ch| ch.to_digit(10)) {
                value = value.saturating_mul(10).saturating_add(digit as usize);
                any = true;
                i += 1;
            }
            if any {
                width = Some(value);
            }
        }
        let mut precision = None;
        if chars.get(i) == Some(&'.') {
            i += 1;
            if chars.get(i) == Some(&'*') {
                i += 1;
                let value = match state.next() {
                    Ok(value) => value,
                    Err(message) => return raise_percent_type_error(&message),
                };
                let Some(value) = (unsafe { int_type::to_bigint_including_bool(value) }) else {
                    return raise_percent_type_error("* wants int");
                };
                let Some(value) = value.to_isize() else {
                    return raise_percent_value_error("precision too big");
                };
                precision = Some(value.max(0).unsigned_abs());
            } else {
                let mut value = 0usize;
                while let Some(digit) = chars.get(i).and_then(|ch| ch.to_digit(10)) {
                    value = value.saturating_mul(10).saturating_add(digit as usize);
                    i += 1;
                }
                precision = Some(value);
            }
        }
        while matches!(chars.get(i), Some('h') | Some('l') | Some('L')) {
            i += 1;
        }
        let Some(&ty) = chars.get(i) else {
            return raise_percent_value_error("incomplete format");
        };
        let ty_index = i;
        i += 1;
        if ty == '%' {
            out.push('%');
            continue;
        }
        let arg = if let Some(key) = key {
            if !arg_is_mapping {
                return raise_percent_type_error("format requires a mapping");
            }
            let key_object = match boxed_str(&key) {
                Ok(object) => object,
                Err(message) => return raise_percent_type_error(&message),
            };
            let value = unsafe { super::object::pon_subscript_get(args, key_object, ptr::null_mut()) };
            if value.is_null() {
                return ptr::null_mut();
            }
            value
        } else {
            match state.next() {
                Ok(value) => value,
                Err(message) => return raise_percent_type_error(&message),
            }
        };
        let rendered = match ty {
            's' | 'r' | 'a' => {
                let text = match ty {
                    's' => object_to_str(arg),
                    'r' => object_to_repr(arg),
                    _ => object_to_repr(arg).map(|text| str_type::escape_non_ascii(&text)),
                };
                let text = match text {
                    Ok(text) => text,
                    Err(message) => return raise_percent_type_error(&message),
                };
                let text = match precision {
                    Some(precision) => truncate_to_precision(&text, precision),
                    None => text,
                };
                pad_percent_text(&text, width, left)
            }
            'd' | 'i' | 'u' => match unsafe { percent_int_operand(arg, true, ty) } {
                Ok(value) => render_percent_int(&value, 10, false, false, plus, space, zero, left, width, precision),
                Err(message) => return raise_percent_type_error(&message),
            },
            'o' | 'x' | 'X' => match unsafe { percent_int_operand(arg, false, ty) } {
                Ok(value) => {
                    render_percent_int(&value, if ty == 'o' { 8 } else { 16 }, ty == 'X', alternate, plus, space, zero, left, width, precision)
                }
                Err(message) => return raise_percent_type_error(&message),
            },
            'e' | 'E' | 'f' | 'F' | 'g' | 'G' => {
                let value = match unsafe { percent_float_operand(arg) } {
                    Ok(value) => value,
                    Err(message) => return raise_percent_type_error(&message),
                };
                let sign = if plus {
                    "+"
                } else if space {
                    " "
                } else {
                    ""
                };
                let hash = if alternate { "#" } else { "" };
                let prec = precision.unwrap_or(6);
                let spec = match width {
                    Some(w) if left => format!("<{sign}{hash}{w}.{prec}{ty}"),
                    Some(w) if zero => format!("{sign}{hash}0{w}.{prec}{ty}"),
                    Some(w) => format!("{sign}{hash}{w}.{prec}{ty}"),
                    None => format!("{sign}{hash}.{prec}{ty}"),
                };
                match format_float(value, &spec) {
                    Ok(text) => text,
                    Err(message) => return raise_percent_value_error(&message),
                }
            }
            'c' => {
                let text = match exact_str(arg) {
                    Ok(text) => text,
                    Err(message) => return raise_percent_type_error(&message),
                };
                if let Some(text) = text {
                    if text.chars().count() != 1 {
                        return raise_percent_type_error("%c requires an int or a unicode character, not str");
                    }
                    pad_percent_text(&text, width, left)
                } else if let Some(value) = unsafe { int_type::to_bigint_including_bool(arg) } {
                    let Some(ch) = value.to_u32().and_then(char::from_u32) else {
                        return raise_percent_value_error("%c arg not in range(0x110000)");
                    };
                    pad_percent_text(&ch.to_string(), width, left)
                } else {
                    return raise_percent_type_error(&format!(
                        "%c requires an int or a unicode character, not {}",
                        type_name(arg)
                    ));
                }
            }
            other => {
                return raise_percent_value_error(&format!(
                    "unsupported format character '{other}' (0x{:x}) at index {ty_index}",
                    other as u32
                ));
            }
        };
        out.push_str(&rendered);
    }
    if !arg_is_mapping && state.leftover() {
        return raise_percent_type_error("not all arguments converted during string formatting");
    }
    match boxed_str(&out) {
        Ok(object) => object,
        Err(message) => raise_percent_type_error(&message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread_state::test_state_lock;

    fn init() -> std::sync::MutexGuard<'static, ()> {
        let guard = test_state_lock();
        unsafe {
            assert_eq!(super::super::pon_runtime_init(), 0);
        }
        guard
    }

    #[test]
    fn formats_integer_bases_grouping_and_sign_aware_zero() {
        let _guard = init();
        unsafe {
            let value = super::super::pon_const_int(-255);
            assert_eq!(format_object_with_spec(value, "#08x").unwrap(), "-0x000ff");
            assert_eq!(format_object_with_spec(value, "+,d").unwrap(), "-255");
            let big = int_type::from_bigint(BigInt::from(65_535));
            assert_eq!(format_object_with_spec(big, "#_x").unwrap(), "0xffff");
        }
    }

    #[test]
    fn formats_float_percent_z_and_general() {
        let _guard = init();
        let value = float_type::from_f64(-0.00001);
        assert_eq!(format_object_with_spec(value, "z.1f").unwrap(), "0.0");
        assert_eq!(format_object_with_spec(float_type::from_f64(0.125), ".1%").unwrap(), "12.5%");
        assert_eq!(format_object_with_spec(float_type::from_f64(12345.0), ",.2f").unwrap(), "12,345.00");
    }

    #[test]
    fn formats_text_alignment_and_precision() {
        let _guard = init();
        let value = boxed_str("héllo").unwrap();
        assert_eq!(format_object_with_spec(value, ".3s").unwrap(), "hél");
        assert_eq!(format_object_with_spec(value, "*^7").unwrap(), "*héllo*");
    }

    #[test]
    fn template_renderer_resolves_auto_manual_attrs_and_items() {
        let _guard = init();
        unsafe {
            let first = boxed_str("alpha").unwrap();
            let second = super::super::pon_const_int(42);
            let args = [first, second];
            assert_eq!(format_template("{0} {1:#x} {{ok}}", &args, None).unwrap(), "alpha 0x2a {ok}");
        }
    }
}
