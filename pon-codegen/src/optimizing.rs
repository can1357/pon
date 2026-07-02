//! Phase-D optimizing codegen.
//!
//! The default tier-0 path still uses boxed baseline lowering, but tier-up/AoT
//! callers can lower a typed region with `IntI64` values in CLIF registers and
//! side-exit to a cold boxed twin that reuses baseline lowering for exact
//! semantics.

use std::collections::HashMap;

use cranelift_codegen::Context;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{self, AbiParam, InstBuilder, MemFlagsData, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{FuncId, Module};
use pon_ir::Type;
use pon_ir::ir::{BinOp, Block as IrBlock, BlockId, CmpOp, Function, Inst, InstKind, PyConst, Terminator, UnOp, Value as IrValue};

use crate::baseline::{self, CodegenError, HelperFuncRefs, LowerState, NameMap, control};
use crate::helpers::HelperRefs;
use crate::region::{self, TypedInput, TypedRegion, TypedValue};

/// Optimizing plan for one function's current best typed region.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OptimizingPlan {
    /// Single-entry region selected from IR type metadata.
    pub region: TypedRegion,
    /// Guards that validate boxed live-ins before entering the fast path.
    pub entry_guards: Vec<EntryGuard>,
    /// Unboxed values the fast path may keep in CLIF registers.
    pub fast_path: FastPathPlan,
    /// Cold boxed twin and stack-map declarations for its safepoints.
    pub cold_twin: ColdTwinPlan,
}

/// An entry guard that checks and unboxes a value flowing into the region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntryGuard {
    pub value: IrValue,
    pub expected: Type,
    pub failure: GuardFailure,
}

/// Where an entry guard transfers control on failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuardFailure {
    /// Branch to the boxed cold twin for the same region.
    ColdTwin,
}

/// The unboxed fast-path view of a typed region.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FastPathPlan {
    pub entry: pon_ir::ir::BlockId,
    pub values: Vec<TypedValue>,
    pub exits_requiring_boxing: Vec<IrValue>,
}

/// The boxed fallback copy of the same region.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColdTwinPlan {
    pub entry: pon_ir::ir::BlockId,
    pub blocks: Vec<pon_ir::ir::BlockId>,
    pub calls: Vec<ColdCallSite>,
    pub stack_maps: Vec<StackMapDecl>,
}

/// One boxed helper call that can act as a cold-path safepoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ColdCallSite {
    pub block: pon_ir::ir::BlockId,
    pub inst_index: usize,
    pub result: IrValue,
}

/// Conservative stack-map declaration for boxed values live across a cold call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StackMapDecl {
    pub call: ColdCallSite,
    pub boxed_values: Vec<IrValue>,
}

/// Ordered lowering steps for plan inspection and tier-up bookkeeping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoweringStep {
    EntryGuard(EntryGuard),
    UnboxedFastPathValue(TypedValue),
    BoxForFastPathExit(IrValue),
    BoxedColdTwinCall(ColdCallSite),
    StackMap(StackMapDecl),
}

/// Build an optimizing plan for the largest currently typed region in `function`.
#[must_use]
pub fn plan_function(function: &Function) -> Option<OptimizingPlan> {
    region::find_maximal_typed_region(function).map(|region| plan_region(function, region))
}

/// Return true when `plan` uses only typed shapes the current optimizing
/// lowering can emit safely.
///
/// The Phase-D lowerer currently has real unboxed CLIF for signed 64-bit
/// integers. Other unboxable IR types are valid typed-region metadata, but they
/// must keep using boxed baseline lowering until their fast paths grow matching
/// guards, arithmetic, and reboxing support.
#[must_use]
pub fn can_compile_plan(plan: &OptimizingPlan) -> bool {
    plan.fast_path.values.iter().all(|value| value.ty == Type::IntI64)
        && plan.entry_guards.iter().all(|guard| guard.expected == Type::IntI64)
        && plan
            .fast_path
            .exits_requiring_boxing
            .iter()
            .filter_map(|value| {
                plan.fast_path
                    .values
                    .iter()
                    .find(|typed| typed.value == *value)
                    .map(|typed| typed.ty)
            })
            .all(|ty| ty == Type::IntI64)
}

/// Build an optimizing plan for a caller-selected typed region.
#[must_use]
pub fn plan_region(function: &Function, region: TypedRegion) -> OptimizingPlan {
    let entry_guards = region.live_ins.iter().copied().map(entry_guard).collect();
    let exits_requiring_boxing = values_reaching_region_exits(function, &region);
    let calls = cold_call_sites(function, &region);
    let stack_maps = stack_maps(function, &region, &calls);

    OptimizingPlan {
        fast_path: FastPathPlan {
            entry: region.entry,
            values: region.values.clone(),
            exits_requiring_boxing,
        },
        cold_twin: ColdTwinPlan {
            entry: region.entry,
            blocks: region.blocks.clone(),
            calls,
            stack_maps,
        },
        region,
        entry_guards,
    }
}

/// Linearize a plan into the order the eventual CLIF lowering will emit.
#[must_use]
pub fn lowering_steps(plan: &OptimizingPlan) -> Vec<LoweringStep> {
    let mut steps = Vec::new();
    steps.extend(plan.entry_guards.iter().copied().map(LoweringStep::EntryGuard));
    steps.extend(
        plan.fast_path
            .values
            .iter()
            .copied()
            .map(LoweringStep::UnboxedFastPathValue),
    );
    steps.extend(
        plan.fast_path
            .exits_requiring_boxing
            .iter()
            .copied()
            .map(LoweringStep::BoxForFastPathExit),
    );
    steps.extend(plan.cold_twin.calls.iter().copied().map(LoweringStep::BoxedColdTwinCall));
    steps.extend(plan.cold_twin.stack_maps.iter().cloned().map(LoweringStep::StackMap));
    steps
}

/// Lower the boxed cold twin for `region` by delegating every instruction to the
/// baseline sub-region hook.
///
/// This is not called by the public baseline path.  It exists so the future
/// optimizing entry point can share the exact boxed semantics with tier-0 codegen
/// instead of forking helper calls, name remapping, NULL checks, or local-slot
/// rules.
#[allow(dead_code, clippy::too_many_arguments, reason = "Phase-D cold-twin lowering is reserved until typed tier entry wiring lands")]
pub(crate) fn lower_boxed_cold_twin<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    func_ids: &[FuncId],
    names: &NameMap,
    state: &mut LowerState,
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
    function: &Function,
    region: &TypedRegion,
) -> Result<(), CodegenError> {
    let mut blocks = Vec::with_capacity(region.blocks.len());
    for block_id in &region.blocks {
        let Some(block) = block_by_id(function, *block_id) else {
            return Err(CodegenError::Unsupported("typed region references missing block"));
        };
        blocks.push(block);
    }

    baseline::lower_boxed_subregion(
        module,
        builder,
        helpers,
        func_ids,
        names,
        state,
        ptr_ty,
        ptr_bytes,
        exception_exit,
        &blocks,
    )
}

