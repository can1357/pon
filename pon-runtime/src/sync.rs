//! Synchronization primitives for the no-GIL runtime.
//!
//! This module owns object/type critical sections, generated-code write-barrier
//! shims, and the blocking-region/safepoint contract used by the GC handshake.

use core::{
	ops::{Deref, DerefMut},
	ptr,
	sync::atomic::{AtomicUsize, Ordering},
};
use std::cell::RefCell;
use std::{
	collections::HashMap,
	sync::{LazyLock, LockResult, Mutex, MutexGuard, TryLockError, TryLockResult},
};

use crate::{
	object::{PyObject, PyType},
	thread_state::thread_state_lock,
};

/// Runtime mutex used for state shared across Python threads.
///
/// `PonMutex` is a thin documented wrapper around [`std::sync::Mutex`].
/// Poisoning is preserved: callers see the same [`LockResult`] and
/// [`TryLockResult`] contract as the standard library.
#[derive(Debug, Default)]
pub struct PonMutex<T: ?Sized> {
	inner: Mutex<T>,
}

impl<T> PonMutex<T> {
	/// Creates a new runtime mutex containing `value`.
	#[must_use]
	pub const fn new(value: T) -> Self {
		Self { inner: Mutex::new(value) }
	}

	/// Consumes the mutex and returns the protected value.
	///
	/// Poisoning is reported exactly as it is by [`Mutex::into_inner`].
	pub fn into_inner(self) -> LockResult<T> {
		self.inner.into_inner()
	}
}

impl<T: ?Sized> PonMutex<T> {
	/// Locks the mutex, blocking the current thread until it can be acquired.
	///
	pub fn lock(&self) -> LockResult<PonMutexGuard<'_, T>> {
		self
			.inner
			.lock()
			.map(PonMutexGuard::new)
			.map_err(|poison| std::sync::PoisonError::new(PonMutexGuard::new(poison.into_inner())))
	}

	/// Attempts to lock the mutex without blocking.
	pub fn try_lock(&self) -> TryLockResult<PonMutexGuard<'_, T>> {
		self
			.inner
			.try_lock()
			.map(PonMutexGuard::new)
			.map_err(|err| match err {
				TryLockError::Poisoned(poison) => TryLockError::Poisoned(std::sync::PoisonError::new(
					PonMutexGuard::new(poison.into_inner()),
				)),
				TryLockError::WouldBlock => TryLockError::WouldBlock,
			})
	}

	/// Returns a mutable reference to the protected value.
	///
	/// This requires exclusive access to the mutex object and therefore cannot
	/// race with any lock holder.
	pub fn get_mut(&mut self) -> LockResult<&mut T> {
		self.inner.get_mut()
	}
}

/// Guard returned by [`PonMutex::lock`] and [`PonMutex::try_lock`].
///
/// Dropping the guard releases the mutex.  The wrapper keeps the public runtime
/// API independent from the concrete standard-library guard type while still
/// dereferencing to the protected value.
#[derive(Debug)]
pub struct PonMutexGuard<'a, T: ?Sized> {
	inner: MutexGuard<'a, T>,
}

impl<'a, T: ?Sized> PonMutexGuard<'a, T> {
	fn new(inner: MutexGuard<'a, T>) -> Self {
		Self { inner }
	}
}

impl<T: ?Sized> Deref for PonMutexGuard<'_, T> {
	type Target = T;

	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

impl<T: ?Sized> DerefMut for PonMutexGuard<'_, T> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.inner
	}
}

/// Side-table entry for no-GIL type coordination.
///
/// This intentionally lives outside [`PyType`] so enabling future type-level
/// locks or version epochs cannot shift object/type payload offsets.
#[derive(Debug)]
struct TypeThreadingMeta {
	lock:          PonMutex<()>,
	version_epoch: AtomicUsize,
}

impl TypeThreadingMeta {
	fn new() -> Self {
		Self { lock: PonMutex::new(()), version_epoch: AtomicUsize::new(0) }
	}
}

static TYPE_THREADING_META: LazyLock<Mutex<HashMap<usize, &'static TypeThreadingMeta>>> =
	LazyLock::new(|| Mutex::new(HashMap::new()));

