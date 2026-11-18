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

## The branch rows

`sys.git.branches` is the first relation served from the repository's Git state; `sys.git.tags`, `sys.git.remotes`,
`sys.git.refs` and `sys.git.stashes` follow it, below. The evaluator reads the repository the host attached itself
(`SysHostContext::root`) and `orna-evaluator/src/git_rows.rs` holds what the five members share: the pin, the failure
when the repository or the pin is unavailable, and the pages (ordered by a key the member names: a name, or the index
of a stash). `orna-evaluator/src/git_branches.rs` reads the local branches
(`Repository::local_branches`) and pins every reference to the commit the host attached at
(`SysHostContext::head_commit`): the commit `HEAD` named when `orna run` or the REPL session bound its catalogue
identities, so one activation carries one pin. A `sys.Branch` row exposes `reference` (`sys.BranchRef`, keyed by the
branch name), `name`, `commit` (`sys.CommitRef` over the branch tip), `current` (the branch `HEAD` is attached to; a
detached `HEAD` attaches to none), `upstream` (`sys.RefRef` of the full tracked ref name, for example
`refs/remotes/origin/main`) and `ahead`/`behind` (commits the branch has that the upstream lacks, and the reverse).
The three upstream fields are null together when the branch tracks nothing, and present together when it does,
including `0` counts for a branch level with its upstream. An upstream configured but gone (its ref deleted or its
commit unreadable) has no ref to name and nothing to count against, so it reads as no upstream. With no readable
commit to pin to, a repository that has branches fails the read with `sys.snapshot.incomplete`; a repository with no
branch (an unborn `HEAD`) serves an empty relation. Remote-tracking branches are refs, not rows of this relation;
`sys.Ref.remote` and `sys.git.refs` carry them. `orna serve` pins no `HEAD` of its own, so the relation is not served
there yet and fails closed with `sys.snapshot.incomplete`.

## The tag rows

`sys.git.tags` serves one `sys.Tag` row per tag of `refs/tags`, in name order (`orna-evaluator/src/git_tags.rs`,
`Repository::local_tags`: one `for-each-ref` run, so the tags and the tag objects behind them are one observation). It
reads and pins exactly as the branch rows do. `reference` is the `sys.TagRef` keyed by the tag name (the ref name
without `refs/tags/`) and `name` is that name. `object` is the `sys.GitObjectRef` of the object the tag names: for an
annotated tag, the `object` header of its tag object, one level and not peeled (a tag of a tag names the inner tag
object, not the commit it finally peels to); for a lightweight tag, the object its ref names (a commit, or a tree or
blob, which Git permits). `annotated` says whether the ref names a tag object. `message` and `tagger` are the tag
object's, so a lightweight tag has neither (null). An annotated tag has a message (possibly empty), Git's
`%(contents)`: everything after the headers, a signed tag's signature block included, with the leading blank lines Git
skips dropped. It has a tagger (`sys.PersonIdentity`: `name` and `email`) unless its tag object has no `tagger`
header, as Git's earliest tag objects have none; `email` is null for an identity line whose address is empty
(`Name <>`). Nothing is guessed: a tag whose object cannot be read, a name that is not a ref name, an object id of the
wrong width, text that is not UTF-8, more than 10,000 tags, or a listing past the 4 MiB bound of a Git listing fails the
read with `sys.snapshot.incomplete`.

## The remote rows

`sys.git.remotes` serves one `sys.Remote` row per remote the repository configures, in name order
(`orna-evaluator/src/git_remotes.rs`, `Repository::configured_remotes`: one `git config --null --get-regexp` run over
`remote.<name>.*` and `extensions.partialClone`, so the URLs and the promisor flag of every remote are one observation
of the configuration). It reads and pins exactly as the branch rows do. A remote is every name some
`remote.<name>.<variable>` entry carries, as `git remote` lists them; one with only `fetch` refspecs has no URL and
empty URL lists. `reference` is the `sys.RemoteRef` keyed by the name. `fetch_urls` are the `url` values in
configuration order. `push_urls` are the `pushurl` values, or the `url` values when none is configured: the URLs a push
goes to, as `git remote get-url --push --all` reports them. An empty `url` or `pushurl` clears the earlier ones, as it
does for Git. The configuration is read as written: `url.<base>.insteadOf` rewrites are not applied and nothing
contacts a remote. `promisor` is true when `remote.<name>.promisor` is true or `extensions.partialClone` names the
remote, which is how Git treats both. `last_error` is null: this runtime retains no remote failure to report, so
there is no error to read back.

