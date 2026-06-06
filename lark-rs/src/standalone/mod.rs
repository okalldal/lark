//! Standalone parser generation (issue #42, Phase 3).
//!
//! Emits a *self-contained* Rust source file that parses a fixed grammar without
//! any dependency on lark-rs at parse time — only the `regex` crate and the Rust
//! standard library. This mirrors Python Lark's `lark.tools.standalone`, which
//! bakes a grammar into a single importable module.
//!
//! ## What this is for (and what it is not)
//!
//! The value is **dependency footprint and parity with Python's `standalone`**:
//! exporting a parser to a consumer that must not depend on lark-rs (a small crate,
//! a vendored file, a different build graph). It is deliberately *not* a throughput
//! play and *not* (yet) a `no_std`/firmware artifact:
//!
//!   * the baked parser is still **table-interpreted** (it walks the same dense
//!     ACTION/GOTO tables the in-process engine does), so it is not faster than
//!     lark-rs — hence there is no benchmark here;
//!   * the lexer **compiles its combined regex at runtime** on first use, and the
//!     shim uses `regex` + `HashMap`/`HashSet`, so the output is not `#![no_std]`.
//!
//! The issue gestures at no-std firmware/wasm as motivation; that remains future
//! work (it would need a baked DFA lexer and an alloc-only runtime). What ships
//! here is the self-contained-export use case, which is the Python-parity goal.
//!
//! ## What is baked
//!
//! The generator runs the *normal* lark-rs pipeline once at build time:
//!
//! ```text
//! .lark text → load_grammar → lower → build_lalr_table  (ParseTable)
//!                                   → basic_lexer_conf + scanner_plan (lexer)
//! ```
//!
//! and serializes the results into one `static DATA: GrammarData` (see
//! [`runtime::GrammarData`]): the sparse LALR ACTION/GOTO tables, every rule's
//! tree-shaping flags, the symbol-name table, and the
//! [`ScannerPlan`](crate::lexer::ScannerPlan) (alternation order + each terminal's
//! inline regex + the `unless` keyword-retype map + `%ignore` + global flags).
//!
//! ## How drift is prevented
//!
//! Two pieces could drift from the in-process engine; both are *shared by
//! construction* rather than re-derived:
//!
//!   * the **lexer recipe** comes from the same [`scanner_plan`](crate::lexer::scanner_plan)
//!     `Scanner::build` uses, so the baked scanner is byte-identical; and
//!   * the **driver** (lexer loop + LALR reduce/shape) lives in [`runtime`], a real
//!     compiled, type-checked, unit-tested module — `include_str!`d into the
//!     generated file, never hand-copied as text.
//!
//! A generated parser therefore produces byte-identical trees to lark-rs, pinned
//! by `tests/test_standalone.rs` (committed fixtures compiled + run vs the live
//! oracle, plus a determinism/freshness gate).
//!
//! ## Limitations (documented parity gaps)
//!
//!   * **LALR only** — the baked artifact is a `ParseTable`; Earley/CYK are not
//!     supported (the generator returns an error).
//!   * **Basic lexer only** — the standalone lexer is the combined-regex basic
//!     lexer, not the contextual lexer. Grammars that *require* the contextual
//!     lexer to resolve terminal collisions are rejected by Python Lark's
//!     standalone tool too; here they will simply fail to lex at runtime.
//!   * **No postlex** — `%declare` + an `Indenter` postlex hook is not baked
//!     (the generator returns an error if one is configured).

// Compiled + type-checked here so the embedded driver cannot rot, then `include_str!`d
// into every generated parser. `dead_code` is expected: nothing in the lib's normal
// build path calls it (the round-trip fixtures and the unit test below do).
#[allow(dead_code)]
pub mod runtime;

use std::fmt::Write as _;

use crate::error::{GrammarError, LarkError};
use crate::grammar::load_grammar_with_base;
use crate::lexer::scanner_plan;
use crate::parsers::basic_lexer_conf;
use crate::parsers::lalr::{build_lalr_table, Action};
use crate::{LarkOptions, ParserAlgorithm};

