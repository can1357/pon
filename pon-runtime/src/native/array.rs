//! Native `array` (CT wave 2: buffer-flavored stdlib tests import it at
//! module scope — test_bytes, test_struct, test_io, test_gzip, ...).
//!
//! CPython's `array` is a C extension with no pure-Python fallback.  This
//! seed implements `array.array` over raw native-endian storage: a
//! `Vec<u8>` payload plus a one-byte typecode, with per-typecode element
//! views (`bBuwhHiIlLqQfd`, the full 3.14 set; `u` is 4-byte wchar_t as on
//! every supported host).  Element conversion goes through the runtime's
//! arbitrary-precision integer payloads (`types::int::to_bigint`) so range
//! checks and `OverflowError` texts match CPython even for values that do
//! not fit the inline `i64` fast path.
//!
//! Instances are immortal leaked boxes (the `_contextvars`/`_collections`
//! pattern).  Arrays store no Python object references — the payload is raw
//! numeric bytes — so there is nothing to report as GC roots.
//!
//! Surface: construction from bytes/str/iterables, `append`/`extend`/
//! `insert`/`tolist`/`tobytes`/`frombytes`/`fromlist`/`count`/`index`/
//! `remove`/`pop`/`clear`/`reverse`/`byteswap`, `typecode`/`itemsize`
//! attributes, `len`/index and slice get/set/del, iteration (dedicated
//! iterator type), `in`, value-based rich comparisons, `repr`, truthiness,
//! `_array_reconstructor`, and writable native-endian buffer exports for
//! `memoryview(array)`.

use core::ffi::c_int;
use std::{
	ptr,
	sync::{LazyLock, Mutex},
};

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive};

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{
	abi,
	abstract_op::{RICH_EQ, RICH_GE, RICH_GT, RICH_LE, RICH_LT, RICH_NE},
	intern::intern,
	object::{PyMappingMethods, PyObject, PyObjectHeader, PySequenceMethods, PyType},
	thread_state::pon_err_clear,
	types::{exc::ExceptionKind, slice_::PySlice, type_::unicode_text},
};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

/// Every 3.14 typecode, in `array.typecodes` order.
const TYPECODES: &str = "bBuwhHiIlLqQfd";

// ---------------------------------------------------------------------------
// Layouts

#[repr(C)]
struct PyArrayObject {
	ob_base:  PyObjectHeader,
	/// ASCII typecode, one of [`TYPECODES`].
	typecode: u8,
	/// Native-endian element storage; length is always a multiple of the
	/// typecode's item size.
	data:     Vec<u8>,
}

#[repr(C)]
struct PyArrayIter {
	ob_base: PyObjectHeader,
	array:   *mut PyArrayObject,
	index:   usize,
}

// ---------------------------------------------------------------------------
// Types

static ARRAY_SEQUENCE: PySequenceMethods = PySequenceMethods {
	sq_length: Some(array_len_slot),
	sq_item: Some(array_item_slot),
	sq_ass_item: Some(array_ass_item_slot),
	sq_contains: Some(array_contains_slot),
	..PySequenceMethods::EMPTY
};

static ARRAY_MAPPING: PyMappingMethods = PyMappingMethods {
	mp_length:        Some(array_len_slot),
	mp_subscript:     Some(array_subscript_slot),
	mp_ass_subscript: Some(array_ass_subscript_slot),
};

static ARRAY_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"array.array",
		std::mem::size_of::<PyArrayObject>(),
	);
	ty.tp_base = abi::runtime_global(intern("object"))
		.map_or(ptr::null_mut(), |object| object.cast::<PyType>());
	ty.tp_new = Some(array_new);
	ty.tp_getattro = Some(array_getattro);
	ty.tp_repr = Some(array_repr);
	ty.tp_str = Some(array_repr);
	ty.tp_bool = Some(array_bool);
	ty.tp_iter = Some(array_iter);
	ty.tp_richcmp = Some(array_richcmp_slot);
	ty.tp_as_sequence = ptr::addr_of!(ARRAY_SEQUENCE).cast_mut();
	ty.tp_as_mapping = ptr::addr_of!(ARRAY_MAPPING).cast_mut();
	Box::into_raw(Box::new(ty)) as usize
});

static ARRAY_ITER_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"arrayiterator",
		std::mem::size_of::<PyArrayIter>(),
	);
	ty.tp_iter = Some(identity_iter);
	ty.tp_iternext = Some(array_iter_next);
	Box::into_raw(Box::new(ty)) as usize
});

fn array_type() -> *mut PyType {
	*ARRAY_TYPE as *mut PyType
}

// ---------------------------------------------------------------------------
// Allocation
//
// Instances are immortal leaked boxes.  The registry exists only so the
// allocation pattern stays auditable alongside the other native seeds; array
// payloads are raw bytes, never GC references, so no root reporting is needed.

static REGISTRY: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn alloc_array(typecode: u8, data: Vec<u8>) -> *mut PyObject {
	let object = Box::into_raw(Box::new(PyArrayObject {
		ob_base: PyObjectHeader::new(array_type()),
		typecode,
		data,
	}));
	REGISTRY
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.push(object as usize);
	object.cast::<PyObject>()
}

unsafe fn as_array<'a>(object: *mut PyObject) -> Option<&'a mut PyArrayObject> {
	let object = untag(object);
	if object.is_null() {
		return None;
	}
	// SAFETY: A non-NULL heap object carries a live header.
	if unsafe { (*object).ob_type } != array_type().cast_const() {
		return None;
	}
	// SAFETY: The type check above proved the layout.
	Some(unsafe { &mut *object.cast::<PyArrayObject>() })
}

/// Contiguous writable buffer exported by an array for memoryview/C-API seams.
pub(crate) struct ArrayBufferView {
	/// Pointer to the first payload byte.
	pub data:     *mut u8,
	/// Payload length in bytes.
	pub len:      usize,
	/// PEP-3118/native-memory format code exposed by `memoryview(array)`.
	pub format:   u8,
	/// Width in bytes of one array element.
	pub itemsize: usize,
}

/// Borrows an exact array as a raw contiguous native-endian buffer.
pub(crate) unsafe fn buffer_view(object: *mut PyObject) -> Option<ArrayBufferView> {
	let array = unsafe { as_array(object) }?;
	Some(ArrayBufferView {
		data:     array.data.as_mut_ptr(),
		len:      array.data.len(),
		format:   buffer_format(array.typecode),
		itemsize: item_size(array.typecode),
	})
}

// ---------------------------------------------------------------------------
// Helpers (contextvars idioms)

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

