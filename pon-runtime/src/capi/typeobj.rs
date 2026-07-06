//! typeobj family: `PyType_Ready` and C-defined type instantiation.
//!
//! A C extension's static `PyTypeObject` (foreign, see [`super::twin`]) is
//! translated into a native runtime type by [`capi_type_ready`]:
//! - methods/getset/members/doc become descriptors in a native class dict,
//! - CPython-signature slots (`tp_repr`, `tp_hash`, `tp_call`, `tp_init`, ...)
//!   are bridged pointer-for-pointer (the ABI shapes match),
//! - `tp_new` goes through a trampoline that hands the C function its FOREIGN
//!   type pointer,
//! - instances live on the GC heap ([`TYPE_ID_CAPI_INSTANCE`]) with the C
//!   layout padded up to any required Pon builtin prefix (`max(tp_basicsize,
//!   runtime_prefix) + nitems * tp_itemsize`); `tp_dealloc` is bridged through
//!   the GC's deferred finalizer (objects stay valid for the whole finalization
//!   cycle; reclamation happens a cycle later).
//!
//! Resurrection contract (CPython parity): a `tp_dealloc` that releases its
//! own payload and then keeps the object alive produces a valid-but-torn-down
//! object, exactly as on CPython. The GC layer itself stays sound.
//!
//! GC-tracked C types (`Py_TPFLAGS_HAVE_GC`) are accepted: their
//! `tp_traverse` slots are bridged into Pon's tracer, while `tp_clear` is
//! intentionally ignored (Pon's tracing collector does not need C-level
//! cycle breaking).

use core::{
	ffi::{c_char, c_int, c_uint, c_void},
	mem, ptr,
};
use std::{
	collections::HashSet,
	ffi::CString,
	sync::{LazyLock, Mutex},
};

use pon_gc::{GcTypeInfo, TypeId};

use super::{
	c_string,
	twin::{self, ForeignTypeObject},
};
use crate::{
	abi,
	intern::intern,
	object::{
		BinaryFunc, InitFunc, InquiryFunc, LenFunc, ObjObjArgProc, ObjObjProc, PyMappingMethods,
		PyNumberMethods, PyObject, PyObjectHeader, PySequenceMethods, PyType, PyUnicode,
		SSizeArgFunc, SSizeObjArgProc, TernaryFunc, TraverseFunc, UnaryFunc, as_object_ptr,
	},
	types::{
		exc::ExceptionKind,
		type_::{PyClassDict, new_namespace},
	},
};

/// GC type id for C-extension instances (registry: fixed ids live in
/// `abi::register_gc_types` and per-module constants; 140 sits next to the
/// native-file id 120 and the carrier id 141 in `capi::mod`).
const TYPE_ID_CAPI_INSTANCE: TypeId = TypeId(140);

// CPython flag bits mirrored in include/Python.h.
const TPFLAGS_READY: u64 = 1 << 12;
const TPFLAGS_HAVE_GC: u64 = 1 << 14;

// CPython stable-ABI slot ids mirrored in include/Python.h/typeslots.h.
const PY_BF_GETBUFFER: c_int = 1;
const PY_BF_RELEASEBUFFER: c_int = 2;
const PY_MP_ASS_SUBSCRIPT: c_int = 3;
const PY_MP_LENGTH: c_int = 4;
const PY_MP_SUBSCRIPT: c_int = 5;
const PY_NB_ABSOLUTE: c_int = 6;
const PY_NB_ADD: c_int = 7;
const PY_NB_AND: c_int = 8;
const PY_NB_BOOL: c_int = 9;
const PY_NB_DIVMOD: c_int = 10;
const PY_NB_FLOAT: c_int = 11;
const PY_NB_FLOOR_DIVIDE: c_int = 12;
const PY_NB_INDEX: c_int = 13;
const PY_NB_INPLACE_ADD: c_int = 14;
const PY_NB_INPLACE_AND: c_int = 15;
const PY_NB_INPLACE_FLOOR_DIVIDE: c_int = 16;
const PY_NB_INPLACE_LSHIFT: c_int = 17;
const PY_NB_INPLACE_MULTIPLY: c_int = 18;
const PY_NB_INPLACE_OR: c_int = 19;
const PY_NB_INPLACE_POWER: c_int = 20;
const PY_NB_INPLACE_REMAINDER: c_int = 21;
const PY_NB_INPLACE_RSHIFT: c_int = 22;
const PY_NB_INPLACE_SUBTRACT: c_int = 23;
const PY_NB_INPLACE_TRUE_DIVIDE: c_int = 24;
const PY_NB_INPLACE_XOR: c_int = 25;
const PY_NB_INT: c_int = 26;
const PY_NB_INVERT: c_int = 27;
const PY_NB_LSHIFT: c_int = 28;
const PY_NB_MULTIPLY: c_int = 29;
const PY_NB_NEGATIVE: c_int = 30;
const PY_NB_OR: c_int = 31;
const PY_NB_POSITIVE: c_int = 32;
const PY_NB_POWER: c_int = 33;
const PY_NB_REMAINDER: c_int = 34;
const PY_NB_RSHIFT: c_int = 35;
const PY_NB_SUBTRACT: c_int = 36;
const PY_NB_TRUE_DIVIDE: c_int = 37;
const PY_NB_XOR: c_int = 38;
const PY_SQ_ASS_ITEM: c_int = 39;
const PY_SQ_CONCAT: c_int = 40;
const PY_SQ_CONTAINS: c_int = 41;
const PY_SQ_INPLACE_CONCAT: c_int = 42;
const PY_SQ_INPLACE_REPEAT: c_int = 43;
const PY_SQ_ITEM: c_int = 44;
const PY_SQ_LENGTH: c_int = 45;
const PY_SQ_REPEAT: c_int = 46;
const PY_TP_ALLOC: c_int = 47;
const PY_TP_BASE: c_int = 48;
const PY_TP_BASES: c_int = 49;
const PY_TP_CALL: c_int = 50;
const PY_TP_CLEAR: c_int = 51;
const PY_TP_DEALLOC: c_int = 52;
const PY_TP_DEL: c_int = 53;
const PY_TP_DESCR_GET: c_int = 54;
const PY_TP_DESCR_SET: c_int = 55;
const PY_TP_DOC: c_int = 56;
const PY_TP_GETATTR: c_int = 57;
const PY_TP_GETATTRO: c_int = 58;
const PY_TP_HASH: c_int = 59;
const PY_TP_INIT: c_int = 60;
const PY_TP_IS_GC: c_int = 61;
const PY_TP_ITER: c_int = 62;
const PY_TP_ITERNEXT: c_int = 63;
const PY_TP_METHODS: c_int = 64;
const PY_TP_NEW: c_int = 65;
const PY_TP_REPR: c_int = 66;
const PY_TP_RICHCOMPARE: c_int = 67;
const PY_TP_SETATTR: c_int = 68;
const PY_TP_SETATTRO: c_int = 69;
const PY_TP_STR: c_int = 70;
const PY_TP_TRAVERSE: c_int = 71;
const PY_TP_MEMBERS: c_int = 72;
const PY_TP_GETSET: c_int = 73;
const PY_TP_FREE: c_int = 74;
const PY_NB_MATRIX_MULTIPLY: c_int = 75;
const PY_NB_INPLACE_MATRIX_MULTIPLY: c_int = 76;
const PY_AM_AWAIT: c_int = 77;
const PY_AM_ANEXT: c_int = 79;
const PY_TP_FINALIZE: c_int = 80;
const PY_AM_SEND: c_int = 81;
const PY_TP_VECTORCALL: c_int = 82;
const PY_TP_TOKEN: c_int = 83;

#[repr(C)]
struct PyTypeSlot {
	slot:  c_int,
	pfunc: *mut c_void,
}

#[repr(C)]
struct PyTypeSpec {
	name:      *const c_char,
	basicsize: c_int,
	itemsize:  c_int,
	flags:     c_uint,
	slots:     *mut PyTypeSlot,
}

#[repr(C)]
struct PyBufferProcs {
	bf_getbuffer:     *mut c_void,
	bf_releasebuffer: *mut c_void,
}

/// C-facing `PyNumberMethods`: exact CPython 3.14 `object.h` field order.
/// Do not reuse [`PyNumberMethods`]: Pon's native table adds reflected slots.
#[repr(C)]
struct CNumberMethods {
	nb_add: *mut (),
	nb_subtract: *mut (),
	nb_multiply: *mut (),
	nb_remainder: *mut (),
	nb_divmod: *mut (),
	nb_power: *mut (),
	nb_negative: *mut (),
	nb_positive: *mut (),
	nb_absolute: *mut (),
	nb_bool: *mut (),
	nb_invert: *mut (),
	nb_lshift: *mut (),
	nb_rshift: *mut (),
	nb_and: *mut (),
	nb_xor: *mut (),
	nb_or: *mut (),
	nb_int: *mut (),
	nb_reserved: *mut (),
	nb_float: *mut (),
	nb_inplace_add: *mut (),
	nb_inplace_subtract: *mut (),
	nb_inplace_multiply: *mut (),
	nb_inplace_remainder: *mut (),
	nb_inplace_power: *mut (),
	nb_inplace_lshift: *mut (),
	nb_inplace_rshift: *mut (),
	nb_inplace_and: *mut (),
	nb_inplace_xor: *mut (),
	nb_inplace_or: *mut (),
	nb_floor_divide: *mut (),
	nb_true_divide: *mut (),
	nb_inplace_floor_divide: *mut (),
	nb_inplace_true_divide: *mut (),
	nb_index: *mut (),
	nb_matrix_multiply: *mut (),
	nb_inplace_matrix_multiply: *mut (),
}

/// C-facing `PySequenceMethods`: exact CPython 3.14 `object.h` field order.
/// Pon's native table intentionally differs for repeat slots, so translate.
#[repr(C)]
struct CSequenceMethods {
	sq_length:         *mut (),
	sq_concat:         *mut (),
	sq_repeat:         *mut (),
	sq_item:           *mut (),
	was_sq_slice:      *mut (),
	sq_ass_item:       *mut (),
	was_sq_ass_slice:  *mut (),
	sq_contains:       *mut (),
	sq_inplace_concat: *mut (),
	sq_inplace_repeat: *mut (),
}

/// C-facing `PyMappingMethods`: exact CPython 3.14 `object.h` field order.
#[repr(C)]
struct CMappingMethods {
	mp_length:        *mut (),
	mp_subscript:     *mut (),
	mp_ass_subscript: *mut (),
}

/// Heap protocol tables allocated while materializing `PyType_FromSpec` types.
/// Static extension tables are not ours, so per-member inheritance only mutates
/// pointers recorded here.
static FROMSPEC_NUMBER_TABLES: LazyLock<Mutex<HashSet<usize>>> =
	LazyLock::new(|| Mutex::new(HashSet::new()));
static FROMSPEC_SEQUENCE_TABLES: LazyLock<Mutex<HashSet<usize>>> =
	LazyLock::new(|| Mutex::new(HashSet::new()));
static FROMSPEC_MAPPING_TABLES: LazyLock<Mutex<HashSet<usize>>> =
	LazyLock::new(|| Mutex::new(HashSet::new()));

/// Addresses of live C-extension instances allocated on the GC heap.
/// `PyObject_Free` must no-op for these (the GC owns the block); the
/// finalizer drops entries as objects die.
static CAPI_INSTANCES: LazyLock<Mutex<HashSet<usize>>> =
	LazyLock::new(|| Mutex::new(HashSet::new()));

/// C mirror: `include/pon_capi/typeobj.h` `PyPonCapiTypeObj`.
#[repr(C)]
pub(crate) struct PyPonCapiTypeObj {
	type_ready:                unsafe extern "C" fn(*mut ForeignTypeObject) -> c_int,
	generic_alloc:             unsafe extern "C" fn(*mut ForeignTypeObject, isize) -> *mut PyObject,
	generic_new:
		unsafe extern "C" fn(*mut ForeignTypeObject, *mut PyObject, *mut PyObject) -> *mut PyObject,
	is_subtype: unsafe extern "C" fn(*mut ForeignTypeObject, *mut ForeignTypeObject) -> c_int,
	object_free:               unsafe extern "C" fn(*mut c_void),
	object_init: unsafe extern "C" fn(*mut PyObject, *mut ForeignTypeObject) -> *mut PyObject,
	object_new_raw:            unsafe extern "C" fn(*mut ForeignTypeObject, isize) -> *mut PyObject,
	type_from_spec:            unsafe extern "C" fn(*mut PyTypeSpec) -> *mut PyObject,
	type_from_spec_with_bases: unsafe extern "C" fn(*mut PyTypeSpec, *mut PyObject) -> *mut PyObject,
	type_from_module_and_spec:
		unsafe extern "C" fn(*mut PyObject, *mut PyTypeSpec, *mut PyObject) -> *mut PyObject,
	type_modified:             unsafe extern "C" fn(*mut ForeignTypeObject),
	type_from_metaclass: unsafe extern "C" fn(
		*mut ForeignTypeObject,
		*mut PyObject,
		*mut PyTypeSpec,
		*mut PyObject,
	) -> *mut PyObject,
	generic_alias: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
}

unsafe impl Send for PyPonCapiTypeObj {}
unsafe impl Sync for PyPonCapiTypeObj {}

pub(crate) fn build() -> PyPonCapiTypeObj {
	PyPonCapiTypeObj {
		type_ready:                capi_type_ready,
		generic_alloc:             capi_generic_alloc,
		generic_new:               capi_generic_new,
		is_subtype:                capi_is_subtype,
		object_free:               capi_object_free,
		object_init:               capi_object_init,
		object_new_raw:            capi_object_new_raw,
		type_from_spec:            capi_type_from_spec,
		type_from_spec_with_bases: capi_type_from_spec_with_bases,
		type_from_module_and_spec: capi_type_from_module_and_spec,
		type_modified:             capi_type_modified,
		type_from_metaclass:       capi_type_from_metaclass,
		generic_alias:             crate::types::typealias::pon_make_generic_alias,
	}
}

fn new_reference(object: *mut PyObject) -> *mut PyObject {
	super::pin_new_reference(object)
}

fn transfer_new_reference_to_runtime(object: *mut PyObject) -> *mut PyObject {
	super::unpin_object(object);
	object
}

/// True when `ptr` is a live C-extension instance owned by the GC heap.
pub(crate) fn is_capi_instance(ptr: *mut c_void) -> bool {
	CAPI_INSTANCES
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.contains(&(ptr as usize))
}

/// True when `cls` was built by [`capi_type_ready`] (C-extension instance
/// layout). Constructor calls on such classes follow CPython `type_call`
/// semantics: `tp_new` only allocates; `tp_init` initializes and expects a
/// real (possibly empty) args tuple, never NULL.
///
/// # Safety
///
/// `cls` must be NULL or a live type object.
pub(crate) unsafe fn is_capi_class(cls: *const PyType) -> bool {
	// SAFETY: live per contract.
	!cls.is_null() && unsafe { (*cls).gc_type_id } == TYPE_ID_CAPI_INSTANCE.0 as usize
}

fn raise_type_error(message: impl AsRef<str>) {
	let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message.as_ref());
}

/// Reads an optional slot pointer from a foreign struct field.
///
/// # Safety
///
/// `F` must be the exact C function-pointer type the field was declared
/// with; foreign structs are written by extension code compiled against
/// `include/Python.h`, whose typedefs match the runtime slot ABI.
unsafe fn slot<F>(field: *mut ()) -> Option<F> {
	if field.is_null() {
		None
	} else {
		// SAFETY: caller contract — matching function-pointer type.
		Some(unsafe { core::mem::transmute_copy::<*mut (), F>(&field) })
	}
}

/// Minimum fixed prefix Pon runtime code may legitimately read for instances
/// whose C type derives from `base_native`.  C-origin instances own their C
/// trailing layout, but selected builtin ancestors (notably `str`) still have
/// runtime slots/helpers that may inspect their native prefix when a value is
/// routed through a builtin path.  Do not use every ancestor's `tp_basicsize`:
/// Pon's placeholder `object` type has an internal payload layout that object
/// subclasses do not embed.
unsafe fn required_runtime_prefix_size(mut base_native: *mut PyType) -> usize {
	let mut required = mem::size_of::<PyObjectHeader>();
	while !base_native.is_null() {
		let ty = unsafe { &*base_native };
		if ty.gc_type_id == TYPE_ID_CAPI_INSTANCE.0 as usize {
			required = required.max(ty.tp_basicsize);
		} else if ty.name() == "str" {
			required = required.max(mem::size_of::<PyUnicode>());
		}
		base_native = ty.tp_base;
	}
	required
}

unsafe fn derives_from_native_str(mut native: *mut PyType) -> bool {
	while !native.is_null() {
		if unsafe { (*native).name() == "str" } {
			return true;
		}
		native = unsafe { (*native).tp_base };
	}
	false
}

fn checked_capi_instance_size(
	foreign: &ForeignTypeObject,
	native: *mut PyType,
	nitems: isize,
) -> Result<usize, String> {
	let foreign_fixed = foreign.tp_basicsize.max(0) as usize;
	let native_fixed = if native.is_null() {
		mem::size_of::<PyObjectHeader>()
	} else {
		unsafe { (*native).tp_basicsize }
	};
	let fixed = foreign_fixed
		.max(native_fixed)
		.max(mem::size_of::<PyObjectHeader>());
	if foreign.tp_dictoffset > 0 {
		let dict_offset = foreign.tp_dictoffset as usize;
		let dict_end = dict_offset
			.checked_add(mem::size_of::<*mut PyObject>())
			.ok_or_else(|| "PyType_GenericAlloc: tp_dictoffset overflow".to_owned())?;
		if dict_end > fixed {
			return Err(format!(
				"PyType_GenericAlloc: tp_dictoffset {} exceeds fixed instance size {fixed}",
				foreign.tp_dictoffset
			));
		}
	}
	let count = nitems.max(0) as usize;
	let itemsize = foreign.tp_itemsize.max(0) as usize;
	let variable = itemsize
		.checked_mul(count)
		.ok_or_else(|| "PyType_GenericAlloc: variable-size item payload overflow".to_owned())?;
	fixed
		.checked_add(variable)
		.ok_or_else(|| "PyType_GenericAlloc: instance size overflow".to_owned())
}

