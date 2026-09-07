//! `POKE` reproduces the read half of BASIC's own store.

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

fn asm_of(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default())
        .unwrap_or_else(|e| panic!("compiles: {e}\n{src}"))
        .asm
}

fn insns(asm: &str) -> Vec<String> {
    asm.lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with(';')
                && !l.starts_with('*')
                && !l.starts_with('.')
                && !l.ends_with(':')
                && !l.contains('=')
        })
        .map(|l| l.split(';').next().unwrap().trim().to_string())
        .collect()
}

fn has_discard_read_before_store(asm: &str, hex: &str) -> bool {
    let bit = format!("BIT ${hex}");
    let sta = format!("STA ${hex}");
    let v = insns(asm);
    v.windows(2).any(|w| w[0] == bit && w[1] == sta)
}

fn count(asm: &str, needle: &str) -> usize {
    insns(asm).iter().filter(|l| l.as_str() == needle).count()
}

#[test]
fn poke_to_sprite_sprite_collision_latch_reads_first() {
    // `POKE 53278,0` ; the idiom for "clear the collision latch".
    let asm = asm_of("10 POKE 53278,0\n");
    assert!(
        has_discard_read_before_store(&asm, "D01E"),
        "BIT $D01E immediately before STA $D01E:\n{asm}"
    );
}

#[test]
fn poke_to_sprite_background_collision_latch_reads_first() {
    let asm = asm_of("10 POKE 53279,0\n");
    assert!(
        has_discard_read_before_store(&asm, "D01F"),
        "BIT $D01F immediately before STA $D01F:\n{asm}"
    );
}

#[test]
fn poke_to_cia1_icr_reads_first() {

    let asm = asm_of("10 POKE 56333,127\n");
    assert!(
        has_discard_read_before_store(&asm, "DC0D"),
        "BIT $DC0D immediately before STA $DC0D:\n{asm}"
    );
}

#[test]
fn poke_to_cia2_icr_reads_first() {
    let asm = asm_of("10 POKE 56589,0\n");
    assert!(
        has_discard_read_before_store(&asm, "DD0D"),
        "BIT $DD0D immediately before STA $DD0D:\n{asm}"
    );
}

#[test]
fn poke_to_tod_registers_reads_first() {
    // $DC0B latches the TOD for reading, $DC08 releases it.
    let hours = asm_of("10 POKE 56331,18\n");
    assert!(
        has_discard_read_before_store(&hours, "DC0B"),
        "BIT $DC0B before the store:\n{hours}"
    );
    let tenths = asm_of("10 POKE 56328,0\n");
    assert!(
        has_discard_read_before_store(&tenths, "DC08"),
        "BIT $DC08 before the store:\n{tenths}"
    );
}

#[test]
fn the_incomplete_address_decode_mirrors_are_covered() {

    let vic = asm_of("10 POKE 53342,0\n");
    assert!(
        has_discard_read_before_store(&vic, "D05E"),
        "VIC mirror $D05E treated as the latch:\n{vic}"
    );
    // The CIAs repeat their 16 registers every $10 across the page, so $DC1D is CIA1's ICR. 56349 = $DC1D.
    let cia = asm_of("10 POKE 56349,127\n");
    assert!(
        has_discard_read_before_store(&cia, "DC1D"),
        "CIA1 mirror $DC1D treated as the ICR:\n{cia}"
    );
}

#[test]
fn the_address_may_come_from_the_programs_own_variables() {

    let asm = asm_of("10 V=53248\n20 POKE V+30,0\n");
    assert!(
        has_discard_read_before_store(&asm, "D01E"),
        "folded V+30 still gets the read:\n{asm}"
    );
}

