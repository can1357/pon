#![doc = "Standalone stop-the-world heap for Phase-A runtime objects."]
#![allow(improper_ctypes_definitions)]


use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::collections::{HashMap, VecDeque};
use std::ptr::NonNull;
use std::sync::{Mutex, MutexGuard};

/// Default byte alignment used for every heap allocation.
///
/// Phase A does not carry per-type alignment metadata, so the heap uses a
/// conservative C-compatible alignment for every object.
pub const DEFAULT_HEAP_ALIGNMENT: usize = 16;

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
/// allocation alive.  Roots may point to the start or interior of an allocation.
pub trait RootSource {
    /// Calls `visitor` for every root value currently visible to the runtime.
    fn for_each_root(&mut self, visitor: &mut dyn FnMut(*mut u8));
}

/// Public write-barrier hook reserved for future incremental collectors.
pub struct WriteBarrier;

impl WriteBarrier {
    /// Records that `slot` now contains `new`.
    ///
    /// Phase A uses a stop-the-world collector, so this hook intentionally does
    /// nothing.  The signature is kept stable for Phase E.
    pub fn record(_slot: *mut *mut u8, _new: *mut u8) {}
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
        state.mark_states.push(MarkState::default());
        state.index_allocation(allocation_index);

        raw
    }

    /// Performs a full stop-the-world mark/sweep collection.
    ///
    /// The collector enumerates roots, resolves root and traced interior
    /// pointers to allocation starts, traces registered object layouts, and then
    /// finalizes and frees every unreached allocation.
    pub fn collect(&self, roots: &mut dyn RootSource) {
        let mut root_values = Vec::new();
        roots.for_each_root(&mut |root| root_values.push(root));

        let mut state = self.lock_state();
        let mut mark_queue = MarkQueue::new();

        for root in root_values {
            mark_pointer(&mut state, &mut mark_queue, root);
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

        self.spans.clear();
        self.large_fallbacks.clear();

        for (index, allocation) in old_allocations.into_iter().enumerate() {
            if old_mark_states
                .get(index)
                .is_some_and(|mark_state| mark_state.is_reached())
            {
                survivors.push(allocation);
                continue;
            }

            if let Some(finalize) = self
                .types
                .get(&allocation.type_id)
                .and_then(|info| info.finalize)
            {
                // SAFETY: The object is unreachable and still allocated; this
                // is the single finalizer call before freeing the allocation.
                unsafe {
                    finalize(allocation.start.as_ptr());
                }
            }

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MarkColor {
    White,
    Gray,
    Black,
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

fn size_class_for(size: usize) -> usize {
    size.max(1).next_multiple_of(DEFAULT_HEAP_ALIGNMENT)
}

fn mark_pointer(state: &mut HeapState, mark_queue: &mut MarkQueue, pointer: *mut u8) -> bool {
    let Some(classification) = state.classify_pointer(pointer) else {
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
    fn write_barrier_record_is_noop() {
        let mut slot = ptr::null_mut();
        let new = NonNull::<u8>::dangling().as_ptr();

        WriteBarrier::record(&mut slot, new);
        WriteBarrier::record(ptr::null_mut(), new);

        assert!(slot.is_null());
    }
}
