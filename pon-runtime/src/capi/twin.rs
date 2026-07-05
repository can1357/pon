//! Foreign type-object twins for the C-API compatibility layer.
//!
//! Extension code compiled against Pon's `Python.h` only ever sees FOREIGN
//! `PyTypeObject` pointers: its own static type definitions, or twins owned
//! by this module that mirror runtime-native `PyType`s. The runtime's
//! internal `crate::object::PyType` never crosses the boundary.
//!
//! Registration rules:
//! - Explicitly registered foreign objects (bootstrap-local builtin twins,
//!   `PyType_Ready` statics) WIN as the canonical foreign face of a native
//!   type; later fabrication never replaces them.
//! - `foreign_of_native` is total: a native type with no registered face gets
//!   a runtime-owned twin fabricated on demand.
//! - Readers never observe partially-filled twins: fabrication happens in a
//!   thread-private batch under [`FABRICATION`] (meta/base cycles resolve
//!   inside the batch) and the whole batch publishes atomically.
//! - Lookup keys for builtin ids include BOTH the helper-family type pointer
//!   instances actually carry and the installed canonical builtin global
//!   (see `types::type_::canonical_type_object`).

use core::ffi::{c_char, c_int, c_uint, c_ushort};
use core::ptr;
use std::collections::HashMap;
use std::ffi::CString;
use std::sync::{LazyLock, Mutex, OnceLock};

use crate::abi;
use crate::object::{PyObject, PyType};

/// Builtin twin ids; must match the `PON_TID_*` constants in
/// `include/pon_capi/core.h` (frozen, append-only).
pub(crate) const TID_TYPE: usize = 0;
pub(crate) const TID_OBJECT: usize = 1;
pub(crate) const TID_LONG: usize = 2;
pub(crate) const TID_BOOL: usize = 3;
pub(crate) const TID_FLOAT: usize = 4;
pub(crate) const TID_COMPLEX: usize = 5;
pub(crate) const TID_UNICODE: usize = 6;
pub(crate) const TID_BYTES: usize = 7;
pub(crate) const TID_BYTEARRAY: usize = 8;
pub(crate) const TID_TUPLE: usize = 9;
pub(crate) const TID_LIST: usize = 10;
pub(crate) const TID_DICT: usize = 11;
pub(crate) const TID_SET: usize = 12;
pub(crate) const TID_FROZENSET: usize = 13;
pub(crate) const TID_SLICE: usize = 14;
pub(crate) const TID_MEMORYVIEW: usize = 15;
pub(crate) const TID_CAPSULE: usize = 16;
pub(crate) const TID_NONE_TYPE: usize = 17;
pub(crate) const BUILTIN_TYPE_COUNT: usize = 18;

// CPython type-flag bits mirrored in include/Python.h.
const TPFLAGS_BASETYPE: u64 = 1 << 10;
const TPFLAGS_LONG_SUBCLASS: u64 = 1 << 24;
const TPFLAGS_LIST_SUBCLASS: u64 = 1 << 25;
const TPFLAGS_TUPLE_SUBCLASS: u64 = 1 << 26;
const TPFLAGS_BYTES_SUBCLASS: u64 = 1 << 27;
const TPFLAGS_UNICODE_SUBCLASS: u64 = 1 << 28;
const TPFLAGS_DICT_SUBCLASS: u64 = 1 << 29;
const TPFLAGS_BASE_EXC_SUBCLASS: u64 = 1 << 30;
const TPFLAGS_TYPE_SUBCLASS: u64 = 1 << 31;

