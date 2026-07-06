//! Native `_signal` module (unittest chain: `unittest.signals` -> `signal`).
//!
//! The module owns process signal disposition for Python-visible handlers:
//! `signal.signal()` installs a small async-signal-safe `sigaction` trampoline,
//! records the signal in an atomic pending mask, and optionally writes the
//! signal byte to `set_wakeup_fd()` for selector/event-loop wakeups.  Python
//! handlers never run from the OS signal frame; they are drained on the main
//! runtime thread by [`process_pending_signals`] (called by signal-owned waits
//! here and by the WS3 safepoint/blocking-region substrate).
//!
//! Remaining product boundary: frame objects are not yet materialized for the
//! handler's second argument, so handlers receive `None` for `frame`.

use std::sync::{
	Mutex,
	atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering},
};

use super::install_module;
use crate::{
	abi::{
		exc::raise_kind_error_no_args, pon_call, pon_const_int, pon_make_function,
		return_null_with_error,
	},
	intern::intern,
	object::{PyObject, PyType},
	types::exc::{ExceptionKind, PyBaseException},
};

/// `SIG_DFL` sentinel value (CPython exposes it as the int 0).
const SIG_DFL: i64 = 0;
/// `SIG_IGN` sentinel value (CPython exposes it as the int 1).
const SIG_IGN: i64 = 1;

/// Highest signal number + 1, matching the host CPython's `signal.NSIG`.
#[cfg(target_os = "macos")]
const NSIG: i64 = 32;
#[cfg(not(target_os = "macos"))]
const NSIG: i64 = 65;

/// `SIG*` numbers shared by every supported host, host-correct via `libc`
/// (the `errno` precedent).  `signal.py` turns these module attrs into the
/// `Signals` IntEnum by name filtering, so plain int attrs are the contract.
const CONSTANTS: &[(&str, i32)] = &[
	("SIGHUP", libc::SIGHUP),
	("SIGINT", libc::SIGINT),
	("SIGQUIT", libc::SIGQUIT),
	("SIGILL", libc::SIGILL),
	("SIGTRAP", libc::SIGTRAP),
	("SIGABRT", libc::SIGABRT),
	("SIGBUS", libc::SIGBUS),
	("SIGFPE", libc::SIGFPE),
	("SIGKILL", libc::SIGKILL),
	("SIGUSR1", libc::SIGUSR1),
	("SIGSEGV", libc::SIGSEGV),
	("SIGUSR2", libc::SIGUSR2),
	("SIGPIPE", libc::SIGPIPE),
	("SIGALRM", libc::SIGALRM),
	("SIGTERM", libc::SIGTERM),
	("SIGCHLD", libc::SIGCHLD),
	("SIGCONT", libc::SIGCONT),
	("SIGSTOP", libc::SIGSTOP),
	("SIGTSTP", libc::SIGTSTP),
	("SIGTTIN", libc::SIGTTIN),
	("SIGTTOU", libc::SIGTTOU),
	("SIGURG", libc::SIGURG),
	("SIGXCPU", libc::SIGXCPU),
	("SIGXFSZ", libc::SIGXFSZ),
	("SIGVTALRM", libc::SIGVTALRM),
	("SIGPROF", libc::SIGPROF),
	("SIGWINCH", libc::SIGWINCH),
	("SIGIO", libc::SIGIO),
	("SIGSYS", libc::SIGSYS),
];

/// Host-specific extras CPython also exposes.
#[cfg(target_os = "macos")]
const OS_CONSTANTS: &[(&str, i32)] = &[("SIGEMT", libc::SIGEMT), ("SIGINFO", libc::SIGINFO)];
#[cfg(target_os = "linux")]
const OS_CONSTANTS: &[(&str, i32)] =
	&[("SIGSTKFLT", libc::SIGSTKFLT), ("SIGPWR", libc::SIGPWR), ("SIGPOLL", libc::SIGPOLL)];
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const OS_CONSTANTS: &[(&str, i32)] = &[];

const POSIX_CONSTANTS: &[(&str, i64)] = &[
	("ITIMER_PROF", 2),
	("ITIMER_REAL", 0),
	("ITIMER_VIRTUAL", 1),
	("SIGIOT", libc::SIGABRT as i64),
	("SIG_BLOCK", 1),
	("SIG_SETMASK", 3),
	("SIG_UNBLOCK", 2),
];

static WAKEUP_FD: AtomicI32 = AtomicI32::new(-1);

/// Fast-path flag checked by safepoints and blocking-region exits.  The no-work
/// path is exactly one relaxed atomic load; the heavier pending mask is touched
/// only after this flag is observed set.
static PENDING_SIGNAL_FLAG: AtomicBool = AtomicBool::new(false);
static PENDING_SIGNALS: [AtomicU64; PENDING_WORDS] =
	[AtomicU64::new(0), AtomicU64::new(0)];
const PENDING_WORDS: usize = 2;

/// OS thread selected to run Python signal handlers.  CPython uses the main
/// interpreter thread; pon records the thread that initializes the runtime.
static MAIN_THREAD_ID: AtomicUsize = AtomicUsize::new(0);

/// Startup installs pon's SIGINT trampoline before `_signal` itself is imported.
/// Until the module table contains `default_int_handler`, this flag tells the
/// pending-signal drain to raise `KeyboardInterrupt` for SIGINT directly.
static DEFAULT_SIGINT_ACTIVE: AtomicBool = AtomicBool::new(false);

