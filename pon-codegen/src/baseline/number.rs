//! Numeric Phase-B lowering family.
//!
//! Numeric runtime helpers now cover the built-in numeric tower.  Baseline
//! lowering routes supported binary operators through the selector-bearing
//! numeric dispatch helper so tier-0 and tier-1 kernels share the same ABI.

use cranelift_codegen::ir::{self, InstBuilder};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Module;
use pon_ir::ir::{BinOp, UnOp, Value as IrValue};
use pon_runtime::abstract_op::{
	BINARY_ADD, BINARY_AND, BINARY_DIV, BINARY_FLOORDIV, BINARY_LSHIFT, BINARY_MATMUL, BINARY_MOD,
	BINARY_MUL, BINARY_OR, BINARY_POW, BINARY_RSHIFT, BINARY_SUB, BINARY_XOR,
};

use super::{CodegenError, HelperFuncRefs, LowerState, call_pyobject_helper, declare_string_data};

/// Lower a Phase-A integer literal, directly materializing tagged immediates
/// when the value fits the runtime tag range.
pub(crate) fn lower_const_int(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	value: i64,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	if ptr_ty.bytes() == 8
		&& (pon_runtime::tag::SMALL_INT_MIN..=pon_runtime::tag::SMALL_INT_MAX).contains(&value)
	{
		let bits = ((value as u64) << 1) | pon_runtime::tag::TAG_INT_BIT as u64;
		return Ok(builder.ins().iconst(ptr_ty, bits as i64));
	}

	let arg = builder.ins().iconst(ir::types::I64, value);
	Ok(call_pyobject_helper(builder, helpers.const_int, &[arg], ptr_ty, exception_exit))
}

/// Lower an oversized integer-literal token through `pon_const_bigint`,
/// materializing the digit text in the module constant pool.
pub(crate) fn lower_const_bigint<M: Module>(
	module: &mut M,
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	value: &str,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let data_ptr = declare_string_data(module, builder, value, ptr_ty)?;
	let len = builder.ins().iconst(ptr_ty, value.len() as i64);
	Ok(call_pyobject_helper(builder, helpers.const_bigint, &[data_ptr, len], ptr_ty, exception_exit))
}

/// Lower a boolean literal through `pon_const_bool`.
pub(crate) fn lower_const_bool(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	value: bool,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let arg = builder.ins().iconst(ir::types::I32, i64::from(value));
	Ok(call_pyobject_helper(builder, helpers.const_bool, &[arg], ptr_ty, exception_exit))
}

/// Lower a Phase-B float literal through `pon_const_float`.
pub(crate) fn lower_const_float(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	value: f64,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let arg = builder.ins().f64const(value);
	Ok(call_pyobject_helper(builder, helpers.const_float, &[arg], ptr_ty, exception_exit))
}

/// Lower a Phase-B complex literal through `pon_const_complex`.
pub(crate) fn lower_const_complex(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	real: f64,
	imag: f64,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let real = builder.ins().f64const(real);
	let imag = builder.ins().f64const(imag);
	Ok(call_pyobject_helper(builder, helpers.const_complex, &[real, imag], ptr_ty, exception_exit))
}

/// Lower a binary numeric operation through selector-bearing numeric dispatch.
pub(crate) fn lower_binary_op(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	state: &LowerState,
	op: BinOp,
	lhs: IrValue,
	rhs: IrValue,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let selector = binary_selector(op)?;
	let selector = builder.ins().iconst(ir::types::I8, i64::from(selector));
	let lhs = state.value(lhs)?;
	let rhs = state.value(rhs)?;
	let feedback = builder.ins().iconst(ptr_ty, 0);
	Ok(call_pyobject_helper(
		builder,
		helpers.number_binary,
		&[selector, lhs, rhs, feedback],
		ptr_ty,
		exception_exit,
	))
}

pub(crate) fn lower_unary_op(
	builder: &mut FunctionBuilder<'_>,
	helper: ir::FuncRef,
	state: &LowerState,
	op: UnOp,
	operand: IrValue,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let selector = match op {
		UnOp::Neg => 0,
		UnOp::Pos => 1,
		UnOp::Invert => 2,
		_ => return Err(CodegenError::Unsupported("unary op")),
	};
	let selector = builder.ins().iconst(ir::types::I8, selector);
	let operand = state.value(operand)?;
	let feedback = builder.ins().iconst(ptr_ty, 0);
	Ok(call_pyobject_helper(builder, helper, &[selector, operand, feedback], ptr_ty, exception_exit))
}

pub(crate) fn lower_inplace_op(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	state: &LowerState,
	op: BinOp,
	lhs: IrValue,
	rhs: IrValue,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let selector = builder
		.ins()
		.iconst(ir::types::I8, i64::from(binary_selector(op)?));
	let lhs = state.value(lhs)?;
	let rhs = state.value(rhs)?;
	let feedback = builder.ins().iconst(ptr_ty, 0);
	Ok(call_pyobject_helper(
		builder,
		helpers.number_inplace,
		&[selector, lhs, rhs, feedback],
		ptr_ty,
		exception_exit,
	))
}

fn binary_selector(op: BinOp) -> Result<u8, CodegenError> {
	Ok(match op {
		BinOp::Add => BINARY_ADD,
		BinOp::Sub => BINARY_SUB,
		BinOp::Mul => BINARY_MUL,
		BinOp::MatMul => BINARY_MATMUL,
		BinOp::Div => BINARY_DIV,
		BinOp::FloorDiv => BINARY_FLOORDIV,
		BinOp::Mod => BINARY_MOD,
		BinOp::Pow => BINARY_POW,
		BinOp::LShift => BINARY_LSHIFT,
		BinOp::RShift => BINARY_RSHIFT,
		BinOp::And => BINARY_AND,
		BinOp::Or => BINARY_OR,
		BinOp::Xor => BINARY_XOR,
		_ => return Err(CodegenError::Unsupported("binary op")),
	})
}
