use std::time::Instant;
use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

/// Generate many float variables and densely connected GOSUB/GOTO targets.
fn mesh_program(n: usize) -> String {
    let mut s = String::new();
    s.push_str("1 GOSUB 1000\n");
    for i in 0..n {
        let line = 10 + i * 10;
        let v = (b'A' + (i % 26) as u8) as char;
        let w = (b'A' + ((i * 7 + 3) % 26) as u8) as char;
        // Every line both writes a scalar and jumps into the shared tail, so each of those targets collects a predecessor per line.
        s.push_str(&format!(
            "{line} {v}={w}+{}: IF {v}>100 THEN GOSUB 1000\n",
            i % 9 + 1
        ));
        s.push_str(&format!("{} IF {w}<0 THEN GOTO 10\n", line + 5));
    }
    s.push_str("1000 Z=Z+1: RETURN\n");
    s
}

fn compile_secs(src: &str) -> f64 {
    let prg = tokenize_program(src).expect("tokenize");
    let t = Instant::now();
    compile_with_options(&prg, CompileOptions::default()).expect("compiles");
    t.elapsed().as_secs_f64()
}

#[test]
fn doubling_the_program_does_not_multiply_the_time_by_an_order_of_magnitude() {
    // Warm the allocator and any lazily-built tables so the small measurement is not dominated by first-call cost.
    let _ = compile_secs(&mesh_program(40));

    let small = compile_secs(&mesh_program(150));
    let large = compile_secs(&mesh_program(300));

    // A floor keeps the ratio meaningful when both runs are fast enough that timer granularity dominates.
    let ratio = large / small.max(0.005);
    assert!(
        ratio < 12.0,
        "compile time grew {ratio:.1}x for 2x the program \
         ({small:.3}s -> {large:.3}s); the CFG dataflow has gone \
         super-linear again — see the module header"
    );
}

#[test]
fn a_high_in_degree_cfg_compiles_in_reasonable_time() {

    let secs = compile_secs(&mesh_program(400));
    assert!(
        secs < 60.0,
        "a 800-line high-in-degree program took {secs:.1}s to compile"
    );
}
