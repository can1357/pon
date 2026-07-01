//! Function object metadata and Phase-B argument binding.
//!
//! The boxed `PyFunction` layout still lives in `crate::object` for the Phase-A
//! ABI.  Phase-B call helpers need richer metadata (defaults, keyword-only
//! defaults, closure cells, and `ParamSpec` copies) before the central object
//! layout is widened, so this module owns a side table keyed by function object
//! address.  The table is deliberately boring: raw object pointers are stored as
//! integer addresses so the global mutex remains `Send`, and every public helper
//! returns a `Result` instead of unwinding across the C ABI.

use std::collections::{BTreeMap, HashMap};
use std::mem;
use std::ptr;
use std::sync::{LazyLock, Mutex};

use crate::abi::{CodeInfo, ParamSpec};
use crate::intern;
use crate::object::{PyCodeFn, PyFunction, PyObject, PyUnicode};
use crate::thread_state::{pon_err_clear, pon_err_occurred};
use crate::types::{dict, list::PyList, tuple::PyTuple};

static FUNCTION_RECORDS: LazyLock<Mutex<HashMap<usize, FunctionRecord>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Owned Phase-B metadata for a boxed function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionRecord {
    /// Compiled entrypoint address using the Phase-A compiled-code ABI.
    pub entry: *const u8,
    /// Interned function name used in diagnostics.
    pub name_interned: u32,
    /// Required frame-local count advertised by lowering.
    pub n_locals: u32,
    /// Forward-compatible function/code flags.
    pub flags: u32,
    /// Parameter descriptor copied out of the lowering-owned `CodeInfo`.
    pub params: Option<OwnedParamSpec>,
    /// Positional defaults for the trailing positional parameters.
    defaults: Vec<usize>,
    /// Keyword-only defaults by interned parameter name.
    kwdefaults: BTreeMap<u32, usize>,
    /// Closure cell objects captured by this function, in free-var order.
    closure: Vec<usize>,
}

// Raw object/code addresses are stored and copied, never dereferenced by the
// metadata table itself.  The call helpers perform all unsafe dereferences under
// their normal runtime checks.
unsafe impl Send for FunctionRecord {}

impl FunctionRecord {
    /// Positional arity enforced for Phase-A-compatible calls.
    #[must_use]
    pub fn positional_arity(&self) -> usize {
        self.params.as_ref().map_or(0, OwnedParamSpec::positional_arity)
    }

    /// Return the stored positional defaults as object pointers.
    #[must_use]
    pub fn defaults(&self) -> Vec<*mut PyObject> {
        self.defaults.iter().map(|value| *value as *mut PyObject).collect()
    }

    /// Return captured closure cells as object pointers.
    #[must_use]
    pub fn closure(&self) -> Vec<*mut PyObject> {
        self.closure.iter().map(|value| *value as *mut PyObject).collect()
    }
}

/// Owned, Rust-friendly copy of the C ABI `ParamSpec`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedParamSpec {
    /// Interned parameter names in source/slot order, excluding `*args`/`**kwargs`.
    pub names: Vec<u32>,
    /// Leading positional-only parameter count.
    pub positional_only_count: usize,
    /// Positional-or-keyword parameter count after positional-only parameters.
    pub positional_count: usize,
    /// Keyword-only parameter count after positional parameters.
    pub keyword_only_count: usize,
    /// Interned `*args` parameter name when present.
    pub varargs_name: Option<u32>,
    /// Interned `**kwargs` parameter name when present.
    pub varkw_name: Option<u32>,
}

impl OwnedParamSpec {
    /// Positional-only plus positional-or-keyword arity.
    #[must_use]
    pub fn positional_arity(&self) -> usize {
        self.positional_only_count + self.positional_count
    }

    fn total_slots(&self) -> usize {
        self.names.len() + usize::from(self.varargs_name.is_some()) + usize::from(self.varkw_name.is_some())
    }

    fn varargs_slot(&self) -> Option<usize> {
        self.varargs_name.map(|_| self.names.len())
    }

    fn varkw_slot(&self) -> Option<usize> {
        self.varkw_name
            .map(|_| self.names.len() + usize::from(self.varargs_name.is_some()))
    }
}

