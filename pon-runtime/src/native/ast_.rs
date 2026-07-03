//! Native `_ast` module: the CPython 3.14 AST node class hierarchy as DATA.
//!
//! v0 scope (CT grind wave 2): make `import ast` succeed.  The vendored
//! `Lib/ast.py` is `from _ast import *` plus pure-Python helpers, so `_ast`
//! must export the `AST` base, the ~125 node classes, and the `PyCF_*`
//! compiler-flag constants.  Node classes are heap classes generated from
//! [`NODES`], a static table transcribed from CPython 3.14.6 `_ast` class
//! dicts: per-class ASDL docstring, `_fields`, own-dict `_attributes`
//! (category classes only — leaves inherit), and `__match_args__` (CPython
//! aliases the `_fields` tuple).  `__module__` is "ast", matching CPython.
//!
//! Construction is complete in `tp_new` ([`ast_node_new`]): positional args
//! zip against the class `_fields` and keyword args store as instance
//! attributes (CPython `ast_type_init` shape).  Both call paths treat a
//! non-default `tp_new` as full construction, so no `__init__` leg exists.
//! CPython 3.13+ class data modeled for `ast.dump` default-omission parity:
//! optional (`?`-typed) fields get a class-level `None` default, and
//! `_field_types` maps each list (`*`-typed) field to a marker class whose
//! `__origin__` is the builtin `list` (dump only probes `__origin__ is
//! list`; full `typing` generics are deliberately not modeled).  NOT modeled
//! in v0: the missing-required-field DeprecationWarning.
//!
//! `compile(source, filename, mode, PyCF_ONLY_AST)` (`ast.parse`) is served
//! by the dynexec `compile` builtin through [`AstNode`]/[`AstValue`]: the
//! host frontend parses with ruff and returns this neutral pure-data tree,
//! and [`build_ast_object`] materializes it as `_ast` instances here, where
//! the class table and construction machinery live.  Operator/context
//! singletons (`Add`, `Load`, ...) are fresh instances per node, not
//! CPython's interned singletons.

use std::collections::HashMap;
use std::ptr;
use std::sync::OnceLock;

use crate::abi::{self, pon_const_str};
use crate::intern::intern;
use crate::object::{PyObject, PyType};
use crate::thread_state::{pon_err_clear, pon_err_message};
use crate::types::exc::ExceptionKind;
use crate::types::type_;

use super::install_module;

/// One CPython 3.14 `_ast` class: name, single base ("" = object, `AST`
/// only), ASDL `_fields`, own-dict `_attributes` (`None` = inherit from the
/// category base), and the ASDL signature docstring.  Table order is
/// topological: every base precedes its subclasses.
struct NodeSpec {
    name: &'static str,
    base: &'static str,
    fields: &'static [&'static str],
    attrs: Option<&'static [&'static str]>,
    doc: &'static str,
}