/// C-facing `PyTypeObject` mirror; layout must match `struct _typeobject`
/// in `include/Python.h` exactly.
#[repr(C)]
pub(crate) struct ForeignTypeObject {
    pub ob_type: *mut ForeignTypeObject,
    pub gc_meta: usize,
    pub ob_size: isize,
    pub tp_name: *const c_char,
    pub tp_basicsize: isize,
    pub tp_itemsize: isize,
    pub tp_dealloc: *mut (),
    pub tp_vectorcall_offset: isize,
    pub tp_getattr: *mut (),
    pub tp_setattr: *mut (),
    pub tp_as_async: *mut (),
    pub tp_repr: *mut (),
    pub tp_as_number: *mut (),
    pub tp_as_sequence: *mut (),
    pub tp_as_mapping: *mut (),
    pub tp_hash: *mut (),
    pub tp_call: *mut (),
    pub tp_str: *mut (),
    pub tp_getattro: *mut (),
    pub tp_setattro: *mut (),
    pub tp_as_buffer: *mut (),
    pub tp_flags: u64,
    pub tp_doc: *const c_char,
    pub tp_traverse: *mut (),
    pub tp_clear: *mut (),
    pub tp_richcompare: *mut (),
    pub tp_weaklistoffset: isize,
    pub tp_iter: *mut (),
    pub tp_iternext: *mut (),
    pub tp_methods: *mut (),
    pub tp_members: *mut (),
    pub tp_getset: *mut (),
    pub tp_base: *mut ForeignTypeObject,
    pub tp_dict: *mut PyObject,
    pub tp_descr_get: *mut (),
    pub tp_descr_set: *mut (),
    pub tp_dictoffset: isize,
    pub tp_init: *mut (),
    pub tp_alloc: *mut (),
    pub tp_new: *mut (),
    pub tp_free: *mut (),
    pub tp_is_gc: *mut (),
    pub tp_bases: *mut PyObject,
    pub tp_mro: *mut PyObject,
    pub tp_cache: *mut PyObject,
    pub tp_subclasses: *mut (),
    pub tp_weaklist: *mut PyObject,
    pub tp_del: *mut (),
    pub tp_version_tag: c_uint,
    pub tp_finalize: *mut (),
    pub tp_vectorcall: *mut (),
    pub tp_watched: u8,
    pub tp_versions_used: c_ushort,
    pub tp_pon_twin: *mut PyType,
}

unsafe impl Send for ForeignTypeObject {}
unsafe impl Sync for ForeignTypeObject {}

#[derive(Default)]
struct TwinRegistry {
    /// native `*mut PyType` -> canonical foreign face.
    foreign_by_native: HashMap<usize, usize>,
    /// foreign `*mut ForeignTypeObject` -> native `*mut PyType`.
    native_by_foreign: HashMap<usize, usize>,
    /// Twin addresses fabricated by this module; explicit registrations
    /// (bootstrap locals, `PyType_Ready` statics) may displace these as the
    /// canonical face, never other explicit faces.
    fabricated: std::collections::HashSet<usize>,
}

static REGISTRY: LazyLock<Mutex<TwinRegistry>> = LazyLock::new(|| Mutex::new(TwinRegistry::default()));

/// Serializes fabrication batches. Never held while [`REGISTRY`] is locked
/// EXCEPT for the final publish step; readers that miss in the registry queue
/// here, re-check, and only then start their own batch.
static FABRICATION: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

struct BuiltinKey {
    tid: usize,
    /// Every native pointer that identifies this builtin (helper-family
    /// pointer instances carry, plus the installed canonical global).
    natives: Vec<usize>,
    /// Canonical native pointer surfaced outward.
    canonical: usize,
}

unsafe impl Send for BuiltinKey {}
unsafe impl Sync for BuiltinKey {}

/// Built after runtime init (runtime input required, hence `OnceLock`).
static BUILTIN_KEYS: OnceLock<Vec<BuiltinKey>> = OnceLock::new();

fn builtin_keys() -> Option<&'static [BuiltinKey]> {
    if let Some(keys) = BUILTIN_KEYS.get() {
        return Some(keys.as_slice());
    }
    if !abi::runtime_is_initialized() {
        return None;
    }
    let _ = BUILTIN_KEYS.set(collect_builtin_keys());
    BUILTIN_KEYS.get().map(Vec::as_slice)
}