/// Keyword argument arrays passed by `CallEx`.
#[derive(Clone, Copy, Debug)]
pub struct KeywordArgs<'a> {
    /// Interned keyword names.
    pub names: &'a [u32],
    /// Boxed keyword values; length must match `names`.
    pub values: &'a [*mut PyObject],
}

/// Register Phase-B metadata for an already allocated `PyFunction`.
pub fn register_function_record(
    function: *mut PyObject,
    code: &CodeInfo,
    defaults: &[*mut PyObject],
    kwdefault_names: &[u32],
    kwdefault_values: &[*mut PyObject],
    closure: &[*mut PyObject],
) -> Result<(), String> {
    if function.is_null() {
        return Err("cannot register metadata for NULL function".to_owned());
    }
    if code.entry.is_null() {
        return Err("function code pointer is null".to_owned());
    }
    if kwdefault_names.len() != kwdefault_values.len() {
        return Err(format!(
            "kwdefault name/value length mismatch: {} names for {} values",
            kwdefault_names.len(),
            kwdefault_values.len()
        ));
    }

    let params = unsafe { copy_param_spec(code.params)? };
    let mut kwdefaults = BTreeMap::new();
    for (name, value) in kwdefault_names.iter().copied().zip(kwdefault_values.iter().copied()) {
        if value.is_null() {
            return Err(format!("keyword-only default for interned name {name} is NULL"));
        }
        if kwdefaults.insert(name, value as usize).is_some() {
            return Err(format!("duplicate keyword-only default for interned name {name}"));
        }
    }

    let record = FunctionRecord {
        entry: code.entry,
        name_interned: code.name_interned,
        n_locals: code.n_locals,
        flags: code.flags,
        params,
        defaults: defaults.iter().map(|value| *value as usize).collect(),
        kwdefaults,
        closure: closure.iter().map(|value| *value as usize).collect(),
    };
    FUNCTION_RECORDS
        .lock()
        .map_err(|_| "function metadata table is poisoned".to_owned())?
        .insert(function as usize, record);
    Ok(())
}

pub fn set_function_closure(function: *mut PyObject, closure: &[*mut PyObject]) -> Result<(), String> {
    if function.is_null() {
        return Err("cannot set closure for NULL function".to_owned());
    }
    let mut records = FUNCTION_RECORDS
        .lock()
        .map_err(|_| "function metadata table is poisoned".to_owned())?;
    let record = records
        .get_mut(&(function as usize))
        .ok_or_else(|| "function has no Phase-B metadata record".to_owned())?;
    record.closure = closure.iter().map(|value| *value as usize).collect();
    Ok(())
}

/// Remove side-table metadata for a function object.
pub fn unregister_function_record(function: *mut PyObject) {
    if let Ok(mut records) = FUNCTION_RECORDS.lock() {
        records.remove(&(function as usize));
    }
}

/// Return a copy of side-table metadata for `function` when it has Phase-B data.
#[must_use]
pub fn function_record(function: *mut PyObject) -> Option<FunctionRecord> {
    FUNCTION_RECORDS
        .lock()
        .ok()
        .and_then(|records| records.get(&(function as usize)).cloned())
}

/// Descriptor binding for function attributes stored on Python classes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn function_descr_get(descr: *mut PyObject, obj: *mut PyObject, _owner: *mut PyObject) -> *mut PyObject {
    if descr.is_null() || obj.is_null() {
        return descr;
    }
    match crate::types::method::new_bound_method(descr, obj) {
        Ok(method) => method.cast::<PyObject>(),
        Err(_) => descr,
    }
}

unsafe fn positional_args_from_star(object: *mut PyObject) -> Result<Vec<*mut PyObject>, String> {
    match unsafe { dict::type_name(object) } {
        Some("tuple") => Ok(unsafe { (&*object.cast::<PyTuple>()).as_slice() }.to_vec()),
        Some("list") => Ok(unsafe { (&*object.cast::<PyList>()).as_slice() }.to_vec()),
        Some(name) => Err(format!("argument after * must be an iterable, not {name}")),
        None => Err("argument after * is invalid".to_owned()),
    }
}

