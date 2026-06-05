/*
 * smoke.c — C smoke test for the lark_h C API (issue #48).
 *
 * Builds a JSON grammar through lark_new, parses two documents through
 * lark_parse, and walks the resulting tree with lark_tree_* to assert the
 * structure matches what Python/Rust Lark produce.
 *
 * The entry point is `lark_h_run_smoke()` (not `main`): build.rs compiles this
 * file with the `cc` crate and links it into the crate, and the unit test in
 * src/lib.rs calls it over FFI, treating a nonzero return as a failure. That
 * keeps the test running under a plain `cargo test` — which builds only the
 * rlib, never a standalone .a/.so to link a separate C executable against.
 */
#include <stddef.h>
#include <stdio.h>
#include <string.h>

#include "lark.h"

/* The canonical Lark JSON grammar (same as examples/json_parser.rs). */
static const char *JSON_GRAMMAR =
    "?start: value\n"
    "?value: object\n"
    "      | array\n"
    "      | string\n"
    "      | SIGNED_NUMBER  -> number\n"
    "      | \"true\"         -> true\n"
    "      | \"false\"        -> false\n"
    "      | \"null\"         -> null\n"
    "array  : \"[\" [value (\",\" value)*] \"]\"\n"
    "object : \"{\" [pair (\",\" pair)*] \"}\"\n"
    "pair   : string \":\" value\n"
    "string : ESCAPED_STRING\n"
    "%import common.ESCAPED_STRING\n"
    "%import common.SIGNED_NUMBER\n"
    "%import common.WS\n"
    "%ignore WS\n";

static int failures = 0;

#define CHECK(cond, msg)                                                  \
    do {                                                                  \
        if (!(cond)) {                                                    \
            fprintf(stderr, "FAIL: %s (%s:%d)\n", (msg), __FILE__, __LINE__); \
            failures++;                                                   \
        }                                                                 \
    } while (0)

/* Assert a node is a rule node with the given label and child count. */
static void check_rule(const lark_tree_t *n, const char *data, size_t nchildren) {
    CHECK(n != NULL, "node is non-null");
    if (n == NULL) return;
    CHECK(lark_tree_is_token(n) == 0, "node is a rule, not a token");
    CHECK(strcmp(lark_tree_data(n), data) == 0, data);
    CHECK(lark_tree_child_count(n) == nchildren, "child count");
}

/* Assert a node is a token leaf with the given type and value. */
static void check_token(const lark_tree_t *n, const char *type, const char *value) {
    CHECK(n != NULL, "token node is non-null");
    if (n == NULL) return;
    CHECK(lark_tree_is_token(n) == 1, "node is a token");
    CHECK(strcmp(lark_tree_data(n), type) == 0, type);
    CHECK(strcmp(lark_tree_token_value(n), value) == 0, value);
}

int lark_h_run_smoke(void) {
    lark_options_t opts = lark_default_options();
    opts.parser = 1; /* lalr */
    opts.lexer = 2;  /* contextual */

    lark_t *lark = lark_new(JSON_GRAMMAR, opts);
    if (lark == NULL) {
        fprintf(stderr, "lark_new failed: %s\n", lark_last_error());
        return 1;
    }

    /* --- 1. Parse an array of numbers: [1, 2, 3] -------------------------- */
    {
        const char *input = "[1, 2, 3]";
        lark_tree_t *root = lark_parse(lark, input, strlen(input));
        if (root == NULL) {
            fprintf(stderr, "lark_parse(array) failed: %s\n", lark_last_error());
            lark_free(lark);
            return 1;
        }
        /* ?start collapses to the array node. */
        check_rule(root, "array", 3);
        const char *values[3] = {"1", "2", "3"};
        for (size_t i = 0; i < 3; i++) {
            const lark_tree_t *num = lark_tree_child(root, i);
            /* SIGNED_NUMBER -> number  ⇒  Tree("number", [Token(SIGNED_NUMBER)]) */
            check_rule(num, "number", 1);
            check_token(lark_tree_child(num, 0), "SIGNED_NUMBER", values[i]);
        }
        /* Out-of-range child is NULL, not UB. */
        CHECK(lark_tree_child(root, 3) == NULL, "out-of-range child is NULL");
        lark_tree_free(root);
    }

    /* --- 2. Parse an object: {"a": 1} ------------------------------------ */
    {
        const char *input = "{\"a\": 1}";
        lark_tree_t *root = lark_parse(lark, input, strlen(input));
        if (root == NULL) {
            fprintf(stderr, "lark_parse(object) failed: %s\n", lark_last_error());
            lark_free(lark);
            return 1;
        }
        check_rule(root, "object", 1);
        const lark_tree_t *pair = lark_tree_child(root, 0);
        check_rule(pair, "pair", 2);
        const lark_tree_t *str = lark_tree_child(pair, 0);
        check_rule(str, "string", 1);
        check_token(lark_tree_child(str, 0), "ESCAPED_STRING", "\"a\"");
        const lark_tree_t *num = lark_tree_child(pair, 1);
        check_rule(num, "number", 1);
        check_token(lark_tree_child(num, 0), "SIGNED_NUMBER", "1");
        lark_tree_free(root);
    }

    /* --- 3. A parse error returns NULL and sets the error message. -------- */
    {
        const char *bad = "[1, ]";
        lark_tree_t *root = lark_parse(lark, bad, strlen(bad));
        CHECK(root == NULL, "malformed input yields NULL");
        CHECK(lark_last_error() != NULL, "error message is set on failure");
        if (root != NULL) lark_tree_free(root);
    }

    lark_free(lark);

    if (failures != 0) {
        fprintf(stderr, "%d check(s) failed\n", failures);
        return 1;
    }
    printf("lark_h C smoke test passed\n");
    return 0;
}
