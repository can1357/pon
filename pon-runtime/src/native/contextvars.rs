//! Native `_contextvars` seed (the PEP 567 subset the vendored stdlib uses).
//!
//! `Lib/_py_warnings.py` binds one module-level
//! `ContextVar('warnings_context')` and calls `.get()`/`.set(value)`;
//! `Lib/contextvars.py` re-exports `Context`, `ContextVar`, `Token`, and
//! `copy_context`.  The implementation is a correct simple model, not a HAMT:
//! each thread owns a current [`PyContext`] (an insertion-ordered association
//! list keyed by `ContextVar` identity), `set` mutates the current context in
//! place and returns a single-use [`PyToken`] snapshot, and `Context.run` swaps
//! the thread's current context around the call (mirroring how CPython swaps
//! the thread state's context stack).
//!
//! Objects are immortal leaked boxes like the other native seeds (`_sre`,
//! `_thread`); the Python values they hold (defaults, context entries, token
//! snapshots) live on the GC heap and are reported as roots through
//! [`gc_held_roots`], which `crate::abi::collect` walks.

use core::ffi::c_int;
use std::{
	cell::Cell,
	ptr,
	sync::{LazyLock, Mutex},
};

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{
	abi,
	intern::intern,
	object::{PyObject, PyObjectHeader, PyType},
	thread_state::pon_err_set,
	types::type_::unicode_text,
};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let name = "_contextvars";
	let copy_context = unsafe {
		abi::pon_make_function(
			copy_context_entry as *const u8,
			VARIADIC_ARITY,
			intern("copy_context"),
		)
	};
	if copy_context.is_null() {
		return Err("failed to allocate _contextvars.copy_context".to_owned());
	}
	install_module(name, vec![
		(intern("__name__"), unsafe { abi::pon_const_str(name.as_ptr(), name.len()) }),
		(intern("ContextVar"), contextvar_type().cast::<PyObject>()),
		(intern("Token"), token_type().cast::<PyObject>()),
		(intern("Context"), context_type().cast::<PyObject>()),
		(intern("copy_context"), copy_context),
	])
}

// ---------------------------------------------------------------------------
// Object layouts

#[repr(C)]
struct PyContextVar {
	ob_base:  PyObjectHeader,
	/// Variable name copy used by repr and diagnostics.
	name:     String,
	/// Original name str object echoed by the `.name` attribute.
	name_obj: *mut PyObject,
	/// Constructor default, or NULL when the variable has none.
	default:  *mut PyObject,
}

#[repr(C)]
struct PyToken {
	ob_base:   PyObjectHeader,
	/// The variable this token belongs to (`Token.var`).
	var:       *mut PyObject,
	/// Value the variable held before `set`, or NULL for `Token.MISSING`.
	old_value: *mut PyObject,
	/// Tokens are single-use; `ContextVar.reset` refuses a second pass.
	used:      bool,
}

#[repr(C)]
struct PyContext {
	ob_base: PyObjectHeader,
	/// Insertion-ordered `(variable, value)` pairs keyed by var identity.
	entries: Vec<(*mut PyObject, *mut PyObject)>,
	/// Guard against re-entrant `Context.run`.
	entered: bool,
}

/// `Token.MISSING` sentinel instance layout (header only).
#[repr(C)]
struct PyMissing {
	ob_base: PyObjectHeader,
}

// ---------------------------------------------------------------------------
// Types

static CONTEXTVAR_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"ContextVar",
		std::mem::size_of::<PyContextVar>(),
	);
	// `object` as base wires the generic keyword call path: with a custom
	// `tp_new` slot, `call_type_with_keywords` resolves `__init__` to the
	// inherited `object.__init__` carrier and accepts `default=` keywords.
	ty.tp_base = runtime_object_type();
	ty.tp_new = Some(contextvar_new);
	ty.tp_getattro = Some(contextvar_getattro);
	ty.tp_repr = Some(contextvar_repr);
	ty.tp_str = Some(contextvar_repr);
	ty.tp_hash = Some(identity_hash);
	ty.tp_bool = Some(always_true);
	Box::into_raw(Box::new(ty)) as usize
});

