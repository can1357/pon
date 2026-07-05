//! Native `_multiprocessing` support backed by POSIX named semaphores.
//!
//! CPython's POSIX `_multiprocessing` extension exposes only the `SemLock`
//! semaphore/mutex wrapper plus `sem_unlink` and build flags.  Pon already has
//! real POSIX process/fd primitives (`os.fork`, `os.waitpid`,
//! `_posixsubprocess`), so this module intentionally implements that semaphore
//! surface and nothing from Windows-only connection/socket support.

use std::{
	ffi::CString,
	ptr,
	sync::{LazyLock, Mutex},
	time::{Duration, Instant},
};

use num_traits::ToPrimitive;

use super::install_module;
use crate::{
	abi::{
		self, pon_const_bool, pon_const_int, pon_const_str, pon_is_true, pon_make_function, pon_none,
	},
	intern::intern,
	object::{PyObject, PyObjectHeader, PyType},
	thread_state::pon_err_set,
	types::{dict, exc::ExceptionKind, float, int, method, type_},
};

const RECURSIVE_MUTEX: i32 = 0;
const SEMAPHORE: i32 = 1;
const POLL_INTERVAL: Duration = Duration::from_millis(1);
type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

type SemHandle = *mut libc::sem_t;

#[repr(C)]
struct PySemLock {
	_ob_base: PyObjectHeader,
	handle:   SemHandle,
	kind:     i32,
	maxvalue: i32,
	name:     Option<String>,
	state:    Mutex<SemLockState>,
}

struct SemLockState {
	last_tid: u64,
	count:    i32,
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	install_module("_multiprocessing", vec![
		(intern("__name__"), py_str("_multiprocessing")?),
		(intern("SemLock"), semlock_type().cast::<PyObject>()),
		(intern("sem_unlink"), module_function("sem_unlink", sem_unlink_entry)?),
		(intern("flags"), flags_dict()?),
	])
}

fn module_function(name: &str, entry: BuiltinFn) -> Result<*mut PyObject, String> {
	// SAFETY: `entry` is a live builtin entry with the runtime calling convention.
	let function = unsafe {
		pon_make_function(
			entry as *const u8,
			crate::native::builtins_mod::VARIADIC_ARITY,
			intern(name),
		)
	};
	if function.is_null() {
		return Err(format!("failed to allocate _multiprocessing.{name}"));
	}
	Ok(function)
}

fn bound_method(receiver: *mut PyObject, name: &str, entry: BuiltinFn) -> *mut PyObject {
	// SAFETY: `entry` is a live builtin entry with the runtime calling convention.
	let function = unsafe {
		pon_make_function(
			entry as *const u8,
			crate::native::builtins_mod::VARIADIC_ARITY,
			intern(name),
		)
	};
	if function.is_null() {
		return ptr::null_mut();
	}
	match method::new_bound_method(function, receiver) {
		Ok(method) => method.cast::<PyObject>(),
		Err(message) => return_error(message),
	}
}

fn py_str(value: &str) -> Result<*mut PyObject, String> {
	// SAFETY: The bytes are valid UTF-8 and live for the duration of the call.
	let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
	if object.is_null() {
		Err(format!("failed to allocate string {value:?}"))
	} else {
		Ok(object)
	}
}

fn py_int(value: i64) -> *mut PyObject {
	// SAFETY: Integer boxing helper; callers check for NULL where needed.
	unsafe { pon_const_int(value) }
}

fn py_bool(value: bool) -> *mut PyObject {
	// SAFETY: Bool boxing helper; callers check for NULL where needed.
	unsafe { pon_const_bool(i32::from(value)) }
}

