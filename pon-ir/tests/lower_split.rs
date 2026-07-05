use pon_ir::{
	BinOp, CellId, Function, InstKind, LocalId, Module, NameId, PyConst, Terminator, Value,
	lower_source,
};

const PHASE_A_HELLO: &str = r#"
def add(a, b):
    return a + b

print("hello, world")
print(add(2, 3))
"#;

#[test]
fn phase_a_hello_lowers_to_main_and_add_functions() {
	let module = lower_source(PHASE_A_HELLO).expect("Phase-A hello source should lower");

	assert_eq!(module.functions.len(), 2);
	assert_eq!(module.names, vec!["add".to_owned(), "print".to_owned()]);
	assert!(
		!module.names.iter().any(|name| name == "a" || name == "b"),
		"function-local parameter names must not be interned into Module.names: {:?}",
		module.names
	);

	let main = module
		.functions
		.get(module.main.0 as usize)
		.expect("module.main should point at a lowered function");
	assert_eq!(main.name, "__main__");

	let add = module
		.functions
		.iter()
		.find(|function| function.name == "add")
		.expect("expected lowered functions to include add");
	assert_eq!(add.blocks[0].insts[0].kind, InstKind::LoadLocal(LocalId(0)));
	assert_eq!(add.blocks[0].insts[1].kind, InstKind::LoadLocal(LocalId(1)));
}

#[test]
fn module_function_read_lowers_add_call_through_global_lookup() {
	let module = lower_source(
		r#"
def add(a, b):
    return a + b

print(add(1, 2))
"#,
	)
	.expect("module-level function call should lower");

	let add_name = NameId(
		module
			.names
			.iter()
			.position(|name| name == "add")
			.expect("add should be interned in module names") as u32,
	);
	let main = &module.functions[module.main.0 as usize];
	assert_eq!(main.name, "__main__");

	let main_insts = main
		.blocks
		.iter()
		.flat_map(|block| block.insts.iter())
		.collect::<Vec<_>>();
	assert!(
		!main_insts
			.iter()
			.any(|inst| inst.kind == InstKind::LoadName(add_name)),
		"module-level reads of add must not use class-namespace LoadName semantics"
	);

	let add_load = main_insts
		.iter()
		.find(|inst| inst.kind == InstKind::LoadGlobal(add_name))
		.expect("module-level read of add should lower as LoadGlobal");
	let add_call = main_insts
		.iter()
		.find(|inst| {
			matches!(
				 &inst.kind,
				 InstKind::Call { callee, args } if *callee == add_load.result && args.len() == 2
			)
		})
		.expect("add(1, 2) should call the globally loaded add function");
	let InstKind::Call { args, .. } = &add_call.kind else {
		unreachable!("add_call was selected by InstKind::Call shape");
	};
	let const_for = |value: &Value| {
		main_insts
			.iter()
			.find(|inst| inst.result == *value)
			.map(|inst| &inst.kind)
	};
	assert_eq!(const_for(&args[0]), Some(&InstKind::Const(PyConst::Int(1))));
	assert_eq!(const_for(&args[1]), Some(&InstKind::Const(PyConst::Int(2))));
}

