use yabcompiler_core::Program;
use yabcompiler_core::ast::Statement;
use yabcompiler_core::parse::{self, ParseOptions};
use yabcompiler_core::tokens::Dialect;

fn sunnybrook_shape() -> Vec<u8> {
    const LOAD: u16 = 0x0801;
    let bodies: [(u16, &[u8]); 3] = [
        (130, b"\x99\"A\""),
        (140, b"\x64\x74\x31\x33\x2c\x22\x53\x41\x56\x45\x22"),
        (150, b"\x99\"B\""),
    ];
    let mut addr = LOAD;
    let mut starts = Vec::new();
    for (_, b) in bodies {
        starts.push(addr);
        addr += 5 + b.len() as u16;
    }
    let mut out = LOAD.to_le_bytes().to_vec();
    for (i, (num, body)) in bodies.iter().enumerate() {
        let next = starts.get(i + 1).copied().unwrap_or(addr);
        out.extend_from_slice(&next.to_le_bytes());
        out.extend_from_slice(&num.to_le_bytes());
        out.extend_from_slice(body);
        out.push(0);
    }
    out.extend_from_slice(&[0, 0]);
    out
}

fn extension_statement_name(s: &Statement) -> Option<&'static str> {
    Some(match s {
        Statement::KeySet { .. } => "KeySet",
        Statement::KeyGet { .. } => "KeyGet",
        Statement::Hires { .. } => "Hires",
        Statement::Nrm => "Nrm",
        Statement::Cset { .. } => "Cset",
        Statement::Draw { .. } => "Draw",
        Statement::DrawTo { .. } => "DrawTo",
        Statement::Multi { .. } => "Multi",
        Statement::HiCol => "HiCol",
        _ => return None,
    })
}

fn extension_statements(bytes: &[u8], dialect: Dialect) -> Vec<(u16, &'static str)> {
    let prg = Program::parse(bytes).expect("parses as a .prg");
    let parsed = parse::program_with_options(
        &prg,
        ParseOptions {
            dialect,
            ..Default::default()
        },
    )
    .expect("parses");
    let mut out = Vec::new();
    for l in &parsed.lines {
        for st in &l.statements {
            if let Some(n) = extension_statement_name(st) {
                out.push((l.number, n));
            }
        }
    }
    out
}

#[test]
fn stock_v2_yields_no_extension_statement() {
    let got = extension_statements(&sunnybrook_shape(), Dialect::BasicV2);
    assert!(
        got.is_empty(),
        "a stock BASIC v2 parse produced extension statements: {got:?}"
    );
}

#[test]
fn auto_reads_the_prefix_as_an_extension_statement() {

    let got = extension_statements(&sunnybrook_shape(), Dialect::Auto);
    assert_eq!(got, vec![(140u16, "KeySet")], "auto parse gave {got:?}");
}

#[test]
fn stock_v2_still_parses_the_ordinary_statements() {

    let bytes = sunnybrook_shape();
    let prg = Program::parse(&bytes).expect("parses as a .prg");
    let parsed = parse::program_with_options(
        &prg,
        ParseOptions {
            dialect: Dialect::BasicV2,
            ..Default::default()
        },
    )
    .expect("parses");
    let prints: Vec<u16> = parsed
        .lines
        .iter()
        .filter(|l| l.statements.iter().any(|s| matches!(s, Statement::Print(_))))
        .map(|l| l.number)
        .collect();
    assert_eq!(prints, vec![130, 150]);
}