fn flags_dict() -> Result<*mut PyObject, String> {
	let mut pairs: Vec<*mut PyObject> = Vec::new();
	push_flag(&mut pairs, "HAVE_SEM_OPEN", 1)?;
	#[cfg(target_os = "macos")]
	push_flag(&mut pairs, "HAVE_BROKEN_SEM_GETVALUE", 1)?;
	#[cfg(not(target_os = "macos"))]
	push_flag(&mut pairs, "HAVE_SEM_TIMEDWAIT", 1)?;

	// SAFETY: Pairs are `[key, value, ...]` live objects; the helper allocates
	// an exact dict and pre-hashes outside the runtime lock.
	let dict = unsafe { abi::map::pon_build_map(pairs.as_mut_ptr(), pairs.len() / 2) };
	if dict.is_null() {
		Err("failed to allocate _multiprocessing.flags".to_owned())
	} else {
		Ok(dict)
	}
}

fn push_flag(pairs: &mut Vec<*mut PyObject>, name: &str, value: i64) -> Result<(), String> {
	let key = py_str(name)?;
	let value = py_int(value);
	if value.is_null() {
		return Err(format!("failed to allocate _multiprocessing.flags[{name:?}]"));
	}
	pairs.push(key);
	pairs.push(value);
	Ok(())
}

static SEMLOCK_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"SemLock",
		core::mem::size_of::<PySemLock>(),
	);
	ty.tp_base = abi::runtime_global(intern("object"))
		.map_or(ptr::null_mut(), |object| object.cast::<PyType>());
	ty.tp_new = Some(semlock_new);
	ty.tp_getattro = Some(semlock_getattro);

	let namespace = type_::new_namespace();
	if !namespace.is_null() {
		set_type_str(namespace, "__module__", "_multiprocessing");
		set_type_str(namespace, "__doc__", "Semaphore/Mutex type");
		set_type_int(namespace, "SEM_VALUE_MAX", i64::from(sem_value_max()));
		for &(name, entry) in SEMLOCK_METHODS {
			set_type_function(namespace, name, entry);
		}
		ty.tp_dict = namespace.cast::<PyObject>();
	}

	let ty = Box::into_raw(Box::new(ty));
	crate::sync::register_namespaced_type(ty);
	crate::sync::type_modified(ty);
	ty as usize
});

fn semlock_type() -> *mut PyType {
	*SEMLOCK_TYPE as *mut PyType
}

const SEMLOCK_METHODS: &[(&str, BuiltinFn)] = &[
	("acquire", semlock_acquire_entry),
	("release", semlock_release_entry),
	("__enter__", semlock_enter_entry),
	("__exit__", semlock_exit_entry),
	("_count", semlock_count_entry),
	("_is_mine", semlock_is_mine_entry),
	("_get_value", semlock_get_value_entry),
	("_is_zero", semlock_is_zero_entry),
	("_after_fork", semlock_after_fork_entry),
	("_rebuild", semlock_rebuild_entry),
];

fn semlock_method_entry(name: &str) -> Option<BuiltinFn> {
	match name {
		"acquire" => Some(semlock_acquire_entry),
		"release" => Some(semlock_release_entry),
		"__enter__" => Some(semlock_enter_entry),
		"__exit__" => Some(semlock_exit_entry),
		"_count" => Some(semlock_count_entry),
		"_is_mine" => Some(semlock_is_mine_entry),
		"_get_value" => Some(semlock_get_value_entry),
		"_is_zero" => Some(semlock_is_zero_entry),
		"_after_fork" => Some(semlock_after_fork_entry),
		_ => None,
	}
}

fn set_type_str(namespace: *mut type_::PyClassDict, name: &str, value: &str) {
	if let Ok(object) = py_str(value) {
		// SAFETY: The namespace is a live class dict owned by `SemLock`.
		unsafe { (&mut *namespace).set(intern(name), object) };
	}
}

fn set_type_int(namespace: *mut type_::PyClassDict, name: &str, value: i64) {
	let object = py_int(value);
	if !object.is_null() {
		// SAFETY: The namespace is a live class dict owned by `SemLock`.
		unsafe { (&mut *namespace).set(intern(name), object) };
	}
}

