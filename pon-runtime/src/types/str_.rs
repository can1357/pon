//! Unicode string implementation.
//!
//! Tier-0 strings store validated UTF-8 in [`crate::object::PyUnicode`].  The
//! helpers here keep Python-visible operations in code-point space whenever an
//! index or length is exposed, while preserving UTF-8 bytes for allocation and
//! ABI transfer.  Unicode policy for K2 is deliberately dependency-free: case
//! conversion uses Rust's standard `char` Unicode mappings; predicates use Rust's
//! Unicode properties plus the small decimal/digit/identifier tables below where
//! CPython exposes a property Rust does not expose directly.

use std::collections::HashMap;

/// Result of a Python `str.startswith`-style predicate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrPredicate {
    True,
    False,
}

impl StrPredicate {
    #[must_use]
    pub fn as_python_text(self) -> &'static str {
        match self {
            Self::True => "True",
            Self::False => "False",
        }
    }
}

/// Owned translation table produced by the representative `str.maketrans` path.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TranslationTable {
    map: HashMap<char, Option<String>>,
}

impl TranslationTable {
    #[must_use]
    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }

    pub fn insert(&mut self, key: char, value: Option<String>) {
        self.map.insert(key, value);
    }

    #[must_use]
    pub fn get(&self, key: char) -> Option<&Option<String>> {
        self.map.get(&key)
    }
}

/// Returns the number of Unicode scalar values in `text`.
#[must_use]
pub fn codepoint_len(text: &str) -> usize {
    text.chars().count()
}

/// Converts a Python code-point index into a UTF-8 byte offset.
#[must_use]
pub fn byte_offset_for_codepoint(text: &str, index: usize) -> Option<usize> {
    if index == codepoint_len(text) {
        return Some(text.len());
    }
    text.char_indices().nth(index).map(|(offset, _)| offset)
}

/// Concatenates two Python strings.
#[must_use]
pub fn concat(left: &str, right: &str) -> String {
    let mut out = String::with_capacity(left.len() + right.len());
    out.push_str(left);
    out.push_str(right);
    out
}

/// Repeats a Python string, saturating negative counts to the empty string.
#[must_use]
pub fn repeat(text: &str, count: isize) -> String {
    let count = usize::try_from(count).unwrap_or(0);
    let mut out = String::with_capacity(text.len().saturating_mul(count));
    for _ in 0..count {
        out.push_str(text);
    }
    out
}

/// Python `str.find`, returning a code-point offset or `-1`.
#[must_use]
pub fn find(haystack: &str, needle: &str) -> isize {
    find_range(haystack, needle, 0, codepoint_len(haystack)).unwrap_or(-1)
}

#[must_use]
pub fn find_range(text: &str, needle: &str, start: usize, end: usize) -> Option<isize> {
    let (slice, cp_start) = bounded_slice(text, start, end);
    if needle.is_empty() {
        return Some(cp_start as isize);
    }
    slice.find(needle).map(|byte| (cp_start + codepoint_len(&slice[..byte])) as isize)
}

#[must_use]
pub fn rfind_range(text: &str, needle: &str, start: usize, end: usize) -> Option<isize> {
    let (slice, cp_start) = bounded_slice(text, start, end);
    if needle.is_empty() {
        return Some((cp_start + codepoint_len(slice)) as isize);
    }
    slice.rfind(needle).map(|byte| (cp_start + codepoint_len(&slice[..byte])) as isize)
}

/// Python `str.startswith` for one prefix.
#[must_use]
pub fn startswith(text: &str, prefix: &str) -> StrPredicate {
    startswith_range(text, prefix, 0, codepoint_len(text))
}

#[must_use]
pub fn startswith_range(text: &str, prefix: &str, start: usize, end: usize) -> StrPredicate {
    let (slice, _) = bounded_slice(text, start, end);
    if slice.starts_with(prefix) { StrPredicate::True } else { StrPredicate::False }
}

#[must_use]
pub fn endswith_range(text: &str, suffix: &str, start: usize, end: usize) -> StrPredicate {
    let (slice, _) = bounded_slice(text, start, end);
    if slice.ends_with(suffix) { StrPredicate::True } else { StrPredicate::False }
}

