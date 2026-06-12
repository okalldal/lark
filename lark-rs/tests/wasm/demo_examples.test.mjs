// The playground's example bank (wasm/demo/examples.js) must stay parseable:
// every example's grammar builds with its configured options (including the
// importSources virtual files) and its sample input parses. Runs under the
// same `npm test` as the other WASM smoke tests, so a grammar-side change
// that breaks a demo example fails CI instead of rotting on GitHub Pages.

import { test } from "node:test";
import assert from "node:assert/strict";

import { loadPkg } from "./oracle.mjs";
import { EXAMPLES } from "../../wasm/demo/examples.js";

const { Lark } = loadPkg();

test("demo example bank is non-trivial", () => {
  assert.ok(EXAMPLES.length >= 4);
  assert.ok(
    EXAMPLES.some((ex) => ex.imports && Object.keys(ex.imports).length > 0),
    "at least one example demonstrates importSources",
  );
  assert.ok(
    EXAMPLES.some((ex) => ex.ambiguity === "explicit"),
    "at least one example demonstrates explicit ambiguity",
  );
});

for (const ex of EXAMPLES) {
  test(`demo example parses: ${ex.name}`, () => {
    // The same option object the demo page builds in currentOptions().
    const parser = new Lark(ex.grammar, {
      parser: ex.parser ?? "earley",
      ambiguity: ex.ambiguity ?? "resolve",
      importSources: ex.imports ?? {},
    });
    const tree = parser.parse(ex.input);
    assert.ok(tree.type === "tree" || tree.type === "token");
    if (ex.ambiguity === "explicit") {
      assert.ok(
        JSON.stringify(tree).includes('"_ambig"'),
        "the explicit-ambiguity example must actually be ambiguous",
      );
    }
  });
}
