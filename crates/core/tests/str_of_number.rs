mod common;

const SRC: &str = "\
10 DATA 0,1,-1,123456789,-123456789,1E10,1.5E-10,-1.23456789E-38,3.14159265,.001
20 DIM A$(9)
30 FOR I=0 TO 9:READ X:PRINT X:A$(I)=STR$(X)+\"=\":B$=STR$(X)+\"#\":PRINT B$;LEN(STR$(X)):NEXT
40 X=FRE(0):FOR J=1 TO 20:T$=MID$(\"ABCDEFGHIJKLMNOPQRST\",J,12)+\"Q\":NEXT
50 FOR I=0 TO 9:PRINT A$(I):NEXT
";

const BOUND: &str = "\
10 C$=STR$(-1.23456789E-38):D$=STR$(7):E$=STR$(-2.5)
20 IF C$=\"\" THEN C$=C$+\"!\":D$=D$+\"!\":E$=E$+\"!\"
30 FOR J=1 TO 3:T$=MID$(\"ABCDEFGH\",J,4)+\"Z\":NEXT:X=FRE(0)
40 FOR J=1 TO 3:T$=MID$(\"ABCDEFGH\",J,4)+\"Q\":NEXT
50 PRINT C$;\"/\";D$;\"/\";E$
";

#[test]
fn a_string_bound_to_str_survives_the_next_allocation_and_a_collection() {
    let prg = common::compile_source(BOUND).expect("compiles");
    let out = common::run_clean(&prg).expect("reaches a clean exit");
    assert_eq!(out, "-1.23456789E-38/ 7/-2.5\n");
}

#[test]
fn str_matches_what_print_shows() {
    let prg = common::compile_source(SRC).expect("compiles");
    let out = common::run_clean(&prg).expect("reaches a clean exit");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 30, "{out}");
    for i in 0..10 {
        let text = lines[2 * i].strip_suffix(' ').expect("PRINT's trailing space");
        assert_eq!(lines[2 * i + 1], format!("{text}# {} ", text.len()), "{out}");
        assert_eq!(lines[20 + i], format!("{text}="), "the kept copy after a collection:\n{out}");
    }
}
