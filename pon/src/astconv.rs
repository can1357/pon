//! Ruff-AST → `_ast` neutral-tree conversion behind the dynexec
//! [`DynAstParseHook`](pon_runtime::dynexec::DynAstParseHook), serving
//! `compile(source, filename, mode, PyCF_ONLY_AST)` — i.e. `ast.parse`.
//!
//! The converter parses with the same pinned options as `pon_ir::parse`
//! (PY314) and maps every ruff statement/expression/pattern/type-param node
//! onto the CPython 3.14 `_ast` schema as pure data
//! ([`AstNode`]/[`AstValue`]); `pon_runtime::native::build_ast_object`
//! materializes the instances.  Ruff-vs-CPython shape differences reconciled
//! here:
//!
//! - split literal nodes (`StringLiteral`, `NumberLiteral`, ...) → `Constant`
//!   (with `kind='u'` recovered from the source prefix);
//! - flattened `elif` clauses → nested `If` in `orelse`, where each nested
//!   `If` spans clause start → outer-statement end (the CPython tail shape);
//! - `is_async`-collapsed defs/loops/withs → `Async*` classes;
//! - per-parameter defaults → the `arguments` `defaults`/`kw_defaults` split;
//! - f/t-string parts → `JoinedStr`/`TemplateStr` with adjacent literal runs
//!   merged into single `Constant`s and `f"{x=}"` debug text re-expanded;
//! - `Try.is_star` → `TryStar`.
//!
//! Locations are best-effort from ruff byte ranges via a line-start table
//! (1-based `lineno`, 0-based UTF-8 byte `col_offset` — CPython's own
//! convention): decorated/async `def`/`class` starts are re-anchored past the
//! decorator list onto the keyword, mirroring CPython.  `type_comment`
//! fields are always `None` (ruff does not parse type comments; matches
//! CPython without `PyCF_TYPE_COMMENTS`).  `mode='single'` wraps the module
//! body in `Interactive`; CPython's `func_type` mode is not modeled (pon's
//! `compile` rejects the mode string first).

use pon_runtime::dynexec::{DynCodeMode, DynCompileRequest};
use pon_runtime::native::{AstNode, AstValue, NodeSpan};
use ruff_python_ast::{self as ast, Mod, PythonVersion};
use ruff_python_parser::{Mode, ParseOptions, parse};
use ruff_text_size::{Ranged, TextRange};

/// Dynexec hook: parse `request.source` per `request.mode` and return the
/// neutral `_ast` tree.  `Err` carries the ruff parse diagnostic and surfaces
/// as `SyntaxError`.
pub(crate) fn parse_dynamic_ast(request: DynCompileRequest<'_>) -> Result<AstNode, String> {
    let converter = Converter::new(request.source);
    let mode = match request.mode {
        DynCodeMode::Eval => Mode::Expression,
        DynCodeMode::Exec | DynCodeMode::Single => Mode::Module,
    };
    let options = ParseOptions::from(mode).with_target_version(PythonVersion::PY314);
    let parsed = parse(request.source, options).map_err(|error| error.to_string())?;
    match (request.mode, parsed.into_syntax()) {
        (DynCodeMode::Eval, Mod::Expression(module)) => Ok(AstNode {
            class: "Expression",
            span: None,
            fields: vec![("body", converter.expr_value(&module.body)?)],
        }),
        (DynCodeMode::Exec, Mod::Module(module)) => Ok(AstNode {
            class: "Module",
            span: None,
            fields: vec![("body", converter.stmt_list(&module.body)?), ("type_ignores", AstValue::List(Vec::new()))],
        }),
        (DynCodeMode::Single, Mod::Module(module)) => Ok(AstNode {
            class: "Interactive",
            span: None,
            fields: vec![("body", converter.stmt_list(&module.body)?)],
        }),
        _ => Err("ruff returned a module kind inconsistent with the parse mode".to_owned()),
    }
}

/// Source text plus its line-start table (the `pon-ir` lowering precedent):
/// byte offset of each 1-based line start, for `lineno`/`col_offset`
/// derivation from ruff byte ranges.
struct Converter<'a> {
    source: &'a str,
    line_starts: Vec<u32>,
}

