//! Phase-B lexical scope analysis for Ruff AST modules.
//!
//! The lowering layer consumes this file as a data-only symbol table.  It does
//! not lower Python semantics here; it classifies names, assigns stable local
//! slots, identifies closure cells/free variables, and records parameter shape
//! in the same slot order expected by `CodeInfo`-style runtime metadata.

use std::collections::{BTreeMap, BTreeSet};

use ruff_python_ast::{
    Comprehension, ExceptHandler, Expr, ExprContext, Identifier, ModModule, Parameters, Pattern,
    Stmt, StmtFunctionDef,
};

use crate::LowerError;

/// The kind of lexical namespace described by a [`ScopeInfo`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    /// A module namespace.  Bindings are globals, not function locals.
    Module,
    /// A Python function or async function namespace.
    Function,
    /// A class-body namespace.
    Class,
    /// An implicit function-like namespace used by comprehensions.
    Comprehension,
}

/// Final name classification used by the Phase-B frontend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameClass {
    /// A function-local slot that is not closed over by a child function.
    Local { slot: u32 },
    /// A module/global namespace name.  `explicit` is true for `global x`.
    Global { explicit: bool },
    /// A name provided by an enclosing function through the closure tuple.
    Free { slot: u32 },
    /// A local slot that must be promoted into a closure cell for children.
    Cell { local_slot: u32, cell_slot: u32 },
    /// A known Python builtin reached after local/free/global lookup fails.
    Builtin,
}

impl NameClass {
    /// Return the function-local slot if this classification stores in locals.
    pub fn local_slot(&self) -> Option<u32> {
        match self {
            Self::Local { slot } => Some(*slot),
            Self::Cell { local_slot, .. } => Some(*local_slot),
            Self::Global { .. } | Self::Free { .. } | Self::Builtin => None,
        }
    }

    /// Return the closure-cell slot if this classification provides a cell object.
    pub fn closure_slot(&self) -> Option<u32> {
        match self {
            Self::Cell { cell_slot, .. } | Self::Free { slot: cell_slot } => Some(*cell_slot),
            Self::Local { .. } | Self::Global { .. } | Self::Builtin => None,
        }
    }
}

/// One classified name entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolInfo {
    /// Source spelling.
    pub name: String,
    /// Final Phase-B classification.
    pub class: NameClass,
    /// Whether the name is read in this scope.
    pub is_used: bool,
    /// Whether the name is bound in this scope before global/nonlocal filtering.
    pub is_bound: bool,
    /// Whether the name came from a formal parameter.
    pub is_parameter: bool,
}

/// Local slot assignment for a function body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalSlotInfo {
    /// Source spelling.
    pub name: String,
    /// Numeric local slot.
    pub slot: u32,
    /// True if this slot is occupied by a formal parameter.
    pub is_parameter: bool,
}

/// Parameter slot metadata compatible with a future `CodeInfo` record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParameterSummary {
    /// All positional parameters in ABI/local-slot order.
    pub positional: Vec<ParameterSlot>,
    /// Number of positional-only parameters.
    pub positional_only: usize,
    /// Whether a `*args` parameter exists.
    pub has_vararg: bool,
    /// Number of keyword-only parameters.
    pub keyword_only: usize,
    /// Whether a `**kwargs` parameter exists.
    pub has_kwarg: bool,
}

impl ParameterSummary {
    /// Phase-A-compatible arity: positional-only plus positional-or-keyword.
    pub fn arity(&self) -> usize {
        self.positional.len()
    }
}

/// One formal parameter and its assigned local slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParameterSlot {
    /// Parameter name.
    pub name: String,
    /// Local slot populated from the call argv.
    pub slot: u32,
    /// True for positional-only parameters.
    pub positional_only: bool,
}

/// Final lexical information for one scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeInfo {
    /// Scope kind.
    pub kind: ScopeKind,
    /// Debug/source name.  The module scope is `__main__`.
    pub name: String,
    /// Classified names by spelling.
    pub symbols: BTreeMap<String, SymbolInfo>,
    /// Local slots in numeric order.
    pub locals: Vec<LocalSlotInfo>,
    /// Free variable names in closure-tuple order.
    pub free_vars: Vec<String>,
    /// Cell variable names in cell order.
    pub cell_vars: Vec<String>,
    /// Parameter summary in `CodeInfo` order.
    pub parameters: ParameterSummary,
    /// Direct child scopes discovered in source order.
    pub children: Vec<ScopeInfo>,
    /// True if the function contains `yield` or `yield from` directly in its body.
    pub is_generator: bool,
    /// True for `async def` functions or scopes containing direct `await`.
    pub is_async: bool,
    /// PEP 695 type-parameter names visible inside this scope, enclosing
    /// generic parameters first, then the construct's own, in source order.
    /// Synthesized annotate/alias scopes bind each of these as a plain local
    /// so lowering can initialize them with `MakeTypeVar` in the prologue.
    pub type_params: Vec<String>,
}