#[test]
fn the_runtime_value_paths_get_the_read_too() {

    for src in [
        "10 A%=0\n20 POKE 53278,A%\n",
        "10 A%=1:B%=2\n20 POKE 53278,A%+B%\n",
        "10 A=1.5\n20 POKE 53278,A*2\n",
    ] {
        let asm = asm_of(src);
        assert!(
            has_discard_read_before_store(&asm, "D01E"),
            "read present for:\n{src}\n{asm}"
        );
    }
}

#[test]
fn read_modify_write_on_a_latch_reads_twice_like_the_interpreter() {
    // `POKE A,PEEK(A) AND m` is lowered to LDA/AND/STA.
    let asm = asm_of("10 POKE 53278,PEEK(53278)AND1\n");
    let v = insns(&asm);
    let reads = count(&asm, "LDA $D01E") + count(&asm, "BIT $D01E");
    assert_eq!(reads, 2, "two bus reads of $D01E:\n{asm}");
    assert!(
        has_discard_read_before_store(&asm, "D01E"),
        "the second read is the store's, so it sits just before it:\n{asm}"
    );

    let lda = v.iter().position(|l| l == "LDA $D01E").expect("PEEK read");
    let and = v.iter().position(|l| l == "AND #$01").expect("mask");
    assert!(lda < and, "PEEK read precedes the mask:\n{asm}");
}

#[test]
fn poking_ram_gets_no_read() {
    let asm = asm_of("10 POKE 1024,32\n");
    assert_eq!(count(&asm, "BIT $0400"), 0, "no read for RAM:\n{asm}");
    assert_eq!(count(&asm, "STA $0400"), 1, "the store is still there");
}

#[test]
fn the_neighbouring_io_registers_get_no_read() {

    for (addr, hex) in [
        (53280u32, "D020"), // border colour
        (53281, "D021"),    // background colour
        (53273, "D019"),    // VIC IRQ latch - write-to-acknowledge
        (53269, "D015"),    // sprite enable
        (56332, "DC0C"),
        (56334, "DC0E"),
        (56576, "DD00"),    // CIA2 port A (the VIC bank select)
    ] {
        let asm = asm_of(&format!("10 POKE {addr},0\n"));
        assert_eq!(
            count(&asm, &format!("BIT ${hex}")),
            0,
            "${hex} must not acquire a discard read:\n{asm}"
        );
        assert_eq!(
            count(&asm, &format!("STA ${hex}")),
            1,
            "${hex} is still stored to:\n{asm}"
        );
    }
}

#[test]
fn a_peek_of_a_latch_is_still_a_single_read() {

    let asm = asm_of("10 IF PEEK(53278)=9 THEN 100\n100 END\n");
    assert_eq!(
        count(&asm, "LDA $D01E") + count(&asm, "BIT $D01E"),
        1,
        "one read per source PEEK:\n{asm}"
    );
}

#[test]
fn the_indirect_store_path_is_left_alone() {

    let asm = asm_of("10 FOR I=1024 TO 1030:POKE I,32:NEXT\n");
    let v = insns(&asm);
    let indirect = v.iter().any(|l| l.starts_with("STA (") && l.ends_with("),Y"));
    let fill = v.iter().any(|l| l.contains("__POKE_FILL"));
    assert!(
        indirect || fill,
        "the loop lowers to an indirect store or the fill helper:\n{asm}"
    );
    assert_eq!(
        v.iter().filter(|l| l.starts_with("BIT $")).count(),
        0,
        "no discard read added to the indirect path:\n{asm}"
    );
}

#[test]
fn nothing_is_added_to_a_program_that_pokes_nothing_sensitive() {

    let asm = asm_of(
        "10 POKE 53280,0:POKE 53281,0\n\
         20 FOR I=0 TO 39:POKE 1024+I,81:POKE 55296+I,1:NEXT\n\
         30 POKE 646,7:POKE 198,0\n",
    );
    assert_eq!(
        insns(&asm).iter().filter(|l| l.starts_with("BIT $")).count(),
        0,
        "an ordinary program is untouched:\n{asm}"
    );
}
