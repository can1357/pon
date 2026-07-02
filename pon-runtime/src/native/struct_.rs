//! Native `_struct` — the standard-library struct format engine.
//!
//! `Lib/struct.py` is a thin re-export (`from _struct import *` plus
//! `_clearcache` / `__doc__`), so this module provides the full public
//! surface: `pack`, `pack_into`, `unpack`, `unpack_from`, `iter_unpack`,
//! `calcsize`, the `Struct` class, and the `error` exception.
//!
//! # Format engine
//!
//! Byte-order prefixes `@` (native order + native sizes + C alignment),
//! `=` (native order, standard sizes, no alignment), `<`, `>`, `!`
//! (explicit order, standard sizes, no alignment). Format codes
//! `x b B c s p ? h H i I l L q Q n N e f d P` with repeat counts, where
//! `n N P` are native-mode-only exactly like CPython. Integer packing is
//! manual two's-complement byte assembly (`u128` windows written little- or
//! big-endian by hand); floats go through `f64`/`f32` bit patterns and a
//! hand-rolled IEEE 754 binary16 converter for `e` — no external crates.
//!
//! # Divergences (documented)
//!
//! * Module-level functions accept a `Struct` instance anywhere a format
//!   string is accepted (receiver/format arg0 dual dispatch lets the
//!   `Struct` methods share the module entry points); CPython would raise
//!   `TypeError`. Format structs are re-parsed per call instead of cached,
//!   so `_clearcache` is a no-op.
//! * `iter_unpack` snapshots the buffer eagerly; mutating a `bytearray`
//!   while iterating diverges from CPython's live view.
//!
//! GC: `Struct` instances, the unpack iterator, and both native types are
//! immortal leaked boxes holding no GC references (formats are plain Rust
//! data; the iterator owns a byte copy), so no root registration is needed.
//! The `error` class and function objects are rooted by the module object.

use std::ptr;
use std::sync::LazyLock;

use num_bigint::BigInt;
use num_traits::ToPrimitive;

use crate::abi;
use crate::intern::intern;
use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::types::exc::ExceptionKind;
use crate::types::type_::unicode_text;
use crate::types::{bytearray_, bytes_, memoryview};

use super::builtins_mod::VARIADIC_ARITY;
use super::install_module;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

// ---------------------------------------------------------------------------
// Module construction

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let error_cls = *ERROR_CLASS as *mut PyObject;
    if error_cls.is_null() {
        return Err("failed to create the struct.error exception class".to_owned());
    }

    let mut attrs: Vec<(u32, *mut PyObject)> = Vec::with_capacity(12);
    let mut string_attr = |name: &str, value: &str| -> Result<(), String> {
        let object = alloc_str_object(value);
        if object.is_null() {
            return Err(format!("failed to allocate _struct.{name}"));
        }
        attrs.push((intern(name), object));
        Ok(())
    };
    string_attr("__name__", "_struct")?;
    string_attr(
        "__doc__",
        "Functions to convert between Python values and C structs.\n\
         Python bytes objects are used to hold the data representing the C struct\n\
         and also as format strings (explained below) to describe the layout of data\n\
         in the C struct.\n",
    )?;

    let mut function_attr = |name: &str, entry: BuiltinFn| -> Result<(), String> {
        // SAFETY: `entry` is a live builtin entry point with the runtime
        // calling convention.
        let function = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
        if function.is_null() {
            return Err(format!("failed to allocate _struct.{name}"));
        }
        attrs.push((intern(name), function));
        Ok(())
    };
    function_attr("pack", pack_entry)?;
    function_attr("pack_into", pack_into_entry)?;
    function_attr("unpack", unpack_entry)?;
    function_attr("unpack_from", unpack_from_entry)?;
    function_attr("iter_unpack", iter_unpack_entry)?;
    function_attr("calcsize", calcsize_entry)?;
    function_attr("_clearcache", clearcache_entry)?;

    attrs.push((intern("error"), error_cls));
    attrs.push((intern("Struct"), struct_type().cast::<PyObject>()));

    install_module("_struct", attrs)
}

// ---------------------------------------------------------------------------
// The `error` exception class (`class error(Exception)` with
// `__module__ = 'struct'`, built through the 3-argument `type()` path so
// raise/except machinery treats it exactly like a Python-defined subclass).

static ERROR_CLASS: LazyLock<usize> = LazyLock::new(|| {
    let Some(exception) = abi::runtime_global(intern("Exception")) else {
        return 0;
    };
    let name = alloc_str_object("error");
    let module_key = alloc_str_object("__module__");
    let module_value = alloc_str_object("struct");
    if name.is_null() || module_key.is_null() || module_value.is_null() {
        return 0;
    }
    let mut base_slots = [exception];
    // SAFETY: `base_slots` is a live window for the duration of the call.
    let bases = unsafe { abi::seq::pon_build_tuple(base_slots.as_mut_ptr(), base_slots.len()) };
    let mut pair_slots = [module_key, module_value];
    // SAFETY: `pair_slots` holds one live key/value pair.
    let namespace = unsafe { abi::map::pon_build_map(pair_slots.as_mut_ptr(), 1) };
    if bases.is_null() || namespace.is_null() {
        return 0;
    }
    let mut argv = [name, bases, namespace];
    // SAFETY: `argv` carries the `type(name, bases, ns)` triple.
    let cls = unsafe { crate::types::type_::builtin_type(argv.as_mut_ptr(), argv.len()) };
    cls as usize
});

