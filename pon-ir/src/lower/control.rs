use super::*;
use crate::ir::{CmpOp, UnOp};

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
	if scope.info.is_async && scope.info.is_generator && ret.value.is_some() {
		// CPython rejects this at compile time (PEP 525): async generators
		// signal exhaustion with StopAsyncIteration, which carries no value.
		return unsupported_at(
			"'return' with value in async generator",
			span_bounds(ret.range.start().to_u32(), ret.range.end().to_u32()),
		);
	}
	let value = match ret.value.as_deref() {
		Some(expr) => driver.lower_expr(scope, expr)?,
		None => scope.emit(InstKind::Const(PyConst::None))?,
	};
	// A `return` inside a protected `try` phase departs through the phase's
	// trampoline (pop handler records + run `finally` bodies on the edge)
	// with the value parked in the shared return slot; the outermost
	// trampoline performs the actual `Return`.
	match scope.return_route {
		Some(route) => {
			let slot = scope.ensure_return_slot();
			scope.emit(InstKind::StoreLocal(slot, value))?;
			scope.set_term(Terminator::Jump(route))
		},
		None => scope.set_term(Terminator::Return(value)),
	}
}

pub(super) fn lower_unary_expr_with_driver(
	driver: &mut LoweringDriver,
	scope: &mut BodyScope,
	unary: &ruff_python_ast::ExprUnaryOp,
) -> Result<Value, LowerError> {
	let operand = driver.lower_expr(scope, &unary.operand)?;
	match unary.op {
		ruff_python_ast::UnaryOp::Not => scope.emit(InstKind::Not { val: operand }),
		ruff_python_ast::UnaryOp::USub => scope.emit(InstKind::UnaryOp { op: UnOp::Neg, operand }),
		ruff_python_ast::UnaryOp::UAdd => scope.emit(InstKind::UnaryOp { op: UnOp::Pos, operand }),
		ruff_python_ast::UnaryOp::Invert => {
			scope.emit(InstKind::UnaryOp { op: UnOp::Invert, operand })
		},
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
	let Some((&first_op, rest_ops)) = compare.ops.split_first() else {
		return Err(LowerError::internal("empty comparison expression"));
	};
	let (first_comparator, rest_comparators) = compare
		.comparators
		.split_first()
		.ok_or_else(|| LowerError::internal("empty comparison expression"))?;

	let lhs = driver.lower_expr(scope, &compare.left)?;
	let rhs = driver.lower_expr(scope, first_comparator)?;
	let first = lower_single_compare(scope, first_op, lhs, rhs)?;
	if rest_ops.is_empty() {
		return Ok(first);
	}

	// CPython chain semantics: `a op1 b op2 c` is `a op1 b and b op2 c` with
	// `b` evaluated once. Each intermediate RAW comparison result is
	// truth-tested only to decide the short circuit; a falsy intermediate is
	// itself the value of the whole expression, and the remaining
	// comparators are never evaluated.
	let result_slot = scope.alloc_temp_local();
	let done_block = scope.alloc_block()?;
	scope.emit(InstKind::StoreLocal(result_slot, first))?;
	let mut lhs = rhs;
	let mut comparison = first;
	for (op, rhs_expr) in rest_ops.iter().copied().zip(rest_comparators) {
		let cond = scope.emit(InstKind::BoolTest { val: comparison })?;
		let next_block = scope.alloc_block()?;
		scope.set_term(Terminator::CondBranch { cond, then_: next_block, else_: done_block })?;
		scope.switch_to(next_block)?;
		let rhs = driver.lower_expr(scope, rhs_expr)?;
		comparison = lower_single_compare(scope, op, lhs, rhs)?;
		scope.emit(InstKind::StoreLocal(result_slot, comparison))?;
		lhs = rhs;
	}
	scope.jump_if_open(done_block)?;
	scope.switch_to(done_block)?;
	scope.emit(InstKind::LoadLocal(result_slot))
}

pub(super) fn lower_bool_expr_with_driver(
	driver: &mut LoweringDriver,
	scope: &mut BodyScope,
	bool_op: &ruff_python_ast::ExprBoolOp,
) -> Result<Value, LowerError> {
	if bool_op.values.is_empty() {
		return Err(LowerError::internal("empty boolean operation"));
	}
	// CPython semantics: `a or b` / `a and b` evaluate operands left to right,
	// short-circuit, and yield the deciding OPERAND VALUE (not its truthiness).
	let result_slot = scope.alloc_temp_local();
	let done_block = scope.alloc_block()?;
	let last = bool_op.values.len() - 1;
	for (index, expr) in bool_op.values.iter().enumerate() {
		let value = driver.lower_expr(scope, expr)?;
		scope.emit(InstKind::StoreLocal(result_slot, value))?;
		if index == last {
			scope.jump_if_open(done_block)?;
			break;
		}
		let cond = scope.emit(InstKind::BoolTest { val: value })?;
		let next_block = scope.alloc_block()?;
		let (then_, else_) = match bool_op.op {
			// `and`: falsy operand short-circuits and is the result
			ruff_python_ast::BoolOp::And => (next_block, done_block),
			// `or`: truthy operand short-circuits and is the result
			ruff_python_ast::BoolOp::Or => (done_block, next_block),
		};
		scope.set_term(Terminator::CondBranch { cond, then_, else_ })?;
		scope.switch_to(next_block)?;
	}
	scope.switch_to(done_block)?;
	scope.emit(InstKind::LoadLocal(result_slot))
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
	scope.set_term(Terminator::CondBranch { cond, then_: then_block, else_: else_block })?;

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
		ruff_python_ast::CmpOp::Eq => scope.emit(InstKind::Compare { op: CmpOp::Eq, lhs, rhs }),
		ruff_python_ast::CmpOp::NotEq => scope.emit(InstKind::Compare { op: CmpOp::Ne, lhs, rhs }),
		ruff_python_ast::CmpOp::Lt => scope.emit(InstKind::Compare { op: CmpOp::Lt, lhs, rhs }),
		ruff_python_ast::CmpOp::LtE => scope.emit(InstKind::Compare { op: CmpOp::Le, lhs, rhs }),
		ruff_python_ast::CmpOp::Gt => scope.emit(InstKind::Compare { op: CmpOp::Gt, lhs, rhs }),
		ruff_python_ast::CmpOp::GtE => scope.emit(InstKind::Compare { op: CmpOp::Ge, lhs, rhs }),
		ruff_python_ast::CmpOp::Is => scope.emit(InstKind::Is { lhs, rhs, negate: false }),
		ruff_python_ast::CmpOp::IsNot => scope.emit(InstKind::Is { lhs, rhs, negate: true }),
		ruff_python_ast::CmpOp::In => {
			scope.emit(InstKind::Contains { item: lhs, container: rhs, negate: false })
		},
		ruff_python_ast::CmpOp::NotIn => {
			scope.emit(InstKind::Contains { item: lhs, container: rhs, negate: true })
		},
	}
}
#[derive(Clone, Copy)]
pub(super) struct LoopTargets {
	pub(super) break_block:    BlockId,
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
	scope.set_term(Terminator::CondBranch { cond, then_: then_block, else_: else_block })
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
	scope.set_term(Terminator::CondBranch { cond, then_: body_block, else_: done_block })
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
	scope.set_term(Terminator::ForLoop { iter, body: body_block, done: done_block })
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

pub(super) fn lower_assert(
	driver: &mut LoweringDriver,
	scope: &mut BodyScope,
	stmt: &ruff_python_ast::StmtAssert,
) -> Result<(), LowerError> {
	let test = driver.lower_expr(scope, &stmt.test)?;
	let cond = scope.emit(InstKind::BoolTest { val: test })?;
	let ok_block = scope.alloc_block()?;
	let fail_block = scope.alloc_block()?;
	scope.set_term(Terminator::CondBranch { cond, then_: ok_block, else_: fail_block })?;

	scope.switch_to(fail_block)?;
	let assertion_name = driver.names.intern("AssertionError")?;
	let assertion_type = scope.emit(InstKind::LoadGlobal(assertion_name))?;
	let exc = if let Some(msg) = stmt.msg.as_deref() {
		let msg = driver.lower_expr(scope, msg)?;
		scope.emit(InstKind::Call { callee: assertion_type, args: vec![msg] })?
	} else {
		scope.emit(InstKind::Call { callee: assertion_type, args: Vec::new() })?
	};
	scope.emit(InstKind::Raise { exc: Some(exc), cause: None })?;
	scope.set_term(Terminator::RaiseTerm)?;
	scope.switch_to(ok_block)?;
	Ok(())
}

pub(super) fn lower_pass(_stmt: &ruff_python_ast::StmtPass) -> Result<(), LowerError> {
	Ok(())
}
