use super::*;

pub(super) fn lower_class_def(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtClassDef,
) -> Result<(), LowerError> {
    let class_info = scope
        .info
        .child(scope::ScopeKind::Class, stmt.name.as_str())
        .cloned()
        .ok_or_else(|| LowerError::internal(format!("missing class scope for {}", stmt.name)))?;
    if !class_info.cell_vars.is_empty() {
        return unsupported_at("class body cell variables", span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()));
    }
    // Free variables of the class scope are enclosing-function locals that
    // the class body (or a method/comprehension nested in it) closes over.
    // Resolve them against the enclosing scope NOW and attach them as the
    // body function's closure via `BuildClass`.
    let closure = synth::closure_cells(scope, &class_info)?;
    if class_info.is_generator || class_info.is_async {
        return unsupported_at("generator or async class body", span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()));
    }

    let body_id = driver.reserve_function(stmt.name.as_str())?;
    let mut body = BodyScope::new(&class_info);

    // PEP 649: claim the deferred class `__annotate__` child (children[0])
    // BEFORE lowering nested statements so annotated `def`s inside the body
    // claim their own `__annotate__` children, but synthesize and store it at
    // class-body END (CPython stores the class annotate function after the
    // body executes; probed via dis on python3.14).
    let namespace_annotate = synth::claim_namespace_annotate(&mut body, &stmt.body)?;
    for nested in &stmt.body {
        driver.lower_stmt(&mut body, nested)?;
    }
    if let Some((annotate_info, entries)) = namespace_annotate {
        if !body.is_terminated() {
            let annotate = synth::synthesize_annotate_scope(driver, &mut body, annotate_info, &entries)?;
            let annotate_name = driver.names.intern(scope::ANNOTATE_SCOPE_NAME)?;
            body.emit(InstKind::StoreName(annotate_name, annotate))?;
        }
    }
    let body_fn = body.finish()?;
    driver.replace_reserved_function(body_id, body_fn)?;

    let name = driver.names.intern(stmt.name.as_str())?;
    let mut bases = Vec::new();
    let mut bases_seq = None;
    let mut keywords = Vec::new();
    let mut dstar = None;
    if let Some(arguments) = stmt.arguments.as_deref() {
        let has_starred_base = arguments.args.iter().any(|arg| matches!(arg, Expr::Starred(_)));
        let has_dstar = arguments.keywords.iter().any(|keyword| keyword.arg.is_none());
        if has_starred_base || has_dstar {
            // Dynamic construction path: materialize the bases into a list
            // exactly like `lower_call` materializes `*args` (`class C(*bs)`
            // iterates like a call), so codegen can hand the runtime one
            // sequence object regardless of star placement.
            let list = scope.emit(InstKind::BuildList { elts: Vec::new() })?;
            for arg in &arguments.args {
                if let Expr::Starred(starred) = arg {
                    let iter = driver.lower_expr(scope, &starred.value)?;
                    scope.emit(InstKind::ListExtend { list, iter })?;
                } else {
                    let item = driver.lower_expr(scope, arg)?;
                    scope.emit(InstKind::ListAppend { list, item })?;
                }
            }
            bases_seq = Some(list);
        } else {
            bases.reserve(arguments.args.len());
            for arg in &arguments.args {
                bases.push(driver.lower_expr(scope, arg)?);
            }
        }
        // Keyword `**` materialization mirrors `lower_call`: a single `**`
        // passes its mapping through, several fold left-to-right into a
        // fresh map with duplicate detection.  Statically named keywords
        // keep their interned-name fast path either way.
        let has_multiple_dstar = arguments
            .keywords
            .iter()
            .filter(|keyword| keyword.arg.is_none())
            .nth(1)
            .is_some();
        if has_multiple_dstar {
            dstar = Some(scope.emit(InstKind::BuildMap { pairs: Vec::new() })?);
        }
        keywords.reserve(arguments.keywords.len());
        for keyword in &arguments.keywords {
            let value = driver.lower_expr(scope, &keyword.value)?;
            if let Some(arg_name) = keyword.arg.as_ref() {
                let key = driver.names.intern(arg_name.as_str())?;
                keywords.push((key, value));
            } else if let Some(map) = dstar {
                scope.emit(InstKind::DictMergeUnique { map, other: value })?;
            } else {
                dstar = Some(value);
            }
        }
    }

    let mut decorators = Vec::with_capacity(stmt.decorator_list.len());
    for decorator in &stmt.decorator_list {
        decorators.push(driver.lower_expr(scope, &decorator.expression)?);
    }

    let _build_class = scope.emit(InstKind::LoadBuildClass)?;
    let class_value = scope.emit(InstKind::BuildClass {
        body: body_id,
        name,
        bases,
        bases_seq,
        keywords,
        dstar,
        decorators,
        closure,
    })?;

    if scope.is_global_name(stmt.name.as_str()) {
        scope.emit(InstKind::StoreGlobal(name, class_value))?;
    } else if let Some(slot) = scope.local_slot(stmt.name.as_str()) {
        scope.emit(InstKind::StoreLocal(slot, class_value))?;
    } else {
        scope.emit(InstKind::StoreName(name, class_value))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowers_class_definition_to_build_class() {
        let module = lower_source(
            r#"
class Base:
    pass

class Child(Base, metaclass=Base):
    answer = 42
"#,
        )
        .expect("class definitions should lower");
        let main = &module.functions[module.main.0 as usize];
        assert!(matches!(main.blocks[0].insts[0].kind, InstKind::LoadBuildClass));
        assert!(matches!(main.blocks[0].insts[1].kind, InstKind::BuildClass { .. }));
        assert!(matches!(main.blocks[0].insts[2].kind, InstKind::StoreGlobal(_, _)));
        assert!(matches!(main.blocks[0].insts[5].kind, InstKind::LoadBuildClass));
        assert!(matches!(main.blocks[0].insts[6].kind, InstKind::BuildClass { .. }));
        if let InstKind::BuildClass { bases, keywords, .. } = &main.blocks[0].insts[6].kind {
            assert_eq!(bases.len(), 1);
            assert_eq!(keywords.len(), 1);
        }
    }
}
