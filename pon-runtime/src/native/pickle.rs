//! Native `_pickle` seed: `PickleBuffer` only (PEP 574).
//!
//! `Lib/pickle.py` imports `_pickle` in two INDEPENDENT `try/except
//! ImportError` blocks: `from _pickle import PickleBuffer` (line 42 — no
//! pure-Python fallback exists for it) and the C accelerators
//! (`from _pickle import PickleError, ..., Pickler, Unpickler, ...`, line
//! 1894 — a complete pure-Python fallback exists).  This module deliberately
//! exports ONLY `PickleBuffer`: the first block succeeds (so
//! `pickle.PickleBuffer` and the protocol-5 `save_picklebuffer` dispatch
//! exist), while the second still raises `ImportError` ("cannot import name
//! 'PickleError' from '_pickle'") and the Python `Pickler`/`Unpickler`
//! implementations stay in service — an honest partial module, never a stub
//! pickler.
//!
//! `PickleBuffer` wraps a contiguous readonly/writable byte window over a
//! bytes/bytearray/memoryview exporter, exactly the subset the vendored
//! stdlib and `test.test_picklebuffer` drive: construction type/value
//! errors, `raw()` (flat B-format memoryview), `release()` idempotency, and
//! buffer extraction via `bytes(pb)` / `memoryview(pb)` (hooked from
//! `abi/str_.rs`).  Instances are immortal leaked boxes (the `_contextvars`
//! pattern); held exporter objects are reported through [`gc_held_roots`].

use std::{
	ptr,
	sync::{LazyLock, Mutex},
};

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{
	abi,
	intern::intern,
	object::{PyObject, PyObjectHeader, PyType, as_object_ptr},
	thread_state::{pon_err_clear, pon_err_set},
	types::{bytearray_, bytes_, memoryview, type_::unicode_text},
};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

/// CPython's diagnostic for any operation on a released `PickleBuffer`.
const RELEASED_ERROR: &str = "operation forbidden on released PickleBuffer object";

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let name = "_pickle";
	// SAFETY: Runtime allocation helper; NULL is checked below.
	let name_obj = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
	if name_obj.is_null() {
		return Err("failed to allocate _pickle.__name__".to_owned());
	}
	let module = install_module(name, vec![
		(intern("__name__"), name_obj),
		(intern("PickleBuffer"), picklebuffer_type().cast::<PyObject>()),
	])?;
	populate_python_pickle_exports();
	Ok(module)
}

fn populate_python_pickle_exports() {
	let pickle_id = intern("pickle");
	if crate::import::module_attrs_snapshot(pickle_id).is_some() {
		return;
	}
	let module = unsafe { crate::import::pon_import_name(pickle_id, ptr::null(), 0, 0) };
	if module.is_null() {
		pon_err_clear();
	}
}

// ---------------------------------------------------------------------------
// Object layout and type

#[repr(C)]
struct PyPickleBuffer {
	ob_base:  PyObjectHeader,
	/// Buffer exporter (bytes/bytearray/memoryview), or NULL once
	/// `release()` ran.
	base:     *mut PyObject,
	/// Raw pointer to the first visible byte of the exporter's storage.
	data:     *mut u8,
	/// Visible byte length.
	len:      usize,
	/// Whether writes through views over this buffer are forbidden.
	readonly: bool,
}

static PICKLEBUFFER_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		// CPython's C type is `pickle.PickleBuffer`: the dotted tp_name keeps
		// `repr(type)` and type-in-error-message parity, while the `__name__`
		// getter exposes only the tail component.
		"pickle.PickleBuffer",
		std::mem::size_of::<PyPickleBuffer>(),
	);
	ty.tp_base = runtime_object_type();
	ty.tp_new = Some(picklebuffer_new);
	ty.tp_getattro = Some(picklebuffer_getattro);
	// pon's `__module__` getter reads tp_dict with a "builtins" default for
	// native types; carry the CPython value explicitly.
	let namespace = crate::types::type_::new_namespace();
	if !namespace.is_null() {
		// SAFETY: String allocation helper; NULL skips the binding.
		let module = unsafe { abi::pon_const_str("pickle".as_ptr(), "pickle".len()) };
		if !module.is_null() {
			// SAFETY: Freshly allocated namespace box.
			unsafe { (&mut *namespace).set(intern("__module__"), module) };
			ty.tp_dict = namespace.cast::<PyObject>();
		}
	}
	Box::into_raw(Box::new(ty)) as usize
});

fn picklebuffer_type() -> *mut PyType {
	*PICKLEBUFFER_TYPE as *mut PyType
}

fn runtime_object_type() -> *mut PyType {
	abi::runtime_global(intern("object")).map_or(ptr::null_mut(), |object| object.cast::<PyType>())
}

// ---------------------------------------------------------------------------
// Allocation and the GC root registry (the `_contextvars` pattern)

