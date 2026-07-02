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
use std::collections::HashMap;
use std::io::{self, Write};
use std::mem;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::sync::atomic::{AtomicPtr, Ordering};

use pon_gc::{GcTypeInfo, Heap, RootSource, TypeId};

use crate::builtins as runtime_builtins;
use crate::intern::resolve;
use crate::feedback::{FeedbackCell, FeedbackVec, GlobalIC, TypeTag};
use crate::object::{
    PyCodeFn, PyFunction, PyLong, PyNone, PyObject, PyObjectHeader, PyType, PyUnicode, TIER_STATE_DEFERRED,
    TIER_STATE_QUEUED, TIER_STATE_TIER0, as_object_ptr, is_exact_type,
};
use crate::types::{bool_, float, function, int, type_};
use crate::types::exc::{ExceptionTypeSet, PyBaseException, is_exception_subclass, trace_base_exception};
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
pub type TierUpHook = unsafe extern "C" fn(*mut PyFunction);

static TIERUP_HOOK: AtomicPtr<()> = AtomicPtr::new(ptr::null_mut());

#[derive(Clone, Copy)]
struct CurrentCall {
    function: *mut PyFunction,
    argv: *mut *mut PyObject,
    argc: usize,
}

thread_local! {
    static CURRENT_FUNCTION_STACK: RefCell<Vec<CurrentCall>> = RefCell::new(Vec::new());
}
// ─── J0.3 GlobalIC guard: process-wide namespace version ────────────────────
//
// `pon_load_global` resolves through TWO layered stores (the executing
// module's `PyModuleObject.attrs`, then the flat `Runtime.globals` map that
// also holds builtins), neither of which is a versioned dict object yet.
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
const TYPE_ID_EXCEPTION: TypeId = TypeId(6);

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

/// Raw template-string part consumed by the future string helper family.
///
/// This mirrors [`FStrPartRaw`] while preserving an interned expression name for
/// template interpolation objects.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TStrPartRaw {
    /// Interpolated value, or NULL when this part is a literal.
    pub value: *mut PyObject,
    /// UTF-8 literal bytes for literal parts, or NULL for value parts.
    pub literal: *const u8,
    /// Byte length of `literal`.
    pub literal_len: usize,
    /// Interned interpolation expression/name, or `0` when absent.
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
const PARAMS_FRAME_LOCAL: &[AbiTy] = &[AbiTy::PyFramePtr, AbiTy::U32];
const PARAMS_FRAME_SET_LOCAL: &[AbiTy] = &[AbiTy::PyFramePtr, AbiTy::U32, AbiTy::PyObjectPtr];
const PARAMS_MAKE_GENERATOR: &[AbiTy] = &[AbiTy::GenResumePtr, AbiTy::PyFramePtr, AbiTy::U8];

