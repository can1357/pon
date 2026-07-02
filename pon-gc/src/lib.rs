#![doc = "Standalone stop-the-world heap for Phase-A runtime objects."]
#![allow(improper_ctypes_definitions)]


pub mod handshake;

pub use handshake::{
    GcHandshake, GcPhase, ack_global_stop, clear_global_stop_request, gc_stop_requested,
    global_handshake, request_global_stop, resume_global_stop,
};

use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::collections::{HashMap, VecDeque};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, Ordering};
use std::sync::{Mutex, MutexGuard};

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


/// Conservative stack boundary plus the thread that captured it.
///
/// The boundary is an address inside the capturing thread's native stack, so a
/// scan range derived from it is only meaningful on that same thread.  Reads
/// from any other thread see NULL, which disables conservative scanning there.
struct ExternalStackBase {
    base: usize,
    owner: Option<std::thread::ThreadId>,
}

static EXTERNAL_STACK_BASE: Mutex<ExternalStackBase> = Mutex::new(ExternalStackBase { base: 0, owner: None });

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
/// conservative stack scanning, preserving the existing explicit-root-only path.
///
/// The boundary is owned by the calling thread: only that thread observes it
/// through [`external_stack_base`], because a range between the current stack
/// pointer and another thread's stack is not a scannable interval.
pub fn set_external_stack_base(base: *mut u8) {
    let mut slot = EXTERNAL_STACK_BASE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    slot.base = base as usize;
    slot.owner = if base.is_null() { None } else { Some(std::thread::current().id()) };
}

/// Returns the conservative stack boundary captured by the current thread.
///
/// Threads other than the one that supplied the boundary observe NULL.
#[must_use]
pub fn external_stack_base() -> *mut u8 {
    let slot = EXTERNAL_STACK_BASE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match slot.owner {
        Some(owner) if owner == std::thread::current().id() => slot.base as *mut u8,
        _ => ptr::null_mut(),
    }
}

/// C ABI hook for runtimes that cannot call the Rust wrapper directly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gc_set_external_stack_base(base: *mut u8) {
    set_external_stack_base(base);
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
/// code can pass compact integer type identifiers without depending on Rust internals.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TypeId(
    /// Raw numeric type identifier.
    pub u32,
);

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
    pub size: usize,
    /// Traces outgoing managed pointers stored in an object of this type.
    pub trace: TraceFn,
    /// Optional finalizer run once when an unreachable object is swept.
    pub finalize: Option<FinalizeFn>,
}

/// Compatibility name for the Phase-A object layout contract.
pub type TypeInfo = GcTypeInfo;

/// Configuration for constructing a standalone stop-the-world [`Heap`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeapConfig {
    /// Largest allocation size considered a small object by future span tiers.
    pub small_obj_max: usize,
    /// Nominal span size reserved for future segregated allocation tiers.
    pub span_size: usize,
}

impl Default for HeapConfig {
    fn default() -> Self {
        Self {
            small_obj_max: 512,
            span_size: 8 * 1024,
        }
    }
}

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
/// The barrier is inert for the default stop-the-world collector.  In
/// free-threaded builds it remains inert until concurrent marking is explicitly
/// enabled; while enabled it records the changed slot and shades the newly
/// stored pointer as a candidate for the concurrent marker.
pub struct WriteBarrier;

impl WriteBarrier {
    /// Records that `slot` now contains `new`.
    pub fn record(slot: *mut *mut u8, new: *mut u8) {
        if new.addr() & IMMEDIATE_TAG_MASK != IMMEDIATE_TAG_HEAP {
            return;
        }

        #[cfg(feature = "free-threading")]
        {
            write_barrier_state().record(slot, new);
        }

        #[cfg(not(feature = "free-threading"))]
        {
            let _ = (slot, new);
        }
    }

