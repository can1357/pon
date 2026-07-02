//! String and template-string Phase-B lowering family.

use cranelift_codegen::ir::{self, InstBuilder, StackSlotData, StackSlotKind};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Module;
use pon_ir::ir::{FStrPart, TStrPart};

use super::{CodegenError, HelperFuncRefs, LowerState, call_pyobject_helper, declare_string_data, offset_i32};

const RAW_PART_BYTES: usize = 40;
const RAW_PART_ALIGN_SHIFT: u8 = 3;

/// Lower a Phase-A UTF-8 string literal through the string-part helper so the
/// runtime installs the representative `str` attribute slots before method
/// access can observe the object.
pub(crate) fn lower_const_str<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder<'_>,
    helpers: &HelperFuncRefs,
    value: &str,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let data_ptr = declare_string_data(module, builder, value, ptr_ty)?;
    let Some(slot) = raw_part_slot(builder, 1)? else {
        return Ok(builder.ins().iconst(ptr_ty, 0));
    };
    let null = builder.ins().iconst(ptr_ty, 0);
    stack_store(builder, null, slot, 0)?;
    stack_store(builder, data_ptr, slot, 8)?;
    let len = builder.ins().iconst(ptr_ty, value.len() as i64);
    stack_store(builder, len, slot, 16)?;
    let header = builder.ins().iconst(ir::types::I64, 0);
    stack_store(builder, header, slot, 24)?;
    stack_store(builder, null, slot, 32)?;
    let parts_ptr = builder.ins().stack_addr(ptr_ty, slot, 0);
    let count = builder.ins().iconst(ptr_ty, 1);
    Ok(call_pyobject_helper(builder, helpers.build_string, &[parts_ptr, count], ptr_ty, exception_exit))
}

/// Lower f-string interpolation parts through `pon_build_string`.
pub(crate) fn lower_build_string<M: Module>(
    _module: &mut M,
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    parts: &[FStrPart],
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let parts_ptr = build_fstring_parts(builder, state, parts, ptr_ty)?;
    let count = builder.ins().iconst(ptr_ty, parts.len() as i64);
    Ok(call_pyobject_helper(builder, helper, &[parts_ptr, count], ptr_ty, exception_exit))
}

/// Lower template-string interpolation parts through `pon_build_template`.
pub(crate) fn lower_build_template<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    parts: &[TStrPart],
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let parts_ptr = build_template_parts(module, builder, state, parts, ptr_ty)?;
    let count = builder.ins().iconst(ptr_ty, parts.len() as i64);
    Ok(call_pyobject_helper(builder, helper, &[parts_ptr, count], ptr_ty, exception_exit))
}

fn build_fstring_parts(
    builder: &mut FunctionBuilder<'_>,
    state: &LowerState,
    parts: &[FStrPart],
    ptr_ty: ir::Type,
) -> Result<ir::Value, CodegenError> {
    let Some(slot) = raw_part_slot(builder, parts.len())? else {
        return Ok(builder.ins().iconst(ptr_ty, 0));
    };
    let null = builder.ins().iconst(ptr_ty, 0);
    for (index, part) in parts.iter().enumerate() {
        let base = index * RAW_PART_BYTES;
        match part {
            FStrPart::Literal(_) => return Err(CodegenError::Unsupported("BuildString literal ConstId data")),
            FStrPart::Interp {
                value,
                conversion,
                format_spec,
            } => {
                let value = state.value(*value)?;
                let format_spec = match format_spec {
                    Some(format_spec) => state.value(*format_spec)?,
                    None => null,
                };
                stack_store(builder, value, slot, base)?;
                stack_store(builder, null, slot, base + 8)?;
                stack_store(builder, null, slot, base + 16)?;
                let header = builder.ins().iconst(ir::types::I64, i64::from(*conversion));
                stack_store(builder, header, slot, base + 24)?;
                stack_store(builder, format_spec, slot, base + 32)?;
            }
        }
    }
    Ok(builder.ins().stack_addr(ptr_ty, slot, 0))
}

fn build_template_parts<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder<'_>,
    state: &LowerState,
    parts: &[TStrPart],
    ptr_ty: ir::Type,
) -> Result<ir::Value, CodegenError> {
    let Some(slot) = raw_part_slot(builder, parts.len())? else {
        return Ok(builder.ins().iconst(ptr_ty, 0));
    };
    let null = builder.ins().iconst(ptr_ty, 0);
    for (index, part) in parts.iter().enumerate() {
        let base = index * RAW_PART_BYTES;
        match part {
            TStrPart::Literal(_) => return Err(CodegenError::Unsupported("BuildTemplate literal ConstId data")),
            TStrPart::Interp {
                value,
                expression,
                conversion,
                format_spec,
            } => {
                let value = state.value(*value)?;
                let format_spec = match format_spec {
                    Some(format_spec) => state.value(*format_spec)?,
                    None => null,
                };
                let expression_ptr = if expression.is_empty() {
                    null
                } else {
                    declare_string_data(module, builder, expression, ptr_ty)?
                };
                let expression_len = builder.ins().iconst(ptr_ty, expression.len() as i64);
                stack_store(builder, value, slot, base)?;
                stack_store(builder, expression_ptr, slot, base + 8)?;
                stack_store(builder, expression_len, slot, base + 16)?;
                let header = builder.ins().iconst(ir::types::I64, i64::from(*conversion) << 32);
                stack_store(builder, header, slot, base + 24)?;
                stack_store(builder, format_spec, slot, base + 32)?;
            }
        }
    }
    Ok(builder.ins().stack_addr(ptr_ty, slot, 0))
}

fn raw_part_slot(
    builder: &mut FunctionBuilder<'_>,
    count: usize,
) -> Result<Option<ir::StackSlot>, CodegenError> {
    if count == 0 {
        return Ok(None);
    }
    let size = count
        .checked_mul(RAW_PART_BYTES)
        .ok_or(CodegenError::OffsetTooLarge { offset: usize::MAX })?;
    Ok(Some(builder.create_sized_stack_slot(StackSlotData {
        kind: StackSlotKind::ExplicitSlot,
        size: size.try_into().map_err(|_| CodegenError::OffsetTooLarge { offset: size })?,
        align_shift: RAW_PART_ALIGN_SHIFT,
        key: None,
    })))
}

fn stack_store(
    builder: &mut FunctionBuilder<'_>,
    value: ir::Value,
    slot: ir::StackSlot,
    offset: usize,
) -> Result<(), CodegenError> {
    builder.ins().stack_store(value, slot, offset_i32(offset)?);
    Ok(())
}
