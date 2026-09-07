//! Dynamic FOR limits written with integer literals retain fast counters, including calls into a shared delay routine.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn check(src: &str, expected: &str, integer_counter: bool) {
    check_named(src, expected, Some(integer_counter), "K");
}

fn check_named(src: &str, expected: &str, integer_counter: Option<bool>, counter: &str) {
    let prg = tokenize_program(src).expect("tokenizes");
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let compiled = compile_with_options(
                &prg,
                CompileOptions {
                    profile,
                    extraram,
                    force_extraram_off: !extraram,
                    ..CompileOptions::default()
                },
            )
            .expect("compiles");
            if let Some(integer_counter) = integer_counter {
                assert_eq!(
                    compiled.asm.contains(&format!("VI_{counter}")),
                    integer_counter,
                    "counter storage: {profile:?}, extraram={extraram}\n{src}"
                );
            }
            if integer_counter == Some(true) && src.contains("FOR K=1 TO XX:NEXT:RETURN") {
                assert!(
                    compiled.asm.contains("INC VI_K"),
                    "literal-bound proof must reach NEXT's integer increment"
                );
            }
            assert_eq!(
                common::run_clean(&compiled.prg_bytes).as_deref(),
                Some(expected),
                "execution: {profile:?}, extraram={extraram}\n{src}"
            );
        }
    }
}

#[test]
fn shared_delay_subroutine_uses_integer_counter_for_every_literal_limit() {
    check(
        "10 XX=15:GOSUB100:PRINT K\n20 XX=5000:GOSUB100:PRINT K\n30 END\n100 FOR K=1 TO XX:NEXT:RETURN\n",
        " 16 \n 5001 \n",
        true,
    );
}

#[test]
fn a_call_before_the_limit_is_assigned_observes_basic_default_zero() {
    check(
        "10 GOSUB100:PRINT K\n20 XX=300:GOSUB100:PRINT K\n25 XX=400:GOSUB100:PRINT K\n30 END\n100 FOR K=1 TO XX:NEXT:RETURN\n",
        " 2 \n 301 \n 401 \n",
        true,
    );
}

#[test]
fn clearing_variables_restores_the_zero_covered_by_the_bound() {
    check(
        "10 XX=300:GOSUB100:PRINT K\n20 CLR:GOSUB100:PRINT K\n30 END\n100 FOR K=1 TO XX:NEXT:RETURN\n",
        " 301 \n 2 \n",
        true,
    );
}

#[test]
fn negative_literal_limits_keep_signed_comparisons() {
    check(
        "10 XX=-3:GOSUB100:PRINT K\n20 XX=-300:GOSUB100:PRINT K\n30 END\n100 FOR K=-1 TO XX STEP -1:NEXT:RETURN\n",
        "-4 \n-301 \n",
        true,
    );
}

#[test]
fn limit_changes_in_body_do_not_change_the_saved_limit() {
    check(
        "10 XX=300:GOSUB100:PRINT K;XX\n20 END\n100 FOR K=1 TO XX:XX=2:NEXT:RETURN\n",
        " 301  2 \n",
        true,
    );
}

#[test]
fn a_fractional_limit_anywhere_keeps_source_float_counter() {
    check(
        "10 XX=2.5:GOSUB100:PRINT K\n20 XX=3:GOSUB100:PRINT K\n30 END\n100 FOR K=1 TO XX:NEXT:RETURN\n",
        " 3 \n 4 \n",
        false,
    );
}

#[test]
fn post_loop_value_outside_i16_keeps_source_float_counter() {
    check(
        "10 XX=32767:GOSUB100:PRINT K\n20 END\n100 FOR K=32766 TO XX:NEXT:RETURN\n",
        " 32768 \n",
        false,
    );
}

#[test]
fn input_definition_does_not_gain_a_literal_bound_proof() {
    check(
        "10 READ XX:GOSUB100:PRINT K\n20 XX=3:GOSUB100:PRINT K\n30 END\n100 FOR K=1 TO XX:NEXT:RETURN\n200 DATA 2.5\n",
        " 3 \n 4 \n",
        false,
    );
}

#[test]
fn counter_modified_by_body_retains_the_float_value_domain() {
    check(
        "10 XX=300:GOSUB100:PRINT K\n20 END\n100 FOR K=1 TO XX:K=40000:NEXT:RETURN\n",
        " 40001 \n",
        false,
    );
}

