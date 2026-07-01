//! Unicode string implementation.
//!
//! Tier-0 strings store validated UTF-8 in [`crate::object::PyUnicode`].  The
//! helpers here keep Python-visible operations in code-point space whenever an
//! index or length is exposed, while preserving UTF-8 bytes for allocation and
//! ABI transfer.

/// Result of a Python `str.startswith`-style predicate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrPredicate {
    False,
    True,
}

impl StrPredicate {
    #[must_use]
    pub const fn as_python_text(self) -> &'static str {
        match self {
            Self::False => "False",
            Self::True => "True",
        }
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
    match haystack.find(needle) {
        Some(byte_offset) => haystack[..byte_offset].chars().count() as isize,
        None => -1,
    }
}

/// Python `str.startswith` for one prefix.
#[must_use]
pub fn startswith(text: &str, prefix: &str) -> StrPredicate {
    if text.starts_with(prefix) {
        StrPredicate::True
    } else {
        StrPredicate::False
    }
}

/// Python `str.replace` for the common unlimited-count path.
#[must_use]
pub fn replace(text: &str, old: &str, new: &str) -> String {
    text.replace(old, new)
}

/// Python `str.split` for either a concrete separator or whitespace splitting.
#[must_use]
pub fn split(text: &str, sep: Option<&str>) -> Vec<String> {
    match sep {
        Some(sep) => text.split(sep).map(ToOwned::to_owned).collect(),
        None => text.split_whitespace().map(ToOwned::to_owned).collect(),
    }
}

/// Python `str.join` over pre-rendered string items.
#[must_use]
pub fn join(sep: &str, items: &[String]) -> String {
    items.join(sep)
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
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
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
            '\0' => out.push_str("\\x00"),
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
        let code = ch as u32;
        if code <= 0x7f {
            out.push(ch);
        } else if code <= 0xff {
            out.push_str(&format!("\\x{code:02x}"));
        } else if code <= 0xffff {
            out.push_str(&format!("\\u{code:04x}"));
        } else {
            out.push_str(&format!("\\U{code:08x}"));
        }
    }
    out
}

fn push_hex_escape(out: &mut String, ch: char) {
    let code = ch as u32;
    if code <= 0xff {
        out.push_str(&format!("\\x{code:02x}"));
    } else if code <= 0xffff {
        out.push_str(&format!("\\u{code:04x}"));
    } else {
        out.push_str(&format!("\\U{code:08x}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codepoint_indexes_are_not_byte_indexes() {
        assert_eq!(codepoint_len("éβx"), 3);
        assert_eq!(byte_offset_for_codepoint("éβx", 2), Some(4));
        assert_eq!(find("éβx", "x"), 2);
    }

    #[test]
    fn repr_keeps_printable_unicode_and_escapes_controls() {
        assert_eq!(repr("é\\n"), "'é\\\\n'");
        assert_eq!(ascii_repr("é"), "'\\xe9'");
    }
}
