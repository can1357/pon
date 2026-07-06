//! Integer numeric tower implementation.

use core::{ffi::c_int, ptr};
use std::{
	collections::HashMap,
	sync::{LazyLock, Mutex},
};

use num_bigint::{BigInt, Sign};
use num_traits::{One, Signed, ToPrimitive, Zero};

use crate::{
	abi,
	object::{PyLong, PyNumberMethods, PyObject, PyObjectHeader, PyType},
};

static BIG_INTS: LazyLock<Mutex<HashMap<usize, BigInt>>> =
	LazyLock::new(|| Mutex::new(HashMap::new()));
static INT_NUMBER_METHODS: LazyLock<usize> =
	LazyLock::new(|| Box::into_raw(Box::new(make_number_methods())) as usize);

pub(crate) unsafe extern "C" fn finalize_bigint_shell(object: *mut u8) {
	if object.is_null() {
		return;
	}
	BIG_INTS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.remove(&(object as usize));
}

/// Returns true when `object` has a runtime type whose name bytes match
/// `expected`.
#[must_use]
pub unsafe fn type_name_is(object: *mut PyObject, expected: &str) -> bool {
	if object.is_null() {
		return false;
	}
	let ty = unsafe { (*object).ob_type };
	if ty.is_null() {
		return false;
	}
	let ty = unsafe { &*ty };
	if ty.name.is_null() && ty.name_len != 0 {
		return false;
	}
	let name = unsafe { core::slice::from_raw_parts(ty.name, ty.name_len) };
	name == expected.as_bytes()
}

/// Returns true for exact `int` objects, not for `bool`.
#[must_use]
pub unsafe fn is_exact_int(object: *mut PyObject) -> bool {
	unsafe { type_name_is(object, "int") }
}

/// Extracts the arbitrary-precision integer payload for an exact `int` or an
/// int-subclass instance (IntEnum members, `_NamedIntConstant`, ...), reading
/// the latter through its embedded canonical payload.
#[must_use]
pub unsafe fn to_bigint(object: *mut PyObject) -> Option<BigInt> {
	let object = unsafe { crate::types::type_::payload_subclass_value(object) }
		.map(crate::tag::untag_arg)
		.unwrap_or(object);
	if !unsafe { is_exact_int(object) } {
		return None;
	}
	let key = object as usize;
	if let Some(value) = BIG_INTS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.get(&key)
	{
		return Some(value.clone());
	}
	Some(BigInt::from(unsafe { (*object.cast::<PyLong>()).value }))
}

/// Boxes an arbitrary-precision integer as a `PyLong`.
///
/// Values that fit in the Phase-A inline `i64` payload keep using
/// `pon_const_int`, preserving the existing small-integer path. Wider values
/// are represented by a normal `PyLong` shell plus an out-of-line BigInt
/// payload keyed by object address.
#[must_use]
pub fn from_bigint(value: BigInt) -> *mut PyObject {
	if let Some(inline) = value.to_i64() {
		return unsafe { abi::pon_const_int(inline) };
	}
	let template = unsafe { abi::pon_const_int(0) };
	if template.is_null() {
		return template;
	}
	let ty = unsafe { (*template).ob_type };
	let object = Box::into_raw(Box::new(PyLong { ob_base: PyObjectHeader::new(ty), value: 0 }))
		as *mut PyObject;
	BIG_INTS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.insert(object as usize, value);
	object
}

/// Boxes a signed 64-bit integer through the compatibility constructor.
#[must_use]
pub fn from_i64(value: i64) -> *mut PyObject {
	unsafe { abi::pon_const_int(value) }
}

/// Boxes the value of a compiler-validated integer-literal token wider than
/// `i64` (decimal or `0b`/`0o`/`0x` prefixed, `_` separators allowed).
///
/// Returns NULL with a `ValueError` set when the token does not parse, which
/// only happens for callers bypassing the Python lexer.
#[must_use]
pub fn from_literal_token(text: &str) -> *mut PyObject {
	match parse_int_text(text, 0) {
		Ok(value) => from_bigint(value),
		Err(message) => raise_value_error(&message),
	}
}

/// Extracts an integer payload from exact `int` and `bool` objects.
#[must_use]
pub unsafe fn to_bigint_including_bool(object: *mut PyObject) -> Option<BigInt> {
	if let Some(value) = unsafe { crate::types::bool_::to_bool(object) } {
		return Some(BigInt::from(i32::from(value)));
	}
	unsafe { to_bigint(object) }
}

/// Implements the built-in `int()` constructor once the builtin shim has sliced
/// argv.
#[must_use]
pub fn construct_from_args(args: &[*mut PyObject]) -> *mut PyObject {
	match args.len() {
		0 => from_i64(0),
		1 => unsafe { construct_one(args[0]) },
		2 => unsafe { construct_with_base(args[0], args[1]) },
		len => raise_type_error(&format!("int() expected at most 2 arguments, got {len}")),
	}
}

