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
/// How many of `lo..=hi` are reserved.
fn claimed(reserved: &[(u16, u16)], lo: u16, hi: u16) -> usize {
    (lo..=hi).filter(|a| touched(reserved, *a)).count()
}

#[test]
fn a_guarded_walk_that_changes_direction_is_not_bounded_by_the_net_stride() {
    let r = reserved_of("10 R=12288\n20 FORI=1TO4:R=R+100:POKER,1:IFI>0THENR=R-50\n25 NEXT\n30 END\n");
    assert!(
        !covers(&r, 0x3000, 0x30C8) || covers(&r, 0x3000, 0x30FA),
        "a run was claimed down to $3000 but stops before $30FA, which the \
         fourth pass writes; a signed sum of strides of both signs is not a \
         bound, got {r:02X?}"
    );
}

#[test]
fn a_guarded_rebase_inside_the_loop_is_refused() {
    let r = reserved_of("10 R=12288\n20 FORI=1TO100:R=R+1:POKER,1:IFI>0THENR=PEEK(1)\n25 NEXT\n30 END\n");
    assert!(
        !touched(&r, 0x3064),
        "R is re-read from PEEK every pass, so the header's 100 trips say \
         nothing about where the POKE goes and $3000-$3064 is not a run; \
         got {r:02X?}"
    );
}

#[test]
fn a_guarded_constant_stride_still_bounds_the_walk() {
    let r = reserved_of("10 R=12288\n20 FORI=1TO10:POKER,1:IFI>0THENR=R+64\n25 NEXT\n30 END\n");
    assert!(
        covers(&r, 0x3000, 0x3240),
        "ten guarded passes of R=R+64 from $3000 write $3000,$3040,...,$3240; \
         the guard is not a reason to refuse, only a reason not to trim, \
         got {r:02X?}"
    );
}

#[test]
fn a_dpoke_walk_reserves_the_high_byte_of_the_last_write() {
    let r = reserved_of("10 R=12288\n20 FORI=1TO10:DPOKER,1:R=R+2:NEXT\n30 END\n");
    assert!(
        covers(&r, 0x3000, 0x3013),
        "the last DPOKE names $3012 and writes $3013 as well; a walk that \
         reserves only the low byte leaves the high half of the last element \
         of the fill open, got {r:02X?}"
    );
}

#[test]
fn an_unclosed_frame_over_counts_rather_than_under_counts() {
    let closed =
        reserved_of("10 R=12288\n20 FORG=1TO6:FORI=1TO63:POKER,1:R=R+1:NEXT:NEXT\n30 END\n");
    let open = reserved_of("10 R=12288\n20 FORG=1TO6:FORI=1TO63:POKER,1:R=R+1:NEXT\n30 END\n");
    let hi_closed = closed.iter().map(|(_, e)| *e).max().unwrap_or(0);
    let hi_open = open.iter().map(|(_, e)| *e).max().unwrap_or(0);
    assert!(
        hi_open >= hi_closed,
        "dropping the outer NEXT shortened the reserved run from ${hi_closed:04X} \
         to ${hi_open:04X}; an unclosed frame is scanned to the end of the \
         program precisely so that it over-counts rather than under-counts"
    );
}

#[test]
fn two_walking_scalars_in_one_address_are_bounded_together_or_not_at_all() {
    let r = reserved_of("10 R=12288:Q=0\n20 FORI=1TO10:POKER+Q,1:R=R+1:Q=Q+16:NEXT\n30 END\n");
    let n = claimed(&r, 0x3000, 0x3099);
    assert!(
        n <= 1 || covers(&r, 0x3000, 0x3099),
        "{n} of the 154 bytes $3000-$3099 are reserved; the loop advances both \
         R and Q, so it writes up to $3099, and a run claimed for one scalar \
         with the other frozen at its first-pass value stops at $3090, \
         got {r:02X?}"
    );
}

#[test]
fn a_second_variable_the_loop_does_not_advance_is_not_a_second_walker() {
    let r = reserved_of("10 R=12288:Q=8\n20 FORI=1TO100:POKER+Q,1:R=R+1:NEXT\n30 END\n");
    assert!(
        covers(&r, 0x3008, 0x306B),
        "Q is constant through the loop, so the walk is R in [12288,12387] \
         offset by 8 and the run is $3008-$306B, got {r:02X?}"
    );
}

#[test]
fn a_net_negative_mixed_walk_does_not_claim_a_run_below_everything_it_writes() {
    let r = reserved_of(
        "10 R=12288\n20 FORI=1TO10:R=R+300:POKER,1:IFI>0THENR=R-400\n25 NEXT\n30 END\n",
    );
    assert!(
        !touched(&r, 0x2C18) || covers(&r, 0x2DA8, 0x312C),
        "a run was claimed from $2C18 while the program writes $2DA8-$312C; a \
         signed sum of strides of both signs put the whole reservation below \
         the fill and left the fill itself open, got {r:02X?}"
    );
}