impl ScopeInfo {
    /// Look up one classified symbol.
    pub fn symbol(&self, name: &str) -> Option<&SymbolInfo> {
        self.symbols.get(name)
    }

    /// Return the local slot for a name classified as local or cell.
    pub fn local_slot(&self, name: &str) -> Option<u32> {
        self.symbol(name).and_then(|symbol| symbol.class.local_slot())
    }

    /// Return the closure slot for a name classified as a cell or free variable.
    pub fn closure_slot(&self, name: &str) -> Option<u32> {
        self.symbol(name).and_then(|symbol| symbol.class.closure_slot())
    }

    /// Find a direct child scope by kind and source name.
    pub fn child(&self, kind: ScopeKind, name: &str) -> Option<&ScopeInfo> {
        self.children
            .iter()
            .find(|child| child.kind == kind && child.name == name)
    }
}

/// Full module scope-analysis result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeAnalysis {
    /// Synthetic module scope.
    pub root: ScopeInfo,
}

/// Analyze a Ruff module into Phase-B scope metadata.
pub fn analyze_module(module: &ModModule) -> Result<ScopeAnalysis, LowerError> {
    let mut builder = ScopeBuilder::new(ScopeKind::Module, "__main__", false);
    scan_body(&module.body, &mut builder)?;
    Ok(ScopeAnalysis {
        root: finalize_scope(builder, &BTreeSet::new()),
    })
}

/// Analyze a single function definition as an independent lowering unit.
pub fn analyze_function_def(def: &StmtFunctionDef) -> Result<ScopeInfo, LowerError> {
    let mut builder = function_builder(def)?;
    scan_body(&def.body, &mut builder)?;
    Ok(finalize_scope(builder, &BTreeSet::new()))
}

/// Reserved scope name for synthesized PEP 649 `__annotate__` functions.
pub const ANNOTATE_SCOPE_NAME: &str = "__annotate__";

/// Reserved scope name for synthesized PEP 695 type-alias value thunks.
pub const TYPE_ALIAS_SCOPE_NAME: &str = "<type_alias>";

#[derive(Clone, Debug)]
struct ScopeBuilder {
    kind: ScopeKind,
    name: String,
    params: Vec<String>,
    positional_only: usize,
    has_vararg: bool,
    keyword_only: usize,
    has_kwarg: bool,
    bound: BTreeSet<String>,
    bound_order: Vec<String>,
    used: BTreeSet<String>,
    global_decl: BTreeSet<String>,
    nonlocal_decl: BTreeSet<String>,
    children: Vec<ScopeBuilder>,
    is_generator: bool,
    is_async: bool,
    /// PEP 695 type-parameter names visible to annotation scopes created
    /// inside this scope: enclosing generic parameters first, then this
    /// construct's own parameters, in source order.
    active_type_params: Vec<String>,
    /// Deferred `__annotate__` child accumulating module/class-level
    /// `AnnAssign` annotation expressions (PEP 649).
    annotate: Option<Box<ScopeBuilder>>,
}

impl ScopeBuilder {
    fn new(kind: ScopeKind, name: &str, is_async: bool) -> Self {
        Self {
            kind,
            name: name.to_owned(),
            params: Vec::new(),
            positional_only: 0,
            has_vararg: false,
            keyword_only: 0,
            has_kwarg: false,
            bound: BTreeSet::new(),
            bound_order: Vec::new(),
            used: BTreeSet::new(),
            global_decl: BTreeSet::new(),
            nonlocal_decl: BTreeSet::new(),
            children: Vec::new(),
            is_generator: false,
            is_async,
            active_type_params: Vec::new(),
            annotate: None,
        }
    }

    fn bind(&mut self, name: &str) {
        if self.bound.insert(name.to_owned()) {
            self.bound_order.push(name.to_owned());
        }
    }

    fn use_name(&mut self, name: &str) {
        self.used.insert(name.to_owned());
    }

    fn declare_global(&mut self, name: &str) {
        self.global_decl.insert(name.to_owned());
    }

    fn declare_nonlocal(&mut self, name: &str) {
        self.nonlocal_decl.insert(name.to_owned());
    }

    /// Return the deferred module/class `__annotate__` child, creating it on
    /// first use with the current active type parameters bound as locals.
    fn annotate_child(&mut self) -> &mut ScopeBuilder {
        if self.annotate.is_none() {
            let child = annotate_builder(ANNOTATE_SCOPE_NAME, &self.active_type_params);
            self.annotate = Some(Box::new(child));
        }
        self.annotate.as_mut().expect("annotate child was just created")
    }
}

/// Build the scope for a synthesized function-like annotation/alias thunk.
///
/// The scope binds `type_params` as ordinary non-parameter locals so lowering
/// can initialize them with `MakeTypeVar` before evaluating any expression.
fn annotate_builder(name: &str, type_params: &[String]) -> ScopeBuilder {
    let mut builder = ScopeBuilder::new(ScopeKind::Function, name, false);
    if name == ANNOTATE_SCOPE_NAME {
        builder.params.push("format".to_owned());
        builder.bind("format");
    }
    for param in type_params {
        builder.bind(param);
    }
    builder.active_type_params = type_params.to_vec();
    builder
}

