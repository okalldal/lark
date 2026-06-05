//! Cross-engine end-to-end throughput: lark-rs vs Python Lark (issue #50, Phase 4).
//!
//! The existing `benches/parse.rs` measures lark-rs *internally* (LALR vs Earley
//! cost-of-generality, scaling shapes). This bench is the **cross-engine
//! comparison** — the number behind the project's "10–100× faster than Python
//! Lark" goal — over three real workloads:
//!
//!   1. **JSON**   — the canonical JSON grammar on a ~92 KB array of records.
//!   2. **Python** — a significant-whitespace Python subset (driven by the
//!                   `Indenter` postlex hook) over a representative source file.
//!   3. **SQL**    — a SELECT/INSERT/UPDATE/DELETE grammar over a batch of statements.
//!
//! Each workload is run on **LALR + the contextual lexer** (Lark's primary USP),
//! and JSON + SQL are *also* run on **Earley** (the second engine) so the
//! Earley-vs-Python-Earley story has a number too. Python has no Earley row: its
//! `Indenter` postlex hook is LALR-only in lark-rs, and Python Lark can't pair
//! postlex with the dynamic lexer either — there is no apples-to-apples Earley
//! configuration for a significant-whitespace grammar. (The Earley-vs-LALR *cost
//! of generality* — same engine, same input — lives in `benches/parse.rs`.)
//!
//! A single command does the whole comparison:
//!
//! ```bash
//! cargo bench --bench vs_python_lark
//! ```
//!
//! It generates each workload once, times lark-rs, then writes the *byte-identical*
//! inputs to a temp dir and shells out to `benches/vs_python_lark.py` (the same
//! grammars, the in-tree Python Lark) so both engines parse the same bytes. It then
//! prints MB/s for each engine and the `python / rust` speedup per (engine,
//! workload). If Python Lark is unavailable the Rust numbers still print, with a note.
//!
//! Wired as `harness = false` in Cargo.toml, so `main()` runs directly. Like
//! `parse.rs` it uses a self-contained `std::time` loop, not a benchmarking crate —
//! a recorded trend, not a CI gate (wall-clock on shared runners is too noisy to
//! gate; see `BENCH.md`).

use lark_rs::{Indenter, Lark, LarkOptions, LexerType, ParserAlgorithm};
use std::collections::HashMap;
use std::hint::black_box;
use std::io::Write;
use std::time::{Duration, Instant};

// --- Grammars — byte-identical to benches/vs_python_lark.py ------------------

const JSON_GRAMMAR: &str = r#"
    ?start: value
    ?value: object
          | array
          | string
          | SIGNED_NUMBER  -> number
          | "true"         -> true
          | "false"        -> false
          | "null"         -> null
    array  : "[" [value ("," value)*] "]"
    object : "{" [pair ("," pair)*] "}"
    pair   : string ":" value
    string : ESCAPED_STRING
    %import common.ESCAPED_STRING
    %import common.SIGNED_NUMBER
    %import common.WS
    %ignore WS
"#;

const PY_GRAMMAR: &str = r#"
start: _NL? stmt*
?stmt: simple_stmt | compound_stmt
simple_stmt: expr_stmt _NL
?expr_stmt: expr ("=" expr)* -> assign
          | "return" [expr]  -> return_stmt
          | "pass"           -> pass_stmt
?compound_stmt: func_def | class_def | if_stmt | for_stmt | while_stmt
func_def: "def" NAME "(" [params] ")" ":" suite
class_def: "class" NAME ["(" [arglist] ")"] ":" suite
if_stmt: "if" expr ":" suite ("elif" expr ":" suite)* ["else" ":" suite]
for_stmt: "for" NAME "in" expr ":" suite
while_stmt: "while" expr ":" suite
suite: _NL _INDENT stmt+ _DEDENT
params: NAME ("," NAME)*
arglist: expr ("," expr)*
?expr: or_test
?or_test: and_test ("or" and_test)*
?and_test: comparison ("and" comparison)*
?comparison: arith (comp_op arith)*
comp_op: "==" | "!=" | "<" | ">" | "<=" | ">="
?arith: term (("+"|"-") term)*
?term: factor (("*"|"/"|"%") factor)*
?factor: "-" factor | power
?power: trailer ("**" factor)?
?trailer: trailer "(" [arglist] ")" -> call
        | trailer "." NAME           -> getattr
        | trailer "[" expr "]"        -> getitem
        | atom
