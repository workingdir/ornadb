# Migrating to OrnaDB 1.0.0

> **Legacy guide.** The source-command workflow below belongs to the removed
> `orna` binary and the legacy `orna-core` crate, not the current `orna-cli-v1`
> CLI. Command names in this file were checked against
> `crates/orna-cli-v1/src/cli_args.rs`.

**Status:** This is migration guidance for the current source tree, not a
promise that a production package is available. The checkout has no
tracked production package or in-place upgrade command. The immutable Orna
1.0.0 reference bundle is present separately and is the authority for the
current contract; this note does not amend it or claim implementation
conformance.

## Authority boundary

Chapter 1 of the immutable Orna 1.0.0 specification
defines the language, repository, embedded-runtime, system, storage, and
live-presentation profiles. It explicitly does not prescribe a compiler
implementation language, a user-interface toolkit, or a hosting provider.
Its conformance requirements in chapters 1 and 32 require claims to identify
their classes, publication digest, and actually
executed tests; a fixture, index, or source-tree check is not implementation
conformance by itself.

The workflows and boundaries below describe this checkout's managed-product
and implementation evidence. Their command examples remain source-tree
guidance; they do not rename or enlarge the immutable specification's CLI
contract.

## Read this before changing anything

There is no supported 0.x-or-development-engine to 1.0.0 upgrade path in this
checkout. The first-release decision intentionally defines empty accepted
predecessor and forward-edge sets. In particular:

- Do not treat a development or 0.x database as a supported predecessor for a
  1.0 installation.
- Do not run `orna server upgrade`: that command is not present in the current
  CLI. There is no hidden compatibility mode or environment-variable override
  that creates an upgrade path.
- Do not edit `_orna_kernel` tables, retained revision rows, generated SQL, or
  migration ledger entries by hand. Recovery validates their identities and
  digests, and hand edits are unsupported.
- Do not split one application into backend-specific `.orna` files. Orna source
  is backend-neutral. Keep one source model and let the selected adapter accept
  or reject its currently implemented physical subset; an unsupported shape
  must fail closed, not acquire different semantics.
- Do not treat a retained evidence artifact, a local binary, or a build directory as
  a production distribution authority. The old
  [first-release decision](decisions/0047-first-one-zero-release.md) records a
  historical managed-product release plan; it does not amend the immutable
  1.0.0 contract or establish a current distribution requirement.

The current CLI has no application source-revision command, and no engine
upgrade path exists in this checkout.

## Application source changes (legacy, not in the v1 CLI)

The `orna source check`, `orna source diff` and `orna source apply` commands
that earlier revisions of this guide documented are not part of the current
`orna` CLI. The verb list in `crates/orna-cli-v1/src/cli_args.rs` has no
`source` verb. These commands existed only in the legacy `orna-core` crate and
the removed `orna` binary; [CHANGELOG.md](../CHANGELOG.md) records their
removal. Do not follow the removed workflow to change an installed
application. This guide does not give a replacement command for source
revisions.

## Backend-neutral storage boundary (legacy)

The `.orna` source is the portable contract. The adapter capability statements
that earlier revisions of this section made (a direct SQLite filesystem
adapter, its supported and rejected shapes, and a checked-in
`0046_application_migrations.orna` migration source) describe the legacy
`orna-core` surface. None of them is verified against the v1 CLI in this
checkout: `crates/orna-storage` does not exist (the storage crate is
`crates/orna-storage-v1`), and no `0046_application_migrations.orna` file is
present. Do not rely on those statements. For the `--db` endpoint forms the
v1 CLI accepts, read `Endpoint::parse` in `crates/orna-cli-v1/src/main.rs`.


## Standard-library compatibility (managed-product evidence)

Standard-library snapshots are content-addressed and immutable. Product
selection retains revisions `orna.std/1` through `orna.std/9`. The pre-1.0 V10
`std/cli.orna` source and V9-to-V10 upgrade constructor have been retired; the
V10 revision identity remains reserved, and a stored V10 snapshot can only be
verified when explicitly supplied. V10 is never reconstructed or replaced with
different source. V11's SQL-shaped math source is retired as well; pure math
comes from the pinned Orna 1.0 source. Each supported upgrade requires the
exact expected parent revision and verifies the parent before constructing the
child; an already-installed or mismatched base fails closed.

### Source-level pinned pure-module boundary

The current v1 command-line source boundary has one smaller, executable
standard-module profile: `orna.std/v1-reference-library`. It contains the
bundled `std.math` module and the `std.collection` source surface. This is a
pinned source bundle, not discovery of an installed or host-provided standard
library.

When a project imports a standard module, `orna check` first requires
the imported logical module to be named by the selected profile. It then
verifies the exact source bytes of every profile module, requires the complete
declared bundle, parses the verified sources, and derives the semantic
catalogue from their declarations. A changed, missing, duplicate, malformed,
or unlisted standard module is rejected before that catalogue is admitted.

