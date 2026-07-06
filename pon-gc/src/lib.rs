//! Stop-the-world heap for runtime objects.
//!
//! # Architecture
//!
//! Small objects (at most 1 KiB) live in 64 KiB, 64 KiB-aligned spans
//! segregated by 16-byte size classes, in the style of Go's page-heap and its
//! Green Tea collector:
//!
//! - **Allocation** is a free-bit scan in the class's open span: no system
//!   malloc on the hot path, and freed slots are reused after each sweep.
//! - **Pointer classification** masks an address to its 64 KiB span base and
//!   binary-searches the small sorted span-base table; slot arithmetic resolves
//!   interior pointers. No per-object index is ever built.
//! - **Marking** is span-granular: the work queue holds spans, not objects.
//!   Each dequeue scans every accumulated seen-but-unscanned object in that
//!   span in address order (per-span seen/scanned bitmaps), converting random
//!   pointer chasing into near-linear passes over span memory.
//!
//! Objects larger than 1 KiB get individual 16-byte-aligned system
//! allocations tracked by a lazily sorted address table.
#![allow(improper_ctypes_definitions)]

pub mod handshake;

use std::{
	alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error},
	collections::VecDeque,
	ptr::{self, NonNull},
	sync::{
		LazyLock, Mutex, MutexGuard,
		atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicUsize, Ordering},
	},
};

pub use handshake::{
	GcHandshake, GcPhase, ack_global_stop, clear_global_stop_request, gc_stop_requested,
	global_handshake, request_global_stop, resume_global_stop, wait_for_global_resume,
};

/// Default byte alignment used for every heap allocation.
///
/// Phase A does not carry per-type alignment metadata, so the heap uses a
/// conservative C-compatible alignment for every object.
pub const DEFAULT_HEAP_ALIGNMENT: usize = 16;
/// Low bits used by runtime immediate values; non-heap candidates are skipped.
pub const IMMEDIATE_TAG_MASK: usize = 0b11;
/// Low-two-bits pattern of every real GC heap pointer candidate.
pub const IMMEDIATE_TAG_HEAP: usize = 0b00;
/// Environment variable that turns on per-collection root diagnostics.
///
/// When set, every root that resolves to a live allocation is logged to stderr
/// with its provenance: an explicit runtime root, a precise stack-map root, or
/// the conservative stack slot address that produced it.
pub const TRACE_ROOTS_ENV: &str = "PON_GC_TRACE_ROOTS";

/// Bytes per small-object span; also the span alignment, so any interior
/// pointer resolves to its span base with a single mask.
const SPAN_BYTES: usize = 64 * 1024;
/// Largest allocation served from spans; bigger blocks are individually
/// system-allocated.
const SMALL_OBJECT_MAX: usize = 1024;
/// Number of 16-byte size classes covering `1..=SMALL_OBJECT_MAX`.
const NUM_SIZE_CLASSES: usize = SMALL_OBJECT_MAX / DEFAULT_HEAP_ALIGNMENT;

/// Where one collection root came from, for [`TRACE_ROOTS_ENV`] diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RootProvenance {
	/// Enumerated by the runtime's [`RootSource`].
	Explicit,
	/// Reported by the installed precise stack-root hook.
	Precise,
	/// Read from `slot` during the conservative external-stack scan.
	StackSlot { slot: usize },
}

thread_local! {
	/// Conservative stack boundary captured for this OS thread.
	static EXTERNAL_STACK_BASE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
	/// Non-zero while this thread is running the collector.
	static COLLECTOR_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Guard marking the current thread as the active collector.
#[must_use]
pub struct CollectorThreadGuard;

impl Drop for CollectorThreadGuard {
	fn drop(&mut self) {
		COLLECTOR_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
	}
}

/// Marks this thread as running collection work.
pub fn enter_collector_thread() -> CollectorThreadGuard {
	COLLECTOR_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
	CollectorThreadGuard
}

/// Returns true while this thread is executing collector/finalizer work under a
/// process-wide stop request.
#[must_use]
pub fn current_thread_is_collecting() -> bool {
	COLLECTOR_DEPTH.with(|depth| depth.get() != 0)
}

/// Precise stack-root enumerator installed by runtimes with stack maps.
///
/// The callback returns `true` only when it handled the whole active native
/// stack precisely.  Returning `false` asks the collector to keep the existing
/// conservative stack scan after accepting any precise roots already reported.
pub type PreciseStackRootFn = unsafe extern "C" fn(visitor: &mut dyn FnMut(*mut u8)) -> bool;

static PRECISE_STACK_ROOTS: AtomicPtr<()> = AtomicPtr::new(ptr::null_mut());

/// Supplies the conservative stack root scanner with the outer stack boundary.
///
/// Runtimes that enter generated code from a native frame should set this to an
/// address in that entry frame before collections can run. A NULL base disables
/// conservative stack scanning, preserving the existing explicit-root-only
/// path.
///
/// The boundary is thread-local: each attached mutator publishes the outer
/// stack frame that encloses its generated-code execution.
pub fn set_external_stack_base(base: *mut u8) {
	EXTERNAL_STACK_BASE.with(|slot| slot.set(base as usize));
}

/// Returns the conservative stack boundary captured by the current thread.
#[must_use]
pub fn external_stack_base() -> *mut u8 {
	EXTERNAL_STACK_BASE.with(|slot| slot.get() as *mut u8)
}

/// C ABI hook for runtimes that cannot call the Rust wrapper directly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gc_set_external_stack_base(base: *mut u8) {
	set_external_stack_base(base);
}

thread_local! {
	/// Lower bound for this thread's conservative stack scan, set around a
	/// collection by the runtime (see [`set_conservative_scan_floor`]).
	static CONSERVATIVE_SCAN_FLOOR: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Raises the conservative scan's lower bound for the current thread.
///
/// The runtime sets this to the generated-code caller's stack pointer when a
/// collection is dispatched through its call helper: every frame below it —
/// the dispatch glue and the collector itself — keeps its GC references alive
/// through explicit roots, while its raw stack memory holds ghosts (stale
/// callee-saved register spills, dead prior-frame residue) that would
/// otherwise be conservatively retained and break `del x; gc.collect()`
/// finalization parity.  A floor outside the `(scan start, stack base)`
/// interval is ignored.  Pass 0 to clear.
pub fn set_conservative_scan_floor(floor: usize) {
	CONSERVATIVE_SCAN_FLOOR.with(|cell| cell.set(floor));
}

/// Returns the current thread's conservative scan floor (0 when unset).
#[must_use]
pub fn conservative_scan_floor() -> usize {
	CONSERVATIVE_SCAN_FLOOR.with(std::cell::Cell::get)
}

/// Installs or clears the precise stack-root hook.
///
/// A hook is optional: with no hook, or when the hook reports an incomplete
/// precise walk, collection falls back to the conservative external-stack scan.
pub fn set_precise_stack_roots(hook: Option<PreciseStackRootFn>) {
	let hook = hook.map_or(ptr::null_mut(), |hook| (hook as *const ()).cast_mut());
	PRECISE_STACK_ROOTS.store(hook, Ordering::SeqCst);
}

/// Returns the currently installed precise stack-root hook, if any.
#[must_use]
pub fn precise_stack_roots() -> Option<PreciseStackRootFn> {
	let hook = PRECISE_STACK_ROOTS.load(Ordering::SeqCst);
	if hook.is_null() {
		return None;
	}

	// SAFETY: `set_precise_stack_roots` stores only function pointers with this
	// exact signature, and null is handled above.
	Some(unsafe { std::mem::transmute::<*mut (), PreciseStackRootFn>(hook) })
}

/// Clears the precise-root hook from C-compatible embedders.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gc_clear_precise_stack_roots() {
	set_precise_stack_roots(None);
}

/// Stable numeric identifier for a registered heap object layout.
///
/// Runtimes register a [`GcTypeInfo`] for each `TypeId` before allocating
/// objects of that type.  The value is intentionally transparent so generated
/// code can pass compact integer type identifiers without depending on Rust
/// internals.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TypeId(
	/// Raw numeric type identifier.
	pub u32,
);

/// Exclusive upper bound for registered [`TypeId`] values.
///
/// Type identifiers index a dense registry table and are stored per object
/// slot as 16-bit values, so they must stay compact; [`Heap::register_type`]
/// rejects anything at or above this bound.
pub const MAX_TYPE_ID: u32 = 1 << 16;

/// Type-specific tracing callback for a heap allocation.
///
/// The callback receives the allocation start and a visitor closure.  It must
/// call `visitor(child)` for every raw pointer field that could reference a
/// managed object.  Reported children may be allocation starts or interiors.
pub type TraceFn = unsafe extern "C" fn(object: *mut u8, visitor: &mut dyn FnMut(*mut u8));

/// Type-specific finalizer callback for a heap allocation.
///
/// The callback receives the allocation start.  The heap calls it at most once
/// for an unreached allocation immediately before releasing that allocation.
pub type FinalizeFn = unsafe extern "C" fn(object: *mut u8);

/// Registered layout and lifecycle hooks for one heap object type.
///
/// The field order and representation are part of the Phase-A GC ABI contract:
/// `size`, `trace`, then `finalize`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GcTypeInfo {
	/// Nominal object size in bytes for this type.
	pub size:     usize,
	/// Traces outgoing managed pointers stored in an object of this type.
	pub trace:    TraceFn,
	/// Optional finalizer run once when an unreachable object is swept.
	pub finalize: Option<FinalizeFn>,
}

/// Compatibility name for the Phase-A object layout contract.
pub type TypeInfo = GcTypeInfo;

