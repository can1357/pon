//! Minimal `_symtable` constants plus an honest analyzer blocker.
//!
//! The integer flags are CPython 3.14's public contract and let
//! `Lib/symtable.py` import. Building the raw symbol-table tree requires the
//! compiler's lexical-scope analysis pass; Pon does not expose that subsystem
//! to runtime native modules yet, so `_symtable.symtable()` raises a precise
//! `NotImplementedError` instead of returning fabricated scope data.

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{intern::intern, object::PyObject, types::exc::ExceptionKind};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

const CONSTANTS: [(&str, i64); 25] = [
	("CELL", 5),
	("DEF_ANNOT", 256),
	("DEF_BOUND", 134),
	("DEF_COMP_CELL", 2048),
	("DEF_COMP_ITER", 512),
	("DEF_FREE_CLASS", 64),
	("DEF_GLOBAL", 1),
	("DEF_IMPORT", 128),
	("DEF_LOCAL", 2),
	("DEF_NONLOCAL", 8),
	("DEF_PARAM", 4),
	("DEF_TYPE_PARAM", 1024),
	("FREE", 4),
	("GLOBAL_EXPLICIT", 2),
	("GLOBAL_IMPLICIT", 3),
	("LOCAL", 1),
	("SCOPE_MASK", 15),
	("SCOPE_OFF", 12),
	("TYPE_ANNOTATION", 3),
	("TYPE_CLASS", 1),
	("TYPE_FUNCTION", 0),
	("TYPE_MODULE", 2),
	("TYPE_TYPE_ALIAS", 4),
	("TYPE_TYPE_PARAMETERS", 5),
	("TYPE_TYPE_VARIABLE", 6),
];

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let mut attrs = Vec::with_capacity(CONSTANTS.len() + 3);
	attrs.push((intern("__name__"), str_object("_symtable")?));
	attrs.push((intern("USE"), int_object(16)?));
	for (name, value) in CONSTANTS {
		attrs.push((intern(name), int_object(value)?));
	}
	attrs.push(function_attr("symtable", symtable_entry)?);
	install_module("_symtable", attrs)
}

unsafe extern "C" fn symtable_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	crate::abi::exc::raise_kind_error_text(
		ExceptionKind::NotImplementedError,
		"_symtable.symtable() requires Pon compiler symbol-table analysis to be exposed at runtime",
	)
}

fn function_attr(name: &str, entry: BuiltinFn) -> Result<(u32, *mut PyObject), String> {
	let function =
		unsafe { crate::abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	(!function.is_null())
		.then_some((intern(name), function))
		.ok_or_else(|| format!("failed to allocate _symtable.{name}"))
}

fn int_object(value: i64) -> Result<*mut PyObject, String> {
	let object = unsafe { crate::abi::pon_const_int(value) };
	(!object.is_null())
		.then_some(object)
		.ok_or_else(|| format!("failed to allocate int {value}"))
}

fn str_object(text: &str) -> Result<*mut PyObject, String> {
	let object = unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) };
	(!object.is_null())
		.then_some(object)
		.ok_or_else(|| format!("failed to allocate string {text:?}"))
}
