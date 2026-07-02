//! Call, function, and class-construction Phase-B lowering family.

use cranelift_codegen::ir::{self, InstBuilder, StackSlotData, StackSlotKind};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{DataDescription, FuncId, Module};
use pon_ir::ir::{CellId, Function, FunctionId, NameId, Value as IrValue};

use super::{
    CodegenError, HelperFuncRefs, LowerState, NameMap, PyObjectArray, build_call_argv,
    call_pyobject_helper, call_pyobject_helper_consuming, offset_i32,
};

const CODE_INFO_ENTRY_OFFSET: usize = core::mem::offset_of!(pon_runtime::abi::CodeInfo, entry);
const CODE_INFO_PARAMS_OFFSET: usize = core::mem::offset_of!(pon_runtime::abi::CodeInfo, params);
const CODE_INFO_NAME_OFFSET: usize = core::mem::offset_of!(pon_runtime::abi::CodeInfo, name_interned);
const CODE_INFO_N_LOCALS_OFFSET: usize = core::mem::offset_of!(pon_runtime::abi::CodeInfo, n_locals);
const CODE_INFO_N_FEEDBACK_OFFSET: usize = core::mem::offset_of!(pon_runtime::abi::CodeInfo, n_feedback);
const CODE_INFO_FLAGS_OFFSET: usize = core::mem::offset_of!(pon_runtime::abi::CodeInfo, flags);
const PARAM_SPEC_NAMES_OFFSET: usize = core::mem::offset_of!(pon_runtime::abi::ParamSpec, names);
const PARAM_SPEC_TOTAL_OFFSET: usize = core::mem::offset_of!(pon_runtime::abi::ParamSpec, total_param_count);
const PARAM_SPEC_POSONLY_OFFSET: usize = core::mem::offset_of!(pon_runtime::abi::ParamSpec, positional_only_count);
const PARAM_SPEC_POS_OFFSET: usize = core::mem::offset_of!(pon_runtime::abi::ParamSpec, positional_count);
const PARAM_SPEC_KWONLY_OFFSET: usize = core::mem::offset_of!(pon_runtime::abi::ParamSpec, keyword_only_count);
const PARAM_SPEC_VARARGS_OFFSET: usize = core::mem::offset_of!(pon_runtime::abi::ParamSpec, varargs_name);
const PARAM_SPEC_VARKW_OFFSET: usize = core::mem::offset_of!(pon_runtime::abi::ParamSpec, varkw_name);

/// Lower a Phase-A positional Python call through `pon_call`.
pub(crate) fn lower_call(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    state: &LowerState,
    callee: IrValue,
    args: &[IrValue],
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let callee = state.value(callee)?;
    let argv = build_call_argv(builder, helpers, state, args, ptr_ty, ptr_bytes)?;
    let argc = builder.ins().iconst(ptr_ty, args.len() as i64);
    call_pyobject_helper_consuming(
        builder,
        helpers.call,
        &[callee, argv.addr, argc],
        &[&argv],
        ptr_ty,
        ptr_bytes,
        exception_exit,
    )
}

/// Lower a Phase-A function object construction through `pon_make_function`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_make_function<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    func_ids: &[FuncId],
    names: &NameMap,
    func_index: u32,
    name_interned: u32,
    arity: usize,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let func_id = *func_ids
        .get(func_index as usize)
        .ok_or(CodegenError::FunctionIndexOutOfRange { func_index })?;
    let func_ref = module.declare_func_in_func(func_id, builder.func);
    let code = builder.ins().func_addr(ptr_ty, func_ref);
    let arity = builder.ins().iconst(ptr_ty, arity as i64);
    let runtime_name = builder.ins().iconst(ir::types::I32, i64::from(names.runtime_id(name_interned)?));
    Ok(call_pyobject_helper(
        builder,
        helpers.make_function,
        &[code, arity, runtime_name],
        ptr_ty,
        exception_exit,
    ))
}

/// Extended-call payload accepted by the central `CallEx` dispatch arm.
pub(crate) struct CallExArgs<'a> {
    pub(crate) callee: IrValue,
    pub(crate) args: &'a [IrValue],
    pub(crate) star: Option<IrValue>,
    pub(crate) kwargs: &'a [(NameId, IrValue)],
    pub(crate) dstar: Option<IrValue>,
}

