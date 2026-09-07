//! Byte consumers must reject values that wrap into 0..=255 modulo 65536.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(source: &str, profile: Profile) -> yabcompiler_core::Compiled {
    compile_with_options(
        &tokenize_program(source).unwrap(),
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap()
}

fn assert_error(source: &str, line: u16) {
    for profile in PROFILES {
        let compiled = build(source, profile);
        let result = emu64::run_prg_to_end(&compiled.prg_bytes, common::MAX_INSNS).unwrap();
        assert_eq!(
            result.error_banner(),
            Some(format!("?ILLEGAL QUANTITY  ERROR IN {line}")),
            "{profile:?}: {source}"
        );
    }
}

#[test]
fn wrapped_byte_values_raise_at_both_ends_of_each_alias_interval() {
    // Includes adjacent values outside each alias interval: those must also raise, but already did so through the existing high-byte check.
    for data in [
        "30000,30000,5535",
        "30000,30000,5536",
        "30000,30000,5601",
        "30000,30000,5791",
        "30000,30000,5792",
        "-32768,-32768,-1",
        "-32768,-32768,0",
        "-32768,-32768,65",
        "-32768,-32768,255",
        "-32768,-32768,256",
    ] {
        assert_error(
            &format!("10 READ A%,B%,C%\n20 POKE 1024,A%+B%+C%\n30 PRINT \"Z\"\n40 DATA {data}\n"),
            20,
        );
    }
    assert_error(
        "10 READ A%,B%\n20 POKE 1024,A%+B%\n30 PRINT \"Z\"\n40 DATA -32768,-32768\n",
        20,
    );
    assert_error(
        "10 READ A%,B%,C%\n20 POKE 1024,A%-B%-C%\n30 PRINT \"Z\"\n40 DATA 30000,-30000,-5601\n",
        20,
    );
}

#[test]
fn chr_uses_the_same_byte_window() {
    assert_error(
        "10 READ A%,B%,C%\n20 PRINT CHR$(A%+B%+C%)\n30 PRINT \"Z\"\n40 DATA 30000,30000,5601\n",
        20,
    );
}

#[test]
fn an_unbounded_product_cannot_skip_the_byte_check() {
    // Range analysis drops this product because its upper bound exceeds the fact limit.
    assert_error(
        "10 POKE 2,0\n20 A%=PEEK(2)+64\n30 POKE 1024,A%*A%*A%\n40 PRINT \"Z\"\n",
        30,
    );
}

#[test]
fn a_range_can_alias_a_nonzero_byte_without_containing_a_multiple_of_65536() {

    for source in [
        "10 POKE 2,0\n20 A%=PEEK(2)+30000\n30 POKE 1024,A%+32767+2770\n40 PRINT PEEK(1024)\n",
        "10 POKE 2,0\n20 A%=PEEK(2)-32767\n30 POKE 1024,A%+(-32768)\n40 PRINT PEEK(1024)\n",
    ] {
        assert_error(source, 30);
    }
}

#[test]
fn a_legal_result_is_not_rejected_when_the_range_is_wide() {
    for (data, expected) in [
        ("32767,-32768,66", " 65 \n"),
        ("32767,-32768,256", " 255 \n"),
    ] {
        let source = format!(
            "10 READ A%,B%,C%\n20 POKE 1024,A%+B%+C%\n30 PRINT PEEK(1024)\n40 DATA {data}\n"
        );
        for profile in PROFILES {
            let compiled = build(&source, profile);
            let result = emu64::run_prg_to_end(&compiled.prg_bytes, common::MAX_INSNS).unwrap();
            assert_eq!(result.error_banner(), None, "{profile:?}");
            assert_eq!(
                common::petscii_to_string(&result.output),
                expected,
                "{profile:?}"
            );
        }
    }

    let source = "10 READ A%,B%,C%,D%\n20 POKE 1024,A%+B%+C%+D%\n30 PRINT PEEK(1024)\n40 DATA 32767,32767,-32767,-32702\n";
    for profile in PROFILES {
        let result =
            emu64::run_prg_to_end(&build(source, profile).prg_bytes, common::MAX_INSNS).unwrap();
        assert_eq!(result.error_banner(), None, "{profile:?}");
        assert_eq!(
            common::petscii_to_string(&result.output),
            " 65 \n",
            "{profile:?}"
        );
    }
}

