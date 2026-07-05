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

pub(super) fn lower_delete(
	driver: &mut LoweringDriver,
	scope: &mut BodyScope,
	stmt: &ruff_python_ast::StmtDelete,
) -> Result<(), LowerError> {
	for target in &stmt.targets {
		lower_delete_target(driver, scope, target)?;
	}
	Ok(())
}

fn lower_delete_target(
	driver: &mut LoweringDriver,
	scope: &mut BodyScope,
	target: &Expr,
) -> Result<(), LowerError> {
	match target {
		Expr::Name(name) if matches!(name.ctx, ExprContext::Del) => {
			lower_delete_name(driver, scope, name.id.as_str())
		},
		Expr::Attribute(attr) if matches!(attr.ctx, ExprContext::Del) => {
			let obj = driver.lower_expr(scope, &attr.value)?;
			let name = driver.names.intern(attr.attr.as_str())?;
			scope.emit(InstKind::DeleteAttr { obj, name })?;
			Ok(())
		},
		Expr::Subscript(subscript) if matches!(subscript.ctx, ExprContext::Del) => {
			let obj = driver.lower_expr(scope, &subscript.value)?;
			let index = driver.lower_expr(scope, &subscript.slice)?;
			scope.emit(InstKind::SubscriptDel { obj, index })?;
			Ok(())
		},
		Expr::Tuple(tuple) => {
			for elt in &tuple.elts {
				lower_delete_target(driver, scope, elt)?;
			}
			Ok(())
		},
		Expr::List(list) => {
			for elt in &list.elts {
				lower_delete_target(driver, scope, elt)?;
			}
			Ok(())
		},
		_ => unsupported_expr("delete target", target),
	}
}

fn lower_delete_name(
	driver: &mut LoweringDriver,
	scope: &mut BodyScope,
	raw_name: &str,
) -> Result<(), LowerError> {
	if scope.is_global_name(raw_name) {
		let name = driver.names.intern(raw_name)?;
		scope.emit(InstKind::DeleteGlobal(name))?;
	} else if scope.is_class() {
		if let Some(cell) = scope.class_deref_cell(raw_name) {
			scope.emit(InstKind::DeleteCell(cell))?;
		} else {
			let name = driver.names.intern(raw_name)?;
			scope.emit(InstKind::DeleteName(name))?;
		}
	} else {
		match scope.name_class(raw_name).cloned() {
			Some(NameClass::Cell { cell_slot, .. }) => {
				scope.emit(InstKind::DeleteCell(CellId(cell_slot)))?;
			},
			Some(NameClass::Free { slot }) => {
				let cell = scope.free_cell(slot);
				scope.emit(InstKind::DeleteCell(cell))?;
			},
			Some(NameClass::Local { slot }) => {
				scope.emit(InstKind::DeleteLocal(LocalId(slot)))?;
			},
			Some(NameClass::Builtin) | Some(NameClass::Global { .. }) | None => {
				let name = driver.names.intern(raw_name)?;
				scope.emit(InstKind::DeleteName(name))?;
			},
		}
	}
	Ok(())
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
	let value = scope.emit(InstKind::InplaceOp { op, lhs: target.current, rhs })?;
	target.store(scope, value)
}

struct AugTarget {
	current: Value,
	store:   AugStore,
}

impl AugTarget {
	fn store(self, scope: &mut BodyScope, value: Value) -> Result<(), LowerError> {
		match self.store {
			AugStore::Local(slot) => {
				scope.emit(InstKind::StoreLocal(slot, value))?;
			},
			AugStore::Global(name) => {
				scope.emit(InstKind::StoreGlobal(name, value))?;
			},
			AugStore::Cell(cell) => {
				scope.emit(InstKind::StoreCell(cell, value))?;
			},
			AugStore::Name(name) => {
				scope.emit(InstKind::StoreName(name, value))?;
			},
			AugStore::Attr { obj, name } => {
				scope.emit(InstKind::StoreAttr { obj, name, val: value })?;
			},
			AugStore::Subscript { obj, index } => {
				scope.emit(InstKind::SubscriptSet { obj, index, val: value })?;
			},
		}
		Ok(())
	}
}

