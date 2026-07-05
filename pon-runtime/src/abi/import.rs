//! Import helper family namespace.
//!
//! Import-name, import-from, and import-star helpers will live here once module
//! objects and import state are implemented.  B0 only freezes the
//! collision-free module boundary.

/// Interned module or imported-name id carried through the helper ABI.
pub type ImportName = u32;
