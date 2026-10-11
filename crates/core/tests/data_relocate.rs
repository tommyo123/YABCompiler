mod common;

use yabcompiler_core::{AutoMode, CompileOptions, Profile, compile_with_options, tokenize_program};

fn moved(profile: Profile, extraram: bool) -> CompileOptions {
    CompileOptions {
        profile,
        extraram,
        data_relocate: AutoMode::On,
        ..CompileOptions::default()
    }
}

fn same_as_the_rom(src: &str) -> Vec<String> {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 20_000_000).expect("runs in the interpreter");
    let want = (common::petscii_to_string(&r.output), r.error_code, r.error_line);
    let mut asms = Vec::new();
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let c = compile_with_options(&prg, moved(profile, extraram))
                .unwrap_or_else(|e| panic!("compiles ({profile:?}, extraram {extraram}): {e}"));
            assert!(c.diagnostics.data_relocated > 0, "{profile:?}: the pool moves\n{src}");
            let r = emu64::run_prg_to_end(&c.prg_bytes, 20_000_000).expect("runs");
            assert_eq!(
                (common::petscii_to_string(&r.output), r.error_code, r.error_line),
                want,
                "{profile:?}, extraram {extraram}:\n{src}"
            );
            asms.push(c.asm);
        }
    }
    asms
}

#[test]
fn text_and_numbers() {
    same_as_the_rom(
        "10 DIM A(20),B$(5)\n\
         20 FOR I=1 TO 5:READ A(I):NEXT\n\
         30 FOR I=1 TO 3:READ B$(I):NEXT\n\
         40 READ X,Y$,Z%\n\
         50 PRINT A(1);A(5);B$(1);B$(3);X;Y$;Z%\n\
         60 RESTORE:READ Q:PRINT Q\n\
         70 READ C$:C$=C$+\"!\":PRINT C$\n\
         80 DATA 1,2,3,4,500,hello,\"world, x\",abc,3.25,text,-7\n",
    );
}

#[test]
fn binary_pools() {
    let cases = [
        ("binary u8", "1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,255,0"),
        ("binary i16", "1000,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,30000,300"),
        ("binary MFLPT", "1.5,2.25,300000,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,7.5,7"),
    ];
    for (layout, data) in cases {
        let asms = same_as_the_rom(&format!(
            "10 DIM A%(30),B(30)\n\
             20 FOR I=0 TO 9:READ A%(I):NEXT\n\
             30 FOR I=0 TO 9:READ B(I):NEXT\n\
             40 READ X%,Y\n\
             50 FOR I=0 TO 9:S=S+A%(I)+B(I):NEXT:PRINT S;X%;Y\n\
             60 RESTORE:READ Z:PRINT Z:READ Q:READ Q\n\
             70 DATA {data}\n"
        ));
        for asm in asms {
            assert!(asm.contains(&format!("; --- DATA pool ({layout}")), "{layout}\n{asm}");
        }
    }
}

#[test]
fn tagged_items_read_as_text() {
    same_as_the_rom(
        "10 FOR I=1 TO 6:READ A$:PRINT A$;\",\";:NEXT:PRINT\n\
         20 FOR I=1 TO 3:READ A:PRINT A;:NEXT:PRINT\n\
         30 READ A\n\
         40 DATA 1,2000,-5,hello,77,9\n\
         50 DATA 12,abc,3\n",
    );
}

#[test]
fn text_into_an_array() {
    same_as_the_rom(
        "10 DIM N$(20)\n\
         20 FOR I=0 TO 19:READ N$(I):NEXT\n\
         30 FOR I=0 TO 19 STEP 3:PRINT N$(I);\" \";:NEXT:PRINT\n\
         40 N$(1)=N$(1)+N$(2):PRINT N$(1)\n\
         50 DATA alpha,beta,gamma,delta,epsilon,zeta,eta,theta,iota,kappa\n\
         60 DATA lambda,mu,nu,xi,omicron,pi,rho,sigma,tau,upsilon\n",
    );
}

#[test]
fn a_bad_number_reports_its_line() {
    same_as_the_rom("10 READ A,B:PRINT A\n20 READ C\n30 DATA 1,2\n40 DATA x\n");
}

