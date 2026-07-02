//! Builtins owned by the K1b compatibility batch.
//!
//! The public entry points keep the same argv/argc ABI as the native builtins
//! registry.  Shared behavior lives here so `builtins_mod.rs` only needs to queue
//! registry rows/wrappers through its owner.

use core::cmp::Ordering;
use core::ptr;

use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{One, Signed, ToPrimitive, Zero};

use crate::abi;
use crate::abstract_op;
use crate::intern::{intern, resolve};
use crate::object::{PyObject, PyType};
use crate::thread_state::{pon_err_clear, pon_err_occurred, thread_state_lock};
use crate::types::{bool_, dict, float, int, lazy_iter, type_};
use crate::types::type_::{PyClassDict, PyHeapInstance};

const DEFAULT_SENTINEL: *mut PyObject = ptr::null_mut();

pub unsafe extern "C" fn builtin_vars(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return raise_type_error("vars() received a null argv pointer");
    };
    match args.len() {
        0 => unsafe { crate::native::builtins_mod::builtin_locals(ptr::null_mut(), 0) },
        1 => match materialize_namespace(args[0]) {
            Ok(dict) => dict,
            Err(message) => raise_type_error(&message),
        },
        _ => raise_type_error(&format!("vars expected at most 1 argument, got {}", args.len())),
    }
}

pub unsafe extern "C" fn builtin_dir(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return raise_type_error("dir() received a null argv pointer");
    };
    if args.len() > 1 {
        return raise_type_error(&format!("dir expected at most 1 argument, got {}", args.len()));
    }

    let mut names = if args.is_empty() {
        match unsafe { names_from_mapping(crate::native::builtins_mod::builtin_locals(ptr::null_mut(), 0)) } {
            Ok(names) => names,
            Err(message) => return raise_type_error(&message),
        }
    } else if let Some(dir_method) = unsafe { try_get_attr(args[0], "__dir__") } {
        let result = unsafe { abi::pon_call(dir_method, ptr::null_mut(), 0) };
        if result.is_null() {
            return ptr::null_mut();
        }
        match collect_iterable(result) {
            Ok(values) => values.into_iter().map(name_text).collect(),
            Err(message) => return raise_type_error(&message),
        }
    } else if let Some(namespace) = crate::import::module_namespace_for_object(args[0]) {
        // Module arm: CPython's `module.__dir__` returns `list(module.__dict__)`.
        // Module attrs live on the module object (mirrored into the registered
        // namespace dict), not in any class dict, so the fallback below would
        // see nothing.
        match namespace.and_then(|dict| unsafe { names_from_mapping(dict) }) {
            Ok(names) => names,
            Err(message) => return raise_type_error(&message),
        }
    } else {
        names_for_object(args[0])
    };

    names.sort();
    names.dedup();
    build_str_list(names)
}

pub unsafe extern "C" fn builtin_divmod(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 2, "divmod") }) else {
        return ptr::null_mut();
    };

    if let Some(result) = unsafe { call_binary_dunder(args[0], args[1], "__divmod__") } {
        return result;
    }
    match unsafe { (numeric_value(args[0]), numeric_value(args[1])) } {
        (Some(NumberValue::Int(left)), Some(NumberValue::Int(right))) => divmod_int(&left, &right),
        (Some(left), Some(right)) => divmod_float(left, right),
        _ => raise_type_error("unsupported operand type(s) for divmod()"),
    }
}

pub unsafe extern "C" fn builtin_pow(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return raise_type_error("pow() received a null argv pointer");
    };
    if !(2..=3).contains(&args.len()) {
        return raise_type_error(&format!("pow() expected 2 or 3 arguments, got {}", args.len()));
    }
    let modulo = args.get(2).copied().filter(|value| !unsafe { is_none(*value) });

    if let Some(modulo) = modulo {
        let (Some(base), Some(exp), Some(modulus)) = (
            unsafe { object_to_integer(args[0]) },
            unsafe { object_to_integer(args[1]) },
            unsafe { object_to_integer(modulo) },
        ) else {
            return raise_type_error("pow() 3rd argument not allowed unless all arguments are integers");
        };
        return pow_int_mod(&base, &exp, &modulus);
    }

    if let Some(result) = unsafe { call_pow_dunder(args[0], args[1], ptr::null_mut()) } {
        return result;
    }
    unsafe { abi::number::pon_binary_op(abstract_op::BINARY_POW, args[0], args[1], ptr::null_mut()) }
}

