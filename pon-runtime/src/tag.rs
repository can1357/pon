//! Pointer-tagging scheme for immediate small integers (pin J0.2).
//!
//! Every value crossing the compiled-code ABI is a `*mut PyObject`.  This
//! module carves the low two address bits into a tag channel:
//!
//! | low bits | meaning                                            |
//! |----------|----------------------------------------------------|
//! | `..._1`  | immediate SmallInt; payload = `value << 1`, 63-bit |
//! | `...00`  | heap `*mut PyObject` (allocations are 16-aligned)  |
//! | `...10`  | RESERVED (future interned small str / float)       |
//!
//! Tagged values are pure bit patterns: they carry no provenance, are never
//! dereferenced, and are immutable.  The API below is the frozen J0.2 contract
//! consumed by track N1 (helper audit + codegen fast paths) and the K/L
//! agents; see `plans/pon-pin-J02-tagged-ints.md` for the full design,
//! GC filter contract, and cutover plan.
//!
//! Everything here is `#[inline(always)]` and branch-free where possible so
//! tier-1 codegen and helper preludes pay one test-and-branch per operand.

use crate::object::PyObject;

/// Bit 0 set marks an immediate small integer.
pub const TAG_INT_BIT: usize = 1;
/// Low-two-bits pattern reserved for a future immediate kind (`...10`).
///
/// No value with this pattern is produced today.  The GC filter and helper
/// preludes already treat it as a non-pointer so activating it later is a
/// runtime-local change.
pub const TAG_RESERVED: usize = 0b10;
/// Mask selecting the tag channel (low two bits).
pub const TAG_MASK: usize = 0b11;
/// Low-two-bits pattern of every real heap `*mut PyObject`.
pub const TAG_HEAP: usize = 0b00;

/// Smallest immediate value: `-(2^62)`.
pub const SMALL_INT_MIN: i64 = -(1 << 62);
/// Largest immediate value: `2^62 - 1`.
pub const SMALL_INT_MAX: i64 = (1 << 62) - 1;

const _: () = assert!(
	core::mem::align_of::<PyObject>() >= 4,
	"heap PyObject pointers must have two zero low bits for the tag channel",
);

/// Returns true when `p` is a tagged immediate small integer.
#[inline(always)]
#[must_use]
pub fn is_small_int(p: *mut PyObject) -> bool {
	p.addr() & TAG_INT_BIT != 0
}

/// Returns true when `p` is an ordinary heap object pointer (low bits `00`).
///
/// NULL also reports true: it is not an immediate, and every helper keeps its
/// existing NULL-sentinel discipline ahead of any tag test.
#[inline(always)]
#[must_use]
pub fn is_heap(p: *mut PyObject) -> bool {
	p.addr() & TAG_MASK == TAG_HEAP
}

/// Encodes an in-range value as a tagged immediate: `(v << 1) | 1`.
///
/// The result is a provenance-free bit pattern; it must never be dereferenced.
/// Callers prove the range (`SMALL_INT_MIN..=SMALL_INT_MAX`) themselves or use
/// [`try_tag_small_int`]; out-of-range values are rejected only by
/// `debug_assert!`.
#[inline(always)]
#[must_use]
pub fn tag_small_int(v: i64) -> *mut PyObject {
	debug_assert!(
		(SMALL_INT_MIN..=SMALL_INT_MAX).contains(&v),
		"tag_small_int: {v} outside the 63-bit immediate range",
	);
	core::ptr::without_provenance_mut::<PyObject>(((v as usize) << 1) | TAG_INT_BIT)
}

/// Decodes a tagged immediate produced by [`tag_small_int`].
///
/// The arithmetic right shift restores the sign of the 63-bit payload.
/// Calling this on a non-immediate is a logic error; it is rejected only by
/// `debug_assert!`.
#[inline(always)]
#[must_use]
pub fn untag_small_int(p: *mut PyObject) -> i64 {
	debug_assert!(is_small_int(p), "untag_small_int on a non-immediate value");
	(p.addr() as i64) >> 1
}

/// Encodes `v` as a tagged immediate, or returns `None` when it does not fit
/// the 63-bit range.
#[inline(always)]
#[must_use]
pub fn try_tag_small_int(v: i64) -> Option<*mut PyObject> {
	if (SMALL_INT_MIN..=SMALL_INT_MAX).contains(&v) {
		Some(tag_small_int(v))
	} else {
		None
	}
}

/// Normalizes a possibly-tagged helper argument into a boxed `*mut PyObject`.
///
/// Heap pointers (and NULL) pass through untouched.  A tagged immediate is
/// boxed through [`crate::abi::boxed_const_int`], which deliberately bypasses
/// `pon_const_int` so enabling tagged producers cannot make normalization loop.
/// On allocation failure this returns NULL with the thread-state error already set.
///
/// This is the slow-path floor used by [`untag_prelude!`]; helpers with a
/// dedicated immediate fast path test [`is_small_int`] themselves instead and
/// skip the allocation.
#[inline(always)]
#[must_use]
pub fn untag_arg(p: *mut PyObject) -> *mut PyObject {
	if is_small_int(p) {
		crate::abi::boxed_const_int(untag_small_int(p))
	} else {
		p
	}
}

