//! typeobj family: `PyType_Ready` and C-defined type instantiation.
//!
//! A C extension's static `PyTypeObject` (foreign, see [`super::twin`]) is
//! translated into a native runtime type by [`capi_type_ready`]:
//! - methods/getset/members/doc become descriptors in a native class dict,
//! - CPython-signature slots (`tp_repr`, `tp_hash`, `tp_call`, `tp_init`,
//!   ...) are bridged pointer-for-pointer (the ABI shapes match),
//! - `tp_new` goes through a trampoline that hands the C function its
//!   FOREIGN type pointer,
//! - instances live on the GC heap ([`TYPE_ID_CAPI_INSTANCE`]) with the C
//!   layout (`tp_basicsize + nitems * tp_itemsize`); `tp_dealloc` is bridged
//!   through the GC's deferred finalizer (objects stay valid for the whole
//!   finalization cycle; reclamation happens a cycle later).
//!
//! Resurrection contract (CPython parity): a `tp_dealloc` that releases its
//! own payload and then keeps the object alive produces a valid-but-torn-down
//! object, exactly as on CPython. The GC layer itself stays sound.
//!
//! Unsupported (loud `PyType_Ready` failure, never silent): GC-tracked types
//! (`Py_TPFLAGS_HAVE_GC`, `tp_traverse`, `tp_clear`) and custom metatypes.

use core::ffi::{c_char, c_int, c_uint, c_void};
use core::ptr;
use std::collections::HashSet;
use std::ffi::CString;
use std::sync::{LazyLock, Mutex};

use pon_gc::{GcTypeInfo, TypeId};

use crate::abi;
use crate::intern::intern;
use crate::object::{InitFunc, PyObject, PyObjectHeader, PyType, as_object_ptr};
use crate::types::exc::ExceptionKind;
use crate::types::type_::{PyClassDict, new_namespace};

use super::c_string;
use super::twin::{self, ForeignTypeObject};

/// GC type id for C-extension instances (registry: fixed ids live in
/// `abi::register_gc_types` and per-module constants; 140 sits next to the
/// native-file id 120 and the carrier id 141 in `capi::mod`).
const TYPE_ID_CAPI_INSTANCE: TypeId = TypeId(140);

// CPython flag bits mirrored in include/Python.h.
const TPFLAGS_READY: u64 = 1 << 12;
const TPFLAGS_HAVE_GC: u64 = 1 << 14;

// CPython stable-ABI slot ids mirrored in include/Python.h/typeslots.h.
const PY_BF_GETBUFFER: c_int = 1;
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
const PY_AM_SEND: c_int = 81;
const PY_TP_FINALIZE: c_int = 80;
const PY_TP_VECTORCALL: c_int = 82;
const PY_TP_TOKEN: c_int = 83;

#[repr(C)]
struct PyTypeSlot {
    slot: c_int,
    pfunc: *mut c_void,
}

#[repr(C)]
struct PyTypeSpec {
    name: *const c_char,
    basicsize: c_int,
    itemsize: c_int,
    flags: c_uint,
    slots: *mut PyTypeSlot,
}

/// Addresses of live C-extension instances allocated on the GC heap.
/// `PyObject_Free` must no-op for these (the GC owns the block); the
/// finalizer drops entries as objects die.
static CAPI_INSTANCES: LazyLock<Mutex<HashSet<usize>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// C mirror: `include/pon_capi/typeobj.h` `PyPonCapiTypeObj`.
#[repr(C)]
pub(crate) struct PyPonCapiTypeObj {
    type_ready: unsafe extern "C" fn(*mut ForeignTypeObject) -> c_int,
    generic_alloc: unsafe extern "C" fn(*mut ForeignTypeObject, isize) -> *mut PyObject,
    generic_new: unsafe extern "C" fn(*mut ForeignTypeObject, *mut PyObject, *mut PyObject) -> *mut PyObject,
    is_subtype: unsafe extern "C" fn(*mut ForeignTypeObject, *mut ForeignTypeObject) -> c_int,
    object_free: unsafe extern "C" fn(*mut c_void),
    object_init: unsafe extern "C" fn(*mut PyObject, *mut ForeignTypeObject) -> *mut PyObject,
    object_new_raw: unsafe extern "C" fn(*mut ForeignTypeObject, isize) -> *mut PyObject,
    type_from_spec: unsafe extern "C" fn(*mut PyTypeSpec) -> *mut PyObject,
    type_from_spec_with_bases: unsafe extern "C" fn(*mut PyTypeSpec, *mut PyObject) -> *mut PyObject,
    type_from_module_and_spec: unsafe extern "C" fn(*mut PyObject, *mut PyTypeSpec, *mut PyObject) -> *mut PyObject,
}

