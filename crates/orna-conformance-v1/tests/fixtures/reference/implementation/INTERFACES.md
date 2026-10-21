# Shared implementation interfaces — ownership, not new source syntax

These are production seam contracts. API names in api/sys.json are language
surface; native type names below are descriptive and may fit existing types.
Do not create duplicate carriers simply to match this document's spelling.

| Seam | Input | Output/invariant | Owner |
|---|---|---|---|
| Blob value | admitted descriptor + canonical annotations + pin | lazy value, no hidden eager bytes | Values |
| Object resolver | snapshot, OID, expected kind, availability policy | verified object or explicit unavailable | Graph |
| Row read | domain/schema/root and typed key | pinned fields/overflow refs | Rows |
| Mutation candidate | baseline, complete updates, apply identity | admitted single transaction outcome | Runtime |
| Capture | granted source, limits, declaration | protected immutable value | Capture/runtime |
| Publication | frozen batch or reviewed index candidate | ref/index/journal transition | Repository |
| VFS draft | row/field identity, baseline, extents | calls shared candidate acceptance | VFS |
| VFS row text | pinned schema/key/fields | data-only document and sibling map | VFS/values |
| Long-name lookup | schema/parent/key digest | checked full logical key via normal index | Rows/VFS |
| Resource read | selected package + relative resource | captured Blob, not ambient filesystem | Loader |
| Renderer | typed value/intent + host facilities | platform view with no app semantics | UI |

All authority-bearing handles are owner/runtime scoped and non-forgeable from
raw IDs. Every OS/network/renderer adapter receives only documented host
capabilities. No FUSE or HTTP adapter may access a mutable SQLite/Git handle to
bypass the shared transaction/publication boundary.

Library socket/HTML/FUSE/native calls may use safe platform implementations;
wrapping a complete hardcoded app server is not source-hosted conformance.
Historical recovery cannot require executable native plugins or model APIs.

Integration prerequisites: agree codec/tag/snapshot/profile dispatch first;
then row/Blob types; then capture and admission; then publication and VFS.
MIME and filename code uses one generated/pinned registry. Orchestration may
parallelize work only after these interfaces have an owner and acceptance
examples. Shared registries/manifests have one active writer.
