//! Native `_thread` module: the surface the vendored 3.14 `threading.py`
//! binds at import plus the original free-threading hooks.
//!
//! Real OS threads exist only under the `free-threading` feature (the GC and
//! type machinery require registered threads).  Default builds run
//! `start_joinable_thread` bodies inline on the calling thread under a
//! synthetic `get_ident()` override, which keeps `threading.Thread`
//! start/join, `current_thread()` bookkeeping, and `RLock` owner accounting
//! coherent and deterministic for single-threaded conformance runs.
//!
//! Objects are immortal leaked boxes like the other native seeds; the Python
//! values held by `_local` instances are reported as GC roots through
//! [`gc_held_roots`] (the `_contextvars` pattern).

use std::{
	cell::Cell,
	collections::HashMap,
	ffi::{CStr, CString},
	ptr,
	sync::{
		Arc, Condvar, LazyLock, Mutex,
		atomic::{AtomicI64, Ordering},
	},
	time::{Duration, Instant},
};

use num_traits::ToPrimitive;

use super::install_module;
use crate::{
	abi::{
		self,
		exc::pon_raise_attribute_error,
		number::{pon_const_bool, pon_const_float},
		pon_call, pon_const_int, pon_is_true, pon_load_global, pon_make_function, pon_none,
		pon_thread_start_new,
	},
	intern::intern,
	native::builtins_mod::VARIADIC_ARITY,
	object::{PyObject, PyObjectHeader, PySequenceMethods, PyType},
	thread_state::pon_err_set,
	types::{exc::ExceptionKind, method, type_},
};

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

/// `_thread.TIMEOUT_MAX` (CPython 3.14 darwin: `PY_TIMEOUT_MAX` microseconds
/// as seconds).
const TIMEOUT_MAX: f64 = 9_223_372_036.0;
const NAME_MAXLEN: usize = 63;
static ACTIVE_THREAD_COUNT: AtomicI64 = AtomicI64::new(1);

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	// Pin the main-thread ident while make_module still runs on it (the
	// `_thread` seed is in `EAGER_MODULES`, installed during runtime init).
	let _ = *MAIN_THREAD_IDENT;
	let error_type = {
		let value = unsafe { pon_load_global(intern("RuntimeError"), ptr::null_mut()) };
		if value.is_null() {
			return Err("RuntimeError is not registered for _thread.error".to_string());
		}
		value
	};
	let timeout_max = unsafe { pon_const_float(TIMEOUT_MAX) };
	if timeout_max.is_null() {
		return Err("failed to allocate _thread.TIMEOUT_MAX".to_string());
	}
	install_module("_thread", vec![
		(intern("start_new_thread"), module_function("start_new_thread", native_start_new_thread)?),
		(intern("start_new"), module_function("start_new", native_start_new_thread)?),
		(
			intern("start_joinable_thread"),
			module_function("start_joinable_thread", native_start_joinable_thread)?,
		),
		(
			intern("daemon_threads_allowed"),
			module_function("daemon_threads_allowed", native_daemon_threads_allowed)?,
		),
		(intern("allocate_lock"), module_function("allocate_lock", native_allocate_lock)?),
		(intern("allocate"), module_function("allocate", native_allocate_lock)?),
		(intern("get_ident"), module_function("get_ident", native_get_ident)?),
		(intern("get_native_id"), module_function("get_native_id", native_get_native_id)?),
		(intern("_count"), module_function("_count", native_count)?),
		(intern("_get_name"), module_function("_get_name", native_get_name)?),
		(intern("set_name"), module_function("set_name", native_set_name)?),
		(intern("interrupt_main"), module_function("interrupt_main", native_interrupt_main)?),
		(intern("exit"), module_function("exit", native_exit)?),
		(intern("exit_thread"), module_function("exit_thread", native_exit)?),
		(
			intern("_get_main_thread_ident"),
			module_function("_get_main_thread_ident", native_get_main_thread_ident)?,
		),
		(
			intern("_is_main_interpreter"),
			module_function("_is_main_interpreter", native_is_main_interpreter)?,
		),
		(intern("_shutdown"), module_function("_shutdown", native_shutdown)?),
		(
			intern("_make_thread_handle"),
			module_function("_make_thread_handle", native_make_thread_handle)?,
		),
		(intern("stack_size"), module_function("stack_size", native_stack_size)?),
		(intern("RLock"), module_function("RLock", native_rlock_new)?),
		(intern("LockType"), lock_type().cast::<PyObject>()),
		(intern("lock"), lock_type().cast::<PyObject>()),
		(intern("_ThreadHandle"), thread_handle_type().cast::<PyObject>()),
		(intern("_ExceptHookArgs"), except_hook_args_type().cast::<PyObject>()),
		(intern("_excepthook"), module_function("_excepthook", native_excepthook)?),
		(intern("_local"), local_type().cast::<PyObject>()),
		(intern("error"), error_type),
		(intern("TIMEOUT_MAX"), timeout_max),
		(intern("_NAME_MAXLEN"), unsafe { pon_const_int(NAME_MAXLEN as i64) }),
	])
}

fn module_function(name: &str, entry: BuiltinFn) -> Result<*mut PyObject, String> {
	// SAFETY: `entry` is a live builtin entry with the runtime calling convention.
	let function = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	if function.is_null() {
		return Err(format!("failed to allocate _thread.{name}"));
	}
	Ok(function)
}

/// Base type for the native seeds below (`object`), wiring the generic
/// keyword call path for types with a custom `tp_new`.
fn runtime_object_type() -> *mut PyType {
	abi::runtime_global(intern("object")).map_or(ptr::null_mut(), |object| object.cast::<PyType>())
}

/// Builds a bound native method for a `tp_getattro` hit.
fn bound_method(receiver: *mut PyObject, name: &str, entry: BuiltinFn) -> *mut PyObject {
	// SAFETY: `entry` is a live builtin entry with the runtime calling convention.
	let function = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	if function.is_null() {
		return ptr::null_mut();
	}
	match method::new_bound_method(function, receiver) {
		Ok(method) => method.cast::<PyObject>(),
		Err(message) => {
			pon_err_set(message);
			ptr::null_mut()
		},
	}
}

// ---------------------------------------------------------------------------
// Thread excepthook arguments

const EXCEPT_HOOK_FIELDS: [&str; 4] = ["exc_type", "exc_value", "exc_traceback", "thread"];

#[repr(C)]
struct PyExceptHookArgs {
	ob_base: PyObjectHeader,
	fields:  [*mut PyObject; 4],
}

