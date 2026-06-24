//! PyO3 bindings for `lark-rs` — a drop-in, Rust-powered replacement for the
//! Python Lark parsing toolkit.
//!
//! The module exposes three types mirroring Python Lark's API surface:
//!
//! * [`PyLark`] — `Lark(grammar, parser=..., lexer=..., start=...)` + `.parse()`
//! * [`PyTree`] — `Tree(data, children)` with `.data`, `.children`, `.pretty()`
//! * [`PyToken`] — a `str`-like leaf with `.type`, `.value`, and position info
//!
//! All parsing logic lives in the shared `lark-rs` crate; this file is a thin
//! adapter that translates Python kwargs into [`LarkOptions`] and the Rust
//! `Tree`/`Token` types into Python objects.

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::PyList;

use lark_core::{
    Ambiguity, Child, Lark, LarkOptions, LexerType, ParseTree, ParserAlgorithm, Token, Tree,
};
use lark_core::{
    GrammarError as RsGrammarError, LarkError as RsLarkError, ParseError as RsParseError,
};

// ─── Exceptions ─────────────────────────────────────────────────────────────
// Mirror Python Lark's exception hierarchy closely enough to be catchable in the
// same way: a `LarkError` base with `GrammarError` / `ParseError` subclasses.

create_exception!(
    lark_rs,
    LarkError,
    PyException,
    "Base class for all lark_rs errors."
);
create_exception!(
    lark_rs,
    GrammarError,
    LarkError,
    "Raised when a grammar fails to compile."
);
create_exception!(
    lark_rs,
    ParseError,
    LarkError,
    "Raised when input fails to parse."
);

fn map_lark_error(e: RsLarkError) -> PyErr {
    match e {
        RsLarkError::Grammar(g) => map_grammar_error(g),
        RsLarkError::Parse(p) => map_parse_error(p),
    }
}

fn map_grammar_error(e: RsGrammarError) -> PyErr {
    GrammarError::new_err(e.to_string())
}

fn map_parse_error(e: RsParseError) -> PyErr {
    ParseError::new_err(e.to_string())
}

// ─── Token ──────────────────────────────────────────────────────────────────

/// A positioned lexer token, mirroring `lark.Token`.
///
/// Like Python Lark's `Token` (which subclasses `str`), it compares equal to its
/// own string `value`, so existing code that does `tok == "foo"` keeps working.
#[pyclass(name = "Token", module = "lark_rs")]
#[derive(Clone)]
struct PyToken {
    #[pyo3(get, name = "type")]
    type_: String,
    #[pyo3(get)]
    value: String,
    #[pyo3(get)]
    line: usize,
    #[pyo3(get)]
    column: usize,
    #[pyo3(get)]
    end_line: usize,
    #[pyo3(get)]
    end_column: usize,
    #[pyo3(get)]
    start_pos: usize,
    #[pyo3(get)]
    end_pos: usize,
}

impl PyToken {
    fn from_token(t: &Token) -> Self {
        PyToken {
            type_: t.type_.clone(),
            value: t.value.clone(),
            line: t.line,
            column: t.column,
            end_line: t.end_line,
            end_column: t.end_column,
            start_pos: t.start_pos,
            end_pos: t.end_pos,
        }
    }
}

#[pymethods]
impl PyToken {
    #[new]
    #[pyo3(signature = (type_, value))]
    fn new(type_: String, value: String) -> Self {
        PyToken {
            type_,
            value,
            line: 0,
            column: 0,
            end_line: 0,
            end_column: 0,
            start_pos: 0,
            end_pos: 0,
        }
    }

    fn __str__(&self) -> &str {
        &self.value
    }

    fn __repr__(&self) -> String {
        format!("Token({:?}, {:?})", self.type_, self.value)
    }

    fn __len__(&self) -> usize {
        self.value.chars().count()
    }

    fn __hash__(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        self.value.hash(&mut h);
        h.finish()
    }

    /// `str`-like equality: a token equals another token (or a plain string) when
    /// their text values match — exactly the semantics of Python Lark's `Token`,
    /// which is a `str` subclass.
    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        if let Ok(s) = other.extract::<String>() {
            return self.value == s;
        }
        if let Ok(tok) = other.extract::<PyRef<'_, PyToken>>() {
            return self.value == tok.value;
        }
        false
    }
}

// ─── Tree ───────────────────────────────────────────────────────────────────

/// A parse-tree node, mirroring `lark.Tree`.
///
/// `children` is a real Python `list` so it can be indexed, iterated and mutated
/// exactly like Python Lark's. Each child is a `Tree`, a `Token`, or `None`
/// (the latter only when the parser is built with `maybe_placeholders=True`).
#[pyclass(name = "Tree", module = "lark_rs")]
struct PyTree {
    #[pyo3(get, set)]
    data: String,
    #[pyo3(get, set)]
    children: Py<PyList>,
}

