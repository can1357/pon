//! Local, global, builtin, import, and closure-name Phase-B lowering family.

use std::mem;

use cranelift_codegen::ir::{self, InstBuilder};
use cranelift_frontend::FunctionBuilder;
use pon_ir::ir::{NameId, Value as IrValue};

use super::{
	CodegenError, HelperFuncRefs, LowerState, NameMap, call_pyobject_helper, load_local, offset_i32,
	store_local,
};

/// Lower a Phase-A local load.
pub(crate) fn lower_load_local(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	state: &LowerState,
	slot: u32,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let value = load_local(builder, state, slot)?;
	Ok(call_pyobject_helper(builder, helpers.load_local, &[value], ptr_ty, exception_exit))
}

/// Lower a Phase-A local store and return the stored boxed value.
pub(crate) fn lower_store_local(
	builder: &mut FunctionBuilder<'_>,
	state: &mut LowerState,
	slot: u32,
	value: IrValue,
) -> Result<ir::Value, CodegenError> {
	let value = state.value(value)?;
	store_local(builder, state, slot, value)?;
	Ok(value)
}

/// Lower a global load through `pon_load_global(name, feedback)`.
///
/// `feedback_cell` is the site's static J0.3 GlobalIC cell address, or `None`
/// for a NULL cell (helper skips IC consultation).
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_load_global(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	names: &NameMap,
	name: u32,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
	feedback_cell: Option<ir::Value>,
) -> Result<ir::Value, CodegenError> {
	let runtime_name = builder
		.ins()
		.iconst(ir::types::I32, i64::from(names.runtime_id(name)?));
	let feedback = feedback_cell.unwrap_or_else(|| builder.ins().iconst(ptr_ty, 0));
	Ok(call_pyobject_helper(
		builder,
		helpers.load_global,
		&[runtime_name, feedback],
		ptr_ty,
		exception_exit,
	))
}

/// Lower a class/module namespace `LoadName` through `pon_load_name`.
pub(crate) fn lower_load_name(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	names: &NameMap,
	name: u32,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let runtime_name = builder
		.ins()
		.iconst(ir::types::I32, i64::from(names.runtime_id(name)?));
	Ok(call_pyobject_helper(builder, helpers.load_name, &[runtime_name], ptr_ty, exception_exit))
}

/// Lower a Phase-A global store through `pon_store_global`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_store_global(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	names: &NameMap,
	state: &LowerState,
	name: u32,
	value: IrValue,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let runtime_name = builder
		.ins()
		.iconst(ir::types::I32, i64::from(names.runtime_id(name)?));
	let value = state.value(value)?;
	// PHASE-E: WriteBarrier
	Ok(call_pyobject_helper(
		builder,
		helpers.store_global,
		&[runtime_name, value],
		ptr_ty,
		exception_exit,
	))
}

/// Lower `ImportName` through the WS-IMPORT runtime helper.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_import_name_call(
	builder: &mut FunctionBuilder<'_>,
	import_name_helper: ir::FuncRef,
	names: &NameMap,
	name: u32,
	fromlist: &[NameId],
	level: u32,
	ptr_ty: ir::Type,
	ptr_bytes: usize,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let runtime_name = builder
		.ins()
		.iconst(ir::types::I32, i64::from(names.runtime_id(name)?));
	let fromlist_ptr = build_name_array(builder, names, fromlist, ptr_ty, ptr_bytes)?;
	let fromlist_len = builder.ins().iconst(ptr_ty, fromlist.len() as i64);
	let level = builder.ins().iconst(ir::types::I32, i64::from(level));
	Ok(call_pyobject_helper(
		builder,
		import_name_helper,
		&[runtime_name, fromlist_ptr, fromlist_len, level],
		ptr_ty,
		exception_exit,
	))
}