static ITIMER_ERROR_TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
	let base = crate::import::module_attr(intern("builtins"), intern("OSError"))
		.map_or(std::ptr::null_mut(), |object| object.cast::<PyType>());
	let mut ty = PyType::new(
		crate::abi::runtime_type_type().cast_const(),
		"signal.ItimerError",
		std::mem::size_of::<PyBaseException>(),
	);
	ty.tp_base = base;
	ty.tp_getattro = Some(crate::types::exc::exception_getattro);
	ty.tp_setattro = Some(crate::types::exc::exception_setattro);
	Box::into_raw(Box::new(ty)) as usize
});

fn itimer_error_type() -> *mut PyType {
	*ITIMER_ERROR_TYPE as *mut PyType
}

/// One handler-table slot.  Python handler objects are stored as raw
/// addresses; [`gc_held_roots`] reports them so the collector keeps them
/// alive (the `_contextvars` pattern for native statics).
#[derive(Clone, Copy)]
enum StoredHandler {
	Dfl,
	Ign,
	Object(usize),
}

impl StoredHandler {
	/// Boxes the slot back into the Python value `signal()`/`getsignal()`
	/// return: the sentinel ints or the registered object itself.
	fn to_object(self) -> *mut PyObject {
		match self {
			// SAFETY: Integer boxing helper; NULL propagates to the caller's check.
			Self::Dfl => unsafe { pon_const_int(SIG_DFL) },
			// SAFETY: As above.
			Self::Ign => unsafe { pon_const_int(SIG_IGN) },
			Self::Object(address) => address as *mut PyObject,
		}
	}
}

/// Process-level handler table indexed by signal number (`0..NSIG`), lazily
/// sized on first touch.  Slot 0 is unused (signal numbers start at 1).
static HANDLERS: Mutex<Vec<StoredHandler>> = Mutex::new(Vec::new());

fn handlers_lock() -> std::sync::MutexGuard<'static, Vec<StoredHandler>> {
	let mut table = HANDLERS.lock().unwrap_or_else(|poison| poison.into_inner());
	if table.is_empty() {
		table.resize(NSIG as usize, StoredHandler::Dfl);
	}
	table
}

/// Python handler objects held by the native table, reported as GC roots
/// (mirrors `_contextvars`/`_codecs`/`_collections`).  Locks only this
/// module's mutex and never re-enters the runtime.
pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
	HANDLERS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.iter()
		.filter_map(|slot| match slot {
			StoredHandler::Object(address) => Some(*address as *mut PyObject),
			_ => None,
		})
		.collect()
}

/// Installs process-start signal state that must exist even before Python code
/// imports `signal`: the main-thread identity and pon's default SIGINT handler.
pub(crate) fn init_main_thread_signal_handlers() -> Result<(), String> {
	note_main_thread();
	install_signal_trampoline(libc::SIGINT as usize).map_err(|errno| {
		format!("failed to install SIGINT handler: {}", errno_message(errno))
	})?;
	DEFAULT_SIGINT_ACTIVE.store(true, Ordering::Release);
	Ok(())
}

fn note_main_thread() {
	let current = pthread_self_word();
	let _ = MAIN_THREAD_ID.compare_exchange(0, current, Ordering::AcqRel, Ordering::Acquire);
}

fn pthread_self_word() -> usize {
	unsafe { libc::pthread_self() as usize }
}

fn running_on_main_thread() -> bool {
	let main = MAIN_THREAD_ID.load(Ordering::Relaxed);
	main != 0 && pthread_self_word() == main
}

/// Returns whether any OS signal has been recorded by the trampoline.  This is
/// the hot-path predicate for safepoints and blocking-region exits.
#[must_use]
pub(crate) fn has_pending_signals() -> bool {
	PENDING_SIGNAL_FLAG.load(Ordering::Relaxed)
}

/// C-shaped helper for generated-code poll bodies.  Returns `0` when no Python
/// exception is pending and `-1` when draining a signal raised one.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_signal_check_pending() -> libc::c_int {
	match unsafe { process_pending_signals() } {
		Ok(()) => 0,
		Err(_) => -1,
	}
}

/// Runs pending Python signal handlers on the main runtime thread.
///
/// Non-main threads leave the pending flag set so the next main-thread
/// interruption point will perform the drain.  On a raising handler, unprocessed
/// signals are restored to the atomic mask.
pub(crate) unsafe fn process_pending_signals() -> Result<(), *mut PyObject> {
	if !has_pending_signals() {
		return Ok(());
	}
	if !running_on_main_thread() {
		return Ok(());
	}

	let mut words = take_pending_words();
	for signalnum in 1..NSIG as usize {
		let Some((word, bit)) = signal_bit(signalnum) else {
			continue;
		};
		if words[word] & bit == 0 {
			continue;
		}
		words[word] &= !bit;
		if let Err(error) = unsafe { run_python_signal_handler(signalnum) } {
			restore_pending_words(words);
			return Err(error);
		}
	}
	Ok(())
}

fn take_pending_words() -> [u64; PENDING_WORDS] {
	PENDING_SIGNAL_FLAG.store(false, Ordering::Relaxed);
	let mut words = [0; PENDING_WORDS];
	for (index, word) in PENDING_SIGNALS.iter().enumerate() {
		words[index] = word.swap(0, Ordering::AcqRel);
	}
	words
}

fn restore_pending_words(words: [u64; PENDING_WORDS]) {
	let mut restored = false;
	for (index, bits) in words.into_iter().enumerate() {
		if bits != 0 {
			PENDING_SIGNALS[index].fetch_or(bits, Ordering::AcqRel);
			restored = true;
		}
	}
	if restored {
		PENDING_SIGNAL_FLAG.store(true, Ordering::Release);
	}
}

