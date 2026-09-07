//! Multi-dimensional array subscript lowering.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn asm_of(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .asm
}

fn asm_of_profile(src: &str, profile: Profile) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        ..CompileOptions::default()
    };
    compile_with_options(&prg, opts).expect("compiles").asm
}
const FMULT: &str = "JSR $BA28";
const MOVMF: &str = "JSR $BBD4";

fn run(src: &str) -> emu64::RunToEnd {
    let prg = common::compile_source(src).expect("compiles");
    emu64::run_prg_to_end(&prg, 20_000_000).expect("runs")
}

/// Run and require a clean exit with exactly `expected` printed.
fn check_prints(src: &str, expected: &str) {
    let r = run(src);
    assert_eq!(r.error_banner(), None, "unexpected error for:\n{src}");
    assert!(r.clean_exit, "must reach END for:\n{src}");
    assert_eq!(
        common::petscii_to_string(&r.output).trim_end(),
        expected,
        "output mismatch for:\n{src}"
    );
}

/// Run and require the ROM error reporter to have printed `banner`.
fn check_stops_with(src: &str, banner: &str) {
    let r = run(src);
    assert_eq!(
        r.error_banner().as_deref(),
        Some(banner),
        "wrong BASIC error for:\n{src}"
    );
    assert!(
        !common::petscii_to_string(&r.output).contains("NOTREACHED"),
        "execution continued past the error for:\n{src}"
    );
}
#[test]
fn all_literal_multidim_subscript_folds_to_a_constant_address() {
    let asm = asm_of("10 DIM B(10,10)\n20 X=B(3,4)\n30 END\n");
    // (3*11 + 4) = element 37.
    assert!(
        asm.contains("LDA #<(__ARR_B_I+74)") && asm.contains("LDA #>(__ARR_B_I+74)"),
        "expected a folded constant address, got:\n{asm}"
    );
    assert!(
        !asm.contains(FMULT),
        "constant subscripts must not reach FMULT:\n{asm}"
    );
}
#[test]
fn constant_fold_scales_by_the_element_size() {
    let asm = asm_of("10 DIM B(10,10)\n20 B(3,4)=1.5\n30 PRINT B(3,4)\n40 END\n");
    // 37 elements * 5 bytes = 185.
    assert!(
        asm.contains("__ARR_B+185"),
        "expected a 5-byte-scaled constant address, got:\n{asm}"
    );
}
#[test]
fn float_scalar_multidim_subscript_keeps_the_horner_chain_native() {
    let asm = asm_of(concat!(
        "10 DIM B(10,10)\n",
        "20 POKE 679,3:POKE 680,4:P=PEEK(679):Q=PEEK(680)\n",
        "30 X=B(P,Q)\n",
        "40 END\n",
    ));
    assert!(
        !asm.contains(FMULT),
        "stride multiply must not reach FMULT:\n{asm}"
    );
    assert!(
        !asm.contains(MOVMF),
        "no MFLPT accumulator stash should survive:\n{asm}"
    );
}
#[test]
fn float_scalar_subscript_handles_a_non_power_of_two_stride() {
    // dims[1] = 4 => stride 5 => ShiftAddSelf { k: 2 }.
    let asm = asm_of(concat!(
        "10 DIM S(6,4)\n",
        "20 POKE 679,5:POKE 680,3:W=PEEK(679):V=PEEK(680)\n",
        "30 X=S(W,V)\n",
        "40 END\n",
    ));
    assert!(!asm.contains(FMULT), "expected a native stride:\n{asm}");
}
#[test]
fn runtime_dim_multidim_subscript_uses_the_16_bit_multiply() {
    let with_access = asm_of(concat!(
        "10 POKE 679,10:N=PEEK(679)\n",
        "20 DIM B(N,N)\n",
        "30 POKE 680,3:P=PEEK(680)\n",
        "40 X=B(P,P)\n",
        "50 END\n",
    ));
    assert!(
        with_access.contains("JSR __MUL16"),
        "expected the 16-bit stride multiply:\n{with_access}"
    );
    assert!(
        with_access.contains("_DIM_1"),
        "stride must come from the runtime DIM slot:\n{with_access}"
    );
    // A runtime `DIM` computes its own element count in FAC, so the program contains FMULT
    // whatever the access does.
    let without_access = asm_of(concat!(
        "10 POKE 679,10:N=PEEK(679)\n",
        "20 DIM B(N,N)\n",
        "30 POKE 680,3:P=PEEK(680)\n",
        "40 X=P\n",
        "50 END\n",
    ));
    assert_eq!(
        with_access.matches(FMULT).count(),
        without_access.matches(FMULT).count(),
        "the access itself must add no FMULT:\n{with_access}"
    );
}
#[test]
fn int_island_subscripts_still_take_the_fully_native_route() {
    let asm = asm_of(concat!(
        "10 DIM A%(7,7)\n",
        "20 FOR I=0 TO 7:FOR J=0 TO 7\n",
        "30 A%(I,J)=I*8+J\n",
        "40 NEXT J:NEXT I\n",
        "50 END\n",
    ));
    // GETADR ($B7F7) is the FAC->u16 conversion the hybrid needs; the fully-native route never
    // calls it.
    assert!(
        !asm.contains("JSR $B7F7"),
        "int-island subscripts must not convert through FAC:\n{asm}"
    );
    assert!(!asm.contains(FMULT), "and must not reach FMULT:\n{asm}");
}
#[test]
fn an_unlowerable_stride_still_uses_the_float_horner_chain() {
    // dims[1] = 22 => stride 23 = 16+4+2+1, four terms: past what `stride_lowering` covers.
    let asm = asm_of(concat!(
        "10 DIM B(2,22)\n",
        "20 POKE 679,1:POKE 680,7:P=PEEK(679):Q=PEEK(680)\n",
        "30 X=B(P,Q)\n",
        "40 END\n",
    ));
    assert!(
        asm.contains(FMULT),
        "unlowerable stride should still use FMULT:\n{asm}"
    );
}
#[test]
fn the_fast_routes_fire_in_every_profile() {
    let src = concat!(
        "10 DIM B(10,10)\n",
        "20 POKE 679,3:POKE 680,4:P=PEEK(679):Q=PEEK(680)\n",
        "30 X=B(P,Q):Y=B(3,4)\n",
        "40 END\n",
    );
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let asm = asm_of_profile(src, profile);
        assert!(
            !asm.contains(FMULT),
            "{profile:?} lost the native stride chain:\n{asm}"
        );
        assert!(
            asm.contains("__ARR_B_I+74"),
            "{profile:?} lost the constant fold:\n{asm}"
        );
    }
}

