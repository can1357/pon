//! Function object metadata and Phase-B argument binding.
//!
//! The boxed `PyFunction` layout still lives in `crate::object` for the Phase-A
//! ABI.  Phase-B call helpers need richer metadata (defaults, keyword-only
//! defaults, closure cells, and `ParamSpec` copies) before the central object
//! layout is widened, so this module owns a side table keyed by function object
//! address.  The table is deliberately boring: raw object pointers are stored as
//! integer addresses so the global mutex remains `Send`, and every public helper
//! returns a `Result` instead of unwinding across the C ABI.

use core::ffi::c_int;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::mem;
use std::ptr;
use std::sync::{LazyLock, Mutex};

use crate::abi::{CodeInfo, ParamSpec, return_null_with_error};
use crate::intern::{self, intern, resolve};
use crate::object::{PyCodeFn, PyFunction, PyObject, PyObjectHeader, PyType, PyUnicode};
use crate::thread_state::{pon_err_occurred, pon_err_set};
use crate::types::{classmethod, dict, list::PyList, method, tuple::PyTuple, type_::{self, PyClassDict}};

static FUNCTION_RECORDS: LazyLock<Mutex<HashMap<usize, FunctionRecord>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Live `__defaults__`/`__kwdefaults__` overrides installed by attribute
/// assignment after function creation (`f.__defaults__ = ...`).
///
/// Fields hold raw object addresses (a validated tuple/dict payload or the
/// `None` singleton for a cleared slot) — the same accepted pattern as
/// [`FUNCTION_RECORDS`].  `None` in a field means that attribute was never
/// assigned, so creation-time metadata stays authoritative.  A side table
/// (not a `PyFunction` field) keeps `bind_arguments`' documented contract of
/// never dereferencing the function pointer itself; entries are cleared by
/// the GC dealloc hook via [`unregister_function_record`].
#[derive(Clone, Copy, Debug, Default)]
struct DefaultsOverride {
    defaults: Option<usize>,
    kwdefaults: Option<usize>,
}

