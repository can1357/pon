//! Lowering from Ruff's Python AST into pon IR.
//!
//! Phase B keeps the public Phase-A entry points (`lower_source` and
//! `lower_module`) in place while introducing a real driver boundary.  The driver
//! performs scope analysis first, then routes every statement/expression family
//! through a named module.  Most Phase-B families intentionally return precise
//! `LowerError::Unsupported` values today; the routing exists so future semantic
//! work lands in the right family instead of expanding a single monolithic match.

use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};

use ruff_python_ast::{Expr, ExprContext, ModModule, Number, Parameters, Stmt, StmtAssign, StmtFunctionDef};
use ruff_python_ast::visitor::{self, Visitor};
use ruff_text_size::Ranged;

use crate::desugar::desugar_module;
use crate::ir::{
    Block, BlockId, CellId, FeedbackSlot, Function, FunctionId, Inst, InstKind, LocalId, Module,
    NameId, ParamLayout, PyConst, Terminator, Value,
};
use crate::parse::parse_module_source;

pub mod scope;
pub use scope::{
    LocalSlotInfo, NameClass, ParameterSlot, ParameterSummary, ScopeAnalysis, ScopeInfo, ScopeKind,
    SymbolInfo,
};

/// Byte span for a source construct that lowering rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceSpan {
    /// Inclusive byte offset of the first byte.
    pub start: u32,
    /// Exclusive byte offset one past the final byte.
    pub end: u32,
}

impl SourceSpan {
    fn from_bounds(start: u32, end: u32) -> Self {
        Self { start, end }
    }
}

/// Direct dynamic-code entry points that an AoT build cannot compile away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DynamicCodeKind {
    /// Python's `eval` builtin.
    Eval,
    /// Python's `exec` builtin.
    Exec,
    /// Python's `compile` builtin.
    Compile,
}

impl DynamicCodeKind {
    /// Source spelling of the builtin.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eval => "eval",
            Self::Exec => "exec",
            Self::Compile => "compile",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "eval" => Some(Self::Eval),
            "exec" => Some(Self::Exec),
            "compile" => Some(Self::Compile),
            _ => None,
        }
    }
}

/// A direct call to `eval`, `exec`, or `compile` found before lowering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicSink {
    /// Which dynamic-code builtin was called.
    pub kind: DynamicCodeKind,
    /// Byte span of the callee name, suitable for file:line diagnostics.
    pub span: SourceSpan,
}

/// Error returned when parsing or lowering cannot produce executable IR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LowerError {
    /// Ruff rejected the source before lowering.
    Parse(String),
    /// The source uses Python syntax outside the currently executable slice.
    Unsupported {
        /// User-facing feature name.
        feature: String,
        /// Source byte span when the rejected AST node provided one.
        span: Option<SourceSpan>,
    },
    /// The lowerer hit an internal capacity or consistency limit.
    Internal(String),
}

impl LowerError {
    /// Build a parse error from Ruff's diagnostic text.
    #[must_use]
    pub fn parse(message: impl Into<String>) -> Self {
        Self::Parse(message.into())
    }

    /// Build an unsupported-construct error without a source span.
    #[must_use]
    pub fn unsupported(feature: impl Into<String>) -> Self {
        Self::Unsupported {
            feature: feature.into(),
            span: None,
        }
    }

    fn unsupported_at(feature: impl Into<String>, span: SourceSpan) -> Self {
        Self::Unsupported {
            feature: feature.into(),
            span: Some(span),
        }
    }


    fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }
}

impl Display for LowerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            LowerError::Parse(message) => write!(f, "failed to parse Python module: {message}"),
            LowerError::Unsupported { feature, span } => {
                write!(f, "unsupported Phase-B Python construct: {feature}")?;
                if let Some(span) = span {
                    write!(f, " at byte {}..{}", span.start, span.end)?;
                }
                Ok(())
            }
            LowerError::Internal(message) => write!(f, "internal lowering error: {message}"),
        }
    }
}

impl Error for LowerError {}

/// Parse and lower Python source into IR.
pub fn lower_source(source: &str) -> Result<Module, LowerError> {
    let parsed = parse_module_source(source)?;
    LoweringDriver::with_source(source)
        .lower_module(&parsed)
        .map(desugar_module)
}

/// Lower a Ruff module AST into IR while preserving the Phase-A executable slice.
///
/// `source` must be the text the AST's byte ranges index (annotation-scrubbed
/// clones keep original ranges, so the pre-scrub text is correct for them).
/// When present it provides statement-level [`Inst::line`] numbers; with
/// `None` every lowered instruction carries line `0` and downstream traceback
/// entries report line 0.
pub fn lower_module(module: &ModModule, source: Option<&str>) -> Result<Module, LowerError> {
    let driver = match source {
        Some(source) => LoweringDriver::with_source(source),
        None => LoweringDriver::new(),
    };
    driver.lower_module(module).map(desugar_module)
}

/// Parse Python source and report direct `eval`/`exec`/`compile` calls.
///
/// This intentionally runs before lowering so AoT diagnostics can retain source
/// locations even though the executable tier-0 IR does not carry spans.
pub fn scan_dynamic_sinks_source(source: &str) -> Result<Vec<DynamicSink>, LowerError> {
    let parsed = parse_module_source(source)?;
    Ok(scan_dynamic_sinks(&parsed))
}

/// Report direct `eval`/`exec`/`compile` calls in a parsed Python module.
///
/// The scan is deliberately conservative and narrow: it catches the direct call
/// form (`eval(...)`, `exec(...)`, `compile(...)`) that lowers through a builtin
/// name load followed by `Call`. Indirect access through `getattr`, rebinding, or
/// imports remains a runtime AoT-boundary check.
#[must_use]
pub fn scan_dynamic_sinks(module: &ModModule) -> Vec<DynamicSink> {
    let mut scanner = DynamicSinkScanner { sinks: Vec::new() };
    scanner.visit_body(&module.body);
    scanner.sinks
}

struct DynamicSinkScanner {
    sinks: Vec<DynamicSink>,
}

impl<'a> Visitor<'a> for DynamicSinkScanner {
    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Call(call) = expr {
            if let Expr::Name(callee) = call.func.as_ref() {
                if let Some(kind) = DynamicCodeKind::from_name(callee.id.as_str()) {
                    self.sinks.push(DynamicSink {
                        kind,
                        span: span_expr(call.func.as_ref()),
                    });
                }
            }
        }

        visitor::walk_expr(self, expr);
    }
}

#[derive(Default)]
struct NameTable {
    names: Vec<String>,
    ids: HashMap<String, NameId>,
}

impl NameTable {
    fn intern(&mut self, name: &str) -> Result<NameId, LowerError> {
        if let Some(id) = self.ids.get(name) {
            return Ok(*id);
        }

        let id = u32::try_from(self.names.len())
            .map(NameId)
            .map_err(|_| LowerError::internal("too many interned names for u32 ids"))?;
        let owned = name.to_owned();
        self.names.push(owned.clone());
        self.ids.insert(owned, id);
        Ok(id)
    }
}

/// Phase-B lowering driver.
///
/// The driver owns module-wide allocation state (function table and interned
/// name table), performs one scope-analysis pass, and then delegates AST
/// families to small routing modules.  Implemented Phase-A semantics remain in
/// the `assign` and `func` families; unimplemented Phase-B families return
/// `LowerError::Unsupported { feature, span }` from their route point.
pub(crate) struct LoweringDriver {
    functions: Vec<Function>,
    names: NameTable,
    source: Option<String>,
    /// Byte offset of each 1-based line start; `None` without source text.
    line_starts: Option<Vec<u32>>,
    /// `from __future__ import annotations` (PEP 563) is active for the module
    /// being lowered: annotation values are stored as their source text and
    /// the synthesized `__annotate__` returns those strings for every format
    /// rather than evaluating the expressions.
    future_annotations: bool,
}

/// True when the module opens with `from __future__ import annotations`
/// (PEP 563).  Mirrors Python's future-statement placement rule: an optional
/// leading docstring, then a run of `from __future__` imports; the first
/// ordinary statement closes the future block.
fn module_enables_future_annotations(body: &[Stmt]) -> bool {
    let mut start = 0;
    if let Some(Stmt::Expr(stmt)) = body.first() {
        if matches!(&*stmt.value, Expr::StringLiteral(_)) {
            start = 1;
        }
    }
    for stmt in &body[start..] {
        let Stmt::ImportFrom(import) = stmt else {
            return false;
        };
        if import.module.as_ref().map(ruff_python_ast::Identifier::as_str) != Some("__future__") {
            return false;
        }
        if import.names.iter().any(|alias| alias.name.as_str() == "annotations") {
            return true;
        }
    }
    false
}

impl LoweringDriver {
    fn new() -> Self {
        Self {
            functions: Vec::new(),
            names: NameTable::default(),
            source: None,
            line_starts: None,
            future_annotations: false,
        }
    }

    fn with_source(source: &str) -> Self {
        Self {
            functions: Vec::new(),
            names: NameTable::default(),
            source: Some(source.to_owned()),
            line_starts: Some(compute_line_starts(source)),
            future_annotations: false,
        }
    }
    pub(crate) fn source_slice(&self, span: SourceSpan) -> Option<&str> {
        let source = self.source.as_deref()?;
        source.get(span.start as usize..span.end as usize)
    }

    /// 1-based source line containing byte `offset`, or `0` without source.
    fn line_at_offset(&self, offset: u32) -> u32 {
        let Some(starts) = &self.line_starts else {
            return 0;
        };
        starts.partition_point(|&start| start <= offset) as u32
    }

    /// Statement-granularity line stamp for everything `stmt` lowers to.
    fn statement_line(&self, stmt: &Stmt) -> u32 {
        self.line_at_offset(stmt.range().start().to_u32())
    }

    pub(crate) fn expr_source(&self, expr: &Expr) -> Option<&str> {
        self.source_slice(span_expr(expr))
    }

