//! Call, function, closure-cell, and argument-binding helper family.
//!
//! The central Phase-A ABI hub still exports `pon_call` and
//! `pon_make_function`.  This module owns the Phase-B call-family surfaces so
//! the integration pass can wire the helper table without redesigning
//! semantics.

use super::{
	alloc_function, catch_object_helper, ensure_runtime_initialized, pon_call,
	return_null_with_error, with_runtime,
};
use crate::{
	abi::{CodeInfo, ParamSpec},
	feedback::{CallIC, FeedbackCell},
	object::PyObject,
	types::{cell, function, method},
};

/// Function/code flags carried by [`crate::abi::CodeInfo`].
pub type CodeFlags = u32;
/// Function body contains generator suspension points and must return a
/// generator object on call.
pub const CODE_FLAG_GENERATOR: CodeFlags = 1 << 0;
/// Function body was produced by `async def` and must return a coroutine object
/// on call.
pub const CODE_FLAG_COROUTINE: CodeFlags = 1 << 1;
/// Function body was produced by `async def` with `yield` and must return an
/// async-generator object on call (PEP 525).
pub const CODE_FLAG_ASYNC_GENERATOR: CodeFlags = 1 << 2;

/// CPython `PyCallable_Check`: `type(o)->tp_call != NULL`.  pon's function/
/// method/type objects dispatch through dedicated [`pon_call`] fast paths
/// without `tp_call` slots, so they are named explicitly.  Type objects with
/// a custom metaclass (ABCs, `class Meta(type)`) report the metaclass name
/// instead of `"type"` yet inherit `type.tp_call`, so any metaclass instance
/// is callable.  Everything else is callable when its type carries `tp_call`
/// or its MRO defines `__call__` (heap instances go through the DunderCall
/// path).  Backs `builtins.callable` and the argument checks in `os` and
/// `collections`.
pub(crate) fn is_callable_object(object: *mut PyObject) -> bool {
	// Tagged small ints are ints: never callable, and must not be
	// dereferenced as heap pointers.
	if object.is_null() || crate::tag::is_small_int(object) {
		return false;
	}
	if matches!(
		unsafe { crate::types::dict::type_name(object) },
		Some("function" | "method" | "type")
	) {
		return true;
	}
	// Same criterion as `pon_call`'s type-callee dispatch
	// (`is_runtime_type_object`): the metatype is a `type` subtype, so the
	// object inherits `type.tp_call` regardless of its metaclass name.
	let is_type =
		super::with_runtime(|runtime| unsafe { super::is_runtime_type_object(runtime, object) });
	if is_type == Some(true) {
		return true;
	}
	let ty = unsafe { (*object).ob_type.cast_mut() };
	!ty.is_null()
		&& (unsafe { (*ty).tp_call.is_some() }
			|| !unsafe {
				crate::descr::lookup_in_type(ty, crate::intern::intern(crate::intern::DUNDER_CALL))
			}
			.is_null())
}

