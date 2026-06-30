//! Baseline IR-to-CLIF lowering for Phase A.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use cranelift_codegen::Context;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{self, AbiParam, FuncRef, InstBuilder, MemFlagsData, StackSlotData, StackSlotKind};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{DataDescription, FuncId, Module, ModuleError};
use pon_ir::ir::{BinOp, Function, InstKind, Module as IrModule, PyConst, Terminator, Value as IrValue};

use crate::helpers::HelperRefs;

/// Runtime-name id remapping for a lowered IR module.
///
/// `pon-ir` name operands are source-local indexes into `pon_ir::ir::Module::names`.
/// Runtime helpers consume ids from `pon_runtime::intern`, so codegen must remap
/// every source-local id before emitting `LoadGlobal`, `LoadName`, `StoreGlobal`,
/// or `MakeFunction` helper arguments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NameMap {
    runtime_ids: Vec<u32>,
}

impl NameMap {
    /// Build the runtime-id map for all names in an IR module.
    #[must_use]
    pub fn from_ir_module(module: &IrModule) -> Self {
        Self {
            runtime_ids: module
                .names
                .iter()
                .map(|name| pon_runtime::intern::intern(name))
                .collect(),
        }
    }

    fn runtime_id(&self, source_id: u32) -> Result<u32, CodegenError> {
        self.runtime_ids
            .get(source_id as usize)
            .copied()
            .ok_or(CodegenError::NameOutOfRange { source_id })
    }
}

/// Error reported while lowering Phase-A IR into Cranelift IR.
#[derive(Debug)]
pub enum CodegenError {
    /// Cranelift module declaration or data definition failed.
    Module(ModuleError),
    /// A source-local IR name id has no runtime interner mapping.
    NameOutOfRange { source_id: u32 },
    /// A function index referenced by `MakeFunction` has no declared `FuncId`.
    FunctionIndexOutOfRange { func_index: u32 },
    /// A local slot index is outside the function's declared local range.
    LocalOutOfRange { slot: u32, n_locals: usize },
    /// A local slot was read before a parameter load or local store defined it.
    LocalUsedBeforeDefinition { slot: u32 },
    /// An SSA value operand was referenced before its producing instruction lowered.
    ValueNotDefined(IrValue),
    /// A stack or memory offset does not fit Cranelift's 32-bit offset immediate.
    OffsetTooLarge { offset: usize },
    /// Phase A received an IR operation reserved for a later phase.
    Unsupported(&'static str),
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Module(error) => write!(f, "Cranelift module error: {error}"),
            Self::NameOutOfRange { source_id } => {
                write!(f, "IR name id {source_id} has no runtime interner mapping")
            }
            Self::FunctionIndexOutOfRange { func_index } => {
                write!(f, "function index {func_index} has no declared FuncId")
            }
            Self::LocalOutOfRange { slot, n_locals } => {
                write!(f, "local slot {slot} is outside n_locals={n_locals}")
            }
            Self::LocalUsedBeforeDefinition { slot } => {
                write!(f, "local slot {slot} was used before definition")
            }
            Self::ValueNotDefined(value) => write!(f, "SSA value {:?} was used before definition", value),
            Self::OffsetTooLarge { offset } => write!(f, "offset {offset} does not fit in i32"),
            Self::Unsupported(op) => write!(f, "unsupported Phase-A lowering operation: {op}"),
        }
    }
}

impl Error for CodegenError {}

impl From<ModuleError> for CodegenError {
    fn from(error: ModuleError) -> Self {
        Self::Module(error)
    }
}

#[derive(Clone, Copy)]
struct HelperFuncRefs {
    const_int: FuncRef,
    const_str: FuncRef,
    binary_add: FuncRef,
    call: FuncRef,
    load_global: FuncRef,
    make_function: FuncRef,
    store_global: FuncRef,
    none: FuncRef,
}

struct LowerState {
    values: HashMap<IrValue, ir::Value>,
    locals: Vec<Variable>,
    local_defined: Vec<bool>,
}

impl LowerState {
    fn new(local_count: usize) -> Self {
        Self {
            values: HashMap::new(),
            locals: Vec::with_capacity(local_count),
            local_defined: vec![false; local_count],
        }
    }

    fn define_value(&mut self, ir_value: IrValue, clif_value: ir::Value) {
        self.values.insert(ir_value, clif_value);
    }

