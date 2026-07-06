//! Iterator, async-iterator, generator, and coroutine lowering family.
//!
//! This module owns the pin J0.1 tier-0 generator codegen: a generator
//! function compiles to TWO Cranelift functions —
//!
//! 1. a **stub** at the function's declared `FuncId` with the normal `(argv,
//!    argc) -> obj` ABI that allocates a [`GenFrame`], stores the bound
//!    arguments into the frame's parameter slots, and wraps the frame in a
//!    generator/coroutine object without running any body code; and
//! 2. an anonymous **body** with the single-argument resume ABI `(frame) ->
//!    obj` (`GenResumeBodyFn`): the entry block loads `resume_state`, stores
//!    `RESUME_RUNNING`, reloads every local from its frame slot, and
//!    `br_table`-dispatches over the dense suspend states to IR block 0
//!    (`RESUME_START`) or a resume block.  `Terminator::Suspend` spills all
//!    locals, stores its state number, and returns the yielded value;
//!    `Terminator::Return` routes through `pon_gen_finish`; the function-level
//!    exception exit routes through `pon_gen_unwind` (PEP 479 + slot zeroing
//!    live in the runtime helpers).
//!
//! Frame layout facts (`GEN_FRAME_HEADER_SIZE`, `resume_state` offset, the
//! `RESUME_*` sentinels) are imported from `pon_runtime::types::generator` so
//! compiled code and the allocator can never drift.

use cranelift_codegen::{
	Context,
	ir::{self, AbiParam, InstBuilder, JumpTableData, MemFlagsData},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Module};
use pon_ir::ir::{Function, InstKind, Terminator, Value as IrValue};
use pon_runtime::types::generator::{GEN_FRAME_HEADER_SIZE, GenFrame, RESUME_RUNNING};

use super::{
	CodegenError, HelperFuncRefs, LowerState, call_pyobject_helper, declare_feedback_cells,
	declare_helper_refs, emit_safepoint_poll, lower_function_blocks, offset_i32, parameter_bindings,
	store_local,
};
use crate::helpers::HelperRefs;

/// Byte offset of `GenFrame.resume_state` (pin J0.1 §1: 16).
const RESUME_STATE_OFFSET: i32 = core::mem::offset_of!(GenFrame, resume_state) as i32;

/// Generator-body lowering context threaded through [`LowerState`].
///
/// Present exactly when the function being lowered is a resumable generator
/// body; carries the frame pointer (the body's only parameter) that suspend
/// spills, payload consumes, and the finish/unwind epilogues address.
#[derive(Clone, Copy)]
pub(crate) struct GenBodyCtx {
	/// The `*mut GenFrame` argument of the compiled body.
	pub(crate) frame: ir::Value,
}

/// `pon_make_generator` kind flag for `function`; the same byte selects the
/// PEP 479/525 wording family in `pon_gen_unwind`.
fn generator_kind_flag(function: &Function) -> u8 {
	use pon_runtime::types::generator::GeneratorKind;
	if function.is_async_generator {
		GeneratorKind::AsyncGenerator.as_u8()
	} else if function.is_coroutine {
		GeneratorKind::Coroutine.as_u8()
	} else {
		GeneratorKind::Generator.as_u8()
	}
}

/// Byte offset of spill slot `slot` inside a [`GenFrame`] allocation.
fn frame_slot_offset(slot: usize, ptr_bytes: usize) -> Result<i32, CodegenError> {
	offset_i32(GEN_FRAME_HEADER_SIZE + slot * ptr_bytes)
}

/// Number of own closure cells (`MakeCell` results) in `function`.
///
/// Own cells get frame spill slots after the `n_locals` local slots: the cell
/// object must survive suspension (dominance for resume paths + GC
/// reachability from the suspended frame), exactly like locals.
fn own_cell_count(function: &Function) -> usize {
	function
		.blocks
		.iter()
		.flat_map(|block| &block.insts)
		.filter(|inst| matches!(inst.kind, InstKind::MakeCell(_)))
		.count()
}