static EXCEPT_HOOK_ARGS_SEQUENCE: LazyLock<PySequenceMethods> =
	LazyLock::new(|| PySequenceMethods {
		sq_length: Some(except_hook_args_len),
		sq_item: Some(except_hook_args_item),
		..PySequenceMethods::EMPTY
	});

static EXCEPT_HOOK_ARGS_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"_thread._ExceptHookArgs",
		std::mem::size_of::<PyExceptHookArgs>(),
	);
	ty.tp_base = runtime_object_type();
	ty.tp_new = Some(except_hook_args_new);
	ty.tp_getattro = Some(except_hook_args_getattro);
	ty.tp_repr = Some(except_hook_args_repr);
	ty.tp_as_sequence =
		&*EXCEPT_HOOK_ARGS_SEQUENCE as *const PySequenceMethods as *mut PySequenceMethods;
	Box::into_raw(Box::new(ty)) as usize
});

static EXCEPT_HOOK_ARGS_REGISTRY: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn except_hook_args_type() -> *mut PyType {
	*EXCEPT_HOOK_ARGS_TYPE as *mut PyType
}

fn alloc_except_hook_args(fields: [*mut PyObject; 4]) -> *mut PyObject {
	let object = Box::into_raw(Box::new(PyExceptHookArgs {
		ob_base: PyObjectHeader::new(except_hook_args_type()),
		fields,
	}));
	EXCEPT_HOOK_ARGS_REGISTRY
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.push(object as usize);
	object.cast::<PyObject>()
}

unsafe fn as_except_hook_args<'a>(object: *mut PyObject) -> Option<&'a PyExceptHookArgs> {
	let object = crate::tag::untag_arg(object);
	if object.is_null() || unsafe { (*object).ob_type } != except_hook_args_type().cast_const() {
		return None;
	}
	Some(unsafe { &*object.cast::<PyExceptHookArgs>() })
}

fn thread_type_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

unsafe extern "C" fn except_hook_args_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	if !kwargs.is_null() {
		return thread_type_error("_thread._ExceptHookArgs() takes no keyword arguments");
	}
	let positional = match unsafe { type_::positional_args_from_object(args) } {
		Ok(positional) => positional,
		Err(message) => return thread_type_error(&message),
	};
	let values = match positional.as_slice() {
		[] => Vec::new(),
		[iterable] => match crate::abi::seq::sequence_to_vec(*iterable) {
			Ok(values) => values,
			Err(message) => return thread_type_error(&message),
		},
		_ => {
			return thread_type_error(&format!(
				"_thread._ExceptHookArgs() takes at most 1 argument ({} given)",
				positional.len()
			));
		},
	};
	if values.len() != EXCEPT_HOOK_FIELDS.len() {
		return thread_type_error(&format!(
			"_thread._ExceptHookArgs() takes a 4-sequence ({}-sequence given)",
			values.len()
		));
	}
	alloc_except_hook_args([values[0], values[1], values[2], values[3]])
}

unsafe extern "C" fn except_hook_args_len(_object: *mut PyObject) -> isize {
	EXCEPT_HOOK_FIELDS.len() as isize
}

unsafe extern "C" fn except_hook_args_item(object: *mut PyObject, index: isize) -> *mut PyObject {
	let Ok(index) = usize::try_from(index) else {
		return abi::exc::raise_kind_error_text(
			ExceptionKind::IndexError,
			"tuple index out of range",
		);
	};
	let Some(args) = (unsafe { as_except_hook_args(object) }) else {
		return thread_type_error("_ExceptHookArgs receiver is invalid");
	};
	args.fields.get(index).copied().unwrap_or_else(|| {
		abi::exc::raise_kind_error_text(ExceptionKind::IndexError, "tuple index out of range")
	})
}

unsafe extern "C" fn except_hook_args_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(name_text) = (unsafe { type_::unicode_text(crate::tag::untag_arg(name)) }) else {
		return abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			"attribute name must be str",
		);
	};
	if name_text == "n_fields" || name_text == "n_sequence_fields" {
		return unsafe { pon_const_int(EXCEPT_HOOK_FIELDS.len() as i64) };
	}
	if name_text == "n_unnamed_fields" {
		return unsafe { pon_const_int(0) };
	}
	let Some(args) = (unsafe { as_except_hook_args(object) }) else {
		return thread_type_error("_ExceptHookArgs receiver is invalid");
	};
	if let Some(index) = EXCEPT_HOOK_FIELDS
		.iter()
		.position(|&field| field == name_text)
	{
		return args.fields[index];
	}
	unsafe { pon_raise_attribute_error(object, intern(name_text)) }
}

unsafe extern "C" fn except_hook_args_repr(object: *mut PyObject) -> *mut PyObject {
	let Some(args) = (unsafe { as_except_hook_args(object) }) else {
		return thread_type_error("_ExceptHookArgs receiver is invalid");
	};
	let text = format!(
		"_thread._ExceptHookArgs(exc_type={}, exc_value={}, exc_traceback={}, thread={})",
		super::builtins_mod::repr_text(args.fields[0]),
		super::builtins_mod::repr_text(args.fields[1]),
		super::builtins_mod::repr_text(args.fields[2]),
		super::builtins_mod::repr_text(args.fields[3]),
	);
	unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) }
}

unsafe extern "C" fn native_excepthook(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	use std::io::Write;

	if argc != 1 || argv.is_null() {
		return thread_type_error(&format!(
			"_thread._excepthook() takes exactly one argument ({argc} given)"
		));
	}
	let Some(args) = (unsafe { as_except_hook_args(*argv) }) else {
		return thread_type_error("_thread.excepthook argument type must be ExceptHookArgs");
	};
	if Some(args.fields[0]) == crate::import::module_attr(intern("builtins"), intern("SystemExit")) {
		return unsafe { pon_none() };
	}
	let thread = args.fields[3];
	let name = if !is_none_object(thread) {
		let attr = unsafe { abi::object::pon_get_attr(thread, intern("name"), ptr::null_mut()) };
		if attr.is_null() {
			crate::thread_state::pon_err_clear();
			current_ident().to_string()
		} else {
			super::builtins_mod::str_text(attr)
		}
	} else {
		current_ident().to_string()
	};
	let value = args.fields[1];
	let type_name = exception_type_name(args.fields[0], value);
	let message = super::builtins_mod::str_text(value);
	let mut stderr = std::io::stderr().lock();
	let outcome = writeln!(stderr, "Exception in thread {name}:").and_then(|()| {
		if message.is_empty() {
			writeln!(stderr, "{type_name}")
		} else {
			writeln!(stderr, "{type_name}: {message}")
		}
	});
	if let Err(error) = outcome.and_then(|()| stderr.flush()) {
		pon_err_set(format!("_thread._excepthook() failed to write stderr: {error}"));
		return ptr::null_mut();
	}
	unsafe { pon_none() }
}

