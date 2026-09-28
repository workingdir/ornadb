# Work ADR 0106: Studio Source Reload and Hot-Revision Deferral

**Status:** Deferred for OrnaDB 1.0.0

## Decision

The frozen Orna 1.0.0 reference does not define a Studio source-reload flow or
hot-revision/session transition. Defer this Studio component for the 1.0.0
boundary. This decision records the missing contract; it does not add a reload
operation, select a revision transition, or change active session/resource
identity.

The existing accepted Studio boundaries remain controlling:

* Work ADR 0100 accepts portable semantic/raw Git diff and read-only retained
  source/revision inspection. It explicitly defers a Studio source editor,
  Studio apply workflow, revision-browser interface, and public revision
  activation/restoration behavior until a versioned contract defines them.
  The issue's source-reload/hot-revision flow reaches those deferred surfaces;
  read-only source/revision APIs do not define it.
* Work ADR 0103 records that generic Orna inspection/presentation rules do not
  define a Studio-specific runtime, host adapter, navigation, or interaction
  model. A generic page value or renderer is not a source-reload/session
  contract.
* Work ADR 0086 defers populated Inspector projection rows and states that its
  implementation proposal is not an Orna 1.0.0 behavior contract. Those rows
  cannot supply missing source-revision or session-transition semantics.

This decision is limited to the source-reload/hot-revision component. It does
not change the other Studio component leases or the accepted underlying
source, revision, runtime, session, and security boundaries.

## Frozen-reference search evidence

The accepted Work ADR 0100 records this corpus search and its result:

```text
rg -n -i '\bStudio\b|source apply|interactive apply|revision browser|source editor|apply source' \
  Orna-1.0.0.md source/ grammar/ tests/ examples/ api/

Result: no matches (exit 1).
```

Additional exact searches for the terms used by this component were run against
the frozen reference's normative source, grammar, tests, examples, API, and
combined Markdown specification:

```text
rg -n -i -F -e 'source reload' -e 'hot-revision' -e 'revision browser' \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/source \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/grammar \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/tests \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/examples \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/api \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/Orna-1.0.0.md
  [no matches]
exit code: 1
```

## Resolution

Resolve issue #29 as a documented Orna 1.0.0 deferral. No Studio source-reload
or hot-revision behavior is implemented or claimed by this slice. No Cargo
tests were run because no product source or test files changed.
