use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

fn reserved_of(src: &str) -> Vec<(u16, u16)> {
    let tok = tokenize_program(src).unwrap_or_else(|e| panic!("tokenize: {e:?}"));
    compile_with_options(&tok, CompileOptions::default())
        .unwrap_or_else(|e| panic!("compile: {e}"))
        .diagnostics
        .auto_reserved
}

fn covers(reserved: &[(u16, u16)], lo: u16, hi: u16) -> bool {
    (lo..=hi).all(|a| reserved.iter().any(|(s, e)| a >= *s && a <= *e))
}

fn touched(reserved: &[(u16, u16)], addr: u16) -> bool {
    reserved.iter().any(|(s, e)| addr >= *s && addr <= *e)
}

/// Infer array values written by a bounded loop.
const ROX_FILL: &str = "20 FORX=0TO6:S(X)=192+X:NEXT X\n";

#[test]
fn a_sprite_pointer_named_through_an_array_reserves_the_block_it_names() {
    let r = reserved_of(&format!("{ROX_FILL}30 POKE2041,S(1)\n40 END\n"));
    // `S(1)` is 193 and nothing else.
    assert!(
        covers(&r, 0x3040, 0x307F),
        "S(1) is exactly 193, so block 193 = $3040-$307F, got {r:02X?}"
    );

    assert!(
        !touched(&r, 0x3000) && !touched(&r, 0x31BF),
        "blocks 192 and 198 are assigned to the array but named by no \
         pointer here; a union answer would wrongly cover them, got {r:02X?}"
    );
    assert!(
        !touched(&r, 0x31C0),
        "the blocks stop at $31BF; the extra byte must come from the fill \
         loop, not from the pointer rule, got {r:02X?}"
    );
}

#[test]
fn the_fill_loop_bound_reaches_the_byte_past_the_last_block() {
    let r = reserved_of(&format!(
        "{ROX_FILL}30 FORX=S(0)*64TOS(6)*64+64:POKEX,0:NEXTX\n40 END\n"
    ));
    assert!(
        covers(&r, 0x3000, 0x31C0),
        "the fill runs to S(6)*64+64 = $31C0 inclusive, got {r:02X?}"
    );
}

#[test]
fn the_rox_window_is_a_real_hole_in_the_image() {
    let mut src = String::from(ROX_FILL);
    src.push_str("30 FORX=S(0)*64TOS(6)*64+64:POKEX,0:NEXTX\n");
    src.push_str("40 POKE2041,S(1)\n");
    // Distinct payloads: identical literals get pooled, and a pooled fixture stops short of the window it is supposed to step over.
    for n in 0..1400 {
        src.push_str(&format!("{} PRINT\"{n:04}ABCDEFGHIJKLMNOPQRST\"\n", 100 + n));
    }
    src.push_str("60000 END\n");
    let tok = tokenize_program(&src).unwrap();
    let out = compile_with_options(&tok, CompileOptions::default()).unwrap();
    let load = u16::from_le_bytes([out.prg_bytes[0], out.prg_bytes[1]]);
    let end = load as usize + out.prg_bytes.len() - 2;
    assert!(
        end > 0x31C0,
        "the fixture must reach past the window; ended at ${end:04X}"
    );
    assert!(
        covers(&out.diagnostics.auto_reserved, 0x3000, 0x31C0),
        "the window must be reserved for this test to mean anything, got {:02X?}",
        out.diagnostics.auto_reserved
    );
    let lo = 0x3000usize - load as usize + 2;
    let hi = 0x31C0usize - load as usize + 2;
    let live = out.prg_bytes[lo..=hi].iter().filter(|b| **b != 0).count();
    assert_eq!(
        live, 0,
        "$3000-$31C0 holds {live} live byte(s); the fill will destroy them"
    );
}

#[test]
fn the_union_spans_every_assignment_not_the_nearest_one() {
    let r = reserved_of("10 S(1)=205\n20 POKE2041,S(1)\n30 S(1)=200:PRINTS(1)\n40 END\n");
    assert!(
        covers(&r, 0x3200, 0x337F),
        "the union of blocks 200 and 205 is $3200-$337F, got {r:02X?}"
    );
}

#[test]
fn a_read_into_a_subscript_poisons_an_otherwise_readable_array() {
    let r = reserved_of(&format!(
        "{ROX_FILL}25 FORX=0TO6:READS(X):NEXT X\n\
         26 DATA192,193,194,195,196,197,198\n\
         30 POKE2041,S(1)\n40 END\n"
    ));
    let sprite_area = (0x0900u16..=0x3FFF).filter(|a| touched(&r, *a)).count();
    assert_eq!(
        sprite_area, 0,
        "READ into a subscript must poison the array, got {r:02X?}"
    );
}

