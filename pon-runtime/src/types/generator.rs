//! Generator object implementation.
//!
//! The runtime object is intentionally stackless: all suspended execution state
//! lives in the heap `PyFrame`, and resumption is a normal call to `resume`.

use core::ffi::c_int;
use core::mem;
use core::ptr;
use std::sync::{LazyLock, Mutex};

use pon_gc::TypeId;

use crate::abi::{PyFrame, r#gen::GenResumeFn};
use crate::intern;
use crate::object::{GetAttrFunc, PyAsyncMethods, PyObject, PyObjectHeader, PyType, SendFunc, as_object_ptr};
use crate::types::{method, type_};
use crate::types::frame::FRAME_STATE_EXHAUSTED;

/// GC type id reserved for generator objects in the WS-GEN family.
pub const TYPE_ID_GENERATOR: TypeId = TypeId(30);

/// Generator-kind discriminator stored in [`PyGenerator::kind`].
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeneratorKind {
    /// A synchronous Python generator.
    Generator = 0,
    /// A coroutine object produced by `async def`.
    Coroutine = 1,
    /// An asynchronous generator.
    AsyncGenerator = 2,
}

impl GeneratorKind {
    /// Converts the ABI byte into a known kind.
    #[must_use]
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Generator),
            1 => Some(Self::Coroutine),
            2 => Some(Self::AsyncGenerator),
            _ => None,
        }
    }

    /// Returns the ABI byte for this kind.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Boxed Python generator/coroutine payload.
#[repr(C)]
#[derive(Debug)]
pub struct PyGenerator {
    /// Standard boxed-object header at offset zero.
    pub header: PyObjectHeader,
    /// Compiled body resume function.
    pub resume: GenResumeFn,
    /// Heap frame holding the state index and locals.
    pub frame: *mut PyFrame,
    /// `0=generator`, `1=coroutine`, `2=async_generator`.
    pub kind: u8,
    /// Re-entrancy guard.
    pub running: bool,
    /// Set after `close` so late resumes raise `StopIteration(None)`.
    pub closed: bool,
    /// Exception scheduled by `throw` for the next resume-site check.
    pub pending_throw: *mut PyObject,
}

impl PyGenerator {
    /// Builds a generator payload around an already-allocated heap frame.
    #[must_use]
    pub fn new(ty: *const PyType, resume: GenResumeFn, frame: *mut PyFrame, kind: GeneratorKind) -> Self {
        Self {
            header: PyObjectHeader::new(ty),
            resume,
            frame,
            kind: kind.as_u8(),
            running: false,
            closed: false,
            pending_throw: ptr::null_mut(),
        }
    }

    /// Returns true when the object has no further resume point.
    #[must_use]
    pub unsafe fn is_exhausted(&self) -> bool {
        self.closed || self.frame.is_null() || unsafe { (*self.frame).state == FRAME_STATE_EXHAUSTED }
    }
}

static GENERATOR_TYPE: LazyLock<Mutex<Option<usize>>> = LazyLock::new(|| Mutex::new(None));
static GENERATOR_ASYNC_METHODS: LazyLock<Mutex<Option<usize>>> = LazyLock::new(|| Mutex::new(None));

/// Returns the process-lifetime generator type object, creating it if needed.
pub fn ensure_generator_type(type_type: *mut PyType) -> *mut PyType {
    let mut slot = GENERATOR_TYPE.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(ptr) = *slot {
        return ptr as *mut PyType;
    }

    let async_methods = ensure_generator_async_methods();
    let mut ty = PyType::new(type_type.cast_const(), "generator", mem::size_of::<PyGenerator>());
    ty.tp_iter = Some(generator_iter);
    ty.tp_iternext = Some(generator_next);
    ty.tp_getattro = Some(generator_getattro as GetAttrFunc);
    ty.tp_as_async = async_methods;
    ty.gc_type_id = TYPE_ID_GENERATOR.0 as usize;
    let ptr = Box::into_raw(Box::new(ty));
    *slot = Some(ptr as usize);
    ptr
}

fn ensure_generator_async_methods() -> *mut PyAsyncMethods {
    let mut slot = GENERATOR_ASYNC_METHODS.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(ptr) = *slot {
        return ptr as *mut PyAsyncMethods;
    }
    let mut methods = PyAsyncMethods::EMPTY;
    methods.am_send = Some(generator_am_send as SendFunc);
    let ptr = Box::into_raw(Box::new(methods));
    *slot = Some(ptr as usize);
    ptr
}

/// `iter(g) is g` for Python generators.
///
/// # Safety
/// `object` must be a boxed generator object.
pub unsafe extern "C" fn generator_iter(object: *mut PyObject) -> *mut PyObject {
    object
}

