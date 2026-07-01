//! Runtime helper import declaration for Cranelift modules.
//!
//! `declare_helpers` and [`HelperRefs`] remain the stable Phase-A/JIT API while
//! growing to include only Phase-B helpers that are present in
//! `pon_runtime::abi::HELPERS`.  [`PHASE_B_HELPERS`] stays as the descriptive
//! frozen signature table; imports are declared from the runtime table so codegen
//! cannot drift from real exported symbols.

use cranelift_codegen::ir::{self, AbiParam};
use cranelift_module::{FuncId, Linkage, Module, ModuleResult};
use pon_runtime::abi::{AbiTy, HELPERS};

/// Cranelift module function ids for every runtime helper import codegen may call.
///
/// The ids are declared directly from [`pon_runtime::abi::HELPERS`], so symbol
/// spelling, parameter order, and return types stay locked to the runtime ABI.
/// Consumers pass these ids to baseline lowering, which then imports per-function
/// [`cranelift_codegen::ir::FuncRef`] handles with `Module::declare_func_in_func`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HelperRefs {
    /// `pon_const_int(i64) -> *mut PyObject`.
    pub const_int: FuncId,
    /// `pon_const_float(f64) -> *mut PyObject`.
    pub const_float: FuncId,
    /// `pon_const_complex(f64, f64) -> *mut PyObject`.
    pub const_complex: FuncId,
    /// `pon_const_bool(i32) -> *mut PyObject`.
    pub const_bool: FuncId,
    /// `pon_const_str(*const u8, usize) -> *mut PyObject`.
    pub const_str: FuncId,
    /// `pon_binary_add(*mut PyObject, *mut PyObject) -> *mut PyObject`.
    pub binary_add: FuncId,
    /// `pon_rich_compare(op, lhs, rhs, feedback) -> *mut PyObject`.
    pub rich_compare: FuncId,
    /// `pon_number_unary(op, value, feedback) -> *mut PyObject`.
    pub number_unary: FuncId,
    /// `pon_number_binary(op, lhs, rhs, feedback) -> *mut PyObject`.
    pub number_binary: FuncId,
    /// `pon_number_inplace(op, lhs, rhs, feedback) -> *mut PyObject`.
    pub number_inplace: FuncId,
    /// `pon_is_true(value) -> i32`.
    pub is_true: FuncId,
    /// `pon_contains(container, item) -> i32`.
    pub contains: FuncId,
    /// `pon_call(callee, argv, argc) -> *mut PyObject`.
    pub call: FuncId,
    /// `pon_call_ex(callee, argv, argc, star, kw_names, kw_values, kw_count, dstar, feedback) -> *mut PyObject`.
    pub call_ex: FuncId,
    /// `pon_call_method(recv_pair, argv, argc, feedback) -> *mut PyObject`.
    pub call_method: FuncId,
    /// `pon_load_global(name_id) -> *mut PyObject`.
    pub load_global: FuncId,
    /// `pon_load_name(name_id) -> *mut PyObject`.
    pub load_name: FuncId,
    /// `pon_load_builtin(name_id) -> *mut PyObject`.
    pub load_builtin: FuncId,
    /// `pon_store_name(name_id, value) -> *mut PyObject`.
    pub store_name: FuncId,
    /// `pon_get_attr(object, name_id, feedback) -> *mut PyObject`.
    pub get_attr: FuncId,
    /// `pon_set_attr(object, name_id, value) -> i32`.
    pub set_attr: FuncId,
    /// `pon_del_attr(object, name_id) -> i32`.
    pub del_attr: FuncId,
    /// `pon_build_tuple(argv, argc) -> *mut PyObject`.
    pub build_tuple: FuncId,
    /// `pon_build_list(argv, argc) -> *mut PyObject`.
    pub build_list: FuncId,
    /// `pon_build_set(argv, argc) -> *mut PyObject`.
    pub build_set: FuncId,
    /// `pon_build_slice(start, stop, step) -> *mut PyObject`.
    pub build_slice: FuncId,
    /// `pon_list_append(list, value) -> *mut PyObject`.
    pub list_append: FuncId,
    /// `pon_set_add(set, value) -> *mut PyObject`.
    pub set_add: FuncId,
    /// `pon_list_extend(list, iterable) -> *mut PyObject`.
    pub list_extend: FuncId,
    /// `pon_unpack_seq(value, n, feedback) -> *mut *mut PyObject`.
    pub unpack_seq: FuncId,
    /// `pon_unpack_ex(value, before, after) -> *mut *mut PyObject`.
    pub unpack_ex: FuncId,
    /// `pon_get_len(value, feedback) -> *mut PyObject`.
    pub get_len: FuncId,
    /// `pon_build_map(flat_pairs, pair_count) -> *mut PyObject`.
    pub build_map: FuncId,
    /// `pon_map_insert(map, key, value) -> *mut PyObject`.
    pub map_insert: FuncId,
    /// `pon_dict_merge(map, other) -> *mut PyObject`.
    pub dict_merge: FuncId,
    /// `pon_dict_merge_unique(map, other) -> *mut PyObject`.
    pub dict_merge_unique: FuncId,
    /// `pon_subscript_get(object, key, feedback) -> *mut PyObject`.
    pub subscript_get: FuncId,
    /// `pon_subscript_set(object, key, value) -> *mut PyObject`.
    pub subscript_set: FuncId,
    /// `pon_subscript_del(object, key) -> *mut PyObject`.
    pub subscript_del: FuncId,
    /// `pon_build_string(parts, len) -> *mut PyObject`.
    pub build_string: FuncId,
    /// `pon_build_template(parts, len) -> *mut PyObject`.
    pub build_template: FuncId,
    /// `pon_import_name(name, fromlist, fromlist_len, level) -> *mut PyObject`.
    pub import_name: FuncId,
    /// `pon_import_from(module, name) -> *mut PyObject`.
    pub import_from: FuncId,
    /// `pon_import_star(module) -> *mut PyObject`.
    pub import_star: FuncId,
    /// `pon_raise(exc, cause) -> *mut PyObject`.
    pub raise: FuncId,
    /// `pon_reraise() -> *mut PyObject`.
    pub reraise: FuncId,
    /// `pon_push_exc_info(target, stack_depth, kind) -> *mut PyObject`.
    pub push_exc_info: FuncId,
    /// `pon_pop_exc_info() -> *mut PyObject`.
    pub pop_exc_info: FuncId,
    /// `pon_match_exc(exc_type) -> *mut PyObject`.
    pub match_exc: FuncId,
    /// `pon_check_exc_star(exc_types) -> *mut PyObject`.
    pub check_exc_star: FuncId,
    /// `pon_get_current_exc() -> *mut PyObject`.
    pub get_current_exc: FuncId,
    /// `pon_build_exc_group(argv, argc) -> *mut PyObject`.
    pub build_exc_group: FuncId,
    /// `pon_get_iter(value, feedback) -> *mut PyObject`.
    pub get_iter: FuncId,
    /// `pon_get_aiter(value, feedback) -> *mut PyObject`.
    pub get_aiter: FuncId,
    /// `pon_for_next(iterator, feedback) -> *mut PyObject`.
    pub for_next: FuncId,
    /// `pon_gen_stop_value() -> *mut PyObject`.
    pub gen_stop_value: FuncId,
    /// `pon_yield(value) -> *mut PyObject`.
    pub yield_value: FuncId,
    /// `pon_yield_from(iterator, feedback) -> *mut PyObject`.
    pub yield_from: FuncId,
    /// `pon_await(awaitable, feedback) -> *mut PyObject`.
    pub await_value: FuncId,
    /// `pon_eager_yield_generator(return_value) -> *mut PyObject`.
    pub eager_yield_generator: FuncId,
    /// `pon_match_sequence(subject, feedback) -> *mut PyObject`.
    pub match_sequence: FuncId,
    /// `pon_match_mapping(subject, feedback) -> *mut PyObject`.
    pub match_mapping: FuncId,
    /// `pon_match_class(subject, cls, nargs, kw, nkw) -> *mut PyObject`.
    pub match_class: FuncId,
    /// `pon_match_keys(subject, keys, nkeys) -> *mut PyObject`.
    pub match_keys: FuncId,
    /// `pon_match_len_ge(subject, n, exact) -> *mut PyObject`.
    pub match_len_ge: FuncId,
    /// `pon_print(value) -> *mut PyObject`.
    pub print: FuncId,
    /// `pon_make_function(code, arity, name_id) -> *mut PyObject`.
    pub make_function: FuncId,
    /// `pon_make_function_full(code_info, defaults, default_count, kwdefault_names, kwdefaults, kwdefault_count, annotation_names, annotations, annotation_count) -> *mut PyObject`.
    pub make_function_full: FuncId,
    /// `pon_function_set_closure(function, cells, count) -> *mut PyObject`.
    pub function_set_closure: FuncId,
    /// `pon_make_cell(value) -> *mut PyObject`.
    pub make_cell: FuncId,
    /// `pon_cell_get(cell) -> *mut PyObject`.
    pub cell_get: FuncId,
    /// `pon_cell_set(cell, value) -> *mut PyObject`.
    pub cell_set: FuncId,
    /// `pon_cell_delete(cell) -> *mut PyObject`.
    pub cell_delete: FuncId,
    /// `pon_current_closure_cell(index) -> *mut PyObject`.
    pub current_closure_cell: FuncId,
    /// `pon_setup_annotations() -> *mut PyObject`.
    pub setup_annotations: FuncId,
    /// `pon_build_class(body, name_id, bases, base_count) -> *mut PyObject`.
    pub build_class: FuncId,
    /// `pon_load_build_class() -> *mut PyObject`.
    pub load_build_class: FuncId,
    /// `pon_store_global(name_id, value) -> *mut PyObject`.
    pub store_global: FuncId,
    /// `pon_none() -> *mut PyObject`.
    pub none: FuncId,
    /// `pon_runtime_init() -> i32`.
    pub runtime_init: FuncId,
    /// `pon_safepoint_poll()`.
    #[cfg(feature = "free-threading")]
    pub safepoint_poll: FuncId,
    /// `pon_gc_write_barrier(slot, value)`.
    #[cfg(feature = "free-threading")]
    pub gc_write_barrier: FuncId,
    /// `pon_gc_stop_requested() -> bool`.
    #[cfg(feature = "free-threading")]
    pub gc_stop_requested: FuncId,
}