/// Python `str.count`, counting non-overlapping occurrences in a range.
#[must_use]
pub fn count_range(text: &str, needle: &str, start: usize, end: usize) -> usize {
    let (slice, _) = bounded_slice(text, start, end);
    if needle.is_empty() {
        return codepoint_len(slice) + 1;
    }
    let mut count = 0usize;
    let mut rest = slice;
    while let Some(pos) = rest.find(needle) {
        count += 1;
        rest = &rest[pos + needle.len()..];
    }
    count
}

/// Python `str.replace` with a CPython-shaped `count` argument (`None`/negative = unlimited).
#[must_use]
pub fn replace_count(text: &str, old: &str, new: &str, count: Option<isize>) -> String {
    let limit = count.and_then(|value| usize::try_from(value).ok());
    if matches!(limit, Some(0)) {
        return text.to_owned();
    }
    if old.is_empty() {
        let max_insertions = text.chars().count() + 1;
        let wanted = limit.unwrap_or(max_insertions).min(max_insertions);
        let mut inserted = 0usize;
        let mut out = String::with_capacity(text.len() + new.len().saturating_mul(wanted));
        if inserted < wanted {
            out.push_str(new);
            inserted += 1;
        }
        for ch in text.chars() {
            out.push(ch);
            if inserted < wanted {
                out.push_str(new);
                inserted += 1;
            }
        }
        return out;
    }

    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut done = 0usize;
    while limit.is_none_or(|max| done < max) {
        let Some(pos) = rest.find(old) else { break; };
        out.push_str(&rest[..pos]);
        out.push_str(new);
        rest = &rest[pos + old.len()..];
        done += 1;
    }
    out.push_str(rest);
    out
}

/// Python `str.replace` for the common unlimited-count path.
#[must_use]
pub fn replace(text: &str, old: &str, new: &str) -> String {
    replace_count(text, old, new, None)
}

/// Python `str.split` for either a concrete separator or whitespace splitting.
#[must_use]
pub fn split(text: &str, sep: Option<&str>) -> Vec<String> {
    split_limited(text, sep, -1)
}

#[must_use]
pub fn split_limited(text: &str, sep: Option<&str>, maxsplit: isize) -> Vec<String> {
    match sep {
        Some(sep) => split_on(text, sep, maxsplit),
        None => split_whitespace(text, maxsplit),
    }
}

#[must_use]
pub fn rsplit_limited(text: &str, sep: Option<&str>, maxsplit: isize) -> Vec<String> {
    match sep {
        Some(sep) => rsplit_on(text, sep, maxsplit),
        None => rsplit_whitespace(text, maxsplit),
    }
}

#[must_use]
pub fn splitlines(text: &str, keepends: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let ch = text[i..].chars().next().expect("valid utf-8 boundary");
        let ch_len = ch.len_utf8();
        let mut end = None;
        let mut next = i + ch_len;
        match ch {
            '\n' | '\u{000B}' | '\u{000C}' | '\u{001C}' | '\u{001D}' | '\u{001E}' | '\u{0085}' | '\u{2028}' | '\u{2029}' => {
                end = Some(if keepends { next } else { i });
            }
            '\r' => {
                if next < bytes.len() && bytes[next] == b'\n' {
                    next += 1;
                }
                end = Some(if keepends { next } else { i });
            }
            _ => {}
        }
        if let Some(end_index) = end {
            out.push(text[start..end_index].to_owned());
            start = next;
        }
        i = next;
    }
    if start < text.len() {
        out.push(text[start..].to_owned());
    }
    out
}

/// Python `str.join` over pre-rendered string items.
#[must_use]
pub fn join(sep: &str, items: &[String]) -> String {
    items.join(sep)
}

#[must_use]
pub fn strip(text: &str, chars: Option<&str>) -> String {
    strip_sides(text, chars, true, true)
}

#[must_use]
pub fn lstrip(text: &str, chars: Option<&str>) -> String {
    strip_sides(text, chars, true, false)
}

#[must_use]
pub fn rstrip(text: &str, chars: Option<&str>) -> String {
    strip_sides(text, chars, false, true)
}

#[must_use]
pub fn lower(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

#[must_use]
pub fn upper(text: &str) -> String {
    text.chars().flat_map(char::to_uppercase).collect()
}

#[must_use]
pub fn casefold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\u{00DF}' | '\u{1E9E}' => out.push_str("ss"),
            _ => out.extend(ch.to_lowercase()),
        }
    }
    out
}

