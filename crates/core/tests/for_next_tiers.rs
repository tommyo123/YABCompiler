mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn asm_of(src: &str) -> String {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, CompileOptions::default())
        .expect("compiles")
        .asm
}

/// `(frames pushed, handlers emitted)`, read off the emitted assembly.
fn frames(asm: &str) -> (usize, usize) {
    (
        asm.lines()
            .filter(|l| l.trim() == "STA __FOR_STACK,X")
            .count(),
        asm.lines()
            .filter(|l| l.starts_with("__FOR_NEXT_HANDLER_"))
            .count(),
    )
}

/// Handler ids stamped by each push, in emission order.
fn push_ids(asm: &str) -> Vec<u8> {
    let lines: Vec<&str> = asm.lines().map(str::trim).collect();
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| **l == "STA __FOR_STACK,X")
        .filter_map(|(i, _)| {
            let prev = lines[..i].iter().rev().find(|l| l.starts_with("LDA #$"))?;
            u8::from_str_radix(prev.trim_start_matches("LDA #$"), 16).ok()
        })
        .collect()
}

/// Every profile and both memory layouts agree on the tier split.
fn assert_tiers(name: &str, src: &str, want: (usize, usize)) {
    let prg = tokenize_program(src).expect("tokenizes");
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let opts = CompileOptions {
                profile,
                extraram,
                force_extraram_off: !extraram,
                ..CompileOptions::default()
            };
            let c = compile_with_options(&prg, opts)
                .unwrap_or_else(|e| panic!("{name} ({profile:?}, {extraram}): {e}"));
            assert_eq!(
                frames(&c.asm),
                want,
                "{name} ({profile:?}, extraram={extraram}): (pushes, handlers)"
            );
        }
    }
}

/// The bubble-sort nest.
#[test]
fn clean_nest_gets_no_frames_at_all() {
    let src = "\
10 dim a(20)
20 for i=1 to 20:a(i)=21-i:next i
30 for i=1 to 19
40 for x=1 to 19-i
50 if a(x)<=a(x+1) then 70
60 t=a(x):a(x)=a(x+1):a(x+1)=t
70 next x
80 next i
90 print a(1);a(20)
";
    assert_tiers("clean_nest", src, (0, 0));
}

#[test]
fn uncrossed_loop_keeps_the_fast_path_beside_a_dispatcher() {
    // The `k` loop is untouched; the `i` loop is entered and left by GOTO and shares a bare
    // `NEXT` with nothing.
    let src = "\
10 goto 30
20 next:goto 50
30 for i=1 to 3
40 print i;:goto 20
50 for k=1 to 3
60 print \"k\";k;
70 next k
80 end
";
    assert_tiers("uncrossed_beside_dispatcher", src, (1, 1));
    assert_eq!(
        common::run_clean(
            &compile_with_options(
                &tokenize_program(src).unwrap(),
                CompileOptions::default()
            )
            .unwrap()
            .prg_bytes
        )
        .as_deref()
        .map(str::trim_end),
        Some(" 1  2  3 K 1 K 2 K 3"),
    );
}

/// Leaving a body in a program that has no dispatching `NEXT` anywhere costs nothing.
#[test]
fn crossing_without_a_dispatcher_is_free() {
    let src = "\
10 for i=1 to 5
20 print i;
30 if i=3 then 60
40 next i
50 print \"no\"
60 print \"out\"
";
    assert_tiers("crossing_no_dispatcher", src, (0, 0));
}

const TIER1: &str = "\
10 goto 50
20 next j:goto 200
30 end
50 for j=1 to 3
60 for w=1 to 4
70 print w;
80 if w=2 then 100
90 next w
100 print \"/\";
110 goto 20
200 print \"done\"
";

#[test]
fn crossed_loop_with_an_unmatchable_dispatcher_gets_a_frame_but_no_handler() {
    assert_tiers("tier1", TIER1, (2, 1));
    // `j` (line 50, pushed first) is the dispatched one and holds handler 1; `w` (line 60)
    // carries the shared tier-1 id.
    assert_eq!(push_ids(&asm_of(TIER1)), vec![1, 2]);
}

#[test]
fn tier1_keeps_the_direct_back_edge() {
    let asm = asm_of(TIER1);
    // The one handler that exists belongs to `j`, and a handler ends by returning through the
    // dispatcher's vector.
    assert_eq!(
        asm.lines()
            .filter(|l| l.trim() == "JMP (__FOR_EXIT_VEC)")
            .count(),
        1,
        "only the dispatched loop returns through the exit vector:\n{asm}"
    );

    assert!(
        asm.lines()
            .filter(|l| l.trim() == "DEC __FOR_STACK_PTR")
            .count()
            >= 2,
        "the tier-1 frame must be popped by its own inline tail:\n{asm}"
    );
}

#[test]
fn tier1_runs_correctly() {
    let prg = tokenize_program(TIER1).unwrap();
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let c = compile_with_options(
            &prg,
            CompileOptions {
                profile,
                force_extraram_off: true,
                ..CompileOptions::default()
            },
        )
        .unwrap();
        let got = common::run_clean(&c.prg_bytes)
            .unwrap_or_else(|| panic!("tier1 ({profile:?}) did not reach a clean exit"));
        assert_eq!(
            got.trim_end(),
            " 1  2 / 1  2 / 1  2 /DONE",
            "tier1 ({profile:?})"
        );
    }
}

