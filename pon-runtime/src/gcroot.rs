//! GC root registries for immortal leaked-box native objects.
//!
//! Native seeds allocate their objects as `Box::into_raw` leaked boxes on the
//! Rust heap, so `pon-gc` marking can neither reach nor trace them.  Any
//! GC-heap reference such an object holds (a source iterator, a saved value,
//! a callable) is invisible to `crate::abi::collect` and would be swept while
//! the holder is still live — a use-after-free the first time the iterator
//! advances again.
//!
//! [`RootRegistry`] is the shared mechanism behind the per-module
//! `gc_held_roots()` functions that `crate::abi::collect` walks (the
//! `_contextvars` pattern): every allocation registers itself together with a
//! monomorphized [`HeldRoots`] thunk, and each collection replays the thunks
//! to enumerate the currently held references.  Objects are immortal, so
//! registries only grow and entries are never invalidated; the held sources
//! stay pinned for the process lifetime, matching the established leaked-box
//! memory model.
//!
//! `held_roots` runs while `crate::abi::collect` holds the runtime lock, so
//! thunks must not re-enter the runtime: they only read fields of their own
//! (live, leaked) allocation and report pointers.  NULL fields and tagged
//! immediates are filtered here; pointers to other leaked boxes are harmless
//! to report (the collector ignores addresses outside its heap).

use std::sync::Mutex;

use crate::object::PyObject;

/// Enumerates every possibly-GC-heap reference one leaked-box object holds.
///
/// Implementations report raw field values as-is: NULL slots and tagged
/// immediates are fine (the registry filters them), and exhausted iterators
/// that null their sources naturally stop reporting them.
pub(crate) trait HeldRoots {
	/// Calls `push` for each held reference.
	///
	/// # Safety
	/// `self` must be a live allocation of the implementing layout; the
	/// implementation must not re-enter the runtime.
	unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject));
}

/// Type-erased [`HeldRoots::held_roots`] entry for one registered object.
type RootsFn = unsafe fn(*mut PyObject, &mut dyn FnMut(*mut PyObject));

unsafe fn roots_thunk<T: HeldRoots>(object: *mut PyObject, push: &mut dyn FnMut(*mut PyObject)) {
	// SAFETY: The registry only pairs `roots_thunk::<T>` with objects that
	// were allocated as `T`, and registered objects are immortal.
	unsafe { (*object.cast::<T>()).held_roots(push) }
}

/// Append-only registry of one native family's leaked-box allocations.
pub(crate) struct RootRegistry {
	entries: Mutex<Vec<(usize, RootsFn)>>,
}

impl RootRegistry {
	pub(crate) const fn new() -> Self {
		Self { entries: Mutex::new(Vec::new()) }
	}

	/// Records a freshly leaked `T` allocation and echoes the pointer, so
	/// allocation sites can wrap their `Box::into_raw(..).cast()` expression.
	pub(crate) fn register<T: HeldRoots>(&self, object: *mut PyObject) -> *mut PyObject {
		self
			.entries
			.lock()
			.unwrap_or_else(|poison| poison.into_inner())
			.push((object as usize, roots_thunk::<T>));
		object
	}