    fn lower_module(mut self, module: &ModModule) -> Result<Module, LowerError> {
        let analysis = scope::analyze_module(module)?;
        let main = self.reserve_function("__main__")?;
        let mut body = BodyScope::new(&analysis.root);
        self.future_annotations = module_enables_future_annotations(&module.body);

        // PEP 649: synthesize and store the module `__annotate__` FIRST —
        // CPython stores it before any user statement (probed via dis on
        // python3.14: the store precedes imports).  `pon_store_global` bumps
        // the namespace version, so GlobalIC records stay coherent.
        if let Some((annotate_info, entries)) = synth::claim_namespace_annotate(&mut body, &module.body)? {
            let annotate = synth::synthesize_annotate_scope(&mut self, &mut body, annotate_info, &entries)?;
            let annotate_name = self.names.intern(scope::ANNOTATE_SCOPE_NAME)?;
            body.emit(InstKind::StoreGlobal(annotate_name, annotate))?;
        }

        for stmt in &module.body {
            self.lower_stmt(&mut body, stmt)?;
        }

        let main_function = body.finish()?;
        self.replace_reserved_function(main, main_function)?;

        Ok(Module {
            functions: self.functions,
            main,
            names: self.names.names,
        })
    }

    fn reserve_function(&mut self, name: &str) -> Result<FunctionId, LowerError> {
        let index = u32::try_from(self.functions.len())
            .map_err(|_| LowerError::internal("too many functions for u32 ids"))?;
        self.functions.push(Function {
            name: name.to_owned(),
            arity: 0,
            is_coroutine: false,
            is_generator: false,
            is_async_generator: false,
            params: ParamLayout::default(),
            blocks: vec![Block {
                id: BlockId(0),
                insts: Vec::new(),
                term: Terminator::Unreachable,
            }],
            n_locals: 0,
        });
        Ok(FunctionId(index))
    }

    fn replace_reserved_function(
        &mut self,
        id: FunctionId,
        function: Function,
    ) -> Result<(), LowerError> {
        let slot = self
            .functions
            .get_mut(id.0 as usize)
            .ok_or_else(|| LowerError::internal("reserved function id is out of bounds"))?;
        *slot = function;
        Ok(())
    }

    fn append_function(&mut self, function: Function) -> Result<FunctionId, LowerError> {
        let index = u32::try_from(self.functions.len())
            .map_err(|_| LowerError::internal("too many functions for u32 ids"))?;
        self.functions.push(function);
        Ok(FunctionId(index))
    }

    #[allow(dead_code)]
    fn lower_function_def(&mut self, def: &StmtFunctionDef) -> Result<FunctionId, LowerError> {
        validate_function_header(def)?;
        let info = scope::analyze_function_def(def)?;
        if !info.free_vars.is_empty() || !info.cell_vars.is_empty() {
            return unsupported_at("closure variables", span_function(def));
        }
        let mut body = BodyScope::new(&info);

        for stmt in &def.body {
            self.lower_stmt(&mut body, stmt)?;
        }

        let function = body.finish()?;
        self.append_function(function)
    }

    fn lower_stmt(&mut self, scope: &mut BodyScope, stmt: &Stmt) -> Result<(), LowerError> {
        self.lower_stmt_with_loop(scope, stmt, None)
    }