/// Source of raw root pointers for a collection cycle.
///
/// Implementors enumerate every pointer-sized value that should keep a managed
/// allocation alive. Roots may point to the start or interior of an allocation.
pub trait RootSource {
	/// Calls `visitor` for every root value currently visible to the runtime.
	fn for_each_root(&mut self, visitor: &mut dyn FnMut(*mut u8));
}

/// Public write-barrier hook used by generated stores into managed objects.
///
/// The barrier is inert until concurrent marking is explicitly enabled; while
/// enabled it records the changed slot and shades the newly stored pointer as a
/// candidate for the concurrent marker.
pub struct WriteBarrier;

impl WriteBarrier {
	/// Records that `slot` now contains `new`.
	pub fn record(slot: *mut *mut u8, new: *mut u8) {
		if new.addr() & IMMEDIATE_TAG_MASK != IMMEDIATE_TAG_HEAP {
			return;
		}

		write_barrier_state().record(slot, new);
	}

	/// Enables write recording and allocation-black behavior for a concurrent
	/// mark cycle.
	pub fn begin_concurrent_marking() {
		write_barrier_state().begin_concurrent_marking();
	}

	/// Disables concurrent write recording.
	pub fn end_concurrent_marking() {
		write_barrier_state().end_concurrent_marking();
	}

	/// Drains recorded slot updates.
	#[must_use]
	pub fn drain_records() -> Vec<WriteBarrierRecord> {
		write_barrier_state().drain_records()
	}

	/// Drains pointers shaded by the barrier.
	#[must_use]
	pub fn drain_shaded() -> Vec<*mut u8> {
		write_barrier_state().drain_shaded()
	}

	/// Returns whether allocation-black is currently active.
	#[must_use]
	pub fn allocation_black_active() -> bool {
		write_barrier_state().allocation_black_active()
	}
}

/// C ABI write-barrier hook for generated code.
///
/// Default builds export an inert hook so callers do not need a second symbol
/// contract for stop-the-world execution.
#[unsafe(no_mangle)]
pub extern "C" fn pon_gc_write_barrier(slot: *mut *mut u8, new: *mut u8) {
	WriteBarrier::record(slot, new);
}

/// A raw slot update captured by the no-GIL write barrier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriteBarrierRecord {
	slot: usize,
	new:  usize,
}

impl WriteBarrierRecord {
	fn new(slot: *mut *mut u8, new: *mut u8) -> Self {
		Self { slot: slot as usize, new: new.addr() }
	}

	/// Returns the slot whose contents changed.
	#[must_use]
	pub const fn slot(self) -> *mut *mut u8 {
		self.slot as *mut *mut u8
	}

	/// Returns the pointer written into the slot.
	#[must_use]
	pub const fn new_value(self) -> *mut u8 {
		self.new as *mut u8
	}
}

#[derive(Debug, Default)]
struct WriteBarrierState {
	concurrent_marking: AtomicBool,
	records:            Mutex<Vec<WriteBarrierRecord>>,
	shaded:             Mutex<Vec<usize>>,
}

impl WriteBarrierState {
	fn begin_concurrent_marking(&self) {
		self.concurrent_marking.store(true, Ordering::Release);
	}

	fn end_concurrent_marking(&self) {
		self.concurrent_marking.store(false, Ordering::Release);
	}

	fn allocation_black_active(&self) -> bool {
		self.concurrent_marking.load(Ordering::Acquire)
	}

	fn record(&self, slot: *mut *mut u8, new: *mut u8) {
		if slot.is_null() || new.is_null() || !self.concurrent_marking.load(Ordering::Acquire) {
			return;
		}

		self
			.records
			.lock()
			.unwrap_or_else(|poison| poison.into_inner())
			.push(WriteBarrierRecord::new(slot, new));
		self
			.shaded
			.lock()
			.unwrap_or_else(|poison| poison.into_inner())
			.push(new.addr());
	}

	fn drain_records(&self) -> Vec<WriteBarrierRecord> {
		std::mem::take(
			&mut *self
				.records
				.lock()
				.unwrap_or_else(|poison| poison.into_inner()),
		)
	}

	fn drain_shaded(&self) -> Vec<*mut u8> {
		std::mem::take(
			&mut *self
				.shaded
				.lock()
				.unwrap_or_else(|poison| poison.into_inner()),
		)
		.into_iter()
		.map(|address| address as *mut u8)
		.collect()
	}
}

static WRITE_BARRIER_STATE: LazyLock<WriteBarrierState> = LazyLock::new(WriteBarrierState::default);

fn write_barrier_state() -> &'static WriteBarrierState {
	&WRITE_BARRIER_STATE
}

/// Non-owning thread-local allocation buffer descriptor for future fast paths.
///
/// The descriptor only tracks bounds; backing memory remains owned by the heap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThreadLocalAllocationBuffer {
	start:  usize,
	cursor: usize,
	limit:  usize,
}

impl ThreadLocalAllocationBuffer {
	/// Returns an empty buffer.
	#[must_use]
	pub const fn empty() -> Self {
		Self { start: 0, cursor: 0, limit: 0 }
	}

	/// Creates a descriptor for the half-open range `[start, limit)`.
	#[must_use]
	pub fn from_bounds(start: *mut u8, limit: *mut u8) -> Self {
		let start = start.addr();
		let limit = limit.addr();
		let cursor = start.min(limit);
		Self { start: cursor, cursor, limit: start.max(limit) }
	}

	/// Returns the number of bytes available in this buffer.
	#[must_use]
	pub const fn remaining_bytes(self) -> usize {
		self.limit.saturating_sub(self.cursor)
	}

	/// Returns whether this descriptor has no allocatable bytes.
	#[must_use]
	pub fn is_empty(self) -> bool {
		self.remaining_bytes() == 0
	}
}

/// Minimal wake flag for a future concurrent GC worker.
#[derive(Debug, Default)]
pub struct ConcurrentWorkerState {
	wake_requested: AtomicBool,
}

impl ConcurrentWorkerState {
	/// Requests that the worker perform a collection step.
	pub fn request_cycle(&self) {
		self.wake_requested.store(true, Ordering::Release);
	}

	/// Consumes one pending worker request.
	#[must_use]
	pub fn take_request(&self) -> bool {
		self.wake_requested.swap(false, Ordering::AcqRel)
	}
}

/// Non-moving stop-the-world heap for managed runtime allocations.
///
/// The heap owns all memory returned by [`Heap::alloc`].  Allocation addresses
/// stay stable until the object is swept or the heap is dropped.
pub struct Heap {
	state:               Mutex<HeapState>,
	/// Bytes allocated since the last collection: the allocation-pressure
	/// "debt" the runtime's automatic-collection trigger reads lock-free.
	bytes_since_collect: AtomicUsize,
	/// Estimated live bytes: alloc adds, sweep subtracts reclaimed blocks.
	live_bytes:          AtomicUsize,
}

impl Heap {
	/// Creates an empty heap.
	#[must_use]
	pub fn new() -> Self {
		Self {
			state:               Mutex::new(HeapState::new()),
			bytes_since_collect: AtomicUsize::new(0),
			live_bytes:          AtomicUsize::new(0),
		}
	}

	/// Registers or replaces layout information for `type_id`.
	///
	/// `type_id.0` must be below [`MAX_TYPE_ID`]: identifiers index a dense
	/// registry table and are stored per object slot as 16-bit values.
	pub fn register_type(&self, type_id: TypeId, info: GcTypeInfo) {
		assert!(
			type_id.0 < MAX_TYPE_ID,
			"GC type id {} exceeds dense registry bound {MAX_TYPE_ID}",
			type_id.0,
		);
		let slot = type_id.0 as usize;
		let mut state = self.lock_state();
		if state.types.len() <= slot {
			state.types.resize(slot + 1, None);
		}
		state.types[slot] = Some(info);
	}

	/// Bytes allocated since the last collection (lock-free).
	///
	/// Consumed by the runtime's allocation-pressure trigger; reset by
	/// [`Heap::collect`].
	#[must_use]
	pub fn allocation_debt(&self) -> usize {
		self.bytes_since_collect.load(Ordering::Relaxed)
	}

	/// Estimated live bytes on the heap (lock-free; allocation adds,
	/// sweeping subtracts reclaimed blocks).
	#[must_use]
	pub fn live_bytes(&self) -> usize {
		self.live_bytes.load(Ordering::Relaxed)
	}

	/// Allocates a zeroed, aligned, non-moving object for `type_id`.
	///
	/// Small requests (at most 1 KiB) are served from size-class span slots;
	/// larger requests get individual system allocations.  The returned
	/// pointer is never null.  Out-of-memory and invalid layout conditions
	/// abort the process rather than returning a sentinel pointer.
	/// `type_id` must already have been registered with [`Heap::register_type`].
	pub fn alloc(&self, size: usize, type_id: TypeId) -> *mut u8 {
		let mut state = self.lock_state();
		assert!(
			state.type_info(type_id).is_some(),
			"cannot allocate unregistered GC type {type_id:?}",
		);

		let allocation_black = write_barrier_state().allocation_black_active();
		let (raw, allocated_size) = if size <= SMALL_OBJECT_MAX {
			state.alloc_small(size, type_id, allocation_black)
		} else {
			state.alloc_large(size, type_id, allocation_black)
		};
		self
			.bytes_since_collect
			.fetch_add(allocated_size, Ordering::Relaxed);
		self.live_bytes.fetch_add(allocated_size, Ordering::Relaxed);

		raw
	}