fn set_type_function(namespace: *mut type_::PyClassDict, name: &str, entry: BuiltinFn) {
	// SAFETY: `entry` is a live builtin entry with the runtime calling convention.
	let function = unsafe {
		pon_make_function(
			entry as *const u8,
			crate::native::builtins_mod::VARIADIC_ARITY,
			intern(name),
		)
	};
	if !function.is_null() {
		// SAFETY: The namespace is a live class dict owned by `SemLock`.
		unsafe { (&mut *namespace).set(intern(name), function) };
	}
}

unsafe extern "C" fn semlock_new(
	_cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	if let Err(message) = reject_keywords(kwargs, "SemLock()") {
		return raise_type_error(&message);
	}
	let positional = match unsafe { type_::positional_args_from_object(args) } {
		Ok(positional) => positional,
		Err(message) => return raise_type_error(&message),
	};
	if positional.len() != 5 {
		return raise_type_error(&format!(
			"SemLock() takes exactly 5 arguments ({} given)",
			positional.len()
		));
	}

	let Some(kind) = to_i32(positional[0]) else {
		return raise_type_error("SemLock() argument 'kind' must be an integer");
	};
	let Some(value) = to_i32(positional[1]) else {
		return raise_type_error("SemLock() argument 'value' must be an integer");
	};
	let Some(maxvalue) = to_i32(positional[2]) else {
		return raise_type_error("SemLock() argument 'maxvalue' must be an integer");
	};
	let Some(name) = object_str(positional[3]) else {
		return raise_type_error("SemLock() argument 'name' must be str");
	};
	let unlink = match object_bool(positional[4]) {
		Some(unlink) => unlink,
		None => return ptr::null_mut(),
	};

	if kind != RECURSIVE_MUTEX && kind != SEMAPHORE {
		return raise_value_error("unrecognized kind");
	}
	if value < 0 || maxvalue < 1 || value > maxvalue {
		return raise_value_error("semaphore initial value must be between 0 and maxvalue");
	}

	let c_name = match CString::new(name.as_str()) {
		Ok(name) => name,
		Err(_) => return raise_value_error("embedded null character"),
	};

	// SAFETY: `c_name` is a NUL-terminated POSIX semaphore name; mode/value
	// match CPython's SEM_CREATE(name, val, max) on Unix.
	let handle = unsafe {
		libc::sem_open(c_name.as_ptr(), libc::O_CREAT | libc::O_EXCL, 0o600, value as libc::c_uint)
	};
	if is_sem_failed(handle) {
		return raise_errno("sem_open");
	}

	if unlink {
		// SAFETY: Same live C string passed to `sem_open` above.
		if unsafe { libc::sem_unlink(c_name.as_ptr()) } < 0 {
			let error = errno_snapshot("sem_unlink");
			// SAFETY: `handle` was returned by `sem_open` and has not been closed.
			let _ = unsafe { libc::sem_close(handle) };
			return raise_errno_snapshot(error);
		}
	}

	alloc_semlock(handle, kind, maxvalue, (!unlink).then_some(name))
}

unsafe extern "C" fn semlock_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { type_::unicode_text(name) }) else {
		return raise_type_error("SemLock attribute name must be str");
	};
	let Some(lock) = semlock_object(object) else {
		return raise_type_error("SemLock attribute receiver must be SemLock");
	};
	match name_text {
		"handle" => py_uint(lock.handle as usize),
		"kind" => py_int(i64::from(lock.kind)),
		"maxvalue" => py_int(i64::from(lock.maxvalue)),
		"name" => match &lock.name {
			Some(name) => match py_str(name) {
				Ok(value) => value,
				Err(message) => return_error(message),
			},
			None => unsafe { pon_none() },
		},
		"__class__" => semlock_type().cast::<PyObject>(),
		"__doc__" => match py_str("Semaphore/Mutex type") {
			Ok(value) => value,
			Err(message) => return_error(message),
		},
		"__module__" => match py_str("_multiprocessing") {
			Ok(value) => value,
			Err(message) => return_error(message),
		},
		"SEM_VALUE_MAX" => py_int(i64::from(sem_value_max())),
		_ => match semlock_method_entry(name_text) {
			Some(entry) => bound_method(object, name_text, entry),
			None => unsafe { abi::pon_raise_attribute_error(object, intern(name_text)) },
		},
	}
}