/// Exported helper table consumed by later codegen/JIT stages.
pub static HELPERS: &[HelperDecl] = &[
    HelperDecl {
        symbol: "pon_const_int",
        address: pon_const_int as *const (),
        params: PARAMS_CONST_INT,
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
        ret: AbiTy::PyObjectPtrPtr,
    },
    HelperDecl {
        symbol: "pon_unpack_ex",
        address: seq::pon_unpack_ex as *const (),
        params: PARAMS_UNPACK_EX,
        ret: AbiTy::PyObjectPtrPtr,
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
        symbol: "pon_make_frame",
        address: r#gen::pon_make_frame as *const (),
        params: PARAMS_GEN_MAKE_FRAME,
        ret: AbiTy::PyFramePtr,
    },
    HelperDecl {
        symbol: "pon_frame_get_local",
        address: r#gen::pon_frame_get_local as *const (),
        params: PARAMS_FRAME_LOCAL,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_frame_set_local",
        address: r#gen::pon_frame_set_local as *const (),
        params: PARAMS_FRAME_SET_LOCAL,
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
        symbol: "pon_yield",
        address: r#gen::pon_yield as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_eager_yield_generator",
        address: r#gen::pon_eager_yield_generator as *const (),
        params: PARAMS_OBJ,
        ret: AbiTy::PyObjectPtr,
    },
    HelperDecl {
        symbol: "pon_yield_from",
        address: r#gen::pon_yield_from as *const (),
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
];

struct Runtime {
    heap: Heap,
    _type_type: *mut PyType,
    long_type: *mut PyType,
    unicode_type: *mut PyType,
    function_type: *mut PyType,
    none_type: *mut PyType,
    exception_types: ExceptionTypeSet,
    none: *mut PyNone,
    globals: HashMap<u32, *mut PyObject>,
    class_namespace_stack: Vec<*mut type_::PyClassDict>,
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

fn init_runtime() -> Result<(), String> {
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
            let function_type = Box::into_raw(function_type);
            let none_type = Box::into_raw(Box::new(PyType::new(type_type, "NoneType", mem::size_of::<PyNone>())));
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

            let mut runtime = Runtime {
                heap,
                _type_type: type_type,
                long_type,
                unicode_type,
                function_type,
                none_type,
                exception_types,
                none,
                globals: HashMap::new(),
                class_namespace_stack: Vec::new(),
            };

            register_builtins(&mut runtime)?;
            *slot = Some(runtime);
            true
        }
    };

    if should_register_native {
        crate::import::register_native_modules()?;
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
            trace: trace_no_refs,
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
        TYPE_ID_EXCEPTION,
        GcTypeInfo {
            size: mem::size_of::<PyBaseException>(),
            trace: trace_base_exception,
            finalize: None,
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

unsafe extern "C" fn finalize_function(object: *mut u8) {
    if object.is_null() {
        return;
    }

    let function = object.cast::<PyFunction>();
    function::unregister_function_record(function.cast::<PyObject>());
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
}

/// Calls a boxed callable, including native builtins, heap types, and bound methods.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_call(callee: *mut PyObject, argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
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
        return return_null_with_error(format!("function expected {arity} arguments, got {argc}"));
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
    pon_err_clear();
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
            CURRENT_FUNCTION_STACK.with(|stack| stack.borrow_mut().push(CurrentCall { function, argv, argc }));
            return Self { pushed: true };
        }
        Self { pushed: false }
    }
}

impl Drop for CurrentFunctionGuard {
    fn drop(&mut self) {
        if self.pushed {
            CURRENT_FUNCTION_STACK.with(|stack| {
                let _ = stack.borrow_mut().pop();
            });
        }
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

fn maybe_queue_tierup(function: *mut PyFunction) {
    if function.is_null() {
        return;
    }
    let hook = TIERUP_HOOK.load(Ordering::Acquire);
    if hook.is_null() {
        return;
    }
    // SAFETY: `function` is a live PyFunction while a runtime call/backedge is
    // executing.  The CAS ensures one transition into the queued state.
    let queued = unsafe {
        let function_ref = &*function;
        match function_ref.tier_state.load(Ordering::Acquire) {
            TIER_STATE_TIER0 => function_ref
                .tier_state
                .compare_exchange(TIER_STATE_TIER0, TIER_STATE_QUEUED, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            TIER_STATE_DEFERRED
                if function_ref.hotness.load(Ordering::Acquire) >= TIER1_DEFERRED_CALL_THRESHOLD
                    || function_ref.loop_hotness.load(Ordering::Acquire) >= TIER1_LOOP_THRESHOLD =>
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
        unsafe { hook(function) };
    }
}

/// Installs or clears the runtime-to-tier-up hook.
///
/// Passing NULL clears the hook.  Non-NULL values must point at a `TierUpHook`
/// function; `pon-runtime` keeps the pointer opaque to avoid depending on JIT.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_tierup_set_hook(hook: *mut ()) {
    TIERUP_HOOK.store(hook, Ordering::Release);
}

/// Function-entry tier-up probe.  Bumps hotness and queues tier-up once hot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_tierup_bump_call(function: *mut PyFunction) {
    if function.is_null() {
        return;
    }
    // SAFETY: Non-NULL callers pass a live PyFunction object.
    let function_ref = unsafe { &*function };
    let hotness = bump_saturating(&function_ref.hotness);
    match function_ref.tier_state.load(Ordering::Acquire) {
        TIER_STATE_TIER0 if hotness >= TIER1_CALL_THRESHOLD => maybe_queue_tierup(function),
        TIER_STATE_DEFERRED if hotness >= TIER1_DEFERRED_CALL_THRESHOLD => maybe_queue_tierup(function),
        _ => {}
    }
}

/// Loop back-edge probe for tier-0 code.  Uses the current runtime call context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_backedge_poll() {
    let function = current_function_for_tierup();
    if function.is_null() {
        return;
    }
    // SAFETY: The current-function stack only contains live frames while their
    // compiled entrypoint is executing.
    let function_ref = unsafe { &*function };
    let hotness = bump_saturating(&function_ref.loop_hotness);
    match function_ref.tier_state.load(Ordering::Acquire) {
        TIER_STATE_TIER0 | TIER_STATE_DEFERRED if hotness >= TIER1_LOOP_THRESHOLD => maybe_queue_tierup(function),
        _ => {}
    }
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
        let message = args.first().copied().unwrap_or(ptr::null_mut());
        return match with_runtime(|runtime| exc::alloc_exception_object(runtime, cls, message, ptr::null_mut())) {
            Some(Ok(exception)) => exception,
            Some(Err(message)) => return_null_with_error(message),
            None => return_null_with_error("runtime is not initialized"),
        };
    }

