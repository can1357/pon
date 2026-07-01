use super::*;

fn lower_enter_call(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    manager: Value,
    name: &str,
    is_async: bool,
) -> Result<Value, LowerError> {
    let name = driver.names.intern(name)?;
    let method = scope.emit(InstKind::LoadAttr { obj: manager, name })?;
    let called = scope.emit(InstKind::Call {
        callee: method,
        args: Vec::new(),
    })?;
    if is_async {
        scope.emit(InstKind::Await { awaitable: called })
    } else {
        Ok(called)
    }
}

fn lower_exit_call(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    manager: Value,
    name: &str,
    is_async: bool,
) -> Result<(), LowerError> {
    let name = driver.names.intern(name)?;
    let method = scope.emit(InstKind::LoadAttr { obj: manager, name })?;
    let none = scope.emit(InstKind::Const(PyConst::None))?;
    let called = scope.emit(InstKind::Call {
        callee: method,
        args: vec![none, none, none],
    })?;
    if is_async {
        scope.emit(InstKind::Await { awaitable: called })?;
    }
    Ok(())
}

/// Lowers representative `with`/`async with` ordering into the family-owned IR
/// skeleton: evaluate managers left-to-right, call enter left-to-right, lower the
/// body, then call exits in reverse order.  The exception/finally integration pass
/// wraps this skeleton with handler edges so `__exit__` sees real exception info.
pub(super) fn lower_with_stmt(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtWith,
) -> Result<(), LowerError> {
    let mut managers = Vec::with_capacity(stmt.items.len());
    for item in &stmt.items {
        let manager = driver.lower_expr(scope, &item.context_expr)?;
        let enter_value = lower_enter_call(
            driver,
            scope,
            manager,
            if stmt.is_async { "__aenter__" } else { "__enter__" },
            stmt.is_async,
        )?;
        if let Some(optional_vars) = &item.optional_vars {
            driver.lower_store_target(scope, optional_vars, enter_value)?;
        }
        managers.push(manager);
    }

    for body_stmt in &stmt.body {
        driver.lower_stmt(scope, body_stmt)?;
    }

    for manager in managers.into_iter().rev() {
        lower_exit_call(
            driver,
            scope,
            manager,
            if stmt.is_async { "__aexit__" } else { "__exit__" },
            stmt.is_async,
        )?;
    }
    Ok(())
}

#[allow(dead_code)]
pub(super) fn lower_with(stmt: &ruff_python_ast::StmtWith) -> Result<(), LowerError> {
    unsupported_at(
        if stmt.is_async {
            "async with statement (WS-GEN lowering surface is ready; dispatch seam not wired)"
        } else {
            "with statement (WS-GEN lowering surface is ready; dispatch seam not wired)"
        },
        span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()),
    )
}
