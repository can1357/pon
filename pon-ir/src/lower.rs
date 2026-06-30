//! Lowering from Ruff's Python AST into Phase-A pon IR.

use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};

use ruff_python_ast::{
    Expr, ExprContext, ModModule, Number, Operator, Parameters, Stmt, StmtAssign, StmtFunctionDef,
};

use crate::desugar::desugar_module;
use crate::ir::{
    BinOp, Block, BlockId, Function, FunctionId, Inst, InstKind, Module, PyConst, Terminator,
    Value,
};
use crate::parse::parse_module_source;

/// Error returned when parsing or lowering cannot produce Phase-A IR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LowerError {
    /// Ruff rejected the source before lowering.
    Parse(String),
    /// The source uses Python syntax outside the Phase-A slice.
    Unsupported(String),
    /// A numeric literal is syntactically valid Python but not representable in Phase A.
    InvalidInteger(String),
    /// The lowerer hit an internal capacity or consistency limit.
    Internal(String),
}

impl LowerError {
    /// Build a parse error from Ruff's diagnostic text.
    #[must_use]
    pub fn parse(message: impl Into<String>) -> Self {
        Self::Parse(message.into())
    }

    /// Build an unsupported-construct error.
    #[must_use]
    pub fn unsupported(construct: impl Into<String>) -> Self {
        Self::Unsupported(construct.into())
    }

    fn invalid_integer(message: impl Into<String>) -> Self {
        Self::InvalidInteger(message.into())
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }
}

impl Display for LowerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            LowerError::Parse(message) => write!(f, "failed to parse Python module: {message}"),
            LowerError::Unsupported(construct) => {
                write!(f, "unsupported Phase-A Python construct: {construct}")
            }
            LowerError::InvalidInteger(message) => {
                write!(f, "unsupported Phase-A integer literal: {message}")
            }
            LowerError::Internal(message) => write!(f, "internal lowering error: {message}"),
        }
    }
}

impl Error for LowerError {}

/// Parse and lower Python source into a Phase-A IR module.
pub fn lower_source(source: &str) -> Result<Module, LowerError> {
    let parsed = parse_module_source(source)?;
    lower_module(&parsed)
}

/// Lower a Ruff module AST into a Phase-A IR module.
pub fn lower_module(module: &ModModule) -> Result<Module, LowerError> {
    Lowerer::new().lower_module(module).map(desugar_module)
}

#[derive(Default)]
struct NameTable {
    names: Vec<String>,
    ids: HashMap<String, u32>,
}

impl NameTable {
    fn intern(&mut self, name: &str) -> Result<u32, LowerError> {
        if let Some(id) = self.ids.get(name) {
            return Ok(*id);
        }

        let id = u32::try_from(self.names.len())
            .map_err(|_| LowerError::internal("too many interned names for u32 ids"))?;
        let owned = name.to_owned();
        self.names.push(owned.clone());
        self.ids.insert(owned, id);
        Ok(id)
    }
}

struct Lowerer {
    functions: Vec<Function>,
    names: NameTable,
}

impl Lowerer {
    fn new() -> Self {
        Self {
            functions: Vec::new(),
            names: NameTable::default(),
        }
    }

