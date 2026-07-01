//! Generator object implementation.
//!
//! The runtime object is intentionally stackless: all suspended execution state
//! lives in the heap `PyFrame`, and resumption is a normal call to `resume`.

use core::ffi::c_int;
use core::mem;
use core::ptr;
use std::sync::{LazyLock, Mutex};

use pon_gc::TypeId;

use crate::abi::{PyFrame, r#gen::GenResumeFn};
use crate::intern;
use crate::object::{GetAttrFunc, PyAsyncMethods, PyObject, PyObjectHeader, PyType, SendFunc, as_object_ptr};
use crate::types::{method, type_};
use crate::types::frame::FRAME_STATE_EXHAUSTED;

/// GC type id reserved for generator objects in the WS-GEN family.
pub const TYPE_ID_GENERATOR: TypeId = TypeId(30);

/// Generator-kind discriminator stored in [`PyGenerator::kind`].
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeneratorKind {
    /// A synchronous Python generator.
    Generator = 0,
    /// A coroutine object produced by `async def`.
    Coroutine = 1,
    /// An asynchronous generator.
    AsyncGenerator = 2,
}

impl GeneratorKind {
    /// Converts the ABI byte into a known kind.
    #[must_use]
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Generator),
            1 => Some(Self::Coroutine),
            2 => Some(Self::AsyncGenerator),
            _ => None,
        }
    }

    /// Returns the ABI byte for this kind.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Boxed Python generator/coroutine payload.
#[repr(C)]
#[derive(Debug)]
pub struct PyGenerator {
    /// Standard boxed-object header at offset zero.
    pub header: PyObjectHeader,
    /// Compiled body resume function.
    pub resume: GenResumeFn,
    /// Heap frame holding the state index and locals.
    pub frame: *mut PyFrame,
    /// `0=generator`, `1=coroutine`, `2=async_generator`.
    pub kind: u8,
    /// Re-entrancy guard.
    pub running: bool,
    /// Set after `close` so late resumes raise `StopIteration(None)`.
    pub closed: bool,
    /// Exception scheduled by `throw` for the next resume-site check.
    pub pending_throw: *mut PyObject,
}

impl PyGenerator {
    /// Builds a generator payload around an already-allocated heap frame.
    #[must_use]
    pub fn new(ty: *const PyType, resume: GenResumeFn, frame: *mut PyFrame, kind: GeneratorKind) -> Self {
        Self {
            header: PyObjectHeader::new(ty),
            resume,
            frame,
            kind: kind.as_u8(),
            running: false,
            closed: false,
            pending_throw: ptr::null_mut(),
        }
    }

    /// Returns true when the object has no further resume point.
    #[must_use]
    pub unsafe fn is_exhausted(&self) -> bool {
        self.closed || self.frame.is_null() || unsafe { (*self.frame).state == FRAME_STATE_EXHAUSTED }
    }
}

static GENERATOR_TYPE: LazyLock<Mutex<Option<usize>>> = LazyLock::new(|| Mutex::new(None));
static GENERATOR_ASYNC_METHODS: LazyLock<Mutex<Option<usize>>> = LazyLock::new(|| Mutex::new(None));

/// Returns the process-lifetime generator type object, creating it if needed.
pub fn ensure_generator_type(type_type: *mut PyType) -> *mut PyType {
    let mut slot = GENERATOR_TYPE.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(ptr) = *slot {
        return ptr as *mut PyType;
    }

    let async_methods = ensure_generator_async_methods();
    let mut ty = PyType::new(type_type.cast_const(), "generator", mem::size_of::<PyGenerator>());
    ty.tp_iter = Some(generator_iter);
    ty.tp_iternext = Some(generator_next);
    ty.tp_getattro = Some(generator_getattro as GetAttrFunc);
    ty.tp_as_async = async_methods;
    ty.gc_type_id = TYPE_ID_GENERATOR.0 as usize;
    let ptr = Box::into_raw(Box::new(ty));
    *slot = Some(ptr as usize);
    ptr
}

