//! Precise stack-map registry and frame-pointer walker for Phase-D roots.
//!
//! Tier-1 and typed AoT code can register safepoint maps keyed by return
//! address.  During collection the GC asks this module to walk the native
//! frame-pointer chain; indexed frames report only the pointer slots named by
//! their stack map, while non-indexed frames are surfaced to the caller so it
//! can retain the conservative fallback.

use std::ptr;
use std::sync::{LazyLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// One stack slot containing a managed pointer at a safepoint.
///
/// `fp_offset` is a signed byte offset from the native frame pointer to the
/// slot that stores the root pointer.  The pointed-to value may be an allocation
/// start or an interior pointer; the GC already normalizes both forms.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StackRootSlot {
    pub fp_offset: isize,
}

impl StackRootSlot {
    #[must_use]
    pub const fn new(fp_offset: isize) -> Self {
        Self { fp_offset }
    }
}

/// Precise root map for one generated-code safepoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SafepointMap {
    return_address: usize,
    roots: Vec<StackRootSlot>,
}

impl SafepointMap {
    /// Builds a safepoint map keyed by the return address stored in its frame.
    #[must_use]
    pub fn new(return_address: *const u8, roots: Vec<StackRootSlot>) -> Self {
        Self {
            return_address: return_address.addr(),
            roots,
        }
    }

    /// Return address used to identify this safepoint.
    #[must_use]
    pub fn return_address(&self) -> *const u8 {
        self.return_address as *const u8
    }

    /// Precise pointer slots live at this safepoint.
    #[must_use]
    pub fn roots(&self) -> &[StackRootSlot] {
        &self.roots
    }
}

/// Sorted index of safepoint maps keyed by return address.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StackMapIndex {
    safepoints: Vec<SafepointMap>,
}

impl StackMapIndex {
    /// Builds an index from unsorted maps.  Later duplicate return addresses
    /// replace earlier entries so module re-registration is deterministic.
    #[must_use]
    pub fn new(safepoints: Vec<SafepointMap>) -> Self {
        let mut index = Self::default();
        for map in safepoints {
            index.insert(map);
        }
        index
    }

    /// Number of safepoints stored in the index.
    #[must_use]
    pub fn len(&self) -> usize {
        self.safepoints.len()
    }

    /// Returns true when the index has no safepoints.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.safepoints.is_empty()
    }

    /// Inserts or replaces one safepoint map.
    pub fn insert(&mut self, map: SafepointMap) {
        match self
            .safepoints
            .binary_search_by_key(&map.return_address, |candidate| candidate.return_address)
        {
            Ok(index) => self.safepoints[index] = map,
            Err(index) => self.safepoints.insert(index, map),
        }
    }

    /// Merges another index into this one, replacing duplicate addresses.
    pub fn extend(&mut self, other: StackMapIndex) {
        for map in other.safepoints {
            self.insert(map);
        }
    }

    /// Looks up the stack map for a return address.
    #[must_use]
    pub fn lookup(&self, return_address: *const u8) -> Option<&SafepointMap> {
        let address = return_address.addr();
        self.safepoints
            .binary_search_by_key(&address, |candidate| candidate.return_address)
            .ok()
            .map(|index| &self.safepoints[index])
    }
}

/// C-compatible safepoint descriptor for generated-code registration.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RawSafepointMap {
    pub return_address: *const u8,
    pub roots: *const StackRootSlot,
    pub root_count: usize,
}

/// One frame observed while walking the native frame-pointer chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameInfo {
    pub frame_pointer: *const usize,
    pub return_address: *const u8,
}

/// Counts produced by a precise stack walk.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FrameWalkSummary {
    pub indexed_frames: usize,
    pub fallback_frames: usize,
}

impl FrameWalkSummary {
    #[must_use]
    pub const fn all_frames_indexed(self) -> bool {
        self.fallback_frames == 0
    }
}

static STACK_MAP_INDEX: LazyLock<RwLock<StackMapIndex>> = LazyLock::new(|| RwLock::new(StackMapIndex::default()));
const MAX_FP_CHAIN_FRAMES: usize = 4096;

/// Registers an index globally and installs the GC precise-root hook.
pub fn register_stack_map_index(index: StackMapIndex) {
    write_index().extend(index);
    pon_gc::set_precise_stack_roots(Some(collect_precise_stack_roots));
}