impl PyTree {
    fn from_tree(py: Python<'_>, t: &Tree) -> PyResult<Py<PyTree>> {
        let children = PyList::empty(py);
        for child in &t.children {
            let obj = child_to_py(py, child)?;
            children.append(obj)?;
        }
        Py::new(
            py,
            PyTree {
                data: t.data.clone(),
                children: children.unbind(),
            },
        )
    }
}

/// Convert a single `Child` to its Python representation.
fn child_to_py(py: Python<'_>, child: &Child) -> PyResult<PyObject> {
    match child {
        Child::Tree(t) => Ok(PyTree::from_tree(py, t)?.into_any()),
        Child::Token(tok) => Ok(Py::new(py, PyToken::from_token(tok))?.into_any()),
        Child::None => Ok(py.None()),
    }
}

#[pymethods]
impl PyTree {
    #[new]
    #[pyo3(signature = (data, children=None))]
    fn new(py: Python<'_>, data: String, children: Option<Bound<'_, PyList>>) -> PyTree {
        let children = match children {
            Some(list) => list.unbind(),
            None => PyList::empty(py).unbind(),
        };
        PyTree { data, children }
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        let items: Vec<String> = self
            .children
            .bind(py)
            .iter()
            .map(|c| child_repr(&c))
            .collect();
        Ok(format!("Tree({:?}, [{}])", self.data, items.join(", ")))
    }

    /// Structural equality on `data` + `children`, like Python Lark's `Tree`.
    fn __eq__(&self, py: Python<'_>, other: &Bound<'_, PyAny>) -> PyResult<bool> {
        let Ok(other) = other.extract::<PyRef<'_, PyTree>>() else {
            return Ok(false);
        };
        if self.data != other.data {
            return Ok(false);
        }
        let a = self.children.bind(py);
        let b = other.children.bind(py);
        if a.len() != b.len() {
            return Ok(false);
        }
        for (x, y) in a.iter().zip(b.iter()) {
            if !x.eq(&y)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Multi-line indented rendering, matching `lark.Tree.pretty()`.
    #[pyo3(signature = (indent_str="  "))]
    fn pretty(&self, py: Python<'_>, indent_str: &str) -> PyResult<String> {
        let mut out = String::new();
        self.pretty_into(py, 0, indent_str, &mut out)?;
        Ok(out)
    }

    fn __str__(&self, py: Python<'_>) -> PyResult<String> {
        self.__repr__(py)
    }
}

impl PyTree {
    fn pretty_into(
        &self,
        py: Python<'_>,
        level: usize,
        indent_str: &str,
        out: &mut String,
    ) -> PyResult<()> {
        let pad = indent_str.repeat(level);
        let children = self.children.bind(py);
        if children.len() == 1 {
            // A single non-tree child renders inline: `data\tvalue`.
            let only = children.get_item(0)?;
            if let Ok(tok) = only.extract::<PyRef<'_, PyToken>>() {
                out.push_str(&format!("{}{}\t{}\n", pad, self.data, tok.value));
                return Ok(());
            }
        }
        out.push_str(&format!("{}{}\n", pad, self.data));
        for child in children.iter() {
            if let Ok(subtree) = child.extract::<PyRef<'_, PyTree>>() {
                subtree.pretty_into(py, level + 1, indent_str, out)?;
            } else if let Ok(tok) = child.extract::<PyRef<'_, PyToken>>() {
                out.push_str(&format!("{}{}\n", indent_str.repeat(level + 1), tok.value));
            } else {
                out.push_str(&format!("{}None\n", indent_str.repeat(level + 1)));
            }
        }
        Ok(())
    }
}

fn child_repr(c: &Bound<'_, PyAny>) -> String {
    if let Ok(tok) = c.extract::<PyRef<'_, PyToken>>() {
        tok.__repr__()
    } else if let Ok(tree) = c.extract::<PyRef<'_, PyTree>>() {
        tree.__repr__(c.py())
            .unwrap_or_else(|_| "Tree(...)".to_string())
    } else {
        "None".to_string()
    }
}

// ─── Lark ───────────────────────────────────────────────────────────────────

/// The compiled parser, mirroring `lark.Lark`.
#[pyclass(name = "Lark", module = "lark_rs", unsendable)]
struct PyLark {
    inner: Lark,
}