static TOKEN_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty =
		PyType::new(abi::runtime_type_type().cast_const(), "Token", std::mem::size_of::<PyToken>());
	ty.tp_base = runtime_object_type();
	ty.tp_new = Some(token_new);
	ty.tp_getattro = Some(token_getattro);
	ty.tp_repr = Some(token_repr);
	ty.tp_str = Some(token_repr);
	ty.tp_hash = Some(identity_hash);
	ty.tp_bool = Some(always_true);
	// Class attribute `Token.MISSING` (CPython exposes the sentinel only here).
	let namespace = crate::types::type_::new_namespace();
	// SAFETY: `new_namespace` returns a fresh live allocation.
	unsafe { (*namespace).set(intern("MISSING"), missing_object()) };
	ty.tp_dict = namespace.cast::<PyObject>();
	Box::into_raw(Box::new(ty)) as usize
});

static CONTEXT_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"Context",
		std::mem::size_of::<PyContext>(),
	);
	ty.tp_base = runtime_object_type();
	ty.tp_new = Some(context_new);
	ty.tp_getattro = Some(context_getattro);
	ty.tp_hash = Some(identity_hash);
	ty.tp_bool = Some(always_true);
	Box::into_raw(Box::new(ty)) as usize
});

static MISSING_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(ptr::null(), "Token.MISSING", std::mem::size_of::<PyMissing>());
	ty.tp_repr = Some(missing_repr);
	ty.tp_str = Some(missing_repr);
	ty.tp_hash = Some(identity_hash);
	ty.tp_bool = Some(always_true);
	Box::into_raw(Box::new(ty)) as usize
});

/// The `Token.MISSING` singleton (an immortal leaked box, never registered:
/// it holds no GC references).
static MISSING: LazyLock<usize> = LazyLock::new(|| {
	Box::into_raw(Box::new(PyMissing { ob_base: PyObjectHeader::new(*MISSING_TYPE as *mut PyType) }))
		as usize
});

fn contextvar_type() -> *mut PyType {
	*CONTEXTVAR_TYPE as *mut PyType
}

fn token_type() -> *mut PyType {
	*TOKEN_TYPE as *mut PyType
}

fn context_type() -> *mut PyType {
	*CONTEXT_TYPE as *mut PyType
}

fn missing_object() -> *mut PyObject {
	*MISSING as *mut PyObject
}

fn runtime_object_type() -> *mut PyType {
	abi::runtime_global(intern("object")).map_or(ptr::null_mut(), |object| object.cast::<PyType>())
}

// ---------------------------------------------------------------------------
// Thread-local current context and the GC root registry

thread_local! {
	 /// The thread's current `Context`, or NULL before first use (an empty
	 /// context is materialized lazily; new threads start empty like CPython).
	 static CURRENT: Cell<*mut PyObject> = const { Cell::new(ptr::null_mut()) };
}

/// Every `_contextvars` allocation, for GC root reporting.  Objects are
/// immortal leaked boxes, so the registry only grows.
static REGISTRY: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn register(object: *mut PyObject) -> *mut PyObject {
	REGISTRY
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.push(object as usize);
	object
}

/// GC roots held by `_contextvars` state: constructor defaults, echoed name
/// objects, context entry values, and token snapshots.  Consumed by
/// `crate::abi::collect` while the runtime lock is held, so this must not
/// re-enter the runtime; type pointers are read without forcing their
/// `LazyLock`s (uninitialized types mean no objects exist yet).
pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
	let registry = REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
	if registry.is_empty() {
		return Vec::new();
	}
	let var_type = LazyLock::get(&CONTEXTVAR_TYPE).map_or(ptr::null(), |&ty| ty as *const PyType);
	let token_type = LazyLock::get(&TOKEN_TYPE).map_or(ptr::null(), |&ty| ty as *const PyType);
	let context_type = LazyLock::get(&CONTEXT_TYPE).map_or(ptr::null(), |&ty| ty as *const PyType);
	let mut roots = Vec::new();
	let mut push = |value: *mut PyObject| {
		if !value.is_null() && crate::tag::is_heap(value) {
			roots.push(value);
		}
	};
	for &addr in registry.iter() {
		let object = addr as *mut PyObject;
		// SAFETY: Registry members are live leaked allocations of the three
		// contextvars layouts, discriminated by their type pointer.
		unsafe {
			let ty = (*object).ob_type;
			if !var_type.is_null() && ty == var_type {
				push((*object.cast::<PyContextVar>()).name_obj);
				push((*object.cast::<PyContextVar>()).default);
			} else if !token_type.is_null() && ty == token_type {
				push((*object.cast::<PyToken>()).old_value);
			} else if !context_type.is_null() && ty == context_type {
				for &(_, value) in &(*object.cast::<PyContext>()).entries {
					push(value);
				}
			}
		}
	}
	roots
}