/// Phase-B helper family used by the frozen codegen helper hub.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum HelperFamily {
    /// Numeric constructors and numeric operators.
    Number,
    /// Unicode, bytes, f-string, and template-string helpers.
    Str,
    /// Sequence/list/tuple/set/slice/unpack helpers.
    Seq,
    /// Dict, mapping, and subscript helpers.
    Map,
    /// Object, attribute, method, and truth helpers.
    ObjectAttr,
    /// Python call, function, class, and argument-binding helpers.
    Call,
    /// Exception-state helpers.
    Exc,
    /// Iterator, generator, coroutine, and async-iterator helpers.
    IterGen,
    /// Import, builtin, and namespace bootstrap helpers.
    ImportBuiltins,
    /// Structural pattern-matching helpers.
    Match,
}

/// ABI shape names used by [`HelperSig`].
///
/// These are descriptive hub shapes, not Cranelift imports.  Pointer-shaped
/// entries lower to the target pointer width when a runtime body is eventually
/// declared.  [`AbiShape::FeedbackCellPtr`] is the trailing profiling cell used
/// by specializable helpers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum AbiShape {
    /// No returned value.
    Void,
    /// C/Rust `u8` selector.
    U8,
    /// C/Rust `i32`.
    I32,
    /// C/Rust `i64`.
    I64,
    /// C/Rust `f64`.
    F64,
    /// C/Rust `u32`.
    U32,
    /// C/Rust `usize`.
    Usize,
    /// `*const u8`.
    ConstU8Ptr,
    /// `*const u32` interned-name array.
    ConstNamePtr,
    /// `*mut PyObject`.
    PyObjectPtr,
    /// `*mut *mut PyObject` argument/object array.
    PyObjectPtrPtr,
    /// Compiled-code entry pointer.
    CodePtr,
    /// `*const CodeInfo`.
    CodeInfoPtr,
    /// `*const FStrPartRaw`.
    FStrPartPtr,
    /// `*const TStrPartRaw`.
    TStrPartPtr,
    /// `*mut FeedbackCell`, always trailing when present.
    FeedbackCellPtr,
}

