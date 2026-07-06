//! Baseline IR-to-CLIF lowering for Phase A with Phase-B family dispatch hubs.

pub(crate) mod attr;
pub(crate) mod call;
pub(crate) mod compare;
pub(crate) mod container;
pub(crate) mod control;
pub(crate) mod exc;
pub(crate) mod r#gen;
pub(crate) mod mapping;
pub(crate) mod match_;
pub(crate) mod name;
pub(crate) mod number;
pub(crate) mod spill;
pub(crate) mod strings;

use std::{
	collections::{HashMap, HashSet, VecDeque},
	error::Error,
	fmt,
};

use cranelift_codegen::{
	Context,
	ir::{
		self, AbiParam, FuncRef, InstBuilder, MemFlagsData, StackSlotData, StackSlotKind,
		condcodes::IntCC,
	},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, ModuleError};
use pon_ir::ir::{
	Block as IrBlock, BlockId, Function, InstKind, Module as IrModule, PyConst, Terminator,
	Value as IrValue,
};

use crate::helpers::HelperRefs;

/// Runtime-name id remapping for a lowered IR module.
///
/// `pon-ir` name operands are source-local indexes into
/// `pon_ir::ir::Module::names`. Runtime helpers consume ids from
/// `pon_runtime::intern`, so codegen must remap every source-local id before
/// emitting `LoadGlobal`, `LoadName`, `StoreGlobal`, or `MakeFunction` helper
/// arguments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NameMap {
	runtime_ids: Vec<u32>,
}

impl NameMap {
	/// Build the runtime-id map for all names in an IR module.
	#[must_use]
	pub fn from_ir_module(module: &IrModule) -> Self {
		Self {
			runtime_ids: module
				.names
				.iter()
				.map(|name| pon_runtime::intern::intern(name))
				.collect(),
		}
	}

	pub(crate) fn runtime_id(&self, source_id: u32) -> Result<u32, CodegenError> {
		self
			.runtime_ids
			.get(source_id as usize)
			.copied()
			.ok_or(CodegenError::NameOutOfRange { source_id })
	}
}

pub fn entry_arg_counts(module: &IrModule) -> Vec<usize> {
	let mut counts = module
		.functions
		.iter()
		.map(|function| function.arity)
		.collect::<Vec<_>>();
	for function in &module.functions {
		for block in &function.blocks {
			for inst in &block.insts {
				if let InstKind::MakeFunctionFull { code, .. } = &inst.kind {
					if let Some(count) = counts.get_mut(code.0 as usize) {
						let target = &module.functions[code.0 as usize];
						*count = (*count).max(target.params.total_slot_count().max(target.arity));
					}
				}
			}
		}
	}
	counts
}

/// Error reported while lowering Phase-A IR into Cranelift IR.
#[derive(Debug)]
pub enum CodegenError {
	/// Cranelift module declaration or data definition failed.
	Module(ModuleError),
	/// A source-local IR name id has no runtime interner mapping.
	NameOutOfRange { source_id: u32 },
	/// A function index referenced by `MakeFunction` has no declared `FuncId`.
	FunctionIndexOutOfRange { func_index: u32 },
	/// A local slot index is outside the function's declared local range.
	LocalOutOfRange { slot: u32, n_locals: usize },
	/// A local slot was read before a parameter load or local store defined it.
	LocalUsedBeforeDefinition { slot: u32 },
	/// An SSA value operand was referenced before its producing instruction
	/// lowered.
	ValueNotDefined(IrValue),
	/// A stack or memory offset does not fit Cranelift's 32-bit offset
	/// immediate.
	OffsetTooLarge { offset: usize },
	/// Phase A received an IR operation reserved for a later phase.
	Unsupported(&'static str),
	/// A cell id below the own-cell (`MakeCell`) count missed the cell map.
	ClosureCellUnderflow { cell: u32, own_cells: usize },
}

impl fmt::Display for CodegenError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Module(error) => write!(f, "Cranelift module error: {error}"),
			Self::NameOutOfRange { source_id } => {
				write!(f, "IR name id {source_id} has no runtime interner mapping")
			},
			Self::FunctionIndexOutOfRange { func_index } => {
				write!(f, "function index {func_index} has no declared FuncId")
			},
			Self::LocalOutOfRange { slot, n_locals } => {
				write!(f, "local slot {slot} is outside n_locals={n_locals}")
			},
			Self::LocalUsedBeforeDefinition { slot } => {
				write!(f, "local slot {slot} was used before definition")
			},
			Self::ValueNotDefined(value) => {
				write!(f, "SSA value {:?} was used before definition", value)
			},
			Self::OffsetTooLarge { offset } => write!(f, "offset {offset} does not fit in i32"),
			Self::Unsupported(op) => write!(f, "unsupported Phase-A lowering operation: {op}"),
			Self::ClosureCellUnderflow { cell, own_cells } => {
				write!(f, "cell id {cell} is below the own-cell count {own_cells} but has no MakeCell")
			},
		}
	}
}

impl Error for CodegenError {}

impl From<ModuleError> for CodegenError {
	fn from(error: ModuleError) -> Self {
		Self::Module(error)
	}
}

#[derive(Clone, Copy)]
pub(crate) struct HelperFuncRefs {
	pub(crate) const_int:             FuncRef,
	pub(crate) const_float:           FuncRef,
	pub(crate) const_complex:         FuncRef,
	pub(crate) const_bool:            FuncRef,
	pub(crate) const_bytes:           FuncRef,
	pub(crate) const_bigint:          FuncRef,
	pub(crate) rich_compare:          FuncRef,
	pub(crate) number_unary:          FuncRef,
	pub(crate) number_binary:         FuncRef,
	pub(crate) number_inplace:        FuncRef,
	pub(crate) is_true:               FuncRef,
	pub(crate) contains:              FuncRef,
	pub(crate) call:                  FuncRef,
	pub(crate) call_ex:               FuncRef,
	pub(crate) call_method:           FuncRef,
	pub(crate) load_global:           FuncRef,
	pub(crate) load_name:             FuncRef,
	pub(crate) load_local:            FuncRef,
	pub(crate) delete_local:          FuncRef,
	pub(crate) delete_global:         FuncRef,
	pub(crate) delete_name:           FuncRef,
	pub(crate) get_attr:              FuncRef,
	pub(crate) set_attr:              FuncRef,
	pub(crate) del_attr:              FuncRef,
	pub(crate) build_tuple:           FuncRef,
	pub(crate) build_list:            FuncRef,
	pub(crate) build_set:             FuncRef,
	pub(crate) build_slice:           FuncRef,
	pub(crate) list_append:           FuncRef,
	pub(crate) set_add:               FuncRef,
	pub(crate) list_extend:           FuncRef,
	pub(crate) list_to_tuple:         FuncRef,
	pub(crate) set_update:            FuncRef,
	pub(crate) unpack_seq:            FuncRef,
	pub(crate) unpack_ex:             FuncRef,
	pub(crate) get_len:               FuncRef,
	pub(crate) build_map:             FuncRef,
	pub(crate) map_insert:            FuncRef,
	pub(crate) dict_merge:            FuncRef,
	pub(crate) dict_merge_unique:     FuncRef,
	pub(crate) subscript_get:         FuncRef,
	pub(crate) subscript_set:         FuncRef,
	pub(crate) subscript_del:         FuncRef,
	pub(crate) build_string:          FuncRef,
	pub(crate) build_template:        FuncRef,
	pub(crate) load_builtin:          FuncRef,
	pub(crate) store_name:            FuncRef,
	pub(crate) import_name:           FuncRef,
	pub(crate) import_from:           FuncRef,
	pub(crate) import_star:           FuncRef,
	pub(crate) raise:                 FuncRef,
	pub(crate) reraise:               FuncRef,
	pub(crate) push_exc_info:         FuncRef,
	pub(crate) pop_exc_info:          FuncRef,
	pub(crate) match_exc:             FuncRef,
	pub(crate) check_exc_star:        FuncRef,
	pub(crate) exc_star_enter:        FuncRef,
	pub(crate) exc_star_match:        FuncRef,
	pub(crate) exc_star_body_ok:      FuncRef,
	pub(crate) exc_star_body_raised:  FuncRef,
	pub(crate) exc_star_finish:       FuncRef,
	pub(crate) get_current_exc:       FuncRef,
	pub(crate) build_exc_group:       FuncRef,
	pub(crate) get_iter:              FuncRef,
	pub(crate) get_aiter:             FuncRef,
	pub(crate) for_next:              FuncRef,
	pub(crate) gen_stop_value:        FuncRef,
	pub(crate) gen_last_stop_value:   FuncRef,
	pub(crate) gen_frame_alloc:       FuncRef,
	pub(crate) make_generator:        FuncRef,
	pub(crate) gen_consume_payload:   FuncRef,
	pub(crate) gen_finish:            FuncRef,
	pub(crate) gen_unwind:            FuncRef,
	pub(crate) gen_delegate_step:     FuncRef,
	pub(crate) await_value:           FuncRef,
	pub(crate) match_sequence:        FuncRef,
	pub(crate) match_mapping:         FuncRef,
	pub(crate) match_class:           FuncRef,
	pub(crate) match_keys:            FuncRef,
	pub(crate) match_len_ge:          FuncRef,
	pub(crate) make_function:         FuncRef,
	#[allow(dead_code)]
	pub(crate) make_function_full:    FuncRef,
	pub(crate) function_set_closure:  FuncRef,
	pub(crate) make_cell:             FuncRef,
	pub(crate) cell_get:              FuncRef,
	pub(crate) cell_set:              FuncRef,
	pub(crate) cell_delete:           FuncRef,
	pub(crate) current_closure_cell:  FuncRef,
	pub(crate) function_set_annotate: FuncRef,
	pub(crate) make_type_alias:       FuncRef,
	pub(crate) make_typevar:          FuncRef,
	pub(crate) setup_annotations:     FuncRef,
	pub(crate) build_class:           FuncRef,
	pub(crate) build_class_full:      FuncRef,
	pub(crate) load_build_class:      FuncRef,
	pub(crate) store_global:          FuncRef,
	pub(crate) none:                  FuncRef,
	pub(crate) osr_poll:              FuncRef,
	pub(crate) deopt_note:            FuncRef,
	#[cfg(feature = "free-threading")]
	pub(crate) safepoint_poll:        FuncRef,
	#[cfg(feature = "free-threading")]
	pub(crate) gc_write_barrier:      FuncRef,
}

pub(crate) struct LowerState {
	pub(crate) values:        HashMap<IrValue, ir::Value>,
	pub(crate) locals:        Vec<Variable>,
	pub(crate) local_defined: Vec<bool>,
	/// Explicit per-local frame slots mirroring every named-local store.
	///
	/// Conservative collection scans frame memory, never registers, so a
	/// local whose only up-to-date home is a register would be invisible to
	/// a `gc.collect()` reached from inside the function (and rooting raw
	/// registers instead retains *dead* values the allocator never cleared).
	/// `store_local` mirrors each named-local write — including the unbind
	/// sentinel written by `del` — into its shadow slot, so exactly the
	/// still-bound locals stay reachable from the stack scan.
	pub(crate) local_shadow:  Vec<ir::StackSlot>,
	/// Own closure cells (`MakeCell` results) by dense cell id.  Cells are
	/// SSA `Variable`s, not raw values: generator bodies redefine them from
	/// frame spill slots in the dispatch block, so resume paths that never
	/// execute the entry prologue still see a dominating definition.
	pub(crate) cells:         HashMap<u32, Variable>,
	/// Next dense cell id handed to a lowered `MakeCell`.
	pub(crate) next_cell_id:  u32,
	pub(crate) last_value:    Option<ir::Value>,
	/// Present exactly while lowering a resumable generator body (pin J0.1):
	/// carries the frame pointer for suspend spills and the gen epilogues.
	pub(crate) gen_ctx:       Option<r#gen::GenBodyCtx>,
}

impl LowerState {
	pub(crate) fn new(local_count: usize) -> Self {
		Self {
			values:        HashMap::new(),
			locals:        Vec::with_capacity(local_count),
			local_shadow:  Vec::with_capacity(local_count),
			local_defined: vec![false; local_count],
			cells:         HashMap::new(),
			next_cell_id:  0,
			last_value:    None,
			gen_ctx:       None,
		}
	}

	/// Declares the SSA variable and shadow frame slot for every local.
	///
	/// Every `LowerState` construction site must call this exactly once
	/// before lowering instructions; `store_local` indexes both vectors
	/// unconditionally.
	pub(crate) fn declare_local_storage(
		&mut self,
		builder: &mut FunctionBuilder<'_>,
		ptr_ty: ir::Type,
	) {
		for _ in 0..self.local_defined.len() {
			self.locals.push(builder.declare_var(ptr_ty));
			self
				.local_shadow
				.push(builder.create_sized_stack_slot(StackSlotData {
					kind:        StackSlotKind::ExplicitSlot,
					size:        ptr_ty.bytes(),
					align_shift: ptr_ty.bytes().trailing_zeros() as u8,
					key:         None,
				}));
		}
	}

