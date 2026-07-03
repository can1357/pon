//! Weak reference support and heap-instance finalization hooks.

use core::ffi::c_int;
use core::ptr;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use pon_gc::TypeId;

use crate::abstract_op::{RICH_EQ, RICH_NE};
use crate::descr;
use crate::intern;
use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::thread_state::{pon_err_clear, pon_err_occurred, pon_err_set, pon_err_message};
use crate::types::type_::{self, PyHeapInstance};

/// GC type id for weakref.ref objects once the ref object itself moves into the heap.
pub const TYPE_ID_WEAKREF: TypeId = TypeId(11);

#[repr(C)]
#[derive(Debug)]
pub struct PyWeakRef {
    pub ob_base: PyObjectHeader,
    referent: *mut PyObject,
    callback: *mut PyObject,
    hash: isize,
    hash_valid: bool,
    builtin_hash: i64,
    builtin_hash_valid: bool,
    /// Subclass wrapper owning this canonical ref (`PyPayloadSubclassInstance`
    /// whose payload is this object), or NULL for a plain `weakref.ref`.  The
    /// death callback receives the wrapper — CPython invokes callbacks with
    /// the subclass instance, and importlib's `KeyedRef.remove(wr)` reads
    /// `wr.key` off it.
    wrapper: *mut PyObject,
}

static WEAKREFS: LazyLock<Mutex<HashMap<usize, Vec<usize>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

static WEAKREF_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(crate::abi::runtime_type_type(), "ReferenceType", core::mem::size_of::<PyWeakRef>());
    ty.tp_new = Some(weakref_new);
    ty.tp_call = Some(weakref_call);
    ty.tp_hash = Some(weakref_hash);
    ty.tp_richcmp = Some(weakref_richcmp);
    ty.tp_getattro = Some(weakref_getattro);
    // A real `object` base completes subclass MROs (`class KeyedRef(ref)`),
    // so `super().__init__` and cooperative dunders resolve past this type —
    // the WeakKeyDictionary type follows the same pattern.
    ty.tp_base = crate::abi::runtime_global(intern::intern("object"))
        .map_or(ptr::null_mut(), |object| object.cast::<PyType>());
    Box::into_raw(Box::new(ty)) as usize
});

fn weakref_type() -> *mut PyType {
    *WEAKREF_TYPE as *mut PyType
}

#[must_use]
pub fn weakref_ref_type() -> *mut PyObject {
    weakref_type().cast::<PyObject>()
}

/// True when `object` is exactly a `weakref.ref` object (not a subclass).
///
/// Reads the type slot WITHOUT forcing initialization: the initializer takes
/// the runtime lock (via `runtime_type_type`), and hash/eq callers such as
/// `pon_build_map` already hold it — forcing here deadlocks. An uninitialized
/// slot also means no weakref can exist yet (`weakref_new` is the only
/// constructor and it forces the type first), so `false` is exact, not lossy.
#[must_use]
pub unsafe fn is_weakref(object: *mut PyObject) -> bool {
    let Some(&ty) = LazyLock::get(&WEAKREF_TYPE) else {
        return false;
    };
    !object.is_null() && unsafe { (*object).ob_type }.cast::<PyObject>() == (ty as *mut PyObject)
}

/// Referent of a `weakref.ref` object; null once the referent was cleared.
/// Callers must have established `is_weakref(object)`.
#[must_use]
pub unsafe fn weakref_target(object: *mut PyObject) -> *mut PyObject {
    unsafe { (*object.cast::<PyWeakRef>()).referent }
}

fn registry() -> std::sync::MutexGuard<'static, HashMap<usize, Vec<usize>>> {
    WEAKREFS.lock().unwrap_or_else(|poison| poison.into_inner())
}

