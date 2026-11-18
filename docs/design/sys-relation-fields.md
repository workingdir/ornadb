# Relation-valued fields in `sys` rows (`sys.Commit.parents`)

**Bead:** `ornadb-5il1.276` (blocker analysis of `sys.git.commits`). **Authority:** `reference/Orna-1.1.0`
(`source/08-relations.md`, ORNA-REL-001 and ORNA-REL-002; `source/17-repl.md`, ORNA-REPL-004; `source/29-formats.md`,
"Portable opaque and contextual values": tag 60027, tag 60022 and the sentence "A finite relation may be encoded only
after its complete bounded observation succeeds"; `source/15-system.md`, ORNA-SYS-009, ORNA-SYS-012, ORNA-SYS-021,
ORNA-SYS-074 and ORNA-SYS-094; `source/13-presentation.md`, ORNA-PRES-001, ORNA-PRES-002 and ORNA-PRES-006).
**Status:** the decision below is made and implemented: the `sys.GitOid` value it depends on
([the `sys.GitOid` value](sys-row-references.md)), the lazy `parents` handle, the presentation summary and the
`sys.git.commits` member (slices 1 to 3 below).

## The problem

`sys.Commit.parents` is `Relation<sys.Commit>`. The same shape is declared by `sys.Snapshot.parents`,
`sys.PlanNode.inputs` and `sys.Function.calls`, and the last of those is cyclic for a recursive function. A row of such a
relation cannot hold its relation field as a nested list: that list would be O(history) per row (and exponential in
the number of merges, because a shared ancestor would be listed once per path), and for a cyclic relation it would be
infinite. The evaluator also has no canonical form for a relation value (`Value::raw()` answers
`ORNA-EVAL-UNSUPPORTED`), so a row that holds one cannot cross the REPL result boundary, the `$_` binding or a UI node
property, all of which need a canonical value.

## What the specification fixes

- A relation value is a lazy, composable query source (ORNA-REL-001). Presentation asks for a bounded window and never
  for the whole relation (ORNA-REL-002, ORNA-REPL-004).
- A finite relation has one canonical form, tag 60027 `[ordered, rows]` with complete typed rows, and "may be encoded
  only after its complete bounded observation succeeds. Exceeding the configured resource bound fails; the codec must
  not emit a truncated successful relation. A stored table field may not use a live Relation in place of a finite
  stored list; materialisation and the field's declared type are explicit." Recursive structures are expressed
  through references, not by embedding rows (`source/29-formats.md`, "stored references can be cyclic across tables
  without embedding recursive row contents").
- Presentation is not persistence (ORNA-PRES-001). Inspect is structural, cycle-safe and bounded, and may truncate
  large collections (ORNA-PRES-002, ORNA-PRES-006). A result is "encoded where supported and presented independently"
  (ORNA-SYS-074).
- Missing, pruned or unhydrated data is reported as unavailable, never as observed emptiness (ORNA-SYS-012,
  ORNA-SYS-094), and a grouped handle and its canonical relation identify the same rows at one snapshot
  (ORNA-SYS-009).

The specification gives no display form for a relation nested in a row, so the form below is derived from those
clauses and is an implementation decision, not a specified encoding.

## Decision

1. **In memory a relation-valued field is a lazy handle.** The row holds a `Value::Relation` whose plan names a
   self-describing sub-source that the provider serves, for `sys.Commit.parents` the object ids of the parents the
   commit records (`sys.git.commits/parents:<oid>,<oid>`), and re-queries it every time the field is enumerated. A
   parent id is an immutable fact of a commit object, so the plan stays valid for the pin it was built at, one
   enumeration reads O(parents) commits and never walks history, and `commit.parents | map(p => p.oid)` composes lazily
   (ORNA-REL-001). Each parent row carries its own handle, so recursion is one level per enumeration.
2. **The codec stays strict.** `Value::raw()` and `Value::canonical()` keep refusing a value that holds a relation:
   tag 60027 needs a complete bounded observation, which for a recursive relation walks the history and fails past the
   bound (it must not truncate), so a stored field, an effect argument, an equality (ORNA-EQ-001) and every
   `std.encoding.*` codec refuse such a row with `ORNA-EVAL-UNSUPPORTED`. A caller compares `row.reference` or `row.oid`
   and materialises what it needs explicitly.
3. **Presentation summarises a nested relation.** The REPL result boundary (`Value::canonical_repl_result`, which
   already summarises a format-3 Blob) and the UI node property check replace each relation nested in a list, record,
   tuple, option or enum payload with the record `{ type: "Relation", rows: N }`, where `rows` is present only when the
   handle knows its exact count without reading (the parents a commit records; the rows of an inline relation).
   Nothing is read, nothing recurses, the output is deterministic and bounded, the summary never claims emptiness
   (`rows` is absent, not `0`, when unknown), and it is a record, so it cannot be mistaken for a relation. A result
   that is itself a relation keeps its two existing contracts: `RelationDisplay::Reject` refuses it and
   `RelationDisplay::Window` shows a bounded window. `orna run` keeps its strict canonical result, as it already
   refuses a relation result, so a function returns `row.oid` or a projection, not the row.
   A REPL `let` retains a canonical value and so cannot retain a lazy handle (it does not retain a relation either);
   `$_` after such a row holds the summary, and a later input queries again from the source.
4. **Unavailable is not empty.** A shallow clone records no parents for its boundary commits (`git log` prints none,
   while the commit object still names them). The parents of a commit listed in the repository's `shallow` file are
   unavailable, so enumerating its `parents` fails with `sys.snapshot.incomplete` (ORNA-SYS-012, ORNA-SYS-052); its
   `rows` count is absent. A root commit is the only commit whose parents are known to be empty (`rows: 0`).
5. **The row set is the ancestry of the pin.** `sys.git.commits` serves the commits reachable from the commit the host
   attached at (`SysHostContext::head_commit`), the pin every other Git member uses (ORNA-SYS-011): that set never
   changes for a commit, so paging is stable. Pages are `git log --skip --max-count` runs over the pin; the cursor is
   the position and the id of the last row served, re-checked on resume, and a mismatch (the history behind the pin
   changed, for example by a `fetch --deepen`) fails with `sys.snapshot.incomplete` instead of splicing two views
   (ORNA-SYS-089, ORNA-SYS-092). A page backs off when a listing would pass the 4 MiB bound of a Git listing, a read
   past 100,000 commits fails closed, and an unborn `HEAD` serves an empty relation.

## Slice plan

1. `sys.GitOid` value (done): `Value::GitOid` in `orna-evaluator` and `orna-exec`, the `orna-value` codec, REPL display
   and `$_`.
2. Lazy handle and presentation summary (done): `git_rows::served_page` dispatches the `parents` sub-source;
   `Value::canonical_repl_result` and the UI node property check summarise nested relations.
3. `sys.git.commits` (done): a bounded `git log` reader in `orna-repository`, the `sys.Commit` rows in `orna-evaluator`
   (`reference`, `oid`, `snapshot`, `parents`, `author`, `committer`, `message`, `authored_at`, `committed_at`,
   `tree`), the `LINKED_GROUPED_HANDLES` entry in the `orna-sys-id` generator and the `git_rows::MEMBERS` entry.
