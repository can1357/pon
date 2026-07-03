//! Native `unicodedata` (Track L4): the CPython 3.14 surface the vendored
//! stdlib consumes, over generated Unicode 16.0.0 tables (`tables.rs`,
//! derived from the HOST oracle by `scratch/gen_unicodedata_tables.py` —
//! zero new deps, the K2 in-tree-table precedent scaled up).
//!
//! Served: `normalize` (UAX #15 `NFC`/`NFD`/`NFKC`/`NFKD` — algorithmic
//! Hangul plus table-driven decomposition/canonical ordering/composition),
//! `category`,
//! `combining`, `east_asian_width`, `decimal`, `digit`, `numeric`, and
//! `unidata_version`.  The at-import consumer is
//! `test.support.os_helper:30-31` (`normalize('NFD', …)` under `is_apple`);
//! lazy consumers on the current walks: `traceback`/`_pyrepl`
//! (`east_asian_width`, `category`) and `urllib.parse`
//! (`normalize('NFKC', …)`).
//!
//! Deliberately unserved (absent attribute -> loud `AttributeError`, the
//! `sys.flags` idiom): `name`/`lookup` — their only stdlib consumer is
//! `re._parser`'s lazily-imported `\N{…}` escape and the ~30k-entry name
//! table would triple the payload; `ucd_3_2_0` (`encodings.idna`);
//! `is_normalized`, `bidirectional`, `mirrored`, `decomposition`.  Grow the
//! generator + this file when a walk consumes one.
//!
//! Divergence (documented): CPython's normalization quickcheck returns the
//! *argument itself* for already-normalized input; pon returns the argument
//! only for the ASCII fast path (ASCII is closed under all four forms) and
//! otherwise allocates a fresh equal str.  Every stdlib caller compares by
//! value, not identity.
//!
//! Error-message shapes follow the host oracle exactly: the one-argument
//! functions are `METH_O` (module-qualified arity errors,
//! `unicodedata.category() takes exactly one argument (2 given)`); the rest
//! are Argument-Clinic-shaped (`normalize expected 2 arguments, got 1`,
//! `decimal() argument 1 must be a unicode character, not int`).

mod tables;

use crate::abi;
use crate::intern::intern;
use crate::object::{PyObject, PyUnicode};

use super::builtins_mod::VARIADIC_ARITY;
use super::install_module;

use tables::{
    CATEGORY_INDEX, CATEGORY_NAMES, CATEGORY_STARTS, COMBINING_RANGES, COMPOSE_KEYS, COMPOSE_VALUES, DECIMAL_RANGES,
    DECOMP_POOL, DIGIT_RANGES, EAW_INDEX, EAW_NAMES, EAW_STARTS, NFD_CPS, NFD_SLICES, NFKD_CPS, NFKD_SLICES,
    NUMERIC_RANGES, UNIDATA_VERSION,
};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

// ---------------------------------------------------------------------------
// Small helpers (string_mod idioms, local so the module stays self-contained)

fn untag(object: *mut PyObject) -> *mut PyObject {
    crate::tag::untag_arg(object)
}

fn raise_type_error(message: &str) -> *mut PyObject {
    // SAFETY: Message bytes are a live UTF-8 slice for the duration of the call.
    unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn raise_value_error(message: &str) -> *mut PyObject {
    // SAFETY: Message bytes are a live UTF-8 slice for the duration of the call.
    unsafe { abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) }
}

fn str_object(text: &str) -> *mut PyObject {
    // SAFETY: `text` is a live UTF-8 slice; the runtime copies the bytes.
    unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn int_object(value: i64) -> *mut PyObject {
    untag(crate::types::int::from_i64(value))
}

/// Type name for the clinic-shaped converter errors; CPython's
/// `_PyArg_BadArgument` spells the None singleton `None`, not `NoneType`.
fn type_name(object: *mut PyObject) -> &'static str {
    if object.is_null() {
        return "NULL";
    }
    // SAFETY: Singleton accessor.
    if object == unsafe { abi::pon_none() } {
        return "None";
    }
    // SAFETY: `object` is a live untagged object with a type slot.
    let ty = unsafe { (*object).ob_type };
    // SAFETY: A non-null type pointer refers to a live PyType.
    if ty.is_null() { "<unknown>" } else { unsafe { (*ty).name() } }
}

