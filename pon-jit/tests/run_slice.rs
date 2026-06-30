const PHASE_A_HELLO: &str = r#"
def add(a, b):
    return a + b

print("hello, world")
print(add(2, 3))
"#;

#[test]
fn phase_a_hello_slice_executes_successfully() {
    let module =
        pon_ir::lower_source(PHASE_A_HELLO).expect("Phase-A hello source lowers to IR");
    let mut engine = pon_jit::JitEngine::new();

    engine
        .run(&module)
        .expect("Phase-A hello source executes successfully through the JIT");
}