unsafe fn run_python_signal_handler(signalnum: usize) -> Result<(), *mut PyObject> {
	let slot = handlers_lock()[signalnum];
	match slot {
		StoredHandler::Object(handler) => {
			let signal_object = unsafe { pon_const_int(signalnum as i64) };
			if signal_object.is_null() {
				return Err(std::ptr::null_mut());
			}
			let frame = unsafe { crate::abi::pon_none() };
			if frame.is_null() {
				return Err(std::ptr::null_mut());
			}
			let mut args = [signal_object, frame];
			let result =
				unsafe { pon_call(handler as *mut PyObject, args.as_mut_ptr(), args.len()) };
			if result.is_null() {
				Err(std::ptr::null_mut())
			} else {
				Ok(())
			}
		},
		StoredHandler::Dfl
			if signalnum == libc::SIGINT as usize
				&& DEFAULT_SIGINT_ACTIVE.load(Ordering::Relaxed) =>
		{
			Err(raise_kind_error_no_args(ExceptionKind::KeyboardInterrupt))
		},
		StoredHandler::Dfl | StoredHandler::Ign => Ok(()),
	}
}

extern "C" fn signal_trampoline(signalnum: libc::c_int) {
	mark_signal_pending(signalnum);
}

fn mark_signal_pending(signalnum: libc::c_int) {
	let Ok(signalnum) = usize::try_from(signalnum) else {
		return;
	};
	if signalnum == 0 || signalnum >= NSIG as usize {
		return;
	}
	let Some((word, bit)) = signal_bit(signalnum) else {
		return;
	};
	PENDING_SIGNALS[word].fetch_or(bit, Ordering::Relaxed);
	PENDING_SIGNAL_FLAG.store(true, Ordering::Relaxed);

	let fd = WAKEUP_FD.load(Ordering::Relaxed);
	if fd >= 0 {
		let byte = [signalnum as u8];
		unsafe {
			let _ = libc::write(fd, byte.as_ptr().cast::<libc::c_void>(), byte.len());
		}
	}
}

fn signal_bit(signalnum: usize) -> Option<(usize, u64)> {
	let word = signalnum / u64::BITS as usize;
	let shift = signalnum % u64::BITS as usize;
	(word < PENDING_WORDS).then_some((word, 1_u64 << shift))
}

fn install_signal_disposition(signalnum: usize, handler: StoredHandler) -> Result<(), i32> {
	match handler {
		StoredHandler::Dfl => install_sigaction(signalnum, libc::SIG_DFL, 0),
		StoredHandler::Ign => install_sigaction(signalnum, libc::SIG_IGN, 0),
		StoredHandler::Object(_) => install_signal_trampoline(signalnum),
	}
}

fn install_signal_trampoline(signalnum: usize) -> Result<(), i32> {
	install_sigaction(signalnum, signal_trampoline as *const () as libc::sighandler_t, 0)
}

fn install_sigaction(
	signalnum: usize,
	handler: libc::sighandler_t,
	flags: libc::c_int,
) -> Result<(), i32> {
	let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
	action.sa_sigaction = handler;
	action.sa_flags = flags;
	unsafe { libc::sigemptyset(&mut action.sa_mask) };
	if unsafe { libc::sigaction(signalnum as libc::c_int, &action, std::ptr::null_mut()) } != 0 {
		Err(last_errno())
	} else {
		Ok(())
	}
}

fn errno_message(errno: i32) -> String {
	unsafe { std::ffi::CStr::from_ptr(libc::strerror(errno)) }
		.to_string_lossy()
		.into_owned()
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	init_main_thread_signal_handlers()?;
	let name = "_signal";
	// SAFETY: Runtime allocation helper; NULL is checked below.
	let name_obj = unsafe { crate::abi::pon_const_str(name.as_ptr(), name.len()) };
	if name_obj.is_null() {
		return Err("failed to allocate _signal.__name__".to_owned());
	}
	let mut attrs = vec![(intern("__name__"), name_obj)];
	for &(const_name, value) in CONSTANTS.iter().chain(OS_CONSTANTS) {
		attrs.push(int_attr(const_name, i64::from(value))?);
	}
	for &(const_name, value) in POSIX_CONSTANTS {
		attrs.push(int_attr(const_name, value)?);
	}
	attrs.push(int_attr("SIG_DFL", SIG_DFL)?);
	attrs.push(int_attr("SIG_IGN", SIG_IGN)?);
	attrs.push(int_attr("NSIG", NSIG)?);
	attrs.push((intern("ItimerError"), itimer_error_type().cast::<PyObject>()));
	#[cfg(target_os = "linux")]
	attrs.push((intern("struct_siginfo"), siginfo_result_type().cast::<PyObject>()));
	let default_int_handler = function_attr("default_int_handler", signal_default_int_handler)?;
	// CPython's module init registers `default_int_handler` for SIGINT; the
	// table mirrors that so `getsignal(SIGINT)` observes it at startup.
	handlers_lock()[libc::SIGINT as usize] = StoredHandler::Object(default_int_handler.1 as usize);
	attrs.push(default_int_handler);
	attrs.push(function_attr("signal", signal_signal)?);
	attrs.push(function_attr("getsignal", signal_getsignal)?);
	attrs.push(function_attr("alarm", signal_alarm)?);
	attrs.push(function_attr("getitimer", signal_getitimer)?);
	attrs.push(function_attr("setitimer", signal_setitimer)?);
	attrs.push(function_attr("pause", signal_pause)?);
	attrs.push(function_attr("pthread_kill", signal_pthread_kill)?);
	attrs.push(function_attr("pthread_sigmask", signal_pthread_sigmask)?);
	attrs.push(function_attr("raise_signal", signal_raise_signal)?);
	attrs.push(function_attr("set_wakeup_fd", signal_set_wakeup_fd)?);
	attrs.push(function_attr("siginterrupt", signal_siginterrupt)?);
	attrs.push(function_attr("sigpending", signal_sigpending)?);
	attrs.push(function_attr("sigwait", signal_sigwait)?);
	#[cfg(target_os = "linux")]
	attrs.push(function_attr("sigwaitinfo", signal_sigwaitinfo)?);
	attrs.push(function_attr("strsignal", signal_strsignal)?);
	attrs.push(function_attr("valid_signals", signal_valid_signals)?);
	install_module(name, attrs)
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
	// SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
	let object = unsafe { pon_const_int(value) };
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate _signal.{name}"))
}

