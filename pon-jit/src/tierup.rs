//! Phase-D JIT tier-up driver.
//!
//! The runtime owns hotness counters and the function dispatch cell; the JIT owns
//! the concrete tier-1 code.  This module bridges the two with a process-wide
//! runtime hook that queues from `pon-runtime`, compiles a tier-1 body, installs
//! the entry through `PyFunction::entry`, and keeps the executable module alive
//! for as long as the owning [`TierUpDriver`] lives.

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

use cranelift_codegen::ir::AbiParam;
use cranelift_frontend::FunctionBuilderContext;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module as ClifModule, ModuleError, default_libcall_names};
use pon_codegen::baseline::{CodegenError, NameMap, compile_function as compile_baseline_function};
use pon_codegen::helpers::declare_helpers;
use pon_codegen::isa::{OptLevel, make_isa};
use pon_codegen::optimizing;
use pon_codegen::{ModuleAnnotations, OptimizingPlan, infer_module_types, lowering_steps, plan_function};
use pon_ir::ir::{Function, Module as IrModule};
use pon_runtime::abi::{HELPERS, TIER1_CALL_THRESHOLD, TIER1_LOOP_THRESHOLD, pon_tierup_set_hook};
use pon_runtime::feedback::{FeedbackVec, TypeTag};
use pon_runtime::object::{PyFunction, TIER_STATE_DISABLED, TIER_STATE_QUEUED, TIER_STATE_TIER0, TIER_STATE_TIER1, Tier1Code};

/// Function-entry hotness threshold mirrored from the runtime probe.
pub const CALL_THRESHOLD: u32 = TIER1_CALL_THRESHOLD;
/// Loop-backedge hotness threshold mirrored from the runtime probe.
pub const LOOP_THRESHOLD: u32 = TIER1_LOOP_THRESHOLD;

static ACTIVE_DRIVER: AtomicPtr<TierUpDriver> = AtomicPtr::new(ptr::null_mut());

/// Process-local owner for tier-up metadata and installed tier-1 modules.
pub struct TierUpDriver {
    modules: Vec<RegisteredModule>,
    functions: Vec<RegisteredFunction>,
    installed: Vec<Box<Tier1Compilation>>,
}

#[derive(Clone)]
struct RegisteredFunction {
    tier0_entry: *const u8,
    module_index: usize,
    function_index: usize,
    feedback_len: usize,
}

struct RegisteredModule {
    ir: IrModule,
}

#[allow(dead_code, reason = "tier-1 executable modules and plans are retained for dispatch and later precise-root consumers")]
struct Tier1Compilation {
    module: JITModule,
    entry: *const u8,
    function_index: usize,
    feedback_len: usize,
    plan: OptimizingPlan,
    lowering_steps: Vec<pon_codegen::LoweringStep>,
    feedback: Vec<Option<(TypeTag, TypeTag)>>,
}

#[allow(dead_code, reason = "tier-up failures are intentionally swallowed by the runtime hook after resetting the tier state")]
#[derive(Debug)]
enum TierUpCompileError {
    Codegen(CodegenError),
    Module(ModuleError),
    MissingFunction { function_index: usize },
}

impl From<CodegenError> for TierUpCompileError {
    fn from(error: CodegenError) -> Self {
        Self::Codegen(error)
    }
}

impl From<ModuleError> for TierUpCompileError {
    fn from(error: ModuleError) -> Self {
        Self::Module(error)
    }
}

impl TierUpDriver {
    /// Build an empty driver. Call [`register_runtime_hook`] once the boxed driver
    /// has a stable address.
    #[must_use]
    pub fn new() -> Self {
        Self {
            modules: Vec::new(),
            functions: Vec::new(),
            installed: Vec::new(),
        }
    }

    /// Record finalized tier-0 entrypoints for a just-compiled IR module.
    pub fn register_module(&mut self, ir_module: &IrModule, func_ids: &[FuncId], module: &JITModule) {
        let module_index = self.modules.len();
        self.modules.push(RegisteredModule { ir: ir_module.clone() });

        self.functions
            .extend(ir_module.functions.iter().enumerate().filter_map(|(function_index, function)| {
                let func_id = *func_ids.get(function_index)?;
                let tier0_entry = module.get_finalized_function(func_id);
                Some(RegisteredFunction {
                    tier0_entry,
                    module_index,
                    function_index,
                    feedback_len: feedback_len(function),
                })
            }));
    }

