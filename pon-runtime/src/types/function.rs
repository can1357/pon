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

use crate::abi::{CodeInfo, ParamSpec, return_null_with_error};
use crate::abi::call::{CODE_FLAG_COROUTINE, CODE_FLAG_GENERATOR};
use crate::intern::{self, intern, resolve};
use crate::object::{PyCodeFn, PyFunction, PyObject, PyUnicode};
use crate::thread_state::{pon_err_clear, pon_err_occurred};
use crate::types::{dict, generator::GeneratorKind, list::PyList, tuple::PyTuple};

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

/// Attribute lookup for function metadata exposed at Python level.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn function_getattro(function: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    if function.is_null() || name.is_null() {
        return return_null_with_error("function attribute lookup received NULL");
    }
    let Some(name_text) = (unsafe { (&*name.cast::<PyUnicode>()).as_str() }) else {
        return return_null_with_error("function attribute name is not valid UTF-8");
    };
    let name_id = intern(name_text);
    if name_id == intern("__name__") {
        let fname = resolve(unsafe { (*function.cast::<PyFunction>()).name_interned })
            .unwrap_or_else(|| "<lambda>".to_owned());
        return unsafe { crate::abi::pon_const_str(fname.as_ptr(), fname.len()) };
    }
    if name_id == intern("__annotations__") {
        return unsafe { function_annotations(function) };
    }
    if name_id == intern("__annotate__") {
        return function_annotate(function)
            .unwrap_or_else(|| unsafe { crate::abi::pon_none() });
    }
    return_null_with_error(format!("function has no attribute '{name_text}'"))
}

pub unsafe fn set_function_annotations(
    function: *mut PyObject,
    names: &[u32],
    values: &[*mut PyObject],
) -> Result<(), String> {
    if function.is_null() {
        return Err("cannot set annotations on NULL function".to_owned());
    }
    let annotations = unsafe { build_annotations_dict(names, values)? };
    unsafe {
        (*function.cast::<PyFunction>()).annotations = annotations;
    }
    Ok(())
}

/// PEP 649 side table: function object address -> synthesized `__annotate__`
/// function object address.  Entries are raw unrooted pointers, the same
/// accepted pattern as `FUNCTION_RECORDS` defaults/closures.
static ANNOTATE_FUNCTIONS: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Attach a synthesized PEP 649 `__annotate__` function to `function`.
pub fn set_function_annotate(function: *mut PyObject, annotate: *mut PyObject) -> Result<(), String> {
    if function.is_null() {
        return Err("cannot set __annotate__ on NULL function".to_owned());
    }
    if annotate.is_null() {
        return Err("cannot set NULL __annotate__ function".to_owned());
    }
    ANNOTATE_FUNCTIONS
        .lock()
        .map_err(|_| "annotate side table is poisoned".to_owned())?
        .insert(function as usize, annotate as usize);
    Ok(())
}

/// C ABI seam for `InstKind::FunctionSetAnnotate`: registers `annotate` as
/// the PEP 649 `__annotate__` of `function` and returns `function`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_function_set_annotate(
    function: *mut PyObject,
    annotate: *mut PyObject,
) -> *mut PyObject {
    match set_function_annotate(function, annotate) {
        Ok(()) => function,
        Err(message) => return_null_with_error(message),
    }
}

/// Return the synthesized `__annotate__` function for `function`, if any.
#[must_use]
pub fn function_annotate(function: *mut PyObject) -> Option<*mut PyObject> {
    ANNOTATE_FUNCTIONS
        .lock()
        .ok()
        .and_then(|table| table.get(&(function as usize)).copied())
        .map(|address| address as *mut PyObject)
}

