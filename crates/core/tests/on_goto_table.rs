mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (Vec<u8>, Option<u8>, Option<u16>);

const BUDGET: u64 = 20_000_000;

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, BUDGET).expect("runs in the interpreter");
    assert!(r.ready_reached, "the interpreter reaches READY:\n{src}");
    (r.output, r.error_code, r.error_line)
}

fn asm(src: &str, profile: Profile) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"))
    .asm
}

fn compiled(src: &str, profile: Profile) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, BUDGET).expect("runs");
    assert!(
        r.clean_exit || r.error_code.is_some(),
        "the compiled program ends ({profile:?}):\n{src}"
    );
    (r.output, r.error_code, r.error_line)
}

fn same_as_the_rom(src: &str) {
    let want = interpreted(src);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let got = compiled(src, profile);
        assert_eq!(
            (common::petscii_to_string(&got.0), got.1, got.2),
            (common::petscii_to_string(&want.0), want.1, want.2),
            "{profile:?}:\n{src}"
        );
    }
}

const ELEVEN: &str = "100,200,300,400,500,600,700,999,900,1000,1100";

fn targets_print_their_line(after: &str) -> String {
    let mut src = String::new();
    for line in [100, 200, 300, 400, 500, 600, 700, 900, 1000, 1100] {
        src.push_str(&format!("{line} PRINT \"L{line}\";:{after}\n"));
    }
    src
}

#[test]
fn every_case_and_the_values_that_go_on() {
    let src = format!(
        "10 READ V:IF V<0 THEN END\n20 PRINT V;:ON V GOTO {ELEVEN}\n30 PRINT \"ON\":GOTO 10\n\
         {}\
         2000 DATA 0,1,2,3,4,5,6,7,9,10,11,12,255,2.7,10.99,-1\n",
        targets_print_their_line("GOTO 10")
    );
    same_as_the_rom(&src);
}

#[test]
fn a_missing_target_line_is_undefd_statement() {
    let src = format!("10 V=8\n20 ON V GOTO {ELEVEN}\n{}", targets_print_their_line("END"));
    same_as_the_rom(&src);
}

#[test]
fn a_negative_value_is_illegal_quantity() {
    let src = format!("10 V=-1\n20 ON V GOTO {ELEVEN}\n{}", targets_print_their_line("END"));
    same_as_the_rom(&src);
}

#[test]
fn a_value_past_255_is_illegal_quantity() {
    let src = format!("10 V=256\n20 ON V GOTO {ELEVEN}\n{}", targets_print_their_line("END"));
    same_as_the_rom(&src);
}

#[test]
fn an_error_in_a_target_reports_its_line() {
    for v in [1, 3, 5, 9, 10, 11] {
        let src = format!(
            "10 V={v}:IF V=11 THEN 40\n20 ON V GOTO {ELEVEN}\n30 END\n40 PRINT \"G\";:GOTO 1100\n{}",
            targets_print_their_line("X=1/(V-V)")
        );
        same_as_the_rom(&src);
    }
}

#[test]
fn a_dispatch_in_a_subroutine_returns_to_the_callers_line() {
    for v in [2, 6, 10] {
        let src = format!(
            "10 V={v}:GOSUB 3000:X=1/(V-V)\n20 END\n{}3000 ON V GOTO {ELEVEN}\n3010 RETURN\n",
            targets_print_their_line("RETURN")
        );
        same_as_the_rom(&src);
    }
}

#[test]
fn ten_targets_dispatch_through_a_table_and_nine_compare() {
    let ten = "10 ON V GOTO 100,100,100,100,100,100,100,100,100,100\n100 END\n";
    let nine = "10 ON V GOTO 100,100,100,100,100,100,100,100,100\n100 END\n";
    for profile in [Profile::Default, Profile::Speed] {
        assert!(asm(ten, profile).contains("JMP ($22)"), "{profile:?}");
        assert!(!asm(nine, profile).contains("JMP ($22)"), "{profile:?}");
    }
    let five = "10 ON V GOTO 100,100,100,100,100\n100 END\n";
    assert!(asm(five, Profile::Size).contains("JMP ($22)"));
}

#[test]
fn targets_take_the_high_byte_from_the_dispatch() {
    let mut src = String::from("10 READ V:GOTO 1300\n1300 ON V GOTO ");
    let targets: Vec<String> = (1310..=1400).step_by(10).map(|l| l.to_string()).collect();
    src.push_str(&targets.join(","));
    src.push_str("\n1305 END\n");
    for t in &targets {
        src.push_str(&format!("{t} PRINT 1/(V-{}):END\n", (t.parse::<u32>().unwrap() - 1300) / 10));
    }
    src.push_str("1500 DATA 4\n");
    let a = asm(&src, Profile::Default);
    assert!(a.contains("JMP ($22)"));
    assert_eq!(a.matches("STA $3A").count(), 2, "{a}");
    same_as_the_rom(&src);
}

fn subroutines(body: &str) -> String {
    let mut src = String::new();
    for line in [100, 200, 300, 400, 500, 600, 700, 900, 1000, 1100] {
        src.push_str(&format!("{line} PRINT \"S{line}\";:{body}\n"));
    }
    src
}

const TEN: &str = "100,200,300,400,500,600,700,900,1000,1100";

#[test]
fn on_gosub_through_a_table_returns_past_it() {
    let src = format!(
        "10 READ V:IF V<0 THEN END\n20 ON V GOSUB {TEN}\n30 PRINT \"R\";V:GOTO 10\n{}\
         2000 DATA 0,1,2,5,9,10,11,255,3.7,-1\n",
        subroutines("RETURN")
    );
    same_as_the_rom(&src);
    assert!(asm(&src, Profile::Default).contains("JSR __JMP22"));
}

#[test]
fn on_gosub_targets_with_loops_inside_a_loop() {
    let src = format!(
        "10 FOR I=1 TO 3:ON I GOSUB {TEN}:PRINT \"B\";I:NEXT\n20 END\n{}",
        subroutines("FOR J=1 TO 2:PRINT J;:NEXT:RETURN")
    );
    same_as_the_rom(&src);
}

#[test]
fn an_error_in_an_on_gosub_target_names_its_line() {
    let src = format!("10 V=4:ON V GOSUB {TEN}\n20 END\n{}", subroutines("X=1/(V-4):RETURN"));
    same_as_the_rom(&src);
}