/// Raises `struct.error(text)` and returns NULL.
fn raise_struct_error(text: &str) -> *mut PyObject {
    let cls = *ERROR_CLASS as *mut PyObject;
    if cls.is_null() {
        // The class failed to build (runtime not initialized): degrade to a
        // typed ValueError so callers still see a catchable exception.
        return abi::exc::raise_kind_error_text(ExceptionKind::ValueError, text);
    }
    let message = alloc_str_object(text);
    if message.is_null() {
        return ptr::null_mut();
    }
    let mut argv = [message];
    // SAFETY: `cls` is a live type object; the call constructs an instance.
    let exception = unsafe { abi::pon_call(cls, argv.as_mut_ptr(), argv.len()) };
    if exception.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `exception` is a live exception instance.
    unsafe { abi::exc::pon_raise(untag(exception), ptr::null_mut()) }
}

// ---------------------------------------------------------------------------
// Small helpers (contextvars/codecs idioms)

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

fn is_none(object: *mut PyObject) -> bool {
    object == none()
}

fn alloc_str_object(text: &str) -> *mut PyObject {
    // SAFETY: Runtime allocation helper; NULL on failure with the error set.
    unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn alloc_bytes_object(bytes: &[u8]) -> *mut PyObject {
    // SAFETY: Runtime allocation helper; NULL on failure with the error set.
    unsafe { abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) }
}

fn alloc_int_object(value: i64) -> *mut PyObject {
    // SAFETY: Runtime allocation helper; NULL on failure with the error set.
    unsafe { abi::pon_const_int(value) }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

fn raise_overflow_error(message: &str) -> *mut PyObject {
    abi::exc::raise_kind_error_text(ExceptionKind::OverflowError, message)
}

fn raise_stop_iteration() -> *mut PyObject {
    // SAFETY: NULL value produces a plain StopIteration.
    unsafe { abi::pon_raise_stop_iteration(ptr::null_mut()) }
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

fn tuple_from(mut items: Vec<*mut PyObject>) -> *mut PyObject {
    let argv = if items.is_empty() { ptr::null_mut() } else { items.as_mut_ptr() };
    // SAFETY: `argv` is a live window; the result is a real PyTuple.
    unsafe { abi::seq::pon_build_tuple(argv, items.len()) }
}

fn bound_method(receiver: *mut PyObject, name: &str, entry: BuiltinFn) -> *mut PyObject {
    // SAFETY: `entry` is a live builtin entry point with the runtime calling convention.
    let function = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match crate::types::method::new_bound_method(function, receiver) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => fail(message),
    }
}

fn runtime_object_type() -> *mut PyType {
    abi::runtime_global(intern("object")).map_or(ptr::null_mut(), |object| object.cast::<PyType>())
}

/// Borrows a readable buffer argument's payload (bytes, bytearray, memoryview).
fn buffer_arg<'a>(object: *mut PyObject) -> Result<&'a [u8], String> {
    let object = untag(object);
    if object.is_null() {
        return Err("a bytes-like object is required, not 'NULL'".to_owned());
    }
    // SAFETY: A non-NULL heap object carries a live header.
    let ty = unsafe { (*object).ob_type };
    if bytes_::is_bytes_type(ty) {
        // SAFETY: Type check above proved the layout.
        return Ok(unsafe { (*object.cast::<bytes_::PyBytes>()).as_slice() });
    }
    if bytearray_::is_bytearray_type(ty) {
        // SAFETY: Type check above proved the layout.
        return Ok(unsafe { (*object.cast::<bytearray_::PyByteArray>()).as_slice() });
    }
    if memoryview::is_memoryview_type(ty) {
        // SAFETY: Type check above proved the layout.
        let view = unsafe { &*object.cast::<memoryview::PyMemoryView>() };
        if view.data.is_null() && view.len != 0 {
            return Err("memoryview data pointer is null".to_owned());
        }
        // SAFETY: The view describes a live contiguous window.
        return Ok(unsafe { std::slice::from_raw_parts(view.data.cast_const(), view.len) });
    }
    Err(format!("a bytes-like object is required, not '{}'", value_type_name(object)))
}

/// Borrows a writable buffer argument's payload (bytearray, writable memoryview).
fn writable_buffer_arg<'a>(object: *mut PyObject) -> Result<&'a mut [u8], String> {
    let object = untag(object);
    if object.is_null() {
        return Err("argument must be read-write bytes-like object, not NULL".to_owned());
    }
    // SAFETY: A non-NULL heap object carries a live header.
    let ty = unsafe { (*object).ob_type };
    if bytearray_::is_bytearray_type(ty) {
        // SAFETY: Type check above proved the layout.
        let bytes = unsafe { &mut (*object.cast::<bytearray_::PyByteArray>()).bytes };
        return Ok(bytes.as_mut_slice());
    }
    if memoryview::is_memoryview_type(ty) {
        // SAFETY: Type check above proved the layout.
        let view = unsafe { &*object.cast::<memoryview::PyMemoryView>() };
        if view.readonly {
            return Err("argument must be read-write bytes-like object, not memoryview".to_owned());
        }
        if view.data.is_null() && view.len != 0 {
            return Err("memoryview data pointer is null".to_owned());
        }
        // SAFETY: The view describes a live, writable contiguous window.
        return Ok(unsafe { std::slice::from_raw_parts_mut(view.data, view.len) });
    }
    Err(format!(
        "argument must be read-write bytes-like object, not {}",
        value_type_name(object)
    ))
}

/// Absent-or-None optional argument (the native keyword binder fills gaps
/// with None).
fn optional_arg(args: &[*mut PyObject], index: usize) -> Option<*mut PyObject> {
    match args.get(index).copied() {
        None => None,
        Some(value) if value.is_null() || is_none(untag(value)) => None,
        Some(value) => Some(value),
    }
}

fn int_arg(object: *mut PyObject, what: &str) -> Result<i64, *mut PyObject> {
    let object = untag(object);
    // SAFETY: `untag` normalized the pointer; conversion type-checks.
    match unsafe { crate::types::int::to_bigint_including_bool(object) } {
        Some(value) => value
            .to_i64()
            .ok_or_else(|| raise_overflow_error(&format!("{what} does not fit in an int"))),
        None => Err(raise_type_error(&format!(
            "{what} must be an integer, not {}",
            value_type_name(object)
        ))),
    }
}

// ---------------------------------------------------------------------------
// Format model

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// `x`: pad bytes, no argument.
    Pad,
    /// `b h i l q n`: signed integers.
    Signed,
    /// `B H I L Q N P`: unsigned integers.
    Unsigned,
    /// `?`: bool.
    Bool,
    /// `c`: single byte as a length-1 bytes object.
    Char,
    /// `s`: fixed-length byte string (count = byte length).
    Str,
    /// `p`: Pascal string (count = total field length incl. length byte).
    Pascal,
    /// `e f d`: IEEE 754 binary16/32/64 (distinguished by size).
    Float,
}

