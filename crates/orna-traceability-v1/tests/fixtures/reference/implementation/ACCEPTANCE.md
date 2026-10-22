# Integrated acceptance gates

These are engine tests to implement/run. They are **not executed** by the
publication model suite. Use disposable test repositories and synthetic or
explicitly authorized media; never run destructive testing on personal data.

## Gate A — format and existing behaviour

Retain original source, mutation, assertion, effect and codec tests. New format
has exactly one writer. Unknown/mixed formats fail early. Old commits decode
under old rules. Blob annotations/equality and legacy protocol rejection agree
across schema, runtime, export and reflection. MIME normalization/hints match
profile vectors. Required source/row identities survive ordinary rename/rekey.

## Gate B — actual data path

Two small media fixtures and one 10 GB high-entropy fixture import with bounded
RSS and temporary storage. Record peak RSS, bytes read/written, network objects
and elapsed time. Title/MIME/hint-only update, row lookup and summary read must
read and write exactly zero media-payload bytes; count metadata separately.
Seek/range reads verify bytes and report whole-object overfetch. Full export
verifies raw hashes. Binary edit rehash cost is measured rather than hidden.

## Gate C — indexed rows at scale

Ten and one million rows use ORP-1. Test indexed point/range queries, missing
keys, unindexed scan and concurrent writes in cold and warm partial clones.
Report root/tree traffic, payload traffic, latency, allocations and indices.
Prepend/delete/rekey/large-row cases measure index churn separately from media
reuse. Fail malformed overlapping ranges, broken counts, giant fanout and
wrong schema domains. Do not claim fixed-row batching is content-stable.

## Gate D — durability and publication

Fault injection or SIGKILL at each capture/pack install/pin/row commit/private
candidate/ref CAS/index reconcile/tail cleanup boundary. Restart and verify
no acknowledged row lacks bytes, no action repeats, no later tail is consumed,
no unrelated staged/unstaged files are lost and no abandoned owner retains
write permission. Exercise coordinated GC/repack and captured external edits.
No production claim of arbitrary concurrent uncoordinated `git prune --expire
now`. Recovery of uncertain external effects never fabricates proven rollback.

## Gate E — actual writable mount

Run supported versions of at least one in-place editor and one temp/rename
editor; record editor/kernel/libfuse versions and save settings. Also test a
media player plus binary export/replacement. Invalid rows/refinements reject
without partial state; detailed diagnostic and rejected draft remain. Cover
fsync/fdatasync, repeated flush, dup/fork, temp sync/rename, writes after sync,
zero truncation, sparse holes, quota exhaustion and close-error ignorance.

Test concurrent REPL changes against captured file-write baselines and strong
`orna edit` read baselines. Demonstrate blind replacement's limitation rather
than claiming it impossible. Open reads remain on old content after replacement.
Cover schema changes, renamed MIME suffix, key/namespace collisions, long/
empty/composite keys, nested collections, large Str, source multi-file edits,
permission changes and unsupported operations. Kernel caching must not leak
private drafts into another handle/generation. Unmount/restart retain only
what was actually acknowledged or explicitly retained as draft.

## Gate F — history, migration and complete copies

Migrate a populated format-1/2 workspace with dirty index/source/submodule work
in a disposable copy. Preserve old commit decoding and byte identity; report
annotation defaults as a new semantic coordinate. Resume a migrated stream
without resetting its consumer/checkpoint identity. Reopen new/old mounts
simultaneously. With original remotes disabled, reconstruct both snapshots
from a complete superproject/dependency export using an independent extractor.

## Gate G — minimal user-visible integration

Table list, input, Run, typed output, explicit refresh and download/native audio
or image rendering over the same production API. Console bindings do not alter
page environment. An invalid mutation stays invalid from UI, REPL and mount.
CSS removal leaves usable native controls. Remove devtools and keep local
queries, mount and content IO working. A second ordinary app uses the same
source-hosted stack. No watch/snippet/theme programme is prerequisite here.

## Gate H — later separately claimed profiles

Orna-authored Wasm renderer/client parity, generated host-only JS, structured
local drafts, reconnect/status recovery and typed event handling require their
own executed tests. Media decoders require pinned implementations and bounded
malformed-input tests. Remote analysis requires explicit policy and provenance.
Do not make those optional claims based on screenshots or a Rust LSP Wasm build.

## Evidence record per gate

Retain tested commit, publication hash, command, host/tool versions, inputs,
exit status, measured counters, output log hash, reviewer and limitations.
`not-executed`, `unavailable`, `failed` and `passed` are different states.
Whole-product completion cannot be derived from a passing subset.
