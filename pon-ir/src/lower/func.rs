use super::*;
use crate::ir::{CellId, LocalId, NameId};

pub(super) fn lower_function_def_stmt(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    def: &StmtFunctionDef,
) -> Result<(), LowerError> {
    let name = def.name.as_str();
    let name_interned = if binding_needs_name_id(scope, name) {
        Some(driver.names.intern(name)?)
    } else {
        None
    };

    // PEP 649: scope analysis pushes the `__annotate__` child immediately
    // BEFORE the def's own Function child, so the claim order here is frozen:
    // annotate first, def second.
    let annotate = if scope::function_def_has_annotations(def) {
        let annotate_info = scope.next_child_scope(ScopeKind::Function, scope::ANNOTATE_SCOPE_NAME)?;
        let entries = annotation_entries(&def.parameters, def.returns.as_deref());
        Some(synth::synthesize_annotate_scope(driver, scope, annotate_info, &entries)?)
    } else {
        None
    };

    let function_info = scope.next_child_scope(ScopeKind::Function, name)?;
    let mut value = synth::synthesize_scope_function_with_annotate(
        driver,
        scope,
        function_info,
        &def.parameters,
        annotate,
        |driver, body| {
            for stmt in &def.body {
                driver.lower_stmt(body, stmt)?;
            }
            Ok(())
        },
    )?;

    for decorator in def.decorator_list.iter().rev() {
        let callee = driver.lower_expr(scope, &decorator.expression)?;
        value = scope.emit(InstKind::Call {
            callee,
            args: vec![value],
        })?;
    }

    store_function_value(driver, scope, name, name_interned, value)
}

/// Annotation entries in CPython `__annotate__` evaluation order: positional
/// parameters, `*args`, keyword-only, `**kwargs` (annotated only), `return`.
fn annotation_entries<'a>(parameters: &'a Parameters, returns: Option<&'a Expr>) -> Vec<(String, &'a Expr)> {
    let mut entries = Vec::new();
    for parameter in parameters.posonlyargs.iter().chain(&parameters.args) {
        if let Some(annotation) = parameter.annotation() {
            entries.push((parameter.name().as_str().to_owned(), annotation));
        }
    }
    if let Some(vararg) = parameters.vararg.as_deref() {
        if let Some(annotation) = vararg.annotation() {
            entries.push((vararg.name().as_str().to_owned(), annotation));
        }
    }
    for parameter in &parameters.kwonlyargs {
        if let Some(annotation) = parameter.annotation() {
            entries.push((parameter.name().as_str().to_owned(), annotation));
        }
    }
    if let Some(kwarg) = parameters.kwarg.as_deref() {
        if let Some(annotation) = kwarg.annotation() {
            entries.push((kwarg.name().as_str().to_owned(), annotation));
        }
    }
    if let Some(annotation) = returns {
        entries.push(("return".to_owned(), annotation));
    }
    entries
}

pub(super) fn lower_call(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    call: &ruff_python_ast::ExprCall,
) -> Result<Value, LowerError> {
    if is_builtin_locals_snapshot(scope, call) {
        return lower_locals_snapshot(scope);
    }

    let callee = driver.lower_expr(scope, &call.func)?;
    let has_starred = call.arguments.args.iter().any(|arg| matches!(arg, Expr::Starred(_)));
    let mut args = Vec::with_capacity(call.arguments.args.len());
    let star = if has_starred {
        let list = scope.emit(InstKind::BuildList { elts: Vec::new() })?;
        for arg in &call.arguments.args {
            if let Expr::Starred(starred) = arg {
                let iter = driver.lower_expr(scope, &starred.value)?;
                scope.emit(InstKind::ListExtend { list, iter })?;
            } else {
                let item = driver.lower_expr(scope, arg)?;
                scope.emit(InstKind::ListAppend { list, item })?;
            }
        }
        Some(list)
    } else {
        for arg in &call.arguments.args {
            args.push(driver.lower_expr(scope, arg)?);
        }
        None
    };

    let has_multiple_dstar = call
        .arguments
        .keywords
        .iter()
        .filter(|keyword| keyword.arg.is_none())
        .nth(1)
        .is_some();
    let mut kwargs = Vec::with_capacity(call.arguments.keywords.len());
    let mut dstar = if has_multiple_dstar {
        Some(scope.emit(InstKind::BuildMap { pairs: Vec::new() })?)
    } else {
        None
    };
    for keyword in &call.arguments.keywords {
        let value = driver.lower_expr(scope, &keyword.value)?;
        if let Some(name) = keyword.arg.as_ref() {
            kwargs.push((driver.names.intern(name.as_str())?, value));
        } else if let Some(map) = dstar {
            scope.emit(InstKind::DictMergeUnique { map, other: value })?;
        } else {
            dstar = Some(value);
        }
    }

    if star.is_none() && kwargs.is_empty() && dstar.is_none() {
        scope.emit(InstKind::Call { callee, args })
    } else {
        scope.emit(InstKind::CallEx {
            callee,
            args,
            star,
            kwargs,
            dstar,
        })
    }
}

