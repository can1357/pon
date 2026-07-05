//! Error family: exception raising/inspection and `PyExc_*` singletons.
//!
//! Exception classes cross the boundary as foreign twins (see [`super::twin`]),
//! so `PyErr_Occurred() == PyExc_TypeError` holds by pointer identity.

use core::ffi::{c_char, c_int};
use core::ptr;
use std::io::Write;

use crate::abi;
use crate::intern::intern;
use crate::object::{PyObject, PyType};
use crate::thread_state::{pon_err_clear, pon_err_message, pon_err_occurred, thread_state_lock};
use crate::types::exc::{ExceptionKind, PyBaseException};

use super::c_string;
use super::twin::{self, ForeignTypeObject};

/// C mirror: `include/pon_capi/err.h` `PyPonCapiErr`.
#[repr(C)]
pub(crate) struct PyPonCapiErr {
    set_string: unsafe extern "C" fn(*mut PyObject, *const c_char),
    set_object: unsafe extern "C" fn(*mut PyObject, *mut PyObject),
    set_none: unsafe extern "C" fn(*mut PyObject),
    occurred: unsafe extern "C" fn() -> *mut PyObject,
    clear: unsafe extern "C" fn(),
    exc_base_exception: *mut PyObject,
    exc_exception: *mut PyObject,
    exc_runtime_error: *mut PyObject,
    exc_type_error: *mut PyObject,
    exc_value_error: *mut PyObject,
    exc_import_error: *mut PyObject,
    exc_overflow_error: *mut PyObject,
    exc_index_error: *mut PyObject,
    exc_key_error: *mut PyObject,
    exc_attribute_error: *mut PyObject,
    exc_not_implemented_error: *mut PyObject,
    exc_stop_iteration: *mut PyObject,
    exc_memory_error: *mut PyObject,
    exc_os_error: *mut PyObject,
    exc_system_error: *mut PyObject,
    exc_buffer_error: *mut PyObject,
    exc_zero_division_error: *mut PyObject,
    exc_arithmetic_error: *mut PyObject,
    exc_floating_point_error: *mut PyObject,
    exc_deprecation_warning: *mut PyObject,
    exc_runtime_warning: *mut PyObject,
    exc_user_warning: *mut PyObject,
    exc_lookup_error: *mut PyObject,
    exception_matches: unsafe extern "C" fn(*mut PyObject) -> c_int,
    given_exception_matches: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int,
    fetch: unsafe extern "C" fn(*mut *mut PyObject, *mut *mut PyObject, *mut *mut PyObject),
    restore: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject),
    warn_ex: unsafe extern "C" fn(*mut PyObject, *const c_char, isize) -> c_int,
    write_unraisable: unsafe extern "C" fn(*mut PyObject),
    normalize_exception: unsafe extern "C" fn(*mut *mut PyObject, *mut *mut PyObject, *mut *mut PyObject),
    print: unsafe extern "C" fn(),
    print_ex: unsafe extern "C" fn(c_int),
    set_from_errno: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    exception_set_cause: unsafe extern "C" fn(*mut PyObject, *mut PyObject),
    exception_set_context: unsafe extern "C" fn(*mut PyObject, *mut PyObject),
    exception_set_traceback: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int,
    exc_warning: *mut PyObject,
    exc_future_warning: *mut PyObject,
    exc_import_warning: *mut PyObject,
    exc_module_not_found_error: *mut PyObject,
    exc_assertion_error: *mut PyObject,
    exc_name_error: *mut PyObject,
    exc_unicode_error: *mut PyObject,
    exc_unicode_encode_error: *mut PyObject,
    exc_unicode_decode_error: *mut PyObject,
    exc_recursion_error: *mut PyObject,
    new_exception: unsafe extern "C" fn(*const c_char, *mut PyObject, *mut PyObject) -> *mut PyObject,
    check_signals: unsafe extern "C" fn() -> c_int,
    set_raised_exception: unsafe extern "C" fn(*mut PyObject),
    exc_generator_exit: *mut PyObject,
    exc_stop_async_iteration: *mut PyObject,
    exc_unbound_local_error: *mut PyObject,
}

unsafe impl Send for PyPonCapiErr {}
unsafe impl Sync for PyPonCapiErr {}