fn alloc_str_object(text: &str) -> *mut PyObject {
	// SAFETY: Runtime allocation helper; NULL on failure with the error set.
	unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn raise_kind(kind: ExceptionKind, message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(kind, message)
}

fn raise_type_error(message: &str) -> *mut PyObject {
	raise_kind(ExceptionKind::TypeError, message)
}

fn raise_value_error(message: &str) -> *mut PyObject {
	raise_kind(ExceptionKind::ValueError, message)
}

fn raise_index_error(message: &str) -> *mut PyObject {
	raise_kind(ExceptionKind::IndexError, message)
}

fn raise_overflow_error(message: &str) -> *mut PyObject {
	raise_kind(ExceptionKind::OverflowError, message)
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

fn bound_method(receiver: *mut PyObject, name: &str, entry: BuiltinFn) -> *mut PyObject {
	// SAFETY: `entry` is a live builtin entry point with the runtime calling
	// convention.
	let function =
		unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	if function.is_null() {
		return ptr::null_mut();
	}
	match crate::types::method::new_bound_method(function, receiver) {
		Ok(method) => method.cast::<PyObject>(),
		Err(message) => fail(message),
	}
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

// ---------------------------------------------------------------------------
// Typecode element views

fn is_typecode(code: u8) -> bool {
	TYPECODES.as_bytes().contains(&code)
}

fn item_size(typecode: u8) -> usize {
	match typecode {
		b'b' | b'B' => 1,
		b'h' | b'H' => 2,
		b'i' | b'I' | b'f' | b'u' | b'w' => 4,
		_ => 8,
	}
}

fn buffer_format(typecode: u8) -> u8 {
	match typecode {
		// CPython exposes both unicode array typecodes through the PEP-3118 `w`
		// format.  `memoryview.tolist()` refuses that format rather than
		// pretending it is an integer array.
		b'u' | b'w' => b'w',
		other => other,
	}
}

/// A converted element as raw native-endian bytes (at most 8).
struct ItemBytes {
	bytes: [u8; 8],
	len:   usize,
}

impl ItemBytes {
	fn new(bytes: &[u8]) -> Self {
		let mut out = Self { bytes: [0; 8], len: bytes.len() };
		out.bytes[..bytes.len()].copy_from_slice(bytes);
		out
	}

	fn as_slice(&self) -> &[u8] {
		&self.bytes[..self.len]
	}
}

/// Signed range check with CPython's per-typecode `OverflowError` texts.
fn checked_signed(value: &BigInt, min: i64, max: i64, what: &str) -> Result<i64, *mut PyObject> {
	match value.to_i64() {
		Some(value) if (min..=max).contains(&value) => Ok(value),
		Some(value) if value < min => {
			Err(raise_overflow_error(&format!("{what} is less than minimum")))
		},
		Some(_) => Err(raise_overflow_error(&format!("{what} is greater than maximum"))),
		None if value.is_negative() => {
			Err(raise_overflow_error(&format!("{what} is less than minimum")))
		},
		None => Err(raise_overflow_error(&format!("{what} is greater than maximum"))),
	}
}

/// Unsigned range check with CPython's `PyLong_AsUnsigned*` error texts.
fn checked_unsigned(value: &BigInt, max: u64, too_large: &str) -> Result<u64, *mut PyObject> {
	if value.is_negative() {
		return Err(raise_overflow_error("can't convert negative value to unsigned int"));
	}
	match value.to_u64() {
		Some(value) if value <= max => Ok(value),
		_ => Err(raise_overflow_error(too_large)),
	}
}

/// Converts one Python value into raw element bytes for `typecode`.
fn value_to_item(typecode: u8, value: *mut PyObject) -> Result<ItemBytes, *mut PyObject> {
	let value = untag(value);
	match typecode {
		b'f' | b'd' => {
			// SAFETY: `value` is a live untagged object.
			let float_value = if let Some(float_value) = unsafe { crate::types::float::to_f64(value) }
			{
				float_value
			} else if let Some(int_value) =
				unsafe { crate::types::int::to_bigint_including_bool(value) }
			{
				let approx = int_value.to_f64().unwrap_or(f64::INFINITY);
				if !approx.is_finite() {
					return Err(raise_overflow_error("int too large to convert to float"));
				}
				approx
			} else {
				return Err(raise_type_error(&format!(
					"must be real number, not {}",
					value_type_name(value)
				)));
			};
			if typecode == b'f' {
				Ok(ItemBytes::new(&(float_value as f32).to_ne_bytes()))
			} else {
				Ok(ItemBytes::new(&float_value.to_ne_bytes()))
			}
		},
		b'u' | b'w' => {
			// SAFETY: `value` is a live untagged object.
			let Some(text) = (unsafe { unicode_text(value) }) else {
				return Err(raise_type_error("array item must be unicode character"));
			};
			let mut chars = text.chars();
			let (Some(ch), None) = (chars.next(), chars.next()) else {
				return Err(raise_type_error("array item must be unicode character"));
			};
			Ok(ItemBytes::new(&(ch as u32).to_ne_bytes()))
		},
		_ => {
			// SAFETY: `value` is a live untagged object.
			let Some(int_value) = (unsafe { crate::types::int::to_bigint_including_bool(value) })
			else {
				return Err(raise_type_error(&format!(
					"'{}' object cannot be interpreted as an integer",
					value_type_name(value)
				)));
			};
			match typecode {
				b'b' => {
					checked_signed(&int_value, i64::from(i8::MIN), i64::from(i8::MAX), "signed char")
						.map(|value| ItemBytes::new(&(value as i8).to_ne_bytes()))
				},
				b'B' => checked_signed(&int_value, 0, i64::from(u8::MAX), "unsigned byte integer")
					.map(|value| ItemBytes::new(&(value as u8).to_ne_bytes())),
				b'h' => checked_signed(
					&int_value,
					i64::from(i16::MIN),
					i64::from(i16::MAX),
					"signed short integer",
				)
				.map(|value| ItemBytes::new(&(value as i16).to_ne_bytes())),
				b'H' => checked_signed(&int_value, 0, i64::from(u16::MAX), "unsigned short")
					.map(|value| ItemBytes::new(&(value as u16).to_ne_bytes())),
				b'i' => checked_signed(
					&int_value,
					i64::from(i32::MIN),
					i64::from(i32::MAX),
					"signed integer",
				)
				.map(|value| ItemBytes::new(&(value as i32).to_ne_bytes())),
				b'I' => checked_unsigned(
					&int_value,
					u64::from(u32::MAX),
					"unsigned int is greater than maximum",
				)
				.map(|value| ItemBytes::new(&(value as u32).to_ne_bytes())),
				b'l' => int_value
					.to_i64()
					.ok_or_else(|| raise_overflow_error("Python int too large to convert to C long"))
					.map(|value| ItemBytes::new(&value.to_ne_bytes())),
				b'q' => int_value
					.to_i64()
					.ok_or_else(|| {
						raise_overflow_error("Python int too large to convert to C long long")
					})
					.map(|value| ItemBytes::new(&value.to_ne_bytes())),
				b'L' => checked_unsigned(
					&int_value,
					u64::MAX,
					"Python int too large to convert to C unsigned long",
				)
				.map(|value| ItemBytes::new(&value.to_ne_bytes())),
				_ => checked_unsigned(
					&int_value,
					u64::MAX,
					"Python int too large to convert to C unsigned long long",
				)
				.map(|value| ItemBytes::new(&value.to_ne_bytes())),
			}
		},
	}
}

/// Boxes the element starting at `offset` as a Python value.
fn item_to_object(array: &PyArrayObject, index: usize) -> *mut PyObject {
	let size = item_size(array.typecode);
	let offset = index * size;
	let bytes = &array.data[offset..offset + size];
	let boxed_i64 = |value: i64| -> *mut PyObject {
		// SAFETY: Runtime allocation helper.
		unsafe { abi::pon_const_int(value) }
	};
	match array.typecode {
		b'b' => boxed_i64(i64::from(i8::from_ne_bytes([bytes[0]]))),
		b'B' => boxed_i64(i64::from(bytes[0])),
		b'h' => boxed_i64(i64::from(i16::from_ne_bytes([bytes[0], bytes[1]]))),
		b'H' => boxed_i64(i64::from(u16::from_ne_bytes([bytes[0], bytes[1]]))),
		b'i' => boxed_i64(i64::from(i32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))),
		b'I' => boxed_i64(i64::from(u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))),
		b'l' | b'q' => {
			let mut raw = [0u8; 8];
			raw.copy_from_slice(bytes);
			boxed_i64(i64::from_ne_bytes(raw))
		},
		b'L' | b'Q' => {
			let mut raw = [0u8; 8];
			raw.copy_from_slice(bytes);
			let value = u64::from_ne_bytes(raw);
			match i64::try_from(value) {
				Ok(value) => boxed_i64(value),
				Err(_) => crate::types::int::from_bigint(BigInt::from(value)),
			}
		},
		b'f' => {
			let mut raw = [0u8; 4];
			raw.copy_from_slice(bytes);
			// SAFETY: Runtime allocation helper.
			unsafe { abi::number::pon_const_float(f64::from(f32::from_ne_bytes(raw))) }
		},
		b'd' => {
			let mut raw = [0u8; 8];
			raw.copy_from_slice(bytes);
			// SAFETY: Runtime allocation helper.
			unsafe { abi::number::pon_const_float(f64::from_ne_bytes(raw)) }
		},
		_ => {
			let mut raw = [0u8; 4];
			raw.copy_from_slice(bytes);
			match char::from_u32(u32::from_ne_bytes(raw)) {
				Some(ch) => alloc_str_object(&ch.to_string()),
				None => raise_value_error("character U+110000 is not in range [U+0000; U+10FFFF]"),
			}
		},
	}
}

fn element_count(array: &PyArrayObject) -> usize {
	array.data.len() / item_size(array.typecode)
}

/// Drains a Python iterable, converting every element for `typecode`.
/// On `Err` the exception is already set.
fn collect_converted(typecode: u8, iterable: *mut PyObject) -> Result<Vec<u8>, ()> {
	// SAFETY: Iterator helpers follow the NULL-sentinel error contract.
	let iterator = unsafe { abi::pon_get_iter(iterable, ptr::null_mut()) };
	if iterator.is_null() {
		return Err(());
	}
	let mut out = Vec::new();
	loop {
		// SAFETY: `iterator` is live; NULL signals exhaustion or error.
		let item = unsafe { abi::pon_iter_next(iterator, ptr::null_mut()) };
		if item.is_null() {
			if abi::exc::pending_exception_is("StopIteration") {
				pon_err_clear();
				break;
			}
			if crate::thread_state::pon_err_occurred() {
				return Err(());
			}
			break;
		}
		match value_to_item(typecode, item) {
			Ok(item_bytes) => out.extend_from_slice(item_bytes.as_slice()),
			Err(_raised) => return Err(()),
		}
	}
	Ok(out)
}

/// Borrows a bytes-like initializer payload (bytes or bytearray).
fn bytes_like<'a>(object: *mut PyObject) -> Option<&'a [u8]> {
	if object.is_null() {
		return None;
	}
	// SAFETY: A non-NULL heap object carries a live header.
	let ty = unsafe { (*object).ob_type };
	if crate::types::bytes_::is_bytes_type(ty) {
		// SAFETY: Type check above proved the layout.
		return Some(unsafe { (*object.cast::<crate::types::bytes_::PyBytes>()).as_slice() });
	}
	if crate::types::bytearray_::is_bytearray_type(ty) {
		// SAFETY: Type check above proved the layout.
		return Some(unsafe { (*object.cast::<crate::types::bytearray_::PyByteArray>()).as_slice() });
	}
	None
}

/// Appends a bytes-like payload, enforcing CPython's item-size divisibility.
fn extend_from_bytes(array: &mut PyArrayObject, payload: &[u8]) -> Result<(), *mut PyObject> {
	if payload.len() % item_size(array.typecode) != 0 {
		return Err(raise_value_error("bytes length not a multiple of item size"));
	}
	array.data.extend_from_slice(payload);
	Ok(())
}

/// Normalizes a possibly-negative index; `Err` carries the raised exception.
fn normalize_index(index: isize, len: usize, message: &str) -> Result<usize, *mut PyObject> {
	let normalized = if index < 0 {
		index + len as isize
	} else {
		index
	};
	if normalized < 0 || normalized as usize >= len {
		return Err(raise_index_error(message));
	}
	Ok(normalized as usize)
}

// ---------------------------------------------------------------------------
// Slots

unsafe extern "C" fn array_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	let positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
		Ok(positional) => positional,
		Err(message) => return fail(message),
	};
	if !kwargs.is_null() {
		let entries = match unsafe { crate::types::dict::dict_entries_snapshot(kwargs) } {
			Ok(entries) => entries,
			Err(message) => return fail(message),
		};
		if !entries.is_empty() {
			return raise_type_error("array.array() takes no keyword arguments");
		}
	}
	if positional.is_empty() {
		return raise_type_error("array() takes at least 1 argument (0 given)");
	}
	if positional.len() > 2 {
		return raise_type_error(&format!(
			"array() takes at most 2 arguments ({} given)",
			positional.len()
		));
	}
	let code_obj = untag(positional[0]);
	// SAFETY: `untag` normalized the pointer; `unicode_text` type-checks.
	let Some(code_text) = (unsafe { unicode_text(code_obj) }) else {
		return raise_type_error(&format!(
			"array() argument 1 must be a unicode character, not {}",
			value_type_name(code_obj)
		));
	};
	let mut chars = code_text.chars();
	let (Some(code_char), None) = (chars.next(), chars.next()) else {
		return raise_type_error("array() argument 1 must be a unicode character");
	};
	if !code_char.is_ascii() || !is_typecode(code_char as u8) {
		return raise_value_error(
			"bad typecode (must be b, B, u, w, h, H, i, I, l, L, q, Q, f or d)",
		);
	}
	let typecode = code_char as u8;
	let object = alloc_array(typecode, Vec::new());
	let Some(array) = (unsafe { as_array(object) }) else {
		return fail("array allocation failed");
	};
	let Some(initializer) = positional.get(1).copied().map(untag) else {
		return object;
	};
	if initializer == none() {
		// CPython rejects None initializers with the iterable TypeError.
		return raise_type_error("'NoneType' object is not iterable");
	}
	if let Some(payload) = bytes_like(initializer) {
		return match extend_from_bytes(array, payload) {
			Ok(()) => object,
			Err(raised) => raised,
		};
	}
	// SAFETY: `initializer` is a live untagged object.
	if let Some(text) = unsafe { unicode_text(initializer) } {
		if typecode != b'u' && typecode != b'w' {
			return raise_type_error(&format!(
				"cannot use a str to initialize an array with typecode '{}'",
				char::from(typecode)
			));
		}
		for ch in text.chars() {
			array.data.extend_from_slice(&(ch as u32).to_ne_bytes());
		}
		return object;
	}
	if let Some(other) = unsafe { as_array(initializer) } {
		if other.typecode == typecode {
			array.data.extend_from_slice(&other.data);
			return object;
		}
		// Cross-typecode construction converts element by element.
		let count = element_count(other);
		for index in 0..count {
			let boxed = item_to_object(other, index);
			if boxed.is_null() {
				return ptr::null_mut();
			}
			match value_to_item(typecode, boxed) {
				Ok(item_bytes) => array.data.extend_from_slice(item_bytes.as_slice()),
				Err(raised) => return raised,
			}
		}
		return object;
	}
	match collect_converted(typecode, initializer) {
		Ok(converted) => {
			array.data.extend_from_slice(&converted);
			object
		},
		Err(()) => ptr::null_mut(),
	}
}

