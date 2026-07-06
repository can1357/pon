//! Runtime thread state.
//!
//! Each attached OS thread owns one leaked `PonThreadState` mutex registered in
//! `crate::thread`.  Runtime helpers keep using the same guard API while the GC
//! can enumerate every live thread's exception, frame, and safe-region metadata.

#[cfg(test)]
use std::sync::LazyLock;
use std::{ptr, sync::{Mutex, MutexGuard}};

use crate::{
	abi::{HandlerInfo, PyFrame},
	object::PyObject,
};

/// Live `except*` dispatch bookkeeping; one frame per dynamically-active
/// dispatcher, innermost last.
#[derive(Debug)]
pub struct ExcStarFrame {
	/// Exception pending when the dispatcher was entered.
	pub original: *mut PyObject,
	/// Unmatched remainder after clauses processed so far.
	pub rest:     *mut PyObject,
	/// Exceptions raised by clause bodies, in clause order.
	pub raised:   Vec<*mut PyObject>,
}

impl ExcStarFrame {
	#[must_use]
	pub fn new(original: *mut PyObject) -> Self {
		Self { original, rest: original, raised: Vec::new() }
	}
}

/// One in-flight call whose callable/argv roots must remain visible to the GC
/// even when the owning Rust helper frame is suspended or another thread is
/// being collected.
#[derive(Clone, Copy, Debug)]
pub struct GcCallRoots {
	pub callable: *mut PyObject,
	pub argv:     *mut *mut PyObject,
	pub argc:     usize,
}

/// Type-erased root-source callback stored in [`PonThreadState`].
pub type ScopedRootsFn = unsafe fn(usize, &mut dyn FnMut(*mut PyObject));

/// One helper-owned root source whose backing storage lives outside the stack.
#[derive(Clone, Copy, Debug)]
pub struct ScopedRootSourceEntry {
	pub addr:  usize,
	pub thunk: ScopedRootsFn,
}

#[derive(Clone, Debug)]
enum DiagnosticMessage {
	Text(String),
	LazyExceptionDisplay { placeholder: String },
}

impl DiagnosticMessage {
	fn text(message: impl Into<String>) -> Self {
		Self::Text(message.into())
	}
}

/// Interpreter state observed by runtime helpers.
#[derive(Debug)]
pub struct PonThreadState {
	/// Current exception object.  Non-null means an error is pending.
	pub current_exc:           *mut PyObject,
	/// Python frame stack roots for later traceback, generators, and GC
	/// integration.
	pub frame_stack:           Vec<*mut PyFrame>,
	/// Active exception-handler chain, innermost handler last.
	pub handler_chain:         Vec<HandlerInfo>,
	/// Saved exception states for `finally`, generators, and exception-group
	/// flows.
	pub exception_state_stack: Vec<*mut PyObject>,
	/// Active `except*` dispatcher frames, innermost handler last.
	pub exc_star_stack:        Vec<ExcStarFrame>,
	/// Exception most recently caught by a live handler in the current frame
	/// context (CPython's `exc_info->exc_value`, the `sys.exception()`
	/// source).  Parked by the handler-entry helpers (`pon_match_exc`,
	/// `pon_get_current_exc`, `pon_exc_star_match`) — which run before any
	/// call can clear `current_exc` — and saved/restored around every
	/// compiled-code call boundary so it scopes to the catching frame.
	/// Divergence (documented in `native/sys.rs`): CPython resets this when
	/// the `except` BLOCK exits; pon resets when the catching FRAME returns,
	/// so same-frame reads after the block still see the last caught
	/// exception.
	pub handled_exc:           *mut PyObject,
	/// Call-boundary save stack for [`Self::handled_exc`] (innermost last).
	/// Lives here rather than in guard locals so saved exceptions stay
	/// visible to the precise GC root scan.
	pub handled_exc_saves:     Vec<*mut PyObject>,
	/// Conservative stack-base capture for stop-the-world collection.
	pub stack_base:            *mut u8,
	/// Approximate top frame/stack pointer recorded when entering a GC-safe
	/// region.
	pub stack_top_fp:          *mut u8,
	/// In-flight Python call roots mirrored out of TLS for cross-thread GC.
	pub current_call_roots:    Vec<GcCallRoots>,
	/// In-flight helper-frame call operands mirrored out of TLS for cross-thread GC.
	pub helper_call_roots:     Vec<GcCallRoots>,
	/// Helper-owned heap buffers that must stay rooted across re-entry.
	pub scoped_root_sources:   Vec<ScopedRootSourceEntry>,
	/// Nested GC-safe-region depth for the current thread.
	gc_safe_region_depth:      usize,
	diagnostic_message:        Option<DiagnosticMessage>,
}

