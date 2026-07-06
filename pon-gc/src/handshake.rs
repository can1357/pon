//! GC stop-request handshake primitives for the no-GIL runtime.
//!
//! Mutators poll the process-wide stop flag at generated-code safepoints and
//! around blocking native waits.  A collector requests a stop, waits until
//! every other registered thread has published a GC-safe stack range, then
//! resumes them after tracing.

use std::sync::{
	Condvar, Mutex,
	atomic::{AtomicBool, AtomicU8, Ordering},
};

/// Collector handshake phase visible to runtime safepoints.
///
/// The numeric representation is stable for generated code and diagnostics.
/// Unknown byte values are treated as [`GcPhase::Idle`] when read back through
/// the safe API so a corrupted or future value never fabricates a stop request.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GcPhase {
	/// No collection coordination is active.
	#[default]
	Idle          = 0,
	/// A collector has requested participating threads to stop at safepoints.
	StopRequested = 1,
	/// Participating threads have stopped and the collector may proceed.
	Stopped       = 2,
	/// The collector is tracing or sweeping while mutators remain stopped.
	Collecting    = 3,
}

impl GcPhase {
	const fn from_byte(value: u8) -> Self {
		match value {
			value if value == Self::StopRequested as u8 => Self::StopRequested,
			value if value == Self::Stopped as u8 => Self::Stopped,
			value if value == Self::Collecting as u8 => Self::Collecting,
			_ => Self::Idle,
		}
	}
}

/// Per-process GC handshake state.
///
/// The atomics are the hot path: generated code performs a relaxed-sized query
/// through [`gc_stop_requested`].  The mutex/condvar pair is cold and exists
/// only to park mutators that have acknowledged a stop until the collector
/// resumes them.
#[derive(Debug)]
pub struct GcHandshake {
	phase:          AtomicU8,
	stop_requested: AtomicBool,
	wait_lock:      Mutex<()>,
	resumed:        Condvar,
}

impl GcHandshake {
	/// Creates an idle handshake with no stop request pending.
	#[must_use]
	pub const fn new() -> Self {
		Self {
			phase:          AtomicU8::new(GcPhase::Idle as u8),
			stop_requested: AtomicBool::new(false),
			wait_lock:      Mutex::new(()),
			resumed:        Condvar::new(),
		}
	}

	/// Returns the current collector phase.
	#[must_use]
	pub fn phase(&self) -> GcPhase {
		GcPhase::from_byte(self.phase.load(Ordering::Acquire))
	}

	/// Returns whether this handshake has a stop request pending.
	#[must_use]
	pub fn stop_requested(&self) -> bool {
		self.stop_requested.load(Ordering::Acquire)
	}

	/// Requests that mutators stop at their next safepoint.
	pub fn request_stop(&self) {
		let _guard = self
			.wait_lock
			.lock()
			.unwrap_or_else(|poison| poison.into_inner());
		self.stop_requested.store(true, Ordering::Release);
		self
			.phase
			.store(GcPhase::StopRequested as u8, Ordering::Release);
	}

	/// Records that one or more mutators reached a safepoint for this stop.
	///
	/// Acknowledgement advances only an active stop request from
	/// [`GcPhase::StopRequested`] to [`GcPhase::Stopped`].  It is idempotent:
	/// per-thread stopped-state is published in the runtime thread registry.
	pub fn ack_stop(&self) {
		if self.stop_requested.load(Ordering::Acquire) {
			let _ = self.phase.compare_exchange(
				GcPhase::StopRequested as u8,
				GcPhase::Stopped as u8,
				Ordering::AcqRel,
				Ordering::Acquire,
			);
		}
	}

	/// Parks the current mutator until the active stop request is cleared.
	pub fn wait_for_resume(&self) {
		if !self.stop_requested.load(Ordering::Acquire) {
			return;
		}
		let mut guard = self
			.wait_lock
			.lock()
			.unwrap_or_else(|poison| poison.into_inner());
		while self.stop_requested.load(Ordering::Acquire) {
			guard = self
				.resumed
				.wait(guard)
				.unwrap_or_else(|poison| poison.into_inner());
		}
	}

