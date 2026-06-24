//! `diffcheck` — flexible differential harness for the bug-bounty strike teams.
//!
//! Unlike `differ` (which is hard-wired to LALR + contextual on a fixture
//! grammar for the fuzz minimizer), this binary takes an arbitrary grammar file,
//! input file, and the commonly-tested public options (parser, lexer, start,
//! ambiguity, maybe_placeholders, keep_all_tokens, strict — NOT g_regex_flags,
//! base_path, import_sources, postlex, or lexer_backend), then prints the lark-rs
//! result as oracle-shaped JSON. Its Python counterpart (`tools/diffcheck.py`)
//! runs the same job through Python Lark and diffs the two — so a team can probe
//! any (grammar, input, options) tuple and get a machine-checkable verdict.
//!
//! Build failures (grammar construction) and parse failures are reported
//! distinctly so a team can tell a `GrammarError`/`build` divergence from a
//! `ParseError`/`parse` divergence — both are first-class bounty outcomes.
//!
//! Usage:
//!     diffcheck --grammar-file G.lark --input-file IN.txt \
//!         [--parser lalr|earley|cyk] [--lexer auto|basic|contextual|dynamic|dynamic_complete] \
//!         [--start NAME] [--ambiguity resolve|explicit] \
//!         [--maybe-placeholders] [--keep-all-tokens] [--strict]
//!
//! Output (stdout, one JSON line):
//!     {"stage": "build",  "ok": false, "error": "..."}             grammar rejected
//!     {"stage": "parse",  "ok": false, "error": "..."}             input rejected
//!     {"stage": "parse",  "ok": true,  "tree": {...}}              accepted

use std::io::Read;
use std::process::ExitCode;

use lark_rs::{Ambiguity, Child, Lark, LarkOptions, LexerType, ParseTree, ParserAlgorithm, Tree};

fn main() -> ExitCode {
    let mut grammar_file: Option<String> = None;
    let mut input_file: Option<String> = None;
    let mut start = "start".to_string();
    let mut parser = ParserAlgorithm::Lalr;
    let mut lexer = LexerType::Contextual;
    let mut ambiguity = Ambiguity::Resolve;
    let mut maybe_placeholders = false;
    let mut keep_all_tokens = false;
    let mut strict = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--grammar-file" => grammar_file = args.next(),
            "--input-file" => input_file = args.next(),
            "--start" => start = args.next().unwrap_or(start),
            "--parser" => {
                parser = match args.next().as_deref() {
                    Some("lalr") => ParserAlgorithm::Lalr,
                    Some("earley") => ParserAlgorithm::Earley,
                    Some("cyk") => ParserAlgorithm::Cyk,
                    other => {
                        eprintln!("diffcheck: bad --parser {other:?}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--lexer" => {
                lexer = match args.next().as_deref() {
                    Some("auto") => LexerType::Auto,
                    Some("basic") => LexerType::Basic,
                    Some("contextual") => LexerType::Contextual,
                    Some("dynamic") => LexerType::Dynamic,
                    Some("dynamic_complete") => LexerType::DynamicComplete,
                    other => {
                        eprintln!("diffcheck: bad --lexer {other:?}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--ambiguity" => {
                ambiguity = match args.next().as_deref() {
                    Some("resolve") => Ambiguity::Resolve,
                    Some("explicit") => Ambiguity::Explicit,
                    Some("forest") => Ambiguity::Forest,
                    other => {
                        eprintln!("diffcheck: bad --ambiguity {other:?}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--maybe-placeholders" => maybe_placeholders = true,
            "--keep-all-tokens" => keep_all_tokens = true,
            "--strict" => strict = true,
            "-h" | "--help" => {
                eprintln!(
                    "usage: diffcheck --grammar-file G --input-file IN [options] (see source)"
                );
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("diffcheck: unexpected argument {other:?}");
                return ExitCode::FAILURE;
            }
        }
    }

    let grammar_path = match grammar_file {
        Some(p) => p,
        None => {
            eprintln!("diffcheck: --grammar-file is required");
            return ExitCode::FAILURE;
        }
    };
    let grammar_text = match std::fs::read_to_string(&grammar_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("diffcheck: cannot read {grammar_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let input = match input_file {
        Some(p) => match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("diffcheck: cannot read {p}: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => {
            let mut s = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut s) {
                eprintln!("diffcheck: cannot read stdin: {e}");
                return ExitCode::FAILURE;
            }
            s
        }
    };

    let lark = Lark::new(
        &grammar_text,
        LarkOptions {
            parser,
            lexer,
            ambiguity,
            start: vec![start.clone()],
            maybe_placeholders,
            keep_all_tokens,
            strict,
            ..Default::default()
        },
    );
    let lark = match lark {
        Ok(l) => l,
        Err(e) => {
            let mut out = String::from("{\"stage\": \"build\", \"ok\": false, \"error\": ");
            write_json_string(&mut out, &format!("{e}"));
            out.push('}');
            println!("{out}");
            return ExitCode::SUCCESS;
        }
    };

    let mut out = String::new();
    match lark.parse_with_start(&input, &start) {
        Ok(tree) => {
            out.push_str("{\"stage\": \"parse\", \"ok\": true, \"tree\": ");
            write_parse_tree(&mut out, &tree);
            out.push('}');
        }
        Err(e) => {
            out.push_str("{\"stage\": \"parse\", \"ok\": false, \"error\": ");
            write_json_string(&mut out, &format!("{e}"));
            out.push('}');
        }
    }
    println!("{out}");
    ExitCode::SUCCESS
}

fn write_parse_tree(out: &mut String, tree: &ParseTree) {
    match tree {
        ParseTree::Tree(t) => write_tree(out, t),
        ParseTree::Token(tok) => write_token(out, &tok.type_, &tok.value),
        // Python's `tree_to_dict(None)` is JSON `null` — a bare-`None` root collapse
        // (`?start: [A]` on `""`, #289) must serialize identically for the differ.
        ParseTree::None => out.push_str("null"),
    }
}

fn write_tree(out: &mut String, tree: &Tree) {
    out.push_str("{\"type\": \"tree\", \"data\": ");
    write_json_string(out, &tree.data);
    out.push_str(", \"children\": [");
    for (i, child) in tree.children.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write_child(out, child);
    }
    out.push_str("]}");
}

fn write_child(out: &mut String, child: &Child) {
    match child {
        Child::Tree(t) => write_tree(out, t),
        Child::Token(tok) => write_token(out, &tok.type_, &tok.value),
        Child::None => out.push_str("null"),
    }
}

fn write_token(out: &mut String, token_type: &str, value: &str) {
    out.push_str("{\"type\": \"token\", \"token_type\": ");
    write_json_string(out, token_type);
    out.push_str(", \"value\": ");
    write_json_string(out, value);
    out.push('}');
}

fn write_json_string(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}