unsafe impl Send for PyPonCapiTypeObj {}
unsafe impl Sync for PyPonCapiTypeObj {}

pub(crate) fn build() -> PyPonCapiTypeObj {
    PyPonCapiTypeObj {
        type_ready: capi_type_ready,
        generic_alloc: capi_generic_alloc,
        generic_new: capi_generic_new,
        is_subtype: capi_is_subtype,
        object_free: capi_object_free,
        object_init: capi_object_init,
        object_new_raw: capi_object_new_raw,
        type_from_spec: capi_type_from_spec,
        type_from_spec_with_bases: capi_type_from_spec_with_bases,
        type_from_module_and_spec: capi_type_from_module_and_spec,
    }
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

    // Unsupported surface gates: loud failure over silent misbehavior.
    if foreign_ref.tp_flags & TPFLAGS_HAVE_GC != 0 || !foreign_ref.tp_traverse.is_null() || !foreign_ref.tp_clear.is_null() {
        raise_type_error(format!("PyType_Ready: GC-tracked C types are not supported yet: {name_full}"));
        return -1;
    }
    if !foreign_ref.ob_type.is_null() {
        // A metatype is only acceptable when it resolves to plain `type`.
        match twin::native_of_foreign(foreign_ref.ob_type) {
            Some(meta) if meta == abi::runtime_type_type() => {}
            _ => {
                raise_type_error(format!("PyType_Ready: custom metatypes are not supported yet: {name_full}"));
                return -1;
            }
        }
    }

    // Base resolution: NULL means `object`.
    let base_native = if foreign_ref.tp_base.is_null() {
        crate::native::builtins_mod::builtin_native_type("object").unwrap_or(ptr::null_mut())
    } else {
        match twin::native_of_foreign(foreign_ref.tp_base) {
            Some(native) => native,
            None => {
                raise_type_error(format!("PyType_Ready: base type of {name_full} is not ready"));
                return -1;
            }
        }
    };
    if base_native.is_null() {
        raise_type_error("PyType_Ready: cannot resolve base type");
        return -1;
    }
    // CPython inheritance: sizes default to the base's.
    // SAFETY: live native base type.
    if foreign_ref.tp_basicsize == 0 {
        foreign_ref.tp_basicsize = unsafe { (*base_native).tp_basicsize } as isize;
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
            (&mut foreign_ref.tp_init, base.tp_init),
            (&mut foreign_ref.tp_alloc, base.tp_alloc),
            (&mut foreign_ref.tp_free, base.tp_free),
        ];
        for (child, parent) in inherited {
            if child.is_null() {
                *child = parent;
            }
        }
    }

    let namespace = new_namespace();
    // SAFETY: fresh namespace; carrier construction only allocates.
    unsafe {
        if !install_namespace(namespace, foreign_ref) {
            return -1;
        }
    }

    // SAFETY: live base type, live namespace, runtime initialized.
    let native = unsafe {
        crate::types::type_::construct_capi_class(
            name,
            &[base_native],
            namespace,
            foreign_ref.tp_basicsize.max(0) as usize,
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
            ty.tp_call = Some(call);
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
        ty.bump_version();
    }

    // Publish the twin BEFORE filling the foreign back-references so
    // trampolines can translate from either side.
    twin::register_foreign_twin(foreign, native_ty);

    // Fill the foreign struct's runtime-owned fields (CPython PyType_Ready
    // parity: inherited slots surface in the static struct).
    foreign_ref.tp_pon_twin = native_ty;
    foreign_ref.tp_flags |= TPFLAGS_READY;
    if foreign_ref.ob_type.is_null() {
        foreign_ref.ob_type = twin::foreign_of_native(abi::runtime_type_type());
    }
    // tp_dict stays NULL in this iteration (documented in typeobj.h): the
    // native class dict has no PyObject header yet, so it cannot cross the
    // boundary as a dict object. Type attributes are reachable through
    // PyObject_GetAttr/SetAttr on the type object instead.
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
    unsafe { capi_type_from_module_and_spec(ptr::null_mut(), spec, ptr::null_mut()) }
}

