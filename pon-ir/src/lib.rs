#![doc = "Phase-A Python AST lowering into pon IR."]

pub mod desugar;
pub mod ir;
pub mod lower;
pub mod parse;

pub use desugar::desugar_module;
pub use ir::{
    BinOp, Block, BlockId, Function, FunctionId, Inst, InstId, InstKind, Module, PyConst,
    Terminator, Value,
};
pub use lower::{LowerError, lower_module, lower_source};
pub use parse::parse_module_source;