#[test]
fn the_buffer_stays_in_the_image_when_the_program_names_c000() {
    for asm in same_as_the_rom(
        "10 READ A,B:PRINT A;B\n\
         20 POKE 49152,1:PRINT PEEK(49152)\n\
         30 READ C$:PRINT C$\n\
         40 DATA 3,4,x\n",
    ) {
        assert!(asm.contains("D__DATA_BUF:"), "{asm}");
    }
}

#[test]
fn a_second_run_reads_the_pool_in_place() {
    let src = "10 READ A,B$:PRINT A;B$\n20 READ C:PRINT C\n30 DATA 7,seven,1.5\n";
    let prg = tokenize_program(src).expect("tokenizes");
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        let c = compile_with_options(&prg, moved(profile, false)).expect("compiles");
        assert!(c.diagnostics.data_relocated > 0);
        let (first, second) = common::load_and_run_twice(&c.prg_bytes);
        assert_eq!(first.0, " 7 SEVEN\n 1.5 \n", "{profile:?}");
        assert_eq!(first, second, "{profile:?}");
    }
}

#[test]
fn a_program_that_writes_under_the_kernal_keeps_its_pool() {
    let prg = tokenize_program("10 POKE 57344,1:READ A:PRINT A;PEEK(57344)\n20 DATA 5\n")
        .expect("tokenizes");
    let c = compile_with_options(&prg, moved(Profile::Default, false)).expect("compiles");
    assert_eq!(c.diagnostics.data_relocated, 0);
}

#[test]
fn auto_leaves_a_small_program_in_place() {
    let prg = tokenize_program("10 READ A:PRINT A\n20 DATA 5\n").expect("tokenizes");
    let c = compile_with_options(&prg, CompileOptions::default()).expect("compiles");
    assert_eq!(c.diagnostics.data_relocated, 0);
    assert_eq!(c.diagnostics.compressed_from, None);
}

fn data_walker(lines: usize, data_lines: usize) -> String {
    let mut out = String::from(
        "10 read k:if k=1 then gosub 1000\n\
         20 n=0:s=0\n\
         30 read a$:if a$=\"zzz\" then 60\n\
         40 n=n+1:s=s+len(a$)+asc(a$)\n\
         50 goto 30\n\
         60 print n;s\n\
         70 restore:read k,a$,b$:print a$;b$\n\
         80 dim q$(50):for i=0 to 50:read q$(i):next:print q$(0);q$(50)\n\
         90 end\n",
    );
    let v = b"abcdefghij";
    for i in 0..lines {
        let (a, b, c) = (v[i % 10] as char, v[(i * 3 + 1) % 10] as char, v[(i * 7 + 2) % 10] as char);
        out.push_str(&format!(
            "{} {a}={b}*{c}+{i}:if {a}>{b} then {c}={a}-{b}/3:{b}=sin({a})*{}\n",
            1000 + i,
            i % 9 + 1
        ));
    }
    out.push_str(&format!("{} return\n20000 data 0\n", 1000 + lines));
    for i in 0..data_lines {
        let items: Vec<String> = (0..4).map(|j| format!("w{i:03}x{j}abcdefgh")).collect();
        out.push_str(&format!("{} data {}\n", 20001 + i, items.join(",")));
    }
    out.push_str(&format!("{} data zzz\n", 20001 + data_lines));
    out
}

#[test]
fn auto_moves_and_packs_a_program_that_does_not_fit() {
    let small = tokenize_program(&data_walker(5, 80)).expect("tokenizes");
    let r = emu64::run_basic_to_end(&small, 50_000_000).expect("runs in the interpreter");
    let want = (common::petscii_to_string(&r.output), r.error_code);
    assert_eq!(want.1, None, "{}", want.0);

    let big = tokenize_program(&data_walker(560, 80)).expect("tokenizes");
    let off = CompileOptions { data_relocate: AutoMode::Off, ..CompileOptions::default() };
    assert!(compile_with_options(&big, off).is_err(), "premise: it does not fit as it is");
    let c = compile_with_options(&big, CompileOptions::default()).expect("compiles");
    assert!(c.diagnostics.data_relocated > 0);
    let plain = c.diagnostics.compressed_from.expect("the file is packed");
    assert!(0x0801 + plain - 2 > 0xD000, "premise: the plain file reaches past $CFFF");
    let (first, second) = common::load_and_run_twice(&c.prg_bytes);
    assert_eq!((first.0.clone(), first.1), want);
    assert_eq!(second.0, first.0);
}
