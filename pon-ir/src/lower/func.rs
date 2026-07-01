use super::*;
use crate::ir::{CellId, LocalId, NameId};

pub(super) fn lower_function_def_stmt(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    def: &StmtFunctionDef,
) -> Result<(), LowerError> {
    if def.type_params.is_some() {
        return unsupported_at("function type parameter", span_function(def));
    }

    let name = def.name.as_str();
    let function_info = scope.next_child_scope(ScopeKind::Function, name)?;
    let name_interned = if binding_needs_name_id(scope, name) {
        Some(driver.names.intern(name)?)
    } else {
        None
    };
    let annotations = lower_function_annotations(driver, scope, &def.parameters, def.returns.as_deref())?;
    let mut value = synth::synthesize_scope_function_with_annotations(
        driver,
        scope,
        function_info,
        &def.parameters,
        annotations,
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

pub(super) fn lower_call(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    call: &ruff_python_ast::ExprCall,
) -> Result<Value, LowerError> {
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


fn lower_function_annotations(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    parameters: &Parameters,
    returns: Option<&Expr>,
) -> Result<Vec<(NameId, Value)>, LowerError> {
    let mut annotations = Vec::new();
    for parameter in parameters.posonlyargs.iter().chain(&parameters.args) {
        push_parameter_with_default_annotation(driver, scope, &mut annotations, parameter)?;
    }
    if let Some(parameter) = parameters.vararg.as_deref() {
        push_parameter_annotation(driver, scope, &mut annotations, parameter)?;
    }
    for parameter in &parameters.kwonlyargs {
        push_parameter_with_default_annotation(driver, scope, &mut annotations, parameter)?;
    }
    if let Some(parameter) = parameters.kwarg.as_deref() {
        push_parameter_annotation(driver, scope, &mut annotations, parameter)?;
    }
    if let Some(annotation) = returns {
        let name = driver.names.intern("return")?;
        let value = driver.lower_expr(scope, annotation)?;
        annotations.push((name, value));
    }
    Ok(annotations)
}

fn push_parameter_with_default_annotation(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    annotations: &mut Vec<(NameId, Value)>,
    parameter: &ruff_python_ast::ParameterWithDefault,
) -> Result<(), LowerError> {
    if let Some(annotation) = parameter.annotation() {
        let name = driver.names.intern(parameter.name().as_str())?;
        let value = driver.lower_expr(scope, annotation)?;
        annotations.push((name, value));
    }
    Ok(())
}

fn push_parameter_annotation(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    annotations: &mut Vec<(NameId, Value)>,
    parameter: &ruff_python_ast::Parameter,
) -> Result<(), LowerError> {
    if let Some(annotation) = parameter.annotation() {
        let name = driver.names.intern(parameter.name().as_str())?;
        let value = driver.lower_expr(scope, annotation)?;
        annotations.push((name, value));
    }
    Ok(())
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
        let name_interned = ensure_name_id(driver, name, &mut name_interned)?;
        scope.emit(InstKind::StoreName(name_interned, value))?;
    } else {
        match scope.name_class(name) {
            Some(NameClass::Cell { cell_slot, .. }) => {
                scope.emit(InstKind::StoreCell(CellId(*cell_slot), value))?;
            }
            Some(NameClass::Free { slot }) => {
                scope.emit(InstKind::StoreCell(CellId(*slot), value))?;
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
    fn phase_a_function_shape_is_preserved() {
        let module = lower_source(
            r#"
def add(a, b):
    return a + b
"#,
        )
        .expect("Phase-A function shape should still lower");
        let main = &module.functions[module.main.0 as usize];
        assert!(matches!(
            main.blocks[0].insts[0].kind,
            InstKind::MakeFunction {
                func_index: 1,
                arity: 2,
                ..
            }
        ));
        assert_eq!(main.blocks[0].insts[2].kind, InstKind::Const(PyConst::None));
    }
}
