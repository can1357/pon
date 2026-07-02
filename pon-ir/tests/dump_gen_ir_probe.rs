//! TEMP PROBE (J1c): dump transformed generator IR. Deleted before yield.

#[test]
fn dump_try_finally_gen_ir() {
    for (label, source) in [
        ("gen try/finally", "def g():\n    try:\n        yield 1\n    finally:\n        print(\"fin\")\n"),
        ("plain try/finally", "def f():\n    try:\n        print(\"body\")\n    finally:\n        print(\"fin\")\n"),
        ("gen try/except", "def g():\n    try:\n        yield 1\n    except KeyError:\n        print(\"caught\")\n        yield 2\n"),
    ] {
        let module = pon_ir::lower::lower_source(source).expect("lower");
        for function in &module.functions {
            if function.name == "__main__" { continue; }
            eprintln!("=== [{label}] fn {} is_gen={} n_locals={} ===", function.name, function.is_generator, function.n_locals);
            for block in &function.blocks {
                eprintln!("  block {:?}:", block.id);
                for inst in &block.insts {
                    eprintln!("    {:?} = {:?}", inst.result, inst.kind);
                }
                eprintln!("    term {:?}", block.term);
            }
        }
    }
    panic!("dump only");
}