fn ensure_generator_async_methods() -> *mut PyAsyncMethods {
    let mut slot = GENERATOR_ASYNC_METHODS.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(ptr) = *slot {
        return ptr as *mut PyAsyncMethods;
    }
    let mut methods = PyAsyncMethods::EMPTY;
    methods.am_send = Some(generator_am_send as SendFunc);
    let ptr = Box::into_raw(Box::new(methods));
    *slot = Some(ptr as usize);
    ptr
}

/// `iter(g) is g` for Python generators.
///
/// # Safety
/// `object` must be a boxed generator object.
pub unsafe extern "C" fn generator_iter(object: *mut PyObject) -> *mut PyObject {
    object
}

/// Implements `next(g)` by sending `None` into the generator.
///
/// # Safety
/// `object` must be a boxed generator object.
pub unsafe extern "C" fn generator_next(object: *mut PyObject) -> *mut PyObject {
    // SAFETY: `pon_none` follows the runtime NULL-sentinel discipline.
    let none = unsafe { crate::abi::pon_none() };
    if none.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: The iterator slot is installed only on generator objects.
    unsafe { crate::abi::r#gen::pon_gen_send(object, none) }
}

/// Async `am_send` bridge used by coroutine drivers.
///
/// # Safety
/// `object` must be a boxed generator/coroutine and `out` must be writable when
/// non-NULL.
pub unsafe extern "C" fn generator_am_send(object: *mut PyObject, value: *mut PyObject, out: *mut *mut PyObject) -> c_int {
    if out.is_null() {
        return -1;
    }
    // SAFETY: `out` is non-NULL and owned by the caller.
    unsafe {
        *out = ptr::null_mut();
    }
    // SAFETY: Delegates to the generator ABI helper.
    let result = unsafe { crate::abi::r#gen::pon_gen_send(object, value) };
    if result.is_null() {
        -1
    } else {
        // SAFETY: `out` is non-NULL and owned by the caller.
        unsafe {
            *out = result;
        }
        0
    }
}

/// Traces a generator allocation for the runtime GC.
///
/// # Safety
/// `object` must point at a live `PyGenerator` allocation.
pub unsafe extern "C" fn trace_generator(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    // SAFETY: The GC passes the allocation start for a registered PyGenerator.
    let generator = unsafe { &*object.cast::<PyGenerator>() };
    if !generator.frame.is_null() {
        visitor(generator.frame.cast::<u8>());
    }
    if !generator.pending_throw.is_null() {
        visitor(generator.pending_throw.cast::<u8>());
    }
}

/// Casts a boxed object to a generator payload without changing ownership.
///
/// # Safety
/// The caller must have established that `object` has the generator layout.
pub unsafe fn as_generator_mut(object: *mut PyObject) -> *mut PyGenerator {
    object.cast::<PyGenerator>()
}

/// Returns a boxed pointer to `generator`.
#[must_use]
pub fn as_generator_object(generator: *mut PyGenerator) -> *mut PyObject {
    as_object_ptr(generator)
}

unsafe fn argv_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Result<&'a [*mut PyObject], String> {
    if argv.is_null() && argc != 0 {
        return Err("argv pointer is null".to_owned());
    }
    Ok(if argc == 0 {
        &[]
    } else {
        // SAFETY: The caller supplies `argc` contiguous object-pointer entries.
        unsafe { core::slice::from_raw_parts(argv.cast_const(), argc) }
    })
}

unsafe fn exact_args<'a>(argv: *mut *mut PyObject, argc: usize, expected: usize, name: &str) -> Result<&'a [*mut PyObject], String> {
    if argc != expected {
        return Err(format!("{name} expected {expected} arguments, got {argc}"));
    }
    unsafe { argv_slice(argv, argc) }
}

unsafe extern "C" fn generator_send_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { exact_args(argv, argc, 2, "send") } {
        Ok(args) => args,
        Err(message) => return crate::abi::return_null_with_error(message),
    };
    // SAFETY: The bound method receiver and value occupy the two exact slots.
    unsafe { crate::abi::r#gen::pon_gen_send(args[0], args[1]) }
}

unsafe extern "C" fn generator_throw_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { exact_args(argv, argc, 2, "throw") } {
        Ok(args) => args,
        Err(message) => return crate::abi::return_null_with_error(message),
    };
    // SAFETY: The bound method receiver and exception occupy the two exact slots.
    unsafe { crate::abi::r#gen::pon_gen_throw(args[0], args[1]) }
}

unsafe extern "C" fn generator_close_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { exact_args(argv, argc, 1, "close") } {
        Ok(args) => args,
        Err(message) => return crate::abi::return_null_with_error(message),
    };
    // SAFETY: The bound method receiver is the only exact slot.
    unsafe { crate::abi::r#gen::pon_gen_close(args[0]) }
}

unsafe fn bound_generator_method(
    object: *mut PyObject,
    name: &'static str,
    arity: usize,
    code: *const u8,
) -> *mut PyObject {
    // SAFETY: `pon_make_function` allocates a normal runtime function object.
    let function = unsafe { crate::abi::pon_make_function(code, arity, intern::intern(name)) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match method::new_bound_method(function, object) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => crate::abi::return_null_with_error(message),
    }
}

/// Attribute surface for generator-family native methods.
///
/// # Safety
/// `object` must be a boxed generator/coroutine object and `name` must be a boxed
/// runtime string.
pub unsafe extern "C" fn generator_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = (unsafe { type_::unicode_text(name) }) else {
        return crate::abi::return_null_with_error("generator attribute name must be str");
    };
    match name {
        "send" => unsafe { bound_generator_method(object, "send", 2, generator_send_method as *const u8) },
        "throw" => unsafe { bound_generator_method(object, "throw", 2, generator_throw_method as *const u8) },
        "close" => unsafe { bound_generator_method(object, "close", 1, generator_close_method as *const u8) },
        _ => crate::abi::return_null_with_error(format!("attribute '{name}' was not found")),
    }
}