    fn value(&self, ir_value: IrValue) -> Result<ir::Value, CodegenError> {
        self.values
            .get(&ir_value)
            .copied()
            .ok_or(CodegenError::ValueNotDefined(ir_value))
    }
}

/// Lower one IR function into the supplied Cranelift [`Context`].
///
/// The emitted function ABI is always
/// `(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject`, represented in
/// CLIF as `(ptr, ptr) -> ptr`. Parameter locals `0..arity` are initialized by
/// loading boxed pointers from `argv + slot * pointer_size`. Runtime helper calls
/// returning boxed objects are followed by the Phase-A NULL-sentinel branch to a
/// shared exception exit that returns NULL.
///
/// `names` must be built with [`NameMap::from_ir_module`] for the enclosing IR
/// module; raw source-local IR ids must never be passed directly to runtime
/// helpers.
pub fn compile_function<M: Module>(
    module: &mut M,
    helpers: &HelperRefs,
    func_ids: &[FuncId],
    names: &NameMap,
    ir: &Function,
    ctx: &mut Context,
    fctx: &mut FunctionBuilderContext,
) -> Result<(), CodegenError> {
    module.clear_context(ctx);
    let ptr_ty = module.target_config().pointer_type();
    let ptr_bytes = ptr_ty.bytes() as usize;

    ctx.func.signature.params.push(AbiParam::new(ptr_ty));
    ctx.func.signature.params.push(AbiParam::new(ptr_ty));
    ctx.func.signature.returns.push(AbiParam::new(ptr_ty));

    let helper_refs = declare_helper_refs(module, helpers, &mut ctx.func);

    let mut builder = FunctionBuilder::new(&mut ctx.func, fctx);
    let entry = builder.create_block();
    let exception_exit = builder.create_block();
    builder.set_cold_block(exception_exit);
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);

    let argv = builder.func.dfg.block_params(entry)[0];
    let _argc = builder.func.dfg.block_params(entry)[1];

    let mut state = LowerState::new(ir.n_locals);
    for _ in 0..ir.n_locals {
        state.locals.push(builder.declare_var(ptr_ty));
    }
    initialize_parameter_locals(&mut builder, &mut state, argv, ptr_bytes, ir.arity, ir.n_locals, ptr_ty)?;

    for block in &ir.blocks {
        if block.id.0 != 0 {
            return Err(CodegenError::Unsupported("non-entry basic block"));
        }
        for inst in &block.insts {
            let value = lower_inst(
                module,
                &mut builder,
                &helper_refs,
                func_ids,
                names,
                &mut state,
                ptr_ty,
                ptr_bytes,
                exception_exit,
                &inst.kind,
            )?;
            state.define_value(inst.result, value);
        }
        lower_terminator(&mut builder, &state, ptr_ty, &block.term)?;
    }

    builder.switch_to_block(exception_exit);
    let null = builder.ins().iconst(ptr_ty, 0);
    builder.ins().return_(&[null]);
    builder.seal_all_blocks();
    builder.finalize();

    Ok(())
}

fn declare_helper_refs<M: Module>(module: &mut M, helpers: &HelperRefs, func: &mut ir::Function) -> HelperFuncRefs {
    HelperFuncRefs {
        const_int: module.declare_func_in_func(helpers.const_int, func),
        const_str: module.declare_func_in_func(helpers.const_str, func),
        binary_add: module.declare_func_in_func(helpers.binary_add, func),
        call: module.declare_func_in_func(helpers.call, func),
        load_global: module.declare_func_in_func(helpers.load_global, func),
        make_function: module.declare_func_in_func(helpers.make_function, func),
        store_global: module.declare_func_in_func(helpers.store_global, func),
        none: module.declare_func_in_func(helpers.none, func),
    }
}

