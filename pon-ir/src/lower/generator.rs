use super::*;
use crate::ir::{FStrPart, TStrPart};

/// Lowers a `yield` expression to the transform-input [`InstKind::Yield`]
/// marker.  The post-lowering state-machine transform replaces it with a
/// [`Terminator::Suspend`] split (pin J0.1 §7).
pub(super) fn lower_yield_expr(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    expr: &ruff_python_ast::ExprYield,
) -> Result<Value, LowerError> {
    let val = match expr.value.as_deref() {
        Some(value) => driver.lower_expr(scope, value)?,
        None => scope.emit(InstKind::Const(PyConst::None))?,
    };
    scope.emit(InstKind::Yield { val })
}

/// Lowers `yield from EXPR` to iterator acquisition plus the generator
/// delegation marker.  The state-machine transform expands the marker into a
/// resumable delegation loop with the delegate spilled to a frame slot.
pub(super) fn lower_yield_from_expr(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    expr: &ruff_python_ast::ExprYieldFrom,
) -> Result<Value, LowerError> {
    let iterable = driver.lower_expr(scope, &expr.value)?;
    let iter = scope.emit(InstKind::GetIter { iterable })?;
    scope.emit(InstKind::YieldFrom { iter })
}

/// Lowers `await EXPR` to `__await__` normalization followed by the same
/// delegation machinery used for `yield from`.
pub(super) fn lower_await_expr(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    expr: &ruff_python_ast::ExprAwait,
) -> Result<Value, LowerError> {
    let awaitable = driver.lower_expr(scope, &expr.value)?;
    let iter = scope.emit(InstKind::Await { awaitable })?;
    scope.emit(InstKind::YieldFrom { iter })
}

/// One enclosing static handler record active at a suspend point.
#[derive(Clone, Copy)]
struct HandlerSpec {
    target: BlockId,
    stack_depth: u32,
    kind: u8,
}

/// Fresh-id allocation state threaded through the transform.
struct GenIds {
    next_value: u32,
    next_block: u32,
    next_local: u32,
}

impl GenIds {
    fn value(&mut self) -> Result<Value, LowerError> {
        let id = Value(self.next_value);
        self.next_value = self
            .next_value
            .checked_add(1)
            .ok_or_else(|| LowerError::internal("too many SSA values for u32 ids"))?;
        Ok(id)
    }

    fn block(&mut self) -> Result<BlockId, LowerError> {
        let id = BlockId(self.next_block);
        self.next_block = self
            .next_block
            .checked_add(1)
            .ok_or_else(|| LowerError::internal("too many basic blocks for u32 ids"))?;
        Ok(id)
    }

    fn local(&mut self) -> Result<LocalId, LowerError> {
        let id = LocalId(self.next_local);
        self.next_local = self
            .next_local
            .checked_add(1)
            .ok_or_else(|| LowerError::internal("too many local slots for u32 ids"))?;
        Ok(id)
    }
}

/// Rewrites a lowered generator/coroutine body into a resumable state machine
/// (pin J0.1 §7).
///
/// - Every [`InstKind::Yield`] becomes a block split with a
///   [`Terminator::Suspend`] carrying a dense state number and a resume block
///   that re-pushes the statically enclosing handler records and consumes the
///   frame payload ([`InstKind::GenResumePayload`]).
/// - Every [`InstKind::YieldFrom`] becomes a delegation loop around
///   [`InstKind::GenDelegateStep`] owning exactly one state number.
/// - A [`InstKind::GenResumePayload`] is prepended to the entry block so a
///   `throw()` into a never-started generator raises before user code.
/// - Every SSA value used outside its defining block is spilled to a fresh
///   temp local (store after def, load before use), so codegen's whole-frame
///   spill/reload at suspend points covers all live state.
pub(super) fn transform_generator_function(function: &mut Function) -> Result<(), LowerError> {
    let mut ids = GenIds {
        next_value: max_value_id(function)?,
        next_block: max_block_id(function)?,
        next_local: u32::try_from(function.n_locals)
            .map_err(|_| LowerError::internal("local count exceeds u32"))?,
    };

    split_suspend_points(function, &mut ids)?;
    prepend_entry_payload(function, &mut ids)?;
    localize_cross_block_values(function, &mut ids)?;

    function.n_locals = ids.next_local as usize;
    debug_assert_suspend_numbering(function);
    Ok(())
}

fn max_value_id(function: &Function) -> Result<u32, LowerError> {
    let mut max = 0u32;
    for block in &function.blocks {
        for inst in &block.insts {
            max = max.max(inst.result.0.checked_add(1).ok_or_else(|| {
                LowerError::internal("too many SSA values for u32 ids")
            })?);
        }
    }
    Ok(max)
}

