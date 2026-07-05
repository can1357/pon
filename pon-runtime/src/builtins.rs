//! Phase-A builtins exposed through the normal `PyFunction` ABI.

use crate::{native::builtins_mod, object::PyObject};

/// Interned global name for the Phase-A `print` builtin.
#[must_use]
pub fn print_name_interned() -> u32 {
	crate::intern::intern("print")
}

/// Trampoline used by the builtin `print` function object.
///
/// The Phase-B native implementation keeps the Phase-A one-argument behavior
/// while accepting the common variadic positional `print(a, b, ...)` form.
pub unsafe extern "C" fn print_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: `builtin_print` follows the same argv/argc ABI as compiled
	// `PyFunction` entrypoints and uses NULL-sentinel errors.
	unsafe { builtins_mod::builtin_print(argv, argc) }
}

#[must_use]
pub fn variadic_arity() -> usize {
	builtins_mod::VARIADIC_ARITY
}

pub fn for_each_builtin(f: impl FnMut(&'static str, usize, *const u8)) {
	builtins_mod::for_each_builtin(f);
}