// ---------------------------------------------------------------------------
// Pin J0.1 — resumable generator frames (frozen contract).
// Design doc: plans/pon-pin-J01-generator-frames.md. Nothing below is wired
// into codegen or the HELPERS table yet; Track J1 implements against it and
// then deletes the eager-yield path above.
// ---------------------------------------------------------------------------

/// GC type id reserved for resumable generator frames in the WS-GEN family.
#[allow(dead_code, reason = "J0 pin: consumed by Track J1 in a later wave")]
pub const TYPE_ID_GEN_FRAME: TypeId = TypeId(33);

/// Resume state for a frame that has never been resumed (zeroed allocation).
#[allow(dead_code, reason = "J0 pin: consumed by Track J1 in a later wave")]
pub const RESUME_START: u32 = 0;
/// Sentinel resume state stored by the compiled body while it is executing.
#[allow(dead_code, reason = "J0 pin: consumed by Track J1 in a later wave")]
pub const RESUME_RUNNING: u32 = u32::MAX - 1;
/// Sentinel resume state for a finished (returned, unwound, or closed) frame.
#[allow(dead_code, reason = "J0 pin: consumed by Track J1 in a later wave")]
pub const RESUME_FINISHED: u32 = u32::MAX;

const _: () = {
    assert!(
        RESUME_FINISHED == FRAME_STATE_EXHAUSTED,
        "pin J0.1: RESUME_FINISHED must equal the legacy exhausted sentinel"
    );
    assert!(
        RESUME_RUNNING == RESUME_FINISHED - 1,
        "pin J0.1: both sentinels must satisfy `state >= RESUME_RUNNING`"
    );
};