/// Stable identifier for a Phase-B helper signature.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum HelperId {
    ConstInt,
    ConstFloat,
    ConstComplex,
    ConstBool,
    NumberUnary,
    NumberBinary,
    NumberInplace,
    Contains,
    ConstStr,
    BuildString,
    BuildTemplate,
    BuildTuple,
    BuildList,
    BuildSet,
    BuildSlice,
    ListAppend,
    SetAdd,
    UnpackSeq,
    GetLen,
    BuildMap,
    MapInsert,
    DictMerge,
    DictMergeUnique,
    SubscriptGet,
    SubscriptSet,
    SubscriptDel,
    LoadGlobal,
    StoreGlobal,
    LoadName,
    StoreName,
    LoadBuiltin,
    LoadAttr,
    StoreAttr,
    DeleteAttr,
    LoadMethod,
    BoolTest,
    Is,
    Call,
    CallEx,
    CallMethod,
    MakeFunction,
    MakeFunctionFull,
    BuildClass,
    Raise,
    Reraise,
    PushExcInfo,
    PopExcInfo,
    MatchExc,
    CheckExcStar,
    GetCurrentExc,
    BuildExcGroup,
    GetIter,
    GetAIter,
    ForNext,
    Yield,
    YieldFrom,
    Await,
    EagerGeneratorReturn,
    ImportName,
    ImportFrom,
    ImportStar,
    SetupAnnotations,
    FunctionSetClosure,
    MakeCell,
    CellGet,
    CellSet,
    CellDelete,
    CurrentClosureCell,
    LoadBuildClass,
    MatchSequence,
    MatchMapping,
    MatchClass,
    MatchKeys,
    MatchLenGe,
}

/// Frozen Phase-B helper signature description.
///
/// `feedback_trailing` counts how many entries at the end of [`Self::params`] are
/// feedback/profiling cells.  The table is intentionally descriptive until each
/// runtime body exists; Phase-A codegen must keep using [`declare_helpers`] so it
/// imports only the implemented helpers it calls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HelperSig {
    /// Stable helper identity.
    pub id: HelperId,
    /// Owning semantic family.
    pub family: HelperFamily,
    /// Future C symbol name, or the current Phase-A symbol when already exported.
    pub symbol: &'static str,
    /// Parameter ABI shapes in call order.
    pub params: &'static [AbiShape],
    /// Return ABI shape.
    pub ret: AbiShape,
    /// Count of trailing feedback parameters in `params`.
    pub feedback_trailing: usize,
}

