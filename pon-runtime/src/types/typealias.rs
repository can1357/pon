//! Runtime PEP 695 objects: `TypeAliasType`, `TypeVar`, and `GenericAlias`.
//!
//! `type X = expr` lowers to a zero-argument value thunk plus
//! [`pon_make_type_alias`]; the alias evaluates `expr` lazily on first
//! `__value__` access and caches the result (CPython 3.14 semantics).
//! `def f[T](...)` binds `T` through [`pon_make_typevar`] inside synthesized
//! annotate/alias scopes.  `GenericAlias` carries `origin[args]` subscript
//! results (`list[int]`) produced by the builtin-constructor subscript
//! fallback in `abstract_op::subscript_get`.

use core::mem::{offset_of, size_of};
use core::ptr;
use std::sync::LazyLock;

use crate::intern::resolve;
use crate::object::{as_object_ptr, PyFunction, PyObject, PyObjectHeader, PyType, PyUnicode};
use crate::thread_state::pon_err_set;

/// Runtime object for Python 3.12+ `type X = ...` aliases.
///
/// The evaluated value is computed lazily: `thunk` is a zero-argument
/// synthesized function evaluating the alias body, and `value` caches its
/// first result.  This mirrors CPython's `TypeAliasType.__value__` laziness
/// (forward references in the alias body resolve at access time, not at the
/// `type` statement).
#[repr(C)]
#[derive(Debug)]
pub struct PyTypeAlias {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Interned alias name.
    pub name_interned: u32,
    /// Zero-argument value thunk, or NULL for eagerly-built aliases.
    pub thunk: *mut PyObject,
    /// Cached evaluated value, or NULL until first `__value__` access.
    pub value: *mut PyObject,
}

impl PyTypeAlias {
    /// Builds a type-alias payload for an allocated object slot.
    #[must_use]
    pub const fn new(ty: *const PyType, name_interned: u32, thunk: *mut PyObject) -> Self {
        Self {
            ob_base: PyObjectHeader::new(ty),
            name_interned,
            thunk,
            value: ptr::null_mut(),
        }
    }
}

/// Minimal PEP 695 `TypeVar`: an interned name with CPython's bare-name repr.
#[repr(C)]
#[derive(Debug)]
pub struct PyTypeVar {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Interned type-parameter name (`T`).
    pub name_interned: u32,
}

/// Minimal `types.GenericAlias`: `origin[args]` (`list[int]`).
///
/// The payload fields are Rust-only (never read through the C ABI), so the
/// `Vec` behind `repr(C)` is acceptable; only `ob_base` has a layout contract.
#[repr(C)]
#[derive(Debug)]
pub struct PyGenericAlias {
    /// Common object header; this field must remain first.
    pub ob_base: PyObjectHeader,
    /// Subscripted constructor (`list` in `list[int]`).
    pub origin: *mut PyObject,
    /// Subscript arguments, tuple-flattened (`[str, int]` in `dict[str, int]`).
    pub args: Vec<*mut PyObject>,
}

fn resolved_name(name_interned: u32) -> String {
    resolve(name_interned).unwrap_or_else(|| format!("<interned:{name_interned}>"))
}

unsafe fn attribute_name(name: *mut PyObject) -> Option<&'static str> {
    if name.is_null() {
        return None;
    }
    unsafe { (&*name.cast::<PyUnicode>()).as_str() }
}

fn raise_attr(message: String) -> *mut PyObject {
    pon_err_set(message);
    ptr::null_mut()
}