unsafe fn object_type_name(object: *mut PyObject) -> Option<&'static str> {
    if object.is_null() || unsafe { (*object).ob_type.is_null() } {
        return None;
    }
    Some(unsafe { core::mem::transmute::<&str, &'static str>((*(*object).ob_type).name()) })
}

unsafe fn is_none(object: *mut PyObject) -> bool {
    unsafe { object_type_name(object) == Some("NoneType") }
}

/// Class objects are immortal in this runtime (leaked boxes), so a weak
/// reference to one is legal and simply never clears.
unsafe fn is_type_referent(object: *mut PyObject) -> bool {
    if object.is_null() {
        return false;
    }
    let ty = unsafe { (*object).ob_type.cast_mut() };
    !ty.is_null() && unsafe { crate::mro::is_subtype(ty, crate::abi::runtime_type_type()) }
}

unsafe fn is_weakrefable(object: *mut PyObject) -> bool {
    if object.is_null() {
        return false;
    }
    if unsafe { is_type_referent(object) } {
        return true;
    }
    let ty = unsafe { (*object).ob_type };
    if ty.is_null() {
        return false;
    }
    if unsafe { (*ty).gc_type_id == type_::TYPE_ID_HEAP_INSTANCE.0 as usize } {
        return true;
    }
    matches!(unsafe { (*ty).name() }, "function")
}

fn register_weakref(referent: *mut PyObject, weakref: *mut PyObject) {
    registry().entry(referent as usize).or_default().push(weakref as usize);
    unsafe {
        // A class referent is a PyType, never a PyHeapInstance: writing the
        // instance weakref-list field would scribble over type slots.
        if is_type_referent(referent) {
            return;
        }
        let ty = (*referent).ob_type;
        if !ty.is_null() && (*ty).gc_type_id == type_::TYPE_ID_HEAP_INSTANCE.0 as usize {
            (*referent.cast::<PyHeapInstance>()).weakrefs = weakref;
        }
    }
}

fn unregister_weakref(referent: *mut PyObject, weakref: *mut PyObject) {
    if referent.is_null() {
        return;
    }
    let mut registry = registry();
    if let Some(list) = registry.get_mut(&(referent as usize)) {
        list.retain(|entry| *entry != weakref as usize);
        if list.is_empty() {
            registry.remove(&(referent as usize));
        }
    }
}

/// Validates `(referent[, callback])` positionals and allocates a canonical
/// registered `PyWeakRef` typed as `ty`.  Shared by the `tp_new` slot and the
/// Python-visible `__new__` staticmethod carrier.
unsafe fn alloc_weakref(ty: *mut PyType, positional: &[*mut PyObject]) -> *mut PyObject {
    if !(positional.len() == 1 || positional.len() == 2) {
        let message = "weakref.ref expected object and optional callback";
        return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    }
    let referent = positional[0];
    if unsafe { !is_weakrefable(referent) } {
        let name = unsafe { object_type_name(referent) }.unwrap_or("object");
        let message = format!("cannot create weak reference to '{name}' object");
        return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    }
    let callback = positional.get(1).copied().unwrap_or(ptr::null_mut());
    let callback = if callback.is_null() || unsafe { is_none(callback) } { ptr::null_mut() } else { callback };
    let object = Box::into_raw(Box::new(PyWeakRef {
        ob_base: PyObjectHeader::new(ty),
        referent,
        callback,
        hash: -1,
        hash_valid: false,
        builtin_hash: -1,
        builtin_hash_valid: false,
        wrapper: ptr::null_mut(),
    }))
    .cast::<PyObject>();
    register_weakref(referent, object);
    object
}

unsafe extern "C" fn weakref_new(cls: *mut PyType, args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
    let positional = match unsafe { type_::positional_args_from_object(args) } {
        Ok(args) => args,
        Err(message) => {
            pon_err_set(message);
            return ptr::null_mut();
        }
    };
    let ty = if cls.is_null() { weakref_type() } else { cls };
    unsafe { alloc_weakref(ty, &positional) }
}

unsafe extern "C" fn weakref_call(object: *mut PyObject, _args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
    if object.is_null() {
        pon_err_set("weakref receiver is NULL");
        return ptr::null_mut();
    }
    let referent = unsafe { (*object.cast::<PyWeakRef>()).referent };
    if referent.is_null() {
        unsafe { crate::abi::pon_none() }
    } else {
        referent
    }
}

unsafe extern "C" fn weakref_hash(object: *mut PyObject) -> isize {
    if object.is_null() {
        pon_err_set("weakref hash receiver is NULL");
        return -1;
    }
    match unsafe { weakref_container_hash(object) } {
        Ok(hash) => hash,
        Err(message) => {
            pon_err_set(message);
            -1
        }
    }
}

/// Container-universe hash of a weakref (the dict/set key domain): the live
/// referent's `hash_object` value, cached so it survives referent death the
/// way CPython's `wr_hash` does (WeakSet discards dead refs by cached hash).
pub unsafe fn weakref_container_hash(object: *mut PyObject) -> Result<isize, String> {
    let weakref = unsafe { &mut *object.cast::<PyWeakRef>() };
    if weakref.hash_valid {
        return Ok(weakref.hash);
    }
    if weakref.referent.is_null() {
        return Err("weak object has gone away".to_owned());
    }
    let hash = unsafe { crate::types::dict::hash_object(weakref.referent)? };
    weakref.hash = hash;
    weakref.hash_valid = true;
    Ok(hash)
}

/// Cached `hash()`-builtin value, if one was computed while the referent
/// lived. Kept separate from the container hash: the two hash domains
/// disagree for some referents (e.g. class objects), and sharing one cache
/// would leak values across domains.
#[must_use]
pub unsafe fn weakref_cached_builtin_hash(object: *mut PyObject) -> Option<i64> {
    let weakref = unsafe { &*object.cast::<PyWeakRef>() };
    weakref.builtin_hash_valid.then_some(weakref.builtin_hash)
}

/// Records the `hash()`-builtin value for a weakref while its referent lives.
pub unsafe fn weakref_store_builtin_hash(object: *mut PyObject, hash: i64) {
    let weakref = unsafe { &mut *object.cast::<PyWeakRef>() };
    weakref.builtin_hash = hash;
    weakref.builtin_hash_valid = true;
}

unsafe extern "C" fn weakref_richcmp(left: *mut PyObject, right: *mut PyObject, op: c_int) -> *mut PyObject {
    if op != i32::from(RICH_EQ) && op != i32::from(RICH_NE) {
        pon_err_set("weakref only supports equality comparison");
        return ptr::null_mut();
    }
    let mut equal = left == right;
    if !left.is_null() && !right.is_null() && unsafe { object_type_name(right) == Some("ReferenceType") } {
        let left_ref = unsafe { &*left.cast::<PyWeakRef>() };
        let right_ref = unsafe { &*right.cast::<PyWeakRef>() };
        equal = if !left_ref.referent.is_null() && !right_ref.referent.is_null() {
            unsafe { crate::types::dict::object_equal(left_ref.referent, right_ref.referent).unwrap_or(false) }
        } else {
            left == right
        };
    }
    if op == i32::from(RICH_NE) {
        equal = !equal;
    }
    unsafe { crate::abi::number::pon_const_bool(i32::from(equal)) }
}

unsafe extern "C" fn weakref_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        pon_err_set("weakref attribute name must be str");
        return ptr::null_mut();
    };
    match name {
        "__callback__" => {
            let callback = unsafe { (*object.cast::<PyWeakRef>()).callback };
            if callback.is_null() { unsafe { crate::abi::pon_none() } } else { callback }
        }
        _ => unsafe { crate::abi::pon_raise_attribute_error(object, intern::intern(name)) },
    }
}

