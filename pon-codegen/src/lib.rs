#![doc = "Cranelift codegen support for Pon."]
#![doc = "This crate publishes shared ISA configuration, stable runtime helper"]
#![doc = "imports, Phase-B helper signatures, and baseline IR lowering."]

use cranelift_codegen::Context;
use cranelift_codegen::ir::AbiParam;
use cranelift_frontend::FunctionBuilderContext;
use cranelift_module::{FuncId, Linkage, Module as ClifModule, ModuleError};
use pon_ir::ir::Module as IrModule;

use crate::baseline::{CodegenError, NameMap, compile_function as compile_baseline_function};
use crate::helpers::declare_helpers;

/// Stable exported symbol for the zero-argument AoT module wrapper.
pub const AOT_MODULE_MAIN: &str = "pon_module_main";

/// Object-defined AoT hook that seeds runtime name ids before runtime startup.
pub const AOT_INIT_NAMES: &str = "pon_aot_init_names";

/// Runtime helper imported by the AoT name-id seed hook.
pub const AOT_INTERN_NAME: &str = "pon_aot_intern_name";

/// Free-threaded generated-code safepoint helper.
#[cfg(feature = "free-threading")]
pub const FT_SAFEPOINT_POLL: &str = "pon_safepoint_poll";

/// Free-threaded generated-code write-barrier helper.
#[cfg(feature = "free-threading")]
pub const FT_GC_WRITE_BARRIER: &str = "pon_gc_write_barrier";

/// Free-threaded generated-code stop-request query helper.
#[cfg(feature = "free-threading")]
pub const FT_GC_STOP_REQUESTED: &str = "pon_gc_stop_requested";

/// Local symbol for the real boxed top-level AoT body.
const AOT_MODULE_BODY: &str = "__pon_module_body";

/// Code-generation consumer mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompileMode {
    /// JIT mode keeps every lowered IR function local and addressable by `FuncId`.
    Jit,
    /// AoT mode gives the IR module's top-level body a stable local symbol so
    /// the object backend can export a zero-argument [`AOT_MODULE_MAIN`] wrapper.
    Aot,
}

/// Declare, lower, and define every function in `ir_module` into `module`.
///
/// This is the public module-agnostic cutover point for the existing baseline
/// lowering: JIT callers may keep using their historical path, while AoT callers
/// can reuse the returned `FuncId`s to wrap the top-level body with the process
/// entry ABI expected by the runtime.
pub fn compile_ir_module<M: ClifModule>(
    module: &mut M,
    ir_module: &IrModule,
    mode: CompileMode,
    ctx: &mut Context,
    fctx: &mut FunctionBuilderContext,
) -> Result<Vec<FuncId>, CodegenError> {
    let helpers = declare_helpers(module)?;
    let func_ids = declare_ir_functions(module, ir_module, mode)?;
    let names = NameMap::from_ir_module(ir_module);
    let entry_arg_counts = baseline::entry_arg_counts(ir_module);


    for (index, function) in ir_module.functions.iter().enumerate() {
        compile_baseline_function(module, &helpers, &func_ids, &ir_module.functions, &names, function, entry_arg_counts[index], ctx, fctx)?;
        module.define_function(func_ids[index], ctx)?;
    }

    Ok(func_ids)
}

/// Compile typed IR through the Phase-D optimizing entry point.
///
/// Functions with a typed region that the optimizing lowerer can currently
/// handle are emitted through the unboxed fast-path compiler.  Everything else
/// is lowered by the baseline boxed compiler, preserving tier-0 semantics for
/// functions that have no safe typed region yet.
pub fn compile_optimized_ir_module<M: ClifModule>(
    module: &mut M,
    ir_module: &IrModule,
    mode: CompileMode,
    ctx: &mut Context,
    fctx: &mut FunctionBuilderContext,
) -> Result<Vec<FuncId>, CodegenError> {
    let helpers = declare_helpers(module)?;
    let func_ids = declare_ir_functions(module, ir_module, mode)?;
    let names = NameMap::from_ir_module(ir_module);
    let entry_arg_counts = baseline::entry_arg_counts(ir_module);


    for (index, function) in ir_module.functions.iter().enumerate() {
        match optimizing::plan_function(function).filter(optimizing::can_compile_plan) {
            Some(plan) => optimizing::compile_function(
                module, &helpers, &func_ids, &names, function, &plan, ctx, fctx,
            )?,
            None => compile_baseline_function(
                module,
                &helpers,
                &func_ids,
                &ir_module.functions,
                &names,
                function,
                entry_arg_counts[index],
                ctx,
                fctx,
            )?,
        }
        module.define_function(func_ids[index], ctx)?;
    }

    Ok(func_ids)
}

fn declare_ir_functions<M: ClifModule>(
    module: &mut M,
    ir_module: &IrModule,
    mode: CompileMode,
) -> Result<Vec<FuncId>, ModuleError> {
    let mut sig = module.make_signature();
    let ptr_ty = module.target_config().pointer_type();
    sig.params.push(AbiParam::new(ptr_ty));
    sig.params.push(AbiParam::new(ptr_ty));
    sig.returns.push(AbiParam::new(ptr_ty));

    ir_module
        .functions
        .iter()
        .enumerate()
        .map(|(index, _function)| {
            let is_main = index == ir_module.main.0 as usize;
            let (symbol, linkage) = match (mode, is_main) {
                (CompileMode::Aot, true) => (AOT_MODULE_BODY.to_owned(), Linkage::Local),
                _ => (format!("__pon_fn_{index}"), Linkage::Local),
            };
            module.declare_function(&symbol, linkage, &sig)
        })
        .collect()
}


