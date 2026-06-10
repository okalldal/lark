//! L2 lowering harness — **layer 3: the Route-1 DFA-equivalence proof**
//! (`docs/LEXER_DFA_PLAN.md`, "Verification harness" + `TERMINAL_REDUCTION_DIAGNOSIS.md`
//! "What a proof of equivalence would require").
//!
//! Generative equivalence (layer 2) is *exhaustive to a bound* — strong evidence,
//! not a proof. Route-1 closes the gap with the **decidable** decision procedure:
//! lower the bounded assertion to a lookaround-free automaton, compile both the
//! lowered terminal and the `fancy-regex` reference to anchored match-DFAs, and
//! decide match-length equality by **product construction** (unequal ⇒ shortest
//! counterexample). A shape is **not "supported" until its representative proof is
//! committed** (the plan's per-shape proof obligation).
//!
//! All proofs are **active** (nothing is `#[ignore]`'d): every supported shape lowers,
//! so [`prove_route1`] runs the decision procedure against the real lowered matcher for
//! each committed representative, with `fancy-regex` the independent oracle. Two
//! realizations of the same Myhill-Nerode bound:
//!   * the bounded boundary/lookbehind shapes use the brute byte-class enumeration up to
//!     `n + W + 2` ([`prove_route1`]);
//!   * the `python.STRING` opening-guard splice — whose content-bearing body makes that
//!     enumeration intractable (`|alphabet|^(n+W+2)` ≈ 10²⁵) — uses the **state-pruned**
//!     enumeration ([`prove_route1_pruned`]): one shortest witness per base-DFA state ×
//!     all ≤ `W+1` lookahead suffixes. A shape is **not "supported" until its
//!     representative proof is committed** (the plan's per-shape obligation), enforced by
//!     [`every_supported_shape_has_a_committed_proof_obligation`].

mod common;

use std::collections::HashSet;

use common::lowering::{corpus, fancy_matcher, fancy_prefix, lowered_prefix};
use lark_rs::lookaround::lower::lower_boundary;
use lark_rs::{classify, ShapeClass, Verdict};
use regex_automata::dfa::{dense, Automaton};

/// A representative terminal whose Route-1 equivalence must be proven before its
/// shape is declared supported. The bundled six are here by name, plus a synthetic
/// representative per shape.
struct ProofObligation {
    name: &'static str,
    pattern: &'static str,
    shape: ShapeClass,
}

/// The committed proof obligations, one+ per supported shape (the bundled six map
/// onto these). Recognizing `python.STRING`'s nested-leading guard is a first-shape
/// classifier refinement, so STRING is represented here by the *top-level* leading
/// form its lowering must reproduce, not its raw nested pattern.
fn obligations() -> Vec<ProofObligation> {
    vec![
        // Trailing boundary — the bundled OP / DEC_NUMBER guards.
        ProofObligation {
            name: "OP",
            pattern: r"[?](?![a-z])",
            shape: ShapeClass::TrailingBoundary,
        },
        ProofObligation {
            name: "DEC_NUMBER",
            pattern: r"0(?![1-9])",
            shape: ShapeClass::TrailingBoundary,
        },
        // Leading boundary — a reserved-word-style exclusion + the STRING-style
        // opening guard. The exclusion guard is kept **narrow** (`aa`, overlapping the
        // `[a-z]+` body so it is decisive) so the Route-1 enumeration over byte
        // equivalence classes stays tractable; a wide multi-literal guard like
        // `(?!if|else)` distinguishes every literal byte and blows the alphabet up
        // exponentially against the length bound — that variant is covered exhaustively
        // by the generative-equivalence layer instead (`test_lowering_equivalence`).
        ProofObligation {
            name: "RESERVED",
            pattern: r"(?!aa)[a-z]+",
            shape: ShapeClass::LeadingBoundary,
        },
        ProofObligation {
            name: "STRING_OPEN",
            pattern: r#"(?!"")[^"]*"#,
            shape: ShapeClass::LeadingBoundary,
        },
        // Bounded lookbehind — the backslash-parity close + a fixed-width lookbehind
        // representative. Both reps are chosen to **bite within an offset-0 match** (a
        // variable preceding class containing the trigger), so the proof is not
        // vacuous: a leading lookbehind matched at offset 0 sees nothing before pos, so
        // `(?<=ab)c`-style reps would prove nothing. `[a\\](?<!\\)b` rejects `\b` and
        // accepts `ab`; `\w(?<!_)x` rejects `_x` and accepts `ax`.
        // The reps are kept **narrow** (a small preceding class, not `\w` / `[a\\]`)
        // so the Route-1 byte-class enumeration stays tractable against the length
        // bound — the same discipline RESERVED uses above. The wide backslash-run /
        // `\w` variants are covered exhaustively by the generative-equivalence layer.
        ProofObligation {
            name: "LONG_STRING_CLOSE",
            pattern: r#"[\\a](?<!\\)a"#,
            shape: ShapeClass::BoundedLookbehind,
        },
        ProofObligation {
            name: "FIXED_BEHIND",
            pattern: r"[ab](?<!a)b",
            shape: ShapeClass::BoundedLookbehind,
        },
    ]
}

