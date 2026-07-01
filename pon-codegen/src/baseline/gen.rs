//! Iterator, async-iterator, generator, and coroutine Phase-B lowering family.
//!
//! This module owns helper-symbol choices and the NULL-sentinel contract for the
//! generator-family instructions baseline can represent as single helper calls.

use cranelift_codegen::ir::{self, InstBuilder};
use cranelift_frontend::FunctionBuilder;
use pon_ir::ir::Value as IrValue;

use super::{CodegenError, LowerState, call_pyobject_helper};

/// Runtime helper used for async iterator acquisition.
#[allow(dead_code)]
pub(crate) const GET_AITER_HELPER: &str = "pon_get_aiter";
/// Runtime helper used for `for` iterator advance.
#[allow(dead_code)]
pub(crate) const FOR_NEXT_HELPER: &str = "pon_for_next";
/// Runtime helper used at a suspension yield value.
#[allow(dead_code)]
pub(crate) const YIELD_HELPER: &str = "pon_yield";
/// Runtime helper used for one delegated `yield from` step.
#[allow(dead_code)]
pub(crate) const YIELD_FROM_HELPER: &str = "pon_yield_from";
/// Runtime helper used to normalize an awaitable into an iterator.
#[allow(dead_code)]
pub(crate) const AWAIT_HELPER: &str = "pon_await";
/// Runtime helper used to allocate a heap frame.
#[allow(dead_code)]
pub(crate) const MAKE_FRAME_HELPER: &str = "pon_make_frame";
/// Runtime helper used to allocate a generator/coroutine object.
#[allow(dead_code)]
pub(crate) const MAKE_GENERATOR_HELPER: &str = "pon_make_generator";
/// Runtime helper used to resume a generator/coroutine.
#[allow(dead_code)]
pub(crate) const GEN_SEND_HELPER: &str = "pon_gen_send";

/// Generator object kind values accepted by `pon_make_generator`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum GeneratorKindAbi {
    Generator = 0,
    Coroutine = 1,
    AsyncGenerator = 2,
}

/// Reserved state value used by the frame ABI for exhausted generators.
#[allow(dead_code)]
pub(crate) const FRAME_STATE_EXHAUSTED: u32 = u32::MAX;

/// Records the codegen contract for a suspend terminator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) struct SuspendLoweringPlan {
    pub(crate) state: u32,
    pub(crate) helper: &'static str,
}

#[allow(dead_code)]
impl SuspendLoweringPlan {
    /// Builds the helper call plan for `Terminator::Suspend`.
    #[must_use]
    pub(crate) const fn yield_value(state: u32) -> Self {
        Self {
            state,
            helper: YIELD_HELPER,
        }
    }
}

/// Lower synchronous iterator acquisition through `pon_get_iter`.
pub(crate) fn lower_get_iter(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    iterable: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    lower_feedback_unary(builder, helper, state, iterable, ptr_ty, exception_exit)
}

/// Lower asynchronous iterator acquisition through `pon_get_aiter`.
pub(crate) fn lower_get_aiter(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    iterable: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    lower_feedback_unary(builder, helper, state, iterable, ptr_ty, exception_exit)
}

/// Lower iterator advance through `pon_for_next`.
///
/// `ForNext` is nullable by design: a NULL result may be either StopIteration or
/// a real iterator error.  The loop terminator consumes the raw value and asks
/// `pon_gen_stop_value` to distinguish those cases.
pub(crate) fn lower_for_next(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    iter: IrValue,
    _ptr_ty: ir::Type,
    _exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let iter = state.value(iter)?;
    let iter_ty = builder.func.dfg.value_type(iter);
    let feedback = builder.ins().iconst(iter_ty, 0);
    let call = builder.ins().call(helper, &[iter, feedback]);
    Ok(builder.func.dfg.inst_results(call)[0])
}

/// Lower generator yield through `pon_yield`.
pub(crate) fn lower_yield(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    val: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let val = state.value(val)?;
    Ok(call_pyobject_helper(builder, helper, &[val], ptr_ty, exception_exit))
}

/// Lower eager generator completion through `pon_eager_yield_generator`.
pub(crate) fn lower_eager_generator_return(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    value: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let value = state.value(value)?;
    Ok(call_pyobject_helper(builder, helper, &[value], ptr_ty, exception_exit))
}

/// Lower `yield from` through `pon_yield_from`.
pub(crate) fn lower_yield_from(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    iter: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    lower_feedback_unary(builder, helper, state, iter, ptr_ty, exception_exit)
}

/// Lower `await` through `pon_await`.
pub(crate) fn lower_await(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    awaitable: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    lower_feedback_unary(builder, helper, state, awaitable, ptr_ty, exception_exit)
}

fn lower_feedback_unary(
    builder: &mut FunctionBuilder<'_>,
    helper: ir::FuncRef,
    state: &LowerState,
    value: IrValue,
    ptr_ty: ir::Type,
    exception_exit: ir::Block,
) -> Result<ir::Value, CodegenError> {
    let value = state.value(value)?;
    let feedback = builder.ins().iconst(ptr_ty, 0);
    Ok(call_pyobject_helper(builder, helper, &[value, feedback], ptr_ty, exception_exit))
}