unsafe impl Send for PonThreadState {}

impl Default for PonThreadState {
	fn default() -> Self {
		Self {
			current_exc:           ptr::null_mut(),
			frame_stack:           Vec::new(),
			handler_chain:         Vec::new(),
			exception_state_stack: Vec::new(),
			exc_star_stack:        Vec::new(),
			handled_exc:           ptr::null_mut(),
			handled_exc_saves:     Vec::new(),
			stack_base:            ptr::null_mut(),
			stack_top_fp:          ptr::null_mut(),
			current_call_roots:    Vec::new(),
			helper_call_roots:     Vec::new(),
			scoped_root_sources:   Vec::new(),
			gc_safe_region_depth:  0,
			diagnostic_message:    None,
		}
	}
}

impl PonThreadState {
	/// Returns the active Python frame stack.
	#[must_use]
	pub fn frames(&self) -> &[*mut PyFrame] {
		&self.frame_stack
	}

	/// Pushes a frame pointer onto the active frame stack.
	pub fn push_frame(&mut self, frame: *mut PyFrame) {
		self.frame_stack.push(frame);
	}

	/// Pops the active frame stack.
	pub fn pop_frame(&mut self) -> Option<*mut PyFrame> {
		self.frame_stack.pop()
	}

	/// Returns the current frame pointer, if one is active.
	#[must_use]
	pub fn current_frame(&self) -> Option<*mut PyFrame> {
		self.frame_stack.last().copied()
	}

	/// Returns the active exception-handler chain.
	#[must_use]
	pub fn handlers(&self) -> &[HandlerInfo] {
		&self.handler_chain
	}

	/// Pushes an exception-handler record.
	pub fn push_handler(&mut self, handler: HandlerInfo) {
		self.handler_chain.push(handler);
	}

	/// Pops the innermost exception-handler record.
	pub fn pop_handler(&mut self) -> Option<HandlerInfo> {
		self.handler_chain.pop()
	}

	/// Returns the innermost exception-handler record, if any.
	#[must_use]
	pub fn current_handler(&self) -> Option<HandlerInfo> {
		self.handler_chain.last().copied()
	}

	/// Returns saved exception states.
	#[must_use]
	pub fn exception_states(&self) -> &[*mut PyObject] {
		&self.exception_state_stack
	}

	/// Saves an exception state pointer on the stack.
	pub fn push_exception_state(&mut self, exception: *mut PyObject) {
		self.exception_state_stack.push(exception);
	}

	/// Restores the latest saved exception state pointer.
	pub fn pop_exception_state(&mut self) -> Option<*mut PyObject> {
		self.exception_state_stack.pop()
	}

	/// Marks this thread as being in a GC-safe region.
	pub fn enter_gc_safe_region(&mut self, stack_top_fp: *mut u8) {
		if self.gc_safe_region_depth == 0 {
			self.stack_top_fp = stack_top_fp;
		}
		self.gc_safe_region_depth = self.gc_safe_region_depth.saturating_add(1);
	}

