use super::*;
use crate::ir::CmpOp;

/// Synthesize a Python function object from an already-discovered child scope.
///
/// Today this is the shared materialization seam for `def` statements and
/// `lambda` expressions: callers resolve the next child [`ScopeInfo`], then
/// this helper computes closure captures, lowers the child body into a fresh
/// [`BodyScope`], appends the IR function, lowers defaults in the enclosing
/// scope, and emits either `MakeFunction` or `MakeFunctionFull`.
///
/// Comprehension desugaring (G2) reuses the same seam with a body callback
/// that emits nested loop IR, and the PEP 649 `__annotate__` seam below feeds
/// it a lazily-evaluated annotate function via [`synthesize_annotate_scope`].
pub(crate) fn synthesize_scope_function(
    driver: &mut LoweringDriver,
    enclosing: &mut BodyScope,
    child_info: ScopeInfo,
    parameters: &Parameters,
    lower_body: impl FnOnce(&mut LoweringDriver, &mut BodyScope) -> Result<(), LowerError>,
) -> Result<Value, LowerError> {
    synthesize_scope_function_with_annotate(driver, enclosing, child_info, parameters, None, lower_body)
}

/// [`synthesize_scope_function`] with an optional pre-synthesized PEP 649
/// `__annotate__` function attached via `FunctionSetAnnotate` after the
/// function object materializes (before decorators run).
pub(crate) fn synthesize_scope_function_with_annotate(
    driver: &mut LoweringDriver,
    enclosing: &mut BodyScope,
    child_info: ScopeInfo,
    parameters: &Parameters,
    annotate: Option<Value>,
    lower_body: impl FnOnce(&mut LoweringDriver, &mut BodyScope) -> Result<(), LowerError>,
) -> Result<Value, LowerError> {
    let closure = closure_cells(enclosing, &child_info)?;
    // Closure-free non-generator comprehension scopes keep the record-less
    // Phase-A shape: they are synthesized callees, invoked exactly once with
    // the iterator as the sole positional argument, so they never need
    // keyword metadata.  Every OTHER function object — def/lambda in any
    // position, plus comprehensions with captures or generator/async bodies —
    // materializes through `MakeFunctionFull` so the runtime registers a
    // FunctionRecord: keyword binding (`A(x=1)` reaching `__init__`,
    // namedtuple's eval'd `__new__`) is impossible without the
    // parameter-name table, regardless of how simple the signature is.
    let phase_a_comprehension = child_info.kind == ScopeKind::Comprehension
        && closure.is_empty()
        && child_info.cell_vars.is_empty()
        && !child_info.is_generator
        && !child_info.is_async;
    let name_interned = if phase_a_comprehension {
        Some(driver.names.intern(&child_info.name)?)
    } else {
        None
    };
    let function = lower_function_body(driver, &child_info, lower_body)?;
    let defaults = lower_positional_defaults(driver, enclosing, parameters)?;
    let kwdefaults = lower_keyword_defaults(driver, enclosing, parameters)?;

    let value = if let Some(name_interned) = name_interned {
        let arity = parameters.posonlyargs.len() + parameters.args.len();
        enclosing.emit(InstKind::MakeFunction {
            func_index: function.0,
            name_interned,
            arity,
        })?
    } else {
        enclosing.emit(InstKind::MakeFunctionFull {
            code: function,
            defaults,
            kwdefaults,
            closure,
            // PEP 649: annotations are never eagerly evaluated; the field is
            // frozen-ABI legacy and always empty (see `InstKind` docs).
            annotations: Vec::new(),
        })?
    };

    if let Some(annotate) = annotate {
        enclosing.emit(InstKind::FunctionSetAnnotate {
            function: value,
            annotate,
        })?;
    }
    Ok(value)
}

/// Synthesize a PEP 649 `__annotate__(format)` function from its claimed
/// scope-analysis child.
///
/// Body shape (pinned against CPython 3.14 probes):
/// 1. `if format > 2: raise NotImplementedError` (formats 0/-1/1/2 all
///    VALUE-evaluate; only formats above 2 are rejected),
/// 2. `MakeTypeVar` prologue binding every visible PEP 695 type parameter,
/// 3. evaluate each annotation expression and return the `BuildMap` dict.
pub(crate) fn synthesize_annotate_scope(
    driver: &mut LoweringDriver,
    enclosing: &mut BodyScope,
    child_info: ScopeInfo,
    entries: &[(String, &Expr)],
) -> Result<Value, LowerError> {
    // PEP 563 (`from __future__ import annotations`): the annotate function
    // returns annotation source text for every format, so it never evaluates
    // expressions and drops the `if format > 2: raise NotImplementedError`
    // guard — `annotationlib` reads the string dict directly.
    let format_guard = !driver.future_annotations;
    synthesize_lazy_scope(driver, enclosing, child_info, format_guard, |driver, body| {
        emit_annotations_map(driver, body, entries)
    })
}