unsafe extern "C" fn semlock_acquire_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(lock) = receiver(argv, argc, "acquire") else {
		return ptr::null_mut();
	};
	let mode = match parse_acquire_args(argv, argc) {
		Ok(mode) => mode,
		Err(()) => return ptr::null_mut(),
	};
	match lock.acquire(mode) {
		Ok(acquired) => py_bool(acquired),
		Err(error) => raise_errno_snapshot(error),
	}
}

unsafe extern "C" fn semlock_release_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(lock) = receiver(argv, argc, "release") else {
		return ptr::null_mut();
	};
	if argc != 1 {
		return raise_type_error(&format!(
			"release() takes no arguments ({} given)",
			argc.saturating_sub(1)
		));
	}
	match lock.release() {
		Ok(()) => unsafe { pon_none() },
		Err(SemReleaseError::Exception(kind, message)) => raise(kind, message),
		Err(SemReleaseError::Errno(error)) => raise_errno_snapshot(error),
	}
}

unsafe extern "C" fn semlock_enter_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 1 {
		return raise_type_error(&format!(
			"__enter__() takes no arguments ({} given)",
			argc.saturating_sub(1)
		));
	}
	unsafe { semlock_acquire_entry(argv, argc) }
}

unsafe extern "C" fn semlock_exit_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc == 0 || argv.is_null() {
		return raise_type_error("__exit__ missing receiver");
	}
	if argc > 4 {
		return raise_type_error(&format!(
			"__exit__() takes at most 3 arguments ({} given)",
			argc - 1
		));
	}
	unsafe { semlock_release_entry(argv, 1) }
}

unsafe extern "C" fn semlock_count_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(lock) = receiver(argv, argc, "_count") else {
		return ptr::null_mut();
	};
	if argc != 1 {
		return raise_type_error(&format!(
			"_count() takes no arguments ({} given)",
			argc.saturating_sub(1)
		));
	}
	py_int(i64::from(
		lock
			.state
			.lock()
			.unwrap_or_else(|poison| poison.into_inner())
			.count,
	))
}

unsafe extern "C" fn semlock_is_mine_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(lock) = receiver(argv, argc, "_is_mine") else {
		return ptr::null_mut();
	};
	if argc != 1 {
		return raise_type_error(&format!(
			"_is_mine() takes no arguments ({} given)",
			argc.saturating_sub(1)
		));
	}
	py_bool(lock.is_mine())
}

unsafe extern "C" fn semlock_get_value_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let Some(lock) = receiver(argv, argc, "_get_value") else {
		return ptr::null_mut();
	};
	if argc != 1 {
		return raise_type_error(&format!(
			"_get_value() takes no arguments ({} given)",
			argc.saturating_sub(1)
		));
	}
	#[cfg(target_os = "macos")]
	{
		let _ = lock;
		raise(
			ExceptionKind::NotImplementedError,
			"sem_getvalue() is not implemented on this platform",
		)
	}
	#[cfg(not(target_os = "macos"))]
	{
		match lock.value() {
			Ok(value) => py_int(i64::from(value.max(0))),
			Err(error) => raise_errno_snapshot(error),
		}
	}
}

unsafe extern "C" fn semlock_is_zero_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let Some(lock) = receiver(argv, argc, "_is_zero") else {
		return ptr::null_mut();
	};
	if argc != 1 {
		return raise_type_error(&format!(
			"_is_zero() takes no arguments ({} given)",
			argc.saturating_sub(1)
		));
	}
	match lock.is_zero() {
		Ok(value) => py_bool(value),
		Err(error) => raise_errno_snapshot(error),
	}
}

