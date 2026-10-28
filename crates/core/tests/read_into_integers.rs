//! Integer READ targets decode tagged values and short digit strings directly.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

type Outcome = (Vec<u8>, Option<u8>, Option<u16>);

const BUDGET: u64 = 40_000_000;

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, BUDGET).expect("runs in the interpreter");
    assert!(r.ready_reached, "the interpreter reaches READY:\n{src}");
    (r.output, r.error_code, r.error_line)
}

fn compiled(src: &str, profile: Profile) -> (Outcome, String) {
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
        "ends ({profile:?}):\n{src}"
    );
    ((r.output, r.error_code, r.error_line), c.asm)
}

fn same_as_the_rom(src: &str) {
    let want = interpreted(src);
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let (got, _) = compiled(src, profile);
        assert_eq!(
            (common::petscii_to_string(&got.0), got.1, got.2),
            (common::petscii_to_string(&want.0), want.1, want.2),
            "{profile:?}:\n{src}"
        );
    }
}

const TAGGED: &str = "\
10 DIM A%(600)
20 FOR I=0 TO 599:READ A%(I):NEXT
30 S=0:FOR I=0 TO 599:S=S+A%(I):NEXT:PRINT S
40 RESTORE:FOR I=1 TO 600:READ B%:T=T+B%:NEXT:PRINT T
50 READ X$,Y%,Z:PRINT X$;Y%;Z
60 READ P%:PRINT P%
100 DATA 218,28651,148,216,35,5807,31427,27,4595,2331,24,6972,247,84,115,6792,63,158,68,31844\n\
120 DATA 25302,12,4620,58,36,26,23221,31048,72,35,24402,28720,5555,73,17362,238,211,6769,12673,21\n\
140 DATA 118,10320,18,25838,171,18803,4,164,61,5861,53,60,7,3,9897,0,30,24750,18,4926\n\
160 DATA 35,9239,217,9327,26418,52,54,19639,34,185,31100,210,17409,34,19060,2363,14,241,29383,22498\n\
180 DATA 157,27228,36,2,74,26,12,78,18180,30,83,180,100,97,42,12,2701,3651,20786,28905\n\
200 DATA 122,48,8554,88,126,38,132,109,50,56,1,216,150,115,15545,207,109,3,20,12203\n\
220 DATA 31179,148,19900,7177,34,203,10862,15435,30326,13,58,5606,192,13016,16925,153,144,26454,49,0\n\
240 DATA 17378,255,98,30,24770,22,133,6364,49,29,201,6901,584,245,77,14588,46,31848,5,32011\n\
260 DATA 650,34,2107,54,9546,146,2,31864,2,56,26318,12754,159,135,27651,3354,189,157,6733,227\n\
280 DATA 19,12,16,22169,12383,22,35,47,8721,11823,49,13632,27,16909,12186,29649,17144,57,142,14548\n\
300 DATA 27,27153,230,155,16916,11,245,188,82,32158,5790,9826,4150,63,12728,17,17519,4974,108,21663\n\
320 DATA 16830,32351,2171,224,84,48,5585,60,187,34,659,23261,207,204,17000,194,5561,240,184,59\n\
340 DATA 206,77,183,121,15068,5640,56,25590,135,56,28,53,8473,23,32158,16411,13625,42,240,24084\n\
360 DATA 955,159,31,219,34,11990,59,12157,29,18083,63,237,156,9005,10,1,22591,2613,165,245\n\
380 DATA 37,26,41,30,18846,41,120,55,230,60,43,47,64,63,1,47,10210,4,43,82\n\
400 DATA 27477,19036,17,202,49,12206,98,66,59,210,243,44,24264,179,248,37,30,30,28,27\n\
420 DATA 21,42,156,4160,17,163,27440,57,12,11142,57,178,23625,66,248,16,49,130,3,76\n\
440 DATA 7,194,66,55,254,9858,1151,245,185,122,13007,78,30,156,206,91,170,19,16833,172\n\
460 DATA 34,254,21,7505,8,254,76,7,1,21958,55,19,46,190,108,17,0,41,61,21862\n\
480 DATA 69,228,88,17105,707,4,27200,0,48,21,5564,13150,19485,32707,32534,46,122,5,196,83\n\
500 DATA 5,10601,160,47,216,49,46,8235,51,54,30,194,242,250,239,146,10106,37,178,5224\n\
520 DATA 4,199,49,38,32,116,25599,10666,40,61,20899,17950,236,153,18466,25609,31,17481,60,29845\n\
540 DATA 10,1,9149,63,245,19090,36,16666,11213,1333,56,9,80,520,4,45,158,93,172,28487\n\
560 DATA 152,22816,31932,131,55,58,2,209,1,21712,0,24951,29,8335,19380,152,42,214,16627,219\n\
580 DATA 22323,3158,22891,28,31,22346,17958,68,13124,25077,6,42,24723,13326,27,22,41,130,11,3743\n\
600 DATA 33,126,72,16230,11129,85,99,20290,10774,36,14003,78,192,27985,22128,13152,12306,29038,19,42\n\
620 DATA 136,13154,28636,88,244,1321,38,19088,33,69,20859,29904,17,4518,526,73,14,163,3,31321\n\
640 DATA 101,75,17,12501,17953,61,92,247,3137,15889,29,27742,160,209,7,30,31556,176,184,109\n\
660 DATA 19394,53,251,44,122,36,53,149,183,29830,255,176,25183,237,239,670,29,219,59,49\n\
680 DATA 107,17081,6,22460,144,6019,118,226,75,9794,18072,192,40,11536,18829,112,17305,10608,9,14\n\
900 DATA HELLO,1.5,2.5,40000
";