/// Implements `next(g)` by sending `None` into the generator.
///
/// # Safety
/// `object` must be a boxed generator object.
pub unsafe extern "C" fn generator_next(object: *mut PyObject) -> *mut PyObject {
    // SAFETY: `pon_none` follows the runtime NULL-sentinel discipline.
    let none = unsafe { crate::abi::pon_none() };
    if none.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: The iterator slot is installed only on generator objects.
    unsafe { crate::abi::r#gen::pon_gen_send(object, none) }
}

/// Async `am_send` bridge used by coroutine drivers.
///
/// # Safety
/// `object` must be a boxed generator/coroutine and `out` must be writable when
/// non-NULL.
pub unsafe extern "C" fn generator_am_send(object: *mut PyObject, value: *mut PyObject, out: *mut *mut PyObject) -> c_int {
    if out.is_null() {
        return -1;
    }
    // SAFETY: `out` is non-NULL and owned by the caller.
    unsafe {
        *out = ptr::null_mut();
    }
    // SAFETY: Delegates to the generator ABI helper.
    let result = unsafe { crate::abi::r#gen::pon_gen_send(object, value) };
    if result.is_null() {
        -1
    } else {
        // SAFETY: `out` is non-NULL and owned by the caller.
        unsafe {
            *out = result;
        }
        0
    }
}

/// Traces a generator allocation for the runtime GC.
///
/// # Safety
/// `object` must point at a live `PyGenerator` allocation.
pub unsafe extern "C" fn trace_generator(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    // SAFETY: The GC passes the allocation start for a registered PyGenerator.
    let generator = unsafe { &*object.cast::<PyGenerator>() };
    if !generator.frame.is_null() {
        visitor(generator.frame.cast::<u8>());
    }
    if !generator.pending_throw.is_null() {
        visitor(generator.pending_throw.cast::<u8>());
    }
}

/// Casts a boxed object to a generator payload without changing ownership.
///
/// # Safety
/// The caller must have established that `object` has the generator layout.
pub unsafe fn as_generator_mut(object: *mut PyObject) -> *mut PyGenerator {
    object.cast::<PyGenerator>()
}

/// Returns a boxed pointer to `generator`.
#[must_use]
pub fn as_generator_object(generator: *mut PyGenerator) -> *mut PyObject {
    as_object_ptr(generator)
}

unsafe fn argv_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Result<&'a [*mut PyObject], String> {
    if argv.is_null() && argc != 0 {
        return Err("argv pointer is null".to_owned());
    }
    Ok(if argc == 0 {
        &[]
    } else {
        // SAFETY: The caller supplies `argc` contiguous object-pointer entries.
        unsafe { core::slice::from_raw_parts(argv.cast_const(), argc) }
    })
}

unsafe fn exact_args<'a>(argv: *mut *mut PyObject, argc: usize, expected: usize, name: &str) -> Result<&'a [*mut PyObject], String> {
    if argc != expected {
        return Err(format!("{name} expected {expected} arguments, got {argc}"));
    }
    unsafe { argv_slice(argv, argc) }
}

unsafe extern "C" fn generator_send_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { exact_args(argv, argc, 2, "send") } {
        Ok(args) => args,
        Err(message) => return crate::abi::return_null_with_error(message),
    };
    // SAFETY: The bound method receiver and value occupy the two exact slots.
    unsafe { crate::abi::r#gen::pon_gen_send(args[0], args[1]) }
}

unsafe extern "C" fn generator_throw_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { exact_args(argv, argc, 2, "throw") } {
        Ok(args) => args,
        Err(message) => return crate::abi::return_null_with_error(message),
    };
    // SAFETY: The bound method receiver and exception occupy the two exact slots.
    unsafe { crate::abi::r#gen::pon_gen_throw(args[0], args[1]) }
}

unsafe extern "C" fn generator_close_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { exact_args(argv, argc, 1, "close") } {
        Ok(args) => args,
        Err(message) => return crate::abi::return_null_with_error(message),
    };
    // SAFETY: The bound method receiver is the only exact slot.
    unsafe { crate::abi::r#gen::pon_gen_close(args[0]) }
}

unsafe fn bound_generator_method(
    object: *mut PyObject,
    name: &'static str,
    arity: usize,
    code: *const u8,
) -> *mut PyObject {
    // SAFETY: `pon_make_function` allocates a normal runtime function object.
    let function = unsafe { crate::abi::pon_make_function(code, arity, intern::intern(name)) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match method::new_bound_method(function, object) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => crate::abi::return_null_with_error(message),
    }
}

/// Attribute surface for generator-family native methods.
///
/// # Safety
/// `object` must be a boxed generator/coroutine object and `name` must be a boxed
/// runtime string.
pub unsafe extern "C" fn generator_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        return crate::abi::return_null_with_error("generator attribute name must be str");
    };
    match name {
        "send" => unsafe { bound_generator_method(object, "send", 2, generator_send_method as *const u8) },
        "throw" => unsafe { bound_generator_method(object, "throw", 2, generator_throw_method as *const u8) },
        "close" => unsafe { bound_generator_method(object, "close", 1, generator_close_method as *const u8) },
        _ => crate::abi::return_null_with_error(format!("attribute '{name}' was not found")),
    }
}
