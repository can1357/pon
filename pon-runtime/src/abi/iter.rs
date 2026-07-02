//! Iterator and async-iterator helper family namespace.
//!
//! Phase-B exposes the synchronous iteration ABI now.  Async iteration and
//! unpack helpers remain owned by later workstreams.

use crate::abstract_op;
use crate::feedback::FeedbackCell;
use crate::object::PyObject;

/// Unpack helper status return: `0` success, `-1` error.
pub type UnpackStatus = i32;

/// Builds an iterator from an object through `tp_iter` or the sequence iterator
/// seam.  The feedback pointer is accepted for ABI stability and ignored.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_get_iter(object: *mut PyObject, feedback: *mut FeedbackCell) -> *mut PyObject {
    unsafe { super::record_feedback_unary(feedback, object) };
    super::catch_object_helper(|| {
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