#[must_use]
pub fn swapcase(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_lowercase() {
            out.extend(ch.to_uppercase());
        } else if ch.is_uppercase() {
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[must_use]
pub fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    let Some(first) = chars.next() else { return String::new(); };
    let mut out = String::with_capacity(text.len());
    out.extend(first.to_uppercase());
    for ch in chars {
        out.extend(ch.to_lowercase());
    }
    out
}

#[must_use]
pub fn title(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut new_word = true;
    for ch in text.chars() {
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
    out
}

#[must_use]
pub fn center(text: &str, width: usize, fill: char) -> String {
    pad(text, width, fill, PadMode::Center)
}

#[must_use]
pub fn ljust(text: &str, width: usize, fill: char) -> String {
    pad(text, width, fill, PadMode::Left)
}

#[must_use]
pub fn rjust(text: &str, width: usize, fill: char) -> String {
    pad(text, width, fill, PadMode::Right)
}

#[must_use]
pub fn zfill(text: &str, width: usize) -> String {
    let len = codepoint_len(text);
    if width <= len {
        return text.to_owned();
    }
    let fill = width - len;
    let mut chars = text.chars();
    let first = chars.next();
    let mut out = String::with_capacity(text.len() + fill);
    if matches!(first, Some('+') | Some('-')) {
        out.push(first.unwrap());
        out.extend(std::iter::repeat_n('0', fill));
        out.extend(chars);
    } else {
        out.extend(std::iter::repeat_n('0', fill));
        out.push_str(text);
    }
    out
}

#[must_use]
pub fn expandtabs(text: &str, tabsize: isize) -> String {
    let tabsize = usize::try_from(tabsize).unwrap_or(0);
    let mut out = String::with_capacity(text.len());
    let mut column = 0usize;
    for ch in text.chars() {
        match ch {
            '\t' => {
                let spaces = if tabsize == 0 { 0 } else { tabsize - (column % tabsize) };
                out.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
            }
            '\n' | '\r' => {
                out.push(ch);
                column = 0;
            }
            _ => {
                out.push(ch);
                column += 1;
            }
        }
    }
    out
}

#[must_use]
pub fn partition(text: &str, sep: &str) -> (String, String, String) {
    if sep.is_empty() {
        return (text.to_owned(), String::new(), String::new());
    }
    match text.find(sep) {
        Some(pos) => (text[..pos].to_owned(), sep.to_owned(), text[pos + sep.len()..].to_owned()),
        None => (text.to_owned(), String::new(), String::new()),
    }
}

#[must_use]
pub fn rpartition(text: &str, sep: &str) -> (String, String, String) {
    if sep.is_empty() {
        return (String::new(), String::new(), text.to_owned());
    }
    match text.rfind(sep) {
        Some(pos) => (text[..pos].to_owned(), sep.to_owned(), text[pos + sep.len()..].to_owned()),
        None => (String::new(), String::new(), text.to_owned()),
    }
}

#[must_use]
pub fn removeprefix(text: &str, prefix: &str) -> String {
    text.strip_prefix(prefix).unwrap_or(text).to_owned()
}

#[must_use]
pub fn removesuffix(text: &str, suffix: &str) -> String {
    text.strip_suffix(suffix).unwrap_or(text).to_owned()
}

#[must_use]
pub fn is_decimal_str(text: &str) -> bool {
    nonempty_all(text, is_decimal_char)
}

#[must_use]
pub fn is_digit_str(text: &str) -> bool {
    nonempty_all(text, is_digit_char)
}

#[must_use]
pub fn is_numeric_str(text: &str) -> bool {
    nonempty_all(text, |ch| ch.is_numeric() || is_digit_char(ch))
}

#[must_use]
pub fn is_alpha_str(text: &str) -> bool {
    nonempty_all(text, char::is_alphabetic)
}

#[must_use]
pub fn is_alnum_str(text: &str) -> bool {
    nonempty_all(text, |ch| ch.is_alphanumeric() || is_digit_char(ch))
}

#[must_use]
pub fn is_space_str(text: &str) -> bool {
    nonempty_all(text, char::is_whitespace)
}

#[must_use]
pub fn is_ascii_str(text: &str) -> bool {
    text.is_ascii()
}

#[must_use]
pub fn is_lower_str(text: &str) -> bool {
    let mut cased = false;
    for ch in text.chars() {
        if ch.is_uppercase() {
            return false;
        }
        if ch.is_lowercase() {
            cased = true;
        }
    }
    cased
}

#[must_use]
pub fn is_upper_str(text: &str) -> bool {
    let mut cased = false;
    for ch in text.chars() {
        if ch.is_lowercase() {
            return false;
        }
        if ch.is_uppercase() {
            cased = true;
        }
    }
    cased
}

#[must_use]
pub fn is_title_str(text: &str) -> bool {
    let mut cased = false;
    let mut expect_upper = true;
    for ch in text.chars() {
        if ch.is_uppercase() {
            if !expect_upper {
                return false;
            }
            cased = true;
            expect_upper = false;
        } else if ch.is_lowercase() {
            if expect_upper {
                return false;
            }
            cased = true;
        } else if !ch.is_alphabetic() {
            expect_upper = true;
        }
    }
    cased
}

#[must_use]
pub fn is_identifier_str(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else { return false; };
    if !is_identifier_start(first) {
        return false;
    }
    chars.all(is_identifier_continue)
}

#[must_use]
pub fn is_printable_str(text: &str) -> bool {
    text.chars().all(is_printable_char)
}

#[must_use]
pub fn translate(text: &str, table: &TranslationTable) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match table.get(ch) {
            Some(Some(replacement)) => out.push_str(replacement),
            Some(None) => {}
            None => out.push(ch),
        }
    }
    out
}

