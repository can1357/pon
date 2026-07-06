//!Phase-A Cranelift JIT driver.

use std::{error::Error, fmt, mem, ptr};

use cranelift_codegen::{Context, ir::AbiParam};
use cranelift_frontend::FunctionBuilderContext;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module as ClifModule, ModuleError, default_libcall_names};
use pon_codegen::{
	baseline::{CodegenError, NameMap, compile_function},
	helpers::declare_helpers,
	isa::{OptLevel, make_isa},
};
use pon_ir::{ir::Module as IrModule, lower_source};
use pon_runtime::{
	abi::{HELPERS, pon_runtime_init},
	object::PyObject,
	thread_state::pon_err_message,
};

mod inspect;
pub mod tierup;

/// Phase-A compiled Python function ABI.
pub type MainFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

/// JIT driver that owns the executable Cranelift module and reusable lowering
/// scratch state.
///
/// The engine must stay alive while compiled code runs: Phase-A string
/// constants can be emitted as JIT module data and borrowed by runtime objects.
pub struct JitEngine {
	module: JITModule,
	ctx:    Context,
	fctx:   FunctionBuilderContext,
	tierup: Box<tierup::TierUpDriver>,
}

/// Opaque dynamic-execution handle for the runtime eval/exec seam.
///
/// The handle owns its JIT engine, so compiled code and tier-up state stay
/// alive for every later [`execute`] call. Runtime globals/locals rebinding is
/// owned by `pon-runtime`; this seam only compiles source and re-enters the
/// compiled body.
pub struct DynExecHandle {
	engine:   JitEngine,
	main:     MainFn,
	filename: String,
	mode:     String,
}

impl DynExecHandle {
	#[must_use]
	pub fn filename(&self) -> &str {
		&self.filename
	}

	#[must_use]
	pub fn mode(&self) -> &str {
		&self.mode
	}
}

/// Error reported while compiling or running Phase-A JIT code.
#[derive(Debug)]
pub enum JitError {
	/// Cranelift module declaration, definition, or finalization failed.
	Module(ModuleError),
	/// Baseline IR-to-Cranelift lowering failed.
	Codegen(CodegenError),
	/// Source parsing/lowering failed before JIT codegen started.
	Lower(String),
	/// The IR module did not contain its declared `__main__` function.
	MissingMain { main: u32 },
	/// Runtime initialization failed before user code executed.
	RuntimeInit(String),
	/// Compiled user code reported an exception through the NULL sentinel.
	Runtime(String),
}

impl fmt::Display for JitError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Module(error) => write!(f, "JIT module error: {error}"),
			Self::Codegen(error) => write!(f, "JIT codegen error: {error}"),
			Self::Lower(message) => write!(f, "source lowering error: {message}"),
			Self::MissingMain { main } => write!(f, "IR main function index {main} is out of range"),
			Self::RuntimeInit(message) => write!(f, "runtime initialization failed: {message}"),
			Self::Runtime(message) => write!(f, "runtime error: {message}"),
		}
	}
}

impl Error for JitError {}

impl From<ModuleError> for JitError {
	fn from(error: ModuleError) -> Self {
		Self::Module(error)
	}
}

impl From<CodegenError> for JitError {
	fn from(error: CodegenError) -> Self {
		Self::Codegen(error)
	}
}

impl JitEngine {
	/// Build a Phase-A JIT with the shared no-optimization ISA and all runtime
	/// helper symbols registered before the executable module is created.
	#[must_use]
	pub fn new() -> Self {
		let isa = make_isa(OptLevel::None, false);
		let mut builder = JITBuilder::with_isa(isa, default_libcall_names());
		for helper in HELPERS {
			builder.symbol(helper.symbol, helper.address.cast::<u8>());
		}
		builder.symbol(
			pon_runtime::abi::CURRENT_LINE_SYMBOL,
			pon_runtime::abi::current_line_cell_address(),
		);
		register_free_threading_symbols(&mut builder);
		let module = JITModule::new(builder);
		let ctx = module.make_context();
		let fctx = FunctionBuilderContext::new();
		let tierup = Box::new(tierup::TierUpDriver::new());
		let mut engine = Self { module, ctx, fctx, tierup };
		tierup::register_runtime_hook(engine.tierup.as_mut());
		engine
	}

	/// Compile every lowered IR function and return the finalized `__main__`
	/// entrypoint.
	///
	/// Runtime name ids are created with [`NameMap::from_ir_module`], never by
	/// passing source-local IR name ids directly to runtime helpers.
	pub fn compile(&mut self, ir_module: &IrModule) -> Result<MainFn, JitError> {
		let helpers = declare_helpers(&mut self.module)?;
		let func_ids = self.declare_ir_functions(ir_module)?;
		let names = NameMap::from_ir_module(ir_module);
		let entry_arg_counts = pon_codegen::baseline::entry_arg_counts(ir_module);

		for (index, function) in ir_module.functions.iter().enumerate() {
			compile_function(
				&mut self.module,
				&helpers,
				&func_ids,
				&ir_module.functions,
				&names,
				function,
				entry_arg_counts[index],
				&mut self.ctx,
				&mut self.fctx,
			)?;
			self
				.module
				.define_function(func_ids[index], &mut self.ctx)?;
		}

		self.module.finalize_definitions()?;

		self
			.tierup
			.register_module(ir_module, &func_ids, &self.module);

		let main_id = func_ids
			.get(ir_module.main.0 as usize)
			.copied()
			.ok_or(JitError::MissingMain { main: ir_module.main.0 })?;
		let entry = self.module.get_finalized_function(main_id);

		// SAFETY: `entry` is the finalized address for a function declared and
		// defined with the Phase-A compiled Python ABI.
		Ok(unsafe { mem::transmute::<*const u8, MainFn>(entry) })
	}

