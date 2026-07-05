//! Object family: generic object protocol, calls, attributes, iteration, and type checks.

use core::ffi::{c_char, c_int};
use core::ptr;
use std::panic::{catch_unwind, AssertUnwindSafe};

use num_traits::cast::ToPrimitive;

use crate::abi;
use crate::object::{PyObject, PyType};
use crate::thread_state::{pon_err_clear, pon_err_occurred};

use super::c_string;
use super::twin::{self, ForeignTypeObject};

/// C mirror: `include/pon_capi/object.h` `PyPonCapiObject`.
#[repr(C)]
pub(crate) struct PyPonCapiObject {
    get_attr: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
    get_attr_string: unsafe extern "C" fn(*mut PyObject, *const c_char) -> *mut PyObject,
    set_attr: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int,
    set_attr_string: unsafe extern "C" fn(*mut PyObject, *const c_char, *mut PyObject) -> c_int,
    has_attr: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int,
    has_attr_string: unsafe extern "C" fn(*mut PyObject, *const c_char) -> c_int,
    call: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject,
    call_object: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
    call_no_args: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    call_one_arg: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
    call_varargs: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut *mut PyObject, usize) -> *mut PyObject,
    repr: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    str_: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    is_true: unsafe extern "C" fn(*mut PyObject) -> c_int,
    not_: unsafe extern "C" fn(*mut PyObject) -> c_int,
    rich_compare: unsafe extern "C" fn(*mut PyObject, *mut PyObject, c_int) -> *mut PyObject,
    rich_compare_bool: unsafe extern "C" fn(*mut PyObject, *mut PyObject, c_int) -> c_int,
    get_item: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
    set_item: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int,
    del_item: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int,
    get_iter: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    iter_next: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    size: unsafe extern "C" fn(*mut PyObject) -> isize,
    hash: unsafe extern "C" fn(*mut PyObject) -> isize,
    callable_check: unsafe extern "C" fn(*mut PyObject) -> c_int,
    is_instance: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int,
    is_subclass: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int,
    type_: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
    self_iter: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
}

unsafe impl Send for PyPonCapiObject {}
unsafe impl Sync for PyPonCapiObject {}

pub(crate) fn build() -> PyPonCapiObject {
    PyPonCapiObject {
        get_attr: capi_get_attr,
        get_attr_string: capi_get_attr_string,
        set_attr: capi_set_attr,
        set_attr_string: capi_set_attr_string,
        has_attr: capi_has_attr,
        has_attr_string: capi_has_attr_string,
        call: capi_call,
        call_object: capi_call_object,
        call_no_args: capi_call_no_args,
        call_one_arg: capi_call_one_arg,
        call_varargs: capi_call_varargs,
        repr: capi_repr,
        str_: capi_str,
        is_true: capi_is_true,
        not_: capi_not,
        rich_compare: capi_rich_compare,
        rich_compare_bool: capi_rich_compare_bool,
        get_item: capi_get_item,
        set_item: capi_set_item,
        del_item: capi_del_item,
        get_iter: capi_get_iter,
        iter_next: capi_iter_next,
        size: capi_size,
        hash: capi_hash,
        callable_check: capi_callable_check,
        is_instance: capi_is_instance,
        is_subclass: capi_is_subclass,
        type_: capi_type,
        self_iter: capi_self_iter,
    }
}

fn catch_object(f: impl FnOnce() -> *mut PyObject) -> *mut PyObject {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(_) => abi::return_null_with_error("object C-API helper panicked"),
    }
}

fn catch_status(f: impl FnOnce() -> c_int) -> c_int {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(_) => abi::return_minus_one_with_error("object C-API helper panicked"),
    }
}

