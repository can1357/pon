//! Dynamic code execution builtins and runtime/JIT callback seam.
//!
//! `pon-runtime` deliberately does not depend on `pon-ir` or `pon-jit`.  The
//! embedding frontend installs small function-pointer hooks that validate and
//! execute source through the normal lowering/JIT pipeline.  This module owns the
//! Python-visible code object shell plus namespace defaulting for
//! `compile`/`eval`/`exec`, `globals`, `locals`, and `__import__`.

use std::collections::HashMap;
use std::mem;
use std::ptr;
use std::sync::{LazyLock, Mutex};

use num_traits::ToPrimitive;

use crate::abi::{self, map, pon_const_str, pon_none, return_null_with_error};
use crate::intern::{intern, resolve};
use crate::object::{PyObject, PyObjectHeader, PyType, PyUnicode};
use crate::types::{dict, int};

/// Dynamic code compilation mode accepted by Python's `compile` builtin.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DynCodeMode {
    /// Expression mode used by `eval`.
    Eval = 0,
    /// Module/statement mode used by `exec`.
    Exec = 1,
    /// Interactive single-input mode.  Pon currently executes it like `exec`.
    Single = 2,
}

impl DynCodeMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eval => "eval",
            Self::Exec => "exec",
            Self::Single => "single",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "eval" => Some(Self::Eval),
            "exec" => Some(Self::Exec),
            "single" => Some(Self::Single),
            _ => None,
        }
    }
}

/// Host-side compile validation request.
pub struct DynCompileRequest<'a> {
    pub source: &'a str,
    pub filename: &'a str,
    pub mode: DynCodeMode,
}

/// Host-side execution request.
pub struct DynExecuteRequest<'a> {
    pub source: &'a str,
    pub filename: &'a str,
    pub mode: DynCodeMode,
    pub globals: *mut PyObject,
    pub locals: *mut PyObject,
}

/// Validate dynamic source without running it.
pub type DynCompileHook = for<'a> fn(DynCompileRequest<'a>) -> Result<(), String>;
/// Compile and execute dynamic source.
pub type DynExecuteHook = for<'a> fn(DynExecuteRequest<'a>) -> Result<*mut PyObject, String>;
/// Parse dynamic source to a neutral `_ast` tree (`compile` with
/// `PyCF_ONLY_AST`, i.e. `ast.parse`).  `Err` means the source failed to
/// parse and surfaces as `SyntaxError`.
pub type DynAstParseHook = for<'a> fn(DynCompileRequest<'a>) -> Result<crate::native::AstNode, String>;

#[derive(Default)]
struct DynHooks {
    compile: Option<DynCompileHook>,
    execute: Option<DynExecuteHook>,
    ast_parse: Option<DynAstParseHook>,
}

static DYN_HOOKS: LazyLock<Mutex<DynHooks>> = LazyLock::new(|| Mutex::new(DynHooks::default()));

/// Install the host callbacks used by `compile`, `eval`, and `exec`.
pub fn set_dynamic_code_hooks(compile: DynCompileHook, execute: DynExecuteHook) {
    let mut hooks = DYN_HOOKS.lock().unwrap_or_else(|poison| poison.into_inner());
    hooks.compile = Some(compile);
    hooks.execute = Some(execute);
}

/// Install the host callback serving `compile(..., PyCF_ONLY_AST)`.
/// Separate from [`set_dynamic_code_hooks`] so embeddings without an AST
/// bridge (AoT products) keep the typed refusal.
pub fn set_ast_parse_hook(hook: DynAstParseHook) {
    let mut hooks = DYN_HOOKS.lock().unwrap_or_else(|poison| poison.into_inner());
    hooks.ast_parse = Some(hook);
}

#[repr(C)]
#[derive(Debug)]
pub struct PyCodeObject {
    /// Common object header; must remain first.
    pub ob_base: PyObjectHeader,
    source: String,
    filename: String,
    mode: DynCodeMode,
}

unsafe impl Send for PyCodeObject {}

fn code_type() -> *mut PyType {
    static CODE_TYPE: LazyLock<usize> = LazyLock::new(|| {
        let ty = PyType::new(ptr::null(), "code", mem::size_of::<PyCodeObject>());
        Box::into_raw(Box::new(ty)) as usize
    });
    *CODE_TYPE as *mut PyType
}