unsafe extern "C" fn semlock_after_fork_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	let Some(lock) = receiver(argv, argc, "_after_fork") else {
		return ptr::null_mut();
	};
	if argc != 1 {
		return raise_type_error(&format!(
			"_after_fork() takes no arguments ({} given)",
			argc.saturating_sub(1)
		));
	}
	lock
		.state
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.count = 0;
	unsafe { pon_none() }
}

unsafe extern "C" fn semlock_rebuild_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { argv_slice(argv, argc) };
	if args.len() != 4 {
		return raise_type_error(&format!(
			"_rebuild() takes exactly 4 arguments ({} given)",
			args.len()
		));
	}
	let Some(raw_handle) = to_usize(args[0]) else {
		return raise_type_error("_rebuild() argument 'handle' must be an integer");
	};
	let Some(kind) = to_i32(args[1]) else {
		return raise_type_error("_rebuild() argument 'kind' must be an integer");
	};
	let Some(maxvalue) = to_i32(args[2]) else {
		return raise_type_error("_rebuild() argument 'maxvalue' must be an integer");
	};
	let name = if is_none(args[3]) {
		None
	} else if let Some(text) = object_str(args[3]) {
		Some(text)
	} else {
		return raise_type_error("_rebuild() argument 'name' must be str or None");
	};

	if kind != RECURSIVE_MUTEX && kind != SEMAPHORE {
		return raise_value_error("unrecognized kind");
	}

	let handle = if let Some(name_text) = &name {
		let c_name = match CString::new(name_text.as_str()) {
			Ok(name) => name,
			Err(_) => return raise_value_error("embedded null character"),
		};
		// SAFETY: `c_name` is a NUL-terminated POSIX semaphore name.
		let handle = unsafe { libc::sem_open(c_name.as_ptr(), 0) };
		if is_sem_failed(handle) {
			return raise_errno("sem_open");
		}
		handle
	} else {
		raw_handle as SemHandle
	};

	if is_sem_failed(handle) || handle.is_null() {
		return raise_value_error("invalid semaphore handle");
	}
	alloc_semlock(handle, kind, maxvalue, name)
}

unsafe extern "C" fn sem_unlink_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = unsafe { argv_slice(argv, argc) };
	if args.len() != 1 {
		return raise_type_error(&format!(
			"sem_unlink() takes exactly one argument ({} given)",
			args.len()
		));
	}
	let Some(name) = object_str(args[0]) else {
		return raise_type_error("sem_unlink() argument must be str");
	};
	let c_name = match CString::new(name.as_str()) {
		Ok(name) => name,
		Err(_) => return raise_value_error("embedded null character"),
	};
	// SAFETY: `c_name` is a NUL-terminated POSIX semaphore name.
	if unsafe { libc::sem_unlink(c_name.as_ptr()) } < 0 {
		return raise_errno("sem_unlink");
	}
	unsafe { pon_none() }
}

impl PySemLock {
	fn is_mine(&self) -> bool {
		let state = self
			.state
			.lock()
			.unwrap_or_else(|poison| poison.into_inner());
		state.count > 0 && state.last_tid == current_ident()
	}

	fn acquire(&self, mode: AcquireMode) -> Result<bool, ErrnoSnapshot> {
		if self.kind == RECURSIVE_MUTEX {
			let mut state = self
				.state
				.lock()
				.unwrap_or_else(|poison| poison.into_inner());
			if state.count > 0 && state.last_tid == current_ident() {
				state.count += 1;
				return Ok(true);
			}
		}

		let acquired = match mode {
			AcquireMode::NonBlocking => sem_try_wait(self.handle)?,
			AcquireMode::Blocking => {
				sem_wait_blocking(self.handle)?;
				true
			},
			AcquireMode::Timed(seconds) => sem_wait_deadline(self.handle, seconds)?,
		};

		if acquired {
			let mut state = self
				.state
				.lock()
				.unwrap_or_else(|poison| poison.into_inner());
			state.count += 1;
			state.last_tid = current_ident();
		}
		Ok(acquired)
	}

