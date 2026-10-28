mod common;

use yabcompiler_core::{
    CompileOptions, Dialect, Profile, compile_with_options, tokenize_program,
    tokenize_program_with,
};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(src: &str, profile: Profile) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).expect("tokenize");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .expect("compile")
}

/// (printed text, clean exit, error banner) from a bounded emu64 run.
fn run(src: &str, profile: Profile) -> (String, bool, Option<String>) {
    let c = build(src, profile);
    let r = emu64::run_prg_to_end(&c.prg_bytes, 20_000_000).expect("runs");
    (
        common::petscii_to_string(&r.output),
        r.clean_exit,
        r.error_banner(),
    )
}

#[test]
fn self_recursive_tail_gosub_still_runs_out_of_stack() {
    let src = "10 d=0\n20 gosub 100\n30 end\n100 d=d+1:print d;:gosub 100:return\n";
    for p in PROFILES {
        let (out, clean, banner) = run(src, p);
        assert!(!clean, "{p:?}: must not exit cleanly, got {out:?}");
        assert_eq!(
            banner.as_deref(),
            Some("?OUT OF MEMORY  ERROR IN 100"),
            "{p:?}: wrong error for a runaway self-recursive GOSUB"
        );
        assert!(
            out.starts_with(" 1  2  3"),
            "{p:?}: expected the recursion to count up, got {out:?}"
        );
    }
}

#[test]
fn tail_gosub_rewrite_still_flattens_a_non_recursive_call() {
    let src = "10 gosub 100\n20 end\n100 print \"a\";:gosub 200:return\n200 print \"b\";:return\n";
    for p in PROFILES {
        let asm = build(src, p).asm;
        assert!(
            !asm.contains("JSR L200"),
            "{p:?}: tail GOSUB 200 : RETURN must still flatten to a jump\n{asm}"
        );
        let (out, clean, _) = run(src, p);
        assert!(clean, "{p:?}: must finish");
        assert_eq!(out.trim_end(), "AB", "{p:?}");
    }
}

#[test]
fn mutually_recursive_tail_gosub_keeps_its_frame() {
    let src = "10 gosub 100\n20 end\n100 c=c+1:print c;:gosub 200:return\n200 gosub 100:return\n";
    for p in PROFILES {
        let (out, clean, banner) = run(src, p);
        assert!(!clean, "{p:?}: must not exit cleanly, got {out:?}");
        assert_eq!(
            banner.as_deref(),
            Some("?OUT OF MEMORY  ERROR IN 100"),
            "{p:?}: mutual recursion must still exhaust the BASIC stack"
        );
    }
}

#[test]
fn clr_restores_the_auto_dim_bound() {
    let src = "10 dim k(3):k(1)=5\n20 clr\n30 k(1)=k(1)+1:print k(1);\n40 print k(9)\n";
    for p in PROFILES {
        let (out, clean, banner) = run(src, p);
        assert_eq!(banner, None, "{p:?}: no error should be raised");
        assert!(clean, "{p:?}: must finish");
        assert_eq!(out.trim_end(), " 1  0", "{p:?}");
    }
}

#[test]
fn a_widened_array_still_bounds_checks_against_its_declared_dim() {
    let src = "10 dim k(3)\n20 print k(5)\n30 clr\n";
    for p in PROFILES {
        let (_, clean, banner) = run(src, p);
        assert!(!clean, "{p:?}: must not finish");
        assert_eq!(
            banner.as_deref(),
            Some("?BAD SUBSCRIPT  ERROR IN 20"),
            "{p:?}: the declared bound still governs before the CLR"
        );
    }
}

#[test]
fn only_an_out_of_range_index_pays_for_widening() {
    let cases = [
        // No CLR: the DIM is in force for the whole program.
        ("10 dim k(3)\n20 print k(9)\n", "; DIM K(3)"),
        // CLR, but every index is inside the declared bound, so a widened slot would read
        // exactly the same byte.
        ("10 dim k(3)\n20 k(3)=1:print k(3)\n30 clr\n", "; DIM K(3)"),
        // CLR and an index the DIM does not cover: this one needs it.
        ("10 dim k(3)\n20 clr\n30 print k(9)\n", "; DIM K(10)"),
    ];
    for (src, want) in cases {
        for p in PROFILES {
            assert!(
                build(src, p).asm.contains(want),
                "{p:?}: expected {want:?} for {src:?}"
            );
        }
    }
}