	/// Performs a full stop-the-world mark/sweep collection.
	///
	/// The collector enumerates roots, resolves root and traced interior
	/// pointers to object starts, traces registered object layouts span by
	/// span, and then finalizes and frees every unreached allocation.
	pub fn collect(&self, roots: &mut dyn RootSource) {
		// Snapshot the pre-collect allocation debt: finalizers running later
		// in this cycle may allocate, and THEIR debt must survive the reset
		// below (only debt this collection actually examined is paid off).
		let debt_at_entry = self.bytes_since_collect.load(Ordering::Relaxed);
		let trace_roots = std::env::var_os(TRACE_ROOTS_ENV).is_some();
		let mut root_values: Vec<*mut u8> = Vec::new();
		// Provenance is recorded only under [`TRACE_ROOTS_ENV`]; the default
		// path keeps the compact root vector and does no extra bookkeeping.
		let mut root_provenance: Vec<RootProvenance> = Vec::new();
		roots.for_each_root(&mut |root| {
			root_values.push(root);
			if trace_roots {
				root_provenance.push(RootProvenance::Explicit);
			}
		});
		if !collect_precise_stack_roots(&mut |root| {
			root_values.push(root);
			if trace_roots {
				root_provenance.push(RootProvenance::Precise);
			}
		}) {
			collect_external_stack_roots(&mut |slot, root| {
				root_values.push(root);
				if trace_roots {
					root_provenance.push(RootProvenance::StackSlot { slot });
				}
			});
		}
		if trace_roots {
			eprintln!(
				"[pon-gc] collect begin: {} roots, external base {:#x}",
				root_values.len(),
				external_stack_base() as usize,
			);
		}
		let mut state = self.lock_state();
		state.prepare_large_index();

		for (index, root) in root_values.into_iter().enumerate() {
			let marked = mark_pointer(&mut state, root);
			if trace_roots
				&& marked
				&& let Some(representative) = state.classify_pointer(root)
			{
				eprintln!(
					"[pon-gc] root {root:p} -> alloc {representative:p} via {:?}",
					root_provenance[index],
				);
			}
		}

		// Objects whose finalizers are RUNNING right now (outer collect
		// frame, lock released) are roots: a nested collection must not
		// free memory an in-flight finalizer is still touching.
		let in_flight = state.pending_finalizer_roots.clone();
		for root in in_flight {
			mark_pointer(&mut state, root.as_ptr());
		}

		drain_marks(&mut state);

		// Finalization-deferred-free: an unreached allocation whose type
		// carries a not-yet-run finalizer is resurrected for THIS cycle as a
		// root wave, so everything its finalizer can touch or store —
		// including the object itself — stays valid while the finalizer runs.
		// The allocation is reclaimed by a later cycle once its finalized bit
		// is set (finalizers run at most once; a resurrected object that dies
		// again is freed without a second callback).  Blocks born since the
		// last collection are age-gated: they may still be referenced by
		// suspended Rust helper frames (heap-backed buffers the conservative
		// scan cannot see) and are neither finalized nor swept this cycle.
		let pending = state.collect_pending_finalizers();
		if !pending.is_empty() {
			for entry in &pending {
				mark_pointer(&mut state, entry.start.as_ptr());
			}
			drain_marks(&mut state);
			for entry in &pending {
				state.set_finalized(entry.location);
			}
			// Root the pending set across the unlocked callback window below.
			state
				.pending_finalizer_roots
				.extend(pending.iter().map(|entry| entry.start));
		}
		let (unreachable, reclaimed) = state.sweep();
		drop(state);
		// Finalizers run OUTSIDE the state lock: Python-level hooks
		// (`__del__`, weakref death callbacks, C `tp_dealloc` bridges)
		// re-enter `Heap::alloc`, which takes the same mutex — running them
		// under the lock self-deadlocks the collecting thread.  Every pending
		// object and its whole subgraph survived the sweep above, and stays
		// rooted through `pending_finalizer_roots` until every callback
		// returns, so a nested collection cannot free memory an in-flight
		// finalizer still touches; the finalized bit suppresses a second
		// callback.
		for entry in &pending {
			// SAFETY: The object survived this cycle's sweep and remains
			// allocated; its finalized bit was set under the lock so the
			// callback runs at most once process-wide.
			unsafe {
				(entry.finalize)(entry.start.as_ptr());
			}
		}
		if !pending.is_empty() {
			let mut state = self.lock_state();
			for entry in &pending {
				if let Some(position) = state
					.pending_finalizer_roots
					.iter()
					.position(|root| *root == entry.start)
				{
					state.pending_finalizer_roots.swap_remove(position);
				}
			}
		}
		for (start, layout) in unreachable {
			// SAFETY: Every detached block was created from `alloc_zeroed`
			// with the same layout and has not yet been deallocated; detached
			// records are owned solely by this frame.
			unsafe {
				dealloc(start.as_ptr(), layout);
			}
		}
		self.live_bytes.fetch_sub(reclaimed, Ordering::Relaxed);
		self
			.bytes_since_collect
			.fetch_sub(debt_at_entry, Ordering::Relaxed);
	}

	fn lock_state(&self) -> MutexGuard<'_, HeapState> {
		self
			.state
			.lock()
			.unwrap_or_else(|poison| poison.into_inner())
	}
}

impl Default for Heap {
	fn default() -> Self {
		Self::new()
	}
}

impl Drop for Heap {
	fn drop(&mut self) {
		let state = self
			.state
			.get_mut()
			.unwrap_or_else(|poison| poison.into_inner());
		for span in state.spans.iter().flatten() {
			// SAFETY: Every span was allocated with `span_layout()` and is
			// deallocated exactly once (here or in sweep, never both).
			unsafe {
				dealloc(span.base as *mut u8, span_layout());
			}
		}
		for large in &state.large {
			// SAFETY: Every large block was allocated with
			// `large_layout(size)` and has not been deallocated.
			unsafe {
				dealloc(large.start.as_ptr(), large_layout(large.size));
			}
		}
	}
}

/// One 64 KiB size-class span and its per-slot metadata.
///
/// Slot `i` occupies `[base + i * elem_size, base + (i + 1) * elem_size)`.
/// Bitmaps follow Go's Green Tea design: `mark_bits` records objects SEEN by
/// the collector, `scan_bits` records objects already SCANNED; their
/// difference is the span's pending work when it is dequeued.
struct Span {
	/// Span start address; `SPAN_BYTES`-aligned.
	base:             usize,
	elem_size:        u32,
	nelems:           u32,
	class:            u8,
	/// Span currently sits on the mark work queue.
	on_queue:         bool,
	/// Number of allocated slots.
	live_count:       u32,
	/// First slot index worth scanning for a free slot (Go `freeindex`).
	free_hint:        u32,
	/// Slots at or above this index have never been allocated and still hold
	/// the span's original zero fill; reused slots below it are re-zeroed at
	/// allocation time.
	zeroed_watermark: u32,
	/// Slot is allocated.
	alloc_bits:       Box<[u64]>,
	/// Slot was seen (marked) this cycle.
	mark_bits:        Box<[u64]>,
	/// Slot was scanned this cycle.
	scan_bits:        Box<[u64]>,
	/// Slot was allocated since the last completed collection (age gate).
	young_bits:       Box<[u64]>,
	/// Slot's finalizer already ran; free silently once unreached again.
	finalized_bits:   Box<[u64]>,
	/// Per-slot registered type id.
	type_ids:         Box<[u16]>,
}

impl Span {
	fn new(class: usize) -> Self {
		let elem_size = class_elem_size(class);
		let nelems = (SPAN_BYTES / elem_size) as u32;
		let words = (nelems as usize).div_ceil(64);
		let layout = span_layout();
		// SAFETY: `layout` is the constant non-zero span layout.
		let raw = unsafe { alloc_zeroed(layout) };
		if raw.is_null() {
			handle_alloc_error(layout);
		}
		Self {
			base: raw.addr(),
			elem_size: elem_size as u32,
			nelems,
			class: class as u8,
			on_queue: false,
			live_count: 0,
			free_hint: 0,
			zeroed_watermark: 0,
			alloc_bits: vec![0u64; words].into_boxed_slice(),
			mark_bits: vec![0u64; words].into_boxed_slice(),
			scan_bits: vec![0u64; words].into_boxed_slice(),
			young_bits: vec![0u64; words].into_boxed_slice(),
			finalized_bits: vec![0u64; words].into_boxed_slice(),
			type_ids: vec![0u16; nelems as usize].into_boxed_slice(),
		}
	}

	/// Finds the lowest free slot at or after `free_hint`, if any.
	fn find_free_slot(&mut self) -> Option<u32> {
		let words = self.alloc_bits.len();
		let mut word_index = (self.free_hint / 64) as usize;
		while word_index < words {
			let mut free = !self.alloc_bits[word_index];
			if word_index == words - 1 {
				let tail = self.nelems % 64;
				if tail != 0 {
					free &= (1u64 << tail) - 1;
				}
			}
			if free != 0 {
				let slot = (word_index as u32) * 64 + free.trailing_zeros();
				self.free_hint = slot;
				return Some(slot);
			}
			word_index += 1;
		}
		None
	}

	const fn slot_address(&self, slot: u32) -> usize {
		self.base + slot as usize * self.elem_size as usize
	}
}

/// A block too large for spans, individually system-allocated.
struct LargeObject {
	start:     NonNull<u8>,
	/// Allocated byte size (the requested size, minimum 1).
	size:      usize,
	type_id:   u16,
	color:     MarkColor,
	/// Allocated since the last completed collection (age gate).
	young:     bool,
	/// Finalizer already ran; free silently once unreached again.
	finalized: bool,
}

/// Where one live object lives, for the finalization pass.
#[derive(Clone, Copy)]
enum ObjectLocation {
	Span { span: u32, slot: u32 },
	Large { index: u32 },
}