/// Lower a Phase-B `CallEx` through `pon_call_ex`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_call_ex(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    names: &NameMap,
    state: &LowerState,
    call: CallExArgs<'_>,
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
    feedback_cell: Option<ir::Value>,
) -> Result<ir::Value, CodegenError> {
    let callee = state.value(call.callee)?;
    let argv = build_call_argv(builder, helpers, state, call.args, ptr_ty, ptr_bytes)?;
    let argc = builder.ins().iconst(ptr_ty, call.args.len() as i64);
    let star = optional_value(builder, state, call.star, ptr_ty)?;
    let kw_names = build_kw_name_array(builder, names, call.kwargs, ptr_ty)?;
    let kw_values = build_kw_value_array(builder, state, call.kwargs, ptr_ty, ptr_bytes)?;
    let kw_count = builder.ins().iconst(ptr_ty, call.kwargs.len() as i64);
    let dstar = optional_value(builder, state, call.dstar, ptr_ty)?;
    let feedback = feedback_cell.unwrap_or_else(|| builder.ins().iconst(ptr_ty, 0));
    call_pyobject_helper_consuming(
        builder,
        helpers.call_ex,
        &[callee, argv.addr, argc, star, kw_names, kw_values.addr, kw_count, dstar, feedback],
        &[&argv, &kw_values],
        ptr_ty,
        ptr_bytes,
        exception_exit,
    )
}

/// Method-call payload accepted by the central `CallMethod` dispatch arm.
pub(crate) struct CallMethodArgs<'a> {
    pub(crate) recv_pair: IrValue,
    pub(crate) args: &'a [IrValue],
}

/// Lower method calls through `pon_call_method`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_call_method(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    state: &LowerState,
    call: CallMethodArgs<'_>,
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
    feedback_cell: Option<ir::Value>,
) -> Result<ir::Value, CodegenError> {
    let recv_pair = state.value(call.recv_pair)?;
    let argv = build_call_argv(builder, helpers, state, call.args, ptr_ty, ptr_bytes)?;
    let argc = builder.ins().iconst(ptr_ty, call.args.len() as i64);
    let feedback = feedback_cell.unwrap_or_else(|| builder.ins().iconst(ptr_ty, 0));
    call_pyobject_helper_consuming(
        builder,
        helpers.call_method,
        &[recv_pair, argv.addr, argc, feedback],
        &[&argv],
        ptr_ty,
        ptr_bytes,
        exception_exit,
    )
}

pub(crate) struct MakeFunctionFullArgs<'a> {
    pub(crate) code: FunctionId,
    pub(crate) defaults: &'a [IrValue],
    pub(crate) kwdefaults: &'a [(NameId, IrValue)],
    pub(crate) closure: &'a [CellId],
    pub(crate) annotations: &'a [(NameId, IrValue)],
}