/// Lower `ImportFrom` through the WS-IMPORT runtime helper.
pub(crate) fn lower_import_from_call(
	builder: &mut FunctionBuilder<'_>,
	import_from_helper: ir::FuncRef,
	names: &NameMap,
	state: &LowerState,
	module: IrValue,
	name: u32,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let module = state.value(module)?;
	let runtime_name = builder
		.ins()
		.iconst(ir::types::I32, i64::from(names.runtime_id(name)?));
	Ok(call_pyobject_helper(
		builder,
		import_from_helper,
		&[module, runtime_name],
		ptr_ty,
		exception_exit,
	))
}

/// Lower `ImportStar` through the WS-IMPORT runtime helper.
pub(crate) fn lower_import_star_call(
	builder: &mut FunctionBuilder<'_>,
	import_star_helper: ir::FuncRef,
	state: &LowerState,
	module: IrValue,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let module = state.value(module)?;
	Ok(call_pyobject_helper(builder, import_star_helper, &[module], ptr_ty, exception_exit))
}

fn build_name_array(
	builder: &mut FunctionBuilder<'_>,
	names: &NameMap,
	source_names: &[NameId],
	ptr_ty: ir::Type,
	_ptr_bytes: usize,
) -> Result<ir::Value, CodegenError> {
	if source_names.is_empty() {
		return Ok(builder.ins().iconst(ptr_ty, 0));
	}

	let elem_bytes = mem::size_of::<u32>();
	let size = source_names
		.len()
		.checked_mul(elem_bytes)
		.ok_or(CodegenError::OffsetTooLarge { offset: usize::MAX })?;
	let slot = builder.create_sized_stack_slot(ir::StackSlotData {
		kind:        ir::StackSlotKind::ExplicitSlot,
		size:        size
			.try_into()
			.map_err(|_| CodegenError::OffsetTooLarge { offset: size })?,
		align_shift: elem_bytes.trailing_zeros() as u8,
		key:         None,
	});
	for (index, name) in source_names.iter().enumerate() {
		let runtime_name = builder
			.ins()
			.iconst(ir::types::I32, i64::from(names.runtime_id(name.0)?));
		builder
			.ins()
			.stack_store(runtime_name, slot, offset_i32(index * elem_bytes)?);
	}

	Ok(builder.ins().stack_addr(ptr_ty, slot, 0))
}

/// Lower local deletion to a checked unbind.
pub(crate) fn lower_delete_local(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	state: &mut LowerState,
	slot: u32,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let value = load_local(builder, state, slot)?;
	let result =
		call_pyobject_helper(builder, helpers.delete_local, &[value], ptr_ty, exception_exit);
	let unbound = builder.ins().iconst(ptr_ty, 0);
	store_local(builder, state, slot, unbound)?;
	Ok(result)
}

/// Lower global deletion through `pon_delete_global`.
pub(crate) fn lower_delete_global(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	names: &NameMap,
	name: u32,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let runtime_name = builder
		.ins()
		.iconst(ir::types::I32, i64::from(names.runtime_id(name)?));
	Ok(call_pyobject_helper(builder, helpers.delete_global, &[runtime_name], ptr_ty, exception_exit))
}

/// Lower a class/module namespace store through `pon_store_name`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_store_name(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	names: &NameMap,
	state: &LowerState,
	name: u32,
	value: IrValue,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let runtime_name = builder
		.ins()
		.iconst(ir::types::I32, i64::from(names.runtime_id(name)?));
	let value = state.value(value)?;
	Ok(call_pyobject_helper(
		builder,
		helpers.store_name,
		&[runtime_name, value],
		ptr_ty,
		exception_exit,
	))
}

/// Lower class/module namespace deletion through `pon_delete_name`.
pub(crate) fn lower_delete_name(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	names: &NameMap,
	name: u32,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let runtime_name = builder
		.ins()
		.iconst(ir::types::I32, i64::from(names.runtime_id(name)?));
	Ok(call_pyobject_helper(builder, helpers.delete_name, &[runtime_name], ptr_ty, exception_exit))
}

