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
  inputs/       real inputs, vendored verbatim (or curated strings; the
                meta.json inputs map records where each came from)
  LICENSE       the upstream project's license, vendored verbatim
```

## Projects

| project        | language                  | engine             | source |
|----------------|---------------------------|--------------------|--------|
| hcl2           | Terraform HCL2            | LALR               | amplify-education/python-hcl2 @ v4.3.5 |
| mappyfile      | MapServer mapfiles        | LALR               | geographika/mappyfile |
| mistql         | MistQL JSON queries       | **Earley/dynamic** | evinism/mistql |
| poetry_markers | PEP 508 env markers       | LALR               | python-poetry/poetry-core |
| poetry_pep508  | PEP 508 dependency specs  | LALR (relative %import) | python-poetry/poetry-core |
| pyquil         | Quil (quantum ISA)        | LALR               | rigetti/pyquil @ v3.5.4 |
| synapse_storm  | Storm query language      | LALR (regex=True)  | vertexproject/synapse |
| tartiflette    | GraphQL SDL               | LALR               | dailymotion/tartiflette |
| vyper          | Vyper contracts           | LALR + PythonIndenter postlex | vyperlang/vyper |

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
redistribution with attribution (MIT/Apache-2.0/BSD), keep the LICENSE file,
and record the SPDX id in `meta.json`.