unsafe extern "C" fn array_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	// SAFETY: `untag` normalized the pointer; `unicode_text` type-checks.
	let Some(name_text) = (unsafe { unicode_text(untag(name)) }) else {
		return fail("attribute name must be str");
	};
	let Some(array) = (unsafe { as_array(object) }) else {
		return fail("array receiver is invalid");
	};
	match name_text {
		"typecode" => alloc_str_object(&char::from(array.typecode).to_string()),
		// SAFETY: Runtime allocation helper.
		"itemsize" => unsafe { abi::pon_const_int(item_size(array.typecode) as i64) },
		"append" => bound_method(object, name_text, array_append_method),
		"extend" => bound_method(object, name_text, array_extend_method),
		"insert" => bound_method(object, name_text, array_insert_method),
		"tolist" => bound_method(object, name_text, array_tolist_method),
		"tobytes" => bound_method(object, name_text, array_tobytes_method),
		"frombytes" => bound_method(object, name_text, array_frombytes_method),
		"fromlist" => bound_method(object, name_text, array_fromlist_method),
		"count" => bound_method(object, name_text, array_count_method),
		"index" => bound_method(object, name_text, array_index_method),
		"remove" => bound_method(object, name_text, array_remove_method),
		"pop" => bound_method(object, name_text, array_pop_method),
		"clear" => bound_method(object, name_text, array_clear_method),
		"reverse" => bound_method(object, name_text, array_reverse_method),
		"byteswap" => bound_method(object, name_text, array_byteswap_method),
		// SAFETY: Raise helper with the interned attribute name.
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

unsafe extern "C" fn array_repr(object: *mut PyObject) -> *mut PyObject {
	let Some(array) = (unsafe { as_array(object) }) else {
		return fail("array receiver is invalid");
	};
	let typecode = char::from(array.typecode);
	if array.data.is_empty() {
		return alloc_str_object(&format!("array('{typecode}')"));
	}
	let mut out = format!("array('{typecode}', ");
	if array.typecode == b'u' || array.typecode == b'w' {
		// CPython reprs unicode arrays as a string literal.
		let mut text = String::with_capacity(element_count(array));
		for index in 0..element_count(array) {
			let size = item_size(array.typecode);
			let mut raw = [0u8; 4];
			raw.copy_from_slice(&array.data[index * size..(index + 1) * size]);
			text.push(char::from_u32(u32::from_ne_bytes(raw)).unwrap_or('\u{fffd}'));
		}
		let boxed = alloc_str_object(&text);
		if boxed.is_null() {
			return ptr::null_mut();
		}
		out.push_str(&super::builtins_mod::repr_text(boxed));
	} else {
		out.push('[');
		for index in 0..element_count(array) {
			if index > 0 {
				out.push_str(", ");
			}
			let boxed = item_to_object(array, index);
			if boxed.is_null() {
				return ptr::null_mut();
			}
			out.push_str(&super::builtins_mod::repr_text(boxed));
		}
		out.push(']');
	}
	out.push(')');
	alloc_str_object(&out)
}

unsafe extern "C" fn array_bool(object: *mut PyObject) -> c_int {
	match unsafe { as_array(object) } {
		Some(array) => c_int::from(!array.data.is_empty()),
		None => -1,
	}
}

unsafe extern "C" fn array_len_slot(object: *mut PyObject) -> isize {
	match unsafe { as_array(object) } {
		Some(array) => element_count(array) as isize,
		None => -1,
	}
}

unsafe extern "C" fn array_item_slot(object: *mut PyObject, index: isize) -> *mut PyObject {
	let Some(array) = (unsafe { as_array(object) }) else {
		return fail("array receiver is invalid");
	};
	match normalize_index(index, element_count(array), "array index out of range") {
		Ok(index) => item_to_object(array, index),
		Err(raised) => raised,
	}
}

unsafe extern "C" fn array_ass_item_slot(
	object: *mut PyObject,
	index: isize,
	value: *mut PyObject,
) -> c_int {
	let Some(array) = (unsafe { as_array(object) }) else {
		let _ = fail("array receiver is invalid");
		return -1;
	};
	let size = item_size(array.typecode);
	if value.is_null() {
		// Deletion (`del arr[i]`).
		match normalize_index(index, element_count(array), "array assignment index out of range") {
			Ok(index) => {
				array.data.drain(index * size..(index + 1) * size);
				0
			},
			Err(_raised) => -1,
		}
	} else {
		let index = match normalize_index(
			index,
			element_count(array),
			"array assignment index out of range",
		) {
			Ok(index) => index,
			Err(_raised) => return -1,
		};
		match value_to_item(array.typecode, value) {
			Ok(item_bytes) => {
				array.data[index * size..(index + 1) * size].copy_from_slice(item_bytes.as_slice());
				0
			},
			Err(_raised) => -1,
		}
	}
}

fn array_slice_data(array: &PyArrayObject, indices: crate::types::slice_::SliceIndices) -> Vec<u8> {
	let size = item_size(array.typecode);
	if indices.len == 0 {
		return Vec::new();
	}
	if indices.step == 1 {
		let start = indices.start as usize * size;
		let stop = indices.stop as usize * size;
		return array.data[start..stop].to_vec();
	}
	let mut out = Vec::with_capacity(indices.len * size);
	let mut index = indices.start;
	for _ in 0..indices.len {
		let offset = index as usize * size;
		out.extend_from_slice(&array.data[offset..offset + size]);
		index = index.saturating_add(indices.step);
	}
	out
}

fn raise_slice_error(message: String) -> *mut PyObject {
	if message == "slice step cannot be zero" {
		raise_value_error(&message)
	} else {
		raise_type_error(&message)
	}
}

unsafe extern "C" fn array_subscript_slot(
	object: *mut PyObject,
	key: *mut PyObject,
) -> *mut PyObject {
	let Some(array) = (unsafe { as_array(object) }) else {
		return fail("array receiver is invalid");
	};
	if crate::abi::seq::is_slice(key) {
		let indices = match crate::abi::seq::normalize_slice(
			unsafe { &*key.cast::<PySlice>() },
			element_count(array),
		) {
			Ok(indices) => indices,
			Err(message) => return raise_slice_error(message),
		};
		return alloc_array(array.typecode, array_slice_data(array, indices));
	}
	let Some(raw) = (unsafe { crate::types::int::to_i64_including_bool(untag(key)) }) else {
		return raise_type_error("array indices must be integers or slices");
	};
	let Ok(index) = isize::try_from(raw) else {
		return raise_type_error("array index is out of range for this platform");
	};
	unsafe { array_item_slot(object, index) }
}

fn array_delete_slice(array: &mut PyArrayObject, indices: crate::types::slice_::SliceIndices) {
	let size = item_size(array.typecode);
	if indices.step == 1 {
		let start = indices.start as usize * size;
		let stop = indices.stop as usize * size;
		array.data.drain(start..stop);
		return;
	}
	let mut positions = Vec::with_capacity(indices.len);
	let mut index = indices.start;
	for _ in 0..indices.len {
		positions.push(index as usize);
		index = index.saturating_add(indices.step);
	}
	positions.sort_unstable_by(|left, right| right.cmp(left));
	for index in positions {
		let offset = index * size;
		array.data.drain(offset..offset + size);
	}
}

fn array_assign_slice(
	array: &mut PyArrayObject,
	indices: crate::types::slice_::SliceIndices,
	replacement: Vec<u8>,
) -> Result<(), String> {
	let size = item_size(array.typecode);
	let replacement_len = replacement.len() / size;
	if indices.step == 1 {
		let start = indices.start as usize * size;
		let stop = indices.stop as usize * size;
		array.data.splice(start..stop, replacement);
		return Ok(());
	}
	if replacement_len != indices.len {
		return Err(format!(
			"attempt to assign array of size {} to extended slice of size {}",
			replacement_len, indices.len
		));
	}
	let mut index = indices.start;
	for chunk in replacement.chunks_exact(size) {
		let offset = index as usize * size;
		array.data[offset..offset + size].copy_from_slice(chunk);
		index = index.saturating_add(indices.step);
	}
	Ok(())
}

unsafe extern "C" fn array_ass_subscript_slot(
	object: *mut PyObject,
	key: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	if crate::abi::seq::is_slice(key) {
		let (replacement_typecode, replacement) = if value.is_null() {
			(0, Vec::new())
		} else {
			let Some(other) = (unsafe { as_array(value) }) else {
				let _ = raise_type_error(&format!(
					"can only assign array (not \"{}\") to array slice",
					value_type_name(untag(value))
				));
				return -1;
			};
			(other.typecode, other.data.clone())
		};
		let Some(array) = (unsafe { as_array(object) }) else {
			let _ = fail("array receiver is invalid");
			return -1;
		};
		if !value.is_null() && replacement_typecode != array.typecode {
			let _ = raise_type_error("bad argument type for built-in operation");
			return -1;
		}
		let indices = match crate::abi::seq::normalize_slice(
			unsafe { &*key.cast::<PySlice>() },
			element_count(array),
		) {
			Ok(indices) => indices,
			Err(message) => {
				let _ = raise_slice_error(message);
				return -1;
			},
		};
		if value.is_null() {
			array_delete_slice(array, indices);
			return 0;
		}
		match array_assign_slice(array, indices, replacement) {
			Ok(()) => 0,
			Err(message) if message == "slice step cannot be zero" => {
				let _ = raise_value_error(&message);
				-1
			},
			Err(message) => {
				let _ = raise_value_error(&message);
				-1
			},
		}
	} else {
		let Some(raw) = (unsafe { crate::types::int::to_i64_including_bool(untag(key)) }) else {
			let _ = raise_type_error("array indices must be integers");
			return -1;
		};
		let Ok(index) = isize::try_from(raw) else {
			let _ = raise_type_error("array index is out of range for this platform");
			return -1;
		};
		unsafe { array_ass_item_slot(object, index, value) }
	}
}

unsafe extern "C" fn identity_iter(object: *mut PyObject) -> *mut PyObject {
	object
}

unsafe extern "C" fn array_iter(object: *mut PyObject) -> *mut PyObject {
	if unsafe { as_array(object) }.is_none() {
		return fail("array receiver is invalid");
	}
	let iter = Box::into_raw(Box::new(PyArrayIter {
		ob_base: PyObjectHeader::new(*ARRAY_ITER_TYPE as *mut PyType),
		array:   untag(object).cast::<PyArrayObject>(),
		index:   0,
	}));
	iter.cast::<PyObject>()
}

unsafe extern "C" fn array_iter_next(object: *mut PyObject) -> *mut PyObject {
	let object = untag(object);
	if object.is_null() {
		return fail("array iterator receiver is NULL");
	}
	// SAFETY: Receiver is a live PyArrayIter allocated by `array_iter`.
	let iter = unsafe { &mut *object.cast::<PyArrayIter>() };
	// SAFETY: The referenced array is an immortal leaked box.
	let array = unsafe { &*iter.array };
	if iter.index < element_count(array) {
		let value = item_to_object(array, iter.index);
		iter.index += 1;
		value
	} else {
		// Typed StopIteration: consumers distinguish exhaustion from failure
		// by the pending exception's type.
		// SAFETY: Raise helper follows the NULL-sentinel contract.
		unsafe { abi::exc::pon_raise_stop_iteration(ptr::null_mut()) }
	}
}

/// `==` through the runtime rich comparison; identity short-circuits first.
fn value_equal(lhs: *mut PyObject, rhs: *mut PyObject) -> Result<bool, ()> {
	if lhs == rhs {
		return Ok(true);
	}
	// SAFETY: Comparison helper follows the NULL-sentinel error contract.
	let result = unsafe { crate::abstract_op::rich_compare(RICH_EQ, lhs, rhs) };
	if result.is_null() {
		return Err(());
	}
	// SAFETY: Truthiness helper follows the error-sentinel contract.
	match unsafe { abi::pon_is_true(result) } {
		0 => Ok(false),
		1 => Ok(true),
		_ => Err(()),
	}
}

/// `tp_richcmp` for array: lexicographic value comparison against another
/// array (cross-typecode, like CPython's boxed-item comparison); everything
/// else is NotImplemented so the dispatcher applies identity/reflected
/// fallbacks.
fn value_compare_truth(lhs: *mut PyObject, rhs: *mut PyObject, op: u8) -> Result<bool, ()> {
	if op == RICH_EQ {
		return value_equal(lhs, rhs);
	}
	// SAFETY: Comparison helper follows the NULL-sentinel error contract.
	let result = unsafe { crate::abstract_op::rich_compare(op, lhs, rhs) };
	if result.is_null() {
		return Err(());
	}
	// SAFETY: Truthiness helper follows the error-sentinel contract.
	match unsafe { abi::pon_is_true(result) } {
		0 => Ok(false),
		1 => Ok(true),
		_ => Err(()),
	}
}

fn length_compare(op: u8, lhs: usize, rhs: usize) -> bool {
	match op {
		RICH_LT => lhs < rhs,
		RICH_LE => lhs <= rhs,
		RICH_EQ => lhs == rhs,
		RICH_NE => lhs != rhs,
		RICH_GT => lhs > rhs,
		RICH_GE => lhs >= rhs,
		_ => false,
	}
}

fn lexicographic_array_compare(
	lhs: &PyArrayObject,
	rhs: &PyArrayObject,
	op: u8,
) -> Result<bool, ()> {
	if core::ptr::eq(lhs, rhs) {
		return Ok(length_compare(op, 0, 0));
	}
	let lhs_len = element_count(lhs);
	let rhs_len = element_count(rhs);
	if lhs.typecode == rhs.typecode && lhs.data == rhs.data {
		return Ok(length_compare(op, lhs_len, rhs_len));
	}
	let shared = lhs_len.min(rhs_len);
	for index in 0..shared {
		let a = item_to_object(lhs, index);
		let b = item_to_object(rhs, index);
		if a.is_null() || b.is_null() {
			return Err(());
		}
		if !value_equal(a, b)? {
			return match op {
				RICH_EQ => Ok(false),
				RICH_NE => Ok(true),
				RICH_LT | RICH_LE => value_compare_truth(a, b, RICH_LT),
				RICH_GT | RICH_GE => value_compare_truth(a, b, RICH_GT),
				_ => Ok(false),
			};
		}
	}
	Ok(length_compare(op, lhs_len, rhs_len))
}

unsafe extern "C" fn array_richcmp_slot(
	left: *mut PyObject,
	right: *mut PyObject,
	op: c_int,
) -> *mut PyObject {
	let Ok(op) = u8::try_from(op) else {
		// SAFETY: Singleton accessor.
		return unsafe { abi::pon_not_implemented() };
	};
	if !matches!(op, RICH_LT | RICH_LE | RICH_EQ | RICH_NE | RICH_GT | RICH_GE) {
		// SAFETY: Singleton accessor.
		return unsafe { abi::pon_not_implemented() };
	}
	let (Some(_), Some(_)) = (unsafe { as_array(left) }, unsafe { as_array(right) }) else {
		// SAFETY: Singleton accessor.
		return unsafe { abi::pon_not_implemented() };
	};
	let lhs = untag(left).cast::<PyArrayObject>();
	let rhs = untag(right).cast::<PyArrayObject>();
	// SAFETY: Both layouts were proved by `as_array` above.
	let ordered = unsafe { lexicographic_array_compare(&*lhs, &*rhs, op) };
	match ordered {
		Ok(value) => unsafe { abi::pon_const_bool(c_int::from(value)) },
		Err(()) => ptr::null_mut(),
	}
}

/// `sq_contains`: linear value-equality scan (1 found, 0 absent, -1 error).
unsafe extern "C" fn array_contains_slot(object: *mut PyObject, item: *mut PyObject) -> c_int {
	let Some(array) = (unsafe { as_array(object) }) else {
		let _ = fail("array receiver is invalid");
		return -1;
	};
	for index in 0..element_count(array) {
		let boxed = item_to_object(array, index);
		if boxed.is_null() {
			return -1;
		}
		match value_equal(boxed, untag(item)) {
			Ok(true) => return 1,
			Ok(false) => {},
			Err(()) => return -1,
		}
	}
	0
}

// ---------------------------------------------------------------------------
// Methods

/// Shared receiver/argument prologue for array methods.
unsafe fn array_receiver_and_args<'a>(
	argv: *mut *mut PyObject,
	argc: usize,
	method: &str,
) -> Result<(&'a mut PyArrayObject, &'a [*mut PyObject]), *mut PyObject> {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return Err(raise_type_error(&format!("array.{method}() received a null argv pointer")));
	};
	let Some((receiver, rest)) = args.split_first() else {
		return Err(raise_type_error(&format!("array.{method}() missing its receiver")));
	};
	let Some(array) = (unsafe { as_array(*receiver) }) else {
		return Err(raise_type_error(&format!("array.{method}() receiver is not an array")));
	};
	Ok((array, rest))
}