static NODES: &[NodeSpec] = &[
    NodeSpec { name: "AST", base: "", fields: &[], attrs: Some(&[]), doc: "" },
    NodeSpec { name: "operator", base: "AST", fields: &[], attrs: Some(&[]), doc: "operator = Add | Sub | Mult | MatMult | Div | Mod | Pow | LShift | RShift | BitOr | BitXor | BitAnd | FloorDiv" },
    NodeSpec { name: "Add", base: "operator", fields: &[], attrs: None, doc: "Add" },
    NodeSpec { name: "boolop", base: "AST", fields: &[], attrs: Some(&[]), doc: "boolop = And | Or" },
    NodeSpec { name: "And", base: "boolop", fields: &[], attrs: None, doc: "And" },
    NodeSpec { name: "stmt", base: "AST", fields: &[], attrs: Some(&["lineno", "col_offset", "end_lineno", "end_col_offset"]), doc: "stmt = FunctionDef(identifier name, arguments args, stmt* body, expr* decorator_list, expr? returns, string? type_comment, type_param* type_params)\n     | AsyncFunctionDef(identifier name, arguments args, stmt* body, expr* decorator_list, expr? returns, string? type_comment, type_param* type_params)\n     | ClassDef(identifier name, expr* bases, keyword* keywords, stmt* body, expr* decorator_list, type_param* type_params)\n     | Return(expr? value)\n     | Delete(expr* targets)\n     | Assign(expr* targets, expr value, string? type_comment)\n     | TypeAlias(expr name, type_param* type_params, expr value)\n     | AugAssign(expr target, operator op, expr value)\n     | AnnAssign(expr target, expr annotation, expr? value, int simple)\n     | For(expr target, expr iter, stmt* body, stmt* orelse, string? type_comment)\n     | AsyncFor(expr target, expr iter, stmt* body, stmt* orelse, string? type_comment)\n     | While(expr test, stmt* body, stmt* orelse)\n     | If(expr test, stmt* body, stmt* orelse)\n     | With(withitem* items, stmt* body, string? type_comment)\n     | AsyncWith(withitem* items, stmt* body, string? type_comment)\n     | Match(expr subject, match_case* cases)\n     | Raise(expr? exc, expr? cause)\n     | Try(stmt* body, excepthandler* handlers, stmt* orelse, stmt* finalbody)\n     | TryStar(stmt* body, excepthandler* handlers, stmt* orelse, stmt* finalbody)\n     | Assert(expr test, expr? msg)\n     | Import(alias* names)\n     | ImportFrom(identifier? module, alias* names, int? level)\n     | Global(identifier* names)\n     | Nonlocal(identifier* names)\n     | Expr(expr value)\n     | Pass\n     | Break\n     | Continue" },
    NodeSpec { name: "AnnAssign", base: "stmt", fields: &["target", "annotation", "value", "simple"], attrs: None, doc: "AnnAssign(expr target, expr annotation, expr? value, int simple)" },
    NodeSpec { name: "Assert", base: "stmt", fields: &["test", "msg"], attrs: None, doc: "Assert(expr test, expr? msg)" },
    NodeSpec { name: "Assign", base: "stmt", fields: &["targets", "value", "type_comment"], attrs: None, doc: "Assign(expr* targets, expr value, string? type_comment)" },
    NodeSpec { name: "AsyncFor", base: "stmt", fields: &["target", "iter", "body", "orelse", "type_comment"], attrs: None, doc: "AsyncFor(expr target, expr iter, stmt* body, stmt* orelse, string? type_comment)" },
    NodeSpec { name: "AsyncFunctionDef", base: "stmt", fields: &["name", "args", "body", "decorator_list", "returns", "type_comment", "type_params"], attrs: None, doc: "AsyncFunctionDef(identifier name, arguments args, stmt* body, expr* decorator_list, expr? returns, string? type_comment, type_param* type_params)" },
    NodeSpec { name: "AsyncWith", base: "stmt", fields: &["items", "body", "type_comment"], attrs: None, doc: "AsyncWith(withitem* items, stmt* body, string? type_comment)" },
    NodeSpec { name: "expr", base: "AST", fields: &[], attrs: Some(&["lineno", "col_offset", "end_lineno", "end_col_offset"]), doc: "expr = BoolOp(boolop op, expr* values)\n     | NamedExpr(expr target, expr value)\n     | BinOp(expr left, operator op, expr right)\n     | UnaryOp(unaryop op, expr operand)\n     | Lambda(arguments args, expr body)\n     | IfExp(expr test, expr body, expr orelse)\n     | Dict(expr?* keys, expr* values)\n     | Set(expr* elts)\n     | ListComp(expr elt, comprehension* generators)\n     | SetComp(expr elt, comprehension* generators)\n     | DictComp(expr key, expr value, comprehension* generators)\n     | GeneratorExp(expr elt, comprehension* generators)\n     | Await(expr value)\n     | Yield(expr? value)\n     | YieldFrom(expr value)\n     | Compare(expr left, cmpop* ops, expr* comparators)\n     | Call(expr func, expr* args, keyword* keywords)\n     | FormattedValue(expr value, int conversion, expr? format_spec)\n     | Interpolation(expr value, constant str, int conversion, expr? format_spec)\n     | JoinedStr(expr* values)\n     | TemplateStr(expr* values)\n     | Constant(constant value, string? kind)\n     | Attribute(expr value, identifier attr, expr_context ctx)\n     | Subscript(expr value, expr slice, expr_context ctx)\n     | Starred(expr value, expr_context ctx)\n     | Name(identifier id, expr_context ctx)\n     | List(expr* elts, expr_context ctx)\n     | Tuple(expr* elts, expr_context ctx)\n     | Slice(expr? lower, expr? upper, expr? step)" },
    NodeSpec { name: "Attribute", base: "expr", fields: &["value", "attr", "ctx"], attrs: None, doc: "Attribute(expr value, identifier attr, expr_context ctx)" },
    NodeSpec { name: "AugAssign", base: "stmt", fields: &["target", "op", "value"], attrs: None, doc: "AugAssign(expr target, operator op, expr value)" },
    NodeSpec { name: "Await", base: "expr", fields: &["value"], attrs: None, doc: "Await(expr value)" },
    NodeSpec { name: "BinOp", base: "expr", fields: &["left", "op", "right"], attrs: None, doc: "BinOp(expr left, operator op, expr right)" },
    NodeSpec { name: "BitAnd", base: "operator", fields: &[], attrs: None, doc: "BitAnd" },
    NodeSpec { name: "BitOr", base: "operator", fields: &[], attrs: None, doc: "BitOr" },
    NodeSpec { name: "BitXor", base: "operator", fields: &[], attrs: None, doc: "BitXor" },
    NodeSpec { name: "BoolOp", base: "expr", fields: &["op", "values"], attrs: None, doc: "BoolOp(boolop op, expr* values)" },
    NodeSpec { name: "Break", base: "stmt", fields: &[], attrs: None, doc: "Break" },
    NodeSpec { name: "Call", base: "expr", fields: &["func", "args", "keywords"], attrs: None, doc: "Call(expr func, expr* args, keyword* keywords)" },
    NodeSpec { name: "ClassDef", base: "stmt", fields: &["name", "bases", "keywords", "body", "decorator_list", "type_params"], attrs: None, doc: "ClassDef(identifier name, expr* bases, keyword* keywords, stmt* body, expr* decorator_list, type_param* type_params)" },
    NodeSpec { name: "Compare", base: "expr", fields: &["left", "ops", "comparators"], attrs: None, doc: "Compare(expr left, cmpop* ops, expr* comparators)" },
    NodeSpec { name: "Constant", base: "expr", fields: &["value", "kind"], attrs: None, doc: "Constant(constant value, string? kind)" },
    NodeSpec { name: "Continue", base: "stmt", fields: &[], attrs: None, doc: "Continue" },
    NodeSpec { name: "expr_context", base: "AST", fields: &[], attrs: Some(&[]), doc: "expr_context = Load | Store | Del" },
    NodeSpec { name: "Del", base: "expr_context", fields: &[], attrs: None, doc: "Del" },
    NodeSpec { name: "Delete", base: "stmt", fields: &["targets"], attrs: None, doc: "Delete(expr* targets)" },
    NodeSpec { name: "Dict", base: "expr", fields: &["keys", "values"], attrs: None, doc: "Dict(expr?* keys, expr* values)" },
    NodeSpec { name: "DictComp", base: "expr", fields: &["key", "value", "generators"], attrs: None, doc: "DictComp(expr key, expr value, comprehension* generators)" },
    NodeSpec { name: "Div", base: "operator", fields: &[], attrs: None, doc: "Div" },
    NodeSpec { name: "cmpop", base: "AST", fields: &[], attrs: Some(&[]), doc: "cmpop = Eq | NotEq | Lt | LtE | Gt | GtE | Is | IsNot | In | NotIn" },
    NodeSpec { name: "Eq", base: "cmpop", fields: &[], attrs: None, doc: "Eq" },
    NodeSpec { name: "excepthandler", base: "AST", fields: &[], attrs: Some(&["lineno", "col_offset", "end_lineno", "end_col_offset"]), doc: "excepthandler = ExceptHandler(expr? type, identifier? name, stmt* body)" },
    NodeSpec { name: "ExceptHandler", base: "excepthandler", fields: &["type", "name", "body"], attrs: None, doc: "ExceptHandler(expr? type, identifier? name, stmt* body)" },
    NodeSpec { name: "Expr", base: "stmt", fields: &["value"], attrs: None, doc: "Expr(expr value)" },
    NodeSpec { name: "mod", base: "AST", fields: &[], attrs: Some(&[]), doc: "mod = Module(stmt* body, type_ignore* type_ignores)\n    | Interactive(stmt* body)\n    | Expression(expr body)\n    | FunctionType(expr* argtypes, expr returns)" },
    NodeSpec { name: "Expression", base: "mod", fields: &["body"], attrs: None, doc: "Expression(expr body)" },
    NodeSpec { name: "FloorDiv", base: "operator", fields: &[], attrs: None, doc: "FloorDiv" },
    NodeSpec { name: "For", base: "stmt", fields: &["target", "iter", "body", "orelse", "type_comment"], attrs: None, doc: "For(expr target, expr iter, stmt* body, stmt* orelse, string? type_comment)" },
    NodeSpec { name: "FormattedValue", base: "expr", fields: &["value", "conversion", "format_spec"], attrs: None, doc: "FormattedValue(expr value, int conversion, expr? format_spec)" },
    NodeSpec { name: "FunctionDef", base: "stmt", fields: &["name", "args", "body", "decorator_list", "returns", "type_comment", "type_params"], attrs: None, doc: "FunctionDef(identifier name, arguments args, stmt* body, expr* decorator_list, expr? returns, string? type_comment, type_param* type_params)" },
    NodeSpec { name: "FunctionType", base: "mod", fields: &["argtypes", "returns"], attrs: None, doc: "FunctionType(expr* argtypes, expr returns)" },
    NodeSpec { name: "GeneratorExp", base: "expr", fields: &["elt", "generators"], attrs: None, doc: "GeneratorExp(expr elt, comprehension* generators)" },
    NodeSpec { name: "Global", base: "stmt", fields: &["names"], attrs: None, doc: "Global(identifier* names)" },
    NodeSpec { name: "Gt", base: "cmpop", fields: &[], attrs: None, doc: "Gt" },
    NodeSpec { name: "GtE", base: "cmpop", fields: &[], attrs: None, doc: "GtE" },
    NodeSpec { name: "If", base: "stmt", fields: &["test", "body", "orelse"], attrs: None, doc: "If(expr test, stmt* body, stmt* orelse)" },
    NodeSpec { name: "IfExp", base: "expr", fields: &["test", "body", "orelse"], attrs: None, doc: "IfExp(expr test, expr body, expr orelse)" },
    NodeSpec { name: "Import", base: "stmt", fields: &["names"], attrs: None, doc: "Import(alias* names)" },
    NodeSpec { name: "ImportFrom", base: "stmt", fields: &["module", "names", "level"], attrs: None, doc: "ImportFrom(identifier? module, alias* names, int? level)" },
    NodeSpec { name: "In", base: "cmpop", fields: &[], attrs: None, doc: "In" },
    NodeSpec { name: "Interactive", base: "mod", fields: &["body"], attrs: None, doc: "Interactive(stmt* body)" },
    NodeSpec { name: "Interpolation", base: "expr", fields: &["value", "str", "conversion", "format_spec"], attrs: None, doc: "Interpolation(expr value, constant str, int conversion, expr? format_spec)" },
    NodeSpec { name: "unaryop", base: "AST", fields: &[], attrs: Some(&[]), doc: "unaryop = Invert | Not | UAdd | USub" },
    NodeSpec { name: "Invert", base: "unaryop", fields: &[], attrs: None, doc: "Invert" },
    NodeSpec { name: "Is", base: "cmpop", fields: &[], attrs: None, doc: "Is" },
    NodeSpec { name: "IsNot", base: "cmpop", fields: &[], attrs: None, doc: "IsNot" },
    NodeSpec { name: "JoinedStr", base: "expr", fields: &["values"], attrs: None, doc: "JoinedStr(expr* values)" },
    NodeSpec { name: "LShift", base: "operator", fields: &[], attrs: None, doc: "LShift" },
    NodeSpec { name: "Lambda", base: "expr", fields: &["args", "body"], attrs: None, doc: "Lambda(arguments args, expr body)" },
    NodeSpec { name: "List", base: "expr", fields: &["elts", "ctx"], attrs: None, doc: "List(expr* elts, expr_context ctx)" },
    NodeSpec { name: "ListComp", base: "expr", fields: &["elt", "generators"], attrs: None, doc: "ListComp(expr elt, comprehension* generators)" },
    NodeSpec { name: "Load", base: "expr_context", fields: &[], attrs: None, doc: "Load" },
    NodeSpec { name: "Lt", base: "cmpop", fields: &[], attrs: None, doc: "Lt" },
    NodeSpec { name: "LtE", base: "cmpop", fields: &[], attrs: None, doc: "LtE" },
    NodeSpec { name: "MatMult", base: "operator", fields: &[], attrs: None, doc: "MatMult" },
    NodeSpec { name: "Match", base: "stmt", fields: &["subject", "cases"], attrs: None, doc: "Match(expr subject, match_case* cases)" },
    NodeSpec { name: "pattern", base: "AST", fields: &[], attrs: Some(&["lineno", "col_offset", "end_lineno", "end_col_offset"]), doc: "pattern = MatchValue(expr value)\n        | MatchSingleton(constant value)\n        | MatchSequence(pattern* patterns)\n        | MatchMapping(expr* keys, pattern* patterns, identifier? rest)\n        | MatchClass(expr cls, pattern* patterns, identifier* kwd_attrs, pattern* kwd_patterns)\n        | MatchStar(identifier? name)\n        | MatchAs(pattern? pattern, identifier? name)\n        | MatchOr(pattern* patterns)" },
    NodeSpec { name: "MatchAs", base: "pattern", fields: &["pattern", "name"], attrs: None, doc: "MatchAs(pattern? pattern, identifier? name)" },
    NodeSpec { name: "MatchClass", base: "pattern", fields: &["cls", "patterns", "kwd_attrs", "kwd_patterns"], attrs: None, doc: "MatchClass(expr cls, pattern* patterns, identifier* kwd_attrs, pattern* kwd_patterns)" },
    NodeSpec { name: "MatchMapping", base: "pattern", fields: &["keys", "patterns", "rest"], attrs: None, doc: "MatchMapping(expr* keys, pattern* patterns, identifier? rest)" },
    NodeSpec { name: "MatchOr", base: "pattern", fields: &["patterns"], attrs: None, doc: "MatchOr(pattern* patterns)" },
    NodeSpec { name: "MatchSequence", base: "pattern", fields: &["patterns"], attrs: None, doc: "MatchSequence(pattern* patterns)" },
    NodeSpec { name: "MatchSingleton", base: "pattern", fields: &["value"], attrs: None, doc: "MatchSingleton(constant value)" },
    NodeSpec { name: "MatchStar", base: "pattern", fields: &["name"], attrs: None, doc: "MatchStar(identifier? name)" },
    NodeSpec { name: "MatchValue", base: "pattern", fields: &["value"], attrs: None, doc: "MatchValue(expr value)" },
    NodeSpec { name: "Mod", base: "operator", fields: &[], attrs: None, doc: "Mod" },
    NodeSpec { name: "Module", base: "mod", fields: &["body", "type_ignores"], attrs: None, doc: "Module(stmt* body, type_ignore* type_ignores)" },
    NodeSpec { name: "Mult", base: "operator", fields: &[], attrs: None, doc: "Mult" },
    NodeSpec { name: "Name", base: "expr", fields: &["id", "ctx"], attrs: None, doc: "Name(identifier id, expr_context ctx)" },
    NodeSpec { name: "NamedExpr", base: "expr", fields: &["target", "value"], attrs: None, doc: "NamedExpr(expr target, expr value)" },
    NodeSpec { name: "Nonlocal", base: "stmt", fields: &["names"], attrs: None, doc: "Nonlocal(identifier* names)" },
    NodeSpec { name: "Not", base: "unaryop", fields: &[], attrs: None, doc: "Not" },
    NodeSpec { name: "NotEq", base: "cmpop", fields: &[], attrs: None, doc: "NotEq" },
    NodeSpec { name: "NotIn", base: "cmpop", fields: &[], attrs: None, doc: "NotIn" },
    NodeSpec { name: "Or", base: "boolop", fields: &[], attrs: None, doc: "Or" },
    NodeSpec { name: "type_param", base: "AST", fields: &[], attrs: Some(&["lineno", "col_offset", "end_lineno", "end_col_offset"]), doc: "type_param = TypeVar(identifier name, expr? bound, expr? default_value)\n           | ParamSpec(identifier name, expr? default_value)\n           | TypeVarTuple(identifier name, expr? default_value)" },
    NodeSpec { name: "ParamSpec", base: "type_param", fields: &["name", "default_value"], attrs: None, doc: "ParamSpec(identifier name, expr? default_value)" },
    NodeSpec { name: "Pass", base: "stmt", fields: &[], attrs: None, doc: "Pass" },
    NodeSpec { name: "Pow", base: "operator", fields: &[], attrs: None, doc: "Pow" },
    NodeSpec { name: "RShift", base: "operator", fields: &[], attrs: None, doc: "RShift" },
    NodeSpec { name: "Raise", base: "stmt", fields: &["exc", "cause"], attrs: None, doc: "Raise(expr? exc, expr? cause)" },
    NodeSpec { name: "Return", base: "stmt", fields: &["value"], attrs: None, doc: "Return(expr? value)" },
    NodeSpec { name: "Set", base: "expr", fields: &["elts"], attrs: None, doc: "Set(expr* elts)" },
    NodeSpec { name: "SetComp", base: "expr", fields: &["elt", "generators"], attrs: None, doc: "SetComp(expr elt, comprehension* generators)" },
    NodeSpec { name: "Slice", base: "expr", fields: &["lower", "upper", "step"], attrs: None, doc: "Slice(expr? lower, expr? upper, expr? step)" },
    NodeSpec { name: "Starred", base: "expr", fields: &["value", "ctx"], attrs: None, doc: "Starred(expr value, expr_context ctx)" },
    NodeSpec { name: "Store", base: "expr_context", fields: &[], attrs: None, doc: "Store" },
    NodeSpec { name: "Sub", base: "operator", fields: &[], attrs: None, doc: "Sub" },
    NodeSpec { name: "Subscript", base: "expr", fields: &["value", "slice", "ctx"], attrs: None, doc: "Subscript(expr value, expr slice, expr_context ctx)" },
    NodeSpec { name: "TemplateStr", base: "expr", fields: &["values"], attrs: None, doc: "TemplateStr(expr* values)" },
    NodeSpec { name: "Try", base: "stmt", fields: &["body", "handlers", "orelse", "finalbody"], attrs: None, doc: "Try(stmt* body, excepthandler* handlers, stmt* orelse, stmt* finalbody)" },
    NodeSpec { name: "TryStar", base: "stmt", fields: &["body", "handlers", "orelse", "finalbody"], attrs: None, doc: "TryStar(stmt* body, excepthandler* handlers, stmt* orelse, stmt* finalbody)" },
    NodeSpec { name: "Tuple", base: "expr", fields: &["elts", "ctx"], attrs: None, doc: "Tuple(expr* elts, expr_context ctx)" },
    NodeSpec { name: "TypeAlias", base: "stmt", fields: &["name", "type_params", "value"], attrs: None, doc: "TypeAlias(expr name, type_param* type_params, expr value)" },
    NodeSpec { name: "type_ignore", base: "AST", fields: &[], attrs: Some(&[]), doc: "type_ignore = TypeIgnore(int lineno, string tag)" },
    NodeSpec { name: "TypeIgnore", base: "type_ignore", fields: &["lineno", "tag"], attrs: None, doc: "TypeIgnore(int lineno, string tag)" },
    NodeSpec { name: "TypeVar", base: "type_param", fields: &["name", "bound", "default_value"], attrs: None, doc: "TypeVar(identifier name, expr? bound, expr? default_value)" },
    NodeSpec { name: "TypeVarTuple", base: "type_param", fields: &["name", "default_value"], attrs: None, doc: "TypeVarTuple(identifier name, expr? default_value)" },
    NodeSpec { name: "UAdd", base: "unaryop", fields: &[], attrs: None, doc: "UAdd" },
    NodeSpec { name: "USub", base: "unaryop", fields: &[], attrs: None, doc: "USub" },
    NodeSpec { name: "UnaryOp", base: "expr", fields: &["op", "operand"], attrs: None, doc: "UnaryOp(unaryop op, expr operand)" },
    NodeSpec { name: "While", base: "stmt", fields: &["test", "body", "orelse"], attrs: None, doc: "While(expr test, stmt* body, stmt* orelse)" },
    NodeSpec { name: "With", base: "stmt", fields: &["items", "body", "type_comment"], attrs: None, doc: "With(withitem* items, stmt* body, string? type_comment)" },
    NodeSpec { name: "Yield", base: "expr", fields: &["value"], attrs: None, doc: "Yield(expr? value)" },
    NodeSpec { name: "YieldFrom", base: "expr", fields: &["value"], attrs: None, doc: "YieldFrom(expr value)" },
    NodeSpec { name: "alias", base: "AST", fields: &["name", "asname"], attrs: Some(&["lineno", "col_offset", "end_lineno", "end_col_offset"]), doc: "alias(identifier name, identifier? asname)" },
    NodeSpec { name: "arg", base: "AST", fields: &["arg", "annotation", "type_comment"], attrs: Some(&["lineno", "col_offset", "end_lineno", "end_col_offset"]), doc: "arg(identifier arg, expr? annotation, string? type_comment)" },
    NodeSpec { name: "arguments", base: "AST", fields: &["posonlyargs", "args", "vararg", "kwonlyargs", "kw_defaults", "kwarg", "defaults"], attrs: Some(&[]), doc: "arguments(arg* posonlyargs, arg* args, arg? vararg, arg* kwonlyargs, expr?* kw_defaults, arg? kwarg, expr* defaults)" },
    NodeSpec { name: "comprehension", base: "AST", fields: &["target", "iter", "ifs", "is_async"], attrs: Some(&[]), doc: "comprehension(expr target, expr iter, expr* ifs, int is_async)" },
    NodeSpec { name: "keyword", base: "AST", fields: &["arg", "value"], attrs: Some(&["lineno", "col_offset", "end_lineno", "end_col_offset"]), doc: "keyword(identifier? arg, expr value)" },
    NodeSpec { name: "match_case", base: "AST", fields: &["pattern", "guard", "body"], attrs: Some(&[]), doc: "match_case(pattern pattern, expr? guard, stmt* body)" },
    NodeSpec { name: "withitem", base: "AST", fields: &["context_expr", "optional_vars"], attrs: Some(&[]), doc: "withitem(expr context_expr, expr? optional_vars)" },
];