/// `PyPonCapiTypeObj.type_ready`.
pub(crate) unsafe extern "C" fn capi_type_ready(foreign: *mut ForeignTypeObject) -> c_int {
	if foreign.is_null() {
		raise_type_error("PyType_Ready(NULL)");
		return -1;
	}
	// SAFETY: live foreign static handed by extension code.
	let foreign_ref = unsafe { &mut *foreign };
	if foreign_ref.tp_flags & TPFLAGS_READY != 0 {
		return 0;
	}
	if !abi::runtime_is_initialized() {
		raise_type_error("PyType_Ready before runtime initialization");
		return -1;
	}
	let Some(name_full) = c_string(foreign_ref.tp_name) else {
		raise_type_error("PyType_Ready: tp_name is NULL");
		return -1;
	};
	// CPython: type.__name__ is the segment after the last dot.
	let name = name_full.rsplit('.').next().unwrap_or(&name_full);

	// `tp_clear` is intentionally not bridged. Pon's tracing GC never asks C
	// types to break cycles manually; finalization order and deferred-free
	// safety are handled by the collector.
	let has_gc = foreign_ref.tp_flags & TPFLAGS_HAVE_GC != 0;
	let metaclass_native = if foreign_ref.ob_type.is_null() {
		abi::runtime_type_type()
	} else {
		// CPython readies an unready metatype on demand.
		if twin::registered_native_of_foreign(foreign_ref.ob_type).is_none()
			&& unsafe { capi_type_ready(foreign_ref.ob_type) } != 0
		{
			return -1;
		}
		let Some(meta) = twin::registered_native_of_foreign(foreign_ref.ob_type) else {
			raise_type_error(format!("PyType_Ready: metatype of {name_full} is not ready"));
			return -1;
		};
		if unsafe { !crate::mro::is_subtype(meta, abi::runtime_type_type()) } {
			raise_type_error(format!(
				"PyType_Ready: metatype of {name_full} does not derive from type"
			));
			return -1;
		}
		meta
	};
	if metaclass_native.is_null() {
		raise_type_error("PyType_Ready: cannot resolve metatype");
		return -1;
	}

	// Base resolution: NULL means `object`.
	let base_native = if foreign_ref.tp_base.is_null() {
		crate::native::builtins_mod::builtin_native_type("object").unwrap_or(ptr::null_mut())
	} else {
		// CPython `PyType_Ready` recursively readies an unready tp_base
		// (numpy readies DType subclasses before their descriptor base).
		if twin::native_of_foreign(foreign_ref.tp_base).is_none()
			&& unsafe { capi_type_ready(foreign_ref.tp_base) } != 0
		{
			return -1;
		}
		match twin::native_of_foreign(foreign_ref.tp_base) {
			Some(native) => native,
			None => {
				raise_type_error(format!("PyType_Ready: base type of {name_full} is not ready"));
				return -1;
			},
		}
	};
	if base_native.is_null() {
		raise_type_error("PyType_Ready: cannot resolve base type");
		return -1;
	}
	// CPython inheritance: sizes default to the base's.  Pon additionally
	// requires enough fixed prefix for any native builtin ancestor whose slots
	// may inspect the instance (for example `str`'s PyUnicode prefix).
	let required_prefix = unsafe { required_runtime_prefix_size(base_native) };
	if foreign_ref.tp_basicsize < 0 {
		raise_type_error(format!("PyType_Ready: tp_basicsize for {name_full} is negative"));
		return -1;
	}
	if foreign_ref.tp_itemsize < 0 {
		raise_type_error(format!("PyType_Ready: tp_itemsize for {name_full} is negative"));
		return -1;
	}
	if foreign_ref.tp_basicsize == 0 {
		foreign_ref.tp_basicsize = required_prefix as isize;
	}
	if (foreign_ref.tp_basicsize as usize) < required_prefix {
		let base_name = unsafe { (*base_native).name() };
		raise_type_error(format!(
			"PyType_Ready: tp_basicsize for {name_full} ({}) is smaller than Pon's required layout \
			 for base {base_name} ({required_prefix})",
			foreign_ref.tp_basicsize
		));
		return -1;
	}
	// CPython `inherit_slots` for the supported C-to-C single-base case:
	// slots the child leaves NULL surface the base's, so the bridging and
	// backfill below see the effective values (a foreign base was validated
	// Ready above, so its own backfill already ran). `tp_new` is inherited
	// by the existing backfill at the end.
	if !foreign_ref.tp_base.is_null() {
		// SAFETY: ready base statics stay live for the process; the base is
		// a distinct object (a self-base would have failed MRO validation on
		// its own PyType_Ready).
		let base = unsafe { &*foreign_ref.tp_base };
		let inherited = [
			(&mut foreign_ref.tp_dealloc, base.tp_dealloc),
			(&mut foreign_ref.tp_repr, base.tp_repr),
			(&mut foreign_ref.tp_str, base.tp_str),
			(&mut foreign_ref.tp_hash, base.tp_hash),
			(&mut foreign_ref.tp_call, base.tp_call),
			(&mut foreign_ref.tp_richcompare, base.tp_richcompare),
			(&mut foreign_ref.tp_iter, base.tp_iter),
			(&mut foreign_ref.tp_iternext, base.tp_iternext),
			(&mut foreign_ref.tp_getattro, base.tp_getattro),
			(&mut foreign_ref.tp_setattro, base.tp_setattro),
			(&mut foreign_ref.tp_descr_get, base.tp_descr_get),
			(&mut foreign_ref.tp_descr_set, base.tp_descr_set),
			(&mut foreign_ref.tp_as_buffer, base.tp_as_buffer),
			(&mut foreign_ref.tp_init, base.tp_init),
			(&mut foreign_ref.tp_alloc, base.tp_alloc),
			(&mut foreign_ref.tp_free, base.tp_free),
			(&mut foreign_ref.tp_traverse, base.tp_traverse),
			(&mut foreign_ref.tp_clear, base.tp_clear),
		];
		for (child, parent) in inherited {
			if child.is_null() {
				*child = parent;
			}
		}
		// Protocol table inheritance: when the child has no table, inherit the
		// base table wholesale. If a PyType_FromSpec child owns a heap table,
		// copy only missing members into it; extension-owned static child
		// tables are left untouched because Pon does not own their storage.
		unsafe { inherit_foreign_protocol_tables(foreign_ref, base) };
	}

	let namespace = new_namespace();
	// SAFETY: fresh namespace; carrier construction only allocates.
	unsafe {
		if !install_namespace(namespace, foreign_ref) {
			return -1;
		}
	}

	// SAFETY: live metaclass/base type, live namespace, runtime initialized.
	let runtime_basicsize = (foreign_ref.tp_basicsize as usize).max(required_prefix);
	let native = unsafe {
		crate::types::type_::construct_capi_class(
			metaclass_native,
			name,
			&[base_native],
			namespace,
			runtime_basicsize,
			foreign_ref.tp_itemsize,
			TYPE_ID_CAPI_INSTANCE.0 as usize,
		)
	};
	if native.is_null() {
		return -1;
	}
	let native_ty = native.cast::<PyType>();

	// Slot bridging: CPython slot ABIs match the runtime's slot typedefs
	// one-for-one, so foreign function pointers install directly. `tp_new`
	// is the exception (its first argument is the FOREIGN type) and runs
	// through the trampoline below.
	// SAFETY: `native_ty` is the freshly constructed live type.
	unsafe {
		let ty = &mut *native_ty;
		ty.tp_new = Some(capi_tp_new_trampoline);
		ty.tp_init = slot::<InitFunc>(foreign_ref.tp_init);
		ty.capi_tp_traverse = if has_gc || !foreign_ref.tp_traverse.is_null() {
			slot::<TraverseFunc>(foreign_ref.tp_traverse)
		} else {
			None
		};
		ty.tp_flags = foreign_ref.tp_flags as usize;
		ty.tp_dictoffset = if foreign_ref.tp_dictoffset > 0 {
			foreign_ref.tp_dictoffset
		} else {
			0
		};
		if let Some(repr) = slot(foreign_ref.tp_repr) {
			ty.tp_repr = Some(repr);
		}
		if let Some(str_slot) = slot(foreign_ref.tp_str) {
			ty.tp_str = Some(str_slot);
		}
		if let Some(hash) = slot(foreign_ref.tp_hash) {
			ty.tp_hash = Some(hash);
		}
		if let Some(call) = slot(foreign_ref.tp_call) {
			ty.tp_call = if foreign_ref.tp_vectorcall_offset > 0
			{
				Some(crate::capi::object_::capi_vectorcall_call)
			} else {
				Some(call)
			};
		}
		if let Some(richcmp) = slot(foreign_ref.tp_richcompare) {
			ty.tp_richcmp = Some(richcmp);
		}
		if let Some(iter) = slot(foreign_ref.tp_iter) {
			ty.tp_iter = Some(iter);
		}
		if let Some(iternext) = slot(foreign_ref.tp_iternext) {
			ty.tp_iternext = Some(iternext);
		}
		if let Some(getattro) = slot(foreign_ref.tp_getattro) {
			ty.tp_getattro = Some(getattro);
		}
		if let Some(setattro) = slot(foreign_ref.tp_setattro) {
			ty.tp_setattro = Some(setattro);
		}
		if let Some(descr_get) = slot(foreign_ref.tp_descr_get) {
			ty.tp_descr_get = Some(descr_get);
		}
		if let Some(descr_set) = slot(foreign_ref.tp_descr_set) {
			ty.tp_descr_set = Some(descr_set);
		}
		install_native_protocol_tables(ty, foreign_ref);
		ty.bump_version();
	}

	// Publish the twin BEFORE filling the foreign back-references so
	// trampolines can translate from either side.
	twin::register_foreign_twin(foreign, native_ty);

	// Fill the foreign struct's runtime-owned fields (CPython PyType_Ready
	// parity: inherited slots surface in the static struct).
	foreign_ref.tp_pon_twin = native_ty;
	if foreign_ref.ob_type.is_null() {
		foreign_ref.ob_type = twin::foreign_of_native(abi::runtime_type_type());
	}
	if !foreign_ref.tp_dict.is_null()
		&& !unsafe { crate::types::type_::merge_tp_dict_into_class(native_ty, foreign_ref.tp_dict) }
	{
		return -1;
	}
	let tp_dict = unsafe { crate::types::type_::new_class_dict_view(native_ty) };
	if tp_dict.is_null() {
		return -1;
	}
	super::pin_object(tp_dict);
	foreign_ref.tp_dict = tp_dict;
	if foreign_ref.tp_base.is_null() {
		// CPython sets a NULL tp_base to `&PyBaseObject_Type` on ready.
		foreign_ref.tp_base = twin::foreign_of_native(base_native);
	}
	if foreign_ref.tp_bases.is_null() && !foreign_ref.tp_base.is_null() {
		// CPython parity: `tp_bases` holds the declared bases. Elements are
		// FOREIGN faces so C-side identity checks (`GET_ITEM(bases, 0) ==
		// &Base`) hold; the tuple itself is a pinned pon allocation the GC
		// traces (face elements classify as non-heap and are skipped). It is
		// C-face-only state and never crosses into pon code paths.
		let mut items = [foreign_ref.tp_base.cast::<PyObject>()];
		let bases = unsafe { abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) };
		if bases.is_null() {
			return -1;
		}
		super::pin_object(bases);
		foreign_ref.tp_bases = bases;
	}
	foreign_ref.tp_flags |= TPFLAGS_READY;
	if foreign_ref.tp_alloc.is_null() {
		foreign_ref.tp_alloc = capi_generic_alloc as *mut ();
	}
	if foreign_ref.tp_free.is_null() {
		foreign_ref.tp_free = capi_object_free as *mut ();
	}
	if foreign_ref.tp_new.is_null() {
		// Inherit the base's tp_new when it is a ready foreign type;
		// otherwise generic allocation (object.__new__ parity for C types).
		let base_new = if foreign_ref.tp_base.is_null() {
			ptr::null_mut()
		} else {
			// SAFETY: base twin was validated/ready above.
			unsafe { (*foreign_ref.tp_base).tp_new }
		};
		foreign_ref.tp_new = if base_new.is_null() {
			capi_generic_new as *mut ()
		} else {
			base_new
		};
	}
	0
}

/// `PyPonCapiTypeObj.type_from_spec` (`PyType_FromSpec`).
unsafe extern "C" fn capi_type_from_spec(spec: *mut PyTypeSpec) -> *mut PyObject {
	unsafe { type_from_metaclass_impl(ptr::null_mut(), ptr::null_mut(), spec, ptr::null_mut()) }
}

/// `PyType_Modified`: invalidates cached type state after C-side mutation
/// (numpy pokes docstrings/slots after PyType_Ready).
unsafe extern "C" fn capi_type_modified(foreign: *mut ForeignTypeObject) {
	if let Some(native) = twin::registered_native_of_foreign(foreign) {
		// `sync::type_modified` bumps the owner and subclass versions itself.
		crate::sync::type_modified(native);
	}
}

/// `PyPonCapiTypeObj.type_from_spec_with_bases` (`PyType_FromSpecWithBases`).
unsafe extern "C" fn capi_type_from_spec_with_bases(
	spec: *mut PyTypeSpec,
	bases: *mut PyObject,
) -> *mut PyObject {
	unsafe { type_from_metaclass_impl(ptr::null_mut(), ptr::null_mut(), spec, bases) }
}

/// `PyPonCapiTypeObj.type_from_module_and_spec` (`PyType_FromModuleAndSpec`).
///
/// Pon does not expose `PyType_GetModule`/module-state lookup yet; the module
/// argument is intentionally ignored while the C type itself is made ready.
unsafe extern "C" fn capi_type_from_module_and_spec(
	module: *mut PyObject,
	spec: *mut PyTypeSpec,
	bases: *mut PyObject,
) -> *mut PyObject {
	unsafe { type_from_metaclass_impl(ptr::null_mut(), module, spec, bases) }
}

/// `PyPonCapiTypeObj.type_from_metaclass` (`PyType_FromMetaclass`).
unsafe extern "C" fn capi_type_from_metaclass(
	metaclass: *mut ForeignTypeObject,
	module: *mut PyObject,
	spec: *mut PyTypeSpec,
	bases: *mut PyObject,
) -> *mut PyObject {
	unsafe { type_from_metaclass_impl(metaclass, module, spec, bases) }
}

/// Shared `PyType_Spec` materialization path. `module` is intentionally ignored
/// until Pon exposes module-state lookups, matching `PyType_FromModuleAndSpec`.
unsafe fn type_from_metaclass_impl(
	metaclass: *mut ForeignTypeObject,
	_module: *mut PyObject,
	spec: *mut PyTypeSpec,
	bases: *mut PyObject,
) -> *mut PyObject {
	if spec.is_null() {
		raise_type_error("PyType_FromSpec(NULL)");
		return ptr::null_mut();
	}
	// SAFETY: extension-owned spec pointer per C-API contract.
	let spec_ref = unsafe { &*spec };
	let Some(name_full) = c_string(spec_ref.name) else {
		raise_type_error("PyType_FromSpec: spec name is NULL");
		return ptr::null_mut();
	};
	if spec_ref.slots.is_null() {
		raise_type_error(format!("PyType_FromSpec: spec slots are NULL for {name_full}"));
		return ptr::null_mut();
	}

	// SAFETY: ForeignTypeObject is a POD C mirror; all-zero is NULL/0.
	let mut foreign: ForeignTypeObject = unsafe { core::mem::zeroed() };
	foreign.ob_type = metaclass;
	foreign.tp_basicsize = spec_ref.basicsize as isize;
	foreign.tp_itemsize = spec_ref.itemsize as isize;
	foreign.tp_flags = spec_ref.flags as u64;

	if unsafe { !apply_type_spec_slots(&mut foreign, spec_ref.slots, &name_full) } {
		return ptr::null_mut();
	}
	if !bases.is_null() && unsafe { !apply_type_spec_bases(&mut foreign, bases, &name_full) } {
		return ptr::null_mut();
	}

	let Ok(name_copy) = CString::new(name_full.as_bytes()) else {
		raise_type_error(format!("PyType_FromSpec: type name contains NUL: {name_full}"));
		return ptr::null_mut();
	};
	foreign.tp_name = name_copy.into_raw().cast_const();

	let foreign_ptr = Box::into_raw(Box::new(foreign));
	if unsafe { capi_type_ready(foreign_ptr) } < 0 {
		return ptr::null_mut();
	}
	new_reference(foreign_ptr.cast::<PyObject>())
}

unsafe fn apply_type_spec_slots(
	foreign: &mut ForeignTypeObject,
	slots: *mut PyTypeSlot,
	type_name: &str,
) -> bool {
	let mut cursor = slots;
	loop {
		// SAFETY: PyType_Spec slot arrays are 0-terminated by contract.
		let slot = unsafe { &*cursor };
		if slot.slot == 0 {
			return true;
		}
		let field = slot.pfunc.cast::<()>();
		match slot.slot {
			PY_BF_GETBUFFER => unsafe { ensure_buffer_procs(foreign).bf_getbuffer = slot.pfunc },
			PY_BF_RELEASEBUFFER => unsafe {
				ensure_buffer_procs(foreign).bf_releasebuffer = slot.pfunc
			},
			PY_MP_ASS_SUBSCRIPT => unsafe {
				ensure_fromspec_mapping_table(foreign).mp_ass_subscript = field
			},
			PY_MP_LENGTH => unsafe { ensure_fromspec_mapping_table(foreign).mp_length = field },
			PY_MP_SUBSCRIPT => unsafe { ensure_fromspec_mapping_table(foreign).mp_subscript = field },
			PY_NB_ABSOLUTE => unsafe { ensure_fromspec_number_table(foreign).nb_absolute = field },
			PY_NB_ADD => unsafe { ensure_fromspec_number_table(foreign).nb_add = field },
			PY_NB_AND => unsafe { ensure_fromspec_number_table(foreign).nb_and = field },
			PY_NB_BOOL => unsafe { ensure_fromspec_number_table(foreign).nb_bool = field },
			PY_NB_DIVMOD => unsafe { ensure_fromspec_number_table(foreign).nb_divmod = field },
			PY_NB_FLOAT => unsafe { ensure_fromspec_number_table(foreign).nb_float = field },
			PY_NB_FLOOR_DIVIDE => unsafe {
				ensure_fromspec_number_table(foreign).nb_floor_divide = field
			},
			PY_NB_INDEX => unsafe { ensure_fromspec_number_table(foreign).nb_index = field },
			PY_NB_INPLACE_ADD => unsafe {
				ensure_fromspec_number_table(foreign).nb_inplace_add = field
			},
			PY_NB_INPLACE_AND => unsafe {
				ensure_fromspec_number_table(foreign).nb_inplace_and = field
			},
			PY_NB_INPLACE_FLOOR_DIVIDE => unsafe {
				ensure_fromspec_number_table(foreign).nb_inplace_floor_divide = field
			},
			PY_NB_INPLACE_LSHIFT => unsafe {
				ensure_fromspec_number_table(foreign).nb_inplace_lshift = field
			},
			PY_NB_INPLACE_MULTIPLY => unsafe {
				ensure_fromspec_number_table(foreign).nb_inplace_multiply = field
			},
			PY_NB_INPLACE_OR => unsafe { ensure_fromspec_number_table(foreign).nb_inplace_or = field },
			PY_NB_INPLACE_POWER => unsafe {
				ensure_fromspec_number_table(foreign).nb_inplace_power = field
			},
			PY_NB_INPLACE_REMAINDER => unsafe {
				ensure_fromspec_number_table(foreign).nb_inplace_remainder = field
			},
			PY_NB_INPLACE_RSHIFT => unsafe {
				ensure_fromspec_number_table(foreign).nb_inplace_rshift = field
			},
			PY_NB_INPLACE_SUBTRACT => unsafe {
				ensure_fromspec_number_table(foreign).nb_inplace_subtract = field
			},
			PY_NB_INPLACE_TRUE_DIVIDE => unsafe {
				ensure_fromspec_number_table(foreign).nb_inplace_true_divide = field
			},
			PY_NB_INPLACE_XOR => unsafe {
				ensure_fromspec_number_table(foreign).nb_inplace_xor = field
			},
			PY_NB_INT => unsafe { ensure_fromspec_number_table(foreign).nb_int = field },
			PY_NB_INVERT => unsafe { ensure_fromspec_number_table(foreign).nb_invert = field },
			PY_NB_LSHIFT => unsafe { ensure_fromspec_number_table(foreign).nb_lshift = field },
			PY_NB_MATRIX_MULTIPLY => unsafe {
				ensure_fromspec_number_table(foreign).nb_matrix_multiply = field
			},
			PY_NB_MULTIPLY => unsafe { ensure_fromspec_number_table(foreign).nb_multiply = field },
			PY_NB_NEGATIVE => unsafe { ensure_fromspec_number_table(foreign).nb_negative = field },
			PY_NB_OR => unsafe { ensure_fromspec_number_table(foreign).nb_or = field },
			PY_NB_POSITIVE => unsafe { ensure_fromspec_number_table(foreign).nb_positive = field },
			PY_NB_POWER => unsafe { ensure_fromspec_number_table(foreign).nb_power = field },
			PY_NB_REMAINDER => unsafe { ensure_fromspec_number_table(foreign).nb_remainder = field },
			PY_NB_RSHIFT => unsafe { ensure_fromspec_number_table(foreign).nb_rshift = field },
			PY_NB_SUBTRACT => unsafe { ensure_fromspec_number_table(foreign).nb_subtract = field },
			PY_NB_TRUE_DIVIDE => unsafe {
				ensure_fromspec_number_table(foreign).nb_true_divide = field
			},
			PY_NB_XOR => unsafe { ensure_fromspec_number_table(foreign).nb_xor = field },
			PY_NB_INPLACE_MATRIX_MULTIPLY => unsafe {
				ensure_fromspec_number_table(foreign).nb_inplace_matrix_multiply = field
			},
			PY_SQ_ASS_ITEM => unsafe { ensure_fromspec_sequence_table(foreign).sq_ass_item = field },
			PY_SQ_CONCAT => unsafe { ensure_fromspec_sequence_table(foreign).sq_concat = field },
			PY_SQ_CONTAINS => unsafe { ensure_fromspec_sequence_table(foreign).sq_contains = field },
			PY_SQ_INPLACE_CONCAT => unsafe {
				ensure_fromspec_sequence_table(foreign).sq_inplace_concat = field
			},
			PY_SQ_INPLACE_REPEAT => unsafe {
				ensure_fromspec_sequence_table(foreign).sq_inplace_repeat = field
			},
			PY_SQ_ITEM => unsafe { ensure_fromspec_sequence_table(foreign).sq_item = field },
			PY_SQ_LENGTH => unsafe { ensure_fromspec_sequence_table(foreign).sq_length = field },
			PY_SQ_REPEAT => unsafe { ensure_fromspec_sequence_table(foreign).sq_repeat = field },
			PY_TP_ALLOC => foreign.tp_alloc = field,
			PY_TP_BASE => {
				if !apply_type_spec_base(
					foreign,
					slot.pfunc.cast::<ForeignTypeObject>(),
					type_name,
					"Py_tp_base",
				) {
					return false;
				}
			},
			PY_TP_BASES => {
				if unsafe { !apply_type_spec_bases(foreign, slot.pfunc.cast::<PyObject>(), type_name) }
				{
					return false;
				}
			},
			PY_TP_CALL => foreign.tp_call = field,
			PY_TP_CLEAR => foreign.tp_clear = field,
			PY_TP_DEALLOC => foreign.tp_dealloc = field,
			PY_TP_DESCR_GET => foreign.tp_descr_get = field,
			PY_TP_DESCR_SET => foreign.tp_descr_set = field,
			PY_TP_DOC => foreign.tp_doc = slot.pfunc.cast::<c_char>().cast_const(),
			PY_TP_GETATTRO => foreign.tp_getattro = field,
			PY_TP_HASH => foreign.tp_hash = field,
			PY_TP_INIT => foreign.tp_init = field,
			PY_TP_ITER => foreign.tp_iter = field,
			PY_TP_ITERNEXT => foreign.tp_iternext = field,
			PY_TP_METHODS => foreign.tp_methods = field,
			PY_TP_MEMBERS => foreign.tp_members = field,
			PY_TP_GETSET => foreign.tp_getset = field,
			PY_TP_NEW => foreign.tp_new = field,
			PY_TP_REPR => foreign.tp_repr = field,
			PY_TP_RICHCOMPARE => foreign.tp_richcompare = field,
			PY_TP_SETATTRO => foreign.tp_setattro = field,
			PY_TP_STR => foreign.tp_str = field,
			PY_TP_TRAVERSE => foreign.tp_traverse = field,
			PY_TP_FREE => foreign.tp_free = field,
			PY_TP_FINALIZE => foreign.tp_finalize = field,
			PY_TP_DEL
			| PY_TP_GETATTR
			| PY_TP_IS_GC
			| PY_TP_SETATTR
			| PY_TP_VECTORCALL
			| PY_TP_TOKEN
			| PY_AM_AWAIT..=PY_AM_ANEXT
			| PY_AM_SEND => {
				raise_type_error(format!(
					"PyType_FromSpec: slot id {} is not supported yet for {type_name}",
					slot.slot
				));
				return false;
			},
			_ => {
				raise_type_error(format!(
					"PyType_FromSpec: unknown slot id {} for {type_name}",
					slot.slot
				));
				return false;
			},
		}
		// SAFETY: 0-terminated slot array; cursor is advanced one element.
		cursor = unsafe { cursor.add(1) };
	}
}