impl<'a> Converter<'a> {
    fn new(source: &'a str) -> Self {
        let mut line_starts = vec![0u32];
        for (index, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(index as u32 + 1);
            }
        }
        Self { source, line_starts }
    }

    /// (1-based line, 0-based UTF-8 byte column) of a byte offset.
    fn position(&self, offset: u32) -> (u32, u32) {
        let line = self.line_starts.partition_point(|&start| start <= offset);
        (line as u32, offset - self.line_starts[line - 1])
    }

    fn span(&self, range: TextRange) -> Option<NodeSpan> {
        self.span_between(range.start().to_u32(), range.end().to_u32())
    }

    fn span_between(&self, start: u32, end: u32) -> Option<NodeSpan> {
        let (lineno, col_offset) = self.position(start);
        let (end_lineno, end_col_offset) = self.position(end);
        Some(NodeSpan { lineno, col_offset, end_lineno, end_col_offset })
    }

    fn slice(&self, range: TextRange) -> &'a str {
        &self.source[range.start().to_usize()..range.end().to_usize()]
    }

    /// Statement span with decorated/async definitions re-anchored: CPython
    /// stamps `def`/`class` statements at the keyword (`async` for async
    /// defs), never at the first decorator.  Scanning past trivia after the
    /// last decorator is correct whether or not ruff's range includes the
    /// decorator list.
    fn definition_span(&self, range: TextRange, decorators: &[ast::Decorator]) -> Option<NodeSpan> {
        let start = match decorators.last() {
            Some(decorator) => skip_trivia(self.source, decorator.range().end().to_usize()) as u32,
            None => range.start().to_u32(),
        };
        self.span_between(start, range.end().to_u32())
    }

    // ---- statements -----------------------------------------------------

    fn stmt_list(&self, stmts: &[ast::Stmt]) -> Result<AstValue, String> {
        let mut items = Vec::with_capacity(stmts.len());
        for stmt in stmts {
            items.push(node_value(self.stmt(stmt)?));
        }
        Ok(AstValue::List(items))
    }

    fn stmt(&self, stmt: &ast::Stmt) -> Result<AstNode, String> {
        let span = self.span(stmt.range());
        match stmt {
            ast::Stmt::FunctionDef(s) => {
                let class = if s.is_async { "AsyncFunctionDef" } else { "FunctionDef" };
                Ok(AstNode {
                    class,
                    span: self.definition_span(s.range, &s.decorator_list),
                    fields: vec![
                        ("name", AstValue::Str(s.name.id.as_str().to_owned())),
                        ("args", self.parameters(Some(&s.parameters))?),
                        ("body", self.stmt_list(&s.body)?),
                        ("decorator_list", self.decorators(&s.decorator_list)?),
                        ("returns", self.opt_expr(s.returns.as_deref())?),
                        ("type_comment", AstValue::None),
                        ("type_params", self.type_params(s.type_params.as_deref())?),
                    ],
                })
            }
            ast::Stmt::ClassDef(s) => Ok(AstNode {
                class: "ClassDef",
                span: self.definition_span(s.range, &s.decorator_list),
                fields: vec![
                    ("name", AstValue::Str(s.name.id.as_str().to_owned())),
                    ("bases", self.expr_list(s.bases())?),
                    ("keywords", self.keywords(s.keywords())?),
                    ("body", self.stmt_list(&s.body)?),
                    ("decorator_list", self.decorators(&s.decorator_list)?),
                    ("type_params", self.type_params(s.type_params.as_deref())?),
                ],
            }),
            ast::Stmt::Return(s) => Ok(AstNode {
                class: "Return",
                span,
                fields: vec![("value", self.opt_expr(s.value.as_deref())?)],
            }),
            ast::Stmt::Delete(s) => Ok(AstNode {
                class: "Delete",
                span,
                fields: vec![("targets", self.expr_list(&s.targets)?)],
            }),
            ast::Stmt::TypeAlias(s) => Ok(AstNode {
                class: "TypeAlias",
                span,
                fields: vec![
                    ("name", self.expr_value(&s.name)?),
                    ("type_params", self.type_params(s.type_params.as_deref())?),
                    ("value", self.expr_value(&s.value)?),
                ],
            }),
            ast::Stmt::Assign(s) => Ok(AstNode {
                class: "Assign",
                span,
                fields: vec![
                    ("targets", self.expr_list(&s.targets)?),
                    ("value", self.expr_value(&s.value)?),
                    ("type_comment", AstValue::None),
                ],
            }),
            ast::Stmt::AugAssign(s) => Ok(AstNode {
                class: "AugAssign",
                span,
                fields: vec![
                    ("target", self.expr_value(&s.target)?),
                    ("op", op_node(operator_class(s.op))),
                    ("value", self.expr_value(&s.value)?),
                ],
            }),
            ast::Stmt::AnnAssign(s) => Ok(AstNode {
                class: "AnnAssign",
                span,
                fields: vec![
                    ("target", self.expr_value(&s.target)?),
                    ("annotation", self.expr_value(&s.annotation)?),
                    ("value", self.opt_expr(s.value.as_deref())?),
                    ("simple", AstValue::Int(i64::from(s.simple))),
                ],
            }),
            ast::Stmt::For(s) => Ok(AstNode {
                class: if s.is_async { "AsyncFor" } else { "For" },
                span,
                fields: vec![
                    ("target", self.expr_value(&s.target)?),
                    ("iter", self.expr_value(&s.iter)?),
                    ("body", self.stmt_list(&s.body)?),
                    ("orelse", self.stmt_list(&s.orelse)?),
                    ("type_comment", AstValue::None),
                ],
            }),
            ast::Stmt::While(s) => Ok(AstNode {
                class: "While",
                span,
                fields: vec![
                    ("test", self.expr_value(&s.test)?),
                    ("body", self.stmt_list(&s.body)?),
                    ("orelse", self.stmt_list(&s.orelse)?),
                ],
            }),
            ast::Stmt::If(s) => self.if_chain(s),
            ast::Stmt::With(s) => {
                let mut items = Vec::with_capacity(s.items.len());
                for item in &s.items {
                    items.push(node_value(AstNode {
                        class: "withitem",
                        span: None,
                        fields: vec![
                            ("context_expr", self.expr_value(&item.context_expr)?),
                            ("optional_vars", self.opt_expr(item.optional_vars.as_deref())?),
                        ],
                    }));
                }
                Ok(AstNode {
                    class: if s.is_async { "AsyncWith" } else { "With" },
                    span,
                    fields: vec![
                        ("items", AstValue::List(items)),
                        ("body", self.stmt_list(&s.body)?),
                        ("type_comment", AstValue::None),
                    ],
                })
            }
            ast::Stmt::Match(s) => {
                let mut cases = Vec::with_capacity(s.cases.len());
                for case in &s.cases {
                    cases.push(node_value(AstNode {
                        class: "match_case",
                        span: None,
                        fields: vec![
                            ("pattern", node_value(self.pattern(&case.pattern)?)),
                            ("guard", self.opt_expr(case.guard.as_deref())?),
                            ("body", self.stmt_list(&case.body)?),
                        ],
                    }));
                }
                Ok(AstNode {
                    class: "Match",
                    span,
                    fields: vec![("subject", self.expr_value(&s.subject)?), ("cases", AstValue::List(cases))],
                })
            }
            ast::Stmt::Raise(s) => Ok(AstNode {
                class: "Raise",
                span,
                fields: vec![
                    ("exc", self.opt_expr(s.exc.as_deref())?),
                    ("cause", self.opt_expr(s.cause.as_deref())?),
                ],
            }),
            ast::Stmt::Try(s) => {
                let mut handlers = Vec::with_capacity(s.handlers.len());
                for handler in &s.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = handler;
                    handlers.push(node_value(AstNode {
                        class: "ExceptHandler",
                        span: self.span(h.range),
                        fields: vec![
                            ("type", self.opt_expr(h.type_.as_deref())?),
                            ("name", opt_identifier(h.name.as_ref())),
                            ("body", self.stmt_list(&h.body)?),
                        ],
                    }));
                }
                Ok(AstNode {
                    class: if s.is_star { "TryStar" } else { "Try" },
                    span,
                    fields: vec![
                        ("body", self.stmt_list(&s.body)?),
                        ("handlers", AstValue::List(handlers)),
                        ("orelse", self.stmt_list(&s.orelse)?),
                        ("finalbody", self.stmt_list(&s.finalbody)?),
                    ],
                })
            }
            ast::Stmt::Assert(s) => Ok(AstNode {
                class: "Assert",
                span,
                fields: vec![("test", self.expr_value(&s.test)?), ("msg", self.opt_expr(s.msg.as_deref())?)],
            }),
            ast::Stmt::Import(s) => Ok(AstNode {
                class: "Import",
                span,
                fields: vec![("names", self.aliases(&s.names))],
            }),
            ast::Stmt::ImportFrom(s) => Ok(AstNode {
                class: "ImportFrom",
                span,
                fields: vec![
                    ("module", opt_identifier(s.module.as_ref())),
                    ("names", self.aliases(&s.names)),
                    ("level", AstValue::Int(i64::from(s.level))),
                ],
            }),
            ast::Stmt::Global(s) => Ok(AstNode {
                class: "Global",
                span,
                fields: vec![("names", identifier_list(&s.names))],
            }),
            ast::Stmt::Nonlocal(s) => Ok(AstNode {
                class: "Nonlocal",
                span,
                fields: vec![("names", identifier_list(&s.names))],
            }),
            ast::Stmt::Expr(s) => Ok(AstNode {
                class: "Expr",
                span,
                fields: vec![("value", self.expr_value(&s.value)?)],
            }),
            ast::Stmt::Pass(_) => Ok(AstNode { class: "Pass", span, fields: Vec::new() }),
            ast::Stmt::Break(_) => Ok(AstNode { class: "Break", span, fields: Vec::new() }),
            ast::Stmt::Continue(_) => Ok(AstNode { class: "Continue", span, fields: Vec::new() }),
            ast::Stmt::IpyEscapeCommand(_) => Err("IPython escape commands have no _ast representation".to_owned()),
        }
    }

    /// Rebuilds CPython's nested `If` from ruff's flattened
    /// `elif_else_clauses`.  Each `elif` becomes an `If` in the previous
    /// branch's `orelse`, spanning its clause start through the end of the
    /// whole chain (CPython's tail shape); a final `else` contributes its
    /// body directly.
    fn if_chain(&self, s: &ast::StmtIf) -> Result<AstNode, String> {
        let outer_end = s.range.end().to_u32();
        let mut orelse = AstValue::List(Vec::new());
        for clause in s.elif_else_clauses.iter().rev() {
            orelse = match &clause.test {
                Some(test) => AstValue::List(vec![node_value(AstNode {
                    class: "If",
                    span: self.span_between(clause.range.start().to_u32(), outer_end),
                    fields: vec![
                        ("test", self.expr_value(test)?),
                        ("body", self.stmt_list(&clause.body)?),
                        ("orelse", orelse),
                    ],
                })]),
                None => self.stmt_list(&clause.body)?,
            };
        }
        Ok(AstNode {
            class: "If",
            span: self.span(s.range),
            fields: vec![
                ("test", self.expr_value(&s.test)?),
                ("body", self.stmt_list(&s.body)?),
                ("orelse", orelse),
            ],
        })
    }

    fn decorators(&self, decorators: &[ast::Decorator]) -> Result<AstValue, String> {
        let mut items = Vec::with_capacity(decorators.len());
        for decorator in decorators {
            items.push(self.expr_value(&decorator.expression)?);
        }
        Ok(AstValue::List(items))
    }

    fn aliases(&self, names: &[ast::Alias]) -> AstValue {
        AstValue::List(
            names
                .iter()
                .map(|alias| {
                    node_value(AstNode {
                        class: "alias",
                        span: self.span(alias.range),
                        fields: vec![
                            ("name", AstValue::Str(alias.name.id.as_str().to_owned())),
                            ("asname", opt_identifier(alias.asname.as_ref())),
                        ],
                    })
                })
                .collect(),
        )
    }

    // ---- expressions ----------------------------------------------------

    fn expr_list(&self, exprs: &[ast::Expr]) -> Result<AstValue, String> {
        let mut items = Vec::with_capacity(exprs.len());
        for expr in exprs {
            items.push(self.expr_value(expr)?);
        }
        Ok(AstValue::List(items))
    }

    fn opt_expr(&self, expr: Option<&ast::Expr>) -> Result<AstValue, String> {
        expr.map_or(Ok(AstValue::None), |expr| self.expr_value(expr))
    }

    fn expr_value(&self, expr: &ast::Expr) -> Result<AstValue, String> {
        Ok(node_value(self.expr(expr)?))
    }

    fn expr(&self, expr: &ast::Expr) -> Result<AstNode, String> {
        let span = self.span(expr.range());
        match expr {
            ast::Expr::BoolOp(e) => Ok(AstNode {
                class: "BoolOp",
                span,
                fields: vec![
                    ("op", op_node(match e.op {
                        ast::BoolOp::And => "And",
                        ast::BoolOp::Or => "Or",
                    })),
                    ("values", self.expr_list(&e.values)?),
                ],
            }),
            ast::Expr::Named(e) => Ok(AstNode {
                class: "NamedExpr",
                span,
                fields: vec![("target", self.expr_value(&e.target)?), ("value", self.expr_value(&e.value)?)],
            }),
            ast::Expr::BinOp(e) => Ok(AstNode {
                class: "BinOp",
                span,
                fields: vec![
                    ("left", self.expr_value(&e.left)?),
                    ("op", op_node(operator_class(e.op))),
                    ("right", self.expr_value(&e.right)?),
                ],
            }),
            ast::Expr::UnaryOp(e) => Ok(AstNode {
                class: "UnaryOp",
                span,
                fields: vec![
                    ("op", op_node(match e.op {
                        ast::UnaryOp::Invert => "Invert",
                        ast::UnaryOp::Not => "Not",
                        ast::UnaryOp::UAdd => "UAdd",
                        ast::UnaryOp::USub => "USub",
                    })),
                    ("operand", self.expr_value(&e.operand)?),
                ],
            }),
            ast::Expr::Lambda(e) => Ok(AstNode {
                class: "Lambda",
                span,
                fields: vec![
                    ("args", self.parameters(e.parameters.as_deref())?),
                    ("body", self.expr_value(&e.body)?),
                ],
            }),
            ast::Expr::If(e) => Ok(AstNode {
                class: "IfExp",
                span,
                fields: vec![
                    ("test", self.expr_value(&e.test)?),
                    ("body", self.expr_value(&e.body)?),
                    ("orelse", self.expr_value(&e.orelse)?),
                ],
            }),
            ast::Expr::Dict(e) => {
                let mut keys = Vec::with_capacity(e.items.len());
                let mut values = Vec::with_capacity(e.items.len());
                for item in &e.items {
                    keys.push(self.opt_expr(item.key.as_ref())?);
                    values.push(self.expr_value(&item.value)?);
                }
                Ok(AstNode {
                    class: "Dict",
                    span,
                    fields: vec![("keys", AstValue::List(keys)), ("values", AstValue::List(values))],
                })
            }
            ast::Expr::Set(e) => Ok(AstNode {
                class: "Set",
                span,
                fields: vec![("elts", self.expr_list(&e.elts)?)],
            }),
            ast::Expr::ListComp(e) => Ok(AstNode {
                class: "ListComp",
                span,
                fields: vec![
                    ("elt", self.expr_value(&e.elt)?),
                    ("generators", self.comprehensions(&e.generators)?),
                ],
            }),
            ast::Expr::SetComp(e) => Ok(AstNode {
                class: "SetComp",
                span,
                fields: vec![
                    ("elt", self.expr_value(&e.elt)?),
                    ("generators", self.comprehensions(&e.generators)?),
                ],
            }),
            ast::Expr::DictComp(e) => Ok(AstNode {
                class: "DictComp",
                span,
                fields: vec![
                    ("key", self.expr_value(&e.key)?),
                    ("value", self.expr_value(&e.value)?),
                    ("generators", self.comprehensions(&e.generators)?),
                ],
            }),
            ast::Expr::Generator(e) => Ok(AstNode {
                class: "GeneratorExp",
                span,
                fields: vec![
                    ("elt", self.expr_value(&e.elt)?),
                    ("generators", self.comprehensions(&e.generators)?),
                ],
            }),
            ast::Expr::Await(e) => Ok(AstNode {
                class: "Await",
                span,
                fields: vec![("value", self.expr_value(&e.value)?)],
            }),
            ast::Expr::Yield(e) => Ok(AstNode {
                class: "Yield",
                span,
                fields: vec![("value", self.opt_expr(e.value.as_deref())?)],
            }),
            ast::Expr::YieldFrom(e) => Ok(AstNode {
                class: "YieldFrom",
                span,
                fields: vec![("value", self.expr_value(&e.value)?)],
            }),
            ast::Expr::Compare(e) => {
                let ops = e
                    .ops
                    .iter()
                    .map(|op| {
                        op_node(match op {
                            ast::CmpOp::Eq => "Eq",
                            ast::CmpOp::NotEq => "NotEq",
                            ast::CmpOp::Lt => "Lt",
                            ast::CmpOp::LtE => "LtE",
                            ast::CmpOp::Gt => "Gt",
                            ast::CmpOp::GtE => "GtE",
                            ast::CmpOp::Is => "Is",
                            ast::CmpOp::IsNot => "IsNot",
                            ast::CmpOp::In => "In",
                            ast::CmpOp::NotIn => "NotIn",
                        })
                    })
                    .collect();
                Ok(AstNode {
                    class: "Compare",
                    span,
                    fields: vec![
                        ("left", self.expr_value(&e.left)?),
                        ("ops", AstValue::List(ops)),
                        ("comparators", self.expr_list(&e.comparators)?),
                    ],
                })
            }
            ast::Expr::Call(e) => Ok(AstNode {
                class: "Call",
                span,
                fields: vec![
                    ("func", self.expr_value(&e.func)?),
                    ("args", self.expr_list(&e.arguments.args)?),
                    ("keywords", self.keywords(&e.arguments.keywords)?),
                ],
            }),
            ast::Expr::FString(e) => self.joined_str(e),
            ast::Expr::TString(e) => self.template_str(e),
            ast::Expr::StringLiteral(e) => Ok(AstNode {
                class: "Constant",
                span,
                fields: vec![
                    ("value", AstValue::Str(e.value.to_str().to_owned())),
                    ("kind", self.string_kind(expr.range())),
                ],
            }),
            ast::Expr::BytesLiteral(e) => Ok(AstNode {
                class: "Constant",
                span,
                fields: vec![("value", AstValue::Bytes(e.value.bytes().collect())), ("kind", AstValue::None)],
            }),
            ast::Expr::NumberLiteral(e) => Ok(constant(
                span,
                match &e.value {
                    ast::Number::Int(value) => value
                        .as_i64()
                        .map_or_else(|| AstValue::BigInt(value.to_string()), AstValue::Int),
                    ast::Number::Float(value) => AstValue::Float(*value),
                    ast::Number::Complex { real, imag } => AstValue::Complex { real: *real, imag: *imag },
                },
            )),
            ast::Expr::BooleanLiteral(e) => Ok(constant(span, AstValue::Bool(e.value))),
            ast::Expr::NoneLiteral(_) => Ok(constant(span, AstValue::None)),
            ast::Expr::EllipsisLiteral(_) => Ok(constant(span, AstValue::Ellipsis)),
            ast::Expr::Attribute(e) => Ok(AstNode {
                class: "Attribute",
                span,
                fields: vec![
                    ("value", self.expr_value(&e.value)?),
                    ("attr", AstValue::Str(e.attr.id.as_str().to_owned())),
                    ("ctx", context_node(e.ctx)),
                ],
            }),
            ast::Expr::Subscript(e) => Ok(AstNode {
                class: "Subscript",
                span,
                fields: vec![
                    ("value", self.expr_value(&e.value)?),
                    ("slice", self.expr_value(&e.slice)?),
                    ("ctx", context_node(e.ctx)),
                ],
            }),
            ast::Expr::Starred(e) => Ok(AstNode {
                class: "Starred",
                span,
                fields: vec![("value", self.expr_value(&e.value)?), ("ctx", context_node(e.ctx))],
            }),
            ast::Expr::Name(e) => Ok(AstNode {
                class: "Name",
                span,
                fields: vec![("id", AstValue::Str(e.id.as_str().to_owned())), ("ctx", context_node(e.ctx))],
            }),
            ast::Expr::List(e) => Ok(AstNode {
                class: "List",
                span,
                fields: vec![("elts", self.expr_list(&e.elts)?), ("ctx", context_node(e.ctx))],
            }),
            ast::Expr::Tuple(e) => Ok(AstNode {
                class: "Tuple",
                span,
                fields: vec![("elts", self.expr_list(&e.elts)?), ("ctx", context_node(e.ctx))],
            }),
            ast::Expr::Slice(e) => Ok(AstNode {
                class: "Slice",
                span,
                fields: vec![
                    ("lower", self.opt_expr(e.lower.as_deref())?),
                    ("upper", self.opt_expr(e.upper.as_deref())?),
                    ("step", self.opt_expr(e.step.as_deref())?),
                ],
            }),
            ast::Expr::IpyEscapeCommand(_) => Err("IPython escape commands have no _ast representation".to_owned()),
        }
    }

    /// `Constant.kind`: `'u'` for a `u`/`U`-prefixed string literal (first
    /// part of an implicit concatenation, CPython's rule), else `None`.
    fn string_kind(&self, range: TextRange) -> AstValue {
        if self.source[range.start().to_usize()..].starts_with(['u', 'U']) {
            AstValue::Str("u".to_owned())
        } else {
            AstValue::None
        }
    }

    fn comprehensions(&self, generators: &[ast::Comprehension]) -> Result<AstValue, String> {
        let mut items = Vec::with_capacity(generators.len());
        for generator in generators {
            items.push(node_value(AstNode {
                class: "comprehension",
                span: None,
                fields: vec![
                    ("target", self.expr_value(&generator.target)?),
                    ("iter", self.expr_value(&generator.iter)?),
                    ("ifs", self.expr_list(&generator.ifs)?),
                    ("is_async", AstValue::Int(i64::from(generator.is_async))),
                ],
            }));
        }
        Ok(AstValue::List(items))
    }

    fn keywords(&self, keywords: &[ast::Keyword]) -> Result<AstValue, String> {
        let mut items = Vec::with_capacity(keywords.len());
        for keyword in keywords {
            items.push(node_value(AstNode {
                class: "keyword",
                span: self.span(keyword.range),
                fields: vec![
                    ("arg", opt_identifier(keyword.arg.as_ref())),
                    ("value", self.expr_value(&keyword.value)?),
                ],
            }));
        }
        Ok(AstValue::List(items))
    }

    // ---- parameters -------------------------------------------------------

    /// Ruff attaches defaults to parameters; CPython splits them off:
    /// `defaults` collects present positional defaults (parser-guaranteed to
    /// be a suffix), `kw_defaults` is per-keyword-only `expr | None`.
    fn parameters(&self, parameters: Option<&ast::Parameters>) -> Result<AstValue, String> {
        let Some(parameters) = parameters else {
            return Ok(node_value(AstNode {
                class: "arguments",
                span: None,
                fields: vec![
                    ("posonlyargs", AstValue::List(Vec::new())),
                    ("args", AstValue::List(Vec::new())),
                    ("vararg", AstValue::None),
                    ("kwonlyargs", AstValue::List(Vec::new())),
                    ("kw_defaults", AstValue::List(Vec::new())),
                    ("kwarg", AstValue::None),
                    ("defaults", AstValue::List(Vec::new())),
                ],
            }));
        };
        let mut defaults = Vec::new();
        let mut posonlyargs = Vec::with_capacity(parameters.posonlyargs.len());
        let mut args = Vec::with_capacity(parameters.args.len());
        for (list, source) in [(&mut posonlyargs, &parameters.posonlyargs), (&mut args, &parameters.args)] {
            for parameter in source {
                list.push(node_value(self.arg(&parameter.parameter)?));
                if let Some(default) = parameter.default.as_deref() {
                    defaults.push(self.expr_value(default)?);
                }
            }
        }
        let mut kwonlyargs = Vec::with_capacity(parameters.kwonlyargs.len());
        let mut kw_defaults = Vec::with_capacity(parameters.kwonlyargs.len());
        for parameter in &parameters.kwonlyargs {
            kwonlyargs.push(node_value(self.arg(&parameter.parameter)?));
            kw_defaults.push(self.opt_expr(parameter.default.as_deref())?);
        }
        let variadic = |parameter: Option<&ast::Parameter>| -> Result<AstValue, String> {
            parameter.map_or(Ok(AstValue::None), |parameter| Ok(node_value(self.arg(parameter)?)))
        };
        Ok(node_value(AstNode {
            class: "arguments",
            span: None,
            fields: vec![
                ("posonlyargs", AstValue::List(posonlyargs)),
                ("args", AstValue::List(args)),
                ("vararg", variadic(parameters.vararg.as_deref())?),
                ("kwonlyargs", AstValue::List(kwonlyargs)),
                ("kw_defaults", AstValue::List(kw_defaults)),
                ("kwarg", variadic(parameters.kwarg.as_deref())?),
                ("defaults", AstValue::List(defaults)),
            ],
        }))
    }

    fn arg(&self, parameter: &ast::Parameter) -> Result<AstNode, String> {
        Ok(AstNode {
            class: "arg",
            span: self.span(parameter.range),
            fields: vec![
                ("arg", AstValue::Str(parameter.name.id.as_str().to_owned())),
                ("annotation", self.opt_expr(parameter.annotation.as_deref())?),
                ("type_comment", AstValue::None),
            ],
        })
    }

    // ---- type parameters --------------------------------------------------

    fn type_params(&self, type_params: Option<&ast::TypeParams>) -> Result<AstValue, String> {
        let Some(type_params) = type_params else {
            return Ok(AstValue::List(Vec::new()));
        };
        let mut items = Vec::with_capacity(type_params.type_params.len());
        for type_param in &type_params.type_params {
            let (class, name, bound, default, range) = match type_param {
                ast::TypeParam::TypeVar(p) => ("TypeVar", &p.name, p.bound.as_deref(), p.default.as_deref(), p.range),
                ast::TypeParam::ParamSpec(p) => ("ParamSpec", &p.name, None, p.default.as_deref(), p.range),
                ast::TypeParam::TypeVarTuple(p) => ("TypeVarTuple", &p.name, None, p.default.as_deref(), p.range),
            };
            let mut fields = vec![("name", AstValue::Str(name.id.as_str().to_owned()))];
            if class == "TypeVar" {
                fields.push(("bound", self.opt_expr(bound)?));
            }
            fields.push(("default_value", self.opt_expr(default)?));
            items.push(node_value(AstNode { class, span: self.span(range), fields }));
        }
        Ok(AstValue::List(items))
    }

    // ---- match patterns ---------------------------------------------------

    fn pattern(&self, pattern: &ast::Pattern) -> Result<AstNode, String> {
        let span = self.span(pattern.range());
        match pattern {
            ast::Pattern::MatchValue(p) => Ok(AstNode {
                class: "MatchValue",
                span,
                fields: vec![("value", self.expr_value(&p.value)?)],
            }),
            ast::Pattern::MatchSingleton(p) => Ok(AstNode {
                class: "MatchSingleton",
                span,
                fields: vec![(
                    "value",
                    match p.value {
                        ast::Singleton::None => AstValue::None,
                        ast::Singleton::True => AstValue::Bool(true),
                        ast::Singleton::False => AstValue::Bool(false),
                    },
                )],
            }),
            ast::Pattern::MatchSequence(p) => Ok(AstNode {
                class: "MatchSequence",
                span,
                fields: vec![("patterns", self.pattern_list(&p.patterns)?)],
            }),
            ast::Pattern::MatchMapping(p) => Ok(AstNode {
                class: "MatchMapping",
                span,
                fields: vec![
                    ("keys", self.expr_list(&p.keys)?),
                    ("patterns", self.pattern_list(&p.patterns)?),
                    ("rest", opt_identifier(p.rest.as_ref())),
                ],
            }),
            ast::Pattern::MatchClass(p) => {
                let kwd_attrs = p
                    .arguments
                    .keywords
                    .iter()
                    .map(|keyword| AstValue::Str(keyword.attr.id.as_str().to_owned()))
                    .collect();
                let mut kwd_patterns = Vec::with_capacity(p.arguments.keywords.len());
                for keyword in &p.arguments.keywords {
                    kwd_patterns.push(node_value(self.pattern(&keyword.pattern)?));
                }
                Ok(AstNode {
                    class: "MatchClass",
                    span,
                    fields: vec![
                        ("cls", self.expr_value(&p.cls)?),
                        ("patterns", self.pattern_list(&p.arguments.patterns)?),
                        ("kwd_attrs", AstValue::List(kwd_attrs)),
                        ("kwd_patterns", AstValue::List(kwd_patterns)),
                    ],
                })
            }
            ast::Pattern::MatchStar(p) => Ok(AstNode {
                class: "MatchStar",
                span,
                fields: vec![("name", opt_identifier(p.name.as_ref()))],
            }),
            ast::Pattern::MatchAs(p) => {
                let inner = match p.pattern.as_deref() {
                    Some(pattern) => node_value(self.pattern(pattern)?),
                    None => AstValue::None,
                };
                Ok(AstNode {
                    class: "MatchAs",
                    span,
                    fields: vec![("pattern", inner), ("name", opt_identifier(p.name.as_ref()))],
                })
            }
            ast::Pattern::MatchOr(p) => Ok(AstNode {
                class: "MatchOr",
                span,
                fields: vec![("patterns", self.pattern_list(&p.patterns)?)],
            }),
        }
    }

    fn pattern_list(&self, patterns: &[ast::Pattern]) -> Result<AstValue, String> {
        let mut items = Vec::with_capacity(patterns.len());
        for pattern in patterns {
            items.push(node_value(self.pattern(pattern)?));
        }
        Ok(AstValue::List(items))
    }

    // ---- f-strings / t-strings ---------------------------------------------

    /// `JoinedStr` from a (possibly implicitly concatenated) f-string:
    /// adjacent literal text merges into single `Constant`s (CPython's
    /// shape), interpolations become `FormattedValue`s, and `f"{x=}"` debug
    /// text re-expands to a leading `Constant` run.
    fn joined_str(&self, e: &ast::ExprFString) -> Result<AstNode, String> {
        let mut values = InterpolatedValues::new(self);
        for part in e.value.iter() {
            match part {
                ast::FStringPart::Literal(literal) => values.push_text(&literal.value, literal.range),
                ast::FStringPart::FString(fstring) => {
                    for element in &*fstring.elements {
                        self.interpolated_element(&mut values, element, false)?;
                    }
                }
            }
        }
        Ok(AstNode {
            class: "JoinedStr",
            span: self.span(e.range),
            fields: vec![("values", AstValue::List(values.finish()))],
        })
    }

    /// `TemplateStr` from a t-string; `Interpolation` nodes additionally
    /// carry the expression's source text in `str`.
    fn template_str(&self, e: &ast::ExprTString) -> Result<AstNode, String> {
        let mut values = InterpolatedValues::new(self);
        for tstring in e.value.iter() {
            for element in &*tstring.elements {
                self.interpolated_element(&mut values, element, true)?;
            }
        }
        Ok(AstNode {
            class: "TemplateStr",
            span: self.span(e.range),
            fields: vec![("values", AstValue::List(values.finish()))],
        })
    }

    fn interpolated_element(
        &self,
        values: &mut InterpolatedValues,
        element: &ast::InterpolatedStringElement,
        template: bool,
    ) -> Result<(), String> {
        match element {
            ast::InterpolatedStringElement::Literal(literal) => {
                values.push_text(&literal.value, literal.range);
            }
            ast::InterpolatedStringElement::Interpolation(interpolation) => {
                let expression_text = self.slice(interpolation.expression.range());
                if let Some(debug) = &interpolation.debug_text {
                    let text = format!("{}{expression_text}{}", debug.leading, debug.trailing);
                    values.push_owned_text(text, interpolation.range);
                }
                // CPython: a bare `=` debug spec (no conversion, no format
                // spec) implies `!r`.
                let mut conversion = i64::from(interpolation.conversion as i8);
                if interpolation.debug_text.is_some() && conversion < 0 && interpolation.format_spec.is_none() {
                    conversion = i64::from(b'r');
                }
                let format_spec = match interpolation.format_spec.as_deref() {
                    None => AstValue::None,
                    Some(spec) => {
                        let mut spec_values = InterpolatedValues::new(self);
                        for element in &*spec.elements {
                            self.interpolated_element(&mut spec_values, element, false)?;
                        }
                        node_value(AstNode {
                            class: "JoinedStr",
                            span: self.span(spec.range),
                            fields: vec![("values", AstValue::List(spec_values.finish()))],
                        })
                    }
                };
                let mut fields = vec![("value", self.expr_value(&interpolation.expression)?)];
                if template {
                    fields.push(("str", AstValue::Str(expression_text.to_owned())));
                }
                fields.push(("conversion", AstValue::Int(conversion)));
                fields.push(("format_spec", format_spec));
                values.flush_then_push(node_value(AstNode {
                    class: if template { "Interpolation" } else { "FormattedValue" },
                    span: self.span(interpolation.range),
                    fields,
                }));
            }
        }
        Ok(())
    }
}

