//! Runtime helper import declaration for Phase-A Cranelift modules.

use cranelift_codegen::ir::{self, AbiParam};
use cranelift_module::{FuncId, Linkage, Module, ModuleResult};
use pon_runtime::abi::{AbiTy, HELPERS};

/// Cranelift module function ids for every Phase-A runtime helper import.
///
/// The ids are declared directly from [`pon_runtime::abi::HELPERS`], so symbol
/// spelling, parameter order, and return types stay locked to the runtime ABI.
/// Consumers pass these ids to baseline lowering, which then imports per-function
/// [`cranelift_codegen::ir::FuncRef`] handles with `Module::declare_func_in_func`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HelperRefs {
    /// `pon_const_int(i64) -> *mut PyObject`.
    pub const_int: FuncId,
    /// `pon_const_str(*const u8, usize) -> *mut PyObject`.
    pub const_str: FuncId,
    /// `pon_binary_add(*mut PyObject, *mut PyObject) -> *mut PyObject`.
    pub binary_add: FuncId,
    /// `pon_call(callee, argv, argc) -> *mut PyObject`.
    pub call: FuncId,
    /// `pon_load_global(name_id) -> *mut PyObject`.
    pub load_global: FuncId,
    /// `pon_print(value) -> *mut PyObject`.
    pub print: FuncId,
    /// `pon_make_function(code, arity, name_id) -> *mut PyObject`.
    pub make_function: FuncId,
    /// `pon_store_global(name_id, value) -> *mut PyObject`.
    pub store_global: FuncId,
    /// `pon_none() -> *mut PyObject`.
    pub none: FuncId,
    /// `pon_runtime_init() -> i32`.
    pub runtime_init: FuncId,
}

/// Declare all Phase-A runtime helpers as Cranelift module imports.
///
/// Signatures are built from [`pon_runtime::abi::HELPERS`] and [`AbiTy`]. Pointer
/// ABI types use `module.target_config().pointer_type()`, while unsigned integer
/// ABI types are represented by same-width Cranelift integer types because CLIF
/// integer values carry width, not signedness. Every import uses the exact
/// runtime `symbol` string and [`Linkage::Import`].
pub fn declare_helpers<M: Module>(module: &mut M) -> ModuleResult<HelperRefs> {
    Ok(HelperRefs {
        const_int: declare_one(module, "pon_const_int")?,
        const_str: declare_one(module, "pon_const_str")?,
        binary_add: declare_one(module, "pon_binary_add")?,
        call: declare_one(module, "pon_call")?,
        load_global: declare_one(module, "pon_load_global")?,
        print: declare_one(module, "pon_print")?,
        make_function: declare_one(module, "pon_make_function")?,
        store_global: declare_one(module, "pon_store_global")?,
        none: declare_one(module, "pon_none")?,
        runtime_init: declare_one(module, "pon_runtime_init")?,
    })
}

fn declare_one<M: Module>(module: &mut M, symbol: &str) -> ModuleResult<FuncId> {
    let decl = HELPERS
        .iter()
        .find(|decl| decl.symbol == symbol)
        .expect("pon-runtime HELPERS must contain every Phase-A helper symbol");
    let sig = helper_signature(module, decl.params, decl.ret);
    module.declare_function(decl.symbol, Linkage::Import, &sig)
}

fn helper_signature<M: Module>(module: &M, params: &[AbiTy], ret: AbiTy) -> ir::Signature {
    let mut sig = module.make_signature();
    sig.params
        .extend(params.iter().copied().map(|ty| AbiParam::new(abi_type(module, ty))));
    sig.returns.push(AbiParam::new(abi_type(module, ret)));
    sig
}

fn abi_type<M: Module>(module: &M, ty: AbiTy) -> ir::Type {
    let ptr = module.target_config().pointer_type();
    match ty {
        AbiTy::I32 | AbiTy::U32 => ir::types::I32,
        AbiTy::I64 => ir::types::I64,
        AbiTy::Usize | AbiTy::ConstU8Ptr | AbiTy::PyObjectPtr | AbiTy::PyObjectPtrPtr => ptr,
    }
}

