//! Native `unicodedata` (Track L4): the CPython 3.14 surface the vendored
//! stdlib consumes, over generated Unicode 16.0.0 tables (`tables.rs`,
//! derived from the HOST oracle by `scratch/gen_unicodedata_tables.py` —
//! zero new deps, the K2 in-tree-table precedent scaled up).
//!
//! Served: module-level `normalize`/`is_normalized` (UAX #15
//! `NFC`/`NFD`/`NFKC`/`NFKD` — algorithmic Hangul plus table-driven
//! decomposition/canonical ordering/composition), `category`, `bidirectional`,
//! `combining`, `east_asian_width`, `decomposition`, `decimal`, `digit`,
//! `numeric`, `mirrored`, `name`, `lookup`, and `unidata_version`; plus the
//! `UCD` type and `ucd_3_2_0` object for IDNA/stringprep.  Name lookup covers
//! generated Unicode names, formal name aliases, and named character sequences
//! from the Unicode 16.0.0 UCD files.
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

use core::ptr;
use std::sync::LazyLock;

use crate::abi;
use crate::intern::intern;
use crate::object::{PyObject, PyObjectHeader, PyType, PyUnicode};

use crate::types::exc::ExceptionKind;
use super::builtins_mod::VARIADIC_ARITY;
use super::install_module;

use tables::{
    BIDI_NAMES, BIDI_RANGES, CATEGORY_INDEX, CATEGORY_NAMES, CATEGORY_STARTS, COMBINING_RANGES, COMPOSE_KEYS,
    COMPOSE_VALUES, DECIMAL_RANGES, DECOMP_POOL, DECOMP_TEXT_CPS, DECOMP_TEXT_POOL, DECOMP_TEXT_SLICES, DIGIT_RANGES,
    EAW_INDEX, EAW_NAMES, EAW_STARTS, MIRRORED_RANGES, NAME_ALIAS_CPS, NAME_ALIAS_POOL, NAME_ALIAS_SLICES, NAME_CPS,
    NAME_POOL, NAME_SLICES, NAMED_SEQUENCE_DATA_POOL, NAMED_SEQUENCE_DATA_SLICES, NAMED_SEQUENCE_NAME_POOL,
    NAMED_SEQUENCE_NAME_SLICES, NFD_CPS, NFD_SLICES, NFKD_CPS, NFKD_SLICES, NUMERIC_RANGES, UNIDATA_VERSION,
};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