fn catch_isize(f: impl FnOnce() -> isize) -> isize {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(_) => {
            let _ = abi::return_null_with_error("object C-API helper panicked");
            -1
        }
    }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    // SAFETY: The exception helper copies the message bytes before returning.
    unsafe { abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn type_error_status(message: &str) -> c_int {
    let _ = raise_type_error(message);
    -1
}

fn type_error_isize(message: &str) -> isize {
    let _ = raise_type_error(message);
    -1
}

fn normalize_object_arg(object: *mut PyObject) -> *mut PyObject {
    twin::registered_native_of_foreign(object.cast::<ForeignTypeObject>())
        .map_or(object, |native| native.cast::<PyObject>())
}

unsafe fn name_object_to_interned(name: *mut PyObject) -> Result<u32, *mut PyObject> {
    let name = normalize_object_arg(name);
    let name = crate::tag::untag_arg(name);
    let Some(text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        return Err(raise_type_error("attribute name must be string"));
    };
    Ok(crate::intern::intern(text))
}

fn name_string_to_interned(name: *const c_char) -> Result<u32, *mut PyObject> {
    let Some(text) = c_string(name) else {
        return Err(raise_type_error("attribute name must be string"));
    };
    Ok(crate::intern::intern(&text))
}

unsafe fn normalize_argv(argv: *mut *mut PyObject, argc: usize) -> Result<Vec<*mut PyObject>, *mut PyObject> {
    if argv.is_null() && argc != 0 {
        return Err(abi::return_null_with_error("argv pointer is NULL"));
    }
    let mut out = Vec::with_capacity(argc);
    for index in 0..argc {
        // SAFETY: The caller supplied an array with `argc` readable entries.
        let value = unsafe { *argv.add(index) };
        out.push(normalize_object_arg(value));
    }
    Ok(out)
}

fn argv_ptr(args: &mut [*mut PyObject]) -> *mut *mut PyObject {
    if args.is_empty() { ptr::null_mut() } else { args.as_mut_ptr() }
}

unsafe fn call_with_argv(callee: *mut PyObject, argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let callee = normalize_object_arg(callee);
    let mut args = match unsafe { normalize_argv(argv, argc) } {
        Ok(args) => args,
        Err(error) => return error,
    };
    // SAFETY: `args` lives for the duration of the call and its pointer is NULL only for zero args.
    unsafe { abi::pon_call(callee, argv_ptr(&mut args), args.len()) }
}

unsafe fn call_method_with_argv(
    object: *mut PyObject,
    name: *mut PyObject,
    argv: *mut *mut PyObject,
    argc: usize,
) -> *mut PyObject {
    let object = normalize_object_arg(object);
    let name = match unsafe { name_object_to_interned(name) } {
        Ok(name) => name,
        Err(error) => return error,
    };
    // SAFETY: Attribute dispatch tolerates a NULL feedback cell.
    let method = unsafe { abi::pon_get_attr(object, name, ptr::null_mut()) };
    if method.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `method` is a live callable returned by attribute lookup.
    unsafe { call_with_argv(method, argv, argc) }
}

unsafe fn positional_args_from_object(args: *mut PyObject) -> Result<Vec<*mut PyObject>, *mut PyObject> {
    if args.is_null() {
        return Ok(Vec::new());
    }
    let args = normalize_object_arg(args);
    let mut positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
        Ok(values) => values,
        Err(message) => return Err(raise_type_error(&message)),
    };
    for value in &mut positional {
        *value = normalize_object_arg(*value);
    }
    Ok(positional)
}

fn valid_rich_compare_op(op: c_int) -> bool {
    matches!(op as u8, abi::object::RICH_LT | abi::object::RICH_LE | abi::object::RICH_EQ | abi::object::RICH_NE | abi::object::RICH_GT | abi::object::RICH_GE)
        && (0..=5).contains(&op)
}

unsafe fn object_native_type(object: *mut PyObject) -> Result<*mut PyType, *mut PyObject> {
    let object = normalize_object_arg(object);
    if object.is_null() {
        return Err(abi::return_null_with_error("PyObject_Type received NULL"));
    }
    if crate::tag::is_small_int(object) {
        let ty = abi::runtime_long_type();
        return if ty.is_null() {
            Err(abi::return_null_with_error("runtime is not initialized"))
        } else {
            Ok(ty)
        };
    }
    if !crate::tag::is_heap(object) {
        return Err(abi::return_null_with_error("object pointer is not a heap object"));
    }
    // SAFETY: Heap-tagged, non-NULL objects carry a readable header.
    let ty = unsafe { (*object).ob_type }.cast_mut();
    if ty.is_null() {
        Err(abi::return_null_with_error("object has NULL type"))
    } else {
        Ok(ty)
    }
}

