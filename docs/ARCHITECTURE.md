# OrnaDB architecture

This document is the normative description of the workspace layout, the dependency
rules, and the size budgets. [ARCHITECTURE.md](../ARCHITECTURE.md) at the repository
root is the short entry point; this file is the full document. Behaviour, wire
formats and CLI output are defined by the Orna 1.1.0 specification, not here: every
change described below is a structural move that must not change observable
behaviour.

## 1. What OrnaDB is

OrnaDB is a local, Git-backed data platform for the Orna language. A repository
holds typed source modules and durable data in the same Git working tree; the
embedded state layer and the Git object graph are two views of one project. The
`orna` binary checks, runs and inspects a project; `orna-lsp` serves the editor.

It is not a network service you deploy, and it is not a general-purpose language
runtime. The workspace is layered so that a change to the CLI, the LSP or the
conformance harness cannot reach the format codecs or the evaluator through an
undocumented edge.

## 2. The layer table

A crate may depend only on a crate in a lower layer. A same-layer dependency is
legal only when the source crate's `allowed` list names it, so a same-layer edge is
a reviewed decision rather than an accident. The table is kept here and in
[docs/design/layers.toml](design/layers.toml); the checker reads the TOML file and
the unit tests assert the two agree.

| layer | name | crates |
|---|---|---|
| 0 | primitives | (empty until orna-base) |
| 1 | value | orna-value, orna-foundation, orna-sys-model |
| 2 | syntax | orna-syntax, orna-sys-codegen |
| 3 | identity | orna-sys-id, orna-standard-sources, orna-sys-macros |
| 4 | analysis | orna-semantic |
| 5 | evaluation | orna-evaluator |
| 6 | hosts (I/O) | orna-sys, orna-security |
| 7 | formats and planning | orna-evolution, orna-stream |
| 8 | repository layout | orna-repository |
| 9 | durable state | orna-runtime |
| 10 | publication | orna-storage |
| 11 | project | orna-project, orna-project-repl |
| 12 | presentation | orna-protocol, orna-serving, orna-live |
| 13 | engine | orna-application |
| 14 | edges | orna-cli, orna-lsp, orna-conformance, orna-traceability, orna-table |
| 15 | archive | orna-core |

The numbers are the target architecture's. `orna-foundation` is parked at layer 1
until its split because it already depends on `orna-value`; layer 0 is empty until
`orna-base` exists. The target names replace the current ones as the split stages
land; a rename changes the `name` in `layers.toml` in the same PR.

`orna-core` is layer 15, the 1.0.0 archive: nothing may depend on it. The archive
role and the layer number are the structural half of the archive decision; the
rename to `orna-compat-v1` follows separately.

### Same-layer edges that are recorded today

`orna-foundation -> orna-value`, `orna-sys -> orna-security`,
`orna-project-repl -> orna-project`, `orna-serving -> orna-protocol`,
`orna-live -> orna-protocol`, `orna-live -> orna-serving`,
`orna-cli -> orna-conformance`, `orna-conformance -> orna-table`,
`orna-traceability -> orna-conformance`.

The last three are also recorded violations: the first two under R2, the third
under the standalone-tool rule. They are listed in `allowed` because the layer
check must report one diagnostic per problem, not two.

### Roles

A crate entry may carry a `role`, which is how the checker knows what may reach
the shipped binary:

| role | meaning |
|---|---|
| `production` (default) | may be linked by `orna-cli` |
| `tool` | a separate binary or a development tool; never linked by `orna-cli` |
| `harness` | the conformance harness; never linked by `orna-cli` |
| `oracle` | a table model consumed only by the harness |
| `archive` | the 1.0.0 compatibility archive; nothing may depend on it |

## 3. The rules

**R1. No optional internal dependencies.** An internal crate edge is either a
production dependency or absent. Capability variation is expressed by a trait in a
lower layer, never by a feature. A `#[cfg(feature)]`-gated internal edge makes the
graph a non-DAG and makes the shipped closure a function of the feature set.
Enforced by the checker: every optional internal edge is a violation.

**R2. The shipped closure is production only.** The production dependency closure
of `orna-cli` must not contain a crate whose role is not `production`, nor any name
in `forbidden_in_shipped`. The harness, the oracle table, the traceability tool and
the archive stay out of the binary. Enforced by the checker.

**R3. Names state a layer or a subject.** Allowed suffixes are `-id`, `-sources`,
`-format3`, `-protocol`, `-admission`, `-engine`; "core", "foundation", "runtime",
"storage", "application" and "live" as bare layer words are rejected in review.

**R4. `src/lib.rs` declares and re-exports.** It contains no logic. Intra-crate
module cycles and orphaned source files are caught by `cargo-modules` (section 7);
that gate needs a build, so it runs in the stage PRs and nightly, not in the
metadata-only `arch` job.