/// Converts a finite `f64` to the exact integer obtained by truncating toward
/// zero.
#[must_use]
pub fn bigint_from_f64_trunc(value: f64) -> Option<BigInt> {
	if !value.is_finite() {
		return None;
	}
	if value == 0.0 {
		return Some(BigInt::zero());
	}

	let bits = value.to_bits();
	let negative = bits >> 63 != 0;
	let exp_bits = ((bits >> 52) & 0x7ff) as i32;
	let frac = bits & ((1_u64 << 52) - 1);
	let (mantissa, exponent) = if exp_bits == 0 {
		(frac, 1 - 1023 - 52)
	} else {
		((1_u64 << 52) | frac, exp_bits - 1023 - 52)
	};
	let mut value = BigInt::from(mantissa);
	if exponent >= 0 {
		value <<= exponent as usize;
	} else {
		value >>= (-exponent) as usize;
	}
	if negative {
		value = -value;
	}
	Some(value)
}

unsafe fn construct_one(object: *mut PyObject) -> *mut PyObject {
	// `int`/`str`-subclass instances (IntEnum/StrEnum members, ...) convert
	// through their embedded canonical payload (CPython `int(x)` reads the
	// base value of an int subclass).
	let object = unsafe { crate::types::type_::payload_subclass_value(object) }.unwrap_or(object);
	if let Some(value) = unsafe { to_bigint_including_bool(object) } {
		return from_bigint(value);
	}
	if let Some(value) = unsafe { crate::types::float::to_f64(object) } {
		if value.is_nan() {
			return raise_value_error("cannot convert float NaN to integer");
		}
		if value.is_infinite() {
			return raise_value_error("cannot convert float infinity to integer");
		}
		return match bigint_from_f64_trunc(value) {
			Some(value) => from_bigint(value),
			None => raise_value_error("cannot convert float infinity to integer"),
		};
	}
	if let Some(text) = unsafe { crate::types::type_::unicode_text(object) } {
		return match parse_int_text(text, 10) {
			Ok(value) => from_bigint(value),
			Err(message) => raise_value_error(&message),
		};
	}
	if let Some(bytes) = unsafe { bytes_like_slice(object) } {
		return match bytes_like_text(bytes, 10).and_then(|text| parse_int_text(text, 10)) {
			Ok(value) => from_bigint(value),
			Err(message) => raise_value_error(&message),
		};
	}
	// User-defined `__int__`/`__index__` (CPython `PyNumber_Long` nb_int /
	// nb_index legs): `ipaddress.py` module exec runs
	// `packed = int(self.network_address)` over IPv4Address instances.
	for dunder in ["__int__", "__index__"] {
		// SAFETY: Generic attribute lookup tolerates any live object.
		let method = unsafe { crate::abstract_op::get_attr(object, crate::intern::intern(dunder)) };
		if method.is_null() {
			if crate::thread_state::pon_err_occurred() {
				crate::thread_state::pon_err_clear();
			}
			continue;
		}
		// SAFETY: Bound method invoked with zero arguments.
		let result = unsafe { abi::pon_call(method, ptr::null_mut(), 0) };
		if result.is_null() {
			return ptr::null_mut(); // propagate the dunder's exception
		}
		let result = crate::tag::untag_arg(result);
		// Int-subclass results (IntEnum members, ...) read their canonical
		// payload exactly like the receiver pierce above.
		let result = unsafe { crate::types::type_::payload_subclass_value(result) }.unwrap_or(result);
		if let Some(value) = unsafe { to_bigint_including_bool(result) } {
			return from_bigint(value);
		}
		let type_name = unsafe { crate::types::dict::type_name(result) }.unwrap_or("object");
		return raise_type_error(&format!("{dunder} returned non-int (type {type_name})"));
	}
	let type_name = unsafe { crate::types::dict::type_name(object) }.unwrap_or("object");
	raise_type_error(&format!(
		"int() argument must be a string, a bytes-like object or a real number, not '{type_name}'"
	))
}

unsafe fn construct_with_base(object: *mut PyObject, base_object: *mut PyObject) -> *mut PyObject {
	let unicode = unsafe { crate::types::type_::unicode_text(object) };
	let bytes = if unicode.is_none() {
		unsafe { bytes_like_slice(object) }
	} else {
		None
	};
	if unicode.is_none() && bytes.is_none() {
		return raise_type_error("int() can't convert non-string with explicit base");
	}
	let Some(base) =
		(unsafe { to_bigint_including_bool(base_object).and_then(|value| value.to_i32()) })
	else {
		return raise_value_error("int() base must be >= 2 and <= 36, or 0");
	};
	if base != 0 && !(2..=36).contains(&base) {
		return raise_value_error("int() base must be >= 2 and <= 36, or 0");
	}
	let text = match (unicode, bytes) {
		(Some(text), _) => text,
		(None, Some(bytes)) => match bytes_like_text(bytes, base) {
			Ok(text) => text,
			Err(message) => return raise_value_error(&message),
		},
		(None, None) => unreachable!("guarded above"),
	};
	match parse_int_text(text, base) {
		Ok(value) => from_bigint(value),
		Err(message) => raise_value_error(&message),
	}
}