#[derive(Clone, Copy, Debug)]
struct Item {
    /// Original format character, for error messages.
    code: u8,
    kind: Kind,
    /// Repeat count; for `s`/`p`/`x` the total byte length of the field.
    count: usize,
    /// Per-element size in bytes (1 for `s`/`p`/`x`).
    size: usize,
    /// Alignment padding inserted before the first element (native mode).
    pad_before: usize,
}

#[derive(Clone, Debug)]
struct Format {
    little: bool,
    items: Vec<Item>,
    size: usize,
    arg_count: usize,
}

/// `(kind, size, align)` for a format code; `None` = bad char in this mode.
fn code_spec(code: u8, native: bool) -> Option<(Kind, usize, usize)> {
    use core::ffi::{c_int, c_long, c_longlong, c_short};
    Some(match code {
        b'x' => (Kind::Pad, 1, 1),
        b'b' => (Kind::Signed, 1, 1),
        b'B' => (Kind::Unsigned, 1, 1),
        b'c' => (Kind::Char, 1, 1),
        b's' => (Kind::Str, 1, 1),
        b'p' => (Kind::Pascal, 1, 1),
        b'?' => (Kind::Bool, 1, 1),
        b'h' if native => (Kind::Signed, size_of::<c_short>(), align_of::<c_short>()),
        b'H' if native => (Kind::Unsigned, size_of::<c_short>(), align_of::<c_short>()),
        b'h' => (Kind::Signed, 2, 1),
        b'H' => (Kind::Unsigned, 2, 1),
        b'i' if native => (Kind::Signed, size_of::<c_int>(), align_of::<c_int>()),
        b'I' if native => (Kind::Unsigned, size_of::<c_int>(), align_of::<c_int>()),
        b'i' => (Kind::Signed, 4, 1),
        b'I' => (Kind::Unsigned, 4, 1),
        b'l' if native => (Kind::Signed, size_of::<c_long>(), align_of::<c_long>()),
        b'L' if native => (Kind::Unsigned, size_of::<c_long>(), align_of::<c_long>()),
        b'l' => (Kind::Signed, 4, 1),
        b'L' => (Kind::Unsigned, 4, 1),
        b'q' if native => (Kind::Signed, size_of::<c_longlong>(), align_of::<c_longlong>()),
        b'Q' if native => (Kind::Unsigned, size_of::<c_longlong>(), align_of::<c_longlong>()),
        b'q' => (Kind::Signed, 8, 1),
        b'Q' => (Kind::Unsigned, 8, 1),
        b'n' if native => (Kind::Signed, size_of::<isize>(), align_of::<isize>()),
        b'N' if native => (Kind::Unsigned, size_of::<usize>(), align_of::<usize>()),
        b'P' if native => (Kind::Unsigned, size_of::<usize>(), align_of::<usize>()),
        b'e' if native => (Kind::Float, 2, 2),
        b'f' if native => (Kind::Float, size_of::<f32>(), align_of::<f32>()),
        b'd' if native => (Kind::Float, size_of::<f64>(), align_of::<f64>()),
        b'e' => (Kind::Float, 2, 1),
        b'f' => (Kind::Float, 4, 1),
        b'd' => (Kind::Float, 8, 1),
        _ => return None,
    })
}

/// Parses a format string into the item list. `Err` carries a
/// `struct.error` message.
fn parse_format(fmt: &str) -> Result<Format, String> {
    let bytes = fmt.as_bytes();
    let (native, little, body) = match bytes.first() {
        Some(b'@') => (true, cfg!(target_endian = "little"), &bytes[1..]),
        Some(b'=') => (false, cfg!(target_endian = "little"), &bytes[1..]),
        Some(b'<') => (false, true, &bytes[1..]),
        Some(b'>') | Some(b'!') => (false, false, &bytes[1..]),
        _ => (true, cfg!(target_endian = "little"), bytes),
    };

    let mut items = Vec::new();
    let mut pos: usize = 0;
    let mut arg_count: usize = 0;
    let mut index = 0;
    while index < body.len() {
        let ch = body[index];
        if ch.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        let mut count: Option<usize> = None;
        if ch.is_ascii_digit() {
            let mut value: usize = 0;
            while index < body.len() && body[index].is_ascii_digit() {
                value = value
                    .checked_mul(10)
                    .and_then(|value| value.checked_add(usize::from(body[index] - b'0')))
                    .ok_or_else(|| "overflow in item count".to_owned())?;
                index += 1;
            }
            if index >= body.len() {
                return Err("repeat count given without format specifier".to_owned());
            }
            count = Some(value);
        }
        let code = body[index];
        index += 1;
        let Some((kind, size, align)) = code_spec(code, native) else {
            return Err("bad char in struct format".to_owned());
        };
        let align = if native { align } else { 1 };
        let count = count.unwrap_or(1);

        let pad_before = if pos % align == 0 { 0 } else { align - pos % align };
        pos = pos
            .checked_add(pad_before)
            .ok_or_else(|| "total struct size too long".to_owned())?;
        let field_bytes = match kind {
            Kind::Str | Kind::Pascal | Kind::Pad => count,
            _ => count
                .checked_mul(size)
                .ok_or_else(|| "total struct size too long".to_owned())?,
        };
        pos = pos
            .checked_add(field_bytes)
            .ok_or_else(|| "total struct size too long".to_owned())?;
        arg_count += match kind {
            Kind::Pad => 0,
            Kind::Str | Kind::Pascal => 1,
            _ => count,
        };
        items.push(Item {
            code,
            kind,
            count,
            size,
            pad_before,
        });
    }

    Ok(Format {
        little,
        items,
        size: pos,
        arg_count,
    })
}

