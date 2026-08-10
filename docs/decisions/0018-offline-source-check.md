# ADR 0018: Source Check Is an Offline One-File Compiler Command

**Status:** Accepted

## Decision

The first developer source command is exactly:

```text
orna source check <file.orna>
```

It checks one regular UTF-8 file as the complete source for a new application.
It accepts no flag, standard-input form, directory, glob, second file, active
revision, or database target. The `<file.orna>` text in usage describes the
path argument. The command does not require an `.orna` suffix.

This is a standalone compiler check. It ends at one
`StandardApplicationCheckReport`; it does not compute a semantic diff, call
`prepare`, create a `DeployableRevision`, apply a candidate, activate a
revision, or create a durable source or catalogue revision. Its empty-base
identity is an observable in-memory checking sentinel, not durable identity or
continuity evidence.

The command is offline. It does not read configuration or environment
variables, connect to PostgreSQL, inspect an Orna service or instance, use the
bundled PostgreSQL runtime, open a network connection, start a child process,
or open a filesystem path for writing. Apart from output to the caller-supplied
standard-error stream, it requests no filesystem content, namespace, or
metadata mutation. Ordinary file reads can update filesystem access time.
Standard input is never read. Standard output is always empty.

## Compiler and standard-library boundary

After the verified-standard compiler work required by work ADR 0016, the
compiler exposes this exact public seam:

```rust
pub fn check_new_application(
    bundle: &SourceBundle,
    standard: &CheckedStandardLibrary,
) -> Result<StandardApplicationCheckReport, NewApplicationCheckError>
```

`NewApplicationCheckError` is a public compiler-owned error with exactly these
variants:

```rust
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NewApplicationCheckError {
    SourceUnitCount { actual: usize },
    Catalogue { source: CatalogueSnapshotError },
    Context { source: StandardApplicationContextError },
}
```

Its exact `Display` text is:

```text
SourceUnitCount    new-application check requires exactly one source unit; received <actual>
Catalogue          new-application check could not create the empty application catalogue: <source>
Context            new-application check could not establish the standard application context: <source>
```

`<actual>` is the decimal `usize` value. For `Catalogue`, `<source>` is the
exact `CatalogueSnapshotError` display text. For `Context`, `<source>` is the
exact `StandardApplicationContextError` display text.
`std::error::Error::source()` returns `None` for `SourceUnitCount` and
`Some(source)` for `Catalogue` and `Context`.
`NewApplicationCheckError` implements
`std::error::Error`. No other variant or implicit conversion is accepted.

`check_new_application` reads `bundle.len()` first. If the value is not exactly
`1`, it returns `SourceUnitCount` before parsing source or starting semantic
checking. For one source unit, it calls `CatalogueSnapshot::new` with the empty
sentinel and empty vectors, maps a failure to `Catalogue { source }`, then
constructs `StandardApplicationCheckContext::try_new` from that catalogue and
the supplied checked standard library, maps a failure to `Context { source }`,
and returns `check_standard_application`'s distinct report. It never receives
or wraps an `orna_standard::StandardLibraryError`, and it uses no `panic!` or
`expect` path for empty-catalogue construction.

The gate order is `SourceUnitCount`, `CatalogueSnapshot::new` mapped to
`Catalogue`, context construction mapped to `Context`, then
`check_standard_application`.

The public path and module tests use the same production-private empty-catalogue
construction seam. Module tests supply a typed `CatalogueSnapshotError` result
through that seam together with hostile context data, proving that `Catalogue`
wins before context construction. The seam has no public interface and no
test-only production branch.

The compiler also exposes this exact sentinel identity:

```rust
pub const EMPTY_APPLICATION_CATALOGUE_REVISION_ID: CatalogueRevisionId =
    CatalogueRevisionId::from_bytes([0; 16]);
```

The all-zero value is reserved only for the ephemeral empty application
catalogue used by `check_new_application`. It is distinct from work ADR 0016's
standard `CatalogueRevisionId`, whose bytes are fifteen zero bytes followed by
`01`. A successful `StandardApplicationCheckReport` exposes the all-zero
sentinel through its distinct checked bundle's base catalogue revision.

