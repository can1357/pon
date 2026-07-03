//! C ABI helpers exported by the Phase-A runtime.
//!
//! The [`HELPERS`] table is the single source of truth for later codegen and JIT
//! import declarations: symbol names, Rust entrypoint addresses, parameter
//! shapes, and return types all live here.

pub mod attr;
pub mod builtins;
pub use builtins::pon_load_builtin;
pub mod call;
pub mod exc;
pub use exc::{
    pon_exc_fetch, pon_exc_group_split, pon_exc_matches, pon_exc_restore, pon_raise, pon_raise_attribute_error,
    pon_raise_index_error, pon_raise_key_error, pon_raise_stop_iteration, pon_raise_type_error, pon_raise_value_error,
    pon_reraise, raise_import_error_text,
};
pub mod format;
pub mod r#gen;
pub mod import;
pub mod iter;
pub mod map;
pub mod match_;
pub mod number;
pub mod object;
pub mod seq;
pub mod str_;
pub use iter::{pon_get_iter, pon_iter_next};
pub use number::{pon_binary_op, pon_const_bool, pon_unary_op};
pub use object::{pon_del_attr, pon_get_attr, pon_is_true, pon_rich_compare, pon_set_attr, pon_subscript_get};
pub use crate::aot_entry::{pon_aot_entry, pon_err_report_uncaught, pon_threadstate_capture_stack_base};
pub use crate::thread::{
    pon_gc_safe_region_enter, pon_gc_safe_region_leave, pon_thread_attach, pon_thread_detach, pon_thread_start_new,
};
pub use crate::sys::{pon_io_flush_std, pon_sys_set_argv};

use std::cell::RefCell;
use std::ffi::c_void;
use std::collections::HashMap;
use std::io::{self, Write};
use std::mem;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::{Condvar, LazyLock, Mutex, MutexGuard};
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use pon_gc::{GcTypeInfo, Heap, RootSource, TypeId};

use crate::builtins as runtime_builtins;
use crate::intern::resolve;
use crate::feedback::{FeedbackCell, FeedbackVec, GlobalIC, TypeTag};
use crate::object::{
    NewFunc, PyCodeFn, PyFunction, PyLong, PyNone, PyObject, PyObjectHeader, PyType, PyUnicode, TIER_STATE_DEFERRED,
    TIER_STATE_DISABLED, TIER_STATE_QUEUED, TIER_STATE_TIER0, TIER_STATE_TIER1, as_object_ptr, is_exact_type,
};
use crate::types::{bool_, bytearray_, bytes_, classmethod, complex_, float, function, int, memoryview, type_, typealias};
use crate::types::exc::{ExceptionTypeSet, PyBaseException, is_exception_subclass, trace_base_exception, trace_exception_group};
use crate::thread_state::{pon_err_clear, pon_err_occurred, pon_err_set, thread_state_lock};

const TYPE_ID_TYPE: TypeId = TypeId(1);

/// Function-entry hotness threshold for the runtime-side tier-up probe.  The
/// triggering call reloads the dispatch cell after this probe; keeping a small
/// hotness floor avoids synchronous compilation in one-shot benchmark kernels.
pub const TIER1_CALL_THRESHOLD: u32 = 16;
/// Deferred functions must become hot enough to amortize synchronous tier-1
/// compilation before the runtime asks the backend to try them again.
pub const TIER1_DEFERRED_CALL_THRESHOLD: u32 = 16;
/// Loop back-edge hotness threshold for the runtime-side tier-up probe.
pub const TIER1_LOOP_THRESHOLD: u32 = 10_000;

/// Runtime-to-tier-up backend hook.  The concrete installer lives outside
/// `pon-runtime`; the runtime calls through this pointer only after CAS-queuing.
///
/// `reason == 0` means function-entry heat; `reason > 0` means a loop back-edge
/// crossed the threshold and names IR block `reason - 1` as the OSR header.
pub type TierUpHook = unsafe extern "C" fn(*mut PyFunction, u32);

/// Visitor used by the tier-up root hook to publish pinned function objects to GC.
pub type TierUpRootVisit = unsafe extern "C" fn(*mut u8, *mut c_void);
/// Optional root provider installed by the JIT while a tier-up driver is active.
pub type TierUpRootHook = unsafe extern "C" fn(TierUpRootVisit, *mut c_void);

static TIERUP_HOOK: AtomicPtr<()> = AtomicPtr::new(ptr::null_mut());
static TIERUP_ROOT_HOOK: AtomicPtr<()> = AtomicPtr::new(ptr::null_mut());
/// Identity anchor for the Python-visible `object.__new__` staticmethod
/// descriptor: `call_type_from_argv` must keep taking the default `tp_new`
/// slot path when the only `__new__` in a class's MRO is object's generic
/// allocator (builtin containers rely on their slot allocators).
static OBJECT_DUNDER_NEW_DESCRIPTOR: AtomicPtr<PyObject> = AtomicPtr::new(ptr::null_mut());
/// Identity anchor for the permissive `object.__init__` carrier: keyword
/// call sites treat it as "no `__init__` defined" (CPython raises
/// `C() takes no arguments`; the no-op would silently swallow keywords).
static OBJECT_DUNDER_INIT_CARRIER: AtomicPtr<PyObject> = AtomicPtr::new(ptr::null_mut());
/// Identity anchors for object's default `__repr__`/`__str__` carriers:
/// Python-level dispatch (`builtins_mod::try_repr_text`/`try_str_text`)
/// treats a hook that resolves to these as "no user override" and keeps the
/// native fallback text instead of re-entering the terminus.
static OBJECT_DUNDER_REPR_CARRIER: AtomicPtr<PyObject> = AtomicPtr::new(ptr::null_mut());
static OBJECT_DUNDER_STR_CARRIER: AtomicPtr<PyObject> = AtomicPtr::new(ptr::null_mut());

pub(crate) fn object_dunder_repr_carrier() -> *mut PyObject {
    OBJECT_DUNDER_REPR_CARRIER.load(Ordering::Acquire)
}

pub(crate) fn object_dunder_str_carrier() -> *mut PyObject {
    OBJECT_DUNDER_STR_CARRIER.load(Ordering::Acquire)
}

const DEOPT_THRASH_THRESHOLD: u32 = 64;
const DEOPT_BACKOFF_MAX_SHIFT: u32 = 6;
const DEOPT_PIN_EPOCH: u8 = 8;

#[derive(Clone, Copy)]
struct CurrentCall {
    function: *mut PyFunction,
    argv: *mut *mut PyObject,
    argc: usize,
    /// `pon_current_line` value at push time — the caller's active statement
    /// line.  Restored on pop so a raise later in the caller's statement (after
    /// the callee returned) attributes to the caller's line again, and read by
    /// traceback capture as the caller frame's "last executed" line.
    caller_line: u32,
}

thread_local! {
    static CURRENT_FUNCTION_STACK: RefCell<Vec<CurrentCall>> = RefCell::new(Vec::new());
}

/// Import symbol through which generated code stores the current source line.
pub const CURRENT_LINE_SYMBOL: &str = "pon_current_line";

/// Current 1-based Python source line; `0` means "no line recorded yet".
///
/// Generated code stores to this cell directly (no helper call) at every
/// statement-line transition; traceback capture reads it as the raise-site
/// line.  The cell is process-global like the Phase-A thread state: compiled
/// Python executes single-threaded today, and [`CurrentFunctionGuard`]
/// save/restore keeps it frame-accurate across compiled calls.  A
/// free-threading build needs a per-thread cell (TLS data symbol or a helper
/// call) before generated code may run on multiple threads.
#[unsafe(export_name = "pon_current_line")]
static CURRENT_LINE_CELL: AtomicU32 = AtomicU32::new(0);

/// Source line most recently recorded by generated code, `0` when unknown.
#[must_use]
pub(crate) fn current_line() -> u32 {
    CURRENT_LINE_CELL.load(Ordering::Relaxed)
}

/// Overwrites the recorded source line (guard restore path and tests).
pub(crate) fn set_current_line(line: u32) {
    CURRENT_LINE_CELL.store(line, Ordering::Relaxed);
}

/// Address of the current-line cell for JIT data-symbol registration.
#[must_use]
pub fn current_line_cell_address() -> *const u8 {
    (&raw const CURRENT_LINE_CELL).cast::<u8>()
}
// ─── J0.3 GlobalIC guard: process-wide namespace version ────────────────────
//
// `pon_load_global` resolves through layered stores (the defining module's
// `PyModuleObject.attrs`, then the active module's, then the flat
// `Runtime.globals` map holding ONLY builtins — module-scope stores never
// write it), none of which is a versioned dict object yet.
// Until N4 lands the real module-dict representation (J0.3 §5), GlobalIC
// guards on ONE process-wide counter that is bumped by
//   1. every mutation of either store (insert/replace/delete), and
//   2. every module-execution context switch (`begin/end_module_execution`),
//      because the switch changes which overlay `pon_load_global` consults.
// A guard match therefore proves a recorded resolution would replay
// identically.  Coarse (any store invalidates every GlobalIC) but exactly
// sound, and steady-state module code — the hot case — never bumps it.
//
// Seeded to 1: `FeedbackCell::record_global` refuses `dict_version == 0`
// (the empty-cell sentinel).  The counter address doubles as the cell's
// identity word so a cell can never be confused with a future per-dict
// version word living at a real dict address.
static NAMESPACE_VERSION: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(1);

/// Current global-namespace version (Relaxed; same soundness argument as
/// `PyType::version` — the counter is a pure invalidation fact).
#[inline]
#[must_use]
pub fn namespace_version() -> u32 {
    NAMESPACE_VERSION.load(Ordering::Relaxed)
}

/// Records a mutation of the module-attr overlay or the flat globals map
/// (or a module-context switch).  Every such site MUST call this AFTER the
/// write, mirroring the J0.3 type-mutation discipline.
#[inline]
pub fn bump_namespace_version() {
    NAMESPACE_VERSION.fetch_add(1, Ordering::Relaxed);
}

/// Stable identity word for GlobalIC records guarded by [`namespace_version`].
#[inline]
fn namespace_identity() -> usize {
    (&raw const NAMESPACE_VERSION) as usize
}

/// Crate-test accessor for the GlobalIC identity word.
#[cfg(test)]
pub(crate) fn namespace_identity_for_tests() -> usize {
    namespace_identity()
}
const TYPE_ID_LONG: TypeId = TypeId(2);
const TYPE_ID_UNICODE: TypeId = TypeId(3);
const TYPE_ID_FUNCTION: TypeId = TypeId(4);
const TYPE_ID_NONE: TypeId = TypeId(5);
pub(crate) const TYPE_ID_EXCEPTION: TypeId = TypeId(6);
const TYPE_ID_NOT_IMPLEMENTED: TypeId = TypeId(7);
pub(crate) const TYPE_ID_EXCEPTION_GROUP: TypeId = TypeId(8);
const TYPE_ID_ELLIPSIS: TypeId = TypeId(9);

/// Compiled function metadata shared across Phase-B call, frame, and tier-up helpers.
///
/// The Phase-A `pon_make_function` helper keeps accepting a raw code pointer.  New
/// Phase-B helper families pass `CodeInfo` by pointer so the ABI has one stable
/// home for entrypoint, argument-binding, local-frame, and feedback-vector shape.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodeInfo {
    /// Compiled entrypoint address using the compiled-code calling convention.
    pub entry: *const u8,
    /// Optional parameter-layout descriptor; NULL means no Phase-B binding data.
    pub params: *const ParamSpec,
    /// Interned function name used for diagnostics and traceback records.
    pub name_interned: u32,
    /// Number of local/temp slots that must be addressable from a frame.
    pub n_locals: u32,
    /// Number of reserved feedback cells in the per-function feedback vector.
    pub n_feedback: u32,
    /// Forward-compatible code flags; bit assignments are owned by lowering.
    pub flags: u32,
}

/// Argument-binding descriptor referenced by [`CodeInfo`].
///
/// Names are interned `u32` ids in source order.  Counts are split so positional-
/// only, positional-or-keyword, keyword-only, `*args`, and `**kwargs` can be
/// represented without changing the helper ABI when full binding lands.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParamSpec {
    /// Pointer to `total_param_count` interned parameter names, or NULL.
    pub names: *const u32,
    /// Total number of named parameters in `names`.
    pub total_param_count: u32,
    /// Leading positional-only parameter count.
    pub positional_only_count: u32,
    /// Positional-or-keyword parameter count after positional-only parameters.
    pub positional_count: u32,
    /// Keyword-only parameter count after positional parameters.
    pub keyword_only_count: u32,
    /// Interned `*args` parameter name, or `0` when absent.
    pub varargs_name: u32,
    /// Interned `**kwargs` parameter name, or `0` when absent.
    pub varkw_name: u32,
}

/// Python frame layout reserved for tracebacks, generators, precise roots, and
/// Phase-D tier-up probes.
#[repr(C)]
#[derive(Debug)]
pub struct PyFrame {
    /// Standard boxed-object header at offset zero.
    pub header: PyObjectHeader,
    /// Resume point; `0` means not started and `u32::MAX` means exhausted.
    pub state: u32,
    /// Number of entries addressable through `locals`.
    pub n_locals: u32,
    /// GC-managed array of frame-resident locals and temporaries.
    pub locals: *mut *mut PyObject,
    /// Delegate sub-iterator for `yield from`/`await`, or NULL.
    pub parent: *mut PyObject,
    /// Saved exception state across suspension, or NULL.
    pub exc_state: *mut PyObject,
    /// 1-based source line for traceback-synthesized frames, `0` when unknown.
    ///
    /// Only traceback capture writes it.  The runtime `frame` PyType is shared
    /// by `PyFrame` and resumable `GenFrame` allocations, so type-dispatched
    /// slots (`frame_getattro`) must NOT read this field — it does not exist
    /// past the shared header on generator frames.
    pub line: u32,
}

/// Active exception-handler record stored in [`PonThreadState`](crate::PonThreadState).
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HandlerInfo {
    /// Frame that owns this handler, or NULL for synthetic Phase-A records.
    pub frame: *mut PyFrame,
    /// Lowering-owned handler target block or bytecode offset.
    pub target: u32,
    /// Operand-stack depth to restore before entering the handler.
    pub stack_depth: u32,
    /// Handler kind; values are owned by the exception-lowering workstream.
    pub kind: u8,
    /// Reserved padding that must be zeroed by producers.
    pub reserved: [u8; 3],
}

impl HandlerInfo {
    /// Builds a handler-chain entry with reserved bytes zeroed.
    #[must_use]
    pub const fn new(frame: *mut PyFrame, target: u32, stack_depth: u32, kind: u8) -> Self {
        Self {
            frame,
            target,
            stack_depth,
            kind,
            reserved: [0; 3],
        }
    }
}

/// Raw f-string part consumed by the future string helper family.
///
/// A part is either a literal byte range (`value == NULL`) or a formatted value.
/// `conversion` follows CPython's `!s`/`!r`/`!a` encoding; zero means no explicit
/// conversion.  `format_spec` may be NULL.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FStrPartRaw {
    /// Formatted value, or NULL when this part is a literal.
    pub value: *mut PyObject,
    /// UTF-8 literal bytes for literal parts, or NULL for value parts.
    pub literal: *const u8,
    /// Byte length of `literal`.
    pub literal_len: usize,
    /// Conversion marker (`0`, `b's'`, `b'r'`, or `b'a'`).
    pub conversion: u8,
    /// Reserved padding that must be zeroed by producers.
    pub reserved: [u8; 7],
    /// Optional already-boxed format specifier, or NULL.
    pub format_spec: *mut PyObject,
}

/// Raw template-string part consumed by the string helper family.
///
/// For interpolation parts, `literal`/`literal_len` carry the UTF-8 source
/// spelling of the expression.  `expression_interned` remains available for
/// future intern-table producers and is zero for baseline codegen.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TStrPartRaw {
    /// Interpolated value, or NULL when this part is a literal.
    pub value: *mut PyObject,
    /// UTF-8 literal bytes for raw literal parts; for interpolation parts this
    /// is the source spelling stored on `Interpolation.expression`.
    pub literal: *const u8,
    /// Byte length of `literal`.
    pub literal_len: usize,
    /// Interned interpolation expression/name fallback, or `0` when absent.
    pub expression_interned: u32,
    /// Conversion marker (`0`, `b's'`, `b'r'`, or `b'a'`).
    pub conversion: u8,
    /// Reserved padding that must be zeroed by producers.
    pub reserved: [u8; 3],
    /// Optional already-boxed format specifier, or NULL.
    pub format_spec: *mut PyObject,
}

/// ABI-level type descriptor used by [`HelperDecl`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbiTy {
    /// C `i32`.
    I32,
    /// C/Rust `i64`.
    I64,
    /// C/Rust `isize`.
    ISize,
    /// C/Rust `u16`.
    U16,
    /// C/Rust `u32`.
    U32,
    /// C/Rust `usize`.
    Usize,
    /// C/Rust `u8`.
    U8,
    /// C/Rust `f64`.
    F64,
    /// `*const u8`.
    ConstU8Ptr,
    /// `*const u32`.
    ConstU32Ptr,
    /// Compiled code entry pointer.
    CodePtr,
    /// `*const CodeInfo`.
    CodeInfoPtr,
    /// `*const FStrPartRaw`.
    FStrPartPtr,
    /// `*const TStrPartRaw`.
    TStrPartPtr,
    /// Generator/coroutine resume function pointer.
    GenResumePtr,
    /// `*mut PyFrame`.
    PyFramePtr,
    /// `*mut PyObject`.
    PyObjectPtr,
    /// `*mut *mut PyObject`.
    PyObjectPtrPtr,
    /// `*mut FeedbackCell`.
    FeedbackCellPtr,
    /// `*mut PonThreadState`.
    ThreadStatePtr,
}

/// One exported helper declaration for codegen and JIT import binding.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct HelperDecl {
    /// Exact exported symbol name.
    pub symbol: &'static str,
    /// Runtime entrypoint address.
    pub address: *const (),
    /// Parameter ABI types in call order.
    pub params: &'static [AbiTy],
    /// Return ABI type.
    pub ret: AbiTy,
}

unsafe impl Sync for HelperDecl {}

const PARAMS_CONST_INT: &[AbiTy] = &[AbiTy::I64];
const PARAMS_CONST_STR: &[AbiTy] = &[AbiTy::ConstU8Ptr, AbiTy::Usize];
const PARAMS_BINARY_ADD: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::PyObjectPtr];
const PARAMS_OP_OBJ: &[AbiTy] = &[AbiTy::U8, AbiTy::PyObjectPtr, AbiTy::FeedbackCellPtr];
const PARAMS_OP_OBJ_OBJ: &[AbiTy] = &[AbiTy::U8, AbiTy::PyObjectPtr, AbiTy::PyObjectPtr, AbiTy::FeedbackCellPtr];
const PARAMS_OBJ: &[AbiTy] = &[AbiTy::PyObjectPtr];
const PARAMS_OBJ_FEEDBACK: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::FeedbackCellPtr];
const PARAMS_OBJ_NAME: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::U32];
const PARAMS_OBJ_NAME_FEEDBACK: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::U32, AbiTy::FeedbackCellPtr];
const PARAMS_OBJ_NAME_OBJ: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::U32, AbiTy::PyObjectPtr];
const PARAMS_OBJ_OBJ_FEEDBACK: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::PyObjectPtr, AbiTy::FeedbackCellPtr];
const PARAMS_CALL: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::PyObjectPtrPtr, AbiTy::Usize];
const PARAMS_LOAD_BUILD_CLASS: &[AbiTy] = &[];
const PARAMS_BUILD_CLASS: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::U32, AbiTy::PyObjectPtrPtr, AbiTy::Usize];
const PARAMS_BUILD_CLASS_EX: &[AbiTy] = &[
    AbiTy::PyObjectPtr,
    AbiTy::U32,
    AbiTy::PyObjectPtrPtr,
    AbiTy::Usize,
    AbiTy::ConstU32Ptr,
    AbiTy::PyObjectPtrPtr,
    AbiTy::Usize,
];
const PARAMS_BUILD_CLASS_FULL: &[AbiTy] = &[
    AbiTy::PyObjectPtr,
    AbiTy::U32,
    AbiTy::PyObjectPtr,
    AbiTy::ConstU32Ptr,
    AbiTy::PyObjectPtrPtr,
    AbiTy::Usize,
    AbiTy::PyObjectPtr,
];
const PARAMS_LOAD_GLOBAL: &[AbiTy] = &[AbiTy::U32, AbiTy::FeedbackCellPtr];
const PARAMS_LOAD_BUILTIN: &[AbiTy] = &[AbiTy::U32];
const PARAMS_LOAD_NAME: &[AbiTy] = &[AbiTy::U32];
const PARAMS_PRINT: &[AbiTy] = &[AbiTy::PyObjectPtr];
const PARAMS_MAKE_FUNCTION: &[AbiTy] = &[AbiTy::CodePtr, AbiTy::Usize, AbiTy::U32];
const PARAMS_STORE_GLOBAL: &[AbiTy] = &[AbiTy::U32, AbiTy::PyObjectPtr];
const PARAMS_STORE_NAME: &[AbiTy] = &[AbiTy::U32, AbiTy::PyObjectPtr];
const PARAMS_NONE: &[AbiTy] = &[];
const PARAMS_RUNTIME_INIT: &[AbiTy] = &[];
const PARAMS_THREAD_START_NEW: &[AbiTy] = &[AbiTy::CodePtr, AbiTy::PyObjectPtr];
const PARAMS_RAISE: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::PyObjectPtr];
const PARAMS_RAISE_MESSAGE: &[AbiTy] = &[AbiTy::ConstU8Ptr, AbiTy::Usize];
const PARAMS_RAISE_KEY_ERROR: &[AbiTy] = &[AbiTy::PyObjectPtr];
const PARAMS_RAISE_ATTRIBUTE_ERROR: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::U32];
const PARAMS_RAISE_STOP_ITERATION: &[AbiTy] = &[AbiTy::PyObjectPtr];
const PARAMS_EXC_MATCHES: &[AbiTy] = &[AbiTy::PyObjectPtr];
const PARAMS_EXC_FETCH: &[AbiTy] = &[];
const PARAMS_EXC_RESTORE: &[AbiTy] = &[AbiTy::PyObjectPtr];
const PARAMS_EXC_GROUP_SPLIT: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::PyObjectPtrPtr];
const PARAMS_CONST_FLOAT: &[AbiTy] = &[AbiTy::F64];
const PARAMS_CONST_COMPLEX: &[AbiTy] = &[AbiTy::F64, AbiTy::F64];
const PARAMS_CONST_BOOL: &[AbiTy] = &[AbiTy::I32];
const PARAMS_STR_REPEAT: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::ISize];
const PARAMS_FORMAT_VALUE: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::U8, AbiTy::PyObjectPtr];
const PARAMS_FSTR_PARTS: &[AbiTy] = &[AbiTy::FStrPartPtr, AbiTy::Usize];
const PARAMS_TSTR_PARTS: &[AbiTy] = &[AbiTy::TStrPartPtr, AbiTy::Usize];
const PARAMS_STR_METHOD: &[AbiTy] = &[AbiTy::U16, AbiTy::PyObjectPtr, AbiTy::PyObjectPtrPtr, AbiTy::Usize];
const PARAMS_OBJ_ARRAY: &[AbiTy] = &[AbiTy::PyObjectPtrPtr, AbiTy::Usize];
const PARAMS_UNPACK_SEQ: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::Usize, AbiTy::FeedbackCellPtr];
const PARAMS_BUILD_RANGE: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::PyObjectPtr, AbiTy::PyObjectPtr];
const PARAMS_SEQ_SET_ITEM: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::PyObjectPtr, AbiTy::PyObjectPtr];
const PARAMS_UNPACK_EX: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::Usize, AbiTy::Usize];
const PARAMS_CALL_FEEDBACK: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::PyObjectPtrPtr, AbiTy::Usize, AbiTy::FeedbackCellPtr];
const PARAMS_CALL_EX: &[AbiTy] = &[
    AbiTy::PyObjectPtr,
    AbiTy::PyObjectPtrPtr,
    AbiTy::Usize,
    AbiTy::PyObjectPtr,
    AbiTy::ConstU32Ptr,
    AbiTy::PyObjectPtrPtr,
    AbiTy::Usize,
    AbiTy::PyObjectPtr,
    AbiTy::FeedbackCellPtr,
];
const PARAMS_MAKE_FUNCTION_FULL: &[AbiTy] = &[
    AbiTy::CodeInfoPtr,
    AbiTy::PyObjectPtrPtr,
    AbiTy::Usize,
    AbiTy::ConstU32Ptr,
    AbiTy::PyObjectPtrPtr,
    AbiTy::Usize,
    AbiTy::ConstU32Ptr,
    AbiTy::PyObjectPtrPtr,
    AbiTy::Usize,
];
const PARAMS_FUNCTION_SET_CLOSURE: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::PyObjectPtrPtr, AbiTy::Usize];
const PARAMS_CELL_SET: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::PyObjectPtr];
const PARAMS_INDEX: &[AbiTy] = &[AbiTy::Usize];
const PARAMS_IMPORT_NAME: &[AbiTy] = &[AbiTy::U32, AbiTy::ConstU32Ptr, AbiTy::Usize, AbiTy::U32];
const PARAMS_MATCH_CLASS: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::PyObjectPtr, AbiTy::Usize, AbiTy::ConstU32Ptr, AbiTy::Usize];
const PARAMS_MATCH_KEYS: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::PyObjectPtrPtr, AbiTy::Usize];
const PARAMS_MATCH_LEN: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::Usize, AbiTy::U8];
const PARAMS_EXC_HANDLER: &[AbiTy] = &[AbiTy::U32, AbiTy::U32, AbiTy::U8];
const PARAMS_GEN_MAKE_FRAME: &[AbiTy] = &[AbiTy::U32];
const PARAMS_GEN_FRAME: &[AbiTy] = &[AbiTy::PyFramePtr];
const PARAMS_GEN_FINISH: &[AbiTy] = &[AbiTy::PyFramePtr, AbiTy::PyObjectPtr];
const PARAMS_GEN_UNWIND: &[AbiTy] = &[AbiTy::PyFramePtr, AbiTy::U8];
const PARAMS_GEN_DELEGATE_STEP: &[AbiTy] = &[AbiTy::PyFramePtr, AbiTy::PyObjectPtr];
const PARAMS_MAKE_GENERATOR: &[AbiTy] = &[AbiTy::GenResumePtr, AbiTy::PyFramePtr, AbiTy::U8];
const PARAMS_FUNCTION_SET_ANNOTATE: &[AbiTy] = &[AbiTy::PyObjectPtr, AbiTy::PyObjectPtr];
const PARAMS_MAKE_TYPE_ALIAS: &[AbiTy] = &[AbiTy::U32, AbiTy::PyObjectPtr];
const PARAMS_MAKE_TYPEVAR: &[AbiTy] = &[AbiTy::U32];
const PARAMS_OSR_POLL: &[AbiTy] = &[AbiTy::U32];
const PARAMS_DEOPT_NOTE: &[AbiTy] = &[AbiTy::PyObjectPtr];

