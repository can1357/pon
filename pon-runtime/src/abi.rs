//! C ABI helpers exported by the Phase-A runtime.
//!
//! The [`HELPERS`] table is the single source of truth for later codegen and JIT
//! import declarations: symbol names, Rust entrypoint addresses, parameter
//! shapes, and return types all live here.

use std::collections::HashMap;
use std::io::{self, Write};
use std::mem;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::{LazyLock, Mutex, MutexGuard};

use pon_gc::{GcTypeInfo, Heap, RootSource, TypeId};

use crate::builtins;
use crate::intern::resolve;
use crate::object::{PyCodeFn, PyFunction, PyLong, PyNone, PyObject, PyObjectHeader, PyType, PyUnicode, as_object_ptr, is_exact_type};
use crate::thread_state::{pon_err_clear, pon_err_occurred, pon_err_set, thread_state_lock};

const TYPE_ID_TYPE: TypeId = TypeId(1);
const TYPE_ID_LONG: TypeId = TypeId(2);
const TYPE_ID_UNICODE: TypeId = TypeId(3);
const TYPE_ID_FUNCTION: TypeId = TypeId(4);
const TYPE_ID_NONE: TypeId = TypeId(5);

/// ABI-level type descriptor used by [`HelperDecl`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbiTy {
    /// C `i32`.
    I32,
    /// C/Rust `i64`.
    I64,
    /// C/Rust `u32`.
    U32,
    /// C/Rust `usize`.
    Usize,
    /// `*const u8`.
    ConstU8Ptr,
    /// `*mut PyObject`.
    PyObjectPtr,
    /// `*mut *mut PyObject`.
    PyObjectPtrPtr,
}

/// One exported helper declaration for codegen and JIT import binding.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct HelperDecl {
    /// Exact exported symbol name.
    pub symbol: &'static str,
    /// Runtime entrypoint address.
    pub address: *const (),
    /// Parameter ABI types in call order.
    pub params: &'static [AbiTy],
    /// Return ABI type.
    pub ret: AbiTy,
}

unsafe impl Sync for HelperDecl {}

const PARAMS_CONST_INT: &[AbiTy] = &[AbiTy::I64];
const PARAMS_CONST_STR: &[AbiTy] = &[AbiTy::ConstU8Ptr, AbiTy::Usize];
const PARAMS_BINARY_ADD: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::PyObjectPtr];
const PARAMS_CALL: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::PyObjectPtrPtr, AbiTy::Usize];
const PARAMS_LOAD_GLOBAL: &[AbiTy] = &[AbiTy::U32];
const PARAMS_PRINT: &[AbiTy] = &[AbiTy::PyObjectPtr];
const PARAMS_MAKE_FUNCTION: &[AbiTy] = &[AbiTy::ConstU8Ptr, AbiTy::Usize, AbiTy::U32];
const PARAMS_STORE_GLOBAL: &[AbiTy] = &[AbiTy::U32, AbiTy::PyObjectPtr];
const PARAMS_NONE: &[AbiTy] = &[];
const PARAMS_RUNTIME_INIT: &[AbiTy] = &[];

/// Exported helper table consumed by later codegen/JIT stages.
pub static HELPERS: &[HelperDecl] = &[
    HelperDecl {
        symbol: "pon_const_int",
        address: pon_const_int as *const (),
        params: PARAMS_CONST_INT,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_const_str",
        address: pon_const_str as *const (),
        params: PARAMS_CONST_STR,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_binary_add",
        address: pon_binary_add as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_call",
        address: pon_call as *const (),
        params: PARAMS_CALL,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_load_global",
        address: pon_load_global as *const (),
        params: PARAMS_LOAD_GLOBAL,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_print",
        address: pon_print as *const (),
        params: PARAMS_PRINT,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_make_function",
        address: pon_make_function as *const (),
        params: PARAMS_MAKE_FUNCTION,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_store_global",
        address: pon_store_global as *const (),
        params: PARAMS_STORE_GLOBAL,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_none",
        address: pon_none as *const (),
        params: PARAMS_NONE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_runtime_init",
        address: pon_runtime_init as *const (),
        params: PARAMS_RUNTIME_INIT,
        ret: AbiTy::I32,
    },
];