pub fn maketrans(from: &str, to: &str, delete: Option<&str>) -> Result<TranslationTable, String> {
    if codepoint_len(from) != codepoint_len(to) {
        return Err("the first two maketrans arguments must have equal length".to_owned());
    }
    let mut table = TranslationTable::new();
    for (source, target) in from.chars().zip(to.chars()) {
        table.insert(source, Some(target.to_string()));
    }
    if let Some(delete) = delete {
        for ch in delete.chars() {
            table.insert(ch, None);
        }
    }
    Ok(table)
}

/// Python `str.encode()` for the default UTF-8 path.
#[must_use]
pub fn encode_utf8(text: &str) -> Vec<u8> {
    text.as_bytes().to_vec()
}

/// Formats a string list exactly like CPython's list repr for string elements.
#[must_use]
pub fn repr_string_list(items: &[String]) -> String {
    let mut out = String::from("[");
    for (index, item) in items.iter().enumerate() {
        if index != 0 {
            out.push_str(", ");
        }
        out.push_str(&repr(item));
    }
    out.push(']');
    out
}

/// CPython-shaped `repr(str)` for representative ASCII and Unicode text.
#[must_use]
pub fn repr(text: &str) -> String {
    let contains_single = text.contains('\'');
    let contains_double = text.contains('"');
    let quote = if contains_single && !contains_double { '"' } else { '\'' };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' if quote == '\'' => out.push_str("\\'"),
            '"' if quote == '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => push_hex_escape(&mut out, ch),
            ch => out.push(ch),
        }
    }
    out.push(quote);
    out
}

/// CPython `ascii(str)` shape: `repr`, then escape non-ASCII scalars.
#[must_use]
pub fn ascii_repr(text: &str) -> String {
    escape_non_ascii(&repr(text))
}

/// Escapes non-ASCII scalars using CPython-compatible lowercase hex escapes.
#[must_use]
pub fn escape_non_ascii(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else if (ch as u32) <= 0xff {
            out.push_str(&format!("\\x{:02x}", ch as u32));
        } else if (ch as u32) <= 0xffff {
            out.push_str(&format!("\\u{:04x}", ch as u32));
        } else {
            out.push_str(&format!("\\U{:08x}", ch as u32));
        }
    }
    out
}

fn bounded_slice(text: &str, start: usize, end: usize) -> (&str, usize) {
    let len = codepoint_len(text);
    let start = start.min(len);
    let end = end.min(len).max(start);
    let start_byte = byte_offset_for_codepoint(text, start).unwrap_or(text.len());
    let end_byte = byte_offset_for_codepoint(text, end).unwrap_or(text.len());
    (&text[start_byte..end_byte], start)
}