/// Calls a boxed callable with positional, keyword, `*args`, and `**kwargs`
/// operands.  Unsupported expansion forms report a NULL-sentinel error rather
/// than unwinding.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_call_ex(
	callee: *mut PyObject,
	argv: *mut *mut PyObject,
	argc: usize,
	star: *mut PyObject,
	kw_names: *const u32,
	kw_values: *mut *mut PyObject,
	kw_count: usize,
	dstar: *mut PyObject,
	feedback: *mut FeedbackCell,
) -> *mut PyObject {
	crate::untag_prelude!(callee, star, dstar);
	// J0.3 CallIC: record the observed callee identity (first target wins;
	// a second distinct target latches the cell megamorphic).  Tier-0 never
	// consults this — it exists to feed O3's tier-1 call specialization.
	if let Some(cell) = unsafe { feedback.as_ref() } {
		if !callee.is_null() {
			cell.record_call(CallIC { callee_identity: callee as usize });
		}
	}
	catch_object_helper(|| {
		if let Err(message) = ensure_runtime_initialized() {
			return return_null_with_error(message);
		}
		if star.is_null()
			&& dstar.is_null()
			&& kw_count == 0
			&& function::function_record(callee).is_none()
		{
			// SAFETY: Delegates to the established Phase-A helper for the hot path.
			return unsafe { pon_call(callee, argv, argc) };
		}
		// A bound-method callee (e.g. a method resolved through a super()
		// proxy, then called with */** arguments) is a PyMethod pair, not a
		// PyFunction: pierce it and prepend the receiver so the underlying
		// function binds `self`.  Mirrors `pon_call`'s Method target, which
		// covers the no-keyword path above.
		let (callee, bound_receiver) = match method::bound_method_parts(callee) {
			Some((function, receiver)) => (function, Some(receiver)),
			None => (callee, None),
		};
		let explicit = match unsafe { object_slice(argv, argc) } {
			Ok(values) => values,
			Err(message) => return return_null_with_error(message),
		};
		let bound_positional: Vec<*mut PyObject>;
		let positional: &[*mut PyObject] = match bound_receiver {
			Some(receiver) => {
				let mut values = Vec::with_capacity(explicit.len() + 1);
				values.push(receiver);
				values.extend_from_slice(explicit);
				bound_positional = values;
				&bound_positional
			},
			None => explicit,
		};
		let names = match unsafe { name_slice(kw_names, kw_count) } {
			Ok(values) => values,
			Err(message) => return return_null_with_error(message),
		};
		let values = match unsafe { object_slice(kw_values, kw_count) } {
			Ok(values) => values,
			Err(message) => return return_null_with_error(message),
		};
		let star = (!star.is_null()).then_some(star);
		let dstar = (!dstar.is_null()).then_some(dstar);
		let keywords = function::KeywordArgs { names, values };
		if function::function_record(callee).is_some() {
			return unsafe { super::call_phase_b_function(callee, positional, keywords, star, dstar) };
		}
		// Non-function callee (type constructor, native callable) reached
		// with `*`/`**` operands: materialize the expansion once, then reuse
		// the generic dispatch paths (`str(*args)` in enum's member creation,
		// `dict(**kw)`, ...).
		let mut flat_positional: Vec<*mut PyObject> = positional.to_vec();
		if let Some(star) = star {
			match unsafe { function::positional_args_from_star(star) } {
				Ok(extra) => flat_positional.extend_from_slice(&extra),
				Err(message) => return return_null_with_error(message),
			}
		}
		let mut flat_names = names.to_vec();
		let mut flat_values = values.to_vec();
		if let Some(dstar) = dstar {
			if let Err(message) = unsafe {
				function::extend_keywords_from_mapping(callee, dstar, &mut flat_names, &mut flat_values)
			} {
				return return_null_with_error(message);
			}
		}
		if flat_names.is_empty() {
			let argv_ptr = if flat_positional.is_empty() {
				core::ptr::null_mut()
			} else {
				flat_positional.as_mut_ptr()
			};
			return unsafe { pon_call(callee, argv_ptr, flat_positional.len()) };
		}
		let keywords = function::KeywordArgs { names: &flat_names, values: &flat_values };
		if let Some(result) =
			unsafe { super::call_builtin_type_with_keywords(callee, &flat_positional, keywords) }
		{
			return result;
		}
		// A class callee (metaclass instances included) with keyword
		// arguments: `A(x=3)` binds the keywords into `__new__`/`__init__`.
		if unsafe { crate::types::type_::is_type_object(callee) } {
			return unsafe { super::call_type_with_keywords(callee, &flat_positional, keywords) };
		}
		// Instance of a class with no `tp_call`, reached with keyword
		// arguments (`Mock(return_value=…)`, `m.foo(1, key="v")` — mock's
		// `CallableMixin.__call__(self, /, *args, **kwargs)`): dispatch
		// through the type's `__call__` descriptor, mirroring `pon_call`'s
		// DunderCall target on the no-keyword path.  The bound result
		// re-enters this helper once: the method pair is pierced at entry
		// and the underlying Python function binds the keywords.
		if !callee.is_null() {
			let ty = unsafe { (*callee).ob_type.cast_mut() };
			if !ty.is_null() && unsafe { (*ty).tp_call.is_none() } {
				let dunder = unsafe {
					crate::descr::lookup_in_type(ty, crate::intern::intern(crate::intern::DUNDER_CALL))
				};
				if !dunder.is_null() {
					let bound = unsafe { crate::descr::descriptor_get(dunder, callee, ty) };
					if bound.is_null() {
						return core::ptr::null_mut();
					}
					return unsafe {
						pon_call_ex(
							bound,
							flat_positional.as_mut_ptr(),
							flat_positional.len(),
							core::ptr::null_mut(),
							flat_names.as_ptr(),
							flat_values.as_mut_ptr(),
							flat_names.len(),
							core::ptr::null_mut(),
							core::ptr::null_mut(),
						)
					};
				}
			}
		}
		match unsafe { function::call_bound_function(callee, &flat_positional, keywords, None, None) }
		{
			Ok(result) => result,
			Err(message) => return_null_with_error(message),
		}
	})
}