struct Runtime {
    heap: Heap,
    _type_type: *mut PyType,
    long_type: *mut PyType,
    unicode_type: *mut PyType,
    function_type: *mut PyType,
    none_type: *mut PyType,
    none: *mut PyNone,
    globals: HashMap<u32, *mut PyObject>,
}

unsafe impl Send for Runtime {}

static RUNTIME: LazyLock<Mutex<Option<Runtime>>> = LazyLock::new(|| Mutex::new(None));

fn runtime_lock() -> MutexGuard<'static, Option<Runtime>> {
    RUNTIME.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn with_runtime<T>(f: impl FnOnce(&mut Runtime) -> T) -> Option<T> {
    let mut runtime = runtime_lock();
    runtime.as_mut().map(f)
}

fn init_runtime() -> Result<(), String> {
    let mut slot = runtime_lock();
    if slot.is_some() {
        return Ok(());
    }

    let heap = Heap::new();
    register_gc_types(&heap);

    let type_type = Box::into_raw(Box::new(PyType::new(ptr::null(), "type", mem::size_of::<PyType>())));
    let long_type = Box::into_raw(Box::new(PyType::new(type_type, "int", mem::size_of::<PyLong>())));
    let unicode_type = Box::into_raw(Box::new(PyType::new(type_type, "str", mem::size_of::<PyUnicode>())));
    let function_type = Box::into_raw(Box::new(PyType::new(type_type, "function", mem::size_of::<PyFunction>())));
    let none_type = Box::into_raw(Box::new(PyType::new(type_type, "NoneType", mem::size_of::<PyNone>())));

    // SAFETY: The leaked type object remains valid for the process lifetime.
    unsafe {
        (*type_type).ob_base.ob_type = type_type;
    }

    let none = heap.alloc(mem::size_of::<PyNone>(), TYPE_ID_NONE).cast::<PyNone>();
    // SAFETY: `none` points to a freshly allocated zeroed block of the right size.
    unsafe {
        ptr::write(
            none,
            PyNone {
                ob_base: PyObjectHeader::new(none_type),
            },
        );
    }

    let mut runtime = Runtime {
        heap,
        _type_type: type_type,
        long_type,
        unicode_type,
        function_type,
        none_type,
        none,
        globals: HashMap::new(),
    };

    register_builtins(&mut runtime)?;
    *slot = Some(runtime);
    Ok(())
}

fn register_gc_types(heap: &Heap) {
    heap.register_type(
        TYPE_ID_TYPE,
        GcTypeInfo {
            size: mem::size_of::<PyType>(),
            trace: trace_no_refs,
            finalize: None,
        },
    );
    heap.register_type(
        TYPE_ID_LONG,
        GcTypeInfo {
            size: mem::size_of::<PyLong>(),
            trace: trace_no_refs,
            finalize: None,
        },
    );
    heap.register_type(
        TYPE_ID_UNICODE,
        GcTypeInfo {
            size: mem::size_of::<PyUnicode>(),
            trace: trace_no_refs,
            finalize: Some(finalize_unicode),
        },
    );
    heap.register_type(
        TYPE_ID_FUNCTION,
        GcTypeInfo {
            size: mem::size_of::<PyFunction>(),
            trace: trace_no_refs,
            finalize: None,
        },
    );
    heap.register_type(
        TYPE_ID_NONE,
        GcTypeInfo {
            size: mem::size_of::<PyNone>(),
            trace: trace_no_refs,
            finalize: None,
        },
    );
}

unsafe extern "C" fn trace_no_refs(_object: *mut u8, _visitor: &mut dyn FnMut(*mut u8)) {}

unsafe extern "C" fn finalize_unicode(object: *mut u8) {
    if object.is_null() {
        return;
    }

    // SAFETY: The GC calls this only for live allocations registered as PyUnicode.
    let unicode = unsafe { &mut *object.cast::<PyUnicode>() };
    if unicode.owns_data && !unicode.data.is_null() {
        let data = unicode.data.cast_mut();
        let len = unicode.len;
        unicode.data = ptr::null();
        unicode.len = 0;
        unicode.owns_data = false;
        let slice = ptr::slice_from_raw_parts_mut(data, len);
        // SAFETY: Owned unicode data is created by `Box<[u8]>::into_raw`.
        unsafe {
            drop(Box::<[u8]>::from_raw(slice));
        }
    }
}

