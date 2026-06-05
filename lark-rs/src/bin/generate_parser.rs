//! `lark-rs generate-parser` — emit a self-contained standalone parser (issue #42).
//!
//! ```text
//! generate_parser --grammar foo.lark [--output parser.rs] [--start start ...]
//!                 [--keep-all-tokens] [--maybe-placeholders] [--strict]
//! ```
//!
//! Reads a `.lark` grammar, bakes its LALR table + basic-lexer config into Rust
//! `const` data, and writes a self-contained `parser.rs` (see
//! [`lark_rs::standalone`]). With no `--output`, the source is written to stdout.

use std::path::PathBuf;
use std::process::ExitCode;

use lark_rs::{generate_standalone, LarkOptions};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("generate-parser: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut grammar_path: Option<PathBuf> = None;
    let mut output_path: Option<PathBuf> = None;
    let mut starts: Vec<String> = Vec::new();
    let mut keep_all_tokens = false;
    let mut maybe_placeholders = false;
    let mut strict = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--grammar" | "-g" => {
                grammar_path = Some(PathBuf::from(next(&args, &mut i, arg)?));
            }
            "--output" | "-o" => {
                output_path = Some(PathBuf::from(next(&args, &mut i, arg)?));
            }
            "--start" | "-s" => {
                starts.push(next(&args, &mut i, arg)?);
            }
            "--keep-all-tokens" => keep_all_tokens = true,
            "--maybe-placeholders" => maybe_placeholders = true,
            "--strict" => strict = true,
            "--help" | "-h" => {
                print_usage();
                return Ok(());
            }
            other => return Err(format!("unknown argument {other:?} (try --help)")),
        }
        i += 1;
    }

    let grammar_path = grammar_path.ok_or("missing required --grammar <path>")?;
    let grammar_src = std::fs::read_to_string(&grammar_path)
        .map_err(|e| format!("reading {}: {e}", grammar_path.display()))?;

    if starts.is_empty() {
        starts.push("start".to_string());
    }

    // Relative `%import .module` resolves against the grammar file's directory.
    let base_path = grammar_path.parent().map(|p| p.to_path_buf());

    let options = LarkOptions {
        start: starts,
        keep_all_tokens,
        maybe_placeholders,
        strict,
        base_path,
        ..Default::default()
    };

    let source = generate_standalone(&grammar_src, &options).map_err(|e| e.to_string())?;

    match output_path {
        Some(path) => {
            std::fs::write(&path, source).map_err(|e| format!("writing {}: {e}", path.display()))?
        }
        None => print!("{source}"),
    }
    Ok(())
}

fn next(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn print_usage() {
    eprintln!(
        "Usage: generate_parser --grammar <path> [options]\n\
         \n\
         Emit a self-contained Rust LALR parser for a .lark grammar.\n\
         \n\
         Options:\n\
         \x20 -g, --grammar <path>     grammar file to compile (required)\n\
         \x20 -o, --output <path>      write to <path> (default: stdout)\n\
         \x20 -s, --start <name>       start symbol (repeatable; default: start)\n\
         \x20     --keep-all-tokens    keep punctuation tokens in the tree\n\
         \x20     --maybe-placeholders emit None for absent [...] groups\n\
         \x20     --strict             reject shift/reduce + regex collisions\n\
         \x20 -h, --help               show this help"
    );
}