unsafe fn ensure_fromspec_number_table(foreign: &mut ForeignTypeObject) -> &mut CNumberMethods {
	if foreign.tp_as_number.is_null() {
		// SAFETY: C protocol tables are plain nullable-pointer records.
		let table = Box::into_raw(Box::new(unsafe { core::mem::zeroed::<CNumberMethods>() }));
		FROMSPEC_NUMBER_TABLES
			.lock()
			.unwrap_or_else(|poison| poison.into_inner())
			.insert(table as usize);
		foreign.tp_as_number = table.cast::<()>();
	}
	// SAFETY: created above or supplied by an earlier Py_nb_* slot in this spec.
	unsafe { &mut *foreign.tp_as_number.cast::<CNumberMethods>() }
}

unsafe fn ensure_fromspec_sequence_table(foreign: &mut ForeignTypeObject) -> &mut CSequenceMethods {
	if foreign.tp_as_sequence.is_null() {
		// SAFETY: C protocol tables are plain nullable-pointer records.
		let table = Box::into_raw(Box::new(unsafe { core::mem::zeroed::<CSequenceMethods>() }));
		FROMSPEC_SEQUENCE_TABLES
			.lock()
			.unwrap_or_else(|poison| poison.into_inner())
			.insert(table as usize);
		foreign.tp_as_sequence = table.cast::<()>();
	}
	// SAFETY: created above or supplied by an earlier Py_sq_* slot in this spec.
	unsafe { &mut *foreign.tp_as_sequence.cast::<CSequenceMethods>() }
}

unsafe fn ensure_fromspec_mapping_table(foreign: &mut ForeignTypeObject) -> &mut CMappingMethods {
	if foreign.tp_as_mapping.is_null() {
		// SAFETY: C protocol tables are plain nullable-pointer records.
		let table = Box::into_raw(Box::new(unsafe { core::mem::zeroed::<CMappingMethods>() }));
		FROMSPEC_MAPPING_TABLES
			.lock()
			.unwrap_or_else(|poison| poison.into_inner())
			.insert(table as usize);
		foreign.tp_as_mapping = table.cast::<()>();
	}
	// SAFETY: created above or supplied by an earlier Py_mp_* slot in this spec.
	unsafe { &mut *foreign.tp_as_mapping.cast::<CMappingMethods>() }
}

unsafe fn ensure_buffer_procs(foreign: &mut ForeignTypeObject) -> &mut PyBufferProcs {
	if foreign.tp_as_buffer.is_null() {
		let table = Box::new(PyBufferProcs {
			bf_getbuffer:     ptr::null_mut(),
			bf_releasebuffer: ptr::null_mut(),
		});
		foreign.tp_as_buffer = Box::into_raw(table).cast::<()>();
	}
	// SAFETY: the field is either extension-owned `PyBufferProcs` storage or the
	// table allocated above.
	unsafe { &mut *foreign.tp_as_buffer.cast::<PyBufferProcs>() }
}

fn apply_type_spec_base(
	foreign: &mut ForeignTypeObject,
	base: *mut ForeignTypeObject,
	type_name: &str,
	source: &str,
) -> bool {
	if !base.is_null() && twin::registered_native_of_foreign(base).is_none() {
		raise_type_error(format!(
			"PyType_FromSpec: {source} for {type_name} is not a ready foreign PyTypeObject*"
		));
		return false;
	}
	foreign.tp_base = base;
	true
}

unsafe fn apply_type_spec_bases(
	foreign: &mut ForeignTypeObject,
	bases: *mut PyObject,
	type_name: &str,
) -> bool {
	if bases.is_null() {
		foreign.tp_bases = ptr::null_mut();
		foreign.tp_base = ptr::null_mut();
		return true;
	}
	let bases = crate::tag::untag_arg(bases);
	let Some(items) = (unsafe { abi::seq::exact_tuple_slice(bases) }) else {
		raise_type_error(format!(
			"PyType_FromSpec: Py_tp_bases for {type_name} must be an exact tuple"
		));
		return false;
	};
	if items.len() != 1 {
		raise_type_error(format!(
			"PyType_FromSpec: Py_tp_bases for {type_name} must contain exactly one base (got {})",
			items.len()
		));
		return false;
	}
	if !apply_type_spec_base(
		foreign,
		items[0].cast::<ForeignTypeObject>(),
		type_name,
		"Py_tp_bases[0]",
	) {
		return false;
	}
	foreign.tp_bases = bases;
	true
}

/// Builds the class-dict namespace from the foreign method/getset/member
/// tables. Returns false with an error set on malformed entries.
unsafe fn install_namespace(namespace: *mut PyClassDict, foreign: &ForeignTypeObject) -> bool {
	// SAFETY: fresh exclusive namespace.
	let ns = unsafe { &mut *namespace };
	if let Some(doc) = c_string(foreign.tp_doc) {
		let doc_object = unsafe { abi::pon_const_str(doc.as_ptr(), doc.len()) };
		if !doc_object.is_null() {
			ns.set(intern("__doc__"), doc_object);
		}
	}
	if !foreign.tp_methods.is_null() {
		let mut cursor = foreign.tp_methods.cast::<super::PyMethodDef>();
		// SAFETY: NULL-name terminated array per CPython contract.
		while !unsafe { (*cursor).ml_name }.is_null() {
			let method = unsafe { &*cursor };
			let Some(method_name) = c_string(method.ml_name) else {
				raise_type_error("PyType_Ready: method with invalid name");
				return false;
			};
			if method.ml_meth.is_none() {
				raise_type_error(format!("PyType_Ready: method '{method_name}' has no function"));
				return false;
			}
			let carrier =
				super::alloc_cfunction_from_method_def(cursor, ptr::null_mut(), &method_name);
			if carrier.is_null() {
				return false;
			}
			ns.set(intern(&method_name), carrier);
			cursor = unsafe { cursor.add(1) };
		}
	}
	if !foreign.tp_getset.is_null() {
		let mut cursor = foreign.tp_getset.cast::<CGetSetDef>();
		// SAFETY: NULL-name terminated array per CPython contract.
		while !unsafe { (*cursor).name }.is_null() {
			let def = unsafe { &*cursor };
			let Some(attr_name) = c_string(def.name) else {
				raise_type_error("PyType_Ready: getset with invalid name");
				return false;
			};
			let descriptor = alloc_getset_descriptor(def, &attr_name);
			if descriptor.is_null() {
				return false;
			}
			ns.set(intern(&attr_name), descriptor);
			cursor = unsafe { cursor.add(1) };
		}
	}
	if !foreign.tp_members.is_null() {
		let mut cursor = foreign.tp_members.cast::<CMemberDef>();
		// SAFETY: NULL-name terminated array per CPython contract.
		while !unsafe { (*cursor).name }.is_null() {
			let def = unsafe { &*cursor };
			let Some(attr_name) = c_string(def.name) else {
				raise_type_error("PyType_Ready: member with invalid name");
				return false;
			};
			let descriptor = alloc_member_descriptor(def, &attr_name);
			if descriptor.is_null() {
				return false;
			}
			ns.set(intern(&attr_name), descriptor);
			cursor = unsafe { cursor.add(1) };
		}
	}
	unsafe { install_slot_wrappers(ns, foreign).is_some() }
}

/// `tp_new` bridge: recovers the FOREIGN type for the native class and calls
/// its C `tp_new` (or generic allocation when the extension left it NULL).
unsafe extern "C" fn capi_tp_new_trampoline(
	cls: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	let Some(foreign) = twin::registered_foreign_of_native(cls) else {
		return abi::return_null_with_error("C type is not registered with the C-API layer");
	};
	// SAFETY: registered foreign statics stay live for the process.
	let tp_new = unsafe { (*foreign).tp_new };
	let result = if tp_new.is_null() || tp_new == capi_generic_new as *mut () {
		unsafe { capi_generic_alloc(foreign, 0) }
	} else {
		let new_fn: unsafe extern "C" fn(*mut ForeignTypeObject, *mut PyObject, *mut PyObject) -> *mut PyObject =
            // SAFETY: tp_new fields hold newfunc pointers by header contract.
            unsafe { core::mem::transmute(tp_new) };
		// CPython always hands tp_new a REAL args tuple (possibly empty);
		// NULL breaks PyArg_ParseTuple* inside C constructors.
		let args = if args.is_null() {
			let empty = unsafe { abi::seq::pon_build_tuple(ptr::null_mut(), 0) };
			if empty.is_null() {
				return ptr::null_mut();
			}
			empty
		} else {
			args
		};
		unsafe { new_fn(foreign, args, kwargs) }
	};
	transfer_new_reference_to_runtime(result)
}

/// `PyPonCapiTypeObj.generic_new` (`PyType_GenericNew`).
unsafe extern "C" fn capi_generic_new(
	foreign: *mut ForeignTypeObject,
	_args: *mut PyObject,
	_kwargs: *mut PyObject,
) -> *mut PyObject {
	unsafe { capi_generic_alloc(foreign, 0) }
}

/// `PyPonCapiTypeObj.generic_alloc` (`PyType_GenericAlloc`): zeroed C-layout
/// instance on the GC heap.  The allocation uses the larger of the C type's
/// declared `tp_basicsize` and the runtime prefix required by its resolved
/// native type, plus any variable-size item payload.
unsafe extern "C" fn capi_generic_alloc(
	foreign: *mut ForeignTypeObject,
	nitems: isize,
) -> *mut PyObject {
	if foreign.is_null() {
		return abi::return_null_with_error("PyType_GenericAlloc(NULL)");
	}
	let Some(native) = twin::registered_native_of_foreign(foreign) else {
		return abi::return_null_with_error(
			"PyType_GenericAlloc on a type that is not PyType_Ready'd",
		);
	};
	// SAFETY: live foreign static registered by PyType_Ready.
	let foreign_ref = unsafe { &*foreign };
	let size = match checked_capi_instance_size(foreign_ref, native, nitems) {
		Ok(size) => size,
		Err(message) => return abi::return_null_with_error(message),
	};
	let itemsize = foreign_ref.tp_itemsize;
	let info = GcTypeInfo {
		size:     core::mem::size_of::<PyObjectHeader>(),
		trace:    trace_capi_instance,
		finalize: Some(finalize_capi_instance),
	};
	let block = match abi::alloc_gc_object_sized(TYPE_ID_CAPI_INSTANCE, info, size) {
		Ok(block) => block,
		Err(message) => return abi::return_null_with_error(message),
	};
	let object = block.cast::<PyObject>();
	// SAFETY: fresh zeroed allocation of at least header size.
	unsafe {
		object.write(PyObject { ob_type: native, gc_meta: crate::object::GcMeta::default() });
		if derives_from_native_str(native) && size >= mem::size_of::<PyUnicode>() {
			let unicode = object.cast::<PyUnicode>();
			(*unicode).len = 0;
			(*unicode).data = ptr::null();
			(*unicode).owns_data = false;
		}
		if itemsize > 0 {
			// PyVarObject.ob_size sits directly after the header.
			object
				.cast::<u8>()
				.add(mem::size_of::<PyObjectHeader>())
				.cast::<isize>()
				.write(nitems);
		}
		if let Some(dict_slot) = capi_instance_dict_slot(block, native) {
			dict_slot.write(ptr::null_mut());
		}
	}
	CAPI_INSTANCES
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.insert(block as usize);
	new_reference(as_object_ptr(object))
}

struct CApiTraverseVisitArg<'a> {
	visitor: &'a mut dyn FnMut(*mut u8),
}

fn visit_capi_reference(object: *mut PyObject, visitor: &mut dyn FnMut(*mut u8)) {
	if object.is_null() {
		return;
	}
	let object = twin::registered_native_of_foreign(object.cast::<ForeignTypeObject>())
		.map_or(object, |native| native.cast::<PyObject>());
	if crate::tag::is_heap(object) {
		visitor(object.cast::<u8>());
	}
}

unsafe extern "C" fn capi_traverse_visit(object: *mut PyObject, arg: *mut c_void) -> c_int {
	if arg.is_null() {
		return 0;
	}
	// SAFETY: `trace_capi_instance` passes a live adapter frame for the
	// duration of the synchronous `tp_traverse` call. The C visitproc contract
	// does not retain `arg` after returning.
	let visit_arg = unsafe { &mut *arg.cast::<CApiTraverseVisitArg<'_>>() };
	visit_capi_reference(object, visit_arg.visitor);
	0
}

unsafe fn capi_instance_dict_slot(
	object: *mut u8,
	native: *const PyType,
) -> Option<*mut *mut PyObject> {
	if object.is_null() || native.is_null() {
		return None;
	}
	let offset = unsafe { (*native).tp_dictoffset };
	if offset <= 0 {
		return None;
	}
	let offset = offset as usize;
	let end = offset.checked_add(mem::size_of::<*mut PyObject>())?;
	if end > unsafe { (*native).tp_basicsize } {
		return None;
	}
	Some(unsafe { object.add(offset).cast::<*mut PyObject>() })
}

/// Traces declared `T_OBJECT`/`T_OBJECT_EX` members precisely: the foreign
/// member table names every ref-holding field offset, so stored values —
/// including ones C code wrote straight into the struct — live exactly as
/// long as the instance. Undeclared stored references remain the extension's
/// obligation via `Py_INCREF` (which pins through `capi::gc_held_roots`).
unsafe extern "C" fn trace_capi_instance(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
	if object.is_null() {
		return;
	}
	// SAFETY: the GC hands a live allocation start with a PyObject header.
	let native = unsafe { (*object.cast::<PyObject>()).ob_type };
	// Registry lookup only reads a map under its own mutex: no allocation,
	// no heap re-entry, safe under the collector's state lock.
	let Some(foreign) = twin::registered_foreign_of_native(native.cast_mut()) else {
		return;
	};
	// SAFETY: registered foreign statics stay live for the process.
	let mut cursor = unsafe { (*foreign).tp_members }.cast::<CMemberDef>();
	if !cursor.is_null() {
		// SAFETY: NULL-name terminated array per CPython contract; offsets were
		// declared by the extension against this instance layout.
		unsafe {
			while !(*cursor).name.is_null() {
				if matches!((*cursor).kind, T_OBJECT | T_OBJECT_EX) {
					let value = object
						.offset((*cursor).offset)
						.cast::<*mut PyObject>()
						.read();
					visit_capi_reference(value, visitor);
				}
				cursor = cursor.add(1);
			}
		}
	}
	let dict = crate::descr::registered_capi_instance_dict(object.cast::<PyObject>());
	if !dict.is_null() {
		visit_capi_reference(dict, visitor);
	} else if let Some(dict_slot) = unsafe { capi_instance_dict_slot(object, native) } {
		let dict = unsafe { dict_slot.read() };
		visit_capi_reference(dict, visitor);
	}

	// SAFETY: `native` is the runtime type object paired with `foreign`.
	let Some(traverse) = (unsafe { (*native).capi_tp_traverse }) else {
		return;
	};
	let mut visit_arg = CApiTraverseVisitArg { visitor };
	// SAFETY: `traverse` is the CPython-compatible slot recorded by
	// `PyType_Ready`; the visitproc adapter only queues valid heap candidates
	// and translates any visited foreign type face to its native twin.
	let _ = unsafe {
		traverse(
			object.cast::<PyObject>(),
			capi_traverse_visit,
			(&mut visit_arg as *mut CApiTraverseVisitArg<'_>).cast::<c_void>(),
		)
	};
}

/// GC finalizer: bridges the foreign `tp_dealloc`. Runs on a fully valid
/// object (deferred-free protocol); the block is reclaimed next cycle.
///
/// The instance stays in [`CAPI_INSTANCES`] until the dealloc returns:
/// `Py_TYPE(self)->tp_free(self)` inside the dealloc must hit the GC-owned
/// no-op path, never `libc::free`. The entry is dropped afterwards, before
/// the block itself is reclaimed by the next cycle.
unsafe extern "C" fn finalize_capi_instance(object: *mut u8) {
	if object.is_null() {
		return;
	}
	// SAFETY: the GC hands a live allocation start with a PyObject header.
	let native = unsafe { (*object.cast::<PyObject>()).ob_type };
	let dealloc = twin::registered_foreign_of_native(native.cast_mut())
		// SAFETY: registered foreign statics stay live for the process.
		.map(|foreign| unsafe { (*foreign).tp_dealloc })
		.filter(|dealloc| !dealloc.is_null());
	if let Some(dealloc) = dealloc {
		let dealloc_fn: unsafe extern "C" fn(*mut PyObject) =
            // SAFETY: tp_dealloc fields hold destructor pointers by header contract.
            unsafe { core::mem::transmute(dealloc) };
		unsafe { dealloc_fn(object.cast::<PyObject>()) };
	}
	unsafe { crate::descr::forget_capi_instance_dict(object.cast::<PyObject>()) };
	CAPI_INSTANCES
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.remove(&(object as usize));
}

/// `PyPonCapiTypeObj.is_subtype` (`PyType_IsSubtype`).
unsafe extern "C" fn capi_is_subtype(
	a: *mut ForeignTypeObject,
	b: *mut ForeignTypeObject,
) -> c_int {
	let (Some(a_native), Some(b_native)) = (twin::native_of_foreign(a), twin::native_of_foreign(b))
	else {
		return 0;
	};
	// SAFETY: live native type objects.
	c_int::from(unsafe { crate::mro::is_subtype(a_native, b_native) })
}

/// `PyPonCapiTypeObj.object_free` (`PyObject_Free` / default `tp_free`):
/// GC-owned instances are reclaimed by the collector, everything else came
/// from `PyObject_Malloc`.
unsafe extern "C" fn capi_object_free(ptr: *mut c_void) {
	if ptr.is_null() || is_capi_instance(ptr) {
		return;
	}
	unsafe { crate::descr::forget_capi_instance_dict(ptr.cast::<PyObject>()) };
	// SAFETY: non-instance pointers passed here were PyObject_Malloc'd.
	unsafe { libc::free(ptr) };
}

/// `PyPonCapiTypeObj.object_init` (`PyObject_Init`): stamps the native type
/// into a caller-allocated (malloc'd) object. Such objects are immortal from
/// the GC's perspective.
unsafe extern "C" fn capi_object_init(
	object: *mut PyObject,
	foreign: *mut ForeignTypeObject,
) -> *mut PyObject {
	if object.is_null() {
		return abi::return_null_with_error("PyObject_Init(NULL)");
	}
	let Some(native) = twin::registered_native_of_foreign(foreign) else {
		return abi::return_null_with_error("PyObject_Init on a type that is not PyType_Ready'd");
	};
	// SAFETY: caller-allocated block of at least basicsize bytes.
	unsafe {
		(*object).ob_type = native;
		(*object).gc_meta = crate::object::GcMeta::default();
		if let Some(dict_slot) = capi_instance_dict_slot(object.cast::<u8>(), native) {
			dict_slot.write(ptr::null_mut());
		}
	}
	new_reference(object)
}

/// `PyPonCapiTypeObj.object_new_raw` (`PyObject_New`/`PyObject_NewVar`):
/// allocation without calling the C `tp_new`.
unsafe extern "C" fn capi_object_new_raw(
	foreign: *mut ForeignTypeObject,
	nitems: isize,
) -> *mut PyObject {
	unsafe { capi_generic_alloc(foreign, nitems) }
}

/// GC type id for slot-wrapper descriptor carriers.
const TYPE_ID_CAPI_SLOT_WRAPPER: TypeId = TypeId(142);

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SlotKind {
	Unary,
	Binary,
	ReflectedBinary,
	Ternary,
	ReflectedTernary,
	RichCompare,
	InquiryBool,
	Len,
	SSizeItem,
	SSizeRepeat,
	SSizeSetItem,
	SSizeDelItem,
	ObjObjProc,
	ObjObjArgSet,
	ObjObjArgDel,
}

#[repr(C)]
struct PySlotWrapper {
	ob_base:     PyObjectHeader,
	slot:        *mut (),
	self_object: *mut PyObject,
	name:        u32,
	kind:        SlotKind,
	compare_op:  c_int,
	doc:         *const c_char,
}

unsafe extern "C" fn trace_slot_wrapper(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
	if object.is_null() {
		return;
	}
	// SAFETY: GC dispatch supplies a live PySlotWrapper allocation.
	let receiver = unsafe { (*object.cast::<PySlotWrapper>()).self_object };
	if !receiver.is_null() && crate::tag::is_heap(receiver.cast()) {
		visitor(receiver.cast());
	}
}

static SLOT_WRAPPER_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type(),
		"wrapper_descriptor",
		core::mem::size_of::<PySlotWrapper>(),
	);
	ty.tp_call = Some(slot_wrapper_call);
	ty.tp_descr_get = Some(slot_wrapper_descr_get);
	ty.tp_getattro = Some(slot_wrapper_getattro);
	ty.tp_setattro = Some(slot_wrapper_setattro);
	Box::into_raw(Box::new(ty)) as usize
});