fn alloc_code_object(source: String, filename: String, mode: DynCodeMode) -> *mut PyObject {
    Box::into_raw(Box::new(PyCodeObject {
        ob_base: PyObjectHeader::new(code_type()),
        source,
        filename,
        mode,
    }))
    .cast::<PyObject>()
}

unsafe fn as_code_object<'a>(object: *mut PyObject) -> Option<&'a PyCodeObject> {
    if object.is_null() || unsafe { !int::type_name_is(object, "code") } {
        return None;
    }
    Some(unsafe { &*object.cast::<PyCodeObject>() })
}

#[derive(Clone, Copy)]
struct GlobalsBinding {
    module_name: u32,
}

#[derive(Default)]
struct GlobalsRegistry {
    by_module: HashMap<u32, usize>,
    by_dict: HashMap<usize, GlobalsBinding>,
}

static GLOBALS_REGISTRY: LazyLock<Mutex<GlobalsRegistry>> = LazyLock::new(|| Mutex::new(GlobalsRegistry::default()));

/// GC roots for module globals dictionaries returned by `globals()`.
pub(crate) fn rooted_globals_dicts() -> Vec<*mut PyObject> {
    let registry = GLOBALS_REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
    registry
        .by_dict
        .keys()
        .copied()
        .map(|addr| addr as *mut PyObject)
        .collect()
}

fn argv_slice<'a>(argv: *mut *mut PyObject, argc: usize, name: &str) -> Result<&'a [*mut PyObject], String> {
    if argv.is_null() && argc != 0 {
        return Err(format!("{name}() received a NULL argv pointer"));
    }
    Ok(if argc == 0 {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(argv.cast_const(), argc) }
    })
}

unsafe fn str_text(object: *mut PyObject) -> Option<String> {
    if unsafe { !int::type_name_is(object, "str") } {
        return None;
    }
    let unicode = unsafe { &*object.cast::<PyUnicode>() };
    if unicode.data.is_null() && unicode.len != 0 {
        return None;
    }
    let bytes = unsafe { core::slice::from_raw_parts(unicode.data, unicode.len) };
    core::str::from_utf8(bytes).ok().map(ToOwned::to_owned)
}

unsafe fn is_none(object: *mut PyObject) -> bool {
    unsafe { int::type_name_is(object, "NoneType") }
}

fn const_str_object(value: &str) -> Result<*mut PyObject, String> {
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    if object.is_null() {
        Err(format!("failed to allocate string '{value}'"))
    } else {
        Ok(object)
    }
}

fn empty_dict() -> Result<*mut PyObject, String> {
    let dict = unsafe { map::pon_build_map(ptr::null_mut(), 0) };
    if dict.is_null() {
        Err("failed to allocate dict".to_owned())
    } else {
        Ok(dict)
    }
}

unsafe fn require_dict(object: *mut PyObject, name: &str) -> Result<*mut PyObject, String> {
    if unsafe { dict::is_dict(object) } {
        Ok(object)
    } else {
        Err(format!("{name} must be a dict"))
    }
}

fn module_name_for_globals() -> u32 {
    crate::import::active_module_name_id().unwrap_or_else(|| intern("__main__"))
}

fn sync_module_attrs_into_dict(module_name: u32, dict_object: *mut PyObject) -> Result<(), String> {
    let Some(attrs) = crate::import::module_attrs_snapshot(module_name) else {
        return Ok(());
    };
    for (name, value) in attrs {
        let Some(name_text) = resolve(name) else {
            continue;
        };
        let key = const_str_object(&name_text)?;
        unsafe { dict::dict_insert(dict_object, key, value)? };
    }
    Ok(())
}

fn module_globals_dict() -> Result<*mut PyObject, String> {
    module_namespace_dict(module_name_for_globals())
}

/// Live namespace dict for one module, registered so mutations through it
/// sync back into the module's attrs (`globals()` for the active module,
/// `some_module.__dict__` for any module).
pub(crate) fn module_namespace_dict(module_name: u32) -> Result<*mut PyObject, String> {
    if let Some(dict_addr) = GLOBALS_REGISTRY
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .by_module
        .get(&module_name)
        .copied()
    {
        let dict_object = dict_addr as *mut PyObject;
        sync_module_attrs_into_dict(module_name, dict_object)?;
        return Ok(dict_object);
    }

    let dict_object = empty_dict()?;
    sync_module_attrs_into_dict(module_name, dict_object)?;
    let mut registry = GLOBALS_REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
    registry.by_module.insert(module_name, dict_object as usize);
    registry
        .by_dict
        .insert(dict_object as usize, GlobalsBinding { module_name });
    Ok(dict_object)
}

