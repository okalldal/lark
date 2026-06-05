//! `lark_h` — a C-compatible FFI surface for [`lark-rs`](../../).
//!
//! This crate exposes a small, hand-stable `#[no_mangle]` C API so lark-rs can be
//! embedded from C, C++, Go (cgo), or Python (ctypes). The committed header
//! [`lark.h`](../lark.h) mirrors the symbols defined here; keep the two in sync.
//!
//! # Ownership / lifetime contract
//!
//! * [`lark_new`] returns an owning `lark_t*`; release it with [`lark_free`].
//! * [`lark_parse`] returns an owning `lark_tree_t*` (the root node); release the
//!   whole tree with [`lark_tree_free`]. Child pointers from [`lark_tree_child`]
//!   are *borrowed* — they live as long as the root and must NOT be freed.
//! * String pointers ([`lark_tree_data`], [`lark_tree_token_value`],
//!   [`lark_last_error`]) borrow memory owned by their node (or the thread-local
//!   error slot) and stay valid until that node is freed (or the next failing
//!   call on the same thread, respectively). Copy them if you need them longer.
//!
//! Every function is null-safe: passing a null handle yields a benign default
//! (NULL / 0 / -1) rather than undefined behaviour.

// The handle types deliberately use C-style snake_case names so they read
// identically in Rust and in the committed `lark.h`.
#![allow(non_camel_case_types)]

use std::cell::RefCell;
use std::ffi::{c_char, c_int, CStr, CString};
use std::ptr;

use lark_rs::{
    Ambiguity, Child, Lark, LarkOptions, LexerType, ParseTree, ParserAlgorithm, Token, Tree,
};

// ---------------------------------------------------------------------------
// Opaque handle types (forward-declared in lark.h as incomplete structs).
// ---------------------------------------------------------------------------

/// Opaque compiled-grammar handle. Created by [`lark_new`], freed by [`lark_free`].
pub struct lark_t {
    inner: Lark,
}

/// One node of a parse tree. Either a rule node (with children) or a leaf token.
///
/// Owned transitively from the root returned by [`lark_parse`]; freeing the root
/// with [`lark_tree_free`] frees every descendant.
pub struct lark_tree_t {
    /// Rule name (tree node) or terminal type name (token leaf).
    data: CString,
    /// Matched source text — `Some` only for token leaves.
    value: Option<CString>,
    is_token: bool,
    children: Vec<Box<lark_tree_t>>,
}

/// Options passed by value to [`lark_new`]. Mirrors the relevant subset of Rust's
/// `LarkOptions`. Build a sane default with [`lark_default_options`] and override
/// fields as needed.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct lark_options_t {
    /// Parsing algorithm: 0 = earley, 1 = lalr, 2 = cyk.
    pub parser: c_int,
    /// Lexer: 0 = auto, 1 = basic, 2 = contextual, 3 = dynamic, 4 = dynamic_complete.
    pub lexer: c_int,
    /// Ambiguity handling: 0 = resolve, 1 = explicit, 2 = forest.
    pub ambiguity: c_int,
    /// Start rule name. NULL selects the default "start".
    pub start: *const c_char,
    /// Keep every token in the tree (Lark's `keep_all_tokens`). Nonzero = true.
    pub keep_all_tokens: c_int,
    /// Insert None placeholders for absent `[...]` groups (`maybe_placeholders`).
    pub maybe_placeholders: c_int,
}

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_error(msg: impl Into<String>) {
    let msg = msg.into();
    let c = CString::new(msg).unwrap_or_else(|_| CString::new("error").unwrap());
    LAST_ERROR.with(|e| *e.borrow_mut() = Some(c));
}

fn clear_error() {
    LAST_ERROR.with(|e| *e.borrow_mut() = None);
}

/// Last error message on the current thread, or NULL if the last fallible call
/// succeeded. The pointer is valid until the next failing call on this thread;
/// copy it if you need to keep it.
///
/// # Safety
/// The returned pointer must not be freed by the caller.
#[no_mangle]
pub extern "C" fn lark_last_error() -> *const c_char {
    LAST_ERROR.with(|e| match &*e.borrow() {
        Some(c) => c.as_ptr(),
        None => ptr::null(),
    })
}

/// A fully-populated default options value (LALR parser, auto lexer, resolve
/// ambiguity, "start" start symbol). Mirrors Rust's `LarkOptions::default()`.
#[no_mangle]
pub extern "C" fn lark_default_options() -> lark_options_t {
    lark_options_t {
        parser: 1,    // lalr — the always-buildable default, matching LarkOptions::default
        lexer: 0,     // auto
        ambiguity: 0, // resolve
        start: ptr::null(),
        keep_all_tokens: 0,
        maybe_placeholders: 0,
    }
}

fn parser_from_int(v: c_int) -> Option<ParserAlgorithm> {
    match v {
        0 => Some(ParserAlgorithm::Earley),
        1 => Some(ParserAlgorithm::Lalr),
        2 => Some(ParserAlgorithm::Cyk),
        _ => None,
    }
}

