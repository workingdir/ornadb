# Work ADR 0122: Mount Names and Repository Key Paths Use Different Spellings

**Status:** Accepted

## Decision

The VFS mount and the repository key path are two naming surfaces governed by
two different documents. Neither borrows the other's spellings, and neither is
an alias of the other. Bead `ornadb-5il1.245.1`.

| Surface | Document | Escape | Empty key component |
|---|---|---|---|
| Mount (`orna mount`) | `profiles/vfs-1.md:35-45` | `%HH`, uppercase hex | `~empty` |
| Repository key path | `tests/path-vectors.json` (ORNA-PATH-001 through 012) | `~hh`, lowercase hex | `~ff` |

Both surfaces escape a literal `%` and a literal `~`, so no spelling on one
surface can be read as a spelling of the other:

| Canonical key component | Mount name | Repository key path |
|---|---|---|
| `london` | `london` | `london` |
| `~2e` | `%7E2e` | `~7e2e` |
| `~ff` | `%7Eff` | `~7eff` |
| *(empty)* | `~empty` | `~ff` |
| `.` | `%2E` | `~2e` |

The mount therefore does **not** accept `~hh` as an alternative spelling. In a
mount component `~hh` is a literal `~` followed by hex digits, because
`profiles/vfs-1.md:40-41` escapes every literal `~`. Accepting it would give one
canonical key two host names, which is the aliasing the same paragraph forbids
for `~empty`.

Implementation already matched both documents when this decision was recorded,
on each surface for its own document: the mount's `project_component`,
`project_key_component` and `unproject_component` (uppercase `%HH`, `~empty`,
`~hh` refused) and the repository's `orna_value::path_encode_component`,
`path_decode_component` and `orna_storage::LoosePath::for_key` (lowercase
`~hh`, `~ff`). No behaviour changed; this ADR and the test below record and
pin the split.

## Evidence

`crates/orna-cli/tests/vfs_key_escape_spellings.rs` drives a real FUSE mount
(`spawn_mount_process`, `ORNA_REQUIRE_FUSE=1`) of a repository holding the rows
`~2e`, `~ff` and `~empty`, and asserts:

1. the rows appear as `%7E2e`, `%7Eff` and `%7Eempty`, and neither `~2e` nor
   `%2E` resolves (a `~hh` component is not decoded as an escape);
2. `%7Eff` and `%7Eempty` are two different rows, and `~ff` resolves to
   nothing on the mount;
3. `unproject_component("~empty")` is the empty text and
   `unproject_component("~ff")` is refused, while
   `orna_value::path_encode_component("") == "~ff"` and
   `LoosePath::for_key("Music", &[""])` is `Music/~ff.orna` — one key, two
   spellings, each per its own document.

`crates/orna-cli/tests/vfs_composite_key_mount.rs`
(`empty_key_component_projects_as_tilde_empty`) stores and publishes a
composite row whose first key component is empty: the mount exposes it at
`Stock/~empty/widget/data.orna` while `LoosePath::for_key` spells the same
component `Stock/~ff/widget.orna`. A single-component empty text key still
cannot be stored (`ornadb-5il1.462.1`), so that case is pinned by the mount's
projection and inverse rule instead of by a row.

## Consequences

- A repository spelling read through the mount is a different literal key, not
  an escape. A client must use the mount's own spelling; `%HH` and `%hh` are
  not interchangeable in either direction.
- A single-component empty text key cannot be stored yet (the table row key
  rejects an empty key, `ornadb-5il1.462.1`), so the mount half of the empty
  key is pinned by the mount's own projection and inverse rule; the repository
  half is pinned by the ORNA-PATH encoders. The composite case (an empty key
  component beside another component) is stored and mounted normally.
- The Gate E evidence record keeps the two-surface statement beside the mount
  naming rules it already quotes (`docs/conformance/gate-e-evidence-2026-10-29.md`,
  E8 "Key encoding").