fn is_none_object(object: *mut PyObject) -> bool {
	crate::tag::untag_arg(object) == unsafe { pon_none() }
}

fn exception_type_name(exc_type: *mut PyObject, value: *mut PyObject) -> String {
	let exc_type = crate::tag::untag_arg(exc_type);
	if !exc_type.is_null() && unsafe { (*exc_type).ob_type } == abi::runtime_type_type().cast_const()
	{
		return unsafe { (*exc_type.cast::<PyType>()).name() }.to_owned();
	}
	let value = crate::tag::untag_arg(value);
	if value.is_null() {
		return "<unknown>".to_owned();
	}
	let ty = unsafe { (*value).ob_type };
	if ty.is_null() {
		"<unknown>".to_owned()
	} else {
		unsafe { (*ty).name() }.to_owned()
	}
}

// ---------------------------------------------------------------------------
// Idents

/// CPython `_thread.get_ident()`: a nonzero integer unique per live thread.
unsafe extern "C" fn native_get_ident(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	unsafe { pon_const_int(current_ident()) }
}

thread_local! {
	 /// Nonzero while an inline joinable-thread body runs on this OS thread
	 /// (single-threaded builds); the synthetic value stands in for the ident
	 /// the body would observe on its own OS thread.
	 static IDENT_OVERRIDE: Cell<i64> = const { Cell::new(0) };
}

/// Synthetic idents for inline joinable threads.  The base is far below any
/// mapped address on supported targets, so values never collide with the
/// anchor-address idents of real threads.
static SYNTHETIC_IDENT: AtomicI64 = AtomicI64::new(0x7061_0001);

/// Nonzero integer unique per live thread: the address of a thread-local
/// cell, stable for the thread's lifetime (shared by `get_ident` and the
/// `RLock` owner bookkeeping), unless an inline joinable-thread override is
/// active.
fn current_ident() -> i64 {
	let overridden = IDENT_OVERRIDE.with(Cell::get);
	if overridden != 0 {
		return overridden;
	}
	thread_local! {
		 static IDENT_ANCHOR: u8 = const { 0 };
	}
	IDENT_ANCHOR.with(|slot| core::ptr::from_ref(slot) as i64)
}

/// The ident of the thread that installed the native modules, pinned by
/// `make_module` before any Python code runs.
static MAIN_THREAD_IDENT: LazyLock<i64> = LazyLock::new(current_ident);

unsafe extern "C" fn native_get_main_thread_ident(
	_argv: *mut *mut PyObject,
	_argc: usize,
) -> *mut PyObject {
	unsafe { pon_const_int(*MAIN_THREAD_IDENT) }
}

unsafe extern "C" fn native_is_main_interpreter(
	_argv: *mut *mut PyObject,
	_argc: usize,
) -> *mut PyObject {
	unsafe { pon_const_bool(1) }
}

/// `_thread.daemon_threads_allowed()`: always true in the main interpreter.
unsafe extern "C" fn native_daemon_threads_allowed(
	_argv: *mut *mut PyObject,
	_argc: usize,
) -> *mut PyObject {
	unsafe { pon_const_bool(1) }
}

/// `_thread._shutdown()`: joining of straggler non-daemon threads at exit.
/// Inline threads finish before `start()` returns and real threads are only
/// spawned under free-threading where process exit does not wait, so there
/// is nothing to join.
unsafe extern "C" fn native_shutdown(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
	unsafe { pon_none() }
}

/// `_thread.stack_size([size])`: pon never adjusts stack sizes; 0 means
/// "platform default" like CPython.
unsafe extern "C" fn native_stack_size(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc > 1 {
		pon_err_set(format!("stack_size() takes at most 1 argument ({argc} given)"));
		return ptr::null_mut();
	}
	unsafe { pon_const_int(0) }
}

unsafe extern "C" fn native_exit(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		pon_err_set(format!("exit() takes no arguments ({argc} given)"));
		return ptr::null_mut();
	}
	crate::abi::exc::raise_system_exit(ptr::null_mut())
}

unsafe extern "C" fn native_interrupt_main(
	_argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	if argc > 1 {
		pon_err_set(format!("interrupt_main() takes at most 1 argument ({argc} given)"));
		return ptr::null_mut();
	}
	crate::abi::exc::raise_kind_error_no_args(ExceptionKind::KeyboardInterrupt)
}

unsafe extern "C" fn native_count(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		pon_err_set(format!("_count() takes no arguments ({argc} given)"));
		return ptr::null_mut();
	}
	unsafe { pon_const_int(ACTIVE_THREAD_COUNT.load(Ordering::Relaxed)) }
}

unsafe extern "C" fn native_get_native_id(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		pon_err_set(format!("get_native_id() takes no arguments ({argc} given)"));
		return ptr::null_mut();
	}
	#[cfg(target_os = "macos")]
	{
		let mut tid: u64 = 0;
		let rc = unsafe { libc::pthread_threadid_np(0, &mut tid) };
		if rc != 0 {
			pon_err_set(format!("pthread_threadid_np failed with errno {rc}"));
			return ptr::null_mut();
		}
		return unsafe { pon_const_int(tid as i64) };
	}
	#[cfg(not(target_os = "macos"))]
	{
		unsafe { pon_const_int(current_ident()) }
	}
}

unsafe extern "C" fn native_get_name(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		pon_err_set(format!("_get_name() takes no arguments ({argc} given)"));
		return ptr::null_mut();
	}
	let mut buffer = [0 as libc::c_char; NAME_MAXLEN + 1];
	#[cfg(target_os = "macos")]
	{
		let rc = unsafe {
			libc::pthread_getname_np(libc::pthread_self(), buffer.as_mut_ptr(), buffer.len())
		};
		if rc != 0 {
			pon_err_set(format!("pthread_getname_np failed with errno {rc}"));
			return ptr::null_mut();
		}
	}
	let text = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_string_lossy();
	unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) }
}