/// Value bound in `module_name`'s registered namespace dict, or `None` when
/// no dict was ever materialized or it lacks `name`. Read-only peek that
/// never creates the dict: module attr lookups fall back here so dict-only
/// bindings (e.g. `vars(mod)["k"] = v`) resolve like CPython, where the
/// module `__dict__` IS the attribute namespace.
pub(crate) fn peek_module_namespace_value(module_name: u32, name: &str) -> Option<*mut PyObject> {
    let dict_addr = GLOBALS_REGISTRY
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .by_module
        .get(&module_name)
        .copied()?;
    let key = const_str_object(name).ok()?;
    unsafe { dict::dict_get(dict_addr as *mut PyObject, key) }.ok().flatten()
}

fn binding_for_dict(dict_object: *mut PyObject) -> Option<GlobalsBinding> {
    GLOBALS_REGISTRY
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .by_dict
        .get(&(dict_object as usize))
        .copied()
}

fn key_name_id(key: *mut PyObject) -> Option<u32> {
    let text = unsafe { str_text(key) }?;
    Some(intern(&text))
}

/// Mirror a compiled global store into `module_name`'s previously-returned
/// globals dict (defining-module scoping: the store may target a module other
/// than the active one when a cross-module function body rebinds a global).
pub(crate) fn sync_global_store_for_module(module_name: u32, name: u32, value: *mut PyObject) {
    if value.is_null() {
        return;
    }
    let dict_addr = {
        let registry = GLOBALS_REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
        registry.by_module.get(&module_name).copied()
    };
    let Some(dict_addr) = dict_addr else {
        return;
    };
    let Some(name_text) = resolve(name) else {
        return;
    };
    if let Ok(key) = const_str_object(&name_text) {
        let _ = unsafe { dict::dict_insert(dict_addr as *mut PyObject, key, value) };
    }
}

/// Mirror a compiled global deletion into `module_name`'s previously-returned
/// globals dict (defining-module scoping, matching `sync_global_store_for_module`).
pub(crate) fn sync_global_delete_for_module(module_name: u32, name: u32) {
    let dict_addr = {
        let registry = GLOBALS_REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
        registry.by_module.get(&module_name).copied()
    };
    let Some(dict_addr) = dict_addr else {
        return;
    };
    let Some(name_text) = resolve(name) else {
        return;
    };
    if let Ok(key) = const_str_object(&name_text) {
        let _ = unsafe { dict::dict_remove(dict_addr as *mut PyObject, key) };
    }
}

/// Called by dict item-assignment helpers after a successful write.
pub(crate) fn sync_globals_dict_set(dict_object: *mut PyObject, key: *mut PyObject, value: *mut PyObject) {
    if dict_object.is_null() || value.is_null() {
        return;
    }
    let Some(binding) = binding_for_dict(dict_object) else {
        return;
    };
    if crate::import::active_module_name_id() != Some(binding.module_name) {
        return;
    }
    let Some(name) = key_name_id(key) else {
        return;
    };
    crate::import::store_active_module_attr(name, value);
}

/// Called by dict item-deletion helpers after a successful delete.
pub(crate) fn sync_globals_dict_delete(dict_object: *mut PyObject, key: *mut PyObject) {
    if dict_object.is_null() {
        return;
    }
    let Some(binding) = binding_for_dict(dict_object) else {
        return;
    };
    if crate::import::active_module_name_id() != Some(binding.module_name) {
        return;
    }
    let Some(name) = key_name_id(key) else {
        return;
    };
    crate::import::delete_active_module_attr(name);
}

/// Called by `dict.update` after a successful bulk write.
///
/// `globals().update({...})` mutates the registered globals dict without
/// going through the item-assignment helpers, so the new bindings must be
/// copied back into the active module's attrs for compiled name lookups to
/// see them (re._constants injects its opcode constants this way).
pub(crate) fn sync_globals_dict_bulk(dict_object: *mut PyObject) {
    if dict_object.is_null() {
        return;
    }
    let Some(binding) = binding_for_dict(dict_object) else {
        return;
    };
    if crate::import::active_module_name_id() != Some(binding.module_name) {
        return;
    }
    let _ = copy_dict_to_module(dict_object, binding.module_name);
}