pub unsafe extern "C" fn builtin_round(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return raise_type_error("round() received a null argv pointer");
    };
    if !(1..=2).contains(&args.len()) {
        return raise_type_error(&format!("round() expected 1 or 2 arguments, got {}", args.len()));
    }
    let ndigits = args.get(1).copied().filter(|value| !unsafe { is_none(*value) });

    if let Some(value) = unsafe { object_to_integer(args[0]) } {
        return match ndigits {
            Some(ndigits) => match unsafe { index_bigint(ndigits) }.and_then(|n| n.to_i64()) {
                Some(n) => int::from_bigint(round_bigint(value, n)),
                None => raise_type_error(&index_type_error(ndigits)),
            },
            None => int::from_bigint(value),
        };
    }
    if let Some(value) = unsafe { float::to_f64(args[0]) } {
        return match ndigits {
            Some(ndigits) => match unsafe { index_bigint(ndigits) }.and_then(|n| n.to_i64()) {
                Some(n) => float::from_f64(round_float_ndigits(value, n)),
                None => raise_type_error(&index_type_error(ndigits)),
            },
            None => match round_float_to_bigint(value) {
                Some(value) => int::from_bigint(value),
                None => raise_type_error("cannot convert float NaN to integer"),
            },
        };
    }

    if let Some(round_method) = unsafe { try_get_attr(args[0], "__round__") } {
        let mut call_args = ndigits.map_or_else(Vec::new, |value| vec![value]);
        return unsafe { abi::pon_call(round_method, call_args.as_mut_ptr(), call_args.len()) };
    }
    raise_type_error(&format!("type {} doesn't define __round__ method", type_name(args[0])))
}

pub unsafe extern "C" fn builtin_format(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return raise_type_error("format() received a null argv pointer");
    };
    if !(1..=2).contains(&args.len()) {
        return raise_type_error(&format!("format() expected 1 or 2 arguments, got {}", args.len()));
    }
    let spec_object = if let Some(spec) = args.get(1).copied() {
        if unsafe { object_to_string(spec) }.is_none() {
            return raise_type_error(&format!("format() argument 2 must be str, not {}", type_name(spec)));
        }
        spec
    } else {
        alloc_str("")
    };
    if spec_object.is_null() {
        return ptr::null_mut();
    }
    let spec = unsafe { object_to_string(spec_object) }.unwrap_or_default();

    if let Some(format_method) = unsafe { try_get_attr(args[0], "__format__") } {
        let mut call_args = [spec_object];
        let result = unsafe { abi::pon_call(format_method, call_args.as_mut_ptr(), 1) };
        if result.is_null() {
            return ptr::null_mut();
        }
        if unsafe { object_to_string(result) }.is_some() {
            return result;
        }
        return raise_type_error(&format!("__format__ must return a str, not {}", type_name(result)));
    }

    match abi::str_::format_object_with_spec(args[0], &spec) {
        Ok(text) => alloc_str(&text),
        Err(message) => raise_type_error(&message),
    }
}

pub unsafe extern "C" fn builtin_chr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 1, "chr") }) else {
        return ptr::null_mut();
    };
    let Some(value) = (unsafe { index_bigint(args[0]) }) else {
        return raise_type_error(&index_type_error(args[0]));
    };
    let Some(code) = value.to_u32() else {
        return raise_value_error("chr() arg not in range(0x110000)");
    };
    let Some(ch) = char::from_u32(code) else {
        return raise_value_error("chr() arg not in range(0x110000)");
    };
    alloc_str(&ch.to_string())
}

pub unsafe extern "C" fn builtin_ord(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 1, "ord") }) else {
        return ptr::null_mut();
    };
    let Some(text) = (unsafe { object_to_string(args[0]) }) else {
        return raise_type_error(&format!("ord() expected string of length 1, but {} found", type_name(args[0])));
    };
    let mut chars = text.chars();
    let Some(ch) = chars.next() else {
        return raise_type_error("ord() expected a character, but string of length 0 found");
    };
    if chars.next().is_some() {
        let len = text.chars().count();
        return raise_type_error(&format!("ord() expected a character, but string of length {len} found"));
    }
    int::from_i64(i64::from(ch as u32))
}

pub unsafe extern "C" fn builtin_bin(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { radix_builtin(argv, argc, "bin", 2, "0b") }
}

pub unsafe extern "C" fn builtin_oct(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { radix_builtin(argv, argc, "oct", 8, "0o") }
}

pub unsafe extern "C" fn builtin_hex(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { radix_builtin(argv, argc, "hex", 16, "0x") }
}

