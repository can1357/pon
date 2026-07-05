//! Native `_operator`: the C-accelerated core behind `operator.py`.
//!
//! The vendored `operator.py` ends with `from _operator import *`, so these
//! carriers replace the pure-Python fallbacks exactly like CPython's C
//! module.  That matters beyond speed: CPython's operator functions are
//! `builtin_function_or_method`s, which implement no descriptor protocol —
//! `glob._StringGlobber.concat_path = operator.add` must NOT bind the
//! instance when `pathlib.Path.glob` calls `self.concat_path(a, b)` (meson's
//! BLAS framework probe globs `*.framework/` through exactly that
//! attribute).  A pure-Python fallback binds the receiver and breaks the
//! call shape.
//!
//! Names with no native primitive (`attrgetter`, `itemgetter`,
//! `methodcaller`, `call`, `concat`/`iconcat`, `countOf`, `indexOf`,
//! `length_hint`) are intentionally absent: the pure-Python definitions stay
//! live, and the star-import only overrides what this module exports.

use super::install_module;
use crate::{abi, intern::intern, object::PyObject, types::exc::ExceptionKind};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

unsafe fn call_args<'a>(argv: *mut *mut PyObject, argc: usize) -> &'a [*mut PyObject] {
	if argv.is_null() || argc == 0 {
		&[]
	} else {
		// SAFETY: The caller passed `argc` live argument slots.
		unsafe { std::slice::from_raw_parts(argv, argc) }
	}
}

fn arity_error(name: &str, expected: usize, got: usize) -> *mut PyObject {
	crate::abi::exc::raise_kind_error_text(
		ExceptionKind::TypeError,
		&format!("{name} expected {expected} arguments, got {got}"),
	)
}

macro_rules! binary_entries {
    ($(($entry:ident, $name:literal, $op:expr)),+ $(,)?) => {
        $(
            unsafe extern "C" fn $entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
                let args = unsafe { call_args(argv, argc) };
                if args.len() != 2 {
                    return arity_error($name, 2, args.len());
                }
                // SAFETY: Binary dispatch untags and validates both operands.
                unsafe { abi::number::pon_binary_op($op, args[0], args[1], core::ptr::null_mut()) }
            }
        )+
    };
}

binary_entries!(
	(op_add, "add", abi::number::BINARY_ADD),
	(op_sub, "sub", abi::number::BINARY_SUB),
	(op_mul, "mul", abi::number::BINARY_MUL),
	(op_matmul, "matmul", abi::number::BINARY_MATMUL),
	(op_truediv, "truediv", abi::number::BINARY_DIV),
	(op_floordiv, "floordiv", abi::number::BINARY_FLOORDIV),
	(op_mod, "mod", abi::number::BINARY_MOD),
	(op_pow, "pow", abi::number::BINARY_POW),
	(op_lshift, "lshift", abi::number::BINARY_LSHIFT),
	(op_rshift, "rshift", abi::number::BINARY_RSHIFT),
	(op_and, "and_", abi::number::BINARY_AND),
	(op_or, "or_", abi::number::BINARY_OR),
	(op_xor, "xor", abi::number::BINARY_XOR),
);

macro_rules! inplace_entries {
    ($(($entry:ident, $name:literal, $op:expr)),+ $(,)?) => {
        $(
            unsafe extern "C" fn $entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
                let args = unsafe { call_args(argv, argc) };
                if args.len() != 2 {
                    return arity_error($name, 2, args.len());
                }
                // SAFETY: The in-place dispatcher untags both operands, runs
                // the receiver's `__i*__` slot (list extend, PEP 584 dict
                // merge), and falls back to the plain binary path.
                unsafe { abi::number::pon_number_inplace($op, args[0], args[1], core::ptr::null_mut()) }
            }
        )+
    };
}