	pub(crate) fn define_value(&mut self, ir_value: IrValue, clif_value: ir::Value) {
		self.values.insert(ir_value, clif_value);
		self.last_value = Some(clif_value);
	}

	pub(crate) fn last_value(&self) -> Option<ir::Value> {
		self.last_value
	}

	pub(crate) fn value(&self, ir_value: IrValue) -> Result<ir::Value, CodegenError> {
		self
			.values
			.get(&ir_value)
			.copied()
			.ok_or(CodegenError::ValueNotDefined(ir_value))
	}

	pub(crate) fn define_cell(&mut self, cell: u32, var: Variable) {
		self.cells.insert(cell, var);
	}

	pub(crate) fn cell(&self, cell: u32) -> Option<Variable> {
		self.cells.get(&cell).copied()
	}
}

/// Lower a contiguous boxed IR sub-region with the baseline instruction
/// lowering.
///
/// Phase-D optimizing codegen uses this as the cold-twin escape hatch: the fast
/// path can speculate on unboxed values, while guard failures and unsupported
/// typed operations jump to a cold copy that reuses the existing boxed lowering
/// instead of duplicating baseline semantics.
#[allow(
	dead_code,
	clippy::too_many_arguments,
	reason = "Phase-D cold-twin hook is reserved until the optimizing entry point lands"
)]
pub(crate) fn lower_boxed_subregion<M: Module>(
	module: &mut M,
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	func_ids: &[FuncId],
	names: &NameMap,
	state: &mut LowerState,
	ptr_ty: ir::Type,
	ptr_bytes: usize,
	exception_exit: ir::Block,
	blocks: &[&IrBlock],
) -> Result<(), CodegenError> {
	for block in blocks {
		for inst in &block.insts {
			let value = lower_inst(
				module,
				builder,
				helpers,
				func_ids,
				&[],
				names,
				state,
				ptr_ty,
				ptr_bytes,
				exception_exit,
				&inst.kind,
				None,
			)?;
			state.define_value(inst.result, value);
		}
	}
	Ok(())
}

/// Lower one IR function into the supplied Cranelift [`Context`].
///
/// The emitted function ABI is always
/// `(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject`, represented in
/// CLIF as `(ptr, ptr) -> ptr`. Parameter locals `0..arity` are initialized by
/// loading boxed pointers from `argv + slot * pointer_size`. Runtime helper
/// calls returning boxed objects are followed by the Phase-A NULL-sentinel
/// branch to a shared exception exit that returns NULL.
///
/// `names` must be built with [`NameMap::from_ir_module`] for the enclosing IR
/// module; raw source-local IR ids must never be passed directly to runtime
/// helpers.
pub fn compile_function<M: Module>(
	module: &mut M,
	helpers: &HelperRefs,
	func_ids: &[FuncId],
	functions: &[Function],
	names: &NameMap,
	ir: &Function,
	entry_arg_count: usize,
	ctx: &mut Context,
	fctx: &mut FunctionBuilderContext,
) -> Result<(), CodegenError> {
	if ir.is_generator {
		// Pin J0.1 two-function scheme: allocation stub at the declared
		// FuncId + anonymous resume body (baseline/gen.rs).
		return r#gen::compile_generator_function(
			module,
			helpers,
			func_ids,
			functions,
			names,
			ir,
			entry_arg_count,
			ctx,
			fctx,
		);
	}
	module.clear_context(ctx);
	let ptr_ty = module.target_config().pointer_type();
	let ptr_bytes = ptr_ty.bytes() as usize;

	ctx.func.signature.params.push(AbiParam::new(ptr_ty));
	ctx.func.signature.params.push(AbiParam::new(ptr_ty));
	ctx.func.signature.returns.push(AbiParam::new(ptr_ty));

	let helper_refs = declare_helper_refs(module, helpers, &mut ctx.func);
	// J0.3: one static, writable, zero-initialized FeedbackCell array per
	// compiled code object.  Every closure produced from one `def` shares
	// these cells (CPython-style per-code caches); IC guards validate
	// identity+version, so sharing is semantically inert.  NULL when the
	// function has no specializable sites.
	let feedback_base = declare_feedback_cells(module, ir)?;
	let feedback_base_gv =
		feedback_base.map(|data_id| module.declare_data_in_func(data_id, &mut ctx.func));
	let line_cell_gv = declare_line_cell_gv(module, ir, &mut ctx.func)?;

	let mut builder = FunctionBuilder::new(&mut ctx.func, fctx);
	let entry = builder.create_block();
	let exception_exit = builder.create_block();
	builder.set_cold_block(exception_exit);
	builder.append_block_params_for_function_params(entry);
	builder.switch_to_block(entry);
	builder.seal_block(entry);

	let argv = builder.func.dfg.block_params(entry)[0];
	let _argc = builder.func.dfg.block_params(entry)[1];

	let mut state = LowerState::new(ir.n_locals);
	state.declare_local_storage(&mut builder, ptr_ty);
	let unbound = builder.ins().iconst(ptr_ty, 0);
	for slot in 0..ir.n_locals {
		store_local(&mut builder, &mut state, slot as u32, unbound)?;
	}
	initialize_parameter_locals(
		&mut builder,
		&mut state,
		argv,
		ptr_bytes,
		entry_arg_count,
		ir,
		ptr_ty,
	)?;
	emit_safepoint_poll(&mut builder, &helper_refs);

	let block_map: Vec<(pon_ir::ir::BlockId, ir::Block)> = ir
		.blocks
		.iter()
		.map(|block| {
			if block.id.0 == 0 {
				(block.id, entry)
			} else {
				(block.id, builder.create_block())
			}
		})
		.collect();
	lower_function_blocks(
		module,
		&mut builder,
		&helper_refs,
		func_ids,
		functions,
		names,
		&mut state,
		ptr_ty,
		ptr_bytes,
		exception_exit,
		ir,
		&block_map,
		feedback_base_gv,
		line_cell_gv,
		Some(entry),
	)?;

	builder.switch_to_block(exception_exit);
	let null = builder.ins().iconst(ptr_ty, 0);
	builder.ins().return_(&[null]);
	builder.seal_all_blocks();
	declare_gc_values(&mut builder, ptr_ty);
	builder.finalize();

	Ok(())
}

/// Lower a baseline OSR entry that resumes `ir` at `header` from an
/// `OsrTransferBuffer`.
#[allow(clippy::too_many_arguments)]
pub fn compile_osr_function<M: Module>(
	module: &mut M,
	helpers: &HelperRefs,
	func_ids: &[FuncId],
	functions: &[Function],
	names: &NameMap,
	ir: &Function,
	header: BlockId,
	live_values: &[IrValue],
	ctx: &mut Context,
	fctx: &mut FunctionBuilderContext,
) -> Result<(), CodegenError> {
	module.clear_context(ctx);
	let ptr_ty = module.target_config().pointer_type();
	let ptr_bytes = ptr_ty.bytes() as usize;
	let live_count = ir.n_locals.saturating_add(live_values.len());
	if live_count > 16 {
		return Err(CodegenError::Unsupported("OSR live set exceeds transfer buffer"));
	}

	ctx.func.signature.params.push(AbiParam::new(ptr_ty));
	ctx.func.signature.returns.push(AbiParam::new(ptr_ty));

	let helper_refs = declare_helper_refs(module, helpers, &mut ctx.func);
	let feedback_base = declare_feedback_cells(module, ir)?;
	let feedback_base_gv =
		feedback_base.map(|data_id| module.declare_data_in_func(data_id, &mut ctx.func));
	let line_cell_gv = declare_line_cell_gv(module, ir, &mut ctx.func)?;
	let reachable = reachable_from(ir, header);

	let mut builder = FunctionBuilder::new(&mut ctx.func, fctx);
	let entry = builder.create_block();
	let exception_exit = builder.create_block();
	builder.set_cold_block(exception_exit);
	builder.append_block_params_for_function_params(entry);
	builder.switch_to_block(entry);
	builder.seal_block(entry);
	let buffer = builder.func.dfg.block_params(entry)[0];

	let mut state = LowerState::new(ir.n_locals);
	state.declare_local_storage(&mut builder, ptr_ty);
	for slot in 0..ir.n_locals {
		let value =
			builder
				.ins()
				.load(ptr_ty, MemFlagsData::new(), buffer, offset_i32(8 + slot * ptr_bytes)?);
		store_local(&mut builder, &mut state, slot as u32, value)?;
	}
	for (index, value) in live_values.iter().enumerate() {
		let offset = 8 + (ir.n_locals + index) * ptr_bytes;
		let boxed = builder
			.ins()
			.load(ptr_ty, MemFlagsData::new(), buffer, offset_i32(offset)?);
		state.define_value(*value, boxed);
	}

	let block_map: Vec<(BlockId, ir::Block)> = ir
		.blocks
		.iter()
		.filter(|block| reachable.contains(&block.id))
		.map(|block| (block.id, builder.create_block()))
		.collect();
	let header_block = block_map
		.iter()
		.find_map(|(id, block)| (*id == header).then_some(*block))
		.ok_or(CodegenError::Unsupported("OSR header block"))?;
	builder.ins().jump(header_block, &[]);

	lower_function_blocks_subset(
		module,
		&mut builder,
		&helper_refs,
		func_ids,
		functions,
		names,
		&mut state,
		ptr_ty,
		ptr_bytes,
		exception_exit,
		ir,
		&block_map,
		feedback_base_gv,
		line_cell_gv,
		&reachable,
	)?;

	builder.switch_to_block(exception_exit);
	let null = builder.ins().iconst(ptr_ty, 0);
	builder.ins().return_(&[null]);
	builder.seal_all_blocks();
	declare_gc_values(&mut builder, ptr_ty);
	builder.finalize();
	Ok(())
}

/// Declares every pointer-typed SSA value as a stack-map root before
/// `finalize()`, so Cranelift's safepoint pass spills the values LIVE across
/// each helper call into mapped sized slots and records a `UserStackMap` per
/// call site.  The maps (surfaced through `MachBufferFinalized::
/// user_stack_maps`) let the collector scan tier-0 frames precisely: dead
/// values parked in regalloc spill slots or callee-saved registers stop
/// pinning garbage, which conservative scanning cannot avoid (corpus
/// `weakref_dicts` on x86-64).  Non-reference `i64`s (argc, raw addresses)
/// are over-approximated as roots; the GC's pointer classifier already
/// ignores non-heap words, mirroring the conservative scan's tolerance.
pub(crate) fn declare_gc_values(builder: &mut FunctionBuilder<'_>, ptr_ty: ir::Type) {
	let values: Vec<ir::Value> = builder.func.dfg.values().collect();
	for value in values {
		if builder.func.dfg.value_type(value) == ptr_ty {
			builder.declare_value_needs_stack_map(value);
		}
	}
}
type ExceptionTargetStack = Vec<BlockId>;

fn merge_exception_entry_stack(
	entries: &mut HashMap<BlockId, ExceptionTargetStack>,
	worklist: &mut VecDeque<BlockId>,
	block: BlockId,
	stack: &[BlockId],
) -> Result<(), CodegenError> {
	if let Some(existing) = entries.get(&block) {
		if existing == stack {
			return Ok(());
		}
		return Err(CodegenError::Unsupported("inconsistent exception handler stack at block join"));
	}
	entries.insert(block, stack.to_vec());
	worklist.push_back(block);
	Ok(())
}

fn exception_successors(term: &Terminator, stack: &[BlockId]) -> Vec<BlockId> {
	match term {
		Terminator::Jump(target) => vec![*target],
		Terminator::Branch { then_blk, else_blk, .. } => vec![*then_blk, *else_blk],
		Terminator::CondBranch { then_, else_, .. } => vec![*then_, *else_],
		Terminator::ForLoop { body, done, .. } => vec![*body, *done],
		Terminator::Suspend { resume, .. } => vec![*resume],
		Terminator::RaiseTerm => stack.last().copied().into_iter().collect(),
		Terminator::Return(_) | Terminator::Unreachable => Vec::new(),
		_ => Vec::new(),
	}
}

pub(crate) fn block_exception_entry_stacks(
	ir: &Function,
) -> Result<HashMap<BlockId, ExceptionTargetStack>, CodegenError> {
	let Some(entry) = ir.blocks.first().map(|block| block.id) else {
		return Ok(HashMap::new());
	};
	let blocks = ir
		.blocks
		.iter()
		.map(|block| (block.id, block))
		.collect::<HashMap<_, _>>();
	let mut entries = HashMap::new();
	let mut worklist = VecDeque::new();
	entries.insert(entry, Vec::new());
	worklist.push_back(entry);

	while let Some(block_id) = worklist.pop_front() {
		let block = *blocks
			.get(&block_id)
			.ok_or(CodegenError::Unsupported("exception stack references missing block"))?;
		let mut stack = entries.get(&block_id).cloned().unwrap_or_default();
		for inst in &block.insts {
			match &inst.kind {
				InstKind::PushExcInfo { target, .. } => {
					stack.push(*target);
					merge_exception_entry_stack(&mut entries, &mut worklist, *target, &stack)?;
				},
				InstKind::PopExcInfo => {
					stack.pop();
				},
				_ => {},
			}
		}
		for successor in exception_successors(&block.term, &stack) {
			merge_exception_entry_stack(&mut entries, &mut worklist, successor, &stack)?;
		}
	}

	Ok(entries)
}