?atom: NAME | NUMBER | STRING | "True" | "False" | "None"
     | "(" expr ")"
     | "[" [arglist] "]" -> list
     | "{" [pair ("," pair)*] "}" -> dict
pair: expr ":" expr
LPAR: "("
RPAR: ")"
LSQB: "["
RSQB: "]"
LBRACE: "{"
RBRACE: "}"
NAME: /[a-zA-Z_]\w*/
NUMBER: /\d+(\.\d+)?/
STRING: /"[^"\n]*"/ | /'[^'\n]*'/
COMMENT: /#[^\n]*/
_NL: /(\r?\n[\t ]*)+/
%ignore /[\t ]+/
%ignore COMMENT
%declare _INDENT _DEDENT
"#;

const SQL_GRAMMAR: &str = r#"
start: (stmt ";")+
?stmt: select_stmt | insert_stmt | update_stmt | delete_stmt
select_stmt: "SELECT" select_list "FROM" table_ref join* where_clause? group_by? order_by? limit_clause?
insert_stmt: "INSERT" "INTO" NAME "(" name_list ")" "VALUES" "(" value_list ")"
update_stmt: "UPDATE" NAME "SET" assignment ("," assignment)* where_clause?
delete_stmt: "DELETE" "FROM" NAME where_clause?
assignment: NAME "=" value
select_list: "*" | expr ("," expr)*
name_list: NAME ("," NAME)*
value_list: value ("," value)*
table_ref: NAME [NAME]
join: ("INNER" | "LEFT" | "RIGHT")? "JOIN" table_ref "ON" condition
where_clause: "WHERE" condition
group_by: "GROUP" "BY" expr ("," expr)*
order_by: "ORDER" "BY" order_term ("," order_term)*
order_term: expr ("ASC" | "DESC")?
limit_clause: "LIMIT" NUMBER
?condition: or_cond
?or_cond: and_cond ("OR" and_cond)*
?and_cond: comparison ("AND" comparison)*
?comparison: expr COMP_OP expr
           | expr "BETWEEN" expr "AND" expr -> between
           | expr "IN" "(" value_list ")"   -> in_list
           | "(" condition ")"
?expr: term (("+"|"-") term)*
?term: factor (("*"|"/") factor)*
?factor: NUMBER | STRING | column_ref | func_call | "(" expr ")"
column_ref: NAME ("." NAME)?
func_call: NAME "(" (select_list)? ")"
?value: NUMBER | STRING | "NULL" | "TRUE" | "FALSE"
COMP_OP: "=" | "!=" | "<>" | "<=" | ">=" | "<" | ">"
NAME: /[a-zA-Z_]\w*/
NUMBER: /\d+(\.\d+)?/
STRING: /'[^']*'/
COMMENT: /--[^\n]*/
%import common.WS
%ignore WS
%ignore COMMENT
"#;

// --- Input generators — mirror benches/vs_python_lark.py byte-for-byte -------

/// A JSON array of `records` flat objects, each with `fields` key/value pairs
/// (identical to `parse.rs`'s generator). ~92 KB at (512, 5).
fn gen_json(records: usize, fields: usize) -> String {
    let mut s = String::from("[");
    for r in 0..records {
        if r > 0 {
            s.push(',');
        }
        s.push('{');
        for f in 0..fields {
            if f > 0 {
                s.push_str(", ");
            }
            s.push_str(&format!(
                "\"key{f}\": {}, \"name{f}\": \"value{r}_{f}\"",
                r * 10 + f
            ));
        }
        s.push('}');
    }
    s.push(']');
    s
}

