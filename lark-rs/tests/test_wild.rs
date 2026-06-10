//! Wild-grammar bank: replays real-world Lark grammars strip-mined from open
//! source projects (`tests/wild/<project>/` — HCL2/Terraform, MapServer
//! mapfiles, GraphQL SDL, PEP 508, MistQL, Synapse Storm, Vyper, Quil) against
//! oracles frozen from Python Lark by `tools/generate_wild_oracles.py`.
//!
//! Each project's `meta.json` records the upstream repo, commit pin, license,
//! and the exact Lark options the project itself uses; the inputs are verbatim
//! files/strings from the same upstream. This is the "wild" complement to the
//! compliance bank: where that bank covers Lark's *own* test grammars, this one
//! covers what users actually write.
//!
//! Big trees are compared by digest (node/token counts + FNV-1a 64 over the
//! canonical serialization defined in generate_wild_oracles.py); small trees
//! are additionally compared structurally for readable diagnostics.
//!
//! Same XFAIL discipline as the compliance bank: known failures live in
//! `tests/fixtures/oracles/wild/xfail.json`, the build fails only on
//! *regressions*, and `LARK_WILD_WRITE_XFAIL=1` regenerates the allow-list.

mod common;

use common::tree_matches_oracle;
use lark_rs::tree::{Child, ParseTree, Tree};
use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

fn wild_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/wild")
}

fn oracles_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oracles/wild")
}

fn load_json(path: &PathBuf) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(serde_json::from_str(&text).expect("valid JSON"))
}

// ── Canonical serialization + FNV-1a 64 (mirrors generate_wild_oracles.py) ──

const UNIT_SEP: char = '\x1f';

fn canon_child(out: &mut String, child: &Child) {
    match child {
        Child::Tree(t) => canon_tree(out, t),
        Child::Token(tok) => {
            write!(
                out,
                "T{}{UNIT_SEP}{}{UNIT_SEP}{}",
                tok.type_,
                tok.value.len(),
                tok.value
            )
            .unwrap();
        }
        Child::None => out.push('_'),
    }
}

fn canon_tree(out: &mut String, tree: &Tree) {
    write!(
        out,
        "N{}{UNIT_SEP}{}{UNIT_SEP}[",
        tree.data,
        tree.children.len()
    )
    .unwrap();
    for c in &tree.children {
        canon_child(out, c);
    }
    out.push(']');
}

fn canon(result: &ParseTree) -> String {
    let mut out = String::new();
    match result {
        ParseTree::Tree(t) => canon_tree(&mut out, t),
        ParseTree::Token(tok) => write!(
            out,
            "T{}{UNIT_SEP}{}{UNIT_SEP}{}",
            tok.type_,
            tok.value.len(),
            tok.value
        )
        .unwrap(),
    }
    out
}

