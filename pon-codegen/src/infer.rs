//! Local type inference for Phase-D typed AoT seeding.
//!
//! This is intentionally a metadata pass: it mutates only `Inst::inferred_type`
//! and `Inst::static_type`. The executable CLIF path still goes through boxed
//! baseline lowering unless a future optimizing lowerer consumes the metadata.

use std::collections::{HashMap, HashSet};

use pon_ir::{BinOp, InstKind, LocalId, Module, PyConst, Terminator, Type, UnOp, Value};

use crate::annotations::{AnnotationSource, FunctionAnnotations, ModuleAnnotations};

/// Run local annotation seeding and type inference over every function in `module`.
pub fn infer_module_types(module: &mut Module, annotations: &ModuleAnnotations) {
    let names = module.names.clone();
    for (index, function) in module.functions.iter_mut().enumerate() {
        let function_annotations = annotations.function(index).filter(|ann| ann.name == function.name);
        infer_function_types(function, &names, function_annotations);
    }
}

fn infer_function_types(
    function: &mut pon_ir::Function,
    names: &[String],
    annotations: Option<&FunctionAnnotations>,
) {
    let mut locals = vec![Type::Bottom; function.n_locals];
    seed_locals(&mut locals, annotations);
    seed_speculative_parameters(&mut locals, function.arity);

    let mut values = HashMap::<Value, Type>::new();
    let mut value_names = HashMap::<Value, String>::new();
    let mut range_values = HashSet::<Value>::new();
    let mut range_iters = HashSet::<Value>::new();
    let mut return_values = Vec::new();

    for block in &mut function.blocks {
        for inst in &mut block.insts {
            let inferred = infer_inst(
                inst.result,
                &inst.kind,
                names,
                &mut locals,
                &values,
                &mut value_names,
                &mut range_values,
                &mut range_iters,
            );
            write_inferred_type(inst, inferred);
            values.insert(inst.result, inst.inferred_type);
        }
        collect_return_value(&block.term, &mut return_values);
    }

    if let Some(return_type) = annotations.and_then(|ann| ann.return_type) {
        for value in return_values {
            write_value_type(function, value, return_type);
        }
    }
}

fn seed_locals(locals: &mut [Type], annotations: Option<&FunctionAnnotations>) {
    let Some(annotations) = annotations else {
        return;
    };

    for annotation in &annotations.locals {
        let index = annotation.slot.0 as usize;
        let Some(local) = locals.get_mut(index) else {
            continue;
        };
        *local = match annotation.source {
            AnnotationSource::Parameter => annotation.ty,
            AnnotationSource::AnnAssign => local.join(annotation.ty),
        };
    }
}

fn seed_speculative_parameters(locals: &mut [Type], arity: usize) {
    for local in locals.iter_mut().take(arity) {
        if *local == Type::Bottom {
            *local = Type::IntI64;
        }
    }
}

fn infer_inst(
    result: Value,
    kind: &InstKind,
    names: &[String],
    locals: &mut [Type],
    values: &HashMap<Value, Type>,
    value_names: &mut HashMap<Value, String>,
    range_values: &mut HashSet<Value>,
    range_iters: &mut HashSet<Value>,
) -> Type {
    match kind {
        InstKind::Const(constant) => const_type(constant),
        InstKind::LoadLocal(local) => local_type(locals, *local),
        InstKind::StoreLocal(local, value) => {
            let ty = value_type(values, *value);
            store_local_type(locals, *local, ty);
            ty
        }
        InstKind::BinaryOp { op, lhs, rhs } | InstKind::InplaceOp { op, lhs, rhs } => {
            arithmetic_type(*op, value_type(values, *lhs), value_type(values, *rhs))
        }
        InstKind::UnaryOp { op, operand } => unary_type(*op, value_type(values, *operand)),
        InstKind::Compare { .. }
        | InstKind::BoolTest { .. }
        | InstKind::Not { .. }
        | InstKind::Contains { .. }
        | InstKind::Is { .. }
        | InstKind::MatchSequence { .. }
        | InstKind::MatchMapping { .. }
        | InstKind::MatchLenGe { .. } => Type::Bool,
        InstKind::LoadBuiltin(name) | InstKind::LoadGlobal(name) | InstKind::LoadName(name) => {
            if let Some(name) = names.get(name.0 as usize) {
                // The result is still a boxed callable/object; the side table lets
                // calls to representative builtins infer their result types.
                value_names.insert(result, name.clone());
            }
            Type::Object
        }
        InstKind::Call { callee, args } => call_type(result, *callee, args, values, value_names, range_values),
        InstKind::GetIter { iterable } => {
            if range_values.contains(iterable) {
                range_iters.insert(result);
            }
            Type::Object
        }
        InstKind::ForNext { iter } => {
            if range_iters.contains(iter) {
                Type::IntI64
            } else {
                Type::Object
            }
        }
        InstKind::GetLen { .. } => Type::IntI64,
        _ => Type::Object,
    }
}