	fn release(&self) -> Result<(), SemReleaseError> {
		if self.kind == RECURSIVE_MUTEX {
			let mut state = self
				.state
				.lock()
				.unwrap_or_else(|poison| poison.into_inner());
			if state.count == 0 || state.last_tid != current_ident() {
				return Err(SemReleaseError::Exception(
					ExceptionKind::AssertionError,
					"attempt to release recursive lock not owned by thread",
				));
			}
			if state.count > 1 {
				state.count -= 1;
				return Ok(());
			}
		} else if self.maxvalue == 1 {
			if !self.is_zero().map_err(SemReleaseError::Errno)? {
				return Err(SemReleaseError::Exception(
					ExceptionKind::ValueError,
					"semaphore or lock released too many times",
				));
			}
		} else {
			#[cfg(not(target_os = "macos"))]
			{
				let value = self.value().map_err(SemReleaseError::Errno)?;
				if value >= self.maxvalue {
					return Err(SemReleaseError::Exception(
						ExceptionKind::ValueError,
						"semaphore or lock released too many times",
					));
				}
			}
		}

		// SAFETY: `handle` is a live POSIX semaphore handle.
		if unsafe { libc::sem_post(self.handle) } < 0 {
			return Err(SemReleaseError::Errno(errno_snapshot("sem_post")));
		}

		let mut state = self
			.state
			.lock()
			.unwrap_or_else(|poison| poison.into_inner());
		state.count -= 1;
		if state.count == 0 {
			state.last_tid = 0;
		}
		Ok(())
	}

	#[cfg(not(target_os = "macos"))]
	fn value(&self) -> Result<i32, ErrnoSnapshot> {
		let mut value = 0;
		// SAFETY: `handle` is a live POSIX semaphore handle; `value` points to
		// valid writable memory for the call.
		if unsafe { libc::sem_getvalue(self.handle, &mut value) } < 0 {
			return Err(errno_snapshot("sem_getvalue"));
		}
		Ok(value)
	}

	#[cfg(target_os = "macos")]
	fn is_zero(&self) -> Result<bool, ErrnoSnapshot> {
		match sem_try_wait(self.handle)? {
			true => {
				// SAFETY: Undo the successful probe on the same live semaphore.
				if unsafe { libc::sem_post(self.handle) } < 0 {
					return Err(errno_snapshot("sem_post"));
				}
				Ok(false)
			},
			false => Ok(true),
		}
	}

	#[cfg(not(target_os = "macos"))]
	fn is_zero(&self) -> Result<bool, ErrnoSnapshot> {
		Ok(self.value()? == 0)
	}
}

enum AcquireMode {
	NonBlocking,
	Blocking,
	Timed(f64),
}