/// Calls a method pair produced by `LoadMethod`, inserting the receiver before
/// explicit positional arguments.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_call_method(
	recv_pair: *mut PyObject,
	argv: *mut *mut PyObject,
	argc: usize,
	feedback: *mut FeedbackCell,
) -> *mut PyObject {
	crate::untag_prelude!(recv_pair);
	catch_object_helper(|| {
		if let Err(message) = ensure_runtime_initialized() {
			return return_null_with_error(message);
		}
		let (function, receiver) =
			match unsafe { method::split_bound_method(recv_pair.cast::<method::PyMethod>()) } {
				Ok(pair) => pair,
				Err(message) => return return_null_with_error(message),
			};
		// J0.3 CallIC: record the UNDERLYING function, not the bound-method
		// pair — the pair is a fresh allocation per LoadMethod, so its
		// address would immediately latch the cell megamorphic.
		if let Some(cell) = unsafe { feedback.as_ref() } {
			if !function.is_null() {
				cell.record_call(CallIC { callee_identity: function as usize });
			}
		}
		let args = match unsafe { object_slice(argv, argc) } {
			Ok(values) => values,
			Err(message) => return return_null_with_error(message),
		};
		let mut positional = Vec::with_capacity(argc.saturating_add(1));
		positional.push(receiver);
		positional.extend_from_slice(args);
		let keywords = function::KeywordArgs { names: &[], values: &[] };
		match unsafe { function::call_bound_function(function, &positional, keywords, None, None) } {
			Ok(result) => result,
			Err(message) => return_null_with_error(message),
		}
	})
}