// ---------------------------------------------------------------------------
// Allocation and downcasts

fn alloc_contextvar(
	name: String,
	name_obj: *mut PyObject,
	default: *mut PyObject,
) -> *mut PyObject {
	register(
		Box::into_raw(Box::new(PyContextVar {
			ob_base: PyObjectHeader::new(contextvar_type()),
			name,
			name_obj,
			default,
		}))
		.cast::<PyObject>(),
	)
}

pub(crate) fn capi_contextvar_new(name: &str, default: *mut PyObject) -> *mut PyObject {
	let name_obj = alloc_str_object(name);
	if name_obj.is_null() {
		return ptr::null_mut();
	}
	alloc_contextvar(name.to_owned(), name_obj, default)
}

fn alloc_token(var: *mut PyObject, old_value: *mut PyObject) -> *mut PyObject {
	register(
		Box::into_raw(Box::new(PyToken {
			ob_base: PyObjectHeader::new(token_type()),
			var,
			old_value,
			used: false,
		}))
		.cast::<PyObject>(),
	)
}

fn alloc_context(entries: Vec<(*mut PyObject, *mut PyObject)>) -> *mut PyObject {
	register(
		Box::into_raw(Box::new(PyContext {
			ob_base: PyObjectHeader::new(context_type()),
			entries,
			entered: false,
		}))
		.cast::<PyObject>(),
	)
}

unsafe fn as_contextvar<'a>(object: *mut PyObject) -> Option<&'a mut PyContextVar> {
	let object = untag(object);
	if object.is_null() {
		return None;
	}
	// SAFETY: NULL was rejected above; the type check gates the downcast.
	(unsafe { (*object).ob_type } == contextvar_type().cast_const())
		.then(|| unsafe { &mut *object.cast::<PyContextVar>() })
}

unsafe fn as_token<'a>(object: *mut PyObject) -> Option<&'a mut PyToken> {
	let object = untag(object);
	if object.is_null() {
		return None;
	}
	// SAFETY: NULL was rejected above; the type check gates the downcast.
	(unsafe { (*object).ob_type } == token_type().cast_const())
		.then(|| unsafe { &mut *object.cast::<PyToken>() })
}

unsafe fn as_context<'a>(object: *mut PyObject) -> Option<&'a mut PyContext> {
	let object = untag(object);
	if object.is_null() {
		return None;
	}
	// SAFETY: NULL was rejected above; the type check gates the downcast.
	(unsafe { (*object).ob_type } == context_type().cast_const())
		.then(|| unsafe { &mut *object.cast::<PyContext>() })
}

pub(crate) unsafe fn capi_contextvar_get(
	var: *mut PyObject,
	default: *mut PyObject,
	value: *mut *mut PyObject,
) -> c_int {
	if value.is_null() {
		pon_err_set("PyContextVar_Get received a NULL value pointer".to_owned());
		return -1;
	}
	unsafe {
		*value = ptr::null_mut();
	}

	let constructor_default = match unsafe { as_contextvar(var) } {
		Some(var) => var.default,
		None => {
			let _ = raise_type_error("PyContextVar_Get expected a ContextVar");
			return -1;
		},
	};
	let var = untag(var);
	let current = CURRENT.with(Cell::get);
	if !current.is_null() {
		let Some(context) = (unsafe { as_context(current) }) else {
			pon_err_set("current context is invalid".to_owned());
			return -1;
		};
		for &(entry_var, entry_value) in &context.entries {
			if entry_var == var {
				unsafe {
					*value = entry_value;
				}
				return 0;
			}
		}
	}

	let fallback = if !default.is_null() {
		default
	} else {
		constructor_default
	};
	if !fallback.is_null() {
		unsafe {
			*value = fallback;
		}
	}
	0
}