unsafe fn is_instance_impl(object: *mut PyObject, classinfo: *mut PyObject) -> c_int {
    let object = normalize_object_arg(object);
    let classinfo = normalize_object_arg(classinfo);
    if !classinfo.is_null() && crate::tag::is_heap(classinfo) {
        // SAFETY: Heap-tagged runtime tuples expose stable element storage.
        if let Some(entries) = unsafe { abi::seq::exact_tuple_slice(classinfo) } {
            for entry in entries.iter().copied() {
                let result = unsafe { is_instance_impl(object, entry) };
                if result != 0 {
                    return result;
                }
            }
            return 0;
        }
    }
    // SAFETY: `classinfo` has been translated when it is a registered foreign type twin.
    unsafe { abi::attr::pon_isinstance(object, classinfo) }
}

unsafe fn is_subclass_impl(cls: *mut PyObject, classinfo: *mut PyObject) -> c_int {
    let cls = normalize_object_arg(cls);
    let classinfo = normalize_object_arg(classinfo);
    if !classinfo.is_null() && crate::tag::is_heap(classinfo) {
        // SAFETY: Heap-tagged runtime tuples expose stable element storage.
        if let Some(entries) = unsafe { abi::seq::exact_tuple_slice(classinfo) } {
            for entry in entries.iter().copied() {
                let result = unsafe { is_subclass_impl(cls, entry) };
                if result != 0 {
                    return result;
                }
            }
            return 0;
        }
    }
    // SAFETY: `cls`/`classinfo` have been translated when they are registered foreign type twins.
    unsafe { abi::attr::pon_issubclass(cls, classinfo) }
}

/// Attribute lookup shared by the C getters and the `hasattr` probes: no
/// pin, so probe-style callers do not accumulate owned references.
unsafe fn get_attr_unpinned(object: *mut PyObject, name: u32) -> *mut PyObject {
    let object = normalize_object_arg(object);
    // SAFETY: Attribute dispatch tolerates a NULL feedback cell.
    unsafe { abi::pon_get_attr(object, name, ptr::null_mut()) }
}

unsafe extern "C" fn capi_get_attr(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    catch_object(|| {
        let name = match unsafe { name_object_to_interned(name) } {
            Ok(name) => name,
            Err(error) => return error,
        };
        let value = unsafe { get_attr_unpinned(object, name) };
        // C owned-reference contract: the result must outlive its source
        // (e.g. an instance-held object member) and the caller releases it
        // with Py_DECREF; pin to balance that release.
        super::pin_object(value);
        value
    })
}

unsafe extern "C" fn capi_get_attr_string(object: *mut PyObject, name: *const c_char) -> *mut PyObject {
    catch_object(|| {
        let name = match name_string_to_interned(name) {
            Ok(name) => name,
            Err(error) => return error,
        };
        let value = unsafe { get_attr_unpinned(object, name) };
        // Same owned-reference contract as `capi_get_attr`.
        super::pin_object(value);
        value
    })
}

unsafe extern "C" fn capi_set_attr(object: *mut PyObject, name: *mut PyObject, value: *mut PyObject) -> c_int {
    catch_status(|| {
        let object = normalize_object_arg(object);
        let name = match unsafe { name_object_to_interned(name) } {
            Ok(name) => name,
            Err(_) => return -1,
        };
        let value = normalize_object_arg(value);
        if value.is_null() {
            // SAFETY: Attribute deletion dispatch tolerates a live receiver and interned name.
            unsafe { abi::pon_del_attr(object, name) }
        } else {
            // SAFETY: Attribute assignment dispatch tolerates a live receiver/value and interned name.
            unsafe { abi::pon_set_attr(object, name, value) }
        }
    })
}

unsafe extern "C" fn capi_set_attr_string(object: *mut PyObject, name: *const c_char, value: *mut PyObject) -> c_int {
    catch_status(|| {
        let object = normalize_object_arg(object);
        let name = match name_string_to_interned(name) {
            Ok(name) => name,
            Err(_) => return -1,
        };
        let value = normalize_object_arg(value);
        if value.is_null() {
            // SAFETY: Attribute deletion dispatch tolerates a live receiver and interned name.
            unsafe { abi::pon_del_attr(object, name) }
        } else {
            // SAFETY: Attribute assignment dispatch tolerates a live receiver/value and interned name.
            unsafe { abi::pon_set_attr(object, name, value) }
        }
    })
}