fn exception_exit_from_stack(
	stack: &[BlockId],
	block_map: &[(BlockId, ir::Block)],
	exception_exit: ir::Block,
	missing_message: &'static str,
) -> Result<ir::Block, CodegenError> {
	let Some(target) = stack.last() else {
		return Ok(exception_exit);
	};
	block_map
		.iter()
		.find_map(|(id, clif)| (*id == *target).then_some(*clif))
		.ok_or(CodegenError::Unsupported(missing_message))
}

/// Lower every IR block of `ir` into its mapped CLIF block, deriving the
/// active exception-handler route for each block from the IR CFG instead of
/// leaking `PushExcInfo`/`PopExcInfo` effects across the linear block walk.
///
/// `prefilled_entry` is the CLIF block that IR block 0 maps to when the
/// caller already switched to it and emitted a prologue (the non-generator
/// entry); `None` means every block (including block 0) still needs
/// `switch_to_block`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_function_blocks<M: Module>(
	module: &mut M,
	builder: &mut FunctionBuilder<'_>,
	helper_refs: &HelperFuncRefs,
	func_ids: &[FuncId],
	functions: &[Function],
	names: &NameMap,
	state: &mut LowerState,
	ptr_ty: ir::Type,
	ptr_bytes: usize,
	exception_exit: ir::Block,
	ir: &Function,
	block_map: &[(pon_ir::ir::BlockId, ir::Block)],
	feedback_base_gv: Option<ir::GlobalValue>,
	line_cell_gv: Option<ir::GlobalValue>,
	prefilled_entry: Option<ir::Block>,
) -> Result<(), CodegenError> {
	let exception_entry_stacks = block_exception_entry_stacks(ir)?;
	// GC temp-spill schedule: root call-crossing expression temporaries in
	// frame memory around every helper window that may re-enter user code
	// (see `baseline::spill`).
	let temp_spill = spill::TempSpillPlan::compute(builder, ir, ptr_ty)?;

	for block in &ir.blocks {
		let clif_block = block_map
			.iter()
			.find_map(|(id, clif)| (*id == block.id).then_some(*clif))
			.ok_or(CodegenError::Unsupported("missing basic block"))?;
		if Some(clif_block) != prefilled_entry {
			builder.switch_to_block(clif_block);
		}
		let mut exception_target_stack = exception_entry_stacks
			.get(&block.id)
			.cloned()
			.unwrap_or_default();
		let mut current_exception_exit = exception_exit_from_stack(
			&exception_target_stack,
			block_map,
			exception_exit,
			"exception handler target block",
		)?;
		let mut last_line = 0u32;
		for (inst_index, inst) in block.insts.iter().enumerate() {
			if let Some(plan) = &temp_spill {
				plan.emit_inst_window(builder, state, block.id, inst_index, ptr_ty);
			}
			// Statement-line transition: record the new line into the runtime
			// cell before the statement's first effect can raise or call.
			if let Some(line_cell_gv) = line_cell_gv {
				if inst.line != 0 && inst.line != last_line {
					emit_line_store(builder, line_cell_gv, ptr_ty, inst.line);
					last_line = inst.line;
				}
			}
			// J0.3: materialize this site's static feedback-cell address
			// (base + slot * FEEDBACK_CELL_SIZE) for specializable ops.
			let feedback_cell = match (inst.feedback_slot, feedback_base_gv) {
				(Some(slot), Some(gv)) => {
					let base = builder.ins().global_value(ptr_ty, gv);
					Some(builder.ins().iadd_imm(
						base,
						i64::from(slot.0) * pon_runtime::feedback::FEEDBACK_CELL_SIZE as i64,
					))
				},
				_ => None,
			};
			let value = match &inst.kind {
				InstKind::PushExcInfo { target, stack_depth, kind } => {
					let value = exc::lower_push_exc_info(
						builder,
						helper_refs.push_exc_info,
						target.0,
						*stack_depth,
						*kind,
						ptr_ty,
						current_exception_exit,
					)?;
					exception_target_stack.push(*target);
					current_exception_exit = exception_exit_from_stack(
						&exception_target_stack,
						block_map,
						exception_exit,
						"exception handler target block",
					)?;
					value
				},
				InstKind::PopExcInfo => {
					exception_target_stack.pop();
					let previous_exception_exit = exception_exit_from_stack(
						&exception_target_stack,
						block_map,
						exception_exit,
						"exception handler target block",
					)?;
					let value = exc::lower_pop_exc_info(
						builder,
						helper_refs.pop_exc_info,
						ptr_ty,
						previous_exception_exit,
					)?;
					current_exception_exit = previous_exception_exit;
					value
				},
				_ => lower_inst(
					module,
					builder,
					helper_refs,
					func_ids,
					functions,
					names,
					state,
					ptr_ty,
					ptr_bytes,
					current_exception_exit,
					&inst.kind,
					feedback_cell,
				)?,
			};
			state.define_value(inst.result, value);
		}
		if let Some(plan) = &temp_spill {
			plan.emit_term_window(builder, state, block.id, ptr_ty);
		}
		if ir.blocks.len() == 1 && prefilled_entry.is_some() {
			control::lower_terminator(
				builder,
				state,
				helper_refs,
				ptr_ty,
				current_exception_exit,
				&block.term,
			)?;
		} else {
			control::lower_terminator_with_blocks(
				builder,
				state,
				helper_refs,
				ptr_ty,
				current_exception_exit,
				block_map,
				ir,
				block.id,
				&block.term,
				true,
			)?;
		}
	}
	Ok(())
}

#[allow(clippy::too_many_arguments)]
fn lower_function_blocks_subset<M: Module>(
	module: &mut M,
	builder: &mut FunctionBuilder<'_>,
	helper_refs: &HelperFuncRefs,
	func_ids: &[FuncId],
	functions: &[Function],
	names: &NameMap,
	state: &mut LowerState,
	ptr_ty: ir::Type,
	ptr_bytes: usize,
	exception_exit: ir::Block,
	ir: &Function,
	block_map: &[(BlockId, ir::Block)],
	feedback_base_gv: Option<ir::GlobalValue>,
	line_cell_gv: Option<ir::GlobalValue>,
	reachable: &HashSet<BlockId>,
) -> Result<(), CodegenError> {
	let exception_entry_stacks = block_exception_entry_stacks(ir)?;
	let temp_spill = spill::TempSpillPlan::compute(builder, ir, ptr_ty)?;

	for block in &ir.blocks {
		if !reachable.contains(&block.id) {
			continue;
		}
		let clif_block = block_map
			.iter()
			.find_map(|(id, clif)| (*id == block.id).then_some(*clif))
			.ok_or(CodegenError::Unsupported("missing OSR basic block"))?;
		builder.switch_to_block(clif_block);
		let mut exception_target_stack = exception_entry_stacks
			.get(&block.id)
			.cloned()
			.unwrap_or_default();
		let mut current_exception_exit = exception_exit_from_stack(
			&exception_target_stack,
			block_map,
			exception_exit,
			"OSR exception handler target block",
		)?;
		let mut last_line = 0u32;
		for (inst_index, inst) in block.insts.iter().enumerate() {
			if let Some(plan) = &temp_spill {
				plan.emit_inst_window(builder, state, block.id, inst_index, ptr_ty);
			}
			if let Some(line_cell_gv) = line_cell_gv {
				if inst.line != 0 && inst.line != last_line {
					emit_line_store(builder, line_cell_gv, ptr_ty, inst.line);
					last_line = inst.line;
				}
			}
			let feedback_cell = match (inst.feedback_slot, feedback_base_gv) {
				(Some(slot), Some(gv)) => {
					let base = builder.ins().global_value(ptr_ty, gv);
					Some(builder.ins().iadd_imm(
						base,
						i64::from(slot.0) * pon_runtime::feedback::FEEDBACK_CELL_SIZE as i64,
					))
				},
				_ => None,
			};
			let value = match &inst.kind {
				InstKind::PushExcInfo { target, stack_depth, kind } => {
					let value = exc::lower_push_exc_info(
						builder,
						helper_refs.push_exc_info,
						target.0,
						*stack_depth,
						*kind,
						ptr_ty,
						current_exception_exit,
					)?;
					exception_target_stack.push(*target);
					current_exception_exit = exception_exit_from_stack(
						&exception_target_stack,
						block_map,
						exception_exit,
						"OSR exception handler target block",
					)?;
					value
				},
				InstKind::PopExcInfo => {
					exception_target_stack.pop();
					let previous_exception_exit = exception_exit_from_stack(
						&exception_target_stack,
						block_map,
						exception_exit,
						"OSR exception handler target block",
					)?;
					let value = exc::lower_pop_exc_info(
						builder,
						helper_refs.pop_exc_info,
						ptr_ty,
						previous_exception_exit,
					)?;
					current_exception_exit = previous_exception_exit;
					value
				},
				_ => lower_inst(
					module,
					builder,
					helper_refs,
					func_ids,
					functions,
					names,
					state,
					ptr_ty,
					ptr_bytes,
					current_exception_exit,
					&inst.kind,
					feedback_cell,
				)?,
			};
			state.define_value(inst.result, value);
		}
		if let Some(plan) = &temp_spill {
			plan.emit_term_window(builder, state, block.id, ptr_ty);
		}
		control::lower_terminator_with_blocks(
			builder,
			state,
			helper_refs,
			ptr_ty,
			current_exception_exit,
			block_map,
			ir,
			block.id,
			&block.term,
			false,
		)?;
	}
	Ok(())
}

