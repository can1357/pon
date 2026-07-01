//! Sequence/container Phase-B lowering family.
//!
//! Tuple, list, set, slice, unpacking, and length helpers are centralized here
//! so helper imports stay behind one dispatch seam.

use cranelift_codegen::ir::{self, InstBuilder};
use cranelift_frontend::FunctionBuilder;
use pon_ir::ir::Value as IrValue;

use super::{CodegenError, HelperFuncRefs, LowerState, build_call_argv, call_pyobject_helper};

/// Lower tuple construction through `pon_build_tuple(argv, argc)`.
pub(crate) fn lower_build_tuple(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    helpers: &HelperFuncRefs,
    state: &LowerState,
    elts: &[IrValue],
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    lower_build_sequence(builder, helper, helpers, state, elts, ptr_ty, ptr_bytes, exception_exit)
}

/// Lower list construction through `pon_build_list(argv, argc)`.
pub(crate) fn lower_build_list(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    helpers: &HelperFuncRefs,
    state: &LowerState,
    elts: &[IrValue],
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    lower_build_sequence(builder, helper, helpers, state, elts, ptr_ty, ptr_bytes, exception_exit)
}

/// Lower set construction through `pon_build_set(argv, argc)`.
pub(crate) fn lower_build_set(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    helpers: &HelperFuncRefs,
    state: &LowerState,
    elts: &[IrValue],
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    lower_build_sequence(builder, helper, helpers, state, elts, ptr_ty, ptr_bytes, exception_exit)
}

/// Lower slice construction through `pon_build_slice(start, stop, step)`.
pub(crate) fn lower_build_slice(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    lower: IrValue,
    upper: IrValue,
    step: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let lower = state.value(lower)?;
    let upper = state.value(upper)?;
    let step = state.value(step)?;
    Ok(call_pyobject_helper(builder, helper, &[lower, upper, step], ptr_ty, exception_exit))
}

/// Lower list append through `pon_list_append(list, item)`.
pub(crate) fn lower_list_append(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    list: IrValue,
    item: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let list = state.value(list)?;
    let item = state.value(item)?;
    Ok(call_pyobject_helper(builder, helper, &[list, item], ptr_ty, exception_exit))
}

/// Lower set add through `pon_set_add(set, item)`.
pub(crate) fn lower_set_add(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    set: IrValue,
    item: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let set = state.value(set)?;
    let item = state.value(item)?;
    Ok(call_pyobject_helper(builder, helper, &[set, item], ptr_ty, exception_exit))
}

/// Lower list extend through `pon_list_extend(list, iterable)`.
pub(crate) fn lower_list_extend(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    list: IrValue,
    iter: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let list = state.value(list)?;
    let iter = state.value(iter)?;
    Ok(call_pyobject_helper(builder, helper, &[list, iter], ptr_ty, exception_exit))
}

/// Lower exact sequence unpack through `pon_unpack_seq(value, n, feedback=NULL)`.
pub(crate) fn lower_unpack_seq(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    val: IrValue,
    n: usize,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let val = state.value(val)?;
    let n = builder.ins().iconst(ptr_ty, n as i64);
    let feedback = builder.ins().iconst(ptr_ty, 0);
    Ok(call_pyobject_helper(builder, helper, &[val, n, feedback], ptr_ty, exception_exit))
}

/// Lower starred sequence unpack through `pon_unpack_ex(value, before, after)`.
pub(crate) fn lower_unpack_ex(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    val: IrValue,
    before: usize,
    after: usize,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let val = state.value(val)?;
    let before = builder.ins().iconst(ptr_ty, before as i64);
    let after = builder.ins().iconst(ptr_ty, after as i64);
    Ok(call_pyobject_helper(builder, helper, &[val, before, after], ptr_ty, exception_exit))
}

/// Lower pattern/container length through `pon_get_len(subject, feedback=NULL)`.
pub(crate) fn lower_get_len(
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

fn lower_build_sequence(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    helpers: &HelperFuncRefs,
    state: &LowerState,
    elts: &[IrValue],
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let argv = build_call_argv(builder, helpers, state, elts, ptr_ty, ptr_bytes)?;
    let argc = builder.ins().iconst(ptr_ty, elts.len() as i64);
    Ok(call_pyobject_helper(builder, helper, &[argv, argc], ptr_ty, exception_exit))
}