fn collect_builtin_keys() -> Vec<BuiltinKey> {
    let type_type = abi::runtime_type_type();
    let mut keys = Vec::with_capacity(BUILTIN_TYPE_COUNT);
    let mut push = |tid: usize, raw: *mut PyType| {
        if raw.is_null() {
            return;
        }
        // SAFETY: `raw` is a live process-lifetime type object from its getter.
        let canonical = unsafe { crate::types::type_::canonical_type_object(raw) };
        let mut natives = vec![raw as usize];
        if canonical != raw && !canonical.is_null() {
            natives.push(canonical as usize);
        }
        keys.push(BuiltinKey { tid, natives, canonical: canonical as usize });
    };
    push(TID_TYPE, type_type);
    push(TID_OBJECT, crate::native::builtins_mod::builtin_native_type("object").unwrap_or(ptr::null_mut()));
    push(TID_LONG, abi::runtime_long_type());
    push(TID_BOOL, crate::types::bool_::bool_type());
    push(TID_FLOAT, crate::types::float::float_type());
    push(TID_COMPLEX, crate::types::complex_::complex_type());
    push(TID_UNICODE, abi::runtime_unicode_type());
    push(TID_BYTES, crate::types::bytes_::bytes_type());
    push(TID_BYTEARRAY, crate::types::bytearray_::bytearray_type());
    push(TID_TUPLE, abi::seq::tuple_type());
    push(TID_LIST, abi::seq::list_type());
    push(TID_DICT, crate::types::dict::dict_type(type_type));
    push(TID_SET, crate::types::set_::set_type(type_type));
    push(TID_FROZENSET, crate::types::frozenset::frozenset_type(type_type));
    push(TID_SLICE, abi::seq::slice_type());
    push(TID_MEMORYVIEW, crate::types::memoryview::memoryview_type());
    push(TID_CAPSULE, super::runtime_::capsule_type());
    push(TID_NONE_TYPE, abi::runtime_none_type());
    keys
}

fn lookup_foreign(native: *mut PyType) -> Option<*mut ForeignTypeObject> {
    let registry = REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
    registry
        .foreign_by_native
        .get(&(native as usize))
        .map(|&foreign| foreign as *mut ForeignTypeObject)
}

/// Explicit registration (`PyType_Ready` statics): the extension-owned,
/// already-initialized foreign face WINS as canonical for its native type.
pub(crate) fn register_foreign_twin(foreign: *mut ForeignTypeObject, native: *mut PyType) {
    // SAFETY: live type object; lookups canonicalize, so registration must
    // key off the same collapsed pointer.
    let canonical = unsafe { crate::types::type_::canonical_type_object(native) };
    let mut registry = REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
    registry.native_by_foreign.insert(foreign as usize, canonical as usize);
    registry.foreign_by_native.insert(canonical as usize, foreign as usize);
    if canonical != native {
        registry.foreign_by_native.insert(native as usize, foreign as usize);
    }
}

/// Translates a foreign type pointer back to the runtime-native type.
pub(crate) fn native_of_foreign(foreign: *mut ForeignTypeObject) -> Option<*mut PyType> {
    if foreign.is_null() {
        return None;
    }
    {
        let registry = REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(&native) = registry.native_by_foreign.get(&(foreign as usize)) {
            return Some(native as *mut PyType);
        }
    }
    // Unregistered: trust a filled twin's own back pointer.
    // SAFETY: caller hands a live foreign type object.
    let twin = unsafe { (*foreign).tp_pon_twin };
    (!twin.is_null()).then_some(twin)
}

/// Registry-only foreign -> native translation for generic `PyObject *`
/// boundaries. Unlike [`native_of_foreign`], this never trusts/dereferences the
/// candidate as a `ForeignTypeObject`; callers can probe arbitrary object
/// pointers safely before deciding whether to treat them as runtime objects.
pub(crate) fn registered_native_of_foreign(foreign: *mut ForeignTypeObject) -> Option<*mut PyType> {
    if foreign.is_null() {
        return None;
    }
    let registry = REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
    registry
        .native_by_foreign
        .get(&(foreign as usize))
        .map(|&native| native as *mut PyType)
}

/// Registry-only native -> foreign translation (no fabrication): the
/// registered face of a `PyType_Ready`'d C type or an already-published
/// twin. Used by slot trampolines and the instance finalizer, which must
/// never fabricate mid-callback.
pub(crate) fn registered_foreign_of_native(native: *mut PyType) -> Option<*mut ForeignTypeObject> {
    if native.is_null() {
        return None;
    }
    // SAFETY: live type object; lookups key off the canonical pointer.
    let canonical = unsafe { crate::types::type_::canonical_type_object(native) };
    lookup_foreign(canonical)
}

