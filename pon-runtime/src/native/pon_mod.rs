//! Native `pon` module: compiler views of live Python functions for debugging.
//!
//! ```python
//! import pon
//! print(pon.ir(f))     # lowered pon IR (tier-0 input)
//! print(pon.clif(f))   # baseline Cranelift IR
//! print(pon.asm(f))    # baseline native code (Cranelift disassembly)
//! print(pon.tier(f))   # current tier-up state name
//! print(pon.state(f))  # runtime counters and function metadata
//! ```
//!
//! Rendering happens in the JIT frontend through the [`crate::inspect`] hook;
//! embeddings without a JIT (ahead-of-time products) raise `RuntimeError`.

use std::{ptr, sync::atomic::Ordering};

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{
	inspect::{InspectError, InspectView, inspect},
	intern::{intern, resolve},
	object::{
		PyFunction, PyObject, TIER_STATE_DEFERRED, TIER_STATE_DISABLED, TIER_STATE_QUEUED,
		TIER_STATE_TIER0, TIER_STATE_TIER1,
	},
	types::exc::ExceptionKind,
};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	// SAFETY: Runtime allocation helper over a static byte literal.
	let module_name = unsafe { crate::abi::pon_const_str(b"pon".as_ptr(), 3) };
	let mut attrs = vec![(intern("__name__"), module_name)];
	for (name, entry) in [
		("ir", debug_ir as BuiltinFn),
		("clif", debug_clif),
		("asm", debug_asm),
		("tier", debug_tier),
		("state", debug_state),
	] {
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

unsafe extern "C" fn debug_tier(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Forwarding the builtin's own argv/argc contract.
	unsafe { render_tier(argv, argc) }
}

unsafe extern "C" fn debug_state(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	// SAFETY: Forwarding the builtin's own argv/argc contract.
	unsafe { render_state(argv, argc) }
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
	let target = match unsafe { expect_function_arg(argv, argc, name) } {
		Ok(target) => target,
		Err(raised) => return raised,
	};
	// SAFETY: `expect_function_arg` proved `target` is a live `PyFunction`.
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

/// Validate the shared `pon.*(function)` argument shape and unwrap bound methods.
///
/// # Safety
/// `argv` must point to `argc` live argument slots.
unsafe fn expect_function_arg(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
) -> Result<*mut PyObject, *mut PyObject> {
	if argc != 1 || argv.is_null() {
		return Err(crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			&format!("pon.{name} expected exactly one argument (a Python function)"),
		));
	}
	// Tagged immediates must be boxed before any `ob_type` dereference; NULL
	// here means the boxing allocation failed with the error already set.
	// SAFETY: `argv` holds at least one slot per the check above.
	let mut target = crate::tag::untag_arg(unsafe { *argv });
	if target.is_null() {
		return Err(ptr::null_mut());
	}
	// Bound methods expose their underlying function state.
	if let Some((function, _receiver)) = crate::types::method::bound_method_parts(target) {
		target = function;
	}
	if !crate::types::function::is_function_object(target) {
		// SAFETY: `target` is boxed (untagged above) and non-NULL.
		let got = unsafe { crate::types::dict::type_name(target) }.unwrap_or("object");
		return Err(crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			&format!("pon.{name} expected a Python function, got {got}"),
		));
	}
	Ok(target)
}

unsafe fn render_tier(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let target = match unsafe { expect_function_arg(argv, argc, "tier") } {
		Ok(target) => target,
		Err(raised) => return raised,
	};
	// SAFETY: `expect_function_arg` proved `target` is a live `PyFunction`.
	let state = unsafe { (*target.cast::<PyFunction>()).tier_state.load(Ordering::Acquire) };
	py_str(tier_state_name(state))
}

