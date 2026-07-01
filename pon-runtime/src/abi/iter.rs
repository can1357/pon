//! Iterator and async-iterator helper family namespace.
//!
//! Phase-B exposes the synchronous iteration ABI now.  Async iteration and
//! unpack helpers remain owned by later workstreams.

use crate::abstract_op;
use crate::feedback::FeedbackCell;
use crate::object::{PyObject, is_exact_type};

/// Unpack helper status return: `0` success, `-1` error.
pub type UnpackStatus = i32;

unsafe fn is_none_object(object: *mut PyObject) -> bool {
    if object.is_null() {
        return false;
    }
    super::with_runtime(|runtime| unsafe { is_exact_type(object, runtime.none_type.cast_const()) }).unwrap_or(false)
}

/// Builds an iterator from an object through `tp_iter` or the sequence iterator
/// seam.  The feedback pointer is accepted for ABI stability and ignored.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_get_iter(object: *mut PyObject, feedback: *mut FeedbackCell) -> *mut PyObject {
    unsafe { super::record_feedback_unary(feedback, object) };
    super::catch_object_helper(|| {
        if unsafe { is_none_object(object) } && super::r#gen::has_eager_yields() {
            // SAFETY: Consumes values recorded by eager `yield` lowering and wraps
            // them in the normal heap-frame generator representation.
            return unsafe { super::r#gen::take_eager_yield_generator() };
        }
        // SAFETY: Delegates to the generic iterator protocol.
        unsafe { abstract_op::get_iter(object) }
    })
}

/// Advances an iterator through `tp_iternext` or the sequence next seam.  The
/// feedback pointer is accepted for ABI stability and ignored.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_iter_next(iterator: *mut PyObject, feedback: *mut FeedbackCell) -> *mut PyObject {
    unsafe { super::record_feedback_unary(feedback, iterator) };
    super::catch_object_helper(|| unsafe { abstract_op::iter_next(iterator) })
}