/// Builds the family table; requires an initialized runtime (twin fabrication
/// touches the exception hierarchy).
pub(crate) fn build() -> PyPonCapiErr {
    let singleton = |kind: ExceptionKind| -> *mut PyObject {
        twin::foreign_of_native(abi::exception_type_object(kind)).cast::<PyObject>()
    };
    PyPonCapiErr {
        set_string: capi_err_set_string,
        set_object: capi_err_set_object,
        set_none: capi_err_set_none,
        occurred: capi_err_occurred,
        clear: capi_err_clear,
        exc_base_exception: singleton(ExceptionKind::BaseException),
        exc_exception: singleton(ExceptionKind::Exception),
        exc_runtime_error: singleton(ExceptionKind::RuntimeError),
        exc_type_error: singleton(ExceptionKind::TypeError),
        exc_value_error: singleton(ExceptionKind::ValueError),
        exc_import_error: singleton(ExceptionKind::ImportError),
        exc_overflow_error: singleton(ExceptionKind::OverflowError),
        exc_index_error: singleton(ExceptionKind::IndexError),
        exc_key_error: singleton(ExceptionKind::KeyError),
        exc_attribute_error: singleton(ExceptionKind::AttributeError),
        exc_not_implemented_error: singleton(ExceptionKind::NotImplementedError),
        exc_stop_iteration: singleton(ExceptionKind::StopIteration),
        exc_memory_error: singleton(ExceptionKind::MemoryError),
        exc_os_error: singleton(ExceptionKind::OSError),
        exc_system_error: singleton(ExceptionKind::SystemError),
        exc_buffer_error: singleton(ExceptionKind::BufferError),
        exc_zero_division_error: singleton(ExceptionKind::ZeroDivisionError),
        exc_arithmetic_error: singleton(ExceptionKind::ArithmeticError),
        exc_floating_point_error: singleton(ExceptionKind::FloatingPointError),
        exc_deprecation_warning: singleton(ExceptionKind::DeprecationWarning),
        exc_runtime_warning: singleton(ExceptionKind::RuntimeWarning),
        exc_user_warning: singleton(ExceptionKind::UserWarning),
        exc_lookup_error: singleton(ExceptionKind::LookupError),
        exception_matches: capi_err_exception_matches,
        given_exception_matches: capi_err_given_exception_matches,
        fetch: capi_err_fetch,
        restore: capi_err_restore,
        warn_ex: capi_err_warn_ex,
        write_unraisable: capi_err_write_unraisable,
        normalize_exception: capi_err_normalize_exception,
        print: capi_err_print,
        print_ex: capi_err_print_ex,
        set_from_errno: capi_err_set_from_errno,
        exception_set_cause: capi_exception_set_cause,
        exception_set_context: capi_exception_set_context,
        exception_set_traceback: capi_exception_set_traceback,
        exc_warning: singleton(ExceptionKind::Warning),
        exc_future_warning: singleton(ExceptionKind::FutureWarning),
        exc_import_warning: singleton(ExceptionKind::ImportWarning),
        exc_module_not_found_error: singleton(ExceptionKind::ModuleNotFoundError),
        exc_assertion_error: singleton(ExceptionKind::AssertionError),
        exc_name_error: singleton(ExceptionKind::NameError),
        exc_unicode_error: singleton(ExceptionKind::UnicodeError),
        exc_unicode_encode_error: singleton(ExceptionKind::UnicodeEncodeError),
        exc_unicode_decode_error: singleton(ExceptionKind::UnicodeDecodeError),
        exc_recursion_error: singleton(ExceptionKind::RecursionError),
        new_exception: capi_err_new_exception,
        check_signals: capi_err_check_signals,
        set_raised_exception: capi_err_set_raised_exception,
        exc_generator_exit: singleton(ExceptionKind::GeneratorExit),
        exc_stop_async_iteration: singleton(ExceptionKind::StopAsyncIteration),
        exc_unbound_local_error: singleton(ExceptionKind::UnboundLocalError),
    }
}

/// Resolves a boundary exception-class argument to its runtime-native type
/// object, as a callable class `PyObject`.
unsafe fn native_class(exception: *mut PyObject) -> Option<*mut PyObject> {
    unsafe { native_class_type(exception) }.map(|native| native.cast::<PyObject>())
}

unsafe fn native_class_type(exception: *mut PyObject) -> Option<*mut PyType> {
    if exception.is_null() {
        return None;
    }
    if let Some(native) = twin::native_of_foreign(exception.cast::<ForeignTypeObject>()) {
        return Some(native);
    }
    if crate::tag::is_heap(exception) && unsafe { crate::types::type_::is_type_object(exception) } {
        return Some(exception.cast::<PyType>());
    }
    None
}

unsafe fn given_exception_type(given: *mut PyObject) -> Option<*mut PyType> {
    if given.is_null() {
        return None;
    }
    if let Some(native) = twin::registered_native_of_foreign(given.cast::<ForeignTypeObject>()) {
        return Some(native);
    }
    if crate::tag::is_heap(given) && unsafe { crate::types::type_::is_type_object(given) } {
        return Some(given.cast::<PyType>());
    }
    None
}

/// Raises `class(*argv)`; on constructor failure the pending error from the
/// failed call is left in place.
unsafe fn raise_call(class: *mut PyObject, argv: &mut [*mut PyObject]) {
    let instance = unsafe { abi::pon_call(class, argv.as_mut_ptr(), argv.len()) };
    if instance.is_null() {
        return;
    }
    let _ = unsafe { abi::exc::pon_raise(instance, ptr::null_mut()) };
}

unsafe extern "C" fn capi_err_set_string(exception: *mut PyObject, message: *const c_char) {
    let text = c_string(message).unwrap_or_else(|| "C extension error".to_owned());
    let Some(class) = (unsafe { native_class(exception) }) else {
        let _ = abi::exc::raise_kind_error_text(ExceptionKind::RuntimeError, &text);
        return;
    };
    let message_object = unsafe { abi::pon_const_str(text.as_ptr(), text.len()) };
    if message_object.is_null() {
        return;
    }
    let mut argv = [message_object];
    unsafe { raise_call(class, &mut argv) };
}

unsafe extern "C" fn capi_err_set_object(exception: *mut PyObject, value: *mut PyObject) {
    let Some(class) = (unsafe { native_class(exception) }) else {
        let _ = abi::exc::raise_kind_error_text(ExceptionKind::RuntimeError, "C extension error");
        return;
    };
    if !value.is_null() {
        // CPython semantics: an instance of the class is raised as-is.
        if unsafe { crate::types::exc::is_exception_instance(value, class.cast()) } {
            let _ = unsafe { abi::exc::pon_raise(value, ptr::null_mut()) };
            return;
        }
        let mut argv = [value];
        unsafe { raise_call(class, &mut argv) };
        return;
    }
    unsafe { raise_call(class, &mut []) };
}

