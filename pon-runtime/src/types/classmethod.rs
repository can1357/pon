//! Classmethod and staticmethod descriptor implementation.

use core::{ffi::c_int, ptr};

use crate::{
	object::{PyObject, PyObjectHeader, PyType},
	thread_state::pon_err_set,
};

/// Python `classmethod` descriptor.
#[repr(C)]
#[derive(Debug)]
pub struct PyClassMethod {
	/// Common object header; must remain first.
	pub ob_base:  PyObjectHeader,
	/// Wrapped callable.
	pub callable: *mut PyObject,
}

/// Python `staticmethod` descriptor.
#[repr(C)]
#[derive(Debug)]
pub struct PyStaticMethod {
	/// Common object header; must remain first.
	pub ob_base:  PyObjectHeader,
	/// Wrapped callable.
	pub callable: *mut PyObject,
}

fn raise_method(message: &str) -> *mut PyObject {
	pon_err_set(message);
	ptr::null_mut()
}

/// Allocate a classmethod descriptor.
#[must_use]
pub unsafe fn new_classmethod(
	classmethod_type: *const PyType,
	callable: *mut PyObject,
) -> *mut PyObject {
	Box::into_raw(Box::new(PyClassMethod {
		ob_base: PyObjectHeader::new(classmethod_type),
		callable,
	}))
	.cast::<PyObject>()
}

/// Allocate a staticmethod descriptor.
#[must_use]
pub unsafe fn new_staticmethod(
	staticmethod_type: *const PyType,
	callable: *mut PyObject,
) -> *mut PyObject {
	Box::into_raw(Box::new(PyStaticMethod {
		ob_base: PyObjectHeader::new(staticmethod_type),
		callable,
	}))
	.cast::<PyObject>()
}

/// Descriptor `classmethod.__get__`: binds the wrapped callable to the owner
/// class as a standard bound-method pair (CPython parity).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn classmethod_descr_get(
	descr: *mut PyObject,
	obj: *mut PyObject,
	owner: *mut PyObject,
) -> *mut PyObject {
	if descr.is_null() {
		return raise_method("classmethod descriptor is NULL");
	}
	let owner = if !owner.is_null() {
		owner
	} else if !obj.is_null() {
		unsafe { (*obj).ob_type.cast_mut().cast::<PyObject>() }
	} else {
		return raise_method("classmethod needs an owner");
	};
	let classmethod = unsafe { &*descr.cast::<PyClassMethod>() };
	if classmethod.callable.is_null() {
		return raise_method("classmethod wraps NULL callable");
	}
	match crate::types::method::new_bound_method(classmethod.callable, owner) {
		Ok(method) => method.cast::<PyObject>(),
		Err(message) => raise_method(&message),
	}
}

/// Descriptor `staticmethod.__get__`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn staticmethod_descr_get(
	descr: *mut PyObject,
	_obj: *mut PyObject,
	_owner: *mut PyObject,
) -> *mut PyObject {
	if descr.is_null() {
		return raise_method("staticmethod descriptor is NULL");
	}
	let staticmethod = unsafe { &*descr.cast::<PyStaticMethod>() };
	if staticmethod.callable.is_null() {
		raise_method("staticmethod wraps NULL callable")
	} else {
		staticmethod.callable
	}
}

/// Shared `tp_getattro` for classmethod/staticmethod descriptors: both carry
/// the wrapped callable at the same offset and expose it as `__func__` (and
/// the functools-facing `__wrapped__` alias, CPython parity).  Their
/// descriptor protocol must also be visible as an attribute: enum's
/// `_is_descriptor` probes class-body values with `hasattr(value, '__get__')`.
unsafe extern "C" fn method_descriptor_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(name) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		return raise_method("attribute name must be str");
	};
	match name {
		"__get__" => bound_entry(object, name, method_descriptor_dunder_get_entry),
		"__func__" | "__wrapped__" => {
			let callable = unsafe { (*object.cast::<PyStaticMethod>()).callable };
			if callable.is_null() {
				raise_method("method descriptor wraps NULL callable")
			} else {
				callable
			}
		},
		_ => raise_method(&format!("attribute '{name}' was not found")),
	}
}