fn function_attr(
	name: &str,
	entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Result<(u32, *mut PyObject), String> {
	// SAFETY: Live builtin entry point with the runtime calling convention.
	let object = unsafe {
		pon_make_function(entry as *const u8, crate::builtins::variadic_arity(), intern(name))
	};
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate _signal.{name}"))
}

/// Reads an `int`-coercible argument (tagged immediate, boxed int, bool, or
/// int subclass such as an IntEnum member) as an `i64`.
unsafe fn arg_as_i64(object: *mut PyObject) -> Option<i64> {
	if crate::tag::is_small_int(object) {
		return Some(crate::tag::untag_small_int(object));
	}
	unsafe { crate::types::int::to_bigint_including_bool(object) }
		.and_then(|value| num_traits::ToPrimitive::to_i64(&value))
}

/// Best-effort receiver type name for CPython-shaped TypeError messages.
unsafe fn display_type_name(object: *mut PyObject) -> &'static str {
	if object.is_null() {
		return "NoneType";
	}
	if crate::tag::is_small_int(object) {
		return "int";
	}
	// SAFETY: Heap-or-NULL after the immediate test; NULL handled above.
	let ty = unsafe { (*object).ob_type };
	if ty.is_null() {
		return "object";
	}
	// SAFETY: Live type pointer per object-header invariant.
	unsafe { (*ty).name() }
}

/// Validates `signalnum` per CPython: an int in `1..NSIG`.
unsafe fn parse_signalnum(object: *mut PyObject, function: &str) -> Result<usize, *mut PyObject> {
	let Some(signalnum) = (unsafe { arg_as_i64(object) }) else {
		let message =
			format!("{function}: '{}' object cannot be interpreted as an integer", unsafe {
				display_type_name(object)
			});
		return Err(return_null_with_error(message));
	};
	if signalnum < 1 || signalnum >= NSIG {
		let message = "signal number out of range";
		// SAFETY: Typed raise helper with a static message.
		return Err(unsafe {
			crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len())
		});
	}
	Ok(signalnum as usize)
}

/// `_signal.signal(signalnum, handler)`: validate, swap the table entry, and
/// return the prior handler.
unsafe extern "C" fn signal_signal(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 2 || argv.is_null() {
		return return_null_with_error(format!("signal() takes exactly 2 arguments ({argc} given)"));
	}
	// SAFETY: `argv` carries `argc` argument slots per the call ABI.
	let (signalnum, handler) = unsafe { (*argv, *argv.add(1)) };
	let signalnum = match unsafe { parse_signalnum(signalnum, "signal") } {
		Ok(signalnum) => signalnum,
		Err(raised) => return raised,
	};
	let new_slot = match unsafe { arg_as_i64(handler) } {
		Some(SIG_DFL) => StoredHandler::Dfl,
		Some(SIG_IGN) => StoredHandler::Ign,
		Some(_) => {
			let message =
				"signal handler must be signal.SIG_IGN, signal.SIG_DFL, or a callable object";
			// SAFETY: Typed raise helper with a static message.
			return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
		},
		None => {
			if handler.is_null() || unsafe { crate::types::int::type_name_is(handler, "NoneType") } {
				let message =
					"signal handler must be signal.SIG_IGN, signal.SIG_DFL, or a callable object";
				// SAFETY: Typed raise helper with a static message.
				return unsafe {
					crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len())
				};
			}
			StoredHandler::Object(handler as usize)
		},
	};
	if let Err(errno) = install_signal_disposition(signalnum, new_slot) {
		return raise_errno_text(errno);
	}
	if signalnum == libc::SIGINT as usize {
		DEFAULT_SIGINT_ACTIVE.store(false, Ordering::Release);
	}
	let previous = {
		let mut table = handlers_lock();
		core::mem::replace(&mut table[signalnum], new_slot)
	};
	previous.to_object()
}

/// `_signal.getsignal(signalnum)`: read the table entry.
unsafe extern "C" fn signal_getsignal(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 1 || argv.is_null() {
		return return_null_with_error(format!(
			"getsignal() takes exactly 1 argument ({argc} given)"
		));
	}
	// SAFETY: One live argument slot per the check above.
	let signalnum = match unsafe { parse_signalnum(*argv, "getsignal") } {
		Ok(signalnum) => signalnum,
		Err(raised) => return raised,
	};
	handlers_lock()[signalnum].to_object()
}