/// One unreached object whose finalizer must run this cycle.
struct PendingFinalizer {
	start:    NonNull<u8>,
	finalize: FinalizeFn,
	location: ObjectLocation,
}

struct HeapState {
	/// Dense type registry indexed by `TypeId.0`; see [`Heap::register_type`].
	types:                   Vec<Option<GcTypeInfo>>,
	/// Slot arena of spans; freed entries become `None` and their indices are
	/// recycled, so span indices stay stable across sweeps.
	spans:                   Vec<Option<Span>>,
	span_slots_free:         Vec<u32>,
	/// `(span base, spans index)` sorted by base — the whole pointer
	/// classification index.  Kept sorted incrementally: spans are created
	/// rarely, so a sorted insert is cheap, and classification is always a
	/// clean binary search with NO per-collection rebuild.
	span_base_index:         Vec<(usize, u32)>,
	/// Current allocation target per size class.
	open_span:               [Option<u32>; NUM_SIZE_CLASSES],
	/// Per class: spans with at least one free slot (rebuilt by sweep).
	partial_spans:           Vec<Vec<u32>>,
	large:                   Vec<LargeObject>,
	/// `(start address, large index)` sorted by start; rebuilt lazily —
	/// allocation only flips the dirty flag.
	large_index:             Vec<(usize, u32)>,
	large_index_dirty:       bool,
	/// Green Tea work queue: SPANS with seen-but-unscanned objects, FIFO so
	/// marks accumulate per span before it is scanned.
	span_queue:              VecDeque<u32>,
	large_queue:             VecDeque<u32>,
	/// Reusable gather buffer for one span's pending `(object, type)` pairs.
	scan_buffer:             Vec<(usize, u16)>,
	/// Allocation starts whose finalizers are executing in an outer
	/// `collect` frame with the lock released; rooted by every mark phase.
	pending_finalizer_roots: Vec<NonNull<u8>>,
}

impl HeapState {
	fn new() -> Self {
		Self {
			types:                   Vec::new(),
			spans:                   Vec::new(),
			span_slots_free:         Vec::new(),
			span_base_index:         Vec::new(),
			open_span:               [None; NUM_SIZE_CLASSES],
			partial_spans:           vec![Vec::new(); NUM_SIZE_CLASSES],
			large:                   Vec::new(),
			large_index:             Vec::new(),
			large_index_dirty:       false,
			span_queue:              VecDeque::new(),
			large_queue:             VecDeque::new(),
			scan_buffer:             Vec::new(),
			pending_finalizer_roots: Vec::new(),
		}
	}

	/// Registered layout for `type_id`, if any.
	fn type_info(&self, type_id: TypeId) -> Option<GcTypeInfo> {
		self.types.get(type_id.0 as usize).copied().flatten()
	}

	/// Allocates one slot from the class's spans; the hot allocation path.
	fn alloc_small(
		&mut self,
		size: usize,
		type_id: TypeId,
		allocation_black: bool,
	) -> (*mut u8, usize) {
		let class = size_class_of(size);
		loop {
			let span_index = if let Some(index) = self.open_span[class] {
				index
			} else {
				let index = self.take_allocatable_span(class);
				self.open_span[class] = Some(index);
				index
			};
			let Some(span) = self.spans[span_index as usize].as_mut() else {
				self.open_span[class] = None;
				continue;
			};
			let Some(slot) = span.find_free_slot() else {
				self.open_span[class] = None;
				continue;
			};

			bit_set(&mut span.alloc_bits, slot);
			bit_set(&mut span.young_bits, slot);
			bit_clear(&mut span.finalized_bits, slot);
			if allocation_black {
				// Allocation-black during concurrent marking: born marked
				// AND scanned so the cycle never frees or rescans it.
				bit_set(&mut span.mark_bits, slot);
				bit_set(&mut span.scan_bits, slot);
			}
			span.type_ids[slot as usize] = type_id.0 as u16;
			span.live_count += 1;
			let elem_size = span.elem_size as usize;
			let raw = span.slot_address(slot) as *mut u8;
			if slot < span.zeroed_watermark {
				// SAFETY: The slot lies fully inside this heap-owned span and
				// is free: no live object aliases it.
				unsafe {
					ptr::write_bytes(raw, 0, elem_size);
				}
			} else {
				span.zeroed_watermark = slot + 1;
			}
			return (raw, elem_size);
		}
	}

	/// Pops a span with free capacity for `class`, or creates one.
	fn take_allocatable_span(&mut self, class: usize) -> u32 {
		while let Some(index) = self.partial_spans[class].pop() {
			if let Some(span) = self.spans[index as usize].as_ref()
				&& span.live_count < span.nelems
			{
				return index;
			}
		}
		self.new_span(class)
	}

	fn new_span(&mut self, class: usize) -> u32 {
		let span = Span::new(class);
		let base = span.base;
		let index = if let Some(index) = self.span_slots_free.pop() {
			self.spans[index as usize] = Some(span);
			index
		} else {
			self.spans.push(Some(span));
			(self.spans.len() - 1) as u32
		};
		let position = self
			.span_base_index
			.partition_point(|&(existing, _)| existing < base);
		self.span_base_index.insert(position, (base, index));
		index
	}

	fn alloc_large(
		&mut self,
		size: usize,
		type_id: TypeId,
		allocation_black: bool,
	) -> (*mut u8, usize) {
		let allocated_size = size.max(1);
		let layout = large_layout(allocated_size);
		self
			.large
			.try_reserve(1)
			.unwrap_or_else(|_| std::process::abort());
		// SAFETY: `layout` was constructed above and is non-zero-sized.  A null
		// result is handled with Rust's standard OOM abort path below.
		let raw = unsafe { alloc_zeroed(layout) };
		if raw.is_null() {
			handle_alloc_error(layout);
		}
		// SAFETY: `raw` was just checked for null.
		let start = unsafe { NonNull::new_unchecked(raw) };
		self.large.push(LargeObject {
			start,
			size: allocated_size,
			type_id: type_id.0 as u16,
			color: if allocation_black {
				MarkColor::Black
			} else {
				MarkColor::White
			},
			young: true,
			finalized: false,
		});
		self.large_index_dirty = true;
		(raw, allocated_size)
	}

	/// Rebuilds the sorted large-object address index if allocations changed.
	fn prepare_large_index(&mut self) {
		if !self.large_index_dirty {
			return;
		}
		self.large_index.clear();
		self.large_index.extend(
			self
				.large
				.iter()
				.enumerate()
				.map(|(index, large)| (large.start.as_ptr().addr(), index as u32)),
		);
		self.large_index.sort_unstable();
		self.large_index_dirty = false;
	}

	/// Resolves a pointer (start or interior) to its object start.
	///
	/// Used by [`TRACE_ROOTS_ENV`] diagnostics and tests; the marking path
	/// inlines the same lookups in [`mark_pointer`].
	fn classify_pointer(&mut self, pointer: *mut u8) -> Option<*mut u8> {
		self.prepare_large_index();
		let address = pointer.addr();
		if address == 0 {
			return None;
		}
		let base = address & !(SPAN_BYTES - 1);
		if let Ok(position) = self
			.span_base_index
			.binary_search_by_key(&base, |&(existing, _)| existing)
		{
			let span_index = self.span_base_index[position].1;
			let span = self.spans[span_index as usize].as_ref()?;
			let slot = ((address - base) as u32) / span.elem_size;
			if slot >= span.nelems || !bit_get(&span.alloc_bits, slot) {
				return None;
			}
			return Some(span.slot_address(slot) as *mut u8);
		}
		let position = self
			.large_index
			.partition_point(|&(start, _)| start <= address);
		let &(start, large_index) = self.large_index[..position].last()?;
		let large = self.large.get(large_index as usize)?;
		(address - start < large.size).then_some(large.start.as_ptr())
	}

	/// Gathers every unreached, aged, unfinalized object with a registered
	/// finalizer.  Runs after the mark phase reached fixpoint.
	fn collect_pending_finalizers(&self) -> Vec<PendingFinalizer> {
		let mut pending = Vec::new();
		for (span_index, span) in self.spans.iter().enumerate() {
			let Some(span) = span.as_ref() else {
				continue;
			};
			for word_index in 0..span.alloc_bits.len() {
				let mut dead = span.alloc_bits[word_index]
					& !span.mark_bits[word_index]
					& !span.young_bits[word_index]
					& !span.finalized_bits[word_index];
				while dead != 0 {
					let bit = dead.trailing_zeros();
					dead &= dead - 1;
					let slot = (word_index as u32) * 64 + bit;
					let type_id = TypeId(u32::from(span.type_ids[slot as usize]));
					let Some(finalize) = self.type_info(type_id).and_then(|info| info.finalize) else {
						continue;
					};
					// SAFETY: Slot addresses inside a live span are non-null.
					let start = unsafe { NonNull::new_unchecked(span.slot_address(slot) as *mut u8) };
					pending.push(PendingFinalizer {
						start,
						finalize,
						location: ObjectLocation::Span { span: span_index as u32, slot },
					});
				}
			}
		}
		for (index, large) in self.large.iter().enumerate() {
			if large.color.is_reached() || large.young || large.finalized {
				continue;
			}
			let Some(finalize) = self
				.type_info(TypeId(u32::from(large.type_id)))
				.and_then(|info| info.finalize)
			else {
				continue;
			};
			pending.push(PendingFinalizer {
				start: large.start,
				finalize,
				location: ObjectLocation::Large { index: index as u32 },
			});
		}
		pending
	}

	fn set_finalized(&mut self, location: ObjectLocation) {
		match location {
			ObjectLocation::Span { span, slot } => {
				if let Some(span) = self.spans[span as usize].as_mut() {
					bit_set(&mut span.finalized_bits, slot);
				}
			},
			ObjectLocation::Large { index } => {
				if let Some(large) = self.large.get_mut(index as usize) {
					large.finalized = true;
				}
			},
		}
	}