pub unsafe extern "C" fn builtin_min(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { min_max(argv, argc, false) }
}

pub unsafe extern "C" fn builtin_max(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { min_max(argv, argc, true) }
}

pub unsafe extern "C" fn builtin_sum(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return raise_type_error("sum() received a null argv pointer");
    };
    if args.is_empty() {
        return raise_type_error("sum() takes at least 1 positional argument (0 given)");
    }
    if args.len() > 2 {
        return raise_type_error(&format!("sum() takes at most 2 arguments ({} given)", args.len()));
    }
    let mut total = args.get(1).copied().unwrap_or_else(|| int::from_i64(0));
    // Only the start value is type-checked; a string-like item surfaces the
    // generic `+` TypeError instead, as in CPython.
    match type_name(total) {
        "str" => return raise_type_error("sum() can't sum strings [use ''.join(seq) instead]"),
        "bytes" => return raise_type_error("sum() can't sum bytes [use b''.join(seq) instead]"),
        "bytearray" => return raise_type_error("sum() can't sum bytearray [use b''.join(seq) instead]"),
        _ => {}
    }
    let items = match collect_iterable(args[0]) {
        Ok(items) => items,
        Err(message) => return raise_type_error(&message),
    };
    let mut items = items.into_iter().map(crate::tag::untag_arg);

    // Exact-int phase: exact ints and bools accumulate as a BigInt (CPython
    // keeps a C long and escapes to object adds on overflow; both routes are
    // exact, so the only observable transition is the first non-int item).
    if unsafe { int::is_exact_int(total) } {
        let Some(mut int_total) = (unsafe { int::to_bigint(total) }) else {
            return raise_type_error("sum() start is not an int");
        };
        loop {
            let Some(item) = items.next() else {
                return int::from_bigint(int_total);
            };
            if let Some(value) = unsafe { sum_exact_int_item(item) } {
                int_total += value;
                continue;
            }
            // First non-int item: box the subtotal and add generically; an
            // exact-float result rides the float fast path below (CPython's
            // fall-through between its typed loops).
            total = unsafe { abi::number::pon_binary_op(abstract_op::BINARY_ADD, int::from_bigint(int_total), item, ptr::null_mut()) };
            if total.is_null() {
                return ptr::null_mut();
            }
            total = crate::tag::untag_arg(total);
            break;
        }
    }

    // Exact-float phase: Neumaier-compensated summation (gh-100425). Ints
    // that fit an i64 fold in uncompensated, exactly as CPython's loop does.
    if unsafe { float::is_exact_float(total) } {
        let mut f_result = unsafe { float::to_f64(total) }.unwrap_or(0.0);
        let mut c = 0.0f64;
        loop {
            let Some(item) = items.next() else {
                // Skip a zero or non-finite carry so inf/overflowed sums are
                // not converted to NaN by the compensation.
                if c != 0.0 && c.is_finite() {
                    f_result += c;
                }
                return float::from_f64(f_result);
            };
            if unsafe { float::is_exact_float(item) } {
                let x = unsafe { float::to_f64(item) }.unwrap_or(f64::NAN);
                let t = f_result + x;
                c += if f_result.abs() >= x.abs() { (f_result - t) + x } else { (x - t) + f_result };
                f_result = t;
                continue;
            }
            if let Some(value) = unsafe { object_to_integer(item) }.and_then(|value| value.to_i64()) {
                #[allow(clippy::cast_precision_loss)] // CPython folds via `(double)value`.
                {
                    f_result += value as f64;
                }
                continue;
            }
            // Non-numeric item: flush the compensation and fall back to the
            // generic loop for the rest, as CPython does.
            if c != 0.0 && c.is_finite() {
                f_result += c;
            }
            total = unsafe { abi::number::pon_binary_op(abstract_op::BINARY_ADD, float::from_f64(f_result), item, ptr::null_mut()) };
            if total.is_null() {
                return ptr::null_mut();
            }
            total = crate::tag::untag_arg(total);
            break;
        }
    }

    for item in items {
        total = unsafe { abi::number::pon_binary_op(abstract_op::BINARY_ADD, total, item, ptr::null_mut()) };
        if total.is_null() {
            return ptr::null_mut();
        }
        total = crate::tag::untag_arg(total);
    }
    total
}

/// An item `sum`'s exact-int phase folds: exact ints and bools, matching
/// CPython's `PyLong_CheckExact(item) || PyBool_Check(item)` gate (subclasses
/// take the generic route so their `__radd__` stays observable).
unsafe fn sum_exact_int_item(item: *mut PyObject) -> Option<BigInt> {
    if let Some(value) = unsafe { bool_::to_bool(item) } {
        return Some(BigInt::from(i64::from(value)));
    }
    if unsafe { int::is_exact_int(item) } {
        return unsafe { int::to_bigint(item) };
    }
    None
}