/// The shared runtime driver, embedded verbatim into each generated parser.
const RUNTIME_SRC: &str = include_str!("runtime.rs");

/// Generate self-contained Rust source for a standalone parser of `grammar_src`.
///
/// The returned string is a complete `.rs` file: write it next to a crate that
/// depends on `regex` and call the generated `Parser::new().parse(text)`.
///
/// Errors if the grammar fails to load/build, or if the requested configuration
/// is not supported by the standalone backend (non-LALR parser, or a postlex
/// hook — see the module docs).
pub fn generate(grammar_src: &str, options: &LarkOptions) -> Result<String, LarkError> {
    if options.parser != ParserAlgorithm::Lalr {
        return Err(LarkError::Grammar(GrammarError::Other {
            msg: "standalone generation supports only parser='lalr'".to_string(),
        }));
    }
    if options.postlex.is_some() {
        return Err(LarkError::Grammar(GrammarError::Other {
            msg: "standalone generation does not support a postlex (Indenter) hook".to_string(),
        }));
    }

    let grammar = load_grammar_with_base(
        grammar_src,
        &options.start,
        options.maybe_placeholders,
        options.keep_all_tokens,
        options.base_path.clone(),
    )?;
    let cg = crate::grammar::lower(&grammar);
    let table = build_lalr_table(&cg, options.strict)?;
    let lexer_conf = basic_lexer_conf(&cg, options.g_regex_flags);

    // Reuse the in-process scanner recipe so the baked lexer is byte-identical.
    let term_refs: Vec<_> = lexer_conf
        .terminals
        .iter()
        .map(|(id, t)| (*id, t))
        .collect();
    let plan = scanner_plan(&term_refs, lexer_conf.global_flags)?;

    let mut out = String::new();
    emit_header(&mut out, grammar_src);
    // The shared driver (its leading `//!` module-doc block stripped — the generated
    // file has its own header, and an inner doc comment mid-module would not compile).
    out.push_str(runtime_body());
    out.push('\n');
    emit_data(&mut out, &table, &plan, &lexer_conf.ignore, options);
    emit_glue(&mut out);
    // Close the wrapping `mod parser` opened by the header and re-export its public
    // surface, so the file works both compiled directly (crate root) and `include!`d
    // into another module.
    out.push_str(
        "}\n\n#[allow(unused_imports)]\npub use parser::{Child, ParseTree, Parser, Token, Tree};\n",
    );
    Ok(out)
}

/// [`RUNTIME_SRC`] with its leading `//!` doc block (and the blank lines around it)
/// removed, so it can be pasted after the generated header and the baked data.
fn runtime_body() -> &'static str {
    let mut offset = 0;
    for line in RUNTIME_SRC.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//!") || trimmed.is_empty() {
            offset += line.len() + 1; // +1 for the '\n'
        } else {
            break;
        }
    }
    &RUNTIME_SRC[offset..]
}

/// A Rust string literal for `s` (`{:?}` produces valid, fully-escaped source).
fn lit(s: &str) -> String {
    format!("{s:?}")
}

fn emit_header(out: &mut String, grammar_src: &str) {
    out.push_str(
        "// @generated by `lark-rs generate-parser` — DO NOT EDIT.\n\
         //\n\
         // A self-contained LALR parser. Depends only on the `regex` crate and the\n\
         // Rust standard library — not on lark-rs. Drop it into any crate that has\n\
         // `regex` as a dependency and call `Parser::new().parse(text)`.\n\
         //\n\
         // Source grammar:\n",
    );
    for line in grammar_src.lines() {
        out.push_str("//   ");
        out.push_str(line);
        out.push('\n');
    }
    // Everything lives in an inner module carrying an *outer* `#[allow]` — an
    // inner `#![allow]` would be rejected when the file is `include!`d into another
    // module (macro-expanded inner attributes are not permitted there).
    out.push_str("\n#[allow(dead_code, unused_parens, clippy::all)]\npub mod parser {\n");
}