inplace_entries!(
	(op_iadd, "iadd", abi::number::BINARY_ADD),
	(op_isub, "isub", abi::number::BINARY_SUB),
	(op_imul, "imul", abi::number::BINARY_MUL),
	(op_imatmul, "imatmul", abi::number::BINARY_MATMUL),
	(op_itruediv, "itruediv", abi::number::BINARY_DIV),
	(op_ifloordiv, "ifloordiv", abi::number::BINARY_FLOORDIV),
	(op_imod, "imod", abi::number::BINARY_MOD),
	(op_ipow, "ipow", abi::number::BINARY_POW),
	(op_ilshift, "ilshift", abi::number::BINARY_LSHIFT),
	(op_irshift, "irshift", abi::number::BINARY_RSHIFT),
	(op_iand, "iand", abi::number::BINARY_AND),
	(op_ior, "ior", abi::number::BINARY_OR),
	(op_ixor, "ixor", abi::number::BINARY_XOR),
);

macro_rules! compare_entries {
    ($(($entry:ident, $name:literal, $op:expr)),+ $(,)?) => {
        $(
            unsafe extern "C" fn $entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
                let args = unsafe { call_args(argv, argc) };
                if args.len() != 2 {
                    return arity_error($name, 2, args.len());
                }
                // SAFETY: Rich-compare dispatch untags and validates operands.
                unsafe { abi::object::pon_rich_compare($op, args[0], args[1], core::ptr::null_mut()) }
            }
        )+
    };
}

compare_entries!(
	(op_lt, "lt", abi::object::RICH_LT),
	(op_le, "le", abi::object::RICH_LE),
	(op_eq, "eq", abi::object::RICH_EQ),
	(op_ne, "ne", abi::object::RICH_NE),
	(op_ge, "ge", abi::object::RICH_GE),
	(op_gt, "gt", abi::object::RICH_GT),
);

macro_rules! unary_entries {
    ($(($entry:ident, $name:literal, $op:expr)),+ $(,)?) => {
        $(
            unsafe extern "C" fn $entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
                let args = unsafe { call_args(argv, argc) };
                if args.len() != 1 {
                    return arity_error($name, 1, args.len());
                }
                // SAFETY: Unary dispatch untags and validates the operand.
                unsafe { abi::number::pon_unary_op($op, args[0], core::ptr::null_mut()) }
            }
        )+
    };
}

unary_entries!(
	(op_neg, "neg", abi::number::UNARY_NEG),
	(op_pos, "pos", abi::number::UNARY_POS),
	(op_invert, "invert", abi::number::UNARY_INVERT),
);

unsafe extern "C" fn op_not(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return arity_error("not_", 1, args.len());
	}
	// SAFETY: Truth dispatch untags and validates the operand.
	match unsafe { abi::object::pon_is_true(args[0]) } {
		-1 => core::ptr::null_mut(),
		value => unsafe { abi::number::pon_const_bool(i32::from(value == 0)) },
	}
}

unsafe extern "C" fn op_truth(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return arity_error("truth", 1, args.len());
	}
	// SAFETY: Truth dispatch untags and validates the operand.
	match unsafe { abi::object::pon_is_true(args[0]) } {
		-1 => core::ptr::null_mut(),
		value => unsafe { abi::number::pon_const_bool(value) },
	}
}

/// Raw tagged-word identity: the same physical comparison compiled `is`
/// performs (heap objects by pointer, immediate small ints by value).
fn same_object(a: *mut PyObject, b: *mut PyObject) -> bool {
	a == b
}

unsafe extern "C" fn op_is(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return arity_error("is_", 2, args.len());
	}
	unsafe { abi::number::pon_const_bool(i32::from(same_object(args[0], args[1]))) }
}

unsafe extern "C" fn op_is_not(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return arity_error("is_not", 2, args.len());
	}
	unsafe { abi::number::pon_const_bool(i32::from(!same_object(args[0], args[1]))) }
}

unsafe extern "C" fn op_is_none(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return arity_error("is_none", 1, args.len());
	}
	let none = unsafe { abi::pon_none() };
	unsafe { abi::number::pon_const_bool(i32::from(same_object(args[0], none))) }
}

unsafe extern "C" fn op_is_not_none(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return arity_error("is_not_none", 1, args.len());
	}
	let none = unsafe { abi::pon_none() };
	unsafe { abi::number::pon_const_bool(i32::from(!same_object(args[0], none))) }
}

unsafe extern "C" fn op_index(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 1 {
		return arity_error("index", 1, args.len());
	}
	// SAFETY: `__index__` dispatch untags and validates the operand.
	unsafe { abi::number::pon_index(args[0]) }
}