#[test]
fn the_same_program_with_an_assignment_instead_of_read_does_reserve() {
    let r = reserved_of(&format!("{ROX_FILL}30 POKE2041,S(1)\n40 END\n"));
    assert!(
        covers(&r, 0x3040, 0x307F),
        "the READ counterweight is only meaningful if this fires, got {r:02X?}"
    );
}

#[test]
fn one_unreadable_assignment_poisons_the_whole_array() {
    let r = reserved_of(&format!(
        "{ROX_FILL}25 S(3)=PEEK(1000)\n30 POKE2041,S(1)\n40 END\n"
    ));
    assert!(
        !touched(&r, 0x3000),
        "S(3)=PEEK(...) makes every element of S unknown, got {r:02X?}"
    );
}

#[test]
fn fetch_into_a_subscript_poisons_the_array() {
    let r = reserved_of(&format!(
        "{ROX_FILL}25 FETCH \"\",8,S(2)\n30 POKE2041,S(1)\n40 END\n"
    ));
    assert!(
        !touched(&r, 0x3000),
        "FETCH into S(2) makes every element of S unknown, got {r:02X?}"
    );
}

#[test]
fn fetch_into_a_scalar_leaves_the_array_alone() {
    let r = reserved_of(&format!(
        "{ROX_FILL}25 FETCH \"\",8,Q\n30 POKE2041,S(1)\n40 END\n"
    ));
    assert!(
        covers(&r, 0x3040, 0x307F),
        "FETCH into a scalar says nothing about S(), got {r:02X?}"
    );
}

#[test]
fn an_array_never_assigned_reserves_nothing() {
    let r = reserved_of("10 POKE2041,S(1)\n20 END\n");
    let sprite_area = (0x0900u16..=0x3FFF).filter(|a| touched(&r, *a)).count();
    assert_eq!(
        sprite_area, 0,
        "an array with no readable assignment reserves nothing, got {r:02X?}"
    );
}

#[test]
fn an_out_of_range_array_union_is_dropped_not_clamped() {
    let r = reserved_of("10 FORX=0TO6:S(X)=4000+X:NEXT X\n20 POKE2041,S(1)\n30 END\n");
    assert!(
        !covers(&r, 0x0900, 0x3FFF),
        "an impossible block interval must not reserve the whole sprite area, got {r:02X?}"
    );
}

#[test]
fn the_cassette_buffer_is_still_out_of_scope_through_an_array() {
    let r = reserved_of("10 FORX=0TO3:S(X)=13+X:NEXT X\n20 POKE2040,S(0)\n30 END\n");
    assert!(
        !touched(&r, 0x0340),
        "blocks 13..16 are $0340-$043F, below the window, got {r:02X?}"
    );
}

#[test]
fn a_moved_vic_bank_still_refuses_through_an_array() {
    let r = reserved_of(&format!(
        "10 POKE56576,2\n{ROX_FILL}30 POKE2041,S(1)\n40 END\n"
    ));
    assert!(
        !touched(&r, 0x3000),
        "bank 2 makes $07F8 not the pointer table; reserve nothing, got {r:02X?}"
    );
}

#[test]
fn a_string_array_of_the_same_name_does_not_poison_the_numeric_one() {
    let r = reserved_of(&format!(
        "10 S$(1)=\"A\"\n{ROX_FILL}30 POKE2041,S(1)\n40 END\n"
    ));
    assert!(
        covers(&r, 0x3040, 0x307F),
        "S$() is a different array from S(), got {r:02X?}"
    );
}

#[test]
fn a_program_with_no_array_reserves_no_sprite_block() {
    let r = reserved_of("10 FORI=0TO9:PRINTI:NEXT\n20 END\n");
    assert!(
        !(0x0900u16..=0x3FFF).any(|a| touched(&r, a)),
        "no array, no new reservation, got {r:02X?}"
    );
}

#[test]
fn the_older_reserve_rules_still_fire_after_the_extra_pass() {
    let lit = reserved_of("10 POKE2040,236\n20 END\n");
    assert!(
        covers(&lit, 0x3B00, 0x3B3F),
        "the literal pointer spelling must survive, got {lit:02X?}"
    );
    let loop_spelling = reserved_of("10 FORI=0TO3:POKE2040+I,250+I:NEXT\n20 END\n");
    assert!(
        covers(&loop_spelling, 0x3E80, 0x3F7F),
        "the loop pointer spelling must survive, got {loop_spelling:02X?}"
    );
    let charset = reserved_of("10 POKE53272,28\n20 END\n");
    assert!(
        covers(&charset, 0x3000, 0x37FF),
        "the $D018 charset window must survive, got {charset:02X?}"
    );
    let screen = reserved_of("10 POKE648,132\n20 END\n");
    assert!(
        covers(&screen, 0x8400, 0x87E7),
        "the $0288 screen window must survive, got {screen:02X?}"
    );
}

