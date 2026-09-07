//! The six bytes that replace the withdrawn `$57-$5B`, and the two gates that guard them.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const LEGACY: [u8; 12] = [
    0x02, 0x0A, 0xA7, 0xA8, 0xA9, 0xAA, 0xAB, 0xAE, 0xF7, 0xF8, 0xF9, 0xFA,
];
/// Reachable only when the program touches neither tape nor RS-232.
const TAPE_GATED: [u8; 3] = [0x9E, 0x9F, 0xA6];

const SERIAL_GATED: [u8; 3] = [0xA3, 0xA4, 0xA5];

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

const SATURATE: &str = "\
10 GOSUB 900
20 FOR Q=1 TO 20
30 POKE XS+Q,RP:POKE YS+Q,RT:POKE CO+Q,RV:POKE EN+Q,RB
40 POKE DI+Q,NE:POKE XI+Q,NL:POKE YI+Q,SP:POKE HI+Q,SM
50 POKE AA+Q,RP:POKE BB+Q,RT:POKE CC+Q,RV:POKE DD+Q,NE
60 POKE EE+Q,NL:POKE FF+Q,SP:POKE GG+Q,SM:POKE HH+Q,RB
70 NEXT Q
80 PRINT RB;RP;RT;RV;NE;NL;SP;SM;XS;YS;CO;EN;DI;XI;YI;HI
85 PRINT AA;BB;CC;DD;EE;FF;GG;HH
90 END
900 RB=0:RP=5:RT=6:RV=7:NE=4:NL=10:SP=1:SM=1
910 XS=828:YS=838:CO=848:EN=858:DI=868:XI=878:YI=888:HI=898
915 AA=908:BB=918:CC=928:DD=938:EE=948:FF=958:GG=968:HH=978
920 RETURN
";

/// Two variables and nothing else: the allocator never gets past the head of the preference order.
const TINY: &str = "\
10 A=1:B=2
20 FOR Q=1 TO 10:A=A+B:POKE 1024+Q,A:NEXT
30 PRINT A;B
40 END
";

fn build(src: &str, profile: Profile) -> (String, Vec<u8>) {
    let tok = tokenize_program(src).unwrap_or_else(|e| panic!("tokenize: {e:?}"));
    let c = compile_with_options(
        &tok,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compile: {e}"));
    (c.asm, c.prg_bytes)
}

/// Every `label = $XX` equate the codegen emits for a zero-page cell.
fn zp_equates(asm: &str) -> Vec<(String, u8)> {
    let mut out = Vec::new();
    for line in asm.lines() {
        let line = line.trim();
        let Some((label, rhs)) = line.split_once(" = $") else {
            continue;
        };
        if label.contains(char::is_whitespace) || rhs.len() != 2 {
            continue;
        }
        if let Ok(a) = u8::from_str_radix(rhs, 16) {
            out.push((label.to_string(), a));
        }
    }
    out
}

fn zp_used(src: &str, profile: Profile) -> Vec<u8> {
    zp_equates(&build(src, profile).0)
        .into_iter()
        .map(|(_, a)| a)
        .collect()
}

#[test]
fn a_saturating_program_reaches_the_replacement_bytes() {
    for profile in PROFILES {
        let used = zp_used(SATURATE, profile);
        let reached: Vec<u8> = used
            .iter()
            .copied()
            .filter(|a| TAPE_GATED.contains(a) || SERIAL_GATED.contains(a))
            .collect();
        assert!(
            !reached.is_empty(),
            "{profile:?}: the replacement bytes are never handed out; \
             promotions were {used:02X?}"
        );

        let gated = SATURATE.replace("10 GOSUB 900", "5 OPEN 1,1,0\n10 GOSUB 900");
        let before = zp_used(&gated, profile).len();
        assert!(
            used.len() > before,
            "{profile:?}: {} promotions with the replacements and {before} without — \
             the pool did not actually grow: {used:02X?}",
            used.len()
        );
    }
}

#[test]
fn a_program_that_fits_in_the_legacy_twelve_never_sees_a_new_byte() {
    for profile in PROFILES {
        let used = zp_used(TINY, profile);
        for a in &used {
            assert!(
                LEGACY.contains(a) || (0x03..=0x06).contains(a) || (0xFB..=0xFE).contains(a),
                "{profile:?}: a two-variable program was given ${a:02X}, which is neither one \
                 of the legacy twelve nor a compiler pointer pair; the replacement bytes must \
                 be last in the preference order, not first: {used:02X?}"
            );
        }
    }
}

/// The tape gate.
#[test]
fn cassette_io_withdraws_every_replacement_byte() {
    let src = SATURATE.replace("10 GOSUB 900", "5 OPEN 1,1,0\n10 GOSUB 900");
    for profile in PROFILES {
        let used = zp_used(&src, profile);
        for a in &used {
            assert!(
                !TAPE_GATED.contains(a) && !SERIAL_GATED.contains(a),
                "{profile:?}: cassette program was given ${a:02X}: {used:02X?}"
            );
        }
    }
}

/// Keep serial-bus workspace separate from compiler scratch storage.
#[test]
fn disk_io_withdraws_only_the_serial_three() {
    let src = SATURATE.replace("10 GOSUB 900", "5 OPEN 1,8,15:CLOSE 1\n10 GOSUB 900");
    let mut saw_tape_gated = false;
    for profile in PROFILES {
        let used = zp_used(&src, profile);
        for a in &used {
            assert!(
                !SERIAL_GATED.contains(a),
                "{profile:?}: disk program was given serial scratch ${a:02X}: {used:02X?}"
            );
        }
        saw_tape_gated |= used.iter().any(|a| TAPE_GATED.contains(a));
    }
    assert!(
        saw_tape_gated,
        "a disk program lost $9E/$9F/$A6 as well — the split gate has collapsed into one"
    );
}

#[test]
fn file_io_in_either_arm_of_an_if_else_is_seen() {
    for head in [
        "5 IF Q=1 THEN OPEN 1,1,0 ELSE Q=2",
        "5 IF Q=1 THEN Q=2 ELSE OPEN 1,1,0",
    ] {
        let src = SATURATE.replace("10 GOSUB 900", &format!("{head}\n10 GOSUB 900"));
        for profile in PROFILES {
            let used = zp_used(&src, profile);
            for a in &used {
                assert!(
                    !TAPE_GATED.contains(a) && !SERIAL_GATED.contains(a),
                    "{profile:?}: `{head}` — cassette OPEN inside a conditional arm was \
                     missed and ${a:02X} was handed out: {used:02X?}"
                );
            }
        }
    }
}

#[test]
fn the_saturating_program_prints_the_machines_answer() {
    const EXPECTED: &str = " 0  5  6  7  4  10  1  1  828  838  848  858  868  878  888  898 \n \
                            908  918  928  938  948  958  968  978 \n";
    for profile in PROFILES {
        let (_, prg) = build(SATURATE, profile);
        let got = common::run_clean(&prg)
            .unwrap_or_else(|| panic!("{profile:?}: program did not reach a clean exit"));
        assert_eq!(got, EXPECTED, "{profile:?}: wrong values");
    }
}