	/// Initialize the runtime, compile the module, and invoke `__main__(NULL,
	/// 0)`.
	///
	/// A NULL return from compiled code is the Phase-A runtime-error sentinel
	/// and is reported as [`JitError::Runtime`].
	pub fn run(&mut self, ir_module: &IrModule) -> Result<(), JitError> {
		// SAFETY: `pon_runtime_init` is the C-ABI runtime initializer and is
		// idempotent for the process.
		let init_status = unsafe { pon_runtime_init() };
		if init_status != 0 {
			return Err(JitError::RuntimeInit(runtime_message()));
		}

		let main = self.compile(ir_module)?;
		let argv = ptr::null_mut();
		// SAFETY: `main` was returned by `compile`, and `(NULL, 0)` is the
		// Phase-A top-level invocation ABI.
		let result = unsafe { main(argv, 0) };
		if result.is_null() {
			return Err(JitError::Runtime(runtime_message()));
		}
		Ok(())
	}

	fn declare_ir_functions(
		&mut self,
		ir_module: &IrModule,
	) -> Result<Vec<cranelift_module::FuncId>, JitError> {
		let mut sig = self.module.make_signature();
		let ptr_ty = self.module.target_config().pointer_type();
		sig.params.push(AbiParam::new(ptr_ty));
		sig.params.push(AbiParam::new(ptr_ty));
		sig.returns.push(AbiParam::new(ptr_ty));

		ir_module
			.functions
			.iter()
			.enumerate()
			.map(|(index, _function)| {
				let symbol = format!("__pon_fn_{index}");
				self
					.module
					.declare_function(&symbol, Linkage::Local, &sig)
					.map_err(JitError::from)
			})
			.collect()
	}
}

/// Compile source text for the runtime dynamic-execution seam.
///
/// `filename` and `mode` are retained for diagnostics and future mode-specific
/// lowering; today the parser accepts the same module grammar as `pon run`.
pub fn compile_source_to_module(
	source: &str,
	filename: &str,
	mode: &str,
) -> Result<DynExecHandle, JitError> {
	// SAFETY: `pon_runtime_init` is idempotent for the process.
	let init_status = unsafe { pon_runtime_init() };
	if init_status != 0 {
		return Err(JitError::RuntimeInit(runtime_message()));
	}

	let ir_module = lower_source(source).map_err(|error| JitError::Lower(error.to_string()))?;
	let mut engine = JitEngine::new();
	let main = engine.compile(&ir_module)?;
	Ok(DynExecHandle { engine, main, filename: filename.to_owned(), mode: mode.to_owned() })
}

/// Execute a previously compiled dynamic source handle.
///
/// The globals/locals pointers are accepted for the runtime-owned eval/exec
/// bridge; this thin JIT seam preserves the compiled engine lifetime and
/// invokes the compiled module body.
///
/// # Safety
/// `handle` must be the live handle returned by [`compile_source_to_module`].
/// `globals_dict` and `locals_dict`, when non-NULL, must be live Python objects
/// managed by `pon-runtime`; this function does not dereference them yet.
pub unsafe fn execute(
	handle: &mut DynExecHandle,
	_globals_dict: *mut PyObject,
	_locals_dict: *mut PyObject,
) -> *mut PyObject {
	let _keep_engine_alive = &handle.engine;
	unsafe { (handle.main)(ptr::null_mut(), 0) }
}

impl Default for JitEngine {
	fn default() -> Self {
		Self::new()
	}
}

impl Drop for JitEngine {
	fn drop(&mut self) {
		tierup::unregister_runtime_hook(self.tierup.as_ref());
	}
}

fn runtime_message() -> String {
	pon_err_message().unwrap_or_else(|| "runtime returned NULL without a diagnostic".to_owned())
}

#[cfg(feature = "free-threading")]
fn register_free_threading_symbols(builder: &mut JITBuilder) {
	builder.symbol(pon_codegen::FT_SAFEPOINT_POLL, jit_safepoint_poll as *const u8);
	builder.symbol(pon_codegen::FT_GC_WRITE_BARRIER, jit_gc_write_barrier as *const u8);
	builder.symbol(pon_codegen::FT_GC_STOP_REQUESTED, jit_gc_stop_requested as *const u8);
}

#[cfg(not(feature = "free-threading"))]
fn register_free_threading_symbols(_builder: &mut JITBuilder) {}

#[cfg(feature = "free-threading")]
unsafe extern "C" fn jit_safepoint_poll() {
	if pon_gc::gc_stop_requested() {
		std::hint::spin_loop();
	}
}

#[cfg(feature = "free-threading")]
unsafe extern "C" fn jit_gc_write_barrier(slot: *mut *mut PyObject, new: *mut PyObject) {
	pon_gc::WriteBarrier::record(slot.cast::<*mut u8>(), new.cast::<u8>());
}

#[cfg(feature = "free-threading")]
unsafe extern "C" fn jit_gc_stop_requested() -> bool {
	pon_gc::gc_stop_requested()
}