/// Borrows the payload of an exact bytes/bytearray object or bytes payload
/// subclass.
unsafe fn bytes_like_slice<'a>(object: *mut PyObject) -> Option<&'a [u8]> {
	if object.is_null() {
		return None;
	}
	let object = unsafe { crate::types::type_::payload_subclass_value(object) }.unwrap_or(object);
	if !crate::tag::is_heap(object) {
		return None;
	}
	let ty = unsafe { (*object).ob_type };
	if crate::types::bytes_::is_bytes_type(ty) {
		return Some(unsafe { (*object.cast::<crate::types::bytes_::PyBytes>()).as_slice() });
	}
	if crate::types::bytearray_::is_bytearray_type(ty) {
		return Some(unsafe { (*object.cast::<crate::types::bytearray_::PyByteArray>()).as_slice() });
	}
	None
}

/// Decodes an int literal payload from a bytes-like object, mirroring
/// CPython's ASCII requirement for `int(b'...', base)`.
fn bytes_like_text(bytes: &[u8], base: i32) -> Result<&str, String> {
	core::str::from_utf8(bytes)
		.map_err(|_| invalid_int_literal(&crate::types::bytes_::repr(bytes), base))
}

fn parse_int_text(text: &str, requested_base: i32) -> Result<BigInt, String> {
	let trimmed = text.trim();
	let invalid = || invalid_int_literal(text, requested_base);
	if trimmed.is_empty() {
		return Err(invalid());
	}

	let mut rest = trimmed;
	let mut negative = false;
	if let Some(after) = rest.strip_prefix('+') {
		rest = after;
	} else if let Some(after) = rest.strip_prefix('-') {
		negative = true;
		rest = after;
	}
	if rest.is_empty() {
		return Err(invalid());
	}

	let (base, digits, prefixed) = detect_base(rest, requested_base)?;
	let value = parse_digits(digits, base, prefixed).ok_or_else(invalid)?;
	if requested_base == 0 && !prefixed && decimal_base_zero_is_invalid(digits, &value) {
		return Err(invalid());
	}
	Ok(if negative { -value } else { value })
}

fn detect_base(rest: &str, requested_base: i32) -> Result<(u32, &str, bool), String> {
	if requested_base != 0 && !(2..=36).contains(&requested_base) {
		return Err("int() base must be >= 2 and <= 36, or 0".to_owned());
	}

	let lower = rest.as_bytes();
	let prefix_base = if lower.len() >= 2 && lower[0] == b'0' {
		match lower[1].to_ascii_lowercase() {
			b'b' => Some(2),
			b'o' => Some(8),
			b'x' => Some(16),
			_ => None,
		}
	} else {
		None
	};

	match (requested_base, prefix_base) {
		(0, Some(base)) => Ok((base, &rest[2..], true)),
		(0, None) => Ok((10, rest, false)),
		(base, Some(prefix)) if base as u32 == prefix => Ok((prefix, &rest[2..], true)),
		(base, _) => Ok((base as u32, rest, false)),
	}
}

fn parse_digits(digits: &str, base: u32, prefixed: bool) -> Option<BigInt> {
	let mut value = BigInt::zero();
	let mut saw_digit = false;
	let mut previous_digit = false;
	let mut after_prefix = prefixed;
	for ch in digits.chars() {
		if ch == '_' {
			if !previous_digit && !after_prefix {
				return None;
			}
			previous_digit = false;
			after_prefix = false;
			continue;
		}
		let digit = digit_value(ch)?;
		if digit >= base {
			return None;
		}
		value = value * base + digit;
		saw_digit = true;
		previous_digit = true;
		after_prefix = false;
	}
	if !saw_digit || !previous_digit {
		return None;
	}
	Some(value)
}

fn digit_value(ch: char) -> Option<u32> {
	match ch {
		'0'..='9' => Some(u32::from(ch as u8 - b'0')),
		'a'..='z' => Some(u32::from(ch as u8 - b'a') + 10),
		'A'..='Z' => Some(u32::from(ch as u8 - b'A') + 10),
		_ => None,
	}
}

fn decimal_base_zero_is_invalid(digits: &str, value: &BigInt) -> bool {
	digits.starts_with('0') && !value.is_zero()
}

fn invalid_int_literal(text: &str, base: i32) -> String {
	format!("invalid literal for int() with base {base}: {}", python_string_repr(text))
}

fn python_string_repr(text: &str) -> String {
	let mut out = String::with_capacity(text.len() + 2);
	out.push('\'');
	for ch in text.chars() {
		match ch {
			'\\' => out.push_str("\\\\"),
			'\'' => out.push_str("\\'"),
			'\n' => out.push_str("\\n"),
			'\r' => out.push_str("\\r"),
			'\t' => out.push_str("\\t"),
			ch if ch.is_control() => out.push_str(&format!("\\x{:02x}", ch as u32)),
			ch => out.push(ch),
		}
	}
	out.push('\'');
	out
}

/// Installs integer slots on the runtime `int` type reached from an object.
pub unsafe fn install_slots_for_object(object: *mut PyObject) {
	if !unsafe { is_exact_int(object) } {
		return;
	}
	let ty = unsafe { (*object).ob_type.cast_mut() };
	if ty.is_null() {
		return;
	}
	unsafe {
		(*ty).tp_hash = Some(hash_slot);
		(*ty).tp_bool = Some(bool_slot);
		(*ty).tp_as_number = number_methods_ptr();
	}
}

