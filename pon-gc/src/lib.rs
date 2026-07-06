//! Stop-the-world heap for runtime objects.
#![allow(improper_ctypes_definitions)]

pub mod handshake;

use std::{
	alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error},
	collections::VecDeque,
	ptr::{self, NonNull},
	sync::{
		Mutex, MutexGuard,
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
	let hook = hook.map_or(ptr::null_mut(), |hook| hook as *const () as *mut ());
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
/// Type identifiers index a dense registry table, so they must stay compact;
/// [`Heap::register_type`] rejects anything at or above this bound.
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
	pub fn slot(self) -> *mut *mut u8 {
		self.slot as *mut *mut u8
	}

	/// Returns the pointer written into the slot.
	#[must_use]
	pub fn new_value(self) -> *mut u8 {
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

fn write_barrier_state() -> &'static WriteBarrierState {
	static STATE: std::sync::OnceLock<WriteBarrierState> = std::sync::OnceLock::new();
	STATE.get_or_init(WriteBarrierState::default)
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
	pub fn remaining_bytes(self) -> usize {
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
	pub fn new() -> Self {
		Self {
			state:               Mutex::new(HeapState::default()),
			bytes_since_collect: AtomicUsize::new(0),
			live_bytes:          AtomicUsize::new(0),
		}
	}

	/// Registers or replaces layout information for `type_id`.
	///
	/// `type_id.0` must be below [`MAX_TYPE_ID`]: identifiers index a dense
	/// registry table and are compact small integers by ABI contract.
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
	/// The returned pointer is never null.  Out-of-memory and invalid layout
	/// conditions abort the process rather than returning a sentinel pointer.
	/// `type_id` must already have been registered with [`Heap::register_type`].
	pub fn alloc(&self, size: usize, type_id: TypeId) -> *mut u8 {
		let mut state = self.lock_state();
		assert!(
			state.type_info(type_id).is_some(),
			"cannot allocate unregistered GC type {type_id:?}",
		);

		let allocated_size = size.max(1);
		let layout = heap_layout(allocated_size);
		state
			.allocations
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
		let birth_epoch = state.epoch;
		state.allocations.push(Allocation {
			start,
			allocated_size,
			type_id,
			finalized: false,
			birth_epoch,
		});
		let mark_color = state.new_allocation_mark_state();
		state.mark_states.push(mark_color);
		state.addr_index_dirty = true;
		self
			.bytes_since_collect
			.fetch_add(allocated_size, Ordering::Relaxed);
		self.live_bytes.fetch_add(allocated_size, Ordering::Relaxed);

		raw
	}

	/// Performs a full stop-the-world mark/sweep collection.
	///
	/// The collector enumerates roots, resolves root and traced interior
	/// pointers to allocation starts, traces registered object layouts, and then
	/// finalizes and frees every unreached allocation.
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
		state.prepare_address_index();
		let mut mark_queue = MarkQueue::new();

		for (index, root) in root_values.into_iter().enumerate() {
			let marked = mark_pointer(&mut state, &mut mark_queue, root);
			if trace_roots && marked {
				if let Some(classification) = state.classify_pointer(root) {
					eprintln!(
						"[pon-gc] root {root:p} -> alloc {:p} (index {}) via {:?}",
						classification.representative, classification.index, root_provenance[index],
					);
				}
			}
		}

		// Objects whose finalizers are RUNNING right now (outer collect
		// frame, lock released) are roots: a nested collection must not
		// free memory an in-flight finalizer is still touching.
		let in_flight = state.pending_finalizer_roots.clone();
		for root in in_flight {
			mark_pointer(&mut state, &mut mark_queue, root.as_ptr());
		}

		drain_mark_queue(&mut state, &mut mark_queue);

		// Finalization-deferred-free: an unreached allocation whose type
		// carries a not-yet-run finalizer is resurrected for THIS cycle as a
		// root wave, so everything its finalizer can touch or store —
		// including the object itself — stays valid while the finalizer runs.
		// The allocation is reclaimed by a later cycle once `finalized` is
		// set (finalizers run at most once; a resurrected object that dies
		// again is freed without a second callback).
		let mut pending: Vec<(NonNull<u8>, FinalizeFn)> = Vec::new();
		for index in 0..state.allocations.len() {
			if state
				.mark_states
				.get(index)
				.is_some_and(|color| color.is_reached())
			{
				continue;
			}
			let allocation = &state.allocations[index];
			if allocation.finalized {
				continue;
			}
			// Age gate: unreached blocks born in the CURRENT epoch may still
			// be referenced by suspended Rust helper frames (heap-backed
			// buffers the conservative scan cannot see). They are neither
			// finalized nor swept until at least one full epoch old.
			if allocation.birth_epoch >= state.epoch {
				continue;
			}
			let Some(finalize) = state
				.type_info(allocation.type_id)
				.and_then(|info| info.finalize)
			else {
				continue;
			};
			pending.push((allocation.start, finalize));
		}
		if !pending.is_empty() {
			for &(start, _) in &pending {
				let root = start.as_ptr();
				mark_pointer(&mut state, &mut mark_queue, root);
			}
			drain_mark_queue(&mut state, &mut mark_queue);
			for &(start, _) in &pending {
				if let Some(classification) = state.classify_pointer(start.as_ptr()) {
					state.allocations[classification.index].finalized = true;
				}
			}
			// Root the pending set across the unlocked callback window below.
			state
				.pending_finalizer_roots
				.extend(pending.iter().map(|&(start, _)| start));
		}
		let unreachable = state.sweep();
		state.epoch += 1;
		drop(state);
		// Finalizers run OUTSIDE the state lock: Python-level hooks
		// (`__del__`, weakref death callbacks, C `tp_dealloc` bridges)
		// re-enter `Heap::alloc`, which takes the same mutex — running them
		// under the lock self-deadlocks the collecting thread.  Every pending
		// object and its whole subgraph survived the sweep above, and stays
		// rooted through `pending_finalizer_roots` until every callback
		// returns, so a nested collection cannot free memory an in-flight
		// finalizer still touches; `finalized` suppresses a second callback.
		for &(start, finalize) in &pending {
			// SAFETY: The object survived this cycle's sweep and remains
			// allocated; `finalized` was set under the lock so the callback
			// runs at most once process-wide.
			unsafe {
				finalize(start.as_ptr());
			}
		}
		if !pending.is_empty() {
			let mut state = self.lock_state();
			for (start, _) in &pending {
				if let Some(position) = state
					.pending_finalizer_roots
					.iter()
					.position(|root| root == start)
				{
					state.pending_finalizer_roots.swap_remove(position);
				}
			}
		}
		let mut reclaimed = 0usize;
		for allocation in unreachable {
			reclaimed += allocation.layout.size();
			// SAFETY: Every allocation record was created from `alloc_zeroed`
			// with the same layout and has not yet been deallocated; detached
			// records are owned solely by this frame.
			unsafe {
				dealloc(allocation.start.as_ptr(), allocation.layout);
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
		for allocation in state.allocations.drain(..) {
			// SAFETY: Every allocation record was created from `alloc_zeroed`
			// with the same layout and has not yet been deallocated.
			unsafe {
				dealloc(allocation.start.as_ptr(), allocation.layout());
			}
		}
	}
}

#[derive(Default)]
struct HeapState {
	/// Dense type registry indexed by `TypeId.0`; see [`Heap::register_type`].
	types:                   Vec<Option<GcTypeInfo>>,
	allocations:             Vec<Allocation>,
	mark_states:             Vec<MarkColor>,
	/// Allocation starts whose finalizers are executing in an outer
	/// `collect` frame with the lock released; rooted by every mark phase.
	pending_finalizer_roots: Vec<NonNull<u8>>,
	/// Sorted `(start address, allocations index)` pairs backing
	/// [`HeapState::classify_pointer`]'s binary search.
	///
	/// Rebuilt lazily by [`HeapState::prepare_address_index`]: allocation
	/// only flips `addr_index_dirty`, so the hot alloc path carries no
	/// per-object index maintenance (this used to be a per-alloc `BTreeMap`
	/// insert that dominated allocation-heavy workload profiles).
	addr_index:              Vec<(usize, usize)>,
	/// Allocations changed since `addr_index` was last rebuilt.
	addr_index_dirty:        bool,
	/// Completed-collection counter; allocations record it at birth. The
	/// sweep only reclaims unreached blocks born BEFORE the current epoch
	/// (see `Allocation::birth_epoch`).
	epoch:                   u64,
}

impl HeapState {
	fn new_allocation_mark_state(&self) -> MarkColor {
		if write_barrier_state().allocation_black_active() {
			return MarkColor::Black;
		}

		MarkColor::White
	}

	/// Rebuilds the sorted address index after allocations changed.
	///
	/// Classification requires a clean index: [`Heap::collect`] prepares it
	/// once after taking the state lock, and [`mark_pointer`] re-checks
	/// defensively (a no-op branch when already clean).
	fn prepare_address_index(&mut self) {
		if !self.addr_index_dirty {
			return;
		}
		self.addr_index.clear();
		self.addr_index.extend(
			self
				.allocations
				.iter()
				.enumerate()
				.map(|(index, allocation)| (allocation.start.as_ptr().addr(), index)),
		);
		self.addr_index.sort_unstable();
		self.addr_index_dirty = false;
	}

	/// Registered layout for `type_id`, if any.
	fn type_info(&self, type_id: TypeId) -> Option<GcTypeInfo> {
		self.types.get(type_id.0 as usize).copied().flatten()
	}

	fn classify_pointer(&self, pointer: *mut u8) -> Option<PointerClassification> {
		if pointer.is_null() {
			return None;
		}
		// Hard assert even in release: classifying against a stale index
		// silently under-marks and frees live objects.
		assert!(
			!self.addr_index_dirty,
			"classify_pointer called with a stale address index",
		);
		let address = pointer.addr();
		// Address-ordered lookup: allocations are disjoint byte ranges, so
		// the greatest start <= address is the only candidate block. This
		// MUST stay sublinear — every conservative stack word and traced
		// edge classifies, so a linear scan turns collection quadratic.
		let position = self.addr_index.partition_point(|&(start, _)| start <= address);
		let (_, index) = *self.addr_index[..position].last()?;
		let allocation = self.allocations.get(index)?;
		if !allocation.contains_address(address) {
			return None;
		}
		Some(PointerClassification { index, representative: allocation.start.as_ptr() })
	}

	fn begin_object_scan(&self, index: usize) -> bool {
		self.mark_states.get(index) == Some(&MarkColor::Gray)
	}

	fn finish_object_scan(&mut self, index: usize) {
		if let Some(color) = self.mark_states.get_mut(index) {
			*color = MarkColor::Black;
		}
	}

	/// Detaches every unreached allocation from the heap tables and re-indexes
	/// the survivors.  Deallocation is the CALLER's job, outside the state
	/// lock (see [`Heap::collect`]).  Finalizers never run on the detached
	/// set: an unreached allocation with a pending finalizer was resurrected
	/// for one cycle by `collect` before this point.
	fn sweep(&mut self) -> Vec<UnreachableAllocation> {
		let old_allocations = std::mem::take(&mut self.allocations);
		let old_mark_states = std::mem::take(&mut self.mark_states);
		let mut survivors = Vec::with_capacity(old_allocations.len());
		let mut unreachable = Vec::new();

		for (index, allocation) in old_allocations.into_iter().enumerate() {
			let reached = old_mark_states
				.get(index)
				.is_some_and(|color| color.is_reached());
			// Age gate: unreached blocks born in the CURRENT epoch survive
			// one cycle — they may still be referenced from Rust helper
			// buffers the conservative scan cannot see.
			if reached || allocation.birth_epoch >= self.epoch {
				survivors.push(allocation);
			} else {
				unreachable
					.push(UnreachableAllocation { start: allocation.start, layout: allocation.layout() });
			}
		}

		self.allocations = survivors;
		self.mark_states = vec![MarkColor::White; self.allocations.len()];
		self.addr_index_dirty = true;
		unreachable
	}
}

struct Allocation {
	start:          NonNull<u8>,
	/// Allocated byte size: the requested size clamped to a 1-byte minimum.
	/// Alignment is always [`DEFAULT_HEAP_ALIGNMENT`]; the deallocation
	/// layout is reconstructed by [`Allocation::layout`].
	allocated_size: usize,
	type_id:        TypeId,
	/// The type's finalizer already ran for this allocation; it is freed
	/// without a second callback once unreached again.
	finalized:      bool,
	/// Collection epoch this block was allocated in. Unreached allocations
	/// are only RECLAIMED once they are at least one full epoch old:
	/// in-flight temporaries held solely by Rust helper frames (heap-backed
	/// Vecs the conservative stack scan cannot see) are young by
	/// construction and survive the cycle that races them.
	birth_epoch:    u64,
}
/// A detached, unreached allocation awaiting deallocation outside the heap
/// state lock (produced by [`HeapState::sweep`]).
struct UnreachableAllocation {
	start:  NonNull<u8>,
	layout: Layout,
}

impl Allocation {
	fn layout(&self) -> Layout {
		heap_layout(self.allocated_size)
	}

	fn contains_address(&self, address: usize) -> bool {
		let start = self.start.as_ptr().addr();
		let end = start.saturating_add(self.allocated_size);
		(start..end).contains(&address)
	}
}

/// The single layout rule for every heap block: `size` bytes (already
/// clamped to a 1-byte minimum by the caller) at [`DEFAULT_HEAP_ALIGNMENT`].
/// Allocation and deallocation MUST both go through this helper.
fn heap_layout(size: usize) -> Layout {
	Layout::from_size_align(size, DEFAULT_HEAP_ALIGNMENT).unwrap_or_else(|_| std::process::abort())
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PointerClassification {
	index:          usize,
	representative: *mut u8,
}

#[derive(Default)]
struct MarkQueue {
	entries: VecDeque<usize>,
}

impl MarkQueue {
	fn new() -> Self {
		Self::default()
	}

	fn push(&mut self, index: usize) {
		self.entries.push_back(index);
	}

	fn pop(&mut self) -> Option<usize> {
		self.entries.pop_front()
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
/// current thread or for another mutator that has stopped in a published GC-safe
/// region.
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
		let candidate = unsafe { ptr::read_unaligned(slot as *const usize) } as *mut u8;
		if !candidate.is_null() {
			visitor(slot, candidate);
		}
		slot += word;
	}
}

fn is_tagged_non_heap_candidate(pointer: *mut u8) -> bool {
	pointer.addr() & IMMEDIATE_TAG_MASK != IMMEDIATE_TAG_HEAP
}

/// Scans queued gray objects to fixpoint, shading every traced child.
fn drain_mark_queue(state: &mut HeapState, mark_queue: &mut MarkQueue) {
	while let Some(index) = mark_queue.pop() {
		if !state.begin_object_scan(index) {
			continue;
		}

		let Some(allocation) = state.allocations.get(index) else {
			continue;
		};
		let start = allocation.start.as_ptr();
		let type_id = allocation.type_id;
		if let Some(info) = state.type_info(type_id) {
			let mut visitor = |child| {
				mark_pointer(state, mark_queue, child);
			};
			// SAFETY: The allocation is owned by this heap and remains live
			// for the whole mark phase.  The visitor only records additional
			// candidate pointers in this collector's mark queue.
			unsafe {
				(info.trace)(start, &mut visitor);
			}
		}
		state.finish_object_scan(index);
	}
}

fn mark_pointer(state: &mut HeapState, mark_queue: &mut MarkQueue, pointer: *mut u8) -> bool {
	state.prepare_address_index();
	let Some(classification) = state.classify_pointer(pointer) else {
		let _is_immediate = is_tagged_non_heap_candidate(pointer);
		return false;
	};
	let PointerClassification { index, representative } = classification;
	debug_assert!(!representative.is_null());

	let Some(color) = state.mark_states.get_mut(index) else {
		return false;
	};

	if *color == MarkColor::White {
		*color = MarkColor::Gray;
		mark_queue.push(index);
	}

	true
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
	fn mark_queue_pops_fifo() {
		let mut queue = MarkQueue::new();

		queue.push(7);
		queue.push(3);
		queue.push(11);

		assert_eq!(queue.pop(), Some(7));
		assert_eq!(queue.pop(), Some(3));
		assert_eq!(queue.pop(), Some(11));
		assert_eq!(queue.pop(), None);
	}

	#[test]
	fn classify_pointer_resolves_small_and_large_allocations() {
		let heap = Heap::new();
		heap.register_type(TYPE_ID, type_info(1024, None));

		let small = heap.alloc(32, TYPE_ID);
		let large = heap.alloc(4096, TYPE_ID);

		let mut state = heap.lock_state();
		state.prepare_address_index();
		let resolved_small = state.classify_pointer(small).unwrap();
		let resolved_large = state.classify_pointer(large).unwrap();
		assert_eq!(resolved_small.index, 0);
		assert_eq!(resolved_small.representative, small);
		assert_eq!(resolved_large.index, 1);
		assert_eq!(resolved_large.representative, large);
	}

	#[test]
	fn interior_pointer_resolves_to_allocation_start() {
		let heap = Heap::new();
		heap.register_type(TYPE_ID, type_info(64, None));
		let object = heap.alloc(64, TYPE_ID);

		// SAFETY: Offset 31 remains within the 64-byte allocation.
		let interior = unsafe { object.add(31) };
		let mut state = heap.lock_state();
		state.prepare_address_index();
		let classification = state.classify_pointer(interior).unwrap();

		assert_eq!(classification.index, 0);
		assert_eq!(classification.representative, object);
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
		let mut mark_queue = MarkQueue::new();
		assert!(mark_pointer(&mut state, &mut mark_queue, first));
		let first_index = mark_queue.pop().unwrap();
		assert!(state.begin_object_scan(first_index));

		let start = state.allocations[first_index].start.as_ptr();
		let info = state.type_info(TYPE_ID).unwrap();
		let mut visitor = |child| {
			mark_pointer(&mut state, &mut mark_queue, child);
		};

		// SAFETY: `start` identifies the initialized first `Node` allocation.
		unsafe {
			(info.trace)(start, &mut visitor);
		}
		state.finish_object_scan(first_index);

		assert_eq!(state.mark_states[first_index], MarkColor::Black);
		assert_eq!(mark_queue.pop(), Some(1), "traced child was shaded gray and queued");
		assert_eq!(state.mark_states[1], MarkColor::Gray);
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
		assert_eq!(heap.lock_state().allocations.len(), 1, "young garbage survives its birth epoch");

		heap.collect(&mut Roots(Vec::new()));
		assert_eq!(FINALIZED.load(Ordering::SeqCst), 1);
		assert_eq!(
			heap.lock_state().allocations.len(),
			1,
			"finalized object is resurrected for its finalization cycle"
		);

		heap.collect(&mut Roots(Vec::new()));
		assert_eq!(FINALIZED.load(Ordering::SeqCst), 1, "finalizer never re-runs");
		assert!(heap.lock_state().allocations.is_empty(), "freed one cycle later");
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
		assert_eq!(heap.lock_state().allocations.len(), 1);

		heap.collect(&mut Roots(Vec::new()));
		assert_eq!(FINALIZED.load(Ordering::SeqCst), 1);
		let resurrected = RESURRECTED.load(Ordering::SeqCst) as *mut u8;
		assert!(!resurrected.is_null());

		// The resurrected object is a live root now: it must survive, and its
		// finalizer must not run again.
		heap.collect(&mut Roots(vec![resurrected]));
		assert_eq!(FINALIZED.load(Ordering::SeqCst), 1);
		assert_eq!(heap.lock_state().allocations.len(), 1);

		// Dropping the last reference frees it silently.
		heap.collect(&mut Roots(Vec::new()));
		assert_eq!(FINALIZED.load(Ordering::SeqCst), 1);
		assert!(heap.lock_state().allocations.is_empty());
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
		assert!(!heap.lock_state().allocations.is_empty());
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
		state.prepare_address_index();
		assert!(state.classify_pointer(holder).is_some());
		assert!(state.classify_pointer(sibling).is_some());
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
		state.prepare_address_index();
		assert_eq!(state.classify_pointer(first).unwrap().index, 0);
		assert_eq!(state.mark_states[0], MarkColor::White);
		assert_eq!(state.classify_pointer(second).unwrap().index, 1);
		assert_eq!(state.mark_states[1], MarkColor::Black);
	}
}
