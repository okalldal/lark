/// Shared test utilities: oracle loading, tree comparison, parser helpers.

/// L2 bounded-lookaround lowering harness infrastructure (generators, the
/// `fancy-regex` oracle, the mutation framework). See `tests/common/lowering.rs`.
pub mod lowering;

use lark_rs::{
    basic_lexer_conf, load_grammar, lower, Ambiguity, BasicLexer, Child, EarleyParser, Lark,
    LarkError, LarkOptions, Lexer, LexerType, ParseTree, ParserAlgorithm, Token, Tree,
};

/// Build a LALR + contextual-lexer parser for the given grammar text.
pub fn make_lalr(grammar_text: &str) -> Lark {
    Lark::new(
        grammar_text,
        LarkOptions {
            parser: ParserAlgorithm::Lalr,
            lexer: LexerType::Contextual,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("Grammar failed to load: {e}"))
}

/// Build an Earley + basic-lexer parser for the given grammar text and ambiguity
/// mode. Returns a `Result`: until the Phase-2 engine lands, building an Earley
/// parser fails with a "not yet implemented" error, which the Earley oracle tests
/// detect to gate themselves (see [`earley_unimplemented`]). Earley uses the basic
/// lexer — the contextual lexer narrows terminals by LALR state, which Earley has
/// none of.
pub fn make_earley(grammar_text: &str, ambiguity: Ambiguity) -> Result<Lark, LarkError> {
    Lark::new(
        grammar_text,
        LarkOptions {
            parser: ParserAlgorithm::Earley,
            lexer: LexerType::Basic,
            ambiguity,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
}

/// Build an Earley parser with the **dynamic lexer** (Sprint 5). `lexer` is
/// `"dynamic"` or `"dynamic_complete"`; `ambiguity` selects resolve / explicit.
/// Mirrors how Python Lark is built for the dynamic-lexer oracles and bank.
pub fn make_earley_dynamic(
    grammar_text: &str,
    lexer: &str,
    ambiguity: Ambiguity,
) -> Result<Lark, LarkError> {
    let lexer = match lexer {
        "dynamic_complete" => LexerType::DynamicComplete,
        _ => LexerType::Dynamic,
    };
    Lark::new(
        grammar_text,
        LarkOptions {
            parser: ParserAlgorithm::Earley,
            lexer,
            ambiguity,
            start: vec!["start".to_string()],
            ..Default::default()
        },
    )
}

/// True while the Earley backend is still a stub (build returns "not yet
/// implemented"). The Earley oracle/compliance tests probe this once and skip
/// themselves while it holds, so Sprint 0 lands green with the harness in place;
/// the moment Sprint 1 wires up a real Earley frontend, the probe flips and the
/// same tests start enforcing the oracles. Mirrors the self-activating pattern
/// the fuzz corpus uses.
pub fn earley_unimplemented() -> bool {
    match make_earley("start: \"a\"", Ambiguity::Resolve) {
        Err(LarkError::Grammar(e)) => format!("{e}").contains("not yet implemented"),
        _ => false,
    }
}

/// Build an Earley recognizer + basic lexer for the given grammar text.
///
/// Sprint 1 verifies the recognizer (boolean accept/reject) directly, since the
/// tree-producing Earley frontend is Sprint 2 — so this bypasses `Lark`/the
/// frontend and drives [`EarleyParser`] over the basic-lexer token stream.
pub fn make_earley_recognizer(grammar_text: &str) -> (EarleyParser, BasicLexer) {
    let grammar = load_grammar(grammar_text, &["start".to_string()], false, false)
        .unwrap_or_else(|e| panic!("Grammar failed to load: {e}"));
    let cg = lower(&grammar);
    let conf = basic_lexer_conf(&cg, 0);
    let lexer = BasicLexer::new(&conf).unwrap_or_else(|e| panic!("Lexer failed to build: {e}"));
    let parser = EarleyParser::new(cg);
    (parser, lexer)
}

/// Build an Earley recognizer + basic lexer from a grammar file under
/// tests/grammars/.
pub fn make_earley_recognizer_from_file(name: &str) -> (EarleyParser, BasicLexer) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/grammars")
        .join(format!("{name}.lark"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Cannot read {}: {e}", path.display()));
    make_earley_recognizer(&text)
}

/// Lex `input` with the basic lexer and ask the recognizer whether the grammar
/// derives it. A lexer failure (no valid token) counts as a reject, matching how
/// Python Lark reports an un-lexable input as a parse failure.
pub fn earley_accepts(parser: &EarleyParser, lexer: &BasicLexer, input: &str) -> bool {
    match lexer.lex(input) {
        Ok(tokens) => parser.recognize(&tokens, Some("start")),
        Err(_) => false,
    }
}

/// Build an Earley + basic-lexer parser (ambiguity='resolve') from a grammar file
/// under tests/grammars/. Used to verify Earley produces the *same* tree as LALR
/// on unambiguous grammars (Phase 2, Sprint 2 exit criterion).
pub fn make_earley_from_file(name: &str) -> Lark {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/grammars")
        .join(format!("{name}.lark"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Cannot read {}: {e}", path.display()));
    make_earley(&text, Ambiguity::Resolve)
        .unwrap_or_else(|e| panic!("Earley grammar failed to load: {e}"))
}

/// Build a LALR + contextual-lexer parser using a grammar file under tests/grammars/.
pub fn make_lalr_from_file(name: &str) -> Lark {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/grammars")
        .join(format!("{name}.lark"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Cannot read {}: {e}", path.display()));
    make_lalr(&text)
}

/// Load a JSON oracle file from tests/fixtures/oracles/<suite>/<name>.json.
pub fn load_oracle(suite: &str, name: &str) -> serde_json::Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/oracles")
        .join(suite)
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Cannot read oracle {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("Oracle JSON parse error: {e}"))
}

/// Compare a parse result against the oracle JSON node produced by
/// generate_oracles.py.
///
/// The oracle root is normally a `tree`, but a `?start` rule that collapses via
/// expand1 to a single token gives a bare `token` root — and lark-rs's
/// [`ParseTree`] now mirrors that, so both shapes are compared here uniformly.
///
/// Returns `Ok(())` on match, `Err(String)` describing the first mismatch.
pub fn tree_matches_oracle(result: &ParseTree, oracle: &serde_json::Value) -> Result<(), String> {
    let node_type = oracle["type"].as_str().unwrap_or("?");
    match (result, node_type) {
        (ParseTree::Tree(tree), "tree") => match_node_tree(tree, oracle),
        (ParseTree::Token(tok), "token") => match_token(tok, oracle),
        (ParseTree::Tree(tree), other) => Err(format!(
            "root is Tree('{}') but oracle node type is '{other}'",
            tree.data
        )),
        (ParseTree::Token(tok), other) => Err(format!(
            "root is Token({}) but oracle node type is '{other}'",
            tok.type_
        )),
    }
}

/// Compare a leaf `Token` against an oracle `token` node (type + value).
fn match_token(tok: &Token, oracle: &serde_json::Value) -> Result<(), String> {
    let expected_type = oracle["token_type"].as_str().unwrap_or("?");
    let expected_value = oracle["value"].as_str().unwrap_or("?");
    if tok.type_ != expected_type {
        return Err(format!("token type '{}' != '{expected_type}'", tok.type_));
    }
    if tok.value != expected_value {
        return Err(format!("token value {:?} != {expected_value:?}", tok.value));
    }
    Ok(())
}

fn match_node_tree(tree: &Tree, oracle: &serde_json::Value) -> Result<(), String> {
    let expected_data = oracle["data"].as_str().unwrap_or("?");
    if tree.data != expected_data {
        return Err(format!(
            "Tree rule mismatch: got '{}', expected '{expected_data}'",
            tree.data
        ));
    }

    let oracle_children = oracle["children"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or(&[]);
    if tree.children.len() != oracle_children.len() {
        return Err(format!(
            "In '{}': got {} children, expected {}",
            tree.data,
            tree.children.len(),
            oracle_children.len()
        ));
    }

    // `_ambig` is the ambiguity-forest node (parser='earley', ambiguity='explicit'):
    // its children are the alternative derivations, and Lark does NOT guarantee
    // their order. Compare them as an unordered set — each oracle alternative must
    // match exactly one Rust alternative, bijectively.
    if expected_data == "_ambig" {
        return match_ambig(&tree.children, oracle_children);
    }

    for (i, (child, oc)) in tree.children.iter().zip(oracle_children.iter()).enumerate() {
        match_child(child, oc).map_err(|e| format!("In '{}' child {i}: {e}", tree.data))?;
    }
    Ok(())
}

/// Compare one `Child` against one oracle node (tree / token / `None` placeholder).
fn match_child(child: &Child, oracle: &serde_json::Value) -> Result<(), String> {
    let node_type = oracle["type"].as_str().unwrap_or("?");
    match child {
        Child::Tree(subtree) => {
            if node_type != "tree" {
                return Err(format!("Rust has Tree but oracle has '{node_type}'"));
            }
            match_node_tree(subtree, oracle)
        }
        Child::None => {
            // maybe_placeholders: a None child matches the oracle's serialized
            // placeholder {"type": "unknown", "repr": "None"}.
            if node_type != "unknown" {
                return Err(format!(
                    "Rust has None placeholder but oracle has '{node_type}'"
                ));
            }
            Ok(())
        }
        Child::Token(tok) => {
            if node_type != "token" {
                return Err(format!(
                    "Rust has Token({}) but oracle has '{node_type}'",
                    tok.type_
                ));
            }
            match_token(tok, oracle)
        }
    }
}

/// Bijectively match an `_ambig` node's alternatives against the oracle's,
/// ignoring order. Sizes are already checked equal by the caller. Uses
/// backtracking assignment (the forests are tiny), so a greedy mis-pairing can't
/// produce a false mismatch.
fn match_ambig(rust: &[Child], oracle: &[serde_json::Value]) -> Result<(), String> {
    let mut used = vec![false; rust.len()];
    if assign(rust, oracle, 0, &mut used) {
        Ok(())
    } else {
        Err(format!(
            "_ambig: could not match all {} alternatives between Rust and oracle (unordered)",
            oracle.len()
        ))
    }
}

fn assign(rust: &[Child], oracle: &[serde_json::Value], i: usize, used: &mut [bool]) -> bool {
    if i == oracle.len() {
        return true;
    }
    for j in 0..rust.len() {
        if used[j] {
            continue;
        }
        if match_child(&rust[j], &oracle[i]).is_ok() {
            used[j] = true;
            if assign(rust, oracle, i + 1, used) {
                return true;
            }
            used[j] = false;
        }
    }
    false
}