unsafe extern "C" fn capi_err_set_none(exception: *mut PyObject) {
    let Some(class) = (unsafe { native_class(exception) }) else {
        let _ = abi::exc::raise_kind_error_text(ExceptionKind::RuntimeError, "C extension error");
        return;
    };
    if class.cast::<PyType>() == abi::exception_type_object(ExceptionKind::StopIteration) {
        let none = unsafe { abi::pon_none() };
        if none.is_null() {
            return;
        }
        let mut argv = [none];
        unsafe { raise_call(class, &mut argv) };
        return;
    }
    unsafe { raise_call(class, &mut []) };
}

/// Returns the pending exception's TYPE as a foreign twin (CPython contract:
/// borrowed type object, NULL when no error is pending). Message-only
/// diagnostics report as `RuntimeError`.
unsafe extern "C" fn capi_err_occurred() -> *mut PyObject {
    if let Some(exception) = abi::exc::pending_exception_object() {
        // SAFETY: live boxed exception per `pending_exception_object`.
        let ty = unsafe { (*exception).ob_type }.cast_mut();
        return twin::foreign_of_native(ty).cast::<PyObject>();
    }
    if pon_err_occurred() {
        return twin::foreign_of_native(abi::exception_type_object(ExceptionKind::RuntimeError)).cast::<PyObject>();
    }
    ptr::null_mut()
}

unsafe extern "C" fn capi_err_clear() {
    pon_err_clear();
}

unsafe extern "C" fn capi_err_exception_matches(exception: *mut PyObject) -> c_int {
    let Some(target) = (unsafe { native_class_type(exception) }) else {
        return 0;
    };
    if let Some(pending) = abi::exc::pending_exception_object() {
        if !crate::tag::is_heap(pending) {
            return 0;
        }
        // SAFETY: `pending` is a live heap exception object per `pending_exception_object`.
        let pending_type = unsafe { (*pending).ob_type };
        return (unsafe { crate::types::exc::is_exception_subclass(pending_type, target.cast_const()) }) as c_int;
    }
    if pon_err_occurred() {
        let runtime_error = abi::exception_type_object(ExceptionKind::RuntimeError);
        return (unsafe { crate::types::exc::is_exception_subclass(runtime_error.cast_const(), target.cast_const()) }) as c_int;
    }
    0
}

unsafe extern "C" fn capi_err_given_exception_matches(given: *mut PyObject, exception: *mut PyObject) -> c_int {
    let Some(target) = (unsafe { native_class_type(exception) }) else {
        return 0;
    };
    if let Some(given_type) = unsafe { given_exception_type(given) } {
        return (unsafe { crate::types::exc::is_exception_subclass(given_type.cast_const(), target.cast_const()) }) as c_int;
    }
    if given.is_null() || !crate::tag::is_heap(given) {
        return 0;
    }
    (unsafe { crate::types::exc::is_exception_instance(given, target.cast_const()) }) as c_int
}

unsafe extern "C" fn capi_err_fetch(
    ptype: *mut *mut PyObject,
    pvalue: *mut *mut PyObject,
    ptraceback: *mut *mut PyObject,
) {
    if !ptype.is_null() {
        unsafe { *ptype = ptr::null_mut() };
    }
    if !pvalue.is_null() {
        unsafe { *pvalue = ptr::null_mut() };
    }
    if !ptraceback.is_null() {
        unsafe { *ptraceback = ptr::null_mut() };
    }

    let current = thread_state_lock().current_exc;
    if current.is_null() {
        return;
    }
    pon_err_clear();

    if abi::exc::is_diagnostic_sentinel(current) || !crate::tag::is_heap(current) {
        if !ptype.is_null() {
            unsafe { *ptype = twin::foreign_of_native(abi::exception_type_object(ExceptionKind::RuntimeError)).cast::<PyObject>() };
        }
        return;
    }

    // SAFETY: `current` is a heap exception object from the thread state.
    let ty = unsafe { (*current).ob_type }.cast_mut();
    if !ptype.is_null() {
        unsafe { *ptype = twin::foreign_of_native(ty).cast::<PyObject>() };
    }
    if !pvalue.is_null() {
        unsafe { *pvalue = current };
    }
}

unsafe extern "C" fn capi_err_restore(exception: *mut PyObject, value: *mut PyObject, traceback: *mut PyObject) {
    let _ = traceback;
    if exception.is_null() && value.is_null() {
        pon_err_clear();
        return;
    }

    let base_exception = abi::exception_type_object(ExceptionKind::BaseException);
    if !value.is_null()
        && crate::tag::is_heap(value)
        && unsafe { crate::types::exc::is_exception_instance(value, base_exception.cast_const()) }
    {
        let _ = unsafe { abi::exc::pon_raise(value, ptr::null_mut()) };
        return;
    }

    if !exception.is_null()
        && crate::tag::is_heap(exception)
        && unsafe { crate::types::exc::is_exception_instance(exception, base_exception.cast_const()) }
    {
        let _ = unsafe { abi::exc::pon_raise(exception, ptr::null_mut()) };
        return;
    }

    if !exception.is_null() {
        if value.is_null() {
            unsafe { capi_err_set_none(exception) };
        } else {
            unsafe { capi_err_set_object(exception, value) };
        }
        return;
    }

    let _ = abi::exc::raise_kind_error_text(ExceptionKind::SystemError, "PyErr_Restore called without an exception type");
}