/// Accumulator for `JoinedStr`/`TemplateStr` values: merges adjacent literal
/// text into one pending `Constant` (CPython merges across implicit
/// concatenation), flushing before each interpolation node.  Borrows the
/// converter for table-backed spans on the merged runs.
struct InterpolatedValues<'c, 'a> {
    converter: &'c Converter<'a>,
    values: Vec<AstValue>,
    pending: Option<(String, u32, u32)>,
}

impl<'c, 'a> InterpolatedValues<'c, 'a> {
    fn new(converter: &'c Converter<'a>) -> Self {
        Self { converter, values: Vec::new(), pending: None }
    }

    fn push_text(&mut self, text: &str, range: TextRange) {
        self.append(text, range.start().to_u32(), range.end().to_u32());
    }

    fn push_owned_text(&mut self, text: String, range: TextRange) {
        self.append(&text, range.start().to_u32(), range.end().to_u32());
    }

    fn append(&mut self, text: &str, start: u32, end: u32) {
        match &mut self.pending {
            Some((buffer, _, pending_end)) => {
                buffer.push_str(text);
                *pending_end = end;
            }
            None => self.pending = Some((text.to_owned(), start, end)),
        }
    }

    fn flush(&mut self) {
        if let Some((text, start, end)) = self.pending.take() {
            self.values.push(node_value(AstNode {
                class: "Constant",
                span: self.converter.span_between(start, end),
                fields: vec![("value", AstValue::Str(text)), ("kind", AstValue::None)],
            }));
        }
    }

