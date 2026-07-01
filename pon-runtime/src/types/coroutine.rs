//! Coroutine object implementation.
//!
//! Coroutines share the same stackless payload as generators; the public type
//! module owns the coroutine type descriptor and `__await__` behavior.

use core::mem;
use std::sync::{LazyLock, Mutex};

use pon_gc::TypeId;

use crate::object::{PyAsyncMethods, PyObject, PyType};
use crate::types::generator::{PyGenerator, generator_am_send, generator_iter, generator_next};

/// GC type id reserved for coroutine objects in the WS-GEN family.
pub const TYPE_ID_COROUTINE: TypeId = TypeId(31);

/// Coroutines use the generator payload with `kind == GeneratorKind::Coroutine`.
pub type PyCoroutine = PyGenerator;

static COROUTINE_TYPE: LazyLock<Mutex<Option<usize>>> = LazyLock::new(|| Mutex::new(None));
static COROUTINE_ASYNC_METHODS: LazyLock<Mutex<Option<usize>>> = LazyLock::new(|| Mutex::new(None));

/// Returns the process-lifetime coroutine type object, creating it if needed.
pub fn ensure_coroutine_type(type_type: *mut PyType) -> *mut PyType {
    let mut slot = COROUTINE_TYPE.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(ptr) = *slot {
        return ptr as *mut PyType;
    }

    let async_methods = ensure_coroutine_async_methods();
    let mut ty = PyType::new(type_type.cast_const(), "coroutine", mem::size_of::<PyCoroutine>());
    ty.tp_iter = Some(generator_iter);
    ty.tp_iternext = Some(generator_next);
    ty.tp_as_async = async_methods;
    ty.gc_type_id = TYPE_ID_COROUTINE.0 as usize;
    let ptr = Box::into_raw(Box::new(ty));
    *slot = Some(ptr as usize);
    ptr
}

fn ensure_coroutine_async_methods() -> *mut PyAsyncMethods {
    let mut slot = COROUTINE_ASYNC_METHODS.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(ptr) = *slot {
        return ptr as *mut PyAsyncMethods;
    }
    let mut methods = PyAsyncMethods::EMPTY;
    methods.am_await = Some(coroutine_await);
    methods.am_send = Some(generator_am_send);
    let ptr = Box::into_raw(Box::new(methods));
    *slot = Some(ptr as usize);
    ptr
}

/// Implements `coro.__await__()` for the baseline coroutine object.
///
/// # Safety
/// `object` must be a boxed coroutine object.
pub unsafe extern "C" fn coroutine_await(object: *mut PyObject) -> *mut PyObject {
    object
}