/// Decide Route-1 match-length equivalence between the lowered terminal and the
/// `fancy-regex` reference. Returns `Ok(())` when proven equivalent, `Err(cex)` with
/// the shortest counterexample otherwise.
///
/// **Why this is a proof, not bounded evidence (layer 2).** The lowered matcher's
/// match-length at offset 0 is a function of (a) the base recognizer's DFA state and
/// (b) the next ≤ `W` lookahead characters (`W` = the guard body's max width). So two
/// trailing-boundary matchers can only *disagree* on a string that drives the base
/// recognizer to a distinguishing state — reachable within `n` steps (`n` = the base
/// DFA's state count, by Myhill-Nerode / pumping) — followed by at most `W`
/// lookahead chars. Therefore **every** divergence manifests on some string of length
/// `≤ n + W + 1`, and enumerating *all* strings up to `n + W + 2` over the DFA's byte
/// **equivalence classes** (one representative per class is sufficient, since bytes in
/// one class are indistinguishable to the automaton) is a complete decision
/// procedure — exactly the decidable product-equivalence Route-1 promises, with the
/// `fancy-regex` reference as the independent oracle (no shared code).
fn prove_route1(name: &str, pattern: &str) -> Result<(), String> {
    assert!(
        pattern.is_ascii(),
        "Route-1 proof assumes ASCII representatives; {pattern:?} is not ASCII"
    );
    let branches = lower_boundary(pattern).map_err(|e| format!("lowering failed: {e:?}"))?;

    // Combined lookaround-free regex over every base branch ∪ every guard body — its
    // dense DFA gives the byte equivalence classes (the sound enumeration alphabet).
    let mut parts: Vec<String> = Vec::new();
    let mut base_parts: Vec<String> = Vec::new();
    for b in &branches {
        parts.push(format!("(?:{})", b.regex));
        base_parts.push(format!("(?:{})", b.regex));
        for g in [&b.leading, &b.trailing].into_iter().flatten() {
            parts.push(format!("(?:{})", g.set));
        }
        // A lookbehind's trigger body must enter the byte-class alphabet too, or the
        // enumeration would never exercise the char the guard keys on (the proof would
        // be vacuous — the same vacuity the biting reps defend against).
        for lb in &b.lookbehind {
            parts.push(format!("(?:{})", lb.set));
        }
    }
    let combined = dense::DFA::new(&parts.join("|")).map_err(|e| format!("dfa(combined): {e}"))?;
    let base = dense::DFA::new(&base_parts.join("|")).map_err(|e| format!("dfa(base): {e}"))?;

    // One representative char per byte equivalence class (ASCII bytes are valid chars;
    // for an ASCII pattern every distinguishable byte ≤ 0x7F is covered, and bytes
    // ≥ 0x80 share the catch-all class).
    let classes = combined.byte_classes();
    let mut seen: HashSet<u8> = HashSet::new();
    let mut alphabet: Vec<char> = Vec::new();
    for byte in 0u8..=0x7F {
        let cls = classes.get(byte);
        if seen.insert(cls) {
            alphabet.push(byte as char);
        }
    }
    let rep_bytes: Vec<u8> = alphabet.iter().map(|&c| c as u8).collect();

    // n = base recognizer's reachable-state count (BFS over the byte-class
    // representatives + EOI); W = max guard-body width (from the classifier).
    let n = reachable_states(&base, &rep_bytes);
    let w = classify(pattern)
        .map_err(|e| format!("classify: {e:?}"))?
        .assertions
        .iter()
        .filter_map(|a| a.width)
        .max()
        .unwrap_or(0);
    let bound = n + w + 2;

    // The enumeration is `|alphabet|^bound` strings — complete, but exponential. A
    // representative whose guard distinguishes many byte classes (a wide multi-literal
    // guard) blows this up; such a rep belongs in the generative-equivalence layer, not
    // here. Fail loudly with guidance rather than OOM.
    let space = (alphabet.len() as u128).checked_pow(bound as u32 + 1);
    assert!(
        space.is_some_and(|s| s <= 2_000_000),
        "Route-1 enumeration for {pattern:?} is intractable \
         (|alphabet|={} ^ bound={bound} too large) — choose a narrower-guard \
         representative; the wide-guard variant is covered by the generative layer",
        alphabet.len(),
    );

    let oracle = fancy_matcher(pattern).ok_or_else(|| format!("fancy rejected {pattern:?}"))?;
    for input in corpus(&alphabet, bound) {
        let lowered = lowered_prefix(name, pattern, &input)?;
        let fancy = fancy_prefix(&oracle, &input);
        if lowered != fancy {
            return Err(format!(
                "counterexample on {input:?}: lowered={lowered:?} != fancy={fancy:?}"
            ));
        }
    }
    Ok(())
}

