//! Thread attachment and no-GIL registry support.
//!
//! Each attached OS thread receives its own leaked `PonThreadState` mutex so
//! existing `thread_state_lock()` users keep the same guard-based API while
//! GC-facing code can enumerate live thread handles.

use std::sync::atomic::Ordering;
use std::{
	panic::{AssertUnwindSafe, catch_unwind},
	ptr,
	sync::{Mutex, MutexGuard, atomic::AtomicU64},
	thread as os_thread,
};

use crate::{
	object::PyObject,
	thread_state::{PonThreadState, pon_err_set},
};

/// Callback signature accepted by [`pon_thread_start_new`].
pub type PonThreadEntry = unsafe extern "C" fn(*mut PyObject) -> *mut PyObject;

#[derive(Clone, Copy)]
struct ThreadRecord {
	id:           u64,
	os_thread_id: os_thread::ThreadId,
	state:        &'static Mutex<PonThreadState>,
}

/// Snapshot of one live runtime thread.
///
/// `state` points at the thread's `PonThreadState` while a
/// [`ThreadRegistry::for_each`] callback is running.  GC handshake code should
/// copy the root metadata it needs inside that callback and must not retain the
/// raw pointer after the callback returns.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ThreadHandle {
	/// Stable registry id assigned on attach.
	pub id:                u64,
	/// Opaque OS thread id hash for diagnostics and deterministic tests.
	pub os_thread_id:      u64,
	/// Current thread-state pointer, valid only for the callback duration.
	pub state:             *mut PonThreadState,
	/// Conservative stack base captured by compiled/native entry shims.
	pub stack_base:        *mut u8,
	/// Most recent safe-region top frame/stack pointer, or NULL when unknown.
	pub stack_top_fp:      *mut u8,
	/// Non-zero when the thread is inside a GC-safe region.
	pub in_gc_safe_region: u8,
}

/// Live-thread registry used by the no-GIL runtime and future GC
/// handshakes.
///
/// `attach_current` registers the calling OS thread if it is not already
/// attached. `detach_current` removes the calling OS thread from the live set.
/// `for_each` locks each live thread state long enough to emit a
/// [`ThreadHandle`] snapshot with stack-base/top-frame metadata.
pub struct ThreadRegistry {
	next_id: AtomicU64,
	threads: Mutex<Vec<ThreadRecord>>,
}

impl ThreadRegistry {
	/// Creates an empty registry.
	#[must_use]
	pub const fn new() -> Self {
		Self { next_id: AtomicU64::new(1), threads: Mutex::new(Vec::new()) }
	}

	/// Attaches the calling OS thread and returns its thread-state mutex.
	#[must_use]
	pub fn attach_current(&'static self) -> &'static Mutex<PonThreadState> {
		attach_current_thread(self)
	}

	/// Detaches the calling OS thread from the live set.
	pub fn detach_current(&'static self) -> Result<(), &'static str> {
		detach_current_thread(self)
	}

	/// Visits every live thread handle.
	///
	/// The raw `state` pointer in each handle is valid only until the callback
	/// returns; callers should copy fields rather than retaining the pointer.
	pub fn for_each(&self, mut visit: impl FnMut(ThreadHandle)) {
		let records = {
			let guard = lock_records(&self.threads);
			guard.clone()
		};
		for record in records {
			let mut state = lock_state(record.state);
			let state_ptr = (&mut *state) as *mut PonThreadState;
			visit(ThreadHandle {
				id:                record.id,
				os_thread_id:      os_thread_id_u64(record.os_thread_id),
				state:             state_ptr,
				stack_base:        state.stack_base,
				stack_top_fp:      state.stack_top_fp,
				in_gc_safe_region: u8::from(state.in_gc_safe_region()),
			});
		}
	}
}

impl Default for ThreadRegistry {
	fn default() -> Self {
		Self::new()
	}
}

static THREAD_REGISTRY: ThreadRegistry = ThreadRegistry::new();

/// Returns the process registry for attached runtime threads.
#[must_use]
pub fn thread_registry() -> &'static ThreadRegistry {
	&THREAD_REGISTRY
}

thread_local! {
	 static LOCAL_THREAD_STATE: std::cell::Cell<*const Mutex<PonThreadState>> = const { std::cell::Cell::new(ptr::null()) };
}

/// Returns the current OS thread's runtime state, attaching it lazily.
pub(crate) fn current_thread_state_mutex() -> &'static Mutex<PonThreadState> {
	thread_registry().attach_current()
}

