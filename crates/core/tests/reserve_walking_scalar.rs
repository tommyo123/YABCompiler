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
/// How many bytes of the auto-reserve window this program claims.
fn in_window(reserved: &[(u16, u16)]) -> usize {
    (0x0900u16..=0xCFFF).filter(|a| touched(reserved, *a)).count()
}

const CHAR_WINDOW_FILL: &str =
    "144 R=12288:FORG=1TO6:FORI=1TO63:READA:DC=DC+A:POKER,A:R=R+1:NEXT:R=R+1:NEXT\n";

#[test]
fn the_walking_scalar_fill_is_reserved_end_to_end() {
    let mut src = String::from("140 POKE2040,192:POKE2041,196:POKE2042,197\n");
    src.push_str(CHAR_WINDOW_FILL);
    for n in 0..400 {
        src.push_str(&format!("{} DATA{},{},{}\n", 200 + n, n % 256, 1, 2));
    }
    src.push_str("60000 END\n");
    let r = reserved_of(&src);
    // $3000-$317E is what the six passes write; $317F is the byte the sixth pass skips and is inside the sprite-pointer reservation anyway.
    assert!(
        covers(&r, 0x3000, 0x317E),
        "the fill walks $3000-$317E through a scalar; without this analysis \
         only the blocks the sprite pointers name are reserved, leaving \
         $3080-$30FF open, got {r:02X?}"
    );
}

#[test]
fn character_window_is_reserved_in_the_image() {
    let mut src = String::from("140 POKE2040,192:POKE2041,196:POKE2042,197\n");
    src.push_str(CHAR_WINDOW_FILL);
    // Distinct payloads: identical literals are pooled, and a pooled fixture stops short of the window it is supposed to step over.
    for n in 0..1400 {
        src.push_str(&format!("{} PRINT\"{n:04}ABCDEFGHIJKLMNOPQRST\"\n", 1000 + n));
    }
    src.push_str("60000 END\n");
    let tok = tokenize_program(&src).unwrap();
    let out = compile_with_options(&tok, CompileOptions::default()).unwrap();
    let load = u16::from_le_bytes([out.prg_bytes[0], out.prg_bytes[1]]) as usize;
    let end = load + out.prg_bytes.len() - 2;
    assert!(
        end > 0x317E,
        "the fixture must reach past the window; it ended at ${end:04X}"
    );
    let lo = 0x3000 - load + 2;
    let hi = 0x317E - load + 2;
    let live = out.prg_bytes[lo..=hi].iter().filter(|b| **b != 0).count();
    assert_eq!(
        live, 0,
        "$3000-$317E holds {live} live byte(s); line 144 writes every one of \
         them and the program's own PRINT strings are what it destroys"
    );
}

#[test]
fn a_single_loop_constant_stride_walk_is_bounded() {
    let r = reserved_of("10 R=12288\n20 FORI=1TO100:R=R+1:POKER,1:NEXT\n30 END\n");
    assert!(
        covers(&r, 0x3001, 0x3064),
        "R=12288 then 100 passes of R=R+1 before the POKE walks $3001-$3064, \
         got {r:02X?}"
    );
}

/// A scalar the loop body sets from somewhere else is not walking anything this analysis can read, and the header says nothing about where it goes.
#[test]
fn a_rebased_scalar_inside_the_loop_is_refused() {
    let r = reserved_of("10 R=12288\n20 FORI=1TO100:R=R+1:POKER,1:R=PEEK(1):NEXT\n30 END\n");
    assert!(
        !touched(&r, 0x3064),
        "R is re-read from PEEK every pass, so the walk is not bounded by the \
         header and $3001-$3064 is not a run; got {r:02X?}"
    );
}

#[test]
fn a_non_constant_stride_is_refused() {
    let r = reserved_of("10 R=12288\n20 FORI=1TO100:R=R*2:POKER,1:NEXT\n30 END\n");
    assert!(
        !touched(&r, 0x3064),
        "R doubles every pass; a trip-count bound of $3001-$3064 would be a \
         fiction, got {r:02X?}"
    );
}

/// The base has to be known where the walk starts.
#[test]
fn a_base_the_walk_cannot_read_is_refused() {
    let r = reserved_of("10 INPUTR\n20 FORI=1TO100:R=R+1:POKER,1:NEXT\n30 END\n");
    assert_eq!(
        in_window(&r),
        0,
        "R comes off the keyboard, so there is no base and no run, got {r:02X?}"
    );
}

/// A FOR with unknown bounds provides no limit on the write range.
#[test]
fn a_loop_with_an_unreadable_bound_is_refused() {
    let r = reserved_of("10 R=12288:N=PEEK(2)\n20 FORI=1TON:R=R+1:POKER,1:NEXT\n30 END\n");
    assert!(
        !touched(&r, 0x3064),
        "the header's limit is a PEEK, so there is no trip count and no run, \
         got {r:02X?}"
    );
}