/// `_signal.default_int_handler(*args)`: raises `KeyboardInterrupt` when
/// called (CPython ignores the `(signalnum, frame)` arguments too).
unsafe extern "C" fn signal_default_int_handler(
	_argv: *mut *mut PyObject,
	_argc: usize,
) -> *mut PyObject {
	raise_kind_error_no_args(ExceptionKind::KeyboardInterrupt)
}

unsafe extern "C" fn signal_alarm(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 1 || argv.is_null() {
		return return_null_with_error(format!("alarm() takes exactly 1 argument ({argc} given)"));
	}
	let seconds = match unsigned_int_arg(unsafe { *argv }, "seconds") {
		Ok(seconds) => seconds,
		Err(error) => return error,
	};
	let previous = unsafe { libc::alarm(seconds) };
	unsafe { pon_const_int(i64::from(previous)) }
}

unsafe extern "C" fn signal_getitimer(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 1 || argv.is_null() {
		return return_null_with_error(format!(
			"getitimer() takes exactly 1 argument ({argc} given)"
		));
	}
	let which = match c_int_arg(unsafe { *argv }, "which") {
		Ok(which) => which,
		Err(error) => return error,
	};
	let mut current = std::mem::MaybeUninit::<libc::itimerval>::uninit();
	if unsafe { libc::getitimer(which, current.as_mut_ptr()) } != 0 {
		return raise_itimer_errno();
	}
	itimer_tuple(unsafe { current.assume_init() })
}

unsafe extern "C" fn signal_setitimer(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if (2..=3).contains(&args.len()) => args,
		_ => {
			return return_null_with_error(format!(
				"setitimer() takes 2 or 3 arguments ({argc} given)"
			));
		},
	};
	let which = match c_int_arg(args[0], "which") {
		Ok(which) => which,
		Err(error) => return error,
	};
	let seconds = match float_arg(args[1], "seconds") {
		Ok(seconds) => seconds,
		Err(error) => return error,
	};
	let interval = match args.get(2).copied() {
		Some(object) => match float_arg(object, "interval") {
			Ok(interval) => interval,
			Err(error) => return error,
		},
		None => 0.0,
	};
	let new_value = libc::itimerval {
		it_interval: seconds_to_timeval(interval),
		it_value:    seconds_to_timeval(seconds),
	};
	let mut old_value = std::mem::MaybeUninit::<libc::itimerval>::uninit();
	if unsafe { libc::setitimer(which, &new_value, old_value.as_mut_ptr()) } != 0 {
		return raise_itimer_errno();
	}
	itimer_tuple(unsafe { old_value.assume_init() })
}

unsafe extern "C" fn signal_pause(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return return_null_with_error(format!("pause() takes no arguments ({argc} given)"));
	}
	loop {
		crate::sync::enter_blocking_region();
		let result = unsafe { libc::pause() };
		let leave_result = crate::sync::leave_blocking_region();
		if crate::thread_state::pon_err_occurred() {
			return std::ptr::null_mut();
		}
		if let Err(message) = leave_result {
			return return_null_with_error(message);
		}
		if result == -1 {
			let errno = last_errno();
			if errno == libc::EINTR {
				return match unsafe { process_pending_signals() } {
					Ok(()) => unsafe { crate::abi::pon_none() },
					Err(error) => error,
				};
			}
			return raise_errno_text(errno);
		}
	}
}