/// Extracts the text of a `str` (or `str` subclass, whose layout embeds
/// `PyUnicode`) argument; `None` for non-strings.
unsafe fn text_argument(object: *mut PyObject) -> Option<String> {
    if object.is_null() {
        return None;
    }
    // SAFETY: `object` is a live untagged object with a type slot.
    let mut ty = unsafe { (*object).ob_type };
    while !ty.is_null() {
        // SAFETY: `ty` walks live tp_base links starting from a live type.
        if unsafe { (*ty).name() } == "str" {
            // SAFETY: A str (sub)type instance carries the PyUnicode layout.
            return unsafe { (*object.cast::<PyUnicode>()).as_str() }.map(ToOwned::to_owned);
        }
        // SAFETY: `ty` is a live type per the loop invariant above.
        ty = unsafe { (*ty).tp_base };
    }
    None
}

/// Borrows the argv slots as a slice; NULL argv reads as empty.
unsafe fn arg_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> &'a [*mut PyObject] {
    if argc == 0 || argv.is_null() {
        &[]
    } else {
        // SAFETY: The caller passed `argc` live argument slots.
        unsafe { std::slice::from_raw_parts(argv, argc) }
    }
}

// ---------------------------------------------------------------------------
// Argument shapes

/// The single one-codepoint str argument of the `METH_O` functions
/// (`category`/`combining`/`east_asian_width`): CPython's arity error is
/// module-qualified, the converter errors are clinic-shaped (colon before
/// "argument" only in the wrong-length message, like the oracle).
unsafe fn char_argument(argv: *mut *mut PyObject, argc: usize, name: &str) -> Result<char, *mut PyObject> {
    // SAFETY: The caller forwarded its own live argv/argc pair.
    let args = unsafe { arg_slice(argv, argc) };
    if args.len() != 1 {
        return Err(raise_type_error(&format!(
            "unicodedata.{name}() takes exactly one argument ({} given)",
            args.len()
        )));
    }
    // SAFETY: `args[0]` is a live argument slot from the call ABI.
    unsafe { char_of(args[0], name, "argument") }
}

/// `decimal`/`digit`/`numeric`: one required one-codepoint str plus an
/// optional default object, with clinic-shaped arity errors.
unsafe fn char_and_default(
    argv: *mut *mut PyObject,
    argc: usize,
    name: &str,
) -> Result<(char, Option<*mut PyObject>), *mut PyObject> {
    // SAFETY: The caller forwarded its own live argv/argc pair.
    let args = unsafe { arg_slice(argv, argc) };
    if args.is_empty() {
        return Err(raise_type_error(&format!("{name} expected at least 1 argument, got 0")));
    }
    if args.len() > 2 {
        return Err(raise_type_error(&format!("{name} expected at most 2 arguments, got {}", args.len())));
    }
    // SAFETY: `args[0]` is a live argument slot from the call ABI.
    let ch = unsafe { char_of(args[0], name, "argument 1") }?;
    Ok((ch, args.get(1).copied()))
}

/// Shared one-codepoint converter with the oracle's two message shapes.
unsafe fn char_of(object: *mut PyObject, name: &str, what: &str) -> Result<char, *mut PyObject> {
    let object = untag(object);
    if object.is_null() {
        // Boxing a tagged immediate failed; the error is already recorded.
        return Err(core::ptr::null_mut());
    }
    // SAFETY: `untag` normalized the pointer; `text_argument` type-checks.
    let Some(text) = (unsafe { text_argument(object) }) else {
        return Err(raise_type_error(&format!(
            "{name}() {what} must be a unicode character, not {}",
            type_name(object)
        )));
    };
    let mut chars = text.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        return Ok(ch);
    }
    Err(raise_type_error(&format!(
        "{name}(): {what} must be a unicode character, not a string of length {}",
        text.chars().count()
    )))
}

// ---------------------------------------------------------------------------
// Table lookups

/// Category name for `cp`: [`CATEGORY_STARTS`] covers the full range from
/// U+0000, so the preceding run always exists.
fn category_of(cp: u32) -> &'static str {
    let run = CATEGORY_STARTS.partition_point(|&start| start <= cp) - 1;
    CATEGORY_NAMES[CATEGORY_INDEX[run] as usize]
}

/// East-Asian-width name for `cp` (same full-coverage run scheme).
fn east_asian_width_of(cp: u32) -> &'static str {
    let run = EAW_STARTS.partition_point(|&start| start <= cp) - 1;
    EAW_NAMES[EAW_INDEX[run] as usize]
}

/// Canonical combining class; 0 for anything outside the sparse ranges.
fn combining_class(cp: u32) -> u8 {
    range_search(&COMBINING_RANGES, cp).map_or(0, |(_, ccc)| ccc)
}

