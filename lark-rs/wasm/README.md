# lark-rs-wasm — Lark parsing for browser and Node.js (#47)

WebAssembly bindings for [`lark-rs`](../), built with `wasm-bindgen` /
`wasm-pack`. All parsing logic lives in the shared crate one directory up;
this is a thin adapter, mirroring the PyO3 binding (`../python/`).

## Build

```bash
npm install            # installs wasm-pack (devDependency)
npm run build          # → pkg/        (Node.js / CommonJS npm package)
npm run build:web      # → pkg-web/    (browser / ES module npm package)
```

`pkg/` is the publishable npm package (`npm publish` from inside it).

## Usage

```js
const { Lark } = require("./pkg/lark_rs_wasm.js");

const parser = new Lark(grammarText, { parser: "lalr" });
const tree = parser.parse('{"key": [1, 2]}');
// → { type: "tree", data: "...", children: [...] }   (plain JS object)
```

Options mirror the Python binding's kwargs (camelCase or snake_case):
`parser` (`"earley"` default | `"lalr"` | `"cyk"`), `lexer`, `start` (string or
array), `ambiguity`, `propagatePositions`, `keepAllTokens`,
`maybePlaceholders` (default `true`, like Python Lark), `strict`,
`gRegexFlags` (flag letters, e.g. `"i"`), and `importSources` (below). Errors
are JS `Error`s with `name` set to `"GrammarError"` or `"ParseError"`.

Relative `%import` works without a filesystem via `importSources` — a plain
object mapping virtual `/`-separated paths to grammar text; an imported
grammar's own relative imports resolve against its virtual directory, exactly
like sibling files on disk:

```js
const parser = new Lark('%import .dir.lib (greeting)\nstart: greeting', {
  importSources: {
    "dir/lib.lark":    '%import .tokens (NAME)\ngreeting: "hello" NAME',
    "dir/tokens.lark": "NAME: /[a-z]+/",
  },
});
```

Trees are returned in the repo's oracle JSON shape
(`tools/generate_oracles.py`): tree nodes `{type, data, children}`, token
nodes `{type, token_type, value, line, column, ...}`, and `maybePlaceholders`
holes `{type: "unknown", repr: "None"}` — so results compare directly against
committed Python-Lark oracle fixtures. `parseToJson()` returns the same shape
as a JSON string.

## WASM constraints (and why they're fine)

* **No `std::thread`, ~1 MB stacks** — the engine never recurses to input
  depth: the Earley forest walk is iterative (#33), `Tree::drop`/`clone` are
  manual worklist implementations (#151), and this binding's serializer is an
  explicit-stack walk. The smoke test pins a 50,000-level-deep parse.
* **No filesystem** — `%import` of the bundled libraries (`common`, `python`,
  `lark`, `unicode`) works (they are compiled from in-memory sources), and
  relative file imports resolve through the in-memory `importSources` map
  (above). Without the option, a file import fails with the same
  `ImportNotFound` error a string-loaded grammar gets everywhere else.

## Tests

```bash
npm test               # builds pkg/ and runs the JS smoke tests
```

The smoke tests live in [`../tests/wasm/`](../tests/wasm/) and replay the
JSON oracle corpus (`tests/fixtures/oracles/json/cases.json`) through the
WASM module, comparing every tree against the committed Python-Lark output.
Host-side unit tests for the serializer run with plain `cargo test` here.

In a sandbox without GitHub release access, build with
`./node_modules/.bin/wasm-pack build --target nodejs --no-opt` (skips
downloading the optional `wasm-opt` optimizer).
