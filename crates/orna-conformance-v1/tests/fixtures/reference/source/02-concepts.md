# 2. Concepts and boundaries {#concepts}

Orna combines a typed language, relational data and Git-backed database history. The following concepts describe the relationships between source declarations, stored data and running programs.

## The four states of a worktree

A database worktree has a committed `HEAD`, an index containing staged changes, ordinary unstaged files, and durable local runtime changes. The **CWD** is their reconciled logical database view, distinct from the process's filesystem current directory. Runtime changes can be queryable and durable before they appear in a Git commit.

```text
source modules ───→ resolved declarations ───→ typed program
                          │                       │
committed snapshots ──────┼──→ logical CWD ←── activations
                          │         │
                          │         └──→ durable local transactions
                          │                       │
                          └──────── publication ──┴──→ Git history
```

[Repository state](#repository) defines these layers precisely. [Branching](#branching) preserves ordinary Git behaviour; [publication](#publication) describes how local runtime data becomes committed while preserving staged and unstaged changes.

## Values, declarations and relations

A **value** has a static type. A **nominal type** has an identity distinct from its representation; a **transparent alias** does not. A **refined type** is nominal and validates its base value through declaration-owned assertions. Optional values use `Some(value)` and `null`.

A **module unit** contains declarations. A **row unit** contains one schema-directed record body. Module reachability begins at root `main.orna`; a recursive filesystem scan is not the program. See [source and modules](#source-modules) and [tables](#tables).

A **table** stores keyed rows. A **relation** is a typed, possibly lazy collection over which queries operate. A table is ordered by its canonical primary key, whereas a derived relation has only the order guaranteed by its operations. An **assertion** is an always-enforced proposition, not a debug-only test. See [relations](#relations) for observation and ordering.

## Execution and progress

An **activation** is a root execution/atomicity boundary: one submitted input, a page action, a direct finite run, a child activation or a stream callback. Synchronous helper calls share their activation. A **task** is scheduled work with an activation or session owner. A **run** is a separately launched program; a **session** can own work across multiple prompt inputs.

When an owner ends, unfinished children are cancelled and joined. Cancellation is not an ordinary failure caught by `|?`. Open Orna transactions roll back; already committed work remains. See [execution](#execution).

A **stream** produces ordered items, possibly without end. A **consumer identity** identifies the resumable processing configuration, not only a function name. A **checkpoint** records the provider position from which that consumer safely resumes. Each processed item/batch and its checkpoint advance are committed together. A **failed delivery** has one stable identity across retries. See [streams](#streams) and [checkpoints](#checkpoints).

## Identity and snapshots

An **ObjectId** identifies a declaration across ordinary edits and renames. A **revision** identifies one immutable meaning of that declaration. A **snapshot** identifies an exact database state: either a committed state or a pinned local CWD generation. A moving selector such as a branch name is resolved to a snapshot before it can be used as a stable reference.

A relation's `as_of` selects historical data and its decoding schema. Whole-program historical evaluation additionally selects historical code and dependencies. The distinction is observable; see [historical queries](#relations) and [system references](#system).

## Presentation and encoding

**Inspect** provides structural developer output. **Display** provides human-facing text. **Present** provides a typed presentation tree for tables, trees, charts and other renderings. A **codec** encodes or decodes values in a specified format. Changing a formatter does not change equality, stored values, canonical bytes or Git identity. See [presentation](#presentation) and [canonical values](#formats).

The `sys` namespace is the mandatory typed introspection and control plane. The `std` namespace is optional ordinary library code, selected and pinned like other dependencies. The [intrinsic environment](#standard-library) remains sufficient for core integrity and execution without an optional library package.

