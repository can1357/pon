//! Typed-region discovery for the future Phase-D optimizing tier.
//!
//! The finder is intentionally conservative: it only admits blocks whose
//! instructions already carry unboxable type metadata (or literal constants whose
//! unboxed representation is obvious) and whose CFG can be entered only through
//! the selected entry block.  That gives later tier-up/AoT work a stable region
//! contract without changing today's baseline codegen path.

use std::collections::{HashSet, VecDeque};

use pon_ir::Type;
use pon_ir::ir::{BinOp, BlockId, CmpOp, FStrPart, Function, Inst, InstKind, PyConst, TStrPart, Terminator, UnOp, Value as IrValue};

/// A maximal single-entry typed region inside one IR function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypedRegion {
    /// The only block external control flow may enter directly.
    pub entry: BlockId,
    /// Region blocks in the function's layout order.
    pub blocks: Vec<BlockId>,
    /// Unboxed SSA values produced by region instructions.
    pub values: Vec<TypedValue>,
    /// Typed values consumed by the region but produced before it.
    pub live_ins: Vec<TypedInput>,
    /// Control-flow edges leaving the region.
    pub exits: Vec<RegionExit>,
}

/// One unboxed SSA value produced inside a typed region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypedValue {
    pub value: IrValue,
    pub ty: Type,
    pub block: BlockId,
    pub inst_index: usize,
}

/// One typed boxed value that must be guarded/unboxed on region entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypedInput {
    pub value: IrValue,
    pub ty: Type,
}

/// A CFG edge from a region block to a non-region block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionExit {
    pub from: BlockId,
    pub to: BlockId,
    pub kind: RegionExitKind,
}

/// Why control leaves a typed region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionExitKind {
    Jump,
    Branch,
    ForLoopBody,
    ForLoopDone,
    SuspendResume,
}

/// Find the largest single-entry typed region in `function`.
///
/// Ties are resolved by the function's existing block layout, which keeps region
/// selection deterministic and makes the entry block stable for later feedback
/// plumbing.
#[must_use]
pub fn find_maximal_typed_region(function: &Function) -> Option<TypedRegion> {
    let compatible: Vec<bool> = function.blocks.iter().map(|block| is_typed_block(block)).collect();
    let predecessors = predecessors(function);
    let mut best = None;

    for (entry_index, block) in function.blocks.iter().enumerate() {
        if !compatible[entry_index] {
            continue;
        }
        let Some(candidate) = build_region(function, &compatible, &predecessors, block.id) else {
            continue;
        };
        if is_better_region(&candidate, best.as_ref()) {
            best = Some(candidate);
        }
    }

    best
}

/// Return the unboxable type selected for an instruction result, if any.
#[must_use]
pub fn inst_unboxed_type(inst: &Inst) -> Option<Type> {
    if let Some(ty) = fast_path_result_type(&inst.kind) {
        return Some(ty);
    }
    if inst.inferred_type.is_unboxable() {
        return Some(inst.inferred_type);
    }
    if inst.static_type.is_unboxable() {
        return Some(inst.static_type);
    }
    literal_unboxed_type(&inst.kind)
}

/// Return true when `kind` is in the Phase-D skeleton's unboxed fast-path subset.
#[must_use]
pub fn is_fast_path_kind(kind: &InstKind) -> bool {
    matches!(
        kind,
        InstKind::Const(PyConst::Int(_) | PyConst::Float(_))
            | InstKind::LoadLocal(_)
            | InstKind::StoreLocal(_, _)
            | InstKind::ForNext { .. }
            | InstKind::BinaryOp {
                op: BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::FloorDiv | BinOp::Mod,
                ..
            }
            | InstKind::Compare {
                op: CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge | CmpOp::Eq | CmpOp::Ne,
                ..
            }
            | InstKind::BoolTest { .. }
            | InstKind::Not { .. }
            | InstKind::UnaryOp {
                op: UnOp::Neg | UnOp::Pos,
                ..
            }
    )
}

/// Return all SSA operands read by an instruction kind.
#[must_use]
pub fn inst_operands(kind: &InstKind) -> Vec<IrValue> {
    let mut operands = Vec::new();
    push_inst_operands(kind, &mut operands);
    operands
}