// ---------------------------------------------------------------------------
// Subclass surface (`class KeyedRef(weakref.ref)`, importlib's module-lock
// bookkeeping): instances use the payload-subclass layout with a canonical
// registered `PyWeakRef` payload, and the type dict carries the `__new__`/
// `__call__` entries MRO dispatch (including `super()`) resolves.

/// Returns whether some base linearizes over the builtin `weakref.ref` type,
/// so class construction can install the subclass surface first (the
/// `class_bases_embed_dict` pattern).
#[must_use]
pub unsafe fn class_bases_embed_weakref(bases: &[*mut PyType]) -> bool {
    let ty = weakref_type();
    bases.iter().copied().any(|base| unsafe { crate::mro::mro_entries(base) }.contains(&ty))
}

/// Canonical `PyWeakRef` payload of a weakref-subclass wrapper instance, when
/// `object` is one.
unsafe fn wrapper_payload(object: *mut PyObject) -> Option<*mut PyWeakRef> {
    let value = unsafe { type_::payload_subclass_value(object)? };
    unsafe { is_weakref_layout(value) }.then(|| value.cast::<PyWeakRef>())
}

/// True when `object` is a `PyWeakRef`-layout allocation: exactly the builtin
/// ref/proxy types (subclass wrappers use the payload-subclass layout).
unsafe fn is_weakref_layout(object: *mut PyObject) -> bool {
    if object.is_null() || !crate::tag::is_heap(object) {
        return false;
    }
    let ty = unsafe { (*object).ob_type };
    ty == weakref_type().cast_const() || ty == (*WEAKPROXY_TYPE as *mut PyType).cast_const()
}
/// Referent of any weakref-shaped object — a plain `ref`/`proxy` or a
/// payload-subclass wrapper (`KeyedRef`, `WeakMethod`).  `None` when `object`
/// is not a weak reference; `Some(NULL)` when the referent already died.
#[must_use]
pub(crate) unsafe fn weakref_referent_any(object: *mut PyObject) -> Option<*mut PyObject> {
    unsafe {
        if is_weakref_layout(object) {
            return Some((*object.cast::<PyWeakRef>()).referent);
        }
        wrapper_payload(object).map(|payload| (*payload).referent)
    }
}

