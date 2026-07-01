//! Control-flow and singleton lowering family.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{self, FuncRef, InstBuilder};
use cranelift_frontend::FunctionBuilder;
use pon_ir::ir::{BlockId, Terminator};

use super::{CodegenError, HelperFuncRefs, LowerState, call_pyobject_helper};
#[cfg(feature = "free-threading")]
use super::emit_safepoint_poll;

/// Lower the Phase-A `None` singleton through `pon_none`.
pub(crate) fn lower_const_none(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    Ok(call_pyobject_helper(builder, helpers.none, &[], ptr_ty, exception_exit))
}

/// Return a typed unsupported value-lowering error for future Phase-B variants.
pub(crate) fn lower_future_value(feature: &'static str) -> Result<ir::Value, CodegenError> {
    Err(CodegenError::Unsupported(feature))
}

/// Lower a basic-block terminator, accepting only Phase-A `Return` today.
pub(crate) fn lower_terminator(
    builder: &mut FunctionBuilder<'_>,
    state: &LowerState,
    ptr_ty: ir::Type,
    term: &Terminator,
) -> Result<(), CodegenError> {
    match term {
        Terminator::Return(value) => {
            let value = state.value(*value)?;
            builder.ins().return_(&[value]);
            Ok(())
        }
        Terminator::Jump(_)
        | Terminator::Branch { .. }
        | Terminator::CondBranch { .. }
        | Terminator::ForLoop { .. }
        | Terminator::Suspend { .. }
        | Terminator::RaiseTerm
        | Terminator::Unreachable => {
            let null = builder.ins().iconst(ptr_ty, 0);
            builder.ins().return_(&[null]);
            Err(CodegenError::Unsupported("non-return terminator"))
        }
        _ => Err(CodegenError::Unsupported("unknown future terminator")),
    }
}

pub(crate) fn lower_terminator_with_blocks(
    builder: &mut FunctionBuilder<'_>,
    state: &LowerState,
    helpers: &HelperFuncRefs,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
    block_map: &[(BlockId, ir::Block)],
    current_block: BlockId,
    term: &Terminator,
) -> Result<(), CodegenError> {
    match term {
        Terminator::Return(value) => {
            let value = state.value(*value)?;
            builder.ins().return_(&[value]);
            Ok(())
        }
        Terminator::Jump(target) => {
            emit_loop_backedge_safepoint(builder, helpers, current_block, *target);
            let target = clif_block(block_map, *target)?;
            builder.ins().jump(target, &[]);
            Ok(())
        }
        Terminator::Branch {
            cond,
            then_blk,
            else_blk,
        } => {
            emit_conditional_backedge_safepoint(builder, helpers, current_block, *then_blk, *else_blk);
            lower_conditional_branch(builder, state, helpers.is_true, exception_exit, block_map, *cond, *then_blk, *else_blk)
        }
        Terminator::CondBranch { cond, then_, else_ } => {
            emit_conditional_backedge_safepoint(builder, helpers, current_block, *then_, *else_);
            lower_conditional_branch(builder, state, helpers.is_true, exception_exit, block_map, *cond, *then_, *else_)
        }
        Terminator::ForLoop { iter: _, body, done } => {
            let item = state
                .last_value()
                .ok_or(CodegenError::Unsupported("ForLoop without preceding ForNext"))?;
            let body = clif_block(block_map, *body)?;
            let done = clif_block(block_map, *done)?;
            let stop_check = builder.create_block();
            let has_item = builder.ins().icmp_imm(IntCC::NotEqual, item, 0);
            builder.ins().brif(has_item, body, &[], stop_check, &[]);
            builder.switch_to_block(stop_check);
            builder.seal_block(stop_check);
            let stop_value = call_pyobject_helper(builder, helpers.gen_stop_value, &[], ptr_ty, exception_exit);
            let stopped = builder.ins().icmp_imm(IntCC::NotEqual, stop_value, 0);
            builder.ins().brif(stopped, done, &[], exception_exit, &[]);
            Ok(())
        }
        Terminator::Unreachable => Err(CodegenError::Unsupported("unreachable terminator")),
        Terminator::Suspend { .. } | Terminator::RaiseTerm => Err(CodegenError::Unsupported("generator/exception terminator")),
        _ => Err(CodegenError::Unsupported("unknown future terminator")),
    }
}

fn emit_loop_backedge_safepoint(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    current_block: BlockId,
    target: BlockId,
) {
    #[cfg(feature = "free-threading")]
    {
        if target.0 <= current_block.0 {
            emit_safepoint_poll(builder, helpers);
        }
    }

    #[cfg(not(feature = "free-threading"))]
    {
        let _ = (builder, helpers, current_block, target);
    }
}

fn emit_conditional_backedge_safepoint(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    current_block: BlockId,
    true_target: BlockId,
    false_target: BlockId,
) {
    #[cfg(feature = "free-threading")]
    {
        if true_target.0 <= current_block.0 || false_target.0 <= current_block.0 {
            emit_safepoint_poll(builder, helpers);
        }
    }

    #[cfg(not(feature = "free-threading"))]
    {
        let _ = (builder, helpers, current_block, true_target, false_target);
    }
}

fn lower_conditional_branch(
    builder: &mut FunctionBuilder<'_>,
    state: &LowerState,
    is_true: FuncRef,
    exception_exit: ir::Block,
    block_map: &[(BlockId, ir::Block)],
    cond: pon_ir::ir::Value,
    then_block: BlockId,
    else_block: BlockId,
) -> Result<(), CodegenError> {
    let cond = state.value(cond)?;
    let truth = call_is_true(builder, is_true, cond, exception_exit);
    let then_block = clif_block(block_map, then_block)?;
    let else_block = clif_block(block_map, else_block)?;
    builder.ins().brif(truth, then_block, &[], else_block, &[]);
    Ok(())
}

fn call_is_true(
    builder: &mut FunctionBuilder<'_>,
    is_true: FuncRef,
    value: ir::Value,
    exception_exit: ir::Block,
) -> ir::Value {
    let call = builder.ins().call(is_true, &[value]);
    let status = builder.func.dfg.inst_results(call)[0];
    let failed = builder.ins().icmp_imm(IntCC::Equal, status, -1);
    let continue_block = builder.create_block();
    builder.ins().brif(failed, exception_exit, &[], continue_block, &[]);
    builder.switch_to_block(continue_block);
    builder.seal_block(continue_block);
    builder.ins().icmp_imm(IntCC::NotEqual, status, 0)
}

fn clif_block(block_map: &[(BlockId, ir::Block)], target: BlockId) -> Result<ir::Block, CodegenError> {
    block_map
        .iter()
        .find_map(|(id, block)| (*id == target).then_some(*block))
        .ok_or(CodegenError::Unsupported("branch target block"))
}
