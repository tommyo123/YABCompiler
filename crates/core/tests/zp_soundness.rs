mod common;

use std::collections::HashMap;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

// helpers

fn opts(profile: Profile) -> CompileOptions {
    CompileOptions {
        profile,
        ..CompileOptions::default()
    }
}

struct Built {
    asm: String,
    prg: Vec<u8>,
}

fn build(src: &str, profile: Profile) -> Built {
    let tok = tokenize_program(src).unwrap_or_else(|e| panic!("tokenize: {e:?}"));
    let c = compile_with_options(&tok, opts(profile)).unwrap_or_else(|e| panic!("compile: {e}"));
    Built {
        asm: c.asm,
        prg: c.prg_bytes,
    }
}

/// The `label = $XX` equates the codegen emits for every zero-page promotion, as a map from
/// label to address.
fn zp_equates(asm: &str) -> HashMap<String, u8> {
    let mut out = HashMap::new();
    for line in asm.lines() {
        let line = line.trim();
        let Some((label, rhs)) = line.split_once(" = $") else {
            continue;
        };
        if label.contains(char::is_whitespace) {
            continue;
        }
        let Ok(addr) = u8::from_str_radix(rhs.trim(), 16) else {
            continue;
        };
        out.insert(label.to_string(), addr);
    }
    out
}

fn run_text(prg: &[u8]) -> String {
    let r = emu64::run_prg_to_end(prg, common::MAX_INSNS).expect("emu64 runs the program");
    common::petscii_to_string(&r.output)
}

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

// Variable writes conflict with live storage.

/// `K` is written by `FOR K=1 TO 2` inside a subroutine and never read (no `NEXT K`), so its
/// live range is empty.
const DEAD_DEF_SHARES_LIVE_SLOT: &str = "\
1 for i=1 to 3
2 gosub 100
3 next i
4 print \"done\";i
10 for a=1 to 2:poke 1024,a:next a
11 for b=1 to 2:poke 1024,b:next b
12 for c=1 to 2:poke 1024,c:next c
13 for d=1 to 2:poke 1024,d:next d
14 for e=1 to 2:poke 1024,e:next e
15 for f=1 to 2:poke 1024,f:next f
16 for g=1 to 2:poke 1024,g:next g
17 for h=1 to 2:poke 1024,h:next h
18 for j=1 to 2:poke 1024,j:next j
19 for l=1 to 2:poke 1024,l:next l
20 for m=1 to 2:poke 1024,m:next m
21 for n=1 to 2:poke 1024,n:next n
22 for o=1 to 2:poke 1024,o:next o
23 for p=1 to 2:poke 1024,p:next p
24 for q=1 to 2:poke 1024,q:next q
25 for r=1 to 2:poke 1024,r:next r
26 for t=1 to 2:poke 1024,t:next t
27 for u=1 to 2:poke 1024,u:next u
90 end
100 for k=1 to 2
110 return
";

#[test]
fn dead_for_counter_never_shares_a_live_variables_zp_slot() {
    for profile in PROFILES {
        let b = build(DEAD_DEF_SHARES_LIVE_SLOT, profile);
        let eq = zp_equates(&b.asm);
        let (Some(&i), Some(&k)) = (eq.get("VI_I"), eq.get("VI_K")) else {
            continue;
        };
        assert_ne!(
            i, k,
            "{profile:?}: K is stored inside the GOSUB while I is live across it; \
             they must not share ${i:02X}"
        );
    }
}

#[test]
fn dead_for_counter_program_terminates_with_rom_answer() {
    for profile in PROFILES {
        let b = build(DEAD_DEF_SHARES_LIVE_SLOT, profile);
        let text = run_text(&b.prg);
        assert!(
            text.contains("DONE 4"),
            "{profile:?}: real BASIC prints DONE 4, got {text:?}"
        );
    }
}

#[test]
fn zp_slot_sharing_still_fires_after_the_def_edges() {
    let b = build(DEAD_DEF_SHARES_LIVE_SLOT, Profile::Default);
    let eq = zp_equates(&b.asm);
    let i = *eq.get("VI_I").expect("I is zero-page promoted");
    for late in ["VI_T", "VI_U"] {
        let a = *eq
            .get(late)
            .unwrap_or_else(|| panic!("{late} is zero-page promoted"));
        assert_eq!(
            a, i,
            "{late} runs after I is dead and must still be able to reuse ${i:02X}"
        );
    }
    let vars: Vec<u8> = eq
        .iter()
        .filter(|(l, _)| l.starts_with("VI_"))
        .map(|(_, a)| *a)
        .collect();
    let distinct: std::collections::HashSet<u8> = vars.iter().copied().collect();
    assert!(
        vars.len() > distinct.len(),
        "expected shared bytes; {} promotions over {} distinct bytes",
        vars.len(),
        distinct.len()
    );
}

