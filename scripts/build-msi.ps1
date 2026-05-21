#!/usr/bin/env pwsh
# Build the Windows MSI installer locally, end to end.
#
# The MSI bundles two binaries from two crates (the GUI and the CLI),
# and the GUI is not a default workspace member, so a plain
# `cargo build` does not produce it. This script builds both, then
# packages them with the `cargo msi` alias (see .cargo/config.toml).
#
# Needs cargo-wix (`cargo install cargo-wix`) and the WiX Toolset 3.x.
# The MSI lands in target\wix\ as yabcompiler-<version>-x86_64.msi.
#
# Any extra arguments are forwarded to `cargo msi` (e.g. -b "<wix bin>").

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    cargo build --release -p yabcompiler-gui -p yabcompiler-cli
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    cargo msi @args
    if ($LASTEXITCODE -ne 0) { throw "cargo msi failed" }
    Get-ChildItem target\wix\*.msi | Select-Object Name, Length
}
finally {
    Pop-Location
}
