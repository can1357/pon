//! Exception helper family namespace.
//!
//! Helpers here follow the runtime-wide NULL-sentinel discipline: fallible object
//! helpers set `PonThreadState.current_exc` and return NULL, while status helpers
//! return `-1` on helper misuse.  No native unwinding crosses the C ABI.

use core::ffi::c_int;
use core::ptr;
use std::panic::{AssertUnwindSafe, catch_unwind};

#[path = "../traceback.rs"]
mod traceback;

use crate::intern;
use crate::object::{PyObject, PyType, as_object_ptr, is_exact_type};
use crate::thread_state::{pon_err_clear, pon_err_occurred, pon_err_set_object, thread_state_lock};
use crate::types::exc::{ExceptionKind, PyBaseException, is_exception_instance, is_exception_subclass};

use super::{HandlerInfo, Runtime, TYPE_ID_EXCEPTION};

/// Exception-handler kind selector; concrete values are assigned by lowering.
pub type HandlerKind = u8;

fn catch_i32_helper(f: impl FnOnce() -> c_int) -> c_int {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(_) => super::return_minus_one_with_error("runtime helper panicked"),
    }
}

fn ensure_runtime_for_exc() -> Result<(), String> {
    super::ensure_runtime_initialized()
}

fn bytes_from_raw<'a>(ptr: *const u8, len: usize) -> Result<&'a [u8], String> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() {
        return Err("exception message pointer is null".to_owned());
    }
    // SAFETY: The helper ABI requires callers to pass `len` readable bytes.
    Ok(unsafe { core::slice::from_raw_parts(ptr, len) })
}

fn diagnostic_sentinel() -> *mut PyObject {
    core::ptr::NonNull::<PyObject>::dangling().as_ptr()
}

fn is_diagnostic_sentinel(value: *mut PyObject) -> bool {
    value == diagnostic_sentinel()
}

fn active_context() -> *mut PyObject {
    let current = thread_state_lock().current_exc;
    if current.is_null() || is_diagnostic_sentinel(current) {
        ptr::null_mut()
    } else {
        current
    }
}

pub(super) fn alloc_exception_object(
    runtime: &Runtime,
    ty: *mut PyType,
    message: *mut PyObject,
    cause: *mut PyObject,
) -> Result<*mut PyObject, String> {
    if ty.is_null() {
        return Err("exception type is null".to_owned());
    }

    let object = runtime
        .heap
        .alloc(core::mem::size_of::<PyBaseException>(), TYPE_ID_EXCEPTION)
        .cast::<PyBaseException>();
    let context = active_context();
    // SAFETY: `object` points to a freshly allocated zeroed block of the right size.
    unsafe {
        ptr::write(
            object,
            PyBaseException::new(ty.cast_const(), message, cause, context, ptr::null_mut()),
        );
    }
    Ok(as_object_ptr(object))
}

fn install_current_exception(exception: *mut PyObject, diagnostic: String) {
    traceback::record_current_frame(exception);
    pon_err_set_object(exception, diagnostic);
}

fn set_current_exception(runtime: &Runtime, exception: *mut PyObject) {
    install_current_exception(exception, exception_diagnostic(runtime, exception));
}

fn raise_builtin_value(runtime: &Runtime, kind: ExceptionKind, value: *mut PyObject, diagnostic: String) -> *mut PyObject {
    match alloc_exception_object(runtime, runtime.exception_types.get(kind), value, ptr::null_mut()) {
        Ok(exception) => {
            install_current_exception(exception, diagnostic);
            ptr::null_mut()
        }
        Err(message) => super::return_null_with_error(message),
    }
}

fn raise_builtin_text(runtime: &Runtime, kind: ExceptionKind, text: &str) -> *mut PyObject {
    match super::alloc_unicode(runtime, text.as_bytes()) {
        Ok(message) => raise_builtin_value(runtime, kind, message, format!("{}: {text}", exception_kind_name(kind))),
        Err(message) => super::return_null_with_error(message),
    }
}

