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
use crate::intern::{self, intern, resolve};
use crate::object::{PyCodeFn, PyFunction, PyObject, PyObjectHeader, PyType, PyUnicode};
use crate::thread_state::{pon_err_clear, pon_err_occurred};
use crate::types::{dict, list::PyList, tuple::PyTuple, type_::{self, PyClassDict}};

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

const CPY_CO_OPTIMIZED: u32 = 0x01;
const CPY_CO_NEWLOCALS: u32 = 0x02;
const CPY_CO_VARARGS: u32 = 0x04;
const CPY_CO_VARKEYWORDS: u32 = 0x08;
const CPY_CO_GENERATOR: u32 = 0x20;
const CPY_CO_COROUTINE: u32 = 0x80;

#[derive(Clone, Debug, PartialEq, Eq)]
struct FunctionCodeMetadata {
    name_interned: u32,
    n_locals: u32,
    flags: u32,
    params: Option<OwnedParamSpec>,
}

#[repr(C)]
struct PyFunctionCodeObject {
    ob_base: PyObjectHeader,
    metadata: FunctionCodeMetadata,
}

#[repr(C)]
struct PyFunctionGetSetDescriptor {
    ob_base: PyObjectHeader,
    name: u32,
}

fn function_code_type() -> *mut PyType {
    static CODE_TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(ptr::null(), "code", mem::size_of::<PyFunctionCodeObject>());
        ty.tp_getattro = Some(function_code_getattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    *CODE_TYPE as *mut PyType
}

fn function_getset_descriptor_type() -> *mut PyType {
    static DESCRIPTOR_TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(ptr::null(), "getset_descriptor", mem::size_of::<PyFunctionGetSetDescriptor>());
        ty.tp_descr_get = Some(function_getset_descr_get);
        Box::into_raw(Box::new(ty)) as usize
    });
    *DESCRIPTOR_TYPE as *mut PyType
}

/// Install Python-visible function/code descriptor metadata on the runtime's
/// singleton `function` type.
///
/// The compiler only needs a lightweight code-object shell here: stdlib modules
/// such as `types` and `inspect` probe `__code__` for identity and signature
/// metadata, while execution continues to use the raw entrypoint in
/// [`PyFunction`].
pub unsafe fn install_function_type_attrs(function_type: *mut PyType, type_type: *mut PyType) {
    if function_type.is_null() {
        return;
    }
    let code_type = function_code_type();
    let descriptor_type = function_getset_descriptor_type();
    unsafe {
        (*code_type).ob_base.ob_type = type_type;
        (*descriptor_type).ob_base.ob_type = type_type;
    }

    let dict = unsafe {
        if (*function_type).tp_dict.is_null() {
            let dict = type_::new_namespace();
            (*function_type).tp_dict = dict.cast::<PyObject>();
            dict
        } else {
            (*function_type).tp_dict.cast::<PyClassDict>()
        }
    };
    for attr in [
        "__code__",
        "__globals__",
        "__defaults__",
        "__kwdefaults__",
        "__closure__",
        "__annotations__",
        "__annotate__",
    ] {
        let name = intern(attr);
        let descriptor = Box::into_raw(Box::new(PyFunctionGetSetDescriptor {
            ob_base: PyObjectHeader::new(descriptor_type),
            name,
        }))
        .cast::<PyObject>();
        unsafe { (&mut *dict).set(name, descriptor) };
    }
}

unsafe extern "C" fn function_getset_descr_get(
    descr: *mut PyObject,
    obj: *mut PyObject,
    _owner: *mut PyObject,
) -> *mut PyObject {
    if descr.is_null() {
        return return_null_with_error("function descriptor is NULL");
    }
    if obj.is_null() {
        return descr;
    }
    let obj_ty = unsafe { (*obj).ob_type };
    if !obj_ty.is_null() && unsafe { (*obj_ty).name() == "type" } {
        return descr;
    }
    let name = unsafe { (*descr.cast::<PyFunctionGetSetDescriptor>()).name };
    function_attr_by_id(obj, name).unwrap_or_else(|| return_null_with_error("unknown function descriptor"))
}

