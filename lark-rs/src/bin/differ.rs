//! `differ` — a thin tree-printer the differential fuzzer's minimizer calls into.
//!
//! The differential fuzzer (`tools/fuzz_differential.py`) finds inputs where
//! lark-rs disagrees with Python Lark (the oracle), then shrinks them with
//! `--minimize`. To shrink *while preserving the divergence* (issue #37) the
//! Python minimizer must, at each step, ask lark-rs what tree it produces and
//! compare it against the Python oracle — accepting a candidate only when the two
//! engines still disagree. This binary is that "ask lark-rs" half: it parses one
//! input and prints the result as a single JSON line in the *exact* shape
//! `generate_oracles.py::tree_to_dict` emits, so the Python side can diff the two
//! trees with one structural equality check.
//!
//! It is deliberately tiny and self-contained: it only depends on the public
//! `lark_rs` API and hand-serializes JSON (lark-rs has no runtime JSON dep, and
//! adding one for a test-tool binary would be the wrong blast radius). The grammar
//! is loaded exactly as the fuzz oracle loads it — LALR + contextual lexer,
//! `start="start"`, `maybe_placeholders=false` — so the trees are comparable.
//!
//! Usage:
//!     echo -n "1 + 2" | differ --grammar arithmetic
//!     printf '%s' "$INPUT" | differ --grammar json
//!     printf '%s' "$INPUT" | differ --grammar-file /tmp/random_grammar.lark
//!
//! `--grammar <name>` loads `tests/grammars/<name>.lark` (the trusted fixtures);
//! `--grammar-file <path>` loads an arbitrary grammar file by path. The latter is
//! what the `--fuzz-grammars` mode uses: a randomly generated grammar has no
//! committed fixture to name, so the fuzzer writes it to a scratch `.lark` and
//! diffs lark-rs against the Python oracle through this same online differ.
//!
//! Output (stdout, one line):
//!     {"ok": true,  "tree": {"type": "tree", "data": "start", "children": [...]}}
//!     {"ok": false, "tree": null}
//!
//! The input is read from stdin verbatim (no trailing-newline fixup) so the Python
//! caller controls the exact bytes diffed. Exit status is 0 on a clean run
//! (parse success *or* a parse error — both are valid, reportable outcomes) and 1
//! only on a usage/grammar-load failure the caller cannot recover from.

use std::io::Read;
use std::process::ExitCode;

use lark_rs::{Child, Lark, LarkOptions, LexerType, ParseTree, ParserAlgorithm, Tree};

fn main() -> ExitCode {
    let mut grammar_name: Option<String> = None;
    let mut grammar_file: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--grammar" => grammar_name = args.next(),
            "--grammar-file" => grammar_file = args.next(),
            "-h" | "--help" => {
                eprintln!(
                    "usage: differ (--grammar <name> | --grammar-file <path>)   \
                     (reads input from stdin, prints {{\"ok\":bool,\"tree\":...}} as JSON)"
                );
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("differ: unexpected argument {other:?}");
                return ExitCode::FAILURE;
            }
        }
    }

    // Resolve the grammar source path: --grammar-file <path> wins (an arbitrary
    // file, used by --fuzz-grammars), else --grammar <name> maps to the trusted
    // fixture tests/grammars/<name>.lark (the same source the fuzz oracle,
    // `tools/fuzz_differential.py::load_parser`, reads). The two are mutually
    // exclusive; exactly one is required.
    let grammar_label;
    let grammar_path: std::path::PathBuf = match (grammar_file, grammar_name) {
        (Some(_), Some(_)) => {
            eprintln!("differ: pass only one of --grammar / --grammar-file");
            return ExitCode::FAILURE;
        }
        (Some(path), None) => {
            grammar_label = path.clone();
            std::path::PathBuf::from(path)
        }
        (None, Some(name)) => {
            grammar_label = name.clone();
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/grammars")
                .join(format!("{name}.lark"))
        }
        (None, None) => {
            eprintln!("differ: one of --grammar <name> / --grammar-file <path> is required");
            return ExitCode::FAILURE;
        }
    };
    let grammar_name = grammar_label;
    let grammar_text = match std::fs::read_to_string(&grammar_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("differ: cannot read {}: {e}", grammar_path.display());
            return ExitCode::FAILURE;
        }
    };

    // Build exactly as the fuzz oracle does: LALR + contextual, start="start",
    // maybe_placeholders=false. A build failure is a real config problem the
    // caller can't shrink around, so it is a hard error (exit 1), distinct from a
    // per-input parse error (a normal, reported outcome).
    let lark = match Lark::new(
        &grammar_text,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Contextual,
            start: vec!["start".to_string()],
            maybe_placeholders: false,
            ..Default::default()
        },
    ) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("differ: grammar {grammar_name:?} failed to build: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("differ: cannot read stdin: {e}");
        return ExitCode::FAILURE;
    }

    let mut out = String::new();
    match lark.parse(&input) {
        Ok(tree) => {
            out.push_str("{\"ok\": true, \"tree\": ");
            write_parse_tree(&mut out, &tree);
            out.push('}');
        }
        Err(_) => out.push_str("{\"ok\": false, \"tree\": null}"),
    }
    println!("{out}");
    ExitCode::SUCCESS
}

/// Serialize a [`ParseTree`] root in `tree_to_dict` shape. The root is normally a
/// `Tree`, but a `?start` expand1 collapse can yield a bare `Token`.
fn write_parse_tree(out: &mut String, tree: &ParseTree) {
    match tree {
        ParseTree::Tree(t) => write_tree(out, t),
        ParseTree::Token(tok) => write_token(out, &tok.type_, &tok.value),
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
        // maybe_placeholders=false in the fuzz config, so this should not occur;
        // mirror tree_to_dict's None serialization defensively if it ever does.
        Child::None => out.push_str("{\"type\": \"unknown\", \"repr\": \"None\"}"),
    }
}

fn write_token(out: &mut String, token_type: &str, value: &str) {
    out.push_str("{\"type\": \"token\", \"token_type\": ");
    write_json_string(out, token_type);
    out.push_str(", \"value\": ");
    write_json_string(out, value);
    out.push('}');
}

/// Minimal RFC 8259 JSON string escaper — escapes the control characters and the
/// two mandatory escapes (`"` and `\`). Emits `\uXXXX` for C0 control bytes, matching
/// what `json.dumps` accepts when the Python side parses this back.
fn write_json_string(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}