unsafe extern "C" fn capi_has_attr(object: *mut PyObject, name: *mut PyObject) -> c_int {
    catch_status(|| {
        let name = match unsafe { name_object_to_interned(name) } {
            Ok(name) => name,
            Err(_) => {
                pon_err_clear();
                return 0;
            }
        };
        // Probe only (PyObject_HasAttr returns no reference): the unpinned
        // lookup keeps successful probes from leaking owned references.
        let value = unsafe { get_attr_unpinned(object, name) };
        if value.is_null() {
            pon_err_clear();
            0
        } else {
            1
        }
    })
}

unsafe extern "C" fn capi_has_attr_string(object: *mut PyObject, name: *const c_char) -> c_int {
    catch_status(|| {
        let name = match name_string_to_interned(name) {
            Ok(name) => name,
            Err(_) => {
                pon_err_clear();
                return 0;
            }
        };
        // Probe only, mirroring `capi_has_attr`.
        let value = unsafe { get_attr_unpinned(object, name) };
        if value.is_null() {
            pon_err_clear();
            0
        } else {
            1
        }
    })
}

unsafe extern "C" fn capi_call(callee: *mut PyObject, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    catch_object(|| {
        let callee = normalize_object_arg(callee);
        let mut positional = match unsafe { positional_args_from_object(args) } {
            Ok(values) => values,
            Err(error) => return error,
        };
        let kwargs = normalize_object_arg(kwargs);
        if kwargs.is_null() {
            // SAFETY: `positional` lives for the duration of the call.
            unsafe { abi::pon_call(callee, argv_ptr(&mut positional), positional.len()) }
        } else {
            // SAFETY: `positional` lives for the duration of the call; kwargs is delegated as `**kwargs`.
            unsafe {
                abi::call::pon_call_ex(
                    callee,
                    argv_ptr(&mut positional),
                    positional.len(),
                    ptr::null_mut(),
                    ptr::null(),
                    ptr::null_mut(),
                    0,
                    kwargs,
                    ptr::null_mut(),
                )
            }
        }
    })
}

unsafe extern "C" fn capi_call_object(callee: *mut PyObject, args: *mut PyObject) -> *mut PyObject {
    // SAFETY: Same contract as PyObject_Call with NULL kwargs.
    unsafe { capi_call(callee, args, ptr::null_mut()) }
}

unsafe extern "C" fn capi_call_no_args(callee: *mut PyObject) -> *mut PyObject {
    catch_object(|| {
        // SAFETY: NULL argv with zero argc denotes no positional arguments.
        unsafe { call_with_argv(callee, ptr::null_mut(), 0) }
    })
}

unsafe extern "C" fn capi_call_one_arg(callee: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    catch_object(|| {
        let mut argv = [arg];
        // SAFETY: `argv` has one live slot for the duration of the call.
        unsafe { call_with_argv(callee, argv.as_mut_ptr(), 1) }
    })
}

unsafe extern "C" fn capi_call_varargs(
    target: *mut PyObject,
    name: *mut PyObject,
    argv: *mut *mut PyObject,
    argc: usize,
) -> *mut PyObject {
    catch_object(|| {
        if name.is_null() {
            // SAFETY: The inline C wrapper supplied `argv`/`argc` from a bounded local array.
            unsafe { call_with_argv(target, argv, argc) }
        } else {
            // SAFETY: The inline C wrapper supplied `argv`/`argc` from a bounded local array.
            unsafe { call_method_with_argv(target, name, argv, argc) }
        }
    })
}

unsafe extern "C" fn capi_repr(object: *mut PyObject) -> *mut PyObject {
    catch_object(|| {
        let object = normalize_object_arg(object);
        let text = match crate::native::builtins_mod::try_repr_text(object) {
            Ok(text) => text,
            Err(()) => return ptr::null_mut(),
        };
        // SAFETY: The string helper copies the UTF-8 bytes into a runtime str.
        unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
    })
}

unsafe extern "C" fn capi_str(object: *mut PyObject) -> *mut PyObject {
    catch_object(|| {
        let object = normalize_object_arg(object);
        let text = match crate::native::builtins_mod::try_str_text(object) {
            Ok(text) => text,
            Err(()) => return ptr::null_mut(),
        };
        // SAFETY: The string helper copies the UTF-8 bytes into a runtime str.
        unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
    })
}

