# Building YABCompiler

## From source

Rust 1.85 or newer.

```sh
cargo build --release -p yabcompiler-cli
cargo build --release -p yabcompiler-gui
```

The CLI binary lands at `target/release/yabcompiler`, the GUI at
`target/release/yabcompiler-gui`.

On Linux you also need the system libraries `eframe` links against
(X11 or Wayland, OpenGL, fontconfig) plus GTK for the file dialogs. On
Debian or Ubuntu:

```sh
sudo apt-get install build-essential pkg-config libgtk-3-dev \
  libx11-dev libxcursor-dev libxrandr-dev libxi-dev \
  libxkbcommon-dev libwayland-dev libgl1-mesa-dev
```

## Tests

```sh
cargo test -p yabcompiler-core
```

The corpus tests compile each program and run the result through the
bundled `emu64` emulator, then compare the captured output against a
golden file. emu64 runs in process, so the full suite finishes in
seconds. Add `--workspace` to also build and test the GUI (needs the
Linux libraries listed above).

## Release packaging

Tagging a commit `vX.Y.Z` and pushing the tag runs the GitHub Actions
workflow in `.github/workflows/release.yml`, which builds the Windows
MSI and portable ZIP plus Linux and macOS tarballs and attaches them to
a GitHub release. You can also trigger it by hand from the Actions tab.

### Windows MSI, locally

Needs [`cargo-wix`](https://crates.io/crates/cargo-wix)
(`cargo install cargo-wix`) and the
[WiX Toolset 3.x](https://wixtoolset.org/). The installer that ships
both binaries is described by `wix/main.wxs`.

The simplest path is the script, which builds both binaries and
packages them in one step:

```powershell
.\scripts\build-msi.ps1
```

The MSI lands in `target\wix\` as `yabcompiler-<version>-x86_64.msi`.

Under the hood the script runs two commands. The MSI bundles the GUI
and the CLI, but the GUI is not a default workspace member, so a plain
`cargo build` does not produce it. Build both, then package:

```powershell
cargo build --release -p yabcompiler-gui -p yabcompiler-cli
cargo msi
```

`cargo msi` is an alias (in `.cargo/config.toml`) for:

```powershell
cargo wix -p yabcompiler-gui -n yabcompiler `
  --no-build --target-bin-dir target\release `
  -I wix\main.wxs -o target\wix\
```

The `-p` is required because this is a workspace (plain `cargo wix`
errors with "please pass a package name"), and `--no-build` is what
lets the MSI pick up both pre-built binaries instead of only the GUI
crate. `cargo-wix` finds WiX through the `WIX` environment variable the
WiX installer sets; pass `-b "<path>\WiX Toolset v3.x\bin"` if it
cannot.

Without `cargo-wix` you can call `candle` and `light` directly. They
are not on `PATH`, so use the WiX `bin` folder:

```powershell
cargo build --release -p yabcompiler-gui -p yabcompiler-cli

$wix = "C:\Program Files (x86)\WiX Toolset v3.14\bin"
& "$wix\candle.exe" -nologo -ext WixUIExtension -ext WixUtilExtension `
  -dCargoTargetBinDir=target\release -dVersion=0.9.3.0 wix\main.wxs
& "$wix\light.exe" -nologo -ext WixUIExtension -ext WixUtilExtension -sval `
  -out yabcompiler-0.9.3-x86_64-windows.msi main.wixobj
```

`-dVersion` takes a four-part `MAJOR.MINOR.PATCH.0` number. Either way
the installer puts both binaries in `Program Files\YABCompiler` and
adds Desktop and Start Menu shortcuts for the GUI. The GitHub Actions
workflow uses the `candle`/`light` route so it needs no extra install.