pub(crate) fn declare_helper_refs<M: Module>(
	module: &mut M,
	helpers: &HelperRefs,
	func: &mut ir::Function,
) -> HelperFuncRefs {
	HelperFuncRefs {
		const_int: module.declare_func_in_func(helpers.const_int, func),
		const_float: module.declare_func_in_func(helpers.const_float, func),
		const_complex: module.declare_func_in_func(helpers.const_complex, func),
		const_bool: module.declare_func_in_func(helpers.const_bool, func),
		const_bytes: module.declare_func_in_func(helpers.const_bytes, func),
		const_bigint: module.declare_func_in_func(helpers.const_bigint, func),
		rich_compare: module.declare_func_in_func(helpers.rich_compare, func),
		number_unary: module.declare_func_in_func(helpers.number_unary, func),
		number_binary: module.declare_func_in_func(helpers.number_binary, func),
		number_inplace: module.declare_func_in_func(helpers.number_inplace, func),
		is_true: module.declare_func_in_func(helpers.is_true, func),
		contains: module.declare_func_in_func(helpers.contains, func),
		call: module.declare_func_in_func(helpers.call, func),
		call_ex: module.declare_func_in_func(helpers.call_ex, func),
		call_method: module.declare_func_in_func(helpers.call_method, func),
		load_global: module.declare_func_in_func(helpers.load_global, func),
		load_name: module.declare_func_in_func(helpers.load_name, func),
		load_local: module.declare_func_in_func(helpers.load_local, func),
		delete_local: module.declare_func_in_func(helpers.delete_local, func),
		delete_global: module.declare_func_in_func(helpers.delete_global, func),
		delete_name: module.declare_func_in_func(helpers.delete_name, func),
		get_attr: module.declare_func_in_func(helpers.get_attr, func),
		set_attr: module.declare_func_in_func(helpers.set_attr, func),
		del_attr: module.declare_func_in_func(helpers.del_attr, func),
		build_tuple: module.declare_func_in_func(helpers.build_tuple, func),
		build_list: module.declare_func_in_func(helpers.build_list, func),
		build_set: module.declare_func_in_func(helpers.build_set, func),
		build_slice: module.declare_func_in_func(helpers.build_slice, func),
		list_append: module.declare_func_in_func(helpers.list_append, func),
		set_add: module.declare_func_in_func(helpers.set_add, func),
		list_extend: module.declare_func_in_func(helpers.list_extend, func),
		list_to_tuple: module.declare_func_in_func(helpers.list_to_tuple, func),
		set_update: module.declare_func_in_func(helpers.set_update, func),
		unpack_seq: module.declare_func_in_func(helpers.unpack_seq, func),
		unpack_ex: module.declare_func_in_func(helpers.unpack_ex, func),
		get_len: module.declare_func_in_func(helpers.get_len, func),
		build_map: module.declare_func_in_func(helpers.build_map, func),
		map_insert: module.declare_func_in_func(helpers.map_insert, func),
		dict_merge: module.declare_func_in_func(helpers.dict_merge, func),
		dict_merge_unique: module.declare_func_in_func(helpers.dict_merge_unique, func),
		subscript_get: module.declare_func_in_func(helpers.subscript_get, func),
		subscript_set: module.declare_func_in_func(helpers.subscript_set, func),
		subscript_del: module.declare_func_in_func(helpers.subscript_del, func),
		build_string: module.declare_func_in_func(helpers.build_string, func),
		build_template: module.declare_func_in_func(helpers.build_template, func),
		load_builtin: module.declare_func_in_func(helpers.load_builtin, func),
		store_name: module.declare_func_in_func(helpers.store_name, func),
		import_name: module.declare_func_in_func(helpers.import_name, func),
		import_from: module.declare_func_in_func(helpers.import_from, func),
		import_star: module.declare_func_in_func(helpers.import_star, func),
		raise: module.declare_func_in_func(helpers.raise, func),
		reraise: module.declare_func_in_func(helpers.reraise, func),
		push_exc_info: module.declare_func_in_func(helpers.push_exc_info, func),
		pop_exc_info: module.declare_func_in_func(helpers.pop_exc_info, func),
		match_exc: module.declare_func_in_func(helpers.match_exc, func),
		check_exc_star: module.declare_func_in_func(helpers.check_exc_star, func),
		exc_star_enter: module.declare_func_in_func(helpers.exc_star_enter, func),
		exc_star_match: module.declare_func_in_func(helpers.exc_star_match, func),
		exc_star_body_ok: module.declare_func_in_func(helpers.exc_star_body_ok, func),
		exc_star_body_raised: module.declare_func_in_func(helpers.exc_star_body_raised, func),
		exc_star_finish: module.declare_func_in_func(helpers.exc_star_finish, func),
		get_current_exc: module.declare_func_in_func(helpers.get_current_exc, func),
		build_exc_group: module.declare_func_in_func(helpers.build_exc_group, func),
		get_iter: module.declare_func_in_func(helpers.get_iter, func),
		get_aiter: module.declare_func_in_func(helpers.get_aiter, func),
		for_next: module.declare_func_in_func(helpers.for_next, func),
		gen_stop_value: module.declare_func_in_func(helpers.gen_stop_value, func),
		gen_last_stop_value: module.declare_func_in_func(helpers.gen_last_stop_value, func),
		gen_frame_alloc: module.declare_func_in_func(helpers.gen_frame_alloc, func),
		make_generator: module.declare_func_in_func(helpers.make_generator, func),
		gen_consume_payload: module.declare_func_in_func(helpers.gen_consume_payload, func),
		gen_finish: module.declare_func_in_func(helpers.gen_finish, func),
		gen_unwind: module.declare_func_in_func(helpers.gen_unwind, func),
		gen_delegate_step: module.declare_func_in_func(helpers.gen_delegate_step, func),
		await_value: module.declare_func_in_func(helpers.await_value, func),
		match_sequence: module.declare_func_in_func(helpers.match_sequence, func),
		match_mapping: module.declare_func_in_func(helpers.match_mapping, func),
		match_class: module.declare_func_in_func(helpers.match_class, func),
		match_keys: module.declare_func_in_func(helpers.match_keys, func),
		match_len_ge: module.declare_func_in_func(helpers.match_len_ge, func),
		make_function: module.declare_func_in_func(helpers.make_function, func),
		make_function_full: module.declare_func_in_func(helpers.make_function_full, func),
		function_set_closure: module.declare_func_in_func(helpers.function_set_closure, func),
		make_cell: module.declare_func_in_func(helpers.make_cell, func),
		cell_get: module.declare_func_in_func(helpers.cell_get, func),
		cell_set: module.declare_func_in_func(helpers.cell_set, func),
		cell_delete: module.declare_func_in_func(helpers.cell_delete, func),
		current_closure_cell: module.declare_func_in_func(helpers.current_closure_cell, func),
		function_set_annotate: module.declare_func_in_func(helpers.function_set_annotate, func),
		make_type_alias: module.declare_func_in_func(helpers.make_type_alias, func),
		make_typevar: module.declare_func_in_func(helpers.make_typevar, func),
		setup_annotations: module.declare_func_in_func(helpers.setup_annotations, func),
		build_class: module.declare_func_in_func(helpers.build_class, func),
		build_class_full: module.declare_func_in_func(helpers.build_class_full, func),
		load_build_class: module.declare_func_in_func(helpers.load_build_class, func),
		store_global: module.declare_func_in_func(helpers.store_global, func),
		none: module.declare_func_in_func(helpers.none, func),
		osr_poll: module.declare_func_in_func(helpers.osr_poll, func),
		deopt_note: module.declare_func_in_func(helpers.deopt_note, func),
		#[cfg(feature = "free-threading")]
		safepoint_poll: module.declare_func_in_func(helpers.safepoint_poll, func),
		#[cfg(feature = "free-threading")]
		gc_write_barrier: module.declare_func_in_func(helpers.gc_write_barrier, func),
	}
}

/// The argv-slot -> local-slot permutation produced by runtime argument
/// binding (`(argv_slot, local_slot)` pairs, in argv order).
///
/// Shared by [`initialize_parameter_locals`] (normal functions define locals
/// from argv) and the generator stub (stores bound arguments into frame
/// parameter slots) so the two can never drift.
pub(crate) fn parameter_bindings(
	function: &Function,
	entry_arg_count: usize,
) -> Vec<(usize, usize)> {
	if function.params.total_slot_count() == 0 {
		return (0..entry_arg_count).map(|slot| (slot, slot)).collect();
	}

	let positional = function.params.positional_arity();
	let keyword_only_local = positional + usize::from(function.params.vararg_name.is_some());
	let mut bindings: Vec<(usize, usize)> = (0..positional).map(|slot| (slot, slot)).collect();

	let mut argv_slot = positional;
	for index in 0..function.params.keyword_only_count {
		bindings.push((argv_slot, keyword_only_local + index));
		argv_slot += 1;
	}
	if function.params.vararg_name.is_some() {
		bindings.push((argv_slot, positional));
		argv_slot += 1;
	}
	if function.params.kwarg_name.is_some() {
		bindings.push((argv_slot, keyword_only_local + function.params.keyword_only_count));
	}
	bindings
}

pub(crate) fn initialize_parameter_locals(
	builder: &mut FunctionBuilder<'_>,
	state: &mut LowerState,
	argv: ir::Value,
	ptr_bytes: usize,
	entry_arg_count: usize,
	function: &Function,
	ptr_ty: ir::Type,
) -> Result<(), CodegenError> {
	if entry_arg_count > function.n_locals {
		return Err(CodegenError::LocalOutOfRange {
			slot:     entry_arg_count as u32,
			n_locals: function.n_locals,
		});
	}
	for (argv_slot, local_slot) in parameter_bindings(function, entry_arg_count) {
		define_local_from_argv(builder, state, argv, ptr_bytes, ptr_ty, argv_slot, local_slot)?;
	}
	Ok(())
}