/// `PyPonCapiTypeObj.type_from_spec_with_bases` (`PyType_FromSpecWithBases`).
unsafe extern "C" fn capi_type_from_spec_with_bases(spec: *mut PyTypeSpec, bases: *mut PyObject) -> *mut PyObject {
    unsafe { capi_type_from_module_and_spec(ptr::null_mut(), spec, bases) }
}

/// `PyPonCapiTypeObj.type_from_module_and_spec` (`PyType_FromModuleAndSpec`).
///
/// Pon does not expose `PyType_GetModule`/module-state lookup yet; the module
/// argument is intentionally ignored while the C type itself is made ready.
unsafe extern "C" fn capi_type_from_module_and_spec(_module: *mut PyObject, spec: *mut PyTypeSpec, bases: *mut PyObject) -> *mut PyObject {
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
    foreign_ptr.cast::<PyObject>()
}

unsafe fn apply_type_spec_slots(foreign: &mut ForeignTypeObject, slots: *mut PyTypeSlot, type_name: &str) -> bool {
    let mut cursor = slots;
    loop {
        // SAFETY: PyType_Spec slot arrays are 0-terminated by contract.
        let slot = unsafe { &*cursor };
        if slot.slot == 0 {
            return true;
        }
        if is_unsupported_protocol_slot(slot.slot) {
            raise_type_error(format!(
                "PyType_FromSpec: protocol slot id {} is not supported yet for {type_name}",
                slot.slot
            ));
            return false;
        }
        let field = slot.pfunc.cast::<()>();
        match slot.slot {
            PY_TP_ALLOC => foreign.tp_alloc = field,
            PY_TP_BASE => {
                if !apply_type_spec_base(foreign, slot.pfunc.cast::<ForeignTypeObject>(), type_name, "Py_tp_base") {
                    return false;
                }
            }
            PY_TP_BASES => {
                if unsafe { !apply_type_spec_bases(foreign, slot.pfunc.cast::<PyObject>(), type_name) } {
                    return false;
                }
            }
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
            PY_TP_DEL | PY_TP_GETATTR | PY_TP_IS_GC | PY_TP_SETATTR | PY_TP_VECTORCALL | PY_TP_TOKEN | PY_AM_AWAIT..=PY_AM_ANEXT | PY_AM_SEND => {
                raise_type_error(format!("PyType_FromSpec: slot id {} is not supported yet for {type_name}", slot.slot));
                return false;
            }
            _ => {
                raise_type_error(format!("PyType_FromSpec: unknown slot id {} for {type_name}", slot.slot));
                return false;
            }
        }
        // SAFETY: 0-terminated slot array; cursor is advanced one element.
        cursor = unsafe { cursor.add(1) };
    }
}

fn is_unsupported_protocol_slot(slot_id: c_int) -> bool {
    (PY_BF_GETBUFFER..=PY_SQ_REPEAT).contains(&slot_id)
        || matches!(slot_id, PY_NB_MATRIX_MULTIPLY | PY_NB_INPLACE_MATRIX_MULTIPLY)
}

fn apply_type_spec_base(foreign: &mut ForeignTypeObject, base: *mut ForeignTypeObject, type_name: &str, source: &str) -> bool {
    if !base.is_null() && twin::registered_native_of_foreign(base).is_none() {
        raise_type_error(format!("PyType_FromSpec: {source} for {type_name} is not a ready foreign PyTypeObject*"));
        return false;
    }
    foreign.tp_base = base;
    true
}