/// Exported helper table consumed by later codegen/JIT stages.
pub static HELPERS: &[HelperDecl] = &[
    HelperDecl {
        symbol: "pon_const_int",
        address: pon_const_int as *const (),
        params: PARAMS_CONST_INT,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_const_bigint",
        address: number::pon_const_bigint as *const (),
        params: PARAMS_CONST_STR,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_const_str",
        address: pon_const_str as *const (),
        params: PARAMS_CONST_STR,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_binary_add",
        address: pon_binary_add as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_binary_op",
        address: number::pon_binary_op as *const (),
        params: PARAMS_OP_OBJ_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_unary_op",
        address: number::pon_unary_op as *const (),
        params: PARAMS_OP_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_rich_compare",
        address: object::pon_rich_compare as *const (),
        params: PARAMS_OP_OBJ_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_is_true",
        address: object::pon_is_true as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_get_attr",
        address: object::pon_get_attr as *const (),
        params: PARAMS_OBJ_NAME_FEEDBACK,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_set_attr",
        address: object::pon_set_attr as *const (),
        params: PARAMS_OBJ_NAME_OBJ,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_del_attr",
        address: object::pon_del_attr as *const (),
        params: PARAMS_OBJ_NAME,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_get_iter",
        address: iter::pon_get_iter as *const (),
        params: PARAMS_OBJ_FEEDBACK,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_iter_next",
        address: iter::pon_iter_next as *const (),
        params: PARAMS_OBJ_FEEDBACK,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_subscript_get",
        address: object::pon_subscript_get as *const (),
        params: PARAMS_OBJ_OBJ_FEEDBACK,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_call",
        address: pon_call as *const (),
        params: PARAMS_CALL,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_load_global",
        address: pon_load_global as *const (),
        params: PARAMS_LOAD_GLOBAL,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_load_name",
        address: pon_load_name as *const (),
        params: PARAMS_LOAD_NAME,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_load_builtin",
        address: builtins::pon_load_builtin as *const (),
        params: PARAMS_LOAD_BUILTIN,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_load_build_class",
        address: pon_load_build_class as *const (),
        params: PARAMS_LOAD_BUILD_CLASS,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_setup_annotations",
        address: pon_setup_annotations as *const (),
        params: PARAMS_NONE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_build_class",
        address: pon_build_class as *const (),
        params: PARAMS_BUILD_CLASS,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_print",
        address: pon_print as *const (),
        params: PARAMS_PRINT,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_make_function",
        address: pon_make_function as *const (),
        params: PARAMS_MAKE_FUNCTION,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_function_set_closure",
        address: call::pon_function_set_closure as *const (),
        params: PARAMS_FUNCTION_SET_CLOSURE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_make_cell",
        address: call::pon_make_cell as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_cell_get",
        address: call::pon_cell_get as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_cell_set",
        address: call::pon_cell_set as *const (),
        params: PARAMS_CELL_SET,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_cell_delete",
        address: call::pon_cell_delete as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_current_closure_cell",
        address: call::pon_current_closure_cell as *const (),
        params: PARAMS_INDEX,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_store_global",
        address: pon_store_global as *const (),
        params: PARAMS_STORE_GLOBAL,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_store_name",
        address: pon_store_name as *const (),
        params: PARAMS_STORE_NAME,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_none",
        address: pon_none as *const (),
        params: PARAMS_NONE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_raise",
        address: pon_raise as *const (),
        params: PARAMS_RAISE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_reraise",
        address: pon_reraise as *const (),
        params: PARAMS_NONE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_raise_type_error",
        address: pon_raise_type_error as *const (),
        params: PARAMS_RAISE_MESSAGE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_raise_value_error",
        address: pon_raise_value_error as *const (),
        params: PARAMS_RAISE_MESSAGE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_raise_index_error",
        address: pon_raise_index_error as *const (),
        params: PARAMS_RAISE_MESSAGE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_raise_key_error",
        address: pon_raise_key_error as *const (),
        params: PARAMS_RAISE_KEY_ERROR,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_raise_attribute_error",
        address: pon_raise_attribute_error as *const (),
        params: PARAMS_RAISE_ATTRIBUTE_ERROR,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_raise_stop_iteration",
        address: pon_raise_stop_iteration as *const (),
        params: PARAMS_RAISE_STOP_ITERATION,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_exc_matches",
        address: pon_exc_matches as *const (),
        params: PARAMS_EXC_MATCHES,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_exc_fetch",
        address: pon_exc_fetch as *const (),
        params: PARAMS_EXC_FETCH,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_exc_restore",
        address: pon_exc_restore as *const (),
        params: PARAMS_EXC_RESTORE,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_exc_group_split",
        address: pon_exc_group_split as *const (),
        params: PARAMS_EXC_GROUP_SPLIT,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_const_float",
        address: number::pon_const_float as *const (),
        params: PARAMS_CONST_FLOAT,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_const_bool",
        address: number::pon_const_bool as *const (),
        params: PARAMS_CONST_BOOL,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_const_complex",
        address: number::pon_const_complex as *const (),
        params: PARAMS_CONST_COMPLEX,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_number_binary",
        address: number::pon_number_binary as *const (),
        params: PARAMS_OP_OBJ_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_number_unary",
        address: number::pon_number_unary as *const (),
        params: PARAMS_OP_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_number_inplace",
        address: number::pon_number_inplace as *const (),
        params: PARAMS_OP_OBJ_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_index",
        address: number::pon_index as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_const_bytes",
        address: str_::pon_const_bytes as *const (),
        params: PARAMS_CONST_STR,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_const_bytearray",
        address: str_::pon_const_bytearray as *const (),
        params: PARAMS_CONST_STR,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_str_concat",
        address: str_::pon_str_concat as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_str_repeat",
        address: str_::pon_str_repeat as *const (),
        params: PARAMS_STR_REPEAT,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_bytes_concat",
        address: str_::pon_bytes_concat as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_bytes_repeat",
        address: str_::pon_bytes_repeat as *const (),
        params: PARAMS_STR_REPEAT,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_bytearray_concat",
        address: str_::pon_bytearray_concat as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_bytearray_repeat",
        address: str_::pon_bytearray_repeat as *const (),
        params: PARAMS_STR_REPEAT,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_format_value",
        address: str_::pon_format_value as *const (),
        params: PARAMS_FORMAT_VALUE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_build_fstring",
        address: str_::pon_build_fstring as *const (),
        params: PARAMS_FSTR_PARTS,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_build_string",
        address: str_::pon_build_string as *const (),
        params: PARAMS_FSTR_PARTS,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_build_template",
        address: str_::pon_build_template as *const (),
        params: PARAMS_TSTR_PARTS,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_str_method",
        address: str_::pon_str_method as *const (),
        params: PARAMS_STR_METHOD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_bytes_method",
        address: str_::pon_bytes_method as *const (),
        params: PARAMS_STR_METHOD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_build_list",
        address: seq::pon_build_list as *const (),
        params: PARAMS_OBJ_ARRAY,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_build_tuple",
        address: seq::pon_build_tuple as *const (),
        params: PARAMS_OBJ_ARRAY,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_build_range",
        address: seq::pon_build_range as *const (),
        params: PARAMS_BUILD_RANGE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_build_slice",
        address: seq::pon_build_slice as *const (),
        params: PARAMS_BUILD_RANGE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_list_append",
        address: seq::pon_list_append as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_list_extend",
        address: seq::pon_list_extend as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_list_sort",
        address: seq::pon_list_sort as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_seq_len",
        address: seq::pon_seq_len as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::ISize,
    },
    HelperDecl {
        symbol: "pon_get_len",
        address: seq::pon_get_len as *const (),
        params: PARAMS_OBJ_FEEDBACK,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_seq_get_item",
        address: seq::pon_seq_get_item as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_seq_set_item",
        address: seq::pon_seq_set_item as *const (),
        params: PARAMS_SEQ_SET_ITEM,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_seq_del_item",
        address: seq::pon_seq_del_item as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_unpack_seq",
        address: seq::pon_unpack_seq as *const (),
        params: PARAMS_UNPACK_SEQ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_unpack_ex",
        address: seq::pon_unpack_ex as *const (),
        params: PARAMS_UNPACK_EX,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_build_map",
        address: map::pon_build_map as *const (),
        params: PARAMS_OBJ_ARRAY,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_build_set",
        address: map::pon_build_set as *const (),
        params: PARAMS_OBJ_ARRAY,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_build_frozenset",
        address: map::pon_build_frozenset as *const (),
        params: PARAMS_OBJ_ARRAY,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_map_insert",
        address: map::pon_map_insert as *const (),
        params: PARAMS_SEQ_SET_ITEM,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_dict_set_item_status",
        address: map::pon_dict_set_item_status as *const (),
        params: PARAMS_SEQ_SET_ITEM,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_dict_get_item",
        address: map::pon_dict_get_item as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_subscript_set",
        address: map::pon_subscript_set as *const (),
        params: PARAMS_SEQ_SET_ITEM,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_subscript_del",
        address: map::pon_subscript_del as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_dict_del_item_status",
        address: map::pon_dict_del_item_status as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_dict_merge",
        address: map::pon_dict_merge as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_dict_merge_unique",
        address: map::pon_dict_merge_unique as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_dict_update",
        address: map::pon_dict_update as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_dict_get_method",
        address: map::pon_dict_get_method as *const (),
        params: PARAMS_SEQ_SET_ITEM,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_dict_setdefault",
        address: map::pon_dict_setdefault as *const (),
        params: PARAMS_SEQ_SET_ITEM,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_dict_pop",
        address: map::pon_dict_pop as *const (),
        params: PARAMS_SEQ_SET_ITEM,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_dict_keys",
        address: map::pon_dict_keys as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_dict_values",
        address: map::pon_dict_values as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_dict_items",
        address: map::pon_dict_items as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_dict_iter_keys",
        address: map::pon_dict_iter_keys as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_dict_iter_next",
        address: map::pon_dict_iter_next as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_set_add",
        address: map::pon_set_add as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_contains",
        address: map::pon_contains as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_set_iter",
        address: map::pon_set_iter as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_set_iter_next",
        address: map::pon_set_iter_next as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_set_union",
        address: map::pon_set_union as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_set_intersection",
        address: map::pon_set_intersection as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_set_difference",
        address: map::pon_set_difference as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_frozenset_hash",
        address: map::pon_frozenset_hash as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::ISize,
    },
    HelperDecl {
        symbol: "pon_load_attr",
        address: attr::pon_load_attr as *const (),
        params: PARAMS_OBJ_NAME_FEEDBACK,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_store_attr",
        address: attr::pon_store_attr as *const (),
        params: PARAMS_OBJ_NAME_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_delete_attr",
        address: attr::pon_delete_attr as *const (),
        params: PARAMS_OBJ_NAME,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_load_method",
        address: attr::pon_load_method as *const (),
        params: PARAMS_OBJ_NAME_FEEDBACK,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_isinstance",
        address: attr::pon_isinstance as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_issubclass",
        address: attr::pon_issubclass as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_call_ex",
        address: call::pon_call_ex as *const (),
        params: PARAMS_CALL_EX,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_call_method",
        address: call::pon_call_method as *const (),
        params: PARAMS_CALL_FEEDBACK,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_make_function_full",
        address: call::pon_make_function_full as *const (),
        params: PARAMS_MAKE_FUNCTION_FULL,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_load_method_pair",
        address: call::pon_load_method_pair as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_push_exc_info",
        address: exc::pon_push_exc_info as *const (),
        params: PARAMS_EXC_HANDLER,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_pop_exc_info",
        address: exc::pon_pop_exc_info as *const (),
        params: PARAMS_NONE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_match_exc",
        address: exc::pon_match_exc as *const (),
        params: PARAMS_EXC_MATCHES,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_check_exc_star",
        address: exc::pon_check_exc_star as *const (),
        params: PARAMS_EXC_MATCHES,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_get_current_exc",
        address: exc::pon_get_current_exc as *const (),
        params: PARAMS_NONE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_build_exc_group",
        address: exc::pon_build_exc_group as *const (),
        params: PARAMS_OBJ_ARRAY,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_gen_frame_alloc",
        address: r#gen::pon_gen_frame_alloc as *const (),
        params: PARAMS_GEN_MAKE_FRAME,
        ret: AbiTy::PyFramePtr,
    },
    HelperDecl {
        symbol: "pon_gen_consume_payload",
        address: r#gen::pon_gen_consume_payload as *const (),
        params: PARAMS_GEN_FRAME,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_gen_finish",
        address: r#gen::pon_gen_finish as *const (),
        params: PARAMS_GEN_FINISH,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_gen_unwind",
        address: r#gen::pon_gen_unwind as *const (),
        params: PARAMS_GEN_UNWIND,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_gen_delegate_step",
        address: r#gen::pon_gen_delegate_step as *const (),
        params: PARAMS_GEN_DELEGATE_STEP,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_gen_last_stop_value",
        address: r#gen::pon_gen_last_stop_value as *const (),
        params: PARAMS_NONE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_make_generator",
        address: r#gen::pon_make_generator as *const (),
        params: PARAMS_MAKE_GENERATOR,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_gen_send",
        address: r#gen::pon_gen_send as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_gen_stop_value",
        address: r#gen::pon_gen_stop_value as *const (),
        params: PARAMS_NONE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_gen_throw",
        address: r#gen::pon_gen_throw as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_gen_close",
        address: r#gen::pon_gen_close as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_get_aiter",
        address: r#gen::pon_get_aiter as *const (),
        params: PARAMS_OBJ_FEEDBACK,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_for_next",
        address: r#gen::pon_for_next as *const (),
        params: PARAMS_OBJ_FEEDBACK,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_await",
        address: r#gen::pon_await as *const (),
        params: PARAMS_OBJ_FEEDBACK,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_import_name",
        address: crate::import::pon_import_name as *const (),
        params: PARAMS_IMPORT_NAME,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_import_from",
        address: crate::import::pon_import_from as *const (),
        params: PARAMS_OBJ_NAME,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_import_star",
        address: crate::import::pon_import_star as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_module_has_attr",
        address: crate::import::pon_module_has_attr as *const (),
        params: PARAMS_OBJ_NAME,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_match_sequence",
        address: match_::pon_match_sequence as *const (),
        params: PARAMS_OBJ_FEEDBACK,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_match_mapping",
        address: match_::pon_match_mapping as *const (),
        params: PARAMS_OBJ_FEEDBACK,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_match_len_ge",
        address: match_::pon_match_len_ge as *const (),
        params: PARAMS_MATCH_LEN,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_match_keys",
        address: match_::pon_match_keys as *const (),
        params: PARAMS_MATCH_KEYS,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_match_class",
        address: match_::pon_match_class as *const (),
        params: PARAMS_MATCH_CLASS,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_assert",
        address: match_::pon_assert as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_runtime_init",
        address: pon_runtime_init as *const (),
        params: PARAMS_RUNTIME_INIT,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_thread_attach",
        address: pon_thread_attach as *const (),
        params: PARAMS_NONE,
        ret: AbiTy::ThreadStatePtr,
    },
    HelperDecl {
        symbol: "pon_thread_detach",
        address: pon_thread_detach as *const (),
        params: PARAMS_NONE,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_thread_start_new",
        address: pon_thread_start_new as *const (),
        params: PARAMS_THREAD_START_NEW,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_gc_safe_region_enter",
        address: pon_gc_safe_region_enter as *const (),
        params: PARAMS_NONE,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_gc_safe_region_leave",
        address: pon_gc_safe_region_leave as *const (),
        params: PARAMS_NONE,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_function_set_annotate",
        address: crate::types::function::pon_function_set_annotate as *const (),
        params: PARAMS_FUNCTION_SET_ANNOTATE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_make_type_alias",
        address: crate::types::typealias::pon_make_type_alias as *const (),
        params: PARAMS_MAKE_TYPE_ALIAS,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_make_typevar",
        address: crate::types::typealias::pon_make_typevar as *const (),
        params: PARAMS_MAKE_TYPEVAR,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_osr_poll",
        address: pon_osr_poll as *const (),
        params: PARAMS_OSR_POLL,
        ret: AbiTy::CodePtr,
    },
    HelperDecl {
        symbol: "pon_deopt_note",
        address: pon_deopt_note as *const (),
        params: PARAMS_DEOPT_NOTE,
        ret: AbiTy::I32,
    },
    HelperDecl {
        symbol: "pon_load_local",
        address: pon_load_local as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_delete_local",
        address: pon_delete_local as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_delete_global",
        address: pon_delete_global as *const (),
        params: PARAMS_LOAD_BUILTIN,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_delete_name",
        address: pon_delete_name as *const (),
        params: PARAMS_LOAD_BUILTIN,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_build_class_ex",
        address: pon_build_class_ex as *const (),
        params: PARAMS_BUILD_CLASS_EX,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_build_class_full",
        address: pon_build_class_full as *const (),
        params: PARAMS_BUILD_CLASS_FULL,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_exc_star_enter",
        address: exc::pon_exc_star_enter as *const (),
        params: PARAMS_NONE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_exc_star_match",
        address: exc::pon_exc_star_match as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_exc_star_body_ok",
        address: exc::pon_exc_star_body_ok as *const (),
        params: PARAMS_NONE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_exc_star_body_raised",
        address: exc::pon_exc_star_body_raised as *const (),
        params: PARAMS_NONE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_exc_star_finish",
        address: exc::pon_exc_star_finish as *const (),
        params: PARAMS_NONE,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_list_to_tuple",
        address: seq::pon_list_to_tuple as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_set_update",
        address: map::pon_set_update as *const (),
        params: PARAMS_BINARY_ADD,
        ret: AbiTy::PyObjectPtr,
    },
];

pub(crate) struct Runtime {
    heap: Heap,
    _type_type: *mut PyType,
    long_type: *mut PyType,
    unicode_type: *mut PyType,
    function_type: *mut PyType,
    none_type: *mut PyType,
    not_implemented_type: *mut PyType,
    ellipsis_type: *mut PyType,
    exception_types: ExceptionTypeSet,
    none: *mut PyNone,
    not_implemented: *mut PyNone,
    globals: HashMap<u32, *mut PyObject>,
    ellipsis: *mut PyNone,
    class_namespace_stack: Vec<ClassBodyFrame>,
    class_construction_stack: Vec<ClassBodyFrame>,
}

/// One active class-body scope.  `mapping` is the `__prepare__`-provided
/// namespace whose `__setitem__`/`__getitem__` the body's name operations
/// route through; NULL selects the plain `PyClassDict` fast path.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ClassBodyFrame {
    namespace: *mut type_::PyClassDict,
    mapping: *mut PyObject,
    /// Pre-resolution bases tuple (PEP 560), NULL when `__mro_entries__`
    /// never fired.  Rooted with the frame so a mid-body `gc.collect()`
    /// cannot sweep it before `__orig_bases__` publication.
    orig_bases: *mut PyObject,
}

/// Scope guard for one entry on `Runtime::class_construction_stack`.
///
/// `build_class_with_body` pushes the popped body frame onto the construction
/// registry and holds this guard across class construction, so the
/// `__prepare__` mapping and the internal namespace's values stay rooted while
/// metaclass hooks run.  Dropping pops on every exit path, including panics
/// unwinding to the ABI `catch_object_helper` boundary.
struct ClassConstructionRootGuard;

impl Drop for ClassConstructionRootGuard {
    fn drop(&mut self) {
        let _ = with_runtime(|runtime| runtime.class_construction_stack.pop());
    }
}

unsafe impl Send for Runtime {}

static RUNTIME: LazyLock<Mutex<Option<Runtime>>> = LazyLock::new(|| Mutex::new(None));

fn runtime_lock() -> MutexGuard<'static, Option<Runtime>> {
    RUNTIME.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn with_runtime<T>(f: impl FnOnce(&mut Runtime) -> T) -> Option<T> {
    let mut runtime = runtime_lock();
    runtime.as_mut().map(f)
}

/// Returns whether `pon_runtime_init` has completed in this process.
pub(crate) fn runtime_is_initialized() -> bool {
    runtime_lock().is_some()
}

pub(crate) fn runtime_type_type() -> *mut PyType {
    with_runtime(|runtime| runtime._type_type).unwrap_or(ptr::null_mut())
}

/// Non-raising lookup of an installed runtime builtin global.
pub(crate) fn runtime_global(name: u32) -> Option<*mut PyObject> {
    with_runtime(|runtime| runtime.globals.get(&name).copied()).flatten()
}

pub(crate) fn alloc_heap_instance(
    cls: *mut PyType,
    dict: *mut type_::PyClassDict,
    slots: Vec<type_::PySlotValue>,
) -> Result<*mut PyObject, String> {
    with_runtime(|runtime| {
        let object = runtime
            .heap
            .alloc(mem::size_of::<type_::PyHeapInstance>(), type_::TYPE_ID_HEAP_INSTANCE)
            .cast::<type_::PyHeapInstance>();
        unsafe {
            ptr::write(
                object,
                type_::PyHeapInstance {
                    ob_base: PyObjectHeader::new(cls),
                    dict,
                    slots,
                    weakrefs: ptr::null_mut(),
                    finalized: false,
                },
            );
        }
        Ok(as_object_ptr(object))
    })
    .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
}

/// One-shot gate serializing full runtime initialization across threads.
///
/// `RUNTIME` alone cannot express "publishing finished but eager module
/// registration is still running": `init_runtime` must release the runtime
/// mutex before `register_native_modules`/`ensure_tuple_subclass_surface`
/// (their factories re-enter the runtime through `with_runtime`), so the
/// occupied slot is visible while `sys` and the other eager modules are
/// still missing.  Treating "slot occupied" as "initialized" let a second
/// initializer race into `pon_sys_set_argv` during that window and fail
/// with "sys module is not initialized" (pon-pm PEP 517 hook flake).
///
/// The gate closes the window: exactly one thread performs initialization;
/// concurrent callers block until it reaches `Ready` (or `Failed`), while
/// re-entrant calls from the initializing thread itself (ABI entry points
/// invoked by eager module factories) return immediately — the runtime slot
/// is already published, which is all nested runtime use needs.
enum InitPhase {
    Uninit,
    Running(std::thread::ThreadId),
    Ready,
    Failed(String),
}

static INIT_PHASE: LazyLock<(Mutex<InitPhase>, Condvar)> =
    LazyLock::new(|| (Mutex::new(InitPhase::Uninit), Condvar::new()));