const P_I64: &[AbiShape] = &[AbiShape::I64];
const P_STR_BYTES: &[AbiShape] = &[AbiShape::ConstU8Ptr, AbiShape::Usize];
const P_STR_PARTS: &[AbiShape] = &[AbiShape::FStrPartPtr, AbiShape::Usize];
const P_TSTR_PARTS: &[AbiShape] = &[AbiShape::TStrPartPtr, AbiShape::Usize];
const P_OBJ_ARR: &[AbiShape] = &[AbiShape::PyObjectPtrPtr, AbiShape::Usize];
const P_OBJ_ARR_FEEDBACK: &[AbiShape] = &[AbiShape::PyObjectPtrPtr, AbiShape::Usize, AbiShape::FeedbackCellPtr];
const P_OBJ: &[AbiShape] = &[AbiShape::PyObjectPtr];
const P_OBJ_FEEDBACK: &[AbiShape] = &[AbiShape::PyObjectPtr, AbiShape::FeedbackCellPtr];
const P_OBJ_OBJ: &[AbiShape] = &[AbiShape::PyObjectPtr, AbiShape::PyObjectPtr];
const P_OBJ_OBJ_FEEDBACK: &[AbiShape] = &[AbiShape::PyObjectPtr, AbiShape::PyObjectPtr, AbiShape::FeedbackCellPtr];
const P_OBJ_OBJ_OBJ: &[AbiShape] = &[AbiShape::PyObjectPtr, AbiShape::PyObjectPtr, AbiShape::PyObjectPtr];
const P_OP_OBJ: &[AbiShape] = &[AbiShape::U8, AbiShape::PyObjectPtr, AbiShape::FeedbackCellPtr];
const P_OP_OBJ_OBJ: &[AbiShape] = &[AbiShape::U8, AbiShape::PyObjectPtr, AbiShape::PyObjectPtr, AbiShape::FeedbackCellPtr];
const P_NAME: &[AbiShape] = &[AbiShape::U32];
const P_NAME_OBJ: &[AbiShape] = &[AbiShape::U32, AbiShape::PyObjectPtr];
const P_OBJ_NAME: &[AbiShape] = &[AbiShape::PyObjectPtr, AbiShape::U32];
const P_OBJ_NAME_FEEDBACK: &[AbiShape] = &[AbiShape::PyObjectPtr, AbiShape::U32, AbiShape::FeedbackCellPtr];
const P_OBJ_NAME_OBJ: &[AbiShape] = &[AbiShape::PyObjectPtr, AbiShape::U32, AbiShape::PyObjectPtr];
const P_CALL: &[AbiShape] = &[AbiShape::PyObjectPtr, AbiShape::PyObjectPtrPtr, AbiShape::Usize];
const P_CALL_FEEDBACK: &[AbiShape] = &[
    AbiShape::PyObjectPtr,
    AbiShape::PyObjectPtrPtr,
    AbiShape::Usize,
    AbiShape::FeedbackCellPtr,
];
const P_CALL_EX: &[AbiShape] = &[
    AbiShape::PyObjectPtr,
    AbiShape::PyObjectPtrPtr,
    AbiShape::Usize,
    AbiShape::PyObjectPtr,
    AbiShape::ConstNamePtr,
    AbiShape::PyObjectPtrPtr,
    AbiShape::Usize,
    AbiShape::PyObjectPtr,
    AbiShape::FeedbackCellPtr,
];
const P_MAKE_FUNCTION: &[AbiShape] = &[AbiShape::CodePtr, AbiShape::Usize, AbiShape::U32];
const P_MAKE_FUNCTION_FULL: &[AbiShape] = &[
    AbiShape::CodeInfoPtr,
    AbiShape::PyObjectPtrPtr,
    AbiShape::Usize,
    AbiShape::ConstNamePtr,
    AbiShape::PyObjectPtrPtr,
    AbiShape::Usize,
    AbiShape::ConstNamePtr,
    AbiShape::PyObjectPtrPtr,
    AbiShape::Usize,
];
const P_FUNCTION_SET_CLOSURE: &[AbiShape] = &[AbiShape::PyObjectPtr, AbiShape::PyObjectPtrPtr, AbiShape::Usize];
const P_CELL_SET: &[AbiShape] = &[AbiShape::PyObjectPtr, AbiShape::PyObjectPtr];
const P_INDEX: &[AbiShape] = &[AbiShape::Usize];
const P_BUILD_CLASS: &[AbiShape] = &[AbiShape::PyObjectPtr, AbiShape::U32, AbiShape::PyObjectPtrPtr, AbiShape::Usize];
const P_RAISE: &[AbiShape] = &[AbiShape::PyObjectPtr, AbiShape::PyObjectPtr];
const P_HANDLER: &[AbiShape] = &[AbiShape::U32, AbiShape::U32, AbiShape::U8];
const P_IMPORT_NAME: &[AbiShape] = &[AbiShape::U32, AbiShape::ConstNamePtr, AbiShape::Usize, AbiShape::U32];
const P_MATCH_CLASS: &[AbiShape] = &[AbiShape::PyObjectPtr, AbiShape::PyObjectPtr, AbiShape::Usize, AbiShape::ConstNamePtr, AbiShape::Usize];
const P_MATCH_KEYS: &[AbiShape] = &[AbiShape::PyObjectPtr, AbiShape::PyObjectPtrPtr, AbiShape::Usize];
const P_MATCH_LEN: &[AbiShape] = &[AbiShape::PyObjectPtr, AbiShape::Usize, AbiShape::U8];
const P_NONE: &[AbiShape] = &[];