/// Store `value` into frame spill slot `slot` (+ FT write barrier, mirroring
/// every heap store emitted by baseline codegen).
fn store_frame_slot(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	frame: ir::Value,
	slot: usize,
	value: ir::Value,
	ptr_ty: ir::Type,
	ptr_bytes: usize,
) -> Result<(), CodegenError> {
	let offset = frame_slot_offset(slot, ptr_bytes)?;
	builder
		.ins()
		.store(MemFlagsData::new(), value, frame, offset);

	#[cfg(feature = "free-threading")]
	{
		let slot_addr = builder.ins().iadd_imm(frame, i64::from(offset));
		builder
			.ins()
			.call(helpers.gc_write_barrier, &[slot_addr, value]);
	}

	#[cfg(not(feature = "free-threading"))]
	{
		let _ = (helpers, ptr_ty);
	}
	Ok(())
}

/// Compile one generator/coroutine IR function as the pin J0.1 two-function
/// scheme: define the anonymous resume body, then build the allocation stub
/// into `ctx` (the caller defines it at the declared `FuncId`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn compile_generator_function<M: Module>(
	module: &mut M,
	helpers: &HelperRefs,
	func_ids: &[FuncId],
	functions: &[Function],
	names: &super::NameMap,
	ir_function: &Function,
	entry_arg_count: usize,
	ctx: &mut Context,
	fctx: &mut FunctionBuilderContext,
) -> Result<(), CodegenError> {
	let ptr_ty = module.target_config().pointer_type();

	// (1) Anonymous resume body: `(frame) -> obj`.
	let mut body_sig = module.make_signature();
	body_sig.params.push(AbiParam::new(ptr_ty));
	body_sig.returns.push(AbiParam::new(ptr_ty));
	let body_id = module.declare_anonymous_function(&body_sig)?;

	let mut body_ctx = module.make_context();
	body_ctx.func.signature = body_sig;
	compile_generator_body(
		module,
		helpers,
		func_ids,
		functions,
		names,
		ir_function,
		&mut body_ctx,
		fctx,
	)?;
	module.define_function(body_id, &mut body_ctx)?;

	// (2) Stub at the declared FuncId: `(argv, argc) -> obj`.
	compile_generator_stub(module, helpers, ir_function, entry_arg_count, body_id, ctx, fctx)
}