/// The thread's current context object, materializing an empty one on first
/// mutation-side use.
fn current_context_or_create() -> *mut PyObject {
	let current = CURRENT.with(Cell::get);
	if !current.is_null() {
		return current;
	}
	let context = alloc_context(Vec::new());
	CURRENT.with(|cell| cell.set(context));
	context
}

// ---------------------------------------------------------------------------
// Shared slot implementations and small helpers

unsafe extern "C" fn identity_hash(object: *mut PyObject) -> isize {
	object.addr() as isize
}

unsafe extern "C" fn always_true(_object: *mut PyObject) -> c_int {
	1
}

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

fn alloc_str_object(text: &str) -> *mut PyObject {
	// SAFETY: Runtime allocation helper; NULL on failure with the error set.
	unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn raise_type_error(message: &str) -> *mut PyObject {
	// SAFETY: Message bytes are a live UTF-8 slice for the duration of the call.
	unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn raise_value_error(message: &str) -> *mut PyObject {
	// SAFETY: Message bytes are a live UTF-8 slice for the duration of the call.
	unsafe { abi::exc::pon_raise_value_error(message.as_ptr(), message.len()) }
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

fn contextvar_repr_text(var: &PyContextVar) -> String {
	format!("<ContextVar name='{}' at {:#x}>", var.name, ptr::from_ref(var).addr())
}

// ---------------------------------------------------------------------------
// ContextVar

unsafe extern "C" fn contextvar_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	let positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
		Ok(positional) => positional,
		Err(message) => return fail(message),
	};
	if positional.is_empty() {
		return raise_type_error("ContextVar() missing required argument 'name' (pos 1)");
	}
	if positional.len() > 1 {
		let message =
			format!("ContextVar() takes exactly 1 positional argument ({} given)", positional.len());
		return raise_type_error(&message);
	}
	let name_obj = untag(positional[0]);
	let Some(name) = (unsafe { unicode_text(name_obj) }) else {
		return raise_type_error("context variable name must be a str");
	};
	let mut default = ptr::null_mut();
	if !kwargs.is_null() {
		// `call_type_with_keywords` materializes keywords as a real dict.
		let entries = match unsafe { crate::types::dict::dict_entries_snapshot(kwargs) } {
			Ok(entries) => entries,
			Err(message) => return fail(message),
		};
		for entry in entries {
			match unsafe { unicode_text(untag(entry.key)) } {
				Some("default") => default = entry.value,
				Some(other) => {
					let message = format!("ContextVar() got an unexpected keyword argument '{other}'");
					return raise_type_error(&message);
				},
				None => return raise_type_error("ContextVar() keywords must be strings"),
			}
		}
	}
	alloc_contextvar(name.to_owned(), name_obj, default)
}

unsafe extern "C" fn contextvar_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(name_text) = (unsafe { unicode_text(untag(name)) }) else {
		return fail("attribute name must be str");
	};
	let Some(var) = (unsafe { as_contextvar(object) }) else {
		return fail("ContextVar receiver is invalid");
	};
	match name_text {
		"name" => var.name_obj,
		"get" => bound_method(object, name_text, var_get_method),
		"set" => bound_method(object, name_text, var_set_method),
		"reset" => bound_method(object, name_text, var_reset_method),
		// SAFETY: Raise helper with the interned attribute name.
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

unsafe extern "C" fn contextvar_repr(object: *mut PyObject) -> *mut PyObject {
	let Some(var) = (unsafe { as_contextvar(object) }) else {
		return fail("ContextVar receiver is invalid");
	};
	alloc_str_object(&contextvar_repr_text(var))
}

/// `ContextVar.get([default])`: current-context binding, else the call
/// default, else the constructor default, else `LookupError(var)`.
unsafe extern "C" fn var_get_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("ContextVar.get received a NULL argv pointer");
	};
	let Some((&receiver, rest)) = args.split_first() else {
		return fail("ContextVar.get requires a receiver");
	};
	let Some(var) = (unsafe { as_contextvar(receiver) }) else {
		return fail("ContextVar.get receiver is invalid");
	};
	if rest.len() > 1 {
		let message = format!("get() takes at most 1 argument ({} given)", rest.len());
		return raise_type_error(&message);
	}
	let receiver = untag(receiver);
	let current = CURRENT.with(Cell::get);
	if !current.is_null() {
		if let Some(context) = unsafe { as_context(current) } {
			for &(entry_var, value) in &context.entries {
				if entry_var == receiver {
					return value;
				}
			}
		}
	}
	if let Some(&default) = rest.first() {
		return default;
	}
	if !var.default.is_null() {
		return var.default;
	}
	abi::exc::raise_lookup_error_text(&contextvar_repr_text(var))
}