fn max_block_id(function: &Function) -> Result<u32, LowerError> {
    let mut max = 0u32;
    for block in &function.blocks {
        max = max.max(block.id.0.checked_add(1).ok_or_else(|| {
            LowerError::internal("too many basic blocks for u32 ids")
        })?);
    }
    Ok(max)
}

/// Splits every `Yield`/`YieldFrom` into suspend-point blocks with dense
/// `1..=N` state numbering in visit order.
fn split_suspend_points(function: &mut Function, ids: &mut GenIds) -> Result<(), LowerError> {
    let mut result: Vec<Block> = Vec::with_capacity(function.blocks.len());
    let mut pending: std::collections::VecDeque<Block> = function.blocks.drain(..).collect();
    let mut handler_stack: Vec<HandlerSpec> = Vec::new();
    let mut next_state: u32 = 1;

    while let Some(block) = pending.pop_front() {
        let mut split_at = None;
        for (index, inst) in block.insts.iter().enumerate() {
            match &inst.kind {
                InstKind::PushExcInfo {
                    target,
                    stack_depth,
                    kind,
                } => handler_stack.push(HandlerSpec {
                    target: *target,
                    stack_depth: *stack_depth,
                    kind: *kind,
                }),
                InstKind::PopExcInfo => {
                    let _ = handler_stack.pop();
                }
                InstKind::Yield { .. } | InstKind::YieldFrom { .. } => {
                    split_at = Some(index);
                    break;
                }
                _ => {}
            }
        }

        let Some(index) = split_at else {
            result.push(block);
            continue;
        };

        let state = next_state;
        next_state = next_state
            .checked_add(1)
            .ok_or_else(|| LowerError::internal("too many generator suspend points"))?;

        let Block { id, mut insts, term } = block;
        let tail_insts = insts.split_off(index + 1);
        let suspend_inst = insts.pop().expect("split index points at an instruction");
        let mut head_insts = insts;

        match suspend_inst.kind {
            InstKind::Yield { val } => {
                // head: pops, Suspend(state, val) -> resume
                // resume: re-pushes, GenResumePayload(result), tail...
                for _ in &handler_stack {
                    head_insts.push(Inst::new(ids.value()?, InstKind::PopExcInfo));
                }
                let resume_blk = ids.block()?;
                result.push(Block {
                    id,
                    insts: head_insts,
                    term: Terminator::Suspend {
                        state,
                        val,
                        resume: resume_blk,
                    },
                });

                let mut resume_insts = Vec::with_capacity(handler_stack.len() + 1 + tail_insts.len());
                for spec in &handler_stack {
                    resume_insts.push(Inst::new(
                        ids.value()?,
                        InstKind::PushExcInfo {
                            target: spec.target,
                            stack_depth: spec.stack_depth,
                            kind: spec.kind,
                        },
                    ));
                }
                resume_insts.push(Inst::new(suspend_inst.result, InstKind::GenResumePayload));
                resume_insts.extend(tail_insts);
                // The scanner sees head's pops then resume's re-pushes: net
                // zero.  Keep `handler_stack` unchanged so the tail (and later
                // blocks) still observe the enclosing handlers.
                pending.push_front(Block {
                    id: resume_blk,
                    insts: resume_insts,
                    term,
                });
            }
            InstKind::YieldFrom { iter } => {
                // head:   StoreLocal(delegate_slot, iter); Jump loop
                // loop:   d = LoadLocal(slot); step = GenDelegateStep(d);
                //         ForLoop(step) -> yield_blk / done_blk
                // yield:  pops; Suspend(state, step) -> resume
                // resume: re-pushes; Jump loop
                // done:   result = GenLastStopValue; tail...
                let delegate_slot = ids.local()?;
                let loop_blk = ids.block()?;
                let yield_blk = ids.block()?;
                let resume_blk = ids.block()?;
                let done_blk = ids.block()?;

                head_insts.push(Inst::new(
                    ids.value()?,
                    InstKind::StoreLocal(delegate_slot, iter),
                ));
                result.push(Block {
                    id,
                    insts: head_insts,
                    term: Terminator::Jump(loop_blk),
                });

                let delegate = ids.value()?;
                let step = ids.value()?;
                let loop_block = Block {
                    id: loop_blk,
                    insts: vec![
                        Inst::new(delegate, InstKind::LoadLocal(delegate_slot)),
                        Inst::new(step, InstKind::GenDelegateStep { delegate }),
                    ],
                    term: Terminator::ForLoop {
                        iter: delegate,
                        body: yield_blk,
                        done: done_blk,
                    },
                };

                let mut yield_insts = Vec::with_capacity(handler_stack.len());
                for _ in &handler_stack {
                    yield_insts.push(Inst::new(ids.value()?, InstKind::PopExcInfo));
                }
                let yield_block = Block {
                    id: yield_blk,
                    insts: yield_insts,
                    term: Terminator::Suspend {
                        state,
                        val: step,
                        resume: resume_blk,
                    },
                };

                let mut resume_insts = Vec::with_capacity(handler_stack.len());
                for spec in &handler_stack {
                    resume_insts.push(Inst::new(
                        ids.value()?,
                        InstKind::PushExcInfo {
                            target: spec.target,
                            stack_depth: spec.stack_depth,
                            kind: spec.kind,
                        },
                    ));
                }
                let resume_block = Block {
                    id: resume_blk,
                    insts: resume_insts,
                    term: Terminator::Jump(loop_blk),
                };

                let mut done_insts = vec![Inst::new(suspend_inst.result, InstKind::GenLastStopValue)];
                done_insts.extend(tail_insts);
                let done_block = Block {
                    id: done_blk,
                    insts: done_insts,
                    term,
                };

                // Scan order must mirror codegen's linear walk: loop and done
                // run under the enclosing handlers; yield's pops and resume's
                // re-pushes cancel between them.
                pending.push_front(done_block);
                pending.push_front(resume_block);
                pending.push_front(yield_block);
                pending.push_front(loop_block);
            }
            _ => unreachable!("split index points at a yield-family instruction"),
        }
    }

    function.blocks = result;
    Ok(())
}