#[repr(C)]
struct PyUcd {
    ob_base: PyObjectHeader,
    version: UcdVersion,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum UcdVersion {
    Ucd3_2,
}


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

fn raise_kind(kind: ExceptionKind, message: &str) -> *mut PyObject {
    abi::exc::raise_kind_error_text(kind, message)
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

/// Bidirectional class for `cp`; unassigned/default codepoints use CPython's
/// empty-string result.
fn bidirectional_of(cp: u32) -> &'static str {
    range_search(&BIDI_RANGES, cp).map_or("", |(_, index)| BIDI_NAMES[index as usize])
}

/// Unicode `Bidi_Mirrored` property as CPython's integer 0/1 result.
fn mirrored_of(cp: u32) -> bool {
    pair_range_contains(&MIRRORED_RANGES, cp)
}

fn packed_str(pool: &'static str, slices: &[u32], index: usize) -> &'static str {
    let packed = slices[index] as usize;
    let start = packed >> 8;
    let end = start + (packed & 0xFF);
    &pool[start..end]
}

fn sparse_text(cps: &[u32], slices: &[u32], pool: &'static str, cp: u32) -> Option<&'static str> {
    let index = cps.binary_search(&cp).ok()?;
    Some(packed_str(pool, slices, index))
}

fn name_of(cp: u32) -> Option<&'static str> {
    sparse_text(&NAME_CPS, &NAME_SLICES, NAME_POOL, cp)
}

fn raw_decomposition_of(cp: u32) -> &'static str {
    sparse_text(&DECOMP_TEXT_CPS, &DECOMP_TEXT_SLICES, DECOMP_TEXT_POOL, cp).unwrap_or("")
}

fn lookup_unicode_name(name: &str) -> Option<String> {
    for (index, &cp) in NAME_CPS.iter().enumerate() {
        if packed_str(NAME_POOL, &NAME_SLICES, index) == name {
            return char::from_u32(cp).map(|ch| ch.to_string());
        }
    }
    for (index, &cp) in NAME_ALIAS_CPS.iter().enumerate() {
        if packed_str(NAME_ALIAS_POOL, &NAME_ALIAS_SLICES, index) == name {
            return char::from_u32(cp).map(|ch| ch.to_string());
        }
    }
    for (index, &packed) in NAMED_SEQUENCE_DATA_SLICES.iter().enumerate() {
        if packed_str(NAMED_SEQUENCE_NAME_POOL, &NAMED_SEQUENCE_NAME_SLICES, index) == name {
            let packed = packed as usize;
            let start = packed >> 8;
            let end = start + (packed & 0xFF);
            let mut out = String::new();
            for cp in &NAMED_SEQUENCE_DATA_POOL[start..end] {
                out.push(char::from_u32(*cp)?);
            }
            return Some(out);
        }
    }
    None
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

fn pair_range_contains(ranges: &[(u32, u32)], cp: u32) -> bool {
    ranges
        .binary_search_by(|&(start, end)| {
            if end < cp {
                core::cmp::Ordering::Less
            } else if start > cp {
                core::cmp::Ordering::Greater
            } else {
                core::cmp::Ordering::Equal
            }
        })
        .is_ok()
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
// Unicode 3.2 view for IDNA/stringprep

const UCD_3_2_VERSION: &str = "3.2.0";

/// True when `cp` had an assigned character in Unicode 3.2.0.
fn ucd_3_2_assigned(cp: u32) -> bool {
    UCD_3_2_ASSIGNED_RANGES
        .binary_search_by(|&(start, end)| {
            if end < cp {
                core::cmp::Ordering::Less
            } else if start > cp {
                core::cmp::Ordering::Greater
            } else {
                core::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// Unicode 3.2.0 category used by `stringprep.in_table_a1`.
fn ucd_3_2_category_of(cp: u32) -> &'static str {
    if ucd_3_2_assigned(cp) {
        category_of(cp)
    } else {
        "Cn"
    }
}

/// Unicode 3.2.0 bidirectional class used by nameprep's RandALCat/LCat checks.
fn ucd_3_2_bidirectional_of(cp: u32) -> &'static str {
    range_search(&UCD_3_2_BIDI_RANGES, cp).map_or("", |(_, index)| UCD_3_2_BIDI_NAMES[index as usize])
}

/// Current-table decomposition text for the 3.2.0 object.
fn decomposition_text(cp: u32) -> String {
    let Some(slice) = decomposition_slice(&NFKD_CPS, &NFKD_SLICES, cp)
        .or_else(|| decomposition_slice(&NFD_CPS, &NFD_SLICES, cp))
    else {
        return String::new();
    };
    let mut result = String::new();
    for (index, cp) in slice.iter().copied().enumerate() {
        if index != 0 {
            result.push(' ');
        }
        use core::fmt::Write as _;
        let _ = write!(&mut result, "{cp:04X}");
    }
    result
}

// The in-tree generator emits only current Unicode tables.  CPython's 3.2.0
// view is represented here by compact assignment and bidirectional tables from
// the CPython 3.14 oracle; assigned-codepoint category plus decomposition and
// normalization delegate to the current tables, so changed post-3.2 values can
// differ while IDNA/stringprep's exercised 3.2 checks remain available.
static UCD_3_2_ASSIGNED_RANGES: [(u32, u32); 386] = [
    (0x0, 0x220), (0x222, 0x233), (0x250, 0x2AD), (0x2B0, 0x2EE),
    (0x300, 0x34F), (0x360, 0x36F), (0x374, 0x375), (0x37A, 0x37A),
    (0x37E, 0x37E), (0x384, 0x38A), (0x38C, 0x38C), (0x38E, 0x3A1),
    (0x3A3, 0x3CE), (0x3D0, 0x3F6), (0x400, 0x486), (0x488, 0x4CE),
    (0x4D0, 0x4F5), (0x4F8, 0x4F9), (0x500, 0x50F), (0x531, 0x556),
    (0x559, 0x55F), (0x561, 0x587), (0x589, 0x58A), (0x591, 0x5A1),
    (0x5A3, 0x5B9), (0x5BB, 0x5C4), (0x5D0, 0x5EA), (0x5F0, 0x5F4),
    (0x60C, 0x60C), (0x61B, 0x61B), (0x61F, 0x61F), (0x621, 0x63A),
    (0x640, 0x655), (0x660, 0x6ED), (0x6F0, 0x6FE), (0x700, 0x70D),
    (0x70F, 0x72C), (0x730, 0x74A), (0x780, 0x7B1), (0x901, 0x903),
    (0x905, 0x939), (0x93C, 0x94D), (0x950, 0x954), (0x958, 0x970),
    (0x981, 0x983), (0x985, 0x98C), (0x98F, 0x990), (0x993, 0x9A8),
    (0x9AA, 0x9B0), (0x9B2, 0x9B2), (0x9B6, 0x9B9), (0x9BC, 0x9BC),
    (0x9BE, 0x9C4), (0x9C7, 0x9C8), (0x9CB, 0x9CD), (0x9D7, 0x9D7),
    (0x9DC, 0x9DD), (0x9DF, 0x9E3), (0x9E6, 0x9FA), (0xA02, 0xA02),
    (0xA05, 0xA0A), (0xA0F, 0xA10), (0xA13, 0xA28), (0xA2A, 0xA30),
    (0xA32, 0xA33), (0xA35, 0xA36), (0xA38, 0xA39), (0xA3C, 0xA3C),
    (0xA3E, 0xA42), (0xA47, 0xA48), (0xA4B, 0xA4D), (0xA59, 0xA5C),
    (0xA5E, 0xA5E), (0xA66, 0xA74), (0xA81, 0xA83), (0xA85, 0xA8B),
    (0xA8D, 0xA8D), (0xA8F, 0xA91), (0xA93, 0xAA8), (0xAAA, 0xAB0),
    (0xAB2, 0xAB3), (0xAB5, 0xAB9), (0xABC, 0xAC5), (0xAC7, 0xAC9),
    (0xACB, 0xACD), (0xAD0, 0xAD0), (0xAE0, 0xAE0), (0xAE6, 0xAEF),
    (0xB01, 0xB03), (0xB05, 0xB0C), (0xB0F, 0xB10), (0xB13, 0xB28),
    (0xB2A, 0xB30), (0xB32, 0xB33), (0xB36, 0xB39), (0xB3C, 0xB43),
    (0xB47, 0xB48), (0xB4B, 0xB4D), (0xB56, 0xB57), (0xB5C, 0xB5D),
    (0xB5F, 0xB61), (0xB66, 0xB70), (0xB82, 0xB83), (0xB85, 0xB8A),
    (0xB8E, 0xB90), (0xB92, 0xB95), (0xB99, 0xB9A), (0xB9C, 0xB9C),
    (0xB9E, 0xB9F), (0xBA3, 0xBA4), (0xBA8, 0xBAA), (0xBAE, 0xBB5),
    (0xBB7, 0xBB9), (0xBBE, 0xBC2), (0xBC6, 0xBC8), (0xBCA, 0xBCD),
    (0xBD7, 0xBD7), (0xBE7, 0xBF2), (0xC01, 0xC03), (0xC05, 0xC0C),
    (0xC0E, 0xC10), (0xC12, 0xC28), (0xC2A, 0xC33), (0xC35, 0xC39),
    (0xC3E, 0xC44), (0xC46, 0xC48), (0xC4A, 0xC4D), (0xC55, 0xC56),
    (0xC60, 0xC61), (0xC66, 0xC6F), (0xC82, 0xC83), (0xC85, 0xC8C),
    (0xC8E, 0xC90), (0xC92, 0xCA8), (0xCAA, 0xCB3), (0xCB5, 0xCB9),
    (0xCBE, 0xCC4), (0xCC6, 0xCC8), (0xCCA, 0xCCD), (0xCD5, 0xCD6),
    (0xCDE, 0xCDE), (0xCE0, 0xCE1), (0xCE6, 0xCEF), (0xD02, 0xD03),
    (0xD05, 0xD0C), (0xD0E, 0xD10), (0xD12, 0xD28), (0xD2A, 0xD39),
    (0xD3E, 0xD43), (0xD46, 0xD48), (0xD4A, 0xD4D), (0xD57, 0xD57),
    (0xD60, 0xD61), (0xD66, 0xD6F), (0xD82, 0xD83), (0xD85, 0xD96),
    (0xD9A, 0xDB1), (0xDB3, 0xDBB), (0xDBD, 0xDBD), (0xDC0, 0xDC6),
    (0xDCA, 0xDCA), (0xDCF, 0xDD4), (0xDD6, 0xDD6), (0xDD8, 0xDDF),
    (0xDF2, 0xDF4), (0xE01, 0xE3A), (0xE3F, 0xE5B), (0xE81, 0xE82),
    (0xE84, 0xE84), (0xE87, 0xE88), (0xE8A, 0xE8A), (0xE8D, 0xE8D),
    (0xE94, 0xE97), (0xE99, 0xE9F), (0xEA1, 0xEA3), (0xEA5, 0xEA5),
    (0xEA7, 0xEA7), (0xEAA, 0xEAB), (0xEAD, 0xEB9), (0xEBB, 0xEBD),
    (0xEC0, 0xEC4), (0xEC6, 0xEC6), (0xEC8, 0xECD), (0xED0, 0xED9),
    (0xEDC, 0xEDD), (0xF00, 0xF47), (0xF49, 0xF6A), (0xF71, 0xF8B),
    (0xF90, 0xF97), (0xF99, 0xFBC), (0xFBE, 0xFCC), (0xFCF, 0xFCF),
    (0x1000, 0x1021), (0x1023, 0x1027), (0x1029, 0x102A), (0x102C, 0x1032),
    (0x1036, 0x1039), (0x1040, 0x1059), (0x10A0, 0x10C5), (0x10D0, 0x10F8),
    (0x10FB, 0x10FB), (0x1100, 0x1159), (0x115F, 0x11A2), (0x11A8, 0x11F9),
    (0x1200, 0x1206), (0x1208, 0x1246), (0x1248, 0x1248), (0x124A, 0x124D),
    (0x1250, 0x1256), (0x1258, 0x1258), (0x125A, 0x125D), (0x1260, 0x1286),
    (0x1288, 0x1288), (0x128A, 0x128D), (0x1290, 0x12AE), (0x12B0, 0x12B0),
    (0x12B2, 0x12B5), (0x12B8, 0x12BE), (0x12C0, 0x12C0), (0x12C2, 0x12C5),
    (0x12C8, 0x12CE), (0x12D0, 0x12D6), (0x12D8, 0x12EE), (0x12F0, 0x130E),
    (0x1310, 0x1310), (0x1312, 0x1315), (0x1318, 0x131E), (0x1320, 0x1346),
    (0x1348, 0x135A), (0x1361, 0x137C), (0x13A0, 0x13F4), (0x1401, 0x1676),
    (0x1680, 0x169C), (0x16A0, 0x16F0), (0x1700, 0x170C), (0x170E, 0x1714),
    (0x1720, 0x1736), (0x1740, 0x1753), (0x1760, 0x176C), (0x176E, 0x1770),
    (0x1772, 0x1773), (0x1780, 0x17DC), (0x17E0, 0x17E9), (0x1800, 0x180E),
    (0x1810, 0x1819), (0x1820, 0x1877), (0x1880, 0x18A9), (0x1E00, 0x1E9B),
    (0x1EA0, 0x1EF9), (0x1F00, 0x1F15), (0x1F18, 0x1F1D), (0x1F20, 0x1F45),
    (0x1F48, 0x1F4D), (0x1F50, 0x1F57), (0x1F59, 0x1F59), (0x1F5B, 0x1F5B),
    (0x1F5D, 0x1F5D), (0x1F5F, 0x1F7D), (0x1F80, 0x1FB4), (0x1FB6, 0x1FC4),
    (0x1FC6, 0x1FD3), (0x1FD6, 0x1FDB), (0x1FDD, 0x1FEF), (0x1FF2, 0x1FF4),
    (0x1FF6, 0x1FFE), (0x2000, 0x2052), (0x2057, 0x2057), (0x205F, 0x2063),
    (0x206A, 0x2071), (0x2074, 0x208E), (0x20A0, 0x20B1), (0x20D0, 0x20EA),
    (0x2100, 0x213A), (0x213D, 0x214B), (0x2153, 0x2183), (0x2190, 0x23CE),
    (0x2400, 0x2426), (0x2440, 0x244A), (0x2460, 0x24FE), (0x2500, 0x2613),
    (0x2616, 0x2617), (0x2619, 0x267D), (0x2680, 0x2689), (0x2701, 0x2704),
    (0x2706, 0x2709), (0x270C, 0x2727), (0x2729, 0x274B), (0x274D, 0x274D),
    (0x274F, 0x2752), (0x2756, 0x2756), (0x2758, 0x275E), (0x2761, 0x2794),
    (0x2798, 0x27AF), (0x27B1, 0x27BE), (0x27D0, 0x27EB), (0x27F0, 0x2AFF),
    (0x2E80, 0x2E99), (0x2E9B, 0x2EF3), (0x2F00, 0x2FD5), (0x2FF0, 0x2FFB),
    (0x3000, 0x303F), (0x3041, 0x3096), (0x3099, 0x30FF), (0x3105, 0x312C),
    (0x3131, 0x318E), (0x3190, 0x31B7), (0x31F0, 0x321C), (0x3220, 0x3243),
    (0x3251, 0x327B), (0x327F, 0x32CB), (0x32D0, 0x32FE), (0x3300, 0x3376),
    (0x337B, 0x33DD), (0x33E0, 0x33FE), (0x3400, 0x4DB5), (0x4E00, 0x9FA5),
    (0xA000, 0xA48C), (0xA490, 0xA4C6), (0xAC00, 0xD7A3), (0xE000, 0xFA2D),
    (0xFA30, 0xFA6A), (0xFB00, 0xFB06), (0xFB13, 0xFB17), (0xFB1D, 0xFB36),
    (0xFB38, 0xFB3C), (0xFB3E, 0xFB3E), (0xFB40, 0xFB41), (0xFB43, 0xFB44),
    (0xFB46, 0xFBB1), (0xFBD3, 0xFD3F), (0xFD50, 0xFD8F), (0xFD92, 0xFDC7),
    (0xFDF0, 0xFDFC), (0xFE00, 0xFE0F), (0xFE20, 0xFE23), (0xFE30, 0xFE46),
    (0xFE49, 0xFE52), (0xFE54, 0xFE66), (0xFE68, 0xFE6B), (0xFE70, 0xFE74),
    (0xFE76, 0xFEFC), (0xFEFF, 0xFEFF), (0xFF01, 0xFFBE), (0xFFC2, 0xFFC7),
    (0xFFCA, 0xFFCF), (0xFFD2, 0xFFD7), (0xFFDA, 0xFFDC), (0xFFE0, 0xFFE6),
    (0xFFE8, 0xFFEE), (0xFFF9, 0xFFFD), (0x10300, 0x1031E), (0x10320, 0x10323),
    (0x10330, 0x1034A), (0x10400, 0x10425), (0x10428, 0x1044D), (0x1D000, 0x1D0F5),
    (0x1D100, 0x1D126), (0x1D12A, 0x1D1DD), (0x1D400, 0x1D454), (0x1D456, 0x1D49C),
    (0x1D49E, 0x1D49F), (0x1D4A2, 0x1D4A2), (0x1D4A5, 0x1D4A6), (0x1D4A9, 0x1D4AC),
    (0x1D4AE, 0x1D4B9), (0x1D4BB, 0x1D4BB), (0x1D4BD, 0x1D4C0), (0x1D4C2, 0x1D4C3),
    (0x1D4C5, 0x1D505), (0x1D507, 0x1D50A), (0x1D50D, 0x1D514), (0x1D516, 0x1D51C),
    (0x1D51E, 0x1D539), (0x1D53B, 0x1D53E), (0x1D540, 0x1D544), (0x1D546, 0x1D546),
    (0x1D54A, 0x1D550), (0x1D552, 0x1D6A3), (0x1D6A8, 0x1D7C9), (0x1D7CE, 0x1D7FF),
    (0x20000, 0x2A6D6), (0x2F800, 0x2FA1D), (0xE0001, 0xE0001), (0xE0020, 0xE007F),
    (0xF0000, 0xFFFFD), (0x100000, 0x10FFFD),
];

static UCD_3_2_BIDI_NAMES: [&str; 19] = ["AL", "AN", "B", "BN", "CS", "EN", "ES", "ET", "L", "LRE", "LRO", "NSM", "ON", "PDF", "R", "RLE", "RLO", "S", "WS"];

static UCD_3_2_BIDI_RANGES: [(u32, u32, u8); 692] = [
    (0x0, 0x8, 3), (0x9, 0x9, 17), (0xA, 0xA, 2), (0xB, 0xB, 17),
    (0xC, 0xC, 18), (0xD, 0xD, 2), (0xE, 0x1B, 3), (0x1C, 0x1E, 2),
    (0x1F, 0x1F, 17), (0x20, 0x20, 18), (0x21, 0x22, 12), (0x23, 0x25, 7),
    (0x26, 0x2A, 12), (0x2B, 0x2B, 7), (0x2C, 0x2C, 4), (0x2D, 0x2D, 7),
    (0x2E, 0x2E, 4), (0x2F, 0x2F, 6), (0x30, 0x39, 5), (0x3A, 0x3A, 4),
    (0x3B, 0x40, 12), (0x41, 0x5A, 8), (0x5B, 0x60, 12), (0x61, 0x7A, 8),
    (0x7B, 0x7E, 12), (0x7F, 0x84, 3), (0x85, 0x85, 2), (0x86, 0x9F, 3),
    (0xA0, 0xA0, 4), (0xA1, 0xA1, 12), (0xA2, 0xA5, 7), (0xA6, 0xA9, 12),
    (0xAA, 0xAA, 8), (0xAB, 0xAF, 12), (0xB0, 0xB1, 7), (0xB2, 0xB3, 5),
    (0xB4, 0xB4, 12), (0xB5, 0xB5, 8), (0xB6, 0xB8, 12), (0xB9, 0xB9, 5),
    (0xBA, 0xBA, 8), (0xBB, 0xBF, 12), (0xC0, 0xD6, 8), (0xD7, 0xD7, 12),
    (0xD8, 0xF6, 8), (0xF7, 0xF7, 12), (0xF8, 0x220, 8), (0x222, 0x233, 8),
    (0x250, 0x2AD, 8), (0x2B0, 0x2B8, 8), (0x2B9, 0x2BA, 12), (0x2BB, 0x2C1, 8),
    (0x2C2, 0x2CF, 12), (0x2D0, 0x2D1, 8), (0x2D2, 0x2DF, 12), (0x2E0, 0x2E4, 8),
    (0x2E5, 0x2ED, 12), (0x2EE, 0x2EE, 8), (0x300, 0x34F, 11), (0x360, 0x36F, 11),
    (0x374, 0x375, 12), (0x37A, 0x37A, 8), (0x37E, 0x37E, 12), (0x384, 0x385, 12),
    (0x386, 0x386, 8), (0x387, 0x387, 12), (0x388, 0x38A, 8), (0x38C, 0x38C, 8),
    (0x38E, 0x3A1, 8), (0x3A3, 0x3CE, 8), (0x3D0, 0x3F5, 8), (0x3F6, 0x3F6, 12),
    (0x400, 0x482, 8), (0x483, 0x486, 11), (0x488, 0x489, 11), (0x48A, 0x4CE, 8),
    (0x4D0, 0x4F5, 8), (0x4F8, 0x4F9, 8), (0x500, 0x50F, 8), (0x531, 0x556, 8),
    (0x559, 0x55F, 8), (0x561, 0x587, 8), (0x589, 0x589, 8), (0x58A, 0x58A, 12),
    (0x591, 0x5A1, 11), (0x5A3, 0x5B9, 11), (0x5BB, 0x5BD, 11), (0x5BE, 0x5BE, 14),
    (0x5BF, 0x5BF, 11), (0x5C0, 0x5C0, 14), (0x5C1, 0x5C2, 11), (0x5C3, 0x5C3, 14),
    (0x5C4, 0x5C4, 11), (0x5D0, 0x5EA, 14), (0x5F0, 0x5F4, 14), (0x60C, 0x60C, 4),
    (0x61B, 0x61B, 0), (0x61F, 0x61F, 0), (0x621, 0x63A, 0), (0x640, 0x64A, 0),
    (0x64B, 0x655, 11), (0x660, 0x669, 1), (0x66A, 0x66A, 7), (0x66B, 0x66C, 1),
    (0x66D, 0x66F, 0), (0x670, 0x670, 11), (0x671, 0x6D5, 0), (0x6D6, 0x6DC, 11),
    (0x6DD, 0x6DD, 0), (0x6DE, 0x6E4, 11), (0x6E5, 0x6E6, 0), (0x6E7, 0x6E8, 11),
    (0x6E9, 0x6E9, 12), (0x6EA, 0x6ED, 11), (0x6F0, 0x6F9, 5), (0x6FA, 0x6FE, 0),
    (0x700, 0x70D, 0), (0x70F, 0x70F, 3), (0x710, 0x710, 0), (0x711, 0x711, 11),
    (0x712, 0x72C, 0), (0x730, 0x74A, 11), (0x780, 0x7A5, 0), (0x7A6, 0x7B0, 11),
    (0x7B1, 0x7B1, 0), (0x901, 0x902, 11), (0x903, 0x903, 8), (0x905, 0x939, 8),
    (0x93C, 0x93C, 11), (0x93D, 0x940, 8), (0x941, 0x948, 11), (0x949, 0x94C, 8),
    (0x94D, 0x94D, 11), (0x950, 0x950, 8), (0x951, 0x954, 11), (0x958, 0x961, 8),
    (0x962, 0x963, 11), (0x964, 0x970, 8), (0x981, 0x981, 11), (0x982, 0x983, 8),
    (0x985, 0x98C, 8), (0x98F, 0x990, 8), (0x993, 0x9A8, 8), (0x9AA, 0x9B0, 8),
    (0x9B2, 0x9B2, 8), (0x9B6, 0x9B9, 8), (0x9BC, 0x9BC, 11), (0x9BE, 0x9C0, 8),
    (0x9C1, 0x9C4, 11), (0x9C7, 0x9C8, 8), (0x9CB, 0x9CC, 8), (0x9CD, 0x9CD, 11),
    (0x9D7, 0x9D7, 8), (0x9DC, 0x9DD, 8), (0x9DF, 0x9E1, 8), (0x9E2, 0x9E3, 11),
    (0x9E6, 0x9F1, 8), (0x9F2, 0x9F3, 7), (0x9F4, 0x9FA, 8), (0xA02, 0xA02, 11),
    (0xA05, 0xA0A, 8), (0xA0F, 0xA10, 8), (0xA13, 0xA28, 8), (0xA2A, 0xA30, 8),
    (0xA32, 0xA33, 8), (0xA35, 0xA36, 8), (0xA38, 0xA39, 8), (0xA3C, 0xA3C, 11),
    (0xA3E, 0xA40, 8), (0xA41, 0xA42, 11), (0xA47, 0xA48, 11), (0xA4B, 0xA4D, 11),
    (0xA59, 0xA5C, 8), (0xA5E, 0xA5E, 8), (0xA66, 0xA6F, 8), (0xA70, 0xA71, 11),
    (0xA72, 0xA74, 8), (0xA81, 0xA82, 11), (0xA83, 0xA83, 8), (0xA85, 0xA8B, 8),
    (0xA8D, 0xA8D, 8), (0xA8F, 0xA91, 8), (0xA93, 0xAA8, 8), (0xAAA, 0xAB0, 8),
    (0xAB2, 0xAB3, 8), (0xAB5, 0xAB9, 8), (0xABC, 0xABC, 11), (0xABD, 0xAC0, 8),
    (0xAC1, 0xAC5, 11), (0xAC7, 0xAC8, 11), (0xAC9, 0xAC9, 8), (0xACB, 0xACC, 8),
    (0xACD, 0xACD, 11), (0xAD0, 0xAD0, 8), (0xAE0, 0xAE0, 8), (0xAE6, 0xAEF, 8),
    (0xB01, 0xB01, 11), (0xB02, 0xB03, 8), (0xB05, 0xB0C, 8), (0xB0F, 0xB10, 8),
    (0xB13, 0xB28, 8), (0xB2A, 0xB30, 8), (0xB32, 0xB33, 8), (0xB36, 0xB39, 8),
    (0xB3C, 0xB3C, 11), (0xB3D, 0xB3E, 8), (0xB3F, 0xB3F, 11), (0xB40, 0xB40, 8),
    (0xB41, 0xB43, 11), (0xB47, 0xB48, 8), (0xB4B, 0xB4C, 8), (0xB4D, 0xB4D, 11),
    (0xB56, 0xB56, 11), (0xB57, 0xB57, 8), (0xB5C, 0xB5D, 8), (0xB5F, 0xB61, 8),
    (0xB66, 0xB70, 8), (0xB82, 0xB82, 11), (0xB83, 0xB83, 8), (0xB85, 0xB8A, 8),
    (0xB8E, 0xB90, 8), (0xB92, 0xB95, 8), (0xB99, 0xB9A, 8), (0xB9C, 0xB9C, 8),
    (0xB9E, 0xB9F, 8), (0xBA3, 0xBA4, 8), (0xBA8, 0xBAA, 8), (0xBAE, 0xBB5, 8),
    (0xBB7, 0xBB9, 8), (0xBBE, 0xBBF, 8), (0xBC0, 0xBC0, 11), (0xBC1, 0xBC2, 8),
    (0xBC6, 0xBC8, 8), (0xBCA, 0xBCC, 8), (0xBCD, 0xBCD, 11), (0xBD7, 0xBD7, 8),
    (0xBE7, 0xBF2, 8), (0xC01, 0xC03, 8), (0xC05, 0xC0C, 8), (0xC0E, 0xC10, 8),
    (0xC12, 0xC28, 8), (0xC2A, 0xC33, 8), (0xC35, 0xC39, 8), (0xC3E, 0xC40, 11),
    (0xC41, 0xC44, 8), (0xC46, 0xC48, 11), (0xC4A, 0xC4D, 11), (0xC55, 0xC56, 11),
    (0xC60, 0xC61, 8), (0xC66, 0xC6F, 8), (0xC82, 0xC83, 8), (0xC85, 0xC8C, 8),
    (0xC8E, 0xC90, 8), (0xC92, 0xCA8, 8), (0xCAA, 0xCB3, 8), (0xCB5, 0xCB9, 8),
    (0xCBE, 0xCBE, 8), (0xCBF, 0xCBF, 11), (0xCC0, 0xCC4, 8), (0xCC6, 0xCC6, 11),
    (0xCC7, 0xCC8, 8), (0xCCA, 0xCCB, 8), (0xCCC, 0xCCD, 11), (0xCD5, 0xCD6, 8),
    (0xCDE, 0xCDE, 8), (0xCE0, 0xCE1, 8), (0xCE6, 0xCEF, 8), (0xD02, 0xD03, 8),
    (0xD05, 0xD0C, 8), (0xD0E, 0xD10, 8), (0xD12, 0xD28, 8), (0xD2A, 0xD39, 8),
    (0xD3E, 0xD40, 8), (0xD41, 0xD43, 11), (0xD46, 0xD48, 8), (0xD4A, 0xD4C, 8),
    (0xD4D, 0xD4D, 11), (0xD57, 0xD57, 8), (0xD60, 0xD61, 8), (0xD66, 0xD6F, 8),
    (0xD82, 0xD83, 8), (0xD85, 0xD96, 8), (0xD9A, 0xDB1, 8), (0xDB3, 0xDBB, 8),
    (0xDBD, 0xDBD, 8), (0xDC0, 0xDC6, 8), (0xDCA, 0xDCA, 11), (0xDCF, 0xDD1, 8),
    (0xDD2, 0xDD4, 11), (0xDD6, 0xDD6, 11), (0xDD8, 0xDDF, 8), (0xDF2, 0xDF4, 8),
    (0xE01, 0xE30, 8), (0xE31, 0xE31, 11), (0xE32, 0xE33, 8), (0xE34, 0xE3A, 11),
    (0xE3F, 0xE3F, 7), (0xE40, 0xE46, 8), (0xE47, 0xE4E, 11), (0xE4F, 0xE5B, 8),
    (0xE81, 0xE82, 8), (0xE84, 0xE84, 8), (0xE87, 0xE88, 8), (0xE8A, 0xE8A, 8),
    (0xE8D, 0xE8D, 8), (0xE94, 0xE97, 8), (0xE99, 0xE9F, 8), (0xEA1, 0xEA3, 8),
    (0xEA5, 0xEA5, 8), (0xEA7, 0xEA7, 8), (0xEAA, 0xEAB, 8), (0xEAD, 0xEB0, 8),
    (0xEB1, 0xEB1, 11), (0xEB2, 0xEB3, 8), (0xEB4, 0xEB9, 11), (0xEBB, 0xEBC, 11),
    (0xEBD, 0xEBD, 8), (0xEC0, 0xEC4, 8), (0xEC6, 0xEC6, 8), (0xEC8, 0xECD, 11),
    (0xED0, 0xED9, 8), (0xEDC, 0xEDD, 8), (0xF00, 0xF17, 8), (0xF18, 0xF19, 11),
    (0xF1A, 0xF34, 8), (0xF35, 0xF35, 11), (0xF36, 0xF36, 8), (0xF37, 0xF37, 11),
    (0xF38, 0xF38, 8), (0xF39, 0xF39, 11), (0xF3A, 0xF3D, 12), (0xF3E, 0xF47, 8),
    (0xF49, 0xF6A, 8), (0xF71, 0xF7E, 11), (0xF7F, 0xF7F, 8), (0xF80, 0xF84, 11),
    (0xF85, 0xF85, 8), (0xF86, 0xF87, 11), (0xF88, 0xF8B, 8), (0xF90, 0xF97, 11),
    (0xF99, 0xFBC, 11), (0xFBE, 0xFC5, 8), (0xFC6, 0xFC6, 11), (0xFC7, 0xFCC, 8),
    (0xFCF, 0xFCF, 8), (0x1000, 0x1021, 8), (0x1023, 0x1027, 8), (0x1029, 0x102A, 8),
    (0x102C, 0x102C, 8), (0x102D, 0x1030, 11), (0x1031, 0x1031, 8), (0x1032, 0x1032, 11),
    (0x1036, 0x1037, 11), (0x1038, 0x1038, 8), (0x1039, 0x1039, 11), (0x1040, 0x1057, 8),
    (0x1058, 0x1059, 11), (0x10A0, 0x10C5, 8), (0x10D0, 0x10F8, 8), (0x10FB, 0x10FB, 8),
    (0x1100, 0x1159, 8), (0x115F, 0x11A2, 8), (0x11A8, 0x11F9, 8), (0x1200, 0x1206, 8),
    (0x1208, 0x1246, 8), (0x1248, 0x1248, 8), (0x124A, 0x124D, 8), (0x1250, 0x1256, 8),
    (0x1258, 0x1258, 8), (0x125A, 0x125D, 8), (0x1260, 0x1286, 8), (0x1288, 0x1288, 8),
    (0x128A, 0x128D, 8), (0x1290, 0x12AE, 8), (0x12B0, 0x12B0, 8), (0x12B2, 0x12B5, 8),
    (0x12B8, 0x12BE, 8), (0x12C0, 0x12C0, 8), (0x12C2, 0x12C5, 8), (0x12C8, 0x12CE, 8),
    (0x12D0, 0x12D6, 8), (0x12D8, 0x12EE, 8), (0x12F0, 0x130E, 8), (0x1310, 0x1310, 8),
    (0x1312, 0x1315, 8), (0x1318, 0x131E, 8), (0x1320, 0x1346, 8), (0x1348, 0x135A, 8),
    (0x1361, 0x137C, 8), (0x13A0, 0x13F4, 8), (0x1401, 0x1676, 8), (0x1680, 0x1680, 18),
    (0x1681, 0x169A, 8), (0x169B, 0x169C, 12), (0x16A0, 0x16F0, 8), (0x1700, 0x170C, 8),
    (0x170E, 0x1711, 8), (0x1712, 0x1714, 11), (0x1720, 0x1731, 8), (0x1732, 0x1734, 11),
    (0x1735, 0x1736, 8), (0x1740, 0x1751, 8), (0x1752, 0x1753, 11), (0x1760, 0x176C, 8),
    (0x176E, 0x1770, 8), (0x1772, 0x1773, 11), (0x1780, 0x17B6, 8), (0x17B7, 0x17BD, 11),
    (0x17BE, 0x17C5, 8), (0x17C6, 0x17C6, 11), (0x17C7, 0x17C8, 8), (0x17C9, 0x17D3, 11),
    (0x17D4, 0x17DA, 8), (0x17DB, 0x17DB, 7), (0x17DC, 0x17DC, 8), (0x17E0, 0x17E9, 8),
    (0x1800, 0x180A, 12), (0x180B, 0x180D, 11), (0x180E, 0x180E, 3), (0x1810, 0x1819, 8),
    (0x1820, 0x1877, 8), (0x1880, 0x18A8, 8), (0x18A9, 0x18A9, 11), (0x1E00, 0x1E9B, 8),
    (0x1EA0, 0x1EF9, 8), (0x1F00, 0x1F15, 8), (0x1F18, 0x1F1D, 8), (0x1F20, 0x1F45, 8),
    (0x1F48, 0x1F4D, 8), (0x1F50, 0x1F57, 8), (0x1F59, 0x1F59, 8), (0x1F5B, 0x1F5B, 8),
    (0x1F5D, 0x1F5D, 8), (0x1F5F, 0x1F7D, 8), (0x1F80, 0x1FB4, 8), (0x1FB6, 0x1FBC, 8),
    (0x1FBD, 0x1FBD, 12), (0x1FBE, 0x1FBE, 8), (0x1FBF, 0x1FC1, 12), (0x1FC2, 0x1FC4, 8),
    (0x1FC6, 0x1FCC, 8), (0x1FCD, 0x1FCF, 12), (0x1FD0, 0x1FD3, 8), (0x1FD6, 0x1FDB, 8),
    (0x1FDD, 0x1FDF, 12), (0x1FE0, 0x1FEC, 8), (0x1FED, 0x1FEF, 12), (0x1FF2, 0x1FF4, 8),
    (0x1FF6, 0x1FFC, 8), (0x1FFD, 0x1FFE, 12), (0x2000, 0x200A, 18), (0x200B, 0x200D, 3),
    (0x200E, 0x200E, 8), (0x200F, 0x200F, 14), (0x2010, 0x2027, 12), (0x2028, 0x2028, 18),
    (0x2029, 0x2029, 2), (0x202A, 0x202A, 9), (0x202B, 0x202B, 15), (0x202C, 0x202C, 13),
    (0x202D, 0x202D, 10), (0x202E, 0x202E, 16), (0x202F, 0x202F, 18), (0x2030, 0x2034, 7),
    (0x2035, 0x2052, 12), (0x2057, 0x2057, 12), (0x205F, 0x205F, 18), (0x2060, 0x2063, 3),
    (0x206A, 0x206F, 3), (0x2070, 0x2070, 5), (0x2071, 0x2071, 8), (0x2074, 0x2079, 5),
    (0x207A, 0x207B, 7), (0x207C, 0x207E, 12), (0x207F, 0x207F, 8), (0x2080, 0x2089, 5),
    (0x208A, 0x208B, 7), (0x208C, 0x208E, 12), (0x20A0, 0x20B1, 7), (0x20D0, 0x20EA, 11),
    (0x2100, 0x2101, 12), (0x2102, 0x2102, 8), (0x2103, 0x2106, 12), (0x2107, 0x2107, 8),
    (0x2108, 0x2109, 12), (0x210A, 0x2113, 8), (0x2114, 0x2114, 12), (0x2115, 0x2115, 8),
    (0x2116, 0x2118, 12), (0x2119, 0x211D, 8), (0x211E, 0x2123, 12), (0x2124, 0x2124, 8),
    (0x2125, 0x2125, 12), (0x2126, 0x2126, 8), (0x2127, 0x2127, 12), (0x2128, 0x2128, 8),
    (0x2129, 0x2129, 12), (0x212A, 0x212D, 8), (0x212E, 0x212E, 7), (0x212F, 0x2131, 8),
    (0x2132, 0x2132, 12), (0x2133, 0x2139, 8), (0x213A, 0x213A, 12), (0x213D, 0x213F, 8),
    (0x2140, 0x2144, 12), (0x2145, 0x2149, 8), (0x214A, 0x214B, 12), (0x2153, 0x215F, 12),
    (0x2160, 0x2183, 8), (0x2190, 0x2211, 12), (0x2212, 0x2213, 7), (0x2214, 0x2335, 12),
    (0x2336, 0x237A, 8), (0x237B, 0x2394, 12), (0x2395, 0x2395, 8), (0x2396, 0x23CE, 12),
    (0x2400, 0x2426, 12), (0x2440, 0x244A, 12), (0x2460, 0x249B, 5), (0x249C, 0x24E9, 8),
    (0x24EA, 0x24EA, 5), (0x24EB, 0x24FE, 12), (0x2500, 0x2613, 12), (0x2616, 0x2617, 12),
    (0x2619, 0x267D, 12), (0x2680, 0x2689, 12), (0x2701, 0x2704, 12), (0x2706, 0x2709, 12),
    (0x270C, 0x2727, 12), (0x2729, 0x274B, 12), (0x274D, 0x274D, 12), (0x274F, 0x2752, 12),
    (0x2756, 0x2756, 12), (0x2758, 0x275E, 12), (0x2761, 0x2794, 12), (0x2798, 0x27AF, 12),
    (0x27B1, 0x27BE, 12), (0x27D0, 0x27EB, 12), (0x27F0, 0x2AFF, 12), (0x2E80, 0x2E99, 12),
    (0x2E9B, 0x2EF3, 12), (0x2F00, 0x2FD5, 12), (0x2FF0, 0x2FFB, 12), (0x3000, 0x3000, 18),
    (0x3001, 0x3004, 12), (0x3005, 0x3007, 8), (0x3008, 0x3020, 12), (0x3021, 0x3029, 8),
    (0x302A, 0x302F, 11), (0x3030, 0x3030, 12), (0x3031, 0x3035, 8), (0x3036, 0x3037, 12),
    (0x3038, 0x303C, 8), (0x303D, 0x303F, 12), (0x3041, 0x3096, 8), (0x3099, 0x309A, 11),
    (0x309B, 0x309C, 12), (0x309D, 0x309F, 8), (0x30A0, 0x30A0, 12), (0x30A1, 0x30FA, 8),
    (0x30FB, 0x30FB, 12), (0x30FC, 0x30FF, 8), (0x3105, 0x312C, 8), (0x3131, 0x318E, 8),
    (0x3190, 0x31B7, 8), (0x31F0, 0x321C, 8), (0x3220, 0x3243, 8), (0x3251, 0x325F, 12),
    (0x3260, 0x327B, 8), (0x327F, 0x32B0, 8), (0x32B1, 0x32BF, 12), (0x32C0, 0x32CB, 8),
    (0x32D0, 0x32FE, 8), (0x3300, 0x3376, 8), (0x337B, 0x33DD, 8), (0x33E0, 0x33FE, 8),
    (0x3400, 0x4DB5, 8), (0x4E00, 0x9FA5, 8), (0xA000, 0xA48C, 8), (0xA490, 0xA4C6, 12),
    (0xAC00, 0xD7A3, 8), (0xE000, 0xFA2D, 8), (0xFA30, 0xFA6A, 8), (0xFB00, 0xFB06, 8),
    (0xFB13, 0xFB17, 8), (0xFB1D, 0xFB1D, 14), (0xFB1E, 0xFB1E, 11), (0xFB1F, 0xFB28, 14),
    (0xFB29, 0xFB29, 7), (0xFB2A, 0xFB36, 14), (0xFB38, 0xFB3C, 14), (0xFB3E, 0xFB3E, 14),
    (0xFB40, 0xFB41, 14), (0xFB43, 0xFB44, 14), (0xFB46, 0xFB4F, 14), (0xFB50, 0xFBB1, 0),
    (0xFBD3, 0xFD3D, 0), (0xFD3E, 0xFD3F, 12), (0xFD50, 0xFD8F, 0), (0xFD92, 0xFDC7, 0),
    (0xFDF0, 0xFDFC, 0), (0xFE00, 0xFE0F, 11), (0xFE20, 0xFE23, 11), (0xFE30, 0xFE46, 12),
    (0xFE49, 0xFE4F, 12), (0xFE50, 0xFE50, 4), (0xFE51, 0xFE51, 12), (0xFE52, 0xFE52, 4),
    (0xFE54, 0xFE54, 12), (0xFE55, 0xFE55, 4), (0xFE56, 0xFE5E, 12), (0xFE5F, 0xFE5F, 7),
    (0xFE60, 0xFE61, 12), (0xFE62, 0xFE63, 7), (0xFE64, 0xFE66, 12), (0xFE68, 0xFE68, 12),
    (0xFE69, 0xFE6A, 7), (0xFE6B, 0xFE6B, 12), (0xFE70, 0xFE74, 0), (0xFE76, 0xFEFC, 0),
    (0xFEFF, 0xFEFF, 3), (0xFF01, 0xFF02, 12), (0xFF03, 0xFF05, 7), (0xFF06, 0xFF0A, 12),
    (0xFF0B, 0xFF0B, 7), (0xFF0C, 0xFF0C, 4), (0xFF0D, 0xFF0D, 7), (0xFF0E, 0xFF0E, 4),
    (0xFF0F, 0xFF0F, 6), (0xFF10, 0xFF19, 5), (0xFF1A, 0xFF1A, 4), (0xFF1B, 0xFF20, 12),
    (0xFF21, 0xFF3A, 8), (0xFF3B, 0xFF40, 12), (0xFF41, 0xFF5A, 8), (0xFF5B, 0xFF65, 12),
    (0xFF66, 0xFFBE, 8), (0xFFC2, 0xFFC7, 8), (0xFFCA, 0xFFCF, 8), (0xFFD2, 0xFFD7, 8),
    (0xFFDA, 0xFFDC, 8), (0xFFE0, 0xFFE1, 7), (0xFFE2, 0xFFE4, 12), (0xFFE5, 0xFFE6, 7),
    (0xFFE8, 0xFFEE, 12), (0xFFF9, 0xFFFB, 3), (0xFFFC, 0xFFFD, 12), (0x10300, 0x1031E, 8),
    (0x10320, 0x10323, 8), (0x10330, 0x1034A, 8), (0x10400, 0x10425, 8), (0x10428, 0x1044D, 8),
    (0x1D000, 0x1D0F5, 8), (0x1D100, 0x1D126, 8), (0x1D12A, 0x1D166, 8), (0x1D167, 0x1D169, 11),
    (0x1D16A, 0x1D172, 8), (0x1D173, 0x1D17A, 3), (0x1D17B, 0x1D182, 11), (0x1D183, 0x1D184, 8),
    (0x1D185, 0x1D18B, 11), (0x1D18C, 0x1D1A9, 8), (0x1D1AA, 0x1D1AD, 11), (0x1D1AE, 0x1D1DD, 8),
    (0x1D400, 0x1D454, 8), (0x1D456, 0x1D49C, 8), (0x1D49E, 0x1D49F, 8), (0x1D4A2, 0x1D4A2, 8),
    (0x1D4A5, 0x1D4A6, 8), (0x1D4A9, 0x1D4AC, 8), (0x1D4AE, 0x1D4B9, 8), (0x1D4BB, 0x1D4BB, 8),
    (0x1D4BD, 0x1D4C0, 8), (0x1D4C2, 0x1D4C3, 8), (0x1D4C5, 0x1D505, 8), (0x1D507, 0x1D50A, 8),
    (0x1D50D, 0x1D514, 8), (0x1D516, 0x1D51C, 8), (0x1D51E, 0x1D539, 8), (0x1D53B, 0x1D53E, 8),
    (0x1D540, 0x1D544, 8), (0x1D546, 0x1D546, 8), (0x1D54A, 0x1D550, 8), (0x1D552, 0x1D6A3, 8),
    (0x1D6A8, 0x1D7C9, 8), (0x1D7CE, 0x1D7FF, 5), (0x20000, 0x2A6D6, 8), (0x2F800, 0x2FA1D, 8),
    (0xE0001, 0xE0001, 3), (0xE0020, 0xE007F, 3), (0xF0000, 0xFFFFD, 8), (0x100000, 0x10FFFD, 8),
];

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

fn normalize_to_string(form: &str, text: &str) -> Result<String, *mut PyObject> {
    let compat = match form {
        "NFC" | "NFD" => false,
        "NFKC" | "NFKD" => true,
        _ => return Err(raise_value_error("invalid normalization form")),
    };
    if text.is_ascii() {
        return Ok(text.to_owned());
    }
    let mut buf = decompose(text, compat);
    if matches!(form, "NFC" | "NFKC") {
        buf = compose_buffer(&buf);
    }
    let mut result = String::with_capacity(text.len());
    for cp in buf {
        match char::from_u32(cp) {
            Some(ch) => result.push(ch),
            None => return Err(raise_value_error("unicodedata: generated table produced an invalid code point")),
        }
    }
    Ok(result)
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
    if !matches!(form.as_str(), "NFC" | "NFD" | "NFKC" | "NFKD") {
        return raise_value_error("invalid normalization form");
    }
    if text.is_ascii() {
        // ASCII is closed under all four forms; return the argument itself
        // (matching CPython's quickcheck fast path for this subset).
        return text_obj;
    }
    match normalize_to_string(&form, &text) {
        Ok(result) => str_object(&result),
        Err(raised) => raised,
    }
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

/// `unicodedata.bidirectional(chr)`.
unsafe extern "C" fn bidirectional_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    match unsafe { char_argument(argv, argc, "bidirectional") } {
        Ok(ch) => str_object(bidirectional_of(ch as u32)),
        Err(error) => error,
    }
}

/// `unicodedata.decomposition(chr)`.
unsafe extern "C" fn decomposition_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    match unsafe { char_argument(argv, argc, "decomposition") } {
        Ok(ch) => str_object(raw_decomposition_of(ch as u32)),
        Err(error) => error,
    }
}

/// `unicodedata.mirrored(chr)`.
unsafe extern "C" fn mirrored_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    match unsafe { char_argument(argv, argc, "mirrored") } {
        Ok(ch) => int_object(i64::from(mirrored_of(ch as u32))),
        Err(error) => error,
    }
}

/// `unicodedata.name(chr[, default])`.
unsafe extern "C" fn name_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (ch, default) = match unsafe { char_and_default(argv, argc, "name") } {
        Ok(parsed) => parsed,
        Err(error) => return error,
    };
    match name_of(ch as u32) {
        Some(name) => str_object(name),
        None => match default {
            Some(default) => untag(default),
            None => raise_value_error("no such name"),
        },
    }
}

/// `unicodedata.lookup(name)`.
unsafe extern "C" fn lookup_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = unsafe { arg_slice(argv, argc) };
    if args.len() != 1 {
        return raise_type_error(&format!("unicodedata.lookup() takes exactly one argument ({} given)", args.len()));
    }
    let name_obj = untag(args[0]);
    let Some(name) = (unsafe { text_argument(name_obj) }) else {
        return raise_type_error(&format!("lookup() argument must be str, not {}", type_name(name_obj)));
    };
    match lookup_unicode_name(&name) {
        Some(text) => str_object(&text),
        None => raise_kind(ExceptionKind::KeyError, &format!("undefined character name '{name}'")),
    }
}

/// `unicodedata.is_normalized(form, unistr)`.
unsafe extern "C" fn is_normalized_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = unsafe { arg_slice(argv, argc) };
    if args.len() != 2 {
        return raise_type_error(&format!("is_normalized expected 2 arguments, got {}", args.len()));
    }
    let form_obj = untag(args[0]);
    let Some(form) = (unsafe { text_argument(form_obj) }) else {
        return raise_type_error(&format!("is_normalized() argument 1 must be str, not {}", type_name(form_obj)));
    };
    let text_obj = untag(args[1]);
    let Some(text) = (unsafe { text_argument(text_obj) }) else {
        return raise_type_error(&format!("is_normalized() argument 2 must be str, not {}", type_name(text_obj)));
    };
    match normalize_to_string(&form, &text) {
        Ok(normalized) => crate::types::bool_::from_bool(normalized == text),
        Err(raised) => raised,
    }
}

