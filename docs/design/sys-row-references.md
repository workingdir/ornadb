# `sys` row references: the canonical encoding of every `sys.*Ref`

**Bead:** `ornadb-fthi5`. **Authority:** `reference/Orna-1.1.0` (`source/34-system-reference.md`, "Typed row-reference
aliases"; `source/15-system.md`, ORNA-SYS-111 to ORNA-SYS-114 and ORNA-SYS-125; `source/29-formats.md`, tag 60010;
`api/sys.json`, `reference_aliases` and the `key_fields` of each relation).

## What the specification fixes

Each of the 78 aliases (`sys.BranchRef`, `sys.CommitRef`, `sys.ColumnRef`, ...) is a pinned `sys.RowRef<T>` for the
canonical relation `T`. A reference "preserves the row natural key and resolution context", "does not grant
authority" and, for a live relation, "is runtime-generation-bound". Every canonical row exposes it as `reference`
(ORNA-SYS-111), it introduces no synthetic identifier (ORNA-SYS-112), and a row is never implicitly converted to a
reference or back (ORNA-SYS-113). Possession of a reference bypasses no visibility, retention, redaction, trust or
precondition check (ORNA-SYS-114). The canonical value of a reference is the OVB tag-60010 value
`[database_id, table_id, key, snapshot]`.

The specification fixes none of the sixteen-byte `table_id`s and none of the key encodings, so both are
implementation coordinates.

## The encoding

`orna_value::relations` holds one `RelationSpec` per relation of `api/sys.json`, in catalogue order:

| Slot | Content |
|---|---|
| `database_id` | the attached database's UUID. Every snapshot pin and every nested reference names the same database. |
| `table_id` | `RelationSpec::table_id`, a versioned implementation constant per relation. The eleven relations that had a reference constructor before the registry keep their constants (`SYS_OBJECT_TABLE_ID`, `SYS_RUN_TABLE_ID`, ...). |
| `key` | the natural key `api/sys.json` declares: the bare component of a one-field key (`sys.Branch`: the name as text), or the array of the components in declaration order (`sys.Failure`: five components). |
| `snapshot` | the CWD or commit pin the reference resolves in; dereferencing uses it and never the current CWD (ORNA-SYS-019). |

A key component has the representation of its declared type (`KeyPartKind`):

| Declared type | Component |
|---|---|
| UUID-backed `sys.*Id` | tag-37 UUID |
| `sys.RevisionId` | 32-byte string |
| `sys.GitOid` | `[algorithm, bytes]`: `["sha1", 20 bytes]` or `["sha256", 32 bytes]` (the representation inside tag 60023, without that tag's nominal wrapper, because the relation identity already fixes the key's type) |
| `Str`, `Str?` | nonempty text; null when an optional component is absent |
| `Int` | nonnegative integer that fits 64 bits |
| `Path` | tag-60024 `[flavour, text]` |
| `sys.XRef`, `sys.XRef?` | the canonical form of that reference (tag 60010), whose relation and key are checked recursively; null when absent |
| closed `sys` enum | tag-60008 value of a payload-free variant of that enum |
| `sys.ChangeTarget`, `sys.SourceSpan?` | the canonical value of the supporting record |

The encodings that predate the registry are preserved bit for bit: `sys.Object` is keyed by `[id, snapshot pin]`,
`sys.Snapshot` by the 32-byte identity of its own pin, `sys.Checkpoint` and `sys.Failure` by their text components,
`sys.Materialization` by two digests. A test builds each of them through the registry and through its dedicated
constructor and compares the references.

## Constructing and reading a reference

- `orna_value::SysRef::new(&relations::BRANCH, database, snapshot, vec![KeyPart::Text(name)])` builds any
  reference. The checked helpers `branch_reference`, `commit_reference`, `git_ref_reference`, `tag_reference`,
  `remote_reference`, `stash_reference`, `git_object_reference` and `object_keyed_reference` (every relation keyed by
  one `sys.ObjectRef`: `sys.Table`, `sys.Column`, `sys.Namespace`, ...) wrap it.
- `SysRef::from_row_ref` accepts a decoded tag-60010 value only when its `table_id` names a canonical relation and
  its key has that relation's shape. It proves coordinates, never that the row exists or may be read.
- `orna_exec::Value::SysRef` holds a checked reference. `Value::sys_ref` builds it, `Value::as_sys_ref_of(&relations::BRANCH)`
  reads a `sys.BranchRef`, and a stored reference reads back as the same value. A reference to an unknown
  relation, or with a key that is not its relation's, stays an opaque `Value::RowRef`.

`crates/orna-sys/tests/relation_reference_registry.rs` proves the registry names exactly the relations, aliases
and key fields of the generated catalogue, so a relation added to `api/sys.json` fails that test until it is
registered.

## What is not covered

A reference is coordinates only. The provider that serves a relation checks that the row exists, is retained
and is visible, and serves the rows with their `reference` fields built here, never `null` and never a bare
identifier.

## The catalogue rows

`sys.catalog.tables` and `sys.catalog.columns` are the first relations served with these references
(`orna_check::catalogue_rows`). The host reads the object identity inventory committed at one commit and hands the
ids of its live `ObjectKind::Table` and `ObjectKind::Field` (`Table.column`) records, the database id and that commit
as the snapshot pin to `orna_check::catalogue_identities::CatalogueIdentities`. Over them a `sys.Table` row exposes
`reference` (`sys.TableRef`), `object` (`sys.ObjectRef`) and `definition` (`sys.DefinitionRef`, whose id is the table
identity: the only rename-stable per-table UUID), and a `sys.Column` row exposes `reference` (`sys.ColumnRef`),
`object` and `table` (`sys.TableRef`). A table or column with no committed identity has no spec-correct reference, so
`CatalogueRows::build` fails and a read of the relation fails with `sys.snapshot.incomplete` (the required snapshot
objects are unavailable); only a project of no tables serves an empty catalogue without identities.
`sys.Column.type`, `sys.Table.key`, `storage`, `references` and `assertions` stay in their earlier form: their targets
(`sys.Type`, `sys.Key`, `sys.Storage`, `sys.Reference`, `sys.Assertion`) are keyed by catalogue object identities that
the committed inventory does not hold for builtin types, keys or storage.