#[test]
fn if_else_lowers_to_conditional_branch_blocks() {
	let module = lower_source(
		r#"
if 1:
    print("ok")
else:
    print("no")
"#,
	)
	.expect("representative if/else source should lower");

	let main = &module.functions[module.main.0 as usize];
	assert!(
		main.blocks.len() >= 4,
		"if/else lowering should create entry, branch-body, and join blocks, got {:?}",
		main.blocks
	);

	let (then_, else_) = main
		.blocks
		.iter()
		.find_map(|block| match &block.term {
			Terminator::CondBranch { then_, else_, .. } => Some((*then_, *else_)),
			_ => None,
		})
		.expect("if/else lowering should emit a CondBranch terminator");

	assert_ne!(then_, else_, "conditional branch should use distinct true and false destinations");
	assert!(
		(then_.0 as usize) < main.blocks.len(),
		"true destination should reference an existing block"
	);
	assert!(
		(else_.0 as usize) < main.blocks.len(),
		"false destination should reference an existing block"
	);

	let strings_in_block = |block_index: u32| {
		main.blocks[block_index as usize]
			.insts
			.iter()
			.filter_map(|inst| match &inst.kind {
				InstKind::Const(PyConst::Str(value)) => Some(value.as_str()),
				_ => None,
			})
			.collect::<Vec<_>>()
	};

	assert!(strings_in_block(then_.0).contains(&"ok"), "true branch should lower the if body");
	assert!(strings_in_block(else_.0).contains(&"no"), "false branch should lower the else body");
}

#[test]
fn while_statement_lowers_to_conditional_loop_blocks() {
	let module = lower_source(
		r#"
i = 0
while i < 1:
    i = i + 1
"#,
	)
	.expect("while statements should lower into CFG blocks");

	let main = module
		.functions
		.get(module.main.0 as usize)
		.expect("module.main should point at a lowered function");
	assert!(
		main
			.blocks
			.iter()
			.any(|block| matches!(block.term, Terminator::CondBranch { .. })),
		"while header should lower to a conditional branch"
	);
}

#[test]
fn nested_closure_lowers_with_captured_cells() {
	let module = lower_source(
		r#"
def add(a, b):
    return a + b

def outer(x):
    def inner(y):
        return x + y
    return inner
"#,
	)
	.expect("nested closure source should lower");

	let outer = module
		.functions
		.iter()
		.find(|function| function.name == "outer")
		.expect("expected lowered functions to include outer");
	assert!(
		outer
			.blocks
			.iter()
			.flat_map(|block| &block.insts)
			.any(|inst| inst.kind == InstKind::MakeCell(LocalId(0))),
		"outer should promote captured x from local slot 0 into a cell"
	);
	let inner_constructor = outer.blocks[0]
		.insts
		.iter()
		.find(|inst| {
			matches!(
				 &inst.kind,
				 InstKind::MakeFunctionFull { closure, .. } if !closure.is_empty()
			)
		})
		.expect("outer should construct inner with a non-empty closure vector");
	let InstKind::MakeFunctionFull { closure, .. } = &inner_constructor.kind else {
		unreachable!("inner constructor was selected by MakeFunctionFull shape");
	};
	assert_eq!(
		closure.as_slice(),
		&[CellId(0)][..],
		"outer should pass captured x as closure cell 0"
	);

	let inner = module
		.functions
		.iter()
		.find(|function| function.name == "inner")
		.expect("expected lowered functions to include inner");
	assert_eq!(inner.arity, 1);

	let inner_block = &inner.blocks[0];
	let captured_load = inner_block
		.insts
		.iter()
		.find(|inst| inst.kind == InstKind::LoadCell(CellId(0)))
		.expect("inner should load captured x through closure cell 0");
	let parameter_load = inner_block
		.insts
		.iter()
		.find(|inst| inst.kind == InstKind::LoadLocal(LocalId(0)))
		.expect("inner should load its y parameter from local slot 0");
	let add = inner_block
		.insts
		.iter()
		.find(|inst| {
			matches!(
				 &inst.kind,
				 InstKind::BinaryOp {
					  op: BinOp::Add,
					  lhs,
					  rhs,
				 } if *lhs == captured_load.result && *rhs == parameter_load.result
			)
		})
		.expect("inner should add the closure-loaded x value to its y parameter");
	assert_eq!(
		inner_block.term,
		Terminator::Return(add.result),
		"inner should return the captured-variable addition"
	);
}