/// Registers one safepoint map globally and installs the GC precise-root hook.
pub fn register_safepoint_map(map: SafepointMap) {
    write_index().insert(map);
    pon_gc::set_precise_stack_roots(Some(collect_precise_stack_roots));
}

/// Looks up a globally registered safepoint by return address.
#[must_use]
pub fn lookup_safepoint_map(return_address: *const u8) -> Option<SafepointMap> {
    read_index().lookup(return_address).cloned()
}

/// Clears all registered stack maps and disables the runtime precise-root hook.
pub fn clear_stack_map_index() {
    *write_index() = StackMapIndex::default();
    pon_gc::set_precise_stack_roots(None);
}

/// Registers raw safepoint maps emitted by generated code.
///
/// Returns `0` on success and `-1` for null or malformed descriptors.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_stackmap_register(maps: *const RawSafepointMap, len: usize) -> i32 {
    if maps.is_null() && len != 0 {
        return -1;
    }

    let raw_maps = if len == 0 {
        &[]
    } else {
        // SAFETY: The caller promises `maps` points at `len` descriptors for the
        // duration of this registration call.
        unsafe { std::slice::from_raw_parts(maps, len) }
    };

    let mut safepoints = Vec::with_capacity(raw_maps.len());
    for raw in raw_maps {
        if raw.return_address.is_null() || (raw.roots.is_null() && raw.root_count != 0) {
            return -1;
        }
        let roots = if raw.root_count == 0 {
            Vec::new()
        } else {
            // SAFETY: The descriptor was validated above and the caller promises
            // the root-slot slice remains readable for this registration call.
            unsafe { std::slice::from_raw_parts(raw.roots, raw.root_count) }.to_vec()
        };
        safepoints.push(SafepointMap::new(raw.return_address, roots));
    }

    register_stack_map_index(StackMapIndex::new(safepoints));
    0
}

/// Walks the current thread's frame-pointer chain using the registered index.
///
/// Indexed frames visit precise root slots.  Non-indexed frames are passed to
/// `fallback`, allowing the caller to retain conservative scanning for those
/// frames or for the whole stack.
pub fn walk_current_fp_chain(
    visitor: &mut dyn FnMut(*mut u8),
    fallback: &mut dyn FnMut(FrameInfo),
) -> FrameWalkSummary {
    let base = pon_gc::external_stack_base();
    if base.is_null() {
        return FrameWalkSummary::default();
    }

    let stack_marker = 0usize;
    let current = ptr::addr_of!(stack_marker).cast::<u8>();
    let bounds = StackBounds::new(current, base);
    let fp = current_frame_pointer();
    if fp.is_null() || bounds.is_empty() {
        return FrameWalkSummary::default();
    }

    // SAFETY: The runtime only installs this hook for code compiled with frame
    // pointers preserved.  The bounds are derived from the same conservative
    // stack-base contract used by the existing collector path.
    unsafe { walk_fp_chain_in_bounds(fp, bounds, visitor, fallback) }
}

/// Walks a frame-pointer chain bounded by `stack_low` and `stack_high`.
///
/// # Safety
///
/// `start_fp` must identify a live native frame record whose caller links remain
/// readable while the walk runs.  The bounds must cover the stack interval that
/// can be read safely.
pub unsafe fn walk_fp_chain(
    start_fp: *const usize,
    stack_low: *const u8,
    stack_high: *const u8,
    visitor: &mut dyn FnMut(*mut u8),
    fallback: &mut dyn FnMut(FrameInfo),
) -> FrameWalkSummary {
    let bounds = StackBounds::new(stack_low, stack_high);
    if start_fp.is_null() || bounds.is_empty() {
        return FrameWalkSummary::default();
    }

    // SAFETY: Forwarded from this function's contract.
    unsafe { walk_fp_chain_in_bounds(start_fp, bounds, visitor, fallback) }
}

unsafe extern "C" fn collect_precise_stack_roots(visitor: &mut dyn FnMut(*mut u8)) -> bool {
    let mut complete = true;
    let summary = walk_current_fp_chain(visitor, &mut |_frame| {
        complete = false;
    });
    complete && summary.indexed_frames != 0 && summary.all_frames_indexed()
}