    fn lower_stmt_with_loop(
        &mut self,
        scope: &mut BodyScope,
        stmt: &Stmt,
        loop_targets: Option<control::LoopTargets>,
    ) -> Result<(), LowerError> {
        if scope.is_terminated() {
            // Unreachable statement in an already-terminated statement list
            // (dead code after `return`/`raise`/`break`/`continue`).  CPython
            // compiles such statements and never executes them, so skipping
            // the lowering is observably identical: scope analysis already
            // scanned the full AST (bindings and generator-ness are settled),
            // and nothing here can run.  `def f(): return; yield` — the empty
            // generator idiom — relies on exactly this.
            return Ok(());
        }

        // Statement-granularity source line for every instruction this
        // statement lowers (`0` when the driver has no source text).
        scope.current_line = self.statement_line(stmt);

        match stmt {
            Stmt::FunctionDef(def) => func::lower_function_def_stmt(self, scope, def),
            Stmt::Return(ret) => control::lower_return(self, scope, ret),
            Stmt::Expr(expr_stmt) => {
                self.lower_expr(scope, &expr_stmt.value)?;
                Ok(())
            }
            Stmt::Assign(assign) => assign::lower_assign(self, scope, assign),
            Stmt::ClassDef(def) => class::lower_class_def(self, scope, def),
            Stmt::For(stmt) => self.lower_for_stmt(scope, stmt, loop_targets),
            Stmt::While(stmt) => self.lower_while_stmt(scope, stmt, loop_targets),
            Stmt::If(stmt) => self.lower_if_stmt(scope, stmt, loop_targets),
            Stmt::Break(stmt) => control::lower_break_with_targets(scope, stmt, loop_targets),
            Stmt::Continue(stmt) => control::lower_continue_with_targets(scope, stmt, loop_targets),
            Stmt::With(stmt) => with_::lower_with_stmt(self, scope, stmt, loop_targets),
            Stmt::Match(stmt) => match_::lower_match(self, scope, stmt, loop_targets),
            Stmt::Try(stmt) => try_::lower_try(self, scope, stmt, loop_targets),
            Stmt::Import(stmt) => import::lower_import_stmt(self, scope, stmt),
            Stmt::ImportFrom(stmt) => import::lower_import_from_stmt(self, scope, stmt),
            Stmt::Delete(stmt) => assign::lower_delete(self, scope, stmt),
            Stmt::AugAssign(stmt) => assign::lower_aug_assign_with_driver(self, scope, stmt),
            Stmt::AnnAssign(stmt) => assign::lower_ann_assign(self, scope, stmt),
            Stmt::TypeAlias(stmt) => assign::lower_type_alias(self, scope, stmt),
            Stmt::Raise(stmt) => try_::lower_raise(self, scope, stmt),
            Stmt::Assert(stmt) => control::lower_assert(self, scope, stmt),
            Stmt::Global(stmt) => import::lower_global(stmt),
            Stmt::Nonlocal(stmt) => import::lower_nonlocal(stmt),
            Stmt::Pass(stmt) => control::lower_pass(stmt),
            Stmt::IpyEscapeCommand(stmt) => unsupported_at("IPython escape command", span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32())),
        }
    }

    fn lower_stmt_list(
        &mut self,
        scope: &mut BodyScope,
        body: &[Stmt],
        loop_targets: Option<control::LoopTargets>,
    ) -> Result<(), LowerError> {
        for stmt in body {
            if scope.is_terminated() {
                break;
            }
            self.lower_stmt_with_loop(scope, stmt, loop_targets)?;
        }
        Ok(())
    }

    fn lower_if_stmt(
        &mut self,
        scope: &mut BodyScope,
        stmt: &ruff_python_ast::StmtIf,
        loop_targets: Option<control::LoopTargets>,
    ) -> Result<(), LowerError> {
        let then_block = scope.alloc_block()?;
        let else_block = scope.alloc_block()?;
        let done_block = scope.alloc_block()?;
        control::lower_if_header_with_driver(self, scope, stmt, then_block, else_block)?;

        scope.switch_to(then_block)?;
        self.lower_stmt_list(scope, &stmt.body, loop_targets)?;
        scope.jump_if_open(done_block)?;

        scope.switch_to(else_block)?;
        self.lower_elif_else_clauses(scope, &stmt.elif_else_clauses, done_block, loop_targets)?;
        scope.jump_if_open(done_block)?;

        scope.switch_to(done_block)?;
        Ok(())
    }

    fn lower_elif_else_clauses(
        &mut self,
        scope: &mut BodyScope,
        clauses: &[ruff_python_ast::ElifElseClause],
        done_block: BlockId,
        loop_targets: Option<control::LoopTargets>,
    ) -> Result<(), LowerError> {
        for clause in clauses {
            if let Some(test) = clause.test.as_ref() {
                let body_block = scope.alloc_block()?;
                let next_block = scope.alloc_block()?;
                let test = self.lower_expr(scope, test)?;
                let cond = scope.emit(InstKind::BoolTest { val: test })?;
                scope.set_term(Terminator::CondBranch {
                    cond,
                    then_: body_block,
                    else_: next_block,
                })?;
                scope.switch_to(body_block)?;
                self.lower_stmt_list(scope, &clause.body, loop_targets)?;
                scope.jump_if_open(done_block)?;
                scope.switch_to(next_block)?;
            } else {
                self.lower_stmt_list(scope, &clause.body, loop_targets)?;
                return Ok(());
            }
        }
        Ok(())
    }

    fn lower_for_stmt(
        &mut self,
        scope: &mut BodyScope,
        stmt: &ruff_python_ast::StmtFor,
        _loop_targets: Option<control::LoopTargets>,
    ) -> Result<(), LowerError> {
        if stmt.is_async {
            return self.lower_async_for_stmt(scope, stmt, _loop_targets);
        }
        let header_block = scope.alloc_block()?;
        let body_block = scope.alloc_block()?;
        let else_block = if stmt.orelse.is_empty() {
            None
        } else {
            Some(scope.alloc_block()?)
        };
        let done_block = scope.alloc_block()?;
        let iterable = self.lower_expr(scope, &stmt.iter)?;
        let iter = scope.emit(InstKind::GetIter { iterable })?;
        scope.set_term(Terminator::Jump(header_block))?;

        scope.switch_to(header_block)?;
        let item = scope.emit(InstKind::ForNext { iter })?;
        scope.set_term(Terminator::ForLoop {
            iter,
            body: body_block,
            done: else_block.unwrap_or(done_block),
        })?;

        scope.switch_to(body_block)?;
        control::lower_for_item_store_with_driver(self, scope, stmt, item)?;
        let nested_targets = control::LoopTargets {
            break_block: done_block,
            continue_block: header_block,
        };
        self.lower_stmt_list(scope, &stmt.body, Some(nested_targets))?;
        scope.jump_if_open(header_block)?;

        if let Some(else_block) = else_block {
            scope.switch_to(else_block)?;
            self.lower_stmt_list(scope, &stmt.orelse, _loop_targets)?;
            scope.jump_if_open(done_block)?;
        }

        scope.switch_to(done_block)?;
        Ok(())
    }

    /// Lowers `async for` under CPython's `END_ASYNC_FOR` discipline: acquire
    /// the async iterator once, then each iteration awaits
    /// `aiter.__anext__()` inside a protected region whose handler routes
    /// `StopAsyncIteration` to the `else`/done edge and re-raises everything
    /// else ([`generator::emit_async_for_step`]).  The loop body runs OUTSIDE
    /// the protected region, so a `StopAsyncIteration` raised there
    /// propagates instead of terminating the loop.
    fn lower_async_for_stmt(
        &mut self,
        scope: &mut BodyScope,
        stmt: &ruff_python_ast::StmtFor,
        _loop_targets: Option<control::LoopTargets>,
    ) -> Result<(), LowerError> {
        if !scope.info.is_async {
            // CPython rejects this shape at compile time with the same words.
            return unsupported_at(
                "'async for' outside async function",
                span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()),
            );
        }
        let header_block = scope.alloc_block()?;
        let else_block = if stmt.orelse.is_empty() {
            None
        } else {
            Some(scope.alloc_block()?)
        };
        let done_block = scope.alloc_block()?;
        let iterable = self.lower_expr(scope, &stmt.iter)?;
        let iter = scope.emit(InstKind::GetAIter { iterable })?;
        scope.set_term(Terminator::Jump(header_block))?;

        scope.switch_to(header_block)?;
        let item =
            generator::emit_async_for_step(self, scope, iter, else_block.unwrap_or(done_block))?;
        control::lower_for_item_store_with_driver(self, scope, stmt, item)?;
        let nested_targets = control::LoopTargets {
            break_block: done_block,
            continue_block: header_block,
        };
        self.lower_stmt_list(scope, &stmt.body, Some(nested_targets))?;
        scope.jump_if_open(header_block)?;

        if let Some(else_block) = else_block {
            scope.switch_to(else_block)?;
            self.lower_stmt_list(scope, &stmt.orelse, _loop_targets)?;
            scope.jump_if_open(done_block)?;
        }

        scope.switch_to(done_block)?;
        Ok(())
    }

    fn lower_while_stmt(
        &mut self,
        scope: &mut BodyScope,
        stmt: &ruff_python_ast::StmtWhile,
        _loop_targets: Option<control::LoopTargets>,
    ) -> Result<(), LowerError> {
        let header_block = scope.alloc_block()?;
        let body_block = scope.alloc_block()?;
        let else_block = if stmt.orelse.is_empty() {
            None
        } else {
            Some(scope.alloc_block()?)
        };
        let done_block = scope.alloc_block()?;
        scope.set_term(Terminator::Jump(header_block))?;

        scope.switch_to(header_block)?;
        control::lower_while_header_with_driver(
            self,
            scope,
            stmt,
            body_block,
            else_block.unwrap_or(done_block),
        )?;

        scope.switch_to(body_block)?;
        let nested_targets = control::LoopTargets {
            break_block: done_block,
            continue_block: header_block,
        };
        self.lower_stmt_list(scope, &stmt.body, Some(nested_targets))?;
        scope.jump_if_open(header_block)?;

        if let Some(else_block) = else_block {
            scope.switch_to(else_block)?;
            self.lower_stmt_list(scope, &stmt.orelse, _loop_targets)?;
            scope.jump_if_open(done_block)?;
        }

        scope.switch_to(done_block)?;
        Ok(())
    }

    fn lower_expr(&mut self, scope: &mut BodyScope, expr: &Expr) -> Result<Value, LowerError> {
        match expr {
            Expr::Name(name) if matches!(name.ctx, ExprContext::Load) => {
                let raw_name = name.id.as_str();
                if scope.is_class() && !scope.is_global_name(raw_name) {
                    // Class-body reads normally resolve through the namespace
                    // mapping (LOAD_NAME); names captured from an enclosing
                    // function read the closure cell instead (CPython
                    // LOAD_FROM_DICT_OR_DEREF).
                    if let Some(cell) = scope.class_deref_cell(raw_name) {
                        scope.emit(InstKind::LoadCell(cell))
                    } else {
                        let name_id = self.names.intern(raw_name)?;
                        scope.emit(InstKind::LoadName(name_id))
                    }
                } else {
                    match scope.name_class(raw_name) {
                        Some(NameClass::Local { slot }) => scope.emit(InstKind::LoadLocal(LocalId(*slot))),
                        Some(NameClass::Cell { cell_slot, .. }) => {
                            scope.emit(InstKind::LoadCell(CellId(*cell_slot)))
                        }
                        Some(NameClass::Free { slot }) => {
                            let cell = scope.free_cell(*slot);
                            scope.emit(InstKind::LoadCell(cell))
                        }
                        Some(NameClass::Builtin) => {
                            let name_id = self.names.intern(raw_name)?;
                            scope.emit(InstKind::LoadBuiltin(name_id))
                        }
                        Some(NameClass::Global { .. }) | None => {
                            let name_id = self.names.intern(raw_name)?;
                            scope.emit(InstKind::LoadGlobal(name_id))
                        }
                    }
                }
            }
            Expr::Name(_) => unsupported_expr("non-load name expression", expr),
            Expr::Call(call) => func::lower_call(self, scope, call),
            Expr::BinOp(binop) => {
                let op = assign::bin_op_from_operator(binop.op)?;
                let lhs = self.lower_expr(scope, &binop.left)?;
                let rhs = self.lower_expr(scope, &binop.right)?;
                scope.emit(InstKind::BinaryOp { op, lhs, rhs })
            }
            Expr::StringLiteral(literal) => {
                scope.emit(InstKind::Const(PyConst::Str(literal.value.to_str().to_owned())))
            }
            Expr::NumberLiteral(literal) => match &literal.value {
                Number::Int(value) => match value.as_i64() {
                    Some(value) => scope.emit(InstKind::Const(PyConst::Int(value))),
                    // Wider than i64: keep the lexer's token (decimal, or
                    // radix-prefixed for hex/octal/binary) and let the runtime
                    // parse it into an arbitrary-precision int.
                    None => scope.emit(InstKind::Const(PyConst::BigInt(value.to_string().into_boxed_str()))),
                },
                Number::Float(value) => scope.emit(InstKind::Const(PyConst::Float(*value))),
                Number::Complex { real, imag } => scope.emit(InstKind::Const(PyConst::Complex {
                    real: *real,
                    imag: *imag,
                })),
            },
            Expr::NoneLiteral(_) => scope.emit(InstKind::Const(PyConst::None)),
            Expr::FString(fstring) => strings::lower_f_string(self, scope, fstring),
            Expr::TString(tstring) => strings::lower_t_string(self, scope, tstring),
            Expr::ListComp(list_comp) => comprehension::lower_list_comp_inline(self, scope, list_comp),
            Expr::SetComp(set_comp) => comprehension::lower_set_comp_inline(self, scope, set_comp),
            Expr::DictComp(dict_comp) => comprehension::lower_dict_comp_inline(self, scope, dict_comp),
            Expr::Generator(generator) => comprehension::lower_generator_expr(self, scope, generator),
            Expr::Yield(yield_expr) => generator::lower_yield_expr(self, scope, yield_expr),
            Expr::YieldFrom(yield_from) => generator::lower_yield_from_expr(self, scope, yield_from),
            Expr::Await(await_expr) => generator::lower_await_expr(self, scope, await_expr),
            Expr::BoolOp(bool_op) => control::lower_bool_expr_with_driver(self, scope, bool_op),
            Expr::Named(named) => control::lower_named_expr_with_driver(self, scope, named),
            Expr::UnaryOp(unary) => control::lower_unary_expr_with_driver(self, scope, unary),
            Expr::Lambda(lambda) => func::lower_lambda(self, scope, lambda),
            Expr::If(expr_if) => control::lower_if_expr_with_driver(self, scope, expr_if),
            Expr::Dict(dict) => self.lower_dict_expr(scope, dict),
            Expr::Set(set) => self.lower_set_expr(scope, set),
            Expr::Compare(compare) => control::lower_compare_expr_with_driver(self, scope, compare),
            Expr::BytesLiteral(bytes) => self.lower_bytes_literal(scope, bytes),
            Expr::BooleanLiteral(boolean) => scope.emit(InstKind::Const(PyConst::Bool(boolean.value))),
            Expr::EllipsisLiteral(_) => {
                let name_id = self.names.intern("Ellipsis")?;
                scope.emit(InstKind::LoadBuiltin(name_id))
            }
            Expr::Attribute(attr) => self.lower_attribute_expr(scope, attr),
            Expr::Subscript(subscript) => self.lower_subscript_expr(scope, subscript),
            Expr::Starred(_) => unsupported_expr("starred expression outside container literal or call", expr),
            Expr::List(list) => self.lower_list_expr(scope, list),
            Expr::Tuple(tuple) => self.lower_tuple_expr(scope, tuple),
            Expr::Slice(slice) => self.lower_slice_expr(scope, slice),
            Expr::IpyEscapeCommand(_) => unsupported_expr("IPython escape expression", expr),
        }
    }

    fn lower_bytes_literal(
        &mut self,
        scope: &mut BodyScope,
        bytes: &ruff_python_ast::ExprBytesLiteral,
    ) -> Result<Value, LowerError> {
        let mut value = Vec::new();
        for part in bytes.value.iter() {
            value.extend_from_slice(part.as_slice());
        }
        scope.emit(InstKind::Const(PyConst::Bytes(value)))
    }

    fn lower_attribute_expr(
        &mut self,
        scope: &mut BodyScope,
        attr: &ruff_python_ast::ExprAttribute,
    ) -> Result<Value, LowerError> {
        if !matches!(attr.ctx, ExprContext::Load) {
            return unsupported_at(
                "non-load attribute expression",
                span_bounds(attr.range.start().to_u32(), attr.range.end().to_u32()),
            );
        }
        let obj = self.lower_expr(scope, &attr.value)?;
        let name = self.names.intern(attr.attr.as_str())?;
        scope.emit(InstKind::LoadAttr { obj, name })
    }

    fn lower_subscript_expr(
        &mut self,
        scope: &mut BodyScope,
        subscript: &ruff_python_ast::ExprSubscript,
    ) -> Result<Value, LowerError> {
        if !matches!(subscript.ctx, ExprContext::Load) {
            return unsupported_at(
                "non-load subscript expression",
                span_bounds(subscript.range.start().to_u32(), subscript.range.end().to_u32()),
            );
        }
        let obj = self.lower_expr(scope, &subscript.value)?;
        let index = self.lower_expr(scope, &subscript.slice)?;
        scope.emit(InstKind::SubscriptGet { obj, index })
    }

    fn lower_list_expr(
        &mut self,
        scope: &mut BodyScope,
        list: &ruff_python_ast::ExprList,
    ) -> Result<Value, LowerError> {
        if !matches!(list.ctx, ExprContext::Load) {
            return unsupported_at(
                "non-load list expression",
                span_bounds(list.range.start().to_u32(), list.range.end().to_u32()),
            );
        }
        if list.elts.iter().any(|elt| matches!(elt, Expr::Starred(_))) {
            let value = scope.emit(InstKind::BuildList { elts: Vec::new() })?;
            for elt in &list.elts {
                if let Expr::Starred(starred) = elt {
                    let iter = self.lower_expr(scope, &starred.value)?;
                    scope.emit(InstKind::ListExtend { list: value, iter })?;
                } else {
                    let item = self.lower_expr(scope, elt)?;
                    scope.emit(InstKind::ListAppend { list: value, item })?;
                }
            }
            Ok(value)
        } else {
            let mut elts = Vec::with_capacity(list.elts.len());
            for elt in &list.elts {
                elts.push(self.lower_expr(scope, elt)?);
            }
            scope.emit(InstKind::BuildList { elts })
        }
    }

    fn lower_tuple_expr(
        &mut self,
        scope: &mut BodyScope,
        tuple: &ruff_python_ast::ExprTuple,
    ) -> Result<Value, LowerError> {
        if !matches!(tuple.ctx, ExprContext::Load) {
            return unsupported_at(
                "non-load tuple expression",
                span_bounds(tuple.range.start().to_u32(), tuple.range.end().to_u32()),
            );
        }
        if tuple.elts.iter().any(|elt| matches!(elt, Expr::Starred(_))) {
            let list = scope.emit(InstKind::BuildList { elts: Vec::new() })?;
            for elt in &tuple.elts {
                if let Expr::Starred(starred) = elt {
                    let iter = self.lower_expr(scope, &starred.value)?;
                    scope.emit(InstKind::ListExtend { list, iter })?;
                } else {
                    let item = self.lower_expr(scope, elt)?;
                    scope.emit(InstKind::ListAppend { list, item })?;
                }
            }
            return scope.emit(InstKind::ListToTuple { list });
        }
        let mut elts = Vec::with_capacity(tuple.elts.len());
        for elt in &tuple.elts {
            elts.push(self.lower_expr(scope, elt)?);
        }
        scope.emit(InstKind::BuildTuple { elts })
    }

    fn lower_set_expr(
        &mut self,
        scope: &mut BodyScope,
        set: &ruff_python_ast::ExprSet,
    ) -> Result<Value, LowerError> {
        if set.elts.iter().any(|elt| matches!(elt, Expr::Starred(_))) {
            let value = scope.emit(InstKind::BuildSet { elts: Vec::new() })?;
            for elt in &set.elts {
                if let Expr::Starred(starred) = elt {
                    let iter = self.lower_expr(scope, &starred.value)?;
                    scope.emit(InstKind::SetUpdate { set: value, iter })?;
                } else {
                    let item = self.lower_expr(scope, elt)?;
                    scope.emit(InstKind::SetAdd { set: value, item })?;
                }
            }
            return Ok(value);
        }
        let mut elts = Vec::with_capacity(set.elts.len());
        for elt in &set.elts {
            elts.push(self.lower_expr(scope, elt)?);
        }
        scope.emit(InstKind::BuildSet { elts })
    }

    fn lower_dict_expr(
        &mut self,
        scope: &mut BodyScope,
        dict: &ruff_python_ast::ExprDict,
    ) -> Result<Value, LowerError> {
        if dict.items.iter().any(|item| item.key.is_none()) {
            let map = scope.emit(InstKind::BuildMap { pairs: Vec::new() })?;
            for item in &dict.items {
                if let Some(key_expr) = &item.key {
                    let key = self.lower_expr(scope, key_expr)?;
                    let val = self.lower_expr(scope, &item.value)?;
                    scope.emit(InstKind::MapInsert { map, key, val })?;
                } else {
                    let other = self.lower_expr(scope, &item.value)?;
                    scope.emit(InstKind::DictMerge { map, other })?;
                }
            }
            Ok(map)
        } else {
            let mut pairs = Vec::with_capacity(dict.items.len());
            for item in &dict.items {
                let key = item
                    .key
                    .as_ref()
                    .ok_or_else(|| LowerError::internal("dict item key disappeared"))?;
                pairs.push((self.lower_expr(scope, key)?, self.lower_expr(scope, &item.value)?));
            }
            scope.emit(InstKind::BuildMap { pairs })
        }
    }

    fn lower_slice_expr(
        &mut self,
        scope: &mut BodyScope,
        slice: &ruff_python_ast::ExprSlice,
    ) -> Result<Value, LowerError> {
        let lower = self.lower_optional_slice_bound(scope, slice.lower.as_deref())?;
        let upper = self.lower_optional_slice_bound(scope, slice.upper.as_deref())?;
        let step = self.lower_optional_slice_bound(scope, slice.step.as_deref())?;
        scope.emit(InstKind::BuildSlice { lower, upper, step })
    }

    fn lower_optional_slice_bound(
        &mut self,
        scope: &mut BodyScope,
        bound: Option<&Expr>,
    ) -> Result<Value, LowerError> {
        match bound {
            Some(expr) => self.lower_expr(scope, expr),
            None => scope.emit(InstKind::Const(PyConst::None)),
        }
    }

    fn lower_sequence_store_target(
        &mut self,
        scope: &mut BodyScope,
        target: &Expr,
        elts: &[Expr],
        value: Value,
    ) -> Result<(), LowerError> {
        let mut starred_index = None;
        for (index, elt) in elts.iter().enumerate() {
            if matches!(elt, Expr::Starred(_)) {
                if starred_index.replace(index).is_some() {
                    return unsupported_expr("multiple starred assignment targets", target);
                }
            }
        }

        if let Some(starred_index) = starred_index {
            let before = starred_index;
            let after = elts.len() - starred_index - 1;
            // `pon_unpack_ex` returns a fresh tuple: leading items, the
            // middle list, then trailing items.  Targets subscript THAT
            // tuple — never the source object (mappings/enums/generators).
            let unpacked = scope.emit(InstKind::UnpackEx { val: value, before, after })?;
            for (index, elt) in elts[..before].iter().enumerate() {
                let item = self.lower_sequence_item(scope, unpacked, index as i64)?;
                self.lower_store_target(scope, elt, item)?;
            }
            if let Expr::Starred(starred) = &elts[starred_index] {
                let rest = self.lower_sequence_item(scope, unpacked, before as i64)?;
                self.lower_store_target(scope, &starred.value, rest)?;
            }
            for (offset, elt) in elts[starred_index + 1..].iter().enumerate() {
                let index = before + 1 + offset;
                let item = self.lower_sequence_item(scope, unpacked, index as i64)?;
                self.lower_store_target(scope, elt, item)?;
            }
        } else {
            // `pon_unpack_seq` returns a fresh tuple of exactly `n` items.
            let unpacked = scope.emit(InstKind::UnpackSeq {
                val: value,
                n: elts.len(),
            })?;
            for (index, elt) in elts.iter().enumerate() {
                let item = self.lower_sequence_item(scope, unpacked, index as i64)?;
                self.lower_store_target(scope, elt, item)?;
            }
        }
        Ok(())
    }

    fn lower_sequence_item(
        &mut self,
        scope: &mut BodyScope,
        value: Value,
        index: i64,
    ) -> Result<Value, LowerError> {
        let index = scope.emit(InstKind::Const(PyConst::Int(index)))?;
        scope.emit(InstKind::SubscriptGet { obj: value, index })
    }

    /// Slice `value[before..len-after]` into a fresh list.  Used by match
    /// sequence patterns, whose subjects are guaranteed Sequences; plain
    /// unpack assignment reads the `UnpackEx` result tuple instead.
    fn lower_sequence_rest(
        &mut self,
        scope: &mut BodyScope,
        value: Value,
        before: i64,
        after: i64,
    ) -> Result<Value, LowerError> {
        let lower = scope.emit(InstKind::Const(PyConst::Int(before)))?;
        let upper = if after == 0 {
            scope.emit(InstKind::Const(PyConst::None))?
        } else {
            scope.emit(InstKind::Const(PyConst::Int(-after)))?
        };
        let step = scope.emit(InstKind::Const(PyConst::None))?;
        let slice = scope.emit(InstKind::BuildSlice { lower, upper, step })?;
        let values = scope.emit(InstKind::SubscriptGet { obj: value, index: slice })?;
        let list = scope.emit(InstKind::BuildList { elts: Vec::new() })?;
        scope.emit(InstKind::ListExtend { list, iter: values })?;
        Ok(list)
    }

    fn lower_store_target(
        &mut self,
        scope: &mut BodyScope,
        target: &Expr,
        value: Value,
    ) -> Result<(), LowerError> {
        match target {
            Expr::Name(name) if matches!(name.ctx, ExprContext::Store) => {
                self.store_name_value(scope, name.id.as_str(), value)
            }
            Expr::Attribute(attr) if matches!(attr.ctx, ExprContext::Store) => {
                let obj = self.lower_expr(scope, &attr.value)?;
                let name = self.names.intern(attr.attr.as_str())?;
                scope.emit(InstKind::StoreAttr { obj, name, val: value })?;
                Ok(())
            }
            Expr::Subscript(subscript) if matches!(subscript.ctx, ExprContext::Store) => {
                let obj = self.lower_expr(scope, &subscript.value)?;
                let index = self.lower_expr(scope, &subscript.slice)?;
                scope.emit(InstKind::SubscriptSet { obj, index, val: value })?;
                Ok(())
            }
            Expr::List(list) => self.lower_sequence_store_target(scope, target, &list.elts, value),
            Expr::Tuple(tuple) => self.lower_sequence_store_target(scope, target, &tuple.elts, value),
            _ => unsupported_expr("assignment target", target),
        }
    }

    /// Stores `value` under a plain name using the scope's binding class.
    ///
    /// Shared by assignment targets and match-pattern captures, which bind
    /// names without an `Expr::Name` node.
    fn store_name_value(
        &mut self,
        scope: &mut BodyScope,
        raw_name: &str,
        value: Value,
    ) -> Result<(), LowerError> {
        if scope.is_global_name(raw_name) {
            let name_id = self.names.intern(raw_name)?;
            scope.emit(InstKind::StoreGlobal(name_id, value))?;
        } else if scope.is_class() {
            if let Some(cell) = scope.class_deref_cell(raw_name) {
                scope.emit(InstKind::StoreCell(cell, value))?;
            } else {
                let name_id = self.names.intern(raw_name)?;
                scope.emit(InstKind::StoreName(name_id, value))?;
            }
        } else {
            match scope.name_class(raw_name).cloned() {
                Some(NameClass::Cell { cell_slot, local_slot }) => {
                    scope.emit(InstKind::StoreCell(CellId(cell_slot), value))?;
                    scope.mark_local_defined(local_slot);
                }
                Some(NameClass::Free { slot }) => {
                    let cell = scope.free_cell(slot);
                    scope.emit(InstKind::StoreCell(cell, value))?;
                }
                Some(NameClass::Local { slot }) => {
                    scope.emit(InstKind::StoreLocal(LocalId(slot), value))?;
                    scope.mark_local_defined(slot);
                }
                Some(NameClass::Builtin) | Some(NameClass::Global { .. }) | None => {
                    let name_id = self.names.intern(raw_name)?;
                    scope.emit(InstKind::StoreName(name_id, value))?;
                }
            }
        }
        Ok(())
    }
}

