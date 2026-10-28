mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn compile(src: &str, profile: Profile) -> yabcompiler_core::Compiled {
    compile_with_options(
        &tokenize_program(src).expect("tokenizes"),
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{profile:?}: {e}\n{src}"))
}

fn output(src: &str, expected: &str) {
    for profile in PROFILES {
        let c = compile(src, profile);
        let r = emu64::run_prg_to_end(&c.prg_bytes, 80_000_000).expect("runs");
        assert!(r.clean_exit, "{profile:?}: {:?}\n{src}", r.error_banner());
        let text = common::petscii_to_string(&r.output);
        assert_eq!(
            text.split_whitespace().collect::<Vec<_>>().join(" "),
            expected,
            "{profile:?}\n{src}"
        );
    }
}

const BUBBLE: &str = "10 W=1024\n20 FOR I=0 TO 239:POKE W+I,240-I:NEXT\n30 M=240:S=1\n40 M=M-1:IF M=0 THEN 120\n50 IF S=0 THEN 120\n60 S=0:X=-1\n70 X=X+1:IF X=M THEN 40\n80 C=PEEK(W+X+1):D=PEEK(W+X):IF C<D THEN GOSUB 100\n90 GOTO 70\n100 POKE W+X+1,D:POKE W+X,C:S=1:RETURN\n120 PRINT PEEK(W);PEEK(W+1);PEEK(W+119);PEEK(W+238);PEEK(W+239)\n";

#[test]
fn a01_bubble_promotes_both_guarded_counters_and_sorts() {
    let asm = compile(BUBBLE, Profile::Default).asm;
    assert!(asm.contains("VI_M") && asm.contains("VI_X"));
    output(BUBBLE, "1 2 120 239 240");
}

#[test]
fn a02_equality_down_counter_promotes() {
    let src = "10 M=240\n20 M=M-1:IF M=0 THEN 40\n30 GOTO 20\n40 PRINT M\n";
    let asm = compile(src, Profile::Default).asm;
    // The cost gate may retain a Float home with a fast integer shadow.
    assert!(asm.contains("VI_M") || asm.contains("ZSI_V_M"));
    output(src, "0");
}

#[test]
fn a03_inequality_guard_admits_large_step() {
    let src = "10 T=0\n20 POKE 12800+T,255:T=T+100:IF T<1000 THEN 20\n30 PRINT T\n";
    assert!(compile(src, Profile::Default).asm.contains("VI_T"));
    output(src, "1000");
}

#[test]
fn a04_equality_guard_does_not_bound_a_step_over() {
    // X steps over M; the later X>300 exit is what bounds it.
    let src =
        "10 M=240:X=1\n20 X=X+2:IF X=M THEN 40\n25 IF X>300 THEN 40\n30 GOTO 20\n40 PRINT X\n";
    output(src, "301");
    // Without that exit nothing bounds X, and it passes 32767.
    let src = "10 M=240:X=1\n20 X=X+2:K=K+1:IF X=M THEN 40\n25 IF K<20000 THEN 20\n40 PRINT X\n";
    assert!(!compile(src, Profile::Default).asm.contains("VI_X"));
    output(src, "40001");
}

#[test]
fn a05_equality_guard_does_not_bound_a_wrong_side_start() {
    // X starts past M; the later X>300 exit is what bounds it.
    let src =
        "10 M=10:X=20\n20 X=X+1:IF X=M THEN 40\n25 IF X>300 THEN 40\n30 GOTO 20\n40 PRINT X\n";
    output(src, "301");
    // Without that exit nothing bounds X, and it passes 32767.
    let src = "10 M=10:X=20\n20 X=X+1:K=K+1:IF X=M THEN 40\n25 IF K<40000 THEN 20\n40 PRINT X\n";
    assert!(!compile(src, Profile::Default).asm.contains("VI_X"));
    output(src, "40020");
}

fn quantity_error(src: &str, line: u16) {
    for profile in PROFILES {
        let c = compile(src, profile);
        let r = emu64::run_prg_to_end(&c.prg_bytes, 2_000_000).expect("runs");
        assert_eq!(
            r.error_code,
            Some(14),
            "{profile:?}: {:?}",
            r.error_banner()
        );
        assert_eq!(r.error_line, Some(line), "{profile:?}");
        assert!(r.output.is_empty(), "must stop before PRINT");
    }
}

#[test]
fn a06_int16_increment_traps_at_upper_boundary() {
    quantity_error("10 S%=32767\n20 S%=S%+1\n30 PRINT S%\n", 20);
}

#[test]
fn a07_int16_decrement_traps_at_lower_boundary() {
    quantity_error("10 S%=-32768\n20 S%=S%-1\n30 PRINT S%\n", 20);
}

#[test]
fn a08_int16_step_keeps_fast_path_and_valid_boundaries() {
    // READ prevents constant folding from hiding the increment under test.
    let src = "10 READ S%\n20 S%=S%+1\n30 PRINT S%\n40 DATA 32766\n";
    assert!(compile(src, Profile::Default).asm.contains("INC VI_S"));
    output(src, "32767");
    output(
        "10 READ S%\n20 S%=S%-1\n30 PRINT S%\n40 DATA -32767\n",
        "-32768",
    );
}

#[test]
fn a09_trapped_step_does_not_modify_destination() {
    for (initial, op) in [(32767, "+"), (-32768, "-")] {
        let src = format!(
            "10 ON ERROR GOTO 100\n20 S%={initial}\n30 S%=S%{op}1\n40 PRINT S%:END\n100 PRINT S%;EL:RESUME NEXT\n"
        );
        output(&src, &format!("{initial} 30 {initial}"));
    }
}

#[test]
fn a10_int_assignment_checks_final_value_not_intermediate() {
    output(
        "10 READ B%,C%,D%\n20 A%=B%+C%-D%:PRINT A%\n30 DATA 32767,1,1\n",
        "32767",
    );
    output(
        "10 READ B%,C%,D%\n20 A%=B%-C%+D%:PRINT A%\n30 DATA -32768,1,1\n",
        "-32768",
    );
}

#[test]
fn a11_wide_counter_byte_expression_avoids_word_temporaries() {
    let asm = compile(
        "10 W=1024\n20 FOR I=0 TO 499:POKE W+I,(I AND 31)+1:NEXT\n",
        Profile::Default,
    )
    .asm;
    let pos = asm.find("AND #$1F").expect("mask emitted directly");
    let before = &asm[..pos];
    assert!(before.lines().rev().take(3).any(|l| l.contains("LDA ")));
    let tail = &asm[pos..];
    let adc = tail.find("ADC #$01").expect("byte increment");
    assert!(!tail[..adc].contains("STA T"));
    assert!(tail[..adc].contains("CLC"));
}

#[test]
fn a12_wide_counter_byte_values_survive_page_boundaries() {
    for (start, end) in [(0, 255), (0, 256), (0, 499), (0, 511), (0, 512), (7, 506)] {
        let src = format!(
            "10 READ K:W=1024\n20 FOR I=K TO K+{}:POKE W+I,(I AND 31)+1:NEXT\n30 S=0:FOR J=K TO K+{}:S=S+PEEK(W+J):NEXT\n40 PRINT S;PEEK(W+K);PEEK(W+K+1);PEEK(W+K+31);PEEK(W+K+32);PEEK(W+{});PEEK(W+{})\n50 DATA {start}\n",
            end - start,
            end - start,
            end - 1,
            end
        );
        let value = |i: i32| (i & 31) + 1;
        let sum: i32 = (start..=end).map(value).sum();
        let expected = format!(
            "{sum} {} {} {} {} {} {}",
            value(start),
            value(start + 1),
            value(start + 31),
            value(start + 32),
            value(end - 1),
            value(end)
        );
        output(&src, &expected);
    }
}

#[test]
fn a13_other_byte_consumers_keep_full_counter_values() {
    output(
        "10 S=0\n20 FOR I=1 TO 500:IF I>250 THEN S=S+1\n30 NEXT:PRINT S;I\n",
        "250 501",
    );
    output(
        "10 K=1024\n20 FOR I=0 TO 511:POKE K+I,123:NEXT\n30 FOR I=0 TO 511:POKE K+I,0:NEXT\n40 PRINT PEEK(K);PEEK(K+255);PEEK(K+256);PEEK(K+511)\n",
        "0 0 0 0",
    );
    output(
        "10 K=1024:POKE K+256,123\n20 FOR I=0 TO 511:IF PEEK(K+I)=123 THEN PRINT I\n30 NEXT\n",
        "256",
    );
}

#[test]
fn a14_clock_literal_sets_numeric_time() {
    output("10 TI$=\"001122\"\n20 PRINT TI\n", "40920");
    output("10 TI$=\"999999\"\n20 PRINT TI\n", "4969124");
}

#[test]
fn a15_zero_clock_literal_folds_without_runtime_parser() {
    let src = "10 TI$=\"000000\"\n20 PRINT TI\n";
    for profile in PROFILES {
        let asm = compile(src, profile).asm;
        assert!(asm.contains("STA $A0"));
        assert!(!asm.contains("JSR __SET_TI"));
    }
    output(src, "0");
}

#[test]
fn a16_pseudo_digits_keep_rom_compatible_clock_helper() {
    let src = "10 TI$=\"OOOOOO\"\n20 PRINT TI\n";
    assert!(compile(src, Profile::Default).asm.contains("JSR __SET_TI"));
    output(src, "503804");
}

#[test]
fn a17_dynamic_clock_string_keeps_helper() {
    let src = "10 READ A$\n20 TI$=A$\n30 PRINT TI\n40 DATA 001122\n";
    assert!(compile(src, Profile::Default).asm.contains("JSR __SET_TI"));
    output(src, "40920");
}

#[test]
fn a18_monotone_cap_keeps_bubble_comparison_narrow() {
    let src = "10 M=8\n20 M=M-1:IF M=0 THEN 80\n30 X=-1\n40 X=X+1:IF X=M THEN 20\n50 S=S+X\n60 GOTO 40\n80 PRINT S\n";
    let asm = compile(src, Profile::Default).asm;
    assert!(asm.contains("CMP VI_M"), "must retain low-byte comparison");
    output(src, "56");
}

#[test]
fn monotone_cap_rejects_all_measured_false_bounds() {
    for (src, expected) in [
        (
            "10 M=5\n20 X=100\n30 X=X+1:IF X=M THEN 50\n40 IF X<400 THEN 30\n50 PRINT X\n",
            "400",
        ),
        (
            "10 M=5\n20 X=1000\n30 X=X+1:IF X=M THEN 50\n40 IF X<2000 THEN 30\n50 PRINT X\n",
            "2000",
        ),
        (
            "10 M=250\n20 X=0\n30 X=X+1:IF X=M THEN 60\n40 IF X=100 THEN X=X+400\n50 IF X<900 THEN 30\n60 PRINT X\n",
            "900",
        ),
        (
            "10 M=5\n20 X=100\n30 X=X+1:IF X=M THEN 80\n35 IF X>400 THEN 90\n40 GOTO 30\n80 PRINT \"EQ\";X:END\n90 PRINT \"END\";X\n",
            "END 401",
        ),
    ] {
        output(src, expected);
    }
    quantity_error(
        "10 M=5\n20 X=100\n30 X=X+1:IF X=M THEN 50\n40 POKE 1024,X\n45 IF X<400 THEN 30\n50 PRINT X\n",
        40,
    );
}

#[test]
fn a19_inverted_range_asserts_in_debug_and_takes_hull_in_release() {
    use yabcompiler_core::dataflow::IntRange;
    if cfg!(debug_assertions) {
        assert!(std::panic::catch_unwind(|| IntRange::new(100, 5)).is_err());
    } else {
        let r = IntRange::new(100, 5);

        assert_eq!((r.min, r.max), (5, 100));
        let r = IntRange::new(400, 5);
        assert_eq!((r.min, r.max), (5, 400));
        assert!(!r.fits_u8());
        assert!(!IntRange::new(40000, -40000).fits_i16());
    }
}
