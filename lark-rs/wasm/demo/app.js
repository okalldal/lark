// lark-rs playground — a static page over the wasm-pack web build (../pkg-web,
// copied to ./pkg by `npm run demo:build`). No framework, no bundler: the
// browser loads the ES module, instantiates the WASM, and everything below is
// plain DOM wiring.

import init, { Lark, version } from "./pkg/lark_rs_wasm.js";
import { EXAMPLES } from "./examples.js";

const $ = (id) => document.getElementById(id);
const els = {
  example: $("example"),
  parserOpt: $("parser-opt"),
  ambiguity: $("ambiguity"),
  keepAllTokens: $("keep-all-tokens"),
  grammar: $("grammar"),
  importsDetails: $("imports-details"),
  importsList: $("imports-list"),
  addImport: $("add-import"),
  input: $("input"),
  tabTree: $("tab-tree"),
  tabJson: $("tab-json"),
  status: $("status"),
  error: $("error"),
  outTree: $("out-tree"),
  outJson: $("out-json"),
  version: $("version"),
};

// ─── Import-files panel ──────────────────────────────────────────────────────
// Each row is one virtual file: a path input + a grammar textarea. The rows
// become the `importSources` option (a plain { path: text } object).

function addImportRow(path = "", text = "") {
  const row = document.createElement("div");
  row.className = "import-row";

  const head = document.createElement("div");
  head.className = "import-row-head";
  const pathInput = document.createElement("input");
  pathInput.type = "text";
  pathInput.placeholder = "lib.lark";
  pathInput.value = path;
  pathInput.setAttribute("aria-label", "virtual file path");
  const remove = document.createElement("button");
  remove.type = "button";
  remove.textContent = "remove";
  remove.addEventListener("click", () => {
    row.remove();
    scheduleParse();
  });
  head.append(pathInput, remove);

  const body = document.createElement("textarea");
  body.spellcheck = false;
  body.wrap = "off";
  body.value = text;
  body.setAttribute("aria-label", "virtual file grammar text");

  row.append(head, body);
  els.importsList.appendChild(row);
  for (const el of [pathInput, body]) el.addEventListener("input", scheduleParse);
  return row;
}

function collectImportSources() {
  const sources = {};
  for (const row of els.importsList.querySelectorAll(".import-row")) {
    const path = row.querySelector("input").value.trim();
    if (path) sources[path] = row.querySelector("textarea").value;
  }
  return sources;
}

function setImportRows(sources) {
  els.importsList.replaceChildren();
  for (const [path, text] of Object.entries(sources ?? {})) addImportRow(path, text);
  els.importsDetails.open = Object.keys(sources ?? {}).length > 0;
}

// ─── Examples ────────────────────────────────────────────────────────────────

for (const [i, ex] of EXAMPLES.entries()) {
  const opt = document.createElement("option");
  opt.value = String(i);
  opt.textContent = ex.name;
  els.example.appendChild(opt);
}

function loadExample(i) {
  const ex = EXAMPLES[i];
  els.grammar.value = ex.grammar;
  els.input.value = ex.input;
  els.parserOpt.value = ex.parser ?? "earley";
  els.ambiguity.value = ex.ambiguity ?? "resolve";
  els.keepAllTokens.checked = false;
  setImportRows(ex.imports);
  scheduleParse();
}

// ─── Parsing ─────────────────────────────────────────────────────────────────
// The compiled parser is cached and rebuilt only when the grammar, options, or
// import files change — typing in the input box reuses it.

let parser = null;
let parserKey = null;

function currentOptions() {
  return {
    parser: els.parserOpt.value,
    ambiguity: els.ambiguity.value,
    keepAllTokens: els.keepAllTokens.checked,
    importSources: collectImportSources(),
  };
}

function getParser() {
  const grammar = els.grammar.value;
  const options = currentOptions();
  const key = JSON.stringify([grammar, options]);
  if (key !== parserKey) {
    parser = new Lark(grammar, options); // throws GrammarError on bad grammar
    parserKey = key;
  }
  return parser;
}