/// Dynamic-compile failure split by Python-visible type: `Syntax` raises a
/// catchable `SyntaxError` (the parse/lower pipeline rejected the source);
/// `Unavailable` stays on the untyped diagnostic path (no compile hook is
/// installed in this embedding, an embedder defect rather than user code).
enum CompileError {
    Syntax(String),
    Unavailable(String),
}

fn raise_compile_error(error: CompileError) -> *mut PyObject {
    match error {
        CompileError::Syntax(message) => raise_dyn_syntax_error(&message),
        CompileError::Unavailable(message) => return_null_with_error(message),
    }
}

fn raise_dyn_type_error(message: &str) -> *mut PyObject {
    abi::exc::raise_kind_error_text(crate::types::exc::ExceptionKind::TypeError, message)
}

fn raise_dyn_value_error(message: &str) -> *mut PyObject {
    abi::exc::raise_kind_error_text(crate::types::exc::ExceptionKind::ValueError, message)
}

fn raise_dyn_syntax_error(message: &str) -> *mut PyObject {
    abi::exc::raise_kind_error_text(crate::types::exc::ExceptionKind::SyntaxError, message)
}

fn compile_source(source: String, filename: String, mode: DynCodeMode) -> Result<*mut PyObject, CompileError> {
    let hook = DYN_HOOKS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .compile;
    let Some(hook) = hook else {
        return Err(CompileError::Unavailable(
            "dynamic code compilation is not available in this runtime".to_owned(),
        ));
    };
    hook(DynCompileRequest {
        source: &source,
        filename: &filename,
        mode,
    })
    .map_err(CompileError::Syntax)?;
    Ok(alloc_code_object(source, filename, mode))
}

/// Source-encoding classes the PEP 263 decoder understands.  CPython accepts
/// any registered codec; pon supports the encodings that appear in practice
/// (UTF-8 default, the Latin-1 family, ASCII) and reports the rest as
/// `SyntaxError: unknown encoding`.
enum SourceEncoding {
    Utf8,
    Latin1,
    Ascii,
    Unknown,
}

fn normalize_source_encoding(cookie: &str) -> SourceEncoding {
    let normalized = cookie.to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "utf-8" | "utf8" => SourceEncoding::Utf8,
        "latin-1" | "latin1" | "latin" | "l1" | "iso-8859-1" | "iso8859-1" | "iso-latin-1" | "8859" | "cp819" => {
            SourceEncoding::Latin1
        }
        "ascii" | "us-ascii" | "646" => SourceEncoding::Ascii,
        _ => SourceEncoding::Unknown,
    }
}

/// PEP 263 cookie scan: a comment on line 1 (or line 2 when line 1 is blank
/// or comment-only) matching `coding[:=][ \t]*([-_.a-zA-Z0-9]+)`.
fn source_coding_cookie(bytes: &[u8]) -> Option<String> {
    let mut lines = bytes.split(|&b| b == b'\n');
    for _ in 0..2 {
        let line = lines.next()?;
        if let Some(cookie) = line_coding_cookie(line) {
            return Some(cookie);
        }
        match line.iter().position(|&b| !matches!(b, b' ' | b'\t' | b'\x0c' | b'\r')) {
            // Blank line or bare comment: the cookie may still be on line 2.
            None => {}
            Some(index) if line[index] == b'#' => {}
            // Real code before any cookie: no declaration possible.
            Some(_) => return None,
        }
    }
    None
}

fn line_coding_cookie(line: &[u8]) -> Option<String> {
    let start = line.iter().position(|&b| !matches!(b, b' ' | b'\t' | b'\x0c'))?;
    if line[start] != b'#' {
        return None;
    }
    let comment = &line[start..];
    let mut index = 0;
    while index + 7 <= comment.len() {
        if &comment[index..index + 6] == b"coding" && matches!(comment[index + 6], b':' | b'=') {
            let mut cursor = index + 7;
            while cursor < comment.len() && matches!(comment[cursor], b' ' | b'\t') {
                cursor += 1;
            }
            let name_start = cursor;
            while cursor < comment.len()
                && (comment[cursor].is_ascii_alphanumeric() || matches!(comment[cursor], b'-' | b'_' | b'.'))
            {
                cursor += 1;
            }
            if cursor > name_start {
                // The character class above is pure ASCII; from_utf8 cannot fail.
                return core::str::from_utf8(&comment[name_start..cursor]).ok().map(str::to_owned);
            }
            return None;
        }
        index += 1;
    }
    None
}