fn alloc_slot_wrapper(
	slot_ptr: *mut (),
	kind: SlotKind,
	self_object: *mut PyObject,
	name: u32,
) -> *mut PyObject {
	alloc_slot_wrapper_full(slot_ptr, kind, self_object, name, 0, ptr::null())
}

fn alloc_slot_wrapper_full(
	slot_ptr: *mut (),
	kind: SlotKind,
	self_object: *mut PyObject,
	name: u32,
	compare_op: c_int,
	doc: *const c_char,
) -> *mut PyObject {
	let info = GcTypeInfo {
		size:     core::mem::size_of::<PySlotWrapper>(),
		trace:    trace_slot_wrapper,
		finalize: None,
	};
	let Ok(block) = abi::alloc_gc_object(TYPE_ID_CAPI_SLOT_WRAPPER, info) else {
		return abi::return_null_with_error("runtime is not initialized");
	};
	let object = block.cast::<PySlotWrapper>();
	// SAFETY: `block` is a fresh zeroed allocation of the carrier's size.
	unsafe {
		object.write(PySlotWrapper {
			ob_base: PyObjectHeader::new(*SLOT_WRAPPER_TYPE as *const PyType),
			slot: slot_ptr,
			self_object,
			name,
			kind,
			compare_op,
			doc,
		});
	}
	as_object_ptr(object)
}