// ---------------------------------------------------------------------------
// Raw byte codecs

/// Writes the low `dest.len()` bytes of `value` in the requested order.
fn write_uint(dest: &mut [u8], value: u128, little: bool) {
    let bytes = value.to_le_bytes();
    let n = dest.len();
    if little {
        dest.copy_from_slice(&bytes[..n]);
    } else {
        for (i, slot) in dest.iter_mut().enumerate() {
            *slot = bytes[n - 1 - i];
        }
    }
}

fn read_uint(src: &[u8], little: bool) -> u128 {
    let mut value: u128 = 0;
    if little {
        for &byte in src.iter().rev() {
            value = (value << 8) | u128::from(byte);
        }
    } else {
        for &byte in src {
            value = (value << 8) | u128::from(byte);
        }
    }
    value
}

/// Converts an `f64` to IEEE 754 binary16 bits with round-to-nearest-even.
/// `None` = magnitude rounds beyond the binary16 range (overflow).
fn f64_to_f16_bits(value: f64) -> Option<u16> {
    let sign = if value.is_sign_negative() { 0x8000u16 } else { 0 };
    if value.is_nan() {
        return Some(sign | 0x7e00);
    }
    let abs = value.abs();
    if abs.is_infinite() {
        return Some(sign | 0x7c00);
    }
    // 65519.999… rounds down to 65504 (f16 max); >= 65520 rounds to 65536.
    if abs >= 65520.0 {
        return None;
    }
    if abs < f64::powi(2.0, -14) {
        // Subnormal range: units of 2^-24, ties to even.
        let scaled = (abs * f64::powi(2.0, 24)).round_ties_even() as u16;
        if scaled >= 0x400 {
            // Rounded up into the smallest normal.
            return Some(sign | 0x0400);
        }
        return Some(sign | scaled);
    }
    // Normal range: find the exponent, round the 10-bit mantissa.
    let mut exp = abs.log2().floor() as i32;
    exp = exp.clamp(-14, 15);
    // Guard against boundary drift from log2 rounding.
    if abs < f64::powi(2.0, exp) {
        exp -= 1;
    } else if abs >= f64::powi(2.0, exp + 1) {
        exp += 1;
    }
    let mantissa = (abs / f64::powi(2.0, exp) * 1024.0).round_ties_even() as u32;
    let (exp, mantissa) = if mantissa >= 2048 { (exp + 1, 1024u32) } else { (exp, mantissa) };
    if exp > 15 {
        return None;
    }
    Some(sign | (((exp + 15) as u16) << 10) | ((mantissa as u16) & 0x3ff))
}

fn f16_bits_to_f64(bits: u16) -> f64 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = (bits >> 10) & 0x1f;
    let mantissa = f64::from(bits & 0x3ff);
    match exp {
        0 => sign * mantissa * f64::powi(2.0, -24),
        0x1f => {
            if mantissa == 0.0 {
                sign * f64::INFINITY
            } else {
                f64::NAN
            }
        }
        _ => sign * (1.0 + mantissa / 1024.0) * f64::powi(2.0, i32::from(exp) - 15),
    }
}

// ---------------------------------------------------------------------------
// Packing

/// Two's-complement bounds for an integer item.
fn int_bounds(kind: Kind, size: usize) -> (i128, i128) {
    let bits = (size * 8) as u32;
    if kind == Kind::Signed {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, ((1u128 << bits) - 1) as i128)
    }
}

fn range_error_message(item: &Item) -> String {
    let (min, max) = int_bounds(item.kind, item.size);
    match item.code {
        b'b' => "byte format requires -128 <= number <= 127".to_owned(),
        b'B' => "ubyte format requires 0 <= number <= 255".to_owned(),
        b'h' => "short format requires -32768 <= number <= 32767".to_owned(),
        b'H' => "ushort format requires 0 <= number <= 65535".to_owned(),
        code => format!("'{}' format requires {min} <= number <= {max}", code as char),
    }
}

