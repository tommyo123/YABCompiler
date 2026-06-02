//! Central application configuration, shared by every front-end (CLI,
//! GUI) and any tooling. This is the single source of truth for the
//! version and product naming. Bump [`VERSION`] here and it propagates
//! to the About box, the CLI banner and the installer metadata.

/// Application version. The one place the version is defined; keep the
/// workspace `Cargo.toml` `version` in sync so packaged artifacts match.
pub const VERSION: &str = "0.9.6";

/// Short program name.
pub const APP_NAME: &str = "YABCompiler";

/// Full window/title-bar name (name + tagline).
pub const APP_TITLE: &str = "YABCompiler · Yet Another Basic Compiler";

/// One-line tagline.
pub const TAGLINE: &str = "Yet Another Basic Compiler";

/// Author, shown in the About box.
pub const AUTHOR: &str = "Tommy Olsen";

/// Project home page.
pub const HOMEPAGE: &str = "https://github.com/tommyo123/YABCompiler";

/// One-line product description.
pub const DESCRIPTION: &str =
    "Compiles Commodore BASIC V2 (plus a Simons' BASIC subset) to native 6502 machine code.";
