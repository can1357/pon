//! Function-inspection seam backing the native `pon` debug module.
//!
//! `pon-runtime` deliberately does not depend on `pon-ir` or `pon-jit`, so
//! rendering a function's compiler views is delegated to a host hook installed
//! by the JIT frontend (the same seam pattern as [`crate::dynexec`]).
//! Ahead-of-time products install no hook and report inspection as
//! unavailable.

use std::sync::Mutex;

/// Compiler view requested through [`InspectHook`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectView {
	/// Lowered pon IR exactly as registered for tier-0 compilation.
	Ir,
	/// Baseline Cranelift IR (CLIF) produced by the tier-0 lowering.
	Clif,
	/// Baseline native code rendered through Cranelift's disassembly.
	Asm,
}

/// Why [`InspectHook`] could not render a view.
#[derive(Debug)]
pub enum InspectError {
	/// The entrypoint is not a registered JIT-compiled Python function.
	UnknownFunction,
	/// Rendering failed; the message surfaces as a Python `RuntimeError`.
	Render(String),
}

/// Host hook rendering a compiler view for the tier-0 entrypoint address of a
/// JIT-compiled Python function. Installed once per process by the JIT
/// frontend; never uninstalled.
pub type InspectHook =
	fn(tier0_entry: *const u8, view: InspectView) -> Result<String, InspectError>;

static INSPECT_HOOK: Mutex<Option<InspectHook>> = Mutex::new(None);

/// Install the host callback backing `pon.ir`/`pon.clif`/`pon.asm`.
pub fn set_inspect_hook(hook: InspectHook) {
	*INSPECT_HOOK
		.lock()
		.unwrap_or_else(|poison| poison.into_inner()) = Some(hook);
}

/// Render `view` for a tier-0 entrypoint, or `None` when no hook is installed.
pub(crate) fn inspect(
	tier0_entry: *const u8,
	view: InspectView,
) -> Option<Result<String, InspectError>> {
	let hook = *INSPECT_HOOK
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	hook.map(|hook| hook(tier0_entry, view))
}
