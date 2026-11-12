# OrnaDB architecture

OrnaDB is a layered Rust workspace: a language front end, a pure evaluator, a
`sys` host boundary, a durable local state layer, a Git repository layer, and the
edges (CLI, LSP) that wire them together. The package version is 1.0.0 while the
implementation targets the Orna 1.1.0 language.

The full architecture, with the rules behind each layer, the recorded debt and
the procedure for adding or splitting a crate, is in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## The three rules

R1. No optional internal dependencies. A crate edge is production or absent;
    capability variation lives in a trait in a lower layer, never in a feature.
R2. The shipped `orna` binary never links a harness, tool, oracle or archive
    crate: not `orna-conformance`, `orna-traceability`, `orna-lsp` or
    `orna-compat-v1`, and nothing whose role is not `production`.
R3. A crate name states its layer or its subject, never a metaphor. "core",
    "foundation", "runtime", "storage", "application" and "live" as bare words
    are rejected in review.

## Layers (a crate may depend only on a lower layer)

A same-layer edge is legal only when the source crate's `allowed` list in
`docs/design/layers.toml` names it; a lower-layer edge needs no entry. Layer 0
is `orna-base`, the primitives crate; the former `orna-foundation` was absorbed
into `orna-base` and `orna-value` in stage 03.

| layer | name | crates |
|---|---|---|
| 0 | primitives | orna-base |
| 1 | value | orna-value, orna-sys-model |
| 2 | syntax | orna-syntax, orna-intrinsics, orna-sys-codegen, orna-cursor |
| 3 | identity and IR | orna-hir, orna-sys-id, orna-standard-sources, orna-sys-macros |
| 4 | analysis | orna-check |
| 5 | evaluation | orna-evaluator |
| 6 | hosts (I/O) | orna-sys, orna-security |
| 7 | formats and planning | orna-evolution, orna-format3, orna-stream |
| 8 | repository layout | orna-repository |
| 9 | durable state | orna-runtime |
| 10 | publication | orna-storage |
| 11 | project | orna-project, orna-project-repl |
| 12 | presentation | orna-protocol, orna-serving, orna-live |
| 13 | engine | orna-application |
| 14 | edges | orna-cli, orna-differential, orna-lsp, orna-conformance, orna-traceability, orna-table |
| 15 | archive | orna-compat-v1 |

`docs/design/layers.toml` is the machine-readable table and the source of truth;
`python3 -m unittest scripts.test_check_architecture` asserts that this table and
that file agree.

## Size budgets

File 1,200 lines and function 120 lines; there is no crate-level line budget. A PR
fails only for a new over-cap file or function, or a recorded entry growing by more
than `max(5%, 25 lines)`; shrinkage and small growth are notes.
`python3 scripts/check_architecture.py --mode=budget` enforces this against
`scripts/architecture-budget.toml`, whose entries may only shrink. Lower the drift
with `--mode=budget --update`; a deliberate raise is a reviewed `[[raise]]` in
`scripts/architecture-budget-raises.toml` with a reason and a bead.

## Checking

```sh
python3 scripts/check_architecture.py --mode=report   # current state, exit 0
python3 scripts/check_architecture.py --mode=deny     # fails on new violations
python3 scripts/check_architecture.py --mode=budget   # fails on budget growth
python3 scripts/check_architecture.py --mode=budget --update   # lower drift; never raises
python3 -m unittest scripts.test_check_architecture
cargo deny --locked check bans licenses sources
```

The `arch` job in [.github/workflows/lint.yml](.github/workflows/lint.yml) runs
the unit tests, both checker modes and `cargo deny`.