unsafe fn extend_keywords_from_mapping(
    function: *mut PyObject,
    mapping: *mut PyObject,
    names: &mut Vec<u32>,
    values: &mut Vec<*mut PyObject>,
) -> Result<(), String> {
    if unsafe { dict::type_name(mapping) } != Some("dict") {
        let type_name = unsafe { dict::type_name(mapping) }.unwrap_or("object");
        return Err(format!("argument after ** must be a mapping, not {type_name}"));
    }
    for entry in unsafe { dict::dict_entries_snapshot(mapping)? } {
        if unsafe { dict::type_name(entry.key) } != Some("str") {
            return Err("keywords must be strings".to_owned());
        }
        let Some(name_text) = (unsafe { (&*entry.key.cast::<PyUnicode>()).as_str() }) else {
            return Err("keyword name is not valid UTF-8".to_owned());
        };
        let name = intern::intern(name_text);
        if names.contains(&name) {
            return Err(format!(
                "{} got multiple values for keyword argument '{}'",
                function_call_name(function),
                name_text
            ));
        }
        names.push(name);
        values.push(entry.value);
    }
    Ok(())
}

fn function_call_name(function: *mut PyObject) -> String {
    let name = function_name(function).unwrap_or_else(|| "function".to_owned());
    format!("__main__.{name}()")
}

fn function_name(function: *mut PyObject) -> Option<String> {
    if function.is_null() {
        return None;
    }
    let name = function_record(function)
        .map(|record| record.name_interned)
        .unwrap_or_else(|| unsafe { (*function.cast::<PyFunction>()).name_interned });
    intern::resolve(name)
}

fn keyword_name(name: u32) -> String {
    intern::resolve(name).unwrap_or_else(|| name.to_string())
}

unsafe fn build_tuple_from_slice(values: &[*mut PyObject]) -> Result<*mut PyObject, String> {
    let mut owned = values.to_vec();
    let ptr = if owned.is_empty() {
        ptr::null_mut()
    } else {
        owned.as_mut_ptr()
    };
    let object = unsafe { crate::abi::seq::pon_build_tuple(ptr, owned.len()) };
    if object.is_null() {
        Err("failed to build *args tuple".to_owned())
    } else {
        Ok(object)
    }
}

unsafe fn build_kwargs_dict(pairs: &[(u32, *mut PyObject)]) -> Result<*mut PyObject, String> {
    let mut flat = Vec::with_capacity(pairs.len().saturating_mul(2));
    for (name, value) in pairs {
        let text = keyword_name(*name);
        let key = unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) };
        if key.is_null() {
            return Err("failed to build **kwargs key".to_owned());
        }
        flat.push(key);
        flat.push(*value);
    }
    let ptr = if flat.is_empty() {
        ptr::null_mut()
    } else {
        flat.as_mut_ptr()
    };
    let object = unsafe { crate::abi::map::pon_build_map(ptr, pairs.len()) };
    if object.is_null() {
        Err("failed to build **kwargs dict".to_owned())
    } else {
        Ok(object)
    }
}