/// `unicodedata.ucd_3_2_0.category(chr)`.
unsafe extern "C" fn ucd_3_2_category_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: The runtime call ABI passed `argc` live argument slots.
    match unsafe { char_argument(argv, argc, "category") } {
        Ok(ch) => str_object(ucd_3_2_category_of(ch as u32)),
        Err(error) => error,
    }
}

/// `unicodedata.ucd_3_2_0.bidirectional(chr)`.
unsafe extern "C" fn ucd_3_2_bidirectional_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: The runtime call ABI passed `argc` live argument slots.
    match unsafe { char_argument(argv, argc, "bidirectional") } {
        Ok(ch) => str_object(ucd_3_2_bidirectional_of(ch as u32)),
        Err(error) => error,
    }
}

/// `unicodedata.ucd_3_2_0.decomposition(chr)`.
unsafe extern "C" fn ucd_3_2_decomposition_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: The runtime call ABI passed `argc` live argument slots.
    match unsafe { char_argument(argv, argc, "decomposition") } {
        Ok(ch) => str_object(&decomposition_text(ch as u32)),
        Err(error) => error,
    }
}

// ---------------------------------------------------------------------------
// Module factory

fn builtin_function(module: &str, name: &str, entry: BuiltinFn) -> Result<*mut PyObject, String> {
    // SAFETY: `entry` is a live builtin entry point.
    let function = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    if function.is_null() {
        Err(format!("failed to allocate {module}.{name}"))
    } else {
        Ok(function)
    }
}