/// Returns the process-lifetime `TypeAliasType` descriptor.
///
/// Named `typing.TypeAliasType` so `print(type(X))` matches CPython
/// (`<class 'typing.TypeAliasType'>`).
#[must_use]
pub fn type_alias_type(type_type: *const PyType) -> *mut PyType {
    let _ = type_type;
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(core::ptr::null(), "typing.TypeAliasType", size_of::<PyTypeAlias>());
        ty.tp_getattro = Some(type_alias_getattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    *TYPE as *mut PyType
}

/// Returns the process-lifetime `TypeVar` descriptor.
#[must_use]
pub fn typevar_type() -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(core::ptr::null(), "TypeVar", size_of::<PyTypeVar>());
        ty.tp_getattro = Some(typevar_getattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    *TYPE as *mut PyType
}

/// Returns the process-lifetime `types.GenericAlias` descriptor.
#[must_use]
pub fn generic_alias_type() -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(core::ptr::null(), "types.GenericAlias", size_of::<PyGenericAlias>());
        ty.tp_getattro = Some(generic_alias_getattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    *TYPE as *mut PyType
}

/// True when `object` is a boxed `PyTypeAlias`.
#[must_use]
pub fn is_type_alias(object: *mut PyObject) -> bool {
    !object.is_null() && unsafe { (*object).ob_type } == type_alias_type(ptr::null()).cast_const()
}

/// True when `object` is a boxed `PyTypeVar`.
#[must_use]
pub fn is_typevar(object: *mut PyObject) -> bool {
    !object.is_null() && unsafe { (*object).ob_type } == typevar_type().cast_const()
}

/// True when `object` is a boxed `PyGenericAlias`.
#[must_use]
pub fn is_generic_alias(object: *mut PyObject) -> bool {
    !object.is_null() && unsafe { (*object).ob_type } == generic_alias_type().cast_const()
}

/// Allocates a boxed `TypeAliasType` with a lazy value thunk.
///
/// The object is leaked intentionally: aliases are module-lifetime objects and
/// the runtime has no registered GC family for them (same accepted pattern as
/// the function metadata side tables).
#[must_use]
pub fn new_type_alias(name_interned: u32, thunk: *mut PyObject, type_type: *const PyType) -> *mut PyObject {
    let ty = type_alias_type(type_type);
    as_object_ptr(Box::into_raw(Box::new(PyTypeAlias::new(ty.cast_const(), name_interned, thunk))))
}

/// Allocates a boxed minimal `TypeVar`.
#[must_use]
pub fn new_typevar(name_interned: u32) -> *mut PyObject {
    let object = Box::new(PyTypeVar {
        ob_base: PyObjectHeader::new(typevar_type().cast_const()),
        name_interned,
    });
    as_object_ptr(Box::into_raw(object))
}

/// Allocates a boxed `GenericAlias` for `origin[args]`.
#[must_use]
pub fn new_generic_alias(origin: *mut PyObject, args: Vec<*mut PyObject>) -> *mut PyObject {
    let object = Box::new(PyGenericAlias {
        ob_base: PyObjectHeader::new(generic_alias_type().cast_const()),
        origin,
        args,
    });
    as_object_ptr(Box::into_raw(object))
}

/// C ABI constructor for `InstKind::MakeTypeAlias` (`type X = expr`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_make_type_alias(name_interned: u32, thunk: *mut PyObject) -> *mut PyObject {
    if thunk.is_null() {
        pon_err_set("type alias thunk is NULL");
        return ptr::null_mut();
    }
    new_type_alias(name_interned, thunk, core::ptr::null())
}

/// C ABI constructor for `InstKind::MakeTypeVar` (`def f[T](...)`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_make_typevar(name_interned: u32) -> *mut PyObject {
    new_typevar(name_interned)
}

/// Lazily evaluates and caches the alias value (`X.__value__`).
pub unsafe fn type_alias_value(alias: *mut PyObject) -> *mut PyObject {
    let alias = alias.cast::<PyTypeAlias>();
    let cached = unsafe { (*alias).value };
    if !cached.is_null() {
        return cached;
    }
    let thunk = unsafe { (*alias).thunk };
    if thunk.is_null() {
        return raise_attr("type alias has no value thunk".to_owned());
    }
    let value = unsafe { crate::abi::pon_call(thunk, ptr::null_mut(), 0) };
    if value.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        (*alias).value = value;
    }
    value
}