fn type_threading_meta(ty: *const PyType) -> &'static TypeThreadingMeta {
	let key = ty as usize;
	let mut table = TYPE_THREADING_META
		.lock()
		.expect("type threading side table should not be poisoned");

	*table
		.entry(key)
		.or_insert_with(|| Box::leak(Box::new(TypeThreadingMeta::new())))
}

/// Locks side-table metadata for `ty` without adding fields to [`PyType`].
///
/// The metadata is created lazily and remains off-object, preserving the object
/// layout while giving mutation paths a stable type-level mutex accessor.
pub fn lock_type(ty: *const PyType) -> LockResult<PonMutexGuard<'static, ()>> {
	type_threading_meta(ty).lock.lock()
}

/// Returns the side-table type version epoch for `ty`.
#[must_use]
pub fn type_version_epoch(ty: *const PyType) -> usize {
	type_threading_meta(ty)
		.version_epoch
		.load(Ordering::Acquire)
}

/// Bumps and returns the side-table type version epoch for `ty`.
pub fn bump_type_version_epoch(ty: *const PyType) -> usize {
	type_threading_meta(ty)
		.version_epoch
		.fetch_add(1, Ordering::AcqRel)
		+ 1
}
// ─── J0.3 §6 note A: subclass registry for transitive IC invalidation ────────
//
// An `AttrIC` guards only the receiver type's version tag, but attribute
// lookup traverses the MRO: mutating `Base.attr` changes lookups on `Derived`
// instances whose caches guard `Derived.version_tag`.  Every published type
// therefore registers with its direct bases at class-creation time, and
// [`type_modified`] bumps the mutated type plus all transitive descendants.
//
// The table holds weak addresses.  Types are currently immortal (leaked
// boxes/statics); if type reclamation ever lands, the collector must clear
// dead entries (J0.3 §3.1 identity-lifetime contract).
static TYPE_SUBCLASSES: LazyLock<Mutex<HashMap<usize, Vec<usize>>>> =
	LazyLock::new(|| Mutex::new(HashMap::new()));

/// Records `derived` as a direct subclass of `base` for transitive
/// version-tag invalidation.  Called by class creation once per direct base;
/// self-registration is ignored.
pub fn register_subclass(base: *const PyType, derived: *const PyType) {
	if base.is_null() || derived.is_null() || core::ptr::eq(base, derived) {
		return;
	}
	let mut table = TYPE_SUBCLASSES
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	let children = table.entry(base as usize).or_default();
	if !children.contains(&(derived as usize)) {
		children.push(derived as usize);
	}
}

/// Direct (declared-base) subclass registry backing `cls.__subclasses__()`.
/// Distinct from [`TYPE_SUBCLASSES`], which records every MRO ancestor for
/// transitive IC invalidation.
static TYPE_DIRECT_SUBCLASSES: LazyLock<Mutex<HashMap<usize, Vec<usize>>>> =
	LazyLock::new(|| Mutex::new(HashMap::new()));

/// Records `derived` as a declared direct subclass of `base`.
pub fn register_direct_subclass(base: *const PyType, derived: *const PyType) {
	if base.is_null() || derived.is_null() || core::ptr::eq(base, derived) {
		return;
	}
	let mut table = TYPE_DIRECT_SUBCLASSES
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	let children = table.entry(base as usize).or_default();
	if !children.contains(&(derived as usize)) {
		children.push(derived as usize);
	}
}

/// Declared direct subclasses of `base` in registration order.
#[must_use]
pub fn direct_subclasses(base: *const PyType) -> Vec<*mut PyType> {
	let table = TYPE_DIRECT_SUBCLASSES
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	table
		.get(&(base as usize))
		.map(|children| {
			children
				.iter()
				.map(|&address| address as *mut PyType)
				.collect()
		})
		.unwrap_or_default()
}

