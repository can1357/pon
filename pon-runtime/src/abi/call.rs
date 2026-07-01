//! Call, function, closure-cell, and argument-binding helper family.
//!
//! The central Phase-A ABI hub still exports `pon_call` and
//! `pon_make_function`.  This module owns the Phase-B call-family surfaces so the
//! integration pass can wire the helper table without redesigning semantics.

use crate::abi::{CodeInfo, ParamSpec};
use crate::feedback::FeedbackCell;
use crate::object::PyObject;
use crate::types::{cell, function, method};

use super::{alloc_function, catch_object_helper, ensure_runtime_initialized, pon_call, return_null_with_error, with_runtime};

/// Function/code flags carried by [`crate::abi::CodeInfo`].
pub type CodeFlags = u32;

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
    unsafe { super::record_feedback_unary(feedback, callee) };
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        if star.is_null() && dstar.is_null() && kw_count == 0 && function::function_record(callee).is_none() {
            // SAFETY: Delegates to the established Phase-A helper for the hot path.
            return unsafe { pon_call(callee, argv, argc) };
        }
        let positional = match unsafe { object_slice(argv, argc) } {
            Ok(values) => values,
            Err(message) => return return_null_with_error(message),
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
        match unsafe { function::call_bound_function(callee, positional, keywords, star, dstar) } {
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
    unsafe { super::record_feedback_unary(feedback, recv_pair) };
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        let (function, receiver) = match unsafe { method::split_bound_method(recv_pair.cast::<method::PyMethod>()) } {
            Ok(pair) => pair,
            Err(message) => return return_null_with_error(message),
        };
        let args = match unsafe { object_slice(argv, argc) } {
            Ok(values) => values,
            Err(message) => return return_null_with_error(message),
        };
        let mut positional = Vec::with_capacity(argc.saturating_add(1));
        positional.push(receiver);
        positional.extend_from_slice(args);
        let keywords = function::KeywordArgs {
            names: &[],
            values: &[],
        };
        match unsafe { function::call_bound_function(function, &positional, keywords, None, None) } {
            Ok(result) => result,
            Err(message) => return_null_with_error(message),
        }
    })
}

/// Creates a boxed function object from full Phase-B `CodeInfo` plus evaluated
/// defaults.  Keyword-only defaults are assigned to the trailing keyword-only
/// parameters in the copied `ParamSpec`; the temporary helper ABI has no separate
/// name array yet.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_make_function_full(
    code: *const CodeInfo,
    defaults: *mut *mut PyObject,
    default_count: usize,
    kwdefaults: *mut *mut PyObject,
    kwdefault_count: usize,
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
        let params = match unsafe { copy_param_names(code.params) } {
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
        let kwdefault_values = match unsafe { object_slice(kwdefaults, kwdefault_count) } {
            Ok(values) => values,
            Err(message) => return return_null_with_error(message),
        };
        let kwdefault_names = derive_kwdefault_names(params.as_ref(), kwdefault_count);
        let object = match with_runtime(|runtime| alloc_function(runtime, code.entry, arity, code.name_interned)) {
            Some(Ok(object)) => object,
            Some(Err(message)) => return return_null_with_error(message),
            None => return return_null_with_error("runtime is not initialized"),
        };
        if let Err(message) = unsafe { super::install_function_feedback(object, code.n_feedback) } {
            return return_null_with_error(message);
        }
        if let Err(message) =
            function::register_function_record(object, code, defaults, &kwdefault_names, kwdefault_values, &[])
        {
            return return_null_with_error(message);
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
    catch_object_helper(|| {
        if value.is_null() {
            return return_null_with_error("cannot create closure cell from NULL");
        }
        cell::new_cell(value).cast::<PyObject>()
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_cell_get(cell_object: *mut PyObject) -> *mut PyObject {
    catch_object_helper(|| match unsafe { cell::cell_get(cell_object.cast::<cell::PyCell>()) } {
        Ok(value) => value,
        Err(message) => return_null_with_error(message),
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_cell_set(cell_object: *mut PyObject, value: *mut PyObject) -> *mut PyObject {
    catch_object_helper(|| match unsafe { cell::cell_set(cell_object.cast::<cell::PyCell>(), value) } {
        Ok(()) => value,
        Err(message) => return_null_with_error(message),
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_cell_delete(cell_object: *mut PyObject) -> *mut PyObject {
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
        record
            .closure()
            .get(index)
            .copied()
            .unwrap_or_else(|| return_null_with_error(format!("closure cell index {index} out of range")))
    })
}

/// Load-method scaffold used by call-family tests and the later attr-family
/// wiring pass.  Descriptor lookup is intentionally not performed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_load_method_pair(function: *mut PyObject, receiver: *mut PyObject) -> *mut PyObject {
    catch_object_helper(|| match method::new_bound_method(function, receiver) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => return_null_with_error(message),
    })
}

unsafe fn object_slice<'a>(values: *mut *mut PyObject, len: usize) -> Result<&'a [*mut PyObject], String> {
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
struct ParamNameCopy {
    names: Vec<u32>,
    positional_only_count: u32,
    positional_count: u32,
    keyword_only_count: u32,
}

unsafe fn copy_param_names(params: *const ParamSpec) -> Result<Option<ParamNameCopy>, String> {
    if params.is_null() {
        return Ok(None);
    }
    // SAFETY: The caller supplies a valid `ParamSpec` for the duration of this copy.
    let params = unsafe { *params };
    let names = if params.total_param_count == 0 {
        Vec::new()
    } else if params.names.is_null() {
        return Err("ParamSpec names pointer is null".to_owned());
    } else {
        // SAFETY: `names` points to `total_param_count` ids by ABI contract.
        unsafe { core::slice::from_raw_parts(params.names, params.total_param_count as usize) }.to_vec()
    };
    Ok(Some(ParamNameCopy {
        names,
        positional_only_count: params.positional_only_count,
        positional_count: params.positional_count,
        keyword_only_count: params.keyword_only_count,
    }))
}

fn derive_kwdefault_names(params: Option<&ParamNameCopy>, kwdefault_count: usize) -> Vec<u32> {
    let Some(params) = params else {
        return Vec::new();
    };
    let positional = (params.positional_only_count + params.positional_count) as usize;
    let kw_start = positional;
    let kw_end = kw_start + params.keyword_only_count as usize;
    params.names.get(kw_start..kw_end).unwrap_or(&[])
        .iter()
        .rev()
        .take(kwdefault_count)
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ptr;

    #[test]
    fn derives_trailing_keyword_default_names() {
        let params = ParamNameCopy {
            names: vec![1, 2, 3, 4],
            positional_only_count: 0,
            positional_count: 2,
            keyword_only_count: 2,
        };

        assert_eq!(derive_kwdefault_names(Some(&params), 1), vec![4]);
        assert_eq!(derive_kwdefault_names(Some(&params), 2), vec![3, 4]);
        assert!(derive_kwdefault_names(None, 1).is_empty());
    }

    #[test]
    fn object_slice_rejects_null_non_empty_array() {
        let err = unsafe { object_slice(ptr::null_mut(), 1) }.unwrap_err();
        assert!(err.contains("null"));
    }
}