/// Helper-entry prelude: rebinds each named `*mut PyObject` argument to its
/// boxed form before any dereference.
///
/// EVERY `extern "C" pon_*` helper that receives `*mut PyObject` arguments
/// must run this (or an explicit immediate fast path) before touching
/// `ob_type` or any other field — after the `tagged-ints` cutover, compiled
/// code passes tagged immediates through every object-typed ABI slot.
///
/// Boxing a tagged argument can fail (allocation/runtime error).  The first
/// form early-returns NULL — correct for object-returning helpers.  Helpers
/// returning `c_int`/`isize` name their error sentinel with the `err = …`
/// form.
///
/// ```ignore
/// pub unsafe extern "C" fn pon_example(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
///     untag_prelude!(a, b);
///     // `a` and `b` are now guaranteed heap-or-NULL.
/// }
///
/// pub unsafe extern "C" fn pon_example_len(o: *mut PyObject) -> isize {
///     untag_prelude!(err = -1; o);
///     // ...
/// }
/// ```
#[macro_export]
macro_rules! untag_prelude {
    (err = $sentinel:expr; $($arg:ident),+ $(,)?) => {
        $(
            let $arg: *mut $crate::object::PyObject = {
                let normalized = $crate::tag::untag_arg($arg);
                if $crate::tag::is_small_int($arg) && normalized.is_null() {
                    return $sentinel;
                }
                normalized
            };
        )+
    };
    ($($arg:ident),+ $(,)?) => {
        $crate::untag_prelude!(err = core::ptr::null_mut(); $($arg),+);
    };
}

#[cfg(test)]
mod macro_tests {
	use crate::object::PyObject;

	/// Shape-A helper proves the macro expands and type-checks with the default
	/// NULL sentinel.
	unsafe extern "C" fn passthrough(o: *mut PyObject) -> *mut PyObject {
		untag_prelude!(o);
		o
	}

	/// Sentinel form: proves `err = …` expansion for non-object returns.
	unsafe extern "C" fn probe_len(o: *mut PyObject) -> isize {
		untag_prelude!(err = -1; o);
		if o.is_null() { 0 } else { 1 }
	}

	#[test]
	fn gc_tag_constants_match_runtime_tag_constants() {
		assert_eq!(pon_gc::IMMEDIATE_TAG_MASK, super::TAG_MASK);
		assert_eq!(pon_gc::IMMEDIATE_TAG_HEAP, super::TAG_HEAP);
	}

	#[test]
	fn prelude_passes_heap_and_null_through() {
		// SAFETY: NULL is the documented pass-through sentinel; no deref occurs.
		unsafe {
			assert!(passthrough(core::ptr::null_mut()).is_null());
			assert_eq!(probe_len(core::ptr::null_mut()), 0);
		}
	}

	#[test]
	fn prelude_boxes_tagged_arguments() {
		let _guard = crate::thread_state::test_state_lock();
		crate::thread_state::pon_err_clear();
		// SAFETY: init is idempotent and required before allocating helpers.
		unsafe {
			assert_eq!(crate::abi::pon_runtime_init(), 0);
		}
		let tagged = super::tag_small_int(-9);
		// SAFETY: `passthrough` normalizes the tagged value before returning it.
		let boxed = unsafe { passthrough(tagged) };
		assert!(super::is_heap(boxed), "prelude must yield a heap object");
		assert!(!boxed.is_null());
		// SAFETY: The prelude returned a live boxed PyLong allocation.
		let value = unsafe { (*boxed.cast::<crate::object::PyLong>()).value };
		assert_eq!(value, -9);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn addr(p: *mut PyObject) -> usize {
		p.addr()
	}

	#[test]
	fn round_trips_zero_and_units() {
		for v in [0i64, 1, -1] {
			let tagged = tag_small_int(v);
			assert!(is_small_int(tagged));
			assert!(!is_heap(tagged));
			assert!(!tagged.is_null(), "tagged {v} must not alias NULL");
			assert_eq!(untag_small_int(tagged), v);
		}
		assert_eq!(addr(tag_small_int(0)), 1);
		assert_eq!(addr(tag_small_int(1)), 3);
	}

	#[test]
	fn round_trips_range_boundaries() {
		for v in [SMALL_INT_MIN, SMALL_INT_MIN + 1, SMALL_INT_MAX - 1, SMALL_INT_MAX] {
			let tagged = try_tag_small_int(v).expect("boundary value must fit");
			assert!(is_small_int(tagged));
			assert_eq!(untag_small_int(tagged), v);
		}
	}

	#[test]
	fn rejects_out_of_range() {
		assert_eq!(try_tag_small_int(SMALL_INT_MIN - 1), None);
		assert_eq!(try_tag_small_int(SMALL_INT_MAX + 1), None);
		assert_eq!(try_tag_small_int(i64::MIN), None);
		assert_eq!(try_tag_small_int(i64::MAX), None);
	}

	#[test]
	fn heap_patterns_classify_as_heap() {
		for fake in [0usize, 16, 4096, usize::MAX & !TAG_MASK] {
			let p = core::ptr::without_provenance_mut::<PyObject>(fake);
			assert!(is_heap(p), "aligned pattern {fake:#x} must classify as heap");
			assert!(!is_small_int(p));
		}
	}

	#[test]
	fn reserved_pattern_is_neither_heap_nor_small_int() {
		let p = core::ptr::without_provenance_mut::<PyObject>(0x1000 | TAG_RESERVED);
		assert!(!is_heap(p));
		assert!(!is_small_int(p));
	}

	#[test]
	fn tagged_identity_is_value_identity() {
		assert_eq!(addr(tag_small_int(42)), addr(tag_small_int(42)));
		assert_ne!(addr(tag_small_int(42)), addr(tag_small_int(43)));
	}
}