/// Frozen Phase-B helper signature table.
pub static PHASE_B_HELPERS: &[HelperSig] = &[
    HelperSig { id: HelperId::ConstInt, family: HelperFamily::Number, symbol: "pon_const_int", params: P_I64, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::ConstFloat, family: HelperFamily::Number, symbol: "pon_const_float", params: &[AbiShape::F64], ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::ConstComplex, family: HelperFamily::Number, symbol: "pon_const_complex", params: &[AbiShape::F64, AbiShape::F64], ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::ConstBool, family: HelperFamily::Number, symbol: "pon_const_bool", params: &[AbiShape::I32], ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::NumberUnary, family: HelperFamily::Number, symbol: "pon_number_unary", params: P_OP_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 1 },
    HelperSig { id: HelperId::NumberBinary, family: HelperFamily::Number, symbol: "pon_number_binary", params: P_OP_OBJ_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 1 },
    HelperSig { id: HelperId::NumberInplace, family: HelperFamily::Number, symbol: "pon_number_inplace", params: P_OP_OBJ_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 1 },
    HelperSig { id: HelperId::Contains, family: HelperFamily::Seq, symbol: "pon_contains", params: P_OBJ_OBJ, ret: AbiShape::I32, feedback_trailing: 0 },
    HelperSig { id: HelperId::ConstStr, family: HelperFamily::Str, symbol: "pon_const_str", params: P_STR_BYTES, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::BuildString, family: HelperFamily::Str, symbol: "pon_build_string", params: P_STR_PARTS, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::BuildTemplate, family: HelperFamily::Str, symbol: "pon_build_template", params: P_TSTR_PARTS, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::BuildTuple, family: HelperFamily::Seq, symbol: "pon_build_tuple", params: P_OBJ_ARR, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::BuildList, family: HelperFamily::Seq, symbol: "pon_build_list", params: P_OBJ_ARR, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::BuildSet, family: HelperFamily::Seq, symbol: "pon_build_set", params: P_OBJ_ARR, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::BuildSlice, family: HelperFamily::Seq, symbol: "pon_build_slice", params: P_OBJ_OBJ_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::ListAppend, family: HelperFamily::Seq, symbol: "pon_list_append", params: P_OBJ_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::SetAdd, family: HelperFamily::Seq, symbol: "pon_set_add", params: P_OBJ_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::UnpackSeq, family: HelperFamily::Seq, symbol: "pon_unpack_seq", params: P_OBJ_ARR_FEEDBACK, ret: AbiShape::PyObjectPtrPtr, feedback_trailing: 1 },
    HelperSig { id: HelperId::GetLen, family: HelperFamily::Seq, symbol: "pon_get_len", params: P_OBJ_FEEDBACK, ret: AbiShape::PyObjectPtr, feedback_trailing: 1 },
    HelperSig { id: HelperId::BuildMap, family: HelperFamily::Map, symbol: "pon_build_map", params: P_OBJ_ARR, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::MapInsert, family: HelperFamily::Map, symbol: "pon_map_insert", params: P_OBJ_OBJ_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::DictMerge, family: HelperFamily::Map, symbol: "pon_dict_merge", params: P_OBJ_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::DictMergeUnique, family: HelperFamily::Map, symbol: "pon_dict_merge_unique", params: P_OBJ_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::SubscriptGet, family: HelperFamily::Map, symbol: "pon_subscript_get", params: P_OBJ_OBJ_FEEDBACK, ret: AbiShape::PyObjectPtr, feedback_trailing: 1 },
    HelperSig { id: HelperId::SubscriptSet, family: HelperFamily::Map, symbol: "pon_subscript_set", params: P_OBJ_OBJ_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::SubscriptDel, family: HelperFamily::Map, symbol: "pon_subscript_del", params: P_OBJ_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::LoadGlobal, family: HelperFamily::ImportBuiltins, symbol: "pon_load_global", params: P_NAME, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::StoreGlobal, family: HelperFamily::ImportBuiltins, symbol: "pon_store_global", params: P_NAME_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::LoadName, family: HelperFamily::ImportBuiltins, symbol: "pon_load_name", params: P_NAME, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::StoreName, family: HelperFamily::ImportBuiltins, symbol: "pon_store_name", params: P_NAME_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::LoadBuiltin, family: HelperFamily::ImportBuiltins, symbol: "pon_load_builtin", params: P_NAME, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::LoadAttr, family: HelperFamily::ObjectAttr, symbol: "pon_load_attr", params: P_OBJ_NAME_FEEDBACK, ret: AbiShape::PyObjectPtr, feedback_trailing: 1 },
    HelperSig { id: HelperId::StoreAttr, family: HelperFamily::ObjectAttr, symbol: "pon_store_attr", params: P_OBJ_NAME_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::DeleteAttr, family: HelperFamily::ObjectAttr, symbol: "pon_delete_attr", params: P_OBJ_NAME, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::LoadMethod, family: HelperFamily::ObjectAttr, symbol: "pon_load_method", params: P_OBJ_NAME_FEEDBACK, ret: AbiShape::PyObjectPtr, feedback_trailing: 1 },
    HelperSig { id: HelperId::BoolTest, family: HelperFamily::ObjectAttr, symbol: "pon_bool_test", params: P_OBJ_FEEDBACK, ret: AbiShape::PyObjectPtr, feedback_trailing: 1 },
    HelperSig { id: HelperId::Is, family: HelperFamily::ObjectAttr, symbol: "pon_is", params: P_OBJ_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::Call, family: HelperFamily::Call, symbol: "pon_call", params: P_CALL, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::CallEx, family: HelperFamily::Call, symbol: "pon_call_ex", params: P_CALL_EX, ret: AbiShape::PyObjectPtr, feedback_trailing: 1 },
    HelperSig { id: HelperId::CallMethod, family: HelperFamily::Call, symbol: "pon_call_method", params: P_CALL_FEEDBACK, ret: AbiShape::PyObjectPtr, feedback_trailing: 1 },
    HelperSig { id: HelperId::MakeFunction, family: HelperFamily::Call, symbol: "pon_make_function", params: P_MAKE_FUNCTION, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::MakeFunctionFull, family: HelperFamily::Call, symbol: "pon_make_function_full", params: P_MAKE_FUNCTION_FULL, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::BuildClass, family: HelperFamily::Call, symbol: "pon_build_class", params: P_BUILD_CLASS, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::Raise, family: HelperFamily::Exc, symbol: "pon_raise", params: P_RAISE, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::Reraise, family: HelperFamily::Exc, symbol: "pon_reraise", params: P_NONE, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::PushExcInfo, family: HelperFamily::Exc, symbol: "pon_push_exc_info", params: P_HANDLER, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::PopExcInfo, family: HelperFamily::Exc, symbol: "pon_pop_exc_info", params: P_NONE, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::MatchExc, family: HelperFamily::Exc, symbol: "pon_match_exc", params: P_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::CheckExcStar, family: HelperFamily::Exc, symbol: "pon_check_exc_star", params: P_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::GetCurrentExc, family: HelperFamily::Exc, symbol: "pon_get_current_exc", params: P_NONE, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::BuildExcGroup, family: HelperFamily::Exc, symbol: "pon_build_exc_group", params: P_OBJ_ARR, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::GetIter, family: HelperFamily::IterGen, symbol: "pon_get_iter", params: P_OBJ_FEEDBACK, ret: AbiShape::PyObjectPtr, feedback_trailing: 1 },
    HelperSig { id: HelperId::GetAIter, family: HelperFamily::IterGen, symbol: "pon_get_aiter", params: P_OBJ_FEEDBACK, ret: AbiShape::PyObjectPtr, feedback_trailing: 1 },
    HelperSig { id: HelperId::ForNext, family: HelperFamily::IterGen, symbol: "pon_for_next", params: P_OBJ_FEEDBACK, ret: AbiShape::PyObjectPtr, feedback_trailing: 1 },
    HelperSig { id: HelperId::Yield, family: HelperFamily::IterGen, symbol: "pon_yield", params: P_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::FunctionSetClosure, family: HelperFamily::Call, symbol: "pon_function_set_closure", params: P_FUNCTION_SET_CLOSURE, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::MakeCell, family: HelperFamily::Call, symbol: "pon_make_cell", params: P_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::CellGet, family: HelperFamily::Call, symbol: "pon_cell_get", params: P_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::CellSet, family: HelperFamily::Call, symbol: "pon_cell_set", params: P_CELL_SET, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::CellDelete, family: HelperFamily::Call, symbol: "pon_cell_delete", params: P_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::CurrentClosureCell, family: HelperFamily::Call, symbol: "pon_current_closure_cell", params: P_INDEX, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::YieldFrom, family: HelperFamily::IterGen, symbol: "pon_yield_from", params: P_OBJ_FEEDBACK, ret: AbiShape::PyObjectPtr, feedback_trailing: 1 },
    HelperSig { id: HelperId::Await, family: HelperFamily::IterGen, symbol: "pon_await", params: P_OBJ_FEEDBACK, ret: AbiShape::PyObjectPtr, feedback_trailing: 1 },
    HelperSig { id: HelperId::EagerGeneratorReturn, family: HelperFamily::IterGen, symbol: "pon_eager_yield_generator", params: P_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::ImportName, family: HelperFamily::ImportBuiltins, symbol: "pon_import_name", params: P_IMPORT_NAME, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::ImportFrom, family: HelperFamily::ImportBuiltins, symbol: "pon_import_from", params: P_OBJ_NAME, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::ImportStar, family: HelperFamily::ImportBuiltins, symbol: "pon_import_star", params: P_OBJ, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::SetupAnnotations, family: HelperFamily::ImportBuiltins, symbol: "pon_setup_annotations", params: P_NONE, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::LoadBuildClass, family: HelperFamily::ImportBuiltins, symbol: "pon_load_build_class", params: P_NONE, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::MatchSequence, family: HelperFamily::Match, symbol: "pon_match_sequence", params: P_OBJ_FEEDBACK, ret: AbiShape::PyObjectPtr, feedback_trailing: 1 },
    HelperSig { id: HelperId::MatchMapping, family: HelperFamily::Match, symbol: "pon_match_mapping", params: P_OBJ_FEEDBACK, ret: AbiShape::PyObjectPtr, feedback_trailing: 1 },
    HelperSig { id: HelperId::MatchClass, family: HelperFamily::Match, symbol: "pon_match_class", params: P_MATCH_CLASS, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::MatchKeys, family: HelperFamily::Match, symbol: "pon_match_keys", params: P_MATCH_KEYS, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
    HelperSig { id: HelperId::MatchLenGe, family: HelperFamily::Match, symbol: "pon_match_len_ge", params: P_MATCH_LEN, ret: AbiShape::PyObjectPtr, feedback_trailing: 0 },
];

/// Look up a frozen Phase-B helper signature by id.
#[must_use]
pub fn helper_sig(id: HelperId) -> Option<&'static HelperSig> {
    PHASE_B_HELPERS.iter().find(|sig| sig.id == id)
}

