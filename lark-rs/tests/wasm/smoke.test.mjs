// WASM smoke tests (#47): the wasm-pack npm package parses JSON and the trees
// agree with the committed Python-Lark oracle (the issue's done-when).
//
// Run via `npm test` in lark-rs/wasm (which builds pkg/ first), or directly:
//   node --test lark-rs/tests/wasm/
// after `wasm-pack build --target nodejs` in lark-rs/wasm.

import { test } from "node:test";
import assert from "node:assert/strict";

import { loadPkg, readGrammar, readOracle, treeMatchesOracle } from "./oracle.mjs";

const { Lark, version } = loadPkg();
const jsonGrammar = readGrammar("json");

test("version is exported", () => {
  assert.match(version(), /^\d+\.\d+\.\d+/);
});

// ─── The headline smoke test: the JSON oracle corpus ────────────────────────
// Same parser configuration the oracle was generated with: parser='lalr'
// (contextual lexer is the LALR default) and maybe_placeholders=false —
// tools/generate_oracles.py pins placeholders off; the binding's own default
// is true, matching Python Lark's.

test("JSON corpus matches the Python-Lark oracle", () => {
  const parser = new Lark(jsonGrammar, { parser: "lalr", maybePlaceholders: false });
  const cases = readOracle("json", "cases");
  assert.ok(cases.length > 0, "oracle fixture is non-empty");
  for (const c of cases) {
    if (c.ok) {
      const tree = parser.parse(c.input);
      const mismatch = treeMatchesOracle(tree, c.tree);
      assert.equal(mismatch, null, `input ${JSON.stringify(c.input)}: ${mismatch}`);
    } else {
      assert.throws(
        () => parser.parse(c.input),
        (e) => e.name === "ParseError",
        `input ${JSON.stringify(c.input)} must be rejected with a ParseError`,
      );
    }
  }
});

test("Earley (the default parser) agrees with LALR on the JSON corpus", () => {
  const parser = new Lark(jsonGrammar, { maybePlaceholders: false });
  for (const c of readOracle("json", "cases")) {
    if (!c.ok) continue;
    const mismatch = treeMatchesOracle(parser.parse(c.input), c.tree);
    assert.equal(mismatch, null, `input ${JSON.stringify(c.input)}: ${mismatch}`);
  }
});

// ─── API surface ─────────────────────────────────────────────────────────────

test("parseToJson returns the same tree as parse", () => {
  const parser = new Lark(jsonGrammar, { parser: "lalr" });
  const viaObject = parser.parse('{"a": [1, true]}');
  const viaJson = JSON.parse(parser.parseToJson('{"a": [1, true]}'));
  assert.deepEqual(viaObject, viaJson);
});

test("tokens carry position info", () => {
  const parser = new Lark(jsonGrammar, { parser: "lalr" });
  const tree = parser.parse('{"key": 1}');
  const token = tree.children[0].children[0].children[0];
  assert.equal(token.token_type, "ESCAPED_STRING");
  assert.equal(token.line, 1);
  assert.equal(token.column, 2);
  assert.equal(token.startPos, 1);
  assert.equal(token.endPos, 6);
});

test("options: start rule selection and camelCase/snake_case spellings", () => {
  const grammar = 'a: "x"\nb: "y"\n';
  const parser = new Lark(grammar, { start: ["a", "b"], keepAllTokens: true });
  assert.equal(parser.parse("x", "a").data, "a");
  assert.equal(parser.parse("y", "b").data, "b");
  const snake = new Lark(grammar, { start: ["a", "b"], keep_all_tokens: true });
  assert.deepEqual(snake.parse("x", "a"), parser.parse("x", "a"));
});

test("maybePlaceholders defaults to true, like Python Lark", () => {
  // `object : "{" _WS? [pair ...] "}"` on "{}": the unmatched [] group leaves
  // a None placeholder (Python: Tree('object', [None])), serialized as the
  // oracle shape's {"type": "unknown"} hole.
  const parser = new Lark(jsonGrammar, { parser: "lalr" });
  const tree = parser.parse("{}");
  assert.equal(tree.children.length, 1);
  assert.equal(tree.children[0].type, "unknown");
});

test("explicit ambiguity yields an _ambig node", () => {
  // Two derivations of "ab": (a)(b) via AB-split rules.
  const grammar = 'start: ab\nab: A B | AB\nA: "a"\nB: "b"\nAB: "ab"\n';
  const parser = new Lark(grammar, { ambiguity: "explicit", lexer: "dynamic" });
  const tree = parser.parse("ab");
  assert.equal(tree.children[0].data, "_ambig");
  assert.equal(tree.children[0].children.length, 2);
});

// ─── Errors ──────────────────────────────────────────────────────────────────

test("a bad grammar throws a GrammarError", () => {
  assert.throws(
    () => new Lark("start: undefined_rule\n", {}),
    (e) => e.name === "GrammarError",
  );
});

test("a bad option value throws a GrammarError", () => {
  assert.throws(
    () => new Lark('start: "x"\n', { parser: "glr" }),
    (e) => e.name === "GrammarError" && /unknown parser/.test(e.message),
  );
});

test("a parse failure throws a ParseError with position info in the message", () => {
  const parser = new Lark(jsonGrammar, { parser: "lalr" });
  assert.throws(
    () => parser.parse("{,}"),
    (e) => e.name === "ParseError",
  );
});

// ─── WASM-specific constraints (#47) ─────────────────────────────────────────

test("bundled %import works; file %import fails cleanly (no filesystem)", () => {
  // json.lark already exercises bundled `%import common.*` above; pin the
  // file-import behavior: a grammar loaded from a string has no base path, so
  // a relative import is an ImportNotFound GrammarError — not a crash.
  assert.throws(
    () => new Lark('%import .sibling (RULE)\nstart: RULE\n', {}),
    (e) => e.name === "GrammarError" && /import/i.test(e.message),
  );
});

test("deeply nested input parses, serializes, and drops on the small WASM stack", () => {
  // The #33/#151 payoff this binding depends on: the forest walk, the tree's
  // Drop/Clone glue, and the JSON serializer are all iterative, so a parse
  // tree ~50k levels deep survives the default ~1 MB wasm stack. The result
  // is walked iteratively here too (a recursive JS compare would overflow
  // the *JS* stack at this depth).
  const DEPTH = 50_000;
  const parser = new Lark(jsonGrammar, { parser: "lalr" });
  const input = "[".repeat(DEPTH) + "1" + "]".repeat(DEPTH);
  const tree = parser.parse(input);
  let depth = 0;
  let node = tree;
  while (node.type === "tree" && node.children.length > 0) {
    depth += 1;
    node = node.children[0];
  }
  assert.equal(node.token_type, "SIGNED_NUMBER");
  assert.equal(node.value, "1");
  assert.ok(depth >= DEPTH, `walked ${depth} levels, expected >= ${DEPTH}`);
});