// ─── GC rooting registry for types that own a namespace dictionary ──────────
//
// Type objects are malloc'd (`Box::into_raw`) rather than GC-heap allocations,
// so the collector cannot trace through a type to the GC-managed values stored
// in its `tp_dict` (`PyClassDict`).  Every type that acquires a namespace
// registers here, and `abi::collect` roots each registered type's dict values
// on every collection.  Like the subclass tables above, entries rely on the
// current immortal-types contract (leaked boxes/statics); if type reclamation
// ever lands, the collector must clear dead entries.
static TYPES_WITH_NAMESPACE: LazyLock<Mutex<Vec<usize>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Records `ty` as owning a `PyClassDict` namespace whose values must be
/// treated as GC roots.  Idempotent; NULL is ignored.
pub fn register_namespaced_type(ty: *const PyType) {
	if ty.is_null() {
		return;
	}
	let mut table = TYPES_WITH_NAMESPACE
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	if !table.contains(&(ty as usize)) {
		table.push(ty as usize);
	}
}

/// Snapshot of every namespace-owning type in registration order.
#[must_use]
pub fn namespaced_types() -> Vec<*mut PyType> {
	let table = TYPES_WITH_NAMESPACE
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	table
		.iter()
		.map(|&address| address as *mut PyType)
		.collect()
}

/// Post-publication type mutation hook (J0.3 §6): bumps the in-object
/// `version_tag` of `ty` and every transitive subclass, plus each type's
/// side-table `version_epoch` (the ordered FT counter, step 4 of the §7
/// mutation discipline).
///
/// Callers own mutation atomicity: invoke this AFTER the type-dict/slot write,
/// on the mutating thread (single-threaded today; under the type critical
/// section).
pub fn type_modified(ty: *const PyType) {
	if ty.is_null() {
		return;
	}
	let mut pending = vec![ty as usize];
	let mut seen: Vec<usize> = Vec::new();
	let table = TYPE_SUBCLASSES
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	while let Some(address) = pending.pop() {
		if seen.contains(&address) {
			continue;
		}
		seen.push(address);
		if let Some(children) = table.get(&address) {
			pending.extend(children.iter().copied());
		}
	}
	drop(table);
	for address in seen {
		let ty = address as *const PyType;
		// SAFETY: Registered types are immortal for the process lifetime; the
		// registry only ever holds addresses of published `PyType` objects.
		unsafe { (*ty).bump_version() };
		bump_type_version_epoch(ty);
	}
}

/// Marks the current thread as parked in a GC-safe blocking region.
///
/// Call this immediately before a native wait that may block without executing
/// generated-code safepoints (poll/select, condition-variable waits, sleeps,
/// joins, blocking syscalls).  The function publishes a stack pointer inside
/// the caller's frame and increments a nesting counter in the current
/// `PonThreadState`.  While the counter is non-zero, a collector treats the
/// thread as stopped and scans the published `[stack_top_fp, stack_base]` span.
pub fn enter_blocking_region() {
	let mut marker = 0usize;
	let mut state = thread_state_lock();
	state.enter_gc_safe_region((&mut marker as *mut usize).cast::<u8>());
}

/// Leaves a blocking region previously entered by this thread.
///
/// The call is nesting-safe.  If a collector requested a stop while the native
/// wait was finishing, this function immediately runs the safepoint slow path
/// before returning to Python execution.
pub fn leave_blocking_region() -> Result<(), &'static str> {
	let result = {
		let mut state = thread_state_lock();
		state.leave_gc_safe_region()
	};
	if result.is_ok() {
		safepoint_poll();
	}
	result
}

/// RAII helper for native waits with a lexical blocking scope.
#[must_use]
pub struct BlockingRegionGuard {
	active: bool,
}

impl BlockingRegionGuard {
	/// Enters a blocking region and returns a guard that leaves on drop.
	pub fn enter() -> Self {
		enter_blocking_region();
		Self { active: true }
	}

	/// Leaves now and reports unmatched-leave errors to the caller.
	pub fn leave(&mut self) -> Result<(), &'static str> {
		if !self.active {
			return Ok(());
		}
		self.active = false;
		leave_blocking_region()
	}
}

impl Drop for BlockingRegionGuard {
	fn drop(&mut self) {
		let _ = self.leave();
	}
}