    fn lower_module(mut self, module: &ModModule) -> Result<Module, LowerError> {
        let main = self.reserve_function("__main__")?;
        let mut scope = Scope::module("__main__");

        for stmt in &module.body {
            self.lower_stmt(&mut scope, stmt)?;
        }

        let main_function = scope.finish()?;
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

    fn lower_function_def(&mut self, def: &StmtFunctionDef) -> Result<FunctionId, LowerError> {
        validate_function_header(def)?;

        let params = positional_parameters(&def.parameters)?;
        let mut scope = Scope::function(def.name.as_str(), params.len());
        for parameter in &params {
            scope.declare_local(parameter)?;
        }
        collect_function_locals(&def.body, &mut scope)?;

        for stmt in &def.body {
            self.lower_stmt(&mut scope, stmt)?;
        }

        let function = scope.finish()?;
        self.append_function(function)
    }

    fn lower_stmt(&mut self, scope: &mut Scope, stmt: &Stmt) -> Result<(), LowerError> {
        if scope.is_terminated() {
            return Err(LowerError::unsupported("statement after return"));
        }

        match stmt {
            Stmt::FunctionDef(def) => self.lower_function_def_stmt(scope, def),
            Stmt::Return(ret) => {
                if scope.is_module() {
                    return Err(LowerError::unsupported("top-level return statement"));
                }
                let value = match ret.value.as_deref() {
                    Some(expr) => self.lower_expr(scope, expr)?,
                    None => scope.emit(InstKind::Const(PyConst::None))?,
                };
                scope.set_term(Terminator::Return(value))
            }
            Stmt::Expr(expr_stmt) => {
                self.lower_expr(scope, &expr_stmt.value)?;
                Ok(())
            }
            Stmt::Assign(assign) => self.lower_assign(scope, assign),
            Stmt::ClassDef(_) => Err(LowerError::unsupported("class definition")),
            Stmt::Delete(_) => Err(LowerError::unsupported("delete statement")),
            Stmt::TypeAlias(_) => Err(LowerError::unsupported("type alias statement")),
            Stmt::AugAssign(_) => Err(LowerError::unsupported("augmented assignment")),
            Stmt::AnnAssign(_) => Err(LowerError::unsupported("annotated assignment")),
            Stmt::For(_) => Err(LowerError::unsupported("for statement")),
            Stmt::While(_) => Err(LowerError::unsupported("while statement")),
            Stmt::If(_) => Err(LowerError::unsupported("if statement")),
            Stmt::With(_) => Err(LowerError::unsupported("with statement")),
            Stmt::Match(_) => Err(LowerError::unsupported("match statement")),
            Stmt::Raise(_) => Err(LowerError::unsupported("raise statement")),
            Stmt::Try(_) => Err(LowerError::unsupported("try statement")),
            Stmt::Assert(_) => Err(LowerError::unsupported("assert statement")),
            Stmt::Import(_) => Err(LowerError::unsupported("import statement")),
            Stmt::ImportFrom(_) => Err(LowerError::unsupported("import-from statement")),
            Stmt::Global(_) => Err(LowerError::unsupported("global statement")),
            Stmt::Nonlocal(_) => Err(LowerError::unsupported("nonlocal statement")),
            Stmt::Pass(_) => Err(LowerError::unsupported("pass statement")),
            Stmt::Break(_) => Err(LowerError::unsupported("break statement")),
            Stmt::Continue(_) => Err(LowerError::unsupported("continue statement")),
            Stmt::IpyEscapeCommand(_) => Err(LowerError::unsupported("IPython escape command")),
        }
    }

    fn lower_function_def_stmt(
        &mut self,
        scope: &mut Scope,
        def: &StmtFunctionDef,
    ) -> Result<(), LowerError> {
        let name = def.name.as_str();
        let name_interned = self.names.intern(name)?;
        let function = self.lower_function_def(def)?;
        let arity = positional_parameters(&def.parameters)?.len();
        let value = scope.emit(InstKind::MakeFunction {
            func_index: function.0,
            name_interned,
            arity,
        })?;

        if scope.is_module() {
            scope.emit(InstKind::StoreGlobal(name_interned, value))?;
        } else {
            let slot = scope.local_slot(name)?;
            scope.emit(InstKind::StoreLocal(slot, value))?;
        }
        Ok(())
    }

    fn lower_assign(&mut self, scope: &mut Scope, assign: &StmtAssign) -> Result<(), LowerError> {
        let value = self.lower_expr(scope, &assign.value)?;
        for target in &assign.targets {
            self.lower_store_target(scope, target, value)?;
        }
        Ok(())
    }

    fn lower_store_target(
        &mut self,
        scope: &mut Scope,
        target: &Expr,
        value: Value,
    ) -> Result<(), LowerError> {
        match target {
            Expr::Name(name) if matches!(name.ctx, ExprContext::Store) => {
                let raw_name = name.id.as_str();
                if scope.is_module() {
                    let name_id = self.names.intern(raw_name)?;
                    scope.emit(InstKind::StoreGlobal(name_id, value))?;
                } else {
                    let slot = scope.local_slot(raw_name)?;
                    scope.emit(InstKind::StoreLocal(slot, value))?;
                }
                Ok(())
            }
            Expr::Name(_) => Err(LowerError::unsupported("non-store name assignment target")),
            _ => Err(LowerError::unsupported("non-name assignment target")),
        }
    }

    fn lower_expr(&mut self, scope: &mut Scope, expr: &Expr) -> Result<Value, LowerError> {
        match expr {
            Expr::Name(name) if matches!(name.ctx, ExprContext::Load) => {
                if let Some(slot) = scope.lookup_local(name.id.as_str()) {
                    scope.emit(InstKind::LoadLocal(slot))
                } else {
                    let name_id = self.names.intern(name.id.as_str())?;
                    scope.emit(InstKind::LoadName(name_id))
                }
            }
            Expr::Name(_) => Err(LowerError::unsupported("non-load name expression")),
            Expr::Call(call) => {
                if !call.arguments.keywords.is_empty() {
                    return Err(LowerError::unsupported("keyword call argument"));
                }

                let callee = self.lower_expr(scope, &call.func)?;
                let mut args = Vec::with_capacity(call.arguments.args.len());
                for arg in &call.arguments.args {
                    args.push(self.lower_expr(scope, arg)?);
                }
                scope.emit(InstKind::Call { callee, args })
            }
            Expr::BinOp(binop) => {
                let op = match binop.op {
                    Operator::Add => BinOp::Add,
                    _ => return Err(LowerError::unsupported("non-add binary operator")),
                };
                let lhs = self.lower_expr(scope, &binop.left)?;
                let rhs = self.lower_expr(scope, &binop.right)?;
                scope.emit(InstKind::BinaryOp { op, lhs, rhs })
            }
            Expr::StringLiteral(literal) => {
                scope.emit(InstKind::Const(PyConst::Str(literal.value.to_str().to_owned())))
            }
            Expr::NumberLiteral(literal) => match &literal.value {
                Number::Int(value) => {
                    let value = value.as_i64().ok_or_else(|| {
                        LowerError::invalid_integer(format!("{value} does not fit in i64"))
                    })?;
                    scope.emit(InstKind::Const(PyConst::Int(value)))
                }
                Number::Float(_) => Err(LowerError::unsupported("float literal")),
                Number::Complex { .. } => Err(LowerError::unsupported("complex literal")),
            },
            Expr::NoneLiteral(_) => scope.emit(InstKind::Const(PyConst::None)),
            Expr::BoolOp(_) => Err(LowerError::unsupported("boolean operation")),
            Expr::Named(_) => Err(LowerError::unsupported("assignment expression")),
            Expr::UnaryOp(_) => Err(LowerError::unsupported("unary operation")),
            Expr::Lambda(_) => Err(LowerError::unsupported("lambda expression")),
            Expr::If(_) => Err(LowerError::unsupported("conditional expression")),
            Expr::Dict(_) => Err(LowerError::unsupported("dict literal")),
            Expr::Set(_) => Err(LowerError::unsupported("set literal")),
            Expr::ListComp(_) => Err(LowerError::unsupported("list comprehension")),
            Expr::SetComp(_) => Err(LowerError::unsupported("set comprehension")),
            Expr::DictComp(_) => Err(LowerError::unsupported("dict comprehension")),
            Expr::Generator(_) => Err(LowerError::unsupported("generator expression")),
            Expr::Await(_) => Err(LowerError::unsupported("await expression")),
            Expr::Yield(_) => Err(LowerError::unsupported("yield expression")),
            Expr::YieldFrom(_) => Err(LowerError::unsupported("yield-from expression")),
            Expr::Compare(_) => Err(LowerError::unsupported("comparison expression")),
            Expr::FString(_) => Err(LowerError::unsupported("f-string expression")),
            Expr::TString(_) => Err(LowerError::unsupported("t-string expression")),
            Expr::BytesLiteral(_) => Err(LowerError::unsupported("bytes literal")),
            Expr::BooleanLiteral(_) => Err(LowerError::unsupported("boolean literal")),
            Expr::EllipsisLiteral(_) => Err(LowerError::unsupported("ellipsis literal")),
            Expr::Attribute(_) => Err(LowerError::unsupported("attribute expression")),
            Expr::Subscript(_) => Err(LowerError::unsupported("subscript expression")),
            Expr::Starred(_) => Err(LowerError::unsupported("starred expression")),
            Expr::List(_) => Err(LowerError::unsupported("list literal")),
            Expr::Tuple(_) => Err(LowerError::unsupported("tuple literal")),
            Expr::Slice(_) => Err(LowerError::unsupported("slice expression")),
            Expr::IpyEscapeCommand(_) => Err(LowerError::unsupported("IPython escape expression")),
        }
    }
}

struct Scope {
    kind: ScopeKind,
    name: String,
    arity: usize,
    locals: HashMap<String, u32>,
    next_local: u32,
    insts: Vec<Inst>,
    next_value: u32,
    term: Option<Terminator>,
}

impl Scope {
    fn module(name: &str) -> Self {
        Self::new(ScopeKind::Module, name, 0)
    }

