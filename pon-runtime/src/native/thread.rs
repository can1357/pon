//! Minimal native `_thread` module hook for free-threading integration.

use std::ptr;
use std::sync::{Condvar, Mutex, OnceLock};

use crate::abi::{pon_call, pon_const_int, pon_make_function, pon_none, pon_thread_start_new};
use crate::intern::intern;
use crate::native::builtins_mod::VARIADIC_ARITY;
use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::thread_state::pon_err_set;
use crate::types::{method, type_};

use super::install_module;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let start_new = unsafe { pon_make_function(native_start_new_thread as *const u8, VARIADIC_ARITY, intern("start_new_thread")) };
    if start_new.is_null() {
        return Err("failed to allocate _thread.start_new_thread".to_string());
    }
    let allocate_lock = unsafe { pon_make_function(native_allocate_lock as *const u8, 0, intern("allocate_lock")) };
    if allocate_lock.is_null() {
        return Err("failed to allocate _thread.allocate_lock".to_string());
    }
    install_module(
        "_thread",
        vec![
            (intern("start_new_thread"), start_new),
            (intern("allocate_lock"), allocate_lock),
        ],
    )
}

unsafe extern "C" fn native_start_new_thread(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc < 2 || argv.is_null() {
        pon_err_set("_thread.start_new_thread requires a callable and args tuple");
        return ptr::null_mut();
    }
    if argc > 2 {
        pon_err_set("_thread.start_new_thread kwargs are not supported");
        return ptr::null_mut();
    }
    // SAFETY: The call helper supplies `argv` with at least `argc` entries.
    let callable = unsafe { *argv };
    let args = unsafe { *argv.add(1) };
    let args = match crate::abi::seq::sequence_to_vec(args) {
        Ok(args) => args,
        Err(message) => {
            pon_err_set(message);
            return ptr::null_mut();
        }
    };
    let call = Box::new(ThreadCall { callable, args });
    let call_arg = Box::into_raw(call).cast::<PyObject>();
    let status = unsafe { pon_thread_start_new(start_new_trampoline as *const u8, call_arg) };
    if status != 0 {
        unsafe { drop(Box::from_raw(call_arg.cast::<ThreadCall>())) };
        return ptr::null_mut();
    }
    unsafe { pon_const_int(1) }
}

struct ThreadCall {
    callable: *mut PyObject,
    args: Vec<*mut PyObject>,
}

unsafe extern "C" fn start_new_trampoline(call: *mut PyObject) -> *mut PyObject {
    if call.is_null() {
        pon_err_set("_thread.start_new_thread call record is null");
        return ptr::null_mut();
    }
    let mut call = unsafe { Box::from_raw(call.cast::<ThreadCall>()) };
    let argc = call.args.len();
    let argv = if argc == 0 { ptr::null_mut() } else { call.args.as_mut_ptr() };
    unsafe { pon_call(call.callable, argv, argc) }
}

#[repr(C)]
struct PyLock {
    _ob_base: PyObjectHeader,
    state: Box<LockState>,
}

struct LockState {
    locked: Mutex<bool>,
    available: Condvar,
}

impl LockState {
    fn new() -> Self {
        Self {
            locked: Mutex::new(false),
            available: Condvar::new(),
        }
    }

    fn acquire(&self) {
        let mut locked = self.locked.lock().unwrap_or_else(|poison| poison.into_inner());
        while *locked {
            locked = self.available.wait(locked).unwrap_or_else(|poison| poison.into_inner());
        }
        *locked = true;
    }

    fn release(&self) -> Result<(), &'static str> {
        let mut locked = self.locked.lock().unwrap_or_else(|poison| poison.into_inner());
        if !*locked {
            return Err("release unlocked lock");
        }
        *locked = false;
        self.available.notify_one();
        Ok(())
    }
}

fn lock_type() -> *mut PyType {
    static LOCK_TYPE: OnceLock<usize> = OnceLock::new();
    *LOCK_TYPE.get_or_init(|| {
        let mut ty = PyType::new(ptr::null(), "lock", std::mem::size_of::<PyLock>());
        ty.tp_getattro = Some(lock_getattro);
        Box::into_raw(Box::new(ty)) as usize
    }) as *mut PyType
}

unsafe extern "C" fn lock_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        pon_err_set("lock attribute name must be str");
        return ptr::null_mut();
    };
    let (entry, arity) = match name {
        "acquire" => (lock_acquire_entry as unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject, 1),
        "release" => (lock_release_entry as unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject, 1),
        _ => {
            pon_err_set(format!("attribute '{name}' was not found"));
            return ptr::null_mut();
        }
    };
    let function = unsafe { pon_make_function(entry as *const u8, arity, intern(name)) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match method::new_bound_method(function, object) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => {
            pon_err_set(message);
            ptr::null_mut()
        }
    }
}

unsafe extern "C" fn native_allocate_lock(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    Box::into_raw(Box::new(PyLock {
        _ob_base: PyObjectHeader::new(lock_type().cast_const()),
        state: Box::new(LockState::new()),
    }))
    .cast::<PyObject>()
}

unsafe extern "C" fn lock_acquire_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(lock) = lock_receiver(argv, argc) else {
        return ptr::null_mut();
    };
    unsafe { &*lock }.state.acquire();
    unsafe { pon_const_int(1) }
}

unsafe extern "C" fn lock_release_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(lock) = lock_receiver(argv, argc) else {
        return ptr::null_mut();
    };
    match unsafe { &*lock }.state.release() {
        Ok(()) => unsafe { pon_none() },
        Err(message) => {
            pon_err_set(message);
            ptr::null_mut()
        }
    }
}

fn lock_receiver(argv: *mut *mut PyObject, argc: usize) -> Option<*mut PyLock> {
    if argc == 0 || argv.is_null() {
        pon_err_set("lock method missing receiver");
        return None;
    }
    let receiver = unsafe { *argv };
    if receiver.is_null() || unsafe { (*receiver).ob_type } != lock_type().cast_const() {
        pon_err_set("lock method receiver is not a lock");
        return None;
    }
    Some(receiver.cast::<PyLock>())
}