fn function_builder(def: &StmtFunctionDef) -> Result<ScopeBuilder, LowerError> {
    let mut builder = ScopeBuilder::new(ScopeKind::Function, def.name.as_str(), def.is_async);
    fill_parameters(&def.parameters, &mut builder)?;
    Ok(builder)
}

fn fill_parameters(parameters: &Parameters, builder: &mut ScopeBuilder) -> Result<(), LowerError> {
    builder.positional_only = parameters.posonlyargs.len();
    builder.keyword_only = parameters.kwonlyargs.len();
    builder.has_vararg = parameters.vararg.is_some();
    builder.has_kwarg = parameters.kwarg.is_some();

    for parameter in parameters.posonlyargs.iter().chain(&parameters.args) {
        let name = parameter.name().as_str();
        builder.params.push(name.to_owned());
        builder.bind(name);
    }

    if let Some(vararg) = parameters.vararg.as_deref() {
        let name = vararg.name().as_str();
        builder.bind(name);
    }

    for parameter in &parameters.kwonlyargs {
        let name = parameter.name().as_str();
        builder.bind(name);
    }

    if let Some(kwarg) = parameters.kwarg.as_deref() {
        let name = kwarg.name().as_str();
        builder.bind(name);
    }

    Ok(())
}

fn scan_body(body: &[Stmt], scope: &mut ScopeBuilder) -> Result<(), LowerError> {
    for stmt in body {
        scan_stmt(stmt, scope)?;
    }
    Ok(())
}