unsafe fn apply_type_spec_bases(foreign: &mut ForeignTypeObject, bases: *mut PyObject, type_name: &str) -> bool {
    if bases.is_null() {
        foreign.tp_bases = ptr::null_mut();
        foreign.tp_base = ptr::null_mut();
        return true;
    }
    let bases = crate::tag::untag_arg(bases);
    let Some(items) = (unsafe { abi::seq::exact_tuple_slice(bases) }) else {
        raise_type_error(format!("PyType_FromSpec: Py_tp_bases for {type_name} must be an exact tuple"));
        return false;
    };
    if items.len() != 1 {
        raise_type_error(format!(
            "PyType_FromSpec: Py_tp_bases for {type_name} must contain exactly one base (got {})",
            items.len()
        ));
        return false;
    }
    if !apply_type_spec_base(foreign, items[0].cast::<ForeignTypeObject>(), type_name, "Py_tp_bases[0]") {
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
            let Some(function) = method.ml_meth else {
                raise_type_error(format!("PyType_Ready: method '{method_name}' has no function"));
                return false;
            };
            let carrier = super::alloc_cfunction(function, method.ml_flags, ptr::null_mut(), &method_name);
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
    true
}

/// `tp_new` bridge: recovers the FOREIGN type for the native class and calls
/// its C `tp_new` (or generic allocation when the extension left it NULL).
unsafe extern "C" fn capi_tp_new_trampoline(cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    let Some(foreign) = twin::registered_foreign_of_native(cls) else {
        return abi::return_null_with_error("C type is not registered with the C-API layer");
    };
    // SAFETY: registered foreign statics stay live for the process.
    let tp_new = unsafe { (*foreign).tp_new };
    if tp_new.is_null() || tp_new == capi_generic_new as *mut () {
        return unsafe { capi_generic_alloc(foreign, 0) };
    }
    let new_fn: unsafe extern "C" fn(*mut ForeignTypeObject, *mut PyObject, *mut PyObject) -> *mut PyObject =
        // SAFETY: tp_new fields hold newfunc pointers by header contract.
        unsafe { core::mem::transmute(tp_new) };
    unsafe { new_fn(foreign, args, kwargs) }
}

/// `PyPonCapiTypeObj.generic_new` (`PyType_GenericNew`).
unsafe extern "C" fn capi_generic_new(foreign: *mut ForeignTypeObject, _args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { capi_generic_alloc(foreign, 0) }
}

/// `PyPonCapiTypeObj.generic_alloc` (`PyType_GenericAlloc`): zeroed C-layout
/// instance on the GC heap; `ob_size` is written only for var-size types.
unsafe extern "C" fn capi_generic_alloc(foreign: *mut ForeignTypeObject, nitems: isize) -> *mut PyObject {
    if foreign.is_null() {
        return abi::return_null_with_error("PyType_GenericAlloc(NULL)");
    }
    let Some(native) = twin::registered_native_of_foreign(foreign) else {
        return abi::return_null_with_error("PyType_GenericAlloc on a type that is not PyType_Ready'd");
    };
    // SAFETY: live foreign static.
    let (basicsize, itemsize) = unsafe { ((*foreign).tp_basicsize, (*foreign).tp_itemsize) };
    let payload = basicsize.max(0) as usize + itemsize.max(0) as usize * nitems.max(0) as usize;
    let size = payload.max(core::mem::size_of::<PyObjectHeader>());
    let info = GcTypeInfo {
        size: core::mem::size_of::<PyObjectHeader>(),
        trace: trace_capi_instance,
        finalize: Some(finalize_capi_instance),
    };
    let block = match abi::alloc_gc_object_sized(TYPE_ID_CAPI_INSTANCE, info, size) {
        Ok(block) => block,
        Err(message) => return abi::return_null_with_error(message),
    };
    let object = block.cast::<PyObject>();
    // SAFETY: fresh zeroed allocation of at least header size.
    unsafe {
        object.write(PyObject {
            ob_type: native,
            gc_meta: crate::object::GcMeta::default(),
        });
        if itemsize > 0 {
            // PyVarObject.ob_size sits directly after the header.
            object.cast::<u8>().add(core::mem::size_of::<PyObjectHeader>()).cast::<isize>().write(nitems);
        }
    }
    CAPI_INSTANCES
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(block as usize);
    as_object_ptr(object)
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
    if cursor.is_null() {
        return;
    }
    // SAFETY: NULL-name terminated array per CPython contract; offsets were
    // declared by the extension against this instance layout.
    unsafe {
        while !(*cursor).name.is_null() {
            if matches!((*cursor).kind, T_OBJECT | T_OBJECT_EX) {
                let value = object.offset((*cursor).offset).cast::<*mut PyObject>().read();
                if !value.is_null() && crate::tag::is_heap(value) {
                    visitor(value.cast::<u8>());
                }
            }
            cursor = cursor.add(1);
        }
    }
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
    CAPI_INSTANCES
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .remove(&(object as usize));
}

/// `PyPonCapiTypeObj.is_subtype` (`PyType_IsSubtype`).
unsafe extern "C" fn capi_is_subtype(a: *mut ForeignTypeObject, b: *mut ForeignTypeObject) -> c_int {
    let (Some(a_native), Some(b_native)) = (twin::native_of_foreign(a), twin::native_of_foreign(b)) else {
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
    // SAFETY: non-instance pointers passed here were PyObject_Malloc'd.
    unsafe { libc::free(ptr) };
}

/// `PyPonCapiTypeObj.object_init` (`PyObject_Init`): stamps the native type
/// into a caller-allocated (malloc'd) object. Such objects are immortal from
/// the GC's perspective.
unsafe extern "C" fn capi_object_init(object: *mut PyObject, foreign: *mut ForeignTypeObject) -> *mut PyObject {
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
    }
    object
}

/// `PyPonCapiTypeObj.object_new_raw` (`PyObject_New`/`PyObject_NewVar`):
/// allocation without calling the C `tp_new`.
unsafe extern "C" fn capi_object_new_raw(foreign: *mut ForeignTypeObject, nitems: isize) -> *mut PyObject {
    unsafe { capi_generic_alloc(foreign, nitems) }
}

/// getset descriptor carrier.
#[repr(C)]
struct CGetSetDef {
    name: *const c_char,
    get: *mut (),
    set: *mut (),
    doc: *const c_char,
    closure: *mut c_void,
}

#[repr(C)]
struct PyGetSetDescr {
    ob_base: PyObjectHeader,
    get: *mut (),
    set: *mut (),
    closure: *mut c_void,
    name: u32,
}

static GETSET_DESCR_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(abi::runtime_type_type(), "getset_descriptor", core::mem::size_of::<PyGetSetDescr>());
    ty.tp_descr_get = Some(getset_descr_get);
    ty.tp_descr_set = Some(getset_descr_set);
    Box::into_raw(Box::new(ty)) as usize
});

