use super::*;

fn lower_stmt_list(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    body: &[ruff_python_ast::Stmt],
    loop_targets: Option<control::LoopTargets>,
) -> Result<Option<Terminator>, LowerError> {
    for stmt in body {
        if scope.term.is_some() {
            break;
        }
        driver.lower_stmt_with_loop(scope, stmt, loop_targets)?;
    }
    Ok(scope.term.take())
}

fn lower_stmt_list_preserving_term(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    body: &[ruff_python_ast::Stmt],
    loop_targets: Option<control::LoopTargets>,
) -> Result<(), LowerError> {
    for stmt in body {
        if scope.term.is_some() {
            break;
        }
        driver.lower_stmt_with_loop(scope, stmt, loop_targets)?;
    }
    Ok(())
}

pub(super) fn redirect_raise_terms(scope: &mut BodyScope, start_block: usize, handler_block: BlockId) -> bool {
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

/// Departing-edge unwind trampolines for one protected phase of a `try`
/// statement.
///
/// Pin J0.6 §3.1 requires `PopExcInfo` on EVERY exit edge of a protected
/// region, and CPython runs `finally` bodies on `break`/`continue`/`return`
/// edges that leave the statement.  While a phase's statements are lowered,
/// `break`/`continue` target the trampolines (via rewritten
/// [`control::LoopTargets`]) and `return` routes through
/// `BodyScope::return_route` with its value parked in the shared return
/// slot.  [`PhaseExits::fill`] then materializes only the trampolines that
/// were actually jumped to: each pops this phase's handler record (protected
/// body only), inlines a `finally` copy under the OUTER loop/return context,
/// and forwards outward — nested `try`s chain trampolines, so every frame
/// pops exactly its own record and runs exactly its own `finally`.
struct PhaseExits {
    /// Pop this phase's own handler record on the way out (true only while
    /// the record pushed for the protected body is still active).
    pop_record: bool,
    break_tramp: Option<BlockId>,
    continue_tramp: Option<BlockId>,
    return_tramp: Option<BlockId>,
    /// Loop targets in force outside the `try` statement.
    outer_targets: Option<control::LoopTargets>,
    /// Return route in force outside the `try` statement.
    outer_route: Option<BlockId>,
    /// Targets the phase body is lowered under: the trampolines, or the
    /// outer targets when this phase has no unwind obligations.
    inner_targets: Option<control::LoopTargets>,
}

impl PhaseExits {
    /// Allocates trampoline ids for a phase that must pop its handler record
    /// (`pop_record`) and/or run a `finally` body (`needs_finally`) on
    /// departing edges.  A phase with neither obligation passes the outer
    /// context straight through.
    fn begin(
        scope: &mut BodyScope,
        outer_targets: Option<control::LoopTargets>,
        pop_record: bool,
        needs_finally: bool,
    ) -> Result<Self, LowerError> {
        let outer_route = scope.return_route;
        if !(pop_record || needs_finally) {
            return Ok(Self {
                pop_record,
                break_tramp: None,
                continue_tramp: None,
                return_tramp: None,
                outer_targets,
                outer_route,
                inner_targets: outer_targets,
            });
        }
        let (break_tramp, continue_tramp, inner_targets) = match outer_targets {
            Some(_) => {
                let break_tramp = scope.alloc_block()?;
                let continue_tramp = scope.alloc_block()?;
                (
                    Some(break_tramp),
                    Some(continue_tramp),
                    Some(control::LoopTargets {
                        break_block: break_tramp,
                        continue_block: continue_tramp,
                    }),
                )
            }
            None => (None, None, None),
        };
        let return_tramp = if scope.is_module() {
            None
        } else {
            Some(scope.alloc_block()?)
        };
        Ok(Self {
            pop_record,
            break_tramp,
            continue_tramp,
            return_tramp,
            outer_targets,
            outer_route,
            inner_targets,
        })
    }

    /// Installs this phase's return route; returns the route to restore.
    fn enter(&self, scope: &mut BodyScope) -> Option<BlockId> {
        let saved = scope.return_route;
        if let Some(tramp) = self.return_tramp {
            scope.return_route = Some(tramp);
        }
        saved
    }

    fn exit(scope: &mut BodyScope, saved: Option<BlockId>) {
        scope.return_route = saved;
    }

    /// Whether `target` is one of this phase's trampolines.
    fn is_tramp(&self, target: BlockId) -> bool {
        [self.break_tramp, self.continue_tramp, self.return_tramp]
            .into_iter()
            .flatten()
            .any(|tramp| tramp == target)
    }

    /// Whether a pending terminator departs through one of this phase's
    /// trampolines (which then owns the pop + `finally` duties).
    fn routes(&self, term: Option<&Terminator>) -> bool {
        matches!(term, Some(Terminator::Jump(target)) if self.is_tramp(*target))
    }

    /// Materializes every trampoline that lowered code jumped to.  MUST run
    /// with the phase contexts exited (outer targets/route back in force) and
    /// `scope.term` set, before control switches to the try's done block.
    fn fill(
        self,
        driver: &mut LoweringDriver,
        scope: &mut BodyScope,
        finalbody: &[ruff_python_ast::Stmt],
        scan_start: usize,
    ) -> Result<(), LowerError> {
        fn referenced(scope: &BodyScope, scan_start: usize, tramp: BlockId) -> bool {
            scope.blocks[scan_start..]
                .iter()
                .map(|block| &block.term)
                .chain(scope.term.as_ref())
                .any(|term| matches!(term, Terminator::Jump(target) if *target == tramp))
        }

        // break / continue: pop, run the finally copy, resume the outer edge.
        for (tramp, outer) in [
            (self.break_tramp, self.outer_targets.map(|t| t.break_block)),
            (self.continue_tramp, self.outer_targets.map(|t| t.continue_block)),
        ] {
            let (Some(tramp), Some(outer)) = (tramp, outer) else {
                continue;
            };
            if !referenced(scope, scan_start, tramp) {
                continue;
            }
            scope.switch_to(tramp)?;
            if self.pop_record {
                scope.emit(InstKind::PopExcInfo)?;
            }
            lower_finally(
                driver,
                scope,
                finalbody,
                Some(Terminator::Jump(outer)),
                self.outer_targets,
            )?;
        }

        // return: pop, run the finally copy, then forward to the next route
        // outward (value stays parked in the shared slot) or actually return.
        let Some(tramp) = self.return_tramp else {
            return Ok(());
        };
        if !referenced(scope, scan_start, tramp) {
            return Ok(());
        }
        scope.switch_to(tramp)?;
        if self.pop_record {
            scope.emit(InstKind::PopExcInfo)?;
        }
        lower_finally(driver, scope, finalbody, None, self.outer_targets)?;
        if scope.term.is_none() {
            match self.outer_route {
                Some(route) => scope.set_term(Terminator::Jump(route))?,
                None => {
                    let slot = scope
                        .return_slot
                        .ok_or_else(|| LowerError::internal("routed return without a return slot"))?;
                    let value = scope.emit(InstKind::LoadLocal(slot))?;
                    scope.set_term(Terminator::Return(value))?;
                }
            }
        }
        Ok(())
    }
}

fn lower_handler_bodies(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtTry,
    done_block: BlockId,
    aux_exits: &PhaseExits,
    loop_targets: Option<control::LoopTargets>,
) -> Result<(), LowerError> {
    for handler in &stmt.handlers {
        let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
        let body_block = scope.alloc_block()?;
        let next_block = if handler.type_.is_some() {
            Some(scope.alloc_block()?)
        } else {
            None
        };

        if let Some(type_) = handler.type_.as_deref() {
            let exc_type = driver.lower_expr(scope, type_)?;
            let matched = scope.emit(InstKind::MatchExc { exc_type })?;
            let cond = scope.emit(InstKind::BoolTest { val: matched })?;
            scope.set_term(Terminator::CondBranch {
                cond,
                then_: body_block,
                else_: next_block.expect("typed handler has a miss block"),
            })?;
        } else {
            scope.set_term(Terminator::Jump(body_block))?;
        }

        scope.switch_to(body_block)?;
        if let Some(name) = handler.name.as_ref() {
            bind_handler_name(driver, scope, name)?;
        }
        let saved_exc = scope.emit(InstKind::GetCurrentExc)?;
        let saved_slot = scope.alloc_temp_local();
        scope.emit(InstKind::StoreLocal(saved_slot, saved_exc))?;
        let previous_reraise = scope.reraise_exc;
        scope.reraise_exc = Some(saved_slot);
        let saved_route = aux_exits.enter(scope);
        let handler_term = lower_stmt_list(driver, scope, &handler.body, aux_exits.inner_targets)?;
        PhaseExits::exit(scope, saved_route);
        scope.reraise_exc = previous_reraise;
        if aux_exits.routes(handler_term.as_ref()) {
            restore_term(scope, handler_term)?;
        } else {
            lower_finally(driver, scope, &stmt.finalbody, handler_term, loop_targets)?;
        }
        scope.jump_if_open(done_block)?;

        let Some(next_block) = next_block else {
            return Ok(());
        };
        scope.switch_to(next_block)?;
    }

    scope.emit(InstKind::Reraise)?;
    lower_finally(driver, scope, &stmt.finalbody, Some(Terminator::RaiseTerm), loop_targets)?;
    scope.jump_if_open(done_block)?;
    Ok(())
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
    loop_targets: Option<control::LoopTargets>,
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
    let final_term = lower_stmt_list(driver, scope, finalbody, loop_targets)?;
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

fn store_handler_name_value(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    name: &ruff_python_ast::Identifier,
    value: Value,
) -> Result<(), LowerError> {
    let raw_name = name.as_str();
    if scope.is_global_name(raw_name) {
        let name_id = driver.names.intern(raw_name)?;
        scope.emit(InstKind::StoreGlobal(name_id, value))?;
    } else if let Some(slot) = scope.local_slot(raw_name) {
        scope.emit(InstKind::StoreLocal(slot, value))?;
    } else {
        let name_id = driver.names.intern(raw_name)?;
        scope.emit(InstKind::StoreName(name_id, value))?;
    }
    Ok(())
}

fn bind_handler_name(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    name: &ruff_python_ast::Identifier,
) -> Result<(), LowerError> {
    let current = scope.emit(InstKind::GetCurrentExc)?;
    store_handler_name_value(driver, scope, name, current)
}

fn clear_handler_name(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    name: Option<&ruff_python_ast::Identifier>,
) -> Result<(), LowerError> {
    if let Some(name) = name {
        let none = scope.emit(InstKind::Const(PyConst::None))?;
        store_handler_name_value(driver, scope, name, none)?;
    }
    Ok(())
}


fn lower_exc_star_clause(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    handler: &ruff_python_ast::ExceptHandlerExceptHandler,
    next_head: BlockId,
    loop_targets: Option<control::LoopTargets>,
) -> Result<(), LowerError> {
    let Some(type_) = handler.type_.as_deref() else {
        return Err(LowerError::parse("expected one or more exception types after except*"));
    };

    let body_block = scope.alloc_block()?;
    let raised_block = scope.alloc_block()?;
    let exc_types = driver.lower_expr(scope, type_)?;
    let matched = scope.emit(InstKind::ExcStarMatch { exc_types })?;
    let cond = scope.emit(InstKind::BoolTest { val: matched })?;
    scope.set_term(Terminator::CondBranch {
        cond,
        then_: body_block,
        else_: next_head,
    })?;

    scope.switch_to(body_block)?;
    if let Some(name) = handler.name.as_ref() {
        bind_handler_name(driver, scope, name)?;
    }
    let saved_exc = scope.emit(InstKind::GetCurrentExc)?;
    let saved_slot = scope.alloc_temp_local();
    scope.emit(InstKind::StoreLocal(saved_slot, saved_exc))?;
    scope.emit(InstKind::PushExcInfo {
        target: raised_block,
        stack_depth: 0,
        kind: 1,
    })?;
    let previous_reraise = scope.reraise_exc;
    scope.reraise_exc = Some(saved_slot);
    let body_start = scope.blocks.len();
    lower_stmt_list_preserving_term(driver, scope, &handler.body, loop_targets)?;
    redirect_raise_terms(scope, body_start, raised_block);
    scope.reraise_exc = previous_reraise;

    match scope.term.take() {
        // A lexical `raise` ended the body: `redirect_raise_terms` already
        // points the open block at `raised_block`.  Keep that edge as the
        // block's terminator (the raised path pops the handler record and
        // records the body exception) so `switch_to` below can close it.
        Some(term @ Terminator::Jump(target)) if target == raised_block => {
            scope.set_term(term)?;
        }
        Some(term) => {
            scope.emit(InstKind::PopExcInfo)?;
            clear_handler_name(driver, scope, handler.name.as_ref())?;
            scope.emit(InstKind::ExcStarBodyOk)?;
            scope.set_term(term)?;
        }
        None => {
            scope.emit(InstKind::PopExcInfo)?;
            clear_handler_name(driver, scope, handler.name.as_ref())?;
            scope.emit(InstKind::ExcStarBodyOk)?;
            scope.set_term(Terminator::Jump(next_head))?;
        }
    }

    scope.switch_to(raised_block)?;
    scope.emit(InstKind::PopExcInfo)?;
    clear_handler_name(driver, scope, handler.name.as_ref())?;
    scope.emit(InstKind::ExcStarBodyRaised)?;
    scope.set_term(Terminator::Jump(next_head))?;
    scope.switch_to(next_head)?;
    Ok(())
}

fn lower_exc_star_handlers(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtTry,
    finish_block: BlockId,
    loop_targets: Option<control::LoopTargets>,
) -> Result<(), LowerError> {
    for handler in &stmt.handlers {
        let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
        let next_head = scope.alloc_block()?;
        lower_exc_star_clause(driver, scope, handler, next_head, loop_targets)?;
    }
    scope.set_term(Terminator::Jump(finish_block))?;
    Ok(())
}

fn lower_try_star(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtTry,
    loop_targets: Option<control::LoopTargets>,
) -> Result<(), LowerError> {
    let dispatch_block = scope.alloc_block()?;
    let finish_block = scope.alloc_block()?;
    let done_block = scope.alloc_block()?;
    let scan_start = scope.blocks.len();
    let body_exits = PhaseExits::begin(scope, loop_targets, true, !stmt.finalbody.is_empty())?;
    let aux_exits = PhaseExits::begin(scope, loop_targets, false, !stmt.finalbody.is_empty())?;
    scope.emit(InstKind::PushExcInfo {
        target: dispatch_block,
        stack_depth: 0,
        kind: 1,
    })?;
    let protected_start = scope.blocks.len();
    let saved_route = body_exits.enter(scope);
    lower_stmt_list_preserving_term(driver, scope, &stmt.body, body_exits.inner_targets)?;
    PhaseExits::exit(scope, saved_route);

    let redirected = redirect_raise_terms(scope, protected_start, dispatch_block);
    if !redirected && !body_exits.routes(scope.term.as_ref()) && scope.term.is_some() {
        let body_term = scope.term.take();
        scope.emit(InstKind::PopExcInfo)?;
        lower_finally(driver, scope, &stmt.finalbody, body_term, loop_targets)?;
    }

    if scope.term.is_none() {
        scope.emit(InstKind::PopExcInfo)?;
        let saved_route = aux_exits.enter(scope);
        let normal_term = lower_stmt_list(driver, scope, &stmt.orelse, aux_exits.inner_targets)?;
        PhaseExits::exit(scope, saved_route);
        if aux_exits.routes(normal_term.as_ref()) {
            restore_term(scope, normal_term)?;
        } else {
            lower_finally(driver, scope, &stmt.finalbody, normal_term, loop_targets)?;
        }
        scope.jump_if_open(done_block)?;
    }

    scope.switch_to(dispatch_block)?;
    scope.emit(InstKind::PopExcInfo)?;
    scope.emit(InstKind::ExcStarEnter)?;
    lower_exc_star_handlers(driver, scope, stmt, finish_block, loop_targets)?;

    scope.switch_to(finish_block)?;
    scope.emit(InstKind::ExcStarFinish)?;
    lower_finally(driver, scope, &stmt.finalbody, None, loop_targets)?;
    scope.jump_if_open(done_block)?;
    body_exits.fill(driver, scope, &stmt.finalbody, scan_start)?;
    aux_exits.fill(driver, scope, &stmt.finalbody, scan_start)?;
    scope.switch_to(done_block)?;
    Ok(())
}


pub(super) fn lower_try(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtTry,
    loop_targets: Option<control::LoopTargets>,
) -> Result<(), LowerError> {
    if stmt.is_star {
        return lower_try_star(driver, scope, stmt, loop_targets);
    }

    if stmt.handlers.is_empty() {
        if stmt.finalbody.is_empty() {
            let body_term = lower_stmt_list(driver, scope, &stmt.body, loop_targets)?;
            return restore_term(scope, body_term);
        }
        return lower_try_finally_only(driver, scope, stmt, loop_targets);
    }

    let handler_block = scope.alloc_block()?;
    let done_block = scope.alloc_block()?;
    let scan_start = scope.blocks.len();
    let body_exits = PhaseExits::begin(scope, loop_targets, true, !stmt.finalbody.is_empty())?;
    let aux_exits = PhaseExits::begin(scope, loop_targets, false, !stmt.finalbody.is_empty())?;
    scope.emit(InstKind::PushExcInfo {
        target: handler_block,
        stack_depth: 0,
        kind: 0,
    })?;
    let protected_start = scope.blocks.len();
    let saved_route = body_exits.enter(scope);
    lower_stmt_list_preserving_term(driver, scope, &stmt.body, body_exits.inner_targets)?;
    PhaseExits::exit(scope, saved_route);

    let redirected = redirect_raise_terms(scope, protected_start, handler_block);
    if !redirected && !body_exits.routes(scope.term.as_ref()) && scope.term.is_some() {
        let body_term = scope.term.take();
        scope.emit(InstKind::PopExcInfo)?;
        lower_finally(driver, scope, &stmt.finalbody, body_term, loop_targets)?;
        scope.jump_if_open(done_block)?;
    }

    if scope.term.is_none() {
        scope.emit(InstKind::PopExcInfo)?;
        let saved_route = aux_exits.enter(scope);
        let normal_term = lower_stmt_list(driver, scope, &stmt.orelse, aux_exits.inner_targets)?;
        PhaseExits::exit(scope, saved_route);
        if aux_exits.routes(normal_term.as_ref()) {
            restore_term(scope, normal_term)?;
        } else {
            lower_finally(driver, scope, &stmt.finalbody, normal_term, loop_targets)?;
        }
        scope.jump_if_open(done_block)?;
    }

    scope.switch_to(handler_block)?;
    scope.emit(InstKind::PopExcInfo)?;
    lower_handler_bodies(driver, scope, stmt, done_block, &aux_exits, loop_targets)?;
    body_exits.fill(driver, scope, &stmt.finalbody, scan_start)?;
    aux_exits.fill(driver, scope, &stmt.finalbody, scan_start)?;
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
    loop_targets: Option<control::LoopTargets>,
) -> Result<(), LowerError> {
    let finally_exc_block = scope.alloc_block()?;
    let done_block = scope.alloc_block()?;
    let scan_start = scope.blocks.len();
    let body_exits = PhaseExits::begin(scope, loop_targets, true, true)?;
    scope.emit(InstKind::PushExcInfo {
        target: finally_exc_block,
        stack_depth: 0,
        kind: 0,
    })?;
    let protected_start = scope.blocks.len();
    let saved_route = body_exits.enter(scope);
    lower_stmt_list_preserving_term(driver, scope, &stmt.body, body_exits.inner_targets)?;
    PhaseExits::exit(scope, saved_route);
    // A lexical `raise` in the body already NULL-routes to the finally block
    // through its own Raise instruction; retarget its unreachable static
    // terminator too so the raise edge is explicit in the CFG.
    redirect_raise_terms(scope, protected_start, finally_exc_block);

    // Static exits: pop the record, then run the inline finally copy before
    // the preserved terminator.  Departing edges (`break`/`continue`/
    // `return` from nested statements) already routed to a trampoline that
    // owns those duties.
    if !matches!(scope.term, Some(Terminator::Jump(target)) if target == finally_exc_block)
        && !body_exits.routes(scope.term.as_ref())
    {
        let body_term = scope.term.take();
        scope.emit(InstKind::PopExcInfo)?;
        lower_finally(driver, scope, &stmt.finalbody, body_term, loop_targets)?;
    }
    scope.jump_if_open(done_block)?;

    // Dynamic exit: the pending exception routed here.  Pop the record, run
    // the exception-path finally copy, and re-deliver the exception (a
    // finally body that returns/raises replaces it, matching CPython).
    scope.switch_to(finally_exc_block)?;
    scope.emit(InstKind::PopExcInfo)?;
    lower_finally(driver, scope, &stmt.finalbody, Some(Terminator::RaiseTerm), loop_targets)?;
    scope.jump_if_open(done_block)?;
    body_exits.fill(driver, scope, &stmt.finalbody, scan_start)?;
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
