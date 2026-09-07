mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(src: &str, profile: Profile) -> yabcompiler_core::Compiled {
    let prg = tokenize_program(src).unwrap_or_else(|e| panic!("tokenize: {e:?}"));
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compile: {e}"))
}

fn run(prg: &[u8]) -> (bool, String) {
    let r = emu64::run_prg_to_end(prg, common::MAX_INSNS).expect("emu64 run");
    (r.clean_exit, common::petscii_to_string(&r.output))
}

fn reuse_sites(asm: &str) -> usize {
    asm.lines().filter(|l| l.contains("; reuse ARRAY_ADDR")).count()
}

fn is_curlin_stamp(line: &str) -> bool {
    let l = line.trim();
    l.starts_with("LDA #$") || l.starts_with("STA $39") || l.starts_with("INC $39")
}

fn lines_after_every_loop_top(asm: &str) -> Vec<(String, String)> {
    let all: Vec<&str> = asm.lines().collect();
    let mut out = Vec::new();
    for (i, l) in all.iter().enumerate() {
        let t = l.trim_end();
        if !(t.ends_with(':')
            && (t.starts_with("__FOR_TOP_")
                || t.starts_with("__REPEAT_TOP_")
                || t.starts_with("__LOOP_TOP_")))
        {
            continue;
        }
        if let Some(next) = all[i + 1..].iter().find(|n| !is_curlin_stamp(n)) {
            out.push((t.to_string(), next.trim().to_string()));
        }
    }
    out
}

const SPUK_GRAPH: &str = "10 DIMK(9,2)\n\
                          20 FORL=0TO9:FORI=0TO2:K(L,I)=64:NEXTI:NEXTL\n\
                          25 S=0\n\
                          30 FORL=0TO9\n\
                          50 FORI=0TO2\n\
                          60 IFK(L,I)<>64THEN110\n\
                          70 S=S+3:IFS>9-LTHENS=0\n\
                          72 B(I)=L+S\n\
                          80 FORJ=0TO2\n\
                          90 IFK(B(I),J)=64THEN105\n\
                          100 NEXTJ\n\
                          102 GOTO110\n\
                          105 K(B(I),J)=L:K(L,I)=B(I)\n\
                          110 NEXTI\n\
                          120 NEXTL\n\
                          130 PRINTK(0,0);K(3,1);K(9,2)\n";

#[test]
fn a_loop_body_cannot_reuse_an_address_computed_before_the_loop() {
    for p in PROFILES {
        let b = build(SPUK_GRAPH, p);
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: clean exit");
        assert_eq!(
            out.trim_end(),
            " 3  3  9",
            "{p:?}: B(I) inside the J loop must be re-addressed on every \
             iteration; ` 3  3  6` is the stale $FD/$FE reading the K \
             element the previous iteration examined"
        );
    }
}

#[test]
fn no_loop_top_opens_with_a_reuse() {
    for p in PROFILES {
        let b = build(SPUK_GRAPH, p);
        let tops = lines_after_every_loop_top(&b.asm);
        assert!(
            tops.len() >= 5,
            "{p:?}: the shape has five loops; only {} loop tops were found, \
             so this test is not looking at what it claims to",
            tops.len()
        );
        for (label, first) in &tops {
            assert!(
                !first.contains("; reuse ARRAY_ADDR"),
                "{p:?}: {label} is a back-edge target and cannot inherit the \
                 cache from its fall-in predecessor; found `{first}`"
            );
        }
    }
}

const GOTO_TARGET: &str = "10 DIMA(20),B(3)\n\
                           20 FORI=0TO20:A(I)=I+1:NEXTI\n\
                           30 B(1)=3:N=0\n\
                           40 X=A(B(1))\n\
                           50 N=N+1:IFN<3THEN40\n\
                           70 PRINTX\n";