/// Instance attribute surface for exact `int`/`bool` receivers (slotless
/// native types reach here from `abstract_op::get_attr`): `bit_length`/
/// `bit_count`/`__index__`/`__trunc__` bound methods plus the numeric-tower
/// value attributes (`operator.index` calls `a.__index__()` in the vendored
/// stdlib, so the dunder must resolve as an instance attribute).
pub unsafe fn int_instance_attr(object: *mut PyObject, name: u32) -> Option<*mut PyObject> {
	let value = unsafe { to_bigint_including_bool(crate::tag::untag_arg(object)) }?;
	let name_text = crate::intern::resolve(name)?;
	match name_text.as_str() {
		"bit_length" => bound_int_method(object, name, int_bit_length_method),
		"bit_count" => bound_int_method(object, name, int_bit_count_method),
		"to_bytes" => bound_int_method(object, name, int_to_bytes_method),
		"__index__" | "__int__" | "__trunc__" | "__floor__" | "__ceil__" => {
			bound_int_method(object, name, int_identity_method)
		},
		"__add__" => bound_int_method(object, name, int_dunder_add_entry),
		"__format__" => bound_int_method(object, name, int_dunder_format_entry),
		"numerator" | "real" => Some(from_bigint(value)),
		"denominator" => Some(from_bigint(BigInt::from(1))),
		"imag" => Some(from_bigint(BigInt::from(0))),
		_ => None,
	}
}

fn bound_int_method(
	receiver: *mut PyObject,
	name: u32,
	entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Option<*mut PyObject> {
	let function = unsafe {
		crate::abi::pon_make_function(entry as *const u8, crate::builtins::variadic_arity(), name)
	};
	if function.is_null() {
		return Some(core::ptr::null_mut());
	}
	match crate::types::method::new_bound_method(function, receiver) {
		Ok(method) => Some(method.cast::<PyObject>()),
		Err(message) => Some(raise_type_error(&message)),
	}
}

/// One-shot installer for the builtin `int` type object's `tp_dict` method
/// surface, so type-level access resolves through the regular MRO lookup:
/// the unbound `int.bit_length(n)` / `_nbits = int.bit_length` patterns
/// (vendored `_pydecimal` module scope) and `member_type.__format__` (enum).
/// The entries are the bound-path trampolines, which already peel `argv[0]`
/// as the receiver and validate it ([`int_method_receiver`] descriptor
/// shapes), so `descriptor_get` with a NULL instance hands them back
/// unbound while heap int-subclass instances bind them through the MRO.
/// `bool` inherits the surface through its `int` MRO rung (no copies into
/// bool's tp_dict: `bool.bit_length is int.bit_length` in CPython).
/// Existing `tp_dict` entries are kept: only missing names are added.
pub(crate) fn ensure_int_type_methods_installed(ty: *mut PyType) {
	use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
	static INSTALLED: AtomicBool = AtomicBool::new(false);
	if ty.is_null() || INSTALLED.load(AtomicOrdering::SeqCst) {
		return;
	}
	// Pre-runtime call sites must not latch a no-op install: the function
	// allocations below need a live runtime.
	if crate::abi::runtime_type_type().is_null() {
		return;
	}
	if INSTALLED.swap(true, AtomicOrdering::SeqCst) {
		return;
	}
	let namespace = unsafe { (*ty).tp_dict.cast::<crate::types::type_::PyClassDict>() };
	let namespace = if namespace.is_null() {
		crate::types::type_::new_namespace()
	} else {
		namespace
	};
	type Entry = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;
	let natives: &[(&str, Entry)] = &[
		("bit_length", int_bit_length_method),
		("bit_count", int_bit_count_method),
		("to_bytes", int_to_bytes_method),
		("__index__", int_identity_method),
		("__int__", int_identity_method),
		("__trunc__", int_identity_method),
		("__floor__", int_identity_method),
		("__ceil__", int_identity_method),
		("__add__", int_dunder_add_entry),
		("__bool__", int_dunder_bool_entry),
		("__format__", int_dunder_format_entry),
	];
	for (name, entry) in natives {
		let interned = crate::intern::intern(name);
		if unsafe { (&*namespace).get(interned) }.is_some() {
			continue;
		}
		// SAFETY: Live builtin entry points with the runtime calling convention.
		let function = unsafe {
			crate::abi::pon_make_function(
				*entry as *const u8,
				crate::builtins::variadic_arity(),
				interned,
			)
		};
		if !function.is_null() {
			crate::types::function::mark_native_method_descriptor(function);
			unsafe { (&mut *namespace).set(interned, function) };
		}
	}
	unsafe {
		(*ty).tp_dict = namespace.cast::<PyObject>();
	}
	// GC rooting for the namespace values plus IC invalidation for any
	// AttrIC guarding the type object.
	crate::sync::register_namespaced_type(ty);
	crate::sync::type_modified(ty);
}

/// Ensures the GLOBAL `int` type object carries the type-level method
/// surface.  Trigger for receivers that only reach `int` through their MRO
/// (`bool.bit_length` after the bool→int base wiring); mirrors
/// `abi::seq::ensure_list_subclass_surface`.
pub(crate) fn ensure_int_surface_on_global() {
	if let Some(ty) = crate::native::builtins_mod::builtin_native_type("int") {
		ensure_int_type_methods_installed(ty);
	}
}
/// `int.__index__`/`__int__`/`__trunc__`/`__floor__`/`__ceil__`: identity on
/// exact ints (CPython returns self; the runtime's canonical boxing keeps
/// value identity).
unsafe extern "C" fn int_identity_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match unsafe { int_method_receiver(argv, argc, "__index__") } {
		Ok(value) => from_bigint(value),
		Err(error) => error,
	}
}