#[test]
fn a_costly_multi_dim_widening_is_declined() {
    let src = "10 dim a(3,3,3)\n20 clr\n30 print a(9,1,1)\n";
    for p in PROFILES {
        assert!(
            build(src, p).asm.contains("; DIM A(3,3,3)"),
            "{p:?}: widening A(3,3,3) to A(10,10,10) is over budget"
        );
    }
}

#[test]
fn next_restores_curlin_to_the_for_line() {
    let src = "10 for i=1 to 2:print peek(57)+peek(58)*256;\n20 next\n";
    for p in PROFILES {
        let (out, clean, _) = run(src, p);
        assert!(clean, "{p:?}: must finish");
        assert_eq!(out.trim_end(), " 10  10", "{p:?}");
    }
}

#[test]
fn next_restores_curlin_inside_a_subroutine() {
    let src =
        "10 gosub 100\n20 end\n100 for i=1 to 3:print peek(57)+peek(58)*256;\n110 next:return\n";
    for p in PROFILES {
        let (out, clean, _) = run(src, p);
        assert!(clean, "{p:?}: must finish");
        assert_eq!(out.trim_end(), " 100  100  100", "{p:?}");
    }
}

#[test]
fn a_loop_that_never_reads_curlin_gets_no_restamp() {

    let quiet = "10 for i=1 to 10\n20 a=a+i\n30 next\n40 print a\n";
    // A tail, but one that provably cannot raise, so there is no error to attribute and no reason to spend the stamp.
    let quiet_tail = "10 for i=1 to 10:poke 1024,5\n20 next\n30 print a\n";

    let reads = "10 for i=1 to 10:a=peek(57)\n20 next\n30 print a\n";
    // A loop body that can raise an error ends on another BASIC line.
    let fallible_tail = "10 for i=1 to 10:a=a*100+1e30\n20 next\n30 print a\n";
    for p in PROFILES {
        assert_ne!(
            top_of_for_body(&build(quiet, p).asm),
            Some(10),
            "{p:?}: line 20 is a re-entry line and stamps itself; the head \
             must not stamp 10 on top of that"
        );
        assert_eq!(
            top_of_for_body(&build(quiet_tail, p).asm),
            None,
            "{p:?}: a tail that cannot raise buys nothing"
        );
        assert_eq!(
            top_of_for_body(&build(reads, p).asm),
            Some(10),
            "{p:?}: a program that reads CURLIN must get the FOR's line back"
        );
        assert_eq!(
            top_of_for_body(&build(fallible_tail, p).asm),
            Some(10),
            "{p:?}: `?OVERFLOW` in the tail is `ERROR IN 10` on the ROM, and \
             nothing else re-stamps 10 before the back edge"
        );
    }
}

/// The line number stamped immediately after the first `__FOR_TOP_n:`, or `None` when the body
/// does not open with a CURLIN stamp.
fn top_of_for_body(asm: &str) -> Option<u16> {
    let start = asm.find("__FOR_TOP_").expect("no __FOR_TOP_ in asm");
    let mut it = asm[start..].lines().skip(1);
    let lo = it.next()?.trim().strip_prefix("LDA #$")?.to_string();
    if it.next()?.trim() != "STA $39" {
        return None;
    }
    u16::from_str_radix(&lo, 16).ok()
}

#[test]
fn string_plus_number_is_a_runtime_type_mismatch() {
    let src = "10 a$=\"x\"\n20 print \"y\";\n30 print a$+1\n";
    for p in PROFILES {
        let (out, clean, banner) = run(src, p);
        assert!(!clean, "{p:?}: must not finish");
        assert_eq!(out.trim_end(), "Y", "{p:?}: line 20 still runs");
        assert_eq!(
            banner.as_deref(),
            Some("?TYPE MISMATCH  ERROR IN 30"),
            "{p:?}"
        );
    }
}

