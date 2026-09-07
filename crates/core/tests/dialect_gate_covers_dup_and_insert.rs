use yabcompiler_core::{
    CompileOptions, Dialect, Profile, compile_with_options, tokenize_program_with,
};

/// `XQ=5 : A=<name> : PRINT XQ` ; stock v2 prints 5, because `<name>` is an undefined numeric variable.
fn build(name: &str, d: Dialect) -> Option<(usize, usize)> {
    let src = format!("10 XQ=5\n20 A={name}\n30 PRINT XQ\n40 END\n");
    let tok = tokenize_program_with(&src, d).ok()?;
    let o = compile_with_options(
        &tok,
        CompileOptions {
            profile: Profile::Default,
            dialect: d,
            ..CompileOptions::default()
        },
    )
    .ok()?;
    Some((o.prg_bytes.len(), o.diagnostics.syntax_traps.len()))
}

fn reference() -> (usize, usize) {
    build("ZZ", Dialect::BasicV2).expect("the reference program must compile")
}

#[test]
fn dup_and_insert_are_ordinary_variables_under_basic_v2() {
    let want = reference();
    for name in ["DUP", "INSERT"] {
        let got = build(name, Dialect::BasicV2)
            .unwrap_or_else(|| panic!("{name} must compile under --dialect=basic2"));
        assert_eq!(
            got, want,
            "`A={name}` is a read of the undefined variable \
             \"{}\" on a real C64 and must compile like any other \
             variable under --dialect=basic2, with no syntax trap",
            &name[..2]
        );
    }
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
fn the_bare_ascii_string_functions_survive_under_the_extended_dialect() {
    for (name, body) in [
        (
            "DUP",
            vec![0x99, b'D', b'U', b'P', b'(', b'"', b'X', b'"', b',', b'3', b')'],
        ),
        (
            "INSERT",
            vec![
                0x99, b'I', b'N', b'S', b'E', b'R', b'T', b'(', b'"', b'X', b'"', b',', b'"', b'Y',
                b'"', b')',
            ],
        ),
    ] {
        let prg = one_liner(&body);
        let tsb = compile_with_options(
            &prg,
            CompileOptions {
                profile: Profile::Default,
                dialect: Dialect::Tsb,
                ..CompileOptions::default()
            },
        );
        let v2 = compile_with_options(
            &prg,
            CompileOptions {
                profile: Profile::Default,
                dialect: Dialect::BasicV2,
                ..CompileOptions::default()
            },
        );
        let tsb = tsb.unwrap_or_else(|e| {
            panic!(
                "bare-ASCII `{name}(...)` must still be the extension string \
                 function under --dialect=tsb: {e}"
            )
        });
        let v2 = v2.unwrap_or_else(|e| panic!("`{name}` under --dialect=basic2: {e}"));

        assert_ne!(
            tsb.prg_bytes,
            v2.prg_bytes,
            "bare-ASCII `{name}` must mean the extension string function \
             under --dialect=tsb and the variable \"{}\" under \
             --dialect=basic2; identical output means the ASCII spelling is \
             no longer dialect-sensitive",
            &name[..2]
        );
    }
}

#[test]
fn names_that_crunch_to_an_embedded_keyword_still_fail_under_basic_v2() {
    let want = reference();
    for name in ["AUTO", "COLOR", "EXOR", "OPTION", "PAINT"] {
        let got = build(name, Dialect::BasicV2);
        assert_ne!(
            got,
            Some(want),
            "`A={name}` crunches to an embedded BASIC v2 keyword on the real \
             ROM and raises ?SYNTAX ERROR there; it must not compile like an \
             ordinary variable read"
        );
    }
}

#[test]
fn ordinary_names_are_unchanged() {
    let want = reference();
    for name in ["ZZ", "DU", "IN", "DUPE", "INSERTED", "XQ2"] {
        assert_eq!(
            build(name, Dialect::BasicV2),
            Some(want),
            "`A={name}` is an ordinary variable read under --dialect=basic2"
        );
    }
}

#[test]
fn the_tokenized_extension_forms_stay_ungated() {
    // $64 $0C is DUP; build the line by hand so the tokenizer's own dialect handling cannot demote it.
    let mut prg: Vec<u8> = vec![0x01, 0x08];
    let body: Vec<u8> = vec![b'A', b'$', 0xB2, 0x64, 0x0C, b'(', b'"', b'X', b'"', b',', b'3', b')'];
    let next = 0x0801 + 4 + body.len() as u16 + 1;
    prg.extend_from_slice(&next.to_le_bytes());
    prg.extend_from_slice(&10u16.to_le_bytes());
    prg.extend_from_slice(&body);
    prg.push(0x00);
    prg.extend_from_slice(&[0x00, 0x00]);
    for d in [Dialect::Auto, Dialect::BasicV2, Dialect::Tsb] {
        let r = compile_with_options(
            &prg,
            CompileOptions {
                profile: Profile::Default,
                dialect: d,
                ..CompileOptions::default()
            },
        );
        assert!(
            r.is_ok(),
            "the tokenized $64 DUP form must keep parsing under {d:?} — the \
             $64 surface is intentionally not dialect-gated"
        );
    }
}
