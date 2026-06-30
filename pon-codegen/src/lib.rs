#![doc = "Phase-A Cranelift codegen support for Pon."]
#![doc = "This crate publishes shared ISA configuration, runtime helper import"]
#![doc = "declaration, and module-agnostic baseline IR lowering."]

/// Baseline Cranelift lowering for Phase-A boxed Python IR.
pub mod baseline;
/// Runtime helper import declaration for Cranelift modules.
pub mod helpers;
/// Shared Cranelift ISA and flag construction helpers.
pub mod isa;