/// Heap frame for one resumable generator/coroutine instance (pin J0.1).
///
/// One GC allocation of [`gen_frame_alloc_size`]`(slot_count)` bytes: this
/// fixed header followed inline by `slot_count` object-pointer spill slots.
/// Allocations are zeroed, so a fresh frame is already suspended at
/// [`RESUME_START`] with NULL payload fields and NULL slots.
#[repr(C)]
#[derive(Debug)]
pub struct GenFrame {
    /// Standard boxed-object header at offset zero.
    pub header: PyObjectHeader,
    /// State word: [`RESUME_START`], a suspend-point number `1..=N`,
    /// [`RESUME_RUNNING`], or [`RESUME_FINISHED`].  Written only by the
    /// compiled body; the driver reads it for its guards.
    pub resume_state: u32,
    /// Number of trailing spill slots; fixed at allocation.
    pub slot_count: u32,
    /// Value delivered by `send`; consumed-and-cleared by the resume block.
    pub sent_value: *mut PyObject,
    /// Exception scheduled by `throw`; consumed-and-cleared by the resume block.
    pub thrown_exc: *mut PyObject,
    /// Start of the trailing live-local spill array (`slot_count` entries).
    pub slots: [*mut PyObject; 0],
}

/// Byte size of the fixed [`GenFrame`] header preceding the trailing slot array.
#[allow(dead_code, reason = "J0 pin: consumed by Track J1 in a later wave")]
pub const GEN_FRAME_HEADER_SIZE: usize = mem::size_of::<GenFrame>();

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(mem::offset_of!(GenFrame, resume_state) == 16, "pin J0.1 layout");
    assert!(mem::offset_of!(GenFrame, slot_count) == 20, "pin J0.1 layout");
    assert!(mem::offset_of!(GenFrame, sent_value) == 24, "pin J0.1 layout");
    assert!(mem::offset_of!(GenFrame, thrown_exc) == 32, "pin J0.1 layout");
    assert!(mem::offset_of!(GenFrame, slots) == 40, "pin J0.1 layout");
    assert!(GEN_FRAME_HEADER_SIZE == 40, "pin J0.1 layout");
};

/// Total allocation size in bytes for a frame with `slot_count` spill slots.
///
/// Compiled code and the allocator must agree on this formula forever:
/// slot `k` lives at byte offset `GEN_FRAME_HEADER_SIZE + k * 8` (64-bit).
#[allow(dead_code, reason = "J0 pin: consumed by Track J1 in a later wave")]
#[must_use]
pub const fn gen_frame_alloc_size(slot_count: u32) -> usize {
    GEN_FRAME_HEADER_SIZE + slot_count as usize * mem::size_of::<*mut PyObject>()
}

impl GenFrame {
    /// Returns the address of spill slot `index` in `frame`'s trailing array.
    ///
    /// # Safety
    /// `frame` must point at a live `GenFrame` allocation and `index` must be
    /// below its `slot_count`.
    #[allow(dead_code, reason = "J0 pin: consumed by Track J1 in a later wave")]
    #[must_use]
    pub unsafe fn slot_ptr(frame: *mut Self, index: u32) -> *mut *mut PyObject {
        // SAFETY: The caller guarantees `frame` is live; `slots` names the
        // trailing array start without materializing a reference to it.
        unsafe {
            debug_assert!(index < (*frame).slot_count, "GenFrame slot index out of range");
            ptr::addr_of_mut!((*frame).slots).cast::<*mut PyObject>().add(index as usize)
        }
    }

    /// Reads spill slot `index`.
    ///
    /// # Safety
    /// Same contract as [`GenFrame::slot_ptr`].
    #[allow(dead_code, reason = "J0 pin: consumed by Track J1 in a later wave")]
    #[must_use]
    pub unsafe fn slot(frame: *mut Self, index: u32) -> *mut PyObject {
        // SAFETY: `slot_ptr` upholds bounds under the caller's contract.
        unsafe { *Self::slot_ptr(frame, index) }
    }

    /// Stores `value` into spill slot `index` through the GC write barrier.
    ///
    /// # Safety
    /// Same contract as [`GenFrame::slot_ptr`]; `value` must be a boxed object
    /// pointer or NULL.
    #[allow(dead_code, reason = "J0 pin: consumed by Track J1 in a later wave")]
    pub unsafe fn set_slot(frame: *mut Self, index: u32, value: *mut PyObject) {
        // SAFETY: `slot_ptr` upholds bounds under the caller's contract, and
        // every heap pointer store routes through the write barrier.
        unsafe { crate::sync::store_heap_pointer(Self::slot_ptr(frame, index), value) }
    }
}

