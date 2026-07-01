//! Ruff-AST annotation seeding for the Phase-D typed AoT path.
//!
//! The baseline lowerer intentionally keeps annotations out of tier-0 semantics.
//! This module reads the same source AST out-of-band, turns representative Python
//! annotations into the IR [`Type`] lattice, and provides an opt-only AST scrubber
//! so annotated code can still lower through the existing boxed frontend.

use pon_ir::lower::{LowerError, scope};
use pon_ir::{LocalId, Type};
use ruff_python_ast::{
    ElifElseClause, Expr, ExprContext, ModModule, Parameters, Stmt, StmtAnnAssign,
    StmtAssign, StmtFunctionDef, StmtPass,
};

/// Annotation-derived type seeds for an entire lowered IR module.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModuleAnnotations {
    /// Function annotations in the same append order used by the existing lowerer.
    /// Index `0` is always the synthetic `__main__` body.
    pub functions: Vec<FunctionAnnotations>,
}

impl ModuleAnnotations {
    /// Return annotations for the IR function at `index` when the AST order still
    /// matches the lowered module.
    #[must_use]
    pub fn function(&self, index: usize) -> Option<&FunctionAnnotations> {
        self.functions.get(index)
    }
}

/// Annotation-derived type seeds for one IR function body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FunctionAnnotations {
    /// Debug/source function name. Used as a guard against order drift from
    /// unannotated generated functions such as lambdas.
    pub name: String,
    /// Local-slot seeds from parameter annotations and annotated assignments.
    pub locals: Vec<LocalAnnotation>,
    /// Return annotation, if it is one of the supported primitive spellings.
    pub return_type: Option<Type>,
}

/// One local-slot annotation seed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalAnnotation {
    pub slot: LocalId,
    pub ty: Type,
    pub source: AnnotationSource,
}

/// Where an annotation seed came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnotationSource {
    Parameter,
    AnnAssign,
}

/// Read representative annotations from a Ruff module AST.
///
/// Supported spellings are bare `int`, `float`, `bool`, and `str` names, plus the
/// same names written as string-literal forward annotations. `int` seeds the
/// speculative `IntI64` tier type so the Phase-D region planner can form an
/// unboxed integer fast path while the boxed baseline remains the executable
/// fallback.
pub fn read_module_annotations(module: &ModModule) -> Result<ModuleAnnotations, LowerError> {
    let analysis = scope::analyze_module(module)?;
    let mut functions = Vec::new();

    functions.push(annotations_for_body("__main__", &module.body, &analysis.root, None));
    collect_function_annotations(&module.body, &analysis.root, &mut functions)?;

    Ok(ModuleAnnotations { functions })
}

/// Remove annotations from a cloned Ruff AST before opt-only lowering.
///
/// Runtime annotation side effects are not part of the typed AoT acceptance slice;
/// this scrubber is deliberately only used by `pon build --opt`. Default boxed AoT
/// lowering continues to reject the same annotated constructs it rejected before.
pub fn strip_annotations_for_lowering(module: &mut ModModule) {
    strip_body_annotations(&mut module.body);
}

fn collect_function_annotations(
    body: &[Stmt],
    parent_scope: &scope::ScopeInfo,
    out: &mut Vec<FunctionAnnotations>,
) -> Result<(), LowerError> {
    for stmt in body {
        let Stmt::FunctionDef(def) = stmt else {
            continue;
        };
        let child_scope = parent_scope
            .child(scope::ScopeKind::Function, def.name.as_str())
            .ok_or_else(|| LowerError::unsupported(format!("scope metadata missing for function `{}`", def.name)))?;

        // The lowerer appends nested functions while lowering this body, then
        // appends the current function. Mirror that post-order to preserve IR ids.
        collect_function_annotations(&def.body, child_scope, out)?;
        out.push(annotations_for_body(
            def.name.as_str(),
            &def.body,
            child_scope,
            Some(def),
        ));
    }
    Ok(())
}

fn annotations_for_body(
    name: &str,
    body: &[Stmt],
    scope_info: &scope::ScopeInfo,
    def: Option<&StmtFunctionDef>,
) -> FunctionAnnotations {
    let mut annotations = FunctionAnnotations {
        name: name.to_owned(),
        locals: Vec::new(),
        return_type: def.and_then(|def| def.returns.as_deref()).and_then(annotation_type),
    };

    if let Some(def) = def {
        collect_parameter_annotations(&def.parameters, scope_info, &mut annotations.locals);
    }
    collect_ann_assigns(body, scope_info, &mut annotations.locals);

    annotations
}

fn collect_parameter_annotations(
    parameters: &Parameters,
    scope_info: &scope::ScopeInfo,
    out: &mut Vec<LocalAnnotation>,
) {
    for parameter in parameters.iter() {
        let Some(ty) = parameter.annotation().and_then(annotation_type) else {
            continue;
        };
        let Some(slot) = scope_info.local_slot(parameter.name().as_str()) else {
            continue;
        };
        out.push(LocalAnnotation {
            slot: LocalId(slot),
            ty,
            source: AnnotationSource::Parameter,
        });
    }
}

fn collect_ann_assigns(body: &[Stmt], scope_info: &scope::ScopeInfo, out: &mut Vec<LocalAnnotation>) {
    for stmt in body {
        collect_ann_assigns_stmt(stmt, scope_info, out);
    }
}

