# 23. One indexed row store {#storage}


## One representation, not a placement menu

All new-format persisted rows use ORP-1, the immutable indexed row profile in
`profiles/store-format-3.md`. Small tables have shallow roots; large tables
add index levels. There are no editable/compact/hybrid new writers, no row
placement preference, and no size threshold that changes the table model.
Parquet/text are explicit interchange or legacy-reader formats only.

**ORNA-ROW-001** Every new-format table MUST have one ordered map from typed primary key to stored row value, bound to a stable table/schema identity.
**ORNA-ROW-002** Table lookup/order MUST use the language's logical key comparison, not filename order, CBOR byte order or hash order.
**ORNA-ROW-003** The canonical page/group algorithm, encoding and bounds MUST be those in ORP-1; runtime optimizations MUST produce identical canonical output for the same logical snapshot.
**ORNA-ROW-004** Blob fields MUST use small annotated-content references in stored rows; media bytes MUST never be expanded into row-page bytes or base64 row text.
**ORNA-ROW-005** Large rows/strings/collections MUST use the shared value-overflow graph so metadata pages and direct dependency fanout remain bounded.
**ORNA-ROW-006** Copy-on-write updates MUST reuse unchanged nodes where canonical boundaries permit it; changed indices and payload bytes MUST be measured separately.
**ORNA-ROW-007** Partial fetch MUST preserve exact schema/root pins and return unavailable rather than empty/successfully truncated results on missing objects.
**ORNA-ROW-008** Secondary indexes and long-key VFS name indexes MUST use the same ordered store profile and commit atomically with the rows they index.
**ORNA-ROW-009** An unindexed scan MAY require broad reads; explain/counters MUST distinguish estimated from actual transfer, decode and scan work.
**ORNA-ROW-010** Page rewrites/repacking/rechunking MUST NOT produce logical mutations when decoded values/annotations are unchanged.

## Representation versus interfaces

```text
Orna mutation             -> shared transaction -> stored map
Devtools edit             -> shared transaction -> stored map
VFS completed candidate   -> shared transaction -> stored map
```

A virtual `.orna` document is a schema-directed view, not another stored row.
Snapshot exports use a documented data/Blob manifest rather than guessing at
virtual paths. Opening data does not execute source scripts.

**ORNA-ROW-011** Mutation/assertion/reference validation MUST be shared across all entrypoints; no VFS-specific weaker rules are permitted.
**ORNA-ROW-012** Raw storage nodes MUST not be exposed as writable row files; external graph modification requires full admission before use.
**ORNA-ROW-013** Format-1/2 histories MUST retain versioned readers. Migration MUST NOT dual-write the old row layouts or silently upgrade a historical commit.
**ORNA-ROW-014** New `sys.Storage` metadata MUST describe root, schema, generation, row count when known, and observed bytes/availability; removed preference/placement modes MUST NOT survive as hidden toggles.
**ORNA-ROW-015** The native graph MUST connect all direct dependencies through real Git tree entries, use one object store, and reject mismatching, unused or type-invalid dependency edges.

## Physical paths

`.orna/store` is the current snapshot graph, not an append-only collection of
all historical chunks. Every node uses the same `data`/optional `refs` envelope;
raw terminal byte chunks are ordinary Git blobs. Git owns `.git/objects` fanout
and packfiles. Orna MUST NOT repartition Git's object directory, write a second
hash warehouse, or require a separate media daemon.

A million rows are a required performance test. Neither native Git trees nor
prolly-style partitioning is permission to claim zero index churn, constant
clone size or exact-byte network reads without measurements.