#[test]
fn next_before_for_uses_the_active_dynamic_bound_frame() {
    check(
        "10 XX=3:GOTO100\n20 NEXT:PRINT K:END\n100 FOR K=1 TO XX\n110 PRINT K;:GOTO20\n",
        " 1  2  3  4 \n",
        true,
    );
}

#[test]
fn shared_next_switches_between_dynamic_bound_for_headers() {
    check(
        "10 XX=2:GOTO100\n20 NEXT:IF XX=2 THEN XX=4:GOTO200\n30 PRINT K:END\n100 FOR K=1 TO XX\n110 PRINT K;:GOTO20\n200 FOR K=3 TO XX\n210 PRINT K;:GOTO20\n",
        " 1  2  3  4  5 \n",
        true,
    );
}

#[test]
fn runtime_next_dispatch_handles_renamed_and_descending_dynamic_loops() {
    let shapes = [
        (
            "10 XX=-2:GOTO100\n20 NEXT:IF XX=-2 THEN XX=-4:GOTO200\n30 PRINT K:END\n100 FOR K=-1 TO XX STEP -1\n110 PRINT K;:GOTO20\n200 FOR K=-3 TO XX STEP -1\n210 PRINT K;:GOTO20\n",
            "-1 -2 -3 -4 -5 \n",
        ),
        (
            "10 XX=4:GOTO100\n20 NEXT:PRINT K:END\n30 NEXT:PRINT K:END\n100 FOR K=1 TO XX\n110 XX=3:PRINT K;:ON (K AND 1)+1 GOTO20,30\n",
            " 1  2  3  4  5 \n",
        ),
        (
            "10 XX=-4:GOTO100\n20 NEXT:PRINT K:END\n30 NEXT:PRINT K:END\n100 FOR K=-1 TO XX STEP -1\n110 XX=-3:PRINT K;:ON (K AND 1)+1 GOTO20,30\n",
            "-1 -2 -3 -4 -5 \n",
        ),
    ];
    for name in ["J", "Q", "AB"] {
        for (src, expected) in shapes {
            check_named(&src.replace('K', name), expected, None, name);
        }
    }
}

#[test]
fn dynamic_outer_next_discards_an_inner_frame_and_preserves_its_counter() {
    check(
        "10 XX=2:YY=4:GOTO100\n20 NEXT K:PRINT K;J:END\n100 FOR K=1 TO XX:FOR J=3 TO YY\n110 XX=3:YY=5:PRINT K;J;:GOTO20\n",
        " 1  3  2  3  3  3 \n",
        true,
    );
}

#[test]
fn a_dispatched_counter_can_move_away_from_its_for_limit() {
    for (start, end, step, bump, expected) in [
        (32700, -32766, -1, "+2", " 32781  80 \n"),
        (-32700, 32766, 1, "-2", "-32781  80 \n"),
    ] {
        let src = format!(
            "10 XX={end}:S=0:GOTO100\n20 NEXT K:GOTO300\n100 FOR K={start} TO XX STEP {step}\n110 K=K{bump}:S=S+1:IF S=80 THEN200\n120 GOTO20\n200 PRINT K;S:END\n300 XX={}:GOTO100\n",
            end - step,
        );
        check(&src, expected, false);
    }
}

#[test]
fn a_lexical_next_does_not_bound_counter_writes_outside_its_textual_body() {
    let src = "10 POKE828,0:XX=-32766:S=0:GOTO100\n20 NEXT K:GOTO300\n100 FOR K=32700 TO XX STEP -1\n105 IF PEEK(828)=255 THEN115\n110 GOTO150\n115 NEXT K:GOTO300\n150 K=K+2:S=S+1:IF S=80 THEN200\n160 GOTO20\n200 PRINT K;S:END\n300 XX=-32765:GOTO100\n";
    check(src, " 32781  80 \n", false);
}

#[test]
fn an_external_bump_towards_the_limit_keeps_its_shared_integer_home() {
    let src = "10 FOR K=1 TO 6\n20 GOTO100\n30 PRINT K;\n40 NEXT K\n50 PRINT K:END\n100 K=K+2\n110 GOTO30\n";
    check(src, " 3  6  7 \n", true);
}