fn raise_type_error_text(text: &str) -> *mut PyObject {
    match ensure_runtime_for_exc() {
        Ok(()) => match super::with_runtime(|runtime| raise_builtin_text(runtime, ExceptionKind::TypeError, text)) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}

pub fn raise_import_error_text(text: &str) -> *mut PyObject {
    match ensure_runtime_for_exc() {
        Ok(()) => match super::with_runtime(|runtime| raise_builtin_text(runtime, ExceptionKind::ImportError, text)) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}

/// Raises a typed `NameError(text)` for failed name/global/builtin lookups.
pub(super) fn raise_name_error_text(text: &str) -> *mut PyObject {
    match ensure_runtime_for_exc() {
        Ok(()) => match super::with_runtime(|runtime| raise_builtin_text(runtime, ExceptionKind::NameError, text)) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}

fn raise_message_exception(kind: ExceptionKind, ptr: *const u8, len: usize) -> *mut PyObject {
    let bytes = match bytes_from_raw(ptr, len) {
        Ok(bytes) => bytes,
        Err(message) => return raise_type_error_text(&message),
    };

    match ensure_runtime_for_exc() {
        Ok(()) => match super::with_runtime(|runtime| match super::alloc_unicode(runtime, bytes) {
            Ok(message) => raise_builtin_value(runtime, kind, message, exception_diagnostic_from_unicode(runtime, kind, message)),
            Err(message) => raise_builtin_text(runtime, ExceptionKind::TypeError, &message),
        }) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}

fn raise_value_exception(kind: ExceptionKind, value: *mut PyObject, diagnostic: String) -> *mut PyObject {
    match ensure_runtime_for_exc() {
        Ok(()) => match super::with_runtime(|runtime| raise_builtin_value(runtime, kind, value, diagnostic)) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        },
        Err(message) => super::return_null_with_error(message),
    }
}

fn is_type_object(runtime: &Runtime, object: *mut PyObject) -> bool {
    if object.is_null() {
        return false;
    }
    // SAFETY: A non-NULL `object` is expected to be a live boxed object from the ABI.
    unsafe { (*object).ob_type == runtime._type_type.cast_const() }
}

fn exception_kind_name(kind: ExceptionKind) -> &'static str {
    match kind {
        ExceptionKind::BaseException => "BaseException",
        ExceptionKind::Exception => "Exception",
        ExceptionKind::ImportError => "ImportError",
        ExceptionKind::TypeError => "TypeError",
        ExceptionKind::ValueError => "ValueError",
        ExceptionKind::KeyError => "KeyError",
        ExceptionKind::IndexError => "IndexError",
        ExceptionKind::AttributeError => "AttributeError",
        ExceptionKind::NameError => "NameError",
        ExceptionKind::NotImplementedError => "NotImplementedError",
        ExceptionKind::StopIteration => "StopIteration",
        ExceptionKind::GeneratorExit => "GeneratorExit",
        ExceptionKind::RuntimeError => "RuntimeError",
        ExceptionKind::OSError => "OSError",
        ExceptionKind::AssertionError => "AssertionError",
        ExceptionKind::BaseExceptionGroup => "BaseExceptionGroup",
        ExceptionKind::ExceptionGroup => "ExceptionGroup",
    }
}

fn exception_diagnostic_from_unicode(runtime: &Runtime, kind: ExceptionKind, value: *mut PyObject) -> String {
    let name = exception_kind_name(kind);
    if value.is_null() {
        return name.to_owned();
    }

    // SAFETY: `value` is a live boxed object allocated by the runtime.
    unsafe {
        if is_exact_type(value, runtime.unicode_type.cast_const()) {
            if let Some(text) = (*value.cast::<crate::object::PyUnicode>()).as_str() {
                return format!("{name}: {text}");
            }
        }
    }
    name.to_owned()
}

fn exception_diagnostic(runtime: &Runtime, exception: *mut PyObject) -> String {
    if exception.is_null() {
        return "NULL exception".to_owned();
    }
    if is_diagnostic_sentinel(exception) {
        return "diagnostic exception".to_owned();
    }

    // SAFETY: Callers pass a live boxed exception instance.
    unsafe {
        let ty = (*exception).ob_type;
        let name = if ty.is_null() { "BaseException" } else { (*ty).name() };
        let message = (*exception.cast::<PyBaseException>()).message;
        if !message.is_null() && is_exact_type(message, runtime.unicode_type.cast_const()) {
            if let Some(text) = (*message.cast::<crate::object::PyUnicode>()).as_str() {
                return format!("{name}: {text}");
            }
        }
        name.to_owned()
    }
}

unsafe fn set_exception_links(exception: *mut PyObject, cause: *mut PyObject) {
    if exception.is_null() || is_diagnostic_sentinel(exception) {
        return;
    }
    let context = active_context();
    // SAFETY: Caller validated that `exception` is a live base-exception instance.
    let exception = unsafe { &mut *exception.cast::<PyBaseException>() };
    exception.cause = cause;
    if !context.is_null() && !core::ptr::eq(context, exception as *mut PyBaseException as *mut PyObject) {
        exception.context = context;
    }
}

/// Raises an existing exception instance or exception type, records `cause`, and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_raise(exc: *mut PyObject, cause: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        if exc.is_null() {
            return raise_type_error_text("exceptions must derive from BaseException");
        }
        if let Err(message) = ensure_runtime_for_exc() {
            return super::return_null_with_error(message);
        }

        match super::with_runtime(|runtime| {
            if is_type_object(runtime, exc) {
                let ty = exc.cast::<PyType>();
                // SAFETY: `ty` is a live type object and the root type is initialized.
                if unsafe { !is_exception_subclass(ty.cast_const(), runtime.exception_types.base_exception.cast_const()) } {
                    return raise_builtin_text(runtime, ExceptionKind::TypeError, "exceptions must derive from BaseException");
                }
                match alloc_exception_object(runtime, ty, ptr::null_mut(), cause) {
                    Ok(exception) => {
                        set_current_exception(runtime, exception);
                        ptr::null_mut()
                    }
                    Err(message) => super::return_null_with_error(message),
                }
            } else {
                // SAFETY: `exc` is a live boxed object from the ABI.
                if unsafe { !is_exception_instance(exc, runtime.exception_types.base_exception.cast_const()) } {
                    return raise_builtin_text(runtime, ExceptionKind::TypeError, "exceptions must derive from BaseException");
                }
                // SAFETY: The branch above validated the exception instance layout.
                unsafe {
                    set_exception_links(exc, cause);
                }
                set_current_exception(runtime, exc);
                ptr::null_mut()
            }
        }) {
            Some(result) => result,
            None => super::return_null_with_error("runtime is not initialized"),
        }
    })
}

