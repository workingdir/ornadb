# Work ADR 0104: Studio Catalogue Tree and Function Search Boundary

**Status:** Generic catalogue identities accepted; Studio tree and search deferred

## Decision

OrnaDB 1.0.0 defines catalogue identities and snapshot rules that a future
Studio catalogue view must preserve. A grouped catalogue/history handle and
its canonical relation identify the same rows at the same snapshot
(**ORNA-SYS-009**, `source/15-system.md:52`). Historical system queries are
snapshot-pinned (**ORNA-SYS-011**, `source/15-system.md:54`), and
`qualified_name` is the name at the row's snapshot rather than a timeless
property of a stable identity (**ORNA-SYS-033**, `source/15-system.md:132`).
Dependency traversal is cycle-safe and deterministic
(**ORNA-SYS-043**, `source/15-system.md:150`). A coherent multi-relation
inspection pins and exposes one observation snapshot
(**ORNA-SYS-091**, `source/15-system.md:329`); pruned optional detail is
unavailable metadata, not an empty value (**ORNA-SYS-094**,
`source/15-system.md:335`). The default frontend recommendation says to expose
functions when installed at `/`, but does not define a Studio hierarchy or
search interaction (**ORNA-SERVE-008/009**, `source/28-serving.md:31-33`).

The API reference defines `sys.resolve_function(name, at, from)` as resolving
a function reference (`api/sys.json`, `sys.resolve_function` entry). This is
semantic name resolution under the ordinary visibility/import rules; it does
not specify free-text search, partial matching, case handling, ranking,
filtering, result paging, or a tree projection.

Defer implementation of a Studio catalogue tree and free-text function search
until those Studio-facing contracts and a production consumer path are
accepted. Do not infer hierarchy, grouping, matching, ranking, or navigation
from catalogue order, semantic resolution, dependency traversal, generic
presentation, or the serving frontend recommendation. Work ADR 0086 continues
to defer populated Inspector rows; ADR 0100 governs the Studio source-tooling
boundary; ADR 0103 governs generic inspection/presentation constraints and
defers the Studio-specific runtime and Inspector explorer.

## Search evidence

The frozen-reference search covered the requirement summary, source chapters,
grammar, tests, examples, and API reference:

```sh
rg -n -i 'Studio|catalogue[[:space:]-]+tree|function[[:space:]-]+search' \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/Orna-1.0.0.md \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/source \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/grammar \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/tests \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/examples \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/api
```

It returned no matches (exit 1). The positive nearby clauses are the
snapshot, identity, traversal, and frontend requirements listed above; the
API's `sys.resolve_function` entry only promises semantic resolution.

The repository was also searched for a Studio production path and existing
catalogue/search implementation:

```sh
rg --files -g '*studio*' -g '*catalog*' -g '*inspector*'
rg -n -i 'catalogue tree|function search|search.*function|function.*search' \
  crates/orna-client/src crates/orna-client/examples
```

The filename search found catalogue implementation files and
`crates/orna-client/examples/studio_demo.rs`, but no Studio application tree.
The source search matched only the demo's empty text-input placeholder
“Search functions” (`studio_demo.rs:194`; exit 0). That node has no catalogue
binding, search action, or result projection. The existing Studio artifact is
a demo shell, not a production catalogue view.

## Precedence

The frozen OrnaDB 1.0.0 reference remains authoritative. This decision records
the Studio-specific scope boundary and adds no catalogue API, tree schema,
search algorithm, UI behavior, or runtime contract.