/// Total native -> foreign translation; fabricates a runtime-owned twin when
/// no explicitly registered face exists. Readers only ever observe
/// fully-filled twins: the batch below publishes atomically.
pub(crate) fn foreign_of_native(native: *mut PyType) -> *mut ForeignTypeObject {
    if native.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: live type object; collapses helper-family shadows onto the
    // installed builtin global so identity never splits.
    let native = unsafe { crate::types::type_::canonical_type_object(native) };
    if let Some(foreign) = lookup_foreign(native) {
        return foreign;
    }
    let _fabrication = FABRICATION.lock().unwrap_or_else(|poison| poison.into_inner());
    // A concurrent batch may have published while this thread queued.
    if let Some(foreign) = lookup_foreign(native) {
        return foreign;
    }
    let mut batch: HashMap<usize, *mut ForeignTypeObject> = HashMap::new();
    let twin = fabricate_into_batch(native, &mut batch);
    publish_batch(&batch);
    twin
}

/// Publishes a completed fabrication batch; every twin in it is fully
/// filled. A concurrently registered EXPLICIT face keeps the canonical slot;
/// the batch twin then only serves reverse translation.
fn publish_batch(batch: &HashMap<usize, *mut ForeignTypeObject>) {
    let mut registry = REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
    for (&native, &twin) in batch {
        registry.native_by_foreign.insert(twin as usize, native);
        if !registry.foreign_by_native.contains_key(&native) {
            registry.foreign_by_native.insert(native, twin as usize);
            registry.fabricated.insert(twin as usize);
        }
    }
}

/// Allocates and fills a twin inside the thread-private batch. Cycles
/// (`type`/`object` meta/base) resolve against the batch, so recursion
/// terminates without exposing partially-filled state.
fn fabricate_into_batch(native: *mut PyType, batch: &mut HashMap<usize, *mut ForeignTypeObject>) -> *mut ForeignTypeObject {
    if let Some(&twin) = batch.get(&(native as usize)) {
        return twin;
    }
    if let Some(foreign) = lookup_foreign(native) {
        return foreign;
    }
    let twin = Box::into_raw(Box::new(zeroed_twin()));
    batch.insert(native as usize, twin);
    fill_twin(twin, native, None, batch);
    twin
}

/// Resolves a dependency type (metatype/base) to its foreign face within the
/// current batch.
fn resolve_in_batch(native: *mut PyType, batch: &mut HashMap<usize, *mut ForeignTypeObject>) -> *mut ForeignTypeObject {
    if native.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: live type object (see `foreign_of_native`).
    let native = unsafe { crate::types::type_::canonical_type_object(native) };
    fabricate_into_batch(native, batch)
}

/// Fills `twin` from its native type: identity fields, flags, metatype, and
/// base chain.
fn fill_twin(twin: *mut ForeignTypeObject, native: *mut PyType, tid: Option<usize>, batch: &mut HashMap<usize, *mut ForeignTypeObject>) {
    let type_type = abi::runtime_type_type();
    // SAFETY: `native` is a live type object; `twin` is exclusively owned by
    // this batch (fabrication) or bootstrap static storage being initialized
    // before publication.
    unsafe {
        let name = (*native).name().to_owned();
        (*twin).tp_name = CString::new(name).map_or(ptr::null(), |text| text.into_raw().cast_const());
        (*twin).tp_basicsize = (*native).tp_basicsize as isize;
        (*twin).tp_itemsize = (*native).tp_itemsize;
        (*twin).tp_flags = twin_flags(native, tid, type_type);
        (*twin).tp_pon_twin = native;
        let meta = (*native).ob_base.ob_type.cast_mut();
        let meta = if meta.is_null() { type_type } else { meta };
        (*twin).ob_type = resolve_in_batch(meta, batch);
        let base = (*native).tp_base;
        (*twin).tp_base = resolve_in_batch(base, batch);
    }
}