pub unsafe extern "C" fn builtin_sorted(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return raise_type_error("sorted() received a null argv pointer");
    };
    let (iterable, key, reverse) = match args {
        [iterable] => (*iterable, ptr::null_mut(), false),
        [iterable, options] => match unsafe { lazy_iter::sort_options_value(*options) } {
            Some(options) => (*iterable, options.key, options.reverse),
            None => return raise_type_error(&format!("sorted expected 1 argument, got {}", args.len())),
        },
        _ => return raise_type_error(&format!("sorted expected 1 argument, got {}", args.len())),
    };
    let mut items = match collect_iterable(iterable) {
        Ok(items) => items,
        Err(message) => return raise_type_error(&message),
    };
    match stable_sort(&mut items, key, reverse) {
        Ok(()) => build_list(items),
        Err(()) => ptr::null_mut(),
    }
}

pub unsafe extern "C" fn builtin_slice(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return raise_type_error("slice() received a null argv pointer");
    };
    if !(1..=3).contains(&args.len()) {
        return raise_type_error(&format!("slice expected at least 1 argument, got {}", args.len()));
    }
    let none = unsafe { abi::pon_none() };
    if none.is_null() {
        return ptr::null_mut();
    }
    let (start, stop, step) = match args.len() {
        1 => (none, args[0], none),
        2 => (args[0], args[1], none),
        3 => (args[0], args[1], args[2]),
        _ => unreachable!(),
    };
    unsafe { abi::seq::pon_build_slice(start, stop, step) }
}

pub unsafe extern "C" fn builtin_reversed(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 1, "reversed") }) else {
        return ptr::null_mut();
    };
    if let Some(reversed_method) = unsafe { try_get_attr(args[0], "__reversed__") } {
        return unsafe { abi::pon_call(reversed_method, ptr::null_mut(), 0) };
    }
    let len = unsafe { abi::seq::pon_seq_len(args[0]) };
    if len < 0 {
        return raise_type_error(&format!("'{}' object is not reversible", type_name(args[0])));
    }
    lazy_iter::new_reversed(args[0], len)
}

pub unsafe extern "C" fn builtin_map(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return raise_type_error("map() received a null argv pointer");
    };
    if args.len() < 2 {
        return raise_type_error(&format!("map() must have at least two arguments, got {}", args.len()));
    }
    let mut iters = Vec::with_capacity(args.len() - 1);
    for arg in &args[1..] {
        let iter = unsafe { abi::pon_get_iter(*arg, ptr::null_mut()) };
        if iter.is_null() {
            return ptr::null_mut();
        }
        iters.push(iter);
    }
    lazy_iter::new_map(args[0], iters)
}

pub unsafe extern "C" fn builtin_filter(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 2, "filter") }) else {
        return ptr::null_mut();
    };
    let iter = unsafe { abi::pon_get_iter(args[1], ptr::null_mut()) };
    if iter.is_null() {
        return ptr::null_mut();
    }
    lazy_iter::new_filter(args[0], iter)
}

pub unsafe extern "C" fn builtin_zip(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return raise_type_error("zip() received a null argv pointer");
    };
    let mut positional = args;
    let mut strict = false;
    if let Some(last) = args.last().copied() {
        if let Some(value) = unsafe { lazy_iter::zip_strict_marker_value(last) } {
            strict = value;
            positional = &args[..args.len() - 1];
        }
    }
    let mut iters = Vec::with_capacity(positional.len());
    for arg in positional.iter().copied() {
        let iter = unsafe { abi::pon_get_iter(arg, ptr::null_mut()) };
        if iter.is_null() {
            return ptr::null_mut();
        }
        iters.push(iter);
    }
    lazy_iter::new_zip(iters, strict)
}