/// Class registry for [`build_ast_object`]: node-class name -> live class
/// object address, populated once by [`make_module`].  `OnceLock` (not
/// `LazyLock`): the values are runtime-built heap classes, so the initializer
/// cannot exist at declaration time.
static CLASSES: OnceLock<HashMap<&'static str, usize>> = OnceLock::new();

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let list_field_marker = list_field_marker_class()?;
    let mut classes: Vec<(&'static str, *mut PyObject)> = Vec::with_capacity(NODES.len());
    for spec in NODES {
        let base = if spec.base.is_empty() {
            ptr::null_mut()
        } else {
            classes
                .iter()
                .find(|&&(name, _)| name == spec.base)
                .map(|&(_, class)| class)
                .ok_or_else(|| format!("_ast table order broken: base '{}' of '{}' not built yet", spec.base, spec.name))?
        };
        classes.push((spec.name, ast_class(spec, base, list_field_marker)?));
    }
    let _ = CLASSES.set(classes.iter().map(|&(name, class)| (name, class as usize)).collect());
    let mut attrs = Vec::with_capacity(classes.len() + 5);
    attrs.push(string_attr("__name__", "_ast")?);
    // CPython 3.14 compiler-flag constants re-exported by `ast.py`.
    attrs.push(int_attr("PyCF_ALLOW_TOP_LEVEL_AWAIT", 8192)?);
    attrs.push(int_attr("PyCF_ONLY_AST", 1024)?);
    attrs.push(int_attr("PyCF_OPTIMIZED_AST", 33792)?);
    attrs.push(int_attr("PyCF_TYPE_COMMENTS", 4096)?);
    for (name, class) in classes {
        attrs.push((intern(name), class));
    }
    install_module("_ast", attrs)
}

