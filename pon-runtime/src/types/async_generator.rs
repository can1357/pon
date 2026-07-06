//! Asynchronous generator object implementation (PEP 525).
//!
//! An async generator reuses the stackless [`PyGenerator`] payload with
//! `kind == GeneratorKind::AsyncGenerator`; this module owns the public type
//! descriptor plus the two auxiliary object families that make the async
//! protocol drivable without an event loop:
//!
//! - `async_generator_asend` / `async_generator_athrow` awaitables returned by
//!   `__anext__`/`asend`/`athrow`/`aclose`.  Each one is its own await
//!   iterator: stepping it resumes the async generator once and classifies the
//!   outcome (async yield → `StopIteration(value)`, inner-await suspension →
//!   transparent passthrough, exhaustion → `StopAsyncIteration`).
//! - `async_generator_wrapped_value`, the marker box the resume driver wraps
//!   genuine async yields in so the awaitable driver can tell them apart from
//!   passthrough yields of a delegated inner awaitable (CPython's
//!   `_PyAsyncGenWrappedValue`).
//!
//! The drivers themselves live in `abi/gen.rs` next to the shared resume
//! machinery; this module only owns layouts, type descriptors, and the
//! attribute surface.

use core::mem;
use std::sync::{LazyLock, Mutex};

use pon_gc::TypeId;

use crate::{
	object::{GetAttrFunc, PyAsyncMethods, PyObject, PyObjectHeader, PyType},
	types::{
		generator::{PyGenerator, bound_generator_method, exact_args},
		type_,
	},
};

/// GC type id reserved for async generator objects in the WS-GEN family.
pub const TYPE_ID_ASYNC_GENERATOR: TypeId = TypeId(36);
/// GC type id reserved for asend/athrow awaitable objects in the WS-GEN family.
pub const TYPE_ID_ASYNC_GEN_AWAITABLE: TypeId = TypeId(37);
/// GC type id reserved for wrapped async-yield marker boxes in the WS-GEN
/// family.
pub const TYPE_ID_ASYNC_GEN_WRAPPED: TypeId = TypeId(38);

/// Async generators use the generator payload with `kind == AsyncGenerator`.
pub type PyAsyncGenerator = PyGenerator;

/// `__anext__()`/`asend(v)` awaitable: first step sends the stored payload.
pub const AWAITABLE_MODE_SEND: u8 = 0;
/// `athrow(exc)` awaitable: first step throws the stored payload.
pub const AWAITABLE_MODE_THROW: u8 = 1;
/// `aclose()` awaitable: first step throws `GeneratorExit` and swallows the
/// exhaustion family into `StopIteration(None)`.
pub const AWAITABLE_MODE_CLOSE: u8 = 2;

/// Awaitable has not resumed the async generator yet.
pub const AWAITABLE_STATE_START: u8 = 0;
/// Awaitable is mid-flight (the generator suspended in an inner await).
pub const AWAITABLE_STATE_ITER: u8 = 1;
/// Awaitable completed (or was closed); further steps are a RuntimeError.
pub const AWAITABLE_STATE_CLOSED: u8 = 2;

/// Boxed asend/athrow/aclose awaitable payload.
#[repr(C)]
#[derive(Debug)]
pub struct PyAsyncGenAwaitable {
	/// Standard boxed-object header at offset zero.
	pub header:  PyObjectHeader,
	/// The async generator this awaitable drives.
	pub agen:    *mut PyObject,
	/// First-step payload: send value (`asend`) or exception (`athrow`), NULL
	/// for plain `__anext__()`/`aclose()`.
	pub payload: *mut PyObject,
	/// One of the `AWAITABLE_MODE_*` constants.
	pub mode:    u8,
	/// One of the `AWAITABLE_STATE_*` constants.
	pub state:   u8,
}

/// Marker box distinguishing a genuine async `yield` from a passthrough yield
/// of a delegated inner awaitable (CPython `_PyAsyncGenWrappedValue`).
#[repr(C)]
#[derive(Debug)]
pub struct PyAsyncGenWrappedValue {
	/// Standard boxed-object header at offset zero.
	pub header: PyObjectHeader,
	/// The value the async generator body yielded.
	pub value:  *mut PyObject,
}

static ASYNC_GENERATOR_TYPE: LazyLock<Mutex<Option<usize>>> = LazyLock::new(|| Mutex::new(None));
static ASYNC_GENERATOR_ASYNC_METHODS: LazyLock<Mutex<Option<usize>>> =
	LazyLock::new(|| Mutex::new(None));
