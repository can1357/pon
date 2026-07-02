//! Generator, coroutine, and async-generator helper family namespace.
//!
//! These helpers implement the pin J0.1 stackless generator contract: one heap
//! [`GenFrame`] per generator instance carries the resume state, the
//! send/throw payload, and every live-across-suspend local in trailing spill
//! slots; the compiled body is a single-argument resume function dispatched by
//! `resume_state`; and every fallible helper reports errors through the
//! runtime NULL-sentinel thread-state path.

use core::{cell::Cell, ptr};

use pon_gc::GcTypeInfo;

use crate::abstract_op;
use crate::feedback::FeedbackCell;
use crate::object::{PyObject, PyType, is_exact_type};
use crate::thread_state::{pon_err_clear, pon_err_occurred, thread_state_lock};
use crate::types::coroutine::{TYPE_ID_COROUTINE, ensure_coroutine_type};
use crate::types::exc::{ExceptionKind, PyBaseException, is_exception_instance, is_exception_subclass};
use crate::types::frame::{TYPE_ID_FRAME, ensure_frame_type, finalize_frame, trace_frame};
use crate::types::generator::{
    GEN_FRAME_HEADER_SIZE, GenFrame, GeneratorKind, PyGenerator, RESUME_FINISHED, RESUME_RUNNING,
    RESUME_START, TYPE_ID_GEN_FRAME, TYPE_ID_GENERATOR, as_generator_mut, as_generator_object,
    ensure_generator_type, gen_frame_alloc_size, trace_gen_frame, trace_generator,
};

/// Compiled resumable-body ABI (pin J0.1 §2): one argument, payload in the frame.
pub use crate::types::generator::GenResumeBodyFn;

thread_local! {
    /// `StopIteration.value` of the last delegation/loop finish consumed by
    /// [`pon_gen_stop_value`]; read back by [`pon_gen_last_stop_value`] as the
    /// `yield from` expression result.  Rooted via [`last_stop_value_root`].
    static LAST_STOP_VALUE: Cell<*mut PyObject> = const { Cell::new(ptr::null_mut()) };
}