/// `int.__add__(self, other)` — wrapper-descriptor surface used at import time
/// by `multiprocessing.reduction` (`type(int.__add__)`).  Foreign RHS operands
/// follow CPython and return `NotImplemented`; only receiver/arity mismatches
/// raise.
unsafe extern "C" fn int_dunder_add_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argv.is_null() || argc == 0 {
		return raise_type_error("descriptor '__add__' of 'int' object needs an argument");
	}
	let receiver = unsafe { crate::tag::untag_arg(*argv) };
	let Some(left) = (unsafe { to_bigint_including_bool(receiver) }) else {
		let got = unsafe { crate::types::dict::type_name(receiver) }.unwrap_or("object");
		return raise_type_error(&format!(
			"descriptor '__add__' requires a 'int' object but received a '{got}'"
		));
	};
	if argc != 2 {
		return raise_type_error(&format!("expected 1 argument, got {}", argc.saturating_sub(1)));
	}
	let right = unsafe { crate::tag::untag_arg(*argv.add(1)) };
	let Some(right) = (unsafe { to_bigint_including_bool(right) }) else {
		return unsafe { abi::pon_not_implemented() };
	};
	from_bigint(left + right)
}

/// Receiver/arity validation for the zero-argument int instance methods,
/// shared by the bound path (`(7).bit_length()`) and the unbound
/// type-access path (`int.bit_length(7)` / `_nbits = int.bit_length` in
/// vendored `_pydecimal`).  Error shapes mirror the CPython 3.14
/// method_descriptor oracle byte-for-byte, in CPython's check order:
/// missing receiver, then receiver type (bool and payload int subclasses
/// pass through `to_bigint_including_bool`), then arity.
unsafe fn int_method_receiver(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
) -> Result<BigInt, *mut PyObject> {
	if argv.is_null() || argc == 0 {
		return Err(raise_type_error(&format!("unbound method int.{name}() needs an argument")));
	}
	let receiver = unsafe { crate::tag::untag_arg(*argv) };
	let Some(value) = (unsafe { to_bigint_including_bool(receiver) }) else {
		let got = unsafe { crate::types::dict::type_name(receiver) }.unwrap_or("object");
		return Err(raise_type_error(&format!(
			"descriptor '{name}' for 'int' objects doesn't apply to a '{got}' object"
		)));
	};
	if argc != 1 {
		return Err(raise_type_error(&format!(
			"int.{name}() takes no arguments ({} given)",
			argc - 1
		)));
	}
	Ok(value)
}

/// `int.__bool__(self)`: nonzero test.  The tp_dict seam lets payload int
/// SUBCLASS instances (heap layout, no `nb_bool` slot of their own)
/// truth-test through MRO lookup — `bool(IntSubclass(0))` must be False.
unsafe extern "C" fn int_dunder_bool_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match unsafe { int_method_receiver(argv, argc, "__bool__") } {
		Ok(value) => {
			use num_traits::Zero;
			// SAFETY: Bool constructor returns the shared singletons.
			unsafe { crate::abi::number::pon_const_bool(i32::from(!value.is_zero())) }
		},
		Err(raised) => raised,
	}
}

/// `int.__format__(self, format_spec)` — the real formatting method CPython
/// exposes on `int`, so int SUBCLASS instances format through the int path:
/// enum's `EnumType.__new__` copies `member_type.__format__` into every
/// `IntEnum`/`IntFlag` class dict (vendored enum.py:573-575), and plain
/// `class MyInt(int)` instances resolve it through the MRO ahead of
/// `object.__format__` (whose non-empty-spec TypeError was the pre-fix
/// failure).  The receiver reads through the payload-subclass pierce in
/// `to_bigint_including_bool`, with an `__index__`-protocol fallback for
/// subclass shapes whose canonical payload is not directly readable.
pub(crate) unsafe extern "C" fn int_dunder_format_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	if argv.is_null() || argc != 2 {
		return raise_type_error(&format!(
			"int.__format__() takes exactly one argument ({} given)",
			argc.saturating_sub(1)
		));
	}
	// SAFETY: The caller passes a live argv window of length argc.
	let args = unsafe { core::slice::from_raw_parts(argv, argc) };
	let receiver = crate::tag::untag_arg(args[0]);
	let spec_object = crate::tag::untag_arg(args[1]);
	let Some(spec) = (unsafe { crate::types::type_::unicode_text(spec_object) }) else {
		let got = unsafe { crate::types::dict::type_name(spec_object) }.unwrap_or("object");
		return raise_type_error(&format!("__format__() argument must be str, not {got}"));
	};
	let Some(value) = (unsafe { format_receiver_bigint(receiver) }) else {
		let got = unsafe { crate::types::dict::type_name(receiver) }.unwrap_or("object");
		return raise_type_error(&format!(
			"descriptor '__format__' requires a 'int' object but received a '{got}'"
		));
	};
	if spec.is_empty() {
		// CPython `_PyLong_FormatAdvancedWriter`: an empty spec is `str(self)`
		// WITH subclass `__str__` dispatch (`format(True)` is 'True').
		let Ok(text) = crate::native::builtins_mod::try_str_text(receiver) else {
			return ptr::null_mut();
		};
		// SAFETY: Runtime string allocation helper.
		return unsafe { abi::pon_const_str(text.as_ptr(), text.len()) };
	}
	match crate::abi::format::format_int(&value, spec) {
		// SAFETY: Runtime string allocation helper.
		Ok(text) => unsafe { abi::pon_const_str(text.as_ptr(), text.len()) },
		Err(message) => raise_value_error(&message),
	}
}