pub(crate) fn cell_object(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	state: &LowerState,
	cell: u32,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	if let Some(var) = state.cell(cell) {
		return Ok(builder.use_var(var));
	}
	// Cell ids form one per-function space: ids below the own-cell count are
	// `MakeCell` results (resolved above); the rest index the current
	// function's closure tuple.  All `MakeCell`s live in the entry prologue,
	// so the map is complete before any closure-cell use lowers.
	let closure_index = (cell as usize)
		.checked_sub(state.cells.len())
		.ok_or(CodegenError::ClosureCellUnderflow { cell, own_cells: state.cells.len() })?;
	let index = builder.ins().iconst(ptr_ty, closure_index as i64);
	Ok(call_pyobject_helper(builder, helpers.current_closure_cell, &[index], ptr_ty, exception_exit))
}

/// Lower closure-cell load.
pub(crate) fn lower_load_cell(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	state: &LowerState,
	cell: u32,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let cell = cell_object(builder, helpers, state, cell, ptr_ty, exception_exit)?;
	Ok(call_pyobject_helper(builder, helpers.cell_get, &[cell], ptr_ty, exception_exit))
}

/// Lower closure-cell store and return the stored boxed value.
pub(crate) fn lower_store_cell(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	state: &LowerState,
	cell: u32,
	value: IrValue,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let cell = cell_object(builder, helpers, state, cell, ptr_ty, exception_exit)?;
	let value = state.value(value)?;
	Ok(call_pyobject_helper(builder, helpers.cell_set, &[cell, value], ptr_ty, exception_exit))
}

/// Lower closure-cell delete.
pub(crate) fn lower_delete_cell(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	state: &LowerState,
	cell: u32,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let cell = cell_object(builder, helpers, state, cell, ptr_ty, exception_exit)?;
	Ok(call_pyobject_helper(builder, helpers.cell_delete, &[cell], ptr_ty, exception_exit))
}

/// Lower local-to-cell conversion.
pub(crate) fn lower_make_cell(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	state: &mut LowerState,
	local: u32,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let value = load_local(builder, state, local)?;
	let cell = call_pyobject_helper(builder, helpers.make_cell, &[value], ptr_ty, exception_exit);
	let cell_id = state.next_cell_id;
	state.next_cell_id += 1;
	// Generator bodies pre-declare every own-cell variable in the dispatch
	// block (primed from the frame spill slot); everything else declares the
	// variable at the defining `MakeCell`.
	let var = match state.cell(cell_id) {
		Some(var) => var,
		None => {
			let var = builder.declare_var(ptr_ty);
			state.define_cell(cell_id, var);
			var
		},
	};
	builder.def_var(var, cell);
	Ok(cell)
}

/// Lower closure object load.
pub(crate) fn lower_load_closure(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	state: &LowerState,
	cell: u32,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	cell_object(builder, helpers, state, cell, ptr_ty, exception_exit)
}

/// Lower a builtin load through the dedicated builtin helper ABI.
pub(crate) fn lower_load_builtin(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	names: &NameMap,
	name: u32,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let runtime_name = builder
		.ins()
		.iconst(ir::types::I32, i64::from(names.runtime_id(name)?));
	Ok(call_pyobject_helper(builder, helpers.load_builtin, &[runtime_name], ptr_ty, exception_exit))
}

/// Lower annotations setup through `pon_setup_annotations`.
pub(crate) fn lower_setup_annotations(
	builder: &mut FunctionBuilder<'_>,
	helper: ir::FuncRef,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	Ok(call_pyobject_helper(builder, helper, &[], ptr_ty, exception_exit))
}

pub(crate) fn lower_load_build_class(
	builder: &mut FunctionBuilder<'_>,
	helper: ir::FuncRef,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	Ok(call_pyobject_helper(builder, helper, &[], ptr_ty, exception_exit))
}