	/// Every reference currently held across the family, NULL-free and with
	/// tagged immediates dropped.  Consumed by `crate::abi::collect`.
	pub(crate) fn held_roots(&self) -> Vec<*mut PyObject> {
		let entries = self
			.entries
			.lock()
			.unwrap_or_else(|poison| poison.into_inner());
		let mut roots = Vec::new();
		let mut push = |value: *mut PyObject| {
			if !value.is_null() && crate::tag::is_heap(value) {
				roots.push(value);
			}
		};
		for &(addr, thunk) in entries.iter() {
			// SAFETY: Entries pair live immortal allocations with the thunk
			// monomorphized for their layout (`register` is the only writer).
			unsafe { thunk(addr as *mut PyObject, &mut push) };
		}
		roots
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Leaks a header-only allocation to stand in for a GC-heap value.  Box
	/// alignment keeps the low tag bits clear, so `crate::tag::is_heap`
	/// accepts it under both feature configurations.  Test allocations are
	/// deliberately leaked, matching the immortal leaked-box model the
	/// registry serves.
	fn heap_value() -> *mut PyObject {
		Box::into_raw(Box::new(PyObject::new(core::ptr::null())))
	}

	/// `held_roots` promises a NULL-free set of held references, not an
	/// ordering; comparisons sort both sides.
	fn sorted(mut roots: Vec<*mut PyObject>) -> Vec<*mut PyObject> {
		roots.sort_unstable();
		roots
	}

	/// Iterator-like layout reporting both of its source slots.
	#[repr(C)]
	struct PairHolder {
		header: PyObject,
		first:  *mut PyObject,
		second: *mut PyObject,
	}

	impl HeldRoots for PairHolder {
		unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
			push(self.first);
			push(self.second);
		}
	}

	fn leak_pair(first: *mut PyObject, second: *mut PyObject) -> *mut PyObject {
		let header = PyObject::new(core::ptr::null());
		Box::into_raw(Box::new(PairHolder { header, first, second })).cast()
	}

	/// A second layout whose one root sits at `PairHolder::second`'s offset,
	/// with a non-root pointer at `PairHolder::first`'s offset: a thunk
	/// dispatched with the wrong layout observably reports `decoy`.
	#[repr(C)]
	struct SkewHolder {
		header: PyObject,
		decoy:  *mut PyObject,
		held:   *mut PyObject,
	}

	impl HeldRoots for SkewHolder {
		unsafe fn held_roots(&self, push: &mut dyn FnMut(*mut PyObject)) {
			push(self.held);
		}
	}

	fn leak_skew(decoy: *mut PyObject, held: *mut PyObject) -> *mut PyObject {
		let header = PyObject::new(core::ptr::null());
		Box::into_raw(Box::new(SkewHolder { header, decoy, held })).cast()
	}

	#[test]
	fn register_echoes_the_pointer_it_was_given() {
		let registry = RootRegistry::new();
		assert!(registry.held_roots().is_empty());

		let object = leak_pair(heap_value(), heap_value());
		assert_eq!(registry.register::<PairHolder>(object), object);
	}

	#[test]
	fn held_roots_dispatches_each_entry_through_its_own_layout() {
		let registry = RootRegistry::new();
		let (a, b, c) = (heap_value(), heap_value(), heap_value());
		let decoy = heap_value();

		registry.register::<PairHolder>(leak_pair(a, b));
		registry.register::<SkewHolder>(leak_skew(decoy, c));

		// Exactly the union of what each impl pushes: a swapped or
		// wrong-layout thunk would surface `decoy` (SkewHolder read through
		// PairHolder's layout) or drop a root.
		assert_eq!(sorted(registry.held_roots()), sorted(vec![a, b, c]));
	}

	#[test]
	fn held_roots_drops_null_fields_on_every_replay() {
		let registry = RootRegistry::new();
		let kept = heap_value();
		let released = heap_value();
		let holder = leak_pair(kept, released);
		registry.register::<PairHolder>(holder);

		assert_eq!(sorted(registry.held_roots()), sorted(vec![kept, released]));

		// An exhausted iterator nulls its source; the next replay must stop
		// reporting it without any registry update.
		unsafe { (*holder.cast::<PairHolder>()).second = core::ptr::null_mut() };
		assert_eq!(registry.held_roots(), vec![kept]);
	}

	#[test]
	fn held_roots_drops_tagged_immediates() {
		let registry = RootRegistry::new();
		let boxed = heap_value();
		registry.register::<PairHolder>(leak_pair(crate::tag::tag_small_int(7), boxed));
		assert_eq!(registry.held_roots(), vec![boxed]);
	}
}