/// Build the call-time stub: allocate the frame, store bound arguments into
/// their parameter slots, wrap frame + body address in a generator object
/// (pin J0.1 §4.0 — no user code runs).
fn compile_generator_stub<M: Module>(
	module: &mut M,
	helpers: &HelperRefs,
	ir_function: &Function,
	entry_arg_count: usize,
	body_id: FuncId,
	ctx: &mut Context,
	fctx: &mut FunctionBuilderContext,
) -> Result<(), CodegenError> {
	module.clear_context(ctx);
	let ptr_ty = module.target_config().pointer_type();
	let ptr_bytes = ptr_ty.bytes() as usize;

	ctx.func.signature.params.push(AbiParam::new(ptr_ty));
	ctx.func.signature.params.push(AbiParam::new(ptr_ty));
	ctx.func.signature.returns.push(AbiParam::new(ptr_ty));

	let helper_refs = declare_helper_refs(module, helpers, &mut ctx.func);
	let body_ref = module.declare_func_in_func(body_id, &mut ctx.func);

	let mut builder = FunctionBuilder::new(&mut ctx.func, fctx);
	let entry = builder.create_block();
	let exception_exit = builder.create_block();
	builder.set_cold_block(exception_exit);
	builder.append_block_params_for_function_params(entry);
	builder.switch_to_block(entry);
	builder.seal_block(entry);

	let argv = builder.func.dfg.block_params(entry)[0];
	emit_safepoint_poll(&mut builder, &helper_refs);

	// frame = pon_gen_frame_alloc(n_locals + own_cells): every local owns a
	// spill slot (slot k = local k) and every own closure cell owns one after
	// them (slot n_locals + c = cell c), so the body's whole-frame
	// spill/reload covers all live state (pin J0.1 §1).
	let total_slots = ir_function.n_locals + own_cell_count(ir_function);
	let slot_count = u32::try_from(total_slots)
		.map_err(|_| CodegenError::OffsetTooLarge { offset: total_slots })?;
	let slot_count_value = builder.ins().iconst(ir::types::I32, i64::from(slot_count));
	let frame = call_pyobject_helper(
		&mut builder,
		helper_refs.gen_frame_alloc,
		&[slot_count_value],
		ptr_ty,
		exception_exit,
	);

	// Store bound arguments into their parameter slots — the same
	// argv -> local permutation as initialize_parameter_locals.
	for (argv_slot, local_slot) in parameter_bindings(ir_function, entry_arg_count) {
		if local_slot >= ir_function.n_locals {
			return Err(CodegenError::LocalOutOfRange {
				slot:     local_slot as u32,
				n_locals: ir_function.n_locals,
			});
		}
		let offset = offset_i32(argv_slot * ptr_bytes)?;
		let value = builder
			.ins()
			.load(ptr_ty, MemFlagsData::new(), argv, offset);
		store_frame_slot(&mut builder, &helper_refs, frame, local_slot, value, ptr_ty, ptr_bytes)?;
	}

	// pon_make_generator(body_addr, frame, kind).
	let body_addr = builder.ins().func_addr(ptr_ty, body_ref);
	let kind = builder
		.ins()
		.iconst(ir::types::I8, i64::from(generator_kind_flag(ir_function)));
	let generator = call_pyobject_helper(
		&mut builder,
		helper_refs.make_generator,
		&[body_addr, frame, kind],
		ptr_ty,
		exception_exit,
	);
	builder.ins().return_(&[generator]);

	builder.switch_to_block(exception_exit);
	let null = builder.ins().iconst(ptr_ty, 0);
	builder.ins().return_(&[null]);
	builder.seal_all_blocks();
	super::declare_gc_values(&mut builder, ptr_ty);
	builder.finalize();

	Ok(())
}

