# 24. Git history, remotes and partial clones {#git-history}


**ORNA-GIT-001** A committed snapshot MUST identify all required local source, rows and content through native Git reachability and all dependencies through exact gitlinks/recorded origins.
**ORNA-GIT-002** Partial clones MAY defer blob payloads using ordinary filters and promisor remotes; unknown missing objects MUST be distinguished from promised objects.
**ORNA-GIT-003** A successful full/offline export MUST include all selected snapshot closure and recursive pinned dependencies, not just a pointer-only clone or mounted text files.
**ORNA-GIT-004** Fetch/push/clone MUST preserve Git compatibility and never require LFS to retrieve required content.
**ORNA-GIT-005** Sparse checkout MUST control materialized paths only; it MUST NOT be presented as an availability or confidentiality boundary.

## Lazy reads

Use blobless no-checkout cloning where the remote supports it, then fetch
needed metadata/rows/ranges in bounded batches. A server can refuse filtering;
the command MUST report fallback/estimated scope before silently transferring
an enormous repository. Native Git transfers complete objects, not sub-object
byte ranges. Read-ahead is separately bounded and observable.

**ORNA-GIT-006** Metadata-only queries and updates MUST read zero media payload bytes; metadata objects and tree traffic MUST be accounted separately.
**ORNA-GIT-007** A full mirror of the superproject is not a complete submodule copy; export verification MUST recursively account for dependency object databases.
**ORNA-GIT-008** Refs required for allocator/consumer continuity MUST be synchronized with their existing ordering/validation rules; private edit/capture pins MUST NOT be exposed by ordinary push.
**ORNA-GIT-009** Git host size/push/object limits MUST be reported as deployment constraints. Native format compatibility MUST NOT be advertised as unlimited GitHub storage.

## Retention and security

A metadata edit does not duplicate a recording. Re-encoding all its bytes can
cost another full recording. Chunk reuse is byte reuse, not semantic media
compression. Old reachable ancestry keeps its content; deleting the current
row does not erase its history. History rewrite/export-to-new-root is explicit,
reviewed and never a background prune policy.

**ORNA-GIT-010** Collection MUST preserve content reachable from retained commits, local pins, pending accepted changes, open pins and unfinished recovery journals.
**ORNA-GIT-011** Repository clone credentials MUST be treated as access to repository content; VFS path hiding, namespaces or filtered clone are not row-level permissions.
**ORNA-GIT-012** Offline failure MUST name unavailable scope without exposing secrets; it MUST NOT silently use a current host file or another snapshot.

Complete copy algorithm: freeze/pin a source or CWD root; enumerate its native
closure and dependency commits; hydrate/verify all selected objects; create a
portable archive/mirror plus explicit dependency map; run an independent
read-only reconstruction with the original remote disabled. No historical
application execution is required to recover typed records or bytes.
