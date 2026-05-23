//! GUI-side wrapper around `yabcompiler-core`.
//!
//! Keeps every call into the compiler in one place, takes plain
//! values + paths, and returns owned strings/bytes. The UI layer
//! never touches `compile_with_options` or `Program::parse` directly,
//! so swapping the front-end (or running the same actions from a
//! background task) only touches this module.

use std::path::Path;

use yabcompiler_core::{
    CompileOptions, Diagnostics, Program, compile_with_options, is_basic_source_path,
    parse_reserved_ranges, parse_start_address, tokenize_program,
};

pub use yabcompiler_core::Profile;

/// Successful compile result, ready for the UI to surface and write
/// to disk. `prg_bytes` is the full SYS-stub-wrapped program; `asm`
/// is the textual assembly used to produce it. `diagnostics` carries
/// the layout / extraram / reserved-range summary that the status
/// panel renders.
pub struct BuildArtifact {
    pub asm: String,
    pub machine_code_len: usize,
    pub prg_bytes: Vec<u8>,
    pub diagnostics: Diagnostics,
}

/// Build the inputs for a compile run. Validates mutual exclusion
/// and reserved-range parsing up front so the UI can show errors
/// without round-tripping through the compiler.
pub struct BuildRequest<'a> {
    pub input_path: &'a Path,
    pub profile: Profile,
    pub extraram: bool,
    pub force_extraram_off: bool,
    pub auto_reserve: bool,
    pub lenient_syntax: bool,
    pub safe_sys_calls: bool,
    pub reserved_text: &'a str,
    /// `Some(text)` enables the custom-start path; the text is parsed
    /// via [`yabcompiler_core::parse_start_address`]. `None` keeps the
    /// default $0801 + SYS launcher.
    pub custom_start_text: Option<&'a str>,
}

impl BuildRequest<'_> {
    pub fn validate(&self) -> Result<(), String> {
        if self.extraram && self.force_extraram_off {
            return Err(
                "extraram and force-extraram-off are mutually exclusive — uncheck one".to_string(),
            );
        }
        let trimmed = self.reserved_text.trim();
        if !trimmed.is_empty() {
            parse_reserved_ranges(trimmed).map_err(|e| format!("reserved ranges: {e}"))?;
        }
        if let Some(text) = self.custom_start_text {
            parse_start_address(text.trim()).map_err(|e| format!("start address: {e}"))?;
        }
        Ok(())
    }

    fn options(&self) -> Result<CompileOptions, String> {
        let mut options = CompileOptions {
            profile: self.profile,
            extraram: self.extraram,
            force_extraram_off: self.force_extraram_off,
            auto_reserve: self.auto_reserve,
            lenient_syntax: self.lenient_syntax,
            safe_sys_calls: self.safe_sys_calls,
            ..CompileOptions::default()
        };
        let trimmed = self.reserved_text.trim();
        if !trimmed.is_empty() {
            options.reserved_ranges =
                parse_reserved_ranges(trimmed).map_err(|e| format!("reserved ranges: {e}"))?;
        }
        if let Some(text) = self.custom_start_text {
            options.custom_start_address =
                Some(parse_start_address(text.trim()).map_err(|e| format!("start address: {e}"))?);
        }
        Ok(options)
    }
}

pub fn read_input(path: &Path) -> Result<Vec<u8>, String> {
    if path.as_os_str().is_empty() {
        return Err("input path is empty".to_string());
    }
    std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))
}

/// Read a `.bas` source file or `.prg` tokenized file at `path` and
/// return tokenized .prg bytes. `.bas` inputs go through the
/// in-process tokenizer (`yabcompiler_core::tokenize_program`); the
/// rest is returned unchanged. Used by every UI action that needs a
/// .prg shape (list, dump, build) so each action accepts either
/// extension transparently.
pub fn load_prg_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = read_input(path)?;
    if !is_basic_source_path(path) {
        return Ok(bytes);
    }
    let source = String::from_utf8(bytes)
        .map_err(|e| format!("{} is not valid UTF-8 source: {e}", path.display()))?;
    tokenize_program(&source).map_err(|e| format!("tokenize {}: {e}", path.display()))
}

pub fn list_program(path: &Path) -> Result<String, String> {
    let bytes = load_prg_bytes(path)?;
    let program = Program::parse(&bytes).map_err(|e| format!("prg parse: {e}"))?;
    Ok(program.detokenize())
}