#[test]
fn string_into_numeric_var_is_a_runtime_type_mismatch() {
    let src = "10 for i=1 to 3\n20 a$=\"x\"\n30 print \"a\";i;a$\n40 a=a$\n50 next\n";
    for p in PROFILES {
        let (out, clean, banner) = run(src, p);
        assert!(!clean, "{p:?}: must not finish");
        assert_eq!(out.trim_end(), "A 1 X", "{p:?}: one pass before the trap");
        assert_eq!(
            banner.as_deref(),
            Some("?TYPE MISMATCH  ERROR IN 40"),
            "{p:?}"
        );
    }
}

#[test]
fn well_typed_string_expressions_are_untouched() {
    let ok = [
        "10 a$=\"x\":b$=a$+\"y\"+chr$(65):print b$\n",
        "10 a$=\"x\":print a$+left$(\"abc\",2)\n",
        "10 a$=\"5\":print val(a$)+1\n",
        "10 a$=\"x\":if a$=\"x\" then print \"eq\"\n",
        "10 a$=\"x\":print len(a$+str$(3))\n",
    ];
    for src in ok {
        for p in PROFILES {
            let c = build(src, p);
            assert!(
                !c.asm.contains(&format!("LDX #${:02X}", 22u8)),
                "{src:?} at {p:?} must not compile to a ?TYPE MISMATCH trap\n{}",
                c.asm
            );
        }
    }
}

#[test]
fn a_truncated_expression_is_syntax_not_type_mismatch() {
    let src = "10 a$=\"x\"+\n";
    for p in PROFILES {
        let (_, clean, banner) = run(src, p);
        assert!(!clean, "{p:?}: must not finish");
        assert_eq!(banner.as_deref(), Some("?SYNTAX  ERROR IN 10"), "{p:?}");
    }

    let (out, clean, banner) = run("10 print\"a\"\n20 end\n30 a$=\"x\"+\n", Profile::Default);
    assert!(clean && banner.is_none(), "{out:?} {banner:?}");
    assert_eq!(out.trim_end(), "A");
}

#[test]
fn an_unimplemented_construct_is_still_a_compile_error() {
    // `Unsupported { name }` ; a TSB keyword with no implementation.
    let prg = tokenize_program_with("10 secure\n", Dialect::Tsb).expect("tokenize");
    let err = compile_with_options(
        &prg,
        CompileOptions {
            dialect: Dialect::Tsb,
            ..CompileOptions::default()
        },
    )
    .err()
    .expect("an unimplemented TSB keyword must not compile");
    assert!(
        err.to_string().contains("SECURE"),
        "the diagnostic must name the missing feature: {err}"
    );

    let prg = tokenize_program_with("10 save\"x\" use 0,4096\n", Dialect::Tsb).expect("tokenize");
    let err = compile_with_options(
        &prg,
        CompileOptions {
            dialect: Dialect::Tsb,
            ..CompileOptions::default()
        },
    )
    .err()
    .expect("an unimplemented TSB suffix must not compile");
    assert!(
        err.to_string().contains("USE suffix is only supported on LOAD"),
        "the diagnostic must name the missing feature: {err}"
    );
}

#[test]
fn cont_is_a_hang_and_not_a_syntax_trap() {
    let prg = tokenize_program("10 print\"a\"\n20 cont\n30 print\"b\"\n").expect("tokenize");
    let out = compile_with_options(&prg, CompileOptions::default())
        .expect("a reached bare CONT is a hang, not a refusal");
    let r = emu64::run_prg_to_end(&out.prg_bytes, 20_000_000).expect("runs");
    assert_eq!(
        r.error_banner(),
        None,
        "CONT must not invent an error the ROM never prints"
    );
    assert!(!r.clean_exit, "…and must not fall through to line 30 either");
    let text = common::petscii_to_string(&r.output);
    assert!(text.contains('A'), "{text:?}");
    assert!(!text.contains('B'), "{text:?}");
}

#[test]
fn for_with_an_integer_counter_is_a_kept_dialect_extension() {
    for src in [
        "10 for i%=1 to 3\n20 print i%;:next\n30 print \"e\"\n",

        "10 for i %=1 to 3\n20 print i%;:next\n30 print \"e\"\n",
    ] {
        for p in PROFILES {
            let (out, clean, banner) = run(src, p);
            assert_eq!(banner, None, "{p:?}: the extension must not raise");
            assert!(clean, "{p:?}: must finish");
            assert_eq!(out.trim_end(), " 1  2  3 E", "{p:?}");
        }
    }
}