fn initialize_parameter_locals(
    builder: &mut FunctionBuilder<'_>,
    state: &mut LowerState,
    argv: ir::Value,
    ptr_bytes: usize,
    arity: usize,
    n_locals: usize,
    ptr_ty: ir::Type,
) -> Result<(), CodegenError> {
    if arity > n_locals {
        return Err(CodegenError::LocalOutOfRange { slot: arity as u32, n_locals });
    }
    for slot in 0..arity {
        let offset = offset_i32(slot * ptr_bytes)?;
        let value = builder.ins().load(ptr_ty, MemFlagsData::new(), argv, offset);
        builder.def_var(state.locals[slot], value);
        state.local_defined[slot] = true;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn lower_inst<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    func_ids: &[FuncId],
    names: &NameMap,
    state: &mut LowerState,
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
    kind: &InstKind,
) -> Result<ir::Value, CodegenError> {
    match kind {
        InstKind::Const(PyConst::Int(value)) => {
            let arg = builder.ins().iconst(ir::types::I64, *value);
            Ok(call_pyobject_helper(builder, helpers.const_int, &[arg], ptr_ty, exception_exit))
        }
        InstKind::Const(PyConst::Str(value)) => {
            let data_ptr = declare_string_data(module, builder, value, ptr_ty)?;
            let len = builder.ins().iconst(ptr_ty, value.len() as i64);
            Ok(call_pyobject_helper(builder, helpers.const_str, &[data_ptr, len], ptr_ty, exception_exit))
        }
        InstKind::Const(PyConst::None) => Ok(call_pyobject_helper(builder, helpers.none, &[], ptr_ty, exception_exit)),
        InstKind::Const(PyConst::Float(_)) => Err(CodegenError::Unsupported("Const(Float)")),
        InstKind::LoadLocal(slot) => load_local(builder, state, *slot),
        InstKind::StoreLocal(slot, value) => {
            let value = state.value(*value)?;
            store_local(builder, state, *slot, value)?;
            Ok(value)
        }
        InstKind::LoadGlobal(name) | InstKind::LoadName(name) => {
            let runtime_name = builder.ins().iconst(ir::types::I32, i64::from(names.runtime_id(*name)?));
            Ok(call_pyobject_helper(
                builder,
                helpers.load_global,
                &[runtime_name],
                ptr_ty,
                exception_exit,
            ))
        }
        InstKind::StoreGlobal(name, value) => {
            let runtime_name = builder.ins().iconst(ir::types::I32, i64::from(names.runtime_id(*name)?));
            let value = state.value(*value)?;
            Ok(call_pyobject_helper(
                builder,
                helpers.store_global,
                &[runtime_name, value],
                ptr_ty,
                exception_exit,
            ))
        }
        InstKind::BinaryOp { op: BinOp::Add, lhs, rhs } => {
            let lhs = state.value(*lhs)?;
            let rhs = state.value(*rhs)?;
            Ok(call_pyobject_helper(builder, helpers.binary_add, &[lhs, rhs], ptr_ty, exception_exit))
        }
        InstKind::BinaryOp { .. } => Err(CodegenError::Unsupported("BinaryOp other than Add")),
        InstKind::Call { callee, args } => {
            let callee = state.value(*callee)?;
            let argv = build_call_argv(builder, state, args, ptr_ty, ptr_bytes)?;
            let argc = builder.ins().iconst(ptr_ty, args.len() as i64);
            Ok(call_pyobject_helper(builder, helpers.call, &[callee, argv, argc], ptr_ty, exception_exit))
        }
        InstKind::MakeFunction {
            func_index,
            name_interned,
            arity,
        } => {
            let func_id = *func_ids
                .get(*func_index as usize)
                .ok_or(CodegenError::FunctionIndexOutOfRange { func_index: *func_index })?;
            let func_ref = module.declare_func_in_func(func_id, builder.func);
            let code = builder.ins().func_addr(ptr_ty, func_ref);
            let arity = builder.ins().iconst(ptr_ty, *arity as i64);
            let runtime_name = builder.ins().iconst(ir::types::I32, i64::from(names.runtime_id(*name_interned)?));
            Ok(call_pyobject_helper(
                builder,
                helpers.make_function,
                &[code, arity, runtime_name],
                ptr_ty,
                exception_exit,
            ))
        }
        _ => Err(CodegenError::Unsupported("unknown future InstKind")),
    }
}

fn lower_terminator(
    builder: &mut FunctionBuilder<'_>,
    state: &LowerState,
    ptr_ty: ir::Type,
    term: &Terminator,
) -> Result<(), CodegenError> {
    match term {
        Terminator::Return(value) => {
            let value = state.value(*value)?;
            builder.ins().return_(&[value]);
            Ok(())
        }
        Terminator::Jump(_) | Terminator::Branch { .. } | Terminator::Unreachable => {
            let null = builder.ins().iconst(ptr_ty, 0);
            builder.ins().return_(&[null]);
            Err(CodegenError::Unsupported("non-return terminator"))
        }
        _ => Err(CodegenError::Unsupported("unknown future terminator")),
    }
}

fn load_local(
    builder: &mut FunctionBuilder<'_>,
    state: &mut LowerState,
    slot: u32,
) -> Result<ir::Value, CodegenError> {
    let index = slot as usize;
    if index >= state.locals.len() {
        return Err(CodegenError::LocalOutOfRange { slot, n_locals: state.locals.len() });
    }
    if !state.local_defined[index] {
        return Err(CodegenError::LocalUsedBeforeDefinition { slot });
    }
    Ok(builder.use_var(state.locals[index]))
}

fn store_local(
    builder: &mut FunctionBuilder<'_>,
    state: &mut LowerState,
    slot: u32,
    value: ir::Value,
) -> Result<(), CodegenError> {
    let index = slot as usize;
    if index >= state.locals.len() {
        return Err(CodegenError::LocalOutOfRange { slot, n_locals: state.locals.len() });
    }
    builder.def_var(state.locals[index], value);
    state.local_defined[index] = true;
    Ok(())
}

fn declare_string_data<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder<'_>,
    value: &str,
    ptr_ty: ir::Type,
) -> Result<ir::Value, CodegenError> {
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

fn build_call_argv(
    builder: &mut FunctionBuilder<'_>,
    state: &LowerState,
    args: &[IrValue],
    ptr_ty: ir::Type,
    ptr_bytes: usize,
) -> Result<ir::Value, CodegenError> {
    if args.is_empty() {
        return Ok(builder.ins().iconst(ptr_ty, 0));
    }

    let size = args
        .len()
        .checked_mul(ptr_bytes)
        .ok_or(CodegenError::OffsetTooLarge { offset: usize::MAX })?;
    let slot = builder.create_sized_stack_slot(StackSlotData {
        kind: StackSlotKind::ExplicitSlot,
        size: size.try_into().map_err(|_| CodegenError::OffsetTooLarge { offset: size })?,
        align_shift: ptr_bytes.trailing_zeros() as u8,
        key: None,
    });
    for (index, arg) in args.iter().enumerate() {
        let value = state.value(*arg)?;
        let offset = offset_i32(index * ptr_bytes)?;
        // PHASE-E: WriteBarrier
        builder.ins().stack_store(value, slot, offset);
    }
    Ok(builder.ins().stack_addr(ptr_ty, slot, 0))
}

fn call_pyobject_helper(
    builder: &mut FunctionBuilder<'_>,
    helper: FuncRef,
    args: &[ir::Value],
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> ir::Value {
    // PHASE-D: stack-map safepoint
    let call = builder.ins().call(helper, args);
    let result = builder.func.dfg.inst_results(call)[0];
    emit_null_check(builder, result, ptr_ty, exception_exit);
    result
}

fn emit_null_check(
    builder: &mut FunctionBuilder<'_>,
    value: ir::Value,
    _ptr_ty: ir::Type,
    exception_exit: ir::Block,
) {
    let continue_block = builder.create_block();
    let non_null = builder.ins().icmp_imm(IntCC::NotEqual, value, 0);
    builder.ins().brif(non_null, continue_block, &[], exception_exit, &[]);
    builder.switch_to_block(continue_block);
    builder.seal_block(continue_block);
}

fn offset_i32(offset: usize) -> Result<i32, CodegenError> {
    i32::try_from(offset).map_err(|_| CodegenError::OffsetTooLarge { offset })
}

#[cfg(test)]
mod tests {
    use cranelift_frontend::FunctionBuilderContext;
    use cranelift_module::{Linkage, Module};
    use pon_ir::ir::{Block, BlockId, FunctionId, Inst, Module as IrModule, Value};
    use pon_runtime::abi::HELPERS;

    use super::*;
    use crate::helpers::declare_helpers;

    fn jit_module() -> cranelift_jit::JITModule {
        let isa = crate::isa::make_isa(crate::isa::OptLevel::None, false);
        let mut builder = cranelift_jit::JITBuilder::with_isa(
            isa,
            cranelift_module::default_libcall_names(),
        );
        for helper in HELPERS {
            builder.symbol(helper.symbol, helper.address.cast::<u8>());
        }
        cranelift_jit::JITModule::new(builder)
    }

    fn compiled_clif(ir_module: &IrModule, function_index: usize) -> String {
        let mut module = jit_module();
        let helpers = declare_helpers(&mut module).expect("helpers declare");
        let mut sig = module.make_signature();
        let ptr = module.target_config().pointer_type();
        sig.params.push(AbiParam::new(ptr));
        sig.params.push(AbiParam::new(ptr));
        sig.returns.push(AbiParam::new(ptr));
        let func_ids = ir_module
            .functions
            .iter()
            .map(|func| {
                module
                    .declare_function(&func.name, Linkage::Local, &sig)
                    .expect("function declare")
            })
            .collect::<Vec<_>>();
        let names = NameMap::from_ir_module(ir_module);

        let mut rendered_functions = Vec::with_capacity(ir_module.functions.len());
        for (index, function) in ir_module.functions.iter().enumerate() {
            let mut ctx = module.make_context();
            let mut fctx = FunctionBuilderContext::new();
            compile_function(
                &mut module,
                &helpers,
                &func_ids,
                &names,
                function,
                &mut ctx,
                &mut fctx,
            )
            .expect("function compiles");

            let mut rendered = String::new();
            for (_, decl) in module.declarations().get_functions() {
                if let Some(name) = &decl.name {
                    rendered.push_str(name);
                    rendered.push('\n');
                }
            }
            rendered.push_str(&ctx.func.display().to_string());
            module
                .define_function(func_ids[index], &mut ctx)
                .expect("function defines");
            rendered_functions.push(rendered);
        }
        module
            .finalize_definitions()
            .expect("compiled functions finalize");
        rendered_functions.remove(function_index)
    }

    #[test]
    fn add_function_clif_calls_binary_add_and_checks_null() {
        let ir = IrModule {
            functions: vec![Function {
                name: "add".to_owned(),
                arity: 2,
                n_locals: 2,
                blocks: vec![Block {
                    id: BlockId(0),
                    insts: vec![
                        Inst {
                            result: Value(0),
                            kind: InstKind::LoadLocal(0),
                        },
                        Inst {
                            result: Value(1),
                            kind: InstKind::LoadLocal(1),
                        },
                        Inst {
                            result: Value(2),
                            kind: InstKind::BinaryOp {
                                op: BinOp::Add,
                                lhs: Value(0),
                                rhs: Value(1),
                            },
                        },
                    ],
                    term: Terminator::Return(Value(2)),
                }],
            }],
            main: FunctionId(0),
            names: vec![],
        };

        let clif = compiled_clif(&ir, 0);

        assert!(clif.contains("pon_binary_add"));
        assert!(clif.contains("brif"));
    }

    #[test]
    fn main_function_clif_lowers_make_function_and_global_store_with_null_checks() {
        let ir = IrModule {
            functions: vec![
                Function {
                    name: "__main__".to_owned(),
                    arity: 0,
                    n_locals: 0,
                    blocks: vec![Block {
                        id: BlockId(0),
                        insts: vec![
                            Inst {
                                result: Value(0),
                                kind: InstKind::MakeFunction {
                                    func_index: 1,
                                    name_interned: 0,
                                    arity: 2,
                                },
                            },
                            Inst {
                                result: Value(1),
                                kind: InstKind::StoreGlobal(0, Value(0)),
                            },
                            Inst {
                                result: Value(2),
                                kind: InstKind::Const(PyConst::None),
                            },
                        ],
                        term: Terminator::Return(Value(2)),
                    }],
                },
                Function {
                    name: "add".to_owned(),
                    arity: 2,
                    n_locals: 2,
                    blocks: vec![Block {
                        id: BlockId(0),
                        insts: vec![
                            Inst {
                                result: Value(0),
                                kind: InstKind::LoadLocal(0),
                            },
                            Inst {
                                result: Value(1),
                                kind: InstKind::LoadLocal(1),
                            },
                            Inst {
                                result: Value(2),
                                kind: InstKind::BinaryOp {
                                    op: BinOp::Add,
                                    lhs: Value(0),
                                    rhs: Value(1),
                                },
                            },
                        ],
                        term: Terminator::Return(Value(2)),
                    }],
                },
            ],
            main: FunctionId(0),
            names: vec!["add".to_owned()],
        };

        let clif = compiled_clif(&ir, 0);

        assert!(clif.contains("pon_make_function"));
        assert!(clif.contains("pon_store_global"));
        assert!(clif.matches("brif").count() >= 3);
    }
}