/// `classes` repeated Python class blocks — methods with if/else, for loops,
/// arithmetic, attribute access — exercising the Indenter (INDENT/DEDENT).
fn gen_python(classes: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    for c in 0..classes {
        lines.push(format!("class Account{c}:"));
        lines.push("    def __init__(self, owner, balance):".into());
        lines.push("        self.owner = owner".into());
        lines.push("        self.balance = balance".into());
        lines.push("".into());
        lines.push("    def deposit(self, amount):".into());
        lines.push("        if amount > 0:".into());
        lines.push("            self.balance = self.balance + amount".into());
        lines.push("            return self.balance".into());
        lines.push("        else:".into());
        lines.push("            return None".into());
        lines.push("".into());
        lines.push("    def summarize(self, items):".into());
        lines.push("        total = 0".into());
        lines.push("        for it in items:".into());
        lines.push("            total = total + it * 2".into());
        lines.push("            if total > 100:".into());
        lines.push("                total = total - 1".into());
        lines.push("        return total".into());
        lines.push("".into());
    }
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

/// `statements` SQL statements cycling through SELECT/INSERT/UPDATE/DELETE/JOIN.
fn gen_sql(statements: usize) -> String {
    const TEMPLATES: [&str; 7] = [
        "SELECT id, name, email FROM users WHERE age >= {n} AND status = 'active' ORDER BY name ASC LIMIT 100",
        "SELECT u.name, o.total FROM users u INNER JOIN orders o ON u.id = o.user_id WHERE o.total > {n} ORDER BY o.total DESC",
        "INSERT INTO products (id, name, price) VALUES ({n}, 'Widget', 9)",
        "UPDATE accounts SET balance = {n}, status = 'ok' WHERE id = {n}",
        "DELETE FROM sessions WHERE id = {n}",
        "SELECT COUNT(id), category FROM products WHERE price BETWEEN 10 AND {n} GROUP BY category ORDER BY category",
        "SELECT * FROM logs WHERE level IN ('warn', 'error') AND service = 'api' AND id > {n}",
    ];
    let mut lines: Vec<String> = Vec::new();
    for i in 0..statements {
        let n = (i % 900 + 1).to_string();
        lines.push(format!(
            "{};",
            TEMPLATES[i % TEMPLATES.len()].replace("{n}", &n)
        ));
    }
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

// --- Timing harness (mirrors parse.rs) ---------------------------------------

struct Stat {
    min_ns: f64,
    median_ns: f64,
}

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

fn python_indenter() -> Indenter {
    Indenter {
        nl_type: "_NL".to_string(),
        open_paren_types: vec!["LPAR".into(), "LSQB".into(), "LBRACE".into()],
        close_paren_types: vec!["RPAR".into(), "RSQB".into(), "RBRACE".into()],
        indent_type: "_INDENT".to_string(),
        dedent_type: "_DEDENT".to_string(),
        tab_len: 8,
    }
}

#[derive(Clone, Copy)]
enum Algo {
    Lalr,
    Earley,
}

impl Algo {
    fn tag(self) -> &'static str {
        match self {
            Algo::Lalr => "lalr",
            Algo::Earley => "earley",
        }
    }
}

fn lexer_name(l: &LexerType) -> &'static str {
    match l {
        LexerType::Basic => "basic",
        LexerType::Contextual => "contextual",
        LexerType::Dynamic => "dynamic",
        LexerType::DynamicComplete => "dynamic_complete",
        _ => "auto",
    }
}

/// One (workload, engine) configuration both lark-rs and Python Lark can run.
/// The list is mirrored in `benches/vs_python_lark.py`.
struct Config {
    name: &'static str,
    algo: Algo,
    lexer: LexerType,
    grammar: &'static str,
    postlex: bool,
}