unsafe extern "C" fn capi_err_set_raised_exception(exception: *mut PyObject) {
    if exception.is_null() {
        pon_err_clear();
        return;
    }
    let normalized = crate::tag::untag_arg(exception);
    let base_exception = abi::exception_type_object(ExceptionKind::BaseException);
    if normalized.is_null()
        || !crate::tag::is_heap(normalized)
        || unsafe { !crate::types::exc::is_exception_instance(normalized, base_exception.cast_const()) }
    {
        let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, "PyErr_SetRaisedException expected a BaseException instance");
        super::unpin_object(exception);
        return;
    }
    let _ = unsafe { abi::exc::pon_raise(normalized, ptr::null_mut()) };
    super::unpin_object(exception);
}

unsafe fn warning_category_type(category: *mut PyObject) -> Option<*mut PyType> {
    let ty = if category.is_null() {
        abi::exception_type_object(ExceptionKind::Warning)
    } else {
        let Some(native) = (unsafe { native_class_type(category) }) else {
            let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, "category must be a Warning subclass");
            return None;
        };
        native
    };
    let warning = abi::exception_type_object(ExceptionKind::Warning);
    if unsafe { !crate::types::exc::is_exception_subclass(ty.cast_const(), warning.cast_const()) } {
        let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, "category must be a Warning subclass");
        return None;
    }
    Some(ty)
}

unsafe fn warn_as_error(category: *mut PyType, text: &str) -> c_int {
    let message = unsafe { abi::pon_const_str(text.as_ptr(), text.len()) };
    if message.is_null() {
        return -1;
    }
    let mut argv = [message];
    unsafe { raise_call(category.cast::<PyObject>(), &mut argv) };
    -1
}

unsafe extern "C" fn capi_err_warn_ex(category: *mut PyObject, message: *const c_char, stack_level: isize) -> c_int {
    let text = c_string(message).unwrap_or_default();
    let Some(category) = (unsafe { warning_category_type(category) }) else {
        return -1;
    };
    let warnings = import_module_text("warnings");
    if warnings.is_null() {
        pon_err_clear();
        return unsafe { warn_as_error(category, &text) };
    }
    let no_python_frame = abi::current_function_stack_depth() == 0;
    let warn_name = if no_python_frame { "warn_explicit" } else { "warn" };
    let warn = unsafe { abi::pon_get_attr(warnings, intern(warn_name), ptr::null_mut()) };
    if warn.is_null() {
        pon_err_clear();
        return unsafe { warn_as_error(category, &text) };
    }
    let message = unsafe { abi::pon_const_str(text.as_ptr(), text.len()) };
    if message.is_null() {
        return -1;
    }
    let category = category.cast::<PyObject>();
    let result = if no_python_frame {
        // Direct C-extension calls from Rust tests have no Python frame for
        // warnings.warn() to inspect. Match CPython's no-frame fallback by
        // issuing the warning at an explicit synthetic <sys>:0 location.
        let filename = unsafe { abi::pon_const_str("<sys>".as_ptr(), "<sys>".len()) };
        let lineno = unsafe { abi::pon_const_int(0) };
        if filename.is_null() || lineno.is_null() {
            return -1;
        }
        let mut argv = [message, category, filename, lineno];
        unsafe { abi::pon_call(warn, argv.as_mut_ptr(), argv.len()) }
    } else {
        let stack_level = unsafe { abi::pon_const_int(stack_level as i64) };
        if stack_level.is_null() {
            return -1;
        }
        let mut argv = [message, category, stack_level];
        unsafe { abi::pon_call(warn, argv.as_mut_ptr(), argv.len()) }
    };
    if result.is_null() { -1 } else { 0 }
}

unsafe extern "C" fn capi_err_write_unraisable(object: *mut PyObject) {
    let pending = abi::exc::pending_exception_object();
    let diagnostic = pon_err_message().unwrap_or_else(|| "unraisable exception".to_owned());
    let subject = if object.is_null() || !crate::tag::is_heap(object) {
        None
    } else {
        abi::format_object_for_print(object).ok()
    };
    let pending_text = pending.and_then(|exception| abi::format_object_for_print(exception).ok());
    match (subject, pending_text) {
        (Some(subject), Some(pending)) => eprintln!("Exception ignored in: {subject}: {pending}"),
        (Some(subject), None) => eprintln!("Exception ignored in: {subject}: {diagnostic}"),
        (None, Some(pending)) => eprintln!("Exception ignored: {pending}"),
        (None, None) => eprintln!("Exception ignored: {diagnostic}"),
    }
    pon_err_clear();
}

unsafe extern "C" fn capi_err_normalize_exception(
    ptype: *mut *mut PyObject,
    pvalue: *mut *mut PyObject,
    _ptraceback: *mut *mut PyObject,
) {
    if ptype.is_null() || pvalue.is_null() {
        let _ = abi::exc::raise_kind_error_text(ExceptionKind::SystemError, "PyErr_NormalizeException requires type and value slots");
        return;
    }

    let exception_type = unsafe { *ptype };
    let Some(native_type) = (unsafe { native_class_type(exception_type) }) else {
        let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, "PyErr_NormalizeException expected an exception type");
        return;
    };

    let original_value = unsafe { *pvalue };
    let value = crate::tag::untag_arg(original_value);
    if crate::tag::is_small_int(original_value) && value.is_null() {
        return;
    }
    if !value.is_null() && !crate::tag::is_heap(value) {
        let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, "PyErr_NormalizeException value is not an object");
        return;
    }
    unsafe { *pvalue = value };

    if !value.is_null() && unsafe { crate::types::exc::is_exception_instance(value, native_type.cast_const()) } {
        return;
    }

    let mut argv = [value];
    let argv_ptr = if value.is_null() { ptr::null_mut() } else { argv.as_mut_ptr() };
    let argc = if value.is_null() { 0 } else { 1 };
    let instance = unsafe { abi::pon_call(native_type.cast::<PyObject>(), argv_ptr, argc) };
    if instance.is_null() {
        return;
    }
    if unsafe { !crate::types::exc::is_exception_instance(instance, native_type.cast_const()) } {
        let type_name = unsafe { (*native_type).name() };
        let _ = abi::exc::raise_kind_error_text(
            ExceptionKind::TypeError,
            &format!("PyErr_NormalizeException: {type_name} constructor did not return an exception instance"),
        );
        return;
    }
    unsafe { *pvalue = instance };
}