The shared revision model exposes this exact public role type:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurableCatalogueRevisionRole {
    ActiveOrRecoveredApplication,
    ActiveOrRecoveredStandard,
    DeployableExpectedBase,
    DeployableParent,
    DeployableCandidate,
}
```

The existing public `RevisionInvariantError` gains exactly this typed variant,
alongside its existing variants:

```rust
ReservedOfflineCheckCatalogueRevision {
    revision: CatalogueRevisionId,
    role: DurableCatalogueRevisionRole,
}
```

The variant has this one exact `Display` text for every role:

```text
the reserved offline-check catalogue identity cannot be used in a durable revision
```

Its `revision` field contains exact
`EMPTY_APPLICATION_CATALOGUE_REVISION_ID`. Its `role` field identifies the
rejected structural position without requiring a caller to parse the display
text. Like the other `RevisionInvariantError` variants, its
`std::error::Error::source()` value is `None`.

`ActiveOrRecoveredApplication` identifies the application catalogue supplied
to active construction, including raw recovery. `ActiveOrRecoveredStandard`
identifies the standard catalogue supplied to a standard snapshot or active
context, including raw recovery. The three `Deployable*` roles identify the
three fields of `DeployableRevisionInput` without combining their evidence.

The sentinel can never identify an active, stored, prepared, or recovered
catalogue revision. Active-revision construction and recovery fail closed if
an application or standard catalogue uses it. `DeployableRevision` construction
checks its expected-base catalogue, explicit parent catalogue, and candidate
catalogue in that order and rejects the sentinel at every position with the
corresponding typed role before another deployable invariant or allocation.

Report separation is compile-time: no legacy `prepare` signature accepts
`StandardApplicationCheckReport` or `CheckedStandardApplicationBundle`, and
neither type offers a conversion, dereference, borrow, inner, or extraction
path to `CheckReport` or `CheckedBundle`. The sentinel remains visible in the
distinct bundle and the durable core rejection remains fail-closed. A later
`prepare_standard_application` row owns standard-application preparation and
any preparation-specific sentinel rejection.

The seam checks the supplied bundle against an empty application catalogue and
the supplied checked standard-library overlay. It does not accept an active
application catalogue, a source revision, or a prior `RevisionPair`. Every
application declaration is new for this check. A rename, delete, or reference
that needs an earlier application definition has no continuity context and
cannot infer one.

The empty application base is a checking input only. The returned
`StandardApplicationCheckReport` retains its lossless parse report,
diagnostics, and, on success, distinct checked definitions labelled with the
exact sentinel. The caller does not prepare that report through the legacy
path. The compiler returns diagnostics in its established order and preserves
the invariant that a report with no diagnostics contains one complete distinct
checked bundle.

The only standard-library authority accepted by this seam is a
`CheckedStandardLibrary`. Host composition reconstructs the embedded standard
snapshot, calls `orna_standard::verify_standard_library_snapshot`, and passes
the resulting core capability to `check_standard_library_source` before it
calls `check_new_application`. Retained-source mismatch, accepted-golden
mismatch, standard verification failure, and standard source reconciliation
failure all stop host composition before this seam. The compiler accepts no raw
verified snapshot, standard-owned error, manifest, digest, or trust flag.

The source-independent `StandardLibraryManifest` from work ADR 0016 can help
validate the retained standard source, but it has no source, origins, hashes,
accepted digest, or verification capability. The command must never pass that
manifest or its catalogue to application checking as standard-library
authority. It must not fall back to `StandardScalar` spelling rules or another
hard-coded type-name table when host standard composition fails.

The check therefore uses the one standard identity relationship accepted by
work ADR 0016:

```text
BOOLEAN             standard-prelude binding
BOOL                standard-prelude binding
std.boolean         qualified standard binding
std.types.boolean   canonical value-type definition
```

All four names resolve to the same Boolean `TypeId`. Standard types in function
signatures retain the exact `ValueType` and `NamedType` reference evidence
required by work ADR 0016. Normal application checking also rejects protected
`std` declarations, `KERNEL CONTRACT`, qualified type exports, and prelude type
exports with the accepted `ORNA0303` diagnostics and spans.

The application bundle contains exactly one source unit. Its logical path is
the path token exactly as supplied in `argv`. The standard source is a separate
verified overlay and is not copied into that application bundle. Apart from
the standard overlay, every schema, type, function, and other application name
needed by the candidate must be declared in that one file. The command does not
search neighbouring files, follow source imports, or discover a project root.

## Command shape and global usage

After `argv[0]`, the command accepts exactly these three tokens:

```text
source
check
<one path>
```

The first two tokens must be exact Unicode text. The path token must be valid
UTF-8, non-empty, contain no Unicode control character, contain neither
`U+2028` nor `U+2029`, and not start with the ASCII hyphen `-`. A path such as
`./-x` is valid. There is no `--` path separator and no `-` standard-input
convention.

The command does not interpret wildcard characters or expand a glob. A shell
can expand tokens before Orna starts, but an expansion that supplies more than
one path fails the exact command-shape rule. Orna receives and checks only the
one resulting path token.

An absent path, empty path, non-UTF-8 path, control character, `U+2028`,
`U+2029`, leading-hyphen path, flag, additional token, or another command shape
is a usage error. It writes the exact global usage text below, including the
final line feed, to standard error and exits with status `2`:

```text
Usage:
  orna server run
  orna server upgrade
  orna server backend-shell
  orna source check <file.orna>