fn optional_value(
    builder: &mut FunctionBuilder<'_>,
    state: &LowerState,
    value: Option<IrValue>,
    ptr_ty: ir::Type,
) -> Result<ir::Value, CodegenError> {
    match value {
        Some(value) => state.value(value),
        None => Ok(builder.ins().iconst(ptr_ty, 0)),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_build_class<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    func_ids: &[FuncId],
    functions: &[Function],
    names: &NameMap,
    state: &LowerState,
    body: pon_ir::ir::FunctionId,
    name: NameId,
    bases: &[IrValue],
    keywords: &[(NameId, IrValue)],
    decorators: &[IrValue],
    closure: &[CellId],
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let body = if closure.is_empty() {
        lower_make_function(
            module,
            builder,
            helpers,
            func_ids,
            names,
            body.0,
            name.0,
            0,
            ptr_ty,
            exception_exit,
        )?
    } else {
        // The class body function captures enclosing-function cells exactly
        // like a nested `def`: build it through the Phase-B full constructor
        // (closure storage needs the metadata record) so `__build_class__`
        // runs the body with the closure tuple attached.
        lower_make_function_full(
            module,
            builder,
            helpers,
            func_ids,
            functions,
            names,
            state,
            MakeFunctionFullArgs {
                code: body,
                defaults: &[],
                kwdefaults: &[],
                closure,
                annotations: &[],
            },
            ptr_ty,
            ptr_bytes,
            exception_exit,
        )?
    };
    let bases_ptr = build_call_argv(builder, helpers, state, bases, ptr_ty, ptr_bytes)?;
    let base_count = builder.ins().iconst(ptr_ty, bases.len() as i64);
    let kw_names = build_kw_name_array(builder, names, keywords, ptr_ty)?;
    let kw_values = build_kw_value_array(builder, state, keywords, ptr_ty, ptr_bytes)?;
    let kw_count = builder.ins().iconst(ptr_ty, keywords.len() as i64);
    let runtime_name = builder.ins().iconst(ir::types::I32, i64::from(names.runtime_id(name.0)?));
    let mut class_value = call_pyobject_helper_consuming(
        builder,
        helpers.build_class,
        &[body, runtime_name, bases_ptr.addr, base_count, kw_names, kw_values.addr, kw_count],
        &[&bases_ptr, &kw_values],
        ptr_ty,
        ptr_bytes,
        exception_exit,
    )?;
    for decorator in decorators.iter().rev().copied() {
        let decorator = state.value(decorator)?;
        let slot = builder.create_sized_stack_slot(StackSlotData {
            kind: StackSlotKind::ExplicitSlot,
            size: ptr_bytes.try_into().map_err(|_| CodegenError::OffsetTooLarge { offset: ptr_bytes })?,
            align_shift: ptr_bytes.trailing_zeros() as u8,
            key: None,
        });
        builder.ins().stack_store(class_value, slot, 0);
        let argv = PyObjectArray::slot(builder.ins().stack_addr(ptr_ty, slot, 0), slot, ptr_bytes);
        let argc = builder.ins().iconst(ptr_ty, 1);
        class_value = call_pyobject_helper_consuming(
            builder,
            helpers.call,
            &[decorator, argv.addr, argc],
            &[&argv],
            ptr_ty,
            ptr_bytes,
            exception_exit,
        )?;
    }
    Ok(class_value)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_make_function_full<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    func_ids: &[FuncId],
    functions: &[Function],
    names: &NameMap,
    state: &LowerState,
    function: MakeFunctionFullArgs<'_>,
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let func_index = function.code.0;
    let target = functions
        .get(func_index as usize)
        .ok_or(CodegenError::FunctionIndexOutOfRange { func_index })?;
    let func_id = *func_ids
        .get(func_index as usize)
        .ok_or(CodegenError::FunctionIndexOutOfRange { func_index })?;
    let code_info = declare_code_info(module, builder, func_id, target, function.kwdefaults, names, ptr_ty, ptr_bytes)?;
    let defaults = build_value_array(builder, helpers, state, function.defaults, ptr_ty, ptr_bytes)?;
    let default_count = builder.ins().iconst(ptr_ty, function.defaults.len() as i64);
    let kwdefault_names = build_kw_name_array(builder, names, function.kwdefaults, ptr_ty)?;
    let kwdefaults = build_kw_value_array(builder, state, function.kwdefaults, ptr_ty, ptr_bytes)?;
    let kwdefault_count = builder.ins().iconst(ptr_ty, function.kwdefaults.len() as i64);
    let annotation_names = build_kw_name_array(builder, names, function.annotations, ptr_ty)?;
    let annotations = build_kw_value_array(builder, state, function.annotations, ptr_ty, ptr_bytes)?;
    let annotation_count = builder.ins().iconst(ptr_ty, function.annotations.len() as i64);
    let object = call_pyobject_helper_consuming(
        builder,
        helpers.make_function_full,
        &[
            code_info,
            defaults.addr,
            default_count,
            kwdefault_names,
            kwdefaults.addr,
            kwdefault_count,
            annotation_names,
            annotations.addr,
            annotation_count,
        ],
        &[&defaults, &kwdefaults, &annotations],
        ptr_ty,
        ptr_bytes,
        exception_exit,
    )?;
    if function.closure.is_empty() {
        return Ok(object);
    }
    let closure = build_closure_array(builder, helpers, state, function.closure, ptr_ty, ptr_bytes, exception_exit)?;
    let closure_count = builder.ins().iconst(ptr_ty, function.closure.len() as i64);
    call_pyobject_helper_consuming(
        builder,
        helpers.function_set_closure,
        &[object, closure.addr, closure_count],
        &[&closure],
        ptr_ty,
        ptr_bytes,
        exception_exit,
    )
}

fn declare_code_info<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder<'_>,
    func_id: FuncId,
    target: &Function,
    _kwdefaults: &[(NameId, IrValue)],
    _names: &NameMap,
    ptr_ty: ir::Type,
    ptr_bytes: usize,
) -> Result<ir::Value, CodegenError> {
    let params = &target.params;
    let total_param_count = params.total_slot_count();
    let names_data = declare_param_names_data(module, &params.names)?;
    let params_data = declare_param_spec_data(module, names_data, params, total_param_count, ptr_bytes)?;

    let data_id = module.declare_anonymous_data(false, false)?;
    let mut data = DataDescription::new();
    data.set_align(ptr_bytes as u64);
    let mut bytes = vec![0_u8; core::mem::size_of::<pon_runtime::abi::CodeInfo>()];
    put_u32(&mut bytes, CODE_INFO_NAME_OFFSET, pon_runtime::intern::intern(&target.name));
    put_u32(&mut bytes, CODE_INFO_N_LOCALS_OFFSET, u32::try_from(target.n_locals).map_err(|_| CodegenError::OffsetTooLarge { offset: target.n_locals })?);
    put_u32(&mut bytes, CODE_INFO_N_FEEDBACK_OFFSET, 0);
    put_u32(&mut bytes, CODE_INFO_FLAGS_OFFSET, code_flags(target));
    data.define(bytes.into_boxed_slice());
    let func_ref = module.declare_func_in_data(func_id, &mut data);
    data.write_function_addr(offset_u32(CODE_INFO_ENTRY_OFFSET)?, func_ref);
    if let Some(params_data) = params_data {
        let data_ref = module.declare_data_in_data(params_data, &mut data);
        data.write_data_addr(offset_u32(CODE_INFO_PARAMS_OFFSET)?, data_ref, 0);
    }
    module.define_data(data_id, &data)?;
    let global = module.declare_data_in_func(data_id, builder.func);
    Ok(builder.ins().global_value(ptr_ty, global))
}