	/// Frees every unreached, aged object and resets per-cycle mark state.
	///
	/// Returns the detached memory blocks (deallocation is the CALLER's job,
	/// outside the state lock — see [`Heap::collect`]) and the number of
	/// reclaimed bytes.  Finalizers never run on the detached set: an
	/// unreached allocation with a pending finalizer was resurrected for one
	/// cycle before this point.  Whole spans whose last object died are
	/// released too, keeping at most one empty span cached per size class.
	fn sweep(&mut self) -> (Vec<(NonNull<u8>, Layout)>, usize) {
		debug_assert!(self.span_queue.is_empty() && self.large_queue.is_empty());
		let mut detached: Vec<(NonNull<u8>, Layout)> = Vec::new();
		let mut reclaimed = 0usize;

		self.open_span = [None; NUM_SIZE_CLASSES];
		for list in &mut self.partial_spans {
			list.clear();
		}
		let mut kept_empty = [false; NUM_SIZE_CLASSES];

		for span_index in 0..self.spans.len() {
			let Some(span) = self.spans[span_index].as_mut() else {
				continue;
			};
			let mut live = 0u32;
			let mut freed = 0u32;
			for word_index in 0..span.alloc_bits.len() {
				// Survivors: marked objects plus the age-gated young ones
				// (allocated since the last collection; they may still be
				// referenced from Rust helper buffers the conservative scan
				// cannot see).  Go's trick applies: the mark bitmap BECOMES
				// the next cycle's allocation bitmap, so dead slots are
				// implicitly freed for reuse.
				let keep = span.mark_bits[word_index]
					| (span.alloc_bits[word_index] & span.young_bits[word_index]);
				freed += (span.alloc_bits[word_index] & !keep).count_ones();
				live += keep.count_ones();
				span.alloc_bits[word_index] = keep;
				span.mark_bits[word_index] = 0;
				span.scan_bits[word_index] = 0;
				span.young_bits[word_index] = 0;
			}
			reclaimed += freed as usize * span.elem_size as usize;
			span.live_count = live;
			span.free_hint = 0;
			span.on_queue = false;
			let class = span.class as usize;
			let base = span.base;
			let nelems = span.nelems;

			if live == 0 {
				if kept_empty[class] {
					// SAFETY: A live span's base is non-null.
					detached.push((unsafe { NonNull::new_unchecked(base as *mut u8) }, span_layout()));
					if let Ok(position) = self
						.span_base_index
						.binary_search_by_key(&base, |&(existing, _)| existing)
					{
						self.span_base_index.remove(position);
					}
					self.spans[span_index] = None;
					self.span_slots_free.push(span_index as u32);
				} else {
					kept_empty[class] = true;
					self.partial_spans[class].push(span_index as u32);
				}
			} else if live < nelems {
				self.partial_spans[class].push(span_index as u32);
			}
		}

		let old_large = std::mem::take(&mut self.large);
		let mut survivors = Vec::with_capacity(old_large.len());
		for mut large in old_large {
			if large.color.is_reached() || large.young {
				large.color = MarkColor::White;
				large.young = false;
				survivors.push(large);
			} else {
				reclaimed += large.size;
				detached.push((large.start, large_layout(large.size)));
			}
		}
		self.large = survivors;
		self.large_index_dirty = true;

		(detached, reclaimed)
	}

	/// Total allocated objects (spans plus large blocks); test observability.
	#[cfg(test)]
	fn live_object_count(&self) -> usize {
		let span_objects: usize = self
			.spans
			.iter()
			.flatten()
			.map(|span| span.live_count as usize)
			.sum();
		span_objects + self.large.len()
	}

	/// Tri-color state of the object containing `pointer`; test observability.
	#[cfg(test)]
	fn color_of(&mut self, pointer: *mut u8) -> Option<MarkColor> {
		self.prepare_large_index();
		let address = pointer.addr();
		let base = address & !(SPAN_BYTES - 1);
		if let Ok(position) = self
			.span_base_index
			.binary_search_by_key(&base, |&(existing, _)| existing)
		{
			let span_index = self.span_base_index[position].1;
			let span = self.spans[span_index as usize].as_ref()?;
			let slot = ((address - base) as u32) / span.elem_size;
			if slot >= span.nelems || !bit_get(&span.alloc_bits, slot) {
				return None;
			}
			if !bit_get(&span.mark_bits, slot) {
				return Some(MarkColor::White);
			}
			return Some(if bit_get(&span.scan_bits, slot) {
				MarkColor::Black
			} else {
				MarkColor::Gray
			});
		}
		let position = self
			.large_index
			.partition_point(|&(start, _)| start <= address);
		let &(start, large_index) = self.large_index[..position].last()?;
		let large = self.large.get(large_index as usize)?;
		(address - start < large.size).then_some(large.color)
	}
}

/// The size class serving `size` bytes (`size <= SMALL_OBJECT_MAX`).
fn size_class_of(size: usize) -> usize {
	(size.max(1) - 1) / DEFAULT_HEAP_ALIGNMENT
}

/// Slot size in bytes for a size class.
const fn class_elem_size(class: usize) -> usize {
	(class + 1) * DEFAULT_HEAP_ALIGNMENT
}

/// The single span layout: `SPAN_BYTES` at `SPAN_BYTES` alignment, so span
/// bases are recoverable from interior pointers by masking.
fn span_layout() -> Layout {
	Layout::from_size_align(SPAN_BYTES, SPAN_BYTES).unwrap_or_else(|_| std::process::abort())
}

/// The single layout rule for large blocks: `size` bytes (minimum 1) at
/// [`DEFAULT_HEAP_ALIGNMENT`].  Allocation and deallocation MUST both go
/// through this helper.
fn large_layout(size: usize) -> Layout {
	Layout::from_size_align(size.max(1), DEFAULT_HEAP_ALIGNMENT)
		.unwrap_or_else(|_| std::process::abort())
}

fn bit_get(bits: &[u64], index: u32) -> bool {
	bits[(index / 64) as usize] & (1u64 << (index % 64)) != 0
}

fn bit_set(bits: &mut [u64], index: u32) {
	bits[(index / 64) as usize] |= 1u64 << (index % 64);
}

fn bit_clear(bits: &mut [u64], index: u32) {
	bits[(index / 64) as usize] &= !(1u64 << (index % 64));
}

/// Resolves `pointer` (start or interior) and marks its object as seen.
///
/// Returns whether the pointer resolved to a live heap object.  Span objects
/// set their per-span seen bit and enqueue the whole span; large objects shade
/// white-to-gray and enqueue individually.
fn mark_pointer(state: &mut HeapState, pointer: *mut u8) -> bool {
	let address = pointer.addr();
	if address == 0 {
		return false;
	}
	let base = address & !(SPAN_BYTES - 1);
	if let Ok(position) = state
		.span_base_index
		.binary_search_by_key(&base, |&(existing, _)| existing)
	{
		let span_index = state.span_base_index[position].1;
		let Some(span) = state.spans[span_index as usize].as_mut() else {
			return false;
		};
		let slot = ((address - base) as u32) / span.elem_size;
		if slot >= span.nelems || !bit_get(&span.alloc_bits, slot) {
			return false;
		}
		if !bit_get(&span.mark_bits, slot) {
			bit_set(&mut span.mark_bits, slot);
			if !span.on_queue {
				span.on_queue = true;
				state.span_queue.push_back(span_index);
			}
		}
		return true;
	}
	state.prepare_large_index();
	let position = state
		.large_index
		.partition_point(|&(start, _)| start <= address);
	let Some(&(start, large_index)) = state.large_index[..position].last() else {
		return false;
	};
	let Some(large) = state.large.get_mut(large_index as usize) else {
		return false;
	};
	if address - start >= large.size {
		return false;
	}
	if large.color == MarkColor::White {
		large.color = MarkColor::Gray;
		state.large_queue.push_back(large_index);
	}
	true
}