/// Python-visible weak references registered against `referent` (subclass
/// wrapper instances stand in for their canonical payloads — the object user
/// code constructed, mirroring CPython's `tp_weaklist` contents).
#[must_use]
pub(crate) fn weakrefs_of(referent: *mut PyObject) -> Vec<*mut PyObject> {
    let addrs = registry().get(&(referent as usize)).cloned().unwrap_or_default();
    addrs
        .into_iter()
        .map(|addr| {
            let weakref = addr as *mut PyWeakRef;
            // SAFETY: registry entries are live registered refs; wrapper is
            // NULL or the owning payload-subclass instance.
            let wrapper = unsafe { (*weakref).wrapper };
            if wrapper.is_null() { weakref.cast::<PyObject>() } else { wrapper }
        })
        .collect()
}

/// Detaches the canonical payload of a dying wrapper instance: unregisters it
/// so referent death can never call back into freed wrapper memory.
pub(crate) unsafe fn detach_wrapper_payload(object: *mut PyObject) {
    let Some(payload) = (unsafe { wrapper_payload(object) }) else {
        return;
    };
    let payload = unsafe { &mut *payload };
    unregister_weakref(payload.referent, (payload as *mut PyWeakRef).cast::<PyObject>());
    payload.referent = ptr::null_mut();
    payload.callback = ptr::null_mut();
    payload.wrapper = ptr::null_mut();
}