/// The LALR row uses the contextual lexer (Lark's USP) on all three workloads.
/// The Earley row covers the two workloads Earley can run cross-engine: JSON
/// (basic lexer) and SQL (the *dynamic* lexer — the basic lexer can't tell the
/// assignment `=` from the comparison `=` here, in either engine). Python is
/// **omitted under Earley** (postlex is LALR-only); see the module header.
fn configs() -> Vec<Config> {
    vec![
        Config {
            name: "json",
            algo: Algo::Lalr,
            lexer: LexerType::Contextual,
            grammar: JSON_GRAMMAR,
            postlex: false,
        },
        Config {
            name: "python",
            algo: Algo::Lalr,
            lexer: LexerType::Contextual,
            grammar: PY_GRAMMAR,
            postlex: true,
        },
        Config {
            name: "sql",
            algo: Algo::Lalr,
            lexer: LexerType::Contextual,
            grammar: SQL_GRAMMAR,
            postlex: false,
        },
        Config {
            name: "json",
            algo: Algo::Earley,
            lexer: LexerType::Basic,
            grammar: JSON_GRAMMAR,
            postlex: false,
        },
        Config {
            name: "sql",
            algo: Algo::Earley,
            lexer: LexerType::Dynamic,
            grammar: SQL_GRAMMAR,
            postlex: false,
        },
    ]
}

fn build(cfg: &Config) -> Lark {
    let mut opts = LarkOptions {
        start: vec!["start".to_string()],
        parser: match cfg.algo {
            Algo::Lalr => ParserAlgorithm::Lalr,
            Algo::Earley => ParserAlgorithm::Earley,
        },
        lexer: cfg.lexer.clone(),
        ..LarkOptions::default()
    };
    if cfg.postlex {
        opts.postlex = Some(python_indenter());
    }
    Lark::new(cfg.grammar, opts)
        .unwrap_or_else(|e| panic!("[{}/{}] grammar must build: {e}", cfg.algo.tag(), cfg.name))
}

/// One workload's lark-rs result.
struct RustResult {
    bytes: usize,
    median_ns: f64,
    mb_per_s: f64,
}

fn run_rust(cfg: &Config, input: &str) -> RustResult {
    let parser = build(cfg);
    let bytes = input.len();
    parser.parse(input).unwrap_or_else(|e| {
        panic!(
            "[{}/{}] workload must parse in lark-rs: {e}",
            cfg.algo.tag(),
            cfg.name
        )
    });
    let stat = measure(|| {
        black_box(parser.parse(black_box(input)).expect("must parse"));
    });
    let mb_per_s = bytes as f64 / stat.median_ns * 1e3;
    let (algo, name) = (cfg.algo.tag(), cfg.name);
    println!(
        "BENCH\trust\t{algo}\t{name}\t{bytes}\t{:.0}\t{:.0}\t{mb_per_s:.1}",
        stat.median_ns, stat.min_ns
    );
    println!(
        "  rust {algo:<7} {name:<7} {:<11} {bytes:>8} B   {:>12.0} ns/iter (min {:>12.0})   {mb_per_s:>7.1} MB/s",
        lexer_name(&cfg.lexer),
        stat.median_ns,
        stat.min_ns
    );
    RustResult {
        bytes,
        median_ns: stat.median_ns,
        mb_per_s,
    }
}

/// One Python Lark result, keyed by `"<algo>/<name>"`.
struct PyResult {
    median_ns: f64,
    mb_per_s: f64,
}

