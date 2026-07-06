//! Dynamic-code hooks linked only into AoT executables that need eval/exec.
//!
//! The normal AoT runtime archive deliberately leaves `compile`, `eval`, `exec`,
//! and `ast.parse` unavailable. Builds whose reachability graph contains an
//! allowed dynamic-code sink link this crate's static archive instead; the
//! generated `main` trampoline calls [`pon_aot_install_dynamic_hooks`] before
//! entering `pon_runtime`'s process entrypoint.

#[path = "../../pon/src/astconv.rs"]
mod astconv;

use pon_jit::JitEngine;
use pon_runtime::{
	abi::pon_none,
	dynexec::{
		DynCodeMode, DynCompileRequest, DynExecuteRequest, set_ast_parse_hook,
		set_dynamic_code_hooks,
	},
	import::active_module_attr,
	intern::intern,
	object::PyObject,
};

/// Install the parser/JIT callbacks used by dynamic Python code in AoT images.
///
/// The function is idempotent: later calls replace the same global hook slots
/// with the same function pointers. It has a C ABI because the generated AoT
/// object calls it from its `main` trampoline before `pon_aot_entry` initializes
/// the runtime.
#[unsafe(no_mangle)]
pub extern "C" fn pon_aot_install_dynamic_hooks() {
	set_dynamic_code_hooks(validate_dynamic_source, execute_dynamic_source);
	set_ast_parse_hook(astconv::parse_dynamic_ast);
}

fn dynexec_source(source: &str, mode: DynCodeMode) -> String {
	match mode {
		DynCodeMode::Eval => format!("__pon_dyn_eval_result = ({source})\n"),
		DynCodeMode::Exec => source.to_owned(),
		DynCodeMode::Single => dynexec_single_source(source),
	}
}

fn dynexec_single_source(source: &str) -> String {
	let display_source = format!(
		concat!(
			"__pon_dyn_single_result = ({})\n",
			"if __pon_dyn_single_result is not None:\n",
			"    print(repr(__pon_dyn_single_result))\n",
		),
		source
	);
	if pon_ir::lower_source(&display_source).is_ok() {
		display_source
	} else {
		source.to_owned()
	}
}

fn validate_dynamic_source(request: DynCompileRequest<'_>) -> Result<(), String> {
	let source = dynexec_source(request.source, request.mode);
	pon_ir::lower_source(&source).map(|_| ()).map_err(|error| {
		format!("failed to parse/lower dynamic source '{}': {error}", request.filename)
	})
}

fn execute_dynamic_source(request: DynExecuteRequest<'_>) -> Result<*mut PyObject, String> {
	let source = dynexec_source(request.source, request.mode);
	let module = pon_ir::lower_source(&source).map_err(|error| {
		format!("failed to parse/lower dynamic source '{}': {error}", request.filename)
	})?;
	let mut engine = JitEngine::new();
	engine.run(&module).map_err(|error| {
		format!("failed to execute dynamic source '{}': {error}", request.filename)
	})?;
	std::mem::forget(engine);
	match request.mode {
		DynCodeMode::Eval => {
			let name = intern("__pon_dyn_eval_result");
			active_module_attr(name).ok_or_else(|| "dynamic eval did not produce a result".to_owned())
		},
		DynCodeMode::Exec | DynCodeMode::Single => {
			// SAFETY: `pon_none` returns the initialized singleton or NULL with an error.
			let none = unsafe { pon_none() };
			if none.is_null() {
				Err("failed to allocate None for dynamic exec result".to_owned())
			} else {
				Ok(none)
			}
		},
	}
}