enum SemReleaseError {
	Exception(ExceptionKind, &'static str),
	Errno(ErrnoSnapshot),
}

#[derive(Clone)]
struct ErrnoSnapshot {
	function: &'static str,
	code:     i32,
	message:  String,
}

fn alloc_semlock(
	handle: SemHandle,
	kind: i32,
	maxvalue: i32,
	name: Option<String>,
) -> *mut PyObject {
	Box::into_raw(Box::new(PySemLock {
		_ob_base: PyObjectHeader::new(semlock_type().cast_const()),
		handle,
		kind,
		maxvalue,
		name,
		state: Mutex::new(SemLockState { last_tid: 0, count: 0 }),
	}))
	.cast::<PyObject>()
}

fn semlock_object<'a>(object: *mut PyObject) -> Option<&'a PySemLock> {
	if object.is_null() || unsafe { (*object).ob_type } != semlock_type().cast_const() {
		return None;
	}
	Some(unsafe { &*object.cast::<PySemLock>() })
}

fn receiver<'a>(argv: *mut *mut PyObject, argc: usize, method: &str) -> Option<&'a PySemLock> {
	if argc == 0 || argv.is_null() {
		let _ = raise_type_error(&format!("{method}() missing receiver"));
		return None;
	}
	let receiver = unsafe { *argv };
	match semlock_object(receiver) {
		Some(lock) => Some(lock),
		None => {
			let _ = raise_type_error(&format!("{method}() receiver is not a SemLock"));
			None
		},
	}
}

fn parse_acquire_args(argv: *mut *mut PyObject, argc: usize) -> Result<AcquireMode, ()> {
	if argc == 0 || argv.is_null() {
		let _ = raise_type_error("acquire() missing receiver");
		return Err(());
	}
	if argc > 3 {
		let _ =
			raise_type_error(&format!("acquire() takes at most 2 arguments ({} given)", argc - 1));
		return Err(());
	}

	let mut blocking = true;
	if argc >= 2 {
		let object = unsafe { *argv.add(1) };
		match object_bool(object) {
			Some(value) => blocking = value,
			None => return Err(()),
		}
	}

	let mut timeout = None;
	if argc == 3 {
		let object = crate::tag::untag_arg(unsafe { *argv.add(2) });
		if !is_none(object) {
			let Some(seconds) = to_f64(object) else {
				let _ = raise_type_error("timeout must be a number or None");
				return Err(());
			};
			if !seconds.is_finite() {
				let _ = raise_overflow_error("timeout is too large");
				return Err(());
			}
			timeout = Some(seconds.max(0.0));
		}
	}

	match (blocking, timeout) {
		(false, Some(_)) => {
			let _ = raise_value_error("can't specify timeout for non-blocking acquire");
			Err(())
		},
		(false, None) => Ok(AcquireMode::NonBlocking),
		(true, None) => Ok(AcquireMode::Blocking),
		(true, Some(seconds)) => Ok(AcquireMode::Timed(seconds)),
	}
}

fn reject_keywords(kwargs: *mut PyObject, owner: &str) -> Result<(), String> {
	if kwargs.is_null() {
		return Ok(());
	}
	// SAFETY: `call_type_with_keywords` materializes keyword args as a dict.
	let entries = unsafe { dict::dict_entries_snapshot(kwargs)? };
	if entries.is_empty() {
		Ok(())
	} else {
		Err(format!("{owner} takes no keyword arguments"))
	}
}

unsafe fn argv_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> &'a [*mut PyObject] {
	if argv.is_null() || argc == 0 {
		&[]
	} else {
		unsafe { core::slice::from_raw_parts(argv, argc) }
	}
}

fn to_i32(object: *mut PyObject) -> Option<i32> {
	let value = crate::tag::untag_arg(object);
	unsafe { int::to_bigint_including_bool(value) }.and_then(|value| value.to_i32())
}

fn to_usize(object: *mut PyObject) -> Option<usize> {
	let value = crate::tag::untag_arg(object);
	unsafe { int::to_bigint_including_bool(value) }.and_then(|value| value.to_usize())
}

fn to_f64(object: *mut PyObject) -> Option<f64> {
	let value = crate::tag::untag_arg(object);
	if let Some(value) = unsafe { float::to_f64(value) } {
		Some(value)
	} else {
		unsafe { int::to_bigint_including_bool(value) }.and_then(|value| value.to_f64())
	}
}

fn object_str(object: *mut PyObject) -> Option<String> {
	let object = crate::tag::untag_arg(object);
	unsafe { type_::unicode_text(object) }.map(str::to_owned)
}

fn object_bool(object: *mut PyObject) -> Option<bool> {
	// SAFETY: `pon_is_true` self-normalizes its argument and sets an exception
	// on failure.
	match unsafe { pon_is_true(object) } {
		-1 => None,
		0 => Some(false),
		_ => Some(true),
	}
}