fn register_builtins(runtime: &mut Runtime) -> Result<(), String> {
    let name = builtins::print_name_interned();
    let function = alloc_function(runtime, builtins::print_trampoline as *const u8, 1, name)?;
    runtime.globals.insert(name, function);
    Ok(())
}

fn alloc_long(runtime: &Runtime, value: i64) -> Result<*mut PyObject, String> {
    let object = runtime.heap.alloc(mem::size_of::<PyLong>(), TYPE_ID_LONG).cast::<PyLong>();
    // SAFETY: `object` points to a freshly allocated zeroed block of the right size.
    unsafe {
        ptr::write(
            object,
            PyLong {
                ob_base: PyObjectHeader::new(runtime.long_type),
                value,
            },
        );
    }
    Ok(as_object_ptr(object))
}

fn alloc_unicode(runtime: &Runtime, bytes: &[u8]) -> Result<*mut PyObject, String> {
    if core::str::from_utf8(bytes).is_err() {
        return Err("string constant is not valid UTF-8".to_owned());
    }

    let owned = bytes.to_vec().into_boxed_slice();
    let len = owned.len();
    let data = Box::into_raw(owned).cast::<u8>();
    let object = runtime.heap.alloc(mem::size_of::<PyUnicode>(), TYPE_ID_UNICODE).cast::<PyUnicode>();
    // SAFETY: `object` points to a freshly allocated zeroed block of the right size.
    unsafe {
        ptr::write(
            object,
            PyUnicode {
                ob_base: PyObjectHeader::new(runtime.unicode_type),
                len,
                data,
                owns_data: true,
            },
        );
    }
    Ok(as_object_ptr(object))
}

fn alloc_function(runtime: &Runtime, code: *const u8, arity: usize, name_interned: u32) -> Result<*mut PyObject, String> {
    if code.is_null() {
        return Err("function code pointer is null".to_owned());
    }

    let object = runtime.heap.alloc(mem::size_of::<PyFunction>(), TYPE_ID_FUNCTION).cast::<PyFunction>();
    // SAFETY: `object` points to a freshly allocated zeroed block of the right size.
    unsafe {
        ptr::write(
            object,
            PyFunction {
                ob_base: PyObjectHeader::new(runtime.function_type),
                code,
                arity,
                name_interned,
            },
        );
    }
    Ok(as_object_ptr(object))
}

fn ensure_runtime_initialized() -> Result<(), String> {
    init_runtime()
}

fn null_with_error(message: impl Into<String>) -> *mut PyObject {
    pon_err_set(message);
    ptr::null_mut()
}

fn catch_object_helper(f: impl FnOnce() -> *mut PyObject) -> *mut PyObject {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(_) => null_with_error("runtime helper panicked"),
    }
}

/// Creates a boxed Phase-A integer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_const_int(value: i64) -> *mut PyObject {
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return null_with_error(message);
        }
        match with_runtime(|runtime| alloc_long(runtime, value)) {
            Some(Ok(object)) => object,
            Some(Err(message)) => null_with_error(message),
            None => null_with_error("runtime is not initialized"),
        }
    })
}

/// Creates a boxed Phase-A UTF-8 string from raw bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_const_str(ptr: *const u8, len: usize) -> *mut PyObject {
    catch_object_helper(|| {
        if ptr.is_null() && len != 0 {
            return null_with_error("string pointer is null");
        }
        if let Err(message) = ensure_runtime_initialized() {
            return null_with_error(message);
        }
        let bytes = if len == 0 {
            &[]
        } else {
            // SAFETY: The caller supplies `len` bytes at non-null `ptr`.
            unsafe { core::slice::from_raw_parts(ptr, len) }
        };
        match with_runtime(|runtime| alloc_unicode(runtime, bytes)) {
            Some(Ok(object)) => object,
            Some(Err(message)) => null_with_error(message),
            None => null_with_error("runtime is not initialized"),
        }
    })
}