static UCD_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(
        abi::runtime_type_type().cast_const(),
        "unicodedata.UCD",
        core::mem::size_of::<PyUcd>(),
    );
    ty.tp_getattro = Some(ucd_getattro);
    Box::into_raw(Box::new(ty)) as usize
});

fn ucd_type() -> *mut PyType {
    *UCD_TYPE as *mut PyType
}

fn make_ucd_object(version: UcdVersion) -> *mut PyObject {
    Box::into_raw(Box::new(PyUcd {
        ob_base: PyObjectHeader::new(ucd_type()),
        version,
    }))
    .cast::<PyObject>()
}

unsafe extern "C" fn ucd_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { text_argument(untag(name)) }) else {
        return raise_type_error("attribute name must be str");
    };
    match name_text.as_str() {
        "unidata_version" => {
            if ucd_from_object(object).is_none() {
                return raise_type_error("invalid unicodedata.UCD receiver");
            }
            str_object(UCD_3_2_VERSION)
        }
        "bidirectional" => ucd_bound_method(object, "bidirectional", ucd_bidirectional_method),
        "category" => ucd_bound_method(object, "category", ucd_category_method),
        "combining" => ucd_bound_method(object, "combining", ucd_combining_method),
        "decimal" => ucd_bound_method(object, "decimal", ucd_decimal_method),
        "decomposition" => ucd_bound_method(object, "decomposition", ucd_decomposition_method),
        "digit" => ucd_bound_method(object, "digit", ucd_digit_method),
        "east_asian_width" => ucd_bound_method(object, "east_asian_width", ucd_east_asian_width_method),
        "is_normalized" => ucd_bound_method(object, "is_normalized", ucd_is_normalized_method),
        "lookup" => ucd_bound_method(object, "lookup", ucd_lookup_method),
        "mirrored" => ucd_bound_method(object, "mirrored", ucd_mirrored_method),
        "name" => ucd_bound_method(object, "name", ucd_name_method),
        "normalize" => ucd_bound_method(object, "normalize", ucd_normalize_method),
        "numeric" => ucd_bound_method(object, "numeric", ucd_numeric_method),
        _ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(&name_text)) },
    }
}