/// Re-raises the pending exception and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_reraise() -> *mut PyObject {
    super::catch_object_helper(|| {
        if pon_err_occurred() {
            ptr::null_mut()
        } else {
            match ensure_runtime_for_exc() {
                Ok(()) => match super::with_runtime(|runtime| raise_builtin_text(runtime, ExceptionKind::RuntimeError, "no active exception to reraise")) {
                    Some(result) => result,
                    None => super::return_null_with_error("runtime is not initialized"),
                },
                Err(message) => super::return_null_with_error(message),
            }
        }
    })
}

/// Raises `TypeError(message)` and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_raise_type_error(ptr: *const u8, len: usize) -> *mut PyObject {
    super::catch_object_helper(|| raise_message_exception(ExceptionKind::TypeError, ptr, len))
}

/// Raises `ValueError(message)` and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_raise_value_error(ptr: *const u8, len: usize) -> *mut PyObject {
    super::catch_object_helper(|| raise_message_exception(ExceptionKind::ValueError, ptr, len))
}

/// Raises `IndexError(message)` and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_raise_index_error(ptr: *const u8, len: usize) -> *mut PyObject {
    super::catch_object_helper(|| raise_message_exception(ExceptionKind::IndexError, ptr, len))
}

/// Raises `KeyError(key)` and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_raise_key_error(key: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| raise_value_exception(ExceptionKind::KeyError, key, "KeyError".to_owned()))
}