fn alloc_getset_descriptor(def: &CGetSetDef, name: &str) -> *mut PyObject {
    let descriptor = Box::new(PyGetSetDescr {
        ob_base: PyObjectHeader::new(*GETSET_DESCR_TYPE as *const PyType),
        get: def.get,
        set: def.set,
        closure: def.closure,
        name: intern(name),
    });
    as_object_ptr(Box::into_raw(descriptor))
}

unsafe extern "C" fn getset_descr_get(descriptor: *mut PyObject, instance: *mut PyObject, _owner: *mut PyObject) -> *mut PyObject {
    // SAFETY: dispatched only for PyGetSetDescr values.
    let descr = unsafe { &*descriptor.cast::<PyGetSetDescr>() };
    if instance.is_null() {
        return descriptor;
    }
    let Some(get) = (unsafe { slot::<unsafe extern "C" fn(*mut PyObject, *mut c_void) -> *mut PyObject>(descr.get) }) else {
        let name = crate::intern::resolve(descr.name).unwrap_or_default();
        return abi::return_null_with_error(format!("attribute '{name}' is not readable"));
    };
    unsafe { get(instance, descr.closure) }
}

unsafe extern "C" fn getset_descr_set(descriptor: *mut PyObject, instance: *mut PyObject, value: *mut PyObject) -> c_int {
    // SAFETY: dispatched only for PyGetSetDescr values.
    let descr = unsafe { &*descriptor.cast::<PyGetSetDescr>() };
    let Some(set) = (unsafe { slot::<unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut c_void) -> c_int>(descr.set) }) else {
        let name = crate::intern::resolve(descr.name).unwrap_or_default();
        raise_type_error(format!("attribute '{name}' is read-only"));
        return -1;
    };
    unsafe { set(instance, value, descr.closure) }
}