/// Return all SSA operands read by a terminator.
#[must_use]
pub fn terminator_operands(term: &Terminator) -> Vec<IrValue> {
    let mut operands = Vec::new();
    match term {
        Terminator::Return(value)
        | Terminator::Branch { cond: value, .. }
        | Terminator::CondBranch { cond: value, .. }
        | Terminator::ForLoop { iter: value, .. }
        | Terminator::Suspend { val: value, .. } => operands.push(*value),
        Terminator::Jump(_) | Terminator::RaiseTerm | Terminator::Unreachable => {}
        _ => {}
    }
    operands
}

fn build_region(
    function: &Function,
    compatible: &[bool],
    predecessors: &[Vec<BlockId>],
    entry: BlockId,
) -> Option<TypedRegion> {
    let mut reachable = collect_compatible_reachable(function, compatible, entry);
    prune_external_entries(function, predecessors, entry, &mut reachable);

    let blocks: Vec<BlockId> = function
        .blocks
        .iter()
        .filter_map(|block| reachable.contains(&block.id).then_some(block.id))
        .collect();
    if blocks.is_empty() {
        return None;
    }

    let values = collect_typed_values(function, &reachable);
    if values.is_empty() {
        return None;
    }

    Some(TypedRegion {
        entry,
        blocks,
        values,
        live_ins: collect_live_ins(function, &reachable),
        exits: collect_exits(function, &reachable),
    })
}

fn collect_compatible_reachable(function: &Function, compatible: &[bool], entry: BlockId) -> HashSet<BlockId> {
    let mut reachable = HashSet::new();
    let mut queue = VecDeque::new();
    reachable.insert(entry);
    queue.push_back(entry);

    while let Some(block_id) = queue.pop_front() {
        let Some(current_block_index) = block_index(function, block_id) else {
            continue;
        };
        for successor in successors(&function.blocks[current_block_index].term) {
            let Some(successor_index) = block_index(function, successor) else {
                continue;
            };
            if compatible[successor_index] && reachable.insert(successor) {
                queue.push_back(successor);
            }
        }
    }

    reachable
}

fn prune_external_entries(
    function: &Function,
    predecessors: &[Vec<BlockId>],
    entry: BlockId,
    reachable: &mut HashSet<BlockId>,
) {
    loop {
        let mut removed_any = false;
        let current: Vec<BlockId> = reachable.iter().copied().collect();
        for block_id in current {
            if block_id == entry {
                continue;
            }
            let Some(index) = block_index(function, block_id) else {
                continue;
            };
            if predecessors[index].iter().any(|pred| !reachable.contains(pred)) {
                reachable.remove(&block_id);
                removed_any = true;
            }
        }
        if !removed_any {
            break;
        }
    }
}

fn collect_typed_values(function: &Function, region_blocks: &HashSet<BlockId>) -> Vec<TypedValue> {
    let mut values = Vec::new();
    for block in &function.blocks {
        if !region_blocks.contains(&block.id) {
            continue;
        }
        for (inst_index, inst) in block.insts.iter().enumerate() {
            if let Some(ty) = inst_unboxed_type(inst) {
                values.push(TypedValue {
                    value: inst.result,
                    ty,
                    block: block.id,
                    inst_index,
                });
            }
        }
    }
    values
}

fn collect_live_ins(function: &Function, region_blocks: &HashSet<BlockId>) -> Vec<TypedInput> {
    let mut produced = HashSet::new();
    for block in &function.blocks {
        if region_blocks.contains(&block.id) {
            for inst in &block.insts {
                produced.insert(inst.result);
            }
        }
    }

    let mut live_ins = Vec::new();
    let mut seen = HashSet::new();
    for block in &function.blocks {
        if !region_blocks.contains(&block.id) {
            continue;
        }
        for inst in &block.insts {
            for operand in inst_operands(&inst.kind) {
                push_live_in(function, &produced, &mut seen, &mut live_ins, operand);
            }
        }
        for operand in terminator_operands(&block.term) {
            push_live_in(function, &produced, &mut seen, &mut live_ins, operand);
        }
    }
    live_ins
}

fn push_live_in(
    function: &Function,
    produced: &HashSet<IrValue>,
    seen: &mut HashSet<IrValue>,
    live_ins: &mut Vec<TypedInput>,
    operand: IrValue,
) {
    if produced.contains(&operand) || !seen.insert(operand) {
        return;
    }
    let ty = value_unboxed_type(function, operand).unwrap_or(Type::Object);
    live_ins.push(TypedInput { value: operand, ty });
}

