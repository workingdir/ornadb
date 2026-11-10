# Orna language reference 1.2: overview

Orna is a statically typed scripting language for programs whose code, data and
history live in Git. A program is a directory of `.orna` modules. Its data is
kept in typed tables. Every committed state of both is a Git snapshot, so
branching, merging and diffing work on programs and data together.

This reference states rules: grammar, typing rules, effect rules and evaluation
rules. Formats, storage, the repository protocol and the command line are
specified elsewhere and are not repeated here.

## The pieces

**Values.** Every value has a static type and is immutable. A variable is a
slot that holds one value; assigning to the variable replaces the value in the
slot and never changes a value anyone else holds (chapter 3).

**Modules.** One `.orna` file is one module. Its path gives its namespace:
`sensors/greenhouse.orna` is `sensors.greenhouse`, and a directory's `main.orna`
is the directory's namespace. A module contains declarations only: `use`,
`table`, `fn`, `type`, `enum`, `protocol` and `assert`. Loading a module never
runs code (chapter 1).

**Tables.** A table stores rows under a key.

```orna
pub table Reading(id: Uuid = uuid7()) {
    sensor: Str,
    taken: Instant,
    value: Decimal,
    label: Str => "{sensor}: {value}",

    assert every(r => r.value >= 0.decimal);
}
```

Key parameters come first and form the primary key. A field is stored
(`name: T`), stored with an insertion default (`name: T = expr`) or computed
(`name: T => expr`). An `assert` is an always-enforced proposition about the
whole table. A table row type is an ordinary nominal type; `Reading.insert(...)`
and the query operators are the only ways to change or read stored data.

**Activations.** An activation is one unit of execution and one unit of
atomicity: a REPL input, a `run` of a function, a page action, one stream item,
or an explicitly started child. Everything an activation writes commits
together on success and rolls back together if anything escapes it as a failure
or cancellation (chapter 4). The activation reads one snapshot taken when it
starts and sees its own writes.

**Snapshots.** A committed database state is a Git commit. Code, schemas and
rows at that commit are one value; reading historical data reads that commit.
Merging two branches merges code and data, then re-checks every assertion on the
merged result.

**Core, sys and std.** Three layers supply names. `core` and `sys` ship with the
engine and are versioned with it: `core` holds the intrinsic types and
operations, `sys` the typed introspection and control plane. `std` is ordinary
Orna written on top of them, pinned to a Git commit, optional and replaceable.
A package cannot ship native code; only the engine's embedded modules declare
native functions (chapter 1, "Native declarations").

## The execution pipeline

What happens to a program, in order:

1. **Read.** The engine reads the source files of one snapshot. Source must be
   UTF-8. Each file becomes a module through its path.
2. **Parse.** Each module is parsed by the grammar in chapter 1 into
   declarations. A parse error is reported with a line, column and byte span;
   the parser does not guess.
3. **Resolve.** `use` declarations and names are resolved to declarations.
   Imports execute nothing. An unresolved name, or a name two wildcard imports
   both provide, is an error.
4. **Check.** Types are inferred and checked (chapters 2 and 3). The effects of
   every function are computed from its body and calls and checked at the places
   that restrict them (chapter 4). Assertions are type-checked as predicates.
   The checker either finds a static type for every expression or reports a
   diagnostic; there is no dynamic `Any`.
5. **Activate.** A caller (the REPL, `orna run`, a page, a stream) starts an
   activation: it fixes the snapshot, the activation time and an open
   transaction.
6. **Evaluate.** The function body runs, left to right. Reads see the starting
   snapshot plus the activation's own writes. Writes go into a pending
   candidate state.
7. **Validate.** Before the activation's writes become visible, every assertion
   affected by them is evaluated against the candidate state. A false assertion
   aborts with a diagnostic naming a witness row where one exists.
8. **Complete.** On success the writes commit together. On failure or
   cancellation they roll back. Work that left the engine (a network request)
   cannot be rolled back and the engine does not pretend otherwise.

Steps 1 to 4 depend only on the snapshot. They give the same result for the same
snapshot on any machine.

## A small program

```orna
pub table Room(id: Uuid = uuid7()) {
    name: Str,
    temp: Decimal,

    assert all_unique(r => r.name);
    assert every(r => r.temp >= -273.15);
}

pub fn record(name: Str, temp: Decimal): Room =
    Room.insert({ name: name, temp: temp });

pub fn warmest(): Room? =
    Room | sort_by(r => r.temp) | last();
```

`record` fails if the temperature is below absolute zero or if another room has
the same name. In each case nothing is written.
`warmest` returns `null` when the table is empty.

## Reading the rest

| Chapter | Content |
|---|---|
| 01 | Lexical structure and the grammar |
| 02 | Types |
| 03 | Bindings, expressions and patterns |
| 04 | Effects, failure, rollback and cancellation |

## Conventions

* A rule is written `premises => conclusion` (typing) or as a numbered step list
  (evaluation). A diagnostic names the rule that produced it.
* Examples are small and follow the grammar. An example marked `// error` is
  rejected by the checker.
* Resource limits (digit counts, nesting, memory) are quotas that fail with a
  quota error. They are never part of the meaning of a program.
* Each chapter ends with the changes from the frozen 1.1.0 text and the
  decisions (D-ids) in [DECIDED.md](../DECIDED.md) that produced them.

## Changes from 1.1.0

* The reference states rules instead of numbered MUST clauses and omits history,
  product requirements and orchestration text. Format, storage, repository and
  CLI material is out of scope.
* The reference is edition 1.2. The 1.1.0 publication stays pinned as history.
* `core` is named as the engine-versioned intrinsic layer beside `sys`.
* Units and dimensions are not part of the language (D8).

## Decided by

D12 (core, sys, std layering), D13 (separate reference, edition 1.2), D8 (no
user units), D10 (variables are plain slots).