/// Packs one non-pad element into `dest`; raises and returns `Err` on failure.
fn pack_element(dest: &mut [u8], value: *mut PyObject, item: &Item, little: bool) -> Result<(), ()> {
    let value = untag(value);
    match item.kind {
        Kind::Pad => {}
        Kind::Signed | Kind::Unsigned => {
            // SAFETY: `value` is heap-or-NULL after untagging; conversion type-checks.
            let Some(big) = (unsafe { crate::types::int::to_bigint_including_bool(value) }) else {
                raise_struct_error("required argument is not an integer");
                return Err(());
            };
            let (min, max) = int_bounds(item.kind, item.size);
            let Some(int) = big.to_i128().filter(|int| (min..=max).contains(int)) else {
                raise_struct_error(&range_error_message(item));
                return Err(());
            };
            let bits = (item.size * 8) as u32;
            let mask = if bits == 128 { u128::MAX } else { (1u128 << bits) - 1 };
            write_uint(dest, (int as u128) & mask, little);
        }
        Kind::Bool => {
            // SAFETY: `pon_is_true` self-normalizes its argument.
            match unsafe { abi::pon_is_true(value) } {
                0 => dest[0] = 0,
                1 => dest[0] = 1,
                _ => return Err(()),
            }
        }
        Kind::Char => {
            let Ok(bytes) = buffer_arg(value) else {
                raise_struct_error("char format requires a bytes object of length 1");
                return Err(());
            };
            if bytes.len() != 1 {
                raise_struct_error("char format requires a bytes object of length 1");
                return Err(());
            }
            dest[0] = bytes[0];
        }
        Kind::Str => {
            // SAFETY: A non-NULL heap object carries a live header.
            if value.is_null() || !bytes_::is_bytes_type(unsafe { (*value).ob_type }) {
                raise_struct_error("argument for 's' must be a bytes object");
                return Err(());
            }
            // SAFETY: Type check above proved the layout.
            let bytes = unsafe { (*value.cast::<bytes_::PyBytes>()).as_slice() };
            let n = bytes.len().min(item.count);
            dest[..n].copy_from_slice(&bytes[..n]);
        }
        Kind::Pascal => {
            // SAFETY: A non-NULL heap object carries a live header.
            if value.is_null() || !bytes_::is_bytes_type(unsafe { (*value).ob_type }) {
                raise_struct_error("argument for 'p' must be a bytes object");
                return Err(());
            }
            if item.count == 0 {
                return Ok(());
            }
            // SAFETY: Type check above proved the layout.
            let bytes = unsafe { (*value.cast::<bytes_::PyBytes>()).as_slice() };
            let n = bytes.len().min(item.count - 1).min(255);
            dest[0] = n as u8;
            dest[1..1 + n].copy_from_slice(&bytes[..n]);
        }
        Kind::Float => {
            // SAFETY: `value` is heap-or-NULL after untagging; both probes type-check.
            let float = match unsafe { crate::types::float::to_f64(value) } {
                Some(float) => float,
                None => match unsafe { crate::types::int::to_bigint_including_bool(value) }
                    .and_then(|int| int.to_f64())
                {
                    Some(float) => float,
                    None => {
                        raise_struct_error("required argument is not a float");
                        return Err(());
                    }
                },
            };
            match item.size {
                2 => {
                    let Some(bits) = f64_to_f16_bits(float) else {
                        raise_overflow_error("float too large to pack with e format");
                        return Err(());
                    };
                    write_uint(dest, u128::from(bits), little);
                }
                4 => {
                    let single = float as f32;
                    if single.is_infinite() && float.is_finite() {
                        raise_overflow_error("float too large to pack with f format");
                        return Err(());
                    }
                    write_uint(dest, u128::from(single.to_bits()), little);
                }
                _ => write_uint(dest, u128::from(float.to_bits()), little),
            }
        }
    }
    Ok(())
}