/// Every `PickleBuffer` allocation, for GC root reporting of held exporters.
static REGISTRY: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn alloc_picklebuffer(
	base: *mut PyObject,
	data: *mut u8,
	len: usize,
	readonly: bool,
) -> *mut PyObject {
	let object = Box::into_raw(Box::new(PyPickleBuffer {
		ob_base: PyObjectHeader::new(picklebuffer_type()),
		base,
		data,
		len,
		readonly,
	}))
	.cast::<PyObject>();
	REGISTRY
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.push(object as usize);
	object
}

/// GC roots held by live `PickleBuffer` instances: their buffer exporters.
/// Consumed by `crate::abi::collect` while the runtime lock is held, so this
/// must not re-enter the runtime.  Released buffers hold nothing.
pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
	let registry = REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
	let mut roots = Vec::new();
	for &addr in registry.iter() {
		// SAFETY: Registry members are live leaked PyPickleBuffer boxes.
		let base = unsafe { (*(addr as *mut PyPickleBuffer)).base };
		if !base.is_null() && crate::tag::is_heap(base) {
			roots.push(base);
		}
	}
	roots
}

unsafe fn as_picklebuffer<'a>(object: *mut PyObject) -> Option<&'a mut PyPickleBuffer> {
	let object = untag(object);
	if object.is_null() {
		return None;
	}
	// The type is created lazily; before the first `_pickle` import no
	// instance can exist, and forcing the LazyLock here would be wasted work
	// on every bytes/memoryview call.
	let ty = LazyLock::get(&PICKLEBUFFER_TYPE).map_or(ptr::null(), |&ty| ty as *const PyType);
	if ty.is_null() {
		return None;
	}
	// SAFETY: NULL was rejected above; the type check gates the downcast.
	(unsafe { (*object).ob_type } == ty).then(|| unsafe { &mut *object.cast::<PyPickleBuffer>() })
}

// ---------------------------------------------------------------------------
// Hooks consumed by `abi/str_.rs` (buffer-protocol seams)

/// `memoryview(pb)` branch: `None` when `object` is not a `PickleBuffer`;
/// otherwise the fully-handled result (a fresh B-format view, or NULL with a
/// raised `ValueError` for released buffers).
pub(crate) fn memoryview_over_picklebuffer(object: *mut PyObject) -> Option<*mut PyObject> {
	// SAFETY: Type-checked downcast; NULL/foreign objects return None.
	let buffer = unsafe { as_picklebuffer(object) }?;
	if buffer.base.is_null() {
		// SAFETY: Typed raise helper; the message bytes are copied.
		return Some(unsafe {
			abi::exc::pon_raise_value_error(RELEASED_ERROR.as_ptr(), RELEASED_ERROR.len())
		});
	}
	Some(as_object_ptr(memoryview::boxed_memoryview_from_raw(
		buffer.base,
		buffer.data,
		buffer.len,
		buffer.readonly,
		b'B',
	)))
}

/// `bytes(pb)` / bytes-like coercion branch: `None` when `object` is not a
/// `PickleBuffer`; otherwise the copied window or the released-buffer error.
pub(crate) fn picklebuffer_bytes(object: *mut PyObject) -> Option<Result<Vec<u8>, String>> {
	// SAFETY: Type-checked downcast; NULL/foreign objects return None.
	let buffer = unsafe { as_picklebuffer(object) }?;
	if buffer.base.is_null() {
		return Some(Err(RELEASED_ERROR.to_owned()));
	}
	if buffer.data.is_null() {
		return Some(Ok(Vec::new()));
	}
	// SAFETY: Live buffers keep `data`/`len` in sync with their exporter.
	Some(Ok(unsafe { core::slice::from_raw_parts(buffer.data.cast_const(), buffer.len) }.to_vec()))
}

// ---------------------------------------------------------------------------
// Constructor

unsafe extern "C" fn picklebuffer_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	let positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
		Ok(positional) => positional,
		Err(message) => return fail(message),
	};
	if !kwargs.is_null() {
		let entries = match unsafe { crate::types::dict::dict_entries_snapshot(kwargs) } {
			Ok(entries) => entries,
			Err(message) => return fail(message),
		};
		if !entries.is_empty() {
			return raise_type_error("PickleBuffer() takes no keyword arguments");
		}
	}
	if positional.len() != 1 {
		let message =
			format!("PickleBuffer() takes exactly 1 positional argument ({} given)", positional.len());
		return raise_type_error(&message);
	}
	let source = untag(positional[0]);
	if source.is_null() {
		return raise_type_error("PickleBuffer() argument is NULL");
	}
	// SAFETY: `source` is a live object; type checks gate every downcast.
	let ty = unsafe { (*source).ob_type };
	if bytes_::is_bytes_type(ty) {
		// SAFETY: Type check above proved the layout.
		let bytes = unsafe { &*source.cast::<bytes_::PyBytes>() };
		// SAFETY: Bytes storage is stable for the object's lifetime.
		let slice = unsafe { bytes.as_slice() };
		return alloc_picklebuffer(source, slice.as_ptr().cast_mut(), slice.len(), true);
	}
	if bytearray_::is_bytearray_type(ty) {
		// SAFETY: Type check above proved the layout.
		let bytearray = unsafe { &mut *source.cast::<bytearray_::PyByteArray>() };
		return alloc_picklebuffer(
			source,
			bytearray.bytes.as_mut_ptr(),
			bytearray.bytes.len(),
			false,
		);
	}
	if memoryview::is_memoryview_type(ty) {
		// SAFETY: Type check above proved the layout.
		let view = unsafe { &*source.cast::<memoryview::PyMemoryView>() };
		if view.released {
			let message = memoryview::RELEASED_ERROR;
			// SAFETY: Typed raise helper; the message bytes are copied.
			return unsafe { abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) };
		}
		return alloc_picklebuffer(source, view.data, view.len, view.readonly);
	}
	if let Some(buffer) = unsafe { as_picklebuffer(source) } {
		if buffer.base.is_null() {
			// SAFETY: Typed raise helper; the message bytes are copied.
			return unsafe {
				abi::exc::pon_raise_value_error(RELEASED_ERROR.as_ptr(), RELEASED_ERROR.len())
			};
		}
		return alloc_picklebuffer(buffer.base, buffer.data, buffer.len, buffer.readonly);
	}
	let type_name = unsafe { crate::types::dict::type_name(source) }.unwrap_or("object");
	let message = format!("a bytes-like object is required, not '{type_name}'");
	raise_type_error(&message)
}

