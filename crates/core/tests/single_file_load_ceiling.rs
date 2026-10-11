use yabcompiler_core::{
    CompileError, CompileOptions, Compiled, Profile, compile_with_options, tokenize_program,
};

/// First address of the register block.
const IO_LO: u32 = 0xD000;

fn options_at(origin: Option<u16>, profile: Profile) -> CompileOptions {
    CompileOptions {
        profile,
        custom_start_address: origin,
        ..CompileOptions::default()
    }
}

fn build(src: &str, options: CompileOptions) -> Result<Compiled, CompileError> {
    let prg = tokenize_program(src).expect("tokenizes");
    compile_with_options(&prg, options)
}

/// Last address the loaded image occupies at run time: the stored body plus the BSS tail the truncation left out of the file.
fn runtime_end(c: &Compiled) -> u32 {
    let start = u32::from(c.diagnostics.start_address);
    let len = (c.prg_bytes.len() - 2) as u32 + c.diagnostics.bss_truncated as u32;
    start + len - 1
}

fn stored_end(c: &Compiled) -> u32 {
    u32::from(c.diagnostics.start_address) + (c.prg_bytes.len() - 2) as u32 - 1
}

/// 4002 bytes of zeroed array: the tail truncates, so the stored body stays tiny while the run-time span does not.
const BIG_BSS: &str = "10 DIM A%(2000)\n20 PRINT\"HI\"\n";

fn big_data() -> String {
    let mut s = String::from("10 PRINT\"A\"\n");
    for i in 0..300 {
        s.push_str(&format!(
            "{} DATA 111111,222222,333333,444444,555555,666666,777777,888888\n",
            20 + i
        ));
    }
    s
}

fn must_build() -> Vec<(&'static str, String, CompileOptions)> {
    vec![
        (
            "big BSS at the default origin",
            BIG_BSS.to_string(),
            options_at(None, Profile::Default),
        ),
        (
            "big BSS at the default origin, Profile::Size",
            BIG_BSS.to_string(),
            options_at(None, Profile::Size),
        ),
        (
            "big DATA pool at the default origin",
            big_data(),
            options_at(None, Profile::Default),
        ),
        (

            "big BSS at $C015 — last byte exactly on $CFFF",
            BIG_BSS.to_string(),
            options_at(Some(0xC015), Profile::Default),
        ),
    ]
}

fn must_refuse() -> Vec<(&'static str, String, CompileOptions, u32)> {
    vec![
        (
            "big BSS at $C016 — one byte past the ceiling",
            BIG_BSS.to_string(),
            options_at(Some(0xC016), Profile::Default),
            1,
        ),
        (
            "big BSS at $CE00 — only the entry-time fill overruns",
            BIG_BSS.to_string(),
            options_at(Some(0xCE00), Profile::Default),
            0xDDEA - (IO_LO - 1),
        ),
        (
            "big DATA at $C000 — the overrun is stored bytes",
            big_data(),
            options_at(Some(0xC000), Profile::Default),
            0xEF09 - (IO_LO - 1),
        ),
    ]
}

/// The message the gate raises, or `None` if the build succeeded or failed for some unrelated reason.
fn io_hole_message(r: &Result<Compiled, CompileError>) -> Option<String> {
    match r {
        Err(CompileError::Assemble { message, .. }) if message.contains("of the I/O area") => {
            Some(message.clone())
        }
        _ => None,
    }
}

#[test]
fn builds_end_at_or_below_cfff() {
    for (what, src, opts) in must_build() {
        let c = build(&src, opts).unwrap_or_else(|e| panic!("{what}: must build, got {e:?}"));
        let end = runtime_end(&c);
        assert!(
            end < IO_LO,
            "{what}: run-time end ${end:04X} is not below ${IO_LO:04X}",
        );
        assert!(
            stored_end(&c) <= end,
            "{what}: stored end above run-time end — the split is inverted",
        );
    }
}

#[test]
fn exactly_cfff_builds_and_one_byte_more_does_not() {
    let ok = build(BIG_BSS, options_at(Some(0xC015), Profile::Default)).expect("$C015 builds");
    assert_eq!(
        runtime_end(&ok),
        IO_LO - 1,
        "the tight case must land its last byte exactly on $CFFF, or the \
         boundary below is not being tested at one-byte resolution",
    );

    let bad = build(BIG_BSS, options_at(Some(0xC016), Profile::Default));
    let msg = io_hole_message(&bad).expect("$C016 must be refused by the I/O-area gate");
    assert!(
        msg.contains("remove about 1 bytes"),
        "one byte over must be reported as one byte over; got: {msg}",
    );
    assert!(
        msg.contains("end at or before $CFFF"),
        "the message must name the ceiling it is enforcing; got: {msg}",
    );
}

#[test]
fn stored_bytes_past_the_hole_are_unloadable() {
    let r = build(&big_data(), options_at(Some(0xC000), Profile::Default));
    let msg = io_hole_message(&r).expect("a 12 KB DATA pool from $C000 must be refused");
    // The distinguishing fact: nothing here is BSS, so the bytes are in the file and a KERNAL LOAD is what cannot deliver them.
    assert!(
        msg.contains("A KERNAL LOAD stores that range into the VIC/SID/CIA registers"),
        "the refusal must say it is the LOAD that fails; got: {msg}",
    );
}

#[test]
fn bss_only_overrun_is_still_refused() {
    // Stored body is ~73 bytes ending around $CE48 ; comfortably loadable.
    let r = build(BIG_BSS, options_at(Some(0xCE00), Profile::Default));
    let msg = io_hole_message(&r)
        .expect("an image whose BSS fill reaches the register block must be refused");
    assert!(
        msg.contains("$CE00-$DDEA"),
        "the refusal must report the RUN-TIME span, not the stored one; got: {msg}",
    );

    let ok = build(BIG_BSS, options_at(Some(0xC015), Profile::Default));
    assert!(ok.is_ok(), "the same program at $C015 must build");
}

#[test]
fn the_two_lists_score_zero_on_each_other() {
    // Swap the lists' roles.
    for (what, src, opts) in must_build() {
        let r = build(&src, opts);
        assert!(
            io_hole_message(&r).is_none(),
            "{what} is in the must-BUILD list but the I/O gate refused it",
        );
        assert!(
            r.is_ok(),
            "{what} has to build, or the positive side is not holding the \
             suite honest",
        );
    }

    for (what, src, opts, excess) in must_refuse() {
        let r = build(&src, opts);
        assert!(
            r.is_err(),
            "{what} is in the must-REFUSE list but it built",
        );
        let msg = io_hole_message(&r)
            .unwrap_or_else(|| panic!("{what} was refused, but not by the I/O-area gate"));
        assert!(
            msg.contains(&format!("remove about {excess} bytes")),
            "{what}: expected an excess of {excess}; got: {msg}",
        );
    }
}