#[test]
fn generator_yield_and_return_lower_to_suspend_state_machine() {
	let module = lower_source(
		r#"
def inner():
    yield 1
    return 6
"#,
	)
	.expect("generator return should lower");
	let inner = module
		.functions
		.iter()
		.find(|function| function.name == "inner")
		.expect("expected lowered functions to include inner");
	assert!(inner.is_generator, "yield body must be marked as a generator");
	let suspends: Vec<u32> = inner
		.blocks
		.iter()
		.filter_map(|block| match block.term {
			Terminator::Suspend { state, .. } => Some(state),
			_ => None,
		})
		.collect();
	assert_eq!(suspends, vec![1], "one yield owns exactly state 1");
	assert!(
		inner
			.blocks
			.iter()
			.any(|block| matches!(block.term, Terminator::Return(_))),
		"explicit return survives as a plain Return terminator; the runtime finish epilogue carries \
		 StopIteration.value"
	);
	assert!(
		!inner
			.blocks
			.iter()
			.flat_map(|block| &block.insts)
			.any(|inst| matches!(inst.kind, InstKind::Yield { .. } | InstKind::YieldFrom { .. })),
		"the transform must consume every Yield/YieldFrom marker"
	);
}

// --- span-keyed child-scope pairing regressions ------------------------------
//
// Lowering pairs each def/lambda/comprehension with its ScopeInfo child by
// (kind, name, span).  The order-based first-unused pairing it replaced failed
// in observable ways covered below:
//   * `try` lowers `else` before its handlers, so same-named defs in the two
//     suites swapped ScopeInfos: the `yield` body lowered as a plain function
//     (raw Yield markers survived for codegen to reject) while the plain body
//     was rewritten as a bogus state machine;
//   * the `except*` lowering path failed the same way;
//   * `finally` suites are inlined once per departing edge, so the second claim
//     of the same def died with "scope metadata was not discovered".

fn functions_named<'m>(module: &'m Module, name: &str) -> Vec<&'m Function> {
	module
		.functions
		.iter()
		.filter(|function| function.name == name)
		.collect()
}

fn has_suspend(function: &Function) -> bool {
	function
		.blocks
		.iter()
		.any(|block| matches!(block.term, Terminator::Suspend { .. }))
}

fn has_str_const(function: &Function, wanted: &str) -> bool {
	function
		.blocks
		.iter()
		.flat_map(|block| &block.insts)
		.any(|inst| matches!(&inst.kind, InstKind::Const(PyConst::Str(s)) if s.as_str() == wanted))
}

fn raw_yield_marker_count(module: &Module) -> usize {
	module
		.functions
		.iter()
		.flat_map(|function| &function.blocks)
		.flat_map(|block| &block.insts)
		.filter(|inst| matches!(inst.kind, InstKind::Yield { .. } | InstKind::YieldFrom { .. }))
		.count()
}

/// Shared assertions for the try/except/else twin-`f` shapes: whichever suite
/// holds the `yield p` body must come out as the generator state machine, and
/// the `raise RuntimeError('x')` body must stay a plain function.
fn assert_twin_f_pairing(source: &str) {
	let module = lower_source(source).expect("twin-f try shape should lower");
	let fs = functions_named(&module, "f");
	assert_eq!(fs.len(), 2, "both defs of f lower to distinct functions");

	let generators: Vec<&Function> = fs.iter().copied().filter(|f| f.is_generator).collect();
	let plains: Vec<&Function> = fs.iter().copied().filter(|f| !f.is_generator).collect();
	assert_eq!(generators.len(), 1, "exactly one lowered f is a generator");
	assert_eq!(plains.len(), 1, "exactly one lowered f is a plain function");

	let generator = generators[0];
	let plain = plains[0];
	assert!(has_suspend(generator), "the yield body must lower to a Suspend state machine");
	assert!(
		!has_str_const(generator, "x"),
		"the generator ScopeInfo must pair with the yield body, not the raise body"
	);
	assert!(
		has_str_const(plain, "x"),
		"the raise body must stay the plain (non-generator) function"
	);
	assert!(!has_suspend(plain), "the raise body must not be rewritten as a state machine");

	assert_eq!(
		raw_yield_marker_count(&module),
		0,
		"the generator transform must consume every raw Yield/YieldFrom marker"
	);
}

