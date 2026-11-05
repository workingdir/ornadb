# 20. Embedded ownership and local durability {#embedded}


A long import, HTTP service, FUSE session or media decoder is not one open
SQLite write transaction. Capture/parse work is bounded and staged; a short
admission transaction publishes a logical state change.

**ORNA-TURSO-001** The local durable CWD MUST use embedded Turso/SQLite-compatible transaction storage, with no mandatory external server.
**ORNA-TURSO-002** Row writes and associated checkpoint/request outcomes MUST commit atomically in the owning local transaction.
**ORNA-TURSO-003** Implementations MUST document process/OS/power-loss guarantees, flush assumptions and measured settings.
**ORNA-TURSO-004** There MUST be at most one owning writable embedded state instance per worktree under the single-owner profile.
**ORNA-TURSO-005** Other local writers MUST coordinate with that owner or fail, never independently reopen an unsafe writable handle.
**ORNA-TURSO-006** Ownership acquisition/recovery MUST fence old writers before admitting a replacement.
**ORNA-IMPL-001** Git plumbing MAY use Git, gix or libgit2 while preserving native objects and observable Git semantics.

## CAPTURE-3: bytes to an admitted immutable value

1. Authorize source access and reserve storage/length/time quotas. Open one
   stable source handle; capture never follows a later pathname replacement.
2. Stream into a private object quarantine and/or disk-backed edit extents.
   Compute raw-byte SHA-256 and the pinned chunk profile; do not retain one
   giant byte vector. Explicit import declarations determine MIME/hint.
3. Finish the immutable graph, verify new object types/digests/lengths, flush
   objects/packs, and install a private native protection ref. Objects must
   survive Git GC before any dependent row is acknowledged.
4. Return an owner-scoped Blob. Insertion/update uses the normal transaction,
   storing its graph dependency with row mutation, request and checkpoint.
   Finishing capture alone does not insert a row or create a public commit.
5. Transfer pin ownership to durable pending state before releasing the
   capture owner. Cancellation frees only unadmitted/unreferenced scratch.
6. Publication makes the graph reachable from a commit. Release private pins
   only after the commit and local visibility journal make protection redundant.

**ORNA-CAP-001** Every acknowledged row containing new content MUST have durable verified new objects and native retention roots; a hash string in SQLite is insufficient.
**ORNA-CAP-002** Inherited promised objects MAY remain unhydrated only with recorded snapshot/promisor evidence; newly imported unknown bytes MUST NOT be acknowledged as merely promised.
**ORNA-CAP-003** Runtime pin refs MUST be local, owner-scoped and excluded from ordinary remote push; export of unpublished work MUST first freeze a recoverable consistent root.
**ORNA-CAP-004** Recovery MUST distinguish unaccepted scratch, accepted pending content, and published content; cleanup MUST NOT infer orphanhood from one absent in-memory handle.
**ORNA-CAP-005** Disk-full, cancellation, source mutation and failed flush MUST leave no falsely acknowledged dependent row; uncertain external effects MUST remain explicitly uncertain.
**ORNA-CAP-006** Git GC/repack MUST share the declared maintenance boundary. Concurrent external destructive prune that bypasses protection is unsupported and MUST NOT be advertised as safe.

A regular-file capture checks size/identity/change observations before and
after reading, but that is not a universal filesystem snapshot. A changing
source either fails under the strict capture profile or is supplied from a
source-snapshot mechanism; do not claim a pre-import version that was not
observed. Native content pins may survive a crash and be cleaned later.