fn scan_stmt(stmt: &Stmt, scope: &mut ScopeBuilder) -> Result<(), LowerError> {
    match stmt {
        Stmt::FunctionDef(def) => {
            for decorator in &def.decorator_list {
                scan_expr(&decorator.expression, scope)?;
            }
            scan_parameter_defaults(&def.parameters, scope)?;
            scope.bind(def.name.as_str());

            let own_type_params = type_param_names(def.type_params.as_deref());
            let mut active = scope.active_type_params.clone();
            active.extend(own_type_params.iter().cloned());

            if function_def_has_annotations(def) {
                let mut annotate = annotate_builder(ANNOTATE_SCOPE_NAME, &active);
                if let Some(returns) = def.returns.as_deref() {
                    scan_expr(returns, &mut annotate)?;
                }
                scan_parameter_annotations(&def.parameters, &mut annotate)?;
                scope.children.push(annotate);
            }

            let mut child = function_builder(def)?;
            child.active_type_params = active;
            scan_body(&def.body, &mut child)?;
            scope.children.push(child);
        }
        Stmt::ClassDef(def) => {
            for decorator in &def.decorator_list {
                scan_expr(&decorator.expression, scope)?;
            }
            if let Some(arguments) = def.arguments.as_deref() {
                for arg in &arguments.args {
                    scan_expr(arg, scope)?;
                }
                for keyword in &arguments.keywords {
                    scan_expr(&keyword.value, scope)?;
                }
            }
            scope.bind(def.name.as_str());

            let own_type_params = type_param_names(def.type_params.as_deref());
            let mut active = scope.active_type_params.clone();
            active.extend(own_type_params.iter().cloned());

            let mut child = ScopeBuilder::new(ScopeKind::Class, def.name.as_str(), false);
            child.active_type_params = active;
            scan_body(&def.body, &mut child)?;
            scope.children.push(child);
        }
        Stmt::Return(ret) => {
            if let Some(value) = ret.value.as_deref() {
                scan_expr(value, scope)?;
            }
        }
        Stmt::Delete(delete) => {
            for target in &delete.targets {
                bind_target(target, scope)?;
            }
        }
        Stmt::TypeAlias(alias) => {
            bind_target(&alias.name, scope)?;
            let mut active = scope.active_type_params.clone();
            active.extend(type_param_names(alias.type_params.as_deref()));
            let mut thunk = annotate_builder(TYPE_ALIAS_SCOPE_NAME, &active);
            scan_expr(&alias.value, &mut thunk)?;
            scope.children.push(thunk);
        }
        Stmt::Assign(assign) => {
            scan_expr(&assign.value, scope)?;
            for target in &assign.targets {
                bind_target(target, scope)?;
            }
        }
        Stmt::AugAssign(assign) => {
            scan_expr(&assign.target, scope)?;
            scan_expr(&assign.value, scope)?;
            bind_target(&assign.target, scope)?;
        }
        Stmt::AnnAssign(assign) => {
            if matches!(scope.kind, ScopeKind::Module | ScopeKind::Class)
                && matches!(assign.target.as_ref(), Expr::Name(_))
            {
                scan_expr(&assign.annotation, scope.annotate_child())?;
            } else {
                scan_expr(&assign.annotation, scope)?;
            }
            if let Some(value) = assign.value.as_deref() {
                scan_expr(value, scope)?;
            }
            bind_target(&assign.target, scope)?;
        }
        Stmt::For(for_stmt) => {
            scan_expr(&for_stmt.iter, scope)?;
            bind_target(&for_stmt.target, scope)?;
            scan_body(&for_stmt.body, scope)?;
            scan_body(&for_stmt.orelse, scope)?;
        }
        Stmt::While(while_stmt) => {
            scan_expr(&while_stmt.test, scope)?;
            scan_body(&while_stmt.body, scope)?;
            scan_body(&while_stmt.orelse, scope)?;
        }
        Stmt::If(if_stmt) => {
            scan_expr(&if_stmt.test, scope)?;
            scan_body(&if_stmt.body, scope)?;
            for clause in &if_stmt.elif_else_clauses {
                if let Some(test) = clause.test.as_ref() {
                    scan_expr(test, scope)?;
                }
                scan_body(&clause.body, scope)?;
            }
        }
        Stmt::With(with_stmt) => {
            for item in &with_stmt.items {
                scan_expr(&item.context_expr, scope)?;
                if let Some(optional_vars) = item.optional_vars.as_deref() {
                    bind_target(optional_vars, scope)?;
                }
            }
            scan_body(&with_stmt.body, scope)?;
        }
        Stmt::Match(match_stmt) => {
            scan_expr(&match_stmt.subject, scope)?;
            for case in &match_stmt.cases {
                bind_pattern(&case.pattern, scope)?;
                if let Some(guard) = case.guard.as_deref() {
                    scan_expr(guard, scope)?;
                }
                scan_body(&case.body, scope)?;
            }
        }
        Stmt::Raise(raise) => {
            if let Some(exc) = raise.exc.as_deref() {
                scan_expr(exc, scope)?;
            }
            if let Some(cause) = raise.cause.as_deref() {
                scan_expr(cause, scope)?;
            }
        }
        Stmt::Try(try_stmt) => {
            scan_body(&try_stmt.body, scope)?;
            for handler in &try_stmt.handlers {
                let ExceptHandler::ExceptHandler(handler) = handler;
                if let Some(type_) = handler.type_.as_deref() {
                    scan_expr(type_, scope)?;
                }
                if let Some(name) = handler.name.as_ref() {
                    scope.bind(name.as_str());
                }
                scan_body(&handler.body, scope)?;
            }
            scan_body(&try_stmt.orelse, scope)?;
            scan_body(&try_stmt.finalbody, scope)?;
        }
        Stmt::Assert(assert_stmt) => {
            scan_expr(&assert_stmt.test, scope)?;
            if let Some(msg) = assert_stmt.msg.as_deref() {
                scan_expr(msg, scope)?;
            }
        }
        Stmt::Import(import) => {
            for alias in &import.names {
                let bound = import_binding_name(&alias.name, alias.asname.as_ref());
                scope.bind(&bound);
            }
        }
        Stmt::ImportFrom(import) => {
            for alias in &import.names {
                let bound = import_binding_name(&alias.name, alias.asname.as_ref());
                scope.bind(&bound);
            }
        }
        Stmt::Global(global) => {
            for name in &global.names {
                scope.declare_global(name.as_str());
            }
        }
        Stmt::Nonlocal(nonlocal) => {
            for name in &nonlocal.names {
                scope.declare_nonlocal(name.as_str());
            }
        }
        Stmt::Expr(expr) => scan_expr(&expr.value, scope)?,
        Stmt::Pass(_) | Stmt::Break(_) | Stmt::Continue(_) | Stmt::IpyEscapeCommand(_) => {}
    }
    Ok(())
}

fn scan_parameter_defaults(parameters: &Parameters, scope: &mut ScopeBuilder) -> Result<(), LowerError> {
    for parameter in parameters.posonlyargs.iter().chain(&parameters.args).chain(&parameters.kwonlyargs) {
        if let Some(default) = parameter.default() {
            scan_expr(default, scope)?;
        }
    }
    Ok(())
}

/// Scan every parameter annotation into the (annotate) scope, in the same
/// order lowering later evaluates them: positional, `*args`, keyword-only,
/// `**kwargs`.
fn scan_parameter_annotations(parameters: &Parameters, scope: &mut ScopeBuilder) -> Result<(), LowerError> {
    for parameter in parameters.posonlyargs.iter().chain(&parameters.args) {
        if let Some(annotation) = parameter.annotation() {
            scan_expr(annotation, scope)?;
        }
    }
    if let Some(vararg) = parameters.vararg.as_deref() {
        if let Some(annotation) = vararg.annotation() {
            scan_expr(annotation, scope)?;
        }
    }
    for parameter in &parameters.kwonlyargs {
        if let Some(annotation) = parameter.annotation() {
            scan_expr(annotation, scope)?;
        }
    }
    if let Some(kwarg) = parameters.kwarg.as_deref() {
        if let Some(annotation) = kwarg.annotation() {
            scan_expr(annotation, scope)?;
        }
    }
    Ok(())
}