fn init_runtime() -> Result<(), String> {
    let (phase_lock, phase_signal) = &*INIT_PHASE;
    let mut phase = phase_lock.lock().unwrap_or_else(|poison| poison.into_inner());
    loop {
        match &*phase {
            InitPhase::Ready => return Ok(()),
            InitPhase::Failed(message) => return Err(message.clone()),
            InitPhase::Running(thread) if *thread == std::thread::current().id() => return Ok(()),
            InitPhase::Running(_) => {
                phase = phase_signal.wait(phase).unwrap_or_else(|poison| poison.into_inner());
            }
            InitPhase::Uninit => break,
        }
    }
    *phase = InitPhase::Running(std::thread::current().id());
    drop(phase);

    // A panic inside `perform_runtime_init` unwinds to `pon_runtime_init`'s
    // `catch_unwind`; without this guard the phase would stay `Running`
    // forever and every waiter would deadlock.
    struct FailOnUnwind;
    impl Drop for FailOnUnwind {
        fn drop(&mut self) {
            let (phase_lock, phase_signal) = &*INIT_PHASE;
            let mut phase = phase_lock.lock().unwrap_or_else(|poison| poison.into_inner());
            if matches!(&*phase, InitPhase::Running(_)) {
                *phase = InitPhase::Failed("runtime initialization panicked".to_owned());
            }
            phase_signal.notify_all();
        }
    }
    let guard = FailOnUnwind;

    let result = perform_runtime_init();
    let mut phase = phase_lock.lock().unwrap_or_else(|poison| poison.into_inner());
    *phase = match &result {
        Ok(()) => InitPhase::Ready,
        Err(message) => InitPhase::Failed(message.clone()),
    };
    drop(phase);
    mem::forget(guard);
    phase_signal.notify_all();
    result
}

fn perform_runtime_init() -> Result<(), String> {
    let should_register_native = {
        let mut slot = runtime_lock();
        if slot.is_some() {
            false
        } else {
            let heap = Heap::new();
            register_gc_types(&heap);

            let mut type_type = Box::new(PyType::new(ptr::null(), "type", mem::size_of::<PyType>()));
            type_type.tp_call = Some(type_::type_call);
            type_type.tp_getattro = Some(crate::descr::generic_get_attr);
            // J0.3 §6: class-attribute assignment (`SomeClass.attr = v`) must
            // reach generic_set_attr's type branch so type-dict mutation bumps
            // the version tag and invalidates recorded AttrICs.
            type_type.tp_setattro = Some(crate::descr::generic_set_attr);
            let type_type = Box::into_raw(type_type);
            let long_type = Box::into_raw(Box::new(PyType::new(type_type, "int", mem::size_of::<PyLong>())));
            let unicode_type = Box::into_raw(Box::new(PyType::new(type_type, "str", mem::size_of::<PyUnicode>())));
            let mut function_type = Box::new(PyType::new(type_type, "function", mem::size_of::<PyFunction>()));
            function_type.tp_descr_get = Some(function::function_descr_get);
            function_type.tp_getattro = Some(function::function_getattro);
            function_type.tp_setattro = Some(function::function_setattro);
            unsafe { function::install_function_type_attrs(function_type.as_mut(), type_type) };
            let function_type = Box::into_raw(function_type);
            let none_type = Box::into_raw(Box::new(PyType::new(type_type, "NoneType", mem::size_of::<PyNone>())));
            let not_implemented_type = Box::into_raw(Box::new(PyType::new(
                type_type,
                "NotImplementedType",
                mem::size_of::<PyNone>(),
            )));
            let ellipsis_type = Box::into_raw(Box::new(PyType::new(type_type, "ellipsis", mem::size_of::<PyNone>())));
            let exception_types = ExceptionTypeSet::new(type_type);

            // SAFETY: The leaked type object remains valid for the process lifetime.
            unsafe {
                (*type_type).ob_base.ob_type = type_type;
            }

            let none = heap.alloc(mem::size_of::<PyNone>(), TYPE_ID_NONE).cast::<PyNone>();
            // SAFETY: `none` points to a freshly allocated zeroed block of the right size.
            unsafe {
                ptr::write(
                    none,
                    PyNone {
                        ob_base: PyObjectHeader::new(none_type),
                    },
                );
            }
            let not_implemented = heap.alloc(mem::size_of::<PyNone>(), TYPE_ID_NOT_IMPLEMENTED).cast::<PyNone>();
            // SAFETY: `not_implemented` points to a freshly allocated zeroed block of the right size.
            unsafe {
                ptr::write(
                    not_implemented,
                    PyNone {
                        ob_base: PyObjectHeader::new(not_implemented_type),
                    },
                );
            }
            let ellipsis = heap.alloc(mem::size_of::<PyNone>(), TYPE_ID_ELLIPSIS).cast::<PyNone>();
            // SAFETY: `ellipsis` points to a freshly allocated zeroed block of the right size.
            unsafe {
                ptr::write(
                    ellipsis,
                    PyNone {
                        ob_base: PyObjectHeader::new(ellipsis_type),
                    },
                );
            }



            let mut runtime = Runtime {
                heap,
                _type_type: type_type,
                long_type,
                unicode_type,
                function_type,
                none_type,
                not_implemented_type,
                ellipsis_type,
                exception_types,
                none,
                not_implemented,
                ellipsis,
                globals: HashMap::new(),
                class_namespace_stack: Vec::new(),
                class_construction_stack: Vec::new(),
            };

            register_builtins(&mut runtime)?;
            *slot = Some(runtime);
            true
        }
    };

    if should_register_native {
        crate::import::register_native_modules()?;
        // Eager tuple surface: `collections.namedtuple` captures
        // `tuple.__new__` at import time (`_tuple_new = tuple.__new__`),
        // which can happen before any tuple-derived class construction
        // triggers the lazy install.  Must run after the runtime is stored:
        // the installer allocates carriers through `with_runtime`.
        seq::ensure_tuple_subclass_surface();
    }
    Ok(())
}

fn register_gc_types(heap: &Heap) {
    heap.register_type(
        TYPE_ID_TYPE,
        GcTypeInfo {
            size: mem::size_of::<PyType>(),
            trace: trace_no_refs,
            finalize: None,
        },
    );
    heap.register_type(
        TYPE_ID_LONG,
        GcTypeInfo {
            size: mem::size_of::<PyLong>(),
            trace: trace_no_refs,
            finalize: None,
        },
    );
    heap.register_type(
        TYPE_ID_UNICODE,
        GcTypeInfo {
            size: mem::size_of::<PyUnicode>(),
            trace: trace_no_refs,
            finalize: Some(finalize_unicode),
        },
    );
    heap.register_type(
        TYPE_ID_FUNCTION,
        GcTypeInfo {
            size: mem::size_of::<PyFunction>(),
            trace: trace_function,
            finalize: Some(finalize_function),
        },
    );
    heap.register_type(
        TYPE_ID_NONE,
        GcTypeInfo {
            size: mem::size_of::<PyNone>(),
            trace: trace_no_refs,
            finalize: None,
        },
    );
    heap.register_type(
        TYPE_ID_NOT_IMPLEMENTED,
        GcTypeInfo {
            size: mem::size_of::<PyNone>(),
            trace: trace_no_refs,
            finalize: None,
        },
    );
    heap.register_type(
        TYPE_ID_ELLIPSIS,
        GcTypeInfo {
            size: mem::size_of::<PyNone>(),
            trace: trace_no_refs,
            finalize: None,
        },
    );
    heap.register_type(
        TYPE_ID_EXCEPTION,
        GcTypeInfo {
            size: mem::size_of::<PyBaseException>(),
            trace: trace_base_exception,
            finalize: Some(crate::types::exc::finalize_base_exception),
        },
    );
    heap.register_type(
        TYPE_ID_EXCEPTION_GROUP,
        GcTypeInfo {
            size: mem::size_of::<crate::types::exc::PyExceptionGroup>(),
            trace: trace_exception_group,
            finalize: Some(crate::types::exc::finalize_base_exception),
        },
    );
    heap.register_type(
        type_::TYPE_ID_HEAP_INSTANCE,
        GcTypeInfo {
            size: mem::size_of::<type_::PyHeapInstance>(),
            trace: crate::types::weakref::trace_heap_instance,
            finalize: Some(crate::types::weakref::finalize_heap_instance),
        },
    );
    heap.register_type(
        type_::TYPE_ID_PAYLOAD_SUBCLASS_INSTANCE,
        GcTypeInfo {
            size: mem::size_of::<type_::PyPayloadSubclassInstance>(),
            trace: type_::trace_payload_subclass_instance,
            finalize: Some(type_::finalize_payload_subclass_instance),
        },
    );
    heap.register_type(
        crate::types::weakref::TYPE_ID_WEAKREF,
        GcTypeInfo {
            size: mem::size_of::<crate::types::weakref::PyWeakRef>(),
            trace: crate::types::weakref::trace_weakref,
            finalize: Some(crate::types::weakref::finalize_weakref),
        },
    );
}

unsafe extern "C" fn trace_no_refs(_object: *mut u8, _visitor: &mut dyn FnMut(*mut u8)) {}

unsafe extern "C" fn finalize_unicode(object: *mut u8) {
    if object.is_null() {
        return;
    }

    // SAFETY: The GC calls this only for live allocations registered as PyUnicode.
    let unicode = unsafe { &mut *object.cast::<PyUnicode>() };
    if unicode.owns_data && !unicode.data.is_null() {
        let data = unicode.data.cast_mut();
        let len = unicode.len;
        unicode.data = ptr::null();
        unicode.len = 0;
        unicode.owns_data = false;
        let slice = ptr::slice_from_raw_parts_mut(data, len);
        // SAFETY: Owned unicode data is created by `Box<[u8]>::into_raw`.
        unsafe {
            drop(Box::<[u8]>::from_raw(slice));
        }
    }
}

unsafe extern "C" fn trace_function(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
    if object.is_null() {
        return;
    }
    // SAFETY: The GC calls this only for live allocations registered as PyFunction.
    let function = unsafe { &*object.cast::<PyFunction>() };
    if !function.attr_dict.is_null() {
        visitor(function.attr_dict.cast::<u8>());
    }
    if !function.annotations.is_null() {
        visitor(function.annotations.cast::<u8>());
    }
    crate::types::function::visit_function_gc_refs(object.cast::<PyObject>(), visitor);
}

unsafe extern "C" fn finalize_function(object: *mut u8) {
    if object.is_null() {
        return;
    }

    crate::types::weakref::clear_weakrefs(object.cast::<PyObject>());
    let function = object.cast::<PyFunction>();
    function::unregister_function_record(function.cast::<PyObject>());
    function::clear_function_module(function.cast::<PyObject>());
    function::clear_native_function(function.cast::<PyObject>());
    // SAFETY: The GC calls this only for unreachable PyFunction allocations, so
    // no compiled code can concurrently read these interior-mutable owners.
    unsafe {
        *(*function).feedback.get() = None;
        *(*function).tier1.get() = None;
    }
}

fn register_builtins(runtime: &mut Runtime) -> Result<(), String> {
    runtime_builtins::for_each_builtin(|builtin_name, arity, code| {
        let name = crate::intern::intern(builtin_name);
        if let Ok(function) = alloc_function(runtime, code, arity, name) {
            runtime.globals.insert(name, function);
        }
    });
    register_builtin_type_globals(runtime);
    runtime
        .globals
        .insert(crate::intern::intern("NotImplemented"), as_object_ptr(runtime.not_implemented));
    runtime
        .globals
        .insert(crate::intern::intern("Ellipsis"), as_object_ptr(runtime.ellipsis));
    register_exception_builtins(runtime);
    if runtime.globals.contains_key(&runtime_builtins::print_name_interned()) && runtime.globals.contains_key(&crate::intern::intern("ValueError")) {
        Ok(())
    } else {
        Err("failed to register print builtin".to_owned())
    }
}

fn register_exception_builtins(runtime: &mut Runtime) {
    for (_kind, ty) in runtime.exception_types.core_types() {
        if ty.is_null() {
            continue;
        }
        let name = unsafe { (*ty).name() };
        runtime.globals.insert(crate::intern::intern(name), ty.cast::<PyObject>());
    }
    for (alias, ty) in [
        ("EnvironmentError", runtime.exception_types.os_error),
        ("IOError", runtime.exception_types.os_error),
    ] {
        runtime.globals.insert(crate::intern::intern(alias), ty.cast::<PyObject>());
    }
}

fn register_builtin_type_globals(runtime: &mut Runtime) {
    let Some(object_type) = crate::native::builtins_mod::builtin_native_type("object") else {
        return;
    };
    unsafe {
        typealias::install_type_or_slots(runtime._type_type);
        let union_type = typealias::union_type();
        (*union_type).ob_base.ob_type = runtime._type_type;
        if (*union_type).tp_base.is_null() {
            (*union_type).tp_base = object_type;
        }
        install_builtin_type(runtime, "object", object_type, Some(builtin_object_new), object_type);
        // PEP 3119 default: `object.__subclasshook__` returns NotImplemented
        // so ABC `__subclasscheck__` falls through to MRO/registry checks.
        install_object_subclasshook(runtime, object_type);
        // Default `__repr__`/`__str__`/`__format__`/`__reduce_ex__` surface
        // (MRO terminus identity anchors for class machinery like enum).
        install_object_dunders(runtime, object_type);
        install_builtin_type(runtime, "type", runtime._type_type, Some(builtin_type_new), object_type);
        // Python-visible `type.__new__` staticmethod: metaclass `__new__`
        // overrides terminate here via `super().__new__(mcls, ...)`.
        install_type_dunder_new(runtime);
        install_type_dunder_prepare(runtime);
        // Default `__instancecheck__`/`__subclasscheck__` terminus for
        // metaclass overrides that delegate via `super()`.
        install_type_check_dunders(runtime);
        // `type.__dict__` getset descriptors: `__annotations__` (PEP 649;
        // annotationlib captures its unbound `__get__` at module scope),
        // `__mro__` and `__dict__` (inspect.py:1667/1668 capture theirs; the
        // whole `test.support` import chain runs those lines).
        install_type_getset_descriptors(runtime);
        // `None.__new__` and friends: NoneType gets real attribute lookup
        // over `object`, with `__new__` aliased to object's carrier so the
        // identity checks in enum's `_find_new_` hold.
        install_none_attribute_support(runtime, object_type);
        install_builtin_type(runtime, "int", runtime.long_type, Some(builtin_int_new), object_type);
        // `int.__format__` as a real int-formatting method: int SUBCLASS
        // instances (IntEnum/IntFlag members, plain `class C(int)`) resolve
        // it through the MRO ahead of `object.__format__`'s non-empty-spec
        // TypeError, and enum's `member_type.__format__` class-dict copy
        // picks up the genuine formatter.  Eager because instance lookups
        // never pass `descr::synthetic_type_attr`'s lazy type-level trigger.
        install_int_dunder_format(runtime, runtime.long_type);
        install_builtin_type(runtime, "str", runtime.unicode_type, Some(builtin_str_new), object_type);

        let bool_type = (*bool_::from_bool(false)).ob_type.cast_mut();
        // CPython: `bool` subclasses `int` (`bool.__mro__ == (bool, int,
        // object)`, `issubclass(bool, int)` is True).  The static bool type
        // must re-point at THIS runtime's per-init `long_type` on every
        // registration, so the assignment is unconditional (a null-guarded
        // write would keep a stale pointer across runtime re-inits).
        (*bool_type).tp_base = runtime.long_type;
        install_builtin_type(runtime, "bool", bool_type, Some(builtin_bool_new), object_type);

        let float_sample = float::from_f64(0.0);
        let float_type = (*float_sample).ob_type.cast_mut();
        install_builtin_type(runtime, "float", float_type, Some(builtin_float_new), object_type);

        let complex_sample = complex_::from_f64s(0.0, 0.0);
        let complex_type = (*complex_sample).ob_type.cast_mut();
        install_builtin_type(runtime, "complex", complex_type, Some(builtin_complex_new), object_type);

        install_builtin_type(runtime, "bytes", bytes_::bytes_type(), Some(builtin_bytes_new), object_type);
        install_builtin_type(runtime, "bytearray", bytearray_::bytearray_type(), Some(builtin_bytearray_new), object_type);
        install_builtin_type(runtime, "memoryview", memoryview::memoryview_type(), Some(builtin_memoryview_new), object_type);
        install_builtin_type(
            runtime,
            "classmethod",
            classmethod_builtin_type(),
            Some(builtin_classmethod_new),
            object_type,
        );
        install_builtin_type(
            runtime,
            "staticmethod",
            staticmethod_builtin_type(),
            Some(builtin_staticmethod_new),
            object_type,
        );

        for (name, constructor) in [
            ("list", builtin_list_new as NewFunc),
            ("tuple", builtin_tuple_new as NewFunc),
            ("range", builtin_range_new as NewFunc),
            ("enumerate", builtin_enumerate_new as NewFunc),
            ("zip", builtin_zip_new as NewFunc),
            ("map", builtin_map_new as NewFunc),
            ("filter", builtin_filter_new as NewFunc),
            ("property", builtin_property_new as NewFunc),
            ("super", builtin_super_new as NewFunc),
        ] {
            if let Some(ty) = crate::native::builtins_mod::builtin_native_type(name) {
                install_builtin_type(runtime, name, ty, Some(constructor), object_type);
            }
        }

        install_builtin_type(
            runtime,
            "dict",
            crate::types::dict::dict_type(runtime._type_type),
            Some(builtin_dict_new),
            object_type,
        );
        install_builtin_type(
            runtime,
            "set",
            crate::types::set_::set_type(runtime._type_type),
            Some(builtin_set_new),
            object_type,
        );
        install_builtin_type(
            runtime,
            "frozenset",
            crate::types::frozenset::frozenset_type(runtime._type_type),
            Some(builtin_frozenset_new),
            object_type,
        );
        // Real `__new__`/`__repr__` (+`__str__`) entries for the data types
        // enum mixes with; must run after the int/str globals exist.
        install_data_type_dunders(runtime);
    }
}

fn install_type_dunder_new(runtime: &mut Runtime) {
    let name = crate::intern::intern("__new__");
    let Ok(function) = alloc_function(
        runtime,
        type_::type_dunder_new as *const u8,
        crate::builtins::variadic_arity(),
        name,
    ) else {
        return;
    };
    // A staticmethod carrier keeps `super().__new__` and `cls.__new__`
    // lookups from binding the receiver (CPython: `__new__` is implicitly
    // static).
    let descriptor = unsafe { classmethod::new_staticmethod(staticmethod_builtin_type(), function) };
    if descriptor.is_null() {
        return;
    }
    unsafe {
        let type_type = runtime._type_type;
        let mut dict = (*type_type).tp_dict.cast::<type_::PyClassDict>();
        if dict.is_null() {
            dict = type_::new_namespace();
            (*type_type).tp_dict = dict.cast::<PyObject>();
        }
        (&mut *dict).set(name, descriptor.cast::<PyObject>());
        crate::sync::register_namespaced_type(type_type);
    }
}

/// `type.__prepare__(name, bases, **kwds)` — ignores its arguments and
/// returns a fresh empty dict (CPython parity; installed as a classmethod).
unsafe extern "C" fn type_dunder_prepare_native(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    unsafe { map::pon_build_map(ptr::null_mut(), 0) }
}

fn install_type_dunder_prepare(runtime: &mut Runtime) {
    let name = crate::intern::intern("__prepare__");
    let Ok(function) = alloc_function(
        runtime,
        type_dunder_prepare_native as *const u8,
        crate::builtins::variadic_arity(),
        name,
    ) else {
        return;
    };
    let descriptor = unsafe { classmethod::new_classmethod(classmethod_builtin_type(), function) };
    if descriptor.is_null() {
        return;
    }
    unsafe {
        let type_type = runtime._type_type;
        let mut dict = (*type_type).tp_dict.cast::<type_::PyClassDict>();
        if dict.is_null() {
            dict = type_::new_namespace();
            (*type_type).tp_dict = dict.cast::<PyObject>();
        }
        (&mut *dict).set(name, descriptor.cast::<PyObject>());
        crate::sync::register_namespaced_type(type_type);
    }
}

/// `tp_getattro` for `None`: MRO-only lookup with descriptor binding.  No
/// instance dict — `PyNone` is header-only, so the generic heap-instance
/// resolver must not be installed here (its `PyHeapInstance` dict cast would
/// read past the allocation).
unsafe extern "C" fn none_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { type_::unicode_text(name) }) else {
        return return_null_with_error("attribute name must be str");
    };
    let name_id = crate::intern::intern(name_text);
    let ty = unsafe { (*object).ob_type.cast_mut() };
    let descr = unsafe { crate::descr::lookup_in_type(ty, name_id) };
    if descr.is_null() {
        return unsafe { pon_raise_attribute_error(object, name_id) };
    }
    unsafe { crate::descr::descriptor_get(descr, object, ty) }
}

/// NoneType attribute support: MRO lookups over an `object` base, with
/// `__new__` aliased to the same staticmethod carrier `object.__new__`
/// resolves to (CPython: `None.__new__ is object.__new__`; enum's
/// `_find_new_` compares the two by identity).
fn install_none_attribute_support(runtime: &mut Runtime, object_type: *mut PyType) {
    let none_type = runtime.none_type;
    if none_type.is_null() {
        return;
    }
    unsafe {
        (*none_type).tp_base = object_type;
        (*none_type).tp_getattro = Some(none_getattro);
        let object_dict = (*object_type).tp_dict.cast::<type_::PyClassDict>();
        if !object_dict.is_null() {
            if let Some(new_carrier) = (&*object_dict).get(crate::intern::intern("__new__")) {
                let dict = type_::new_namespace();
                (&mut *dict).set(crate::intern::intern("__new__"), new_carrier);
                (*none_type).tp_dict = dict.cast::<PyObject>();
            }
        }
        crate::sync::register_namespaced_type(none_type);
    }
}

/// `type.__instancecheck__(cls, instance)` — default isinstance semantics
/// (`type(instance)` MRO walk), the terminus for metaclass hooks that
/// delegate via `super().__instancecheck__(instance)`.  Deliberately does
/// NOT re-enter the metaclass hook dispatch (CPython: `type_call` slot
/// wrapper calls `recursive_isinstance`).
unsafe extern "C" fn type_dunder_instancecheck_native(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() || argc != 2 {
        const MESSAGE: &str = "__instancecheck__() takes exactly one argument";
        return unsafe { exc::pon_raise_type_error(MESSAGE.as_ptr(), MESSAGE.len()) };
    }
    let cls = unsafe { *argv };
    let instance = unsafe { *argv.add(1) };
    if !unsafe { crate::types::type_::is_type_object(cls) } {
        const MESSAGE: &str = "isinstance() arg 2 must be a class";
        return unsafe { exc::pon_raise_type_error(MESSAGE.as_ptr(), MESSAGE.len()) };
    }
    let ty = if instance.is_null() { ptr::null_mut() } else { unsafe { (*instance).ob_type }.cast_mut() };
    let ok = !ty.is_null() && unsafe { crate::mro::is_subtype(ty, cls.cast::<PyType>()) };
    unsafe { number::pon_const_bool(i32::from(ok)) }
}

/// `type.__subclasscheck__(cls, subclass)` — default issubclass semantics
/// (MRO walk), the terminus for `super().__subclasscheck__(subclass)`.
unsafe extern "C" fn type_dunder_subclasscheck_native(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() || argc != 2 {
        const MESSAGE: &str = "__subclasscheck__() takes exactly one argument";
        return unsafe { exc::pon_raise_type_error(MESSAGE.as_ptr(), MESSAGE.len()) };
    }
    let cls = unsafe { *argv };
    let subclass = unsafe { *argv.add(1) };
    if unsafe { !crate::types::type_::is_type_object(cls) || !crate::types::type_::is_type_object(subclass) } {
        const MESSAGE: &str = "issubclass() arguments must be classes";
        return unsafe { exc::pon_raise_type_error(MESSAGE.as_ptr(), MESSAGE.len()) };
    }
    let ok = unsafe { crate::mro::is_subtype(subclass.cast::<PyType>(), cls.cast::<PyType>()) };
    unsafe { number::pon_const_bool(i32::from(ok)) }
}

/// Installs `type.__instancecheck__` / `type.__subclasscheck__` as plain
/// methods in the builtin `type`'s dict so metaclass overrides can delegate
/// through `super()` (CPython exposes them as slot wrappers on `type`).
/// Hook dispatch is unaffected: `metaclass_check_hook` stops strictly below
/// the builtin `type`, so these natives never detour plain isinstance().
fn install_type_check_dunders(runtime: &mut Runtime) {
    for (name, entry) in [
        ("__instancecheck__", type_dunder_instancecheck_native as unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject),
        ("__subclasscheck__", type_dunder_subclasscheck_native as unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject),
    ] {
        let name = crate::intern::intern(name);
        let Ok(function) = alloc_function(runtime, entry as *const u8, 2, name) else {
            continue;
        };
        unsafe {
            let type_type = runtime._type_type;
            let mut dict = (*type_type).tp_dict.cast::<type_::PyClassDict>();
            if dict.is_null() {
                dict = type_::new_namespace();
                (*type_type).tp_dict = dict.cast::<PyObject>();
            }
            (&mut *dict).set(name, function);
            crate::sync::register_namespaced_type(type_type);
        }
    }
}

