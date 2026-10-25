# Orna 1.1.0 — final consolidation

Publication revision: **final-2026-10-05**. This supersedes the earlier supplied
1.1.0 authoring draft and is pinned independently by artifact hash.

## Replaced

The three new-write row layouts become ORP-1. Separate tracked tables/blobs/
watermark folders become one native `.orna/store` graph. `format.orna` and
`database.orna` bootstrap metadata become one literal `database.orna` header.
Bytes-only new Blob semantics become explicitly annotated OVB-2/ROV-3/SOV-3
values. Fixed ordinal index batching is replaced by pinned anchor partitioning.

## Added

Writable VFS-1 row directories, safe MIME suffix policy, shared edit-candidate
state and errors, strong guarded editing, exact staging/export/mount boundaries,
format-3 migration and explicit legacy consumer identity mappings. The final
package contains updated API metadata, profiles, scenarios, examples, legacy
provenance, executable reference policy checks and isolated native-Git tests.

## Preserved

Native Git history and partial clone; exact `std` submodule pins; core language
and row-mutation spelling; snapshot/code distinction; embedded local durability;
structured concurrency and stream/checkpoint safety; legacy protocol byte
semantics; explicit request recovery; minimal source-hosted devtools first.

## Not claimed

The publication is not a compiled Orna release. Reference checks are not proof
of production FUSE/editor compatibility, crash safety, large-file memory bounds
or million-row query performance. See the evidence and implementation gates.