/// Generated-code safepoint body shared by JIT and AoT.
///
/// Returns `0` when execution may continue and `-1` when a pending Python
/// signal handler raised an exception.  The GC handshake is still honored before
/// returning a signal error so collectors are never left waiting on a mutator
/// that happened to notice a signal first.
pub fn safepoint_poll() -> i32 {
	let mut status = 0;
	if crate::native::signal::has_pending_signals()
		&& unsafe { crate::native::signal::process_pending_signals() }.is_err()
	{
		status = -1;
	}
	if pon_gc::gc_stop_requested() && !pon_gc::current_thread_is_collecting() {
		let mut marker = 0usize;
		{
			let mut state = thread_state_lock();
			state.enter_gc_safe_region((&mut marker as *mut usize).cast::<u8>());
		}
		pon_gc::ack_global_stop();
		pon_gc::wait_for_global_resume();
		let _ = {
			let mut state = thread_state_lock();
			state.leave_gc_safe_region()
		};
	}
	status
}

/// AoT-visible generated-code safepoint helper.
#[unsafe(no_mangle)]
pub extern "C" fn pon_safepoint_poll() -> i32 {
	safepoint_poll()
}

const GLOBAL_CRITICAL_SECTION_KEY: usize = 0;

static GLOBAL_CRITICAL_SECTION_LOCK: PonMutex<()> = PonMutex::new(());

static OBJECT_CRITICAL_SECTION_LOCKS: LazyLock<Mutex<HashMap<usize, &'static PonMutex<()>>>> =
	LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug)]
struct HeldCriticalLock {
	key:   usize,
	owns:  bool,
	guard: Option<PonMutexGuard<'static, ()>>,
}

#[derive(Debug, Default)]
struct CriticalSectionFrame {
	locks:     Vec<HeldCriticalLock>,
	suspended: Vec<usize>,
}

thread_local! {
	 static CRITICAL_SECTION_STACK: RefCell<Vec<CriticalSectionFrame>> = const { RefCell::new(Vec::new()) };
}

/// Guard for runtime object critical sections.
///
/// Locks per-object side-table mutexes keyed by object address.  Nested
/// sections that would acquire a new mutex suspend
/// the current thread's active critical-section guards, acquire the requested
/// mutexes in deterministic address order, and restore the suspended guards
/// when the inner section ends.  This mirrors CPython-style suspend-on-reentry
/// while preserving the existing object layout.
#[derive(Debug)]
pub struct CriticalSectionGuard {
	active: bool,
}

impl CriticalSectionGuard {
	fn enter(object: *mut PyObject) -> Self {
		push_critical_section(ordered_keys(&[critical_section_key(object)]));
		Self { active: true }
	}

	fn enter2(left: *mut PyObject, right: *mut PyObject) -> Self {
		push_critical_section(ordered_keys(&[
			critical_section_key(left),
			critical_section_key(right),
		]));
		Self { active: true }
	}

	fn leave(&mut self) {
		if self.active {
			pop_critical_section();
			self.active = false;
		}
	}
}

impl Drop for CriticalSectionGuard {
	fn drop(&mut self) {
		self.leave();
	}
}

fn critical_section_key(object: *mut PyObject) -> usize {
	object as usize
}

fn ordered_keys(keys: &[usize]) -> Vec<usize> {
	let mut keys: Vec<usize> = keys.iter().copied().collect();
	keys.sort_unstable();
	keys.dedup();
	keys
}

fn critical_section_lock(key: usize) -> &'static PonMutex<()> {
	if key == GLOBAL_CRITICAL_SECTION_KEY {
		return &GLOBAL_CRITICAL_SECTION_LOCK;
	}

	let mut table = OBJECT_CRITICAL_SECTION_LOCKS
		.lock()
		.expect("object critical-section side table should not be poisoned");
	*table
		.entry(key)
		.or_insert_with(|| Box::leak(Box::new(PonMutex::new(()))))
}

fn active_lock_keys(stack: &[CriticalSectionFrame]) -> Vec<usize> {
	let mut keys = Vec::new();
	for frame in stack {
		for lock in &frame.locks {
			if lock.owns && lock.guard.is_some() {
				keys.push(lock.key);
			}
		}
	}
	keys.sort_unstable();
	keys.dedup();
	keys
}

fn suspend_active_locks(stack: &mut [CriticalSectionFrame]) -> Vec<usize> {
	let mut keys = Vec::new();
	for frame in stack {
		for lock in &mut frame.locks {
			if lock.owns && lock.guard.is_some() {
				lock.guard.take();
				keys.push(lock.key);
			}
		}
	}
	keys.sort_unstable();
	keys.dedup();
	keys
}

