use super::*;
use crate::ir::BinOp;

pub(super) fn lower_assign(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    assign: &StmtAssign,
) -> Result<(), LowerError> {
    let value = driver.lower_expr(scope, &assign.value)?;
    for target in &assign.targets {
        driver.lower_store_target(scope, target, value)?;
    }
    Ok(())
}

pub(super) fn lower_delete(stmt: &ruff_python_ast::StmtDelete) -> Result<(), LowerError> {
    unsupported_at(
        "delete statement",
        span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()),
    )
}

#[allow(dead_code)]
pub(super) fn lower_aug_assign(stmt: &ruff_python_ast::StmtAugAssign) -> Result<(), LowerError> {
    unsupported_at(
        "augmented assignment",
        span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()),
    )
}

pub(super) fn lower_aug_assign_with_driver(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtAugAssign,
) -> Result<(), LowerError> {
    let op = bin_op_from_operator(stmt.op)?;
    let target = lower_aug_target(driver, scope, &stmt.target)?;
    let rhs = driver.lower_expr(scope, &stmt.value)?;
    let value = scope.emit(InstKind::InplaceOp {
        op,
        lhs: target.current,
        rhs,
    })?;
    target.store(scope, value)
}

struct AugTarget {
    current: Value,
    store: AugStore,
}

impl AugTarget {
    fn store(self, scope: &mut BodyScope, value: Value) -> Result<(), LowerError> {
        match self.store {
            AugStore::Local(slot) => {
                scope.emit(InstKind::StoreLocal(slot, value))?;
            }
            AugStore::Global(name) => {
                scope.emit(InstKind::StoreGlobal(name, value))?;
            }
            AugStore::Name(name) => {
                scope.emit(InstKind::StoreName(name, value))?;
            }
            AugStore::Attr { obj, name } => {
                scope.emit(InstKind::StoreAttr { obj, name, val: value })?;
            }
            AugStore::Subscript { obj, index } => {
                scope.emit(InstKind::SubscriptSet { obj, index, val: value })?;
            }
        }
        Ok(())
    }
}

enum AugStore {
    Local(LocalId),
    Global(NameId),
    Name(NameId),
    Attr { obj: Value, name: NameId },
    Subscript { obj: Value, index: Value },
}

fn lower_aug_target(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    target: &Expr,
) -> Result<AugTarget, LowerError> {
    match target {
        Expr::Name(name) => {
            let raw_name = name.id.as_str();
            if let Some(slot) = scope.local_slot(raw_name) {
                let current = scope.emit(InstKind::LoadLocal(slot))?;
                Ok(AugTarget {
                    current,
                    store: AugStore::Local(slot),
                })
            } else {
                let name = driver.names.intern(raw_name)?;
                let (current, store) = if scope.is_global_name(raw_name) {
                    (scope.emit(InstKind::LoadGlobal(name))?, AugStore::Global(name))
                } else {
                    (scope.emit(InstKind::LoadName(name))?, AugStore::Name(name))
                };
                Ok(AugTarget { current, store })
            }
        }
        Expr::Attribute(attr) => {
            let obj = driver.lower_expr(scope, &attr.value)?;
            let name = driver.names.intern(attr.attr.as_str())?;
            let current = scope.emit(InstKind::LoadAttr { obj, name })?;
            Ok(AugTarget {
                current,
                store: AugStore::Attr { obj, name },
            })
        }
        Expr::Subscript(subscript) => {
            let obj = driver.lower_expr(scope, &subscript.value)?;
            let index = driver.lower_expr(scope, &subscript.slice)?;
            let current = scope.emit(InstKind::SubscriptGet { obj, index })?;
            Ok(AugTarget {
                current,
                store: AugStore::Subscript { obj, index },
            })
        }
        _ => unsupported_expr("augmented assignment target", target),
    }
}

pub(super) fn bin_op_from_operator(op: ruff_python_ast::Operator) -> Result<BinOp, LowerError> {
    Ok(match op {
        ruff_python_ast::Operator::Add => BinOp::Add,
        ruff_python_ast::Operator::Sub => BinOp::Sub,
        ruff_python_ast::Operator::Mult => BinOp::Mul,
        ruff_python_ast::Operator::MatMult => BinOp::MatMul,
        ruff_python_ast::Operator::Div => BinOp::Div,
        ruff_python_ast::Operator::Mod => BinOp::Mod,
        ruff_python_ast::Operator::Pow => BinOp::Pow,
        ruff_python_ast::Operator::LShift => BinOp::LShift,
        ruff_python_ast::Operator::RShift => BinOp::RShift,
        ruff_python_ast::Operator::BitOr => BinOp::Or,
        ruff_python_ast::Operator::BitXor => BinOp::Xor,
        ruff_python_ast::Operator::BitAnd => BinOp::And,
        ruff_python_ast::Operator::FloorDiv => BinOp::FloorDiv,
    })
}

pub(super) fn lower_ann_assign(stmt: &ruff_python_ast::StmtAnnAssign) -> Result<(), LowerError> {
    unsupported_at(
        "annotated assignment",
        span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()),
    )
}

pub(super) fn lower_type_alias(stmt: &ruff_python_ast::StmtTypeAlias) -> Result<(), LowerError> {
    unsupported_at(
        "type alias statement",
        span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()),
    )
}