fn twin_flags(native: *mut PyType, tid: Option<usize>, type_type: *mut PyType) -> u64 {
    let mut flags = match tid {
        Some(TID_TYPE) => TPFLAGS_TYPE_SUBCLASS,
        Some(TID_LONG | TID_BOOL) => TPFLAGS_LONG_SUBCLASS,
        Some(TID_LIST) => TPFLAGS_LIST_SUBCLASS,
        Some(TID_TUPLE) => TPFLAGS_TUPLE_SUBCLASS,
        Some(TID_BYTES) => TPFLAGS_BYTES_SUBCLASS,
        Some(TID_UNICODE) => TPFLAGS_UNICODE_SUBCLASS,
        Some(TID_DICT) => TPFLAGS_DICT_SUBCLASS,
        None if native == type_type => TPFLAGS_TYPE_SUBCLASS,
        _ => 0,
    };
    let unsubclassable = matches!(tid, Some(TID_BOOL | TID_NONE_TYPE | TID_SLICE | TID_MEMORYVIEW | TID_CAPSULE));
    if !unsubclassable {
        flags |= TPFLAGS_BASETYPE;
    }
    if crate::abi::exc::type_derives_base_exception(native.cast_const()) {
        flags |= TPFLAGS_BASE_EXC_SUBCLASS;
    }
    flags
}

fn zeroed_twin() -> ForeignTypeObject {
    // SAFETY: ForeignTypeObject is a POD of pointers/integers; the all-zero
    // pattern is valid (all NULL/0).
    unsafe { core::mem::zeroed() }
}

/// `PyPonCapiCore.register_local_twins`: called once per extension from
/// `PyPon_SetCapi` with the bootstrap's local twin globals.
///
/// The whole operation runs as one fabrication batch: EVERY local twin is
/// filled (an extension reads its own statics' fields regardless of who owns
/// the canonical face), wired against this bootstrap's own locals, and then
/// published with the win-rules: vacant slot -> local wins; fabricated face
/// -> local displaces it; explicit face -> first registration stays.
pub(crate) unsafe extern "C" fn capi_register_local_twins(twins: *const *mut ForeignTypeObject, count: c_int) -> c_int {
    if twins.is_null() || count < 0 {
        return -1;
    }
    let Some(keys) = builtin_keys() else {
        return -1;
    };
    let count = (count as usize).min(BUILTIN_TYPE_COUNT);
    // SAFETY: bootstrap passes an array of `count` twin pointers.
    let twins = unsafe { core::slice::from_raw_parts(twins, count) };
    let _fabrication = FABRICATION.lock().unwrap_or_else(|poison| poison.into_inner());
    // Locals shadow any global face for this batch's internal wiring, so an
    // extension's statics reference each other (its `PyLong_Type.tp_base`
    // points at its `PyBaseObject_Type`).
    let mut batch: HashMap<usize, *mut ForeignTypeObject> = HashMap::new();
    let mut locals: HashMap<usize, usize> = HashMap::new();
    for key in keys {
        let Some(&twin) = twins.get(key.tid) else {
            continue;
        };
        if twin.is_null() {
            continue;
        }
        batch.insert(key.canonical, twin);
        locals.insert(twin as usize, key.canonical);
    }
    for (&native, &twin) in &batch.clone() {
        let tid = keys.iter().find(|key| key.canonical == native).map(|key| key.tid);
        fill_twin(twin, native as *mut PyType, tid, &mut batch);
    }
    let mut registry = REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
    for (&native, &twin) in &batch {
        let twin_key = twin as usize;
        registry.native_by_foreign.insert(twin_key, native);
        let is_local = locals.contains_key(&twin_key);
        let wins = match registry.foreign_by_native.get(&native) {
            None => true,
            Some(existing) => is_local && registry.fabricated.contains(existing),
        };
        if wins {
            registry.foreign_by_native.insert(native, twin_key);
            if !is_local {
                registry.fabricated.insert(twin_key);
            }
        }
        // A fabricated dependency that lost to an explicit face only needs
        // the reverse translation installed above.
    }
    // Helper-family alias pointers resolve to the same face as the canonical.
    for key in keys {
        let Some(&face) = registry.foreign_by_native.get(&key.canonical) else {
            continue;
        };
        for &alias in &key.natives {
            let wins = match registry.foreign_by_native.get(&alias) {
                None => true,
                Some(existing) => registry.fabricated.contains(existing) && !registry.fabricated.contains(&face),
            };
            if wins {
                registry.foreign_by_native.insert(alias, face);
            }
        }
    }
    0
}