/// member descriptor carrier (structmember.h `PyMemberDef`).
#[repr(C)]
struct CMemberDef {
    name: *const c_char,
    kind: c_int,
    offset: isize,
    flags: c_int,
    doc: *const c_char,
}

#[repr(C)]
struct PyCMemberDescr {
    ob_base: PyObjectHeader,
    kind: c_int,
    flags: c_int,
    offset: isize,
    name: u32,
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
    let mut ty = PyType::new(abi::runtime_type_type(), "member_descriptor", core::mem::size_of::<PyCMemberDescr>());
    ty.tp_descr_get = Some(member_descr_get);
    ty.tp_descr_set = Some(member_descr_set);
    Box::into_raw(Box::new(ty)) as usize
});

fn alloc_member_descriptor(def: &CMemberDef, name: &str) -> *mut PyObject {
    let descriptor = Box::new(PyCMemberDescr {
        ob_base: PyObjectHeader::new(*MEMBER_DESCR_TYPE as *const PyType),
        kind: def.kind,
        flags: def.flags,
        offset: def.offset,
        name: intern(name),
    });
    as_object_ptr(Box::into_raw(descriptor))
}

unsafe extern "C" fn member_descr_get(descriptor: *mut PyObject, instance: *mut PyObject, _owner: *mut PyObject) -> *mut PyObject {
    // SAFETY: dispatched only for PyCMemberDescr values.
    let descr = unsafe { &*descriptor.cast::<PyCMemberDescr>() };
    if instance.is_null() {
        return descriptor;
    }
    // SAFETY: the member offset was declared by the extension against its
    // own instance layout; `instance` is one of its instances.
    let field = unsafe { instance.cast::<u8>().offset(descr.offset) };
    unsafe { read_member(field, descr) }
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
                    }
                }
            }
            T_FLOAT => crate::types::float::from_f64(f64::from(field.cast::<f32>().read())),
            T_DOUBLE => crate::types::float::from_f64(field.cast::<f64>().read()),
            T_BOOL => crate::types::bool_::from_bool(field.cast::<u8>().read() != 0),
            T_CHAR => {
                let byte = field.cast::<c_char>().read() as u8;
                abi::pon_const_str([byte].as_ptr(), 1)
            }
            T_STRING => {
                let text = field.cast::<*const c_char>().read();
                match c_string(text) {
                    Some(text) => abi::pon_const_str(text.as_ptr(), text.len()),
                    None => abi::pon_none(),
                }
            }
            T_OBJECT | T_OBJECT_EX => {
                let value = field.cast::<*mut PyObject>().read();
                if !value.is_null() {
                    value
                } else if descr.kind == T_OBJECT {
                    abi::pon_none()
                } else {
                    let name = crate::intern::resolve(descr.name).unwrap_or_default();
                    crate::abi::exc::raise_attribute_error_text(&name)
                }
            }
            T_NONE => abi::pon_none(),
            _ => {
                raise_type_error("unsupported PyMemberDef type code");
                ptr::null_mut()
            }
        }
    }
}

unsafe extern "C" fn member_descr_set(descriptor: *mut PyObject, instance: *mut PyObject, value: *mut PyObject) -> c_int {
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
    // SAFETY: extension-declared offset into one of its instances.
    let field = unsafe { instance.cast::<u8>().offset(descr.offset) };
    unsafe { write_member(field, descr, value) }
}

