//! Keep BASIC floating-point workspace out of the zero-page allocation pool.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

/// Sixteen live integers plus `2^RM`.
const PRESSURE: &str = "\
10 GOSUB 900
20 FOR Q=1 TO 50
30 RM=RB+Q:RS=2^RM:RR=255-RS
40 POKE XS+RM,RP:POKE YS+RM,RT:POKE CO+RM,RV:POKE EN+RM,RB
50 POKE DI+RM,NE:POKE XI+RM,NL:POKE YI+RM,SP:POKE HI+RM,SM
55 POKE AA+RM,RP:POKE BB+RM,RT:POKE CC+RM,RV:POKE DD+RM,NE
57 POKE EE+RM,NL:POKE FF+RM,SP:POKE GG+RM,SM:POKE HH+RM,RB
60 NEXT Q
70 PRINT RB;RP;RT;RV;NE;NL;SP;SM;XS;YS;CO;EN;DI;XI;YI;HI
75 PRINT AA;BB;CC;DD;EE;FF;GG;HH
80 END
900 RB=0:RP=5:RT=6:RV=7:NE=4:NL=10:SP=1:SM=1
910 XS=828:YS=838:CO=848:EN=858:DI=868:XI=878:YI=888:HI=898
915 AA=908:BB=918:CC=928:DD=938:EE=948:FF=958:GG=968:HH=978
920 RETURN
";

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

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

/// Every `label = $XX` equate the codegen emits for a zero-page promotion.
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

/// Integer promotion preserves BASIC floating-point workspace.
#[test]
fn no_variable_is_parked_in_the_rom_float_work_area() {
    for profile in PROFILES {
        let (asm, _) = build(PRESSURE, profile);
        let bad: Vec<_> = zp_equates(&asm)
            .into_iter()
            .filter(|&(_, a)| (0x57..=0x60).contains(&a))
            .collect();
        assert!(
            bad.is_empty(),
            "{profile:?}: promoted into BASIC's float scratch $57-$60: {bad:?}"
        );
    }
}

#[test]
fn the_safe_pool_is_still_handed_out_in_full() {
    const SAFE: [u8; 18] = [
        0x02, 0x0A, 0x9E, 0x9F, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xAB, 0xAE, 0xF7,
        0xF8, 0xF9, 0xFA,
    ];
    for profile in PROFILES {
        let (asm, _) = build(PRESSURE, profile);
        let used: Vec<u8> = zp_equates(&asm).into_iter().map(|(_, a)| a).collect();
        assert!(
            used.len() >= 8,
            "{profile:?}: only {} zero-page promotions, the pool has stopped working: {used:?}",
            used.len()
        );
        for a in &used {
            assert!(
                SAFE.contains(a) || (0xFB..=0xFE).contains(a) || (0x03..=0x06).contains(a),
                "{profile:?}: promotion to ${a:02X}, which is neither a safe pool byte nor a \
                 relocated compiler pair"
            );
        }
    }
}

#[test]
fn the_pressure_program_prints_the_machines_answer() {

    const EXPECTED: &str = " 0  5  6  7  4  10  1  1  828  838  848  858  868  878  888  898 \n \
                            908  918  928  938  948  958  968  978 \n";
    for profile in PROFILES {
        let (_, prg) = build(PRESSURE, profile);
        let got = common::run_clean(&prg)
            .unwrap_or_else(|| panic!("{profile:?}: program did not reach a clean exit"));
        assert_eq!(got, EXPECTED, "{profile:?}: wrong values");
    }
}