    let new = unsafe { (*cls).tp_new.unwrap_or(type_::type_new) };
    let instance = unsafe { new(cls, ptr::null_mut(), ptr::null_mut()) };
    if instance.is_null() {
        return ptr::null_mut();
    }

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
    !object.is_null() && unsafe { (*object).ob_type == runtime._type_type.cast_const() }
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
        let namespace = type_::new_namespace();
        if with_runtime(|runtime| runtime.class_namespace_stack.push(namespace)).is_none() {
            return return_null_with_error("runtime is not initialized");
        }
        if !body.is_null() {
            let result = unsafe { pon_call(body, ptr::null_mut(), 0) };
            let popped = with_runtime(|runtime| runtime.class_namespace_stack.pop()).flatten();
            if popped != Some(namespace) {
                return return_null_with_error("class namespace stack is corrupted");
            }
            if result.is_null() {
                return ptr::null_mut();
            }
        } else {
            let popped = with_runtime(|runtime| runtime.class_namespace_stack.pop()).flatten();
            if popped != Some(namespace) {
                return return_null_with_error("class namespace stack is corrupted");
            }
        }
        let base_slice = if base_count == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(bases, base_count) }
        };
        let class = unsafe { type_::build_class_from_namespace(&name, base_slice, namespace, &[]) };
        if class.is_null() {
            return ptr::null_mut();
        }
        let _ = with_runtime(|runtime| unsafe {
            if (*class).ob_type.is_null() {
                (*class).ob_type = runtime._type_type.cast_const();
            }
        });
        class
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_setup_annotations() -> *mut PyObject {
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        let name = crate::intern::intern("__annotations__");
        if let Some(existing) = with_runtime(|runtime| {
            if let Some(namespace) = runtime.class_namespace_stack.last() {
                unsafe { (&**namespace).get(name) }
            } else {
                runtime.globals.get(&name).copied()
            }
        })
        .flatten()
        {
            return existing;
        }
        let annotations = unsafe { map::pon_build_map(ptr::null_mut(), 0) };
        if annotations.is_null() {
            return annotations;
        }
        match with_runtime(|runtime| {
            if let Some(namespace) = runtime.class_namespace_stack.last().copied() {
                unsafe {
                    (&mut *namespace).set(name, annotations);
                }
                Ok(annotations)
            } else {
                runtime.globals.insert(name, annotations);
                crate::import::store_active_module_attr(name, annotations);
                // J0.3 GlobalIC site: module-level __annotations__ insert.
                bump_namespace_version();
                ensure_module_annotate_function(runtime).map(|()| annotations)
            }
        }) {
            Some(Ok(value)) => value,
            Some(Err(message)) => return_null_with_error(message),
            None => return_null_with_error("runtime is not initialized"),
        }
    })
}

fn ensure_module_annotate_function(runtime: &mut Runtime) -> Result<(), String> {
    let name = crate::intern::intern("__annotate__");
    if runtime.globals.contains_key(&name) {
        return Ok(());
    }
    let function = alloc_function(runtime, module_annotations_annotate as *const u8, 1, name)?;
    runtime.globals.insert(name, function);
    crate::import::store_active_module_attr(name, function);
    // J0.3 GlobalIC site: module __annotate__ registration.
    bump_namespace_version();
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
        let resolved = crate::import::active_module_attr(name_interned)
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
                return_null_with_error(format!("name '{name}' is not defined"))
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
        with_runtime(|runtime| {
            runtime
                .class_namespace_stack
                .last()
                .and_then(|namespace| unsafe { (&**namespace).get(name_interned) })
        })
        .flatten()
        .or_else(|| crate::import::active_module_attr(name_interned))
        .or_else(|| with_runtime(|runtime| runtime.globals.get(&name_interned).copied()).flatten())
        .unwrap_or_else(|| {
            let name = resolve(name_interned).unwrap_or_else(|| format!("<interned:{name_interned}>"));
            return_null_with_error(format!("name '{name}' is not defined"))
        })
    })
}