/// Prepends the entry-state payload consume so `throw()` into a just-started
/// generator raises before any user code (pin J0.1 §4.2).
fn prepend_entry_payload(function: &mut Function, ids: &mut GenIds) -> Result<(), LowerError> {
    let entry = function
        .blocks
        .first_mut()
        .ok_or_else(|| LowerError::internal("generator body has no entry block"))?;
    let result = ids.value()?;
    entry.insts.insert(0, Inst::new(result, InstKind::GenResumePayload));
    Ok(())
}

/// Spills every SSA value used outside its defining block through a temp
/// local.  Codegen reloads all locals in the body prologue and spills them all
/// at each suspend, so localized values survive suspension and satisfy CLIF
/// dominance for resume blocks entered from the dispatch table.
fn localize_cross_block_values(function: &mut Function, ids: &mut GenIds) -> Result<(), LowerError> {
    use std::collections::HashMap;

    let mut def_block: HashMap<Value, usize> = HashMap::new();
    for (block_index, block) in function.blocks.iter().enumerate() {
        for inst in &block.insts {
            def_block.insert(inst.result, block_index);
        }
    }

    let mut crossing: HashMap<Value, LocalId> = HashMap::new();
    for (block_index, block) in function.blocks.iter().enumerate() {
        let mut note = |value: Value| {
            if let Some(def) = def_block.get(&value) {
                if *def != block_index && !crossing.contains_key(&value) {
                    crossing.insert(value, LocalId(0));
                }
            }
        };
        for inst in &block.insts {
            for_each_operand(&inst.kind, &mut note);
        }
        for_each_term_operand(&block.term, &mut note);
    }
    if crossing.is_empty() {
        return Ok(());
    }
    for slot in crossing.values_mut() {
        *slot = ids.local()?;
    }

    for (block_index, block) in function.blocks.iter_mut().enumerate() {
        // Loads for values defined elsewhere and used here.
        let mut needed: Vec<Value> = Vec::new();
        let mut note_use = |value: Value| {
            if let Some(def) = def_block.get(&value) {
                if *def != block_index && !needed.contains(&value) {
                    needed.push(value);
                }
            }
        };
        for inst in &block.insts {
            for_each_operand(&inst.kind, &mut note_use);
        }
        for_each_term_operand(&block.term, &mut note_use);

        let mut remap: HashMap<Value, Value> = HashMap::new();
        let mut prologue: Vec<Inst> = Vec::with_capacity(needed.len());
        for value in needed {
            let slot = crossing[&value];
            let loaded = ids.value()?;
            prologue.push(Inst::new(loaded, InstKind::LoadLocal(slot)));
            remap.insert(value, loaded);
        }

        // Rewrite uses (defs in this block keep their original ids).
        let local_defs: std::collections::HashSet<Value> =
            block.insts.iter().map(|inst| inst.result).collect();
        let mut rewrite = |value: &mut Value| {
            if local_defs.contains(value) {
                return;
            }
            if let Some(new) = remap.get(value) {
                *value = *new;
            }
        };
        for inst in &mut block.insts {
            rewrite_operands(&mut inst.kind, &mut rewrite);
        }
        rewrite_term_operands(&mut block.term, &mut rewrite);

        if !prologue.is_empty() {
            prologue.extend(std::mem::take(&mut block.insts));
            block.insts = prologue;
        }

        // Stores after each crossing def.
        let mut index = 0;
        while index < block.insts.len() {
            let result = block.insts[index].result;
            if let Some(slot) = crossing.get(&result) {
                let store = Inst::new(ids.value()?, InstKind::StoreLocal(*slot, result));
                block.insts.insert(index + 1, store);
                index += 1;
            }
            index += 1;
        }
    }
    Ok(())
}

