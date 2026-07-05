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

use tables::{
	BIDI_NAMES, BIDI_RANGES, CATEGORY_INDEX, CATEGORY_NAMES, CATEGORY_STARTS, COMBINING_RANGES,
	COMPOSE_KEYS, COMPOSE_VALUES, DECIMAL_RANGES, DECOMP_POOL, DECOMP_TEXT_CPS, DECOMP_TEXT_POOL,
	DECOMP_TEXT_SLICES, DIGIT_RANGES, EAW_INDEX, EAW_NAMES, EAW_STARTS, MIRRORED_RANGES,
	NAME_ALIAS_CPS, NAME_ALIAS_POOL, NAME_ALIAS_SLICES, NAME_CPS, NAME_POOL, NAME_SLICES,
	NAMED_SEQUENCE_DATA_POOL, NAMED_SEQUENCE_DATA_SLICES, NAMED_SEQUENCE_NAME_POOL,
	NAMED_SEQUENCE_NAME_SLICES, NFD_CPS, NFD_SLICES, NFKD_CPS, NFKD_SLICES, NUMERIC_RANGES,
	UNIDATA_VERSION,
};

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{
	abi,
	intern::intern,
	object::{PyObject, PyObjectHeader, PyType, PyUnicode},
	types::exc::ExceptionKind,
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
	if ty.is_null() {
		"<unknown>"
	} else {
		unsafe { (*ty).name() }
	}
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
unsafe fn char_argument(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
) -> Result<char, *mut PyObject> {
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
		return Err(raise_type_error(&format!(
			"{name} expected at most 2 arguments, got {}",
			args.len()
		)));
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
	let end = start + (packed & 0xff);
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
			let end = start + (packed & 0xff);
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
	range_search(&UCD_3_2_BIDI_RANGES, cp)
		.map_or("", |(_, index)| UCD_3_2_BIDI_NAMES[index as usize])
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
	(0x0, 0x220),
	(0x222, 0x233),
	(0x250, 0x2ad),
	(0x2b0, 0x2ee),
	(0x300, 0x34f),
	(0x360, 0x36f),
	(0x374, 0x375),
	(0x37a, 0x37a),
	(0x37e, 0x37e),
	(0x384, 0x38a),
	(0x38c, 0x38c),
	(0x38e, 0x3a1),
	(0x3a3, 0x3ce),
	(0x3d0, 0x3f6),
	(0x400, 0x486),
	(0x488, 0x4ce),
	(0x4d0, 0x4f5),
	(0x4f8, 0x4f9),
	(0x500, 0x50f),
	(0x531, 0x556),
	(0x559, 0x55f),
	(0x561, 0x587),
	(0x589, 0x58a),
	(0x591, 0x5a1),
	(0x5a3, 0x5b9),
	(0x5bb, 0x5c4),
	(0x5d0, 0x5ea),
	(0x5f0, 0x5f4),
	(0x60c, 0x60c),
	(0x61b, 0x61b),
	(0x61f, 0x61f),
	(0x621, 0x63a),
	(0x640, 0x655),
	(0x660, 0x6ed),
	(0x6f0, 0x6fe),
	(0x700, 0x70d),
	(0x70f, 0x72c),
	(0x730, 0x74a),
	(0x780, 0x7b1),
	(0x901, 0x903),
	(0x905, 0x939),
	(0x93c, 0x94d),
	(0x950, 0x954),
	(0x958, 0x970),
	(0x981, 0x983),
	(0x985, 0x98c),
	(0x98f, 0x990),
	(0x993, 0x9a8),
	(0x9aa, 0x9b0),
	(0x9b2, 0x9b2),
	(0x9b6, 0x9b9),
	(0x9bc, 0x9bc),
	(0x9be, 0x9c4),
	(0x9c7, 0x9c8),
	(0x9cb, 0x9cd),
	(0x9d7, 0x9d7),
	(0x9dc, 0x9dd),
	(0x9df, 0x9e3),
	(0x9e6, 0x9fa),
	(0xa02, 0xa02),
	(0xa05, 0xa0a),
	(0xa0f, 0xa10),
	(0xa13, 0xa28),
	(0xa2a, 0xa30),
	(0xa32, 0xa33),
	(0xa35, 0xa36),
	(0xa38, 0xa39),
	(0xa3c, 0xa3c),
	(0xa3e, 0xa42),
	(0xa47, 0xa48),
	(0xa4b, 0xa4d),
	(0xa59, 0xa5c),
	(0xa5e, 0xa5e),
	(0xa66, 0xa74),
	(0xa81, 0xa83),
	(0xa85, 0xa8b),
	(0xa8d, 0xa8d),
	(0xa8f, 0xa91),
	(0xa93, 0xaa8),
	(0xaaa, 0xab0),
	(0xab2, 0xab3),
	(0xab5, 0xab9),
	(0xabc, 0xac5),
	(0xac7, 0xac9),
	(0xacb, 0xacd),
	(0xad0, 0xad0),
	(0xae0, 0xae0),
	(0xae6, 0xaef),
	(0xb01, 0xb03),
	(0xb05, 0xb0c),
	(0xb0f, 0xb10),
	(0xb13, 0xb28),
	(0xb2a, 0xb30),
	(0xb32, 0xb33),
	(0xb36, 0xb39),
	(0xb3c, 0xb43),
	(0xb47, 0xb48),
	(0xb4b, 0xb4d),
	(0xb56, 0xb57),
	(0xb5c, 0xb5d),
	(0xb5f, 0xb61),
	(0xb66, 0xb70),
	(0xb82, 0xb83),
	(0xb85, 0xb8a),
	(0xb8e, 0xb90),
	(0xb92, 0xb95),
	(0xb99, 0xb9a),
	(0xb9c, 0xb9c),
	(0xb9e, 0xb9f),
	(0xba3, 0xba4),
	(0xba8, 0xbaa),
	(0xbae, 0xbb5),
	(0xbb7, 0xbb9),
	(0xbbe, 0xbc2),
	(0xbc6, 0xbc8),
	(0xbca, 0xbcd),
	(0xbd7, 0xbd7),
	(0xbe7, 0xbf2),
	(0xc01, 0xc03),
	(0xc05, 0xc0c),
	(0xc0e, 0xc10),
	(0xc12, 0xc28),
	(0xc2a, 0xc33),
	(0xc35, 0xc39),
	(0xc3e, 0xc44),
	(0xc46, 0xc48),
	(0xc4a, 0xc4d),
	(0xc55, 0xc56),
	(0xc60, 0xc61),
	(0xc66, 0xc6f),
	(0xc82, 0xc83),
	(0xc85, 0xc8c),
	(0xc8e, 0xc90),
	(0xc92, 0xca8),
	(0xcaa, 0xcb3),
	(0xcb5, 0xcb9),
	(0xcbe, 0xcc4),
	(0xcc6, 0xcc8),
	(0xcca, 0xccd),
	(0xcd5, 0xcd6),
	(0xcde, 0xcde),
	(0xce0, 0xce1),
	(0xce6, 0xcef),
	(0xd02, 0xd03),
	(0xd05, 0xd0c),
	(0xd0e, 0xd10),
	(0xd12, 0xd28),
	(0xd2a, 0xd39),
	(0xd3e, 0xd43),
	(0xd46, 0xd48),
	(0xd4a, 0xd4d),
	(0xd57, 0xd57),
	(0xd60, 0xd61),
	(0xd66, 0xd6f),
	(0xd82, 0xd83),
	(0xd85, 0xd96),
	(0xd9a, 0xdb1),
	(0xdb3, 0xdbb),
	(0xdbd, 0xdbd),
	(0xdc0, 0xdc6),
	(0xdca, 0xdca),
	(0xdcf, 0xdd4),
	(0xdd6, 0xdd6),
	(0xdd8, 0xddf),
	(0xdf2, 0xdf4),
	(0xe01, 0xe3a),
	(0xe3f, 0xe5b),
	(0xe81, 0xe82),
	(0xe84, 0xe84),
	(0xe87, 0xe88),
	(0xe8a, 0xe8a),
	(0xe8d, 0xe8d),
	(0xe94, 0xe97),
	(0xe99, 0xe9f),
	(0xea1, 0xea3),
	(0xea5, 0xea5),
	(0xea7, 0xea7),
	(0xeaa, 0xeab),
	(0xead, 0xeb9),
	(0xebb, 0xebd),
	(0xec0, 0xec4),
	(0xec6, 0xec6),
	(0xec8, 0xecd),
	(0xed0, 0xed9),
	(0xedc, 0xedd),
	(0xf00, 0xf47),
	(0xf49, 0xf6a),
	(0xf71, 0xf8b),
	(0xf90, 0xf97),
	(0xf99, 0xfbc),
	(0xfbe, 0xfcc),
	(0xfcf, 0xfcf),
	(0x1000, 0x1021),
	(0x1023, 0x1027),
	(0x1029, 0x102a),
	(0x102c, 0x1032),
	(0x1036, 0x1039),
	(0x1040, 0x1059),
	(0x10a0, 0x10c5),
	(0x10d0, 0x10f8),
	(0x10fb, 0x10fb),
	(0x1100, 0x1159),
	(0x115f, 0x11a2),
	(0x11a8, 0x11f9),
	(0x1200, 0x1206),
	(0x1208, 0x1246),
	(0x1248, 0x1248),
	(0x124a, 0x124d),
	(0x1250, 0x1256),
	(0x1258, 0x1258),
	(0x125a, 0x125d),
	(0x1260, 0x1286),
	(0x1288, 0x1288),
	(0x128a, 0x128d),
	(0x1290, 0x12ae),
	(0x12b0, 0x12b0),
	(0x12b2, 0x12b5),
	(0x12b8, 0x12be),
	(0x12c0, 0x12c0),
	(0x12c2, 0x12c5),
	(0x12c8, 0x12ce),
	(0x12d0, 0x12d6),
	(0x12d8, 0x12ee),
	(0x12f0, 0x130e),
	(0x1310, 0x1310),
	(0x1312, 0x1315),
	(0x1318, 0x131e),
	(0x1320, 0x1346),
	(0x1348, 0x135a),
	(0x1361, 0x137c),
	(0x13a0, 0x13f4),
	(0x1401, 0x1676),
	(0x1680, 0x169c),
	(0x16a0, 0x16f0),
	(0x1700, 0x170c),
	(0x170e, 0x1714),
	(0x1720, 0x1736),
	(0x1740, 0x1753),
	(0x1760, 0x176c),
	(0x176e, 0x1770),
	(0x1772, 0x1773),
	(0x1780, 0x17dc),
	(0x17e0, 0x17e9),
	(0x1800, 0x180e),
	(0x1810, 0x1819),
	(0x1820, 0x1877),
	(0x1880, 0x18a9),
	(0x1e00, 0x1e9b),
	(0x1ea0, 0x1ef9),
	(0x1f00, 0x1f15),
	(0x1f18, 0x1f1d),
	(0x1f20, 0x1f45),
	(0x1f48, 0x1f4d),
	(0x1f50, 0x1f57),
	(0x1f59, 0x1f59),
	(0x1f5b, 0x1f5b),
	(0x1f5d, 0x1f5d),
	(0x1f5f, 0x1f7d),
	(0x1f80, 0x1fb4),
	(0x1fb6, 0x1fc4),
	(0x1fc6, 0x1fd3),
	(0x1fd6, 0x1fdb),
	(0x1fdd, 0x1fef),
	(0x1ff2, 0x1ff4),
	(0x1ff6, 0x1ffe),
	(0x2000, 0x2052),
	(0x2057, 0x2057),
	(0x205f, 0x2063),
	(0x206a, 0x2071),
	(0x2074, 0x208e),
	(0x20a0, 0x20b1),
	(0x20d0, 0x20ea),
	(0x2100, 0x213a),
	(0x213d, 0x214b),
	(0x2153, 0x2183),
	(0x2190, 0x23ce),
	(0x2400, 0x2426),
	(0x2440, 0x244a),
	(0x2460, 0x24fe),
	(0x2500, 0x2613),
	(0x2616, 0x2617),
	(0x2619, 0x267d),
	(0x2680, 0x2689),
	(0x2701, 0x2704),
	(0x2706, 0x2709),
	(0x270c, 0x2727),
	(0x2729, 0x274b),
	(0x274d, 0x274d),
	(0x274f, 0x2752),
	(0x2756, 0x2756),
	(0x2758, 0x275e),
	(0x2761, 0x2794),
	(0x2798, 0x27af),
	(0x27b1, 0x27be),
	(0x27d0, 0x27eb),
	(0x27f0, 0x2aff),
	(0x2e80, 0x2e99),
	(0x2e9b, 0x2ef3),
	(0x2f00, 0x2fd5),
	(0x2ff0, 0x2ffb),
	(0x3000, 0x303f),
	(0x3041, 0x3096),
	(0x3099, 0x30ff),
	(0x3105, 0x312c),
	(0x3131, 0x318e),
	(0x3190, 0x31b7),
	(0x31f0, 0x321c),
	(0x3220, 0x3243),
	(0x3251, 0x327b),
	(0x327f, 0x32cb),
	(0x32d0, 0x32fe),
	(0x3300, 0x3376),
	(0x337b, 0x33dd),
	(0x33e0, 0x33fe),
	(0x3400, 0x4db5),
	(0x4e00, 0x9fa5),
	(0xa000, 0xa48c),
	(0xa490, 0xa4c6),
	(0xac00, 0xd7a3),
	(0xe000, 0xfa2d),
	(0xfa30, 0xfa6a),
	(0xfb00, 0xfb06),
	(0xfb13, 0xfb17),
	(0xfb1d, 0xfb36),
	(0xfb38, 0xfb3c),
	(0xfb3e, 0xfb3e),
	(0xfb40, 0xfb41),
	(0xfb43, 0xfb44),
	(0xfb46, 0xfbb1),
	(0xfbd3, 0xfd3f),
	(0xfd50, 0xfd8f),
	(0xfd92, 0xfdc7),
	(0xfdf0, 0xfdfc),
	(0xfe00, 0xfe0f),
	(0xfe20, 0xfe23),
	(0xfe30, 0xfe46),
	(0xfe49, 0xfe52),
	(0xfe54, 0xfe66),
	(0xfe68, 0xfe6b),
	(0xfe70, 0xfe74),
	(0xfe76, 0xfefc),
	(0xfeff, 0xfeff),
	(0xff01, 0xffbe),
	(0xffc2, 0xffc7),
	(0xffca, 0xffcf),
	(0xffd2, 0xffd7),
	(0xffda, 0xffdc),
	(0xffe0, 0xffe6),
	(0xffe8, 0xffee),
	(0xfff9, 0xfffd),
	(0x10300, 0x1031e),
	(0x10320, 0x10323),
	(0x10330, 0x1034a),
	(0x10400, 0x10425),
	(0x10428, 0x1044d),
	(0x1d000, 0x1d0f5),
	(0x1d100, 0x1d126),
	(0x1d12a, 0x1d1dd),
	(0x1d400, 0x1d454),
	(0x1d456, 0x1d49c),
	(0x1d49e, 0x1d49f),
	(0x1d4a2, 0x1d4a2),
	(0x1d4a5, 0x1d4a6),
	(0x1d4a9, 0x1d4ac),
	(0x1d4ae, 0x1d4b9),
	(0x1d4bb, 0x1d4bb),
	(0x1d4bd, 0x1d4c0),
	(0x1d4c2, 0x1d4c3),
	(0x1d4c5, 0x1d505),
	(0x1d507, 0x1d50a),
	(0x1d50d, 0x1d514),
	(0x1d516, 0x1d51c),
	(0x1d51e, 0x1d539),
	(0x1d53b, 0x1d53e),
	(0x1d540, 0x1d544),
	(0x1d546, 0x1d546),
	(0x1d54a, 0x1d550),
	(0x1d552, 0x1d6a3),
	(0x1d6a8, 0x1d7c9),
	(0x1d7ce, 0x1d7ff),
	(0x20000, 0x2a6d6),
	(0x2f800, 0x2fa1d),
	(0xe0001, 0xe0001),
	(0xe0020, 0xe007f),
	(0xf0000, 0xffffd),
	(0x100000, 0x10fffd),
];

static UCD_3_2_BIDI_NAMES: [&str; 19] = [
	"AL", "AN", "B", "BN", "CS", "EN", "ES", "ET", "L", "LRE", "LRO", "NSM", "ON", "PDF", "R",
	"RLE", "RLO", "S", "WS",
];

static UCD_3_2_BIDI_RANGES: [(u32, u32, u8); 692] = [
	(0x0, 0x8, 3),
	(0x9, 0x9, 17),
	(0xa, 0xa, 2),
	(0xb, 0xb, 17),
	(0xc, 0xc, 18),
	(0xd, 0xd, 2),
	(0xe, 0x1b, 3),
	(0x1c, 0x1e, 2),
	(0x1f, 0x1f, 17),
	(0x20, 0x20, 18),
	(0x21, 0x22, 12),
	(0x23, 0x25, 7),
	(0x26, 0x2a, 12),
	(0x2b, 0x2b, 7),
	(0x2c, 0x2c, 4),
	(0x2d, 0x2d, 7),
	(0x2e, 0x2e, 4),
	(0x2f, 0x2f, 6),
	(0x30, 0x39, 5),
	(0x3a, 0x3a, 4),
	(0x3b, 0x40, 12),
	(0x41, 0x5a, 8),
	(0x5b, 0x60, 12),
	(0x61, 0x7a, 8),
	(0x7b, 0x7e, 12),
	(0x7f, 0x84, 3),
	(0x85, 0x85, 2),
	(0x86, 0x9f, 3),
	(0xa0, 0xa0, 4),
	(0xa1, 0xa1, 12),
	(0xa2, 0xa5, 7),
	(0xa6, 0xa9, 12),
	(0xaa, 0xaa, 8),
	(0xab, 0xaf, 12),
	(0xb0, 0xb1, 7),
	(0xb2, 0xb3, 5),
	(0xb4, 0xb4, 12),
	(0xb5, 0xb5, 8),
	(0xb6, 0xb8, 12),
	(0xb9, 0xb9, 5),
	(0xba, 0xba, 8),
	(0xbb, 0xbf, 12),
	(0xc0, 0xd6, 8),
	(0xd7, 0xd7, 12),
	(0xd8, 0xf6, 8),
	(0xf7, 0xf7, 12),
	(0xf8, 0x220, 8),
	(0x222, 0x233, 8),
	(0x250, 0x2ad, 8),
	(0x2b0, 0x2b8, 8),
	(0x2b9, 0x2ba, 12),
	(0x2bb, 0x2c1, 8),
	(0x2c2, 0x2cf, 12),
	(0x2d0, 0x2d1, 8),
	(0x2d2, 0x2df, 12),
	(0x2e0, 0x2e4, 8),
	(0x2e5, 0x2ed, 12),
	(0x2ee, 0x2ee, 8),
	(0x300, 0x34f, 11),
	(0x360, 0x36f, 11),
	(0x374, 0x375, 12),
	(0x37a, 0x37a, 8),
	(0x37e, 0x37e, 12),
	(0x384, 0x385, 12),
	(0x386, 0x386, 8),
	(0x387, 0x387, 12),
	(0x388, 0x38a, 8),
	(0x38c, 0x38c, 8),
	(0x38e, 0x3a1, 8),
	(0x3a3, 0x3ce, 8),
	(0x3d0, 0x3f5, 8),
	(0x3f6, 0x3f6, 12),
	(0x400, 0x482, 8),
	(0x483, 0x486, 11),
	(0x488, 0x489, 11),
	(0x48a, 0x4ce, 8),
	(0x4d0, 0x4f5, 8),
	(0x4f8, 0x4f9, 8),
	(0x500, 0x50f, 8),
	(0x531, 0x556, 8),
	(0x559, 0x55f, 8),
	(0x561, 0x587, 8),
	(0x589, 0x589, 8),
	(0x58a, 0x58a, 12),
	(0x591, 0x5a1, 11),
	(0x5a3, 0x5b9, 11),
	(0x5bb, 0x5bd, 11),
	(0x5be, 0x5be, 14),
	(0x5bf, 0x5bf, 11),
	(0x5c0, 0x5c0, 14),
	(0x5c1, 0x5c2, 11),
	(0x5c3, 0x5c3, 14),
	(0x5c4, 0x5c4, 11),
	(0x5d0, 0x5ea, 14),
	(0x5f0, 0x5f4, 14),
	(0x60c, 0x60c, 4),
	(0x61b, 0x61b, 0),
	(0x61f, 0x61f, 0),
	(0x621, 0x63a, 0),
	(0x640, 0x64a, 0),
	(0x64b, 0x655, 11),
	(0x660, 0x669, 1),
	(0x66a, 0x66a, 7),
	(0x66b, 0x66c, 1),
	(0x66d, 0x66f, 0),
	(0x670, 0x670, 11),
	(0x671, 0x6d5, 0),
	(0x6d6, 0x6dc, 11),
	(0x6dd, 0x6dd, 0),
	(0x6de, 0x6e4, 11),
	(0x6e5, 0x6e6, 0),
	(0x6e7, 0x6e8, 11),
	(0x6e9, 0x6e9, 12),
	(0x6ea, 0x6ed, 11),
	(0x6f0, 0x6f9, 5),
	(0x6fa, 0x6fe, 0),
	(0x700, 0x70d, 0),
	(0x70f, 0x70f, 3),
	(0x710, 0x710, 0),
	(0x711, 0x711, 11),
	(0x712, 0x72c, 0),
	(0x730, 0x74a, 11),
	(0x780, 0x7a5, 0),
	(0x7a6, 0x7b0, 11),
	(0x7b1, 0x7b1, 0),
	(0x901, 0x902, 11),
	(0x903, 0x903, 8),
	(0x905, 0x939, 8),
	(0x93c, 0x93c, 11),
	(0x93d, 0x940, 8),
	(0x941, 0x948, 11),
	(0x949, 0x94c, 8),
	(0x94d, 0x94d, 11),
	(0x950, 0x950, 8),
	(0x951, 0x954, 11),
	(0x958, 0x961, 8),
	(0x962, 0x963, 11),
	(0x964, 0x970, 8),
	(0x981, 0x981, 11),
	(0x982, 0x983, 8),
	(0x985, 0x98c, 8),
	(0x98f, 0x990, 8),
	(0x993, 0x9a8, 8),
	(0x9aa, 0x9b0, 8),
	(0x9b2, 0x9b2, 8),
	(0x9b6, 0x9b9, 8),
	(0x9bc, 0x9bc, 11),
	(0x9be, 0x9c0, 8),
	(0x9c1, 0x9c4, 11),
	(0x9c7, 0x9c8, 8),
	(0x9cb, 0x9cc, 8),
	(0x9cd, 0x9cd, 11),
	(0x9d7, 0x9d7, 8),
	(0x9dc, 0x9dd, 8),
	(0x9df, 0x9e1, 8),
	(0x9e2, 0x9e3, 11),
	(0x9e6, 0x9f1, 8),
	(0x9f2, 0x9f3, 7),
	(0x9f4, 0x9fa, 8),
	(0xa02, 0xa02, 11),
	(0xa05, 0xa0a, 8),
	(0xa0f, 0xa10, 8),
	(0xa13, 0xa28, 8),
	(0xa2a, 0xa30, 8),
	(0xa32, 0xa33, 8),
	(0xa35, 0xa36, 8),
	(0xa38, 0xa39, 8),
	(0xa3c, 0xa3c, 11),
	(0xa3e, 0xa40, 8),
	(0xa41, 0xa42, 11),
	(0xa47, 0xa48, 11),
	(0xa4b, 0xa4d, 11),
	(0xa59, 0xa5c, 8),
	(0xa5e, 0xa5e, 8),
	(0xa66, 0xa6f, 8),
	(0xa70, 0xa71, 11),
	(0xa72, 0xa74, 8),
	(0xa81, 0xa82, 11),
	(0xa83, 0xa83, 8),
	(0xa85, 0xa8b, 8),
	(0xa8d, 0xa8d, 8),
	(0xa8f, 0xa91, 8),
	(0xa93, 0xaa8, 8),
	(0xaaa, 0xab0, 8),
	(0xab2, 0xab3, 8),
	(0xab5, 0xab9, 8),
	(0xabc, 0xabc, 11),
	(0xabd, 0xac0, 8),
	(0xac1, 0xac5, 11),
	(0xac7, 0xac8, 11),
	(0xac9, 0xac9, 8),
	(0xacb, 0xacc, 8),
	(0xacd, 0xacd, 11),
	(0xad0, 0xad0, 8),
	(0xae0, 0xae0, 8),
	(0xae6, 0xaef, 8),
	(0xb01, 0xb01, 11),
	(0xb02, 0xb03, 8),
	(0xb05, 0xb0c, 8),
	(0xb0f, 0xb10, 8),
	(0xb13, 0xb28, 8),
	(0xb2a, 0xb30, 8),
	(0xb32, 0xb33, 8),
	(0xb36, 0xb39, 8),
	(0xb3c, 0xb3c, 11),
	(0xb3d, 0xb3e, 8),
	(0xb3f, 0xb3f, 11),
	(0xb40, 0xb40, 8),
	(0xb41, 0xb43, 11),
	(0xb47, 0xb48, 8),
	(0xb4b, 0xb4c, 8),
	(0xb4d, 0xb4d, 11),
	(0xb56, 0xb56, 11),
	(0xb57, 0xb57, 8),
	(0xb5c, 0xb5d, 8),
	(0xb5f, 0xb61, 8),
	(0xb66, 0xb70, 8),
	(0xb82, 0xb82, 11),
	(0xb83, 0xb83, 8),
	(0xb85, 0xb8a, 8),
	(0xb8e, 0xb90, 8),
	(0xb92, 0xb95, 8),
	(0xb99, 0xb9a, 8),
	(0xb9c, 0xb9c, 8),
	(0xb9e, 0xb9f, 8),
	(0xba3, 0xba4, 8),
	(0xba8, 0xbaa, 8),
	(0xbae, 0xbb5, 8),
	(0xbb7, 0xbb9, 8),
	(0xbbe, 0xbbf, 8),
	(0xbc0, 0xbc0, 11),
	(0xbc1, 0xbc2, 8),
	(0xbc6, 0xbc8, 8),
	(0xbca, 0xbcc, 8),
	(0xbcd, 0xbcd, 11),
	(0xbd7, 0xbd7, 8),
	(0xbe7, 0xbf2, 8),
	(0xc01, 0xc03, 8),
	(0xc05, 0xc0c, 8),
	(0xc0e, 0xc10, 8),
	(0xc12, 0xc28, 8),
	(0xc2a, 0xc33, 8),
	(0xc35, 0xc39, 8),
	(0xc3e, 0xc40, 11),
	(0xc41, 0xc44, 8),
	(0xc46, 0xc48, 11),
	(0xc4a, 0xc4d, 11),
	(0xc55, 0xc56, 11),
	(0xc60, 0xc61, 8),
	(0xc66, 0xc6f, 8),
	(0xc82, 0xc83, 8),
	(0xc85, 0xc8c, 8),
	(0xc8e, 0xc90, 8),
	(0xc92, 0xca8, 8),
	(0xcaa, 0xcb3, 8),
	(0xcb5, 0xcb9, 8),
	(0xcbe, 0xcbe, 8),
	(0xcbf, 0xcbf, 11),
	(0xcc0, 0xcc4, 8),
	(0xcc6, 0xcc6, 11),
	(0xcc7, 0xcc8, 8),
	(0xcca, 0xccb, 8),
	(0xccc, 0xccd, 11),
	(0xcd5, 0xcd6, 8),
	(0xcde, 0xcde, 8),
	(0xce0, 0xce1, 8),
	(0xce6, 0xcef, 8),
	(0xd02, 0xd03, 8),
	(0xd05, 0xd0c, 8),
	(0xd0e, 0xd10, 8),
	(0xd12, 0xd28, 8),
	(0xd2a, 0xd39, 8),
	(0xd3e, 0xd40, 8),
	(0xd41, 0xd43, 11),
	(0xd46, 0xd48, 8),
	(0xd4a, 0xd4c, 8),
	(0xd4d, 0xd4d, 11),
	(0xd57, 0xd57, 8),
	(0xd60, 0xd61, 8),
	(0xd66, 0xd6f, 8),
	(0xd82, 0xd83, 8),
	(0xd85, 0xd96, 8),
	(0xd9a, 0xdb1, 8),
	(0xdb3, 0xdbb, 8),
	(0xdbd, 0xdbd, 8),
	(0xdc0, 0xdc6, 8),
	(0xdca, 0xdca, 11),
	(0xdcf, 0xdd1, 8),
	(0xdd2, 0xdd4, 11),
	(0xdd6, 0xdd6, 11),
	(0xdd8, 0xddf, 8),
	(0xdf2, 0xdf4, 8),
	(0xe01, 0xe30, 8),
	(0xe31, 0xe31, 11),
	(0xe32, 0xe33, 8),
	(0xe34, 0xe3a, 11),
	(0xe3f, 0xe3f, 7),
	(0xe40, 0xe46, 8),
	(0xe47, 0xe4e, 11),
	(0xe4f, 0xe5b, 8),
	(0xe81, 0xe82, 8),
	(0xe84, 0xe84, 8),
	(0xe87, 0xe88, 8),
	(0xe8a, 0xe8a, 8),
	(0xe8d, 0xe8d, 8),
	(0xe94, 0xe97, 8),
	(0xe99, 0xe9f, 8),
	(0xea1, 0xea3, 8),
	(0xea5, 0xea5, 8),
	(0xea7, 0xea7, 8),
	(0xeaa, 0xeab, 8),
	(0xead, 0xeb0, 8),
	(0xeb1, 0xeb1, 11),
	(0xeb2, 0xeb3, 8),
	(0xeb4, 0xeb9, 11),
	(0xebb, 0xebc, 11),
	(0xebd, 0xebd, 8),
	(0xec0, 0xec4, 8),
	(0xec6, 0xec6, 8),
	(0xec8, 0xecd, 11),
	(0xed0, 0xed9, 8),
	(0xedc, 0xedd, 8),
	(0xf00, 0xf17, 8),
	(0xf18, 0xf19, 11),
	(0xf1a, 0xf34, 8),
	(0xf35, 0xf35, 11),
	(0xf36, 0xf36, 8),
	(0xf37, 0xf37, 11),
	(0xf38, 0xf38, 8),
	(0xf39, 0xf39, 11),
	(0xf3a, 0xf3d, 12),
	(0xf3e, 0xf47, 8),
	(0xf49, 0xf6a, 8),
	(0xf71, 0xf7e, 11),
	(0xf7f, 0xf7f, 8),
	(0xf80, 0xf84, 11),
	(0xf85, 0xf85, 8),
	(0xf86, 0xf87, 11),
	(0xf88, 0xf8b, 8),
	(0xf90, 0xf97, 11),
	(0xf99, 0xfbc, 11),
	(0xfbe, 0xfc5, 8),
	(0xfc6, 0xfc6, 11),
	(0xfc7, 0xfcc, 8),
	(0xfcf, 0xfcf, 8),
	(0x1000, 0x1021, 8),
	(0x1023, 0x1027, 8),
	(0x1029, 0x102a, 8),
	(0x102c, 0x102c, 8),
	(0x102d, 0x1030, 11),
	(0x1031, 0x1031, 8),
	(0x1032, 0x1032, 11),
	(0x1036, 0x1037, 11),
	(0x1038, 0x1038, 8),
	(0x1039, 0x1039, 11),
	(0x1040, 0x1057, 8),
	(0x1058, 0x1059, 11),
	(0x10a0, 0x10c5, 8),
	(0x10d0, 0x10f8, 8),
	(0x10fb, 0x10fb, 8),
	(0x1100, 0x1159, 8),
	(0x115f, 0x11a2, 8),
	(0x11a8, 0x11f9, 8),
	(0x1200, 0x1206, 8),
	(0x1208, 0x1246, 8),
	(0x1248, 0x1248, 8),
	(0x124a, 0x124d, 8),
	(0x1250, 0x1256, 8),
	(0x1258, 0x1258, 8),
	(0x125a, 0x125d, 8),
	(0x1260, 0x1286, 8),
	(0x1288, 0x1288, 8),
	(0x128a, 0x128d, 8),
	(0x1290, 0x12ae, 8),
	(0x12b0, 0x12b0, 8),
	(0x12b2, 0x12b5, 8),
	(0x12b8, 0x12be, 8),
	(0x12c0, 0x12c0, 8),
	(0x12c2, 0x12c5, 8),
	(0x12c8, 0x12ce, 8),
	(0x12d0, 0x12d6, 8),
	(0x12d8, 0x12ee, 8),
	(0x12f0, 0x130e, 8),
	(0x1310, 0x1310, 8),
	(0x1312, 0x1315, 8),
	(0x1318, 0x131e, 8),
	(0x1320, 0x1346, 8),
	(0x1348, 0x135a, 8),
	(0x1361, 0x137c, 8),
	(0x13a0, 0x13f4, 8),
	(0x1401, 0x1676, 8),
	(0x1680, 0x1680, 18),
	(0x1681, 0x169a, 8),
	(0x169b, 0x169c, 12),
	(0x16a0, 0x16f0, 8),
	(0x1700, 0x170c, 8),
	(0x170e, 0x1711, 8),
	(0x1712, 0x1714, 11),
	(0x1720, 0x1731, 8),
	(0x1732, 0x1734, 11),
	(0x1735, 0x1736, 8),
	(0x1740, 0x1751, 8),
	(0x1752, 0x1753, 11),
	(0x1760, 0x176c, 8),
	(0x176e, 0x1770, 8),
	(0x1772, 0x1773, 11),
	(0x1780, 0x17b6, 8),
	(0x17b7, 0x17bd, 11),
	(0x17be, 0x17c5, 8),
	(0x17c6, 0x17c6, 11),
	(0x17c7, 0x17c8, 8),
	(0x17c9, 0x17d3, 11),
	(0x17d4, 0x17da, 8),
	(0x17db, 0x17db, 7),
	(0x17dc, 0x17dc, 8),
	(0x17e0, 0x17e9, 8),
	(0x1800, 0x180a, 12),
	(0x180b, 0x180d, 11),
	(0x180e, 0x180e, 3),
	(0x1810, 0x1819, 8),
	(0x1820, 0x1877, 8),
	(0x1880, 0x18a8, 8),
	(0x18a9, 0x18a9, 11),
	(0x1e00, 0x1e9b, 8),
	(0x1ea0, 0x1ef9, 8),
	(0x1f00, 0x1f15, 8),
	(0x1f18, 0x1f1d, 8),
	(0x1f20, 0x1f45, 8),
	(0x1f48, 0x1f4d, 8),
	(0x1f50, 0x1f57, 8),
	(0x1f59, 0x1f59, 8),
	(0x1f5b, 0x1f5b, 8),
	(0x1f5d, 0x1f5d, 8),
	(0x1f5f, 0x1f7d, 8),
	(0x1f80, 0x1fb4, 8),
	(0x1fb6, 0x1fbc, 8),
	(0x1fbd, 0x1fbd, 12),
	(0x1fbe, 0x1fbe, 8),
	(0x1fbf, 0x1fc1, 12),
	(0x1fc2, 0x1fc4, 8),
	(0x1fc6, 0x1fcc, 8),
	(0x1fcd, 0x1fcf, 12),
	(0x1fd0, 0x1fd3, 8),
	(0x1fd6, 0x1fdb, 8),
	(0x1fdd, 0x1fdf, 12),
	(0x1fe0, 0x1fec, 8),
	(0x1fed, 0x1fef, 12),
	(0x1ff2, 0x1ff4, 8),
	(0x1ff6, 0x1ffc, 8),
	(0x1ffd, 0x1ffe, 12),
	(0x2000, 0x200a, 18),
	(0x200b, 0x200d, 3),
	(0x200e, 0x200e, 8),
	(0x200f, 0x200f, 14),
	(0x2010, 0x2027, 12),
	(0x2028, 0x2028, 18),
	(0x2029, 0x2029, 2),
	(0x202a, 0x202a, 9),
	(0x202b, 0x202b, 15),
	(0x202c, 0x202c, 13),
	(0x202d, 0x202d, 10),
	(0x202e, 0x202e, 16),
	(0x202f, 0x202f, 18),
	(0x2030, 0x2034, 7),
	(0x2035, 0x2052, 12),
	(0x2057, 0x2057, 12),
	(0x205f, 0x205f, 18),
	(0x2060, 0x2063, 3),
	(0x206a, 0x206f, 3),
	(0x2070, 0x2070, 5),
	(0x2071, 0x2071, 8),
	(0x2074, 0x2079, 5),
	(0x207a, 0x207b, 7),
	(0x207c, 0x207e, 12),
	(0x207f, 0x207f, 8),
	(0x2080, 0x2089, 5),
	(0x208a, 0x208b, 7),
	(0x208c, 0x208e, 12),
	(0x20a0, 0x20b1, 7),
	(0x20d0, 0x20ea, 11),
	(0x2100, 0x2101, 12),
	(0x2102, 0x2102, 8),
	(0x2103, 0x2106, 12),
	(0x2107, 0x2107, 8),
	(0x2108, 0x2109, 12),
	(0x210a, 0x2113, 8),
	(0x2114, 0x2114, 12),
	(0x2115, 0x2115, 8),
	(0x2116, 0x2118, 12),
	(0x2119, 0x211d, 8),
	(0x211e, 0x2123, 12),
	(0x2124, 0x2124, 8),
	(0x2125, 0x2125, 12),
	(0x2126, 0x2126, 8),
	(0x2127, 0x2127, 12),
	(0x2128, 0x2128, 8),
	(0x2129, 0x2129, 12),
	(0x212a, 0x212d, 8),
	(0x212e, 0x212e, 7),
	(0x212f, 0x2131, 8),
	(0x2132, 0x2132, 12),
	(0x2133, 0x2139, 8),
	(0x213a, 0x213a, 12),
	(0x213d, 0x213f, 8),
	(0x2140, 0x2144, 12),
	(0x2145, 0x2149, 8),
	(0x214a, 0x214b, 12),
	(0x2153, 0x215f, 12),
	(0x2160, 0x2183, 8),
	(0x2190, 0x2211, 12),
	(0x2212, 0x2213, 7),
	(0x2214, 0x2335, 12),
	(0x2336, 0x237a, 8),
	(0x237b, 0x2394, 12),
	(0x2395, 0x2395, 8),
	(0x2396, 0x23ce, 12),
	(0x2400, 0x2426, 12),
	(0x2440, 0x244a, 12),
	(0x2460, 0x249b, 5),
	(0x249c, 0x24e9, 8),
	(0x24ea, 0x24ea, 5),
	(0x24eb, 0x24fe, 12),
	(0x2500, 0x2613, 12),
	(0x2616, 0x2617, 12),
	(0x2619, 0x267d, 12),
	(0x2680, 0x2689, 12),
	(0x2701, 0x2704, 12),
	(0x2706, 0x2709, 12),
	(0x270c, 0x2727, 12),
	(0x2729, 0x274b, 12),
	(0x274d, 0x274d, 12),
	(0x274f, 0x2752, 12),
	(0x2756, 0x2756, 12),
	(0x2758, 0x275e, 12),
	(0x2761, 0x2794, 12),
	(0x2798, 0x27af, 12),
	(0x27b1, 0x27be, 12),
	(0x27d0, 0x27eb, 12),
	(0x27f0, 0x2aff, 12),
	(0x2e80, 0x2e99, 12),
	(0x2e9b, 0x2ef3, 12),
	(0x2f00, 0x2fd5, 12),
	(0x2ff0, 0x2ffb, 12),
	(0x3000, 0x3000, 18),
	(0x3001, 0x3004, 12),
	(0x3005, 0x3007, 8),
	(0x3008, 0x3020, 12),
	(0x3021, 0x3029, 8),
	(0x302a, 0x302f, 11),
	(0x3030, 0x3030, 12),
	(0x3031, 0x3035, 8),
	(0x3036, 0x3037, 12),
	(0x3038, 0x303c, 8),
	(0x303d, 0x303f, 12),
	(0x3041, 0x3096, 8),
	(0x3099, 0x309a, 11),
	(0x309b, 0x309c, 12),
	(0x309d, 0x309f, 8),
	(0x30a0, 0x30a0, 12),
	(0x30a1, 0x30fa, 8),
	(0x30fb, 0x30fb, 12),
	(0x30fc, 0x30ff, 8),
	(0x3105, 0x312c, 8),
	(0x3131, 0x318e, 8),
	(0x3190, 0x31b7, 8),
	(0x31f0, 0x321c, 8),
	(0x3220, 0x3243, 8),
	(0x3251, 0x325f, 12),
	(0x3260, 0x327b, 8),
	(0x327f, 0x32b0, 8),
	(0x32b1, 0x32bf, 12),
	(0x32c0, 0x32cb, 8),
	(0x32d0, 0x32fe, 8),
	(0x3300, 0x3376, 8),
	(0x337b, 0x33dd, 8),
	(0x33e0, 0x33fe, 8),
	(0x3400, 0x4db5, 8),
	(0x4e00, 0x9fa5, 8),
	(0xa000, 0xa48c, 8),
	(0xa490, 0xa4c6, 12),
	(0xac00, 0xd7a3, 8),
	(0xe000, 0xfa2d, 8),
	(0xfa30, 0xfa6a, 8),
	(0xfb00, 0xfb06, 8),
	(0xfb13, 0xfb17, 8),
	(0xfb1d, 0xfb1d, 14),
	(0xfb1e, 0xfb1e, 11),
	(0xfb1f, 0xfb28, 14),
	(0xfb29, 0xfb29, 7),
	(0xfb2a, 0xfb36, 14),
	(0xfb38, 0xfb3c, 14),
	(0xfb3e, 0xfb3e, 14),
	(0xfb40, 0xfb41, 14),
	(0xfb43, 0xfb44, 14),
	(0xfb46, 0xfb4f, 14),
	(0xfb50, 0xfbb1, 0),
	(0xfbd3, 0xfd3d, 0),
	(0xfd3e, 0xfd3f, 12),
	(0xfd50, 0xfd8f, 0),
	(0xfd92, 0xfdc7, 0),
	(0xfdf0, 0xfdfc, 0),
	(0xfe00, 0xfe0f, 11),
	(0xfe20, 0xfe23, 11),
	(0xfe30, 0xfe46, 12),
	(0xfe49, 0xfe4f, 12),
	(0xfe50, 0xfe50, 4),
	(0xfe51, 0xfe51, 12),
	(0xfe52, 0xfe52, 4),
	(0xfe54, 0xfe54, 12),
	(0xfe55, 0xfe55, 4),
	(0xfe56, 0xfe5e, 12),
	(0xfe5f, 0xfe5f, 7),
	(0xfe60, 0xfe61, 12),
	(0xfe62, 0xfe63, 7),
	(0xfe64, 0xfe66, 12),
	(0xfe68, 0xfe68, 12),
	(0xfe69, 0xfe6a, 7),
	(0xfe6b, 0xfe6b, 12),
	(0xfe70, 0xfe74, 0),
	(0xfe76, 0xfefc, 0),
	(0xfeff, 0xfeff, 3),
	(0xff01, 0xff02, 12),
	(0xff03, 0xff05, 7),
	(0xff06, 0xff0a, 12),
	(0xff0b, 0xff0b, 7),
	(0xff0c, 0xff0c, 4),
	(0xff0d, 0xff0d, 7),
	(0xff0e, 0xff0e, 4),
	(0xff0f, 0xff0f, 6),
	(0xff10, 0xff19, 5),
	(0xff1a, 0xff1a, 4),
	(0xff1b, 0xff20, 12),
	(0xff21, 0xff3a, 8),
	(0xff3b, 0xff40, 12),
	(0xff41, 0xff5a, 8),
	(0xff5b, 0xff65, 12),
	(0xff66, 0xffbe, 8),
	(0xffc2, 0xffc7, 8),
	(0xffca, 0xffcf, 8),
	(0xffd2, 0xffd7, 8),
	(0xffda, 0xffdc, 8),
	(0xffe0, 0xffe1, 7),
	(0xffe2, 0xffe4, 12),
	(0xffe5, 0xffe6, 7),
	(0xffe8, 0xffee, 12),
	(0xfff9, 0xfffb, 3),
	(0xfffc, 0xfffd, 12),
	(0x10300, 0x1031e, 8),
	(0x10320, 0x10323, 8),
	(0x10330, 0x1034a, 8),
	(0x10400, 0x10425, 8),
	(0x10428, 0x1044d, 8),
	(0x1d000, 0x1d0f5, 8),
	(0x1d100, 0x1d126, 8),
	(0x1d12a, 0x1d166, 8),
	(0x1d167, 0x1d169, 11),
	(0x1d16a, 0x1d172, 8),
	(0x1d173, 0x1d17a, 3),
	(0x1d17b, 0x1d182, 11),
	(0x1d183, 0x1d184, 8),
	(0x1d185, 0x1d18b, 11),
	(0x1d18c, 0x1d1a9, 8),
	(0x1d1aa, 0x1d1ad, 11),
	(0x1d1ae, 0x1d1dd, 8),
	(0x1d400, 0x1d454, 8),
	(0x1d456, 0x1d49c, 8),
	(0x1d49e, 0x1d49f, 8),
	(0x1d4a2, 0x1d4a2, 8),
	(0x1d4a5, 0x1d4a6, 8),
	(0x1d4a9, 0x1d4ac, 8),
	(0x1d4ae, 0x1d4b9, 8),
	(0x1d4bb, 0x1d4bb, 8),
	(0x1d4bd, 0x1d4c0, 8),
	(0x1d4c2, 0x1d4c3, 8),
	(0x1d4c5, 0x1d505, 8),
	(0x1d507, 0x1d50a, 8),
	(0x1d50d, 0x1d514, 8),
	(0x1d516, 0x1d51c, 8),
	(0x1d51e, 0x1d539, 8),
	(0x1d53b, 0x1d53e, 8),
	(0x1d540, 0x1d544, 8),
	(0x1d546, 0x1d546, 8),
	(0x1d54a, 0x1d550, 8),
	(0x1d552, 0x1d6a3, 8),
	(0x1d6a8, 0x1d7c9, 8),
	(0x1d7ce, 0x1d7ff, 5),
	(0x20000, 0x2a6d6, 8),
	(0x2f800, 0x2fa1d, 8),
	(0xe0001, 0xe0001, 3),
	(0xe0020, 0xe007f, 3),
	(0xf0000, 0xffffd, 8),
	(0x100000, 0x10fffd, 8),
];

// ---------------------------------------------------------------------------
// UAX #15 normalization

// Hangul syllable constants (UAX #15 §3.12).
const S_BASE: u32 = 0xac00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11a7;
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
	COMPOSE_KEYS
		.binary_search(&key)
		.ok()
		.map(|index| COMPOSE_VALUES[index])
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
			None => {
				return Err(raise_value_error(
					"unicodedata: generated table produced an invalid code point",
				));
			},
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
		return raise_type_error(&format!(
			"normalize() argument 1 must be str, not {}",
			type_name(form_obj)
		));
	};
	let text_obj = untag(args[1]);
	// SAFETY: `untag` normalized the pointer; `text_argument` type-checks.
	let Some(text) = (unsafe { text_argument(text_obj) }) else {
		return raise_type_error(&format!(
			"normalize() argument 2 must be str, not {}",
			type_name(text_obj)
		));
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
unsafe extern "C" fn east_asian_width_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
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
		return raise_type_error(&format!(
			"unicodedata.lookup() takes exactly one argument ({} given)",
			args.len()
		));
	}
	let name_obj = untag(args[0]);
	let Some(name) = (unsafe { text_argument(name_obj) }) else {
		return raise_type_error(&format!(
			"lookup() argument must be str, not {}",
			type_name(name_obj)
		));
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
		return raise_type_error(&format!(
			"is_normalized() argument 1 must be str, not {}",
			type_name(form_obj)
		));
	};
	let text_obj = untag(args[1]);
	let Some(text) = (unsafe { text_argument(text_obj) }) else {
		return raise_type_error(&format!(
			"is_normalized() argument 2 must be str, not {}",
			type_name(text_obj)
		));
	};
	match normalize_to_string(&form, &text) {
		Ok(normalized) => crate::types::bool_::from_bool(normalized == text),
		Err(raised) => raised,
	}
}