/// True when a `def` carries any parameter or return annotation.
pub(crate) fn function_def_has_annotations(def: &StmtFunctionDef) -> bool {
    def.returns.is_some() || parameters_have_annotations(&def.parameters)
}

fn parameters_have_annotations(parameters: &Parameters) -> bool {
    parameters
        .posonlyargs
        .iter()
        .chain(&parameters.args)
        .chain(&parameters.kwonlyargs)
        .any(|parameter| parameter.annotation().is_some())
        || parameters
            .vararg
            .as_deref()
            .is_some_and(|parameter| parameter.annotation().is_some())
        || parameters
            .kwarg
            .as_deref()
            .is_some_and(|parameter| parameter.annotation().is_some())
}

/// Names of PEP 695 type parameters in source order.
pub(crate) fn type_param_names(type_params: Option<&ruff_python_ast::TypeParams>) -> Vec<String> {
    let Some(type_params) = type_params else {
        return Vec::new();
    };
    type_params
        .type_params
        .iter()
        .map(|param| param.name().as_str().to_owned())
        .collect()
}

fn scan_expr(expr: &Expr, scope: &mut ScopeBuilder) -> Result<(), LowerError> {
    match expr {
        Expr::Name(name) => match name.ctx {
            ExprContext::Load => scope.use_name(name.id.as_str()),
            ExprContext::Store | ExprContext::Del => scope.bind(name.id.as_str()),
            ExprContext::Invalid => {}
        },
        Expr::BoolOp(expr) => {
            for value in &expr.values {
                scan_expr(value, scope)?;
            }
        }
        Expr::Named(expr) => {
            scan_expr(&expr.value, scope)?;
            bind_target(&expr.target, scope)?;
        }
        Expr::BinOp(expr) => {
            scan_expr(&expr.left, scope)?;
            scan_expr(&expr.right, scope)?;
        }
        Expr::UnaryOp(expr) => scan_expr(&expr.operand, scope)?,
        Expr::Lambda(expr) => {
            let mut child = ScopeBuilder::new(ScopeKind::Function, "<lambda>", false);
            if let Some(parameters) = expr.parameters.as_deref() {
                scan_parameter_defaults(parameters, scope)?;
                fill_parameters(parameters, &mut child)?;
            }
            scan_expr(&expr.body, &mut child)?;
            scope.children.push(child);
        }
        Expr::If(expr) => {
            scan_expr(&expr.test, scope)?;
            scan_expr(&expr.body, scope)?;
            scan_expr(&expr.orelse, scope)?;
        }
        Expr::Dict(expr) => {
            for item in &expr.items {
                if let Some(key) = item.key.as_ref() {
                    scan_expr(key, scope)?;
                }
                scan_expr(&item.value, scope)?;
            }
        }
        Expr::Set(expr) => {
            for elt in &expr.elts {
                scan_expr(elt, scope)?;
            }
        }
        Expr::ListComp(expr) => {
            scan_comprehension("<listcomp>", &expr.elt, &expr.generators, scope)?
        }
        Expr::SetComp(expr) => {
            scan_comprehension("<setcomp>", &expr.elt, &expr.generators, scope)?
        }
        Expr::DictComp(expr) => {
            let mut child = ScopeBuilder::new(ScopeKind::Comprehension, "<dictcomp>", false);
            scan_comprehension_generators(&expr.generators, scope, &mut child)?;
            scan_expr(&expr.key, &mut child)?;
            scan_expr(&expr.value, &mut child)?;
            scope.children.push(child);
        }
        Expr::Generator(expr) => {
            let mut child = ScopeBuilder::new(ScopeKind::Comprehension, "<genexpr>", false);
            child.is_generator = true;
            scan_comprehension_generators(&expr.generators, scope, &mut child)?;
            scan_expr(&expr.elt, &mut child)?;
            scope.children.push(child);
        }
        Expr::Await(expr) => {
            scope.is_async = true;
            scan_expr(&expr.value, scope)?;
        }
        Expr::Yield(expr) => {
            scope.is_generator = true;
            if let Some(value) = expr.value.as_deref() {
                scan_expr(value, scope)?;
            }
        }
        Expr::YieldFrom(expr) => {
            scope.is_generator = true;
            scan_expr(&expr.value, scope)?;
        }
        Expr::Compare(expr) => {
            scan_expr(&expr.left, scope)?;
            for comparator in expr.comparators.iter() {
                scan_expr(comparator, scope)?;
            }
        }
        Expr::Call(expr) => {
            scan_expr(&expr.func, scope)?;
            for arg in &expr.arguments.args {
                scan_expr(arg, scope)?;
            }
            for keyword in &expr.arguments.keywords {
                scan_expr(&keyword.value, scope)?;
            }
        }
        Expr::FString(expr) => scan_interpolated_elements(expr.value.elements(), scope)?,
        Expr::TString(expr) => scan_interpolated_elements(expr.value.elements(), scope)?,
        Expr::Attribute(expr) => scan_expr(&expr.value, scope)?,
        Expr::Subscript(expr) => {
            scan_expr(&expr.value, scope)?;
            scan_expr(&expr.slice, scope)?;
        }
        Expr::Starred(expr) => scan_expr(&expr.value, scope)?,
        Expr::List(expr) => {
            for elt in &expr.elts {
                scan_expr(elt, scope)?;
            }
        }
        Expr::Tuple(expr) => {
            for elt in &expr.elts {
                scan_expr(elt, scope)?;
            }
        }
        Expr::Slice(expr) => {
            if let Some(lower) = expr.lower.as_deref() {
                scan_expr(lower, scope)?;
            }
            if let Some(upper) = expr.upper.as_deref() {
                scan_expr(upper, scope)?;
            }
            if let Some(step) = expr.step.as_deref() {
                scan_expr(step, scope)?;
            }
        }
        Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_)
        | Expr::EllipsisLiteral(_)
        | Expr::IpyEscapeCommand(_) => {}
    }
    Ok(())
}

