# 33. Ingestion, inspection and derived knowledge {#ingestion}

## One way to put data into the database

Ingestion is an ordinary function that captures an input and performs ordinary table operations. There is no `ingest`, `asset`, `file` or `source` declaration kind. This extends the same pattern used by the retained sensor example, not a separate import pipeline language.

```orna
pub table Song(id: Str) {
    title: Str,
    content: Blob,
}

pub fn import_song(id: Str, title: Str, root: Str, path: Str) {
    let content = sys.blob.capture_file(root, path, max_bytes: 67108864);
    Song.insert({ id: id, title: title, content: content });
}

pub fn bytes_in_song(id: Str): Int {
    let song = Song | one(item => item.id == id);
    sys.blob.length(song.content)
}
```

A shipped standard library exposes an ordinary `std.io.fs.read_blob` wrapper; the example uses the core operation so it does not depend on an unavailable package pin. The example is valid source against the 1.1 API, not a claim that its engine implementation has executed.

**ORNA-INGEST-001** Import MUST capture the actual bytes, not a mutable source path or URL. Once the row commit is acknowledged, removing the source MUST NOT invalidate the imported Blob. Import MUST NOT automatically create a global media catalogue or unrelated derived records.

**ORNA-INGEST-002** File capture MUST require an authorised root and a safe relative path, resolve beneath the held root without traversal/symlink races, reject device/FIFO/socket/directory inputs, enforce the byte bound, and avoid implicit shell construction. Missing permission, changed source, quota exhaustion and cancellation MUST have distinct typed outcomes without leaking private path details.

**ORNA-INGEST-003** A regular-file capture MUST compare identity, size and modification observations before/after the streamed read and reject detected changes as `sys.blob.source_changed`. Without an OS snapshot or cooperating source lock it guarantees the exact observed byte stream, not an atomic historical photograph of a concurrently modified external file. Stronger source-snapshot claims require the relevant lock/snapshot witness.

**ORNA-INGEST-004** Long byte acquisition MUST NOT hold an exclusive database write lock or one open SQLite transaction for its duration. Its immutable result is installed through the existing optimistic/activation boundary. On conflict, Orna-controlled changes roll back; external reading is not undone. A retry uses the captured protected Blob where valid, not an unnoticed reread of a newer source.

**ORNA-INGEST-005** Programmatic/network ingestion MUST use the same owned Blob writer and atomic row/dependency boundary as regular-file import. Adapter staging is local temporary state, not another persistent content service. Quotas MUST include quarantined, protected, pending and retained objects as separate observable categories.

**ORNA-INGEST-006** Duplicate ingestion of identical bytes MAY share payload storage, but MUST NOT silently merge two distinct application rows or import events. Row deduplication follows the application's keys and transaction rules; content deduplication follows raw-byte identity, regardless of media annotations; annotated value equality remains separate.

## Properties without a second database

There are three layers: intrinsic content properties (length/digest); decoded facts from bytes (format, dimensions, duration, embedded tags); and authored/derived observations (labels, transcripts, visual matches, relationships). They may be displayed together, but their origin and certainty are different.

**ORNA-MEDIA-001** Default inspection MUST make Blob length and content identity available without a plugin or payload read. Common-format inspection SHOULD be supplied by bundled, version-pinned ordinary library functions, not require a separately administered extraction service. An unknown format remains storable, queryable by record metadata and retrievable.

**ORNA-MEDIA-002** A decoder MUST consume the selected immutable Blob through bounded reads, not reopen its import pathname. Its output MUST distinguish valid properties, absent embedded properties, unsupported formats, malformed content and resource/unavailable failures. File extensions and supplied MIME strings MUST NOT substitute for byte validation.

**ORNA-MEDIA-003** Decoding/sniffing MUST be side-effect-free with respect to database state; recording its results is an explicit ordinary transaction. Inspecting a Blob MUST NOT silently start network analysis, purchase inference, run embedded scripts/macros, execute source contained in a document or persist a catalogue of guesses.

**ORNA-MEDIA-004** A decoded property's cache key MUST bind content identity, decoder code/dependency revision, output schema and effective options/bounds that affect the result. A partial result obtained under a small budget MUST NOT masquerade as the complete result under another budget. Cache data belongs in local runtime/cache storage; it MUST NOT be required to interpret a committed record.

**ORNA-MEDIA-005** Native libraries and explicitly authorised decoder processes MAY implement codecs behind typed operations. They MUST use bounded time/memory/output, cancellation, argument-vector execution and protected temporary resources. Their presence MUST NOT create a separate file catalogue or bypass transaction, source-pin and privacy rules.

The first implementation milestone uses a PNG image and PCM RIFF/WAVE recording. JPEG, MP3, FLAC, MP4, EXIF and further properties are ordinary versioned decoder additions, not new Blob subtypes. The release does not promise every file format can be decoded. Browser playback support similarly depends on the selected renderer's actual codecs; failure to preview never prevents byte retrieval.

## Analysis and knowledge

A vision/transcription function is ordinary explicit Orna behaviour with declared effects, not special query syntax. Relational filters first narrow candidate records; an analysis function may examine their Blob values. Lack of an analysis index can require scanning all candidates and MUST be exposed in the plan/budget.

**ORNA-KNOW-001** A persisted analysis result MUST identify every input revision that affects it, function/dependency revision, provider/model coordinate, effective parameters/prompt, output schema, time and success/failure/uncertainty state. Source-byte identity alone is insufficient when related notes, prior outputs or external context participate.

**ORNA-KNOW-002** Persisted annotations/results MUST be ordinary typed records related to their inputs. User corrections and original bytes MUST be preserved independently of regenerated results. Re-analysis MUST NOT silently replace historical observations or mutate an older snapshot.

**ORNA-KNOW-003** External model use, disclosure of content and nontrivial cost MUST require explicit host/operator policy. Recorded provenance does not guarantee a changing external service can reproduce the result. Reproducibility MUST distinguish retained output replay from re-execution with retained model artifacts.

**ORNA-KNOW-004** Historical evaluation MUST use the selected program/data/result revisions. It MUST NOT silently run the host's newest decoder/model to fill a missing historical property. Explicit new analysis of old immutable content is a new operation with a new result, not a revision of history.

There is no mandatory `Asset`, `File`, `Embedding` or `Analysis` system relation. Applications use tables and relationships. A small shared library can define useful report/annotation types without giving its tables privileged storage semantics.
