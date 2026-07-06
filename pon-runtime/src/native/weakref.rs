//! Native `_weakref` module: the C-accelerated core `Lib/weakref.py` binds at
//! import — the `ref`/`proxy` type aliases plus the introspection and
//! dead-entry removal helpers.
//!
//! The pure-Python `weakref` module itself runs from the vendored stdlib
//! (there is deliberately NO `weakref` registry row): `WeakKeyDictionary`,
//! `WeakValueDictionary`, `WeakSet`, `WeakMethod`, and `finalize` are
//! CPython's own `MutableMapping`/`MutableSet` subclasses, so the ABC mixin
//! methods (`clear`, `update`, `setdefault`, ...) resolve through the normal
//! MRO walk instead of a native shim surface.

use std::ptr;

use super::install_module;
use crate::{
	abi::{self, pon_make_function, pon_none},
	intern::intern,
	native::builtins_mod::VARIADIC_ARITY,
	object::PyObject,
	thread_state::pon_err_set,
	types::{dict, exc::ExceptionKind, weakref as weakref_types},
};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

pub(super) fn make_underscore_module() -> Result<*mut PyObject, String> {
	let name = "_weakref";
	let attrs = vec![
		(intern("__name__"), unsafe { abi::pon_const_str(name.as_ptr(), name.len()) }),
		(intern("ref"), weakref_types::weakref_ref_type()),
		(intern("ReferenceType"), weakref_types::weakref_ref_type()),
		(intern("proxy"), weakref_types::weakref_proxy_type()),
		(intern("ProxyType"), weakref_types::weakref_proxy_type()),
		(intern("CallableProxyType"), weakref_types::weakref_proxy_type()),
		(intern("getweakrefcount"), module_function("getweakrefcount", native_getweakrefcount)?),
		(intern("getweakrefs"), module_function("getweakrefs", native_getweakrefs)?),
		(
			intern("_remove_dead_weakref"),
			module_function("_remove_dead_weakref", native_remove_dead_weakref)?,
		),
	];
	install_module(name, attrs)
}

fn module_function(name: &str, entry: BuiltinFn) -> Result<*mut PyObject, String> {
	// SAFETY: `entry` is a live builtin entry with the runtime calling convention.
	let function = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	if function.is_null() {
		return Err(format!("failed to allocate _weakref.{name}"));
	}
	Ok(function)
}

fn raise_type_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

/// Exact-arity positional args of a module-level native function.
unsafe fn native_args<'a>(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
	expect: usize,
) -> Option<&'a [*mut PyObject]> {
	if argv.is_null() {
		pon_err_set(format!("_weakref.{name} received a NULL argv pointer"));
		return None;
	}
	let args = unsafe { std::slice::from_raw_parts(argv, argc) };
	if args.len() != expect {
		raise_type_error(&format!(
			"{name} expected exactly {expect} argument(s) ({} given)",
			args.len()
		));
		return None;
	}
	Some(args)
}

/// `_weakref.getweakrefcount(object)`: number of live weak references
/// (including proxies and subclass instances) targeting `object`.
unsafe extern "C" fn native_getweakrefcount(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let Some(args) = (unsafe { native_args(argv, argc, "getweakrefcount", 1) }) else {
		return ptr::null_mut();
	};
	let count = weakref_types::weakrefs_of(args[0]).len();
	unsafe { abi::pon_const_int(count as i64) }
}

/// `_weakref.getweakrefs(object)`: list of live weak references targeting
/// `object` (subclass wrappers reported as the instances user code built).
unsafe extern "C" fn native_getweakrefs(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { native_args(argv, argc, "getweakrefs", 1) }) else {
		return ptr::null_mut();
	};
	super::builtins_mod::alloc_list(weakref_types::weakrefs_of(args[0]))
}

/// `_weakref._remove_dead_weakref(dct, key)`: atomically remove `dct[key]`
/// when the stored weak reference is dead.  A missing key is silently
/// tolerated (the entry may already be gone — CPython issue #28427); a
/// present non-weakref value raises `TypeError` ("not a weakref").
unsafe extern "C" fn native_remove_dead_weakref(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let Some(args) = (unsafe { native_args(argv, argc, "_remove_dead_weakref", 2) }) else {
		return ptr::null_mut();
	};
	let dct = crate::tag::untag_arg(args[0]);
	let key = args[1];
	let value = match unsafe { dict::dict_get(dct, key) } {
		Ok(value) => value,
		Err(_) => return raise_type_error("_remove_dead_weakref: first argument must be a dict"),
	};
	let Some(value) = value else {
		return unsafe { pon_none() };
	};
	let Some(referent) = (unsafe { weakref_types::weakref_referent_any(value) }) else {
		return raise_type_error("not a weakref");
	};
	if referent.is_null() {
		if let Err(message) = unsafe { dict::dict_remove(dct, key) } {
			pon_err_set(message);
			return ptr::null_mut();
		}
	}
	unsafe { pon_none() }
}