fn code_flags(target: &Function) -> u32 {
    // The state-machine transform consumes every Yield/YieldFrom marker, so
    // the IR flags are the only source of truth for generator-ness.
    if target.is_coroutine {
        return pon_runtime::abi::call::CODE_FLAG_COROUTINE;
    }
    if target.is_generator {
        pon_runtime::abi::call::CODE_FLAG_GENERATOR
    } else {
        0
    }
}

fn declare_param_names_data<M: Module>(
    module: &mut M,
    param_names: &[String],
) -> Result<Option<cranelift_module::DataId>, CodegenError> {
    if param_names.is_empty() {
        return Ok(None);
    }
    let size = param_names
        .len()
        .checked_mul(core::mem::size_of::<u32>())
        .ok_or(CodegenError::OffsetTooLarge { offset: usize::MAX })?;
    let mut bytes = vec![0_u8; size];
    for (index, name) in param_names.iter().enumerate() {
        put_u32(&mut bytes, index * 4, pon_runtime::intern::intern(name));
    }
    let data_id = module.declare_anonymous_data(false, false)?;
    let mut data = DataDescription::new();
    data.set_align(core::mem::align_of::<u32>() as u64);
    data.define(bytes.into_boxed_slice());
    module.define_data(data_id, &data)?;
    Ok(Some(data_id))
}

fn declare_param_spec_data<M: Module>(
    module: &mut M,
    names_data: Option<cranelift_module::DataId>,
    params: &pon_ir::ir::ParamLayout,
    total_param_count: usize,
    ptr_bytes: usize,
) -> Result<Option<cranelift_module::DataId>, CodegenError> {
    if total_param_count == 0 {
        return Ok(None);
    }
    let data_id = module.declare_anonymous_data(false, false)?;
    let mut data = DataDescription::new();
    data.set_align(ptr_bytes as u64);
    let mut bytes = vec![0_u8; core::mem::size_of::<pon_runtime::abi::ParamSpec>()];
    put_u32(&mut bytes, PARAM_SPEC_TOTAL_OFFSET, u32::try_from(total_param_count).map_err(|_| CodegenError::OffsetTooLarge { offset: total_param_count })?);
    put_u32(&mut bytes, PARAM_SPEC_POSONLY_OFFSET, u32::try_from(params.positional_only_count).map_err(|_| CodegenError::OffsetTooLarge { offset: params.positional_only_count })?);
    put_u32(&mut bytes, PARAM_SPEC_POS_OFFSET, u32::try_from(params.positional_count).map_err(|_| CodegenError::OffsetTooLarge { offset: params.positional_count })?);
    put_u32(&mut bytes, PARAM_SPEC_KWONLY_OFFSET, u32::try_from(params.keyword_only_count).map_err(|_| CodegenError::OffsetTooLarge { offset: params.keyword_only_count })?);
    put_u32(&mut bytes, PARAM_SPEC_VARARGS_OFFSET, params.vararg_name.as_deref().map_or(0, pon_runtime::intern::intern));
    put_u32(&mut bytes, PARAM_SPEC_VARKW_OFFSET, params.kwarg_name.as_deref().map_or(0, pon_runtime::intern::intern));
    data.define(bytes.into_boxed_slice());
    if let Some(names_data) = names_data {
        let data_ref = module.declare_data_in_data(names_data, &mut data);
        data.write_data_addr(offset_u32(PARAM_SPEC_NAMES_OFFSET)?, data_ref, 0);
    }
    module.define_data(data_id, &data)?;
    Ok(Some(data_id))
}

