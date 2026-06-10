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
//!   * **No lookaround grammars — the DFA-plan bakeability payoff is not realized yet.**
//!     The baked `ScannerPlan` is a *regex* alternation (each terminal's inline pattern
//!     compiled on the `regex` crate at runtime), so a grammar with **lookaround**
//!     terminals (the bundled `python`/`lark`) is **not** standalone-able: the regex
//!     runtime cannot host `(?!…)`/`(?<…)`, and this generator does not bake the
//!     `regex-automata` DFA scanner bundle or its guard side-tables. Closing this is
//!     **L5** of the lexer DFA plan (serialize the plain + guarded DFAs, guard/lookbehind
//!     tables, rank maps, start-byte prefilter, `unless`, and `%ignore`, and replace the
//!     `ScannerPlan` path with it). L4 (drop runtime `fancy-regex`) has landed, so L5
//!     is unblocked. See `docs/LEXER_DFA_PLAN.md` (L5) and
//!     `docs/LEXER_DFA_STATUS.md`.

// Compiled + type-checked here so the embedded driver cannot rot, then `include_str!`d
// into every generated parser. `dead_code` is expected: nothing in the lib's normal
// build path calls it (the round-trip fixtures and the unit test below do).
#[allow(dead_code)]
pub mod runtime;

use std::fmt::Write as _;

use crate::error::{GrammarError, LarkError};
use crate::grammar::load_grammar_with_base;
use crate::lexer::{check_regex_collisions, check_zero_width_terminals, scanner_plan};
use crate::parsers::basic_lexer_conf;
use crate::parsers::lalr::{build_lalr_table, Action};
use crate::{LarkOptions, ParserAlgorithm};

/// A rule's baked tree-shaping metadata, owned (the lifetime-free mirror of
/// [`runtime::RuleData`]).
struct BakedRule {
    origin: u32,
    len: u32,
    tree_name: String,
    transparent: bool,
    expand1: bool,
    has_alias: bool,
    keep_all: bool,
    filter_pos: Vec<bool>,
    placeholder_count: u32,
    nones_before: Vec<u32>,
    is_start: bool,
}

/// The fully-baked, owned grammar tables — the single source both the code
/// emitter ([`emit_data`]) and the in-process oracle runner (`leak_grammar_data`
/// in tests) read from, so the bytes a generated parser ships are the bytes the
/// compliance oracle actually exercises. The lifetime-free mirror of
/// [`runtime::GrammarData`].
struct Baked {
    n_terminals: u32,
    symbol_names: Vec<String>,
    rules: Vec<BakedRule>,
    /// Sparse `(terminal id, action)` per state.
    action: Vec<Vec<(u32, Action)>>,
    /// Sparse `(nonterminal index, next state)` per state.
    goto: Vec<Vec<(u32, u32)>>,
    start_states: Vec<(String, u32)>,
    start_default: String,
    global_prefix: String,
    scan_groups: Vec<(u32, String)>,
    unless: Vec<(u32, Vec<(String, u32)>)>,
    ignore: Vec<u32>,
}