fn attach_current_thread(registry: &'static ThreadRegistry) -> &'static Mutex<PonThreadState> {
	if let Some(existing) = LOCAL_THREAD_STATE.with(|slot| {
		let ptr = slot.get();
		(!ptr.is_null()).then_some(ptr)
	}) {
		// SAFETY: Pointers stored in LOCAL_THREAD_STATE come from Box::leak below.
		return unsafe { &*existing };
	}

	let state = Box::leak(Box::new(Mutex::new(PonThreadState::default())));
	let record = ThreadRecord {
		id: registry.next_id.fetch_add(1, Ordering::Relaxed),
		os_thread_id: os_thread::current().id(),
		state,
	};
	lock_records(&registry.threads).push(record);
	LOCAL_THREAD_STATE.with(|slot| slot.set(state as *const Mutex<PonThreadState>));
	state
}

fn detach_current_thread(registry: &'static ThreadRegistry) -> Result<(), &'static str> {
	let state_ptr = LOCAL_THREAD_STATE.with(|slot| {
		let ptr = slot.get();
		if !ptr.is_null() {
			slot.set(ptr::null());
		}
		ptr
	});
	if state_ptr.is_null() {
		return Err("thread is not attached");
	}
	pon_gc::set_external_stack_base(ptr::null_mut());
	let mut threads = lock_records(&registry.threads);
	if let Some(index) = threads
		.iter()
		.position(|record| ptr::addr_eq(record.state, state_ptr))
	{
		threads.swap_remove(index);
		Ok(())
	} else {
		Err("thread registry entry is missing")
	}
}


fn lock_records(records: &Mutex<Vec<ThreadRecord>>) -> MutexGuard<'_, Vec<ThreadRecord>> {
	records.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn lock_state(state: &'static Mutex<PonThreadState>) -> MutexGuard<'static, PonThreadState> {
	state.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn os_thread_id_u64(id: os_thread::ThreadId) -> u64 {
	use std::{
		collections::hash_map::DefaultHasher,
		hash::{Hash, Hasher},
	};

	let mut hasher = DefaultHasher::new();
	id.hash(&mut hasher);
	hasher.finish()
}

/// Opaque numeric identifier for the current OS thread.
#[must_use]
pub fn current_os_thread_id() -> u64 {
	os_thread_id_u64(os_thread::current().id())
}

fn thread_state_ptr(state: &'static Mutex<PonThreadState>) -> *mut PonThreadState {
	let mut guard = lock_state(state);
	(&mut *guard) as *mut PonThreadState
}

/// Attaches the current OS thread to the runtime.
///
/// Returns NULL and records a diagnostic on panic; otherwise returns a stable
/// pointer to the calling thread's state.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_thread_attach() -> *mut PonThreadState {
	match catch_unwind(AssertUnwindSafe(|| thread_state_ptr(thread_registry().attach_current()))) {
		Ok(state) => state,
		Err(_) => {
			pon_err_set("thread attach panicked");
			ptr::null_mut()
		},
	}
}

/// Detaches the current OS thread from the runtime live-thread set.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_thread_detach() -> i32 {
	match catch_unwind(AssertUnwindSafe(|| thread_registry().detach_current())) {
		Ok(Ok(())) => 0,
		Ok(Err(message)) => {
			pon_err_set(message);
			-1
		},
		Err(_) => {
			pon_err_set("thread detach panicked");
			-1
		},
	}
}

/// Starts a new attached OS thread running `entry(arg)`.
///
/// The new thread attaches before invoking the callback and detaches after the
/// callback returns or panics.  A NULL entry returns `-1` and records a
/// diagnostic.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_thread_start_new(entry: *const u8, arg: *mut PyObject) -> i32 {
	crate::untag_prelude!(err = -1; arg);
	match catch_unwind(AssertUnwindSafe(|| {
		if entry.is_null() {
			pon_err_set("thread entry pointer is null");
			return -1;
		}
		{
			let entry_addr = entry as usize;
			let arg_addr = arg as usize;
			match os_thread::Builder::new()
				.name("pon-runtime".to_string())
				.spawn(move || {
					let mut stack_base_marker = 0usize;
					let _state = thread_registry().attach_current();
					crate::aot_entry::capture_stack_base(ptr::addr_of_mut!(stack_base_marker).cast::<u8>());
					let _ = catch_unwind(AssertUnwindSafe(|| {
						// SAFETY: The ABI caller supplied a function with PonThreadEntry shape.
						let entry: PonThreadEntry = unsafe { std::mem::transmute(entry_addr) };
						let arg = arg_addr as *mut PyObject;
						unsafe { entry(arg) };
					}));
					let _ = thread_registry().detach_current();
				}) {
				Ok(_handle) => 0,
				Err(error) => {
					pon_err_set(format!("failed to start thread: {error}"));
					-1
				},
			}
		}
	})) {
		Ok(status) => status,
		Err(_) => {
			pon_err_set("thread start panicked");
			-1
		},
	}
}

/// Marks the current thread as being inside a GC-safe region.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gc_safe_region_enter() -> i32 {
	match catch_unwind(AssertUnwindSafe(|| {
		let mut marker = 0usize;
		let mut state = crate::thread_state::thread_state_lock();
		state.enter_gc_safe_region((&mut marker as *mut usize).cast::<u8>());
		0
	})) {
		Ok(status) => status,
		Err(_) => {
			pon_err_set("GC safe-region enter panicked");
			-1
		},
	}
}

