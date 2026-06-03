/// Shared test utilities: oracle loading, tree comparison, parser helpers.

use lark_rs::{Lark, LarkOptions, ParserAlgorithm, LexerType, ParseTree, Token, Tree, Child};

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
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("Oracle JSON parse error: {e}"))
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
        return Err(format!(
            "token value {:?} != {expected_value:?}",
            tok.value
        ));
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

    let oracle_children = oracle["children"].as_array().map(|a| a.as_slice()).unwrap_or(&[]);
    if tree.children.len() != oracle_children.len() {
        return Err(format!(
            "In '{}': got {} children, expected {}",
            tree.data,
            tree.children.len(),
            oracle_children.len()
        ));
    }

    for (i, (child, oc)) in tree.children.iter().zip(oracle_children.iter()).enumerate() {
        let node_type = oc["type"].as_str().unwrap_or("?");
        match child {
            Child::Tree(subtree) => {
                if node_type != "tree" {
                    return Err(format!(
                        "In '{}' child {i}: Rust has Tree but oracle has '{node_type}'",
                        tree.data
                    ));
                }
                match_node_tree(subtree, oc)
                    .map_err(|e| format!("In '{}' child {i}: {e}", tree.data))?;
            }
            Child::None => {
                // maybe_placeholders: a None child matches the oracle's serialized
                // placeholder {"type": "unknown", "repr": "None"}.
                if node_type != "unknown" {
                    return Err(format!(
                        "In '{}' child {i}: Rust has None placeholder but oracle has '{node_type}'",
                        tree.data
                    ));
                }
            }
            Child::Token(tok) => {
                if node_type != "token" {
                    return Err(format!(
                        "In '{}' child {i}: Rust has Token({}) but oracle has '{node_type}'",
                        tree.data, tok.type_
                    ));
                }
                match_token(tok, oc)
                    .map_err(|e| format!("In '{}' child {i}: {e}", tree.data))?;
            }
        }
    }
    Ok(())
}