/// Lower one function through the Phase-D typed fast path selected by `plan`.
///
/// The generated body contains two copies of the function's CFG: a primary path
/// that keeps `IntI64` region values and locals in CLIF `i64` registers, plus a
/// cold boxed twin lowered with the baseline instruction lowerer.  Entry/type
/// guards and divide-by-zero guards side-exit to the cold twin after reboxing any
/// dirty unboxed locals, so unsupported shapes preserve boxed semantics instead
/// of trapping in the optimized path.
#[allow(clippy::too_many_arguments)]
pub fn compile_function<M: Module>(
    module: &mut M,
    helpers: &HelperRefs,
    func_ids: &[FuncId],
    names: &NameMap,
    function: &Function,
    plan: &OptimizingPlan,
    ctx: &mut Context,
    fctx: &mut FunctionBuilderContext,
) -> Result<(), CodegenError> {
    module.clear_context(ctx);
    let ptr_ty = module.target_config().pointer_type();
    let ptr_bytes = ptr_ty.bytes() as usize;

    ctx.func.signature.params.push(AbiParam::new(ptr_ty));
    ctx.func.signature.params.push(AbiParam::new(ptr_ty));
    ctx.func.signature.returns.push(AbiParam::new(ptr_ty));

    let helper_refs = baseline::declare_helper_refs(module, helpers, &mut ctx.func);
    let mut builder = FunctionBuilder::new(&mut ctx.func, fctx);
    let entry = builder.create_block();
    let exception_exit = builder.create_block();
    builder.set_cold_block(exception_exit);
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);

    let argv = builder.func.dfg.block_params(entry)[0];
    let mut fast = FastLowerState::new(&mut builder, function.n_locals, ptr_ty);
    baseline::initialize_parameter_locals(
        &mut builder,
        &mut fast.boxed,
        argv,
        ptr_bytes,
        function.params.total_slot_count().max(function.arity),
        function,
        ptr_ty,
    )?;

    let primary_blocks = make_block_map(function, entry, &mut builder);
    let cold_blocks = make_cold_block_map(function, &mut builder);
    append_cold_region_entry_params(&mut builder, &cold_blocks, &plan.region, ptr_ty)?;
    let int_type = initialize_int_type(&mut builder, &helper_refs, ptr_ty, exception_exit);
    let region_blocks = region_block_set(&plan.region);
    let range_iter_specs = range_iter_specs(function);

    lower_primary_copy(
        module,
        &mut builder,
        &helper_refs,
        func_ids,
        names,
        &mut fast,
        ptr_ty,
        ptr_bytes,
        exception_exit,
        function,
        &plan.region,
        &region_blocks,
        &primary_blocks,
        &cold_blocks,
        int_type,
        &range_iter_specs,
    )?;

    lower_cold_copy(
        module,
        &mut builder,
        &helper_refs,
        func_ids,
        names,
        &fast,
        ptr_ty,
        ptr_bytes,
        exception_exit,
        function,
        &plan.region,
        &cold_blocks,
    )?;

    builder.switch_to_block(exception_exit);
    let null = builder.ins().iconst(ptr_ty, 0);
    builder.ins().return_(&[null]);
    builder.seal_all_blocks();
    builder.finalize();
    Ok(())
}


#[derive(Clone, Copy)]
struct FastValue {
    ty: Type,
    value: ir::Value,
}

#[derive(Clone, Copy)]
struct FastForNext {
    has_item: ir::Value,
}

#[derive(Clone, Copy)]
struct RangeIterSpec {
    start: i64,
    stop: i64,
    step: i64,
}

struct FastRangeIter {
    current: Variable,
    initialized: bool,
}

struct FastLowerState {
    boxed: LowerState,
    unboxed_values: HashMap<IrValue, FastValue>,
    int_locals: Vec<Variable>,
    int_local_defined: Vec<bool>,
    int_local_dirty: Vec<bool>,
    cold_exit_args: Vec<ir::Value>,
    range_iters: HashMap<IrValue, FastRangeIter>,
    last_fast_for_next: Option<FastForNext>,
}

impl FastLowerState {
    fn new(builder: &mut FunctionBuilder<'_>, local_count: usize, ptr_ty: ir::Type) -> Self {
        let mut boxed = LowerState::new(local_count);
        let mut int_locals = Vec::with_capacity(local_count);
        for _ in 0..local_count {
            boxed.locals.push(builder.declare_var(ptr_ty));
            int_locals.push(builder.declare_var(types::I64));
        }
        Self {
            boxed,
            unboxed_values: HashMap::new(),
            int_locals,
            int_local_defined: vec![false; local_count],
            int_local_dirty: vec![false; local_count],
            cold_exit_args: Vec::new(),
            range_iters: HashMap::new(),
            last_fast_for_next: None,
        }
    }

    fn define_unboxed(&mut self, ir_value: IrValue, ty: Type, value: ir::Value) {
        self.unboxed_values.insert(ir_value, FastValue { ty, value });
    }