fn lexer_from_int(v: c_int) -> Option<LexerType> {
    match v {
        0 => Some(LexerType::Auto),
        1 => Some(LexerType::Basic),
        2 => Some(LexerType::Contextual),
        3 => Some(LexerType::Dynamic),
        4 => Some(LexerType::DynamicComplete),
        _ => None,
    }
}

fn ambiguity_from_int(v: c_int) -> Option<Ambiguity> {
    match v {
        0 => Some(Ambiguity::Resolve),
        1 => Some(Ambiguity::Explicit),
        2 => Some(Ambiguity::Forest),
        _ => None,
    }
}

/// Build options from the C struct, returning a descriptive error for any
/// out-of-range enum so a misuse surfaces through [`lark_last_error`].
fn options_from_c(opts: &lark_options_t) -> Result<LarkOptions, String> {
    let parser =
        parser_from_int(opts.parser).ok_or_else(|| format!("invalid parser: {}", opts.parser))?;
    let lexer =
        lexer_from_int(opts.lexer).ok_or_else(|| format!("invalid lexer: {}", opts.lexer))?;
    let ambiguity = ambiguity_from_int(opts.ambiguity)
        .ok_or_else(|| format!("invalid ambiguity: {}", opts.ambiguity))?;

    let start = if opts.start.is_null() {
        "start".to_string()
    } else {
        // SAFETY: caller guarantees `start` is a valid NUL-terminated C string.
        unsafe { CStr::from_ptr(opts.start) }
            .to_str()
            .map_err(|_| "start symbol is not valid UTF-8".to_string())?
            .to_string()
    };

    Ok(LarkOptions {
        start: vec![start],
        parser,
        lexer,
        ambiguity,
        keep_all_tokens: opts.keep_all_tokens != 0,
        maybe_placeholders: opts.maybe_placeholders != 0,
        ..LarkOptions::default()
    })
}

/// Compile `grammar` (a NUL-terminated `.lark` source string) into a parser.
///
/// Returns an owning handle, or NULL on error (grammar syntax error, LALR
/// conflict, invalid options, …); call [`lark_last_error`] for the message.
/// Free the handle with [`lark_free`].
///
/// # Safety
/// `grammar` must be a valid pointer to a NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn lark_new(grammar: *const c_char, opts: lark_options_t) -> *mut lark_t {
    clear_error();
    if grammar.is_null() {
        set_error("grammar pointer is null");
        return ptr::null_mut();
    }
    let grammar_text = match CStr::from_ptr(grammar).to_str() {
        Ok(s) => s,
        Err(_) => {
            set_error("grammar is not valid UTF-8");
            return ptr::null_mut();
        }
    };

    let options = match options_from_c(&opts) {
        Ok(o) => o,
        Err(e) => {
            set_error(e);
            return ptr::null_mut();
        }
    };

    match Lark::new(grammar_text, options) {
        Ok(inner) => Box::into_raw(Box::new(lark_t { inner })),
        Err(e) => {
            set_error(e.to_string());
            ptr::null_mut()
        }
    }
}

/// Free a parser handle returned by [`lark_new`]. NULL is a no-op.
///
/// # Safety
/// `lark` must be a pointer returned by [`lark_new`] (and not already freed).
#[no_mangle]
pub unsafe extern "C" fn lark_free(lark: *mut lark_t) {
    if !lark.is_null() {
        drop(Box::from_raw(lark));
    }
}

/// Parse `input` (`len` bytes, need not be NUL-terminated) and return the root
/// node of the resulting parse tree.
///
/// Returns NULL on a parse error (call [`lark_last_error`] for the message). Free
/// the returned tree with [`lark_tree_free`].
///
/// # Safety
/// `lark` must come from [`lark_new`]; `input` must point to at least `len`
/// readable bytes.
#[no_mangle]
pub unsafe extern "C" fn lark_parse(
    lark: *mut lark_t,
    input: *const c_char,
    len: usize,
) -> *mut lark_tree_t {
    clear_error();
    if lark.is_null() {
        set_error("lark handle is null");
        return ptr::null_mut();
    }
    if input.is_null() && len != 0 {
        set_error("input pointer is null");
        return ptr::null_mut();
    }
    let bytes = if len == 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts(input as *const u8, len)
    };
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => {
            set_error("input is not valid UTF-8");
            return ptr::null_mut();
        }
    };

    let lark = &(*lark).inner;
    match lark.parse(text) {
        Ok(parse_tree) => Box::into_raw(node_from_parse_tree(&parse_tree)),
        Err(e) => {
            set_error(e.to_string());
            ptr::null_mut()
        }
    }
}

/// Free a parse tree returned by [`lark_parse`]. Frees the whole tree; child
/// pointers obtained via [`lark_tree_child`] become dangling. NULL is a no-op.
///
/// # Safety
/// `tree` must be a *root* pointer from [`lark_parse`] (not a borrowed child).
#[no_mangle]
pub unsafe extern "C" fn lark_tree_free(tree: *mut lark_tree_t) {
    if !tree.is_null() {
        drop(Box::from_raw(tree));
    }
}

