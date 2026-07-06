//! Bound method implementation.
//!
//! Class descriptor support is intentionally outside WS-FUNC, but the call
//! protocol needs a concrete "method pair" representation so `LoadMethod` and
//! `CallMethod` can agree on receiver insertion semantics.

use core::{mem, ptr};
use std::sync::OnceLock;

use crate::object::{PyObject, PyObjectHeader, PyType};

static METHOD_TYPE: OnceLock<usize> = OnceLock::new();

fn method_type() -> *mut PyType {
	*METHOD_TYPE.get_or_init(|| {
		let mut ty = Box::new(PyType::new(ptr::null(), "method", mem::size_of::<PyMethod>()));
		ty.tp_getattro = Some(method_getattro);
		ty.tp_new = Some(method_new);
		Box::into_raw(ty) as usize
	}) as *mut PyType
}

/// `types.MethodType(function, instance)`: CPython `method_new` — binds an
/// arbitrary callable to a receiver (contextlib's `ExitStack` builds its
/// `__exit__` wrappers this way).
unsafe extern "C" fn method_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	_kwargs: *mut PyObject,
) -> *mut PyObject {
	let positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
		Ok(args) => args,
		Err(message) => {
			crate::thread_state::pon_err_set(message);
			return ptr::null_mut();
		},
	};
	if positional.len() != 2 {
		crate::thread_state::pon_err_set(format!(
			"TypeError: method expected 2 arguments, got {}",
			positional.len()
		));
		return ptr::null_mut();
	}
	match new_bound_method(
		crate::tag::untag_arg(positional[0]),
		crate::tag::untag_arg(positional[1]),
	) {
		Ok(method) => method.cast::<PyObject>(),
		Err(message) => {
			crate::thread_state::pon_err_set(message);
			ptr::null_mut()
		},
	}
}

/// `tp_getattro` for bound methods: `__func__`/`__self__` answer from the
/// pair; every other name forwards to the underlying function (CPython
/// `method_getattro` parity — methods proxy the function's attribute surface,
/// e.g. `__doc__`/`__name__` for unittest's TestCase introspection).
unsafe extern "C" fn method_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let Some(text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		let message = "attribute name must be str";
		return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
	};
	let method = object.cast::<PyMethod>();
	if text == "__func__" {
		return unsafe { (*method).function };
	}
	if text == "__self__" {
		return unsafe { (*method).receiver };
	}
	unsafe { crate::abstract_op::get_attr((*method).function, crate::intern::intern(text)) }
}

/// Receiver/function pair produced by method loading.
#[repr(C)]
#[derive(Debug)]
pub struct PyMethod {
	/// Common object header; this field must remain first when bound methods are
	/// returned through ordinary attribute lookup.
	pub ob_base: PyObjectHeader,
	function:    *mut PyObject,
	receiver:    *mut PyObject,
}

impl PyMethod {
	/// Construct a bound method pair.
	pub fn new(function: *mut PyObject, receiver: *mut PyObject) -> Result<Self, String> {
		if function.is_null() {
			return Err("bound method function is NULL".to_owned());
		}
		if receiver.is_null() {
			return Err("bound method receiver is NULL".to_owned());
		}
		Ok(Self { ob_base: PyObjectHeader::new(method_type()), function, receiver })
	}

	/// Underlying callable.
	#[must_use]
	pub fn function(&self) -> *mut PyObject {
		self.function
	}

	/// Bound receiver inserted before explicit arguments by `CallMethod`.
	#[must_use]
	pub fn receiver(&self) -> *mut PyObject {
		self.receiver
	}
}

/// Allocate a bound method pair.
pub fn new_bound_method(
	function: *mut PyObject,
	receiver: *mut PyObject,
) -> Result<*mut PyMethod, String> {
	Ok(Box::into_raw(Box::new(PyMethod::new(function, receiver)?)))
}

/// Returns `(function, receiver)` when `object` is a bound-method pair.
///
/// Used by GC rooting to pierce the malloc'd `PyMethod` box: both fields are
/// GC-managed objects the collector cannot otherwise reach through it.
#[must_use]
pub(crate) fn bound_method_parts(object: *mut PyObject) -> Option<(*mut PyObject, *mut PyObject)> {
	if object.is_null() || unsafe { (*object).ob_type } != method_type().cast_const() {
		return None;
	}
	let method = unsafe { &*object.cast::<PyMethod>() };
	Some((method.function, method.receiver))
}

/// Release a method pair allocated with [`new_bound_method`].
pub unsafe fn drop_bound_method(method: *mut PyMethod) {
	if !method.is_null() {
		// SAFETY: The caller promises the pointer came from `Box::into_raw`.
		unsafe {
			drop(Box::from_raw(method));
		}
	}
}

/// Split a method pair into `(callable, receiver)`.
pub unsafe fn split_bound_method(
	method: *mut PyMethod,
) -> Result<(*mut PyObject, *mut PyObject), String> {
	if method.is_null() {
		return Err("bound method pointer is null".to_owned());
	}
	// SAFETY: The caller supplied a live method pointer.
	let method = unsafe { &*method };
	Ok((method.function(), method.receiver()))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn method_pair_preserves_receiver_order() {
		let function = 0x40usize as *mut PyObject;
		let receiver = 0x80usize as *mut PyObject;
		let method = new_bound_method(function, receiver).unwrap();
		unsafe {
			assert_eq!(split_bound_method(method).unwrap(), (function, receiver));
			drop_bound_method(method);
		}
	}

	#[test]
	fn method_type_inherits_object_when_unbased() {
		let object_type = crate::native::builtins_mod::builtin_native_type("object").unwrap();
		let ty = method_type();
		unsafe {
			assert!(crate::mro::is_subtype(ty, object_type));
			let names: Vec<_> = crate::mro::mro_entries(ty)
				.iter()
				.map(|ty| (**ty).name())
				.collect();
			assert_eq!(names, ["method", "object"]);
		}
	}
}
