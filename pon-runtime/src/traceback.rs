//! Boxed Python traceback objects for NULL-sentinel exception paths.
//!
//! Raising helpers in `abi::exc` snapshot the active Python call stack into a
//! `tb_next`-linked `PyTraceback` chain and store it on the exception instance
//! (`PyBaseException.traceback`), so `exc.__traceback__` observes CPython's
//! None-or-chain contract.  `tb_lineno` is real at statement granularity: the
//! raise-site entry carries the live `pon_current_line` value and outer
//! entries the call-site line saved by `CurrentFunctionGuard` (see
//! `abi::exc::attach_current_traceback` for the precision contract).

use core::{mem, ptr};
use std::sync::{LazyLock, Mutex};

use pon_gc::TypeId;

use crate::{
	object::{PyObject, PyObjectHeader, PyType},
	thread_state::pon_err_set,
};

/// GC type id reserved for boxed traceback objects; sits in the WS-GEN frame
/// family next to `crate::types::frame::TYPE_ID_FRAME`.
pub(crate) const TYPE_ID_TRACEBACK: TypeId = TypeId(34);

/// Boxed traceback chain entry mirroring CPython's `tb_next`-linked layout.
///
/// The head of a chain is the outermost observed frame; `tb_next` descends
/// toward the raise site (CPython's most-recent-call-last order).
#[repr(C)]
pub(crate) struct PyTraceback {
	ob_base:            PyObjectHeader,
	/// Next (deeper, toward the raise site) entry, or NULL at the raise site.
	pub(crate) tb_next: *mut PyObject,
	/// Frame observed for this entry; non-NULL by construction.
	pub(crate) frame:   *mut PyObject,
	/// 1-based statement-level source line of this entry; `0` when unknown.
	pub(crate) lineno:  i64,
}

impl PyTraceback {
	/// Builds a chain-entry payload for placement into runtime-allocated memory.
	#[must_use]
	pub(crate) fn new(
		ty: *const PyType,
		frame: *mut PyObject,
		lineno: i64,
		tb_next: *mut PyObject,
	) -> Self {
		Self { ob_base: PyObjectHeader::new(ty), tb_next, frame, lineno }
	}
}

static TRACEBACK_TYPE: LazyLock<Mutex<Option<usize>>> = LazyLock::new(|| Mutex::new(None));

/// Returns the process-lifetime traceback type object, creating it if needed.
pub(crate) fn ensure_traceback_type(type_type: *mut PyType) -> *mut PyType {
	let mut slot = TRACEBACK_TYPE
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	if let Some(existing) = *slot {
		return existing as *mut PyType;
	}

	let mut ty = PyType::new(type_type.cast_const(), "traceback", mem::size_of::<PyTraceback>());
	ty.tp_getattro = Some(traceback_getattro);
	ty.tp_setattro = Some(traceback_setattro);
	ty.gc_type_id = TYPE_ID_TRACEBACK.0 as usize;
	let ptr = Box::into_raw(Box::new(ty));
	*slot = Some(ptr as usize);
	ptr
}

/// Serves `tb_frame`/`tb_next`/`tb_lineno` on traceback chain entries.
unsafe extern "C" fn traceback_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(name) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		pon_err_set("traceback attribute name must be str");
		return ptr::null_mut();
	};
	// SAFETY: The runtime dispatches this slot only for PyTraceback instances.
	let entry = unsafe { &*object.cast::<PyTraceback>() };
	match name {
		"tb_frame" => entry.frame,
		"tb_next" => {
			if entry.tb_next.is_null() {
				unsafe { crate::abi::pon_none() }
			} else {
				entry.tb_next
			}
		},
		"tb_lineno" => unsafe { crate::abi::pon_const_int(entry.lineno) },
		// No bytecode exists, so no instruction index: -1 routes
		// `traceback._get_code_position` to its tb_lineno fallback without
		// ever touching `co_positions()`.
		"tb_lasti" => unsafe { crate::abi::pon_const_int(-1) },
		_ => unsafe { crate::abi::pon_raise_attribute_error(object, crate::intern::intern(name)) },
	}
}

/// Allows `tb_next = None | <traceback>` (unittest's `TestResult.
/// _remove_unittest_tb_frames` truncates chains this way); other attributes
/// and value types keep CPython's TypeError contract.
unsafe extern "C" fn traceback_setattro(
	object: *mut PyObject,
	name: *mut PyObject,
	value: *mut PyObject,
) -> core::ffi::c_int {
	let Some(name) = (unsafe { crate::types::type_::unicode_text(crate::tag::untag_arg(name)) })
	else {
		pon_err_set("traceback attribute name must be str");
		return -1;
	};
	if name != "tb_next" {
		pon_err_set(format!("traceback attribute '{name}' is not writable"));
		return -1;
	}
	let value = crate::tag::untag_arg(value);
	// SAFETY: The runtime dispatches this slot only for PyTraceback instances.
	let entry = unsafe { &mut *object.cast::<PyTraceback>() };
	let stored =
		if value.is_null() || unsafe { crate::types::dict::type_name(value) } == Some("NoneType") {
			ptr::null_mut()
		} else if unsafe { (*value).ob_type } == entry.ob_base.ob_type {
			value
		} else {
			pon_err_set("tb_next must be a traceback or None");
			return -1;
		};
	entry.tb_next = stored;
	0
}

/// Traces a boxed traceback entry for the runtime GC.
///
/// # Safety
///
/// `object` must be NULL or point at a live `PyTraceback` allocation.
pub(crate) unsafe extern "C" fn trace_traceback(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
	if object.is_null() {
		return;
	}
	// SAFETY: The GC registered this callback only for `PyTraceback` allocations.
	let entry = unsafe { &*object.cast::<PyTraceback>() };
	if !entry.tb_next.is_null() {
		visitor(entry.tb_next.cast::<u8>());
	}
	if !entry.frame.is_null() {
		visitor(entry.frame.cast::<u8>());
	}
}
