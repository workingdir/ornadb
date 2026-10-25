# 31. Annotated immutable Blob values {#blob-values}


## One value; two identities

A `Blob` in this final 1.1.0 contract is immutable binary content with a
canonical declared media type and optional suffix hint. It has no intrinsic
path, owner row, mutable file identity or separate history. Every constructor
has a defined declaration: unknown content uses `application/octet-stream`.
A `Song(id)` row owns its application identity and may replace its Blob value.

**ORNA-BLOB-001** Blob MUST represent a finite octet sequence with canonical media type and optional suffix hint. The metadata MUST NOT alter the raw-byte content commitment.
**ORNA-BLOB-002** This release MUST NOT add intrinsic `File`, `Asset`, `Bytes` or `Clob` synonyms or mandatory tables. Existing `sys.File` source-catalogue objects remain distinct from media values.
**ORNA-BLOB-003** Blob MUST work in ordinary stored/nested values; Blob primary keys remain inadmissible.
**ORNA-BLOB-004** Content identity MUST be `(length, SHA256(raw bytes))`, independent of MIME, suffix, Git object header/algorithm, compression, chunking, path and snapshot.
**ORNA-BLOB-005** Blob value equality MUST compare canonical annotations and exact content. `same_content` ignores annotations. Equal digests MUST NOT override detected unequal bytes; independently supplied equal-digest content without a shared admitted witness requires exact streamed comparison or explicit unavailability.
**ORNA-BLOB-006** Detected content collisions MUST fail without overwriting or aliasing an existing content identity.

A Git OID identifies an encoding/layout, not the logical content. A descriptor
is a commitment, not evidence that every promised chunk has been downloaded.
Full verification checks all bytes; range verification checks the fetched
chunk bytes and the selected descriptor path only.

## End-to-end lazy materialization

**ORNA-BLOB-007** Loading a row, passing a same-database Blob reference, showing metadata, reading length/digest or changing MIME/title MUST NOT hydrate media just to form a `Vec<u8>`, encode a row or calculate its semantic digest.
**ORNA-BLOB-008** Held values MUST retain their exact resolution context for their owner/pin lifetime; unavailable/expired values MUST NOT rebind through a later row or HEAD.
**ORNA-BLOB-009** Structured portable annotated values MUST use negotiated OVB-2; old OVB-1 keeps its original bytes-only semantics. No implicit annotation-dropping or reference-in-byte-slot conversion is allowed.
**ORNA-BLOB-010** Inspectors MUST display type, length, declaration and content commitment without payload dumps or remote analysis. The VFS/row representation MUST use small references, not base64 media.

## Shared API

`api/sys.json` is the exact callable inventory. The existing storage functions
remain; these additions make the annotation contract explicit:

```orna
sys.blob.media_type(value)                 // Str, metadata only
sys.blob.suffix(value)                     // Str?, metadata only
sys.blob.annotate(value, media_type, suffix: null)
sys.blob.same_content(left, right)         // Bool, may read bytes
```

`annotate` returns a new annotated value sharing bytes; it never mutates a
content-global MIME registry. An invalid declaration/hint fails before change.
The old constructors keep their signatures and supply the octet-stream/null
annotation by default. An importer may explicitly annotate after capture.
A resource has package-owned metadata if declared; otherwise the same fallback.

**ORNA-BLOB-011** `read` MUST check nonnegative offsets/lengths, offset at/before EOF, checked arithmetic and at most 8 MiB per call; only known EOF clips a successful result.
**ORNA-BLOB-012** Range acquisition MUST fetch required descriptors/intersecting chunks and only separately bounded read-ahead. Whole Git-object overfetch MUST be measured, not called exact range transfer.
**ORNA-BLOB-013** BlobWriter MUST be owner/runtime-bound and nonserializable, with open/finished/aborted states, serialized append/finish/abort, idempotent terminal outcomes and cancellation cleanup.
**ORNA-BLOB-014** Begin/append/finish MUST enforce declared expected length, total quota and the format maximum 2^63−1. No unbounded buffer or temporary-file bypass is allowed.
**ORNA-BLOB-015** Exact snapshot hydration MUST respect offline/network policy and cancellation; host `capture_file` is external I/O and remains forbidden in historical/pure/presenter/assertion contexts.
**ORNA-BLOB-016** Format-3 row/argument/consumer semantic commitments MUST use the versioned SOV-3 domains, not Git layout OIDs or eager full-byte encodings.
**ORNA-BLOB-017** Cross-format comparison MUST compare decoded values under explicit migration semantics, not unrelated digest bytes.
**ORNA-BLOB-018** The existing live protocol MUST reject unnegotiated OVB-2/storage tags. Use typed safe inspector/reference responses and separately authorized content I/O, not automatic full-Blob result serialization.
**ORNA-BLOB-019** Copying into an independent database MUST establish and protect destination closure before acknowledging a dependent row, with no hidden dependency on the original user's path.
**ORNA-BLOB-020** Generic slicing/concatenation MUST produce octet-stream/null annotations unless it is an exact identity no-op; part of a JPEG does not automatically remain image/jpeg.
**ORNA-BLOB-021** Byte export MUST mean original raw bytes. Full annotated value export MUST preserve metadata explicitly; an export operation cannot silently switch between these two promises.

A 10 GB binary edit may require a whole-result streaming pass to calculate its
raw SHA-256. Bounded memory and storage reuse do not imply zero read cost.
Annotation-only edits MUST avoid this pass.