fn restore_suspended_locks(stack: &mut [CriticalSectionFrame], keys: Vec<usize>) {
	for key in keys {
		let guard = critical_section_lock(key)
			.lock()
			.expect("object critical-section mutex should not be poisoned");
		let mut guard = Some(guard);
		for frame in stack.iter_mut() {
			if let Some(lock) = frame
				.locks
				.iter_mut()
				.find(|lock| lock.key == key && lock.owns && lock.guard.is_none())
			{
				lock.guard = guard.take();
				break;
			}
		}
		debug_assert!(guard.is_none(), "suspended critical-section lock had no owner");
	}
}

fn push_critical_section(keys: Vec<usize>) {
	CRITICAL_SECTION_STACK.with(|stack| {
		let mut stack = stack.borrow_mut();
		let active_keys = active_lock_keys(&stack);
		let all_requested_locks_are_already_active = keys
			.iter()
			.all(|key| active_keys.binary_search(key).is_ok());
		let suspended = if stack.is_empty() || all_requested_locks_are_already_active {
			Vec::new()
		} else {
			suspend_active_locks(&mut stack)
		};

		let mut locks = Vec::with_capacity(keys.len());
		for key in keys {
			if suspended.is_empty() && active_keys.binary_search(&key).is_ok() {
				locks.push(HeldCriticalLock { key, owns: false, guard: None });
			} else {
				let guard = critical_section_lock(key)
					.lock()
					.expect("object critical-section mutex should not be poisoned");
				locks.push(HeldCriticalLock { key, owns: true, guard: Some(guard) });
			}
		}
		stack.push(CriticalSectionFrame { locks, suspended });
	});
}

fn pop_critical_section() {
	CRITICAL_SECTION_STACK.with(|stack| {
		let mut stack = stack.borrow_mut();
		let frame = stack
			.pop()
			.expect("critical-section end without matching begin");
		let CriticalSectionFrame { locks, suspended } = frame;
		drop(locks);
		restore_suspended_locks(&mut stack, suspended);
	});
}

/// Enters a one-object runtime critical section and returns an RAII guard.
#[must_use]
pub fn begin_critical_section(object: *mut PyObject) -> CriticalSectionGuard {
	CriticalSectionGuard::enter(object)
}

/// Enters a two-object runtime critical section in deterministic address order.
#[must_use]
pub fn begin_critical_section2(left: *mut PyObject, right: *mut PyObject) -> CriticalSectionGuard {
	CriticalSectionGuard::enter2(left, right)
}

/// Enters the legacy process-wide runtime critical section and returns a guard.
///
/// This is modeled as the global side-table key.
#[must_use]
pub fn enter_critical_section() -> CriticalSectionGuard {
	CriticalSectionGuard::enter(ptr::null_mut())
}

/// Returns whether this thread currently has an active runtime critical
/// section.
#[must_use]
pub fn critical_section_is_held() -> bool {
	CRITICAL_SECTION_STACK.with(|stack| !stack.borrow().is_empty())
}

/// Records a heap pointer write for future incremental collectors.
#[inline]
pub unsafe fn record_gc_write_barrier(slot: *mut *mut PyObject, value: *mut PyObject) {
	pon_gc::WriteBarrier::record(slot.cast::<*mut u8>(), value.cast::<u8>());
}

/// Stores a heap object pointer after routing it through the write barrier.
///
/// # Safety
///
/// `slot` must be valid for writes of one `*mut PyObject`.
#[inline]
pub unsafe fn store_heap_pointer(slot: *mut *mut PyObject, value: *mut PyObject) {
	unsafe { record_gc_write_barrier(slot, value) };
	unsafe {
		*slot = value;
	}
}
/// Acquires the legacy global critical section for generated-code callers.
#[unsafe(no_mangle)]
pub extern "C" fn pon_runtime_enter_critical_section() {
	push_critical_section(ordered_keys(&[GLOBAL_CRITICAL_SECTION_KEY]));
}

/// Releases the legacy global critical section for generated-code callers.
#[unsafe(no_mangle)]
pub extern "C" fn pon_runtime_leave_critical_section() {
	pop_critical_section();
}