fn ucd_from_object(object: *mut PyObject) -> Option<&'static PyUcd> {
    if object.is_null() || crate::tag::is_small_int(object) {
        return None;
    }
    if unsafe { (*object).ob_type } == ucd_type().cast_const() {
        Some(unsafe { &*object.cast::<PyUcd>() })
    } else {
        None
    }
}

fn ucd_bound_method(receiver: *mut PyObject, name: &str, entry: BuiltinFn) -> *mut PyObject {
    let function = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match crate::types::method::new_bound_method(function, receiver) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => raise_type_error(&message),
    }
}

unsafe fn ucd_receiver_and_args<'a>(
    argv: *mut *mut PyObject,
    argc: usize,
    method: &str,
) -> Result<(&'static PyUcd, &'a [*mut PyObject]), *mut PyObject> {
    let args = unsafe { arg_slice(argv, argc) };
    let Some((receiver, rest)) = args.split_first() else {
        return Err(raise_type_error(&format!("unicodedata.UCD.{method}() missing receiver")));
    };
    let receiver = untag(*receiver);
    match ucd_from_object(receiver) {
        Some(ucd) => Ok((ucd, rest)),
        None => Err(raise_type_error(&format!("descriptor '{method}' for 'unicodedata.UCD' objects doesn't apply"))),
    }
}