static ASEND_TYPE: LazyLock<Mutex<Option<usize>>> = LazyLock::new(|| Mutex::new(None));
static ATHROW_TYPE: LazyLock<Mutex<Option<usize>>> = LazyLock::new(|| Mutex::new(None));
static AWAITABLE_ASYNC_METHODS: LazyLock<Mutex<Option<usize>>> = LazyLock::new(|| Mutex::new(None));
static WRAPPED_VALUE_TYPE: LazyLock<Mutex<Option<usize>>> = LazyLock::new(|| Mutex::new(None));

/// Returns the process-lifetime async generator type object, creating it if
/// needed.  Async generators are not sync-iterable and not awaitable: only
/// `am_aiter`/`am_anext` and the `a*` attribute surface are populated.
pub fn ensure_async_generator_type(type_type: *mut PyType) -> *mut PyType {
	let mut slot = ASYNC_GENERATOR_TYPE
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	if let Some(ptr) = *slot {
		return ptr as *mut PyType;
	}

	let async_methods = ensure_async_generator_async_methods();
	let mut ty =
		PyType::new(type_type.cast_const(), "async_generator", mem::size_of::<PyAsyncGenerator>());
	ty.tp_getattro = Some(async_generator_getattro as GetAttrFunc);
	ty.tp_as_async = async_methods;
	ty.gc_type_id = TYPE_ID_ASYNC_GENERATOR.0 as usize;
	let ptr = Box::into_raw(Box::new(ty));
	*slot = Some(ptr as usize);
	ptr
}

fn ensure_async_generator_async_methods() -> *mut PyAsyncMethods {
	let mut slot = ASYNC_GENERATOR_ASYNC_METHODS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	if let Some(ptr) = *slot {
		return ptr as *mut PyAsyncMethods;
	}
	let mut methods = PyAsyncMethods::EMPTY;
	methods.am_aiter = Some(async_generator_aiter);
	methods.am_anext = Some(async_generator_anext);
	let ptr = Box::into_raw(Box::new(methods));
	*slot = Some(ptr as usize);
	ptr
}

/// Returns the process-lifetime `async_generator_asend` type object.
pub fn ensure_asend_type(type_type: *mut PyType) -> *mut PyType {
	ensure_awaitable_type(type_type, &ASEND_TYPE, "async_generator_asend")
}

/// Returns the process-lifetime `async_generator_athrow` type object (shared
/// by `athrow` and `aclose` awaitables, mirroring CPython).
pub fn ensure_athrow_type(type_type: *mut PyType) -> *mut PyType {
	ensure_awaitable_type(type_type, &ATHROW_TYPE, "async_generator_athrow")
}

fn ensure_awaitable_type(
	type_type: *mut PyType,
	slot: &Mutex<Option<usize>>,
	name: &'static str,
) -> *mut PyType {
	let mut slot = slot.lock().unwrap_or_else(|poison| poison.into_inner());
	if let Some(ptr) = *slot {
		return ptr as *mut PyType;
	}

	let async_methods = ensure_awaitable_async_methods();
	let mut ty = PyType::new(type_type.cast_const(), name, mem::size_of::<PyAsyncGenAwaitable>());
	ty.tp_iter = Some(awaitable_iter);
	ty.tp_iternext = Some(awaitable_next);
	ty.tp_getattro = Some(awaitable_getattro as GetAttrFunc);
	ty.tp_as_async = async_methods;
	ty.gc_type_id = TYPE_ID_ASYNC_GEN_AWAITABLE.0 as usize;
	let ptr = Box::into_raw(Box::new(ty));
	*slot = Some(ptr as usize);
	ptr
}

fn ensure_awaitable_async_methods() -> *mut PyAsyncMethods {
	let mut slot = AWAITABLE_ASYNC_METHODS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	if let Some(ptr) = *slot {
		return ptr as *mut PyAsyncMethods;
	}
	let mut methods = PyAsyncMethods::EMPTY;
	methods.am_await = Some(awaitable_await);
	let ptr = Box::into_raw(Box::new(methods));
	*slot = Some(ptr as usize);
	ptr
}

/// Returns the process-lifetime `async_generator_wrapped_value` type object.
pub fn ensure_wrapped_value_type(type_type: *mut PyType) -> *mut PyType {
	let mut slot = WRAPPED_VALUE_TYPE
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	if let Some(ptr) = *slot {
		return ptr as *mut PyType;
	}
	let mut ty = PyType::new(
		type_type.cast_const(),
		"async_generator_wrapped_value",
		mem::size_of::<PyAsyncGenWrappedValue>(),
	);
	ty.gc_type_id = TYPE_ID_ASYNC_GEN_WRAPPED.0 as usize;
	let ptr = Box::into_raw(Box::new(ty));
	*slot = Some(ptr as usize);
	ptr
}

