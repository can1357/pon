use crate::ir::{BinOp, CmpOp};
use ruff_python_ast::{Expr, Pattern, Stmt};

use super::*;

pub(super) fn lower_match(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtMatch,
    loop_targets: Option<control::LoopTargets>,
) -> Result<(), LowerError> {
    if !stmt
        .cases
        .iter()
        .all(|case| case_is_layer1_representative(&case.pattern, case.guard.as_deref(), &case.body))
    {
        return unsupported_at(
            "match pattern outside WS-MATCH Layer 1 representative set",
            span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()),
        );
    }

    let subject = driver.lower_expr(scope, &stmt.subject)?;
    let done_block = scope.alloc_block()?;
    for case in &stmt.cases {
        let case_block = scope.alloc_block()?;
        let next_block = scope.alloc_block()?;
        let mut cond = lower_pattern_test(driver, scope, subject, &case.pattern)?;
        if let Some(guard) = case.guard.as_deref() {
            let pattern_truth = scope.emit(InstKind::BoolTest { val: cond })?;
            let guard = driver.lower_expr(scope, guard)?;
            let guard_truth = scope.emit(InstKind::BoolTest { val: guard })?;
            cond = scope.emit(InstKind::BinaryOp {
                op: BinOp::And,
                lhs: pattern_truth,
                rhs: guard_truth,
            })?;
        }
        let cond = scope.emit(InstKind::BoolTest { val: cond })?;
        scope.set_term(Terminator::CondBranch {
            cond,
            then_: case_block,
            else_: next_block,
        })?;
        scope.switch_to(case_block)?;
        driver.lower_stmt_list(scope, &case.body, loop_targets)?;
        scope.jump_if_open(done_block)?;
        scope.switch_to(next_block)?;
    }
    scope.jump_if_open(done_block)?;
    scope.switch_to(done_block)?;
    Ok(())
}

fn lower_pattern_test(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    subject: Value,
    pattern: &Pattern,
) -> Result<Value, LowerError> {
    match pattern {
        Pattern::MatchValue(pattern) => {
            let expected = driver.lower_expr(scope, &pattern.value)?;
            scope.emit(InstKind::Compare {
                op: CmpOp::Eq,
                lhs: subject,
                rhs: expected,
            })
        }
        Pattern::MatchAs(pattern) if pattern.pattern.is_none() && pattern.name.is_none() => {
            scope.emit(InstKind::Const(PyConst::Bool(true)))
        }
        _ => Err(LowerError::unsupported(
            "match pattern outside executable Layer 1 subset",
        )),
    }
}


fn case_is_layer1_representative(pattern: &Pattern, guard: Option<&Expr>, body: &[Stmt]) -> bool {
    guard.map_or(true, is_guard_representative)
        && !body.is_empty()
        && body.iter().all(is_representative_body_stmt)
        && pattern_is_representative(pattern)
}

fn is_guard_representative(guard: &Expr) -> bool {
    matches!(guard, Expr::Name(_) | Expr::BooleanLiteral(_) | Expr::Compare(_) | Expr::Call(_))
}

fn is_representative_body_stmt(stmt: &Stmt) -> bool {
    matches!(stmt, Stmt::Assign(_) | Stmt::Expr(_) | Stmt::Return(_) | Stmt::Pass(_))
}

fn pattern_is_representative(pattern: &Pattern) -> bool {
    match pattern {
        Pattern::MatchValue(pattern) => literal_or_name_value(&pattern.value),
        Pattern::MatchSingleton(_) => true,
        Pattern::MatchSequence(pattern) => {
            let mut stars = 0usize;
            for nested in &pattern.patterns {
                if matches!(nested, Pattern::MatchStar(_)) {
                    stars += 1;
                    if stars > 1 {
                        return false;
                    }
                    continue;
                }
                if !pattern_is_representative(nested) {
                    return false;
                }
            }
            true
        }
        Pattern::MatchMapping(pattern) => {
            pattern.keys.len() == pattern.patterns.len()
                && pattern.keys.iter().all(literal_or_name_value)
                && pattern.patterns.iter().all(pattern_is_representative)
        }
        Pattern::MatchClass(pattern) => {
            literal_or_name_value(&pattern.cls)
                && pattern.arguments.patterns.iter().all(pattern_is_representative)
                && pattern
                    .arguments
                    .keywords
                    .iter()
                    .all(|keyword| pattern_is_representative(&keyword.pattern))
        }
        Pattern::MatchStar(_) => true,
        Pattern::MatchAs(pattern) => pattern
            .pattern
            .as_deref()
            .map_or(true, pattern_is_representative),
        Pattern::MatchOr(pattern) => {
            pattern.patterns.len() >= 2 && pattern.patterns.iter().all(pattern_is_representative)
        }
    }
}

fn literal_or_name_value(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Name(_)
            | Expr::StringLiteral(_)
            | Expr::NumberLiteral(_)
            | Expr::NoneLiteral(_)
            | Expr::BooleanLiteral(_)
            | Expr::Attribute(_)
    )
}