    /// Enables write recording and allocation-black behavior for a concurrent
    /// mark cycle.
    #[cfg(feature = "free-threading")]
    pub fn begin_concurrent_marking() {
        write_barrier_state().begin_concurrent_marking();
    }

    /// Disables concurrent write recording.
    #[cfg(feature = "free-threading")]
    pub fn end_concurrent_marking() {
        write_barrier_state().end_concurrent_marking();
    }

    /// Drains recorded slot updates.
    #[cfg(feature = "free-threading")]
    #[must_use]
    pub fn drain_records() -> Vec<WriteBarrierRecord> {
        write_barrier_state().drain_records()
    }

    /// Drains pointers shaded by the barrier.
    #[cfg(feature = "free-threading")]
    #[must_use]
    pub fn drain_shaded() -> Vec<*mut u8> {
        write_barrier_state().drain_shaded()
    }

    /// Returns whether allocation-black is currently active.
    #[cfg(feature = "free-threading")]
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

/// A raw slot update captured by the free-threaded write barrier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriteBarrierRecord {
    slot: usize,
    new: usize,
}

impl WriteBarrierRecord {
    #[cfg(feature = "free-threading")]
    fn new(slot: *mut *mut u8, new: *mut u8) -> Self {
        Self {
            slot: slot as usize,
            new: new.addr(),
        }
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

#[cfg(feature = "free-threading")]
#[derive(Debug, Default)]
struct WriteBarrierState {
    concurrent_marking: AtomicBool,
    records: Mutex<Vec<WriteBarrierRecord>>,
    shaded: Mutex<Vec<usize>>,
}

#[cfg(feature = "free-threading")]
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

        self.records
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(WriteBarrierRecord::new(slot, new));
        self.shaded
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(new.addr());
    }

    fn drain_records(&self) -> Vec<WriteBarrierRecord> {
        std::mem::take(&mut *self.records.lock().unwrap_or_else(|poison| poison.into_inner()))
    }

    fn drain_shaded(&self) -> Vec<*mut u8> {
        std::mem::take(&mut *self.shaded.lock().unwrap_or_else(|poison| poison.into_inner()))
            .into_iter()
            .map(|address| address as *mut u8)
            .collect()
    }
}

#[cfg(feature = "free-threading")]
fn write_barrier_state() -> &'static WriteBarrierState {
    static STATE: std::sync::OnceLock<WriteBarrierState> = std::sync::OnceLock::new();
    STATE.get_or_init(WriteBarrierState::default)
}

/// Non-owning thread-local allocation buffer descriptor for future fast paths.
///
/// The descriptor only tracks bounds; backing memory remains owned by the heap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThreadLocalAllocationBuffer {
    start: usize,
    cursor: usize,
    limit: usize,
}

impl ThreadLocalAllocationBuffer {
    /// Returns an empty buffer.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            start: 0,
            cursor: 0,
            limit: 0,
        }
    }

    /// Creates a descriptor for the half-open range `[start, limit)`.
    #[must_use]
    pub fn from_bounds(start: *mut u8, limit: *mut u8) -> Self {
        let start = start.addr();
        let limit = limit.addr();
        let cursor = start.min(limit);
        Self {
            start: cursor,
            cursor,
            limit: start.max(limit),
        }
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
    state: Mutex<HeapState>,
    _config: HeapConfig,
}

impl Heap {
    /// Creates an empty heap with [`HeapConfig::default`].
    pub fn new() -> Self {
        Self::with_config(HeapConfig::default())
    }

    /// Creates an empty heap with explicit allocation configuration.
    pub fn with_config(config: HeapConfig) -> Self {
        assert!(config.span_size > 0, "heap span size must be non-zero");
        assert!(
            config.small_obj_max <= config.span_size,
            "heap small-object limit must not exceed span size",
        );

        Self {
            state: Mutex::new(HeapState::new(config)),
            _config: config,
        }
    }

    /// Registers or replaces layout information for `type_id`.
    pub fn register_type(&self, type_id: TypeId, info: GcTypeInfo) {
        self.lock_state().types.insert(type_id, info);
    }