fn scan_interpolated_elements<'a>(
    elements: impl IntoIterator<Item = &'a ruff_python_ast::InterpolatedStringElement>,
    scope: &mut ScopeBuilder,
) -> Result<(), LowerError> {
    for element in elements {
        if let ruff_python_ast::InterpolatedStringElement::Interpolation(interpolation) = element {
            scan_expr(&interpolation.expression, scope)?;
            if let Some(format_spec) = interpolation.format_spec.as_deref() {
                scan_interpolated_elements(format_spec.elements.iter(), scope)?;
            }
        }
    }
    Ok(())
}

fn scan_comprehension(
    name: &str,
    elt: &Expr,
    generators: &[Comprehension],
    scope: &mut ScopeBuilder,
) -> Result<(), LowerError> {
    let mut child = ScopeBuilder::new(ScopeKind::Comprehension, name, false);
    scan_comprehension_generators(generators, scope, &mut child)?;
    scan_expr(elt, &mut child)?;
    scope.children.push(child);
    Ok(())
}

fn scan_comprehension_generators(
    generators: &[Comprehension],
    enclosing: &mut ScopeBuilder,
    child: &mut ScopeBuilder,
) -> Result<(), LowerError> {
    let Some((first, rest)) = generators.split_first() else {
        return Err(LowerError::internal("comprehension without generator clause"));
    };

    child.params.push(".0".to_owned());
    child.bind(".0");

    scan_expr(&first.iter, enclosing)?;
    bind_target(&first.target, child)?;
    for if_expr in &first.ifs {
        scan_expr(if_expr, child)?;
    }
    child.is_async |= first.is_async;

    for generator in rest {
        scan_expr(&generator.iter, child)?;
        bind_target(&generator.target, child)?;
        for if_expr in &generator.ifs {
            scan_expr(if_expr, child)?;
        }
        child.is_async |= generator.is_async;
    }
    Ok(())
}

fn bind_target(target: &Expr, scope: &mut ScopeBuilder) -> Result<(), LowerError> {
    match target {
        Expr::Name(name) if matches!(name.ctx, ExprContext::Store | ExprContext::Del) => {
            scope.bind(name.id.as_str());
        }
        Expr::Tuple(tuple) => {
            for elt in &tuple.elts {
                bind_target(elt, scope)?;
            }
        }
        Expr::List(list) => {
            for elt in &list.elts {
                bind_target(elt, scope)?;
            }
        }
        Expr::Starred(starred) => bind_target(&starred.value, scope)?,
        Expr::Attribute(attribute) => scan_expr(&attribute.value, scope)?,
        Expr::Subscript(subscript) => {
            scan_expr(&subscript.value, scope)?;
            scan_expr(&subscript.slice, scope)?;
        }
        _ => {}
    }
    Ok(())
}

fn bind_pattern(pattern: &Pattern, scope: &mut ScopeBuilder) -> Result<(), LowerError> {
    match pattern {
        Pattern::MatchValue(pattern) => scan_expr(&pattern.value, scope)?,
        Pattern::MatchSingleton(_) => {}
        Pattern::MatchSequence(pattern) => {
            for nested in &pattern.patterns {
                bind_pattern(nested, scope)?;
            }
        }
        Pattern::MatchMapping(pattern) => {
            for key in &pattern.keys {
                scan_expr(key, scope)?;
            }
            for nested in &pattern.patterns {
                bind_pattern(nested, scope)?;
            }
            if let Some(rest) = pattern.rest.as_ref() {
                scope.bind(rest.as_str());
            }
        }
        Pattern::MatchClass(pattern) => {
            scan_expr(&pattern.cls, scope)?;
            for nested in &pattern.arguments.patterns {
                bind_pattern(nested, scope)?;
            }
            for keyword in &pattern.arguments.keywords {
                bind_pattern(&keyword.pattern, scope)?;
            }
        }
        Pattern::MatchStar(pattern) => {
            if let Some(name) = pattern.name.as_ref() {
                scope.bind(name.as_str());
            }
        }
        Pattern::MatchAs(pattern) => {
            if let Some(nested) = pattern.pattern.as_deref() {
                bind_pattern(nested, scope)?;
            }
            if let Some(name) = pattern.name.as_ref() {
                scope.bind(name.as_str());
            }
        }
        Pattern::MatchOr(pattern) => {
            for nested in &pattern.patterns {
                bind_pattern(nested, scope)?;
            }
        }
    }
    Ok(())
}