// Behavioural: the addresses the routes compute are correct.

#[test]
fn constant_and_variable_subscripts_address_the_same_element() {
    check_prints(
        concat!(
            "10 DIM B(10,10)\n",
            "20 FOR I=0 TO 10:FOR J=0 TO 10:B(I,J)=I*11+J:NEXT J:NEXT I\n",
            "30 POKE 679,3:POKE 680,4:P=PEEK(679):Q=PEEK(680)\n",
            "40 PRINT B(3,4);B(P,Q);B(10,10)\n",
            "50 END\n",
        ),
        " 37  37  120",
    );
}

#[test]
fn three_dimensional_subscripts_are_row_major() {
    check_prints(
        concat!(
            "10 DIM Z(2,3,4)\n",
            "20 FOR I=0 TO 2:FOR J=0 TO 3:FOR K=0 TO 4\n",
            "30 Z(I,J,K)=I*100+J*10+K\n",
            "40 NEXT K:NEXT J:NEXT I\n",
            "50 POKE 679,1:POKE 680,2:POKE 681,3\n",
            "60 P=PEEK(679):Q=PEEK(680):R=PEEK(681)\n",
            "70 PRINT Z(2,3,4);Z(P,Q,R);Z(0,0,0)\n",
            "80 END\n",
        ),
        " 234  123  0",
    );
}

#[test]
fn runtime_dimmed_arrays_address_correctly() {
    check_prints(
        concat!(
            "10 POKE 679,10:N=PEEK(679)\n",
            "20 DIM B(N,N)\n",
            "30 FOR I=0 TO 10:FOR J=0 TO 10:B(I,J)=I*11+J:NEXT J:NEXT I\n",
            "40 POKE 680,3:POKE 681,4:P=PEEK(680):Q=PEEK(681)\n",
            "50 PRINT B(3,4);B(P,Q);B(10,10)\n",
            "60 END\n",
        ),
        " 37  37  120",
    );
}

