//! Backend for the runtime's native `pon` debug module.
//!
//! Every tier-0 module compile registers its finalized entrypoints here (see
//! [`crate::tierup::TierUpDriver::register_module`]); `pon.ir`/`pon.clif`/
//! `pon.asm` resolve a function's tier-0 entry address through the
//! [`pon_runtime::inspect`] hook into this process-wide registry. The registry
//! is global rather than per-driver on purpose: the frontend creates one
//! `JitEngine` per source module, so the active tier-up driver only knows the
//! most recently compiled module.

use std::{
	collections::HashMap,
	sync::{Arc, LazyLock, Mutex},
};

use cranelift_codegen::control::ControlPlane;
use cranelift_frontend::FunctionBuilderContext;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Module as ClifModule, default_libcall_names};
use pon_codegen::{
	baseline::{NameMap, compile_function as compile_baseline_function, entry_arg_counts},
	helpers::declare_helpers,
	isa::{OptLevel, make_isa},
};
use pon_ir::ir::Module as IrModule;
use pon_runtime::inspect::{InspectError, InspectView, set_inspect_hook};

/// Registered IR snapshot and the function's index within it.
type RegisteredIr = (Arc<IrModule>, usize);

/// Tier-0 entry address -> registered IR function.
static REGISTRY: LazyLock<Mutex<HashMap<usize, RegisteredIr>>> =
	LazyLock::new(|| Mutex::new(HashMap::new()));

/// Record finalized tier-0 entrypoints of a just-compiled module and keep the
/// runtime inspection hook installed (idempotent).
pub fn register_functions(
	ir: &Arc<IrModule>,
	entries: impl IntoIterator<Item = (usize, *const u8)>,
) {
	set_inspect_hook(inspect);
	let mut registry = REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
	for (function_index, entry) in entries {
		registry.insert(entry as usize, (Arc::clone(ir), function_index));
	}
}

/// [`pon_runtime::inspect::InspectHook`] implementation.
fn inspect(tier0_entry: *const u8, view: InspectView) -> Result<String, InspectError> {
	let (ir, index) = {
		let registry = REGISTRY.lock().unwrap_or_else(|poison| poison.into_inner());
		registry
			.get(&(tier0_entry as usize))
			.map(|(ir, index)| (Arc::clone(ir), *index))
	}
	.ok_or(InspectError::UnknownFunction)?;
	match view {
		InspectView::Ir => pon_ir::print::function_text(&ir, index)
			.ok_or_else(|| render_error("registered function index out of range")),
		InspectView::Clif => render_lowered(&ir, index, false),
		InspectView::Asm => render_lowered(&ir, index, true),
	}
}

fn render_error(message: impl ToString) -> InspectError {
	InspectError::Render(message.to_string())
}

/// Lower one registered function through the baseline (tier-0) pipeline and
/// render its CLIF or its final native code via Cranelift's disassembly.
///
/// The scratch `JITModule` is declaration-only: nothing is defined or
/// finalized, so no helper symbols are registered and no executable memory
/// escapes; `free_memory` reclaims it on every path.
fn render_lowered(
	ir_module: &IrModule,
	function_index: usize,
	want_asm: bool,
) -> Result<String, InspectError> {
	let isa = make_isa(OptLevel::None, false);
	let mut module = JITModule::new(JITBuilder::with_isa(isa, default_libcall_names()));
	let result = lower_into(&mut module, ir_module, function_index, want_asm);
	// SAFETY: no function was defined or finalized in this scratch module, so
	// no code pointers exist that freeing could invalidate.
	unsafe { module.free_memory() };
	result
}

fn lower_into(
	module: &mut JITModule,
	ir_module: &IrModule,
	function_index: usize,
	want_asm: bool,
) -> Result<String, InspectError> {
	let function = ir_module
		.functions
		.get(function_index)
		.ok_or_else(|| render_error("registered function index out of range"))?;
	let helpers = declare_helpers(module).map_err(render_error)?;
	let func_ids =
		crate::tierup::declare_tier1_functions(module, ir_module).map_err(render_error)?;
	let names = NameMap::from_ir_module(ir_module);
	let counts = entry_arg_counts(ir_module);
	let mut ctx = module.make_context();
	let mut fctx = FunctionBuilderContext::new();
	compile_baseline_function(
		module,
		&helpers,
		&func_ids,
		&ir_module.functions,
		&names,
		function,
		counts[function_index],
		&mut ctx,
		&mut fctx,
		// Match the executing tier-0 lowering so `pon.clif`/`pon.asm`
		// render the code that actually runs (incl. safepoint spills).
		true,
	)
	.map_err(render_error)?;
	if !want_asm {
		return Ok(ctx.func.display().to_string());
	}
	// `compile_function` cleared the context before building, so the disasm
	// request must be re-armed here, right before compilation.
	ctx.set_disasm(true);
	let compiled = ctx
		.compile(module.isa(), &mut ControlPlane::default())
		.map_err(|error| render_error(&error.inner))?;
	compiled
		.vcode
		.clone()
		.ok_or_else(|| render_error("cranelift produced no disassembly"))
}
