//! Range sequence implementation.

use core::mem::offset_of;

use crate::object::PyObjectHeader;

/// Boxed Python `range` storing normalized integer bounds.
#[repr(C)]
#[derive(Debug)]
pub struct PyRange {
	/// Common object header; this field must remain first.
	pub ob_base: PyObjectHeader,
	/// First value yielded by the range.
	pub start:   i64,
	/// Exclusive stop bound used to construct the range.
	pub stop:    i64,
	/// Non-zero step.
	pub step:    i64,
	/// Number of values in the range.
	pub len:     usize,
}

/// Ranges contain no outgoing managed references.
///
/// # Safety
///
/// Accepts NULL or a live [`PyRange`] allocation.
pub unsafe extern "C" fn trace_range(_object: *mut u8, _visitor: &mut dyn FnMut(*mut u8)) {}

const _: () = {
	assert!(offset_of!(PyRange, ob_base) == 0);
};