fn split_on(text: &str, sep: &str, maxsplit: isize) -> Vec<String> {
    if sep.is_empty() {
        return vec![text.to_owned()];
    }
    let limit = usize::try_from(maxsplit).ok();
    let mut out = Vec::new();
    let mut rest = text;
    while limit.is_none_or(|max| out.len() < max) {
        let Some(pos) = rest.find(sep) else { break; };
        out.push(rest[..pos].to_owned());
        rest = &rest[pos + sep.len()..];
    }
    out.push(rest.to_owned());
    out
}

fn rsplit_on(text: &str, sep: &str, maxsplit: isize) -> Vec<String> {
    if sep.is_empty() {
        return vec![text.to_owned()];
    }
    let limit = usize::try_from(maxsplit).ok();
    let mut out = Vec::new();
    let mut rest = text;
    while limit.is_none_or(|max| out.len() < max) {
        let Some(pos) = rest.rfind(sep) else { break; };
        out.push(rest[pos + sep.len()..].to_owned());
        rest = &rest[..pos];
    }
    out.push(rest.to_owned());
    out.reverse();
    out
}

fn split_whitespace(text: &str, maxsplit: isize) -> Vec<String> {
    let limit = usize::try_from(maxsplit).ok();
    if matches!(limit, Some(0)) {
        let trimmed = text.trim_start_matches(char::is_whitespace);
        return if trimmed.is_empty() { Vec::new() } else { vec![trimmed.to_owned()] };
    }
    let mut out = Vec::new();
    let mut rest = text.trim_start_matches(char::is_whitespace);
    while !rest.is_empty() && limit.is_none_or(|max| out.len() < max) {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        out.push(rest[..end].to_owned());
        rest = rest[end..].trim_start_matches(char::is_whitespace);
    }
    if !rest.is_empty() {
        out.push(rest.to_owned());
    }
    out
}

fn rsplit_whitespace(text: &str, maxsplit: isize) -> Vec<String> {
    let limit = usize::try_from(maxsplit).ok();
    if matches!(limit, Some(0)) {
        let trimmed = text.trim_end_matches(char::is_whitespace);
        return if trimmed.is_empty() { Vec::new() } else { vec![trimmed.to_owned()] };
    }
    let mut out = Vec::new();
    let mut rest = text.trim_end_matches(char::is_whitespace);
    while !rest.is_empty() && limit.is_none_or(|max| out.len() < max) {
        let start = rest.rfind(char::is_whitespace).map_or(0, |index| index + rest[index..].chars().next().unwrap().len_utf8());
        out.push(rest[start..].to_owned());
        rest = rest[..start].trim_end_matches(char::is_whitespace);
    }
    if !rest.is_empty() {
        out.push(rest.to_owned());
    }
    out.reverse();
    out
}

fn strip_sides(text: &str, chars: Option<&str>, left: bool, right: bool) -> String {
    let should_strip = |ch: char| match chars {
        Some(chars) => chars.contains(ch),
        None => ch.is_whitespace(),
    };
    let mut start = 0usize;
    let mut end = text.len();
    if left {
        for (idx, ch) in text.char_indices() {
            if should_strip(ch) {
                start = idx + ch.len_utf8();
            } else {
                break;
            }
        }
    }
    if right {
        for (idx, ch) in text.char_indices().rev() {
            if idx < start {
                break;
            }
            if should_strip(ch) {
                end = idx;
            } else {
                break;
            }
        }
    }
    text[start..end].to_owned()
}

#[derive(Clone, Copy)]
enum PadMode {
    Left,
    Right,
    Center,
}

fn pad(text: &str, width: usize, fill: char, mode: PadMode) -> String {
    let len = codepoint_len(text);
    if width <= len {
        return text.to_owned();
    }
    let total = width - len;
    let (left, right) = match mode {
        PadMode::Left => (0, total),
        PadMode::Right => (total, 0),
        PadMode::Center => (total / 2, total - (total / 2)),
    };
    let mut out = String::with_capacity(text.len() + fill.len_utf8().saturating_mul(total));
    out.extend(std::iter::repeat_n(fill, left));
    out.push_str(text);
    out.extend(std::iter::repeat_n(fill, right));
    out
}