fn emit_data(
    out: &mut String,
    table: &crate::parsers::lalr::ParseTable,
    plan: &crate::lexer::ScannerPlan,
    ignore: &[crate::grammar::SymbolId],
    options: &LarkOptions,
) {
    out.push_str("\n// ── baked grammar tables ──\nstatic DATA: GrammarData = GrammarData {\n");
    let _ = writeln!(out, "    n_terminals: {},", table.n_terminals);

    // Symbol names, indexed by id.
    out.push_str("    symbol_names: &[\n");
    for i in 0..table.symbols.len() {
        let name = table.symbols.name(crate::grammar::SymbolId(i as u32));
        let _ = writeln!(out, "        {},", lit(name));
    }
    out.push_str("    ],\n");

    // Rules.
    out.push_str("    rules: &[\n");
    for r in &table.rules {
        let filter: String = r
            .filter_pos
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "        RuleData {{ origin: {}, len: {}, tree_name: {}, transparent: {}, expand1: {}, has_alias: {}, keep_all: {}, filter_pos: &[{}], placeholder_count: {}, is_start: {} }},",
            r.origin.0,
            r.expansion.len(),
            lit(&r.tree_name),
            r.transparent,
            r.options.expand1,
            r.alias.is_some(),
            r.options.keep_all_tokens,
            filter,
            r.options.placeholder_count,
            r.is_start,
        );
    }
    out.push_str("    ],\n");

    // ACTION table — one sparse row per state, terminals ascending.
    out.push_str("    action: &[\n");
    for row in &table.action {
        out.push_str("        &[");
        let mut first = true;
        for (term, cell) in row.iter().enumerate() {
            let Some(action) = cell else { continue };
            if !first {
                out.push_str(", ");
            }
            first = false;
            let a = match action {
                Action::Shift(s) => format!("Action::Shift({s})"),
                Action::Reduce(r) => format!("Action::Reduce({r})"),
                Action::Accept => "Action::Accept".to_string(),
            };
            let _ = write!(out, "({term}, {a})");
        }
        out.push_str("],\n");
    }
    out.push_str("    ],\n");

    // GOTO table — sparse (nonterminal index, next state) per state.
    out.push_str("    goto: &[\n");
    for row in &table.goto {
        out.push_str("        &[");
        let mut first = true;
        for (nt, cell) in row.iter().enumerate() {
            let Some(next) = cell else { continue };
            if !first {
                out.push_str(", ");
            }
            first = false;
            let _ = write!(out, "({nt}, {next})");
        }
        out.push_str("],\n");
    }
    out.push_str("    ],\n");

    // Start states (name → state), sorted by name for deterministic output.
    let mut starts: Vec<(String, usize)> = table
        .start_states
        .iter()
        .map(|(id, st)| (table.symbols.name(*id).to_string(), *st))
        .collect();
    starts.sort();
    out.push_str("    start_states: &[");
    for (i, (name, st)) in starts.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "({}, {})", lit(name), st);
    }
    out.push_str("],\n");
    let default_start = options
        .start
        .first()
        .cloned()
        .unwrap_or_else(|| "start".to_string());
    let _ = writeln!(out, "    start_default: {},", lit(&default_start));

    // Lexer: global prefix, scanner alternation, unless map, ignore set.
    let _ = writeln!(out, "    global_prefix: {},", lit(&plan.global_prefix));

    out.push_str("    scan_groups: &[\n");
    for (id, rx) in &plan.groups {
        let _ = writeln!(out, "        ({}, {}),", id.0, lit(rx));
    }
    out.push_str("    ],\n");

    // unless: sorted by regex id, inner by matched value, for determinism.
    let mut unless: Vec<(u32, Vec<(String, u32)>)> = plan
        .unless
        .iter()
        .map(|(re_id, m)| {
            let mut entries: Vec<(String, u32)> =
                m.iter().map(|(v, kw)| (v.clone(), kw.0)).collect();
            entries.sort();
            (re_id.0, entries)
        })
        .collect();
    unless.sort();
    out.push_str("    unless: &[\n");
    for (re_id, entries) in &unless {
        let _ = write!(out, "        ({re_id}, &[");
        let mut first = true;
        for (v, kw) in entries {
            if !first {
                out.push_str(", ");
            }
            first = false;
            let _ = write!(out, "({}, {})", lit(v), kw);
        }
        out.push_str("]),\n");
    }
    out.push_str("    ],\n");

    let mut ig: Vec<u32> = ignore.iter().map(|s| s.0).collect();
    ig.sort_unstable();
    let ig_list: String = ig
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(out, "    ignore: &[{ig_list}],");

    out.push_str("};\n");
}

