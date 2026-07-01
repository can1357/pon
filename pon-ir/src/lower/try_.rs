use super::*;

fn lower_stmt_list(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    body: &[ruff_python_ast::Stmt],
) -> Result<Option<Terminator>, LowerError> {
    for stmt in body {
        if scope.term.is_some() {
            break;
        }
        driver.lower_stmt(scope, stmt)?;
    }
    Ok(scope.term.take())
}

fn restore_term(scope: &mut BodyScope, term: Option<Terminator>) -> Result<(), LowerError> {
    if let Some(term) = term {
        if scope.term.is_none() {
            scope.set_term(term)?;
        }
    }
    Ok(())
}

fn lower_finally(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    finalbody: &[ruff_python_ast::Stmt],
    pending: Option<Terminator>,
) -> Result<(), LowerError> {
    if finalbody.is_empty() {
        return restore_term(scope, pending);
    }

    let final_term = lower_stmt_list(driver, scope, finalbody)?;
    match final_term {
        Some(term) => restore_term(scope, Some(term)),
        None => restore_term(scope, pending),
    }
}

fn bind_handler_name(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    name: &ruff_python_ast::Identifier,
) -> Result<(), LowerError> {
    let current = scope.emit(InstKind::GetCurrentExc)?;
    let raw_name = name.as_str();
    if scope.is_global_name(raw_name) {
        let name_id = driver.names.intern(raw_name)?;
        scope.emit(InstKind::StoreGlobal(name_id, current))?;
    } else if let Some(slot) = scope.local_slot(raw_name) {
        scope.emit(InstKind::StoreLocal(slot, current))?;
    } else {
        let name_id = driver.names.intern(raw_name)?;
        scope.emit(InstKind::StoreName(name_id, current))?;
    }
    Ok(())
}

fn lower_handler_header(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    handler: &ruff_python_ast::ExceptHandlerExceptHandler,
    is_star: bool,
) -> Result<(), LowerError> {
    if let Some(type_) = handler.type_.as_deref() {
        let exc_type = driver.lower_expr(scope, type_)?;
        if is_star {
            scope.emit(InstKind::CheckExcStar { exc_types: exc_type })?;
        } else {
            scope.emit(InstKind::MatchExc { exc_type })?;
        }
    }
    if let Some(name) = handler.name.as_ref() {
        bind_handler_name(driver, scope, name)?;
    }
    Ok(())
}

fn lower_handlers(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtTry,
) -> Result<Option<Terminator>, LowerError> {
    for handler in &stmt.handlers {
        let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
        scope.emit(InstKind::PushExcInfo)?;
        lower_handler_header(driver, scope, handler, stmt.is_star)?;
        let handler_term = lower_stmt_list(driver, scope, &handler.body)?;
        scope.emit(InstKind::PopExcInfo)?;
        if handler_term.is_some() {
            return Ok(handler_term);
        }
    }
    Ok(None)
}

pub(super) fn lower_try(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtTry,
) -> Result<(), LowerError> {
    scope.emit(InstKind::PushExcInfo)?;
    let body_term = lower_stmt_list(driver, scope, &stmt.body)?;
    scope.emit(InstKind::PopExcInfo)?;

    let pending = if matches!(body_term, Some(Terminator::RaiseTerm)) && !stmt.handlers.is_empty() {
        lower_handlers(driver, scope, stmt)?
    } else {
        if body_term.is_none() {
            lower_stmt_list(driver, scope, &stmt.orelse)?
        } else {
            body_term
        }
    };

    lower_finally(driver, scope, &stmt.finalbody, pending)
}

pub(super) fn lower_raise(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtRaise,
) -> Result<(), LowerError> {
    let exc = match stmt.exc.as_deref() {
        Some(exc) => Some(driver.lower_expr(scope, exc)?),
        None => None,
    };
    let cause = match stmt.cause.as_deref() {
        Some(cause) => Some(driver.lower_expr(scope, cause)?),
        None => None,
    };

    if exc.is_some() {
        scope.emit(InstKind::Raise { exc, cause })?;
    } else {
        scope.emit(InstKind::Reraise)?;
    }
    scope.set_term(Terminator::RaiseTerm)
}
