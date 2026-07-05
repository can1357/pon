//! Attribute access helper family namespace.

use core::ptr;

use crate::{abstract_op, descr, feedback::FeedbackCell, object::PyObject};

/// Interned attribute-name id carried through the helper ABI.
pub type AttrName = u32;

/// Loads an attribute by interned name, using generic descriptor semantics
/// behind the receiver type's `tp_getattro` slot.
///
/// With a non-NULL `feedback` cell and a receiver whose `tp_getattro` is the
/// stock [`descr::generic_get_attr`], this dispatches straight to the
/// IC-aware core (J0.3 tier-0 consultation) — skipping the name-object
/// round-trip the generic slot path pays.  Custom `tp_getattro` slots keep
/// the plain dispatch and never see the cell.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_load_attr(
	object: *mut PyObject,
	name: AttrName,
	feedback: *mut FeedbackCell,
) -> *mut PyObject {
	crate::untag_prelude!(object);
	super::catch_object_helper(|| unsafe { get_attr_dispatch(object, name, feedback) })
}

/// Shared IC-or-generic attribute dispatch for `pon_load_attr`/`pon_get_attr`.
///
/// # Safety
///
/// `object` must be NULL or a live boxed object pointer.
pub(super) unsafe fn get_attr_dispatch(
	object: *mut PyObject,
	name: AttrName,
	feedback: *mut FeedbackCell,
) -> *mut PyObject {
	if !feedback.is_null() && !object.is_null() {
		let ty = unsafe { (*object).ob_type };
		// fn_addr_eq: a false negative merely skips the IC (safe); types
		// assign this slot from the same crate-local item, so the addresses
		// agree in practice.
		if !ty.is_null()
			&& unsafe { (*ty).tp_getattro }.is_some_and(|slot| {
				core::ptr::fn_addr_eq(slot, descr::generic_get_attr as unsafe extern "C" fn(_, _) -> _)
			}) {
			return unsafe { descr::generic_get_attr_cached(object, name, feedback) };
		}
	}
	unsafe { abstract_op::get_attr(object, name) }
}

/// Stores an attribute by interned name and returns the stored value on
/// success.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_store_attr(
	object: *mut PyObject,
	name: AttrName,
	value: *mut PyObject,
) -> *mut PyObject {
	crate::untag_prelude!(object, value);
	super::catch_object_helper(|| {
		if value.is_null() {
			return super::return_null_with_error("cannot store NULL attribute value");
		}
		if unsafe { abstract_op::set_attr(object, name, value) } < 0 {
			ptr::null_mut()
		} else {
			value
		}
	})
}

/// Deletes an attribute by interned name and returns `None` on success.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_delete_attr(object: *mut PyObject, name: AttrName) -> *mut PyObject {
	crate::untag_prelude!(object);
	super::catch_object_helper(|| {
		if unsafe { abstract_op::del_attr(object, name) } < 0 {
			ptr::null_mut()
		} else {
			unsafe { super::pon_none() }
		}
	})
}

/// Tier-0 method load.  Descriptor binding is already performed by attribute
/// lookup, so this helper returns the callable object directly; later tiers can
/// replace it with a receiver-pair specialization without changing the IR
/// shape.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_load_method(
	object: *mut PyObject,
	name: AttrName,
	feedback: *mut FeedbackCell,
) -> *mut PyObject {
	crate::untag_prelude!(object);
	unsafe { pon_load_attr(object, name, feedback) }
}

/// Core `isinstance` hook for builtin wiring.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_isinstance(object: *mut PyObject, cls: *mut PyObject) -> i32 {
	crate::untag_prelude!(err = -1; object, cls);
	super::catch_status_helper(|| unsafe { descr::isinstance(object, cls) })
}

/// Core `issubclass` hook for builtin wiring.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_issubclass(cls: *mut PyObject, base: *mut PyObject) -> i32 {
	crate::untag_prelude!(err = -1; cls, base);
	super::catch_status_helper(|| unsafe { descr::issubclass(cls, base) })
}
