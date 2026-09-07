mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

// helpers

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

struct Built {
    asm: String,
    prg: Vec<u8>,
}

fn build(src: &str, profile: Profile) -> Built {
    let tok = tokenize_program(src).unwrap_or_else(|e| panic!("tokenize: {e:?}"));
    let c = compile_with_options(
        &tok,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compile ({profile:?}): {e}"));
    Built {
        asm: c.asm,
        prg: c.prg_bytes,
    }
}

fn run_text(prg: &[u8]) -> String {
    let r = emu64::run_prg_to_end(prg, common::MAX_INSNS).expect("emu64 runs the program");
    common::petscii_to_string(&r.output)
}

/// The program's own code, without the shared-helper / pool / BSS sections that follow it.
fn main_code(asm: &str) -> &str {
    asm.find("; --- helper subroutines ---")
        .or_else(|| asm.find("; --- DEF FN bodies ---"))
        .or_else(|| asm.find("; --- variable slots ---"))
        .map(|i| &asm[..i])
        .unwrap_or(asm)
}

/// Compile at all three profiles and assert the CHROUT stream matches what the ROM produced for
/// the same source.
#[track_caller]
fn assert_rom_output(src: &str, expected: &str) {
    let norm = |s: &str| {
        s.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    };
    for profile in PROFILES {
        let b = build(src, profile);
        let got = run_text(&b.prg);
        assert_eq!(
            norm(&got),
            norm(expected),
            "{profile:?}: real BASIC prints {expected:?}, got {got:?}"
        );
    }
}

/// ROM: ` 6`.
const F1_NUMERIC: &str = "\
10 def fn f(x)=x+a
20 a=5
30 print fn f(1)
";

#[test]
fn fn_body_read_keeps_an_earlier_looking_store_alive() {
    assert_rom_output(F1_NUMERIC, "6");
}

/// ROM: ` 4`.
const F1_STRING: &str = "\
10 def fn f(x)=x+len(b$)
20 b$=\"abc\"
30 print fn f(1)
";

#[test]
fn fn_body_string_read_keeps_the_string_store_alive() {
    assert_rom_output(F1_STRING, "4");
}

/// ROM: ` 6`.
const F1_NESTED: &str = "\
10 def fn g(y)=y+a
20 def fn f(x)=fn g(x)
30 a=5
40 print fn f(1)
";

#[test]
fn fn_body_read_is_charged_transitively_through_nested_calls() {
    assert_rom_output(F1_NESTED, "6");
}

/// ROM: ` 2  4  6`.
const F1_IN_LOOP: &str = "\
10 def fn f(x)=x*k
20 for i=1 to 3:k=i:print fn f(2);:next
";

#[test]
fn fn_body_read_keeps_a_loop_carried_store_alive() {
    assert_rom_output(F1_IN_LOOP, "2  4  6");
}

#[test]
fn dead_store_elimination_still_fires_around_a_def_fn() {
    let src = "\
10 def fn f(x)=x+a
20 b=5
30 a=7
40 print fn f(1)
";
    for profile in PROFILES {
        let b = build(src, profile);
        let code = main_code(&b.asm);
        assert!(
            !code.contains("V_B") && !code.contains("VI_B"),
            "{profile:?}: B is never read; its store is dead and must go\n{}",
            b.asm
        );
        assert!(
            code.contains("V_A") || code.contains("VI_A"),
            "{profile:?}: A is read by the FN body and must be stored\n{}",
            b.asm
        );
    }
    assert_rom_output(src, "8");
}

/// A `DEF FN` that is never called must not keep anything alive: the body is a declaration, and
/// a declaration executes no reads.
#[test]
fn an_uncalled_def_fn_keeps_nothing_alive() {
    let src = "\
10 def fn f(x)=x+a
20 a=5
30 print \"x\"
";
    for profile in PROFILES {
        let b = build(src, profile);
        let code = main_code(&b.asm);
        assert!(
            !code.contains("VI_A") && !code.contains("V_A"),
            "{profile:?}: FN F is never called, so A=5 is dead\n{}",
            b.asm
        );
    }
}

#[test]
fn fn_body_reads_excludes_the_parameter_and_closes_transitively() {
    use yabcompiler_core::ast::FnName;
    use yabcompiler_core::dataflow::FnBodyReads;

    let module = ir_of(
        "\
10 def fn g(y)=y+a
20 def fn f(x)=fn g(x)+x
30 def fn r(z)=fn r(z)+b
40 print fn f(1)+fn r(1)
",
    );
    let env = FnBodyReads::build(&module);

    let bases = |n: &str| -> Vec<String> {
        env.free_vars(&FnName(n.to_string()))
            .iter()
            .map(|v| v.base.clone())
            .collect()
    };
    assert_eq!(bases("G"), vec!["A".to_string()], "Y is G's parameter");
    assert_eq!(
        bases("F"),
        vec!["A".to_string()],
        "X is F's parameter; A comes through the call to G"
    );
    assert_eq!(
        bases("R"),
        vec!["B".to_string()],
        "self-recursion must converge, not hang or double-count Z"
    );
    assert!(
        env.free_vars(&FnName("ZZ".to_string())).is_empty(),
        "an undefined name has no body and so no reads"
    );
}

#[test]
fn a_variable_read_only_from_an_fn_body_interferes_across_the_call() {
    use yabcompiler_core::analysis::Registry;
    use yabcompiler_core::ast::{VarKind, VarName};
    use yabcompiler_core::dataflow::VarInterference;

    let module = ir_of(
        "\
1 def fn f(x)=x+k
2 k=5:print k
3 for i=1 to 3:print fn f(i);:next
",
    );
    let mut reg = Registry::new();
    let graph = reg.get(&module, &VarInterference).clone();
    let v = |b: &str| VarName {
        base: b.to_string(),
        kind: VarKind::Float,
    };
    assert!(
        graph.interferes(&v("K"), &v("I")),
        "K is read by the FN body at the call inside the FOR, so it is \
         live across the loop and must not share I's slot"
    );
}

fn ir_of(src: &str) -> yabcompiler_core::ir::Module {
    let tok = tokenize_program(src).expect("tokenize");
    let prg = yabcompiler_core::Program::parse(&tok).expect("prg");
    let ast = yabcompiler_core::parse::program(&prg).expect("parse");
    yabcompiler_core::ir::lower(&ast).expect("lower")
}

/// ROM: ` 101`.
const F2_REDEFINED: &str = "\
10 def fn f(x)=x+1
20 def fn f(x)=x+100
30 print fn f(1)
";

#[test]
fn a_redefined_fn_uses_the_last_definition() {
    assert_rom_output(F2_REDEFINED, "101");
}

/// ROM: ` 101  101`.
const F2_TWO_CHAR_COLLISION: &str = "\
10 def fn abc(x)=x+1
20 def fn abd(x)=x+100
30 print fn abc(1);fn abd(1)
";

#[test]
fn two_significant_characters_make_fnabc_and_fnabd_one_function() {
    assert_rom_output(F2_TWO_CHAR_COLLISION, "101  101");
}

/// ROM: ` 2` then ` 8`.
const F2_INTERLEAVED: &str = "\
10 def fn f(x)=x*2
20 print fn f(1)
30 def fn f(x)=x+7
40 print fn f(1)
";

#[test]
fn a_call_between_two_definitions_sees_the_first_body() {
    assert_rom_output(F2_INTERLEAVED, "2\n8");
}

/// ROM: ` 8`.
const F2_GOTO_ORDER: &str = "\
10 goto 100
20 def fn f(x)=x*2
30 print fn f(1):end
100 def fn f(x)=x+7
110 goto 30
";

#[test]
fn redefinition_follows_execution_order_not_source_order() {
    assert_rom_output(F2_GOTO_ORDER, "8");
}

/// ROM: ` 6`.
const F2_CONDITIONAL_TAKEN: &str = "\
10 read a
20 if a then def fn f(x)=x*2
30 print fn f(3)
40 data 1
";

#[test]
fn a_conditionally_defined_fn_works_on_the_taken_path() {
    for profile in PROFILES {
        let b = build(F2_CONDITIONAL_TAKEN, profile);
        assert!(
            b.asm.contains("JSR __FNCALL_F") && b.asm.contains("__FNV_F"),
            "{profile:?}: the binding only exists on the taken path, \
             so the call must go through the vector\n{}",
            b.asm
        );
    }
    assert_rom_output(F2_CONDITIONAL_TAKEN, "6");
}

#[test]
fn a_conditionally_defined_fn_traps_when_the_branch_is_not_taken() {
    let src = "\
10 read a
20 if a then def fn f(x)=x*2
30 print fn f(3)
40 data 0
";
    for profile in PROFILES {
        let b = build(src, profile);
        assert!(
            b.asm.contains("JSR __FNCALL_F"),
            "{profile:?}: the binding may not exist, so the call must be indirect\n{}",
            b.asm
        );
        assert!(
            b.asm.contains("BEQ __UNDEF_FN"),
            "{profile:?}: the trampoline must test the vector for zero\n{}",
            b.asm
        );
        assert!(
            b.asm.contains("__UNDEF_FN:") && b.asm.contains("LDX #$1B"),
            "{profile:?}: the zero vector must raise ?UNDEF'D FUNCTION (error 27)\n{}",
            b.asm
        );
    }
}

/// ROM: ` 3  33  63  93  123  153` then `-1  2  5  8  11  14`.
const F2_THREE_BINDINGS: &str = "\
10 dim a(5):for i=0 to 5:a(i)=i*3:next
20 def fn v(x)=a(x)+1
30 s$=\"tot\"
40 def fn v(x)=a(x)*10+len(s$)
50 for i=0 to 5:print fn v(i);:next
60 print
70 def fn v(x)=a(x)-1
80 for i=0 to 5:print fn v(i);:next
";

#[test]
fn three_bindings_of_one_name_each_take_effect_in_turn() {
    assert_rom_output(
        F2_THREE_BINDINGS,
        "3  33  63  93  123  153\n-1  2  5  8  11  14",
    );
}

/// ROM: ` 12  7`.
#[test]
fn a_dispatched_call_still_restores_the_parameter_variable() {
    assert_rom_output(
        "\
10 q=7
20 def fn f(q)=q*2
30 def fn f(q)=q*3
40 print fn f(4);q
",
        "12  7",
    );
}

/// ROM: ` 51  3  4`.
#[test]
fn each_dispatched_body_owns_its_own_parameter_variable() {
    assert_rom_output(
        "\
10 def fn f(a)=a*2
20 def fn f(b)=b+50
30 a=3:b=4
40 print fn f(1);a;b
",
        "51  3  4",
    );
}

/// The dispatch must not go through `JMP (vector)`.
#[test]
fn fn_dispatch_never_uses_an_indirect_jmp() {
    for src in [
        F2_REDEFINED,
        F2_TWO_CHAR_COLLISION,
        F2_INTERLEAVED,
        F2_THREE_BINDINGS,
        F2_CONDITIONAL_TAKEN,
    ] {
        for profile in PROFILES {
            let b = build(src, profile);
            assert!(
                b.asm.contains("__FNCALL_"),
                "{profile:?}: expected a dispatched call\n{}",
                b.asm
            );
            assert!(
                !b.asm.contains("JMP ("),
                "{profile:?}: indirect JMP is not page-safe on the 6510\n{}",
                b.asm
            );
        }
    }
}

#[test]
fn recursion_through_the_dispatch_vector_is_guarded() {
    for profile in PROFILES {
        let b = build(
            "\
10 def fn f(x)=x
20 def fn f(x)=fn f(x)
30 print fn f(1)
",
            profile,
        );
        assert!(
            b.asm.contains("BCC __FN_OOM") && b.asm.contains("__FN_OOM:"),
            "{profile:?}: a name whose call graph has a cycle needs the guard\n{}",
            b.asm
        );
    }
}

#[test]
fn a_single_definition_is_still_bound_statically() {
    let small = build("10 def fn f(x)=x*2\n20 print fn f(3)\n", Profile::Default);
    assert!(
        small.asm.contains("; inline __FN_F"),
        "a 3-node body is still inlined at the call site\n{}",
        small.asm
    );
    let big = build(
        "10 def fn f(x)=(x*2+x*3)/(x+1)-x*x\n20 print fn f(3)\n",
        Profile::Default,
    );
    assert!(
        big.asm.contains("JSR __FN_F") && !big.asm.contains("__FNCALL_F"),
        "a large body is still a direct JSR to a fixed label\n{}",
        big.asm
    );
    for asm in [&small.asm, &big.asm] {
        assert!(
            !asm.contains("__FNV_F"),
            "no dispatch vector for a name defined once\n{asm}"
        );
    }
}

/// ROM: ` 7  3`.
const F3_TWO_CALLS: &str = "\
10 dim a(3):a(0)=7:a(1)=3
20 def fn f(x)=a(x)
30 print fn f(0);fn f(1)
";

#[test]
fn an_array_indexing_fn_body_recomputes_the_address_per_call() {
    assert_rom_output(F3_TWO_CALLS, "7  3");
}

/// ROM: ` 7  3`.
const F3_READ_AFTER_CALL: &str = "\
10 dim a(3):a(0)=7:a(1)=3:x=1
20 def fn f(x)=a(x)
30 print fn f(0);a(x)
";

#[test]
fn a_plain_array_read_after_an_fn_call_recomputes_the_address() {
    assert_rom_output(F3_READ_AFTER_CALL, "7  3");
}

/// ROM: ` 7  9  9`.
const F3_DIFFERENT_INDEX_VARS: &str = "\
10 dim a(5):a(0)=7:a(1)=3:a(2)=9
20 def fn f(i)=a(i)
30 j=2:print fn f(0);fn f(j);a(j)
";

#[test]
fn different_index_variables_still_recompute_the_address() {
    assert_rom_output(F3_DIFFERENT_INDEX_VARS, "7  9  9");
}

/// ROM: ` 0  2  4  6`.
const F3_IN_FOR: &str = "\
10 dim a(3):for k=0 to 3:a(k)=k*2:next
20 def fn f(x)=a(x)
30 for k=0 to 3:print fn f(k);:next
";

#[test]
fn an_fn_call_inside_a_for_recomputes_the_address_each_iteration() {
    assert_rom_output(F3_IN_FOR, "0  2  4  6");
}

#[test]
fn the_array_address_cse_still_fires_next_to_an_fn_call() {
    let src = "\
10 dim a(10):i=3
20 def fn f(x)=a(x)
30 a(i)=a(i)+1
40 print a(i);fn f(i)
";
    let b = build(src, Profile::Default);
    let reuses = b.asm.matches("; reuse ARRAY_ADDR").count();
    assert!(
        reuses >= 2,
        "expected the A(I) address to be computed once and reused; got {reuses} reuses\n{}",
        b.asm
    );
    assert_rom_output(src, "1  1");
}

#[test]
fn a_recursive_fn_raises_out_of_memory_instead_of_hanging() {
    let src = "\
10 def fn f(x)=fn f(x)
20 print fn f(1)
";
    for profile in PROFILES {
        let b = build(src, profile);
        assert!(
            b.asm.contains("TSX") && b.asm.contains("BCC __FN_OOM"),
            "{profile:?}: a re-entrant body needs a stack-headroom guard\n{}",
            b.asm
        );
        assert!(
            b.asm.contains("__FN_OOM:") && b.asm.contains("LDX #$10"),
            "{profile:?}: the guard must raise ?OUT OF MEMORY (error 16)\n{}",
            b.asm
        );
    }
}

#[test]
fn mutually_recursive_fns_are_both_guarded() {
    let b = build(
        "\
10 def fn a(x)=fn b(x)
20 def fn b(x)=fn a(x)
30 print fn a(1)
",
        Profile::Default,
    );
    assert_eq!(
        b.asm.matches("BCC __FN_OOM").count(),
        2,
        "both bodies in the cycle need the guard\n{}",
        b.asm
    );
}

#[test]
fn a_non_recursive_fn_has_no_stack_guard() {
    let b = build(
        "\
10 def fn g(y)=y+1
20 def fn f(x)=fn g(x)*2
30 print fn f(3)
",
        Profile::Default,
    );
    assert!(
        !b.asm.contains("__FN_OOM"),
        "no cycle in the call graph, so no guard\n{}",
        b.asm
    );
}

#[test]
fn an_unreachable_call_to_an_undefined_fn_still_runs() {
    assert_rom_output(
        "\
10 print \"a\"
20 if 0 then print fn g(1)
30 print \"b\"
",
        "A\nB",
    );
}

/// A call that *is* reachable emits the trap.
#[test]
fn a_reachable_call_to_an_undefined_fn_traps_at_runtime() {
    for profile in PROFILES {
        let b = build("10 print fn g(1)\n", profile);
        assert!(
            b.asm.contains("__UNDEF_FN:") && b.asm.contains("LDX #$1B"),
            "{profile:?}: must raise ?UNDEF'D FUNCTION (error 27) at runtime\n{}",
            b.asm
        );
    }
}

#[test]
fn an_undefined_fn_evaluates_its_argument_first() {
    let b = build("10 print fn g(1/0)\n", Profile::Default);
    let trap = b
        .asm
        .find("__UNDEF_FN:")
        .expect("the call raises ?UNDEF'D FUNCTION");
    let div = format!("JSR ${:04X}", yabcompiler_core::runtime::FDIV);
    let div_at = b
        .asm
        .find(&div)
        .unwrap_or_else(|| panic!("the division must still be emitted\n{}", b.asm));
    assert!(
        div_at < trap,
        "the argument must be evaluated before the trap\n{}",
        b.asm
    );
}

#[test]
fn a_variable_an_fn_body_reads_is_not_split() {
    assert_rom_output(
        "\
10 def fnk(x)=x*q
20 q=3:print fnk(5);
30 q=10:print fnk(5)
",
        " 15  50",
    );
}

#[test]
fn a_variable_read_through_a_nested_fn_is_not_split() {
    assert_rom_output(
        "\
10 def fnb(y)=y*q
15 def fna(x)=fnb(x)
20 q=3:print fna(5);
30 q=10:print fna(5)
",
        " 15  50",
    );
}

#[test]
fn a_textually_late_def_fn_still_protects_its_free_variables() {
    assert_rom_output(
        "\
10 goto 50
20 q=3:print fnk(5);
30 q=10:print fnk(5)
40 end
50 def fnk(x)=x*q
60 goto 20
",
        " 15  50",
    );
}

/// The formal parameter shares the ordinary variable of that name and the call saves/restores
/// it around the body, so it cannot be split either.
#[test]
fn the_formal_parameter_is_not_split() {
    assert_rom_output(
        "\
10 def fnk(x)=x*2
20 x=5:print fnk(x);
30 x=7:print x
",
        " 10  7",
    );
}

#[test]
fn a_variable_with_no_fn_reader_still_splits() {
    let b = build(
        "10 a=peek(1024):print a\n20 a=peek(1025):print a\n",
        Profile::Default,
    );
    assert!(
        b.asm.contains("A__1"),
        "disjoint live ranges must still be split:\n{}",
        b.asm
    );
}

#[test]
fn a_def_fn_does_not_disable_splitting_for_unrelated_variables() {
    // Body has no free variables at all.
    let b = build(
        "5 def fnk(x)=x*2\n10 a=peek(1024):print a;fnk(a)\n20 a=peek(1025):print a;fnk(a)\n",
        Profile::Default,
    );
    assert!(
        b.asm.contains("A__1"),
        "an FN with no free vars must not block splitting A:\n{}",
        b.asm
    );
    let b = build(
        "5 def fnk(x)=x*b\n7 b=peek(53280)\n\
         10 a=peek(1024):print a;fnk(a)\n20 a=peek(1025):print a;fnk(a)\n",
        Profile::Default,
    );
    assert!(
        b.asm.contains("A__1"),
        "an FN reading only B must not block splitting A:\n{}",
        b.asm
    );
    assert!(
        !b.asm.contains("B__1"),
        "…while B itself stays unsplit:\n{}",
        b.asm
    );
}

/// The name is bound, then `CLR` throws the binding away.
#[test]
fn clr_undefines_a_statically_bound_fn() {
    for profile in PROFILES {
        let b = build(
            "10 def fnk(x)=x+1\n20 print fnk(1);\n30 clr\n40 print fnk(1)\n",
            profile,
        );
        assert!(
            b.asm.contains("__UNDEF_FN"),
            "{profile:?}: a CLR between DEF and call must reach the trap\n{}",
            b.asm
        );
        let r = emu64::run_prg_to_end(&b.prg, common::MAX_INSNS).expect("runs");
        assert_eq!(
            r.error_banner().as_deref(),
            Some("?UNDEF'D FUNCTION  ERROR IN 40"),
            "{profile:?}: output {:?}",
            common::petscii_to_string(&r.output)
        );
        assert_eq!(common::petscii_to_string(&r.output).trim(), "2");
    }
}

#[test]
fn a_fn_redefined_after_clr_works_again() {
    assert_rom_output(
        "\
10 def fnk(x)=x+1
20 print fnk(1);
30 clr
40 def fnk(x)=x+2
50 print fnk(1)
",
        " 2  3",
    );
}

/// `RUN <line>` performs a CLR on the way in, so it undefines too.
#[test]
fn run_undefines_a_statically_bound_fn() {
    let b = build(
        "10 def fnk(x)=x+1\n20 print fnk(1);\n30 z=z+1:if z>1 then end\n40 run 50\n50 print fnk(1)\n",
        Profile::Default,
    );
    assert!(
        b.asm.contains("__UNDEF_FN"),
        "a RUN between DEF and call must reach the trap\n{}",
        b.asm
    );
}

/// A call the `DEF` has not executed yet is `?UNDEF'D FUNCTION`, even though the `DEF` is right
/// there further down the listing.
#[test]
fn a_call_ahead_of_its_def_traps() {
    for profile in PROFILES {
        let b = build(
            "10 print \"a\";\n20 print fnk(1)\n30 def fnk(x)=x\n",
            profile,
        );
        let r = emu64::run_prg_to_end(&b.prg, common::MAX_INSNS).expect("runs");
        assert_eq!(
            r.error_banner().as_deref(),
            Some("?UNDEF'D FUNCTION  ERROR IN 20"),
            "{profile:?}: output {:?}",
            common::petscii_to_string(&r.output)
        );
        assert_eq!(common::petscii_to_string(&r.output).trim(), "A");
    }
}

#[test]
fn an_ordinary_fn_still_binds_statically() {
    let b = build("10 def fnk(x)=x*2\n20 print fnk(3)\n", Profile::Default);
    assert!(
        !b.asm.contains("__FNCALL_"),
        "a single unconditional DEF with no CLR must not be dispatched:\n{}",
        b.asm
    );
    assert!(
        !b.asm.contains("__UNDEF_FN"),
        "…and must not carry the trap:\n{}",
        b.asm
    );
}

#[test]
fn a_def_inside_a_loop_still_binds_statically() {
    let src = "10 for i=1 to 2\n20 def fnk(x)=x*i\n30 print fnk(3);\n40 next\n";
    assert_rom_output(src, " 3  6");
    let b = build(src, Profile::Default);
    assert!(
        !b.asm.contains("__FNCALL_"),
        "a loop-carried DEF ahead of its call must not need dispatch:\n{}",
        b.asm
    );
}

#[test]
fn several_functions_without_clr_all_bind_statically() {
    let b = build(
        "10 def fna(x)=x+1\n20 def fnb(y)=y*2\n30 print fna(1);fnb(3)\n",
        Profile::Default,
    );
    assert!(
        !b.asm.contains("__FNCALL_"),
        "no CLR, no redefinition — nothing should be dispatched:\n{}",
        b.asm
    );
}

/// The CLR gate is whole-module, so a `CLR` buried in a branch body counts even though the
/// branch may never be taken.
#[test]
fn a_clr_inside_a_branch_still_forces_dispatch() {
    let b = build(
        "10 def fnk(x)=x+1\n20 a=1\n30 if a then clr\n40 print fnk(1)\n",
        Profile::Default,
    );
    assert!(
        b.asm.contains("__FNCALL_") && b.asm.contains("__UNDEF_FN"),
        "a CLR in a branch body must still force the dispatch path:\n{}",
        b.asm
    );
}