/// Prints a boxed Phase-A value followed by a newline and returns immortal `None`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_print(value: *mut PyObject) -> *mut PyObject {
    catch_object_helper(|| {
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        let text = match format_object_for_print(value) {
            Ok(text) => text,
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
            Some(Ok(object)) => object,
            Some(Err(message)) => return_null_with_error(message),
            None => return_null_with_error("runtime is not initialized"),
        }
    })
}

/// Stores a module-global value by interned name.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_store_global(name_interned: u32, value: *mut PyObject) -> *mut PyObject {
    catch_object_helper(|| {
        if value.is_null() {
            return return_null_with_error("cannot store NULL global value");
        }
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        crate::import::store_active_module_attr(name_interned, value);
        match with_runtime(|runtime| {
            runtime.globals.insert(name_interned, value);
            // J0.3 GlobalIC site: flat-map insert/replace.
            bump_namespace_version();
            value
        }) {
            Some(stored) => stored,
            None => return_null_with_error("runtime is not initialized"),
        }
    })
}

/// Stores into the active class-body namespace, falling back to module globals.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_store_name(name_interned: u32, value: *mut PyObject) -> *mut PyObject {
    catch_object_helper(|| {
        if value.is_null() {
            return return_null_with_error("cannot store NULL namespace value");
        }
        if let Err(message) = ensure_runtime_initialized() {
            return return_null_with_error(message);
        }
        match with_runtime(|runtime| {
            if let Some(namespace) = runtime.class_namespace_stack.last().copied() {
                unsafe {
                    (&mut *namespace).set(name_interned, value);
                }
            } else {
                runtime.globals.insert(name_interned, value);
                // J0.3 GlobalIC site: module-scope StoreName lands in the flat map.
                bump_namespace_version();
            }
            value
        }) {
            Some(stored) => stored,
            None => return_null_with_error("runtime is not initialized"),
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

    with_runtime(|runtime| {
        // SAFETY: The type checks ensure exact concrete casts.
        unsafe {
            if let Some(value) = bool_::to_bool(value) {
                return Ok(if value { "True".to_owned() } else { "False".to_owned() });
            }
            if is_exact_type(value, runtime.long_type) {
                return Ok((*value.cast::<PyLong>()).value.to_string());
            }
            if is_exact_type(value, runtime.unicode_type) {
                let unicode = &*value.cast::<PyUnicode>();
                return unicode
                    .as_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| "unicode object contains invalid UTF-8".to_owned());
            }
            if crate::types::float::is_exact_float(value) {
                let float = &*value.cast::<crate::types::float::PyFloat>();
                return Ok(crate::types::float::repr_f64(float.value));
            }
            if is_exact_type(value, runtime.none_type) {
                return Ok("None".to_owned());
            }
            Ok(crate::native::builtins_mod::str_text(value))
        }
    })
    .unwrap_or_else(|| Err("runtime is not initialized".to_owned()))
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

/// Runs a stop-the-world collection using the runtime's current root set.
pub fn collect() -> Result<(), String> {
    let mut slot = runtime_lock();
    let Some(runtime) = slot.as_mut() else {
        return Err("runtime is not initialized".to_owned());
    };

    let mut roots = Vec::with_capacity(runtime.globals.len() + 2);
    roots.push(runtime.none.cast::<u8>());
    for value in runtime.globals.values().copied() {
        roots.push(value.cast::<u8>());
    }
    for namespace in runtime.class_namespace_stack.iter().copied() {
        if namespace.is_null() {
            continue;
        }
        for (_, value) in unsafe { (&*namespace).iter() } {
            if !value.is_null() {
                roots.push(value.cast::<u8>());
            }
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
    }

    let mut roots = LocalRoots { roots };
    runtime.heap.collect(&mut roots);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::intern;
    use crate::thread_state::test_state_lock;

    unsafe extern "C" fn return_none(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
        unsafe { pon_none() }
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