unsafe extern "C" fn native_set_name(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 1 || argv.is_null() {
		pon_err_set(format!("set_name() takes exactly one argument ({argc} given)"));
		return ptr::null_mut();
	}
	let Some(name) = (unsafe { type_::unicode_text(crate::tag::untag_arg(*argv)) }) else {
		pon_err_set("set_name() argument must be str");
		return ptr::null_mut();
	};
	let mut end = name.len().min(NAME_MAXLEN);
	while !name.is_char_boundary(end) {
		end -= 1;
	}
	let Ok(c_name) = CString::new(&name.as_bytes()[..end]) else {
		pon_err_set("thread name must not contain null bytes");
		return ptr::null_mut();
	};
	#[cfg(target_os = "macos")]
	{
		let rc = unsafe { libc::pthread_setname_np(c_name.as_ptr()) };
		if rc != 0 {
			pon_err_set(format!("pthread_setname_np failed with errno {rc}"));
			return ptr::null_mut();
		}
	}
	unsafe { pon_none() }
}

// ---------------------------------------------------------------------------
// start_new_thread (free-threading stress surface)

unsafe extern "C" fn native_start_new_thread(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	if argc < 2 || argv.is_null() {
		pon_err_set("_thread.start_new_thread requires a callable and args tuple");
		return ptr::null_mut();
	}
	if argc > 2 {
		pon_err_set("_thread.start_new_thread kwargs are not supported");
		return ptr::null_mut();
	}
	// SAFETY: The call helper supplies `argv` with at least `argc` entries.
	let callable = unsafe { *argv };
	let args = unsafe { *argv.add(1) };
	let args = match crate::abi::seq::sequence_to_vec(args) {
		Ok(args) => args,
		Err(message) => {
			pon_err_set(message);
			return ptr::null_mut();
		},
	};
	let call = Box::new(ThreadCall { callable, args });
	let call_arg = Box::into_raw(call).cast::<PyObject>();
	ACTIVE_THREAD_COUNT.fetch_add(1, Ordering::Relaxed);
	let status = unsafe { pon_thread_start_new(start_new_trampoline as *const u8, call_arg) };
	if status != 0 {
		ACTIVE_THREAD_COUNT.fetch_sub(1, Ordering::Relaxed);
		unsafe { drop(Box::from_raw(call_arg.cast::<ThreadCall>())) };
		return ptr::null_mut();
	}
	unsafe { pon_const_int(1) }
}

struct ThreadCall {
	callable: *mut PyObject,
	args:     Vec<*mut PyObject>,
}

unsafe extern "C" fn start_new_trampoline(call: *mut PyObject) -> *mut PyObject {
	if call.is_null() {
		pon_err_set("_thread.start_new_thread call record is null");
		return ptr::null_mut();
	}
	let mut call = unsafe { Box::from_raw(call.cast::<ThreadCall>()) };
	let argc = call.args.len();
	let argv = if argc == 0 {
		ptr::null_mut()
	} else {
		call.args.as_mut_ptr()
	};
	let result = unsafe { pon_call(call.callable, argv, argc) };
	ACTIVE_THREAD_COUNT.fetch_sub(1, Ordering::Relaxed);
	result
}

// ---------------------------------------------------------------------------
// Thread handles (`_ThreadHandle`, `start_joinable_thread`,
// `_make_thread_handle`)

#[repr(C)]
struct PyThreadHandle {
	_ob_base: PyObjectHeader,
	state:    Arc<HandleState>,
}

struct HandleState {
	inner: Mutex<HandleInner>,
	done:  Condvar,
}

struct HandleInner {
	ident: i64,
	done:  bool,
}

impl HandleState {
	fn new(ident: i64) -> Self {
		Self { inner: Mutex::new(HandleInner { ident, done: false }), done: Condvar::new() }
	}

	fn lock(&self) -> std::sync::MutexGuard<'_, HandleInner> {
		self
			.inner
			.lock()
			.unwrap_or_else(|poison| poison.into_inner())
	}

	fn ident(&self) -> i64 {
		self.lock().ident
	}

	fn set_ident(&self, ident: i64) {
		self.lock().ident = ident;
	}

	fn is_done(&self) -> bool {
		self.lock().done
	}

	fn set_done(&self) {
		self.lock().done = true;
		self.done.notify_all();
	}

	/// Blocks until done; a timeout returns without error like CPython
	/// (`Thread.join` re-checks `is_alive` afterwards).
	fn join(&self, timeout: Option<f64>) {
		let mut inner = self.lock();
		match timeout {
			None => {
				while !inner.done {
					inner = self
						.done
						.wait(inner)
						.unwrap_or_else(|poison| poison.into_inner());
				}
			},
			Some(seconds) => {
				let deadline = Instant::now() + Duration::from_secs_f64(seconds.max(0.0));
				while !inner.done {
					let now = Instant::now();
					if now >= deadline {
						break;
					}
					let (guard, _timed_out) = self
						.done
						.wait_timeout(inner, deadline - now)
						.unwrap_or_else(|poison| poison.into_inner());
					inner = guard;
				}
			},
		}
	}
}

static THREAD_HANDLE_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"_ThreadHandle",
		std::mem::size_of::<PyThreadHandle>(),
	);
	ty.tp_base = runtime_object_type();
	ty.tp_new = Some(thread_handle_new);
	ty.tp_getattro = Some(thread_handle_getattro);
	Box::into_raw(Box::new(ty)) as usize
});

fn thread_handle_type() -> *mut PyType {
	*THREAD_HANDLE_TYPE as *mut PyType
}

fn alloc_thread_handle(state: Arc<HandleState>) -> *mut PyObject {
	Box::into_raw(Box::new(PyThreadHandle {
		_ob_base: PyObjectHeader::new(thread_handle_type().cast_const()),
		state,
	}))
	.cast::<PyObject>()
}

/// `_thread._ThreadHandle()`: a fresh not-started handle (`threading.Thread.
/// __init__` allocates one per thread object).
unsafe extern "C" fn thread_handle_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	_kwargs: *mut PyObject,
) -> *mut PyObject {
	match unsafe { type_::positional_args_from_object(args) } {
		Ok(positional) if positional.is_empty() => alloc_thread_handle(Arc::new(HandleState::new(0))),
		Ok(positional) => {
			pon_err_set(format!("_ThreadHandle() takes no arguments ({} given)", positional.len()));
			ptr::null_mut()
		},
		Err(message) => {
			pon_err_set(message);
			ptr::null_mut()
		},
	}
}

unsafe extern "C" fn thread_handle_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name) = (unsafe { type_::unicode_text(name) }) else {
		pon_err_set("_ThreadHandle attribute name must be str");
		return ptr::null_mut();
	};
	let entry: BuiltinFn = match name {
		"ident" => {
			let Some(state) = handle_state(object) else {
				pon_err_set("_ThreadHandle receiver is invalid");
				return ptr::null_mut();
			};
			return unsafe { pon_const_int(state.ident()) };
		},
		"join" => handle_join_entry,
		"is_done" => handle_is_done_entry,
		"_set_done" => handle_set_done_entry,
		// SAFETY: Raise helper with the interned attribute name.
		_ => return unsafe { pon_raise_attribute_error(object, intern(name)) },
	};
	bound_method(object, name, entry)
}

