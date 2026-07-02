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
/// body, then call exits in reverse order.  The exception/finally integration pass
/// wraps this skeleton with handler edges so `__exit__` sees real exception info.
///
/// Departing edges follow `try_`'s `PhaseExits` discipline: `break`/`continue`
/// (under `loop_targets`) and `return` inside the body jump to per-statement
/// trampolines that run the normal-path `__exit__` calls before resuming the
/// outer edge.  Nested statements chain trampolines, so every `with` runs
/// exactly its own exits, innermost first.
pub(super) fn lower_with_stmt(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtWith,
    loop_targets: Option<control::LoopTargets>,
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

    for body_stmt in &stmt.body {
        if scope.is_terminated() {
            break;
        }
        driver.lower_stmt_with_loop(scope, body_stmt, inner_targets)?;
    }
    scope.return_route = outer_route;

    let tramps = [break_tramp, continue_tramp, return_tramp];
    let is_tramp = |target: BlockId| tramps.into_iter().flatten().any(|tramp| tramp == target);

    let pending = scope.term.take();
    match pending {
        // The body departs through one of this statement's trampolines; the
        // trampoline owns the `__exit__` duties for that edge.
        Some(Terminator::Jump(target)) if is_tramp(target) => {
            scope.set_term(Terminator::Jump(target))?;
        }
        Some(Terminator::RaiseTerm) => {
            for manager in managers.iter().rev().copied() {
                let exc = scope.emit(InstKind::GetCurrentExc)?;
                let type_name = driver.names.intern("type")?;
                let type_fn = scope.emit(InstKind::LoadBuiltin(type_name))?;
                let exc_type = scope.emit(InstKind::Call {
                    callee: type_fn,
                    args: vec![exc],
                })?;
                let tb = scope.emit(InstKind::Const(PyConst::None))?;
                lower_exit_call(
                    driver,
                    scope,
                    manager,
                    if stmt.is_async { "__aexit__" } else { "__exit__" },
                    stmt.is_async,
                    [exc_type, exc, tb],
                )?;
            }
        }
        term => {
            lower_normal_exits(driver, scope, &managers, stmt.is_async)?;
            if let Some(term) = term {
                scope.set_term(term)?;
            }
        }
    }

    if !tramps
        .into_iter()
        .flatten()
        .any(|tramp| referenced(scope, scan_start, tramp))
    {
        return Ok(());
    }

    // Park the fall-through edge (if any) while the trampolines are filled.
    let done_block = if scope.term.is_none() {
        let done = scope.alloc_block()?;
        scope.set_term(Terminator::Jump(done))?;
        Some(done)
    } else {
        None
    };

    // break / continue: run the exits, then resume the outer loop edge.
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
        lower_normal_exits(driver, scope, &managers, stmt.is_async)?;
        scope.set_term(Terminator::Jump(outer))?;
    }

    // return: run the exits, then forward to the next route outward (value
    // stays parked in the shared return slot) or actually return.
    if let Some(tramp) = return_tramp {
        if referenced(scope, scan_start, tramp) {
            scope.switch_to(tramp)?;
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

    if let Some(done) = done_block {
        scope.switch_to(done)?;
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