/// Run the normal pipeline and collect everything a standalone parser needs into
/// an owned [`Baked`]. Applies the same basic-lexer build-time validation
/// `build_frontend`'s LALR/basic path does — zero-width terminals are rejected
/// always, and (under `strict`) same-priority regex-terminal collisions — so the
/// standalone backend accepts exactly what the in-process basic lexer would.
fn bake(grammar_src: &str, options: &LarkOptions) -> Result<Baked, LarkError> {
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

    // Mirror build_frontend's LALR/basic sanitization (the standalone lexer is the
    // basic lexer, so it must reject what that lexer would).
    check_zero_width_terminals(&lexer_conf)?;
    check_regex_collisions(&lexer_conf, options.strict, None)?;

    // Reuse the in-process scanner recipe so the baked lexer is byte-identical.
    let term_refs: Vec<_> = lexer_conf
        .terminals
        .iter()
        .map(|(id, t)| (*id, t))
        .collect();
    let plan = scanner_plan(&term_refs, lexer_conf.global_flags)?;

    let symbol_names = (0..table.symbols.len())
        .map(|i| {
            table
                .symbols
                .name(crate::grammar::SymbolId(i as u32))
                .to_string()
        })
        .collect();

    let rules = table
        .rules
        .iter()
        .map(|r| BakedRule {
            origin: r.origin.0,
            len: r.expansion.len() as u32,
            tree_name: r.tree_name.clone(),
            transparent: r.transparent,
            expand1: r.options.expand1,
            has_alias: r.alias.is_some(),
            keep_all: r.options.keep_all_tokens,
            filter_pos: r.filter_pos.clone(),
            placeholder_count: r.options.placeholder_count as u32,
            nones_before: r.options.nones_before.iter().map(|&n| n as u32).collect(),
            is_start: r.is_start,
        })
        .collect();

    let action = table
        .action
        .iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .filter_map(|(t, cell)| cell.map(|a| (t as u32, a)))
                .collect()
        })
        .collect();

    let goto = table
        .goto
        .iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .filter_map(|(nt, cell)| cell.map(|s| (nt as u32, s)))
                .collect()
        })
        .collect();

    let mut start_states: Vec<(String, u32)> = table
        .start_states
        .iter()
        .map(|(id, st)| (table.symbols.name(*id).to_string(), *st as u32))
        .collect();
    start_states.sort();

    let start_default = options
        .start
        .first()
        .cloned()
        .unwrap_or_else(|| "start".to_string());

    let scan_groups = plan
        .groups
        .iter()
        .map(|(id, rx)| (id.0, rx.clone()))
        .collect();

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

    let mut ignore: Vec<u32> = lexer_conf.ignore.iter().map(|s| s.0).collect();
    ignore.sort_unstable();

    Ok(Baked {
        n_terminals: table.n_terminals as u32,
        symbol_names,
        rules,
        action,
        goto,
        start_states,
        start_default,
        global_prefix: plan.global_prefix.clone(),
        scan_groups,
        unless,
        ignore,
    })
}

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
    let baked = bake(grammar_src, options)?;

    let mut out = String::new();
    emit_header(&mut out, grammar_src);
    // The shared driver (its leading `//!` module-doc block stripped — the generated
    // file has its own header, and an inner doc comment mid-module would not compile).
    out.push_str(runtime_body());
    out.push('\n');
    emit_data(&mut out, &baked);
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

