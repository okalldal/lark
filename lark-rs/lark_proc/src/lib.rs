//! `include_lark!` — compile-time grammar validation for [`lark-rs`].
//!
//! ```ignore
//! use lark_proc::include_lark;
//!
//! include_lark!("grammars/json.lark");          // -> a `JsonParser` struct
//! // include_lark!("grammars/json.lark", Json);  // -> a `Json` struct (explicit name)
//!
//! let parser = JsonParser::new();
//! let tree = parser.parse(r#"{"a": [1, 2, true]}"#)?;
//! ```
//!
//! The macro reads the `.lark` file *while the calling crate compiles* and bakes it
//! through the real lark-rs pipeline (the same [`lark_rs::generate_standalone`]
//! emitter the `generate-parser` CLI uses). A malformed grammar (unknown terminal,
//! LALR conflict, syntax error, …) is reported as a `cargo build` compiler error,
//! not a runtime panic — the headline of issue #49. It expands to a zero-field
//! parser struct whose `parse(&str)` method drives a *baked* LALR `ParseTable` +
//! lexer `ScannerPlan` embedded inline as `const GrammarData` — no table is built
//! at runtime (issue #85), closing #49's own "bake the ParseTable into const data"
//! follow-up. The parser is built once per thread via a `thread_local!` cache (its
//! scanner compiles the combined regex on first use and holds it).
//!
//! ## Path resolution
//!
//! The path is resolved relative to the calling crate's `CARGO_MANIFEST_DIR`
//! (the directory holding its `Cargo.toml`), the same convention `sqlx::query!`
//! and friends use. The grammar file is tracked via `include_bytes!`, so editing
//! it re-triggers compilation.
//!
//! ## Requirements on the caller
//!
//! The baked parser is **self-contained**: it depends only on the `regex` crate and
//! the Rust standard library, not on `lark-rs`. A crate using `include_lark!` need
//! only have `regex` as a dependency (the standalone backend's contract).
//!
//! ## Scope (inherited from the standalone backend)
//!
//! The baked artifact is the standalone parser (see [`lark_rs::standalone`]), so the
//! macro is **LALR + basic-lexer only**: no Earley/CYK, no postlex (`Indenter`), and
//! no grammars whose terminals use lookaround. A non-LALR parser, a postlex hook, a
//! lookaround terminal, or a zero-width terminal is rejected at compile time with
//! the same `cargo build` error #49 introduced.
//!
//! One narrowing from the previous version is worth calling out: the old macro built
//! a default `lark_rs::Lark`, which uses the **contextual** lexer (Lark's USP for
//! resolving LALR terminal conflicts). The baked standalone parser uses the **basic**
//! lexer. A grammar that *relies on* the contextual lexer to disambiguate terminals
//! is **not** rejected at compile time (the LALR tables still build) — exactly as
//! Python Lark's own `standalone` tool, it will instead fail to lex (or mis-lex) at
//! the user's runtime. Disambiguate with explicit terminal priority if you need it
//! (the project's standing discipline; see `lark-rs/CLAUDE.md`).
//!
//! ## Generated surface
//!
//! The generated struct's `parse`/`parse_with_start` return
//! `Result<ParseTree, String>` over the embedded standalone runtime's
//! `Tree`/`Token`/`Child` types (re-exported on the struct's own module as
//! `Tree`, `Token`, `Child`, `ParseTree`), *not* `lark_rs::ParseTree`. This is the
//! self-contained-export model the standalone backend already ships.

use proc_macro::{TokenStream, TokenTree};
use std::path::PathBuf;

/// Validate a `.lark` grammar at compile time and generate a typed parser.
///
/// See the crate-level docs for usage. Accepts either:
/// - `include_lark!("path/to/grammar.lark")` — the struct name is derived from the
///   file stem (`json.lark` → `JsonParser`), or
/// - `include_lark!("path/to/grammar.lark", MyName)` — an explicit struct name.
#[proc_macro]
pub fn include_lark(input: TokenStream) -> TokenStream {
    match expand(input) {
        Ok(ts) => ts,
        Err(msg) => compile_error(&msg),
    }
}