#[test]
fn ranges_that_cannot_alias_a_byte_keep_the_integer_path() {
    // Both are outside AYINT's signed window.
    for source in [
        "10 POKE 2,0\n20 A%=PEEK(2)+30000\n30 POKE 1024,A%+30000\n40 PRINT \"Z\"\n",
        "10 POKE 2,0\n20 A%=PEEK(2)-32512\n30 POKE 1024,A%+(-32768)\n40 PRINT \"Z\"\n",
    ] {
        assert_error(source, 30);
        for profile in PROFILES {
            let asm = build(source, profile).asm;
            assert!(
                !asm.contains("__FAC_BYTE"),
                "{profile:?}: safe range lost its integer path\n{asm}"
            );
        }
    }
}

// Calls prevent numeric range inference while integer storage remains valid.
const PROMOTED_SCALARS: &str = "10 FOR A=1 TO 32\n20 GOSUB 1000\n30 FOR D=1 TO 10\n40 GOSUB 1000\n50 PRINT TAB(A+D)\n60 NEXT D\n70 PRINT TAB(A+D-3)\n80 NEXT A\n90 END\n1000 POKE 59467,16\n1010 POKE 59466,15\n1020 POKE 59464,100\n1030 POKE 59467,0\n1040 RETURN\n";
// Keep this provenance control outside the independently checked linear-array family.
const PROMOTED_ARRAY: &str = "10 DIM A(2)\n20 READ A(0),A(1),A(2)\n30 POKE 1024,A(0)*3+5\n40 PRINT PEEK(1024)\n50 DATA 20,30,15\n";

#[test]
fn promoted_source_floats_do_not_become_explicit_integer_carriers() {
    for profile in PROFILES {
        let scalar = build(PROMOTED_SCALARS, profile).asm;
        assert!(
            scalar.contains("VI_A"),
            "control must actually promote A: {scalar}"
        );
        assert!(
            !scalar.contains("__FAC_BYTE"),
            "{profile:?}: inferred scalars entered the new gate\n{scalar}"
        );
        let array = build(PROMOTED_ARRAY, profile).asm;
        assert!(
            array.contains("__ARR_A_I:"),
            "control must actually promote A(): {array}"
        );
        assert!(
            !array.contains("__FAC_BYTE"),
            "{profile:?}: inferred array entered the new gate\n{array}"
        );

        let explicit_array = build(&PROMOTED_ARRAY.replace("A(", "A%("), profile).asm;
        assert!(
            explicit_array.contains("__FAC_BYTE"),
            "{profile:?}: source A%() must keep the gate\n{explicit_array}"
        );
    }
}

#[test]
fn scalar_and_array_source_namespaces_remain_separate() {
    let float_array_with_integer_scalar = PROMOTED_ARRAY
        .replace("20 READ", "15 READ A%\n20 READ")
        .replace("40 PRINT PEEK(1024)", "40 PRINT PEEK(1024);A%")
        .replace("50 DATA 20,30,15", "50 DATA 0,20,30,15");
    let float_scalar_with_integer_array = format!(
        "5 DIM A%(0):READ A%(0)\n{}",
        PROMOTED_SCALARS.replace("90 END", "85 PRINT A%(0)\n90 END")
    ) + "2000 DATA 0\n";
    for profile in PROFILES {
        for source in [
            &float_array_with_integer_scalar,
            &float_scalar_with_integer_array,
        ] {
            let asm = build(source, profile).asm;
            assert!(
                !asm.contains("__FAC_BYTE"),
                "{profile:?}: explicit kind crossed scalar/array namespaces\n{asm}"
            );
        }
    }
}

#[test]
fn eliminated_integer_reads_do_not_mark_a_promoted_float_twin_as_explicit() {
    let scalar =
        "10 POKE 2,0\n20 A=PEEK(2)+30000\n30 POKE 1024,A+32767+2770\n40 PRINT PEEK(1024)\n";
    let scalar_dead_twin = scalar.replace("30 POKE", "25 IF 0 THEN PRINT A%\n30 POKE");
    let array_dead_twin = PROMOTED_ARRAY.replace("30 POKE", "25 IF 0 THEN PRINT A%(0)\n30 POKE");
    for profile in PROFILES {
        for (plain, dead_twin) in [
            (scalar, scalar_dead_twin.as_str()),
            (PROMOTED_ARRAY, array_dead_twin.as_str()),
        ] {
            assert_eq!(
                build(plain, profile).prg_bytes,
                build(dead_twin, profile).prg_bytes,
                "{profile:?}: dead source name contaminated the later promotion"
            );
        }
    }
}