pub(crate) struct BodyScope {
    info: ScopeInfo,
    defined_locals: Vec<bool>,
    blocks: Vec<Block>,
    current_id: BlockId,
    insts: Vec<Inst>,
    next_value: u32,
    term: Option<Terminator>,
    next_block: u32,
    temp_locals: usize,
    reraise_exc: Option<LocalId>,
    /// Innermost `try`-phase return trampoline: `return` parks its value in
    /// [`Self::return_slot`] and jumps here so departing edges pop handler
    /// records and run `finally` bodies (pin J0.6 §3.1 exit-edge discipline).
    return_route: Option<BlockId>,
    /// Shared spill slot for routed return values.
    return_slot: Option<LocalId>,
    /// Next J0.3 inline-cache feedback slot (one per specializable site).
    next_feedback: u32,
    /// Source line stamped onto every emitted instruction; `0` when unknown.
    current_line: u32,
}

impl BodyScope {
    fn new(info: &ScopeInfo) -> Self {
        let info = info.clone();
        let defined_locals = info.locals.iter().map(|local| local.is_parameter).collect();
        let mut scope = Self {
            info,
            defined_locals,
            blocks: Vec::new(),
            current_id: BlockId(0),
            insts: Vec::new(),
            next_value: 0,
            term: None,
            next_block: 1,
            temp_locals: 0,
            reraise_exc: None,
            return_route: None,
            return_slot: None,
            next_feedback: 0,
            current_line: 0,
        };
        scope.emit_cell_prologue();
        scope
    }