/// Integer payload of an `int.__format__` receiver: exact `int`/`bool` and
/// payload-embedding subclasses read directly; anything else falls back to
/// the `__index__` protocol (nb_index slot, then the Python-level dunder) so
/// int-subclass shapes without a readable canonical payload still format.
unsafe fn format_receiver_bigint(receiver: *mut PyObject) -> Option<BigInt> {
	if let Some(value) = unsafe { to_bigint_including_bool(receiver) } {
		return Some(value);
	}
	let ty = unsafe { receiver.as_ref()?.ob_type.as_ref()? };
	if let Some(slot) = unsafe {
		ty.tp_as_number
			.as_ref()
			.and_then(|methods| methods.nb_index)
	} {
		let result = unsafe { slot(receiver) };
		if result.is_null() {
			crate::thread_state::pon_err_clear();
			return None;
		}
		return unsafe { to_bigint_including_bool(crate::tag::untag_arg(result)) };
	}
	let index =
		unsafe { crate::abstract_op::get_attr(receiver, crate::intern::intern("__index__")) };
	if index.is_null() {
		crate::thread_state::pon_err_clear();
		return None;
	}
	let result = unsafe { abi::pon_call(index, ptr::null_mut(), 0) };
	if result.is_null() {
		crate::thread_state::pon_err_clear();
		return None;
	}
	unsafe { to_bigint_including_bool(crate::tag::untag_arg(result)) }
}

unsafe extern "C" fn int_bit_length_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match unsafe { int_method_receiver(argv, argc, "bit_length") } {
		Ok(value) => from_bigint(BigInt::from(value.bits())),
		Err(error) => error,
	}
}

unsafe extern "C" fn int_bit_count_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match unsafe { int_method_receiver(argv, argc, "bit_count") } {
		Ok(value) => from_bigint(BigInt::from(value.magnitude().count_ones())),
		Err(error) => error,
	}
}

/// `int.to_bytes(length=1, byteorder='big', *, signed=False)` — bound
/// instance method: `argv[0]` is the receiver; keyword slots arrive
/// positionally with None filling absent values (`types::function` binder
/// arm).  `importlib._bootstrap_external` calls it at module scope
/// (`MAGIC_NUMBER = (3610).to_bytes(2, 'little')`, pyc header tokens).
unsafe extern "C" fn int_to_bytes_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argv.is_null() || argc == 0 {
		return raise_type_error("unbound method int.to_bytes() needs an argument");
	}
	if argc > 4 {
		return raise_type_error(&format!(
			"to_bytes() takes 0 to 3 arguments ({} given)",
			argc.saturating_sub(1)
		));
	}
	// SAFETY: The caller passes a live argv window of length argc.
	let args: Vec<*mut PyObject> = unsafe { core::slice::from_raw_parts(argv, argc) }
		.iter()
		.map(|&arg| crate::tag::untag_arg(arg))
		.collect();
	let Some(value) = (unsafe { to_bigint_including_bool(args[0]) }) else {
		let got = unsafe { crate::types::dict::type_name(args[0]) }.unwrap_or("object");
		return raise_type_error(&format!(
			"descriptor 'to_bytes' for 'int' objects doesn't apply to a '{got}' object"
		));
	};
	let is_none = |object: *mut PyObject| {
		// SAFETY: Type probe tolerates any live object.
		let name = unsafe { crate::types::dict::type_name(object) };
		name == Some("NoneType")
	};
	let length = match args.get(1).copied().filter(|&len| !is_none(len)) {
		None => 1usize, // length defaults to 1
		Some(len) => {
			let Some(len) = (unsafe { to_bigint_including_bool(len) }) else {
				return raise_type_error("to_bytes() length argument must be an integer");
			};
			match len.to_isize() {
				Some(len) if len >= 0 => len as usize,
				Some(_) => return raise_value_error("length argument must be non-negative"),
				None => return raise_overflow_error("Python int too large to convert to C ssize_t"),
			}
		},
	};
	let little = match args.get(2).copied().filter(|&order| !is_none(order)) {
		None => false, // byteorder defaults to 'big'
		Some(order) => {
			// SAFETY: `unicode_text` type-checks its argument.
			match unsafe { crate::types::type_::unicode_text(order) } {
				Some("big") => false,
				Some("little") => true,
				_ => return raise_value_error("byteorder must be either 'little' or 'big'"),
			}
		},
	};
	let signed = match args.get(3).copied().filter(|&flag| !is_none(flag)) {
		None => false,
		// SAFETY: Truth helper follows the NULL-sentinel error contract.
		Some(flag) => match unsafe { crate::abstract_op::is_true(flag) } {
			negative if negative < 0 => return ptr::null_mut(),
			truth => truth != 0,
		},
	};
	let mut bytes = if value.sign() == Sign::NoSign {
		// Zero fits any width, including `(0).to_bytes(0, ...)` -> b''.
		vec![0u8; length]
	} else if value.sign() == Sign::Minus && !signed {
		return raise_overflow_error("can't convert negative int to unsigned");
	} else if signed {
		let mut le = value.to_signed_bytes_le();
		if le.len() > length {
			return raise_overflow_error("int too big to convert");
		}
		let fill = if value.sign() == Sign::Minus {
			0xff
		} else {
			0x00
		};
		le.resize(length, fill);
		le
	} else {
		let (_, mut le) = value.to_bytes_le();
		if le.len() > length {
			return raise_overflow_error("int too big to convert");
		}
		le.resize(length, 0x00);
		le
	};
	if !little {
		bytes.reverse();
	}
	// SAFETY: Runtime allocation helper; NULL on failure with the error set.
	unsafe { abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) }
}
/// Cached `int.from_bytes` function object served by type-level attribute
/// lookup (`descr::synthetic_type_attr`); classmethod semantics degenerate to
/// a plain function because the type receiver is not passed through.
#[must_use]
pub fn from_bytes_function() -> *mut PyObject {
	static FUNCTION: LazyLock<usize> = LazyLock::new(|| {
		let name = crate::intern::intern("from_bytes");
		// SAFETY: Live builtin entry point with the runtime calling convention.
		let function = unsafe {
			abi::pon_make_function(
				int_from_bytes_entry as *const u8,
				crate::builtins::variadic_arity(),
				name,
			)
		};
		function as usize
	});
	*FUNCTION as *mut PyObject
}