    unsafe fn compile_and_install(&mut self, function: *mut PyFunction) {
        if function.is_null() {
            return;
        }

        let function_ref = unsafe { &*function };
        if function_ref.tier_state.load(Ordering::Acquire) != TIER_STATE_QUEUED {
            return;
        }

        let Some(record) = self.find_record(function_ref.code).cloned() else {
            disable_tierup(function_ref);
            return;
        };

        unsafe { ensure_feedback(function_ref, record.feedback_len) };
        let feedback = unsafe { feedback_snapshot(function_ref, record.feedback_len) };

        let mut ir_module = self.modules[record.module_index].ir.clone();
        infer_module_types(&mut ir_module, &ModuleAnnotations::default());
        let Some(ir_function) = ir_module.functions.get(record.function_index) else {
            disable_tierup(function_ref);
            return;
        };
        let Some(plan) = plan_function(ir_function) else {
            disable_tierup(function_ref);
            return;
        };
        let steps = lowering_steps(&plan);
        match compile_tier1_module(&ir_module, record.function_index, record.feedback_len, feedback, plan, steps) {
            Ok(compilation) if compilation.entry != record.tier0_entry => self.install(function_ref, compilation),
            Ok(_) | Err(_) => disable_tierup(function_ref),
        }
    }

    fn find_record(&self, tier0_entry: *const u8) -> Option<&RegisteredFunction> {
        self.functions.iter().rev().find(|record| record.tier0_entry == tier0_entry)
    }

    fn install(&mut self, function: &PyFunction, compilation: Tier1Compilation) {
        let mut compilation = Box::new(compilation);
        let entry = compilation.entry;
        let handle = (&mut *compilation as *mut Tier1Compilation).cast::<c_void>();

        unsafe {
            *function.tier1.get() = Some(Tier1Code { entry, handle });
        }
        function.entry.store(entry.cast_mut(), Ordering::Release);
        function.tier_state.store(TIER_STATE_TIER1, Ordering::Release);
        self.installed.push(compilation);
    }
}

impl Default for TierUpDriver {
    fn default() -> Self {
        Self::new()
    }
}

/// Install this driver's runtime hook.
///
/// The driver should be heap allocated before registration so the pointer remains
/// stable across moves of the owning JIT engine.
pub fn register_runtime_hook(driver: &mut TierUpDriver) {
    ACTIVE_DRIVER.store(driver as *mut TierUpDriver, Ordering::Release);
    unsafe { pon_tierup_set_hook((tierup_hook as *const ()).cast_mut()) };
}

/// Clear the runtime hook if it still points at `driver`.
pub fn unregister_runtime_hook(driver: &TierUpDriver) {
    let expected = driver as *const TierUpDriver as *mut TierUpDriver;
    if ACTIVE_DRIVER
        .compare_exchange(expected, ptr::null_mut(), Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        unsafe { pon_tierup_set_hook(ptr::null_mut()) };
    }
}

unsafe extern "C" fn tierup_hook(function: *mut PyFunction) {
    let driver = ACTIVE_DRIVER.load(Ordering::Acquire);
    if driver.is_null() {
        if !function.is_null() {
            reset_to_tier0(unsafe { &*function });
        }
        return;
    }

    unsafe { (*driver).compile_and_install(function) };
}

fn compile_tier1_module(
    ir_module: &IrModule,
    function_index: usize,
    feedback_len: usize,
    feedback: Vec<Option<(TypeTag, TypeTag)>>,
    plan: OptimizingPlan,
    lowering_steps: Vec<pon_codegen::LoweringStep>,
) -> Result<Tier1Compilation, TierUpCompileError> {
    let mut module = make_tier1_module();
    let helpers = declare_helpers(&mut module)?;
    let func_ids = declare_tier1_functions(&mut module, ir_module)?;
    let names = NameMap::from_ir_module(ir_module);
    let mut ctx = module.make_context();
    let mut fctx = FunctionBuilderContext::new();
    for (index, function) in ir_module.functions.iter().enumerate() {
        if index == function_index {
            optimizing::compile_function(
                &mut module,
                &helpers,
                &func_ids,
                &names,
                function,
                &plan,
                &mut ctx,
                &mut fctx,
            )?;
        } else {
            compile_baseline_function(&mut module, &helpers, &func_ids, &names, function, &mut ctx, &mut fctx)?;
        }
        module.define_function(func_ids[index], &mut ctx)?;
    }
    module.finalize_definitions()?;
    let func_id = *func_ids
        .get(function_index)
        .ok_or(TierUpCompileError::MissingFunction { function_index })?;
    let entry = module.get_finalized_function(func_id);

    Ok(Tier1Compilation {
        module,
        entry,
        function_index,
        feedback_len,
        plan,
        feedback,
        lowering_steps,
    })
}