/// Current thread's stashed delegation finish value, for GC rooting.
pub(crate) fn last_stop_value_root() -> *mut PyObject {
    LAST_STOP_VALUE.with(Cell::get)
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
        // Legacy PyFrame registration: `frame_stack`/tracebacks may still hold
        // PyFrame-typed pointers from non-generator paths.
        runtime.heap.register_type(
            TYPE_ID_FRAME,
            GcTypeInfo {
                size: core::mem::size_of::<crate::abi::PyFrame>(),
                trace: trace_frame,
                finalize: Some(finalize_frame),
            },
        );
        // Pin J0.1 §1.1: nominal size only — real size is recorded per
        // allocation (trailing slot array); one allocation, no finalizer.
        runtime.heap.register_type(
            TYPE_ID_GEN_FRAME,
            GcTypeInfo {
                size: core::mem::size_of::<GenFrame>(),
                trace: trace_gen_frame,
                finalize: None,
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

fn raise_value_error(message: &str) -> *mut PyObject {
    // SAFETY: The exception helper copies the byte message under the runtime error discipline.
    unsafe { super::exc::pon_raise_value_error(message.as_ptr(), message.len()) }
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

/// True when the pending exception matches builtin `kind`.
fn pending_exception_matches(kind: ExceptionKind) -> bool {
    let current = thread_state_lock().current_exc;
    if current.is_null() {
        return false;
    }
    super::with_runtime(|runtime| {
        // SAFETY: `current` is the thread state's active exception pointer.
        unsafe { is_exception_instance(current, runtime.exception_types.get(kind).cast_const()) }
    })
    .unwrap_or(false)
}

unsafe fn is_none_value(object: *mut PyObject) -> bool {
    if object.is_null() {
        return false;
    }
    super::with_runtime(|runtime| unsafe { is_exact_type(object, runtime.none_type.cast_const()) }).unwrap_or(false)
}

/// True when `object` is the `GeneratorExit` type or one of its instances.
unsafe fn is_generator_exit(object: *mut PyObject) -> bool {
    if object.is_null() {
        return false;
    }
    super::with_runtime(|runtime| {
        let exit_ty = runtime.exception_types.generator_exit.cast_const();
        if super::is_type_object_for_gen(runtime, object) {
            // SAFETY: `object` is a live type object.
            return unsafe { is_exception_subclass(object.cast::<PyType>().cast_const(), exit_ty) };
        }
        // SAFETY: Non-type objects are checked as instances.
        unsafe { is_exception_instance(object, exit_ty) }
    })
    .unwrap_or(false)
}

/// Stores `RESUME_FINISHED` and zeroes the payload fields and every spill slot
/// (pin J0.1 §4.4 GC hygiene: a finished frame pins nothing).
unsafe fn finish_frame(frame: *mut GenFrame) {
    let _guard = crate::sync::begin_critical_section(frame.cast::<PyObject>());
    // SAFETY: Caller supplies a live frame pointer.
    unsafe {
        (*frame).resume_state = RESUME_FINISHED;
        crate::sync::store_heap_pointer(ptr::addr_of_mut!((*frame).sent_value), ptr::null_mut());
        crate::sync::store_heap_pointer(ptr::addr_of_mut!((*frame).thrown_exc), ptr::null_mut());
        let slot_count = (*frame).slot_count;
        for index in 0..slot_count {
            GenFrame::set_slot(frame, index, ptr::null_mut());
        }
    }
}

/// Allocates a zeroed resumable generator frame with `slot_count` spill slots
/// (pin J0.1 §1.1).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gen_frame_alloc(slot_count: u32) -> *mut GenFrame {
    super::catch_object_helper(|| {
        let types = match ensure_gen_runtime() {
            Ok(types) => types,
            Err(message) => return super::return_null_with_error(message),
        };
        match super::with_runtime(|runtime| {
            let frame = runtime
                .heap
                .alloc(gen_frame_alloc_size(slot_count), TYPE_ID_GEN_FRAME)
                .cast::<GenFrame>();
            // SAFETY: `frame` points to a zeroed block of `gen_frame_alloc_size`
            // bytes: zeroed memory already encodes RESUME_START, NULL payload
            // fields, and NULL slots.  Only the header and slot_count need writes.
            unsafe {
                (*frame).header = crate::object::PyObjectHeader::new(types.frame.cast_const());
                (*frame).slot_count = slot_count;
            }
            frame
        }) {
            Some(frame) => frame.cast::<PyObject>(),
            None => super::return_null_with_error("runtime is not initialized"),
        }
    })
    .cast::<GenFrame>()
}

/// Allocates a boxed generator/coroutine object around an existing resumable
/// frame.  Captures the current function object (when a call is active) so the
/// suspended body keeps its closure-cell context across resumptions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_make_generator(body: GenResumeBodyFn, frame: *mut GenFrame, kind: u8) -> *mut PyObject {
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
        let function = super::current_function_object();
        match super::with_runtime(|runtime| {
            let object = runtime.heap.alloc(core::mem::size_of::<PyGenerator>(), type_id).cast::<PyGenerator>();
            // SAFETY: `object` points to a freshly allocated zeroed block of the right size.
            unsafe {
                ptr::write(object, PyGenerator::new(ty.cast_const(), body, frame, function, kind));
            }
            as_generator_object(object)
        }) {
            Some(object) => object,
            None => super::return_null_with_error("runtime is not initialized"),
        }
    })
}