/// Bind a call into the compiled function's argv/local-slot order.
pub fn bind_arguments(
    function: *mut PyObject,
    positional: &[*mut PyObject],
    keywords: KeywordArgs<'_>,
    star: Option<*mut PyObject>,
    dstar: Option<*mut PyObject>,
) -> Result<Vec<*mut PyObject>, String> {
    if keywords.names.len() != keywords.values.len() {
        return Err(format!(
            "keyword name/value length mismatch: {} names for {} values",
            keywords.names.len(),
            keywords.values.len()
        ));
    }
    let mut positional_values = positional.to_vec();
    if let Some(star) = star {
        positional_values.extend(unsafe { positional_args_from_star(star) }?);
    }
    let positional = positional_values.as_slice();

    let mut keyword_names = keywords.names.to_vec();
    let mut keyword_values = keywords.values.to_vec();
    if let Some(dstar) = dstar {
        unsafe { extend_keywords_from_mapping(function, dstar, &mut keyword_names, &mut keyword_values) }?;
    }
    let keywords = KeywordArgs {
        names: keyword_names.as_slice(),
        values: keyword_values.as_slice(),
    };

    let Some(record) = function_record(function) else {
        return bind_phase_a_arguments(function, positional, keywords);
    };
    let Some(params) = record.params.as_ref() else {
        return bind_phase_a_arguments(function, positional, keywords);
    };

    let positional_arity = params.positional_arity();
    let mut bound = vec![ptr::null_mut(); params.total_slots()];
    if positional.len() > positional_arity {
        if params.varargs_name.is_none() {
            return Err(format!(
                "function expected at most {positional_arity} positional arguments, got {}",
                positional.len()
            ));
        }
    }

    for (index, value) in positional.iter().take(positional_arity).copied().enumerate() {
        if value.is_null() {
            return Err(format!("positional argument {index} is NULL"));
        }
        bound[index] = value;
    }

    if let Some(slot) = params.varargs_slot() {
        bound[slot] = unsafe { build_tuple_from_slice(&positional[positional_arity..]) }?;
    }
    let mut varkw_pairs = Vec::new();

    for (name, value) in keywords.names.iter().copied().zip(keywords.values.iter().copied()) {
        if value.is_null() {
            return Err(format!("keyword argument {} is NULL", keyword_name(name)));
        }
        let Some(index) = params.names.iter().position(|candidate| *candidate == name) else {
            if params.varkw_name.is_some() {
                varkw_pairs.push((name, value));
                continue;
            }
            return Err(format!("unexpected keyword argument {}", keyword_name(name)));
        };
        if index < params.positional_only_count {
            return Err(format!(
                "positional-only parameter {} passed as keyword",
                keyword_name(name)
            ));
        }
        if !bound[index].is_null() {
            return Err(format!(
                "{} got multiple values for keyword argument '{}'",
                function_call_name(function),
                keyword_name(name)
            ));
        }
        bound[index] = value;
    }
    if let Some(slot) = params.varkw_slot() {
        bound[slot] = unsafe { build_kwargs_dict(&varkw_pairs) }?;
    }

    let default_start = positional_arity.saturating_sub(record.defaults.len());
    for index in 0..positional_arity {
        if bound[index].is_null() {
            if index >= default_start {
                let default_index = index - default_start;
                if let Some(default) = record.defaults.get(default_index) {
                    bound[index] = *default as *mut PyObject;
                }
            }
            if bound[index].is_null() {
                let name = params.names.get(index).copied().unwrap_or(0);
                return Err(format!("missing required positional argument {name}"));
            }
        }
    }

    let keyword_start = positional_arity;
    let keyword_end = keyword_start + params.keyword_only_count;
    for index in keyword_start..keyword_end {
        if bound[index].is_null() {
            let name = params.names.get(index).copied().unwrap_or(0);
            if let Some(default) = record.kwdefaults.get(&name) {
                bound[index] = *default as *mut PyObject;
            } else {
                return Err(format!("missing required keyword-only argument {name}"));
            }
        }
    }

    Ok(bound)
}

/// Bind and call a boxed function through Phase-B metadata when present.
pub unsafe fn call_bound_function(
    function: *mut PyObject,
    positional: &[*mut PyObject],
    keywords: KeywordArgs<'_>,
    star: Option<*mut PyObject>,
    dstar: Option<*mut PyObject>,
) -> Result<*mut PyObject, String> {
    let record = function_record(function);
    let mut argv = bind_arguments(function, positional, keywords, star, dstar)?;
    let code = if let Some(record) = record {
        record.entry
    } else {
        // SAFETY: The caller has already established that this is a function.
        unsafe { (*function.cast::<PyFunction>()).code }
    };
    if code.is_null() {
        return Err("function code pointer is null".to_owned());
    }
    pon_err_clear();
    // SAFETY: Function entrypoints are emitted with the compiled-code ABI.
    let entry: PyCodeFn = unsafe { mem::transmute(code) };
    // SAFETY: `argv` is contiguous and lives for the duration of the call.
    let result = unsafe { entry(argv.as_mut_ptr(), argv.len()) };
    if result.is_null() && !pon_err_occurred() {
        return Err("call returned NULL without setting an exception".to_owned());
    }
    Ok(result)
}