unsafe fn min_max(argv: *mut *mut PyObject, argc: usize, max_mode: bool) -> *mut PyObject {
    let name = if max_mode { "max" } else { "min" };
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        return raise_type_error(&format!("{name}() received a null argv pointer"));
    };
    if args.is_empty() {
        return raise_type_error(&format!("{name} expected at least 1 argument, got 0"));
    }
    let mut positional = args;
    let mut options = lazy_iter::MinMaxOptions {
        key: ptr::null_mut(),
        default: DEFAULT_SENTINEL,
        has_default: false,
    };
    if let Some(last) = args.last().copied() {
        if let Some(value) = unsafe { lazy_iter::minmax_options_value(last) } {
            options = value;
            positional = &args[..args.len() - 1];
        }
    }
    if positional.is_empty() {
        return raise_type_error(&format!("{name} expected at least 1 argument, got 0"));
    }
    if options.has_default && positional.len() > 1 {
        return raise_type_error(&format!("Cannot specify a default for {name}() with multiple positional arguments"));
    }

    let items = if positional.len() == 1 {
        match collect_iterable(positional[0]) {
            Ok(items) => items,
            Err(message) => return raise_type_error(&message),
        }
    } else {
        positional.to_vec()
    };
    if items.is_empty() {
        return if options.has_default {
            options.default
        } else {
            raise_value_error(&format!("{name}() iterable argument is empty"))
        };
    }
    match select_min_max(items, options.key, max_mode) {
        Ok(value) => value,
        Err(()) => ptr::null_mut(),
    }
}

fn select_min_max(items: Vec<*mut PyObject>, key: *mut PyObject, max_mode: bool) -> Result<*mut PyObject, ()> {
    let mut iter = items.into_iter();
    let mut best = iter.next().expect("non-empty min/max items");
    let mut best_key = key_value(best, key)?;
    for item in iter {
        let item_key = key_value(item, key)?;
        let better = if max_mode {
            rich_bool(abstract_op::RICH_GT, item_key, best_key)?
        } else {
            rich_bool(abstract_op::RICH_LT, item_key, best_key)?
        };
        if better {
            best = item;
            best_key = item_key;
        }
    }
    Ok(best)
}

/// Insertion-based stable sort shared by `sorted()` and `list.sort()`:
/// key-mapped once, then rich-compare ordered (`reverse` flips to GT).
pub(crate) fn stable_sort(items: &mut Vec<*mut PyObject>, key: *mut PyObject, reverse: bool) -> Result<(), ()> {
    let mut keyed = Vec::with_capacity(items.len());
    for item in items.iter().copied() {
        keyed.push((item, key_value(item, key)?));
    }
    for index in 1..keyed.len() {
        let current = keyed[index];
        let mut pos = index;
        while pos > 0 {
            let should_shift = if reverse {
                rich_bool(abstract_op::RICH_GT, current.1, keyed[pos - 1].1)?
            } else {
                rich_bool(abstract_op::RICH_LT, current.1, keyed[pos - 1].1)?
            };
            if !should_shift {
                break;
            }
            keyed[pos] = keyed[pos - 1];
            pos -= 1;
        }
        keyed[pos] = current;
    }
    *items = keyed.into_iter().map(|(item, _)| item).collect();
    Ok(())
}

fn key_value(item: *mut PyObject, key: *mut PyObject) -> Result<*mut PyObject, ()> {
    if key.is_null() || unsafe { is_none(key) } {
        return Ok(item);
    }
    let mut args = [item];
    let value = unsafe { abi::pon_call(key, args.as_mut_ptr(), 1) };
    if value.is_null() { Err(()) } else { Ok(value) }
}

fn rich_bool(op: u8, left: *mut PyObject, right: *mut PyObject) -> Result<bool, ()> {
    let result = unsafe { abi::object::pon_rich_compare(op, left, right, ptr::null_mut()) };
    if result.is_null() {
        return Err(());
    }
    match unsafe { abi::pon_is_true(result) } {
        1 => Ok(true),
        0 => Ok(false),
        _ => Err(()),
    }
}

unsafe fn radix_builtin(argv: *mut *mut PyObject, argc: usize, name: &str, radix: u32, prefix: &str) -> *mut PyObject {
    let Some(args) = (unsafe { exact_args(argv, argc, 1, name) }) else {
        return ptr::null_mut();
    };
    let Some(value) = (unsafe { index_bigint(args[0]) }) else {
        return raise_type_error(&index_type_error(args[0]));
    };
    let sign = if value.is_negative() { "-" } else { "" };
    let digits = value.abs().to_str_radix(radix);
    alloc_str(&format!("{sign}{prefix}{digits}"))
}

fn divmod_int(left: &BigInt, right: &BigInt) -> *mut PyObject {
    if right.is_zero() {
        return raise_zero_division_error("division by zero");
    }
    let q = left.div_floor(right);
    let r = left.mod_floor(right);
    build_tuple(vec![int::from_bigint(q), int::from_bigint(r)])
}

