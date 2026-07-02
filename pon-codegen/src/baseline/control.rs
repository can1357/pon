//! Control-flow and singleton lowering family.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{self, AbiParam, FuncRef, InstBuilder, StackSlotData, StackSlotKind, TrapCode};
use cranelift_frontend::FunctionBuilder;
use pon_ir::ir::{BlockId, Function, Terminator};

use super::{CodegenError, HelperFuncRefs, LowerState, call_pyobject_helper, offset_i32, osr_live_values};
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

/// Lower a basic-block terminator.
pub(crate) fn lower_terminator(
    builder: &mut FunctionBuilder<'_>,
    state: &LowerState,
    helpers: &HelperFuncRefs,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
    term: &Terminator,
) -> Result<(), CodegenError> {
    match term {
        Terminator::Return(value) => {
            if state.gen_ctx.is_some() {
                return super::r#gen::lower_gen_return(builder, state, helpers, *value);
            }
            let value = state.value(*value)?;
            builder.ins().return_(&[value]);
            Ok(())
        }
        Terminator::RaiseTerm => {
            lower_raise_term(builder, helpers, ptr_ty, exception_exit);
            Ok(())
        }
        Terminator::Suspend { state: suspend_state, val, .. } => {
            super::r#gen::lower_suspend(builder, state, helpers, ptr_ty, *suspend_state, *val)
        }
        Terminator::Jump(_)
        | Terminator::Branch { .. }
        | Terminator::CondBranch { .. }
        | Terminator::ForLoop { .. }
        | Terminator::Unreachable => Err(CodegenError::Unsupported("non-return terminator")),
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
    function: &Function,
    current_block: BlockId,
    term: &Terminator,
) -> Result<(), CodegenError> {
    match term {
        Terminator::Return(value) => {
            if state.gen_ctx.is_some() {
                return super::r#gen::lower_gen_return(builder, state, helpers, *value);
            }
            let value = state.value(*value)?;
            builder.ins().return_(&[value]);
            Ok(())
        }
        Terminator::Jump(target) => {
            emit_loop_backedge_safepoint(builder, state, helpers, ptr_ty, current_block, *target, function)?;
            let target = clif_block(block_map, *target)?;
            builder.ins().jump(target, &[]);
            Ok(())
        }
        Terminator::Branch {
            cond,
            then_blk,
            else_blk,
        } => {
            emit_conditional_backedge_safepoint(builder, state, helpers, ptr_ty, current_block, *then_blk, *else_blk, function)?;
            lower_conditional_branch(builder, state, helpers.is_true, exception_exit, block_map, *cond, *then_blk, *else_blk)
        }
        Terminator::CondBranch { cond, then_, else_ } => {
            emit_conditional_backedge_safepoint(builder, state, helpers, ptr_ty, current_block, *then_, *else_, function)?;
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
        Terminator::RaiseTerm => {
            lower_raise_term(builder, helpers, ptr_ty, exception_exit);
            Ok(())
        }
        Terminator::Suspend { state: suspend_state, val, .. } => {
            super::r#gen::lower_suspend(builder, state, helpers, ptr_ty, *suspend_state, *val)
        }
        Terminator::Unreachable => Err(CodegenError::Unsupported("unreachable terminator")),
        _ => Err(CodegenError::Unsupported("unknown future terminator")),
    }
}

fn lower_raise_term(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) {
    // `Raise`/`Reraise` instructions install the pending Python exception and
    // return NULL.  The terminator owns the control-flow exit: route the normal
    // NULL sentinel to the shared exception block, and trap only if the helper
    // unexpectedly reports a non-NULL object.
    let _ = call_pyobject_helper(builder, helpers.reraise, &[], ptr_ty, exception_exit);
    builder.ins().trap(TrapCode::unwrap_user(1));
}

fn emit_loop_backedge_safepoint(
    builder: &mut FunctionBuilder<'_>,
    state: &LowerState,
    helpers: &HelperFuncRefs,
    ptr_ty: ir::Type,
    current_block: BlockId,
    target: BlockId,
    function: &Function,
) -> Result<(), CodegenError> {
    if target.0 <= current_block.0 {
        emit_osr_poll_and_transfer(builder, state, helpers, ptr_ty, target, function)?;
    }
    Ok(())
}

fn emit_conditional_backedge_safepoint(
    builder: &mut FunctionBuilder<'_>,
    state: &LowerState,
    helpers: &HelperFuncRefs,
    ptr_ty: ir::Type,
    current_block: BlockId,
    true_target: BlockId,
    false_target: BlockId,
    function: &Function,
) -> Result<(), CodegenError> {
    let backedge_target = if true_target.0 <= current_block.0 {
        Some(true_target)
    } else if false_target.0 <= current_block.0 {
        Some(false_target)
    } else {
        None
    };
    if let Some(target) = backedge_target {
        emit_osr_poll_and_transfer(builder, state, helpers, ptr_ty, target, function)?;
    }
    Ok(())
}

fn emit_osr_poll_and_transfer(
    builder: &mut FunctionBuilder<'_>,
    state: &LowerState,
    helpers: &HelperFuncRefs,
    ptr_ty: ir::Type,
    target: BlockId,
    function: &Function,
) -> Result<(), CodegenError> {
    #[cfg(feature = "free-threading")]
    emit_safepoint_poll(builder, helpers);

    let header = builder.ins().iconst(ir::types::I32, i64::from(target.0));
    let call = builder.ins().call(helpers.osr_poll, &[header]);
    let osr_entry = builder.func.dfg.inst_results(call)[0];
    let live_values = osr_live_values(function, target);
    let live_count = function.n_locals.saturating_add(live_values.len());
    if live_count > pon_jit_osr_max_live() {
        return Ok(());
    }

    let transfer = builder.create_block();
    let cont = builder.create_block();
    builder.set_cold_block(transfer);
    let has_entry = builder.ins().icmp_imm(IntCC::NotEqual, osr_entry, 0);
    builder.ins().brif(has_entry, transfer, &[], cont, &[]);

    builder.switch_to_block(transfer);
    let ptr_bytes = ptr_ty.bytes() as usize;
    let size = 8usize
        .checked_add(pon_jit_osr_max_live().saturating_mul(ptr_bytes))
        .ok_or(CodegenError::OffsetTooLarge { offset: usize::MAX })?;
    let slot = builder.create_sized_stack_slot(StackSlotData {
        kind: StackSlotKind::ExplicitSlot,
        size: size.try_into().map_err(|_| CodegenError::OffsetTooLarge { offset: size })?,
        align_shift: 3,
        key: None,
    });
    builder.ins().stack_store(header, slot, 0);
    let count = builder.ins().iconst(ir::types::I32, live_count as i64);
    builder.ins().stack_store(count, slot, 4);
    let null = builder.ins().iconst(ptr_ty, 0);
    for index in 0..pon_jit_osr_max_live() {
        builder.ins().stack_store(null, slot, offset_i32(8 + index * ptr_bytes)?);
    }
    for local in 0..function.n_locals {
        let value = if state.local_defined.get(local).copied().unwrap_or(false) {
            builder.use_var(state.locals[local])
        } else {
            null
        };
        builder.ins().stack_store(value, slot, offset_i32(8 + local * ptr_bytes)?);
    }
    for (index, value) in live_values.iter().enumerate() {
        let boxed = state.value(*value)?;
        builder
            .ins()
            .stack_store(boxed, slot, offset_i32(8 + (function.n_locals + index) * ptr_bytes)?);
    }
    let buffer = builder.ins().stack_addr(ptr_ty, slot, 0);
    let mut sig = ir::Signature::new(builder.func.signature.call_conv);
    sig.params.push(AbiParam::new(ptr_ty));
    sig.returns.push(AbiParam::new(ptr_ty));
    let sig = builder.import_signature(sig);
    let ret = builder.ins().call_indirect(sig, osr_entry, &[buffer]);
    let result = builder.func.dfg.inst_results(ret)[0];
    builder.ins().return_(&[result]);
    builder.seal_block(transfer);

    builder.switch_to_block(cont);
    builder.seal_block(cont);
    Ok(())
}

const fn pon_jit_osr_max_live() -> usize {
    16
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