/// Declare runtime helpers that baseline codegen can call as Cranelift imports.
///
/// Signatures are built from [`pon_runtime::abi::HELPERS`] and [`AbiTy`]. Pointer
/// ABI types use `module.target_config().pointer_type()`, while integer ABI types
/// are represented by same-width Cranelift integer types because CLIF integer
/// values carry width, not signedness. Every import uses a runtime-exported
/// `symbol` string and [`Linkage::Import`].
pub fn declare_helpers<M: Module>(module: &mut M) -> ModuleResult<HelperRefs> {
    Ok(HelperRefs {
        const_int: declare_one(module, "pon_const_int")?,
        const_float: declare_one(module, "pon_const_float")?,
        const_complex: declare_one(module, "pon_const_complex")?,
        const_bool: declare_one(module, "pon_const_bool")?,
        const_str: declare_one(module, "pon_const_str")?,
        binary_add: declare_one(module, "pon_binary_add")?,
        rich_compare: declare_one(module, "pon_rich_compare")?,
        number_unary: declare_one(module, "pon_number_unary")?,
        number_binary: declare_one(module, "pon_number_binary")?,
        number_inplace: declare_one(module, "pon_number_inplace")?,
        is_true: declare_one(module, "pon_is_true")?,
        contains: declare_one(module, "pon_contains")?,
        call: declare_one(module, "pon_call")?,
        call_ex: declare_one(module, "pon_call_ex")?,
        call_method: declare_one(module, "pon_call_method")?,
        load_global: declare_one(module, "pon_load_global")?,
        load_name: declare_one(module, "pon_load_name")?,
        load_builtin: declare_one(module, "pon_load_builtin")?,
        store_name: declare_one(module, "pon_store_name")?,
        get_attr: declare_one(module, "pon_get_attr")?,
        set_attr: declare_one(module, "pon_set_attr")?,
        del_attr: declare_one(module, "pon_del_attr")?,
        build_tuple: declare_one(module, "pon_build_tuple")?,
        build_list: declare_one(module, "pon_build_list")?,
        build_set: declare_one(module, "pon_build_set")?,
        build_slice: declare_one(module, "pon_build_slice")?,
        list_append: declare_one(module, "pon_list_append")?,
        set_add: declare_one(module, "pon_set_add")?,
        list_extend: declare_one(module, "pon_list_extend")?,
        unpack_seq: declare_one(module, "pon_unpack_seq")?,
        unpack_ex: declare_one(module, "pon_unpack_ex")?,
        get_len: declare_one(module, "pon_get_len")?,
        build_map: declare_one(module, "pon_build_map")?,
        map_insert: declare_one(module, "pon_map_insert")?,
        dict_merge: declare_one(module, "pon_dict_merge")?,
        dict_merge_unique: declare_one(module, "pon_dict_merge_unique")?,
        subscript_get: declare_one(module, "pon_subscript_get")?,
        subscript_set: declare_one(module, "pon_subscript_set")?,
        subscript_del: declare_one(module, "pon_subscript_del")?,
        build_string: declare_one(module, "pon_build_string")?,
        build_template: declare_one(module, "pon_build_template")?,
        import_name: declare_one(module, "pon_import_name")?,
        import_from: declare_one(module, "pon_import_from")?,
        import_star: declare_one(module, "pon_import_star")?,
        raise: declare_one(module, "pon_raise")?,
        reraise: declare_one(module, "pon_reraise")?,
        push_exc_info: declare_one(module, "pon_push_exc_info")?,
        pop_exc_info: declare_one(module, "pon_pop_exc_info")?,
        match_exc: declare_one(module, "pon_match_exc")?,
        check_exc_star: declare_one(module, "pon_check_exc_star")?,
        get_current_exc: declare_one(module, "pon_get_current_exc")?,
        build_exc_group: declare_one(module, "pon_build_exc_group")?,
        get_iter: declare_one(module, "pon_get_iter")?,
        get_aiter: declare_one(module, "pon_get_aiter")?,
        for_next: declare_one(module, "pon_for_next")?,
        gen_stop_value: declare_one(module, "pon_gen_stop_value")?,
        yield_value: declare_one(module, "pon_yield")?,
        yield_from: declare_one(module, "pon_yield_from")?,
        await_value: declare_one(module, "pon_await")?,
        eager_yield_generator: declare_one(module, "pon_eager_yield_generator")?,
        match_sequence: declare_one(module, "pon_match_sequence")?,
        match_mapping: declare_one(module, "pon_match_mapping")?,
        match_class: declare_one(module, "pon_match_class")?,
        match_keys: declare_one(module, "pon_match_keys")?,
        match_len_ge: declare_one(module, "pon_match_len_ge")?,
        print: declare_one(module, "pon_print")?,
        make_function: declare_one(module, "pon_make_function")?,
        make_function_full: declare_one(module, "pon_make_function_full")?,
        function_set_closure: declare_one(module, "pon_function_set_closure")?,
        make_cell: declare_one(module, "pon_make_cell")?,
        cell_get: declare_one(module, "pon_cell_get")?,
        cell_set: declare_one(module, "pon_cell_set")?,
        cell_delete: declare_one(module, "pon_cell_delete")?,
        current_closure_cell: declare_one(module, "pon_current_closure_cell")?,
        setup_annotations: declare_one(module, "pon_setup_annotations")?,
        build_class: declare_one(module, "pon_build_class")?,
        load_build_class: declare_one(module, "pon_load_build_class")?,
        store_global: declare_one(module, "pon_store_global")?,
        none: declare_one(module, "pon_none")?,
        runtime_init: declare_one(module, "pon_runtime_init")?,
        #[cfg(feature = "free-threading")]
        safepoint_poll: declare_free_threading_helper(module, crate::FT_SAFEPOINT_POLL, &[], None)?,
        #[cfg(feature = "free-threading")]
        gc_write_barrier: declare_free_threading_helper(
            module,
            crate::FT_GC_WRITE_BARRIER,
            &[module.target_config().pointer_type(), module.target_config().pointer_type()],
            None,
        )?,
        #[cfg(feature = "free-threading")]
        gc_stop_requested: declare_free_threading_helper(module, crate::FT_GC_STOP_REQUESTED, &[], Some(ir::types::I8))?,
    })
}