/// PEP 263 source-bytes decoding for `compile`/`eval`/`exec` and source
/// module loading: honors a UTF-8 BOM and a coding cookie on the first two
/// lines, defaulting to UTF-8.  `Err` carries CPython's SyntaxError message
/// shapes (`Non-UTF-8 code starting with ...` for undeclared non-UTF-8
/// bytes, `unknown encoding: ...` for unsupported cookies).
pub(crate) fn decode_python_source(bytes: &[u8], filename: &str) -> Result<String, String> {
    let (bytes, bom) = match bytes {
        [0xef, 0xbb, 0xbf, rest @ ..] => (rest, true),
        _ => (bytes, false),
    };
    let declared = source_coding_cookie(bytes);
    let encoding = match &declared {
        Some(cookie) => normalize_source_encoding(cookie),
        None => SourceEncoding::Utf8,
    };
    match encoding {
        SourceEncoding::Unknown => Err(format!("unknown encoding: {}", declared.unwrap_or_default())),
        SourceEncoding::Latin1 | SourceEncoding::Ascii if bom => Err(format!(
            "encoding problem: {} with BOM",
            declared.unwrap_or_default()
        )),
        SourceEncoding::Latin1 => Ok(bytes.iter().map(|&b| char::from(b)).collect()),
        SourceEncoding::Ascii => match bytes.iter().position(|&b| b >= 0x80) {
            None => {
                // All-ASCII bytes are valid UTF-8 by construction.
                Ok(core::str::from_utf8(bytes).unwrap_or_default().to_owned())
            }
            Some(index) => Err(format!(
                "(unicode error) 'ascii' codec can't decode byte 0x{:02x} in position {index}: ordinal not in range(128)",
                bytes[index]
            )),
        },
        SourceEncoding::Utf8 => core::str::from_utf8(bytes).map(str::to_owned).map_err(|error| {
            let index = error.valid_up_to();
            let byte = bytes[index];
            if declared.is_some() {
                format!("(unicode error) 'utf-8' codec can't decode byte 0x{byte:02x} in position {index}: invalid start byte")
            } else {
                let line = bytes[..index].iter().filter(|&&b| b == b'\n').count() + 1;
                format!(
                    "Non-UTF-8 code starting with '\\x{byte:02x}' in file {filename} on line {line}, but no encoding declared; see https://peps.python.org/pep-0263/ for details"
                )
            }
        }),
    }
}

/// Extracts dynamic source text from a `str` or bytes-like argument.
/// `Ok(None)`: the argument is neither (the caller raises its own
/// TypeError).  `Err`: a PEP 263 decode failure, already CPython-shaped for
/// `SyntaxError`.
unsafe fn source_text_arg(object: *mut PyObject, filename: &str) -> Result<Option<String>, String> {
    if let Some(text) = unsafe { str_text(object) } {
        return Ok(Some(text));
    }
    match crate::abi::str_::expect_bytes_like(object) {
        Ok(bytes) => decode_python_source(&bytes, filename).map(Some),
        Err(_) => Ok(None),
    }
}