fn define_local_from_argv(
	builder: &mut FunctionBuilder<'_>,
	state: &mut LowerState,
	argv: ir::Value,
	ptr_bytes: usize,
	ptr_ty: ir::Type,
	argv_slot: usize,
	local_slot: usize,
) -> Result<(), CodegenError> {
	let offset = offset_i32(argv_slot * ptr_bytes)?;
	let value = builder
		.ins()
		.load(ptr_ty, MemFlagsData::new(), argv, offset);
	store_local(builder, state, local_slot as u32, value)?;
	Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_inst<M: Module>(
	module: &mut M,
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	func_ids: &[FuncId],
	functions: &[Function],
	names: &NameMap,
	state: &mut LowerState,
	ptr_ty: ir::Type,
	ptr_bytes: usize,
	exception_exit: ir::Block,
	kind: &InstKind,
	feedback_cell: Option<ir::Value>,
) -> Result<ir::Value, CodegenError> {
	match kind {
		InstKind::Const(PyConst::Int(value)) => {
			number::lower_const_int(builder, helpers, *value, ptr_ty, exception_exit)
		},
		InstKind::Const(PyConst::Str(value)) => {
			strings::lower_const_str(module, builder, helpers, value, ptr_ty, exception_exit)
		},
		InstKind::Const(PyConst::None) => {
			control::lower_const_none(builder, helpers, ptr_ty, exception_exit)
		},
		InstKind::Const(PyConst::Bool(value)) => {
			number::lower_const_bool(builder, helpers, *value, ptr_ty, exception_exit)
		},
		InstKind::Const(PyConst::Float(value)) => {
			number::lower_const_float(builder, helpers, *value, ptr_ty, exception_exit)
		},
		InstKind::Const(PyConst::Complex { real, imag }) => {
			number::lower_const_complex(builder, helpers, *real, *imag, ptr_ty, exception_exit)
		},
		InstKind::Const(PyConst::Bytes(value)) => {
			strings::lower_const_bytes(module, builder, helpers, value, ptr_ty, exception_exit)
		},
		InstKind::Const(PyConst::BigInt(value)) => {
			number::lower_const_bigint(module, builder, helpers, value, ptr_ty, exception_exit)
		},
		InstKind::Const(_) => control::lower_future_value("Const(non Phase-A literal)"),
		InstKind::ConstRef(_) => control::lower_future_value("ConstRef"),
		InstKind::BuildTuple { elts } => container::lower_build_tuple(
			builder,
			helpers.build_tuple,
			helpers,
			state,
			elts,
			ptr_ty,
			ptr_bytes,
			exception_exit,
		),
		InstKind::BuildList { elts } => container::lower_build_list(
			builder,
			helpers.build_list,
			helpers,
			state,
			elts,
			ptr_ty,
			ptr_bytes,
			exception_exit,
		),
		InstKind::BuildSet { elts } => container::lower_build_set(
			builder,
			helpers.build_set,
			helpers,
			state,
			elts,
			ptr_ty,
			ptr_bytes,
			exception_exit,
		),
		InstKind::BuildMap { pairs } => mapping::lower_build_map_with_helper(
			builder,
			helpers.build_map,
			helpers,
			state,
			pairs,
			ptr_ty,
			ptr_bytes,
			exception_exit,
		),
		InstKind::BuildSlice { lower, upper, step } => container::lower_build_slice(
			builder,
			helpers.build_slice,
			state,
			*lower,
			*upper,
			*step,
			ptr_ty,
			exception_exit,
		),
		InstKind::BuildString { parts } => strings::lower_build_string(
			module,
			builder,
			helpers.build_string,
			state,
			parts,
			ptr_ty,
			exception_exit,
		),
		InstKind::BuildTemplate { parts } => strings::lower_build_template(
			module,
			builder,
			helpers.build_template,
			state,
			parts,
			ptr_ty,
			exception_exit,
		),
		InstKind::ListAppend { list, item } => container::lower_list_append(
			builder,
			helpers.list_append,
			state,
			*list,
			*item,
			ptr_ty,
			exception_exit,
		),
		InstKind::SetAdd { set, item } => container::lower_set_add(
			builder,
			helpers.set_add,
			state,
			*set,
			*item,
			ptr_ty,
			exception_exit,
		),
		InstKind::MapInsert { map, key, val } => mapping::lower_map_insert_with_helper(
			builder,
			helpers.map_insert,
			state,
			*map,
			*key,
			*val,
			ptr_ty,
			exception_exit,
		),
		InstKind::ListExtend { list, iter } => container::lower_list_extend(
			builder,
			helpers.list_extend,
			state,
			*list,
			*iter,
			ptr_ty,
			exception_exit,
		),
		InstKind::ListToTuple { list } => container::lower_list_to_tuple(
			builder,
			helpers.list_to_tuple,
			state,
			*list,
			ptr_ty,
			exception_exit,
		),
		InstKind::SetUpdate { set, iter } => container::lower_set_update(
			builder,
			helpers.set_update,
			state,
			*set,
			*iter,
			ptr_ty,
			exception_exit,
		),
		InstKind::DictMerge { map, other } => mapping::lower_dict_merge_with_helper(
			builder,
			helpers.dict_merge,
			state,
			*map,
			*other,
			ptr_ty,
			exception_exit,
		),
		InstKind::DictMergeUnique { map, other } => mapping::lower_dict_merge_with_helper(
			builder,
			helpers.dict_merge_unique,
			state,
			*map,
			*other,
			ptr_ty,
			exception_exit,
		),
		InstKind::LoadLocal(slot) => {
			name::lower_load_local(builder, helpers, state, slot.0, ptr_ty, exception_exit)
		},
		InstKind::StoreLocal(slot, value) => name::lower_store_local(builder, state, slot.0, *value),
		InstKind::DeleteLocal(slot) => {
			name::lower_delete_local(builder, helpers, state, slot.0, ptr_ty, exception_exit)
		},
		InstKind::LoadGlobal(name) => name::lower_load_global(
			builder,
			helpers,
			names,
			name.0,
			ptr_ty,
			exception_exit,
			feedback_cell,
		),
		InstKind::StoreGlobal(name, value) => name::lower_store_global(
			builder,
			helpers,
			names,
			state,
			name.0,
			*value,
			ptr_ty,
			exception_exit,
		),
		InstKind::DeleteGlobal(name) => {
			name::lower_delete_global(builder, helpers, names, name.0, ptr_ty, exception_exit)
		},
		InstKind::LoadName(name) => {
			name::lower_load_name(builder, helpers, names, name.0, ptr_ty, exception_exit)
		},
		InstKind::StoreName(name, value) => name::lower_store_name(
			builder,
			helpers,
			names,
			state,
			name.0,
			*value,
			ptr_ty,
			exception_exit,
		),
		InstKind::DeleteName(name) => {
			name::lower_delete_name(builder, helpers, names, name.0, ptr_ty, exception_exit)
		},
		InstKind::LoadCell(cell) => {
			name::lower_load_cell(builder, helpers, state, cell.0, ptr_ty, exception_exit)
		},
		InstKind::StoreCell(cell, value) => {
			name::lower_store_cell(builder, helpers, state, cell.0, *value, ptr_ty, exception_exit)
		},
		InstKind::DeleteCell(cell) => {
			name::lower_delete_cell(builder, helpers, state, cell.0, ptr_ty, exception_exit)
		},
		InstKind::MakeCell(local) => {
			name::lower_make_cell(builder, helpers, state, local.0, ptr_ty, exception_exit)
		},
		InstKind::LoadClosure(cell) => {
			name::lower_load_closure(builder, helpers, state, cell.0, ptr_ty, exception_exit)
		},
		InstKind::LoadBuiltin(name_id) => {
			name::lower_load_builtin(builder, helpers, names, name_id.0, ptr_ty, exception_exit)
		},
		InstKind::BinaryOp { op, lhs, rhs } => {
			number::lower_binary_op(builder, helpers, state, *op, *lhs, *rhs, ptr_ty, exception_exit)
		},
		InstKind::InplaceOp { op, lhs, rhs } => {
			number::lower_inplace_op(builder, helpers, state, *op, *lhs, *rhs, ptr_ty, exception_exit)
		},
		InstKind::UnaryOp { op, operand } => number::lower_unary_op(
			builder,
			helpers.number_unary,
			state,
			*op,
			*operand,
			ptr_ty,
			exception_exit,
		),
		InstKind::Compare { op, lhs, rhs } => compare::lower_compare_op(
			builder,
			helpers.rich_compare,
			state,
			*op,
			*lhs,
			*rhs,
			ptr_ty,
			exception_exit,
		),
		InstKind::Contains { item, container, negate } => compare::lower_contains_op(
			builder,
			helpers.contains,
			helpers.const_bool,
			state,
			*item,
			*container,
			*negate,
			ptr_ty,
			exception_exit,
		),
		InstKind::Is { lhs, rhs, negate } => compare::lower_is_op(
			builder,
			helpers.const_bool,
			state,
			*lhs,
			*rhs,
			*negate,
			ptr_ty,
			exception_exit,
		),
		InstKind::BoolTest { val } => compare::lower_bool_test_op(
			builder,
			helpers.is_true,
			helpers.const_bool,
			state,
			*val,
			ptr_ty,
			exception_exit,
		),
		InstKind::Not { val } => compare::lower_not_op(
			builder,
			helpers.is_true,
			helpers.const_bool,
			state,
			*val,
			ptr_ty,
			exception_exit,
		),
		InstKind::LoadAttr { obj, name } => attr::lower_load_attr(
			builder,
			helpers,
			names,
			state,
			*obj,
			name.0,
			ptr_ty,
			exception_exit,
			feedback_cell,
		),
		InstKind::StoreAttr { obj, name, val } => attr::lower_store_attr(
			builder,
			helpers,
			names,
			state,
			*obj,
			name.0,
			*val,
			ptr_ty,
			exception_exit,
		),
		InstKind::DeleteAttr { obj, name } => attr::lower_delete_attr(
			builder,
			helpers,
			names,
			state,
			*obj,
			name.0,
			ptr_ty,
			exception_exit,
		),
		InstKind::LoadMethod { obj, name } => attr::lower_load_method(
			builder,
			helpers,
			names,
			state,
			*obj,
			name.0,
			ptr_ty,
			exception_exit,
			feedback_cell,
		),
		InstKind::SubscriptGet { obj, index } => mapping::lower_subscript_get_with_helper(
			builder,
			helpers.subscript_get,
			state,
			*obj,
			*index,
			ptr_ty,
			exception_exit,
		),
		InstKind::SubscriptSet { obj, index, val } => mapping::lower_subscript_set_with_helper(
			builder,
			helpers.subscript_set,
			state,
			*obj,
			*index,
			*val,
			ptr_ty,
			exception_exit,
		),
		InstKind::SubscriptDel { obj, index } => mapping::lower_subscript_del_with_helper(
			builder,
			helpers.subscript_del,
			state,
			*obj,
			*index,
			ptr_ty,
			exception_exit,
		),
		InstKind::Call { callee, args } => {
			call::lower_call(builder, helpers, state, *callee, args, ptr_ty, ptr_bytes, exception_exit)
		},
		InstKind::CallEx { callee, args, star, kwargs, dstar } => call::lower_call_ex(
			builder,
			helpers,
			names,
			state,
			call::CallExArgs { callee: *callee, args, star: *star, kwargs, dstar: *dstar },
			ptr_ty,
			ptr_bytes,
			exception_exit,
			feedback_cell,
		),
		InstKind::CallMethod { recv_pair, args } => call::lower_call_method(
			builder,
			helpers,
			state,
			call::CallMethodArgs { recv_pair: *recv_pair, args },
			ptr_ty,
			ptr_bytes,
			exception_exit,
			feedback_cell,
		),
		InstKind::GetIter { iterable } => {
			r#gen::lower_get_iter(builder, helpers.get_iter, state, *iterable, ptr_ty, exception_exit)
		},
		InstKind::GetAIter { iterable } => r#gen::lower_get_aiter(
			builder,
			helpers.get_aiter,
			state,
			*iterable,
			ptr_ty,
			exception_exit,
		),
		InstKind::ForNext { iter } => {
			r#gen::lower_for_next(builder, helpers.for_next, state, *iter, ptr_ty, exception_exit)
		},
		InstKind::UnpackSeq { val, n } => container::lower_unpack_seq(
			builder,
			helpers.unpack_seq,
			state,
			*val,
			*n,
			ptr_ty,
			exception_exit,
		),
		InstKind::UnpackEx { val, before, after } => container::lower_unpack_ex(
			builder,
			helpers.unpack_ex,
			state,
			*val,
			*before,
			*after,
			ptr_ty,
			exception_exit,
		),
		InstKind::Yield { .. } | InstKind::YieldFrom { .. } => Err(CodegenError::Unsupported(
			"raw yield marker reached codegen; generator state-machine transform must run first",
		)),
		InstKind::Await { awaitable } => {
			r#gen::lower_await(builder, helpers.await_value, state, *awaitable, ptr_ty, exception_exit)
		},
		InstKind::GenResumePayload => {
			r#gen::lower_gen_resume_payload(builder, helpers, state, ptr_ty, exception_exit)
		},
		InstKind::GenDelegateStep { delegate } => {
			r#gen::lower_gen_delegate_step(builder, helpers, state, *delegate)
		},
		InstKind::GenLastStopValue => {
			r#gen::lower_gen_last_stop_value(builder, helpers, ptr_ty, exception_exit)
		},
		InstKind::Raise { exc, cause } => exc::lower_raise(
			builder,
			helpers.raise,
			helpers.reraise,
			state,
			*exc,
			*cause,
			ptr_ty,
			exception_exit,
		),
		InstKind::Reraise => exc::lower_reraise(builder, helpers.reraise, ptr_ty, exception_exit),
		InstKind::PushExcInfo { target, stack_depth, kind } => exc::lower_push_exc_info(
			builder,
			helpers.push_exc_info,
			target.0,
			*stack_depth,
			*kind,
			ptr_ty,
			exception_exit,
		),
		InstKind::PopExcInfo => {
			exc::lower_pop_exc_info(builder, helpers.pop_exc_info, ptr_ty, exception_exit)
		},
		InstKind::MatchExc { exc_type } => {
			exc::lower_match_exc(builder, helpers.match_exc, state, *exc_type, ptr_ty, exception_exit)
		},
		InstKind::CheckExcStar { exc_types } => exc::lower_check_exc_star(
			builder,
			helpers.check_exc_star,
			state,
			*exc_types,
			ptr_ty,
			exception_exit,
		),
		InstKind::ExcStarEnter => {
			exc::lower_exc_star_enter(builder, helpers.exc_star_enter, ptr_ty, exception_exit)
		},
		InstKind::ExcStarMatch { exc_types } => exc::lower_exc_star_match(
			builder,
			helpers.exc_star_match,
			state,
			*exc_types,
			ptr_ty,
			exception_exit,
		),
		InstKind::ExcStarBodyOk => {
			exc::lower_exc_star_body_ok(builder, helpers.exc_star_body_ok, ptr_ty, exception_exit)
		},
		InstKind::ExcStarBodyRaised => exc::lower_exc_star_body_raised(
			builder,
			helpers.exc_star_body_raised,
			ptr_ty,
			exception_exit,
		),
		InstKind::ExcStarFinish => {
			exc::lower_exc_star_finish(builder, helpers.exc_star_finish, ptr_ty, exception_exit)
		},
		InstKind::GetCurrentExc => {
			exc::lower_get_current_exc(builder, helpers.get_current_exc, ptr_ty, exception_exit)
		},
		InstKind::BuildExcGroup { excs } => exc::lower_build_exc_group(
			builder,
			helpers.build_exc_group,
			helpers,
			state,
			excs,
			ptr_ty,
			ptr_bytes,
			exception_exit,
		),
		InstKind::MatchSequence { subj } => match_::lower_match_sequence(
			builder,
			helpers.match_sequence,
			state,
			*subj,
			ptr_ty,
			exception_exit,
		),
		InstKind::MatchMapping { subj } => match_::lower_match_mapping(
			builder,
			helpers.match_mapping,
			state,
			*subj,
			ptr_ty,
			exception_exit,
		),
		InstKind::MatchClass { subj, cls, nargs, kw } => match_::lower_match_class(
			builder,
			helpers.match_class,
			names,
			state,
			*subj,
			*cls,
			*nargs,
			kw,
			ptr_ty,
			exception_exit,
		),
		InstKind::MatchKeys { subj, keys } => match_::lower_match_keys(
			builder,
			helpers.match_keys,
			helpers,
			state,
			*subj,
			keys,
			ptr_ty,
			ptr_bytes,
			exception_exit,
		),
		InstKind::GetLen { subj } => {
			container::lower_get_len(builder, helpers.get_len, state, *subj, ptr_ty, exception_exit)
		},
		InstKind::MatchLenGe { subj, n, exact } => match_::lower_match_len_ge(
			builder,
			helpers.match_len_ge,
			state,
			*subj,
			*n,
			*exact,
			ptr_ty,
			exception_exit,
		),
		InstKind::ImportName { name, fromlist, level } => name::lower_import_name_call(
			builder,
			helpers.import_name,
			names,
			name.0,
			fromlist,
			*level,
			ptr_ty,
			ptr_bytes,
			exception_exit,
		),
		InstKind::ImportFrom { module, name } => name::lower_import_from_call(
			builder,
			helpers.import_from,
			names,
			state,
			*module,
			name.0,
			ptr_ty,
			exception_exit,
		),
		InstKind::ImportStar { module } => name::lower_import_star_call(
			builder,
			helpers.import_star,
			state,
			*module,
			ptr_ty,
			exception_exit,
		),
		InstKind::BuildClass {
			body,
			name,
			bases,
			bases_seq,
			keywords,
			dstar,
			decorators,
			closure,
		} => call::lower_build_class(
			module,
			builder,
			helpers,
			func_ids,
			functions,
			names,
			state,
			call::BuildClassArgs {
				body: *body,
				name: *name,
				bases,
				bases_seq: *bases_seq,
				keywords,
				dstar: *dstar,
				decorators,
				closure,
			},
			ptr_ty,
			ptr_bytes,
			exception_exit,
		),
		InstKind::MakeFunction { func_index, name_interned, arity } => call::lower_make_function(
			module,
			builder,
			helpers,
			func_ids,
			names,
			*func_index,
			name_interned.0,
			*arity,
			ptr_ty,
			exception_exit,
		),
		InstKind::MakeFunctionFull { code, defaults, kwdefaults, closure, annotations } => {
			call::lower_make_function_full(
				module,
				builder,
				helpers,
				func_ids,
				functions,
				names,
				state,
				call::MakeFunctionFullArgs { code: *code, defaults, kwdefaults, closure, annotations },
				ptr_ty,
				ptr_bytes,
				exception_exit,
			)
		},
		InstKind::SetupAnnotations => {
			name::lower_setup_annotations(builder, helpers.setup_annotations, ptr_ty, exception_exit)
		},
		InstKind::LoadBuildClass => {
			name::lower_load_build_class(builder, helpers.load_build_class, ptr_ty, exception_exit)
		},
		InstKind::FunctionSetAnnotate { function, annotate } => {
			let function = state.value(*function)?;
			let annotate = state.value(*annotate)?;
			Ok(call_pyobject_helper(
				builder,
				helpers.function_set_annotate,
				&[function, annotate],
				ptr_ty,
				exception_exit,
			))
		},
		InstKind::MakeTypeAlias { name, thunk } => {
			let runtime_name = builder
				.ins()
				.iconst(ir::types::I32, i64::from(names.runtime_id(name.0)?));
			let thunk = state.value(*thunk)?;
			Ok(call_pyobject_helper(
				builder,
				helpers.make_type_alias,
				&[runtime_name, thunk],
				ptr_ty,
				exception_exit,
			))
		},
		InstKind::MakeTypeVar { name } => {
			let runtime_name = builder
				.ins()
				.iconst(ir::types::I32, i64::from(names.runtime_id(name.0)?));
			Ok(call_pyobject_helper(
				builder,
				helpers.make_typevar,
				&[runtime_name],
				ptr_ty,
				exception_exit,
			))
		},
		_ => control::lower_future_value("unknown future InstKind"),
	}
}