#[test]
fn a_scalar_walking_screen_memory_is_refused_not_clamped_into_the_window() {
    let r = reserved_of("85 S=1799\n89 FORX=1TO16\n90 POKES,81\n91 S=S+40:NEXT\n99 END\n");
    assert_eq!(
        in_window(&r),
        0,
        "the walk is $0707-$0A2F, which starts below the auto-reserve window; \
         clamping it up manufactures a range in our window out of a program \
         that writes to the screen, got {r:02X?}"
    );
}

#[test]
fn a_walk_that_changes_direction_is_not_bounded_by_the_net_stride() {
    let r = reserved_of("10 R=12288\n20 FORI=1TO4:R=R+100:POKER,1:R=R-50:NEXT\n30 END\n");
    assert!(
        !covers(&r, 0x3000, 0x30C8) || covers(&r, 0x3000, 0x30FA),
        "a run was claimed down to $3000 but stops before $30FA, which the \
         fourth pass writes; a net-stride sum is not a bound, got {r:02X?}"
    );
}

#[test]
fn a_counter_the_body_assigns_does_not_supply_a_trip_count() {
    let r = reserved_of("10 R=12288\n20 FORI=1TO10:R=R+1:POKER,1:I=I-.5:NEXT\n30 END\n");
    assert!(
        !covers(&r, 0x3001, 0x300A) || covers(&r, 0x3001, 0x3014),
        "a run was claimed to $300A, but the body halves the counter's step so \
         the loop runs twenty times and R reaches $3014, got {r:02X?}"
    );
}

#[test]
fn the_outermost_clean_frame_is_the_one_that_bounds_the_walk() {
    let r = reserved_of(
        "10 R=12288\n20 FORG=1TO6:FORI=1TO63:POKER,1:R=R+1:NEXT:R=R+1:NEXT\n30 END\n",
    );
    assert!(
        covers(&r, 0x3000, 0x317E),
        "6 x 63 inner steps plus 6 outer ones is a 384-byte run from $3000; an \
         inner-frame bound stops at $303E, got {r:02X?}"
    );
}

#[test]
fn a_descending_walk_is_bounded_downward() {
    let r = reserved_of("10 R=12500\n20 FORI=1TO10:POKER,1:R=R-8:NEXT\n30 END\n");
    assert!(
        covers(&r, 0x308C, 0x30D4),
        "ten passes of R=R-8 from 12500 walk $308C-$30D4, got {r:02X?}"
    );
}

#[test]
fn the_run_is_taken_through_the_whole_address_expression() {
    let r = reserved_of("10 R=12288\n20 FORI=1TO100:POKER+8,1:R=R+1:NEXT\n30 END\n");
    assert!(
        covers(&r, 0x3008, 0x306B),
        "the walk is R in [12288,12387] and the address is R+8, so the run is \
         $3008-$306B, got {r:02X?}"
    );
}

#[test]
fn the_increment_after_the_last_write_is_not_part_of_the_run() {
    let r = reserved_of("10 R=12288\n20 FORI=1TO10:POKER,1:R=R+64:NEXT\n30 END\n");
    assert!(
        covers(&r, 0x3000, 0x3240),
        "ten passes write $3000,$3040,…,$3240, got {r:02X?}"
    );
    assert!(
        !touched(&r, 0x3280),
        "R ends at $3280 but the last write is at $3240; the trailing \
         increment is not part of the run, got {r:02X?}"
    );
}

#[test]
fn a_conditional_increment_does_not_trim_the_run() {
    let r = reserved_of("10 R=12288\n20 FORI=1TO4:POKER,1:IFI<9THENR=R+64\n25 NEXT\n30 END\n");
    assert!(
        covers(&r, 0x3000, 0x3100),
        "the only increment after the write is guarded, so it may not be \
         subtracted and the run is the full 4 x 64 from $3000, got {r:02X?}"
    );
}

#[test]
fn a_run_just_under_the_span_cap_is_reserved() {
    let r = reserved_of("10 R=12288\n20 FORI=1TO8000:POKER,I:R=R+1:NEXT\n30 END\n");
    assert!(
        covers(&r, 0x3000, 0x4F3F),
        "an 8,000-byte run is under the 8 KB cap and must be reserved, \
         got {r:02X?}"
    );
}

#[test]
fn a_run_just_over_the_span_cap_is_refused() {
    let r = reserved_of("10 R=12288\n20 FORI=1TO8200:POKER,I:R=R+1:NEXT\n30 END\n");
    assert!(
        !touched(&r, 0x4F3F),
        "an 8,200-byte run is over the 8 KB cap; refusing it leaves the \
         program as it was, clamping it would not, got {r:02X?}"
    );
}

#[test]
fn the_counter_sweep_next_door_is_untouched() {
    let r = reserved_of("10 FORF=1024TO33685:IFPEEK(F)=81THENPOKEF,32\n20 NEXT\n30 END\n");
    assert!(
        covers(&r, 0x0900, 0x8395),
        "a 31,894-byte counter sweep is a range the program walks and must \
         still be reserved, got {r:02X?}"
    );
}