fn is_builtin_locals_snapshot(scope: &BodyScope, call: &ruff_python_ast::ExprCall) -> bool {
    if !scope.is_function_like()
        || !call.arguments.args.is_empty()
        || !call.arguments.keywords.is_empty()
    {
        return false;
    }
    let Expr::Name(callee) = call.func.as_ref() else {
        return false;
    };
    callee.id.as_str() == "locals" && matches!(scope.name_class("locals"), Some(NameClass::Builtin))
}

fn lower_locals_snapshot(scope: &mut BodyScope) -> Result<Value, LowerError> {
    let items = scope.locals_snapshot_items();
    let mut pairs = Vec::with_capacity(items.len());
    for (name, slot) in items {
        let key = scope.emit(InstKind::Const(PyConst::Str(name)))?;
        let value = scope.emit(InstKind::LoadLocal(slot))?;
        pairs.push((key, value));
    }
    scope.emit(InstKind::BuildMap { pairs })
}

pub(super) fn lower_lambda(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    lambda: &ruff_python_ast::ExprLambda,
) -> Result<Value, LowerError> {
    let parameters = lambda.parameters.as_deref().cloned().unwrap_or_default();
    reject_parameter_annotations(&parameters)?;
    let lambda_info = scope.next_child_scope(ScopeKind::Function, "<lambda>")?;
    synth::synthesize_scope_function(driver, scope, lambda_info, &parameters, |driver, body| {
        let value = driver.lower_expr(body, &lambda.body)?;
        body.set_term(Terminator::Return(value))
    })
}


fn store_function_value(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    name: &str,
    mut name_interned: Option<NameId>,
    value: Value,
) -> Result<(), LowerError> {
    if scope.is_global_name(name) {
        let name_interned = ensure_name_id(driver, name, &mut name_interned)?;
        scope.emit(InstKind::StoreGlobal(name_interned, value))?;
    } else if scope.is_class() {
        if let Some(cell) = scope.class_deref_cell(name) {
            scope.emit(InstKind::StoreCell(cell, value))?;
        } else {
            let name_interned = ensure_name_id(driver, name, &mut name_interned)?;
            scope.emit(InstKind::StoreName(name_interned, value))?;
        }
    } else {
        match scope.name_class(name) {
            Some(NameClass::Cell { cell_slot, .. }) => {
                scope.emit(InstKind::StoreCell(CellId(*cell_slot), value))?;
            }
            Some(NameClass::Free { slot }) => {
                let cell = scope.free_cell(*slot);
                scope.emit(InstKind::StoreCell(cell, value))?;
            }
            Some(NameClass::Local { slot }) => {
                scope.emit(InstKind::StoreLocal(LocalId(*slot), value))?;
            }
            Some(NameClass::Builtin) | Some(NameClass::Global { .. }) | None => {
                let name_interned = ensure_name_id(driver, name, &mut name_interned)?;
                scope.emit(InstKind::StoreName(name_interned, value))?;
            }
        }
    }
    Ok(())
}


fn binding_needs_name_id(scope: &BodyScope, name: &str) -> bool {
    scope.is_global_name(name)
        || matches!(
            scope.name_class(name),
            Some(NameClass::Builtin) | Some(NameClass::Global { .. }) | None
        )
}

fn ensure_name_id(
    driver: &mut LoweringDriver,
    name: &str,
    name_interned: &mut Option<NameId>,
) -> Result<NameId, LowerError> {
    if let Some(name_interned) = *name_interned {
        Ok(name_interned)
    } else {
        let interned = driver.names.intern(name)?;
        *name_interned = Some(interned);
        Ok(interned)
    }
}