fn call_unbound(entry: BuiltinFn, args: &[*mut PyObject]) -> *mut PyObject {
    unsafe { entry(args.as_ptr().cast_mut(), args.len()) }
}

unsafe extern "C" fn ucd_category_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (ucd, rest) = match unsafe { ucd_receiver_and_args(argv, argc, "category") } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    if ucd.version == UcdVersion::Ucd3_2 {
        return call_unbound(ucd_3_2_category_entry, rest);
    }
    call_unbound(category_entry, rest)
}

unsafe extern "C" fn ucd_bidirectional_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (ucd, rest) = match unsafe { ucd_receiver_and_args(argv, argc, "bidirectional") } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    if ucd.version == UcdVersion::Ucd3_2 {
        return call_unbound(ucd_3_2_bidirectional_entry, rest);
    }
    call_unbound(bidirectional_entry, rest)
}

unsafe extern "C" fn ucd_decomposition_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (ucd, rest) = match unsafe { ucd_receiver_and_args(argv, argc, "decomposition") } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    if ucd.version == UcdVersion::Ucd3_2 {
        return call_unbound(ucd_3_2_decomposition_entry, rest);
    }
    call_unbound(decomposition_entry, rest)
}

unsafe extern "C" fn ucd_combining_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (_, rest) = match unsafe { ucd_receiver_and_args(argv, argc, "combining") } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    call_unbound(combining_entry, rest)
}

