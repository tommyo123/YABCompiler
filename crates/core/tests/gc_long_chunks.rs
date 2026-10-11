mod common;

const SRC: &str = "\
10 B$=\"\"
20 FOR K=1 TO 255:B$=B$+CHR$(65+K-INT(K/26)*26):NEXT
30 DIM A$(5)
40 FOR R=1 TO 3
50 FOR I=0 TO 5:T$=B$+\"\":A$(I)=MID$(B$,R,250+I):NEXT
60 X=FRE(0):FOR J=1 TO 3:T$=MID$(B$,J,120)+MID$(B$,J+1,120):NEXT
70 FOR I=0 TO 5:PRINT LEN(A$(I));ASC(A$(I));ASC(RIGHT$(A$(I),1));:NEXT:PRINT
80 NEXT
";

fn expected() -> String {
    let ch = |k: usize| 65 + k % 26;
    let mut out = String::new();
    for r in 1..=3usize {
        for i in 0..6usize {
            let len = (250 + i).min(256 - r);
            out.push_str(&format!(" {len}  {}  {} ", ch(r), ch(r + len - 1)));
        }
        out.push('\n');
    }
    out
}

#[test]
fn strings_up_to_255_characters_survive_collection() {
    let prg = common::compile_source(SRC).expect("compiles");
    let out = common::run_clean(&prg).expect("reaches a clean exit");
    assert_eq!(out, expected());
}