/// Ruff-AST annotation reader and opt-only annotation scrubber.
pub mod annotations;
/// Baseline Cranelift lowering for boxed Python IR with Phase-B family hubs.
pub mod baseline;
/// Phase-D optimizing codegen planning and cold-twin lowering skeleton.
pub mod optimizing;
/// Runtime helper import declaration and Phase-B signature metadata.
pub mod helpers;
/// Local typed metadata inference for Phase-D AoT.
pub mod infer;
/// Shared Cranelift ISA and flag construction helpers.
pub mod isa;
/// Typed-region discovery for future optimizing-tier entry points.
pub mod region;

pub use annotations::{
    AnnotationSource, FunctionAnnotations, LocalAnnotation, ModuleAnnotations,
    read_module_annotations, strip_annotations_for_lowering,
};
pub use infer::infer_module_types;
pub use optimizing::{
    ColdCallSite, ColdTwinPlan, EntryGuard, FastPathPlan, GuardFailure, LoweringStep, OptimizingPlan,
    StackMapDecl, lowering_steps, plan_function, plan_region,
};
pub use region::{
    RegionExit, RegionExitKind, TypedInput, TypedRegion, TypedValue, find_maximal_typed_region,
    inst_operands, inst_unboxed_type, is_fast_path_kind, terminator_operands,
};

#[cfg(test)]
mod tests {
    use super::*;

    use cranelift_frontend::FunctionBuilderContext;
    use cranelift_module::{Module, default_libcall_names};
    use pon_ir::{
        ir::{Block, BlockId, Function, FunctionId, Inst, InstKind, LocalId, Module as IrModule, Terminator, Value},
        types::Type,
    };
    use pon_runtime::abi::HELPERS;

    fn jit_module() -> cranelift_jit::JITModule {
        let isa = crate::isa::make_isa(crate::isa::OptLevel::None, false);
        let mut builder = cranelift_jit::JITBuilder::with_isa(isa, default_libcall_names());
        for helper in HELPERS {
            builder.symbol(helper.symbol, helper.address.cast::<u8>());
        }
        register_free_threading_symbols(&mut builder);
        cranelift_jit::JITModule::new(builder)
    }

    #[cfg(feature = "free-threading")]
    fn register_free_threading_symbols(builder: &mut cranelift_jit::JITBuilder) {
        unsafe extern "C" fn safepoint_poll() {}
        unsafe extern "C" fn write_barrier(_slot: *mut *mut pon_runtime::object::PyObject, _new: *mut pon_runtime::object::PyObject) {}
        unsafe extern "C" fn stop_requested() -> bool {
            false
        }

        builder.symbol(FT_SAFEPOINT_POLL, safepoint_poll as *const u8);
        builder.symbol(FT_GC_WRITE_BARRIER, write_barrier as *const u8);
        builder.symbol(FT_GC_STOP_REQUESTED, stop_requested as *const u8);
    }

    #[cfg(not(feature = "free-threading"))]
    fn register_free_threading_symbols(_builder: &mut cranelift_jit::JITBuilder) {}

    fn optimizable_load_local_module() -> IrModule {
        IrModule {
            functions: vec![Function {
                name: "typed_arg".to_owned(),
                arity: 1,
                n_locals: 1,
                blocks: vec![Block {
                    id: BlockId(0),
                    insts: vec![
                        Inst::new(Value(0), InstKind::LoadLocal(LocalId(0)))
                            .with_inferred_type(Type::IntI64),
                    ],
                    term: Terminator::Return(Value(0)),
                }],
            }],
            main: FunctionId(0),
            names: vec![],
        }
    }

    fn compiled_entry_clif(ir_module: &IrModule, optimized: bool) -> String {
        let mut module = jit_module();
        let mut ctx = module.make_context();
        let mut fctx = FunctionBuilderContext::new();

        let func_ids = if optimized {
            compile_optimized_ir_module(&mut module, ir_module, CompileMode::Jit, &mut ctx, &mut fctx)
        } else {
            compile_ir_module(&mut module, ir_module, CompileMode::Jit, &mut ctx, &mut fctx)
        }
        .expect("module compiles");

        assert_eq!(func_ids.len(), ir_module.functions.len());
        ctx.func.display().to_string()
    }

    #[test]
    fn optimized_module_entry_uses_typed_lowering_for_optimizable_function() {
        let ir_module = optimizable_load_local_module();
        assert!(
            plan_function(&ir_module.functions[0]).is_some(),
            "fixture must remain eligible for the optimizing entry"
        );

        let baseline = compiled_entry_clif(&ir_module, false);
        let optimized = compiled_entry_clif(&ir_module, true);

        let payload_offset = crate::optimizing::pylong_value_offset_i32(
            jit_module().target_config().pointer_type().bytes() as usize,
        )
        .expect("PyLong payload offset fits CLIF offset");
        assert!(
            !baseline.contains(&format!("+{payload_offset}")),
            "baseline entry should stay on the boxed lowering path:\n{baseline}"
        );
        assert!(
            optimized.contains(&format!("+{payload_offset}")),
            "optimized entry should unbox the PyLong payload in the typed fast path:\n{optimized}"
        );
        assert!(
            optimized.contains("brif"),
            "optimized entry should emit typed guard control flow rather than pure boxed CLIF:\n{optimized}"
        );
    }
}