/// `unicodedata.ucd_3_2_0.category(chr)`.
unsafe extern "C" fn ucd_3_2_category_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	// SAFETY: The runtime call ABI passed `argc` live argument slots.
	match unsafe { char_argument(argv, argc, "category") } {
		Ok(ch) => str_object(ucd_3_2_category_of(ch as u32)),
		Err(error) => error,
	}
}

/// `unicodedata.ucd_3_2_0.bidirectional(chr)`.
unsafe extern "C" fn ucd_3_2_bidirectional_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	// SAFETY: The runtime call ABI passed `argc` live argument slots.
	match unsafe { char_argument(argv, argc, "bidirectional") } {
		Ok(ch) => str_object(ucd_3_2_bidirectional_of(ch as u32)),
		Err(error) => error,
	}
}

/// `unicodedata.ucd_3_2_0.decomposition(chr)`.
unsafe extern "C" fn ucd_3_2_decomposition_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
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
	let function =
		unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
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
	Box::into_raw(Box::new(PyUcd { ob_base: PyObjectHeader::new(ucd_type()), version }))
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
		},
		"bidirectional" => ucd_bound_method(object, "bidirectional", ucd_bidirectional_method),
		"category" => ucd_bound_method(object, "category", ucd_category_method),
		"combining" => ucd_bound_method(object, "combining", ucd_combining_method),
		"decimal" => ucd_bound_method(object, "decimal", ucd_decimal_method),
		"decomposition" => ucd_bound_method(object, "decomposition", ucd_decomposition_method),
		"digit" => ucd_bound_method(object, "digit", ucd_digit_method),
		"east_asian_width" => {
			ucd_bound_method(object, "east_asian_width", ucd_east_asian_width_method)
		},
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
	let function =
		unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
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
		None => Err(raise_type_error(&format!(
			"descriptor '{method}' for 'unicodedata.UCD' objects doesn't apply"
		))),
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

