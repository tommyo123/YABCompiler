mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn same_as_the_rom(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 20_000_000).expect("runs in the interpreter");
    let want = (common::petscii_to_string(&r.output), r.error_code, r.error_line);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let c = compile_with_options(
            &prg,
            CompileOptions {
                profile,
                ..CompileOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
        let r = emu64::run_prg_to_end(&c.prg_bytes, 20_000_000).expect("runs");
        assert_eq!(
            (common::petscii_to_string(&r.output), r.error_code, r.error_line),
            want,
            "{profile:?}:\n{src}"
        );
    }
    want.0
}

#[test]
fn a_waiting_key_and_then_none() {
    let out = same_as_the_rom(
        "10 POKE 631,65:POKE 632,193:POKE 633,49:POKE 198,3\n\
         20 FOR I=1 TO 5:GET A$:PRINT LEN(A$);A$;\"/\";:IF A$<>\"\" THEN PRINT ASC(A$);\n\
         30 NEXT:PRINT\n\
         40 GET B$:IF B$=\"\" THEN PRINT \"EMPTY\"\n",
    );
    assert!(out.contains("1 A/ 65") && out.contains("0 /"), "{out}");
}

#[test]
fn get_keeps_each_key_in_its_own_variable() {
    let out = same_as_the_rom(
        "10 POKE 631,72:POKE 632,73:POKE 198,2\n\
         20 GET A$:GET B$:GET C$:PRINT A$;B$;C$;LEN(A$+B$+C$)\n",
    );
    assert!(out.starts_with("HI 2"), "{out}");
}

#[test]
fn get_from_a_keyboard_file() {
    let out = same_as_the_rom(
        "10 POKE 631,65:POKE 632,66:POKE 198,2:OPEN 1,0\n\
         20 GET#1,A$,B$,C$:CLOSE 1:PRINT A$;B$;LEN(A$);LEN(B$);LEN(C$)\n",
    );
    assert!(out.starts_with("AB 1  1  0"), "{out}");
}

#[test]
fn get_numbers_from_a_keyboard_file() {
    same_as_the_rom(
        "10 POKE 631,53:POKE 632,55:POKE 198,2:OPEN 1,0\n\
         20 GET#1,A%,B:CLOSE 1:PRINT A%;B\n",
    );
}

#[test]
fn get_from_a_file_in_separate_statements() {
    let out = same_as_the_rom(
        "10 POKE 631,88:POKE 632,89:POKE 633,90:POKE 198,3:OPEN 2,0\n\
         20 GET#2,A$:GET#2,B$:S$=A$+B$:GET#2,A$:CLOSE 2:PRINT S$;A$;B$\n",
    );
    assert!(out.starts_with("XYZY"), "{out}");
}

#[test]
fn a_letter_into_a_number_from_a_keyboard_file() {
    same_as_the_rom(
        "10 POKE 631,49:POKE 632,65:POKE 198,2:OPEN 1,0\n\
         20 GET#1,A:PRINT A:GET#1,B:PRINT B\n",
    );
}

#[test]
fn format_characters_into_numbers_from_a_keyboard_file() {
    same_as_the_rom(
        "10 FOR I=0 TO 4:POKE 631+I,ASC(MID$(\"+-.E9\",I+1,1)):NEXT:POKE 198,5:OPEN 1,0\n\
         20 FOR I=1 TO 6:GET#1,A%:PRINT A%;:NEXT:CLOSE 1:PRINT\n",
    );
}

#[test]
fn a_letter_into_a_number_from_the_keyboard() {
    same_as_the_rom("10 POKE 631,49:POKE 632,65:POKE 198,2\n20 GET A:PRINT A:GET B:PRINT B\n");
}