fn expect_args<'a>(
	rest: &'a [*mut PyObject],
	count: usize,
	method: &str,
) -> Result<&'a [*mut PyObject], *mut PyObject> {
	if rest.len() != count {
		return Err(raise_type_error(&format!(
			"array.{method}() takes exactly {count} argument{} ({} given)",
			if count == 1 { "" } else { "s" },
			rest.len()
		)));
	}
	Ok(rest)
}

unsafe extern "C" fn array_append_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (array, rest) = match unsafe { array_receiver_and_args(argv, argc, "append") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	let rest = match expect_args(rest, 1, "append") {
		Ok(rest) => rest,
		Err(raised) => return raised,
	};
	match value_to_item(array.typecode, rest[0]) {
		Ok(item_bytes) => {
			array.data.extend_from_slice(item_bytes.as_slice());
			none()
		},
		Err(raised) => raised,
	}
}

unsafe extern "C" fn array_extend_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (array, rest) = match unsafe { array_receiver_and_args(argv, argc, "extend") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	let rest = match expect_args(rest, 1, "extend") {
		Ok(rest) => rest,
		Err(raised) => return raised,
	};
	let other = untag(rest[0]);
	if let Some(other_array) = unsafe { as_array(other) } {
		if other_array.typecode != array.typecode {
			return raise_type_error("can only extend with array of same kind");
		}
		// Self-extend snapshots via the copied Vec either way.
		let payload = other_array.data.clone();
		array.data.extend_from_slice(&payload);
		return none();
	}
	match collect_converted(array.typecode, other) {
		Ok(converted) => {
			array.data.extend_from_slice(&converted);
			none()
		},
		Err(()) => ptr::null_mut(),
	}
}