    fn unboxed(&self, ir_value: IrValue) -> Option<FastValue> {
        self.unboxed_values.get(&ir_value).copied()
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_primary_copy<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    func_ids: &[FuncId],
    names: &NameMap,
    state: &mut FastLowerState,
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
    function: &Function,
    region: &TypedRegion,
    region_blocks: &[BlockId],
    primary_blocks: &[(BlockId, ir::Block)],
    cold_blocks: &[(BlockId, ir::Block)],
    int_type: ir::Value,
    range_iter_specs: &HashMap<IrValue, RangeIterSpec>,
) -> Result<(), CodegenError> {
    for block in &function.blocks {
        let clif_block = find_clif_block(primary_blocks, block.id)?;
        if block.id.0 != 0 {
            builder.switch_to_block(clif_block);
        }
        let in_region = region_blocks.contains(&block.id);
        if block.id == region.entry {
            define_trusted_region_live_ins(
                builder,
                helpers,
                state,
                ptr_ty,
                ptr_bytes,
                exception_exit,
                function,
                region,
                cold_blocks,
                int_type,
            )?;
        }
        for inst in &block.insts {
            let lowered = if in_region {
                lower_fast_inst(
                    module,
                    builder,
                    helpers,
                    func_ids,
                    names,
                    state,
                    ptr_ty,
                    ptr_bytes,
                    exception_exit,
                    inst,
                    block.id,
                    cold_blocks,
                    int_type,
                    range_iter_specs,
                    function,
                )?
            } else {
                None
            };
            let value = match lowered {
                Some(value) => value,
                None => lower_baseline_inst(
                    module,
                    builder,
                    helpers,
                    func_ids,
                    names,
                    state,
                    ptr_ty,
                    ptr_bytes,
                    exception_exit,
                    inst,
                )?,
            };
            if !state.unboxed_values.contains_key(&inst.result) {
                state.boxed.define_value(inst.result, value);
            }
        }
        lower_primary_terminator(
            builder,
            helpers,
            state,
            ptr_ty,
            ptr_bytes,
            exception_exit,
            function,
            region,
            cold_blocks,
            int_type,
            block.id,
            &block.term,
            primary_blocks,
        )?;
    }
    Ok(())
}

fn define_trusted_region_live_ins(
    builder: &mut FunctionBuilder<'_>,
    _helpers: &HelperFuncRefs,
    state: &mut FastLowerState,
    _ptr_ty: ir::Type,
    ptr_bytes: usize,
    _exception_exit: ir::Block,
    function: &Function,
    region: &TypedRegion,
    _cold_blocks: &[(BlockId, ir::Block)],
    _int_type: ir::Value,
) -> Result<(), CodegenError> {
    state.cold_exit_args = region
        .live_ins
        .iter()
        .map(|input| state.boxed.value(input.value))
        .collect::<Result<Vec<_>, _>>()?;
    for input in &region.live_ins {
        if input.ty != Type::IntI64 || state.unboxed(input.value).is_some() || !is_for_next_result(function, input.value) {
            continue;
        }
        let boxed = state.boxed.value(input.value)?;
        let offset = pylong_value_offset_i32(ptr_bytes)?;
        let value = builder.ins().load(types::I64, MemFlagsData::new(), boxed, offset);
        state.define_unboxed(input.value, Type::IntI64, value);
    }
    Ok(())
}

fn is_for_next_result(function: &Function, value: IrValue) -> bool {
    function
        .blocks
        .iter()
        .flat_map(|block| block.insts.iter())
        .any(|inst| inst.result == value && matches!(inst.kind, InstKind::ForNext { .. }))
}

fn entry_load_before_store_int_locals(function: &Function, region: &TypedRegion) -> Vec<u32> {
    let mut slots = Vec::new();
    let mut stored = Vec::new();
    for block_id in &region.blocks {
        let Some(block) = block_by_id(function, *block_id) else {
            continue;
        };
        for inst in &block.insts {
            match &inst.kind {
                InstKind::LoadLocal(slot)
                    if region::inst_unboxed_type(inst) == Some(Type::IntI64)
                        && !stored.contains(&slot.0)
                        && !slots.contains(&slot.0) =>
                {
                    slots.push(slot.0);
                }
                InstKind::StoreLocal(slot, _) if region::inst_unboxed_type(inst) == Some(Type::IntI64) => {
                    stored.push(slot.0);
                }
                _ => {}
            }
        }
    }
    slots
}


fn range_iter_specs(function: &Function) -> HashMap<IrValue, RangeIterSpec> {
    let mut const_ints = HashMap::<IrValue, i64>::new();
    let mut call_args = HashMap::<IrValue, Vec<IrValue>>::new();
    let mut get_iters = HashMap::<IrValue, IrValue>::new();
    let mut specs = HashMap::new();

    for block in &function.blocks {
        for inst in &block.insts {
            match &inst.kind {
                InstKind::Const(PyConst::Int(value)) => {
                    const_ints.insert(inst.result, *value);
                }
                InstKind::Call { args, .. } => {
                    call_args.insert(inst.result, args.clone());
                }
                InstKind::GetIter { iterable } => {
                    get_iters.insert(inst.result, *iterable);
                }
                InstKind::ForNext { iter } if region::inst_unboxed_type(inst) == Some(Type::IntI64) => {
                    let Some(iterable) = get_iters.get(iter) else {
                        continue;
                    };
                    let Some(args) = call_args.get(iterable) else {
                        continue;
                    };
                    let Some(spec) = range_spec_from_args(args, &const_ints) else {
                        continue;
                    };
                    specs.insert(*iter, spec);
                }
                _ => {}
            }
        }
    }

    specs
}

fn range_spec_from_args(args: &[IrValue], const_ints: &HashMap<IrValue, i64>) -> Option<RangeIterSpec> {
    match args {
        [stop] => Some(RangeIterSpec {
            start: 0,
            stop: *const_ints.get(stop)?,
            step: 1,
        }),
        [start, stop] => Some(RangeIterSpec {
            start: *const_ints.get(start)?,
            stop: *const_ints.get(stop)?,
            step: 1,
        }),
        [start, stop, step] => Some(RangeIterSpec {
            start: *const_ints.get(start)?,
            stop: *const_ints.get(stop)?,
            step: *const_ints.get(step)?,
        }),
        _ => None,
    }
}

fn const_int_value(function: &Function, value: IrValue) -> Option<i64> {
    function
        .blocks
        .iter()
        .flat_map(|block| block.insts.iter())
        .find_map(|inst| match (&inst.kind, inst.result == value) {
            (InstKind::Const(PyConst::Int(value)), true) => Some(*value),
            _ => None,
        })
}
fn formal_local_slots(function: &Function) -> Vec<usize> {
    if function.params.total_slot_count() == 0 {
        return (0..function.arity.min(function.n_locals)).collect();
    }
    let positional = function.params.positional_arity();
    let keyword_only_local = positional + usize::from(function.params.vararg_name.is_some());
    let mut slots: Vec<usize> = (0..positional).collect();
    slots.extend((0..function.params.keyword_only_count).map(|index| keyword_only_local + index));
    if function.params.vararg_name.is_some() {
        slots.push(positional);
    }
    if function.params.kwarg_name.is_some() {
        slots.push(keyword_only_local + function.params.keyword_only_count);
    }
    slots
}

#[allow(clippy::too_many_arguments)]
fn lower_cold_copy<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    func_ids: &[FuncId],
    names: &NameMap,
    fast_state: &FastLowerState,
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
    function: &Function,
    region: &TypedRegion,
    cold_blocks: &[(BlockId, ir::Block)],
) -> Result<(), CodegenError> {
    let mut cold_state = LowerState::new(function.n_locals);
    cold_state.locals = fast_state.boxed.locals.clone();
    cold_state.local_defined = vec![false; function.n_locals];
    for slot in formal_local_slots(function) {
        if slot < cold_state.local_defined.len() {
            cold_state.local_defined[slot] = true;
        }
    }

    for block in &function.blocks {
        builder.switch_to_block(find_clif_block(cold_blocks, block.id)?);
        if block.id == region.entry {
            define_cold_region_live_in_params(builder, &mut cold_state, region, cold_blocks)?;
        }
        for inst in &block.insts {
            let value = baseline::lower_inst(
                module,
                builder,
                helpers,
                func_ids,
                &[],
                names,
                &mut cold_state,
                ptr_ty,
                ptr_bytes,
                exception_exit,
                &inst.kind,
                None,
            )?;
            cold_state.define_value(inst.result, value);
        }
        lower_cold_terminator_with_region_args(
            builder,
            &cold_state,
            helpers,
            ptr_ty,
            exception_exit,
            cold_blocks,
            region,
            block.id,
            &block.term,
        )?;
    }
    Ok(())
}

fn define_cold_region_live_in_params(
    builder: &mut FunctionBuilder<'_>,
    state: &mut LowerState,
    region: &TypedRegion,
    cold_blocks: &[(BlockId, ir::Block)],
) -> Result<(), CodegenError> {
    let cold_entry = find_clif_block(cold_blocks, region.entry)?;
    let params = builder.func.dfg.block_params(cold_entry).to_vec();
    for (input, param) in region.live_ins.iter().zip(params) {
        state.define_value(input.value, param);
    }
    Ok(())
}