/// Binary search over sorted inclusive `(start, end, payload)` ranges;
/// returns `(cp - start, payload)` on a hit.
fn range_search<T: Copy>(ranges: &[(u32, u32, T)], cp: u32) -> Option<(u32, T)> {
    let index = ranges
        .binary_search_by(|&(start, end, _)| {
            if end < cp {
                core::cmp::Ordering::Less
            } else if start > cp {
                core::cmp::Ordering::Greater
            } else {
                core::cmp::Ordering::Equal
            }
        })
        .ok()?;
    let (start, _, payload) = ranges[index];
    Some((cp - start, payload))
}

/// Decimal/digit table value: run value increments by 1 per codepoint.
fn incrementing_value(ranges: &[(u32, u32, u8)], cp: u32) -> Option<i64> {
    range_search(ranges, cp).map(|(offset, first)| i64::from(first) + i64::from(offset))
}

/// Numeric table value: f64 bits of the run start, +1.0 per codepoint
/// (fractional values are singleton runs by construction).
fn numeric_value(cp: u32) -> Option<f64> {
    range_search(&NUMERIC_RANGES, cp).map(|(offset, bits)| f64::from_bits(bits) + f64::from(offset))
}

/// Full (host-pre-expanded) decomposition of `cp`, if any.
fn decomposition_slice(cps: &[u32], slices: &[u32], cp: u32) -> Option<&'static [u32]> {
    let index = cps.binary_search(&cp).ok()?;
    let packed = slices[index] as usize;
    Some(&DECOMP_POOL[packed >> 5..(packed >> 5) + (packed & 31)])
}

// ---------------------------------------------------------------------------
// UAX #15 normalization

// Hangul syllable constants (UAX #15 §3.12).
const S_BASE: u32 = 0xAC00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT;
const S_COUNT: u32 = L_COUNT * N_COUNT;

/// Decomposition + canonical ordering.  Table slices are FULL expansions
/// (recursion pre-applied by the generator), so a single pass suffices.
fn decompose(text: &str, compat: bool) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::with_capacity(text.len());
    for ch in text.chars() {
        let cp = ch as u32;
        if (S_BASE..S_BASE + S_COUNT).contains(&cp) {
            let s = cp - S_BASE;
            out.push(L_BASE + s / N_COUNT);
            out.push(V_BASE + (s % N_COUNT) / T_COUNT);
            if !s.is_multiple_of(T_COUNT) {
                out.push(T_BASE + s % T_COUNT);
            }
            continue;
        }
        let slice = if compat {
            decomposition_slice(&NFKD_CPS, &NFKD_SLICES, cp)
        } else {
            decomposition_slice(&NFD_CPS, &NFD_SLICES, cp)
        };
        match slice {
            Some(slice) => out.extend_from_slice(slice),
            None => out.push(cp),
        }
    }
    canonical_order(&mut out);
    out
}

/// Canonical ordering (UAX #15 D109): stable insertion sort of nonzero-ccc
/// runs — swap adjacent pairs while ccc(left) > ccc(right) > 0.
fn canonical_order(buf: &mut [u32]) {
    for i in 1..buf.len() {
        let ccc = combining_class(buf[i]);
        if ccc == 0 {
            continue;
        }
        let mut j = i;
        while j > 0 && combining_class(buf[j - 1]) > ccc {
            buf.swap(j - 1, j);
            j -= 1;
        }
    }
}

/// Primary composite of a pair: algorithmic Hangul (L+V, LV+T), then the
/// generated pair table (`Full_Composition_Exclusion` already applied).
fn compose_pair(a: u32, b: u32) -> Option<u32> {
    if (L_BASE..L_BASE + L_COUNT).contains(&a) && (V_BASE..V_BASE + V_COUNT).contains(&b) {
        return Some(S_BASE + ((a - L_BASE) * V_COUNT + (b - V_BASE)) * T_COUNT);
    }
    if (S_BASE..S_BASE + S_COUNT).contains(&a)
        && (a - S_BASE).is_multiple_of(T_COUNT)
        && (T_BASE + 1..T_BASE + T_COUNT).contains(&b)
    {
        return Some(a + (b - T_BASE));
    }
    let key = (u64::from(a) << 32) | u64::from(b);
    COMPOSE_KEYS.binary_search(&key).ok().map(|index| COMPOSE_VALUES[index])
}

/// Canonical composition (UAX #15 D117) over a canonically-ordered buffer.
fn compose_buffer(buf: &[u32]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::with_capacity(buf.len());
    let mut starter: Option<usize> = None;
    let mut last_ccc = 0u8;
    for &cp in buf {
        let ccc = combining_class(cp);
        if let Some(si) = starter
            && (out.len() == si + 1 || last_ccc < ccc)
            && let Some(composed) = compose_pair(out[si], cp)
        {
            out[si] = composed;
            continue;
        }
        if ccc == 0 {
            starter = Some(out.len());
            last_ccc = 0;
        } else {
            last_ccc = ccc;
        }
        out.push(cp);
    }
    out
}