enum AugStore {
	Local(LocalId),
	Cell(CellId),
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
			if scope.is_global_name(raw_name) {
				let name = driver.names.intern(raw_name)?;
				let current = scope.emit(InstKind::LoadGlobal(name))?;
				return Ok(AugTarget { current, store: AugStore::Global(name) });
			}
			if scope.is_class() {
				// `nonlocal` targets in a class body augment the enclosing
				// function's cell; everything else goes through the namespace.
				if let Some(cell) = scope.class_deref_cell(raw_name) {
					let current = scope.emit(InstKind::LoadCell(cell))?;
					return Ok(AugTarget { current, store: AugStore::Cell(cell) });
				}
				let name = driver.names.intern(raw_name)?;
				let current = scope.emit(InstKind::LoadName(name))?;
				return Ok(AugTarget { current, store: AugStore::Name(name) });
			}
			match scope.name_class(raw_name) {
				Some(NameClass::Cell { cell_slot, .. }) => {
					let cell = CellId(*cell_slot);
					let current = scope.emit(InstKind::LoadCell(cell))?;
					Ok(AugTarget { current, store: AugStore::Cell(cell) })
				},
				Some(NameClass::Free { slot }) => {
					let cell = scope.free_cell(*slot);
					let current = scope.emit(InstKind::LoadCell(cell))?;
					Ok(AugTarget { current, store: AugStore::Cell(cell) })
				},
				Some(NameClass::Local { slot }) => {
					let slot = LocalId(*slot);
					let current = scope.emit(InstKind::LoadLocal(slot))?;
					Ok(AugTarget { current, store: AugStore::Local(slot) })
				},
				Some(NameClass::Builtin) | Some(NameClass::Global { .. }) | None => {
					let name = driver.names.intern(raw_name)?;
					let current = scope.emit(InstKind::LoadName(name))?;
					Ok(AugTarget { current, store: AugStore::Name(name) })
				},
			}
		},
		Expr::Attribute(attr) => {
			let obj = driver.lower_expr(scope, &attr.value)?;
			let name = driver.names.intern(attr.attr.as_str())?;
			let current = scope.emit(InstKind::LoadAttr { obj, name })?;
			Ok(AugTarget { current, store: AugStore::Attr { obj, name } })
		},
		Expr::Subscript(subscript) => {
			let obj = driver.lower_expr(scope, &subscript.value)?;
			let index = driver.lower_expr(scope, &subscript.slice)?;
			let current = scope.emit(InstKind::SubscriptGet { obj, index })?;
			Ok(AugTarget { current, store: AugStore::Subscript { obj, index } })
		},
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

pub(super) fn lower_ann_assign(
	driver: &mut LoweringDriver,
	scope: &mut BodyScope,
	stmt: &ruff_python_ast::StmtAnnAssign,
) -> Result<(), LowerError> {
	// PEP 649: the annotation expression is NOT evaluated here.  Module and
	// class name-target annotations were routed into the deferred
	// `__annotate__` child by scope analysis and evaluate lazily on first
	// `__annotations__` access; function-local annotations evaluate never.
	if let Some(value) = stmt.value.as_deref() {
		let value = driver.lower_expr(scope, value)?;
		driver.lower_store_target(scope, &stmt.target, value)?;
	}
	Ok(())
}

pub(super) fn lower_type_alias(
	driver: &mut LoweringDriver,
	scope: &mut BodyScope,
	stmt: &ruff_python_ast::StmtTypeAlias,
) -> Result<(), LowerError> {
	// PEP 695: `type X = expr` synthesizes a zero-argument value thunk (the
	// `<type_alias>` child claimed by this statement's span) and wraps it
	// in a `TypeAliasType` that evaluates `expr` on first `__value__` access.
	let thunk_info = scope.next_child_scope(
		ScopeKind::Function,
		scope::TYPE_ALIAS_SCOPE_NAME,
		Some(scope::span_key(stmt.range)),
	)?;
	let thunk = synth::synthesize_type_alias_thunk(driver, scope, thunk_info, &stmt.value)?;

	let Expr::Name(target) = stmt.name.as_ref() else {
		return unsupported_expr("type alias target", &stmt.name);
	};
	let name = driver.names.intern(target.id.as_str())?;
	let alias = scope.emit(InstKind::MakeTypeAlias { name, thunk })?;
	driver.lower_store_target(scope, &stmt.name, alias)
}