/// Resumes a generator/coroutine body once: the shared driver core behind
/// `send`, `throw`, and `close` (pin J0.1 §4.1).
///
/// At most one of `sent`/`thrown` is non-NULL (both NULL means `next(g)`).
/// The driver reads `resume_state` for its guards and never writes it; the
/// payload travels through the frame; the body is the only execution site.
///
/// # Safety
/// `generator` must be a boxed generator/coroutine object; `sent` and `thrown`
/// must each be a valid boxed object or NULL.
unsafe fn pon_gen_resume(generator: *mut PyObject, sent: *mut PyObject, thrown: *mut PyObject) -> *mut PyObject {
    // 1. Validate the generator object; enter its critical section.
    let (generator, _types) = match unsafe { expect_generator(generator) } {
        Ok(pair) => pair,
        Err(message) => return raise_type_error(&message),
    };
    let _guard = crate::sync::begin_critical_section(as_generator_object(generator));
    // SAFETY: `expect_generator` proved the layout.
    let generator_ref = unsafe { &mut *generator };
    let is_coroutine = generator_ref.kind == GeneratorKind::Coroutine.as_u8();
    let frame = generator_ref.frame;
    if frame.is_null() {
        return super::return_null_with_error("generator frame pointer is null");
    }

    // 2-3. RUNNING guard (single-writer discipline: the body wrote this).
    // SAFETY: `frame` is non-NULL and owned by this generator.
    let state = unsafe { (*frame).resume_state };
    if state == RESUME_RUNNING {
        return raise_value_error(if is_coroutine {
            "coroutine already executing"
        } else {
            "generator already executing"
        });
    }

    // 4. Finished/closed guard.
    if state == RESUME_FINISHED || generator_ref.closed {
        if !thrown.is_null() {
            // throw into a finished generator re-raises the exception.
            // SAFETY: `thrown` is a boxed exception instance or type.
            return unsafe { super::exc::pon_raise(thrown, ptr::null_mut()) };
        }
        if is_coroutine {
            let message = "cannot reuse already awaited coroutine";
            return super::return_null_with_error(message);
        }
        // SAFETY: `pon_none` and `pon_raise_stop_iteration` follow the NULL-sentinel ABI.
        let none = unsafe { super::pon_none() };
        return unsafe { super::exc::pon_raise_stop_iteration(none) };
    }

    // 5. Non-None send into a just-started generator.
    if state == RESUME_START && !sent.is_null() && !unsafe { is_none_value(sent) } && thrown.is_null() {
        return raise_type_error(if is_coroutine {
            "can't send non-None value to a just-started coroutine"
        } else {
            "can't send non-None value to a just-started generator"
        });
    }

    // 6. Write the payload through the frame (write-barriered).
    unsafe {
        let _frame_guard = crate::sync::begin_critical_section(frame.cast::<PyObject>());
        crate::sync::store_heap_pointer(ptr::addr_of_mut!((*frame).sent_value), sent);
        crate::sync::store_heap_pointer(ptr::addr_of_mut!((*frame).thrown_exc), thrown);
    }

    // 7. Root the frame for GC + traceback identity; clear stale errors.
    // The cast is sound: frame_stack consumers treat entries as opaque
    // identity pointers and never read past the shared PyObjectHeader.
    thread_state_lock().push_frame(frame.cast::<crate::abi::PyFrame>());
    pon_err_clear();

    // 8. Call the compiled body — the ONLY body call site.  The generator's
    // captured function object is pushed as the current call so closure-cell
    // loads inside the body resolve across suspensions.
    let body = generator_ref.body;
    let call_guard = super::push_current_call(generator_ref.function.cast::<crate::abi::PyFunction>(), ptr::null_mut(), 0);
    // SAFETY: `body` was supplied by codegen with the GenResumeBodyFn ABI; `frame` is live.
    let result = unsafe { body(frame) };
    drop(call_guard);

    // 9. Unroot.
    let _ = thread_state_lock().pop_frame();

    // 10. NULL ⇒ pending exception (StopIteration = normal exhaustion).
    if result.is_null() && !pon_err_occurred() {
        return super::return_null_with_error("generator body returned NULL without setting an exception");
    }
    result
}

/// Sends a value into a generator/coroutine and returns the next yielded value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gen_send(generator: *mut PyObject, value: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| unsafe { pon_gen_resume(generator, value, ptr::null_mut()) })
}

/// Throws an exception into a generator/coroutine at its suspend point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gen_throw(generator: *mut PyObject, exc: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        if exc.is_null() {
            return raise_type_error("generator throw exception is null");
        }
        unsafe { pon_gen_resume(generator, ptr::null_mut(), exc) }
    })
}

/// Closes a generator/coroutine: throw `GeneratorExit`, expect
/// `StopIteration`/`GeneratorExit` back (pin J0.1 §4.5).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gen_close(generator: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        let (generator, _types) = match unsafe { expect_generator(generator) } {
            Ok(pair) => pair,
            Err(message) => return raise_type_error(&message),
        };
        // 1-2. Validate state; close-on-START/FINISHED never runs the body.
        {
            let _guard = crate::sync::begin_critical_section(as_generator_object(generator));
            // SAFETY: `expect_generator` proved the layout.
            let generator_ref = unsafe { &mut *generator };
            let frame = generator_ref.frame;
            if frame.is_null() {
                return super::return_null_with_error("generator frame pointer is null");
            }
            // SAFETY: `frame` is non-NULL and owned by this generator.
            let state = unsafe { (*frame).resume_state };
            if state == RESUME_RUNNING {
                return raise_value_error("generator already executing");
            }
            if generator_ref.closed || state == RESUME_FINISHED || state == RESUME_START {
                generator_ref.closed = true;
                if state != RESUME_FINISHED {
                    // SAFETY: `frame` is live; finishing zeroes payload+slots.
                    unsafe { finish_frame(frame) };
                }
                // SAFETY: `pon_none` returns the initialized immortal singleton.
                return unsafe { super::pon_none() };
            }
        }

        // 3. Build a GeneratorExit instance.
        let exit_exc = match super::with_runtime(|runtime| {
            super::exc::alloc_exception_object(runtime, runtime.exception_types.generator_exit, ptr::null_mut(), ptr::null_mut())
        }) {
            Some(Ok(exception)) => exception,
            Some(Err(message)) => return super::return_null_with_error(message),
            None => return super::return_null_with_error("runtime is not initialized"),
        };

        // 4. Deliver it at the suspend point.
        let result = unsafe { pon_gen_resume(as_generator_object(generator), ptr::null_mut(), exit_exc) };

        // 5. The body caught GeneratorExit and yielded again.
        if !result.is_null() {
            let message = "generator ignored GeneratorExit";
            return super::return_null_with_error(message);
        }

        // 6. GeneratorExit/StopIteration ⇒ swallowed; anything else propagates.
        if pending_exception_matches(ExceptionKind::GeneratorExit) || pending_exception_matches(ExceptionKind::StopIteration) {
            pon_err_clear();
            // SAFETY: `expect_generator` proved the layout; mark idempotent-closed.
            unsafe {
                let _guard = crate::sync::begin_critical_section(as_generator_object(generator));
                (*generator).closed = true;
            }
            // SAFETY: `pon_none` returns the initialized immortal singleton.
            return unsafe { super::pon_none() };
        }
        ptr::null_mut()
    })
}

