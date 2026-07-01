use super::*;

/// Synthesize a Python function object from an already-discovered child scope.
///
/// Today this is the shared materialization seam for `def` statements and
/// `lambda` expressions: callers resolve the next child [`ScopeInfo`], then
/// this helper computes closure captures, lowers the child body into a fresh
/// [`BodyScope`], appends the IR function, lowers defaults in the enclosing
/// scope, and emits either `MakeFunction` or `MakeFunctionFull`.
///
/// Future lowering passes should reuse the same seam for implicit or synthetic
/// function scopes.  In particular, G2 comprehension desugaring will pass a
/// body callback that emits nested loop IR instead of lowering a statement list,
/// and G3 `__annotate__` synthesis will use it for annotation-scope functions.
pub(crate) fn synthesize_scope_function(
    driver: &mut LoweringDriver,
    enclosing: &mut BodyScope,
    child_info: ScopeInfo,
    parameters: &Parameters,
    lower_body: impl FnOnce(&mut LoweringDriver, &mut BodyScope) -> Result<(), LowerError>,
) -> Result<Value, LowerError> {
    synthesize_scope_function_with_annotations(driver, enclosing, child_info, parameters, Vec::new(), lower_body)
}

pub(crate) fn synthesize_scope_function_with_annotations(
    driver: &mut LoweringDriver,
    enclosing: &mut BodyScope,
    child_info: ScopeInfo,
    parameters: &Parameters,
    annotations: Vec<(NameId, Value)>,
    lower_body: impl FnOnce(&mut LoweringDriver, &mut BodyScope) -> Result<(), LowerError>,
) -> Result<Value, LowerError> {
    let closure = closure_cells(enclosing, &child_info)?;
    let mut name_interned =
        if function_shape_requires_full(parameters, &child_info, &closure, &annotations) {
            None
        } else {
            Some(driver.names.intern(&child_info.name)?)
        };
    let function = lower_function_body(driver, &child_info, lower_body)?;
    let defaults = lower_positional_defaults(driver, enclosing, parameters)?;
    let kwdefaults = lower_keyword_defaults(driver, enclosing, parameters)?;

    if needs_full_function(parameters, &child_info, &defaults, &kwdefaults, &closure, &annotations) {
        enclosing.emit(InstKind::MakeFunctionFull {
            code: function,
            defaults,
            kwdefaults,
            closure,
            annotations,
        })
    } else {
        let name_interned = match name_interned.take() {
            Some(name_interned) => name_interned,
            None => driver.names.intern(&child_info.name)?,
        };
        let arity = positional_parameters(parameters)?.len();
        enclosing.emit(InstKind::MakeFunction {
            func_index: function.0,
            name_interned,
            arity,
        })
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

fn closure_cells(scope: &BodyScope, info: &ScopeInfo) -> Result<Vec<CellId>, LowerError> {
    info.free_vars
        .iter()
        .map(|name| {
            scope.closure_slot(name).ok_or_else(|| {
                LowerError::internal(format!("closure metadata missing parent cell for {name}"))
            })
        })
        .collect()
}

fn function_shape_requires_full(
    parameters: &Parameters,
    info: &ScopeInfo,
    closure: &[CellId],
    annotations: &[(NameId, Value)],
) -> bool {
    positional_parameters_have_defaults(parameters)
        || parameters.vararg.is_some()
        || parameters.kwarg.is_some()
        || !parameters.kwonlyargs.is_empty()
        || !closure.is_empty()
        || !info.cell_vars.is_empty()
        || info.is_generator
        || info.is_async
        || !annotations.is_empty()
}

fn positional_parameters_have_defaults(parameters: &Parameters) -> bool {
    parameters
        .posonlyargs
        .iter()
        .chain(&parameters.args)
        .any(|parameter| parameter.default().is_some())
}

fn needs_full_function(
    parameters: &Parameters,
    info: &ScopeInfo,
    defaults: &[Value],
    kwdefaults: &[(NameId, Value)],
    closure: &[CellId],
    annotations: &[(NameId, Value)],
) -> bool {
    !defaults.is_empty()
        || !kwdefaults.is_empty()
        || parameters.vararg.is_some()
        || parameters.kwarg.is_some()
        || !parameters.kwonlyargs.is_empty()
        || !closure.is_empty()
        || !info.cell_vars.is_empty()
        || info.is_generator
        || info.is_async
        || !annotations.is_empty()
}

fn positional_parameters(parameters: &Parameters) -> Result<Vec<String>, LowerError> {
    let mut params = Vec::with_capacity(parameters.posonlyargs.len() + parameters.args.len());
    for parameter in parameters.posonlyargs.iter().chain(&parameters.args) {
        params.push(parameter.name().as_str().to_owned());
    }
    Ok(params)
}