/// Synthesize a PEP 695 zero-argument type-alias value thunk from its claimed
/// `<type_alias>` child: `MakeTypeVar` prologue, then return the lazily
/// evaluated alias value expression.
pub(crate) fn synthesize_type_alias_thunk(
    driver: &mut LoweringDriver,
    enclosing: &mut BodyScope,
    child_info: ScopeInfo,
    value: &Expr,
) -> Result<Value, LowerError> {
    synthesize_lazy_scope(driver, enclosing, child_info, false, |driver, body| {
        driver.lower_expr(body, value)
    })
}

/// Collect module/class-level variable annotations from `body` and claim the
/// deferred `__annotate__` child that scope analysis merged for them.
///
/// The merged namespace child is the only `__annotate__` scope with no
/// defining source construct, so it is claimed with a `None` span key.
///
/// Returns `None` when `body` carries no name-target `AnnAssign` (scope
/// analysis created no child in that case).
pub(crate) fn claim_namespace_annotate<'a>(
    scope: &mut BodyScope,
    body: &'a [Stmt],
) -> Result<Option<(ScopeInfo, Vec<(String, &'a Expr)>)>, LowerError> {
    let mut entries = Vec::new();
    collect_namespace_annotations(body, &mut entries);
    if entries.is_empty() {
        return Ok(None);
    }
    let child_info = scope.next_child_scope(ScopeKind::Function, scope::ANNOTATE_SCOPE_NAME, None)?;
    Ok(Some((child_info, entries)))
}

/// Shared PEP 649/695 lazy-scope materialization: lower the synthesized body
/// (optional format guard, `MakeTypeVar` prologue, caller-provided result
/// expression) and emit the function object in the enclosing scope.
fn synthesize_lazy_scope(
    driver: &mut LoweringDriver,
    enclosing: &mut BodyScope,
    child_info: ScopeInfo,
    format_guard: bool,
    lower_result: impl FnOnce(&mut LoweringDriver, &mut BodyScope) -> Result<Value, LowerError>,
) -> Result<Value, LowerError> {
    let closure = closure_cells(enclosing, &child_info)?;
    let type_params = child_info.type_params.clone();
    let name_interned = driver.names.intern(&child_info.name)?;
    let arity = child_info.parameters.arity();

    let function = lower_function_body(driver, &child_info, |driver, body| {
        if format_guard {
            emit_format_guard(driver, body)?;
        }
        emit_type_param_prologue(driver, body, &type_params)?;
        let result = lower_result(driver, body)?;
        body.set_term(Terminator::Return(result))
    })?;

    if closure.is_empty() && child_info.cell_vars.is_empty() {
        enclosing.emit(InstKind::MakeFunction {
            func_index: function.0,
            name_interned,
            arity,
        })
    } else {
        enclosing.emit(InstKind::MakeFunctionFull {
            code: function,
            defaults: Vec::new(),
            kwdefaults: Vec::new(),
            closure,
            annotations: Vec::new(),
        })
    }
}

/// `if format > 2: raise NotImplementedError` prologue for `__annotate__`.
fn emit_format_guard(driver: &mut LoweringDriver, body: &mut BodyScope) -> Result<(), LowerError> {
    let format_slot = body
        .local_slot("format")
        .ok_or_else(|| LowerError::internal("annotate scope is missing its format parameter"))?;
    let format = body.emit(InstKind::LoadLocal(format_slot))?;
    let two = body.emit(InstKind::Const(PyConst::Int(2)))?;
    let gt = body.emit(InstKind::Compare {
        op: CmpOp::Gt,
        lhs: format,
        rhs: two,
    })?;
    let cond = body.emit(InstKind::BoolTest { val: gt })?;
    let raise_block = body.alloc_block()?;
    let cont_block = body.alloc_block()?;
    body.set_term(Terminator::CondBranch {
        cond,
        then_: raise_block,
        else_: cont_block,
    })?;

    body.switch_to(raise_block)?;
    let exc_name = driver.names.intern("NotImplementedError")?;
    let exc = body.emit(InstKind::LoadGlobal(exc_name))?;
    body.emit(InstKind::Raise {
        exc: Some(exc),
        cause: None,
    })?;
    body.set_term(Terminator::RaiseTerm)?;

    body.switch_to(cont_block)?;
    Ok(())
}

/// Bind every visible PEP 695 type parameter as a fresh `MakeTypeVar` local.
/// `BodyScope::emit` rewrites the store to `StoreCell` for captured params.
fn emit_type_param_prologue(
    driver: &mut LoweringDriver,
    body: &mut BodyScope,
    type_params: &[String],
) -> Result<(), LowerError> {
    for param in type_params {
        let slot = body.local_slot(param).ok_or_else(|| {
            LowerError::internal(format!(
                "type parameter {param} has no local slot in its annotation scope"
            ))
        })?;
        let name = driver.names.intern(param)?;
        let typevar = body.emit(InstKind::MakeTypeVar { name })?;
        body.emit(InstKind::StoreLocal(slot, typevar))?;
    }
    Ok(())
}

/// Build the annotation dict from `entries` in source order.
fn emit_annotations_map(
    driver: &mut LoweringDriver,
    body: &mut BodyScope,
    entries: &[(String, &Expr)],
) -> Result<Value, LowerError> {
    let mut pairs = Vec::with_capacity(entries.len());
    for (key, annotation) in entries {
        let key = body.emit(InstKind::Const(PyConst::Str(key.clone())))?;
        let value = emit_annotation_value(driver, body, annotation)?;
        pairs.push((key, value));
    }
    body.emit(InstKind::BuildMap { pairs })
}

/// Lower one annotation entry's value.  Under PEP 563 this is the annotation's
/// verbatim source text as a string constant (never evaluated); otherwise the
/// evaluated expression, where PEP 646 `*args: *Ts` unpacks to its single
/// `Unpack[...]` item and every other annotation lowers as a plain expression.
fn emit_annotation_value(
    driver: &mut LoweringDriver,
    body: &mut BodyScope,
    annotation: &Expr,
) -> Result<Value, LowerError> {
    if driver.future_annotations {
        if let Some(text) = driver.expr_source(annotation) {
            let text = text.to_owned();
            return body.emit(InstKind::Const(PyConst::Str(text)));
        }
    }
    if let Expr::Starred(starred) = annotation {
        let seq = driver.lower_expr(body, &starred.value)?;
        let unpacked = body.emit(InstKind::UnpackSeq { val: seq, n: 1 })?;
        let index = body.emit(InstKind::Const(PyConst::Int(0)))?;
        body.emit(InstKind::SubscriptGet { obj: unpacked, index })
    } else {
        driver.lower_expr(body, annotation)
    }
}

/// Collect name-target `AnnAssign` annotations in source order, recursing
/// exactly where scope analysis (`scan_stmt`) recurses with the SAME scope:
/// control-flow statement bodies, but never nested `def`/`class` scopes.
/// Any divergence from `scan_stmt` here breaks the annotate-child claim.
fn collect_namespace_annotations<'a>(body: &'a [Stmt], entries: &mut Vec<(String, &'a Expr)>) {
    for stmt in body {
        match stmt {
            Stmt::AnnAssign(assign) => {
                if let Expr::Name(name) = assign.target.as_ref() {
                    entries.push((name.id.as_str().to_owned(), assign.annotation.as_ref()));
                }
            }
            Stmt::For(for_stmt) => {
                collect_namespace_annotations(&for_stmt.body, entries);
                collect_namespace_annotations(&for_stmt.orelse, entries);
            }
            Stmt::While(while_stmt) => {
                collect_namespace_annotations(&while_stmt.body, entries);
                collect_namespace_annotations(&while_stmt.orelse, entries);
            }
            Stmt::If(if_stmt) => {
                collect_namespace_annotations(&if_stmt.body, entries);
                for clause in &if_stmt.elif_else_clauses {
                    collect_namespace_annotations(&clause.body, entries);
                }
            }
            Stmt::With(with_stmt) => {
                collect_namespace_annotations(&with_stmt.body, entries);
            }
            Stmt::Match(match_stmt) => {
                for case in &match_stmt.cases {
                    collect_namespace_annotations(&case.body, entries);
                }
            }
            Stmt::Try(try_stmt) => {
                collect_namespace_annotations(&try_stmt.body, entries);
                for handler in &try_stmt.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
                    collect_namespace_annotations(&handler.body, entries);
                }
                collect_namespace_annotations(&try_stmt.orelse, entries);
                collect_namespace_annotations(&try_stmt.finalbody, entries);
            }
            _ => {}
        }
    }
}