/// Parse the macro input, load+validate the grammar, and emit the parser struct.
/// Any failure is returned as a human-readable string that becomes a
/// `compile_error!`.
fn expand(input: TokenStream) -> Result<TokenStream, String> {
    let Args { rel_path, name } = parse_args(input)?;

    // Resolve relative to the *calling* crate's manifest dir — Cargo sets
    // CARGO_MANIFEST_DIR to the crate currently being compiled, which is the one
    // invoking the macro.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").map_err(|_| {
        "CARGO_MANIFEST_DIR is not set; include_lark! must be expanded by a Cargo build".to_string()
    })?;
    let abs_path = PathBuf::from(manifest_dir).join(&rel_path);

    let source = std::fs::read_to_string(&abs_path).map_err(|e| {
        format!(
            "include_lark!: cannot read grammar file {}: {e}",
            abs_path.display()
        )
    })?;

    // The whole point of the macro: surface grammar errors at compile time. We bake
    // the parser through the *same* standalone emitter the `generate-parser` CLI
    // uses (issue #85) — so anything that would fail at runtime (unknown terminal,
    // LALR conflict, bad regex, unsupported backend, …) fails the build instead,
    // attributed to this file. The baked `GrammarData` literal + runtime is emitted
    // inline; nothing constructs a `lark_rs::Lark` at runtime (closes #49's
    // const-table-bake follow-up).
    let base_path = abs_path.parent().map(|p| p.to_path_buf());
    let options = lark_rs::LarkOptions {
        base_path,
        ..Default::default()
    };
    let baked = lark_rs::generate_standalone(&source, &options).map_err(|e| {
        format!(
            "include_lark!: grammar {} is invalid:\n{e}",
            abs_path.display()
        )
    })?;

    // Determine the struct name: explicit, or `<FileStem>Parser`.
    let struct_name = match name {
        Some(n) => n,
        None => default_struct_name(&rel_path)?,
    };

    Ok(generate(&struct_name, &source, &baked, &abs_path))
}

struct Args {
    rel_path: String,
    name: Option<String>,
}

/// Parse `"path"` or `"path", Ident` out of the raw macro token stream. We do this
/// by hand (no `syn`) to keep this a zero-dependency proc-macro crate.
fn parse_args(input: TokenStream) -> Result<Args, String> {
    let mut it = input.into_iter();

    let rel_path = match it.next() {
        Some(TokenTree::Literal(lit)) => {
            string_literal_value(&lit.to_string()).ok_or_else(|| {
                "include_lark! expects a string-literal path as its first argument".to_string()
            })?
        }
        Some(other) => {
            return Err(format!(
                "include_lark! expects a string-literal path, found `{other}`"
            ))
        }
        None => return Err("include_lark! requires a grammar-file path argument".to_string()),
    };

    // Optional `, Name`.
    let name = match it.next() {
        None => None,
        Some(TokenTree::Punct(p)) if p.as_char() == ',' => match it.next() {
            Some(TokenTree::Ident(id)) => Some(id.to_string()),
            Some(other) => {
                return Err(format!(
                    "include_lark!: expected a struct-name identifier after `,`, found `{other}`"
                ))
            }
            None => {
                return Err(
                    "include_lark!: trailing `,` — expected a struct-name identifier".to_string(),
                )
            }
        },
        Some(other) => {
            return Err(format!(
                "include_lark!: unexpected token after path: `{other}` (expected `, Name`)"
            ))
        }
    };

    if name.is_some() && it.next().is_some() {
        return Err(
            "include_lark!: too many arguments (expected `\"path\"` or `\"path\", Name`)"
                .to_string(),
        );
    }

    Ok(Args { rel_path, name })
}

/// Extract the value of a string literal token as rendered by `Literal::to_string`
/// — either `"…"` or a raw string `r#"…"#`. Returns `None` for non-string tokens
/// (e.g. a numeric or char literal).
fn string_literal_value(rendered: &str) -> Option<String> {
    let bytes = rendered.as_bytes();
    if bytes.first() == Some(&b'"') && bytes.last() == Some(&b'"') {
        // Ordinary string literal. Unescape the few sequences a path realistically
        // contains; anything exotic is unlikely in a file path.
        return Some(unescape(&rendered[1..rendered.len() - 1]));
    }
    // Raw string: r"...", r#"..."#, r##"..."##, ...
    if bytes.first() == Some(&b'r') {
        let hashes = rendered[1..].bytes().take_while(|&b| b == b'#').count();
        let open = 1 + hashes + 1; // r + hashes + opening quote
        let close = rendered.len() - hashes - 1; // closing quote position
        if open <= close && rendered.as_bytes().get(open - 1) == Some(&b'"') {
            return Some(rendered[open..close].to_string());
        }
    }
    None
}

/// Minimal unescaping for the common escapes that can appear in a quoted path.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some('0') => out.push('\0'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `grammars/json.lark` → `JsonParser`. Splits the file stem on non-alphanumeric
/// boundaries and PascalCases the pieces, then appends `Parser`.
fn default_struct_name(rel_path: &str) -> Result<String, String> {
    let stem = std::path::Path::new(rel_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("include_lark!: cannot derive a name from path {rel_path:?}"))?;

    let mut name = String::new();
    let mut new_word = true;
    for ch in stem.chars() {
        if ch.is_alphanumeric() {
            if new_word {
                name.extend(ch.to_uppercase());
                new_word = false;
            } else {
                name.push(ch);
            }
        } else {
            new_word = true;
        }
    }
    if name.is_empty() || name.chars().next().unwrap().is_numeric() {
        return Err(format!(
            "include_lark!: cannot derive a valid Rust identifier from path {rel_path:?}; \
             pass an explicit name: include_lark!({rel_path:?}, MyParser)"
        ));
    }
    name.push_str("Parser");
    Ok(name)
}

