//! Attribute and method lookup Phase-B lowering family.

use cranelift_codegen::ir::{self, InstBuilder};
use cranelift_frontend::FunctionBuilder;
use pon_ir::ir::Value as IrValue;

use super::{CodegenError, HelperFuncRefs, LowerState, NameMap, call_pyobject_helper};

/// Lower `LoadAttr` through the runtime descriptor-aware attribute helper.
///
/// `feedback_cell` is the site's static J0.3 feedback-cell address (attr
/// kind), or `None` for a NULL cell (helper skips IC consultation).
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_load_attr(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	names: &NameMap,
	state: &LowerState,
	obj: IrValue,
	name: u32,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
	feedback_cell: Option<ir::Value>,
) -> Result<ir::Value, CodegenError> {
	let obj = state.value(obj)?;
	let runtime_name = builder
		.ins()
		.iconst(ir::types::I32, i64::from(names.runtime_id(name)?));
	let feedback = feedback_cell.unwrap_or_else(|| builder.ins().iconst(ptr_ty, 0));
	Ok(call_pyobject_helper(
		builder,
		helpers.get_attr,
		&[obj, runtime_name, feedback],
		ptr_ty,
		exception_exit,
	))
}

/// Lower `StoreAttr` through the runtime setter helper and return the stored
/// value.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_store_attr(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	names: &NameMap,
	state: &LowerState,
	obj: IrValue,
	name: u32,
	val: IrValue,
	_ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let obj = state.value(obj)?;
	let val = state.value(val)?;
	let runtime_name = builder
		.ins()
		.iconst(ir::types::I32, i64::from(names.runtime_id(name)?));
	let call = builder
		.ins()
		.call(helpers.set_attr, &[obj, runtime_name, val]);
	let status = builder.func.dfg.inst_results(call)[0];
	emit_status_check(builder, status, exception_exit);
	Ok(val)
}

/// Lower `DeleteAttr` through the runtime deleter helper and return `None`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_delete_attr(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	names: &NameMap,
	state: &LowerState,
	obj: IrValue,
	name: u32,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let obj = state.value(obj)?;
	let runtime_name = builder
		.ins()
		.iconst(ir::types::I32, i64::from(names.runtime_id(name)?));
	let call = builder.ins().call(helpers.del_attr, &[obj, runtime_name]);
	let status = builder.func.dfg.inst_results(call)[0];
	emit_status_check(builder, status, exception_exit);
	Ok(call_pyobject_helper(builder, helpers.none, &[], ptr_ty, exception_exit))
}

/// Tier-0 `LoadMethod` is descriptor-aware `LoadAttr`; later tiers can split a
/// method/receiver pair without changing the runtime semantics.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_load_method(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	names: &NameMap,
	state: &LowerState,
	obj: IrValue,
	name: u32,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
	feedback_cell: Option<ir::Value>,
) -> Result<ir::Value, CodegenError> {
	lower_load_attr(builder, helpers, names, state, obj, name, ptr_ty, exception_exit, feedback_cell)
}

fn emit_status_check(
	builder: &mut FunctionBuilder<'_>,
	status: ir::Value,
	exception_exit: ir::Block,
) {
	let ok = builder
		.ins()
		.icmp_imm(ir::condcodes::IntCC::SignedGreaterThanOrEqual, status, 0);
	let continue_block = builder.create_block();
	builder
		.ins()
		.brif(ok, continue_block, &[], exception_exit, &[]);
	builder.seal_block(continue_block);
	builder.switch_to_block(continue_block);
}
