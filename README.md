# YABCompiler

Yet Another Basic Compiler. Compiles Commodore BASIC V2 programs to
native 6502 machine code that runs on a stock C64. It also compiles
the subset of Simons' BASIC (Tuned Simons' BASIC dialect) commands that make
sense in a compiled program (see below).

Input can be either a tokenized `.prg` (as saved by an emulator or a
real 1541) or plain `.bas` source. Output is a runnable `.prg`.
Programs that worked under the interpreter usually work compiled, only
faster.

## Why

The C64 BASIC interpreter is slow. Most type-in programs from the 80s
spend more time in the interpreter loop than doing useful work. A
compiler that takes the same source and emits real 6502 code lifts
games and utilities from "barely playable" to "fast enough to enjoy"
without rewriting them.

YABCompiler keeps the source untouched. You compile the `.prg`, copy
the result to a disk image, and run it on real hardware or in an
emulator.

## Download

Prebuilt packages are on the
[Releases page](https://github.com/tommyo123/YABCompiler/releases).

* Windows MSI installer. Installs the GUI and CLI with Start Menu and
  Desktop shortcuts.
* Windows portable ZIP. Unpack and run, no install.
* Linux and macOS tarballs with both binaries.

To build from source instead, see [BUILD.md](BUILD.md).

## Build

Rust 1.85 or newer:

```sh
cargo build --release -p yabcompiler-cli
cargo build --release -p yabcompiler-gui
```

The CLI binary lands at `target/release/yabcompiler`, the GUI at
`target/release/yabcompiler-gui`. Building the Windows installer and
the Linux library requirements are covered in [BUILD.md](BUILD.md).

## Usage

CLI:

```sh
yabcompiler compile mygame.bas mygame.prg
```

Run `yabcompiler` with no arguments for the full list of commands and
options.

GUI: launch `yabcompiler-gui`, pick a `.bas` or `.prg` as the input,
press Compile. The menu bar holds the same actions plus theme and
about. The settings panel is on the left, the output and generated
assembly on the right.

## Features

* BASIC V2 compatible, plus a useful subset of Simons' BASIC and Tuned
  Simons' BASIC commands (see below).
* Type inference promotes integer-only variables to byte or word
  storage where it is safe to do so.
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
compiled program. Some runtime routines were ported from TSB so a
compiled program behaves the same way it does under the interpreter,
and the compiler is tested against real TSB programs.

Thanks to godot64 for TSB.

## Project layout

```
crates/core     parser, IR, optimisation passes, code generator
crates/cli      command line front-end
crates/gui      egui GUI
crates/emu64    small in-process C64 emulator used by the tests
test_corpus     regression programs saved as .bas and .prg
test_synthetic  behavioural tests with expected output
```

## Testing

```sh
cargo test -p yabcompiler-core
```

The corpus tests compile each program and run the result through the
bundled `emu64` emulator, then compare the captured output against a
golden file. emu64 runs in process, so the full suite finishes in
seconds. See [BUILD.md](BUILD.md) for running the full workspace tests.

## License

MIT. See [LICENSE](LICENSE).