/// Build the anonymous resume body (pin J0.1 §2.1/§2.2).
#[allow(clippy::too_many_arguments)]
fn compile_generator_body<M: Module>(
	module: &mut M,
	helpers: &HelperRefs,
	func_ids: &[FuncId],
	functions: &[Function],
	names: &super::NameMap,
	ir_function: &Function,
	ctx: &mut Context,
	fctx: &mut FunctionBuilderContext,
) -> Result<(), CodegenError> {
	let ptr_ty = module.target_config().pointer_type();
	let ptr_bytes = ptr_ty.bytes() as usize;

	let helper_refs = declare_helper_refs(module, helpers, &mut ctx.func);
	let feedback_base = declare_feedback_cells(module, ir_function)?;
	let feedback_base_gv =
		feedback_base.map(|data_id| module.declare_data_in_func(data_id, &mut ctx.func));
	let line_cell_gv = super::declare_line_cell_gv(module, ir_function, &mut ctx.func)?;

	let mut builder = FunctionBuilder::new(&mut ctx.func, fctx);
	let dispatch = builder.create_block();
	let exception_exit = builder.create_block();
	builder.set_cold_block(exception_exit);
	builder.append_block_params_for_function_params(dispatch);
	builder.switch_to_block(dispatch);
	builder.seal_block(dispatch);

	let frame = builder.func.dfg.block_params(dispatch)[0];
	emit_safepoint_poll(&mut builder, &helper_refs);

	// state = load frame.resume_state; store RESUME_RUNNING (the body owns
	// the word from here — closes the re-entrancy window, pin §3.1).
	let state_value =
		builder
			.ins()
			.load(ir::types::I32, MemFlagsData::new(), frame, RESUME_STATE_OFFSET);
	let running = builder
		.ins()
		.iconst(ir::types::I32, i64::from(RESUME_RUNNING as i32));
	builder
		.ins()
		.store(MemFlagsData::new(), running, frame, RESUME_STATE_OFFSET);

	// Reload ALL locals and own closure cells from their spill slots.  The
	// dispatch block dominates every other block, so every local and cell has
	// a defining store before any use, including in resume blocks entered
	// straight from the br_table.
	let mut lower_state = LowerState::new(ir_function.n_locals);
	lower_state.declare_local_storage(&mut builder, ptr_ty);
	for slot in 0..ir_function.n_locals {
		let offset = frame_slot_offset(slot, ptr_bytes)?;
		let value = builder
			.ins()
			.load(ptr_ty, MemFlagsData::new(), frame, offset);
		store_local(&mut builder, &mut lower_state, slot as u32, value)?;
	}
	for cell in 0..own_cell_count(ir_function) {
		let var = builder.declare_var(ptr_ty);
		let offset = frame_slot_offset(ir_function.n_locals + cell, ptr_bytes)?;
		let value = builder
			.ins()
			.load(ptr_ty, MemFlagsData::new(), frame, offset);
		builder.def_var(var, value);
		lower_state.define_cell(cell as u32, var);
	}
	lower_state.gen_ctx = Some(GenBodyCtx { frame });

	// Fresh CLIF block per IR block (IR block 0 = RESUME_START target).
	let block_map: Vec<(pon_ir::ir::BlockId, ir::Block)> = ir_function
		.blocks
		.iter()
		.map(|block| (block.id, builder.create_block()))
		.collect();
	let start_block = block_map
		.iter()
		.find_map(|(id, clif)| (id.0 == 0).then_some(*clif))
		.ok_or(CodegenError::Unsupported("generator body without entry block"))?;

	// Dense resume table: index 0 -> start, index k -> Suspend{state:k}.resume
	// (pin J0.1 §7: states are dense 1..=N in IR order; codegen never
	// renumbers).  RUNNING/FINISHED fall past the table into the default —
	// defensive only, the driver never calls the body in those states.
	let mut resume_targets: Vec<Option<ir::Block>> = Vec::new();
	for block in &ir_function.blocks {
		if let Terminator::Suspend { state, resume, .. } = &block.term {
			let index = *state as usize;
			if index == 0 {
				return Err(CodegenError::Unsupported("suspend state 0 is reserved for RESUME_START"));
			}
			if resume_targets.len() < index {
				resume_targets.resize(index, None);
			}
			let clif = block_map
				.iter()
				.find_map(|(id, clif)| (id == resume).then_some(*clif))
				.ok_or(CodegenError::Unsupported("suspend resume target block"))?;
			if resume_targets[index - 1].replace(clif).is_some() {
				return Err(CodegenError::Unsupported("duplicate generator suspend state"));
			}
		}
	}
	let mut table_calls = Vec::with_capacity(resume_targets.len() + 1);
	table_calls.push(
		builder
			.func
			.dfg
			.block_call(start_block, &[] as &[ir::BlockArg]),
	);
	for target in &resume_targets {
		let target =
			target.ok_or(CodegenError::Unsupported("generator suspend states are not dense"))?;
		table_calls.push(builder.func.dfg.block_call(target, &[] as &[ir::BlockArg]));
	}
	let default_call = builder
		.func
		.dfg
		.block_call(exception_exit, &[] as &[ir::BlockArg]);
	let table = builder.create_jump_table(JumpTableData::new(default_call, &table_calls));
	builder.ins().br_table(state_value, table);

	lower_function_blocks(
		module,
		&mut builder,
		&helper_refs,
		func_ids,
		functions,
		names,
		&mut lower_state,
		ptr_ty,
		ptr_bytes,
		exception_exit,
		ir_function,
		&block_map,
		feedback_base_gv,
		line_cell_gv,
		None,
	)?;

	// Function-level exception exit: nothing caught the pending exception —
	// finish the frame via pon_gen_unwind (PEP 479/525 + slot zeroing) and
	// propagate its NULL.
	builder.switch_to_block(exception_exit);
	let kind = builder
		.ins()
		.iconst(ir::types::I8, i64::from(generator_kind_flag(ir_function)));
	let unwind_call = builder.ins().call(helper_refs.gen_unwind, &[frame, kind]);
	let unwind_result = builder.func.dfg.inst_results(unwind_call)[0];
	builder.ins().return_(&[unwind_result]);
	builder.seal_all_blocks();
	super::declare_gc_values(&mut builder, ptr_ty);
	builder.finalize();

	Ok(())
}