// Program accesses must not overlap compiler zero-page storage.

const ROM_ADJUDICATED: &[(&str, &str)] = &[
    // $FC is the high byte of the compiled DATA pointer.
    ("10 poke 252,0\n20 read b:print b\n30 data 22\n", "22"),
    // $FD/$FE are the array-address scratch.
    (
        "10 poke 253,91:poke 254,92\n30 print peek(253);peek(254)\n",
        "91  92",
    ),
    // $03/$04 are the string-compare LHS pointer.
    (
        "10 a$=\"a\":poke 3,91\n20 if a$=a$ then print peek(3)\n",
        "91",
    ),
    // $A7 is in the RS-232 pool region, where the FOR counter lands.
    (
        "20 for i=1 to 5:s=s+i:poke 167,200:next\n30 print s\n",
        "15",
    ),
];

#[test]
fn rom_adjudicated_zero_page_cases() {
    for (src, want) in ROM_ADJUDICATED {
        for profile in PROFILES {
            let b = build(src, profile);
            let text = run_text(&b.prg);
            assert!(
                text.contains(want),
                "{profile:?}: real BASIC prints {want:?}, got {text:?}\nsource:\n{src}"
            );
        }
    }
}

/// Two-hex-digit `$XX` operands in the generated assembly are the compiler's own zero page.
fn compiler_zp_operands(asm: &str) -> std::collections::BTreeSet<u8> {
    let mut out = std::collections::BTreeSet::new();
    for line in asm.lines() {
        let body = line.split(';').next().unwrap_or("");
        let t = body.trim_start();
        let is_insn = body.starts_with(char::is_whitespace)
            && t.len() > 3
            && t.as_bytes()[..3].iter().all(|c| c.is_ascii_uppercase())
            && t[3..].starts_with(char::is_whitespace);
        if !is_insn {
            continue;
        }
        let bytes = body.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$' && !(i > 0 && bytes[i - 1] == b'#') {
                let n = bytes[i + 1..]
                    .iter()
                    .take_while(|c| c.is_ascii_hexdigit())
                    .count();
                if n == 2 {
                    out.insert(u8::from_str_radix(&body[i + 1..i + 3], 16).unwrap());
                }
                i += 1 + n.max(1);
            } else {
                i += 1;
            }
        }
    }
    out
}

#[test]
fn compiler_never_names_a_byte_the_program_pokes() {
    // (source, addresses the program reaches)
    let cases: &[(&str, &[u8])] = &[
        ("10 poke 252,0\n20 read b:print b\n30 data 22\n", &[0xFC]),
        (
            "10 poke 253,91:poke 254,92\n30 print peek(253);peek(254)\n",
            &[0xFD, 0xFE],
        ),
        (
            "10 a$=\"a\":poke 3,91\n20 if a$=a$ then print peek(3)\n",
            &[0x03],
        ),
        (
            "20 for i=1 to 5:s=s+i:poke 167,200:next\n30 print s\n",
            &[0xA7],
        ),
        // Whole pointer pair plus a neighbour: forces two relocations.
        (
            "10 poke 251,1:poke 252,2:poke 253,3:poke 254,4\n20 read b:print b;peek(251)\n30 data 7\n",
            &[0xFB, 0xFC, 0xFD, 0xFE],
        ),
        // DPOKE spans two bytes.
        ("10 dpoke 3,258\n20 print peek(4)\n", &[0x03, 0x04]),
        // Range-analysed, not literal: I is bounded 250..253.
        (
            "10 for i=250 to 253:poke i,0:next\n20 print \"ok\"\n",
            &[0xFA, 0xFB, 0xFC, 0xFD],
        ),
    ];
    for (src, blocked) in cases {
        for profile in PROFILES {
            let b = build(src, profile);
            let used = compiler_zp_operands(&b.asm);
            for a in *blocked {
                assert!(
                    !used.contains(a),
                    "{profile:?}: compiler still names ${a:02X}, which the program touches\n\
                     source:\n{src}used: {used:02X?}"
                );
            }
        }
    }
}

#[test]
fn poke_proved_clear_of_zero_page_costs_nothing() {
    use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};
    let diag = |src: &str| {
        let tok = tokenize_program(src).unwrap();
        compile_with_options(&tok, CompileOptions::default())
            .unwrap()
            .diagnostics
            .zp_pressure
    };
    // Screen RAM through a bounded FOR counter, colour registers, a VIC register, a sprite
    // block, and a PEEK of the raster line.
    let with_poke = "\
10 for x=0 to 20:poke 1024+x,81:next x
20 poke 53280,0:poke 53281,6
25 for y=0 to 62:poke 12288+y,255:next y
28 r=peek(53266)
30 for i=1 to 5:s=s+i:next
40 print s
";
    let no_poke = "10 for i=1 to 5:s=s+i:next\n20 print s\n";
    let a = diag(with_poke);
    let b = diag(no_poke);
    assert_eq!(a.pool_initial, b.pool_initial, "rung 1 must cost no pool");
    assert_eq!(a.pool_lost_to_program, 0);
    assert_eq!(a.pairs_relocated, 0, "no pointer pair should have to move");
    assert_eq!(a.addrs_unprotectable, 0, "every address was bounded");
    assert!(
        a.addrs_proved_clear >= 5,
        "the analysis should have cleared all of them, got {}",
        a.addrs_proved_clear
    );
    let built = build(with_poke, Profile::Default);
    assert_eq!(run_text(&built.prg).trim(), "15");
    let used = compiler_zp_operands(&built.asm);
    assert!(
        used.contains(&0xFB) || used.contains(&0xFD) || used.contains(&0x03),
        "with no zero-page conflict the compiler should still use its default cells"
    );
}

