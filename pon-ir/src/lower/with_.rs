use super::try_::redirect_raise_terms;
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
    args: [Value; 3],
) -> Result<Value, LowerError> {
    let name = driver.names.intern(name)?;
    let method = scope.emit(InstKind::LoadAttr { obj: manager, name })?;
    let called = scope.emit(InstKind::Call {
        callee: method,
        args: args.to_vec(),
    })?;
    if is_async {
        scope.emit(InstKind::Await { awaitable: called })
    } else {
        Ok(called)
    }
}

/// Emits the normal-path `__exit__(None, None, None)` calls for `managers` in
/// reverse acquisition order.
fn lower_normal_exits(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    managers: &[Value],
    is_async: bool,
) -> Result<(), LowerError> {
    for manager in managers.iter().rev().copied() {
        let none_type = scope.emit(InstKind::Const(PyConst::None))?;
        let none = scope.emit(InstKind::Const(PyConst::None))?;
        let tb = scope.emit(InstKind::Const(PyConst::None))?;
        lower_exit_call(
            driver,
            scope,
            manager,
            if is_async { "__aexit__" } else { "__exit__" },
            is_async,
            [none_type, none, tb],
        )?;
    }
    Ok(())
}

/// Whether any block lowered since `scan_start` (or the pending terminator)
/// jumps to `tramp`.
fn referenced(scope: &BodyScope, scan_start: usize, tramp: BlockId) -> bool {
    scope.blocks[scan_start..]
        .iter()
        .map(|block| &block.term)
        .chain(scope.term.as_ref())
        .any(|term| matches!(term, Terminator::Jump(target) if *target == tramp))
}