/// Adds two boxed Phase-A integers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_binary_add(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return null_with_error(message);
        }
        with_runtime(|runtime| {
            // SAFETY: Type checks below ensure both casts are exact PyLong casts.
            unsafe {
                if !is_exact_type(a, runtime.long_type) || !is_exact_type(b, runtime.long_type) {
                    return null_with_error("unsupported operands for +");
                }
                let left = (*a.cast::<PyLong>()).value;
                let right = (*b.cast::<PyLong>()).value;
                match left.checked_add(right) {
                    Some(sum) => match alloc_long(runtime, sum) {
                        Ok(object) => object,
                        Err(message) => null_with_error(message),
                    },
                    None => null_with_error("integer addition overflow"),
                }
            }
        })
        .unwrap_or_else(|| null_with_error("runtime is not initialized"))
    })
}

/// Calls a boxed `PyFunction`, enforcing its Phase-A positional arity.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_call(callee: *mut PyObject, argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return null_with_error(message);
        }
        let (code, arity) = match with_runtime(|runtime| {
            // SAFETY: Type check below ensures `callee` is a PyFunction.
            unsafe {
                if !is_exact_type(callee, runtime.function_type) {
                    return Err("callee is not callable".to_owned());
                }
                let function = &*callee.cast::<PyFunction>();
                Ok((function.code, function.arity))
            }
        }) {
            Some(Ok(pair)) => pair,
            Some(Err(message)) => return null_with_error(message),
            None => return null_with_error("runtime is not initialized"),
        };

        if arity != argc {
            return null_with_error(format!("function expected {arity} arguments, got {argc}"));
        }
        if argv.is_null() && argc != 0 {
            return null_with_error("argv pointer is null");
        }
        if code.is_null() {
            return null_with_error("function code pointer is null");
        }

        pon_err_clear();
        // SAFETY: `PyFunction::code` is created from a `PyCodeFn` entrypoint.
        let entry: PyCodeFn = unsafe { mem::transmute(code) };
        // SAFETY: The entrypoint follows the Phase-A compiled function ABI.
        let result = unsafe { entry(argv, argc) };
        if result.is_null() && !pon_err_occurred() {
            return null_with_error("call returned NULL without setting an exception");
        }
        result
    })
}

/// Loads a module-global or builtin value by interned name.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_load_global(name_interned: u32) -> *mut PyObject {
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return null_with_error(message);
        }
        with_runtime(|runtime| runtime.globals.get(&name_interned).copied())
            .flatten()
            .unwrap_or_else(|| {
                let name = resolve(name_interned).unwrap_or_else(|| format!("<interned:{name_interned}>"));
                null_with_error(format!("name '{name}' is not defined"))
            })
    })
}

/// Prints a boxed Phase-A value followed by a newline and returns immortal `None`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_print(value: *mut PyObject) -> *mut PyObject {
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return null_with_error(message);
        }
        let text = match format_object_for_print(value) {
            Ok(text) => text,
            Err(message) => return null_with_error(message),
        };
        let mut stdout = io::stdout().lock();
        if let Err(error) = writeln!(stdout, "{text}").and_then(|()| stdout.flush()) {
            return null_with_error(format!("failed to write stdout: {error}"));
        }
        // SAFETY: `pon_none` returns the initialized immortal singleton.
        unsafe { pon_none() }
    })
}

/// Creates a boxed function object from a compiled entrypoint address.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_make_function(code: *const u8, arity: usize, name_interned: u32) -> *mut PyObject {
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return null_with_error(message);
        }
        match with_runtime(|runtime| alloc_function(runtime, code, arity, name_interned)) {
            Some(Ok(object)) => object,
            Some(Err(message)) => null_with_error(message),
            None => null_with_error("runtime is not initialized"),
        }
    })
}

/// Stores a module-global value by interned name.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_store_global(name_interned: u32, value: *mut PyObject) -> *mut PyObject {
    catch_object_helper(|| {
        if value.is_null() {
            return null_with_error("cannot store NULL global value");
        }
        if let Err(message) = ensure_runtime_initialized() {
            return null_with_error(message);
        }
        match with_runtime(|runtime| {
            runtime.globals.insert(name_interned, value);
            value
        }) {
            Some(stored) => stored,
            None => null_with_error("runtime is not initialized"),
        }
    })
}

/// Returns the immortal `None` singleton.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_none() -> *mut PyObject {
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return null_with_error(message);
        }
        with_runtime(|runtime| as_object_ptr(runtime.none)).unwrap_or_else(|| null_with_error("runtime is not initialized"))
    })
}