fn descriptor_name_attr(name: u32) -> *mut PyObject {
	let text = crate::intern::resolve(name).unwrap_or_else(|| "<descriptor>".to_owned());
	unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn descriptor_doc_attr(doc: *const c_char) -> *mut PyObject {
	if doc.is_null() {
		return unsafe { abi::pon_none() };
	}
	let text = unsafe { std::ffi::CStr::from_ptr(doc) }.to_string_lossy();
	unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn set_descriptor_doc(doc: &mut *const c_char, value: *mut PyObject) -> c_int {
	if value.is_null() {
		*doc = ptr::null();
		return 0;
	}
	let Some(text) = (unsafe { crate::types::type_::unicode_text(value) }) else {
		let _ = abi::return_null_with_type_error("__doc__ must be a str");
		return -1;
	};
	let Ok(c_text) = CString::new(text) else {
		let _ = abi::return_null_with_type_error("__doc__ contains NUL");
		return -1;
	};
	*doc = c_text.into_raw();
	0
}

unsafe extern "C" fn slot_wrapper_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let attr = unsafe { crate::types::type_::unicode_text(name) };
	let wrapper = unsafe { &*object.cast::<PySlotWrapper>() };
	match attr {
		Some("__name__") | Some("__qualname__") => descriptor_name_attr(wrapper.name),
		Some("__doc__") => descriptor_doc_attr(wrapper.doc),
		_ => unsafe { crate::descr::generic_get_attr(object, name) },
	}
}

unsafe extern "C" fn slot_wrapper_setattro(
	object: *mut PyObject,
	name: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	let attr = unsafe { crate::types::type_::unicode_text(name) };
	let wrapper = unsafe { &mut *object.cast::<PySlotWrapper>() };
	match attr {
		Some("__doc__") => set_descriptor_doc(&mut wrapper.doc, value),
		_ => {
			let _ = abi::return_null_with_type_error("object does not support attribute assignment");
			-1
		},
	}
}

unsafe extern "C" fn slot_wrapper_descr_get(
	descriptor: *mut PyObject,
	instance: *mut PyObject,
	_owner: *mut PyObject,
) -> *mut PyObject {
	if descriptor.is_null() {
		return abi::return_null_with_error("NULL slot wrapper descriptor");
	}
	if instance.is_null() {
		return descriptor;
	}
	// SAFETY: descriptor protocol dispatches here only for PySlotWrapper values.
	let wrapper = unsafe { &*descriptor.cast::<PySlotWrapper>() };
	alloc_slot_wrapper_full(
		wrapper.slot,
		wrapper.kind,
		instance,
		wrapper.name,
		wrapper.compare_op,
		wrapper.doc,
	)
}

unsafe extern "C" fn slot_wrapper_call(
	callee: *mut PyObject,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	if callee.is_null() {
		return abi::return_null_with_error("NULL slot wrapper");
	}
	if !kwargs.is_null() {
		return abi::return_null_with_error("slot wrappers do not accept keyword arguments");
	}
	// SAFETY: tp_call dispatches here only for PySlotWrapper values.
	let wrapper = unsafe { &*callee.cast::<PySlotWrapper>() };
	let positional = match unsafe { slot_wrapper_args(args) } {
		Ok(values) => values,
		Err(message) => return abi::return_null_with_error(message),
	};
	let (receiver, rest) = if wrapper.self_object.is_null() {
		let Some((&receiver, rest)) = positional.split_first() else {
			return abi::return_null_with_error(format!(
				"descriptor '{}' needs an argument",
				slot_wrapper_name(wrapper)
			));
		};
		(receiver, rest)
	} else {
		(wrapper.self_object, positional)
	};
	match wrapper.kind {
		SlotKind::Unary => unsafe { call_unary_slot_wrapper(wrapper, receiver, rest) },
		SlotKind::Binary => unsafe { call_binary_slot_wrapper(wrapper, receiver, rest, false) },
		SlotKind::ReflectedBinary => unsafe {
			call_binary_slot_wrapper(wrapper, receiver, rest, true)
		},
		SlotKind::Ternary => unsafe { call_ternary_slot_wrapper(wrapper, receiver, rest, false) },
		SlotKind::ReflectedTernary => unsafe {
			call_ternary_slot_wrapper(wrapper, receiver, rest, true)
		},
		SlotKind::RichCompare => unsafe { call_richcompare_slot_wrapper(wrapper, receiver, rest) },
		SlotKind::InquiryBool => unsafe { call_inquiry_slot_wrapper(wrapper, receiver, rest) },
		SlotKind::Len => unsafe { call_len_slot_wrapper(wrapper, receiver, rest) },
		SlotKind::SSizeItem => unsafe { call_ssizearg_slot_wrapper(wrapper, receiver, rest) },
		SlotKind::SSizeRepeat => unsafe { call_ssizearg_slot_wrapper(wrapper, receiver, rest) },
		SlotKind::SSizeSetItem => unsafe {
			call_ssizeobjarg_slot_wrapper(wrapper, receiver, rest, false)
		},
		SlotKind::SSizeDelItem => unsafe {
			call_ssizeobjarg_slot_wrapper(wrapper, receiver, rest, true)
		},
		SlotKind::ObjObjProc => unsafe { call_objobjproc_slot_wrapper(wrapper, receiver, rest) },
		SlotKind::ObjObjArgSet => unsafe {
			call_objobjarg_slot_wrapper(wrapper, receiver, rest, false)
		},
		SlotKind::ObjObjArgDel => unsafe {
			call_objobjarg_slot_wrapper(wrapper, receiver, rest, true)
		},
	}
}

unsafe fn slot_wrapper_args<'a>(args: *mut PyObject) -> Result<&'a [*mut PyObject], String> {
	if args.is_null() {
		return Ok(&[]);
	}
	unsafe { abi::seq::exact_tuple_slice(args) }
		.ok_or_else(|| "slot wrapper call args were not a tuple".to_owned())
}

fn slot_wrapper_name(wrapper: &PySlotWrapper) -> String {
	crate::intern::resolve(wrapper.name).unwrap_or_else(|| "<slot>".to_owned())
}

fn slot_wrapper_arity_error(wrapper: &PySlotWrapper, expected: usize, got: usize) -> *mut PyObject {
	abi::return_null_with_error(format!(
		"{} expected {expected} argument(s), got {got}",
		slot_wrapper_name(wrapper)
	))
}

fn require_slot_arity(wrapper: &PySlotWrapper, args: &[*mut PyObject], expected: usize) -> bool {
	args.len() == expected || {
		let _ = slot_wrapper_arity_error(wrapper, expected, args.len());
		false
	}
}

fn ensure_slot_exception(message: impl Into<String>) {
	if !crate::thread_state::pon_err_occurred() {
		crate::thread_state::pon_err_set(message);
	}
}

fn normalize_object_slot_result(result: *mut PyObject, message: &'static str) -> *mut PyObject {
	if result.is_null() {
		ensure_slot_exception(message);
		return result;
	}
	transfer_new_reference_to_runtime(result)
}

unsafe fn call_unary_slot_wrapper(
	wrapper: &PySlotWrapper,
	receiver: *mut PyObject,
	args: &[*mut PyObject],
) -> *mut PyObject {
	if !require_slot_arity(wrapper, args, 0) {
		return ptr::null_mut();
	}
	let Some(function) = (unsafe { slot::<UnaryFunc>(wrapper.slot) }) else {
		return abi::return_null_with_error("slot wrapper has no unary function");
	};
	normalize_object_slot_result(
		unsafe { function(receiver) },
		"unary slot returned NULL without setting an exception",
	)
}

unsafe fn call_binary_slot_wrapper(
	wrapper: &PySlotWrapper,
	receiver: *mut PyObject,
	args: &[*mut PyObject],
	reflected: bool,
) -> *mut PyObject {
	if !require_slot_arity(wrapper, args, 1) {
		return ptr::null_mut();
	}
	let Some(function) = (unsafe { slot::<BinaryFunc>(wrapper.slot) }) else {
		return abi::return_null_with_error("slot wrapper has no binary function");
	};
	let result = if reflected {
		unsafe { function(args[0], receiver) }
	} else {
		unsafe { function(receiver, args[0]) }
	};
	normalize_object_slot_result(result, "binary slot returned NULL without setting an exception")
}

unsafe fn call_richcompare_slot_wrapper(
	wrapper: &PySlotWrapper,
	receiver: *mut PyObject,
	args: &[*mut PyObject],
) -> *mut PyObject {
	if !require_slot_arity(wrapper, args, 1) {
		return ptr::null_mut();
	}
	let Some(function) = (unsafe { slot::<crate::object::RichCmpFunc>(wrapper.slot) }) else {
		return abi::return_null_with_error("slot wrapper has no rich-compare function");
	};
	normalize_object_slot_result(
		unsafe { function(receiver, args[0], wrapper.compare_op) },
		"rich-compare slot returned NULL without setting an exception",
	)
}

unsafe fn call_ternary_slot_wrapper(
	wrapper: &PySlotWrapper,
	receiver: *mut PyObject,
	args: &[*mut PyObject],
	reflected: bool,
) -> *mut PyObject {
	if !require_slot_arity(wrapper, args, 1) {
		return ptr::null_mut();
	}
	let Some(function) = (unsafe { slot::<TernaryFunc>(wrapper.slot) }) else {
		return abi::return_null_with_error("slot wrapper has no ternary function");
	};
	let none = unsafe { abi::pon_none() };
	let result = if reflected {
		unsafe { function(args[0], receiver, none) }
	} else {
		unsafe { function(receiver, args[0], none) }
	};
	normalize_object_slot_result(result, "ternary slot returned NULL without setting an exception")
}

unsafe fn call_inquiry_slot_wrapper(
	wrapper: &PySlotWrapper,
	receiver: *mut PyObject,
	args: &[*mut PyObject],
) -> *mut PyObject {
	if !require_slot_arity(wrapper, args, 0) {
		return ptr::null_mut();
	}
	let Some(function) = (unsafe { slot::<InquiryFunc>(wrapper.slot) }) else {
		return abi::return_null_with_error("slot wrapper has no inquiry function");
	};
	let status = unsafe { function(receiver) };
	if status < 0 {
		ensure_slot_exception("inquiry slot returned an error without setting an exception");
		return ptr::null_mut();
	}
	unsafe { abi::number::pon_const_bool(c_int::from(status != 0)) }
}

unsafe fn call_len_slot_wrapper(
	wrapper: &PySlotWrapper,
	receiver: *mut PyObject,
	args: &[*mut PyObject],
) -> *mut PyObject {
	if !require_slot_arity(wrapper, args, 0) {
		return ptr::null_mut();
	}
	let Some(function) = (unsafe { slot::<LenFunc>(wrapper.slot) }) else {
		return abi::return_null_with_error("slot wrapper has no length function");
	};
	let len = unsafe { function(receiver) };
	if len < 0 {
		ensure_slot_exception("length slot returned a negative value without setting an exception");
		return ptr::null_mut();
	}
	let Ok(len) = i64::try_from(len) else {
		return abi::return_null_with_error("length slot result exceeds i64");
	};
	unsafe { abi::pon_const_int(len) }
}

unsafe fn call_ssizearg_slot_wrapper(
	wrapper: &PySlotWrapper,
	receiver: *mut PyObject,
	args: &[*mut PyObject],
) -> *mut PyObject {
	if !require_slot_arity(wrapper, args, 1) {
		return ptr::null_mut();
	}
	let Some(index) = (unsafe { object_to_ssize(args[0], &slot_wrapper_name(wrapper)) }) else {
		return ptr::null_mut();
	};
	let Some(function) = (unsafe { slot::<SSizeArgFunc>(wrapper.slot) }) else {
		return abi::return_null_with_error("slot wrapper has no ssizearg function");
	};
	normalize_object_slot_result(
		unsafe { function(receiver, index) },
		"ssizearg slot returned NULL without setting an exception",
	)
}

unsafe fn call_ssizeobjarg_slot_wrapper(
	wrapper: &PySlotWrapper,
	receiver: *mut PyObject,
	args: &[*mut PyObject],
	delete: bool,
) -> *mut PyObject {
	let expected = if delete { 1 } else { 2 };
	if !require_slot_arity(wrapper, args, expected) {
		return ptr::null_mut();
	}
	let Some(index) = (unsafe { object_to_ssize(args[0], &slot_wrapper_name(wrapper)) }) else {
		return ptr::null_mut();
	};
	let Some(function) = (unsafe { slot::<SSizeObjArgProc>(wrapper.slot) }) else {
		return abi::return_null_with_error("slot wrapper has no ssizeobjarg function");
	};
	let value = if delete { ptr::null_mut() } else { args[1] };
	if unsafe { function(receiver, index, value) } < 0 {
		ensure_slot_exception("ssizeobjarg slot returned an error without setting an exception");
		return ptr::null_mut();
	}
	unsafe { abi::pon_none() }
}

unsafe fn call_objobjproc_slot_wrapper(
	wrapper: &PySlotWrapper,
	receiver: *mut PyObject,
	args: &[*mut PyObject],
) -> *mut PyObject {
	if !require_slot_arity(wrapper, args, 1) {
		return ptr::null_mut();
	}
	let Some(function) = (unsafe { slot::<ObjObjProc>(wrapper.slot) }) else {
		return abi::return_null_with_error("slot wrapper has no objobjproc function");
	};
	let status = unsafe { function(receiver, args[0]) };
	if status < 0 {
		ensure_slot_exception("objobjproc slot returned an error without setting an exception");
		return ptr::null_mut();
	}
	unsafe { abi::number::pon_const_bool(c_int::from(status != 0)) }
}

unsafe fn call_objobjarg_slot_wrapper(
	wrapper: &PySlotWrapper,
	receiver: *mut PyObject,
	args: &[*mut PyObject],
	delete: bool,
) -> *mut PyObject {
	let expected = if delete { 1 } else { 2 };
	if !require_slot_arity(wrapper, args, expected) {
		return ptr::null_mut();
	}
	let Some(function) = (unsafe { slot::<ObjObjArgProc>(wrapper.slot) }) else {
		return abi::return_null_with_error("slot wrapper has no objobjarg function");
	};
	let value = if delete { ptr::null_mut() } else { args[1] };
	if unsafe { function(receiver, args[0], value) } < 0 {
		ensure_slot_exception("objobjarg slot returned an error without setting an exception");
		return ptr::null_mut();
	}
	unsafe { abi::pon_none() }
}

unsafe fn object_to_ssize(value: *mut PyObject, context: &str) -> Option<isize> {
	if value.is_null() {
		raise_type_error(format!("{context} argument cannot be NULL"));
		return None;
	}
	let value = crate::tag::untag_arg(value);
	if let Some(index) = unsafe { bigint_object_to_ssize(value, context) } {
		return Some(index);
	}
	let ty = unsafe { value.as_ref().and_then(|object| object.ob_type.as_ref()) };
	if let Some(index_slot) = ty.and_then(|ty| unsafe {
		ty.tp_as_number
			.as_ref()
			.and_then(|methods| methods.nb_index)
	}) {
		let result = unsafe { index_slot(value) };
		if result.is_null() {
			ensure_slot_exception("__index__ slot returned NULL without setting an exception");
			return None;
		}
		let result = crate::tag::untag_arg(result);
		if let Some(index) = unsafe { bigint_object_to_ssize(result, context) } {
			return Some(index);
		}
		raise_type_error("__index__ returned non-int");
		return None;
	}
	raise_type_error(format!("{context} argument cannot be interpreted as an integer"));
	None
}

unsafe fn bigint_object_to_ssize(value: *mut PyObject, context: &str) -> Option<isize> {
	let value = unsafe { crate::types::int::to_bigint_including_bool(value) }?;
	match num_traits::ToPrimitive::to_isize(&value) {
		Some(index) => Some(index),
		None => {
			raise_type_error(format!("{context} argument is out of Py_ssize_t range"));
			None
		},
	}
}

unsafe fn install_slot_wrappers(ns: &mut PyClassDict, foreign: &ForeignTypeObject) -> Option<()> {
	let mapping = if foreign.tp_as_mapping.is_null() {
		None
	} else {
		Some(unsafe { &*foreign.tp_as_mapping.cast::<CMappingMethods>() })
	};
	let mapping_has_len = mapping.map_or(false, |methods| !methods.mp_length.is_null());
	let mapping_has_getitem = mapping.map_or(false, |methods| !methods.mp_subscript.is_null());
	let mapping_has_ass_item = mapping.map_or(false, |methods| !methods.mp_ass_subscript.is_null());

	if !foreign.tp_as_number.is_null() {
		let methods = unsafe { &*foreign.tp_as_number.cast::<CNumberMethods>() };
		install_slot_wrapper(ns, "__add__", methods.nb_add, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__radd__", methods.nb_add, SlotKind::ReflectedBinary)?;
		install_slot_wrapper(ns, "__sub__", methods.nb_subtract, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__rsub__", methods.nb_subtract, SlotKind::ReflectedBinary)?;
		install_slot_wrapper(ns, "__mul__", methods.nb_multiply, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__rmul__", methods.nb_multiply, SlotKind::ReflectedBinary)?;
		install_slot_wrapper(ns, "__mod__", methods.nb_remainder, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__rmod__", methods.nb_remainder, SlotKind::ReflectedBinary)?;
		install_slot_wrapper(ns, "__divmod__", methods.nb_divmod, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__rdivmod__", methods.nb_divmod, SlotKind::ReflectedBinary)?;
		install_slot_wrapper(ns, "__pow__", methods.nb_power, SlotKind::Ternary)?;
		install_slot_wrapper(ns, "__rpow__", methods.nb_power, SlotKind::ReflectedTernary)?;
		install_slot_wrapper(ns, "__neg__", methods.nb_negative, SlotKind::Unary)?;
		install_slot_wrapper(ns, "__pos__", methods.nb_positive, SlotKind::Unary)?;
		install_slot_wrapper(ns, "__abs__", methods.nb_absolute, SlotKind::Unary)?;
		install_slot_wrapper(ns, "__bool__", methods.nb_bool, SlotKind::InquiryBool)?;
		install_slot_wrapper(ns, "__invert__", methods.nb_invert, SlotKind::Unary)?;
		install_slot_wrapper(ns, "__lshift__", methods.nb_lshift, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__rlshift__", methods.nb_lshift, SlotKind::ReflectedBinary)?;
		install_slot_wrapper(ns, "__rshift__", methods.nb_rshift, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__rrshift__", methods.nb_rshift, SlotKind::ReflectedBinary)?;
		install_slot_wrapper(ns, "__and__", methods.nb_and, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__rand__", methods.nb_and, SlotKind::ReflectedBinary)?;
		install_slot_wrapper(ns, "__xor__", methods.nb_xor, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__rxor__", methods.nb_xor, SlotKind::ReflectedBinary)?;
		install_slot_wrapper(ns, "__or__", methods.nb_or, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__ror__", methods.nb_or, SlotKind::ReflectedBinary)?;
		install_slot_wrapper(ns, "__int__", methods.nb_int, SlotKind::Unary)?;
		install_slot_wrapper(ns, "__float__", methods.nb_float, SlotKind::Unary)?;
		install_slot_wrapper(ns, "__iadd__", methods.nb_inplace_add, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__isub__", methods.nb_inplace_subtract, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__imul__", methods.nb_inplace_multiply, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__imod__", methods.nb_inplace_remainder, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__ipow__", methods.nb_inplace_power, SlotKind::Ternary)?;
		install_slot_wrapper(ns, "__ilshift__", methods.nb_inplace_lshift, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__irshift__", methods.nb_inplace_rshift, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__iand__", methods.nb_inplace_and, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__ixor__", methods.nb_inplace_xor, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__ior__", methods.nb_inplace_or, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__floordiv__", methods.nb_floor_divide, SlotKind::Binary)?;
		install_slot_wrapper(
			ns,
			"__rfloordiv__",
			methods.nb_floor_divide,
			SlotKind::ReflectedBinary,
		)?;
		install_slot_wrapper(ns, "__truediv__", methods.nb_true_divide, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__rtruediv__", methods.nb_true_divide, SlotKind::ReflectedBinary)?;
		install_slot_wrapper(ns, "__ifloordiv__", methods.nb_inplace_floor_divide, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__itruediv__", methods.nb_inplace_true_divide, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__index__", methods.nb_index, SlotKind::Unary)?;
		install_slot_wrapper(ns, "__matmul__", methods.nb_matrix_multiply, SlotKind::Binary)?;
		install_slot_wrapper(
			ns,
			"__rmatmul__",
			methods.nb_matrix_multiply,
			SlotKind::ReflectedBinary,
		)?;
		install_slot_wrapper(
			ns,
			"__imatmul__",
			methods.nb_inplace_matrix_multiply,
			SlotKind::Binary,
		)?;
	}
	if !foreign.tp_richcompare.is_null() {
		install_richcompare_slot_wrapper(ns, "__lt__", foreign.tp_richcompare, abi::object::RICH_LT as c_int)?;
		install_richcompare_slot_wrapper(ns, "__le__", foreign.tp_richcompare, abi::object::RICH_LE as c_int)?;
		install_richcompare_slot_wrapper(ns, "__eq__", foreign.tp_richcompare, abi::object::RICH_EQ as c_int)?;
		install_richcompare_slot_wrapper(ns, "__ne__", foreign.tp_richcompare, abi::object::RICH_NE as c_int)?;
		install_richcompare_slot_wrapper(ns, "__gt__", foreign.tp_richcompare, abi::object::RICH_GT as c_int)?;
		install_richcompare_slot_wrapper(ns, "__ge__", foreign.tp_richcompare, abi::object::RICH_GE as c_int)?;
	}

	if !foreign.tp_as_sequence.is_null() {
		let methods = unsafe { &*foreign.tp_as_sequence.cast::<CSequenceMethods>() };
		if !mapping_has_len {
			install_slot_wrapper(ns, "__len__", methods.sq_length, SlotKind::Len)?;
		}
		install_slot_wrapper(ns, "__add__", methods.sq_concat, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__mul__", methods.sq_repeat, SlotKind::SSizeRepeat)?;
		install_slot_wrapper(ns, "__rmul__", methods.sq_repeat, SlotKind::SSizeRepeat)?;
		if !mapping_has_getitem {
			install_slot_wrapper(ns, "__getitem__", methods.sq_item, SlotKind::SSizeItem)?;
		}
		if !mapping_has_ass_item {
			install_slot_wrapper(ns, "__setitem__", methods.sq_ass_item, SlotKind::SSizeSetItem)?;
			install_slot_wrapper(ns, "__delitem__", methods.sq_ass_item, SlotKind::SSizeDelItem)?;
		}
		install_slot_wrapper(ns, "__contains__", methods.sq_contains, SlotKind::ObjObjProc)?;
		install_slot_wrapper(ns, "__iadd__", methods.sq_inplace_concat, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__imul__", methods.sq_inplace_repeat, SlotKind::SSizeRepeat)?;
	}

	if let Some(methods) = mapping {
		install_slot_wrapper(ns, "__len__", methods.mp_length, SlotKind::Len)?;
		install_slot_wrapper(ns, "__getitem__", methods.mp_subscript, SlotKind::Binary)?;
		install_slot_wrapper(ns, "__setitem__", methods.mp_ass_subscript, SlotKind::ObjObjArgSet)?;
		install_slot_wrapper(ns, "__delitem__", methods.mp_ass_subscript, SlotKind::ObjObjArgDel)?;
	}

	Some(())
}

fn install_slot_wrapper(
	ns: &mut PyClassDict,
	name: &str,
	slot_ptr: *mut (),
	kind: SlotKind,
) -> Option<()> {
	if slot_ptr.is_null() {
		return Some(());
	}
	let name_id = intern(name);
	if ns.get(name_id).is_some() {
		return Some(());
	}
	let descriptor = alloc_slot_wrapper(slot_ptr, kind, ptr::null_mut(), name_id);
	if descriptor.is_null() {
		return None;
	}
	ns.set(name_id, descriptor);
	Some(())
}

fn install_richcompare_slot_wrapper(
	ns: &mut PyClassDict,
	name: &str,
	slot_ptr: *mut (),
	compare_op: c_int,
) -> Option<()> {
	if slot_ptr.is_null() {
		return Some(());
	}
	let name_id = intern(name);
	if ns.get(name_id).is_some() {
		return Some(());
	}
	let descriptor = alloc_slot_wrapper_full(
		slot_ptr,
		SlotKind::RichCompare,
		ptr::null_mut(),
		name_id,
		compare_op,
		ptr::null(),
	);
	if descriptor.is_null() {
		return None;
	}
	ns.set(name_id, descriptor);
	Some(())
}

unsafe fn inherit_foreign_protocol_tables(child: &mut ForeignTypeObject, base: &ForeignTypeObject) {
	unsafe { inherit_number_protocol_table(&mut child.tp_as_number, base.tp_as_number) };
	unsafe { inherit_sequence_protocol_table(&mut child.tp_as_sequence, base.tp_as_sequence) };
	unsafe { inherit_mapping_protocol_table(&mut child.tp_as_mapping, base.tp_as_mapping) };
}

unsafe fn inherit_number_protocol_table(child: &mut *mut (), base: *mut ()) {
	if base.is_null() {
		return;
	}
	if child.is_null() {
		*child = base;
		return;
	}
	if !FROMSPEC_NUMBER_TABLES
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.contains(&(*child as usize))
	{
		return;
	}
	let child = unsafe { &mut *child.cast::<CNumberMethods>() };
	let base = unsafe { &*base.cast::<CNumberMethods>() };
	macro_rules! inherit_field {
		($field:ident) => {
			if child.$field.is_null() {
				child.$field = base.$field;
			}
		};
	}
	inherit_field!(nb_add);
	inherit_field!(nb_subtract);
	inherit_field!(nb_multiply);
	inherit_field!(nb_remainder);
	inherit_field!(nb_divmod);
	inherit_field!(nb_power);
	inherit_field!(nb_negative);
	inherit_field!(nb_positive);
	inherit_field!(nb_absolute);
	inherit_field!(nb_bool);
	inherit_field!(nb_invert);
	inherit_field!(nb_lshift);
	inherit_field!(nb_rshift);
	inherit_field!(nb_and);
	inherit_field!(nb_xor);
	inherit_field!(nb_or);
	inherit_field!(nb_int);
	inherit_field!(nb_reserved);
	inherit_field!(nb_float);
	inherit_field!(nb_inplace_add);
	inherit_field!(nb_inplace_subtract);
	inherit_field!(nb_inplace_multiply);
	inherit_field!(nb_inplace_remainder);
	inherit_field!(nb_inplace_power);
	inherit_field!(nb_inplace_lshift);
	inherit_field!(nb_inplace_rshift);
	inherit_field!(nb_inplace_and);
	inherit_field!(nb_inplace_xor);
	inherit_field!(nb_inplace_or);
	inherit_field!(nb_floor_divide);
	inherit_field!(nb_true_divide);
	inherit_field!(nb_inplace_floor_divide);
	inherit_field!(nb_inplace_true_divide);
	inherit_field!(nb_index);
	inherit_field!(nb_matrix_multiply);
	inherit_field!(nb_inplace_matrix_multiply);
}

unsafe fn inherit_sequence_protocol_table(child: &mut *mut (), base: *mut ()) {
	if base.is_null() {
		return;
	}
	if child.is_null() {
		*child = base;
		return;
	}
	if !FROMSPEC_SEQUENCE_TABLES
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.contains(&(*child as usize))
	{
		return;
	}
	let child = unsafe { &mut *child.cast::<CSequenceMethods>() };
	let base = unsafe { &*base.cast::<CSequenceMethods>() };
	macro_rules! inherit_field {
		($field:ident) => {
			if child.$field.is_null() {
				child.$field = base.$field;
			}
		};
	}
	inherit_field!(sq_length);
	inherit_field!(sq_concat);
	inherit_field!(sq_repeat);
	inherit_field!(sq_item);
	inherit_field!(was_sq_slice);
	inherit_field!(sq_ass_item);
	inherit_field!(was_sq_ass_slice);
	inherit_field!(sq_contains);
	inherit_field!(sq_inplace_concat);
	inherit_field!(sq_inplace_repeat);
}

unsafe fn inherit_mapping_protocol_table(child: &mut *mut (), base: *mut ()) {
	if base.is_null() {
		return;
	}
	if child.is_null() {
		*child = base;
		return;
	}
	if !FROMSPEC_MAPPING_TABLES
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.contains(&(*child as usize))
	{
		return;
	}
	let child = unsafe { &mut *child.cast::<CMappingMethods>() };
	let base = unsafe { &*base.cast::<CMappingMethods>() };
	if child.mp_length.is_null() {
		child.mp_length = base.mp_length;
	}
	if child.mp_subscript.is_null() {
		child.mp_subscript = base.mp_subscript;
	}
	if child.mp_ass_subscript.is_null() {
		child.mp_ass_subscript = base.mp_ass_subscript;
	}
}

unsafe fn install_native_protocol_tables(ty: &mut PyType, foreign: &ForeignTypeObject) {
	if let Some(methods) = unsafe { native_number_methods(foreign) } {
		ty.tp_as_number = Box::into_raw(Box::new(methods));
	}
	if let Some(methods) = unsafe { native_sequence_methods(foreign) } {
		ty.tp_as_sequence = Box::into_raw(Box::new(methods));
	}
	if let Some(methods) = unsafe { native_mapping_methods(foreign) } {
		ty.tp_as_mapping = Box::into_raw(Box::new(methods));
	}
}

unsafe fn native_number_methods(foreign: &ForeignTypeObject) -> Option<PyNumberMethods> {
	if foreign.tp_as_number.is_null() {
		return None;
	}
	let c = unsafe { &*foreign.tp_as_number.cast::<CNumberMethods>() };
	let mut methods = PyNumberMethods::EMPTY;
	if let Some(function) = unsafe { slot::<BinaryFunc>(c.nb_add) } {
		methods.nb_add = Some(function);
		methods.nb_reflected_add = Some(capi_nb_reflected_add);
	}
	if let Some(function) = unsafe { slot::<BinaryFunc>(c.nb_subtract) } {
		methods.nb_subtract = Some(function);
		methods.nb_reflected_subtract = Some(capi_nb_reflected_subtract);
	}
	if let Some(function) = unsafe { slot::<BinaryFunc>(c.nb_multiply) } {
		methods.nb_multiply = Some(function);
		methods.nb_reflected_multiply = Some(capi_nb_reflected_multiply);
	}
	if let Some(function) = unsafe { slot::<BinaryFunc>(c.nb_remainder) } {
		methods.nb_remainder = Some(function);
		methods.nb_reflected_remainder = Some(capi_nb_reflected_remainder);
	}
	if let Some(function) = unsafe { slot::<BinaryFunc>(c.nb_divmod) } {
		methods.nb_divmod = Some(function);
		methods.nb_reflected_divmod = Some(capi_nb_reflected_divmod);
	}
	methods.nb_power = unsafe { slot::<TernaryFunc>(c.nb_power) };
	if methods.nb_power.is_some() {
		methods.nb_reflected_power = Some(capi_nb_reflected_power);
	}
	methods.nb_negative = unsafe { slot::<UnaryFunc>(c.nb_negative) };
	methods.nb_positive = unsafe { slot::<UnaryFunc>(c.nb_positive) };
	methods.nb_absolute = unsafe { slot::<UnaryFunc>(c.nb_absolute) };
	methods.nb_bool = unsafe { slot::<InquiryFunc>(c.nb_bool) };
	methods.nb_invert = unsafe { slot::<UnaryFunc>(c.nb_invert) };
	if let Some(function) = unsafe { slot::<BinaryFunc>(c.nb_lshift) } {
		methods.nb_lshift = Some(function);
		methods.nb_reflected_lshift = Some(capi_nb_reflected_lshift);
	}
	if let Some(function) = unsafe { slot::<BinaryFunc>(c.nb_rshift) } {
		methods.nb_rshift = Some(function);
		methods.nb_reflected_rshift = Some(capi_nb_reflected_rshift);
	}
	if let Some(function) = unsafe { slot::<BinaryFunc>(c.nb_and) } {
		methods.nb_and = Some(function);
		methods.nb_reflected_and = Some(capi_nb_reflected_and);
	}
	if let Some(function) = unsafe { slot::<BinaryFunc>(c.nb_xor) } {
		methods.nb_xor = Some(function);
		methods.nb_reflected_xor = Some(capi_nb_reflected_xor);
	}
	if let Some(function) = unsafe { slot::<BinaryFunc>(c.nb_or) } {
		methods.nb_or = Some(function);
		methods.nb_reflected_or = Some(capi_nb_reflected_or);
	}
	methods.nb_int = unsafe { slot::<UnaryFunc>(c.nb_int) };
	methods.nb_float = unsafe { slot::<UnaryFunc>(c.nb_float) };
	methods.nb_inplace_add = unsafe { slot::<BinaryFunc>(c.nb_inplace_add) };
	methods.nb_inplace_subtract = unsafe { slot::<BinaryFunc>(c.nb_inplace_subtract) };
	methods.nb_inplace_multiply = unsafe { slot::<BinaryFunc>(c.nb_inplace_multiply) };
	methods.nb_inplace_remainder = unsafe { slot::<BinaryFunc>(c.nb_inplace_remainder) };
	methods.nb_inplace_power = unsafe { slot::<TernaryFunc>(c.nb_inplace_power) };
	methods.nb_inplace_lshift = unsafe { slot::<BinaryFunc>(c.nb_inplace_lshift) };
	methods.nb_inplace_rshift = unsafe { slot::<BinaryFunc>(c.nb_inplace_rshift) };
	methods.nb_inplace_and = unsafe { slot::<BinaryFunc>(c.nb_inplace_and) };
	methods.nb_inplace_xor = unsafe { slot::<BinaryFunc>(c.nb_inplace_xor) };
	methods.nb_inplace_or = unsafe { slot::<BinaryFunc>(c.nb_inplace_or) };
	if let Some(function) = unsafe { slot::<BinaryFunc>(c.nb_floor_divide) } {
		methods.nb_floor_divide = Some(function);
		methods.nb_reflected_floor_divide = Some(capi_nb_reflected_floor_divide);
	}
	if let Some(function) = unsafe { slot::<BinaryFunc>(c.nb_true_divide) } {
		methods.nb_true_divide = Some(function);
		methods.nb_reflected_true_divide = Some(capi_nb_reflected_true_divide);
	}
	methods.nb_inplace_floor_divide = unsafe { slot::<BinaryFunc>(c.nb_inplace_floor_divide) };
	methods.nb_inplace_true_divide = unsafe { slot::<BinaryFunc>(c.nb_inplace_true_divide) };
	methods.nb_index = unsafe { slot::<UnaryFunc>(c.nb_index) };
	if let Some(function) = unsafe { slot::<BinaryFunc>(c.nb_matrix_multiply) } {
		methods.nb_matrix_multiply = Some(function);
		methods.nb_reflected_matrix_multiply = Some(capi_nb_reflected_matrix_multiply);
	}
	methods.nb_inplace_matrix_multiply = unsafe { slot::<BinaryFunc>(c.nb_inplace_matrix_multiply) };
	Some(methods)
}

unsafe fn native_sequence_methods(foreign: &ForeignTypeObject) -> Option<PySequenceMethods> {
	if foreign.tp_as_sequence.is_null() {
		return None;
	}
	let c = unsafe { &*foreign.tp_as_sequence.cast::<CSequenceMethods>() };
	let mapping_has_length = if foreign.tp_as_mapping.is_null() {
		false
	} else {
		!unsafe { (*foreign.tp_as_mapping.cast::<CMappingMethods>()).mp_length }.is_null()
	};
	let mut methods = PySequenceMethods::EMPTY;
	if !mapping_has_length {
		methods.sq_length = unsafe { slot::<LenFunc>(c.sq_length) };
	}
	methods.sq_concat = unsafe { slot::<BinaryFunc>(c.sq_concat) };
	if !c.sq_repeat.is_null() {
		methods.sq_repeat = Some(capi_sq_repeat);
	}
	methods.sq_item = unsafe { slot::<SSizeArgFunc>(c.sq_item) };
	methods.sq_ass_item = unsafe { slot::<SSizeObjArgProc>(c.sq_ass_item) };
	methods.sq_contains = unsafe { slot::<ObjObjProc>(c.sq_contains) };
	methods.sq_inplace_concat = unsafe { slot::<BinaryFunc>(c.sq_inplace_concat) };
	if !c.sq_inplace_repeat.is_null() {
		methods.sq_inplace_repeat = Some(capi_sq_inplace_repeat);
	}
	Some(methods)
}

unsafe fn native_mapping_methods(foreign: &ForeignTypeObject) -> Option<PyMappingMethods> {
	if foreign.tp_as_mapping.is_null() {
		return None;
	}
	let c = unsafe { &*foreign.tp_as_mapping.cast::<CMappingMethods>() };
	let mut methods = PyMappingMethods::EMPTY;
	methods.mp_length = unsafe { slot::<LenFunc>(c.mp_length) };
	methods.mp_subscript = unsafe { slot::<BinaryFunc>(c.mp_subscript) };
	methods.mp_ass_subscript = unsafe { slot::<ObjObjArgProc>(c.mp_ass_subscript) };
	Some(methods)
}

unsafe fn foreign_type_for_slot_receiver(
	receiver: *mut PyObject,
) -> Option<*mut ForeignTypeObject> {
	if receiver.is_null() {
		return None;
	}
	let receiver = crate::tag::untag_arg(receiver);
	// SAFETY: `receiver` is a live object supplied to a native slot.
	let native = unsafe { (*receiver).ob_type.cast_mut() };
	twin::registered_foreign_of_native(native)
}

unsafe fn call_reflected_number_slot(
	receiver: *mut PyObject,
	other: *mut PyObject,
	select: impl FnOnce(&CNumberMethods) -> *mut (),
	slot_name: &'static str,
) -> *mut PyObject {
	let Some(foreign) = (unsafe { foreign_type_for_slot_receiver(receiver) }) else {
		return abi::return_null_with_error(format!(
			"{slot_name} reflected slot receiver is not a ready C type"
		));
	};
	let table = unsafe { (*foreign).tp_as_number };
	if table.is_null() {
		return abi::return_null_with_error(format!("{slot_name} reflected slot table is missing"));
	}
	let slot_ptr = select(unsafe { &*table.cast::<CNumberMethods>() });
	let Some(function) = (unsafe { slot::<BinaryFunc>(slot_ptr) }) else {
		return abi::return_null_with_error(format!("{slot_name} reflected slot is missing"));
	};
	normalize_object_slot_result(
		unsafe { function(other, receiver) },
		"reflected binary slot returned NULL without setting an exception",
	)
}

unsafe fn call_reflected_power_slot(
	receiver: *mut PyObject,
	other: *mut PyObject,
) -> *mut PyObject {
	let Some(foreign) = (unsafe { foreign_type_for_slot_receiver(receiver) }) else {
		return abi::return_null_with_error("nb_power reflected slot receiver is not a ready C type");
	};
	let table = unsafe { (*foreign).tp_as_number };
	if table.is_null() {
		return abi::return_null_with_error("nb_power reflected slot table is missing");
	}
	let slot_ptr = unsafe { (*table.cast::<CNumberMethods>()).nb_power };
	let Some(function) = (unsafe { slot::<TernaryFunc>(slot_ptr) }) else {
		return abi::return_null_with_error("nb_power reflected slot is missing");
	};
	let none = unsafe { abi::pon_none() };
	normalize_object_slot_result(
		unsafe { function(other, receiver, none) },
		"reflected power slot returned NULL without setting an exception",
	)
}

macro_rules! reflected_number_slot {
	($fn_name:ident, $field:ident) => {
		unsafe extern "C" fn $fn_name(
			receiver: *mut PyObject,
			other: *mut PyObject,
		) -> *mut PyObject {
			unsafe {
				call_reflected_number_slot(
					receiver,
					other,
					|methods| methods.$field,
					stringify!($field),
				)
			}
		}
	};
}

reflected_number_slot!(capi_nb_reflected_add, nb_add);
reflected_number_slot!(capi_nb_reflected_subtract, nb_subtract);
reflected_number_slot!(capi_nb_reflected_multiply, nb_multiply);
reflected_number_slot!(capi_nb_reflected_remainder, nb_remainder);
reflected_number_slot!(capi_nb_reflected_divmod, nb_divmod);
reflected_number_slot!(capi_nb_reflected_lshift, nb_lshift);
reflected_number_slot!(capi_nb_reflected_rshift, nb_rshift);
reflected_number_slot!(capi_nb_reflected_and, nb_and);
reflected_number_slot!(capi_nb_reflected_xor, nb_xor);
reflected_number_slot!(capi_nb_reflected_or, nb_or);
reflected_number_slot!(capi_nb_reflected_floor_divide, nb_floor_divide);
reflected_number_slot!(capi_nb_reflected_true_divide, nb_true_divide);
reflected_number_slot!(capi_nb_reflected_matrix_multiply, nb_matrix_multiply);

unsafe extern "C" fn capi_nb_reflected_power(
	receiver: *mut PyObject,
	other: *mut PyObject,
	_modulo: *mut PyObject,
) -> *mut PyObject {
	unsafe { call_reflected_power_slot(receiver, other) }
}

unsafe extern "C" fn capi_sq_repeat(
	receiver: *mut PyObject,
	count: *mut PyObject,
) -> *mut PyObject {
	unsafe { call_sequence_repeat_slot(receiver, count, false) }
}

unsafe extern "C" fn capi_sq_inplace_repeat(
	receiver: *mut PyObject,
	count: *mut PyObject,
) -> *mut PyObject {
	unsafe { call_sequence_repeat_slot(receiver, count, true) }
}

unsafe fn call_sequence_repeat_slot(
	receiver: *mut PyObject,
	count: *mut PyObject,
	inplace: bool,
) -> *mut PyObject {
	let Some(index) = (unsafe {
		object_to_ssize(
			count,
			if inplace {
				"sq_inplace_repeat"
			} else {
				"sq_repeat"
			},
		)
	}) else {
		return ptr::null_mut();
	};
	let Some(foreign) = (unsafe { foreign_type_for_slot_receiver(receiver) }) else {
		return abi::return_null_with_error("sequence repeat slot receiver is not a ready C type");
	};
	let table = unsafe { (*foreign).tp_as_sequence };
	if table.is_null() {
		return abi::return_null_with_error("sequence repeat slot table is missing");
	}
	let methods = unsafe { &*table.cast::<CSequenceMethods>() };
	let slot_ptr = if inplace {
		methods.sq_inplace_repeat
	} else {
		methods.sq_repeat
	};
	let Some(function) = (unsafe { slot::<SSizeArgFunc>(slot_ptr) }) else {
		return abi::return_null_with_error("sequence repeat slot is missing");
	};
	normalize_object_slot_result(
		unsafe { function(receiver, index) },
		"sequence repeat slot returned NULL without setting an exception",
	)
}

/// getset descriptor carrier.
#[repr(C)]
struct CGetSetDef {
	name:    *const c_char,
	get:     *mut (),
	set:     *mut (),
	doc:     *const c_char,
	closure: *mut c_void,
}

#[repr(C)]
struct PyGetSetDescr {
	ob_base: PyObjectHeader,
	get:     *mut (),
	set:     *mut (),
	closure: *mut c_void,
	name:    u32,
	doc:     *const c_char,
}

static GETSET_DESCR_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type(),
		"getset_descriptor",
		core::mem::size_of::<PyGetSetDescr>(),
	);
	ty.tp_descr_get = Some(getset_descr_get);
	ty.tp_descr_set = Some(getset_descr_set);
	ty.tp_getattro = Some(getset_descr_getattro);
	ty.tp_setattro = Some(getset_descr_setattro);
	Box::into_raw(Box::new(ty)) as usize
});

fn alloc_getset_descriptor(def: &CGetSetDef, name: &str) -> *mut PyObject {
	let descriptor = Box::new(PyGetSetDescr {
		ob_base: PyObjectHeader::new(*GETSET_DESCR_TYPE as *const PyType),
		get:     def.get,
		set:     def.set,
		closure: def.closure,
		name:    intern(name),
		doc:     def.doc,
	});
	as_object_ptr(Box::into_raw(descriptor))
}

unsafe extern "C" fn getset_descr_get(
	descriptor: *mut PyObject,
	instance: *mut PyObject,
	_owner: *mut PyObject,
) -> *mut PyObject {
	// SAFETY: dispatched only for PyGetSetDescr values.
	let descr = unsafe { &*descriptor.cast::<PyGetSetDescr>() };
	if instance.is_null() {
		return descriptor;
	}
	// Twin contract, inbound-to-C leg: a registered native TYPE receiver
	// (e.g. a numpy DTypeMeta class read through its twin) must cross into
	// C as its foreign face — the getter reads C struct fields off `self`.
	let instance = super::foreignize_type_result(instance);
	let Some(get) = (unsafe {
		slot::<unsafe extern "C" fn(*mut PyObject, *mut c_void) -> *mut PyObject>(descr.get)
	}) else {
		let name = crate::intern::resolve(descr.name).unwrap_or_default();
		return abi::return_null_with_error(format!("attribute '{name}' is not readable"));
	};
	transfer_new_reference_to_runtime(unsafe { get(instance, descr.closure) })
}

unsafe extern "C" fn getset_descr_set(
	descriptor: *mut PyObject,
	instance: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	// SAFETY: dispatched only for PyGetSetDescr values.
	let descr = unsafe { &*descriptor.cast::<PyGetSetDescr>() };
	let instance = super::foreignize_type_result(instance);
	let Some(set) = (unsafe {
		slot::<unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut c_void) -> c_int>(descr.set)
	}) else {
		let name = crate::intern::resolve(descr.name).unwrap_or_default();
		raise_type_error(format!("attribute '{name}' is read-only"));
		return -1;
	};
	unsafe { set(instance, value, descr.closure) }
}

unsafe extern "C" fn getset_descr_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let attr = unsafe { crate::types::type_::unicode_text(name) };
	let descr = unsafe { &*object.cast::<PyGetSetDescr>() };
	match attr {
		Some("__name__") | Some("__qualname__") => descriptor_name_attr(descr.name),
		Some("__doc__") => descriptor_doc_attr(descr.doc),
		_ => unsafe { crate::descr::generic_get_attr(object, name) },
	}
}

unsafe extern "C" fn getset_descr_setattro(
	object: *mut PyObject,
	name: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	let attr = unsafe { crate::types::type_::unicode_text(name) };
	let descr = unsafe { &mut *object.cast::<PyGetSetDescr>() };
	match attr {
		Some("__doc__") => set_descriptor_doc(&mut descr.doc, value),
		_ => {
			let _ = abi::return_null_with_type_error("object does not support attribute assignment");
			-1
		},
	}
}

/// member descriptor carrier (structmember.h `PyMemberDef`).
#[repr(C)]
struct CMemberDef {
	name:   *const c_char,
	kind:   c_int,
	offset: isize,
	flags:  c_int,
	doc:    *const c_char,
}

#[repr(C)]
struct PyCMemberDescr {
	ob_base: PyObjectHeader,
	kind:    c_int,
	flags:   c_int,
	offset:  isize,
	name:    u32,
	doc:     *const c_char,
}

// structmember.h T_* codes.
const T_SHORT: c_int = 0;
const T_INT: c_int = 1;
const T_LONG: c_int = 2;
const T_FLOAT: c_int = 3;
const T_DOUBLE: c_int = 4;
const T_STRING: c_int = 5;
const T_OBJECT: c_int = 6;
const T_CHAR: c_int = 7;
const T_BYTE: c_int = 8;
const T_UBYTE: c_int = 9;
const T_USHORT: c_int = 10;
const T_UINT: c_int = 11;
const T_ULONG: c_int = 12;
const T_BOOL: c_int = 14;
const T_OBJECT_EX: c_int = 16;
const T_LONGLONG: c_int = 17;
const T_ULONGLONG: c_int = 18;
const T_PYSSIZET: c_int = 19;
const T_NONE: c_int = 20;
const READONLY: c_int = 1;

static MEMBER_DESCR_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type(),
		"member_descriptor",
		core::mem::size_of::<PyCMemberDescr>(),
	);
	ty.tp_descr_get = Some(member_descr_get);
	ty.tp_descr_set = Some(member_descr_set);
	ty.tp_getattro = Some(member_descr_getattro);
	ty.tp_setattro = Some(member_descr_setattro);
	Box::into_raw(Box::new(ty)) as usize
});

fn alloc_member_descriptor(def: &CMemberDef, name: &str) -> *mut PyObject {
	let descriptor = Box::new(PyCMemberDescr {
		ob_base: PyObjectHeader::new(*MEMBER_DESCR_TYPE as *const PyType),
		kind:    def.kind,
		flags:   def.flags,
		offset:  def.offset,
		name:    intern(name),
		doc:     def.doc,
	});
	as_object_ptr(Box::into_raw(descriptor))
}

unsafe extern "C" fn member_descr_get(
	descriptor: *mut PyObject,
	instance: *mut PyObject,
	_owner: *mut PyObject,
) -> *mut PyObject {
	// SAFETY: dispatched only for PyCMemberDescr values.
	let descr = unsafe { &*descriptor.cast::<PyCMemberDescr>() };
	if instance.is_null() {
		return descriptor;
	}
	// Twin contract: type receivers cross as their foreign face — the
	// declared offset indexes the extension's own C struct layout.
	let instance = super::foreignize_type_result(instance);
	// SAFETY: the member offset was declared by the extension against its
	// own instance layout; `instance` is one of its instances.
	let field = unsafe { instance.cast::<u8>().offset(descr.offset) };
	unsafe { read_member(field, descr) }
}

unsafe extern "C" fn member_descr_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let attr = unsafe { crate::types::type_::unicode_text(name) };
	let descr = unsafe { &*object.cast::<PyCMemberDescr>() };
	match attr {
		Some("__name__") | Some("__qualname__") => descriptor_name_attr(descr.name),
		Some("__doc__") => descriptor_doc_attr(descr.doc),
		_ => unsafe { crate::descr::generic_get_attr(object, name) },
	}
}

unsafe extern "C" fn member_descr_setattro(
	object: *mut PyObject,
	name: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	let attr = unsafe { crate::types::type_::unicode_text(name) };
	let descr = unsafe { &mut *object.cast::<PyCMemberDescr>() };
	match attr {
		Some("__doc__") => set_descriptor_doc(&mut descr.doc, value),
		_ => {
			let _ = abi::return_null_with_type_error("object does not support attribute assignment");
			-1
		},
	}
}

unsafe fn read_member(field: *mut u8, descr: &PyCMemberDescr) -> *mut PyObject {
	// SAFETY (whole body): typed loads at extension-declared offsets.
	unsafe {
		match descr.kind {
			T_SHORT => abi::pon_const_int(i64::from(field.cast::<i16>().read())),
			T_INT => abi::pon_const_int(i64::from(field.cast::<c_int>().read())),
			T_LONG => abi::pon_const_int(field.cast::<i64>().read()),
			T_LONGLONG => abi::pon_const_int(field.cast::<i64>().read()),
			T_PYSSIZET => abi::pon_const_int(field.cast::<isize>().read() as i64),
			T_BYTE => abi::pon_const_int(i64::from(field.cast::<i8>().read())),
			T_UBYTE => abi::pon_const_int(i64::from(field.cast::<u8>().read())),
			T_USHORT => abi::pon_const_int(i64::from(field.cast::<u16>().read())),
			T_UINT => abi::pon_const_int(i64::from(field.cast::<u32>().read())),
			T_ULONG | T_ULONGLONG => {
				let value = field.cast::<u64>().read();
				match i64::try_from(value) {
					Ok(value) => abi::pon_const_int(value),
					Err(_) => {
						raise_type_error("unsigned member exceeds i64 range");
						ptr::null_mut()
					},
				}
			},
			T_FLOAT => crate::types::float::from_f64(f64::from(field.cast::<f32>().read())),
			T_DOUBLE => crate::types::float::from_f64(field.cast::<f64>().read()),
			T_BOOL => crate::types::bool_::from_bool(field.cast::<u8>().read() != 0),
			T_CHAR => {
				let byte = field.cast::<c_char>().read() as u8;
				abi::pon_const_str([byte].as_ptr(), 1)
			},
			T_STRING => {
				let text = field.cast::<*const c_char>().read();
				match c_string(text) {
					Some(text) => abi::pon_const_str(text.as_ptr(), text.len()),
					None => abi::pon_none(),
				}
			},
			T_OBJECT | T_OBJECT_EX => {
				let value = field.cast::<*mut PyObject>().read();
				if !value.is_null() {
					super::py_normalize_foreign(value)
				} else if descr.kind == T_OBJECT {
					abi::pon_none()
				} else {
					let name = crate::intern::resolve(descr.name).unwrap_or_default();
					crate::abi::exc::raise_attribute_error_text(&name)
				}
			},
			T_NONE => abi::pon_none(),
			_ => {
				raise_type_error("unsupported PyMemberDef type code");
				ptr::null_mut()
			},
		}
	}
}

unsafe extern "C" fn member_descr_set(
	descriptor: *mut PyObject,
	instance: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	// SAFETY: dispatched only for PyCMemberDescr values.
	let descr = unsafe { &*descriptor.cast::<PyCMemberDescr>() };
	if descr.flags & READONLY != 0 {
		let name = crate::intern::resolve(descr.name).unwrap_or_default();
		raise_type_error(format!("attribute '{name}' is read-only"));
		return -1;
	}
	if instance.is_null() {
		raise_type_error("member assignment needs an instance");
		return -1;
	}
	let instance = super::foreignize_type_result(instance);
	// SAFETY: extension-declared offset into one of its instances.
	let field = unsafe { instance.cast::<u8>().offset(descr.offset) };
	unsafe { write_member(field, descr, value) }
}

unsafe fn write_member(field: *mut u8, descr: &PyCMemberDescr, value: *mut PyObject) -> c_int {
	let as_i64 = |value: *mut PyObject| -> Option<i64> {
		let untagged = crate::tag::untag_arg(value);
		// SAFETY: untagged live object.
		unsafe { crate::types::int::to_bigint_including_bool(untagged) }
			.and_then(|big| num_traits::ToPrimitive::to_i64(&big))
	};
	// SAFETY (whole body): typed stores at extension-declared offsets.
	unsafe {
		match descr.kind {
			T_SHORT | T_INT | T_LONG | T_LONGLONG | T_PYSSIZET | T_BYTE | T_UBYTE | T_USHORT
			| T_UINT | T_ULONG | T_ULONGLONG => {
				let Some(number) = as_i64(value) else {
					raise_type_error("an integer is required");
					return -1;
				};
				match descr.kind {
					T_SHORT => field.cast::<i16>().write(number as i16),
					T_INT => field.cast::<c_int>().write(number as c_int),
					T_LONG | T_LONGLONG => field.cast::<i64>().write(number),
					T_PYSSIZET => field.cast::<isize>().write(number as isize),
					T_BYTE => field.cast::<i8>().write(number as i8),
					T_UBYTE => field.cast::<u8>().write(number as u8),
					T_USHORT => field.cast::<u16>().write(number as u16),
					T_UINT => field.cast::<u32>().write(number as u32),
					_ => field.cast::<u64>().write(number as u64),
				}
				0
			},
			T_FLOAT | T_DOUBLE => {
				let untagged = crate::tag::untag_arg(value);
				let number = if let Some(number) = crate::types::float::to_f64(untagged) {
					Some(number)
				} else {
					as_i64(value).map(|number| number as f64)
				};
				let Some(number) = number else {
					raise_type_error("a number is required");
					return -1;
				};
				if descr.kind == T_FLOAT {
					field.cast::<f32>().write(number as f32);
				} else {
					field.cast::<f64>().write(number);
				}
				0
			},
			T_BOOL => {
				let untagged = crate::tag::untag_arg(value);
				let Some(flag) = crate::types::bool_::to_bool(untagged) else {
					raise_type_error("attribute value type must be bool");
					return -1;
				};
				field.cast::<u8>().write(u8::from(flag));
				0
			},
			T_OBJECT | T_OBJECT_EX => {
				// Raw store: declared object members are traced precisely by
				// `trace_capi_instance`, so the value lives as long as the
				// instance without pin bookkeeping.
				if value.is_null()
					&& descr.kind == T_OBJECT_EX
					&& field.cast::<*mut PyObject>().read().is_null()
				{
					let name = crate::intern::resolve(descr.name).unwrap_or_default();
					let _ = crate::abi::exc::raise_attribute_error_text(&name);
					return -1;
				}
				field.cast::<*mut PyObject>().write(value);
				0
			},
			_ => {
				raise_type_error("unsupported PyMemberDef type code");
				-1
			},
		}
	}
}

#[cfg(test)]
mod tests {
	use core::ptr;

	use super::super::{
		load_extension_module,
		tests::{ResetImportStateOnDrop, TempExtensionRoot, compile_extension},
	};
	use crate::{
		abi::{format_object_for_print, pon_call, pon_const_int, pon_get_attr, pon_runtime_init},
		import::module_attr,
		intern::intern,
		object::PyObject,
		thread_state::{pon_err_message, test_state_lock},
	};

	/// Static `Counter` type: custom `tp_new`/`tp_init`/`tp_repr`/`tp_dealloc`,
	/// a METH_NOARGS method, T_LONG and T_OBJECT_EX members, a read-only getset.
	const COUNTER_SOURCE: &str = r#"
#include <Python.h>
#include <structmember.h>

typedef struct {
    PyObject_HEAD
    long value;
    PyObject *label;
} CounterObject;

static long counter_dealloc_count = 0;

static PyObject *Counter_new(PyTypeObject *type, PyObject *args, PyObject *kwds) {
    (void)args;
    (void)kwds;
    return type->tp_alloc(type, 0);
}

static int Counter_init(PyObject *self, PyObject *args, PyObject *kwds) {
    CounterObject *c = (CounterObject *)self;
    long value = 0;
    (void)kwds;
    if (!PyArg_ParseTuple(args, "|l", &value)) {
        return -1;
    }
    c->value = value;
    return 0;
}

static void Counter_dealloc(PyObject *self) {
    CounterObject *c = (CounterObject *)self;
    counter_dealloc_count += 1;
    Py_CLEAR(c->label);
    Py_TYPE(self)->tp_free(self);
}

static PyObject *Counter_repr(PyObject *self) {
    CounterObject *c = (CounterObject *)self;
    return PyUnicode_FromFormat("Counter(%ld)", c->value);
}

static PyObject *Counter_increment(PyObject *self, PyObject *args) {
    CounterObject *c = (CounterObject *)self;
    (void)args;
    c->value += 1;
    return PyLong_FromLong(c->value);
}

static PyObject *Counter_get_twice(PyObject *self, void *closure) {
    CounterObject *c = (CounterObject *)self;
    (void)closure;
    return PyLong_FromLong(c->value * 2);
}

static PyMethodDef Counter_methods[] = {
    {"increment", Counter_increment, METH_NOARGS, "bump and return value"},
    {NULL, NULL, 0, NULL},
};

static PyMemberDef Counter_members[] = {
    {"value", T_LONG, offsetof(CounterObject, value), 0, "current count"},
    {"label", T_OBJECT_EX, offsetof(CounterObject, label), 0, "optional tag"},
    {NULL, 0, 0, 0, NULL},
};

static PyGetSetDef Counter_getset[] = {
    {"twice", Counter_get_twice, NULL, "value doubled", NULL},
    {NULL, NULL, NULL, NULL, NULL},
};

static PyTypeObject CounterType = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "capi_typeobj_ext.Counter",
    .tp_basicsize = sizeof(CounterObject),
    .tp_dealloc = Counter_dealloc,
    .tp_repr = Counter_repr,
    .tp_flags = Py_TPFLAGS_DEFAULT,
    .tp_methods = Counter_methods,
    .tp_members = Counter_members,
    .tp_getset = Counter_getset,
    .tp_init = Counter_init,
    .tp_new = Counter_new,
};

/* Returns a bitmask of passed checks; Rust asserts the full mask. */
static PyObject *drive(PyObject *self, PyObject *args) {
    long ok = 0;
    (void)self;
    (void)args;

    PyObject *seven = PyLong_FromLong(7);
    PyObject *obj = PyObject_CallOneArg((PyObject *)&CounterType, seven);
    if (obj == NULL) {
        return NULL;
    }
    ok |= 1L << 0;
    if (Py_TYPE(obj) == &CounterType) ok |= 1L << 1;
    if (PyObject_IsInstance(obj, (PyObject *)&CounterType) == 1) ok |= 1L << 2;

    /* tp_init parsed the real args tuple. */
    PyObject *value = PyObject_GetAttrString(obj, "value");
    if (value != NULL && PyLong_Check(value) && PyLong_AsLong(value) == 7) ok |= 1L << 3;
    Py_XDECREF(value);
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    /* METH_NOARGS method bound through the descriptor path. */
    PyObject *meth = PyObject_GetAttrString(obj, "increment");
    if (meth != NULL) {
        PyObject *bumped = PyObject_CallNoArgs(meth);
        if (bumped != NULL && PyLong_AsLong(bumped) == 8) ok |= 1L << 4;
        Py_DECREF(meth);
    }
    if (PyErr_Occurred() != NULL) PyErr_Clear();
    if (((CounterObject *)obj)->value == 8) ok |= 1L << 5;

    /* T_LONG member write through the descriptor. */
    if (PyObject_SetAttrString(obj, "value", PyLong_FromLong(41)) == 0
        && ((CounterObject *)obj)->value == 41) ok |= 1L << 6;
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    /* T_OBJECT_EX: unset read raises AttributeError. */
    PyObject *missing = PyObject_GetAttrString(obj, "label");
    if (missing == NULL && PyErr_Occurred() != NULL) {
        PyErr_Clear();
        ok |= 1L << 7;
    }

    /* T_OBJECT_EX write, then read back the identical object. */
    PyObject *tag = PyUnicode_FromString("tag");
    if (PyObject_SetAttrString(obj, "label", tag) == 0) {
        PyObject *got = PyObject_GetAttrString(obj, "label");
        if (got == tag) ok |= 1L << 8;
        Py_XDECREF(got);
    }
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    /* Read-only getset: get works, set fails. */
    PyObject *twice = PyObject_GetAttrString(obj, "twice");
    if (twice != NULL && PyLong_AsLong(twice) == 82) ok |= 1L << 9;
    Py_XDECREF(twice);
    if (PyErr_Occurred() != NULL) PyErr_Clear();
    if (PyObject_SetAttrString(obj, "twice", PyLong_FromLong(1)) < 0) {
        PyErr_Clear();
        ok |= 1L << 10;
    }

    /* tp_repr through PyObject_Repr. */
    PyObject *repr = PyObject_Repr(obj);
    if (repr != NULL) {
        const char *text = PyUnicode_AsUTF8(repr);
        if (text != NULL && strcmp(text, "Counter(41)") == 0) ok |= 1L << 11;
    }
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    if (PyType_IsSubtype(&CounterType, &CounterType)) ok |= 1L << 12;

    if (CounterType.tp_dict != NULL) ok |= 1L << 13;
    if (CounterType.tp_dict != NULL && PyDict_CheckExact(CounterType.tp_dict)) ok |= 1L << 17;
    PyObject *carrier = CounterType.tp_dict == NULL ? NULL : PyDict_GetItemString(CounterType.tp_dict, "increment");
    if (carrier != NULL) ok |= 1L << 14;
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    PyObject *extra = PyUnicode_FromString("from-tp-dict");
    if (CounterType.tp_dict != NULL && extra != NULL
            && PyDict_SetItemString(CounterType.tp_dict, "extra", extra) == 0) {
        PyObject *got = PyObject_GetAttrString((PyObject *)&CounterType, "extra");
        if (got == extra) ok |= 1L << 15;
        Py_XDECREF(got);
    }
    Py_XDECREF(extra);
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    PyObject *pre_ready = PyObject_GetAttrString((PyObject *)&CounterType, "pre_ready");
    if (pre_ready != NULL) {
        const char *text = PyUnicode_AsUTF8(pre_ready);
        if (text != NULL && strcmp(text, "pre-ready") == 0) ok |= 1L << 16;
    }
    Py_XDECREF(pre_ready);
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    Py_DECREF(obj);
    return PyLong_FromLong(ok);
}

static PyObject *dealloc_count(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    return PyLong_FromLong(counter_dealloc_count);
}

static PyMethodDef module_methods[] = {
    {"drive", drive, METH_NOARGS, "exercise the Counter type from C"},
    {"dealloc_count", dealloc_count, METH_NOARGS, "Counter tp_dealloc invocations"},
    {NULL, NULL, 0, NULL},
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_typeobj_ext",
    "PyType_Ready round-trip fixture",
    -1,
    module_methods,
};

PyMODINIT_FUNC PyInit_capi_typeobj_ext(void) {
    PyObject *m;
    PyObject *pre_ready = PyUnicode_FromString("pre-ready");
    CounterType.tp_dict = PyDict_New();
    if (CounterType.tp_dict == NULL || pre_ready == NULL) {
        Py_XDECREF(pre_ready);
        return NULL;
    }
    if (PyDict_SetItemString(CounterType.tp_dict, "pre_ready", pre_ready) < 0) {
        Py_DECREF(pre_ready);
        return NULL;
    }
    Py_DECREF(pre_ready);
    if (PyType_Ready(&CounterType) < 0) {
        return NULL;
    }
    m = PyModule_Create(&module);
    if (m == NULL) {
        return NULL;
    }
    Py_INCREF(&CounterType);
    if (PyModule_AddObject(m, "Counter", (PyObject *)&CounterType) < 0) {
        return NULL;
    }
    return m;
}
"#;

	const CUSTOM_METATYPE_SOURCE: &str = r#"
#include <Python.h>

#define BIT(n) (1L << (n))

typedef struct {
    PyObject_HEAD
} ThingObject;

typedef struct {
    PyTypeObject type;
    int extra;
} ThingTypeObject;

static PyObject *Meta_tag(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    return PyUnicode_FromString("metatype-tag");
}

static PyMethodDef Meta_methods[] = {
    {"tag", Meta_tag, METH_NOARGS, "metatype method"},
    {NULL, NULL, 0, NULL},
};

static PyTypeObject MetaType = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "capi_custom_metatype_ext.Meta",
    .tp_basicsize = sizeof(ThingTypeObject),
    .tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE,
    .tp_methods = Meta_methods,
};

static ThingTypeObject ThingType = {
    {
        PyVarObject_HEAD_INIT(&MetaType, 0)
        .tp_name = "capi_custom_metatype_ext.Thing",
        .tp_basicsize = sizeof(ThingObject),
        .tp_flags = Py_TPFLAGS_DEFAULT,
        .tp_new = PyType_GenericNew,
    },
    314159,
};

static PyObject *drive(PyObject *self, PyObject *args) {
    long ok = 0;
    (void)self;
    (void)args;

    MetaType.tp_base = &PyType_Type;

    if (PyType_Ready(&MetaType) == 0) {
        ok |= BIT(0);
    } else if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }

    if (PyType_Ready((PyTypeObject *)&ThingType) == 0) {
        ok |= BIT(1);
    } else if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }

    if (Py_TYPE((PyObject *)&ThingType) == &MetaType) ok |= BIT(2);
    if (PyObject_IsInstance((PyObject *)&ThingType, (PyObject *)&MetaType) == 1) ok |= BIT(3);
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    PyObject *obj = PyObject_CallObject((PyObject *)&ThingType, NULL);
    if (obj != NULL) {
        ok |= BIT(4);
        if (Py_TYPE(obj) == (PyTypeObject *)&ThingType) ok |= BIT(5);
    } else if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }

    PyObject *tag = PyObject_CallMethod((PyObject *)&ThingType, "tag", NULL);
    if (tag != NULL) {
        const char *text = PyUnicode_AsUTF8(tag);
        if (text != NULL && strcmp(text, "metatype-tag") == 0) ok |= BIT(6);
        Py_DECREF(tag);
    }
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    if (ThingType.extra == 314159) ok |= BIT(7);

    Py_XDECREF(obj);
    return PyLong_FromLong(ok);
}

static PyMethodDef module_methods[] = {
    {"drive", drive, METH_NOARGS, "exercise static custom metatype"},
    {NULL, NULL, 0, NULL},
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_custom_metatype_ext",
    "custom metatype PyType_Ready fixture",
    -1,
    module_methods,
};

PyMODINIT_FUNC PyInit_capi_custom_metatype_ext(void) {
    return PyModule_Create(&module);
}
"#;

	const PROTOCOL_SOURCE: &str = r#"
#include <Python.h>

#define BIT(n) (1L << (n))

typedef struct {
    PyObject_HEAD
} ProtoObject;

static PyTypeObject ProtoType;
static PyTypeObject LeftType;
static long set_count = 0;
static long del_count = 0;
static long last_set_key = 0;
static long last_set_value = 0;
static long last_del_key = 0;

static PyObject *Proto_add(PyObject *left, PyObject *right) {
    if (Py_TYPE(left) == &LeftType && Py_TYPE(right) == &ProtoType) {
        return PyLong_FromLong(7001);
    }
    if (Py_TYPE(left) == &ProtoType && Py_TYPE(right) == &LeftType) {
        return PyLong_FromLong(7002);
    }
    if (Py_TYPE(left) == &ProtoType && Py_TYPE(right) == &ProtoType) {
        return PyLong_FromLong(7003);
    }
    Py_RETURN_NOTIMPLEMENTED;
}

static PyObject *Proto_negative(PyObject *self) {
    (void)self;
    return PyLong_FromLong(7100);
}

static PyObject *Proto_absolute(PyObject *self) {
    (void)self;
    return PyLong_FromLong(7400);
}

static int Proto_bool(PyObject *self) {
    (void)self;
    return 1;
}

static PyObject *Proto_iadd(PyObject *left, PyObject *right) {
    (void)left;
    (void)right;
    return PyLong_FromLong(7200);
}

static PyObject *Proto_power(PyObject *left, PyObject *right, PyObject *modulo) {
    (void)left;
    (void)right;
    return PyLong_FromLong(modulo == Py_None ? 7300 : 7301);
}

static Py_ssize_t Proto_sq_length(PyObject *self) {
    (void)self;
    return 5;
}

static PyObject *Proto_sq_item(PyObject *self, Py_ssize_t index) {
    (void)self;
    return PyLong_FromLong(8000 + (long)index);
}

static int Proto_sq_contains(PyObject *self, PyObject *value) {
    (void)self;
    long needle = PyLong_AsLong(value);
    if (PyErr_Occurred() != NULL) {
        return -1;
    }
    return needle == 7;
}

static PyObject *Proto_mp_subscript(PyObject *self, PyObject *key) {
    (void)self;
    long index = PyLong_AsLong(key);
    if (PyErr_Occurred() != NULL) {
        return NULL;
    }
    return PyLong_FromLong(8100 + index);
}

static int Proto_mp_ass_subscript(PyObject *self, PyObject *key, PyObject *value) {
    (void)self;
    long index = PyLong_AsLong(key);
    if (PyErr_Occurred() != NULL) {
        return -1;
    }
    if (value == NULL) {
        del_count += 1;
        last_del_key = index;
        return 0;
    }
    long stored = PyLong_AsLong(value);
    if (PyErr_Occurred() != NULL) {
        return -1;
    }
    set_count += 1;
    last_set_key = index;
    last_set_value = stored;
    return 0;
}

static PyObject *Explicit_abs(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    return PyLong_FromLong(9999);
}

static PyMethodDef Proto_methods[] = {
    {"__abs__", Explicit_abs, METH_NOARGS, "explicit method must beat slot wrapper"},
    {NULL, NULL, 0, NULL},
};

static PyNumberMethods Proto_as_number = {
    .nb_add = Proto_add,
    .nb_power = Proto_power,
    .nb_negative = Proto_negative,
    .nb_absolute = Proto_absolute,
    .nb_bool = Proto_bool,
    .nb_inplace_add = Proto_iadd,
};

static PySequenceMethods Proto_as_sequence = {
    .sq_length = Proto_sq_length,
    .sq_item = Proto_sq_item,
    .sq_contains = Proto_sq_contains,
};

static PyMappingMethods Proto_as_mapping = {
    .mp_subscript = Proto_mp_subscript,
    .mp_ass_subscript = Proto_mp_ass_subscript,
};

static PyTypeObject ProtoType = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "capi_protocol_slots_ext.Proto",
    .tp_basicsize = sizeof(ProtoObject),
    .tp_flags = Py_TPFLAGS_DEFAULT,
    .tp_as_number = &Proto_as_number,
    .tp_as_sequence = &Proto_as_sequence,
    .tp_as_mapping = &Proto_as_mapping,
    .tp_methods = Proto_methods,
    .tp_new = PyType_GenericNew,
};

static PyTypeObject LeftType = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "capi_protocol_slots_ext.Left",
    .tp_basicsize = sizeof(ProtoObject),
    .tp_flags = Py_TPFLAGS_DEFAULT,
    .tp_new = PyType_GenericNew,
};