unsafe fn write_member(field: *mut u8, descr: &PyCMemberDescr, value: *mut PyObject) -> c_int {
    let as_i64 = |value: *mut PyObject| -> Option<i64> {
        let untagged = crate::tag::untag_arg(value);
        // SAFETY: untagged live object.
        unsafe { crate::types::int::to_bigint_including_bool(untagged) }.and_then(|big| num_traits::ToPrimitive::to_i64(&big))
    };
    // SAFETY (whole body): typed stores at extension-declared offsets.
    unsafe {
        match descr.kind {
            T_SHORT | T_INT | T_LONG | T_LONGLONG | T_PYSSIZET | T_BYTE | T_UBYTE | T_USHORT | T_UINT | T_ULONG | T_ULONGLONG => {
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
            }
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
            }
            T_BOOL => {
                let untagged = crate::tag::untag_arg(value);
                let Some(flag) = crate::types::bool_::to_bool(untagged) else {
                    raise_type_error("attribute value type must be bool");
                    return -1;
                };
                field.cast::<u8>().write(u8::from(flag));
                0
            }
            T_OBJECT | T_OBJECT_EX => {
                // Raw store: declared object members are traced precisely by
                // `trace_capi_instance`, so the value lives as long as the
                // instance without pin bookkeeping.
                if value.is_null() && descr.kind == T_OBJECT_EX && field.cast::<*mut PyObject>().read().is_null() {
                    let name = crate::intern::resolve(descr.name).unwrap_or_default();
                    let _ = crate::abi::exc::raise_attribute_error_text(&name);
                    return -1;
                }
                field.cast::<*mut PyObject>().write(value);
                0
            }
            _ => {
                raise_type_error("unsupported PyMemberDef type code");
                -1
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::ptr;

    use super::super::load_extension_module;
    use super::super::tests::{ResetImportStateOnDrop, TempExtensionRoot, compile_extension};
    use crate::abi::{format_object_for_print, pon_call, pon_const_int, pon_runtime_init};
    use crate::import::module_attr;
    use crate::intern::intern;
    use crate::object::PyObject;
    use crate::thread_state::{pon_err_message, test_state_lock};

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

        // C-side probe: all thirteen bits must hold; a partial mask names
        // the first failing surface.
        let drive = module_attr(module_name, intern("drive")).expect("drive registered");
        let result = unsafe { pon_call(drive, ptr::null_mut(), 0) };
        assert!(!result.is_null(), "drive() returned NULL: {:?}", pon_err_message());
        assert_eq!(format_object_for_print(result).as_deref(), Ok("8191"), "C-side bitmask mismatch");

        let counter = module_attr(module_name, intern("Counter")).expect("Counter registered");
        construct_and_probe_counter(counter);

        // Dealloc bridge: both instances are garbage now. The first collect
        // runs the deferred tp_dealloc (objects stay valid through it), the
        // second reclaims the blocks.
        crate::abi::collect().expect("first collect");
        crate::abi::collect().expect("second collect");
        let count_fn = module_attr(module_name, intern("dealloc_count")).expect("dealloc_count registered");
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
}

#[cfg(test)]
mod fromspec_tests {
    use core::ptr;

    use super::super::load_extension_module;
    use super::super::tests::{ResetImportStateOnDrop, TempExtensionRoot, compile_extension};
    use crate::abi::{format_object_for_print, pon_call, pon_runtime_init};
    use crate::import::module_attr;
    use crate::intern::intern;
    use crate::thread_state::{pon_err_message, test_state_lock};

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

static PyObject *Bad_add(PyObject *left, PyObject *right) {
    (void)left;
    (void)right;
    Py_RETURN_NOTIMPLEMENTED;
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
    {Py_nb_add, Bad_add},
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

    PyObject *bad = PyType_FromSpec(&Bad_spec);
    if (bad == NULL && PyErr_ExceptionMatches(PyExc_TypeError)) {
        PyErr_Clear();
        ok |= 1L << 7;
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
    fn capi_type_from_spec_builds_heap_type_and_rejects_protocol_slots() {
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
        assert_eq!(format_object_for_print(result).as_deref(), Ok("255"), "C-side bitmask mismatch");
    }
}
