//! Generator, coroutine, and async-generator helper family namespace.
//!
//! These helpers implement the Phase-B stackless generator contract: heap frames
//! carry the state index and locals, resume functions are ordinary `extern "C"`
//! calls, and every fallible helper reports errors through the runtime
//! NULL-sentinel thread-state path.

use core::{cell::Cell, ptr};

use pon_gc::GcTypeInfo;
use std::cell::RefCell;

use crate::abstract_op;
use crate::feedback::FeedbackCell;
use crate::object::{PyObject, PyType, is_exact_type};
use crate::thread_state::{pon_err_clear, pon_err_occurred, thread_state_lock};
use crate::types::coroutine::{TYPE_ID_COROUTINE, ensure_coroutine_type};
use crate::types::exc::{PyBaseException, is_exception_instance};
use crate::types::frame::{FRAME_STATE_EXHAUSTED, TYPE_ID_FRAME, alloc_frame_locals, ensure_frame_type, finalize_frame, trace_frame};
use crate::types::generator::{GeneratorKind, PyGenerator, TYPE_ID_GENERATOR, as_generator_mut, as_generator_object, ensure_generator_type, trace_generator};

/// Compiled generator/coroutine resume function ABI.
pub type GenResumeFn = unsafe extern "C" fn(frame: *mut crate::abi::PyFrame, sent: *mut PyObject) -> *mut PyObject;
thread_local! {
    static EAGER_YIELDS: RefCell<Vec<*mut PyObject>> = RefCell::new(Vec::new());
    static RESUME_DEPTH: Cell<usize> = const { Cell::new(0) };
}


#[derive(Clone, Copy)]
struct GenTypes {
    frame: *mut PyType,
    generator: *mut PyType,
    coroutine: *mut PyType,
}

fn ensure_gen_runtime() -> Result<GenTypes, String> {
    super::ensure_runtime_initialized()?;
    super::with_runtime(|runtime| {
        runtime.heap.register_type(
            TYPE_ID_GENERATOR,
            GcTypeInfo {
                size: core::mem::size_of::<PyGenerator>(),
                trace: trace_generator,
                finalize: None,
            },
        );
        runtime.heap.register_type(
            TYPE_ID_COROUTINE,
            GcTypeInfo {
                size: core::mem::size_of::<PyGenerator>(),
                trace: trace_generator,
                finalize: None,
            },
        );
        runtime.heap.register_type(
            TYPE_ID_FRAME,
            GcTypeInfo {
                size: core::mem::size_of::<crate::abi::PyFrame>(),
                trace: trace_frame,
                finalize: Some(finalize_frame),
            },
        );

        GenTypes {
            frame: ensure_frame_type(runtime._type_type),
            generator: ensure_generator_type(runtime._type_type),
            coroutine: ensure_coroutine_type(runtime._type_type),
        }
    })
    .ok_or_else(|| "runtime is not initialized".to_owned())
}

fn kind_type(types: GenTypes, kind: GeneratorKind) -> *mut PyType {
    match kind {
        GeneratorKind::Generator | GeneratorKind::AsyncGenerator => types.generator,
        GeneratorKind::Coroutine => types.coroutine,
    }
}

unsafe fn generator_kind_for(runtime_generator: *mut PyObject, types: GenTypes) -> Option<GeneratorKind> {
    if runtime_generator.is_null() {
        return None;
    }
    // SAFETY: Caller promises non-NULL object pointer; exact-type checks only read the header.
    if unsafe { is_exact_type(runtime_generator, types.generator.cast_const()) || is_exact_type(runtime_generator, types.coroutine.cast_const()) } {
        // SAFETY: Exact type checks prove this object uses the PyGenerator layout.
        return GeneratorKind::from_u8(unsafe { (*runtime_generator.cast::<PyGenerator>()).kind });
    }
    None
}

unsafe fn expect_generator(object: *mut PyObject) -> Result<(*mut PyGenerator, GenTypes), String> {
    let types = ensure_gen_runtime()?;
    // SAFETY: `generator_kind_for` only reads object headers/layout after exact-type checks.
    if unsafe { generator_kind_for(object, types) }.is_none() {
        return Err("object is not a generator or coroutine".to_owned());
    }
    // SAFETY: The exact-type check above proves the layout.
    Ok((unsafe { as_generator_mut(object) }, types))
}

