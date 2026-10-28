mod common;

use yabcompiler_core::romfloat::{Fac, fin};
use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn compile(src: &str, profile: Profile) -> Vec<u8> {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"))
    .prg_bytes
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

fn with_many_digit_items(lines: &str) -> String {
    let items: Vec<String> = (0..70).map(|i| (i * 37 % 1000).to_string()).collect();
    format!(
        "1 S=0:FOR I=1 TO 70:READ A:S=S+A:NEXT:PRINT S\n{lines}9000 DATA {}\n",
        items.join(",")
    )
}

fn same_as_the_rom(src: &str) {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 40_000_000).expect("runs in the interpreter");
    let want = (common::petscii_to_string(&r.output), r.error_code, r.error_line);
    for profile in PROFILES {
        let r = emu64::run_prg_to_end(&compile(src, profile), 40_000_000).expect("runs");
        assert_eq!(
            (common::petscii_to_string(&r.output), r.error_code, r.error_line),
            want,
            "{profile:?}:\n{src}"
        );
    }
}

#[test]
fn fin_of_digits_is_the_exact_integer() {
    for n in 0u32..=65535 {
        let want = Fac::from_f64(f64::from(n)).unwrap();
        assert_eq!(want.round, 0);
        for text in [n.to_string(), format!("0{n}"), format!("{n:06}")] {
            assert_eq!(fin(text.as_bytes()), Some(want), "{text}");
        }
    }
}

#[test]
fn val_of_every_16_bit_value_is_exact() {
    let src = "10 FOR N=0 TO 65535:A$=MID$(STR$(N),2)\n\
               20 IF VAL(A$)<>N OR VAL(\"0\"+A$)<>N THEN PRINT N\n\
               30 NEXT:PRINT \"DONE\"\n";
    assert!(asm(src, Profile::Speed).contains("__VAL_DIGITS:"));
    let r = emu64::run_prg_to_end(&compile(src, Profile::Speed), 400_000_000).expect("runs");
    assert_eq!(common::petscii_to_string(&r.output), "DONE\n");
    assert!(r.clean_exit);
}

#[test]
fn read_digits_and_the_texts_fin_keeps() {
    let src = with_many_digit_items(
        "10 READ T$:PRINT T$\n\
         20 FOR I=1 TO 29:READ A:PRINT A;:NEXT:PRINT\n\
         30 FOR I=1 TO 6:READ N,A,B%:PRINT A=N;B%=N;:NEXT:PRINT\n\
         40 RESTORE:READ T$,A,B:PRINT T$;A;B\n\
         50 DATA HELLO,0,1,9,10,99,255,256,999,1000,4095,32767,32768,32769,65534,65535\n\
         60 DATA 65536,99999,100000,007,0000,-5,-0,1.5,.5,1E3,+3, 12,,123456789\n\
         70 DATA 0,0,0,255,255,255,32767,32767,32767,65535,65535,-1,7,007,7,300,300,300\n",
    );
    assert!(asm(&src, Profile::Default).contains("__READ_DIGITS:"));
    same_as_the_rom(&src);
}

#[test]
fn a_bad_item_after_digit_items_is_still_an_error() {
    same_as_the_rom(&with_many_digit_items(
        "10 READ A,B,C:PRINT A;B;C\n\
         20 READ D:PRINT D\n\
         30 DATA 1,22,333\n\
         40 DATA 4X\n",
    ));
}

#[test]
fn a_poke_loader_reads_its_bytes() {
    same_as_the_rom(&with_many_digit_items(
        "10 DIM D%(9):FOR I=0 TO 9:READ D%(I):POKE 832+I,D%(I):NEXT\n\
         20 FOR I=0 TO 9:PRINT PEEK(832+I);:NEXT:READ S$:PRINT S$\n\
         30 DATA 0,1,2,127,128,129,254,255,16,32,DONE\n",
    ));
}

#[test]
fn spaces_inside_and_after_digits_are_skipped_as_fin_does() {
    let src = with_many_digit_items(
        "10 READ A,B%,C$:PRINT A;B%;C$\n\
         20 PRINT VAL(\"1 2\");VAL(\" 34 \")\n\
         30 DATA 7 ,1 2,8 \n",
    );
    assert!(asm(&src, Profile::Default).contains("__DIGITS_LIN:"));
    same_as_the_rom(&src);
}

#[test]
fn the_profiles_choose_the_shortcut_by_pool_size() {
    let few = "10 READ A,B$:PRINT A;B$\n20 DATA 12,AB\n";
    assert!(!asm(few, Profile::Default).contains("__DIGITS_LIN"));
    assert!(asm(few, Profile::Speed).contains("__READ_DIGITS:"));
    let many = with_many_digit_items("10 READ B$:PRINT B$\n20 DATA X\n");
    assert!(asm(&many, Profile::Default).contains("__DIGITS_LIN:"));
    assert!(!asm(&many, Profile::Size).contains("__DIGITS_LIN"));
}

#[test]
fn a_quoted_digit_item_is_still_refused() {
    let src = with_many_digit_items("10 READ A$,B:PRINT A$;B\n20 READ C:PRINT C\n30 DATA \"7\",8,\"9\"\n");
    assert!(!asm(&src, Profile::Default).contains("__DIGITS_LIN"));
    same_as_the_rom(&src);
}

#[test]
fn integer_targets_take_digits_without_fac() {
    let items: Vec<String> = (0..70).map(|i| (i * 37 % 1000).to_string()).collect();
    for strings in ["5 READ T$:PRINT T$\n6 DATA HELLO\n", ""] {
        let src = format!(
            "1 S=0:FOR I=1 TO 70:READ A%:S=S+A%:NEXT:PRINT S\n{strings}\
             10 FOR I=0 TO 9:READ B%:PRINT B%;:NEXT:PRINT\n\
             20 DATA 7,-5,1.5,+3, 12,-0,9999,-9999,1E3,-1.5\n\
             30 READ A%:PRINT A%\n40 DATA 40000\n\
             9000 DATA {}\n",
            items.join(",")
        );
        let profile = if strings.is_empty() {
            Profile::Speed
        } else {
            Profile::Default
        };
        let code = asm(&src, profile);
        assert!(code.contains("__READ_DIGITS_INT:"), "{code}");
        assert!(!code.contains("__DIGITS_GIVAYF"), "{code}");
        same_as_the_rom(&src);
    }
}