unsafe extern "C" fn op_getitem(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return arity_error("getitem", 2, args.len());
	}
	// SAFETY: Subscript dispatch untags and validates both operands.
	unsafe { abi::object::pon_subscript_get(args[0], args[1], core::ptr::null_mut()) }
}

unsafe extern "C" fn op_setitem(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 3 {
		return arity_error("setitem", 3, args.len());
	}
	// SAFETY: Subscript dispatch untags and validates the operands.
	let result = unsafe { abi::map::pon_subscript_set(args[0], args[1], args[2]) };
	if result.is_null() {
		return core::ptr::null_mut();
	}
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn op_delitem(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return arity_error("delitem", 2, args.len());
	}
	// SAFETY: Subscript dispatch untags and validates the operands.
	let result = unsafe { abi::map::pon_subscript_del(args[0], args[1]) };
	if result.is_null() {
		return core::ptr::null_mut();
	}
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn op_contains(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { call_args(argv, argc) };
	if args.len() != 2 {
		return arity_error("contains", 2, args.len());
	}
	// SAFETY: Containment dispatch untags and validates the operands.
	match unsafe { abi::map::pon_contains(args[0], args[1]) } {
		-1 => core::ptr::null_mut(),
		value => unsafe { abi::number::pon_const_bool(value) },
	}
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let name = "_operator";
	let doc = "Operator interface.\n\nThis module exports a set of functions implemented in C \
	           corresponding\nto the intrinsic operators of Python.  For example, operator.add(x, \
	           y)\nis equivalent to the expression x+y.  The function names are those\nused for \
	           special methods; variants without leading and trailing\n'__' are also provided for \
	           convenience.";
	// SAFETY: Runtime allocation helpers; NULL is checked below.
	let name_object = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
	let doc_object = unsafe { abi::pon_const_str(doc.as_ptr(), doc.len()) };
	if name_object.is_null() || doc_object.is_null() {
		return Err("failed to allocate _operator module strings".to_owned());
	}
	let functions: &[(&str, BuiltinFn)] = &[
		("abs", crate::native::builtins_mod::builtin_abs),
		("add", op_add),
		("and_", op_and),
		("contains", op_contains),
		("delitem", op_delitem),
		("eq", op_eq),
		("floordiv", op_floordiv),
		("ge", op_ge),
		("getitem", op_getitem),
		("gt", op_gt),
		("iadd", op_iadd),
		("iand", op_iand),
		("ifloordiv", op_ifloordiv),
		("ilshift", op_ilshift),
		("imatmul", op_imatmul),
		("imod", op_imod),
		("imul", op_imul),
		("index", op_index),
		("inv", op_invert),
		("invert", op_invert),
		("ior", op_ior),
		("ipow", op_ipow),
		("irshift", op_irshift),
		("is_", op_is),
		("is_none", op_is_none),
		("is_not", op_is_not),
		("is_not_none", op_is_not_none),
		("isub", op_isub),
		("itruediv", op_itruediv),
		("ixor", op_ixor),
		("le", op_le),
		("lshift", op_lshift),
		("lt", op_lt),
		("matmul", op_matmul),
		("mod", op_mod),
		("mul", op_mul),
		("ne", op_ne),
		("neg", op_neg),
		("not_", op_not),
		("or_", op_or),
		("pos", op_pos),
		("pow", op_pow),
		("rshift", op_rshift),
		("setitem", op_setitem),
		("sub", op_sub),
		("truediv", op_truediv),
		("truth", op_truth),
		("xor", op_xor),
	];
	let mut attrs = vec![(intern("__name__"), name_object), (intern("__doc__"), doc_object)];
	for (function_name, entry) in functions {
		// SAFETY: Live builtin entry point with the runtime calling convention.
		let function = unsafe {
			abi::pon_make_function(
				*entry as *const u8,
				crate::builtins::variadic_arity(),
				intern(function_name),
			)
		};
		if function.is_null() {
			return Err(format!("failed to allocate _operator.{function_name}"));
		}
		crate::types::function::mark_native_function(function);
		attrs.push((intern(function_name), function));
	}
	install_module(name, attrs)
}
