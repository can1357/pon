use super::*;

/// Lowers a `yield` expression once the integration pass wires expression
/// dispatch to pass the active driver and body scope into this family module.
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
/// delegation instruction.  The later state-machine transform splits this at the
/// suspension point and stores the delegate in the heap frame.
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
/// delegation machinery used for `yield from`.  The delegation step records
/// suspension values under the eager-yield fallback and returns the awaited
/// `StopIteration.value` as the expression result.
pub(super) fn lower_await_expr(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    expr: &ruff_python_ast::ExprAwait,
) -> Result<Value, LowerError> {
    let awaitable = driver.lower_expr(scope, &expr.value)?;
    let iter = scope.emit(InstKind::Await { awaitable })?;
    scope.emit(InstKind::YieldFrom { iter })
}

#[allow(dead_code)]
pub(super) fn lower_yield(expr: &ruff_python_ast::ExprYield) -> Result<Value, LowerError> {
    unsupported_at(
        "yield expression (WS-GEN lowering surface is ready; dispatch seam not wired)",
        span_bounds(expr.range.start().to_u32(), expr.range.end().to_u32()),
    )
}

#[allow(dead_code)]
pub(super) fn lower_yield_from(expr: &ruff_python_ast::ExprYieldFrom) -> Result<Value, LowerError> {
    unsupported_at(
        "yield-from expression (WS-GEN lowering surface is ready; dispatch seam not wired)",
        span_bounds(expr.range.start().to_u32(), expr.range.end().to_u32()),
    )
}

#[allow(dead_code)]
pub(super) fn lower_await(expr: &ruff_python_ast::ExprAwait) -> Result<Value, LowerError> {
    unsupported_at(
        "await expression (WS-GEN lowering surface is ready; dispatch seam not wired)",
        span_bounds(expr.range.start().to_u32(), expr.range.end().to_u32()),
    )
}