fn declare_one<M: Module>(module: &mut M, symbol: &str) -> ModuleResult<FuncId> {
    let decl = HELPERS
        .iter()
        .find(|decl| decl.symbol == symbol)
        .unwrap_or_else(|| panic!("pon-runtime HELPERS must contain helper symbol {symbol}"));
    let sig = helper_signature(module, decl.params, decl.ret);
    module.declare_function(decl.symbol, Linkage::Import, &sig)
}

#[cfg(feature = "free-threading")]
fn declare_free_threading_helper<M: Module>(
    module: &mut M,
    symbol: &str,
    params: &[ir::Type],
    ret: Option<ir::Type>,
) -> ModuleResult<FuncId> {
    let mut sig = module.make_signature();
    sig.params.extend(params.iter().copied().map(AbiParam::new));
    if let Some(ret) = ret {
        sig.returns.push(AbiParam::new(ret));
    }
    module.declare_function(symbol, Linkage::Import, &sig)
}

fn helper_signature<M: Module>(module: &M, params: &[AbiTy], ret: AbiTy) -> ir::Signature {
    let mut sig = module.make_signature();
    sig.params
        .extend(params.iter().copied().map(|ty| AbiParam::new(abi_type(module, ty))));
    sig.returns.push(AbiParam::new(abi_type(module, ret)));
    sig
}

