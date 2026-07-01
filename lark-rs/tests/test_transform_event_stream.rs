//! Event-stream differential (issue #595, Slice 1 of #594): a grammar-agnostic
//! `parse_into` regression net over the **whole** LALR compliance bank.
//!
//! The 20-case transformer trace oracle (`test_transformer_oracle.rs`) proved the
//! semantic-output seam on curated grammars. This generalizes it: for every
//! accepted `(grammar, options, input)` in the compliance bank,
//! `tools/generate_event_stream_oracle.py` ran a "log every callback" transformer
//! **embedded** in Python Lark (`Lark(…, transformer=T)`, LALR) and committed the
//! ordered event stream (`transformer/event_stream_bank.json`). Here we drive the
//! same accepted cases through the public [`Lark::parse_into`] seam with an
//! event-logging [`OutputBuilder`] and assert a **byte-identical** stream, over
//! **both** the basic and contextual lexers.
//!
//! ## Why the two observation points line up
//!
//! * **Token events** fire for *every shifted terminal*. Python's embedded path
//!   applies a terminal's transformer method as the lazy lexer yields each token
//!   (interleaved with reductions), and `parse_into`'s [`OutputBuilder::token`]
//!   fires at shift time — the same set and order. The generator registers an
//!   explicit method per terminal (not `__default_token__`, which the embedded path
//!   never wires — issue #229), so token events fire symmetrically. `%ignore`d
//!   terminals never surface to the parser on either side.
//! * **Rule events** fire for every *non-transparent* reduction. `parse_into`'s
//!   [`OutputBuilder::reduce`] runs only for a rule that builds a kept node; a
//!   `_rule` / `__anon_*` helper is spliced without a `reduce` call, and a
//!   collapsing `?rule` builds none. The generator's `__default__` tracer drops the
//!   `_`-prefixed names to match, and Python's `ExpandSingleChild` skips the
//!   collapsing `?rule` callback identically.
//!
//! ## XFAIL discipline (ADR-0030)
//!
//! Gated by `event_stream_xfail.json`, exactly like the compliance banks: every
//! known divergence is listed explicitly, the list only shrinks, and there are **no
//! silent skips** — a case whose lark-rs stream differs from Python's (an extra,
//! missing, mis-ordered, or mis-valued event) fails unless allow-listed. Set
//! `LARK_EVENT_STREAM_WRITE_XFAIL=1` to regenerate the allow-list after an
//! intentional change, then review the diff before committing.
//!
//! Scope: LALR + basic/contextual only — Python's `transformer=` and `parse_into`
//! are both LALR-only (ADR-0029 fork 4), so this is symmetric, not a coverage gap.

mod common;

use lark_rs::grammar::terminal::flags;
use lark_rs::{
    Lark, LarkOptions, LexerType, Meta, OutputBuilder, OutputContext, ParserAlgorithm, Token,
};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

// ── The event-logging OutputBuilder ──────────────────────────────────────────

/// An [`OutputBuilder`] that records one `{kind, name, …}` event per shifted
/// terminal and per non-transparent reduction — the `parse_into` mirror of the
/// generator's embedded tracing transformer. It builds no value (`Value = ()`);
/// the event log *is* the output.
struct EventLog {
    events: Vec<Value>,
}

impl<'i> OutputBuilder<'i> for EventLog {
    type Value = ();

    fn token(&mut self, token: Token, _input: &'i str, _ctx: &OutputContext) {
        // Fires for every shifted terminal — kept or filtered — exactly as the
        // embedded lexer callback does. `value` mirrors Python's `str(token)`.
        self.events.push(json!({
            "kind": "token",
            "name": token.type_,
            "value": token.value,
        }));
    }

    fn reduce(&mut self, rule: usize, _children: &mut Vec<()>, _meta: &Meta, ctx: &OutputContext) {
        // Only non-transparent, non-collapsed reductions reach `reduce` — the same
        // rules Python's `__default__` tracer logs (after dropping `_`-prefixed
        // helpers). `callback_name` is the alias-else-origin name Python dispatches on.
        self.events.push(json!({
            "kind": "rule",
            "name": ctx.callback_name(rule),
        }));
    }

    fn placeholder(&mut self, _ctx: &OutputContext) {
        // A `maybe_placeholders` absent `[...]` inserts a None child but fires no
        // callback on either side — no event.
    }
}

// ── Fixture loading + option decoding ────────────────────────────────────────

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oracles/transformer")
}

fn load_json(name: &str) -> Option<Value> {
    let path = fixtures_dir().join(name);
    let text = std::fs::read_to_string(&path).ok()?;
    Some(serde_json::from_str(&text).expect("valid JSON"))
}

/// The two configs every entry may carry; maps a config key to its lexer.
fn lexer_of(config_key: &str) -> LexerType {
    match config_key {
        "lalr_basic" => LexerType::Basic,
        _ => LexerType::Contextual,
    }
}