/// `agen.__aiter__() is agen` (PEP 525).
///
/// # Safety
/// `object` must be a boxed async generator object.
pub unsafe extern "C" fn async_generator_aiter(object: *mut PyObject) -> *mut PyObject {
	object
}

/// `am_anext` slot: builds the `__anext__()` awaitable (send `None`).
///
/// # Safety
/// `object` must be a boxed async generator object.
pub unsafe extern "C" fn async_generator_anext(object: *mut PyObject) -> *mut PyObject {
	// SAFETY: Delegates to the awaitable allocator with a NULL (None) payload.
	unsafe {
		crate::abi::r#gen::async_gen_make_awaitable(
			object,
			core::ptr::null_mut(),
			AWAITABLE_MODE_SEND,
		)
	}
}

/// The awaitable is its own iterator.
///
/// # Safety
/// `object` must be a boxed asend/athrow awaitable.
pub unsafe extern "C" fn awaitable_iter(object: *mut PyObject) -> *mut PyObject {
	object
}

/// The awaitable is its own await iterator.
///
/// # Safety
/// `object` must be a boxed asend/athrow awaitable.
pub unsafe extern "C" fn awaitable_await(object: *mut PyObject) -> *mut PyObject {
	object
}

/// `tp_iternext`: one bare step (send `None`).
///
/// # Safety
/// `object` must be a boxed asend/athrow awaitable.
pub unsafe extern "C" fn awaitable_next(object: *mut PyObject) -> *mut PyObject {
	// SAFETY: Delegates to the shared awaitable driver with no payload.
	unsafe {
		crate::abi::r#gen::async_gen_awaitable_step(
			object,
			core::ptr::null_mut(),
			core::ptr::null_mut(),
		)
	}
}

/// Traces an asend/athrow awaitable allocation for the runtime GC.
///
/// # Safety
/// `object` must point at a live `PyAsyncGenAwaitable` allocation.
pub unsafe extern "C" fn trace_async_gen_awaitable(
	object: *mut u8,
	visitor: &mut dyn FnMut(*mut u8),
) {
	if object.is_null() {
		return;
	}
	// SAFETY: The GC passes the allocation start for a registered awaitable.
	let awaitable = unsafe { &*object.cast::<PyAsyncGenAwaitable>() };
	if !awaitable.agen.is_null() {
		visitor(awaitable.agen.cast::<u8>());
	}
	if !awaitable.payload.is_null() {
		visitor(awaitable.payload.cast::<u8>());
	}
}

/// Traces a wrapped async-yield marker box for the runtime GC.
///
/// # Safety
/// `object` must point at a live `PyAsyncGenWrappedValue` allocation.
pub unsafe extern "C" fn trace_async_gen_wrapped(
	object: *mut u8,
	visitor: &mut dyn FnMut(*mut u8),
) {
	if object.is_null() {
		return;
	}
	// SAFETY: The GC passes the allocation start for a registered wrapper.
	let wrapped = unsafe { &*object.cast::<PyAsyncGenWrappedValue>() };
	if !wrapped.value.is_null() {
		visitor(wrapped.value.cast::<u8>());
	}
}

unsafe extern "C" fn asyncgen_aiter_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { exact_args(argv, argc, 1, "__aiter__") } {
		Ok(args) => args,
		Err(message) => return crate::abi::return_null_with_error(message),
	};
	args[0]
}

unsafe extern "C" fn asyncgen_anext_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { exact_args(argv, argc, 1, "__anext__") } {
		Ok(args) => args,
		Err(message) => return crate::abi::return_null_with_error(message),
	};
	// SAFETY: The bound method receiver is the only exact slot.
	unsafe {
		crate::abi::r#gen::async_gen_make_awaitable(
			args[0],
			core::ptr::null_mut(),
			AWAITABLE_MODE_SEND,
		)
	}
}

unsafe extern "C" fn asyncgen_asend_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { exact_args(argv, argc, 2, "asend") } {
		Ok(args) => args,
		Err(message) => return crate::abi::return_null_with_error(message),
	};
	// SAFETY: The bound method receiver and value occupy the two exact slots.
	unsafe { crate::abi::r#gen::async_gen_make_awaitable(args[0], args[1], AWAITABLE_MODE_SEND) }
}

unsafe extern "C" fn asyncgen_athrow_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match unsafe { exact_args(argv, argc, 2, "athrow") } {
		Ok(args) => args,
		Err(message) => return crate::abi::return_null_with_error(message),
	};
	// SAFETY: The bound method receiver and exception occupy the two exact slots.
	unsafe { crate::abi::r#gen::async_gen_make_awaitable(args[0], args[1], AWAITABLE_MODE_THROW) }
}