/// `int.from_bytes(bytes, byteorder='big', *, signed=False)`; keyword slots
/// arrive positionally with None filling absent values (`types::function`
/// binder arm), and the `random.py` str-seed path calls it one-argument.
/// The payload accepts CPython's `PyObject_Bytes` universe (bytes-like
/// buffers, `__bytes__` carriers, iterables of ints): `ipaddress.py` feeds
/// `map(cls._parse_octet, octets)` at module exec.
unsafe extern "C" fn int_from_bytes_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argv.is_null() || argc == 0 || argc > 3 {
		return raise_type_error(&format!("from_bytes() takes 1 to 3 arguments ({argc} given)"));
	}
	// SAFETY: The caller passes a live argv window of length argc.
	let args: Vec<*mut PyObject> = unsafe { core::slice::from_raw_parts(argv, argc) }
		.iter()
		.map(|&arg| crate::tag::untag_arg(arg))
		.collect();
	let is_none = |object: *mut PyObject| {
		// SAFETY: Type probe tolerates any live object.
		let name = unsafe { crate::types::dict::type_name(object) };
		name == Some("NoneType")
	};
	let little = match args.get(1).copied().filter(|&order| !is_none(order)) {
		None => false, // byteorder defaults to 'big'
		Some(order) => {
			// SAFETY: `unicode_text` type-checks its argument.
			match unsafe { crate::types::type_::unicode_text(order) } {
				Some("big") => false,
				Some("little") => true,
				_ => return raise_value_error("byteorder must be either 'little' or 'big'"),
			}
		},
	};
	let signed = match args.get(2).copied().filter(|&flag| !is_none(flag)) {
		None => false,
		// SAFETY: Truth helper follows the NULL-sentinel error contract.
		Some(flag) => match unsafe { crate::abstract_op::is_true(flag) } {
			negative if negative < 0 => return ptr::null_mut(),
			truth => truth != 0,
		},
	};
	// Byteorder/signed parse first (CPython argument-clinic order), then
	// the payload conversion with its own diagnostics.
	let payload: Vec<u8> = match crate::abi::str_::bytes_payload_from_object(args[0]) {
		Some(payload) => payload,
		None => return ptr::null_mut(),
	};
	let value = match (little, signed) {
		(false, false) => BigInt::from_bytes_be(Sign::Plus, &payload),
		(true, false) => BigInt::from_bytes_le(Sign::Plus, &payload),
		(false, true) => BigInt::from_signed_bytes_be(&payload),
		(true, true) => BigInt::from_signed_bytes_le(&payload),
	};
	from_bigint(value)
}

/// Returns the integer protocol slot table.
#[must_use]
pub fn number_methods_ptr() -> *mut PyNumberMethods {
	*INT_NUMBER_METHODS as *mut PyNumberMethods
}

/// CPython-style integer hash reduction using the 64-bit `PyHASH_MODULUS`.
#[must_use]
pub fn hash_bigint(value: &BigInt) -> isize {
	const HASH_BITS: usize = 61;
	let modulus = (BigInt::one() << HASH_BITS) - BigInt::one();
	let mut reduced = (value.abs() % &modulus).to_isize().unwrap_or(0);
	if value.sign() == Sign::Minus {
		reduced = -reduced;
	}
	if reduced == -1 { -2 } else { reduced }
}

unsafe extern "C" fn hash_slot(object: *mut PyObject) -> isize {
	match unsafe { to_bigint(object) } {
		Some(value) => hash_bigint(&value),
		None => -1,
	}
}

