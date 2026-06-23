//! Meta-test: the oracle test-suite must stay HONEST (#253, ADR-0030).
//!
//! Two invariants, enforced at `cargo test` time without needing Python:
//!
//! 1. The tolerated-contradiction allow-list (`tools/oracle_contradictions.json`)
//!    exactly matches the set the generator detected and froze
//!    (`tests/fixtures/oracles/_meta/contradictions.json`) — no un-reasoned
//!    contradiction, no stale entry — and every entry carries a real reason.
//!    `tools/generate_oracles.py` enforces the same at regeneration time (exiting
//!    non-zero otherwise); this guards the two files against being hand-edited out
//!    of sync between regenerations, in a CI job that may not run Python.
//!
//! 2. No oracle replay silently skips a case whose author expectation contradicts
//!    Python — the `(true, false, _) => {}` / `(false, true, _) => {}` anti-pattern.
//!    Such a case must fail loudly or be routed through a documented allow-list
//!    (e.g. `common::replay_oracle_cases`'s `more_permissive`), never swallowed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_json(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("invalid JSON in {}: {e}", path.display()))
}

#[test]
fn test_oracle_contradiction_allowlist_matches_detected() {
    let allow = read_json(&manifest().join("tools/oracle_contradictions.json"));
    let entries = allow["entries"]
        .as_object()
        .expect("oracle_contradictions.json needs an `entries` object");
    let allowed: BTreeSet<String> = entries.keys().cloned().collect();

    let detected_json =
        read_json(&manifest().join("tests/fixtures/oracles/_meta/contradictions.json"));
    let detected: BTreeSet<String> = detected_json["keys"]
        .as_array()
        .expect("_meta/contradictions.json needs a `keys` array")
        .iter()
        .map(|k| k.as_str().expect("key must be a string").to_string())
        .collect();

    let un_allowed: Vec<&String> = detected.difference(&allowed).collect();
    let stale: Vec<&String> = allowed.difference(&detected).collect();

    assert!(
        un_allowed.is_empty(),
        "oracle contradictions detected by the generator but NOT allow-listed with a \
         reason (run `python3 tools/generate_oracles.py` and add reasons to \
         tools/oracle_contradictions.json):\n{un_allowed:#?}"
    );
    assert!(
        stale.is_empty(),
        "stale entries in tools/oracle_contradictions.json — listed but no longer \
         detected by the generator (remove them):\n{stale:#?}"
    );

    // Every reason must be substantive, not a blank or placeholder.
    for (key, reason) in entries {
        let r = reason.as_str().unwrap_or("");
        assert!(
            r.trim().len() >= 20,
            "allow-list entry {key:?} needs a substantive reason (got {r:?})"
        );
    }
}

#[test]
fn test_no_silent_contradiction_skips_in_oracle_replays() {
    // The exact arms that silently swallow an author-expectation/oracle contradiction.
    // (A genuinely-tolerated divergence goes through an allow-list, not an empty arm.)
    let banned = ["(true, false, _) => {}", "(false, true, _) => {}"];
    let mut offenders = Vec::new();

    for entry in std::fs::read_dir(manifest().join("tests")).expect("read tests dir") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        // This meta-test names the anti-pattern as string literals — skip itself.
        if path.file_name().and_then(|n| n.to_str()) == Some("test_oracle_honesty.rs") {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap_or_default();
        for pat in banned {
            if src.contains(pat) {
                offenders.push(format!("{}: contains silent skip `{pat}`", path.display()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "oracle replays must not silently skip oracle/expectation contradictions — fail \
         loudly or route through a documented allow-list instead:\n{}",
        offenders.join("\n")
    );
}