	/// Leaves one nested GC-safe region.
	pub fn leave_gc_safe_region(&mut self) -> Result<(), &'static str> {
		if self.gc_safe_region_depth == 0 {
			return Err("GC safe-region leave without matching enter");
		}
		self.gc_safe_region_depth -= 1;
		if self.gc_safe_region_depth == 0 {
			self.stack_top_fp = ptr::null_mut();
		}
		Ok(())
	}

	/// Returns true when this thread is inside at least one GC-safe region.
	#[must_use]
	pub fn in_gc_safe_region(&self) -> bool {
		self.gc_safe_region_depth != 0
	}

	/// Returns the current nested GC-safe-region depth.
	#[must_use]
	pub fn gc_safe_region_depth(&self) -> usize {
		self.gc_safe_region_depth
	}

	/// Clears the diagnostic message (the in-lock counterpart of
	/// [`pon_err_clear`] for callers already holding the state guard).
	pub fn clear_diagnostic(&mut self) {
		self.diagnostic_message = None;
	}
}

/// Returns the active thread state mutex.
#[must_use]
pub fn thread_state() -> &'static Mutex<PonThreadState> {
	crate::thread::current_thread_state_mutex()
}

/// Locks the active thread state, recovering poisoned state instead of
/// unwinding through the C ABI.
#[must_use]
pub fn thread_state_lock() -> MutexGuard<'static, PonThreadState> {
	let state = crate::thread::current_thread_state_mutex();

	match state.lock() {
		Ok(guard) => guard,
		Err(poisoned) => poisoned.into_inner(),
	}
}

#[cfg(test)]
static TEST_STATE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Serializes tests that mutate process-global runtime/thread exception state.
#[cfg(test)]
#[must_use]
pub fn test_state_lock() -> MutexGuard<'static, ()> {
	TEST_STATE_LOCK
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
}

/// Records a Phase-A diagnostic error.
///
/// Until exception classes are introduced, the diagnostic string is the stable
/// payload and `current_exc` is a non-null sentinel.  Runtime helpers that have
/// a concrete boxed exception may use [`pon_err_set_object`] instead.
///
/// A live BOXED exception already pending is authoritative and is preserved:
/// replacing it with a message-only sentinel would strip its type and make it
/// uncatchable by `except` clauses (`pon_exc_matches` never matches the
/// sentinel).  Helpers that intend to substitute a new error must
/// [`pon_err_clear`] first or raise a boxed exception via `pon_raise_*`.
pub fn pon_err_set(message: impl Into<String>) {
	let mut state = thread_state_lock();
	let sentinel = core::ptr::NonNull::<PyObject>::dangling().as_ptr();
	if !state.current_exc.is_null() && state.current_exc != sentinel {
		return;
	}
	state.current_exc = sentinel;
	state.diagnostic_message = Some(DiagnosticMessage::text(message));
}

/// Records an error with a concrete boxed exception object.
pub fn pon_err_set_object(exception: *mut PyObject, message: impl Into<String>) {
	let mut state = thread_state_lock();
	state.current_exc = if exception.is_null() {
		core::ptr::NonNull::<PyObject>::dangling().as_ptr()
	} else {
		exception
	};
	state.diagnostic_message = Some(DiagnosticMessage::text(message));
}

/// Records a boxed exception whose human-readable display may re-enter Pon.
///
/// The cheap placeholder remains available for consumers that only need a
/// type-shaped diagnostic; [`pon_err_message`] renders the full text after
/// dropping the thread-state lock.
pub(crate) fn pon_err_set_object_lazy_display(
	exception: *mut PyObject,
	placeholder: impl Into<String>,
) {
	let mut state = thread_state_lock();
	state.current_exc = if exception.is_null() {
		core::ptr::NonNull::<PyObject>::dangling().as_ptr()
	} else {
		exception
	};
	state.diagnostic_message =
		Some(DiagnosticMessage::LazyExceptionDisplay { placeholder: placeholder.into() });
}

/// Clears the current exception state.
pub fn pon_err_clear() {
	let mut state = thread_state_lock();
	state.current_exc = ptr::null_mut();
	state.diagnostic_message = None;
}

/// Returns true when an exception is pending.
#[must_use]
pub fn pon_err_occurred() -> bool {
	!thread_state_lock().current_exc.is_null()
}