fn nonempty_all(text: &str, predicate: impl Fn(char) -> bool) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else { return false; };
    predicate(first) && chars.all(predicate)
}

fn in_ranges(ch: char, ranges: &[(u32, u32)]) -> bool {
    let value = ch as u32;
    ranges.iter().any(|&(start, end)| (start..=end).contains(&value))
}

fn is_decimal_char(ch: char) -> bool {
    const DECIMAL_RANGES: &[(u32, u32)] = &[
        (0x0030, 0x0039), (0x0660, 0x0669), (0x06F0, 0x06F9), (0x0966, 0x096F),
        (0x09E6, 0x09EF), (0x0A66, 0x0A6F), (0x0AE6, 0x0AEF), (0x0B66, 0x0B6F),
        (0x0BE6, 0x0BEF), (0x0C66, 0x0C6F), (0x0CE6, 0x0CEF), (0x0D66, 0x0D6F),
        (0x0E50, 0x0E59), (0x0ED0, 0x0ED9), (0x0F20, 0x0F29), (0x1040, 0x1049),
        (0x17E0, 0x17E9), (0x1810, 0x1819), (0xFF10, 0xFF19), (0x1D7CE, 0x1D7FF),
    ];
    in_ranges(ch, DECIMAL_RANGES)
}

fn is_digit_char(ch: char) -> bool {
    is_decimal_char(ch)
        || matches!(
            ch,
            '\u{00B2}' | '\u{00B3}' | '\u{00B9}' | '\u{2070}' | '\u{2074}'..='\u{2079}' | '\u{2080}'..='\u{2089}'
        )
}

fn is_identifier_start(ch: char) -> bool {
    ch == '_' || ch.is_alphabetic() || matches!(ch, '\u{1885}' | '\u{1886}' | '\u{2118}' | '\u{212E}' | '\u{309B}' | '\u{309C}')
}

fn is_identifier_continue(ch: char) -> bool {
    is_identifier_start(ch)
        || is_decimal_char(ch)
        || in_ranges(ch, &[(0x0300, 0x036F), (0x1AB0, 0x1AFF), (0x1DC0, 0x1DFF), (0x20D0, 0x20FF), (0xFE20, 0xFE2F)])
        || ch == '\u{00B7}'
}

fn is_printable_char(ch: char) -> bool {
    ch == ' '
        || (!ch.is_control()
            && !matches!(
                ch,
                '\u{00AD}' | '\u{0600}'..='\u{0605}' | '\u{061C}' | '\u{06DD}' | '\u{070F}' | '\u{0890}'..='\u{0891}'
                    | '\u{08E2}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2060}'..='\u{206F}'
                    | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}'
            ))
}

fn push_hex_escape(out: &mut String, ch: char) {
    if (ch as u32) <= 0xff {
        out.push_str(&format!("\\x{:02x}", ch as u32));
    } else if (ch as u32) <= 0xffff {
        out.push_str(&format!("\\u{:04x}", ch as u32));
    } else {
        out.push_str(&format!("\\U{:08x}", ch as u32));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repr_chooses_quotes_and_escapes_controls() {
        assert_eq!(repr("a'b"), "\"a'b\"");
        assert_eq!(repr("a\n"), "'a\\n'");
        assert_eq!(ascii_repr("é"), "'\\xe9'");
    }

    #[test]
    fn split_replace_and_find_use_codepoint_offsets() {
        assert_eq!(split("a--b--", Some("--")), vec!["a", "b", ""]);
        assert_eq!(rsplit_limited("a--b--c", Some("--"), 1), vec!["a--b", "c"]);
        assert_eq!(replace_count("banana", "na", "X", Some(1)), "baXna");
        assert_eq!(find("éx", "x"), 1);
        assert_eq!(rfind_range("éxéx", "x", 0, 4), Some(3));
    }

    #[test]
    fn unicode_case_and_predicates_cover_k2_corpus() {
        assert_eq!(upper("ß"), "SS");
        assert_eq!(lower("İ"), "i\u{307}");
        assert_eq!(casefold("Straße"), "strasse");
        assert!(is_identifier_str("e\u{301}"));
        assert!(is_decimal_str("١٢"));
        assert!(is_digit_str("²"));
        assert!(is_numeric_str("Ⅻ"));
    }
}