/// Builds one `_ast` heap class: `__doc__`/`__module__`/`_fields`/
/// `__match_args__` (+ own `_attributes` for category classes) in the
/// namespace, then a real heap type with [`ast_node_new`] as its constructor.
/// Leaf classes additionally get the CPython 3.13+ dump-parity class data:
/// class-level `None` for `?`-optional fields and a `_field_types` dict
/// mapping `*`-list fields to the shared list marker class.
fn ast_class(spec: &NodeSpec, base: *mut PyObject, list_field_marker: *mut PyObject) -> Result<*mut PyObject, String> {
    let namespace = type_::new_namespace();
    if namespace.is_null() {
        return Err(format!("failed to allocate _ast.{} namespace", spec.name));
    }
    let doc = unsafe { pon_const_str(spec.doc.as_ptr(), spec.doc.len()) };
    if doc.is_null() {
        return Err(format!("failed to allocate _ast.{}.__doc__", spec.name));
    }
    // CPython AST classes report `__module__ == "ast"`, not "_ast".
    let module = unsafe { pon_const_str("ast".as_ptr(), "ast".len()) };
    if module.is_null() {
        return Err(format!("failed to allocate _ast.{}.__module__", spec.name));
    }
    let fields = str_tuple(spec.name, "_fields", spec.fields)?;
    // SAFETY: `new_namespace` returned a live namespace box; values are live.
    unsafe {
        let namespace = &mut *namespace;
        namespace.set(intern("__doc__"), doc);
        namespace.set(intern("__module__"), module);
        namespace.set(intern("_fields"), fields);
        // CPython: `__match_args__` IS the `_fields` tuple (same object).
        namespace.set(intern("__match_args__"), fields);
        if let Some(names) = spec.attrs {
            namespace.set(intern("_attributes"), str_tuple(spec.name, "_attributes", names)?);
        }
        let none = crate::abi::pon_none();
        if none.is_null() {
            return Err(format!("failed to fetch None for _ast.{} field defaults", spec.name));
        }
        for name in optional_field_names(spec) {
            namespace.set(intern(name), none);
        }
        namespace.set(intern("_field_types"), field_types_dict(spec, list_field_marker)?);
    }
    let bases = if base.is_null() { &[][..] } else { std::slice::from_ref(&base) };
    // SAFETY: Bases are live class objects owned by this module build.
    let class = unsafe { type_::build_class_from_namespace(spec.name, bases, namespace, &[]) };
    if class.is_null() {
        let detail = pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
        pon_err_clear();
        return Err(format!("failed to create _ast.{}: {detail}", spec.name));
    }
    // SAFETY: Freshly built class; mirror `pon_build_class`'s ob_type fix,
    // then install complete-construction `tp_new` (see module docs).
    unsafe {
        if (*class).ob_type.is_null() {
            (*class).ob_type = abi::runtime_type_type().cast_const();
        }
        (*class.cast::<PyType>()).tp_new = Some(ast_node_new);
    }
    Ok(class)
}