/// Raises `AttributeError` for `obj.name` and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_raise_attribute_error(obj: *mut PyObject, name: u32) -> *mut PyObject {
    super::catch_object_helper(|| {
        let attribute = intern::resolve(name).unwrap_or_else(|| format!("<intern:{name}>"));
        let object_name = if obj.is_null() {
            "NULL".to_owned()
        } else if is_diagnostic_sentinel(obj) {
            "diagnostic".to_owned()
        } else {
            // SAFETY: A non-NULL non-sentinel `obj` is expected to be a live boxed object.
            unsafe {
                let ty = (*obj).ob_type;
                if ty.is_null() {
                    "object".to_owned()
                } else {
                    (*ty).name().to_owned()
                }
            }
        };
        let text = format!("'{object_name}' object has no attribute '{attribute}'");
        match ensure_runtime_for_exc() {
            Ok(()) => match super::with_runtime(|runtime| raise_builtin_text(runtime, ExceptionKind::AttributeError, &text)) {
                Some(result) => result,
                None => super::return_null_with_error("runtime is not initialized"),
            },
            Err(message) => super::return_null_with_error(message),
        }
    })
}

/// Raises `StopIteration(value)` and returns NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_raise_stop_iteration(value: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| raise_value_exception(ExceptionKind::StopIteration, value, "StopIteration".to_owned()))
}

/// Returns `1` when the current exception matches `exc_type`, `0` for no match, and `-1` on misuse.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_exc_matches(exc_type: *mut PyObject) -> c_int {
    catch_i32_helper(|| {
        if exc_type.is_null() {
            raise_type_error_text("catching classes that do not inherit from BaseException is not allowed");
            return -1;
        }
        let current = thread_state_lock().current_exc;
        if current.is_null() {
            return 0;
        }
        if let Err(message) = ensure_runtime_for_exc() {
            super::return_minus_one_with_error(message);
            return -1;
        }

        match super::with_runtime(|runtime| {
            if !is_type_object(runtime, exc_type) {
                raise_builtin_text(runtime, ExceptionKind::TypeError, "catch target must be an exception type");
                return -1;
            }
            let ty = exc_type.cast::<PyType>();
            // SAFETY: `ty` is a live type object.
            if unsafe { !is_exception_subclass(ty.cast_const(), runtime.exception_types.base_exception.cast_const()) } {
                raise_builtin_text(runtime, ExceptionKind::TypeError, "catching classes that do not inherit from BaseException is not allowed");
                return -1;
            }
            if is_diagnostic_sentinel(current) {
                return 0;
            }
            // SAFETY: `current` is a live boxed object and `ty` is a live type object.
            if unsafe { is_exception_instance(current, ty.cast_const()) } {
                1
            } else {
                0
            }
        }) {
            Some(result) => result,
            None => {
                super::return_minus_one_with_error("runtime is not initialized");
                -1
            }
        }
    })
}

/// Takes the current exception, clears it, pushes it on the exception-state stack, and returns it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_exc_fetch() -> *mut PyObject {
    super::catch_object_helper(|| {
        let fetched = {
            let mut state = thread_state_lock();
            let fetched = state.current_exc;
            state.current_exc = ptr::null_mut();
            if !fetched.is_null() {
                state.push_exception_state(fetched);
            }
            fetched
        };
        pon_err_clear();
        fetched
    })
}

/// Restores a saved exception, consuming the matching saved state stack entry when present.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_exc_restore(saved: *mut PyObject) -> c_int {
    catch_i32_helper(|| {
        let stacked = {
            let mut state = thread_state_lock();
            state.pop_exception_state()
        };
        let restored = if saved.is_null() {
            stacked.unwrap_or(ptr::null_mut())
        } else {
            saved
        };

        if restored.is_null() {
            pon_err_clear();
        } else if is_diagnostic_sentinel(restored) {
            pon_err_set_object(restored, "restored diagnostic exception");
        } else if let Some(()) = super::with_runtime(|runtime| set_current_exception(runtime, restored)) {
        } else {
            pon_err_set_object(restored, "restored exception");
        }
        0
    })
}