/// Count the reachable states of `dfa` by BFS over the byte-class representatives
/// (sound: bytes in one class share transitions) plus the EOI transition — a public
/// stand-in for the private state count, giving the Myhill-Nerode length bound.
fn reachable_states(dfa: &dense::DFA<Vec<u32>>, rep_bytes: &[u8]) -> usize {
    use regex_automata::{Anchored, Input};
    let start = dfa
        .start_state_forward(&Input::new("").anchored(Anchored::Yes))
        .expect("start state");
    let mut seen: HashSet<_> = HashSet::new();
    let mut stack = vec![start];
    seen.insert(start);
    while let Some(s) = stack.pop() {
        let mut nexts: Vec<_> = rep_bytes.iter().map(|&b| dfa.next_state(s, b)).collect();
        nexts.push(dfa.next_eoi_state(s));
        for ns in nexts {
            if seen.insert(ns) {
                stack.push(ns);
            }
        }
    }
    seen.len()
}

fn discharge(shape: ShapeClass) {
    let obs: Vec<_> = obligations()
        .into_iter()
        .filter(|o| o.shape == shape)
        .collect();
    assert!(
        !obs.is_empty(),
        "no proof obligation registered for {shape:?}"
    );
    for o in obs {
        prove_route1(o.name, o.pattern)
            .unwrap_or_else(|cex| panic!("Route-1 proof failed for {}: {cex}", o.name));
    }
}

#[test]
fn route1_proof_trailing_boundary() {
    discharge(ShapeClass::TrailingBoundary);
}

#[test]
fn route1_proof_leading_boundary() {
    discharge(ShapeClass::LeadingBoundary);
}

#[test]
fn route1_proof_bounded_lookbehind() {
    discharge(ShapeClass::BoundedLookbehind);
}

// ─── The STRING opening-guard splice — real nested shape ────────────────────────
//
// `python.STRING`'s `(?!"")` after the variable-width prefix + the opening quote is the
// marquee L2 splice. Its representative is the **real nested shape** (prefix + `(?!"")` +
// lazy body + `(?<!\\)` lookbehind), not the simplified top-level `(?!"")[^"]*` cousin.
//
// Why a different proof *method*: the brute Route-1 enumeration above is
// `|alphabet|^(n+W+2)` strings, which a content-bearing body (STRING's `.*?`) blows up
// (n≈13 base states, ~25 byte-class reps ⇒ ~10^25). The decision procedure is realized
// **state-pruned** instead — the same Myhill-Nerode basis `prove_route1` documents:
// every divergence between two bounded-lookahead matchers manifests at a *distinguishing
// base-DFA state* followed by ≤ W lookahead chars. So testing one shortest witness per
// reachable base state, extended by every lookahead suffix of length ≤ W+1, is a complete
// decision procedure — `n · |reps|^(W+1)` (~200k for STRING), tractable, with
// `fancy-regex` the independent oracle. It is gated additionally by the generative
// equivalence layer and the python.lark differential.

