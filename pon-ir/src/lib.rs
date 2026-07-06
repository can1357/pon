//!Phase-B Python AST lowering and frozen IR surface.

pub mod desugar;
pub mod ir;
pub mod lower;
pub mod parse;
pub mod print;
pub mod types;

pub use desugar::desugar_module;
pub use ir::{
	BinOp, Block, BlockId, CellId, CmpOp, ConstId, FStrPart, FeedbackSlot, FuncId, Function,
	FunctionId, Inst, InstId, InstKind, LocalId, Module, NameId, Op, PyConst, TStrPart, Terminator,
	UnOp, Value, ValueId,
};
pub use lower::{LowerError, lower_module, lower_source};
pub use parse::parse_module_source;
pub use types::Type;