fn import_binding_name(name: &Identifier, asname: Option<&Identifier>) -> String {
    if let Some(asname) = asname {
        return asname.as_str().to_owned();
    }
    name.as_str().split('.').next().unwrap_or(name.as_str()).to_owned()
}

fn finalize_scope(mut builder: ScopeBuilder, enclosing_locals: &BTreeSet<String>) -> ScopeInfo {
    if let Some(annotate) = builder.annotate.take() {
        builder.children.insert(0, *annotate);
    }
    let local_names = local_names(&builder);
    let mut child_enclosing = enclosing_locals.clone();
    if matches!(builder.kind, ScopeKind::Function | ScopeKind::Comprehension) {
        child_enclosing.extend(local_names.iter().cloned());
    }

    let mut children = Vec::with_capacity(builder.children.len());
    let mut names_needed_by_children = BTreeSet::new();
    for child in builder.children.drain(..) {
        let child_info = finalize_scope(child, &child_enclosing);
        for free in &child_info.free_vars {
            names_needed_by_children.insert(free.clone());
        }
        children.push(child_info);
    }

    let mut free_names = BTreeSet::new();
    for name in &builder.nonlocal_decl {
        free_names.insert(name.clone());
    }
    for name in &builder.used {
        if local_names.contains(name) || builder.global_decl.contains(name) {
            continue;
        }
        if enclosing_locals.contains(name) {
            free_names.insert(name.clone());
        }
    }
    if matches!(builder.kind, ScopeKind::Function | ScopeKind::Comprehension) {
        for name in &names_needed_by_children {
            if !local_names.contains(name) && !builder.global_decl.contains(name) {
                free_names.insert(name.clone());
            }
        }
    }

    let cell_names: BTreeSet<_> = names_needed_by_children
        .iter()
        .filter(|name| local_names.contains(*name))
        .cloned()
        .collect();

    let locals = assign_local_slots(&builder, &local_names);
    let slot_by_name: BTreeMap<String, u32> = locals
        .iter()
        .map(|slot| (slot.name.clone(), slot.slot))
        .collect();
    let free_vars: Vec<String> = free_names.into_iter().collect();
    let free_slot_by_name: BTreeMap<String, u32> = free_vars
        .iter()
        .enumerate()
        .map(|(index, name)| (name.clone(), index as u32))
        .collect();
    let cell_vars: Vec<String> = cell_names.into_iter().collect();
    let cell_slot_by_name: BTreeMap<String, u32> = cell_vars
        .iter()
        .enumerate()
        .map(|(index, name)| (name.clone(), index as u32))
        .collect();

    let mut all_names = BTreeSet::new();
    all_names.extend(builder.bound.iter().cloned());
    all_names.extend(builder.used.iter().cloned());
    all_names.extend(builder.global_decl.iter().cloned());
    all_names.extend(builder.nonlocal_decl.iter().cloned());
    all_names.extend(names_needed_by_children);

    let mut symbols = BTreeMap::new();
    for name in all_names {
        let class = if let Some(local_slot) = slot_by_name.get(&name) {
            if let Some(cell_slot) = cell_slot_by_name.get(&name) {
                NameClass::Cell {
                    local_slot: *local_slot,
                    cell_slot: *cell_slot,
                }
            } else {
                NameClass::Local { slot: *local_slot }
            }
        } else if let Some(free_slot) = free_slot_by_name.get(&name) {
            NameClass::Free { slot: *free_slot }
        } else if builder.global_decl.contains(&name) {
            NameClass::Global { explicit: true }
        } else if is_known_builtin(&name) {
            NameClass::Builtin
        } else {
            NameClass::Global { explicit: false }
        };
        let is_parameter = builder.params.iter().any(|param| param == &name);
        symbols.insert(
            name.clone(),
            SymbolInfo {
                name: name.clone(),
                class,
                is_used: builder.used.contains(&name),
                is_bound: builder.bound.contains(&name),
                is_parameter,
            },
        );
    }

    let parameters = parameter_summary(&builder, &slot_by_name);

    ScopeInfo {
        kind: builder.kind,
        name: builder.name,
        symbols,
        locals,
        free_vars,
        cell_vars,
        parameters,
        children,
        is_generator: builder.is_generator,
        is_async: builder.is_async,
        type_params: builder.active_type_params,
    }
}