fn bound_entry(
	receiver: *mut PyObject,
	name: &str,
	entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> *mut PyObject {
	let function = unsafe {
		crate::abi::pon_make_function(
			entry as *const u8,
			crate::builtins::variadic_arity(),
			crate::intern::intern(name),
		)
	};
	if function.is_null() {
		return ptr::null_mut();
	}
	match crate::types::method::new_bound_method(function, receiver) {
		Ok(method) => method.cast::<PyObject>(),
		Err(message) => raise_method(&message),
	}
}

unsafe fn entry_args<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
	if argv.is_null() {
		return (argc == 0).then_some(&[]);
	}
	Some(unsafe { core::slice::from_raw_parts(argv.cast_const(), argc) })
}

fn is_none_arg(object: *mut PyObject) -> bool {
	crate::tag::untag_arg(object) == unsafe { crate::abi::pon_none() }
}

/// Python-visible `classmethod.__get__`/`staticmethod.__get__`.
unsafe extern "C" fn method_descriptor_dunder_get_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let Some(args) = (unsafe { entry_args(argv, argc) }) else {
		return raise_method("__get__ received a NULL argv pointer");
	};
	let (&receiver, rest) = args.split_first().unwrap_or((&ptr::null_mut(), &[]));
	if rest.is_empty() || rest.len() > 2 {
		return raise_method("__get__(instance, owner=None) takes 1 or 2 arguments");
	}
	if receiver.is_null() {
		return raise_method("__get__ receiver is NULL");
	}
	let ty = unsafe { (*receiver).ob_type.cast_mut() };
	if ty.is_null() {
		return raise_method("__get__ receiver has no type");
	}
	let Some(get) = (unsafe { (*ty).tp_descr_get }) else {
		return raise_method("__get__ receiver is not a descriptor");
	};
	let obj = if is_none_arg(rest[0]) {
		ptr::null_mut()
	} else {
		rest[0]
	};
	let owner = rest
		.get(1)
		.copied()
		.filter(|owner| !is_none_arg(*owner))
		.unwrap_or(ptr::null_mut());
	unsafe { get(receiver, obj, owner.cast::<PyObject>()) }
}

/// Populate slots on the `classmethod` type descriptor.
pub fn install_classmethod_slots(ty: &mut PyType) {
	ty.tp_descr_get = Some(classmethod_descr_get);
	ty.tp_getattro = Some(method_descriptor_getattro);
}

/// `staticmethod.__call__` (CPython 3.10+, bpo-43682): a staticmethod object
/// invoked directly delegates to the wrapped callable.  Load-bearing for
/// module-level `@staticmethod` functions (`_pyio.open` in 3.14): module
/// attribute access does NOT run the descriptor protocol, so callers receive
/// the carrier itself and call it.  The args/kwargs carriers are forwarded
/// verbatim through `pon_call_ex`'s `*`/`**` legs.
unsafe extern "C" fn staticmethod_call(
	descr: *mut PyObject,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	if descr.is_null() {
		return raise_method("staticmethod object is NULL");
	}
	// SAFETY: tp_call receivers are live PyStaticMethod allocations.
	let staticmethod = unsafe { &*descr.cast::<PyStaticMethod>() };
	if staticmethod.callable.is_null() {
		return raise_method("uninitialized staticmethod object");
	}
	// SAFETY: Delegation through the established call-expansion helper; the
	// positional tuple and keyword mapping ride the star/dstar carriers.
	unsafe {
		crate::abi::call::pon_call_ex(
			staticmethod.callable,
			ptr::null_mut(),
			0,
			args,
			ptr::null(),
			ptr::null_mut(),
			0,
			kwargs,
			ptr::null_mut(),
		)
	}
}

/// Populate slots on the `staticmethod` type descriptor.
pub fn install_staticmethod_slots(ty: &mut PyType) {
	ty.tp_descr_get = Some(staticmethod_descr_get);
	ty.tp_getattro = Some(method_descriptor_getattro);
	ty.tp_call = Some(staticmethod_call);
}

/// Classmethod and staticmethod have no descriptor setter.
pub unsafe extern "C" fn readonly_descr_set(
	_descr: *mut PyObject,
	_obj: *mut PyObject,
	_value: *mut PyObject,
) -> c_int {
	pon_err_set("descriptor is read-only");
	-1
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn staticmethod_returns_wrapped_callable() {
		let mut ty = PyType::new(ptr::null(), "staticmethod", core::mem::size_of::<PyStaticMethod>());
		install_staticmethod_slots(&mut ty);
		let callable = 7usize as *mut PyObject;
		let descr = unsafe { new_staticmethod(&ty, callable) };
		assert_eq!(
			unsafe { staticmethod_descr_get(descr, ptr::null_mut(), ptr::null_mut()) },
			callable
		);
	}
}
