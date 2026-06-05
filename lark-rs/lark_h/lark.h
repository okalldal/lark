/*
 * lark.h — C API for lark-rs (the `lark_h` crate).
 *
 * A C-compatible surface over the Rust Lark parsing toolkit, for embedding from
 * C, C++, Go (cgo), or Python (ctypes). This header is committed alongside the
 * crate and kept in sync with `src/lib.rs` — it is the source of truth for the
 * symbols a C consumer links against.
 *
 * Ownership / lifetime contract:
 *   - lark_new returns an owning lark_t*; free it with lark_free.
 *   - lark_parse returns an owning lark_tree_t* root; free the whole tree with
 *     lark_tree_free. Children from lark_tree_child are BORROWED — never free
 *     them, and they die with the root.
 *   - const char* returns borrow memory owned by their node (or the thread-local
 *     error slot) and are valid until that node is freed (or the next failing
 *     call on the same thread). Copy them if you need them longer.
 *
 * Every function is null-safe: a null handle yields a benign default, never UB.
 */
#ifndef LARK_H
#define LARK_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handles. */
typedef struct lark_t lark_t;
typedef struct lark_tree_t lark_tree_t;

/* Options passed by value to lark_new. Use lark_default_options() then override. */
typedef struct lark_options_t {
    int parser;             /* 0=earley, 1=lalr, 2=cyk                          */
    int lexer;              /* 0=auto, 1=basic, 2=contextual, 3=dynamic,
                               4=dynamic_complete                               */
    int ambiguity;          /* 0=resolve, 1=explicit, 2=forest                  */
    const char *start;      /* start rule name; NULL => "start"                 */
    int keep_all_tokens;    /* nonzero => keep every token in the tree          */
    int maybe_placeholders; /* nonzero => None placeholders for absent [...]    */
} lark_options_t;

/* Default options: LALR parser, auto lexer, resolve ambiguity, "start". */
lark_options_t lark_default_options(void);

/* Last error message on this thread, or NULL if the last fallible call
 * succeeded. Borrowed; valid until the next failing call on this thread. */
const char *lark_last_error(void);

/* Compile a `.lark` grammar string. Returns NULL on error (see lark_last_error).
 * Free with lark_free. */
lark_t *lark_new(const char *grammar, lark_options_t opts);

/* Free a parser handle. NULL is a no-op. */
void lark_free(lark_t *lark);

/* Parse `len` bytes of `input` (need not be NUL-terminated). Returns the root
 * tree node, or NULL on a parse error (see lark_last_error). Free with
 * lark_tree_free. */
lark_tree_t *lark_parse(lark_t *lark, const char *input, size_t len);

/* Free a parse tree root (frees all descendants). NULL is a no-op. */
void lark_tree_free(lark_tree_t *tree);

/* Node label: rule/alias name for a tree node, terminal type for a token leaf.
 * Borrowed; valid until the tree is freed. */
const char *lark_tree_data(const lark_tree_t *tree);

/* Nonzero if this node is a token leaf, zero if it is a rule (tree) node. */
int lark_tree_is_token(const lark_tree_t *tree);

/* Matched text of a token leaf, or NULL for a rule node. Borrowed. */
const char *lark_tree_token_value(const lark_tree_t *tree);

/* Number of children (0 for a token leaf). */
size_t lark_tree_child_count(const lark_tree_t *tree);

/* Borrow the i-th child (0-based), or NULL if out of range. Do NOT free it;
 * it is owned by the root and dies with it. */
const lark_tree_t *lark_tree_child(const lark_tree_t *tree, size_t i);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* LARK_H */
