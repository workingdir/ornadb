# Historical input — not the current specification
`previous-1.1.0-draft.zip` is the exact supplied earlier authoring bundle. It is
included for migration readers, golden-byte comparison and provenance only.
Its format-2, loose/compact/hybrid writers, bytes-only Blob semantics and old
execution reports are NOT active 1.1.0 final requirements. Do not extract it
alongside current chapters or treat its tests as current execution evidence.

The current package keeps the requested language version 1.1.0 and uses the
publication revision `final-2026-10-05`. Its new repository writer is format 3.
This is an explicit supersession of an authoring draft, not a claim that two
incompatible published engine formats can be identified by a version string
alone. Pin `release.json`/the artifact SHA-256 when handing it to an agent.

Retained format-1/2 commits are decoded by their original value/storage
profiles. No history rewrite or metadata-only downgrade is permitted.