/// Lowers representative `with`/`async with` ordering into the family-owned IR
/// skeleton: evaluate managers left-to-right, call enter left-to-right, lower the
/// body inside a `PushExcInfo` protected region, then call exits in reverse order.
///
/// Exception path (`try_`'s handler discipline): every failing call and
/// syntactic `raise` in the body routes to a handler block that pops the
/// record and calls `__exit__(type(exc), exc, None)` innermost-first.  A
/// truthy `__exit__` result suppresses the exception — the remaining
/// managers see the normal `(None, None, None)` exits — while an all-falsy
/// chain re-raises the original exception object (PEP 343).
///
/// Departing edges follow `try_`'s `PhaseExits` discipline: `break`/`continue`
/// (under `loop_targets`) and `return` inside the body jump to per-statement
/// trampolines that pop the handler record and run the normal-path `__exit__`
/// calls before resuming the outer edge.  Nested statements chain
/// trampolines, so every `with` runs exactly its own exits, innermost first.
pub(super) fn lower_with_stmt(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtWith,
    loop_targets: Option<control::LoopTargets>,
) -> Result<(), LowerError> {
    let exit_name = if stmt.is_async { "__aexit__" } else { "__exit__" };
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

    let handler_block = scope.alloc_block()?;
    let done_block = scope.alloc_block()?;

    // Trampoline ids are allocated eagerly: blocks only materialize when
    // switched to, so an unreferenced trampoline costs an id, not a block.
    let (break_tramp, continue_tramp, inner_targets) = match loop_targets {
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
    let outer_route = scope.return_route;
    if let Some(tramp) = return_tramp {
        scope.return_route = Some(tramp);
    }
    let scan_start = scope.blocks.len();

    scope.emit(InstKind::PushExcInfo {
        target: handler_block,
        stack_depth: 0,
        kind: 0,
    })?;
    let protected_start = scope.blocks.len();

    for body_stmt in &stmt.body {
        if scope.is_terminated() {
            break;
        }
        driver.lower_stmt_with_loop(scope, body_stmt, inner_targets)?;
    }
    scope.return_route = outer_route;

    // Syntactic raises inside the protected region route to the handler like
    // failing calls (which codegen already routes via the pushed record).
    redirect_raise_terms(scope, protected_start, handler_block);

    let tramps = [break_tramp, continue_tramp, return_tramp];
    let is_tramp = |target: BlockId| tramps.into_iter().flatten().any(|tramp| tramp == target);

    let pending = scope.term.take();
    match pending {
        // The body departs through one of this statement's trampolines; the
        // trampoline owns the pop + `__exit__` duties for that edge.
        Some(Terminator::Jump(target)) if is_tramp(target) => {
            scope.set_term(Terminator::Jump(target))?;
        }
        // A redirected syntactic raise: the handler block owns the pop and
        // the exception-path `__exit__` walk; the departing edge must leave
        // the record pushed so it joins the dynamic NULL edges consistently.
        Some(Terminator::Jump(target)) if target == handler_block => {
            scope.set_term(Terminator::Jump(target))?;
        }
        term => {
            scope.emit(InstKind::PopExcInfo)?;
            lower_normal_exits(driver, scope, &managers, stmt.is_async)?;
            if let Some(term) = term {
                scope.set_term(term)?;
            }
        }
    }
    scope.jump_if_open(done_block)?;

    // Exception path: pop the record, then walk the managers innermost-first
    // with the caught exception until one suppresses it.
    scope.switch_to(handler_block)?;
    scope.emit(InstKind::PopExcInfo)?;
    let exc = scope.emit(InstKind::GetCurrentExc)?;
    let type_name = driver.names.intern("type")?;
    let type_fn = scope.emit(InstKind::LoadBuiltin(type_name))?;
    let exc_type = scope.emit(InstKind::Call {
        callee: type_fn,
        args: vec![exc],
    })?;
    let tb = scope.emit(InstKind::Const(PyConst::None))?;
    for (index, manager) in managers.iter().rev().copied().enumerate() {
        let result = lower_exit_call(driver, scope, manager, exit_name, stmt.is_async, [exc_type, exc, tb])?;
        let cond = scope.emit(InstKind::BoolTest { val: result })?;
        let suppress_block = scope.alloc_block()?;
        let next_block = scope.alloc_block()?;
        scope.set_term(Terminator::CondBranch {
            cond,
            then_: suppress_block,
            else_: next_block,
        })?;
        // Suppressed: the managers acquired before this one still see the
        // normal-path exits, then control resumes after the statement.
        scope.switch_to(suppress_block)?;
        lower_normal_exits(driver, scope, &managers[..managers.len() - 1 - index], stmt.is_async)?;
        scope.set_term(Terminator::Jump(done_block))?;
        scope.switch_to(next_block)?;
    }
    // No manager suppressed: re-raise the original exception object.
    scope.emit(InstKind::Raise {
        exc: Some(exc),
        cause: None,
    })?;
    scope.set_term(Terminator::RaiseTerm)?;

    // break / continue: pop the record, run the exits, resume the outer edge.
    for (tramp, outer) in [
        (break_tramp, loop_targets.map(|targets| targets.break_block)),
        (
            continue_tramp,
            loop_targets.map(|targets| targets.continue_block),
        ),
    ] {
        let (Some(tramp), Some(outer)) = (tramp, outer) else {
            continue;
        };
        if !referenced(scope, scan_start, tramp) {
            continue;
        }
        scope.switch_to(tramp)?;
        scope.emit(InstKind::PopExcInfo)?;
        lower_normal_exits(driver, scope, &managers, stmt.is_async)?;
        scope.set_term(Terminator::Jump(outer))?;
    }

    // return: pop the record, run the exits, then forward to the next route
    // outward (value stays parked in the shared return slot) or actually
    // return.
    if let Some(tramp) = return_tramp {
        if referenced(scope, scan_start, tramp) {
            scope.switch_to(tramp)?;
            scope.emit(InstKind::PopExcInfo)?;
            lower_normal_exits(driver, scope, &managers, stmt.is_async)?;
            match outer_route {
                Some(route) => scope.set_term(Terminator::Jump(route))?,
                None => {
                    let slot = scope.return_slot.ok_or_else(|| {
                        LowerError::internal("routed return without a return slot")
                    })?;
                    let value = scope.emit(InstKind::LoadLocal(slot))?;
                    scope.set_term(Terminator::Return(value))?;
                }
            }
        }
    }

    scope.switch_to(done_block)?;
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
