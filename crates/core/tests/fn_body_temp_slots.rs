mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

const PROFILES: [Profile; 3] = [Profile::Default, Profile::Speed, Profile::Size];

fn build(src: &str, profile: Profile) -> Vec<u8> {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("must compile ({profile:?}): {e}\n{src}"))
    .prg_bytes
}

#[track_caller]
fn check(src: &str, want: &str) {
    for profile in PROFILES {
        let prg = build(src, profile);
        let r = emu64::run_prg_to_end(&prg, common::MAX_INSNS).expect("run");
        assert_eq!(r.error_banner(), None, "({profile:?}):\n{src}");
        assert!(r.clean_exit, "clean exit expected ({profile:?}):\n{src}");
        assert_eq!(
            common::petscii_to_string(&r.output).trim(),
            want,
            "({profile:?}):\n{src}"
        );
    }
}

#[test]
fn two_calls_in_one_sum_keep_separate_results() {
    check(
        "1 def fnh(c)=c-48+7*(c>64)\n2 print fnh(65)+fnh(56)\n",
        "18",
    );
}

#[test]
fn call_times_constant_plus_call() {
    check(
        "1 def fnh(c)=c-48+7*(c>64)\n2 print fnh(65)*16+fnh(56)\n",
        "168",
    );
}

/// Non-commutative, so an accidentally-symmetric result cannot pass: `10 - 8 = 2`, while the clobbered form gave `8 - 8 = 0`.
#[test]
fn two_calls_in_one_difference() {
    check("1 def fnh(c)=c-48+7*(c>64)\n2 print fnh(65)-fnh(56)\n", "2");
}

/// Three calls in one expression: two `T`-slot handoffs, not one.
#[test]
fn three_calls_in_one_expression() {
    check(
        "1 def fnh(c)=c-48+7*(c>64)\n2 print fnh(65)*100+fnh(56)*10+fnh(57)\n",
        "1089",
    );
}

/// A body needs an interior operator to want a temp.
#[test]
fn relational_inside_the_body_survives_a_second_call() {
    check("1 def fnr(c)=7*(c>64)\n2 print fnr(65)+fnr(48)\n", "-7");
}

/// Two different functions in one expression, so the two bodies are live at once as well as the caller.
#[test]
fn two_distinct_functions_in_one_expression() {
    check(
        "1 def fna(x)=x*x+1\n2 def fnb(y)=y*y+1\n3 print fna(3)*10+fnb(4)\n",
        "117",
    );
}

#[test]
fn nested_call_from_inside_a_body() {
    check(
        "1 def fni(x)=x*x+1\n2 def fno(y)=fni(y)*100+y*2\n3 print fno(3)\n",
        "1006",
    );
}

#[test]
fn stretch64_first_data_line_checksums() {
    check(
        "1 def fnh(c)=c-48+7*(c>64)\n\
         2 read a$,a\n\
         3 ch=0\n\
         4 for f=0 to 31\n\
         5 q=fnh(asc(mid$(a$,f*2+1)))*16+fnh(asc(mid$(a$,f*2+2)))\n\
         6 ch=ch+q:next\n\
         7 if ch=a then print \"ok\":end\n\
         8 print \"checksum error\"\n\
         100 data 78A9648D1403A9C08D1503A23EBD96C09D4003CA10F7A207A90D9DF807A9029D, 3614\n",
        "OK",
    );
}

#[test]
fn trivial_body_still_works_twice() {
    check("1 def fng(c)=c+1\n2 print fng(1)*10+fng(2)\n", "23");
}

#[test]
fn single_call_is_unchanged() {
    check("1 def fnh(c)=c-48+7*(c>64)\n2 print fnh(65)\n", "10");
    check("1 def fnh(c)=c-48+7*(c>64)\n2 print fnh(56)\n", "8");
    check("1 def fnh(c)=c-48+7*(c>64)\n2 print fnh(48);fnh(57)\n", "0  9");
}

#[test]
fn parameter_variable_is_restored_after_two_calls() {
    check(
        "1 def fnh(c)=c-48+7*(c>64)\n2 c=5\n3 print fnh(65)+fnh(56);c\n",
        "18  5",
    );
}

#[test]
fn main_program_temps_survive_a_call() {
    check(
        "1 def fnh(c)=c-48+7*(c>64)\n2 a=3:b=4\n3 print a*100+fnh(65)*10+b\n",
        "404",
    );
    check(
        "1 def fnh(c)=c-48+7*(c>64)\n2 a=3:b=4\n3 print fnh(65)*100+a*10+b\n",
        "1034",
    );
}

/// A redefined name is bound through the dispatch trampoline instead of statically, and its body owns the parameter save/restore.
#[test]
fn redefined_name_dispatches_and_still_separates_temps() {
    check(
        "1 def fnh(c)=c-48+7*(c>64)\n\
         2 print fnh(65)+fnh(56)\n\
         3 def fnh(c)=c-48+7*(c>64)+50\n\
         4 print fnh(65)+fnh(56)\n",
        "18 \n 118",
    );
}

/// A body whose result feeds straight into a second call's argument ; no temp handoff in the caller at all, so this must be unaffected.
#[test]
fn call_as_the_argument_of_a_call() {
    check("1 def fng(c)=c+1\n2 print fng(fng(1))\n", "3");
}