/// Build [`LarkOptions`] from an entry under one lexer — the same decoding
/// `test_compliance.rs` uses (start string-or-array, `imsx` → flag bitset).
fn entry_options(entry: &Value, lexer: LexerType) -> LarkOptions {
    let start = match &entry["start"] {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => vec!["start".to_string()],
    };
    let mut g_regex_flags = 0u32;
    if let Some(letters) = entry["g_regex_flags"].as_str() {
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
        lexer,
        maybe_placeholders: entry["maybe_placeholders"].as_bool().unwrap_or(true),
        keep_all_tokens: entry["keep_all_tokens"].as_bool().unwrap_or(false),
        strict: entry["strict"].as_bool().unwrap_or(false),
        g_regex_flags,
        ..Default::default()
    }
}

/// Build a parser, treating both errors and panics as "did not build".
fn try_build(grammar: &str, opts: LarkOptions) -> Option<Lark> {
    match catch_unwind(AssertUnwindSafe(|| Lark::new(grammar, opts))) {
        Ok(Ok(lark)) => Some(lark),
        _ => None,
    }
}

/// Drive one input through `parse_into` with a fresh [`EventLog`], returning the
/// recorded event array, or `None` on error/panic.
fn try_event_stream(lark: &Lark, input: &str) -> Option<Value> {
    match catch_unwind(AssertUnwindSafe(|| {
        let mut log = EventLog { events: Vec::new() };
        lark.parse_into(input, &mut log).map(|_| log.events)
    })) {
        Ok(Ok(events)) => Some(Value::Array(events)),
        _ => None,
    }
}

/// The committed oracle stream for one case under `config_key`: the shared
/// `events` array, or the per-config override when a lexer-interleaving artifact
/// was pinned separately.
fn oracle_events<'a>(case: &'a Value, config_key: &str) -> &'a Value {
    if let Some(by_config) = case.get("events_by_config") {
        &by_config[config_key]
    } else {
        &case["events"]
    }
}

// ── The differential ─────────────────────────────────────────────────────────

#[test]
fn test_transform_event_stream_bank() {
    let Some(data) = load_json("event_stream_bank.json") else {
        eprintln!("event_stream_bank.json not found — run tools/generate_event_stream_oracle.py");
        return;
    };
    let entries = data["entries"].as_array().expect("entries is an array");

    // Silence panic backtraces from any grammar lark-rs cannot yet build.
    std::panic::set_hook(Box::new(|_| {}));

    let mut failures: BTreeSet<String> = BTreeSet::new();
    let mut checked = 0usize;

    for entry in entries {
        let ri = entry["record_index"].as_u64().unwrap_or(0);
        let grammar = entry["grammar"].as_str().unwrap_or("");
        let configs = entry["configs"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let cases = entry["cases"].as_array().map(Vec::as_slice).unwrap_or(&[]);

        for config in configs {
            let config_key = config.as_str().unwrap_or("");
            let lexer = lexer_of(config_key);
            let lark = try_build(grammar, entry_options(entry, lexer));

            for (ci, case) in cases.iter().enumerate() {
                // Only replay configs whose Python embedded transform produced a
                // stream for this input (a contextual grammar can reject an input
                // the basic lexer mis-tokenizes — no stream, nothing to compare).
                let ok_here = case["ok_configs"]
                    .as_array()
                    .map(|a| a.iter().any(|c| c.as_str() == Some(config_key)))
                    .unwrap_or(false);
                if !ok_here {
                    continue;
                }
                checked += 1;

                let id = format!("{ri}:{config_key}:{ci}");
                let Some(lark) = &lark else {
                    // Python built + transformed this; lark-rs could not build the
                    // grammar. A real divergence, not a silent skip.
                    failures.insert(id);
                    continue;
                };
                let input = case["input"].as_str().unwrap_or("");
                match try_event_stream(lark, input) {
                    Some(got) if &got == oracle_events(case, config_key) => {}
                    _ => {
                        failures.insert(id);
                    }
                }
            }
        }
    }

    let xfail: BTreeSet<String> = load_json("event_stream_xfail.json")
        .and_then(|v| v.as_array().cloned())
        .map(|a| {
            a.into_iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    assert!(
        checked > 0,
        "no accepted case-configs were checked — event_stream_bank.json is empty or malformed"
    );

    let passing = checked - failures.len();
    let pct = if checked == 0 {
        100.0
    } else {
        100.0 * passing as f64 / checked as f64
    };
    eprintln!(
        "event-stream differential: {passing}/{checked} case-configs byte-identical \
         ({pct:.1}%); {} known-XFAIL",
        xfail.len()
    );

    if std::env::var("LARK_EVENT_STREAM_WRITE_XFAIL").is_ok() {
        let list: Vec<&String> = failures.iter().collect();
        let path = fixtures_dir().join("event_stream_xfail.json");
        std::fs::write(&path, serde_json::to_string_pretty(&list).unwrap() + "\n")
            .expect("write event_stream_xfail.json");
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
            "note: {} XFAIL entries now pass — consider regenerating event_stream_xfail.json",
            fixed.len()
        );
    }
    assert!(
        regressions.is_empty(),
        "event-stream regressions ({} case-configs newly diverging and not in \
         event_stream_xfail.json):\n{}",
        regressions.len(),
        regressions
            .iter()
            .take(40)
            .map(|s| format!("  - {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