/// Leaves a GC-safe region previously entered by this thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gc_safe_region_leave() -> i32 {
	match catch_unwind(AssertUnwindSafe(|| {
		let result = {
			let mut state = crate::thread_state::thread_state_lock();
			state.leave_gc_safe_region()
		};
		match result {
			Ok(()) => 0,
			Err(message) => {
				pon_err_set(message);
				-1
			},
		}
	})) {
		Ok(status) => status,
		Err(_) => {
			pon_err_set("GC safe-region leave panicked");
			-1
		},
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::thread_state::{pon_err_clear, pon_err_message, test_state_lock, thread_state_lock};

	fn handle_for_state(state: *mut PonThreadState) -> Option<ThreadHandle> {
		let mut found = None;
		thread_registry().for_each(|handle| {
			if ptr::addr_eq(handle.state, state) {
				found = Some(handle);
			}
		});
		found
	}

	#[test]
	fn registry_for_each_observes_current_thread_until_detach() {
		let _guard = test_state_lock();

		let registry = thread_registry();
		let state_mutex = registry.attach_current();
		let state = thread_state_ptr(state_mutex);

		let observed: Vec<_> = {
			let mut handles = Vec::new();
			registry.for_each(|handle| {
				if ptr::addr_eq(handle.state, state) {
					handles.push(handle);
				}
			});
			handles
		};
		assert_eq!(observed.len(), 1);
		let handle = observed[0];
		assert_eq!(handle.state, state);
		assert_ne!(handle.id, 0);
		assert_ne!(handle.os_thread_id, 0);
		assert!(handle.stack_base.is_null());
		assert!(handle.stack_top_fp.is_null());
		assert_eq!(handle.in_gc_safe_region, 0);

		let duplicate_state = registry.attach_current();
		assert!(ptr::addr_eq(duplicate_state, state_mutex));
		let mut duplicate_count = 0;
		registry.for_each(|handle| {
			if ptr::addr_eq(handle.state, state) {
				duplicate_count += 1;
			}
		});
		assert_eq!(duplicate_count, 1);

		assert_eq!(registry.detach_current(), Ok(()));
		let mut after_detach_count = 0;
		registry.for_each(|handle| {
			if ptr::addr_eq(handle.state, state) {
				after_detach_count += 1;
			}
		});
		assert_eq!(after_detach_count, 0);
		assert_eq!(registry.detach_current(), Err("thread is not attached"));
	}

	#[test]
	fn gc_safe_region_abi_updates_current_state_and_registry_handle() {
		let _guard = test_state_lock();
		pon_err_clear();

		let state = unsafe { pon_thread_attach() };
		assert!(!state.is_null());
		{
			let current = thread_state_lock();
			assert_eq!(current.gc_safe_region_depth(), 0);
			assert!(!current.in_gc_safe_region());
			assert!(current.stack_top_fp.is_null());
		}

		assert_eq!(unsafe { pon_gc_safe_region_enter() }, 0);
		{
			let current = thread_state_lock();
			assert_eq!(current.gc_safe_region_depth(), 1);
			assert!(current.in_gc_safe_region());
			assert!(!current.stack_top_fp.is_null());
		}
		let handle = handle_for_state(state).expect("attached current thread handle");
		assert_eq!(handle.in_gc_safe_region, 1);
		assert!(!handle.stack_top_fp.is_null());

		assert_eq!(unsafe { pon_gc_safe_region_enter() }, 0);
		{
			let current = thread_state_lock();
			assert_eq!(current.gc_safe_region_depth(), 2);
			assert!(current.in_gc_safe_region());
			assert!(!current.stack_top_fp.is_null());
		}

		assert_eq!(unsafe { pon_gc_safe_region_leave() }, 0);
		{
			let current = thread_state_lock();
			assert_eq!(current.gc_safe_region_depth(), 1);
			assert!(current.in_gc_safe_region());
			assert!(!current.stack_top_fp.is_null());
		}
		let handle = handle_for_state(state).expect("attached current thread handle");
		assert_eq!(handle.in_gc_safe_region, 1);
		assert!(!handle.stack_top_fp.is_null());

		assert_eq!(unsafe { pon_gc_safe_region_leave() }, 0);
		{
			let current = thread_state_lock();
			assert_eq!(current.gc_safe_region_depth(), 0);
			assert!(!current.in_gc_safe_region());
			assert!(current.stack_top_fp.is_null());
		}
		let handle = handle_for_state(state).expect("attached current thread handle");
		assert_eq!(handle.in_gc_safe_region, 0);
		assert!(handle.stack_top_fp.is_null());

		assert_eq!(unsafe { pon_gc_safe_region_leave() }, -1);
		assert_eq!(pon_err_message().as_deref(), Some("GC safe-region leave without matching enter"));
		{
			let current = thread_state_lock();
			assert_eq!(current.gc_safe_region_depth(), 0);
			assert!(!current.in_gc_safe_region());
			assert!(current.stack_top_fp.is_null());
		}
		pon_err_clear();
		assert_eq!(unsafe { pon_thread_detach() }, 0);
	}
}