fn make_tier1_module() -> JITModule {
    let isa = make_isa(OptLevel::Speed, false);
    let mut builder = JITBuilder::with_isa(isa, default_libcall_names());
    for helper in HELPERS {
        builder.symbol(helper.symbol, helper.address.cast::<u8>());
    }
    crate::register_free_threading_symbols(&mut builder);
    JITModule::new(builder)
}

fn declare_tier1_functions(module: &mut JITModule, ir_module: &IrModule) -> Result<Vec<FuncId>, ModuleError> {
    let mut sig = module.make_signature();
    let ptr_ty = module.target_config().pointer_type();
    sig.params.push(AbiParam::new(ptr_ty));
    sig.params.push(AbiParam::new(ptr_ty));
    sig.returns.push(AbiParam::new(ptr_ty));

    ir_module
        .functions
        .iter()
        .enumerate()
        .map(|(index, _function)| module.declare_function(&format!("__pon_tier1_fn_{index}"), Linkage::Local, &sig))
        .collect()
}

fn feedback_len(function: &Function) -> usize {
    function
        .blocks
        .iter()
        .flat_map(|block| block.insts.iter())
        .filter_map(|inst| inst.feedback_slot)
        .map(|slot| slot.0 as usize + 1)
        .max()
        .unwrap_or(0)
}


unsafe fn feedback_snapshot(function: &PyFunction, len: usize) -> Vec<Option<(TypeTag, TypeTag)>> {
    if len == 0 {
        return Vec::new();
    }

    let feedback = unsafe { &*function.feedback.get() };
    let Some(feedback) = feedback.as_ref() else {
        return vec![None; len];
    };

    (0..len)
        .map(|index| feedback.get(index).and_then(|cell| cell.speculate()))
        .collect()
}

unsafe fn ensure_feedback(function: &PyFunction, len: usize) {
    if len == 0 {
        return;
    }

    let feedback = unsafe { &mut *function.feedback.get() };
    let needs_install = feedback.as_ref().map_or(true, |existing| existing.len() < len);
    if needs_install {
        *feedback = Some(FeedbackVec::new(len));
    }
}

fn reset_to_tier0(function: &PyFunction) {
    function.entry.store(function.code.cast_mut(), Ordering::Release);
    function.tier_state.store(TIER_STATE_TIER0, Ordering::Release);
}

fn disable_tierup(function: &PyFunction) {
    function.entry.store(function.code.cast_mut(), Ordering::Release);
    function.tier_state.store(TIER_STATE_DISABLED, Ordering::Release);
}