fn execute_code(code: &PyCodeObject, globals: *mut PyObject, locals: *mut PyObject) -> Result<*mut PyObject, String> {
    let hook = DYN_HOOKS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .execute;
    let Some(hook) = hook else {
        return Err("dynamic code execution is not available in this runtime".to_owned());
    };
    let registered_module = execution_context_module(globals, locals);
    let module_name = registered_module.unwrap_or_else(module_name_for_globals);
    let name_key = intern("__name__");
    let restore_name = registered_module.is_none().then(|| crate::import::module_attr(module_name, name_key)).flatten();
    if unsafe { dict::is_dict(globals) } {
        copy_dict_to_module(globals, module_name)?;
    }
    if locals != globals && unsafe { dict::is_dict(locals) } {
        copy_dict_to_module(locals, module_name)?;
    }
    // Dynamic code always executes through one backing module namespace: a
    // real registered module when `globals`/`locals` are that module's live
    // `__dict__`, else the current active module / `__main__` scratch space.
    // The scratch path preserves today's exec/eval plumbing (the JIT hook
    // reads `active_module_attr("__pon_dyn_eval_result")`) but restores the
    // caller's `__name__` afterward so helpers like `collections.namedtuple`
    // do not permanently clobber the importing module (`decimal.__name__`
    // must stay `decimal`, not `namedtuple_DecimalTuple`).
    let context_module_name = resolve(module_name).filter(|name| crate::import::begin_module_execution(name).is_ok());
    let result = hook(DynExecuteRequest {
        source: &code.source,
        filename: &code.filename,
        mode: code.mode,
        globals,
        locals,
    });
    if let Some(name) = context_module_name {
        crate::import::end_module_execution(&name);
    }
    if unsafe { dict::is_dict(globals) } {
        sync_module_attrs_into_dict(module_name, globals)?;
    }
    if locals != globals && unsafe { dict::is_dict(locals) } {
        sync_module_attrs_into_dict(module_name, locals)?;
    }
    if registered_module.is_none() {
        match restore_name {
            Some(value) => {
                crate::import::store_module_attr(module_name, name_key, value);
            }
            None => {
                crate::import::delete_module_attr(module_name, name_key);
            }
        }
    }
    let result = result?;
    if result.is_null() {
        Err("dynamic code execution returned NULL".to_owned())
    } else {
        Ok(result)
    }
}

fn execution_context_module(globals: *mut PyObject, locals: *mut PyObject) -> Option<u32> {
    binding_for_dict(globals)
        .or_else(|| (locals != globals).then(|| binding_for_dict(locals)).flatten())
        .map(|binding| binding.module_name)
}

fn copy_dict_to_module(dict_object: *mut PyObject, module_name: u32) -> Result<(), String> {
    let entries = unsafe { dict::dict_entries_snapshot(dict_object)? };
    for entry in entries {
        let Some(name) = key_name_id(entry.key) else {
            continue;
        };
        crate::import::store_module_attr(module_name, name, entry.value);
    }
    Ok(())
}


fn namespace_args(args: &[*mut PyObject], name: &str) -> Result<(*mut PyObject, *mut PyObject), String> {
    if args.len() > 3 {
        return Err(format!("{name}() expected at most 3 arguments, got {}", args.len()));
    }
    let globals = if let Some(&globals) = args.get(1) {
        if unsafe { is_none(globals) } {
            module_globals_dict()?
        } else {
            unsafe { require_dict(globals, "globals")? }
        }
    } else {
        module_globals_dict()?
    };
    let locals = if let Some(&locals) = args.get(2) {
        if unsafe { is_none(locals) } {
            globals
        } else {
            unsafe { require_dict(locals, "locals")? }
        }
    } else {
        globals
    };
    Ok((globals, locals))
}

/// CPython `PyCF_ONLY_AST` (`ast.PyCF_ONLY_AST` / `_ast` re-export): compile
/// to an AST object instead of a code object.  With an installed
/// [`DynAstParseHook`] the host parses the source and the `_ast` builder
/// materializes real node trees; without one (AoT products) the typed
/// refusal below stands.
const PYCF_ONLY_AST: i64 = 0x400;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn builtin_compile(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match argv_slice(argv, argc, "compile") {
        Ok(args) => args,
        Err(message) => return return_null_with_error(message),
    };
    // `compile(source, filename, mode, flags=0, dont_inherit=False,
    // optimize=-1, *, _feature_version=-1)`: the keyword binder flattens the
    // full signature to seven slots (absent = NULL); positional callers pass
    // three to six.  `dont_inherit`/`optimize`/`_feature_version` select
    // CPython pipeline variants pon does not model and are accepted unread
    // (`ast.parse` passes their defaults).  Of the remaining flag bits only
    // `PyCF_ONLY_AST` is honored; `PyCF_TYPE_COMMENTS`/`ALLOW_TOP_LEVEL_AWAIT`
    // /`OPTIMIZED_AST` are accepted unread.
    if args.len() < 3 || args.len() > 7 {
        return raise_dyn_type_error(&format!("compile() expected 3 to 6 arguments, got {}", args.len()));
    }
    let Some(filename) = (unsafe { str_text(args[1]) }) else {
        return raise_dyn_type_error("compile() arg 2 must be a string");
    };
    let Some(mode_text) = (unsafe { str_text(args[2]) }) else {
        return raise_dyn_type_error("compile() arg 3 must be a string");
    };
    let source = match unsafe { source_text_arg(args[0], &filename) } {
        Ok(Some(source)) => source,
        Ok(None) => return raise_dyn_type_error("compile() arg 1 must be a string, bytes or AST object"),
        Err(message) => return raise_dyn_syntax_error(&message),
    };
    let flags = match optional_int_arg(args, 3, "flags") {
        Ok(flags) => flags,
        Err(message) => return raise_dyn_type_error(&message),
    };
    let Some(mode) = DynCodeMode::from_str(&mode_text) else {
        return raise_dyn_value_error("compile() mode must be 'exec', 'eval' or 'single'");
    };
    if flags & PYCF_ONLY_AST != 0 {
        return compile_only_ast(&source, &filename, mode);
    }
    match compile_source(source, filename, mode) {
        Ok(code) => code,
        Err(error) => raise_compile_error(error),
    }
}