    /// Allocates a zeroed, aligned, non-moving object for `type_id`.
    ///
    /// The returned pointer is never null.  Out-of-memory and invalid layout
    /// conditions abort the process rather than returning a sentinel pointer.
    /// `type_id` must already have been registered with [`Heap::register_type`].
    pub fn alloc(&self, size: usize, type_id: TypeId) -> *mut u8 {
        let mut state = self.lock_state();
        assert!(
            state.types.contains_key(&type_id),
            "cannot allocate unregistered GC type {type_id:?}",
        );

        let allocated_size = size.max(1);
        let layout = Layout::from_size_align(allocated_size, DEFAULT_HEAP_ALIGNMENT)
            .unwrap_or_else(|_| std::process::abort());
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
        let allocation_index = state.allocations.len();
        state.allocations.push(Allocation {
            start,
            requested_size: size,
            allocated_size,
            layout,
            type_id,
            classification: AllocationClass::LargeFallback,
        });
        let mark_state = state.new_allocation_mark_state();
        state.mark_states.push(mark_state);
        state.index_allocation(allocation_index);

        raw
    }

    /// Performs a full stop-the-world mark/sweep collection.
    ///
    /// The collector enumerates roots, resolves root and traced interior
    /// pointers to allocation starts, traces registered object layouts, and then
    /// finalizes and frees every unreached allocation.
    pub fn collect(&self, roots: &mut dyn RootSource) {
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

        while let Some(index) = mark_queue.pop() {
            if !state.begin_object_scan(index) {
                continue;
            }

            let Some(allocation) = state.allocations.get(index) else {
                continue;
            };
            let start = allocation.start.as_ptr();
            let type_id = allocation.type_id;
            let Some(info) = state.types.get(&type_id).copied() else {
                state.finish_object_scan(index, SingleObjectScan::new(start));
                continue;
            };

            let mut scan = SingleObjectScan::new(start);
            let mut visitor = |child| {
                if mark_pointer(&mut state, &mut mark_queue, child) {
                    scan.record_hit();
                }
            };

            // SAFETY: The allocation is owned by this heap and remains live for
            // the whole mark phase.  The visitor only records additional
            // candidate pointers in this collector's mark queue.
            unsafe {
                (info.trace)(start, &mut visitor);
            }
            state.finish_object_scan(index, scan);

        }
        state.sweep();
    }

    fn lock_state(&self) -> MutexGuard<'_, HeapState> {
        self.state.lock().unwrap_or_else(|poison| poison.into_inner())
    }
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(|poison| poison.into_inner());
        for allocation in state.allocations.drain(..) {
            // SAFETY: Every allocation record was created from `alloc_zeroed`
            // with the same layout and has not yet been deallocated.
            unsafe {
                dealloc(allocation.start.as_ptr(), allocation.layout);
            }
        }
    }
}

struct HeapState {
    config: HeapConfig,
    types: HashMap<TypeId, GcTypeInfo>,
    allocations: Vec<Allocation>,
    mark_states: Vec<MarkState>,
    spans: Vec<SmallSpan>,
    large_fallbacks: Vec<usize>,
}

impl Default for HeapState {
    fn default() -> Self {
        Self::new(HeapConfig::default())
    }
}

impl HeapState {
    fn new(config: HeapConfig) -> Self {
        Self {
            config,
            types: HashMap::new(),
            allocations: Vec::new(),
            mark_states: Vec::new(),
            spans: Vec::new(),
            large_fallbacks: Vec::new(),
        }
    }

    fn new_allocation_mark_state(&self) -> MarkState {
        #[cfg(feature = "free-threading")]
        {
            if write_barrier_state().allocation_black_active() {
                return MarkState::black();
            }
        }

        MarkState::default()
    }

