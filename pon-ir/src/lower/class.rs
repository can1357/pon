use super::*;

pub(super) fn lower_class_def(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtClassDef,
) -> Result<(), LowerError> {
    let class_info = scope.next_child_scope(
        scope::ScopeKind::Class,
        stmt.name.as_str(),
        Some(scope::span_key(stmt.range)),
    )?;
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

    // PEP 649: claim the deferred class `__annotate__` child (span-less merged
    // namespace scope) up front, but synthesize and store it at class-body END
    // (CPython stores the class annotate function after the body executes;
    // probed via dis on python3.14).
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

    /// Every instruction kind of `func` across all of its blocks, in layout order.
    fn inst_kinds(func: &Function) -> impl Iterator<Item = &InstKind> {
        func.blocks.iter().flat_map(|block| block.insts.iter().map(|inst| &inst.kind))
    }

    /// Entry-block index of the sole `BuildClass` emitted by the module body.
    fn build_class_at(main: &Function) -> usize {
        main.blocks[0]
            .insts
            .iter()
            .position(|inst| matches!(inst.kind, InstKind::BuildClass { .. }))
            .expect("module body should emit a BuildClass")
    }

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
        if let InstKind::BuildClass { bases, bases_seq, keywords, dstar, .. } = &main.blocks[0].insts[6].kind {
            assert_eq!(bases.len(), 1);
            assert!(bases_seq.is_none(), "static bases keep the direct-operand fast path");
            assert_eq!(keywords.len(), 1);
            assert!(dstar.is_none(), "no ** in the header leaves dstar empty");
        }
    }

    #[test]
    fn dstar_keyword_forces_bases_into_sequence() {
        let module = lower_source(
            r#"
kw = {"slot": 1}

class C(int, **kw):
    pass
"#,
        )
        .expect("class with ** keywords should lower");
        let main = &module.functions[module.main.0 as usize];
        let insts = &main.blocks[0].insts;
        let at = build_class_at(main);
        let InstKind::BuildClass { bases, bases_seq, keywords, dstar, .. } = &insts[at].kind else {
            unreachable!();
        };
        assert!(bases.is_empty(), "dynamic path moves every base into bases_seq");
        assert!(bases_seq.is_some(), "bases materialize as one sequence operand");
        assert!(keywords.is_empty(), "header has no statically named keywords");
        assert!(dstar.is_some(), "a single ** mapping passes through as dstar");
        assert!(
            insts[..at].iter().any(|inst| matches!(inst.kind, InstKind::BuildList { .. })),
            "bases list is built before BuildClass"
        );
        assert!(
            insts[..at].iter().any(|inst| matches!(inst.kind, InstKind::ListAppend { .. })),
            "the static base appends into the bases list"
        );
    }

    #[test]
    fn starred_bases_lower_through_list_extend() {
        let module = lower_source(
            r#"
bases = (int,)

class C(*bases):
    pass
"#,
        )
        .expect("class with starred bases should lower");
        let main = &module.functions[module.main.0 as usize];
        let insts = &main.blocks[0].insts;
        let at = build_class_at(main);
        let InstKind::BuildClass { bases, bases_seq, keywords, dstar, .. } = &insts[at].kind else {
            unreachable!();
        };
        assert!(bases.is_empty());
        assert!(bases_seq.is_some(), "starred bases force the sequence operand");
        assert!(keywords.is_empty());
        assert!(dstar.is_none(), "no ** keyword leaves dstar empty");
        assert!(
            insts[..at].iter().any(|inst| matches!(inst.kind, InstKind::ListExtend { .. })),
            "the starred base extends the bases list"
        );
    }

    #[test]
    fn repeated_dstar_folds_into_fresh_map() {
        let module = lower_source(
            r#"
k1 = {"a": 1}
k2 = {"b": 2}

class C(int, x=1, **k1, **k2):
    pass
"#,
        )
        .expect("class with repeated ** keywords should lower");
        let main = &module.functions[module.main.0 as usize];
        let insts = &main.blocks[0].insts;
        let at = build_class_at(main);
        let InstKind::BuildClass { keywords, dstar, .. } = &insts[at].kind else {
            unreachable!();
        };
        assert_eq!(keywords.len(), 1, "x=1 keeps the interned-name fast path");
        let fold_map = dstar.expect("repeated ** folds into a dstar map");
        let merges: Vec<Value> = insts[..at]
            .iter()
            .filter_map(|inst| match inst.kind {
                InstKind::DictMergeUnique { map, .. } => Some(map),
                _ => None,
            })
            .collect();
        assert_eq!(merges.len(), 2, "one duplicate-checking merge per ** mapping");
        assert!(merges.iter().all(|&map| map == fold_map), "every ** merges into the dstar map");
        assert!(
            insts[..at]
                .iter()
                .any(|inst| matches!(&inst.kind, InstKind::BuildMap { pairs } if pairs.is_empty())),
            "the fold target is a fresh empty map"
        );
    }

    #[test]
    fn pep646_starred_annotation_unpacks_in_annotate() {
        let module = lower_source(
            r#"
X = (int,)

def f(*args: *X):
    pass
"#,
        )
        .expect("PEP 646 starred parameter annotation should lower");
        let main_index = module.main.0 as usize;
        let mut unpackers = module.functions.iter().enumerate().filter(|(_, func)| {
            inst_kinds(func).any(|kind| matches!(kind, InstKind::UnpackSeq { n: 1, .. }))
        });
        let (index, annotate) = unpackers
            .next()
            .expect("a synthesized function unpacks the starred annotation");
        assert!(unpackers.next().is_none(), "only __annotate__ unpacks the annotation");
        assert_ne!(index, main_index, "UnpackSeq belongs to synthesized __annotate__, not the module body");
        assert!(
            inst_kinds(annotate).any(|kind| matches!(kind, InstKind::SubscriptGet { .. })),
            "the unpacked annotation is read back by subscript"
        );
    }

    #[test]
    fn plain_annotation_does_not_unpack() {
        let module = lower_source(
            r#"
def g(x: int):
    pass
"#,
        )
        .expect("plain parameter annotation should lower");
        assert!(
            module
                .functions
                .iter()
                .all(|func| inst_kinds(func).all(|kind| !matches!(kind, InstKind::UnpackSeq { .. }))),
            "plain annotations must not route through the PEP 646 unpack arm"
        );
    }
}