fn divmod_float(left: NumberValue, right: NumberValue) -> *mut PyObject {
    let Some(left) = number_to_f64(left) else {
        return raise_type_error("int too large to convert to float");
    };
    let Some(right) = number_to_f64(right) else {
        return raise_type_error("int too large to convert to float");
    };
    if right == 0.0 {
        return raise_zero_division_error("division by zero");
    }
    let q = (left / right).floor();
    let r = left - q * right;
    build_tuple(vec![float::from_f64(q), float::from_f64(r)])
}

fn pow_int_mod(base: &BigInt, exp: &BigInt, modulus: &BigInt) -> *mut PyObject {
    if modulus.is_zero() {
        return raise_value_error("pow() 3rd argument cannot be 0");
    }
    let mut base = base.mod_floor(modulus);
    let mut exp = exp.clone();
    if exp.is_negative() {
        let Some(inverse) = mod_inverse(&base, modulus) else {
            return raise_value_error("base is not invertible for the given modulus");
        };
        base = inverse;
        exp = -exp;
    }
    int::from_bigint(pow_mod(base, exp, modulus))
}

fn pow_mod(mut base: BigInt, mut exp: BigInt, modulus: &BigInt) -> BigInt {
    let mut result = BigInt::one().mod_floor(modulus);
    while !exp.is_zero() {
        if exp.is_odd() {
            result = (result * &base).mod_floor(modulus);
        }
        exp >>= 1_usize;
        if !exp.is_zero() {
            base = (&base * &base).mod_floor(modulus);
        }
    }
    result.mod_floor(modulus)
}

fn mod_inverse(value: &BigInt, modulus: &BigInt) -> Option<BigInt> {
    let mut t = BigInt::zero();
    let mut new_t = BigInt::one();
    let mut r = modulus.abs();
    let mut new_r = value.mod_floor(&r);
    while !new_r.is_zero() {
        let quotient = &r / &new_r;
        let next_t = &t - &quotient * &new_t;
        t = new_t;
        new_t = next_t;
        let next_r = &r - quotient * &new_r;
        r = new_r;
        new_r = next_r;
    }
    if r != BigInt::one() {
        return None;
    }
    Some(t.mod_floor(modulus))
}

fn round_bigint(value: BigInt, ndigits: i64) -> BigInt {
    if ndigits >= 0 {
        return value;
    }
    let places = ndigits.unsigned_abs();
    let Some(divisor) = pow10_bigint(places) else {
        return BigInt::zero();
    };
    let sign_negative = value.is_negative();
    let abs_value = value.abs();
    let q = &abs_value / &divisor;
    let r = &abs_value % &divisor;
    let twice = &r << 1_usize;
    let rounded_q = match twice.cmp(&divisor) {
        Ordering::Less => q,
        Ordering::Greater => q + 1,
        Ordering::Equal => {
            if q.is_even() { q } else { q + 1 }
        }
    };
    let result = rounded_q * divisor;
    if sign_negative { -result } else { result }
}

fn pow10_bigint(places: u64) -> Option<BigInt> {
    let places = u32::try_from(places).ok()?;
    Some(BigInt::from(10_u8).pow(places))
}

fn round_float_to_bigint(value: f64) -> Option<BigInt> {
    if !value.is_finite() {
        return None;
    }
    format!("{value:.0}").parse::<BigInt>().ok()
}

fn round_float_ndigits(value: f64, ndigits: i64) -> f64 {
    if !value.is_finite() {
        return value;
    }
    if ndigits >= 0 {
        let precision = usize::try_from(ndigits).unwrap_or(usize::MAX).min(308);
        return format!("{value:.precision$}").parse::<f64>().unwrap_or(value);
    }
    let places = ndigits.unsigned_abs().min(i32::MAX as u64) as i32;
    let factor = 10_f64.powi(places);
    if !factor.is_finite() {
        return 0.0;
    }
    let scaled = value / factor;
    let rounded = format!("{scaled:.0}").parse::<f64>().unwrap_or(scaled);
    rounded * factor
}

#[derive(Debug)]
enum NumberValue {
    Int(BigInt),
    Float(f64),
}

unsafe fn numeric_value(object: *mut PyObject) -> Option<NumberValue> {
    if let Some(value) = unsafe { object_to_integer(object) } {
        return Some(NumberValue::Int(value));
    }
    unsafe { float::to_f64(object).map(NumberValue::Float) }
}

fn number_to_f64(value: NumberValue) -> Option<f64> {
    match value {
        NumberValue::Int(value) => value.to_f64(),
        NumberValue::Float(value) => Some(value),
    }
}

