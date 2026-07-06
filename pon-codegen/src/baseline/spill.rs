//! Tier-0 GC spill windows for expression temporaries.
//!
//! Conservative collection scans frame memory, never registers (register
//! roots retain dead values; see `LowerState::local_shadow`).  Named locals
//! are mirrored into shadow frame slots, but expression TEMPORARIES are
//! plain SSA values that regalloc may park in registers across a helper
//! call.  When that helper re-enters user code that reaches an explicit
//! `gc.collect()`, a temporary that is still needed after the call — the
//! label literal in `print("label", collecting_call())` — is invisible to
//! the stack scan and gets swept mid-expression.
//!
//! Mitigation until Phase-D precise stack maps land: before every
//! instruction whose helper may re-enter user code, store the temporaries
//! live ACROSS that instruction into a per-function pool of explicit stack
//! slots and zero the unused tail of the pool.  At every reachable
//! collection point the pool therefore holds exactly the live-across
//! temporaries: dead values never linger in the frame, so the
//! false-retention hazard that argv-array scrubbing exists for
//! (aot-parity `weakref_basic`) cannot re-enter through the pool.
//!
//! Scope: pon collection can be reached from explicit `gc.collect()` in user
//! code, including Python signal handlers drained by generated-code
//! safepoints. Helpers that never invoke a user callable (allocation from
//! evaluated operands, local/cell moves, module-dict access) cannot collect and
//! are not spill points. Two residuals are out
//! of scope here: values passed INTO a collecting helper are the runtime's
//! rooting concern (argv arrays already live in scanned frame slots), and
//! the Phase-D optimizing tier carries its own precise-map plan.

use std::collections::{HashMap, HashSet};

use cranelift_codegen::ir::{self, InstBuilder, StackSlotData, StackSlotKind};
use cranelift_frontend::FunctionBuilder;
use pon_ir::ir::{BlockId, Function, InstKind, Terminator, Value as IrValue};

use super::{CodegenError, LowerState, block_exception_entry_stacks, successors};
use crate::region::{inst_operands, terminator_operands};

/// Per-function spill schedule: which SSA temporaries must be rooted in
/// frame memory across which instruction, plus the shared slot pool they
/// are rooted in.
pub(crate) struct TempSpillPlan {
	/// `(block, instruction index)` -> temporaries live across that
	/// instruction's helper window, sorted by SSA index.
	inst_spills: HashMap<(BlockId, usize), Vec<IrValue>>,
	/// `block` -> temporaries live across the terminator's truth-test
	/// helper window (`pon_is_true` runs user `__bool__`).
	term_spills: HashMap<BlockId, Vec<IrValue>>,
	/// Shared pointer-sized slot pool, sized to the widest spill set.
	pool:        Vec<ir::StackSlot>,
}