/// `ref.__new__(cls, referent[, callback])` — the staticmethod carrier
/// terminus for `super().__new__(cls, ob, cb)` in weakref subclasses
/// (importlib bootstrap's `KeyedRef`).  The builtin class returns a canonical
/// ref; a payload-subclass heap class returns a wrapper instance embedding
/// the canonical, registered ref.
unsafe extern "C" fn weakref_dunder_new(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() || argc == 0 {
        let message = "ref.__new__(): not enough arguments";
        return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    }
    let args = unsafe { core::slice::from_raw_parts(argv, argc) };
    let cls = args[0];
    if unsafe { !type_::is_type_object(cls) } {
        let message = "ref.__new__(X): X is not a type object";
        return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    }
    let cls_ty = cls.cast::<PyType>();
    if unsafe { !crate::mro::is_subtype(cls_ty, weakref_type()) } {
        let cls_name = unsafe { (*cls_ty).name() };
        let message = format!("ref.__new__({cls_name}): {cls_name} is not a subtype of ref");
        return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    }
    if cls_ty == weakref_type() {
        return unsafe { alloc_weakref(cls_ty, &args[1..]) };
    }
    if unsafe { !type_::type_is_payload_subclass(cls_ty) } {
        let cls_name = unsafe { (*cls_ty).name() };
        let message = format!("ref.__new__({cls_name}): {cls_name} does not embed a ref payload");
        return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    }
    let canonical = unsafe { alloc_weakref(weakref_type(), &args[1..]) };
    if canonical.is_null() {
        return ptr::null_mut();
    }
    match unsafe { type_::alloc_payload_instance_for_class(cls_ty, canonical) } {
        Ok(wrapper) => {
            // The callback receiver: referent death reports the wrapper the
            // user constructed, not the embedded canonical ref.
            unsafe { (*canonical.cast::<PyWeakRef>()).wrapper = wrapper };
            wrapper
        }
        Err(message) => {
            pon_err_set(message);
            ptr::null_mut()
        }
    }
}

/// `ref.__call__(self)` — dereference for subclass wrapper instances (plain
/// refs stay on the `tp_call` slot).
unsafe extern "C" fn weakref_dunder_call(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() || argc == 0 {
        let message = "ref.__call__(): not enough arguments";
        return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    }
    if argc != 1 {
        let message = format!("ref.__call__ expected no arguments, got {}", argc - 1);
        return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    }
    let receiver = unsafe { *argv };
    let payload = if unsafe { is_weakref_layout(receiver) } {
        receiver.cast::<PyWeakRef>()
    } else if let Some(payload) = unsafe { wrapper_payload(receiver) } {
        payload
    } else {
        let message = "ref.__call__ expected a weakref receiver";
        return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    };
    let referent = unsafe { (*payload).referent };
    if referent.is_null() { unsafe { crate::abi::pon_none() } } else { referent }
}

/// One-shot installer for the builtin ref type's `tp_dict` surface: the
/// `__new__` staticmethod carrier plus `__call__`, resolved through subclass
/// MROs (the `ensure_tuple_type_methods_installed` pattern).  Idempotent;
/// called from class construction when a class linearizes over `ref`.
pub(crate) fn ensure_weakref_subclass_surface() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.load(Ordering::SeqCst) {
        return;
    }
    // Pre-runtime call sites must not latch a no-op install.
    if crate::abi::runtime_type_type().is_null() {
        return;
    }
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let ty = weakref_type();
    let namespace = unsafe { (*ty).tp_dict.cast::<type_::PyClassDict>() };
    let namespace = if namespace.is_null() { type_::new_namespace() } else { namespace };
    let new_name = intern::intern("__new__");
    if unsafe { (&*namespace).get(new_name) }.is_none() {
        // A staticmethod carrier keeps `super().__new__` and `cls.__new__`
        // lookups from binding the receiver (CPython: `__new__` is
        // implicitly static).
        let function = unsafe {
            crate::abi::pon_make_function(weakref_dunder_new as *const u8, crate::builtins::variadic_arity(), new_name)
        };
        if !function.is_null() {
            let descriptor = unsafe {
                crate::types::classmethod::new_staticmethod(crate::abi::staticmethod_builtin_type(), function)
            };
            if !descriptor.is_null() {
                unsafe { (&mut *namespace).set(new_name, descriptor.cast::<PyObject>()) };
            }
        }
    }
    let call_name = intern::intern("__call__");
    if unsafe { (&*namespace).get(call_name) }.is_none() {
        let function = unsafe {
            crate::abi::pon_make_function(weakref_dunder_call as *const u8, crate::builtins::variadic_arity(), call_name)
        };
        if !function.is_null() {
            unsafe { (&mut *namespace).set(call_name, function) };
        }
    }
    unsafe {
        (*ty).tp_dict = namespace.cast::<PyObject>();
    }
    // GC rooting for the namespace values plus IC invalidation for any AttrIC
    // guarding the type object.
    crate::sync::register_namespaced_type(ty);
    crate::sync::type_modified(ty);
}

