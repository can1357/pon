use super::*;

// Comprehensions are currently rejected rather than lowered.
//
// CPython evaluates a list/set/dict comprehension (and a generator expression)
// in its own implicit function scope: the iteration targets bind as locals of
// that nested scope and never leak into or clobber the enclosing scope. Scope
// analysis already models this (`ScopeKind::Comprehension` children with their
// own symbol tables), but lowering does not yet synthesize the nested function
// and call it. Lowering the body inline in the enclosing `BodyScope` would both
// miscompile the loop (a single straight-line `ForNext` runs once) and misbind
// the target (`lower_store_target` resolves against the wrong symbol table,
// clobbering an enclosing local or a global). Correctness-first, these forms are
// refused until the synthesized comprehension-scope path lands.

fn reject(feature: &str, span: SourceSpan) -> Result<Value, LowerError> {
    unsupported_at(
        format!("{feature} (needs comprehension-scope synthesized-function lowering)"),
        span,
    )
}

/// Rejects a list comprehension pending comprehension-scope lowering.
pub(super) fn lower_list_comp_inline(
    _driver: &mut LoweringDriver,
    _scope: &mut BodyScope,
    expr: &ruff_python_ast::ExprListComp,
) -> Result<Value, LowerError> {
    reject("list comprehension", span_bounds(expr.range.start().to_u32(), expr.range.end().to_u32()))
}

/// Rejects a set comprehension pending comprehension-scope lowering.
pub(super) fn lower_set_comp_inline(
    _driver: &mut LoweringDriver,
    _scope: &mut BodyScope,
    expr: &ruff_python_ast::ExprSetComp,
) -> Result<Value, LowerError> {
    reject("set comprehension", span_bounds(expr.range.start().to_u32(), expr.range.end().to_u32()))
}

/// Rejects a dict comprehension pending comprehension-scope lowering.
pub(super) fn lower_dict_comp_inline(
    _driver: &mut LoweringDriver,
    _scope: &mut BodyScope,
    expr: &ruff_python_ast::ExprDictComp,
) -> Result<Value, LowerError> {
    reject("dict comprehension", span_bounds(expr.range.start().to_u32(), expr.range.end().to_u32()))
}

/// Rejects a generator expression pending comprehension-scope lowering.
pub(super) fn lower_generator_expr(expr: &ruff_python_ast::ExprGenerator) -> Result<Value, LowerError> {
    reject("generator expression", span_bounds(expr.range.start().to_u32(), expr.range.end().to_u32()))
}
