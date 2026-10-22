# Orna 1.1.0: final implementation decisions

**Conceptual integrity is the governing principle.** The system has one
published executable snapshot and one logical mutation path. Multiple ways to
read or edit it must not become multiple authorities to keep synchronized.

## The one-sentence model

**Source defines tables and behaviour; indexed immutable pages store rows;
Blob values identify immutable bytes and their declared interpretation; Git
commits version that state; a writable VFS presents rows as directories.**

## Settled decisions

| Area | Final choice |
|---|---|
| Published history | Native Git commits/trees/blobs and partial clone; no LFS or separate media service |
| Source | Ordinary reachable `.orna` modules, fixed `main.orna` root |
| Rows | One ORP-1 ordered immutable map profile, including small tables |
| Large values | Shared bounded graph; Blob references never expand media into row pages |
| Blob identity | Raw SHA-256 plus length; MIME/hints never enter that content hash |
| Blob value | Content plus required canonical declared MIME and optional safe suffix hint |
| Physical layout | `.orna/database.orna` and `.orna/store` only |
| Object storage | Git owns loose-object prefix fanout and packs; no second hash warehouse |
| VFS row | `Music/london/data.orna`, `content.mp3`, `cover.jpg` |
| VFS write | Draft → complete validation → captured-version CAS → durable transaction |
| Git commit | Separate reviewed publication; save does not mean commit |
| MIME naming | Pinned MIME-1 preference table, compatible hint, otherwise `.bin` |
| Historical mount | Immutable source/schema/data/content pin; branch name resolves once |
| Workspace mount | Same pending state as its actual backing Git worktree |
| Standard library | Existing explicit submodule model retained; local vendoring/fork is explicit |
| UI | One optional devtools app; table/input/Run/result/refresh first |
| CSS and browser bindings | Ordinary selected renderer-package resources, not a devtools primitive |
| Portability | Complete export verifies superproject and recursive dependency closure |
| Long-term recovery | Read-only extraction without executing historic apps, decoders or AI |

## What database.orna is—and is not

It contains the new physical format coordinate and stable UUID. It is bounded
literal data. It is not a list of tables, a server config file, a MIME registry,
a current-commit hash, an object catalogue, or a program that must run to open
the database. Format 3 selects one documented set of codecs; there is no
per-table profile menu.

The store root binds typed relation roots, including published system records.
Each nonterminal storage node uses `data` and optional `refs` with real native
Git edges. Raw chunk leaves are normal Git blobs. Tables and media share that
infrastructure, but use key and byte-range indexes respectively. This avoids
both a separate tables/blobs warehouse and the claim that different values
need no different semantics.

## The important corrections from the previous draft

**One row format replaces three.** Physical loose files become a virtual
editing representation. No Parquet/loose/hybrid dual writer remains.

**Blob annotations are a real value change.** OVB-2, ROV-3 and SOV-3 encode them
explicitly. Old wire remains OVB-1; unnegotiated tags fail. It is not acceptable
to add MIME metadata while continuing to call observably different values
identical. Exact raw content remains separately comparable.

**Chunk indexing is no longer fixed ordinal batches.** Stable anchor grouping
and local span lengths reduce shifts after insertions. Canonical constants and
forced-boundary behaviour are pinned; workload benchmarks still decide whether
performance is sufficient. No “always O(log n)” claim is made.

**Filesystem editing has an explicit durability contract.** `write()` creates
a candidate; sync or validated replacement accepts it. `release()` cannot
communicate validation failure. A generic editor may ignore close errors.
Recovered drafts and explicit guarded edits are part of the product, not
excuses to ship read-only mounts instead.

**The native hash object store is already there.** Git loose objects use
prefix fanout and packfiles; a second `.orna/blobs/ab/cd/...` is unnecessary.
The native graph still has bounded fanout and height. Tree metadata and clone
costs remain real and measured.

## Boundaries that must remain visible

A logical transaction is not a Git publication, a complete export is not a
partial clone, a promised object is not verified local content, a file suffix
is not a format validator, a content hash is not a permission, and a page's
session is not a long database transaction.

Ordinary editor protection starts at its captured write/temp-create baseline.
Only the guarded edit flow can prove the earlier read baseline when the
application discarded its original handle. The adapter cannot infer intent
from a random new temporary file. Multi-file saves are grouped only explicitly.

Historical records must not revive current leases or rewind allocator refs.
Changing code/codec domains must not silently reprocess durable consumers.
A Git text merge does not prove database invariants. An unindexed query or
recursive filesystem scan can require broad reads. Retained history costs
space and cannot be erased while kept reachable.

These are not extra product modes. They are correctness boundaries shared by
every entrypoint.

## Delivery order

1. Shared format/value/row-identity interfaces and legacy-reader preservation.
2. Canonical graph, row index, annotated lazy Blob and independent extraction.
3. Capture, local transaction, staging/publication, migration and crash recovery.
4. Writable VFS with real editors, concurrent API edits and history mounts.
5. Minimum source-hosted devtools over the same APIs, not a new infrastructure.
6. Explicit later profile work: richer inspection and Orna-authored Wasm client.

The first connected proof is one song and one image: import, query metadata,
retrieve/play bytes, edit a title from the REPL, observe the virtual row, edit
the virtual row and content, reject an invalid save, publish, mount history,
and reopen a complete copy offline. Then run the 10 GB and million-row gates.
No simulated application or isolated toy graph completes those gates.

## Scope of this document

This plan and its ordered reference are implementation authority for the
requested final 1.1.0 authoring package. They are not a production release of
Orna or an assertion that every standard-library/FUSE interface already exists.
Unimplemented items have explicit owners and acceptance tests. The compatibility
archive is not another current design. Use the actual checkout, not a stale
repository audit, to determine what code is reusable.