    fn function(name: &str, arity: usize) -> Self {
        Self::new(ScopeKind::Function, name, arity)
    }

    fn new(kind: ScopeKind, name: &str, arity: usize) -> Self {
        Self {
            kind,
            name: name.to_owned(),
            arity,
            locals: HashMap::new(),
            next_local: 0,
            insts: Vec::new(),
            next_value: 0,
            term: None,
        }
    }

    fn is_module(&self) -> bool {
        matches!(self.kind, ScopeKind::Module)
    }

    fn is_terminated(&self) -> bool {
        self.term.is_some()
    }

    fn declare_local(&mut self, name: &str) -> Result<u32, LowerError> {
        if let Some(slot) = self.locals.get(name) {
            return Ok(*slot);
        }

        let slot = self.next_local;
        self.next_local = self
            .next_local
            .checked_add(1)
            .ok_or_else(|| LowerError::internal("too many local slots for u32 ids"))?;
        self.locals.insert(name.to_owned(), slot);
        Ok(slot)
    }

    fn local_slot(&self, name: &str) -> Result<u32, LowerError> {
        self.lookup_local(name)
            .ok_or_else(|| LowerError::internal(format!("local slot for `{name}` was not declared")))
    }

    fn lookup_local(&self, name: &str) -> Option<u32> {
        if self.is_module() {
            None
        } else {
            self.locals.get(name).copied()
        }
    }