/// Consumes the resume payload of `frame` at a dispatched resume point
/// (pin J0.1 §4.2).
///
/// A pending `throw` payload is cleared and re-raised (NULL-routes to the
/// statically enclosing handler); otherwise the sent value (`None` when
/// absent) is returned as the yield-expression result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gen_consume_payload(frame: *mut GenFrame) -> *mut PyObject {
    super::catch_object_helper(|| {
        if frame.is_null() {
            return super::return_null_with_error("generator frame pointer is null");
        }
        // SAFETY: Compiled bodies pass their own live frame.
        let thrown = unsafe { (*frame).thrown_exc };
        if !thrown.is_null() {
            // Clear BEFORE raising: the frame must not keep the exception
            // alive past its delivery.
            unsafe {
                let _guard = crate::sync::begin_critical_section(frame.cast::<PyObject>());
                crate::sync::store_heap_pointer(ptr::addr_of_mut!((*frame).thrown_exc), ptr::null_mut());
            }
            // SAFETY: `thrown` is the boxed exception scheduled by the driver.
            return unsafe { super::exc::pon_raise(thrown, ptr::null_mut()) };
        }
        // SAFETY: Same live-frame contract as above.
        let sent = unsafe { (*frame).sent_value };
        unsafe {
            let _guard = crate::sync::begin_critical_section(frame.cast::<PyObject>());
            crate::sync::store_heap_pointer(ptr::addr_of_mut!((*frame).sent_value), ptr::null_mut());
        }
        if sent.is_null() {
            // NULL-means-None keeps the body total (pin §4.2).
            // SAFETY: `pon_none` returns the initialized immortal singleton.
            unsafe { super::pon_none() }
        } else {
            sent
        }
    })
}

/// Compiled `return v` epilogue for generator bodies (pin J0.1 §4.4): finish
/// the frame and leave `StopIteration(v)` pending.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gen_finish(frame: *mut GenFrame, retval: *mut PyObject) -> *mut PyObject {
    if frame.is_null() {
        return super::return_null_with_error("generator frame pointer is null");
    }
    // SAFETY: Compiled bodies pass their own live frame.
    unsafe { finish_frame(frame) };
    let value = if retval.is_null() {
        // SAFETY: `pon_none` returns the initialized immortal singleton.
        let none = unsafe { super::pon_none() };
        if none.is_null() {
            return ptr::null_mut();
        }
        none
    } else {
        retval
    };
    // SAFETY: Installs StopIteration(value) and returns NULL per the sentinel ABI.
    unsafe { super::exc::pon_raise_stop_iteration(value) }
}