/// `compile(source, filename, mode, PyCF_ONLY_AST)`: host hook parses to a
/// neutral tree, the `_ast` builder materializes node objects.  Parse
/// failures raise `SyntaxError`; builder failures surface through the
/// standard dynamic-code error path.
fn compile_only_ast(source: &str, filename: &str, mode: DynCodeMode) -> *mut PyObject {
    let hook = {
        let hooks = DYN_HOOKS.lock().unwrap_or_else(|poison| poison.into_inner());
        hooks.ast_parse
    };
    let Some(hook) = hook else {
        const MESSAGE: &str =
            "pon does not support compile() with ast.PyCF_ONLY_AST (ast.parse) in this embedding; only code-object compilation is available";
        return abi::exc::raise_kind_error_text(crate::types::exc::ExceptionKind::NotImplementedError, MESSAGE);
    };
    match hook(DynCompileRequest { source, filename, mode }) {
        Ok(tree) => match crate::native::build_ast_object(&tree) {
            Ok(object) => object,
            Err(message) => return_null_with_error(message),
        },
        Err(message) => abi::exc::raise_kind_error_text(crate::types::exc::ExceptionKind::SyntaxError, &message),
    }
}

/// Reads an optional int slot from a flattened native argv: absent (short
/// argv), NULL (keyword-binder fill), and None all mean "default 0".
fn optional_int_arg(args: &[*mut PyObject], index: usize, name: &str) -> Result<i64, String> {
    let Some(&object) = args.get(index) else {
        return Ok(0);
    };
    if object.is_null() {
        return Ok(0);
    }
    if let Some(value) = int_of(object) {
        return Ok(value);
    }
    if unsafe { is_none(object) } {
        return Ok(0);
    }
    Err(format!("compile() {name} must be an int"))
}