unsafe extern "C" fn ucd_bidirectional_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let (ucd, rest) = match unsafe { ucd_receiver_and_args(argv, argc, "bidirectional") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if ucd.version == UcdVersion::Ucd3_2 {
		return call_unbound(ucd_3_2_bidirectional_entry, rest);
	}
	call_unbound(bidirectional_entry, rest)
}

unsafe extern "C" fn ucd_decomposition_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
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

unsafe extern "C" fn ucd_east_asian_width_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let (_, rest) = match unsafe { ucd_receiver_and_args(argv, argc, "east_asian_width") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	call_unbound(east_asian_width_entry, rest)
}

unsafe extern "C" fn ucd_is_normalized_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
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
		assert_eq!(numeric_value(0x00bd), Some(0.5));
		assert_eq!(numeric_value(0x2460), Some(1.0));
		assert_eq!(ucd_3_2_category_of(u32::from('A')), "Lu");
		assert_eq!(ucd_3_2_category_of(0x0221), "Cn");
		assert_eq!(ucd_3_2_bidirectional_of(u32::from('A')), "L");
		assert_eq!(ucd_3_2_bidirectional_of(0x05d0), "R");
		assert_eq!(ucd_3_2_bidirectional_of(0x0627), "AL");
	}

	fn normalize(form: &str, text: &str) -> String {
		let compat = matches!(form, "NFKC" | "NFKD");
		let mut buf = decompose(text, compat);
		if matches!(form, "NFC" | "NFKC") {
			buf = compose_buffer(&buf);
		}
		buf.into_iter()
			.map(|cp| char::from_u32(cp).unwrap())
			.collect()
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
		assert_eq!(
			normalize("NFD", "-\u{E0}\u{F2}\u{258}\u{141}\u{11F}"),
			"-a\u{300}o\u{300}\u{258}\u{141}g\u{306}"
		);
	}
}