fn handle_state(object: *mut PyObject) -> Option<&'static HandleState> {
	if object.is_null() || unsafe { (*object).ob_type } != thread_handle_type().cast_const() {
		return None;
	}
	Some(unsafe { &*(*object.cast::<PyThreadHandle>()).state })
}

fn handle_receiver(argv: *mut *mut PyObject, argc: usize) -> Option<&'static HandleState> {
	if argc == 0 || argv.is_null() {
		pon_err_set("_ThreadHandle method missing receiver");
		return None;
	}
	let state = handle_state(unsafe { *argv });
	if state.is_none() {
		pon_err_set("_ThreadHandle method receiver is not a _ThreadHandle");
	}
	state
}

/// `_ThreadHandle.join(timeout=None)`: blocks until the thread finishes; a
/// numeric timeout bounds the wait and returns None either way.
unsafe extern "C" fn handle_join_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(state) = handle_receiver(argv, argc) else {
		return ptr::null_mut();
	};
	if argc > 2 {
		pon_err_set(format!("join() takes at most 1 argument ({} given)", argc - 1));
		return ptr::null_mut();
	}
	let none = unsafe { pon_none() };
	let mut timeout = None;
	if argc == 2 {
		// SAFETY: The call helper supplies `argv` with `argc` entries.
		let value = crate::tag::untag_arg(unsafe { *argv.add(1) });
		if value.is_null() {
			return ptr::null_mut();
		}
		if value != none {
			let Some(seconds) = object_seconds(value) else {
				pon_err_set("join() timeout must be a number or None");
				return ptr::null_mut();
			};
			timeout = Some(seconds);
		}
	}
	if !state.is_done() && state.ident() == current_ident() {
		pon_err_set("Cannot join current thread");
		return ptr::null_mut();
	}
	state.join(timeout);
	none
}

unsafe extern "C" fn handle_is_done_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(state) = handle_receiver(argv, argc) else {
		return ptr::null_mut();
	};
	unsafe { pon_const_bool(i32::from(state.is_done())) }
}

unsafe extern "C" fn handle_set_done_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(state) = handle_receiver(argv, argc) else {
		return ptr::null_mut();
	};
	state.set_done();
	unsafe { pon_none() }
}

/// `_thread._make_thread_handle(ident)`: a handle for an already-running
/// thread (`_MainThread` and `_DummyThread` bookkeeping).
unsafe extern "C" fn native_make_thread_handle(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	if argc != 1 || argv.is_null() {
		pon_err_set(format!("_make_thread_handle() takes exactly 1 argument ({argc} given)"));
		return ptr::null_mut();
	}
	// SAFETY: The call helper supplies `argv` with at least one entry.
	let ident_object = crate::tag::untag_arg(unsafe { *argv });
	if ident_object.is_null() {
		return ptr::null_mut();
	}
	let Some(ident) = (unsafe { crate::types::int::to_bigint_including_bool(ident_object) })
		.and_then(|value| value.to_i64())
	else {
		pon_err_set("_make_thread_handle() ident must be an int");
		return ptr::null_mut();
	};
	alloc_thread_handle(Arc::new(HandleState::new(ident)))
}

/// Payload for a spawned joinable thread (free-threading builds).
struct JoinableCall {
	callable: *mut PyObject,
	state:    Arc<HandleState>,
}

unsafe extern "C" fn joinable_trampoline(call: *mut PyObject) -> *mut PyObject {
	if call.is_null() {
		pon_err_set("joinable thread call record is null");
		return ptr::null_mut();
	}
	let call = unsafe { Box::from_raw(call.cast::<JoinableCall>()) };
	call.state.set_ident(current_ident());
	let result = unsafe { pon_call(call.callable, ptr::null_mut(), 0) };
	// Done is flagged even when the body raised: `Thread._bootstrap` already
	// reported the exception and joiners must not hang.
	call.state.set_done();
	ACTIVE_THREAD_COUNT.fetch_sub(1, Ordering::Relaxed);
	result
}

/// `_thread.start_joinable_thread(function, handle=None, daemon=True)`.
///
/// Free-threading builds spawn a registered OS thread.  Default builds run
/// `function` inline under a synthetic `get_ident()` override (see module
/// docs); the handle is done when the call returns, so `join` never blocks.
unsafe extern "C" fn native_start_joinable_thread(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	if argc == 0 || argv.is_null() {
		pon_err_set("start_joinable_thread() requires a callable");
		return ptr::null_mut();
	}
	if argc > 3 {
		pon_err_set(format!("start_joinable_thread() takes at most 3 arguments ({argc} given)"));
		return ptr::null_mut();
	}
	let none = unsafe { pon_none() };
	// SAFETY: The call helper supplies `argv` with `argc` entries; the
	// keyword binder canonicalizes absent slots to None.
	let callable = unsafe { *argv };
	let mut handle_object = if argc >= 2 {
		crate::tag::untag_arg(unsafe { *argv.add(1) })
	} else {
		none
	};
	// Slot 3 (`daemon`) is accepted and ignored: inline threads finish
	// eagerly and free-threading exit never waits on threads.
	if handle_object.is_null() {
		return ptr::null_mut();
	}
	if handle_object == none {
		handle_object = alloc_thread_handle(Arc::new(HandleState::new(0)));
	} else if handle_state(handle_object).is_none() {
		pon_err_set("start_joinable_thread() handle must be a _ThreadHandle");
		return ptr::null_mut();
	}
	if handle_state(handle_object).is_none() {
		return ptr::null_mut();
	}
	// The trampoline and joiners must share the one Arc stored in the
	// handle object.
	let state = unsafe { &(*handle_object.cast::<PyThreadHandle>()).state }.clone();
	if cfg!(feature = "free-threading") {
		let call = Box::new(JoinableCall { callable, state });
		let call_arg = Box::into_raw(call).cast::<PyObject>();
		ACTIVE_THREAD_COUNT.fetch_add(1, Ordering::Relaxed);
		let status = unsafe { pon_thread_start_new(joinable_trampoline as *const u8, call_arg) };
		if status != 0 {
			ACTIVE_THREAD_COUNT.fetch_sub(1, Ordering::Relaxed);
			unsafe { drop(Box::from_raw(call_arg.cast::<JoinableCall>())) };
			return ptr::null_mut();
		}
		return handle_object;
	}
	let synthetic = SYNTHETIC_IDENT.fetch_add(2, Ordering::Relaxed);
	let previous = IDENT_OVERRIDE.with(|cell| cell.replace(synthetic));
	state.set_ident(synthetic);
	let result = unsafe { pon_call(callable, ptr::null_mut(), 0) };
	IDENT_OVERRIDE.with(|cell| cell.set(previous));
	state.set_done();
	if result.is_null() {
		return ptr::null_mut();
	}
	handle_object
}