unsafe extern "C" fn capi_is_true(object: *mut PyObject) -> c_int {
    let object = normalize_object_arg(object);
    // SAFETY: Delegates truth dispatch to the runtime helper.
    unsafe { abi::pon_is_true(object) }
}

unsafe extern "C" fn capi_not(object: *mut PyObject) -> c_int {
    let truth = unsafe { capi_is_true(object) };
    if truth < 0 { -1 } else { c_int::from(truth == 0) }
}

unsafe extern "C" fn capi_rich_compare(left: *mut PyObject, right: *mut PyObject, op: c_int) -> *mut PyObject {
    catch_object(|| {
        if !valid_rich_compare_op(op) {
            return raise_type_error("unknown rich comparison operation");
        }
        let left = normalize_object_arg(left);
        let right = normalize_object_arg(right);
        // SAFETY: Rich comparison dispatch tolerates a NULL feedback cell.
        unsafe { abi::pon_rich_compare(op as u8, left, right, ptr::null_mut()) }
    })
}

unsafe extern "C" fn capi_rich_compare_bool(left: *mut PyObject, right: *mut PyObject, op: c_int) -> c_int {
    catch_status(|| {
        if !valid_rich_compare_op(op) {
            return type_error_status("unknown rich comparison operation");
        }
        let left = normalize_object_arg(left);
        let right = normalize_object_arg(right);
        if left == right {
            if op as u8 == abi::object::RICH_EQ {
                return 1;
            }
            if op as u8 == abi::object::RICH_NE {
                return 0;
            }
        }
        // SAFETY: Delegates to the object result form, then coerces through truth testing.
        let result = unsafe { capi_rich_compare(left, right, op) };
        if result.is_null() {
            return -1;
        }
        // SAFETY: Truth dispatch handles arbitrary Python result objects.
        unsafe { abi::pon_is_true(result) }
    })
}

unsafe extern "C" fn capi_get_item(object: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    catch_object(|| {
        let object = normalize_object_arg(object);
        let key = normalize_object_arg(key);
        // SAFETY: Subscription dispatch tolerates a NULL feedback cell.
        unsafe { abi::pon_subscript_get(object, key, ptr::null_mut()) }
    })
}

unsafe extern "C" fn capi_set_item(object: *mut PyObject, key: *mut PyObject, value: *mut PyObject) -> c_int {
    catch_status(|| {
        let object = normalize_object_arg(object);
        let key = normalize_object_arg(key);
        let value = normalize_object_arg(value);
        // SAFETY: Runtime helper implements mapping/sequence assignment and returns NULL on failure.
        let result = unsafe { abi::map::pon_subscript_set(object, key, value) };
        if result.is_null() { -1 } else { 0 }
    })
}

unsafe extern "C" fn capi_del_item(object: *mut PyObject, key: *mut PyObject) -> c_int {
    catch_status(|| {
        let object = normalize_object_arg(object);
        let key = normalize_object_arg(key);
        // SAFETY: Runtime helper implements mapping/sequence deletion and returns NULL on failure.
        let result = unsafe { abi::map::pon_subscript_del(object, key) };
        if result.is_null() { -1 } else { 0 }
    })
}

unsafe extern "C" fn capi_get_iter(object: *mut PyObject) -> *mut PyObject {
    catch_object(|| {
        let object = normalize_object_arg(object);
        // SAFETY: Iterator dispatch tolerates a NULL feedback cell.
        unsafe { abi::pon_get_iter(object, ptr::null_mut()) }
    })
}

unsafe extern "C" fn capi_iter_next(iterator: *mut PyObject) -> *mut PyObject {
    catch_object(|| {
        let iterator = normalize_object_arg(iterator);
        // SAFETY: Iterator dispatch tolerates a NULL feedback cell.
        let result = unsafe { abi::pon_iter_next(iterator, ptr::null_mut()) };
        if result.is_null() && abi::exc::pending_exception_is("StopIteration") {
            pon_err_clear();
        }
        result
    })
}

unsafe extern "C" fn capi_size(object: *mut PyObject) -> isize {
    catch_isize(|| {
        let object = normalize_object_arg(object);
        // SAFETY: Runtime length helper returns a boxed integer or NULL with an error set.
        let result = unsafe { abi::seq::pon_get_len(object, ptr::null_mut()) };
        if result.is_null() {
            return -1;
        }
        let Some(length) = (unsafe { crate::types::int::to_bigint_including_bool(result) }) else {
            return type_error_isize("__len__() should return an integer");
        };
        length
            .to_isize()
            .unwrap_or_else(|| type_error_isize("__len__() result is too large"))
    })
}