#[test]
fn the_second_pass_does_not_inherit_the_first_passs_for_frames() {
    let r = reserved_of("10 POKEQ+16384,0\n20 FORQ=12288TO12799:POKEQ,1\n30 END\n");
    assert!(
        !touched(&r, 0x7000),
        "$7000 can only be reached by reading line 20's counter at line 10, \
         which is a frame left over from the array pre-pass; got {r:02X?}"
    );

    assert!(
        covers(&r, 0x3000, 0x31FF),
        "line 20 must still reserve $3000-$31FF, got {r:02X?}"
    );
}

#[test]
fn the_chained_loader_still_folds_across_three_loops() {
    let r = reserved_of(
        "25 FORI=12800TOI+46:READQ:POKEI,Q:NEXT\n\
         30 FORI=ITOI+38:POKEI,0:NEXT\n\
         35 DATA1,2,3\n40 END\n",
    );
    assert!(
        covers(&r, 0x3200, 0x3255),
        "the two chained loops are $3200-$322E and $322F-$3255, got {r:02X?}"
    );
}

#[test]
fn an_unassigned_element_reads_zero_and_not_a_siblings_value() {
    let r = reserved_of("10 S(0)=200\n20 FORX=S(1)TOS(0)-1STEP300:PRINTX:NEXTX\n30 POKEX*64,0\n40 END\n");
    assert!(
        touched(&r, 0x4B00),
        "S(1) is zero, so the loop exits at 300 and the POKE names \
         $4B00; that is where the program writes, got {r:02X?}"
    );
    assert!(
        !touched(&r, 0x7D00),
        "$7D00 is what S(1)=200 would give — the wrong place, reserved \
         while the real one stayed open, got {r:02X?}"
    );
}

#[test]
fn an_assigned_element_still_answers_its_own_value() {
    let r = reserved_of("10 S(0)=200\n20 S(1)=210\n30 POKE2041,S(1)\n40 END\n");
    assert!(
        covers(&r, 0x3480, 0x34BF),
        "S(1) is 210, block 210 = $3480-$34BF, got {r:02X?}"
    );
    assert!(
        !touched(&r, 0x3200),
        "block 200 belongs to S(0) and no pointer names it, got {r:02X?}"
    );
}

#[test]
fn a_hull_spanning_unrelated_magnitudes_is_refused_not_clamped() {
    let r = reserved_of(
        "10 A(0)=1000\n20 A(1)=60000\n30 FORI=0TO1:POKEA(I),0:NEXT\n40 END\n",
    );
    let window = (0x0900u16..=0xCFFF).filter(|a| touched(&r, *a)).count();
    assert_eq!(
        window, 0,
        "two pokes 59,000 bytes apart describe no block; the hull must \
         be refused, not clamped to the window, got {r:02X?}"
    );
}

#[test]
fn a_narrow_hull_still_reserves() {
    let r = reserved_of("10 A(0)=4096\n20 A(1)=4100\n30 FORI=0TO1:POKEA(I),0:NEXT\n40 END\n");
    assert!(
        touched(&r, 0x1000),
        "a four-byte spread is a block, not a lost variable, got {r:02X?}"
    );
}

#[test]
fn the_answer_does_not_depend_on_assignment_order() {
    let a = reserved_of("10 S(0)=200\n20 POKE2041,T(0)\n30 T(0)=S(0)\n40 S(1)=210\n50 END\n");
    let b = reserved_of("10 S(1)=210\n20 POKE2041,T(0)\n30 T(0)=S(0)\n40 S(0)=200\n50 END\n");
    assert_eq!(a, b, "same program, different text order, different answer");
}

#[test]
fn a_three_deep_chain_settles() {
    let r = reserved_of(
        "10 S(0)=200\n20 T(0)=S(0)\n30 POKE2041,U(0)\n40 U(0)=T(0)\n50 END\n",
    );
    assert!(
        covers(&r, 0x3200, 0x323F),
        "U(0) = T(0) = S(0) = 200, block 200 = $3200-$323F, got {r:02X?}"
    );
}