/// The node's label: the rule/alias name for a tree node, or the terminal type
/// name for a token leaf. Borrowed; valid until the tree is freed.
///
/// # Safety
/// `tree` must be a valid node pointer (root or borrowed child), or NULL.
#[no_mangle]
pub unsafe extern "C" fn lark_tree_data(tree: *const lark_tree_t) -> *const c_char {
    if tree.is_null() {
        return ptr::null();
    }
    let node = &*tree;
    node.data.as_ptr()
}

/// Nonzero if this node is a token leaf, zero if it is a rule (tree) node.
///
/// # Safety
/// `tree` must be a valid node pointer or NULL.
#[no_mangle]
pub unsafe extern "C" fn lark_tree_is_token(tree: *const lark_tree_t) -> c_int {
    if tree.is_null() {
        return 0;
    }
    let node = &*tree;
    node.is_token as c_int
}

/// The matched text of a token leaf, or NULL for a rule node. Borrowed; valid
/// until the tree is freed.
///
/// # Safety
/// `tree` must be a valid node pointer or NULL.
#[no_mangle]
pub unsafe extern "C" fn lark_tree_token_value(tree: *const lark_tree_t) -> *const c_char {
    if tree.is_null() {
        return ptr::null();
    }
    let node = &*tree;
    match &node.value {
        Some(v) => v.as_ptr(),
        None => ptr::null(),
    }
}

/// Number of children of a node (0 for a token leaf).
///
/// # Safety
/// `tree` must be a valid node pointer or NULL.
#[no_mangle]
pub unsafe extern "C" fn lark_tree_child_count(tree: *const lark_tree_t) -> usize {
    if tree.is_null() {
        return 0;
    }
    let node = &*tree;
    node.children.len()
}

/// Borrow the `i`-th child of a node (0-based). Returns NULL if out of range.
/// The returned pointer is owned by the root tree — do NOT free it; it is
/// invalidated when the root is freed with [`lark_tree_free`].
///
/// # Safety
/// `tree` must be a valid node pointer or NULL.
#[no_mangle]
pub unsafe extern "C" fn lark_tree_child(tree: *const lark_tree_t, i: usize) -> *const lark_tree_t {
    if tree.is_null() {
        return ptr::null();
    }
    let node = &*tree;
    match node.children.get(i) {
        Some(child) => child.as_ref() as *const lark_tree_t,
        None => ptr::null(),
    }
}

// ---------------------------------------------------------------------------
// ParseTree -> owned FFI node conversion.
// ---------------------------------------------------------------------------

/// Build an owned C node from a Rust string, replacing interior NULs (which a C
/// string cannot represent) so the conversion is always lossless-enough and
/// never panics on adversarial input.
fn cstring_lossy(s: &str) -> CString {
    match CString::new(s) {
        Ok(c) => c,
        Err(_) => CString::new(s.replace('\0', "\u{fffd}")).unwrap(),
    }
}

fn node_from_token(tok: &Token) -> Box<lark_tree_t> {
    Box::new(lark_tree_t {
        data: cstring_lossy(&tok.type_),
        value: Some(cstring_lossy(&tok.value)),
        is_token: true,
        children: Vec::new(),
    })
}

fn node_from_tree(tree: &Tree) -> Box<lark_tree_t> {
    let children = tree
        .children
        .iter()
        .map(|c| match c {
            Child::Tree(t) => node_from_tree(t),
            Child::Token(t) => node_from_token(t),
            // A maybe_placeholders gap. Surface it as an empty, valueless leaf
            // labelled "None" so child indices line up with Python Lark's.
            Child::None => Box::new(lark_tree_t {
                data: cstring_lossy("None"),
                value: None,
                is_token: false,
                children: Vec::new(),
            }),
        })
        .collect();
    Box::new(lark_tree_t {
        data: cstring_lossy(&tree.data),
        value: None,
        is_token: false,
        children,
    })
}

fn node_from_parse_tree(pt: &ParseTree) -> Box<lark_tree_t> {
    match pt {
        ParseTree::Tree(t) => node_from_tree(t),
        ParseTree::Token(t) => node_from_token(t),
    }
}

#[cfg(test)]
mod tests {
    // The C smoke test (`csrc/smoke.c`) is compiled by build.rs and linked into
    // this crate; it builds a JSON grammar through the C API, parses an array and
    // an object, and walks the tree asserting its structure. Calling it here —
    // from the crate's own unit-test binary, where the build-script linkage
    // applies — is the issue #48 done-when: "A C smoke-test parses JSON and
    // checks the tree structure." A nonzero return means a C-side assertion
    // failed (diagnostics are printed to stderr by smoke.c).
    extern "C" {
        fn lark_h_run_smoke() -> std::ffi::c_int;
    }

    #[test]
    fn c_smoke_test() {
        let rc = unsafe { lark_h_run_smoke() };
        assert_eq!(rc, 0, "C smoke test (csrc/smoke.c) reported failures");
    }
}