    fn index_allocation(&mut self, allocation_index: usize) {
        let Some(allocation) = self.allocations.get(allocation_index) else {
            return;
        };

        if allocation.allocated_size <= self.config.small_obj_max {
            let size_class = size_class_for(allocation.allocated_size);
            let span_index = self.span_for_size_class(size_class);
            self.spans[span_index].push(allocation_index);
            self.allocations[allocation_index].classification = AllocationClass::Small {
                span_index,
                size_class,
            };
        } else {
            self.large_fallbacks.push(allocation_index);
            self.allocations[allocation_index].classification = AllocationClass::LargeFallback;
        }
    }

    fn span_for_size_class(&mut self, size_class: usize) -> usize {
        if let Some((index, _)) = self
            .spans
            .iter()
            .enumerate()
            .find(|(_, span)| span.accepts(size_class))
        {
            return index;
        }

        let span_index = self.spans.len();
        self.spans.push(SmallSpan::new(self.config.span_size, size_class));
        span_index
    }


    fn classify_pointer(&self, pointer: *mut u8) -> Option<PointerClassification> {
        if pointer.is_null() {
            return None;
        }

        let address = pointer.addr();
        self.classify_small_pointer(address)
            .or_else(|| self.classify_large_fallback_pointer(address))
    }

    fn classify_small_pointer(&self, address: usize) -> Option<PointerClassification> {
        for span in &self.spans {
            for &index in &span.allocation_indices {
                let allocation = self.allocations.get(index)?;
                if allocation.contains_address(address) {
                    debug_assert!(matches!(
                        allocation.classification,
                        AllocationClass::Small {
                            span_index,
                            size_class,
                        } if self
                            .spans
                            .get(span_index)
                            .is_some_and(|candidate| candidate.size_class == size_class)
                    ));
                    return Some(PointerClassification {
                        index,
                        representative: allocation.start.as_ptr(),
                        route: ClassificationRoute::SmallSpan {
                            span_size: span.span_size,
                            size_class: span.size_class,
                        },
                    });
                }
            }
        }

        None
    }

    fn classify_large_fallback_pointer(&self, address: usize) -> Option<PointerClassification> {
        for &index in &self.large_fallbacks {
            let allocation = self.allocations.get(index)?;
            if allocation.contains_address(address) {
                return Some(PointerClassification {
                    index,
                    representative: allocation.start.as_ptr(),
                    route: ClassificationRoute::LargeFallback,
                });
            }
        }

        None
    }

    fn begin_object_scan(&mut self, index: usize) -> bool {
        let Some(mark_state) = self.mark_states.get_mut(index) else {
            return false;
        };

        if mark_state.color != MarkColor::Gray {
            return false;
        }

        let _previous_scan = mark_state.last_scan.take();
        true
    }

    fn finish_object_scan(&mut self, index: usize, scan: SingleObjectScan) {
        debug_assert!(!scan.representative.is_null());
        let _scan_hit = scan.hit;
        if let Some(mark_state) = self.mark_states.get_mut(index) {
            mark_state.color = MarkColor::Black;
            mark_state.last_scan = Some(scan);
        }
    }

    fn sweep(&mut self) {
        let old_allocations = std::mem::take(&mut self.allocations);
        let old_mark_states = std::mem::take(&mut self.mark_states);
        let mut survivors = Vec::with_capacity(old_allocations.len());
        let mut unreachable = Vec::new();

        self.spans.clear();
        self.large_fallbacks.clear();

        for (index, allocation) in old_allocations.into_iter().enumerate() {
            if old_mark_states
                .get(index)
                .is_some_and(|mark_state| mark_state.is_reached())
            {
                survivors.push(allocation);
            } else {
                unreachable.push(allocation);
            }
        }

        for allocation in &unreachable {
            if let Some(finalize) = self
                .types
                .get(&allocation.type_id)
                .and_then(|info| info.finalize)
            {
                // SAFETY: The object is unreachable and still allocated.  All
                // unreachable allocation storage remains live until the
                // deallocation pass below, so finalizers may safely inspect
                // other unreachable objects they still reference.
                unsafe {
                    finalize(allocation.start.as_ptr());
                }
            }
        }

        for allocation in unreachable {
            // SAFETY: Every allocation record was created from `alloc_zeroed`
            // with the same layout and has not yet been deallocated.
            unsafe {
                dealloc(allocation.start.as_ptr(), allocation.layout);
            }
        }

        self.allocations = survivors;
        self.rebuild_allocation_metadata();
    }