/// Conservatively splits the current exception group against `types`.
///
/// Until exception groups carry member lists, an actual group is returned wholly
/// as the match when its group object type matches `types`; otherwise the whole
/// pending exception is returned through `out_rest`.  Non-groups never report a
/// fake successful match.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_exc_group_split(types: *mut PyObject, out_rest: *mut *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        if out_rest.is_null() {
            return raise_type_error_text("exception-group split rest pointer is null");
        }
        // SAFETY: `out_rest` is non-NULL and owned by the caller.
        unsafe {
            *out_rest = ptr::null_mut();
        }

        if types.is_null() {
            return raise_type_error_text("exception-group split target is null");
        }

        let current = thread_state_lock().current_exc;
        if current.is_null() {
            return ptr::null_mut();
        }
        if is_diagnostic_sentinel(current) {
            // SAFETY: `out_rest` is non-NULL and owned by the caller.
            unsafe {
                *out_rest = current;
            }
            return ptr::null_mut();
        }

        match ensure_runtime_for_exc() {
            Ok(()) => match super::with_runtime(|runtime| {
                if !is_type_object(runtime, types) {
                    return raise_builtin_text(runtime, ExceptionKind::TypeError, "exception-group split target must be an exception type");
                }
                let match_ty = types.cast::<PyType>();
                // SAFETY: `match_ty` is a live type object.
                if unsafe { !is_exception_subclass(match_ty.cast_const(), runtime.exception_types.base_exception.cast_const()) } {
                    return raise_builtin_text(
                        runtime,
                        ExceptionKind::TypeError,
                        "catching classes that do not inherit from BaseException is not allowed",
                    );
                }

                // SAFETY: `current` is a live boxed object.
                let current_ty = unsafe { (*current).ob_type };
                // SAFETY: `current_ty` is a live type descriptor for a boxed object.
                let is_group = unsafe { runtime.exception_types.is_exception_group_type(current_ty) };
                if !is_group {
                    // SAFETY: `out_rest` is non-NULL and owned by the caller.
                    unsafe {
                        *out_rest = current;
                    }
                    return ptr::null_mut();
                }

                // SAFETY: Both pointers are live type descriptors.
                if unsafe { is_exception_subclass(current_ty, match_ty.cast_const()) } {
                    current
                } else {
                    // SAFETY: `out_rest` is non-NULL and owned by the caller.
                    unsafe {
                        *out_rest = current;
                    }
                    ptr::null_mut()
                }
            }) {
                Some(result) => result,
                None => super::return_null_with_error("runtime is not initialized"),
            },
            Err(message) => super::return_null_with_error(message),
        }
    })
}

/// Pushes an active exception-handler record and returns `None`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_push_exc_info(target: u32, stack_depth: u32, kind: HandlerKind) -> *mut PyObject {
    super::catch_object_helper(|| {
        let frame = thread_state_lock().current_frame().unwrap_or(ptr::null_mut());
        thread_state_lock().push_handler(HandlerInfo::new(frame, target, stack_depth, kind));
        unsafe { super::pon_none() }
    })
}

/// Pops the innermost active exception-handler record and returns `None`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_pop_exc_info() -> *mut PyObject {
    super::catch_object_helper(|| {
        if thread_state_lock().pop_handler().is_none() {
            return raise_type_error_text("exception handler stack underflow");
        }
        unsafe { super::pon_none() }
    })
}

/// Returns the active exception object when it matches `exc_type`, or `None` on miss.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_match_exc(exc_type: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        let matched = unsafe { pon_exc_matches(exc_type) };
        if matched < 0 {
            return ptr::null_mut();
        }
        if matched == 0 {
            return unsafe { super::pon_none() };
        }

        let current = thread_state_lock().current_exc;
        if current.is_null() || is_diagnostic_sentinel(current) {
            unsafe { super::pon_none() }
        } else {
            current
        }
    })
}

/// Representative `except*` split.
///
/// Full exception-group member storage is not available yet, so this helper
/// returns the whole active group when its group type matches `exc_types`, and
/// returns `None` when no group match exists.  NULL remains reserved for helper
/// misuse or runtime allocation errors.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_check_exc_star(exc_types: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        let before = thread_state_lock().current_exc;
        let mut rest = ptr::null_mut();
        let matched = unsafe { pon_exc_group_split(exc_types, &mut rest) };
        if matched.is_null() {
            let after = thread_state_lock().current_exc;
            if !core::ptr::eq(before, after) {
                ptr::null_mut()
            } else {
                unsafe { super::pon_none() }
            }
        } else {
            matched
        }
    })
}

/// Returns the current object-safe exception, or `None` when there is none.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_get_current_exc() -> *mut PyObject {
    super::catch_object_helper(|| {
        let current = thread_state_lock().current_exc;
        if current.is_null() || is_diagnostic_sentinel(current) {
            unsafe { super::pon_none() }
        } else {
            current
        }
    })
}