unsafe fn walk_fp_chain_in_bounds(
    start_fp: *const usize,
    bounds: StackBounds,
    visitor: &mut dyn FnMut(*mut u8),
    fallback: &mut dyn FnMut(FrameInfo),
) -> FrameWalkSummary {
    let mut summary = FrameWalkSummary::default();
    let mut fp = start_fp;
    let grows_down = fp.addr() < bounds.high;
    let index = read_index();
    for _ in 0..MAX_FP_CHAIN_FRAMES {
        if !bounds.contains(fp.cast::<u8>(), 2 * core::mem::size_of::<usize>()) {
            break;
        }

        // SAFETY: The frame record is inside `bounds` and the caller guaranteed
        // the chain is readable while collection is stopped.
        let next_fp = unsafe { ptr::read(fp) } as *const usize;
        // SAFETY: `fp.add(1)` remains inside the checked frame-record range.
        let return_address = unsafe { ptr::read(fp.add(1)) } as *const u8;
        let frame = FrameInfo {
            frame_pointer: fp,
            return_address,
        };

        if let Some(map) = index.lookup(return_address) {
            summary.indexed_frames += 1;
            visit_precise_roots(fp, bounds, map, visitor);
        } else {
            summary.fallback_frames += 1;
            fallback(frame);
        }

        if next_fp.is_null() || !next_frame_pointer_advances(fp, next_fp, bounds, grows_down) {
            break;
        }
        fp = next_fp;
    }

    summary
}

fn visit_precise_roots(
    fp: *const usize,
    bounds: StackBounds,
    map: &SafepointMap,
    visitor: &mut dyn FnMut(*mut u8),
) {
    for root in map.roots() {
        let Some(slot) = offset_pointer(fp.cast::<u8>(), root.fp_offset) else {
            continue;
        };
        if !bounds.contains(slot, core::mem::size_of::<usize>()) {
            continue;
        }
        // SAFETY: The root slot lies within the readable stack bounds.
        let value = unsafe { ptr::read_unaligned(slot.cast::<*mut u8>()) };
        if !value.is_null() {
            visitor(value);
        }
    }
}

fn offset_pointer(base: *const u8, offset: isize) -> Option<*const u8> {
    let address = if offset >= 0 {
        base.addr().checked_add(offset as usize)?
    } else {
        base.addr().checked_sub(offset.unsigned_abs())?
    };
    Some(address as *const u8)
}

fn next_frame_pointer_advances(
    current: *const usize,
    next: *const usize,
    bounds: StackBounds,
    grows_down: bool,
) -> bool {
    if !bounds.contains(next.cast::<u8>(), 2 * core::mem::size_of::<usize>()) {
        return false;
    }

    if grows_down {
        next.addr() > current.addr()
    } else {
        next.addr() < current.addr()
    }
}

#[derive(Clone, Copy, Debug)]
struct StackBounds {
    low: usize,
    high: usize,
}

impl StackBounds {
    fn new(first: *const u8, second: *const u8) -> Self {
        let first = first.addr();
        let second = second.addr();
        let (low, high) = if first <= second { (first, second) } else { (second, first) };
        Self { low, high }
    }

    const fn is_empty(self) -> bool {
        self.low == self.high
    }

    fn contains(self, pointer: *const u8, len: usize) -> bool {
        let start = pointer.addr();
        let Some(end) = start.checked_add(len) else {
            return false;
        };
        start >= self.low && end <= self.high
    }
}

fn read_index() -> RwLockReadGuard<'static, StackMapIndex> {
    STACK_MAP_INDEX.read().unwrap_or_else(|poison| poison.into_inner())
}

fn write_index() -> RwLockWriteGuard<'static, StackMapIndex> {
    STACK_MAP_INDEX.write().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(target_arch = "aarch64")]
fn current_frame_pointer() -> *const usize {
    let fp: *const usize;
    // SAFETY: Reads the architectural frame-pointer register only.
    unsafe {
        core::arch::asm!("mov {}, x29", out(reg) fp, options(nomem, nostack, preserves_flags));
    }
    fp
}

#[cfg(target_arch = "x86_64")]
fn current_frame_pointer() -> *const usize {
    let fp: *const usize;
    // SAFETY: Reads the architectural frame-pointer register only.
    unsafe {
        core::arch::asm!("mov {}, rbp", out(reg) fp, options(nomem, nostack, preserves_flags));
    }
    fp
}

#[cfg(target_arch = "x86")]
fn current_frame_pointer() -> *const usize {
    let fp: *const usize;
    // SAFETY: Reads the architectural frame-pointer register only.
    unsafe {
        core::arch::asm!("mov {}, ebp", out(reg) fp, options(nomem, nostack, preserves_flags));
    }
    fp
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86", target_arch = "x86_64")))]
fn current_frame_pointer() -> *const usize {
    ptr::null()
}