#[test]
fn a_branch_target_line_does_not_open_with_a_reuse() {
    for p in PROFILES {
        let b = build(GOTO_TARGET, p);

        let mut lines = b.asm.lines().skip_while(|l| l.trim_end() != "L40:");
        assert!(lines.next().is_some(), "{p:?}: no L40: label in the asm");
        let first = lines
            .find(|n| !is_curlin_stamp(n))
            .map(str::trim)
            .unwrap_or("");
        assert!(
            !first.contains("; reuse ARRAY_ADDR"),
            "{p:?}: line 40 is a branch target; its predecessor is line 50, \
             not line 30. found: {first}"
        );
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: clean exit");
        assert_eq!(out.trim_end(), " 4", "{p:?}: A(B(1)) is A(3) is 4");
    }
}

#[test]
fn straight_line_reuse_still_fires() {
    let src = "10 DIMA(20),B(3)\n\
               20 FORI=0TO20:A(I)=I+1:NEXTI\n\
               30 B(1)=3\n\
               40 PRINTA(B(1));A(B(1))\n";
    for p in PROFILES {
        let b = build(src, p);
        assert!(
            reuse_sites(&b.asm) > 0,
            "{p:?}: two reads of A(B(1)) in one statement must still share \
             one address computation"
        );
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: clean exit");
        assert_eq!(out.trim_end(), " 4  4", "{p:?}");
    }
}

/// Reuse inside one iteration is untouched ; the invalidation is at the loop top, not everywhere in the body.
#[test]
fn reuse_within_one_iteration_still_fires() {
    let src = "10 DIMA(20)\n\
               20 FORI=0TO20:A(I)=I+1:NEXTI\n\
               40 FORJ=0TO2\n\
               50 PRINTA(2);A(2)\n\
               60 NEXTJ\n";
    for p in PROFILES {
        let b = build(src, p);
        assert!(
            reuse_sites(&b.asm) > 0,
            "{p:?}: the second A(2) in the body is still the same address"
        );
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: clean exit");
        assert_eq!(
            out.lines().map(str::trim_end).collect::<Vec<_>>(),
            [" 3  3", " 3  3", " 3  3"],
            "{p:?}"
        );
    }
}

#[test]
fn an_ordinary_summing_loop_is_unaffected() {
    let src = "10 DIMA(20):T=0\n\
               20 FORI=0TO20:A(I)=I+1:NEXTI\n\
               30 FORI=0TO20:T=T+A(I):NEXTI\n\
               40 PRINTT\n";
    for p in PROFILES {
        let b = build(src, p);
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: clean exit");
        assert_eq!(out.trim_end(), " 231", "{p:?}");
    }
}

#[test]
fn a_program_without_arrays_emits_no_extra_bytes() {
    let src = "10 T=0\n\
               20 FORI=1TO10:T=T+I:NEXTI\n\
               30 PRINTT\n";
    for p in PROFILES {
        let b = build(src, p);
        assert_eq!(reuse_sites(&b.asm), 0, "{p:?}: no arrays, no reuse sites");
        let (clean, out) = run(&b.prg_bytes);
        assert!(clean, "{p:?}: clean exit");
        assert_eq!(out.trim_end(), " 55", "{p:?}");
    }
}

#[test]
fn the_full_size_retry_shape_terminates_and_agrees() {
    let src = "10 DIMK(49,2)\n\
               20 FORL=0TO49:FORI=0TO2:K(L,I)=64:NEXTI:NEXTL\n\
               25 S=0\n\
               30 FORL=0TO49\n\
               50 FORI=0TO2\n\
               60 IFK(L,I)<>64THEN110\n\
               70 S=S+7:IFS>49-LTHENS=0\n\
               72 B(I)=L+S\n\
               80 FORJ=0TO2\n\
               90 IFK(B(I),J)=64THEN105\n\
               100 NEXTJ\n\
               102 GOTO70\n\
               105 K(B(I),J)=L:K(L,I)=B(I)\n\
               110 NEXTI\n\
               120 NEXTL\n\
               130 PRINTK(0,0);K(49,2)\n";
    for p in PROFILES {
        let b = build(src, p);
        let (clean, out) = run(&b.prg_bytes);
        assert!(
            clean,
            "{p:?}: array updates must terminate"
        );
        assert_eq!(out.trim_end(), " 7  42", "{p:?}");
    }
}