/// Packs `values` with `fmt` into a zeroed buffer; raises and returns `Err`
/// on failure.
fn pack_values(fmt: &Format, values: &[*mut PyObject]) -> Result<Vec<u8>, ()> {
    let mut out = vec![0u8; fmt.size];
    let mut pos = 0usize;
    let mut arg = 0usize;
    for item in &fmt.items {
        pos += item.pad_before;
        match item.kind {
            Kind::Pad => pos += item.count,
            Kind::Str | Kind::Pascal => {
                pack_element(&mut out[pos..pos + item.count], values[arg], item, fmt.little)?;
                pos += item.count;
                arg += 1;
            }
            _ => {
                for _ in 0..item.count {
                    pack_element(&mut out[pos..pos + item.size], values[arg], item, fmt.little)?;
                    pos += item.size;
                    arg += 1;
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Unpacking

/// Decodes one non-pad element; NULL means an allocation failure was raised.
fn unpack_element(src: &[u8], item: &Item, little: bool) -> *mut PyObject {
    match item.kind {
        Kind::Pad => none(),
        Kind::Signed => {
            let raw = read_uint(src, little);
            let bits = (item.size * 8) as u32;
            let signed = if bits < 128 && raw >= (1u128 << (bits - 1)) {
                raw as i128 - (1i128 << bits)
            } else {
                raw as i128
            };
            crate::types::int::from_bigint(BigInt::from(signed))
        }
        Kind::Unsigned => crate::types::int::from_bigint(BigInt::from(read_uint(src, little))),
        Kind::Bool => {
            // SAFETY: Runtime allocation helper returning a bool singleton.
            unsafe { abi::pon_const_bool(i32::from(src[0] != 0)) }
        }
        Kind::Char => alloc_bytes_object(&src[..1]),
        Kind::Str => alloc_bytes_object(src),
        Kind::Pascal => {
            if item.count == 0 {
                return alloc_bytes_object(&[]);
            }
            let n = (src[0] as usize).min(item.count - 1);
            alloc_bytes_object(&src[1..1 + n])
        }
        Kind::Float => {
            let raw = read_uint(src, little);
            let float = match item.size {
                2 => f16_bits_to_f64(raw as u16),
                4 => f64::from(f32::from_bits(raw as u32)),
                _ => f64::from_bits(raw as u64),
            };
            // SAFETY: Runtime allocation helper; NULL on failure with the error set.
            unsafe { abi::number::pon_const_float(float) }
        }
    }
}

/// Decodes one struct record at `data[..fmt.size]` into a fresh tuple.
fn unpack_record(fmt: &Format, data: &[u8]) -> *mut PyObject {
    let mut values = Vec::with_capacity(fmt.arg_count);
    let mut pos = 0usize;
    for item in &fmt.items {
        pos += item.pad_before;
        match item.kind {
            Kind::Pad => pos += item.count,
            Kind::Str | Kind::Pascal => {
                let value = unpack_element(&data[pos..pos + item.count], item, fmt.little);
                if value.is_null() {
                    return ptr::null_mut();
                }
                values.push(value);
                pos += item.count;
            }
            _ => {
                for _ in 0..item.count {
                    let value = unpack_element(&data[pos..pos + item.size], item, fmt.little);
                    if value.is_null() {
                        return ptr::null_mut();
                    }
                    values.push(value);
                    pos += item.size;
                }
            }
        }
    }
    tuple_from(values)
}

// ---------------------------------------------------------------------------
// The Struct type

#[repr(C)]
struct PyStructObject {
    ob_base: PyObjectHeader,
    format_text: String,
    fmt: Format,
}

static STRUCT_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(
        abi::runtime_type_type().cast_const(),
        "Struct",
        std::mem::size_of::<PyStructObject>(),
    );
    ty.tp_base = runtime_object_type();
    ty.tp_new = Some(struct_new);
    ty.tp_getattro = Some(struct_getattro);
    ty.tp_repr = Some(struct_repr);
    ty.tp_str = Some(struct_repr);
    Box::into_raw(Box::new(ty)) as usize
});

fn struct_type() -> *mut PyType {
    *STRUCT_TYPE as *mut PyType
}

/// Format text from a str or bytes format argument.
fn format_text_arg(object: *mut PyObject) -> Result<String, *mut PyObject> {
    let object = untag(object);
    // SAFETY: `unicode_text` type-checks before borrowing.
    if let Some(text) = unsafe { unicode_text(object) } {
        return Ok(text.to_owned());
    }
    if !object.is_null() {
        // SAFETY: A non-NULL heap object carries a live header.
        if bytes_::is_bytes_type(unsafe { (*object).ob_type }) {
            // SAFETY: Type check above proved the layout.
            let bytes = unsafe { (*object.cast::<bytes_::PyBytes>()).as_slice() };
            return match std::str::from_utf8(bytes) {
                Ok(text) => Ok(text.to_owned()),
                Err(_) => Err(raise_struct_error("bad char in struct format")),
            };
        }
    }
    Err(raise_type_error(&format!(
        "Struct() argument 1 must be a str or bytes object, not {}",
        value_type_name(object)
    )))
}

fn alloc_struct_object(format_text: String, fmt: Format) -> *mut PyObject {
    Box::into_raw(Box::new(PyStructObject {
        ob_base: PyObjectHeader::new(struct_type()),
        format_text,
        fmt,
    }))
    .cast::<PyObject>()
}

unsafe extern "C" fn struct_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    // SAFETY: `args` is the positional carrier provided by the type-call path.
    let positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
        Ok(positional) => positional,
        Err(message) => return fail(message),
    };
    let mut format_arg = positional.first().copied();
    if positional.len() > 1 {
        return raise_type_error("Struct() takes at most 1 argument");
    }
    if !kwargs.is_null() {
        // SAFETY: `call_type_with_keywords` materializes keywords as a dict.
        let entries = match unsafe { crate::types::dict::dict_entries_snapshot(kwargs) } {
            Ok(entries) => entries,
            Err(message) => return fail(message),
        };
        for entry in entries {
            // SAFETY: Dict keys are live objects; `unicode_text` type-checks.
            match unsafe { unicode_text(untag(entry.key)) } {
                Some("format") if format_arg.is_none() => format_arg = Some(entry.value),
                Some("format") => return raise_type_error("argument for Struct() given by name ('format') and position (1)"),
                Some(other) => {
                    return raise_type_error(&format!("Struct() got an unexpected keyword argument '{other}'"));
                }
                None => return raise_type_error("Struct() keywords must be strings"),
            }
        }
    }
    let Some(format_arg) = format_arg else {
        return raise_type_error("Struct() missing required argument 'format' (pos 1)");
    };
    let format_text = match format_text_arg(format_arg) {
        Ok(text) => text,
        Err(result) => return result,
    };
    match parse_format(&format_text) {
        Ok(fmt) => alloc_struct_object(format_text, fmt),
        Err(message) => raise_struct_error(&message),
    }
}

unsafe fn as_struct<'a>(object: *mut PyObject) -> Option<&'a PyStructObject> {
    if object.is_null() {
        return None;
    }
    // SAFETY: A non-NULL heap object carries a live header.
    if unsafe { (*object).ob_type } != struct_type() {
        return None;
    }
    // SAFETY: Type check above proved the layout.
    Some(unsafe { &*object.cast::<PyStructObject>() })
}

unsafe extern "C" fn struct_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    // SAFETY: Attribute names are str objects; `unicode_text` type-checks.
    let Some(name_text) = (unsafe { unicode_text(untag(name)) }) else {
        return fail("attribute name must be str");
    };
    let Some(record) = (unsafe { as_struct(object) }) else {
        return fail("Struct receiver is invalid");
    };
    match name_text {
        "format" => alloc_str_object(&record.format_text),
        "size" => alloc_int_object(record.fmt.size as i64),
        "pack" => bound_method(object, name_text, pack_entry),
        "pack_into" => bound_method(object, name_text, pack_into_entry),
        "unpack" => bound_method(object, name_text, unpack_entry),
        "unpack_from" => bound_method(object, name_text, unpack_from_entry),
        "iter_unpack" => bound_method(object, name_text, iter_unpack_entry),
        // SAFETY: Raise helper with the interned attribute name.
        _ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
    }
}

unsafe extern "C" fn struct_repr(object: *mut PyObject) -> *mut PyObject {
    let Some(record) = (unsafe { as_struct(object) }) else {
        return fail("Struct receiver is invalid");
    };
    alloc_str_object(&format!("Struct('{}')", record.format_text))
}

// ---------------------------------------------------------------------------
// The iter_unpack iterator

#[repr(C)]
struct PyUnpackIterator {
    ob_base: PyObjectHeader,
    fmt: Format,
    /// Eager copy of the source buffer (documented divergence: mutating a
    /// bytearray mid-iteration is not observed).
    data: Vec<u8>,
    pos: usize,
}

static UNPACK_ITER_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(
        abi::runtime_type_type().cast_const(),
        "unpack_iterator",
        std::mem::size_of::<PyUnpackIterator>(),
    );
    ty.tp_iter = Some(unpack_iter_identity);
    ty.tp_iternext = Some(unpack_iter_next);
    Box::into_raw(Box::new(ty)) as usize
});