#[test]
fn one_poked_pool_byte_costs_one_pool_byte() {
    use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};
    let base = "10 for i=1 to 5:s=s+i:next\n20 print s\n";
    let poked = "10 for i=1 to 5:s=s+i:poke 167,200:next\n20 print s\n";
    let size = |src: &str| {
        let tok = tokenize_program(src).unwrap();
        let c = compile_with_options(&tok, CompileOptions::default()).unwrap();
        c.diagnostics.zp_pressure.pool_initial
    };
    assert_eq!(
        size(base) - size(poked),
        1,
        "poking $A7 should cost exactly the one byte"
    );
}

/// Every zero-page byte the compiler is willing to claim, poked one at a time.
const ZP_PROBE_TAIL: &str = "\
20 read q
30 a$=\"ab\":b$=\"ab\":c$=a$+b$
40 dim v(4)
50 for i=1 to 4:v(i)=i*2:next
60 for i=1 to 4:n=n+v(i):next
70 if a$=b$ then n=n+100
80 if len(c$)=4 then n=n+1
90 print q;n
100 data 7
";

fn compiler_claimable_zp() -> Vec<u8> {
    let mut v = vec![0x02u8];
    v.extend(0x03..=0x06);
    v.push(0x0A);
    v.extend(0x57..=0x5B);
    v.extend(0xA7..=0xAB);
    v.push(0xAE);
    v.extend(0xF7..=0xFE);
    v
}

#[test]
fn poking_any_compiler_owned_byte_changes_nothing() {
    let baseline = run_text(&build(ZP_PROBE_TAIL, Profile::Default).prg);
    assert_eq!(baseline.trim(), "7  121", "probe oracle: {baseline:?}");
    for b in compiler_claimable_zp() {
        let src = format!("10 poke {b},200\n{ZP_PROBE_TAIL}");
        for profile in PROFILES {
            let built = build(&src, profile);
            let got = run_text(&built.prg);
            assert_eq!(
                got.trim(),
                baseline.trim(),
                "{profile:?}: POKE {b} (${b:02X}) changed the answer — \
                 it landed on compiler zero page"
            );
            let used = compiler_zp_operands(&built.asm);
            assert!(
                !used.contains(&b),
                "{profile:?}: compiler still names ${b:02X} after the program poked it"
            );
        }
    }
}

#[test]
fn peeking_any_compiler_owned_byte_reads_the_programs_value() {
    for b in compiler_claimable_zp() {
        let src = format!("10 poke {b},200\n15 p=peek({b})\n{ZP_PROBE_TAIL}\n95 print p\n");
        let built = build(&src, Profile::Default);
        let got = run_text(&built.prg);
        assert!(
            got.contains("200"),
            "PEEK({b}) (${b:02X}) must read back 200, got {got:?}"
        );
    }
}

#[test]
fn unbounded_poke_address_does_not_drain_the_pool() {
    use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};
    let src = "10 a=peek(56320)\n20 poke a,0\n30 for i=1 to 5:s=s+i:next\n40 print s\n";
    let tok = tokenize_program(src).unwrap();
    let c = compile_with_options(&tok, CompileOptions::default()).unwrap();
    assert!(
        c.diagnostics.zp_pressure.pool_initial >= 10,
        "an unprovable POKE address must not empty the pool, got {}",
        c.diagnostics.zp_pressure.pool_initial
    );
    assert!(
        c.diagnostics.zp_pressure.addrs_unprotectable > 0,
        "and it must be reported"
    );
}

#[test]
fn sequential_for_counters_still_reach_zero_page() {
    let src = "\
10 for i=1 to 3:s=s+i:next i
20 for j=1 to 3:s=s+j:next j
30 print s
";
    let b = build(src, Profile::Default);
    assert_eq!(run_text(&b.prg).trim(), "12");
    let eq = zp_equates(&b.asm);
    for v in ["VI_I", "VI_J", "VI_S"] {
        assert!(
            eq.contains_key(v),
            "{v} should be zero-page promoted: {eq:?}"
        );
    }
}
