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
}

static WEAKREFS: LazyLock<Mutex<HashMap<usize, Vec<usize>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

static WEAKREF_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(crate::abi::runtime_type_type(), "ReferenceType", core::mem::size_of::<PyWeakRef>());
    ty.tp_new = Some(weakref_new);
    ty.tp_call = Some(weakref_call);
    ty.tp_hash = Some(weakref_hash);
    ty.tp_richcmp = Some(weakref_richcmp);
    ty.tp_getattro = Some(weakref_getattro);
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

unsafe extern "C" fn weakref_new(cls: *mut PyType, args: *mut PyObject, _kwargs: *mut PyObject) -> *mut PyObject {
    let positional = match unsafe { type_::positional_args_from_object(args) } {
        Ok(args) => args,
        Err(message) => {
            pon_err_set(message);
            return ptr::null_mut();
        }
    };
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
    let ty = if cls.is_null() { weakref_type() } else { cls };
    let object = Box::into_raw(Box::new(PyWeakRef {
        ob_base: PyObjectHeader::new(ty),
        referent,
        callback,
        hash: -1,
        hash_valid: false,
        builtin_hash: -1,
        builtin_hash_valid: false,
    }))
    .cast::<PyObject>();
    register_weakref(referent, object);
    object
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
        let callback = unsafe {
            let weakref_ref = &mut *weakref;
            weakref_ref.referent = ptr::null_mut();
            weakref_ref.callback
        };
        if !callback.is_null() {
            let mut argv = [weakref.cast::<PyObject>()];
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