/// `PyPonCapiCore.builtin_type_id`: PON_TID_* for the object's runtime type.
pub(crate) unsafe extern "C" fn capi_builtin_type_id(object: *mut PyObject) -> c_int {
    if object.is_null() {
        return -1;
    }
    if registered_native_of_foreign(object.cast::<ForeignTypeObject>()).is_some() {
        return -1;
    }
    if crate::tag::is_small_int(object) {
        return TID_LONG as c_int;
    }
    if !crate::tag::is_heap(object) {
        return -1;
    }
    let Some(keys) = builtin_keys() else {
        return -1;
    };
    if unsafe { crate::types::type_::is_class_dict_view(object) } {
        return TID_DICT as c_int;
    }
    // SAFETY: heap-tagged live object.
    let ty = unsafe { (*object).ob_type } as usize;
    for key in keys {
        if key.natives.iter().any(|&native| native == ty) {
            return key.tid as c_int;
        }
    }
    -1
}

/// `PyPonCapiCore.foreign_of`: canonical foreign face for the object's type.
pub(crate) unsafe extern "C" fn capi_foreign_of(object: *mut PyObject) -> *mut ForeignTypeObject {
    if object.is_null() {
        return ptr::null_mut();
    }
    let foreign_type_object = object.cast::<ForeignTypeObject>();
    if registered_native_of_foreign(foreign_type_object).is_some() {
        // `Py_TYPE((PyObject *)SomeReadyPyTypeObject)` reads the extension's
        // own type header.  Do not reinterpret that foreign `ob_type` as a
        // native `PyType *`; for custom metatypes it is another foreign static.
        // SAFETY: registry membership proves this pointer is a ready foreign
        // `PyTypeObject` prefix.
        let meta = unsafe { (*foreign_type_object).ob_type };
        return if meta.is_null() { foreign_of_native(abi::runtime_type_type()) } else { meta };
    }
    let native = if crate::tag::is_small_int(object) {
        abi::runtime_long_type()
    } else if crate::tag::is_heap(object) {
        // SAFETY: heap-tagged live object.
        unsafe { (*object).ob_type }.cast_mut()
    } else {
        return ptr::null_mut();
    };
    foreign_of_native(native)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::pon_runtime_init;
    use crate::thread_state::test_state_lock;

    fn init_runtime() {
        // SAFETY: test-only init; serialized by the state lock in callers.
        let status = unsafe { pon_runtime_init() };
        assert_eq!(status, 0, "runtime init failed");
    }

    #[test]
    fn foreign_of_native_is_stable_total_and_filled() {
        let _guard = test_state_lock();
        init_runtime();
        let int_native = abi::runtime_long_type();
        assert!(!int_native.is_null());
        let first = foreign_of_native(int_native);
        let second = foreign_of_native(int_native);
        assert_eq!(first, second, "twin identity must be stable");
        assert_eq!(native_of_foreign(first), Some(int_native));
        // SAFETY: twin fabricated above; fields must be filled on return.
        unsafe {
            assert!(!(*first).tp_name.is_null(), "published twins are fully filled");
            assert!(!(*first).ob_type.is_null(), "metatype twin wired");
            assert_eq!((*first).tp_flags & TPFLAGS_BASE_EXC_SUBCLASS, 0);
        }
        let runtime_error = abi::exception_type_object(crate::types::exc::ExceptionKind::RuntimeError);
        let exc_twin = foreign_of_native(runtime_error);
        // SAFETY: twin fabricated above.
        unsafe {
            assert_ne!((*exc_twin).tp_flags & TPFLAGS_BASE_EXC_SUBCLASS, 0, "exception twins carry the exc flag");
            assert!(!(*exc_twin).tp_base.is_null(), "exception twins mirror the native base chain");
        }
    }

    #[test]
    fn explicit_registration_wins_over_fabrication() {
        let _guard = test_state_lock();
        init_runtime();
        let float_native = crate::types::float::float_type();
        let fabricated = foreign_of_native(float_native);
        let static_twin = Box::into_raw(Box::new(zeroed_twin()));
        register_foreign_twin(static_twin, float_native);
        assert_eq!(foreign_of_native(float_native), static_twin, "explicit registration replaces fabrication");
        assert_eq!(native_of_foreign(static_twin), Some(float_native));
        assert_eq!(native_of_foreign(fabricated), Some(float_native), "old twin still translates back");
    }
}