/// Tagged-immediate-aware i64 extraction (the `_collections` idiom).
fn int_of(object: *mut PyObject) -> Option<i64> {
    if crate::tag::is_small_int(object) {
        return Some(crate::tag::untag_small_int(object));
    }
    if object.is_null() {
        return None;
    }
    // SAFETY: Heap pointer with a live header; layout proved by the name check.
    (unsafe { crate::types::dict::type_name(object) } == Some("int"))
        .then(|| unsafe { (*object.cast::<crate::object::PyLong>()).value })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn builtin_eval(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match argv_slice(argv, argc, "eval") {
        Ok(args) => args,
        Err(message) => return return_null_with_error(message),
    };
    if args.is_empty() {
        return raise_dyn_type_error("eval() expected at least 1 argument, got 0");
    }
    let (globals, locals) = match namespace_args(args, "eval") {
        Ok(namespaces) => namespaces,
        Err(message) => return raise_dyn_type_error(&message),
    };
    let code_object = if let Some(code) = unsafe { as_code_object(args[0]) } {
        code
    } else {
        let source = match unsafe { source_text_arg(args[0], "<string>") } {
            Ok(Some(source)) => source,
            Ok(None) => return raise_dyn_type_error("eval() arg 1 must be a string, bytes or code object"),
            Err(message) => return raise_dyn_syntax_error(&message),
        };
        let code = match compile_source(source, "<string>".to_owned(), DynCodeMode::Eval) {
            Ok(code) => code,
            Err(error) => return raise_compile_error(error),
        };
        unsafe { &*code.cast::<PyCodeObject>() }
    };
    match execute_code(code_object, globals, locals) {
        Ok(result) => result,
        Err(message) => return_null_with_error(message),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn builtin_exec(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match argv_slice(argv, argc, "exec") {
        Ok(args) => args,
        Err(message) => return return_null_with_error(message),
    };
    if args.is_empty() {
        return raise_dyn_type_error("exec() expected at least 1 argument, got 0");
    }
    let (globals, locals) = match namespace_args(args, "exec") {
        Ok(namespaces) => namespaces,
        Err(message) => return raise_dyn_type_error(&message),
    };
    let code_object = if let Some(code) = unsafe { as_code_object(args[0]) } {
        code
    } else {
        let source = match unsafe { source_text_arg(args[0], "<string>") } {
            Ok(Some(source)) => source,
            Ok(None) => return raise_dyn_type_error("exec() arg 1 must be a string, bytes or code object"),
            Err(message) => return raise_dyn_syntax_error(&message),
        };
        let code = match compile_source(source, "<string>".to_owned(), DynCodeMode::Exec) {
            Ok(code) => code,
            Err(error) => return raise_compile_error(error),
        };
        unsafe { &*code.cast::<PyCodeObject>() }
    };
    match execute_code(code_object, globals, locals) {
        Ok(_) => unsafe { pon_none() },
        Err(message) => return_null_with_error(message),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn builtin_globals(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match argv_slice(argv, argc, "globals") {
        Ok(args) => args,
        Err(message) => return return_null_with_error(message),
    };
    if !args.is_empty() {
        return return_null_with_error(format!("globals() expected no arguments, got {}", args.len()));
    }
    match module_globals_dict() {
        Ok(dict) => dict,
        Err(message) => return_null_with_error(message),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn builtin_locals(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match argv_slice(argv, argc, "locals") {
        Ok(args) => args,
        Err(message) => return return_null_with_error(message),
    };
    if !args.is_empty() {
        return return_null_with_error(format!("locals() expected no arguments, got {}", args.len()));
    }
    match module_globals_dict() {
        Ok(dict) => dict,
        Err(message) => return_null_with_error(message),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn builtin_dunder_import(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match argv_slice(argv, argc, "__import__") {
        Ok(args) => args,
        Err(message) => return return_null_with_error(message),
    };
    if args.is_empty() || args.len() > 5 {
        return return_null_with_error(format!(
            "__import__() expected 1 to 5 arguments, got {}",
            args.len()
        ));
    }
    let Some(name) = (unsafe { str_text(args[0]) }) else {
        return return_null_with_error("__import__() name must be str");
    };
    let level = match args.get(4) {
        // Keyword binding fills absent optionals with None (CPython default 0).
        Some(&level_object) if unsafe { !is_none(level_object) } => {
            match unsafe { int::to_bigint(level_object) }.and_then(|value| value.to_u32()) {
                Some(level) => level,
                None => return return_null_with_error("__import__() level must be int"),
            }
        }
        _ => 0,
    };
    let mut fromlist_names = Vec::new();
    if let Some(&fromlist) = args.get(3) {
        if unsafe { !is_none(fromlist) } {
            collect_fromlist_names(fromlist, &mut fromlist_names);
        }
    }
    let name_id = intern(&name);
    unsafe { crate::import::pon_import_name(name_id, fromlist_names.as_ptr(), fromlist_names.len(), level) }
}

fn collect_fromlist_names(fromlist: *mut PyObject, out: &mut Vec<u32>) {
    if fromlist.is_null() {
        return;
    }
    if unsafe { int::type_name_is(fromlist, "str") } {
        if let Some(text) = unsafe { str_text(fromlist) } {
            out.push(intern(&text));
        }
        return;
    }
    let iter = unsafe { abi::pon_get_iter(fromlist, ptr::null_mut()) };
    if iter.is_null() {
        return;
    }
    loop {
        let item = unsafe { abi::pon_iter_next(iter, ptr::null_mut()) };
        if item.is_null() {
            break;
        }
        if let Some(text) = unsafe { str_text(item) } {
            out.push(intern(&text));
        }
    }
}
