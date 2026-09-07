//! `NOT` is a prefix operator wherever an operand may start.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn output_at(src: &str, profile: Profile) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    let opts = CompileOptions {
        profile,
        ..Default::default()
    };
    let c = compile_with_options(&prg, opts).unwrap_or_else(|e| panic!("compiles: {e}\n{src}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("runs");
    assert!(r.clean_exit, "must reach the final RTS:\n{src}");
    common::petscii_to_string(&r.output)
}

fn output(src: &str) -> String {
    output_at(src, Profile::Default)
}

#[test]
fn not_is_accepted_after_an_operator() {
    let src = "\
10 PRINT 3>=NOT 6
20 PRINT 1+NOT 2
30 PRINT -NOT 2
40 PRINT 2*NOT 1
50 PRINT 5-NOT 3
60 END
";
    assert_eq!(output(src), "-1 \n-2 \n 3 \n-4 \n 9 \n");
}

/// Precedence: `NOT` reaches past arithmetic and comparison, and stops at `AND`/`OR`.
#[test]
fn not_binds_looser_than_arithmetic_and_tighter_than_and() {
    let src = "\
10 PRINT 1+NOT 2+3
20 PRINT 2*NOT 1=1
30 PRINT 3>=NOT 6=1
40 PRINT NOT 1 AND 2
50 END
";
    assert_eq!(output(src), "-5 \n 0 \n-1 \n 2 \n");
}

#[test]
fn not_in_an_if_condition_operand() {
    let src = "\
10 A=3:B=6
20 IF A>=NOT B THEN PRINT \"YES\"
30 IF A<NOT B THEN PRINT \"NO\"
40 END
";
    assert_eq!(output(src), "YES\n");
}

#[test]
fn the_positions_that_already_worked_still_do() {
    let src = "\
10 PRINT NOT 6
20 PRINT NOT 6.7
30 PRINT 1 AND NOT 2
40 PRINT ABS(NOT 2)
50 PRINT NOT NOT 2
60 PRINT (1 OR NOT 0)
70 A=NOT 6:PRINT A
80 END
";
    assert_eq!(output(src), "-7 \n-7 \n 1 \n 3 \n 2 \n-1 \n-7 \n");
}

#[test]
fn all_profiles_agree() {
    let src = "10 PRINT 1+NOT 2+3;3>=NOT 6;NOT 1 AND 2\n20 END\n";
    for profile in [Profile::Default, Profile::Size, Profile::Speed] {
        assert_eq!(
            output_at(src, profile),
            "-5 -1  2 \n",
            "profile {profile:?}"
        );
    }
}