/// Lazy PEP 649 `__annotations__`: return the cached dict, or call
/// `__annotate__(1)` (VALUE format) once and cache the result.  Functions
/// without an annotate function cache an empty dict (CPython identity
/// semantics: `f.__annotations__ is f.__annotations__`).
unsafe fn function_annotations(function: *mut PyObject) -> *mut PyObject {
    let function_ref = function.cast::<PyFunction>();
    let existing = unsafe { (*function_ref).annotations };
    if !existing.is_null() {
        return existing;
    }
    let annotations = match function_annotate(function) {
        Some(annotate) => {
            let format = unsafe { crate::abi::pon_const_int(1) };
            if format.is_null() {
                return ptr::null_mut();
            }
            let mut argv = [format];
            let result = unsafe { crate::abi::pon_call(annotate, argv.as_mut_ptr(), 1) };
            if result.is_null() {
                // Propagate NameError/NotImplementedError from the annotate
                // body without caching a partial dict.
                return ptr::null_mut();
            }
            result
        }
        None => unsafe { crate::abi::map::pon_build_map(ptr::null_mut(), 0) },
    };
    if annotations.is_null() {
        return annotations;
    }
    unsafe {
        (*function_ref).annotations = annotations;
    }
    annotations
}

unsafe fn build_annotations_dict(names: &[u32], values: &[*mut PyObject]) -> Result<*mut PyObject, String> {
    if names.len() != values.len() {
        return Err(format!(
            "annotation name/value length mismatch: {} names for {} values",
            names.len(),
            values.len()
        ));
    }
    let annotations = unsafe { crate::abi::map::pon_build_map(ptr::null_mut(), 0) };
    if annotations.is_null() {
        return Err("failed to allocate function annotations dict".to_owned());
    }
    for (name, value) in names.iter().copied().zip(values.iter().copied()) {
        if value.is_null() {
            return Err(format!("annotation for interned name {name} is NULL"));
        }
        let spelling = resolve(name).ok_or_else(|| format!("annotation name id {name} is not interned"))?;
        let key = unsafe { crate::abi::pon_const_str(spelling.as_ptr(), spelling.len()) };
        if key.is_null() {
            return Err(format!("failed to allocate annotation key for interned name {name}"));
        }
        unsafe { dict::dict_insert(annotations, key, value)? };
    }
    Ok(annotations)
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
                return Err(format!(
                    "missing 1 required keyword-only argument: '{}'",
                    keyword_name(name)
                ));
            }
        }
    }

    Ok(bound)
}

