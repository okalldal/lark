//! Cross-engine end-to-end throughput: lark-rs vs Python Lark (issue #50, Phase 4).
//!
//! The existing `benches/parse.rs` measures lark-rs *internally* (LALR vs Earley,
//! scaling shapes). This bench is the **cross-engine comparison** — the number
//! behind the project's "10–100× faster than Python Lark" goal — over three real
//! workloads, all on LALR + the contextual lexer (Lark's primary USP):
//!
//!   1. **JSON**   — the canonical JSON grammar on a ~92 KB array of records.
//!   2. **Python** — a significant-whitespace Python subset (driven by the
//!                   `Indenter` postlex hook) over a representative source file.
//!   3. **SQL**    — a SELECT/INSERT/UPDATE/DELETE grammar over a batch of statements.
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
//! prints MB/s for each engine and the `python / rust` speedup per workload. If
//! Python Lark is unavailable the Rust numbers still print, with a note.
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

fn lalr_options() -> LarkOptions {
    LarkOptions {
        start: vec!["start".to_string()],
        parser: ParserAlgorithm::Lalr,
        lexer: LexerType::Contextual,
        ..LarkOptions::default()
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

/// One workload's lark-rs result.
struct RustResult {
    bytes: usize,
    median_ns: f64,
    mb_per_s: f64,
}

fn run_rust(name: &str, parser: &Lark, input: &str) -> RustResult {
    let bytes = input.len();
    parser
        .parse(input)
        .unwrap_or_else(|e| panic!("[{name}] workload must parse in lark-rs: {e}"));
    let stat = measure(|| {
        black_box(parser.parse(black_box(input)).expect("must parse"));
    });
    let mb_per_s = bytes as f64 / stat.median_ns * 1e3;
    println!(
        "BENCH\trust\t{name}\t{bytes}\t{:.0}\t{:.0}\t{mb_per_s:.1}",
        stat.median_ns, stat.min_ns
    );
    println!(
        "  rust   {name:<8} {bytes:>8} B   {:>12.0} ns/iter (min {:>12.0})   {mb_per_s:>7.1} MB/s",
        stat.median_ns, stat.min_ns
    );
    RustResult {
        bytes,
        median_ns: stat.median_ns,
        mb_per_s,
    }
}

/// Python Lark's parsed result line (`PYBENCH<TAB>name<TAB>bytes<TAB>median_ns<TAB>min_ns<TAB>mb_per_s`).
struct PyResult {
    median_ns: f64,
    mb_per_s: f64,
}

/// Write the inputs to a temp dir and shell out to the Python timing script.
/// Returns the per-workload Python results, or `None` if Python Lark is unavailable.
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
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.first() != Some(&"PYBENCH") || cols.len() < 6 {
            continue;
        }
        let name = cols[1].to_string();
        let median_ns: f64 = cols[3].parse().ok()?;
        let mb_per_s: f64 = cols[5].parse().ok()?;
        println!(
            "  python {name:<8} {:>8} B   {median_ns:>12.0} ns/iter                          {mb_per_s:>7.1} MB/s",
            cols[2]
        );
        results.insert(
            name,
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
    println!("# lark-rs vs Python Lark — cross-engine throughput (LALR + contextual lexer)");
    println!(
        "# columns: BENCH<TAB>engine<TAB>name<TAB>bytes<TAB>median_ns<TAB>min_ns<TAB>mb_per_s"
    );
    println!();

    let json_input = gen_json(512, 5);
    let python_input = gen_python(220);
    let sql_input = gen_sql(700);

    let json = Lark::new(JSON_GRAMMAR, lalr_options()).expect("JSON grammar must build");
    let sql = Lark::new(SQL_GRAMMAR, lalr_options()).expect("SQL grammar must build");
    let python = Lark::new(
        PY_GRAMMAR,
        LarkOptions {
            postlex: Some(python_indenter()),
            ..lalr_options()
        },
    )
    .expect("Python grammar must build");

    println!("lark-rs (build once, parse many):");
    let mut rust: HashMap<&str, RustResult> = HashMap::new();
    rust.insert("json", run_rust("json", &json, &json_input));
    rust.insert("python", run_rust("python", &python, &python_input));
    rust.insert("sql", run_rust("sql", &sql, &sql_input));
    println!();

    println!("Python Lark (in-tree, same grammars + byte-identical inputs):");
    let py = run_python(&[
        ("json", &json_input),
        ("python", &python_input),
        ("sql", &sql_input),
    ]);
    println!();

    // --- Combined speedup table ----------------------------------------------
    println!("Speedup (python_median / rust_median):");
    println!(
        "  {:<8} {:>12} {:>12} {:>10}",
        "workload", "rust MB/s", "python MB/s", "speedup"
    );
    if let Some(py) = py {
        for name in ["json", "python", "sql"] {
            let r = &rust[name];
            if let Some(p) = py.get(name) {
                let speedup = p.median_ns / r.median_ns;
                println!(
                    "  {name:<8} {:>12.1} {:>12.1} {speedup:>9.1}x",
                    r.mb_per_s, p.mb_per_s
                );
                println!("BENCH\tspeedup\t{name}\t{}\t{speedup:.2}\t0\t0", r.bytes);
            }
        }
    } else {
        for name in ["json", "python", "sql"] {
            let r = &rust[name];
            println!(
                "  {name:<8} {:>12.1} {:>12} {:>10}",
                r.mb_per_s, "n/a", "n/a"
            );
        }
        println!("  (run `python3 benches/vs_python_lark.py` separately for the Python side)");
    }
}
