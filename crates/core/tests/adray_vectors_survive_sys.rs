//! `$03-$06` belong to the machine code, not to the compiler, in any program that calls machine code.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn asm_of(src: &str, profile: Profile) -> String {
    let tok = tokenize_program(src).unwrap_or_else(|e| panic!("tokenize: {e:?}"));
    compile_with_options(
        &tok,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compile: {e}"))
    .asm
}

fn compiler_zp_operands(asm: &str) -> std::collections::BTreeSet<u8> {
    let mut out = std::collections::BTreeSet::new();
    for line in asm.lines() {
        let body = line.split(';').next().unwrap_or("");
        let t = body.trim_start();
        let is_insn = body.starts_with(char::is_whitespace)
            && t.len() > 3
            && t.as_bytes()[..3].iter().all(|c| c.is_ascii_uppercase())
            && t[3..].starts_with(char::is_whitespace);
        if !is_insn {
            continue;
        }
        let bytes = body.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$' && !(i > 0 && bytes[i - 1] == b'#') {
                let n = bytes[i + 1..]
                    .iter()
                    .take_while(|c| c.is_ascii_hexdigit())
                    .count();
                if n == 2 {
                    out.insert(u8::from_str_radix(&body[i + 1..i + 3], 16).unwrap());
                }
                i += 1 + n.max(1);
            } else {
                i += 1;
            }
        }
    }
    out
}

const STRINGY: &str = "\
10 A$=\"HELLO\":B$=\"WORLD\"\n\
20 C$=A$+B$\n\
30 IF LEFT$(C$,5)=A$ THEN PRINT C$\n\
40 FOR I=1 TO 3:D$=MID$(C$,I,2):PRINT D$:NEXT\n";

fn with_sys(tail: &str) -> String {
    format!("{STRINGY}50 SYS 49152{tail}\n")
}

#[test]
fn a_sys_program_does_not_park_a_pointer_on_the_adray_vectors() {
    for profile in PROFILES {
        let used = compiler_zp_operands(&asm_of(&with_sys(""), profile));
        for a in 0x03u8..=0x06 {
            assert!(
                !used.contains(&a),
                "{profile:?}: ${a:02X} is an ADRAY vector byte and this program calls \
                 machine code, which is entitled to `JSR $03`/`JSR $05`\nclaimed: {used:02X?}"
            );
        }
    }
}

#[test]
fn a_usr_program_does_not_park_a_pointer_on_the_adray_vectors() {
    let src = format!("{STRINGY}50 POKE 785,192:POKE 786,0:X=USR(7):PRINT X\n");
    for profile in PROFILES {
        let used = compiler_zp_operands(&asm_of(&src, profile));
        for a in 0x03u8..=0x06 {
            assert!(!used.contains(&a), "{profile:?}: USR program claims ${a:02X}");
        }
    }
}

#[test]
fn a_sys_free_program_still_uses_the_vectors() {
    let used = compiler_zp_operands(&asm_of(STRINGY, Profile::Default));
    assert!(
        (0x03u8..=0x06).any(|a| used.contains(&a)),
        "the string-operation pointers should still default to $03-$06 \
         when nothing calls machine code\nclaimed: {used:02X?}"
    );
}

#[test]
fn a_sys_free_program_is_byte_for_byte_unchanged() {
    for profile in PROFILES {
        let a = asm_of(STRINGY, profile);
        let b = asm_of(STRINGY, profile);
        assert_eq!(a, b, "{profile:?}: compilation is not reproducible");
        let used = compiler_zp_operands(&a);
        assert!(
            !used.is_empty(),
            "{profile:?}: sanity — this program does claim zero page"
        );
    }
}

#[test]
fn the_relocated_pointers_stay_in_zero_page() {
    let asm = asm_of(&with_sys(""), Profile::Default);
    let used = compiler_zp_operands(&asm);
    // The pool's own cells, from `compute_zp_pool_order`.
    let pool: Vec<u8> = (0xA7..=0xAB)
        .chain(std::iter::once(0xAE))
        .chain(0xF7..=0xFA)
        .chain([0x0A, 0x02])
        .chain([0x9E, 0x9F])
        .chain(0xA3..=0xA6)
        .chain(0xFB..=0xFE)
        .collect();
    let adjacent = pool
        .iter()
        .filter(|a| used.contains(a) && used.contains(&(**a + 1)))
        .count();
    assert!(
        adjacent >= 2,
        "expected the displaced pointer pairs on adjacent zero-page cells; \
         claimed: {used:02X?}"
    );
}

#[test]
fn the_string_operations_still_work_after_the_move() {
    let src = "\
10 POKE 49152,96\n\
20 A$=\"HELLO\":B$=\"WORLD\"\n\
30 C$=A$+B$\n\
40 SYS 49152\n\
50 PRINT C$\n\
60 IF LEFT$(C$,5)=A$ THEN PRINT \"OK\"\n";
    for profile in PROFILES {
        let tok = tokenize_program(src).expect("tokenizes");
        let c = compile_with_options(
            &tok,
            CompileOptions {
                profile,
                ..CompileOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("compile: {e}"));
        let got = common::run_clean(&c.prg_bytes)
            .unwrap_or_else(|| panic!("{profile:?}: runs to a clean exit"));
        assert_eq!(got, "HELLOWORLD\nOK\n", "{profile:?}");
    }
}