fn lower_cold_terminator_with_region_args(
    builder: &mut FunctionBuilder<'_>,
    state: &LowerState,
    helpers: &HelperFuncRefs,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
    cold_blocks: &[(BlockId, ir::Block)],
    region: &TypedRegion,
    current_block: BlockId,
    term: &Terminator,
) -> Result<(), CodegenError> {
    match term {
        Terminator::Jump(target) if *target == region.entry => {
            let target = find_clif_block(cold_blocks, *target)?;
            let args = block_args(&cold_region_entry_args(state, region)?);
            builder.ins().jump(target, &args);
            Ok(())
        }
        Terminator::ForLoop { body, done, .. } if *body == region.entry => {
            let item = state
                .last_value()
                .ok_or(CodegenError::Unsupported("ForLoop without preceding ForNext"))?;
            let body = find_clif_block(cold_blocks, *body)?;
            let done = find_clif_block(cold_blocks, *done)?;
            let stop_check = builder.create_block();
            let args = block_args(&cold_region_entry_args(state, region)?);
            let has_item = builder.ins().icmp_imm(IntCC::NotEqual, item, 0);
            builder.ins().brif(has_item, body, &args, stop_check, &[]);
            builder.switch_to_block(stop_check);
            builder.seal_block(stop_check);
            let stop_value = baseline::call_pyobject_helper(builder, helpers.gen_stop_value, &[], ptr_ty, exception_exit);
            let stopped = builder.ins().icmp_imm(IntCC::NotEqual, stop_value, 0);
            builder.ins().brif(stopped, done, &[], exception_exit, &[]);
            Ok(())
        }
        _ => control::lower_terminator_with_blocks(
            builder,
            state,
            helpers,
            ptr_ty,
            exception_exit,
            cold_blocks,
            current_block,
            term,
        ),
    }
}

fn cold_region_entry_args(state: &LowerState, region: &TypedRegion) -> Result<Vec<ir::Value>, CodegenError> {
    region
        .live_ins
        .iter()
        .map(|input| state.value(input.value))
        .collect()
}

fn block_args(values: &[ir::Value]) -> Vec<ir::BlockArg> {
    values.iter().copied().map(ir::BlockArg::Value).collect()
}

#[allow(clippy::too_many_arguments)]
fn lower_fast_inst<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    func_ids: &[FuncId],
    names: &NameMap,
    state: &mut FastLowerState,
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
    inst: &Inst,
    block_id: BlockId,
    cold_blocks: &[(BlockId, ir::Block)],
    int_type: ir::Value,
    range_iter_specs: &HashMap<IrValue, RangeIterSpec>,
    function: &Function,
) -> Result<Option<ir::Value>, CodegenError> {
    let cold_target = find_clif_block(cold_blocks, block_id)?;
    match &inst.kind {
        InstKind::Const(PyConst::Int(value)) if region::inst_unboxed_type(inst) == Some(Type::IntI64) => {
            let value = builder.ins().iconst(types::I64, *value);
            state.define_unboxed(inst.result, Type::IntI64, value);
            Ok(Some(value))
        }
        InstKind::ForNext { iter }
            if region::inst_unboxed_type(inst) == Some(Type::IntI64)
                && range_iter_specs.contains_key(iter) =>
        {
            let spec = range_iter_specs[iter];
            let value = lower_range_for_next(builder, state, inst.result, *iter, spec)?;
            Ok(Some(value))
        }
        InstKind::LoadLocal(slot) if region::inst_unboxed_type(inst) == Some(Type::IntI64) => {
            let value = load_unboxed_local(builder, helpers, state, ptr_ty, ptr_bytes, exception_exit, slot.0, cold_target, int_type)?;
            state.define_unboxed(inst.result, Type::IntI64, value);
            Ok(Some(value))
        }
        InstKind::StoreLocal(slot, value) if region::inst_unboxed_type(inst) == Some(Type::IntI64) => {
            let Some(value) = state.unboxed(*value).filter(|value| value.ty == Type::IntI64).map(|value| value.value) else {
                return Ok(None);
            };
            store_unboxed_local(builder, state, slot.0, value)?;
            state.define_unboxed(inst.result, Type::IntI64, value);
            Ok(Some(value))
        }
        InstKind::BinaryOp { op, lhs, rhs } if region::inst_unboxed_type(inst) == Some(Type::IntI64) => {
            let Some(lhs) = state.unboxed(*lhs).filter(|value| value.ty == Type::IntI64).map(|value| value.value) else {
                return Ok(None);
            };
            let rhs_non_zero = const_int_value(function, *rhs).is_some_and(|value| value != 0);
            let Some(rhs_value) = state.unboxed(*rhs).filter(|value| value.ty == Type::IntI64).map(|value| value.value) else {
                return Ok(None);
            };
            let value = lower_binary_i64(builder, helpers, state, ptr_ty, exception_exit, *op, lhs, rhs_value, cold_target, rhs_non_zero)?;
            state.define_unboxed(inst.result, Type::IntI64, value);
            Ok(Some(value))
        }
        InstKind::UnaryOp { op, operand } if region::inst_unboxed_type(inst) == Some(Type::IntI64) => {
            let Some(operand) = state.unboxed(*operand).filter(|value| value.ty == Type::IntI64).map(|value| value.value) else {
                return Ok(None);
            };
            let value = match op {
                UnOp::Neg => builder.ins().ineg(operand),
                UnOp::Pos => operand,
                _ => return Ok(None),
            };
            state.define_unboxed(inst.result, Type::IntI64, value);
            Ok(Some(value))
        }
        InstKind::Compare { op, lhs, rhs } => {
            let Some(lhs) = state.unboxed(*lhs).filter(|value| value.ty == Type::IntI64).map(|value| value.value) else {
                return Ok(None);
            };
            let Some(rhs) = state.unboxed(*rhs).filter(|value| value.ty == Type::IntI64).map(|value| value.value) else {
                return Ok(None);
            };
            let cc = compare_cc(*op);
            let cmp = builder.ins().icmp(cc, lhs, rhs);
            let value = bool_to_i64(builder, cmp);
            state.define_unboxed(inst.result, Type::IntI64, value);
            Ok(Some(value))
        }
        InstKind::BoolTest { val } | InstKind::Not { val } => {
            let Some(val) = state.unboxed(*val).filter(|value| value.ty == Type::IntI64).map(|value| value.value) else {
                return Ok(None);
            };
            let cc = if matches!(&inst.kind, InstKind::Not { .. }) {
                IntCC::Equal
            } else {
                IntCC::NotEqual
            };
            let cmp = builder.ins().icmp_imm(cc, val, 0);
            let value = bool_to_i64(builder, cmp);
            state.define_unboxed(inst.result, Type::IntI64, value);

            Ok(Some(value))
        }
        _ => {
            let _ = (module, func_ids, names);
            Ok(None)
        }
    }
}

