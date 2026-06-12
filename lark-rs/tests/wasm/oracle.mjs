// Shared helpers for the WASM smoke tests (#47).
//
// The WASM binding serializes trees in the repo's oracle JSON shape
// (tools/generate_oracles.py), so a parse result can be compared directly
// against a committed Python-Lark oracle fixture. The comparison mirrors
// tests/common/mod.rs::tree_matches_oracle: oracle-driven (extra fields the
// binding adds, like token positions, are ignored), with `_ambig` children
// matched as an unordered set, and iterative so deep trees don't overflow the
// JS call stack.

import { createRequire } from "node:module";
import { readFileSync } from "node:fs";

const require = createRequire(import.meta.url);

/** The wasm-pack nodejs package (built into wasm/pkg by `npm run build`). */
export function loadPkg() {
  return require("../../wasm/pkg/lark_rs_wasm.js");
}

const REPO = new URL("../..", import.meta.url);

export function readGrammar(name) {
  return readFileSync(new URL(`tests/grammars/${name}.lark`, REPO), "utf8");
}

export function readOracle(suite, name) {
  return JSON.parse(
    readFileSync(new URL(`tests/fixtures/oracles/${suite}/${name}.json`, REPO), "utf8"),
  );
}

/**
 * Compare a parse result (the binding's JS object) against an oracle node.
 * Returns null on match, or a string describing the first mismatch.
 */
export function treeMatchesOracle(result, oracle) {
  // Worklist of [resultNode, oracleNode, path] — explicit, not recursive:
  // result trees are as deep as the input is nested.
  const work = [[result, oracle, "root"]];
  while (work.length > 0) {
    const [node, expected, path] = work.pop();
    const err = matchShallow(node, expected, path, work);
    if (err !== null) return err;
  }
  return null;
}

function matchShallow(node, expected, path, work) {
  switch (expected.type) {
    case "token": {
      if (node === null || node.type !== "token") {
        return `${path}: expected token, got ${describe(node)}`;
      }
      if (node.token_type !== expected.token_type) {
        return `${path}: token type '${node.token_type}' != '${expected.token_type}'`;
      }
      if (node.value !== expected.value) {
        return `${path}: token value ${JSON.stringify(node.value)} != ${JSON.stringify(expected.value)}`;
      }
      return null;
    }
    case "unknown":
      // maybe_placeholders hole: Python serializes None as {"type": "unknown"}.
      if (node === null || node.type === "unknown") return null;
      return `${path}: expected None placeholder, got ${describe(node)}`;
    case "tree": {
      if (node === null || node.type !== "tree") {
        return `${path}: expected tree, got ${describe(node)}`;
      }
      if (node.data !== expected.data) {
        return `${path}: tree '${node.data}' != '${expected.data}'`;
      }
      if (node.children.length !== expected.children.length) {
        return `${path} ('${node.data}'): ${node.children.length} children != ${expected.children.length}`;
      }
      // `_ambig` children are alternative derivations with no guaranteed
      // order — match them as an unordered set (bijectively, like the Rust
      // harness). Alternatives are small, so recursive subtree comparison
      // inside the assignment is fine.
      if (expected.data === "_ambig") {
        return matchAmbig(node.children, expected.children, path);
      }
      for (let i = node.children.length - 1; i >= 0; i--) {
        work.push([node.children[i], expected.children[i], `${path}.${node.data}[${i}]`]);
      }
      return null;
    }
    default:
      return `${path}: unrecognized oracle node type '${expected.type}'`;
  }
}

function matchAmbig(actual, expected, path) {
  const used = new Array(actual.length).fill(false);
  const assign = (i) => {
    if (i === expected.length) return true;
    for (let j = 0; j < actual.length; j++) {
      if (used[j]) continue;
      if (treeMatchesOracle(actual[j], expected[i]) === null) {
        used[j] = true;
        if (assign(i + 1)) return true;
        used[j] = false;
      }
    }
    return false;
  };
  return assign(0)
    ? null
    : `${path}: _ambig alternatives do not match bijectively (${expected.length} expected)`;
}

function describe(node) {
  if (node === null || node === undefined) return String(node);
  if (node.type === "tree") return `tree '${node.data}'`;
  if (node.type === "token") return `token ${node.token_type}`;
  return `'${node.type}'`;
}