fn local_names(builder: &ScopeBuilder) -> BTreeSet<String> {
    if matches!(builder.kind, ScopeKind::Module | ScopeKind::Class) {
        return BTreeSet::new();
    }

    builder
        .bound
        .iter()
        .filter(|name| !builder.global_decl.contains(*name) && !builder.nonlocal_decl.contains(*name))
        .cloned()
        .collect()
}

fn assign_local_slots(builder: &ScopeBuilder, local_names: &BTreeSet<String>) -> Vec<LocalSlotInfo> {
    let mut locals = Vec::new();
    let mut emitted = BTreeSet::new();
    for name in &builder.params {
        if local_names.contains(name) && emitted.insert(name.clone()) {
            locals.push(LocalSlotInfo {
                name: name.clone(),
                slot: locals.len() as u32,
                is_parameter: true,
            });
        }
    }
    for name in &builder.bound_order {
        if local_names.contains(name) && emitted.insert(name.clone()) {
            locals.push(LocalSlotInfo {
                name: name.clone(),
                slot: locals.len() as u32,
                is_parameter: false,
            });
        }
    }
    locals
}

fn parameter_summary(builder: &ScopeBuilder, slot_by_name: &BTreeMap<String, u32>) -> ParameterSummary {
    let positional = builder
        .params
        .iter()
        .enumerate()
        .filter_map(|(index, name)| {
            Some(ParameterSlot {
                name: name.clone(),
                slot: *slot_by_name.get(name)?,
                positional_only: index < builder.positional_only,
            })
        })
        .collect();
    ParameterSummary {
        positional,
        positional_only: builder.positional_only,
        has_vararg: builder.has_vararg,
        keyword_only: builder.keyword_only,
        has_kwarg: builder.has_kwarg,
    }
}

fn is_known_builtin(name: &str) -> bool {
    super::import::is_known_builtin_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_module_source;

    fn analyze(source: &str) -> ScopeAnalysis {
        let module = parse_module_source(source).expect("fixture should parse");
        analyze_module(&module).expect("scope analysis should succeed")
    }

    #[test]
    fn classifies_nested_closure_cells_and_frees() {
        let analysis = analyze(
            r#"
def outer(a):
    b = 1
    def inner(c):
        return a + b + c + print
    return inner
"#,
        );

        let outer = analysis
            .root
            .child(ScopeKind::Function, "outer")
            .expect("outer function should be discovered");
        assert_eq!(outer.parameters.arity(), 1);
        assert_eq!(outer.local_slot("a"), Some(0));
        assert_eq!(outer.local_slot("b"), Some(1));
        assert_eq!(outer.local_slot("inner"), Some(2));
        assert_eq!(outer.cell_vars, vec!["a".to_owned(), "b".to_owned()]);
        assert!(matches!(
            outer.symbol("a").map(|symbol| &symbol.class),
            Some(NameClass::Cell { local_slot: 0, cell_slot: 0 })
        ));
        assert!(matches!(
            outer.symbol("b").map(|symbol| &symbol.class),
            Some(NameClass::Cell { local_slot: 1, cell_slot: 1 })
        ));

        let inner = outer
            .child(ScopeKind::Function, "inner")
            .expect("inner function should be discovered");
        assert_eq!(inner.parameters.arity(), 1);
        assert_eq!(inner.local_slot("c"), Some(0));
        assert_eq!(inner.free_vars, vec!["a".to_owned(), "b".to_owned()]);
        assert!(matches!(
            inner.symbol("a").map(|symbol| &symbol.class),
            Some(NameClass::Free { slot: 0 })
        ));
        assert!(matches!(
            inner.symbol("b").map(|symbol| &symbol.class),
            Some(NameClass::Free { slot: 1 })
        ));
        assert!(matches!(
            inner.symbol("print").map(|symbol| &symbol.class),
            Some(NameClass::Builtin)
        ));
    }

    #[test]
    fn classifies_global_and_nonlocal_declarations() {
        let analysis = analyze(
            r#"
x = 0
def outer():
    y = 1
    def inner():
        global x
        nonlocal y
        x = y
        return x
"#,
        );

        let outer = analysis
            .root
            .child(ScopeKind::Function, "outer")
            .expect("outer function should be discovered");
        assert_eq!(outer.cell_vars, vec!["y".to_owned()]);

        let inner = outer
            .child(ScopeKind::Function, "inner")
            .expect("inner function should be discovered");
        assert!(matches!(
            inner.symbol("x").map(|symbol| &symbol.class),
            Some(NameClass::Global { explicit: true })
        ));
        assert!(matches!(
            inner.symbol("y").map(|symbol| &symbol.class),
            Some(NameClass::Free { slot: 0 })
        ));
    }

    #[test]
    fn detects_generator_from_yield_inside_function() {
        let analysis = analyze(
            r#"
def gen(n):
    yield n
"#,
        );

        let generator = analysis
            .root
            .child(ScopeKind::Function, "gen")
            .expect("generator function should be discovered");
        assert!(generator.is_generator);
        assert_eq!(generator.parameters.arity(), 1);
    }
}
