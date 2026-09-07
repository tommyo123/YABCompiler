//! A self-modifying-code template must survive the peephole, even when the value it loads is dead.

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn asm_of(src: &str, profile: Profile) -> String {
    let prg = tokenize_program(src).expect("tokenize");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .expect("compiles")
    .asm
}

/// Every `STA <label>+<n>` in the image must name an instruction long enough to contain byte `n`.
fn smc_patch_targets_are_wide_enough(asm: &str) -> Result<(), String> {
    let lines: Vec<&str> = asm.lines().collect();
    // label -> index of the instruction it names
    let mut named: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut pending: Vec<&str> = Vec::new();
    for (i, raw) in lines.iter().enumerate() {
        let code = raw.split(';').next().unwrap_or("");
        let t = code.trim();
        if t.is_empty() {
            continue;
        }
        if !code.starts_with(char::is_whitespace) {
            match t.strip_suffix(':') {
                Some(n) => pending.push(n),
                None => pending.clear(),
            }
            continue;
        }
        for n in pending.drain(..) {
            named.insert(n, i);
        }
    }
    for raw in &lines {
        let t = raw.split(';').next().unwrap_or("").trim();
        let Some(op) = t
            .strip_prefix("STA ")
            .or_else(|| t.strip_prefix("STX "))
            .or_else(|| t.strip_prefix("STY "))
        else {
            continue;
        };
        let Some((base, off)) = op.split_once('+') else {
            continue;
        };
        let Ok(off) = off.trim().parse::<usize>() else {
            continue;
        };
        let Some(&at) = named.get(base.trim()) else {
            continue;
        };
        let target = lines[at].split(';').next().unwrap_or("").trim();
        // Immediate and implied forms are at most two bytes, so a `+2` into either is off the end.
        let two_byte = target.contains("#$") || target.split_whitespace().count() == 1;
        if off >= 2 && two_byte {
            return Err(format!(
                "`{}` patches +{off} of `{target}`, which is not three bytes",
                base.trim()
            ));
        }
    }
    Ok(())
}

fn dead_peek_src() -> &'static str {
    "10 P = 49152 + 7\n\
     20 FOR I = 1 TO 2\n\
     30 A = PEEK(P + I)\n\
     40 A = 1\n\
     50 NEXT\n\
     60 PRINT A\n"
}

#[test]
fn a_dead_smc_peek_keeps_its_three_byte_template() {
    for profile in PROFILES {
        let asm = asm_of(dead_peek_src(), profile);
        if let Err(e) = smc_patch_targets_are_wide_enough(&asm) {
            panic!("{profile:?}: {e}\n{asm}");
        }
    }
}

#[test]
fn every_smc_label_still_names_a_three_byte_absolute_instruction() {

    let srcs = [
        "10 DIM A(30)\n20 FOR I=0 TO 30\n30 A(I)=PEEK(4096+I*3)\n40 NEXT\n",
        "10 FOR I=0 TO 9\n20 POKE 4096+I*7, I\n30 NEXT\n",
        "10 P=828\n20 FOR I=0 TO 4\n30 X=PEEK(P+I*5)\n40 X=0\n50 NEXT\n60 PRINT X\n",
    ];
    for src in srcs {
        for profile in PROFILES {
            let asm = asm_of(src, profile);
            if let Err(e) = smc_patch_targets_are_wide_enough(&asm) {
                panic!("{profile:?} on {src:?}: {e}\n{asm}");
            }
        }
    }
}

#[test]
fn the_dead_load_rule_still_fires_where_no_smc_label_is_involved() {

    let prg = tokenize_program("10 A=PEEK(53266):A=PEEK(53266):PRINT A\n").expect("tokenize");
    let with = compile_with_options(&prg, CompileOptions::default()).expect("compiles");
    let loads = with.asm.lines().filter(|l| l.trim() == "LDA $D012").count();
    assert!(
        loads <= 1,
        "the shadowed I/O load should still be dropped, found {loads}:\n{}",
        with.asm
    );
}