#[test]
fn a_counter_recurrence_that_can_bypass_next_has_no_for_bound() {
    let src = "10 XX=32760:S=0:GOTO100\n20 NEXT K:END\n100 FOR K=32700 TO XX\n110 K=K+2:S=S+1:IF S=80 THEN200\n120 GOTO110\n200 PRINT K;S:END\n300 XX=32759:GOTO100\n";
    check(src, " 32860  80 \n", false);
}

#[test]
fn a_conditional_header_observes_mutations_outside_its_lexical_body() {
    let src = "10 POKE828,0:XX=-32766:S=0:GOTO100\n20 NEXT K:GOTO300\n100 IF PEEK(828)=0 THEN FOR K=32700 TO XX STEP -1\n105 IF PEEK(828)=255 THEN115\n110 GOTO150\n115 NEXT K:GOTO300\n150 K=K+2:S=S+1:IF S=80 THEN200\n160 GOTO20\n200 PRINT K;S:END\n300 XX=-32765:GOTO100\n";
    check(src, " 32781  80 \n", false);
}

#[test]
fn a_conditional_header_preserves_the_float_exit_value() {
    let src = "10 POKE828,0\n20 IF PEEK(828)=0 THEN FOR K=32767 TO 32767\n30 NEXT K\n40 PRINT K\n";
    check(src, " 32768 \n", false);
}

#[test]
fn a_mutable_inline_counter_has_one_shared_float_home() {
    let src =
        "10 POKE828,0\n20 IF PEEK(828)=0 THEN FOR K=1 TO3:K=K+.5:PRINT K:NEXT K\n30 PRINT K\n";
    check(src, " 1.5 \n 3 \n 4 \n", false);
}

fn check_private_counter(src: &str, expected: &str, private_integer: bool) {
    let prg = tokenize_program(src).expect("tokenizes");
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let compiled = compile_with_options(
                &prg,
                CompileOptions {
                    profile,
                    extraram,
                    force_extraram_off: !extraram,
                    ..CompileOptions::default()
                },
            )
            .expect("compiles");
            assert!(
                !compiled.asm.contains("VI_K"),
                "source counter must stay Float: {profile:?}, extraram={extraram}\n{src}"
            );
            assert_eq!(
                compiled.asm.contains("FI_") || compiled.asm.contains("FU_"),
                private_integer,
                "private counter: {profile:?}, extraram={extraram}\n{src}"
            );
            assert_eq!(
                common::run_clean(&compiled.prg_bytes).as_deref(),
                Some(expected)
            );
        }
    }
}

#[test]
fn writes_before_every_for_header_keep_fast_integer_counters() {
    check_private_counter(
        "10 READ K\n20 FOR K=1 TO500:NEXT K\n30 PRINT K\n40 DATA0\n",
        " 501 \n",
        true,
    );
    // Constant folding can remove the fractional home entirely; that existing shared-integer counter must remain fast too.
    check(
        "10 K=.5:PRINT K\n20 FOR K=1 TO500:NEXT K\n30 PRINT K\n",
        " .5 \n 501 \n",
        true,
    );
}

#[test]
fn a_goto_to_an_earlier_read_requires_the_live_float_counter() {
    let src = "10 GOTO100\n20 READ K:GOTO120\n100 FOR K=1 TO3\n110 GOTO20\n120 PRINT K:NEXT K\n130 PRINT K:END\n200 DATA1.5,3\n";
    check_private_counter(src, " 1.5 \n 3 \n 4 \n", false);
}

#[test]
fn a_goto_to_a_post_next_write_still_mutates_the_live_counter() {
    let src = "10 FOR K=1 TO3\n20 GOTO60\n30 PRINT K:NEXT K\n40 PRINT K:END\n60 K=K+.5:GOTO30\n";
    check_private_counter(src, " 1.5 \n 3 \n 4 \n", false);
}

#[test]
fn writes_after_a_static_loop_completes_keep_its_private_counter() {
    for (src, expected) in [
        (
            "10 FOR K=1 TO3\n20 PRINT K\n30 NEXT K\n40 K=K/2:PRINT K\n",
            " 1 \n 2 \n 3 \n 2 \n",
        ),
        (
            "10 FOR K=1 TO4\n20 IF K=3 THEN40\n30 PRINT K\n40 NEXT K\n50 K=K/2:PRINT K\n",
            " 1 \n 2 \n 4 \n 2.5 \n",
        ),
    ] {
        check_private_counter(src, expected, true);
    }
}