/// Scans queued spans and large objects to fixpoint.
///
/// Green Tea discipline: spans are FIFO so seen bits accumulate while a span
/// waits, and each dequeue scans every pending object in the span in address
/// order.  A span re-enqueues itself when new objects in it are seen after it
/// left the queue — including during its own scan.
fn drain_marks(state: &mut HeapState) {
	let mut scan_buffer = std::mem::take(&mut state.scan_buffer);
	loop {
		if let Some(span_index) = state.span_queue.pop_front() {
			scan_buffer.clear();
			if let Some(span) = state.spans[span_index as usize].as_mut() {
				span.on_queue = false;
				let base = span.base;
				let elem_size = span.elem_size as usize;
				for word_index in 0..span.mark_bits.len() {
					let mut active = span.mark_bits[word_index] & !span.scan_bits[word_index];
					span.scan_bits[word_index] |= active;
					while active != 0 {
						let bit = active.trailing_zeros();
						active &= active - 1;
						let slot = word_index * 64 + bit as usize;
						scan_buffer.push((base + slot * elem_size, span.type_ids[slot]));
					}
				}
			}
			for &(object, type_id) in &scan_buffer {
				let Some(info) = state.type_info(TypeId(u32::from(type_id))) else {
					continue;
				};
				let object = object as *mut u8;
				let mut visitor = |child: *mut u8| {
					mark_pointer(state, child);
				};
				// SAFETY: The object is owned by this heap and remains live
				// for the whole mark phase.  The visitor only records
				// additional candidate pointers in this collector's queues.
				unsafe {
					(info.trace)(object, &mut visitor);
				}
			}
			continue;
		}
		if let Some(large_index) = state.large_queue.pop_front() {
			let large = &mut state.large[large_index as usize];
			if large.color != MarkColor::Gray {
				continue;
			}
			large.color = MarkColor::Black;
			let object = large.start.as_ptr();
			let type_id = TypeId(u32::from(large.type_id));
			if let Some(info) = state.type_info(type_id) {
				let mut visitor = |child: *mut u8| {
					mark_pointer(state, child);
				};
				// SAFETY: As above; the large block stays live for the whole
				// mark phase.
				unsafe {
					(info.trace)(object, &mut visitor);
				}
			}
			continue;
		}
		break;
	}
	state.scan_buffer = scan_buffer;
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkColor {
	/// Object has not been reached in the current mark cycle.
	White = 0,
	/// Object has been reached and awaits scanning.
	Gray  = 1,
	/// Object and its outgoing references have been scanned.
	Black = 2,
}

impl MarkColor {
	const fn from_byte(value: u8) -> Self {
		match value {
			value if value == Self::Gray as u8 => Self::Gray,
			value if value == Self::Black as u8 => Self::Black,
			_ => Self::White,
		}
	}

	/// Whether this color counts as reached in the active mark cycle.
	#[must_use]
	pub const fn is_reached(self) -> bool {
		matches!(self, Self::Gray | Self::Black)
	}
}

/// Atomic tri-color mark word for concurrent marking side tables.
#[derive(Debug)]
pub struct AtomicMarkState {
	color: AtomicU8,
}

impl AtomicMarkState {
	/// Creates a white mark word.
	#[must_use]
	pub const fn white() -> Self {
		Self { color: AtomicU8::new(MarkColor::White as u8) }
	}

	/// Creates a black mark word for allocation-black during concurrent mark.
	#[must_use]
	pub const fn allocated_black() -> Self {
		Self { color: AtomicU8::new(MarkColor::Black as u8) }
	}

	/// Returns the current color.
	#[must_use]
	pub fn color(&self) -> MarkColor {
		MarkColor::from_byte(self.color.load(Ordering::Acquire))
	}

	/// Atomically shades a white object gray.
	///
	/// Returns `true` only for the thread that performed the white-to-gray
	/// transition and therefore owns enqueueing the object for scanning.
	pub fn shade_gray(&self) -> bool {
		self
			.color
			.compare_exchange(
				MarkColor::White as u8,
				MarkColor::Gray as u8,
				Ordering::AcqRel,
				Ordering::Acquire,
			)
			.is_ok()
	}

	/// Marks a scanned object black.
	pub fn finish_scan_black(&self) {
		self.color.store(MarkColor::Black as u8, Ordering::Release);
	}

	/// Resets this mark word for a new cycle.
	pub fn reset_white(&self) {
		self.color.store(MarkColor::White as u8, Ordering::Release);
	}

	/// Returns whether this object is reached in the active cycle.
	#[must_use]
	pub fn is_reached(&self) -> bool {
		matches!(self.color(), MarkColor::Gray | MarkColor::Black)
	}
}

impl Default for AtomicMarkState {
	fn default() -> Self {
		Self::white()
	}
}

fn collect_precise_stack_roots(visitor: &mut dyn FnMut(*mut u8)) -> bool {
	let Some(hook) = precise_stack_roots() else {
		return false;
	};

	// SAFETY: The installed hook owns its stack-walk invariants and may only
	// call back with candidate root values for this stop-the-world collection.
	unsafe { hook(visitor) }
}

fn collect_external_stack_roots(visitor: &mut dyn FnMut(usize, *mut u8)) {
	let base = external_stack_base();
	if base.is_null() {
		return;
	}

	let stack_marker = 0usize;
	let current = ptr::addr_of!(stack_marker).cast::<u8>().cast_mut();
	// SAFETY: `current` and the thread-local external base are both addresses in
	// this thread's stack.
	unsafe { collect_stack_range_roots(current, base, conservative_scan_floor(), visitor) };
}

/// Conservatively scans the pointer-sized words in one stopped thread's stack
/// interval.
///
/// `stack_top` and `stack_base` may be supplied in either order; `floor` raises
/// the low end of the scan when it lies inside the interval.  Registers are
/// deliberately not scanned.  A callee-saved register can hold a dead pointer
/// long after its last semantic use, so generated code mirrors live locals into
/// stack slots that this scan observes.
///
/// # Safety
///
/// The caller must guarantee the interval is a readable stack range for the
/// current thread or for another mutator that has stopped in a published
/// GC-safe region.
pub unsafe fn collect_stack_range_roots(
	stack_top: *mut u8,
	stack_base: *mut u8,
	floor: usize,
	visitor: &mut dyn FnMut(usize, *mut u8),
) {
	if stack_top.is_null() || stack_base.is_null() || stack_top == stack_base {
		return;
	}

	let current = stack_top as usize;
	let base = stack_base as usize;
	let (mut low, high) = if current < base {
		(current, base)
	} else {
		(base, current)
	};
	if floor > low && floor < high {
		low = floor;
	}
	let word = core::mem::size_of::<usize>();
	let align_mask = word - 1;
	let mut slot = (low + align_mask) & !align_mask;
	while slot + word <= high {
		// SAFETY: `slot` lies inside the caller-guaranteed readable interval.
		let candidate = unsafe { ptr::read_unaligned(slot as *const usize) } as *mut u8;
		if !candidate.is_null() {
			visitor(slot, candidate);
		}
		slot += word;
	}
}

#[cfg(test)]
mod tests {
	use std::{
		ptr,
		sync::atomic::{AtomicUsize, Ordering},
	};

	use super::*;

	const TYPE_ID: TypeId = TypeId(1);
	static PRECISE_ROOT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
	static PRECISE_ROOT_A: AtomicUsize = AtomicUsize::new(0);
	static PRECISE_ROOT_B: AtomicUsize = AtomicUsize::new(0);

	static BARRIER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

	fn reset_write_barrier_for_test() -> std::sync::MutexGuard<'static, ()> {
		let guard = BARRIER_TEST_LOCK
			.lock()
			.unwrap_or_else(|poison| poison.into_inner());
		WriteBarrier::end_concurrent_marking();
		let _ = WriteBarrier::drain_records();
		let _ = WriteBarrier::drain_shaded();
		guard
	}

	struct Roots(Vec<*mut u8>);

	impl RootSource for Roots {
		fn for_each_root(&mut self, visitor: &mut dyn FnMut(*mut u8)) {
			for &root in &self.0 {
				visitor(root);
			}
		}
	}

	unsafe extern "C" fn no_trace(_object: *mut u8, _visitor: &mut dyn FnMut(*mut u8)) {}

	fn type_info(size: usize, finalize: Option<FinalizeFn>) -> GcTypeInfo {
		GcTypeInfo { size, trace: no_trace, finalize }
	}

	fn tagged_small_int(value: i64) -> *mut u8 {
		(((value as usize) << 1) | 1) as *mut u8
	}

	fn reserved_immediate(address: usize) -> *mut u8 {
		((address & !IMMEDIATE_TAG_MASK) | 0b10) as *mut u8
	}

	unsafe extern "C" fn precise_roots_hook(visitor: &mut dyn FnMut(*mut u8)) -> bool {
		let first = PRECISE_ROOT_A.load(Ordering::SeqCst) as *mut u8;
		let second = PRECISE_ROOT_B.load(Ordering::SeqCst) as *mut u8;
		if !first.is_null() {
			visitor(first);
		}
		if !second.is_null() {
			visitor(second);
		}
		true
	}

	#[test]
	fn classify_pointer_resolves_small_and_large_allocations() {
		let heap = Heap::new();
		heap.register_type(TYPE_ID, type_info(1024, None));

		let small = heap.alloc(32, TYPE_ID);
		let large = heap.alloc(4096, TYPE_ID);

		let mut state = heap.lock_state();
		assert_eq!(state.classify_pointer(small), Some(small));
		assert_eq!(state.classify_pointer(large), Some(large));
		// SAFETY: Offset 2049 remains within the 4096-byte allocation.
		let large_interior = unsafe { large.add(2049) };
		assert_eq!(state.classify_pointer(large_interior), Some(large));
	}

	#[test]
	fn interior_pointer_resolves_to_allocation_start() {
		let heap = Heap::new();
		heap.register_type(TYPE_ID, type_info(64, None));
		let object = heap.alloc(64, TYPE_ID);

		// SAFETY: Offset 31 remains within the 64-byte allocation.
		let interior = unsafe { object.add(31) };
		let mut state = heap.lock_state();
		assert_eq!(state.classify_pointer(interior), Some(object));
	}

	#[test]
	fn tracing_marks_children_and_blackens_scanned_object() {
		let heap = Heap::new();
		heap.register_type(TYPE_ID, GcTypeInfo {
			size:     std::mem::size_of::<Node>(),
			trace:    trace_node,
			finalize: None,
		});

		let first = heap.alloc(std::mem::size_of::<Node>(), TYPE_ID);
		let second = heap.alloc(std::mem::size_of::<Node>(), TYPE_ID);

		// SAFETY: Both allocations are large enough and aligned for `Node`.
		unsafe {
			ptr::write(first.cast::<Node>(), Node { next: second });
			ptr::write(second.cast::<Node>(), Node { next: ptr::null_mut() });
		}

		let mut state = heap.lock_state();
		assert!(!mark_pointer(&mut state, ptr::null_mut()));
		assert!(mark_pointer(&mut state, first));
		assert_eq!(state.color_of(first), Some(MarkColor::Gray), "seen but not yet scanned");
		assert_eq!(state.color_of(second), Some(MarkColor::White));

		drain_marks(&mut state);

		assert_eq!(state.color_of(first), Some(MarkColor::Black));
		assert_eq!(state.color_of(second), Some(MarkColor::Black), "traced child was scanned too");
	}

	#[test]
	fn allocation_returns_zeroed_aligned_memory() {
		let heap = Heap::new();
		heap.register_type(TYPE_ID, type_info(32, None));

		let object = heap.alloc(32, TYPE_ID);

		assert!(!object.is_null());
		assert_eq!(object.addr() % DEFAULT_HEAP_ALIGNMENT, 0);

		// SAFETY: The allocation above has size 32 and remains live until the
		// heap is dropped at the end of the test.
		let bytes = unsafe { std::slice::from_raw_parts(object, 32) };
		assert!(bytes.iter().all(|&byte| byte == 0));
	}

	#[test]
	fn freed_slot_memory_is_reused_and_zeroed() {
		let heap = Heap::new();
		heap.register_type(TYPE_ID, type_info(32, None));

		let first = heap.alloc(32, TYPE_ID);
		// SAFETY: 32-byte live allocation owned by this heap.
		unsafe {
			first.write_bytes(0xab, 32);
		}
		let first_address = first.addr();

		heap.collect(&mut Roots(Vec::new()));
		heap.collect(&mut Roots(Vec::new()));
		assert_eq!(heap.lock_state().live_object_count(), 0);

		let second = heap.alloc(32, TYPE_ID);
		assert_eq!(second.addr(), first_address, "freed slot is reused");
		// SAFETY: Fresh 32-byte allocation.
		let bytes = unsafe { std::slice::from_raw_parts(second, 32) };
		assert!(bytes.iter().all(|&byte| byte == 0), "reused slot is zeroed");
	}

	#[test]
	fn root_preserves_allocation() {
		static FINALIZED: AtomicUsize = AtomicUsize::new(0);

		unsafe extern "C" fn finalize(_object: *mut u8) {
			FINALIZED.fetch_add(1, Ordering::SeqCst);
		}

		FINALIZED.store(0, Ordering::SeqCst);
		let heap = Heap::new();
		heap.register_type(TYPE_ID, type_info(8, Some(finalize)));
		let object = heap.alloc(8, TYPE_ID);

		heap.collect(&mut Roots(vec![object]));

		assert_eq!(FINALIZED.load(Ordering::SeqCst), 0);

		heap.collect(&mut Roots(Vec::new()));

		assert_eq!(FINALIZED.load(Ordering::SeqCst), 1);
	}

	#[test]
	fn interior_pointer_root_preserves_allocation() {
		static FINALIZED: AtomicUsize = AtomicUsize::new(0);

		unsafe extern "C" fn finalize(_object: *mut u8) {
			FINALIZED.fetch_add(1, Ordering::SeqCst);
		}

		FINALIZED.store(0, Ordering::SeqCst);
		let heap = Heap::new();
		heap.register_type(TYPE_ID, type_info(16, Some(finalize)));
		let object = heap.alloc(16, TYPE_ID);

		// SAFETY: Offset 7 remains within the 16-byte allocation.
		let interior = unsafe { object.add(7) };
		heap.collect(&mut Roots(vec![interior]));

		assert_eq!(FINALIZED.load(Ordering::SeqCst), 0);

		heap.collect(&mut Roots(Vec::new()));

		assert_eq!(FINALIZED.load(Ordering::SeqCst), 1);
	}

	#[test]
	fn finalizer_runs_once_when_allocation_is_swept() {
		static FINALIZED: AtomicUsize = AtomicUsize::new(0);

		unsafe extern "C" fn finalize(_object: *mut u8) {
			FINALIZED.fetch_add(1, Ordering::SeqCst);
		}

		FINALIZED.store(0, Ordering::SeqCst);
		let heap = Heap::new();
		heap.register_type(TYPE_ID, type_info(8, Some(finalize)));
		let _object = heap.alloc(8, TYPE_ID);

		heap.collect(&mut Roots(Vec::new()));
		heap.collect(&mut Roots(Vec::new()));

		assert_eq!(FINALIZED.load(Ordering::SeqCst), 1);
	}

	#[test]
	fn finalized_allocation_survives_its_cycle_and_frees_on_the_next() {
		static FINALIZED: AtomicUsize = AtomicUsize::new(0);

		unsafe extern "C" fn finalize(_object: *mut u8) {
			FINALIZED.fetch_add(1, Ordering::SeqCst);
		}

		FINALIZED.store(0, Ordering::SeqCst);
		let heap = Heap::new();
		heap.register_type(TYPE_ID, type_info(16, Some(finalize)));
		let _object = heap.alloc(16, TYPE_ID);

		heap.collect(&mut Roots(Vec::new()));
		assert_eq!(FINALIZED.load(Ordering::SeqCst), 0, "young garbage is age-gated for one cycle");
		assert_eq!(
			heap.lock_state().live_object_count(),
			1,
			"young garbage survives its birth epoch"
		);

		heap.collect(&mut Roots(Vec::new()));
		assert_eq!(FINALIZED.load(Ordering::SeqCst), 1);
		assert_eq!(
			heap.lock_state().live_object_count(),
			1,
			"finalized object is resurrected for its finalization cycle"
		);

		heap.collect(&mut Roots(Vec::new()));
		assert_eq!(FINALIZED.load(Ordering::SeqCst), 1, "finalizer never re-runs");
		assert_eq!(heap.lock_state().live_object_count(), 0, "freed one cycle later");
	}

	#[test]
	fn finalizer_resurrection_keeps_object_alive_without_refinalize() {
		static FINALIZED: AtomicUsize = AtomicUsize::new(0);
		static RESURRECTED: AtomicUsize = AtomicUsize::new(0);

		unsafe extern "C" fn finalize(object: *mut u8) {
			FINALIZED.fetch_add(1, Ordering::SeqCst);
			// Resurrect: publish the dying object where later root
			// enumerations will see it.
			RESURRECTED.store(object as usize, Ordering::SeqCst);
		}

		FINALIZED.store(0, Ordering::SeqCst);
		RESURRECTED.store(0, Ordering::SeqCst);
		let heap = Heap::new();
		heap.register_type(TYPE_ID, type_info(16, Some(finalize)));
		let _object = heap.alloc(16, TYPE_ID);

		heap.collect(&mut Roots(Vec::new()));
		assert_eq!(FINALIZED.load(Ordering::SeqCst), 0, "young garbage is age-gated for one cycle");
		assert_eq!(
			RESURRECTED.load(Ordering::SeqCst),
			0,
			"finalizer has not run during the birth epoch"
		);
		assert_eq!(heap.lock_state().live_object_count(), 1);

		heap.collect(&mut Roots(Vec::new()));
		assert_eq!(FINALIZED.load(Ordering::SeqCst), 1);
		let resurrected = RESURRECTED.load(Ordering::SeqCst) as *mut u8;
		assert!(!resurrected.is_null());

		// The resurrected object is a live root now: it must survive, and its
		// finalizer must not run again.
		heap.collect(&mut Roots(vec![resurrected]));
		assert_eq!(FINALIZED.load(Ordering::SeqCst), 1);
		assert_eq!(heap.lock_state().live_object_count(), 1);

		// Dropping the last reference frees it silently.
		heap.collect(&mut Roots(Vec::new()));
		assert_eq!(FINALIZED.load(Ordering::SeqCst), 1);
		assert_eq!(heap.lock_state().live_object_count(), 0);
	}

	#[test]
	fn finalizer_sees_valid_children_of_the_dying_object() {
		static OBSERVED: AtomicUsize = AtomicUsize::new(0);

		unsafe extern "C" fn trace_node(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
			// SAFETY: test objects of this type are Node-shaped.
			let next = unsafe { (*object.cast::<Node>()).next };
			if !next.is_null() {
				visitor(next);
			}
		}

		unsafe extern "C" fn finalize(object: *mut u8) {
			// SAFETY: the dying object and everything it references stay
			// valid throughout the finalization cycle.
			let child = unsafe { (*object.cast::<Node>()).next };
			let value = unsafe { child.cast::<usize>().read() };
			OBSERVED.store(value, Ordering::SeqCst);
		}

		const CHILD_TYPE_ID: TypeId = TypeId(2);
		OBSERVED.store(0, Ordering::SeqCst);
		let heap = Heap::new();
		heap.register_type(TYPE_ID, GcTypeInfo {
			size:     std::mem::size_of::<Node>(),
			trace:    trace_node,
			finalize: Some(finalize),
		});
		heap.register_type(CHILD_TYPE_ID, type_info(std::mem::size_of::<usize>(), None));

		let child = heap.alloc(std::mem::size_of::<usize>(), CHILD_TYPE_ID);
		// SAFETY: fresh zeroed allocation of usize size.
		unsafe { child.cast::<usize>().write(0xc0ffee) };
		let parent = heap.alloc(std::mem::size_of::<Node>(), TYPE_ID);
		// SAFETY: fresh zeroed Node-sized allocation.
		unsafe { (*parent.cast::<Node>()).next = child };

		heap.collect(&mut Roots(Vec::new()));
		assert_eq!(
			OBSERVED.load(Ordering::SeqCst),
			0,
			"young garbage is not finalized during its birth epoch"
		);

		heap.collect(&mut Roots(Vec::new()));
		assert_eq!(
			OBSERVED.load(Ordering::SeqCst),
			0xc0ffee,
			"child memory was valid during finalize"
		);
	}

	#[test]
	fn tagged_root_patterns_do_not_retain_or_crash() {
		static FINALIZED: AtomicUsize = AtomicUsize::new(0);

		unsafe extern "C" fn finalize(_object: *mut u8) {
			FINALIZED.fetch_add(1, Ordering::SeqCst);
		}

		FINALIZED.store(0, Ordering::SeqCst);
		let heap = Heap::new();
		heap.register_type(TYPE_ID, type_info(32, Some(finalize)));
		let object = heap.alloc(32, TYPE_ID);

		heap.collect(&mut Roots(vec![tagged_small_int(0), tagged_small_int(-1), 0x2 as *mut u8]));
		assert_eq!(
			FINALIZED.load(Ordering::SeqCst),
			0,
			"tagged non-heap roots must not retain young garbage before the age gate opens"
		);

		heap.collect(&mut Roots(vec![tagged_small_int(0), tagged_small_int(-1), 0x2 as *mut u8]));
		assert_eq!(FINALIZED.load(Ordering::SeqCst), 1);

		let heap = Heap::new();
		heap.register_type(TYPE_ID, type_info(32, None));
		heap.collect(&mut Roots(vec![
			tagged_small_int(0),
			tagged_small_int(-1),
			0x2 as *mut u8,
			(usize::MAX | 1) as *mut u8,
		]));

		let _ = object;
	}

	#[test]
	fn tag_like_interior_root_preserves_allocation_after_classification() {
		static FINALIZED: AtomicUsize = AtomicUsize::new(0);

		unsafe extern "C" fn finalize(_object: *mut u8) {
			FINALIZED.fetch_add(1, Ordering::SeqCst);
		}

		FINALIZED.store(0, Ordering::SeqCst);
		let heap = Heap::new();
		heap.register_type(TYPE_ID, type_info(32, Some(finalize)));
		let object = heap.alloc(32, TYPE_ID);
		let odd_alias = (object.addr() | 1) as *mut u8;
		let reserved_alias = reserved_immediate(object.addr());

		heap.collect(&mut Roots(vec![odd_alias, reserved_alias]));

		assert_eq!(FINALIZED.load(Ordering::SeqCst), 0);
		assert!(heap.lock_state().live_object_count() > 0);
	}

	#[repr(C)]
	struct Node {
		next: *mut u8,
	}

	unsafe extern "C" fn trace_node(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
		// SAFETY: Test allocations for this type are initialized as `Node`.
		let node = unsafe { &*object.cast::<Node>() };
		if !node.next.is_null() {
			visitor(node.next);
		}
	}

	#[repr(C)]
	struct Pair {
		first:  *mut u8,
		second: *mut u8,
	}

	unsafe extern "C" fn trace_pair(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
		// SAFETY: Test allocations for this type are initialized as `Pair`.
		let pair = unsafe { &*object.cast::<Pair>() };
		if !pair.first.is_null() {
			visitor(pair.first);
		}
		if !pair.second.is_null() {
			visitor(pair.second);
		}
	}

	#[test]
	fn tagged_child_field_is_skipped_while_aligned_sibling_survives() {
		let heap = Heap::new();
		heap.register_type(TYPE_ID, GcTypeInfo {
			size:     std::mem::size_of::<Pair>(),
			trace:    trace_pair,
			finalize: None,
		});
		let holder = heap.alloc(std::mem::size_of::<Pair>(), TYPE_ID);
		let sibling = heap.alloc(std::mem::size_of::<Pair>(), TYPE_ID);

		// SAFETY: `holder` is a live `Pair` allocation owned by this heap.
		unsafe {
			ptr::write(holder.cast::<Pair>(), Pair { first: tagged_small_int(42), second: sibling });
		}

		heap.collect(&mut Roots(vec![holder]));

		let mut state = heap.lock_state();
		assert_eq!(state.classify_pointer(holder), Some(holder));
		assert_eq!(state.classify_pointer(sibling), Some(sibling));
	}

	#[test]
	fn precise_roots_accept_aligned_values_and_skip_tagged_values() {
		static FINALIZED: AtomicUsize = AtomicUsize::new(0);

		unsafe extern "C" fn finalize(_object: *mut u8) {
			FINALIZED.fetch_add(1, Ordering::SeqCst);
		}

		let _guard = PRECISE_ROOT_TEST_LOCK
			.lock()
			.unwrap_or_else(|poison| poison.into_inner());
		set_precise_stack_roots(None);
		PRECISE_ROOT_A.store(0, Ordering::SeqCst);
		PRECISE_ROOT_B.store(0, Ordering::SeqCst);

		FINALIZED.store(0, Ordering::SeqCst);
		let heap = Heap::new();
		heap.register_type(TYPE_ID, type_info(16, Some(finalize)));
		let object = heap.alloc(16, TYPE_ID);
		PRECISE_ROOT_A.store(tagged_small_int(5).addr(), Ordering::SeqCst);
		PRECISE_ROOT_B.store(object.addr(), Ordering::SeqCst);
		set_precise_stack_roots(Some(precise_roots_hook));

		heap.collect(&mut Roots(Vec::new()));
		set_precise_stack_roots(None);

		assert_eq!(FINALIZED.load(Ordering::SeqCst), 0);
		heap.collect(&mut Roots(Vec::new()));
		assert_eq!(FINALIZED.load(Ordering::SeqCst), 1);
	}

	#[test]
	fn traced_cycle_is_preserved_while_rooted_then_reclaimed() {
		static FINALIZED: AtomicUsize = AtomicUsize::new(0);

		unsafe extern "C" fn finalize(_object: *mut u8) {
			FINALIZED.fetch_add(1, Ordering::SeqCst);
		}

		FINALIZED.store(0, Ordering::SeqCst);
		let heap = Heap::new();
		heap.register_type(TYPE_ID, GcTypeInfo {
			size:     std::mem::size_of::<Node>(),
			trace:    trace_node,
			finalize: Some(finalize),
		});

		let first = heap.alloc(std::mem::size_of::<Node>(), TYPE_ID);
		let second = heap.alloc(std::mem::size_of::<Node>(), TYPE_ID);

		// SAFETY: Both allocations are large enough and aligned for `Node`.
		unsafe {
			ptr::write(first.cast::<Node>(), Node { next: second });
			ptr::write(second.cast::<Node>(), Node { next: first });
		}

		heap.collect(&mut Roots(vec![first]));

		assert_eq!(FINALIZED.load(Ordering::SeqCst), 0);

		heap.collect(&mut Roots(Vec::new()));

		assert_eq!(FINALIZED.load(Ordering::SeqCst), 2);
	}

	#[test]
	fn write_barrier_atomic_mark_state_shades_white_once() {
		let mark = AtomicMarkState::white();

		assert_eq!(mark.color(), MarkColor::White);
		assert!(!mark.is_reached());
		assert!(mark.shade_gray());
		assert!(!mark.shade_gray());
		assert_eq!(mark.color(), MarkColor::Gray);
		assert!(mark.is_reached());

		mark.finish_scan_black();

		assert_eq!(mark.color(), MarkColor::Black);
		assert!(!mark.shade_gray());

		mark.reset_white();

		assert_eq!(mark.color(), MarkColor::White);
	}

	#[test]
	fn write_barrier_record_is_noop_when_inactive() {
		let _guard = reset_write_barrier_for_test();

		let mut slot = ptr::null_mut();
		let new = NonNull::<u8>::dangling().as_ptr();

		WriteBarrier::record(&mut slot, new);
		WriteBarrier::record(ptr::null_mut(), new);
		pon_gc_write_barrier(&mut slot, new);

		assert!(slot.is_null());

		assert!(WriteBarrier::drain_records().is_empty());
		assert!(WriteBarrier::drain_shaded().is_empty());
		assert!(!WriteBarrier::allocation_black_active());
	}

	#[test]
	fn write_barrier_records_and_shades_during_concurrent_marking() {
		let _guard = reset_write_barrier_for_test();
		let mut slot = ptr::null_mut();
		let new = 0x1000 as *mut u8;

		WriteBarrier::begin_concurrent_marking();
		WriteBarrier::record(&mut slot, new);
		WriteBarrier::record(ptr::null_mut(), new);
		WriteBarrier::record(&mut slot, ptr::null_mut());
		WriteBarrier::end_concurrent_marking();

		assert_eq!(WriteBarrier::drain_records(), vec![WriteBarrierRecord::new(&mut slot, new)],);
		assert_eq!(WriteBarrier::drain_shaded(), vec![new]);
		assert!(WriteBarrier::drain_records().is_empty());
		assert!(WriteBarrier::drain_shaded().is_empty());
	}

	#[test]
	fn write_barrier_skips_tagged_new_values_during_concurrent_marking() {
		let _guard = reset_write_barrier_for_test();
		let mut slot = ptr::null_mut();

		WriteBarrier::begin_concurrent_marking();
		WriteBarrier::record(&mut slot, tagged_small_int(7));
		WriteBarrier::record(&mut slot, reserved_immediate(0x1000));
		pon_gc_write_barrier(&mut slot, tagged_small_int(-1));
		WriteBarrier::end_concurrent_marking();

		assert!(WriteBarrier::drain_records().is_empty());
		assert!(WriteBarrier::drain_shaded().is_empty());
	}

	#[test]
	fn write_barrier_allocation_black_marks_new_objects_during_concurrent_marking() {
		let _guard = reset_write_barrier_for_test();
		let heap = Heap::new();
		heap.register_type(TYPE_ID, type_info(8, None));

		let first = heap.alloc(8, TYPE_ID);
		WriteBarrier::begin_concurrent_marking();
		let second = heap.alloc(8, TYPE_ID);
		WriteBarrier::end_concurrent_marking();

		let mut state = heap.lock_state();
		assert_eq!(state.color_of(first), Some(MarkColor::White));
		assert_eq!(state.color_of(second), Some(MarkColor::Black));
	}
}
