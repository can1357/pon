//! Desugaring pass slot for the frontend pipeline.
//!
//! Phase A lowers the accepted Python subset directly, so there are no
//! source-to-IR rewrites to perform yet. Keeping this explicit pass in the
//! pipeline gives later phases a stable home for transformations that should
//! run after AST lowering and before code generation.

use crate::Module;

/// Return `module` unchanged.
///
/// Later phases will add real desugarings here; Phase A intentionally documents
/// and preserves the identity transform instead of hiding an implicit no-op in
/// the lowerer.
#[must_use]
pub fn desugar_module(module: Module) -> Module {
	module
}
