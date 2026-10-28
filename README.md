# YABCompiler

Yet Another Basic Compiler for the Commodore 64. Compiles BASIC V2
and selected Simons' BASIC and Tuned Simons' BASIC commands to 6502
machine code.

Input is `.bas` source or a tokenized `.prg`. The compiled `.prg` runs
on a stock C64 or in an emulator.

## Version 1.0.3

Loops, math, arrays, strings and READ run faster. The default and size
profiles produce smaller programs. Runtime errors match C64 BASIC
more closely.

New options: `--no-error-checks` and `--fast-float`. Both are available
in the CLI and GUI.

## Download

Prebuilt packages are on the
[Releases page](https://github.com/tommyo123/YABCompiler/releases).

* Windows MSI installer. Installs the GUI and CLI with Start Menu and
  Desktop shortcuts.
* Windows portable ZIP. Unpack and run, no install.
* Linux and macOS tarballs with both binaries.

To build from source instead, see [BUILD.md](BUILD.md).

## Build

Rust 1.92 or newer:

```sh
cargo build --release -p yabcompiler-cli
cargo build --release -p yabcompiler-gui
```

The CLI binary is at `target/release/yabcompiler`, the GUI at
`target/release/yabcompiler-gui`. Building the Windows installer and
the Linux library requirements are covered in [BUILD.md](BUILD.md).

## Usage

CLI:

```sh
yabcompiler compile mygame.bas mygame.prg
```

Run `yabcompiler` with no arguments for the full list of commands and
options.

Use `--dialect=basic2` for BASIC V2 or `--dialect=tsb` for extended
keywords. The default, `--dialect=auto`, detects extended commands and
keeps names used as BASIC V2 variables. Both `compile` and `tokenize`
accept this option.

Choose `--profile=speed` or `--profile=size` to favor speed or smaller
programs. The default profile balances both.

`--compress` writes a compressed `.prg` with its decoder. By default
a file that reaches past `$CFFF` is compressed.

`--data-relocate` moves DATA to RAM at `$D000` when the program starts,
which leaves more room for the program and its heap. By default this
happens when a program needs the room.

`--no-error-checks` removes runtime error checks and line tracking for
smaller, faster programs. Use it only with programs that run without
errors.

`--fast-float` adds faster math routines with the same results as the
C64 BASIC ROM. The file gets larger. The compiler leaves this option
out if the program needs the memory.

GUI: launch `yabcompiler-gui`, select a `.bas` or `.prg`, then press
Compile. The left panel includes the dialect, No error checks and
Fast float options. Output and generated assembly appear on the right.

Auto Extra RAM may leave too little heap for variables in large compiled
programs. If you suspect this, try Move DATA set to On (`--data-relocate=on`)
first, as it can free more heap. Force Extra RAM on (`--extraram`) if needed.

Auto-reserve can miss memory ranges. If this causes problems, add the missing
ranges manually in the GUI or with `--reserved`.

## Features

* BASIC V2 compatible, plus a useful subset of Simons' BASIC and Tuned
  Simons' BASIC commands (see below).
* Type inference promotes integer-only variables to byte or word
  storage where it is safe to do so. `FRE()` reports this compiler's
  heap, not the interpreter's number, because a compiled program has
  no BASIC variable table.
* Faster loops, math, arrays, strings and READ. Compact DATA, text and
  shared code reduce file size. Trailing zero data is cleared at startup
  when this saves space in the file.
* Optional extra RAM mode banks BASIC ROM out and uses
  `$A000-$BFFF` for program code or data. The compiler decides
  automatically based on size, or you can force it.
* Auto-reserve scans the program for POKE/PEEK targets and tells the
  assembler to skip those addresses, so sprite blocks, custom
  charsets and screen buffers stay intact.
* Custom start address option: load at an address you pick and start
  it manually (e.g. load at `$C000`, run with `SYS 49152`).
* Lenient parser mode tolerates the source-level typos that the C64
  interpreter catches only at runtime, so legacy programs with dead
  code still compile.
* Keywords from other Commodore BASICs compile away with a warning
  instead of failing the build, so a listing that picks its graphics
  routines by machine (`IF BV=67 THEN SPRDEF ...`) compiles.

## Simons' BASIC support and credits

The extended-command support is based on TSB (Tuned Simons' BASIC) by
godot64:
https://github.com/godot64/TSB

TSB is a modern take on Simons' BASIC. It adds commands for hires
graphics, sprites, sound and structured programming (PROC, LOOP,
REPEAT and so on) on top of Commodore BASIC V2. YABCompiler tokenizes
and compiles the subset of these commands that makes sense in a
compiled program. That includes formatted `USE` (literal or string
expression), `USE` as a disk device select, `RESET` with an expression,
`RCOMP`, `HRDCPY`, `COLD` and `RETRACE`. `TRACE`, `DUMP`, `MAP` and
`OUT` cannot be reproduced: they warn at compile time and raise
`?SYNTAX ERROR` if they run.

Some runtime routines were ported from TSB so a compiled program
behaves the same way it does under the interpreter, and the compiler is
tested against real TSB programs.

Thanks to godot64 for TSB.

## Project layout

```
crates/core     parser, IR, optimisation passes, code generator
crates/cli      command line front-end
crates/gui      egui GUI
crates/emu64    small in-process C64 emulator used by the tests
test_corpus     synthetic language tests saved as .bas and .prg
test_synthetic  behavioural tests with expected output
```

## Testing

```sh
cargo test -p yabcompiler-core
```

Tests run compiled programs in the included `emu64` emulator and
compare their output with expected results. Large program tests can
take several minutes. See [BUILD.md](BUILD.md) for workspace tests.

## License

MIT. See [LICENSE](LICENSE).
