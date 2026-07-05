//! Ruff parser entry point for pon IR lowering.
//!
//! This module is the only place in `pon-ir` that invokes Ruff's parser. It
//! pins parsing to Python 3.14 so the frontend never accidentally falls back to
//! Ruff's older default target version.

use ruff_python_ast::{Mod, ModModule, PythonVersion};
use ruff_python_parser::{Mode, ParseOptions, parse};

use crate::LowerError;

/// Parse Python module source with the project-pinned Ruff options.
///
/// The implementation deliberately uses [`ruff_python_parser::parse`] with
/// [`PythonVersion::PY314`] rather than Ruff's module convenience entry, whose
/// default target version is not the project contract.
pub fn parse_module_source(source: &str) -> Result<ModModule, LowerError> {
	let options = ParseOptions::from(Mode::Module).with_target_version(PythonVersion::PY314);
	let parsed = parse(source, options).map_err(|err| LowerError::parse(err.to_string()))?;

	match parsed.into_syntax() {
		Mod::Module(module) => Ok(module),
		Mod::Expression(_) => Err(LowerError::unsupported("expression parser mode result")),
	}
}
