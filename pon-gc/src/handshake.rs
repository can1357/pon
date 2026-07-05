//! Free-threading GC stop-request handshake primitives.
//!
//! The current collector remains stop-the-world and single-runtime-threaded by
//! default.  This module establishes the phase and stop-request surface that
//! later free-threaded safepoints will share.  Without the `free-threading`
//! feature, requests are intentionally inert and query functions report that no
//! stop is pending.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

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

#[cfg(feature = "free-threading")]
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
/// In default builds the methods that would request or advance a stop are
/// documented no-ops: the invariant is that there are no concurrently executing
/// mutator threads to coordinate.  In free-threaded builds the same methods
/// update atomics that safepoints and collectors can poll.
#[derive(Debug)]
pub struct GcHandshake {
	phase:          AtomicU8,
	stop_requested: AtomicBool,
}

impl GcHandshake {
	/// Creates an idle handshake with no stop request pending.
	#[must_use]
	pub const fn new() -> Self {
		Self {
			phase:          AtomicU8::new(GcPhase::Idle as u8),
			stop_requested: AtomicBool::new(false),
		}
	}

	/// Returns the current collector phase.
	///
	/// Default builds always report [`GcPhase::Idle`].
	#[must_use]
	pub fn phase(&self) -> GcPhase {
		#[cfg(feature = "free-threading")]
		{
			GcPhase::from_byte(self.phase.load(Ordering::Acquire))
		}

		#[cfg(not(feature = "free-threading"))]
		{
			let _ = self.phase.load(Ordering::Relaxed);
			GcPhase::Idle
		}
	}

	/// Returns whether this handshake has a stop request pending.
	///
	/// Default builds always return `false`; a collector stop cannot be pending
	/// without free-threaded mutators.
	#[must_use]
	pub fn stop_requested(&self) -> bool {
		#[cfg(feature = "free-threading")]
		{
			self.stop_requested.load(Ordering::Acquire)
		}

		#[cfg(not(feature = "free-threading"))]
		{
			let _ = self.stop_requested.load(Ordering::Relaxed);
			false
		}
	}

	/// Requests that mutators stop at their next safepoint.
	///
	/// This is intentionally inert in default builds, where there are no
	/// concurrent mutators to stop.
	pub fn request_stop(&self) {
		#[cfg(feature = "free-threading")]
		{
			self.stop_requested.store(true, Ordering::Release);
			self
				.phase
				.store(GcPhase::StopRequested as u8, Ordering::Release);
		}
	}

	/// Records that one or more mutators reached a safepoint for this stop.
	///
	/// Acknowledgement advances only an active stop request from
	/// [`GcPhase::StopRequested`] to [`GcPhase::Stopped`].  It is idempotent and
	/// intentionally inert in default builds.
	pub fn ack_stop(&self) {
		#[cfg(feature = "free-threading")]
		{
			if self.stop_requested.load(Ordering::Acquire) {
				let _ = self.phase.compare_exchange(
					GcPhase::StopRequested as u8,
					GcPhase::Stopped as u8,
					Ordering::AcqRel,
					Ordering::Acquire,
				);
			}
		}
	}

	/// Clears any pending stop request and returns to [`GcPhase::Idle`].
	///
	/// This is intentionally inert in default builds.
	pub fn resume(&self) {
		#[cfg(feature = "free-threading")]
		{
			self.phase.store(GcPhase::Idle as u8, Ordering::Release);
			self.stop_requested.store(false, Ordering::Release);
		}
	}

	/// Compatibility name for resuming mutators after a stop request.
	pub fn clear_stop_request(&self) {
		self.resume();
	}

	/// Sets the current collector phase.
	///
	/// Default builds ignore the phase because collection coordination is inert
	/// unless `free-threading` is enabled.
	pub fn set_phase(&self, phase: GcPhase) {
		#[cfg(feature = "free-threading")]
		{
			self.phase.store(phase as u8, Ordering::Release);
		}

		#[cfg(not(feature = "free-threading"))]
		{
			let _ = phase;
		}
	}
}

impl Default for GcHandshake {
	fn default() -> Self {
		Self::new()
	}
}

/// Process-wide GC handshake used by runtime safepoints.
pub static GLOBAL_GC_HANDSHAKE: GcHandshake = GcHandshake::new();

/// Stop-request flag imported by generated code in free-threaded builds.
///
/// The symbol is absent from default builds.  Codegen must import it only when
/// compiling with `free-threading`; default builds use [`gc_stop_requested`],
/// which is guaranteed to return `false`.
#[cfg(feature = "free-threading")]
#[unsafe(no_mangle)]
pub static GC_STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Returns the process-wide handshake.
#[must_use]
pub fn global_handshake() -> &'static GcHandshake {
	&GLOBAL_GC_HANDSHAKE
}

/// Requests a process-wide GC stop.
///
/// Default builds intentionally leave both the handshake and query surface
/// inert.
pub fn request_global_stop() {
	#[cfg(feature = "free-threading")]
	{
		GC_STOP_REQUESTED.store(true, Ordering::Release);
		GLOBAL_GC_HANDSHAKE.request_stop();
	}
}

