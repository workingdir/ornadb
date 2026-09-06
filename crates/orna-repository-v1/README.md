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

The format record contains:

```orna
{repository_format: 1, storage_profile: "compact-storage-v1"}
```

The database record contains `database_id`, a string holding the canonical
lowercase, hyphenated UUID identity generated at initialization. The typed
`DatabaseId` API also exposes its 16 bytes for runtime and storage adapters.

Valid existing metadata is read without replacement. Reinitialization retains
the identity and metadata bytes; incomplete, malformed, or unsupported
metadata is an error. Metadata belongs in database snapshots, but the caller
decides when to stage and commit it with the source.

Fresh metadata publication currently uses Linux's atomic no-replace directory
rename. Other platforms return an explicit unsupported error instead of using
a replacement-prone fallback. Both records are staged and synced before
publication; initialization never installs one final metadata file at a time.

Local coordination files remain in Git's per-worktree administrative area,
separate from tracked metadata. Repository initialization does not by itself
establish table execution or persistent runtime availability.