function parseNow() {
  let tree;
  const t0 = performance.now();
  try {
    tree = getParser().parse(els.input.value);
  } catch (e) {
    // A failed grammar build must not leave a stale cached parser behind.
    if (e?.name === "GrammarError") parserKey = null;
    showError(e);
    return;
  }
  const ms = performance.now() - t0;
  showTree(tree);
  els.status.textContent = `parsed in ${ms.toFixed(1)} ms`;
}

let parseTimer = null;
function scheduleParse() {
  clearTimeout(parseTimer);
  parseTimer = setTimeout(parseNow, 200);
}

// ─── Output rendering ────────────────────────────────────────────────────────

function showError(e) {
  els.error.hidden = false;
  els.error.textContent = `${e?.name ?? "Error"}: ${e?.message ?? e}`;
  els.outTree.replaceChildren();
  els.outJson.textContent = "";
  els.status.textContent = "";
}

function showTree(tree) {
  els.error.hidden = true;
  els.outTree.replaceChildren(renderNode(tree));
  els.outJson.textContent = JSON.stringify(tree, null, 2);
}

// Build the rendered tree with an explicit work stack — a parse tree is as
// deep as the input is nested, and the engine itself guarantees deep results
// (that's the point of #33/#151), so the demo must not recurse either.
function renderNode(root) {
  const container = document.createElement("div");
  const stack = [[root, container]];
  while (stack.length > 0) {
    const [node, parent] = stack.pop();
    if (node.type === "tree") {
      const details = document.createElement("details");
      details.open = true;
      const summary = document.createElement("summary");
      summary.textContent = node.data;
      summary.className = node.data === "_ambig" ? "rule ambig" : "rule";
      const children = document.createElement("ul");
      details.append(summary, children);
      appendChildEl(parent, details);
      if (node.children.length === 0) {
        const empty = document.createElement("li");
        empty.className = "hole";
        empty.textContent = "(no children)";
        children.appendChild(empty);
      }
      for (let i = node.children.length - 1; i >= 0; i--) {
        stack.push([node.children[i], children]);
      }
    } else if (node.type === "token") {
      const tok = document.createElement("span");
      tok.className = "token";
      tok.title = `line ${node.line}, column ${node.column}`;
      const type = document.createElement("span");
      type.className = "tok-type";
      type.textContent = node.token_type;
      const value = document.createElement("span");
      value.className = "tok-value";
      value.textContent = JSON.stringify(node.value);
      tok.append(type, " ", value);
      appendChildEl(parent, tok);
    } else {
      // maybePlaceholders hole: {type: "unknown", repr: "None"}
      const hole = document.createElement("span");
      hole.className = "hole";
      hole.textContent = "None";
      appendChildEl(parent, hole);
    }
  }
  return container;
}

function appendChildEl(parent, el) {
  if (parent.tagName === "UL") {
    const li = document.createElement("li");
    li.appendChild(el);
    parent.appendChild(li);
  } else {
    parent.appendChild(el);
  }
}

// ─── Tabs ────────────────────────────────────────────────────────────────────

function selectTab(which) {
  const tree = which === "tree";
  els.tabTree.classList.toggle("active", tree);
  els.tabJson.classList.toggle("active", !tree);
  els.outTree.hidden = !tree;
  els.outJson.hidden = tree;
}

// ─── Wiring + boot ───────────────────────────────────────────────────────────

els.example.addEventListener("change", () => loadExample(Number(els.example.value)));
els.addImport.addEventListener("click", () => {
  els.importsDetails.open = true;
  addImportRow();
});
els.tabTree.addEventListener("click", () => selectTab("tree"));
els.tabJson.addEventListener("click", () => selectTab("json"));
for (const el of [els.grammar, els.input]) el.addEventListener("input", scheduleParse);
for (const el of [els.parserOpt, els.ambiguity, els.keepAllTokens]) {
  el.addEventListener("change", scheduleParse);
}

await init();
els.version.textContent = `· lark-rs-wasm ${version()}`;
els.status.textContent = "";
loadExample(0);