/// Function-level exception exit for generator bodies (pin J0.1 §4.4): finish
/// the frame, apply PEP 479, and keep the pending exception.
///
/// `is_coroutine` selects the diagnostic wording (`generator`/`coroutine`
/// raised StopIteration).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gen_unwind(frame: *mut GenFrame, is_coroutine: u8) -> *mut PyObject {
    if frame.is_null() {
        return super::return_null_with_error("generator frame pointer is null");
    }
    // SAFETY: Compiled bodies pass their own live frame.
    unsafe { finish_frame(frame) };
    if !pon_err_occurred() {
        return super::return_null_with_error("generator unwound without a pending exception");
    }
    // PEP 479: StopIteration escaping a generator body becomes RuntimeError
    // with the original exception as __cause__.
    if pending_exception_matches(ExceptionKind::StopIteration) {
        let original = thread_state_lock().current_exc;
        pon_err_clear();
        let text: &str = if is_coroutine != 0 {
            "coroutine raised StopIteration"
        } else {
            "generator raised StopIteration"
        };
        let runtime_error = match super::with_runtime(|runtime| match super::alloc_unicode(runtime, text.as_bytes()) {
            Ok(message) => super::exc::alloc_exception_object(runtime, runtime.exception_types.runtime_error, message, original),
            Err(message) => Err(message),
        }) {
            Some(Ok(exception)) => exception,
            Some(Err(message)) => return super::return_null_with_error(message),
            None => return super::return_null_with_error("runtime is not initialized"),
        };
        // SAFETY: `pon_raise` installs the instance and links __cause__.
        return unsafe { super::exc::pon_raise(runtime_error, original) };
    }
    ptr::null_mut()
}

/// Returns and clears the current `StopIteration.value`.
///
/// Under the runtime NULL-sentinel ABI, a consumed `StopIteration` must produce
/// a non-NULL object so callers can distinguish loop exhaustion from a real
/// helper failure.  Native iterators may raise `StopIteration` without an
/// explicit value (`message == NULL`); normalize that case to boxed `None`.
/// The consumed value is also stashed for [`pon_gen_last_stop_value`] so
/// `yield from`/`await` lowering can read the delegation result.
/// If a non-`StopIteration` exception is pending, return NULL without clearing it;
/// if no exception is pending at all, report helper misuse as a runtime error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gen_stop_value() -> *mut PyObject {
    super::catch_object_helper(|| match stop_iteration_value_and_clear() {
        Some(value) => {
            let value = if value.is_null() {
                // SAFETY: `pon_none` returns the initialized immortal singleton.
                unsafe { super::pon_none() }
            } else {
                value
            };
            LAST_STOP_VALUE.with(|stash| stash.set(value));
            value
        }
        None if pon_err_occurred() => ptr::null_mut(),
        None => super::return_null_with_error("generator stop value requested without pending StopIteration"),
    })
}

/// Produces the stashed `StopIteration.value` of the last finished delegation
/// (`yield from`/`await` expression result).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gen_last_stop_value() -> *mut PyObject {
    super::catch_object_helper(|| {
        let value = LAST_STOP_VALUE.with(|stash| stash.replace(ptr::null_mut()));
        if value.is_null() {
            // A finished delegate always went through pon_gen_stop_value, but
            // stay total: a missing stash decodes as None.
            // SAFETY: `pon_none` returns the initialized immortal singleton.
            unsafe { super::pon_none() }
        } else {
            value
        }
    })
}