/// Write the inputs to a temp dir and shell out to the Python timing script.
/// Returns the per-config Python results (keyed `"<algo>/<name>"`), or `None` if
/// Python Lark is unavailable.
fn run_python(inputs: &[(&str, &str)]) -> Option<HashMap<String, PyResult>> {
    let dir = std::env::temp_dir().join("lark_rs_vs_python");
    std::fs::create_dir_all(&dir).ok()?;
    for (name, text) in inputs {
        let mut f = std::fs::File::create(dir.join(format!("{name}.txt"))).ok()?;
        f.write_all(text.as_bytes()).ok()?;
    }

    let script = format!("{}/benches/vs_python_lark.py", env!("CARGO_MANIFEST_DIR"));
    let output = std::process::Command::new("python3")
        .arg(&script)
        .arg("--inputs")
        .arg(&dir)
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            eprintln!(
                "  (Python Lark comparison skipped — script failed:\n{})",
                String::from_utf8_lossy(&o.stderr).trim()
            );
            return None;
        }
        Err(e) => {
            eprintln!("  (Python Lark comparison skipped — could not run python3: {e})");
            return None;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut results = HashMap::new();
    for line in stdout.lines() {
        // PYBENCH<TAB>algo<TAB>name<TAB>bytes<TAB>median_ns<TAB>min_ns<TAB>mb_per_s
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.first() != Some(&"PYBENCH") || cols.len() < 7 {
            continue;
        }
        let (algo, name) = (cols[1], cols[2]);
        let median_ns: f64 = cols[4].parse().ok()?;
        let mb_per_s: f64 = cols[6].parse().ok()?;
        println!(
            "  python {algo:<7} {name:<7} {:>20} B   {median_ns:>12.0} ns/iter                          {mb_per_s:>7.1} MB/s",
            cols[3]
        );
        results.insert(
            format!("{algo}/{name}"),
            PyResult {
                median_ns,
                mb_per_s,
            },
        );
    }
    if results.is_empty() {
        eprintln!("  (Python Lark comparison skipped — no PYBENCH lines in output)");
        return None;
    }
    Some(results)
}

fn main() {
    println!("# lark-rs vs Python Lark — cross-engine throughput (JSON / Python / SQL)");
    println!(
        "# columns: BENCH<TAB>engine<TAB>algo<TAB>name<TAB>bytes<TAB>median_ns<TAB>min_ns<TAB>mb_per_s"
    );
    println!();

    let inputs: HashMap<&str, String> = HashMap::from([
        ("json", gen_json(512, 5)),
        ("python", gen_python(220)),
        ("sql", gen_sql(700)),
    ]);

    let cfgs = configs();

    println!("lark-rs (build once, parse many):");
    let mut rust: HashMap<String, RustResult> = HashMap::new();
    for cfg in &cfgs {
        let r = run_rust(cfg, &inputs[cfg.name]);
        rust.insert(format!("{}/{}", cfg.algo.tag(), cfg.name), r);
    }
    println!();

    println!("Python Lark (in-tree, same grammars + byte-identical inputs):");
    let py = run_python(&[
        ("json", &inputs["json"]),
        ("python", &inputs["python"]),
        ("sql", &inputs["sql"]),
    ]);
    println!();

    // --- Combined speedup table ----------------------------------------------
    println!("Speedup (python_median / rust_median):");
    println!(
        "  {:<7} {:<7} {:>12} {:>12} {:>10}",
        "algo", "workload", "rust MB/s", "python MB/s", "speedup"
    );
    for cfg in &cfgs {
        let key = format!("{}/{}", cfg.algo.tag(), cfg.name);
        let r = &rust[&key];
        match py.as_ref().and_then(|m| m.get(&key)) {
            Some(p) => {
                let speedup = p.median_ns / r.median_ns;
                println!(
                    "  {:<7} {:<7} {:>12.1} {:>12.1} {speedup:>9.1}x",
                    cfg.algo.tag(),
                    cfg.name,
                    r.mb_per_s,
                    p.mb_per_s
                );
                println!(
                    "BENCH\tspeedup\t{}\t{}\t{}\t{speedup:.2}\t0\t0",
                    cfg.algo.tag(),
                    cfg.name,
                    r.bytes
                );
            }
            None => println!(
                "  {:<7} {:<7} {:>12.1} {:>12} {:>10}",
                cfg.algo.tag(),
                cfg.name,
                r.mb_per_s,
                "n/a",
                "n/a"
            ),
        }
    }
    if py.is_none() {
        println!("  (run `python3 benches/vs_python_lark.py` separately for the Python side)");
    }
}