fn collect_ann_assigns_stmt(stmt: &Stmt, scope_info: &scope::ScopeInfo, out: &mut Vec<LocalAnnotation>) {
    match stmt {
        Stmt::AnnAssign(assign) => push_ann_assign(assign, scope_info, out),
        Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {}
        Stmt::If(stmt) => {
            collect_ann_assigns(&stmt.body, scope_info, out);
            collect_ann_assigns_elif_else(&stmt.elif_else_clauses, scope_info, out);
        }
        Stmt::For(stmt) => {
            collect_ann_assigns(&stmt.body, scope_info, out);
            collect_ann_assigns(&stmt.orelse, scope_info, out);
        }
        Stmt::While(stmt) => {
            collect_ann_assigns(&stmt.body, scope_info, out);
            collect_ann_assigns(&stmt.orelse, scope_info, out);
        }
        Stmt::With(stmt) => collect_ann_assigns(&stmt.body, scope_info, out),
        Stmt::Try(stmt) => {
            collect_ann_assigns(&stmt.body, scope_info, out);
            for handler in &stmt.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
                collect_ann_assigns(&handler.body, scope_info, out);
            }
            collect_ann_assigns(&stmt.orelse, scope_info, out);
            collect_ann_assigns(&stmt.finalbody, scope_info, out);
        }
        Stmt::Match(stmt) => {
            for case in &stmt.cases {
                collect_ann_assigns(&case.body, scope_info, out);
            }
        }
        _ => {}
    }
}

fn collect_ann_assigns_elif_else(
    clauses: &[ElifElseClause],
    scope_info: &scope::ScopeInfo,
    out: &mut Vec<LocalAnnotation>,
) {
    for clause in clauses {
        collect_ann_assigns(&clause.body, scope_info, out);
    }
}

fn push_ann_assign(assign: &StmtAnnAssign, scope_info: &scope::ScopeInfo, out: &mut Vec<LocalAnnotation>) {
    let Some(ty) = annotation_type(&assign.annotation) else {
        return;
    };
    let Expr::Name(name) = assign.target.as_ref() else {
        return;
    };
    if !matches!(name.ctx, ExprContext::Store) {
        return;
    }
    let Some(slot) = scope_info.local_slot(name.id.as_str()) else {
        return;
    };
    out.push(LocalAnnotation {
        slot: LocalId(slot),
        ty,
        source: AnnotationSource::AnnAssign,
    });
}

fn annotation_type(expr: &Expr) -> Option<Type> {
    match expr {
        Expr::Name(name) => primitive_annotation(name.id.as_str()),
        Expr::StringLiteral(literal) => primitive_annotation(literal.value.to_str().trim()),
        Expr::Attribute(attr) => primitive_annotation(attr.attr.as_str()),
        Expr::Subscript(subscript) => annotation_type(&subscript.value),
        _ => None,
    }
}

fn primitive_annotation(name: &str) -> Option<Type> {
    match name {
        "int" => Some(Type::IntI64),
        "float" => Some(Type::Float),
        "bool" => Some(Type::Bool),
        "str" => Some(Type::Str),
        _ => None,
    }
}

fn strip_body_annotations(body: &mut Vec<Stmt>) {
    for stmt in body {
        strip_stmt_annotations(stmt);
    }
}

fn strip_stmt_annotations(stmt: &mut Stmt) {
    match stmt {
        Stmt::FunctionDef(def) => {
            strip_parameters(&mut def.parameters);
            def.returns = None;
            strip_body_annotations(&mut def.body);
        }
        Stmt::AnnAssign(assign) => {
            let replacement = if let Some(value) = assign.value.take() {
                Stmt::Assign(StmtAssign {
                    node_index: assign.node_index.clone(),
                    range: assign.range,
                    targets: vec![*assign.target.clone()],
                    value,
                })
            } else {
                Stmt::Pass(StmtPass {
                    node_index: assign.node_index.clone(),
                    range: assign.range,
                })
            };
            *stmt = replacement;
        }
        Stmt::ClassDef(def) => strip_body_annotations(&mut def.body),
        Stmt::If(stmt) => {
            strip_body_annotations(&mut stmt.body);
            for clause in &mut stmt.elif_else_clauses {
                strip_body_annotations(&mut clause.body);
            }
        }
        Stmt::For(stmt) => {
            strip_body_annotations(&mut stmt.body);
            strip_body_annotations(&mut stmt.orelse);
        }
        Stmt::While(stmt) => {
            strip_body_annotations(&mut stmt.body);
            strip_body_annotations(&mut stmt.orelse);
        }
        Stmt::With(stmt) => strip_body_annotations(&mut stmt.body),
        Stmt::Try(stmt) => {
            strip_body_annotations(&mut stmt.body);
            for handler in &mut stmt.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
                strip_body_annotations(&mut handler.body);
            }
            strip_body_annotations(&mut stmt.orelse);
            strip_body_annotations(&mut stmt.finalbody);
        }
        Stmt::Match(stmt) => {
            for case in &mut stmt.cases {
                strip_body_annotations(&mut case.body);
            }
        }
        _ => {}
    }
}

fn strip_parameters(parameters: &mut Parameters) {
    for parameter in &mut parameters.posonlyargs {
        parameter.parameter.annotation = None;
    }
    for parameter in &mut parameters.args {
        parameter.parameter.annotation = None;
    }
    if let Some(parameter) = parameters.vararg.as_mut() {
        parameter.annotation = None;
    }
    for parameter in &mut parameters.kwonlyargs {
        parameter.parameter.annotation = None;
    }
    if let Some(parameter) = parameters.kwarg.as_mut() {
        parameter.annotation = None;
    }
}