unsafe extern "C" fn array_insert_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (array, rest) = match unsafe { array_receiver_and_args(argv, argc, "insert") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	let rest = match expect_args(rest, 2, "insert") {
		Ok(rest) => rest,
		Err(raised) => return raised,
	};
	let Some(position) = int_of(untag(rest[0])) else {
		return raise_type_error(&format!(
			"'{}' object cannot be interpreted as an integer",
			value_type_name(untag(rest[0]))
		));
	};
	let len = element_count(array) as i64;
	// CPython clamps insert positions instead of raising.
	let clamped = position.clamp(-len, len);
	let index = if clamped < 0 {
		(clamped + len) as usize
	} else {
		clamped as usize
	};
	match value_to_item(array.typecode, rest[1]) {
		Ok(item_bytes) => {
			let size = item_size(array.typecode);
			let offset = index * size;
			let tail = array.data.split_off(offset);
			array.data.extend_from_slice(item_bytes.as_slice());
			array.data.extend_from_slice(&tail);
			none()
		},
		Err(raised) => raised,
	}
}

unsafe extern "C" fn array_tolist_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (array, rest) = match unsafe { array_receiver_and_args(argv, argc, "tolist") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if let Err(raised) = expect_args(rest, 0, "tolist") {
		return raised;
	}
	let mut items = Vec::with_capacity(element_count(array));
	for index in 0..element_count(array) {
		let boxed = item_to_object(array, index);
		if boxed.is_null() {
			return ptr::null_mut();
		}
		items.push(boxed);
	}
	// SAFETY: `items` holds live object slots for the whole call.
	unsafe { abi::seq::pon_build_list(items.as_mut_ptr(), items.len()) }
}