/// Idempotently initializes the runtime heap, type table, singletons, and builtins.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_runtime_init() -> i32 {
    match catch_unwind(AssertUnwindSafe(init_runtime)) {
        Ok(Ok(())) => {
            pon_err_clear();
            0
        }
        Ok(Err(message)) => {
            pon_err_set(message);
            -1
        }
        Err(_) => {
            pon_err_set("runtime initialization panicked");
            -1
        }
    }
}

/// Converts a boxed value to the exact text used by `pon_print`.
#[must_use]
pub fn format_object_for_print(value: *mut PyObject) -> Result<String, String> {
    if value.is_null() {
        return Err("cannot print NULL object".to_owned());
    }

    with_runtime(|runtime| {
        // SAFETY: The type checks ensure exact concrete casts.
        unsafe {
            if is_exact_type(value, runtime.long_type) {
                return Ok((*value.cast::<PyLong>()).value.to_string());
            }
            if is_exact_type(value, runtime.unicode_type) {
                let unicode = &*value.cast::<PyUnicode>();
                return unicode
                    .as_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| "unicode object contains invalid UTF-8".to_owned());
            }
            if is_exact_type(value, runtime.none_type) {
                return Ok("None".to_owned());
            }
            let ty = (*value).ob_type;
            if ty.is_null() {
                return Err("object has null type".to_owned());
            }
            Err(format!("cannot print object of type {}", (*ty).name()))
        }
    })
    .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
}

struct LocalRoots {
    roots: Vec<*mut u8>,
}

impl RootSource for LocalRoots {
    fn for_each_root(&mut self, visitor: &mut dyn FnMut(*mut u8)) {
        for root in self.roots.iter().copied() {
            visitor(root);
        }
    }
}

/// Runs a stop-the-world collection using the runtime's current root set.
pub fn collect() -> Result<(), String> {
    let mut slot = runtime_lock();
    let Some(runtime) = slot.as_mut() else {
        return Err("runtime is not initialized".to_owned());
    };

    let mut roots = Vec::with_capacity(runtime.globals.len() + 2);
    roots.push(runtime.none.cast::<u8>());
    for value in runtime.globals.values().copied() {
        roots.push(value.cast::<u8>());
    }

    {
        let state = thread_state_lock();
        if !state.current_exc.is_null() {
            roots.push(state.current_exc.cast::<u8>());
        }
        for value in state.frame_stack.iter().copied() {
            roots.push(value.cast::<u8>());
        }
    }

    let mut roots = LocalRoots { roots };
    runtime.heap.collect(&mut roots);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::intern;

    unsafe extern "C" fn return_none(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
        unsafe { pon_none() }
    }

    #[test]
    fn runtime_init_is_idempotent() {
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            assert_eq!(pon_runtime_init(), 0);
            assert!(!pon_none().is_null());
        }
    }

    #[test]
    fn int_addition_returns_boxed_long() {
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            let a = pon_const_int(2);
            let b = pon_const_int(40);
            let sum = pon_binary_add(a, b);
            assert!(!sum.is_null());
            assert_eq!(format_object_for_print(sum).as_deref(), Ok("42"));
        }
    }

    #[test]
    fn global_store_and_load_round_trip() {
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            let name = intern("answer");
            let value = pon_const_int(42);
            assert_eq!(pon_store_global(name, value), value);
            assert_eq!(pon_load_global(name), value);
        }
    }

    #[test]
    fn make_function_and_call_enforce_arity() {
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            let function = pon_make_function(return_none as *const u8, 0, intern("return_none"));
            assert!(!function.is_null());
            assert_eq!(pon_call(function, ptr::null_mut(), 0), pon_none());
            assert!(pon_call(function, ptr::null_mut(), 1).is_null());
            assert!(pon_err_occurred());
        }
    }

    #[test]
    fn print_conversion_formats_unicode_and_int() {
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            let string = pon_const_str(b"hello".as_ptr(), 5);
            let integer = pon_const_int(-7);
            assert_eq!(format_object_for_print(string).as_deref(), Ok("hello"));
            assert_eq!(format_object_for_print(integer).as_deref(), Ok("-7"));
        }
    }
}