/// Clears the process-wide GC stop request.
///
/// Default builds intentionally leave the query surface inert.
pub fn clear_global_stop_request() {
	#[cfg(feature = "free-threading")]
	{
		GC_STOP_REQUESTED.store(false, Ordering::Release);
		GLOBAL_GC_HANDSHAKE.clear_stop_request();
	}
}

/// Records that process-wide mutators have acknowledged a pending GC stop.
///
/// Default builds intentionally leave the coordination surface inert.
pub fn ack_global_stop() {
	#[cfg(feature = "free-threading")]
	{
		GLOBAL_GC_HANDSHAKE.ack_stop();
	}
}

/// Resumes process-wide mutators after a GC stop.
///
/// Default builds intentionally leave the query surface inert.
pub fn resume_global_stop() {
	#[cfg(feature = "free-threading")]
	{
		GC_STOP_REQUESTED.store(false, Ordering::Release);
		GLOBAL_GC_HANDSHAKE.resume();
	}
}

/// Returns whether generated code should stop at a GC safepoint.
///
/// Default builds always return `false`, preserving the Phase-A/D invariant
/// that GC never asks another runtime thread to stop.
#[must_use]
pub fn gc_stop_requested() -> bool {
	#[cfg(feature = "free-threading")]
	{
		GC_STOP_REQUESTED.load(Ordering::Acquire) || GLOBAL_GC_HANDSHAKE.stop_requested()
	}

	#[cfg(not(feature = "free-threading"))]
	{
		false
	}
}

/// C ABI query for generated code that cannot import atomics directly.
///
/// The function is exported only for free-threaded builds; default builds keep
/// the generated-code stop-request ABI surface absent and inert.
#[cfg(feature = "free-threading")]
#[unsafe(no_mangle)]
pub extern "C" fn pon_gc_stop_requested() -> bool {
	gc_stop_requested()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn local_handshake_requests_and_phase_transitions_follow_build_mode() {
		let handshake = GcHandshake::new();

		assert_eq!(handshake.phase(), GcPhase::Idle);
		assert!(!handshake.stop_requested());

		handshake.request_stop();

		#[cfg(feature = "free-threading")]
		{
			assert_eq!(handshake.phase(), GcPhase::StopRequested);
			assert!(handshake.stop_requested());
		}

		#[cfg(not(feature = "free-threading"))]
		{
			assert_eq!(handshake.phase(), GcPhase::Idle);
			assert!(!handshake.stop_requested());
		}

		handshake.ack_stop();

		#[cfg(feature = "free-threading")]
		{
			assert_eq!(handshake.phase(), GcPhase::Stopped);
			assert!(handshake.stop_requested());
		}

		#[cfg(not(feature = "free-threading"))]
		{
			assert_eq!(handshake.phase(), GcPhase::Idle);
			assert!(!handshake.stop_requested());
		}

		handshake.set_phase(GcPhase::Collecting);

		#[cfg(feature = "free-threading")]
		assert_eq!(handshake.phase(), GcPhase::Collecting);

		#[cfg(not(feature = "free-threading"))]
		assert_eq!(handshake.phase(), GcPhase::Idle);

		handshake.resume();

		assert_eq!(handshake.phase(), GcPhase::Idle);
		assert!(!handshake.stop_requested());
	}

	#[test]
	fn global_stop_request_query_follows_build_mode_and_clears() {
		clear_global_stop_request();
		assert_eq!(global_handshake().phase(), GcPhase::Idle);
		assert!(!global_handshake().stop_requested());
		assert!(!gc_stop_requested());

		request_global_stop();

		#[cfg(feature = "free-threading")]
		{
			assert_eq!(global_handshake().phase(), GcPhase::StopRequested);
			assert!(global_handshake().stop_requested());
			assert!(GC_STOP_REQUESTED.load(Ordering::Acquire));
			assert!(gc_stop_requested());
			assert!(pon_gc_stop_requested());
		}

		#[cfg(not(feature = "free-threading"))]
		{
			assert_eq!(global_handshake().phase(), GcPhase::Idle);
			assert!(!global_handshake().stop_requested());
			assert!(!gc_stop_requested());
		}

		ack_global_stop();

		#[cfg(feature = "free-threading")]
		{
			assert_eq!(global_handshake().phase(), GcPhase::Stopped);
			assert!(global_handshake().stop_requested());
			assert!(gc_stop_requested());
		}

		#[cfg(not(feature = "free-threading"))]
		{
			assert_eq!(global_handshake().phase(), GcPhase::Idle);
			assert!(!global_handshake().stop_requested());
			assert!(!gc_stop_requested());
		}

		resume_global_stop();

		assert_eq!(global_handshake().phase(), GcPhase::Idle);
		assert!(!global_handshake().stop_requested());
		assert!(!gc_stop_requested());

		#[cfg(feature = "free-threading")]
		assert!(!GC_STOP_REQUESTED.load(Ordering::Acquire));
	}
}