**R5. One format, one reader.** A format version is owned by exactly one crate. A
second reader of the same bytes is a violation even when the bytes agree today.

**R6. Identity text exists exactly once.** A sys or std identifier is generated
into one enum or one table; no crate writes the text twice and no crate parses it
back out of a string.

**R7. New public API needs a reason.** A PR that adds a public item names its caller
in the PR body. This is a review rule in the current stage; the per-crate public
surface is not frozen automatically.

Additional structural rules enforced by the checker: no crate may depend on an
archive crate; a crate with production in-degree 0 must be declared a `tool` or an
`archive`, or be the shipped binary (an unused crate is a decision, not an
accident); a crate outside the shipped closure must not depend on a `harness` or
`oracle` crate.

## 4. Size budgets

| item | cap | recorded worst on main d27f7ceb5 |
|---|---:|---:|
| one source file | 1,200 lines | 35,336 (`orna-runtime/src/lib.rs`) |
| one function | 120 lines | 3,983 (`orna-sys/src/introspection.rs::explain_with_inputs`) |

Only per-file and per-function entries are budgeted. There is no crate-level line
budget: adding a few types to a large crate is not a violation.

A PR fails only for new debt:

- a file or function appears over its hard cap with no recorded entry and no
  reviewed raise;
- a recorded entry grows by more than `max(5%, 25 lines)`.

Shrinkage and growth inside the tolerance are non-failing `[note]` lines, so a PR
that is behind a merged refactor is never blocked by another PR's good news.

[scripts/architecture-budget.toml](../scripts/architecture-budget.toml) holds the
recorded debt. Lower the drift after a merge with:

```sh
python3 scripts/check_architecture.py --mode=budget --update
```

The update lowers or removes entries that shrank and never raises or adds one.

A deliberate raise is explicit: add or update a reviewed `[[raise]]` in
[scripts/architecture-budget-raises.toml](../scripts/architecture-budget-raises.toml)
with `path`, `new_limit`, `reason` and `bead`. The raise is the visible exception
list, so no size grows silently; the budget file itself only moves down. A raise
whose path has not landed yet is reserved for an in-flight PR and is reported as a
note, never a failure.

Scope and measurement: `crates/<crate>/src/**/*.rs`; integration tests under
`tests/` are excluded. Lines are logical lines. A function's length is counted from
its `fn` line through its closing brace; comments and string literals are blanked
before brace matching, so a `}` inside a doc comment, a format string or a raw
string cannot end a function early.

## 5. Enforcement and the ratchet

### The checker

[scripts/check_architecture.py](../scripts/check_architecture.py) reads the crate
graph from `cargo metadata` (never from a hand-written edge list), so a new
dependency cannot escape the check. Diagnostics name the offending `Cargo.toml`
line and carry a stable key.

```sh
python3 scripts/check_architecture.py --mode=report   # current state, always exit 0
python3 scripts/check_architecture.py --mode=deny     # exit 1 on a violation not recorded as known
python3 scripts/check_architecture.py --mode=budget   # exit 1 on budget growth
python3 scripts/check_architecture.py --mode=budget --update   # lower drift; never raises
python3 scripts/check_architecture.py --mode=deny --strict   # ignore the baseline
```

`--mode=report` prints every violation, known or new, with the `V` number and the
warning, then the budget summary. `--mode=deny` is the CI gate: a violation recorded
in `docs/design/layers.toml` under `[[known]]` is the ratchet baseline and does not
fail the job, while any other violation does. `--strict` ignores the baseline and is
how the stage proves the check still has teeth: it reports every violation still
recorded below and exits 1.

Treat a new violation as a bug, not as an entry to add. A baseline entry is deleted
by the stage that fixes it. A stale entry is printed as a warning.

### Recorded debt

| id | violation | fixed by |
|---|---|---|
| V1 | `orna-semantic -> orna-sys` is layer 4 -> 6, upward | stage 06 (`orna-sys-id` at layer 3) |
| V2 | `orna-cli -> orna-conformance`: the harness is in the shipped binary | stage 10 (`orna-admission`) |
| V3 | `orna-conformance -> orna-table`: the oracle reaches the binary through the harness | stage 13 (merge into conformance) |
| V4 | `orna-traceability -> orna-conformance`: a standalone tool depends on the harness | corpus facade |
| V6 | `orna-evaluator -> orna-repository` is optional | stage 05a (`HostPort`) |
| V7 | `orna-evaluator -> orna-sys` is optional | stage 05a (`HostPort`) |
| V8 | `orna-core`: production in-degree 0, role not archive | stage 02 (`orna-compat-v1`) |
| V9 | `orna-lsp`: production in-degree 0, role not tool | the LSP is its own binary |
| V10 | `orna-traceability`: production in-degree 0, role not tool | moves under `tools/` with no production dependent |
| V11 | `orna-sys-id`: production in-degree 0, no adopter yet | stage 06 (`orna-sys` and `orna-eval` adopt it) |
| W1 | `orna-foundation`: no cohesive subject, 16 production dependents | stage 03 (`orna-base`) |