unsafe fn render_state(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let target = match unsafe { expect_function_arg(argv, argc, "state") } {
		Ok(target) => target,
		Err(raised) => return raised,
	};
	// SAFETY: `expect_function_arg` proved `target` is a live `PyFunction`.
	let function = unsafe { &*target.cast::<PyFunction>() };
	let tier_state = function.tier_state.load(Ordering::Acquire);
	let osr_entry = function.osr_entry.load(Ordering::Acquire);
	let record = crate::types::function::function_record(target);
	let params = record.as_ref().and_then(|record| record.params.as_ref());

	let mut fields = vec![
		("name", py_str(&function_name(function))),
		("arity", py_int_usize(function.arity)),
		("tier", py_str(tier_state_name(tier_state))),
		("tier_state", py_int_u8(tier_state)),
		("hotness", py_int_u32(function.hotness.load(Ordering::Acquire))),
		(
			"loop_hotness",
			py_int_u32(function.loop_hotness.load(Ordering::Acquire)),
		),
		("deopt_count", py_int_u32(function.deopt_count.load(Ordering::Acquire))),
		("tier_epoch", py_int_u8(function.tier_epoch.load(Ordering::Acquire))),
		("osr_installed", py_bool(!osr_entry.is_null())),
		(
			"osr_loop_header",
			py_int_u32(function.osr_loop_header.load(Ordering::Acquire)),
		),
		("has_metadata", py_bool(record.is_some())),
		(
			"n_locals",
			py_optional_u32(record.as_ref().map(|record| record.n_locals)),
		),
		("flags", py_optional_u32(record.as_ref().map(|record| record.flags))),
		(
			"positional_arity",
			py_optional_usize(record.as_ref().map(|record| record.positional_arity())),
		),
		(
			"positional_only",
			py_optional_usize(params.map(|params| params.positional_only_count)),
		),
		(
			"positional_or_keyword",
			py_optional_usize(params.map(|params| params.positional_count)),
		),
		(
			"keyword_only",
			py_optional_usize(params.map(|params| params.keyword_only_count)),
		),
		(
			"has_varargs",
			py_optional_bool(params.map(|params| params.varargs_name.is_some())),
		),
		(
			"has_varkw",
			py_optional_bool(params.map(|params| params.varkw_name.is_some())),
		),
		(
			"default_count",
			py_optional_usize(record.as_ref().map(|record| record.default_count())),
		),
		(
			"kwdefault_count",
			py_optional_usize(record.as_ref().map(|record| record.kwdefault_count())),
		),
		(
			"closure_count",
			py_optional_usize(record.as_ref().map(|record| record.closure_count())),
		),
	];
	build_dict(&mut fields)
}

fn function_name(function: &PyFunction) -> String {
	resolve(function.name_interned)
		.unwrap_or_else(|| format!("<interned:{}>", function.name_interned))
}

fn tier_state_name(state: u8) -> &'static str {
	match state {
		TIER_STATE_TIER0 => "tier0",
		TIER_STATE_QUEUED => "queued",
		TIER_STATE_TIER1 => "tier1",
		TIER_STATE_DEFERRED => "deferred",
		TIER_STATE_DISABLED => "disabled",
		_ => "unknown",
	}
}

fn py_str(text: &str) -> *mut PyObject {
	// SAFETY: Runtime allocation helper copying `text`'s UTF-8 bytes.
	unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn py_int_usize(value: usize) -> *mut PyObject {
	unsafe { crate::abi::pon_const_int(i64::try_from(value).unwrap_or(i64::MAX)) }
}

fn py_int_u32(value: u32) -> *mut PyObject {
	unsafe { crate::abi::pon_const_int(i64::from(value)) }
}

fn py_int_u8(value: u8) -> *mut PyObject {
	unsafe { crate::abi::pon_const_int(i64::from(value)) }
}

fn py_bool(value: bool) -> *mut PyObject {
	unsafe { crate::abi::number::pon_const_bool(i32::from(value)) }
}

fn py_none() -> *mut PyObject {
	unsafe { crate::abi::pon_none() }
}

fn py_optional_usize(value: Option<usize>) -> *mut PyObject {
	value.map_or_else(py_none, py_int_usize)
}

fn py_optional_u32(value: Option<u32>) -> *mut PyObject {
	value.map_or_else(py_none, py_int_u32)
}

fn py_optional_bool(value: Option<bool>) -> *mut PyObject {
	value.map_or_else(py_none, py_bool)
}

fn build_dict(fields: &mut [(&'static str, *mut PyObject)]) -> *mut PyObject {
	let mut items = Vec::with_capacity(fields.len() * 2);
	for (name, value) in fields.iter().copied() {
		if value.is_null() {
			return ptr::null_mut();
		}
		let key = py_str(name);
		if key.is_null() {
			return ptr::null_mut();
		}
		items.push(key);
		items.push(value);
	}
	unsafe { crate::abi::map::pon_build_map(items.as_mut_ptr(), items.len() / 2) }
}