/// `ContextVar.set(value)`: rebinds the variable in the thread's current
/// context and returns a single-use `Token` snapshot of the prior state.
unsafe extern "C" fn var_set_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("ContextVar.set received a NULL argv pointer");
	};
	let &[receiver, value] = args else {
		let given = args.len().saturating_sub(1);
		let message = format!("set() takes exactly 1 argument ({given} given)");
		return raise_type_error(&message);
	};
	if unsafe { as_contextvar(receiver) }.is_none() {
		return fail("ContextVar.set receiver is invalid");
	}
	let receiver = untag(receiver);
	let context_obj = current_context_or_create();
	let Some(context) = (unsafe { as_context(context_obj) }) else {
		return fail("current context is invalid");
	};
	let slot = context
		.entries
		.iter_mut()
		.find(|(entry_var, _)| *entry_var == receiver);
	let token = match slot {
		Some((_, existing)) => {
			let token = alloc_token(receiver, *existing);
			*existing = value;
			token
		},
		None => {
			context.entries.push((receiver, value));
			alloc_token(receiver, ptr::null_mut())
		},
	};
	token
}

/// `ContextVar.reset(token)`: restores the state captured by `set`.
unsafe extern "C" fn var_reset_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("ContextVar.reset received a NULL argv pointer");
	};
	let &[receiver, token_obj] = args else {
		let given = args.len().saturating_sub(1);
		let message = format!("reset() takes exactly 1 argument ({given} given)");
		return raise_type_error(&message);
	};
	if unsafe { as_contextvar(receiver) }.is_none() {
		return fail("ContextVar.reset receiver is invalid");
	}
	let Some(token) = (unsafe { as_token(token_obj) }) else {
		return raise_type_error("expected an instance of Token");
	};
	if token.used {
		return abi::exc::raise_runtime_error_text("Token has already been used once");
	}
	let receiver = untag(receiver);
	if token.var != receiver {
		return raise_value_error("Token was created by a different ContextVar");
	}
	let context_obj = current_context_or_create();
	let Some(context) = (unsafe { as_context(context_obj) }) else {
		return fail("current context is invalid");
	};
	if token.old_value.is_null() {
		context
			.entries
			.retain(|(entry_var, _)| *entry_var != receiver);
	} else {
		match context
			.entries
			.iter_mut()
			.find(|(entry_var, _)| *entry_var == receiver)
		{
			Some((_, existing)) => *existing = token.old_value,
			None => context.entries.push((receiver, token.old_value)),
		}
	}
	token.used = true;
	none()
}

// ---------------------------------------------------------------------------
// Token

unsafe extern "C" fn token_new(
	_cls: *mut PyType,
	_args: *mut PyObject,
	_kwargs: *mut PyObject,
) -> *mut PyObject {
	abi::exc::raise_runtime_error_text("Tokens can only be created by ContextVars")
}

unsafe extern "C" fn token_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let Some(name_text) = (unsafe { unicode_text(untag(name)) }) else {
		return fail("attribute name must be str");
	};
	let Some(token) = (unsafe { as_token(object) }) else {
		return fail("Token receiver is invalid");
	};
	match name_text {
		"var" => token.var,
		"old_value" => {
			if token.old_value.is_null() {
				missing_object()
			} else {
				token.old_value
			}
		},
		"MISSING" => missing_object(),
		// SAFETY: Raise helper with the interned attribute name.
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

unsafe extern "C" fn token_repr(object: *mut PyObject) -> *mut PyObject {
	let Some(token) = (unsafe { as_token(object) }) else {
		return fail("Token receiver is invalid");
	};
	let used = if token.used { " used" } else { "" };
	let var = match unsafe { as_contextvar(token.var) } {
		Some(var) => contextvar_repr_text(var),
		None => "<invalid>".to_owned(),
	};
	alloc_str_object(&format!("<Token{used} var={var} at {:#x}>", object.addr()))
}

unsafe extern "C" fn missing_repr(_object: *mut PyObject) -> *mut PyObject {
	alloc_str_object("<Token.MISSING>")
}

// ---------------------------------------------------------------------------
// Context

unsafe extern "C" fn context_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	let positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
		Ok(positional) => positional,
		Err(message) => return fail(message),
	};
	if !positional.is_empty() || !kwargs.is_null() {
		return raise_type_error("Context() takes no arguments");
	}
	alloc_context(Vec::new())
}