fn abi_type<M: Module>(module: &M, ty: AbiTy) -> ir::Type {
    let ptr = module.target_config().pointer_type();
    match ty {
        AbiTy::U8 => ir::types::I8,
        AbiTy::U16 => ir::types::I16,
        AbiTy::I32 | AbiTy::U32 => ir::types::I32,
        AbiTy::I64 => ir::types::I64,
        AbiTy::F64 => ir::types::F64,
        AbiTy::ISize
        | AbiTy::Usize
        | AbiTy::ConstU8Ptr
        | AbiTy::ConstU32Ptr
        | AbiTy::CodePtr
        | AbiTy::CodeInfoPtr
        | AbiTy::FStrPartPtr
        | AbiTy::TStrPartPtr
        | AbiTy::GenResumePtr
        | AbiTy::PyFramePtr
        | AbiTy::PyObjectPtr
        | AbiTy::PyObjectPtrPtr
        | AbiTy::FeedbackCellPtr
        | AbiTy::ThreadStatePtr => ptr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_signature_lookup_covers_feedback_trailing_params() {
        let sig = helper_sig(HelperId::NumberBinary).expect("number helper sig");

        assert_eq!(sig.family, HelperFamily::Number);
        assert_eq!(sig.symbol, "pon_number_binary");
        assert_eq!(sig.ret, AbiShape::PyObjectPtr);
        assert_eq!(sig.feedback_trailing, 1);
        assert_eq!(sig.params.last(), Some(&AbiShape::FeedbackCellPtr));
    }

    #[test]
    fn phase_a_helper_signature_is_recorded_without_changing_declare_api() {
        let sig = helper_sig(HelperId::Call).expect("call helper sig");

        assert_eq!(sig.symbol, "pon_call");
        assert_eq!(sig.params, P_CALL);
        assert_eq!(sig.feedback_trailing, 0);
    }
}