The type is "Git remote without plaintext credentials", and ORNA-SYS-099 and ORNA-SYS-100 require redaction at the value
boundary with an explicit marker, never plausible text. The repository reader redacts as it reads
(`redact_remote_url` in `orna-repository/src/remotes.rs`), so no caller ever holds the configured text of a URL. The user information of a
`scheme://` URL, the user name included (a token is often the user name), is replaced by `<redacted>`
(`https://account:secret@host/p.git` reads `https://<redacted>@host/p.git`), and the whole address of an `ext::` remote
helper, which is an arbitrary command line, is replaced (`ext::<redacted>`). A `user@host:path` URL and a local path
have no credential syntax and are kept as configured; a local path is a host path, shown as configured on the
developer-run default paths (`orna serve` pins no `HEAD`, so it serves no Git member yet). Configuration the reader does
not understand (a `url` with no value, a `promisor` that is not a boolean, an empty remote name) fails the read with
`sys.snapshot.incomplete`, as does more than 1,000 remotes.

## The ref rows

`sys.git.refs` serves one `sys.Ref` row per ref under `refs/`, in name order (`orna-evaluator/src/git_refs.rs`,
`Repository::committed_refs`: one `for-each-ref` run, so the refs and the objects they name are one observation). It
reads and pins exactly as the branch rows do. `name` is the full ref name (`refs/heads/main`, `refs/remotes/origin/main`,
`refs/tags/v1`, `refs/stash`), the key of the `sys.RefRef` that `reference` holds; it is the key `sys.Branch.upstream`
already names a tracked ref by, so the upstream of a branch is the `reference` of one row here. `target` is the
`sys.GitObjectRef` of the object the ref names, never null: for a symbolic ref the object the ref it points at names, for
an annotated tag its tag object (not the commit the tag object tags: that is `sys.Tag.object`'s business), and for the
refs Git permits to name a tree or a blob, that object. `symbolic_target` is the full name of the ref a symbolic ref
points at (`refs/remotes/origin/HEAD` reads `refs/remotes/origin/main`) and null for every ref that names an object
directly. `hidden` is true for the refs under `refs/orna/` (allocator watermarks, checkpoints, pins and the private
publication candidates) and false for every other ref: they are ordinary, valid Git refs that no branch or tag listing
shows, and ORNA-SYS-050 makes them visible here in explicit developer inspection. `remote` is the `sys.RemoteRef` of the
configured remote a remote-tracking ref (`refs/remotes/<remote>/...`) belongs to, and null for every other ref. A remote
name may hold slashes, so the longest configured name the ref lives under wins (`refs/remotes/a/b/c` belongs to `a/b`
when both `a` and `a/b` are configured). A remote-tracking ref of a remote that is not configured (one since removed)
has no `sys.Remote` row for a reference to name, so its `remote` is null. The remote configuration is read, by
`Repository::configured_remotes`, only when some ref lives under `refs/remotes/`.

`HEAD` and the pseudo-refs (`ORIG_HEAD`, `FETCH_HEAD`, ...) are not under `refs/` and are not rows: `sys.Branch.current`
reports what `HEAD` attaches to. A symbolic ref whose target ref does not exist names no object and Git lists none, so
no row is served for it (`target` is not nullable and nothing is invented to fill it). Nothing else is guessed: a ref
whose object is missing (the listing asks Git for the object type of every ref so it must read the object), a name that
is not a ref name, an object id of the wrong width, text that is not UTF-8, more than 10,000 refs, a listing past the
4 MiB bound of a Git listing, or, for a remote-tracking ref, remote configuration the reader does not understand, fails
the read with `sys.snapshot.incomplete`.

## The stash rows

`sys.git.stashes` serves one `sys.Stash` row per entry of the stash list, newest first
(`orna-evaluator/src/git_stashes.rs`, `Repository::committed_stashes`: one `git stash list` run over the reflog of
`refs/stash`, so the entries are one observation; `git reflog show refs/stash` fails in a repository that has no stash,
where `git stash list` lists none). It reads and pins exactly as the branch rows do. `index` is the `N` of `stash@{N}`
and the key of the `sys.StashRef` that `reference` holds: `0` is the newest entry. It is the position in the list, which
is what the selector counts, and the reflog selector itself is not read (it prints a date instead of the number when
`log.date` is configured), so dropping an entry renumbers the entries after it and a `sys.StashRef` held across the drop
names the entry that now has the index, as `stash@{N}` does. `commit` is the `sys.CommitRef` of the stash commit,
`message` the reflog subject Git lists the entry with (`WIP on main: 1a2b3c4 subject`, or `On main: message` for a
stash made with a message; a message of several lines reads as Git flattened it into the one-line reflog) and `created`
the committer date of the stash commit as an `Instant` of whole seconds. `created` is present for every entry served: a
stash commit whose date Git cannot print refuses the listing instead of reading as null.

The pages of the relation are keyed by the index, which is the cursor (eight big-endian bytes, so the entries order by
index and not by the spelling of the number). A repository with no stash serves an empty relation and needs no pin.
A record that does not have the shape Git writes, an object id of the wrong width, a time that is not a number, text
that is not UTF-8, a stash commit Git cannot read, more than 10,000 entries or a listing past the 4 MiB bound of a Git
listing fails the read with `sys.snapshot.incomplete`; so does the absence of the commit pin the references need.
