mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

fn text_heavy_program() -> String {
    let words = [
        "THE", "CAVE", "IS", "DARK", "AND", "COLD", "A", "PASSAGE", "LEADS", "NORTH", "YOU",
        "SEE", "LAMP", "HERE", "DOOR", "LOCKED", "TREASURE", "DRAGON", "SLEEPS", "QUIETLY",
    ];
    let mut src = String::from("10 A$=\"SHARED LITERAL\":PRINT A$\n");
    let mut line = 20;
    for i in 0..48 {
        let text: Vec<&str> = (0..9).map(|k| words[(i * 7 + k * 3) % words.len()]).collect();
        let text = text.join(" ");
        let stmt = match i % 8 {
            0 => format!("PRINT \"{text}\""),
            1 => format!("PRINT \"{text}\";:PRINT \"!\""),
            2 => format!("PRINT \"{text}\",\"X\""),
            3 => format!("PRINT TAB(5)\"{text}\""),
            4 => format!("PRINT SPC(3)\"{text}\";\"SHARED LITERAL\""),
            5 => format!("PRINT \"{{RVS ON}}{text}{{RVS OFF}}\""),
            6 => format!("PRINT \"{text} ----------------------\""),
            _ => format!("PRINT \"{{DOWN}}{{RED}}{text}{{WHITE}}\""),
        };
        src.push_str(&format!("{line} {stmt}\n"));
        line += 10;
    }
    src.push_str(&format!("{line} IF A$=\"SHARED LITERAL\" THEN PRINT \"SAME\"\n"));
    src
}

#[test]
fn packed_print_text_prints_what_the_rom_prints() {
    let src = text_heavy_program();
    let prg = tokenize_program(&src).expect("tokenizes");
    let rom = emu64::run_basic_to_end(&prg, 40_000_000).expect("runs in the interpreter");
    let want = (common::petscii_to_string(&rom.output), rom.error_code);
    for profile in [Profile::Size, Profile::Default] {
        let c = compile_with_options(
            &prg,
            CompileOptions {
                profile,
                ..CompileOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}"));
        assert_eq!(
            c.asm.contains("__CP_BEGIN:"),
            profile == Profile::Size,
            "{profile:?}: only Size packs PRINT text"
        );
        let r = emu64::run_prg_to_end(&c.prg_bytes, 40_000_000).expect("runs");
        assert_eq!((common::petscii_to_string(&r.output), r.error_code), want, "{profile:?}");
    }
}