fn collect_exits(function: &Function, region_blocks: &HashSet<BlockId>) -> Vec<RegionExit> {
    let mut exits = Vec::new();
    for block in &function.blocks {
        if !region_blocks.contains(&block.id) {
            continue;
        }
        for (target, kind) in successor_edges(&block.term) {
            if !region_blocks.contains(&target) {
                let exit = RegionExit {
                    from: block.id,
                    to: target,
                    kind,
                };
                if !exits.contains(&exit) {
                    exits.push(exit);
                }
            }
        }
    }
    exits
}

fn is_typed_block(block: &pon_ir::ir::Block) -> bool {
    block.insts.iter().all(is_typed_inst) && is_region_terminator(&block.term)
}

fn is_typed_inst(inst: &Inst) -> bool {
    is_fast_path_kind(&inst.kind) && inst_unboxed_type(inst).is_some()
}

fn is_region_terminator(term: &Terminator) -> bool {
    matches!(
        term,
        Terminator::Return(_)
            | Terminator::Jump(_)
            | Terminator::Branch { .. }
            | Terminator::CondBranch { .. }
            | Terminator::ForLoop { .. }
            | Terminator::Unreachable
    )
}

fn is_better_region(candidate: &TypedRegion, best: Option<&TypedRegion>) -> bool {
    let Some(best) = best else {
        return true;
    };
    (candidate.blocks.len(), candidate.values.len()) > (best.blocks.len(), best.values.len())
}

fn value_unboxed_type(function: &Function, value: IrValue) -> Option<Type> {
    function
        .blocks
        .iter()
        .flat_map(|block| block.insts.iter())
        .find_map(|inst| (inst.result == value).then(|| inst_unboxed_type(inst)).flatten())
}

fn fast_path_result_type(kind: &InstKind) -> Option<Type> {
    match kind {
        InstKind::Compare { .. } | InstKind::BoolTest { .. } | InstKind::Not { .. } => Some(Type::IntI64),
        _ => None,
    }
}

fn literal_unboxed_type(kind: &InstKind) -> Option<Type> {
    match kind {
        InstKind::Const(PyConst::Int(_)) => Some(Type::IntI64),
        InstKind::Const(PyConst::Float(_)) => Some(Type::Float),
        _ => None,
    }
}

fn predecessors(function: &Function) -> Vec<Vec<BlockId>> {
    let mut predecessors = vec![Vec::new(); function.blocks.len()];
    for block in &function.blocks {
        for successor in successors(&block.term) {
            if let Some(index) = block_index(function, successor) {
                predecessors[index].push(block.id);
            }
        }
    }
    predecessors
}

fn successors(term: &Terminator) -> Vec<BlockId> {
    successor_edges(term).into_iter().map(|(target, _)| target).collect()
}

fn successor_edges(term: &Terminator) -> Vec<(BlockId, RegionExitKind)> {
    match term {
        Terminator::Jump(target) => vec![(*target, RegionExitKind::Jump)],
        Terminator::Branch { then_blk, else_blk, .. } => vec![
            (*then_blk, RegionExitKind::Branch),
            (*else_blk, RegionExitKind::Branch),
        ],
        Terminator::CondBranch { then_, else_, .. } => vec![
            (*then_, RegionExitKind::Branch),
            (*else_, RegionExitKind::Branch),
        ],
        Terminator::ForLoop { body, done, .. } => vec![
            (*body, RegionExitKind::ForLoopBody),
            (*done, RegionExitKind::ForLoopDone),
        ],
        Terminator::Suspend { resume, .. } => vec![(*resume, RegionExitKind::SuspendResume)],
        Terminator::Return(_) | Terminator::RaiseTerm | Terminator::Unreachable => Vec::new(),
        _ => Vec::new(),
    }
}

fn block_index(function: &Function, block_id: BlockId) -> Option<usize> {
    function.blocks.iter().position(|block| block.id == block_id)
}

