use super::*;
use crate::ir::{BinOp, CmpOp, UnOp};

pub(super) fn lower_return(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    ret: &ruff_python_ast::StmtReturn,
) -> Result<(), LowerError> {
    if scope.is_module() {
        return unsupported_at(
            "top-level return statement",
            span_bounds(ret.range.start().to_u32(), ret.range.end().to_u32()),
        );
    }
    let value = match ret.value.as_deref() {
        Some(expr) => driver.lower_expr(scope, expr)?,
        None => scope.emit(InstKind::Const(PyConst::None))?,
    };
    scope.set_term(Terminator::Return(value))
}

pub(super) fn lower_unary_expr_with_driver(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    unary: &ruff_python_ast::ExprUnaryOp,
) -> Result<Value, LowerError> {
    let operand = driver.lower_expr(scope, &unary.operand)?;
    match unary.op {
        ruff_python_ast::UnaryOp::Not => scope.emit(InstKind::Not { val: operand }),
        ruff_python_ast::UnaryOp::USub => scope.emit(InstKind::UnaryOp {
            op: UnOp::Neg,
            operand,
        }),
        ruff_python_ast::UnaryOp::UAdd => scope.emit(InstKind::UnaryOp {
            op: UnOp::Pos,
            operand,
        }),
        ruff_python_ast::UnaryOp::Invert => scope.emit(InstKind::UnaryOp {
            op: UnOp::Invert,
            operand,
        }),
    }
}

pub(super) fn lower_named_expr_with_driver(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    named: &ruff_python_ast::ExprNamed,
) -> Result<Value, LowerError> {
    let value = driver.lower_expr(scope, &named.value)?;
    driver.lower_store_target(scope, &named.target, value)?;
    Ok(value)
}

pub(super) fn lower_compare_expr_with_driver(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    compare: &ruff_python_ast::ExprCompare,
) -> Result<Value, LowerError> {
    if compare.ops.len() != compare.comparators.len() {
        return Err(LowerError::internal("comparison op/comparator arity mismatch"));
    }

    let mut lhs = driver.lower_expr(scope, &compare.left)?;
    let mut folded: Option<Value> = None;
    for (op, rhs_expr) in compare.ops.iter().copied().zip(compare.comparators.iter()) {
        let rhs = driver.lower_expr(scope, rhs_expr)?;
        let comparison = lower_single_compare(scope, op, lhs, rhs)?;
        folded = Some(match folded {
            None => comparison,
            Some(previous) => {
                let previous_truth = scope.emit(InstKind::BoolTest { val: previous })?;
                let comparison_truth = scope.emit(InstKind::BoolTest { val: comparison })?;
                let combined = scope.emit(InstKind::BinaryOp {
                    op: BinOp::And,
                    lhs: previous_truth,
                    rhs: comparison_truth,
                })?;
                scope.emit(InstKind::BoolTest { val: combined })?
            }
        });
        lhs = rhs;
    }

    folded.ok_or_else(|| LowerError::internal("empty comparison expression"))
}

pub(super) fn lower_bool_expr_with_driver(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    bool_op: &ruff_python_ast::ExprBoolOp,
) -> Result<Value, LowerError> {
    let mut values = bool_op.values.iter();
    let Some(first) = values.next() else {
        return Err(LowerError::internal("empty boolean operation"));
    };
    let mut result = driver.lower_expr(scope, first)?;
    for expr in values {
        let rhs = driver.lower_expr(scope, expr)?;
        result = match bool_op.op {
            ruff_python_ast::BoolOp::And => {
                let lhs_truth = scope.emit(InstKind::BoolTest { val: result })?;
                let rhs_truth = scope.emit(InstKind::BoolTest { val: rhs })?;
                let combined = scope.emit(InstKind::BinaryOp {
                    op: BinOp::And,
                    lhs: lhs_truth,
                    rhs: rhs_truth,
                })?;
                scope.emit(InstKind::BoolTest { val: combined })?
            }
            ruff_python_ast::BoolOp::Or => {
                let lhs_truth = scope.emit(InstKind::BoolTest { val: result })?;
                let rhs_truth = scope.emit(InstKind::BoolTest { val: rhs })?;
                let combined = scope.emit(InstKind::BinaryOp {
                    op: BinOp::Or,
                    lhs: lhs_truth,
                    rhs: rhs_truth,
                })?;
                scope.emit(InstKind::BoolTest { val: combined })?
            }
        };
    }
    Ok(result)
}

pub(super) fn lower_if_expr_with_driver(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    expr: &ruff_python_ast::ExprIf,
) -> Result<Value, LowerError> {
    let result_slot = scope.alloc_temp_local();
    let then_block = scope.alloc_block()?;
    let else_block = scope.alloc_block()?;
    let done_block = scope.alloc_block()?;
    let test = driver.lower_expr(scope, &expr.test)?;
    let cond = scope.emit(InstKind::BoolTest { val: test })?;
    scope.set_term(Terminator::CondBranch {
        cond,
        then_: then_block,
        else_: else_block,
    })?;

    scope.switch_to(then_block)?;
    let body = driver.lower_expr(scope, &expr.body)?;
    scope.emit(InstKind::StoreLocal(result_slot, body))?;
    scope.jump_if_open(done_block)?;

    scope.switch_to(else_block)?;
    let orelse = driver.lower_expr(scope, &expr.orelse)?;
    scope.emit(InstKind::StoreLocal(result_slot, orelse))?;
    scope.jump_if_open(done_block)?;

    scope.switch_to(done_block)?;
    scope.emit(InstKind::LoadLocal(result_slot))
}

