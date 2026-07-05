use ruff_python_ast::{
	AtomicNodeIndex, Comprehension, Identifier, Parameter, ParameterWithDefault, Parameters,
};

use super::*;

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
			body.emit(InstKind::ListAppend { list: accumulator, item })?;
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
			body.emit(InstKind::SetAdd { set: accumulator, item })?;
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
			body.emit(InstKind::MapInsert { map: accumulator, key, val })?;
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
		true,
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
	lower_comprehension_call(driver, scope, child_name, generators, span, false, |driver, body| {
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

/// Materializes one comprehension as a call of its synthesized child-scope
/// function over the outer iterable's (async) iterator.
///
/// PEP 530 async comprehensions: when scope analysis marked the child scope
/// async (an `async for` clause or an `await` anywhere in the clauses/element),
/// the child is synthesized as a coroutine — `is_async` flows through
/// `ScopeInfo` into `Function::is_coroutine` — and the call site awaits it,
/// mirroring CPython's `GET_AWAITABLE`/`SEND` sequence after the comprehension
/// call.  The outer iterator handed over as `.0` is acquired with `GetAIter`
/// exactly when the FIRST clause is async; a sync first clause with `await`
/// deeper in still iterates synchronously.
///
/// Async generator expressions (PEP 525) synthesize their child as an async
/// generator function instead: the call site returns the async-generator
/// object directly (constructing it awaits nothing), and consumers drive it
/// through `__anext__`/`asend` awaitables.
fn lower_comprehension_call(
	driver: &mut LoweringDriver,
	enclosing: &mut BodyScope,
	child_name: &str,
	generators: &[Comprehension],
	span: SourceSpan,
	is_genexpr: bool,
	lower_body: impl FnOnce(&mut LoweringDriver, &mut BodyScope) -> Result<(), LowerError>,
) -> Result<Value, LowerError> {
	let first_generator = generators
		.first()
		.ok_or_else(|| LowerError::internal("comprehension without generator clause"))?;

	let outer_iterable = driver.lower_expr(enclosing, &first_generator.iter)?;
	let child_info = enclosing.next_child_scope(
		ScopeKind::Comprehension,
		child_name,
		Some((span.start, span.end)),
	)?;
	let is_async_comp = child_info.is_async;
	// Collecting comprehensions run eagerly: the child coroutine is awaited at
	// the call site, which is only legal inside an async function.  A genexpr
	// child merely constructs an async-generator object, which is legal in any
	// context (PEP 530 / Python 3.7+).
	let awaits_child = is_async_comp && !is_genexpr;
	if awaits_child && !enclosing.info.is_async {
		// CPython rejects this shape at compile time with the same words.
		return unsupported_at(
			"asynchronous comprehension outside of an asynchronous function",
			span,
		);
	}
	let outer_iter = if first_generator.is_async {
		enclosing.emit(InstKind::GetAIter { iterable: outer_iterable })?
	} else {
		enclosing.emit(InstKind::GetIter { iterable: outer_iterable })?
	};
	let parameters = comprehension_parameters(first_generator);
	let function =
		synth::synthesize_scope_function(driver, enclosing, child_info, &parameters, lower_body)?;
	let call = enclosing.emit(InstKind::Call { callee: function, args: vec![outer_iter] })?;
	if awaits_child {
		let iter = enclosing.emit(InstKind::Await { awaitable: call })?;
		enclosing.emit(InstKind::YieldFrom { iter })
	} else {
		Ok(call)
	}
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
	let done_block = body.alloc_block()?;
	let iter = if index == 0 {
		let slot = body.local_slot(COMP_ITER_PARAM).ok_or_else(|| {
			LowerError::internal("comprehension scope is missing synthetic .0 parameter")
		})?;
		body.emit(InstKind::LoadLocal(slot))?
	} else {
		let iterable = driver.lower_expr(body, &generator.iter)?;
		if generator.is_async {
			body.emit(InstKind::GetAIter { iterable })?
		} else {
			body.emit(InstKind::GetIter { iterable })?
		}
	};
	body.set_term(Terminator::Jump(header_block))?;

	body.switch_to(header_block)?;
	let item = if generator.is_async {
		generator::emit_async_for_step(driver, body, iter, done_block)?
	} else {
		let body_block = body.alloc_block()?;
		let item = body.emit(InstKind::ForNext { iter })?;
		body.set_term(Terminator::ForLoop { iter, body: body_block, done: done_block })?;
		body.switch_to(body_block)?;
		item
	};
	driver.lower_store_target(body, &generator.target, item)?;
	for if_expr in &generator.ifs {
		let pass_block = body.alloc_block()?;
		let test = driver.lower_expr(body, if_expr)?;
		let cond = body.emit(InstKind::BoolTest { val: test })?;
		body.set_term(Terminator::CondBranch { cond, then_: pass_block, else_: header_block })?;
		body.switch_to(pass_block)?;
	}

	lower_comprehension_loops(driver, body, generators, index + 1, emit_leaf)?;
	body.jump_if_open(header_block)?;
	body.switch_to(done_block)
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
