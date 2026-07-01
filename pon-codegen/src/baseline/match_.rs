//! Structural pattern-matching Phase-B lowering family.
//!
//! Baseline emits direct helper calls for the pattern predicates/extractors that
//! have runtime bodies. Returned boxed truth values and extract carriers preserve
//! the shared NULL-sentinel error contract.

use cranelift_codegen::ir::{self, InstBuilder, StackSlotData, StackSlotKind};
use cranelift_frontend::FunctionBuilder;
use pon_ir::ir::{NameId, Value as IrValue};

use super::{CodegenError, HelperFuncRefs, LowerState, NameMap, build_call_argv, call_pyobject_helper, offset_i32};

#[allow(dead_code)]
pub(crate) const MATCH_SEQUENCE_HELPER: &str = "pon_match_sequence";
#[allow(dead_code)]
pub(crate) const MATCH_MAPPING_HELPER: &str = "pon_match_mapping";
#[allow(dead_code)]
pub(crate) const MATCH_CLASS_HELPER: &str = "pon_match_class";
#[allow(dead_code)]
pub(crate) const MATCH_KEYS_HELPER: &str = "pon_match_keys";
#[allow(dead_code)]
pub(crate) const MATCH_LEN_GE_HELPER: &str = "pon_match_len_ge";
#[allow(dead_code)]
pub(crate) const GET_LEN_HELPER: &str = "pon_get_len";

/// Sequence-pattern predicate lowering.
pub(crate) fn lower_match_sequence(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    subj: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    lower_subject_feedback(builder, helper, state, subj, ptr_ty, exception_exit)
}

/// Mapping-pattern predicate lowering.
pub(crate) fn lower_match_mapping(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    subj: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    lower_subject_feedback(builder, helper, state, subj, ptr_ty, exception_exit)
}

/// Class-pattern extraction lowering.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_match_class(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    names: &NameMap,
    state: &LowerState,
    subj: IrValue,
    cls: IrValue,
    nargs: usize,
    kw: &[NameId],
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let subj = state.value(subj)?;
    let cls = state.value(cls)?;
    let nargs = builder.ins().iconst(ptr_ty, nargs as i64);
    let kw_ptr = build_name_array(builder, names, kw, ptr_ty)?;
    let nkw = builder.ins().iconst(ptr_ty, kw.len() as i64);
    Ok(call_pyobject_helper(
        builder,
        helper,
        &[subj, cls, nargs, kw_ptr, nkw],
        ptr_ty,
        exception_exit,
    ))
}

/// Mapping-key pattern extraction lowering.
pub(crate) fn lower_match_keys(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    helpers: &HelperFuncRefs,
    state: &LowerState,
    subj: IrValue,
    keys: &[IrValue],
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let subj = state.value(subj)?;
    let key_count = keys.len();
    let keys = build_call_argv(builder, helpers, state, keys, ptr_ty, ptr_bytes)?;
    let count = builder.ins().iconst(ptr_ty, key_count as i64);
    Ok(call_pyobject_helper(builder, helper, &[subj, keys, count], ptr_ty, exception_exit))
}

/// Pattern-length predicate lowering.
pub(crate) fn lower_match_len_ge(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    subj: IrValue,
    n: usize,
    exact: bool,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let subj = state.value(subj)?;
    let n = builder.ins().iconst(ptr_ty, n as i64);
    let exact = builder.ins().iconst(ir::types::I8, i64::from(u8::from(exact)));
    Ok(call_pyobject_helper(builder, helper, &[subj, n, exact], ptr_ty, exception_exit))
}

fn lower_subject_feedback(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    subj: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let subj = state.value(subj)?;
    let feedback = builder.ins().iconst(ptr_ty, 0);
    Ok(call_pyobject_helper(builder, helper, &[subj, feedback], ptr_ty, exception_exit))
}

fn build_name_array(
    builder: &mut FunctionBuilder<'_>,
    names: &NameMap,
    source_names: &[NameId],
    ptr_ty: ir::Type,
) -> Result<ir::Value, CodegenError> {
    if source_names.is_empty() {
        return Ok(builder.ins().iconst(ptr_ty, 0));
    }
    let size = source_names
        .len()
        .checked_mul(4)
        .ok_or(CodegenError::OffsetTooLarge { offset: usize::MAX })?;
    let slot = builder.create_sized_stack_slot(StackSlotData {
        kind: StackSlotKind::ExplicitSlot,
        size: size.try_into().map_err(|_| CodegenError::OffsetTooLarge { offset: size })?,
        align_shift: 2,
        key: None,
    });
    for (index, name) in source_names.iter().enumerate() {
        let runtime_name = builder.ins().iconst(ir::types::I32, i64::from(names.runtime_id(name.0)?));
        builder.ins().stack_store(runtime_name, slot, offset_i32(index * 4)?);
    }
    Ok(builder.ins().stack_addr(ptr_ty, slot, 0))
}