fn bind_phase_a_arguments(
    function: *mut PyObject,
    positional: &[*mut PyObject],
    keywords: KeywordArgs<'_>,
) -> Result<Vec<*mut PyObject>, String> {
    if !keywords.names.is_empty() {
        return Err("keyword arguments require Phase-B function metadata".to_owned());
    }
    if function.is_null() {
        return Err("callee is NULL".to_owned());
    }
    // SAFETY: The caller only invokes binding after the runtime type check.
    let arity = unsafe { (*function.cast::<PyFunction>()).arity };
    if positional.len() != arity {
        return Err(format!("function expected {arity} arguments, got {}", positional.len()));
    }
    for (index, value) in positional.iter().enumerate() {
        if value.is_null() {
            return Err(format!("positional argument {index} is NULL"));
        }
    }
    Ok(positional.to_vec())
}

unsafe fn copy_param_spec(params: *const ParamSpec) -> Result<Option<OwnedParamSpec>, String> {
    if params.is_null() {
        return Ok(None);
    }
    // SAFETY: The caller supplies a valid `ParamSpec` for the duration of this copy.
    let spec = unsafe { *params };
    if spec.names.is_null() && spec.total_param_count != 0 {
        return Err("ParamSpec names pointer is null".to_owned());
    }
    let names = if spec.total_param_count == 0 {
        Vec::new()
    } else {
        // SAFETY: `names` points to `total_param_count` interned ids by ABI contract.
        unsafe { core::slice::from_raw_parts(spec.names, spec.total_param_count as usize) }.to_vec()
    };
    let described = spec.positional_only_count as usize + spec.positional_count as usize + spec.keyword_only_count as usize;
    if described > names.len() {
        return Err(format!(
            "ParamSpec describes {described} named parameters but only {} names were supplied",
            names.len()
        ));
    }
    Ok(Some(OwnedParamSpec {
        names,
        positional_only_count: spec.positional_only_count as usize,
        positional_count: spec.positional_count as usize,
        keyword_only_count: spec.keyword_only_count as usize,
        varargs_name: (spec.varargs_name != 0).then_some(spec.varargs_name),
        varkw_name: (spec.varkw_name != 0).then_some(spec.varkw_name),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn dummy_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
        ptr::null_mut()
    }

    #[test]
    fn binds_positional_defaults_and_keyword_only_defaults() {
        let function = 0x1000usize as *mut PyObject;
        let names = [11_u32, 12, 13];
        let params = ParamSpec {
            names: names.as_ptr(),
            total_param_count: names.len() as u32,
            positional_only_count: 0,
            positional_count: 2,
            keyword_only_count: 1,
            varargs_name: 0,
            varkw_name: 0,
        };
        let code = CodeInfo {
            entry: dummy_entry as *const u8,
            params: &params,
            name_interned: 99,
            n_locals: 3,
            n_feedback: 0,
            flags: 0,
        };
        let arg_a = 0x2000usize as *mut PyObject;
        let default_b = 0x2001usize as *mut PyObject;
        let default_c = 0x2002usize as *mut PyObject;
        register_function_record(function, &code, &[default_b], &[13], &[default_c], &[]).unwrap();

        let bound = bind_arguments(
            function,
            &[arg_a],
            KeywordArgs {
                names: &[],
                values: &[],
            },
            None,
            None,
        )
        .unwrap();

        assert_eq!(bound, vec![arg_a, default_b, default_c]);
        unregister_function_record(function);
    }

    #[test]
    fn rejects_duplicate_keyword_binding() {
        let function = 0x1100usize as *mut PyObject;
        let names = [21_u32];
        let params = ParamSpec {
            names: names.as_ptr(),
            total_param_count: names.len() as u32,
            positional_only_count: 0,
            positional_count: 1,
            keyword_only_count: 0,
            varargs_name: 0,
            varkw_name: 0,
        };
        let code = CodeInfo {
            entry: dummy_entry as *const u8,
            params: &params,
            name_interned: 100,
            n_locals: 1,
            n_feedback: 0,
            flags: 0,
        };
        let positional = 0x3000usize as *mut PyObject;
        let keyword = 0x3001usize as *mut PyObject;
        register_function_record(function, &code, &[], &[], &[], &[]).unwrap();

        let err = bind_arguments(
            function,
            &[positional],
            KeywordArgs {
                names: &[21],
                values: &[keyword],
            },
            None,
            None,
        )
        .unwrap_err();

        assert!(err.contains("multiple values"));
        unregister_function_record(function);
    }
}
