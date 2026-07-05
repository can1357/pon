//! Object protocol helper family namespace.
//!
//! Truthiness, rich comparison, attribute, and subscription helpers route
//! through `crate::abstract_op` so all slot dispatch and TypeError fallback
//! behavior stays centralized.

use crate::{abstract_op, feedback::FeedbackCell, object::PyObject};

/// Rich-comparison operation selector compatible with CPython-style ordering.
pub type RichCompareOp = u8;

pub use abstract_op::{RICH_EQ, RICH_GE, RICH_GT, RICH_LE, RICH_LT, RICH_NE};

/// Dispatches a Python rich comparison and returns the raw result object
/// (a user dunder's return value passes through uncoerced, as in CPython).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_rich_compare(
	op: RichCompareOp,
	a: *mut PyObject,
	b: *mut PyObject,
	feedback: *mut FeedbackCell,
) -> *mut PyObject {
	crate::untag_prelude!(a, b);
	unsafe { super::record_feedback_binary(feedback, a, b) };
	super::catch_object_helper(|| unsafe { abstract_op::rich_compare(op, a, b) })
}

/// Computes Python truth.  Returns `1`, `0`, or `-1` with the current
/// exception.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_is_true(object: *mut PyObject) -> i32 {
	crate::untag_prelude!(err = -1; object);
	super::catch_status_helper(|| unsafe { abstract_op::is_true(object) })
}

/// Loads an interned-name attribute through the object's attribute slot.
///
/// A non-NULL `feedback` cell enables the J0.3 AttrIC fast path for stock
/// `generic_get_attr` receivers (see `abi::attr::get_attr_dispatch`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_get_attr(
	object: *mut PyObject,
	name: u32,
	feedback: *mut FeedbackCell,
) -> *mut PyObject {
	crate::untag_prelude!(object);
	super::catch_object_helper(|| unsafe { super::attr::get_attr_dispatch(object, name, feedback) })
}

/// Stores an interned-name attribute through the object's attribute slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_set_attr(
	object: *mut PyObject,
	name: u32,
	value: *mut PyObject,
) -> i32 {
	crate::untag_prelude!(err = -1; object, value);
	super::catch_status_helper(|| unsafe { abstract_op::set_attr(object, name, value) })
}

/// Deletes an interned-name attribute through the object's attribute slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_del_attr(object: *mut PyObject, name: u32) -> i32 {
	crate::untag_prelude!(err = -1; object);
	super::catch_status_helper(|| unsafe { abstract_op::del_attr(object, name) })
}

/// Loads a subscription through mapping or sequence slots when available.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_subscript_get(
	object: *mut PyObject,
	key: *mut PyObject,
	feedback: *mut FeedbackCell,
) -> *mut PyObject {
	crate::untag_prelude!(object, key);
	unsafe { super::record_feedback_binary(feedback, object, key) };
	super::catch_object_helper(|| unsafe { abstract_op::subscript_get(object, key) })
}
