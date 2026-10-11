mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (Vec<u8>, Option<u8>, Option<u16>);

const BUDGET: u64 = 20_000_000;

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, BUDGET).expect("runs in the interpreter");
    assert!(r.ready_reached, "the interpreter reaches READY:\n{src}");
    (r.output, r.error_code, r.error_line)
}

fn compiled(src: &str, profile: Profile) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, BUDGET).expect("runs");
    assert!(
        r.clean_exit || r.error_code.is_some(),
        "the compiled program ends ({profile:?}):\n{src}"
    );
    (r.output, r.error_code, r.error_line)
}

fn same_as_the_rom(src: &str) {
    let want = interpreted(src);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let got = compiled(src, profile);
        assert_eq!(
            (common::petscii_to_string(&got.0), got.1, got.2),
            (common::petscii_to_string(&want.0), want.1, want.2),
            "{profile:?}:\n{src}"
        );
        assert_eq!(got.0, want.0, "output bytes ({profile:?}):\n{src}");
    }
}

#[test]
fn issue2_fortest2() {
    same_as_the_rom("\
10 goto 30
20 next:end
30 for i=0 to 10
40 print i;
50 gosub 100
60 goto 20
100 for p=0 to 40
110 print tab(1),p
120 if p=4 then return
130 goto 20
");
}

#[test]
fn issue2_forty() {
    same_as_the_rom("\
10 for i=0 to 10
20 print i;
30 goto 80
40 next:end
50 print:for i=11 to 15
60 print i;
70 goto 40
80 next:goto 50
");
}

#[test]
fn issue2_forty_b() {
    same_as_the_rom("\
5 goto 10
6 next:if a=0 then end
7 goto 50
8 next:goto 50
10 for i=0 to 10
20 print i;
30 goto 80
40 a=0:goto 6
50 print:for i=11 to 15
60 print i;
70 goto 40
80 a=1:on (i and 1)+1 goto 6,8
");
}

#[test]
fn issue2_forty2() {
    same_as_the_rom("\
10 a=0:for i=0 to 5
20 print i:next
30 if a=0 then a=1:for i=6 to 10
40 if i=8 then 20
50 print i:next
");
}

#[test]
fn issue2_forty3() {
    same_as_the_rom("\
10 p=-1:for i=0 to 4
20 print i;:goto 50
50 next:if i<6 then 100
60 if p=-1 then for p=11 to 12:print p;:goto 50
70 print \"end!\":end
100 for i=5 to 10:print i;:goto 50
");
}

#[test]
fn issue2_forty5() {
    same_as_the_rom("\
10 for p=-3 to 3
20 for i=3 to 6:goto 40
30 print p;i,:next:end
40 for i=7 to 11:print i,:next
50 goto 30
");
}

#[test]
fn issue2_forty6() {
    same_as_the_rom("\
10 for p=-300 to -285
20 for i=300 to 306:goto 40
30 print p;i,:next:end
40 for i=700 to 711:print i,:next
50 goto 30
");
}

#[test]
fn issue2_forty7() {
    same_as_the_rom("\
10 for i=1 to 5:print i
20 on i goto 110,120,130,140,150
30 end
110 next
120 next
130 next
140 next
150 next:end
");
}

#[test]
fn g01_return_inside_sub_for() {
    same_as_the_rom("\
10 FOR I=1 TO 3:GOSUB 100:PRINT \"R\";I;:NEXT:END
100 FOR J=1 TO 5:PRINT J;:IF J=2 THEN RETURN
110 NEXT J:PRINT \"X\":RETURN
");
}

#[test]
fn g02_next_in_sub_sees_gosub() {
    same_as_the_rom("\
10 FOR I=1 TO 3
20 GOSUB 100
30 PRINT \"B\":END
100 PRINT I;:NEXT I
110 RETURN
");
}

#[test]
fn g03_goto_out_then_return_later() {
    same_as_the_rom("\
10 GOSUB 100:PRINT \"A\":END
100 PRINT \"S\";:GOTO 200
200 PRINT \"T\";:RETURN
");
}

#[test]
fn g04_return_without_gosub_late() {
    same_as_the_rom("\
10 FOR K=1 TO 3:GOSUB 100:NEXT
20 IF K>3 THEN 100
30 END
100 PRINT K;:RETURN
");
}

#[test]
fn g05_bounded_recursion() {
    same_as_the_rom("\
10 D=0:GOSUB 100:PRINT \"E\";D:END
100 D=D+1:PRINT D;:IF D<15 THEN GOSUB 100
110 RETURN
");
}

#[test]
fn g06_leaking_gosub() {
    same_as_the_rom("\
10 N=N+1:PRINT N;:GOSUB 20
20 IF N<50 THEN 10
30 PRINT \"Z\"
");
}

#[test]
fn g07_on_gosub_out_of_range() {
    same_as_the_rom("\
10 FOR X=0 TO 4:ON X GOSUB 100,200,300:PRINT X;:NEXT:END
100 PRINT \"A\";:RETURN
200 PRINT \"B\";:RETURN
300 PRINT \"C\";:RETURN
");
}

#[test]
fn g08_fall_into_shared_return() {
    same_as_the_rom("\
10 GOSUB 100:GOSUB 110:END
100 PRINT \"A\";
110 PRINT \"B\";:RETURN
");
}

#[test]
fn g09_deep_chain() {
    same_as_the_rom("\
10 GOSUB 11:PRINT \"Z\":END
11 GOSUB 12:RETURN
12 GOSUB 13:RETURN
13 GOSUB 14:RETURN
14 GOSUB 15:RETURN
15 GOSUB 16:RETURN
16 GOSUB 17:RETURN
17 GOSUB 18:RETURN
18 GOSUB 19:RETURN
19 GOSUB 20:RETURN
20 PRINT \"D\";:RETURN
");
}

#[test]
fn g10_same_for_var_in_sub() {
    same_as_the_rom("\
10 FOR I=1 TO 2:GOSUB 100:PRINT \"M\";I;:NEXT:END
100 FOR I=7 TO 8:PRINT I;:NEXT:RETURN
");
}

#[test]
fn g11_gosub_in_then() {
    same_as_the_rom("\
10 FOR I=1 TO 4:IF I/2=INT(I/2) THEN GOSUB 100
20 NEXT:END
100 PRINT I;:RETURN
");
}

#[test]
fn g12_end_inside_sub() {
    same_as_the_rom("\
10 GOSUB 100:PRINT \"NO\"
100 PRINT \"Y\";:END
");
}

#[test]
fn g13_return_in_main_for() {
    same_as_the_rom("\
10 FOR I=1 TO 3:PRINT I;:IF I=2 THEN RETURN
20 NEXT
");
}

#[test]
fn g14_leak_then_next() {
    same_as_the_rom("\
10 FOR I=1 TO 3
20 GOSUB 100
30 NEXT I
40 END
100 PRINT I;:GOTO 30
");
}

#[test]
fn g15_sub_reenters_main_loop() {
    same_as_the_rom("\
10 FOR I=1 TO 3:GOSUB 100:NEXT:PRINT \"Q\":END
100 IF I=2 THEN GOTO 120
110 RETURN
120 PRINT \"G\";:RETURN
");
}

#[test]
fn g16_gosub_return_to_mid_line() {
    same_as_the_rom("\
10 A=1:GOSUB 100:A=A+1:GOSUB 100:PRINT A:END
100 A=A*3:RETURN
");
}

#[test]
fn g17_for_left_by_return_reopened() {
    same_as_the_rom("\
10 GOSUB 100:GOSUB 100:PRINT \"OK\":END
100 FOR I=1 TO 9:IF I=3 THEN RETURN
110 NEXT:RETURN
");
}

#[test]
fn g18_next_without_var_after_gosub() {
    same_as_the_rom("\
10 FOR I=1 TO 3:FOR J=1 TO 2:GOSUB 100:NEXT:NEXT:END
100 PRINT I;J;:RETURN
");
}

#[test]
fn g19_run_restart() {
    same_as_the_rom("\
10 C=C+1:PRINT \"C\";C;:IF C<2 THEN GOSUB 100
20 END
30 PRINT \"R\":RETURN
100 RUN 30
");
}

#[test]
fn g20_clr_in_sub() {
    same_as_the_rom("\
10 GOSUB 100:PRINT \"B\"
20 END
100 X=5:CLR:PRINT X;:RETURN
");
}

#[test]
fn f01_var_written_in_body() {
    same_as_the_rom("\
10 FOR I=1 TO 10:PRINT I;:I=I+2:NEXT:PRINT \"E\";I
");
}

#[test]
fn f02_end_changed_in_body() {
    same_as_the_rom("\
10 N=3:FOR I=1 TO N:N=N+1:PRINT I;N;:NEXT:PRINT \"E\";I
");
}

#[test]
fn f03_step_changed_in_body() {
    same_as_the_rom("\
10 S=1:FOR I=1 TO 9 STEP S:S=S+1:PRINT I;:NEXT:PRINT \"E\";I
");
}

#[test]
fn f04_jump_into_body() {
    same_as_the_rom("\
10 I=5:GOTO 30
20 FOR I=1 TO 3
30 PRINT I;
40 NEXT I
50 PRINT \"E\"
");
}

#[test]
fn f05_next_two_vars() {
    same_as_the_rom("\
10 FOR I=1 TO 2:FOR J=1 TO 3:PRINT I;J;:NEXT J,I:PRINT \"E\";I;J
");
}

#[test]
fn f06_refor_same_var() {
    same_as_the_rom("\
10 FOR I=1 TO 3:FOR J=1 TO 3:PRINT I;J;:IF J=2 THEN FOR I=7 TO 8
20 NEXT:NEXT
30 PRINT \"E\";I;J
");
}

#[test]
fn f07_final_value_used() {
    same_as_the_rom("\
10 FOR I=0 TO 255:NEXT:PRINT I:FOR J=10 TO 1 STEP -3:NEXT:PRINT J
");
}

#[test]
fn f08_float_step() {
    same_as_the_rom("\
10 FOR X=0 TO 1 STEP .25:PRINT X;:NEXT:PRINT \"E\";X
");
}

#[test]
fn f09_negative_float() {
    same_as_the_rom("\
10 FOR X=-1.5 TO -3 STEP -.5:PRINT X;:NEXT:PRINT \"E\";X
");
}

#[test]
fn f10_step_zero_exit() {
    same_as_the_rom("\
10 FOR I=1 TO 2 STEP 0:C=C+1:IF C>4 THEN I=2
20 PRINT C;:NEXT:PRINT \"E\";I
");
}

#[test]
fn f11_peek_in_loop() {
    same_as_the_rom("\
10 FOR I=1 TO 3:POKE 830,I*7:PRINT PEEK(830);:NEXT
");
}

#[test]
fn f12_rnd_not_hoisted() {
    same_as_the_rom("\
10 X=RND(-1):FOR I=1 TO 4:PRINT INT(RND(1)*100);:NEXT
");
}

#[test]
fn f13_loop_in_then() {
    same_as_the_rom("\
10 FOR K=1 TO 2:IF K=1 THEN FOR I=1 TO 2:PRINT \"A\";I;
20 PRINT \"B\";K;:NEXT
");
}

#[test]
fn f14_goto_out_and_refor() {
    same_as_the_rom("\
10 FOR I=1 TO 5:IF I=3 THEN 30
20 NEXT
30 PRINT \"O\";I;:FOR I=1 TO 2:PRINT I;:NEXT:PRINT \"E\";I
");
}

#[test]
fn f15_next_wrong_var() {
    same_as_the_rom("\
10 FOR I=1 TO 2:FOR J=1 TO 2:PRINT I;J;:NEXT I
20 PRINT \"E\"
");
}

#[test]
fn f16_for_in_sub_next_in_main() {
    same_as_the_rom("\
10 GOSUB 100:NEXT I:PRINT \"E\"
20 END
100 FOR I=1 TO 3:RETURN
");
}

#[test]
fn f17_nested_same_var() {
    same_as_the_rom("\
10 FOR I=1 TO 2:FOR I=5 TO 6:PRINT I;:NEXT:NEXT:PRINT \"E\";I
");
}

#[test]
fn f18_array_loop_dim_late() {
    same_as_the_rom("\
10 FOR I=0 TO 3:A(I)=I*I:NEXT:PRINT A(2);A(3)
20 DIM B(5):FOR I=5 TO 0 STEP -1:B(I)=I:NEXT:PRINT B(0);B(5)
");
}

#[test]
fn f19_int_promote_overflow() {
    same_as_the_rom("\
10 FOR I=1 TO 3:X=X+30000:PRINT X;:NEXT
");
}

#[test]
fn f20_u8_counter_past_255() {
    same_as_the_rom("\
10 FOR I=250 TO 260 STEP 3:PRINT I;:NEXT:PRINT \"E\";I
");
}

#[test]
fn f21_licm_with_poke_in_body() {
    same_as_the_rom("\
10 V=1:FOR I=1 TO 3:Y=V*10:PRINT Y;:POKE 830,I:V=PEEK(830):NEXT
");
}

#[test]
fn f22_loop_var_string_mixed() {
    same_as_the_rom("\
10 FOR I=1 TO 3:A$=A$+STR$(I):NEXT:PRINT A$;LEN(A$)
");
}

#[test]
fn f23_next_in_if() {
    same_as_the_rom("\
10 FOR I=1 TO 6:IF I/2<>INT(I/2) THEN NEXT
20 PRINT \"V\";I;:IF I<6 THEN NEXT
30 PRINT \"E\"
");
}

#[test]
fn f24_shared_next_two_fors() {
    same_as_the_rom("\
10 FOR I=1 TO 2:GOTO 30
20 FOR J=1 TO 2
30 PRINT \"X\";:NEXT
40 PRINT \"E\";I;J
");
}

#[test]
fn f25_return_skips_for_frames() {
    same_as_the_rom("\
10 FOR I=1 TO 2:GOSUB 100:NEXT:PRINT \"E\";I;J
20 END
100 FOR J=1 TO 9:IF J=2 THEN RETURN
110 NEXT
");
}

#[test]
fn f26_on_goto_in_loop() {
    same_as_the_rom("\
10 FOR I=1 TO 4:ON I GOTO 20,30,20,30
20 PRINT \"A\";:NEXT
30 PRINT \"B\";:NEXT:PRINT \"E\"
");
}

#[test]
fn f27_while_style_loop() {
    same_as_the_rom("\
10 I=0
20 I=I+1:IF I<5 THEN PRINT I;:GOTO 20
30 PRINT \"E\";I
");
}

#[test]
fn f28_def_fn_loop_var() {
    same_as_the_rom("\
10 DEF FN F(X)=X*I:FOR I=1 TO 3:PRINT FN F(2);:NEXT
");
}

#[test]
fn f29_input_free_get_loop() {
    same_as_the_rom("\
10 FOR I=1 TO 3:GET A$:PRINT LEN(A$);:NEXT
");
}

#[test]
fn f30_float_counter_large() {
    same_as_the_rom("\
10 FOR X=1E9 TO 1.0000001E9 STEP 30:C=C+1:NEXT:PRINT C;X
");
}

#[test]
fn c01_clr_restart_after_leaving_a_subroutine() {
    same_as_the_rom("\
10 P=PEEK(830)+1:POKE 830,P:IF P>30 THEN PRINT \"OK\":END
20 GOSUB 100
100 PRINT P;:CLR:GOTO 10
");
}

#[test]
fn c02_clr_at_start_then_subroutines() {
    same_as_the_rom("\
10 CLR:A=1:GOSUB 100:GOSUB 200:PRINT A:END
100 A=A*2:GOSUB 200:RETURN
200 A=A+1:RETURN
");
}

#[test]
fn c03_return_after_clr_in_a_subroutine() {
    same_as_the_rom("\
10 GOSUB 100:PRINT \"NO\":END
100 PRINT \"A\";:CLR:RETURN
");
}

#[test]
fn c04_dispatched_next_with_subroutines() {
    same_as_the_rom("\
10 FOR I=1 TO 3:GOSUB 100:IF I=2 THEN 30
20 NEXT
30 PRINT \"M\";I;:IF I<3 THEN NEXT
40 GOSUB 100:END
100 FOR J=1 TO 2:PRINT J;:NEXT:RETURN
");
}


#[test]
fn k01_constant_set_in_a_subroutine_before_the_read() {
    same_as_the_rom("10 GOSUB 100:PRINT V;V+1:END\n100 V=53248:RETURN\n");
}

#[test]
fn k02_read_before_the_assignment_in_a_loop() {
    same_as_the_rom("10 N=N+1:PRINT V;:V=5:IF N<3 THEN 10\n");
}

#[test]
fn k03_clr_zeroes_a_constant() {
    same_as_the_rom("10 V=5:PRINT V;:CLR:PRINT V\n");
}

#[test]
fn k04_run_zeroes_a_constant() {
    same_as_the_rom("\
10 IF PEEK(830)=0 THEN POKE 830,1:V=3:RUN 20
20 PRINT V;PEEK(830):POKE 830,0
");
}

#[test]
fn k05_assignment_on_one_branch_only() {
    same_as_the_rom("10 FOR I=1 TO 3:IF I=2 THEN V=7\n20 PRINT V;:NEXT\n");
}

#[test]
fn k06_subroutine_reached_by_goto_before_the_assignment() {
    same_as_the_rom("\
10 GOTO 100
20 V=54272:GOSUB 100:END
100 PRINT V;:IF V=0 THEN 20
110 RETURN
");
}

#[test]
fn k07_constant_reassigned_after_clr() {
    same_as_the_rom("\
10 V=53248:GOSUB 100:CLR:PRINT V;:V=53248:GOSUB 100:END
100 PRINT V+21;:RETURN
");
}

#[test]
fn k08_run_is_the_only_way_to_a_read_without_the_assignment() {
    same_as_the_rom("\
10 V=3:IF PEEK(830)=0 THEN POKE 830,1:RUN 30
20 PRINT \"X\"
30 PRINT V;PEEK(830):POKE 830,0
");
}

#[test]
fn f31_dynamic_inner_counter_keeps_its_own_slot() {
    same_as_the_rom("\
10 FOR X=1000 TO 1064 STEP 8:FOR J=X TO X+7:C=C+1:NEXT:NEXT
20 FOR X=40000 TO 40002:NEXT
30 FOR X=300 TO 310:NEXT
40 PRINT C;X;J
");
}

#[test]
fn f32_a_poke_fill_loop_drops_the_open_frame_of_its_variable() {
    same_as_the_rom("\
20 FOR I=1 TO 3:PRINT \"A\";I;:IF I=2 THEN GOTO 40
30 NEXT I
40 FOR I=0 TO 5:POKE 1024+I,1:NEXT
50 PRINT \"B\";I;:NEXT I
60 PRINT \"E\"
");
}

#[test]
fn f33_a_poke_fill_loop_with_a_variable_base_drops_the_open_frame() {
    same_as_the_rom("\
10 V=1024
20 FOR I=1 TO 3:PRINT \"A\";I;:IF I=2 THEN GOTO 40
30 NEXT I
40 FOR I=0 TO 5:POKE V+I,1:NEXT
50 PRINT \"B\";I;:NEXT I
60 PRINT \"E\"
");
}

#[test]
fn f34_a_poke_fill_loop_beside_an_open_loop_of_another_variable() {
    same_as_the_rom("\
20 FOR I=1 TO 3:PRINT \"A\";I;:IF I=2 THEN GOTO 40
30 NEXT I
40 FOR J=0 TO 5:POKE 1024+J,1:NEXT
50 PRINT \"B\";J;PEEK(1029);:NEXT I
60 PRINT \"E\"
");
}


#[test]
fn l01_a_loop_after_the_frame_is_popped() {
    same_as_the_rom("\
10 FOR I=1 TO 3:PRINT I;:IF I=5 THEN 50
20 NEXT I
30 FOR I=7 TO 8:PRINT I;:NEXT I
40 PRINT \"E\":NEXT I
50 PRINT \"X\":END
");
}

#[test]
fn l02_return_drops_the_subroutine_loop() {
    same_as_the_rom("\
10 GOSUB 100:GOSUB 100:FOR I=1 TO 2:PRINT I;:NEXT I:NEXT I
100 FOR I=5 TO 9:PRINT I;:IF I=6 THEN RETURN
110 NEXT I:RETURN
");
}

#[test]
fn l03_a_next_in_an_open_call_stops_at_the_marker() {
    same_as_the_rom("\
10 FOR I=1 TO 3:PRINT I;:GOSUB 100
20 NEXT I:PRINT \"A\":END
100 IF I=2 THEN 200
110 RETURN
200 PRINT \"S\";:NEXT I:PRINT \"B\":END
");
}

#[test]
fn l04_a_recursive_loop_moves_the_callers_counter() {
    same_as_the_rom("\
10 D=0:GOSUB 100:PRINT \"E\":END
100 D=D+1:FOR I=1 TO 2:PRINT D;I;:IF D<2 THEN GOSUB 100
110 NEXT I:D=D-1:RETURN
");
}

#[test]
fn l05_a_subroutine_writes_an_open_loops_variable() {
    same_as_the_rom("\
10 FOR I=1 TO 300:PRINT I;:IF I=2 THEN 30
20 NEXT I:PRINT \"A\":END
30 GOSUB 100:NEXT I:PRINT \"B\":END
100 I=299:RETURN
");
}

#[test]
fn l06_a_write_before_a_dropping_loop() {
    same_as_the_rom("\
10 FOR I=1 TO 3:PRINT I;:IF I=2 THEN 30
20 NEXT I:PRINT \"A\":END
30 GOSUB 100:PRINT \"C\";:FOR I=1 TO 2:PRINT I;:NEXT:NEXT I
100 FOR J=1 TO 2:I=7:NEXT J:RETURN
");
}

#[test]
fn l07_clr_clears_the_open_frame() {
    same_as_the_rom("\
10 FOR I=1 TO 3:PRINT I;:IF I=2 THEN 30
20 NEXT I:PRINT \"A\":END
30 CLR:FOR I=1 TO 2:PRINT I;:NEXT I:PRINT \"B\":NEXT I
");
}

#[test]
fn l08_a_subroutine_loop_and_reset_under_an_open_frame() {
    same_as_the_rom("\
10 FOR I=1 TO 3:PRINT I;:IF I=2 THEN GOSUB 100:GOTO 30
20 NEXT I:PRINT \"A\":END
30 PRINT \"C\";:NEXT I:PRINT \"B\":END
100 FOR I=5 TO 6:PRINT \"S\";:NEXT I:FOR K=0 TO 1:POKE 1024+K,1:NEXT K:F=F+1:IF F=1 THEN I=0
110 RETURN
");
}

#[test]
fn l09_a_write_inside_another_loop() {
    same_as_the_rom("\
10 FOR I=1 TO 3:PRINT I;:IF I=2 THEN 30
20 NEXT I:PRINT \"A\":END
30 FOR J=1 TO 2:I=I+5:NEXT J:NEXT I:PRINT \"B\"
");
}

#[test]
fn l10_a_subroutine_loop_over_the_open_variable() {
    same_as_the_rom("\
10 FOR I=1 TO 3:PRINT I;:IF I=2 THEN 30
20 NEXT I:PRINT \"A\":END
30 GOSUB 100:NEXT I:PRINT \"B\":END
100 FOR I=7 TO 8:NEXT I:RETURN
");
}


#[test]
fn m01_routines_sharing_a_tail() {
    same_as_the_rom("\
10 A=1:GOSUB 100:PRINT A;:A=2:GOSUB 200:PRINT A;:END
100 A=A+10:GOTO 300
200 A=A+20
300 PRINT \"T\";A;:RETURN
");
}

#[test]
fn m02_a_routine_returns_through_code_it_jumps_to() {
    same_as_the_rom("\
10 GOSUB 100:PRINT \"B\";X:END
50 X=X+1:RETURN
100 X=5:GOTO 50
");
}

#[test]
fn m03_a_nested_call_popped_by_code_it_jumps_to() {
    same_as_the_rom("\
10 GOSUB 100:PRINT \"M\";Y:END
50 Y=Y*2:RETURN
100 Y=1:GOSUB 200:Y=Y+100:RETURN
200 Y=Y+10:GOTO 50
");
}

#[test]
fn m04_recursive_routine_returns() {
    same_as_the_rom("\
10 GOSUB 100:PRINT \"R\";Y:END
100 Y=Y+10:IF Y<30 THEN GOSUB 100
110 Y=Y+1:RETURN
");
}

#[test]
fn m05_main_code_reaching_a_return() {
    same_as_the_rom("\
10 GOSUB 100:Z=3
20 PRINT Z:RETURN
100 Z=1:RETURN
");
}

#[test]
fn m06_values_across_separate_routines() {
    same_as_the_rom("\
10 FOR K=1 TO 3:GOSUB 100:GOSUB 200:NEXT:PRINT S;T;C
20 GOSUB 300:PRINT C:END
100 A=K*2:S=S+A:RETURN
200 B=K*3:T=T+B:RETURN
300 C=7:RETURN
");
}

#[test]
fn m07_a_routine_also_entered_by_falling_in() {
    same_as_the_rom("\
10 Q=1:GOSUB 30:Q=Q+5
30 PRINT \"S\";Q:RETURN
");
}

#[test]
fn n01_shadows_sharing_a_slot() {
    same_as_the_rom("\
5 READ A$,D,Z$,IN$:DATA AB,1,X,CD
350 C=LEN(A$)
360 FOR E=1 TOC
390 B$(E)=MID$(A$,D,1)
570 R=R+1
660 T=W+R
1140 Z=ASC(Z$):ZL=LEN(IN$):IF(ZAND127)<32THENPRINT\" {$9D}\";
1200 IFZ=141THENZ$=CHR$(-20*(ZL>1)):FORZ=2TOZL
1300 PRINT B$(1);C;R
");
}


#[test]
fn f35_an_if_for_that_does_not_run_leaves_its_variable() {
    same_as_the_rom("\
10 READ Z:DATA 88
20 IF Z=141 THEN FOR Z=2 TO 3
30 PRINT Z;Z+1
");
}

#[test]
fn f36_an_if_for_whose_next_runs_without_it() {
    same_as_the_rom("\
10 READ Z:DATA 88
20 IF Z=141 THEN FOR Z=2 TO 3
30 PRINT Z;Z+1
40 NEXT
");
}

#[test]
fn f37_an_if_for_taken_once_and_skipped_once() {
    same_as_the_rom("\
10 FOR K=1 TO 2:READ Z:IF Z=141 THEN FOR Z=2 TO 3
20 PRINT K;Z;:IF K=1 THEN NEXT Z
30 NEXT K:PRINT \"E\"
40 DATA 141,88
");
}


#[test]
fn f38_a_failing_next_that_is_jumped_over_leaves_the_frames() {
    same_as_the_rom("\
31 FOR K=3 TO K+1
81 FOR K=2 TO A+1 STEP -1
110 IF C>60 THEN PRINT \"T\"
111 IF A*2<>A+1 THEN 160
121 NEXT I
160 IF C>60 THEN PRINT \"T\"
161 NEXT K
170 PRINT \"E\";K
");
}

#[test]
fn f39_next_of_an_outer_loop_abandons_the_inner_one() {
    same_as_the_rom("\
10 FOR I=1 TO 2:FOR J=1 TO 3:IF J=2 THEN NEXT I
20 PRINT I;J;:NEXT J:PRINT \"X\";:NEXT I:PRINT \"E\"
");
}

#[test]
fn f40_goto_to_an_outer_next_abandons_the_inner_loop() {
    same_as_the_rom("\
10 FOR I=1 TO 3:FOR J=1 TO 2
20 PRINT I;J;:IF I=2 THEN 40
30 NEXT J
40 NEXT I:PRINT \"E\";I;J
");
}


#[test]
fn m08_a_tail_call_in_main_code_keeps_its_error_line() {
    same_as_the_rom("\
10 GOSUB 80:RETURN
80 I=X*1:RETURN
");
}

#[test]
fn m09_a_tail_call_inside_a_routine() {
    same_as_the_rom("\
10 GOSUB 50:PRINT \"M\";A:END
50 A=A+1:GOSUB 80:RETURN
80 A=A*10:RETURN
");
}


#[test]
fn o01_a_gosub_to_a_missing_line_does_not_limit_depth() {
    same_as_the_rom("\
20 IF C>60 THEN PRINT \"T\":END
101 C=C+1
151 C=C+1
161 C=C+1:GOSUB 20:FOR K=1 TO 3:GOSUB 150
");
}

#[test]
fn o02_unbounded_recursion_runs_out_of_memory() {
    same_as_the_rom("10 D=D+1:GOSUB 10\n");
}

#[test]
fn o03_recursion_with_a_loop_per_level_runs_out_of_memory() {
    same_as_the_rom("10 D=D+1:FOR I=1 TO 2:GOSUB 10:NEXT\n");
}

#[test]
fn o04_bounded_recursion_deeper_than_the_stack() {
    same_as_the_rom("\
10 D=D+1:FOR I=1 TO 2:IF D<99 THEN GOSUB 10
20 PRINT D;:NEXT:RETURN
");
}

#[test]
fn f41_a_named_next_after_an_if_then_next_of_another_loop() {
    same_as_the_rom("\
41 FOR J=1 TO 2
51 GOSUB 160
100 IF C>60 THEN PRINT \"T\"
101 FOR J=2 TO A+1:IF A*1>9 THEN NEXT
121 IF 3<>X THEN 100
160 IF C>60 THEN PRINT \"T\"
161 NEXT I
");
}

#[test]
fn f42_a_partnerless_next_reached_from_a_leaking_loop() {
    same_as_the_rom("\
41 FOR I=3 TO 1
121 FOR I=0 TO 3:PRINT \"A\";I;:GOTO 150
130 PRINT \"Z\";
141 PRINT \"B\";:NEXT I
150 PRINT \"Q\";
171 PRINT \"C\";I;:NEXT
181 PRINT \"D\";:GOSUB 130
");
}

#[test]
fn f43_a_failed_search_in_an_if_keeps_the_open_loop() {
    same_as_the_rom("\
41 FOR J=3 TO 2
71 IF K*2=4 THEN NEXT I
121 PRINT \"B\";:NEXT
131 PRINT \"E\"
");
}

#[test]
fn f44_a_bare_next_reads_the_counter_a_store_set() {
    same_as_the_rom("\
31 FOR I=3 TO K+1
40 C=C+1:IF C>5 THEN PRINT \"T\";C:END
41 I=A*1:NEXT
191 GOTO 40
");
}


#[test]
fn f45_a_routine_calling_back_before_the_loop() {
    same_as_the_rom("\
30 PRINT \"S\";
31 FOR I=0 TO 4
61 GOSUB 160
160 PRINT \"N\";
161 NEXT:GOSUB 30
");
}

#[test]
fn f46_a_routine_reaching_the_loop_only_after_its_next() {
    same_as_the_rom("\
20 IF C>60 THEN PRINT \"T\"
21 FOR J=0 TO 4
31 GOSUB 80
80 IF C>60 THEN PRINT \"T\"
81 NEXT J
121 NEXT
141 GOTO 20
");
}

#[test]
fn f47_a_bare_next_after_a_conditional_next() {
    same_as_the_rom("\
71 FOR A=0 TO 2
121 X=B+3
151 FOR K=1 TO X+1:PRINT A;K;:IF K*2>B+3 THEN NEXT
161 PRINT \"N\";:NEXT
171 PRINT \"E\"
");
}

#[test]
fn f48_a_next_after_a_run_time_next_without_a_text_partner() {
    same_as_the_rom("\
120 IF C>60 THEN PRINT \"T\"
121 FOR I=1 TO 4:GOSUB 130
130 IF C>60 THEN PRINT \"T\"
131 FOR A=1 TO I+1:FOR I=3 TO X+1 STEP 2:NEXT A:NEXT
151 IF A=B THEN GOTO 120
");
}

#[test]
fn f49_a_next_finding_a_maybe_open_loop_below_another() {
    same_as_the_rom("\
10 FOR Q=1 TO 5:PRINT \"Q\";Q;:IF Q<3 THEN NEXT Q
20 FOR J=1 TO 2:PRINT \"J\";J;
30 NEXT Q
40 NEXT J
50 PRINT \"E\"
");
}

#[test]
fn f50_a_return_into_a_line_the_analysis_saw_first() {
    same_as_the_rom("\
10 READ C
40 GOSUB 800
70 J%=3:IF C<0 THEN GOSUB 900
80 PRINT D(J%)
120 END
800 X=1
900 J%=J%*SGN(C):RETURN
990 DATA -1
");
}

#[test]
fn f51_a_goto_past_the_for_that_would_drop_the_frame() {
    same_as_the_rom("\
41 FOR J=2 TO 2
51 IF I<5 THEN 160
111 FOR K=1 TO X+1
131 NEXT:FOR J=0 TO 4
151 NEXT
160 PRINT J;
161 NEXT
171 PRINT J
");
}

#[test]
fn f52_an_earlier_discard_that_a_gosub_runs_past() {
    same_as_the_rom("\
91 FOR K=3 TO J+1:GOTO 170
101 FOR K=1 TO 1
131 FOR K=2 TO 2
151 NEXT K:RETURN
170 PRINT K;
171 GOSUB 101
191 NEXT
201 PRINT K
");
}

#[test]
fn f53_a_next_on_another_line_re_enters_the_body_with_its_line() {
    same_as_the_rom("\
41 FOR K=0 TO B+1:IF K+2=3 THEN PRINT I;:NEXT I
61 NEXT
");
}

#[test]
fn f54_counting_down_to_zero() {
    same_as_the_rom("\
10 FOR I=10 TO 0 STEP -1:PRINT I;:NEXT:PRINT I
20 FOR J=9 TO 0 STEP -2:PRINT J;:NEXT:PRINT J
30 FOR K=-5 TO 0 STEP -1:PRINT K;:NEXT:PRINT K
40 READ S:FOR I=S TO 0 STEP -1:PRINT I;:NEXT:PRINT I
50 FOR A=3 TO 0 STEP -1:FOR B=A TO 0 STEP -1:PRINT A*10+B;:NEXT B,A
60 DATA 7
");
}

#[test]
fn f55_a_for_inside_then_restamps_its_line() {
    same_as_the_rom("10 IF K=0 THEN FOR A=1 TO 2:IF A>1 THEN RETURN\n20 NEXT A\n");
    same_as_the_rom("10 IF K=0 THEN PRINT \"X\";:FOR A=1 TO 2:PRINT 1/(A-2)\n20 NEXT A\n");
    same_as_the_rom("10 IF K=0 THEN IF J=0 THEN FOR A=1 TO 2:PRINT A;:X=1/(2-A)\n20 NEXT\n");
}

#[test]
fn f56_to_and_step_see_the_start_value() {
    same_as_the_rom("10 J=5:FOR J=3 TO J+1:PRINT J;:NEXT:PRINT \"/\";J\n");
    same_as_the_rom("10 K=2:FOR K=K*5 TO K+12 STEP K-1:PRINT K;:NEXT:PRINT \"/\";K\n");
    same_as_the_rom("10 DEF FN A(X)=I+X:I=9:FOR I=1 TO FN A(2):PRINT I;:NEXT\n");
    same_as_the_rom("10 I=1:IF I THEN FOR I=I+1 TO I*3:PRINT I;:NEXT:PRINT \"/\"\n");
}

#[test]
fn f57_a_conditionally_closed_loop_keeps_its_frame() {
    same_as_the_rom(
        "81 FOR I=0 TO K+1\n120 C=C+1:IF C>9 THEN PRINT \"T\":END\n\
         121 PRINT \"N\";:NEXT:PRINT \"A\";:FOR J=2 TO X+1:IF K+3=X*2 THEN NEXT\n\
         131 PRINT J;:IF X*1<>J*2 THEN 120\n",
    );
    same_as_the_rom(
        "10 FOR I=1 TO 2:FOR J=1 TO 5:IF J<3 THEN NEXT J\n20 PRINT I;J;:C=C+1:IF C<4 THEN NEXT\n30 PRINT \"/\"\n",
    );
}
