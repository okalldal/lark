//! Wild-grammar benchmarks: build + parse throughput over the real-world bank
//! in `tests/wild/` (HCL2/Terraform, MapServer mapfiles, GraphQL SDL, PEP 508,
//! MistQL, Synapse Storm, Vyper, Quil — see each project's `meta.json`).
//!
//! **Recorded trend, not a gate** — the wild complement to `benches/parse.rs`:
//! where that file measures synthetic scaling workloads, this one measures the
//! grammars and inputs users actually have. Each project is built with the same
//! Lark options upstream uses (recorded in its `meta.json`); a grammar lark-rs
//! cannot build yet (see `tests/fixtures/oracles/wild/xfail.json`) is reported
//! as a SKIP line rather than failing the bench.
//!
//! Run with `cargo bench --bench wild`. Output format matches benches/parse.rs:
//! a greppable `BENCH<TAB>…` line per workload plus a human table.

use lark_rs::{Lark, LarkOptions, LexerType, ParserAlgorithm};
use serde_json::Value;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn wild_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/wild")
}

fn meta_options(meta: &Value) -> Option<LarkOptions> {
    let opts = &meta["lark_options"];
    Some(LarkOptions {
        start: vec![opts["start"].as_str()?.to_string()],
        parser: match opts["parser"].as_str()? {
            "lalr" => ParserAlgorithm::Lalr,
            "earley" => ParserAlgorithm::Earley,
            "cyk" => ParserAlgorithm::Cyk,
            _ => return None,
        },
        lexer: match opts["lexer"].as_str()? {
            "basic" => LexerType::Basic,
            "contextual" => LexerType::Contextual,
            "dynamic" => LexerType::Dynamic,
            "dynamic_complete" => LexerType::DynamicComplete,
            _ => return None,
        },
        maybe_placeholders: opts["maybe_placeholders"].as_bool().unwrap_or(true),
        keep_all_tokens: opts["keep_all_tokens"].as_bool().unwrap_or(false),
        propagate_positions: opts["propagate_positions"].as_bool().unwrap_or(false),
        postlex: match opts["postlex"].as_str() {
            Some("PythonIndenter") => Some(lark_rs::postlex::Indenter::default()),
            Some(_) => return None,
            None => None,
        },
        base_path: None, // set per project below
        ..Default::default()
    })
}

struct Stat {
    min_ns: f64,
    median_ns: f64,
}

/// Same estimator as benches/parse.rs: calibrate the inner iteration count past
/// the timer resolution, then min/median over samples within a wall-time cap.
fn measure<F: FnMut()>(mut f: F) -> Stat {
    let mut iters = 1usize;
    loop {
        let t = Instant::now();
        for _ in 0..iters {
            f();
        }
        if t.elapsed() >= Duration::from_millis(1) || iters >= 1 << 22 {
            break;
        }
        iters = (iters * 2).max(1);
    }
    let mut samples: Vec<f64> = Vec::new();
    let overall = Instant::now();
    while samples.len() < 50 && overall.elapsed() < Duration::from_millis(1500) {
        let t = Instant::now();
        for _ in 0..iters {
            f();
        }
        samples.push(t.elapsed().as_nanos() as f64 / iters as f64);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Stat {
        min_ns: samples[0],
        median_ns: samples[samples.len() / 2],
    }
}

fn bench_project(pdir: &Path) {
    let meta: Value =
        serde_json::from_str(&std::fs::read_to_string(pdir.join("meta.json")).unwrap()).unwrap();
    let name = meta["name"].as_str().unwrap().to_string();
    let grammar =
        std::fs::read_to_string(pdir.join(meta["entry_grammar"].as_str().unwrap())).unwrap();
    let Some(mut opts) = meta_options(&meta) else {
        println!("SKIP\t{name}\toptions not representable");
        return;
    };
    opts.base_path = Some(pdir.join("grammar"));

    // Build cost: one timed sample only — wild grammars can take seconds to
    // build (that is itself the signal), so the calibrated loop would blow the
    // time budget. Single-shot wall time is plenty for a recorded trend.
    let build_opts = opts.clone();
    let t0 = Instant::now();
    let built = Lark::new(&grammar, build_opts);
    let build_ns = t0.elapsed().as_nanos() as f64;
    let lark = match built {
        Ok(l) => l,
        Err(_) => {
            println!("SKIP\t{name}\tgrammar does not build (see wild xfail.json)");
            return;
        }
    };
    println!(
        "BENCH\twild_build\t{name}\t{}\t{build_ns:.0}\t{build_ns:.0}\t0",
        grammar.len()
    );
    println!(
        "  build  {name:<16} {:>8} B   {build_ns:>12.0} ns (single shot)",
        grammar.len()
    );

    // Parse throughput over the whole corpus (every vendored input that
    // parses), plus the single largest parsing input.
    let mut inputs: Vec<(String, String)> = meta["inputs"]
        .as_object()
        .unwrap()
        .keys()
        .map(|rel| {
            (
                rel.clone(),
                std::fs::read_to_string(pdir.join(rel)).unwrap(),
            )
        })
        .filter(|(_, text)| lark.parse(text).is_ok())
        .collect();
    if inputs.is_empty() {
        println!("SKIP\t{name}\tno input parses (see wild xfail.json)");
        return;
    }
    inputs.sort_by_key(|(_, text)| text.len());

    let corpus_bytes: usize = inputs.iter().map(|(_, t)| t.len()).sum();
    let stat = measure(|| {
        for (_, text) in &inputs {
            black_box(lark.parse(black_box(text)).unwrap());
        }
    });
    let mb_per_s = corpus_bytes as f64 / stat.median_ns * 1e3;
    println!(
        "BENCH\twild_corpus\t{name}\t{corpus_bytes}\t{:.0}\t{:.0}\t{mb_per_s:.1}",
        stat.median_ns, stat.min_ns
    );
    println!(
        "  corpus {name:<16} {corpus_bytes:>8} B   {:>12.0} ns/iter   {mb_per_s:>7.1} MB/s   ({} inputs)",
        stat.median_ns,
        inputs.len()
    );

    let (largest_rel, largest) = inputs.last().unwrap();
    let stat = measure(|| {
        black_box(lark.parse(black_box(largest)).unwrap());
    });
    let mb_per_s = largest.len() as f64 / stat.median_ns * 1e3;
    println!(
        "BENCH\twild_largest\t{name}\t{}\t{:.0}\t{:.0}\t{mb_per_s:.1}",
        largest.len(),
        stat.median_ns,
        stat.min_ns
    );
    println!(
        "  large  {name:<16} {:>8} B   {:>12.0} ns/iter   {mb_per_s:>7.1} MB/s   ({largest_rel})",
        largest.len(),
        stat.median_ns
    );
}

fn main() {
    println!("# lark-rs wild-grammar benchmarks (tests/wild bank)");
    println!("# columns: BENCH<TAB>kind<TAB>name<TAB>bytes<TAB>median_ns<TAB>min_ns<TAB>mb_per_s");
    println!();
    let mut projects: Vec<PathBuf> = std::fs::read_dir(wild_dir())
        .expect("tests/wild exists")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("meta.json").is_file())
        .collect();
    projects.sort();
    for pdir in &projects {
        bench_project(pdir);
    }
}