`orna run seed` uses the same selected source bundle. After semantic
checking, its bounded evaluator loads the verified pure module and explicitly
invokes the requested retained function; loading alone does not execute a
function body. This establishes executable evidence for that source-level
pure-function subset, including calls to the bundled `std.math` functions.

It does not establish a complete standard library or general runtime. Standard
modules beyond the selected bundle, tables, effects, streams, credentials,
network access, and non-empty initial state remain outside this boundary and
require the integrated runtime. The broader standard-library revisions and
upgrade preparation described above are separate compatibility work; they do
not enlarge this v1 pure-module profile or constitute a 1.0 release claim.

## Private protocol and runtime compatibility

The local server socket uses the private Orna handshake (versions 1 through 5)
and local peer authentication. Runtime values use canonical ORV5 envelopes;
checked sets use ORV6. These are private Orna contracts, not public SQL or
remote-transport compatibility promises.

Inspector compatibility is similarly strict: inspection epochs use the full
16-byte epoch identity in `ORNA-INSPECT/1`. The superseded low-u64 draft is not
a mixed-width compatibility mode. Readers and writers must move together;
historical inspector helper names are not silently rewritten. Preserve
revision-pinned inspection records and treat an unavailable decoder as an
explicit unavailable result.

The explicit endpoint parser recognizes local paths, managed-local URIs,
Unix-socket URIs, and remote Orna URIs. The bounded installed `invoke` route
accepts only the current managed Orna Unix socket through local authentication;
other explicit Unix command routes and remote routes fail closed because their
session or transport support is unavailable. Parsing an endpoint is not
evidence that migration or a general remote session can use it.

## Runtime and editor limits (source-tree evidence)

For this checkout's installed managed-product route, the documented runtime
boundary is `tty`. The optional Qt path and native ABI checks are separately
gated implementation evidence requiring their own ABI input, native
dependencies, and retained results; no Qt conformance or production-support
claim is made here. The immutable specification does not prescribe a UI
toolkit or hosting provider. Direct SQLite does not provide Qt.

`orna-lsp`, Tree-sitter, TextMate, and the editor integration packages cover
the checked-in static tooling surface. The accepted corpus is evidence for that
bounded surface only. Static checks do not launch Neovim, Vim, Helix, Zed,
VS Code, Sublime, or Emacs host sessions and do not establish full grammar
parity with the immutable 1.0.0 reference bundle.

## Release and evidence boundary (legacy managed-product plan)

ADR 0047 records a historical managed-product release plan: Debian 12 amd64
`1.0.0-1` with a signed `v1.0.0` source tag. That plan is not a requirement of
the immutable 1.0.0 specification. Neither its scope nor its conformance
chapter supplies a Debian, package, or distribution mandate. This checkout
includes `packaging/linux/`, a minimal provenance/install artifact recipe, but
it does not contain a Debian release package or production-package evidence.
No package described by the old plan is an upgrade source; the product
baseline has no accepted predecessor or forward upgrade edge.

The [maintainer runbook](maintainer-runbook.md) defines the required
prerequisites and evidence vocabulary. In particular:

- `cargo fetch --locked` is networked dependency provisioning.
  `CARGO_NET_OFFLINE=true` only makes Cargo dependency resolution fail closed;
  it does not make a native build host- or network-free.
- The immutable 1.0.0 reference bundle supplies the current normative contract
  and authored conformance cases. Local implementation, ABI, and native-host
  gates may still be unavailable or unexecuted; under the bundle's conformance
  and evidence rules, that limits the corresponding implementation claim but
  does not make the specification absent. A successful local/static command
  must not be upgraded into a broader conformance claim.
- Backup/DR policy, production recovery guarantees, support period, SLA, and
  later predecessor edges remain deferred. Do not present the application
  ledger or source files as a backup or restore protocol.

## Compatibility summary

| Surface | Current boundary | 1.0 migration claim |
| --- | --- | --- |
| `.orna` source | Backend-neutral typed source model | One portable source; no public backend-specific SQL |
| Filesystem endpoint | Bounded direct SQLite route | No full physical/runtime parity claim |
| Explicit Unix/remote endpoint | Bounded current-socket `invoke` only for Unix; remote transport unavailable | No migration or general remote-session support |
| Application source revisions | Typed ledger, hashes, atomic apply | Supported only on a ready compatible engine |
| Engine predecessor upgrade | Empty accepted predecessor/edge sets | No 0.x/development in-place upgrade |
| Standard library | Code through V10; acceptance records through V9; pinned Orna 1.0 pure-math source | V10 not promised until evidence is reconciled; V11 legacy math retired |
| Package distribution | Historical ADR0047 Debian plan plus tracked minimal `packaging/linux/` artifact recipe; no Debian package | No distribution mandate and no product upgrade source |

For command details and the current evidence map, see the
[maintainer runbook](maintainer-runbook.md),
[editor tooling guide](editor-tooling.md),
[1.0.0 release inventory](../CHANGELOG.md), and
[decision index](decisions/README.md).