unsafe extern "C" fn unpack_iter_identity(object: *mut PyObject) -> *mut PyObject {
    object
}

unsafe extern "C" fn unpack_iter_next(object: *mut PyObject) -> *mut PyObject {
    // SAFETY: The iterator type only ever wraps `PyUnpackIterator` boxes.
    let state = unsafe { &mut *object.cast::<PyUnpackIterator>() };
    if state.pos >= state.data.len() {
        return raise_stop_iteration();
    }
    let record = unpack_record(&state.fmt, &state.data[state.pos..state.pos + state.fmt.size]);
    if record.is_null() {
        return ptr::null_mut();
    }
    state.pos += state.fmt.size;
    record
}

// ---------------------------------------------------------------------------
// Entry points (each doubles as the module function and the Struct method:
// arg0 is either a format str/bytes or a Struct receiver)

/// Resolves arg0 into a parsed format, either borrowed from a `Struct`
/// receiver or freshly parsed. `Err` means the error is already raised.
enum ResolvedFormat<'a> {
    Borrowed(&'a Format),
    Owned(Format),
}

impl ResolvedFormat<'_> {
    fn get(&self) -> &Format {
        match self {
            ResolvedFormat::Borrowed(fmt) => fmt,
            ResolvedFormat::Owned(fmt) => fmt,
        }
    }
}

fn resolve_format<'a>(object: *mut PyObject) -> Result<ResolvedFormat<'a>, ()> {
    // SAFETY: `as_struct` type-checks before borrowing; Struct boxes are immortal.
    if let Some(record) = unsafe { as_struct(untag(object)) } {
        return Ok(ResolvedFormat::Borrowed(&record.fmt));
    }
    let text = match format_text_arg(object) {
        Ok(text) => text,
        Err(_) => return Err(()),
    };
    match parse_format(&text) {
        Ok(fmt) => Ok(ResolvedFormat::Owned(fmt)),
        Err(message) => {
            raise_struct_error(&message);
            Err(())
        }
    }
}

/// `calcsize(format)`.
unsafe extern "C" fn calcsize_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return fail("calcsize() argv pointer is null");
    };
    if args.len() != 1 {
        return raise_type_error(&format!("calcsize() takes exactly 1 argument ({} given)", args.len()));
    }
    let Ok(fmt) = resolve_format(args[0]) else {
        return ptr::null_mut();
    };
    alloc_int_object(fmt.get().size as i64)
}

/// `pack(format, v1, v2, ...)` / `Struct.pack(v1, v2, ...)`.
unsafe extern "C" fn pack_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return fail("pack() argv pointer is null");
    };
    if args.is_empty() {
        return raise_type_error("pack() missing required format argument");
    }
    let Ok(resolved) = resolve_format(args[0]) else {
        return ptr::null_mut();
    };
    let fmt = resolved.get();
    let values = &args[1..];
    if values.len() != fmt.arg_count {
        return raise_struct_error(&format!(
            "pack expected {} items for packing (got {})",
            fmt.arg_count,
            values.len()
        ));
    }
    match pack_values(fmt, values) {
        Ok(bytes) => alloc_bytes_object(&bytes),
        Err(()) => ptr::null_mut(),
    }
}

/// `pack_into(format, buffer, offset, v1, ...)`.
unsafe extern "C" fn pack_into_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return fail("pack_into() argv pointer is null");
    };
    if args.len() < 3 {
        return raise_type_error(&format!(
            "pack_into() takes at least 3 arguments ({} given)",
            args.len()
        ));
    }
    let Ok(resolved) = resolve_format(args[0]) else {
        return ptr::null_mut();
    };
    let fmt = resolved.get();
    let values = &args[3..];
    if values.len() != fmt.arg_count {
        return raise_struct_error(&format!(
            "pack_into expected {} items for packing (got {})",
            fmt.arg_count,
            values.len()
        ));
    }
    let buffer = match writable_buffer_arg(args[1]) {
        Ok(buffer) => buffer,
        Err(message) => return raise_type_error(&message),
    };
    let offset = match int_arg(args[2], "offset") {
        Ok(offset) => offset as isize,
        Err(result) => return result,
    };
    let length = buffer.len() as isize;
    let offset = if offset < 0 { offset + length } else { offset };
    if offset < 0 || offset > length {
        return raise_struct_error(&format!(
            "offset {} out of range for {length}-byte buffer",
            offset - if offset < 0 { length } else { 0 }
        ));
    }
    if (length - offset) < fmt.size as isize {
        return raise_struct_error(&format!(
            "pack_into requires a buffer of at least {} bytes for packing {} bytes at offset {offset} (actual buffer size is {length})",
            offset as usize + fmt.size,
            fmt.size,
        ));
    }
    match pack_values(fmt, values) {
        Ok(bytes) => {
            buffer[offset as usize..offset as usize + fmt.size].copy_from_slice(&bytes);
            none()
        }
        Err(()) => ptr::null_mut(),
    }
}

/// `unpack(format, buffer)` / `Struct.unpack(buffer)`.
unsafe extern "C" fn unpack_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return fail("unpack() argv pointer is null");
    };
    if args.len() != 2 {
        return raise_type_error(&format!("unpack() takes exactly 2 arguments ({} given)", args.len()));
    }
    let Ok(resolved) = resolve_format(args[0]) else {
        return ptr::null_mut();
    };
    let fmt = resolved.get();
    let buffer = match buffer_arg(args[1]) {
        Ok(buffer) => buffer,
        Err(message) => return raise_type_error(&message),
    };
    if buffer.len() != fmt.size {
        return raise_struct_error(&format!("unpack requires a buffer of {} bytes", fmt.size));
    }
    unpack_record(fmt, buffer)
}