fn lower_range_for_next(
    builder: &mut FunctionBuilder<'_>,
    state: &mut FastLowerState,
    result: IrValue,
    iter: IrValue,
    spec: RangeIterSpec,
) -> Result<ir::Value, CodegenError> {
    if spec.step == 0 {
        return Err(CodegenError::Unsupported("typed range iterator with zero step"));
    }
    ensure_fast_range_iter(builder, state, iter);
    let range = state
        .range_iters
        .get(&iter)
        .ok_or(CodegenError::Unsupported("missing typed range iterator state"))?;
    let current = if range.initialized {
        builder.use_var(range.current)
    } else {
        let start = builder.ins().iconst(types::I64, spec.start);
        builder.def_var(range.current, start);
        start
    };
    let stop = builder.ins().iconst(types::I64, spec.stop);
    let cc = if spec.step > 0 {
        IntCC::SignedLessThan
    } else {
        IntCC::SignedGreaterThan
    };
    let has_item = builder.ins().icmp(cc, current, stop);
    let step = builder.ins().iconst(types::I64, spec.step);
    let next = builder.ins().iadd(current, step);
    let range = state
        .range_iters
        .get_mut(&iter)
        .ok_or(CodegenError::Unsupported("missing typed range iterator state"))?;
    builder.def_var(range.current, next);
    range.initialized = true;
    state.define_unboxed(result, Type::IntI64, current);
    state.last_fast_for_next = Some(FastForNext { has_item });
    Ok(current)
}