	/// Clears any pending stop request and returns to [`GcPhase::Idle`].
	pub fn resume(&self) {
		{
			let _guard = self
				.wait_lock
				.lock()
				.unwrap_or_else(|poison| poison.into_inner());
			self.phase.store(GcPhase::Idle as u8, Ordering::Release);
			self.stop_requested.store(false, Ordering::Release);
		}
		self.resumed.notify_all();
	}

	/// Compatibility name for resuming mutators after a stop request.
	pub fn clear_stop_request(&self) {
		self.resume();
	}

	/// Sets the current collector phase.
	pub fn set_phase(&self, phase: GcPhase) {
		self.phase.store(phase as u8, Ordering::Release);
	}
}

impl Default for GcHandshake {
	fn default() -> Self {
		Self::new()
	}
}

/// Process-wide GC handshake used by runtime safepoints.
pub static GLOBAL_GC_HANDSHAKE: GcHandshake = GcHandshake::new();

/// Stop-request flag imported by generated code.
#[unsafe(no_mangle)]
pub static GC_STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Returns the process-wide handshake.
#[must_use]
pub fn global_handshake() -> &'static GcHandshake {
	&GLOBAL_GC_HANDSHAKE
}

/// Requests a process-wide GC stop.
pub fn request_global_stop() {
	GC_STOP_REQUESTED.store(true, Ordering::Release);
	GLOBAL_GC_HANDSHAKE.request_stop();
}

/// Clears the process-wide GC stop request.
pub fn clear_global_stop_request() {
	GC_STOP_REQUESTED.store(false, Ordering::Release);
	GLOBAL_GC_HANDSHAKE.clear_stop_request();
}

/// Records that process-wide mutators have acknowledged a pending GC stop.
pub fn ack_global_stop() {
	GLOBAL_GC_HANDSHAKE.ack_stop();
}

/// Parks a stopped mutator until the collector resumes it.
pub fn wait_for_global_resume() {
	GLOBAL_GC_HANDSHAKE.wait_for_resume();
}

/// Resumes process-wide mutators after a GC stop.
pub fn resume_global_stop() {
	GC_STOP_REQUESTED.store(false, Ordering::Release);
	GLOBAL_GC_HANDSHAKE.resume();
}

/// Returns whether generated code should stop at a GC safepoint.
#[must_use]
pub fn gc_stop_requested() -> bool {
	GC_STOP_REQUESTED.load(Ordering::Acquire) || GLOBAL_GC_HANDSHAKE.stop_requested()
}

/// C ABI query for generated code that cannot import atomics directly.
#[unsafe(no_mangle)]
pub extern "C" fn pon_gc_stop_requested() -> bool {
	gc_stop_requested()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn local_handshake_requests_and_phase_transitions() {
		let handshake = GcHandshake::new();

		assert_eq!(handshake.phase(), GcPhase::Idle);
		assert!(!handshake.stop_requested());

		handshake.request_stop();

		assert_eq!(handshake.phase(), GcPhase::StopRequested);
		assert!(handshake.stop_requested());

		handshake.ack_stop();

		assert_eq!(handshake.phase(), GcPhase::Stopped);
		assert!(handshake.stop_requested());

		handshake.set_phase(GcPhase::Collecting);
		assert_eq!(handshake.phase(), GcPhase::Collecting);

		handshake.resume();

		assert_eq!(handshake.phase(), GcPhase::Idle);
		assert!(!handshake.stop_requested());
	}

	#[test]
	fn global_stop_request_query_sets_ack_and_clears() {
		clear_global_stop_request();
		assert_eq!(global_handshake().phase(), GcPhase::Idle);
		assert!(!global_handshake().stop_requested());
		assert!(!gc_stop_requested());

		request_global_stop();

		assert_eq!(global_handshake().phase(), GcPhase::StopRequested);
		assert!(global_handshake().stop_requested());
		assert!(GC_STOP_REQUESTED.load(Ordering::Acquire));
		assert!(gc_stop_requested());
		assert!(pon_gc_stop_requested());

		ack_global_stop();

		assert_eq!(global_handshake().phase(), GcPhase::Stopped);
		assert!(global_handshake().stop_requested());
		assert!(gc_stop_requested());

		resume_global_stop();

		assert_eq!(global_handshake().phase(), GcPhase::Idle);
		assert!(!global_handshake().stop_requested());
		assert!(!gc_stop_requested());
		assert!(!GC_STOP_REQUESTED.load(Ordering::Acquire));
	}
}