fn push_inst_operands(kind: &InstKind, operands: &mut Vec<IrValue>) {
    match kind {
        InstKind::Const(_)
        | InstKind::ConstRef(_)
        | InstKind::LoadLocal(_)
        | InstKind::LoadGlobal(_)
        | InstKind::LoadName(_)
        | InstKind::LoadCell(_)
        | InstKind::LoadClosure(_)
        | InstKind::LoadBuiltin(_)
        | InstKind::MakeCell(_)
        | InstKind::SetupAnnotations
        | InstKind::LoadBuildClass => {}
        InstKind::BuildTuple { elts } | InstKind::BuildList { elts } | InstKind::BuildSet { elts } => {
            operands.extend(elts.iter().copied());
        }
        InstKind::BuildString { parts } => push_fstring_operands(parts, operands),
        InstKind::BuildSlice { lower, upper, step } => {
            operands.push(*lower);
            operands.push(*upper);
            operands.push(*step);
        }
        InstKind::BuildMap { pairs } => {
            for (key, value) in pairs {
                operands.push(*key);
                operands.push(*value);
            }
        }
        InstKind::BuildTemplate { parts } => push_tstring_operands(parts, operands),
        InstKind::ListAppend { list, item }
        | InstKind::SetAdd { set: list, item }
        | InstKind::DictMerge { map: list, other: item }
        | InstKind::DictMergeUnique { map: list, other: item }
        | InstKind::Compare { lhs: list, rhs: item, .. }
        | InstKind::Contains { item: list, container: item, .. }
        | InstKind::Is { lhs: list, rhs: item, .. }
        | InstKind::SubscriptGet { obj: list, index: item }
        | InstKind::MapInsert { map: list, key: item, val: _ } => {
            operands.push(*list);
            operands.push(*item);
            if let InstKind::MapInsert { val, .. } = kind {
                operands.push(*val);
            }
        }
        InstKind::ListExtend { list, iter } | InstKind::SetUpdate { set: list, iter } => {
            operands.push(*list);
            operands.push(*iter);
        }
        InstKind::ListToTuple { list } => operands.push(*list),
        InstKind::StoreLocal(_, value)
        | InstKind::StoreGlobal(_, value)
        | InstKind::StoreName(_, value)
        | InstKind::StoreCell(_, value)
        | InstKind::BoolTest { val: value }
        | InstKind::Not { val: value }
        | InstKind::GetIter { iterable: value }
        | InstKind::GetAIter { iterable: value }
        | InstKind::ForNext { iter: value }
        | InstKind::UnpackSeq { val: value, .. }
        | InstKind::Yield { val: value }
        | InstKind::YieldFrom { iter: value }
        | InstKind::Await { awaitable: value }
        | InstKind::GenDelegateStep { delegate: value }
        | InstKind::MatchExc { exc_type: value }
        | InstKind::CheckExcStar { exc_types: value }
        | InstKind::ExcStarMatch { exc_types: value }
        | InstKind::MatchSequence { subj: value }
        | InstKind::MatchMapping { subj: value }
        | InstKind::GetLen { subj: value }
        | InstKind::ImportFrom { module: value, .. }
        | InstKind::ImportStar { module: value } => operands.push(*value),
        InstKind::BinaryOp { lhs, rhs, .. } | InstKind::InplaceOp { lhs, rhs, .. } => {
            operands.push(*lhs);
            operands.push(*rhs);
        }
        InstKind::UnaryOp { operand, .. } => operands.push(*operand),
        InstKind::StoreAttr { obj, val, .. } => {
            operands.push(*obj);
            operands.push(*val);
        }
        InstKind::LoadAttr { obj, .. } | InstKind::DeleteAttr { obj, .. } | InstKind::LoadMethod { obj, .. } => {
            operands.push(*obj);
        }
        InstKind::SubscriptSet { obj, index, val } => {
            operands.push(*obj);
            operands.push(*index);
            operands.push(*val);
        }
        InstKind::SubscriptDel { obj, index } => {
            operands.push(*obj);
            operands.push(*index);
        }
        InstKind::Call { callee, args } | InstKind::CallMethod { recv_pair: callee, args } => {
            operands.push(*callee);
            operands.extend(args.iter().copied());
        }
        InstKind::CallEx {
            callee,
            args,
            star,
            kwargs,
            dstar,
        } => {
            operands.push(*callee);
            operands.extend(args.iter().copied());
            push_optional_operand(operands, *star);
            operands.extend(kwargs.iter().map(|(_, value)| *value));
            push_optional_operand(operands, *dstar);
        }
        InstKind::UnpackEx { val, .. } => operands.push(*val),
        InstKind::Raise { exc, cause } => {
            push_optional_operand(operands, *exc);
            push_optional_operand(operands, *cause);
        }
        InstKind::Reraise | InstKind::PushExcInfo { .. } | InstKind::PopExcInfo | InstKind::GetCurrentExc => {}
        InstKind::ExcStarEnter | InstKind::ExcStarBodyOk | InstKind::ExcStarBodyRaised | InstKind::ExcStarFinish => {}
        InstKind::BuildExcGroup { excs } | InstKind::MatchKeys { keys: excs, .. } => {
            if let InstKind::MatchKeys { subj, .. } = kind {
                operands.push(*subj);
            }
            operands.extend(excs.iter().copied());
        }
        InstKind::MatchClass { subj, cls, .. } => {
            operands.push(*subj);
            operands.push(*cls);
        }
        InstKind::MatchLenGe { subj, .. } => operands.push(*subj),
        InstKind::ImportName { .. } => {}
        InstKind::BuildClass {
            body: _,
            name: _,
            bases,
            keywords,
            decorators,
        } => {
            operands.extend(bases.iter().copied());
            operands.extend(keywords.iter().map(|(_, value)| *value));
            operands.extend(decorators.iter().copied());
        }
        InstKind::MakeFunction { .. } => {}
        InstKind::MakeFunctionFull {
            defaults,
            kwdefaults,
            closure: _,
            annotations,
            ..
        } => {
            operands.extend(defaults.iter().copied());
            operands.extend(kwdefaults.iter().map(|(_, value)| *value));
            operands.extend(annotations.iter().map(|(_, value)| *value));
        }
        InstKind::DeleteLocal(_)
        | InstKind::DeleteGlobal(_)
        | InstKind::DeleteName(_)
        | InstKind::DeleteCell(_) => {}
        _ => {}
    }
}


