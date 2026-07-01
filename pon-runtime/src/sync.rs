//! Synchronization primitives reserved for the free-threaded runtime.
//!
//! The default runtime is still single-threaded.  The Rust primitives in this
//! module are available in every build so data structures can be shaped once,
//! while C ABI critical-section entry points are exported only when the
//! `free-threading` feature is enabled.

use core::ops::{Deref, DerefMut};
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};
#[cfg(feature = "free-threading")]
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{LazyLock, LockResult, Mutex, MutexGuard, TryLockError, TryLockResult};

use crate::object::{PyObject, PyType};

/// Runtime mutex used for state that will become shared in free-threaded builds.
///
/// `PonMutex` is a thin documented wrapper around [`std::sync::Mutex`].  It is
/// intentionally present in default builds as well as free-threaded builds so
/// later waves can add locks without changing public type shapes.  Poisoning is
/// preserved: callers see the same [`LockResult`] and [`TryLockResult`] contract
/// as the standard library.
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
    /// In the default runtime this normally has no contention, but it remains a
    /// real lock so tests and future free-threaded paths exercise the same
    /// semantics.
    pub fn lock(&self) -> LockResult<PonMutexGuard<'_, T>> {
        self.inner.lock().map(PonMutexGuard::new).map_err(|poison| {
            std::sync::PoisonError::new(PonMutexGuard::new(poison.into_inner()))
        })
    }

    /// Attempts to lock the mutex without blocking.
    pub fn try_lock(&self) -> TryLockResult<PonMutexGuard<'_, T>> {
        self.inner.try_lock().map(PonMutexGuard::new).map_err(|err| match err {
            TryLockError::Poisoned(poison) => {
                TryLockError::Poisoned(std::sync::PoisonError::new(PonMutexGuard::new(
                    poison.into_inner(),
                )))
            }
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

/// Side-table entry for free-threaded type coordination.
///
/// This intentionally lives outside [`PyType`] so enabling future type-level
/// locks or version epochs cannot shift object/type payload offsets.
#[derive(Debug)]
struct TypeFreeThreadingMeta {
    lock: PonMutex<()>,
    version_epoch: AtomicUsize,
}

impl TypeFreeThreadingMeta {
    fn new() -> Self {
        Self {
            lock: PonMutex::new(()),
            version_epoch: AtomicUsize::new(0),
        }
    }
}

static TYPE_FREE_THREADING_META: LazyLock<Mutex<HashMap<usize, &'static TypeFreeThreadingMeta>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn type_free_threading_meta(ty: *const PyType) -> &'static TypeFreeThreadingMeta {
    let key = ty as usize;
    let mut table = TYPE_FREE_THREADING_META
        .lock()
        .expect("type free-threading side table should not be poisoned");

    *table
        .entry(key)
        .or_insert_with(|| Box::leak(Box::new(TypeFreeThreadingMeta::new())))
}

/// Locks side-table metadata for `ty` without adding fields to [`PyType`].
///
/// Default builds may call this too; the metadata is created lazily and remains
/// off-object, preserving the GIL layout contract while giving Wave-1 a stable
/// type-level mutex accessor.
pub fn lock_type(ty: *const PyType) -> LockResult<PonMutexGuard<'static, ()>> {
    type_free_threading_meta(ty).lock.lock()
}

/// Returns the side-table type version epoch for `ty`.
#[must_use]
pub fn type_version_epoch(ty: *const PyType) -> usize {
    type_free_threading_meta(ty).version_epoch.load(Ordering::Acquire)
}

/// Bumps and returns the side-table type version epoch for `ty`.
pub fn bump_type_version_epoch(ty: *const PyType) -> usize {
    type_free_threading_meta(ty)
        .version_epoch
        .fetch_add(1, Ordering::AcqRel)
        + 1
}

#[cfg(feature = "free-threading")]
const GLOBAL_CRITICAL_SECTION_KEY: usize = 0;

#[cfg(feature = "free-threading")]
static GLOBAL_CRITICAL_SECTION_LOCK: PonMutex<()> = PonMutex::new(());

#[cfg(feature = "free-threading")]
static OBJECT_CRITICAL_SECTION_LOCKS: LazyLock<Mutex<HashMap<usize, &'static PonMutex<()>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(feature = "free-threading")]
#[derive(Debug)]
struct HeldCriticalLock {
    key: usize,
    owns: bool,
    guard: Option<PonMutexGuard<'static, ()>>,
}

#[cfg(feature = "free-threading")]
#[derive(Debug, Default)]
struct CriticalSectionFrame {
    locks: Vec<HeldCriticalLock>,
    suspended: Vec<usize>,
}

#[cfg(feature = "free-threading")]
thread_local! {
    static CRITICAL_SECTION_STACK: RefCell<Vec<CriticalSectionFrame>> = const { RefCell::new(Vec::new()) };
}

/// Guard for runtime object critical sections.
///
/// Default builds use an inert guard because Python bytecode is not executed
/// concurrently.  Free-threaded builds lock per-object side-table mutexes keyed
/// by object address.  Nested sections that would acquire a new mutex suspend
/// the current thread's active critical-section guards, acquire the requested
/// mutexes in deterministic address order, and restore the suspended guards when
/// the inner section ends.  This mirrors CPython-style suspend-on-reentry while
/// preserving the existing object layout.
#[derive(Debug)]
pub struct CriticalSectionGuard {
    active: bool,
}

impl CriticalSectionGuard {
    fn enter(object: *mut PyObject) -> Self {
        #[cfg(feature = "free-threading")]
        {
            push_critical_section(ordered_keys(&[critical_section_key(object)]));
            Self { active: true }
        }

        #[cfg(not(feature = "free-threading"))]
        {
            let _ = object;
            Self { active: false }
        }
    }

    fn enter2(left: *mut PyObject, right: *mut PyObject) -> Self {
        #[cfg(feature = "free-threading")]
        {
            push_critical_section(ordered_keys(&[
                critical_section_key(left),
                critical_section_key(right),
            ]));
            Self { active: true }
        }

        #[cfg(not(feature = "free-threading"))]
        {
            let _ = (left, right);
            Self { active: false }
        }
    }

    fn leave(&mut self) {
        if self.active {
            #[cfg(feature = "free-threading")]
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

#[cfg(feature = "free-threading")]
fn critical_section_key(object: *mut PyObject) -> usize {
    object as usize
}

#[cfg(feature = "free-threading")]
fn ordered_keys(keys: &[usize]) -> Vec<usize> {
    let mut keys: Vec<usize> = keys.iter().copied().collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

#[cfg(feature = "free-threading")]
fn critical_section_lock(key: usize) -> &'static PonMutex<()> {
    if key == GLOBAL_CRITICAL_SECTION_KEY {
        return &GLOBAL_CRITICAL_SECTION_LOCK;
    }

    let mut table = OBJECT_CRITICAL_SECTION_LOCKS
        .lock()
        .expect("object critical-section side table should not be poisoned");
    *table.entry(key).or_insert_with(|| Box::leak(Box::new(PonMutex::new(()))))
}

#[cfg(feature = "free-threading")]
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

#[cfg(feature = "free-threading")]
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

#[cfg(feature = "free-threading")]
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

#[cfg(feature = "free-threading")]
fn push_critical_section(keys: Vec<usize>) {
    CRITICAL_SECTION_STACK.with(|stack| {
        let mut stack = stack.borrow_mut();
        let active_keys = active_lock_keys(&stack);
        let all_requested_locks_are_already_active = keys.iter().all(|key| active_keys.binary_search(key).is_ok());
        let suspended = if stack.is_empty() || all_requested_locks_are_already_active {
            Vec::new()
        } else {
            suspend_active_locks(&mut stack)
        };

        let mut locks = Vec::with_capacity(keys.len());
        for key in keys {
            if suspended.is_empty() && active_keys.binary_search(&key).is_ok() {
                locks.push(HeldCriticalLock {
                    key,
                    owns: false,
                    guard: None,
                });
            } else {
                let guard = critical_section_lock(key)
                    .lock()
                    .expect("object critical-section mutex should not be poisoned");
                locks.push(HeldCriticalLock {
                    key,
                    owns: true,
                    guard: Some(guard),
                });
            }
        }
        stack.push(CriticalSectionFrame { locks, suspended });
    });
}

#[cfg(feature = "free-threading")]
fn pop_critical_section() {
    CRITICAL_SECTION_STACK.with(|stack| {
        let mut stack = stack.borrow_mut();
        let frame = stack.pop().expect("critical-section end without matching begin");
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
/// This is modeled as the global side-table key in free-threaded builds and as
/// an inert guard in default builds.
#[must_use]
pub fn enter_critical_section() -> CriticalSectionGuard {
    CriticalSectionGuard::enter(ptr::null_mut())
}

/// Returns whether this thread currently has an active runtime critical section.
///
/// Default builds always return `false` because [`enter_critical_section`] is
/// intentionally inert without `free-threading`.
#[must_use]
pub fn critical_section_is_held() -> bool {
    #[cfg(feature = "free-threading")]
    {
        CRITICAL_SECTION_STACK.with(|stack| !stack.borrow().is_empty())
    }

    #[cfg(not(feature = "free-threading"))]
    {
        false
    }
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
#[cfg(feature = "free-threading")]
#[unsafe(no_mangle)]
pub extern "C" fn pon_runtime_enter_critical_section() {
    push_critical_section(ordered_keys(&[GLOBAL_CRITICAL_SECTION_KEY]));
}

/// Releases the legacy global critical section for generated-code callers.
#[cfg(feature = "free-threading")]
#[unsafe(no_mangle)]
pub extern "C" fn pon_runtime_leave_critical_section() {
    pop_critical_section();
}

/// Acquires a one-object critical section for generated-code callers.
#[cfg(feature = "free-threading")]
#[unsafe(no_mangle)]
pub extern "C" fn pon_runtime_begin_critical_section(object: *mut PyObject) {
    push_critical_section(ordered_keys(&[critical_section_key(object)]));
}

/// Releases the most recent one-object critical section for generated-code callers.
#[cfg(feature = "free-threading")]
#[unsafe(no_mangle)]
pub extern "C" fn pon_runtime_end_critical_section() {
    pop_critical_section();
}

/// Acquires a two-object critical section for generated-code callers.
#[cfg(feature = "free-threading")]
#[unsafe(no_mangle)]
pub extern "C" fn pon_runtime_begin_critical_section2(left: *mut PyObject, right: *mut PyObject) {
    push_critical_section(ordered_keys(&[
        critical_section_key(left),
        critical_section_key(right),
    ]));
}

/// Releases the most recent two-object critical section for generated-code callers.
#[cfg(feature = "free-threading")]
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
            Err(TryLockError::WouldBlock) => {}
            other => panic!("expected WouldBlock while lock is held, got {other:?}"),
        }
    }

    #[test]
    fn pon_mutex_get_mut_and_into_inner_return_protected_value() {
        let mut mutex = PonMutex::new(vec![1]);

        mutex.get_mut().expect("mutex should not be poisoned").push(2);

        assert_eq!(mutex.into_inner().expect("mutex should not be poisoned"), vec![1, 2]);
    }

    #[test]
    fn critical_section_held_state_matches_build_mode() {
        assert!(!critical_section_is_held());

        {
            let _guard = enter_critical_section();

            #[cfg(feature = "free-threading")]
            assert!(critical_section_is_held());

            #[cfg(not(feature = "free-threading"))]
            assert!(!critical_section_is_held());
        }

        assert!(!critical_section_is_held());
    }

    #[cfg(feature = "free-threading")]
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

    #[cfg(feature = "free-threading")]
    #[test]
    fn critical_section_sync_begin2_orders_addresses_across_threads() {
        use std::sync::{Arc, Barrier};
        use std::thread;

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