    fn emit(&mut self, kind: InstKind) -> Result<Value, LowerError> {
        if self.is_terminated() {
            return Err(LowerError::unsupported("instruction after terminator"));
        }

        let result = Value(self.next_value);
        self.next_value = self
            .next_value
            .checked_add(1)
            .ok_or_else(|| LowerError::internal("too many SSA values for u32 ids"))?;
        self.insts.push(Inst { result, kind });
        Ok(result)
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
            .ok_or_else(|| LowerError::internal("finished function without terminator"))?;
        Ok(Function {
            name: self.name,
            arity: self.arity,
            blocks: vec![Block {
                id: BlockId(0),
                insts: self.insts,
                term,
            }],
            n_locals: self.next_local as usize,
        })
    }
}

#[derive(Clone, Copy)]
enum ScopeKind {
    Module,
    Function,
}

fn validate_function_header(def: &StmtFunctionDef) -> Result<(), LowerError> {
    if def.is_async {
        return Err(LowerError::unsupported("async function definition"));
    }
    if !def.decorator_list.is_empty() {
        return Err(LowerError::unsupported("function decorator"));
    }
    if def.type_params.is_some() {
        return Err(LowerError::unsupported("function type parameter"));
    }
    if def.returns.is_some() {
        return Err(LowerError::unsupported("function return annotation"));
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
        if parameter.default.is_some() {
            return Err(LowerError::unsupported("default parameter value"));
        }
        if parameter.annotation().is_some() {
            return Err(LowerError::unsupported("parameter annotation"));
        }
        params.push(parameter.name().as_str().to_owned());
    }
    Ok(params)
}

fn collect_function_locals(body: &[Stmt], scope: &mut Scope) -> Result<(), LowerError> {
    for stmt in body {
        match stmt {
            Stmt::FunctionDef(def) => {
                scope.declare_local(def.name.as_str())?;
            }
            Stmt::Assign(assign) => {
                for target in &assign.targets {
                    collect_assignment_target(target, scope)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn collect_assignment_target(target: &Expr, scope: &mut Scope) -> Result<(), LowerError> {
    match target {
        Expr::Name(name) if matches!(name.ctx, ExprContext::Store) => {
            scope.declare_local(name.id.as_str())?;
            Ok(())
        }
        Expr::Name(_) => Err(LowerError::unsupported("non-store name assignment target")),
        _ => Err(LowerError::unsupported("non-name assignment target")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::InstKind;

    #[test]
    fn lowers_phase_a_hello_shape() {
        let module = lower_source(
            r#"
def add(a, b):
    return a + b

print("hello")
print(add(1, 2))
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
        assert_eq!(main_block.id, BlockId(0));
        assert_eq!(main_block.term, Terminator::Return(Value(11)));
        assert!(matches!(
            &main_block.insts[0].kind,
            InstKind::MakeFunction {
                func_index: 1,
                name_interned: 0,
                arity: 2
            }
        ));
        assert_eq!(main_block.insts[1].kind, InstKind::StoreGlobal(0, Value(0)));
        assert_eq!(main_block.insts[2].kind, InstKind::LoadName(1));
        assert_eq!(
            main_block.insts[3].kind,
            InstKind::Const(PyConst::Str("hello".to_owned()))
        );
        assert_eq!(
            main_block.insts[4].kind,
            InstKind::Call {
                callee: Value(2),
                args: vec![Value(3)]
            }
        );
        assert_eq!(main_block.insts[5].kind, InstKind::LoadName(1));
        assert_eq!(main_block.insts[6].kind, InstKind::LoadName(0));
        assert_eq!(main_block.insts[7].kind, InstKind::Const(PyConst::Int(1)));
        assert_eq!(main_block.insts[8].kind, InstKind::Const(PyConst::Int(2)));
        assert_eq!(
            main_block.insts[9].kind,
            InstKind::Call {
                callee: Value(6),
                args: vec![Value(7), Value(8)]
            }
        );
        assert_eq!(
            main_block.insts[10].kind,
            InstKind::Call {
                callee: Value(5),
                args: vec![Value(9)]
            }
        );
        assert_eq!(main_block.insts[11].kind, InstKind::Const(PyConst::None));

        let add = &module.functions[1];
        assert_eq!(add.name, "add");
        assert_eq!(add.arity, 2);
        assert_eq!(add.n_locals, 2);
        assert_eq!(add.blocks.len(), 1);
        let add_block = &add.blocks[0];
        assert_eq!(add_block.insts[0].kind, InstKind::LoadLocal(0));
        assert_eq!(add_block.insts[1].kind, InstKind::LoadLocal(1));
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
    fn rejects_unsupported_construct_with_useful_error() {
        let err = lower_source(
            r#"
if 1:
    print("nope")
"#,
        )
        .expect_err("if statements are outside the Phase-A slice");

        assert!(err.to_string().contains("if statement"));
    }
}