/// `weakref.proxy` type: a transparent forwarder to a weakly-held referent.
///
/// Proxies share `PyWeakRef`'s layout (same registry, clearing, and callback
/// path as `weakref.ref`; `weakref_new` honours the called type), and differ
/// only in slots: attribute get/set forward to the live referent, and a dead
/// referent raises `ReferenceError` the way CPython proxies do. Hash/call/
/// richcmp intentionally stay unset — `collections.OrderedDict` (the driving
/// consumer) only reads and writes link attributes through the proxy.
static WEAKPROXY_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(crate::abi::runtime_type_type(), "weakproxy", core::mem::size_of::<PyWeakRef>());
    ty.tp_new = Some(weakref_new);
    ty.tp_getattro = Some(proxy_getattro);
    ty.tp_setattro = Some(proxy_setattro);
    Box::into_raw(Box::new(ty)) as usize
});

#[must_use]
pub fn weakref_proxy_type() -> *mut PyObject {
    (*WEAKPROXY_TYPE as *mut PyType).cast::<PyObject>()
}

/// Live referent of a proxy, or a raised `ReferenceError` for a dead one.
unsafe fn proxy_live_referent(object: *mut PyObject) -> Result<*mut PyObject, *mut PyObject> {
    if object.is_null() {
        let message = "proxy receiver is NULL";
        return Err(unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) });
    }
    let referent = unsafe { (*object.cast::<PyWeakRef>()).referent };
    if referent.is_null() {
        let message = "weakly-referenced object no longer exists";
        return Err(unsafe { crate::abi::exc::pon_raise_reference_error(message.as_ptr(), message.len()) });
    }
    Ok(referent)
}

unsafe extern "C" fn proxy_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let referent = match unsafe { proxy_live_referent(object) } {
        Ok(referent) => referent,
        Err(raised) => return raised,
    };
    let ty = unsafe { (*referent).ob_type };
    let Some(slot) = (unsafe { ty.as_ref().and_then(|ty| ty.tp_getattro) }) else {
        let message = "proxied object does not support attribute lookup";
        return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    };
    unsafe { slot(referent, name) }
}

unsafe extern "C" fn proxy_setattro(object: *mut PyObject, name: *mut PyObject, value: *mut PyObject) -> c_int {
    let referent = match unsafe { proxy_live_referent(object) } {
        Ok(referent) => referent,
        Err(_) => return -1,
    };
    let ty = unsafe { (*referent).ob_type };
    let Some(slot) = (unsafe { ty.as_ref().and_then(|ty| ty.tp_setattro) }) else {
        let message = "proxied object does not support attribute assignment";
        unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
        return -1;
    };
    unsafe { slot(referent, name, value) }
}

pub unsafe extern "C" fn trace_weakref(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    let weakref = unsafe { &*object.cast::<PyWeakRef>() };
    if !weakref.callback.is_null() {
        visitor(weakref.callback.cast::<u8>());
    }
}

pub unsafe extern "C" fn finalize_weakref(object: *mut u8) {
    if object.is_null() {
        return;
    }
    let weakref = unsafe { &mut *object.cast::<PyWeakRef>() };
    unregister_weakref(weakref.referent, object.cast::<PyObject>());
    weakref.referent = ptr::null_mut();
    weakref.callback = ptr::null_mut();
}

