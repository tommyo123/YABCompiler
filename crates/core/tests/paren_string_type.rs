//! A parenthesised expression takes the type of what is inside it.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn asm_of(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default())
        .unwrap_or_else(|e| panic!("compiles: {e}"))
        .asm
}

/// `LDX #$16` + `JMP $A437` is the unconditional `?TYPE MISMATCH` trap codegen emits for
/// `Statement::RaiseError`.
fn has_type_mismatch_trap(asm: &str) -> bool {
    let lines: Vec<&str> = asm.lines().map(str::trim).collect();
    lines
        .windows(2)
        .any(|w| w[0] == "LDX #$16" && w[1] == "JMP $A437")
}

fn output(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(&prg, CompileOptions::default()).expect("compiles");
    let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("runs");
    assert!(r.clean_exit, "program must reach its final RTS:\n{src}");
    common::petscii_to_string(&r.output)
}

#[test]
fn print_of_a_parenthesised_string_prints_it() {
    let src = "\
10 A$=\"AB\"
20 PRINT (A$)
30 PRINT (A$+\"CD\")
40 PRINT (\"EF\")
50 PRINT \"P\";(A$);\"Q\"
60 PRINT ((A$))
70 PRINT (CHR$(65)+CHR$(66))
80 END
";
    assert!(
        !has_type_mismatch_trap(&asm_of(src)),
        "parenthesised strings in PRINT still compile to a trap:\n{}",
        asm_of(src)
    );
    assert_eq!(output(src), "AB\nABCD\nEF\nPABQ\nAB\nAB\n");
}

#[test]
fn a_parenthesised_string_compares_as_a_string() {
    let src = "\
10 A$=\"AB\"
20 IF (A$)=\"AB\" THEN PRINT \"EQ1\"
30 IF (A$+\"C\")=\"ABC\" THEN PRINT \"EQ2\"
40 IF (\"X\")=\"X\" THEN PRINT \"EQ3\"
50 PRINT (A$)=\"AB\"
60 END
";
    assert!(!has_type_mismatch_trap(&asm_of(src)));
    assert_eq!(output(src), "EQ1\nEQ2\nEQ3\n-1 \n");
}

#[test]
fn a_parenthesised_string_concatenates_and_measures() {
    let src = "\
10 A$=\"AB\"
20 PRINT (A$)+\"C\"
30 PRINT ((A$)+\"D\")
40 PRINT LEN((A$))
50 PRINT LEFT$((A$+\"CD\"),3)
60 END
";
    assert!(!has_type_mismatch_trap(&asm_of(src)));
    assert_eq!(output(src), "ABC\nABD\n 2 \nABC\n");
}
#[test]
fn parenthesised_strings_in_arrays_and_subroutines() {
    let src = "\
10 DIM G$(3)
20 G$(1)=\"ZZ\":A$=\"Q\"
30 PRINT (G$(1))
40 PRINT (G$(1)+A$)
50 PRINT (STR$(12)+\"X\")
60 GOSUB 100
70 END
100 PRINT (A$+\"-SUB\"):RETURN
";
    assert!(!has_type_mismatch_trap(&asm_of(src)));
    assert_eq!(output(src), "ZZ\nZZQ\n 12X\nQ-SUB\n");
}
#[test]
fn all_profiles_agree_on_parenthesised_strings() {
    let src = "\
10 A$=\"AB\"
20 PRINT (A$);(A$+\"C\")
30 END
";
    let prg = tokenize_program(src).expect("tokenizes");
    for profile in [Profile::Default, Profile::Size, Profile::Speed] {
        let opts = CompileOptions {
            profile,
            ..Default::default()
        };
        let c = compile_with_options(&prg, opts).expect("compiles");
        let r = emu64::run_prg_to_end(&c.prg_bytes, common::MAX_INSNS).expect("runs");
        assert_eq!(
            common::petscii_to_string(&r.output),
            "ABABC\n",
            "profile {profile:?}"
        );
    }
}

/// A comparison *inside* the parens makes the group numeric (-1/0), whatever the operands are.
#[test]
fn a_parenthesised_string_comparison_is_numeric() {
    let src = "\
10 A$=\"AB\"
20 PRINT (A$=\"AB\")
30 PRINT (A$<\"B\")
40 PRINT (A$=\"AB\")+2
50 END
";
    assert!(!has_type_mismatch_trap(&asm_of(src)));
    assert_eq!(output(src), "-1 \n-1 \n 1 \n");
}
#[test]
fn numeric_groups_stay_numeric() {
    let src = "\
10 A=5
20 PRINT (A)
30 PRINT (A+1)
40 PRINT (A*2)-1
50 PRINT ((A))
60 PRINT (A=5)
70 END
";
    assert!(!has_type_mismatch_trap(&asm_of(src)));
    assert_eq!(output(src), " 5 \n 6 \n 9 \n 5 \n-1 \n");
}
#[test]
fn a_string_group_in_numeric_position_still_traps() {
    for src in [
        "10 A$=\"AB\"\n20 PRINT \"BEFORE\"\n30 A=(A$)\n40 PRINT \"AFTER\"\n",
        "10 A$=\"AB\"\n20 PRINT \"BEFORE\"\n30 POKE (A$),1\n40 PRINT \"AFTER\"\n",
    ] {
        let asm = asm_of(src);
        assert!(
            has_type_mismatch_trap(&asm),
            "a string in numeric position must keep trapping:\n{src}"
        );
    }
}
#[test]
fn tab_plus_string_still_takes_the_string_path() {
    let src = "10 A$=\"S\"\n20 PRINT TAB(3)+(A$)\n30 END\n";
    assert!(!has_type_mismatch_trap(&asm_of(src)));
    assert_eq!(output(src), "   S\n");
}