```

This appends the source-check line anticipated by work ADR 0017. It does not
change the accepted shapes or operation of `orna server run`,
`orna server upgrade`, or `orna server backend-shell`.

## File boundary

The operating system resolves a relative path against the process current
working directory. The command does not canonicalise the path or replace it
with an absolute path for compiler locations or diagnostics. The source
unit's logical path and every rendered path remain the exact `argv` text.

The resolved target must be a regular file. A symbolic link whose final target
is a regular file is accepted. An absent path, dangling symbolic link,
directory, device, socket, first-in-first-out special file, or other non-regular
target is rejected as a read failure.

The command opens only that source path and performs one complete byte-read
phase through the opened regular file. It reads through end of file. It does
not apply a command-specific size limit. A platform read failure remains a
read failure.

The exact bytes are decoded once as UTF-8. The command does not remove a UTF-8
byte-order mark, change line endings, normalise Unicode, trim whitespace, or
add a final line feed. An empty regular file is valid UTF-8 and is checked as
an empty new application. Byte spans therefore include all submitted bytes.
For example, a retained UTF-8 byte-order mark occupies offsets `0..3`, a CRLF
occupies two bytes, and a multi-byte Unicode character advances offsets by its
UTF-8 byte length.

Any open, file-type, metadata, or read failure writes this exact line with the
submitted path substituted and exits with status `1`:

```text
orna: could not read source file: <path>
```

Invalid UTF-8 writes this exact line with the submitted path substituted and
exits with status `1`:

```text
orna: source file is not valid UTF-8: <path>
```

Neither line appends an operating-system error, an absolute path, or source
bytes.

## Operation order

The command performs its work in this exact order:

1. validate the raw command shape;
2. validate the UTF-8 path token;
3. resolve, open, confirm, and read the one regular source file;
4. decode the exact bytes as UTF-8;
5. construct the one-unit `SourceBundle` with the exact logical path;
6. confirm that the constructed bundle contains exactly one source unit;
7. reconstruct and verify the embedded standard library, then pass the
   verified capability to `check_standard_library_source`;
8. call `check_new_application` with the resulting `CheckedStandardLibrary`; and
9. escape each diagnostic message and render the diagnostics in compiler
   order.

A failure stops this sequence. In particular, invalid command shape does not
touch the source path, a read or UTF-8 failure does not verify the standard
library, an invalid source-unit count does not inspect the standard library,
and a standard verification failure does not check application source. No step
reads standard input, configuration, environment values, a database, or a
service.

## Diagnostics and exit status

If embedded standard-library reconstruction, accepted-golden verification, or
compiler standard-source reconciliation fails before `check_new_application`,
or if `check_new_application` returns `NewApplicationCheckError::Catalogue` or
`NewApplicationCheckError::Context`, the command writes exactly this line to
standard error and exits with status `1`:

```text
orna: embedded standard library could not be verified
```

It does not expose a manifest error, digest, identity, retained standard
source path, or internal error chain.

The host matches the public non-exhaustive `NewApplicationCheckError` with
explicit `Catalogue { .. }` and `Context { .. }` arms and a mandatory wildcard
arm. All three arms write the same embedded-standard line and exit `1`. The
wildcard remains fail-closed when the compiler adds a future error variant
before host code has an explicit mapping.

The accepted CLI construction always supplies one source unit. If the host
integration violates that invariant before or during the compiler call, it
treats `NewApplicationCheckError::SourceUnitCount` as a command-shape failure,
writes the exact global usage, exits `2`, and does not expose the compiler error
display text. Direct compiler callers receive the typed error and exact display
contract above.

Each compiler diagnostic is one exact line on standard error:

```text
<path>:<start>..<end>: <code>: <message>
```

`<path>` is the exact source-unit logical path. `<start>` and `<end>` are
zero-based UTF-8 byte offsets, and the end is exclusive. `<code>` is the
stable Orna diagnostic code, such as `ORNA0001` or `ORNA0303`.

Before inserting `<message>`, the command scans the compiler message as Unicode
scalar values and applies this exact escaping in order:

1. backslash becomes `\\`;
2. line feed, carriage return, and tab become `\n`, `\r`, and `\t`;
3. every other Unicode control character, plus `U+2028` and `U+2029`, becomes
   `\u{HHHH}`, with uppercase hexadecimal and a minimum of four digits; and
4. every other scalar value remains unchanged.

Additional hexadecimal digits are used when a value needs them. This escaping
is one pass, and emitted escape text is not scanned again. It applies only to
`<message>`. It does not change `<path>`, `<start>`, `<end>`, or `<code>`. Path
controls, `U+2028`, and `U+2029` are already rejected by command validation,
and a backslash in a valid path remains a backslash. The escaped message has no
physical line-feed or carriage-return character. It has no source excerpt or
path rewrite.

The command emits diagnostics in the order returned by
`StandardApplicationCheckReport`. It does
not sort, group, deduplicate, colour, annotate, or convert them to another
format. Every line has one final line feed.

The complete exit contract is:

| Result | Standard output | Standard error | Status |
| --- | --- | --- | --- |
| Valid source with no diagnostics | empty | empty | `0` |
| One or more compiler diagnostics | empty | exact diagnostic lines | `1` |
| Source read failure | empty | exact read line | `1` |
| Invalid source UTF-8 | empty | exact UTF-8 line | `1` |
| Embedded standard, empty-catalogue, or context failure | empty | exact standard line | `1` |
| Usage error | empty | exact global usage | `2` |

Checking never emits a success message. A compiler diagnostic is a failed
check, not a usage error.

## Offline and side-effect boundary

The command can run on a host with no PostgreSQL program, library, package,
service, data directory, socket, or network access. Hostile
`ORNA_SERVER_POSTGRES_URL`, `DATABASE_URL`, and `PG*` variables do not affect
it because it does not read them. It also does not read Orna instance
configuration, package state, the current working directory as a project, or
an active database revision.

The only filesystem access is the one supplied source-file read and the host
operations needed to load the already-running `orna` executable. Embedded
standard source and its accepted digest are retained in the binary. The
command opens no filesystem path with write access. It issues no operation that
creates, removes, renames, truncates, changes content, or changes the mode,
owner, or other application-controlled metadata of a filesystem object. The
only write calls are diagnostic bytes sent to the standard-error descriptor
that the caller supplied. It creates no cache, temporary file, lock, durable
source revision, durable catalogue revision, compiler artefact, log, or audit
record. It neither starts nor replaces a process.

An ordinary executable, library, or source-file read can update filesystem
access time according to the mounted filesystem policy. That operating-system
read effect is outside the command-issued write claim. A no-side-effect file
snapshot must either exclude access time or run on a filesystem mounted with
access-time updates disabled. Content, names, owners, modes, and other metadata
must remain unchanged.

The implementation remains in the `orna-server` package and the existing
`orna` binary. This placement reuses the one accepted public executable and
command dispatcher. It does not make source checking a server operation and
does not give `orna-compiler` filesystem or process authority.

## Required proof matrix

| Boundary | Required cases | Required result |
| --- | --- | --- |
| Command shape | exact `source check` plus one path; missing command or path; empty path; flag before, within, or after the command; extra path; `-`; `--`; literal wildcard; shell expansion to multiple paths | Only the exact three-token command continues. Every other invalid shape prints the exact four-line usage and exits `2`. A literal wildcard is an ordinary path and uses the read result for that literal name. Orna performs no glob expansion and never reads standard input. |
| Path token | UTF-8 path; non-UTF-8 Unix `OsString`; tab, line-feed, carriage-return, another Unicode control, `U+2028`, and `U+2029`; `-x`; `./-x`; path without `.orna`; path containing spaces | Invalid UTF-8, control, `U+2028`, `U+2029`, empty, and leading-hyphen paths use the exact usage contract before file access. `./-x`, spaces, and a non-`.orna` suffix remain valid path tokens. No exact rendered path can add a display line. |
| Path resolution | relative path under a changed current working directory; absolute path; symbolic link to a regular file; dangling link; absent path; directory; special file | Relative lookup uses the process current working directory. Logical and rendered paths remain exact `argv`. Only a regular file or link to one reaches decoding. Every other case uses the exact read diagnostic. |
| File access | readable file; permission-denied file; failure during the byte read; empty file; file larger than test buffer boundaries | The command performs one complete byte-read phase, imposes no source-size policy, maps access failures to the exact read line, and does not perform another source read. |
| Exact source | UTF-8 byte-order mark; LF; CRLF; no final line feed; multi-byte Unicode; combining characters; leading and trailing whitespace | `SourceUnit::content`, lossless syntax text, diagnostics, and byte spans prove that no byte-order mark, line ending, Unicode sequence, whitespace, or final-line state was changed. |
| UTF-8 failure | invalid first byte; truncated multi-byte sequence; valid prefix followed by invalid bytes | Each case prints only the exact UTF-8 line with the submitted logical path and exits `1`. No standard verification or compiler check runs. |
| Bundle scope | empty source; declarations and references in one file; reference that exists only in a neighbouring file; directory containing valid `.orna` files | The command checks one source unit only. It performs no directory walk, import, project discovery, or implicit bundle merge. |
| Compiler source-unit cardinality | direct `check_new_application` calls with zero, one, and two ordered source units | Zero and two return exact `SourceUnitCount { actual: 0 }` and `{ actual: 2 }` before `CatalogueSnapshot::new`, parsing, or semantic work. Their exact display text and absent error source match the contract. One proceeds to empty-catalogue construction. |
| Empty application catalogue | one source unit; the same production-private empty-catalogue construction seam used by the public path supplies a typed `CatalogueSnapshotError` for the empty sentinel and empty vectors; simultaneous hostile context data; `Catalogue` derive, field, display, and error source; attempted `panic!` or `expect` construction | `Catalogue { source }` retains the exact supplied `CatalogueSnapshotError` as its only source and wins before context construction or checking. The implementation handles its `Result` directly, with no `panic!` or `expect` path, public seam, or test-only production branch. |
| Empty application identity | successful and failed offline checks; direct active application and standard construction; raw recovered application and standard catalogues; deployable expected base, explicit parent, and candidate, each separately set to the sentinel | A successful offline distinct checked bundle exposes exact `EMPTY_APPLICATION_CATALOGUE_REVISION_ID` bytes `[0; 16]`. A diagnostic report has no checked bundle. Each direct durable revision position returns exact `ReservedOfflineCheckCatalogueRevision { revision, role }` with the corresponding `DurableCatalogueRevisionRole`, exact display text, and no error source. Deployable checks run in expected-base, parent, candidate order. The sentinel remains distinct from the standard catalogue identity ending in `01`; legacy preparation cannot accept the distinct report or bundle at compile time. |
| New-application context | new definitions; rename, delete, and reference that require prior application state; hostile or available database state; every `StandardApplicationContextError` and `NewApplicationCheckError::Context` field, display, and source case; host `Context` mapping; mandatory non-exhaustive wildcard review | Checking uses the all-zero empty application base and no continuity. Database state cannot make a missing application definition resolve or change an identity decision. After successful empty-catalogue construction, the compiler constructs its context from `CheckedStandardLibrary`, preserves the exact typed context error, and never accepts raw verified authority or a standard-owned error. The CLI maps `Catalogue`, `Context`, and every unmatched future compiler error through its mandatory wildcard to the exact embedded-standard line and status `1`; it never calls legacy prepare. |
| Standard authority | accepted retained source and digest; changed retained source; changed hard-coded digest; self-consistent but non-golden snapshot; source-independent manifest alone; host standard-source reconciliation failure; direct compiler raw-capability attempt | Host composition alone verifies the accepted retained `orna.std/1` snapshot and checks its source before application checking. Only `CheckedStandardLibrary` reaches `check_new_application`; no `orna_standard::StandardLibraryError` crosses the compiler seam. The CLI maps host standard failure to the exact embedded-standard line and no application diagnostic. The manifest alone grants no compiler authority. A core-verified nongolden checked standard library remains acceptable only when its checked facts agree and every compatibility contract is supported and unique. |
| Standard type identity | `BOOLEAN`, `BOOL`, `std.boolean`, and `std.types.boolean` in accepted type positions | Every spelling resolves to the same Boolean `TypeId` and emits the exact standard value-type reference evidence. No spelling adapter or second scalar identity participates. |
| Protected standard source | application-owned `std` declaration, `KERNEL CONTRACT`, qualified type export, and prelude export, alone and after valid declarations | The accepted `ORNA0303` code, message, span, protection precedence, and compiler order are preserved. No checked bundle or durable change results. |
| Diagnostic rendering | syntax and semantic failures; multiple ordered diagnostics; path containing spaces and punctuation; CRLF and Unicode before a failure; quoted identifiers containing line feed, carriage return, tab, another control, backslash, `U+2028`, and `U+2029` | Each line is exactly `<path>:<start>..<end>: <code>: <message>` with the exact logical path, zero-based byte offsets, exclusive end, compiler order, exact message escaping, one physical final line feed, and no injected line. Path, span, and code text remain unescaped. |
| Streams and status | success; compiler diagnostics; each pre-check failure; piped hostile standard input; redirected standard output | Standard output is empty in every case. Success is silent `0`, check and operational failures are `1`, usage is `2`, and piped input is neither consumed nor allowed to change or delay the result. |
| PostgreSQL isolation | no PostgreSQL installation or service; hostile PostgreSQL and Orna environment variables; unavailable bundled runtime; changed server configuration | The same source result is produced without a PostgreSQL load, socket, connection, process, package check, runtime check, or environment read. |
| No command-issued state writes | successful and failed checks in a snapshotted writable directory and a read-only directory; process tracing where available; filesystem access-time updates enabled and disabled | The command opens no path for writing and issues no create, content mutation, truncate, rename, remove, ownership, mode, or other metadata mutation. It starts no network or child process. Snapshots exclude access time or use a no-access-time mount. Ordinary read-driven access-time changes do not invalidate the proof. Only the supplied standard-error descriptor can receive command output bytes. |
| Existing host commands | current backend-shell unit and integration suites, valid backend-shell dispatch, invalid global shapes | Source-check dispatch leaves backend-shell terminal, configuration, process, exit, and pre-attachment write behaviour unchanged. The only shared change is the accepted four-line global usage body. |

Normal workspace formatting, build, strict Clippy, unit-test, integration-test,
rustdoc, diff, and similarity gates remain required. Integration tests must run
the built `orna` binary with controlled raw arguments, streams, environment,
current working directory, file types, and permissions. Assertions must use
exact bytes, exit status, diagnostic order, paths, spans, and side-effect
snapshots rather than only checking that an error exists. Compiler and core
unit tests own direct zero/one/two-bundle, sentinel-role, deployable-order,
compile-time report-separation, and durable-guard proof. The PostgreSQL
recovery integration test owns the two raw recovered sentinel positions.

## Implementation prerequisites

This command must not use the legacy compatibility scalar resolver as standard
type authority or the source-independent standard manifest as authority. It
starts only after these exact work ADR 0016 implementation rows are complete,
in their listed order:

* `feat(syntax): parse primitive value types`;
* `feat(std): retain the standard source`;
* `feat(compiler): check standard type source`;
* `feat(compiler): resolve types through std`;
* `feat(compiler): preserve relational value provenance`;
* `feat(compiler): preserve mutation value provenance`;
* `feat(compiler): reference standard function types`.

The dedicated `feat(compiler): check verified new applications` row below
follows those prerequisites and owns the exact public seam and error, sentinel,
durable-role guards, deployable guards, and compile-time report-separation
proof. The later PostgreSQL recovery test, CLI dependency, and command rows
cannot start before that compiler row is complete. The source-check slice does
not reorder, weaken, or bypass any work ADR 0016 authority gate.

Implementation order also requires work ADR 0017's command dispatcher through
all three accepted server leaves. Its sequence must be complete through and
including:

* `feat(server): supervise private PostgreSQL`, which implements
  `orna server run`;
* `feat(server): add offline PostgreSQL upgrade`, which implements
  `orna server upgrade`; and
* `feat(server): bind backend-shell to the ready host`, which rebinds
  `orna server backend-shell` and completes the accepted server dispatcher.

All earlier work ADR 0017 rows required by those three rows are therefore
implementation-order prerequisites. The source-check command row preserves
that working dispatcher and changes its global three-command usage to the
exact final four-line usage in this record. There is no conditional or interim
source-check usage form.

This is an implementation-order dependency, not an operational PostgreSQL
dependency. Dispatch selects `orna source check` before any service-account,
package, instance, runtime, socket, or service check. Source check shares only
the public binary and dispatcher. It must produce the same result when the
bundled PostgreSQL runtime is absent or invalid and when the Orna service is
absent, stopped, or failed.

## Initial implementation sequence

Each row is one buildable, reviewable Conventional Commit. Each row changes
only the exact one to three files listed. The binary remains `orna-server`'s
existing `orna` target; no new crate or executable is added.

| Conventional Commit | Exact files | Required result |
| --- | --- | --- |
| `docs(cli): define offline source check` | `docs/decisions/0018-offline-source-check.md`; `docs/decisions/README.md` | Accept this command, compiler, standard-authority, file, diagnostic, offline, and proof contract. |
| `feat(compiler): check verified new applications` | `crates/orna-core/src/revision.rs`; `crates/orna-compiler/src/lib.rs`; `crates/orna-compiler/src/resolver.rs` | Implement and export exact `NewApplicationCheckError` and `check_new_application`; accept only `&CheckedStandardLibrary`, return `StandardApplicationCheckReport`, reject zero and multiple source units before `CatalogueSnapshot::new`, map empty-sentinel empty-vector catalogue construction to `Catalogue { source }`, then map context construction to `Context { source }`, with no `panic!` or `expect`; reserve and expose exact `EMPTY_APPLICATION_CATALOGUE_REVISION_ID`; add exact `DurableCatalogueRevisionRole` and `ReservedOfflineCheckCatalogueRevision`; reject the sentinel from active or recovered application and standard roles and from deployable expected-base, parent, and candidate positions; label the successful distinct checked bundle with the sentinel; and use the same production-private construction seam as the public path to supply typed `CatalogueSnapshotError` plus hostile context, proving exact `Catalogue` precedence, display, and source without a public seam or test-only production branch, while legacy preparation cannot accept the distinct report or bundle at compile time and durable core rejection remains. This row starts only after all work ADR 0016 prerequisites above. |
| `test(postgres): reject offline catalogue identity` | `crates/orna-kernel-postgres/tests/recovery.rs` | Insert the all-zero identity separately as the raw recovered application catalogue and raw recovered standard catalogue. Prove that each fails through exact shared `RevisionInvariantError::ReservedOfflineCheckCatalogueRevision`, carries the corresponding active-or-recovered role, preserves the exact display and source contract, does not return an active revision, and performs no repair or write. |
| `build(server): add offline source-check dependencies` | `crates/orna-server/Cargo.toml`; `Cargo.lock` | Add normal path dependencies on the core source model, compiler, and retained standard-library orchestrator. Add no PostgreSQL, network, configuration, child-process, glob, or CLI-framework dependency. |
| `feat(server): check one source file offline` | `crates/orna-server/src/main.rs`; `crates/orna-server/src/source_check.rs`; `crates/orna-server/tests/backend_shell.rs` | Preserve every server command leaf, append exact source dispatch and the final global usage, update the existing backend-shell usage assertions without changing its operation, validate the path, read one regular UTF-8 file, reconstruct and verify the embedded standard, check its source, call `check_new_application` with `CheckedStandardLibrary`, render exact diagnostics, and keep standard output empty. |
| `test(server): prove offline source checking` | `crates/orna-server/tests/source_check.rs` | Run the built binary across the complete CLI proof matrix, including raw Unix arguments, exact bytes, escaped messages and spans, piped standard input, hostile PostgreSQL environment, absent and stopped PostgreSQL service states, no command-issued filesystem writes, atime-safe snapshots, and the unchanged backend-shell dispatch boundary. |

## Deferred surface

This record does not accept:

* a directory, project root, source import, recursive walk, glob expansion,
  multiple source units, manifest-selected source, watch mode, or incremental
  daemon;
* source from standard input, an editor buffer, a URL, a database, or another
  process;
* a flag, global option, database selector, configuration file, environment
  input, diagnostic colour, JSON output, source excerpt, warning policy, or
  alternate output format;
* checking against an existing application catalogue, continuity, rename
  preservation, semantic diff, migration planning, `prepare`, apply,
  activation, export, or revision storage;
* automatic standard-library repair, replacement, download, network lookup,
  authority from the source-independent manifest, or fallback scalar naming;
  or
* a new binary, compiler filesystem access, background process, cache, log,
  telemetry, or user interface.

Each later workflow requires its own accepted input, authority, diagnostic,
side-effect, and compatibility contract.

## Consequences

A developer and continuous-integration job can prove the syntax and semantics
of one new application file without installing or starting PostgreSQL. The
result exercises the real lossless parser, semantic checker, protected
standard-library surface, and verified standard type identities rather than a
separate linter.

The command is deliberately not a complete project check. It cannot resolve
another application file or preserve identity from an active revision. That
limit keeps the first offline workflow deterministic and free of hidden
database or project-discovery authority.

## Precedence

This record makes the illustrative `orna source check broken-change.orna`
recovery command in `spec/docs/06-bootstrapping-recovery.md` concrete only for
one new, standalone application file. It narrows the directory-shaped current
proposal in `spec/api/cli.md` and `spec/docs/37-modules-distribution.md`. It
does not accept the companion apply, export, recovery, or revision commands in
those documents.

This record implements the source-to-semantic-check portion of
`spec/api/compiler.md`, `spec/docs/25-source-compiler-ir.md`, and spec ADR
0009. It stops before their semantic diff, code-generation, preparation,
transactional apply, activation, and durable source-snapshot stages.

It depends on the verified catalogue-backed standard type authority in work
ADR 0016 and does not weaken that record's protected source, retained source,
hard-coded digest, direct binding, type-reference, or fail-closed rules.

It appends the source-check line to work ADR 0017's global usage contract. It
does not change work ADR 0017's PostgreSQL distribution, host ABI, package,
service, instance, environment, process, runtime, or backend-shell rules. It
also preserves work ADR 0014's backend-shell operation where work ADR 0017 has
not superseded it.

For the first offline developer source-check command, this accepted record has
precedence.
