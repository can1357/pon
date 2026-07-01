use pon_ir::{BinOp, CellId, InstKind, LocalId, NameId, PyConst, Terminator, Value, lower_source};

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
        .find(|inst| matches!(
            &inst.kind,
            InstKind::Call { callee, args } if *callee == add_load.result && args.len() == 2
        ))
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

    assert_ne!(
        then_, else_,
        "conditional branch should use distinct true and false destinations"
    );
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

    assert!(
        strings_in_block(then_.0).contains(&"ok"),
        "true branch should lower the if body"
    );
    assert!(
        strings_in_block(else_.0).contains(&"no"),
        "false branch should lower the else body"
    );
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
        main.blocks
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
        .find(|inst| matches!(
            &inst.kind,
            InstKind::MakeFunctionFull { closure, .. } if !closure.is_empty()
        ))
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
        .find(|inst| matches!(
            &inst.kind,
            InstKind::BinaryOp {
                op: BinOp::Add,
                lhs,
                rhs,
            } if *lhs == captured_load.result && *rhs == parameter_load.result
        ))
        .expect("inner should add the closure-loaded x value to its y parameter");
    assert_eq!(
        inner_block.term,
        Terminator::Return(add.result),
        "inner should return the captured-variable addition"
    );
}

#[test]
fn generator_return_lowers_to_eager_generator_return_value() {
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
    assert!(
        inner
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|inst| matches!(inst.kind, InstKind::EagerGeneratorReturn { .. })),
        "explicit return in a generator should preserve StopIteration.value through EagerGeneratorReturn"
    );
}