fn const_type(constant: &PyConst) -> Type {
    match constant {
        PyConst::Int(_) => Type::IntI64,
        PyConst::Float(_) => Type::Float,
        PyConst::Bool(_) => Type::Bool,
        PyConst::Str(_) => Type::Str,
        _ => Type::Object,
    }
}

fn local_type(locals: &[Type], local: LocalId) -> Type {
    locals.get(local.0 as usize).copied().unwrap_or(Type::Object)
}

fn store_local_type(locals: &mut [Type], local: LocalId, ty: Type) {
    let Some(slot) = locals.get_mut(local.0 as usize) else {
        return;
    };
    *slot = slot.join(ty);
}

fn value_type(values: &HashMap<Value, Type>, value: Value) -> Type {
    values.get(&value).copied().unwrap_or(Type::Object)
}

fn arithmetic_type(op: BinOp, lhs: Type, rhs: Type) -> Type {
    match (op, lhs, rhs) {
        (
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::FloorDiv | BinOp::Mod,
            Type::IntI64,
            Type::IntI64,
        ) => Type::IntI64,
        (BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::FloorDiv | BinOp::Mod, lhs, rhs)
            if numeric(lhs) && numeric(rhs) && (lhs == Type::Float || rhs == Type::Float) =>
        {
            Type::Float
        }
        _ => Type::Object,
    }
}

fn unary_type(op: UnOp, operand: Type) -> Type {
    match (op, operand) {
        (UnOp::Neg | UnOp::Pos, Type::IntI64) => Type::IntI64,
        (UnOp::Neg | UnOp::Pos, Type::Float) => Type::Float,
        _ => Type::Object,
    }
}

fn numeric(ty: Type) -> bool {
    matches!(ty, Type::IntI64 | Type::Float)
}

fn call_type(
    result: Value,
    callee: Value,
    args: &[Value],
    values: &HashMap<Value, Type>,
    value_names: &HashMap<Value, String>,
    range_values: &mut HashSet<Value>,
) -> Type {
    match value_names.get(&callee).map(String::as_str) {
        Some("len") if args.len() == 1 => Type::IntI64,
        Some("range") if (1..=3).contains(&args.len()) && args.iter().all(|arg| value_type(values, *arg) == Type::IntI64) => {
            range_values.insert(result);
            Type::Object
        }
        _ => Type::Object,
    }
}

fn write_inferred_type(inst: &mut pon_ir::Inst, ty: Type) {
    if ty == Type::Bottom {
        return;
    }
    inst.inferred_type = inst.inferred_type.join(ty);
    if inst.static_type == Type::Object {
        inst.static_type = static_bound(ty);
    }
}

fn static_bound(ty: Type) -> Type {
    match ty {
        Type::IntI64 => Type::Int,
        other => other,
    }
}

fn collect_return_value(term: &Terminator, out: &mut Vec<Value>) {
    if let Terminator::Return(value) = term {
        out.push(*value);
    }
}

fn write_value_type(function: &mut pon_ir::Function, value: Value, ty: Type) {
    for block in &mut function.blocks {
        for inst in &mut block.insts {
            if inst.result == value {
                write_inferred_type(inst, ty);
                return;
            }
        }
    }
}