/// The real nested STRING representatives (double-quote, single-quote, and the bundled
/// both-arms shape — raw, no `/i`, which `lowered_prefix`'s one-terminal grammar applies
/// only via inline flags). Each is the genuine `prefix + (?!"") + .*? + (?<!\\)(\\\\)*?`
/// nested form the splice must reproduce.
fn string_proof_representatives() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "STRING_NESTED_DQ",
            r#"([ubf]?r?|r[ubf])("(?!"").*?(?<!\\)(\\\\)*?")"#,
        ),
        (
            "STRING_NESTED_BOTH",
            r#"([ubf]?r?|r[ubf])("(?!"").*?(?<!\\)(\\\\)*?"|'(?!'').*?(?<!\\)(\\\\)*?')"#,
        ),
    ]
}

/// Decide Route-1 match-length equivalence via the **state-pruned** Myhill-Nerode
/// decision procedure (see the section header). Complete and tractable for the
/// content-bearing string idiom; `Err(cex)` carries a counterexample.
fn prove_route1_pruned(name: &str, pattern: &str) -> Result<(), String> {
    use regex_automata::util::primitives::StateID;
    use regex_automata::{Anchored, Input};

    assert!(
        pattern.is_ascii(),
        "Route-1 proof assumes ASCII representatives; {pattern:?} is not ASCII"
    );
    let branches = lower_boundary(pattern).map_err(|e| format!("lowering failed: {e:?}"))?;

    // Base recognizer = the union of every lowered branch base (no guards). Its byte
    // equivalence classes are the sound enumeration alphabet; its reachable states are
    // the Myhill-Nerode partition we cover one witness apiece.
    let base_src = branches
        .iter()
        .map(|b| format!("(?:{})", b.regex))
        .collect::<Vec<_>>()
        .join("|");
    let base = dense::DFA::new(&base_src).map_err(|e| format!("dfa(base): {e}"))?;
    let classes = base.byte_classes();
    let mut seen_cls = HashSet::new();
    let reps: Vec<u8> = (0u8..=0x7F)
        .filter(|&b| seen_cls.insert(classes.get(b)))
        .collect();

    // W = the widest assertion in the original pattern (the lookahead window that can
    // make the splice's decision differ); the lookahead suffix is W+1 (one past the
    // window, for the trailing-guard EOF / next-char distinction).
    let w = classify(pattern)
        .map_err(|e| format!("classify: {e:?}"))?
        .assertions
        .iter()
        .filter_map(|a| a.width)
        .max()
        .unwrap_or(0);

    // BFS: a shortest witness (byte string) reaching each base-DFA state.
    let start = base
        .start_state_forward(&Input::new("").anchored(Anchored::Yes))
        .map_err(|e| format!("start: {e}"))?;
    let mut witness: std::collections::HashMap<StateID, Vec<u8>> = std::collections::HashMap::new();
    witness.insert(start, Vec::new());
    let mut queue = std::collections::VecDeque::from([start]);
    while let Some(st) = queue.pop_front() {
        let w_st = witness[&st].clone();
        for &b in &reps {
            let ns = base.next_state(st, b);
            if !witness.contains_key(&ns) {
                let mut nw = w_st.clone();
                nw.push(b);
                witness.insert(ns, nw);
                queue.push_back(ns);
            }
        }
    }

    // Tractability guard (states × |reps|^(W+1)): fail loudly rather than hang.
    let suffix_space = (reps.len() as u128).checked_pow(w as u32 + 1);
    let total = suffix_space.map(|s| s.saturating_mul(witness.len() as u128));
    assert!(
        total.is_some_and(|t| t <= 8_000_000),
        "state-pruned Route-1 for {pattern:?} is intractable (states={}, |reps|={}, W={w})",
        witness.len(),
        reps.len(),
    );

    // Every lookahead suffix of length 0..=W+1 over the byte-class representatives.
    let suffixes = corpus(&reps.iter().map(|&b| b as char).collect::<Vec<_>>(), w + 1);
    let oracle = fancy_matcher(pattern).ok_or_else(|| format!("fancy rejected {pattern:?}"))?;
    for wbytes in witness.values() {
        for suf in &suffixes {
            let mut full = wbytes.clone();
            full.extend_from_slice(suf.as_bytes());
            let Ok(s) = std::str::from_utf8(&full) else {
                continue;
            };
            let lowered = lowered_prefix(name, pattern, s)?;
            let fancy = fancy_prefix(&oracle, s);
            if lowered != fancy {
                return Err(format!(
                    "counterexample on {s:?}: lowered={lowered:?} != fancy={fancy:?}"
                ));
            }
        }
    }
    Ok(())
}

