use super::*;
use ruff_python_ast::{
    AtomicNodeIndex, Comprehension, Identifier, Parameter, ParameterWithDefault, Parameters,
};

const COMP_ITER_PARAM: &str = ".0";

#[derive(Clone, Copy)]
enum CollectKind {
    List,
    Set,
    Dict,
}

pub(super) fn lower_list_comp_inline(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    expr: &ruff_python_ast::ExprListComp,
) -> Result<Value, LowerError> {
    lower_collecting_comprehension(
        driver,
        scope,
        "<listcomp>",
        &expr.generators,
        span_bounds(expr.range.start().to_u32(), expr.range.end().to_u32()),
        CollectKind::List,
        |driver, body, accumulator| {
            let item = driver.lower_expr(body, &expr.elt)?;
            body.emit(InstKind::ListAppend {
                list: accumulator,
                item,
            })?;
            Ok(())
        },
    )
}

pub(super) fn lower_set_comp_inline(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    expr: &ruff_python_ast::ExprSetComp,
) -> Result<Value, LowerError> {
    lower_collecting_comprehension(
        driver,
        scope,
        "<setcomp>",
        &expr.generators,
        span_bounds(expr.range.start().to_u32(), expr.range.end().to_u32()),
        CollectKind::Set,
        |driver, body, accumulator| {
            let item = driver.lower_expr(body, &expr.elt)?;
            body.emit(InstKind::SetAdd {
                set: accumulator,
                item,
            })?;
            Ok(())
        },
    )
}

pub(super) fn lower_dict_comp_inline(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    expr: &ruff_python_ast::ExprDictComp,
) -> Result<Value, LowerError> {
    lower_collecting_comprehension(
        driver,
        scope,
        "<dictcomp>",
        &expr.generators,
        span_bounds(expr.range.start().to_u32(), expr.range.end().to_u32()),
        CollectKind::Dict,
        |driver, body, accumulator| {
            let key = driver.lower_expr(body, &expr.key)?;
            let val = driver.lower_expr(body, &expr.value)?;
            body.emit(InstKind::MapInsert {
                map: accumulator,
                key,
                val,
            })?;
            Ok(())
        },
    )
}

pub(super) fn lower_generator_expr(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    expr: &ruff_python_ast::ExprGenerator,
) -> Result<Value, LowerError> {
    lower_comprehension_call(
        driver,
        scope,
        "<genexpr>",
        &expr.generators,
        span_bounds(expr.range.start().to_u32(), expr.range.end().to_u32()),
        |driver, body| {
            lower_comprehension_loops(driver, body, &expr.generators, 0, &mut |driver, body| {
                let item = driver.lower_expr(body, &expr.elt)?;
                body.emit(InstKind::Yield { val: item })?;
                Ok(())
            })
        },
    )
}

fn lower_collecting_comprehension(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    child_name: &str,
    generators: &[Comprehension],
    span: SourceSpan,
    collect_kind: CollectKind,
    mut emit_item: impl FnMut(&mut LoweringDriver, &mut BodyScope, Value) -> Result<(), LowerError>,
) -> Result<Value, LowerError> {
    lower_comprehension_call(driver, scope, child_name, generators, span, |driver, body| {
        let accumulator = match collect_kind {
            CollectKind::List => body.emit(InstKind::BuildList { elts: Vec::new() })?,
            CollectKind::Set => body.emit(InstKind::BuildSet { elts: Vec::new() })?,
            CollectKind::Dict => body.emit(InstKind::BuildMap { pairs: Vec::new() })?,
        };
        lower_comprehension_loops(driver, body, generators, 0, &mut |driver, body| {
            emit_item(driver, body, accumulator)
        })?;
        body.set_term(Terminator::Return(accumulator))
    })
}

fn lower_comprehension_call(
    driver: &mut LoweringDriver,
    enclosing: &mut BodyScope,
    child_name: &str,
    generators: &[Comprehension],
    span: SourceSpan,
    lower_body: impl FnOnce(&mut LoweringDriver, &mut BodyScope) -> Result<(), LowerError>,
) -> Result<Value, LowerError> {
    let first_generator = generators
        .first()
        .ok_or_else(|| LowerError::internal("comprehension without generator clause"))?;
    reject_async_comprehension(generators, span)?;

    let outer_iterable = driver.lower_expr(enclosing, &first_generator.iter)?;
    let outer_iter = enclosing.emit(InstKind::GetIter {
        iterable: outer_iterable,
    })?;
    let parameters = comprehension_parameters(first_generator);
    let child_info = enclosing.next_child_scope(ScopeKind::Comprehension, child_name)?;
    let function = synth::synthesize_scope_function(driver, enclosing, child_info, &parameters, lower_body)?;
    enclosing.emit(InstKind::Call {
        callee: function,
        args: vec![outer_iter],
    })
}

fn lower_comprehension_loops(
    driver: &mut LoweringDriver,
    body: &mut BodyScope,
    generators: &[Comprehension],
    index: usize,
    emit_leaf: &mut impl FnMut(&mut LoweringDriver, &mut BodyScope) -> Result<(), LowerError>,
) -> Result<(), LowerError> {
    let Some(generator) = generators.get(index) else {
        return emit_leaf(driver, body);
    };

    let header_block = body.alloc_block()?;
    let body_block = body.alloc_block()?;
    let done_block = body.alloc_block()?;
    let iter = if index == 0 {
        let slot = body.local_slot(COMP_ITER_PARAM).ok_or_else(|| {
            LowerError::internal("comprehension scope is missing synthetic .0 parameter")
        })?;
        body.emit(InstKind::LoadLocal(slot))?
    } else {
        let iterable = driver.lower_expr(body, &generator.iter)?;
        body.emit(InstKind::GetIter { iterable })?
    };
    body.set_term(Terminator::Jump(header_block))?;

    body.switch_to(header_block)?;
    let item = body.emit(InstKind::ForNext { iter })?;
    body.set_term(Terminator::ForLoop {
        iter,
        body: body_block,
        done: done_block,
    })?;

    body.switch_to(body_block)?;
    driver.lower_store_target(body, &generator.target, item)?;
    for if_expr in &generator.ifs {
        let pass_block = body.alloc_block()?;
        let test = driver.lower_expr(body, if_expr)?;
        let cond = body.emit(InstKind::BoolTest { val: test })?;
        body.set_term(Terminator::CondBranch {
            cond,
            then_: pass_block,
            else_: header_block,
        })?;
        body.switch_to(pass_block)?;
    }

    lower_comprehension_loops(driver, body, generators, index + 1, emit_leaf)?;
    body.jump_if_open(header_block)?;
    body.switch_to(done_block)
}

fn reject_async_comprehension(generators: &[Comprehension], span: SourceSpan) -> Result<(), LowerError> {
    if generators.iter().any(|generator| generator.is_async) {
        return unsupported_at("async comprehensions", span);
    }
    Ok(())
}

fn comprehension_parameters(generator: &Comprehension) -> Parameters {
    let range = generator.range;
    Parameters {
        range,
        node_index: AtomicNodeIndex::NONE,
        posonlyargs: Vec::new(),
        args: vec![ParameterWithDefault {
            range,
            node_index: AtomicNodeIndex::NONE,
            parameter: Parameter {
                range,
                node_index: AtomicNodeIndex::NONE,
                name: Identifier::new(COMP_ITER_PARAM, range),
                annotation: None,
            },
            default: None,
        }],
        vararg: None,
        kwonlyargs: Vec::new(),
        kwarg: None,
    }
}
