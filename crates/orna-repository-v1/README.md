# Repository boundary

This crate preserves Git's worktree, ordinary index, and selected HEAD while
providing Orna repository metadata and local coordination primitives.

## Initialization metadata

`initialize_repository` initializes Git with its configured defaults and writes
the metadata needed to identify an Orna database. It does not stage files,
create a commit, or start a runtime service. A missing root module is created
empty; existing source is preserved.

The metadata uses ordinary Orna records. The exact field spelling below is
this implementation's encoding of the requirements in specification chapters
19 and 23, not an additional language grammar.

`.orna/database.orna` holds the repository format and the database identity:

```orna
{
    repository_format: 3,
    database_id: "00000000-0000-4000-8000-000000000000",
}
```

`database_id` is the canonical lowercase, hyphenated UUID generated at
initialization. The typed `DatabaseId` API also exposes its 16 bytes for
runtime and storage adapters.

A separate `.orna/format.orna` is a legacy input, never a format-3 authority
(ORNA-META-002). The records the legacy writers produced are recognised:
OrnaDB 1.0 wrote `{repository_format: 1, storage_profile:
"compact-storage-v1"}` beside a `{database_id: "…"}` sidecar, and the previous
1.1.0 draft defined a closed format-2 record. Those snapshots stay readable
and read-only: this build has no writer for them, so publication and durable
runtime writes on such a `HEAD` are refused. The one exception is a prepared
`orna migrate copy` destination, whose runtime receives the carried consumer
checkpoints.

Valid existing metadata is read without replacement. Reinitialization retains
the identity and metadata bytes; incomplete, malformed, or unsupported
metadata is an error. Metadata belongs in database snapshots, but the caller
decides when to stage and commit it with the source.

Fresh metadata publication currently uses Linux's atomic no-replace directory
rename. Other platforms return an explicit unsupported error instead of using
a replacement-prone fallback. The record is staged and synced before
publication, so initialization never installs a partial metadata directory.

Local coordination files remain in Git's per-worktree administrative area,
separate from tracked metadata. Repository initialization does not by itself
establish table execution or persistent runtime availability.