unsafe fn object_to_integer(object: *mut PyObject) -> Option<BigInt> {
    if let Some(value) = unsafe { bool_::to_bool(object) } {
        return Some(BigInt::from(if value { 1 } else { 0 }));
    }
    unsafe { int::to_bigint(object) }
}

unsafe fn index_bigint(object: *mut PyObject) -> Option<BigInt> {
    if let Some(value) = unsafe { object_to_integer(object) } {
        return Some(value);
    }
    let ty = unsafe { object.as_ref()?.ob_type.as_ref()? };
    if let Some(slot) = unsafe { ty.tp_as_number.as_ref().and_then(|methods| methods.nb_index) } {
        let result = unsafe { slot(object) };
        if result.is_null() {
            return None;
        }
        return unsafe { object_to_integer(result) };
    }
    if let Some(index_method) = unsafe { try_get_attr(object, "__index__") } {
        let result = unsafe { abi::pon_call(index_method, ptr::null_mut(), 0) };
        if result.is_null() {
            return None;
        }
        return unsafe { object_to_integer(result) };
    }
    None
}

fn index_type_error(object: *mut PyObject) -> String {
    format!("'{}' object cannot be interpreted as an integer", type_name(object))
}

unsafe fn call_binary_dunder(left: *mut PyObject, right: *mut PyObject, name: &str) -> Option<*mut PyObject> {
    let method = unsafe { try_get_attr(left, name)? };
    let mut args = [right];
    let result = unsafe { abi::pon_call(method, args.as_mut_ptr(), 1) };
    (!result.is_null()).then_some(result)
}

unsafe fn call_pow_dunder(base: *mut PyObject, exp: *mut PyObject, modulo: *mut PyObject) -> Option<*mut PyObject> {
    let method = unsafe { try_get_attr(base, "__pow__")? };
    let none;
    let mut args = if modulo.is_null() {
        vec![exp]
    } else {
        none = unsafe { abi::pon_none() };
        vec![exp, if modulo.is_null() { none } else { modulo }]
    };
    let result = unsafe { abi::pon_call(method, args.as_mut_ptr(), args.len()) };
    (!result.is_null()).then_some(result)
}

unsafe fn try_get_attr(object: *mut PyObject, name: &str) -> Option<*mut PyObject> {
    let result = unsafe { abi::pon_get_attr(object, intern(name), ptr::null_mut()) };
    if result.is_null() {
        if pon_err_occurred() {
            pon_err_clear();
        }
        None
    } else {
        Some(result)
    }
}

fn collect_iterable(object: *mut PyObject) -> Result<Vec<*mut PyObject>, String> {
    let iter = unsafe { abi::pon_get_iter(object, ptr::null_mut()) };
    if iter.is_null() {
        return Err(format!("'{}' object is not iterable", type_name(object)));
    }
    let mut items = Vec::new();
    loop {
        let value = unsafe { abi::pon_iter_next(iter, ptr::null_mut()) };
        if value.is_null() {
            if unsafe { current_exception_is("StopIteration") } {
                pon_err_clear();
                break;
            }
            return Err(crate::thread_state::pon_err_message().unwrap_or_else(|| "iteration failed".to_owned()));
        }
        items.push(value);
    }
    Ok(items)
}

unsafe fn current_exception_is(name: &str) -> bool {
    let current = thread_state_lock().current_exc;
    if current.is_null() || current == core::ptr::NonNull::<PyObject>::dangling().as_ptr() {
        return false;
    }
    let ty = unsafe { (*current).ob_type };
    !ty.is_null() && unsafe { (*ty).name() == name }
}

fn materialize_namespace(object: *mut PyObject) -> Result<*mut PyObject, String> {
    if object.is_null() {
        return Err("vars() argument must have __dict__ attribute".to_owned());
    }
    if let Some(dict) = unsafe { try_get_attr(object, "__dict__") } {
        return Ok(dict);
    }
    if let Some(class_dict) = unsafe { class_namespace(object) } {
        return class_dict_to_dict(class_dict);
    }
    Err("vars() argument must have __dict__ attribute".to_owned())
}

unsafe fn class_namespace(object: *mut PyObject) -> Option<*mut PyClassDict> {
    let ty = unsafe { object.as_ref()?.ob_type.as_ref()? };
    if ty.name() == "type" {
        let class = object.cast::<PyType>();
        let dict = unsafe { (*class).tp_dict.cast::<PyClassDict>() };
        return (!dict.is_null()).then_some(dict);
    }
    if ty.tp_dictoffset != 0 {
        let instance = object.cast::<PyHeapInstance>();
        let dict = unsafe { (*instance).dict };
        return (!dict.is_null()).then_some(dict);
    }
    None
}

