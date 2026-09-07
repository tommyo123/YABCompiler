

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(src: &str, profile: Profile) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).unwrap_or_else(|e| panic!("tokenize: {e:?}"));
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compile: {e}"))
}

/// `(clean_exit, decoded PRINT output)`.
fn run(prg: &[u8]) -> (bool, String) {
    let r = emu64::run_prg_to_end(prg, common::MAX_INSNS).expect("emu64 run");
    (r.clean_exit, common::petscii_to_string(&r.output))
}

/// Does the assembly *define* `label` (as opposed to merely mentioning a longer label with the
/// same prefix)?
fn defines(asm: &str, label: &str) -> bool {
    asm.lines().any(|l| l.trim_end() == format!("{label}:"))
}

fn reuse_sites(asm: &str) -> usize {
    asm.lines()
        .filter(|l| l.contains("; reuse ARRAY_ADDR"))
        .count()
}

/// ROM: ` 4  5`.
const A1_BOTH_NAMES: &str = "10 A(0)=4:A%(0)=5\n\
                             20 PRINT A(0);A%(0)\n";
const A1_SEPARATE_BOUNDS: &str = "10 DIM A%(2)\n\
                                  20 A(5)=1\n\
                                  30 PRINT A%(0)\n";
const A1_FETCH_TWIN: &str = "10 A(0)=4\n\
                             20 FETCH \"N\",2,A%(0)\n\
                             30 PRINT A(0)\n";
const A1_STILL_PROMOTES: &str = "10 DIM A(5)\n\
                                 20 A(0)=3\n\
                                 30 A(1)=A(0)\n\
                                 40 PRINT A(1)\n";
const A1_STRING_TWIN_OK: &str = "10 DIM A(5)\n\
                                 20 A$(0)=\"Q\"\n\
                                 30 A(0)=3\n\
                                 40 PRINT A(0);A$(0)\n";

#[test]
fn float_and_int_arrays_keep_separate_storage() {
    for p in PROFILES {
        let b = build(A1_BOTH_NAMES, p);
        assert!(
            defines(&b.asm, "__ARR_A") && defines(&b.asm, "__ARR_A_I"),
            "{p:?}: A() and A%() must get two allocations"
        );
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: clean exit");
        assert_eq!(out.trim_end(), " 4  5", "{p:?}");
    }
}

#[test]
fn int_array_dim_does_not_bound_the_float_array() {
    for p in PROFILES {
        let b = build(A1_SEPARATE_BOUNDS, p);
        assert!(
            defines(&b.asm, "__ARR_A") && defines(&b.asm, "__ARR_A_I"),
            "{p:?}: two allocations"
        );
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: A(5) must not raise ?BAD SUBSCRIPT");
        assert_eq!(out.trim_end(), " 0", "{p:?}");
    }
}

#[test]
fn fetch_element_target_counts_as_a_percent_twin() {
    for p in PROFILES {
        let b = build(A1_FETCH_TWIN, p);
        assert!(
            defines(&b.asm, "__ARR_A"),
            "{p:?}: A() must stay float — A%() exists as a FETCH target"
        );
        assert!(
            defines(&b.asm, "__ARR_A_I"),
            "{p:?}: the FETCH target A%() auto-DIMs to its own allocation"
        );
    }
}

#[test]
fn integer_only_array_still_promotes() {
    for p in PROFILES {
        let b = build(A1_STILL_PROMOTES, p);
        assert!(
            defines(&b.asm, "__ARR_A_I"),
            "{p:?}: promotion must still fire"
        );
        assert!(
            !defines(&b.asm, "__ARR_A"),
            "{p:?}: no float allocation should survive"
        );
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: clean exit");
        assert_eq!(out.trim_end(), " 3", "{p:?}");
    }
}

#[test]
fn string_twin_does_not_block_promotion() {
    for p in PROFILES {
        let b = build(A1_STRING_TWIN_OK, p);
        assert!(
            defines(&b.asm, "__ARR_A_I"),
            "{p:?}: A$() is a different namespace; A() must still promote"
        );
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: clean exit");
        assert_eq!(out.trim_end(), " 3 Q", "{p:?}");
    }
}

/// `A(1, B(C(0),C(0)))` = `A(1, B(2,2))` = `A(1,2)` = 7.
const A2_NESTED: &str = "10 DIM A(3,3),B(3,3),C(3)\n\
                         20 C(0)=2:C(1)=.5\n\
                         30 B(2,2)=2:B(3,3)=.5\n\
                         40 A(1,2)=7:A(2,2)=33:A(3,3)=.5\n\
                         50 PRINT A(1,B(C(0),C(0)))\n";

/// Two levels of nesting: `A(1, B(1, C(E(0),E(0))))` = `A(1, B(1,2))` = `A(1,2)` = 7.
const A2_NESTED_TWICE: &str = "10 DIM A(3,3),B(3,3),C(3,3),E(3)\n\
                               20 E(0)=2:E(1)=.5\n\
                               30 C(2,2)=2:C(3,3)=.5\n\
                               40 B(1,2)=2:B(2,2)=1:B(3,3)=.5\n\
                               50 A(1,2)=7:A(2,2)=33:A(1,1)=44:A(3,3)=.5\n\
                               60 PRINT A(1,B(1,C(E(0),E(0))))\n";