/// `tp_new` shared by every `_ast` node class: allocates the plain heap
/// instance, zips positional args against the class `_fields` (MRO lookup —
/// Python subclasses reach here through `object.__new__` chains), and stores
/// keyword args as instance attributes.  CPython `ast_type_init` error
/// shapes: too many positionals and positional/keyword duplicates raise
/// TypeError.
unsafe extern "C" fn ast_node_new(cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    let instance = unsafe { type_::type_new(cls, ptr::null_mut(), ptr::null_mut()) };
    if instance.is_null() {
        return ptr::null_mut();
    }
    let positional = match unsafe { type_::positional_args_from_object(args) } {
        Ok(values) => values,
        Err(message) => return fail(message),
    };
    let class_name = unsafe { (*cls).name() };
    let fields_tuple = unsafe { crate::descr::lookup_in_type(cls, intern("_fields")) };
    let field_names = if fields_tuple.is_null() {
        Vec::new()
    } else {
        match unsafe { type_::positional_args_from_object(fields_tuple) } {
            Ok(values) => values,
            Err(message) => return fail(message),
        }
    };
    if positional.len() > field_names.len() {
        let suffix = if field_names.len() == 1 { "" } else { "s" };
        return raise_type_error(&format!(
            "{class_name} constructor takes at most {} positional argument{suffix}",
            field_names.len()
        ));
    }
    let mut assigned = Vec::with_capacity(positional.len());
    for (&field, &value) in field_names.iter().zip(positional.iter()) {
        let Some(field_name) = (unsafe { type_::unicode_text(crate::tag::untag_arg(field)) }) else {
            return raise_type_error(&format!("{class_name}._fields entries must be strings"));
        };
        let name_id = intern(field_name);
        assigned.push(name_id);
        // SAFETY: Store helper untags the value and enforces the
        // NULL-sentinel error contract.
        if unsafe { abi::attr::pon_store_attr(instance, name_id, value) }.is_null() {
            return ptr::null_mut();
        }
    }
    if kwargs.is_null() {
        return instance;
    }
    let entries = match unsafe { crate::types::dict::dict_entries_snapshot(kwargs) } {
        Ok(entries) => entries,
        Err(message) => return fail(message),
    };
    for entry in entries {
        let Some(keyword) = (unsafe { type_::unicode_text(crate::tag::untag_arg(entry.key)) }) else {
            return raise_type_error(&format!("{class_name}() keywords must be strings"));
        };
        let name_id = intern(keyword);
        if assigned.contains(&name_id) {
            return raise_type_error(&format!("{class_name} got multiple values for argument '{keyword}'"));
        }
        // SAFETY: Same store contract as the positional leg above.
        if unsafe { abi::attr::pon_store_attr(instance, name_id, entry.value) }.is_null() {
            return ptr::null_mut();
        }
    }
    instance
}

