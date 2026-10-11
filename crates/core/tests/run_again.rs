mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn same_as_the_rom_twice(src: &str) {
    let prg = tokenize_program(src).expect("tokenizes");
    let want = common::load_and_run_twice(&prg);
    assert_eq!(want.0, want.1, "premise: the ROM prints the same twice\n{src}");
    for profile in [Profile::Default, Profile::Speed, Profile::Size] {
        for extraram in [false, true] {
            let options = CompileOptions { profile, extraram, ..CompileOptions::default() };
            let c = compile_with_options(&prg, options)
                .unwrap_or_else(|e| panic!("compiles ({profile:?}, extraram {extraram}): {e}"));
            assert_eq!(
                common::load_and_run_twice(&c.prg_bytes),
                want,
                "{profile:?}, extraram {extraram}:\n{src}"
            );
        }
    }
}

#[test]
fn numbers() {
    same_as_the_rom_twice("10 for i=1 to 4:s=s+i:next:print s\n");
    same_as_the_rom_twice("10 for i=1 to 4:read a:s=s+a:next:print s\n20 data 1,2,3,4\n");
    same_as_the_rom_twice(&format!(
        "10 {}:print v0;v19\n",
        (0..20).map(|i| format!("v{i}=v{i}+1")).collect::<Vec<_>>().join(":")
    ));
}

#[test]
fn numbers_in_the_zero_tail() {
    let src = format!(
        "10 {}:print a;o\n",
        ('a'..='o').map(|v| format!("{v}={v}+1.5")).collect::<Vec<_>>().join(":")
    );
    let prg = tokenize_program(&src).expect("tokenizes");
    let c = compile_with_options(&prg, CompileOptions::default()).expect("compiles");
    assert!(c.asm.contains(";@@IF_NO_FILL@@"), "premise: the fill covers the variables\n{}", c.asm);
    same_as_the_rom_twice(&src);
}

#[test]
fn arrays() {
    same_as_the_rom_twice("10 dim a(5):a(1)=a(1)+1:print a(1)\n20 read x\n30 data 1\n");
    same_as_the_rom_twice(
        "10 dim a(300),b%(400):a(300)=a(300)+1:b%(7)=b%(7)+2:print a(300);b%(7)\n",
    );
    same_as_the_rom_twice("10 n=5:dim q(n):q(2)=q(2)+1:print q(2)\n");
    same_as_the_rom_twice("10 dim s$(3):print \"<\";s$(1);\">\":s$(1)=\"q\"+s$(1)\n");
}

#[test]
fn strings() {
    same_as_the_rom_twice("10 for i=1 to 2:print \"<\";a$;\">\":a$=a$+\"x\":next\n");
    same_as_the_rom_twice(
        "10 for i=1 to 2:print \"<\";a$;\">\":a$=left$(\"xyz\",i+len(a$)):next\n",
    );
}

#[test]
fn clr_and_run_in_the_program() {
    same_as_the_rom_twice(
        "10 dim a(300):print x;a(300):x=x+1:a(300)=a(300)+2:gosub 50\n\
         20 clr:dim a(300):print x;a(300):x=5:a(300)=7:read d:print d\n\
         30 data 4\n40 end\n50 return\n",
    );
    same_as_the_rom_twice(
        "5 if peek(1000)=2 then poke 1000,0\n\
         10 print x;b(2):x=x+1:b(2)=b(2)+1:poke 1000,peek(1000)+1:if peek(1000)<2 then run\n",
    );
}

#[test]
fn fn_definitions() {
    same_as_the_rom_twice("10 print x:x=x+1:def fn f(y)=y*2+x:print fn f(1)\n");
}