const A2_NESTED_FIRST: &str = "10 DIM A(3,3),D(4,4),E(3)\n\
                               20 E(0)=2:E(1)=.5\n\
                               30 D(2,2)=2\n\
                               40 A(1,2)=7:A(2,2)=33:A(3,3)=.5\n\
                               50 PRINT A(E(0)-1,D(2,2))\n";
const A2_UNNESTED: &str = "10 DIM A(3,3),C(3)\n\
                           20 C(0)=2:C(1)=.5\n\
                           30 A(1,2)=7:A(3,3)=.5\n\
                           40 PRINT A(1,C(0))\n";

#[test]
fn nested_multidim_subscript_gets_its_own_scratch() {
    for p in PROFILES {
        let b = build(A2_NESTED, p);
        assert!(
            defines(&b.asm, "__MD_TMP_I1") && defines(&b.asm, "__MD_TMP_J1"),
            "{p:?}: the inner reference needs a depth-1 pair"
        );
        assert!(
            !defines(&b.asm, "__MD_TMP_I2"),
            "{p:?}: and only as deep as it actually nests"
        );
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: clean exit");
        assert_eq!(out.trim_end(), " 7", "{p:?}");
    }
}

#[test]
fn doubly_nested_subscript_gets_two_extra_pairs() {
    for p in PROFILES {
        let b = build(A2_NESTED_TWICE, p);
        assert!(
            defines(&b.asm, "__MD_TMP_I2") && defines(&b.asm, "__MD_TMP_J2"),
            "{p:?}: two levels of nesting need a depth-2 pair"
        );
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: clean exit");
        assert_eq!(out.trim_end(), " 7", "{p:?}");
    }
}

#[test]
fn nested_reference_in_first_subscript_is_correct() {
    for p in PROFILES {
        let b = build(A2_NESTED_FIRST, p);
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: clean exit");
        assert_eq!(out.trim_end(), " 7", "{p:?}");
    }
}

#[test]
fn unnested_multidim_keeps_the_depth_zero_pair() {
    for p in PROFILES {
        let b = build(A2_UNNESTED, p);
        assert!(
            defines(&b.asm, "__MD_TMP_I"),
            "{p:?}: the FAC Horner path must still be in use"
        );
        assert!(
            !defines(&b.asm, "__MD_TMP_I1") && !defines(&b.asm, "__MD_TMP_J1"),
            "{p:?}: no dead depth-1 pair may be allocated"
        );
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: clean exit");
        assert_eq!(out.trim_end(), " 7", "{p:?}");
    }
}

/// `__STR_PRINT` stages its source pointer in `$FD/$FE`.
const A3_PROMPT: &str = "10 A(1)=5.5\n\
                         20 INPUT \"V\";X\n\
                         30 PRINT A(1);X\n";
const A3_NO_PROMPT: &str = "10 A(1)=5.5\n\
                            20 INPUT X\n\
                            30 PRINT A(1);X\n";
const A3_ADJACENT: &str = "10 A(1)=5.5\n\
                           20 A(1)=A(1)+1\n\
                           30 PRINT A(1)\n";
const A3_STRING_INPUT: &str = "10 A(1)=5.5\n\
                               20 INPUT X$\n\
                               30 PRINT A(1)\n";

#[test]
fn input_prompt_invalidates_the_array_address_cache() {
    for p in PROFILES {
        let b = build(A3_PROMPT, p);
        assert_eq!(
            reuse_sites(&b.asm),
            0,
            "{p:?}: __STR_PRINT clobbered $FD/$FE — no address may be reused across it"
        );
    }
}

#[test]
fn prompt_less_input_keeps_the_array_address_cache() {
    for p in PROFILES {
        let b = build(A3_NO_PROMPT, p);
        assert_eq!(
            reuse_sites(&b.asm),
            1,
            "{p:?}: nothing in a bare INPUT touches $FD/$FE"
        );
    }
}

#[test]
fn string_input_invalidates_the_array_address_cache() {
    for p in PROFILES {
        let b = build(A3_STRING_INPUT, p);
        assert_eq!(
            reuse_sites(&b.asm),
            0,
            "{p:?}: the heap copy behind INPUT X$ overwrites $FD/$FE"
        );
    }
}

#[test]
fn adjacent_element_operations_still_share_one_address() {
    for p in PROFILES {
        let b = build(A3_ADJACENT, p);
        assert!(
            reuse_sites(&b.asm) >= 2,
            "{p:?}: read-modify-write must not recompute the address each time"
        );
    }
}

/// emu64 traps `$FFE4` and returns "no key", which is exactly BASIC v2's non-blocking `GET`:
/// the element becomes 0.
const A3_GET_ARRAY: &str = "10 DIM B(5)\n\
                            20 B=9\n\
                            30 GET B(2)\n\
                            40 PRINT B(2);B\n";

