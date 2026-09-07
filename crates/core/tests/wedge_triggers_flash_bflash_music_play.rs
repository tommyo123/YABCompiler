use yabcompiler_core::{
    CompileOptions, Dialect, Profile, compile_with_options, tokenize_program_with,
};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn asm_of(prg: &[u8], d: Dialect, p: Profile) -> Option<String> {
    compile_with_options(
        prg,
        CompileOptions {
            profile: p,
            dialect: d,
            ..CompileOptions::default()
        },
    )
    .ok()
    .map(|o| o.asm)
}

/// The wedge is present iff its installer is both defined and called.
fn wedged_asm(asm: &str) -> bool {
    let defined = asm.contains("__TSB_IRQ_INSTALL:");
    let called = asm.contains("JSR __TSB_IRQ_INSTALL");
    assert_eq!(
        defined, called,
        "__TSB_IRQ_INSTALL must be defined exactly when it is called"
    );
    defined
}

fn wedged_src(src: &str, d: Dialect, p: Profile) -> bool {
    let tok = tokenize_program_with(src, d).unwrap_or_else(|e| panic!("tokenize {src:?}: {e:?}"));
    let asm = asm_of(&tok, d, p).unwrap_or_else(|| panic!("compile {src:?} under {d:?}"));
    wedged_asm(&asm)
}

fn one_liner(body: &[u8]) -> Vec<u8> {
    let mut prg: Vec<u8> = vec![0x01, 0x08];
    let next = 0x0801u16 + 4 + body.len() as u16 + 1;
    prg.extend_from_slice(&next.to_le_bytes());
    prg.extend_from_slice(&10u16.to_le_bytes());
    prg.extend_from_slice(body);
    prg.push(0x00);
    prg.extend_from_slice(&[0x00, 0x00]);
    prg
}

#[test]
fn each_trigger_installs_the_wedge_when_genuinely_used() {
    for src in [
        "10 FLASH1,1\n20 END\n",
        "10 BFLASH2,0,6\n20 END\n",
        "10 MUSIC4,\"CDE\"\n20 END\n",
        "10 PLAY1\n20 END\n",
    ] {
        for p in PROFILES {
            assert!(
                wedged_src(src, Dialect::Auto, p),
                "{src:?} is IRQ-driven and must install the wedge ({p:?})"
            );
        }
    }
}

#[test]
fn the_installed_handler_still_maintains_the_workspace() {
    let tok = tokenize_program_with("10 FLASH1,1\n20 END\n", Dialect::Auto).unwrap();
    let asm = asm_of(&tok, Dialect::Auto, Profile::Default).unwrap();
    assert!(asm.contains("__TSB_IRQ_HANDLER:"));
    assert!(
        asm.contains("INC $C516"),
        "the handler must still bump the jiffy counter"
    );

    for site in ["$0314", "$0315", "$DC0D", "$DD0D", "$D01A", "$D012"] {
        assert!(
            asm.contains(site),
            "the installer is documented as touching {site}; if that changed, \
             this test's header comment is now wrong"
        );
    }
}

#[test]
fn an_ordinary_program_installs_no_interrupt() {
    for p in PROFILES {
        assert!(
            !wedged_src("10 PRINT\"HI\"\n20 END\n", Dialect::Auto, p),
            "a plain program must not install the wedge ({p:?})"
        );
    }
}

/// In stock BASIC v2 these words are ordinary variables ; `FLASH` is `FL`, `BFLASH` is `BF`, `MUSIC` is `MU`, `PLAY` is `PL`.
#[test]
fn the_words_used_as_stock_v2_variables_install_no_interrupt() {
    for (name, two) in [
        ("FLASH", "FL"),
        ("BFLASH", "BF"),
        ("MUSIC", "MU"),
        ("PLAY", "PL"),
    ] {
        let src = format!("10 {name}=1\n20 PRINT {name}\n30 PRINT\"{two}\"\n40 END\n");
        for d in [Dialect::Auto, Dialect::BasicV2] {
            for p in PROFILES {
                assert!(
                    !wedged_src(&src, d, p),
                    "`{name}` as a stock v2 variable must not install the wedge \
                     ({d:?}, {p:?})"
                );
            }
        }
    }
}