static int check_long_result(PyObject *object, long expected) {
    if (object == NULL) {
        if (PyErr_Occurred() != NULL) PyErr_Clear();
        return 0;
    }
    long value = PyLong_AsLong(object);
    int ok = PyErr_Occurred() == NULL && value == expected;
    Py_DECREF(object);
    if (PyErr_Occurred() != NULL) PyErr_Clear();
    return ok;
}

static int check_bool_result(PyObject *object, int expected) {
    if (object == NULL) {
        if (PyErr_Occurred() != NULL) PyErr_Clear();
        return 0;
    }
    int truth = PyObject_IsTrue(object);
    int ok = truth == expected;
    Py_DECREF(object);
    if (PyErr_Occurred() != NULL) PyErr_Clear();
    return ok;
}

static int check_none_result(PyObject *object) {
    if (object == NULL) {
        if (PyErr_Occurred() != NULL) PyErr_Clear();
        return 0;
    }
    int ok = object == Py_None;
    Py_DECREF(object);
    if (PyErr_Occurred() != NULL) PyErr_Clear();
    return ok;
}

static int check_attr_call0(PyObject *receiver, const char *name, long expected) {
    PyObject *method = PyObject_GetAttrString(receiver, name);
    if (method == NULL) {
        if (PyErr_Occurred() != NULL) PyErr_Clear();
        return 0;
    }
    PyObject *result = PyObject_CallNoArgs(method);
    Py_DECREF(method);
    return check_long_result(result, expected);
}