// ---------------------------------------------------------------------------
// Entry points

/// `unicodedata.normalize(form, unistr)`.
unsafe extern "C" fn normalize_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: The runtime call ABI passed `argc` live argument slots.
    let args = unsafe { arg_slice(argv, argc) };
    if args.len() != 2 {
        return raise_type_error(&format!("normalize expected 2 arguments, got {}", args.len()));
    }
    let form_obj = untag(args[0]);
    // SAFETY: `untag` normalized the pointer; `text_argument` type-checks.
    let Some(form) = (unsafe { text_argument(form_obj) }) else {
        return raise_type_error(&format!("normalize() argument 1 must be str, not {}", type_name(form_obj)));
    };
    let text_obj = untag(args[1]);
    // SAFETY: `untag` normalized the pointer; `text_argument` type-checks.
    let Some(text) = (unsafe { text_argument(text_obj) }) else {
        return raise_type_error(&format!("normalize() argument 2 must be str, not {}", type_name(text_obj)));
    };
    let compat = match form.as_str() {
        "NFC" | "NFD" => false,
        "NFKC" | "NFKD" => true,
        _ => return raise_value_error("invalid normalization form"),
    };
    if text.is_ascii() {
        // ASCII is closed under all four forms; return the argument itself
        // (matching CPython's quickcheck fast path for this subset).
        return text_obj;
    }
    let mut buf = decompose(&text, compat);
    if matches!(form.as_str(), "NFC" | "NFKC") {
        buf = compose_buffer(&buf);
    }
    let mut result = String::with_capacity(text.len());
    for cp in buf {
        match char::from_u32(cp) {
            Some(ch) => result.push(ch),
            // Unreachable with intact tables: inputs come from a &str and
            // every table codepoint is a valid scalar value.
            None => return raise_value_error("unicodedata: generated table produced an invalid code point"),
        }
    }
    str_object(&result)
}

/// `unicodedata.category(chr)`.
unsafe extern "C" fn category_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: The runtime call ABI passed `argc` live argument slots.
    match unsafe { char_argument(argv, argc, "category") } {
        Ok(ch) => str_object(category_of(ch as u32)),
        Err(error) => error,
    }
}

/// `unicodedata.combining(chr)`: canonical combining class, 0 by default.
unsafe extern "C" fn combining_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: The runtime call ABI passed `argc` live argument slots.
    match unsafe { char_argument(argv, argc, "combining") } {
        Ok(ch) => int_object(i64::from(combining_class(ch as u32))),
        Err(error) => error,
    }
}

/// `unicodedata.east_asian_width(chr)`.
unsafe extern "C" fn east_asian_width_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: The runtime call ABI passed `argc` live argument slots.
    match unsafe { char_argument(argv, argc, "east_asian_width") } {
        Ok(ch) => str_object(east_asian_width_of(ch as u32)),
        Err(error) => error,
    }
}

/// `unicodedata.decimal(chr[, default])`.
unsafe extern "C" fn decimal_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: The runtime call ABI passed `argc` live argument slots.
    let (ch, default) = match unsafe { char_and_default(argv, argc, "decimal") } {
        Ok(parsed) => parsed,
        Err(error) => return error,
    };
    match incrementing_value(&DECIMAL_RANGES, ch as u32) {
        Some(value) => int_object(value),
        None => match default {
            Some(default) => untag(default),
            None => raise_value_error("not a decimal"),
        },
    }
}

/// `unicodedata.digit(chr[, default])`.
unsafe extern "C" fn digit_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: The runtime call ABI passed `argc` live argument slots.
    let (ch, default) = match unsafe { char_and_default(argv, argc, "digit") } {
        Ok(parsed) => parsed,
        Err(error) => return error,
    };
    match incrementing_value(&DIGIT_RANGES, ch as u32) {
        Some(value) => int_object(value),
        None => match default {
            Some(default) => untag(default),
            None => raise_value_error("not a digit"),
        },
    }
}

/// `unicodedata.numeric(chr[, default])`.
unsafe extern "C" fn numeric_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: The runtime call ABI passed `argc` live argument slots.
    let (ch, default) = match unsafe { char_and_default(argv, argc, "numeric") } {
        Ok(parsed) => parsed,
        Err(error) => return error,
    };
    match numeric_value(ch as u32) {
        Some(value) => crate::types::float::from_f64(value),
        None => match default {
            Some(default) => untag(default),
            None => raise_value_error("not a numeric character"),
        },
    }
}