    fn is_module(&self) -> bool {
        matches!(self.info.kind, ScopeKind::Module)
    }

    fn is_class(&self) -> bool {
        matches!(self.info.kind, ScopeKind::Class)
    }

    fn is_terminated(&self) -> bool {
        self.term.is_some()
    }

    fn local_slot(&self, name: &str) -> Option<LocalId> {
        if self.is_class() {
            None
        } else {
            self.info.local_slot(name).map(LocalId)
        }
    }

    fn is_function_like(&self) -> bool {
        matches!(self.info.kind, ScopeKind::Function | ScopeKind::Comprehension)
    }

    fn mark_local_defined(&mut self, slot: u32) {
        if let Some(defined) = self.defined_locals.get_mut(slot as usize) {
            *defined = true;
        }
    }

    fn locals_snapshot_items(&self) -> Vec<(String, LocalId)> {
        self.info
            .locals
            .iter()
            .filter(|local| {
                self.defined_locals
                    .get(local.slot as usize)
                    .copied()
                    .unwrap_or(false)
            })
            .map(|local| (local.name.clone(), LocalId(local.slot)))
            .collect()
    }

    fn name_class(&self, name: &str) -> Option<&NameClass> {
        self.info.symbol(name).map(|symbol| &symbol.class)
    }

    fn alloc_temp_local(&mut self) -> LocalId {
        let slot = self.info.locals.len() + self.temp_locals;
        self.temp_locals += 1;
        LocalId(slot as u32)
    }

    /// Lazily allocates the shared spill slot used by routed `return`s.
    fn ensure_return_slot(&mut self) -> LocalId {
        if let Some(slot) = self.return_slot {
            return slot;
        }
        let slot = self.alloc_temp_local();
        self.return_slot = Some(slot);
        slot
    }

    fn emit_cell_prologue(&mut self) {
        for index in 0..self.info.cell_vars.len() {
            let known_local = match self.name_class(&self.info.cell_vars[index]) {
                Some(NameClass::Cell { local_slot, .. }) => Some(*local_slot),
                _ => None,
            };
            let (local_slot, is_parameter) = match known_local {
                Some(local_slot) => {
                    let is_parameter = self
                        .info
                        .symbol(&self.info.cell_vars[index])
                        .is_some_and(|symbol| symbol.is_parameter);
                    (local_slot, is_parameter)
                }
                // The implicit class `__class__` cell has no symbols entry:
                // back it with a fresh None-initialized temp slot so
                // `MakeCell` has a local to promote.  `__build_class__`
                // fills the cell with the class object at construction via
                // the `__classcell__` protocol (see `lower_class_def`).
                None if self.info.needs_class_closure
                    && self.info.cell_vars[index] == "__class__" =>
                {
                    (self.alloc_temp_local().0, false)
                }
                None => continue,
            };
            if !is_parameter {
                let none = Value(self.next_value);
                self.next_value = self
                    .next_value
                    .checked_add(1)
                    .expect("too many SSA values for u32 ids");
                self.insts.push(Inst::new(none, InstKind::Const(PyConst::None)));
                let store = Value(self.next_value);
                self.next_value = self
                    .next_value
                    .checked_add(1)
                    .expect("too many SSA values for u32 ids");
                self.insts
                    .push(Inst::new(store, InstKind::StoreLocal(LocalId(local_slot), none)));
            }
            let result = Value(self.next_value);
            self.next_value = self
                .next_value
                .checked_add(1)
                .expect("too many SSA values for u32 ids");
            self.insts.push(Inst::new(result, InstKind::MakeCell(LocalId(local_slot))));
        }
    }

    fn closure_slot(&self, name: &str) -> Option<CellId> {
        // The implicit class cell is always the class body's first (and
        // only) own cell; it deliberately has no symbols entry so class-body
        // source accesses to `__class__` keep namespace semantics.
        if self.info.needs_class_closure && name == "__class__" {
            return Some(CellId(0));
        }
        match self.name_class(name)? {
            NameClass::Cell { cell_slot, .. } => Some(CellId(*cell_slot)),
            NameClass::Free { slot } => Some(self.free_cell(*slot)),
            NameClass::Local { .. } | NameClass::Global { .. } | NameClass::Builtin => None,
        }
    }

    /// Cell id for one of this function's free variables.
    ///
    /// Cell ids form a single per-function space: `0..cell_vars.len()` are
    /// the function's own `MakeCell` results; closure cells received from the
    /// enclosing function follow at `cell_vars.len()..`.
    fn free_cell(&self, slot: u32) -> CellId {
        CellId(self.info.cell_vars.len() as u32 + slot)
    }

    /// For class scopes: the closure cell a name access must use instead of
    /// the namespace — the name is captured from an enclosing function and is
    /// either never bound in the class body or explicitly declared `nonlocal`.
    fn class_deref_cell(&self, name: &str) -> Option<CellId> {
        let symbol = self.info.symbol(name)?;
        let NameClass::Free { slot } = symbol.class else {
            return None;
        };
        if symbol.is_bound && !symbol.is_nonlocal {
            return None;
        }
        Some(self.free_cell(slot))
    }

    fn cell_slot_for_local(&self, local: LocalId) -> Option<CellId> {
        for index in 0..self.info.cell_vars.len() {
            let name = &self.info.cell_vars[index];
            if let Some(NameClass::Cell {
                local_slot,
                cell_slot,
            }) = self.name_class(name)
            {
                if *local_slot == local.0 {
                    return Some(CellId(*cell_slot));
                }
            }
        }
        None
    }

    fn rewrite_cell_local_access(&self, kind: InstKind) -> InstKind {
        match kind {
            InstKind::LoadLocal(local) => {
                if let Some(cell) = self.cell_slot_for_local(local) {
                    InstKind::LoadCell(cell)
                } else {
                    InstKind::LoadLocal(local)
                }
            }
            InstKind::StoreLocal(local, value) => {
                if let Some(cell) = self.cell_slot_for_local(local) {
                    InstKind::StoreCell(cell, value)
                } else {
                    InstKind::StoreLocal(local, value)
                }
            }
            InstKind::DeleteLocal(local) => {
                if let Some(cell) = self.cell_slot_for_local(local) {
                    InstKind::DeleteCell(cell)
                } else {
                    InstKind::DeleteLocal(local)
                }
            }
            kind => kind,
        }
    }

