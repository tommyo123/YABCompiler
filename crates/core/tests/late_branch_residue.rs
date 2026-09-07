//! The branch passes run once more after the factoring family, and this is the test that keeps
//! them there.

mod common;

use yabcompiler_core::{CompileOptions, compile_with_options, tokenize_program};

/// A code line: an instruction (`mnem`, optional operand) or a label.
enum Line {
    Insn(String, Option<String>),
    Label(String),
}

/// Strip comments and blanks, and classify what is left.
fn code_lines(asm: &str) -> Vec<Line> {
    let mut out = Vec::new();
    for raw in asm.lines() {
        let l = match raw.find(" ;") {
            Some(i) => raw[..i].trim(),
            None => raw.trim(),
        };
        if l.is_empty() || l.starts_with(';') || l.starts_with('.') || l.starts_with("*=") {
            continue;
        }
        if l.contains('=') && !l.contains(' ') {
            continue;
        }
        if let Some(name) = l.strip_suffix(':')
            && !name.contains(char::is_whitespace)
        {
            out.push(Line::Label(name.to_string()));
            continue;
        }
        match l.split_once(char::is_whitespace) {
            Some((m, op)) => out.push(Line::Insn(m.to_string(), Some(op.trim().to_string()))),
            None => out.push(Line::Insn(l.to_string(), None)),
        }
    }
    out
}

fn is_cond_branch(m: &str) -> bool {
    matches!(
        m,
        "BEQ" | "BNE" | "BCC" | "BCS" | "BMI" | "BPL" | "BVC" | "BVS"
    )
}

fn min_insn_bytes(mnem: &str, operand: Option<&str>) -> usize {
    match mnem {
        "BEQ" | "BNE" | "BCC" | "BCS" | "BMI" | "BPL" | "BVC" | "BVS" => 2,
        "JSR" | "JMP" => 3,
        _ => match operand {
            None => 1,
            Some(_) => 2,
        },
    }
}

fn min_distance(lines: &[Line], from: usize, target: &str) -> Option<usize> {
    let mut bytes = 0usize;
    for l in &lines[from..] {
        match l {
            Line::Label(n) if n == target => return Some(bytes),
            Line::Insn(m, op) => bytes += min_insn_bytes(m, op.as_deref()),
            _ => {}
        }
    }
    bytes = 0;
    for l in lines[..from].iter().rev() {
        match l {
            Line::Label(n) if n == target => return Some(bytes),
            Line::Insn(m, op) => bytes += min_insn_bytes(m, op.as_deref()),
            _ => {}
        }
    }
    None
}

const DEFINITELY_IN_REACH: usize = 90;

struct Scan {
    helpers: usize,
    tail_jsr_rts: Vec<String>,
    foldable_branch: Vec<String>,
    unreachable_branch: usize,
}

fn scan(name: &str, asm: &str, acc: &mut Scan) {
    let lines = code_lines(asm);
    for l in &lines {
        if let Line::Label(n) = l
            && (n.starts_with("__SEQ_") || n.starts_with("__JSR2_") || n.starts_with("__TAIL_"))
        {
            acc.helpers += 1;
        }
    }
    for i in 0..lines.len() {
        if let Line::Insn(m, Some(op)) = &lines[i]
            && m == "JSR"
            && op.starts_with("__")
            && let Some(Line::Insn(next, None)) = lines.get(i + 1)
            && next == "RTS"
        {
            acc.tail_jsr_rts.push(format!("{name}: JSR {op} / RTS"));
        }
        let (Line::Insn(m, Some(bt)), Some(Line::Insn(j, Some(jt))), Some(Line::Label(lab))) =
            (&lines[i], lines.get(i + 1), lines.get(i + 2))
        else {
            continue;
        };
        if !is_cond_branch(m) || j != "JMP" || lab != bt {
            continue;
        }
        if jt.starts_with('$') {
            continue;
        }
        match min_distance(&lines, i, jt) {
            Some(d) if d <= DEFINITELY_IN_REACH => {
                acc.foldable_branch
                    .push(format!("{name}: {m} {bt} / JMP {jt} (~{d} bytes away)"));
            }
            _ => acc.unreachable_branch += 1,
        }
    }
}

fn scan_corpus() -> (Scan, usize) {
    let mut acc = Scan {
        helpers: 0,
        tail_jsr_rts: Vec::new(),
        foldable_branch: Vec::new(),
        unreachable_branch: 0,
    };
    let mut checked = 0usize;
    for dir in ["test_corpus", "test_synthetic"] {
        let dir = common::repo_path(dir);
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .expect("read corpus dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "bas"))
            .collect();
        paths.sort();
        for path in &paths {
            let name = path.file_stem().unwrap().to_string_lossy().to_string();
            let tokenized = if let Ok(bytes) = std::fs::read(path.with_extension("prg")) {
                bytes
            } else {
                let src = std::fs::read_to_string(path).expect("read .bas");
                match tokenize_program(&src) {
                    Ok(b) => b,
                    Err(_) => continue,
                }
            };
            let Ok(c) = compile_with_options(&tokenized, CompileOptions::default()) else {
                continue;
            };
            checked += 1;
            scan(&name, &c.asm, &mut acc);
        }
    }
    (acc, checked)
}

#[test]
fn the_factoring_family_still_fires_across_the_corpus() {
    let (acc, checked) = scan_corpus();
    assert!(checked > 200, "expected the whole corpus, saw {checked}");
    assert!(
        acc.helpers >= 20,
        "the residue tests are vacuous without factored helpers; \
         found {} across {checked} programs",
        acc.helpers
    );
}

#[test]
fn no_tail_jsr_rts_residue_across_the_corpus() {
    let (acc, checked) = scan_corpus();
    assert!(
        acc.tail_jsr_rts.is_empty(),
        "{} tail JSR/RTS pairs left unconverted across {checked} programs \
         (1 byte + 12 cycles each): {:?}",
        acc.tail_jsr_rts.len(),
        &acc.tail_jsr_rts[..acc.tail_jsr_rts.len().min(20)]
    );
}

#[test]
fn no_reachable_branch_around_jmp_residue_across_the_corpus() {
    let (acc, checked) = scan_corpus();
    assert!(
        acc.foldable_branch.is_empty(),
        "{} in-reach branch-around-JMP shapes left unfolded across \
         {checked} programs (3 bytes each): {:?}",
        acc.foldable_branch.len(),
        &acc.foldable_branch[..acc.foldable_branch.len().min(20)]
    );
}