// ---------------------------------------------------------------------------
// Module factory

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let mut attrs = Vec::new();
    for (name, value) in [
        ("__name__", "unicodedata"),
        ("__doc__", "pon: generated Unicode tables over the CPython unicodedata surface"),
        ("unidata_version", UNIDATA_VERSION),
    ] {
        let object = str_object(value);
        if object.is_null() {
            return Err(format!("failed to allocate unicodedata.{name}"));
        }
        attrs.push((intern(name), object));
    }
    for (name, entry) in [
        ("category", category_entry as BuiltinFn),
        ("combining", combining_entry),
        ("decimal", decimal_entry),
        ("digit", digit_entry),
        ("east_asian_width", east_asian_width_entry),
        ("normalize", normalize_entry),
        ("numeric", numeric_entry),
    ] {
        // SAFETY: `entry` is a live builtin entry point.
        let function = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
        if function.is_null() {
            return Err(format!("failed to allocate unicodedata.{name}"));
        }
        attrs.push((intern(name), function));
    }
    install_module("unicodedata", attrs)
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Table sanity: the run tables cover from U+0000 and the packed
    /// decomposition slices stay inside the pool.
    #[test]
    fn tables_cover_and_pack() {
        assert_eq!(CATEGORY_STARTS[0], 0);
        assert_eq!(EAW_STARTS[0], 0);
        for &packed in NFD_SLICES.iter().chain(NFKD_SLICES.iter()) {
            let packed = packed as usize;
            assert!((packed >> 5) + (packed & 31) <= DECOMP_POOL.len());
            assert!(packed & 31 > 0);
        }
    }

    /// Spot values pinned against the host oracle (python3.14, UCD 16.0.0).
    #[test]
    fn lookups_match_oracle_spots() {
        assert_eq!(category_of(u32::from('A')), "Lu");
        assert_eq!(category_of(0x0301), "Mn");
        assert_eq!(category_of(0x110000 - 1), "Cn");
        assert_eq!(combining_class(0x0301), 230);
        assert_eq!(combining_class(u32::from('a')), 0);
        assert_eq!(east_asian_width_of(u32::from('\u{4E00}')), "W");
        assert_eq!(east_asian_width_of(u32::from('a')), "Na");
        assert_eq!(incrementing_value(&DECIMAL_RANGES, u32::from('7')), Some(7));
        assert_eq!(incrementing_value(&DECIMAL_RANGES, 0x0665), Some(5));
        assert_eq!(incrementing_value(&DECIMAL_RANGES, u32::from('a')), None);
        assert_eq!(incrementing_value(&DIGIT_RANGES, 0x2460), Some(1));
        assert_eq!(numeric_value(0x00BD), Some(0.5));
        assert_eq!(numeric_value(0x2460), Some(1.0));
    }

    fn normalize(form: &str, text: &str) -> String {
        let compat = matches!(form, "NFKC" | "NFKD");
        let mut buf = decompose(text, compat);
        if matches!(form, "NFC" | "NFKC") {
            buf = compose_buffer(&buf);
        }
        buf.into_iter().map(|cp| char::from_u32(cp).unwrap()).collect()
    }

    /// UAX #15 pins: Latin decomposition/recomposition, compat folding,
    /// Hangul round-trip, combining-mark reordering, exclusions.
    #[test]
    fn normalize_matches_oracle_spots() {
        assert_eq!(normalize("NFD", "\u{00E0}"), "a\u{0300}");
        assert_eq!(normalize("NFC", "a\u{0300}"), "\u{00E0}");
        assert_eq!(normalize("NFD", "\u{1E69}"), "s\u{0323}\u{0307}");
        assert_eq!(normalize("NFC", "s\u{0307}\u{0323}"), "\u{1E69}");
        assert_eq!(normalize("NFKD", "\u{FB01}"), "fi");
        assert_eq!(normalize("NFC", "\u{FB01}"), "\u{FB01}");
        assert_eq!(normalize("NFD", "\u{AC01}"), "\u{1100}\u{1161}\u{11A8}");
        assert_eq!(normalize("NFC", "\u{1100}\u{1161}\u{11A8}"), "\u{AC01}");
        // Composition exclusion: U+212B decomposes to Å and never recomposes
        // to itself.
        assert_eq!(normalize("NFC", "\u{212B}"), "\u{00C5}");
        // os_helper:31 shape.
        assert_eq!(normalize("NFD", "-\u{E0}\u{F2}\u{258}\u{141}\u{11F}"), "-a\u{300}o\u{300}\u{258}\u{141}g\u{306}");
    }
}