/// Compiled resumable-body ABI (pin J0.1 §2).
///
/// One body per generator function.  The entry block loads `resume_state`,
/// stores [`RESUME_RUNNING`], and `br_table`-dispatches to the resume block for
/// that state; the send/throw payload travels in the frame's
/// `sent_value`/`thrown_exc` fields.  A non-NULL return is the yielded value;
/// NULL means finished (`StopIteration` pending on return, the escaping
/// exception pending otherwise) with `resume_state == RESUME_FINISHED`.
#[allow(dead_code, reason = "J0 pin: consumed by Track J1 in a later wave")]
pub type GenResumeBodyFn = unsafe extern "C" fn(frame: *mut GenFrame) -> *mut PyObject;

/// Traces a [`GenFrame`] allocation for the runtime GC (pin J0.1 §1.2).
///
/// Conservative body scan: visits `sent_value`, `thrown_exc`, then every one of
/// the `slot_count` trailing words regardless of liveness at the current resume
/// state.  The collector classifies each reported word against its allocation
/// table, so NULL, stale, or non-pointer slot contents are ignored rather than
/// dereferenced.  Registered with `finalize: None` — a frame is one allocation
/// and owns nothing outside it.
///
/// # Safety
/// `object` must point at a live `GenFrame` allocation whose trailing array has
/// `slot_count` entries.
#[allow(dead_code, reason = "J0 pin: consumed by Track J1 in a later wave")]
pub unsafe extern "C" fn trace_gen_frame(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    let frame = object.cast::<GenFrame>();
    // SAFETY: The GC passes the allocation start for a registered GenFrame.
    let (sent, thrown, slot_count) = unsafe { ((*frame).sent_value, (*frame).thrown_exc, (*frame).slot_count) };
    if !sent.is_null() {
        visitor(sent.cast::<u8>());
    }
    if !thrown.is_null() {
        visitor(thrown.cast::<u8>());
    }
    // SAFETY: `slots` names the trailing array start; the loop is bounded by
    // the `slot_count` recorded at allocation.
    let base = unsafe { ptr::addr_of_mut!((*frame).slots) }.cast::<*mut PyObject>();
    for index in 0..slot_count as usize {
        // SAFETY: `index < slot_count` keeps the read inside the allocation.
        let value = unsafe { *base.add(index) };
        if !value.is_null() {
            visitor(value.cast::<u8>());
        }
    }
}

/// Allocates a zeroed resumable generator frame with `slot_count` spill slots.
///
/// Frozen contract (pin J0.1 §1.1): the implementation registers
/// [`TYPE_ID_GEN_FRAME`] (trace = [`trace_gen_frame`], `finalize: None`) in
/// `ensure_gen_runtime`, allocates via
/// `runtime.heap.alloc(gen_frame_alloc_size(slot_count), TYPE_ID_GEN_FRAME)`,
/// and writes only `header.ob_type` and `slot_count` — zeroed memory already
/// encodes [`RESUME_START`], NULL payload fields, and NULL slots.  Follows the
/// runtime NULL-sentinel discipline; the exported `no_mangle` symbol lands with
/// the J1 implementation in `abi/gen.rs`.
///
/// # Safety
/// The runtime must be initialized before the first call.
#[allow(dead_code, reason = "J0 pin: consumed by Track J1 in a later wave")]
pub unsafe extern "C" fn pon_gen_frame_alloc(slot_count: u32) -> *mut GenFrame {
    let _ = slot_count;
    crate::abi::return_null_with_error("pon_gen_frame_alloc is a J0.1 pin stub; Track J1 supplies the implementation")
        .cast::<GenFrame>()
}