/// Builds a representative `ExceptionGroup`.
///
/// Until the exception payload grows member-list storage, the helper validates
/// that every supplied value is an exception instance and returns a boxed
/// `ExceptionGroup` carrying only a diagnostic message.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_build_exc_group(excs: *mut *mut PyObject, len: usize) -> *mut PyObject {
    super::catch_object_helper(|| {
        if len == 0 {
            return raise_type_error_text("ExceptionGroup requires at least one exception");
        }
        if excs.is_null() {
            return raise_type_error_text("ExceptionGroup exception array is null");
        }
        if let Err(message) = ensure_runtime_for_exc() {
            return super::return_null_with_error(message);
        }

        match super::with_runtime(|runtime| {
            let values = unsafe { core::slice::from_raw_parts(excs, len) };
            for value in values {
                if (*value).is_null()
                    || unsafe { !is_exception_instance(*value, runtime.exception_types.base_exception.cast_const()) }
                {
                    return raise_builtin_text(runtime, ExceptionKind::TypeError, "ExceptionGroup members must be exceptions");
                }
            }
            match super::alloc_unicode(runtime, b"exception group") {
                Ok(message) => match alloc_exception_object(
                    runtime,
                    runtime.exception_types.exception_group,
                    message,
                    ptr::null_mut(),
                ) {
                    Ok(group) => group,
                    Err(message) => super::return_null_with_error(message),
                },
                Err(message) => super::return_null_with_error(message),
            }
        }) {
            Some(group) => group,
            None => super::return_null_with_error("runtime is not initialized"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::intern;
    use crate::thread_state::{pon_err_clear, pon_err_occurred, test_state_lock};

    fn reset_exception_state() {
        pon_err_clear();
        thread_state_lock().exception_state_stack.clear();
        thread_state_lock().handler_chain.clear();
        thread_state_lock().frame_stack.clear();
        traceback::clear_records();
    }

    fn exception_types() -> crate::types::exc::ExceptionTypeSet {
        super::ensure_runtime_for_exc().unwrap();
        super::super::with_runtime(|runtime| runtime.exception_types).unwrap()
    }

    #[test]
    fn pon_raise_matches_every_core_exception_type() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            let types = exception_types();
            for (kind, ty) in types.core_types() {
                reset_exception_state();
                assert!(pon_raise(ty.cast::<PyObject>(), ptr::null_mut()).is_null(), "{kind:?}");
                assert_eq!(pon_exc_matches(ty.cast::<PyObject>()), 1, "{kind:?}");
                assert_eq!(pon_exc_matches(types.base_exception.cast::<PyObject>()), 1, "{kind:?}");
            }
            reset_exception_state();
        }
    }

    #[test]
    fn concrete_raise_helpers_install_expected_types() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            let types = exception_types();
            assert!(pon_raise_type_error(b"bad type".as_ptr(), 8).is_null());
            assert_eq!(pon_exc_matches(types.type_error.cast::<PyObject>()), 1);
            reset_exception_state();

            assert!(pon_raise_value_error(b"bad value".as_ptr(), 9).is_null());
            assert_eq!(pon_exc_matches(types.value_error.cast::<PyObject>()), 1);
            reset_exception_state();

            assert!(pon_raise_index_error(b"bad index".as_ptr(), 9).is_null());
            assert_eq!(pon_exc_matches(types.index_error.cast::<PyObject>()), 1);
            reset_exception_state();

            let key = super::super::pon_const_str(b"missing".as_ptr(), 7);
            assert!(pon_raise_key_error(key).is_null());
            assert_eq!(pon_exc_matches(types.key_error.cast::<PyObject>()), 1);
            reset_exception_state();

            let obj = super::super::pon_none();
            assert!(pon_raise_attribute_error(obj, intern("field")).is_null());
            assert_eq!(pon_exc_matches(types.attribute_error.cast::<PyObject>()), 1);
            reset_exception_state();

            assert!(pon_raise_stop_iteration(obj).is_null());
            assert_eq!(pon_exc_matches(types.stop_iteration.cast::<PyObject>()), 1);
            reset_exception_state();
        }
    }

    #[test]
    fn fetch_restore_round_trips_through_exception_state_stack() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            let types = exception_types();
            assert!(pon_raise_value_error(b"round trip".as_ptr(), 10).is_null());
            assert_eq!(pon_exc_matches(types.value_error.cast::<PyObject>()), 1);

            let saved = pon_exc_fetch();
            assert!(!saved.is_null());
            assert!(!pon_err_occurred());
            assert_eq!(thread_state_lock().exception_states(), &[saved]);

            assert_eq!(pon_exc_restore(saved), 0);
            assert!(pon_err_occurred());
            assert_eq!(thread_state_lock().current_exc, saved);
            assert!(thread_state_lock().exception_states().is_empty());
            reset_exception_state();
        }
    }

    #[test]
    fn group_split_does_not_match_plain_exceptions_as_groups() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            let types = exception_types();
            assert!(pon_raise_value_error(b"plain".as_ptr(), 5).is_null());
            let current = thread_state_lock().current_exc;
            let mut rest = ptr::null_mut();

            let matched = pon_exc_group_split(types.value_error.cast::<PyObject>(), &mut rest);

            assert!(matched.is_null());
            assert_eq!(rest, current);
            assert_eq!(thread_state_lock().current_exc, current);
            reset_exception_state();
        }
    }

    #[test]
    fn raise_from_sets_explicit_cause() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            let types = exception_types();
            assert!(pon_raise_value_error(b"cause".as_ptr(), 5).is_null());
            let cause = thread_state_lock().current_exc;
            assert!(!cause.is_null());

            assert!(pon_raise(types.value_error.cast::<PyObject>(), cause).is_null());
            let raised = thread_state_lock().current_exc;
            assert!(!raised.is_null());
            assert_eq!((*raised.cast::<PyBaseException>()).cause, cause);
            reset_exception_state();
        }
    }

    #[test]
    fn object_safe_match_and_current_exception_helpers_return_none_on_miss() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            let types = exception_types();
            let none = super::super::pon_none();

            assert_eq!(pon_get_current_exc(), none);
            assert!(pon_raise_value_error(b"value".as_ptr(), 5).is_null());
            let current = thread_state_lock().current_exc;
            assert_eq!(pon_match_exc(types.value_error.cast::<PyObject>()), current);
            assert_eq!(pon_match_exc(types.type_error.cast::<PyObject>()), none);
            reset_exception_state();
        }
    }


    #[test]
    fn representative_exception_group_matches_except_star_type() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            let types = exception_types();
            assert!(pon_raise_value_error(b"member".as_ptr(), 6).is_null());
            let member = thread_state_lock().current_exc;
            pon_err_clear();
            let mut members = [member];
            let group = pon_build_exc_group(members.as_mut_ptr(), members.len());
            assert!(!group.is_null());

            assert!(pon_raise(group, ptr::null_mut()).is_null());
            assert_eq!(pon_check_exc_star(types.exception_group.cast::<PyObject>()), group);
            assert_eq!(pon_check_exc_star(types.value_error.cast::<PyObject>()), super::super::pon_none());
            reset_exception_state();
        }
    }
    #[test]
    fn push_pop_exc_info_round_trips_handler_chain() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            assert!(!pon_push_exc_info(42, 7, 3).is_null());
            let handlers = thread_state_lock().handlers().to_vec();
            assert_eq!(handlers.len(), 1);
            assert_eq!(handlers[0], HandlerInfo::new(ptr::null_mut(), 42, 7, 3));

            assert!(!pon_pop_exc_info().is_null());
            assert!(thread_state_lock().handlers().is_empty());
            reset_exception_state();
        }
    }

    #[test]
    fn raising_records_active_frame_for_traceback_cutover() {
        let _guard = test_state_lock();
        unsafe {
            reset_exception_state();
            let frame = core::ptr::NonNull::<super::super::PyFrame>::dangling().as_ptr();
            thread_state_lock().push_frame(frame);

            assert!(pon_raise_value_error(b"with frame".as_ptr(), 10).is_null());

            let records = traceback::records();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].frame, frame);
            assert_eq!(records[0].exception, thread_state_lock().current_exc);
            reset_exception_state();
        }
    }
}