unsafe extern "C" fn signal_pthread_kill(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 2 || argv.is_null() {
		return return_null_with_error(format!(
			"pthread_kill() takes exactly 2 arguments ({argc} given)"
		));
	}
	let args = unsafe { std::slice::from_raw_parts(argv, argc) };
	let thread = match thread_id_arg(args[0]) {
		Ok(thread) => thread,
		Err(error) => return error,
	};
	let signalnum = match unsafe { parse_signalnum(args[1], "pthread_kill") } {
		Ok(signalnum) => signalnum as libc::c_int,
		Err(error) => return error,
	};
	let errno = unsafe { libc::pthread_kill(thread, signalnum) };
	if errno != 0 {
		return raise_errno_text(errno);
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn signal_pthread_sigmask(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	if argc != 2 || argv.is_null() {
		return return_null_with_error(format!(
			"pthread_sigmask() takes exactly 2 arguments ({argc} given)"
		));
	}
	let args = unsafe { std::slice::from_raw_parts(argv, argc) };
	let how = match c_int_arg(args[0], "how") {
		Ok(how) => how,
		Err(error) => return error,
	};
	let set = match signal_set_arg(args[1], "pthread_sigmask") {
		Ok(set) => set,
		Err(error) => return error,
	};
	let mut old = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
	let set_ptr = set
		.as_ref()
		.map_or(std::ptr::null(), |value| value as *const libc::sigset_t);
	let errno = unsafe { libc::pthread_sigmask(how, set_ptr, old.as_mut_ptr()) };
	if errno != 0 {
		return raise_errno_text(errno);
	}
	signal_set_to_object(unsafe { old.assume_init() })
}

unsafe extern "C" fn signal_raise_signal(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 1 || argv.is_null() {
		return return_null_with_error(format!(
			"raise_signal() takes exactly 1 argument ({argc} given)"
		));
	}
	let signalnum = match unsafe { parse_signalnum(*argv, "raise_signal") } {
		Ok(signalnum) => signalnum as libc::c_int,
		Err(error) => return error,
	};
	if unsafe { libc::raise(signalnum) } != 0 {
		return raise_errno_text(last_errno());
	}
	match unsafe { process_pending_signals() } {
		Ok(()) => unsafe { crate::abi::pon_none() },
		Err(error) => error,
	}
}

unsafe extern "C" fn signal_set_wakeup_fd(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if (1..=2).contains(&args.len()) => args,
		_ => {
			return return_null_with_error(format!(
				"set_wakeup_fd() takes 1 or 2 arguments ({argc} given)"
			));
		},
	};
	let fd = match int_arg(args[0], "fd") {
		Ok(fd) => fd,
		Err(error) => return error,
	};
	if fd < -1 || fd > i64::from(i32::MAX) {
		return unsafe {
			crate::abi::exc::pon_raise_value_error(
				b"fd must be -1 or a valid file descriptor".as_ptr(),
				37,
			)
		};
	}
	if fd != -1 {
		let flags = unsafe { libc::fcntl(fd as libc::c_int, libc::F_GETFL) };
		if flags == -1 {
			return raise_errno_text(last_errno());
		}
		if flags & libc::O_NONBLOCK == 0 {
			let message = format!("the fd {fd} must be in non-blocking mode");
			return unsafe {
				crate::abi::exc::pon_raise_value_error(message.as_ptr(), message.len())
			};
		}
	}
	let previous = WAKEUP_FD.swap(fd as i32, Ordering::SeqCst);
	unsafe { pon_const_int(i64::from(previous)) }
}

unsafe extern "C" fn signal_siginterrupt(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 2 || argv.is_null() {
		return return_null_with_error(format!(
			"siginterrupt() takes exactly 2 arguments ({argc} given)"
		));
	}
	let args = unsafe { std::slice::from_raw_parts(argv, argc) };
	let signalnum = match unsafe { parse_signalnum(args[0], "siginterrupt") } {
		Ok(signalnum) => signalnum as libc::c_int,
		Err(error) => return error,
	};
	let flag = match truth_arg(args[1]) {
		Ok(flag) => flag,
		Err(error) => return error,
	};
	let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
	if unsafe { libc::sigaction(signalnum, std::ptr::null(), action.as_mut_ptr()) } != 0 {
		return raise_errno_text(last_errno());
	}
	let mut action = unsafe { action.assume_init() };
	if flag == 1 {
		action.sa_flags &= !libc::SA_RESTART;
	} else {
		action.sa_flags |= libc::SA_RESTART;
	}
	if unsafe { libc::sigaction(signalnum, &action, std::ptr::null_mut()) } != 0 {
		return raise_errno_text(last_errno());
	}
	unsafe { crate::abi::pon_none() }
}

unsafe extern "C" fn signal_sigpending(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return return_null_with_error(format!("sigpending() takes no arguments ({argc} given)"));
	}
	let mut pending = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
	if unsafe { libc::sigpending(pending.as_mut_ptr()) } != 0 {
		return raise_errno_text(last_errno());
	}
	signal_set_to_object(unsafe { pending.assume_init() })
}

unsafe extern "C" fn signal_sigwait(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 1 || argv.is_null() {
		return return_null_with_error(format!("sigwait() takes exactly 1 argument ({argc} given)"));
	}
	let set = match signal_set_arg(unsafe { *argv }, "sigwait") {
		Ok(Some(set)) => set,
		Ok(None) => {
			return unsafe {
				crate::abi::exc::pon_raise_type_error(
					b"sigwait() arg must be an iterable of signals".as_ptr(),
					43,
				)
			};
		},
		Err(error) => return error,
	};
	let mut signum = 0;
	loop {
		crate::sync::enter_blocking_region();
		let errno = unsafe { libc::sigwait(&set, &mut signum) };
		let leave_result = crate::sync::leave_blocking_region();
		if crate::thread_state::pon_err_occurred() {
			return std::ptr::null_mut();
		}
		if let Err(message) = leave_result {
			return return_null_with_error(message);
		}
		if errno == 0 {
			return unsafe { pon_const_int(i64::from(signum)) };
		}
		if errno == libc::EINTR {
			if let Err(error) = unsafe { process_pending_signals() } {
				return error;
			}
			continue;
		}
		return raise_errno_text(errno);
	}
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn signal_sigwaitinfo(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 1 || argv.is_null() {
		return return_null_with_error(format!(
			"sigwaitinfo() takes exactly 1 argument ({argc} given)"
		));
	}
	let set = match signal_set_arg(unsafe { *argv }, "sigwaitinfo") {
		Ok(Some(set)) => set,
		Ok(None) => {
			return unsafe {
				crate::abi::exc::pon_raise_type_error(
					b"sigwaitinfo() arg must be an iterable of signals".as_ptr(),
					47,
				)
			};
		},
		Err(error) => return error,
	};
	loop {
		let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
		crate::sync::enter_blocking_region();
		let signalnum = unsafe { libc::sigwaitinfo(&set, info.as_mut_ptr()) };
		let leave_result = crate::sync::leave_blocking_region();
		if crate::thread_state::pon_err_occurred() {
			return std::ptr::null_mut();
		}
		if let Err(message) = leave_result {
			return return_null_with_error(message);
		}
		if signalnum >= 0 {
			return siginfo_result_object(&unsafe { info.assume_init() });
		}
		let errno = last_errno();
		if errno == libc::EINTR {
			if let Err(error) = unsafe { process_pending_signals() } {
				return error;
			}
			continue;
		}
		return raise_errno_text(errno);
	}
}

#[cfg(target_os = "linux")]
const SIGINFO_FIELDS: [&str; 7] = [
	"si_signo",
	"si_code",
	"si_errno",
	"si_pid",
	"si_uid",
	"si_status",
	"si_band",
];

#[cfg(target_os = "linux")]
#[repr(C)]
struct PySiginfoResult {
	ob_base: crate::object::PyObjectHeader,
	values:  [i64; 7],
}

#[cfg(target_os = "linux")]
static SIGINFO_SEQUENCE: std::sync::LazyLock<crate::object::PySequenceMethods> =
	std::sync::LazyLock::new(|| crate::object::PySequenceMethods {
		sq_length: Some(siginfo_result_len),
		sq_item: Some(siginfo_result_item),
		..crate::object::PySequenceMethods::EMPTY
	});

#[cfg(target_os = "linux")]
fn siginfo_result_type() -> *mut PyType {
	static TYPE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
		let mut ty = PyType::new(
			crate::abi::runtime_type_type().cast_const(),
			"signal.struct_siginfo",
			std::mem::size_of::<PySiginfoResult>(),
		);
		ty.tp_as_sequence = &*SIGINFO_SEQUENCE as *const crate::object::PySequenceMethods
			as *mut crate::object::PySequenceMethods;
		ty.tp_getattro = Some(siginfo_result_getattro);
		Box::into_raw(Box::new(ty)) as usize
	});
	*TYPE as *mut PyType
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn siginfo_result_len(_object: *mut PyObject) -> isize {
	SIGINFO_FIELDS.len() as isize
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn siginfo_result_item(object: *mut PyObject, index: isize) -> *mut PyObject {
	if index < 0 || index as usize >= SIGINFO_FIELDS.len() {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::IndexError,
			"struct_siginfo index out of range",
		);
	}
	let result = object.cast::<PySiginfoResult>();
	unsafe { pon_const_int((*result).values[index as usize]) }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn siginfo_result_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let name = crate::tag::untag_arg(name);
	let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		return crate::abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			"attribute name must be str",
		);
	};
	if let Some(index) = SIGINFO_FIELDS.iter().position(|field| *field == name_text) {
		return unsafe { siginfo_result_item(object, index as isize) };
	}
	unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) }
}