impl TempSpillPlan {
	/// Computes the spill schedule for `function` and reserves its slot
	/// pool.  Returns `None` when no window would ever store a value, so
	/// spill-free functions lower byte-identically to before.
	pub(crate) fn compute(
		builder: &mut FunctionBuilder<'_>,
		function: &Function,
		ptr_ty: ir::Type,
	) -> Result<Option<Self>, CodegenError> {
		let has_sites = function.blocks.iter().any(|block| {
			term_spill_point(&block.term)
				|| block.insts.iter().any(|inst| inst_spill_point(&inst.kind))
		});
		if !has_sites {
			return Ok(None);
		}

		// Exception-handler blocks whose live-ins must survive any raising
		// instruction of a protected block: the handlers active on entry
		// plus any pushed within the block.  Treating them as live for the
		// whole block over-approximates (a value stays spilled between a
		// `PopExcInfo` and the block end) but is sound: SSA dominance
		// guarantees a handler can only use values defined before every
		// instruction that can enter it.
		let entry_stacks = block_exception_entry_stacks(function)?;
		let mut handler_targets = HashMap::<BlockId, Vec<BlockId>>::new();
		for block in &function.blocks {
			let mut targets = entry_stacks.get(&block.id).cloned().unwrap_or_default();
			for inst in &block.insts {
				if let InstKind::PushExcInfo { target, .. } = &inst.kind {
					targets.push(*target);
				}
			}
			targets.sort_unstable_by_key(|target| target.0);
			targets.dedup();
			handler_targets.insert(block.id, targets);
		}

		// Standard backward liveness over the CFG extended with the
		// exception edges above (same shape as `osr_live_values`).
		let mut gens = HashMap::<BlockId, HashSet<IrValue>>::new();
		let mut kills = HashMap::<BlockId, HashSet<IrValue>>::new();
		for block in &function.blocks {
			let mut block_gen = HashSet::new();
			let mut block_kill = HashSet::new();
			for inst in &block.insts {
				for operand in inst_operands(&inst.kind) {
					if !block_kill.contains(&operand) {
						block_gen.insert(operand);
					}
				}
				block_kill.insert(inst.result);
			}
			for operand in terminator_operands(&block.term) {
				if !block_kill.contains(&operand) {
					block_gen.insert(operand);
				}
			}
			gens.insert(block.id, block_gen);
			kills.insert(block.id, block_kill);
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
				for handler in &handler_targets[&block.id] {
					if let Some(input) = live_in.get(handler) {
						out.extend(input.iter().copied());
					}
				}
				let mut next = gens.get(&block.id).cloned().unwrap_or_default();
				for value in out {
					if !kills.get(&block.id).is_some_and(|set| set.contains(&value)) {
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

		// Per-site extraction: walk each block backward; on arrival at an
		// instruction the running set holds exactly the values live after
		// it (its own result was just removed — a result does not exist
		// during its defining window and is rooted by the callee/return
		// path instead).
		let mut inst_spills = HashMap::new();
		let mut term_spills = HashMap::new();
		let mut pool_size = 0usize;
		for block in &function.blocks {
			let mut live = HashSet::<IrValue>::new();
			for successor in successors(&block.term) {
				if let Some(input) = live_in.get(&successor) {
					live.extend(input.iter().copied());
				}
			}
			for handler in &handler_targets[&block.id] {
				if let Some(input) = live_in.get(handler) {
					live.extend(input.iter().copied());
				}
			}
			if term_spill_point(&block.term) {
				let set = sorted(&live);
				pool_size = pool_size.max(set.len());
				term_spills.insert(block.id, set);
			}
			// Terminator operands are consumed after the last instruction,
			// so they are live across every earlier window.
			live.extend(terminator_operands(&block.term));
			for (index, inst) in block.insts.iter().enumerate().rev() {
				live.remove(&inst.result);
				if inst_spill_point(&inst.kind) {
					let set = sorted(&live);
					pool_size = pool_size.max(set.len());
					inst_spills.insert((block.id, index), set);
				}
				live.extend(inst_operands(&inst.kind));
			}
		}

		if pool_size == 0 {
			return Ok(None);
		}
		let pool = (0..pool_size)
			.map(|_| {
				builder.create_sized_stack_slot(StackSlotData {
					kind:        StackSlotKind::ExplicitSlot,
					size:        ptr_ty.bytes(),
					align_shift: ptr_ty.bytes().trailing_zeros() as u8,
					key:         None,
				})
			})
			.collect();
		Ok(Some(Self { inst_spills, term_spills, pool }))
	}

	/// Emits the spill window for instruction `index` of `block`, if that
	/// instruction is a spill site.
	pub(crate) fn emit_inst_window(
		&self,
		builder: &mut FunctionBuilder<'_>,
		state: &LowerState,
		block: BlockId,
		index: usize,
		ptr_ty: ir::Type,
	) {
		if let Some(values) = self.inst_spills.get(&(block, index)) {
			self.emit_window(builder, state, values, ptr_ty);
		}
	}

	/// Emits the spill window guarding `block`'s terminator, if it is a
	/// spill site.
	pub(crate) fn emit_term_window(
		&self,
		builder: &mut FunctionBuilder<'_>,
		state: &LowerState,
		block: BlockId,
		ptr_ty: ir::Type,
	) {
		if let Some(values) = self.term_spills.get(&block) {
			self.emit_window(builder, state, values, ptr_ty);
		}
	}

	/// Stores the window's live temporaries into the pool prefix and zeroes
	/// the tail, so the scan roots exactly the live values during the
	/// window.  An empty window still zeroes: a collecting site must never
	/// observe a predecessor window's dead copies.
	fn emit_window(
		&self,
		builder: &mut FunctionBuilder<'_>,
		state: &LowerState,
		values: &[IrValue],
		ptr_ty: ir::Type,
	) {
		let mut next = 0usize;
		for value in values {
			// Absent means the defining instruction is lowered later in
			// this block (a loop-carried range whose def sits below this
			// site): there is no dominating CLIF definition to store, and
			// on the first execution the value does not exist yet.  Such a
			// range stays register-resident across this window — accepted
			// residual until Phase-D precise maps.
			let Some(&clif) = state.values.get(value) else {
				continue;
			};
			// Only boxed object pointers can be roots; the cold twin seeds
			// shared lowering state with raw unboxed values too.
			if builder.func.dfg.value_type(clif) != ptr_ty {
				continue;
			}
			builder.ins().stack_store(clif, self.pool[next], 0);
			next += 1;
		}
		if next < self.pool.len() {
			let zero = builder.ins().iconst(ptr_ty, 0);
			for slot in &self.pool[next..] {
				builder.ins().stack_store(zero, *slot, 0);
			}
		}
	}
}

fn sorted(live: &HashSet<IrValue>) -> Vec<IrValue> {
	let mut values = live.iter().copied().collect::<Vec<_>>();
	values.sort_unstable_by_key(|value| value.0);
	values
}

/// True when the instruction's helper may invoke a user callable and can
/// therefore reach an explicit `gc.collect()`.
///
/// Kept as an exclusion list over the helpers that provably cannot run
/// user code, so new `InstKind` variants default to being spill points
/// (sound by default, opt out for performance):
/// - constant materialization and container/function/cell construction from
///   already-evaluated operands only allocate; allocation does not
///   synchronously enter the collector on this mutator.  A concurrent collector
///   stops generated code only at emitted safepoints, and live locals are
///   mirrored in frame slots before those stops;
/// - local/cell/global moves touch frame variables or module dicts with
///   interned-string keys (no user hooks); `LoadName` is NOT excluded because
///   class bodies execute against user `__prepare__` mappings;
/// - `Is` is pointer identity; exact-list append/copy run no user code;
/// - exception-state bookkeeping (push/pop/reraise/`except*` phase tags) moves
///   already-constructed objects; `Raise`/match/check arms are spill points
///   (user exception `__init__`, metaclass `__instancecheck__`/
///   `__subclasscheck__`);
/// - `Yield`/`YieldFrom` are generator-transform inputs that codegen rejects
///   before any window could be emitted.
fn inst_spill_point(kind: &InstKind) -> bool {
	!matches!(
		kind,
		InstKind::Const(_)
			| InstKind::ConstRef(_)
			| InstKind::BuildTuple { .. }
			| InstKind::BuildList { .. }
			| InstKind::BuildSlice { .. }
			| InstKind::ListAppend { .. }
			| InstKind::ListToTuple { .. }
			| InstKind::LoadLocal(_)
			| InstKind::StoreLocal(..)
			| InstKind::DeleteLocal(_)
			| InstKind::LoadGlobal(_)
			| InstKind::StoreGlobal(..)
			| InstKind::DeleteGlobal(_)
			| InstKind::LoadCell(_)
			| InstKind::StoreCell(..)
			| InstKind::DeleteCell(_)
			| InstKind::MakeCell(_)
			| InstKind::LoadClosure(_)
			| InstKind::LoadBuiltin(_)
			| InstKind::Is { .. }
			| InstKind::MakeFunction { .. }
			| InstKind::MakeFunctionFull { .. }
			| InstKind::FunctionSetAnnotate { .. }
			| InstKind::MakeTypeAlias { .. }
			| InstKind::MakeTypeVar { .. }
			| InstKind::LoadBuildClass
			| InstKind::PushExcInfo { .. }
			| InstKind::PopExcInfo
			| InstKind::GenResumePayload
			| InstKind::GenLastStopValue
			| InstKind::GetCurrentExc
			| InstKind::ExcStarEnter
			| InstKind::ExcStarBodyOk
			| InstKind::ExcStarBodyRaised
			| InstKind::ExcStarFinish
			| InstKind::Reraise
			| InstKind::Yield { .. }
			| InstKind::YieldFrom { .. }
	)
}

/// True when the terminator's lowering may invoke a user callable:
/// `Branch`/`CondBranch` truth-test through `pon_is_true` (user
/// `__bool__`/`__len__`).  Backedge `Jump` terminators drain pending signal
/// handlers through `pon_safepoint_poll`, so they are conservatively spill
/// points even though forward jumps usually only transfer control.  `ForLoop`
/// only branches on the preceding `ForNext` result (its stop path reads the
/// runtime StopIteration stash), and `Suspend`/`Return` route through the
/// generator frame machinery.
fn term_spill_point(term: &Terminator) -> bool {
	matches!(
		term,
		Terminator::Branch { .. } | Terminator::CondBranch { .. } | Terminator::Jump(_)
	)
}

#[cfg(test)]
mod tests {
	use cranelift_codegen::ir::{self, AbiParam, types};
	use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
	use pon_ir::ir::{
		BinOp, Block, BlockId, Function, Inst, InstKind, LocalId, PyConst, Terminator, Value,
	};

	use super::*;

	fn compute_plan(function: &Function) -> Option<TempSpillPlan> {
		let mut func = ir::Function::new();
		func.signature.returns.push(AbiParam::new(types::I64));
		let mut fctx = FunctionBuilderContext::new();
		let mut builder = FunctionBuilder::new(&mut func, &mut fctx);
		let entry = builder.create_block();
		builder.switch_to_block(entry);
		let plan = TempSpillPlan::compute(&mut builder, function, types::I64)
			.expect("fixture should build a spill plan");
		builder.seal_all_blocks();
		builder.finalize();
		plan
	}

	#[test]
	fn tracks_expression_temp_live_across_call() {
		let function = Function {
			name:               "spill".to_owned(),
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
					Inst::new(Value(1), InstKind::Const(PyConst::Int(1))),
					Inst::new(Value(2), InstKind::Const(PyConst::Int(2))),
					Inst::new(Value(3), InstKind::BinaryOp {
						op:  BinOp::Add,
						lhs: Value(1),
						rhs: Value(2),
					}),
					Inst::new(Value(4), InstKind::Call { callee: Value(0), args: Vec::new() }),
					Inst::new(Value(5), InstKind::BuildTuple { elts: vec![Value(3), Value(4)] }),
				],
				term:  Terminator::Return(Value(5)),
			}],
		};

		let plan = compute_plan(&function).expect("call-crossing temp should allocate a spill pool");
		assert_eq!(
			plan.inst_spills.get(&(BlockId(0), 4)),
			Some(&vec![Value(3)]),
			"the binary-op temp must stay rooted across the call window"
		);
		assert_eq!(plan.pool.len(), 1, "one live-across temp needs one spill slot");
		assert!(plan.term_spills.is_empty(), "this fixture has no truth-test spill sites");
	}
}