/// Forwards the enclosing frame's resume payload to a `yield from`/`await`
/// delegate for exactly one step (pin J0.1 §6).
///
/// Returns the delegate's next yielded value, or NULL with `StopIteration`
/// pending when the delegation finished (decoded by the `ForLoop` terminator
/// via [`pon_gen_stop_value`]); other exceptions propagate as plain NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_gen_delegate_step(frame: *mut GenFrame, delegate: *mut PyObject) -> *mut PyObject {
    super::catch_object_helper(|| {
        if frame.is_null() {
            return super::return_null_with_error("generator frame pointer is null");
        }
        if delegate.is_null() {
            return raise_type_error("yield-from delegate is null");
        }
        // Read-and-clear the payload (the delegate consumes it, not this body).
        // SAFETY: Compiled bodies pass their own live frame.
        let (thrown, sent) = unsafe { ((*frame).thrown_exc, (*frame).sent_value) };
        unsafe {
            let _guard = crate::sync::begin_critical_section(frame.cast::<PyObject>());
            crate::sync::store_heap_pointer(ptr::addr_of_mut!((*frame).thrown_exc), ptr::null_mut());
            crate::sync::store_heap_pointer(ptr::addr_of_mut!((*frame).sent_value), ptr::null_mut());
        }

        let types = match ensure_gen_runtime() {
            Ok(types) => types,
            Err(message) => return super::return_null_with_error(message),
        };
        let delegate_is_gen = unsafe { generator_kind_for(delegate, types) }.is_some();

        if !thrown.is_null() {
            // GeneratorExit: close the delegate, then re-raise (PEP 342/380).
            if unsafe { is_generator_exit(thrown) } {
                if delegate_is_gen {
                    // SAFETY: `delegate` is one of our generators/coroutines.
                    let closed = unsafe { pon_gen_close(delegate) };
                    if closed.is_null() {
                        return ptr::null_mut();
                    }
                } else {
                    // Foreign delegate: call close() when present, ignore absence.
                    let close_method = unsafe { abstract_op::get_attr(delegate, crate::intern::intern("close")) };
                    if close_method.is_null() {
                        pon_err_clear();
                    } else {
                        // SAFETY: Bound method invoked through the call ABI.
                        let closed = unsafe { super::pon_call(close_method, ptr::null_mut(), 0) };
                        if closed.is_null() {
                            return ptr::null_mut();
                        }
                    }
                }
                // SAFETY: Re-raises the caller-scheduled GeneratorExit.
                return unsafe { super::exc::pon_raise(thrown, ptr::null_mut()) };
            }
            // Forward the throw when the delegate supports it.
            if delegate_is_gen {
                // SAFETY: `delegate` is one of our generators/coroutines.
                return unsafe { pon_gen_throw(delegate, thrown) };
            }
            let throw_method = unsafe { abstract_op::get_attr(delegate, crate::intern::intern("throw")) };
            if throw_method.is_null() {
                // No throw(): the delegation ends; re-raise here (the delegate
                // is NOT closed, matching CPython).
                pon_err_clear();
                // SAFETY: Re-raises the caller-scheduled exception.
                return unsafe { super::exc::pon_raise(thrown, ptr::null_mut()) };
            }
            let mut argv = [thrown];
            // SAFETY: Bound method invoked through the call ABI.
            return unsafe { super::pon_call(throw_method, argv.as_mut_ptr(), argv.len()) };
        }

        // Plain step: None ⇒ __next__, real value ⇒ send().
        if sent.is_null() || unsafe { is_none_value(sent) } {
            // SAFETY: One nullable iterator step preserving StopIteration.
            return unsafe { abstract_op::iter_next(delegate) };
        }
        if delegate_is_gen {
            // SAFETY: `delegate` is one of our generators/coroutines.
            return unsafe { pon_gen_send(delegate, sent) };
        }
        let send_method = unsafe { abstract_op::get_attr(delegate, crate::intern::intern("send")) };
        if send_method.is_null() {
            return ptr::null_mut();
        }
        let mut argv = [sent];
        // SAFETY: Bound method invoked through the call ABI.
        unsafe { super::pon_call(send_method, argv.as_mut_ptr(), argv.len()) }
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
        let iterator = if let Some(slot) = slot {
            // SAFETY: Slot follows the unary object ABI.
            unsafe { slot(awaitable) }
        } else {
            let method = unsafe { abstract_op::get_attr(awaitable, crate::intern::intern("__await__")) };
            if method.is_null() {
                return ptr::null_mut();
            }
            unsafe { super::pon_call(method, ptr::null_mut(), 0) }
        };
        if iterator.is_null() {
            return ptr::null_mut();
        }
        let types = match ensure_gen_runtime() {
            Ok(types) => types,
            Err(message) => return super::return_null_with_error(message),
        };
        if unsafe { generator_kind_for(iterator, types) } == Some(GeneratorKind::Coroutine) {
            return iterator;
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

    /// Hand-written body mirroring pin J0.1 §4.6: two yields, `a` spilled in
    /// slot 0 across suspend point 2, `StopIteration(a + b)` on finish.
    unsafe extern "C" fn two_yields_body(frame: *mut GenFrame) -> *mut PyObject {
        // SAFETY: Tests pass a live frame allocated with slot_count >= 1.
        unsafe {
            let state = (*frame).resume_state;
            (*frame).resume_state = RESUME_RUNNING;
            match state {
                RESUME_START => {
                    let payload = pon_gen_consume_payload(frame);
                    if payload.is_null() {
                        return pon_gen_unwind(frame, 0);
                    }
                    (*frame).resume_state = 1;
                    pon_const_int(1)
                }
                1 => {
                    let sent = pon_gen_consume_payload(frame);
                    if sent.is_null() {
                        return pon_gen_unwind(frame, 0);
                    }
                    GenFrame::set_slot(frame, 0, sent); // spill `a`
                    (*frame).resume_state = 2;
                    pon_const_int(2)
                }
                2 => {
                    let a = GenFrame::slot(frame, 0);
                    let sent = pon_gen_consume_payload(frame);
                    if sent.is_null() {
                        return pon_gen_unwind(frame, 0);
                    }
                    let _ = sent;
                    pon_gen_finish(frame, a)
                }
                _ => pon_gen_finish(frame, ptr::null_mut()),
            }
        }
    }

    #[test]
    fn generator_send_and_exhaustion_follow_null_sentinel() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            pon_err_clear();
            let frame = pon_gen_frame_alloc(1);
            assert!(!frame.is_null());
            assert_eq!((*frame).resume_state, RESUME_START);
            assert_eq!((*frame).slot_count, 1);
            let generator = pon_make_generator(two_yields_body, frame, GeneratorKind::Generator.as_u8());
            assert!(!generator.is_null());

            let first = pon_gen_send(generator, ptr::null_mut());
            assert_eq!(format_object_for_print(first).as_deref(), Ok("1"));
            assert_eq!((*frame).resume_state, 1);

            let sent = pon_const_int(41);
            let second = pon_gen_send(generator, sent);
            assert_eq!(format_object_for_print(second).as_deref(), Ok("2"));
            assert_eq!((*frame).resume_state, 2);
            assert_eq!(GenFrame::slot(frame, 0), sent);

            // Finish: StopIteration(41) pending, frame finished, slots zeroed.
            let done = pon_gen_send(generator, pon_none());
            assert!(done.is_null());
            assert!(pon_err_occurred());
            let value = pon_gen_stop_value();
            assert_eq!(value, sent, "StopIteration.value must carry the return value");
            assert_eq!((*frame).resume_state, RESUME_FINISHED);
            assert_eq!(GenFrame::slot(frame, 0), ptr::null_mut(), "finished frame must pin nothing");

            // Late send: StopIteration(None) without running the body.
            let late = pon_gen_send(generator, pon_none());
            assert!(late.is_null());
            assert!(pon_err_occurred());
            pon_err_clear();
        }
    }

    #[test]
    fn send_non_none_to_fresh_generator_raises_type_error() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            pon_err_clear();
            let frame = pon_gen_frame_alloc(1);
            let generator = pon_make_generator(two_yields_body, frame, GeneratorKind::Generator.as_u8());
            let result = pon_gen_send(generator, pon_const_int(3));
            assert!(result.is_null());
            let message = pon_err_message().unwrap_or_default();
            assert!(message.contains("just-started"), "got {message:?}");
            assert_eq!((*frame).resume_state, RESUME_START, "guard must reject before running the body");
            pon_err_clear();
        }
    }

    #[test]
    fn throw_into_suspended_generator_unwinds_and_finishes() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            pon_err_clear();
            let frame = pon_gen_frame_alloc(1);
            let generator = pon_make_generator(two_yields_body, frame, GeneratorKind::Generator.as_u8());
            assert_eq!(format_object_for_print(pon_gen_send(generator, ptr::null_mut())).as_deref(), Ok("1"));

            // Build a KeyError instance and throw it in at suspend point 1.
            let exc = crate::abi::with_runtime(|runtime| {
                crate::abi::exc::alloc_exception_object(runtime, runtime.exception_types.key_error, ptr::null_mut(), ptr::null_mut()).unwrap()
            })
            .unwrap();
            let result = pon_gen_throw(generator, exc);
            assert!(result.is_null());
            assert!(pon_err_occurred());
            assert_eq!((*frame).resume_state, RESUME_FINISHED);
            pon_err_clear();

            // Late send after unwind: StopIteration(None).
            assert!(pon_gen_send(generator, pon_none()).is_null());
            assert!(pon_err_occurred());
            pon_err_clear();
        }
    }

    #[test]
    fn close_on_suspended_generator_swallows_generator_exit() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            pon_err_clear();
            let frame = pon_gen_frame_alloc(1);
            let generator = pon_make_generator(two_yields_body, frame, GeneratorKind::Generator.as_u8());
            assert_eq!(format_object_for_print(pon_gen_send(generator, ptr::null_mut())).as_deref(), Ok("1"));

            let closed = pon_gen_close(generator);
            assert_eq!(closed, pon_none());
            assert!(!pon_err_occurred(), "close must swallow GeneratorExit");
            assert_eq!((*frame).resume_state, RESUME_FINISHED);

            assert!(pon_gen_send(generator, pon_none()).is_null());
            assert!(pon_err_occurred());
            pon_err_clear();
        }
    }

    #[test]
    fn close_on_fresh_generator_never_runs_the_body() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            pon_err_clear();
            let frame = pon_gen_frame_alloc(0);
            let generator = pon_make_generator(two_yields_body, frame, GeneratorKind::Generator.as_u8());
            assert_eq!(pon_gen_close(generator), pon_none());
            assert_eq!((*frame).resume_state, RESUME_FINISHED);
            assert!(pon_gen_send(generator, pon_none()).is_null());
            assert!(pon_err_occurred());
            pon_err_clear();
        }
    }

    #[test]
    fn pep479_stop_iteration_escaping_body_becomes_runtime_error() {
        unsafe extern "C" fn raises_stop_iteration(frame: *mut GenFrame) -> *mut PyObject {
            // SAFETY: Tests pass a live frame.
            unsafe {
                (*frame).resume_state = RESUME_RUNNING;
                let payload = pon_gen_consume_payload(frame);
                if payload.is_null() {
                    return pon_gen_unwind(frame, 0);
                }
                // Body raises StopIteration itself -> unwind must PEP 479 it.
                let none = pon_none();
                let _ = crate::abi::exc::pon_raise_stop_iteration(none);
                pon_gen_unwind(frame, 0)
            }
        }

        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            pon_err_clear();
            let frame = pon_gen_frame_alloc(0);
            let generator = pon_make_generator(raises_stop_iteration, frame, GeneratorKind::Generator.as_u8());
            let result = pon_gen_send(generator, ptr::null_mut());
            assert!(result.is_null());
            assert!(pon_err_occurred());
            let message = pon_err_message().unwrap_or_default();
            assert!(message.contains("RuntimeError"), "PEP 479 must convert to RuntimeError, got {message:?}");
            assert!(!pending_exception_matches(ExceptionKind::StopIteration), "StopIteration must not survive PEP 479");
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
    fn generator_object_is_its_own_iterator() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            pon_err_clear();
            let frame = pon_gen_frame_alloc(1);
            let generator = pon_make_generator(two_yields_body, frame, GeneratorKind::Generator.as_u8());
            assert_eq!(pon_get_iter(generator, ptr::null_mut()), generator);
            assert_eq!(format_object_for_print(pon_gen_send(generator, ptr::null_mut())).as_deref(), Ok("1"));
            assert_eq!(format_object_for_print(pon_gen_send(generator, pon_none())).as_deref(), Ok("2"));
            assert!(pon_gen_send(generator, pon_none()).is_null());
            assert!(pon_err_occurred());
            pon_err_clear();
        }
    }

    #[test]
    fn delegate_step_forwards_next_send_and_finish() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            pon_err_clear();

            // Outer frame supplies the payload; inner generator is the delegate.
            let outer = pon_gen_frame_alloc(0);
            let inner_frame = pon_gen_frame_alloc(1);
            let delegate = pon_make_generator(two_yields_body, inner_frame, GeneratorKind::Generator.as_u8());

            // First step: no payload => __next__.
            let first = pon_gen_delegate_step(outer, delegate);
            assert_eq!(format_object_for_print(first).as_deref(), Ok("1"));

            // Sent payload forwards to delegate.send().
            let sent = pon_const_int(9);
            {
                let _g = crate::sync::begin_critical_section(outer.cast::<PyObject>());
                crate::sync::store_heap_pointer(ptr::addr_of_mut!((*outer).sent_value), sent);
            }
            let second = pon_gen_delegate_step(outer, delegate);
            assert_eq!(format_object_for_print(second).as_deref(), Ok("2"));

            // Delegate finish: NULL + StopIteration(9); stop_value stashes it.
            let done = pon_gen_delegate_step(outer, delegate);
            assert!(done.is_null());
            let finish = pon_gen_stop_value();
            assert_eq!(finish, sent);
            assert_eq!(pon_gen_last_stop_value(), sent, "yield-from result must decode the delegation finish value");
            assert!(!pon_err_occurred());
        }
    }

    #[test]
    fn delegate_step_generator_exit_closes_delegate_and_reraises() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            pon_err_clear();

            let outer = pon_gen_frame_alloc(0);
            let inner_frame = pon_gen_frame_alloc(1);
            let delegate = pon_make_generator(two_yields_body, inner_frame, GeneratorKind::Generator.as_u8());
            assert_eq!(format_object_for_print(pon_gen_delegate_step(outer, delegate)).as_deref(), Ok("1"));

            let exit_exc = crate::abi::with_runtime(|runtime| {
                crate::abi::exc::alloc_exception_object(runtime, runtime.exception_types.generator_exit, ptr::null_mut(), ptr::null_mut()).unwrap()
            })
            .unwrap();
            {
                let _g = crate::sync::begin_critical_section(outer.cast::<PyObject>());
                crate::sync::store_heap_pointer(ptr::addr_of_mut!((*outer).thrown_exc), exit_exc);
            }
            let result = pon_gen_delegate_step(outer, delegate);
            assert!(result.is_null());
            assert!(pending_exception_matches(ExceptionKind::GeneratorExit), "GeneratorExit must re-raise after closing the delegate");
            assert_eq!((*inner_frame).resume_state, RESUME_FINISHED, "delegate must be closed");
            pon_err_clear();
        }
    }
}