static int check_attr_call1(PyObject *receiver, const char *name, PyObject *arg, long expected) {
    PyObject *method = PyObject_GetAttrString(receiver, name);
    if (method == NULL) {
        if (PyErr_Occurred() != NULL) PyErr_Clear();
        return 0;
    }
    PyObject *result = PyObject_CallOneArg(method, arg);
    Py_DECREF(method);
    return check_long_result(result, expected);
}

static int check_attr_call1_bool(PyObject *receiver, const char *name, PyObject *arg, int expected) {
    PyObject *method = PyObject_GetAttrString(receiver, name);
    if (method == NULL) {
        if (PyErr_Occurred() != NULL) PyErr_Clear();
        return 0;
    }
    PyObject *result = PyObject_CallOneArg(method, arg);
    Py_DECREF(method);
    return check_bool_result(result, expected);
}

static int check_attr_setitem(PyObject *receiver, PyObject *key, PyObject *value) {
    PyObject *method = PyObject_GetAttrString(receiver, "__setitem__");
    if (method == NULL) {
        if (PyErr_Occurred() != NULL) PyErr_Clear();
        return 0;
    }
    PyObject *args = PyTuple_Pack(2, key, value);
    PyObject *result = args == NULL ? NULL : PyObject_CallObject(method, args);
    Py_XDECREF(args);
    Py_DECREF(method);
    return check_none_result(result);
}