/// Lower `Terminator::Suspend { state, val, resume }` (pin J0.1 §2.2): spill
/// every local to its slot, store the state number, and return the yielded
/// value.  The resume block is entered only through the entry `br_table`.
pub(crate) fn lower_suspend(
	builder: &mut FunctionBuilder<'_>,
	state: &LowerState,
	helpers: &HelperFuncRefs,
	ptr_ty: ir::Type,
	suspend_state: u32,
	val: IrValue,
) -> Result<(), CodegenError> {
	let ctx = state
		.gen_ctx
		.ok_or(CodegenError::Unsupported("suspend terminator outside a generator body"))?;
	let ptr_bytes = ptr_ty.bytes() as usize;
	let yielded = state.value(val)?;

	// 1. Spill all locals and own closure cells (+ barriers).  Handler-record pops
	//    were already emitted by the IR transform as explicit PopExcInfo
	//    instructions.
	for slot in 0..state.locals.len() {
		let value = builder.use_var(state.locals[slot]);
		store_frame_slot(builder, helpers, ctx.frame, slot, value, ptr_ty, ptr_bytes)?;
	}
	for cell in 0..state.cells.len() {
		let cell_id = cell as u32;
		let var = state
			.cell(cell_id)
			.ok_or(CodegenError::ClosureCellUnderflow {
				cell:      cell_id,
				own_cells: state.cells.len(),
			})?;
		let value = builder.use_var(var);
		store_frame_slot(
			builder,
			helpers,
			ctx.frame,
			state.locals.len() + cell,
			value,
			ptr_ty,
			ptr_bytes,
		)?;
	}

	// 2. Set resume_state = k.
	let state_value = builder
		.ins()
		.iconst(ir::types::I32, i64::from(suspend_state as i32));
	builder
		.ins()
		.store(MemFlagsData::new(), state_value, ctx.frame, RESUME_STATE_OFFSET);

	// 3. Return the yielded value (non-NULL by IR construction).
	builder.ins().return_(&[yielded]);
	Ok(())
}

/// Lower `Terminator::Return` inside a generator body (pin J0.1 §4.4):
/// `return pon_gen_finish(frame, v)` — FINISHED, zero slots, StopIteration(v)
/// pending, NULL returned.  The helper's NULL is the protocol here, so it is
/// NOT routed to the exception exit (pon_gen_unwind would replay PEP 479 on
/// the fresh StopIteration).
pub(crate) fn lower_gen_return(
	builder: &mut FunctionBuilder<'_>,
	state: &LowerState,
	helpers: &HelperFuncRefs,
	value: IrValue,
) -> Result<(), CodegenError> {
	let ctx = state
		.gen_ctx
		.ok_or(CodegenError::Unsupported("generator return outside a generator body"))?;
	let retval = state.value(value)?;
	let call = builder.ins().call(helpers.gen_finish, &[ctx.frame, retval]);
	let result = builder.func.dfg.inst_results(call)[0];
	builder.ins().return_(&[result]);
	Ok(())
}