fn fnv1a64(data: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

// ── meta.json → LarkOptions ─────────────────────────────────────────────────

fn meta_options(meta: &Value, project_dir: &PathBuf) -> Option<LarkOptions> {
    let opts = &meta["lark_options"];
    let parser = match opts["parser"].as_str()? {
        "lalr" => ParserAlgorithm::Lalr,
        "earley" => ParserAlgorithm::Earley,
        "cyk" => ParserAlgorithm::Cyk,
        _ => return None,
    };
    let lexer = match opts["lexer"].as_str()? {
        "basic" => LexerType::Basic,
        "contextual" => LexerType::Contextual,
        "dynamic" => LexerType::Dynamic,
        "dynamic_complete" => LexerType::DynamicComplete,
        _ => return None,
    };
    let postlex = match opts["postlex"].as_str() {
        // Python Lark's PythonIndenter == lark-rs's Indenter defaults.
        Some("PythonIndenter") => Some(lark_rs::postlex::Indenter::default()),
        Some(_) => return None,
        None => None,
    };
    // Canonical `imsx` letters, like the compliance bank records them.
    let mut g_regex_flags = 0u32;
    if let Some(letters) = opts["g_regex_flags"].as_str() {
        use lark_rs::grammar::terminal::flags;
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
    Some(LarkOptions {
        g_regex_flags,
        start: vec![opts["start"].as_str()?.to_string()],
        parser,
        lexer,
        maybe_placeholders: opts["maybe_placeholders"].as_bool().unwrap_or(true),
        keep_all_tokens: opts["keep_all_tokens"].as_bool().unwrap_or(false),
        propagate_positions: opts["propagate_positions"].as_bool().unwrap_or(false),
        postlex,
        // Relative %import (poetry_pep508 -> .markers) resolves against the
        // vendored grammar directory, as upstream's Lark.open does.
        base_path: Some(project_dir.join("grammar")),
        ..Default::default()
    })
}

fn try_build(grammar: &str, opts: LarkOptions) -> Result<Lark, String> {
    match catch_unwind(AssertUnwindSafe(|| Lark::new(grammar, opts))) {
        Ok(Ok(lark)) => Ok(lark),
        Ok(Err(e)) => Err(format!("{e}")),
        Err(_) => Err("panic during build".to_string()),
    }
}

fn try_parse(lark: &Lark, input: &str) -> Option<ParseTree> {
    match catch_unwind(AssertUnwindSafe(|| lark.parse(input))) {
        Ok(Ok(tree)) => Some(tree),
        _ => None,
    }
}

/// Compare a parse result against one oracle case. `Ok(())` on agreement.
fn case_matches(parsed: &Option<ParseTree>, case: &Value) -> Result<(), String> {
    let oracle_ok = case["ok"].as_bool().unwrap_or(false);
    match (oracle_ok, parsed) {
        (false, None) => Ok(()),
        (false, Some(_)) => Err("parsed but oracle expects a parse error".into()),
        (true, None) => Err("parse error but oracle expects a tree".into()),
        (true, Some(tree)) => {
            // The embedded tree (when present) gives the readable diagnostic;
            // the digest is the authoritative check either way.
            if case.get("tree").is_some_and(|t| !t.is_null()) {
                tree_matches_oracle(tree, &case["tree"])?;
            }
            let c = canon(tree);
            let canon_len = case["canon_len"].as_u64().unwrap_or(0) as usize;
            let digest = case["fnv1a64"].as_str().unwrap_or("");
            if c.len() != canon_len {
                return Err(format!(
                    "canonical length {} != oracle {canon_len}",
                    c.len()
                ));
            }
            let h = fnv1a64(c.as_bytes());
            if h != digest {
                return Err(format!("canonical digest {h} != oracle {digest}"));
            }
            Ok(())
        }
    }
}

#[test]
fn test_wild_bank() {
    let mut projects: Vec<PathBuf> = std::fs::read_dir(wild_dir())
        .expect("tests/wild exists")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("meta.json").is_file())
        .collect();
    projects.sort();
    assert!(!projects.is_empty(), "wild bank is empty");

    // Silence panic backtraces from expected-to-fail grammars/inputs
    // (LARK_WILD_TRACE=1 keeps them visible for debugging).
    let trace = std::env::var("LARK_WILD_TRACE").is_ok();
    if !trace {
        std::panic::set_hook(Box::new(|_| {}));
    }

    let mut failures: BTreeSet<String> = BTreeSet::new();
    let mut details: Vec<String> = Vec::new();
    let mut total = 0usize;

    for pdir in &projects {
        let project_t0 = std::time::Instant::now();
        let meta = load_json(&pdir.join("meta.json")).expect("meta.json parses");
        let name = meta["name"].as_str().expect("meta has name");
        let oracle = load_json(&oracles_dir().join(format!("{name}.json"))).unwrap_or_else(|| {
            panic!("missing oracle for {name} — run tools/generate_wild_oracles.py")
        });
        let cases = oracle["cases"].as_array().expect("oracle has cases");
        total += cases.len();

        let grammar_path = pdir.join(meta["entry_grammar"].as_str().expect("entry_grammar"));
        let grammar = std::fs::read_to_string(&grammar_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", grammar_path.display()));

        let Some(opts) = meta_options(&meta, pdir) else {
            panic!("{name}: meta.json lark_options not representable in LarkOptions");
        };

        let lark = match try_build(&grammar, opts) {
            Ok(lark) => lark,
            Err(e) => {
                failures.insert(format!("build:{name}"));
                details.push(format!("build:{name}: {e}"));
                for case in cases {
                    let f = case["input_file"].as_str().unwrap_or("?");
                    failures.insert(format!("parse:{name}:{f}"));
                }
                continue;
            }
        };

        for case in cases {
            let input_rel = case["input_file"].as_str().expect("case has input_file");
            let input = std::fs::read_to_string(pdir.join(input_rel))
                .unwrap_or_else(|e| panic!("read {input_rel}: {e}"));
            let parsed = try_parse(&lark, &input);
            if let Err(e) = case_matches(&parsed, case) {
                failures.insert(format!("parse:{name}:{input_rel}"));
                details.push(format!("parse:{name}:{input_rel}: {e}"));
            }
        }
        if trace {
            eprintln!("  {name}: {:.2}s", project_t0.elapsed().as_secs_f64());
        }
    }

    let xfail: BTreeSet<String> = load_json(&oracles_dir().join("xfail.json"))
        .and_then(|v| v.as_array().cloned())
        .map(|a| {
            a.into_iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let n_build_fail = failures.iter().filter(|f| f.starts_with("build:")).count();
    eprintln!(
        "wild bank: {}/{total} inputs agree with oracle across {} projects \
         ({n_build_fail} grammars not building); {} known-XFAIL",
        total - failures.iter().filter(|f| f.starts_with("parse:")).count(),
        projects.len(),
        xfail.len()
    );

    if std::env::var("LARK_WILD_WRITE_XFAIL").is_ok() {
        let list: Vec<&String> = failures.iter().collect();
        let path = oracles_dir().join("xfail.json");
        std::fs::write(&path, serde_json::to_string_pretty(&list).unwrap() + "\n")
            .expect("write xfail.json");
        eprintln!("wrote {} XFAIL entries to {}", list.len(), path.display());
        return;
    }

    let regressions: Vec<&String> = failures.difference(&xfail).collect();
    let fixed: Vec<&String> = xfail.difference(&failures).collect();
    if !fixed.is_empty() {
        eprintln!(
            "note: {} XFAIL entries now pass — consider regenerating xfail.json \
             (LARK_WILD_WRITE_XFAIL=1)",
            fixed.len()
        );
    }
    if !regressions.is_empty() {
        let detail_for = |key: &str| {
            details
                .iter()
                .find(|d| d.starts_with(key))
                .cloned()
                .unwrap_or_else(|| key.to_string())
        };
        let report = format!(
            "wild-bank regressions ({} newly failing, not in xfail.json):\n{}",
            regressions.len(),
            regressions
                .iter()
                .take(40)
                .map(|s| format!("  - {}", detail_for(s)))
                .collect::<Vec<_>>()
                .join("\n")
        );
        // The silencing panic hook installed above would swallow the assert
        // message, so print the report explicitly and restore the default hook.
        eprintln!("{report}");
        let _ = std::panic::take_hook();
        panic!("wild-bank regressions — see report above");
    }
}