    fn flush_then_push(&mut self, value: AstValue) {
        self.flush();
        self.values.push(value);
    }

    fn finish(mut self) -> Vec<AstValue> {
        self.flush();
        self.values
    }
}

fn node_value(node: AstNode) -> AstValue {
    AstValue::Node(Box::new(node))
}

fn constant(span: Option<NodeSpan>, value: AstValue) -> AstNode {
    AstNode {
        class: "Constant",
        span,
        fields: vec![("value", value), ("kind", AstValue::None)],
    }
}

/// Zero-field operator/context singleton node (fresh instance per use; the
/// classes carry no location attributes).
fn op_node(class: &'static str) -> AstValue {
    node_value(AstNode { class, span: None, fields: Vec::new() })
}

fn operator_class(op: ast::Operator) -> &'static str {
    match op {
        ast::Operator::Add => "Add",
        ast::Operator::Sub => "Sub",
        ast::Operator::Mult => "Mult",
        ast::Operator::MatMult => "MatMult",
        ast::Operator::Div => "Div",
        ast::Operator::Mod => "Mod",
        ast::Operator::Pow => "Pow",
        ast::Operator::LShift => "LShift",
        ast::Operator::RShift => "RShift",
        ast::Operator::BitOr => "BitOr",
        ast::Operator::BitXor => "BitXor",
        ast::Operator::BitAnd => "BitAnd",
        ast::Operator::FloorDiv => "FloorDiv",
    }
}