#[test]
fn bare_ascii_spellings_in_a_tokenized_prg_install_no_interrupt() {
    for name in ["FLASH", "BFLASH", "MUSIC", "PLAY"] {

        let mut body: Vec<u8> = name.bytes().collect();
        body.push(0xB2);
        body.push(b'1');
        let prg = one_liner(&body);
        for d in [Dialect::Auto, Dialect::BasicV2] {
            let Some(asm) = asm_of(&prg, d, Profile::Default) else {
                continue;
            };
            assert!(
                !wedged_asm(&asm),
                "bare-ASCII `{name}=1` in a tokenized .prg is a stock v2 \
                 assignment and must not install the wedge ({d:?})"
            );
        }
    }
}

#[test]
fn the_words_as_text_install_no_interrupt() {
    let src = "10 PRINT\"FLASH BFLASH MUSIC PLAY\"\n\
               20 DATA FLASH,BFLASH,MUSIC,PLAY\n\
               30 REM FLASH BFLASH MUSIC PLAY\n\
               40 END\n";
    for d in [Dialect::Auto, Dialect::BasicV2] {
        for p in PROFILES {
            assert!(
                !wedged_src(src, d, p),
                "the words as string/DATA/REM text must not install the wedge \
                 ({d:?}, {p:?})"
            );
        }
    }
}

#[test]
fn the_bare_ascii_surface_is_dialect_gated() {

    for (name, body) in [
        (
            "FLASH",
            vec![b'F', b'L', b'A', b'S', b'H', b' ', b'1', b',', b'1'],
        ),
        (
            "BFLASH",
            vec![b'B', b'F', b'L', b'A', b'S', b'H', b' ', b'2', b',', b'0', b',', b'6'],
        ),
        ("PLAY", vec![b'P', b'L', b'A', b'Y', b' ', b'1']),
    ] {
        let prg = one_liner(&body);
        if let Some(asm) = asm_of(&prg, Dialect::BasicV2, Profile::Default) {
            assert!(
                !wedged_asm(&asm),
                "with the extended dialect off, bare-ASCII `{name}` is not a \
                 statement and must install no wedge"
            );
        }
        let asm = asm_of(&prg, Dialect::Tsb, Profile::Default)
            .unwrap_or_else(|| panic!("`{name}` must compile under --dialect=tsb"));
        assert!(
            wedged_asm(&asm),
            "with the extended dialect on, bare-ASCII `{name}` IS the statement \
             and must install the wedge — otherwise the gate above is passing \
             by deleting the feature"
        );
    }
}

#[test]
fn the_tokenized_surface_is_gated_by_basic2_and_only_by_basic2() {

    for (name, id, args) in [
        ("FLASH", 0x1Fu8, vec![b'1', b',', b'1']),
        ("BFLASH", 0x1C, vec![b'2', b',', b'0', b',', b'6']),
        ("PLAY", 0x21, vec![b'1']),
    ] {
        let mut body = vec![0x64u8, id];
        body.extend_from_slice(&args);
        let prg = one_liner(&body);

        let gated = asm_of(&prg, Dialect::BasicV2, Profile::Default)
            .unwrap_or_else(|| panic!("tokenized {name} must still compile under BasicV2"));
        assert!(
            !wedged_asm(&gated),
            "--dialect=basic2 must not install the IRQ wedge from a tokenized $64 {name}"
        );
        assert!(
            gated.contains("LDX #$0B") && gated.contains("JMP $A437"),
            "--dialect=basic2 must raise v2's own ?SYNTAX ERROR on a $64 {name}\n{gated}"
        );

        for d in [Dialect::Auto, Dialect::Tsb] {
            let asm = asm_of(&prg, d, Profile::Default)
                .unwrap_or_else(|| panic!("tokenized {name} must compile under {d:?}"));
            assert!(
                wedged_asm(&asm),
                "the tokenized $64 {name} form must still install the wedge under {d:?}"
            );
        }
    }
}