fn raise_type_error(message: &str) -> *mut PyObject {
    // SAFETY: The exception helper copies the byte message under the runtime error discipline.
    unsafe { super::exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

fn current_stop_iteration_value() -> Option<*mut PyObject> {
    let current = thread_state_lock().current_exc;
    if current.is_null() {
        return None;
    }

    let is_stop = super::with_runtime(|runtime| {
        // SAFETY: `current` is the thread state's active exception pointer.
        unsafe { is_exception_instance(current, runtime.exception_types.stop_iteration.cast_const()) }
    })
    .unwrap_or(false);

    if !is_stop {
        return None;
    }

    // SAFETY: The type check above proves BaseException-compatible layout.
    Some(unsafe { (*current.cast::<PyBaseException>()).message })
}

fn stop_iteration_value_and_clear() -> Option<*mut PyObject> {
    let value = current_stop_iteration_value()?;
    pon_err_clear();
    Some(value)
}

fn recording_eager_yields() -> bool {
    RESUME_DEPTH.with(|depth| depth.get() == 0)
}

fn record_eager_yield(value: *mut PyObject) {
    EAGER_YIELDS.with(|pending| pending.borrow_mut().push(value));
}

pub(crate) fn has_eager_yields() -> bool {
    EAGER_YIELDS.with(|pending| !pending.borrow().is_empty())
}

unsafe extern "C" fn eager_yields_resume(frame: *mut crate::abi::PyFrame, sent: *mut PyObject) -> *mut PyObject {
    let _ = sent;
    if frame.is_null() {
        return super::return_null_with_error("generator frame pointer is null");
    }

    // SAFETY: The generator owns this heap frame for the lifetime of iteration.
    let _guard = crate::sync::begin_critical_section(frame.cast::<PyObject>());
    let frame = unsafe { &mut *frame };
    if frame.state == FRAME_STATE_EXHAUSTED {
        // SAFETY: `pon_none` and `pon_raise_stop_iteration` follow the NULL-sentinel ABI.
        let none = unsafe { super::pon_none() };
        return unsafe { super::exc::pon_raise_stop_iteration(none) };
    }

    let index = frame.state as usize;
    let len = frame.n_locals as usize;
    if index < len {
        if frame.locals.is_null() {
            return super::return_null_with_error("generator frame locals pointer is null");
        }
        frame.state = frame.state.saturating_add(1);
        // SAFETY: `index < n_locals` proves the slot is in bounds.
        let value = unsafe { *frame.locals.add(index) };
        if value.is_null() {
            // SAFETY: `pon_none` returns the initialized immortal singleton.
            unsafe { super::pon_none() }
        } else {
            value
        }
    } else {
        frame.mark_exhausted();
        // SAFETY: `pon_none` and `pon_raise_stop_iteration` follow the NULL-sentinel ABI.
        let none = unsafe { super::pon_none() };
        unsafe { super::exc::pon_raise_stop_iteration(none) }
    }
}

pub(crate) unsafe fn take_eager_yield_generator() -> *mut PyObject {
    let values = EAGER_YIELDS.with(|pending| core::mem::take(&mut *pending.borrow_mut()));
    if values.is_empty() {
        return ptr::null_mut();
    }
    let Ok(n_locals) = u32::try_from(values.len()) else {
        return super::return_null_with_error("generator yielded too many values");
    };
    // SAFETY: `pon_make_frame` and slot writes follow the generator ABI.
    let frame = unsafe { pon_make_frame(n_locals) };
    if frame.is_null() {
        return ptr::null_mut();
    }
    for (index, value) in values.into_iter().enumerate() {
        // SAFETY: The frame was allocated with exactly `n_locals` slots.
        if unsafe { pon_frame_set_local(frame, index as u32, value) }.is_null() {
            return ptr::null_mut();
        }
    }
    // SAFETY: `eager_yields_resume` follows `GenResumeFn`; `frame` is live.
    unsafe { pon_make_generator(eager_yields_resume, frame, GeneratorKind::Generator.as_u8()) }
}

/// Allocates a heap frame with `n_locals` local/temp slots.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_make_frame(n_locals: u32) -> *mut crate::abi::PyFrame {
    super::catch_object_helper(|| {
        let types = match ensure_gen_runtime() {
            Ok(types) => types,
            Err(message) => return super::return_null_with_error(message),
        };
        let locals = match alloc_frame_locals(n_locals) {
            Ok(locals) => locals,
            Err(message) => return super::return_null_with_error(message),
        };
        match super::with_runtime(|runtime| {
            let frame = runtime.heap.alloc(core::mem::size_of::<crate::abi::PyFrame>(), TYPE_ID_FRAME).cast::<crate::abi::PyFrame>();
            // SAFETY: `frame` points to a freshly allocated zeroed block of the right size.
            unsafe {
                ptr::write(frame, crate::abi::PyFrame::new(types.frame.cast_const(), n_locals, locals));
            }
            frame
        }) {
            Some(frame) => frame.cast::<PyObject>(),
            None => super::return_null_with_error("runtime is not initialized"),
        }
    })
    .cast::<crate::abi::PyFrame>()
}

/// Reads one heap-frame local slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_frame_get_local(frame: *mut crate::abi::PyFrame, index: u32) -> *mut PyObject {
    super::catch_object_helper(|| {
        if frame.is_null() {
            return super::return_null_with_error("frame pointer is null");
        }
        // SAFETY: Caller supplies a live frame pointer.
        let frame_ref = unsafe { &*frame };
        if index >= frame_ref.n_locals {
            return super::return_null_with_error("frame local index out of range");
        }
        if frame_ref.locals.is_null() {
            return super::return_null_with_error("frame has no local storage");
        }
        // SAFETY: Bounds checked above.
        unsafe { *frame_ref.locals.add(index as usize) }
    })
}

