//! `RND(X)` where the program never assigns `X` IS `RND(0)`, because RUN clears the variable table and an unassigned numeric variable reads as zero.

use yabcompiler_core::{parse, prg, tokenize_program};

fn ast_debug(src: &str) -> String {
    let bytes = tokenize_program(src).expect("tokenize");
    let program = prg::Program::parse(&bytes).expect("prg parse");
    let ast = parse::program(&program).expect("basic parse");
    format!("{:?}", ast.lines)
}

fn count(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

/// `Expr::Var` rendered by `Debug` for a float variable with this base name.
fn var(base: &str) -> String {
    format!("Var(VarName {{ base: {base:?}, kind: Float }})")
}

/// `RND(<bare float variable>)` rendered by `Debug`.
fn rnd_of_var(base: &str) -> String {
    format!("Func1(Rnd, {})", var(base))
}

#[test]
fn long_names_fold_to_their_first_two_characters() {

    let d = ast_debug("10 ZEIT=5:PRINT ZE\n");
    assert_eq!(
        count(&d, "base: \"ZE\""),
        2,
        "ZEIT and ZE must be the same variable, both folded to ZE: {d}"
    );
    assert_eq!(count(&d, "base: \"ZEIT\""), 0, "no unfolded name may survive: {d}");

    let d = ast_debug("10 XYZ=1:PRINT XYQ\n");
    assert_eq!(
        count(&d, "base: \"XY\""),
        2,
        "XYZ and XYQ are both XY in BASIC v2: {d}"
    );
}

#[test]
fn two_character_names_that_differ_stay_different() {

    let d = ast_debug("10 X=1:X1=2:PRINT X;X1\n");
    assert_eq!(count(&d, "base: \"X\""), 2, "X appears twice, alone: {d}");
    assert_eq!(count(&d, "base: \"X1\""), 2, "X1 appears twice, alone: {d}");
}

#[test]
fn the_type_suffix_selects_a_different_variable() {

    let d = ast_debug("10 X=1:X%=2:X$=\"A\"\n");
    assert!(d.contains("base: \"X\", kind: Float"), "{d}");
    assert!(d.contains("base: \"X\", kind: Integer"), "{d}");
    assert!(d.contains("base: \"X\", kind: String"), "{d}");
}

#[test]
fn rnd_of_a_bare_variable_parses_as_rnd_of_a_var() {
    let d = ast_debug("10 A=RND(X)\n");
    assert_eq!(count(&d, &rnd_of_var("X")), 1, "expected RND(Var X) in {d}");
}

#[test]
fn rnd_of_things_that_are_not_a_bare_variable_do_not_look_like_one() {

    for (src, why) in [
        ("10 A=RND(0)\n", "a literal zero"),
        ("10 A=RND(1)\n", "a literal one"),
        ("10 A=RND(.)\n", "a bare dot, which is also zero but is a literal"),
        ("10 A=RND(-1)\n", "a negative literal"),
        ("10 A=RND(X+1)\n", "an expression, not a bare variable"),
        ("10 A=RND(X*2)\n", "an expression, not a bare variable"),
    ] {
        let d = ast_debug(src);
        assert_eq!(
            count(&d, &rnd_of_var("X")),
            0,
            "{src:?} is {why} and must not parse as RND(bare var X): {d}"
        );
    }

    // An array element is a read of an array, not of a scalar of that name.
    let d = ast_debug("10 DIM X(4):A=RND(X(3))\n");
    assert_eq!(
        count(&d, &rnd_of_var("X")),
        0,
        "RND(X(3)) reads an array element, not a bare scalar: {d}"
    );
}

#[test]
fn an_assigned_variable_is_still_an_assignment_the_scan_can_see() {

    let d = ast_debug("5 Y=1\n10 A=RND(Y)\n");
    assert_eq!(count(&d, &rnd_of_var("Y")), 1, "the RND(Y) read: {d}");
    assert!(
        d.contains("Let { name: VarName { base: \"Y\", kind: Float }"),
        "the assignment Y=1 must be visible in the parse: {d}"
    );
    assert_eq!(
        count(&d, "base: \"Y\""),
        2,
        "Y occurs twice — once written, once read — so it is not 'nowhere else': {d}"
    );
}

#[test]
fn for_and_read_and_input_all_count_as_touching_the_variable() {

    for src in [
        "10 FOR Q=1 TO 3:NEXT Q\n20 A=RND(Q)\n",
        "10 READ Q\n20 A=RND(Q)\n30 DATA 7\n",
        "10 INPUT Q\n20 A=RND(Q)\n",
    ] {
        let d = ast_debug(src);
        assert!(
            count(&d, "base: \"Q\"") >= 2,
            "{src:?}: Q must appear more than once in the parse: {d}"
        );
    }
}