#[test]
fn runtime_dimmed_three_dimensional_arrays_address_correctly() {
    check_prints(
        concat!(
            "10 POKE 679,2:POKE 680,3:POKE 681,4\n",
            "20 A=PEEK(679):B=PEEK(680):C=PEEK(681)\n",
            "30 DIM Z(A,B,C)\n",
            "40 FOR I=0 TO 2:FOR J=0 TO 3:FOR K=0 TO 4\n",
            "50 Z(I,J,K)=I*100+J*10+K\n",
            "60 NEXT K:NEXT J:NEXT I\n",
            "70 PRINT Z(2,3,4);Z(1,2,3);Z(0,0,0)\n",
            "80 END\n",
        ),
        " 234  123  0",
    );
}

/// An integer array reference is itself int-island, so it can appear *inside* a multi-dim
/// subscript and re-enter `emit_array_address`.
#[test]
fn a_nested_array_reference_in_a_subscript_keeps_the_outer_accumulator() {
    check_prints(
        concat!(
            "10 DIM A%(3,3),B%(3,3)\n",
            "20 B%(1,1)=2:B%(1,2)=3\n",
            "30 A%(2,3)=42\n",
            "40 PRINT A%(B%(1,1),B%(1,2))\n",
            "50 END\n",
        ),
        " 42",
    );
}

#[test]
fn a_nested_float_array_reference_in_a_subscript_works_too() {
    check_prints(
        concat!(
            "10 DIM A(3,3),B(3,3)\n",
            "20 B(1,1)=2:B(1,2)=3\n",
            "30 A(2,3)=42\n",
            "40 PRINT A(B(1,1),B(1,2))\n",
            "50 END\n",
        ),
        " 42",
    );
}
#[test]
fn fractional_subscripts_are_truncated() {
    check_prints(
        concat!(
            "10 DIM B(3,3)\n",
            "20 P=1.7:Q=2.9\n",
            "30 B(1,2)=99\n",
            "40 PRINT B(P,Q)\n",
            "50 END\n",
        ),
        " 99",
    );
}

#[test]
fn string_arrays_index_multidimensionally() {
    check_prints(
        concat!(
            "10 DIM S$(2,2)\n",
            "20 S$(1,2)=\"HI\"\n",
            "30 POKE 679,1:POKE 680,2:P=PEEK(679):Q=PEEK(680)\n",
            "40 PRINT LEN(S$(P,Q));ASC(S$(P,Q))\n",
            "50 END\n",
        ),
        " 2  200",
    );
}

// The ?BAD SUBSCRIPT path, per route, per dimension.

#[test]
fn literal_subscripts_still_raise_bad_subscript_out_of_range() {
    // Dimension 0 out of range.
    check_stops_with(
        "10 DIM B(3,3)\n20 X=B(5,1)\n30 PRINT \"NOTREACHED\"\n",
        "?BAD SUBSCRIPT  ERROR IN 20",
    );
    check_stops_with(
        "10 DIM B(3,3)\n20 X=B(1,5)\n30 PRINT \"NOTREACHED\"\n",
        "?BAD SUBSCRIPT  ERROR IN 20",
    );
    // On the store side as well.
    check_stops_with(
        "10 DIM B(3,3)\n20 B(1,5)=7\n30 PRINT \"NOTREACHED\"\n",
        "?BAD SUBSCRIPT  ERROR IN 20",
    );
}

#[test]
fn a_negative_literal_subscript_is_illegal_quantity() {
    check_stops_with(
        "10 DIM B(3,3)\n20 X=B(-1,1)\n30 PRINT \"NOTREACHED\"\n",
        "?ILLEGAL QUANTITY  ERROR IN 20",
    );
    check_stops_with(
        "10 DIM B(3,3)\n20 X=B(1,-1)\n30 PRINT \"NOTREACHED\"\n",
        "?ILLEGAL QUANTITY  ERROR IN 20",
    );
}