/// Resumes a generator/coroutine body once: the shared driver core behind
/// `send`, `throw`, and `close`.
///
/// Frozen contract (pin J0.1 §4.1): at most one of `sent`/`thrown` is non-NULL
/// (both NULL means `next(g)`, i.e. send `None`).  The driver only reads
/// `resume_state`: [`RESUME_RUNNING`] ⇒ ValueError ("generator already
/// executing"; kind-dispatched wording for coroutines), [`RESUME_FINISHED`] ⇒
/// `StopIteration(None)` for send / re-raise of `thrown` for throw, non-`None`
/// send at [`RESUME_START`] ⇒ TypeError.  Otherwise it writes the payload into
/// `sent_value`/`thrown_exc` (write-barriered, under the generator's critical
/// section), pushes the frame on the thread-state frame stack, clears the error
/// state, and calls the compiled [`GenResumeBodyFn`] — the only body call site.
/// Returns the yielded value, or NULL with the pending exception
/// (`StopIteration` ⇒ normal exhaustion).
///
/// # Safety
/// `generator` must be a boxed generator/coroutine object; `sent` and `thrown`
/// must each be a valid boxed object or NULL.
#[allow(dead_code, reason = "J0 pin: consumed by Track J1 in a later wave")]
pub unsafe extern "C" fn pon_gen_resume(generator: *mut PyObject, sent: *mut PyObject, thrown: *mut PyObject) -> *mut PyObject {
    let _ = (generator, sent, thrown);
    crate::abi::return_null_with_error("pon_gen_resume is a J0.1 pin stub; Track J1 supplies the implementation")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_object(addr: usize) -> *mut PyObject {
        ptr::without_provenance_mut::<PyObject>(addr)
    }

    #[test]
    fn gen_frame_layout_matches_pin() {
        assert_eq!(GEN_FRAME_HEADER_SIZE, mem::size_of::<GenFrame>());
        assert_eq!(mem::align_of::<GenFrame>(), mem::align_of::<*mut PyObject>());
        assert_eq!(gen_frame_alloc_size(0), GEN_FRAME_HEADER_SIZE);
        assert_eq!(gen_frame_alloc_size(3), GEN_FRAME_HEADER_SIZE + 3 * mem::size_of::<*mut PyObject>());
    }

    #[test]
    fn resume_state_sentinels_are_frozen() {
        assert_eq!(RESUME_START, 0);
        assert_eq!(RESUME_RUNNING, u32::MAX - 1);
        assert_eq!(RESUME_FINISHED, u32::MAX);
        assert_eq!(RESUME_FINISHED, FRAME_STATE_EXHAUSTED);
        assert_eq!(TYPE_ID_GEN_FRAME, TypeId(33));
    }

    #[test]
    fn trace_gen_frame_visits_payload_fields_and_every_slot() {
        const SLOTS: u32 = 3;
        // Zeroed u64 backing mirrors the heap's zeroed, pointer-aligned block.
        let mut backing = vec![0_u64; gen_frame_alloc_size(SLOTS).div_ceil(8)];
        let frame = backing.as_mut_ptr().cast::<GenFrame>();

        let sent = fake_object(0x10);
        let thrown = fake_object(0x20);
        let first = fake_object(0x30);
        let last = fake_object(0x40);

        // SAFETY: `backing` is a live, zeroed, aligned block sized for SLOTS.
        unsafe {
            (*frame).slot_count = SLOTS;
            (*frame).sent_value = sent;
            (*frame).thrown_exc = thrown;
            GenFrame::set_slot(frame, 0, first);
            // Slot 1 stays NULL and must be skipped by the tracer.
            GenFrame::set_slot(frame, 2, last);
            assert_eq!(GenFrame::slot(frame, 0), first);
            assert_eq!(GenFrame::slot(frame, 1), ptr::null_mut());
            assert_eq!(GenFrame::slot(frame, 2), last);
        }

        let mut seen = Vec::new();
        // SAFETY: `frame` is fully initialized above; the tracer never
        // dereferences the visited words.
        unsafe {
            trace_gen_frame(frame.cast::<u8>(), &mut |child| seen.push(child));
        }
        assert_eq!(seen, vec![sent.cast::<u8>(), thrown.cast::<u8>(), first.cast::<u8>(), last.cast::<u8>()]);
    }
}