fn lower_function_body(
    driver: &mut LoweringDriver,
    info: &ScopeInfo,
    lower_body: impl FnOnce(&mut LoweringDriver, &mut BodyScope) -> Result<(), LowerError>,
) -> Result<FunctionId, LowerError> {
    let mut body = BodyScope::new(info);
    lower_body(driver, &mut body)?;
    let function = body.finish()?;
    driver.append_function(function)
}

fn lower_positional_defaults(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    parameters: &Parameters,
) -> Result<Vec<Value>, LowerError> {
    let mut defaults = Vec::new();
    for parameter in parameters.posonlyargs.iter().chain(&parameters.args) {
        if let Some(default) = parameter.default() {
            defaults.push(driver.lower_expr(scope, default)?);
        }
    }
    Ok(defaults)
}

fn lower_keyword_defaults(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    parameters: &Parameters,
) -> Result<Vec<(NameId, Value)>, LowerError> {
    let mut defaults = Vec::new();
    for parameter in &parameters.kwonlyargs {
        if let Some(default) = parameter.default() {
            defaults.push((
                driver.names.intern(parameter.name().as_str())?,
                driver.lower_expr(scope, default)?,
            ));
        }
    }
    Ok(defaults)
}

pub(super) fn closure_cells(scope: &BodyScope, info: &ScopeInfo) -> Result<Vec<CellId>, LowerError> {
    info.free_vars
        .iter()
        .map(|name| {
            scope.closure_slot(name).ok_or_else(|| {
                LowerError::internal(format!("closure metadata missing parent cell for {name}"))
            })
        })
        .collect()
}