fn ensure_fast_range_iter(builder: &mut FunctionBuilder<'_>, state: &mut FastLowerState, iter: IrValue) {
    if state.range_iters.contains_key(&iter) {
        return;
    }
    let current = builder.declare_var(types::I64);
    state.range_iters.insert(
        iter,
        FastRangeIter {
            current,
            initialized: false,
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn lower_baseline_inst<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    func_ids: &[FuncId],
    names: &NameMap,
    state: &mut FastLowerState,
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
    inst: &Inst,
) -> Result<ir::Value, CodegenError> {
    sync_for_baseline_inst(builder, helpers, state, ptr_ty, exception_exit, &inst.kind)?;
    let value = baseline::lower_inst(
        module,
        builder,
        helpers,
        func_ids,
        &[],
        names,
        &mut state.boxed,
        ptr_ty,
        ptr_bytes,
        exception_exit,
        &inst.kind,
        None,
    )?;
    if let InstKind::StoreLocal(slot, _) = &inst.kind {
        let index = slot.0 as usize;
        if index < state.int_local_dirty.len() {
            state.int_local_dirty[index] = false;
            state.int_local_defined[index] = false;
        }
    }
    Ok(value)
}

#[allow(clippy::too_many_arguments)]
fn lower_primary_terminator(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    state: &mut FastLowerState,
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
    function: &Function,
    region: &TypedRegion,
    cold_blocks: &[(BlockId, ir::Block)],
    int_type: ir::Value,
    current_block: BlockId,
    term: &Terminator,
    block_map: &[(BlockId, ir::Block)],
) -> Result<(), CodegenError> {
    match term {
        Terminator::Return(value) => {
            let value = ensure_boxed_value(builder, helpers, state, ptr_ty, exception_exit, *value)?;
            builder.ins().return_(&[value]);
            Ok(())
        }
        Terminator::Jump(target) => {
            preload_region_entry_locals_for_successor(
                builder,
                helpers,
                state,
                ptr_ty,
                ptr_bytes,
                exception_exit,
                function,
                region,
                cold_blocks,
                int_type,
                current_block,
                *target,
            )?;
            control::lower_terminator_with_blocks(
                builder,
                &state.boxed,
                helpers,
                ptr_ty,
                exception_exit,
                block_map,
                current_block,
                term,
            )
        }
        Terminator::ForLoop { body, done, .. } => {
            if let Some(fast_next) = state.last_fast_for_next.take() {
                let body = find_clif_block(block_map, *body)?;
                let done = find_clif_block(block_map, *done)?;
                builder.ins().brif(fast_next.has_item, body, &[], done, &[]);
                return Ok(());
            }
            preload_region_entry_locals_for_successor(
                builder,
                helpers,
                state,
                ptr_ty,
                ptr_bytes,
                exception_exit,
                function,
                region,
                cold_blocks,
                int_type,
                current_block,
                *body,
            )?;
            control::lower_terminator_with_blocks(
                builder,
                &state.boxed,
                helpers,
                ptr_ty,
                exception_exit,
                block_map,
                current_block,
                term,
            )
        }
        Terminator::Branch { cond, then_blk, else_blk } => {
            preload_region_entry_locals_for_successor(
                builder,
                helpers,
                state,
                ptr_ty,
                ptr_bytes,
                exception_exit,
                function,
                region,
                cold_blocks,
                int_type,
                current_block,
                *then_blk,
            )?;
            preload_region_entry_locals_for_successor(
                builder,
                helpers,
                state,
                ptr_ty,
                ptr_bytes,
                exception_exit,
                function,
                region,
                cold_blocks,
                int_type,
                current_block,
                *else_blk,
            )?;
            if let Some(cond) = state.unboxed(*cond).filter(|value| value.ty == Type::IntI64).map(|value| value.value) {
                lower_unboxed_branch(builder, cond, *then_blk, *else_blk, block_map)
            } else {
                ensure_boxed_value(builder, helpers, state, ptr_ty, exception_exit, *cond)?;
                control::lower_terminator_with_blocks(
                    builder,
                    &state.boxed,
                    helpers,
                    ptr_ty,
                    exception_exit,
                    block_map,
                    current_block,
                    term,
                )
            }
        }
        Terminator::CondBranch { cond, then_, else_ } => {
            preload_region_entry_locals_for_successor(
                builder,
                helpers,
                state,
                ptr_ty,
                ptr_bytes,
                exception_exit,
                function,
                region,
                cold_blocks,
                int_type,
                current_block,
                *then_,
            )?;
            preload_region_entry_locals_for_successor(
                builder,
                helpers,
                state,
                ptr_ty,
                ptr_bytes,
                exception_exit,
                function,
                region,
                cold_blocks,
                int_type,
                current_block,
                *else_,
            )?;
            if let Some(cond) = state.unboxed(*cond).filter(|value| value.ty == Type::IntI64).map(|value| value.value) {
                lower_unboxed_branch(builder, cond, *then_, *else_, block_map)
            } else {
                ensure_boxed_value(builder, helpers, state, ptr_ty, exception_exit, *cond)?;
                control::lower_terminator_with_blocks(
                    builder,
                    &state.boxed,
                    helpers,
                    ptr_ty,
                    exception_exit,
                    block_map,
                    current_block,
                    term,
                )
            }
        }
        _ => {
            for operand in region::terminator_operands(term) {
                ensure_boxed_value(builder, helpers, state, ptr_ty, exception_exit, operand)?;
            }
            control::lower_terminator_with_blocks(
                builder,
                &state.boxed,
                helpers,
                ptr_ty,
                exception_exit,
                block_map,
                current_block,
                term,
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn preload_region_entry_locals_for_successor(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    state: &mut FastLowerState,
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
    function: &Function,
    region: &TypedRegion,
    cold_blocks: &[(BlockId, ir::Block)],
    int_type: ir::Value,
    current_block: BlockId,
    successor: BlockId,
) -> Result<(), CodegenError> {
    if !successor_flows_to_region_entry(function, successor, region.entry) {
        return Ok(());
    }
    let cold_target = find_clif_block(cold_blocks, current_block)?;
    if !builder.func.dfg.block_params(cold_target).is_empty() && state.cold_exit_args.is_empty() {
        state.cold_exit_args = region
            .live_ins
            .iter()
            .map(|input| state.boxed.value(input.value))
            .collect::<Result<Vec<_>, _>>()?;
    }
    preload_range_iters_for_successor(builder, state, function, successor)?;
    for slot in entry_load_before_store_int_locals(function, region) {
        let index = slot as usize;
        if index >= state.int_local_defined.len() || state.int_local_defined[index] {
            continue;
        }
        let value = load_unboxed_local(
            builder,
            helpers,
            state,
            ptr_ty,
            ptr_bytes,
            exception_exit,
            slot,
            cold_target,
            int_type,
        )?;
        builder.def_var(state.int_locals[index], value);
        state.int_local_defined[index] = true;
    }
    Ok(())
}

fn preload_range_iters_for_successor(
    builder: &mut FunctionBuilder<'_>,
    state: &mut FastLowerState,
    function: &Function,
    successor: BlockId,
) -> Result<(), CodegenError> {
    let Some(block) = block_by_id(function, successor) else {
        return Ok(());
    };
    let specs = range_iter_specs(function);
    for inst in &block.insts {
        let InstKind::ForNext { iter } = inst.kind else {
            continue;
        };
        let Some(spec) = specs.get(&iter).copied() else {
            continue;
        };
        ensure_fast_range_iter(builder, state, iter);
        let range = state
            .range_iters
            .get_mut(&iter)
            .ok_or(CodegenError::Unsupported("missing typed range iterator state"))?;
        if range.initialized {
            continue;
        }
        let start = builder.ins().iconst(types::I64, spec.start);
        builder.def_var(range.current, start);
        range.initialized = true;
    }
    Ok(())
}

fn successor_flows_to_region_entry(function: &Function, successor: BlockId, region_entry: BlockId) -> bool {
    if successor == region_entry {
        return true;
    }
    block_by_id(function, successor).is_some_and(|block| {
        matches!(&block.term, Terminator::ForLoop { body, .. } if *body == region_entry)
    })
}

fn lower_unboxed_branch(
    builder: &mut FunctionBuilder<'_>,
    cond: ir::Value,
    then_blk: BlockId,
    else_blk: BlockId,
    block_map: &[(BlockId, ir::Block)],
) -> Result<(), CodegenError> {
    let then_block = find_clif_block(block_map, then_blk)?;
    let else_block = find_clif_block(block_map, else_blk)?;
    let truthy = builder.ins().icmp_imm(IntCC::NotEqual, cond, 0);
    builder.ins().brif(truthy, then_block, &[], else_block, &[]);
    Ok(())
}

fn sync_for_baseline_inst(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    state: &mut FastLowerState,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
    kind: &InstKind,
) -> Result<(), CodegenError> {
    match kind {
        InstKind::LoadLocal(slot) => sync_dirty_local(builder, helpers, state, ptr_ty, exception_exit, slot.0),
        InstKind::StoreLocal(_, value) => {
            ensure_boxed_value(builder, helpers, state, ptr_ty, exception_exit, *value)?;
            Ok(())
        }
        _ => {
            for operand in region::inst_operands(kind) {
                ensure_boxed_value(builder, helpers, state, ptr_ty, exception_exit, operand)?;
            }
            Ok(())
        }
    }
}

fn ensure_boxed_value(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    state: &mut FastLowerState,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
    value: IrValue,
) -> Result<ir::Value, CodegenError> {
    if let Ok(value) = state.boxed.value(value) {
        return Ok(value);
    }
    let Some(fast) = state.unboxed(value) else {
        return Err(CodegenError::ValueNotDefined(value));
    };
    let boxed = match fast.ty {
        Type::IntI64 => box_i64(builder, helpers, ptr_ty, exception_exit, fast.value),
        _ => return Err(CodegenError::Unsupported("typed fast path can only rebox IntI64 values")),
    };
    state.boxed.define_value(value, boxed);
    Ok(boxed)
}

fn sync_dirty_local(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    state: &mut FastLowerState,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
    slot: u32,
) -> Result<(), CodegenError> {
    let index = slot as usize;
    if index >= state.int_local_dirty.len() || !state.int_local_dirty[index] {
        return Ok(());
    }
    let value = builder.use_var(state.int_locals[index]);
    let boxed = box_i64(builder, helpers, ptr_ty, exception_exit, value);
    baseline::store_local(builder, &mut state.boxed, slot, boxed)?;
    state.int_local_dirty[index] = false;
    Ok(())
}

fn sync_all_dirty_locals(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    state: &mut FastLowerState,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<(), CodegenError> {
    for slot in 0..state.int_local_dirty.len() {
        sync_dirty_local(builder, helpers, state, ptr_ty, exception_exit, slot as u32)?;
    }
    Ok(())
}

fn load_unboxed_local(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    state: &mut FastLowerState,
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
    slot: u32,
    cold_target: ir::Block,
    int_type: ir::Value,
) -> Result<ir::Value, CodegenError> {
    let index = slot as usize;
    if index >= state.int_locals.len() {
        return Err(CodegenError::LocalOutOfRange { slot, n_locals: state.int_locals.len() });
    }
    if state.int_local_defined[index] {
        return Ok(builder.use_var(state.int_locals[index]));
    }
    sync_dirty_local(builder, helpers, state, ptr_ty, exception_exit, slot)?;
    let boxed = baseline::load_local(builder, &state.boxed, slot)?;
    let value = guard_and_unbox_i64(builder, helpers, state, ptr_ty, ptr_bytes, exception_exit, boxed, int_type, cold_target)?;
    builder.def_var(state.int_locals[index], value);
    state.int_local_defined[index] = true;
    Ok(value)
}

fn store_unboxed_local(
    builder: &mut FunctionBuilder<'_>,
    state: &mut FastLowerState,
    slot: u32,
    value: ir::Value,
) -> Result<(), CodegenError> {
    let index = slot as usize;
    if index >= state.int_locals.len() {
        return Err(CodegenError::LocalOutOfRange { slot, n_locals: state.int_locals.len() });
    }
    builder.def_var(state.int_locals[index], value);
    state.int_local_defined[index] = true;
    state.int_local_dirty[index] = true;
    Ok(())
}

fn guard_and_unbox_i64(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    state: &mut FastLowerState,
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
    object: ir::Value,
    int_type: ir::Value,
    cold_target: ir::Block,
) -> Result<ir::Value, CodegenError> {
    guard_ptr_non_null(builder, helpers, state, ptr_ty, exception_exit, object, cold_target)?;
    let object_type = builder.ins().load(ptr_ty, MemFlagsData::new(), object, 0);
    let is_int = builder.ins().icmp(IntCC::Equal, object_type, int_type);
    side_exit_unless(builder, helpers, state, ptr_ty, exception_exit, is_int, cold_target)?;
    let offset = pylong_value_offset_i32(ptr_bytes)?;
    Ok(builder.ins().load(types::I64, MemFlagsData::new(), object, offset))
}

pub(crate) fn pylong_value_offset_i32(ptr_bytes: usize) -> Result<i32, CodegenError> {
    baseline::offset_i32(ptr_bytes * 2)
}

fn guard_ptr_non_null(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    state: &mut FastLowerState,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
    object: ir::Value,
    cold_target: ir::Block,
) -> Result<(), CodegenError> {
    let non_null = builder.ins().icmp_imm(IntCC::NotEqual, object, 0);
    side_exit_unless(builder, helpers, state, ptr_ty, exception_exit, non_null, cold_target)
}

fn guard_i64_non_zero(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    state: &mut FastLowerState,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
    value: ir::Value,
    cold_target: ir::Block,
) -> Result<(), CodegenError> {
    let non_zero = builder.ins().icmp_imm(IntCC::NotEqual, value, 0);
    side_exit_unless(builder, helpers, state, ptr_ty, exception_exit, non_zero, cold_target)
}

fn side_exit_unless(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    state: &mut FastLowerState,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
    cond: ir::Value,
    cold_target: ir::Block,
) -> Result<(), CodegenError> {
    let ok = builder.create_block();
    let fail = builder.create_block();
    builder.set_cold_block(fail);
    builder.ins().brif(cond, ok, &[], fail, &[]);

    builder.switch_to_block(fail);
    sync_all_dirty_locals(builder, helpers, state, ptr_ty, exception_exit)?;
    let target_args = if builder.func.dfg.block_params(cold_target).is_empty() {
        Vec::new()
    } else {
        block_args(&state.cold_exit_args)
    };
    builder.ins().jump(cold_target, &target_args);
    builder.seal_block(fail);

    builder.switch_to_block(ok);
    builder.seal_block(ok);
    Ok(())
}

fn box_i64(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
    value: ir::Value,
) -> ir::Value {
    baseline::call_pyobject_helper(builder, helpers.const_int, &[value], ptr_ty, exception_exit)
}

fn initialize_int_type(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> ir::Value {
    let zero = builder.ins().iconst(types::I64, 0);
    let object = box_i64(builder, helpers, ptr_ty, exception_exit, zero);
    builder.ins().load(ptr_ty, MemFlagsData::new(), object, 0)
}

fn lower_binary_i64(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    state: &mut FastLowerState,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
    op: BinOp,
    lhs: ir::Value,
    rhs: ir::Value,
    cold_target: ir::Block,
    rhs_non_zero: bool,
) -> Result<ir::Value, CodegenError> {
    let value = match op {
        BinOp::Add => builder.ins().iadd(lhs, rhs),
        BinOp::Sub => builder.ins().isub(lhs, rhs),
        BinOp::Mul => builder.ins().imul(lhs, rhs),
        BinOp::FloorDiv => {
            if !rhs_non_zero {
                guard_i64_non_zero(builder, helpers, state, ptr_ty, exception_exit, rhs, cold_target)?;
            }
            builder.ins().sdiv(lhs, rhs)
        }
        BinOp::Mod => {
            if !rhs_non_zero {
                guard_i64_non_zero(builder, helpers, state, ptr_ty, exception_exit, rhs, cold_target)?;
            }
            builder.ins().srem(lhs, rhs)
        }
        _ => return Err(CodegenError::Unsupported("unsupported typed integer binary op")),
    };
    Ok(value)
}

fn compare_cc(op: CmpOp) -> IntCC {
    match op {
        CmpOp::Eq => IntCC::Equal,
        CmpOp::Ne => IntCC::NotEqual,
        CmpOp::Lt => IntCC::SignedLessThan,
        CmpOp::Le => IntCC::SignedLessThanOrEqual,
        CmpOp::Gt => IntCC::SignedGreaterThan,
        CmpOp::Ge => IntCC::SignedGreaterThanOrEqual,
        _ => IntCC::Equal,
    }
}

fn bool_to_i64(builder: &mut FunctionBuilder<'_>, value: ir::Value) -> ir::Value {
    let one = builder.ins().iconst(types::I64, 1);
    let zero = builder.ins().iconst(types::I64, 0);
    builder.ins().select(value, one, zero)
}

fn make_block_map(
    function: &Function,
    entry: ir::Block,
    builder: &mut FunctionBuilder<'_>,
) -> Vec<(BlockId, ir::Block)> {
    function
        .blocks
        .iter()
        .map(|block| {
            if block.id.0 == 0 {
                (block.id, entry)
            } else {
                (block.id, builder.create_block())
            }
        })
        .collect()
}

fn make_cold_block_map(function: &Function, builder: &mut FunctionBuilder<'_>) -> Vec<(BlockId, ir::Block)> {
    function
        .blocks
        .iter()
        .map(|block| {
            let clif_block = builder.create_block();
            builder.set_cold_block(clif_block);
            (block.id, clif_block)
        })
        .collect()
}

fn append_cold_region_entry_params(
    builder: &mut FunctionBuilder<'_>,
    cold_blocks: &[(BlockId, ir::Block)],
    region: &TypedRegion,
    ptr_ty: ir::Type,
) -> Result<(), CodegenError> {
    let cold_entry = find_clif_block(cold_blocks, region.entry)?;
    for _ in &region.live_ins {
        builder.append_block_param(cold_entry, ptr_ty);
    }
    Ok(())
}

fn find_clif_block(blocks: &[(BlockId, ir::Block)], block_id: BlockId) -> Result<ir::Block, CodegenError> {
    blocks
        .iter()
        .find_map(|(id, block)| (*id == block_id).then_some(*block))
        .ok_or(CodegenError::Unsupported("missing basic block"))
}

fn entry_guard(input: TypedInput) -> EntryGuard {
    EntryGuard {
        value: input.value,
        expected: input.ty,
        failure: GuardFailure::ColdTwin,
    }
}

fn values_reaching_region_exits(function: &Function, region: &TypedRegion) -> Vec<IrValue> {
    let region_blocks = region_block_set(region);
    let mut produced = region
        .values
        .iter()
        .map(|value| value.value)
        .collect::<Vec<IrValue>>();
    produced.extend(region.live_ins.iter().map(|input| input.value));

    let mut exits = Vec::new();
    for block in &function.blocks {
        if !region_blocks.contains(&block.id) {
            continue;
        }
        for operand in region::terminator_operands(&block.term) {
            if produced.contains(&operand) && !exits.contains(&operand) {
                exits.push(operand);
            }
        }
    }
    exits
}

fn cold_call_sites(function: &Function, region: &TypedRegion) -> Vec<ColdCallSite> {
    let region_blocks = region_block_set(region);
    let mut calls = Vec::new();
    for block in &function.blocks {
        if !region_blocks.contains(&block.id) {
            continue;
        }
        for (inst_index, inst) in block.insts.iter().enumerate() {
            if boxed_cold_path_call(&inst.kind) {
                calls.push(ColdCallSite {
                    block: block.id,
                    inst_index,
                    result: inst.result,
                });
            }
        }
    }
    calls
}

fn boxed_cold_path_call(kind: &InstKind) -> bool {
    matches!(
        kind,
        InstKind::Const(pon_ir::ir::PyConst::Int(_) | pon_ir::ir::PyConst::Float(_))
            | InstKind::BinaryOp { .. }
            | InstKind::UnaryOp { .. }
    )
}

fn stack_maps(function: &Function, region: &TypedRegion, calls: &[ColdCallSite]) -> Vec<StackMapDecl> {
    let flattened = flatten_region(function, region);
    let positions = flattened
        .iter()
        .enumerate()
        .map(|(position, inst)| ((inst.block, inst.inst_index), position))
        .collect::<HashMap<_, _>>();
    let last_uses = last_uses(function, region, &positions, flattened.len());
    let def_positions = def_positions(region, &positions);

    calls
        .iter()
        .filter_map(|call| {
            let call_position = *positions.get(&(call.block, call.inst_index))?;
            let mut boxed_values = Vec::new();
            for value in region.live_ins.iter().map(|input| input.value) {
                if last_uses.get(&value).is_some_and(|last_use| *last_use >= call_position) {
                    boxed_values.push(value);
                }
            }
            for value in region.values.iter().map(|typed| typed.value) {
                let Some(def_position) = def_positions.get(&value) else {
                    continue;
                };
                if *def_position < call_position
                    && last_uses.get(&value).is_some_and(|last_use| *last_use >= call_position)
                {
                    boxed_values.push(value);
                }
            }
            Some(StackMapDecl {
                call: *call,
                boxed_values,
            })
        })
        .collect()
}

fn last_uses(
    function: &Function,
    region: &TypedRegion,
    positions: &HashMap<(pon_ir::ir::BlockId, usize), usize>,
    terminator_position: usize,
) -> HashMap<IrValue, usize> {
    let region_blocks = region_block_set(region);
    let mut last_uses = HashMap::new();

    for block in &function.blocks {
        if !region_blocks.contains(&block.id) {
            continue;
        }
        for (inst_index, inst) in block.insts.iter().enumerate() {
            let position = positions.get(&(block.id, inst_index)).copied().unwrap_or(terminator_position);
            for operand in region::inst_operands(&inst.kind) {
                last_uses.insert(operand, position);
            }
        }
        for operand in region::terminator_operands(&block.term) {
            last_uses.insert(operand, terminator_position);
        }
    }

    last_uses
}

fn def_positions(
    region: &TypedRegion,
    positions: &HashMap<(pon_ir::ir::BlockId, usize), usize>,
) -> HashMap<IrValue, usize> {
    region
        .values
        .iter()
        .filter_map(|typed| {
            positions
                .get(&(typed.block, typed.inst_index))
                .copied()
                .map(|position| (typed.value, position))
        })
        .collect()
}

fn flatten_region(function: &Function, region: &TypedRegion) -> Vec<FlatInst> {
    let region_blocks = region_block_set(region);
    let mut flattened = Vec::new();
    for block in &function.blocks {
        if !region_blocks.contains(&block.id) {
            continue;
        }
        for (inst_index, _inst) in block.insts.iter().enumerate() {
            flattened.push(FlatInst {
                block: block.id,
                inst_index,
            });
        }
    }
    flattened
}

fn region_block_set(region: &TypedRegion) -> Vec<pon_ir::ir::BlockId> {
    region.blocks.clone()
}

#[allow(dead_code, reason = "reserved for the Phase-D cold-twin lowering path")]
fn block_by_id(function: &Function, block_id: pon_ir::ir::BlockId) -> Option<&IrBlock> {
    function.blocks.iter().find(|block| block.id == block_id)
}

#[derive(Clone, Copy)]
struct FlatInst {
    block: pon_ir::ir::BlockId,
    inst_index: usize,
}


#[cfg(test)]
mod tests {
    use super::*;

    use cranelift_module::Linkage;
    use pon_ir::{
        ir::{
            Block, BlockId, Function, FunctionId, Inst, InstKind, LocalId, Module as IrModule,
            Terminator, Value,
        },
        types::Type,
    };
    use pon_runtime::abi::HELPERS;

    use crate::helpers::declare_helpers;

    fn jit_module() -> cranelift_jit::JITModule {
        let isa = crate::isa::make_isa(crate::isa::OptLevel::None, false);
        let mut builder = cranelift_jit::JITBuilder::with_isa(
            isa,
            cranelift_module::default_libcall_names(),
        );
        for helper in HELPERS {
            builder.symbol(helper.symbol, helper.address.cast::<u8>());
        }
        cranelift_jit::JITModule::new(builder)
    }

    fn compiled_optimized_clif(ir_module: &IrModule, function_index: usize) -> String {
        let mut module = jit_module();
        let helpers = declare_helpers(&mut module).expect("helpers declare");
        let ptr = module.target_config().pointer_type();
        let mut sig = module.make_signature();
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
        let function = &ir_module.functions[function_index];
        let plan = plan_function(function).expect("typed region");
        let mut ctx = module.make_context();
        let mut fctx = FunctionBuilderContext::new();
        compile_function(
            &mut module,
            &helpers,
            &func_ids,
            &names,
            function,
            &plan,
            &mut ctx,
            &mut fctx,
        )
        .expect("optimized function compiles");
        let clif = ctx.func.display().to_string();
        module
            .define_function(func_ids[function_index], &mut ctx)
            .expect("optimized function verifies");
        clif
    }

    #[test]
    fn optimized_int_loop_carries_unboxed_local_through_loop_body() {
        let mut ir = pon_ir::lower_source(
            r#"
def mix(seed):
    acc = seed
    for i in range(32):
        acc = (acc * 1664525 + 1013904223 + i) % 65536
    return acc
"#,
        )
        .expect("int loop source lowers");
        crate::infer_module_types(&mut ir, &crate::ModuleAnnotations::default());
        let function_index = ir
            .functions
            .iter()
            .position(|function| function.name == "mix")
            .expect("mix function");

        let clif = compiled_optimized_clif(&ir, function_index);

        assert!(clif.contains("imul"));
        assert!(clif.contains("iadd"));
        assert!(clif.contains("srem"));
        assert!(
            clif.contains("jump block2(") && (clif.contains(", v49)") || clif.contains(", v50)")),
            "expected the loop backedge to carry the updated unboxed accumulator, got:\n{clif}"
        );
    }

    #[test]
    fn optimized_load_local_reloads_boxed_local_for_cold_guard_path() {
        let ir = IrModule {
            functions: vec![Function {
                name: "reload_arg".to_owned(),
                arity: 1,
                is_coroutine: false, is_generator: false,
                params: Default::default(),
                n_locals: 1,
                blocks: vec![Block {
                    id: BlockId(0),
                    insts: vec![
                        // The first unboxed read of an int-typed local must reload the
                        // boxed slot before it can guard and unbox. This keeps the
                        // baseline::load_local mutability contract covered by a real
                        // optimizing-tier lowering shape rather than a performance check.
                        Inst::new(Value(0), InstKind::LoadLocal(LocalId(0))).with_inferred_type(Type::IntI64),
                    ],
                    term: Terminator::Return(Value(0)),
                }],
            }],
            main: FunctionId(0),
            names: vec![],
        };

        let clif = compiled_optimized_clif(&ir, 0);

        let payload_offset = pylong_value_offset_i32(jit_module().target_config().pointer_type().bytes() as usize)
            .expect("PyLong payload offset fits CLIF offset");
        assert!(clif.contains(&format!("+{payload_offset}")));
        assert!(clif.contains("brif"));
    }
}