# Changelog

All notable changes to YABCompiler are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.9.0] - 2026-05-21

First public release.

### Added

- Compiler from Commodore BASIC V2 to native 6502 machine code. Input
  is plain `.bas` source or a tokenized `.prg`; output is a runnable
  `.prg`.
- Support for a useful subset of TSB Neo, based on TSB by godot64.
  Some runtime routines were ported from TSB. See the README for
  details.
- Type inference that promotes integer-only variables to byte or word
  storage where safe.
- Extra RAM mode that banks BASIC ROM out to use `$A000-$BFFF` for
  code or data, decided automatically or forced.
- Auto-reserve that scans POKE/PEEK targets and keeps the assembler
  off those addresses.
- Custom start address for cartridge and expansion builds.
- Lenient parser mode that tolerates runtime-only BASIC typos in dead
  code.
- Command line front-end (`yabcompiler`) with `compile`, `list`,
  `dump` and `tokenize`.
- egui GUI (`yabcompiler-gui`) with a menu bar, theme selection that
  persists across runs, and an about dialog.
- `emu64`, a small in-process C64 emulator that runs compiled programs
  against the real BASIC and KERNAL ROM, used by the test suite.
- GitHub Actions release workflow that builds a Windows MSI installer,
  a portable ZIP, and Linux and macOS tarballs.

[0.9.0]: https://github.com/tommyo123/YABCompiler/releases/tag/v0.9.0