unsafe extern "C" fn bool_slot(object: *mut PyObject) -> c_int {
	match unsafe { to_bigint(object) } {
		Some(value) if value == BigInt::from(0) => 0,
		Some(_) => 1,
		None => -1,
	}
}

pub unsafe extern "C" fn nb_index(object: *mut PyObject) -> *mut PyObject {
	match unsafe { to_bigint(object) } {
		Some(value) => from_bigint(value),
		None => raise_type_error("object cannot be interpreted as an integer"),
	}
}

pub unsafe extern "C" fn nb_int(object: *mut PyObject) -> *mut PyObject {
	unsafe { nb_index(object) }
}

pub unsafe extern "C" fn nb_float(object: *mut PyObject) -> *mut PyObject {
	match unsafe { to_bigint(object).and_then(|value| value.to_f64()) } {
		Some(value) => crate::types::float::from_f64(value),
		None => raise_type_error("int too large to convert to float"),
	}
}

pub unsafe extern "C" fn nb_add(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
	unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_ADD, a, b) }
}

pub unsafe extern "C" fn nb_subtract(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
	unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_SUB, a, b) }
}

pub unsafe extern "C" fn nb_multiply(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
	unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_MUL, a, b) }
}

pub unsafe extern "C" fn nb_remainder(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
	unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_MOD, a, b) }
}

pub unsafe extern "C" fn nb_negative(object: *mut PyObject) -> *mut PyObject {
	unsafe {
		crate::abi::number::pon_unary_op(crate::abstract_op::UNARY_NEG, object, ptr::null_mut())
	}
}

pub unsafe extern "C" fn nb_absolute(object: *mut PyObject) -> *mut PyObject {
	match unsafe { to_bigint(object) } {
		Some(value) => from_bigint(value.abs()),
		None => raise_type_error("bad operand type for abs()"),
	}
}

pub unsafe extern "C" fn nb_positive(object: *mut PyObject) -> *mut PyObject {
	unsafe {
		crate::abi::number::pon_unary_op(crate::abstract_op::UNARY_POS, object, ptr::null_mut())
	}
}

pub unsafe extern "C" fn nb_invert(object: *mut PyObject) -> *mut PyObject {
	unsafe {
		crate::abi::number::pon_unary_op(crate::abstract_op::UNARY_INVERT, object, ptr::null_mut())
	}
}

pub unsafe extern "C" fn nb_lshift(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
	unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_LSHIFT, a, b) }
}

pub unsafe extern "C" fn nb_rshift(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
	unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_RSHIFT, a, b) }
}

pub unsafe extern "C" fn nb_and(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
	unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_AND, a, b) }
}

pub unsafe extern "C" fn nb_xor(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
	unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_XOR, a, b) }
}

pub unsafe extern "C" fn nb_or(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
	unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_OR, a, b) }
}

pub unsafe extern "C" fn nb_floor_divide(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
	unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_FLOORDIV, a, b) }
}

pub unsafe extern "C" fn nb_true_divide(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
	unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_DIV, a, b) }
}

pub unsafe extern "C" fn nb_power(
	a: *mut PyObject,
	b: *mut PyObject,
	_modulo: *mut PyObject,
) -> *mut PyObject {
	unsafe { crate::abi::number::pon_binary_numeric_slot(crate::abstract_op::BINARY_POW, a, b) }
}

fn make_number_methods() -> PyNumberMethods {
	PyNumberMethods {
		nb_add: Some(nb_add),
		nb_subtract: Some(nb_subtract),
		nb_multiply: Some(nb_multiply),
		nb_remainder: Some(nb_remainder),
		nb_power: Some(nb_power),
		nb_negative: Some(nb_negative),
		nb_positive: Some(nb_positive),
		nb_absolute: Some(nb_absolute),
		nb_bool: Some(bool_slot),
		nb_invert: Some(nb_invert),
		nb_lshift: Some(nb_lshift),
		nb_rshift: Some(nb_rshift),
		nb_and: Some(nb_and),
		nb_xor: Some(nb_xor),
		nb_or: Some(nb_or),
		nb_int: Some(nb_int),
		nb_float: Some(nb_float),
		nb_floor_divide: Some(nb_floor_divide),
		nb_true_divide: Some(nb_true_divide),
		nb_index: Some(nb_index),
		nb_reflected_add: Some(nb_add),
		nb_reflected_subtract: Some(nb_subtract),
		nb_reflected_multiply: Some(nb_multiply),
		nb_reflected_remainder: Some(nb_remainder),
		nb_reflected_power: Some(nb_power),
		nb_reflected_lshift: Some(nb_lshift),
		nb_reflected_rshift: Some(nb_rshift),
		nb_reflected_and: Some(nb_and),
		nb_reflected_xor: Some(nb_xor),
		nb_reflected_or: Some(nb_or),
		nb_reflected_floor_divide: Some(nb_floor_divide),
		nb_reflected_true_divide: Some(nb_true_divide),
		..PyNumberMethods::EMPTY
	}
}

fn raise_type_error(message: &str) -> *mut PyObject {
	unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn raise_value_error(message: &str) -> *mut PyObject {
	unsafe { abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) }
}

fn raise_overflow_error(message: &str) -> *mut PyObject {
	crate::abi::exc::raise_kind_error_text(crate::types::exc::ExceptionKind::OverflowError, message)
}