/// Coerces a Python number to seconds.
fn object_seconds(value: *mut PyObject) -> Option<f64> {
	if let Some(float) = unsafe { crate::types::float::to_f64(value) } {
		return Some(float);
	}
	unsafe { crate::types::int::to_bigint_including_bool(value) }.and_then(|value| value.to_f64())
}

// ---------------------------------------------------------------------------
// Lock

#[repr(C)]
struct PyLock {
	_ob_base: PyObjectHeader,
	state:    Box<LockState>,
}

struct LockState {
	locked:    Mutex<bool>,
	available: Condvar,
}

impl LockState {
	fn new() -> Self {
		Self { locked: Mutex::new(false), available: Condvar::new() }
	}

	fn lock(&self) -> std::sync::MutexGuard<'_, bool> {
		self
			.locked
			.lock()
			.unwrap_or_else(|poison| poison.into_inner())
	}

	fn acquire(&self) {
		let mut locked = self.lock();
		while *locked {
			locked = self
				.available
				.wait(locked)
				.unwrap_or_else(|poison| poison.into_inner());
		}
		*locked = true;
	}

	fn try_acquire(&self) -> bool {
		let mut locked = self.lock();
		if *locked {
			false
		} else {
			*locked = true;
			true
		}
	}

	fn acquire_timeout(&self, timeout: f64) -> bool {
		let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
		let mut locked = self.lock();
		while *locked {
			let now = Instant::now();
			if now >= deadline {
				return false;
			}
			let (guard, _timed_out) = self
				.available
				.wait_timeout(locked, deadline - now)
				.unwrap_or_else(|poison| poison.into_inner());
			locked = guard;
		}
		*locked = true;
		true
	}

	fn is_locked(&self) -> bool {
		*self.lock()
	}

	fn release(&self) -> Result<(), &'static str> {
		let mut locked = self.lock();
		if !*locked {
			return Err("release unlocked lock");
		}
		*locked = false;
		self.available.notify_one();
		Ok(())
	}

	/// CPython `lock._at_fork_reinit()`: reset to a fresh unlocked lock in
	/// the child after `fork` (post-fork the parent's holder is gone, so
	/// waiters must not inherit a locked state).
	fn at_fork_reinit(&self) {
		let mut locked = self.lock();
		*locked = false;
		self.available.notify_all();
	}
}

static LOCK_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty =
		PyType::new(abi::runtime_type_type().cast_const(), "lock", std::mem::size_of::<PyLock>());
	ty.tp_base = runtime_object_type();
	ty.tp_new = Some(lock_new);
	ty.tp_getattro = Some(lock_getattro);
	Box::into_raw(Box::new(ty)) as usize
});

fn lock_type() -> *mut PyType {
	*LOCK_TYPE as *mut PyType
}

fn alloc_lock() -> *mut PyObject {
	Box::into_raw(Box::new(PyLock {
		_ob_base: PyObjectHeader::new(lock_type().cast_const()),
		state:    Box::new(LockState::new()),
	}))
	.cast::<PyObject>()
}

/// `_thread.LockType()` (`threading.Lock`): a fresh unlocked lock.
unsafe extern "C" fn lock_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	_kwargs: *mut PyObject,
) -> *mut PyObject {
	match unsafe { type_::positional_args_from_object(args) } {
		Ok(positional) if positional.is_empty() => alloc_lock(),
		Ok(positional) => {
			pon_err_set(format!("lock() takes no arguments ({} given)", positional.len()));
			ptr::null_mut()
		},
		Err(message) => {
			pon_err_set(message);
			ptr::null_mut()
		},
	}
}

unsafe extern "C" fn lock_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name) = (unsafe { type_::unicode_text(name) }) else {
		pon_err_set("lock attribute name must be str");
		return ptr::null_mut();
	};
	let entry: BuiltinFn = match name {
		"acquire" | "__enter__" => lock_acquire_entry,
		"release" => lock_release_entry,
		"__exit__" => lock_exit_entry,
		"locked" => lock_locked_entry,
		"_at_fork_reinit" => lock_at_fork_reinit_entry,
		// SAFETY: Raise helper with the interned attribute name (Condition's
		// `_release_save`/`_acquire_restore`/`_is_owned` probes must see
		// AttributeError to engage their Python fallbacks).
		_ => return unsafe { pon_raise_attribute_error(object, intern(name)) },
	};
	bound_method(object, name, entry)
}

unsafe extern "C" fn native_allocate_lock(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		pon_err_set(format!("allocate_lock() takes no arguments ({argc} given)"));
		return ptr::null_mut();
	}
	alloc_lock()
}

/// Parsed `acquire(blocking=True, timeout=-1)` shape shared by Lock entries.
enum AcquireMode {
	NonBlocking,
	Blocking,
	Timed(f64),
}

fn parse_acquire_mode(argv: *mut *mut PyObject, argc: usize) -> Result<AcquireMode, ()> {
	if argc > 3 {
		pon_err_set(format!("acquire() takes at most 2 arguments ({} given)", argc - 1));
		return Err(());
	}
	let mut blocking = true;
	if argc >= 2 {
		// SAFETY: The call helper supplies `argv` with `argc` entries;
		// `pon_is_true` self-normalizes its argument.
		match unsafe { pon_is_true(*argv.add(1)) } {
			-1 => return Err(()),
			0 => blocking = false,
			_ => {},
		}
	}
	let mut timeout = None;
	if argc == 3 {
		// SAFETY: As above.
		let value = crate::tag::untag_arg(unsafe { *argv.add(2) });
		if value.is_null() {
			return Err(());
		}
		if value != unsafe { pon_none() } {
			let Some(seconds) = object_seconds(value) else {
				pon_err_set("acquire() timeout must be a number or None");
				return Err(());
			};
			if seconds >= 0.0 {
				timeout = Some(seconds);
			}
		}
	}
	match (blocking, timeout) {
		(false, Some(_)) => {
			pon_err_set("can't specify a timeout for a non-blocking call");
			Err(())
		},
		(false, None) => Ok(AcquireMode::NonBlocking),
		(true, None) => Ok(AcquireMode::Blocking),
		(true, Some(seconds)) => Ok(AcquireMode::Timed(seconds)),
	}
}

