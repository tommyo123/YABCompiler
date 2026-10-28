//! Statements whose run-time behaviour follows the ROM, each checked against the interpreter.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (String, Option<u8>, Option<u16>);

fn rom(prg: &[u8]) -> Outcome {
    let r = emu64::run_basic_to_end(prg, 40_000_000).expect("runs in the interpreter");
    (
        common::petscii_to_string(&r.output),
        r.error_code,
        r.error_line,
    )
}

fn compiled(prg: &[u8], options: CompileOptions) -> Outcome {
    let c = compile_with_options(prg, options).unwrap_or_else(|e| panic!("compiles: {e}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, 40_000_000).expect("runs");
    (
        common::petscii_to_string(&r.output),
        r.error_code,
        r.error_line,
    )
}

fn same_as_the_rom_prg(prg: &[u8], what: &str) {
    let want = rom(prg);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let got = compiled(
            prg,
            CompileOptions {
                profile,
                ..CompileOptions::default()
            },
        );
        assert_eq!(got, want, "{profile:?}:\n{what}");
    }
}

fn same_as_the_rom(src: &str) {
    same_as_the_rom_prg(&tokenize_program(src).expect("tokenizes"), src);
}

/// A tokenized program from raw line bodies.
fn raw_prg(lines: &[(u16, &[u8])]) -> Vec<u8> {
    let mut out = vec![0x01, 0x08];
    let mut addr = 0x0801u16;
    for (number, body) in lines {
        let next = addr + 4 + body.len() as u16 + 1;
        out.extend_from_slice(&next.to_le_bytes());
        out.extend_from_slice(&number.to_le_bytes());
        out.extend_from_slice(body);
        out.push(0);
        addr = next;
    }
    out.extend_from_slice(&[0, 0]);
    out
}

#[test]
fn an_unopened_file_raises_file_not_open() {
    for src in [
        "10 GET#1,A$:PRINT 1\n",
        "10 GET#1,A$,B$(2):PRINT 1\n",
        "10 INPUT#1,A$:PRINT 1\n",
        "10 PRINT 1:CMD 5:PRINT 2\n",
    ] {
        same_as_the_rom(src);
    }
}

#[test]
fn get_reads_one_key_per_variable() {
    for src in [
        "10 POKE 631,65:POKE 632,66:POKE 633,67:POKE 198,3:GET A$,B$:PRINT A$;\"/\";B$:GET C$:PRINT C$\n",
        "10 POKE 631,65:POKE 198,1:GET A$,B$:PRINT A$;\"/\";B$;\"/\";LEN(B$)\n",
        "10 POKE 631,49:POKE 632,50:POKE 198,2:GET A,B:PRINT A;B\n",
        "10 DIM B$(3):POKE 631,65:POKE 632,66:POKE 198,2:K=2:GET A$,B$(K):PRINT A$;B$(2)\n",
    ] {
        same_as_the_rom(src);
    }
}

#[test]
fn dim_raises_the_roms_errors_for_sizes_it_can_never_hold() {
    for src in [
        "10 PRINT 1:DIM A(10000):PRINT 2\n",
        "10 PRINT 1:DIM A(40000):PRINT 2\n",
        "10 PRINT 1:DIM A(5,40000):PRINT 2\n",
        "10 PRINT 1:DIM A%(20000):PRINT 2\n",
        "10 PRINT 1:DIM A$(13000):PRINT 2\n",
        "10 PRINT 1:DIM A(100,100):PRINT 2\n",
        "10 PRINT 1:DIM A%(5,5,5,5,5,5,5,5,5,5):PRINT 2\n",
        "10 PRINT 1:DIM B(5),A(10000),C(5):PRINT 2\n",
        "10 DIM B(3):B(2)=7:PRINT B(2):DIM C(20000)\n",
        "10 IF 0 THEN DIM A(10000)\n20 A(3)=4:PRINT A(3)\n",
        "10 PRINT 1:DIM A(7000):PRINT 2\n",
    ] {
        same_as_the_rom(src);
    }
}

#[test]
fn an_auto_dim_the_rom_can_never_hold_runs_out_of_memory() {
    for src in [
        "10 PRINT 1:A(1,2,3,4)=5:PRINT 2\n",
        "10 PRINT \"X\";A(1,1,1,1):PRINT 2\n",
        "10 PRINT 1:A$(1,2,3,4)=\"X\":PRINT 2\n",
        "10 PRINT 1:A%(1,2,3,4,5)=5:PRINT 2\n",
        "10 PRINT 1:A%(1,2,3,4)=5:PRINT A%(1,2,3,4)\n",
        "10 PRINT 1:A(11,2,3,4)=5:PRINT 2\n",
        "10 PRINT 1:A(-1,1,1,1)=5:PRINT 2\n",
        "10 PRINT 1:X=1/0+A(1,1,1,1):PRINT 2\n",
        "10 PRINT 1:A(1,1,1,1)=1/0:PRINT 2\n",
        "10 FOR I=0 TO 3:A(I,1,1,1)=I:NEXT:PRINT 2\n",
        "10 PRINT 1:READ A(1,2,3,4):PRINT 2\n",
        "10 IF 0 THEN A(1,2,3,4)=5\n20 PRINT 2\n",
        "10 DIM A(2,2,2,2):A(1,1,1,1)=5:PRINT A(1,1,1,1)\n",
    ] {
        same_as_the_rom(src);
    }
}

#[test]
fn new_ends_the_program() {
    same_as_the_rom("10 PRINT 1:NEW:PRINT 2\n20 PRINT 3\n");
}

#[test]
fn restore_with_a_line_number_raises_after_restoring() {
    same_as_the_rom(
        "10 READ A:PRINT A:RESTORE 930:READ B:PRINT B\n20 PRINT \"NOT HERE\"\n930 DATA 1,2\n",
    );
    same_as_the_rom("10 READ A:RESTORE X:READ B:PRINT A;B\n20 DATA 7,8\n");
}

#[test]
fn a_statement_runs_and_then_its_tail_raises() {
    for src in [
        "10 PRINT 1:POKE 830,1;PRINT 2\n",
        "10 POKE 830,1PRINT 2\n20 PRINT 3\n",
        "10 A=1 B:PRINT A\n20 PRINT 2\n",
        "10 A$=\"X\" \"Y\":PRINT A$\n",
        "10 POKE 830,1 ,2:PRINT 1\n",
        "10 READ A B:PRINT A\n20 DATA 5,6\n",
        "10 FOR I=1 TO 2 X:PRINT I:NEXT\n20 PRINT \"DONE\"\n",
        "10 FOR I=1 TO 3:PRINT I:NEXT I X\n20 PRINT \"DONE\"\n",
        "10 IF 1 THEN A=1 B:PRINT 2\n20 PRINT 3\n",
        "10 PRINT \"A\"GOTO 20\n20 PRINT \"B\"\n",
    ] {
        same_as_the_rom(src);
    }
}

#[test]
fn these_statements_check_their_tail_first() {
    for src in [
        "10 PRINT 1:END 5\n20 PRINT 2\n",
        "10 PRINT 1:STOP X\n20 PRINT 2\n",
        "10 PRINT 1:CLR 5:PRINT 2\n",
        "10 PRINT 1:RETURN X\n",
        "10 GOSUB 20:PRINT \"BACK\"\n15 END\n20 PRINT \"SUB\":RETURN 5\n",
        "10 PRINT 1:WAIT 830,0 X:PRINT 2\n",
        "10 CMD 3 X:PRINT 1\n",
    ] {
        same_as_the_rom(src);
    }
}

#[test]
fn return_skips_the_tail_of_a_gosub() {
    for src in [
        "10 GOSUB 20 X:PRINT \"BACK\"\n15 END\n20 PRINT \"SUB\":RETURN\n",
        "10 GOSUB 20 X;PRINT \"NO\":PRINT \"BACK\"\n15 END\n20 PRINT \"SUB\":RETURN\n",
        "10 GOSUB 20 \"A:B\":PRINT \"BACK\"\n15 END\n20 PRINT \"SUB\":RETURN\n",
        "10 GOSUB 20LABEL:PRINT \"BACK\"\n15 END\n20 PRINT \"SUB\":RETURN\n",
        "10 IF 1 THEN GOSUB 20 X:PRINT \"BACK\"\n15 END\n20 PRINT \"SUB\":RETURN\n",
        "10 ON 1 GOSUB 20 X:PRINT \"BACK\"\n15 END\n20 PRINT \"SUB\":RETURN\n",
    ] {
        same_as_the_rom(src);
    }
}

#[test]
fn on_raises_on_its_tail_only_when_it_falls_through() {
    for src in [
        "10 ON 3 GOTO 20,30 X\n15 PRINT \"FELL\"\n20 PRINT 20\n30 PRINT 30\n",
        "10 ON 2 GOTO 20 X,30\n15 PRINT \"FELL\"\n20 PRINT 20\n30 PRINT 30\n",
        "10 ON 1 GOTO 20 X,30\n15 PRINT \"FELL\"\n20 PRINT 20\n30 PRINT 30\n",
    ] {
        same_as_the_rom(src);
    }
}

#[test]
fn bytes_that_are_never_read_raise_nothing() {
    for src in [
        "10 GOTO 20 X\n20 PRINT 20\n",
        "10 IF K=133 THEN 350:REM X\n20 PRINT 2\n350 END\n",
        "10 DATA \"A\"B:PRINT 1\n",
        "10 PRINT 1:REM X Y\n20 IF 0 THEN JUNK JUNK\n30 DATA JUNK JUNK\n40 PRINT 2\n",
    ] {
        same_as_the_rom(src);
    }
}

#[test]
fn a_def_fn_body_with_a_tail_raises_when_called() {
    same_as_the_rom("10 DEF FN A(X)=X*2 Y:PRINT 1\n20 PRINT FN A(2)\n");
}

#[test]
fn a_control_byte_raises() {
    const PRINT: u8 = 0x99;
    const POKE: u8 = 0x97;
    let cases: [&[(u16, &[u8])]; 4] = [
        &[
            (10, &[PRINT, b'1', b':', 0x01, PRINT, b'2']),
            (20, &[PRINT, b'3']),
        ],
        &[(10, &[0x01, PRINT, b'1']), (20, &[PRINT, b'2'])],
        &[
            (10, &[POKE, b'8', b'3', b'0', b',', b'1', 0x1F]),
            (20, &[PRINT, b'2']),
        ],
        &[(10, &[PRINT, b'1', b':', b' ', 0x01]), (20, &[PRINT, b'2'])],
    ];
    for lines in cases {
        same_as_the_rom_prg(&raw_prg(lines), &format!("{lines:?}"));
    }
}

/// Lenient mode keeps skipping a tail.
#[test]
fn lenient_mode_skips_the_tail() {
    let prg = tokenize_program("10 PRINT 1:POKE 830,1;PRINT 2\n20 FOR I%=1 TO 2:PRINT I%:NEXT\n")
        .expect("tokenizes");
    let got = compiled(
        &prg,
        CompileOptions {
            lenient_syntax: true,
            ..CompileOptions::default()
        },
    );
    assert_eq!(got.0, " 1 \n 2 \n 1 \n 2 \n");
    assert_eq!(got.1, None);
}

#[test]
fn a_quoted_data_item_with_more_after_it_cannot_be_read() {
    for src in [
        "10 READ A$:PRINT A$\n20 DATA \"AB\"C\n",
        "10 READ A$,B$:PRINT A$;B$\n20 DATA \"AB\" ,CD\n",
        "10 READ A$:PRINT A$:READ B$:PRINT B$\n20 DATA X,\"AB\"C,D\n",
        "10 READ A$,B$:PRINT B$\n20 DATA \"X\"Y,Z\n",
        "10 READ A:PRINT A\n20 DATA \"1\"2\n",
        "10 FOR I=1 TO 5:READ A$(I):PRINT A$(I):NEXT\n20 DATA A,B,\"C\" D,E,F\n",
        "10 READ A$:PRINT A$:RESTORE:READ B$:PRINT B$\n20 DATA OK\n30 DATA \"Q\"R\n",
    ] {
        same_as_the_rom(src);
    }
}
