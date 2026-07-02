//! Phase-B control-flow IR shared by the frontend and code generators.
//!
//! The IR intentionally keeps Python values opaque: every [`Value`] denotes a
//! boxed Python object at runtime. Name-bearing operations use deterministic
//! `u32` ids into [`Module::names`] so this crate does not depend on runtime
//! interning state. Phase-B additions are data-only here; lowering and codegen
//! decide when each operation becomes executable.

use crate::types::Type;

/// A lowered Python module.
///
/// `functions[main.0]` is the synthetic `__main__` function. Every [`NameId`]
/// operand indexes [`Module::names`].
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

macro_rules! id_newtype {
    ($(#[$meta:meta])* $vis:vis struct $name:ident;) => {
        $(#[$meta])*
        #[repr(transparent)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        $vis struct $name(pub u32);

        impl From<u32> for $name {
            fn from(value: u32) -> Self {
                Self(value)
            }
        }

        impl From<$name> for u32 {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

id_newtype! {
    /// Index of a constant in a future module/function constant table.
    pub struct ConstId;
}

id_newtype! {
    /// Index of an interned Python identifier in [`Module::names`].
    pub struct NameId;
}

id_newtype! {
    /// Index of a function-local slot in [`Function::n_locals`].
    pub struct LocalId;
}

id_newtype! {
    /// Index of a closure cell captured or owned by a function.
    pub struct CellId;
}

/// Index of a lowered function; kept compatible with [`FunctionId`].
pub type FuncId = FunctionId;

id_newtype! {
    /// Index of an inline-cache feedback record reserved for later tiers.
    pub struct FeedbackSlot;
}

/// A lowered Python function.
///
/// Blocks are stored in layout order; `blocks[0]` is the entry block. Positional
/// parameters occupy the leading local slots; [`ParamLayout`] records the full
/// call-binding order for keyword-only, `*args`, and `**kwargs` slots.
#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    /// Debug/source name. The synthetic module body is named `__main__`.
    pub name: String,
    /// Positional argument count accepted by the Phase-A ABI.
    pub arity: usize,
    /// True when this function was produced from `async def` and calls must return a coroutine object.
    pub is_coroutine: bool,
    /// True when the function body is a generator-family resumable state machine
    /// (contains `yield`/`yield from`, or was `async def`).  Calls allocate a
    /// frame and return a generator/coroutine object without running the body.
    pub is_generator: bool,
    /// Full formal-parameter layout used by Phase-B function binding.
    pub params: ParamLayout,
    /// Function control-flow blocks.
    pub blocks: Vec<Block>,
    /// Number of local slots needed by this function.
    pub n_locals: usize,
}

/// Full formal-parameter layout for a lowered function.
///
/// `names` contains positional and keyword-only parameter names in runtime
/// binding order, excluding `*args` and `**kwargs`. Variadic names are kept
/// separately because the runtime ABI stores them outside the named-parameter
/// array.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParamLayout {
    /// Positional and keyword-only parameter names, excluding variadics.
    pub names: Vec<String>,
    /// Leading positional-only count.
    pub positional_only_count: usize,
    /// Positional-or-keyword count after positional-only parameters.
    pub positional_count: usize,
    /// Keyword-only parameter count after positional parameters.
    pub keyword_only_count: usize,
    /// `*args` parameter name when present.
    pub vararg_name: Option<String>,
    /// `**kwargs` parameter name when present.
    pub kwarg_name: Option<String>,
}

impl ParamLayout {
    /// Total number of argv slots produced by runtime argument binding.
    pub fn total_slot_count(&self) -> usize {
        self.names.len() + usize::from(self.vararg_name.is_some()) + usize::from(self.kwarg_name.is_some())
    }

    /// Phase-A-compatible positional arity.
    pub fn positional_arity(&self) -> usize {
        self.positional_only_count + self.positional_count
    }
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

/// Numeric identity of a boxed SSA value; kept compatible with [`Value`].
pub type ValueId = Value;

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
    /// Optional inline-cache feedback record used by later typed tiers.
    pub feedback_slot: Option<FeedbackSlot>,
    /// Best inferred/speculative type for this instruction's SSA result.
    pub inferred_type: Type,
    /// Sound static upper bound for this instruction's SSA result.
    pub static_type: Type,
}

impl Inst {
    /// Build a tier-0 boxed instruction with inert typed-tier metadata.
    ///
    /// The strict-prefix Phase D metadata must not change baseline lowering or
    /// codegen behavior, so fresh instructions start with no feedback slot, an
    /// unobserved inferred type, and boxed-object static type.
    #[must_use]
    pub fn new(result: Value, kind: InstKind) -> Self {
        Self {
            result,
            kind,
            feedback_slot: None,
            inferred_type: Type::Bottom,
            static_type: Type::Object,
        }
    }

    /// Attach a feedback slot to an operation site.
    #[must_use]
    pub fn with_feedback_slot(mut self, feedback_slot: FeedbackSlot) -> Self {
        self.feedback_slot = Some(feedback_slot);
        self
    }

    /// Attach an inferred/speculative SSA-result type.
    #[must_use]
    pub fn with_inferred_type(mut self, inferred_type: Type) -> Self {
        self.inferred_type = inferred_type;
        self
    }

    /// Attach a sound static SSA-result type.
    #[must_use]
    pub fn with_static_type(mut self, static_type: Type) -> Self {
        self.static_type = static_type;
        self
    }
}

/// Phase-B instruction set.
///
/// This enum is non-exhaustive so future Python coverage can add operations
/// without forcing downstream crates to assume Phase B is complete. Existing
/// Phase-A variant names and constructor shapes remain source-compatible.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum InstKind {
    /// Materialize an inline Python constant used by Phase-A lowering.
    Const(PyConst),
    /// Materialize a constant-table entry.
    ConstRef(ConstId),
    /// Build a Python tuple from element values.
    BuildTuple { elts: Vec<ValueId> },
    /// Build a Python list from element values.
    BuildList { elts: Vec<ValueId> },
    /// Build a Python set from element values.
    BuildSet { elts: Vec<ValueId> },
    /// Build a Python dict from key/value pairs.
    BuildMap { pairs: Vec<(ValueId, ValueId)> },
    /// Build a Python slice object from lower, upper, and step values.
    BuildSlice {
        /// Lower bound value, usually `None` when omitted.
        lower: ValueId,
        /// Upper bound value, usually `None` when omitted.
        upper: ValueId,
        /// Step value, usually `None` when omitted.
        step: ValueId,
    },
    /// Concatenate formatted-string parts into a Python string.
    BuildString { parts: Vec<FStrPart> },
    /// Build a Python 3.14 template-string object from parsed parts.
    BuildTemplate { parts: Vec<TStrPart> },
    /// Append an item to a list during comprehension or unpack lowering.
    ListAppend { list: ValueId, item: ValueId },
    /// Add an item to a set during comprehension or unpack lowering.
    SetAdd { set: ValueId, item: ValueId },
    /// Insert a key/value pair into a dict under Python overwrite rules.
    MapInsert {
        /// Dict object being mutated.
        map: ValueId,
        /// Key object to insert.
        key: ValueId,
        /// Value object to insert.
        val: ValueId,
    },
    /// Extend a list with values produced by an iterable.
    ListExtend { list: ValueId, iter: ValueId },
    /// Convert a staging list into a tuple for `*` tuple-display unpacking.
    ListToTuple { list: ValueId },
    /// Update a set with values produced by an iterable for `*` set-display unpacking.
    SetUpdate { set: ValueId, iter: ValueId },
    /// Merge another mapping into a dict for `**` display unpacking.
    DictMerge { map: ValueId, other: ValueId },
    /// Merge another mapping into a call-kwargs dict, rejecting duplicate keys.
    DictMergeUnique { map: ValueId, other: ValueId },
    /// Load a function-local slot.
    LoadLocal(LocalId),
    /// Store a boxed value into a function-local slot.
    StoreLocal(LocalId, ValueId),
    /// Delete a function-local binding.
    DeleteLocal(LocalId),
    /// Load a global by deterministic interned name id.
    LoadGlobal(NameId),
    /// Store a boxed value into a global by deterministic interned name id.
    StoreGlobal(NameId, ValueId),
    /// Delete a global binding by deterministic interned name id.
    DeleteGlobal(NameId),
    /// Load a Python name from the active namespace chain.
    LoadName(NameId),
    /// Store a Python name in the active namespace.
    StoreName(NameId, ValueId),
    /// Delete a Python name from the active namespace.
    DeleteName(NameId),
    /// Load a closure cell's current boxed value.
    LoadCell(CellId),
    /// Store a boxed value into a closure cell.
    StoreCell(CellId, ValueId),
    /// Delete the value currently held by a closure cell.
    DeleteCell(CellId),
    /// Convert a local slot into a closure cell.
    MakeCell(LocalId),
    /// Load the cell object used to close over a free variable.
    LoadClosure(CellId),
    /// Load a builtin by deterministic interned name id.
    LoadBuiltin(NameId),
    /// Apply a binary Python operation.
    BinaryOp {
        /// Python binary operator to apply.
        op: BinOp,
        /// Left-hand operand.
        lhs: ValueId,
        /// Right-hand operand.
        rhs: ValueId,
    },
    /// Apply an in-place Python operation for augmented assignment.
    InplaceOp {
        /// Python operator to apply in-place.
        op: BinOp,
        /// Left-hand target operand.
        lhs: ValueId,
        /// Right-hand operand.
        rhs: ValueId,
    },
    /// Apply a unary Python operation other than logical `not`.
    UnaryOp {
        /// Unary operator to apply.
        op: UnOp,
        /// Operand to transform.
        operand: ValueId,
    },
    /// Apply one rich comparison operation.
    Compare {
        /// Comparison operator to apply.
        op: CmpOp,
        /// Left-hand operand.
        lhs: ValueId,
        /// Right-hand operand.
        rhs: ValueId,
    },
    /// Evaluate `in` or `not in`.
    Contains {
        /// Candidate item.
        item: ValueId,
        /// Container or iterable to search.
        container: ValueId,
        /// Whether this represents `not in`.
        negate: bool,
    },
    /// Evaluate identity with `is` or `is not`.
    Is {
        /// Left-hand object.
        lhs: ValueId,
        /// Right-hand object.
        rhs: ValueId,
        /// Whether this represents `is not`.
        negate: bool,
    },
    /// Convert a Python object to a boxed truth-test result.
    BoolTest { val: ValueId },
    /// Apply Python logical `not`.
    Not { val: ValueId },
    /// Load an attribute by interned name.
    LoadAttr { obj: ValueId, name: NameId },
    /// Store an attribute by interned name.
    StoreAttr {
        /// Object whose attribute is assigned.
        obj: ValueId,
        /// Interned attribute name.
        name: NameId,
        /// Boxed value to store.
        val: ValueId,
    },
    /// Delete an attribute by interned name.
    DeleteAttr { obj: ValueId, name: NameId },
    /// Load a method and receiver pair for call specialization.
    LoadMethod { obj: ValueId, name: NameId },
    /// Load `obj[index]`.
    SubscriptGet { obj: ValueId, index: ValueId },
    /// Store `obj[index] = val`.
    SubscriptSet {
        /// Subscriptable object to mutate.
        obj: ValueId,
        /// Subscript index or key.
        index: ValueId,
        /// Boxed value to store.
        val: ValueId,
    },
    /// Delete `obj[index]`.
    SubscriptDel { obj: ValueId, index: ValueId },
    /// Call a Python callable with positional arguments.
    Call { callee: ValueId, args: Vec<ValueId> },
    /// Call a Python callable with star-args, keywords, and double-star kwargs.
    CallEx {
        /// Callable object.
        callee: ValueId,
        /// Positional arguments already evaluated left-to-right.
        args: Vec<ValueId>,
        /// Optional `*args` iterable.
        star: Option<ValueId>,
        /// Keyword arguments by interned name.
        kwargs: Vec<(NameId, ValueId)>,
        /// Optional `**kwargs` mapping.
        dstar: Option<ValueId>,
    },
    /// Call a method pair produced by [`InstKind::LoadMethod`].
    CallMethod { recv_pair: ValueId, args: Vec<ValueId> },
    /// Get the synchronous iterator for a value.
    GetIter { iterable: ValueId },
    /// Get the asynchronous iterator for a value.
    GetAIter { iterable: ValueId },
    /// Advance an iterator and yield the next item or StopIteration state.
    ForNext { iter: ValueId },
    /// Unpack exactly `n` sequence values.
    UnpackSeq { val: ValueId, n: usize },
    /// Unpack a sequence with one starred target.
    UnpackEx {
        /// Sequence value to unpack.
        val: ValueId,
        /// Number of required targets before the star.
        before: usize,
        /// Number of required targets after the star.
        after: usize,
    },
    /// Produce a generator yield value before suspension.
    ///
    /// Only exists between AST lowering and the generator state-machine
    /// transform; the transform replaces every occurrence with a
    /// [`Terminator::Suspend`] split.  Codegen rejects it.
    Yield { val: ValueId },
    /// Delegate generator control to another iterator.
    ///
    /// Like [`InstKind::Yield`], this is transform input only: the state-machine
    /// transform expands it into a delegation loop around
    /// [`InstKind::GenDelegateStep`].  Codegen rejects it.
    YieldFrom { iter: ValueId },
    /// Await an awaitable via its `__await__` iterator.
    Await { awaitable: ValueId },
    /// Consume the resume payload of the enclosing generator frame.
    ///
    /// Emitted only by the generator transform at resume points: re-raises a
    /// pending `throw` payload (NULL-routing to the active handler) or produces
    /// the sent value (`None` when absent) as the yield-expression result.
    GenResumePayload,
    /// Forward the frame's resume payload to a `yield from` delegate once.
    ///
    /// Returns the next yielded value, or NULL with `StopIteration` pending when
    /// the delegation finished (the finished value is stashed for
    /// [`InstKind::GenLastStopValue`]); other exceptions propagate.
    GenDelegateStep { delegate: ValueId },
    /// Produce the stashed `StopIteration.value` of the last finished delegation.
    GenLastStopValue,
    /// Raise an exception, optionally with an explicit cause.
    Raise {
        /// Exception instance or type; `None` means bare `raise`.
        exc: Option<ValueId>,
        /// Optional `from` cause.
        cause: Option<ValueId>,
    },
    /// Re-raise the active exception.
    Reraise,
    /// Push the current exception state for `except`/`finally` handling.
    PushExcInfo {
        /// Handler block used by the boxed codegen NULL-sentinel edge while this handler is active.
        target: BlockId,
        /// Operand-stack depth to restore before entering the handler.
        stack_depth: u32,
        /// Handler kind tag owned by the lowering/runtime exception workstream.
        kind: u8,
    },
    /// Pop the current exception state after handler cleanup.
    PopExcInfo,
    /// Test whether the active exception matches an exception type.
    MatchExc { exc_type: ValueId },
    /// Legacy representative `except*` split; retained until helper-table consumers migrate.
    CheckExcStar { exc_types: ValueId },
    /// Enter an `except*` dispatcher for the pending exception.
    ExcStarEnter,
    /// Split the active `except*` remainder against one clause type expression.
    ExcStarMatch { exc_types: ValueId },
    /// Mark the current `except*` clause body as completed without raising.
    ExcStarBodyOk,
    /// Mark the current `except*` clause body as having raised.
    ExcStarBodyRaised,
    /// Finish an `except*` dispatcher, installing any remainder/raised group.
    ExcStarFinish,
    /// Load the current active exception object.
    GetCurrentExc,
    /// Build an exception group from exception values.
    BuildExcGroup { excs: Vec<ValueId> },
    /// Test whether a subject is a sequence pattern candidate.
    MatchSequence { subj: ValueId },
    /// Test whether a subject is a mapping pattern candidate.
    MatchMapping { subj: ValueId },
    /// Match a class pattern against positional and keyword pattern names.
    MatchClass {
        /// Subject being matched.
        subj: ValueId,
        /// Class object to match.
        cls: ValueId,
        /// Number of positional subpatterns.
        nargs: usize,
        /// Keyword subpattern names.
        kw: Vec<NameId>,
    },
    /// Extract values for mapping pattern keys.
    MatchKeys { subj: ValueId, keys: Vec<ValueId> },
    /// Load the length of a pattern-match subject.
    GetLen { subj: ValueId },
    /// Test a subject length against a pattern threshold.
    MatchLenGe {
        /// Subject whose length is tested.
        subj: ValueId,
        /// Required length.
        n: usize,
        /// Whether the length must be exactly `n`.
        exact: bool,
    },
    /// Import a module by interned dotted name.
    ImportName {
        /// Module name.
        name: NameId,
        /// Names requested by `from ... import ...`.
        fromlist: Vec<NameId>,
        /// Relative import level.
        level: u32,
    },
    /// Import an attribute from an imported module.
    ImportFrom { module: ValueId, name: NameId },
    /// Import all public names from a module into the active namespace.
    ImportStar { module: ValueId },
    /// Build a Python class object.
    BuildClass {
        /// Function containing the class body.
        body: FuncId,
        /// Interned class name.
        name: NameId,
        /// Evaluated base classes.
        bases: Vec<ValueId>,
        /// Class keyword arguments.
        keywords: Vec<(NameId, ValueId)>,
        /// Decorators applied after class creation.
        decorators: Vec<ValueId>,
        /// Enclosing-scope cells captured by the class body (free variables
        /// of the class scope, in `ScopeInfo::free_vars` order).  The runtime
        /// attaches them as the body function's closure so nested methods and
        /// class-level reads reach the enclosing function's cells.
        closure: Vec<CellId>,
    },
    /// Box a lowered function object using the Phase-A constructor shape.
    MakeFunction {
        /// Index of the lowered function in [`Module::functions`].
        func_index: u32,
        /// Deterministic interned id of the function's source name.
        name_interned: NameId,
        /// Positional argument count.
        arity: usize,
    },
    /// Box a lowered function object with Phase-B defaults and closure data.
    MakeFunctionFull {
        /// Lowered function body.
        code: FuncId,
        /// Positional default values.
        defaults: Vec<ValueId>,
        /// Keyword-only default values.
        kwdefaults: Vec<(NameId, ValueId)>,
        /// Closure cells captured by the function.
        closure: Vec<CellId>,
        /// Evaluated annotations by interned name.
        ///
        /// PEP 649 cutover: lowering no longer eagerly evaluates annotation
        /// expressions, so new IR always carries an empty vector here; the
        /// field is retained only for ABI stability of the frozen
        /// `pon_make_function_full` helper row.
        annotations: Vec<(NameId, ValueId)>,
    },
    /// Attach a synthesized PEP 649 `__annotate__` function to a function object.
    FunctionSetAnnotate {
        /// Target function object.
        function: ValueId,
        /// Synthesized `__annotate__(format)` function object.
        annotate: ValueId,
    },
    /// Build a PEP 695 `TypeAliasType` from a lazy value thunk.
    MakeTypeAlias {
        /// Interned alias name (`X` in `type X = ...`).
        name: NameId,
        /// Zero-argument thunk that evaluates the alias value on demand.
        thunk: ValueId,
    },
    /// Build a minimal PEP 695 `TypeVar` runtime object by interned name.
    MakeTypeVar {
        /// Interned type-parameter name (`T` in `def f[T](...)`).
        name: NameId,
    },
    /// Ensure `__annotations__` exists in the active namespace.
    ///
    /// Legacy pre-PEP-649 eager path; no longer emitted by lowering.
    SetupAnnotations,
    /// Load Python's `__build_class__` helper.
    LoadBuildClass,
}

/// Phase-B operation payload alias used by planning documents.
pub type Op = InstKind;

/// Basic-block terminator.
///
/// Phase A emits only [`Terminator::Return`] for supported source, but `Jump`,
/// `Branch`, and `Unreachable` reserve the CFG shape used by later phases and by
/// internal placeholder blocks before lowering is finalized.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum Terminator {
    /// Return a boxed Python value.
    Return(ValueId),
    /// Unconditional jump.
    Jump(BlockId),
    /// Conditional branch using the Phase-A field names.
    Branch {
        /// Truth-tested condition value.
        cond: ValueId,
        /// Destination when `cond` is true.
        then_blk: BlockId,
        /// Destination when `cond` is false.
        else_blk: BlockId,
    },
    /// Conditional branch using the frozen Phase-B field names.
    CondBranch {
        /// Boxed truth-test result or comparison result.
        cond: ValueId,
        /// Destination when `cond` is true.
        then_: BlockId,
        /// Destination when `cond` is false.
        else_: BlockId,
    },
    /// Iterator loop branch driven by a preceding [`InstKind::ForNext`].
    ForLoop {
        /// Iterator being advanced.
        iter: ValueId,
        /// Destination when an item is available.
        body: BlockId,
        /// Destination when iteration is exhausted.
        done: BlockId,
    },
    /// Suspend a generator or coroutine and record its resume state.
    Suspend {
        /// Generator state number to persist.
        state: u32,
        /// Value yielded to the caller.
        val: ValueId,
        /// Block to enter when resumed.
        resume: BlockId,
    },
    /// Transfer to the active exception handler after an error was raised.
    RaiseTerm,
    /// No valid control-flow continuation.
    Unreachable,
}

/// Python constants representable directly in IR.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum PyConst {
    /// Python integer fitting in the current immediate representation.
    Int(i64),
    /// Python float stored as an IEEE-754 double.
    Float(f64),
    /// Python complex stored as two IEEE-754 doubles.
    Complex { real: f64, imag: f64 },
    /// Python Unicode string stored as UTF-8 Rust text.
    Str(String),
    /// Python bytes object stored as raw bytes.
    Bytes(Vec<u8>),
    /// Python boolean singleton.
    Bool(bool),
    /// Python `None` singleton.
    None,
    /// Python `Ellipsis` singleton.
    Ellipsis,
    /// Python `NotImplemented` singleton.
    NotImplemented,
}