#[test]
fn try_else_generator_span_pairing_survives_handler_first_scope_order() {
	// `else` lowers before the handler suite; order-based pairing handed the
	// else generator the handler's non-generator ScopeInfo.
	assert_twin_f_pairing(
		"try: pass\nexcept ImportError:\n    def f(p):\n        raise RuntimeError('x')\nelse:\n    \
		 def f(p):\n        yield p\n",
	);
}

#[test]
fn try_handler_generator_span_pairing_mirror() {
	// Mirror orientation: the handler owns the generator body.
	assert_twin_f_pairing(
        "try: pass\nexcept ImportError:\n    def f(p):\n        yield p\nelse:\n    def f(p):\n        raise RuntimeError('x')\n",
    );
}

#[test]
fn try_star_generator_span_pairing_both_orientations() {
	// The `except*` lowering path claims children the same way.
	assert_twin_f_pairing(
		"try: pass\nexcept* ValueError:\n    def f(p):\n        raise RuntimeError('x')\nelse:\n    \
		 def f(p):\n        yield p\n",
	);
	assert_twin_f_pairing(
        "try: pass\nexcept* ValueError:\n    def f(p):\n        yield p\nelse:\n    def f(p):\n        raise RuntimeError('x')\n",
    );
}

#[test]
fn finally_inlined_def_span_pairing_lowers_every_clone() {
	// `finally` suites are inlined once per departing edge, so the same def
	// statement claims its ScopeInfo child several times.  Used-marking made
	// the second claim fail with "scope metadata was not discovered for g".
	let module = lower_source(
		"def outer():\n    try:\n        return 1\n    finally:\n        def g():\n            \
		 yield 2\n",
	)
	.expect("def inside finally must lower even though the suite is inlined per edge");

	assert_eq!(functions_named(&module, "outer").len(), 1);

	let clones = functions_named(&module, "g");
	assert!(
		clones.len() >= 2,
		"finally inlining lowers g once per departing edge, got {}",
		clones.len()
	);
	for clone in &clones {
		assert!(clone.is_generator, "every inlined g clone is a generator");
		assert!(has_suspend(clone), "every inlined g clone must carry the Suspend state machine");
	}
	assert_eq!(
		raw_yield_marker_count(&module),
		0,
		"no raw Yield/YieldFrom marker may survive in any clone"
	);
}

#[test]
fn except_star_bare_raise_lowers() {
	// A lexical `raise` ending an `except*` handler body left the body block
	// unterminated: `redirect_raise_terms` rewrote the open block's RaiseTerm
	// into the raised-path jump, but the clause epilogue dropped that term
	// instead of restoring it before switching blocks.
	for source in [
		"try:\n    raise ExceptionGroup('eg', [TypeError(1)])\nexcept* TypeError:\n    raise\n",
		"try:\n    raise ExceptionGroup('eg', [TypeError(1)])\nexcept* TypeError as e:\n    raise\n",
		"try:\n    raise ExceptionGroup('eg', [TypeError(1), ValueError(2)])\nexcept* TypeError as \
		 e:\n    raise\nexcept* ValueError as e:\n    raise\n",
		"try:\n    raise ExceptionGroup('eg', [TypeError(1)])\nexcept* TypeError:\n    raise \
		 ValueError('x')\n",
		"def f():\n    try:\n        raise ExceptionGroup('eg', [TypeError(1)])\n    except* \
		 TypeError:\n        raise\n    finally:\n        pass\n",
	] {
		lower_source(source).unwrap_or_else(|error| {
			panic!("except* body ending in raise must lower: {error}\nsource:\n{source}")
		});
	}
}