fn const_str(text: &str) -> *mut PyObject {
    unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn const_name(name: u32) -> *mut PyObject {
    let text = resolve(name).unwrap_or_else(|| format!("<interned:{name}>"));
    const_str(&text)
}

fn empty_tuple() -> *mut PyObject {
    unsafe { crate::abi::seq::pon_build_tuple(ptr::null_mut(), 0) }
}

fn tuple_from_names(names: impl IntoIterator<Item = u32>) -> *mut PyObject {
    let mut values = Vec::new();
    for name in names {
        let value = const_name(name);
        if value.is_null() {
            return ptr::null_mut();
        }
        values.push(value);
    }
    unsafe {
        crate::abi::seq::pon_build_tuple(
            if values.is_empty() { ptr::null_mut() } else { values.as_mut_ptr() },
            values.len(),
        )
    }
}

fn tuple_from_objects(mut values: Vec<*mut PyObject>) -> *mut PyObject {
    unsafe {
        crate::abi::seq::pon_build_tuple(
            if values.is_empty() { ptr::null_mut() } else { values.as_mut_ptr() },
            values.len(),
        )
    }
}

fn code_metadata_for_function(function: *mut PyObject) -> FunctionCodeMetadata {
    if let Some(record) = function_record(function) {
        return FunctionCodeMetadata {
            name_interned: record.name_interned,
            n_locals: record.n_locals,
            flags: record.flags,
            params: record.params,
        };
    }
    let function_ref = unsafe { &*function.cast::<PyFunction>() };
    FunctionCodeMetadata {
        name_interned: function_ref.name_interned,
        n_locals: u32::try_from(function_ref.arity).unwrap_or(u32::MAX),
        flags: 0,
        params: None,
    }
}

fn alloc_code_object(function: *mut PyObject) -> *mut PyObject {
    if function.is_null() {
        return return_null_with_error("cannot read __code__ from NULL function");
    }
    Box::into_raw(Box::new(PyFunctionCodeObject {
        ob_base: PyObjectHeader::new(function_code_type()),
        metadata: code_metadata_for_function(function),
    }))
    .cast::<PyObject>()
}

fn cpython_code_flags(metadata: &FunctionCodeMetadata) -> u32 {
    let mut flags = CPY_CO_OPTIMIZED | CPY_CO_NEWLOCALS;
    if metadata.params.as_ref().and_then(|params| params.varargs_name).is_some() {
        flags |= CPY_CO_VARARGS;
    }
    if metadata.params.as_ref().and_then(|params| params.varkw_name).is_some() {
        flags |= CPY_CO_VARKEYWORDS;
    }
    if metadata.flags & crate::abi::call::CODE_FLAG_GENERATOR != 0 {
        flags |= CPY_CO_GENERATOR;
    }
    if metadata.flags & crate::abi::call::CODE_FLAG_COROUTINE != 0 {
        flags |= CPY_CO_COROUTINE;
    }
    flags
}

fn code_varnames(metadata: &FunctionCodeMetadata) -> *mut PyObject {
    let Some(params) = metadata.params.as_ref() else {
        return empty_tuple();
    };
    let names = params
        .names
        .iter()
        .copied()
        .chain(params.varargs_name)
        .chain(params.varkw_name);
    tuple_from_names(names)
}

unsafe extern "C" fn function_code_getattro(code: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    if code.is_null() || name.is_null() {
        return return_null_with_error("code attribute lookup received NULL");
    }
    let Some(name_text) = (unsafe { (&*name.cast::<PyUnicode>()).as_str() }) else {
        return return_null_with_error("code attribute name is not valid UTF-8");
    };
    let name_id = intern(name_text);
    let metadata = unsafe { &(*code.cast::<PyFunctionCodeObject>()).metadata };
    if name_id == intern("__class__") {
        return function_code_type().cast::<PyObject>();
    }
    if name_id == intern("co_flags") {
        return unsafe { crate::abi::pon_const_int(i64::from(cpython_code_flags(metadata))) };
    }
    if name_id == intern("co_argcount") {
        let value = metadata.params.as_ref().map_or(0, OwnedParamSpec::positional_arity);
        return unsafe { crate::abi::pon_const_int(value as i64) };
    }
    if name_id == intern("co_posonlyargcount") {
        let value = metadata.params.as_ref().map_or(0, |params| params.positional_only_count);
        return unsafe { crate::abi::pon_const_int(value as i64) };
    }
    if name_id == intern("co_kwonlyargcount") {
        let value = metadata.params.as_ref().map_or(0, |params| params.keyword_only_count);
        return unsafe { crate::abi::pon_const_int(value as i64) };
    }
    if name_id == intern("co_nlocals") {
        return unsafe { crate::abi::pon_const_int(i64::from(metadata.n_locals)) };
    }
    if name_id == intern("co_varnames") {
        return code_varnames(metadata);
    }
    if name_id == intern("co_name") || name_id == intern("co_qualname") {
        return const_name(metadata.name_interned);
    }
    if name_id == intern("co_filename") {
        return const_str("<pon>");
    }
    if name_id == intern("co_firstlineno") || name_id == intern("co_stacksize") {
        return unsafe { crate::abi::pon_const_int(1) };
    }
    if name_id == intern("co_consts")
        || name_id == intern("co_names")
        || name_id == intern("co_freevars")
        || name_id == intern("co_cellvars")
    {
        return empty_tuple();
    }
    if name_id == intern("co_code") || name_id == intern("co_lnotab") {
        return const_str("");
    }
    return_null_with_error(format!("code object has no attribute '{name_text}'"))
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

fn function_attr_by_id(function: *mut PyObject, name_id: u32) -> Option<*mut PyObject> {
    if name_id == intern("__class__") {
        let ty = unsafe { (*function.cast::<PyFunction>()).ob_base.ob_type };
        return Some(ty.cast_mut().cast::<PyObject>());
    }
    if name_id == intern("__name__") || name_id == intern("__qualname__") {
        return Some(const_name(unsafe { (*function.cast::<PyFunction>()).name_interned }));
    }
    if name_id == intern("__code__") {
        return Some(alloc_code_object(function));
    }
    if name_id == intern("__globals__") {
        return Some(unsafe { crate::dynexec::builtin_globals(ptr::null_mut(), 0) });
    }
    if name_id == intern("__defaults__") {
        let Some(record) = function_record(function) else {
            return Some(unsafe { crate::abi::pon_none() });
        };
        if record.defaults.is_empty() {
            return Some(unsafe { crate::abi::pon_none() });
        }
        return Some(tuple_from_objects(record.defaults()));
    }
    if name_id == intern("__kwdefaults__") {
        let Some(record) = function_record(function) else {
            return Some(unsafe { crate::abi::pon_none() });
        };
        if record.kwdefaults.is_empty() {
            return Some(unsafe { crate::abi::pon_none() });
        }
        let mut pairs = Vec::with_capacity(record.kwdefaults.len() * 2);
        for (name, value) in record.kwdefaults {
            let key = const_name(name);
            if key.is_null() {
                return Some(ptr::null_mut());
            }
            pairs.push(key);
            pairs.push(value as *mut PyObject);
        }
        return Some(unsafe { crate::abi::map::pon_build_map(pairs.as_mut_ptr(), pairs.len() / 2) });
    }
    if name_id == intern("__closure__") {
        let Some(record) = function_record(function) else {
            return Some(unsafe { crate::abi::pon_none() });
        };
        let closure = record.closure();
        if closure.is_empty() {
            return Some(unsafe { crate::abi::pon_none() });
        }
        return Some(tuple_from_objects(closure));
    }
    if name_id == intern("__annotations__") {
        return Some(unsafe { function_annotations(function) });
    }
    if name_id == intern("__annotate__") {
        return Some(function_annotate(function).unwrap_or_else(|| unsafe { crate::abi::pon_none() }));
    }
    if name_id == intern("__dict__") {
        return Some(unsafe { crate::abi::map::pon_build_map(ptr::null_mut(), 0) });
    }
    None
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
    if let Some(value) = function_attr_by_id(function, name_id) {
        return value;
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
    crate::untag_prelude!(function, annotate);
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
    // Generator/coroutine functions need no special casing here: the compiled
    // stub at the function's entry allocates the frame and returns the
    // generator object itself (pin J0.1 §4.0).
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
        "sum" => bind_single_keyword(positional, keywords, "sum", "start", 1, 2),
        "round" => bind_named_positional_keywords(positional, keywords, "round", &["number", "ndigits"], 1, 2),
        "pow" => bind_named_positional_keywords(positional, keywords, "pow", &["base", "exp", "mod"], 2, 3),
        "min" => bind_minmax_keywords(positional, keywords, "min"),
        "max" => bind_minmax_keywords(positional, keywords, "max"),
        "zip" => bind_zip_keywords(positional, keywords),
        "enumerate" => bind_single_keyword(positional, keywords, "enumerate", "start", 1, 2),
        _ => Err("keyword arguments require Phase-B function metadata".to_owned()),
    }
}

fn bind_sorted_keywords(positional: &[*mut PyObject], keywords: KeywordArgs<'_>) -> Result<Vec<*mut PyObject>, String> {
    if positional.len() != 1 {
        return Err(format!("sorted expected 1 argument, got {}", positional.len()));
    }
    let mut key = unsafe { crate::abi::pon_none() };
    if key.is_null() {
        return Err("failed to allocate None for sorted key default".to_owned());
    }
    let mut reverse = false;
    for (name, value) in keywords.names.iter().copied().zip(keywords.values.iter().copied()) {
        if value.is_null() {
            return Err(format!("keyword argument {} is NULL", keyword_name(name)));
        }
        match keyword_name(name).as_str() {
            "key" => key = value,
            "reverse" => {
                reverse = match unsafe { crate::abi::pon_is_true(value) } {
                    0 => false,
                    1 => true,
                    _ => return Err("reverse truth-value testing failed".to_owned()),
                };
            }
            other => return Err(format!("sort() got an unexpected keyword argument '{other}'")),
        }
    }
    Ok(vec![positional[0], crate::types::lazy_iter::new_sort_options(key, reverse)])
}

fn bind_minmax_keywords(positional: &[*mut PyObject], keywords: KeywordArgs<'_>, function_name: &str) -> Result<Vec<*mut PyObject>, String> {
    if positional.is_empty() {
        return Err(format!("{function_name} expected at least 1 argument, got 0"));
    }
    let mut key = unsafe { crate::abi::pon_none() };
    if key.is_null() {
        return Err(format!("failed to allocate None for {function_name} key default"));
    }
    let mut default = unsafe { crate::abi::pon_none() };
    if default.is_null() {
        return Err(format!("failed to allocate None for {function_name} default"));
    }
    let mut has_default = false;
    for (name, value) in keywords.names.iter().copied().zip(keywords.values.iter().copied()) {
        if value.is_null() {
            return Err(format!("keyword argument {} is NULL", keyword_name(name)));
        }
        match keyword_name(name).as_str() {
            "key" => key = value,
            "default" => {
                default = value;
                has_default = true;
            }
            other => return Err(format!("{function_name}() got an unexpected keyword argument '{other}'")),
        }
    }
    let mut argv = positional.to_vec();
    argv.push(crate::types::lazy_iter::new_minmax_options(key, default, has_default));
    Ok(argv)
}

fn bind_zip_keywords(positional: &[*mut PyObject], keywords: KeywordArgs<'_>) -> Result<Vec<*mut PyObject>, String> {
    let mut strict = false;
    for (name, value) in keywords.names.iter().copied().zip(keywords.values.iter().copied()) {
        if value.is_null() {
            return Err(format!("keyword argument {} is NULL", keyword_name(name)));
        }
        match keyword_name(name).as_str() {
            "strict" => {
                strict = match unsafe { crate::abi::pon_is_true(value) } {
                    0 => false,
                    1 => true,
                    _ => return Err("strict truth-value testing failed".to_owned()),
                };
            }
            other => return Err(format!("zip() got an unexpected keyword argument '{other}'")),
        }
    }
    let mut argv = positional.to_vec();
    argv.push(crate::types::lazy_iter::new_zip_strict_marker(strict));
    Ok(argv)
}

fn bind_named_positional_keywords(
    positional: &[*mut PyObject],
    keywords: KeywordArgs<'_>,
    function_name: &str,
    names: &[&str],
    min_positional: usize,
    max_positional: usize,
) -> Result<Vec<*mut PyObject>, String> {
    if positional.len() > max_positional {
        return Err(format!("{function_name}() expected at most {max_positional} positional arguments, got {}", positional.len()));
    }
    let mut argv = positional.to_vec();
    argv.resize(max_positional, ptr::null_mut());
    for (name, value) in keywords.names.iter().copied().zip(keywords.values.iter().copied()) {
        if value.is_null() {
            return Err(format!("keyword argument {} is NULL", keyword_name(name)));
        }
        let actual = keyword_name(name);
        let Some(index) = names.iter().position(|expected| *expected == actual) else {
            return Err(format!("{function_name}() got an unexpected keyword argument '{actual}'"));
        };
        if index < positional.len() || !argv[index].is_null() {
            return Err(format!("{function_name}() got multiple values for argument '{actual}'"));
        }
        argv[index] = value;
    }
    while argv.last().is_some_and(|value| value.is_null()) {
        argv.pop();
    }
    if argv.iter().any(|value| value.is_null()) {
        return Err(format!("{function_name}() missing required argument"));
    }
    if argv.len() < min_positional {
        return Err(format!("{function_name}() expected at least {min_positional} arguments, got {}", argv.len()));
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

}