unsafe extern "C" fn capi_err_print() {
    unsafe { capi_err_print_ex(1) };
}

unsafe extern "C" fn capi_err_print_ex(set_sys_last_vars: c_int) {
    // Pon does not expose CPython's sys.last_* slots through the C-API shim yet;
    // the flag is intentionally ignored while matching the top-level renderer.
    let _ = set_sys_last_vars;
    if !pon_err_occurred() {
        return;
    }
    let message = pon_err_message()
        .or_else(|| abi::exc::pending_exception_object().and_then(|exception| abi::format_object_for_print(exception).ok()))
        .unwrap_or_else(|| "uncaught exception without diagnostic".to_owned());
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "Traceback (most recent call last):");
    let _ = writeln!(stderr, "{message}");
    let _ = stderr.flush();
    pon_err_clear();
}

unsafe extern "C" fn capi_err_set_from_errno(exception: *mut PyObject) -> *mut PyObject {
    let Some(class) = (unsafe { native_class(exception) }) else {
        let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, "PyErr_SetFromErrno expected an exception type");
        return ptr::null_mut();
    };
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO);
    let message = unsafe { c_string(libc::strerror(errno)) }.unwrap_or_else(|| format!("Unknown error {errno}"));
    let errno_object = unsafe { abi::pon_const_int(i64::from(errno)) };
    if errno_object.is_null() {
        return ptr::null_mut();
    }
    let message_object = unsafe { abi::pon_const_str(message.as_ptr(), message.len()) };
    if message_object.is_null() {
        return ptr::null_mut();
    }
    let mut argv = [errno_object, message_object];
    unsafe { raise_call(class, &mut argv) };
    ptr::null_mut()
}

unsafe fn exception_payload(exception: *mut PyObject, api_name: &str) -> Option<*mut PyBaseException> {
    let normalized = crate::tag::untag_arg(exception);
    if crate::tag::is_small_int(exception) && normalized.is_null() {
        return None;
    }
    if normalized.is_null() || !crate::tag::is_heap(normalized) {
        let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, &format!("{api_name} expected a BaseException instance"));
        return None;
    }
    let base_exception = abi::exception_type_object(ExceptionKind::BaseException);
    if unsafe { !crate::types::exc::is_exception_instance(normalized, base_exception.cast_const()) } {
        let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, &format!("{api_name} expected a BaseException instance"));
        return None;
    }
    Some(normalized.cast::<PyBaseException>())
}

unsafe fn normalize_nullable_object_arg(value: *mut PyObject, api_name: &str) -> Option<*mut PyObject> {
    if value.is_null() {
        return Some(ptr::null_mut());
    }
    let normalized = crate::tag::untag_arg(value);
    if crate::tag::is_small_int(value) && normalized.is_null() {
        return None;
    }
    if normalized.is_null() || !crate::tag::is_heap(normalized) {
        let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, &format!("{api_name} argument is not an object"));
        return None;
    }
    Some(normalized)
}

unsafe extern "C" fn capi_exception_set_cause(exception: *mut PyObject, cause: *mut PyObject) {
    let Some(exception) = (unsafe { exception_payload(exception, "PyException_SetCause") }) else {
        return;
    };
    let Some(cause) = (unsafe { normalize_nullable_object_arg(cause, "PyException_SetCause") }) else {
        return;
    };
    unsafe {
        (*exception).cause = cause;
        if !cause.is_null() {
            (*exception).suppress_context = true;
        }
    }
}

unsafe extern "C" fn capi_exception_set_context(exception: *mut PyObject, context: *mut PyObject) {
    let Some(exception) = (unsafe { exception_payload(exception, "PyException_SetContext") }) else {
        return;
    };
    let Some(context) = (unsafe { normalize_nullable_object_arg(context, "PyException_SetContext") }) else {
        return;
    };
    unsafe { (*exception).context = context };
}

unsafe extern "C" fn capi_exception_set_traceback(exception: *mut PyObject, traceback: *mut PyObject) -> c_int {
    let Some(exception) = (unsafe { exception_payload(exception, "PyException_SetTraceback") }) else {
        return -1;
    };
    let Some(traceback) = (unsafe { normalize_nullable_object_arg(traceback, "PyException_SetTraceback") }) else {
        return -1;
    };
    let traceback = if traceback == unsafe { abi::pon_none() } {
        ptr::null_mut()
    } else {
        traceback
    };
    if !traceback.is_null() {
        let is_traceback = unsafe { !(*traceback).ob_type.is_null() && (*(*traceback).ob_type).name() == "traceback" };
        if !is_traceback {
            let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, "__traceback__ must be a traceback or None");
            return -1;
        }
    }
    unsafe { (*exception).traceback = traceback };
    0
}

