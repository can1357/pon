//! Exception-state Phase-B lowering family.

use cranelift_codegen::ir::{self, InstBuilder};
use cranelift_frontend::FunctionBuilder;
use pon_ir::ir::{Value as IrValue, ValueId};

use super::{
	CodegenError, HelperFuncRefs, LowerState, build_call_argv, call_pyobject_helper,
	call_pyobject_helper_consuming,
};

fn null_ptr(builder: &mut FunctionBuilder<'_>, ptr_ty: ir::Type) -> ir::Value {
	builder.ins().iconst(ptr_ty, 0)
}

// `raise` helpers intentionally return NULL after installing the pending Python
// exception. Do not route that NULL through the generic helper error exit; the
// surrounding IR terminator decides whether control enters a handler or
// escapes.
fn call_pyobject_helper_without_null_exit(
	builder: &mut FunctionBuilder<'_>,
	helper: ir::FuncRef,
	args: &[ir::Value],
) -> ir::Value {
	let call = builder.ins().call(helper, args);
	builder.func.dfg.inst_results(call)[0]
}

/// Lower `raise exc from cause` or bare `raise`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_raise(
	builder: &mut FunctionBuilder<'_>,
	raise: ir::FuncRef,
	reraise: ir::FuncRef,
	state: &LowerState,
	exc: Option<ValueId>,
	cause: Option<ValueId>,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	match exc {
		Some(exc) => {
			let exc = state.value(exc)?;
			let cause = match cause {
				Some(cause) => state.value(cause)?,
				None => null_ptr(builder, ptr_ty),
			};
			Ok(call_pyobject_helper_without_null_exit(builder, raise, &[exc, cause]))
		},
		None => lower_reraise(builder, reraise, ptr_ty, exception_exit),
	}
}

/// Lower active-exception re-raise.
pub(crate) fn lower_reraise(
	builder: &mut FunctionBuilder<'_>,
	reraise: ir::FuncRef,
	_ptr_ty: ir::Type,
	_exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	Ok(call_pyobject_helper_without_null_exit(builder, reraise, &[]))
}

/// Lower exception-handler chain push.
pub(crate) fn lower_push_exc_info(
	builder: &mut FunctionBuilder<'_>,
	push_exc_info: ir::FuncRef,
	target: u32,
	stack_depth: u32,
	kind: u8,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let target = builder.ins().iconst(ir::types::I32, i64::from(target));
	let stack_depth = builder.ins().iconst(ir::types::I32, i64::from(stack_depth));
	let kind = builder.ins().iconst(ir::types::I8, i64::from(kind));
	Ok(call_pyobject_helper(
		builder,
		push_exc_info,
		&[target, stack_depth, kind],
		ptr_ty,
		exception_exit,
	))
}

/// Lower exception-handler chain pop.
pub(crate) fn lower_pop_exc_info(
	builder: &mut FunctionBuilder<'_>,
	pop_exc_info: ir::FuncRef,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	Ok(call_pyobject_helper(builder, pop_exc_info, &[], ptr_ty, exception_exit))
}

/// Lower active-exception match test.  The runtime returns the current
/// exception on match and `None` on miss, preserving NULL as the error
/// sentinel.
pub(crate) fn lower_match_exc(
	builder: &mut FunctionBuilder<'_>,
	match_exc: ir::FuncRef,
	state: &LowerState,
	exc_type: ValueId,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let exc_type = state.value(exc_type)?;
	Ok(call_pyobject_helper(builder, match_exc, &[exc_type], ptr_ty, exception_exit))
}

/// Lower representative `except*` split.  The runtime returns a matched group
/// or `None` when the active exception is not an exception group match.
pub(crate) fn lower_check_exc_star(
	builder: &mut FunctionBuilder<'_>,
	check_exc_star: ir::FuncRef,
	state: &LowerState,
	exc_types: ValueId,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let exc_types = state.value(exc_types)?;
	Ok(call_pyobject_helper(builder, check_exc_star, &[exc_types], ptr_ty, exception_exit))
}

pub(crate) fn lower_exc_star_enter(
	builder: &mut FunctionBuilder<'_>,
	exc_star_enter: ir::FuncRef,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	Ok(call_pyobject_helper(builder, exc_star_enter, &[], ptr_ty, exception_exit))
}

pub(crate) fn lower_exc_star_match(
	builder: &mut FunctionBuilder<'_>,
	exc_star_match: ir::FuncRef,
	state: &LowerState,
	exc_types: ValueId,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let exc_types = state.value(exc_types)?;
	Ok(call_pyobject_helper(builder, exc_star_match, &[exc_types], ptr_ty, exception_exit))
}

pub(crate) fn lower_exc_star_body_ok(
	builder: &mut FunctionBuilder<'_>,
	exc_star_body_ok: ir::FuncRef,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	Ok(call_pyobject_helper(builder, exc_star_body_ok, &[], ptr_ty, exception_exit))
}

pub(crate) fn lower_exc_star_body_raised(
	builder: &mut FunctionBuilder<'_>,
	exc_star_body_raised: ir::FuncRef,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	Ok(call_pyobject_helper(builder, exc_star_body_raised, &[], ptr_ty, exception_exit))
}

pub(crate) fn lower_exc_star_finish(
	builder: &mut FunctionBuilder<'_>,
	exc_star_finish: ir::FuncRef,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	Ok(call_pyobject_helper(builder, exc_star_finish, &[], ptr_ty, exception_exit))
}

/// Lower active-exception object load.  The runtime returns `None` when there
/// is no object-safe current exception.
pub(crate) fn lower_get_current_exc(
	builder: &mut FunctionBuilder<'_>,
	get_current_exc: ir::FuncRef,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	Ok(call_pyobject_helper(builder, get_current_exc, &[], ptr_ty, exception_exit))
}

/// Lower exception-group construction from boxed exception values.
pub(crate) fn lower_build_exc_group(
	builder: &mut FunctionBuilder<'_>,
	build_exc_group: ir::FuncRef,
	helpers: &HelperFuncRefs,
	state: &LowerState,
	excs: &[IrValue],
	ptr_ty: ir::Type,
	ptr_bytes: usize,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let argv = build_call_argv(builder, helpers, state, excs, ptr_ty, ptr_bytes)?;
	let argc = builder.ins().iconst(ptr_ty, excs.len() as i64);
	call_pyobject_helper_consuming(
		builder,
		build_exc_group,
		&[argv.addr, argc],
		&[&argv],
		ptr_ty,
		ptr_bytes,
		exception_exit,
	)
}