#[test]
fn tagged_items_into_integer_variables_and_arrays() {
    same_as_the_rom(TAGGED);
    let speed = compiled(TAGGED, Profile::Speed).1;
    assert!(speed.contains("__READ_TAGGED_INT:") && speed.contains("__READ_ARR_INT:"));
    assert!(
        compiled(TAGGED, Profile::Default)
            .1
            .contains("__READ_TAGGED_INT:")
    );
    assert!(
        !compiled(TAGGED, Profile::Size)
            .1
            .contains("__READ_TAGGED_INT:")
    );
}

#[test]
fn only_integer_targets_take_tagged_items_without_fac_in_every_profile() {
    let data: String = TAGGED
        .lines()
        .filter(|l| l.contains(" DATA ") && !l.starts_with("900"))
        .map(|l| format!("{l}\n"))
        .collect();
    let src = format!(
        "10 DIM A%(600)\n20 FOR I=0 TO 599:READ A%(I):NEXT\n\
         30 S=0:FOR I=0 TO 599:S=S+A%(I):NEXT:PRINT S\n\
         40 RESTORE:FOR I=1 TO 600:READ B%:T=T+B%:NEXT:PRINT T\n\
         50 READ Y%,P%:PRINT Y%;P%\n{data}900 DATA 1.5,-2.5\n"
    );
    same_as_the_rom(&src);
    for profile in [Profile::Default, Profile::Size] {
        let asm = compiled(&src, profile).1;
        assert!(asm.contains("__READ_TAGGED_INT:"), "{profile:?}:\n{asm}");
        assert!(!asm.contains("__READ_TAGGED:"), "{profile:?}:\n{asm}");
    }
}

#[test]
fn a_leading_minus_takes_the_digit_path() {
    let src = "\
10 FOR I=1 TO 30:READ A:S=S+A:NEXT:PRINT S
20 RESTORE:FOR I=1 TO 30:READ A%:T=T+A%:NEXT:PRINT T
30 READ Q$,Q,R,U:PRINT Q$;Q;R;U
100 DATA 276,-476,520,-265,629,415,931,724,516,336,889,86,-940,722,-46
110 DATA 590,932,-489,330,-893,846,-678,-768,-238,-39,779,-495,-220,114,-791
120 DATA \"X\",-,- 7,5-
";
    same_as_the_rom(src);
}