fn lower_single_compare(
    scope: &mut BodyScope,
    op: ruff_python_ast::CmpOp,
    lhs: Value,
    rhs: Value,
) -> Result<Value, LowerError> {
    match op {
        ruff_python_ast::CmpOp::Eq => scope.emit(InstKind::Compare {
            op: CmpOp::Eq,
            lhs,
            rhs,
        }),
        ruff_python_ast::CmpOp::NotEq => scope.emit(InstKind::Compare {
            op: CmpOp::Ne,
            lhs,
            rhs,
        }),
        ruff_python_ast::CmpOp::Lt => scope.emit(InstKind::Compare {
            op: CmpOp::Lt,
            lhs,
            rhs,
        }),
        ruff_python_ast::CmpOp::LtE => scope.emit(InstKind::Compare {
            op: CmpOp::Le,
            lhs,
            rhs,
        }),
        ruff_python_ast::CmpOp::Gt => scope.emit(InstKind::Compare {
            op: CmpOp::Gt,
            lhs,
            rhs,
        }),
        ruff_python_ast::CmpOp::GtE => scope.emit(InstKind::Compare {
            op: CmpOp::Ge,
            lhs,
            rhs,
        }),
        ruff_python_ast::CmpOp::Is => scope.emit(InstKind::Is {
            lhs,
            rhs,
            negate: false,
        }),
        ruff_python_ast::CmpOp::IsNot => scope.emit(InstKind::Is {
            lhs,
            rhs,
            negate: true,
        }),
        ruff_python_ast::CmpOp::In => scope.emit(InstKind::Contains {
            item: lhs,
            container: rhs,
            negate: false,
        }),
        ruff_python_ast::CmpOp::NotIn => scope.emit(InstKind::Contains {
            item: lhs,
            container: rhs,
            negate: true,
        }),
    }
}
#[derive(Clone, Copy)]
pub(super) struct LoopTargets {
    pub(super) break_block: BlockId,
    pub(super) continue_block: BlockId,
}

pub(super) fn lower_if_header_with_driver(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtIf,
    then_block: BlockId,
    else_block: BlockId,
) -> Result<(), LowerError> {
    let test = driver.lower_expr(scope, &stmt.test)?;
    let cond = scope.emit(InstKind::BoolTest { val: test })?;
    scope.set_term(Terminator::CondBranch {
        cond,
        then_: then_block,
        else_: else_block,
    })
}

#[allow(dead_code)]
pub(super) fn lower_while_header_with_driver(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtWhile,
    body_block: BlockId,
    done_block: BlockId,
) -> Result<(), LowerError> {
    let test = driver.lower_expr(scope, &stmt.test)?;
    let cond = scope.emit(InstKind::BoolTest { val: test })?;
    scope.set_term(Terminator::CondBranch {
        cond,
        then_: body_block,
        else_: done_block,
    })
}

#[allow(dead_code)]
pub(super) fn lower_for_header_with_driver(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtFor,
    body_block: BlockId,
    done_block: BlockId,
) -> Result<(), LowerError> {
    if stmt.is_async {
        return unsupported_at(
            "async for statement",
            span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()),
        );
    }
    let iterable = driver.lower_expr(scope, &stmt.iter)?;
    let iter = scope.emit(InstKind::GetIter { iterable })?;
    scope.set_term(Terminator::ForLoop {
        iter,
        body: body_block,
        done: done_block,
    })
}

pub(super) fn lower_for_item_store_with_driver(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtFor,
    item: Value,
) -> Result<(), LowerError> {
    driver.lower_store_target(scope, &stmt.target, item)
}

pub(super) fn lower_break_with_targets(
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtBreak,
    targets: Option<LoopTargets>,
) -> Result<(), LowerError> {
    let Some(targets) = targets else {
        return unsupported_at(
            "break outside loop",
            span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()),
        );
    };
    scope.set_term(Terminator::Jump(targets.break_block))
}

pub(super) fn lower_continue_with_targets(
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtContinue,
    targets: Option<LoopTargets>,
) -> Result<(), LowerError> {
    let Some(targets) = targets else {
        return unsupported_at(
            "continue outside loop",
            span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()),
        );
    };
    scope.set_term(Terminator::Jump(targets.continue_block))
}

#[allow(dead_code)]
pub(super) fn lower_for(stmt: &ruff_python_ast::StmtFor) -> Result<(), LowerError> {
    unsupported_at(
        if stmt.is_async { "async for statement" } else { "for statement" },
        span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()),
    )
}

#[allow(dead_code)]
pub(super) fn lower_while(stmt: &ruff_python_ast::StmtWhile) -> Result<(), LowerError> {
    unsupported_at(
        "while statement",
        span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()),
    )
}

#[allow(dead_code)]
pub(super) fn lower_if(stmt: &ruff_python_ast::StmtIf) -> Result<(), LowerError> {
    unsupported_at(
        "if statement",
        span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()),
    )
}

#[allow(dead_code)]
pub(super) fn lower_break(stmt: &ruff_python_ast::StmtBreak) -> Result<(), LowerError> {
    unsupported_at(
        "break statement",
        span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()),
    )
}

#[allow(dead_code)]
pub(super) fn lower_continue(stmt: &ruff_python_ast::StmtContinue) -> Result<(), LowerError> {
    unsupported_at(
        "continue statement",
        span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()),
    )
}

pub(super) fn lower_assert(stmt: &ruff_python_ast::StmtAssert) -> Result<(), LowerError> {
    unsupported_at(
        "assert statement",
        span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()),
    )
}

pub(super) fn lower_pass(_stmt: &ruff_python_ast::StmtPass) -> Result<(), LowerError> {
    Ok(())
}