/// Allocates the tuple-of-strings payload for a class-data attr row.
fn str_tuple(class_name: &str, attr_name: &str, names: &[&str]) -> Result<*mut PyObject, String> {
    let mut items = Vec::with_capacity(names.len());
    for name in names {
        // SAFETY: Allocation helpers return NULL with a diagnostic on failure.
        let item = unsafe { pon_const_str(name.as_ptr(), name.len()) };
        if item.is_null() {
            return Err(format!("failed to allocate _ast.{class_name}.{attr_name} entry '{name}'"));
        }
        items.push(item);
    }
    // SAFETY: `items` is a live contiguous slice for the duration of the call.
    let tuple = unsafe { abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) };
    if tuple.is_null() {
        return Err(format!("failed to allocate _ast.{class_name}.{attr_name}"));
    }
    Ok(tuple)
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate _ast.{name}"))
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: `pon_const_int` returns NULL with a diagnostic on failure.
    let object = unsafe { abi::pon_const_int(value) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate _ast.{name}"))
}

fn fail(message: impl Into<String>) -> *mut PyObject {
    crate::thread_state::pon_err_set(message);
    ptr::null_mut()
}

fn raise_type_error(message: &str) -> *mut PyObject {
    abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

/// Names of `spec`'s fields whose ASDL type in the leaf signature docstring
/// ends with exactly `?` (optional): these get a class-level `None` default,
/// which `ast.dump` probes via `getattr(cls, name, ...) is None` to omit
/// unset/None fields.  `?*` is a list of optionals, not an optional field.
fn optional_field_names(spec: &NodeSpec) -> Vec<&'static str> {
    signature_entries(spec)
        .filter(|&(ty, _)| ty.ends_with('?'))
        .map(|(_, name)| name)
        .collect()
}

