//! CLIF `main` trampoline for AoT executables.

use cranelift_codegen::ir::{self, AbiParam, InstBuilder, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, FuncId, Linkage, Module, ModuleResult};
use cranelift_object::ObjectModule;
use pon_codegen::{AOT_INIT_NAMES, AOT_INTERN_NAME, AOT_MODULE_MAIN};

/// Define `main(argc, argv) -> i32` as a tiny trampoline to runtime `pon_aot_entry`.
pub fn define_main_trampoline(module: &mut ObjectModule) -> ModuleResult<FuncId> {
    let ptr_ty = module.target_config().pointer_type();

    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I32));
    sig.params.push(AbiParam::new(ptr_ty));
    sig.returns.push(AbiParam::new(types::I32));

    let main_id = module.declare_function("main", Linkage::Export, &sig)?;
    let entry_id = module.declare_function("pon_aot_entry", Linkage::Import, &sig)?;

    let mut ctx = module.make_context();
    ctx.func.signature = sig;
    let entry_ref = module.declare_func_in_func(entry_id, &mut ctx.func);

    let mut fctx = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fctx);
    let block = builder.create_block();
    builder.append_block_params_for_function_params(block);
    builder.switch_to_block(block);
    builder.seal_block(block);

    let argc = builder.func.dfg.block_params(block)[0];
    let argv = builder.func.dfg.block_params(block)[1];
    let call = builder.ins().call(entry_ref, &[argc, argv]);
    let status = builder.func.dfg.inst_results(call)[0];
    builder.ins().return_(&[status]);
    builder.seal_all_blocks();
    builder.finalize();

    module.define_function(main_id, &mut ctx)?;
    Ok(main_id)
}

/// Define exported `pon_aot_init_names()`.
///
/// Codegen embeds compact runtime name ids into helper calls. In JIT mode those
/// ids are allocated in the same process that runs the code; in AoT mode the
/// executable starts with a fresh interner. This hook replays the build-time
/// interner prefix before `pon_runtime_init` registers builtins, so embedded ids
/// name the same strings in the generated process.
pub fn define_aot_name_initializer(module: &mut ObjectModule, names: &[String]) -> ModuleResult<FuncId> {
    let ptr_ty = module.target_config().pointer_type();

    let init_sig = module.make_signature();
    let init_id = module.declare_function(AOT_INIT_NAMES, Linkage::Export, &init_sig)?;

    let mut intern_sig = module.make_signature();
    intern_sig.params.push(AbiParam::new(ptr_ty));
    intern_sig.params.push(AbiParam::new(ptr_ty));
    let intern_id = module.declare_function(AOT_INTERN_NAME, Linkage::Import, &intern_sig)?;

    let mut ctx = module.make_context();
    ctx.func.signature = init_sig;
    let intern_ref = module.declare_func_in_func(intern_id, &mut ctx.func);

    let mut fctx = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fctx);
    let block = builder.create_block();
    builder.switch_to_block(block);
    builder.seal_block(block);

    for name in names {
        let data = declare_name_data(module, &mut builder, name, ptr_ty)?;
        let len = builder.ins().iconst(ptr_ty, name.len() as i64);
        builder.ins().call(intern_ref, &[data, len]);
    }

    builder.ins().return_(&[]);
    builder.seal_all_blocks();
    builder.finalize();

    module.define_function(init_id, &mut ctx)?;
    Ok(init_id)
}

fn declare_name_data(
    module: &mut ObjectModule,
    builder: &mut FunctionBuilder<'_>,
    value: &str,
    ptr_ty: ir::Type,
) -> ModuleResult<ir::Value> {
    let data_id = module.declare_anonymous_data(false, false)?;
    let mut data = DataDescription::new();
    data.set_align(1);
    if value.is_empty() {
        data.define(vec![0_u8].into_boxed_slice());
    } else {
        data.define(value.as_bytes().to_vec().into_boxed_slice());
    }
    module.define_data(data_id, &data)?;
    let global = module.declare_data_in_func(data_id, builder.func);
    Ok(builder.ins().global_value(ptr_ty, global))
}

/// Define exported `pon_module_main() -> PyObject*` as the runtime-facing AoT ABI.
///
/// The real lowered top-level body keeps the baseline/JIT ABI
/// `(argv: PyObject**, argc: usize) -> PyObject*`. AoT process startup does not
/// pass Python call arguments into module top-level code, so this wrapper calls
/// the body with a null argv and zero argc while presenting the zero-argument
/// symbol imported by `pon_aot_entry`.
pub fn define_module_main_wrapper(module: &mut ObjectModule, body_id: FuncId) -> ModuleResult<FuncId> {
    let ptr_ty = module.target_config().pointer_type();

    let mut sig = module.make_signature();
    sig.returns.push(AbiParam::new(ptr_ty));

    let wrapper_id = module.declare_function(AOT_MODULE_MAIN, Linkage::Export, &sig)?;

    let mut ctx = module.make_context();
    ctx.func.signature = sig;
    let body_ref = module.declare_func_in_func(body_id, &mut ctx.func);

    let mut fctx = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fctx);
    let block = builder.create_block();
    builder.switch_to_block(block);
    builder.seal_block(block);

    let argv = builder.ins().iconst(ptr_ty, 0);
    let argc = builder.ins().iconst(ptr_ty, 0);
    let call = builder.ins().call(body_ref, &[argv, argc]);
    let result = builder.func.dfg.inst_results(call)[0];
    builder.ins().return_(&[result]);
    builder.seal_all_blocks();
    builder.finalize();

    module.define_function(wrapper_id, &mut ctx)?;
    Ok(wrapper_id)
}