unsafe fn exception_base_from_arg(base: *mut PyObject) -> Option<*mut PyObject> {
    let native = if base.is_null() {
        abi::exception_type_object(ExceptionKind::Exception)
    } else {
        let Some(native) = (unsafe { native_class_type(base) }) else {
            let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, "PyErr_NewException base must be an exception class");
            return None;
        };
        native
    };
    if !abi::exc::type_derives_base_exception(native.cast_const()) {
        let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, "PyErr_NewException base must derive from BaseException");
        return None;
    }
    Some(native.cast::<PyObject>())
}

unsafe fn exception_bases_from_arg(base: *mut PyObject) -> Option<Vec<*mut PyObject>> {
    if base.is_null() {
        return unsafe { exception_base_from_arg(base) }.map(|single| vec![single]);
    }
    if let Some(native) = unsafe { native_class_type(base) } {
        if !abi::exc::type_derives_base_exception(native.cast_const()) {
            let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, "PyErr_NewException base must derive from BaseException");
            return None;
        }
        return Some(vec![native.cast::<PyObject>()]);
    }
    let raw_bases = match unsafe { crate::types::type_::positional_args_from_object(base) } {
        Ok(raw_bases) => raw_bases,
        Err(_) => {
            let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, "PyErr_NewException base must be an exception class or tuple");
            return None;
        }
    };
    if raw_bases.is_empty() {
        let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, "PyErr_NewException base tuple must not be empty");
        return None;
    }
    let mut bases = Vec::with_capacity(raw_bases.len());
    for raw in raw_bases {
        bases.push(unsafe { exception_base_from_arg(raw) }?);
    }
    Some(bases)
}

unsafe fn set_namespace_str(namespace: *mut crate::types::type_::PyClassDict, name: &str, value: &str) -> bool {
    let object = unsafe { abi::pon_const_str(value.as_ptr(), value.len()) };
    if object.is_null() {
        return false;
    }
    unsafe { (&mut *namespace).set(intern(name), object) };
    true
}

unsafe fn copy_exception_namespace_entries(dict: *mut PyObject, namespace: *mut crate::types::type_::PyClassDict) -> bool {
    if dict.is_null() {
        return true;
    }
    let entries = if unsafe { crate::types::type_::is_class_dict_view(dict) } {
        match unsafe { crate::types::type_::class_dict_view_entries_snapshot(dict) } {
            Ok(entries) => entries,
            Err(message) => {
                let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, &format!("PyErr_NewException dict copy failed: {message}"));
                return false;
            }
        }
    } else {
        match unsafe { crate::types::dict::dict_entries_snapshot(dict) } {
            Ok(entries) => entries,
            Err(_) => {
                let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, "PyErr_NewException dict must be a dict");
                return false;
            }
        }
    };
    for entry in entries {
        let Some(key) = (unsafe { crate::types::type_::unicode_text(entry.key) }) else {
            let _ = abi::exc::raise_kind_error_text(ExceptionKind::TypeError, "PyErr_NewException dict keys must be str");
            return false;
        };
        unsafe { (&mut *namespace).set(intern(key), entry.value) };
    }
    true
}

unsafe extern "C" fn capi_err_new_exception(name: *const c_char, base: *mut PyObject, dict: *mut PyObject) -> *mut PyObject {
    let Some(full_name) = c_string(name) else {
        let _ = abi::exc::raise_kind_error_text(ExceptionKind::SystemError, "PyErr_NewException name must not be NULL");
        return ptr::null_mut();
    };
    let Some(dot) = full_name.rfind('.') else {
        let _ = abi::exc::raise_kind_error_text(ExceptionKind::SystemError, "PyErr_NewException name must be module-qualified");
        return ptr::null_mut();
    };
    if dot == 0 || dot + 1 == full_name.len() {
        let _ = abi::exc::raise_kind_error_text(ExceptionKind::SystemError, "PyErr_NewException received an invalid qualified name");
        return ptr::null_mut();
    }
    let module_name = &full_name[..dot];
    let class_name = &full_name[dot + 1..];
    let Some(bases) = (unsafe { exception_bases_from_arg(base) }) else {
        return ptr::null_mut();
    };
    let namespace = crate::types::type_::new_namespace();
    if !unsafe { copy_exception_namespace_entries(dict, namespace) } {
        unsafe { drop(Box::from_raw(namespace)) };
        return ptr::null_mut();
    }
    if !unsafe { set_namespace_str(namespace, "__module__", module_name) } {
        unsafe { drop(Box::from_raw(namespace)) };
        return ptr::null_mut();
    }
    super::pin_new_reference(unsafe { crate::types::type_::build_class_from_namespace(class_name, &bases, namespace, &[]) })
}

unsafe extern "C" fn capi_err_check_signals() -> c_int {
    // Pon's native `_signal` module stores handler registrations but installs no
    // asynchronous OS-signal trampoline; there is therefore no pending signal
    // state for this single-threaded C-API surface to drain.
    0
}

fn import_module_text(name: &str) -> *mut PyObject {
    let name_id = intern(name);
    let fromlist = [intern("*")];
    unsafe { crate::import::pon_import_name(name_id, fromlist.as_ptr(), fromlist.len(), 0) }
}

#[cfg(test)]
mod tests {
    use core::ptr;

    use super::super::load_extension_module;
    use super::super::tests::{ResetImportStateOnDrop, TempExtensionRoot, compile_extension};
    use crate::abi::{format_object_for_print, pon_call, pon_runtime_init};
    use crate::import::module_attr;
    use crate::intern::intern;
    use crate::thread_state::{pon_err_message, test_state_lock};