fn debug_assert_suspend_numbering(function: &Function) {
    if cfg!(debug_assertions) {
        let mut states: Vec<u32> = function
            .blocks
            .iter()
            .filter_map(|block| match block.term {
                Terminator::Suspend { state, .. } => Some(state),
                _ => None,
            })
            .collect();
        states.sort_unstable();
        for (index, state) in states.iter().enumerate() {
            assert_eq!(
                *state,
                index as u32 + 1,
                "generator suspend states must be dense 1..=N"
            );
        }
    }
}

/// Visits every `ValueId` operand of an instruction.
fn for_each_operand(kind: &InstKind, f: &mut impl FnMut(Value)) {
    let mut kind = kind.clone();
    rewrite_operands(&mut kind, &mut |value: &mut Value| f(*value));
}

fn for_each_term_operand(term: &Terminator, f: &mut impl FnMut(Value)) {
    let mut term = term.clone();
    rewrite_term_operands(&mut term, &mut |value: &mut Value| f(*value));
}

/// Applies `f` to every `ValueId` operand slot of an instruction, in place.
#[allow(clippy::too_many_lines, reason = "exhaustive over InstKind by design")]
fn rewrite_operands(kind: &mut InstKind, f: &mut impl FnMut(&mut Value)) {
    match kind {
        InstKind::Const(_)
        | InstKind::ConstRef(_)
        | InstKind::LoadLocal(_)
        | InstKind::DeleteLocal(_)
        | InstKind::LoadGlobal(_)
        | InstKind::DeleteGlobal(_)
        | InstKind::LoadName(_)
        | InstKind::DeleteName(_)
        | InstKind::LoadCell(_)
        | InstKind::DeleteCell(_)
        | InstKind::MakeCell(_)
        | InstKind::LoadClosure(_)
        | InstKind::LoadBuiltin(_)
        | InstKind::Reraise
        | InstKind::PushExcInfo { .. }
        | InstKind::PopExcInfo
        | InstKind::GetCurrentExc
        | InstKind::GenResumePayload
        | InstKind::GenLastStopValue
        | InstKind::ImportName { .. }
        | InstKind::MakeFunction { .. }
        | InstKind::MakeTypeVar { .. }
        | InstKind::SetupAnnotations
        | InstKind::LoadBuildClass => {}
        InstKind::BuildTuple { elts } | InstKind::BuildList { elts } | InstKind::BuildSet { elts } => {
            for elt in elts {
                f(elt);
            }
        }
        InstKind::BuildMap { pairs } => {
            for (key, val) in pairs {
                f(key);
                f(val);
            }
        }
        InstKind::BuildSlice { lower, upper, step } => {
            f(lower);
            f(upper);
            f(step);
        }
        InstKind::BuildString { parts } => {
            for part in parts {
                if let FStrPart::Interp { value, format_spec, .. } = part {
                    f(value);
                    if let Some(spec) = format_spec {
                        f(spec);
                    }
                }
            }
        }
        InstKind::BuildTemplate { parts } => {
            for part in parts {
                if let TStrPart::Interp { value, format_spec, .. } = part {
                    f(value);
                    if let Some(spec) = format_spec {
                        f(spec);
                    }
                }
            }
        }
        InstKind::ListAppend { list, item } | InstKind::SetAdd { set: list, item } => {
            f(list);
            f(item);
        }
        InstKind::MapInsert { map, key, val } => {
            f(map);
            f(key);
            f(val);
        }
        InstKind::ListExtend { list, iter } => {
            f(list);
            f(iter);
        }
        InstKind::DictMerge { map, other } | InstKind::DictMergeUnique { map, other } => {
            f(map);
            f(other);
        }
        InstKind::StoreLocal(_, value)
        | InstKind::StoreGlobal(_, value)
        | InstKind::StoreName(_, value)
        | InstKind::StoreCell(_, value) => f(value),
        InstKind::BinaryOp { lhs, rhs, .. }
        | InstKind::InplaceOp { lhs, rhs, .. }
        | InstKind::Compare { lhs, rhs, .. }
        | InstKind::Is { lhs, rhs, .. } => {
            f(lhs);
            f(rhs);
        }
        InstKind::Contains { item, container, .. } => {
            f(item);
            f(container);
        }
        InstKind::UnaryOp { operand, .. } => f(operand),
        InstKind::BoolTest { val } | InstKind::Not { val } => f(val),
        InstKind::LoadAttr { obj, .. }
        | InstKind::DeleteAttr { obj, .. }
        | InstKind::LoadMethod { obj, .. } => f(obj),
        InstKind::StoreAttr { obj, val, .. } => {
            f(obj);
            f(val);
        }
        InstKind::SubscriptGet { obj, index } | InstKind::SubscriptDel { obj, index } => {
            f(obj);
            f(index);
        }
        InstKind::SubscriptSet { obj, index, val } => {
            f(obj);
            f(index);
            f(val);
        }
        InstKind::Call { callee, args } | InstKind::CallMethod { recv_pair: callee, args } => {
            f(callee);
            for arg in args {
                f(arg);
            }
        }
        InstKind::CallEx {
            callee,
            args,
            star,
            kwargs,
            dstar,
        } => {
            f(callee);
            for arg in args {
                f(arg);
            }
            if let Some(star) = star {
                f(star);
            }
            for (_, value) in kwargs {
                f(value);
            }
            if let Some(dstar) = dstar {
                f(dstar);
            }
        }
        InstKind::GetIter { iterable } | InstKind::GetAIter { iterable } => f(iterable),
        InstKind::ForNext { iter } => f(iter),
        InstKind::UnpackSeq { val, .. } | InstKind::UnpackEx { val, .. } => f(val),
        InstKind::Yield { val } => f(val),
        InstKind::YieldFrom { iter } => f(iter),
        InstKind::Await { awaitable } => f(awaitable),
        InstKind::GenDelegateStep { delegate } => f(delegate),
        InstKind::Raise { exc, cause } => {
            if let Some(exc) = exc {
                f(exc);
            }
            if let Some(cause) = cause {
                f(cause);
            }
        }
        InstKind::MatchExc { exc_type } => f(exc_type),
        InstKind::CheckExcStar { exc_types } | InstKind::ExcStarMatch { exc_types } => f(exc_types),
        InstKind::ExcStarEnter | InstKind::ExcStarBodyOk | InstKind::ExcStarBodyRaised |
        InstKind::ExcStarFinish => {}
        InstKind::BuildExcGroup { excs } => {
            for exc in excs {
                f(exc);
            }
        }
        InstKind::MatchSequence { subj }
        | InstKind::MatchMapping { subj }
        | InstKind::GetLen { subj }
        | InstKind::MatchLenGe { subj, .. } => f(subj),
        InstKind::MatchClass { subj, cls, .. } => {
            f(subj);
            f(cls);
        }
        InstKind::MatchKeys { subj, keys } => {
            f(subj);
            for key in keys {
                f(key);
            }
        }
        InstKind::ImportFrom { module, .. } | InstKind::ImportStar { module } => f(module),
        InstKind::BuildClass {
            bases,
            keywords,
            decorators,
            ..
        } => {
            for base in bases {
                f(base);
            }
            for (_, value) in keywords {
                f(value);
            }
            for decorator in decorators {
                f(decorator);
            }
        }
        InstKind::MakeFunctionFull {
            defaults,
            kwdefaults,
            annotations,
            ..
        } => {
            for default in defaults {
                f(default);
            }
            for (_, value) in kwdefaults {
                f(value);
            }
            for (_, value) in annotations {
                f(value);
            }
        }
        InstKind::FunctionSetAnnotate { function, annotate } => {
            f(function);
            f(annotate);
        }
        InstKind::MakeTypeAlias { thunk, .. } => f(thunk),
    }
}

/// Applies `f` to every `ValueId` operand slot of a terminator, in place.
fn rewrite_term_operands(term: &mut Terminator, f: &mut impl FnMut(&mut Value)) {
    match term {
        Terminator::Return(value) => f(value),
        Terminator::Branch { cond, .. } | Terminator::CondBranch { cond, .. } => f(cond),
        Terminator::ForLoop { iter, .. } => f(iter),
        Terminator::Suspend { val, .. } => f(val),
        Terminator::Jump(_) | Terminator::RaiseTerm | Terminator::Unreachable => {}
    }
}