pub(crate) fn load_local(
	builder: &mut FunctionBuilder<'_>,
	state: &LowerState,
	slot: u32,
) -> Result<ir::Value, CodegenError> {
	let index = slot as usize;
	if index >= state.locals.len() {
		return Err(CodegenError::LocalOutOfRange { slot, n_locals: state.locals.len() });
	}
	if !state.local_defined[index] {
		return Err(CodegenError::LocalUsedBeforeDefinition { slot });
	}
	Ok(builder.use_var(state.locals[index]))
}

pub(crate) fn store_local(
	builder: &mut FunctionBuilder<'_>,
	state: &mut LowerState,
	slot: u32,
	value: ir::Value,
) -> Result<(), CodegenError> {
	let index = slot as usize;
	if index >= state.locals.len() {
		return Err(CodegenError::LocalOutOfRange { slot, n_locals: state.locals.len() });
	}
	// PHASE-E: WriteBarrier
	builder.def_var(state.locals[index], value);
	// Mirror into the local's shadow frame slot: the conservative stack scan
	// roots live locals through frame memory (see `LowerState::local_shadow`).
	builder
		.ins()
		.stack_store(value, state.local_shadow[index], 0);
	state.local_defined[index] = true;
	Ok(())
}

pub(crate) fn declare_string_data<M: Module>(
	module: &mut M,
	builder: &mut FunctionBuilder<'_>,
	value: &str,
	ptr_ty: ir::Type,
) -> Result<ir::Value, CodegenError> {
	declare_bytes_data(module, builder, value.as_bytes(), ptr_ty)
}

pub(crate) fn declare_bytes_data<M: Module>(
	module: &mut M,
	builder: &mut FunctionBuilder<'_>,
	value: &[u8],
	ptr_ty: ir::Type,
) -> Result<ir::Value, CodegenError> {
	let data_id = module.declare_anonymous_data(false, false)?;
	let mut data = DataDescription::new();
	data.set_align(1);
	if value.is_empty() {
		data.define(vec![0_u8].into_boxed_slice());
	} else {
		data.define(value.to_vec().into_boxed_slice());
	}
	module.define_data(data_id, &data)?;
	let global = module.declare_data_in_func(data_id, builder.func);
	Ok(builder.ins().global_value(ptr_ty, global))
}
/// J0.3: declare one writable, zero-initialized static `FeedbackCell` array
/// covering a function's lowering-assigned feedback slots (`None` when the
/// function has no specializable sites).  Tier-0 helper calls pass
/// `base + slot * FEEDBACK_CELL_SIZE`; the cells live exactly as long as the
/// emitted code that references them (same JIT/AoT module).
pub(crate) fn declare_feedback_cells<M: Module>(
	module: &mut M,
	ir: &Function,
) -> Result<Option<DataId>, CodegenError> {
	let slots = ir
		.blocks
		.iter()
		.flat_map(|block| block.insts.iter())
		.filter_map(|inst| inst.feedback_slot)
		.map(|slot| slot.0 as usize + 1)
		.max()
		.unwrap_or(0);
	if slots == 0 {
		return Ok(None);
	}
	let data_id = module.declare_anonymous_data(true, false)?;
	let mut data = DataDescription::new();
	data.set_align(align_of::<pon_runtime::feedback::FeedbackCell>() as u64);
	data.define_zeroinit(slots * pon_runtime::feedback::FEEDBACK_CELL_SIZE);
	module.define_data(data_id, &data)?;
	Ok(Some(data_id))
}

/// Declares the imported `pon_current_line` runtime cell and returns its
/// per-function [`ir::GlobalValue`] when `ir` stamps any statement line.
/// Line-free IR (hand-built or lowered without source text) gets `None`, which
/// keeps its emitted code byte-identical to pre-line-plumbing output.
pub(crate) fn declare_line_cell_gv<M: Module>(
	module: &mut M,
	ir: &Function,
	func: &mut ir::Function,
) -> Result<Option<ir::GlobalValue>, CodegenError> {
	let has_lines = ir
		.blocks
		.iter()
		.any(|block| block.insts.iter().any(|inst| inst.line != 0));
	if !has_lines {
		return Ok(None);
	}
	let data_id =
		module.declare_data(pon_runtime::abi::CURRENT_LINE_SYMBOL, Linkage::Import, true, false)?;
	Ok(Some(module.declare_data_in_func(data_id, func)))
}

/// Records a statement-line transition: one direct `i32` store of `line` into
/// the imported `pon_current_line` cell (see `pon_runtime::abi`).  A direct
/// store beats a `pon_set_line` helper call here: no caller-saved register
/// clobbers around it and three machine instructions per transition, emitted
/// only when consecutive instructions disagree on their statement line.
pub(crate) fn emit_line_store(
	builder: &mut FunctionBuilder<'_>,
	line_cell_gv: ir::GlobalValue,
	ptr_ty: ir::Type,
	line: u32,
) {
	let address = builder.ins().global_value(ptr_ty, line_cell_gv);
	let value = builder.ins().iconst(ir::types::I32, i64::from(line));
	builder.ins().store(MemFlagsData::new(), value, address, 0);
}

/// A pointer-carrying `ExplicitSlot` array built for one consuming helper call.
///
/// `addr` is the array base passed to the helper.  `scrub` names the backing
/// stack slot and its byte length so the call site can zero the words after
/// the callee returns (helpers copy what they keep before returning).  Without
/// that scrub, the dead copies stay in the frame for the rest of the enclosing
/// function and the AoT conservative stack scan keeps re-rooting them, so a
/// `del`'d object is falsely retained and its weakrefs never clear
/// (aot-parity `weakref_basic`).  Empty arrays carry no slot.
pub(crate) struct PyObjectArray {
	pub(crate) addr: ir::Value,
	scrub:           Option<(ir::StackSlot, usize)>,
}

impl PyObjectArray {
	/// Wraps an already-populated slot holding `bytes` bytes of call input.
	pub(crate) fn slot(addr: ir::Value, slot: ir::StackSlot, bytes: usize) -> Self {
		Self { addr, scrub: Some((slot, bytes)) }
	}

	/// An empty array: NULL base pointer, nothing to scrub.
	pub(crate) fn empty(builder: &mut FunctionBuilder<'_>, ptr_ty: ir::Type) -> Self {
		Self { addr: builder.ins().iconst(ptr_ty, 0), scrub: None }
	}

	/// Emits stores clearing every pointer-sized word of the backing slot.
	fn emit_scrub(
		&self,
		builder: &mut FunctionBuilder<'_>,
		ptr_ty: ir::Type,
		ptr_bytes: usize,
	) -> Result<(), CodegenError> {
		let Some((slot, bytes)) = self.scrub else {
			return Ok(());
		};
		let zero = builder.ins().iconst(ptr_ty, 0);
		let mut offset = 0;
		while offset + ptr_bytes <= bytes {
			builder.ins().stack_store(zero, slot, offset_i32(offset)?);
			offset += ptr_bytes;
		}
		Ok(())
	}
}

pub(crate) fn build_call_argv(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	state: &LowerState,
	args: &[IrValue],
	ptr_ty: ir::Type,
	ptr_bytes: usize,
) -> Result<PyObjectArray, CodegenError> {
	if args.is_empty() {
		return Ok(PyObjectArray::empty(builder, ptr_ty));
	}

	let size = args
		.len()
		.checked_mul(ptr_bytes)
		.ok_or(CodegenError::OffsetTooLarge { offset: usize::MAX })?;
	let slot = builder.create_sized_stack_slot(StackSlotData {
		kind:        StackSlotKind::ExplicitSlot,
		size:        size
			.try_into()
			.map_err(|_| CodegenError::OffsetTooLarge { offset: size })?,
		align_shift: ptr_bytes.trailing_zeros() as u8,
		key:         None,
	});
	for (index, arg) in args.iter().enumerate() {
		let value = state.value(*arg)?;
		let offset = offset_i32(index * ptr_bytes)?;
		store_stack_pyobject(builder, helpers, slot, offset, value, ptr_ty);
	}
	let addr = builder.ins().stack_addr(ptr_ty, slot, 0);
	Ok(PyObjectArray::slot(addr, slot, size))
}

fn store_stack_pyobject(
	builder: &mut FunctionBuilder<'_>,
	helpers: &HelperFuncRefs,
	slot: ir::StackSlot,
	offset: i32,
	value: ir::Value,
	ptr_ty: ir::Type,
) {
	builder.ins().stack_store(value, slot, offset);

	#[cfg(feature = "free-threading")]
	{
		let slot_addr = builder.ins().stack_addr(ptr_ty, slot, offset);
		builder
			.ins()
			.call(helpers.gc_write_barrier, &[slot_addr, value]);
	}

	#[cfg(not(feature = "free-threading"))]
	{
		let _ = (helpers, ptr_ty);
	}
}

#[cfg(feature = "free-threading")]
pub(crate) fn emit_safepoint_poll(builder: &mut FunctionBuilder<'_>, helpers: &HelperFuncRefs) {
	builder.ins().call(helpers.safepoint_poll, &[]);
}

