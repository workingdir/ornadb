# 32. Unified native-Git graph and Blob chunks {#blob-storage}


## OGS-1: one object infrastructure

The complete binary encoding is in `profiles/store-format-3.md` and executable
reference constructors in `tools/final_model.py`. That model demonstrates
format algorithms, not a production transaction engine.

```text
Git commit
  source files and library gitlinks
  .orna/database.orna
  .orna/store          -> root/index nodes
                           -> row pages
                              -> overflow values / Blob descriptors
                                 -> byte index -> raw chunks
```

**ORNA-GRAPH-001** Each nonterminal node MUST be an ordinary Git tree with `data` and optional `refs`; `data` is one canonical bounded CBOR blob and `refs` maps native OID names to exact child object types.
**ORNA-GRAPH-002** The direct dependency inventory in decoded data MUST exactly match `refs`, deduplicated by OID; missing, unused, malformed or wrong-type edges MUST be rejected.
**ORNA-GRAPH-003** Terminal binary chunks MUST be raw Git blobs. No separate media server, pointer-only object or second hash warehouse is permitted.
**ORNA-GRAPH-004** Node metadata MUST be at most 64 KiB and direct fanout at most 256; larger values/indices MUST use the same bounded graph recursively.
**ORNA-GRAPH-005** Objects MUST be validated under their format, schema and context before use. A shared child MUST NOT bypass per-parent semantic constraints.
**ORNA-GRAPH-006** Git SHA-1 and SHA-256 modes MUST retain native OID widths and verification; neither native OID equals the raw-content SHA-256 by definition.
**ORNA-GRAPH-007** The store root MUST contain only snapshot-required reachable state. Retained history belongs to ancestry/pins, not a current-root catalogue of every historical object.

## Chunked content

OGB-2 retains the earlier draft's fixed Gear chunker parameters: reset after
cuts; min 64 KiB, mask 262143, max 1 MiB; table words are the first eight
big-endian bytes of SHA-256 of `orna.gear.v1` plus NUL plus the octet. Details
and vectors are packaged. Chunking never depends on MIME or file extension.

Content index grouping is changed: stable content-derived anchors partition
ordered child entries, rather than fixed ordinal batches that shift after a
prefix insertion. Spans in each node are local lengths, not global offsets.
Full algorithm/limits are fixed by ORP-1/OGB-2; no implementation may choose
its own chunk/page thresholds while writing the same format coordinate.

**ORNA-OGB-001** The root Blob descriptor MUST bind exact length, raw-byte SHA-256 and its native content graph. MIME and suffix MUST be stored with each referencing value, not as global facts about a hash.
**ORNA-OGB-002** Chunk entries MUST bind native object OID, byte length and independent chunk SHA-256; offset traversal MUST validate lengths, height and bounds.
**ORNA-OGB-003** Zero-length content MUST have one canonical empty descriptor; zero-length nonempty-index children, wrong total length and excessive height MUST be rejected.
**ORNA-OGB-004** Metadata-only access MUST not read chunks. Full verification MUST compare every chunk plus the complete raw-content digest before claiming full integrity.
**ORNA-OGB-005** The chosen partition rule MUST be reproducible; insertions and adversarial patterns MUST have explicit bounded-resource behaviour and measured index churn.
**ORNA-OGB-006** Native Git graph validation MUST not be replaced by checking a descriptor string. Missing promised data is unavailable; unknown missing data is corruption/incomplete closure.
**ORNA-OGB-007** All newly persisted Blob fields MUST use reference form regardless of payload size. Small Blobs may use one terminal chunk, not a second inline-row mode.
**ORNA-OGB-008** Cross-representation equality and semantic diff MUST ignore rechunking, graph OIDs and Git repacking while retaining annotation changes.

## Streamed integrity and retainability

CAPTURE-3 protects newly acquired objects before row acknowledgement; PUB-3
protects and publishes the coherent source/schema/store candidate. Node roots
are retained through actual native refs until public reachability is durable.
Do not run a second independent collector over chunks named in SQLite only.

Git content-defined reuse does not make compressed media edits cheap in every
case. New encodings may have almost no shared chunks. Partial clone defers
payloads, not necessarily all tree metadata. Compatible servers may impose
size/transport limits. These limits MUST be surfaced rather than used to
silently omit content from a snapshot or export.