#[test]
fn float_scalar_subscripts_still_raise_bad_subscript_out_of_range() {
    check_stops_with(
        concat!(
            "10 DIM B(3,3)\n",
            "20 POKE 679,5:P=PEEK(679)\n",
            "30 X=B(P,1)\n",
            "40 PRINT \"NOTREACHED\"\n",
        ),
        "?BAD SUBSCRIPT  ERROR IN 30",
    );
    check_stops_with(
        concat!(
            "10 DIM B(3,3)\n",
            "20 POKE 679,5:P=PEEK(679)\n",
            "30 X=B(1,P)\n",
            "40 PRINT \"NOTREACHED\"\n",
        ),
        "?BAD SUBSCRIPT  ERROR IN 30",
    );
}

#[test]
fn a_negative_float_scalar_subscript_is_illegal_quantity() {
    check_stops_with(
        "10 DIM B(3,3)\n20 P=0-1\n30 X=B(1,P)\n40 PRINT \"NOTREACHED\"\n",
        "?ILLEGAL QUANTITY  ERROR IN 30",
    );
    check_stops_with(
        "10 DIM B(3,3)\n20 P=0-1\n30 X=B(P,1)\n40 PRINT \"NOTREACHED\"\n",
        "?ILLEGAL QUANTITY  ERROR IN 30",
    );
}

#[test]
fn runtime_dimmed_arrays_still_raise_bad_subscript_out_of_range() {
    check_stops_with(
        concat!(
            "10 POKE 679,3:N=PEEK(679)\n",
            "20 DIM B(N,N)\n",
            "30 POKE 680,5:P=PEEK(680)\n",
            "40 X=B(P,1)\n",
            "50 PRINT \"NOTREACHED\"\n",
        ),
        "?BAD SUBSCRIPT  ERROR IN 40",
    );
    check_stops_with(
        concat!(
            "10 POKE 679,3:N=PEEK(679)\n",
            "20 DIM B(N,N)\n",
            "30 POKE 680,5:P=PEEK(680)\n",
            "40 X=B(1,P)\n",
            "50 PRINT \"NOTREACHED\"\n",
        ),
        "?BAD SUBSCRIPT  ERROR IN 40",
    );
    check_stops_with(
        concat!(
            "10 POKE 679,3:N=PEEK(679)\n",
            "20 DIM B(N,N)\n",
            "30 P=0-1\n",
            "40 X=B(1,P)\n",
            "50 PRINT \"NOTREACHED\"\n",
        ),
        "?ILLEGAL QUANTITY  ERROR IN 40",
    );
}

/// Dimension order.
#[test]
fn the_first_failing_dimension_is_the_one_reported() {
    for src in [
        // Literal route.
        "10 DIM B(3,3)\n20 X=B(5,-1)\n30 PRINT \"NOTREACHED\"\n",
        // Float-scalar route.
        concat!(
            "10 DIM B(3,3)\n",
            "20 P=0-1:POKE 679,5:Q=PEEK(679)\n",
            "30 X=B(Q,P)\n",
            "40 PRINT \"NOTREACHED\"\n",
        ),
    ] {
        let r = run(src);
        assert!(
            r.error_banner()
                .as_deref()
                .is_some_and(|b| b.starts_with("?BAD SUBSCRIPT")),
            "expected the dimension-0 failure to win for:\n{src}\ngot {:?}",
            r.error_banner()
        );
    }
    check_stops_with(
        "10 DIM B(3,3)\n20 X=B(-1,5)\n30 PRINT \"NOTREACHED\"\n",
        "?ILLEGAL QUANTITY  ERROR IN 20",
    );
}

/// `CLR` un-DIMs a statically allocated array, dropping its effective bound to the BASIC v2
/// auto-DIM default of 10.
#[test]
fn the_constant_fold_respects_the_post_clr_bound() {
    check_prints(
        "10 DIM B(20,20)\n20 B(15,15)=5\n30 PRINT B(15,15)\n40 END\n",
        " 5",
    );
    check_stops_with(
        concat!(
            "10 DIM B(20,20)\n",
            "20 B(15,15)=5\n",
            "30 CLR\n",
            "40 X=B(15,15)\n",
            "50 PRINT \"NOTREACHED\"\n",
        ),
        "?BAD SUBSCRIPT  ERROR IN 40",
    );
}
#[test]
fn a_runtime_array_used_before_dim_is_still_bad_subscript() {
    check_stops_with(
        concat!(
            "10 POKE 679,5:N=PEEK(679)\n",
            "20 IF N=5 THEN 40\n",
            "30 DIM B(N,N)\n",
            "40 X=B(1,1)\n",
            "50 PRINT \"NOTREACHED\"\n",
        ),
        "?BAD SUBSCRIPT  ERROR IN 40",
    );
}