    #[test]
    fn err_family_c_api_exception_surface() {
        let _guard = test_state_lock();
        let _reset = ResetImportStateOnDrop;
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
        }

        let temp = TempExtensionRoot::new();
        let module_path = compile_extension(&temp, "capi_err_ext", ERR_SOURCE);
        let module = load_extension_module("capi_err_ext", &module_path)
            .unwrap_or_else(|message| panic!("failed to load err C extension: {message}"));
        assert!(!module.is_null(), "extension loader returned NULL module");

        let drive = module_attr(intern("capi_err_ext"), intern("drive")).expect("drive method registered");
        let result = unsafe { pon_call(drive, ptr::null_mut(), 0) };
        assert!(!result.is_null(), "drive() returned NULL: {:?}", pon_err_message());
        assert_eq!(
            format_object_for_print(result).as_deref(),
            Ok("131071"),
            "err C-API bitmask mismatch"
        );
    }

    const ERR_SOURCE: &str = r#"
#include <Python.h>
#include <errno.h>
#include <string.h>

static int check_pending_chain(PyObject *exc, PyObject *base1, PyObject *base2, PyObject *base3) {
    if (exc == NULL) {
        return 0;
    }
    PyErr_SetString(exc, "chain probe");
    int matches = PyErr_ExceptionMatches(exc);
    if (base1 != NULL) {
        matches = matches && PyErr_ExceptionMatches(base1);
    }
    if (base2 != NULL) {
        matches = matches && PyErr_ExceptionMatches(base2);
    }
    if (base3 != NULL) {
        matches = matches && PyErr_ExceptionMatches(base3);
    }
    PyErr_Clear();
    return matches;
}