unsafe extern "C" fn type_alias_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { attribute_name(name) }) else {
        return raise_attr("type alias attribute name must be str".to_owned());
    };
    match name_text {
        "__name__" => {
            let text = resolved_name(unsafe { (*object.cast::<PyTypeAlias>()).name_interned });
            unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) }
        }
        "__value__" => unsafe { type_alias_value(object) },
        _ => raise_attr(format!("'typing.TypeAliasType' object has no attribute '{name_text}'")),
    }
}

unsafe extern "C" fn typevar_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { attribute_name(name) }) else {
        return raise_attr("TypeVar attribute name must be str".to_owned());
    };
    match name_text {
        "__name__" => {
            let text = resolved_name(unsafe { (*object.cast::<PyTypeVar>()).name_interned });
            unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) }
        }
        _ => raise_attr(format!("'TypeVar' object has no attribute '{name_text}'")),
    }
}

unsafe extern "C" fn generic_alias_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { attribute_name(name) }) else {
        return raise_attr("GenericAlias attribute name must be str".to_owned());
    };
    let alias = unsafe { &*object.cast::<PyGenericAlias>() };
    match name_text {
        "__origin__" => alias.origin,
        "__args__" => crate::native::builtins_mod::alloc_tuple(alias.args.clone()),
        _ => raise_attr(format!("'types.GenericAlias' object has no attribute '{name_text}'")),
    }
}

/// Bare alias name used by `repr(X)`/`print(X)` (CPython: `repr(X) == 'X'`).
#[must_use]
pub fn type_alias_repr(object: *mut PyObject) -> String {
    resolved_name(unsafe { (*object.cast::<PyTypeAlias>()).name_interned })
}

/// Bare parameter name used by `repr(T)` (CPython: `repr(T) == 'T'`).
#[must_use]
pub fn typevar_repr(object: *mut PyObject) -> String {
    resolved_name(unsafe { (*object.cast::<PyTypeVar>()).name_interned })
}

/// `origin[arg, ...]` repr matching CPython's `types.GenericAlias`
/// (`repr(list[int]) == 'list[int]'`: type-ish args render as bare names).
#[must_use]
pub fn generic_alias_repr(object: *mut PyObject) -> String {
    let alias = unsafe { &*object.cast::<PyGenericAlias>() };
    let args = alias
        .args
        .iter()
        .copied()
        .map(generic_arg_text)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{}[{args}]", generic_arg_text(alias.origin))
}

/// Formats one subscript argument the way CPython prints generic parameters:
/// classes and constructor functions as bare names, everything else as repr.
fn generic_arg_text(arg: *mut PyObject) -> String {
    if arg.is_null() {
        return "<NULL>".to_owned();
    }
    if is_typevar(arg) {
        return typevar_repr(arg);
    }
    if is_type_alias(arg) {
        return type_alias_repr(arg);
    }
    if is_generic_alias(arg) {
        return generic_alias_repr(arg);
    }
    unsafe {
        let ty = (*arg).ob_type;
        if !ty.is_null() {
            let ty_name = (*ty).name();
            if ty_name == "type" {
                return (*arg.cast::<PyType>()).name().to_owned();
            }
            if ty_name == "function" {
                // pon builtin constructors (`int`, `str`, `list`, ...) are
                // native functions; render their bare name like a class.
                let function = &*arg.cast::<PyFunction>();
                return resolved_name(function.name_interned);
            }
        }
    }
    crate::native::builtins_mod::repr_text(arg)
}

/// Constructor names accepted by the builtin subscript fallback
/// (`list[int]`, `dict[str, int]`, ...).  pon builtins are `PyFunction`
/// objects, not `PyType`s, so plain `mp_subscript` dispatch never fires.
#[must_use]
pub fn is_subscriptable_builtin_constructor(name: &str) -> bool {
    matches!(name, "list" | "dict" | "tuple" | "set" | "frozenset" | "type" | "int" | "str" | "float" | "bool" | "bytes")
}

const _: () = {
    assert!(offset_of!(PyTypeAlias, ob_base) == 0);
    assert!(offset_of!(PyTypeVar, ob_base) == 0);
    assert!(offset_of!(PyGenericAlias, ob_base) == 0);
};