/// `lock.acquire(blocking=True, timeout=-1)` / `lock.__enter__()`: returns
/// whether the lock was acquired (`Condition.wait` drives the timed path
/// through its waiter locks).
unsafe extern "C" fn lock_acquire_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(lock) = lock_receiver(argv, argc) else {
		return ptr::null_mut();
	};
	let Ok(mode) = parse_acquire_mode(argv, argc) else {
		return ptr::null_mut();
	};
	let state = &unsafe { &*lock }.state;
	let acquired = match mode {
		AcquireMode::NonBlocking => state.try_acquire(),
		AcquireMode::Blocking => {
			state.acquire();
			true
		},
		AcquireMode::Timed(seconds) => state.acquire_timeout(seconds),
	};
	unsafe { pon_const_bool(i32::from(acquired)) }
}

unsafe extern "C" fn lock_release_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(lock) = lock_receiver(argv, argc) else {
		return ptr::null_mut();
	};
	match unsafe { &*lock }.state.release() {
		Ok(()) => unsafe { pon_none() },
		Err(message) => {
			pon_err_set(message);
			ptr::null_mut()
		},
	}
}

/// `lock.__exit__(exc_type, exc_value, traceback)`: releases and returns
/// None so exceptions propagate.
unsafe extern "C" fn lock_exit_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc == 0 || argv.is_null() {
		pon_err_set("lock.__exit__ missing receiver");
		return ptr::null_mut();
	}
	unsafe { lock_release_entry(argv, 1) }
}

unsafe extern "C" fn lock_locked_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(lock) = lock_receiver(argv, argc) else {
		return ptr::null_mut();
	};
	unsafe { pon_const_bool(i32::from((*lock).state.is_locked())) }
}

/// Bound `lock._at_fork_reinit()`: resets the lock to unlocked (fork-child
/// hygiene; `concurrent.futures.thread` registers it via
/// `os.register_at_fork`).
unsafe extern "C" fn lock_at_fork_reinit_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let Some(lock) = lock_receiver(argv, argc) else {
		return ptr::null_mut();
	};
	if argc != 1 {
		pon_err_set(format!("_at_fork_reinit() takes no arguments ({} given)", argc - 1));
		return ptr::null_mut();
	}
	unsafe { (*lock).state.at_fork_reinit() };
	unsafe { pon_none() }
}

fn lock_receiver(argv: *mut *mut PyObject, argc: usize) -> Option<*mut PyLock> {
	if argc == 0 || argv.is_null() {
		pon_err_set("lock method missing receiver");
		return None;
	}
	let receiver = unsafe { *argv };
	if receiver.is_null() || unsafe { (*receiver).ob_type } != lock_type().cast_const() {
		pon_err_set("lock method receiver is not a lock");
		return None;
	}
	Some(receiver.cast::<PyLock>())
}

// ---------------------------------------------------------------------------
// RLock

#[repr(C)]
struct PyRLock {
	_ob_base: PyObjectHeader,
	state:    Box<RLockState>,
}

struct RLockState {
	inner:     Mutex<RLockInner>,
	available: Condvar,
}

/// `owner` is a `current_ident()` value (never 0 for a live thread); 0 marks
/// the unowned state.
struct RLockInner {
	owner: i64,
	count: usize,
}

impl RLockState {
	fn new() -> Self {
		Self { inner: Mutex::new(RLockInner { owner: 0, count: 0 }), available: Condvar::new() }
	}

	fn acquire(&self) {
		let me = current_ident();
		let mut inner = self
			.inner
			.lock()
			.unwrap_or_else(|poison| poison.into_inner());
		if inner.owner == me {
			inner.count += 1;
			return;
		}
		while inner.count != 0 {
			inner = self
				.available
				.wait(inner)
				.unwrap_or_else(|poison| poison.into_inner());
		}
		inner.owner = me;
		inner.count = 1;
	}

	fn release(&self) -> Result<(), &'static str> {
		let me = current_ident();
		let mut inner = self
			.inner
			.lock()
			.unwrap_or_else(|poison| poison.into_inner());
		if inner.count == 0 || inner.owner != me {
			return Err("cannot release un-acquired lock");
		}
		inner.count -= 1;
		if inner.count == 0 {
			inner.owner = 0;
			self.available.notify_one();
		}
		Ok(())
	}

	fn is_owned(&self) -> bool {
		let me = current_ident();
		let inner = self
			.inner
			.lock()
			.unwrap_or_else(|poison| poison.into_inner());
		inner.count != 0 && inner.owner == me
	}

	fn is_locked(&self) -> bool {
		let inner = self
			.inner
			.lock()
			.unwrap_or_else(|poison| poison.into_inner());
		inner.count != 0
	}
}

static RLOCK_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(ptr::null(), "RLock", std::mem::size_of::<PyRLock>());
	ty.tp_getattro = Some(rlock_getattro);
	Box::into_raw(Box::new(ty)) as usize
});

fn rlock_type() -> *mut PyType {
	*RLOCK_TYPE as *mut PyType
}

/// `_thread.RLock()`: allocates a reentrant lock (arguments are rejected like
/// CPython's `RLock` type constructor).
unsafe extern "C" fn native_rlock_new(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		pon_err_set(format!("RLock() takes no arguments ({argc} given)"));
		return ptr::null_mut();
	}
	Box::into_raw(Box::new(PyRLock {
		_ob_base: PyObjectHeader::new(rlock_type().cast_const()),
		state:    Box::new(RLockState::new()),
	}))
	.cast::<PyObject>()
}

unsafe extern "C" fn rlock_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name) = (unsafe { type_::unicode_text(name) }) else {
		pon_err_set("RLock attribute name must be str");
		return ptr::null_mut();
	};
	let entry: BuiltinFn = match name {
		"acquire" | "__enter__" => rlock_acquire_entry,
		"release" => rlock_release_entry,
		"__exit__" => rlock_exit_entry,
		"_is_owned" => rlock_is_owned_entry,
		"locked" => rlock_locked_entry,
		// SAFETY: Raise helper with the interned attribute name (Condition's
		// `_release_save`/`_acquire_restore` probes fall back on miss).
		_ => return unsafe { pon_raise_attribute_error(object, intern(name)) },
	};
	bound_method(object, name, entry)
}