/// Installs the `type.__dict__` getset descriptors — `__annotations__`
/// (PEP 649 class-annotations surface), `__mro__`, `__bases__`, and
/// `__dict__` (inspect's static-introspection captures) — see the descriptor
/// section in `descr.rs`.  Attribute reads on class receivers keep resolving
/// through the fast paths in `generic_get_attr_cached`; the dict entries
/// exist for direct `type.__dict__[...]` consumers (annotationlib, inspect)
/// and give class-level writes data-descriptor routing (`__annotations__`
/// writable, the rest read-only).
fn install_type_getset_descriptors(runtime: &mut Runtime) {
    // Stamp the shared descriptor type's metatype and the descriptors'
    // `__objclass__` (the builtin `type`); idempotent with the function-type
    // install path.
    unsafe { crate::descr::finalize_getset_descriptors(runtime._type_type) };
    for (name, descriptor) in crate::descr::type_getset_entries() {
        if descriptor.is_null() {
            continue;
        }
        unsafe {
            let type_type = runtime._type_type;
            let mut dict = (*type_type).tp_dict.cast::<type_::PyClassDict>();
            if dict.is_null() {
                dict = type_::new_namespace();
                (*type_type).tp_dict = dict.cast::<PyObject>();
            }
            (&mut *dict).set(crate::intern::intern(name), descriptor);
            crate::sync::register_namespaced_type(type_type);
        }
    }
}

unsafe extern "C" fn object_subclasshook_native(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    with_runtime(|runtime| as_object_ptr(runtime.not_implemented)).unwrap_or(ptr::null_mut())
}

fn install_object_subclasshook(runtime: &mut Runtime, object_type: *mut PyType) {
    let name = crate::intern::intern("__subclasshook__");
    let Ok(function) = alloc_function(
        runtime,
        object_subclasshook_native as *const u8,
        crate::builtins::variadic_arity(),
        name,
    ) else {
        return;
    };
    let descriptor = unsafe { classmethod::new_classmethod(classmethod_builtin_type(), function) };
    if descriptor.is_null() {
        return;
    }
    unsafe {
        let mut dict = (*object_type).tp_dict.cast::<type_::PyClassDict>();
        if dict.is_null() {
            dict = type_::new_namespace();
            (*object_type).tp_dict = dict.cast::<PyObject>();
        }
        (&mut *dict).set(name, descriptor.cast::<PyObject>());
        crate::sync::register_namespaced_type(object_type);
    }
}

unsafe fn native_receiver_arg(argv: *mut *mut PyObject, argc: usize, name: &str) -> Result<*mut PyObject, *mut PyObject> {
    if argc == 0 || argv.is_null() {
        return Err(return_null_with_error(format!("object.{name}() requires a receiver")));
    }
    Ok(unsafe { *argv })
}

/// `object.__repr__(self)` — the native default repr (MRO terminus).
unsafe extern "C" fn object_dunder_repr_native(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let receiver = match unsafe { native_receiver_arg(argv, argc, "__repr__") } {
        Ok(receiver) => receiver,
        Err(error) => return error,
    };
    // The MRO terminus stays dispatch-free: `try_repr_text` resolves user
    // hooks before ever landing here.
    let text = match crate::native::builtins_mod::repr_text_no_dispatch(receiver) {
        Ok(text) => text,
        Err(()) => return ptr::null_mut(),
    };
    unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

/// `object.__str__(self)` — defaults to repr, matching CPython.
unsafe extern "C" fn object_dunder_str_native(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let receiver = match unsafe { native_receiver_arg(argv, argc, "__str__") } {
        Ok(receiver) => receiver,
        Err(error) => return error,
    };
    // CPython `object.__str__` delegates to `repr(self)` — WITH `__repr__`
    // dispatch (a class overriding only `__repr__` strs through it).
    let text = match crate::native::builtins_mod::try_repr_text(receiver) {
        Ok(text) => text,
        Err(()) => return ptr::null_mut(),
    };
    unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

/// `object.__format__(self, spec)` — empty spec means `str(self)`, anything
/// else is a TypeError (CPython parity).  Deliberately does NOT delegate to
/// the spec formatter, which dispatches back through `__format__`.
unsafe extern "C" fn object_dunder_format_native(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let receiver = match unsafe { native_receiver_arg(argv, argc, "__format__") } {
        Ok(receiver) => receiver,
        Err(error) => return error,
    };
    let spec = if argc >= 2 {
        let spec_object = unsafe { *argv.add(1) };
        match unsafe { type_::unicode_text(spec_object) } {
            Some(text) => text.to_owned(),
            None => return return_null_with_error("__format__() argument must be str"),
        }
    } else {
        String::new()
    };
    if !spec.is_empty() {
        let message = format!(
            "unsupported format string passed to {}.__format__",
            unsafe { object_type_name_for_error(receiver) },
        );
        return unsafe { exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    }
    let text = match crate::native::builtins_mod::try_str_text(receiver) {
        Ok(text) => text,
        Err(()) => return ptr::null_mut(),
    };
    unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

/// `object.__reduce_ex__(self, protocol)` — placeholder terminus so class
/// machinery (enum) can identity-compare and reassign it; pickling itself is
/// not implemented.
unsafe extern "C" fn object_dunder_reduce_ex_native(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let receiver = match unsafe { native_receiver_arg(argv, argc, "__reduce_ex__") } {
        Ok(receiver) => receiver,
        Err(error) => return error,
    };
    let message = format!("cannot pickle '{}' object", unsafe { object_type_name_for_error(receiver) });
    unsafe { exc::pon_raise_type_error(message.as_ptr(), message.len()) }
}

unsafe fn object_type_name_for_error(object: *mut PyObject) -> &'static str {
    if object.is_null() {
        return "object";
    }
    let ty = unsafe { (*object).ob_type };
    if ty.is_null() { "object" } else { unsafe { (*ty).name() } }
}

/// Default dunder surface on `object`'s dict: `__repr__`/`__str__`/
/// `__format__`/`__reduce_ex__`/`__init__` as plain functions (unbound on
/// class access, bound on instance access), plus a staticmethod-wrapped
/// `__new__` allocator, the MRO terminus class machinery like enum's
/// `EnumType.__new__`/`_find_new_` identity-compares against.
fn install_object_dunders(runtime: &mut Runtime, object_type: *mut PyType) {
    let entries: [(&str, *const u8); 5] = [
        ("__repr__", object_dunder_repr_native as *const u8),
        ("__str__", object_dunder_str_native as *const u8),
        ("__format__", object_dunder_format_native as *const u8),
        ("__reduce_ex__", object_dunder_reduce_ex_native as *const u8),
        ("__init__", object_dunder_init_native as *const u8),
    ];
    for (spelling, entry) in entries {
        let name = crate::intern::intern(spelling);
        let Ok(function) = alloc_function(runtime, entry, crate::builtins::variadic_arity(), name) else {
            continue;
        };
        unsafe {
            let mut dict = (*object_type).tp_dict.cast::<type_::PyClassDict>();
            if dict.is_null() {
                dict = type_::new_namespace();
                (*object_type).tp_dict = dict.cast::<PyObject>();
            }
            (&mut *dict).set(name, function.cast::<PyObject>());
        }
        match spelling {
            "__init__" => OBJECT_DUNDER_INIT_CARRIER.store(function.cast::<PyObject>(), Ordering::Release),
            "__repr__" => OBJECT_DUNDER_REPR_CARRIER.store(function.cast::<PyObject>(), Ordering::Release),
            "__str__" => OBJECT_DUNDER_STR_CARRIER.store(function.cast::<PyObject>(), Ordering::Release),
            _ => {}
        }
    }
    // `object.__new__` is a staticmethod carrier like `type.__new__`/
    // `int.__new__` (CPython: `__new__` is implicitly static), so class and
    // instance lookups return the identical unbound carrier.
    let new_name = crate::intern::intern("__new__");
    if let Ok(function) =
        alloc_function(runtime, object_dunder_new_native as *const u8, crate::builtins::variadic_arity(), new_name)
    {
        let descriptor = unsafe { classmethod::new_staticmethod(staticmethod_builtin_type(), function) };
        if !descriptor.is_null() {
            unsafe {
                let mut dict = (*object_type).tp_dict.cast::<type_::PyClassDict>();
                if dict.is_null() {
                    dict = type_::new_namespace();
                    (*object_type).tp_dict = dict.cast::<PyObject>();
                }
                (&mut *dict).set(new_name, descriptor.cast::<PyObject>());
            }
            OBJECT_DUNDER_NEW_DESCRIPTOR.store(descriptor.cast::<PyObject>(), Ordering::Release);
        }
    }
    crate::sync::register_namespaced_type(object_type);
}

/// `int.__format__` on the builtin `int` type's dict: a plain function
/// (unbound on class access, bound on instance access — the
/// [`install_object_dunders`] carrier shape), so int-subclass instance
/// lookups resolve the genuine int formatter ahead of `object.__format__`
/// and enum's `member_type.__format__` class-dict copy (vendored
/// enum.py:573-575) picks it up for IntEnum/IntFlag.  Lives here beside its
/// sibling installers because allocation must go through the lock-free
/// [`alloc_function`] — `register_builtin_type_globals` already holds the
/// runtime, so `pon_make_function`'s `with_runtime` would deadlock.
fn install_int_dunder_format(runtime: &Runtime, long_type: *mut PyType) {
    if long_type.is_null() {
        return;
    }
    let name = crate::intern::intern("__format__");
    let Ok(function) = alloc_function(
        runtime,
        crate::types::int::int_dunder_format_entry as *const u8,
        crate::builtins::variadic_arity(),
        name,
    ) else {
        return;
    };
    unsafe {
        let mut dict = (*long_type).tp_dict.cast::<type_::PyClassDict>();
        if dict.is_null() {
            dict = type_::new_namespace();
            (*long_type).tp_dict = dict.cast::<PyObject>();
        }
        (&mut *dict).set(name, function.cast::<PyObject>());
    }
    // GC rooting for the namespace value; init-time, so no live AttrIC needs
    // invalidation.
    crate::sync::register_namespaced_type(long_type);
}

/// `object.__init__(self, *args)` — the MRO terminus.  Excess arguments are
/// tolerated exactly when the receiver's class overrides `__new__` (CPython
/// `object_init`; enum's member `__init__(*args)` path).  A receiver whose
/// class leaves `__new__` at object's generic allocator raises the CPython
/// TypeError instead of silently swallowing the arguments.
unsafe extern "C" fn object_dunder_init_native(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc == 0 || argv.is_null() {
        return return_null_with_error("object.__init__() requires a receiver");
    }
    if argc > 1 {
        let receiver = unsafe { *argv };
        // Tagged immediates carry no dereferenceable type; their classes
        // (int, bool, float) all override `__new__`, so tolerate.
        if !receiver.is_null() && crate::tag::is_heap(receiver) {
            let ty = unsafe { (*receiver).ob_type.cast_mut() };
            // Only heap-instance classes are probed: builtin-layout receivers
            // (list/tuple/dict instances) reach here through native
            // constructors whose INSTANCE type object is distinct from the
            // global constructor type (no tp_new/tp_base on it), and their
            // builtin classes always override `__new__` in CPython terms.
            if !ty.is_null()
                && unsafe { (*ty).gc_type_id } == crate::types::type_::TYPE_ID_HEAP_INSTANCE.0 as usize
                && !unsafe { type_new_is_overridden(ty) }
            {
                const MESSAGE: &str =
                    "object.__init__() takes exactly one argument (the instance to initialize)";
                return unsafe { exc::pon_raise_type_error(MESSAGE.as_ptr(), MESSAGE.len()) };
            }
        }
    }
    unsafe { pon_none() }
}

/// Whether `ty` overrides `__new__` past object's generic allocator: a
/// Python-level/`install_*` dict entry other than object's carrier, or a
/// native constructor in the `tp_new` slot of `ty` or any MRO ancestor.
/// The ancestor walk mirrors CPython's type-ready slot inheritance: a
/// bytes/tuple subclass inherits the builtin base's `tp_new`, so CPython
/// sees `tp_new != object_new` and `object.__init__` stays permissive
/// about excess arguments (multiprocessing's `AuthenticationString(bytes)`
/// built from `os.urandom(32)`).  Exactly two slots do NOT override:
/// `type_new` (the generic heap allocator every constructed class carries)
/// and `builtin_object_new` (object's own slot — counting it would mark
/// every class overridden, object being every MRO's terminus).
unsafe fn type_new_is_overridden(ty: *mut PyType) -> bool {
    let generic_new = type_::type_new as *const () as usize;
    let object_slot_new = builtin_object_new as *const () as usize;
    for entry in unsafe { crate::mro::mro_entries(ty) } {
        if entry.is_null() {
            continue;
        }
        if let Some(slot) = unsafe { (*entry).tp_new } {
            let slot = slot as usize;
            if slot != generic_new && slot != object_slot_new {
                return true;
            }
        }
    }
    let ty_new = unsafe { crate::descr::lookup_in_type(ty, crate::intern::intern("__new__")) };
    let object_new = OBJECT_DUNDER_NEW_DESCRIPTOR.load(Ordering::Acquire);
    !ty_new.is_null() && ty_new != object_new
}

/// `__new__` for class-call dispatch, honoring CPython slot precedence: a
/// type carrying its OWN complete native `tp_new` slot (bool, list, range,
/// ...) is never shadowed by an ANCESTOR dict's `__new__` — CPython copies
/// slots at type-ready time, so `bool(x)` runs bool's constructor, not
/// `int.__dict__['__new__']`, even though bool linearizes over int.  A
/// `__new__` in the type's OWN dict still wins (the int/str data-type
/// carriers front their own constructors).  Constructed heap classes always
/// carry `tp_new == type_new` (types/type_.rs), so payload/dict-subclass
/// construction through an inherited data-type `__new__` is unaffected.
/// Returns NULL when the `tp_new` slot should drive construction.
unsafe fn mro_new_override(cls: *mut PyType) -> *mut PyObject {
    let name = crate::intern::intern("__new__");
    let user_new = unsafe { crate::descr::lookup_in_type(cls, name) };
    if user_new.is_null() {
        return ptr::null_mut();
    }
    let own_slot_is_native = unsafe { (*cls).tp_new }
        .is_some_and(|slot| slot as usize != type_::type_new as *const () as usize);
    if own_slot_is_native {
        let own_dict = unsafe { (*cls).tp_dict.cast::<type_::PyClassDict>() };
        let owned = !own_dict.is_null() && unsafe { (&*own_dict).get(name) }.is_some();
        if !owned {
            return ptr::null_mut();
        }
    }
    user_new
}

/// `object.__new__(cls, *args)` — the generic instance allocator, exposed as
/// a Python-visible staticmethod carrier.  Excess arguments are tolerated
/// (CPython tolerates them whenever `__init__` is overridden — mirroring the
/// permissive `object.__init__` above); allocation defers to the layout-aware
/// generic `type_new` (heap, dict-subclass, and payload-subclass instances).
unsafe extern "C" fn object_dunder_new_native(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argv.is_null() || argc == 0 {
        const MESSAGE: &str = "object.__new__(): not enough arguments";
        return unsafe { exc::pon_raise_type_error(MESSAGE.as_ptr(), MESSAGE.len()) };
    }
    let cls = unsafe { *argv };
    if unsafe { !type_::is_type_object(cls) } {
        const MESSAGE: &str = "object.__new__(X): X is not a type object";
        return unsafe { exc::pon_raise_type_error(MESSAGE.as_ptr(), MESSAGE.len()) };
    }
    unsafe { type_::type_new(cls.cast::<PyType>(), ptr::null_mut(), ptr::null_mut()) }
}

unsafe fn install_builtin_type(
    runtime: &mut Runtime,
    name: &'static str,
    ty: *mut PyType,
    constructor: Option<NewFunc>,
    object_type: *mut PyType,
) {
    if ty.is_null() {
        return;
    }
    unsafe {
        (*ty).ob_base.ob_type = runtime._type_type;
        if ty != object_type && (*ty).tp_base.is_null() {
            (*ty).tp_base = object_type;
        }
        if let Some(constructor) = constructor {
            (*ty).tp_new = Some(constructor);
        }
        runtime.globals.insert(crate::intern::intern(name), ty.cast::<PyObject>());
    }
}

type BuiltinConstructor = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

unsafe fn call_builtin_type_constructor(
    cls: *mut PyType,
    args: *mut PyObject,
    kwargs: *mut PyObject,
    constructor: BuiltinConstructor,
) -> *mut PyObject {
    if !kwargs.is_null() {
        let name = if cls.is_null() { "<unknown>" } else { unsafe { (*cls).name() } };
        return return_null_with_error(format!("builtin type '{name}' keyword arguments are not supported yet"));
    }
    let mut positional = match unsafe { type_::positional_args_from_object(args) } {
        Ok(positional) => positional,
        Err(message) => return return_null_with_error(message),
    };
    unsafe {
        constructor(
            if positional.is_empty() { ptr::null_mut() } else { positional.as_mut_ptr() },
            positional.len(),
        )
    }
}

pub(super) unsafe fn call_builtin_type_with_keywords(
    callee: *mut PyObject,
    positional: &[*mut PyObject],
    keywords: function::KeywordArgs<'_>,
) -> Option<*mut PyObject> {
    let name = unsafe { builtin_type_callee_name(callee)? };
    let constructor = builtin_constructor_for_name(name)?;
    let mut argv = match function::bind_native_keywords_for_name(name, positional, keywords) {
        Ok(argv) => argv,
        Err(message) => return Some(return_null_with_error(message)),
    };
    Some(unsafe {
        constructor(
            if argv.is_empty() { ptr::null_mut() } else { argv.as_mut_ptr() },
            argv.len(),
        )
    })
}

unsafe fn builtin_type_callee_name(callee: *mut PyObject) -> Option<&'static str> {
    if callee.is_null() {
        return None;
    }
    let meta = unsafe { (*callee).ob_type };
    if meta.is_null() || unsafe { (*meta).name() } != "type" {
        return None;
    }
    Some(unsafe { (*callee.cast::<PyType>()).name() })
}

fn builtin_constructor_for_name(name: &str) -> Option<BuiltinConstructor> {
    match name {
        "enumerate" => Some(crate::native::builtins_mod::builtin_enumerate),
        "zip" => Some(crate::native::builtins_batch::builtin_zip),
        "complex" => Some(crate::native::builtins_mod::builtin_complex),
        // `type(name, bases, ns, **kwds)`: class keywords ride to the
        // metaclass constructor through the phase-A binder's trailing marker.
        "type" => Some(crate::native::builtins_mod::builtin_type),
        // `property(fget=None, fset=None, fdel=None, doc=None)`: the binder
        // flattens keywords into the four positional slots (None = absent).
        "property" => Some(crate::native::builtins_mod::builtin_property),
        // `dict(*args, **kwargs)`: arbitrary keyword entries ride the
        // binder's trailing marker into `builtin_dict`, which merges them
        // after the positional source (argparse's `dict(kwargs, dest=...)`).
        "dict" => Some(crate::native::builtins_mod::builtin_dict),
        _ => None,
    }
}

/// Instantiates a class callee whose call site carries keyword arguments:
/// the keyword-aware sibling of `call_type_from_argv`.  A Python-level
/// `__new__` receives the keywords through the function binder; the
/// `__init__` leg binds them the same way.  Keywords reaching a slot-only
/// constructor are an error (CPython: `list(x=1)` raises TypeError).
pub(super) unsafe fn call_type_with_keywords(
    callee: *mut PyObject,
    args: &[*mut PyObject],
    keywords: function::KeywordArgs<'_>,
) -> *mut PyObject {
    let cls = callee.cast::<PyType>();
    // Exception classes never take the generic heap-instance path:
    // `BaseException.__new__` is keyword-blind and construction must end in
    // the boxed-exception machinery, so they get a dedicated leg.
    let is_exception_type = with_runtime(|runtime| unsafe {
        is_exception_subclass(cls.cast_const(), runtime.exception_types.base_exception.cast_const())
    })
    .unwrap_or(false);
    if is_exception_type {
        return unsafe { call_exception_type_with_keywords(callee, args, keywords) };
    }
    let user_new = unsafe { mro_new_override(cls) };
    let object_new = OBJECT_DUNDER_NEW_DESCRIPTOR.load(Ordering::Acquire);
    let instance = if user_new.is_null() || user_new == object_new {
        let new = unsafe { (*cls).tp_new.unwrap_or(type_::type_new) };
        let args_object = if args.is_empty() {
            ptr::null_mut()
        } else {
            match with_runtime(|runtime| seq::alloc_tuple_from_slice(runtime, args)) {
                Some(Ok(tuple)) => tuple,
                Some(Err(message)) => return return_null_with_error(message),
                None => return return_null_with_error("runtime is not initialized"),
            }
        };
        // Slot constructors receive the keywords as a real dict (CPython
        // `tp_new(cls, args, kwds)`); most reject non-NULL kwargs with their
        // own TypeError, which is the honest failure.
        let kwargs_object = match unsafe { keywords_as_dict(keywords) } {
            Ok(kwargs) => kwargs,
            Err(message) => return return_null_with_error(message),
        };
        let instance = unsafe { new(cls, args_object, kwargs_object) };
        if instance.is_null() {
            return ptr::null_mut();
        }
        instance
    } else {
        let callable = unsafe { crate::descr::descriptor_get(user_new, ptr::null_mut(), cls) };
        if callable.is_null() {
            return ptr::null_mut();
        }
        let mut new_argv = Vec::with_capacity(args.len().saturating_add(1));
        new_argv.push(callee);
        new_argv.extend_from_slice(args);
        let instance = if function::function_record(callable).is_some() {
            unsafe { call_phase_b_function(callable, &new_argv, keywords, None, None) }
        } else {
            match unsafe { function::call_bound_function(callable, &new_argv, keywords, None, None) } {
                Ok(result) => result,
                Err(message) => return return_null_with_error(message),
            }
        };
        if instance.is_null() {
            return ptr::null_mut();
        }
        // `__init__` only runs when `__new__` produced an instance of `cls`.
        let instance_type = unsafe { (*instance).ob_type.cast_mut() };
        if instance_type.is_null() || unsafe { !crate::mro::is_subtype(instance_type, cls) } {
            return instance;
        }
        instance
    };

    let new_overridden =
        !(user_new.is_null() || user_new == object_new) || unsafe { type_new_is_overridden(cls) };
    let init = unsafe { crate::descr::lookup_in_type(cls, crate::intern::intern("__init__")) };
    if init == OBJECT_DUNDER_INIT_CARRIER.load(Ordering::Acquire) {
        // The permissive terminus is not a user override.  With `__new__`
        // overridden CPython's `object.__init__` ignores the arguments — the
        // instance is complete; without it the keywords have nowhere to go.
        if !new_overridden {
            let name = unsafe { (*cls).name() };
            return return_null_with_error(format!("{name}() takes no keyword arguments"));
        }
        return instance;
    }
    if !init.is_null() {
        let mut positional = Vec::with_capacity(args.len().saturating_add(1));
        positional.push(instance);
        positional.extend_from_slice(args);
        let result = if function::function_record(init).is_some() {
            unsafe { call_phase_b_function(init, &positional, keywords, None, None) }
        } else {
            match unsafe { function::call_bound_function(init, &positional, keywords, None, None) } {
                Ok(result) => result,
                Err(message) => return return_null_with_error(message),
            }
        };
        if result.is_null() {
            return ptr::null_mut();
        }
    } else if !keywords.names.is_empty() {
        let name = unsafe { (*cls).name() };
        return return_null_with_error(format!("{name}() takes no keyword arguments"));
    } else if let Some(init_slot) = unsafe { (*cls).tp_init } {
        if unsafe { init_slot(instance, ptr::null_mut(), ptr::null_mut()) } < 0 {
            return ptr::null_mut();
        }
    }
    instance
}

/// Exception-class leg of [`call_type_with_keywords`], mirroring CPython's
/// `type_call` over the builtin exception machinery: `BaseException.__new__`
/// ignores keywords, so they must reach a user-defined `__init__`/`__new__`
/// (bound through the function binder) or the builtin init — where the
/// ImportError family binds `name=`/`path=`/`name_from=` and every other
/// builtin rejects them with CPython's typed TypeError
/// (`exc::apply_builtin_exception_keywords`).
unsafe fn call_exception_type_with_keywords(
    callee: *mut PyObject,
    args: &[*mut PyObject],
    keywords: function::KeywordArgs<'_>,
) -> *mut PyObject {
    let cls = callee.cast::<PyType>();
    let user_new = unsafe { crate::descr::lookup_in_type(cls, crate::intern::intern("__new__")) };
    let object_new = OBJECT_DUNDER_NEW_DESCRIPTOR.load(Ordering::Acquire);
    let has_user_new = !user_new.is_null() && user_new != object_new;
    let init = unsafe { crate::descr::lookup_in_type(cls, crate::intern::intern("__init__")) };
    let has_user_init = !init.is_null() && init != OBJECT_DUNDER_INIT_CARRIER.load(Ordering::Acquire);

    let instance = if has_user_new {
        // CPython type_call: a Python-level `__new__` (implicitly static) is
        // called unbound as `__new__(cls, *args, **kwargs)`.
        let callable = unsafe { crate::descr::descriptor_get(user_new, ptr::null_mut(), cls) };
        if callable.is_null() {
            return ptr::null_mut();
        }
        let mut new_argv = Vec::with_capacity(args.len().saturating_add(1));
        new_argv.push(callee);
        new_argv.extend_from_slice(args);
        // Both function flavors bind through `call_bound_function`; a
        // binding failure inside type_call is a TypeError in CPython, so
        // keep it typed — `except TypeError` must catch it and no raise
        // site may morph it.
        let instance = match unsafe { function::call_bound_function(callable, &new_argv, keywords, None, None) } {
            Ok(result) => result,
            Err(message) => {
                return exc::raise_kind_error_text(crate::types::exc::ExceptionKind::TypeError, &message);
            }
        };
        if instance.is_null() {
            return ptr::null_mut();
        }
        // `__init__` only runs when `__new__` produced an instance of `cls`.
        let instance_type = unsafe { (*instance).ob_type.cast_mut() };
        if instance_type.is_null() || unsafe { !crate::mro::is_subtype(instance_type, cls) } {
            return instance;
        }
        instance
    } else {
        // `BaseException.__new__`: keyword-blind, positional args become the
        // message/args payload (same builder as the positional call path).
        let is_group = with_runtime(|runtime| unsafe { runtime.exception_types.is_exception_group_type(cls.cast_const()) })
            .unwrap_or(false);
        let instance = if is_group {
            match with_runtime(|runtime| exc::build_exception_group_checked(runtime, cls, args)) {
                Some(instance) => instance,
                None => return return_null_with_error("runtime is not initialized"),
            }
        } else {
            exc::alloc_exception_instance(cls, args)
        };
        if instance.is_null() {
            return ptr::null_mut();
        }
        instance
    };

    if has_user_init {
        let mut positional = Vec::with_capacity(args.len().saturating_add(1));
        positional.push(instance);
        positional.extend_from_slice(args);
        // Typed for the same reason as the `__new__` leg above.
        let result = match unsafe { function::call_bound_function(init, &positional, keywords, None, None) } {
            Ok(result) => result,
            Err(message) => {
                return exc::raise_kind_error_text(crate::types::exc::ExceptionKind::TypeError, &message);
            }
        };
        if result.is_null() {
            return ptr::null_mut();
        }
        return instance;
    }
    if keywords.names.is_empty() {
        return instance;
    }
    exc::apply_builtin_exception_keywords(instance, keywords)
}

/// Materializes binder keywords as a Python dict for CPython-style
/// `tp_new(cls, args, kwds)` slots.  Empty keywords stay NULL.
unsafe fn keywords_as_dict(keywords: function::KeywordArgs<'_>) -> Result<*mut PyObject, String> {
    if keywords.names.is_empty() {
        return Ok(ptr::null_mut());
    }
    let mut pairs = Vec::with_capacity(keywords.names.len() * 2);
    for (&name, &value) in keywords.names.iter().zip(keywords.values.iter()) {
        let Some(spelling) = crate::intern::resolve(name) else {
            return Err(format!("keyword name id {name} is not interned"));
        };
        let key = unsafe { pon_const_str(spelling.as_ptr(), spelling.len()) };
        if key.is_null() {
            return Err("failed to allocate keyword name".to_owned());
        }
        pairs.push(key);
        pairs.push(value);
    }
    let dict = unsafe { map::pon_build_map(pairs.as_mut_ptr(), pairs.len() / 2) };
    if dict.is_null() {
        return Err("failed to allocate keyword dict".to_owned());
    }
    Ok(dict)
}

fn classmethod_builtin_type() -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(ptr::null(), "classmethod", mem::size_of::<classmethod::PyClassMethod>());
        classmethod::install_classmethod_slots(&mut ty);
        Box::into_raw(Box::new(ty)) as usize
    });
    *TYPE as *mut PyType
}

