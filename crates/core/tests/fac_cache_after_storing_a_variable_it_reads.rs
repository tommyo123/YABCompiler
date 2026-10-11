mod common;

use common::{compile_source, run_clean};

fn printed(src: &str) -> String {
    let prg = compile_source(src).expect("compiles");
    run_clean(&prg).expect("runs to a clean exit")
}

#[test]
fn a_variable_stepped_by_an_expression_is_not_that_expression_any_more() {
    assert_eq!(printed("10 I=I+.5:B=I+.5:PRINT I;B\n").trim_end(), " .5  1");
}

#[test]
fn a_running_sum_in_a_loop_reads_the_new_value() {
    let src = "10 READ A,Z\n\
               20 FOR K=1 TO 3:Z=Z+A:W=Z+A:PRINT W;:NEXT\n\
               30 DATA 1.5,3\n";
    assert_eq!(printed(src).trim_end(), " 6  7.5  9");
}