/// Stores one heap-frame local slot and returns the stored value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_frame_set_local(frame: *mut crate::abi::PyFrame, index: u32, value: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        if frame.is_null() {
            return super::return_null_with_error("frame pointer is null");
        }
        if value.is_null() {
            return super::return_null_with_error("cannot store NULL in frame local");
        }
        let _guard = crate::sync::begin_critical_section(frame.cast::<PyObject>());
        // SAFETY: Caller supplies a live frame pointer.
        let frame_ref = unsafe { &mut *frame };
        if index >= frame_ref.n_locals {
            return super::return_null_with_error("frame local index out of range");
        }
        if frame_ref.locals.is_null() {
            return super::return_null_with_error("frame has no local storage");
        }
        // SAFETY: Bounds checked above.
        unsafe {
            crate::sync::store_heap_pointer(frame_ref.locals.add(index as usize), value);
        }
        value
    })
}

/// Allocates a boxed generator/coroutine object around an existing heap frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_make_generator(resume: GenResumeFn, frame: *mut crate::abi::PyFrame, kind: u8) -> *mut PyObject {
    super::catch_object_helper(|| {
        if frame.is_null() {
            return super::return_null_with_error("generator frame pointer is null");
        }
        let Some(kind) = GeneratorKind::from_u8(kind) else {
            return super::return_null_with_error("unknown generator kind");
        };
        let types = match ensure_gen_runtime() {
            Ok(types) => types,
            Err(message) => return super::return_null_with_error(message),
        };
        let ty = kind_type(types, kind);
        let type_id = match kind {
            GeneratorKind::Coroutine => TYPE_ID_COROUTINE,
            GeneratorKind::Generator | GeneratorKind::AsyncGenerator => TYPE_ID_GENERATOR,
        };
        match super::with_runtime(|runtime| {
            let object = runtime.heap.alloc(core::mem::size_of::<PyGenerator>(), type_id).cast::<PyGenerator>();
            // SAFETY: `object` points to a freshly allocated zeroed block of the right size.
            unsafe {
                ptr::write(object, PyGenerator::new(ty.cast_const(), resume, frame, kind));
            }
            as_generator_object(object)
        }) {
            Some(object) => object,
            None => super::return_null_with_error("runtime is not initialized"),
        }
    })
}