fn is_none(object: *mut PyObject) -> bool {
	crate::tag::untag_arg(object) == unsafe { pon_none() }
}

fn py_uint(value: usize) -> *mut PyObject {
	match i64::try_from(value) {
		Ok(value) => py_int(value),
		Err(_) => raise_overflow_error("semaphore handle is too large to convert to int"),
	}
}

fn current_ident() -> u64 {
	thread_local! {
		 static IDENT_ANCHOR: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
	}
	IDENT_ANCHOR.with(|anchor| anchor.as_ptr() as usize as u64)
}

fn sem_wait_blocking(handle: SemHandle) -> Result<(), ErrnoSnapshot> {
	loop {
		// SAFETY: `handle` is a live POSIX semaphore handle.
		if unsafe { libc::sem_wait(handle) } == 0 {
			return Ok(());
		}
		let error = errno_snapshot("sem_wait");
		if error.code != libc::EINTR {
			return Err(error);
		}
	}
}

fn sem_wait_deadline(handle: SemHandle, seconds: f64) -> Result<bool, ErrnoSnapshot> {
	let deadline = Instant::now() + Duration::from_secs_f64(seconds.max(0.0));
	loop {
		if sem_try_wait(handle)? {
			return Ok(true);
		}
		if Instant::now() >= deadline {
			return Ok(false);
		}
		std::thread::sleep(POLL_INTERVAL);
	}
}

fn sem_try_wait(handle: SemHandle) -> Result<bool, ErrnoSnapshot> {
	loop {
		// SAFETY: `handle` is a live POSIX semaphore handle.
		if unsafe { libc::sem_trywait(handle) } == 0 {
			return Ok(true);
		}
		let error = errno_snapshot("sem_trywait");
		match error.code {
			libc::EAGAIN => return Ok(false),
			libc::EINTR => continue,
			_ => return Err(error),
		}
	}
}

fn sem_value_max() -> i32 {
	// SAFETY: `sysconf(_SC_SEM_VALUE_MAX)` has no side effects beyond reading
	// process configuration.
	let value = unsafe { libc::sysconf(libc::_SC_SEM_VALUE_MAX) };
	if value > 0 {
		i32::try_from(value).unwrap_or(i32::MAX)
	} else {
		i32::MAX
	}
}

fn is_sem_failed(handle: SemHandle) -> bool {
	handle == libc::SEM_FAILED
}

fn errno_snapshot(function: &'static str) -> ErrnoSnapshot {
	let error = std::io::Error::last_os_error();
	let code = error.raw_os_error().unwrap_or(0);
	ErrnoSnapshot { function, code, message: error.to_string() }
}

fn raise_errno(function: &'static str) -> *mut PyObject {
	raise_errno_snapshot(errno_snapshot(function))
}

fn raise_errno_snapshot(error: ErrnoSnapshot) -> *mut PyObject {
	let kind = match error.code {
		libc::EEXIST => ExceptionKind::FileExistsError,
		libc::ENOENT => ExceptionKind::FileNotFoundError,
		libc::EACCES | libc::EPERM => ExceptionKind::PermissionError,
		libc::EINTR => ExceptionKind::InterruptedError,
		_ => ExceptionKind::OSError,
	};
	raise(kind, &format!("{} failed: {}", error.function, error.message))
}

fn raise(kind: ExceptionKind, message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(kind, message)
}

fn raise_type_error(message: &str) -> *mut PyObject {
	raise(ExceptionKind::TypeError, message)
}

fn raise_value_error(message: &str) -> *mut PyObject {
	raise(ExceptionKind::ValueError, message)
}

fn raise_overflow_error(message: &str) -> *mut PyObject {
	raise(ExceptionKind::OverflowError, message)
}

fn return_error(message: String) -> *mut PyObject {
	pon_err_set(message);
	ptr::null_mut()
}