unsafe extern "C" fn asyncgen_aclose_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match unsafe { exact_args(argv, argc, 1, "aclose") } {
		Ok(args) => args,
		Err(message) => return crate::abi::return_null_with_error(message),
	};
	// SAFETY: The bound method receiver is the only exact slot.
	unsafe {
		crate::abi::r#gen::async_gen_make_awaitable(
			args[0],
			core::ptr::null_mut(),
			AWAITABLE_MODE_CLOSE,
		)
	}
}

/// Attribute surface for async generator objects.
///
/// # Safety
/// `object` must be a boxed async generator object and `name` must be a boxed
/// runtime string.
pub unsafe extern "C" fn async_generator_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(name) = (unsafe { type_::unicode_text(name) }) else {
		return crate::abi::return_null_with_error("async generator attribute name must be str");
	};
	match name {
		"__aiter__" => unsafe {
			bound_generator_method(object, "__aiter__", 1, asyncgen_aiter_method as *const u8)
		},
		"__anext__" => unsafe {
			bound_generator_method(object, "__anext__", 1, asyncgen_anext_method as *const u8)
		},
		"asend" => unsafe {
			bound_generator_method(object, "asend", 2, asyncgen_asend_method as *const u8)
		},
		"athrow" => unsafe {
			bound_generator_method(object, "athrow", 2, asyncgen_athrow_method as *const u8)
		},
		"aclose" => unsafe {
			bound_generator_method(object, "aclose", 1, asyncgen_aclose_method as *const u8)
		},
		_ => crate::abi::return_null_with_error(format!("attribute '{name}' was not found")),
	}
}

unsafe extern "C" fn awaitable_send_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { exact_args(argv, argc, 2, "send") } {
		Ok(args) => args,
		Err(message) => return crate::abi::return_null_with_error(message),
	};
	// SAFETY: The bound method receiver and value occupy the two exact slots.
	unsafe { crate::abi::r#gen::async_gen_awaitable_step(args[0], args[1], core::ptr::null_mut()) }
}

unsafe extern "C" fn awaitable_throw_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match unsafe { exact_args(argv, argc, 2, "throw") } {
		Ok(args) => args,
		Err(message) => return crate::abi::return_null_with_error(message),
	};
	// SAFETY: The bound method receiver and exception occupy the two exact slots.
	unsafe { crate::abi::r#gen::async_gen_awaitable_step(args[0], core::ptr::null_mut(), args[1]) }
}

unsafe extern "C" fn awaitable_close_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match unsafe { exact_args(argv, argc, 1, "close") } {
		Ok(args) => args,
		Err(message) => return crate::abi::return_null_with_error(message),
	};
	// SAFETY: The bound method receiver is the only exact slot.
	unsafe { crate::abi::r#gen::async_gen_awaitable_close(args[0]) }
}

unsafe extern "C" fn awaitable_next_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { exact_args(argv, argc, 1, "__next__") } {
		Ok(args) => args,
		Err(message) => return crate::abi::return_null_with_error(message),
	};
	// SAFETY: The bound method receiver is the only exact slot.
	unsafe {
		crate::abi::r#gen::async_gen_awaitable_step(
			args[0],
			core::ptr::null_mut(),
			core::ptr::null_mut(),
		)
	}
}

unsafe extern "C" fn awaitable_identity_method(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let args = match unsafe { exact_args(argv, argc, 1, "__iter__") } {
		Ok(args) => args,
		Err(message) => return crate::abi::return_null_with_error(message),
	};
	args[0]
}

/// Attribute surface for asend/athrow awaitable objects.
///
/// # Safety
/// `object` must be a boxed asend/athrow awaitable and `name` must be a boxed
/// runtime string.
pub unsafe extern "C" fn awaitable_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(name) = (unsafe { type_::unicode_text(name) }) else {
		return crate::abi::return_null_with_error("awaitable attribute name must be str");
	};
	match name {
		"send" => unsafe {
			bound_generator_method(object, "send", 2, awaitable_send_method as *const u8)
		},
		"throw" => unsafe {
			bound_generator_method(object, "throw", 2, awaitable_throw_method as *const u8)
		},
		"close" => unsafe {
			bound_generator_method(object, "close", 1, awaitable_close_method as *const u8)
		},
		"__next__" => unsafe {
			bound_generator_method(object, "__next__", 1, awaitable_next_method as *const u8)
		},
		"__iter__" | "__await__" => unsafe {
			bound_generator_method(object, "__iter__", 1, awaitable_identity_method as *const u8)
		},
		_ => crate::abi::return_null_with_error(format!("attribute '{name}' was not found")),
	}
}