/// Sends a value into a generator/coroutine and returns the next yielded value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gen_send(generator: *mut PyObject, value: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        let (generator, _types) = match unsafe { expect_generator(generator) } {
            Ok(pair) => pair,
            Err(message) => return raise_type_error(&message),
        };
        let _guard = crate::sync::begin_critical_section(as_generator_object(generator));
        // SAFETY: `expect_generator` proved the layout.
        let generator_ref = unsafe { &mut *generator };
        if generator_ref.running {
            return super::return_null_with_error("generator already executing");
        }
        let frame = generator_ref.frame;
        if frame.is_null() {
            return super::return_null_with_error("generator frame pointer is null");
        }
        if generator_ref.closed || unsafe { (*frame).state == FRAME_STATE_EXHAUSTED } {
            // SAFETY: `pon_none` and `pon_raise_stop_iteration` follow the NULL-sentinel ABI.
            let none = unsafe { super::pon_none() };
            return unsafe { super::exc::pon_raise_stop_iteration(none) };
        }
        if !generator_ref.pending_throw.is_null() {
            let exc = generator_ref.pending_throw;
            unsafe { crate::sync::store_heap_pointer(ptr::addr_of_mut!(generator_ref.pending_throw), ptr::null_mut()) };
            // SAFETY: Frame pointer is non-NULL and owned by this generator.
            unsafe {
                let _frame_guard = crate::sync::begin_critical_section(frame.cast::<PyObject>());
                (*frame).mark_exhausted();
            }
            // SAFETY: Re-raises the caller-provided exception object/type.
            return unsafe { super::exc::pon_raise(exc, ptr::null_mut()) };
        }

        let sent = if value.is_null() {
            // SAFETY: `pon_none` follows the runtime NULL-sentinel ABI.
            let none = unsafe { super::pon_none() };
            if none.is_null() {
                return ptr::null_mut();
            }
            none
        } else {
            value
        };

        generator_ref.running = true;
        thread_state_lock().push_frame(frame);
        pon_err_clear();
        RESUME_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
        // SAFETY: `resume` is supplied by codegen and follows `GenResumeFn`; `frame` is non-NULL.
        let result = unsafe { (generator_ref.resume)(frame, sent) };
        RESUME_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
        let _ = thread_state_lock().pop_frame();
        generator_ref.running = false;

        if result.is_null() {
            if current_stop_iteration_value().is_some() {
                unsafe {
                    let _frame_guard = crate::sync::begin_critical_section(frame.cast::<PyObject>());
                    (*frame).mark_exhausted();
                }
                return ptr::null_mut();
            }
            if !pon_err_occurred() {
                // Some resume implementations signal ordinary exhaustion by
                // marking the frame exhausted and returning NULL after consuming
                // an inner StopIteration. Normalize that path back to the public
                // NULL+StopIteration sentinel contract instead of leaking a
                // bare NULL to callers.
                if unsafe { (*frame).state == FRAME_STATE_EXHAUSTED } {
                    // SAFETY: `pon_none` and `pon_raise_stop_iteration` follow the NULL-sentinel ABI.
                    let none = unsafe { super::pon_none() };
                    return unsafe { super::exc::pon_raise_stop_iteration(none) };
                }
                return super::return_null_with_error("generator resume returned NULL without setting an exception");
            }
        }
        result
    })
}

/// Returns and clears the current `StopIteration.value`.
///
/// Under the runtime NULL-sentinel ABI, a consumed `StopIteration` must produce
/// a non-NULL object so callers can distinguish loop exhaustion from a real
/// helper failure.  Native iterators may raise `StopIteration` without an
/// explicit value (`message == NULL`); normalize that case to boxed `None`.
/// If a non-`StopIteration` exception is pending, return NULL without clearing it;
/// if no exception is pending at all, report helper misuse as a runtime error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gen_stop_value() -> *mut PyObject {
    super::catch_object_helper(|| match stop_iteration_value_and_clear() {
        Some(value) if !value.is_null() => value,
        Some(_) => unsafe { super::pon_none() },
        None if pon_err_occurred() => ptr::null_mut(),
        None => super::return_null_with_error("generator stop value requested without pending StopIteration"),
    })
}