static FUNCTION_DEFAULT_OVERRIDES: LazyLock<Mutex<HashMap<usize, DefaultsOverride>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Snapshot of the live defaults overrides for `function` (both slots
/// `None` when nothing was ever assigned).
fn defaults_override(function: *mut PyObject) -> DefaultsOverride {
    FUNCTION_DEFAULT_OVERRIDES
        .lock()
        .ok()
        .and_then(|table| table.get(&(function as usize)).copied())
        .unwrap_or_default()
}

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

/// Reports the GC-managed objects held by `function`'s side-table records to a
/// GC trace visitor.
///
/// The records are malloc'd side storage keyed by object address, so the
/// collector cannot reach these values through the `PyFunction` allocation;
/// `abi::trace_function` forwards here.  Reported pointers may be tagged
/// immediates or NULL — the GC's pointer classification filters those.
pub fn visit_function_gc_refs(function: *mut PyObject, visitor: &mut dyn FnMut(*mut u8)) {
    let records = FUNCTION_RECORDS.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(record) = records.get(&(function as usize)) {
        for value in record
            .defaults
            .iter()
            .chain(record.kwdefaults.values())
            .chain(record.closure.iter())
        {
            let object = *value as *mut u8;
            if !object.is_null() {
                visitor(object);
            }
        }
    }
    drop(records);
    let override_ = defaults_override(function);
    for stored in [override_.defaults, override_.kwdefaults].into_iter().flatten() {
        visitor(stored as *mut u8);
    }
    if let Some(module_object) = function_module_object(function)
        && let Some(values) = crate::import::module_object_attr_values(module_object)
    {
        for value in values {
            visitor(value.cast::<u8>());
        }
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
const CPY_CO_ASYNC_GENERATOR: u32 = 0x200;

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
    /// co_* attribute replacements installed by `code.replace(**kwargs)`:
    /// `(interned co_* name, replacement object)` pairs, last write wins.
    /// Values are raw unrooted addresses — the accepted `FUNCTION_RECORDS`
    /// defaults/closures pattern (code shells are Box-allocated and never
    /// GC-traced).  `function_code_getattro` consults these before the
    /// derived metadata views, so a replaced field reads back exactly the
    /// object that was passed (CPython `code.replace` returns a new code
    /// object with the named fields swapped; pon's shells carry no
    /// executable payload, so swapping the visible metadata IS the whole
    /// CPython-observable effect here).
    overrides: Vec<(u32, *mut PyObject)>,
}

fn function_code_type() -> *mut PyType {
    static CODE_TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(ptr::null(), "code", mem::size_of::<PyFunctionCodeObject>());
        ty.tp_getattro = Some(function_code_getattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    *CODE_TYPE as *mut PyType
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
    unsafe {
        (*code_type).ob_base.ob_type = type_type;
        // The slot descriptors below are instances of the SHARED
        // `getset_descriptor` type (descr.rs) — `types.GetSetDescriptorType`
        // is derived from `type(FunctionType.__code__)` and must be identical
        // to the `type.__dict__` getsets' type.  Stamp its metatype here too
        // (idempotent with the abi.rs install path, so ordering is free).
        crate::descr::finalize_getset_descriptors(type_type);
    }

    let dict = unsafe {
        if (*function_type).tp_dict.is_null() {
            let dict = type_::new_namespace();
            (*function_type).tp_dict = dict.cast::<PyObject>();
            crate::sync::register_namespaced_type(function_type);
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
        let descriptor = crate::descr::new_function_getset_descriptor(name, function_type);
        unsafe { (&mut *dict).set(name, descriptor) };
    }
}

/// True when `object` is a function object (the shared `getset_descriptor`
/// in descr.rs validates receivers before delegating slot traffic here).
pub(crate) fn is_function_object(object: *mut PyObject) -> bool {
    if object.is_null() || !crate::tag::is_heap(object) {
        return false;
    }
    let ty = unsafe { (*object).ob_type };
    !ty.is_null() && unsafe { (*ty).name() } == "function"
}

/// Descriptor-protocol read of a function slot (`descr.__get__(f)`); the
/// receiver was validated by the caller.
pub(crate) unsafe fn getset_slot_get(function: *mut PyObject, name_id: u32) -> *mut PyObject {
    function_attr_by_id(function, name_id).unwrap_or_else(|| return_null_with_error("unknown function descriptor"))
}

/// Descriptor-protocol write/delete of a function slot (`descr.__set__(f, v)`
/// / `descr.__delete__(f)`): identical semantics to a plain attribute write,
/// so delegate to `function_setattro`.
pub(crate) unsafe fn getset_slot_set(function: *mut PyObject, name_id: u32, value: *mut PyObject) -> c_int {
    let Some(name_text) = crate::intern::resolve(name_id) else {
        pon_err_set("function attribute name is not interned");
        return -1;
    };
    let name = const_str(&name_text);
    if name.is_null() {
        pon_err_set("failed to allocate function attribute key");
        return -1;
    }
    unsafe { function_setattro(function, name, value) }
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
        overrides: Vec::new(),
    }))
    .cast::<PyObject>()
}

/// True when `object` is a function-code shell (`f.__code__`'s type).  The
/// keyword binder uses this to tell `code.replace(co_flags=...)` apart from
/// other natives named `replace` (`str.replace`).
pub(crate) fn is_function_code_object(object: *mut PyObject) -> bool {
    let object = crate::tag::untag_arg(object);
    !object.is_null()
        && crate::tag::is_heap(object)
        && unsafe { (*object).ob_type } == function_code_type().cast_const()
}

/// co_* keyword names accepted by `code.replace(**kwargs)` — the CPython
/// 3.14 signature.  Names without a derived view in
/// `function_code_getattro` (`co_linetable`, `co_exceptiontable`) are still
/// accepted and become readable through the override table, mirroring how a
/// real replace round-trips every field.
const CODE_REPLACE_KEYWORDS: &[&str] = &[
    "co_argcount",
    "co_posonlyargcount",
    "co_kwonlyargcount",
    "co_nlocals",
    "co_stacksize",
    "co_flags",
    "co_firstlineno",
    "co_code",
    "co_consts",
    "co_names",
    "co_varnames",
    "co_freevars",
    "co_cellvars",
    "co_filename",
    "co_name",
    "co_qualname",
    "co_linetable",
    "co_exceptiontable",
];

/// Validates one `code.replace` keyword value against the co_* field's
/// CPython type contract (argument-clinic shape: ints for counts/flags,
/// str for names, tuple for name/const tuples, bytes for code/tables).
fn validate_code_replace_value(name_text: &str, value: *mut PyObject) -> Result<(), String> {
    let probe = crate::tag::untag_arg(value);
    let ok = match name_text {
        "co_argcount" | "co_posonlyargcount" | "co_kwonlyargcount" | "co_nlocals" | "co_stacksize" | "co_flags"
        | "co_firstlineno" => unsafe { crate::types::int::to_bigint_including_bool(probe) }.is_some(),
        "co_filename" | "co_name" | "co_qualname" => unsafe { type_::unicode_text(probe) }.is_some(),
        "co_consts" | "co_names" | "co_varnames" | "co_freevars" | "co_cellvars" => {
            crate::abi::seq::has_tuple_storage(probe)
        }
        "co_code" | "co_linetable" | "co_exceptiontable" => unsafe { crate::types::int::type_name_is(probe, "bytes") },
        _ => return Err(format!("replace() got an unexpected keyword argument '{name_text}'")),
    };
    if ok {
        Ok(())
    } else {
        let expected = match name_text {
            "co_filename" | "co_name" | "co_qualname" => "str",
            "co_consts" | "co_names" | "co_varnames" | "co_freevars" | "co_cellvars" => "tuple",
            "co_code" | "co_linetable" | "co_exceptiontable" => "bytes",
            _ => "int",
        };
        Err(format!(
            "replace() argument '{name_text}' must be {expected}, not {}",
            unsafe { crate::types::dict::type_name(probe) }.unwrap_or("object")
        ))
    }
}

/// `code.replace(**kwargs)` — returns a NEW code shell with the named co_*
/// fields swapped (CPython semantics on the metadata surface pon serves).
/// Reached as a bound method: `argv[0]` is the receiver; keyword pairs ride
/// a trailing `lazy_iter::PyKwMarker` appended by the binder arm, and the
/// no-keyword spelling (`test.support.reset_code`'s `f.__code__.replace()`)
/// arrives as the bare receiver.  Positional arguments are rejected like
/// CPython's keyword-only signature.
unsafe extern "C" fn function_code_replace_trampoline(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() || argc == 0 {
        return return_null_with_error("code.replace() received a NULL argv");
    }
    // SAFETY: The caller passes a live argv window of length argc.
    let args = unsafe { core::slice::from_raw_parts(argv, argc) };
    let receiver = crate::tag::untag_arg(args[0]);
    if !is_function_code_object(receiver) {
        let message = "descriptor 'replace' for 'code' objects doesn't apply to another object";
        let _ = unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
        return ptr::null_mut();
    }
    let mut pairs: &[(u32, *mut PyObject)] = &[];
    let mut extra = &args[1..];
    if let Some((&last, rest)) = extra.split_last() {
        if let Some(marker_pairs) = unsafe { crate::types::lazy_iter::kw_marker_pairs(last) } {
            pairs = marker_pairs;
            extra = rest;
        }
    }
    if !extra.is_empty() {
        let message = "replace() takes no positional arguments";
        let _ = unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
        return ptr::null_mut();
    }
    let code = unsafe { &*receiver.cast::<PyFunctionCodeObject>() };
    let mut overrides = code.overrides.clone();
    for &(name, value) in pairs {
        let Some(name_text) = resolve(name) else {
            return return_null_with_error("replace() keyword name is not interned");
        };
        if let Err(message) = validate_code_replace_value(&name_text, value) {
            let _ = unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
            return ptr::null_mut();
        }
        match overrides.iter_mut().find(|(existing, _)| *existing == name) {
            Some(slot) => slot.1 = value,
            None => overrides.push((name, value)),
        }
    }
    Box::into_raw(Box::new(PyFunctionCodeObject {
        ob_base: PyObjectHeader::new(function_code_type()),
        metadata: code.metadata.clone(),
        overrides,
    }))
    .cast::<PyObject>()
}

/// Allocates the bound `code.replace` method for `function_code_getattro`
/// (the map.rs `alloc_bound_native_method` shape).
fn alloc_code_replace_method(receiver: *mut PyObject) -> *mut PyObject {
    let name = intern("replace");
    let function = unsafe {
        crate::abi::pon_make_function(
            function_code_replace_trampoline as *const u8,
            crate::builtins::variadic_arity(),
            name,
        )
    };
    if function.is_null() {
        return return_null_with_error("failed to allocate code.replace method");
    }
    match method::new_bound_method(function, receiver) {
        Ok(bound) => bound.cast::<PyObject>(),
        Err(message) => return_null_with_error(message),
    }
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
    if metadata.flags & crate::abi::call::CODE_FLAG_ASYNC_GENERATOR != 0 {
        flags |= CPY_CO_ASYNC_GENERATOR;
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
    let code_ref = unsafe { &*code.cast::<PyFunctionCodeObject>() };
    let metadata = &code_ref.metadata;
    if name_id == intern("__class__") {
        return function_code_type().cast::<PyObject>();
    }
    if name_id == intern("replace") {
        return alloc_code_replace_method(code);
    }
    // `code.replace(**kwargs)` swaps win over every derived view below, so a
    // replaced field reads back exactly the object that was installed.
    if let Some(&(_, value)) = code_ref.overrides.iter().find(|&&(id, _)| id == name_id) {
        return value;
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

/// Remove side-table metadata for a function object (Phase-B record and any
/// live defaults overrides), so a reused allocation address can never
/// resurrect stale metadata.
pub fn unregister_function_record(function: *mut PyObject) {
    if let Ok(mut records) = FUNCTION_RECORDS.lock() {
        records.remove(&(function as usize));
    }
    if let Ok(mut overrides) = FUNCTION_DEFAULT_OVERRIDES.lock() {
        overrides.remove(&(function as usize));
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
    if name_id == intern("__doc__") {
        // Lowering does not thread docstring text into function metadata yet,
        // so every function reports CPython's default.  A `__doc__` store
        // still wins: `function_getattro` consults the attr dict first.
        return Some(unsafe { crate::abi::pon_none() });
    }
    if name_id == intern("__module__") {
        let module = function_module(function)
            .or_else(crate::import::active_module_name_id)
            .unwrap_or_else(|| intern("__main__"));
        return Some(const_name(module));
    }
    if name_id == intern("__code__") {
        return Some(alloc_code_object(function));
    }
    if name_id == intern("__builtins__") {
        let builtins = crate::import::cached_module(intern("builtins"))?;
        if let Some(namespace) = crate::import::module_namespace_for_object(builtins) {
            return namespace.ok();
        }
        return Some(builtins);
    }
    if name_id == intern("__globals__") {
        if let Some(module_object) = function_module_object(function)
            && let Some(namespace) = crate::import::module_namespace_for_object(module_object)
        {
            return namespace.ok();
        }
        if let Some(module_name) = function_module(function) {
            return crate::dynexec::module_namespace_dict(module_name).ok();
        }
        return Some(unsafe { crate::dynexec::builtin_globals(ptr::null_mut(), 0) });
    }
    if name_id == intern("__defaults__") {
        if let Some(stored) = defaults_override(function).defaults {
            // Live override installed by `f.__defaults__ = ...`: reads return
            // the assigned object itself (tuple identity preserved) or `None`
            // after clearing.
            return Some(stored as *mut PyObject);
        }
        let Some(record) = function_record(function) else {
            return Some(unsafe { crate::abi::pon_none() });
        };
        if record.defaults.is_empty() {
            return Some(unsafe { crate::abi::pon_none() });
        }
        return Some(tuple_from_objects(record.defaults()));
    }
    if name_id == intern("__kwdefaults__") {
        if let Some(stored) = defaults_override(function).kwdefaults {
            return Some(stored as *mut PyObject);
        }
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
    if name_id == intern("__get__") {
        // CPython parity: builtin functions (`builtin_function_or_method`)
        // implement no descriptor protocol, so a native function has no
        // `__get__` at all (enum's `_is_descriptor` classifies them as plain
        // members).  Falling through raises AttributeError in
        // `function_getattro`.
        if is_native_function(function) {
            return None;
        }
        // Python-visible descriptor protocol: `_is_descriptor`-style probes
        // (`hasattr(f, '__get__')`, enum's member classification) must see
        // USER functions as descriptors.  Served as a bound native so
        // `f.__get__(obj)` binds exactly like implicit method lookup.
        let carrier = unsafe {
            crate::abi::pon_make_function(
                function_dunder_get_native as *const u8,
                crate::builtins::variadic_arity(),
                intern("__get__"),
            )
        };
        if carrier.is_null() {
            return Some(ptr::null_mut());
        }
        return Some(match crate::types::method::new_bound_method(carrier, function) {
            Ok(method) => method.cast::<PyObject>(),
            Err(message) => return_null_with_error(message),
        });
    }
    if name_id == intern("__dict__") {
        return Some(unsafe { ensure_function_attr_dict(function) });
    }
    None
}

/// Returns the function's instance attribute dict, allocating it on first use.
///
/// The pointer lives in the trailing `PyFunction::attr_dict` field, which the
/// GC visits through `trace_function`, so the dict (and, via `trace_dict`, the
/// stored keys/values) stays alive exactly as long as the function does.
unsafe fn ensure_function_attr_dict(function: *mut PyObject) -> *mut PyObject {
    let function_ref = function.cast::<PyFunction>();
    let existing = unsafe { (*function_ref).attr_dict };
    if !existing.is_null() {
        return existing;
    }
    let dict = unsafe { crate::abi::map::pon_build_map(ptr::null_mut(), 0) };
    if !dict.is_null() {
        unsafe {
            (*function_ref).attr_dict = dict;
        }
    }
    dict
}

/// `f.__defaults__ = value` / `del f.__defaults__` (CPython
/// `func_set_defaults`): only a tuple — subclasses included, matching
/// `PyTuple_Check` — or `None` is accepted; `None` and deletion clear the
/// defaults entirely.  The stored object immediately drives both attribute
/// reads and call-time binding.
unsafe fn store_defaults_override(function: *mut PyObject, value: *mut PyObject) -> c_int {
    let none = unsafe { crate::abi::pon_none() };
    let stored = if value.is_null() || value == none {
        none
    } else if crate::abi::seq::has_tuple_storage(value) {
        value
    } else {
        let message = "__defaults__ must be set to a tuple object";
        let _ = unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
        return -1;
    };
    if let Ok(mut table) = FUNCTION_DEFAULT_OVERRIDES.lock() {
        table.entry(function as usize).or_default().defaults = Some(stored as usize);
    }
    0
}

/// `f.__kwdefaults__ = value` / deletion (CPython `func_set_kwdefaults`):
/// only a dict — subclasses included, matching `PyDict_Check` — or `None`
/// is accepted; `None` and deletion clear the keyword-only defaults
/// entirely.
unsafe fn store_kwdefaults_override(function: *mut PyObject, value: *mut PyObject) -> c_int {
    let none = unsafe { crate::abi::pon_none() };
    let stored = if value.is_null() || value == none {
        none
    } else if unsafe { dict::has_dict_storage(value) } {
        value
    } else {
        let message = "__kwdefaults__ must be set to a dict object";
        let _ = unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
        return -1;
    };
    if let Ok(mut table) = FUNCTION_DEFAULT_OVERRIDES.lock() {
        table.entry(function as usize).or_default().kwdefaults = Some(stored as usize);
    }
    0
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
    // Instance attributes stored by `function_setattro` win for plain names,
    // matching CPython where the function `__dict__` backs arbitrary
    // attributes.  `__dict__` itself stays a pseudo-getset served below and is
    // never looked up inside the dict.
    if name_id != intern("__dict__") && name_id != intern("__defaults__") && name_id != intern("__kwdefaults__") {
        let dict = unsafe { (*function.cast::<PyFunction>()).attr_dict };
        if !dict.is_null() {
            let key = const_str(name_text);
            if key.is_null() {
                return return_null_with_error("failed to allocate function attribute key");
            }
            match unsafe { dict::dict_get(dict, key) } {
                Ok(Some(value)) => return value,
                Ok(None) => {}
                Err(message) => return return_null_with_error(message),
            }
        }
    }
    if let Some(value) = function_attr_by_id(function, name_id) {
        return value;
    }
    let _ = unsafe { crate::abi::pon_raise_attribute_error(function, name_id) };
    ptr::null_mut()
}

/// Attribute assignment/deletion for function objects (`tp_setattro`).
///
/// Every plain name lands in the per-function attribute dict — CPython's
/// function `__dict__` — which `function_getattro` consults first, so special
/// writable metadata (`__doc__`, `__name__`, `__qualname__`, `__module__`,
/// `__wrapped__`, `__isabstractmethod__`, ...) shares that storage and
/// assign-then-read matches CPython without a dedicated slot per name.
/// Assigning `__dict__` replaces the whole dict and requires a dict object;
/// deleting it is rejected like CPython's function `__dict__` getset.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn function_setattro(function: *mut PyObject, name: *mut PyObject, value: *mut PyObject) -> c_int {
    if function.is_null() || name.is_null() {
        pon_err_set("function attribute assignment received NULL");
        return -1;
    }
    let Some(name_text) = (unsafe { (&*name.cast::<PyUnicode>()).as_str() }) else {
        pon_err_set("function attribute name is not valid UTF-8");
        return -1;
    };
    if name_text == "__code__" {
        // CPython `func_set_code`: assignment (and deletion) requires a code
        // object; the accepted value lands in the attr dict below, which
        // `function_getattro` consults before the derived `__code__` view, so
        // `f.__code__ = f.__code__.replace(...)` (types.coroutine, reached at
        // test.support import time) round-trips the stored object.  All three
        // runtime code shells spell their type name `code`.
        if value.is_null() || !unsafe { crate::types::int::type_name_is(crate::tag::untag_arg(value), "code") } {
            let message = "__code__ must be set to a code object";
            let _ = unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
            return -1;
        }
    }
    if name_text == "__defaults__" {
        return unsafe { store_defaults_override(function, value) };
    }
    if name_text == "__kwdefaults__" {
        return unsafe { store_kwdefaults_override(function, value) };
    }
    if name_text == "__dict__" {
        if value.is_null() {
            pon_err_set("function's dictionary may not be deleted");
            return -1;
        }
        if !unsafe { dict::is_dict(value) } {
            pon_err_set("__dict__ must be set to a dictionary");
            return -1;
        }
        unsafe {
            (*function.cast::<PyFunction>()).attr_dict = value;
        }
        return 0;
    }
    if value.is_null() {
        let dict = unsafe { (*function.cast::<PyFunction>()).attr_dict };
        if !dict.is_null() {
            let key = const_str(name_text);
            if key.is_null() {
                pon_err_set("failed to allocate function attribute key");
                return -1;
            }
            match unsafe { dict::dict_remove(dict, key) } {
                Ok(Some(_)) => return 0,
                Ok(None) => {}
                Err(message) => {
                    pon_err_set(message);
                    return -1;
                }
            }
        }
        let _ = unsafe { crate::abi::pon_raise_attribute_error(function, intern(name_text)) };
        return -1;
    }
    let dict = unsafe { ensure_function_attr_dict(function) };
    if dict.is_null() {
        pon_err_set("failed to allocate function attribute dict");
        return -1;
    }
    let key = const_str(name_text);
    if key.is_null() {
        pon_err_set("failed to allocate function attribute key");
        return -1;
    }
    match unsafe { dict::dict_insert(dict, key, value) } {
        Ok(()) => 0,
        Err(message) => {
            pon_err_set(message);
            -1
        }
    }
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

/// Defining-module side table: function object address -> original module
/// namespace context.
///
/// `name` backs Python-visible `function.__module__`. `module_object` keeps
/// `function.__globals__` / `LOAD_GLOBAL` tied to the module object that
/// created the function, even if user code later rebinds `sys.modules[name]`
/// to a compatibility wrapper.  Raw object addresses are stored as integers so
/// the global mutex remains `Send`; module boxes are process-lifetime import
/// objects, and the GC dealloc hook clears function entries before an address
/// can be reused.
#[derive(Clone, Copy, Debug)]
struct FunctionModuleRecord {
    name: u32,
    module_object: usize,
}

static FUNCTION_MODULES: LazyLock<Mutex<HashMap<usize, FunctionModuleRecord>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Record `module` (interned name) as `function`'s defining module.
pub fn set_function_module(function: *mut PyObject, module: u32) {
    set_function_module_context(function, module, ptr::null_mut());
}

/// Record the defining module name and original module object for `function`.
pub fn set_function_module_context(function: *mut PyObject, module: u32, module_object: *mut PyObject) {
    if function.is_null() {
        return;
    }
    if let Ok(mut table) = FUNCTION_MODULES.lock() {
        table.insert(
            function as usize,
            FunctionModuleRecord {
                name: module,
                module_object: module_object as usize,
            },
        );
    }
}

/// Return the defining-module context recorded for `function`, if any.
#[must_use]
pub fn function_module_context(function: *mut PyObject) -> Option<(u32, *mut PyObject)> {
    if function.is_null() {
        return None;
    }
    FUNCTION_MODULES.lock().ok().and_then(|table| {
        table
            .get(&(function as usize))
            .map(|record| (record.name, record.module_object as *mut PyObject))
    })
}

/// Return the interned defining-module name recorded for `function`, if any.
#[must_use]
pub fn function_module(function: *mut PyObject) -> Option<u32> {
    function_module_context(function).map(|(module, _)| module)
}

/// Return the original defining-module object recorded for `function`, if any.
#[must_use]
pub fn function_module_object(function: *mut PyObject) -> Option<*mut PyObject> {
    function_module_context(function)
        .map(|(_, module_object)| module_object)
        .filter(|module_object| !module_object.is_null())
}

/// Drop the defining-module record for a freed `function` allocation.
pub fn clear_function_module(function: *mut PyObject) {
    if let Ok(mut table) = FUNCTION_MODULES.lock() {
        table.remove(&(function as usize));
    }
}

/// Native (builtin) function side table: addresses of `PyFunction` objects
/// that model CPython `builtin_function_or_method` values — module-level
/// native functions (`time.localtime`, `math.log`) and the flat-pool
/// builtins (`len`, `print`), recorded when a native module's attrs are
/// installed (`import::create_module`).  CPython builtins are NOT
/// descriptors: stored as a class attribute and read off an instance they
/// come back bare (no bound `self`), and they expose no `__get__` — the
/// `self.converter(...)` pattern in `logging.Formatter.formatTime` depends
/// on exactly this.  Entries are raw unrooted addresses, the same accepted
/// pattern as [`FUNCTION_RECORDS`]/[`FUNCTION_MODULES`]; the GC dealloc
/// hook clears entries so a reused allocation address can never resurrect
/// a stale native marking.
static NATIVE_FUNCTIONS: LazyLock<Mutex<HashSet<usize>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Record `function` as a native (builtin) function — a non-descriptor.
/// Values that are not function objects are ignored, so module-attr walks
/// can pass every installed value unfiltered.
pub(crate) fn mark_native_function(function: *mut PyObject) {
    if !is_function_object(function) {
        return;
    }
    if let Ok(mut table) = NATIVE_FUNCTIONS.lock() {
        table.insert(function as usize);
    }
}

/// True when `function` was recorded as a native (builtin) function.
#[must_use]
pub(crate) fn is_native_function(function: *mut PyObject) -> bool {
    if function.is_null() {
        return false;
    }
    NATIVE_FUNCTIONS
        .lock()
        .map(|table| table.contains(&(function as usize)))
        .unwrap_or(false)
}

/// Drop the native marking for a freed `function` allocation.
pub fn clear_native_function(function: *mut PyObject) {
    if let Ok(mut table) = NATIVE_FUNCTIONS.lock() {
        table.remove(&(function as usize));
    }
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
///
/// Native (builtin) functions are exempt: CPython's
/// `builtin_function_or_method` has a NULL `tp_descr_get`, so
/// `class A: conv = time.localtime; A().conv` reads back the BARE function
/// with no `self` prepended (`logging.Formatter.formatTime`'s
/// `self.converter(record.created)` depends on this).  User functions keep
/// full descriptor binding.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn function_descr_get(descr: *mut PyObject, obj: *mut PyObject, _owner: *mut PyObject) -> *mut PyObject {
    if descr.is_null() || obj.is_null() || is_native_function(descr) {
        return descr;
    }
    match crate::types::method::new_bound_method(descr, obj) {
        Ok(method) => method.cast::<PyObject>(),
        Err(_) => descr,
    }
}

/// `function.__get__(obj, owner=None)` — the Python-visible spelling of
/// [`function_descr_get`]: `argv[0]` is the function (bound receiver of the
/// `__get__` carrier), `argv[1]` the instance, optional `argv[2]` the owner
/// class.  A `None` instance with an owner returns the function unbound
/// (CPython `func_descr_get` parity).
unsafe extern "C" fn function_dunder_get_native(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() || argc < 2 {
        return return_null_with_error("expected at least 1 argument, got 0");
    }
    let function = unsafe { *argv };
    let obj = unsafe { *argv.add(1) };
    let obj_is_none = obj.is_null() || obj == unsafe { crate::abi::pon_none() };
    if obj_is_none {
        if argc >= 3 {
            return function;
        }
        return return_null_with_error("__get__(None, None) is invalid");
    }
    match crate::types::method::new_bound_method(function, obj) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => return_null_with_error(message),
    }
}

pub(crate) unsafe fn positional_args_from_star(object: *mut PyObject) -> Result<Vec<*mut PyObject>, String> {
    match unsafe { dict::type_name(object) } {
        Some("tuple") => Ok(unsafe { (&*object.cast::<PyTuple>()).as_slice() }.to_vec()),
        Some("list") => Ok(unsafe { (&*object.cast::<PyList>()).as_slice() }.to_vec()),
        Some(name) => Err(raise_boxed_type_error(format!("argument after * must be an iterable, not {name}"))),
        None => Err("argument after * is invalid".to_owned()),
    }
}

pub(crate) unsafe fn extend_keywords_from_mapping(function: *mut PyObject,
mapping: *mut PyObject,
names: &mut Vec<u32>,
values: &mut Vec<*mut PyObject>,) -> Result<(), String> { if unsafe { dict::type_name(mapping) } != Some("dict") {
    let type_name = unsafe { dict::type_name(mapping) }.unwrap_or("object");
    return Err(raise_boxed_type_error(format!("argument after ** must be a mapping, not {type_name}")));
}
for entry in unsafe { dict::dict_entries_snapshot(mapping)? } {
    if unsafe { dict::type_name(entry.key) } != Some("str") {
        return Err(raise_boxed_type_error("keywords must be strings".to_owned()));
    }
    let Some(name_text) = (unsafe { (&*entry.key.cast::<PyUnicode>()).as_str() }) else {
        return Err("keyword name is not valid UTF-8".to_owned());
    };
    let name = intern::intern(name_text);
    if names.contains(&name) {
        return Err(raise_boxed_type_error(format!(
            "{} got multiple values for keyword argument '{}'",
            function_call_name(function),
            name_text
        )));
    }
    names.push(name);
    values.push(entry.value);
}
Ok(()) }

fn function_call_name(function: *mut PyObject) -> String {
    let name = function_name(function).unwrap_or_else(|| "function".to_owned());
    format!("__main__.{name}()")
}

fn function_short_call_name(function: *mut PyObject) -> String {
    let name = function_name(function).unwrap_or_else(|| "function".to_owned());
    format!("{name}()")
}

fn format_too_many_positional_arguments(function: *mut PyObject, expected: usize, got: usize) -> String {
    let argument = if expected == 1 { "argument" } else { "arguments" };
    let given = if got == 1 { "was" } else { "were" };
    format!(
        "{} takes {expected} positional {argument} but {got} {given} given",
        function_short_call_name(function)
    )
}

fn quoted_argument_names(names: &[u32]) -> String {
    let quoted = names
        .iter()
        .map(|name| format!("'{}'", keyword_name(*name)))
        .collect::<Vec<_>>();
    match quoted.as_slice() {
        [] => String::new(),
        [only] => only.clone(),
        [first, second] => format!("{first} and {second}"),
        _ => {
            let (head, tail) = quoted.split_at(quoted.len() - 1);
            format!("{}, and {}", head.join(", "), tail[0])
        }
    }
}

fn format_missing_required_arguments(function: *mut PyObject, names: &[u32], kind: &str) -> String {
    let count = names.len();
    let argument = if count == 1 { "argument" } else { "arguments" };
    format!(
        "{} missing {count} required {kind} {argument}: {}",
        function_short_call_name(function),
        quoted_argument_names(names)
    )
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

/// Values of the live `__defaults__` override as object addresses, or `None`
/// while creation-time defaults stay authoritative (never assigned).  A
/// cleared override (`= None` / deletion) yields an empty vector.
fn defaults_override_values(function: *mut PyObject) -> Option<Vec<usize>> {
    let stored = defaults_override(function).defaults? as *mut PyObject;
    if stored == unsafe { crate::abi::pon_none() } {
        return Some(Vec::new());
    }
    // The store path validated tuple storage (exact tuple or tuple-subclass
    // instance), so the layout-safe view is always available; treat an
    // impossible mismatch as cleared rather than reading a wrong layout.
    let Some(values) = (unsafe { crate::abi::seq::tuple_storage_slice(stored) }) else {
        debug_assert!(false, "__defaults__ override stored without tuple storage");
        return Some(Vec::new());
    };
    Some(values.iter().map(|value| *value as usize).collect())
}

/// Keyword-only defaults from the live `__kwdefaults__` override, keyed by
/// interned parameter name, or `None` while the creation-time record stays
/// authoritative.
fn kwdefaults_override_map(function: *mut PyObject) -> Result<Option<BTreeMap<u32, usize>>, String> {
    let Some(stored) = defaults_override(function).kwdefaults else {
        return Ok(None);
    };
    let stored = stored as *mut PyObject;
    if stored == unsafe { crate::abi::pon_none() } {
        return Ok(Some(BTreeMap::new()));
    }
    let mut map = BTreeMap::new();
    for entry in unsafe { dict::dict_entries_snapshot(stored)? } {
        if unsafe { dict::type_name(entry.key) } != Some("str") {
            return Err("__kwdefaults__ keys must be strings".to_owned());
        }
        let Some(name_text) = (unsafe { (&*entry.key.cast::<PyUnicode>()).as_str() }) else {
            return Err("__kwdefaults__ key is not valid UTF-8".to_owned());
        };
        map.insert(intern::intern(name_text), entry.value as usize);
    }
    Ok(Some(map))
}

/// Fills trailing positional slots of an arity-only (Phase-A) call from the
/// live `__defaults__` override, using CPython tail alignment (an over-long
/// tuple leaves its head unused).  Returns `None` when no override is
/// installed, the call is not short, or required parameters are still
/// missing, so callers keep their original arity diagnostics.
#[must_use]
pub fn fill_positional_defaults(
    function: *mut PyObject,
    positional: &[*mut PyObject],
    arity: usize,
) -> Option<Vec<*mut PyObject>> {
    if positional.len() >= arity {
        return None;
    }
    let defaults = defaults_override_values(function)?;
    let default_start = arity as isize - defaults.len() as isize;
    if (positional.len() as isize) < default_start {
        return None;
    }
    let mut filled = Vec::with_capacity(arity);
    filled.extend_from_slice(positional);
    for index in positional.len()..arity {
        filled.push(defaults[(index as isize - default_start) as usize] as *mut PyObject);
    }
    Some(filled)
}

/// Module-level `@staticmethod` functions (`_pyio.open`, implicit `__new__`
/// carriers, etc.) can reach the call binder before the descriptor `tp_call`
/// peels them.  Treat the carrier as its wrapped callable so keyword binding
/// and direct call dispatch see the real function object.
fn unwrap_staticmethod_callable(function: *mut PyObject) -> *mut PyObject {
    let function = crate::tag::untag_arg(function);
    if function.is_null() || !crate::tag::is_heap(function) {
        return function;
    }
    if function_record(function).is_some() {
        return function;
    }
    let ty = unsafe { (*function).ob_type.cast_mut() };
    if ty != crate::abi::staticmethod_builtin_type() {
        return function;
    }
    // SAFETY: Exact builtin `staticmethod` carrier layout above.
    let callable = unsafe { (*function.cast::<classmethod::PyStaticMethod>()).callable };
    if callable.is_null() { function } else { callable }
}
/// Bind a call into the compiled function's argv/local-slot order.
pub fn bind_arguments(
    function: *mut PyObject,
    positional: &[*mut PyObject],
    keywords: KeywordArgs<'_>,
    star: Option<*mut PyObject>,
    dstar: Option<*mut PyObject>,
) -> Result<Vec<*mut PyObject>, String> {
    let function = unwrap_staticmethod_callable(function);
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
            return Err(raise_boxed_type_error(format_too_many_positional_arguments(
                function,
                positional_arity,
                positional.len(),
            )));
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
            return Err(raise_boxed_type_error(format!(
                "{} got an unexpected keyword argument '{}'",
                function_short_call_name(function),
                keyword_name(name)
            )));
        };
        if index < params.positional_only_count {
            return Err(raise_boxed_type_error(format!(
                "{} got some positional-only arguments passed as keyword arguments: '{}'",
                function_short_call_name(function),
                keyword_name(name)
            )));
        }
        if !bound[index].is_null() {
            return Err(raise_boxed_type_error(format!(
                "{} got multiple values for argument '{}'",
                function_short_call_name(function),
                keyword_name(name)
            )));
        }
        bound[index] = value;
    }
    if let Some(slot) = params.varkw_slot() {
        bound[slot] = unsafe { build_kwargs_dict(&varkw_pairs) }?;
    }

    // A live `__defaults__` override REPLACES creation-time defaults
    // entirely (CPython: assignment swaps the whole tuple; `None` clears).
    let defaults_override = defaults_override_values(function);
    let defaults: &[usize] = defaults_override.as_deref().unwrap_or(&record.defaults);
    // CPython tail alignment: defaults cover the LAST `defaults.len()`
    // positional parameters; an over-long live tuple leaves its head unused.
    let default_start = positional_arity as isize - defaults.len() as isize;
    let mut missing_positional = Vec::new();
    for index in 0..positional_arity {
        if bound[index].is_null() {
            let default_index = index as isize - default_start;
            if default_index >= 0 {
                if let Some(default) = defaults.get(default_index as usize) {
                    bound[index] = *default as *mut PyObject;
                }
            }
            if bound[index].is_null() {
                missing_positional.push(params.names.get(index).copied().unwrap_or(0));
            }
        }
    }
    if !missing_positional.is_empty() {
        return Err(raise_boxed_type_error(format_missing_required_arguments(
            function,
            &missing_positional,
            "positional",
        )));
    }

    let kwdefaults_override = kwdefaults_override_map(function)?;
    let kwdefaults: &BTreeMap<u32, usize> = kwdefaults_override.as_ref().unwrap_or(&record.kwdefaults);
    let keyword_start = positional_arity;
    let keyword_end = keyword_start + params.keyword_only_count;
    let mut missing_keyword_only = Vec::new();
    for index in keyword_start..keyword_end {
        if bound[index].is_null() {
            let name = params.names.get(index).copied().unwrap_or(0);
            if let Some(default) = kwdefaults.get(&name) {
                bound[index] = *default as *mut PyObject;
            } else {
                missing_keyword_only.push(name);
            }
        }
    }
    if !missing_keyword_only.is_empty() {
        return Err(raise_boxed_type_error(format_missing_required_arguments(
            function,
            &missing_keyword_only,
            "keyword-only",
        )));
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
    let function = unwrap_staticmethod_callable(function);
    let mut argv = bind_arguments(function, positional, keywords, star, dstar)?;
    // Generator/coroutine functions need no special casing here: the compiled
    // stub at the function's entry allocates the frame and returns the
    // generator object itself (pin J0.1 §4.0).
    //
    // Tier-up parity with the record-less code-pointer path in `pon_call`:
    // bump the call-hotness probe, then dispatch through the live `entry`
    // cell so an installed tier-1 body is actually entered.  The record's
    // `entry` is a creation-time tier-0 snapshot and must never pin dispatch
    // to tier-0 (both tiers share the bound `(argv, argc)` ABI).
    let function = function.cast::<PyFunction>();
    // SAFETY: `bind_arguments` only succeeds for live function objects.
    unsafe { crate::abi::pon_tierup_bump_call(function) };
    // SAFETY: See above; `entry` is initialized to the tier-0 code pointer at
    // allocation and only ever replaced by the tier-up install protocol.
    let code = unsafe { (*function).entry.load(Ordering::Acquire) }.cast_const();
    if code.is_null() {
        return Err("function code pointer is null".to_owned());
    }
    let _guard = crate::abi::push_current_call(function.cast::<PyFunction>(), argv.as_mut_ptr(), argv.len());
    let _handled_guard = crate::abi::HandledExcGuard::enter_clearing_pending();
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
    // Only real function objects carry the Phase-A `arity` field; anything
    // else (bound methods, descriptor carriers) reaching this binder is a
    // dispatch bug upstream — fail with the type name instead of reading
    // garbage through the wrong layout.
    let ob_type = unsafe { (*function).ob_type };
    if ob_type.is_null() || unsafe { (*ob_type).name() } != "function" {
        let type_name = if ob_type.is_null() { "<missing type>" } else { unsafe { (*ob_type).name() } };
        return Err(format!("cannot bind arguments for '{type_name}' object: expected a function"));
    }
    let arity = unsafe { (*function.cast::<PyFunction>()).arity };
    if arity != crate::builtins::variadic_arity() && positional.len() != arity {
        // A live `__defaults__` override can still satisfy a short call.
        if let Some(filled) = fill_positional_defaults(function, positional, arity) {
            for (index, value) in filled.iter().enumerate() {
                if value.is_null() {
                    return Err(format!("positional argument {index} is NULL"));
                }
            }
            return Ok(filled);
        }
        return Err(raise_boxed_type_error(format!("function expected {arity} arguments, got {}", positional.len())));
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
    // Only real function objects carry the name/arity layout read below;
    // anything else reaching this binder is a dispatch bug upstream — fail
    // with the type name instead of reading garbage through the wrong layout.
    if function.is_null() {
        return Err("callee is NULL".to_owned());
    }
    let ob_type = unsafe { (*function).ob_type };
    if ob_type.is_null() || unsafe { (*ob_type).name() } != "function" {
        let type_name = if ob_type.is_null() { "<missing type>" } else { unsafe { (*ob_type).name() } };
        return Err(format!("cannot bind keyword arguments for '{type_name}' object: expected a function"));
    }
    let Some(name) = function_name(function) else {
        return Err("keyword arguments require Phase-B function metadata".to_owned());
    };
    bind_native_keywords_for_name(&name, positional, keywords)
}

pub(crate) fn bind_native_keywords_for_name(
    name: &str,
    positional: &[*mut PyObject],
    keywords: KeywordArgs<'_>,
) -> Result<Vec<*mut PyObject>, String> {
    match name {
        "sorted" => bind_sorted_keywords(positional, keywords),
        // `list.sort(key=…, reverse=…)`: the bound receiver is the sole
        // positional, so the `sorted` shape ([receiver, sort_options]) fits.
        "sort" => bind_sorted_keywords(positional, keywords),
        "sum" => bind_single_keyword(positional, keywords, "sum", "start", 1, 2),
        "round" => bind_named_positional_keywords(positional, keywords, "round", &["number", "ndigits"], 1, 2),
        "pow" => bind_named_positional_keywords(positional, keywords, "pow", &["base", "exp", "mod"], 2, 3),
        "min" => bind_minmax_keywords(positional, keywords, "min"),
        "max" => bind_minmax_keywords(positional, keywords, "max"),
        "zip" => bind_zip_keywords(positional, keywords),
        "enumerate" => bind_single_keyword(positional, keywords, "enumerate", "start", 1, 2),
        // `dict(*args, **kwargs)`: arbitrary keyword names become entries;
        // the raw pairs ride a trailing marker that `builtin_dict` merges
        // after the positional mapping/iterable (argparse's
        // `dict(kwargs, dest=..., option_strings=...)` shape).
        "dict" => bind_any_keywords(positional, keywords, "dict"),
        // `type.__prepare__(*args, **kwds)` ignores everything it receives,
        // so keyword binding degenerates to dropping the keywords.
        "__prepare__" => Ok(positional.to_vec()),
        // `type(name, bases, ns, **kwds)`: arbitrary class keywords ride to
        // the metaclass constructor in a trailing marker (`metaclass`, PEP
        // 487 `__init_subclass__` keywords, enum's `boundary`/`_simple`).
        "type" => bind_any_keywords(positional, keywords, "type"),
        // `str.format(*args, **kwargs)`: arbitrary keyword names are template
        // fields, riding in a trailing marker that `str_format_method` peels
        // into the named-field mapping (base64.py renders its `__doc__`
        // templates with keyword fields at import time).
        "format" => bind_any_keywords(positional, keywords, "format"),
        // `bytes.translate(table, /, delete=b'')`: `delete` rides in a
        // trailing marker that `bytes_translate_method` peels, preserving the
        // absent-vs-explicit-None distinction (base64.b16decode passes
        // `delete=` with a None table).
        "translate" => bind_trailing_marker_keywords(positional, keywords, "translate", &["delete"]),
        // `code.replace(*, co_flags=..., co_filename=..., ...)`: the CPython
        // 3.14 keyword-only signature, dispatched on the bound receiver so
        // other natives named `replace` (`str.replace`) keep their current
        // no-keyword path.  Keywords ride a trailing marker that
        // `function_code_replace_trampoline` peels.
        "replace" if positional.first().copied().is_some_and(is_function_code_object) => {
            bind_trailing_marker_keywords(positional, keywords, "replace", CODE_REPLACE_KEYWORDS)
        }
        // `print(*objects, sep=' ', end='\n', file=None, flush=False)`: the
        // keyword-only quartet rides a trailing marker that `builtin_print`
        // peels (test.support.captured_output passes `file=`).
        "print" => bind_trailing_marker_keywords(positional, keywords, "print", &["sep", "end", "file", "flush"]),
        // `__import__(name, globals=None, locals=None, fromlist=(), level=0)`:
        // the vendored `encodings` package search function calls it with
        // `fromlist=`/`level=` keywords; absent optionals arrive as None and
        // `builtin_dunder_import` treats None as the CPython default.
        "__import__" => bind_optional_named_keywords(
            positional,
            keywords,
            "__import__",
            &["name", "globals", "locals", "fromlist", "level"],
            5,
        ),
        // Native `_colorize` keyword-only signatures (`traceback`,
        // `unittest.runner`): absent optionals arrive as None/absent-falsy.
        "can_colorize" => bind_optional_named_keywords(positional, keywords, "can_colorize", &["file"], 0),
        "get_theme" => bind_optional_named_keywords(
            positional,
            keywords,
            "get_theme",
            &["tty_file", "force_color", "force_no_color"],
            0,
        ),
        // `get_colors(colorize=False, *, file=None)`: `doctest` calls it
        // bare and with `file=` when summarizing runs.
        "get_colors" => {
            bind_optional_named_keywords(positional, keywords, "get_colors", &["colorize", "file"], 1)
        }
        // Native `itertools` constructors (J0.4 lazy module): fixed-shape
        // signatures flatten keywords into their positional slots with None
        // filling absent optionals; the variadic constructors carry keywords
        // in a trailing `lazy_iter::PyKwMarker`.
        "count" => bind_optional_named_keywords(positional, keywords, "count", &["start", "step"], 2),
        "repeat" => bind_optional_named_keywords(positional, keywords, "repeat", &["object", "times"], 2),
        "accumulate" => {
            bind_optional_named_keywords(positional, keywords, "accumulate", &["iterable", "func", "initial"], 2)
        }
        "groupby" => bind_optional_named_keywords(positional, keywords, "groupby", &["iterable", "key"], 2),
        "permutations" => {
            bind_optional_named_keywords(positional, keywords, "permutations", &["iterable", "r"], 2)
        }
        "combinations" => {
            bind_optional_named_keywords(positional, keywords, "combinations", &["iterable", "r"], 2)
        }
        "batched" => bind_optional_named_keywords(positional, keywords, "batched", &["iterable", "n", "strict"], 2),
        "zip_longest" => bind_trailing_marker_keywords(positional, keywords, "zip_longest", &["fillvalue"]),
        "product" => bind_trailing_marker_keywords(positional, keywords, "product", &["repeat"]),
        "complex" => bind_named_positional_keywords(positional, keywords, "complex", &["real", "imag"], 0, 2),
        // Native `_struct.unpack_from(format, buffer, offset=0)`; the bound
        // `Struct.unpack_from(buffer, offset=0)` shape fits because the
        // receiver occupies the first slot. Absent optionals arrive as None.
        "unpack_from" => {
            bind_optional_named_keywords(positional, keywords, "unpack_from", &["format", "buffer", "offset"], 3)
        }
        // `compile(source, filename, mode, flags=0, dont_inherit=False,
        // optimize=-1, *, _feature_version=-1)`: `ast.parse` passes
        // `optimize`/`_feature_version` as keywords; absent slots arrive as
        // NULL and the dynexec entry defaults them.
        "compile" => bind_optional_named_keywords(
            positional,
            keywords,
            "compile",
            &["source", "filename", "mode", "flags", "dont_inherit", "optimize", "_feature_version"],
            6,
        ),
        "property" => {
            bind_optional_named_keywords(positional, keywords, "property", &["fget", "fset", "fdel", "doc"], 4)
        }
        // Native hashlib constructors: data is the only positional payload;
        // `usedforsecurity` is accepted and ignored like pon's existing `_sha2`.
        "md5" => bind_optional_named_keywords(positional, keywords, "md5", &["data", "usedforsecurity"], 1),
        "sha1" => bind_optional_named_keywords(positional, keywords, "sha1", &["data", "usedforsecurity"], 1),
        "sha224" => bind_optional_named_keywords(positional, keywords, "sha224", &["data", "usedforsecurity"], 1),
        "sha256" => bind_optional_named_keywords(positional, keywords, "sha256", &["data", "usedforsecurity"], 1),
        "sha384" => bind_optional_named_keywords(positional, keywords, "sha384", &["data", "usedforsecurity"], 1),
        "sha512" => bind_optional_named_keywords(positional, keywords, "sha512", &["data", "usedforsecurity"], 1),
        "sha3_224" => bind_optional_named_keywords(positional, keywords, "sha3_224", &["data", "usedforsecurity"], 1),
        "sha3_256" => bind_optional_named_keywords(positional, keywords, "sha3_256", &["data", "usedforsecurity"], 1),
        "sha3_384" => bind_optional_named_keywords(positional, keywords, "sha3_384", &["data", "usedforsecurity"], 1),
        "sha3_512" => bind_optional_named_keywords(positional, keywords, "sha3_512", &["data", "usedforsecurity"], 1),
        "shake_128" => bind_optional_named_keywords(positional, keywords, "shake_128", &["data", "usedforsecurity"], 1),
        "shake_256" => bind_optional_named_keywords(positional, keywords, "shake_256", &["data", "usedforsecurity"], 1),
        // Native `_blake2` constructors: CPython makes every parameter after
        // the initial data keyword-only; unsupported tree-mode knobs are
        // validated by the native entry so they fail loudly, never silently.
        "blake2b" => bind_optional_named_keywords(
            positional,
            keywords,
            "blake2b",
            &[
                "data",
                "digest_size",
                "key",
                "salt",
                "person",
                "fanout",
                "depth",
                "leaf_size",
                "node_offset",
                "node_depth",
                "inner_size",
                "last_node",
                "usedforsecurity",
            ],
            1,
        ),
        "blake2s" => bind_optional_named_keywords(
            positional,
            keywords,
            "blake2s",
            &[
                "data",
                "digest_size",
                "key",
                "salt",
                "person",
                "fanout",
                "depth",
                "leaf_size",
                "node_offset",
                "node_depth",
                "inner_size",
                "last_node",
                "usedforsecurity",
            ],
            1,
        ),
        // Native `binascii` keyword signatures (email/base64/quopri chain).
        // Fixed shapes flatten keywords into positional slots; absent
        // optionals arrive as None and the entries apply their defaults.
        "a2b_base64" => {
            bind_optional_named_keywords(positional, keywords, "a2b_base64", &["data", "strict_mode"], 1)
        }
        "b2a_base64" => {
            bind_optional_named_keywords(positional, keywords, "b2a_base64", &["data", "newline"], 1)
        }
        "a2b_qp" => bind_optional_named_keywords(positional, keywords, "a2b_qp", &["data", "header"], 2),
        "b2a_qp" => bind_optional_named_keywords(
            positional,
            keywords,
            "b2a_qp",
            &["data", "quotetabs", "istext", "header"],
            4,
        ),
        "b2a_uu" => bind_optional_named_keywords(positional, keywords, "b2a_uu", &["data", "backtick"], 1),
        "b2a_hex" => {
            bind_optional_named_keywords(positional, keywords, "b2a_hex", &["data", "sep", "bytes_per_sep"], 3)
        }
        "hexlify" => {
            bind_optional_named_keywords(positional, keywords, "hexlify", &["data", "sep", "bytes_per_sep"], 3)
        }
        // Native `math` keyword-only parameters (statistics/random chain).
        // Fixed shapes flatten keywords into positional slots; absent
        // optionals arrive as None and the entries apply their defaults.
        "isclose" => {
            bind_optional_named_keywords(positional, keywords, "isclose", &["a", "b", "rel_tol", "abs_tol"], 2)
        }
        "nextafter" => bind_optional_named_keywords(positional, keywords, "nextafter", &["x", "y", "steps"], 2),
        "prod" => bind_optional_named_keywords(positional, keywords, "prod", &["iterable", "start"], 1),
        // Native `os.lstat(path, *, dir_fd=None)`: `glob._lexists` always
        // forwards `dir_fd=` as a keyword; None (the flattened absent slot)
        // selects the plain non-fd syscall and non-None values raise the
        // honest NotImplementedError in the entry.
        "lstat" => bind_optional_named_keywords(positional, keywords, "lstat", &["path", "dir_fd"], 1),
        // Native `_thread.start_joinable_thread(function, handle=None,
        // daemon=True)`: `threading.Thread.start` passes `handle`/`daemon`
        // as keywords; absent optionals arrive as None and the entry
        // defaults them.
        "start_joinable_thread" => bind_optional_named_keywords(
            positional,
            keywords,
            "start_joinable_thread",
            &["function", "handle", "daemon"],
            3,
        ),
        // `int.from_bytes(bytes, byteorder='big', *, signed=False)` served by
        // the synthetic type attribute (`descr::synthetic_type_attr`).
        "from_bytes" => {
            bind_optional_named_keywords(positional, keywords, "from_bytes", &["bytes", "byteorder", "signed"], 2)
        }
        // `int.to_bytes(length=1, byteorder='big', *, signed=False)`: the
        // bound receiver occupies the first slot (`unpack_from` precedent);
        // absent optionals arrive as None and the entry applies the defaults.
        "to_bytes" => {
            bind_optional_named_keywords(positional, keywords, "to_bytes", &["self", "length", "byteorder", "signed"], 3)
        }
        // `str.split(sep=None, maxsplit=-1)` / `str.rsplit` (bytes/bytearray
        // share the row: same signature, dispatch is by function name): the
        // bound receiver occupies the first slot (`to_bytes` precedent) and
        // absent optionals arrive as None (`str_split_args` maps None to the
        // whitespace-sep / unlimited-maxsplit defaults).  `ipaddress.py`
        // module exec runs `ip_str.split(':', maxsplit=_max_parts)`.
        "split" => bind_optional_named_keywords(positional, keywords, "split", &["self", "sep", "maxsplit"], 3),
        "rsplit" => {
            bind_optional_named_keywords(positional, keywords, "rsplit", &["self", "sep", "maxsplit"], 3)
        }
        // `open(file, mode='r', buffering=-1, encoding=None, errors=None,
        // newline=None, closefd=True, opener=None)`: `_osx_support` and the
        // sysconfig/platform chain pass `encoding=` (and friends) as
        // keywords; absent optionals arrive as None and the io entry applies
        // its defaults.  `_io.open` shares this row: both spellings are the
        // same native entry registered under the function name `open`.
        "open" => bind_optional_named_keywords(
            positional,
            keywords,
            "open",
            &["file", "mode", "buffering", "encoding", "errors", "newline", "closefd", "opener"],
            8,
        ),
        _ => Err(format!("keyword arguments require Phase-B function metadata ('{name}')")),
    }
}

/// Boxes a binder failure as a live TypeError object before the message
/// rides the generic `Err(String)` channel.  `pon_err_set` preserves a
/// pending boxed exception, so the eventual `return_null_with_error` in the
/// call plumbing keeps the typed object and `except TypeError:` matches
/// (CPython raises TypeError for every keyword-binding failure).
fn raise_boxed_type_error(message: String) -> String {
    let _ = unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    message
}

/// Binds a fixed-shape native signature whose optionals default to None:
/// keywords land in their named slot and every absent slot is filled with
/// None, so the native entry sees one canonical positional layout.
fn bind_optional_named_keywords(
    positional: &[*mut PyObject],
    keywords: KeywordArgs<'_>,
    function_name: &str,
    names: &[&str],
    max_positional: usize,
) -> Result<Vec<*mut PyObject>, String> {
    if positional.len() > max_positional {
        return Err(raise_boxed_type_error(format!(
            "{function_name}() expected at most {max_positional} positional arguments, got {}",
            positional.len()
        )));
    }
    let mut argv = positional.to_vec();
    argv.resize(names.len(), ptr::null_mut());
    for (name, value) in keywords.names.iter().copied().zip(keywords.values.iter().copied()) {
        if value.is_null() {
            return Err(format!("keyword argument {} is NULL", keyword_name(name)));
        }
        let actual = keyword_name(name);
        let Some(index) = names.iter().position(|expected| *expected == actual) else {
            return Err(raise_boxed_type_error(format!("{function_name}() got an unexpected keyword argument '{actual}'")));
        };
        if index < positional.len() || !argv[index].is_null() {
            return Err(raise_boxed_type_error(format!("{function_name}() got multiple values for argument '{actual}'")));
        }
        argv[index] = value;
    }
    let none = unsafe { crate::abi::pon_none() };
    if none.is_null() {
        return Err(format!("failed to allocate None default for {function_name}()"));
    }
    for slot in &mut argv {
        if slot.is_null() {
            *slot = none;
        }
    }
    Ok(argv)
}

/// Binds a variadic native signature: positionals pass through untouched and
/// the validated keywords ride in a trailing `lazy_iter::PyKwMarker`.
fn bind_trailing_marker_keywords(
    positional: &[*mut PyObject],
    keywords: KeywordArgs<'_>,
    function_name: &str,
    allowed: &[&str],
) -> Result<Vec<*mut PyObject>, String> {
    let mut pairs = Vec::with_capacity(keywords.names.len());
    for (name, value) in keywords.names.iter().copied().zip(keywords.values.iter().copied()) {
        if value.is_null() {
            return Err(format!("keyword argument {} is NULL", keyword_name(name)));
        }
        let actual = keyword_name(name);
        if !allowed.contains(&actual.as_str()) {
            return Err(raise_boxed_type_error(format!("{function_name}() got an unexpected keyword argument '{actual}'")));
        }
        if pairs.iter().any(|&(existing, _)| existing == name) {
            return Err(raise_boxed_type_error(format!("{function_name}() got multiple values for argument '{actual}'")));
        }
        pairs.push((name, value));
    }
    let mut argv = positional.to_vec();
    argv.push(crate::types::lazy_iter::new_kw_marker(pairs));
    Ok(argv)
}

/// Binds a variadic native signature accepting arbitrary keyword names:
/// positionals pass through untouched and every keyword rides in a trailing
/// `lazy_iter::PyKwMarker` (`type(**kwds)` unpacked by `builtin_type`,
/// `str.format(**kwargs)` peeled by `str_format_method`).
fn bind_any_keywords(
    positional: &[*mut PyObject],
    keywords: KeywordArgs<'_>,
    function_name: &str,
) -> Result<Vec<*mut PyObject>, String> {
    let mut pairs = Vec::with_capacity(keywords.names.len());
    for (name, value) in keywords.names.iter().copied().zip(keywords.values.iter().copied()) {
        if value.is_null() {
            return Err(format!("keyword argument {} is NULL", keyword_name(name)));
        }
        if pairs.iter().any(|&(existing, _)| existing == name) {
            return Err(raise_boxed_type_error(format!("{function_name}() got multiple values for argument '{}'", keyword_name(name))));
        }
        pairs.push((name, value));
    }
    let mut argv = positional.to_vec();
    argv.push(crate::types::lazy_iter::new_kw_marker(pairs));
    Ok(argv)
}

fn bind_sorted_keywords(positional: &[*mut PyObject], keywords: KeywordArgs<'_>) -> Result<Vec<*mut PyObject>, String> {
    if positional.len() != 1 {
        return Err(raise_boxed_type_error(format!("sorted expected 1 argument, got {}", positional.len())));
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
            other => return Err(raise_boxed_type_error(format!("sort() got an unexpected keyword argument '{other}'"))),
        }
    }
    Ok(vec![positional[0], crate::types::lazy_iter::new_sort_options(key, reverse)])
}

fn bind_minmax_keywords(positional: &[*mut PyObject], keywords: KeywordArgs<'_>, function_name: &str) -> Result<Vec<*mut PyObject>, String> {
    if positional.is_empty() {
        return Err(raise_boxed_type_error(format!("{function_name} expected at least 1 argument, got 0")));
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
            other => return Err(raise_boxed_type_error(format!("{function_name}() got an unexpected keyword argument '{other}'"))),
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
            other => return Err(raise_boxed_type_error(format!("zip() got an unexpected keyword argument '{other}'"))),
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
        return Err(raise_boxed_type_error(format!("{function_name}() expected at most {max_positional} positional arguments, got {}", positional.len())));
    }
    let mut argv = positional.to_vec();
    argv.resize(max_positional, ptr::null_mut());
    for (name, value) in keywords.names.iter().copied().zip(keywords.values.iter().copied()) {
        if value.is_null() {
            return Err(format!("keyword argument {} is NULL", keyword_name(name)));
        }
        let actual = keyword_name(name);
        let Some(index) = names.iter().position(|expected| *expected == actual) else {
            return Err(raise_boxed_type_error(format!("{function_name}() got an unexpected keyword argument '{actual}'")));
        };
        if index < positional.len() || !argv[index].is_null() {
            return Err(raise_boxed_type_error(format!("{function_name}() got multiple values for argument '{actual}'")));
        }
        argv[index] = value;
    }
    while argv.last().is_some_and(|value| value.is_null()) {
        argv.pop();
    }
    if argv.iter().any(|value| value.is_null()) {
        return Err(raise_boxed_type_error(format!("{function_name}() missing required argument")));
    }
    if argv.len() < min_positional {
        return Err(raise_boxed_type_error(format!("{function_name}() expected at least {min_positional} arguments, got {}", argv.len())));
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
        return Err(raise_boxed_type_error(format!(
            "{function_name}() expected {min_positional} to {max_positional} positional arguments, got {}",
            positional.len()
        )));
    }
    let mut argv = positional.to_vec();
    for (name, value) in keywords.names.iter().copied().zip(keywords.values.iter().copied()) {
        if value.is_null() {
            return Err(format!("keyword argument {} is NULL", keyword_name(name)));
        }
        let actual = keyword_name(name);
        if actual != keyword {
            return Err(raise_boxed_type_error(format!("{function_name}() got an unexpected keyword argument '{actual}'")));
        }
        if argv.len() == max_positional {
            return Err(raise_boxed_type_error(format!("{function_name}() got multiple values for keyword argument '{keyword}'")));
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
    use crate::thread_state::pon_err_clear;

    unsafe extern "C" fn dummy_entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
        ptr::null_mut()
    }

    #[test]
    fn getattro_serves_doc_none_default_and_stored_doc_wins() {
        let _guard = crate::thread_state::test_state_lock();
        unsafe {
            assert_eq!(crate::abi::pon_runtime_init(), 0);
            // The default build keeps process-global thread state: clear any
            // error sentinel a previous test on this harness thread leaked, or
            // every helper below can spuriously return NULL (suite convention,
            // same as abi::number's `init()`).
            pon_err_clear();
            let function =
                crate::abi::pon_make_function(dummy_entry as *const u8, 0, intern("doc_probe"));
            assert!(!function.is_null());
            let doc_name = const_str("__doc__");
            assert!(!doc_name.is_null());
            let none = crate::abi::pon_none();
            assert!(!none.is_null());
            // No docstring metadata is threaded from lowering: default is None.
            assert_eq!(function_getattro(function, doc_name), none);
            // A stored __doc__ wins over the default (attr-dict-first lookup).
            let stored = const_str("stored doc");
            assert!(!stored.is_null());
            assert_eq!(function_setattro(function, doc_name, stored), 0);
            assert_eq!(function_getattro(function, doc_name), stored);
            // Deleting the stored value falls back to the None default.
            assert_eq!(function_setattro(function, doc_name, ptr::null_mut()), 0);
            assert_eq!(function_getattro(function, doc_name), none);
            // __module__ reports the active module name; outside source-module
            // execution that is '__main__'.
            let module_name = const_str("__module__");
            assert!(!module_name.is_null());
            let module = function_getattro(function, module_name);
            assert!(!module.is_null());
            let text = (&*module.cast::<PyUnicode>()).as_str().unwrap();
            assert_eq!(text, "__main__");
        }
    }

    #[test]
    fn binds_positional_defaults_and_keyword_only_defaults() {
        let _guard = crate::thread_state::test_state_lock();
        pon_err_clear();
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
        let _guard = crate::thread_state::test_state_lock();
        pon_err_clear();
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
            "kwonly_binding_case() missing 1 required keyword-only argument: 'required_kwonly'"
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
        let _guard = crate::thread_state::test_state_lock();
        pon_err_clear();
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

    /// Contract: a `__defaults__` tuple assigned after creation drives
    /// Phase-B binding (CPython `func_set_defaults`), and
    /// `unregister_function_record` removes the override so a fresh record at
    /// the same address never resurrects it.
    #[test]
    fn live_defaults_override_drives_binding_and_is_cleared_by_unregister() {
        let _guard = crate::thread_state::test_state_lock();
        unsafe {
            assert_eq!(crate::abi::pon_runtime_init(), 0);
            pon_err_clear();
            let function = crate::abi::pon_make_function(
                dummy_entry as *const u8,
                3,
                intern("live_defaults_override_case"),
            );
            assert!(!function.is_null());
            let names = [intern("lo_a"), intern("lo_b"), intern("lo_c")];
            let params = ParamSpec {
                names: names.as_ptr(),
                total_param_count: names.len() as u32,
                positional_only_count: 0,
                positional_count: 3,
                keyword_only_count: 0,
                varargs_name: 0,
                varkw_name: 0,
            };
            let code = CodeInfo {
                entry: dummy_entry as *const u8,
                params: &params,
                name_interned: intern("live_defaults_override_case"),
                n_locals: 3,
                n_feedback: 0,
                flags: 0,
            };
            register_function_record(function, &code, &[], &[], &[], &[]).unwrap();

            let arg_a = const_str("lo_value_a");
            let val_b = const_str("lo_value_b");
            let val_c = const_str("lo_value_c");
            let defaults_name = const_str("__defaults__");
            assert!(!arg_a.is_null() && !val_b.is_null() && !val_c.is_null());
            assert!(!defaults_name.is_null());
            let override_tuple = tuple_from_objects(vec![val_b, val_c]);
            assert!(!override_tuple.is_null());
            assert_eq!(function_setattro(function, defaults_name, override_tuple), 0);

            let bound = bind_arguments(
                function,
                &[arg_a],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap();
            assert_eq!(bound, vec![arg_a, val_b, val_c]);

            // Unregistering drops the override entry too: a fresh record at
            // the same address starts without defaults again.
            unregister_function_record(function);
            register_function_record(function, &code, &[], &[], &[], &[]).unwrap();
            let err = bind_arguments(
                function,
                &[arg_a],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap_err();
            assert!(err.contains("missing 2 required positional arguments"), "got {err:?}");
            unregister_function_record(function);
        }
    }

    /// Contract: an assigned `__defaults__` tuple REPLACES creation-time
    /// defaults wholesale — a shorter override un-defaults the parameters the
    /// creation tuple used to cover.
    #[test]
    fn defaults_override_replaces_creation_defaults_wholesale() {
        let _guard = crate::thread_state::test_state_lock();
        unsafe {
            assert_eq!(crate::abi::pon_runtime_init(), 0);
            pon_err_clear();
            let function = crate::abi::pon_make_function(
                dummy_entry as *const u8,
                3,
                intern("replace_defaults_case"),
            );
            assert!(!function.is_null());
            let names = [intern("rd_a"), intern("rd_b"), intern("rd_c")];
            let params = ParamSpec {
                names: names.as_ptr(),
                total_param_count: names.len() as u32,
                positional_only_count: 0,
                positional_count: 3,
                keyword_only_count: 0,
                varargs_name: 0,
                varkw_name: 0,
            };
            let code = CodeInfo {
                entry: dummy_entry as *const u8,
                params: &params,
                name_interned: intern("replace_defaults_case"),
                n_locals: 3,
                n_feedback: 0,
                flags: 0,
            };
            let creation_b = const_str("rd_creation_b");
            let creation_c = const_str("rd_creation_c");
            register_function_record(function, &code, &[creation_b, creation_c], &[], &[], &[])
                .unwrap();

            let arg_a = const_str("rd_arg_a");
            let arg_b = const_str("rd_arg_b");
            // Creation defaults stay authoritative until an assignment lands.
            let bound = bind_arguments(
                function,
                &[arg_a],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap();
            assert_eq!(bound, vec![arg_a, creation_b, creation_c]);

            let override_c = const_str("rd_override_c");
            let defaults_name = const_str("__defaults__");
            let override_tuple = tuple_from_objects(vec![override_c]);
            assert!(!override_tuple.is_null());
            assert_eq!(function_setattro(function, defaults_name, override_tuple), 0);

            // The 1-tuple now covers only the LAST parameter...
            let bound = bind_arguments(
                function,
                &[arg_a, arg_b],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap();
            assert_eq!(bound, vec![arg_a, arg_b, override_c]);
            // ...and `rd_b` is no longer defaulted at all (no fallback to the
            // creation tuple).
            let err = bind_arguments(
                function,
                &[arg_a],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap_err();
            assert!(err.contains("missing 1 required positional argument"), "got {err:?}");
            unregister_function_record(function);
        }
    }

    /// Contract: `__defaults__` reads return the assigned object itself
    /// (pointer identity, even with a record holding different creation
    /// defaults), an empty tuple stays readable while defaulting nothing, and
    /// `None` clears the slot with reads reporting `None`.
    #[test]
    fn defaults_override_reads_back_identically_and_clears_via_none_or_empty_tuple() {
        let _guard = crate::thread_state::test_state_lock();
        unsafe {
            assert_eq!(crate::abi::pon_runtime_init(), 0);
            pon_err_clear();
            let function = crate::abi::pon_make_function(
                dummy_entry as *const u8,
                3,
                intern("clear_defaults_case"),
            );
            assert!(!function.is_null());
            let names = [intern("cd_a"), intern("cd_b"), intern("cd_c")];
            let params = ParamSpec {
                names: names.as_ptr(),
                total_param_count: names.len() as u32,
                positional_only_count: 0,
                positional_count: 3,
                keyword_only_count: 0,
                varargs_name: 0,
                varkw_name: 0,
            };
            let code = CodeInfo {
                entry: dummy_entry as *const u8,
                params: &params,
                name_interned: intern("clear_defaults_case"),
                n_locals: 3,
                n_feedback: 0,
                flags: 0,
            };
            let creation_b = const_str("cd_creation_b");
            let creation_c = const_str("cd_creation_c");
            register_function_record(function, &code, &[creation_b, creation_c], &[], &[], &[])
                .unwrap();

            let arg_a = const_str("cd_arg_a");
            let override_b = const_str("cd_override_b");
            let override_c = const_str("cd_override_c");
            let defaults_name = const_str("__defaults__");
            let override_tuple = tuple_from_objects(vec![override_b, override_c]);
            assert!(!override_tuple.is_null());
            assert_eq!(function_setattro(function, defaults_name, override_tuple), 0);
            // Read-your-write identity: the assigned tuple object itself
            // comes back, not a rebuild of the record's creation defaults.
            assert_eq!(function_getattro(function, defaults_name), override_tuple);
            let bound = bind_arguments(
                function,
                &[arg_a],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap();
            assert_eq!(bound, vec![arg_a, override_b, override_c]);

            // An empty tuple defaults nothing (creation defaults stay dead)
            // but reads still return that exact tuple.
            let empty = empty_tuple();
            assert!(!empty.is_null());
            assert_eq!(function_setattro(function, defaults_name, empty), 0);
            let err = bind_arguments(
                function,
                &[arg_a],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap_err();
            assert!(err.contains("missing 2 required positional arguments"), "got {err:?}");
            assert_eq!(function_getattro(function, defaults_name), empty);

            // `None` clears: binding still fails, reads report None.
            let none = crate::abi::pon_none();
            assert!(!none.is_null());
            assert_eq!(function_setattro(function, defaults_name, none), 0);
            let err = bind_arguments(
                function,
                &[arg_a],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap_err();
            assert!(err.contains("missing 2 required positional arguments"), "got {err:?}");
            assert_eq!(function_getattro(function, defaults_name), none);
            unregister_function_record(function);
        }
    }

    /// Contract: an override tuple longer than the positional arity aligns to
    /// the TAIL (CPython semantics) — the unused head is skipped, never bound.
    #[test]
    fn overlong_defaults_override_aligns_to_trailing_positional_slots() {
        let _guard = crate::thread_state::test_state_lock();
        unsafe {
            assert_eq!(crate::abi::pon_runtime_init(), 0);
            pon_err_clear();
            let function = crate::abi::pon_make_function(
                dummy_entry as *const u8,
                3,
                intern("tail_align_case"),
            );
            assert!(!function.is_null());
            let names = [intern("ta_a"), intern("ta_b"), intern("ta_c")];
            let params = ParamSpec {
                names: names.as_ptr(),
                total_param_count: names.len() as u32,
                positional_only_count: 0,
                positional_count: 3,
                keyword_only_count: 0,
                varargs_name: 0,
                varkw_name: 0,
            };
            let code = CodeInfo {
                entry: dummy_entry as *const u8,
                params: &params,
                name_interned: intern("tail_align_case"),
                n_locals: 3,
                n_feedback: 0,
                flags: 0,
            };
            register_function_record(function, &code, &[], &[], &[], &[]).unwrap();

            let d0 = const_str("ta_d0");
            let d1 = const_str("ta_d1");
            let d2 = const_str("ta_d2");
            let d3 = const_str("ta_d3");
            let d4 = const_str("ta_d4");
            let defaults_name = const_str("__defaults__");
            let override_tuple = tuple_from_objects(vec![d0, d1, d2, d3, d4]);
            assert!(!override_tuple.is_null());
            assert_eq!(function_setattro(function, defaults_name, override_tuple), 0);

            let bound = bind_arguments(
                function,
                &[],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap();
            assert_eq!(bound, vec![d2, d3, d4]);

            let arg_a = const_str("ta_arg_a");
            let bound = bind_arguments(
                function,
                &[arg_a],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap();
            assert_eq!(bound, vec![arg_a, d3, d4]);
            unregister_function_record(function);
        }
    }

    /// Contract: a Phase-A function (arity only, no record) honors a live
    /// `__defaults__` override for short calls, both through `bind_arguments`
    /// and via `fill_positional_defaults` directly; the filler declines while
    /// no override was ever assigned or when required slots stay uncovered.
    #[test]
    fn phase_a_short_call_fills_trailing_slots_from_defaults_override() {
        let _guard = crate::thread_state::test_state_lock();
        unsafe {
            assert_eq!(crate::abi::pon_runtime_init(), 0);
            pon_err_clear();
            let function = crate::abi::pon_make_function(
                dummy_entry as *const u8,
                3,
                intern("phase_a_fill_case"),
            );
            assert!(!function.is_null());

            let arg_a = const_str("pa_arg_a");
            // Never-assigned override: the filler declines so callers keep
            // their original arity diagnostics.
            assert!(fill_positional_defaults(function, &[arg_a], 3).is_none());

            let val_x = const_str("pa_val_x");
            let val_y = const_str("pa_val_y");
            let defaults_name = const_str("__defaults__");
            let override_tuple = tuple_from_objects(vec![val_x, val_y]);
            assert!(!override_tuple.is_null());
            assert_eq!(function_setattro(function, defaults_name, override_tuple), 0);

            let bound = bind_arguments(
                function,
                &[arg_a],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap();
            assert_eq!(bound, vec![arg_a, val_x, val_y]);
            assert_eq!(
                fill_positional_defaults(function, &[arg_a], 3),
                Some(vec![arg_a, val_x, val_y])
            );

            // The first slot is not covered by the 2-tuple: still an arity
            // error, not a partial fill.
            let err = bind_arguments(
                function,
                &[],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap_err();
            assert!(err.contains("function expected 3 arguments, got 0"), "got {err:?}");
            unregister_function_record(function);
        }
    }

    /// Contract: `__defaults__` accepts only tuple/None and `__kwdefaults__`
    /// only dict/None — a rejected assignment returns -1, leaves the CPython
    /// error message pending, and installs NOTHING (creation defaults stay
    /// authoritative).
    #[test]
    fn setattro_rejects_non_tuple_defaults_and_non_dict_kwdefaults() {
        let _guard = crate::thread_state::test_state_lock();
        unsafe {
            assert_eq!(crate::abi::pon_runtime_init(), 0);
            pon_err_clear();
            let function = crate::abi::pon_make_function(
                dummy_entry as *const u8,
                1,
                intern("validate_override_case"),
            );
            assert!(!function.is_null());
            let names = [intern("vd_a")];
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
                name_interned: intern("validate_override_case"),
                n_locals: 1,
                n_feedback: 0,
                flags: 0,
            };
            let creation_a = const_str("vd_creation_a");
            register_function_record(function, &code, &[creation_a], &[], &[], &[]).unwrap();

            let defaults_name = const_str("__defaults__");
            let mut list_items = [const_str("vd_list_item")];
            let list = crate::abi::seq::pon_build_list(list_items.as_mut_ptr(), 1);
            assert!(!list.is_null());
            assert_eq!(function_setattro(function, defaults_name, list), -1);
            assert!(pon_err_occurred());
            let message = crate::thread_state::pon_err_message().unwrap_or_default();
            assert!(
                message.contains("__defaults__ must be set to a tuple object"),
                "got {message:?}"
            );
            pon_err_clear();
            // The rejected assignment stored no override: the creation
            // default still fills the slot (a buggy store-as-cleared would
            // make this bind fail).
            let bound = bind_arguments(
                function,
                &[],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap();
            assert_eq!(bound, vec![creation_a]);

            let kwdefaults_name = const_str("__kwdefaults__");
            let not_a_dict = tuple_from_objects(vec![creation_a]);
            assert!(!not_a_dict.is_null());
            assert_eq!(function_setattro(function, kwdefaults_name, not_a_dict), -1);
            assert!(pon_err_occurred());
            let message = crate::thread_state::pon_err_message().unwrap_or_default();
            assert!(
                message.contains("__kwdefaults__ must be set to a dict object"),
                "got {message:?}"
            );
            pon_err_clear();
            unregister_function_record(function);
        }
    }

    /// Contract: a `__kwdefaults__` dict assigned after creation overrides
    /// the creation-time keyword-only default, and assigning `None` clears it
    /// so the parameter becomes required again.
    #[test]
    fn kwdefaults_override_drives_keyword_only_binding_until_cleared() {
        let _guard = crate::thread_state::test_state_lock();
        unsafe {
            assert_eq!(crate::abi::pon_runtime_init(), 0);
            pon_err_clear();
            let function = crate::abi::pon_make_function(
                dummy_entry as *const u8,
                2,
                intern("kwdefaults_override_case"),
            );
            assert!(!function.is_null());
            let flag_name = intern("kw_flag_param");
            let names = [intern("kw_pos_param"), flag_name];
            let params = ParamSpec {
                names: names.as_ptr(),
                total_param_count: names.len() as u32,
                positional_only_count: 0,
                positional_count: 1,
                keyword_only_count: 1,
                varargs_name: 0,
                varkw_name: 0,
            };
            let code = CodeInfo {
                entry: dummy_entry as *const u8,
                params: &params,
                name_interned: intern("kwdefaults_override_case"),
                n_locals: 2,
                n_feedback: 0,
                flags: 0,
            };
            let creation_val = const_str("kw_creation_value");
            register_function_record(function, &code, &[], &[flag_name], &[creation_val], &[])
                .unwrap();

            let arg_pos = const_str("kw_pos_value");
            let bound = bind_arguments(
                function,
                &[arg_pos],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap();
            assert_eq!(bound, vec![arg_pos, creation_val]);

            let override_val = const_str("kw_override_value");
            let mut pairs = [const_str("kw_flag_param"), override_val];
            let override_dict = crate::abi::map::pon_build_map(pairs.as_mut_ptr(), 1);
            assert!(!override_dict.is_null());
            let kwdefaults_name = const_str("__kwdefaults__");
            assert_eq!(function_setattro(function, kwdefaults_name, override_dict), 0);
            let bound = bind_arguments(
                function,
                &[arg_pos],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap();
            assert_eq!(bound, vec![arg_pos, override_val]);

            // Clearing makes the keyword-only parameter required again — no
            // fallback to the creation-time default.
            let none = crate::abi::pon_none();
            assert_eq!(function_setattro(function, kwdefaults_name, none), 0);
            let err = bind_arguments(
                function,
                &[arg_pos],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap_err();
            assert!(
                err.contains("missing 1 required keyword-only argument"),
                "got {err:?}"
            );
            unregister_function_record(function);
        }
    }

    /// Contract: `__defaults__` accepts a REAL tuple-subclass instance
    /// (`PyTuple_Check` semantics, not exact-tuple): assignment returns 0,
    /// reads return the very same object, and binding fills trailing slots
    /// from the subclass's embedded tuple storage — the layout-safe
    /// `tuple_storage_slice` view, where a blind `PyTuple` cast would
    /// misread the heap-instance layout.
    #[test]
    fn defaults_override_accepts_tuple_subclass_instance_and_binds_from_its_storage() {
        let _guard = crate::thread_state::test_state_lock();
        unsafe {
            assert_eq!(crate::abi::pon_runtime_init(), 0);
            pon_err_clear();
            // A real heap class linearizing over builtin `tuple`
            // (`class TsdDefaults(tuple): pass`) and an instance through
            // `type_new` — the same path `TsdDefaults((b, c))` takes.
            let tuple_base = crate::native::builtins_mod::builtin_native_type("tuple")
                .expect("builtin tuple type");
            let cls = crate::types::type_::build_class_from_namespace(
                "TsdDefaults",
                &[tuple_base.cast::<PyObject>()],
                crate::types::type_::new_namespace(),
                &[],
            )
            .cast::<PyType>();
            assert!(!cls.is_null());
            assert!(crate::types::tuple::type_is_tuple_subclass(cls));

            let val_b = const_str("tsd_val_b");
            let val_c = const_str("tsd_val_c");
            let payload = tuple_from_objects(vec![val_b, val_c]);
            assert!(!payload.is_null());
            let ctor_args = tuple_from_objects(vec![payload]);
            assert!(!ctor_args.is_null());
            let instance = crate::types::type_::type_new(cls, ctor_args, ptr::null_mut());
            assert!(!instance.is_null());
            assert!(crate::types::tuple::is_tuple_subclass_instance(instance));

            let function = crate::abi::pon_make_function(
                dummy_entry as *const u8,
                3,
                intern("tsd_subclass_defaults_case"),
            );
            assert!(!function.is_null());
            let names = [intern("tsd_a"), intern("tsd_b"), intern("tsd_c")];
            let params = ParamSpec {
                names: names.as_ptr(),
                total_param_count: names.len() as u32,
                positional_only_count: 0,
                positional_count: 3,
                keyword_only_count: 0,
                varargs_name: 0,
                varkw_name: 0,
            };
            let code = CodeInfo {
                entry: dummy_entry as *const u8,
                params: &params,
                name_interned: intern("tsd_subclass_defaults_case"),
                n_locals: 3,
                n_feedback: 0,
                flags: 0,
            };
            register_function_record(function, &code, &[], &[], &[], &[]).unwrap();

            let defaults_name = const_str("__defaults__");
            assert!(!defaults_name.is_null());
            // The widened validation accepts tuple STORAGE: a pre-widening
            // exact-`PyTuple` gate returned -1 for this instance.
            assert_eq!(function_setattro(function, defaults_name, instance), 0);
            // Identity read-back of the subclass instance itself.
            assert_eq!(function_getattro(function, defaults_name), instance);

            // One supplied arg; the trailing two slots come from the SUBCLASS
            // storage elements.
            let arg_a = const_str("tsd_arg_a");
            let bound = bind_arguments(
                function,
                &[arg_a],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap();
            assert_eq!(bound, vec![arg_a, val_b, val_c]);
            unregister_function_record(function);
        }
    }

    /// Contract: `__kwdefaults__` accepts a REAL dict-subclass instance
    /// (`PyDict_Check` semantics, not exact-dict): assignment returns 0,
    /// reads return the very same object, and keyword-only binding consults
    /// the subclass's embedded dict storage entries.
    #[test]
    fn kwdefaults_override_accepts_dict_subclass_instance_for_keyword_only_binding() {
        let _guard = crate::thread_state::test_state_lock();
        unsafe {
            assert_eq!(crate::abi::pon_runtime_init(), 0);
            pon_err_clear();
            // A real heap class linearizing over builtin `dict`
            // (`class KsdDict(dict): pass`); `type_new` allocates the empty
            // embedded storage and `dict_insert` populates it.
            let type_type = crate::abi::runtime_type_type();
            assert!(!type_type.is_null());
            let dict_base = dict::dict_type(type_type);
            let cls = crate::types::type_::build_class_from_namespace(
                "KsdDict",
                &[dict_base.cast::<PyObject>()],
                crate::types::type_::new_namespace(),
                &[],
            )
            .cast::<PyType>();
            assert!(!cls.is_null());
            let instance = crate::types::type_::type_new(cls, ptr::null_mut(), ptr::null_mut());
            assert!(!instance.is_null());
            assert!(dict::is_dict_subclass_instance(instance));
            let override_val = const_str("ksd_override_value");
            dict::dict_insert(instance, const_str("ksd_flag_param"), override_val).unwrap();

            let function = crate::abi::pon_make_function(
                dummy_entry as *const u8,
                2,
                intern("ksd_subclass_kwdefaults_case"),
            );
            assert!(!function.is_null());
            let flag_name = intern("ksd_flag_param");
            let names = [intern("ksd_pos_param"), flag_name];
            let params = ParamSpec {
                names: names.as_ptr(),
                total_param_count: names.len() as u32,
                positional_only_count: 0,
                positional_count: 1,
                keyword_only_count: 1,
                varargs_name: 0,
                varkw_name: 0,
            };
            let code = CodeInfo {
                entry: dummy_entry as *const u8,
                params: &params,
                name_interned: intern("ksd_subclass_kwdefaults_case"),
                n_locals: 2,
                n_feedback: 0,
                flags: 0,
            };
            register_function_record(function, &code, &[], &[], &[], &[]).unwrap();

            // No creation-time kwdefault: the keyword-only parameter is
            // required, so the successful bind below can only come from the
            // subclass override.
            let arg_pos = const_str("ksd_pos_value");
            let err = bind_arguments(
                function,
                &[arg_pos],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap_err();
            assert!(
                err.contains("missing 1 required keyword-only argument"),
                "got {err:?}"
            );

            let kwdefaults_name = const_str("__kwdefaults__");
            // The widened validation accepts dict STORAGE: a pre-widening
            // exact-dict gate returned -1 for this instance.
            assert_eq!(function_setattro(function, kwdefaults_name, instance), 0);
            assert_eq!(function_getattro(function, kwdefaults_name), instance);

            let bound = bind_arguments(
                function,
                &[arg_pos],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap();
            assert_eq!(bound, vec![arg_pos, override_val]);
            unregister_function_record(function);
        }
    }

    /// Regression pin for the tier-up dispatch contract in
    /// `call_bound_function` (commit e9da81f routed every `def` through the
    /// Phase-B record path, which silently pinned dispatch to the record's
    /// creation-time tier-0 `entry` snapshot and skipped the call-hotness
    /// probe): the call MUST bump `pon_tierup_bump_call` and dispatch through
    /// the live `PyFunction::entry` cell, so an installed tier-1 body is
    /// actually entered.
    #[test]
    fn call_bound_function_dispatches_live_entry_and_bumps_hotness() {
        use std::sync::atomic::AtomicUsize;

        // Per-tier hit counters: dispatch evidence without fabricating
        // PyObject return values (both stubs return the None singleton, which
        // is safe to hand back through `call_bound_function`).
        static TIER0_HITS: AtomicUsize = AtomicUsize::new(0);
        static TIER1_HITS: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn tier0_stub(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
            TIER0_HITS.fetch_add(1, Ordering::SeqCst);
            unsafe { crate::abi::pon_none() }
        }

        unsafe extern "C" fn tier1_stub(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
            TIER1_HITS.fetch_add(1, Ordering::SeqCst);
            unsafe { crate::abi::pon_none() }
        }

        let _guard = crate::thread_state::test_state_lock();
        unsafe {
            assert_eq!(crate::abi::pon_runtime_init(), 0);
            pon_err_clear();
            TIER0_HITS.store(0, Ordering::SeqCst);
            TIER1_HITS.store(0, Ordering::SeqCst);

            let function = crate::abi::pon_make_function(
                tier0_stub as *const u8,
                0,
                intern("tierup_dispatch_probe"),
            );
            assert!(!function.is_null());

            // Phase-B record exactly as `def` registration installs it: the
            // record's `entry` snapshots the tier-0 body.  Zero parameters,
            // so the bound argv is empty (`copy_param_spec` accepts a null
            // names pointer when no named parameter is described).
            let params = ParamSpec {
                names: ptr::null(),
                total_param_count: 0,
                positional_only_count: 0,
                positional_count: 0,
                keyword_only_count: 0,
                varargs_name: 0,
                varkw_name: 0,
            };
            let code = CodeInfo {
                entry: tier0_stub as *const u8,
                params: &params,
                name_interned: intern("tierup_dispatch_probe"),
                n_locals: 0,
                n_feedback: 0,
                flags: 0,
            };
            register_function_record(function, &code, &[], &[], &[], &[]).unwrap();

            let none = crate::abi::pon_none();
            assert!(!none.is_null());

            // First call: the live `entry` cell still holds the tier-0 body.
            let result = call_bound_function(
                function,
                &[],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap();
            assert_eq!(result, none);
            assert_eq!(TIER0_HITS.load(Ordering::SeqCst), 1);
            assert_eq!(TIER1_HITS.load(Ordering::SeqCst), 0);

            // Simulate the tier-up install protocol: publish a tier-1 body in
            // the live dispatch cell.  The record's `entry` and the function's
            // `code` field still hold the tier-0 stub, so a dispatch that
            // reverts to either snapshot runs `tier0_stub` again and fails
            // the counter asserts below.
            (*function.cast::<PyFunction>())
                .entry
                .store(tier1_stub as *mut u8, Ordering::Release);

            let result = call_bound_function(
                function,
                &[],
                KeywordArgs { names: &[], values: &[] },
                None,
                None,
            )
            .unwrap();
            assert_eq!(result, none);
            assert_eq!(TIER0_HITS.load(Ordering::SeqCst), 1);
            assert_eq!(TIER1_HITS.load(Ordering::SeqCst), 1);

            // Both calls must feed the call-hotness probe
            // (`pon_tierup_bump_call`), or functions routed through Phase-B
            // records never get hot and tier-up starves.
            assert!(
                (*function.cast::<PyFunction>()).hotness.load(Ordering::Acquire) >= 2,
                "call_bound_function must bump the tier-up call probe"
            );

            unregister_function_record(function);
        }
    }
}
