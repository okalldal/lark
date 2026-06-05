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
//! The macro reads the `.lark` file *while the calling crate compiles* and runs it
//! through the real [`lark_rs::Lark`] loader. A malformed grammar (unknown
//! terminal, LALR conflict, syntax error, …) is reported as a `cargo build`
//! compiler error, not a runtime panic — the headline of issue #49. It expands to
//! a zero-field parser struct whose `parse(&str)` method builds the underlying
//! [`lark_rs::Lark`] lazily from the embedded grammar source and then reuses it
//! for every call via a `thread_local!` cache — `lark_rs::Lark` holds a `RefCell`
//! scratch buffer in its scanner and so is not `Sync`, so the built parser is
//! cached once per thread rather than process-wide.
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
//! The generated code names `::lark_rs`, so any crate using `include_lark!` must
//! also depend on `lark-rs` (just as `serde_derive` users depend on `serde`).
//!
//! ## Status
//!
//! v1 validates the grammar at compile time and embeds the grammar *source*,
//! building the parse table once at first use. Baking the LALR `ParseTable` itself
//! into `const` data (so zero table construction happens at runtime) is a tracked
//! follow-up; the regex-based lexer always compiles its patterns at runtime
//! regardless, so this affects only first-use latency, not correctness.

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

    // The whole point of the macro: surface grammar errors at compile time. We
    // build the real parser the generated code would build, so anything that would
    // fail at runtime (unknown terminal, LALR conflict, bad regex, …) fails the
    // build instead, attributed to this file.
    let base_path = abs_path.parent().map(|p| p.to_path_buf());
    let options = lark_rs::LarkOptions {
        base_path,
        ..Default::default()
    };
    if let Err(e) = lark_rs::Lark::new(&source, options) {
        return Err(format!(
            "include_lark!: grammar {} is invalid:\n{e}",
            abs_path.display()
        ));
    }

    // Determine the struct name: explicit, or `<FileStem>Parser`.
    let struct_name = match name {
        Some(n) => n,
        None => default_struct_name(&rel_path)?,
    };

    Ok(generate(&struct_name, &source, &abs_path))
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

/// Emit the generated parser struct. The grammar source is embedded as a string
/// literal; `Lark` is built once via `OnceLock`. `include_bytes!` ties the build
/// to the grammar file so edits force a rebuild.
fn generate(struct_name: &str, source: &str, abs_path: &std::path::Path) -> TokenStream {
    // `{:?}` renders a correctly escaped Rust string literal for both the grammar
    // body and the absolute path.
    let grammar_lit = format!("{source:?}");
    let path_lit = format!("{:?}", abs_path.to_string_lossy());

    let code = format!(
        r#"
#[derive(Debug, Clone, Copy, Default)]
pub struct {name};

impl {name} {{
    /// The embedded grammar source, validated at compile time by `include_lark!`.
    pub const GRAMMAR: &'static str = {grammar};

    /// Create a handle to the parser. Building the underlying `lark_rs::Lark` is
    /// deferred to the first `parse` call on this thread (and then cached).
    pub fn new() -> Self {{
        {name}
    }}

    /// Run `f` with the thread-local, lazily-built `lark_rs::Lark` instance.
    fn with_lark<R>(f: impl FnOnce(&::lark_rs::Lark) -> R) -> R {{
        // `lark_rs::Lark` is not `Sync` (its scanner holds a `RefCell` scratch
        // buffer), so the parser is cached per thread, built once on first use.
        ::std::thread_local! {{
            static __LARK: ::lark_rs::Lark =
                ::lark_rs::Lark::new(
                    {name}::GRAMMAR,
                    ::lark_rs::LarkOptions::default(),
                ).expect("include_lark!: grammar was validated at compile time");
        }}
        __LARK.with(|lark| f(lark))
    }}

    /// Parse `input` from the grammar's start symbol.
    pub fn parse(
        &self,
        input: &str,
    ) -> ::core::result::Result<::lark_rs::ParseTree, ::lark_rs::ParseError> {{
        Self::with_lark(|lark| lark.parse(input))
    }}

    /// Parse `input` from an explicit start symbol.
    pub fn parse_with_start(
        &self,
        input: &str,
        start: &str,
    ) -> ::core::result::Result<::lark_rs::ParseTree, ::lark_rs::ParseError> {{
        Self::with_lark(|lark| lark.parse_with_start(input, start))
    }}
}}

// Tie the build to the grammar file so editing it forces a recompile.
const _: &[u8] = include_bytes!({path});
"#,
        name = struct_name,
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