static int check_attr_delitem(PyObject *receiver, PyObject *key) {
    PyObject *method = PyObject_GetAttrString(receiver, "__delitem__");
    if (method == NULL) {
        if (PyErr_Occurred() != NULL) PyErr_Clear();
        return 0;
    }
    PyObject *result = PyObject_CallOneArg(method, key);
    Py_DECREF(method);
    return check_none_result(result);
}

static PyObject *drive(PyObject *self, PyObject *args) {
    long ok = 0;
    (void)self;
    (void)args;

    PyObject *a = PyObject_CallNoArgs((PyObject *)&ProtoType);
    PyObject *b = PyObject_CallNoArgs((PyObject *)&ProtoType);
    PyObject *left = PyObject_CallNoArgs((PyObject *)&LeftType);
    PyObject *key = PyLong_FromLong(3);
    PyObject *value = PyLong_FromLong(11);
    PyObject *seven = PyLong_FromLong(7);
    if (a == NULL || b == NULL || left == NULL || key == NULL || value == NULL || seven == NULL) {
        Py_XDECREF(a);
        Py_XDECREF(b);
        Py_XDECREF(left);
        Py_XDECREF(key);
        Py_XDECREF(value);
        Py_XDECREF(seven);
        return NULL;
    }

    if (check_long_result(Proto_as_number.nb_add(a, b), 7003)) ok |= BIT(0);
    if (check_long_result(Proto_as_number.nb_power(a, b, Py_None), 7300)) ok |= BIT(1);
    if (check_attr_call0(a, "__neg__", 7100)) ok |= BIT(2);
    if (check_attr_call1(a, "__add__", b, 7003)) ok |= BIT(3);
    if (check_attr_call1(b, "__radd__", left, 7001)) ok |= BIT(4);
    if (check_attr_call1(a, "__iadd__", b, 7200)) ok |= BIT(5);
    if (check_attr_call0(a, "__abs__", 9999)) ok |= BIT(6);
    if (check_long_result(PyObject_GetItem(a, key), 8103)) ok |= BIT(7);
    if (check_attr_call1(a, "__getitem__", key, 8103)) ok |= BIT(8);
    if (PyObject_SetItem(a, key, value) == 0 && set_count == 1 && last_set_key == 3 && last_set_value == 11) ok |= BIT(9);
    if (PyObject_DelItem(a, key) == 0 && del_count == 1 && last_del_key == 3) ok |= BIT(10);
    if (check_attr_setitem(a, key, value) && set_count == 2 && last_set_key == 3 && last_set_value == 11) ok |= BIT(11);
    if (check_attr_delitem(a, key) && del_count == 2 && last_del_key == 3) ok |= BIT(12);
    if (PySequence_Contains(a, seven) == 1) ok |= BIT(13);
    if (check_attr_call1_bool(a, "__contains__", seven, 1)) ok |= BIT(14);
    if (PyObject_IsTrue(a) == 1) ok |= BIT(15);
    if (PyObject_Size(a) == 5) ok |= BIT(16);
    if (check_attr_call0(a, "__len__", 5)) ok |= BIT(17);
    if (check_long_result(Proto_as_sequence.sq_item(a, 4), 8004)) ok |= BIT(18);
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    Py_DECREF(a);
    Py_DECREF(b);
    Py_DECREF(left);
    Py_DECREF(key);
    Py_DECREF(value);
    Py_DECREF(seven);
    return PyLong_FromLong(ok);
}

static PyMethodDef module_methods[] = {
    {"drive", drive, METH_NOARGS, "exercise protocol slot wrappers"},
    {NULL, NULL, 0, NULL},
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_protocol_slots_ext",
    "protocol slot wrapper fixture",
    -1,
    module_methods,
};

PyMODINIT_FUNC PyInit_capi_protocol_slots_ext(void) {
    if (PyType_Ready(&LeftType) < 0) {
        return NULL;
    }
    if (PyType_Ready(&ProtoType) < 0) {
        return NULL;
    }
    PyObject *m = PyModule_Create(&module);
    if (m == NULL) {
        return NULL;
    }
    Py_INCREF(&ProtoType);
    if (PyModule_AddObject(m, "Proto", (PyObject *)&ProtoType) < 0) {
        return NULL;
    }
    Py_INCREF(&LeftType);
    if (PyModule_AddObject(m, "Left", (PyObject *)&LeftType) < 0) {
        return NULL;
    }
    return m;
}
"#;

	/// Kept out of line so the constructed instance's only stack slots die
	/// with this frame: the conservative external-stack scan must not see
	/// them once the caller collects.
	#[inline(never)]
	fn construct_and_probe_counter(counter: *mut PyObject) {
		let mut argv = [unsafe { pon_const_int(7) }];
		// pon-side construction: call_type_from_argv -> tp_new trampoline ->
		// tp_alloc -> bridged tp_init with the real args tuple.
		let instance = unsafe { pon_call(counter, argv.as_mut_ptr(), argv.len()) };
		assert!(!instance.is_null(), "Counter(7) returned NULL: {:?}", pon_err_message());
		// str falls back to tp_repr (no tp_str installed).
		assert_eq!(format_object_for_print(instance).as_deref(), Ok("Counter(7)"));
	}

	#[test]
	fn capi_static_type_round_trips_through_type_ready() {
		let _guard = test_state_lock();
		let _reset = ResetImportStateOnDrop;
		unsafe {
			assert_eq!(pon_runtime_init(), 0);
		}

		let temp = TempExtensionRoot::new();
		let module_path = compile_extension(&temp, "capi_typeobj_ext", COUNTER_SOURCE);
		let module = load_extension_module("capi_typeobj_ext", &module_path)
			.unwrap_or_else(|message| panic!("failed to load C extension: {message}"));
		assert!(!module.is_null(), "extension loader returned NULL module");

		let module_name = intern("capi_typeobj_ext");

		// C-side probe: all eighteen bits must hold; a partial mask names
		// the first failing surface.
		let drive = module_attr(module_name, intern("drive")).expect("drive registered");
		let result = unsafe { pon_call(drive, ptr::null_mut(), 0) };
		assert!(!result.is_null(), "drive() returned NULL: {:?}", pon_err_message());
		assert_eq!(
			format_object_for_print(result).as_deref(),
			Ok("262143"),
			"C-side bitmask mismatch"
		);

		let counter = module_attr(module_name, intern("Counter")).expect("Counter registered");
		let extra = unsafe { pon_get_attr(counter, intern("extra"), ptr::null_mut()) };
		assert!(!extra.is_null(), "pon-side Counter.extra lookup failed: {:?}", pon_err_message());
		assert_eq!(format_object_for_print(extra).as_deref(), Ok("from-tp-dict"));
		construct_and_probe_counter(counter);

		// Dealloc bridge: both instances are garbage now. The first collect
		// runs the deferred tp_dealloc (objects stay valid through it), the
		// second reclaims the blocks.
		crate::abi::collect().expect("first collect");
		crate::abi::collect().expect("second collect");
		let count_fn =
			module_attr(module_name, intern("dealloc_count")).expect("dealloc_count registered");
		let count_object = unsafe { pon_call(count_fn, ptr::null_mut(), 0) };
		assert!(!count_object.is_null(), "dealloc_count() returned NULL: {:?}", pon_err_message());
		let deallocs: i64 = format_object_for_print(count_object)
			.expect("dealloc_count formats")
			.parse()
			.expect("dealloc_count returns an int");
		// The C-side instance has no surviving root and MUST be finalized;
		// the pon-side one may be conservatively retained by test-frame
		// stack ghosts, so 1 or 2 are both sound outcomes.
		assert!(
			(1..=2).contains(&deallocs),
			"expected 1-2 Counter deallocs after two collects, got {deallocs}"
		);
	}

	#[test]
	fn capi_static_protocol_tables_install_slot_wrappers() {
		let _guard = test_state_lock();
		let _reset = ResetImportStateOnDrop;
		unsafe {
			assert_eq!(pon_runtime_init(), 0);
		}

		let temp = TempExtensionRoot::new();
		let module_path = compile_extension(&temp, "capi_protocol_slots_ext", PROTOCOL_SOURCE);
		let module = load_extension_module("capi_protocol_slots_ext", &module_path)
			.unwrap_or_else(|message| panic!("failed to load protocol C extension: {message}"));
		assert!(!module.is_null(), "extension loader returned NULL module");

		let module_name = intern("capi_protocol_slots_ext");
		let drive = module_attr(module_name, intern("drive")).expect("drive registered");
		let result = unsafe { pon_call(drive, ptr::null_mut(), 0) };
		assert!(!result.is_null(), "drive() returned NULL: {:?}", pon_err_message());
		assert_eq!(
			format_object_for_print(result).as_deref(),
			Ok("524287"),
			"C-side protocol bitmask mismatch"
		);

		let proto_type = module_attr(module_name, intern("Proto")).expect("Proto registered");
		let left_type = module_attr(module_name, intern("Left")).expect("Left registered");
		let proto_a = unsafe { pon_call(proto_type, ptr::null_mut(), 0) };
		let proto_b = unsafe { pon_call(proto_type, ptr::null_mut(), 0) };
		let left = unsafe { pon_call(left_type, ptr::null_mut(), 0) };
		assert!(
			!proto_a.is_null() && !proto_b.is_null() && !left.is_null(),
			"instance construction failed: {:?}",
			pon_err_message()
		);

		let sum = unsafe {
			crate::abi::number::pon_binary_op(
				crate::abstract_op::BINARY_ADD,
				proto_a,
				proto_b,
				ptr::null_mut(),
			)
		};
		assert!(!sum.is_null(), "Proto + Proto returned NULL: {:?}", pon_err_message());
		assert_eq!(format_object_for_print(sum).as_deref(), Ok("7003"));

		let reflected = unsafe {
			crate::abi::number::pon_binary_op(
				crate::abstract_op::BINARY_ADD,
				left,
				proto_b,
				ptr::null_mut(),
			)
		};
		assert!(!reflected.is_null(), "Left + Proto returned NULL: {:?}", pon_err_message());
		assert_eq!(format_object_for_print(reflected).as_deref(), Ok("7001"));
	}
	#[test]
	fn capi_static_custom_metatype_instances_are_type_objects() {
		let _guard = test_state_lock();
		let _reset = ResetImportStateOnDrop;
		unsafe {
			assert_eq!(pon_runtime_init(), 0);
		}

		let temp = TempExtensionRoot::new();
		let module_path =
			compile_extension(&temp, "capi_custom_metatype_ext", CUSTOM_METATYPE_SOURCE);
		let module = load_extension_module("capi_custom_metatype_ext", &module_path)
			.unwrap_or_else(|message| panic!("failed to load custom metatype C extension: {message}"));
		assert!(!module.is_null(), "extension loader returned NULL module");

		let module_name = intern("capi_custom_metatype_ext");
		let drive = module_attr(module_name, intern("drive")).expect("drive registered");
		let result = unsafe { pon_call(drive, ptr::null_mut(), 0) };
		assert!(!result.is_null(), "drive() returned NULL: {:?}", pon_err_message());
		assert_eq!(format_object_for_print(result).as_deref(), Ok("255"), "C-side bitmask mismatch");
	}
}

#[cfg(test)]
mod fromspec_tests {
	use core::ptr;

	use super::super::{
		load_extension_module,
		tests::{ResetImportStateOnDrop, TempExtensionRoot, compile_extension},
	};
	use crate::{
		abi::{format_object_for_print, pon_call, pon_runtime_init},
		import::module_attr,
		intern::intern,
		thread_state::{pon_err_message, test_state_lock},
	};

	const FROM_SPEC_SOURCE: &str = r#"
#include <Python.h>
#include <structmember.h>

typedef struct {
    PyObject_HEAD
    long value;
} FromSpecObject;

static PyTypeObject *FromSpec_Type = NULL;

static PyObject *FromSpec_new(PyTypeObject *type, PyObject *args, PyObject *kwds) {
    (void)args;
    (void)kwds;
    return type->tp_alloc(type, 0);
}

static int FromSpec_init(PyObject *self, PyObject *args, PyObject *kwds) {
    FromSpecObject *obj = (FromSpecObject *)self;
    long value = 0;
    (void)kwds;
    if (!PyArg_ParseTuple(args, "|l", &value)) {
        return -1;
    }
    obj->value = value;
    return 0;
}

static PyObject *FromSpec_repr(PyObject *self) {
    FromSpecObject *obj = (FromSpecObject *)self;
    return PyUnicode_FromFormat("FromSpecThing(%ld)", obj->value);
}

static PyObject *FromSpec_bump(PyObject *self, PyObject *args) {
    FromSpecObject *obj = (FromSpecObject *)self;
    (void)args;
    obj->value += 1;
    return PyLong_FromLong(obj->value);
}

static PyObject *FromSpec_add(PyObject *left, PyObject *right) {
    (void)left;
    (void)right;
    return PyLong_FromLong(77);
}

static Py_ssize_t FromSpec_len(PyObject *self) {
    (void)self;
    return 12;
}

static PyObject *Bad_await(PyObject *self) {
    (void)self;
    Py_RETURN_NONE;
}

static PyMethodDef FromSpec_methods[] = {
    {"bump", FromSpec_bump, METH_NOARGS, "increment value"},
    {NULL, NULL, 0, NULL},
};

static PyMemberDef FromSpec_members[] = {
    {"value", T_LONG, offsetof(FromSpecObject, value), 0, "stored value"},
    {NULL, 0, 0, 0, NULL},
};

static PyType_Slot FromSpec_slots[] = {
    {Py_tp_methods, FromSpec_methods},
    {Py_tp_members, FromSpec_members},
    {Py_tp_new, FromSpec_new},
    {Py_tp_init, FromSpec_init},
    {Py_tp_repr, FromSpec_repr},
    {Py_nb_add, FromSpec_add},
    {Py_sq_length, FromSpec_len},
    {0, NULL},
};

static PyType_Spec FromSpec_spec = {
    "capi_fromspec_ext.FromSpecThing",
    sizeof(FromSpecObject),
    0,
    Py_TPFLAGS_DEFAULT,
    FromSpec_slots,
};

static PyType_Slot Bad_slots[] = {
    {Py_am_await, Bad_await},
    {0, NULL},
};

static PyType_Spec Bad_spec = {
    "capi_fromspec_ext.Bad",
    sizeof(FromSpecObject),
    0,
    Py_TPFLAGS_DEFAULT,
    Bad_slots,
};

static PyObject *drive(PyObject *self, PyObject *args) {
    long ok = 0;
    (void)self;
    (void)args;

    if (FromSpec_Type != NULL) ok |= 1L << 0;
    PyObject *five = PyLong_FromLong(5);
    PyObject *obj = PyObject_CallOneArg((PyObject *)FromSpec_Type, five);
    if (obj != NULL) ok |= 1L << 1;
    if (obj != NULL && Py_TYPE(obj) == FromSpec_Type) ok |= 1L << 2;

    PyObject *value = obj == NULL ? NULL : PyObject_GetAttrString(obj, "value");
    if (value != NULL && PyLong_AsLong(value) == 5) ok |= 1L << 3;
    Py_XDECREF(value);
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    PyObject *method = obj == NULL ? NULL : PyObject_GetAttrString(obj, "bump");
    if (method != NULL) {
        PyObject *bumped = PyObject_CallNoArgs(method);
        if (bumped != NULL && PyLong_AsLong(bumped) == 6 && ((FromSpecObject *)obj)->value == 6) ok |= 1L << 4;
        Py_XDECREF(bumped);
        Py_DECREF(method);
    }
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    if (obj != NULL && PyObject_SetAttrString(obj, "value", PyLong_FromLong(41)) == 0
        && ((FromSpecObject *)obj)->value == 41) ok |= 1L << 5;
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    PyObject *repr = obj == NULL ? NULL : PyObject_Repr(obj);
    if (repr != NULL) {
        const char *text = PyUnicode_AsUTF8(repr);
        if (text != NULL && strcmp(text, "FromSpecThing(41)") == 0) ok |= 1L << 6;
    }
    Py_XDECREF(repr);
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    PyObject *sum = (obj == NULL || FromSpec_Type->tp_as_number == NULL) ? NULL : FromSpec_Type->tp_as_number->nb_add(obj, obj);
    if (sum != NULL && PyLong_AsLong(sum) == 77) ok |= 1L << 7;
    Py_XDECREF(sum);
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    if (obj != NULL && PyObject_Size(obj) == 12) ok |= 1L << 8;
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    PyObject *add_method = obj == NULL ? NULL : PyObject_GetAttrString(obj, "__add__");
    if (add_method != NULL) {
        PyObject *dunder_sum = PyObject_CallOneArg(add_method, obj);
        if (dunder_sum != NULL && PyLong_AsLong(dunder_sum) == 77) ok |= 1L << 9;
        Py_XDECREF(dunder_sum);
        Py_DECREF(add_method);
    }
    if (PyErr_Occurred() != NULL) PyErr_Clear();

    PyObject *bad = PyType_FromSpec(&Bad_spec);
    if (bad == NULL && PyErr_ExceptionMatches(PyExc_TypeError)) {
        PyErr_Clear();
        ok |= 1L << 10;
    } else {
        Py_XDECREF(bad);
        if (PyErr_Occurred() != NULL) PyErr_Clear();
    }

    Py_XDECREF(obj);
    return PyLong_FromLong(ok);
}

static PyMethodDef module_methods[] = {
    {"drive", drive, METH_NOARGS, "exercise PyType_FromSpec"},
    {NULL, NULL, 0, NULL},
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_fromspec_ext",
    "PyType_FromSpec fixture",
    -1,
    module_methods,
};

PyMODINIT_FUNC PyInit_capi_fromspec_ext(void) {
    PyObject *m;
    FromSpec_Type = (PyTypeObject *)PyType_FromSpec(&FromSpec_spec);
    if (FromSpec_Type == NULL) {
        return NULL;
    }
    m = PyModule_Create(&module);
    if (m == NULL) {
        return NULL;
    }
    Py_INCREF(FromSpec_Type);
    if (PyModule_AddObject(m, "FromSpecThing", (PyObject *)FromSpec_Type) < 0) {
        return NULL;
    }
    return m;
}
"#;

	#[test]
	fn capi_type_from_spec_builds_heap_type_and_protocol_slots() {
		let _guard = test_state_lock();
		let _reset = ResetImportStateOnDrop;
		unsafe {
			assert_eq!(pon_runtime_init(), 0);
		}

		let temp = TempExtensionRoot::new();
		let module_path = compile_extension(&temp, "capi_fromspec_ext", FROM_SPEC_SOURCE);
		let module = load_extension_module("capi_fromspec_ext", &module_path)
			.unwrap_or_else(|message| panic!("failed to load FromSpec C extension: {message}"));
		assert!(!module.is_null(), "extension loader returned NULL module");

		let module_name = intern("capi_fromspec_ext");
		let drive = module_attr(module_name, intern("drive")).expect("drive registered");
		let result = unsafe { pon_call(drive, ptr::null_mut(), 0) };
		assert!(!result.is_null(), "drive() returned NULL: {:?}", pon_err_message());
		assert_eq!(format_object_for_print(result).as_deref(), Ok("2047"), "C-side bitmask mismatch");
	}
}