/// Lower `InstKind::GenResumePayload` through `pon_gen_consume_payload`
/// (pin J0.1 §4.2): a scheduled `throw` re-raises here and NULL-routes to the
/// statically enclosing handler; otherwise the sent value is produced.
pub(crate) fn lower_gen_resume_payload(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	state: &LowerState,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let ctx = state
		.gen_ctx
		.ok_or(CodegenError::Unsupported("generator payload consume outside a generator body"))?;
	Ok(call_pyobject_helper(
		builder,
		helpers.gen_consume_payload,
		&[ctx.frame],
		ptr_ty,
		exception_exit,
	))
}

/// Lower `InstKind::GenDelegateStep` through `pon_gen_delegate_step`
/// (pin J0.1 §6).
///
/// The result is nullable BY DESIGN: NULL with pending `StopIteration` means
/// the delegation finished.  The `ForLoop` terminator decodes NULL via
/// `pon_gen_stop_value`, exactly like `ForNext`.
pub(crate) fn lower_gen_delegate_step(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	state: &LowerState,
	delegate: IrValue,
) -> Result<ir::Value, CodegenError> {
	let ctx = state
		.gen_ctx
		.ok_or(CodegenError::Unsupported("generator delegate step outside a generator body"))?;
	let delegate = state.value(delegate)?;
	let call = builder
		.ins()
		.call(helpers.gen_delegate_step, &[ctx.frame, delegate]);
	Ok(builder.func.dfg.inst_results(call)[0])
}

/// Lower `InstKind::GenLastStopValue` through `pon_gen_last_stop_value`:
/// produce the stashed `StopIteration.value` of the delegation that just
/// finished (the `yield from`/`await` expression result).
pub(crate) fn lower_gen_last_stop_value(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	Ok(call_pyobject_helper(builder, helpers.gen_last_stop_value, &[], ptr_ty, exception_exit))
}

/// Lower synchronous iterator acquisition through `pon_get_iter`.
pub(crate) fn lower_get_iter(
	builder: &mut FunctionBuilder<'_>,
	helper: ir::FuncRef,
	state: &LowerState,
	iterable: IrValue,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	lower_feedback_unary(builder, helper, state, iterable, ptr_ty, exception_exit)
}

/// Lower asynchronous iterator acquisition through `pon_get_aiter`.
pub(crate) fn lower_get_aiter(
	builder: &mut FunctionBuilder<'_>,
	helper: ir::FuncRef,
	state: &LowerState,
	iterable: IrValue,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	lower_feedback_unary(builder, helper, state, iterable, ptr_ty, exception_exit)
}

/// Lower iterator advance through `pon_for_next`.
///
/// `ForNext` is nullable by design: a NULL result may be either StopIteration
/// or a real iterator error.  The loop terminator consumes the raw value and
/// asks `pon_gen_stop_value` to distinguish those cases.
pub(crate) fn lower_for_next(
	builder: &mut FunctionBuilder<'_>,
	helper: ir::FuncRef,
	state: &LowerState,
	iter: IrValue,
	_ptr_ty: ir::Type,
	_exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let iter = state.value(iter)?;
	let iter_ty = builder.func.dfg.value_type(iter);
	let feedback = builder.ins().iconst(iter_ty, 0);
	let call = builder.ins().call(helper, &[iter, feedback]);
	Ok(builder.func.dfg.inst_results(call)[0])
}

/// Lower `await` through `pon_await`.
pub(crate) fn lower_await(
	builder: &mut FunctionBuilder<'_>,
	helper: ir::FuncRef,
	state: &LowerState,
	awaitable: IrValue,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	lower_feedback_unary(builder, helper, state, awaitable, ptr_ty, exception_exit)
}

fn lower_feedback_unary(
	builder: &mut FunctionBuilder<'_>,
	helper: ir::FuncRef,
	state: &LowerState,
	value: IrValue,
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	let value = state.value(value)?;
	let feedback = builder.ins().iconst(ptr_ty, 0);
	Ok(call_pyobject_helper(builder, helper, &[value, feedback], ptr_ty, exception_exit))
}