/// Acquires a one-object critical section for generated-code callers.
#[unsafe(no_mangle)]
pub extern "C" fn pon_runtime_begin_critical_section(object: *mut PyObject) {
	push_critical_section(ordered_keys(&[critical_section_key(object)]));
}

/// Releases the most recent one-object critical section for generated-code
/// callers.
#[unsafe(no_mangle)]
pub extern "C" fn pon_runtime_end_critical_section() {
	pop_critical_section();
}

/// Acquires a two-object critical section for generated-code callers.
#[unsafe(no_mangle)]
pub extern "C" fn pon_runtime_begin_critical_section2(left: *mut PyObject, right: *mut PyObject) {
	push_critical_section(ordered_keys(&[critical_section_key(left), critical_section_key(right)]));
}

/// Releases the most recent two-object critical section for generated-code
/// callers.
#[unsafe(no_mangle)]
pub extern "C" fn pon_runtime_end_critical_section2() {
	pop_critical_section();
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn pon_mutex_lock_allows_mutating_protected_value() {
		let mutex = PonMutex::new(41);

		{
			let mut guard = mutex.lock().expect("mutex should not be poisoned");
			*guard += 1;
		}

		assert_eq!(*mutex.lock().expect("mutex should not be poisoned"), 42);
	}

	#[test]
	fn pon_mutex_try_lock_reports_would_block_while_guard_is_held() {
		let mutex = PonMutex::new(0);
		let _guard = mutex.lock().expect("mutex should not be poisoned");

		match mutex.try_lock() {
			Err(TryLockError::WouldBlock) => {},
			other => panic!("expected WouldBlock while lock is held, got {other:?}"),
		}
	}

	#[test]
	fn pon_mutex_get_mut_and_into_inner_return_protected_value() {
		let mut mutex = PonMutex::new(vec![1]);

		mutex
			.get_mut()
			.expect("mutex should not be poisoned")
			.push(2);

		assert_eq!(mutex.into_inner().expect("mutex should not be poisoned"), vec![1, 2]);
	}

	#[test]
	fn critical_section_held_state_matches_build_mode() {
		assert!(!critical_section_is_held());

		{
			let _guard = enter_critical_section();
			assert!(critical_section_is_held());
		}

		assert!(!critical_section_is_held());
	}

	#[test]
	fn critical_section_sync_reentry_suspends_without_deadlock() {
		let first = Box::into_raw(Box::new(1_u8)) as *mut PyObject;
		let second = Box::into_raw(Box::new(2_u8)) as *mut PyObject;

		{
			let _outer = begin_critical_section(second);
			assert!(critical_section_is_held());

			{
				let _inner = begin_critical_section2(first, second);
				assert!(critical_section_is_held());
			}

			assert!(critical_section_is_held());
		}

		assert!(!critical_section_is_held());

		unsafe {
			drop(Box::from_raw(first.cast::<u8>()));
			drop(Box::from_raw(second.cast::<u8>()));
		}
	}

	#[test]
	fn critical_section_sync_begin2_orders_addresses_across_threads() {
		use std::{
			sync::{Arc, Barrier},
			thread,
		};

		let left = Box::into_raw(Box::new(1_u8)) as usize;
		let right = Box::into_raw(Box::new(2_u8)) as usize;
		let barrier = Arc::new(Barrier::new(2));

		let first_barrier = Arc::clone(&barrier);
		let first = thread::spawn(move || {
			first_barrier.wait();
			for _ in 0..256 {
				let _guard = begin_critical_section2(left as *mut PyObject, right as *mut PyObject);
				thread::yield_now();
			}
		});

		let second_barrier = Arc::clone(&barrier);
		let second = thread::spawn(move || {
			second_barrier.wait();
			for _ in 0..256 {
				let _guard = begin_critical_section2(right as *mut PyObject, left as *mut PyObject);
				thread::yield_now();
			}
		});

		first.join().expect("first locker thread should finish");
		second.join().expect("second locker thread should finish");

		unsafe {
			drop(Box::from_raw(left as *mut u8));
			drop(Box::from_raw(right as *mut u8));
		}
	}
}