/// The per-file glue tying the baked `DATA` to the shared runtime's `Parser`.
fn emit_glue(out: &mut String) {
    out.push_str(
        "\nimpl Parser {\n    /// Construct the parser for this baked grammar.\n    pub fn new() -> Parser {\n        Parser::from_data(&DATA)\n    }\n}\n\nimpl Default for Parser {\n    fn default() -> Self {\n        Parser::new()\n    }\n}\n",
    );
}

#[cfg(test)]
mod tests {
    use super::runtime::{Action, GrammarData, ParseTree, Parser, RuleData};

    // A hand-built `GrammarData` for the trivial grammar `start: "a"`, so the shared
    // runtime is exercised directly as Rust (independently of the code generator and
    // the round-trip fixtures). Layout: terminals $END(0), A(1); non-terminals
    // $root_start(2), start(3). State 0 shifts A→1; state 1 reduces `start: A` (rule
    // 0); after GOTO to the start, $END accepts.
    static TRIVIAL: GrammarData = GrammarData {
        n_terminals: 2,
        symbol_names: &["$END", "A", "$root_start", "start"],
        rules: &[
            RuleData {
                origin: 3,
                len: 1,
                tree_name: "start",
                transparent: false,
                expand1: false,
                has_alias: false,
                keep_all: false,
                filter_pos: &[true], // a literal "a" is filtered out of the tree
                placeholder_count: 0,
                is_start: false,
            },
            RuleData {
                origin: 2,
                len: 1,
                tree_name: "$root_start",
                transparent: false,
                expand1: false,
                has_alias: false,
                keep_all: false,
                filter_pos: &[false],
                placeholder_count: 0,
                is_start: true,
            },
        ],
        // state 0: shift A → state 1, then GOTO start → state 2.
        // state 1: reduce rule 0 (`start: A`) on $END.
        // state 2: accept on $END.
        action: &[
            &[(1, Action::Shift(1))],
            &[(0, Action::Reduce(0))],
            &[(0, Action::Accept)],
        ],
        goto: &[&[(1, 2)], &[], &[]],
        start_states: &[("start", 0)],
        start_default: "start",
        global_prefix: "",
        scan_groups: &[(1, "a")],
        unless: &[],
        ignore: &[],
    };

    #[test]
    fn runtime_parses_with_hand_built_data() {
        let parser = Parser::from_data(&TRIVIAL);
        let tree = parser.parse("a").expect("parses");
        // The "a" literal is filtered, leaving an empty `start` node.
        assert!(matches!(&tree, ParseTree::Tree(t) if t.data == "start" && t.children.is_empty()));
        assert_eq!(tree.to_string(), "Tree(start, [])");
    }

    #[test]
    fn runtime_reports_errors() {
        let parser = Parser::from_data(&TRIVIAL);
        assert!(parser.parse("b").is_err(), "unexpected character");
        assert!(parser.parse("aa").is_err(), "trailing input");
        assert!(parser.parse("").is_err(), "empty input");
    }
}