unsafe extern "C" fn ucd_decimal_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (_, rest) = match unsafe { ucd_receiver_and_args(argv, argc, "decimal") } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    call_unbound(decimal_entry, rest)
}

unsafe extern "C" fn ucd_digit_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (_, rest) = match unsafe { ucd_receiver_and_args(argv, argc, "digit") } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    call_unbound(digit_entry, rest)
}

unsafe extern "C" fn ucd_east_asian_width_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (_, rest) = match unsafe { ucd_receiver_and_args(argv, argc, "east_asian_width") } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    call_unbound(east_asian_width_entry, rest)
}

unsafe extern "C" fn ucd_is_normalized_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (_, rest) = match unsafe { ucd_receiver_and_args(argv, argc, "is_normalized") } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    call_unbound(is_normalized_entry, rest)
}

unsafe extern "C" fn ucd_lookup_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (_, rest) = match unsafe { ucd_receiver_and_args(argv, argc, "lookup") } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    call_unbound(lookup_entry, rest)
}

unsafe extern "C" fn ucd_mirrored_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (_, rest) = match unsafe { ucd_receiver_and_args(argv, argc, "mirrored") } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    call_unbound(mirrored_entry, rest)
}

unsafe extern "C" fn ucd_name_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (_, rest) = match unsafe { ucd_receiver_and_args(argv, argc, "name") } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    call_unbound(name_entry, rest)
}