/// Throws an exception into a generator/coroutine.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gen_throw(generator: *mut PyObject, exc: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        if exc.is_null() {
            return raise_type_error("generator throw exception is null");
        }
        let (generator, _types) = match unsafe { expect_generator(generator) } {
            Ok(pair) => pair,
            Err(message) => return raise_type_error(&message),
        };
        let _guard = crate::sync::begin_critical_section(as_generator_object(generator));
        // SAFETY: `expect_generator` proved the layout.
        let generator_ref = unsafe { &mut *generator };
        if generator_ref.running {
            return super::return_null_with_error("generator already executing");
        }
        if generator_ref.frame.is_null() {
            return super::return_null_with_error("generator frame pointer is null");
        }
        // If currently delegating to another generator, forward first.
        // SAFETY: Frame pointer is non-NULL.
        let parent = unsafe { (*generator_ref.frame).parent };
        if !parent.is_null() {
            if unsafe { expect_generator(parent) }.is_ok() {
                return unsafe { pon_gen_throw(parent, exc) };
            }
        }
        unsafe { crate::sync::store_heap_pointer(ptr::addr_of_mut!(generator_ref.pending_throw), exc) };
        unsafe { pon_gen_send(as_generator_object(generator), ptr::null_mut()) }
    })
}

/// Closes a generator/coroutine, forwarding close to an active delegate when present.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gen_close(generator: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        let (generator, _types) = match unsafe { expect_generator(generator) } {
            Ok(pair) => pair,
            Err(message) => return raise_type_error(&message),
        };
        let _guard = crate::sync::begin_critical_section(as_generator_object(generator));
        // SAFETY: `expect_generator` proved the layout.
        let generator_ref = unsafe { &mut *generator };
        if generator_ref.running {
            return super::return_null_with_error("generator already executing");
        }
        if !generator_ref.frame.is_null() {
            // SAFETY: Frame pointer is non-NULL.
            let parent = unsafe { (*generator_ref.frame).parent };
            if !parent.is_null() && unsafe { expect_generator(parent) }.is_ok() {
                // SAFETY: Parent was validated as a generator above.
                let closed = unsafe { pon_gen_close(parent) };
                if closed.is_null() {
                    return ptr::null_mut();
                }
            }
            // SAFETY: Frame pointer is non-NULL.
            unsafe {
                let _frame_guard = crate::sync::begin_critical_section(generator_ref.frame.cast::<PyObject>());
                (*generator_ref.frame).mark_exhausted();
            }
        }
        generator_ref.closed = true;
        unsafe { crate::sync::store_heap_pointer(ptr::addr_of_mut!(generator_ref.pending_throw), ptr::null_mut()) };
        // SAFETY: `pon_none` returns the initialized immortal singleton.
        unsafe { super::pon_none() }
    })
}

/// Returns an asynchronous iterator via `am_aiter`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_get_aiter(object: *mut PyObject, feedback: *mut FeedbackCell) -> *mut PyObject {
    unsafe { super::record_feedback_unary(feedback, object) };
    super::catch_object_helper(|| {
        if object.is_null() {
            return raise_type_error("async iterable operand is null");
        }
        // SAFETY: Non-NULL boxed object pointer supplied by caller.
        let ty = unsafe { (*object).ob_type };
        if ty.is_null() {
            return raise_type_error("async iterable has null type");
        }
        // SAFETY: `ty` is the object's live type descriptor.
        let slot = unsafe { (*ty).tp_as_async.as_ref().and_then(|methods| methods.am_aiter) };
        let Some(slot) = slot else {
            return raise_type_error("object is not an async iterable");
        };
        // SAFETY: Slot follows the unary object ABI.
        let result = unsafe { slot(object) };
        if result.is_null() && !pon_err_occurred() {
            return super::return_null_with_error("am_aiter returned NULL without setting an exception");
        }
        result
    })
}

/// Advances a synchronous iterator for `for` lowering.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_for_next(iterator: *mut PyObject, feedback: *mut FeedbackCell) -> *mut PyObject {
    // SAFETY: Delegates to the sync iterator helper, preserving NULL+StopIteration.
    unsafe { super::iter::pon_iter_next(iterator, feedback) }
}

