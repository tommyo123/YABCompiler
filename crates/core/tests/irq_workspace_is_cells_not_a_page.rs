//! The TSB IRQ wedge owns four cells, not the whole `$C5xx` page.

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

/// The wedge is present iff its installer is both defined and called.
fn wedged(src: &str, profile: Profile) -> bool {
    let asm = asm_of(src, profile);
    let defined = asm.contains("__TSB_IRQ_INSTALL:");
    let called = asm.contains("JSR __TSB_IRQ_INSTALL");
    assert_eq!(
        defined, called,
        "__TSB_IRQ_INSTALL must be defined exactly when it is called\n{src}"
    );
    defined
}

#[test]
fn poking_the_ahoy_parameter_block_installs_no_interrupt() {
    for p in PROFILES {
        assert!(
            !wedged("10 POKE50432,1\n20 END\n", p),
            "POKE $C500 must not install the IRQ wedge ({p:?})"
        );
    }
}

#[test]
fn the_whole_parameter_block_installs_no_interrupt() {
    let mut src = String::from("10 REM AHOY SPRITE PARAMETERS\n");
    for (i, a) in (50432u32..=50446).enumerate() {
        src.push_str(&format!("{} POKE{},{}\n", 20 + i * 10, a, i));
    }
    src.push_str("500 END\n");
    for p in PROFILES {
        assert!(
            !wedged(&src, p),
            "$C500-$C50E is an Ahoy! parameter block, not IRQ workspace ({p:?})"
        );
    }
}

/// The rule keys on the cell, so its immediate neighbours are inert.
#[test]
fn the_cells_neighbours_install_no_interrupt() {
    for addr in [0xC515u32, 0xC518, 0xC5C6, 0xC5C8, 0xC5D9, 0xC5DB, 0xC5FF] {
        let src = format!("10 POKE{addr},0\n20 END\n");
        assert!(
            !wedged(&src, Profile::Default),
            "${addr:04X} is not IRQ workspace and must not install the wedge"
        );
    }
}

#[test]
fn peeking_each_workspace_cell_still_installs_the_interrupt() {
    for addr in [0xC516u32, 0xC517, 0xC5C7, 0xC5DA] {
        for p in PROFILES {
            let src = format!("10 POKE{addr},0\n20 IFPEEK({addr})<30THEN20\n30 END\n");
            assert!(
                wedged(&src, p),
                "PEEK(${addr:04X}) polls the IRQ workspace and must keep the wedge ({p:?})"
            );
        }
    }
}

#[test]
fn poking_each_workspace_cell_still_installs_the_interrupt() {
    for addr in [0xC516u32, 0xC517, 0xC5C7, 0xC5DA] {
        let src = format!("10 POKE{addr},0\n20 END\n");
        assert!(
            wedged(&src, Profile::Default),
            "POKE ${addr:04X},0 must keep the wedge"
        );
    }
}

#[test]
fn waiting_on_the_flash_sync_byte_still_installs_the_interrupt() {
    let src = "10 WAIT50631,128,128\n20 END\n";
    assert!(
        wedged(src, Profile::Default),
        "WAIT $C5C7,$80,$80 must keep the wedge"
    );
}

#[test]
fn a_workspace_cell_inside_an_expression_still_installs_the_interrupt() {
    let src = "10 T=PEEK(50454)+1\n20 IFT<60THEN10\n30 END\n";
    assert!(
        wedged(src, Profile::Default),
        "PEEK($C516) in a LET right-hand side must keep the wedge"
    );
}

#[test]
fn flash_still_installs_the_interrupt_with_no_c5xx_reference() {
    let src = "10 FLASH1,1\n20 END\n";
    let asm = asm_of(src, Profile::Default);
    assert!(
        asm.contains("JSR __TSB_IRQ_INSTALL"),
        "FLASH is IRQ-driven and must still install the wedge"
    );
    assert!(
        asm.contains("__TSB_IRQ_HANDLER:") && asm.contains("INC $C516"),
        "the handler must still exist and still maintain the jiffy counter"
    );
}

#[test]
fn an_ordinary_program_installs_no_interrupt() {
    for p in PROFILES {
        assert!(
            !wedged("10 PRINT\"HI\"\n20 END\n", p),
            "a plain program must not install the wedge ({p:?})"
        );
    }
}

/// A fractional address is truncated before use, so the spelling of the literal does not decide the rule.
#[test]
fn a_fractional_workspace_address_is_truncated_not_rejected() {
    assert!(
        wedged(
            "10 T=PEEK(50454.7)\n20 IFT<30THEN10\n30 END\n",
            Profile::Default
        ),
        "PEEK(50454.7) reads $C516 and must keep the wedge"
    );

    assert!(
        !wedged(
            "10 T=PEEK(50432.7)\n20 IFT<30THEN10\n30 END\n",
            Profile::Default
        ),
        "PEEK(50432.7) reads $C500 and must not install the wedge"
    );
}