#[cfg(target_os = "linux")]
fn siginfo_result_object(info: &libc::siginfo_t) -> *mut PyObject {
	let values = [
		i64::from(info.si_signo),
		i64::from(info.si_code),
		i64::from(info.si_errno),
		unsafe { info.si_pid() } as i64,
		unsafe { info.si_uid() } as i64,
		unsafe { info.si_status() } as i64,
		0,
	];
	Box::into_raw(Box::new(PySiginfoResult {
		ob_base: crate::object::PyObjectHeader::new(siginfo_result_type()),
		values,
	}))
	.cast::<PyObject>()
}

unsafe extern "C" fn signal_strsignal(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 1 || argv.is_null() {
		return return_null_with_error(format!(
			"strsignal() takes exactly 1 argument ({argc} given)"
		));
	}
	let signalnum = match c_int_arg(unsafe { *argv }, "signalnum") {
		Ok(signalnum) => signalnum,
		Err(error) => return error,
	};
	let ptr = unsafe { libc::strsignal(signalnum) };
	if ptr.is_null() {
		return unsafe { crate::abi::pon_none() };
	}
	let text = unsafe { std::ffi::CStr::from_ptr(ptr) }.to_string_lossy();
	unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) }
}

unsafe extern "C" fn signal_valid_signals(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 {
		return return_null_with_error(format!("valid_signals() takes no arguments ({argc} given)"));
	}
	let mut set = empty_sigset();
	for signum in 1..NSIG {
		unsafe { libc::sigaddset(&mut set, signum as libc::c_int) };
	}
	signal_set_to_object(set)
}