    fn rebuild_allocation_metadata(&mut self) {
        self.mark_states = vec![MarkState::default(); self.allocations.len()];
        self.spans.clear();
        self.large_fallbacks.clear();

        for allocation in &mut self.allocations {
            allocation.classification = AllocationClass::LargeFallback;
        }

        for index in 0..self.allocations.len() {
            self.index_allocation(index);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AllocationClass {
    Small { span_index: usize, size_class: usize },
    LargeFallback,
}

struct Allocation {
    start: NonNull<u8>,
    requested_size: usize,
    allocated_size: usize,
    layout: Layout,
    type_id: TypeId,
    classification: AllocationClass,
}

impl Allocation {
    fn classified_size(&self) -> usize {
        self.requested_size.max(self.allocated_size)
    }

    fn contains_address(&self, address: usize) -> bool {
        let start = self.start.as_ptr().addr();
        let end = start.saturating_add(self.classified_size());
        (start..end).contains(&address)
    }
}

#[derive(Clone, Debug)]
struct SmallSpan {
    span_size: usize,
    size_class: usize,
    used_bytes: usize,
    allocation_indices: Vec<usize>,
}

impl SmallSpan {
    fn new(span_size: usize, size_class: usize) -> Self {
        Self {
            span_size,
            size_class,
            used_bytes: 0,
            allocation_indices: Vec::new(),
        }
    }

    fn accepts(&self, size_class: usize) -> bool {
        self.size_class == size_class && self.used_bytes.saturating_add(size_class) <= self.span_size
    }

    fn push(&mut self, allocation_index: usize) {
        self.used_bytes += self.size_class;
        self.allocation_indices.push(allocation_index);
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkColor {
    /// Object has not been reached in the current mark cycle.
    White = 0,
    /// Object has been reached and awaits scanning.
    Gray = 1,
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
        Self {
            color: AtomicU8::new(MarkColor::White as u8),
        }
    }

    /// Creates a black mark word for allocation-black during concurrent mark.
    #[must_use]
    pub const fn allocated_black() -> Self {
        Self {
            color: AtomicU8::new(MarkColor::Black as u8),
        }
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
        self.color
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

#[derive(Clone, Copy, Debug)]
struct MarkState {
    color: MarkColor,
    last_scan: Option<SingleObjectScan>,
}

impl Default for MarkState {
    fn default() -> Self {
        Self {
            color: MarkColor::White,
            last_scan: None,
        }
    }
}

impl MarkState {
    fn is_reached(&self) -> bool {
        matches!(self.color, MarkColor::Gray | MarkColor::Black)
    }

    #[cfg(feature = "free-threading")]
    fn black() -> Self {
        Self {
            color: MarkColor::Black,
            last_scan: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct SingleObjectScan {
    representative: *mut u8,
    hit: bool,
}

impl SingleObjectScan {
    fn new(representative: *mut u8) -> Self {
        Self {
            representative,
            hit: false,
        }
    }

    fn record_hit(&mut self) {
        self.hit = true;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PointerClassification {
    index: usize,
    representative: *mut u8,
    route: ClassificationRoute,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClassificationRoute {
    SmallSpan { span_size: usize, size_class: usize },
    LargeFallback,
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
    let current = ptr::addr_of!(stack_marker).cast::<u8>() as usize;
    let base = base as usize;
    if current == base {
        return;
    }

    let (low, high) = if current < base { (current, base) } else { (base, current) };
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

fn size_class_for(size: usize) -> usize {
    size.max(1).next_multiple_of(DEFAULT_HEAP_ALIGNMENT)
}

fn is_tagged_non_heap_candidate(pointer: *mut u8) -> bool {
    pointer.addr() & IMMEDIATE_TAG_MASK != IMMEDIATE_TAG_HEAP
}

fn mark_pointer(state: &mut HeapState, mark_queue: &mut MarkQueue, pointer: *mut u8) -> bool {
    let Some(classification) = state.classify_pointer(pointer) else {
        let _is_immediate = is_tagged_non_heap_candidate(pointer);
        return false;
    };
    let PointerClassification {
        index,
        representative,
        route,
    } = classification;
    debug_assert!(!representative.is_null());
    match route {
        ClassificationRoute::SmallSpan {
            span_size,
            size_class,
        } => {
            debug_assert!(span_size > 0);
            debug_assert!(size_class <= span_size);
        }
        ClassificationRoute::LargeFallback => {}
    }

    let Some(mark_state) = state.mark_states.get_mut(index) else {
        return false;
    };

    if mark_state.color == MarkColor::White {
        mark_state.color = MarkColor::Gray;
        mark_queue.push(index);
    }

    true
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const TYPE_ID: TypeId = TypeId(1);
    static PRECISE_ROOT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static PRECISE_ROOT_A: AtomicUsize = AtomicUsize::new(0);
    static PRECISE_ROOT_B: AtomicUsize = AtomicUsize::new(0);


    #[cfg(feature = "free-threading")]
    static BARRIER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(feature = "free-threading")]
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
        GcTypeInfo {
            size,
            trace: no_trace,
            finalize,
        }
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
    fn default_config_uses_phase_a_green_tea_values() {
        assert_eq!(
            HeapConfig::default(),
            HeapConfig {
                small_obj_max: 512,
                span_size: 8 * 1024,
            },
        );
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
    fn allocations_are_classified_as_small_spans_or_large_fallbacks() {
        let heap = Heap::new();
        heap.register_type(TYPE_ID, type_info(1024, None));

        let small = heap.alloc(32, TYPE_ID);
        let large = heap.alloc(HeapConfig::default().small_obj_max + 1, TYPE_ID);

        let state = heap.lock_state();
        assert!(matches!(
            state.allocations[0].classification,
            AllocationClass::Small {
                span_index: 0,
                size_class: 32,
            },
        ));
        assert_eq!(state.spans.len(), 1);
        assert_eq!(state.spans[0].span_size, 8 * 1024);
        assert_eq!(state.spans[0].size_class, 32);
        assert_eq!(state.spans[0].allocation_indices, vec![0]);
        assert!(matches!(
            state.classify_pointer(small).unwrap().route,
            ClassificationRoute::SmallSpan {
                span_size: 8192,
                size_class: 32,
            },
        ));

        assert_eq!(state.allocations[1].classification, AllocationClass::LargeFallback);
        assert_eq!(state.large_fallbacks, vec![1]);
        assert_eq!(
            state.classify_pointer(large).unwrap().route,
            ClassificationRoute::LargeFallback,
        );
    }

    #[test]
    fn interior_pointer_resolution_uses_span_metadata_path() {
        let heap = Heap::new();
        heap.register_type(TYPE_ID, type_info(64, None));
        let object = heap.alloc(64, TYPE_ID);

        // SAFETY: Offset 31 remains within the 64-byte allocation.
        let interior = unsafe { object.add(31) };
        let state = heap.lock_state();
        let classification = state.classify_pointer(interior).unwrap();

        assert_eq!(classification.index, 0);
        assert_eq!(classification.representative, object);
        assert_eq!(
            classification.route,
            ClassificationRoute::SmallSpan {
                span_size: 8 * 1024,
                size_class: 64,
            },
        );
    }

    #[test]
    fn single_object_scan_records_representative_and_child_hit() {
        let heap = Heap::new();
        heap.register_type(
            TYPE_ID,
            GcTypeInfo {
                size: std::mem::size_of::<Node>(),
                trace: trace_node,
                finalize: None,
            },
        );

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
        let info = state.types.get(&TYPE_ID).copied().unwrap();
        let mut scan = SingleObjectScan::new(start);
        let mut visitor = |child| {
            if mark_pointer(&mut state, &mut mark_queue, child) {
                scan.record_hit();
            }
        };

        // SAFETY: `start` identifies the initialized first `Node` allocation.
        unsafe {
            (info.trace)(start, &mut visitor);
        }
        state.finish_object_scan(first_index, scan);

        let recorded = state.mark_states[first_index].last_scan.unwrap();
        assert_eq!(recorded.representative, first);
        assert!(recorded.hit);
        assert_eq!(state.mark_states[first_index].color, MarkColor::Black);
        assert_eq!(mark_queue.pop(), Some(1));
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
        first: *mut u8,
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
        heap.register_type(
            TYPE_ID,
            GcTypeInfo {
                size: std::mem::size_of::<Pair>(),
                trace: trace_pair,
                finalize: None,
            },
        );
        let holder = heap.alloc(std::mem::size_of::<Pair>(), TYPE_ID);
        let sibling = heap.alloc(std::mem::size_of::<Pair>(), TYPE_ID);

        // SAFETY: `holder` is a live `Pair` allocation owned by this heap.
        unsafe {
            ptr::write(
                holder.cast::<Pair>(),
                Pair {
                    first: tagged_small_int(42),
                    second: sibling,
                },
            );
        }

        heap.collect(&mut Roots(vec![holder]));

        let state = heap.lock_state();
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
        heap.register_type(
            TYPE_ID,
            GcTypeInfo {
                size: std::mem::size_of::<Node>(),
                trace: trace_node,
                finalize: Some(finalize),
            },
        );

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
        #[cfg(feature = "free-threading")]
        let _guard = reset_write_barrier_for_test();

        let mut slot = ptr::null_mut();
        let new = NonNull::<u8>::dangling().as_ptr();

        WriteBarrier::record(&mut slot, new);
        WriteBarrier::record(ptr::null_mut(), new);
        pon_gc_write_barrier(&mut slot, new);

        assert!(slot.is_null());

        #[cfg(feature = "free-threading")]
        {
            assert!(WriteBarrier::drain_records().is_empty());
            assert!(WriteBarrier::drain_shaded().is_empty());
            assert!(!WriteBarrier::allocation_black_active());
        }
    }

    #[cfg(feature = "free-threading")]
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

        assert_eq!(
            WriteBarrier::drain_records(),
            vec![WriteBarrierRecord::new(&mut slot, new)],
        );
        assert_eq!(WriteBarrier::drain_shaded(), vec![new]);
        assert!(WriteBarrier::drain_records().is_empty());
        assert!(WriteBarrier::drain_shaded().is_empty());
    }

    #[cfg(feature = "free-threading")]
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

    #[cfg(feature = "free-threading")]
    #[test]
    fn write_barrier_allocation_black_marks_new_objects_during_concurrent_marking() {
        let _guard = reset_write_barrier_for_test();
        let heap = Heap::new();
        heap.register_type(TYPE_ID, type_info(8, None));

        let first = heap.alloc(8, TYPE_ID);
        WriteBarrier::begin_concurrent_marking();
        let second = heap.alloc(8, TYPE_ID);
        WriteBarrier::end_concurrent_marking();

        let state = heap.lock_state();
        assert_eq!(state.classify_pointer(first).unwrap().index, 0);
        assert_eq!(state.mark_states[0].color, MarkColor::White);
        assert_eq!(state.classify_pointer(second).unwrap().index, 1);
        assert_eq!(state.mark_states[1].color, MarkColor::Black);
    }
}