unsafe extern "C" fn array_tobytes_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (array, rest) = match unsafe { array_receiver_and_args(argv, argc, "tobytes") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if let Err(raised) = expect_args(rest, 0, "tobytes") {
		return raised;
	}
	// SAFETY: Runtime allocation helper.
	unsafe { abi::str_::pon_const_bytes(array.data.as_ptr(), array.data.len()) }
}

unsafe extern "C" fn array_frombytes_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let (array, rest) = match unsafe { array_receiver_and_args(argv, argc, "frombytes") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	let rest = match expect_args(rest, 1, "frombytes") {
		Ok(rest) => rest,
		Err(raised) => return raised,
	};
	let Some(payload) = bytes_like(untag(rest[0])) else {
		return raise_type_error(&format!(
			"a bytes-like object is required, not '{}'",
			value_type_name(untag(rest[0]))
		));
	};
	match extend_from_bytes(array, payload) {
		Ok(()) => none(),
		Err(raised) => raised,
	}
}

unsafe extern "C" fn array_fromlist_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (array, rest) = match unsafe { array_receiver_and_args(argv, argc, "fromlist") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	let rest = match expect_args(rest, 1, "fromlist") {
		Ok(rest) => rest,
		Err(raised) => return raised,
	};
	let other = untag(rest[0]);
	if value_type_name(other) != "list" {
		return raise_type_error("arg must be list");
	}
	// Convert into a scratch buffer first: CPython restores the original
	// size when any element fails to convert.
	match collect_converted(array.typecode, other) {
		Ok(converted) => {
			array.data.extend_from_slice(&converted);
			none()
		},
		Err(()) => ptr::null_mut(),
	}
}

