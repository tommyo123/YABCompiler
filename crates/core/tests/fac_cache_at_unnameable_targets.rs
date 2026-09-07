//! Member 3 of the FAC1-cache class: a reuse that survives an edge into a line the compiler cannot name.

use yabcompiler_core::{CompileOptions, Dialect, compile_with_options, tokenize_program_with};

fn asm_of(src: &str) -> String {
    let opts = CompileOptions {
        dialect: Dialect::Tsb,
        ..CompileOptions::default()
    };
    compile_with_options(
        &tokenize_program_with(src, Dialect::Tsb).expect("tokenizes"),
        opts,
    )
    .expect("compiles")
    .asm
}

fn line_block<'a>(asm: &'a str, label: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in asm.lines() {
        let t = line.trim();
        if t == label {
            inside = true;
            continue;
        }
        if inside {
            if t.starts_with('L') && t.ends_with(':') && t[1..t.len() - 1].chars().all(|c| c.is_ascii_digit()) {
                break;
            }
            out.push(t);
        }
    }
    assert!(inside, "label {label} not found in:\n{asm}");
    out
}

/// `ON ERROR GOTO 50`, so `__TSB_GOTO_LINE` emits `JMP L50`.
const ON_ERROR: &str = "10 A=PEEK(197)/7\n\
                        20 ON ERROR GOTO 50\n\
                        25 D=A*100\n\
                        30 ERROR 11\n\
                        40 B=A\n\
                        50 C=A\n\
                        60 GOTO 70\n\
                        70 PRINT A\n\
                        80 PRINT B\n\
                        90 PRINT C\n\
                        95 PRINT D\n\
                        99 END\n";

const NO_ON_ERROR: &str = "10 A=PEEK(197)/7\n\
                           25 D=A*100\n\
                           40 B=A\n\
                           50 C=A\n\
                           60 GOTO 70\n\
                           70 PRINT A\n\
                           80 PRINT B\n\
                           90 PRINT C\n\
                           95 PRINT D\n\
                           99 END\n";

#[test]
fn on_error_handler_line_loads_the_variable_instead_of_reusing_fac() {
    let asm = asm_of(ON_ERROR);
    let block = line_block(&asm, "L50:");
    let joined = block.join("\n");

    assert!(
        asm.lines().any(|l| l.trim() == "JMP L50"),
        "precondition: __TSB_GOTO_LINE must emit `JMP L50`, else this test \
         is vacuous.\n{asm}"
    );

    assert!(
        !joined.contains("; reuse FAC"),
        "line 50 is entered by `JMP L50` from __TSB_GOTO_LINE, so FAC1 holds \
         whatever the error path left. Reusing it stores that value into C \
         with no error banner.\nL50 block:\n{joined}"
    );

    // Values, not presences: the block must load A specifically and store to C specifically.
    assert!(
        block.iter().any(|t| *t == "JSR __LD_A" || *t == "LDA #<V_A"),
        "line 50 must reload V_A.\nL50 block:\n{joined}"
    );
    assert!(
        block.iter().any(|t| *t == "LDX #<V_C"),
        "line 50 must store to V_C.\nL50 block:\n{joined}"
    );

    assert!(
        !block.iter().any(|t| t.contains("V_D")),
        "line 50 must not touch V_D.\nL50 block:\n{joined}"
    );
}

#[test]
fn the_same_reuse_is_still_emitted_when_no_construct_can_jump_into_the_line() {
    let asm = asm_of(NO_ON_ERROR);

    let lines: Vec<&str> = asm.lines().map(str::trim).collect();
    let at = lines
        .iter()
        .position(|t| t.contains("; reuse FAC"))
        .unwrap_or_else(|| panic!(
            "sequential assignments must reuse FAC: {asm}"
        ));
    // Value, not presence: it must be the reuse that stores into C.
    assert!(
        lines[at..].iter().take(4).any(|t| *t == "LDX #<V_C"),
        "the surviving reuse must be the one feeding V_C.\n{:?}",
        &lines[at..(at + 4).min(lines.len())]
    );
}

#[test]
fn on_error_removes_every_reuse_and_its_absence_restores_them() {
    let with = asm_of(ON_ERROR).matches("; reuse FAC").count();
    let without = asm_of(NO_ON_ERROR).matches("; reuse FAC").count();
    assert_eq!(
        with, 0,
        "no reuse may survive anywhere in a module whose error dispatcher \
         can `JMP L<n>` at every line"
    );
    assert!(
        without > 0,
        "the identical program without ON ERROR must keep its reuses; got \
         {without}"
    );
}

#[test]
fn on_key_poll_invalidates_between_statements() {
    let src = "10 A=PEEK(197)/7\n\
               20 ON KEY \"Q\" GOSUB 100\n\
               30 B=A : C=A\n\
               40 PRINT B;C\n\
               50 END\n\
               100 D=A*100\n\
               110 RETURN\n";
    let asm = asm_of(src);
    assert!(
        asm.contains("JSR __ON_KEY_FIRE"),
        "precondition: the poll must be emitted, else this test is vacuous.\n{asm}"
    );
    assert_eq!(
        asm.matches("; reuse FAC").count(),
        0,
        "`C=A` follows `B=A` on the same line, but the poll between them can \
         run line 100's `D=A*100` and return here with FAC1 holding D.\n{asm}"
    );
}

/// A computed `GOTO` with a runtime target reaches any line.
#[test]
fn runtime_computed_goto_invalidates_every_line() {
    let src = "10 A=PEEK(197)/7\n\
               20 G=PEEK(198)+30\n\
               30 B=A\n\
               40 C=A\n\
               50 PRINT B;C\n\
               60 CGOTO G\n\
               70 END\n";
    let asm = asm_of(src);
    assert!(
        asm.contains("__CGOTO"),
        "precondition: the runtime compare chain must be emitted, else this \
         test is vacuous.\n{asm}"
    );
    assert_eq!(
        asm.matches("; reuse FAC").count(),
        0,
        "`CGOTO G` can land on any line, so no line may trust what its linear \
         predecessor left in FAC1.\n{asm}"
    );
}
