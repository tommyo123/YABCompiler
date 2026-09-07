use yabcompiler_core::{
    CompileOptions, Dialect, Profile, compile_with_options, tokenize_program_with,
};

fn asm_dialect(src: &str, dialect: Dialect) -> String {
    let prg = tokenize_program_with(src, dialect).expect("tokenizes");
    let opts = CompileOptions {
        profile: Profile::Default,
        dialect,
        ..CompileOptions::default()
    };
    compile_with_options(&prg, opts)
        .unwrap_or_else(|e| panic!("compiles: {e}"))
        .asm
}

fn asm(src: &str) -> String {
    asm_dialect(src, Dialect::BasicV2)
}

fn traps(a: &str) -> bool {
    a.contains("JMP __ILLEGAL_QTY") || a.contains("__ILLEGAL_QTY:")
}

fn reads_ffff(a: &str) -> bool {
    a.contains("LDA $FFFF") || a.contains("LDY $FFFF")
}

fn check(src: &str, what: &str) {
    let a = asm(src);
    assert!(
        traps(&a),
        "{what}: the ROM raises ?ILLEGAL QUANTITY for PEEK(-1); \
         no trap was emitted\n{a}"
    );
    assert!(
        !reads_ffff(&a),
        "{what}: a bare absolute read of $FFFF survived the trap\n{a}"
    );
}

#[test]
fn poke_value_peek_negative_traps() {
    check("10 POKE 1024,PEEK(-1)\n", "POKE 1024,PEEK(-1)");
}

#[test]
fn poke_value_peek_negative_in_a_sum_traps() {
    check("10 POKE 1024,PEEK(-1)+1\n", "POKE 1024,PEEK(-1)+1");
}

#[test]
fn integer_var_assigned_peek_negative_traps() {
    check("10 A%=PEEK(-1)\n20 PRINT A%\n", "A%=PEEK(-1)");
}

#[test]
fn if_condition_peek_negative_traps() {
    check("10 IF PEEK(-1)=0 THEN PRINT \"Z\"\n", "IF PEEK(-1)=0");
}

#[test]
fn poke_address_from_peek_negative_traps() {
    check("10 POKE PEEK(-1),7\n", "POKE PEEK(-1),7");
}

#[test]
fn fac_context_peek_negative_still_traps() {
    check("10 PRINT PEEK(-1)\n", "PRINT PEEK(-1)");
}

/// Control on the constant class.
#[test]
fn poke_peek_over_65535_still_traps() {
    check("10 POKE 1024,PEEK(70000)\n", "POKE 1024,PEEK(70000)");
}

#[test]
fn a_legal_address_is_not_trapped() {
    let a = asm("10 POKE 1024,PEEK(53280)\n");
    assert!(
        a.contains("LDA $D020"),
        "PEEK(53280) is a legal address: it must still compile to a direct \
         read of $D020, not to a trap:\n{a}"
    );
    assert!(
        !a.contains("JMP __ILLEGAL_QTY"),
        "and nothing in this statement may jump to the range trap:\n{a}"
    );
}

/// The exclusion, on the record.
#[test]
fn mempeek_is_deliberately_not_trapped() {
    let a = asm_dialect("10 POKE 1024,MEMPEEK(-1)\n", Dialect::Tsb);
    assert!(
        a.contains("__MEMPEEK_") || a.contains("LDY $FFFF") || a.contains("LDA $FFFF"),
        "MEMPEEK is consistently untrapped in both contexts — `emit_mempeek` \
         does not range-check either — so the read must still be emitted. \
         If this is changed deliberately, change `emit_mempeek` too:\n{a}"
    );
}