static PyObject *drive(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    long ok = 0;

    if (PyErr_NoMemory() == NULL && PyErr_Occurred() == PyExc_MemoryError) {
        ok |= 1L << 0;
    }
    PyErr_Clear();

    errno = ENOENT;
    PyObject *set_errno_result = PyErr_SetFromErrno(PyExc_OSError);
    if (set_errno_result == NULL && PyErr_Occurred() == PyExc_OSError) {
        ok |= 1L << 1;
    }
    PyObject *errno_type = NULL;
    PyObject *errno_value = NULL;
    PyObject *errno_tb = NULL;
    PyErr_Fetch(&errno_type, &errno_value, &errno_tb);
    PyErr_NormalizeException(&errno_type, &errno_value, &errno_tb);
    if (errno_value != NULL && PyErr_GivenExceptionMatches(errno_value, PyExc_OSError)) {
        ok |= 1L << 2;
    }
    PyObject *errno_args = errno_value == NULL ? NULL : PyObject_GetAttrString(errno_value, "args");
    if (errno_args != NULL && PyTuple_Check(errno_args) && PyTuple_Size(errno_args) == 2) {
        long number = PyLong_AsLong(PyTuple_GetItem(errno_args, 0));
        if (PyErr_Occurred() == NULL && number == ENOENT) {
            ok |= 1L << 3;
        }
        PyObject *text_object = PyTuple_GetItem(errno_args, 1);
        const char *text = PyUnicode_AsUTF8(text_object);
        if (text != NULL && strcmp(text, strerror(ENOENT)) == 0) {
            ok |= 1L << 4;
        }
    }
    if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }

    PyObject *raw_type = PyExc_ValueError;
    PyObject *raw_value = PyUnicode_FromString("raw normalize");
    PyObject *raw_tb = NULL;
    PyErr_NormalizeException(&raw_type, &raw_value, &raw_tb);
    if (raw_value != NULL && PyErr_GivenExceptionMatches(raw_value, PyExc_ValueError)) {
        ok |= 1L << 5;
    }
    PyObject *normalized_once = raw_value;
    PyErr_NormalizeException(&raw_type, &raw_value, &raw_tb);
    if (raw_value == normalized_once) {
        ok |= 1L << 6;
    }
    if (raw_value != NULL) {
        PyBaseExceptionObject *base = (PyBaseExceptionObject *)raw_value;
        const char *message = base->message == NULL ? NULL : PyUnicode_AsUTF8(base->message);
        if (message != NULL && strcmp(message, "raw normalize") == 0) {
            ok |= 1L << 7;
        }
    }
    if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }

    PyErr_SetString(PyExc_ValueError, "tb source");
    PyObject *tb_type = NULL;
    PyObject *tb_value = NULL;
    PyObject *fetched_tb = NULL;
    PyErr_Fetch(&tb_type, &tb_value, &fetched_tb);
    PyObject *trace = tb_value == NULL ? NULL : ((PyBaseExceptionObject *)tb_value)->traceback;
    if (trace != NULL) {
        ok |= 1L << 8;
    }

    PyObject *cause_msg = PyUnicode_FromString("cause");
    PyObject *context_msg = PyUnicode_FromString("context");
    PyObject *outer_msg = PyUnicode_FromString("outer");
    PyObject *cause = cause_msg == NULL ? NULL : PyObject_CallOneArg(PyExc_ValueError, cause_msg);
    PyObject *context = context_msg == NULL ? NULL : PyObject_CallOneArg(PyExc_RuntimeError, context_msg);
    PyObject *outer = outer_msg == NULL ? NULL : PyObject_CallOneArg(PyExc_OSError, outer_msg);
    if (outer != NULL && cause != NULL) {
        PyException_SetCause(outer, cause);
        PyBaseExceptionObject *outer_base = (PyBaseExceptionObject *)outer;
        if (PyErr_Occurred() == NULL && outer_base->cause == cause && outer_base->suppress_context) {
            ok |= 1L << 9;
        }
    }
    if (outer != NULL && context != NULL) {
        PyException_SetContext(outer, context);
        PyBaseExceptionObject *outer_base = (PyBaseExceptionObject *)outer;
        if (PyErr_Occurred() == NULL && outer_base->context == context) {
            ok |= 1L << 10;
        }
    }
    if (outer != NULL && trace != NULL) {
        if (PyException_SetTraceback(outer, trace) == 0 && ((PyBaseExceptionObject *)outer)->traceback == trace) {
            ok |= 1L << 11;
        }
    }
    if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }

    PyObject *not_exception = PyLong_FromLong(7);
    if (PyException_SetTraceback(not_exception, NULL) == -1 && PyErr_ExceptionMatches(PyExc_TypeError)) {
        ok |= 1L << 12;
    }
    if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }


    /* CPython 3.14 oracle checked for these direct chains:
     * ModuleNotFoundError->ImportError, RecursionError->RuntimeError,
     * UnicodeEncodeError/UnicodeDecodeError->UnicodeError->ValueError,
     * FutureWarning/ImportWarning->Warning, NameError/AssertionError->Exception.
     */
    if (check_pending_chain(PyExc_Warning, PyExc_Exception, NULL, NULL) &&
            check_pending_chain(PyExc_FutureWarning, PyExc_Warning, PyExc_Exception, NULL) &&
            check_pending_chain(PyExc_ImportWarning, PyExc_Warning, PyExc_Exception, NULL) &&
            check_pending_chain(PyExc_ModuleNotFoundError, PyExc_ImportError, PyExc_Exception, NULL) &&
            check_pending_chain(PyExc_AssertionError, PyExc_Exception, NULL, NULL) &&
            check_pending_chain(PyExc_NameError, PyExc_Exception, NULL, NULL) &&
            check_pending_chain(PyExc_UnicodeError, PyExc_ValueError, PyExc_Exception, NULL) &&
            check_pending_chain(PyExc_UnicodeEncodeError, PyExc_UnicodeError, PyExc_ValueError, PyExc_Exception) &&
            check_pending_chain(PyExc_UnicodeDecodeError, PyExc_UnicodeError, PyExc_ValueError, PyExc_Exception) &&
            check_pending_chain(PyExc_RecursionError, PyExc_RuntimeError, PyExc_Exception, NULL)) {
        ok |= 1L << 13;
    }

    PyObject *warnings = PyImport_ImportModule("warnings");
    PyObject *filter_result = warnings == NULL ? NULL : PyObject_CallMethod(warnings, "simplefilter", "sO", "error", PyExc_UserWarning);
    if (warnings == NULL && PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }
    if (warnings == NULL || filter_result != NULL) {
        int warn_status = PyErr_WarnFormat(PyExc_UserWarning, 1, "warn %s %zd", "count", (Py_ssize_t)42);
        if (warn_status == -1 && PyErr_ExceptionMatches(PyExc_UserWarning)) {
            PyObject *warn_type = NULL;
            PyObject *warn_value = NULL;
            PyObject *warn_tb = NULL;
            PyErr_Fetch(&warn_type, &warn_value, &warn_tb);
            PyErr_NormalizeException(&warn_type, &warn_value, &warn_tb);
            if (warn_value != NULL) {
                PyBaseExceptionObject *warn_base = (PyBaseExceptionObject *)warn_value;
                const char *warn_text = warn_base->message == NULL ? NULL : PyUnicode_AsUTF8(warn_base->message);
                if (warn_text != NULL && strcmp(warn_text, "warn count 42") == 0) {
                    ok |= 1L << 14;
                }
            }
        }
    }
    if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }

    PyObject *custom_dict = PyDict_New();
    PyObject *marker = PyUnicode_FromString("copied");
    if (custom_dict != NULL && marker != NULL && PyDict_SetItemString(custom_dict, "marker", marker) == 0) {
        PyObject *custom = PyErr_NewException("capi_err_ext.CustomError", PyExc_ValueError, custom_dict);
        PyObject *marker_attr = custom == NULL ? NULL : PyObject_GetAttrString(custom, "marker");
        PyObject *module_attr = custom == NULL ? NULL : PyObject_GetAttrString(custom, "__module__");
        const char *marker_text = marker_attr == NULL ? NULL : PyUnicode_AsUTF8(marker_attr);
        const char *module_text = module_attr == NULL ? NULL : PyUnicode_AsUTF8(module_attr);
        PyErr_SetString(custom, "custom boom");
        if (marker_text != NULL && strcmp(marker_text, "copied") == 0 &&
                module_text != NULL && strcmp(module_text, "capi_err_ext") == 0 &&
                PyErr_ExceptionMatches(custom) && PyErr_ExceptionMatches(PyExc_ValueError)) {
            ok |= 1L << 15;
        }
    }
    if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }

    if (PyErr_CheckSignals() == 0 && PyErr_Occurred() == NULL) {
        ok |= 1L << 16;
    } else if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }
    return PyLong_FromLong(ok);
}

static PyMethodDef methods[] = {
    {"drive", drive, METH_NOARGS, NULL},
    {NULL, NULL, 0, NULL},
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_err_ext",
    NULL,
    -1,
    methods,
    NULL,
    NULL,
    NULL,
    NULL,
};

PyMODINIT_FUNC PyInit_capi_err_ext(void) {
    return PyModule_Create(&module);
}
"#;
}