unsafe fn resume_lazy_delegate(
    frame_ref: &mut crate::abi::PyFrame,
    sent: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: `frame_ref.parent` stores the eager-yield generator materialized
    // by an earlier lazy resume.
    let result = unsafe { crate::abi::r#gen::pon_gen_send(frame_ref.parent, sent) };
    if !result.is_null() || !pon_err_occurred() {
        return result;
    }

    // When the eager delegate finishes, consume its StopIteration.value and raise a
    // fresh StopIteration with the same value from the wrapper.  This makes the
    // lazy layer's public exhaustion path carry the delegate's return value
    // instead of relying on callers to observe the delegate's pending exception.
    let stop_value = unsafe { crate::abi::r#gen::pon_gen_stop_value() };
    if stop_value.is_null() {
        return ptr::null_mut();
    }
    frame_ref.mark_exhausted();
    unsafe { crate::abi::exc::pon_raise_stop_iteration(stop_value) }
}

unsafe fn exhaust_delegate_for_return(delegate: *mut PyObject) -> *mut PyObject {
    loop {
        // SAFETY: The delegate is a generator-family object produced by the eager fallback.
        let item = unsafe { crate::abi::r#gen::pon_gen_send(delegate, ptr::null_mut()) };
        if !item.is_null() {
            continue;
        }
        if !pon_err_occurred() {
            return return_null_with_error("coroutine delegate ended without StopIteration");
        }
        let stop_value = unsafe { crate::abi::r#gen::pon_gen_stop_value() };
        if stop_value.is_null() {
            return ptr::null_mut();
        }
        return unsafe { crate::abi::exc::pon_raise_stop_iteration(stop_value) };
    }
}

unsafe extern "C" fn lazy_eager_generator_resume(
    frame: *mut crate::abi::PyFrame,
    sent: *mut PyObject,
) -> *mut PyObject {
    unsafe { lazy_eager_resume(frame, sent, false) }
}

unsafe extern "C" fn lazy_eager_coroutine_resume(
    frame: *mut crate::abi::PyFrame,
    sent: *mut PyObject,
) -> *mut PyObject {
    unsafe { lazy_eager_resume(frame, sent, true) }
}

unsafe fn lazy_eager_resume(
    frame: *mut crate::abi::PyFrame,
    sent: *mut PyObject,
    is_coroutine: bool,
) -> *mut PyObject {
    if frame.is_null() {
        return return_null_with_error("generator frame pointer is null");
    }
    // SAFETY: The generator owns this heap frame while it is resumable.
    let frame_ref = unsafe { &mut *frame };
    if !frame_ref.parent.is_null() {
        return unsafe { resume_lazy_delegate(frame_ref, sent) };
    }
    if frame_ref.n_locals == 0 {
        return return_null_with_error("lazy generator frame has no function slot");
    }

    // SAFETY: Slot 0 stores the boxed function; later slots store bound arguments.
    let function = unsafe { crate::abi::r#gen::pon_frame_get_local(frame, 0) };
    if function.is_null() {
        return ptr::null_mut();
    }
    let Some(record) = function_record(function) else {
        return return_null_with_error("lazy generator function has no metadata record");
    };
    if record.entry.is_null() {
        return return_null_with_error("lazy generator function code pointer is null");
    }
    let argc = frame_ref.n_locals.saturating_sub(1) as usize;
    let mut argv = Vec::with_capacity(argc);
    for index in 0..argc {
        // SAFETY: The loop stays inside the frame's advertised local count.
        let value = unsafe { crate::abi::r#gen::pon_frame_get_local(frame, (index + 1) as u32) };
        if value.is_null() {
            return ptr::null_mut();
        }
        argv.push(value);
    }

    // SAFETY: Function entrypoints are emitted with the compiled-code ABI.
    let entry: PyCodeFn = unsafe { mem::transmute(record.entry) };
    let result = if is_coroutine {
        let (sent_override, print_suppression) = if frame_ref.state == 0 {
            (ptr::null_mut(), 1)
        } else {
            (sent, 2)
        };
        crate::abi::r#gen::with_eager_coroutine_replay(sent_override, print_suppression, || {
            let _guard = crate::abi::push_current_call(function.cast::<PyFunction>(), argv.as_mut_ptr(), argv.len());
            pon_err_clear();
            // SAFETY: `argv` is contiguous and lives for the duration of the call.
            unsafe { entry(argv.as_mut_ptr(), argv.len()) }
        })
    } else {
        crate::abi::r#gen::with_eager_yield_recording(|| {
            let _guard = crate::abi::push_current_call(function.cast::<PyFunction>(), argv.as_mut_ptr(), argv.len());
            pon_err_clear();
            // SAFETY: `argv` is contiguous and lives for the duration of the call.
            unsafe { entry(argv.as_mut_ptr(), argv.len()) }
        })
    };
    if result.is_null() {
        return ptr::null_mut();
    }
    // Compiled generator bodies return the delegate produced by the eager
    // fallback (`GetIter(None)` for fallthrough or `pon_eager_yield_generator`
    // for explicit `return value`).  Do not wrap it again, or the stored
    // StopIteration.value is lost behind a second empty generator.
    let delegate = result;
    if delegate.is_null() {
        return ptr::null_mut();
    }
    if is_coroutine {
        if frame_ref.state == 0 {
            frame_ref.state = 1;
            return unsafe { crate::abi::r#gen::pon_gen_send(delegate, sent) };
        }
        frame_ref.mark_exhausted();
        return unsafe { exhaust_delegate_for_return(delegate) };
    }
    unsafe { crate::sync::store_heap_pointer(ptr::addr_of_mut!(frame_ref.parent), delegate) };
    unsafe { resume_lazy_delegate(frame_ref, sent) }
}

unsafe fn make_lazy_eager_generator(
    function: *mut PyObject,
    argv: &[*mut PyObject],
    kind: GeneratorKind,
) -> Result<*mut PyObject, String> {
    let n_locals = u32::try_from(argv.len().saturating_add(1))
        .map_err(|_| "generator/coroutine call has too many bound arguments".to_owned())?;
    // SAFETY: Allocates a frame with one function slot plus bound argument slots.
    let frame = unsafe { crate::abi::r#gen::pon_make_frame(n_locals) };
    if frame.is_null() {
        return Ok(ptr::null_mut());
    }
    // SAFETY: The frame was allocated with `n_locals` slots.
    if unsafe { crate::abi::r#gen::pon_frame_set_local(frame, 0, function) }.is_null() {
        return Ok(ptr::null_mut());
    }
    for (index, value) in argv.iter().copied().enumerate() {
        // SAFETY: Slots 1..=argv.len() are in bounds by construction.
        if unsafe { crate::abi::r#gen::pon_frame_set_local(frame, (index + 1) as u32, value) }.is_null() {
            return Ok(ptr::null_mut());
        }
    }
    let resume = if kind == GeneratorKind::Coroutine {
        lazy_eager_coroutine_resume
    } else {
        lazy_eager_generator_resume
    };
    // SAFETY: `resume` follows `GenResumeFn`; `frame` is live.
    Ok(unsafe {
        crate::abi::r#gen::pon_make_generator(
            resume,
            frame,
            kind.as_u8(),
        )
    })
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
    if let Some(record) = record.as_ref() {
        if record.flags & CODE_FLAG_COROUTINE != 0 {
            return unsafe { make_lazy_eager_generator(function, &argv, GeneratorKind::Coroutine) };
        }
        if record.flags & CODE_FLAG_GENERATOR != 0 {
            return unsafe { make_lazy_eager_generator(function, &argv, GeneratorKind::Generator) };
        }
    }
    let code = if let Some(record) = record {
        record.entry
    } else {
        // SAFETY: The caller has already established that this is a function.
        unsafe { (*function.cast::<PyFunction>()).code }
    };
    if code.is_null() {
        return Err("function code pointer is null".to_owned());
    }
    let _guard = crate::abi::push_current_call(function.cast::<PyFunction>(), argv.as_mut_ptr(), argv.len());
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
    if function.is_null() {
        return Err("callee is NULL".to_owned());
    }
    if !keywords.names.is_empty() {
        return bind_native_keywords(function, positional, keywords);
    }
    // SAFETY: The caller only invokes binding after the runtime type check.
    let arity = unsafe { (*function.cast::<PyFunction>()).arity };
    if arity != crate::builtins::variadic_arity() && positional.len() != arity {
        return Err(format!("function expected {arity} arguments, got {}", positional.len()));
    }
    for (index, value) in positional.iter().enumerate() {
        if value.is_null() {
            return Err(format!("positional argument {index} is NULL"));
        }
    }
    Ok(positional.to_vec())
}

fn bind_native_keywords(
    function: *mut PyObject,
    positional: &[*mut PyObject],
    keywords: KeywordArgs<'_>,
) -> Result<Vec<*mut PyObject>, String> {
    let Some(name) = function_name(function) else {
        return Err("keyword arguments require Phase-B function metadata".to_owned());
    };
    match name.as_str() {
        "sorted" => bind_sorted_keywords(positional, keywords),
        "enumerate" => bind_single_keyword(positional, keywords, "enumerate", "start", 1, 2),
        _ => Err("keyword arguments require Phase-B function metadata".to_owned()),
    }
}

fn bind_sorted_keywords(positional: &[*mut PyObject], keywords: KeywordArgs<'_>) -> Result<Vec<*mut PyObject>, String> {
    if positional.is_empty() || positional.len() > 2 {
        return Err(format!("sorted() expected 1 or 2 positional arguments, got {}", positional.len()));
    }
    let mut key = positional.get(1).copied();
    let mut reverse = None;
    for (name, value) in keywords.names.iter().copied().zip(keywords.values.iter().copied()) {
        if value.is_null() {
            return Err(format!("keyword argument {} is NULL", keyword_name(name)));
        }
        match keyword_name(name).as_str() {
            "key" => {
                if key.is_some() {
                    return Err("sorted() got multiple values for keyword argument 'key'".to_owned());
                }
                key = Some(value);
            }
            "reverse" => {
                if reverse.is_some() {
                    return Err("sorted() got multiple values for keyword argument 'reverse'".to_owned());
                }
                reverse = Some(value);
            }
            other => return Err(format!("sorted() got an unexpected keyword argument '{other}'")),
        }
    }
    let mut argv = Vec::with_capacity(1 + usize::from(key.is_some() || reverse.is_some()) + usize::from(reverse.is_some()));
    argv.push(positional[0]);
    if let Some(key) = key {
        argv.push(key);
    } else if reverse.is_some() {
        let none = unsafe { crate::abi::pon_none() };
        if none.is_null() {
            return Err("failed to allocate None for sorted key placeholder".to_owned());
        }
        argv.push(none);
    }
    if let Some(reverse) = reverse {
        argv.push(reverse);
    }
    Ok(argv)
}

fn bind_single_keyword(
    positional: &[*mut PyObject],
    keywords: KeywordArgs<'_>,
    function_name: &str,
    keyword: &str,
    min_positional: usize,
    max_positional: usize,
) -> Result<Vec<*mut PyObject>, String> {
    if positional.len() < min_positional || positional.len() > max_positional {
        return Err(format!(
            "{function_name}() expected {min_positional} to {max_positional} positional arguments, got {}",
            positional.len()
        ));
    }
    let mut argv = positional.to_vec();
    for (name, value) in keywords.names.iter().copied().zip(keywords.values.iter().copied()) {
        if value.is_null() {
            return Err(format!("keyword argument {} is NULL", keyword_name(name)));
        }
        let actual = keyword_name(name);
        if actual != keyword {
            return Err(format!("{function_name}() got an unexpected keyword argument '{actual}'"));
        }
        if argv.len() == max_positional {
            return Err(format!("{function_name}() got multiple values for keyword argument '{keyword}'"));
        }
        argv.push(value);
    }
    Ok(argv)
}

unsafe fn copy_param_spec(params: *const ParamSpec) -> Result<Option<OwnedParamSpec>, String> {
    if params.is_null() {
        return Ok(None);
    }
    // SAFETY: The caller supplies a valid `ParamSpec` for the duration of this copy.
    let spec = unsafe { *params };
    let named_count = spec.positional_only_count as usize
        + spec.positional_count as usize
        + spec.keyword_only_count as usize;
    if spec.names.is_null() && named_count != 0 {
        return Err("ParamSpec names pointer is null".to_owned());
    }
    let names = if named_count == 0 {
        Vec::new()
    } else {
        // SAFETY: `names` points to every named positional/keyword-only id.
        unsafe { core::slice::from_raw_parts(spec.names, named_count) }.to_vec()
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
    fn binds_keyword_only_default_without_masking_later_required_parameter() {
        let function = 0x1200usize as *mut PyObject;
        let positional_name = crate::intern::intern("positional");
        let defaulted_kwonly_name = crate::intern::intern("defaulted_kwonly");
        let required_kwonly_name = crate::intern::intern("required_kwonly");
        let function_name = crate::intern::intern("kwonly_binding_case");
        let names = [positional_name, defaulted_kwonly_name, required_kwonly_name];
        let params = ParamSpec {
            names: names.as_ptr(),
            total_param_count: names.len() as u32,
            positional_only_count: 0,
            positional_count: 1,
            keyword_only_count: 2,
            varargs_name: 0,
            varkw_name: 0,
        };
        let code = CodeInfo {
            entry: dummy_entry as *const u8,
            params: &params,
            name_interned: function_name,
            n_locals: 3,
            n_feedback: 0,
            flags: 0,
        };
        let positional_arg = 0x2000usize as *mut PyObject;
        let defaulted_kwonly_default = 0x2001usize as *mut PyObject;
        let supplied_required_kwonly = 0x2002usize as *mut PyObject;
        register_function_record(
            function,
            &code,
            &[],
            &[defaulted_kwonly_name],
            &[defaulted_kwonly_default],
            &[],
        )
        .unwrap();

        let err = bind_arguments(
            function,
            &[positional_arg],
            KeywordArgs {
                names: &[],
                values: &[],
            },
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(
            err,
            "missing 1 required keyword-only argument: 'required_kwonly'"
        );

        let bound = bind_arguments(
            function,
            &[positional_arg],
            KeywordArgs {
                names: &[required_kwonly_name],
                values: &[supplied_required_kwonly],
            },
            None,
            None,
        )
        .unwrap();

        assert_eq!(
            bound,
            vec![
                positional_arg,
                defaulted_kwonly_default,
                supplied_required_kwonly
            ]
        );
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

    unsafe extern "C" fn lazy_return_six_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
        for value in [2, 4] {
            let object = unsafe { crate::abi::pon_const_int(value) };
            if object.is_null() {
                return ptr::null_mut();
            }
            if unsafe { crate::abi::r#gen::pon_yield(object) }.is_null() {
                return ptr::null_mut();
            }
        }
        let stop_value = unsafe { crate::abi::pon_const_int(6) };
        if stop_value.is_null() {
            return ptr::null_mut();
        }
        unsafe { crate::abi::r#gen::pon_eager_yield_generator(stop_value) }
    }

    #[test]
    fn lazy_eager_generator_preserves_delegate_return_value() {
        let _guard = crate::thread_state::test_state_lock();
        unsafe {
            assert_eq!(crate::abi::pon_runtime_init(), 0);
            crate::thread_state::pon_err_clear();
            let function = crate::abi::pon_make_function(
                lazy_return_six_entry as *const u8,
                0,
                crate::intern::intern("lazy_return_six_entry"),
            );
            assert!(!function.is_null());
            let code = CodeInfo {
                entry: lazy_return_six_entry as *const u8,
                params: ptr::null(),
                name_interned: crate::intern::intern("lazy_return_six_entry"),
                n_locals: 0,
                n_feedback: 0,
                flags: CODE_FLAG_GENERATOR,
            };
            register_function_record(function, &code, &[], &[], &[], &[]).unwrap();

            let generator = call_bound_function(
                function,
                &[],
                KeywordArgs {
                    names: &[],
                    values: &[],
                },
                None,
                None,
            )
            .unwrap();
            assert!(!generator.is_null());

            let first = crate::abi::r#gen::pon_gen_send(generator, crate::abi::pon_none());
            assert_eq!(crate::abi::format_object_for_print(first).as_deref(), Ok("2"));
            let second = crate::abi::r#gen::pon_gen_send(generator, crate::abi::pon_none());
            assert_eq!(crate::abi::format_object_for_print(second).as_deref(), Ok("4"));
            let done = crate::abi::r#gen::pon_gen_send(generator, crate::abi::pon_none());
            assert!(done.is_null());
            assert!(crate::thread_state::pon_err_occurred());
            let stop_value = crate::abi::r#gen::pon_gen_stop_value();
            assert_eq!(crate::abi::format_object_for_print(stop_value).as_deref(), Ok("6"));
            assert!(!crate::thread_state::pon_err_occurred());
            unregister_function_record(function);
        }
    }
}