/// Emit the generated parser struct. `baked` is the self-contained standalone
/// source produced by [`lark_rs::generate_standalone`] (the *same* emitter the
/// `generate-parser` CLI uses, issue #85): a `pub mod parser {{ … }}` carrying the
/// embedded runtime, the baked `static DATA: GrammarData`, and a `Parser::new()`
/// that calls `Parser::from_data(&DATA)`. We wrap it in a uniquely-named private
/// module and delegate the public struct's methods to that module's `Parser`, so
/// no `ParseTable` is built at runtime. The grammar source is embedded as a string
/// literal for `GRAMMAR`; `include_bytes!` ties the build to the grammar file so
/// edits force a rebuild.
fn generate(
    struct_name: &str,
    source: &str,
    baked: &str,
    abs_path: &std::path::Path,
) -> TokenStream {
    // `{:?}` renders a correctly escaped Rust string literal for both the grammar
    // body and the absolute path.
    let grammar_lit = format!("{source:?}");
    let path_lit = format!("{:?}", abs_path.to_string_lossy());
    // A module name unique per struct, so multiple `include_lark!` calls in one
    // scope don't collide on the baked `parser` module. We derive it from the struct
    // name *verbatim* (not lowercased): two calls with the same struct name already
    // collide on the struct definition, so a 1:1 mapping keeps the module unique
    // too — lowercasing would alias distinct names (`Json`/`JSON`) onto one module.
    // It is `pub` so the caller can name the runtime's tree types (e.g.
    // `__lark_baked_<Name>::ParseTree`) to `match` on a parse result — the standalone
    // runtime defines its own types, distinct per grammar, so they live behind this
    // module rather than being re-exported (which would collide across grammars).
    let module = format!("__lark_baked_{struct_name}");

    let code = format!(
        r#"
/// The self-contained standalone parser baked from this grammar (the `GrammarData`
/// literal + embedded runtime), emitted by the same `lark_rs::generate_standalone`
/// the `generate-parser` CLI uses (issue #85). Depends only on `regex` + std, not on
/// lark-rs. Its ParseTree/Tree/Token/Child are the types a parse returns.
#[allow(non_snake_case)]
pub mod {module} {{
{baked}
}}

#[derive(Debug, Clone, Copy, Default)]
pub struct {name};

impl {name} {{
    /// The embedded grammar source, baked at compile time by `include_lark!`.
    pub const GRAMMAR: &'static str = {grammar};

    /// Create a handle to the parser. The baked `Parser` (its lexer regex) is built
    /// once per thread on the first `parse` call and then reused.
    pub fn new() -> Self {{
        {name}
    }}

    /// Run `f` with the thread-local, lazily-built standalone `Parser`.
    fn with_parser<R>(f: impl FnOnce(&{module}::Parser) -> R) -> R {{
        // The baked `Parser` holds a compiled `regex::Regex`; build it once per
        // thread (matches the prior caching contract; `Parser::from_data` is cheap
        // table-wiring plus that one regex compile).
        ::std::thread_local! {{
            static __PARSER: {module}::Parser = {module}::Parser::new();
        }}
        __PARSER.with(|p| f(p))
    }}

    /// Parse `input` from the grammar's start symbol.
    pub fn parse(
        &self,
        input: &str,
    ) -> ::core::result::Result<{module}::ParseTree, ::std::string::String> {{
        Self::with_parser(|p| p.parse(input))
    }}

    /// Parse `input` from an explicit start symbol.
    pub fn parse_with_start(
        &self,
        input: &str,
        start: &str,
    ) -> ::core::result::Result<{module}::ParseTree, ::std::string::String> {{
        Self::with_parser(|p| p.parse_from(input, ::core::option::Option::Some(start)))
    }}
}}

// Tie the build to the grammar file so editing it forces a recompile.
const _: &[u8] = include_bytes!({path});
"#,
        name = struct_name,
        module = module,
        baked = baked,
        grammar = grammar_lit,
        path = path_lit,
    );

    code.parse().expect("generated code is valid Rust")
}

/// Render a `compile_error!("…")` invocation carrying `msg`.
fn compile_error(msg: &str) -> TokenStream {
    format!("compile_error!({:?});", msg)
        .parse()
        .expect("compile_error! invocation is valid Rust")
}