fn reject_parameter_annotations(parameters: &Parameters) -> Result<(), LowerError> {
    for parameter in parameters.iter_non_variadic_params() {
        if parameter.annotation().is_some() {
            return Err(LowerError::unsupported("parameter annotation"));
        }
    }
    if parameters
        .vararg
        .as_ref()
        .and_then(|parameter| parameter.annotation())
        .is_some()
    {
        return Err(LowerError::unsupported("parameter annotation"));
    }
    if parameters
        .kwarg
        .as_ref()
        .and_then(|parameter| parameter.annotation())
        .is_some()
    {
        return Err(LowerError::unsupported("parameter annotation"));
    }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{BinOp, PyConst};
    use crate::lower_source;

    #[test]
    fn lowers_defaults_kwonly_and_decorators_bottom_up() {
        let module = lower_source(
            r#"
def deco(f):
    return f

@deco
def target(a=1, *, b=2):
    return a + b
"#,
        )
        .expect("decorated function with defaults should lower");

        let main = &module.functions[module.main.0 as usize];
        assert!(main
            .blocks[0]
            .insts
            .iter()
            .any(|inst| matches!(inst.kind, InstKind::MakeFunctionFull { ref defaults, ref kwdefaults, .. }
                if defaults.len() == 1 && kwdefaults.len() == 1)));
        let decorator_call = main
            .blocks[0]
            .insts
            .iter()
            .find(|inst| matches!(inst.kind, InstKind::Call { ref args, .. } if args.len() == 1))
            .expect("decorator application call should be emitted");
        assert!(matches!(decorator_call.kind, InstKind::Call { .. }));

        let target = module
            .functions
            .iter()
            .find(|function| function.name == "target")
            .expect("target function should be lowered");
        assert_eq!(target.arity, 1);
        assert!(matches!(
            target.blocks[0].insts[2].kind,
            InstKind::BinaryOp {
                op: BinOp::Add,
                ..
            }
        ));
    }

    #[test]
    fn lowers_keyword_and_double_star_call_to_call_ex() {
        let module = lower_source(
            r#"
def f(a):
    return a

f(1, b=2, **kw)
"#,
        )
        .expect("CallEx shape should lower");
        let main = &module.functions[module.main.0 as usize];
        assert!(main.blocks[0].insts.iter().any(|inst| matches!(
            inst.kind,
            InstKind::CallEx {
                ref args,
                ref kwargs,
                dstar: Some(_),
                ..
            } if args.len() == 1 && kwargs.len() == 1
        )));
    }

    #[test]
    fn lowers_complex_number_literals() {
        let module = lower_source(
            r#"
print(4j)
print(3+4j)
"#,
        )
        .expect("complex literals should lower");
        let main = &module.functions[module.main.0 as usize];
        assert!(main.blocks[0].insts.iter().any(|inst| matches!(
            inst.kind,
            InstKind::Const(PyConst::Complex { real, imag }) if real == 0.0 && imag == 4.0
        )));
    }

    #[test]
    fn lowers_multiple_starred_call_arguments_into_one_star_carrier() {
        let module = lower_source(
            r#"
f(*a, 3, *(4, 5))
"#,
        )
        .expect("multiple starred call arguments should lower");
        let main = &module.functions[module.main.0 as usize];
        assert!(main
            .blocks[0]
            .insts
            .iter()
            .any(|inst| matches!(inst.kind, InstKind::BuildList { ref elts } if elts.is_empty())));
        assert_eq!(
            main.blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst.kind, InstKind::ListExtend { .. }))
                .count(),
            2
        );
        assert!(main.blocks[0].insts.iter().any(|inst| matches!(
            inst.kind,
            InstKind::CallEx {
                ref args,
                star: Some(_),
                ..
            } if args.is_empty()
        )));
    }

    #[test]
    fn lowers_multiple_double_star_call_arguments_into_unique_merge_carrier() {
        let module = lower_source(
            r#"
f(a=1, **b, **c)
"#,
        )
        .expect("multiple double-star call arguments should lower");
        let main = &module.functions[module.main.0 as usize];
        assert_eq!(
            main.blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst.kind, InstKind::DictMergeUnique { .. }))
                .count(),
            2
        );
        assert!(main.blocks[0].insts.iter().any(|inst| matches!(
            inst.kind,
            InstKind::CallEx {
                ref kwargs,
                dstar: Some(_),
                ..
            } if kwargs.len() == 1
        )));
    }

    #[test]
    fn closure_function_uses_full_function_shape() {
        let module = lower_source(
            r#"
def outer(x):
    def inner(y):
        return x + y
    return inner
"#,
        )
        .expect("closure shape should lower to full function construction");
        let outer = module
            .functions
            .iter()
            .find(|function| function.name == "outer")
            .expect("outer function should exist");
        assert!(outer.blocks[0].insts.iter().any(|inst| matches!(
            inst.kind,
            InstKind::MakeFunctionFull { ref closure, .. } if !closure.is_empty()
        )));
    }

    #[test]
    fn simple_function_uses_full_shape_for_keyword_metadata() {
        // A plain positional signature still needs a Phase-B FunctionRecord:
        // `add(a=1, b=2)` binds keywords through the parameter-name table,
        // which only `MakeFunctionFull` registers.
        let module = lower_source(
            r#"
def add(a, b):
    return a + b
"#,
        )
        .expect("simple function should lower");
        let main = &module.functions[module.main.0 as usize];
        assert!(matches!(
            main.blocks[0].insts[0].kind,
            InstKind::MakeFunctionFull {
                code: FunctionId(1),
                ref defaults,
                ref kwdefaults,
                ref closure,
                ..
            } if defaults.is_empty() && kwdefaults.is_empty() && closure.is_empty()
        ));
        assert_eq!(main.blocks[0].insts[2].kind, InstKind::Const(PyConst::None));
    }
}