unsafe extern "C" fn capi_hash(object: *mut PyObject) -> isize {
    catch_isize(|| {
        let object = normalize_object_arg(object);
        match unsafe { crate::types::dict::hash_object(object) } {
            Ok(hash) => hash,
            Err(message) => {
                if !pon_err_occurred() {
                    let _ = raise_type_error(&message);
                }
                -1
            }
        }
    })
}

unsafe extern "C" fn capi_callable_check(object: *mut PyObject) -> c_int {
    let object = normalize_object_arg(object);
    c_int::from(abi::call::is_callable_object(object))
}

unsafe extern "C" fn capi_is_instance(object: *mut PyObject, classinfo: *mut PyObject) -> c_int {
    catch_status(|| unsafe { is_instance_impl(object, classinfo) })
}

unsafe extern "C" fn capi_is_subclass(cls: *mut PyObject, classinfo: *mut PyObject) -> c_int {
    catch_status(|| unsafe { is_subclass_impl(cls, classinfo) })
}

unsafe extern "C" fn capi_type(object: *mut PyObject) -> *mut PyObject {
    catch_object(|| {
        let ty = match unsafe { object_native_type(object) } {
            Ok(ty) => ty,
            Err(error) => return error,
        };
        twin::foreign_of_native(ty).cast::<PyObject>()
    })
}

unsafe extern "C" fn capi_self_iter(object: *mut PyObject) -> *mut PyObject {
    object
}

#[cfg(test)]
mod tests {
    use core::ptr;

    use super::super::tests::{compile_extension, ResetImportStateOnDrop, TempExtensionRoot};
    use crate::abi;
    use crate::abi::{exc, format_object_for_print as format_object, pon_call, pon_const_int, pon_runtime_init};
    use crate::import::module_attr;
    use crate::intern::intern;
    use crate::object::PyObject;
    use crate::thread_state::{pon_err_clear, pon_err_message, test_state_lock};