pub fn clear_weakrefs(referent: *mut PyObject) {
    let weakrefs = registry().remove(&(referent as usize)).unwrap_or_default();
    for weakref_addr in weakrefs {
        let weakref = weakref_addr as *mut PyWeakRef;
        if weakref.is_null() {
            continue;
        }
        let (callback, receiver) = unsafe {
            let weakref_ref = &mut *weakref;
            weakref_ref.referent = ptr::null_mut();
            // Subclass refs deliver the wrapper instance to the callback
            // (CPython passes the weakref object the user constructed).
            let receiver =
                if weakref_ref.wrapper.is_null() { weakref.cast::<PyObject>() } else { weakref_ref.wrapper };
            (weakref_ref.callback, receiver)
        };
        if !callback.is_null() {
            let mut argv = [receiver];
            let result = unsafe { crate::abi::pon_call(callback, argv.as_mut_ptr(), 1) };
            if result.is_null() && pon_err_occurred() {
                if let Some(message) = pon_err_message() {
                    eprintln!("Exception ignored in weakref callback: {message}");
                }
                pon_err_clear();
            }
        }
    }
    unsafe {
        let ty = if referent.is_null() { ptr::null() } else { (*referent).ob_type };
        if !ty.is_null() && (*ty).gc_type_id == type_::TYPE_ID_HEAP_INSTANCE.0 as usize {
            (*referent.cast::<PyHeapInstance>()).weakrefs = ptr::null_mut();
        }
    }
}

/// Traces GC-owned references inside a heap instance.
pub unsafe extern "C" fn trace_heap_instance(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    let instance = unsafe { &*object.cast::<PyHeapInstance>() };
    if !instance.dict.is_null() {
        for (_, value) in unsafe { (&*instance.dict).iter() } {
            if !value.is_null() {
                visitor(value.cast::<u8>());
            }
        }
    }
    for slot in &instance.slots {
        if !slot.value.is_null() {
            visitor(slot.value.cast::<u8>());
        }
    }
}

