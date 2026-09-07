/// URL for compiler error reports.
pub const ISSUES_URL: &str = "https://github.com/tommyo123/YABCompiler/issues";

pub mod analysis;
pub mod ast;
pub mod cfg;
pub mod codegen;
pub mod compile;
pub mod config;
pub mod dataflow;
pub mod expr_type;
pub mod extraram;
pub mod for_next_runtime;
pub mod image_reach;
pub mod ir;
pub mod opt_model;
pub mod pack;
mod packed_print;
pub mod parse;
pub mod passes;
pub mod peephole;
pub mod petscii;
mod compact_data_bad;
pub mod prg;
pub mod rem_hints;
pub mod runtime;
pub mod source;
pub mod sys_params;
pub mod tokens;
pub mod visit;

pub use compile::{
    BasicHintDialect, CompileError, CompileOptions, Compiled, Diagnostics, ExtraRamDecision,
    Profile, ReservedOvershoot, compile_with_options, lower_and_optimize, parse_reserved_ranges,
    parse_start_address, reserved_tail_overshoots, verify_addressing_modes_for_test,
};
pub use prg::Program;
pub use source::{
    TokenizeError, is_basic_source_path, tokenize_program, tokenize_program_at_with,
    tokenize_program_with,
};
pub use tokens::Dialect;