pub(crate) fn staticmethod_builtin_type() -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(ptr::null(), "staticmethod", mem::size_of::<classmethod::PyStaticMethod>());
        classmethod::install_staticmethod_slots(&mut ty);
        Box::into_raw(Box::new(ty)) as usize
    });
    *TYPE as *mut PyType
}

unsafe fn exact_one_descriptor_arg(
    args: *mut PyObject,
    kwargs: *mut PyObject,
    name: &str,
) -> Result<*mut PyObject, *mut PyObject> {
    if !kwargs.is_null() {
        return Err(return_null_with_error(format!("{name} keyword arguments are not supported yet")));
    }
    let positional = match unsafe { type_::positional_args_from_object(args) } {
        Ok(positional) => positional,
        Err(message) => return Err(return_null_with_error(message)),
    };
    if positional.len() != 1 {
        return Err(return_null_with_error(format!("{name} expected 1 argument, got {}", positional.len())));
    }
    Ok(positional[0])
}

unsafe extern "C" fn builtin_type_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_type) }
}

unsafe extern "C" fn builtin_object_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_object) }
}

unsafe extern "C" fn builtin_int_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_int) }
}

unsafe extern "C" fn builtin_str_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_str) }
}

unsafe extern "C" fn builtin_bool_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_bool) }
}

unsafe extern "C" fn builtin_float_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_float) }
}

unsafe extern "C" fn builtin_complex_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_complex) }
}

unsafe extern "C" fn builtin_bytes_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_bytes) }
}

unsafe extern "C" fn builtin_bytearray_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_bytearray) }
}

unsafe extern "C" fn builtin_memoryview_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_memoryview) }
}

unsafe extern "C" fn builtin_list_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_list) }
}

unsafe extern "C" fn builtin_tuple_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_tuple) }
}

unsafe extern "C" fn builtin_dict_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    if kwargs.is_null() {
        return unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_dict) };
    }
    // `dict(*args, **kwargs)`: keyword entries (already a str-keyed dict)
    // merge AFTER the positional mapping/iterable, so duplicate keys keep
    // CPython's keyword-wins update order.
    let positional = match unsafe { type_::positional_args_from_object(args) } {
        Ok(positional) => positional,
        Err(message) => return return_null_with_error(message),
    };
    if positional.len() > 1 {
        return return_null_with_error(format!("dict expected at most 1 argument, got {}", positional.len()));
    }
    let mut pairs = Vec::new();
    if let Some(&source) = positional.first() {
        if unsafe { crate::native::builtins_mod::collect_dict_update_pairs(source, &mut pairs) }.is_err() {
            return ptr::null_mut();
        }
    }
    if unsafe { crate::native::builtins_mod::collect_dict_update_pairs(kwargs, &mut pairs) }.is_err() {
        return ptr::null_mut();
    }
    unsafe { map::pon_build_map(pairs.as_mut_ptr(), pairs.len() / 2) }
}

unsafe extern "C" fn builtin_set_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_set) }
}

unsafe extern "C" fn builtin_frozenset_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_frozenset) }
}

unsafe extern "C" fn builtin_range_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_range) }
}

unsafe extern "C" fn builtin_enumerate_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_enumerate) }
}

unsafe extern "C" fn builtin_zip_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_zip) }
}

unsafe extern "C" fn builtin_map_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_map) }
}

unsafe extern "C" fn builtin_filter_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_filter) }
}

unsafe extern "C" fn builtin_property_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_property) }
}

unsafe extern "C" fn builtin_super_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    unsafe { call_builtin_type_constructor(_cls, args, kwargs, crate::native::builtins_mod::builtin_super) }
}

unsafe extern "C" fn builtin_classmethod_new(cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    match unsafe { exact_one_descriptor_arg(args, kwargs, "classmethod()") } {
        Ok(callable) => unsafe { classmethod::new_classmethod(cls, callable) },
        Err(error) => error,
    }
}

unsafe extern "C" fn builtin_staticmethod_new(cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    match unsafe { exact_one_descriptor_arg(args, kwargs, "staticmethod()") } {
        Ok(callable) => unsafe { classmethod::new_staticmethod(cls, callable) },
        Err(error) => error,
    }
}

fn alloc_long(runtime: &Runtime, value: i64) -> Result<*mut PyObject, String> {
    let object = runtime.heap.alloc(mem::size_of::<PyLong>(), TYPE_ID_LONG).cast::<PyLong>();
    // SAFETY: `object` points to a freshly allocated zeroed block of the right size.
    unsafe {
        ptr::write(
            object,
            PyLong {
                ob_base: PyObjectHeader::new(runtime.long_type),
                value,
            },
        );
    }
    Ok(as_object_ptr(object))
}

fn alloc_unicode(runtime: &Runtime, bytes: &[u8]) -> Result<*mut PyObject, String> {
    if core::str::from_utf8(bytes).is_err() {
        return Err("string constant is not valid UTF-8".to_owned());
    }

    let owned = bytes.to_vec().into_boxed_slice();
    let len = owned.len();
    let data = Box::into_raw(owned).cast::<u8>();
    let object = runtime.heap.alloc(mem::size_of::<PyUnicode>(), TYPE_ID_UNICODE).cast::<PyUnicode>();
    // SAFETY: `object` points to a freshly allocated zeroed block of the right size.
    unsafe {
        ptr::write(
            object,
            PyUnicode {
                ob_base: PyObjectHeader::new(runtime.unicode_type),
                len,
                data,
                owns_data: true,
            },
        );
    }
    Ok(as_object_ptr(object))
}

fn alloc_function(runtime: &Runtime, code: *const u8, arity: usize, name_interned: u32) -> Result<*mut PyObject, String> {
    if code.is_null() {
        return Err("function code pointer is null".to_owned());
    }

    let object = runtime.heap.alloc(mem::size_of::<PyFunction>(), TYPE_ID_FUNCTION).cast::<PyFunction>();
    // SAFETY: `object` points to a freshly allocated zeroed block of the right size.
    unsafe {
        ptr::write(object, PyFunction::new(runtime.function_type, code, arity, name_interned));
    }
    Ok(as_object_ptr(object))
}

pub(super) unsafe fn install_function_feedback(function: *mut PyObject, n_feedback: u32) -> Result<(), String> {
    if function.is_null() {
        return Err("cannot install feedback on NULL function".to_owned());
    }
    if n_feedback == 0 {
        return Ok(());
    }
    let len = usize::try_from(n_feedback).map_err(|_| "feedback cell count does not fit usize".to_owned())?;
    // SAFETY: The caller passes a freshly allocated PyFunction object.
    let function = unsafe { &*function.cast::<PyFunction>() };
    // SAFETY: The function is not yet reachable from compiled code during
    // installation, so replacing the optional feedback vector is exclusive.
    unsafe {
        *function.feedback.get() = Some(FeedbackVec::new(len));
    }
    Ok(())
}

fn ensure_runtime_initialized() -> Result<(), String> {
    init_runtime()
}

/// Records a thread-state error and returns the ABI NULL sentinel.
///
/// Fallible object helpers must use this path (or an equivalent raising helper)
/// instead of panicking or returning a non-NULL placeholder.
#[inline]
pub fn return_null_with_error(message: impl Into<String>) -> *mut PyObject {
    pon_err_set(message);
    ptr::null_mut()
}

/// Records a thread-state error and returns the ABI `-1` sentinel.
///
/// Predicate and status helpers use `-1` for failure so C-style callers can
/// distinguish errors from ordinary `0`/positive results.
#[inline]
pub fn return_minus_one_with_error(message: impl Into<String>) -> i32 {
    pon_err_set(message);
    -1
}

fn catch_object_helper(f: impl FnOnce() -> *mut PyObject) -> *mut PyObject {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(_) => return_null_with_error("runtime helper panicked"),
    }
}

fn catch_status_helper(f: impl FnOnce() -> i32) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(_) => return_minus_one_with_error("runtime helper panicked"),
    }
}

/// Records a unary operand shape into a non-NULL feedback cell.
///
/// Passing NULL is the tier-0 compatibility path and leaves helper behavior
/// byte-for-byte unchanged from the caller's perspective.
#[inline]
pub unsafe fn record_feedback_unary(feedback: *mut FeedbackCell, object: *mut PyObject) {
    if let Some(cell) = unsafe { feedback.as_ref() } {
        cell.record(unsafe { type_tag_for_object(object) }, TypeTag::Other);
    }
}

/// Records a binary operand shape into a non-NULL feedback cell.
///
/// Passing NULL is the tier-0 compatibility path and leaves helper behavior
/// byte-for-byte unchanged from the caller's perspective.
#[inline]
pub unsafe fn record_feedback_binary(feedback: *mut FeedbackCell, lhs: *mut PyObject, rhs: *mut PyObject) {
    if let Some(cell) = unsafe { feedback.as_ref() } {
        cell.record(unsafe { type_tag_for_object(lhs) }, unsafe { type_tag_for_object(rhs) });
    }
}

#[inline]
unsafe fn type_tag_for_object(object: *mut PyObject) -> TypeTag {
    if object.is_null() {
        return TypeTag::Other;
    }
    if unsafe { bool_::is_exact_bool(object) } {
        return TypeTag::Bool;
    }
    if unsafe { int::is_exact_int(object) } {
        return match unsafe { int::to_bigint(object) }.and_then(|value| num_traits::ToPrimitive::to_i64(&value)) {
            Some(_) => TypeTag::IntI64,
            None => TypeTag::Int,
        };
    }
    if unsafe { float::is_exact_float(object) } {
        return TypeTag::Float;
    }
    if unsafe { int::type_name_is(object, "str") } {
        return TypeTag::Str;
    }
    TypeTag::Other
}

/// Creates a boxed Phase-A integer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_const_int(value: i64) -> *mut PyObject {
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        match with_runtime(|runtime| alloc_long(runtime, value)) {
            Some(Ok(object)) => object,
            Some(Err(message)) => return_null_with_error(message),
            None => return_null_with_error("runtime is not initialized"),
        }
    })
}

/// Creates a boxed Phase-A UTF-8 string from raw bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_const_str(ptr: *const u8, len: usize) -> *mut PyObject {
    catch_object_helper(|| {
        if ptr.is_null() && len != 0 {
            return return_null_with_error("string pointer is null");
        }
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        let bytes = if len == 0 {
            &[]
        } else {
            // SAFETY: The caller supplies `len` bytes at non-null `ptr`.
            unsafe { core::slice::from_raw_parts(ptr, len) }
        };
        match with_runtime(|runtime| alloc_unicode(runtime, bytes)) {
            Some(Ok(object)) => object,
            Some(Err(message)) => return_null_with_error(message),
            None => return_null_with_error("runtime is not initialized"),
        }
    })
}

/// Adds two boxed Phase-A integers through the Phase-B binary dispatcher.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_binary_add(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(a, b);
    unsafe { number::pon_binary_op(crate::abstract_op::BINARY_ADD, a, b, ptr::null_mut()) }
}

enum CallTarget {
    Function {
        function: *mut PyFunction,
        code: *const u8,
        arity: usize,
    },
    Type,
    Method,
    Slot(crate::object::CallFunc),
    /// Instance of a class with no `tp_call`: dispatch through the type's
    /// `__call__` descriptor (CPython `slot_tp_call`).
    DunderCall,
}

/// Calls a boxed callable, including native builtins, heap types, and bound methods.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_call(callee: *mut PyObject, argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    crate::untag_prelude!(callee);
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        if argv.is_null() && argc != 0 {
            return return_null_with_error("argv pointer is null");
        }

        let target = match with_runtime(|runtime| unsafe {
            if is_exact_type(callee, runtime.function_type) {
                let function = callee.cast::<PyFunction>();
                let function_ref = &*function;
                Ok(CallTarget::Function {
                    function,
                    code: function_ref.entry.load(Ordering::Acquire).cast_const(),
                    arity: function_ref.arity,
                })
            } else if is_runtime_type_object(runtime, callee) {
                Ok(CallTarget::Type)
            } else if object_type_name(callee).as_deref() == Some("method") {
                Ok(CallTarget::Method)
            } else if !callee.is_null() && !(*callee).ob_type.is_null() && (*(*callee).ob_type).tp_call.is_some() {
                Ok(CallTarget::Slot((*(*callee).ob_type).tp_call.expect("checked Some")))
            } else if !callee.is_null() && !(*callee).ob_type.is_null() {
                Ok(CallTarget::DunderCall)
            } else {
                Err("callee is not callable".to_owned())
            }
        }) {
            Some(Ok(target)) => target,
            Some(Err(message)) => return return_null_with_error(message),
            None => return return_null_with_error("runtime is not initialized"),
        };

        match target {
            CallTarget::Function { function, code, arity } => {
                if function::function_record(callee).is_some() {
                    let positional = match unsafe { object_arg_slice(argv, argc) } {
                        Ok(values) => values,
                        Err(message) => return return_null_with_error(message),
                    };
                    let keywords = function::KeywordArgs {
                        names: &[],
                        values: &[],
                    };
                    unsafe { call_phase_b_function(callee, positional, keywords, None, None) }
                } else {
                    unsafe { call_code_pointer(function, code, arity, argv, argc) }
                }
            }
            CallTarget::Type => unsafe { call_type_from_argv(callee, argv, argc) },
            CallTarget::Method => unsafe { call_method_from_argv(callee, argv, argc) },
            CallTarget::Slot(call) => unsafe { call_slot_from_argv(callee, call, argv, argc) },
            CallTarget::DunderCall => {
                // SAFETY: The target selection proved `callee` and its type
                // are non-NULL live objects.
                let ty = unsafe { (*callee).ob_type.cast_mut() };
                let dunder = unsafe { crate::descr::lookup_in_type(ty, crate::intern::intern(crate::intern::DUNDER_CALL)) };
                if dunder.is_null() {
                    return return_null_with_error("callee is not callable");
                }
                let bound = unsafe { crate::descr::descriptor_get(dunder, callee, ty) };
                if bound.is_null() {
                    return ptr::null_mut();
                }
                unsafe { pon_call(bound, argv, argc) }
            }
        }
    })
}

unsafe fn object_arg_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Result<&'a [*mut PyObject], String> {
    if argv.is_null() && argc != 0 {
        return Err("argv pointer is null".to_owned());
    }
    Ok(if argc == 0 {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(argv.cast_const(), argc) }
    })
}

pub(super) unsafe fn call_phase_b_function(
    function_object: *mut PyObject,
    positional: &[*mut PyObject],
    keywords: function::KeywordArgs<'_>,
    star: Option<*mut PyObject>,
    dstar: Option<*mut PyObject>,
) -> *mut PyObject {
    match unsafe { function::call_bound_function(function_object, positional, keywords, star, dstar) } {
        Ok(result) => result,
        Err(message) => return_null_with_error(message),
    }
}

unsafe fn call_code_pointer(
    function: *mut PyFunction,
    code: *const u8,
    arity: usize,
    argv: *mut *mut PyObject,
    argc: usize,
) -> *mut PyObject {
    if arity != runtime_builtins::variadic_arity() && arity != argc {
        // A live `__defaults__` override (assigned after creation) can still
        // satisfy a short call: fill the trailing parameter slots and enter
        // through the filled argv.  Functions without an override keep the
        // raw arity check as their only tier-0 cost.
        let positional = match unsafe { object_arg_slice(argv, argc) } {
            Ok(values) => values,
            Err(message) => return return_null_with_error(message),
        };
        let Some(mut filled) =
            function::fill_positional_defaults(function.cast::<PyObject>(), positional, arity)
        else {
            return return_null_with_error(format!("function expected {arity} arguments, got {argc}"));
        };
        if code.is_null() {
            return return_null_with_error("function code pointer is null");
        }
        let filled_len = filled.len();
        return unsafe { call_bound_code_pointer(function, code, filled.as_mut_ptr(), filled_len) };
    }
    if code.is_null() {
        return return_null_with_error("function code pointer is null");
    }

    unsafe { call_bound_code_pointer(function, code, argv, argc) }
}

unsafe fn call_bound_code_pointer(
    function: *mut PyFunction,
    code: *const u8,
    argv: *mut *mut PyObject,
    argc: usize,
) -> *mut PyObject {
    if code.is_null() {
        return return_null_with_error("function code pointer is null");
    }

    unsafe { pon_tierup_bump_call(function) };
    let code = unsafe { (*function).entry.load(Ordering::Acquire).cast_const() };
    if code.is_null() {
        return return_null_with_error("function code pointer is null");
    }

    let _guard = CurrentFunctionGuard::push(function, argv, argc);
    let _handled_guard = HandledExcGuard::enter_clearing_pending();
    let entry: PyCodeFn = unsafe { mem::transmute(code) };
    let result = unsafe { entry(argv, argc) };
    if result.is_null() && !pon_err_occurred() {
        return return_null_with_error("call returned NULL without setting an exception");
    }
    result
}

pub(crate) struct CurrentFunctionGuard {
    pushed: bool,
}

impl CurrentFunctionGuard {
    pub(crate) fn push(function: *mut PyFunction, argv: *mut *mut PyObject, argc: usize) -> Self {
        if !function.is_null() {
            let caller_line = current_line();
            CURRENT_FUNCTION_STACK
                .with(|stack| stack.borrow_mut().push(CurrentCall { function, argv, argc, caller_line }));
            return Self { pushed: true };
        }
        Self { pushed: false }
    }
}

impl Drop for CurrentFunctionGuard {
    fn drop(&mut self) {
        if self.pushed {
            let caller_line = CURRENT_FUNCTION_STACK.with(|stack| stack.borrow_mut().pop().map(|call| call.caller_line));
            if let Some(line) = caller_line {
                // The callee's stores are stale once it returned: restore the
                // caller's statement line for post-call raises.
                set_current_line(line);
            }
        }
    }
}

/// Save/restore bracket for `PonThreadState::handled_exc` around one
/// compiled-code call.  The callee inherits the caller's handled exception
/// (CPython: `sys.exception()` is thread-wide while a handler runs); on
/// return the caller's view is restored, so a handler parked inside the
/// callee never leaks into the caller's frame.  The save rides a
/// thread-state stack (not a guard local) to stay visible to the precise GC
/// root scan.
pub(crate) struct HandledExcGuard;

impl HandledExcGuard {
    pub(crate) fn enter() -> Self {
        let mut state = thread_state_lock();
        let current = state.handled_exc;
        state.handled_exc_saves.push(current);
        Self
    }

    /// Single-lock fusion of [`Self::enter`] and `pon_err_clear` for the hot
    /// call boundaries: one thread-state acquisition saves the caller's
    /// handled exception AND clears the pending-error state the callee must
    /// start without.
    pub(crate) fn enter_clearing_pending() -> Self {
        let mut state = thread_state_lock();
        let current = state.handled_exc;
        state.handled_exc_saves.push(current);
        state.current_exc = ptr::null_mut();
        state.clear_diagnostic();
        Self
    }
}

impl Drop for HandledExcGuard {
    fn drop(&mut self) {
        let mut state = thread_state_lock();
        let restored = state.handled_exc_saves.pop().unwrap_or(ptr::null_mut());
        state.handled_exc = restored;
    }
}

fn current_function_for_tierup() -> *mut PyFunction {
    CURRENT_FUNCTION_STACK
        .with(|stack| stack.borrow().last().map(|call| call.function))
        .unwrap_or(ptr::null_mut())
}

pub(crate) fn current_function_object() -> *mut PyObject {
    current_function_for_tierup().cast::<PyObject>()
}

pub(crate) fn push_current_call(function: *mut PyFunction, argv: *mut *mut PyObject, argc: usize) -> CurrentFunctionGuard {
    CurrentFunctionGuard::push(function, argv, argc)
}


pub(crate) fn current_call_snapshots() -> Vec<(*mut PyObject, *mut PyObject)> {
    CURRENT_FUNCTION_STACK.with(|stack| {
        stack
            .borrow()
            .iter()
            .rev()
            .filter_map(|call| {
                if call.argv.is_null() || call.argc == 0 {
                    return None;
                }
                let first_arg = unsafe { *call.argv };
                (!first_arg.is_null()).then_some((call.function.cast::<PyObject>(), first_arg))
            })
            .collect()
    })
}