/// Scans for `value`, returning the first matching element index.
fn find_index(array: &PyArrayObject, value: *mut PyObject) -> Result<Option<usize>, ()> {
	for index in 0..element_count(array) {
		let boxed = item_to_object(array, index);
		if boxed.is_null() {
			return Err(());
		}
		match value_equal(boxed, untag(value)) {
			Ok(true) => return Ok(Some(index)),
			Ok(false) => {},
			Err(()) => return Err(()),
		}
	}
	Ok(None)
}

unsafe extern "C" fn array_count_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (array, rest) = match unsafe { array_receiver_and_args(argv, argc, "count") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	let rest = match expect_args(rest, 1, "count") {
		Ok(rest) => rest,
		Err(raised) => return raised,
	};
	let mut count = 0i64;
	for index in 0..element_count(array) {
		let boxed = item_to_object(array, index);
		if boxed.is_null() {
			return ptr::null_mut();
		}
		match value_equal(boxed, untag(rest[0])) {
			Ok(true) => count += 1,
			Ok(false) => {},
			Err(()) => return ptr::null_mut(),
		}
	}
	// SAFETY: Runtime allocation helper.
	unsafe { abi::pon_const_int(count) }
}

unsafe extern "C" fn array_index_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (array, rest) = match unsafe { array_receiver_and_args(argv, argc, "index") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if rest.is_empty() || rest.len() > 3 {
		return raise_type_error(&format!(
			"array.index() takes 1 to 3 arguments ({} given)",
			rest.len()
		));
	}
	// start/stop windows are accepted but rarely used; implement the common
	// one-argument form plus simple clamped windows.
	let len = element_count(array) as i64;
	let clamp = |raw: i64| -> usize {
		let adjusted = if raw < 0 { raw + len } else { raw };
		adjusted.clamp(0, len) as usize
	};
	let start = match rest.get(1).copied().map(untag) {
		None => 0,
		Some(value) => match int_of(value) {
			Some(raw) => clamp(raw),
			None => return raise_type_error("array indices must be integers"),
		},
	};
	let stop = match rest.get(2).copied().map(untag) {
		None => len as usize,
		Some(value) => match int_of(value) {
			Some(raw) => clamp(raw),
			None => return raise_type_error("array indices must be integers"),
		},
	};
	for index in start..stop {
		let boxed = item_to_object(array, index);
		if boxed.is_null() {
			return ptr::null_mut();
		}
		match value_equal(boxed, untag(rest[0])) {
			// SAFETY: Runtime allocation helper.
			Ok(true) => return unsafe { abi::pon_const_int(index as i64) },
			Ok(false) => {},
			Err(()) => return ptr::null_mut(),
		}
	}
	raise_value_error("array.index(x): x not in array")
}

unsafe extern "C" fn array_remove_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (array, rest) = match unsafe { array_receiver_and_args(argv, argc, "remove") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	let rest = match expect_args(rest, 1, "remove") {
		Ok(rest) => rest,
		Err(raised) => return raised,
	};
	match find_index(array, rest[0]) {
		Ok(Some(index)) => {
			let size = item_size(array.typecode);
			array.data.drain(index * size..(index + 1) * size);
			none()
		},
		Ok(None) => raise_value_error("array.remove(x): x not in array"),
		Err(()) => ptr::null_mut(),
	}
}

unsafe extern "C" fn array_pop_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (array, rest) = match unsafe { array_receiver_and_args(argv, argc, "pop") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if rest.len() > 1 {
		return raise_type_error(&format!(
			"array.pop() takes at most 1 argument ({} given)",
			rest.len()
		));
	}
	let len = element_count(array);
	if len == 0 {
		return raise_index_error("pop from empty array");
	}
	let raw = match rest.first().copied().map(untag) {
		None => -1,
		Some(value) => match int_of(value) {
			Some(raw) => raw,
			None => return raise_type_error("array indices must be integers"),
		},
	};
	let index = match normalize_index(raw as isize, len, "pop index out of range") {
		Ok(index) => index,
		Err(raised) => return raised,
	};
	let value = item_to_object(array, index);
	if value.is_null() {
		return ptr::null_mut();
	}
	let size = item_size(array.typecode);
	array.data.drain(index * size..(index + 1) * size);
	value
}

unsafe extern "C" fn array_clear_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (array, rest) = match unsafe { array_receiver_and_args(argv, argc, "clear") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if let Err(raised) = expect_args(rest, 0, "clear") {
		return raised;
	}
	array.data.clear();
	none()
}

unsafe extern "C" fn array_reverse_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (array, rest) = match unsafe { array_receiver_and_args(argv, argc, "reverse") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if let Err(raised) = expect_args(rest, 0, "reverse") {
		return raised;
	}
	let size = item_size(array.typecode);
	let count = element_count(array);
	for index in 0..count / 2 {
		let (a, b) = (index * size, (count - 1 - index) * size);
		for offset in 0..size {
			array.data.swap(a + offset, b + offset);
		}
	}
	none()
}

unsafe extern "C" fn array_byteswap_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (array, rest) = match unsafe { array_receiver_and_args(argv, argc, "byteswap") } {
		Ok(parts) => parts,
		Err(raised) => return raised,
	};
	if let Err(raised) = expect_args(rest, 0, "byteswap") {
		return raised;
	}
	let size = item_size(array.typecode);
	for chunk in array.data.chunks_exact_mut(size) {
		chunk.reverse();
	}
	none()
}

fn int_of(object: *mut PyObject) -> Option<i64> {
	unsafe { crate::types::int::to_i64(object) }
}

fn reconstructor_typecode(typecode: u8, mformat_code: i64) -> Result<u8, String> {
	match mformat_code {
		0 => Ok(b'B'),
		1 => Ok(b'b'),
		2 | 3 => Ok(b'H'),
		4 | 5 => Ok(b'h'),
		6 | 7 => Ok(b'I'),
		8 | 9 => Ok(b'i'),
		10 | 11 if typecode == b'L' => Ok(b'L'),
		10 | 11 => Ok(b'Q'),
		12 | 13 if typecode == b'l' => Ok(b'l'),
		12 | 13 => Ok(b'q'),
		14..=17 => Ok(typecode),
		18..=21 if matches!(typecode, b'u' | b'w') => Ok(typecode),
		_ => Err("third argument must be a valid machine format code.".to_owned()),
	}
}