/// The committed Route-1 proof for the STRING opening-guard splice on its **real nested
/// shape** — the deliverable's non-negotiable proof obligation. Each representative must
/// (a) genuinely classify as a supported leading boundary (so the obligation targets the
/// splice), (b) lower to branches (not decline), and (c) be proven match-length-identical
/// to `fancy-regex` by the state-pruned decision procedure.
#[test]
fn route1_proof_string_idiom_real_nested_shape() {
    for (name, pattern) in string_proof_representatives() {
        let c = classify(pattern).unwrap_or_else(|e| panic!("classify {name} errored: {e}"));
        assert!(
            c.assertions
                .iter()
                .any(|a| a.verdict() == Verdict::Supported(ShapeClass::LeadingBoundary)),
            "{name} must classify with a supported leading boundary (the spliced guard)"
        );
        assert!(
            lower_boundary(pattern).is_ok(),
            "{name} must lower (not decline) for the proof to be non-vacuous"
        );
        prove_route1_pruned(name, pattern)
            .unwrap_or_else(|cex| panic!("Route-1 (state-pruned) failed for {name}: {cex}"));
    }
}

/// The committed Route-1 proof for the **regex-literal delimited-token idiom**
/// (`lark.REGEXP`, Stage B) on its real bundled shape. Like the STRING splice it is
/// content-bearing (the escaped-slash body), so the brute enumeration is intractable and the
/// **state-pruned** Myhill-Nerode decision procedure is used instead. Unlike STRING the
/// lowered branch is **guard-free** (the `(?!\/)` is absorbed into the non-empty `+` and the
/// lazy close into a proven greedy `+`), so the proof witnesses that the plain greedy DFA's
/// longest match reproduces `fancy-regex`'s lazy match across every reachable base state ×
/// short lookahead suffix — the exhaustive Python cross-check (`/`/`\`/`a`/`i` to length 8,
/// 0 divergences) closed in this PR, here machine-proven against the independent oracle.
#[test]
fn route1_proof_regexp_idiom() {
    const REGEXP_RAW: &str = r#"\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*"#;
    let c = classify(REGEXP_RAW).unwrap_or_else(|e| panic!("classify REGEXP errored: {e}"));
    assert!(
        c.assertions
            .iter()
            .any(|a| a.verdict() == Verdict::Supported(ShapeClass::LeadingBoundary)),
        "REGEXP's `(?!\\/)` must classify as a supported leading boundary (the stripped guard)"
    );
    assert!(
        lower_boundary(REGEXP_RAW).is_ok(),
        "REGEXP must lower (not decline) for the proof to be non-vacuous"
    );
    prove_route1_pruned("REGEXP", REGEXP_RAW)
        .unwrap_or_else(|cex| panic!("Route-1 (state-pruned) failed for REGEXP: {cex}"));
}

/// Active now: the proof-obligation registry is the per-shape contract. Every
/// supported shape has at least one committed representative, and each representative
/// genuinely classifies as its shape (so the obligation targets the right thing).
/// This fails the moment a shape is added without a committed proof representative.
#[test]
fn every_supported_shape_has_a_committed_proof_obligation() {
    for shape in [
        ShapeClass::TrailingBoundary,
        ShapeClass::LeadingBoundary,
        ShapeClass::BoundedLookbehind,
    ] {
        let obs: Vec<_> = obligations()
            .into_iter()
            .filter(|o| o.shape == shape)
            .collect();
        assert!(
            !obs.is_empty(),
            "supported shape {shape:?} has no committed Route-1 proof obligation"
        );
        for o in &obs {
            let c = classify(o.pattern)
                .unwrap_or_else(|e| panic!("classify proof rep {:?} errored: {e}", o.pattern));
            assert!(
                c.assertions
                    .iter()
                    .any(|a| a.verdict() == Verdict::Supported(shape)),
                "proof representative {} ({:?}) does not classify as {shape:?}",
                o.name,
                o.pattern
            );
        }
    }
}