fn itimer_tuple(value: libc::itimerval) -> *mut PyObject {
	let mut items = [
		unsafe { crate::abi::number::pon_const_float(timeval_to_seconds(value.it_value)) },
		unsafe { crate::abi::number::pon_const_float(timeval_to_seconds(value.it_interval)) },
	];
	unsafe { crate::abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

fn timeval_to_seconds(value: libc::timeval) -> f64 {
	value.tv_sec as f64 + value.tv_usec as f64 * 1e-6
}

fn seconds_to_timeval(seconds: f64) -> libc::timeval {
	let seconds = if seconds.is_sign_negative() {
		0.0
	} else {
		seconds
	};
	let whole = seconds.floor();
	let mut usec = ((seconds - whole) * 1_000_000.0).round() as i64;
	let mut sec = whole as i64;
	if usec >= 1_000_000 {
		sec += 1;
		usec -= 1_000_000;
	}
	libc::timeval { tv_sec: sec as libc::time_t, tv_usec: usec as libc::suseconds_t }
}

fn signal_set_arg(
	object: *mut PyObject,
	function: &str,
) -> Result<Option<libc::sigset_t>, *mut PyObject> {
	if object.is_null() || unsafe { crate::types::int::type_name_is(object, "NoneType") } {
		return Ok(None);
	}
	let mut set = empty_sigset();
	let values = match super::builtins_batch::collect_iterable(object) {
		Ok(values) => values,
		Err(message) => return Err(return_null_with_error(message)),
	};
	for value in values {
		let signalnum = match unsafe { parse_signalnum(value, function) } {
			Ok(signalnum) => signalnum as libc::c_int,
			Err(error) => return Err(error),
		};
		unsafe { libc::sigaddset(&mut set, signalnum) };
	}
	Ok(Some(set))
}

fn signal_set_to_object(set: libc::sigset_t) -> *mut PyObject {
	let mut items = Vec::new();
	for signum in 1..NSIG {
		let present =
			unsafe { libc::sigismember(&set as *const libc::sigset_t, signum as libc::c_int) };
		if present == 1 {
			let object = unsafe { pon_const_int(signum) };
			if object.is_null() {
				return std::ptr::null_mut();
			}
			items.push(object);
		}
	}
	let iterable = unsafe {
		crate::abi::seq::pon_build_list(
			if items.is_empty() {
				std::ptr::null_mut()
			} else {
				items.as_mut_ptr()
			},
			items.len(),
		)
	};
	if iterable.is_null() {
		return std::ptr::null_mut();
	}
	let set_ctor = unsafe { crate::abi::pon_load_global(intern("set"), std::ptr::null_mut()) };
	if set_ctor.is_null() {
		return std::ptr::null_mut();
	}
	let mut args = [iterable];
	unsafe { crate::abi::pon_call(set_ctor, args.as_mut_ptr(), args.len()) }
}

fn empty_sigset() -> libc::sigset_t {
	let mut set = unsafe { std::mem::zeroed::<libc::sigset_t>() };
	unsafe { libc::sigemptyset(&mut set) };
	set
}

unsafe fn arg_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
	if argc == 0 {
		Some(&[])
	} else if argv.is_null() {
		None
	} else {
		Some(unsafe { std::slice::from_raw_parts(argv, argc) })
	}
}

fn int_arg(object: *mut PyObject, what: &str) -> Result<i64, *mut PyObject> {
	let Some(value) = (unsafe { arg_as_i64(object) }) else {
		return Err(return_null_with_error(format!("{what} must be an integer")));
	};
	Ok(value)
}

fn float_arg(object: *mut PyObject, what: &str) -> Result<f64, *mut PyObject> {
	if let Some(value) = unsafe { crate::types::float::to_f64(object) } {
		if value.is_finite() && value >= 0.0 {
			return Ok(value);
		}
		return Err(unsafe {
			crate::abi::exc::pon_raise_value_error(
				format!("{what} must be non-negative and finite").as_ptr(),
				format!("{what} must be non-negative and finite").len(),
			)
		});
	}
	match unsafe { arg_as_i64(object) } {
		Some(value) if value >= 0 => Ok(value as f64),
		Some(_) => Err(unsafe {
			crate::abi::exc::pon_raise_value_error(
				format!("{what} must be non-negative").as_ptr(),
				format!("{what} must be non-negative").len(),
			)
		}),
		None => Err(unsafe {
			crate::abi::exc::pon_raise_type_error(
				format!("{what} must be a number").as_ptr(),
				format!("{what} must be a number").len(),
			)
		}),
	}
}
fn c_int_arg(object: *mut PyObject, what: &str) -> Result<libc::c_int, *mut PyObject> {
	let Some(value) = (unsafe { arg_as_i64(object) }) else {
		return Err(return_null_with_error(format!("{what} must be an integer")));
	};
	libc::c_int::try_from(value).map_err(|_| unsafe {
		crate::abi::exc::pon_raise_value_error(
			format!("{what} is out of range").as_ptr(),
			format!("{what} is out of range").len(),
		)
	})
}
fn unsigned_int_arg(object: *mut PyObject, what: &str) -> Result<libc::c_uint, *mut PyObject> {
	let Some(value) = (unsafe { arg_as_i64(object) }) else {
		return Err(return_null_with_error(format!("{what} must be an integer")));
	};
	libc::c_uint::try_from(value).map_err(|_| unsafe {
		crate::abi::exc::pon_raise_value_error(
			format!("{what} is out of range").as_ptr(),
			format!("{what} is out of range").len(),
		)
	})
}
fn thread_id_arg(object: *mut PyObject) -> Result<libc::pthread_t, *mut PyObject> {
	let Some(value) = (unsafe { arg_as_i64(object) }) else {
		return Err(return_null_with_error("thread_id must be an integer"));
	};
	#[allow(clippy::cast_sign_loss)]
	Ok(value as libc::pthread_t)
}

fn truth_arg(object: *mut PyObject) -> Result<libc::c_int, *mut PyObject> {
	match unsafe { crate::abi::pon_is_true(object) } {
		0 => Ok(0),
		1 => Ok(1),
		_ => Err(std::ptr::null_mut()),
	}
}

fn last_errno() -> i32 {
	std::io::Error::last_os_error()
		.raw_os_error()
		.unwrap_or(libc::EIO)
}

fn raise_errno_text(errno: i32) -> *mut PyObject {
	let detail = unsafe { std::ffi::CStr::from_ptr(libc::strerror(errno)) }.to_string_lossy();
	crate::abi::exc::raise_kind_error_text(
		ExceptionKind::OSError,
		&format!("[Errno {errno}] {detail}"),
	)
}

fn raise_itimer_errno() -> *mut PyObject {
	let errno = last_errno();
	let detail = unsafe { std::ffi::CStr::from_ptr(libc::strerror(errno)) }.to_string_lossy();
	let message = format!("[Errno {errno}] {detail}");
	let message_obj = unsafe { crate::abi::pon_const_str(message.as_ptr(), message.len()) };
	if message_obj.is_null() {
		return std::ptr::null_mut();
	}
	let mut args = [message_obj];
	let exception = unsafe {
		crate::abi::pon_call(itimer_error_type().cast::<PyObject>(), args.as_mut_ptr(), args.len())
	};
	if exception.is_null() {
		return std::ptr::null_mut();
	}
	unsafe { crate::abi::pon_raise(exception, std::ptr::null_mut()) }
}