fn push_optional_operand(operands: &mut Vec<IrValue>, value: Option<IrValue>) {
    if let Some(value) = value {
        operands.push(value);
    }
}

fn push_fstring_operands(parts: &[FStrPart], operands: &mut Vec<IrValue>) {
    for part in parts {
        if let FStrPart::Interp {
            value,
            format_spec,
            ..
        } = part
        {
            operands.push(*value);
            push_optional_operand(operands, *format_spec);
        }
    }
}

fn push_tstring_operands(parts: &[TStrPart], operands: &mut Vec<IrValue>) {
    for part in parts {
        if let TStrPart::Interp {
            value,
            format_spec,
            ..
        } = part
        {
            operands.push(*value);
            push_optional_operand(operands, *format_spec);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RegionExit, RegionExitKind, find_maximal_typed_region};
    use pon_ir::Type;
    use pon_ir::ir::{BinOp, Block, BlockId, Function, Inst, InstKind, NameId, PyConst, Terminator, Value};

    #[test]
    fn finds_region_across_noncontiguous_successor_block_ids() {
        let function = Function { name: "branchy".to_owned(), arity: 0, is_coroutine: false, is_generator: false, params: Default::default(), n_locals: 0, blocks: vec![
            Block {
                id: BlockId(10),
                insts: vec![Inst::new(Value(0), InstKind::Const(PyConst::Int(1)))],
                term: Terminator::Jump(BlockId(30)),
            },
            Block {
                id: BlockId(30),
                insts: vec![
                    Inst::new(Value(1), InstKind::Const(PyConst::Int(2))),
                    Inst::new(
                        Value(2),
                        InstKind::BinaryOp {
                            op: BinOp::Add,
                            lhs: Value(0),
                            rhs: Value(1),
                        },
                    )
                    .with_inferred_type(Type::IntI64),
                ],
                term: Terminator::Branch {
                    cond: Value(2),
                    then_blk: BlockId(20),
                    else_blk: BlockId(40),
                },
            },
            Block {
                id: BlockId(20),
                insts: vec![
                    Inst::new(
                        Value(3),
                        InstKind::BinaryOp {
                            op: BinOp::Add,
                            lhs: Value(2),
                            rhs: Value(1),
                        },
                    )
                    .with_inferred_type(Type::IntI64),
                ],
                term: Terminator::Return(Value(3)),
            },
            Block {
                id: BlockId(40),
                insts: vec![Inst::new(Value(4), InstKind::LoadGlobal(NameId(0)))],
                term: Terminator::Return(Value(4)),
            },
        ] };

        let region = find_maximal_typed_region(&function).expect("typed region");

        assert_eq!(region.entry, BlockId(10));
        assert_eq!(region.blocks, vec![BlockId(10), BlockId(30), BlockId(20)]);
        assert_eq!(
            region.exits,
            vec![RegionExit {
                from: BlockId(30),
                to: BlockId(40),
                kind: RegionExitKind::Branch,
            }]
        );
        assert_eq!(
            region.values.iter().map(|value| value.block).collect::<Vec<_>>(),
            vec![BlockId(10), BlockId(30), BlockId(30), BlockId(20)]
        );
    }
}