/// Returns the value being yielded at a generator suspension point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_yield(value: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        let yielded = if value.is_null() {
            // SAFETY: `pon_none` returns the initialized immortal singleton.
            unsafe { super::pon_none() }
        } else {
            value
        };
        if !yielded.is_null() && recording_eager_yields() {
            record_eager_yield(yielded);
        }
        yielded
    })
}

/// Performs one `yield from` iterator step, preserving NULL+StopIteration.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_yield_from(iterator: *mut PyObject, feedback: *mut FeedbackCell) -> *mut PyObject {
    unsafe { super::record_feedback_unary(feedback, iterator) };
    super::catch_object_helper(|| {
        if iterator.is_null() {
            return raise_type_error("yield-from iterator is null");
        }
        // SAFETY: `abstract_op::iter_next` preserves StopIteration as NULL + pending exception.
        unsafe { abstract_op::iter_next(iterator) }
    })
}

/// Converts an awaitable to its await iterator.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_await(awaitable: *mut PyObject, feedback: *mut FeedbackCell) -> *mut PyObject {
    unsafe { super::record_feedback_unary(feedback, awaitable) };
    super::catch_object_helper(|| {
        if awaitable.is_null() {
            return raise_type_error("awaitable operand is null");
        }
        // SAFETY: Non-NULL boxed object pointer supplied by caller.
        let ty = unsafe { (*awaitable).ob_type };
        if ty.is_null() {
            return raise_type_error("awaitable has null type");
        }
        // SAFETY: `ty` is the object's live type descriptor.
        let slot = unsafe { (*ty).tp_as_async.as_ref().and_then(|methods| methods.am_await) };
        let Some(slot) = slot else {
            return raise_type_error("object is not awaitable");
        };
        // SAFETY: Slot follows the unary object ABI.
        let iterator = unsafe { slot(awaitable) };
        if iterator.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: Get an iterator from the await result.
        unsafe { abstract_op::get_iter(iterator) }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::{format_object_for_print, pon_const_int, pon_get_iter, pon_none, pon_runtime_init};
    use crate::abi::seq::pon_build_range;
    use crate::thread_state::{pon_err_clear, pon_err_message, pon_err_occurred, test_state_lock};

    unsafe extern "C" fn two_yields(frame: *mut crate::abi::PyFrame, sent: *mut PyObject) -> *mut PyObject {
        // SAFETY: Tests pass a live frame pointer.
        let frame = unsafe { &mut *frame };
        match frame.state {
            0 => {
                frame.state = 1;
                unsafe { pon_const_int(1) }
            }
            1 => {
                if !frame.locals.is_null() {
                    unsafe {
                        *frame.locals = sent;
                    }
                }
                frame.state = 2;
                unsafe { pon_const_int(2) }
            }
            _ => {
                frame.state = FRAME_STATE_EXHAUSTED;
                unsafe { super::super::exc::pon_raise_stop_iteration(sent) }
            }
        }
    }

    #[test]
    fn generator_send_and_exhaustion_follow_null_sentinel() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            pon_err_clear();
            let frame = pon_make_frame(1);
            assert!(!frame.is_null());
            let generator = pon_make_generator(two_yields, frame, GeneratorKind::Generator.as_u8());
            assert!(!generator.is_null());

            let first = pon_gen_send(generator, pon_none());
            assert_eq!(format_object_for_print(first).as_deref(), Ok("1"));
            let sent = pon_const_int(41);
            let second = pon_gen_send(generator, sent);
            assert_eq!(format_object_for_print(second).as_deref(), Ok("2"));
            assert_eq!(pon_frame_get_local(frame, 0), sent);

            let done = pon_gen_send(generator, pon_none());
            assert!(done.is_null());
            assert!(pon_err_occurred());
            pon_err_clear();
        }
    }

    #[test]
    fn generator_sync_mutator_paths_compile_and_update_state() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            pon_err_clear();
            let frame = pon_make_frame(1);
            assert!(!frame.is_null());
            let local = pon_const_int(7);
            assert_eq!(pon_frame_set_local(frame, 0, local), local);
            assert_eq!(pon_frame_get_local(frame, 0), local);

            let generator = pon_make_generator(two_yields, frame, GeneratorKind::Generator.as_u8());
            assert!(!generator.is_null());
            assert_eq!(format_object_for_print(pon_gen_send(generator, pon_none())).as_deref(), Ok("1"));
            assert!(!pon_gen_close(generator).is_null());
            assert!(pon_gen_send(generator, pon_none()).is_null());
            assert!(pon_err_occurred());
            pon_err_clear();
        }
    }

    #[test]
    fn generator_close_marks_frame_exhausted() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            pon_err_clear();
            let frame = pon_make_frame(0);
            let generator = pon_make_generator(two_yields, frame, GeneratorKind::Generator.as_u8());
            assert_eq!(pon_gen_close(generator), pon_none());
            assert_eq!((*frame).state, FRAME_STATE_EXHAUSTED);
            assert!(pon_gen_send(generator, pon_none()).is_null());
            assert!(pon_err_occurred());
            pon_err_clear();
        }
    }

    #[test]
    fn for_next_exhaustion_stop_value_returns_boxed_none_sentinel() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            pon_err_clear();
            let range = pon_build_range(pon_none(), pon_const_int(0), pon_none());
            assert!(!range.is_null(), "pon_build_range(0, 0) returned NULL");
            assert!(!pon_err_occurred(), "pon_build_range(0, 0) left an exception pending");
            let iterator = pon_get_iter(range, ptr::null_mut());
            assert!(!iterator.is_null(), "iter(range(0)) returned NULL");
            assert!(!pon_err_occurred(), "iter(range(0)) left an exception pending");

            let item = pon_for_next(iterator, ptr::null_mut());
            assert!(item.is_null(), "exhausted range iterator should return the NULL sentinel");
            assert!(pon_err_occurred(), "exhausted range iterator should leave StopIteration pending");

            let stop_value = pon_gen_stop_value();
            assert!(
                !stop_value.is_null(),
                "loop terminator must receive a boxed sentinel for StopIteration(None), not NULL",
            );
            assert_eq!(stop_value, pon_none(), "StopIteration(None) should normalize to boxed None");
            assert_eq!(format_object_for_print(stop_value).as_deref(), Ok("None"));
            assert!(
                !pon_err_occurred(),
                "pon_gen_stop_value should consume StopIteration after returning the boxed sentinel",
            );
        }
    }

    #[test]
    fn gen_stop_value_preserves_real_iterator_errors() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            pon_err_clear();
            let not_an_iterator = pon_const_int(1);

            let item = pon_for_next(not_an_iterator, ptr::null_mut());
            assert!(item.is_null(), "pon_for_next(non-iterator) should fail with NULL");
            assert!(pon_err_occurred(), "pon_for_next(non-iterator) should leave an exception pending");
            let original_error = pon_err_message().unwrap_or_default();
            assert!(
                original_error.to_ascii_lowercase().contains("iterator"),
                "expected iterator diagnostic, got {original_error:?}",
            );

            let stop_value = pon_gen_stop_value();
            assert!(
                stop_value.is_null(),
                "loop terminator helper must not convert non-StopIteration errors into exhaustion",
            );
            assert!(
                pon_err_occurred(),
                "non-StopIteration error should remain pending after pon_gen_stop_value",
            );
            assert_eq!(pon_err_message().as_deref(), Some(original_error.as_str()));
            pon_err_clear();
        }
    }

    #[test]
    fn yield_from_advances_generator_iterator() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            pon_err_clear();
            let frame = pon_make_frame(0);
            let generator = pon_make_generator(two_yields, frame, GeneratorKind::Generator.as_u8());
            let first = pon_yield_from(generator, ptr::null_mut());
            assert_eq!(format_object_for_print(first).as_deref(), Ok("1"));
            let second = pon_yield_from(generator, ptr::null_mut());
            assert_eq!(format_object_for_print(second).as_deref(), Ok("2"));
            assert!(pon_yield_from(generator, ptr::null_mut()).is_null());
            assert!(pon_err_occurred());
            pon_err_clear();
        }
    }
}
