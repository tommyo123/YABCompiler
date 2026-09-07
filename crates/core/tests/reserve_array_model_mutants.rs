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

#[test]
fn two_writes_to_one_element_union_in_ascending_order_too() {
    let r = reserved_of("10 S(0)=200\n20 S(0)=210\n30 POKE2041,S(0)\n40 END\n");
    assert!(
        covers(&r, 0x3200, 0x34BF),
        "S(0) is 200 and 210, so the union is blocks 200..210 = $3200-$34BF; \
         a missing widening limb answers block 200 alone, got {r:02X?}"
    );
}

#[test]
fn one_element_two_writes_gives_the_same_answer_in_both_orders() {
    let up = reserved_of("10 S(0)=200\n20 S(0)=210\n30 POKE2041,S(0)\n40 END\n");
    let down = reserved_of("10 S(0)=210\n20 S(0)=200\n30 POKE2041,S(0)\n40 END\n");
    assert_eq!(
        up, down,
        "the same two assignments to one element, in the two text orders, \
         gave different reservations"
    );
}

#[test]
fn a_counter_sweep_wider_than_the_hull_cap_is_still_reserved() {
    let r = reserved_of("10 FORF=1024TO33685:IFPEEK(F)=81THENPOKEF,32\n20 NEXT\n30 END\n");
    assert!(
        covers(&r, 0x0900, 0x8395),
        "a 31,894-byte counter sweep is a range the program walks, not a \
         hull; the array cap must not reach it, got {r:02X?}"
    );
}

#[test]
fn an_array_hull_just_under_the_cap_still_reserves() {
    let r = reserved_of("10 A(0)=4096\n20 A(1)=14000\n30 FORI=0TO1:POKEA(I),0:NEXT\n40 END\n");
    assert!(
        covers(&r, 0x1000, 0x36B0),
        "a 9,905-byte hull is under the 12 KB cap and must reserve, got {r:02X?}"
    );
}

#[test]
fn an_array_hull_just_over_the_cap_is_refused() {
    let r = reserved_of("10 A(0)=1000\n20 A(1)=15000\n30 FORI=0TO1:POKEA(I),0:NEXT\n40 END\n");
    let window = (0x0900u16..=0xCFFF).filter(|a| touched(&r, *a)).count();
    assert_eq!(
        window, 0,
        "a 12,697-byte hull is over the 12 KB cap and must be refused, not \
         clamped, got {r:02X?}"
    );
}

#[test]
fn a_spread_write_leaves_the_zero_fill_in_the_answer() {
    let r = reserved_of("10 A=PEEK(2)\n20 S(A)=200\n30 POKE2041,S(0)\n40 END\n");
    assert!(
        !touched(&r, 0x3200),
        "S(A) may not have landed on S(0), which then reads its zero fill, so \
         S(0) is [0,200] and names no one block; a precise block-200 answer \
         is a false one, got {r:02X?}"
    );
}

#[test]
fn an_unreadable_unpinned_write_retracts_the_elements_already_pinned() {
    let r = reserved_of("10 A=PEEK(2)\n20 S(0)=200\n30 S(A)=PEEK(3)\n40 POKE2041,S(0)\n50 END\n");
    assert!(
        !touched(&r, 0x3200),
        "S(A)=PEEK(3) may have landed on S(0) and may have put anything there, \
         so the pinned 200 is no longer what S(0) holds, got {r:02X?}"
    );
}

#[test]
fn the_same_array_without_the_unreadable_write_still_reserves() {
    let r = reserved_of("10 A=PEEK(2)\n20 S(0)=200\n30 POKE2041,S(0)\n40 END\n");
    assert!(
        covers(&r, 0x3200, 0x323F),
        "nothing retracts S(0), so block 200 = $3200-$323F, got {r:02X?}"
    );
}

#[test]
fn one_unevaluable_element_does_not_retract_the_elements_that_are_known() {
    let r = reserved_of(
        "20 FORX=0TO6:S(X)=192+X:NEXT X\n25 S(3)=PEEK(1000)\n30 POKE2041,S(1)\n40 END\n",
    );
    assert!(
        covers(&r, 0x3040, 0x307F),
        "S(3) is unevaluable and S(1) is exactly 193; an unknown element is a \
         statement about S(3), not about S, so the pointer still names block \
         193 = $3040-$307F, got {r:02X?}"
    );
}

#[test]
fn the_unevaluable_element_itself_is_still_refused() {
    let r = reserved_of(
        "20 FORX=0TO6:S(X)=192+X:NEXT X\n25 S(3)=PEEK(1000)\n30 POKE2041,S(3)\n40 END\n",
    );
    let window = (0x0900u16..=0xCFFF).filter(|a| touched(&r, *a)).count();
    assert_eq!(
        window, 0,
        "S(3) is assigned from a PEEK this analysis cannot read, so it names \
         no block; answering block 195 off the fill loop would be reading a \
         value the program overwrote, got {r:02X?}"
    );
}