fn class_dict_to_dict(class_dict: *mut PyClassDict) -> Result<*mut PyObject, String> {
    if class_dict.is_null() {
        return Err("namespace dictionary is NULL".to_owned());
    }
    let mut flat = Vec::new();
    for (name, value) in unsafe { &*class_dict }.iter() {
        let key = alloc_str(&resolve(name).unwrap_or_else(|| format!("<interned:{name}>")));
        if key.is_null() {
            return Err("failed to allocate namespace key".to_owned());
        }
        flat.push(key);
        flat.push(value);
    }
    Ok(unsafe { abi::map::pon_build_map(flat.as_mut_ptr(), flat.len() / 2) })
}

fn names_for_object(object: *mut PyObject) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(class_dict) = unsafe { class_namespace(object) } {
        names.extend(class_dict_names(class_dict));
    }
    if !object.is_null() {
        let ty = unsafe { (*object).ob_type.cast_mut() };
        for class in unsafe { crate::mro::mro_entries(ty) } {
            let dict = unsafe { (*class).tp_dict.cast::<PyClassDict>() };
            if !dict.is_null() {
                names.extend(class_dict_names(dict));
            }
        }
    }
    names
}

fn class_dict_names(class_dict: *mut PyClassDict) -> Vec<String> {
    if class_dict.is_null() {
        return Vec::new();
    }
    unsafe { &*class_dict }
        .iter()
        .map(|(name, _)| resolve(name).unwrap_or_else(|| format!("<interned:{name}>")))
        .collect()
}

unsafe fn names_from_mapping(mapping: *mut PyObject) -> Result<Vec<String>, String> {
    if mapping.is_null() {
        return Ok(Vec::new());
    }
    if unsafe { dict::is_dict(mapping) } {
        return Ok(unsafe { dict::dict_entries_snapshot(mapping)? }
            .into_iter()
            .map(|entry| name_text(entry.key))
            .collect());
    }
    Ok(Vec::new())
}

fn name_text(object: *mut PyObject) -> String {
    unsafe { object_to_string(object) }.unwrap_or_else(|| crate::native::builtins_mod::str_text(object))
}

fn build_str_list(names: Vec<String>) -> *mut PyObject {
    let values = names.into_iter().map(|name| alloc_str(&name)).collect::<Vec<_>>();
    build_list(values)
}

fn build_list(mut values: Vec<*mut PyObject>) -> *mut PyObject {
    unsafe { abi::seq::pon_build_list(values.as_mut_ptr(), values.len()) }
}

fn build_tuple(mut values: Vec<*mut PyObject>) -> *mut PyObject {
    unsafe { abi::seq::pon_build_tuple(values.as_mut_ptr(), values.len()) }
}

fn alloc_str(text: &str) -> *mut PyObject {
    unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

unsafe fn object_to_string(object: *mut PyObject) -> Option<String> {
    unsafe { type_::unicode_text(object).map(ToOwned::to_owned) }
}


fn type_name(object: *mut PyObject) -> &'static str {
    if object.is_null() {
        return "NULL";
    }
    let ty = unsafe { (*object).ob_type };
    if ty.is_null() {
        return "object";
    }
    unsafe { (*ty).name() }
}

unsafe fn argv_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
    if argc == 0 {
        Some(&[])
    } else if argv.is_null() {
        None
    } else {
        Some(unsafe { core::slice::from_raw_parts(argv, argc) })
    }
}

unsafe fn exact_args<'a>(argv: *mut *mut PyObject, argc: usize, expected: usize, name: &str) -> Option<&'a [*mut PyObject]> {
    let Some(args) = (unsafe { argv_slice(argv, argc) }) else {
        raise_type_error(&format!("{name}() received a null argv pointer"));
        return None;
    };
    if args.len() != expected {
        raise_type_error(&format!("{name}() expected {expected} arguments, got {}", args.len()));
        return None;
    }
    Some(args)
}

unsafe fn is_none(object: *mut PyObject) -> bool {
    type_name(object) == "NoneType"
}

fn raise_type_error(message: &str) -> *mut PyObject {
    unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn raise_value_error(message: &str) -> *mut PyObject {
    unsafe { abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) }
}

fn raise_zero_division_error(message: &str) -> *mut PyObject {
    unsafe { abi::exc::pon_raise_zero_division_error(message.as_ptr(), message.len()) }
}