#[inline]
fn bump_saturating(counter: &std::sync::atomic::AtomicU32) -> u32 {
    let previous = counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            Some(value.saturating_add(1))
        })
        .unwrap_or(u32::MAX);
    previous.saturating_add(1)
}

fn backoff_shift(function: &PyFunction) -> u32 {
    u32::from(function.tier_epoch.load(Ordering::Acquire)).min(DEOPT_BACKOFF_MAX_SHIFT)
}

fn backed_off_threshold(base: u32, function: &PyFunction) -> u32 {
    base.saturating_mul(1_u32 << backoff_shift(function))
}

fn maybe_queue_tierup(function: *mut PyFunction, reason: u32) {
    if function.is_null() {
        return;
    }
    let hook = TIERUP_HOOK.load(Ordering::Acquire);
    if hook.is_null() {
        return;
    }
    // SAFETY: `function` is a live PyFunction while a runtime call/backedge is
    // executing. The CAS ensures one transition into the queued state.
    let queued = unsafe {
        let function_ref = &*function;
        match function_ref.tier_state.load(Ordering::Acquire) {
            TIER_STATE_TIER0 => function_ref
                .tier_state
                .compare_exchange(TIER_STATE_TIER0, TIER_STATE_QUEUED, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            TIER_STATE_DEFERRED
                if function_ref.hotness.load(Ordering::Acquire)
                    >= backed_off_threshold(TIER1_DEFERRED_CALL_THRESHOLD, function_ref)
                    || function_ref.loop_hotness.load(Ordering::Acquire)
                        >= backed_off_threshold(TIER1_LOOP_THRESHOLD, function_ref) =>
            {
                function_ref
                    .tier_state
                    .compare_exchange(TIER_STATE_DEFERRED, TIER_STATE_QUEUED, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            }
            _ => false,
        }
    };
    if queued {
        // SAFETY: The hook is installed through `pon_tierup_set_hook` with the
        // `TierUpHook` ABI contract.
        let hook: TierUpHook = unsafe { mem::transmute(hook) };
        unsafe { hook(function, reason) };
    }
}

/// Installs or clears the runtime-to-tier-up hook.
///
/// Passing NULL clears the hook. Non-NULL values must point at a `TierUpHook`
/// function; `pon-runtime` keeps the pointer opaque to avoid depending on JIT.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_tierup_set_hook(hook: *mut ()) {
    TIERUP_HOOK.store(hook, Ordering::Release);
}

/// Installs or clears the tier-up pin-root provider.
///
/// Passing NULL clears the hook. Non-NULL values must point at a
/// `TierUpRootHook` function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_tierup_set_root_hook(hook: *mut ()) {
    TIERUP_ROOT_HOOK.store(hook, Ordering::Release);
}

/// Function-entry tier-up probe. Bumps hotness and queues tier-up once hot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_tierup_bump_call(function: *mut PyFunction) {
    if function.is_null() {
        return;
    }
    // SAFETY: Non-NULL callers pass a live PyFunction object.
    let function_ref = unsafe { &*function };
    let hotness = bump_saturating(&function_ref.hotness);
    match function_ref.tier_state.load(Ordering::Acquire) {
        TIER_STATE_TIER0 if hotness >= TIER1_CALL_THRESHOLD => maybe_queue_tierup(function, 0),
        TIER_STATE_DEFERRED if hotness >= backed_off_threshold(TIER1_DEFERRED_CALL_THRESHOLD, function_ref) => {
            maybe_queue_tierup(function, 0);
        }
        _ => {}
    }
}

/// Loop back-edge probe + OSR gate for tier-0 code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_osr_poll(loop_header: u32) -> *const u8 {
    let function = current_function_for_tierup();
    if function.is_null() {
        return ptr::null();
    }
    // SAFETY: The current-function stack only contains live frames while their
    // compiled entrypoint is executing.
    let function_ref = unsafe { &*function };
    let hotness = bump_saturating(&function_ref.loop_hotness);
    match function_ref.tier_state.load(Ordering::Acquire) {
        TIER_STATE_TIER0 if hotness >= TIER1_LOOP_THRESHOLD => {
            maybe_queue_tierup(function, loop_header.saturating_add(1));
        }
        TIER_STATE_DEFERRED if hotness >= backed_off_threshold(TIER1_LOOP_THRESHOLD, function_ref) => {
            maybe_queue_tierup(function, loop_header.saturating_add(1));
        }
        _ => {}
    }

    if function_ref.tier_state.load(Ordering::Acquire) != TIER_STATE_TIER1 {
        return ptr::null();
    }
    let entry = function_ref.osr_entry.load(Ordering::Acquire);
    if !entry.is_null() && function_ref.osr_loop_header.load(Ordering::Relaxed) == loop_header {
        entry.cast_const()
    } else {
        ptr::null()
    }
}

/// Backward-compatible loop probe without OSR transfer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_backedge_poll() {
    let _ = unsafe { pon_osr_poll(0) };
}

/// Notes one tier-1 fast-path-to-cold-twin transfer and applies deopt back-off.
#[unsafe(no_mangle)]
// TAG-OK: `function` is a PyFunction pointer sentinel, not a Python value.
pub unsafe extern "C" fn pon_deopt_note(function: *mut PyObject) -> i32 {
    let function = if function.is_null() {
        current_function_for_tierup()
    } else {
        function.cast::<PyFunction>()
    };
    if function.is_null() {
        return 0;
    }
    // SAFETY: Non-NULL callers pass a PyFunction pointer or use the current call.
    let function_ref = unsafe { &*function };
    let n = function_ref.deopt_count.fetch_add(1, Ordering::AcqRel).saturating_add(1);
    if n < DEOPT_THRASH_THRESHOLD {
        return 0;
    }
    let epoch = function_ref.tier_epoch.load(Ordering::Acquire);
    let target = if epoch >= DEOPT_PIN_EPOCH {
        TIER_STATE_DISABLED
    } else {
        TIER_STATE_DEFERRED
    };
    if function_ref
        .tier_state
        .compare_exchange(TIER_STATE_TIER1, target, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        function_ref.entry.store(function_ref.code.cast_mut(), Ordering::Release);
        let _ = function_ref
            .tier_epoch
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| Some(value.saturating_add(1)));
        function_ref.deopt_count.store(0, Ordering::Release);
        function_ref.hotness.store(0, Ordering::Release);
        function_ref.loop_hotness.store(0, Ordering::Release);
    }
    0
}

unsafe fn call_method_from_argv(callee: *mut PyObject, argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { argv_slice(argv, argc) } {
        Ok(args) => args,
        Err(message) => return return_null_with_error(message),
    };
    let (function, receiver) = match unsafe { crate::types::method::split_bound_method(callee.cast()) } {
        Ok(pair) => pair,
        Err(message) => return return_null_with_error(message),
    };
    let mut positional = Vec::with_capacity(argc.saturating_add(1));
    positional.push(receiver);
    positional.extend_from_slice(args);
    let keywords = function::KeywordArgs {
        names: &[],
        values: &[],
    };
    match unsafe { function::call_bound_function(function, &positional, keywords, None, None) } {
        Ok(result) => result,
        Err(message) => return_null_with_error(message),
    }
}

unsafe fn call_slot_from_argv(
    callee: *mut PyObject,
    call: crate::object::CallFunc,
    argv: *mut *mut PyObject,
    argc: usize,
) -> *mut PyObject {
    let args = match unsafe { argv_slice(argv, argc) } {
        Ok(args) => args,
        Err(message) => return return_null_with_error(message),
    };
    let args_object = if args.is_empty() {
        ptr::null_mut()
    } else {
        match with_runtime(|runtime| seq::alloc_tuple_from_slice(runtime, args)) {
            Some(Ok(tuple)) => tuple,
            Some(Err(message)) => return return_null_with_error(message),
            None => return return_null_with_error("runtime is not initialized"),
        }
    };
    unsafe { call(callee, args_object, ptr::null_mut()) }
}

unsafe fn call_type_from_argv(callee: *mut PyObject, argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { argv_slice(argv, argc) } {
        Ok(args) => args,
        Err(message) => return return_null_with_error(message),
    };
    let cls = callee.cast::<PyType>();
    let is_exception_type = with_runtime(|runtime| unsafe {
        is_exception_subclass(cls.cast_const(), runtime.exception_types.base_exception.cast_const())
    })
    .unwrap_or(false);
    if is_exception_type {
        // Exception classes share the keyword sibling's dedicated leg: it
        // runs user `__new__`/`__init__` overrides (pickle's `_Stop(value)`
        // stores `self.value` in one) and falls back to the keyword-blind
        // builtin construction — group check plus `alloc_exception_instance`,
        // exactly what this branch previously inlined — otherwise.
        let keywords = function::KeywordArgs {
            names: &[],
            values: &[],
        };
        return unsafe { call_exception_type_with_keywords(callee, args, keywords) };
    }

    let user_new = unsafe { mro_new_override(cls) };
    // Object's generic `__new__` carrier is not a user override: keep the
    // default `tp_new` slot path (builtin containers and exception layouts
    // depend on their slot allocators, not the heap-instance fallback).
    let object_new = OBJECT_DUNDER_NEW_DESCRIPTOR.load(Ordering::Acquire);
    let instance = if user_new.is_null() || user_new == object_new {
        let new = unsafe { (*cls).tp_new.unwrap_or(type_::type_new) };
        let args_object = if args.is_empty() {
            ptr::null_mut()
        } else {
            match with_runtime(|runtime| seq::alloc_tuple_from_slice(runtime, args)) {
                Some(Ok(tuple)) => tuple,
                Some(Err(message)) => return return_null_with_error(message),
                None => return return_null_with_error("runtime is not initialized"),
            }
        };
        let instance = unsafe { new(cls, args_object, ptr::null_mut()) };
        if instance.is_null() {
            return ptr::null_mut();
        }
        // Builtin native constructors (tp_new != type_new) perform COMPLETE
        // construction: the returned object is fully initialized from `args`.
        // The class-dict `__init__` installed for heap subclasses resolving
        // through the builtin's MRO (dict/list surfaces) must not run a
        // second construction pass — `list(map(...))` would re-consume the
        // exhausted iterator and replace the contents with the empty tail.
        if new as *const () as usize != type_::type_new as *const () as usize {
            return instance;
        }
        instance
    } else {
        // CPython type_call: a Python-level `__new__` (implicitly static) is
        // called unbound as `__new__(cls, *args)`.
        let callable = unsafe { crate::descr::descriptor_get(user_new, ptr::null_mut(), cls) };
        if callable.is_null() {
            return ptr::null_mut();
        }
        let mut new_argv = Vec::with_capacity(args.len().saturating_add(1));
        new_argv.push(callee);
        new_argv.extend_from_slice(args);
        let instance = unsafe { pon_call(callable, new_argv.as_mut_ptr(), new_argv.len()) };
        if instance.is_null() {
            return ptr::null_mut();
        }
        // `__init__` only runs when `__new__` produced an instance of `cls`.
        let instance_type = unsafe { (*instance).ob_type.cast_mut() };
        if instance_type.is_null() || unsafe { !crate::mro::is_subtype(instance_type, cls) } {
            return instance;
        }
        instance
    };

    let init = unsafe { crate::descr::lookup_in_type(cls, crate::intern::intern("__init__")) };
    if !init.is_null() {
        let init_is_function = with_runtime(|runtime| unsafe { is_exact_type(init, runtime.function_type) }).unwrap_or(false);
        if !init_is_function {
            return return_null_with_error("type __init__ is not callable");
        }
        let mut positional = Vec::with_capacity(argc.saturating_add(1));
        positional.push(instance);
        positional.extend_from_slice(args);
        let keywords = function::KeywordArgs {
            names: &[],
            values: &[],
        };
        match unsafe { function::call_bound_function(init, &positional, keywords, None, None) } {
            Ok(result) => {
                if result.is_null() {
                    return ptr::null_mut();
                }
            }
            Err(message) => return return_null_with_error(message),
        }
    } else if let Some(init_slot) = unsafe { (*cls).tp_init } {
        if unsafe { init_slot(instance, ptr::null_mut(), ptr::null_mut()) } < 0 {
            return ptr::null_mut();
        }
    }
    instance
}

unsafe fn argv_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Result<&'a [*mut PyObject], String> {
    if argv.is_null() && argc != 0 {
        return Err("argv pointer is null".to_owned());
    }
    if argc == 0 {
        Ok(&[])
    } else {
        Ok(unsafe { core::slice::from_raw_parts(argv.cast_const(), argc) })
    }
}

unsafe fn is_runtime_type_object(runtime: &Runtime, object: *mut PyObject) -> bool {
    if object.is_null() {
        return false;
    }
    let meta = unsafe { (*object).ob_type.cast_mut() };
    !meta.is_null() && unsafe { crate::mro::is_subtype(meta, runtime._type_type) }
}

unsafe fn object_type_name(object: *mut PyObject) -> Option<String> {
    if object.is_null() {
        return None;
    }
    let ty = unsafe { (*object).ob_type };
    if ty.is_null() {
        None
    } else {
        Some(unsafe { (*ty).name() }.to_owned())
    }
}

/// Loads the builtin `__build_class__` callable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_load_build_class() -> *mut PyObject {
    unsafe { pon_load_global(crate::intern::intern("__build_class__"), ptr::null_mut()) }
}

/// Builds a heap Python class from an already-emitted body function and bases.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_build_class(
    body: *mut PyObject,
    name_interned: u32,
    bases: *const *mut PyObject,
    base_count: usize,
) -> *mut PyObject {
    crate::untag_prelude!(body);
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        if bases.is_null() && base_count != 0 {
            return return_null_with_error("class bases pointer is null");
        }
        let Some(name) = crate::intern::resolve(name_interned) else {
            return return_null_with_error(format!("class name id {name_interned} is not interned"));
        };
        let base_slice = if base_count == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(bases, base_count) }
        };
        unsafe { build_class_with_body(body, &name, base_slice, &[]) }
    })
}

/// Builds a heap Python class with class-statement keyword arguments.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_build_class_ex(
    body: *mut PyObject,
    name_interned: u32,
    bases: *const *mut PyObject,
    base_count: usize,
    kw_names: *const u32,
    kw_values: *mut *mut PyObject,
    kw_count: usize,
) -> *mut PyObject {
    crate::untag_prelude!(body);
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        if bases.is_null() && base_count != 0 {
            return return_null_with_error("class bases pointer is null");
        }
        if kw_names.is_null() && kw_count != 0 {
            return return_null_with_error("class keyword names pointer is null");
        }
        if kw_values.is_null() && kw_count != 0 {
            return return_null_with_error("class keyword values pointer is null");
        }
        let Some(name) = crate::intern::resolve(name_interned) else {
            return return_null_with_error(format!("class name id {name_interned} is not interned"));
        };
        let mut base_vec = if base_count == 0 {
            Vec::new()
        } else {
            unsafe { core::slice::from_raw_parts(bases, base_count) }.to_vec()
        };
        for base in &mut base_vec {
            let original = *base;
            let normalized = crate::tag::untag_arg(original);
            if crate::tag::is_small_int(original) && normalized.is_null() {
                return ptr::null_mut();
            }
            *base = normalized;
        }
        let names = if kw_count == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(kw_names, kw_count) }
        };
        let mut values = if kw_count == 0 {
            Vec::new()
        } else {
            unsafe { core::slice::from_raw_parts(kw_values, kw_count) }.to_vec()
        };
        for value in &mut values {
            let original = *value;
            let normalized = crate::tag::untag_arg(original);
            if crate::tag::is_small_int(original) && normalized.is_null() {
                return ptr::null_mut();
            }
            *value = normalized;
        }
        let keywords = names
            .iter()
            .copied()
            .zip(values.iter().copied())
            .map(|(name, value)| type_::ClassKeyword { name, value })
            .collect::<Vec<_>>();
        unsafe { build_class_with_body(body, &name, &base_vec, &keywords) }
    })
}

/// Builds a heap Python class through the dynamic construction path:
/// bases arrive as one already-splatted list object (`class C(*bases)`) and
/// the `**kwds` mapping stays whole, flattened here into interned keywords
/// after the static ones — exactly how `pon_call_ex` materializes call-site
/// `**` operands.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_build_class_full(
    body: *mut PyObject,
    name_interned: u32,
    bases_seq: *mut PyObject,
    kw_names: *const u32,
    kw_values: *mut *mut PyObject,
    kw_count: usize,
    dstar: *mut PyObject,
) -> *mut PyObject {
    crate::untag_prelude!(body, bases_seq, dstar);
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        if bases_seq.is_null() {
            return return_null_with_error("class bases sequence is null");
        }
        if kw_names.is_null() && kw_count != 0 {
            return return_null_with_error("class keyword names pointer is null");
        }
        if kw_values.is_null() && kw_count != 0 {
            return return_null_with_error("class keyword values pointer is null");
        }
        let Some(name) = crate::intern::resolve(name_interned) else {
            return return_null_with_error(format!("class name id {name_interned} is not interned"));
        };
        let mut base_vec = match unsafe { crate::types::function::positional_args_from_star(bases_seq) } {
            Ok(bases) => bases,
            Err(message) => return return_null_with_error(message),
        };
        for base in &mut base_vec {
            let original = *base;
            let normalized = crate::tag::untag_arg(original);
            if crate::tag::is_small_int(original) && normalized.is_null() {
                return ptr::null_mut();
            }
            *base = normalized;
        }
        let mut names = if kw_count == 0 {
            Vec::new()
        } else {
            unsafe { core::slice::from_raw_parts(kw_names, kw_count) }.to_vec()
        };
        let mut values = if kw_count == 0 {
            Vec::new()
        } else {
            unsafe { core::slice::from_raw_parts(kw_values, kw_count) }.to_vec()
        };
        if !dstar.is_null() {
            if let Err(message) =
                unsafe { crate::types::function::extend_keywords_from_mapping(body, dstar, &mut names, &mut values) }
            {
                return return_null_with_error(message);
            }
        }
        // Untag AFTER the `**` extend so mapping-sourced values get the same
        // normalization as static ones (untagging a heap pointer is identity).
        for value in &mut values {
            let original = *value;
            let normalized = crate::tag::untag_arg(original);
            if crate::tag::is_small_int(original) && normalized.is_null() {
                return ptr::null_mut();
            }
            *value = normalized;
        }
        let keywords = names
            .iter()
            .copied()
            .zip(values.iter().copied())
            .map(|(name, value)| type_::ClassKeyword { name, value })
            .collect::<Vec<_>>();
        unsafe { build_class_with_body(body, &name, &base_vec, &keywords) }
    })
}

/// Shared `__build_class__` core: runs the `__prepare__` protocol, executes
/// the body into the prepared scope, and dispatches class construction.
unsafe fn build_class_with_body(
    body: *mut PyObject,
    name: &str,
    bases: &[*mut PyObject],
    keywords: &[type_::ClassKeyword],
) -> *mut PyObject {
    let scope = match unsafe { type_::prepare_class_scope(name, bases, keywords) } {
        Ok(scope) => scope,
        Err(()) => return ptr::null_mut(),
    };
    let frame = if scope.mapping.is_null() {
        ClassBodyFrame {
            namespace: type_::new_namespace(),
            mapping: ptr::null_mut(),
            orig_bases: scope.orig_bases,
        }
    } else {
        ClassBodyFrame {
            namespace: ptr::null_mut(),
            mapping: scope.mapping,
            orig_bases: scope.orig_bases,
        }
    };
    if with_runtime(|runtime| runtime.class_namespace_stack.push(frame)).is_none() {
        return return_null_with_error("runtime is not initialized");
    }
    let body_result = if body.is_null() {
        ptr::null_mut()
    } else {
        unsafe { pon_call(body, ptr::null_mut(), 0) }
    };
    // Class-construction GC window (body-frame pop → `construct_class`): once
    // the body frame pops below, the namespace/mapping pair is reachable only
    // through Rust locals while metaclass hooks (`__new__`/`__init__`),
    // `__set_name__`, and `__init_subclass__` run arbitrary Python that may
    // `gc.collect()`.  Push the pair onto the scoped construction registry
    // BEFORE the pop (no unrooted instant) and keep it there until the
    // constructed class (or an error) leaves this frame; `construct_class`
    // publishes the namespace via `register_namespaced_type` before those
    // hooks fire, so this entry covers the whole remaining window.
    if with_runtime(|runtime| runtime.class_construction_stack.push(frame)).is_none() {
        return return_null_with_error("runtime is not initialized");
    }
    let _construction_root = ClassConstructionRootGuard;
    let popped = with_runtime(|runtime| runtime.class_namespace_stack.pop()).flatten();
    if popped != Some(frame) {
        return return_null_with_error("class namespace stack is corrupted");
    }
    if !body.is_null() && body_result.is_null() {
        return ptr::null_mut();
    }
    // PEP 560: publish the pre-resolution bases through the class namespace
    // so construction exposes them as `__orig_bases__` (CPython
    // `__build_class__` stores the original tuple exactly when
    // `update_bases` changed anything).
    if !frame.orig_bases.is_null() {
        if frame.mapping.is_null() {
            unsafe { (&mut *frame.namespace).set(crate::intern::intern("__orig_bases__"), frame.orig_bases) };
        } else {
            let key = unsafe { class_mapping_key(crate::intern::intern("__orig_bases__")) };
            if key.is_null() {
                return ptr::null_mut();
            }
            if unsafe { map::pon_subscript_set(frame.mapping, key, frame.orig_bases) }.is_null() {
                return ptr::null_mut();
            }
        }
    }
    let class = if scope.mapping.is_null() {
        unsafe { type_::build_class_from_namespace(name, &scope.bases, frame.namespace, keywords) }
    } else {
        unsafe { type_::build_class_from_prepared_mapping(name, &scope.bases, scope.mapping, keywords) }
    };
    if class.is_null() {
        return ptr::null_mut();
    }
    let _ = with_runtime(|runtime| unsafe {
        if (*class).ob_type.is_null() {
            (*class).ob_type = runtime._type_type.cast_const();
        }
    });
    class
}

/// Interned name → runtime string key for `__prepare__` mapping operations.
unsafe fn class_mapping_key(name_interned: u32) -> *mut PyObject {
    let Some(spelling) = resolve(name_interned) else {
        return return_null_with_error(format!("name id {name_interned} is not interned"));
    };
    unsafe { pon_const_str(spelling.as_ptr(), spelling.len()) }
}

/// True when the pending exception state allows a name-lookup fallthrough:
/// nothing pending, or a KeyError from the mapping probe (cleared).
fn clear_mapping_miss() -> bool {
    if exc::pending_exception_object().is_none() || exc::pending_exception_is("KeyError") {
        pon_err_clear();
        return true;
    }
    false
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_setup_annotations() -> *mut PyObject {
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        let name = crate::intern::intern("__annotations__");
        let Some(frame) = with_runtime(|runtime| runtime.class_namespace_stack.last().copied()) else {
            return return_null_with_error("runtime is not initialized");
        };
        if let Some(frame) = frame {
            if !frame.mapping.is_null() {
                // `__prepare__` mapping scope: read/write through the mapping
                // protocol so user mappings observe the annotations dict.
                let key = unsafe { class_mapping_key(name) };
                if key.is_null() {
                    return ptr::null_mut();
                }
                let existing = unsafe { crate::abstract_op::subscript_get(frame.mapping, key) };
                if !existing.is_null() {
                    return existing;
                }
                if !clear_mapping_miss() {
                    return ptr::null_mut();
                }
                let annotations = unsafe { map::pon_build_map(ptr::null_mut(), 0) };
                if annotations.is_null() {
                    return annotations;
                }
                return unsafe { map::pon_subscript_set(frame.mapping, key, annotations) };
            }
            if let Some(existing) = unsafe { (&*frame.namespace).get(name) } {
                return existing;
            }
            let annotations = unsafe { map::pon_build_map(ptr::null_mut(), 0) };
            if annotations.is_null() {
                return annotations;
            }
            unsafe {
                (&mut *frame.namespace).set(name, annotations);
            }
            return annotations;
        }
        // Module scope: `__annotations__` is a per-module global (CPython
        // semantics), never a flat-pool binding — the flat map is the
        // builtins table, and a shared entry would merge every module's
        // annotations into the first binder's dict.
        let target_module = current_defining_module().or_else(crate::import::active_module_name_id);
        if let Some(existing) = match target_module {
            Some(module) => crate::import::module_attr(module, name),
            // No module context (embedding/unit tests): flat-pool last resort.
            None => with_runtime(|runtime| runtime.globals.get(&name).copied()).flatten(),
        } {
            return existing;
        }
        let annotations = unsafe { map::pon_build_map(ptr::null_mut(), 0) };
        if annotations.is_null() {
            return annotations;
        }
        match target_module {
            Some(module) => {
                // Bumps the namespace version (J0.3 GlobalIC site).
                crate::import::store_module_attr(module, name, annotations);
                crate::dynexec::sync_global_store_for_module(module, name, annotations);
            }
            None => {
                let _ = with_runtime(|runtime| {
                    runtime.globals.insert(name, annotations);
                    // J0.3 GlobalIC site: context-less __annotations__ insert.
                    bump_namespace_version();
                });
            }
        }
        match with_runtime(|runtime| ensure_module_annotate_function(runtime, target_module)) {
            Some(Ok(())) => annotations,
            Some(Err(message)) => return_null_with_error(message),
            None => return_null_with_error("runtime is not initialized"),
        }
    })
}