/// `_field_types` payload: a dict mapping each `*`-list field to the shared
/// list marker class.  `ast.dump` only consults `_field_types.get(name,
/// object).__origin__ is list` to omit empty list fields, so non-list fields
/// are simply absent (the `object` default has no `__origin__`).
fn field_types_dict(spec: &NodeSpec, list_field_marker: *mut PyObject) -> Result<*mut PyObject, String> {
    // SAFETY: An empty Vec's dangling pointer is valid for a zero-length read.
    let dict = unsafe { abi::map::pon_build_map(Vec::new().as_mut_ptr(), 0) };
    if dict.is_null() {
        return Err(format!("failed to allocate _ast.{}._field_types", spec.name));
    }
    for (ty, name) in signature_entries(spec) {
        if !ty.ends_with('*') {
            continue;
        }
        // SAFETY: Allocation helpers return NULL with a diagnostic on failure.
        let key = unsafe { pon_const_str(name.as_ptr(), name.len()) };
        if key.is_null() {
            return Err(format!("failed to allocate _ast.{}._field_types key '{name}'", spec.name));
        }
        // SAFETY: `dict`, `key`, and the marker class are live heap objects.
        if unsafe { abi::map::pon_map_insert(dict, key, list_field_marker) }.is_null() {
            return Err(drain_error(format!("failed to populate _ast.{}._field_types", spec.name)));
        }
    }
    Ok(dict)
}

/// Parses the flat leaf ASDL signature in `spec.doc` — e.g. `Assign(expr*
/// targets, expr value, string? type_comment)` — into `(type, name)` entries.
/// Category classes have empty `fields` and multi-signature docs; they yield
/// nothing.
fn signature_entries(spec: &NodeSpec) -> impl Iterator<Item = (&'static str, &'static str)> {
    let body = if spec.fields.is_empty() {
        None
    } else {
        spec.doc
            .find('(')
            .and_then(|open| spec.doc.rfind(')').map(|close| &spec.doc[open + 1..close]))
    };
    body.unwrap_or("").split(',').filter_map(|entry| {
        let mut parts = entry.split_whitespace();
        match (parts.next(), parts.next()) {
            (Some(ty), Some(name)) => Some((ty, name)),
            _ => None,
        }
    })
}

/// Builds the shared `_field_types` list marker: a minimal heap class whose
/// only class datum is `__origin__ = <builtins.list>`.  `ast.dump` treats any
/// value with `__origin__ is list` as a list-typed field; full `typing`
/// generic aliases are deliberately not modeled (documented divergence:
/// `repr(SomeNode._field_types)` differs from CPython).
fn list_field_marker_class() -> Result<*mut PyObject, String> {
    // SAFETY: Global loads return NULL with the runtime error set on failure.
    let list_type = unsafe { abi::pon_load_global(intern("list"), ptr::null_mut()) };
    if list_type.is_null() {
        return Err(drain_error("builtin `list` unavailable while building _ast"));
    }
    let namespace = type_::new_namespace();
    if namespace.is_null() {
        return Err("failed to allocate _ast list-field marker namespace".to_owned());
    }
    // SAFETY: `new_namespace` returned a live namespace box; `list_type` is live.
    unsafe {
        (*namespace).set(intern("__origin__"), list_type);
    }
    // SAFETY: Empty bases slice; namespace ownership transfers to the class.
    let class = unsafe { type_::build_class_from_namespace("_ListFieldType", &[], namespace, &[]) };
    if class.is_null() {
        return Err(drain_error("failed to create _ast list-field marker class"));
    }
    // SAFETY: Freshly built class; mirror `pon_build_class`'s ob_type fix.
    unsafe {
        if (*class).ob_type.is_null() {
            (*class).ob_type = abi::runtime_type_type().cast_const();
        }
    }
    Ok(class)
}

/// Reads and clears the pending runtime error, prefixing it with `context`.
fn drain_error(context: impl Into<String>) -> String {
    let context = context.into();
    let Some(detail) = pon_err_message() else {
        return context;
    };
    pon_err_clear();
    format!("{context}: {detail}")
}

/// Location attributes for one node of a ruff-parsed tree: 1-based lines and
/// 0-based UTF-8 byte column offsets, CPython's `ast` convention.  Present
/// only on nodes whose class has location `_attributes`.
#[derive(Clone, Copy, Debug)]
pub struct NodeSpan {
    pub lineno: u32,
    pub col_offset: u32,
    pub end_lineno: u32,
    pub end_col_offset: u32,
}