#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use pon_codegen::{ModuleAnnotations, infer_module_types, plan_function};
    use pon_ir::Type;
    use pon_ir::ir::{
        BinOp, Block, BlockId, Function, FunctionId, Inst, InstKind, LocalId, Module as IrModule,
        NameId, PyConst, Terminator, Value,
    };

    use super::*;

    #[test]
    fn tier_up_infers_cloned_ir_before_planning_range_loop() {
        let ir_module = arithmetic_range_loop_module();
        assert!(
            plan_function(&ir_module.functions[0]).is_none(),
            "raw tier-0 IR must not already carry enough metadata for an optimizing plan"
        );

        let mut inferred_module = ir_module.clone();
        infer_module_types(&mut inferred_module, &ModuleAnnotations::default());
        let inferred_plan = plan_function(&inferred_module.functions[0])
            .expect("fixture should plan once the existing inference pass seeds metadata");
        assert!(
            inferred_plan
                .fast_path
                .values
                .iter()
                .any(|value| value.value == Value(7) && value.ty == Type::IntI64),
            "inference should make the arithmetic body an IntI64 fast-path candidate: {inferred_plan:?}"
        );

        let tier0_entry = 1_usize as *const u8;
        let mut driver = TierUpDriver::new();
        driver.modules.push(RegisteredModule { ir: ir_module });
        driver.functions.push(RegisteredFunction {
            tier0_entry,
            module_index: 0,
            function_index: 0,
            feedback_len: 0,
        });

        let mut function = PyFunction::new(std::ptr::null(), tier0_entry, 0, 0);
        function.tier_state.store(TIER_STATE_QUEUED, Ordering::Release);

        unsafe { driver.compile_and_install(&mut function) };

        assert_eq!(
            inferred_type(&driver.modules[0].ir, Value(7)),
            Type::Bottom,
            "tier-up should infer a cloned module and leave the registered tier-0 IR raw"
        );
        assert_eq!(
            function.tier_state.load(Ordering::Acquire),
            TIER_STATE_TIER1,
            "tier-up should install tier-1 code when clone-local inference enables planning"
        );
        assert_ne!(
            function.tier_state.load(Ordering::Acquire),
            TIER_STATE_DISABLED,
            "successful tier-up must not leave the function permanently disabled"
        );
        let compilation = driver
            .installed
            .first()
            .expect("typed clone should produce an optimizing tier-1 compilation");
        assert!(
            compilation
                .plan
                .fast_path
                .values
                .iter()
                .any(|value| value.value == Value(7) && value.ty == Type::IntI64),
            "installed plan should come from inferred arithmetic metadata: {:?}",
            compilation.plan
        );
    }

    #[test]
    fn failed_tier_up_disables_future_queue_attempts() {
        let tier0_entry = 1_usize as *const u8;
        let unknown_entry = 2_usize as *const u8;
        let mut driver = TierUpDriver::new();
        driver.modules.push(RegisteredModule {
            ir: arithmetic_range_loop_module(),
        });
        driver.functions.push(RegisteredFunction {
            tier0_entry,
            module_index: 0,
            function_index: 0,
            feedback_len: 0,
        });

        let mut function = PyFunction::new(std::ptr::null(), unknown_entry, 0, 0);
        function.entry.store(unknown_entry.cast_mut(), Ordering::Release);
        function.tier_state.store(TIER_STATE_QUEUED, Ordering::Release);

        unsafe { driver.compile_and_install(&mut function) };

        assert_eq!(
            function.tier_state.load(Ordering::Acquire),
            TIER_STATE_DISABLED,
            "functions that cannot be mapped to tier-1 metadata should not be re-queued on every hot probe"
        );
        assert_eq!(
            function.entry.load(Ordering::Acquire).cast_const(),
            unknown_entry,
            "failed tier-up keeps dispatching through the tier-0 entry"
        );
    }

    fn arithmetic_range_loop_module() -> IrModule {
        IrModule {
            functions: vec![Function {
                name: "sum_range".to_owned(),
                arity: 0,
                n_locals: 1,
                blocks: vec![
                    Block {
                        id: BlockId(0),
                        insts: vec![
                            Inst::new(Value(0), InstKind::LoadBuiltin(NameId(0))),
                            Inst::new(Value(1), InstKind::Const(PyConst::Int(0))),
                            Inst::new(Value(2), InstKind::Const(PyConst::Int(8))),
                            Inst::new(
                                Value(3),
                                InstKind::Call {
                                    callee: Value(0),
                                    args: vec![Value(1), Value(2)],
                                },
                            ),
                            Inst::new(Value(4), InstKind::GetIter { iterable: Value(3) }),
                        ],
                        term: Terminator::Jump(BlockId(1)),
                    },
                    Block {
                        id: BlockId(1),
                        insts: vec![Inst::new(Value(5), InstKind::ForNext { iter: Value(4) })],
                        term: Terminator::ForLoop {
                            iter: Value(4),
                            body: BlockId(2),
                            done: BlockId(3),
                        },
                    },
                    Block {
                        id: BlockId(2),
                        insts: vec![
                            Inst::new(Value(6), InstKind::Const(PyConst::Int(1))),
                            Inst::new(
                                Value(7),
                                InstKind::BinaryOp {
                                    op: BinOp::Add,
                                    lhs: Value(5),
                                    rhs: Value(6),
                                },
                            ),
                            Inst::new(Value(8), InstKind::StoreLocal(LocalId(0), Value(7))),
                        ],
                        term: Terminator::Jump(BlockId(1)),
                    },
                    Block {
                        id: BlockId(3),
                        insts: vec![Inst::new(Value(9), InstKind::LoadLocal(LocalId(0)))],
                        term: Terminator::Return(Value(9)),
                    },
                ],
            }],
            main: FunctionId(0),
            names: vec!["range".to_owned()],
        }
    }

    fn inferred_type(module: &IrModule, value: Value) -> Type {
        module
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.insts.iter())
            .find_map(|inst| (inst.result == value).then_some(inst.inferred_type))
            .expect("fixture value should exist")
    }
}