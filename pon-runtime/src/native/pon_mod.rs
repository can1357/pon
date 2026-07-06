//! Native `pon` module: compiler views of live Python functions for debugging.
//!
//! ```python
//! import pon
//! print(pon.ir(f))    # lowered pon IR (tier-0 input)
//! print(pon.clif(f))  # baseline Cranelift IR
//! print(pon.asm(f))   # baseline native code (Cranelift disassembly)
//! ```
//!
//! Rendering happens in the JIT frontend through the [`crate::inspect`] hook;
//! embeddings without a JIT (ahead-of-time products) raise `RuntimeError`.

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{
	inspect::{InspectError, InspectView, inspect},
	intern::intern,
	object::{PyFunction, PyObject},
	types::exc::ExceptionKind,
};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	// SAFETY: Runtime allocation helper over a static byte literal.
	let module_name = unsafe { crate::abi::pon_const_str(b"pon".as_ptr(), 3) };
	let mut attrs = vec![(intern("__name__"), module_name)];
	for (name, entry) in [("ir", debug_ir as BuiltinFn), ("clif", debug_clif), ("asm", debug_asm)] {
		// SAFETY: `entry` is a live builtin with the variadic `(argv, argc)` ABI.
		let function =
			unsafe { crate::abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
		if function.is_null() {
			return Err(format!("failed to allocate pon.{name}"));
		}
		attrs.push((intern(name), function));
	}
	install_module("pon", attrs)
}

unsafe extern "C" fn debug_ir(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Forwarding the builtin's own argv/argc contract.
	unsafe { render(argv, argc, InspectView::Ir, "ir") }
}

unsafe extern "C" fn debug_clif(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Forwarding the builtin's own argv/argc contract.
	unsafe { render(argv, argc, InspectView::Clif, "clif") }
}

unsafe extern "C" fn debug_asm(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Forwarding the builtin's own argv/argc contract.
	unsafe { render(argv, argc, InspectView::Asm, "asm") }
}

/// Shared body of `pon.ir`/`pon.clif`/`pon.asm`: unwrap the function object,
/// then delegate rendering to the JIT-installed inspection hook.
///
/// # Safety
/// `argv` must point to `argc` live argument slots.
unsafe fn render(
	argv: *mut *mut PyObject,
	argc: usize,
	view: InspectView,
	name: &str,
) -> *mut PyObject {
	if argc != 1 || argv.is_null() {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			&format!("pon.{name} expected exactly one argument (a Python function)"),
		);
	}
	// Tagged immediates must be boxed before any `ob_type` dereference; NULL
	// here means the boxing allocation failed with the error already set.
	// SAFETY: `argv` holds at least one slot per the check above.
	let mut target = crate::tag::untag_arg(unsafe { *argv });
	if target.is_null() {
		return std::ptr::null_mut();
	}
	// Bound methods inspect their underlying function.
	if let Some((function, _receiver)) = crate::types::method::bound_method_parts(target) {
		target = function;
	}
	if !crate::types::function::is_function_object(target) {
		// SAFETY: `target` is boxed (untagged above) and non-NULL.
		let got = unsafe { crate::types::dict::type_name(target) }.unwrap_or("object");
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			&format!("pon.{name} expected a Python function, got {got}"),
		);
	}
	// SAFETY: `is_function_object` proved `target` is a live `PyFunction`.
	let entry = unsafe { (*target.cast::<PyFunction>()).code };
	let Some(result) = inspect(entry, view) else {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::RuntimeError,
			"function inspection is not available in this embedding (no JIT frontend)",
		);
	};
	match result {
		// SAFETY: Runtime allocation helper copying `text`'s UTF-8 bytes.
		Ok(text) => unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) },
		Err(InspectError::UnknownFunction) => crate::abi::exc::raise_kind_error_text(
			ExceptionKind::ValueError,
			&format!("pon.{name}: function was not compiled by the pon JIT in this process"),
		),
		Err(InspectError::Render(message)) => {
			crate::abi::exc::raise_kind_error_text(ExceptionKind::RuntimeError, &message)
		},
	}
}