unsafe extern "C" fn ucd_normalize_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (_, rest) = match unsafe { ucd_receiver_and_args(argv, argc, "normalize") } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    call_unbound(normalize_entry, rest)
}

unsafe extern "C" fn ucd_numeric_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (_, rest) = match unsafe { ucd_receiver_and_args(argv, argc, "numeric") } {
        Ok(parts) => parts,
        Err(raised) => return raised,
    };
    call_unbound(numeric_entry, rest)
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let ucd_3_2_0 = make_ucd_object(UcdVersion::Ucd3_2);
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
    attrs.push((intern("ucd_3_2_0"), ucd_3_2_0));
    attrs.push((intern("UCD"), ucd_type().cast::<PyObject>()));
    attrs.push((intern("_ucnhash_CAPI"), unsafe { abi::pon_none() }));
    for (name, entry) in [
        ("bidirectional", bidirectional_entry as BuiltinFn),
        ("category", category_entry),
        ("combining", combining_entry),
        ("decimal", decimal_entry),
        ("decomposition", decomposition_entry),
        ("digit", digit_entry),
        ("east_asian_width", east_asian_width_entry),
        ("is_normalized", is_normalized_entry),
        ("lookup", lookup_entry),
        ("mirrored", mirrored_entry),
        ("name", name_entry),
        ("normalize", normalize_entry),
        ("numeric", numeric_entry),
    ] {
        attrs.push((intern(name), builtin_function("unicodedata", name, entry)?));
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
        assert_eq!(ucd_3_2_category_of(u32::from('A')), "Lu");
        assert_eq!(ucd_3_2_category_of(0x0221), "Cn");
        assert_eq!(ucd_3_2_bidirectional_of(u32::from('A')), "L");
        assert_eq!(ucd_3_2_bidirectional_of(0x05D0), "R");
        assert_eq!(ucd_3_2_bidirectional_of(0x0627), "AL");
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
