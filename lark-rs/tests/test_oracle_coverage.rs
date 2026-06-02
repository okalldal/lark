//! Enforcement: every committed grammar must be backed by an oracle test, or be
//! explicitly quarantined with a reason. This gates the "every feature needs an
//! oracle before we implement it" rule so coverage cannot silently erode — a new
//! `tests/grammars/*.lark` with no oracle and no quarantine entry fails the build.

use std::path::Path;

/// Grammars knowingly committed without an oracle yet, each with a reason.
/// Remove an entry the moment you add its oracle. Do NOT add to this list to
/// silence the test without a genuine reason — that defeats the purpose.
const QUARANTINE: &[(&str, &str)] = &[
    (
        "python2",
        "6440-line WIP needing an INDENT/DEDENT indenter (Phase 3); not yet \
         parseable end-to-end.",
    ),
];

fn grammars_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/grammars")
}

fn oracle_dir(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/oracles")
        .join(name)
}

#[test]
fn test_every_grammar_has_oracle_or_quarantine() {
    let quarantined: std::collections::BTreeSet<&str> =
        QUARANTINE.iter().map(|(n, _)| *n).collect();

    let mut uncovered = Vec::new();
    for entry in std::fs::read_dir(grammars_dir()).expect("read grammars dir") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("lark") {
            continue;
        }
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        let has_oracle = oracle_dir(&name).is_dir();
        if !has_oracle && !quarantined.contains(name.as_str()) {
            uncovered.push(name);
        }
    }

    assert!(
        uncovered.is_empty(),
        "these grammars have no oracle and are not quarantined — add an oracle \
         test (see tools/generate_oracles.py) or a QUARANTINE entry with a reason:\n{}",
        uncovered
            .iter()
            .map(|n| format!("  - tests/grammars/{n}.lark"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn test_quarantine_entries_are_not_stale() {
    // A quarantined grammar must still exist and still lack an oracle, otherwise
    // the entry is stale and should be removed.
    for (name, _reason) in QUARANTINE {
        let grammar = grammars_dir().join(format!("{name}.lark"));
        assert!(
            grammar.exists(),
            "QUARANTINE lists '{name}' but tests/grammars/{name}.lark does not exist — \
             remove the stale entry"
        );
        assert!(
            !oracle_dir(name).is_dir(),
            "QUARANTINE lists '{name}' but an oracle now exists at \
             tests/fixtures/oracles/{name}/ — remove the quarantine entry"
        );
    }
}