/// Returns the latest Phase-A diagnostic message, if any.
#[must_use]
pub fn pon_err_message() -> Option<String> {
	let (current_exc, diagnostic) = {
		let state = thread_state_lock();
		(state.current_exc, state.diagnostic_message.clone())
	};
	match diagnostic? {
		DiagnosticMessage::Text(message) => Some(message),
		DiagnosticMessage::LazyExceptionDisplay { placeholder } => {
			if current_exc.is_null() || crate::abi::exc::is_diagnostic_sentinel(current_exc) {
				return Some(placeholder);
			}
			let roots = vec![current_exc];
			let _guard = crate::abi::scoped_roots(&roots as *const _);
			let rendered = crate::abi::exc::exception_display_diagnostic(current_exc, &placeholder);
			let mut state = thread_state_lock();
			state.current_exc = current_exc;
			state.diagnostic_message = Some(DiagnosticMessage::LazyExceptionDisplay { placeholder });
			Some(rendered)
		},
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn diagnostic_message_sets_exception_sentinel() {
		let _guard = test_state_lock();
		pon_err_clear();
		pon_err_set("boom");
		assert!(pon_err_occurred());
		assert_eq!(pon_err_message().as_deref(), Some("boom"));
		pon_err_clear();
	}

	#[test]
	fn handler_and_exception_stack_accessors_round_trip() {
		let mut state = PonThreadState::default();
		let frame = core::ptr::NonNull::<PyFrame>::dangling().as_ptr();
		let exception = core::ptr::NonNull::<PyObject>::dangling().as_ptr();
		let handler = HandlerInfo::new(frame, 7, 3, 1);

		state.push_frame(frame);
		state.push_handler(handler);
		state.push_exception_state(exception);

		assert_eq!(state.frames(), &[frame]);
		assert_eq!(state.current_frame(), Some(frame));
		assert_eq!(state.handlers(), &[handler]);
		assert_eq!(state.current_handler(), Some(handler));
		assert_eq!(state.exception_states(), &[exception]);

		assert_eq!(state.pop_exception_state(), Some(exception));
		assert_eq!(state.pop_handler(), Some(handler));
		assert_eq!(state.pop_frame(), Some(frame));
		assert!(state.exception_states().is_empty());
		assert!(state.handlers().is_empty());
		assert!(state.frames().is_empty());
	}

	#[test]
	fn gc_safe_region_depth_tracks_nested_enters_and_rejects_unmatched_leave() {
		let mut state = PonThreadState::default();
		let first_stack_top = core::ptr::NonNull::<u8>::dangling().as_ptr();
		let second_stack_top = first_stack_top.wrapping_add(1);

		assert!(!state.in_gc_safe_region());
		assert_eq!(state.gc_safe_region_depth(), 0);
		assert!(state.stack_top_fp.is_null());

		state.enter_gc_safe_region(first_stack_top);
		assert!(state.in_gc_safe_region());
		assert_eq!(state.gc_safe_region_depth(), 1);
		assert_eq!(state.stack_top_fp, first_stack_top);

		state.enter_gc_safe_region(second_stack_top);
		assert!(state.in_gc_safe_region());
		assert_eq!(state.gc_safe_region_depth(), 2);
		assert_eq!(state.stack_top_fp, first_stack_top);

		assert_eq!(state.leave_gc_safe_region(), Ok(()));
		assert!(state.in_gc_safe_region());
		assert_eq!(state.gc_safe_region_depth(), 1);
		assert_eq!(state.stack_top_fp, first_stack_top);

		assert_eq!(state.leave_gc_safe_region(), Ok(()));
		assert!(!state.in_gc_safe_region());
		assert_eq!(state.gc_safe_region_depth(), 0);
		assert!(state.stack_top_fp.is_null());

		assert_eq!(state.leave_gc_safe_region(), Err("GC safe-region leave without matching enter"));
		assert!(!state.in_gc_safe_region());
		assert_eq!(state.gc_safe_region_depth(), 0);
		assert!(state.stack_top_fp.is_null());
	}
}