fn context_node(ctx: ast::ExprContext) -> AstValue {
    op_node(match ctx {
        ast::ExprContext::Store => "Store",
        ast::ExprContext::Del => "Del",
        // `Invalid` is ruff's recovery context; unreachable behind a
        // successful parse.  `Load` is the safe projection.
        ast::ExprContext::Load | ast::ExprContext::Invalid => "Load",
    })
}

fn opt_identifier(identifier: Option<&ast::Identifier>) -> AstValue {
    identifier.map_or(AstValue::None, |identifier| AstValue::Str(identifier.id.as_str().to_owned()))
}

fn identifier_list(identifiers: &[ast::Identifier]) -> AstValue {
    AstValue::List(
        identifiers
            .iter()
            .map(|identifier| AstValue::Str(identifier.id.as_str().to_owned()))
            .collect(),
    )
}

/// First non-trivia byte at or after `offset`: skips whitespace and `#`
/// comments, landing on the `def`/`async`/`class` keyword that follows a
/// decorator list.
fn skip_trivia(source: &str, mut offset: usize) -> usize {
    let bytes = source.as_bytes();
    while offset < bytes.len() {
        match bytes[offset] {
            b' ' | b'\t' | b'\r' | b'\n' | b'\x0c' => offset += 1,
            b'#' => {
                while offset < bytes.len() && bytes[offset] != b'\n' {
                    offset += 1;
                }
            }
            _ => break,
        }
    }
    offset
}