/// Binary operations known to the IR.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinOp {
    /// Python `+`.
    Add,
    /// Python `-`.
    Sub,
    /// Python `*`.
    Mul,
    /// Python matrix multiply `@`.
    MatMul,
    /// Python true division `/`.
    Div,
    /// Python floor division `//`.
    FloorDiv,
    /// Python modulo `%`.
    Mod,
    /// Python exponentiation `**`.
    Pow,
    /// Python left shift `<<`.
    LShift,
    /// Python right shift `>>`.
    RShift,
    /// Python bitwise `&`.
    And,
    /// Python bitwise `|`.
    Or,
    /// Python bitwise `^`.
    Xor,
}

/// Unary operations other than logical `not`.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnOp {
    /// Python unary negation `-x`.
    Neg,
    /// Python unary plus `+x`.
    Pos,
    /// Python bitwise inversion `~x`.
    Invert,
}

/// Rich comparison operations; identity and containment are separate IR ops.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CmpOp {
    /// Python equality `==`.
    Eq,
    /// Python inequality `!=`.
    Ne,
    /// Python less-than `<`.
    Lt,
    /// Python less-than-or-equal `<=`.
    Le,
    /// Python greater-than `>`.
    Gt,
    /// Python greater-than-or-equal `>=`.
    Ge,
}

/// One part of a lowered Python f-string.
#[derive(Clone, Debug, PartialEq)]
pub enum FStrPart {
    /// Literal f-string text stored in the constant table.
    Literal(ConstId),
    /// Interpolated f-string expression with conversion and optional format.
    Interp {
        /// Value to interpolate.
        value: ValueId,
        /// Conversion byte such as `b's'`, `b'r'`, or zero for no conversion.
        conversion: u8,
        /// Optional pre-lowered format-spec value.
        format_spec: Option<ValueId>,
    },
}

/// One part of a lowered Python 3.14 template string.
#[derive(Clone, Debug, PartialEq)]
pub enum TStrPart {
    /// Literal template text stored in the constant table.
    Literal(ConstId),
    /// Interpolated template expression with conversion and optional format.
    Interp {
        /// Value to interpolate.
        value: ValueId,
        /// Source spelling of the interpolation expression for PEP 750 metadata.
        expression: String,
        /// Conversion byte such as `b's'`, `b'r'`, or zero for no conversion.
        conversion: u8,
        /// Optional pre-lowered format-spec value.
        format_spec: Option<ValueId>,
    },
}
