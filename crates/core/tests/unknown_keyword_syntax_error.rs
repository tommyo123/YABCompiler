use yabcompiler_core::ast::Statement;
use yabcompiler_core::tokenize_program;

mod common;

const MAX_INSNS: u64 = 20_000_000;

fn run(src: &str) -> String {
    let prg = common::compile_source(src).expect("compile");
    let r = emu64::run_prg_to_end(&prg, MAX_INSNS).expect("run");
    assert!(r.clean_exit, "program should terminate: {src:?}");
    common::petscii_to_string(&r.output)
}

fn stmts(src: &str) -> Vec<Statement> {
    let bytes = tokenize_program(src).expect("tokenize");
    let prg = yabcompiler_core::Program::parse(&bytes).expect("is a BASIC prg");
    let ast = yabcompiler_core::parse::program(&prg).expect("parse");
    ast.lines.into_iter().flat_map(|l| l.statements).collect()
}

fn run_to_banner(src: &str) -> (String, bool, Option<String>) {
    let prg = common::compile_source(src).unwrap_or_else(|e| panic!("must compile: {e}\n{src}"));
    let r = emu64::run_prg_to_end(&prg, MAX_INSNS).expect("run");
    (
        common::petscii_to_string(&r.output),
        r.clean_exit,
        r.error_banner(),
    )
}

/// Defer malformed keywords and assignments until execution.
#[test]
fn unknown_statement_keyword_raises_and_stops() {
    for (src, line) in [
        ("10 PRNIT \"HELLO\"\n20 PRINT \"AFTER\"\n", 10),
        ("10 POKD 53280,0\n20 PRINT \"AFTER\"\n", 10),
        ("10 WIBBLE 42\n20 PRINT \"AFTER\"\n", 10),
        // Assignment syntax is checked when the statement is reached.
        ("5 CONSOLE 1:PRINT CHR$(147);\n20 PRINT \"AFTER\"\n", 5),
        ("10 CONSOLE1\n20 PRINT \"AFTER\"\n", 10),
    ] {
        let (out, clean, banner) = run_to_banner(src);
        assert!(!clean, "must not finish: {src:?}");
        assert_eq!(
            banner.as_deref(),
            Some(format!("?SYNTAX  ERROR IN {line}").as_str()),
            "for {src:?}"
        );
        assert!(
            !out.contains("AFTER"),
            "the next line must not run, got {out:?} for {src:?}"
        );
    }
}

#[test]
fn mistyped_next_raises_rather_than_being_deleted() {
    let src = "10 FOR I=1 TO 3\n20 PRINT \"X\";I\n30 NXET I\n40 PRINT \"DONE\"\n";
    let (out, clean, banner) = run_to_banner(src);
    assert!(!clean, "must not finish");
    assert_eq!(banner.as_deref(), Some("?SYNTAX  ERROR IN 30"));
    assert_eq!(out.trim_end(), "X 1", "one pass, then the trap");
    assert!(!out.contains("DONE"), "got {out:?}");
}

#[test]
fn bare_proc_call_still_fires() {
    let src = "10 GOTO 50\n20 PROC C\n30 PRINT \"IN C\"\n40 END PROC\n\
               50 C\n60 PRINT \"AFTER\"\n";
    assert_eq!(run(src), "IN C\nAFTER\n");
    assert!(
        matches!(&stmts(src)[4], Statement::ProcCall(n) if n.0 == b"C"),
        "line 50 should parse as ProcCall(\"C\"), got {:?}",
        stmts(src)[4]
    );
}

#[test]
fn bare_proc_call_before_colon_still_fires() {
    let src = "10 GOTO 50\n20 PROC C\n30 PRINT \"IN C\"\n40 END PROC\n\
               50 C:PRINT \"AFTER\"\n";
    assert_eq!(run(src), "IN C\nAFTER\n");
}

/// The explicit `EXEC` form is untouched and agrees with the bare form.
#[test]
fn exec_proc_call_still_fires() {
    let bare = "10 GOTO 50\n20 PROC C\n30 PRINT \"IN C\"\n40 END PROC\n\
                50 C\n60 PRINT \"AFTER\"\n";
    let exec = "10 GOTO 50\n20 PROC C\n30 PRINT \"IN C\"\n40 END PROC\n\
                50 EXEC C\n60 PRINT \"AFTER\"\n";
    assert_eq!(run(exec), "IN C\nAFTER\n");
    assert_eq!(
        common::compile_source(bare).unwrap(),
        common::compile_source(exec).unwrap(),
        "bare call and EXEC should compile identically"
    );
}

#[test]
fn stray_lone_identifier_traps_when_reached() {
    let (out, clean, banner) = run_to_banner("10 PRINT \"A\"\n20 ZZ\n30 PRINT \"B\"\n");
    assert!(!clean, "must not finish");
    assert_eq!(banner.as_deref(), Some("?SYNTAX  ERROR IN 20"));
    assert_eq!(out.trim_end(), "A", "A prints, B does not");
}

#[test]
fn stray_lone_identifier_is_free_when_unreached() {
    assert_eq!(
        run("10 PRINT \"A\"\n20 GOTO 40\n30 ZZ\n40 PRINT \"B\"\n"),
        "A\nB\n"
    );

    let with = common::compile_source("10 PRINT \"A\"\n20 GOTO 40\n30 ZZ\n40 PRINT \"B\"\n")
        .expect("compiles");
    let without =
        common::compile_source("10 PRINT \"A\"\n20 GOTO 40\n40 PRINT \"B\"\n").expect("compiles");
    assert_eq!(
        with.len(),
        without.len(),
        "an unreachable bare identifier must be free"
    );
}

#[test]
fn decorative_dot_label_still_tolerated() {
    assert_eq!(run("10 PRINT \"A\"\n20 .DEFINE\n30 PRINT \"B\"\n"), "A\nB\n");
    assert_eq!(
        run("10 PRINT \"A\"\n20 .READ TABLE\n30 PRINT \"B\"\n"),
        "A\nB\n"
    );
}

#[test]
fn ascii_dpoke_compiles_and_writes_both_bytes() {

    assert_eq!(run("10 DPOKE 3,258\n20 PRINT PEEK(3);PEEK(4)\n"), " 2  1 \n");
    assert!(
        matches!(&stmts("10 DPOKE 3,258\n")[0], Statement::Dpoke { .. }),
        "ASCII DPOKE must lower to Statement::Dpoke, got {:?}",
        stmts("10 DPOKE 3,258\n")[0]
    );
}

#[test]
fn assignments_still_parse_as_assignments() {
    // ` 5 ` is the ROM's numeric PRINT format: leading sign blank and trailing blank.
    assert_eq!(run("10 A=5:PRINT A\n"), " 5 \n");
    assert_eq!(run("10 A%=5:PRINT A%\n"), " 5 \n");
    assert_eq!(run("10 A$=\"X\":PRINT A$\n"), "X\n");
    assert_eq!(run("10 DIM A(3):A(1)=7:PRINT A(1)\n"), " 7 \n");

    assert_eq!(run("10 A = 5:PRINT A\n"), " 5 \n");
}
