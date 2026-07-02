//! Isolated Rust implementation of CPython's `_sre` bytecode VM.
//!
//! This module is intentionally not registered in the native-module table during
//! the Wave-2 pre-gate quarantine.  The vendored `re._compiler` bytecode shape is
//! still exercised directly by the `sre_` cargo fixtures.

mod vm;

pub use vm::{
    compile, compile_checked, getcodesize, CaseMode, Error, Match, MatchedValue, Pattern,
    PatternText, MAXREPEAT, CODESIZE, MAGIC,
};