fn offset_u32(offset: usize) -> Result<u32, CodegenError> {
    u32::try_from(offset).map_err(|_| CodegenError::OffsetTooLarge { offset })
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn build_value_array(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    state: &LowerState,
    values: &[IrValue],
    ptr_ty: ir::Type,
    ptr_bytes: usize,
) -> Result<PyObjectArray, CodegenError> {
    build_call_argv(builder, helpers, state, values, ptr_ty, ptr_bytes)
}

fn build_closure_array(
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    state: &LowerState,
    closure: &[CellId],
    ptr_ty: ir::Type,
    ptr_bytes: usize,
    exception_exit: ir::Block,
) -> Result<PyObjectArray, CodegenError> {
    if closure.is_empty() {
        return Ok(PyObjectArray::empty(builder, ptr_ty));
    }
    let size = closure
        .len()
        .checked_mul(ptr_bytes)
        .ok_or(CodegenError::OffsetTooLarge { offset: usize::MAX })?;
    let slot = builder.create_sized_stack_slot(StackSlotData {
        kind: StackSlotKind::ExplicitSlot,
        size: size.try_into().map_err(|_| CodegenError::OffsetTooLarge { offset: size })?,
        align_shift: ptr_bytes.trailing_zeros() as u8,
        key: None,
    });
    for (index, cell) in closure.iter().enumerate() {
        let value = super::name::cell_object(builder, helpers, state, cell.0, ptr_ty, exception_exit)?;
        builder.ins().stack_store(value, slot, offset_i32(index * ptr_bytes)?);
    }
    let addr = builder.ins().stack_addr(ptr_ty, slot, 0);
    Ok(PyObjectArray::slot(addr, slot, size))
}

fn build_kw_name_array(
    builder: &mut FunctionBuilder<'_>,
    names: &NameMap,
    kwargs: &[(NameId, IrValue)],
    ptr_ty: ir::Type,
) -> Result<ir::Value, CodegenError> {
    if kwargs.is_empty() {
        return Ok(builder.ins().iconst(ptr_ty, 0));
    }
    let size = kwargs
        .len()
        .checked_mul(4)
        .ok_or(CodegenError::OffsetTooLarge { offset: usize::MAX })?;
    let slot = builder.create_sized_stack_slot(StackSlotData {
        kind: StackSlotKind::ExplicitSlot,
        size: size.try_into().map_err(|_| CodegenError::OffsetTooLarge { offset: size })?,
        align_shift: 2,
        key: None,
    });
    for (index, (name, _)) in kwargs.iter().enumerate() {
        let runtime_name = builder.ins().iconst(ir::types::I32, i64::from(names.runtime_id(name.0)?));
        builder.ins().stack_store(runtime_name, slot, offset_i32(index * 4)?);
    }
    Ok(builder.ins().stack_addr(ptr_ty, slot, 0))
}

fn build_kw_value_array(
    builder: &mut FunctionBuilder<'_>,
    state: &LowerState,
    kwargs: &[(NameId, IrValue)],
    ptr_ty: ir::Type,
    ptr_bytes: usize,
) -> Result<PyObjectArray, CodegenError> {
    if kwargs.is_empty() {
        return Ok(PyObjectArray::empty(builder, ptr_ty));
    }
    let size = kwargs
        .len()
        .checked_mul(ptr_bytes)
        .ok_or(CodegenError::OffsetTooLarge { offset: usize::MAX })?;
    let slot = builder.create_sized_stack_slot(StackSlotData {
        kind: StackSlotKind::ExplicitSlot,
        size: size.try_into().map_err(|_| CodegenError::OffsetTooLarge { offset: size })?,
        align_shift: ptr_bytes.trailing_zeros() as u8,
        key: None,
    });
    for (index, (_, value)) in kwargs.iter().enumerate() {
        let value = state.value(*value)?;
        builder.ins().stack_store(value, slot, offset_i32(index * ptr_bytes)?);
    }
    let addr = builder.ins().stack_addr(ptr_ty, slot, 0);
    Ok(PyObjectArray::slot(addr, slot, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_info_layout_offsets_match_runtime_abi() {
        assert_eq!(CODE_INFO_ENTRY_OFFSET, 0);
        assert_eq!(CODE_INFO_PARAMS_OFFSET, core::mem::size_of::<*const u8>());
        assert_eq!(core::mem::size_of::<pon_runtime::abi::CodeInfo>(), core::mem::size_of::<*const u8>() * 2 + 16);
        assert_eq!(PARAM_SPEC_NAMES_OFFSET, 0);
        assert_eq!(PARAM_SPEC_TOTAL_OFFSET, core::mem::size_of::<*const u32>());
    }
}