const A3_GET_INT_ARRAY: &str = "10 DIM B%(5)\n\
                                20 B%=9\n\
                                30 GET B%(2)\n\
                                40 PRINT B%(2);B%\n";

const A3_GET_STR_ARRAY: &str = "10 DIM B$(5)\n\
                                20 B$=\"Z\"\n\
                                30 GET B$(2)\n\
                                40 PRINT \"<\";B$(2);\">\";B$\n";
const A3_GET_SCALAR: &str = "10 B=9\n\
                             20 GET B\n\
                             30 PRINT B\n";

#[test]
fn get_into_array_element_writes_the_element() {
    for p in PROFILES {
        let b = build(A3_GET_ARRAY, p);
        assert!(
            b.asm.contains("JSR __ST_ARR"),
            "{p:?}: the keystroke has to be stored through ARRAY_ADDR"
        );
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: clean exit");
        assert_eq!(
            out.trim_end(),
            " 0  9",
            "{p:?}: the scalar B must be untouched"
        );
    }
}

#[test]
fn get_into_int_array_element_writes_the_element() {
    for p in PROFILES {
        let b = build(A3_GET_INT_ARRAY, p);
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: clean exit");
        assert_eq!(out.trim_end(), " 0  9", "{p:?}");
    }
}

#[test]
fn get_into_string_array_element_writes_the_element() {
    for p in PROFILES {
        let b = build(A3_GET_STR_ARRAY, p);
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: clean exit");
        assert_eq!(out.trim_end(), "<>Z", "{p:?}");
    }
}

#[test]
fn get_into_scalar_is_unchanged() {
    for p in PROFILES {
        let b = build(A3_GET_SCALAR, p);
        assert!(
            !b.asm.contains("JSR __ST_ARR"),
            "{p:?}: the scalar form must not route through ARRAY_ADDR"
        );
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: clean exit");
        assert_eq!(out.trim_end(), " 0", "{p:?}");
    }
}
const A4_OUT_OF_RANGE_AFTER_CLR: &str = "10 DIM A(20)\n\
                                         20 A(15)=7\n\
                                         30 CLR\n\
                                         40 A(15)=1\n\
                                         50 PRINT A(15)\n\
                                         60 PRINT \"DONE\"\n";

/// In-range after the re-auto-DIM, and cleared: ROM prints ` 0`.
const A4_IN_RANGE_AFTER_CLR: &str = "10 DIM A(20)\n\
                                     20 A(15)=7\n\
                                     30 CLR\n\
                                     40 PRINT A(5)\n\
                                     50 PRINT \"DONE\"\n";
const A4_REDIM_AFTER_CLR: &str = "10 DIM A(20)\n\
                                  20 CLR\n\
                                  30 DIM A(20)\n\
                                  40 A(15)=1\n\
                                  50 PRINT A(15)\n\
                                  60 PRINT \"DONE\"\n";
const A4_NO_CLR: &str = "10 DIM A(20)\n\
                         20 A(15)=7\n\
                         30 PRINT A(15)\n\
                         40 PRINT \"DONE\"\n";
const A4_INT_REDIM: &str = "10 DIM A%(20)\n\
                            20 CLR\n\
                            30 DIM A%(20)\n\
                            40 A%(15)=1\n\
                            50 PRINT A%(15)\n\
                            60 PRINT \"DONE\"\n";

#[test]
fn clr_undims_arrays() {
    for p in PROFILES {
        let b = build(A4_OUT_OF_RANGE_AFTER_CLR, p);
        let (clean, out) = run(&b.prg_bytes);
        assert!(
            !clean,
            "{p:?}: A(15) after CLR is ?BAD SUBSCRIPT; the program must not run on"
        );
        assert!(
            !out.contains("DONE"),
            "{p:?}: nothing after the failing store may execute, got {out:?}"
        );
    }
}

#[test]
fn post_clr_reference_reads_the_cleared_array() {
    for p in PROFILES {
        let b = build(A4_IN_RANGE_AFTER_CLR, p);
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: A(5) is inside the auto-DIM bound");
        assert_eq!(out.trim_end(), " 0 \nDONE", "{p:?}");
    }
}

#[test]
fn redim_after_clr_restores_the_declared_bound() {
    for p in PROFILES {
        let b = build(A4_REDIM_AFTER_CLR, p);
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: the re-DIM makes A(15) legal again");
        assert_eq!(out.trim_end(), " 1 \nDONE", "{p:?}");
    }
}

#[test]
fn without_clr_the_declared_bound_stands() {
    for p in PROFILES {
        let b = build(A4_NO_CLR, p);
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: no CLR, no re-auto-DIM");
        assert_eq!(out.trim_end(), " 7 \nDONE", "{p:?}");
    }
}

#[test]
fn redim_after_clr_restores_an_integer_array() {
    for p in PROFILES {
        let b = build(A4_INT_REDIM, p);
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: the re-DIM makes A%(15) legal again");
        assert_eq!(out.trim_end(), " 1 \nDONE", "{p:?}");
    }
}
