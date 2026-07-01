//! Comparison and truth-test Phase-B lowering family.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{self, FuncRef, InstBuilder};
use cranelift_frontend::FunctionBuilder;
use pon_ir::ir::{CmpOp, Value as IrValue};

use super::{CodegenError, LowerState, call_pyobject_helper};

/// Reserve rich-comparison lowering.
#[allow(dead_code)]
pub(crate) fn lower_compare() -> Result<ir::Value, CodegenError> {
    Err(CodegenError::Unsupported("Compare"))
}

/// Reserve containment-test lowering.
pub(crate) fn lower_contains() -> Result<ir::Value, CodegenError> {
    Err(CodegenError::Unsupported("Contains"))
}

/// Reserve identity-test lowering.
#[allow(dead_code)]
pub(crate) fn lower_is() -> Result<ir::Value, CodegenError> {
    Err(CodegenError::Unsupported("Is"))
}

/// Reserve truth-test lowering.
#[allow(dead_code)]
pub(crate) fn lower_bool_test() -> Result<ir::Value, CodegenError> {
    Err(CodegenError::Unsupported("BoolTest"))
}

/// Reserve logical-not lowering.
#[allow(dead_code)]
pub(crate) fn lower_not() -> Result<ir::Value, CodegenError> {
    Err(CodegenError::Unsupported("Not"))
}

pub(crate) fn lower_compare_op(
    builder: &mut FunctionBuilder<'_>,
    rich_compare: FuncRef,
    state: &LowerState,
    op: CmpOp,
    lhs: IrValue,
    rhs: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let op = builder.ins().iconst(ir::types::I8, rich_compare_selector(op)?);
    let lhs = state.value(lhs)?;
    let rhs = state.value(rhs)?;
    let feedback = builder.ins().iconst(ptr_ty, 0);
    Ok(call_pyobject_helper(builder, rich_compare, &[op, lhs, rhs, feedback], ptr_ty, exception_exit))
}

pub(crate) fn lower_is_op(
    builder: &mut FunctionBuilder<'_>,
    const_int: FuncRef,
    state: &LowerState,
    lhs: IrValue,
    rhs: IrValue,
    negate: bool,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let lhs = state.value(lhs)?;
    let rhs = state.value(rhs)?;
    let condition = builder.ins().icmp(if negate { IntCC::NotEqual } else { IntCC::Equal }, lhs, rhs);
    Ok(box_bool(builder, const_int, condition, ptr_ty, exception_exit))
}

pub(crate) fn lower_bool_test_op(
    builder: &mut FunctionBuilder<'_>,
    is_true: FuncRef,
    const_int: FuncRef,
    state: &LowerState,
    val: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let val = state.value(val)?;
    let truth = call_is_true(builder, is_true, val, exception_exit);
    let condition = builder.ins().icmp_imm(IntCC::NotEqual, truth, 0);
    Ok(box_bool(builder, const_int, condition, ptr_ty, exception_exit))
}

pub(crate) fn lower_not_op(
    builder: &mut FunctionBuilder<'_>,
    is_true: FuncRef,
    const_int: FuncRef,
    state: &LowerState,
    val: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let val = state.value(val)?;
    let truth = call_is_true(builder, is_true, val, exception_exit);
    let condition = builder.ins().icmp_imm(IntCC::Equal, truth, 0);
    Ok(box_bool(builder, const_int, condition, ptr_ty, exception_exit))
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
    status
}

fn box_bool(
    builder: &mut FunctionBuilder<'_>,
    const_int: FuncRef,
    condition: ir::Value,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> ir::Value {
    let one = builder.ins().iconst(ir::types::I64, 1);
    let zero = builder.ins().iconst(ir::types::I64, 0);
    let int_value = builder.ins().select(condition, one, zero);
    call_pyobject_helper(builder, const_int, &[int_value], ptr_ty, exception_exit)
}

fn rich_compare_selector(op: CmpOp) -> Result<i64, CodegenError> {
    Ok(match op {
        CmpOp::Lt => 0,
        CmpOp::Le => 1,
        CmpOp::Eq => 2,
        CmpOp::Ne => 3,
        CmpOp::Gt => 4,
        CmpOp::Ge => 5,
        _ => return Err(CodegenError::Unsupported("unknown future comparison op")),
    })
}