#[cfg(not(feature = "free-threading"))]
pub(crate) fn emit_safepoint_poll(_builder: &mut FunctionBuilder<'_>, _helpers: &HelperFuncRefs) {}

pub(crate) fn call_pyobject_helper(
	builder: &mut FunctionBuilder<'_>,
	helper: FuncRef,
	args: &[ir::Value],
	ptr_ty: ir::Type,
	exception_exit: ir::Block,
) -> ir::Value {
	// PHASE-D: stack-map safepoint
	let call = builder.ins().call(helper, args);
	let result = builder.func.dfg.inst_results(call)[0];
	emit_null_check(builder, result, ptr_ty, exception_exit);
	result
}

/// [`call_pyobject_helper`] for helpers that consume stack-slot arrays.
///
/// Zeroes every array's backing slot between the call and its NULL check, so
/// the dead copies are cleared on the success AND exception edges before
/// control can reach another collection point.  See [`PyObjectArray`].
pub(crate) fn call_pyobject_helper_consuming(
	builder: &mut FunctionBuilder<'_>,
	helper: FuncRef,
	args: &[ir::Value],
	arrays: &[&PyObjectArray],
	ptr_ty: ir::Type,
	ptr_bytes: usize,
	exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
	// PHASE-D: stack-map safepoint
	let call = builder.ins().call(helper, args);
	let result = builder.func.dfg.inst_results(call)[0];
	for array in arrays {
		array.emit_scrub(builder, ptr_ty, ptr_bytes)?;
	}
	emit_null_check(builder, result, ptr_ty, exception_exit);
	Ok(result)
}

fn emit_null_check(
	builder: &mut FunctionBuilder<'_>,
	value: ir::Value,
	_ptr_ty: ir::Type,
	exception_exit: ir::Block,
) {
	let continue_block = builder.create_block();
	let non_null = builder.ins().icmp_imm(IntCC::NotEqual, value, 0);
	builder
		.ins()
		.brif(non_null, continue_block, &[], exception_exit, &[]);
	builder.switch_to_block(continue_block);
	builder.seal_block(continue_block);
}

pub(crate) fn offset_i32(offset: usize) -> Result<i32, CodegenError> {
	i32::try_from(offset).map_err(|_| CodegenError::OffsetTooLarge { offset })
}

/// Canonical SSA suffix for a loop-header OSR transfer.
///
/// Locals are carried separately by slot number; this returns only SSA values
/// live-in at `header`, sorted by `Value` index.
#[must_use]
pub fn osr_live_values(function: &Function, header: BlockId) -> Vec<IrValue> {
	let mut gens = HashMap::<BlockId, HashSet<IrValue>>::new();
	let mut kill = HashMap::<BlockId, HashSet<IrValue>>::new();
	for block in &function.blocks {
		let mut block_gen = HashSet::new();
		let mut block_kill = HashSet::new();
		for inst in &block.insts {
			for operand in crate::region::inst_operands(&inst.kind) {
				if !block_kill.contains(&operand) {
					block_gen.insert(operand);
				}
			}
			block_kill.insert(inst.result);
		}
		for operand in crate::region::terminator_operands(&block.term) {
			if !block_kill.contains(&operand) {
				block_gen.insert(operand);
			}
		}
		gens.insert(block.id, block_gen);
		kill.insert(block.id, block_kill);
	}

	let mut live_in = function
		.blocks
		.iter()
		.map(|block| (block.id, HashSet::<IrValue>::new()))
		.collect::<HashMap<_, _>>();
	let mut changed = true;
	while changed {
		changed = false;
		for block in function.blocks.iter().rev() {
			let mut out = HashSet::new();
			for successor in successors(&block.term) {
				if let Some(input) = live_in.get(&successor) {
					out.extend(input.iter().copied());
				}
			}
			let mut next = gens.get(&block.id).cloned().unwrap_or_default();
			for value in out {
				if !kill.get(&block.id).is_some_and(|set| set.contains(&value)) {
					next.insert(value);
				}
			}
			let entry = live_in.entry(block.id).or_default();
			if *entry != next {
				*entry = next;
				changed = true;
			}
		}
	}

	let mut values = live_in
		.remove(&header)
		.unwrap_or_default()
		.into_iter()
		.collect::<Vec<_>>();
	values.sort_by_key(|value| value.0);
	values
}

fn reachable_from(function: &Function, entry: BlockId) -> HashSet<BlockId> {
	let mut reachable = HashSet::new();
	let mut queue = VecDeque::new();
	reachable.insert(entry);
	queue.push_back(entry);
	while let Some(block_id) = queue.pop_front() {
		let Some(block) = function.blocks.iter().find(|block| block.id == block_id) else {
			continue;
		};
		for successor in successors(&block.term) {
			if reachable.insert(successor) {
				queue.push_back(successor);
			}
		}
	}
	reachable
}

pub(crate) fn successors(term: &Terminator) -> Vec<BlockId> {
	match term {
		Terminator::Jump(target) => vec![*target],
		Terminator::Branch { then_blk, else_blk, .. } => vec![*then_blk, *else_blk],
		Terminator::CondBranch { then_, else_, .. } => vec![*then_, *else_],
		Terminator::ForLoop { body, done, .. } => vec![*body, *done],
		Terminator::Suspend { resume, .. } => vec![*resume],
		Terminator::Return(_) | Terminator::RaiseTerm | Terminator::Unreachable => Vec::new(),
		_ => Vec::new(),
	}
}

#[cfg(test)]
mod tests {
	use cranelift_frontend::FunctionBuilderContext;
	use cranelift_module::{Linkage, Module};
	use pon_ir::ir::{
		BinOp, Block, BlockId, CellId, FunctionId, Inst, LocalId, Module as IrModule, NameId,
		Terminator, UnOp, Value,
	};
	use pon_runtime::abi::HELPERS;

	use super::*;
	use crate::helpers::declare_helpers;

	fn jit_module() -> cranelift_jit::JITModule {
		let isa = crate::isa::make_isa(crate::isa::OptLevel::None, false);
		let mut builder =
			cranelift_jit::JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
		for helper in HELPERS {
			builder.symbol(helper.symbol, helper.address.cast::<u8>());
		}
		builder.symbol(
			pon_runtime::abi::CURRENT_LINE_SYMBOL,
			pon_runtime::abi::current_line_cell_address(),
		);
		register_free_threading_symbols(&mut builder);
		cranelift_jit::JITModule::new(builder)
	}

	#[cfg(feature = "free-threading")]
	fn register_free_threading_symbols(builder: &mut cranelift_jit::JITBuilder) {
		unsafe extern "C" fn safepoint_poll() {}
		unsafe extern "C" fn write_barrier(
			_slot: *mut *mut pon_runtime::object::PyObject,
			_new: *mut pon_runtime::object::PyObject,
		) {
		}
		unsafe extern "C" fn stop_requested() -> bool {
			false
		}

		builder.symbol(crate::FT_SAFEPOINT_POLL, safepoint_poll as *const u8);
		builder.symbol(crate::FT_GC_WRITE_BARRIER, write_barrier as *const u8);
		builder.symbol(crate::FT_GC_STOP_REQUESTED, stop_requested as *const u8);
	}

	#[cfg(not(feature = "free-threading"))]
	fn register_free_threading_symbols(_builder: &mut cranelift_jit::JITBuilder) {}

	fn compiled_clif(ir_module: &IrModule, function_index: usize) -> String {
		let mut module = jit_module();
		let helpers = declare_helpers(&mut module).expect("helpers declare");
		let mut sig = module.make_signature();
		let ptr = module.target_config().pointer_type();
		sig.params.push(AbiParam::new(ptr));
		sig.params.push(AbiParam::new(ptr));
		sig.returns.push(AbiParam::new(ptr));
		let func_ids = ir_module
			.functions
			.iter()
			.map(|func| {
				module
					.declare_function(&func.name, Linkage::Local, &sig)
					.expect("function declare")
			})
			.collect::<Vec<_>>();
		let names = NameMap::from_ir_module(ir_module);
		let entry_arg_counts = entry_arg_counts(ir_module);

		let mut rendered_functions = Vec::with_capacity(ir_module.functions.len());
		for (index, function) in ir_module.functions.iter().enumerate() {
			let mut ctx = module.make_context();
			let mut fctx = FunctionBuilderContext::new();
			compile_function(
				&mut module,
				&helpers,
				&func_ids,
				&ir_module.functions,
				&names,
				function,
				entry_arg_counts[index],
				&mut ctx,
				&mut fctx,
			)
			.expect("function compiles");

			let mut rendered = String::new();
			for (_, decl) in module.declarations().get_functions() {
				if let Some(name) = &decl.name {
					rendered.push_str(name);
					rendered.push('\n');
				}
			}
			rendered.push_str(&ctx.func.display().to_string());
			module
				.define_function(func_ids[index], &mut ctx)
				.expect("function defines");
			rendered_functions.push(rendered);
		}
		module
			.finalize_definitions()
			.expect("compiled functions finalize");
		rendered_functions.remove(function_index)
	}

	fn compile_error(ir_module: &IrModule) -> CodegenError {
		let mut module = jit_module();
		let helpers = declare_helpers(&mut module).expect("helpers declare");
		let mut sig = module.make_signature();
		let ptr = module.target_config().pointer_type();
		sig.params.push(AbiParam::new(ptr));
		sig.params.push(AbiParam::new(ptr));
		sig.returns.push(AbiParam::new(ptr));
		let func_ids = ir_module
			.functions
			.iter()
			.map(|func| {
				module
					.declare_function(&func.name, Linkage::Local, &sig)
					.expect("function declare")
			})
			.collect::<Vec<_>>();
		let names = NameMap::from_ir_module(ir_module);
		let entry_arg_counts = entry_arg_counts(ir_module);
		let mut ctx = module.make_context();
		let mut fctx = FunctionBuilderContext::new();

		compile_function(
			&mut module,
			&helpers,
			&func_ids,
			&ir_module.functions,
			&names,
			&ir_module.functions[0],
			entry_arg_counts[0],
			&mut ctx,
			&mut fctx,
		)
		.expect_err("unsupported IR returns typed error")
	}

	fn binary_ir(op: BinOp) -> IrModule {
		IrModule {
			functions: vec![Function {
				name:               "binary".to_owned(),
				arity:              2,
				is_coroutine:       false,
				is_generator:       false,
				is_async_generator: false,
				params:             Default::default(),
				n_locals:           2,
				blocks:             vec![Block {
					id:    BlockId(0),
					insts: vec![
						Inst::new(Value(0), InstKind::LoadLocal(LocalId(0))),
						Inst::new(Value(1), InstKind::LoadLocal(LocalId(1))),
						Inst::new(Value(2), InstKind::BinaryOp { op, lhs: Value(0), rhs: Value(1) }),
					],
					term:  Terminator::Return(Value(2)),
				}],
			}],
			main:      FunctionId(0),
			names:     vec![],
		}
	}

	fn selector_for_known_or_future_binary_op(op: Option<BinOp>) -> Result<u8, CodegenError> {
		match op {
			Some(BinOp::Add) => Ok(0),
			Some(BinOp::Sub) => Ok(1),
			Some(BinOp::Mul) => Ok(2),
			Some(BinOp::MatMul) => Ok(3),
			Some(BinOp::Div) => Ok(4),
			Some(BinOp::FloorDiv) => Ok(5),
			Some(BinOp::Mod) => Ok(6),
			Some(BinOp::Pow) => Ok(7),
			Some(BinOp::LShift) => Ok(8),
			Some(BinOp::RShift) => Ok(9),
			Some(BinOp::And) => Ok(10),
			Some(BinOp::Or) => Ok(11),
			Some(BinOp::Xor) => Ok(12),
			Some(_) | None => Err(CodegenError::Unsupported("binary op")),
		}
	}

	#[test]
	fn binary_op_clif_uses_selector_mapping_and_typed_future_error() {
		for (op, selector) in [
			(BinOp::Add, 0_u8),
			(BinOp::Sub, 1),
			(BinOp::Mul, 2),
			(BinOp::MatMul, 3),
			(BinOp::Div, 4),
			(BinOp::FloorDiv, 5),
			(BinOp::Mod, 6),
			(BinOp::Pow, 7),
			(BinOp::LShift, 8),
			(BinOp::RShift, 9),
			(BinOp::And, 10),
			(BinOp::Or, 11),
			(BinOp::Xor, 12),
		] {
			let clif = compiled_clif(&binary_ir(op), 0);

			assert!(clif.contains("pon_number_binary"));
			assert!(
				clif.contains(&format!("iconst.i8 {selector}")),
				"missing binary selector {selector} in CLIF:\n{clif}"
			);
			assert!(matches!(
				 selector_for_known_or_future_binary_op(Some(op)),
				 Ok(value) if value == selector
			));
		}

		// `BinOp` is non-exhaustive outside `pon-ir`; `None` stands in for the
		// future selector this crate cannot construct until a new variant exists.
		assert!(matches!(
			selector_for_known_or_future_binary_op(None),
			Err(CodegenError::Unsupported("binary op"))
		));
	}

	fn unary_ir(op: UnOp) -> IrModule {
		IrModule {
			functions: vec![Function {
				name:               "unary".to_owned(),
				arity:              1,
				is_coroutine:       false,
				is_generator:       false,
				is_async_generator: false,
				params:             Default::default(),
				n_locals:           1,
				blocks:             vec![Block {
					id:    BlockId(0),
					insts: vec![
						Inst::new(Value(0), InstKind::LoadLocal(LocalId(0))),
						Inst::new(Value(1), InstKind::UnaryOp { op, operand: Value(0) }),
					],
					term:  Terminator::Return(Value(1)),
				}],
			}],
			main:      FunctionId(0),
			names:     vec![],
		}
	}

	fn selector_for_known_or_future_unary_op(op: Option<UnOp>) -> Result<u8, CodegenError> {
		match op {
			Some(UnOp::Neg) => Ok(0),
			Some(UnOp::Pos) => Ok(1),
			Some(UnOp::Invert) => Ok(2),
			Some(_) | None => Err(CodegenError::Unsupported("unary op")),
		}
	}

	#[test]
	fn unary_op_clif_uses_selector_mapping_and_typed_future_error() {
		for (op, selector) in [(UnOp::Neg, 0_u8), (UnOp::Pos, 1), (UnOp::Invert, 2)] {
			let clif = compiled_clif(&unary_ir(op), 0);

			assert!(clif.contains("pon_number_unary"));
			assert!(
				clif.contains(&format!("iconst.i8 {selector}")),
				"missing unary selector {selector} in CLIF:\n{clif}"
			);
			assert!(matches!(
				 selector_for_known_or_future_unary_op(Some(op)),
				 Ok(value) if value == selector
			));
		}

		// `UnOp` is non-exhaustive outside `pon-ir`; `None` stands in for the
		// future selector this crate cannot construct until a new variant exists.
		assert!(matches!(
			selector_for_known_or_future_unary_op(None),
			Err(CodegenError::Unsupported("unary op"))
		));
	}

	#[test]
	fn for_next_helper_clif_regresses_value_type_borrow_shape() {
		let ir = IrModule {
			functions: vec![Function {
				name:               "next_item".to_owned(),
				arity:              1,
				is_coroutine:       false,
				is_generator:       false,
				is_async_generator: false,
				params:             Default::default(),
				n_locals:           1,
				blocks:             vec![Block {
					id:    BlockId(0),
					insts: vec![
						Inst::new(Value(0), InstKind::LoadLocal(LocalId(0))),
						Inst::new(Value(1), InstKind::ForNext { iter: Value(0) }),
					],
					term:  Terminator::Return(Value(1)),
				}],
			}],
			main:      FunctionId(0),
			names:     vec![],
		};

		// Regression harness for `lower_for_next`: compute the iterator value
		// type before starting `builder.ins().iconst(...)`. Inlining
		// `builder.func.dfg.value_type(iter)` into that call creates overlapping
		// immutable/mutable borrows of the builder.
		let clif = compiled_clif(&ir, 0);

		assert!(clif.contains("pon_for_next"));
		assert!(clif.contains("iconst.i64 0") || clif.contains("iconst.i32 0"));
	}

	#[test]
	#[cfg(feature = "free-threading")]
	fn free_threading_clif_polls_function_entry_and_loop_backedge() {
		let ir = IrModule {
			functions: vec![Function {
				name:               "loop_poll".to_owned(),
				arity:              1,
				is_coroutine:       false,
				is_generator:       false,
				is_async_generator: false,
				params:             Default::default(),
				n_locals:           2,
				blocks:             vec![
					Block {
						id:    BlockId(0),
						insts: vec![
							Inst::new(Value(0), InstKind::LoadLocal(LocalId(0))),
							Inst::new(Value(1), InstKind::GetIter { iterable: Value(0) }),
						],
						term:  Terminator::Jump(BlockId(1)),
					},
					Block {
						id:    BlockId(1),
						insts: vec![Inst::new(Value(2), InstKind::ForNext { iter: Value(1) })],
						term:  Terminator::ForLoop { iter: Value(1), body: BlockId(2), done: BlockId(3) },
					},
					Block {
						id:    BlockId(2),
						insts: vec![Inst::new(Value(3), InstKind::StoreLocal(LocalId(1), Value(2)))],
						term:  Terminator::Jump(BlockId(1)),
					},
					Block {
						id:    BlockId(3),
						insts: vec![Inst::new(Value(4), InstKind::Const(PyConst::None))],
						term:  Terminator::Return(Value(4)),
					},
				],
			}],
			main:      FunctionId(0),
			names:     vec![],
		};

		let clif = compiled_clif(&ir, 0);

		assert!(clif.contains(crate::FT_SAFEPOINT_POLL));
		// `pon_safepoint_poll` is the unique zero-arg, no-return helper. Resolve
		// its CLIF func ref structurally so the assertion survives helper-table
		// reordering (a hard-coded `fnN` breaks whenever a helper is inserted).
		let poll_sig = clif
			.lines()
			.find_map(|line| {
				line.trim().strip_prefix("sig").and_then(|rest| {
					let (num, tail) = rest.split_once(" = ")?;
					(tail.starts_with("()") && !tail.contains("->")).then(|| num.to_owned())
				})
			})
			.expect("a zero-arg no-return signature for pon_safepoint_poll");
		let poll_fn = clif
			.lines()
			.find_map(|line| {
				line.trim().strip_prefix("fn").and_then(|rest| {
					let (num, tail) = rest.split_once(" = ")?;
					tail
						.ends_with(&format!("sig{poll_sig}"))
						.then(|| num.to_owned())
				})
			})
			.expect("a func ref bound to the safepoint-poll signature");
		assert!(
			clif.matches(&format!("call fn{poll_fn}()")).count() >= 2,
			"expected function-entry and loop-backedge safepoint calls in CLIF:\n{clif}"
		);
	}

	#[test]
	#[cfg(not(feature = "free-threading"))]
	fn default_clif_does_not_import_safepoint_poll() {
		let ir = IrModule {
			functions: vec![Function {
				name:               "no_poll".to_owned(),
				arity:              0,
				is_coroutine:       false,
				is_generator:       false,
				is_async_generator: false,
				params:             Default::default(),
				n_locals:           0,
				blocks:             vec![Block {
					id:    BlockId(0),
					insts: vec![Inst::new(Value(0), InstKind::Const(PyConst::None))],
					term:  Terminator::Return(Value(0)),
				}],
			}],
			main:      FunctionId(0),
			names:     vec![],
		};

		let clif = compiled_clif(&ir, 0);

		assert!(
			!clif.contains("pon_safepoint_poll"),
			"default CLIF must not import FT safepoint polls:\n{clif}"
		);
	}

	#[test]
	fn main_function_clif_lowers_make_function_and_global_store_with_null_checks() {
		let ir = IrModule {
			functions: vec![
				Function {
					name:               "__main__".to_owned(),
					arity:              0,
					is_coroutine:       false,
					is_generator:       false,
					is_async_generator: false,
					params:             Default::default(),
					n_locals:           0,
					blocks:             vec![Block {
						id:    BlockId(0),
						insts: vec![
							Inst::new(Value(0), InstKind::MakeFunction {
								func_index:    1,
								name_interned: NameId(0),
								arity:         2,
							}),
							Inst::new(Value(1), InstKind::StoreGlobal(NameId(0), Value(0))),
							Inst::new(Value(2), InstKind::Const(PyConst::None)),
						],
						term:  Terminator::Return(Value(2)),
					}],
				},
				Function {
					name:               "add".to_owned(),
					arity:              2,
					is_coroutine:       false,
					is_generator:       false,
					is_async_generator: false,
					params:             Default::default(),
					n_locals:           2,
					blocks:             vec![Block {
						id:    BlockId(0),
						insts: vec![
							Inst::new(Value(0), InstKind::LoadLocal(LocalId(0))),
							Inst::new(Value(1), InstKind::LoadLocal(LocalId(1))),
							Inst::new(Value(2), InstKind::BinaryOp {
								op:  BinOp::Add,
								lhs: Value(0),
								rhs: Value(1),
							}),
						],
						term:  Terminator::Return(Value(2)),
					}],
				},
			],
			main:      FunctionId(0),
			names:     vec!["add".to_owned()],
		};

		let clif = compiled_clif(&ir, 0);

		assert!(clif.contains("pon_make_function"));
		assert!(clif.contains("pon_store_global"));
		assert!(clif.matches("brif").count() >= 3);
	}

	#[test]
	fn make_function_full_clif_uses_code_info_and_default_arrays() {
		let ir = IrModule {
			functions: vec![
				Function {
					name:               "__main__".to_owned(),
					arity:              0,
					is_coroutine:       false,
					is_generator:       false,
					is_async_generator: false,
					params:             Default::default(),
					n_locals:           0,
					blocks:             vec![Block {
						id:    BlockId(0),
						insts: vec![
							Inst::new(Value(0), InstKind::Const(PyConst::Int(7))),
							Inst::new(Value(1), InstKind::Const(PyConst::Int(11))),
							Inst::new(Value(2), InstKind::MakeFunctionFull {
								code:        FunctionId(1),
								defaults:    vec![Value(0)],
								kwdefaults:  vec![(NameId(0), Value(1))],
								closure:     vec![],
								annotations: vec![],
							}),
						],
						term:  Terminator::Return(Value(2)),
					}],
				},
				Function {
					name:               "with_defaults".to_owned(),
					arity:              1,
					is_coroutine:       false,
					is_generator:       false,
					is_async_generator: false,
					params:             Default::default(),
					n_locals:           2,
					blocks:             vec![Block {
						id:    BlockId(0),
						insts: vec![Inst::new(Value(0), InstKind::Const(PyConst::None))],
						term:  Terminator::Return(Value(0)),
					}],
				},
			],
			main:      FunctionId(0),
			names:     vec!["kw_only".to_owned()],
		};

		let clif = compiled_clif(&ir, 0);

		assert!(
			clif.contains("pon_make_function_full"),
			"MakeFunctionFull should declare the full-function helper import:\n{clif}"
		);
		assert!(
			clif.contains("global_value."),
			"MakeFunctionFull should pass a static CodeInfo data pointer:\n{clif}"
		);
		assert!(
			clif.matches("stack_addr.").count() >= 2,
			"positional and keyword-only defaults should be passed through stack arrays:\n{clif}"
		);
		assert!(
			clif.matches("stack_store").count() >= 2,
			"default values should be stored into the generated argument arrays:\n{clif}"
		);
		assert!(
			clif.contains("iconst.i64 1") || clif.contains("iconst.i32 1"),
			"both default-count operands should materialize the one-default case:\n{clif}"
		);
	}

	#[test]
	fn make_function_full_clif_plumbs_closure_cells() {
		let ir = IrModule {
			functions: vec![
				Function {
					name:               "__main__".to_owned(),
					arity:              1,
					is_coroutine:       false,
					is_generator:       false,
					is_async_generator: false,
					params:             Default::default(),
					n_locals:           1,
					blocks:             vec![Block {
						id:    BlockId(0),
						insts: vec![
							Inst::new(Value(0), InstKind::MakeCell(LocalId(0))),
							Inst::new(Value(1), InstKind::MakeFunctionFull {
								code:        FunctionId(1),
								defaults:    vec![],
								kwdefaults:  vec![],
								closure:     vec![CellId(0)],
								annotations: vec![],
							}),
						],
						term:  Terminator::Return(Value(1)),
					}],
				},
				Function {
					name:               "inner".to_owned(),
					arity:              0,
					is_coroutine:       false,
					is_generator:       false,
					is_async_generator: false,
					params:             Default::default(),
					n_locals:           0,
					blocks:             vec![Block {
						id:    BlockId(0),
						insts: vec![Inst::new(Value(0), InstKind::Const(PyConst::None))],
						term:  Terminator::Return(Value(0)),
					}],
				},
			],
			main:      FunctionId(0),
			names:     vec![],
		};

		let clif = compiled_clif(&ir, 0);

		assert!(
			clif.contains("pon_make_cell"),
			"MakeCell should declare the cell-allocation helper import:\n{clif}"
		);
		assert!(
			clif.contains("pon_make_function_full"),
			"closure-bearing MakeFunctionFull should declare the full-function helper import:\n{clif}"
		);
		assert!(
			clif.contains("pon_function_set_closure"),
			"closure-bearing MakeFunctionFull should declare the closure setter import:\n{clif}"
		);
		assert!(
			clif.contains("global_value."),
			"closure-bearing MakeFunctionFull should still pass static CodeInfo data:\n{clif}"
		);
		assert!(
			clif.contains("stack_store") && clif.contains("stack_addr."),
			"captured cells should be written to a closure array before \
			 pon_function_set_closure:\n{clif}"
		);
		assert!(
			clif.matches("call fn").count() >= 3,
			"expected helper calls for make-cell, make-function-full, and set-closure:\n{clif}"
		);
	}

	#[test]
	fn unsupported_phase_b_inst_returns_typed_error() {
		let ir = IrModule {
			functions: vec![Function {
				name:               "future".to_owned(),
				arity:              0,
				is_coroutine:       false,
				is_generator:       false,
				is_async_generator: false,
				params:             Default::default(),
				n_locals:           0,
				blocks:             vec![Block {
					id:    BlockId(0),
					insts: vec![Inst::new(Value(0), InstKind::Const(PyConst::NotImplemented))],
					term:  Terminator::Return(Value(0)),
				}],
			}],
			main:      FunctionId(0),
			names:     vec![],
		};

		assert!(matches!(
			compile_error(&ir),
			CodegenError::Unsupported("Const(non Phase-A literal)")
		));
	}

	#[test]
	fn unsupported_phase_b_terminator_returns_typed_error() {
		let ir = IrModule {
			functions: vec![Function {
				name:               "future_term".to_owned(),
				arity:              0,
				is_coroutine:       false,
				is_generator:       false,
				is_async_generator: false,
				params:             Default::default(),
				n_locals:           0,
				blocks:             vec![Block {
					id:    BlockId(0),
					insts: vec![Inst::new(Value(0), InstKind::Const(PyConst::None))],
					term:  Terminator::Unreachable,
				}],
			}],
			main:      FunctionId(0),
			names:     vec![],
		};

		assert!(matches!(compile_error(&ir), CodegenError::Unsupported("non-return terminator")));
	}

	#[test]
	fn line_transitions_emit_one_store_per_statement() {
		// Statement 1 lowers to several instructions (three consts, two
		// binops) that all share line 1; statement 2 transitions to line 2.
		let ir = pon_ir::lower_source("x = 1 + 2 + 3\ny = 4\n").expect("line-stamped source lowers");
		let clif = compiled_clif(&ir, 0);

		// Exactly one `pon_current_line` store per statement-line transition,
		// deduped across a statement's instruction run.  Boxed locals live in
		// CLIF variables (their shadow-slot writes are `stack_store`s, filtered
		// here) and globals go through helper calls, so line-cell writes are
		// the only plain `store`s in the lowered body.  Each store line's value
		// annotation names the recorded line (`store vN, vM  ; vN = <line>`).
		let stores: Vec<&str> = clif
			.lines()
			.filter(|line| line.contains("store v") && !line.contains("stack_store"))
			.collect();
		assert_eq!(stores.len(), 2, "one line store per statement:\n{clif}");
		assert!(stores[0].ends_with("= 1"), "first store records line 1: {}", stores[0]);
		assert!(stores[1].ends_with("= 2"), "second store records line 2: {}", stores[1]);
	}

	#[test]
	fn line_free_ir_emits_no_line_plumbing() {
		// Hand-built IR carries `line: 0` everywhere; its emitted code must
		// contain no line plumbing (no imported cell, no line-cell stores).
		// Local shadow-slot writes are `stack_store`s and are expected.
		let ir = binary_ir(BinOp::Add);
		let clif = compiled_clif(&ir, 0);
		let line_stores = clif
			.lines()
			.filter(|line| line.contains("store v") && !line.contains("stack_store"))
			.count();
		assert_eq!(line_stores, 0, "no line stores for line-free IR:\n{clif}");
	}
}