// The Speed profile and the whole-program sequence outliner.

#[test]
fn the_runtime_md_subscript_chain_is_outlined_on_speed() {
    let src = concat!(
        "10 POKE 679,8:PP=PEEK(679)\n",
        "20 DIM B(PP,20,5)\n",
        "30 P=1:J=2:K=3\n",
        "40 B(P,J,K)=1:B(J,P,K)=2:B(K,J,P)=3\n",
        "50 T=B(P,J,K)+B(J,P,K)+B(K,J,P)\n",
        "60 PRINT T\n",
        "70 END\n",
    );
    for profile in [Profile::Default, Profile::Speed] {
        let asm = asm_of_profile(src, profile);
        assert!(
            asm.contains("__SEQ_") || asm.contains("__JSR2_"),
            "{profile:?}: subscript chain must be factored, not inlined per site"
        );
    }
}

/// The factored addresses are the same addresses.
#[test]
fn outlined_md_addresses_agree_across_profiles() {
    let src = concat!(
        "10 POKE 679,8:PP=PEEK(679)\n",
        "20 DIM B(PP,20,5)\n",
        "30 FOR P=1 TO 3:FOR J=1 TO 4:FOR K=0 TO 5\n",
        "40 B(P,J,K)=P*100+J*10+K\n",
        "50 NEXT K:NEXT J:NEXT P\n",
        "60 T=0\n",
        "70 FOR P=1 TO 3:FOR J=1 TO 4:FOR K=0 TO 5\n",
        "80 T=T+B(P,J,K)\n",
        "90 NEXT K:NEXT J:NEXT P\n",
        "100 PRINT T\n",
        "110 END\n",
    );
    // sum over P=1..3, J=1..4, K=0..5 of P*100+J*10+K = 6*4*(100+200+300) + 6*3*(10+20+30+40) +
    // 3*4*(0+1+2+3+4+5) = 14400 + 1800 + 180 = 16380
    let want = "16380";
    let mut seen: Vec<(Profile, String)> = Vec::new();
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let prg = tokenize_program(src).expect("tokenizes");
        let opts = CompileOptions {
            profile,
            ..CompileOptions::default()
        };
        let img = compile_with_options(&prg, opts)
            .unwrap_or_else(|e| panic!("{profile:?} compiles: {e}"))
            .prg_bytes;
        let r = emu64::run_prg_to_end(&img, 20_000_000).expect("runs");
        assert!(r.clean_exit, "{profile:?} must reach END");
        let out = common::petscii_to_string(&r.output).trim_end().to_string();
        assert!(
            out.split_whitespace().any(|w| w == want),
            "{profile:?}: expected {want}, got {out:?}"
        );
        seen.push((profile, out));
    }
    let (_, first) = &seen[0];
    for (p, out) in &seen[1..] {
        assert_eq!(first, out, "{p:?} disagrees with Default");
    }
}
#[test]
fn the_int_island_md_route_survives_outlining_on_speed() {
    let src = concat!(
        "10 DIM B%(8,8)\n",
        "20 FOR I=1 TO 8:FOR J=1 TO 8\n",
        "30 B%(I,J)=I+J\n",
        "40 NEXT J:NEXT I\n",
        "50 PRINT B%(3,4)\n",
        "60 END\n",
    );
    let asm = asm_of_profile(src, Profile::Speed);
    assert!(
        !asm.contains(FMULT) && !asm.contains(MOVMF),
        "int-island route must not fall back to the float Horner chain:\n{asm}"
    );
    let prg = common::compile_source(src).expect("compiles");
    let r = emu64::run_prg_to_end(&prg, 20_000_000).expect("runs");
    assert!(r.clean_exit, "must reach END");
    assert_eq!(
        common::petscii_to_string(&r.output).trim_end(),
        " 7",
        "B%(3,4) must be 7"
    );
}
