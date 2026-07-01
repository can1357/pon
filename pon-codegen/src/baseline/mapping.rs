//! Mapping and subscript Phase-B lowering family.
//!
//! The parallel WS-MAP wave owns the lowering routines here, but the shared
//! `HelperFuncRefs`/dispatch wiring is deliberately left to the post-wave
//! integration pass.  The `_with_helper` entry points below are complete CLIF
//! lowerers once that pass supplies the corresponding runtime helper `FuncRef`.

use cranelift_codegen::ir::{self, InstBuilder};
use cranelift_frontend::FunctionBuilder;
use pon_ir::ir::Value as IrValue;

use super::{CodegenError, HelperFuncRefs, LowerState, build_call_argv, call_pyobject_helper};


/// Lowers `BuildMap` through `pon_build_map(flat_pairs, pair_count)`.
pub(crate) fn lower_build_map_with_helper(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    helpers: &HelperFuncRefs,
    state: &LowerState,
    pairs: &[(IrValue, IrValue)],
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let mut flat = Vec::with_capacity(pairs.len().saturating_mul(2));
    for (key, value) in pairs {
        flat.push(*key);
        flat.push(*value);
    }
    let argv = build_call_argv(builder, helpers, state, &flat, ptr_ty, ptr_bytes)?;
    let count = builder.ins().iconst(ptr_ty, pairs.len() as i64);
    Ok(call_pyobject_helper(builder, helper, &[argv, count], ptr_ty, exception_exit))
}

/// Lowers `MapInsert` through `pon_map_insert(map, key, value)`.
pub(crate) fn lower_map_insert_with_helper(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    map: IrValue,
    key: IrValue,
    value: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let map = state.value(map)?;
    let key = state.value(key)?;
    let value = state.value(value)?;
    Ok(call_pyobject_helper(builder, helper, &[map, key, value], ptr_ty, exception_exit))
}

/// Lowers `DictMerge` through `pon_dict_merge(map, other)`.
pub(crate) fn lower_dict_merge_with_helper(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    map: IrValue,
    other: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let map = state.value(map)?;
    let other = state.value(other)?;
    Ok(call_pyobject_helper(builder, helper, &[map, other], ptr_ty, exception_exit))
}

/// Lowers `SubscriptGet` through `pon_subscript_get(obj, index, feedback=NULL)`.
pub(crate) fn lower_subscript_get_with_helper(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    obj: IrValue,
    index: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let obj = state.value(obj)?;
    let index = state.value(index)?;
    let feedback = builder.ins().iconst(ptr_ty, 0);
    Ok(call_pyobject_helper(builder, helper, &[obj, index, feedback], ptr_ty, exception_exit))
}

/// Lowers `SubscriptSet` through `pon_subscript_set(obj, index, value)`.
pub(crate) fn lower_subscript_set_with_helper(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    obj: IrValue,
    index: IrValue,
    value: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let obj = state.value(obj)?;
    let index = state.value(index)?;
    let value = state.value(value)?;
    Ok(call_pyobject_helper(builder, helper, &[obj, index, value], ptr_ty, exception_exit))
}

/// Lowers `SubscriptDel` through `pon_subscript_del(obj, index)`.
pub(crate) fn lower_subscript_del_with_helper(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    obj: IrValue,
    index: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let obj = state.value(obj)?;
    let index = state.value(index)?;
    Ok(call_pyobject_helper(builder, helper, &[obj, index], ptr_ty, exception_exit))
}