    #[test]
    fn object_family_extension_exercises_protocol_surface() {
        let _guard = test_state_lock();
        let _reset = ResetImportStateOnDrop;
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
        }
        let temp = TempExtensionRoot::new();
        let module_path = compile_extension(
            &temp,
            "capi_object_test_ext",
            r#"
#include <Python.h>

static PyObject *one_arg_bit_length(PyObject *self, PyObject *arg) {
    (void)self;
    PyObject *builtins = PyObject_GetAttrString(arg, "__class__");
    if (builtins == NULL) {
        PyErr_Clear();
    }
    PyObject *method_name = PyUnicode_FromString("bit_length");
    if (method_name == NULL) {
        return NULL;
    }
    PyObject *method = PyObject_GetAttr(arg, method_name);
    if (method == NULL) {
        return NULL;
    }
    return PyObject_CallNoArgs(method);
}

static PyObject *call_one_arg(PyObject *self, PyObject *callable) {
    (void)self;
    PyObject *value = PyLong_FromLong(-7);
    if (value == NULL) {
        return NULL;
    }
    return PyObject_CallOneArg(callable, value);
}

static PyObject *varargs_calls(PyObject *self, PyObject *callable) {
    (void)self;
    PyObject *value = PyLong_FromLong(-11);
    if (value == NULL) {
        return NULL;
    }
    PyObject *called = PyObject_CallFunctionObjArgs(callable, value, NULL);
    if (called == NULL) {
        return NULL;
    }
    long first = PyLong_AsLong(called);
    if (PyErr_Occurred()) {
        return NULL;
    }
    PyObject *method_name = PyUnicode_FromString("bit_length");
    PyObject *receiver = PyLong_FromLong(15);
    if (method_name == NULL || receiver == NULL) {
        return NULL;
    }
    PyObject *method_result = PyObject_CallMethodObjArgs(receiver, method_name, NULL);
    if (method_result == NULL) {
        return NULL;
    }
    long second = PyLong_AsLong(method_result);
    if (PyErr_Occurred()) {
        return NULL;
    }
    return PyLong_FromLong(first + second);
}

static PyObject *module_attrs(PyObject *self, PyObject *module_obj) {
    (void)self;
    PyObject *value = PyLong_FromLong(123);
    if (value == NULL) {
        return NULL;
    }
    if (PyObject_SetAttrString(module_obj, "dynamic_value", value) < 0) {
        return NULL;
    }
    if (!PyObject_HasAttrString(module_obj, "dynamic_value")) {
        PyErr_SetString(PyExc_RuntimeError, "attribute was not set");
        return NULL;
    }
    PyObject *name = PyUnicode_FromString("dynamic_value");
    if (name == NULL) {
        return NULL;
    }
    PyObject *read_back = PyObject_GetAttr(module_obj, name);
    if (read_back == NULL) {
        return NULL;
    }
    if (PyObject_SetAttr(module_obj, name, NULL) < 0) {
        return NULL;
    }
    if (PyObject_HasAttr(module_obj, name)) {
        PyErr_SetString(PyExc_RuntimeError, "attribute was not deleted");
        return NULL;
    }
    return read_back;
}

static PyObject *compare_ints(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    PyObject *left = PyLong_FromLong(3);
    PyObject *right = PyLong_FromLong(5);
    if (left == NULL || right == NULL) {
        return NULL;
    }
    int lt = PyObject_RichCompareBool(left, right, Py_LT);
    int ge = PyObject_RichCompareBool(left, right, Py_GE);
    if (lt < 0 || ge < 0) {
        return NULL;
    }
    return PyLong_FromLong((lt == 1 && ge == 0) ? 1 : 0);
}

static PyObject *iterate_and_sum(PyObject *self, PyObject *iterable) {
    (void)self;
    PyObject *iter = PyObject_GetIter(iterable);
    if (iter == NULL) {
        return NULL;
    }
    long total = 0;
    PyObject *item;
    while ((item = PyIter_Next(iter)) != NULL) {
        total += PyLong_AsLong(item);
        if (PyErr_Occurred()) {
            return NULL;
        }
    }
    if (PyErr_Occurred()) {
        return NULL;
    }
    return PyLong_FromLong(total);
}

static PyObject *is_value_error(PyObject *self, PyObject *obj) {
    (void)self;
    int result = PyObject_IsInstance(obj, PyExc_ValueError);
    if (result < 0) {
        return NULL;
    }
    return PyLong_FromLong(result);
}

static PyObject *type_is_value_error(PyObject *self, PyObject *obj) {
    (void)self;
    PyObject *ty = PyObject_Type(obj);
    if (ty == NULL) {
        return NULL;
    }
    int result = PyObject_IsSubclass(ty, PyExc_ValueError);
    if (result < 0) {
        return NULL;
    }
    return PyLong_FromLong(result);
}

static PyObject *repr_and_str_truth(PyObject *self, PyObject *obj) {
    (void)self;
    PyObject *repr_obj = PyObject_Repr(obj);
    PyObject *str_obj = PyObject_Str(obj);
    if (repr_obj == NULL || str_obj == NULL) {
        return NULL;
    }
    int truth = PyObject_IsTrue(obj);
    int not_value = PyObject_Not(obj);
    if (truth < 0 || not_value < 0) {
        return NULL;
    }
    return PyLong_FromLong((truth == 1 && not_value == 0) ? 1 : 0);
}

static PyMethodDef methods[] = {
    {"call_one_arg", call_one_arg, METH_O, "call callable with -7"},
    {"varargs_calls", varargs_calls, METH_O, "call varargs object helpers"},
    {"one_arg_bit_length", one_arg_bit_length, METH_O, "call int.bit_length with no args"},
    {"module_attrs", module_attrs, METH_O, "exercise attrs"},
    {"compare_ints", compare_ints, METH_NOARGS, "rich compare ints"},
    {"iterate_and_sum", iterate_and_sum, METH_O, "iterate and sum"},
    {"is_value_error", is_value_error, METH_O, "isinstance against ValueError twin"},
    {"type_is_value_error", type_is_value_error, METH_O, "issubclass against ValueError twin"},
    {"repr_and_str_truth", repr_and_str_truth, METH_O, "repr/str/truth"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_object_test_ext",
    "Pon object C-API test extension",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_object_test_ext(void) {
    PyObject *m = PyModule_Create(&module);
    if (m == NULL) {
        return NULL;
    }
    return m;
}
"#,
        );

        let module = super::super::load_extension_module("capi_object_test_ext", &module_path)
            .unwrap_or_else(|message| panic!("failed to load object C extension: {message}"));
        assert!(!module.is_null(), "extension loader returned NULL module");

        let module_name = intern("capi_object_test_ext");
        let module_attrs = module_attr(module_name, intern("module_attrs")).expect("module_attrs method registered");
        let mut argv = [module];
        let result = unsafe { pon_call(module_attrs, argv.as_mut_ptr(), 1) };
        assert_eq!(format_object(result).as_deref(), Ok("123"));

        let compare = module_attr(module_name, intern("compare_ints")).expect("compare_ints method registered");
        let result = unsafe { pon_call(compare, ptr::null_mut(), 0) };
        assert_eq!(format_object(result).as_deref(), Ok("1"));

        let iterate = module_attr(module_name, intern("iterate_and_sum")).expect("iterate_and_sum method registered");
        let mut list_items = [unsafe { pon_const_int(2) }, unsafe { pon_const_int(4) }, unsafe { pon_const_int(6) }];
        let list = unsafe { abi::seq::pon_build_list(list_items.as_mut_ptr(), list_items.len()) };
        assert!(!list.is_null(), "failed to build runtime list: {:?}", pon_err_message());
        let mut argv = [list];
        let result = unsafe { pon_call(iterate, argv.as_mut_ptr(), 1) };
        assert_eq!(format_object(result).as_deref(), Ok("12"));

        let call_one_arg = module_attr(module_name, intern("call_one_arg")).expect("call_one_arg method registered");
        let abs_builtin = unsafe { abi::pon_load_builtin(intern("abs")) };
        assert!(!abs_builtin.is_null(), "failed to load abs builtin: {:?}", pon_err_message());
        let mut argv = [abs_builtin];
        let result = unsafe { pon_call(call_one_arg, argv.as_mut_ptr(), 1) };
        assert_eq!(format_object(result).as_deref(), Ok("7"));

        let varargs_calls = module_attr(module_name, intern("varargs_calls")).expect("varargs_calls method registered");
        let mut argv = [abs_builtin];
        let result = unsafe { pon_call(varargs_calls, argv.as_mut_ptr(), 1) };
        assert_eq!(format_object(result).as_deref(), Ok("15"));

        let one_arg_bit_length = module_attr(module_name, intern("one_arg_bit_length")).expect("one_arg_bit_length method registered");
        let negative = unsafe { pon_const_int(-8) };
        let mut argv = [negative];
        let result = unsafe { pon_call(one_arg_bit_length, argv.as_mut_ptr(), 1) };
        assert_eq!(format_object(result).as_deref(), Ok("4"));

        let value_error_type = abi::exception_type_object(crate::types::exc::ExceptionKind::ValueError).cast::<PyObject>();
        let message = unsafe { abi::pon_const_str(b"raised instance".as_ptr(), b"raised instance".len()) };
        let mut exc_argv = [message];
        let instance = unsafe { pon_call(value_error_type, exc_argv.as_mut_ptr(), exc_argv.len()) };
        assert!(!instance.is_null(), "failed to construct ValueError instance: {:?}", pon_err_message());
        let _ = unsafe { exc::pon_raise(instance, ptr::null_mut()) };
        assert!(exc::pending_exception_is("ValueError"), "raised instance should be pending");
        pon_err_clear();

        let is_value_error = module_attr(module_name, intern("is_value_error")).expect("is_value_error method registered");
        let mut argv = [instance];
        let result = unsafe { pon_call(is_value_error, argv.as_mut_ptr(), 1) };
        assert_eq!(format_object(result).as_deref(), Ok("1"));

        let type_is_value_error = module_attr(module_name, intern("type_is_value_error")).expect("type_is_value_error method registered");
        let mut argv = [instance];
        let result = unsafe { pon_call(type_is_value_error, argv.as_mut_ptr(), 1) };
        assert_eq!(format_object(result).as_deref(), Ok("1"));

        let repr_truth = module_attr(module_name, intern("repr_and_str_truth")).expect("repr_and_str_truth method registered");
        let value = unsafe { pon_const_int(9) };
        let mut argv = [value];
        let result = unsafe { pon_call(repr_truth, argv.as_mut_ptr(), 1) };
        assert_eq!(format_object(result).as_deref(), Ok("1"));

    }
}