    /// Claims the child scope discovered for the construct at `span`.
    ///
    /// Pairing is keyed on (kind, name, span) rather than discovery order:
    /// lowering visits `try` clauses in control-flow order (`else` before the
    /// handlers) and inlines `finally` bodies once per departing edge, so
    /// positional claiming cross-pairs same-named siblings and fails outright
    /// on re-lowered statements.  Span keys make claims order-independent and
    /// idempotent.  Synthesized scopes with no defining source construct (the
    /// merged namespace `__annotate__` child) carry — and are claimed with —
    /// `None`.
    fn next_child_scope(
        &self,
        kind: ScopeKind,
        name: &str,
        span: Option<(u32, u32)>,
    ) -> Result<ScopeInfo, LowerError> {
        self.info
            .children
            .iter()
            .find(|child| child.kind == kind && child.name == name && child.span == span)
            .cloned()
            .ok_or_else(|| {
                LowerError::internal(format!("scope metadata was not discovered for {name}"))
            })
    }

    fn is_global_name(&self, name: &str) -> bool {
        self.is_module()
            || matches!(
                self.info.symbol(name).map(|symbol| &symbol.class),
                Some(NameClass::Global { explicit: true })
            )
    }

    fn alloc_block(&mut self) -> Result<BlockId, LowerError> {
        let id = BlockId(self.next_block);
        self.next_block = self
            .next_block
            .checked_add(1)
            .ok_or_else(|| LowerError::internal("too many basic blocks for u32 ids"))?;
        Ok(id)
    }

    #[track_caller]
    fn switch_to(&mut self, id: BlockId) -> Result<(), LowerError> {
        let caller = std::panic::Location::caller();
        let term = self.term.take().ok_or_else(|| {
            LowerError::internal(format!(
                "switching away from unterminated block (block {} -> {} at {}:{})",
                self.current_id.0,
                id.0,
                caller.file(),
                caller.line()
            ))
        })?;
        let insts = std::mem::take(&mut self.insts);
        self.blocks.push(Block {
            id: self.current_id,
            insts,
            term,
        });
        self.current_id = id;
        Ok(())
    }

    fn jump_if_open(&mut self, target: BlockId) -> Result<(), LowerError> {
        if self.term.is_none() {
            self.set_term(Terminator::Jump(target))?;
        }
        Ok(())
    }

    fn emit(&mut self, kind: InstKind) -> Result<Value, LowerError> {
        if self.is_terminated() {
            return Err(LowerError::unsupported("instruction after terminator"));
        }

        let kind = self.rewrite_cell_local_access(kind);
        let result = Value(self.next_value);
        self.next_value = self
            .next_value
            .checked_add(1)
            .ok_or_else(|| LowerError::internal("too many SSA values for u32 ids"))?;
        let mut inst = Inst::new(result, kind);
        if let Some(slot) = self.reserve_feedback_slot(&inst.kind)? {
            inst = inst.with_feedback_slot(slot);
        }
        inst.line = self.current_line;
        self.insts.push(inst);
        Ok(result)
    }

    /// J0.3: reserve one feedback slot per specializable operation.  The IR
    /// op kind statically fixes the cell interpretation (`FeedbackKind`
    /// contract): attribute/method loads get Attr cells, global loads get
    /// Global cells, method/extended calls get Call cells.  Plain `Call`
    /// stays slot-free this wave (its helper has no feedback parameter).
    fn reserve_feedback_slot(&mut self, kind: &InstKind) -> Result<Option<FeedbackSlot>, LowerError> {
        let wants_slot = matches!(
            kind,
            InstKind::LoadAttr { .. }
                | InstKind::LoadMethod { .. }
                | InstKind::LoadGlobal(_)
                | InstKind::CallMethod { .. }
                | InstKind::CallEx { .. }
        );
        if !wants_slot {
            return Ok(None);
        }
        let slot = FeedbackSlot(self.next_feedback);
        self.next_feedback = self
            .next_feedback
            .checked_add(1)
            .ok_or_else(|| LowerError::internal("too many feedback slots for u32 ids"))?;
        Ok(Some(slot))
    }

    fn set_term(&mut self, term: Terminator) -> Result<(), LowerError> {
        if self.term.is_some() {
            return Err(LowerError::unsupported("second terminator in block"));
        }
        self.term = Some(term);
        Ok(())
    }

    fn finish(mut self) -> Result<Function, LowerError> {
        if self.term.is_none() {
            let none = self.emit(InstKind::Const(PyConst::None))?;
            self.term = Some(Terminator::Return(none));
        }

        let term = self
            .term
            .take()
            .ok_or_else(|| LowerError::internal("finished function without terminator"))?;
        self.blocks.push(Block {
            id: self.current_id,
            insts: self.insts,
            term,
        });
        let params = param_layout(&self.info);
        let is_generator_body = self.info.is_generator || self.info.is_async;
        // PEP 525: `async def` with `yield` is an async generator function, not
        // a coroutine function — calls return an async-generator object and the
        // call site must not await it.
        let is_async_generator = self.info.is_generator && self.info.is_async;
        let mut function = Function {
            name: self.info.name,
            arity: self.info.parameters.arity(),
            is_coroutine: self.info.is_async && !self.info.is_generator,
            is_generator: is_generator_body,
            is_async_generator,
            params,
            blocks: self.blocks,
            n_locals: self.info.locals.len() + self.temp_locals,
        };
        if is_generator_body {
            generator::transform_generator_function(&mut function)?;
        }
        Ok(function)
    }
}

fn param_layout(info: &ScopeInfo) -> ParamLayout {
    let positional = info.parameters.arity();
    let has_vararg = info.parameters.has_vararg;
    let keyword_only = info.parameters.keyword_only;
    let has_kwarg = info.parameters.has_kwarg;

    let mut names = Vec::with_capacity(positional + keyword_only);
    for local in info.locals.iter().take(positional) {
        names.push(local.name.clone());
    }
    let keyword_start = positional + usize::from(has_vararg);
    for local in info.locals.iter().skip(keyword_start).take(keyword_only) {
        names.push(local.name.clone());
    }

    ParamLayout {
        names,
        positional_only_count: info.parameters.positional_only,
        positional_count: positional.saturating_sub(info.parameters.positional_only),
        keyword_only_count: keyword_only,
        vararg_name: has_vararg
            .then(|| info.locals.get(positional).map(|local| local.name.clone()))
            .flatten(),
        kwarg_name: has_kwarg
            .then(|| {
                info.locals
                    .get(positional + usize::from(has_vararg) + keyword_only)
                    .map(|local| local.name.clone())
            })
            .flatten(),
    }
}

fn validate_function_header(def: &StmtFunctionDef) -> Result<(), LowerError> {
    if !def.decorator_list.is_empty() {
        return unsupported_at("function decorator", span_function(def));
    }
    if def.type_params.is_some() {
        return unsupported_at("function type parameter", span_function(def));
    }
    if def.returns.is_some() {
        return unsupported_at("function return annotation", span_function(def));
    }
    positional_parameters(&def.parameters)?;
    Ok(())
}

fn positional_parameters(parameters: &Parameters) -> Result<Vec<String>, LowerError> {
    if parameters.vararg.is_some() {
        return Err(LowerError::unsupported("variadic positional parameter"));
    }
    if !parameters.kwonlyargs.is_empty() {
        return Err(LowerError::unsupported("keyword-only parameter"));
    }
    if parameters.kwarg.is_some() {
        return Err(LowerError::unsupported("variadic keyword parameter"));
    }

    let mut params = Vec::with_capacity(parameters.posonlyargs.len() + parameters.args.len());
    for parameter in parameters.posonlyargs.iter().chain(&parameters.args) {
        if parameter.default().is_some() {
            return Err(LowerError::unsupported("default parameter value"));
        }
        if parameter.annotation().is_some() {
            return Err(LowerError::unsupported("parameter annotation"));
        }
        params.push(parameter.name().as_str().to_owned());
    }
    Ok(params)
}

fn unsupported_at<T>(feature: impl Into<String>, span: SourceSpan) -> Result<T, LowerError> {
    Err(LowerError::unsupported_at(feature, span))
}

fn unsupported_expr<T>(feature: impl Into<String>, expr: &Expr) -> Result<T, LowerError> {
    Err(LowerError::unsupported_at(feature, span_expr(expr)))
}

fn span_function(def: &StmtFunctionDef) -> SourceSpan {
    span_bounds(def.range.start().to_u32(), def.range.end().to_u32())
}

fn span_bounds(start: u32, end: u32) -> SourceSpan {
    SourceSpan::from_bounds(start, end)
}

/// Byte offsets of each 1-based line start, for statement line lookups.
fn compute_line_starts(source: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    for (index, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            // Ruff AST offsets are u32 by construction, so `index` fits.
            starts.push(index as u32 + 1);
        }
    }
    starts
}

