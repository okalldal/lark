# Wild-Grammar Bank

Real-world Lark grammars and inputs strip-mined from open source projects, used
two ways:

1. **Wild oracle tests** — `cargo test --test test_wild` replays every vendored
   input against trees frozen from Python Lark
   (`tools/generate_wild_oracles.py` → `tests/fixtures/oracles/wild/`), with
   the same XFAIL-burndown discipline as the compliance bank
   (`wild/xfail.json`, regenerate with `LARK_WILD_WRITE_XFAIL=1`).
2. **Wild benchmarks** — `cargo bench --bench wild` measures build cost and
   parse throughput per project, on the grammars and inputs users actually
   have (the wild complement to the synthetic workloads in `benches/parse.rs`).

Where the compliance bank covers what Python Lark's *own test suite* exercises,
this bank covers what *users in the wild* write: big grammars, heavy terminal
sets, deep EBNF nesting, postlex indentation, file-relative imports, the
`regex`-module extensions.

## Layout

```
<project>/
  meta.json     provenance (repo, ref, commit, license) + the exact Lark
                options upstream itself uses + input→upstream-path map
  grammar/      the .lark file(s), vendored VERBATIM from the pinned commit
                (one exception: an upstream grammar *bug* may be patched in the
                vendored copy, recorded in meta.json `local_patches` — we do not
                file upstream bugs; the alternative is leaving the project
                xfail'd. Precedent: cel's `{4-8}`-for-`{4,8}` quantifier typo.)
  inputs/       real inputs, vendored verbatim (or curated strings; the
                meta.json inputs map records where each came from)
  LICENSE       the upstream project's license, vendored verbatim
```

## Projects

| project        | language                  | engine             | source |
|----------------|---------------------------|--------------------|--------|
| cel            | Common Expression Language | LALR (g_regex_flags) | cloud-custodian/cel-python |
| dotmotif       | graph-motif query DSL     | **Earley/dynamic** | aplbrain/dotmotif |
| gersemi_cmake  | CMake                     | LALR               | BlankSpruce/gersemi |
| hcl2           | Terraform HCL2            | LALR               | amplify-education/python-hcl2 @ v4.3.5 |
| lark_lark      | Lark grammars (self-hosting) | LALR            | lark-parser/lark (this repo) |
| mappyfile      | MapServer mapfiles        | LALR               | geographika/mappyfile |
| matter_idl     | Matter cluster IDL        | LALR (341 KB large bucket) | project-chip/connectedhomeip |
| miniwdl_wdl    | Workflow Description Lang 1.0 | LALR           | chanzuckerberg/miniwdl |
| mistql         | MistQL JSON queries       | **Earley/dynamic** | evinism/mistql |
| poetry_markers | PEP 508 env markers       | LALR               | python-poetry/poetry-core |
| poetry_pep508  | PEP 508 dependency specs  | LALR (relative %import) | python-poetry/poetry-core |
| pylogics_ltl   | Linear Temporal Logic     | LALR (relative %import, lookahead terminals) | whitemech/pylogics |
| pyquil         | Quil (quantum ISA)        | LALR               | rigetti/pyquil @ v3.5.4 |
| synapse_storm  | Storm query language      | LALR (regex=True)  | vertexproject/synapse |
| tartiflette    | GraphQL SDL               | LALR               | dailymotion/tartiflette |
| vyper          | Vyper contracts           | LALR + PythonIndenter postlex | vyperlang/vyper |

Three projects record extra context in `meta.json` `description`/`notes`:
upstream cel/miniwdl pass `lexer_callbacks` (dropped — the oracle and replay
both parse without them), gersemi an inline transformer (dropped, raw tree
frozen), and miniwdl's grammar is materialized byte-exactly from the Python
string `versions["1.0"]` since upstream ships no .lark file.

## Adding a project

1. Vendor the grammar verbatim from a pinned commit, its LICENSE, and a handful
   of real inputs from the same upstream; write `meta.json` (copy an existing
   one — `lark_options` must be exactly what upstream passes to `Lark(...)`).
2. `python3 tools/generate_wild_oracles.py` — every input should parse under
   Python Lark (the generator warns otherwise; a frozen parse *error* is also a
   valid oracle, but prefer inputs that parse).
3. `cargo test --test test_wild` — if the new project fails, decide: fix
   lark-rs, or record the gap with `LARK_WILD_WRITE_XFAIL=1` and file an issue.
4. Commit the vendored files, the regenerated oracle JSON, and any xfail diff
   together.

Keep licensing honest: only vendor from projects whose license permits
redistribution with attribution (MIT/Apache-2.0/BSD/MPL-2.0), keep the LICENSE
file, and record the SPDX id in `meta.json`.

## Vetted leads (mined, not vendored — and why)

| source | verdict |
|--------|---------|
| endgameinc/eql | grammar + fixtures exist, but **AGPL-3.0** — license doesn't fit vendoring |
| gorilla-co/odata-query, pwwang/liquidpy | inspected — **not lark-based** |
| gavanderhoorn/fanuc_va_lark_grammar | Apache-2.0 grammar + 7 real `.va` fixtures, but the grammar **crashes Python Lark itself** (Earley `ForestSumVisitor`: `max() arg is an empty sequence` in `visit_symbol_node_out`) — the oracle is broken; revisit if upstream Lark fixes it (candidate bug report) |
| daltskin/sysml-v2-grammar | ANTLR grammar repo; its one lark file parses OMG KEBNF notation whose input corpus is fetched at build time, not vendored — revisit with the OMG spec files |
| ligurio/lark-grammars | curated grammar collection, MIT-style — but inputs are Hypothesis-generated, no fixed corpus; use as a lead generator for grammars only |
| opendatacube/datacube-core, SPFlow, storyscript, outlines | small/embedded/derivative grammars — low value vs. what's already banked |

GitHub code-search patterns for further mining: `"from lark import Lark" tests`,
`"Lark.open(" grammar`, `"*.lark" "%declare"`, `"*.lark" "%override"`,
`"parser=\"earley\"" lark`, `"lexer=\"dynamic\"" lark`, `"ambiguity=" lark`.