#[test]
fn a_reachable_bare_dispatcher_promotes_tier1_to_tier2() {
    let src = TIER1.replace("20 next j:", "20 next:");
    assert_tiers("bare_dispatcher", &src, (2, 2));
}

#[test]
fn the_dispatcher_variable_decides_which_loop_is_dispatched() {
    let src = TIER1.replace("20 next j:", "20 next w:");
    assert_tiers("named_w", &src, (2, 1));
    assert_eq!(
        push_ids(&asm_of(&src)),
        vec![2, 1],
        "`NEXT W` can only match w, so w is the loop that needs a handler"
    );
    assert_eq!(push_ids(&asm_of(TIER1)), vec![1, 2], "control");
}

/// The one place the tier-1 id can be observed at run time: its handler table slot.
#[test]
fn the_tier1_table_slot_is_a_trap_not_a_hole() {
    let asm = asm_of(TIER1);
    let table: Vec<&str> = asm
        .lines()
        .skip_while(|l| !l.starts_with("__FOR_HANDLER_TABLE:"))
        .skip(1)
        .take_while(|l| l.trim_start().starts_with(".word"))
        .flat_map(|l| l.trim().strip_prefix(".word").unwrap().split(','))
        .map(str::trim)
        .collect();
    assert_eq!(
        table,
        vec![
            "$0000",                 // GOSUB marker.
            "__FOR_NEXT_HANDLER_1",   // Dispatched loop j.
            "__NEXT_NO_FOR",          // Framed loop w.
        ],
        "handler table:\n{asm}"
    );
}

// The barrier that makes tier 1 answerable at all.

/// `ON n GOSUB` is a GOSUB and must push the same 0 marker.
#[test]
fn on_gosub_pushes_the_fndfor_barrier() {
    let src = "\
5 goto 30
10 next:goto 50
30 for i=1 to 2
32 print \"b\";:on 1 gosub 10
50 print \"e\";i
99 end
";
    let prg = tokenize_program(src).unwrap();
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let c = compile_with_options(
            &prg,
            CompileOptions {
                profile,
                force_extraram_off: true,
                ..CompileOptions::default()
            },
        )
        .unwrap();
        let r = emu64::run_prg_to_end(&c.prg_bytes, 20_000_000).expect("runs");
        assert!(
            !r.clean_exit,
            "the ROM stops with ?NEXT WITHOUT FOR IN 10 ({profile:?})"
        );
        assert_eq!(
            common::petscii_to_string(&r.output).trim_end(),
            "B",
            "({profile:?}) the marker must hide the caller's I frame from \
             the NEXT at line 10"
        );
    }
}

// The one-line loop.
const ONE_LINE_DELAY: &str = "\
10 for k=1 to 3
20 print \"k\";k;:gosub 100
30 next k
40 print \"done\"
50 end
100 for z=1 to 5:next
110 return
";

#[test]
fn one_line_loop_gosubbed_from_a_body_stays_tier0() {
    assert_tiers("one_line_delay", ONE_LINE_DELAY, (0, 0));
    let asm = asm_of(ONE_LINE_DELAY);
    assert!(
        !asm.contains("JMP (__FOR_DISP_VEC)"),
        "no NEXT here dispatches:\n{asm}"
    );
    assert!(
        !asm.contains("__NEXT_NO_FOR"),
        "nothing here can be ?NEXT WITHOUT FOR:\n{asm}"
    );
}

#[test]
fn one_line_loop_gosubbed_from_a_body_runs() {
    let prg = tokenize_program(ONE_LINE_DELAY).unwrap();
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let c = compile_with_options(
            &prg,
            CompileOptions {
                profile,
                force_extraram_off: true,
                ..CompileOptions::default()
            },
        )
        .unwrap();
        let got = common::run_clean(&c.prg_bytes).unwrap_or_else(|| {
            panic!("({profile:?}) did not reach a clean exit — the delay loop's NEXT errored")
        });
        assert_eq!(got.trim_end(), "K 1 K 2 K 3 DONE", "({profile:?})");
    }
}

#[test]
fn a_header_after_the_next_on_its_line_is_a_real_orphan() {
    let src = ONE_LINE_DELAY.replace("100 for z=1 to 5:next", "100 next:for z=1 to 5");
    let asm = asm_of(&src);
    assert!(
        asm.contains("JMP (__FOR_DISP_VEC)"),
        "the NEXT at 100 has no header ahead of it and must dispatch:\n{asm}"
    );
    let c = compile_with_options(
        &tokenize_program(&src).unwrap(),
        CompileOptions {
            force_extraram_off: true,
            ..CompileOptions::default()
        },
    )
    .unwrap();
    let r = emu64::run_prg_to_end(&c.prg_bytes, 20_000_000).expect("runs");
    assert!(!r.clean_exit, "the ROM stops with ?NEXT WITHOUT FOR IN 100");
    assert_eq!(
        common::petscii_to_string(&r.output).trim_end(),
        "K 1",
        "one iteration printed, then the error"
    );
}
