//! Phase-A control-flow IR shared by the frontend and code generators.
//!
//! The IR intentionally keeps Python values opaque: every [`Value`] denotes a
//! boxed Python object at runtime. Name-bearing operations use deterministic
//! `u32` ids into [`Module::names`] so this crate does not depend on runtime
//! interning state.

/// A lowered Python module.
///
/// `functions[main.0]` is the synthetic `__main__` function. Every `u32` name id
/// in instruction operands indexes [`Module::names`].
#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    /// All lowered functions, including the synthetic top-level `__main__`.
    pub functions: Vec<Function>,
    /// Index of the synthetic top-level function.
    pub main: FunctionId,
    /// Deterministic source-local name table used by numeric name operands.
    pub names: Vec<String>,
}

/// Index of a function in [`Module::functions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FunctionId(pub u32);

/// A lowered Python function.
///
/// Blocks are stored in layout order; `blocks[0]` is the entry block. Parameters
/// occupy local slots `0..arity`, and `n_locals` includes both parameters and
/// compiler-discovered local bindings.
#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    /// Debug/source name. The synthetic module body is named `__main__`.
    pub name: String,
    /// Positional argument count accepted by the Phase-A ABI.
    pub arity: usize,
    /// Function control-flow blocks.
    pub blocks: Vec<Block>,
    /// Number of local slots needed by this function.
    pub n_locals: usize,
}

/// Index of a basic block in [`Function::blocks`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);

/// A basic block: straight-line instructions followed by one terminator.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    /// Stable block id. For Phase A every function has entry block `0`.
    pub id: BlockId,
    /// Straight-line instruction stream.
    pub insts: Vec<Inst>,
    /// Control-flow terminator.
    pub term: Terminator,
}

/// SSA result value produced by an [`Inst`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Value(pub u32);

/// Numeric identity of an instruction result.
///
/// Phase A uses instruction result ids as SSA values, so this is an alias for
/// [`Value`] rather than a separate namespace.
pub type InstId = Value;

/// A single SSA instruction.
#[derive(Clone, Debug, PartialEq)]
pub struct Inst {
    /// SSA result produced by this instruction.
    pub result: Value,
    /// Instruction payload.
    pub kind: InstKind,
}

/// Phase-A instruction set.
///
/// This enum is non-exhaustive so later Python coverage can add operations
/// without forcing downstream crates to assume Phase A is complete.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum InstKind {
    /// Materialize a Python constant.
    Const(PyConst),
    /// Load a function-local slot.
    LoadLocal(u32),
    /// Store a function-local slot.
    StoreLocal(u32, Value),
    /// Load a global by deterministic interned name id.
    LoadGlobal(u32),
    /// Store a global by deterministic interned name id.
    StoreGlobal(u32, Value),
    /// Load a Python name using LEGB rules; at module scope this is global load.
    LoadName(u32),
    /// Apply a binary Python operation.
    BinaryOp { op: BinOp, lhs: Value, rhs: Value },
    /// Call a Python callable with positional arguments.
    Call { callee: Value, args: Vec<Value> },
    /// Box a lowered function object.
    MakeFunction {
        /// Index of the lowered function in [`Module::functions`].
        func_index: u32,
        /// Deterministic interned id of the function's source name.
        name_interned: u32,
        /// Positional argument count.
        arity: usize,
    },
}

/// Basic-block terminator.
///
/// Phase A emits only [`Terminator::Return`] for supported source, but `Jump`,
/// `Branch`, and `Unreachable` reserve the CFG shape used by later phases and by
/// internal placeholder blocks before lowering is finalized.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum Terminator {
    /// Return a boxed Python value.
    Return(Value),
    /// Unconditional jump.
    Jump(BlockId),
    /// Conditional branch.
    Branch {
        /// Truth-tested condition value.
        cond: Value,
        /// Destination when `cond` is true.
        then_blk: BlockId,
        /// Destination when `cond` is false.
        else_blk: BlockId,
    },
    /// No valid control-flow continuation.
    Unreachable,
}

/// Python constants representable directly in Phase-A IR.
#[derive(Clone, Debug, PartialEq)]
pub enum PyConst {
    /// Python integer fitting in the Phase-A immediate representation.
    Int(i64),
    /// Python float; reserved for later phases but included in the frozen shape.
    Float(f64),
    /// Python Unicode string stored as UTF-8 Rust text.
    Str(String),
    /// Python `None` singleton.
    None,
}

/// Binary operations known to the IR.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinOp {
    /// Python `+`.
    Add,
    /// Python `-`, reserved for later phases.
    Sub,
    /// Python `*`, reserved for later phases.
    Mul,
}
