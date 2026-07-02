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

fn lower_stmt_list_preserving_term(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    body: &[ruff_python_ast::Stmt],
) -> Result<(), LowerError> {
    for stmt in body {
        if scope.term.is_some() {
            break;
        }
        driver.lower_stmt(scope, stmt)?;
    }
    Ok(())
}

fn redirect_raise_terms(scope: &mut BodyScope, start_block: usize, handler_block: BlockId) -> bool {
    let mut redirected = false;
    for block in &mut scope.blocks[start_block..] {
        if matches!(block.term, Terminator::RaiseTerm) {
            block.term = Terminator::Jump(handler_block);
            redirected = true;
        }
    }
    if matches!(scope.term, Some(Terminator::RaiseTerm)) {
        scope.term = Some(Terminator::Jump(handler_block));
        redirected = true;
    }
    redirected
}

fn lower_handler_bodies(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtTry,
) -> Result<Option<Terminator>, LowerError> {
    for handler in &stmt.handlers {
        let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
        lower_handler_header(driver, scope, handler, stmt.is_star)?;
        let saved_exc = scope.emit(InstKind::GetCurrentExc)?;
        let saved_slot = scope.alloc_temp_local();
        scope.emit(InstKind::StoreLocal(saved_slot, saved_exc))?;
        let previous_reraise = scope.reraise_exc;
        scope.reraise_exc = Some(saved_slot);
        let handler_term = lower_stmt_list(driver, scope, &handler.body)?;
        scope.reraise_exc = previous_reraise;
        if handler_term.is_some() {
            return Ok(handler_term);
        }
    }
    Ok(None)
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

    let pending_raise = matches!(pending, Some(Terminator::RaiseTerm));
    let saved_exception = if pending_raise {
        Some(scope.emit(InstKind::GetCurrentExc)?)
    } else {
        None
    };
    let final_term = lower_stmt_list(driver, scope, finalbody)?;
    match final_term {
        Some(term) => restore_term(scope, Some(term)),
        None => {
            if let Some(saved) = saved_exception {
                scope.emit(InstKind::Raise {
                    exc: Some(saved),
                    cause: None,
                })?;
            }
            restore_term(scope, pending)
        }
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


pub(super) fn lower_try(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtTry,
) -> Result<(), LowerError> {
    if stmt.handlers.is_empty() {
        if stmt.finalbody.is_empty() {
            let body_term = lower_stmt_list(driver, scope, &stmt.body)?;
            return restore_term(scope, body_term);
        }
        return lower_try_finally_only(driver, scope, stmt);
    }

    let handler_block = scope.alloc_block()?;
    let done_block = scope.alloc_block()?;
    scope.emit(InstKind::PushExcInfo {
        target: handler_block,
        stack_depth: 0,
        kind: 0,
    })?;
    let protected_start = scope.blocks.len();
    lower_stmt_list_preserving_term(driver, scope, &stmt.body)?;

    let redirected = redirect_raise_terms(scope, protected_start, handler_block);
    if !redirected && scope.term.is_some() {
        let body_term = scope.term.take();
        scope.emit(InstKind::PopExcInfo)?;
        return lower_finally(driver, scope, &stmt.finalbody, body_term);
    }

    if scope.term.is_none() {
        scope.emit(InstKind::PopExcInfo)?;
        let normal_term = lower_stmt_list(driver, scope, &stmt.orelse)?;
        lower_finally(driver, scope, &stmt.finalbody, normal_term)?;
        scope.jump_if_open(done_block)?;
    }

    scope.switch_to(handler_block)?;
    scope.emit(InstKind::PopExcInfo)?;
    let handler_term = lower_handler_bodies(driver, scope, stmt)?;
    lower_finally(driver, scope, &stmt.finalbody, handler_term)?;
    scope.jump_if_open(done_block)?;
    scope.switch_to(done_block)?;
    Ok(())
}
/// Lowers handler-less `try/finally` with a real exception route.
///
/// The finally body must run on BOTH exits (pin J0.1 §5.2):
/// - the static path (fallthrough, `return`, `break`/`continue` jumps, and
///   lexical `raise`) inlines a finally copy before the pending terminator,
///   exactly like the old lowering; and
/// - the dynamic path (a helper call inside the body raising) routes through
///   a handler record (`PushExcInfo` targeting a finally-exception block)
///   that runs a second finally copy and re-raises the pending exception.
///
/// Generators get finally-across-yield for free: the state-machine transform
/// pops/re-pushes this record around suspend points like any other handler,
/// so `throw()`/`close()` payloads delivered at the resume point NULL-route
/// into the finally-exception block.
fn lower_try_finally_only(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtTry,
) -> Result<(), LowerError> {
    let finally_exc_block = scope.alloc_block()?;
    let done_block = scope.alloc_block()?;
    scope.emit(InstKind::PushExcInfo {
        target: finally_exc_block,
        stack_depth: 0,
        kind: 0,
    })?;
    let protected_start = scope.blocks.len();
    lower_stmt_list_preserving_term(driver, scope, &stmt.body)?;
    // A lexical `raise` in the body already NULL-routes to the finally block
    // through its own Raise instruction; retarget its unreachable static
    // terminator too so the raise edge is explicit in the CFG.
    redirect_raise_terms(scope, protected_start, finally_exc_block);

    // Static exits: pop the record, then run the inline finally copy before
    // the preserved terminator (fallthrough / return / loop jumps).
    if !matches!(scope.term, Some(Terminator::Jump(target)) if target == finally_exc_block) {
        let body_term = scope.term.take();
        scope.emit(InstKind::PopExcInfo)?;
        lower_finally(driver, scope, &stmt.finalbody, body_term)?;
    }
    scope.jump_if_open(done_block)?;

    // Dynamic exit: the pending exception routed here.  Pop the record, run
    // the exception-path finally copy, and re-deliver the exception (a
    // finally body that returns/raises replaces it, matching CPython).
    scope.switch_to(finally_exc_block)?;
    scope.emit(InstKind::PopExcInfo)?;
    lower_finally(driver, scope, &stmt.finalbody, Some(Terminator::RaiseTerm))?;
    scope.jump_if_open(done_block)?;
    scope.switch_to(done_block)?;
    Ok(())
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
    } else if let Some(saved) = scope.reraise_exc {
        let exc = scope.emit(InstKind::LoadLocal(saved))?;
        scope.emit(InstKind::Raise {
            exc: Some(exc),
            cause: None,
        })?;
    } else {
        scope.emit(InstKind::Reraise)?;
    }
    scope.set_term(Terminator::RaiseTerm)
}