unsafe extern "C" fn context_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let Some(name_text) = (unsafe { unicode_text(untag(name)) }) else {
		return fail("attribute name must be str");
	};
	if unsafe { as_context(object) }.is_none() {
		return fail("Context receiver is invalid");
	}
	match name_text {
		"run" => bound_method(object, name_text, context_run_method),
		"get" => bound_method(object, name_text, context_get_method),
		"copy" => bound_method(object, name_text, context_copy_method),
		// SAFETY: Raise helper with the interned attribute name.
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

/// `Context.run(callable, *args)`: swaps the thread's current context to the
/// receiver around the call, refusing re-entry like CPython.
unsafe extern "C" fn context_run_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("Context.run received a NULL argv pointer");
	};
	let Some((&receiver, call)) = args.split_first() else {
		return fail("Context.run requires a receiver");
	};
	let Some(context) = (unsafe { as_context(receiver) }) else {
		return fail("Context.run receiver is invalid");
	};
	let Some(&callable) = call.first() else {
		return raise_type_error("run() missing 1 required positional argument: 'callable'");
	};
	if context.entered {
		let message =
			format!("cannot enter context: {:#x} is already entered", untag(receiver).addr());
		return abi::exc::raise_runtime_error_text(&message);
	}
	context.entered = true;
	let previous = CURRENT.with(|cell| cell.replace(untag(receiver)));
	let call_args = &call[1..];
	// SAFETY: `call_args` borrows the live tail of the caller's argv.
	let result = unsafe {
		abi::pon_call(
			callable,
			if call_args.is_empty() {
				ptr::null_mut()
			} else {
				call_args.as_ptr().cast_mut()
			},
			call_args.len(),
		)
	};
	CURRENT.with(|cell| cell.set(previous));
	context.entered = false;
	result
}

/// `Context.get(var[, default])`: identity lookup with a None fallback.
unsafe extern "C" fn context_get_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("Context.get received a NULL argv pointer");
	};
	let Some((&receiver, rest)) = args.split_first() else {
		return fail("Context.get requires a receiver");
	};
	let Some(context) = (unsafe { as_context(receiver) }) else {
		return fail("Context.get receiver is invalid");
	};
	let Some(&var) = rest.first() else {
		return raise_type_error("get() takes at least 1 argument (0 given)");
	};
	if rest.len() > 2 {
		let message = format!("get() takes at most 2 arguments ({} given)", rest.len());
		return raise_type_error(&message);
	}
	let var = untag(var);
	for &(entry_var, value) in &context.entries {
		if entry_var == var {
			return value;
		}
	}
	rest.get(1).copied().unwrap_or_else(none)
}

/// `Context.copy()`: shallow copy with independent entries.
unsafe extern "C" fn context_copy_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
		return fail("Context.copy received a NULL argv pointer");
	};
	let &[receiver] = args else {
		return raise_type_error("copy() takes no arguments");
	};
	let Some(context) = (unsafe { as_context(receiver) }) else {
		return fail("Context.copy receiver is invalid");
	};
	alloc_context(context.entries.clone())
}

// ---------------------------------------------------------------------------
// Module functions

/// `copy_context()`: shallow copy of the thread's current context (empty when
/// the thread has never touched a variable).
unsafe extern "C" fn copy_context_entry(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		let message = format!("copy_context() takes no arguments ({argc} given)");
		return raise_type_error(&message);
	}
	let current = CURRENT.with(Cell::get);
	let entries = if current.is_null() {
		Vec::new()
	} else {
		match unsafe { as_context(current) } {
			Some(context) => context.entries.clone(),
			None => return fail("current context is invalid"),
		}
	};
	alloc_context(entries)
}