fn emit_data(out: &mut String, baked: &Baked) {
    out.push_str("\n// ── baked grammar tables ──\nstatic DATA: GrammarData = GrammarData {\n");
    let _ = writeln!(out, "    n_terminals: {},", baked.n_terminals);

    // Symbol names, indexed by id.
    out.push_str("    symbol_names: &[\n");
    for name in &baked.symbol_names {
        let _ = writeln!(out, "        {},", lit(name));
    }
    out.push_str("    ],\n");

    // Rules.
    out.push_str("    rules: &[\n");
    for r in &baked.rules {
        let filter: String = r
            .filter_pos
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let nones: String = r
            .nones_before
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "        RuleData {{ origin: {}, len: {}, tree_name: {}, transparent: {}, expand1: {}, has_alias: {}, keep_all: {}, filter_pos: &[{}], placeholder_count: {}, nones_before: &[{}], is_start: {} }},",
            r.origin,
            r.len,
            lit(&r.tree_name),
            r.transparent,
            r.expand1,
            r.has_alias,
            r.keep_all,
            filter,
            r.placeholder_count,
            nones,
            r.is_start,
        );
    }
    out.push_str("    ],\n");

    // ACTION table — one sparse row per state, terminals ascending.
    out.push_str("    action: &[\n");
    for row in &baked.action {
        out.push_str("        &[");
        for (i, (term, action)) in row.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
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
    for row in &baked.goto {
        out.push_str("        &[");
        for (i, (nt, next)) in row.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "({nt}, {next})");
        }
        out.push_str("],\n");
    }
    out.push_str("    ],\n");

    // Start states (name → state), already sorted by name in `bake`.
    out.push_str("    start_states: &[");
    for (i, (name, st)) in baked.start_states.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "({}, {})", lit(name), st);
    }
    out.push_str("],\n");
    let _ = writeln!(out, "    start_default: {},", lit(&baked.start_default));

    // Lexer: global prefix, scanner alternation, unless map, ignore set.
    let _ = writeln!(out, "    global_prefix: {},", lit(&baked.global_prefix));

    out.push_str("    scan_groups: &[\n");
    for (id, rx) in &baked.scan_groups {
        let _ = writeln!(out, "        ({}, {}),", id, lit(rx));
    }
    out.push_str("    ],\n");

    // unless: already sorted by regex id (inner by matched value) in `bake`.
    out.push_str("    unless: &[\n");
    for (re_id, entries) in &baked.unless {
        let _ = write!(out, "        ({re_id}, &[");
        for (i, (v, kw)) in entries.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "({}, {})", lit(v), kw);
        }
        out.push_str("]),\n");
    }
    out.push_str("    ],\n");

    let ig_list: String = baked
        .ignore
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
                nones_before: &[],
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
                nones_before: &[],
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

    // ─── Standalone compliance bank (#86) ─────────────────────────────────────
    //
    // `runtime.rs` is a parallel re-expression of the in-process LALR reduce /
    // tree-shaping driver. To widen the drift net beyond the two round-trip
    // fixtures (json, arithmetic), replay the strip-mined Python-Lark compliance
    // bank through the standalone runtime and compare to the *same captured oracle
    // trees* `test_compliance.rs` uses — the same XFAIL-burndown discipline as the
    // LALR (`xfail.json`) and Earley (`earley_xfail.json`) banks.
    //
    // The runtime is exercised over the bank's grammars by baking each (the shared
    // `bake`, the very data a generated parser would ship) and leaking it to
    // `'static` so `runtime::Parser` can run it — no per-grammar codegen/compile.
    // Standalone is LALR + basic-lexer only, so contextual-lexer / strict-collision
    // grammars naturally diverge and are allow-listed, to be burned down under #86.

    use super::{bake, Baked, LarkOptions, ParserAlgorithm};
    use serde_json::Value;
    use std::collections::BTreeSet;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::path::PathBuf;

    fn compliance_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oracles/compliance")
    }

    fn load_json(name: &str) -> Option<Value> {
        let text = std::fs::read_to_string(compliance_dir().join(name)).ok()?;
        Some(serde_json::from_str(&text).expect("valid JSON"))
    }

    fn string_set(name: &str) -> BTreeSet<String> {
        load_json(name)
            .and_then(|v| v.as_array().cloned())
            .map(|a| {
                a.into_iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn record_options(rec: &Value) -> LarkOptions {
        use crate::grammar::terminal::flags;
        let start = match &rec["start"] {
            Value::String(s) => vec![s.clone()],
            Value::Array(a) => a
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            _ => vec!["start".to_string()],
        };
        let mut g_regex_flags = 0u32;
        if let Some(letters) = rec["g_regex_flags"].as_str() {
            for ch in letters.chars() {
                g_regex_flags |= match ch {
                    'i' => flags::IGNORECASE,
                    'm' => flags::MULTILINE,
                    's' => flags::DOTALL,
                    'x' => flags::VERBOSE,
                    _ => 0,
                };
            }
        }
        LarkOptions {
            start,
            parser: ParserAlgorithm::Lalr,
            maybe_placeholders: rec["maybe_placeholders"].as_bool().unwrap_or(true),
            keep_all_tokens: rec["keep_all_tokens"].as_bool().unwrap_or(false),
            strict: rec["strict"].as_bool().unwrap_or(false),
            g_regex_flags,
            ..Default::default()
        }
    }

    /// Leak a [`Baked`] to `'static` so the runtime (which takes `&'static
    /// GrammarData`, matching a generated parser's baked `static`) can run it.
    /// Test-only: the leak is bounded by the bank size and freed at process exit.
    fn leak_grammar_data(b: &Baked) -> &'static GrammarData {
        fn leak_str(s: &str) -> &'static str {
            Box::leak(s.to_string().into_boxed_str())
        }

        let symbol_names: Vec<&'static str> = b.symbol_names.iter().map(|s| leak_str(s)).collect();
        let rules: Vec<RuleData> = b
            .rules
            .iter()
            .map(|r| RuleData {
                origin: r.origin,
                len: r.len,
                tree_name: leak_str(&r.tree_name),
                transparent: r.transparent,
                expand1: r.expand1,
                has_alias: r.has_alias,
                keep_all: r.keep_all,
                filter_pos: &*Box::leak(r.filter_pos.clone().into_boxed_slice()),
                placeholder_count: r.placeholder_count,
                nones_before: &*Box::leak(r.nones_before.clone().into_boxed_slice()),
                is_start: r.is_start,
            })
            .collect();
        let action: Vec<&'static [(u32, Action)]> = b
            .action
            .iter()
            .map(|row| {
                let row: Vec<(u32, Action)> = row
                    .iter()
                    .map(|(t, a)| {
                        let a = match a {
                            super::Action::Shift(s) => Action::Shift(*s as u32),
                            super::Action::Reduce(r) => Action::Reduce(*r as u32),
                            super::Action::Accept => Action::Accept,
                        };
                        (*t, a)
                    })
                    .collect();
                &*Box::leak(row.into_boxed_slice())
            })
            .collect();
        let goto: Vec<&'static [(u32, u32)]> = b
            .goto
            .iter()
            .map(|row| &*Box::leak(row.clone().into_boxed_slice()))
            .collect();
        let start_states: Vec<(&'static str, u32)> = b
            .start_states
            .iter()
            .map(|(n, s)| (leak_str(n), *s))
            .collect();
        let scan_groups: Vec<(u32, &'static str)> = b
            .scan_groups
            .iter()
            .map(|(id, rx)| (*id, leak_str(rx)))
            .collect();
        let unless: Vec<(u32, &'static [(&'static str, u32)])> = b
            .unless
            .iter()
            .map(|(id, entries)| {
                let entries: Vec<(&'static str, u32)> =
                    entries.iter().map(|(v, kw)| (leak_str(v), *kw)).collect();
                (*id, &*Box::leak(entries.into_boxed_slice()))
            })
            .collect();

        Box::leak(Box::new(GrammarData {
            n_terminals: b.n_terminals,
            symbol_names: Box::leak(symbol_names.into_boxed_slice()),
            rules: Box::leak(rules.into_boxed_slice()),
            action: Box::leak(action.into_boxed_slice()),
            goto: Box::leak(goto.into_boxed_slice()),
            start_states: Box::leak(start_states.into_boxed_slice()),
            start_default: leak_str(&b.start_default),
            global_prefix: leak_str(&b.global_prefix),
            scan_groups: Box::leak(scan_groups.into_boxed_slice()),
            unless: Box::leak(unless.into_boxed_slice()),
            ignore: Box::leak(b.ignore.clone().into_boxed_slice()),
        }))
    }

    // ── runtime-tree vs oracle-JSON comparison (mirrors common::tree_matches_oracle,
    //    but over the runtime's own tree types). No `_ambig` — standalone is LALR.
    fn rt_matches(result: &ParseTree, oracle: &Value) -> bool {
        match result {
            ParseTree::Tree(t) => oracle["type"].as_str() == Some("tree") && rt_tree(t, oracle),
            ParseTree::Token(tok) => {
                oracle["type"].as_str() == Some("token") && rt_token(tok, oracle)
            }
        }
    }
    fn rt_tree(t: &super::runtime::Tree, oracle: &Value) -> bool {
        if t.data != oracle["data"].as_str().unwrap_or("?") {
            return false;
        }
        let children = oracle["children"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        t.children.len() == children.len()
            && t.children
                .iter()
                .zip(children)
                .all(|(c, oc)| rt_child(c, oc))
    }
    fn rt_child(c: &super::runtime::Child, oracle: &Value) -> bool {
        match c {
            super::runtime::Child::Tree(st) => {
                oracle["type"].as_str() == Some("tree") && rt_tree(st, oracle)
            }
            super::runtime::Child::Token(tok) => {
                oracle["type"].as_str() == Some("token") && rt_token(tok, oracle)
            }
            super::runtime::Child::None => oracle["type"].as_str() == Some("unknown"),
        }
    }
    fn rt_token(tok: &super::runtime::Token, oracle: &Value) -> bool {
        tok.type_ == oracle["token_type"].as_str().unwrap_or("?")
            && tok.value == oracle["value"].as_str().unwrap_or("?")
    }

    /// Build a standalone parser for `grammar` and parse `input`, catching panics
    /// (a handful of bank grammars abort deep in table/regex construction).
    fn try_standalone(grammar: &str, opts: &LarkOptions, input: &str) -> Option<ParseTree> {
        catch_unwind(AssertUnwindSafe(|| {
            let baked = bake(grammar, opts).ok()?;
            let data = leak_grammar_data(&baked);
            Parser::from_data(data).parse(input).ok()
        }))
        .ok()
        .flatten()
    }

    fn can_bake(grammar: &str, opts: &LarkOptions) -> bool {
        catch_unwind(AssertUnwindSafe(|| bake(grammar, opts).is_ok())).unwrap_or(false)
    }

    #[test]
    fn standalone_compliance_bank() {
        let Some(records) = load_json("bank.json") else {
            eprintln!("compliance bank.json not found — run tools/extract_lark_compliance.py");
            return;
        };
        let records = records.as_array().expect("bank is an array");
        let skip = string_set("skip.json");

        std::panic::set_hook(Box::new(|_| {}));

        let mut failures: BTreeSet<String> = BTreeSet::new();
        let mut total = 0usize;

        for (ri, rec) in records.iter().enumerate() {
            let grammar = rec["grammar"].as_str().unwrap_or("");
            if skip.contains(grammar) {
                continue;
            }
            let opts = record_options(rec);

            // construct-error parity: Python raised at construction; the standalone
            // backend must refuse to bake too.
            if rec["construct_error"].as_bool().unwrap_or(false) {
                total += 1;
                if can_bake(grammar, &opts) {
                    failures.insert(format!("construct:{ri}"));
                }
                continue;
            }

            let cases = rec["cases"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            if cases.is_empty() {
                continue;
            }
            let buildable = can_bake(grammar, &opts);

            for (ci, case) in cases.iter().enumerate() {
                total += 1;
                let input = case["input"].as_str().unwrap_or("");
                let should_parse = case["should_parse"].as_bool().unwrap_or(false);
                let parsed = if buildable {
                    try_standalone(grammar, &opts, input)
                } else {
                    None
                };
                let agree = match (should_parse, &parsed) {
                    (true, Some(tree)) => rt_matches(tree, &case["tree"]),
                    (true, None) => false,
                    (false, None) => true,
                    (false, Some(_)) => false,
                };
                if !agree {
                    failures.insert(format!("parse:{ri}:{ci}"));
                }
            }
        }

        let xfail = string_set("standalone_xfail.json");
        let passing = total - failures.len();
        let pct = if total == 0 {
            100.0
        } else {
            100.0 * passing as f64 / total as f64
        };
        eprintln!(
            "standalone compliance: {passing}/{total} agree with oracle ({pct:.1}%); \
             {} known-XFAIL",
            xfail.len()
        );

        if std::env::var("LARK_STANDALONE_WRITE_XFAIL").is_ok() {
            let list: Vec<&String> = failures.iter().collect();
            let path = compliance_dir().join("standalone_xfail.json");
            std::fs::write(&path, serde_json::to_string_pretty(&list).unwrap() + "\n")
                .expect("write standalone_xfail.json");
            eprintln!(
                "wrote {} XFAIL entries to {}",
                failures.len(),
                path.display()
            );
            return;
        }

        let regressions: Vec<&String> = failures.difference(&xfail).collect();
        let fixed: Vec<&String> = xfail.difference(&failures).collect();
        if !fixed.is_empty() {
            eprintln!(
                "note: {} standalone XFAIL entries now pass — consider regenerating \
                 standalone_xfail.json",
                fixed.len()
            );
        }
        assert!(
            regressions.is_empty(),
            "standalone compliance regressions ({} newly failing, not in standalone_xfail.json):\n{}",
            regressions.len(),
            regressions
                .iter()
                .take(40)
                .map(|s| format!("  - {s}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