/// `RLock.acquire(blocking=True, timeout=-1)` / `RLock.__enter__()`: always
/// blocks until owned (the only mode the embedded stdlib exercises) and
/// returns True.
unsafe extern "C" fn rlock_acquire_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(rlock) = rlock_receiver(argv, argc) else {
		return ptr::null_mut();
	};
	unsafe { &*rlock }.state.acquire();
	unsafe { pon_const_bool(1) }
}

unsafe extern "C" fn rlock_release_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(rlock) = rlock_receiver(argv, argc) else {
		return ptr::null_mut();
	};
	match unsafe { &*rlock }.state.release() {
		Ok(()) => unsafe { pon_none() },
		Err(message) => {
			pon_err_set(message);
			ptr::null_mut()
		},
	}
}

/// `RLock.__exit__(exc_type, exc_value, traceback)`: releases and returns
/// None so exceptions propagate.
unsafe extern "C" fn rlock_exit_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc == 0 || argv.is_null() {
		pon_err_set("RLock.__exit__ missing receiver");
		return ptr::null_mut();
	}
	unsafe { rlock_release_entry(argv, 1) }
}

/// `RLock._is_owned()`: whether the calling thread holds the lock
/// (`threading.Condition` consults it before wait/notify).
unsafe extern "C" fn rlock_is_owned_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(rlock) = rlock_receiver(argv, argc) else {
		return ptr::null_mut();
	};
	unsafe { pon_const_bool(i32::from((*rlock).state.is_owned())) }
}
unsafe extern "C" fn rlock_locked_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(rlock) = rlock_receiver(argv, argc) else {
		return ptr::null_mut();
	};
	unsafe { pon_const_bool(i32::from((*rlock).state.is_locked())) }
}

fn rlock_receiver(argv: *mut *mut PyObject, argc: usize) -> Option<*mut PyRLock> {
	if argc == 0 || argv.is_null() {
		pon_err_set("RLock method missing receiver");
		return None;
	}
	let receiver = unsafe { *argv };
	if receiver.is_null() || unsafe { (*receiver).ob_type } != rlock_type().cast_const() {
		pon_err_set("RLock method receiver is not an RLock");
		return None;
	}
	Some(receiver.cast::<PyRLock>())
}

// ---------------------------------------------------------------------------
// _local

#[repr(C)]
struct PyLocal {
	_ob_base: PyObjectHeader,
	/// Per-thread attribute namespaces: `current_ident()` → interned name →
	/// value address (possibly tagged).
	cells:    Mutex<HashMap<i64, HashMap<u32, usize>>>,
}

static LOCAL_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty =
		PyType::new(abi::runtime_type_type().cast_const(), "_local", std::mem::size_of::<PyLocal>());
	ty.tp_base = runtime_object_type();
	ty.tp_new = Some(local_new);
	ty.tp_getattro = Some(local_getattro);
	ty.tp_setattro = Some(local_setattro);
	Box::into_raw(Box::new(ty)) as usize
});

fn local_type() -> *mut PyType {
	*LOCAL_TYPE as *mut PyType
}

/// Every `_local` allocation, for GC root reporting.  Instances are immortal
/// leaked boxes, so the registry only grows.
static LOCAL_REGISTRY: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// GC roots held by `_local` instances (the values stored per thread).
/// Consumed by `crate::abi::collect` while the runtime lock is held, so this
/// must not re-enter the runtime.
pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
	let registry = LOCAL_REGISTRY
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	let mut roots = Vec::new();
	for &addr in registry.iter() {
		// SAFETY: Registry members are live leaked `PyLocal` allocations.
		let cells = unsafe { &(*(addr as *mut PyLocal)).cells };
		let cells = cells.lock().unwrap_or_else(|poison| poison.into_inner());
		for namespace in cells.values() {
			for &value in namespace.values() {
				let value = value as *mut PyObject;
				if !value.is_null() && crate::tag::is_heap(value) {
					roots.push(value);
				}
			}
		}
	}
	roots
}

/// `_thread._local()`: thread-local attribute namespace (`threading` keeps
/// its dummy-thread tracker in one).  Constructor arguments are accepted and
/// ignored (CPython forwards them to a subclass `__init__`).
unsafe extern "C" fn local_new(
	_cls: *mut PyType,
	_args: *mut PyObject,
	_kwargs: *mut PyObject,
) -> *mut PyObject {
	let object = Box::into_raw(Box::new(PyLocal {
		_ob_base: PyObjectHeader::new(local_type().cast_const()),
		cells:    Mutex::new(HashMap::new()),
	}))
	.cast::<PyObject>();
	LOCAL_REGISTRY
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.push(object as usize);
	object
}

fn local_cells(object: *mut PyObject) -> Option<&'static Mutex<HashMap<i64, HashMap<u32, usize>>>> {
	if object.is_null() || unsafe { (*object).ob_type } != local_type().cast_const() {
		return None;
	}
	Some(unsafe { &(*object.cast::<PyLocal>()).cells })
}

unsafe extern "C" fn local_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { type_::unicode_text(name) }) else {
		pon_err_set("_local attribute name must be str");
		return ptr::null_mut();
	};
	let Some(cells) = local_cells(object) else {
		pon_err_set("_local receiver is invalid");
		return ptr::null_mut();
	};
	let interned = intern(name_text);
	let cells = cells.lock().unwrap_or_else(|poison| poison.into_inner());
	match cells
		.get(&current_ident())
		.and_then(|namespace| namespace.get(&interned))
	{
		Some(&value) => value as *mut PyObject,
		// SAFETY: Raise helper with the interned attribute name.
		None => unsafe { pon_raise_attribute_error(object, interned) },
	}
}

unsafe extern "C" fn local_setattro(
	object: *mut PyObject,
	name: *mut PyObject,
	value: *mut PyObject,
) -> core::ffi::c_int {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { type_::unicode_text(name) }) else {
		pon_err_set("_local attribute name must be str");
		return -1;
	};
	let Some(cells) = local_cells(object) else {
		pon_err_set("_local receiver is invalid");
		return -1;
	};
	let interned = intern(name_text);
	let mut cells = cells.lock().unwrap_or_else(|poison| poison.into_inner());
	let ident = current_ident();
	if value.is_null() {
		// Deletion (`del local.attr`).
		let removed = cells
			.get_mut(&ident)
			.and_then(|namespace| namespace.remove(&interned));
		if removed.is_none() {
			// SAFETY: Raise helper with the interned attribute name.
			unsafe { pon_raise_attribute_error(object, interned) };
			return -1;
		}
		return 0;
	}
	cells
		.entry(ident)
		.or_default()
		.insert(interned, value as usize);
	0
}
