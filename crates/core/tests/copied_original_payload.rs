use yabcompiler_core::{CompileOptions, Compiled, Profile, compile_with_options, tokenize_program};

const SOURCE: u16 = 0x1300;
const DEST: u16 = 0xC300;
const PROGRAM: &str = "10 GOSUB2000\n20 SYS49920\n30 PRINT \"123\"\n40 END\n\
2000 FOR I=900 TO 918:READ X:POKE I,X:NEXT\n\
2010 DATA162,1,160,0,177,247,145,249,200,208,249,230,248,230,250,202,208,240,96\n\
2020 POKE247,0:POKE248,19:POKE249,0:POKE250,195:SYS900:RETURN\n";

fn with_payload(source: &str) -> Vec<u8> {
    with_payload_at(source, SOURCE)
}

fn with_payload_at(source: &str, payload_start: u16) -> Vec<u8> {
    let mut prg = tokenize_program(source).unwrap();
    assert!(prg.len() < 2 + usize::from(payload_start - 0x0801));
    prg.resize(2 + usize::from(payload_start - 0x0801), 0x5A);
    // LDA #'A'; JSR CHROUT; RTS, followed by identifiable non-code bytes.
    let mut payload = vec![0xA9, 0x41, 0x20, 0xD2, 0xFF, 0x60];
    payload.extend((6..256).map(|value| value as u8));
    prg.extend(payload);
    prg
}

fn compile(source: &str) -> Compiled {
    compile_with_options(&with_payload(source), CompileOptions::default()).unwrap()
}

#[test]
fn copied_payload_remains_verbatim_and_executes_after_copying() {
    for profile in [Profile::Default, Profile::Size, Profile::Speed] {
        let original = with_payload(PROGRAM);
        let compiled = compile_with_options(
            &original,
            CompileOptions {
                profile,
                ..CompileOptions::default()
            },
        )
        .unwrap();
        let offset = 2 + usize::from(SOURCE - 0x0801);
        assert_eq!(
            &compiled.prg_bytes[offset..offset + 256],
            &original[offset..offset + 256]
        );
        assert!(
            compiled
                .diagnostics
                .effective_reserved
                .iter()
                .any(|&(start, end)| start <= DEST && end >= DEST + 255)
        );
        let run = emu64::run_prg_to_end(&compiled.prg_bytes, 2_000_000).unwrap();
        assert!(run.clean_exit, "{run:?}");
        assert_eq!(run.error_banner(), None);
        assert_eq!(run.output, b"A123\r");
    }
}

#[test]
fn incomplete_or_ambiguous_loader_evidence_does_not_materialise() {
    for source in [
        PROGRAM.replace("TO 918", "TO 917"),
        PROGRAM.replace("POKE I,X", "POKE I,X+1"),
        PROGRAM.replace("SYS900", "SYS901"),
        PROGRAM.replace("230,248,230,250", "230,250,230,248"),
        PROGRAM.replace("200,208,249", "200,208,248"),
        PROGRAM.replace("162,1,160", "162,0,160"),
        PROGRAM.replace("POKE248,19", "POKE248,P"),
        PROGRAM.replace("POKE249,0:", ""),
        PROGRAM.replace("POKE248,19", "POKE248,8"),
        PROGRAM.replace("POKE248,19", "POKE248,20"),
        PROGRAM.replace("POKE250,195", "POKE250,208"),
        PROGRAM.replace("30 PRINT", "25 IF P=1 THEN READ Q\n30 PRINT"),
        PROGRAM.replace("30 PRINT", "25 IF P=1 THEN RESTORE\n30 PRINT"),
        PROGRAM.replace("2010 DATA162", "1990 DATA7\n2010 DATA162"),
    ] {
        let compiled = compile(&source);
        assert_eq!(compiled.diagnostics.original_image, None, "{source}");
    }
}

#[test]
fn a_copy_without_a_loaded_source_payload_is_not_a_trigger() {
    let compiled = compile_with_options(
        &tokenize_program(PROGRAM).unwrap(),
        CompileOptions::default(),
    )
    .unwrap();
    assert_eq!(compiled.diagnostics.original_image, None);
}

#[test]
fn custom_origin_keeps_the_existing_explicit_placement_contract() {
    let compiled = compile_with_options(
        &with_payload(PROGRAM),
        CompileOptions {
            custom_start_address: Some(0x3000),
            ..CompileOptions::default()
        },
    )
    .unwrap();
    assert_eq!(compiled.diagnostics.original_image, None);
}

#[test]
fn large_layout_bridges_the_copy_destination() {
    // The low reservation declines the optional text trim.
    let compiled = compile_with_options(
        &with_payload(PROGRAM),
        CompileOptions {
            reserved_ranges: vec![(0x0900, 0x0900), (0x1500, DEST - 1)],
            extraram: true,
            ..CompileOptions::default()
        },
    )
    .unwrap();
    let run = emu64::run_prg_to_end(&compiled.prg_bytes, 2_000_000).unwrap();
    assert!(run.clean_exit, "{run:?}");
    assert_eq!(run.output, b"A123\r");
    assert!(compiled.diagnostics.end_address > DEST + 255);
}

#[test]
fn live_string_allocations_cannot_overwrite_a_payload_above_short_code() {
    // Each live array element owns a fresh 200-byte concatenation.
    let allocations = format!(
        "1 DIM A$(60)\n2 FOR J=1 TO 60:A$(J)=CHR$(J)+\"{}\":NEXT\n",
        "Q".repeat(199)
    );
    let source = allocations + &PROGRAM.replace("POKE248,19", "POKE248,48");
    let original = with_payload_at(&source, 0x3000);
    for extraram in [false, true] {
        let compiled = compile_with_options(
            &original,
            CompileOptions {
                extraram,
                ..CompileOptions::default()
            },
        )
        .unwrap();
        let run = emu64::run_prg_to_end(&compiled.prg_bytes, 10_000_000).unwrap();
        assert!(
            run.clean_exit,
            "extraram={extraram}, output={:?}, error={:?}",
            run.output,
            run.error_banner()
        );
        assert_eq!(run.error_banner(), None);
        assert_eq!(run.output, b"A123\r");
    }
}