unsafe extern "C" fn array_reconstructor_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("_array_reconstructor received a null argv pointer");
	};
	if args.len() != 4 {
		return raise_type_error("_array_reconstructor() takes exactly 4 arguments");
	}
	if untag(args[0]) != array_type().cast::<PyObject>() {
		return raise_type_error("first argument must be array.array");
	}
	let Some(typecode_text) = (unsafe { unicode_text(untag(args[1])) }) else {
		return raise_type_error("second argument must be a valid type code");
	};
	let mut chars = typecode_text.chars();
	let (Some(typecode_char), None) = (chars.next(), chars.next()) else {
		return raise_value_error("second argument must be a valid type code");
	};
	if !typecode_char.is_ascii() || !is_typecode(typecode_char as u8) {
		return raise_value_error("second argument must be a valid type code");
	}
	let Some(mformat_code) = int_of(untag(args[2])) else {
		return raise_type_error("third argument must be an int");
	};
	let typecode = match reconstructor_typecode(typecode_char as u8, mformat_code) {
		Ok(typecode) => typecode,
		Err(message) => return raise_value_error(&message),
	};
	let Some(payload) = bytes_like(untag(args[3])) else {
		return raise_type_error(&format!(
			"fourth argument should be bytes, not {}",
			value_type_name(untag(args[3]))
		));
	};
	if payload.len() % item_size(typecode) != 0 {
		return raise_value_error("bytes length not a multiple of item size");
	}
	alloc_array(typecode, payload.to_vec())
}

fn function_attr(name: &str, entry: BuiltinFn) -> Result<(u32, *mut PyObject), String> {
	// SAFETY: Live builtin entry point with the runtime calling convention.
	let function =
		unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	if function.is_null() {
		Err(format!("failed to allocate array.{name}"))
	} else {
		Ok((intern(name), function))
	}
}

// ---------------------------------------------------------------------------

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let name = "array";
	// SAFETY: Runtime allocation helper; NULL is checked below.
	let name_obj = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
	if name_obj.is_null() {
		return Err("failed to allocate array.__name__".to_owned());
	}
	let typecodes = alloc_str_object(TYPECODES);
	if typecodes.is_null() {
		return Err("failed to allocate array.typecodes".to_owned());
	}
	install_module(name, vec![
		(intern("__name__"), name_obj),
		(intern("array"), array_type().cast::<PyObject>()),
		(intern("ArrayType"), array_type().cast::<PyObject>()),
		(intern("typecodes"), typecodes),
		function_attr("_array_reconstructor", array_reconstructor_method)?,
	])
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		thread_state::{pon_err_message, test_state_lock},
		types::memoryview::MemoryViewListValue,
	};

	fn init_runtime() {
		assert_eq!(unsafe { abi::pon_runtime_init() }, 0);
		pon_err_clear();
	}

	fn int_object(value: i64) -> *mut PyObject {
		unsafe { abi::pon_const_int(value) }
	}

	fn i32_array(values: &[i32]) -> *mut PyObject {
		let mut data = Vec::with_capacity(values.len() * 4);
		for value in values {
			data.extend_from_slice(&value.to_ne_bytes());
		}
		alloc_array(b'i', data)
	}

	fn i16_array(values: &[i16]) -> *mut PyObject {
		let mut data = Vec::with_capacity(values.len() * 2);
		for value in values {
			data.extend_from_slice(&value.to_ne_bytes());
		}
		alloc_array(b'h', data)
	}

	fn i32_values(object: *mut PyObject) -> Vec<i32> {
		let array = unsafe { as_array(object) }.expect("expected test array");
		array
			.data
			.chunks_exact(4)
			.map(|chunk| i32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
			.collect()
	}

	fn slice_object(start: Option<i64>, stop: Option<i64>, step: Option<i64>) -> *mut PyObject {
		let none = unsafe { abi::pon_none() };
		let slice = crate::types::slice_::PySlice {
			ob_base: PyObjectHeader::new(crate::abi::seq::slice_type().cast_const()),
			start:   start.map_or(none, int_object),
			stop:    stop.map_or(none, int_object),
			step:    step.map_or(none, int_object),
		};
		Box::into_raw(Box::new(slice)).cast::<PyObject>()
	}

	#[test]
	fn array_slice_get_set_delete_probe() {
		let _guard = test_state_lock();
		init_runtime();
		let array = i32_array(&[1, 2, 3, 4, 5]);
		let key = slice_object(Some(1), Some(4), None);
		let sliced = unsafe { array_subscript_slot(array, key) };
		assert!(!sliced.is_null(), "slice get failed: {:?}", pon_err_message());
		assert_eq!(i32_values(sliced), vec![2, 3, 4]);

		let replacement = i32_array(&[9, 8]);
		let status = unsafe { array_ass_subscript_slot(array, key, replacement) };
		assert_eq!(status, 0, "slice set failed: {:?}", pon_err_message());
		assert_eq!(i32_values(array), vec![1, 9, 8, 5]);

		let delete_key = slice_object(None, None, Some(2));
		let status = unsafe { array_ass_subscript_slot(array, delete_key, ptr::null_mut()) };
		assert_eq!(status, 0, "slice delete failed: {:?}", pon_err_message());
		assert_eq!(i32_values(array), vec![9, 5]);
	}

	#[test]
	fn array_byteswap_ordering_reconstructor_and_memoryview_probe() {
		let _guard = test_state_lock();
		init_runtime();

		let swap = i16_array(&[0x0102]);
		let mut argv = [swap];
		let result = unsafe { array_byteswap_method(argv.as_mut_ptr(), argv.len()) };
		assert!(!result.is_null(), "byteswap failed: {:?}", pon_err_message());
		let mut expected = 0x0102_i16.to_ne_bytes();
		expected.reverse();
		assert_eq!(unsafe { as_array(swap) }.unwrap().data, expected);

		let left = i32_array(&[1, 2]);
		let right = i32_array(&[1, 3]);
		let ordered = unsafe { array_richcmp_slot(left, right, RICH_LT as c_int) };
		assert!(!ordered.is_null(), "ordering failed: {:?}", pon_err_message());
		assert_eq!(unsafe { abi::pon_is_true(ordered) }, 1);

		let mut payload = Vec::new();
		payload.extend_from_slice(&1_i32.to_ne_bytes());
		payload.extend_from_slice(&2_i32.to_ne_bytes());
		let typecode = alloc_str_object("i");
		let bytes = crate::types::bytes_::boxed_bytes(&payload).cast::<PyObject>();
		let mut args = [array_type().cast::<PyObject>(), typecode, int_object(8), bytes];
		let rebuilt = unsafe { array_reconstructor_method(args.as_mut_ptr(), args.len()) };
		assert!(!rebuilt.is_null(), "reconstructor failed: {:?}", pon_err_message());
		assert_eq!(i32_values(rebuilt), vec![1, 2]);

		let view_source = i16_array(&[1, -2]);
		let view = unsafe { crate::types::memoryview::boxed_memoryview_from_object(view_source) }
			.expect("memoryview(array) should succeed");
		assert_eq!(unsafe { (*view).format }, b'h');
		let values = unsafe { crate::types::memoryview::tolist(&*view) }
			.expect("memoryview(array).tolist() should decode h format");
		assert_eq!(values, vec![MemoryViewListValue::Int(1), MemoryViewListValue::Int(-2)]);
	}
}