/// Finalizes a heap instance: weakrefs, `__del__`, and Rust-owned side storage.
pub unsafe extern "C" fn finalize_heap_instance(object: *mut u8) {
    if object.is_null() {
        return;
    }
    let instance = unsafe { &mut *object.cast::<PyHeapInstance>() };
    if !instance.finalized {
        instance.finalized = true;
        let object = object.cast::<PyObject>();
        let del = unsafe { descr::lookup_in_type((*object).ob_type.cast_mut(), intern::intern("__del__")) };
        if !del.is_null() {
            let bound = unsafe { descr::descriptor_get(del, object, (*object).ob_type.cast_mut()) };
            if !bound.is_null() {
                let result = unsafe { crate::abi::pon_call(bound, ptr::null_mut(), 0) };
                if result.is_null() && pon_err_occurred() {
                    if let Some(message) = pon_err_message() {
                        eprintln!("Exception ignored in __del__: {message}");
                    }
                    pon_err_clear();
                }
            } else if pon_err_occurred() {
                if let Some(message) = pon_err_message() {
                    eprintln!("Exception ignored in __del__ binding: {message}");
                }
                pon_err_clear();
            }
        }
        clear_weakrefs(object);
    }
    if !instance.dict.is_null() {
        unsafe { drop(Box::from_raw(instance.dict)) };
        instance.dict = ptr::null_mut();
    }
    unsafe { ptr::drop_in_place(&mut instance.slots) };
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::abi::{collect, pon_call, pon_make_function, pon_none, pon_runtime_init};
    use crate::thread_state::{pon_err_clear, test_state_lock};

    static DEL_CALLS: AtomicUsize = AtomicUsize::new(0);
    static WEAKREF_CALLBACKS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn del_marker(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
        assert_eq!(argc, 1, "__del__ should be called as a bound method");
        DEL_CALLS.fetch_add(1, Ordering::SeqCst);
        unsafe { pon_none() }
    }

    unsafe extern "C" fn weakref_callback(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
        assert_eq!(argc, 1, "weakref callback receives the ref object");
        assert!(!argv.is_null());
        let referent = unsafe { weakref_call(*argv, ptr::null_mut(), ptr::null_mut()) };
        assert_eq!(referent, unsafe { pon_none() });
        WEAKREF_CALLBACKS.fetch_add(1, Ordering::SeqCst);
        unsafe { pon_none() }
    }

    #[test]
    fn heap_instance_collection_runs_del_once_and_clears_weakrefs() {
        let _guard = test_state_lock();
        DEL_CALLS.store(0, Ordering::SeqCst);
        WEAKREF_CALLBACKS.store(0, Ordering::SeqCst);

        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            pon_err_clear();

            let namespace = type_::new_namespace();
            let del = pon_make_function(del_marker as *const u8, 1, intern::intern("__del__"));
            assert!(!del.is_null());
            (&mut *namespace).set(intern::intern("__del__"), del);
            let cls = type_::build_class_from_namespace("WeakFinalized", &[], namespace, &[]);
            assert!(!cls.is_null());

            let object = type_::type_new(cls.cast::<PyType>(), ptr::null_mut(), ptr::null_mut());
            assert!(!object.is_null());
            assert_eq!((*object.cast::<PyHeapInstance>()).weakrefs, ptr::null_mut());

            let callback = pon_make_function(weakref_callback as *const u8, 1, intern::intern("weakref_callback"));
            assert!(!callback.is_null());
            let mut args = [object, callback];
            let weakref = pon_call(weakref_ref_type(), args.as_mut_ptr(), args.len());
            assert!(!weakref.is_null());
            assert_eq!((*object.cast::<PyHeapInstance>()).weakrefs, weakref);
            assert_eq!(pon_call(weakref, ptr::null_mut(), 0), object);

            collect().expect("collection should complete");
            assert_eq!(DEL_CALLS.load(Ordering::SeqCst), 1);
            assert_eq!(WEAKREF_CALLBACKS.load(Ordering::SeqCst), 1);
            assert_eq!(pon_call(weakref, ptr::null_mut(), 0), pon_none());

            collect().expect("second collection should complete");
            assert_eq!(DEL_CALLS.load(Ordering::SeqCst), 1);
            assert_eq!(WEAKREF_CALLBACKS.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn proxy_forwards_attributes_and_dead_proxy_raises() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            pon_err_clear();

            let namespace = type_::new_namespace();
            let cls = type_::build_class_from_namespace("ProxyLink", &[], namespace, &[]);
            assert!(!cls.is_null());
            let object = type_::type_new(cls.cast::<PyType>(), ptr::null_mut(), ptr::null_mut());
            assert!(!object.is_null());

            let mut args = [object];
            let proxy = pon_call(weakref_proxy_type(), args.as_mut_ptr(), args.len());
            assert!(!proxy.is_null());

            let proxy_ty = (*proxy).ob_type;
            let getattro = (*proxy_ty).tp_getattro.expect("proxy type wires tp_getattro");
            let setattro = (*proxy_ty).tp_setattro.expect("proxy type wires tp_setattro");

            // Setting through the proxy lands on the referent; reading it back
            // through the proxy and directly off the referent agree.
            let name = crate::abi::pon_const_str("payload".as_ptr(), "payload".len());
            let value = crate::abi::pon_const_int(7);
            assert_eq!(setattro(proxy, name, value), 0);
            assert_eq!(getattro(proxy, name), value);
            let direct = (*(*object).ob_type).tp_getattro.expect("heap class wires tp_getattro");
            assert_eq!(direct(object, name), value);

            // Clearing the referent (instance death path) makes the proxy dead:
            // attribute access raises instead of forwarding.
            clear_weakrefs(object);
            assert!(getattro(proxy, name).is_null());
            assert!(pon_err_occurred());
            pon_err_clear();
            assert_eq!(setattro(proxy, name, value), -1);
            assert!(pon_err_occurred());
            pon_err_clear();
        }
    }
}