/// One `_ast` node of the neutral tree produced by the host's ruff-AST
/// converter: class name from [`NODES`], field values in `_fields` order,
/// and optional location attributes.
#[derive(Debug)]
pub struct AstNode {
    pub class: &'static str,
    pub span: Option<NodeSpan>,
    pub fields: Vec<(&'static str, AstValue)>,
}

/// One field value of an [`AstNode`]: the closed set of Python values the
/// CPython 3.14 ASDL schema stores in AST fields.
#[derive(Debug)]
pub enum AstValue {
    /// Python `None` (absent optional field or `Constant(None)`).
    None,
    /// Python `Ellipsis` (`Constant(...)`).
    Ellipsis,
    Bool(bool),
    Int(i64),
    /// Integer literal token wider than `i64`, in `pon_const_bigint` syntax
    /// (decimal or `0b`/`0o`/`0x` prefixed, `_` separators allowed).
    BigInt(String),
    Float(f64),
    Complex { real: f64, imag: f64 },
    Str(String),
    Bytes(Vec<u8>),
    Node(Box<AstNode>),
    List(Vec<AstValue>),
}

/// Materializes a neutral tree as real `_ast` instances.  Ensures the `_ast`
/// module exists first (a direct `compile(..., PyCF_ONLY_AST)` call needs no
/// prior `import ast`), so class identities always match the import cache.
pub fn build_ast_object(root: &AstNode) -> Result<*mut PyObject, String> {
    ensure_ast_classes()?;
    build_node(root)
}

/// Guarantees [`CLASSES`] is populated by forcing the `_ast` module through
/// the normal import-cache path exactly once.
fn ensure_ast_classes() -> Result<(), String> {
    if CLASSES.get().is_some() {
        return Ok(());
    }
    if crate::import::cached_module(intern("_ast")).is_none() {
        super::make_module("_ast")?;
    }
    if CLASSES.get().is_some() {
        Ok(())
    } else {
        Err("_ast class registry unavailable after module construction".to_owned())
    }
}

fn class_object(name: &str) -> Result<*mut PyObject, String> {
    CLASSES
        .get()
        .and_then(|map| map.get(name).copied())
        .map(|address| address as *mut PyObject)
        .ok_or_else(|| format!("_ast has no node class named '{name}'"))
}

fn build_node(node: &AstNode) -> Result<*mut PyObject, String> {
    let class = class_object(node.class)?;
    // SAFETY: Registry entries are live `_ast` heap classes built by
    // `make_module`; `type_new` allocates a plain instance of `class`.
    let instance = unsafe { type_::type_new(class.cast::<PyType>(), ptr::null_mut(), ptr::null_mut()) };
    if instance.is_null() {
        return Err(drain_error(format!("failed to allocate _ast.{} instance", node.class)));
    }
    for (field, value) in &node.fields {
        let object = build_value(value)?;
        store_field(instance, node.class, field, object)?;
    }
    if let Some(span) = &node.span {
        for (name, value) in [
            ("lineno", i64::from(span.lineno)),
            ("col_offset", i64::from(span.col_offset)),
            ("end_lineno", i64::from(span.end_lineno)),
            ("end_col_offset", i64::from(span.end_col_offset)),
        ] {
            // SAFETY: `pon_const_int` returns NULL with a diagnostic on failure.
            let object = unsafe { abi::pon_const_int(value) };
            if object.is_null() {
                return Err(drain_error(format!("failed to allocate _ast.{}.{name}", node.class)));
            }
            store_field(instance, node.class, name, object)?;
        }
    }
    Ok(instance)
}

fn store_field(instance: *mut PyObject, class: &str, field: &str, value: *mut PyObject) -> Result<(), String> {
    // SAFETY: The store helper untags the value and enforces the
    // NULL-sentinel error contract (same leg as `ast_node_new`).
    if unsafe { abi::attr::pon_store_attr(instance, intern(field), value) }.is_null() {
        return Err(drain_error(format!("failed to store _ast.{class}.{field}")));
    }
    Ok(())
}

fn build_value(value: &AstValue) -> Result<*mut PyObject, String> {
    // SAFETY: Every constructor below returns NULL with a diagnostic on
    // failure; inputs are plain scalars or live objects built here.
    let object = match value {
        AstValue::None => unsafe { abi::pon_none() },
        AstValue::Ellipsis => unsafe { abi::pon_load_global(intern("Ellipsis"), ptr::null_mut()) },
        AstValue::Bool(value) => unsafe { abi::number::pon_const_bool(i32::from(*value)) },
        AstValue::Int(value) => unsafe { abi::pon_const_int(*value) },
        AstValue::BigInt(token) => unsafe { abi::number::pon_const_bigint(token.as_ptr(), token.len()) },
        AstValue::Float(value) => unsafe { abi::number::pon_const_float(*value) },
        AstValue::Complex { real, imag } => unsafe { abi::number::pon_const_complex(*real, *imag) },
        AstValue::Str(text) => unsafe { pon_const_str(text.as_ptr(), text.len()) },
        AstValue::Bytes(bytes) => unsafe { abi::str_::pon_const_bytes(bytes.as_ptr(), bytes.len()) },
        AstValue::Node(node) => return build_node(node),
        AstValue::List(items) => {
            let mut built = Vec::with_capacity(items.len());
            for item in items {
                built.push(build_value(item)?);
            }
            // SAFETY: `built` is a live contiguous slice for the call; an
            // empty Vec's dangling pointer is valid for a zero-length read.
            unsafe { abi::seq::pon_build_list(built.as_mut_ptr(), built.len()) }
        }
    };
    if object.is_null() {
        return Err(drain_error("failed to build _ast field value"));
    }
    Ok(object)
}