fn ensure_module_annotate_function(runtime: &mut Runtime, target_module: Option<u32>) -> Result<(), String> {
    let name = crate::intern::intern("__annotate__");
    let already_bound = match target_module {
        Some(module) => crate::import::module_attr(module, name).is_some(),
        None => runtime.globals.contains_key(&name),
    };
    if already_bound {
        return Ok(());
    }
    let function = alloc_function(runtime, module_annotations_annotate as *const u8, 1, name)?;
    match target_module {
        Some(module) => {
            // Bumps the namespace version (J0.3 GlobalIC site).
            crate::import::store_module_attr(module, name, function);
        }
        None => {
            runtime.globals.insert(name, function);
            // J0.3 GlobalIC site: context-less __annotate__ registration.
            bump_namespace_version();
        }
    }
    Ok(())
}

unsafe extern "C" fn module_annotations_annotate(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 {
        return return_null_with_error("__annotate__ expects exactly one format argument");
    }
    let name = crate::intern::intern("__annotations__");
    crate::import::active_module_attr(name)
        .or_else(|| with_runtime(|runtime| runtime.globals.get(&name).copied()).flatten())
        .unwrap_or_else(|| unsafe { pon_setup_annotations() })
}

fn raise_unbound_local_error() -> *mut PyObject {
    const MESSAGE: &str = "cannot access local variable where it is not associated with a value";
    if let Err(message) = ensure_runtime_initialized() {
        return return_null_with_error(message);
    }
    match with_runtime(|runtime| {
        let message = match alloc_unicode(runtime, MESSAGE.as_bytes()) {
            Ok(message) => message,
            Err(message) => return return_null_with_error(message),
        };
        match exc::alloc_exception_object(
            runtime,
            runtime
                .exception_types
                .get(crate::types::exc::ExceptionKind::UnboundLocalError),
            message,
            ptr::null_mut(),
        ) {
            Ok(exception) => unsafe { exc::pon_raise(exception, ptr::null_mut()) },
            Err(message) => return_null_with_error(message),
        }
    }) {
        Some(result) => result,
        None => return_null_with_error("runtime is not initialized"),
    }
}

/// Loads a local-slot value, raising when the slot is currently unbound.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_load_local(value: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(value);
    catch_object_helper(|| {
        if value.is_null() {
            raise_unbound_local_error()
        } else {
            value
        }
    })
}

/// Deletes a local-slot value, raising when the slot is already unbound.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_delete_local(value: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(value);
    catch_object_helper(|| {
        if value.is_null() {
            raise_unbound_local_error()
        } else {
            unsafe { pon_none() }
        }
    })
}

/// Deletes a module-global binding by interned name.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_delete_global(name_interned: u32) -> *mut PyObject {
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        // Defining-module scoping, mirrored from `pon_store_global`.
        let target_module = current_defining_module().or_else(crate::import::active_module_name_id);
        let removed = match target_module {
            // CPython rule: `del name` unbinds the module global only.  The
            // flat pool is the builtins table and is never touched — deleting
            // an unbound global raises NameError even when a builtin of the
            // same name exists, and deleting a module-local shadow re-exposes
            // the builtin instead of evicting it process-wide.
            // `delete_module_attr` bumps the namespace version (J0.3).
            Some(module) => {
                let removed = crate::import::delete_module_attr(module, name_interned);
                if removed {
                    crate::dynexec::sync_global_delete_for_module(module, name_interned);
                }
                removed
            }
            // No module context (embedding/unit tests): flat-pool last resort.
            None => with_runtime(|runtime| {
                let removed = runtime.globals.remove(&name_interned).is_some();
                if removed {
                    // J0.3 GlobalIC site: context-less flat-map delete.
                    bump_namespace_version();
                }
                removed
            })
            .unwrap_or(false),
        };
        if removed {
            unsafe { pon_none() }
        } else {
            let name = resolve(name_interned).unwrap_or_else(|| format!("<interned:{name_interned}>"));
            exc::raise_name_error_text(&format!("name '{name}' is not defined"))
        }
    })
}

/// Deletes from the active class namespace, or from globals outside class bodies.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_delete_name(name_interned: u32) -> *mut PyObject {
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        let Some(frame) = with_runtime(|runtime| runtime.class_namespace_stack.last().copied()) else {
            return return_null_with_error("runtime is not initialized");
        };
        match frame {
            Some(frame) if !frame.mapping.is_null() => {
                let key = unsafe { class_mapping_key(name_interned) };
                if key.is_null() {
                    return ptr::null_mut();
                }
                let result = unsafe { crate::abstract_op::subscript_del(frame.mapping, key) };
                if !result.is_null() {
                    return unsafe { pon_none() };
                }
                if !clear_mapping_miss() {
                    return ptr::null_mut();
                }
                let name = resolve(name_interned).unwrap_or_else(|| format!("<interned:{name_interned}>"));
                exc::raise_name_error_text(&format!("name '{name}' is not defined"))
            }
            Some(frame) => {
                if unsafe { (&mut *frame.namespace).del(name_interned) } {
                    unsafe { pon_none() }
                } else {
                    let name = resolve(name_interned).unwrap_or_else(|| format!("<interned:{name_interned}>"));
                    exc::raise_name_error_text(&format!("name '{name}' is not defined"))
                }
            }
            None => unsafe { pon_delete_global(name_interned) },
        }
    })
}

/// Loads a module-global or builtin value by interned name.
///
/// With a non-NULL `feedback` cell this consults a [`GlobalIC`] guarded by
/// the process-wide [`namespace_version`]: a hit returns the cached binding
/// with no mutex, no hash lookup, and no import-state lock.  A miss runs the
/// layered lookup (active module attrs, then flat globals incl. builtins)
/// and publishes a fresh record.  `builtins_version` is recorded `0` because
/// builtins live in the same flat map the counter already guards (J0.3 §5:
/// per-dict versions arrive with N4's dict representation).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_load_global(name_interned: u32, feedback: *mut FeedbackCell) -> *mut PyObject {
    if let Some(cell) = unsafe { feedback.as_ref() } {
        if let Some(ic) = cell.global_hit(namespace_identity(), namespace_version(), 0) {
            // A version+identity match proves no namespace store or module
            // context switch happened since recording: replaying the slow
            // lookup would produce this same binding.  The runtime-init
            // check is subsumed — records only exist post-init.
            return ic.value_ptr as *mut PyObject;
        }
    }
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        // J0.3 capture discipline: version BEFORE the lookup.
        let version = namespace_version();
        // CPython `__globals__` scoping: the executing function's defining
        // module wins over the caller's active module, which wins over the
        // flat pool (builtins + cross-module last-writer fallback).
        let resolved = defining_module_attr(name_interned)
            .or_else(|| crate::import::active_module_attr(name_interned))
            .or_else(|| with_runtime(|runtime| runtime.globals.get(&name_interned).copied()).flatten());
        match resolved {
            Some(value) => {
                if let Some(cell) = unsafe { feedback.as_ref() } {
                    cell.record_global(
                        namespace_identity(),
                        GlobalIC {
                            dict_version: version,
                            builtins_version: 0,
                            value_ptr: value as usize,
                        },
                    );
                }
                value
            }
            None => {
                let name = resolve(name_interned).unwrap_or_else(|| format!("<interned:{name_interned}>"));
                exc::raise_name_error_text(&format!("name '{name}' is not defined"))
            }
        }
    })
}
/// Loads from the active class-body namespace, falling back to globals/builtins.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_load_name(name_interned: u32) -> *mut PyObject {
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        let frame = with_runtime(|runtime| runtime.class_namespace_stack.last().copied()).flatten();
        if let Some(frame) = frame {
            if !frame.mapping.is_null() {
                let key = unsafe { class_mapping_key(name_interned) };
                if key.is_null() {
                    return ptr::null_mut();
                }
                let value = unsafe { crate::abstract_op::subscript_get(frame.mapping, key) };
                if !value.is_null() {
                    return value;
                }
                if !clear_mapping_miss() {
                    return ptr::null_mut();
                }
            } else if let Some(value) = unsafe { (&*frame.namespace).get(name_interned) } {
                return value;
            }
        }
        defining_module_attr(name_interned)
            .or_else(|| crate::import::active_module_attr(name_interned))
            .or_else(|| with_runtime(|runtime| runtime.globals.get(&name_interned).copied()).flatten())
            .unwrap_or_else(|| {
                let name = resolve(name_interned).unwrap_or_else(|| format!("<interned:{name_interned}>"));
                exc::raise_name_error_text(&format!("name '{name}' is not defined"))
            })
    })
}


/// Prints a boxed Phase-A value followed by a newline and returns immortal `None`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_print(value: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(value);
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        let text = match format_object_for_print(value) {
            Ok(text) => text,
            Err(_) if crate::thread_state::pon_err_occurred() => return ptr::null_mut(),
            Err(_) => crate::native::builtins_mod::str_text(value),
        };
        let mut stdout = io::stdout().lock();
        if let Err(error) = writeln!(stdout, "{text}").and_then(|()| stdout.flush()) {
            return return_null_with_error(format!("failed to write stdout: {error}"));
        }
        // SAFETY: `pon_none` returns the initialized immortal singleton.
        unsafe { pon_none() }
    })
}

/// Creates a boxed function object from a compiled entrypoint address.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_make_function(code: *const u8, arity: usize, name_interned: u32) -> *mut PyObject {
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        match with_runtime(|runtime| alloc_function(runtime, code, arity, name_interned)) {
            Some(Ok(object)) => {
                record_new_function_module(object);
                object
            }
            Some(Err(message)) => return_null_with_error(message),
            None => return_null_with_error("runtime is not initialized"),
        }
    })
}

/// Stores a module-global value by interned name.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_store_global(name_interned: u32, value: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(value);
    catch_object_helper(|| {
        if value.is_null() {
            return return_null_with_error("cannot store NULL global value");
        }
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        // CPython `__globals__` scoping, mirrored from `pon_load_global`: a
        // `global` store made by a function body lands in its defining
        // module's namespace, not the caller's active module.  The flat pool
        // is NEVER written here — it is the builtins table, and a module-
        // scope write would leak the binding process-wide (reprlib's
        // module-level `repr = aRepr.repr` must not clobber builtin `repr`).
        // `store_module_attr` bumps the namespace version (J0.3 GlobalIC
        // site), so recorded ICs re-resolve after the visibility change.
        let target_module = current_defining_module().or_else(crate::import::active_module_name_id);
        let stored_in_module = match target_module {
            Some(module) => crate::import::store_module_attr(module, name_interned, value),
            None => false,
        };
        if let Some(module) = target_module {
            crate::dynexec::sync_global_store_for_module(module, name_interned, value);
        }
        if !stored_in_module {
            // No module context (embedding/unit tests) or the defining module
            // was dropped from the cache: the flat pool is the only remaining
            // store that keeps the binding loadable.
            if with_runtime(|runtime| {
                runtime.globals.insert(name_interned, value);
                // J0.3 GlobalIC site: context-less flat-map insert/replace.
                bump_namespace_version();
            })
            .is_none()
            {
                return return_null_with_error("runtime is not initialized");
            }
        }
        value
    })
}

/// Stores into the active class-body namespace, falling back to module globals.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_store_name(name_interned: u32, value: *mut PyObject) -> *mut PyObject {
    crate::untag_prelude!(value);
    catch_object_helper(|| {
        if value.is_null() {
            return return_null_with_error("cannot store NULL namespace value");
        }
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        let Some(frame) = with_runtime(|runtime| runtime.class_namespace_stack.last().copied()) else {
            return return_null_with_error("runtime is not initialized");
        };
        match frame {
            Some(frame) if !frame.mapping.is_null() => {
                let key = unsafe { class_mapping_key(name_interned) };
                if key.is_null() {
                    return ptr::null_mut();
                }
                // MRO-aware `__setitem__` dispatch: user mappings from
                // `__prepare__` observe every class-body store, in order.
                unsafe { map::pon_subscript_set(frame.mapping, key, value) }
            }
            Some(frame) => {
                unsafe {
                    (&mut *frame.namespace).set(name_interned, value);
                }
                value
            }
            None => unsafe { pon_store_global(name_interned, value) },
        }
    })
}

/// Returns the immortal `None` singleton.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_none() -> *mut PyObject {
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        with_runtime(|runtime| as_object_ptr(runtime.none)).unwrap_or_else(|| return_null_with_error("runtime is not initialized"))
    })
}

/// Returns the immortal `NotImplemented` singleton.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_not_implemented() -> *mut PyObject {
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        with_runtime(|runtime| as_object_ptr(runtime.not_implemented))
            .unwrap_or_else(|| return_null_with_error("runtime is not initialized"))
    })
}

/// Idempotently initializes the runtime heap, type table, singletons, and builtins.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_runtime_init() -> i32 {
    match catch_unwind(AssertUnwindSafe(init_runtime)) {
        Ok(Ok(())) => {
            pon_err_clear();
            0
        }
        Ok(Err(message)) => return_minus_one_with_error(message),
        Err(_) => return_minus_one_with_error("runtime initialization panicked"),
    }
}

/// Converts a boxed value to the exact text used by `pon_print`.
#[must_use]
pub fn format_object_for_print(value: *mut PyObject) -> Result<String, String> {
    if value.is_null() {
        return Err("cannot print NULL object".to_owned());
    }

    let fast = with_runtime(|runtime| {
        // SAFETY: The type checks ensure exact concrete casts.
        unsafe {
            if let Some(value) = bool_::to_bool(value) {
                return Some(Ok(if value { "True".to_owned() } else { "False".to_owned() }));
            }
            if is_exact_type(value, runtime.long_type) {
                return Some(Ok((*value.cast::<PyLong>()).value.to_string()));
            }
            if is_exact_type(value, runtime.unicode_type) {
                let unicode = &*value.cast::<PyUnicode>();
                return Some(
                    unicode
                        .as_str()
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| "unicode object contains invalid UTF-8".to_owned()),
                );
            }
            if crate::types::float::is_exact_float(value) {
                let float = &*value.cast::<crate::types::float::PyFloat>();
                return Some(Ok(crate::types::float::repr_f64(float.value)));
            }
            if is_exact_type(value, runtime.none_type) {
                return Some(Ok("None".to_owned()));
            }
            if is_exact_type(value, runtime.not_implemented_type) {
                return Some(Ok("NotImplemented".to_owned()));
            }
            if is_exact_type(value, runtime.ellipsis_type) {
                return Some(Ok("Ellipsis".to_owned()));
            }
            None
        }
    });
    match fast {
        None => Err("runtime is not initialized".to_owned()),
        Some(Some(result)) => result,
        // `__str__` dispatch runs OUTSIDE the `with_runtime` closure: the
        // hook is arbitrary Python and must not re-enter a held runtime.
        Some(None) => crate::native::builtins_mod::try_str_text(value).map_err(|()| "str() raised".to_owned()),
    }
}

struct LocalRoots {
    roots: Vec<*mut u8>,
}

impl RootSource for LocalRoots {
    fn for_each_root(&mut self, visitor: &mut dyn FnMut(*mut u8)) {
        for root in self.roots.iter().copied() {
            visitor(root);
        }
    }
}

unsafe extern "C" fn push_tierup_root(root: *mut u8, ctx: *mut c_void) {
    if root.is_null() || ctx.is_null() {
        return;
    }
    // SAFETY: `collect` passes a live `Vec<*mut u8>` as the callback context for
    // the duration of the root-hook call.
    unsafe { (&mut *ctx.cast::<Vec<*mut u8>>()).push(root) };
}

fn extend_tierup_roots(roots: &mut Vec<*mut u8>) {
    let hook = TIERUP_ROOT_HOOK.load(Ordering::Acquire);
    if hook.is_null() {
        return;
    }
    // SAFETY: The JIT installs only `TierUpRootHook` function pointers through
    // `pon_tierup_set_root_hook`; the callback context is the live roots vector.
    let hook: TierUpRootHook = unsafe { mem::transmute(hook) };
    unsafe { hook(push_tierup_root, (roots as *mut Vec<*mut u8>).cast::<c_void>()) };
}

/// Pushes one class-namespace value onto the root list, piercing the known
/// malloc'd descriptor carriers (classmethod / staticmethod / property /
/// bound method) whose wrapped GC callables marking cannot otherwise reach.
///
/// Tagged immediates and NULL are skipped up front; other non-GC pointers
/// pushed here are filtered by the collector's pointer classification.
fn push_namespace_value_roots(value: *mut PyObject, roots: &mut Vec<*mut u8>) {
    let mut worklist = vec![value];
    // Carrier fields are program-controlled; a pathological carrier cycle
    // must not wedge the collector.
    let mut budget = 64usize;
    while let Some(object) = worklist.pop() {
        if object.is_null() || !crate::tag::is_heap(object) {
            continue;
        }
        roots.push(object.cast::<u8>());
        budget = match budget.checked_sub(1) {
            Some(rest) => rest,
            None => break,
        };
        let ty = unsafe { (*object).ob_type.cast_mut() };
        if ty.is_null() {
            continue;
        }
        if ty == classmethod_builtin_type() {
            worklist.push(unsafe { (*object.cast::<classmethod::PyClassMethod>()).callable });
        } else if ty == staticmethod_builtin_type() {
            worklist.push(unsafe { (*object.cast::<classmethod::PyStaticMethod>()).callable });
        } else if ty == crate::native::builtins_mod::property_type() {
            let property = unsafe { &*object.cast::<crate::types::property::PyProperty>() };
            worklist.push(property.fget);
            worklist.push(property.fset);
            worklist.push(property.fdel);
            worklist.push(property.doc);
        } else if let Some((function, receiver)) = crate::types::method::bound_method_parts(object) {
            worklist.push(function);
            worklist.push(receiver);
        }
    }
}

/// Overwrites the dead stack region the collection call chain is about to
/// occupy with zeros.
///
/// The conservative stack scan reads every word between the collecting
/// thread's stack pointer and the entry boundary.  Frames pushed by the
/// collection itself are allocated but only partially written, so their
/// padding and dead slots would otherwise show *ghosts*: leftover pointer
/// words from earlier, deeper call chains (e.g. the allocation path that
/// constructed an object since deleted).  Ghosts turn `del x; gc.collect()`
/// nondeterministic — the swept object stays reachable through garbage.
/// Zeroing the region below this frame before any collection frame is pushed
/// makes the scan see only live frame contents.  A chain deeper than the
/// scrub degrades to conservative retention, never to unsoundness.
#[inline(never)]
pub(crate) fn scrub_dead_stack_below() {
    const DEAD_STACK_SCRUB_BYTES: usize = 64 * 1024;
    let mut scrub = [0u8; DEAD_STACK_SCRUB_BYTES];
    // Keep the zero-fill: without an observable use the compiler elides the
    // whole frame.
    std::hint::black_box(scrub.as_mut_ptr());
}

/// Runs a stop-the-world collection using the runtime's current root set.
pub fn collect() -> Result<(), String> {
    // Scrub first, then run the whole collection in a fresh callee frame so
    // every collection-path frame — including `collect_impl`'s own — is
    // allocated inside the zeroed region.  Root gathering and marking frames
    // are fat in debug builds; ghosts in their dead zones must not become
    // conservative stack roots.
    scrub_dead_stack_below();
    collect_impl()
}

#[inline(never)]
fn collect_impl() -> Result<(), String> {
    let mut slot = runtime_lock();
    let Some(runtime) = slot.as_mut() else {
        return Err("runtime is not initialized".to_owned());
    };
    let heap = (&runtime.heap) as *const Heap;

    let mut roots = Vec::with_capacity(runtime.globals.len() + 4);
    roots.push(runtime.none.cast::<u8>());
    roots.push(runtime.not_implemented.cast::<u8>());
    roots.push(runtime.ellipsis.cast::<u8>());
    for value in runtime.globals.values().copied() {
        roots.push(value.cast::<u8>());
    }
    // Both class stacks root their frames: `class_namespace_stack` covers
    // bodies still executing, `class_construction_stack` covers popped bodies
    // whose class is being constructed (metaclass hooks may collect).
    for frame in runtime.class_namespace_stack.iter().chain(runtime.class_construction_stack.iter()) {
        // The pre-resolution `__orig_bases__` tuple is reachable only through
        // this frame until the body's publication step lands it in the
        // namespace; root it for both windows.
        if !frame.orig_bases.is_null() {
            roots.push(frame.orig_bases.cast::<u8>());
        }
        // A `__prepare__` mapping is held only by this frame during body
        // execution; root it so its storage (and transitively the body's
        // stores) survives a mid-body collection.
        if !frame.mapping.is_null() {
            roots.push(frame.mapping.cast::<u8>());
        }
        if frame.namespace.is_null() {
            continue;
        }
        for (_, value) in unsafe { (&*frame.namespace).iter() } {
            if !value.is_null() {
                roots.push(value.cast::<u8>());
            }
        }
    }
    // Namespaced-type dict values: type objects are malloc'd boxes, so marking
    // cannot reach the GC-managed values stored in their `PyClassDict`
    // namespaces.  Types are immortal (leaked boxes), so rooting every
    // registered type's dict values is exact, not conservative.
    for ty in crate::sync::namespaced_types() {
        let dict = unsafe { (*ty).tp_dict.cast::<type_::PyClassDict>() };
        if dict.is_null() {
            continue;
        }
        for (_, value) in unsafe { (&*dict).iter() } {
            push_namespace_value_roots(value, &mut roots);
        }
    }

    {
        let state = thread_state_lock();
        if !state.current_exc.is_null() {
            roots.push(state.current_exc.cast::<u8>());
        }
        for value in state.frame_stack.iter().copied() {
            roots.push(value.cast::<u8>());
        }
        for value in state.exception_state_stack.iter().copied() {
            if !value.is_null() {
                roots.push(value.cast::<u8>());
            }
        }
        for frame in &state.exc_star_stack {
            if !frame.original.is_null() {
                roots.push(frame.original.cast::<u8>());
            }
            if !frame.rest.is_null() {
                roots.push(frame.rest.cast::<u8>());
            }
            for raised in frame.raised.iter().copied() {
                if !raised.is_null() {
                    roots.push(raised.cast::<u8>());
                }
            }
        }
        if !state.handled_exc.is_null() {
            roots.push(state.handled_exc.cast::<u8>());
        }
        for value in state.handled_exc_saves.iter().copied() {
            if !value.is_null() {
                roots.push(value.cast::<u8>());
            }
        }
    }

    {
        // Pin J0.1 §6: the thread's stashed delegation finish value must stay
        // alive between pon_gen_stop_value and pon_gen_last_stop_value.
        let stash = r#gen::last_stop_value_root();
        if !stash.is_null() {
            roots.push(stash.cast::<u8>());
        }
    }
    for value in crate::dynexec::rooted_globals_dicts() {
        if !value.is_null() {
            roots.push(value.cast::<u8>());
        }
    }
    let current_function = current_function_object();
    if !current_function.is_null() {
        roots.push(current_function.cast::<u8>());
    }
    for (function, self_arg) in current_call_snapshots() {
        if !function.is_null() {
            roots.push(function.cast::<u8>());
        }
        if !self_arg.is_null() {
            roots.push(self_arg.cast::<u8>());
        }
    }
    // Values held by native `_contextvars` state (context entries, token
    // snapshots, constructor defaults): the holder objects are immortal
    // leaked boxes marking cannot reach, mirroring `rooted_globals_dicts`.
    // Every family below pushes through `push_namespace_value_roots` so held
    // values that are themselves malloc'd carriers (bound methods,
    // classmethod/staticmethod/property wrappers) have their wrapped GC
    // callables and receivers rooted too.
    for value in crate::native::contextvars::gc_held_roots() {
        push_namespace_value_roots(value, &mut roots);
    }
    // Values held by native `_thread._local` per-thread namespaces,
    // mirroring `_contextvars`.
    for value in crate::native::thread::gc_held_roots() {
        push_namespace_value_roots(value, &mut roots);
    }
    // Values held by native `_codecs` registry state (search functions,
    // error handlers, cached CodecInfo objects), mirroring `_contextvars`.
    for value in crate::native::codecs::gc_held_roots() {
        push_namespace_value_roots(value, &mut roots);
    }
    // Entries held by native `_collections` deques, mirroring `_contextvars`.
    for value in crate::native::collections::gc_held_roots() {
        push_namespace_value_roots(value, &mut roots);
    }
    // Python handler objects held by the native `_signal` handler table,
    // mirroring `_contextvars`.
    for value in crate::native::signal::gc_held_roots() {
        push_namespace_value_roots(value, &mut roots);
    }
    // Registered exit callbacks held by native `atexit`, mirroring
    // `_contextvars`.
    for value in crate::native::atexit::gc_held_roots() {
        push_namespace_value_roots(value, &mut roots);
    }
    // Buffer exporters held by live native `PickleBuffer` instances,
    // mirroring `_contextvars`.
    for value in crate::native::pickle::gc_held_roots() {
        push_namespace_value_roots(value, &mut roots);
    }
    // Module attribute values held by the import registry (module objects are
    // immortal leaked boxes marking cannot traverse) plus the `sys.modules`
    // dict: every module-scope binding in every module.
    for value in crate::import::gc_held_roots() {
        push_namespace_value_roots(value, &mut roots);
    }
    // Source iterators, callables, and saved values held by leaked-box
    // itertools iterators, mirroring `_contextvars`.
    for value in crate::native::itertools::gc_held_roots() {
        push_namespace_value_roots(value, &mut roots);
    }
    // Sources and options held by the lazy builtins (`map`/`filter`/`zip`/
    // `reversed`, legacy seq-iter, binder option carriers), mirroring
    // `_contextvars`.
    for value in crate::types::lazy_iter::gc_held_roots() {
        push_namespace_value_roots(value, &mut roots);
    }
    // Unicode receivers borrowed by live `str` iterators, mirroring
    // `_contextvars`.
    for value in str_::gc_held_roots() {
        push_namespace_value_roots(value, &mut roots);
    }
    // Source iterators and callables held by ref-holding native payloads
    // (`enumerate`, JIT-surface `zip`/`map`/`filter`, sentinel iterators),
    // mirroring `_contextvars`.
    for value in crate::native::builtins_mod::gc_held_roots() {
        push_namespace_value_roots(value, &mut roots);
    }
    // Strings/values/interpolations tuples held by leaked-box t-string
    // templates and interpolation records, mirroring `_contextvars`.
    for value in format::gc_held_roots() {
        push_namespace_value_roots(value, &mut roots);
    }
    // Audit hooks registered through `sys.addaudithook`, mirroring
    // `_contextvars`.
    for value in crate::native::sys::gc_held_roots() {
        push_namespace_value_roots(value, &mut roots);
    }
    extend_tierup_roots(&mut roots);

    let mut roots = LocalRoots { roots };
    drop(slot);
    // SAFETY: `heap` points into the process-lifetime runtime slot.  The slot
    // is not cleared after initialization; dropping the runtime mutex here lets
    // object finalizers call back into ABI helpers without deadlocking.
    unsafe { (&*heap).collect(&mut roots) };
    Ok(())
}