#[pymethods]
impl PyLark {
    /// Build a parser from grammar text.
    ///
    /// Accepts the same keyword arguments as Python Lark's `Lark(...)` for the
    /// options the Rust engine supports today.
    #[new]
    #[pyo3(signature = (
        grammar,
        *,
        parser="earley",
        lexer="auto",
        start=None,
        ambiguity="resolve",
        propagate_positions=false,
        keep_all_tokens=false,
        maybe_placeholders=true,
        strict=false,
        g_regex_flags=0,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        grammar: &str,
        parser: &str,
        lexer: &str,
        start: Option<&Bound<'_, PyAny>>,
        ambiguity: &str,
        propagate_positions: bool,
        keep_all_tokens: bool,
        maybe_placeholders: bool,
        strict: bool,
        g_regex_flags: u32,
    ) -> PyResult<Self> {
        let start = parse_start(start)?;
        let options = LarkOptions {
            start,
            parser: parse_parser(parser)?,
            lexer: parse_lexer(lexer)?,
            ambiguity: parse_ambiguity(ambiguity)?,
            propagate_positions,
            keep_all_tokens,
            maybe_placeholders,
            strict,
            g_regex_flags,
            base_path: None,
            // Python callers have a real filesystem; in-memory import sources
            // (#153) are a WASM-binding affordance. Python Lark's own
            // `import_paths` loaders could map here if ever needed.
            import_sources: None,
            postlex: None,
            // No Python kwarg — the binding always uses the default (regex) scanner
            // backend; the DFA backend is an internal lark-rs knob (LEXER_DFA_PLAN).
            lexer_backend: Default::default(),
        };
        let inner = Lark::new(grammar, options).map_err(map_lark_error)?;
        Ok(PyLark { inner })
    }

    /// Parse `text` and return a `Tree` (or a bare `Token` for a collapsing
    /// `?start` rule, exactly as Python Lark does).
    #[pyo3(signature = (text, start=None))]
    fn parse(&self, py: Python<'_>, text: &str, start: Option<&str>) -> PyResult<PyObject> {
        let result = match start {
            Some(s) => self.inner.parse_with_start(text, s),
            None => self.inner.parse(text),
        }
        .map_err(map_parse_error)?;
        parse_tree_to_py(py, &result)
    }
}

fn parse_tree_to_py(py: Python<'_>, pt: &ParseTree) -> PyResult<PyObject> {
    match pt {
        ParseTree::Tree(t) => Ok(PyTree::from_tree(py, t)?.into_any()),
        ParseTree::Token(tok) => Ok(Py::new(py, PyToken::from_token(tok))?.into_any()),
        // A bare `None` root (`?start: [A]` on `""`, #289) maps to Python's literal
        // `None`, exactly what Python Lark returns for that collapse.
        ParseTree::None => Ok(py.None()),
    }
}

// ─── Option parsing helpers ─────────────────────────────────────────────────

fn parse_start(start: Option<&Bound<'_, PyAny>>) -> PyResult<Vec<String>> {
    let Some(obj) = start else {
        return Ok(vec!["start".to_string()]);
    };
    if let Ok(s) = obj.extract::<String>() {
        return Ok(vec![s]);
    }
    if let Ok(v) = obj.extract::<Vec<String>>() {
        if v.is_empty() {
            return Err(GrammarError::new_err("start must not be empty"));
        }
        return Ok(v);
    }
    Err(GrammarError::new_err(
        "start must be a string or a list of strings",
    ))
}

fn parse_parser(s: &str) -> PyResult<ParserAlgorithm> {
    match s {
        "earley" => Ok(ParserAlgorithm::Earley),
        "lalr" => Ok(ParserAlgorithm::Lalr),
        "cyk" => Ok(ParserAlgorithm::Cyk),
        other => Err(GrammarError::new_err(format!(
            "unknown parser {other:?} (expected 'earley', 'lalr', or 'cyk')"
        ))),
    }
}

fn parse_lexer(s: &str) -> PyResult<LexerType> {
    match s {
        "auto" => Ok(LexerType::Auto),
        // Python Lark renamed 'standard' to 'basic'; accept both.
        "basic" | "standard" => Ok(LexerType::Basic),
        "contextual" => Ok(LexerType::Contextual),
        "dynamic" => Ok(LexerType::Dynamic),
        "dynamic_complete" => Ok(LexerType::DynamicComplete),
        other => Err(GrammarError::new_err(format!(
            "unknown lexer {other:?} (expected 'auto', 'basic', 'contextual', \
             'dynamic', or 'dynamic_complete')"
        ))),
    }
}

fn parse_ambiguity(s: &str) -> PyResult<Ambiguity> {
    match s {
        "resolve" => Ok(Ambiguity::Resolve),
        "explicit" => Ok(Ambiguity::Explicit),
        "forest" => Ok(Ambiguity::Forest),
        other => Err(GrammarError::new_err(format!(
            "unknown ambiguity {other:?} (expected 'resolve', 'explicit', or 'forest')"
        ))),
    }
}

// ─── Module ─────────────────────────────────────────────────────────────────

#[pymodule]
fn lark_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyLark>()?;
    m.add_class::<PyTree>()?;
    m.add_class::<PyToken>()?;
    m.add("LarkError", m.py().get_type::<LarkError>())?;
    m.add("GrammarError", m.py().get_type::<GrammarError>())?;
    m.add("ParseError", m.py().get_type::<ParseError>())?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