/// defaults.  Keyword-only defaults arrive with the matching interned parameter
/// names because defaults may be sparse across keyword-only declarations.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_make_function_full(
	code: *const CodeInfo,
	defaults: *mut *mut PyObject,
	default_count: usize,
	kwdefault_names: *const u32,
	kwdefaults: *mut *mut PyObject,
	kwdefault_count: usize,
	annotation_names: *const u32,
	annotations: *mut *mut PyObject,
	annotation_count: usize,
) -> *mut PyObject {
	catch_object_helper(|| {
		if let Err(message) = ensure_runtime_initialized() {
			return return_null_with_error(message);
		}
		if code.is_null() {
			return return_null_with_error("CodeInfo pointer is null");
		}
		// SAFETY: The caller supplies a valid `CodeInfo` for this helper call.
		let code = unsafe { &*code };
		let params = match unsafe { copy_param_counts(code.params) } {
			Ok(params) => params,
			Err(message) => return return_null_with_error(message),
		};
		let arity = params
			.as_ref()
			.map_or(0, |params| (params.positional_only_count + params.positional_count) as usize);
		let defaults = match unsafe { object_slice(defaults, default_count) } {
			Ok(values) => values,
			Err(message) => return return_null_with_error(message),
		};
		let kwdefault_names = match unsafe { name_slice(kwdefault_names, kwdefault_count) } {
			Ok(values) => values,
			Err(message) => return return_null_with_error(message),
		};
		let kwdefault_values = match unsafe { object_slice(kwdefaults, kwdefault_count) } {
			Ok(values) => values,
			Err(message) => return return_null_with_error(message),
		};
		let annotation_names = match unsafe { name_slice(annotation_names, annotation_count) } {
			Ok(values) => values,
			Err(message) => return return_null_with_error(message),
		};
		let annotation_values = match unsafe { object_slice(annotations, annotation_count) } {
			Ok(values) => values,
			Err(message) => return return_null_with_error(message),
		};
		let object = match with_runtime(|runtime| {
			alloc_function(runtime, code.entry, arity, code.name_interned)
		}) {
			Some(Ok(object)) => object,
			Some(Err(message)) => return return_null_with_error(message),
			None => return return_null_with_error("runtime is not initialized"),
		};
		super::record_new_function_module(object);
		if let Err(message) = unsafe { super::install_function_feedback(object, code.n_feedback) } {
			return return_null_with_error(message);
		}
		if let Err(message) = function::register_function_record(
			object,
			code,
			defaults,
			kwdefault_names,
			kwdefault_values,
			&[],
		) {
			return return_null_with_error(message);
		}
		// PEP 649: post-cutover IR always passes zero annotations; keep the
		// eager install only for the legacy non-empty shape so the lazy
		// `__annotations__` cache stays NULL until first access.
		if annotation_count > 0 {
			if let Err(message) = unsafe {
				function::set_function_annotations(object, annotation_names, annotation_values)
			} {
				return return_null_with_error(message);
			}
		}
		object
	})
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_function_set_closure(
	function: *mut PyObject,
	closure: *mut *mut PyObject,
	closure_count: usize,
) -> *mut PyObject {
	crate::untag_prelude!(function);
	catch_object_helper(|| {
		let cells = match unsafe { object_slice(closure, closure_count) } {
			Ok(values) => values,
			Err(message) => return return_null_with_error(message),
		};
		match function::set_function_closure(function, cells) {
			Ok(()) => function,
			Err(message) => return_null_with_error(message),
		}
	})
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_make_cell(value: *mut PyObject) -> *mut PyObject {
	crate::untag_prelude!(value);
	catch_object_helper(|| {
		if value.is_null() {
			return return_null_with_error("cannot create closure cell from NULL");
		}
		cell::new_cell(value).cast::<PyObject>()
	})
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_cell_get(cell_object: *mut PyObject) -> *mut PyObject {
	crate::untag_prelude!(cell_object);
	catch_object_helper(|| match unsafe { cell::cell_get(cell_object.cast::<cell::PyCell>()) } {
		Ok(value) => value,
		Err(message) if message.contains("before assignment") => {
			super::exc::raise_name_error_text(&message)
		},
		Err(message) => return_null_with_error(message),
	})
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_cell_set(
	cell_object: *mut PyObject,
	value: *mut PyObject,
) -> *mut PyObject {
	crate::untag_prelude!(cell_object, value);
	catch_object_helper(|| {
		match unsafe { cell::cell_set(cell_object.cast::<cell::PyCell>(), value) } {
			Ok(()) => value,
			Err(message) => return_null_with_error(message),
		}
	})
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_cell_delete(cell_object: *mut PyObject) -> *mut PyObject {
	crate::untag_prelude!(cell_object);
	catch_object_helper(|| match unsafe { cell::cell_delete(cell_object.cast::<cell::PyCell>()) } {
		Ok(()) => unsafe { super::pon_none() },
		Err(message) => return_null_with_error(message),
	})
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_current_closure_cell(index: usize) -> *mut PyObject {
	catch_object_helper(|| {
		let function = super::current_function_object();
		if function.is_null() {
			return return_null_with_error("no current function closure");
		}
		let Some(record) = function::function_record(function) else {
			return return_null_with_error("current function has no closure metadata");
		};
		record.closure().get(index).copied().unwrap_or_else(|| {
			return_null_with_error(format!("closure cell index {index} out of range"))
		})
	})
}

/// Load-method scaffold used by call-family tests and the later attr-family
/// wiring pass.  Descriptor lookup is intentionally not performed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_load_method_pair(
	function: *mut PyObject,
	receiver: *mut PyObject,
) -> *mut PyObject {
	crate::untag_prelude!(function, receiver);
	catch_object_helper(|| match method::new_bound_method(function, receiver) {
		Ok(method) => method.cast::<PyObject>(),
		Err(message) => return_null_with_error(message),
	})
}

unsafe fn object_slice<'a>(
	values: *mut *mut PyObject,
	len: usize,
) -> Result<&'a [*mut PyObject], String> {
	if values.is_null() && len != 0 {
		return Err("PyObject pointer array is null".to_owned());
	}
	if len == 0 {
		Ok(&[])
	} else {
		// SAFETY: The caller supplies `len` contiguous object-pointer entries.
		Ok(unsafe { core::slice::from_raw_parts(values.cast_const(), len) })
	}
}

unsafe fn name_slice<'a>(values: *const u32, len: usize) -> Result<&'a [u32], String> {
	if values.is_null() && len != 0 {
		return Err("keyword name array is null".to_owned());
	}
	if len == 0 {
		Ok(&[])
	} else {
		// SAFETY: The caller supplies `len` contiguous interned-name entries.
		Ok(unsafe { core::slice::from_raw_parts(values, len) })
	}
}

#[derive(Clone, Debug)]
struct ParamCountCopy {
	positional_only_count: u32,
	positional_count:      u32,
}

unsafe fn copy_param_counts(params: *const ParamSpec) -> Result<Option<ParamCountCopy>, String> {
	if params.is_null() {
		return Ok(None);
	}
	// SAFETY: The caller supplies a valid `ParamSpec` for the duration of this
	// copy.
	let params = unsafe { *params };
	if params.names.is_null()
		&& params
			.positional_only_count
			.saturating_add(params.positional_count)
			.saturating_add(params.keyword_only_count)
			!= 0
	{
		return Err("ParamSpec names pointer is null".to_owned());
	}
	Ok(Some(ParamCountCopy {
		positional_only_count: params.positional_only_count,
		positional_count:      params.positional_count,
	}))
}

#[cfg(test)]
mod tests {
	use core::ptr;

	use super::*;

	#[test]
	fn name_slice_rejects_null_non_empty_array() {
		let err = unsafe { name_slice(ptr::null(), 1) }.unwrap_err();
		assert!(err.contains("null"));
	}

	#[test]
	fn object_slice_rejects_null_non_empty_array() {
		let err = unsafe { object_slice(ptr::null_mut(), 1) }.unwrap_err();
		assert!(err.contains("null"));
	}
}