fn span_expr(expr: &Expr) -> SourceSpan {
    match expr {
        Expr::BoolOp(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::Named(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::BinOp(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::UnaryOp(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::Lambda(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::If(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::Dict(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::Set(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::ListComp(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::SetComp(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::DictComp(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::Generator(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::Await(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::Yield(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::YieldFrom(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::Compare(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::Call(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::FString(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::TString(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::StringLiteral(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::BytesLiteral(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::NumberLiteral(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::BooleanLiteral(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::NoneLiteral(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::EllipsisLiteral(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::Attribute(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::Subscript(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::Starred(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::Name(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::List(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::Tuple(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::Slice(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
        Expr::IpyEscapeCommand(node) => span_bounds(node.range.start().to_u32(), node.range.end().to_u32()),
    }
}

mod assign;
mod func;
pub(crate) mod synth;
mod control;
mod class;
mod strings;
mod match_;
mod try_;
mod generator;
mod comprehension;
mod with_;
mod import;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{BinOp, InstKind, LocalId, NameId};

    fn insts_of(function: &Function) -> impl Iterator<Item = &Inst> + '_ {
        function.blocks.iter().flat_map(|block| &block.insts)
    }

    #[test]
    fn lowers_phase_a_hello_shape() {
        let module = lower_source(
            r#"
def add(a, b):
    return a + b

print("hello", add(1, 2))
"#,
        )
        .expect("Phase-A hello source should lower");

        assert_eq!(module.main, FunctionId(0));
        assert_eq!(module.names, vec!["add".to_owned(), "print".to_owned()]);
        assert_eq!(module.functions.len(), 2);

        let main = &module.functions[module.main.0 as usize];
        assert_eq!(main.name, "__main__");
        assert_eq!(main.arity, 0);
        assert_eq!(main.n_locals, 0);
        assert_eq!(main.blocks.len(), 1);
        let main_block = &main.blocks[0];
        assert_eq!(main_block.term, Terminator::Return(Value(9)));
        assert!(matches!(
            main_block.insts[0].kind,
            InstKind::MakeFunctionFull {
                code: FunctionId(1),
                ref defaults,
                ref kwdefaults,
                ref closure,
                ref annotations,
            } if defaults.is_empty() && kwdefaults.is_empty() && closure.is_empty() && annotations.is_empty()
        ));
        assert_eq!(main_block.insts[1].kind, InstKind::StoreGlobal(NameId(0), Value(0)));
        assert_eq!(main_block.insts[2].kind, InstKind::LoadBuiltin(NameId(1)));
        assert_eq!(
            main_block.insts[3].kind,
            InstKind::Const(PyConst::Str("hello".to_owned()))
        );
        // Module-level function reads use the normal global lookup path; class-body
        // namespace reads are the cases that deliberately lower through LoadName.
        assert_eq!(main_block.insts[4].kind, InstKind::LoadGlobal(NameId(0)));
        assert_eq!(main_block.insts[5].kind, InstKind::Const(PyConst::Int(1)));
        assert_eq!(main_block.insts[6].kind, InstKind::Const(PyConst::Int(2)));
        assert_eq!(
            main_block.insts[7].kind,
            InstKind::Call {
                callee: Value(4),
                args: vec![Value(5), Value(6)]
            }
        );
        assert_eq!(
            main_block.insts[8].kind,
            InstKind::Call {
                callee: Value(2),
                args: vec![Value(3), Value(7)]
            }
        );
        assert_eq!(main_block.insts[9].kind, InstKind::Const(PyConst::None));

        let add = &module.functions[1];
        assert_eq!(add.name, "add");
        assert_eq!(add.arity, 2);
        assert_eq!(add.n_locals, 2);
        assert_eq!(add.blocks.len(), 1);
        let add_block = &add.blocks[0];
        assert_eq!(add_block.insts[0].kind, InstKind::LoadLocal(LocalId(0)));
        assert_eq!(add_block.insts[1].kind, InstKind::LoadLocal(LocalId(1)));
        assert_eq!(
            add_block.insts[2].kind,
            InstKind::BinaryOp {
                op: BinOp::Add,
                lhs: Value(0),
                rhs: Value(1)
            }
        );
        assert_eq!(add_block.term, Terminator::Return(Value(2)));
    }

    #[test]
    fn nested_closure_uses_full_function_shape_without_local_name_pollution() {
        let module = lower_source(
            r#"
def outer(x):
    def inner(y):
        return x + y
    return inner
"#,
        )
        .expect("nested closure source should lower");

        assert_eq!(
            module.names,
            vec!["outer".to_owned()],
            "purely local closure function and parameter names should not be interned"
        );

        let outer = module
            .functions
            .iter()
            .find(|function| function.name == "outer")
            .expect("outer function should be lowered");
        assert!(
            outer.blocks[0]
                .insts
                .iter()
                .any(|inst| inst.kind == InstKind::MakeCell(LocalId(0))),
            "outer should promote captured x from local slot 0 into a cell"
        );
        let inner_constructor = outer.blocks[0]
            .insts
            .iter()
            .find(|inst| matches!(
                &inst.kind,
                InstKind::MakeFunctionFull { closure, .. }
                    if closure.as_slice() == &[CellId(0)][..]
            ))
            .expect("outer should construct inner with the captured x cell");
        let InstKind::MakeFunctionFull {
            code,
            defaults,
            kwdefaults,
            closure,
            annotations,
        } = &inner_constructor.kind
        else {
            unreachable!("inner constructor was selected by MakeFunctionFull shape");
        };
        assert_eq!(closure.as_slice(), &[CellId(0)][..]);
        assert!(
            defaults.is_empty(),
            "plain closure should not synthesize positional defaults"
        );
        assert!(
            kwdefaults.is_empty(),
            "plain closure should not synthesize keyword defaults"
        );
        assert!(
            annotations.is_empty(),
            "plain closure should not synthesize annotations"
        );

        let inner = &module.functions[code.0 as usize];
        assert_eq!(inner.name, "inner");
        assert_eq!(inner.arity, 1);

        let inner_block = &inner.blocks[0];
        let captured_x = inner_block
            .insts
            .iter()
            .find(|inst| inst.kind == InstKind::LoadCell(CellId(0)))
            .expect("inner should read captured x from closure cell 0");
        let parameter_y = inner_block
            .insts
            .iter()
            .find(|inst| inst.kind == InstKind::LoadLocal(LocalId(0)))
            .expect("inner should read y from local slot 0");
        let add = inner_block
            .insts
            .iter()
            .find(|inst| matches!(
                &inst.kind,
                InstKind::BinaryOp {
                    op: BinOp::Add,
                    lhs,
                    rhs,
                } if *lhs == captured_x.result && *rhs == parameter_y.result
            ))
            .expect("inner should add captured x to local y");
        assert_eq!(inner_block.term, Terminator::Return(add.result));
    }

    #[test]
    fn lowers_nested_filtered_list_comprehension_shape() {
        let module = lower_source(
            r#"
result = [(i, j) for i in range(3) if i for j in range(i) if j]
"#,
        )
        .expect("nested filtered list comprehension should lower");

        let listcomp = module
            .functions
            .iter()
            .find(|function| function.name == "<listcomp>")
            .expect("list comprehension should synthesize a child function");
        assert_eq!(listcomp.arity, 1);
        assert!(listcomp
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|inst| matches!(inst.kind, InstKind::BuildList { .. })));
        assert!(listcomp
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|inst| matches!(inst.kind, InstKind::ListAppend { .. })));
        assert_eq!(
            listcomp
                .blocks
                .iter()
                .filter(|block| matches!(block.term, Terminator::ForLoop { .. }))
                .count(),
            2,
            "two generator clauses should lower to two iterator loops"
        );
        assert_eq!(
            listcomp
                .blocks
                .iter()
                .filter(|block| matches!(block.term, Terminator::CondBranch { .. }))
                .count(),
            2,
            "two filters should lower to two guard branches"
        );
    }

    #[test]
    fn lowers_generator_expression_as_generator_function() {
        let module = lower_source(
            r#"
g = (i for i in range(3))
"#,
        )
        .expect("generator expression should lower");

        let main = &module.functions[module.main.0 as usize];
        assert!(main.blocks[0].insts.iter().any(|inst| match inst.kind {
            InstKind::MakeFunctionFull { code, .. } => module.functions[code.0 as usize].name == "<genexpr>",
            InstKind::MakeFunction { func_index, .. } => module.functions[func_index as usize].name == "<genexpr>",
            _ => false,
        }));
        assert!(main.blocks[0]
            .insts
            .iter()
            .any(|inst| matches!(inst.kind, InstKind::Call { ref args, .. } if args.len() == 1)));

        let genexpr = module
            .functions
            .iter()
            .find(|function| function.name == "<genexpr>")
            .expect("generator expression should synthesize a child function");
        assert_eq!(genexpr.arity, 1);
        assert!(genexpr.is_generator, "genexpr body must be a generator state machine");
        assert!(genexpr
            .blocks
            .iter()
            .any(|block| matches!(block.term, Terminator::Suspend { state: 1, .. })));
    }

    #[test]
    fn comprehension_target_does_not_clobber_enclosing_local() {
        let module = lower_source(
            r#"
def f():
    x = 5
    y = [x for x in range(3)]
    return x
"#,
        )
        .expect("target-name isolation fixture should lower");

        let outer = module
            .functions
            .iter()
            .find(|function| function.name == "f")
            .expect("outer function should lower");
        let Terminator::Return(ret) = outer.blocks[0].term else {
            panic!("outer function should return directly");
        };
        assert_eq!(
            outer.blocks[0].insts[ret.0 as usize].kind,
            InstKind::LoadLocal(LocalId(0)),
            "outer return should reload the enclosing x local"
        );

        let listcomp = module
            .functions
            .iter()
            .find(|function| function.name == "<listcomp>")
            .expect("list comprehension should lower");
        assert!(listcomp
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|inst| matches!(inst.kind, InstKind::StoreLocal(LocalId(1), _))));
    }

    #[test]
    fn lowers_async_function_def_as_coroutine() {
        let module = lower_source(
            r#"
async def f():
    return 1
"#,
        )
        .expect("async functions should lower into coroutine-producing functions");

        let function = module
            .functions
            .iter()
            .find(|function| function.name == "f")
            .expect("async function body should be present");
        assert!(function.is_coroutine);
        assert!(
            function.is_generator,
            "async bodies must be resumable state machines"
        );
        assert!(
            function
                .blocks
                .first()
                .is_some_and(|block| matches!(block.insts.first().map(|inst| &inst.kind), Some(InstKind::GenResumePayload))),
            "generator-family entry must consume the resume payload before user code"
        );
        assert!(function
            .blocks
            .iter()
            .any(|block| matches!(block.term, Terminator::Return(_))));
    }

    #[test]
    fn lowers_function_locals_call_to_snapshot_dict() {
        let module = lower_source(
            r#"
def f(a):
    b = 3
    return locals()
"#,
        )
        .expect("function locals() should lower to a snapshot dict");

        let function = module
            .functions
            .iter()
            .find(|function| function.name == "f")
            .expect("function should lower");
        let block = &function.blocks[0];
        assert!(
            block
                .insts
                .iter()
                .any(|inst| matches!(&inst.kind, InstKind::BuildMap { pairs } if pairs.len() == 2)),
            "locals() should snapshot the defined parameter and local"
        );
        assert!(
            !block.insts.iter().any(|inst| {
                matches!(&inst.kind, InstKind::LoadBuiltin(name) if module.names[name.0 as usize] == "locals")
            }),
            "function-scope locals() must not call the module-scope builtin"
        );
    }

    #[test]
    fn scans_direct_dynamic_code_sinks_with_spans() {
        let sinks = scan_dynamic_sinks_source(
            r#"
def f(src):
    return eval(src)

exec("x = 1")
compile("1 + 1", "<test>", "eval")
"#,
        )
        .expect("dynamic sink scanning should parse valid Python");

        assert_eq!(
            sinks.iter().map(|sink| sink.kind).collect::<Vec<_>>(),
            vec![DynamicCodeKind::Eval, DynamicCodeKind::Exec, DynamicCodeKind::Compile]
        );
        assert!(sinks.iter().all(|sink| sink.span.start < sink.span.end));
    }

    #[test]
    fn stamps_statement_lines_on_lowered_instructions() {
        let source = r#"
x = 1
y = x + 1

def f(a):
    b = a * 2
    return b

f(y)
"#;
        let module = lower_source(source).expect("line-stamp source should lower");

        let dedup_lines = |function: &Function| -> Vec<u32> {
            let mut lines = Vec::new();
            for inst in function.blocks.iter().flat_map(|block| &block.insts) {
                if lines.last() != Some(&inst.line) {
                    lines.push(inst.line);
                }
            }
            lines
        };

        // One transition per statement; the implicit module return keeps the
        // final statement's stamp instead of resetting to 0.
        let main = &module.functions[module.main.0 as usize];
        assert_eq!(dedup_lines(main), vec![2, 3, 5, 9]);

        let f = &module.functions[1];
        assert_eq!(f.name, "f");
        assert_eq!(dedup_lines(f), vec![6, 7]);

        // Lowering a bare AST without source text stamps no lines at all.
        let parsed = parse_module_source(source).expect("line-stamp source should parse");
        let bare = lower_module(&parsed, None).expect("bare AST should lower");
        assert!(
            bare.functions
                .iter()
                .flat_map(|function| &function.blocks)
                .flat_map(|block| &block.insts)
                .all(|inst| inst.line == 0),
            "sourceless lowering must stamp line 0 everywhere"
        );
    }

    #[test]
    fn lowers_async_list_comprehension_with_async_iteration_protocol() {
        let module = lower_source(
            r#"
async def f(src):
    return [x * 2 async for x in src]
"#,
        )
        .expect("async list comprehension inside async def should lower");

        let listcomp = module
            .functions
            .iter()
            .find(|function| function.name == "<listcomp>")
            .expect("async comprehension should synthesize a child function");
        assert!(
            listcomp.is_coroutine,
            "an async comprehension body must run as a coroutine"
        );
        assert!(
            listcomp.is_generator,
            "an async comprehension body must be a resumable state machine"
        );

        let outer = module
            .functions
            .iter()
            .find(|function| function.name == "f")
            .expect("enclosing async function should lower");
        assert!(
            insts_of(outer).any(|inst| matches!(inst.kind, InstKind::GetAIter { .. })),
            "an async first clause must acquire the outer iterator with GetAIter in the enclosing scope"
        );
        assert!(
            insts_of(outer).any(|inst| matches!(inst.kind, InstKind::Await { .. })),
            "the enclosing scope must await the comprehension coroutine call"
        );

        let handler_block = listcomp
            .blocks
            .iter()
            .find(|block| {
                block
                    .insts
                    .iter()
                    .any(|inst| matches!(inst.kind, InstKind::MatchExc { .. }))
            })
            .expect("the async advance must lower a protected handler with an exception match");
        let stop_load = handler_block
            .insts
            .iter()
            .find(|inst| matches!(
                &inst.kind,
                InstKind::LoadBuiltin(name) if module.names[name.0 as usize] == "StopAsyncIteration"
            ))
            .expect("the handler block must load the StopAsyncIteration builtin");
        assert!(
            handler_block.insts.iter().any(|inst| matches!(
                &inst.kind,
                InstKind::MatchExc { exc_type } if *exc_type == stop_load.result
            )),
            "the handler must match the active exception against StopAsyncIteration"
        );
    }

    #[test]
    fn async_comprehension_with_sync_first_clause_uses_sync_outer_iterator() {
        let module = lower_source(
            r#"
async def f(src, g):
    return [await g(x) for x in src]
"#,
        )
        .expect("await inside a sync-clause comprehension should lower");

        let outer = module
            .functions
            .iter()
            .find(|function| function.name == "f")
            .expect("enclosing async function should lower");
        assert!(
            insts_of(outer).any(|inst| matches!(inst.kind, InstKind::GetIter { .. })),
            "a sync first clause must acquire the outer iterator with GetIter"
        );
        assert!(
            !insts_of(outer).any(|inst| matches!(inst.kind, InstKind::GetAIter { .. })),
            "a sync first clause must not acquire an async iterator"
        );

        let listcomp = module
            .functions
            .iter()
            .find(|function| function.name == "<listcomp>")
            .expect("comprehension should synthesize a child function");
        assert!(
            listcomp.is_coroutine,
            "await inside the comprehension body must make the child a coroutine"
        );
    }

    #[test]
    fn lowers_async_for_with_stop_async_iteration_handler() {
        let module = lower_source(
            r#"
async def f(src):
    async for x in src:
        pass

def g(src):
    for x in src:
        pass
"#,
        )
        .expect("async for inside async def should lower");

        let async_fn = module
            .functions
            .iter()
            .find(|function| function.name == "f")
            .expect("async function should lower");
        assert!(
            insts_of(async_fn).any(|inst| matches!(inst.kind, InstKind::GetAIter { .. })),
            "async for must acquire its iterator with GetAIter"
        );
        assert!(
            insts_of(async_fn).any(|inst| matches!(inst.kind, InstKind::MatchExc { .. })),
            "the async advance must lower a protected handler with an exception match"
        );

        let sync_fn = module
            .functions
            .iter()
            .find(|function| function.name == "g")
            .expect("sync control function should lower");
        assert!(
            !insts_of(sync_fn).any(|inst| matches!(
                inst.kind,
                InstKind::GetAIter { .. } | InstKind::MatchExc { .. } | InstKind::Await { .. }
            )),
            "a plain for loop must not lower any async-iteration protocol"
        );
    }

    #[test]
    fn rejects_async_constructs_in_unsupported_scopes_with_exact_features() {
        let cases = [
            (
                "async def f(y):\n    yield from y\n",
                "'yield from' inside async function",
            ),
            (
                "async def f(y):\n    yield 1\n    return 2\n",
                "'return' with value in async generator",
            ),
            (
                "def f(y):\n    async for x in y:\n        pass\n",
                "'async for' outside async function",
            ),
            (
                "def f(y):\n    return [x async for x in y]\n",
                "asynchronous comprehension outside of an asynchronous function",
            ),
        ];
        for (source, expected_feature) in cases {
            let err = lower_source(source)
                .expect_err("source should be rejected during lowering");
            match err {
                LowerError::Unsupported { feature, .. } => assert_eq!(
                    feature, expected_feature,
                    "wrong unsupported-feature message for source:\n{source}"
                ),
                other => panic!("expected Unsupported for source:\n{source}\ngot {other:?}"),
            }
        }
    }

    #[test]
    fn lowers_async_generator_expression_without_awaiting_the_call() {
        let module = lower_source(
            r#"
async def f(y):
    return (x async for x in y)

def s(y):
    return (x async for x in y)
"#,
        )
        .expect("async generator expressions should lower in async and sync scopes");

        let genexprs: Vec<_> = module
            .functions
            .iter()
            .filter(|function| function.name == "<genexpr>")
            .collect();
        assert_eq!(genexprs.len(), 2, "each genexpr should synthesize a child function");
        for genexpr in genexprs {
            assert!(
                genexpr.is_async_generator,
                "an async genexpr child must be an async generator"
            );
            assert!(genexpr.is_generator, "the child must be a resumable state machine");
            assert!(
                !genexpr.is_coroutine,
                "an async generator is not a coroutine: its call site must not await it"
            );
        }
        for name in ["f", "s"] {
            let enclosing = module
                .functions
                .iter()
                .find(|function| function.name == name)
                .expect("enclosing function should lower");
            assert!(
                insts_of(enclosing).any(|inst| matches!(inst.kind, InstKind::GetAIter { .. })),
                "the async first clause acquires the outer iterator with GetAIter in `{name}`"
            );
            assert!(
                !insts_of(enclosing).any(|inst| matches!(inst.kind, InstKind::Await { .. })),
                "constructing the async generator must not be awaited in `{name}`"
            );
        }
    }

    #[test]
    fn async_def_with_yield_lowers_as_async_generator_function() {
        let module = lower_source(
            r#"
async def agen(y):
    got = await y
    yield got

async def coro(y):
    return await y
"#,
        )
        .expect("async generator functions should lower");

        let agen = module
            .functions
            .iter()
            .find(|function| function.name == "agen")
            .expect("async generator function should lower");
        assert!(agen.is_async_generator);
        assert!(agen.is_generator);
        assert!(!agen.is_coroutine);

        let coro = module
            .functions
            .iter()
            .find(|function| function.name == "coro")
            .expect("plain coroutine function should lower");
        assert!(coro.is_coroutine);
        assert!(!coro.is_async_generator);
        assert!(coro.is_generator, "async bodies stay resumable state machines");
    }

    #[test]
    fn nested_async_comprehension_marks_enclosing_comprehension_coroutine() {
        let module = lower_source(
            r#"
async def f(xs, ys):
    return [[y async for y in ys] for x in xs]
"#,
        )
        .expect("nested async comprehension inside async def should lower");

        let listcomps: Vec<_> = module
            .functions
            .iter()
            .filter(|function| function.name == "<listcomp>")
            .collect();
        assert_eq!(
            listcomps.len(),
            2,
            "both comprehension levels should synthesize child functions"
        );
        for listcomp in listcomps {
            assert!(
                listcomp.is_coroutine,
                "a nested async comprehension must mark the enclosing comprehension scope async"
            );
        }
    }
}