/// `unpack_from(format, buffer, offset=0)` / `Struct.unpack_from(buffer, offset=0)`.
unsafe extern "C" fn unpack_from_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return fail("unpack_from() argv pointer is null");
    };
    if !(2..=3).contains(&args.len()) {
        return raise_type_error(&format!(
            "unpack_from() takes 2 to 3 arguments ({} given)",
            args.len()
        ));
    }
    let Ok(resolved) = resolve_format(args[0]) else {
        return ptr::null_mut();
    };
    let fmt = resolved.get();
    let Some(buffer_object) = optional_arg(args, 1) else {
        return raise_type_error("unpack_from() missing required argument 'buffer'");
    };
    let buffer = match buffer_arg(buffer_object) {
        Ok(buffer) => buffer,
        Err(message) => return raise_type_error(&message),
    };
    let offset = match optional_arg(args, 2) {
        Some(object) => match int_arg(object, "offset") {
            Ok(offset) => offset as isize,
            Err(result) => return result,
        },
        None => 0,
    };
    let length = buffer.len() as isize;
    let original_offset = offset;
    let offset = if offset < 0 { offset + length } else { offset };
    if offset < 0 || offset > length {
        return raise_struct_error(&format!(
            "offset {original_offset} out of range for {length}-byte buffer"
        ));
    }
    if (length - offset) < fmt.size as isize {
        return raise_struct_error(&format!(
            "unpack_from requires a buffer of at least {} bytes for unpacking {} bytes at offset {offset} (actual buffer size is {length})",
            offset as usize + fmt.size,
            fmt.size,
        ));
    }
    unpack_record(fmt, &buffer[offset as usize..offset as usize + fmt.size])
}

/// `iter_unpack(format, buffer)` / `Struct.iter_unpack(buffer)`.
unsafe extern "C" fn iter_unpack_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return fail("iter_unpack() argv pointer is null");
    };
    if args.len() != 2 {
        return raise_type_error(&format!(
            "iter_unpack() takes exactly 2 arguments ({} given)",
            args.len()
        ));
    }
    let Ok(resolved) = resolve_format(args[0]) else {
        return ptr::null_mut();
    };
    let fmt = resolved.get();
    if fmt.size == 0 {
        return raise_struct_error("cannot iterate with size 0");
    }
    let buffer = match buffer_arg(args[1]) {
        Ok(buffer) => buffer,
        Err(message) => return raise_type_error(&message),
    };
    if buffer.len() % fmt.size != 0 {
        return raise_struct_error(&format!(
            "iterable buffer size ({}) is not a multiple of the struct size ({})",
            buffer.len(),
            fmt.size
        ));
    }
    Box::into_raw(Box::new(PyUnpackIterator {
        ob_base: PyObjectHeader::new(*UNPACK_ITER_TYPE as *mut PyType),
        fmt: fmt.clone(),
        data: buffer.to_vec(),
        pos: 0,
    }))
    .cast::<PyObject>()
}

/// `_clearcache()`: formats are re-parsed per call, so this is a no-op.
unsafe extern "C" fn clearcache_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_standard_sizes_and_counts() {
        let fmt = parse_format("<2i3sxq").unwrap();
        assert_eq!(fmt.size, 2 * 4 + 3 + 1 + 8);
        assert_eq!(fmt.arg_count, 2 + 1 + 1);
        assert!(fmt.little);
    }

    #[test]
    fn parse_native_alignment_pads_before_items() {
        // '@hi' on every supported host: h(2) + pad(2) + i(4) = 8.
        let fmt = parse_format("@hi").unwrap();
        assert_eq!(fmt.size, 8);
        // '@ih': i(4) + h(2) = 6 with no trailing padding.
        let fmt = parse_format("@ih").unwrap();
        assert_eq!(fmt.size, 6);
    }

    #[test]
    fn parse_rejects_standard_native_only_codes() {
        assert_eq!(parse_format("<n").unwrap_err(), "bad char in struct format");
        assert_eq!(parse_format("=P").unwrap_err(), "bad char in struct format");
        assert!(parse_format("@n").is_ok());
        assert_eq!(parse_format("4").unwrap_err(), "repeat count given without format specifier");
        assert_eq!(parse_format("<z").unwrap_err(), "bad char in struct format");
    }

    #[test]
    fn uint_round_trips_both_orders() {
        let mut buffer = [0u8; 4];
        write_uint(&mut buffer, 0x1234_5678, true);
        assert_eq!(buffer, [0x78, 0x56, 0x34, 0x12]);
        assert_eq!(read_uint(&buffer, true), 0x1234_5678);
        write_uint(&mut buffer, 0x1234_5678, false);
        assert_eq!(buffer, [0x12, 0x34, 0x56, 0x78]);
        assert_eq!(read_uint(&buffer, false), 0x1234_5678);
    }

    #[test]
    fn f16_round_trips_and_overflows() {
        for (value, bits) in [
            (0.0, 0x0000u16),
            (-0.0, 0x8000),
            (1.0, 0x3c00),
            (-2.0, 0xc000),
            (65504.0, 0x7bff),
            (6.103515625e-05, 0x0400),    // smallest normal
            (5.960464477539063e-08, 0x0001), // smallest subnormal
            (f64::INFINITY, 0x7c00),
        ] {
            assert_eq!(f64_to_f16_bits(value), Some(bits), "encoding {value}");
            if value.is_finite() {
                assert_eq!(f16_bits_to_f64(bits), value, "decoding {bits:#x}");
            }
        }
        assert_eq!(f64_to_f16_bits(65536.0), None);
        assert_eq!(f64_to_f16_bits(65520.0), None, "tie at the top rounds away");
        assert_eq!(f64_to_f16_bits(65519.0), Some(0x7bff), "below the tie rounds down");
        assert!(f64_to_f16_bits(f64::NAN).unwrap() & 0x7c00 == 0x7c00);
    }
}