pub fn dump_program(path: &Path) -> Result<String, String> {
    let bytes = load_prg_bytes(path)?;
    let program = Program::parse(&bytes).map_err(|e| format!("prg parse: {e}"))?;
    Ok(program.dump())
}

pub fn build(request: &BuildRequest<'_>) -> Result<BuildArtifact, BuildError> {
    request.validate().map_err(|m| BuildError::short(m))?;
    let bytes = load_prg_bytes(request.input_path).map_err(BuildError::short)?;
    let options = request.options().map_err(BuildError::short)?;
    let compiled = compile_with_options(&bytes, options).map_err(|e| {
        let asm = e.generated_asm().map(str::to_owned);
        BuildError {
            message: e.to_string(),
            asm,
        }
    })?;
    Ok(BuildArtifact {
        asm: compiled.asm,
        machine_code_len: compiled.machine_code.len(),
        prg_bytes: compiled.prg_bytes,
        diagnostics: compiled.diagnostics,
    })
}

/// Build-time error split into the short message (shown in status)
/// and the optional generated assembly (offered as a separate panel
/// or "Save asm to file" prompt). Replaces the prior `String` so the
/// UI can route the two pieces independently without parsing the
/// error text back apart.
#[derive(Debug)]
pub struct BuildError {
    pub message: String,
    pub asm: Option<String>,
}

impl BuildError {
    fn short(message: String) -> Self {
        Self { message, asm: None }
    }
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Derive a default output path from an input by suffixing the stem.
/// `foo.prg` → `foo.compiled.prg` (avoid colliding with the input);
/// `foo.bas` → `foo.prg` (the source and the .prg can coexist
/// without a `.compiled` infix). Falls back to `out.prg` next to
/// the input when no usable stem exists.
pub fn default_output_for(input: &Path) -> std::path::PathBuf {
    let parent = input.parent().unwrap_or_else(|| Path::new(""));
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    if is_basic_source_path(input) {
        parent.join(format!("{stem}.prg"))
    } else {
        parent.join(format!("{stem}.compiled.prg"))
    }
}

pub fn default_asm_for(input: &Path) -> std::path::PathBuf {
    let parent = input.parent().unwrap_or_else(|| Path::new(""));
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    parent.join(format!("{stem}.s"))
}

/// Derive a default export path for the requested extension. Sits
/// next to the input so the user only has to confirm the file name
/// in the save dialog. `ext` is the bare extension (`"bas"` /
/// `"prg"`) without the leading dot.
pub fn default_export_path(input: &Path, ext: &str) -> std::path::PathBuf {
    let parent = input.parent().unwrap_or_else(|| Path::new(""));
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    parent.join(format!("{stem}.{ext}"))
}

/// Read `input` (auto-detecting .bas vs .prg) and write a
/// detokenised BASIC source file to `target`. The detokeniser
/// handles both the BASIC v2 keyword set and the `$64 <id>`
/// extended tokens — running it on either input produces the same
/// canonical text. For a `.bas` input the source goes through the
/// tokenizer first to canonicalise (case-fold, normalize PETSCII
/// escapes) before being detokenised back, so the output reads
/// like the LIST a real C64 would print.
pub fn export_as_bas(input: &Path, target: &Path) -> Result<usize, String> {
    let bytes = load_prg_bytes(input)?;
    let program = Program::parse(&bytes).map_err(|e| format!("prg parse: {e}"))?;
    let text = program.detokenize();
    std::fs::write(target, text.as_bytes())
        .map_err(|e| format!("write {}: {e}", target.display()))?;
    Ok(text.len())
}

/// Read `input` (auto-detecting .bas vs .prg) and write a
/// tokenised .prg file to `target`. For `.prg` input we still go
/// through `Program::parse` so a malformed file fails loudly rather
/// than being silently re-saved.
pub fn export_as_prg(input: &Path, target: &Path) -> Result<usize, String> {
    let bytes = load_prg_bytes(input)?;
    Program::parse(&bytes).map_err(|e| format!("prg parse: {e}"))?;
    std::fs::write(target, &bytes).map_err(|e| format!("write {}: {e}", target.display()))?;
    Ok(bytes.len())
}