/// Allocates a zeroed GC block for a crate-internal boxed-object family whose
/// allocation site lives outside `abi` (frames and frame-locals proxies
/// synthesized by `sys._getframe`).
///
/// Registers `info` for `type_id` first; `Heap::register_type` replaces
/// idempotently, mirroring the raise-path registration in `abi::exc`. Callers
/// `ptr::write` the payload into the returned block. Not an ABI helper: no
/// `HELPERS` row, never visible to compiled code.
pub(crate) fn alloc_gc_object(type_id: TypeId, info: GcTypeInfo) -> Result<*mut u8, String> {
    let size = info.size;
    with_runtime(|runtime| {
        runtime.heap.register_type(type_id, info);
        runtime.heap.alloc(size, type_id)
    })
    .ok_or_else(|| "runtime is not initialized".to_owned())
}
// ─── Defining-module scoping for function-body global loads/stores ──────────
//
// CPython gives every function a `__globals__` namespace: the dict of the
// module that DEFINED it, not the module that happens to be executing when
// it is called.  The runtime's layered global stores (active-module attrs,
// then the flat name-keyed pool) lose that scoping, so two modules defining
// the same top-level name (`re/__init__._compile` vs `re._compiler._compile`)
// collide: last writer wins in the flat pool and cross-module calls bind the
// wrong object.  These helpers recover the CPython rule: functions record
// their defining module at creation (`types::function::FUNCTION_MODULES`),
// and loads/stores made while a compiled function body executes scope to
// that module first.  Every layer below stays as a fallback, so contexts
// without a record (native wrappers, pre-init builtins, bare tests) keep the
// pre-existing active→flat behavior.

/// Depth of this thread's compiled-call stack (`CurrentFunctionGuard` pushes).
#[must_use]
pub(crate) fn current_function_stack_depth() -> usize {
    CURRENT_FUNCTION_STACK.with(|stack| stack.borrow().len())
}

/// Defining module of the innermost compiled function that (a) was pushed
/// while the innermost active module body ran — entries below that floor
/// belong to frames suspended behind the module import and must not leak
/// their namespace into it — and (b) carries a defining-module record.
fn current_defining_module() -> Option<u32> {
    let floor = crate::import::active_module_call_floor();
    CURRENT_FUNCTION_STACK.with(|stack| {
        let stack = stack.borrow();
        let floor = floor.min(stack.len());
        stack[floor..]
            .iter()
            .rev()
            .find_map(|call| crate::types::function::function_module(call.function.cast::<PyObject>()))
    })
}

/// Resolve `name` in the executing function's defining-module namespace.
/// `None` when no scoped function is executing, when the module is gone, or
/// when the module does not bind the name — callers then fall back to the
/// active-module / flat-pool layers.
fn defining_module_attr(name: u32) -> Option<*mut PyObject> {
    let module = current_defining_module()?;
    crate::import::module_attr(module, name)
}

/// Call chain captured for `sys._getframe(depth)`: `chain[0]` describes the
/// compiled call-stack frame `depth` levels above the caller of a native
/// builtin, later links its callers outward, and the final link is always
/// the active module's toplevel frame — the `f_back` chain terminator.
///
/// `native_entry` is the builtin's own code pointer (`sys._getframe`): when
/// the innermost stack entry is that very call it is skipped, so `depth`
/// counts from the builtin's caller — CPython counts Python frames only, a
/// C call pushes none.  Entries below the active module's call floor are
/// suspended behind the import and stay invisible, mirroring
/// `current_defining_module`.  A `depth` past the tracked compiled stack
/// clamps to the bare toplevel link (CPython raises ValueError; loosened
/// here, preserving `sys_getframe`'s active-module fallback).
///
/// Per-link lines follow the traceback rule (`abi::exc`): a frame's current
/// line is the line recorded when its innermost callee was pushed — the
/// skipped native entry's push line (else the live line cell) for the
/// innermost frame, and each caller's line is the push line of the call it
/// is executing.
pub(crate) fn frame_chain_for_depth(depth: usize, native_entry: *const u8) -> Box<[crate::types::frame::FrameLink]> {
    use crate::types::frame::FrameLink;
    let floor = crate::import::active_module_call_floor();
    let toplevel_module = crate::import::active_module_name_id();
    CURRENT_FUNCTION_STACK.with(|stack| {
        let stack = stack.borrow();
        let floor = floor.min(stack.len());
        let visible = &stack[floor..];
        let skip = usize::from(visible.last().is_some_and(|call| {
            // SAFETY: Stack entries hold live function objects for the call's duration.
            let entry = unsafe { (*call.function).entry.load(Ordering::Acquire) };
            ptr::eq(entry.cast_const(), native_entry)
        }));
        let funcs = &visible[..visible.len() - skip];
        let innermost_line = if skip == 1 { visible[funcs.len()].caller_line } else { current_line() };
        // Conceptual frame list, innermost-first: tracked function frames,
        // then the module-toplevel frame.
        let start = depth.min(funcs.len());
        let mut chain = Vec::with_capacity(funcs.len() - start + 1);
        for position in start..funcs.len() {
            let index = funcs.len() - 1 - position;
            let call = &funcs[index];
            let line = if position == 0 { innermost_line } else { funcs[index + 1].caller_line };
            // SAFETY: Stack entries hold live function objects for the call's duration.
            let name = unsafe { (*call.function).name_interned };
            chain.push(FrameLink {
                module: crate::types::function::function_module(call.function.cast::<PyObject>()),
                name: Some(name),
                line,
            });
        }
        chain.push(FrameLink {
            module: toplevel_module,
            name: None,
            line: if funcs.is_empty() { innermost_line } else { funcs[0].caller_line },
        });
        chain.into_boxed_slice()
    })
}

/// Record the defining module for a freshly created function object: the
/// enclosing function's module when compiled code is executing (nested defs,
/// decorator-built wrappers), else the actively executing module body.
pub(super) fn record_new_function_module(function: *mut PyObject) {
    if function.is_null() {
        return;
    }
    if let Some(module) = current_defining_module().or_else(crate::import::active_module_name_id) {
        crate::types::function::set_function_module(function, module);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::intern;
    use crate::thread_state::test_state_lock;

    unsafe extern "C" fn return_none(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
        unsafe { pon_none() }
    }

    unsafe extern "C" fn getframe_probe(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
        unsafe { pon_none() }
    }

    #[test]
    fn frame_chain_for_depth_walks_caller_stack() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            crate::import::install_module("fg_mod_a", []).unwrap();
            crate::import::install_module("fg_mod_b", []).unwrap();

            crate::import::begin_module_execution("fg_mod_a").unwrap();
            let fa = pon_make_function(return_none as *const u8, 0, intern("fg_fn_a"));
            crate::import::end_module_execution("fg_mod_a");
            crate::import::begin_module_execution("fg_mod_b").unwrap();
            let fb = pon_make_function(return_none as *const u8, 0, intern("fg_fn_b"));
            crate::import::end_module_execution("fg_mod_b");
            // Created outside any module body: carries no defining-module record,
            // standing in for `sys._getframe`'s own native call entry.
            let native = pon_make_function(getframe_probe as *const u8, 0, intern("fg_getframe"));
            assert!(!fa.is_null() && !fb.is_null() && !native.is_null());

            // Deterministic per-frame lines: toplevel calls fa at 10, fa
            // calls fb at 20, fb calls the probing builtin at 30.
            set_current_line(10);
            let _call_a = push_current_call(fa.cast::<PyFunction>(), ptr::null_mut(), 0);
            set_current_line(20);
            let _call_b = push_current_call(fb.cast::<PyFunction>(), ptr::null_mut(), 0);
            set_current_line(30);
            let _call_n = push_current_call(native.cast::<PyFunction>(), ptr::null_mut(), 0);

            // Innermost entry IS the probing builtin: skipped, so depth 0 is
            // its caller and each further depth walks one caller outward,
            // ending at the module-toplevel link (no active module here).
            let entry = getframe_probe as *const u8;
            let chain = frame_chain_for_depth(0, entry);
            assert_eq!(chain.len(), 3);
            assert_eq!(chain[0].module, Some(intern("fg_mod_b")));
            assert_eq!(chain[0].name, Some(intern("fg_fn_b")));
            assert_eq!(chain[0].line, 30);
            assert_eq!(chain[1].module, Some(intern("fg_mod_a")));
            assert_eq!(chain[1].name, Some(intern("fg_fn_a")));
            assert_eq!(chain[1].line, 20);
            assert_eq!((chain[2].module, chain[2].name, chain[2].line), (None, None, 10));

            // Deeper starts drop inner links; past the tracked stack only the
            // toplevel link remains (CPython's ValueError case, loosened).
            let chain = frame_chain_for_depth(1, entry);
            assert_eq!(chain.len(), 2);
            assert_eq!(chain[0].module, Some(intern("fg_mod_a")));
            let chain = frame_chain_for_depth(2, entry);
            assert_eq!(chain.len(), 1);
            assert_eq!((chain[0].module, chain[0].name), (None, None));
            assert_eq!(frame_chain_for_depth(9, entry).len(), 1);

            // No skip when the innermost entry is not the probing builtin —
            // an entry without a defining-module record links module None
            // but still carries its function name.
            let chain = frame_chain_for_depth(0, return_none as *const u8);
            assert_eq!(chain.len(), 4);
            assert_eq!(chain[0].module, None);
            assert_eq!(chain[0].name, Some(intern("fg_getframe")));
            assert_eq!(chain[1].module, Some(intern("fg_mod_b")));
        }
    }

    #[test]
    fn helper_table_contains_phase_a_seed_symbols() {
        let seed_symbols = [
            "pon_const_int",
            "pon_const_str",
            "pon_binary_add",
            "pon_call",
            "pon_load_global",
            "pon_print",
            "pon_make_function",
            "pon_store_global",
            "pon_none",
            "pon_runtime_init",
        ];

        for symbol in seed_symbols {
            let helper = HELPERS
                .iter()
                .find(|helper| helper.symbol == symbol)
                .unwrap_or_else(|| panic!("missing Phase-A helper symbol {symbol}"));
            assert!(!helper.address.is_null());
        }
    }

    #[test]
    fn helper_table_contains_thread_foundation_symbols() {
        let thread_symbols = [
            "pon_thread_attach",
            "pon_thread_detach",
            "pon_thread_start_new",
            "pon_gc_safe_region_enter",
            "pon_gc_safe_region_leave",
        ];

        for symbol in thread_symbols {
            let helper = HELPERS
                .iter()
                .find(|helper| helper.symbol == symbol)
                .unwrap_or_else(|| panic!("missing thread foundation helper symbol {symbol}"));
            assert!(!helper.address.is_null());
        }
    }

    #[test]
    fn runtime_init_is_idempotent() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            assert_eq!(pon_runtime_init(), 0);
            assert!(!pon_none().is_null());
        }
    }

    #[test]
    fn int_addition_returns_boxed_long() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            let a = pon_const_int(2);
            let b = pon_const_int(40);
            let sum = pon_binary_add(a, b);
            assert!(!sum.is_null());
            assert_eq!(format_object_for_print(sum).as_deref(), Ok("42"));
        }
    }

    #[test]
    fn global_store_and_load_round_trip() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            let name = intern("answer");
            let value = pon_const_int(42);
            assert_eq!(pon_store_global(name, value), value);
            assert_eq!(pon_load_global(name, ptr::null_mut()), value);
        }
    }

    #[test]
    fn store_global_invalidates_recorded_global_ic() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            let name = intern("ic_guarded");
            let old_value = pon_const_int(1);
            let new_value = pon_const_int(2);
            assert_eq!(pon_store_global(name, old_value), old_value);

            // Miss + record through the real helper path.
            let cell = FeedbackCell::EMPTY;
            assert_eq!(pon_load_global(name, (&raw const cell).cast_mut()), old_value);
            let (identity, ic) = cell.global_snapshot().expect("load published a GlobalIC record");
            assert_eq!(ic.value_ptr, old_value as usize);

            // Hit replays the cached binding.
            assert_eq!(pon_load_global(name, (&raw const cell).cast_mut()), old_value);

            // J0.3 §6: the flat-map store bumps the namespace version, so the
            // recorded version can never match again (bumps are monotonic).
            assert_eq!(pon_store_global(name, new_value), new_value);
            assert!(
                cell.global_hit(identity, namespace_version(), 0).is_none(),
                "store must invalidate the recorded GlobalIC"
            );

            // Slow path re-resolves the NEW binding and re-records it.
            assert_eq!(pon_load_global(name, (&raw const cell).cast_mut()), new_value);
            let (_, ic) = cell.global_snapshot().expect("reload re-published");
            assert_eq!(ic.value_ptr, new_value as usize);
            assert_eq!(pon_load_global(name, (&raw const cell).cast_mut()), new_value);
        }
    }

    #[test]
    fn make_function_and_call_enforce_arity() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            let function = pon_make_function(return_none as *const u8, 0, intern("return_none"));
            assert!(!function.is_null());
            assert_eq!(pon_call(function, ptr::null_mut(), 0), pon_none());
            assert!(pon_call(function, ptr::null_mut(), 1).is_null());
            assert!(pon_err_occurred());
        }
    }

    #[test]
    fn load_global_scopes_to_defining_module_over_flat_pool() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            let name = intern("collide_probe");
            crate::import::install_module("scope_mod_a", []).unwrap();
            crate::import::install_module("scope_mod_b", []).unwrap();

            // Module a's body defines the probe function AND binds the name.
            crate::import::begin_module_execution("scope_mod_a").unwrap();
            let fa = pon_make_function(return_none as *const u8, 0, intern("collide_probe_fn"));
            assert!(!fa.is_null());
            let value_a = pon_const_int(11);
            assert_eq!(pon_store_global(name, value_a), value_a);
            crate::import::end_module_execution("scope_mod_a");
            assert_eq!(crate::types::function::function_module(fa), Some(intern("scope_mod_a")));

            // Module b's body is the flat pool's last writer for the SAME name.
            crate::import::begin_module_execution("scope_mod_b").unwrap();
            let value_b = pon_const_int(22);
            assert_eq!(pon_store_global(name, value_b), value_b);
            crate::import::end_module_execution("scope_mod_b");

            // While a scope_mod_a function executes (no module body active),
            // the load must resolve through its DEFINING module's namespace,
            // not the flat pool where module b clobbered the binding.
            {
                let _call = push_current_call(fa.cast::<PyFunction>(), ptr::null_mut(), 0);
                assert_eq!(pon_load_global(name, ptr::null_mut()), value_a);
            }

            // Empty call stack and no active module: module-scope stores no
            // longer write the flat pool, so the name resolves to NOTHING —
            // module b's binding stays private to module b instead of
            // clobbering the process-wide namespace.
            assert!(pon_load_global(name, ptr::null_mut()).is_null());
            assert!(pon_err_occurred());
            pon_err_clear();
            // Builtins keep resolving through the flat pool.
            assert!(!pon_load_global(intern("print"), ptr::null_mut()).is_null());
        }
    }

    #[test]
    fn print_conversion_formats_unicode_and_int() {
        let _guard = test_state_lock();
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
            let string = pon_const_str(b"hello".as_ptr(), 5);
            let integer = pon_const_int(-7);
            assert_eq!(format_object_for_print(string).as_deref(), Ok("hello"));
            assert_eq!(format_object_for_print(integer).as_deref(), Ok("-7"));
        }
    }
}

/// Allocates a payload-subclass heap instance (`str`/`int`-derived class)
/// with the extended layout: heap-instance prefix plus the canonical
/// builtin payload slot.
pub(crate) unsafe fn alloc_payload_subclass_instance(
    cls: *mut PyType,
    dict: *mut type_::PyClassDict,
    slots: Vec<type_::PySlotValue>,
    value: *mut PyObject,
) -> Result<*mut PyObject, String> {
    with_runtime(|runtime| {
        let object = runtime
            .heap
            .alloc(
                mem::size_of::<type_::PyPayloadSubclassInstance>(),
                type_::TYPE_ID_PAYLOAD_SUBCLASS_INSTANCE,
            )
            .cast::<type_::PyPayloadSubclassInstance>();
        unsafe {
            ptr::write(
                object,
                type_::PyPayloadSubclassInstance {
                    base: type_::PyHeapInstance {
                        ob_base: PyObjectHeader::new(cls),
                        dict,
                        slots,
                        weakrefs: ptr::null_mut(),
                        finalized: false,
                    },
                    value,
                },
            );
        }
        Ok(as_object_ptr(object))
    })
    .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
}

/// Core of the Python-visible `int.__new__`/`str.__new__` staticmethod
/// carriers: validate the class argument, build the canonical value with the
/// builtin constructor, and wrap it in the payload-subclass layout when the
/// class is a Python subclass of the owner.
unsafe fn data_type_dunder_new_common(
    owner: &'static str,
    constructor: BuiltinConstructor,
    argv: *mut *mut PyObject,
    argc: usize,
) -> *mut PyObject {
    if argv.is_null() || argc == 0 {
        let message = format!("{owner}.__new__(): not enough arguments");
        return unsafe { exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    }
    let cls = unsafe { *argv };
    if unsafe { !type_::is_type_object(cls) } {
        let message = format!("{owner}.__new__(X): X is not a type object");
        return unsafe { exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    }
    let cls_ty = cls.cast::<PyType>();
    let is_owner_subtype = unsafe { crate::mro::mro_entries(cls_ty) }.iter().any(|entry| {
        !entry.is_null()
            && unsafe {
                (**entry).gc_type_id != type_::TYPE_ID_HEAP_INSTANCE.0 as usize && (**entry).name() == owner
            }
    });
    if !is_owner_subtype {
        let cls_name = unsafe { (*cls_ty).name() };
        let message = format!("{owner}.__new__({cls_name}): {cls_name} is not a subtype of {owner}");
        return unsafe { exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    }
    let value = unsafe { constructor(if argc > 1 { argv.add(1) } else { ptr::null_mut() }, argc - 1) };
    if value.is_null() {
        return ptr::null_mut();
    }
    if unsafe { (*cls_ty).gc_type_id != type_::TYPE_ID_HEAP_INSTANCE.0 as usize } {
        // `cls` is the builtin itself: the canonical value IS the instance.
        return value;
    }
    if unsafe { !type_::type_is_payload_subclass(cls_ty) } {
        let cls_name = unsafe { (*cls_ty).name() };
        let message = format!("{owner}.__new__({cls_name}): {cls_name} does not embed a {owner} payload");
        return unsafe { exc::pon_raise_type_error(message.as_ptr(), message.len()) };
    }
    match unsafe { type_::alloc_payload_instance_for_class(cls_ty, value) } {
        Ok(object) => object,
        Err(message) => return_null_with_error(message),
    }
}

unsafe extern "C" fn int_dunder_new_native(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { data_type_dunder_new_common("int", crate::native::builtins_mod::builtin_int, argv, argc) }
}

unsafe extern "C" fn str_dunder_new_native(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { data_type_dunder_new_common("str", crate::native::builtins_mod::builtin_str, argv, argc) }
}

/// `<data type>.__repr__(self)` — repr of the receiver's canonical value
/// (payload-subclass receivers read through their payload).
unsafe extern "C" fn data_type_dunder_repr_native(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let receiver = match unsafe { native_receiver_arg(argv, argc, "__repr__") } {
        Ok(receiver) => receiver,
        Err(error) => return error,
    };
    let receiver = unsafe { type_::payload_subclass_value(receiver) }.unwrap_or(receiver);
    let text = crate::native::builtins_mod::repr_text(receiver);
    unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

/// `str.__str__(self)` — text of the receiver's canonical value.
unsafe extern "C" fn data_type_dunder_str_native(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let receiver = match unsafe { native_receiver_arg(argv, argc, "__str__") } {
        Ok(receiver) => receiver,
        Err(error) => return error,
    };
    let receiver = unsafe { type_::payload_subclass_value(receiver) }.unwrap_or(receiver);
    let text = crate::native::builtins_mod::str_text(receiver);
    unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

/// Data-type dunder surface for `int` and `str`: a real `__new__`
/// (staticmethod carrier, payload-subclass aware) plus `__repr__` (and
/// `__str__` on `str` only — CPython dict containment), so enum's
/// `_find_data_type_`/`_find_data_repr_`/`_find_new_` probes hold.
fn install_data_type_dunders(runtime: &mut Runtime) {
    let long_type = runtime.long_type;
    let unicode_type = runtime.unicode_type;
    let entries: [(*mut PyType, *const u8, bool); 2] = [
        (long_type, int_dunder_new_native as *const u8, false),
        (unicode_type, str_dunder_new_native as *const u8, true),
    ];
    for (ty, new_entry, with_str) in entries {
        if ty.is_null() {
            continue;
        }
        let new_name = crate::intern::intern("__new__");
        let Ok(function) = alloc_function(runtime, new_entry, crate::builtins::variadic_arity(), new_name) else {
            continue;
        };
        let descriptor = unsafe { classmethod::new_staticmethod(staticmethod_builtin_type(), function) };
        if descriptor.is_null() {
            continue;
        }
        unsafe {
            let mut dict = (*ty).tp_dict.cast::<type_::PyClassDict>();
            if dict.is_null() {
                dict = type_::new_namespace();
                (*ty).tp_dict = dict.cast::<PyObject>();
            }
            (&mut *dict).set(new_name, descriptor.cast::<PyObject>());
            let repr_name = crate::intern::intern("__repr__");
            if let Ok(function) =
                alloc_function(runtime, data_type_dunder_repr_native as *const u8, crate::builtins::variadic_arity(), repr_name)
            {
                (&mut *dict).set(repr_name, function.cast::<PyObject>());
            }
            if with_str {
                let str_name = crate::intern::intern("__str__");
                if let Ok(function) =
                    alloc_function(runtime, data_type_dunder_str_native as *const u8, crate::builtins::variadic_arity(), str_name)
                {
                    (&mut *dict).set(str_name, function.cast::<PyObject>());
                }
            }
            crate::sync::register_namespaced_type(ty);
        }
    }
}