The IDs are stable labels; V5 was fixed by the evaluator refactor and its entry
deleted, so the list has a gap.

### External dependency policy

[deny.toml](../deny.toml) is enforced by
`cargo deny --locked check bans licenses sources`. It records the duplicate
versions present today as `[[bans.skip]]` entries (each may only be removed), bans
`hex` outside `orna-value` and `libsql` outside `orna-cli`/`orna-runtime` until the
stage that moves them, and allowlists the licenses already in the resolved graph.
`base64` is deliberately not banned yet: `reqwest` and `hyper-util` depend on it
transitively, so the ban needs the external wrapper chain and belongs to stage 03.
`wildcards` is `allow` because every workspace-internal dependency is a versionless
path dependency today; pinning the workspace version into those manifests is a
separate stage, and the crate DAG is enforced by the checker meanwhile.

### Lint thresholds

[clippy.toml](../clippy.toml) sets the size thresholds (120 lines per function, 7
arguments, cognitive complexity 25, type complexity 250). A threshold alone does not
enable a lint; the workspace lint table that turns `too_many_lines` and
`cognitive_complexity` on is a later stage, and this stage records the suppression
counts that will become live then.

## 6. How to add a crate

1. Choose the layer. A crate may depend only on lower layers; a same-layer edge
   needs an entry in the source crate's `allowed` list and a reviewer.
2. Add `[[crate]] name, layer` to [docs/design/layers.toml](design/layers.toml),
   plus `allowed`, `role` and `warning` when they apply.
3. Add the crate row to the table in this file and the root entry point.
4. Give the crate a production dependent, or declare `role = "tool"`/`"archive"`.
   A crate with in-degree 0 is an error.
5. Add its entry to [scripts/architecture-budget.toml](../scripts/architecture-budget.toml)
   in the same PR: a new crate gets its `allow_clippy` count (0 unless it needs a
   suppression) and any file or function above the hard cap.
6. Add `publish = false` unless the crate is an intentional release surface.

## 7. How to split a crate

A split moves modules between crates; it does not change behaviour, bytes or CLI
output. The procedure is scripted and rerunnable:

1. Prove no open PR touches the crate the stage is about to split.
2. Move the module set with `git mv` and rewrite the import paths from a fixed
   table, so a sibling PR that lands mid-flight is handled by rerunning the script
   rather than by resolving hand conflicts.
3. Delete the old module in the same PR. No bridge crate and no `pub use`
   re-export shim: a bridge is a layer violation the checker would have to
   whitelist.
4. Update `layers.toml`, the budget entry, and the table in this file in the same
   PR.
5. Run, per crate the stage created or moved modules out of:

```sh
cargo modules dependencies -p <crate> --lib --no-externs --acyclic
cargo modules orphans -p <crate> --lib --deny
```

The first fails on an intra-crate module cycle; the second fails on a source file
that no `mod` item links, which is the signature failure of a botched move. Both
need a build, so they are a stage gate rather than part of the metadata-only `arch`
job.

The acceptance block for every structural stage is:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
python3 scripts/check_architecture.py --mode=deny
python3 scripts/check_architecture.py --mode=budget
cargo deny --locked check bans licenses sources
```

and, for a change touching the CLI, the `sys` catalogue or a format, the golden
vectors, the committed `api/sys.json` export and `orna --help` / `orna status
--porcelain` output must be byte-identical to the pre-stage output.

## 8. Entry points

| entry point | crate | in the shipped binary |
|---|---|---|
| `orna` command-line binary | `orna-cli` | yes |
| `orna-lsp` language server | `orna-lsp` | no, its own binary |
| conformance harness | `orna-conformance` | no, must leave by stage 10 |
| citation/scope tool | `orna-traceability` | no |
| 1.0.0 archive | `orna-core` | no, nothing may link it |

## 9. Glossary

| term | meaning |
|---|---|
| layer | a number in the table above; lower is more primitive |
| production edge | a `[dependencies]` entry with `optional = false` |
| optional internal edge | a `[dependencies]` entry with `optional = true`; forbidden by R1 |
| shipped closure | every crate reachable from `orna-cli` by production edges |
| role | `production`, `tool`, `harness`, `oracle` or `archive`; controls R2 |
| known violation | a violation recorded in `layers.toml` under `[[known]]`; the ratchet baseline |
| recorded debt | a size number in `architecture-budget.toml`; may only shrink |
| archive | a crate kept for reading 1.0.0 state; nothing may depend on it |