// ---------------------------------------------------------------------------
// Attribute surface: raw() and release()

unsafe extern "C" fn picklebuffer_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(name_text) = (unsafe { unicode_text(untag(name)) }) else {
		return fail("attribute name must be str");
	};
	if unsafe { as_picklebuffer(object) }.is_none() {
		return fail("PickleBuffer receiver is invalid");
	}
	match name_text {
		"raw" => bound_method(object, name_text, raw_method),
		"release" => bound_method(object, name_text, release_method),
		// SAFETY: Raise helper with the interned attribute name.
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

/// `PickleBuffer.raw()`: a flat B-format memoryview over the whole window.
/// pon buffers are always C-contiguous, so the BufferError leg of CPython's
/// contract (non-contiguous exporters) is unreachable here.
unsafe extern "C" fn raw_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("PickleBuffer.raw received a NULL argv pointer");
	};
	let Some((&receiver, rest)) = args.split_first() else {
		return fail("PickleBuffer.raw requires a receiver");
	};
	if !rest.is_empty() {
		let message = format!("raw() takes no arguments ({} given)", rest.len());
		return raise_type_error(&message);
	}
	let Some(buffer) = (unsafe { as_picklebuffer(receiver) }) else {
		return fail("PickleBuffer.raw receiver is invalid");
	};
	if buffer.base.is_null() {
		// SAFETY: Typed raise helper; the message bytes are copied.
		return unsafe {
			abi::exc::pon_raise_value_error(RELEASED_ERROR.as_ptr(), RELEASED_ERROR.len())
		};
	}
	if let Err(message) = crate::abi::str_::install_memoryview_slots() {
		return fail(message);
	}
	as_object_ptr(memoryview::boxed_memoryview_from_raw(
		buffer.base,
		buffer.data,
		buffer.len,
		buffer.readonly,
		b'B',
	))
}

/// `PickleBuffer.release()`: drop the exporter reference; idempotent.
unsafe extern "C" fn release_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("PickleBuffer.release received a NULL argv pointer");
	};
	let Some((&receiver, rest)) = args.split_first() else {
		return fail("PickleBuffer.release requires a receiver");
	};
	if !rest.is_empty() {
		let message = format!("release() takes no arguments ({} given)", rest.len());
		return raise_type_error(&message);
	}
	let Some(buffer) = (unsafe { as_picklebuffer(receiver) }) else {
		return fail("PickleBuffer.release receiver is invalid");
	};
	buffer.base = ptr::null_mut();
	buffer.data = ptr::null_mut();
	buffer.len = 0;
	none()
}

// ---------------------------------------------------------------------------
// Helpers (contextvars idioms)

fn untag(object: *mut PyObject) -> *mut PyObject {
	crate::tag::untag_arg(object)
}

fn fail(message: impl Into<String>) -> *mut PyObject {
	pon_err_set(message);
	ptr::null_mut()
}

fn none() -> *mut PyObject {
	// SAFETY: Singleton accessor.
	unsafe { abi::pon_none() }
}

fn raise_type_error(message: &str) -> *mut PyObject {
	// SAFETY: Message bytes are a live UTF-8 slice for the duration of the call.
	unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

unsafe fn arg_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
	if argc == 0 {
		Some(&[])
	} else if argv.is_null() {
		None
	} else {
		// SAFETY: The caller passed `argc` live argument slots.
		Some(unsafe { std::slice::from_raw_parts(argv, argc) })
	}
}

fn bound_method(receiver: *mut PyObject, name: &str, entry: BuiltinFn) -> *mut PyObject {
	// SAFETY: `entry` is a live builtin entry point with the runtime calling
	// convention.
	let function =
		unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	if function.is_null() {
		return ptr::null_mut();
	}
	match crate::types::method::new_bound_method(function, receiver) {
		Ok(method) => method.cast::<PyObject>(),
		Err(message) => fail(message),
	}
}
